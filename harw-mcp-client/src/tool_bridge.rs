//! Brücke von MCP-Server-Tools in die Tool-Registry des Agenten.
//!
//! Jeder konfigurierte MCP-Server ([`McpServerSpec`]) wird beim
//! [`McpToolProvider::connect`] einmal verbunden (`initialize` +
//! `tools/list`). Seine Tools erscheinen danach als echte Tools mit dem Namen
//! `mcp.<server>.<tool>` (beide Teile auf `[a-z0-9_]` bereinigt).
//!
//! # Verbindungen
//! Pro Server existiert genau eine Verbindung (HTTP oder stdio), die hinter
//! einem [`tokio::sync::Mutex`] liegt. Aufrufe an denselben Server werden
//! dadurch serialisiert; alle Tools melden deshalb `parallel_safe == false`.
//!
//! # Autorität
//! Der Executor prüft vor jedem Aufruf die Sandbox des
//! [`ToolExecutionContext`] (fail closed):
//! - HTTP-Server: `Permission::NetworkAccess` **und** der Host des Endpunkts
//!   muss im Netzwerk-Scope der Sandbox liegen ([`require_host_access`]).
//! - stdio-Server: `Permission::ExecuteProcess` ([`require_permission`]), da
//!   der Aufruf Code in einem lokalen Fremdprozess ausführt.
//!
//! Der Serverprozess bzw. die HTTP-Session selbst entsteht bereits beim
//! Verbinden, also außerhalb jeder Sandbox-Prüfung — der Integrator
//! entscheidet über die Konfiguration, welche Server überhaupt gestartet
//! werden.
//!
//! # Fehlerbild
//! Fachliche Tool-Fehler (`isError: true`) und Transportfehler werden als
//! [`ToolOutput::Error`] an das Modell gereicht, nie als `Err` — der Turn
//! läuft weiter.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use harw_extension_api::ToolProvider;
use harw_tools::{
    AdditionalProperties, FunctionToolSpec, JsonSchema, JsonSchemaType, Permission, ToolCall,
    ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput, ToolSpec,
    require_host_access, require_permission,
};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;

use crate::stdio::McpStdioClient;
use crate::{McpClient, McpContent, McpError, McpResult, McpTool};

/// Obergrenze der an das Modell gereichten Tool-Ausgabe in Bytes.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Zeitlimit für Verbindungsaufbau, `initialize` und `tools/list` pro Server.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Präfix aller gebrückten Tool-Namen.
const NAME_PREFIX: &str = "mcp";

/// Transportweg zu einem MCP-Server.
#[derive(Clone)]
pub enum McpEndpoint {
    /// Streamable-HTTP-Server (siehe [`McpClient`]).
    Http {
        /// Endpunkt-URL, z. B. `https://example.com/mcp`.
        url: String,
        /// Optionales Bearer-Token; wird nie geloggt oder im `Debug` gezeigt.
        token: Option<Vec<u8>>,
    },
    /// Lokaler Serverprozess, der MCP über stdin/stdout spricht.
    Stdio {
        /// Auszuführendes Programm.
        command: String,
        /// Kommandozeilenargumente.
        args: Vec<String>,
        /// Zusätzliche Umgebungsvariablen.
        env: Vec<(String, String)>,
    },
}

impl fmt::Debug for McpEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { url, token } => f
                .debug_struct("Http")
                .field("url", url)
                .field("token", &token.as_ref().map(|_| "<redacted>"))
                .finish(),
            // Umgebungswerte können Geheimnisse enthalten: nur die Schlüssel zeigen.
            Self::Stdio { command, args, env } => f
                .debug_struct("Stdio")
                .field("command", command)
                .field("args", args)
                .field(
                    "env_keys",
                    &env.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(),
                )
                .finish(),
        }
    }
}

