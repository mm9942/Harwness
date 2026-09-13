//! Minimal Streamable HTTP MCP client.
//!
//! This client implements the JSON and finite-SSE-response branches of the
//! MCP 2025-06-18 Streamable HTTP transport: each JSON-RPC message is a POST,
//! session IDs returned by `initialize` are replayed on later requests, and a
//! 404 resets the local session rather than accidentally reusing stale
//! authority. Server notifications received on an SSE response are retained
//! for the supervisor; server-initiated requests remain an explicit boundary
//! because they need a supervisor-owned reply policy.

use std::collections::VecDeque;
use std::fmt;

use harw_tools::ToolName;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};

use crate::mcp_runtime::StreamableHttpMcpPlan;

const PROTOCOL_VERSION: &str = "2025-06-18";
const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";
const MAX_PENDING_NOTIFICATIONS: usize = 128;

pub type McpHttpResult<T> = Result<T, McpHttpError>;

#[derive(Debug)]
pub enum McpHttpError {
    Http(reqwest::Error),
    InvalidHeaderValue {
        name: &'static str,
    },
    InvalidSessionId,
    UnexpectedStatus {
        status: u16,
        body: String,
    },
    SessionExpired,
    InvalidSse(String),
    SseResponseMissing,
    ServerRequestUnsupported {
        method: String,
    },
    NotificationBufferFull,
    InvalidJsonRpc(String),
    JsonRpcFailure {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    NotInitialized,
}

impl fmt::Display for McpHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(error) => write!(f, "MCP HTTP transport failed: {error}"),
            Self::InvalidHeaderValue { name } => write!(f, "invalid MCP HTTP header value: {name}"),
            Self::InvalidSessionId => write!(f, "server returned an invalid MCP session id"),
            Self::UnexpectedStatus { status, body } => {
                write!(f, "MCP server returned HTTP {status}: {body}")
            }
            Self::SessionExpired => write!(f, "MCP server session expired; initialize again"),
            Self::InvalidSse(reason) => write!(f, "invalid MCP SSE response: {reason}"),
            Self::SseResponseMissing => write!(
                f,
                "MCP SSE response ended without a JSON-RPC response for the request"
            ),
            Self::ServerRequestUnsupported { method } => write!(
                f,
                "MCP server requested '{method}', but no supervisor reply policy is configured"
            ),
            Self::NotificationBufferFull => write!(
                f,
                "MCP notification buffer is full; supervisor must drain notifications"
            ),
            Self::InvalidJsonRpc(reason) => write!(f, "invalid MCP JSON-RPC response: {reason}"),
            Self::JsonRpcFailure { code, message, .. } => {
                write!(f, "MCP JSON-RPC error {code}: {message}")
            }
            Self::NotInitialized => write!(f, "MCP client must initialize before making requests"),
        }
    }
}

/// A server notification observed while consuming an MCP SSE response.
///
/// The client intentionally does not interpret notification methods: the
/// supervisor decides which progress, logging, or job-state messages are safe
/// to retain in durable run state.
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerNotification {
    pub method: String,
    pub params: Option<Value>,
}

impl std::error::Error for McpHttpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Http(error) => Some(error),
            _ => None,
        }
    }
}

/// Session-bound client. Construct it with an already configured `reqwest`
/// client so auth headers/MTLS/proxy policy remain a composition concern and
/// no secret leaks into catalog or plan types.
pub struct StreamableHttpMcpClient {
    plan: StreamableHttpMcpPlan,
    client: reqwest::Client,
    session_id: Option<String>,
    protocol_version: String,
    next_request_id: u64,
    initialized: bool,
    pending_notifications: VecDeque<McpServerNotification>,
}

impl StreamableHttpMcpClient {
    #[must_use]
    pub fn new(plan: StreamableHttpMcpPlan, client: reqwest::Client) -> Self {
        Self {
            plan,
            client,
            session_id: None,
            protocol_version: PROTOCOL_VERSION.to_owned(),
            next_request_id: 1,
            initialized: false,
            pending_notifications: VecDeque::new(),
        }
    }

    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    #[must_use]
    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    /// Drains notifications in the order received from finite SSE responses.
    pub fn take_notifications(&mut self) -> Vec<McpServerNotification> {
        self.pending_notifications.drain(..).collect()
    }

