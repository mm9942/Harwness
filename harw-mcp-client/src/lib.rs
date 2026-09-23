//! Minimaler MCP-Client für entfernte Streamable-HTTP-Server.
//!
//! Die Cloudflare-Server sprechen MCP über `POST /mcp`. Dieses Crate hält die
//! Transportdetails bewusst klein: JSON-RPC-Requests, optionale Session-ID,
//! Bearer-Authentifizierung sowie JSON- und endliche SSE-Antworten.

use std::fmt;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const MCP_SESSION_HEADER: HeaderName = HeaderName::from_static("mcp-session-id");
const MCP_PROTOCOL_HEADER: HeaderName = HeaderName::from_static("mcp-protocol-version");

/// Maximale Größe einer MCP-Antwort in Bytes. Schützt vor Speichererschöpfung
/// durch übergroße oder fehlerhafte Server-Antworten, etwa Bild- oder
/// Ressourcen-Inhalte in `tools/call`-Ergebnissen.
const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;

pub type McpResult<T> = Result<T, McpError>;

#[derive(Debug)]
pub enum McpError {
    InvalidEndpoint(String),
    InvalidHeader(&'static str),
    Http(reqwest::Error),
    UnexpectedStatus {
        status: u16,
        body: String,
    },
    InvalidSessionId,
    InvalidJsonRpc(String),
    JsonRpcFailure {
        code: i64,
        message: String,
    },
    Sse(String),
    SseResponseMissing,
    NotInitialized,
    /// Die Antwort überschreitet [`MAX_RESPONSE_BYTES`].
    ResponseTooLarge {
        limit: usize,
    },
    /// Das aufgerufene Tool meldet `isError: true`. Dies ist ein
    /// fachlicher Fehler des Tools, kein Transport- oder Protokollfehler.
    ToolCallFailed(Vec<McpContent>),
}

impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint(reason) => write!(f, "invalid MCP endpoint: {reason}"),
            Self::InvalidHeader(name) => write!(f, "invalid MCP header: {name}"),
            Self::Http(error) => write!(f, "MCP HTTP request failed: {error}"),
            Self::UnexpectedStatus { status, body } => {
                write!(f, "MCP server returned HTTP {status}: {body}")
            }
            Self::InvalidSessionId => write!(f, "MCP server returned an invalid session id"),
            Self::InvalidJsonRpc(reason) => write!(f, "invalid MCP JSON-RPC response: {reason}"),
            Self::JsonRpcFailure { code, message } => {
                write!(f, "MCP JSON-RPC error {code}: {message}")
            }
            Self::Sse(reason) => write!(f, "invalid MCP SSE response: {reason}"),
            Self::SseResponseMissing => write!(f, "MCP SSE response contained no JSON-RPC result"),
            Self::NotInitialized => write!(f, "MCP client must initialize before tools/list"),
            Self::ResponseTooLarge { limit } => {
                write!(f, "MCP response exceeded the maximum size of {limit} bytes")
            }
            Self::ToolCallFailed(content) => {
                let text = content
                    .iter()
                    .filter_map(|item| match item {
                        McpContent::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                if text.is_empty() {
                    write!(f, "MCP tool call reported an error")
                } else {
                    write!(f, "MCP tool call reported an error: {text}")
                }
            }
        }
    }
}

impl std::error::Error for McpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Http(error) => Some(error),
            _ => None,
        }
    }
}

/// A single tool advertised by an MCP server.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub input_schema: Value,
}

/// Result page returned by `tools/list`.
#[derive(Debug, Clone)]
pub struct McpToolPage {
    pub tools: Vec<McpTool>,
    pub next_cursor: Option<String>,
}