/// Konfiguration eines einzelnen MCP-Servers.
#[derive(Debug, Clone)]
pub struct McpServerSpec {
    /// Servername; bildet (bereinigt) den mittleren Teil von `mcp.<server>.<tool>`.
    pub name: String,
    /// Transportweg.
    pub endpoint: McpEndpoint,
    /// Erlaubte Tools (MCP-Originalname oder bereinigter Name). Leer = alle.
    pub allowed_tools: Vec<String>,
}

/// Eine offene Verbindung, unabhängig vom Transport.
enum Connection {
    Http(McpClient),
    Stdio(McpStdioClient),
}

impl Connection {
    async fn open(endpoint: &McpEndpoint) -> McpResult<Self> {
        match endpoint {
            McpEndpoint::Http { url, token } => Ok(Self::Http(McpClient::new(url, token.clone())?)),
            McpEndpoint::Stdio { command, args, env } => Ok(Self::Stdio(
                McpStdioClient::spawn(command, args, env).await?,
            )),
        }
    }

    async fn initialize(&mut self, client_name: &str, client_version: &str) -> McpResult<()> {
        match self {
            Self::Http(client) => client.initialize(client_name, client_version).await,
            Self::Stdio(client) => client.initialize(client_name, client_version).await,
        }
    }

    async fn list_all_tools(&mut self) -> McpResult<Vec<McpTool>> {
        match self {
            Self::Http(client) => client.list_all_tools().await,
            Self::Stdio(client) => client.list_all_tools().await,
        }
    }

    async fn call_tool(&mut self, name: &str, arguments: Value) -> McpResult<Vec<McpContent>> {
        match self {
            Self::Http(client) => client.call_tool(name, arguments).await,
            Self::Stdio(client) => client.call_tool(name, arguments).await,
        }
    }

    async fn close(&mut self) -> McpResult<()> {
        match self {
            Self::Http(client) => client.close().await,
            Self::Stdio(client) => client.close().await,
        }
    }
}

/// Welche Autorität ein Aufruf an diesen Server verlangt.
#[derive(Debug, Clone)]
enum Authority {
    /// Netzwerkzugriff auf genau diesen Host.
    Network { host: String },
    /// Ausführung eines lokalen Prozesses.
    Process,
}

/// Geteilter Zustand eines verbundenen Servers.
struct ServerConnection {
    connection: Mutex<Connection>,
    authority: Authority,
}

/// Ausführer eines einzelnen gebrückten MCP-Tools.
struct McpToolExecutor {
    /// Bereinigter, vollqualifizierter Name (`mcp.<server>.<tool>`).
    name: String,
    /// Originalname des Tools auf dem MCP-Server.
    remote_name: String,
    spec: ToolSpec,
    server: Arc<ServerConnection>,
}

impl McpToolExecutor {
    /// Leitet die Argumente an `tools/call` weiter und rendert das Ergebnis.
    /// Enthält keine Autoritätsprüfung — diese erledigt [`ToolExecutor::execute`].
    async fn invoke(&self, arguments: Value) -> ToolOutput {
        let arguments = match arguments {
            Value::Null => json!({}),
            Value::Object(map) => Value::Object(map),
            _ => {
                return ToolOutput::error(format!(
                    "{}: arguments must be a JSON object",
                    self.name
                ));
            }
        };
        let result = {
            let mut connection = self.server.connection.lock().await;
            connection.call_tool(&self.remote_name, arguments).await
        };
        match result {
            Ok(content) => ToolOutput::text(render_content(&content)),
            Err(McpError::ToolCallFailed(content)) => {
                let rendered = render_content(&content);
                if rendered.is_empty() {
                    ToolOutput::error(format!("{}: MCP tool reported an error", self.name))
                } else {
                    ToolOutput::error(format!("{}: {rendered}", self.name))
                }
            }
            Err(error) => {
                tracing::warn!(tool = %self.name, %error, "mcp.tool_call.failed");
                ToolOutput::error(format!("{}: {error}", self.name))
            }
        }
    }
}

