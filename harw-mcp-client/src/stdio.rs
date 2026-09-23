//! MCP-Client für lokale Server über den stdio-Transport.
//!
//! Gemäß MCP-Spezifikation startet der Client den Server als Kindprozess und
//! tauscht zeilenweise getrennte JSON-RPC-2.0-Nachrichten über dessen
//! `stdin`/`stdout` aus. Jede Nachricht steht in genau einer Zeile ohne
//! eingebettete Zeilenumbrüche. `stderr` des Servers wird in einem
//! Hintergrund-Task gelesen und als `tracing::debug`-Zeilen protokolliert.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::{
    MAX_RESPONSE_BYTES, MCP_PROTOCOL_VERSION, McpContent, McpError, McpResult, McpTool,
    McpToolPage, parse_json_rpc_response, parse_tool_call_result,
};

/// Zeitlimit für einen einzelnen Request inklusive Warten auf die Antwort.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Wartezeit nach dem Schließen von `stdin`, bevor der Prozess beendet wird.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// Obergrenze für die Anzahl der `tools/list`-Seiten, schützt vor Servern,
/// die endlos neue Cursor liefern.
const MAX_TOOL_PAGES: usize = 50;

/// Maximale Länge einer protokollierten `stderr`-Zeile in Zeichen.
const MAX_STDERR_LINE_CHARS: usize = 2048;

/// MCP-Client, der mit einem als Kindprozess gestarteten Server über
/// `stdin`/`stdout` spricht.
///
/// Der Kindprozess wird mit `kill_on_drop(true)` gestartet, sodass er beim
/// Verwerfen des Clients spätestens beendet wird. Für ein geordnetes Ende
/// sollte dennoch [`McpStdioClient::close`] aufgerufen werden.
pub struct McpStdioClient {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    protocol_version: String,
    next_id: u64,
    initialized: bool,
}

impl std::fmt::Debug for McpStdioClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpStdioClient")
            .field("pid", &self.child.id())
            .field("protocol_version", &self.protocol_version)
            .field("next_id", &self.next_id)
            .field("initialized", &self.initialized)
            .finish_non_exhaustive()
    }
}