/// Ein einzelnes Inhaltselement einer `tools/call`-Antwort.
///
/// MCP-Server liefern Text-, Bild- und Ressourcen-Inhalte. Unbekannte
/// `type`-Werte (z. B. zukünftige `audio`-Inhalte) werden verlustfrei als
/// `Unknown` abgebildet, damit sie den Parser nicht brechen.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpContent {
    /// Reiner Textinhalt.
    Text {
        /// Der Textinhalt selbst.
        text: String,
    },
    /// Binäres Bild, base64-kodiert.
    Image {
        /// Base64-kodierte Bilddaten.
        data: String,
        /// MIME-Typ des Bildes, z. B. `image/png`.
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    /// Eingebettete Ressource (z. B. Datei- oder API-Inhalt).
    Resource {
        /// Die eigentliche Ressource.
        resource: McpResourceContents,
    },
    /// Fallback für unbekannte, künftige Content-Typen.
    #[serde(other)]
    Unknown,
}

/// Der Inhalt einer eingebetteten MCP-Ressource innerhalb von
/// [`McpContent::Resource`].
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceContents {
    /// URI der Ressource.
    pub uri: String,
    /// Optionaler MIME-Typ der Ressource.
    #[serde(default)]
    pub mime_type: Option<String>,
    /// Textinhalt, falls die Ressource als Text vorliegt.
    #[serde(default)]
    pub text: Option<String>,
    /// Base64-kodierter Binärinhalt, falls die Ressource binär vorliegt.
    #[serde(default)]
    pub blob: Option<String>,
}

/// Session-bound Streamable-HTTP client.
pub struct McpClient {
    endpoint: Url,
    token: Option<Vec<u8>>,
    http: reqwest::Client,
    session_id: Option<String>,
    protocol_version: String,
    next_id: u64,
    initialized: bool,
}

impl McpClient {
    /// Creates a client. The token is held only in memory and is never part of
    /// `Debug`, error messages, or catalog metadata.
    pub fn new(endpoint: &str, token: Option<Vec<u8>>) -> McpResult<Self> {
        let endpoint =
            Url::parse(endpoint).map_err(|error| McpError::InvalidEndpoint(error.to_string()))?;
        match endpoint.scheme() {
            "https" => {}
            "http"
                if endpoint.host_str() == Some("localhost")
                    || endpoint.host_str() == Some("127.0.0.1") => {}
            scheme => {
                return Err(McpError::InvalidEndpoint(format!(
                    "scheme '{scheme}' is not allowed"
                )));
            }
        }
        Ok(Self {
            endpoint,
            token,
            http: reqwest::Client::new(),
            session_id: None,
            protocol_version: MCP_PROTOCOL_VERSION.to_owned(),
            next_id: 1,
            initialized: false,
        })
    }

