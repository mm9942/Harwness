//! HTTP/1 upgrade of the session control plane (W00 §3.1).
//!
//! Endpoint `GET /v1/session-ws` with `Connection: Upgrade`,
//! `Upgrade: websocket`, `Sec-WebSocket-Version: 13`, a `Sec-WebSocket-Key`
//! that is the base64 form of 16 bytes and the exact subprotocol token
//! `harw.session.v1` (missing or wrong → refused, WS-01). A request that
//! carries an `Origin` header is refused (WS-04): browser control is not a v1
//! capability.
//!
//! Identity is established by the transport before any of this runs; this
//! module only checks the HTTP shape of the handshake and fails closed.

use std::fmt;

use bytes::Bytes;
use harw_protocol::session_wire::{SESSION_WS_PATH, SESSION_WS_SUBPROTOCOL};
use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use http::{Method, Request, Response, StatusCode};
use http_body_util::{Empty, Full};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};

/// The only WebSocket protocol version accepted (RFC 6455).
const WS_VERSION: &str = "13";

/// Why an upgrade handshake was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeRejection {
    /// The request method is not `GET`.
    WrongMethod,
    /// The request path is not [`SESSION_WS_PATH`].
    WrongPath,
    /// `Connection: Upgrade` / `Upgrade: websocket` missing (or, client
    /// side, the response is not a `101 Switching Protocols`).
    NotAnUpgrade,
    /// `Sec-WebSocket-Version` is missing, repeated or not `13`.
    WrongVersion,
    /// `Sec-WebSocket-Key` is missing, repeated or not base64 of 16 bytes.
    MissingKey,
    /// The exact subprotocol token `harw.session.v1` is missing (WS-01).
    MissingSubprotocol,
    /// An `Origin` header is present (WS-04).
    OriginRefused,
    /// Client side: `Sec-WebSocket-Accept` does not match the sent key.
    BadAccept,
}

impl UpgradeRejection {
    /// HTTP status the rejection maps to.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::WrongMethod => StatusCode::METHOD_NOT_ALLOWED,
            Self::WrongPath => StatusCode::NOT_FOUND,
            Self::NotAnUpgrade | Self::WrongVersion => StatusCode::UPGRADE_REQUIRED,
            Self::MissingKey | Self::MissingSubprotocol => StatusCode::BAD_REQUEST,
            Self::OriginRefused => StatusCode::FORBIDDEN,
            Self::BadAccept => StatusCode::BAD_GATEWAY,
        }
    }

    /// Short, stable, human-readable reason (also the rejection body).
    pub fn reason(&self) -> &'static str {
        match self {
            Self::WrongMethod => "session websocket requires GET",
            Self::WrongPath => "unknown session websocket path",
            Self::NotAnUpgrade => "websocket upgrade required",
            Self::WrongVersion => "websocket version 13 required",
            Self::MissingKey => "missing or malformed Sec-WebSocket-Key",
            Self::MissingSubprotocol => "subprotocol harw.session.v1 required",
            Self::OriginRefused => "Origin header refused: browser clients are not supported",
            Self::BadAccept => "Sec-WebSocket-Accept does not match the key",
        }
    }
}

impl fmt::Display for UpgradeRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.reason(), self.status())
    }
}

impl std::error::Error for UpgradeRejection {}

/// Validate a server-side upgrade request (headers only, generic over the body).
pub fn validate_upgrade<B>(request: &Request<B>) -> Result<(), UpgradeRejection> {
    if request.method() != Method::GET {
        return Err(UpgradeRejection::WrongMethod);
    }
    if request.uri().path() != SESSION_WS_PATH {
        return Err(UpgradeRejection::WrongPath);
    }
    let headers = request.headers();
    if !has_token_ci(headers, &header::CONNECTION, "upgrade")
        || !has_token_ci(headers, &header::UPGRADE, "websocket")
    {
        return Err(UpgradeRejection::NotAnUpgrade);
    }
    match single_value(headers, &header::SEC_WEBSOCKET_VERSION) {
        Some(version) if version.trim() == WS_VERSION => {}
        _ => return Err(UpgradeRejection::WrongVersion),
    }
    match single_value(headers, &header::SEC_WEBSOCKET_KEY) {
        Some(key) if is_valid_key(key.as_bytes()) => {}
        _ => return Err(UpgradeRejection::MissingKey),
    }
    let offers_subprotocol = header_tokens(headers, &header::SEC_WEBSOCKET_PROTOCOL)
        .any(|token| token == SESSION_WS_SUBPROTOCOL);
    if !offers_subprotocol {
        return Err(UpgradeRejection::MissingSubprotocol);
    }
    if headers.contains_key(header::ORIGIN) {
        return Err(UpgradeRejection::OriginRefused);
    }
    Ok(())
}

