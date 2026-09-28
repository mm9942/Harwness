//! HTTP/1 over a Unix stream socket: one fresh connection per call.
//!
//! Each call:
//! 1. `tokio::net::UnixStream::connect(socket)`;
//! 2. `hyper::client::conn::http1::handshake`;
//! 3. one request;
//! 4. the response body is collected into zeroizing memory, bounded by
//!    [`ClientOptions::max_response_bytes`], after which the sender is
//!    dropped so the connection closes.
//!
//! The connection future is driven with `tokio::join!` in the calling task
//! (no spawned task can outlive the call), and the whole sequence runs
//! under one [`ClientOptions::timeout`].
//!
//! There is no connection pool and no retry: a pooled connection would be
//! shared mutable state behind the cloneable client, and retrying is the
//! caller's decision (mutations must not be auto-retried).
//!
//! Zeroization boundary: request bodies with secret bytes are handed to
//! hyper as `Bytes::from_owner(<zeroizing owner>)`, and response bodies are
//! copied chunk by chunk into a zeroizing buffer that never reallocates
//! without zeroizing the old allocation. hyper's own read/write buffers
//! (and the kernel socket buffers) are outside this crate's control and are
//! not zeroized.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
use http::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HOST};
use http::{HeaderValue, Method, Request, Response, Uri};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use zeroize::Zeroizing;

use crate::error::{InfraClientError, map_status};
use crate::token::BearerToken;

/// Default per-call timeout (connect + request + full response).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
/// Default upper bound for a response body.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 1024 * 1024;

const TOO_LARGE: InfraClientError = InfraClientError::Protocol("response body too large");

/// Per-client call limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientOptions {
    /// Deadline for one call: connect, handshake, request and the complete
    /// response body.
    pub timeout: Duration,
    /// Largest accepted response body; larger responses are a
    /// [`InfraClientError::Protocol`] error.
    pub max_response_bytes: usize,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }
}

/// Request body variants.
pub(crate) enum RequestBody {
    /// No body.
    Empty,
    /// A non-secret body.
    Public(Vec<u8>),
    /// A body containing secret bytes; the zeroizing buffer becomes the
    /// owner of the `Bytes` hyper writes from.
    Secret(Zeroizing<Vec<u8>>),
}

/// `AsRef<[u8]>` owner for `Bytes::from_owner`: zeroized on drop.
struct SecretOwner(Zeroizing<Vec<u8>>);

impl AsRef<[u8]> for SecretOwner {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

/// A successful (2xx) response.
pub(crate) struct RawResponse {
    content_type: Option<HeaderValue>,
    /// The complete body, in zeroizing memory.
    pub(crate) body: Zeroizing<Vec<u8>>,
}

impl RawResponse {
    /// Require the response media type (parameters ignored,
    /// case-insensitive).
    pub(crate) fn expect_content_type(&self, expected: &str) -> Result<(), InfraClientError> {
        let actual = self
            .content_type
            .as_ref()
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim);
        match actual {
            Some(actual) if actual.eq_ignore_ascii_case(expected) => Ok(()),
            _ => Err(InfraClientError::Protocol(
                "unexpected response content type",
            )),
        }
    }

    /// Move the body out as a plain `Vec` (for non-secret payloads).
    pub(crate) fn into_public_body(mut self) -> Vec<u8> {
        std::mem::take(&mut *self.body)
    }
}

/// The immutable connection parameters shared (via `Arc`) by all clones of
/// one client.
pub(crate) struct UdsTransport {
    socket: PathBuf,
    token: Option<BearerToken>,
    options: ClientOptions,
}

