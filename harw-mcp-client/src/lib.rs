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

pub type McpResult<T> = Result<T, McpError>;

#[derive(Debug)]
pub enum McpError {
    InvalidEndpoint(String),
    InvalidHeader(&'static str),
    Http(reqwest::Error),
    UnexpectedStatus { status: u16, body: String },
    InvalidSessionId,
    InvalidJsonRpc(String),
    JsonRpcFailure { code: i64, message: String },
    Sse(String),
    SseResponseMissing,
    NotInitialized,
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
        if response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"))
        {
            return parse_sse_response(response, id).await;
        }
        parse_json_rpc_response(response.json().await.map_err(McpError::Http)?, id)
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

async fn parse_sse_response(mut response: reqwest::Response, expected_id: u64) -> McpResult<Value> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(McpError::Http)? {
        bytes.extend_from_slice(&chunk);
    }
    let text = String::from_utf8(bytes).map_err(|error| McpError::Sse(error.to_string()))?;
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
mod tests {
    use super::*;

    #[test]
    fn parses_tool_page_and_json_rpc_result() {
        let result = parse_json_rpc_response(json!({"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"docs","description":"Cloudflare docs","inputSchema":{"type":"object"}}],"nextCursor":"next"}}), 1).unwrap();
        let tools: Vec<McpTool> = serde_json::from_value(result["tools"].clone()).unwrap();
        assert_eq!(tools[0].name, "docs");
        assert_eq!(result["nextCursor"], "next");
    }

    #[test]
    fn endpoint_requires_https_except_loopback() {
        assert!(McpClient::new("https://mcp.cloudflare.com/mcp", None).is_ok());
        assert!(McpClient::new("http://localhost:3000/mcp", None).is_ok());
        assert!(McpClient::new("http://example.test/mcp", None).is_err());
    }
}
