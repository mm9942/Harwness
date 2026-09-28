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

/// Obergrenze für die Rohbytes einer `stderr`-Zeile, bevor der Rest bis zum
/// nächsten Zeilenumbruch verworfen wird (siehe [`read_stderr_line_bounded`]).
/// Ein Vielfaches von [`MAX_STDERR_LINE_CHARS`], da einzelne UTF-8-Zeichen
/// mehrere Bytes belegen können und die Kürzung auf Zeichen erst danach
/// erfolgt.
const MAX_STDERR_LINE_BYTES: usize = MAX_STDERR_LINE_CHARS * 4;

/// Aus der Prozessumgebung in den Kindprozess übernommene Variablen. Alles
/// andere wird beim Start durch `env_clear` entfernt, damit Geheimnisse aus
/// der Umgebung des Harness (z. B. Provider-API-Schlüssel) nicht automatisch
/// an einen MCP-Server weitergegeben werden. Der Vergleich ist
/// case-insensitiv, da Windows Variablennamen unabhängig von der
/// Groß-/Kleinschreibung behandelt.
const SAFE_ENV_ALLOWLIST: &[&str] = &[
    "PATH", "HOME", "USER", "LOGNAME", "SHELL", "TERM", "LANG", "LC_ALL", "TMPDIR", "SYSTEMROOT",
];

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
    /// Fortschritt eines eventuell durch Zeitüberschreitung abgebrochenen
    /// stdout-Reads; siehe [`PartialLine`].
    partial_line: PartialLine,
}

/// Fortschritt eines noch nicht durch `\n` abgeschlossenen stdout-Reads.
///
/// Lebt auf [`McpStdioClient`] statt in einer lokalen Variable von
/// [`read_line_capped`]: [`McpStdioClient::request`] bricht einen Read per
/// [`tokio::time::timeout`] ab, wenn er zu lange dauert. Ohne diesen
/// gemeinsamen Zustand gingen dabei bereits gelesene Bytes der angefangenen
/// Zeile verloren, und der nächste Aufruf würde mitten in der alten Zeile
/// weiterlesen statt an ihrem Anfang — die Rahmung der Nachrichten würde
/// verrutschen. Mit dem gemeinsamen Zustand setzt der nächste Aufruf exakt
/// dort fort, wo der abgebrochene stehen geblieben ist.
#[derive(Default)]
struct PartialLine {
    /// Bereits gelesene Bytes der angefangenen Zeile (ohne `\n`).
    bytes: Vec<u8>,
    /// Ob die angefangene Zeile bereits [`MAX_RESPONSE_BYTES`] überschritten
    /// hat; in diesem Fall werden weitere Bytes bis zum nächsten
    /// Zeilenumbruch nur noch verworfen statt gepuffert.
    over_limit: bool,
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
    /// Die Umgebung des aktuellen Prozesses wird **nicht** unverändert
    /// geerbt: sie wird zunächst vollständig gelöscht (`env_clear`) und
    /// danach nur um eine kleine Allowlist harmloser Variablen
    /// ([`SAFE_ENV_ALLOWLIST`]) sowie die Paare aus `env` ergänzt bzw.
    /// überschrieben. So gelangen Geheimnisse aus der Umgebung des Harness
    /// (z. B. Provider-API-Schlüssel) nicht automatisch an einen gestarteten
    /// Fremdprozess. `stdin`, `stdout` und `stderr` werden als Pipes
    /// angelegt; `stderr` wird im Hintergrund in `tracing::debug`
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
        let ambient: Vec<(String, String)> = std::env::vars().collect();
        cmd.args(args)
            .env_clear()
            .envs(filter_child_env(&ambient, env))
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
            partial_line: PartialLine::default(),
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
            let line = read_line_capped(
                &mut self.stdout,
                &mut self.partial_line,
                MAX_RESPONSE_BYTES,
            )
            .await?;
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
///
/// Der Fortschritt liegt in `partial` statt in einer rein lokalen Variable:
/// Wird das zurückgegebene Future verworfen (z. B. durch das Zeitlimit in
/// [`McpStdioClient::request`]), bleiben bereits gelesene Bytes der
/// angefangenen Zeile in `partial` erhalten, und der nächste Aufruf setzt
/// exakt dort fort, statt den Byte-Strom zu verschieben. Eine so zu Ende
/// gelesene, aber nicht mehr erwartete Antwort verwirft
/// [`McpStdioClient::exchange`] anschließend anhand ihrer `id`.
///
/// Auch bei [`McpError::ResponseTooLarge`] wird die überlange Zeile
/// vollständig bis zum nächsten Zeilenumbruch aus der Pipe entnommen (nur
/// ohne sie zu puffern), statt den Rest ungelesen stehen zu lassen — sonst
/// würde die nächste Anfrage den Rest der alten Zeile als eigene Antwort
/// lesen und mit einem verwirrenden `InvalidJsonRpc` scheitern.
async fn read_line_capped<R>(
    reader: &mut R,
    partial: &mut PartialLine,
    limit: usize,
) -> McpResult<Vec<u8>>
where
    R: AsyncBufReadExt + Unpin,
{
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
        if !partial.over_limit && partial.bytes.len().saturating_add(chunk.len()) > limit {
            // Ab hier nur noch verwerfen statt puffern, damit eine überlange
            // Zeile nicht unbegrenzt Speicher belegt, während wir bis zum
            // nächsten Zeilenumbruch weiterlesen.
            partial.over_limit = true;
            partial.bytes.clear();
        }
        if !partial.over_limit {
            partial.bytes.extend_from_slice(chunk);
        }
        reader.consume(consumed);
        if done {
            let over_limit = partial.over_limit;
            partial.over_limit = false;
            let line = std::mem::take(&mut partial.bytes);
            return if over_limit {
                Err(McpError::ResponseTooLarge { limit })
            } else {
                Ok(line)
            };
        }
    }
}

