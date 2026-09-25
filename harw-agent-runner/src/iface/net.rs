//! Network hardening shared by the two listening interfaces, `iface::http`
//! and `iface::mcp`'s Streamable HTTP transport (`--listen`).
//!
//! # Description
//! Both listeners enforce the same small policy, so it lives here once
//! instead of as two copies kept in lock-step by convention:
//! - [`validate_listen_requirements`]: a non-loopback bind requires a
//!   bearer token (`HARW_AGENT_HTTP_TOKEN`).
//! - [`bearer_authorized`]/[`constant_time_eq`]: constant-time bearer check.
//! - [`origin_is_loopback`]: a browser-supplied `Origin` must name a
//!   loopback host (DNS-rebinding guard); an absent `Origin` passes, since
//!   native clients normally omit it.
//! - [`read_json_body`]/[`decode_json_body`]: bounded, time-limited body
//!   read, then a nesting-depth check ([`json_too_deep`]) before
//!   `serde_json` ever sees the bytes.
//!
//! Every helper is a small pure function over headers/bytes (except the
//! body read itself), so each is unit tested below without a socket.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Body;
use hyper::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, ORIGIN};
use hyper::{HeaderMap, Request, Response, StatusCode};
use serde_json::{Value, json};

use crate::child_protocol::{MAX_JSON_NESTING_DEPTH, json_nesting_too_deep};

/// The response type both listeners build.
pub(crate) type HttpResponse = Response<BoxBody<Bytes, Infallible>>;

/// A bind address requires a configured token unless it is loopback-only.
/// `surface` names the interface in the error message (e.g. `"the HTTP
/// interface"`).
pub(crate) fn validate_listen_requirements(
    addr: SocketAddr,
    token: Option<&str>,
    surface: &str,
) -> Result<(), String> {
    if !addr.ip().is_loopback() && token.is_none() {
        return Err(format!(
            "{surface} on a non-loopback address requires HARW_AGENT_HTTP_TOKEN"
        ));
    }
    Ok(())
}

/// Constant-time bearer check; `None` (no configured token) always passes.
/// Accepts the `Bearer ` and `bearer ` scheme spellings.
pub(crate) fn bearer_authorized(expected: Option<&[u8]>, headers: &HeaderMap) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    let Some(header) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(presented) = header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))
    else {
        return false;
    };
    constant_time_eq(expected, presented.as_bytes())
}

/// Compares two byte strings without an early exit on the first mismatch,
/// so the time taken does not leak how long a matching prefix was.
pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

/// Rejects a request whose `Origin` header does not name a loopback host —
/// MCP's Streamable HTTP transport recommends this to guard against DNS
/// rebinding attacks from a browser, and the plain HTTP interface applies
/// the same policy. A native client normally omits `Origin` entirely, so
/// only a *supplied* one is checked (structurally the same policy as
/// `harw-mcp-server::transport::origin_is_loopback`).
pub(crate) fn origin_is_loopback(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Some(authority) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = authority.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return false;
    }
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// `true` if `input` nests `[`/`{` deeper than [`MAX_JSON_NESTING_DEPTH`].
/// Checked before `serde_json`'s recursive-descent parser (which has no
/// depth limit of its own) sees a body or stdio line.
pub(crate) fn json_too_deep(input: impl AsRef<[u8]>) -> bool {
    json_nesting_too_deep(input, MAX_JSON_NESTING_DEPTH)
}

/// A JSON response with `Content-Type: application/json`.
pub(crate) fn json_response(status: StatusCode, body: &Value) -> HttpResponse {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body.to_string())).boxed())
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()).boxed()))
}

/// Decodes an already-read request body: empty is `Value::Null`, too deep
/// or unparsable is a ready `400` response.
// The `Err` variant carries a fully-built `HttpResponse` so callers return
// it unchanged; it is never cloned, so the size is not a real cost.
#[allow(clippy::result_large_err)]
pub(crate) fn decode_json_body(bytes: &[u8]) -> Result<Value, HttpResponse> {
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    // Reject a shallow-but-deeply-nested body (e.g. megabytes of `[[[[...`)
    // before handing it to `serde_json`, which could exhaust the stack on
    // such input even within the byte cap.
    if json_too_deep(bytes) {
        return Err(json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error": "invalid_json", "detail": "too deeply nested"}),
        ));
    }
    serde_json::from_slice(bytes)
        .map_err(|_| json_response(StatusCode::BAD_REQUEST, &json!({"error": "invalid_json"})))
}

