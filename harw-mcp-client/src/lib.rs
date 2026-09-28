//! Minimaler MCP-Client für entfernte Streamable-HTTP-Server.
//!
//! Die Cloudflare-Server sprechen MCP über `POST /mcp`. Dieses Crate hält die
//! Transportdetails bewusst klein: JSON-RPC-Requests, die beim `initialize`
//! vergebene Session-ID, Bearer-Authentifizierung, Zeitlimits sowie JSON- und
//! endliche SSE-Antworten.

pub mod stdio;
pub mod tool_bridge;

use std::borrow::Cow;
use std::fmt;
use std::time::Duration;

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

/// Zeitlimit für einen einzelnen HTTP-Request vom Verbindungsaufbau bis zum
/// Ende des Antwortkörpers (wie beim stdio-Transport).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Zeitlimit nur für den Verbindungsaufbau.
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Obergrenze für die Anzahl der `tools/list`-Seiten, schützt vor Servern,
/// die endlos neue Cursor liefern.
const MAX_TOOL_PAGES: usize = 50;

pub type McpResult<T> = Result<T, McpError>;

#[derive(Debug)]
pub enum McpError {
    InvalidEndpoint(String),
    InvalidHeader(&'static str),
    /// Der HTTP-Request ist gescheitert (Verbindung, TLS, Antwortkörper).
    /// Enthält die Ursachenkette als Text, ohne URL.
    Http(String),
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
    /// Transportfehler abseits von HTTP (z. B. stdio-Prozess beendet,
    /// Lese-/Schreibfehler auf der Pipe) sowie Zeitüberschreitungen beider
    /// Transporte.
    Transport(String),
}

impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint(reason) => write!(f, "invalid MCP endpoint: {reason}"),
            Self::Transport(reason) => write!(f, "MCP transport failed: {reason}"),
            Self::InvalidHeader(name) => write!(f, "invalid MCP header: {name}"),
            Self::Http(reason) => write!(f, "MCP HTTP request failed: {reason}"),
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

impl std::error::Error for McpError {}

/// Übersetzt einen `reqwest`-Fehler in [`McpError`], damit kein Fremdtyp in
/// der öffentlichen API erscheint. Die URL entfällt, die Ursachenkette bleibt
/// als Text erhalten; Zeitüberschreitungen werden wie beim stdio-Transport zu
/// [`McpError::Transport`].
fn http_error(error: reqwest::Error) -> McpError {
    let timed_out = error.is_timeout();
    let error = error.without_url();
    let mut reason = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        reason.push_str(": ");
        reason.push_str(&cause.to_string());
        source = cause.source();
    }
    if timed_out {
        McpError::Transport(format!("HTTP request timed out: {reason}"))
    } else {
        McpError::Http(reason)
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
///
/// Jeder Request ist auf 60 s begrenzt, `tools/list` auf 50 Seiten.
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
        Self::with_request_timeout(endpoint, token, REQUEST_TIMEOUT)
    }