/// Filtert `ambient` (üblicherweise `std::env::vars()`) auf
/// [`SAFE_ENV_ALLOWLIST`] und hängt danach die expliziten Paare aus `env`
/// an; spätere, gleichnamige Einträge überschreiben frühere (wie bei
/// [`tokio::process::Command::envs`]).
fn filter_child_env<'a>(
    ambient: &'a [(String, String)],
    env: &'a [(String, String)],
) -> Vec<(&'a str, &'a str)> {
    let mut result: Vec<(&str, &str)> = ambient
        .iter()
        .filter(|(key, _)| {
            SAFE_ENV_ALLOWLIST
                .iter()
                .copied()
                .any(|allowed| key.eq_ignore_ascii_case(allowed))
        })
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    result.extend(env.iter().map(|(key, value)| (key.as_str(), value.as_str())));
    result
}

/// Liest eine `stderr`-Zeile ein, ohne sie unbegrenzt zu puffern: es werden
/// höchstens [`MAX_STDERR_LINE_BYTES`] Bytes behalten, der Rest einer
/// längeren Zeile wird bis zum nächsten Zeilenumbruch verworfen, ohne den
/// Server dabei zu blockieren. Anders als [`read_line_capped`] bricht dies
/// nicht mit einem Fehler ab, da eine lange `stderr`-Ausgabe kein
/// Protokollfehler ist. Liefert `Ok(None)` bei EOF.
async fn read_stderr_line_bounded<R>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>>
where
    R: AsyncBufReadExt + Unpin,
{
    let mut line = Vec::new();
    let mut read_any = false;
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return Ok(if read_any { Some(line) } else { None });
        }
        read_any = true;
        let (chunk, consumed, done) = match buffer.iter().position(|&byte| byte == b'\n') {
            Some(pos) => (&buffer[..pos], pos.saturating_add(1), true),
            None => (buffer, buffer.len(), false),
        };
        if line.len() < MAX_STDERR_LINE_BYTES {
            let take = chunk.len().min(MAX_STDERR_LINE_BYTES.saturating_sub(line.len()));
            line.extend_from_slice(&chunk[..take]);
        }
        reader.consume(consumed);
        if done {
            return Ok(Some(line));
        }
    }
}