impl ToolExecutor for McpToolExecutor {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let denied = match &self.server.authority {
                Authority::Network { host } => require_host_access(context, host, &self.name),
                Authority::Process => {
                    require_permission(context, Permission::ExecuteProcess, &self.name)
                }
            };
            if let Some(denied) = denied {
                return Ok(denied);
            }
            if context.cancel().is_some_and(|cancel| cancel.is_cancelled()) {
                return Err(harw_tools::ToolsError::Cancelled);
            }
            Ok(self.invoke(call.arguments.clone()).await)
        })
    }
}

/// [`ToolProvider`], der die Tools aller verbundenen MCP-Server anbietet.
pub struct McpToolProvider {
    tools: BTreeMap<String, Arc<McpToolExecutor>>,
    servers: Vec<Arc<ServerConnection>>,
}

impl fmt::Debug for McpToolProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpToolProvider")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .field("servers", &self.servers.len())
            .finish()
    }
}

impl McpToolProvider {
    /// Verbindet alle Server (`initialize` + `tools/list`), hält pro Server
    /// eine Verbindung hinter einem [`Mutex`], filtert nach `allowed_tools`
    /// und liefert den Provider plus Warnungen.
    ///
    /// Scheitert ein Server (Verbindung, Handshake, Zeitlimit von 30 s), wird
    /// er übersprungen und eine Warnung erzeugt; die Funktion selbst schlägt
    /// nie fehl. Warnungen entstehen außerdem für Namenskollisionen nach der
    /// Bereinigung und für `allowed_tools`-Einträge, die der Server nicht
    /// anbietet.
    pub async fn connect(
        servers: Vec<McpServerSpec>,
        client_name: &str,
        client_version: &str,
    ) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut tools: BTreeMap<String, Arc<McpToolExecutor>> = BTreeMap::new();
        let mut connected = Vec::new();

        for server in servers {
            let authority = match authority_for(&server.endpoint) {
                Ok(authority) => authority,
                Err(reason) => {
                    warnings.push(format!("MCP server '{}' skipped: {reason}", server.name));
                    continue;
                }
            };
            let handshake = tokio::time::timeout(
                CONNECT_TIMEOUT,
                open_and_list(&server, client_name, client_version),
            )
            .await;
            let (connection, listed) = match handshake {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    warnings.push(format!("MCP server '{}' unavailable: {error}", server.name));
                    continue;
                }
                Err(_) => {
                    warnings.push(format!(
                        "MCP server '{}' unavailable: connect timed out after {}s",
                        server.name,
                        CONNECT_TIMEOUT.as_secs()
                    ));
                    continue;
                }
            };
            let selected = select_tools(&server, listed, &mut warnings);
            let shared = Arc::new(ServerConnection {
                connection: Mutex::new(connection),
                authority,
            });
            let mut registered = 0usize;
            for (name, tool) in selected {
                if tools.contains_key(&name) {
                    warnings.push(format!(
                        "MCP tool '{}' of server '{}' skipped: name '{name}' is already registered",
                        tool.name, server.name
                    ));
                    continue;
                }
                let spec = build_spec(&name, &server.name, &tool);
                tools.insert(
                    name.clone(),
                    Arc::new(McpToolExecutor {
                        name,
                        remote_name: tool.name,
                        spec,
                        server: Arc::clone(&shared),
                    }),
                );
                registered += 1;
            }
            tracing::info!(server = %server.name, tools = registered, "mcp.server.connected");
            connected.push(shared);
        }

        (
            Self {
                tools,
                servers: connected,
            },
            warnings,
        )
    }

    /// Sortierte Namen aller gebrückten Tools (`mcp.<server>.<tool>`).
    #[must_use]
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Schließt alle Serververbindungen (best effort) und liefert Fehler als
    /// Warnungen. Danach aufgerufene Tools melden Transportfehler.
    pub async fn shutdown(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        for server in &self.servers {
            if let Err(error) = server.connection.lock().await.close().await {
                warnings.push(format!("MCP server close failed: {error}"));
            }
        }
        warnings
    }
}