    #[must_use]
    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    #[must_use]
    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    /// Performs MCP initialize and the required initialized notification.
    pub async fn initialize(&mut self, client_name: &str, client_version: &str) -> McpResult<()> {
        self.session_id = None;
        self.initialized = false;
        self.protocol_version = MCP_PROTOCOL_VERSION.to_owned();
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": client_name, "version": client_version}
                }),
                false,
            )
            .await?;
        self.protocol_version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                McpError::InvalidJsonRpc("initialize result has no protocolVersion".to_owned())
            })?
            .to_owned();
        self.initialized = true;
        self.notify("notifications/initialized", json!({})).await
    }

    /// Fetches one `tools/list` page.
    pub async fn list_tools(&mut self, cursor: Option<&str>) -> McpResult<McpToolPage> {
        if !self.initialized {
            return Err(McpError::NotInitialized);
        }
        let params = cursor.map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}));
        let result = self.request("tools/list", params, false).await?;
        let tools =
            serde_json::from_value(result.get("tools").cloned().unwrap_or_else(|| json!([])))
                .map_err(|error| {
                    McpError::InvalidJsonRpc(format!("tools/list result is invalid: {error}"))
                })?;
        Ok(McpToolPage {
            tools,
            next_cursor: result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }

    /// Fetches all pages from `tools/list`.
    pub async fn list_all_tools(&mut self) -> McpResult<Vec<McpTool>> {
        let mut all = Vec::new();
        let mut cursor = None;
        loop {
            let page = self.list_tools(cursor.as_deref()).await?;
            all.extend(page.tools);
            cursor = page.next_cursor;
            if cursor.is_none() {
                return Ok(all);
            }
        }
    }

    /// Ruft ein MCP-Tool per `tools/call` auf und liefert dessen Inhalt.
    ///
    /// `arguments` wird unverändert als `arguments`-Feld des JSON-RPC-Requests
    /// gesendet; leere Argumente können als `serde_json::json!({})` übergeben
    /// werden.
    ///
    /// # Errors
    /// - [`McpError::NotInitialized`], wenn zuvor kein `initialize` aufgerufen wurde.
    /// - [`McpError::ToolCallFailed`], wenn der Server `isError: true` meldet;
    ///   dies ist ein fachlicher Fehler des Tools, kein Transport-/Protokollfehler.
    /// - [`McpError::ResponseTooLarge`], wenn die Antwort [`MAX_RESPONSE_BYTES`] überschreitet.
    /// - Übrige Varianten bei Transport- oder Protokollfehlern, wie bei anderen Requests.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> McpResult<Vec<McpContent>> {
        if !self.initialized {
            return Err(McpError::NotInitialized);
        }
        let result = self
            .request(
                "tools/call",
                json!({"name": name, "arguments": arguments}),
                false,
            )
            .await?;
        parse_tool_call_result(result)
    }

    /// Best-effort MCP session teardown.
    pub async fn close(&mut self) -> McpResult<()> {
        let Some(session) = self.session_id.as_deref() else {
            return Ok(());
        };
        let response = self
            .http
            .delete(self.endpoint.clone())
            .headers(self.headers(true, Some(session))?)
            .send()
            .await
            .map_err(McpError::Http)?;
        let status = response.status().as_u16();
        if !(response.status().is_success() || status == 404 || status == 405) {
            return Err(McpError::UnexpectedStatus {
                status,
                body: safe_body(response.text().await.map_err(McpError::Http)?),
            });
        }
        self.session_id = None;
        self.initialized = false;
        Ok(())
    }

    async fn notify(&self, method: &str, params: Value) -> McpResult<()> {
        let response = self
            .http
            .post(self.endpoint.clone())
            .headers(self.headers(true, self.session_id.as_deref())?)
            .json(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .send()
            .await
            .map_err(McpError::Http)?;
        if !(response.status().is_success() || response.status().as_u16() == 202) {
            let status = response.status().as_u16();
            return Err(McpError::UnexpectedStatus {
                status,
                body: safe_body(response.text().await.map_err(McpError::Http)?),
            });
        }
        Ok(())
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
        initializing: bool,
    ) -> McpResult<Value> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| McpError::InvalidJsonRpc("request id exhausted".to_owned()))?;
        let response = self
            .http
            .post(self.endpoint.clone())
            .headers(self.headers(!initializing, self.session_id.as_deref())?)
            .json(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .send()
            .await
            .map_err(McpError::Http)?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(McpError::UnexpectedStatus {
                status,
                body: safe_body(response.text().await.map_err(McpError::Http)?),
            });
        }
        if initializing {
            if let Some(value) = response.headers().get(&MCP_SESSION_HEADER) {
                let value = value.to_str().map_err(|_| McpError::InvalidSessionId)?;
                if value.is_empty() || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
                    return Err(McpError::InvalidSessionId);
                }
                self.session_id = Some(value.to_owned());
            }
        }
        let is_sse = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        let bytes = read_body_capped(response, MAX_RESPONSE_BYTES).await?;
        if is_sse {
            return parse_sse_bytes(&bytes, id);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            McpError::InvalidJsonRpc(format!("response body is not valid JSON: {error}"))
        })?;
        parse_json_rpc_response(value, id)
    }

    fn headers(&self, include_protocol: bool, session: Option<&str>) -> McpResult<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if include_protocol {
            headers.insert(
                MCP_PROTOCOL_HEADER,
                HeaderValue::from_str(&self.protocol_version)
                    .map_err(|_| McpError::InvalidHeader("MCP-Protocol-Version"))?,
            );
        }
        if let Some(session) = session {
            headers.insert(
                MCP_SESSION_HEADER,
                HeaderValue::from_str(session)
                    .map_err(|_| McpError::InvalidHeader("Mcp-Session-Id"))?,
            );
        }
        if let Some(token) = &self.token {
            let mut value = b"Bearer ".to_vec();
            value.extend_from_slice(token);
            let mut auth = HeaderValue::from_bytes(&value)
                .map_err(|_| McpError::InvalidHeader("Authorization"))?;
            auth.set_sensitive(true);
            headers.insert(AUTHORIZATION, auth);
        }
        Ok(headers)
    }
}