impl McpStdioClient {
    /// Startet `command` mit `args` als MCP-Server-Kindprozess.
    ///
    /// Die Umgebung des aktuellen Prozesses wird geerbt und um die Paare aus
    /// `env` ergänzt bzw. überschrieben. `stdin`, `stdout` und `stderr` werden
    /// als Pipes angelegt; `stderr` wird im Hintergrund in `tracing::debug`
    /// ausgegeben. Muss innerhalb einer Tokio-Laufzeit aufgerufen werden.
    ///
    /// # Errors
    /// [`McpError::Transport`], wenn der Prozess nicht gestartet werden kann
    /// (z. B. unbekanntes Kommando) oder eine Pipe fehlt.
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> McpResult<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .envs(
                env.iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|error| {
            McpError::Transport(format!("failed to spawn MCP server '{command}': {error}"))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("MCP server stdin is not piped".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("MCP server stdout is not piped".to_owned()))?;
        if let Some(stderr) = child.stderr.take() {
            let label = command.to_owned();
            tokio::spawn(drain_stderr(stderr, label));
        }
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            protocol_version: MCP_PROTOCOL_VERSION.to_owned(),
            next_id: 1,
            initialized: false,
        })
    }

    /// Vom Server im `initialize`-Ergebnis ausgehandelte Protokollversion.
    #[must_use]
    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    /// Führt den MCP-Handshake aus: sendet `initialize` und anschließend die
    /// Benachrichtigung `notifications/initialized`.
    ///
    /// # Errors
    /// Transport-, Zeitüberschreitungs- und Protokollfehler; außerdem
    /// [`McpError::InvalidJsonRpc`], wenn das Ergebnis keine
    /// `protocolVersion` enthält.
    pub async fn initialize(&mut self, client_name: &str, client_version: &str) -> McpResult<()> {
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
            )
            .await?;
        self.protocol_version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                McpError::InvalidJsonRpc("initialize result has no protocolVersion".to_owned())
            })?
            .to_owned();
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }))
        .await?;
        self.initialized = true;
        Ok(())
    }

    /// Ruft alle Seiten von `tools/list` ab und folgt dabei `nextCursor`
    /// (höchstens 50 Seiten).
    ///
    /// # Errors
    /// [`McpError::NotInitialized`] vor [`McpStdioClient::initialize`],
    /// [`McpError::InvalidJsonRpc`] bei ungültigem Ergebnis oder mehr als 50
    /// Seiten, sonst Transport- und Protokollfehler.
    pub async fn list_all_tools(&mut self) -> McpResult<Vec<McpTool>> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
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
    /// # Errors
    /// - [`McpError::NotInitialized`] vor [`McpStdioClient::initialize`].
    /// - [`McpError::ToolCallFailed`], wenn der Server `isError: true` meldet.
    /// - [`McpError::JsonRpcFailure`] bei JSON-RPC-Fehlerobjekten.
    /// - [`McpError::Transport`] bei Pipe-Fehlern oder Zeitüberschreitung.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> McpResult<Vec<McpContent>> {
        if !self.initialized {
            return Err(McpError::NotInitialized);
        }
        let result = self
            .request("tools/call", json!({"name": name, "arguments": arguments}))
            .await?;
        parse_tool_call_result(result)
    }

    /// Beendet die Verbindung: schließt `stdin`, wartet kurz auf das
    /// Prozessende und beendet den Prozess andernfalls hart.
    ///
    /// # Errors
    /// [`McpError::Transport`], wenn der Prozess weder regulär endet noch
    /// beendet werden kann.
    pub async fn close(&mut self) -> McpResult<()> {
        self.initialized = false;
        if let Some(mut stdin) = self.stdin.take() {
            // Fehler beim Flush sind hier unerheblich: der Prozess wird ohnehin beendet.
            let _ = stdin.shutdown().await;
            drop(stdin);
        }
        match tokio::time::timeout(CLOSE_GRACE, self.child.wait()).await {
            Ok(Ok(_status)) => Ok(()),
            Ok(Err(error)) => Err(McpError::Transport(format!(
                "failed to wait for MCP server: {error}"
            ))),
            Err(_elapsed) => self.child.kill().await.map_err(|error| {
                McpError::Transport(format!("failed to kill MCP server: {error}"))
            }),
        }
    }

    /// Ruft eine einzelne `tools/list`-Seite ab.
    async fn list_tools(&mut self, cursor: Option<&str>) -> McpResult<McpToolPage> {
        if !self.initialized {
            return Err(McpError::NotInitialized);
        }
        let params = cursor.map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}));
        let result = self.request("tools/list", params).await?;
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

    /// Sendet einen Request und wartet (mit Zeitlimit) auf die passende Antwort.
    async fn request(&mut self, method: &str, params: Value) -> McpResult<Value> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| McpError::InvalidJsonRpc("request id exhausted".to_owned()))?;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        match tokio::time::timeout(REQUEST_TIMEOUT, self.exchange(&message, id)).await {
            Ok(result) => result,
            Err(_elapsed) => Err(McpError::Transport(format!(
                "MCP request '{method}' timed out after {} s",
                REQUEST_TIMEOUT.as_secs()
            ))),
        }
    }

    /// Schreibt `message` und liest so lange Nachrichten, bis die Antwort mit
    /// `id` eintrifft. Server-Benachrichtigungen werden übersprungen,
    /// Server-Requests beantwortet (`ping` mit leerem Ergebnis, alles andere
    /// mit „method not found“).
    async fn exchange(&mut self, message: &Value, id: u64) -> McpResult<Value> {
        self.send(message).await?;
        loop {
            let line = read_line_capped(&mut self.stdout, MAX_RESPONSE_BYTES).await?;
            let trimmed = line.trim_ascii();
            if trimmed.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_slice(trimmed).map_err(|error| {
                McpError::InvalidJsonRpc(format!("server message is not valid JSON: {error}"))
            })?;
            if let Some(method) = value.get("method").and_then(Value::as_str) {
                match value.get("id") {
                    Some(server_id) if !server_id.is_null() => {
                        let reply = if method == "ping" {
                            json!({"jsonrpc": "2.0", "id": server_id, "result": {}})
                        } else {
                            json!({
                                "jsonrpc": "2.0",
                                "id": server_id,
                                "error": {"code": -32_601, "message": "method not found"}
                            })
                        };
                        self.send(&reply).await?;
                    }
                    _ => tracing::debug!(method, "ignoring MCP server notification"),
                }
                continue;
            }
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                tracing::debug!("ignoring MCP response with unexpected id");
                continue;
            }
            return parse_json_rpc_response(value, id);
        }
    }

    /// Schreibt eine JSON-Nachricht als einzelne Zeile auf `stdin`.
    async fn send(&mut self, message: &Value) -> McpResult<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| McpError::Transport("MCP server stdin is closed".to_owned()))?;
        let mut line = serde_json::to_vec(message).map_err(|error| {
            McpError::InvalidJsonRpc(format!("failed to encode request: {error}"))
        })?;
        line.push(b'\n');
        stdin.write_all(&line).await.map_err(|error| {
            McpError::Transport(format!("failed to write to MCP server: {error}"))
        })?;
        stdin.flush().await.map_err(|error| {
            McpError::Transport(format!("failed to flush MCP server stdin: {error}"))
        })
    }
}

