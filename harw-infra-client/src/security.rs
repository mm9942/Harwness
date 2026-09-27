//! [`SecurityHubClient`]: skeleton client of the SecurityHub
//! (`security.sock`, drift report D2).
//!
//! The common surface (health, version, capabilities) plus context
//! verification ([`SecurityHubClient::verify_context`], H12): a verifier
//! peer (e.g. `harw-web`) asks the hub whether a context reference a local
//! client presented is still active. Incident and policy operations come
//! with the daemon (H8).

use core::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use http::Method;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::error::{InfraClientError, RemoteErrorKind};
use crate::info::{self, Capabilities, Health, JSON, VersionInfo};
use crate::transport::{ClientOptions, RequestBody, UdsTransport};

/// Longest context id the hub accepts in a path (mirrors
/// `harw_security_hub::server::MAX_CONTEXT_ID_LEN`).
pub const MAX_CONTEXT_ID_LEN: usize = 128;

/// Outcome of `GET /v1/contexts/{id}` ([`SecurityHubClient::verify_context`]).
///
/// `T` is the caller's view of the context document (the hub sends a
/// `harw_types::SecurityContextSummary`; this crate does not depend on
/// `harw-types`, so the caller picks the type). The document is what the
/// hub vouches for **to this verifier over its own authenticated socket**;
/// it is not something a client may hand over by value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextVerification<T> {
    /// `200 {"status":"active","context":…}`: the context is live.
    Active(T),
    /// `410`: the context expired or was revoked. The hub distinguishes the
    /// two in its body; for an authorization decision both mean "deny".
    Gone,
    /// `404`: unknown id, or this peer is neither a verifier nor the
    /// requester (the hub hides the difference so ids cannot be probed).
    Unknown,
}

#[derive(Deserialize)]
struct ActiveWire<T> {
    status: String,
    context: T,
}

/// `true` if `id` is a syntactically acceptable context id: non-empty, at
/// most [`MAX_CONTEXT_ID_LEN`] bytes, only ASCII alphanumerics, `-`, `_`.
/// This also guarantees the id cannot change the request path.
fn is_valid_context_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_CONTEXT_ID_LEN
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Default SecurityHub socket (drift report D1/D2).
pub const DEFAULT_SECURITY_SOCKET: &str = "/run/harw/infra/security.sock";

/// Client of the SecurityHub. `Clone` shares only immutable connection
/// parameters.
#[derive(Clone)]
pub struct SecurityHubClient {
    transport: Arc<UdsTransport>,
}

impl SecurityHubClient {
    /// A client for the daemon at `socket`.
    pub fn new(socket: impl Into<PathBuf>, options: ClientOptions) -> Self {
        Self {
            transport: Arc::new(UdsTransport::new(socket.into(), None, options)),
        }
    }

    /// The configured socket path.
    pub fn socket_path(&self) -> &Path {
        self.transport.socket()
    }

    /// `GET /v1/health`.
    pub async fn health(&self) -> Result<Health, InfraClientError> {
        info::health(&self.transport).await
    }

    /// `GET /v1/version`.
    pub async fn version(&self) -> Result<VersionInfo, InfraClientError> {
        info::version(&self.transport).await
    }

    /// `GET /v1/capabilities` (descriptive, not authority).
    pub async fn capabilities(&self) -> Result<Capabilities, InfraClientError> {
        info::capabilities(&self.transport).await
    }

    /// `GET /v1/contexts/{id}`: asks the hub whether context `id` is active.
    ///
    /// The calling process must be a configured verifier of the hub (or the
    /// requester of the context); otherwise the hub answers `404` and this
    /// returns [`ContextVerification::Unknown`].
    ///
    /// # Errors
    /// - [`InfraClientError::Protocol`] for a syntactically invalid `id`
    ///   (nothing is sent), a wrong content type, or a malformed/non-active
    ///   `200` document;
    /// - [`InfraClientError::Unavailable`] / [`InfraClientError::Timeout`]
    ///   when the hub cannot be reached in time;
    /// - any other mapped status ([`InfraClientError::Forbidden`], …).
    pub async fn verify_context<T: DeserializeOwned>(
        &self,
        id: &str,
    ) -> Result<ContextVerification<T>, InfraClientError> {
        if !is_valid_context_id(id) {
            return Err(InfraClientError::Protocol("invalid context id"));
        }
        let path = format!("/v1/contexts/{id}");
        let response = match self
            .transport
            .call(Method::GET, &path, RequestBody::Empty, None)
            .await
        {
            Ok(response) => response,
            Err(InfraClientError::NotFound) => return Ok(ContextVerification::Unknown),
            Err(InfraClientError::Remote(RemoteErrorKind::Status(410))) => {
                return Ok(ContextVerification::Gone);
            }
            Err(other) => return Err(other),
        };
        response.expect_content_type(JSON)?;
        let wire: ActiveWire<T> = serde_json::from_slice(&response.body)
            .map_err(|_| InfraClientError::Protocol("malformed context document"))?;
        if wire.status != "active" {
            return Err(InfraClientError::Protocol("context document is not active"));
        }
        Ok(ContextVerification::Active(wire.context))
    }
}