fn safe_body(body: String) -> String {
    let body = body.trim();
    if body.is_empty() {
        "empty response body".to_owned()
    } else {
        body.chars().take(512).collect()
    }
}

fn parse_json_rpc_response(value: Value, expected_id: u64) -> McpResult<Value> {
    let object = value
        .as_object()
        .ok_or_else(|| McpError::InvalidJsonRpc("response is not an object".to_owned()))?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(McpError::InvalidJsonRpc(
            "jsonrpc must equal '2.0'".to_owned(),
        ));
    }
    if object.get("id").and_then(Value::as_u64) != Some(expected_id) {
        return Err(McpError::InvalidJsonRpc(
            "response id does not match request".to_owned(),
        ));
    }
    if let Some(error) = object.get("error").and_then(Value::as_object) {
        return Err(McpError::JsonRpcFailure {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(-32_000),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unspecified MCP error")
                .to_owned(),
        });
    }
    object
        .get("result")
        .cloned()
        .ok_or_else(|| McpError::InvalidJsonRpc("response has neither result nor error".to_owned()))
}

/// Prüft, ob eine gemessene Byte-Länge das gegebene Limit überschreitet.
fn ensure_within_limit(len: usize, limit: usize) -> McpResult<()> {
    if len > limit {
        Err(McpError::ResponseTooLarge { limit })
    } else {
        Ok(())
    }
}