/// Liest eine Zeile (ohne abschließendes `\n`) und bricht mit
/// [`McpError::ResponseTooLarge`] ab, sobald sie `limit` Bytes überschreitet.
async fn read_line_capped<R>(reader: &mut R, limit: usize) -> McpResult<Vec<u8>>
where
    R: AsyncBufReadExt + Unpin,
{
    let mut line = Vec::new();
    loop {
        let buffer = reader
            .fill_buf()
            .await
            .map_err(|error| McpError::Transport(format!("failed to read MCP server: {error}")))?;
        if buffer.is_empty() {
            return Err(McpError::Transport("MCP server closed stdout".to_owned()));
        }
        let (chunk, consumed, done) = match buffer.iter().position(|&byte| byte == b'\n') {
            Some(pos) => (&buffer[..pos], pos.saturating_add(1), true),
            None => (buffer, buffer.len(), false),
        };
        if line.len().saturating_add(chunk.len()) > limit {
            return Err(McpError::ResponseTooLarge { limit });
        }
        line.extend_from_slice(chunk);
        reader.consume(consumed);
        if done {
            return Ok(line);
        }
    }
}

/// Liest `stderr` des Servers bis EOF und protokolliert jede Zeile.
async fn drain_stderr(stderr: tokio::process::ChildStderr, command: String) {
    let mut reader = BufReader::new(stderr);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let text = String::from_utf8_lossy(&buffer);
                let text: String = text
                    .trim_end()
                    .chars()
                    .take(MAX_STDERR_LINE_CHARS)
                    .collect();
                tracing::debug!(command = %command, "MCP server stderr: {text}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Minimaler MCP-Server in Python für die Tests.
    const FAKE_SERVER: &str = r#"
import sys, json, os
def send(o):
    sys.stdout.write(json.dumps(o) + "\n")
    sys.stdout.flush()
sys.stderr.write("fake server started\n")
sys.stderr.flush()
initialized = False
while True:
    line = sys.stdin.readline()
    if not line:
        break
    line = line.strip()
    if not line:
        continue
    m = json.loads(line)
    mid = m.get("id")
    meth = m.get("method")
    if meth is None or mid is None:
        if meth == "notifications/initialized":
            initialized = True
        continue
    if meth == "initialize":
        send({"jsonrpc": "2.0", "id": mid, "result": {"protocolVersion": "2025-06-18",
              "capabilities": {"tools": {}}, "serverInfo": {"name": "fake", "version": "0"}}})
    elif meth == "tools/list":
        if not initialized:
            send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32002, "message": "not initialized"}})
            continue
        cursor = (m.get("params") or {}).get("cursor")
        if cursor is None:
            send({"jsonrpc": "2.0", "id": mid, "result": {"tools": [{"name": "echo",
                  "description": "Echo", "inputSchema": {"type": "object"}}], "nextCursor": "p2"}})
        else:
            send({"jsonrpc": "2.0", "id": mid, "result": {"tools": [{"name": "fail",
                  "inputSchema": {"type": "object"}}, {"name": "env"}]}})
    elif meth == "tools/call":
        params = m.get("params") or {}
        name = params.get("name")
        send({"jsonrpc": "2.0", "method": "notifications/message", "params": {"level": "info"}})
        send({"jsonrpc": "2.0", "id": "srv-1", "method": "ping"})
        pong = json.loads(sys.stdin.readline())
        if pong.get("id") != "srv-1" or pong.get("result") != {}:
            send({"jsonrpc": "2.0", "id": mid, "error": {"code": -1, "message": "bad pong"}})
            continue
        if name == "echo":
            text = (params.get("arguments") or {}).get("text", "")
            send({"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": text}]}})
        elif name == "fail":
            send({"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": "boom"}],
                  "isError": True}})
        elif name == "env":
            send({"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text",
                  "text": os.environ.get("HARW_STDIO_TEST", "")}]}})
        else:
            send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32602, "message": "unknown tool"}})
    else:
        send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "method not found"}})