impl ToolProvider for McpToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|tool| tool.spec.clone()).collect()
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        self.tools
            .get(name.as_str())
            .map(|tool| Arc::clone(tool) as Arc<dyn ToolExecutor>)
    }

    fn parallel_safe(&self, _name: &ToolName) -> bool {
        // Eine geteilte Verbindung pro Server: Aufrufe werden ohnehin
        // serialisiert, Seiteneffekte der Fremd-Tools sind unbekannt.
        false
    }
}

/// Öffnet die Verbindung, führt den Handshake aus und listet alle Tools.
async fn open_and_list(
    server: &McpServerSpec,
    client_name: &str,
    client_version: &str,
) -> McpResult<(Connection, Vec<McpTool>)> {
    let mut connection = Connection::open(&server.endpoint).await?;
    let listed = async {
        connection.initialize(client_name, client_version).await?;
        connection.list_all_tools().await
    }
    .await;
    match listed {
        Ok(tools) => Ok((connection, tools)),
        Err(error) => {
            // Best effort: den halb geöffneten Server nicht verwaist zurücklassen.
            let _ = connection.close().await;
            Err(error)
        }
    }
}

/// Leitet die für Aufrufe erforderliche Autorität aus dem Endpunkt ab.
fn authority_for(endpoint: &McpEndpoint) -> Result<Authority, String> {
    match endpoint {
        McpEndpoint::Http { url, .. } => {
            let parsed = url::Url::parse(url).map_err(|error| format!("invalid URL: {error}"))?;
            let host = parsed
                .host_str()
                .ok_or_else(|| "URL has no host".to_owned())?;
            Ok(Authority::Network {
                host: host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .to_owned(),
            })
        }
        McpEndpoint::Stdio { .. } => Ok(Authority::Process),
    }
}

/// Bereinigt einen Namensteil auf `[a-z0-9_]`: ASCII wird kleingeschrieben,
/// alles andere wird zu `_`. Ein leeres Ergebnis wird zu `_`.
fn sanitize_segment(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            let lower = c.to_ascii_lowercase();
            if lower.is_ascii_lowercase() || lower.is_ascii_digit() || lower == '_' {
                lower
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "_".to_owned()
    } else {
        cleaned
    }
}

/// Bildet den vollqualifizierten Tool-Namen `mcp.<server>.<tool>`.
fn bridged_tool_name(server: &str, tool: &str) -> String {
    format!(
        "{NAME_PREFIX}.{}.{}",
        sanitize_segment(server),
        sanitize_segment(tool)
    )
}

/// Filtert die gelisteten Tools nach `allowed_tools` (Original- oder
/// bereinigter Name) und liefert `(bridged_name, tool)`-Paare. Einträge in
/// `allowed_tools`, die kein Tool treffen, erzeugen eine Warnung.
fn select_tools(
    server: &McpServerSpec,
    listed: Vec<McpTool>,
    warnings: &mut Vec<String>,
) -> Vec<(String, McpTool)> {
    let mut matched = vec![false; server.allowed_tools.len()];
    let mut selected = Vec::new();
    for tool in listed {
        let sanitized = sanitize_segment(&tool.name);
        let allowed = server.allowed_tools.is_empty()
            || server
                .allowed_tools
                .iter()
                .enumerate()
                .fold(false, |found, (index, entry)| {
                    let hit = entry == &tool.name || sanitize_segment(entry) == sanitized;
                    if hit {
                        matched[index] = true;
                    }
                    found || hit
                });
        if allowed {
            selected.push((bridged_tool_name(&server.name, &tool.name), tool));
        }
    }
    for (entry, hit) in server.allowed_tools.iter().zip(matched) {
        if !hit {
            warnings.push(format!(
                "MCP server '{}' does not offer allowed tool '{entry}'",
                server.name
            ));
        }
    }
    selected
}