/// 101 response: Upgrade/Connection headers, `Sec-WebSocket-Accept` derived
/// from the request key, `Sec-WebSocket-Protocol: harw.session.v1`.
pub fn accept_response<B>(
    request: &Request<B>,
) -> Result<Response<Empty<Bytes>>, UpgradeRejection> {
    validate_upgrade(request)?;
    let key = single_value(request.headers(), &header::SEC_WEBSOCKET_KEY)
        .ok_or(UpgradeRejection::MissingKey)?;
    // Base64 output is always a valid header value; fail closed regardless.
    let accept = HeaderValue::from_str(&derive_accept_key(key.as_bytes()))
        .map_err(|_| UpgradeRejection::MissingKey)?;

    let mut response = Response::new(Empty::new());
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let headers = response.headers_mut();
    headers.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    headers.insert(header::SEC_WEBSOCKET_ACCEPT, accept);
    headers.insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_static(SESSION_WS_SUBPROTOCOL),
    );
    Ok(response)
}

/// Plain error response for a rejection (status + short text body).
pub fn rejection_response(rejection: &UpgradeRejection) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from_static(rejection.reason().as_bytes())));
    *response.status_mut() = rejection.status();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    match rejection {
        // RFC 7231 §6.5.15: a 426 names the protocol to switch to.
        UpgradeRejection::NotAnUpgrade => {
            headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        }
        // RFC 6455 §4.4: tell the client which version is supported.
        UpgradeRejection::WrongVersion => {
            headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
            headers.insert(
                header::SEC_WEBSOCKET_VERSION,
                HeaderValue::from_static(WS_VERSION),
            );
        }
        _ => {}
    }
    response
}

/// Server side: validate the request, spawn a task that awaits the hyper
/// upgrade and hands the [`WebSocketStream`] (`Role::Server`, `config`) to
/// `on_socket`, and return the 101 response immediately.
///
/// Must be called inside a Tokio runtime. A rejection is returned as `Err`;
/// the caller answers with [`rejection_response`].
pub fn upgrade_server<B, F, Fut>(
    request: Request<B>,
    config: WebSocketConfig,
    on_socket: F,
) -> Result<Response<Empty<Bytes>>, UpgradeRejection>
where
    B: Send + 'static,
    F: FnOnce(WebSocketStream<TokioIo<Upgraded>>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let response = accept_response(&request)?;
    let on_upgrade = hyper::upgrade::on(request);
    tokio::spawn(async move {
        match on_upgrade.await {
            Ok(upgraded) => {
                let socket = WebSocketStream::from_raw_socket(
                    TokioIo::new(upgraded),
                    Role::Server,
                    Some(config),
                )
                .await;
                on_socket(socket).await;
            }
            Err(error) => {
                tracing::warn!(%error, "session websocket upgrade did not complete");
            }
        }
    });
    Ok(response)
}

/// Client side upgrade request for `authority` (Host header) with a fresh
/// key. Returns the request and the key (for [`verify_client_response`]).
pub fn client_request(authority: &str) -> Result<(Request<Empty<Bytes>>, String), http::Error> {
    let key = generate_key();
    let request = Request::builder()
        .method(Method::GET)
        .uri(SESSION_WS_PATH)
        .header(header::HOST, authority)
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, WS_VERSION)
        .header(header::SEC_WEBSOCKET_KEY, key.as_str())
        .header(header::SEC_WEBSOCKET_PROTOCOL, SESSION_WS_SUBPROTOCOL)
        .body(Empty::new())?;
    Ok((request, key))
}

/// Verify a 101 response against the sent key: status 101,
/// `Upgrade: websocket`, `Connection: Upgrade`, matching
/// `Sec-WebSocket-Accept` and exactly the subprotocol `harw.session.v1`.
pub fn verify_client_response<B>(
    response: &Response<B>,
    key: &str,
) -> Result<(), UpgradeRejection> {
    let headers = response.headers();
    if response.status() != StatusCode::SWITCHING_PROTOCOLS
        || !has_token_ci(headers, &header::CONNECTION, "upgrade")
        || !has_token_ci(headers, &header::UPGRADE, "websocket")
    {
        return Err(UpgradeRejection::NotAnUpgrade);
    }
    let expected = derive_accept_key(key.as_bytes());
    match single_value(headers, &header::SEC_WEBSOCKET_ACCEPT) {
        Some(accept) if accept.trim() == expected => {}
        _ => return Err(UpgradeRejection::BadAccept),
    }
    // The server must select exactly one protocol, and it must be ours.
    match single_value(headers, &header::SEC_WEBSOCKET_PROTOCOL) {
        Some(protocol) if protocol.trim() == SESSION_WS_SUBPROTOCOL => Ok(()),
        _ => Err(UpgradeRejection::MissingSubprotocol),
    }
}