    /// Wie [`McpClient::new`], aber mit frei gewähltem Zeitlimit pro Request.
    fn with_request_timeout(
        endpoint: &str,
        token: Option<Vec<u8>>,
        request_timeout: Duration,
    ) -> McpResult<Self> {
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
        let http = reqwest::Client::builder()
            .timeout(request_timeout)
            .connect_timeout(request_timeout.min(HTTP_CONNECT_TIMEOUT))
            .build()
            .map_err(http_error)?;
        Ok(Self {
            endpoint,
            token,
            http,
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
    ///
    /// Vergibt der Server dabei eine `Mcp-Session-Id`, wird sie gespeichert
    /// und an alle folgenden Requests sowie an [`McpClient::close`] gehängt.
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
                true,
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

    /// Fetches all pages from `tools/list` (höchstens 50 Seiten).
    ///
    /// # Errors
    /// [`McpError::InvalidJsonRpc`] bei mehr als 50 Seiten, sonst wie
    /// [`McpClient::list_tools`].
    pub async fn list_all_tools(&mut self) -> McpResult<Vec<McpTool>> {
        let mut all = Vec::new();
        let mut cursor = None;
        for _ in 0..MAX_TOOL_PAGES {
            let page = self.list_tools(cursor.as_deref()).await?;
            all.extend(page.tools);
            cursor = page.next_cursor;
            if cursor.is_none() {
                return Ok(all);
            }
        }
        Err(McpError::InvalidJsonRpc(format!(
            "tools/list exceeded {MAX_TOOL_PAGES} pages"
        )))
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
    /// - [`McpError::Transport`], wenn der Request länger als 60 s dauert.
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
            .map_err(http_error)?;
        let status = response.status().as_u16();
        if !(response.status().is_success() || status == 404 || status == 405) {
            return Err(McpError::UnexpectedStatus {
                status,
                body: safe_body(response.text().await.map_err(http_error)?),
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
            .map_err(http_error)?;
        if !(response.status().is_success() || response.status().as_u16() == 202) {
            let status = response.status().as_u16();
            return Err(McpError::UnexpectedStatus {
                status,
                body: safe_body(response.text().await.map_err(http_error)?),
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
            .map_err(http_error)?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(McpError::UnexpectedStatus {
                status,
                body: safe_body(response.text().await.map_err(http_error)?),
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
    while let Some(chunk) = response.chunk().await.map_err(http_error)? {
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

/// Sucht in einer endlichen SSE-Antwort die Antwort mit `expected_id`.
///
/// Server dürfen vorher Benachrichtigungen oder eigene Requests senden (z. B.
/// Fortschritt, Logging); diese und Antworten mit fremder `id` werden wie beim
/// stdio-Transport übersprungen. Zeilenenden `\r\n` und `\r` gelten wie `\n`.
fn parse_sse_bytes(bytes: &[u8], expected_id: u64) -> McpResult<Value> {
    let text = std::str::from_utf8(bytes).map_err(|error| McpError::Sse(error.to_string()))?;
    let text: Cow<'_, str> = if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(text)
    };
    for event in text.split("\n\n") {
        let data = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        let value: Value =
            serde_json::from_str(&data).map_err(|error| McpError::Sse(error.to_string()))?;
        if let Some(method) = value.get("method").and_then(Value::as_str) {
            tracing::debug!(method, "ignoring MCP server message on SSE stream");
            continue;
        }
        if value.get("id").and_then(Value::as_u64) != Some(expected_id) {
            tracing::debug!("ignoring MCP SSE response with unexpected id");
            continue;
        }
        return parse_json_rpc_response(value, expected_id);
    }
    Err(McpError::SseResponseMissing)
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

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
    fn parse_sse_bytes_skips_server_messages_and_foreign_ids() -> TestResult {
        let sse = b"\
            data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\n\n\
            data: {\"jsonrpc\":\"2.0\",\"id\":\"srv-1\",\"method\":\"ping\"}\n\n\
            data: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ok\":false}}\n\n\
            data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let value = parse_sse_bytes(sse, 1)?;
        assert_eq!(value["ok"], true);
        let only_notification =
            b"data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}\n\n";
        assert!(matches!(
            parse_sse_bytes(only_notification, 1),
            Err(McpError::SseResponseMissing)
        ));
        Ok(())
    }

    #[test]
    fn parse_sse_bytes_accepts_crlf_and_cr_framing() -> TestResult {
        let crlf = b"event: message\r\n\
            data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\r\n\r\n\
            data: {\"jsonrpc\":\"2.0\",\"id\":1,\r\n\
            data: \"result\":{\"ok\":true}}\r\n\r\n";
        assert_eq!(parse_sse_bytes(crlf, 1)?["ok"], true);
        let cr = b"data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\r\r\
            data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\r\r";
        assert_eq!(parse_sse_bytes(cr, 1)?["ok"], true);
        Ok(())
    }

    #[test]
    fn http_error_is_crate_owned_text() {
        let error = McpError::Http("error sending request: connection refused".to_owned());
        assert_eq!(
            error.to_string(),
            "MCP HTTP request failed: error sending request: connection refused"
        );
        assert!(std::error::Error::source(&error).is_none());
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

    /// Mitschnitt aller Requests, die der Stub-Server gesehen hat.
    type Seen = Arc<Mutex<Vec<String>>>;

    fn runtime() -> TestResult<tokio::runtime::Runtime> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(ctx("tokio runtime"))
    }

    /// Wert eines HTTP-Headers; der Name wird ohne Groß-/Kleinschreibung verglichen.
    fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
        head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            if key.trim().eq_ignore_ascii_case(name) {
                Some(value.trim())
            } else {
                None
            }
        })
    }

    /// Liest einen HTTP/1.1-Request: Kopf bis `\r\n\r\n`, dann
    /// `Content-Length` Bytes Körper.
    fn read_request(stream: &mut TcpStream) -> std::io::Result<String> {
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut data = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = stream.read(&mut buffer)?;
            data.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&data);
            let complete = text.split_once("\r\n\r\n").is_some_and(|(head, body)| {
                let length = header(head, "content-length")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                body.len() >= length
            });
            if complete || read == 0 {
                return Ok(text.into_owned());
            }
        }
    }

    /// Startet einen HTTP/1.1-Stub auf Loopback, der jeden Request mit
    /// `reply` beantwortet und mitschreibt. Jede Verbindung trägt genau einen
    /// Request (`Connection: close`).
    fn spawn_stub(reply: fn(&str) -> String) -> TestResult<(String, Seen)> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(ctx("bind stub"))?;
        let port = listener.local_addr().map_err(ctx("stub address"))?.port();
        let seen = Seen::default();
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let request = match read_request(&mut stream) {
                    Ok(request) if !request.is_empty() => request,
                    _ => continue,
                };
                let response = reply(&request);
                if let Ok(mut log) = log.lock() {
                    log.push(request);
                }
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Ok((format!("http://127.0.0.1:{port}/mcp"), seen))
    }

    fn http_response(status: &str, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// Zustandsbehafteter Streamable-HTTP-Server: `initialize` vergibt die
    /// Session `s-1`, jeder weitere Request ohne sie scheitert mit 400, und
    /// `tools/list` liefert immer einen weiteren Cursor.
    fn stateful_reply(request: &str) -> String {
        let (head, body) = request.split_once("\r\n\r\n").unwrap_or((request, ""));
        let message: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let id = message.get("id").cloned();
        if message.get("method").and_then(Value::as_str) == Some("initialize") {
            let result = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"protocolVersion": MCP_PROTOCOL_VERSION}
            });
            return http_response(
                "200 OK",
                "Mcp-Session-Id: s-1\r\nContent-Type: application/json\r\n",
                &result.to_string(),
            );
        }
        if header(head, "mcp-session-id") != Some("s-1") {
            return http_response("400 Bad Request", "", "missing session");
        }
        if head.starts_with("DELETE ") {
            return http_response("200 OK", "", "");
        }
        match id {
            None => http_response("202 Accepted", "", ""),
            Some(id) => {
                let result = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {"tools": [{"name": "t"}], "nextCursor": "more"}
                });
                http_response(
                    "200 OK",
                    "Content-Type: application/json\r\n",
                    &result.to_string(),
                )
            }
        }
    }

    #[test]
    fn initialize_keeps_session_id_until_close_deletes_it() -> TestResult {
        let (endpoint, seen) = spawn_stub(stateful_reply)?;
        let mut client = McpClient::new(&endpoint, None)?;
        runtime()?.block_on(async {
            client.initialize("harw-test", "0.0.0").await?;
            assert_eq!(client.session_id.as_deref(), Some("s-1"));
            client.close().await?;
            assert_eq!(client.session_id, None);
            let requests = seen.lock().map_err(ctx("stub log"))?.clone();
            let [initialize, initialized, delete] = requests.as_slice() else {
                return Err(TestError::Unexpected(format!(
                    "expected 3 requests, got {requests:?}"
                )));
            };
            // Vor der Aushandlung weder Session noch Protokollversion senden.
            assert_eq!(header(initialize, "mcp-session-id"), None);
            assert_eq!(header(initialize, "mcp-protocol-version"), None);
            assert_eq!(header(initialized, "mcp-session-id"), Some("s-1"));
            assert_eq!(
                header(initialized, "mcp-protocol-version"),
                Some(MCP_PROTOCOL_VERSION)
            );
            assert!(delete.starts_with("DELETE "));
            assert_eq!(header(delete, "mcp-session-id"), Some("s-1"));
            Ok(())
        })
    }

    #[test]
    fn list_all_tools_stops_after_max_pages() -> TestResult {
        let (endpoint, seen) = spawn_stub(stateful_reply)?;
        let mut client = McpClient::new(&endpoint, None)?;
        runtime()?.block_on(async {
            client.initialize("harw-test", "0.0.0").await?;
            match client.list_all_tools().await {
                Err(McpError::InvalidJsonRpc(reason)) => {
                    assert_eq!(reason, format!("tools/list exceeded {MAX_TOOL_PAGES} pages"));
                }
                other => {
                    return Err(TestError::Unexpected(format!(
                        "expected page cap, got {other:?}"
                    )));
                }
            }
            let pages = seen
                .lock()
                .map_err(ctx("stub log"))?
                .iter()
                .filter(|request| request.contains("\"tools/list\""))
                .count();
            assert_eq!(pages, MAX_TOOL_PAGES);
            Ok(())
        })
    }

    #[test]
    fn call_tool_times_out_when_server_never_answers() -> TestResult {
        // Der Kernel nimmt die Verbindung im Backlog an; geantwortet wird nie.
        let listener = TcpListener::bind("127.0.0.1:0").map_err(ctx("bind stub"))?;
        let port = listener.local_addr().map_err(ctx("stub address"))?.port();
        let mut client = McpClient::with_request_timeout(
            &format!("http://127.0.0.1:{port}/mcp"),
            None,
            Duration::from_millis(200),
        )?;
        client.initialized = true;
        runtime()?.block_on(async {
            match client.call_tool("slow", json!({})).await {
                Err(McpError::Transport(reason)) => {
                    assert!(reason.contains("timed out"), "{reason}");
                }
                other => {
                    return Err(TestError::Unexpected(format!(
                        "expected timeout, got {other:?}"
                    )));
                }
            }
            Ok(())
        })
    }
}