/// Baut die [`ToolSpec`] eines gebrückten Tools.
fn build_spec(name: &str, server: &str, tool: &McpTool) -> ToolSpec {
    let summary = tool
        .description
        .as_deref()
        .or(tool.title.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map_or_else(|| format!("MCP tool '{}'.", tool.name), str::to_owned);
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(name),
        description: format!("[MCP {server}] {summary}"),
        parameters: convert_schema(&tool.input_schema),
        strict: false,
    })
}

/// Übersetzt das MCP-`inputSchema` in die [`JsonSchema`]-Teilmenge.
///
/// Nicht abbildbare Konstrukte (Typ-Arrays, Tupel-`items`, boolesche
/// Teilschemata, `oneOf`) werden vorher entschärft; scheitert die Abbildung
/// trotzdem, gilt das permissive `{"type":"object"}`. Die Wurzel ist immer
/// vom Typ `object`.
fn convert_schema(input: &Value) -> JsonSchema {
    let mut schema = match input {
        Value::Object(_) => {
            serde_json::from_value::<JsonSchema>(normalize_schema(input)).unwrap_or_default()
        }
        _ => JsonSchema::default(),
    };
    if schema.schema_type != Some(JsonSchemaType::Object) {
        schema = JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            ..JsonSchema::default()
        };
    }
    if schema.properties.is_none() && schema.additional_properties.is_none() {
        schema.additional_properties = Some(Box::new(AdditionalProperties::Bool(true)));
    }
    schema
}

/// Bringt einen beliebigen JSON-Schema-Wert rekursiv in eine Form, die
/// [`JsonSchema`] deserialisieren kann.
fn normalize_schema(value: &Value) -> Value {
    let Value::Object(source) = value else {
        // Boolesche Schemata (`true`) oder Unsinn: beliebiger Wert erlaubt.
        return json!({});
    };
    let mut out = Map::new();
    for (key, entry) in source {
        match key.as_str() {
            "type" => {
                let chosen = match entry {
                    Value::String(kind) => Some(kind.clone()),
                    Value::Array(kinds) => kinds
                        .iter()
                        .filter_map(Value::as_str)
                        .find(|kind| *kind != "null")
                        .map(str::to_owned),
                    _ => None,
                };
                let known = [
                    "string", "number", "integer", "boolean", "object", "array", "null",
                ];
                if let Some(kind) = chosen.filter(|kind| known.contains(&kind.as_str())) {
                    out.insert("type".to_owned(), Value::String(kind));
                }
            }
            "description" => {
                if let Value::String(text) = entry {
                    out.insert(key.clone(), Value::String(text.clone()));
                }
            }
            "properties" => {
                if let Value::Object(props) = entry {
                    let mapped: Map<String, Value> = props
                        .iter()
                        .map(|(name, sub)| (name.clone(), normalize_schema(sub)))
                        .collect();
                    out.insert(key.clone(), Value::Object(mapped));
                }
            }
            "required" => {
                if let Value::Array(names) = entry {
                    let names: Vec<Value> = names
                        .iter()
                        .filter(|name| name.is_string())
                        .cloned()
                        .collect();
                    out.insert(key.clone(), Value::Array(names));
                }
            }
            "additionalProperties" => match entry {
                Value::Bool(flag) => {
                    out.insert(key.clone(), Value::Bool(*flag));
                }
                Value::Object(_) => {
                    out.insert(key.clone(), normalize_schema(entry));
                }
                _ => {}
            },
            "anyOf" | "oneOf" => {
                if let Value::Array(variants) = entry {
                    if !out.contains_key("anyOf") {
                        let mapped = variants.iter().map(normalize_schema).collect();
                        out.insert("anyOf".to_owned(), Value::Array(mapped));
                    }
                }
            }
            "items" => {
                if entry.is_object() {
                    out.insert(key.clone(), normalize_schema(entry));
                }
            }
            "enum" => {
                if entry.is_array() {
                    out.insert(key.clone(), entry.clone());
                }
            }
            "default" => {
                out.insert(key.clone(), entry.clone());
            }
            _ => {}
        }
    }
    Value::Object(out)
}