/// Comma-separated tokens of every occurrence of `name` (non-UTF-8 values
/// are skipped, i.e. never match).
fn header_tokens<'a>(headers: &'a HeaderMap, name: &HeaderName) -> impl Iterator<Item = &'a str> {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
}

/// Whether `name` carries `token`, compared ASCII case-insensitively.
fn has_token_ci(headers: &HeaderMap, name: &HeaderName, token: &str) -> bool {
    header_tokens(headers, name).any(|candidate| candidate.eq_ignore_ascii_case(token))
}

/// The value of `name` if it occurs exactly once and is valid UTF-8.
fn single_value<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?;
    if values.next().is_some() {
        return None;
    }
    first.to_str().ok()
}

/// Canonical base64 of exactly 16 bytes: 22 alphabet characters (the last
/// one with its 4 padding bits clear) followed by `==`.
fn is_valid_key(key: &[u8]) -> bool {
    match key {
        [body @ .., last, b'=', b'='] if key.len() == 24 => {
            body.iter().all(|byte| is_base64_char(*byte))
                && matches!(*last, b'A' | b'Q' | b'g' | b'w')
        }
        _ => false,
    }
}

fn is_base64_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/'
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    const SAMPLE_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
    const SAMPLE_ACCEPT: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

    fn ensure(condition: bool, message: &str) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.to_owned())
        }
    }

    fn ctx<T, E: fmt::Display>(result: Result<T, E>, context: &str) -> Result<T, String> {
        result.map_err(|error| format!("{context}: {error}"))
    }

    fn valid_builder() -> http::request::Builder {
        Request::builder()
            .method(Method::GET)
            .uri(SESSION_WS_PATH)
            .header(header::HOST, "localhost")
            .header(header::CONNECTION, "keep-alive, Upgrade")
            .header(header::UPGRADE, "WebSocket")
            .header(header::SEC_WEBSOCKET_VERSION, "13")
            .header(header::SEC_WEBSOCKET_KEY, SAMPLE_KEY)
            .header(header::SEC_WEBSOCKET_PROTOCOL, "other.v2, harw.session.v1")
    }

    fn build(builder: http::request::Builder) -> Result<Request<()>, String> {
        ctx(builder.body(()), "build request")
    }

    fn expect_rejection(request: &Request<()>, expected: UpgradeRejection) -> TestResult {
        match validate_upgrade(request) {
            Err(rejection) if rejection == expected => Ok(()),
            other => Err(format!("expected {expected:?}, got {other:?}")),
        }
    }

    fn header_str<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Result<&'a str, String> {
        let value = headers
            .get(name)
            .ok_or_else(|| format!("missing header {name}"))?;
        ctx(value.to_str(), "header to str")
    }

    #[test]
    fn valid_request_is_accepted_with_rfc6455_sample_accept_key() -> TestResult {
        let request = build(valid_builder())?;
        ctx(validate_upgrade(&request), "validate")?;
        let response = ctx(accept_response(&request), "accept")?;
        ensure(
            response.status() == StatusCode::SWITCHING_PROTOCOLS,
            "status must be 101",
        )?;
        let headers = response.headers();
        ensure(
            header_str(headers, &header::SEC_WEBSOCKET_ACCEPT)? == SAMPLE_ACCEPT,
            "accept key must match the RFC 6455 sample",
        )?;
        ensure(
            header_str(headers, &header::SEC_WEBSOCKET_PROTOCOL)? == SESSION_WS_SUBPROTOCOL,
            "server answers with exactly harw.session.v1",
        )?;
        ensure(
            header_str(headers, &header::UPGRADE)?.eq_ignore_ascii_case("websocket"),
            "upgrade header",
        )?;
        ensure(
            header_str(headers, &header::CONNECTION)?.eq_ignore_ascii_case("upgrade"),
            "connection header",
        )
    }

    #[test]
    fn missing_or_wrong_subprotocol_is_refused_ws01() -> TestResult {
        let mut builder = valid_builder();
        if let Some(headers) = builder.headers_mut() {
            headers.remove(header::SEC_WEBSOCKET_PROTOCOL);
        }
        expect_rejection(&build(builder)?, UpgradeRejection::MissingSubprotocol)?;

        for wrong in ["harw.session.v2", "HARW.SESSION.V1", "harw.session", ""] {
            // `insert` replaces the valid offer from the builder.
            let mut request = build(valid_builder())?;
            request.headers_mut().insert(
                header::SEC_WEBSOCKET_PROTOCOL,
                ctx(HeaderValue::from_str(wrong), "header value")?,
            );
            expect_rejection(&request, UpgradeRejection::MissingSubprotocol)?;
        }
        let rejection = UpgradeRejection::MissingSubprotocol;
        ensure(
            rejection.status() == StatusCode::BAD_REQUEST,
            "WS-01 maps to 400",
        )
    }

    #[test]
    fn origin_header_is_refused_ws04() -> TestResult {
        let request = build(valid_builder().header(header::ORIGIN, "https://example.com"))?;
        expect_rejection(&request, UpgradeRejection::OriginRefused)?;
        match accept_response(&request) {
            Err(UpgradeRejection::OriginRefused) => {}
            other => {
                return Err(format!(
                    "accept_response must refuse, got {:?}",
                    other.map(|response| response.status())
                ));
            }
        }
        let response = rejection_response(&UpgradeRejection::OriginRefused);
        ensure(
            response.status() == StatusCode::FORBIDDEN,
            "WS-04 maps to 403",
        )
    }

    #[test]
    fn wrong_method_path_version_and_key_are_refused() -> TestResult {
        expect_rejection(
            &build(valid_builder().method(Method::POST))?,
            UpgradeRejection::WrongMethod,
        )?;
        expect_rejection(
            &build(valid_builder().uri("/v1/other"))?,
            UpgradeRejection::WrongPath,
        )?;

        let mut request = build(valid_builder())?;
        request
            .headers_mut()
            .insert(header::SEC_WEBSOCKET_VERSION, HeaderValue::from_static("8"));
        expect_rejection(&request, UpgradeRejection::WrongVersion)?;
        request.headers_mut().remove(header::SEC_WEBSOCKET_VERSION);
        expect_rejection(&request, UpgradeRejection::WrongVersion)?;

        for bad_key in [
            "",
            "c2hvcnQ=",
            "dGhlIHNhbXBsZSBub25jZR==",
            "dGhlIHNhbXBsZSBub25j*Q==",
        ] {
            let mut request = build(valid_builder())?;
            request.headers_mut().insert(
                header::SEC_WEBSOCKET_KEY,
                ctx(HeaderValue::from_str(bad_key), "header value")?,
            );
            expect_rejection(&request, UpgradeRejection::MissingKey)?;
        }

        let mut request = build(valid_builder())?;
        request
            .headers_mut()
            .insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
        expect_rejection(&request, UpgradeRejection::NotAnUpgrade)?;

        let response = rejection_response(&UpgradeRejection::WrongVersion);
        ensure(
            response.status() == StatusCode::UPGRADE_REQUIRED
                && header_str(response.headers(), &header::SEC_WEBSOCKET_VERSION)? == "13",
            "426 advertises version 13",
        )
    }

    #[test]
    fn client_request_accept_response_and_verify_round_trip() -> TestResult {
        let (request, key) = ctx(client_request("127.0.0.1:7000"), "client request")?;
        ensure(
            is_valid_key(key.as_bytes()),
            "generated key is base64 of 16 bytes",
        )?;
        ensure(
            header_str(request.headers(), &header::HOST)? == "127.0.0.1:7000",
            "host header",
        )?;
        let response = ctx(accept_response(&request), "accept")?;
        ctx(verify_client_response(&response, &key), "verify")
    }

    #[test]
    fn wrong_accept_or_protocol_is_rejected_by_the_client() -> TestResult {
        let (request, key) = ctx(client_request("localhost"), "client request")?;
        let response = ctx(accept_response(&request), "accept")?;
        match verify_client_response(&response, SAMPLE_KEY) {
            Err(UpgradeRejection::BadAccept) => {}
            other => return Err(format!("expected BadAccept, got {other:?}")),
        }

        let mut tampered = ctx(accept_response(&request), "accept")?;
        tampered.headers_mut().insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("harw.session.v2"),
        );
        match verify_client_response(&tampered, &key) {
            Err(UpgradeRejection::MissingSubprotocol) => {}
            other => return Err(format!("expected MissingSubprotocol, got {other:?}")),
        }

        let mut not_switched = ctx(accept_response(&request), "accept")?;
        *not_switched.status_mut() = StatusCode::OK;
        match verify_client_response(&not_switched, &key) {
            Err(UpgradeRejection::NotAnUpgrade) => Ok(()),
            other => Err(format!("expected NotAnUpgrade, got {other:?}")),
        }
    }
}