/// Reads a request body bounded by `max_bytes` (declared `Content-Length`
/// and actual length) and `read_timeout`, then [`decode_json_body`]s it.
/// Generic over the body type so tests can feed an in-memory body; the
/// listeners pass hyper's `Incoming`.
// See `decode_json_body` for the `result_large_err` allowance.
#[allow(clippy::result_large_err)]
pub(crate) async fn read_json_body<B>(
    request: Request<B>,
    max_bytes: usize,
    read_timeout: Duration,
) -> Result<Value, HttpResponse>
where
    B: Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let bytes = read_body_bytes(request, max_bytes, read_timeout).await?;
    decode_json_body(&bytes)
}

/// The bounded, time-limited read half of [`read_json_body`].
#[allow(clippy::result_large_err)]
async fn read_body_bytes<B>(
    request: Request<B>,
    max_bytes: usize,
    read_timeout: Duration,
) -> Result<Bytes, HttpResponse>
where
    B: Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let declared_len = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared_len.is_some_and(|len| len > max_bytes as u64) {
        return Err(json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            &json!({"error": "payload_too_large"}),
        ));
    }
    let limited = Limited::new(request.into_body(), max_bytes);
    match tokio::time::timeout(read_timeout, limited.collect()).await {
        Ok(Ok(collected)) => Ok(collected.to_bytes()),
        Ok(Err(error)) if error.downcast_ref::<LengthLimitError>().is_some() => Err(
            json_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                &json!({"error": "payload_too_large"}),
            ),
        ),
        Ok(Err(_)) => Err(json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error": "bad_request"}),
        )),
        Err(_elapsed) => Err(json_response(
            StatusCode::REQUEST_TIMEOUT,
            &json!({"error": "request_timeout"}),
        )),
    }
}