impl fmt::Debug for SecurityHubClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecurityHubClient")
            .field("socket", &self.transport.socket())
            .field("options", &self.transport.options())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use bytes::Bytes;
    use http::header::{CONTENT_TYPE, HeaderValue};
    use http::{Request, Response, StatusCode};
    use http_body_util::Full;
    use hyper::body::Incoming;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use serde::Deserialize;
    use tokio::net::UnixListener;

    use super::{ContextVerification, SecurityHubClient, is_valid_context_id};
    use crate::error::InfraClientError;
    use crate::test_support::{TestError, TestResult};
    use crate::transport::ClientOptions;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Doc {
        principal_id: String,
    }

    fn answer(
        status: StatusCode,
        content_type: &'static str,
        body: &'static str,
    ) -> Response<Full<Bytes>> {
        let mut response = Response::new(Full::new(Bytes::from_static(body.as_bytes())));
        *response.status_mut() = status;
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        response
    }

    async fn route(request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
        let json = "application/json";
        Ok(match request.uri().path() {
            "/v1/contexts/ctx-active" => answer(
                StatusCode::OK,
                json,
                r#"{"status":"active","context":{"principal_id":"alice","extra":1}}"#,
            ),
            "/v1/contexts/ctx-expired" => answer(StatusCode::GONE, json, r#"{"status":"expired"}"#),
            "/v1/contexts/ctx-revoked" => answer(StatusCode::GONE, json, r#"{"status":"revoked"}"#),
            "/v1/contexts/ctx-weird" => answer(
                StatusCode::OK,
                json,
                r#"{"status":"pending","context":{"principal_id":"x"}}"#,
            ),
            "/v1/contexts/ctx-text" => answer(StatusCode::OK, "text/plain", "active"),
            _ => answer(StatusCode::NOT_FOUND, json, r#"{"error":"not_found"}"#),
        })
    }

    /// A one-route mock hub on a tempdir socket; returns the dir (keeps the
    /// socket alive), the client and the server task.
    async fn mock_hub() -> TestResult<(
        tempfile::TempDir,
        SecurityHubClient,
        tokio::task::JoinHandle<()>,
    )> {
        let dir = tempfile::tempdir()?;
        let socket = dir.path().join("security.sock");
        let listener = UnixListener::bind(&socket)?;
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service_fn(route))
                        .await;
                });
            }
        });
        let client = SecurityHubClient::new(socket, ClientOptions::default());
        Ok((dir, client, task))
    }

    #[test]
    fn test_context_id_validation_rejects_path_changing_ids() {
        assert!(is_valid_context_id("ctx-1_A"));
        assert!(!is_valid_context_id(""));
        assert!(!is_valid_context_id("../health"));
        assert!(!is_valid_context_id("a/b"));
        assert!(!is_valid_context_id("a?b"));
        assert!(!is_valid_context_id(&"a".repeat(129)));
    }

    #[tokio::test]
    async fn test_verify_context_maps_active_gone_and_unknown() -> TestResult {
        let (_dir, client, task) = mock_hub().await?;
        let active = client.verify_context::<Doc>("ctx-active").await?;
        assert_eq!(
            active,
            ContextVerification::Active(Doc {
                principal_id: "alice".to_owned()
            })
        );
        assert_eq!(
            client.verify_context::<Doc>("ctx-expired").await?,
            ContextVerification::Gone
        );
        assert_eq!(
            client.verify_context::<Doc>("ctx-revoked").await?,
            ContextVerification::Gone
        );
        assert_eq!(
            client.verify_context::<Doc>("ctx-missing").await?,
            ContextVerification::Unknown
        );
        task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_verify_context_rejects_non_active_and_wrong_content_type() -> TestResult {
        let (_dir, client, task) = mock_hub().await?;
        let weird = client.verify_context::<Doc>("ctx-weird").await;
        if !matches!(weird, Err(InfraClientError::Protocol(_))) {
            return Err(TestError::Unexpected(format!(
                "non-active status accepted: {weird:?}"
            )));
        }
        let text = client.verify_context::<Doc>("ctx-text").await;
        if !matches!(text, Err(InfraClientError::Protocol(_))) {
            return Err(TestError::Unexpected(format!(
                "text/plain accepted: {text:?}"
            )));
        }
        task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_verify_context_invalid_id_is_rejected_before_sending() -> TestResult {
        // No socket exists at this path: an attempted call would be `Unavailable`.
        let dir = tempfile::tempdir()?;
        let client =
            SecurityHubClient::new(dir.path().join("absent.sock"), ClientOptions::default());
        let result = client.verify_context::<Doc>("../v1/health").await;
        assert_eq!(
            result,
            Err(InfraClientError::Protocol("invalid context id"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_verify_context_unreachable_hub_is_unavailable() -> TestResult {
        let dir = tempfile::tempdir()?;
        let client =
            SecurityHubClient::new(dir.path().join("absent.sock"), ClientOptions::default());
        let result = client.verify_context::<Doc>("ctx-1").await;
        assert_eq!(result, Err(InfraClientError::Unavailable));
        Ok(())
    }
}