/// Rendert MCP-Inhalte zu Text: Texte werden mit Zeilenumbruch verbunden,
/// Bilder und Binärressourcen als kurze Platzhalter, Textressourcen mit
/// Kopfzeile. Das Ergebnis ist auf [`MAX_OUTPUT_BYTES`] begrenzt.
fn render_content(content: &[McpContent]) -> String {
    let parts: Vec<String> = content
        .iter()
        .map(|item| match item {
            McpContent::Text { text } => text.clone(),
            McpContent::Image { data, mime_type } => {
                format!("[image: {mime_type}, {} bytes base64]", data.len())
            }
            McpContent::Resource { resource } => {
                let mime = resource.mime_type.as_deref().unwrap_or("unknown type");
                match (&resource.text, &resource.blob) {
                    (Some(text), _) => format!("[resource: {} ({mime})]\n{text}", resource.uri),
                    (None, Some(blob)) => format!(
                        "[resource: {} ({mime}), {} bytes base64]",
                        resource.uri,
                        blob.len()
                    ),
                    (None, None) => format!("[resource: {} ({mime})]", resource.uri),
                }
            }
            McpContent::Unknown => "[unsupported MCP content]".to_owned(),
        })
        .collect();
    truncate_output(parts.join("\n"))
}

/// Kürzt auf [`MAX_OUTPUT_BYTES`] an einer Zeichengrenze und hängt einen
/// Hinweis an.
fn truncate_output(mut text: String) -> String {
    if text.len() <= MAX_OUTPUT_BYTES {
        return text;
    }
    let original = text.len();
    let mut cut = MAX_OUTPUT_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text.push_str(&format!(
        "\n[output truncated: {original} bytes exceeded the {MAX_OUTPUT_BYTES} byte limit]"
    ));
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::McpResourceContents;
    use crate::test_support::{TestError, TestResult};

    fn tool(name: &str) -> McpTool {
        McpTool {
            name: name.to_owned(),
            title: None,
            description: Some(format!("does {name}")),
            input_schema: json!({"type": "object"}),
        }
    }

    fn server(name: &str, allowed: &[&str]) -> McpServerSpec {
        McpServerSpec {
            name: name.to_owned(),
            endpoint: McpEndpoint::Stdio {
                command: "true".to_owned(),
                args: Vec::new(),
                env: Vec::new(),
            },
            allowed_tools: allowed.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn sanitizes_names_to_lowercase_ascii_and_underscores() {
        assert_eq!(sanitize_segment("GitHub-Server"), "github_server");
        assert_eq!(sanitize_segment("read.file/v2"), "read_file_v2");
        assert_eq!(sanitize_segment("ümlaut"), "_mlaut");
        assert_eq!(sanitize_segment(""), "_");
        assert_eq!(
            bridged_tool_name("My Server", "listIssues"),
            "mcp.my_server.listissues"
        );
    }

    #[test]
    fn renders_text_image_resource_and_unknown() {
        let content = vec![
            McpContent::Text {
                text: "hello".to_owned(),
            },
            McpContent::Image {
                data: "YWJj".to_owned(),
                mime_type: "image/png".to_owned(),
            },
            McpContent::Resource {
                resource: McpResourceContents {
                    uri: "file:///a.txt".to_owned(),
                    mime_type: Some("text/plain".to_owned()),
                    text: Some("abc".to_owned()),
                    blob: None,
                },
            },
            McpContent::Resource {
                resource: McpResourceContents {
                    uri: "file:///b.bin".to_owned(),
                    mime_type: None,
                    text: None,
                    blob: Some("AAAA".to_owned()),
                },
            },
            McpContent::Unknown,
        ];
        assert_eq!(
            render_content(&content),
            "hello\n[image: image/png, 4 bytes base64]\n[resource: file:///a.txt (text/plain)]\nabc\n\
             [resource: file:///b.bin (unknown type), 4 bytes base64]\n[unsupported MCP content]"
        );
    }

    #[test]
    fn caps_output_at_64_kib_on_char_boundary() {
        let text = "ä".repeat(MAX_OUTPUT_BYTES); // 2 Bytes pro Zeichen
        let rendered = render_content(&[McpContent::Text { text }]);
        let (body, note) = rendered
            .split_once("\n[output truncated")
            .unwrap_or((rendered.as_str(), ""));
        assert!(!note.is_empty());
        assert!(body.len() <= MAX_OUTPUT_BYTES);
        assert!(body.chars().all(|c| c == 'ä'));
    }

    #[test]
    fn empty_allowed_tools_selects_everything() {
        let mut warnings = Vec::new();
        let selected = select_tools(
            &server("srv", &[]),
            vec![tool("a"), tool("b")],
            &mut warnings,
        );
        let names: Vec<_> = selected.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["mcp.srv.a", "mcp.srv.b"]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn allowed_tools_filters_by_raw_or_sanitized_name_and_warns_on_misses() {
        let mut warnings = Vec::new();
        let selected = select_tools(
            &server("srv", &["Read-File", "search", "missing"]),
            vec![tool("read-file"), tool("search"), tool("delete")],
            &mut warnings,
        );
        let names: Vec<_> = selected.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["mcp.srv.read_file", "mcp.srv.search"]);
        assert_eq!(selected[0].1.name, "read-file");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("missing"));
    }

    #[test]
    fn spec_uses_prefixed_description_and_input_schema() -> TestResult {
        let mut mcp_tool = tool("echo");
        mcp_tool.input_schema = json!({
            "type": "object",
            "properties": {
                "text": {"type": "string", "description": "what to echo"},
                "count": {"type": ["integer", "null"]},
                "tags": {"type": "array", "items": {"type": "string"}},
                "anything": true
            },
            "required": ["text"],
            "$schema": "http://json-schema.org/draft-07/schema#"
        });
        let ToolSpec::Function(spec) = build_spec("mcp.srv.echo", "srv", &mcp_tool);
        assert_eq!(spec.name.as_str(), "mcp.srv.echo");
        assert_eq!(spec.description, "[MCP srv] does echo");
        let props = spec
            .parameters
            .properties
            .as_ref()
            .ok_or(TestError::Missing("properties"))?;
        assert_eq!(props.len(), 4);
        assert_eq!(
            props.get("count").and_then(|s| s.schema_type.clone()),
            Some(JsonSchemaType::Integer)
        );
        assert_eq!(spec.parameters.required, Some(vec!["text".to_owned()]));
        Ok(())
    }

    #[test]
    fn missing_or_invalid_schema_defaults_to_object() {
        for input in [
            Value::Null,
            json!({"type": "string"}),
            json!({"properties": 5}),
        ] {
            let schema = convert_schema(&input);
            assert_eq!(schema.schema_type, Some(JsonSchemaType::Object));
        }
        let mut untitled = tool("x");
        untitled.description = None;
        let ToolSpec::Function(spec) = build_spec("mcp.s.x", "s", &untitled);
        assert_eq!(spec.description, "[MCP s] MCP tool 'x'.");
    }

    #[test]
    fn debug_redacts_token_and_env_values() {
        let http = McpEndpoint::Http {
            url: "https://example.com/mcp".to_owned(),
            token: Some(b"secret-token".to_vec()),
        };
        let stdio = McpEndpoint::Stdio {
            command: "srv".to_owned(),
            args: Vec::new(),
            env: vec![("API_KEY".to_owned(), "secret-value".to_owned())],
        };
        let rendered = format!("{http:?} {stdio:?}");
        assert!(!rendered.contains("secret"));
        assert!(rendered.contains("API_KEY"));
    }

    #[test]
    fn http_authority_uses_endpoint_host() {
        let endpoint = McpEndpoint::Http {
            url: "https://mcp.example.com:8443/mcp".to_owned(),
            token: None,
        };
        assert!(matches!(
            authority_for(&endpoint),
            Ok(Authority::Network { host }) if host == "mcp.example.com"
        ));
    }

    /// Führt ein Future auf einer frischen current-thread-Runtime aus
    /// (das Crate hat kein `tokio/macros`).
    fn block_on<F: std::future::Future>(future: F) -> TestResult<F::Output> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(crate::test_support::ctx("tokio runtime"))?;
        Ok(runtime.block_on(future))
    }

    #[test]
    fn failing_servers_become_warnings() -> TestResult {
        let spec = McpServerSpec {
            name: "broken".to_owned(),
            endpoint: McpEndpoint::Http {
                url: "http://example.test/mcp".to_owned(),
                token: None,
            },
            allowed_tools: Vec::new(),
        };
        let (provider, warnings) = block_on(McpToolProvider::connect(vec![spec], "harw", "0.0.0"))?;
        assert!(provider.tool_names().is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("broken"));
        Ok(())
    }

    /// Minimaler MCP-stdio-Server (zeilenweises JSON-RPC) für den E2E-Test.
    const FAKE_SERVER: &str = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if "id" not in msg:
        continue
    method = msg.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                  "serverInfo": {"name": "fake", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [
            {"name": "Echo-Text", "description": "Echoes text",
             "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}},
            {"name": "fail", "description": "Always fails", "inputSchema": {"type": "object"}},
            {"name": "hidden", "inputSchema": {"type": "object"}}]}
    elif method == "tools/call":
        params = msg["params"]
        if params["name"] == "fail":
            result = {"content": [{"type": "text", "text": "kaputt"}], "isError": True}
        else:
            result = {"content": [{"type": "text", "text": "echo:" + params["arguments"].get("text", "")},
                                  {"type": "image", "data": "AAAA", "mimeType": "image/png"}]}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"],
                          "error": {"code": -32601, "message": "unknown"}}), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