/// Liest `stderr` des Servers bis EOF und protokolliert jede Zeile, ohne für
/// eine einzelne, sehr lange oder nie durch `\n` abgeschlossene Zeile
/// unbegrenzt Speicher zu belegen (siehe [`read_stderr_line_bounded`]).
async fn drain_stderr(stderr: tokio::process::ChildStderr, command: String) {
    let mut reader = BufReader::new(stderr);
    loop {
        match read_stderr_line_bounded(&mut reader).await {
            Ok(None) | Err(_) => break,
            Ok(Some(buffer)) => {
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
            var = (params.get("arguments") or {}).get("var", "HARW_STDIO_TEST")
            send({"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text",
                  "text": os.environ.get(var, "<absent>")}]}})
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
    fn spawned_child_only_sees_explicit_env_not_arbitrary_names() -> TestResult {
        if !python_available() {
            return Ok(());
        }
        runtime()?.block_on(async {
            let mut client = spawn_fake().await?;
            client.initialize("harw-test", "0.0.0").await?;
            // Explizit über `env` übergebene Variable ist sichtbar.
            let content = client
                .call_tool("env", json!({"var": "HARW_STDIO_TEST"}))
                .await?;
            assert_eq!(first_text(&content)?, "from-env");
            // Ein beliebiger, nicht übergebener Name ist es nicht.
            let content = client
                .call_tool("env", json!({"var": "HARW_MCP_CLIENT_NOT_PASSED_THROUGH"}))
                .await?;
            assert_eq!(first_text(&content)?, "<absent>");
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
            let mut partial = PartialLine::default();
            assert_eq!(read_line_capped(&mut ok, &mut partial, 3).await?, b"abc");
            assert!(matches!(
                read_line_capped(&mut ok, &mut partial, 3).await,
                Err(McpError::Transport(_))
            ));
            let mut big = BufReader::new(&b"abcdef\n"[..]);
            let mut partial = PartialLine::default();
            assert!(matches!(
                read_line_capped(&mut big, &mut partial, 3).await,
                Err(McpError::ResponseTooLarge { limit: 3 })
            ));
            Ok(())
        })
    }

    /// Regressionstest für Finding 3: Eine überlange Zeile darf den Rest der
    /// Pipe nicht verschmutzen. Nach `ResponseTooLarge` muss die nächste,
    /// eigentlich gültige Zeile korrekt gelesen werden können statt an einem
    /// Bruchstück der alten Zeile zu scheitern.
    #[test]
    fn read_line_capped_recovers_after_response_too_large() -> TestResult {
        runtime()?.block_on(async {
            let limit = 16usize;
            let mut stream = vec![b'a'; limit + 100];
            stream.push(b'\n');
            stream.extend_from_slice(b"ok\n");
            let mut reader = BufReader::new(&stream[..]);
            let mut partial = PartialLine::default();
            match read_line_capped(&mut reader, &mut partial, limit).await {
                Err(McpError::ResponseTooLarge { limit: got }) => assert_eq!(got, limit),
                other => {
                    return Err(TestError::Unexpected(format!(
                        "expected ResponseTooLarge, got {other:?}"
                    )));
                }
            }
            let line = read_line_capped(&mut reader, &mut partial, limit).await?;
            assert_eq!(line, b"ok");
            Ok(())
        })
    }

    /// Regressionstest für Finding 3: Wird das Read-Future durch ein
    /// Zeitlimit zwischen zwei Chunks verworfen, dürfen bereits gelesene
    /// Bytes der angefangenen Zeile nicht verloren gehen.
    #[test]
    fn read_line_capped_survives_cancellation_between_chunks() -> TestResult {
        runtime()?.block_on(async {
            let (mut writer, reader) = tokio::io::duplex(4096);
            let mut buffered = BufReader::new(reader);
            let mut partial = PartialLine::default();
            writer
                .write_all(b"hel")
                .await
                .map_err(ctx("write half a line"))?;
            let timed_out = tokio::time::timeout(
                Duration::from_millis(20),
                read_line_capped(&mut buffered, &mut partial, 1024),
            )
            .await;
            assert!(timed_out.is_err());
            writer
                .write_all(b"lo\n")
                .await
                .map_err(ctx("write rest of line"))?;
            let line = read_line_capped(&mut buffered, &mut partial, 1024).await?;
            assert_eq!(line, b"hello");
            Ok(())
        })
    }

    /// Regressionstest für Finding 2: eine `stderr`-Zeile ohne
    /// Zeilenumbruch darf nicht unbegrenzt Speicher belegen; nach dem Cap
    /// muss der Reader trotzdem korrekt bis EOF weiterlesen.
    #[test]
    fn read_stderr_line_bounded_caps_long_line_and_reaches_eof() -> TestResult {
        runtime()?.block_on(async {
            let (mut writer, reader) = tokio::io::duplex(64 * 1024);
            let write_task = tokio::spawn(async move {
                writer.write_all(&vec![b'a'; 10 * 1024 * 1024]).await?;
                writer.write_all(b"x\n").await?;
                writer.shutdown().await?;
                Ok::<(), std::io::Error>(())
            });
            let mut buffered = BufReader::new(reader);
            let line = read_stderr_line_bounded(&mut buffered)
                .await
                .map_err(ctx("read bounded stderr line"))?
                .ok_or(TestError::Missing("stderr line"))?;
            assert!(line.len() <= MAX_STDERR_LINE_BYTES);
            let eof = read_stderr_line_bounded(&mut buffered)
                .await
                .map_err(ctx("read stderr EOF"))?;
            assert!(eof.is_none());
            write_task
                .await
                .map_err(ctx("stderr writer task"))?
                .map_err(ctx("stderr writer"))?;
            Ok(())
        })
    }

    /// Regressionstest für Finding 1: Nur die Allowlist sowie explizit
    /// übergebene Paare landen in der Kindprozess-Umgebung; alles andere
    /// aus der (simulierten) Prozessumgebung wird entfernt.
    #[test]
    fn filter_child_env_keeps_only_allowlisted_ambient_vars_and_explicit_overrides() {
        let ambient = vec![
            ("PATH".to_owned(), "/usr/bin".to_owned()),
            ("Path".to_owned(), "C:\\Windows".to_owned()),
            ("ANTHROPIC_API_KEY".to_owned(), "sk-secret".to_owned()),
            (
                "CLAUDE_CODE_OAUTH_TOKEN".to_owned(),
                "oauth-secret".to_owned(),
            ),
            ("HOME".to_owned(), "/home/harw".to_owned()),
        ];
        let explicit = vec![("SERVER_TOKEN".to_owned(), "explicit-value".to_owned())];
        let filtered = filter_child_env(&ambient, &explicit);
        let keys: Vec<&str> = filtered.iter().map(|(key, _)| *key).collect();
        assert!(keys.contains(&"PATH"));
        assert!(keys.contains(&"Path"));
        assert!(keys.contains(&"HOME"));
        assert!(keys.contains(&"SERVER_TOKEN"));
        assert!(!keys.contains(&"ANTHROPIC_API_KEY"));
        assert!(!keys.contains(&"CLAUDE_CODE_OAUTH_TOKEN"));
    }
}