impl UdsTransport {
    pub(crate) fn new(socket: PathBuf, token: Option<BearerToken>, options: ClientOptions) -> Self {
        Self {
            socket,
            token,
            options,
        }
    }

    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }

    pub(crate) fn options(&self) -> ClientOptions {
        self.options
    }

    pub(crate) fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// One call. Non-2xx statuses are mapped to [`InfraClientError`].
    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        body: RequestBody,
        content_type: Option<&'static str>,
    ) -> Result<RawResponse, InfraClientError> {
        let request = self.build_request(method, path, body, content_type)?;
        tokio::time::timeout(self.options.timeout, self.exchange(request))
            .await
            .unwrap_or(Err(InfraClientError::Timeout))
    }

    fn build_request(
        &self,
        method: Method,
        path: &str,
        body: RequestBody,
        content_type: Option<&'static str>,
    ) -> Result<Request<Full<Bytes>>, InfraClientError> {
        let bytes = match body {
            RequestBody::Empty => Bytes::new(),
            RequestBody::Public(bytes) => Bytes::from(bytes),
            RequestBody::Secret(bytes) => Bytes::from_owner(SecretOwner(bytes)),
        };
        let uri =
            Uri::try_from(path).map_err(|_| InfraClientError::Protocol("invalid request path"))?;
        let mut request = Request::new(Full::new(bytes));
        *request.method_mut() = method;
        *request.uri_mut() = uri;
        let headers = request.headers_mut();
        headers.insert(HOST, HeaderValue::from_static("localhost"));
        if let Some(content_type) = content_type {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        }
        if let Some(token) = &self.token {
            headers.insert(AUTHORIZATION, token.header_value()?);
        }
        Ok(request)
    }

    async fn exchange(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<RawResponse, InfraClientError> {
        let stream = UnixStream::connect(&self.socket)
            .await
            .map_err(|_| InfraClientError::Unavailable)?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(stream))
                .await
                .map_err(map_hyper_error)?;
        let max = self.options.max_response_bytes;
        let exchange = async move {
            let response = sender.send_request(request).await.map_err(map_hyper_error);
            let result = match response {
                Ok(response) => collect_response(response, max).await,
                Err(err) => Err(err),
            };
            // Dropping the sender once the response is fully read lets the
            // connection future finish instead of waiting for another
            // request.
            drop(sender);
            result
        };
        // Any connection error also surfaces through `exchange`.
        let (result, _connection) = tokio::join!(exchange, connection);
        result
    }
}

fn map_hyper_error(err: hyper::Error) -> InfraClientError {
    if err.is_timeout() {
        InfraClientError::Timeout
    } else if err.is_parse() || err.is_user() {
        InfraClientError::Protocol("malformed HTTP exchange")
    } else {
        InfraClientError::Unavailable
    }
}

async fn collect_response(
    response: Response<Incoming>,
    max: usize,
) -> Result<RawResponse, InfraClientError> {
    let status = response.status();
    if !status.is_success() {
        return Err(map_status(status));
    }
    let content_type = response.headers().get(CONTENT_TYPE).cloned();
    let declared = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if declared.is_some_and(|len| len > max) {
        return Err(TOO_LARGE);
    }

    let mut buf = Zeroizing::new(Vec::with_capacity(declared.unwrap_or(0)));
    let mut body = response.into_body();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(map_hyper_error)?;
        if let Ok(data) = frame.into_data() {
            append_zeroizing(&mut buf, &data, max)?;
        }
    }
    Ok(RawResponse {
        content_type,
        body: buf,
    })
}

/// Append `chunk`, bounded by `max`. Growth allocates a new zeroizing buffer
/// and drops (= zeroizes) the old one, instead of letting `Vec` reallocate
/// and free the old allocation un-zeroized.
fn append_zeroizing(
    buf: &mut Zeroizing<Vec<u8>>,
    chunk: &[u8],
    max: usize,
) -> Result<(), InfraClientError> {
    let needed = buf
        .len()
        .checked_add(chunk.len())
        .filter(|needed| *needed <= max)
        .ok_or(TOO_LARGE)?;
    if needed > buf.capacity() {
        let capacity = needed.max(buf.capacity().saturating_mul(2)).min(max);
        let mut grown = Zeroizing::new(Vec::with_capacity(capacity));
        grown.extend_from_slice(buf.as_slice());
        *buf = grown;
    }
    buf.extend_from_slice(chunk);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn test_append_zeroizing_grows_without_realloc_and_bounds() -> TestResult {
        let mut buf = Zeroizing::new(Vec::with_capacity(2));
        append_zeroizing(&mut buf, b"ab", 8)?;
        append_zeroizing(&mut buf, b"cdef", 8)?;
        assert_eq!(buf.as_slice(), b"abcdef");
        // Growth is bounded by `max`.
        assert!(buf.capacity() >= 6 && buf.capacity() <= 8);
        assert_eq!(append_zeroizing(&mut buf, b"xyz", 8), Err(TOO_LARGE));
        assert_eq!(buf.as_slice(), b"abcdef");
        Ok(())
    }

    #[test]
    fn test_content_type_check_ignores_parameters_and_case() {
        let response = RawResponse {
            content_type: Some(HeaderValue::from_static("Application/JSON; charset=utf-8")),
            body: Zeroizing::new(Vec::new()),
        };
        assert!(response.expect_content_type("application/json").is_ok());
        assert!(
            response
                .expect_content_type("application/octet-stream")
                .is_err()
        );

        let missing = RawResponse {
            content_type: None,
            body: Zeroizing::new(Vec::new()),
        };
        assert!(missing.expect_content_type("application/json").is_err());
    }
}