    /// Negotiates the protocol and sends the required `notifications/initialized`
    /// notification after a valid initialize response.
    pub async fn initialize(
        &mut self,
        client_name: &str,
        client_version: &str,
    ) -> McpHttpResult<Value> {
        self.session_id = None;
        self.initialized = false;
        self.protocol_version = PROTOCOL_VERSION.to_owned();
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": client_name, "version": client_version},
                }),
                true,
            )
            .await?;
        let negotiated = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                McpHttpError::InvalidJsonRpc("initialize result has no protocolVersion".to_owned())
            })?;
        self.protocol_version = negotiated.to_owned();
        self.initialized = true;
        self.notify(
            "notifications/initialized",
            Value::Object(Default::default()),
        )
        .await?;
        Ok(result)
    }

    /// Calls one tool using a monotonically increasing JSON-RPC request id.
    pub async fn call_tool(&mut self, name: &ToolName, arguments: Value) -> McpHttpResult<Value> {
        if !self.initialized {
            return Err(McpHttpError::NotInitialized);
        }
        self.request(
            "tools/call",
            json!({"name": name.as_str(), "arguments": arguments}),
            false,
        )
        .await
    }

    /// Best-effort explicit server-side session teardown. HTTP 405 is accepted
    /// because servers may disallow client-directed deletion.
    pub async fn close(&mut self) -> McpHttpResult<()> {
        let Some(session) = self.session_id.clone() else {
            return Ok(());
        };
        let mut request = self.client.delete(self.plan.endpoint.clone());
        request = request.headers(self.headers(true, Some(&session))?);
        let response = request.send().await.map_err(McpHttpError::Http)?;
        let status = response.status().as_u16();
        if !(response.status().is_success() || status == 405 || status == 404) {
            let body = response.text().await.map_err(McpHttpError::Http)?;
            return Err(McpHttpError::UnexpectedStatus { status, body });
        }
        self.session_id = None;
        self.initialized = false;
        Ok(())
    }

    async fn notify(&mut self, method: &str, params: Value) -> McpHttpResult<()> {
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let response = self.post(message, false).await?;
        let status = response.status().as_u16();
        if !(response.status().is_success() || status == 202) {
            let body = response.text().await.map_err(McpHttpError::Http)?;
            return Err(McpHttpError::UnexpectedStatus { status, body });
        }
        Ok(())
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
        initializing: bool,
    ) -> McpHttpResult<Value> {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.checked_add(1).ok_or_else(|| {
            McpHttpError::InvalidJsonRpc("request id counter exhausted".to_owned())
        })?;
        let response = self
            .post(
                json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
                initializing,
            )
            .await?;
        let status = response.status().as_u16();
        if status == 404 && !initializing && self.session_id.is_some() {
            self.session_id = None;
            self.initialized = false;
            return Err(McpHttpError::SessionExpired);
        }
        if !response.status().is_success() {
            let body = response.text().await.map_err(McpHttpError::Http)?;
            return Err(McpHttpError::UnexpectedStatus { status, body });
        }
        if initializing {
            if let Some(session) = response.headers().get(SESSION_HEADER) {
                let session = session
                    .to_str()
                    .map_err(|_| McpHttpError::InvalidSessionId)?;
                if !valid_session_id(session) {
                    return Err(McpHttpError::InvalidSessionId);
                }
                self.session_id = Some(session.to_owned());
            }
        }
        if response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"))
        {
            return self.consume_sse_response(response, id).await;
        }
        let value = response.json::<Value>().await.map_err(McpHttpError::Http)?;
        parse_json_rpc_response(value, id)
    }

    async fn consume_sse_response(
        &mut self,
        mut response: reqwest::Response,
        expected_id: u64,
    ) -> McpHttpResult<Value> {
        let mut decoder = SseDecoder::default();
        while let Some(chunk) = response.chunk().await.map_err(McpHttpError::Http)? {
            for event in decoder.push_chunk(&chunk)? {
                if let Some(result) = self.handle_sse_event(event, expected_id)? {
                    return Ok(result);
                }
            }
        }
        for event in decoder.finish()? {
            if let Some(result) = self.handle_sse_event(event, expected_id)? {
                return Ok(result);
            }
        }
        Err(McpHttpError::SseResponseMissing)
    }

    fn handle_sse_event(
        &mut self,
        event: SseEvent,
        expected_id: u64,
    ) -> McpHttpResult<Option<Value>> {
        let value = serde_json::from_str::<Value>(&event.data).map_err(|error| {
            McpHttpError::InvalidSse(format!("event payload is not JSON-RPC JSON: {error}"))
        })?;
        let object = value.as_object().ok_or_else(|| {
            McpHttpError::InvalidSse("event payload is not a JSON-RPC object".to_owned())
        })?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(McpHttpError::InvalidSse(
                "event jsonrpc must equal '2.0'".to_owned(),
            ));
        }

        if object.contains_key("id") {
            if object.contains_key("method") {
                let method = object
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("<invalid method>")
                    .to_owned();
                return Err(McpHttpError::ServerRequestUnsupported { method });
            }
            return parse_json_rpc_response(value, expected_id).map(Some);
        }

        let method = object
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                McpHttpError::InvalidSse("event has neither response id nor method".to_owned())
            })?
            .to_owned();
        if self.pending_notifications.len() == MAX_PENDING_NOTIFICATIONS {
            return Err(McpHttpError::NotificationBufferFull);
        }
        self.pending_notifications.push_back(McpServerNotification {
            method,
            params: object.get("params").cloned(),
        });
        Ok(None)
    }

    async fn post(&self, message: Value, initializing: bool) -> McpHttpResult<reqwest::Response> {
        let headers = self.headers(!initializing, self.session_id.as_deref())?;
        self.client
            .post(self.plan.endpoint.clone())
            .headers(headers)
            .json(&message)
            .send()
            .await
            .map_err(McpHttpError::Http)
    }

    fn headers(&self, include_protocol: bool, session: Option<&str>) -> McpHttpResult<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if include_protocol {
            headers.insert(
                HeaderName::from_static(PROTOCOL_HEADER),
                HeaderValue::from_str(&self.protocol_version).map_err(|_| {
                    McpHttpError::InvalidHeaderValue {
                        name: PROTOCOL_HEADER,
                    }
                })?,
            );
        }
        if let Some(session) = session {
            headers.insert(
                HeaderName::from_static(SESSION_HEADER),
                HeaderValue::from_str(session).map_err(|_| McpHttpError::InvalidHeaderValue {
                    name: SESSION_HEADER,
                })?,
            );
        }
        Ok(headers)
    }
}