/// Liest den Antwortkörper vollständig ein, bricht aber sofort ab, sobald
/// `limit` überschritten wird, statt die gesamte (potenziell riesige)
/// Antwort erst zu puffern.
async fn read_body_capped(mut response: reqwest::Response, limit: usize) -> McpResult<Vec<u8>> {
    if let Some(len) = response.content_length() {
        ensure_within_limit(usize::try_from(len).unwrap_or(usize::MAX), limit)?;
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(McpError::Http)? {
        bytes.extend_from_slice(&chunk);
        ensure_within_limit(bytes.len(), limit)?;
    }
    Ok(bytes)
}

/// Wertet das `result`-Objekt einer `tools/call`-Antwort aus: parst das
/// `content`-Array in [`McpContent`] und behandelt `isError: true` als
/// eigenständigen, vom Transport getrennten Fehlerfall.
fn parse_tool_call_result(result: Value) -> McpResult<Vec<McpContent>> {
    let content: Vec<McpContent> =
        serde_json::from_value(result.get("content").cloned().unwrap_or_else(|| json!([])))
            .map_err(|error| {
                McpError::InvalidJsonRpc(format!("tools/call result is invalid: {error}"))
            })?;
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if is_error {
        return Err(McpError::ToolCallFailed(content));
    }
    Ok(content)
}

fn parse_sse_bytes(bytes: &[u8], expected_id: u64) -> McpResult<Value> {
    let text = std::str::from_utf8(bytes).map_err(|error| McpError::Sse(error.to_string()))?;
    for event in text.split("\n\n") {
        let data = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        let value =
            serde_json::from_str(&data).map_err(|error| McpError::Sse(error.to_string()))?;
        return parse_json_rpc_response(value, expected_id);
    }
    Err(McpError::SseResponseMissing)
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn parses_tool_page_and_json_rpc_result() -> TestResult {
        let result = parse_json_rpc_response(
            json!({"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"docs","description":"Cloudflare docs","inputSchema":{"type":"object"}}],"nextCursor":"next"}}),
            1,
        )?;
        let tools: Vec<McpTool> = serde_json::from_value(result["tools"].clone())
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        assert_eq!(tools[0].name, "docs");
        assert_eq!(result["nextCursor"], "next");
        Ok(())
    }

    #[test]
    fn endpoint_requires_https_except_loopback() {
        assert!(McpClient::new("https://mcp.cloudflare.com/mcp", None).is_ok());
        assert!(McpClient::new("http://localhost:3000/mcp", None).is_ok());
        assert!(McpClient::new("http://example.test/mcp", None).is_err());
    }

    #[test]
    fn parse_tool_call_result_maps_content_variants() -> TestResult {
        let result = json!({
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "image", "data": "YWJj", "mimeType": "image/png"},
                {"type": "resource", "resource": {
                    "uri": "file:///a.txt",
                    "mimeType": "text/plain",
                    "text": "abc"
                }},
                {"type": "audio", "data": "future-proofing"}
            ],
            "isError": false
        });
        let content = parse_tool_call_result(result)?;
        assert_eq!(content.len(), 4);
        assert!(matches!(&content[0], McpContent::Text { text } if text == "hello"));
        assert!(
            matches!(&content[1], McpContent::Image { data, mime_type } if data == "YWJj" && mime_type == "image/png")
        );
        match &content[2] {
            McpContent::Resource { resource } => {
                assert_eq!(resource.uri, "file:///a.txt");
                assert_eq!(resource.mime_type.as_deref(), Some("text/plain"));
                assert_eq!(resource.text.as_deref(), Some("abc"));
                assert_eq!(resource.blob, None);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Resource content, got {other:?}"
                )));
            }
        }
        assert!(matches!(&content[3], McpContent::Unknown));
        Ok(())
    }

    #[test]
    fn parse_tool_call_result_is_error_yields_tool_call_failed() -> TestResult {
        let result = json!({
            "content": [{"type": "text", "text": "boom"}],
            "isError": true
        });
        match parse_tool_call_result(result) {
            Err(McpError::ToolCallFailed(content)) => {
                assert_eq!(content.len(), 1);
                assert!(matches!(&content[0], McpContent::Text { text } if text == "boom"));
                Ok(())
            }
            other => Err(TestError::Unexpected(format!(
                "expected ToolCallFailed, got {other:?}"
            ))),
        }
    }

    #[test]
    fn parse_tool_call_result_missing_content_defaults_to_empty() -> TestResult {
        let content = parse_tool_call_result(json!({}))?;
        assert!(content.is_empty());
        Ok(())
    }

    #[test]
    fn ensure_within_limit_rejects_oversized_bodies() {
        assert!(ensure_within_limit(10, 20).is_ok());
        assert!(matches!(
            ensure_within_limit(30, 20),
            Err(McpError::ResponseTooLarge { limit: 20 })
        ));
    }

    #[test]
    fn parse_sse_bytes_extracts_first_json_rpc_event() -> TestResult {
        let sse =
            b"event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let value = parse_sse_bytes(sse, 1)?;
        assert_eq!(value["ok"], true);
        Ok(())
    }

    #[test]
    fn parse_sse_bytes_without_data_event_is_missing() {
        assert!(matches!(
            parse_sse_bytes(b"event: ping\n\n", 1),
            Err(McpError::SseResponseMissing)
        ));
    }

    #[test]
    fn tool_call_failed_display_includes_text_content() {
        let error = McpError::ToolCallFailed(vec![McpContent::Text {
            text: "division by zero".to_owned(),
        }]);
        assert_eq!(
            error.to_string(),
            "MCP tool call reported an error: division by zero"
        );
    }
}