#[cfg(test)]
mod tests {
    use hyper::header::HeaderValue;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn with_header(
        name: hyper::header::HeaderName,
        value: &str,
    ) -> Result<HeaderMap, Box<dyn std::error::Error>> {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_str(value)?);
        Ok(headers)
    }

    fn origin(value: &str) -> Result<HeaderMap, Box<dyn std::error::Error>> {
        with_header(ORIGIN, value)
    }

    fn nested(depth: usize) -> String {
        format!("{}{}", "[".repeat(depth), "]".repeat(depth))
    }

    // ── validate_listen_requirements ─────────────────────────────────────

    #[test]
    fn non_loopback_bind_without_token_is_refused_and_names_the_surface() -> TestResult {
        let addr: SocketAddr = "0.0.0.0:8787".parse()?;
        let error = validate_listen_requirements(addr, None, "the test interface")
            .err()
            .ok_or("expected a refusal")?;
        assert!(error.contains("the test interface"), "{error}");
        assert!(error.contains("HARW_AGENT_HTTP_TOKEN"), "{error}");
        Ok(())
    }

    #[test]
    fn non_loopback_bind_with_token_is_allowed() -> TestResult {
        let addr: SocketAddr = "0.0.0.0:8787".parse()?;
        assert!(validate_listen_requirements(addr, Some("secret"), "x").is_ok());
        Ok(())
    }

    #[test]
    fn loopback_binds_without_token_are_allowed() -> TestResult {
        for text in ["127.0.0.1:8787", "[::1]:8787"] {
            let addr: SocketAddr = text.parse()?;
            assert!(validate_listen_requirements(addr, None, "x").is_ok(), "{text}");
        }
        Ok(())
    }

    // ── constant_time_eq / bearer_authorized ─────────────────────────────

    #[test]
    fn constant_time_eq_matches_only_identical_bytes() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret-longer"));
        assert!(!constant_time_eq(b"secret", b""));
    }

    #[test]
    fn bearer_without_configured_token_always_passes() {
        assert!(bearer_authorized(None, &HeaderMap::new()));
    }

    #[test]
    fn bearer_with_configured_token_requires_a_match() -> TestResult {
        let expected = Some(b"tok".as_slice());
        assert!(!bearer_authorized(expected, &HeaderMap::new()));
        assert!(bearer_authorized(
            expected,
            &with_header(AUTHORIZATION, "Bearer tok")?
        ));
        assert!(bearer_authorized(
            expected,
            &with_header(AUTHORIZATION, "bearer tok")?
        ));
        assert!(!bearer_authorized(
            expected,
            &with_header(AUTHORIZATION, "Bearer wrong")?
        ));
        assert!(!bearer_authorized(
            expected,
            &with_header(AUTHORIZATION, "Basic tok")?
        ));
        Ok(())
    }

    // ── origin_is_loopback ───────────────────────────────────────────────

    #[test]
    fn absent_origin_passes() {
        assert!(origin_is_loopback(&HeaderMap::new()));
    }

    #[test]
    fn loopback_origins_pass() -> TestResult {
        for value in [
            "http://localhost",
            "http://localhost:5173",
            "https://LOCALHOST:443/path",
            "http://127.0.0.1:8787",
            "http://127.1.2.3",
            "http://[::1]",
            "http://[::1]:8787",
            "https://[::1]:8787/?q#f",
        ] {
            assert!(origin_is_loopback(&origin(value)?), "{value}");
        }
        Ok(())
    }

    #[test]
    fn non_loopback_or_malformed_origins_are_refused() -> TestResult {
        for value in [
            "http://evil.example",
            "http://localhost.evil.example",
            "http://10.0.0.1:8787",
            "http://[2001:db8::1]:8787",
            "http://[::2]",
            "null",
            "file://localhost",
            "http://",
            "localhost",
        ] {
            assert!(!origin_is_loopback(&origin(value)?), "{value}");
        }
        Ok(())
    }

    // ── json_too_deep / decode_json_body ─────────────────────────────────

    #[test]
    fn json_depth_limit_is_inclusive() {
        assert!(!json_too_deep(nested(MAX_JSON_NESTING_DEPTH)));
        assert!(json_too_deep(nested(MAX_JSON_NESTING_DEPTH + 1)));
        // Brackets inside a string do not count.
        let quoted = format!("\"{}\"", "[".repeat(MAX_JSON_NESTING_DEPTH + 10));
        assert!(!json_too_deep(quoted));
    }

    #[test]
    fn decode_empty_body_is_null() -> TestResult {
        let value = decode_json_body(b"").map_err(|_| "empty body must decode")?;
        assert_eq!(value, Value::Null);
        Ok(())
    }

    #[test]
    fn decode_valid_body_parses() -> TestResult {
        let value = decode_json_body(br#"{"prompt":"hi"}"#)
            .map_err(|_| "valid body must decode")?;
        assert_eq!(value["prompt"], "hi");
        Ok(())
    }

    #[test]
    fn decode_over_deep_body_is_400() -> TestResult {
        let response = decode_json_body(nested(MAX_JSON_NESTING_DEPTH + 1).as_bytes())
            .err()
            .ok_or("an over-deep body must be refused")?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        Ok(())
    }

    #[test]
    fn decode_invalid_json_is_400() -> TestResult {
        let response = decode_json_body(b"{not json")
            .err()
            .ok_or("invalid JSON must be refused")?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        Ok(())
    }

    // ── read_json_body ───────────────────────────────────────────────────

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    fn request_with(body: impl Into<Bytes>) -> Request<Full<Bytes>> {
        Request::new(Full::new(body.into()))
    }

    #[tokio::test]
    async fn read_valid_body_parses() -> TestResult {
        let value = read_json_body(request_with(r#"{"prompt":"hi"}"#), 1024, TEST_TIMEOUT)
            .await
            .map_err(|_| "valid body must decode")?;
        assert_eq!(value["prompt"], "hi");
        Ok(())
    }

    #[tokio::test]
    async fn read_over_deep_body_is_400() -> TestResult {
        let body = nested(MAX_JSON_NESTING_DEPTH + 1);
        let response = read_json_body(request_with(body), 1024 * 1024, TEST_TIMEOUT)
            .await
            .err()
            .ok_or("an over-deep body must be refused")?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        Ok(())
    }

    #[tokio::test]
    async fn read_body_over_the_byte_cap_is_413() -> TestResult {
        let response = read_json_body(request_with(vec![b' '; 64]), 16, TEST_TIMEOUT)
            .await
            .err()
            .ok_or("an oversized body must be refused")?;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        Ok(())
    }

    #[tokio::test]
    async fn read_declared_length_over_the_cap_is_413() -> TestResult {
        let mut request = request_with("{}");
        request
            .headers_mut()
            .insert(CONTENT_LENGTH, HeaderValue::from_static("999999"));
        let response = read_json_body(request, 16, TEST_TIMEOUT)
            .await
            .err()
            .ok_or("an oversized declared length must be refused")?;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        Ok(())
    }

    #[test]
    fn json_response_sets_status_and_content_type() {
        let response = json_response(StatusCode::CREATED, &json!({"ok": true}));
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
    }
}