"#;

    fn runtime() -> TestResult<tokio::runtime::Runtime> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(ctx("tokio runtime"))
    }

    fn python_available() -> bool {
        std::process::Command::new("python3")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    async fn spawn_fake() -> McpResult<McpStdioClient> {
        McpStdioClient::spawn(
            "python3",
            &["-c".to_owned(), FAKE_SERVER.to_owned()],
            &[("HARW_STDIO_TEST".to_owned(), "from-env".to_owned())],
        )
        .await
    }

    fn first_text(content: &[McpContent]) -> TestResult<&str> {
        match content.first() {
            Some(McpContent::Text { text }) => Ok(text),
            other => Err(TestError::Unexpected(format!(
                "expected text, got {other:?}"
            ))),
        }
    }

    #[test]
    fn happy_path_initialize_list_and_call() -> TestResult {
        if !python_available() {
            return Ok(());
        }
        runtime()?.block_on(async {
            let mut client = spawn_fake().await?;
            client.initialize("harw-test", "0.0.0").await?;
            assert_eq!(client.protocol_version(), "2025-06-18");
            let content = client.call_tool("echo", json!({"text": "hallo"})).await?;
            assert_eq!(first_text(&content)?, "hallo");
            let content = client.call_tool("env", json!({})).await?;
            assert_eq!(first_text(&content)?, "from-env");
            client.close().await?;
            Ok(())
        })
    }

    #[test]
    fn pagination_follows_next_cursor() -> TestResult {
        if !python_available() {
            return Ok(());
        }
        runtime()?.block_on(async {
            let mut client = spawn_fake().await?;
            client.initialize("harw-test", "0.0.0").await?;
            let tools = client.list_all_tools().await?;
            let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
            assert_eq!(names, ["echo", "fail", "env"]);
            assert_eq!(tools[0].description.as_deref(), Some("Echo"));
            client.close().await?;
            Ok(())
        })
    }

    #[test]
    fn is_error_maps_to_tool_call_failed() -> TestResult {
        if !python_available() {
            return Ok(());
        }
        runtime()?.block_on(async {
            let mut client = spawn_fake().await?;
            client.initialize("harw-test", "0.0.0").await?;
            match client.call_tool("fail", json!({})).await {
                Err(McpError::ToolCallFailed(content)) => {
                    assert_eq!(first_text(&content)?, "boom");
                }
                other => {
                    return Err(TestError::Unexpected(format!(
                        "expected ToolCallFailed, got {other:?}"
                    )));
                }
            }
            match client.call_tool("missing", json!({})).await {
                Err(McpError::JsonRpcFailure { code, .. }) => assert_eq!(code, -32_602),
                other => {
                    return Err(TestError::Unexpected(format!(
                        "expected JsonRpcFailure, got {other:?}"
                    )));
                }
            }
            client.close().await?;
            Ok(())
        })
    }

    #[test]
    fn methods_before_initialize_fail() -> TestResult {
        if !python_available() {
            return Ok(());
        }
        runtime()?.block_on(async {
            let mut client = spawn_fake().await?;
            assert!(matches!(
                client.list_all_tools().await,
                Err(McpError::NotInitialized)
            ));
            assert!(matches!(
                client.call_tool("echo", json!({})).await,
                Err(McpError::NotInitialized)
            ));
            client.close().await?;
            Ok(())
        })
    }

    #[test]
    fn spawn_of_missing_command_fails() -> TestResult {
        runtime()?.block_on(async {
            match McpStdioClient::spawn("/nonexistent/harw-mcp-server-xyz", &[], &[]).await {
                Err(McpError::Transport(_)) => Ok(()),
                other => Err(TestError::Unexpected(format!(
                    "expected Transport error, got {other:?}"
                ))),
            }
        })
    }

    #[test]
    fn read_line_capped_enforces_limit_and_eof() -> TestResult {
        runtime()?.block_on(async {
            let mut ok = BufReader::new(&b"abc\ndef"[..]);
            assert_eq!(read_line_capped(&mut ok, 3).await?, b"abc");
            assert!(matches!(
                read_line_capped(&mut ok, 3).await,
                Err(McpError::Transport(_))
            ));
            let mut big = BufReader::new(&b"abcdef\n"[..]);
            assert!(matches!(
                read_line_capped(&mut big, 3).await,
                Err(McpError::ResponseTooLarge { limit: 3 })
            ));
            Ok(())
        })
    }
}