"#;

    fn python_available() -> bool {
        std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    }

    #[test]
    fn end_to_end_against_fake_stdio_server() -> TestResult {
        if !python_available() {
            eprintln!("python3 not available; skipping MCP stdio end-to-end test");
            return Ok(());
        }
        block_on(end_to_end())?
    }

    async fn end_to_end() -> TestResult {
        let spec = McpServerSpec {
            name: "Fake".to_owned(),
            endpoint: McpEndpoint::Stdio {
                command: "python3".to_owned(),
                args: vec!["-c".to_owned(), FAKE_SERVER.to_owned()],
                env: Vec::new(),
            },
            allowed_tools: vec!["echo-text".to_owned(), "fail".to_owned()],
        };
        let (provider, warnings) = McpToolProvider::connect(vec![spec], "harw", "0.0.0").await;
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(
            provider.tool_names(),
            ["mcp.fake.echo_text", "mcp.fake.fail"]
        );
        assert!(!provider.parallel_safe(&ToolName::new("mcp.fake.echo_text")));
        assert!(
            provider
                .executor(&ToolName::new("mcp.fake.hidden"))
                .is_none()
        );

        let specs = provider.tools();
        let ToolSpec::Function(echo_spec) = &specs[0];
        assert_eq!(echo_spec.description, "[MCP Fake] Echoes text");

        let echo = provider
            .tools
            .get("mcp.fake.echo_text")
            .ok_or(TestError::Missing("echo executor"))?;
        match echo.invoke(json!({"text": "hi"})).await {
            ToolOutput::Text { content } => {
                assert_eq!(content, "echo:hi\n[image: image/png, 4 bytes base64]");
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        match echo.invoke(json!("not an object")).await {
            ToolOutput::Error { message } => assert!(message.contains("JSON object")),
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }

        let fail = provider
            .tools
            .get("mcp.fake.fail")
            .ok_or(TestError::Missing("fail executor"))?;
        match fail.invoke(Value::Null).await {
            ToolOutput::Error { message } => assert_eq!(message, "mcp.fake.fail: kaputt"),
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }

        let close_warnings = provider.shutdown().await;
        assert!(close_warnings.is_empty(), "{close_warnings:?}");
        Ok(())
    }
}