fn valid_session_id(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn parse_json_rpc_response(value: Value, expected_id: u64) -> McpHttpResult<Value> {
    let object = value
        .as_object()
        .ok_or_else(|| McpHttpError::InvalidJsonRpc("response is not an object".to_owned()))?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(McpHttpError::InvalidJsonRpc(
            "jsonrpc must equal '2.0'".to_owned(),
        ));
    }
    if object.get("id").and_then(Value::as_u64) != Some(expected_id) {
        return Err(McpHttpError::InvalidJsonRpc(
            "response id does not match request".to_owned(),
        ));
    }
    if let Some(error) = object.get("error").and_then(Value::as_object) {
        return Err(McpHttpError::JsonRpcFailure {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(-32_000),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unspecified MCP error")
                .to_owned(),
            data: error.get("data").cloned(),
        });
    }
    object.get("result").cloned().ok_or_else(|| {
        McpHttpError::InvalidJsonRpc("response has neither result nor error".to_owned())
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SseEvent {
    data: String,
}

/// Incremental, bounded-by-caller SSE framing parser. The transport reader
/// feeds network chunks into it, so multi-byte UTF-8 characters and SSE lines
/// split at arbitrary chunk boundaries are preserved correctly.
#[derive(Default)]
struct SseDecoder {
    line: Vec<u8>,
    data_lines: Vec<Vec<u8>>,
}

impl SseDecoder {
    fn push_chunk(&mut self, chunk: &[u8]) -> McpHttpResult<Vec<SseEvent>> {
        let mut events = Vec::new();
        for &byte in chunk {
            if byte == b'\n' {
                if let Some(event) = self.finish_line()? {
                    events.push(event);
                }
            } else {
                self.line.push(byte);
            }
        }
        Ok(events)
    }

    fn finish(&mut self) -> McpHttpResult<Vec<SseEvent>> {
        let mut events = Vec::new();
        if !self.line.is_empty() {
            if let Some(event) = self.finish_line()? {
                events.push(event);
            }
        }
        if let Some(event) = self.finish_event()? {
            events.push(event);
        }
        Ok(events)
    }

    fn finish_line(&mut self) -> McpHttpResult<Option<SseEvent>> {
        if self.line.last() == Some(&b'\r') {
            self.line.pop();
        }
        let line = std::mem::take(&mut self.line);
        if line.is_empty() {
            return self.finish_event();
        }
        if line.starts_with(b":") {
            return Ok(None);
        }
        let (field, value) = if let Some(index) = line.iter().position(|byte| *byte == b':') {
            (&line[..index], &line[index + 1..])
        } else {
            (&line[..], &line[line.len()..])
        };
        if field == b"data" {
            self.data_lines
                .push(value.strip_prefix(b" ").unwrap_or(value).to_vec());
        }
        Ok(None)
    }

    fn finish_event(&mut self) -> McpHttpResult<Option<SseEvent>> {
        if self.data_lines.is_empty() {
            return Ok(None);
        }
        let mut data = Vec::new();
        for (index, line) in self.data_lines.drain(..).enumerate() {
            if index > 0 {
                data.push(b'\n');
            }
            data.extend(line);
        }
        let data = String::from_utf8(data).map_err(|error| {
            McpHttpError::InvalidSse(format!("event data is not UTF-8: {error}"))
        })?;
        Ok(Some(SseEvent { data }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_rpc_response_requires_matching_id_and_preserves_structured_error() {
        let result = parse_json_rpc_response(
            json!({"jsonrpc": "2.0", "id": 7, "result": {"ok": true}}),
            7,
        )
        .unwrap();
        assert_eq!(result["ok"], true);
        assert!(matches!(
            parse_json_rpc_response(json!({"jsonrpc": "2.0", "id": 8, "result": {}}), 7),
            Err(McpHttpError::InvalidJsonRpc(_))
        ));
        assert!(matches!(
            parse_json_rpc_response(
                json!({"jsonrpc": "2.0", "id": 7, "error": {"code": -32601, "message": "missing"}}),
                7
            ),
            Err(McpHttpError::JsonRpcFailure { code: -32601, .. })
        ));
    }

    #[test]
    fn session_ids_must_be_visible_ascii() {
        assert!(valid_session_id("session-ABC_123"));
        assert!(!valid_session_id(""));
        assert!(!valid_session_id("line\nbreak"));
        assert!(!valid_session_id("é"));
    }

    #[test]
    fn sse_decoder_preserves_chunked_utf8_and_multiline_data() {
        let mut decoder = SseDecoder::default();
        assert!(
            decoder
                .push_chunk(b": keepalive\r\ndata: {\"message\":\"Gr\xc3")
                .unwrap()
                .is_empty()
        );
        let events = decoder.push_chunk(b"\xbc\xc3\x9fe\"}\ndata: \n\n").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "{\"message\":\"Grüße\"}\n");
    }

    #[test]
    fn sse_decoder_emits_an_unterminated_final_event() {
        let mut decoder = SseDecoder::default();
        assert!(
            decoder
                .push_chunk(b"event: message\ndata: {\"jsonrpc\":\"2.0\"}")
                .unwrap()
                .is_empty()
        );
        let events = decoder.finish().unwrap();
        assert_eq!(
            events,
            vec![SseEvent {
                data: "{\"jsonrpc\":\"2.0\"}".to_owned(),
            }]
        );
    }

    #[test]
    fn sse_notifications_are_retained_until_the_matching_response() {
        let plan = StreamableHttpMcpPlan {
            name: "jobs".to_owned(),
            endpoint: url::Url::parse("http://127.0.0.1:1337/mcp").unwrap(),
            requires_auth: false,
            tools: Vec::new(),
            definition_sha256: "a".repeat(64),
        };
        let mut client = StreamableHttpMcpClient::new(plan, reqwest::Client::new());
        let notification = SseEvent {
            data: "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"progress\":50}}".to_owned(),
        };
        assert_eq!(client.handle_sse_event(notification, 3).unwrap(), None);
        let response = SseEvent {
            data: "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"ok\":true}}".to_owned(),
        };
        assert_eq!(
            client.handle_sse_event(response, 3).unwrap(),
            Some(json!({"ok": true}))
        );
        assert_eq!(
            client.take_notifications(),
            vec![McpServerNotification {
                method: "notifications/progress".to_owned(),
                params: Some(json!({"progress": 50})),
            }]
        );
    }

    #[test]
    fn sse_server_requests_are_not_silently_dropped() {
        let plan = StreamableHttpMcpPlan {
            name: "jobs".to_owned(),
            endpoint: url::Url::parse("http://127.0.0.1:1337/mcp").unwrap(),
            requires_auth: false,
            tools: Vec::new(),
            definition_sha256: "a".repeat(64),
        };
        let mut client = StreamableHttpMcpClient::new(plan, reqwest::Client::new());
        assert!(matches!(
            client.handle_sse_event(
                SseEvent {
                    data: "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"sampling/createMessage\",\"params\":{}}".to_owned(),
                },
                3,
            ),
            Err(McpHttpError::ServerRequestUnsupported { method }) if method == "sampling/createMessage"
        ));
    }
}
