//! Loopback-only HTTP/1 adapter for the Streamable HTTP MCP session boundary.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use bytes::Bytes;
use harw_session_store::SessionStoreError;
use http_body_util::{BodyExt, Channel, Full, LengthLimitError, Limited, combinators::BoxBody};
use hyper::body::{Body, Incoming};
use hyper::header::{ACCEPT, ALLOW, CONTENT_TYPE, HeaderValue, ORIGIN};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use jiff::SignedDuration;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, watch};
use tokio::task::JoinSet;

use crate::auth::{McpAuthenticator, StaticBearerAuthenticator};
use crate::events::{McpEventBus, McpEventReceiveError, McpLifecycleEventKind};
use crate::session::{McpServerError, McpSessionRegistry};
use crate::supervisor::{
    McpJobCapability, McpJobSubmission, McpPrincipal, McpRequestContext, McpSubmittedJobKind,
    McpSupervisor, McpSupervisorError,
};

const PATH: &str = "/mcp";
/// Single source of truth for the advertised MCP protocol version (Z1-R3-04):
/// re-exported from the crate root instead of redefining the literal here.
use crate::MCP_PROTOCOL_VERSION as VERSION;
const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";
const MAX_BODY_BYTES: usize = 64 * 1024;

type McpResponse = Response<BoxBody<Bytes, Infallible>>;

/// Maps an authenticated `principal_key` to its resolved workspace authority.
/// Built once at composition time from server-trusted configuration; never
/// derived from an MCP request.
///
/// Principal-IDs sind eindeutig: Früher war dies ein `HashMap`-Alias, bei dem
/// ein zweiter Eintrag mit gleicher ID den ersten stillschweigend überschrieb
/// (F-069 / A1). Dadurch konnten beide Credentials die Rechte des letzten
/// Eintrags erben, z. B. `CancelWorkspace` in einem fremden Tenant.
/// [`PrincipalRegistry::try_insert`] weist Duplikate deshalb ab.
#[derive(Debug, Clone, Default)]
pub struct PrincipalRegistry {
    principals: HashMap<String, McpPrincipal>,
    /// IDs, die über den Kompatibilitätspfad [`PrincipalRegistry::insert`]
    /// mehrfach konfiguriert wurden. Sie bleiben dauerhaft ohne Rechte
    /// (fail-closed), auch wenn später noch ein Eintrag folgt.
    conflicted: HashSet<String>,
}

/// Eine Principal-ID war bereits registriert (oder als widersprüchlich
/// gesperrt). Der bestehende Eintrag bleibt unverändert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicatePrincipalId {
    pub principal_key: String,
}

impl fmt::Display for DuplicatePrincipalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MCP principal id '{}' is configured more than once",
            self.principal_key
        )
    }
}

impl std::error::Error for DuplicatePrincipalId {}

impl PrincipalRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registriert `principal_key` genau einmal. Ein Duplikat wird mit
    /// [`DuplicatePrincipalId`] abgewiesen, der erste Eintrag bleibt
    /// unverändert. Die Komposition muss den Start dann abbrechen.
    pub fn try_insert(
        &mut self,
        principal_key: String,
        principal: McpPrincipal,
    ) -> Result<(), DuplicatePrincipalId> {
        let taken = self.conflicted.contains(&principal_key)
            || self.principals.contains_key(&principal_key);
        if taken {
            return Err(DuplicatePrincipalId { principal_key });
        }
        self.principals.insert(principal_key, principal);
        Ok(())
    }

    /// Kompatibilitätspfad für Aufrufer, die noch nicht auf
    /// [`PrincipalRegistry::try_insert`] umgestellt sind (`harw-cli`
    /// `build_principal_registry`, Folgearbeit D7). Anders als das frühere
    /// `HashMap::insert` überschreibt ein Duplikat nichts: Die ID verliert
    /// fail-closed **alle** Rechte (leerer Tool-Katalog, keine Tool-Aufrufe),
    /// statt die Rechte des letzten Eintrags zu erben.
    pub fn insert(&mut self, principal_key: String, principal: McpPrincipal) {
        if let Err(duplicate) = self.try_insert(principal_key, principal) {
            // Nur die ID ins Log, keine Credentials.
            eprintln!(
                "harw-mcp-server: {duplicate}; principal disabled until the configuration is fixed"
            );
            self.principals.remove(&duplicate.principal_key);
            self.conflicted.insert(duplicate.principal_key);
        }
    }

    #[must_use]
    pub fn get(&self, principal_key: &str) -> Option<&McpPrincipal> {
        if self.conflicted.contains(principal_key) {
            return None;
        }
        self.principals.get(principal_key)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.principals.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.principals.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct McpListenerConfig {
    pub address: SocketAddr,
    pub max_sessions: usize,
    pub session_ttl: SignedDuration,
}

/// A bound but not yet serving loopback MCP endpoint. `serve` owns the accept
/// loop so the CLI can run it under its supervisor/cancellation policy.
pub struct BoundMcpListener {
    listener: TcpListener,
    state: Arc<Mutex<McpSessionRegistry>>,
    authenticator: Arc<dyn McpAuthenticator>,
    principals: Arc<PrincipalRegistry>,
    supervisor: Option<Arc<dyn McpSupervisor>>,
    event_bus: Option<Arc<McpEventBus>>,
}

impl BoundMcpListener {
    pub async fn bind(config: McpListenerConfig) -> std::io::Result<Self> {
        Self::bind_authenticated(config, Arc::new(StaticBearerAuthenticator::new(Vec::new()))).await
    }
    pub async fn bind_authenticated(
        config: McpListenerConfig,
        authenticator: Arc<dyn McpAuthenticator>,
    ) -> std::io::Result<Self> {
        Self::bind_with_supervisor(config, authenticator, PrincipalRegistry::new(), None).await
    }

    /// Composition entry point once a durable job supervisor and resolved
    /// principal authority are available. `principals` maps each
    /// authenticator-issued `principal_key` to its capability-scoped
    /// [`McpPrincipal`]; a `principal_key` absent from this map can
    /// authenticate but sees an empty tool catalog and cannot call tools.
    pub async fn bind_with_supervisor(
        config: McpListenerConfig,
        authenticator: Arc<dyn McpAuthenticator>,
        principals: PrincipalRegistry,
        supervisor: Option<Arc<dyn McpSupervisor>>,
    ) -> std::io::Result<Self> {
        Self::bind_with_supervisor_and_events(config, authenticator, principals, supervisor, None)
            .await
    }

    pub async fn bind_with_supervisor_and_events(
        config: McpListenerConfig,
        authenticator: Arc<dyn McpAuthenticator>,
        principals: PrincipalRegistry,
        supervisor: Option<Arc<dyn McpSupervisor>>,
        event_bus: Option<Arc<McpEventBus>>,
    ) -> std::io::Result<Self> {
        if !config.address.ip().is_loopback() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "MCP listener must bind loopback",
            ));
        }
        Ok(Self {
            listener: TcpListener::bind(config.address).await?,
            state: Arc::new(Mutex::new(McpSessionRegistry::new(
                config.max_sessions,
                config.session_ttl,
            ))),
            authenticator,
            principals: Arc::new(principals),
            supervisor,
            event_bus,
        })
    }
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }
    pub async fn serve(self) -> std::io::Result<()> {
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        self.serve_until(shutdown_rx).await
    }

    /// Serves connections until the caller signals shutdown. Existing HTTP
    /// connections are aborted and joined so a test or embedding supervisor
    /// cannot leave detached transport tasks behind.
    pub async fn serve_until(self, mut shutdown: watch::Receiver<bool>) -> std::io::Result<()> {
        let mut connections: JoinSet<()> = JoinSet::new();
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                completed = connections.join_next(), if !connections.is_empty() => {
                    let _ = completed;
                }
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted?;
                    let state = Arc::clone(&self.state);
                    let authenticator = Arc::clone(&self.authenticator);
                    let principals = Arc::clone(&self.principals);
                    let supervisor = self.supervisor.clone();
                    let event_bus = self.event_bus.clone();
                    connections.spawn(async move {
                        let _ = Builder::new(TokioExecutor::new())
                            .serve_connection(
                                TokioIo::new(stream),
                                service_fn(move |request| {
                                    handle(
                                        request,
                                        Arc::clone(&state),
                                        Arc::clone(&authenticator),
                                        Arc::clone(&principals),
                                        supervisor.clone(),
                                        event_bus.clone(),
                                    )
                                }),
                            )
                            .await;
                    });
                }
            }
        }
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        Ok(())
    }
}

async fn handle(
    request: Request<Incoming>,
    state: Arc<Mutex<McpSessionRegistry>>,
    authenticator: Arc<dyn McpAuthenticator>,
    principals: Arc<PrincipalRegistry>,
    supervisor: Option<Arc<dyn McpSupervisor>>,
    event_bus: Option<Arc<McpEventBus>>,
) -> Result<McpResponse, Infallible> {
    let principal = match authenticator.authenticate(
        request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok()),
    ) {
        Ok(principal) => principal,
        Err(_) => return Ok(empty_response(StatusCode::UNAUTHORIZED)),
    };
    if !origin_is_loopback(request.headers()) {
        return Ok(empty_response(StatusCode::FORBIDDEN));
    }
    if request.uri().path() != PATH {
        return Ok(response(StatusCode::NOT_FOUND, Value::Null, None));
    }
    match *request.method() {
        Method::POST => {
            post(
                request,
                state,
                principal.principal_key,
                principals,
                supervisor,
                event_bus,
            )
            .await
        }
        Method::DELETE => delete(request, state, &principal.principal_key).await,
        Method::GET => get(request, state, principal.principal_key, event_bus).await,
        _ => Ok(method_not_allowed()),
    }
}

/// A local listener still needs an Origin check: browsers can otherwise reach
/// it through DNS rebinding. Native MCP clients normally omit Origin; a
/// supplied Origin must name a loopback host.
fn origin_is_loopback(headers: &hyper::HeaderMap) -> bool {
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

    let Some(host) = origin_host(authority) else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// Returns the host from a serialized Origin authority only when the authority
/// is structurally complete. In particular, a bracketed IPv6 literal must end
/// at `]` or be followed by a valid port; accepting a prefix would permit
/// values such as `[::1]untrusted.example` to masquerade as loopback.
fn origin_host(authority: &str) -> Option<&str> {
    if authority.is_empty()
        || authority.contains('/')
        || authority.contains('?')
        || authority.contains('#')
        || authority.contains('@')
    {
        return None;
    }

    if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, suffix) = bracketed.split_once(']')?;
        return valid_origin_port(suffix).then_some(host);
    }

    match authority.split_once(':') {
        Some((host, port)) if !host.is_empty() && valid_origin_port_suffix(port) => Some(host),
        None if !authority.is_empty() => Some(authority),
        _ => None,
    }
}

fn valid_origin_port(suffix: &str) -> bool {
    suffix.is_empty()
        || suffix
            .strip_prefix(':')
            .is_some_and(valid_origin_port_suffix)
}

fn valid_origin_port_suffix(port: &str) -> bool {
    !port.is_empty()
        && port.bytes().all(|byte| byte.is_ascii_digit())
        && port.parse::<u16>().is_ok()
}

/// Beendet eine Session nur für ihren Eigentümer (F-047 / A3).
///
/// Existenz-Leckage vermeiden: „unbekannt“, „abgelaufen“ und „gehört einem
/// anderen Principal“ liefern dieselbe Antwort (404 ohne Body). Deshalb wird
/// das Eigentum **vor** dem vom Client gelieferten Protokoll-Header geprüft;
/// sonst verriete ein 400 bei falscher Version, dass die fremde Session
/// existiert.
async fn delete(
    request: Request<Incoming>,
    state: Arc<Mutex<McpSessionRegistry>>,
    principal_key: &str,
) -> Result<McpResponse, Infallible> {
    let id = request
        .headers()
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok());
    let protocol = request
        .headers()
        .get(PROTOCOL_HEADER)
        .and_then(|value| value.to_str().ok());
    let (Some(id), Some(protocol)) = (id, protocol) else {
        return Ok(empty_response(StatusCode::BAD_REQUEST));
    };
    let mut sessions = state.lock().await;
    // Nachschlagen mit der serverseitigen Version: Jede Session wird mit
    // `VERSION` angelegt, ein Fehler hier bedeutet also „nicht vorhanden“.
    let session = match sessions.require(id, VERSION, jiff::Timestamp::now()) {
        Ok(session) if session.principal_key == principal_key => session,
        Ok(_) | Err(_) => return Ok(empty_response(StatusCode::NOT_FOUND)),
    };
    if protocol != session.protocol_version {
        return Ok(session_error_response(&McpServerError::ProtocolMismatch {
            expected: session.protocol_version,
            actual: protocol.to_owned(),
        }));
    }
    match sessions.remove(id) {
        Ok(()) => Ok(empty_response(StatusCode::NO_CONTENT)),
        Err(_) => Ok(empty_response(StatusCode::NOT_FOUND)),
    }
}

async fn post(
    request: Request<Incoming>,
    state: Arc<Mutex<McpSessionRegistry>>,
    principal_key: String,
    principals: Arc<PrincipalRegistry>,
    supervisor: Option<Arc<dyn McpSupervisor>>,
    event_bus: Option<Arc<McpEventBus>>,
) -> Result<McpResponse, Infallible> {
    let session_id = request
        .headers()
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let protocol = request
        .headers()
        .get(PROTOCOL_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let accepts = request
        .headers()
        .get(ACCEPT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !(accepts.contains("application/json") && accepts.contains("text/event-stream")) {
        return Ok(response(StatusCode::NOT_ACCEPTABLE, Value::Null, None));
    }
    if !request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"))
    {
        return Ok(response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Value::Null,
            None,
        ));
    }
    // Früher Abbruch, wenn `Content-Length` das Limit bereits überschreitet.
    // Bei `Transfer-Encoding: chunked` gibt es keine Obergrenze im
    // `size_hint`; dort greift erst `Limited` unten (F-046).
    if request
        .body()
        .size_hint()
        .upper()
        .is_some_and(|size| size > MAX_BODY_BYTES as u64)
    {
        return Ok(response(StatusCode::PAYLOAD_TOO_LARGE, Value::Null, None));
    }
    let bytes = match read_limited_body(request.into_body()).await {
        Ok(bytes) => bytes,
        Err(status) => return Ok(response(status, Value::Null, None)),
    };
    let message: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return Ok(response(StatusCode::BAD_REQUEST, Value::Null, None)),
    };
    let id = message.get("id").cloned();
    let method = message.get("method").and_then(Value::as_str);
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || method.is_none() {
        return Ok(json_rpc_error(id, -32600, "invalid JSON-RPC request", None));
    }
    if method == Some("initialize") {
        if session_id.is_some() || protocol.is_some() {
            return Ok(empty_response(StatusCode::BAD_REQUEST));
        }
        if id.as_ref().is_none_or(Value::is_null) {
            return Ok(json_rpc_error(
                id,
                -32600,
                "initialize must be a JSON-RPC request with an id",
                None,
            ));
        }
        if message
            .pointer("/params/protocolVersion")
            .and_then(Value::as_str)
            != Some(VERSION)
        {
            return Ok(json_rpc_error(
                id,
                -32602,
                "unsupported or missing MCP protocol version",
                None,
            ));
        }
        let client_name = message
            .pointer("/params/clientInfo/name")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let session_id = format!("harw-{}", harw_types::WorkId::new());
        let session = match state.lock().await.initialize(
            session_id,
            client_name,
            principal_key,
            jiff::Timestamp::now(),
        ) {
            Ok(session) => session,
            Err(error) => return Ok(json_rpc_error(id, -32000, &error.to_string(), None)),
        };
        return Ok(json_rpc_result(
            id,
            json!({"protocolVersion": VERSION, "capabilities": {"tools": {"listChanged": false}}, "serverInfo": {"name": "harwness", "version": "0.1.0"}}),
            Some(session.id),
        ));
    }
    let notification = method == Some("notifications/initialized");
    if notification && id.is_some() {
        return Ok(json_rpc_error(
            id,
            -32600,
            "notifications/initialized must not include an id",
            None,
        ));
    }
    let session = request_session(
        session_id.as_deref(),
        protocol.as_deref(),
        notification,
        &state,
    )
    .await;
    let session = match session {
        Ok(session) if session.principal_key == principal_key => session,
        Ok(_) => {
            return Ok(session_error_response(
                &McpServerError::SessionPrincipalMismatch,
            ));
        }
        Err(error) => return Ok(session_error_response(&error)),
    };
    if notification {
        if let (Some(id), Some(protocol)) = (session_id.as_deref(), protocol.as_deref()) {
            if let Err(error) =
                state
                    .lock()
                    .await
                    .mark_initialized(id, protocol, jiff::Timestamp::now())
            {
                return Ok(session_error_response(&error));
            }
        }
        return Ok(empty_response(StatusCode::ACCEPTED));
    }
    let principal = principals.get(&principal_key);
    if method == Some("tools/list") {
        return Ok(json_rpc_result(
            id,
            json!({"tools": tool_catalog(principal)}),
            None,
        ));
    }
    if method == Some("tools/call") {
        return Ok(tools_call(
            id,
            &message,
            session.id,
            principal,
            supervisor.as_deref(),
            event_bus.as_deref(),
        )
        .await);
    }
    Ok(json_rpc_error(id, -32601, "method not found", None))
}

/// Sammelt den Request-Body höchstens bis `MAX_BODY_BYTES`. `Limited` zählt die
/// tatsächlich gelesenen Datenframes und bricht ab, sobald das Limit
/// überschritten würde; unabhängig davon, ob `Content-Length` oder chunked
/// übertragen wird. So wächst der Puffer nie über das Limit hinaus.
async fn read_limited_body<B>(body: B) -> Result<Bytes, StatusCode>
where
    B: Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    match Limited::new(body, MAX_BODY_BYTES).collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(error) if error.downcast_ref::<LengthLimitError>().is_some() => {
            Err(StatusCode::PAYLOAD_TOO_LARGE)
        }
        Err(_) => Err(StatusCode::BAD_REQUEST),
    }
}

/// Opens the server-to-client stream for an initialized MCP session.  The
/// stream is deliberately backed by the bounded per-session event bus; a slow
/// client receives an explicit lag event rather than unbounded buffering.
async fn get(
    request: Request<Incoming>,
    state: Arc<Mutex<McpSessionRegistry>>,
    principal_key: String,
    event_bus: Option<Arc<McpEventBus>>,
) -> Result<McpResponse, Infallible> {
    let accepts = request
        .headers()
        .get(ACCEPT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !accepts
        .split(',')
        .map(str::trim)
        .map(|value| value.split(';').next().unwrap_or(value).trim())
        .any(|value| value == "text/event-stream")
    {
        return Ok(empty_response(StatusCode::NOT_ACCEPTABLE));
    }
    let session_id = request
        .headers()
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok());
    let protocol = request
        .headers()
        .get(PROTOCOL_HEADER)
        .and_then(|value| value.to_str().ok());
    let (Some(session_id), Some(protocol)) = (session_id, protocol) else {
        return Ok(empty_response(StatusCode::BAD_REQUEST));
    };
    let session = {
        let mut registry = state.lock().await;
        match registry.require_initialized(session_id, protocol, jiff::Timestamp::now()) {
            Ok(session) => session,
            Err(error) => return Ok(session_error_response(&error)),
        }
    };
    if session.principal_key != principal_key {
        return Ok(session_error_response(
            &McpServerError::SessionPrincipalMismatch,
        ));
    }
    let Some(event_bus) = event_bus else {
        return Ok(empty_response(StatusCode::NOT_FOUND));
    };
    let mut subscription = match event_bus.subscribe(session_id) {
        Ok(subscription) => subscription,
        Err(_) => return Ok(empty_response(StatusCode::NOT_FOUND)),
    };
    let (mut sender, body) = Channel::<Bytes, Infallible>::new(8);
    tokio::spawn(async move {
        loop {
            let frame = match subscription.recv().await {
                Ok(event) => sse_frame(
                    "lifecycle",
                    &serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_owned()),
                ),
                Err(McpEventReceiveError::Lagged { skipped }) => {
                    sse_frame("lagged", &json!({"skipped": skipped}).to_string())
                }
                Err(McpEventReceiveError::Closed) => break,
                Err(McpEventReceiveError::Empty) => continue,
            };
            if sender.send_data(frame).await.is_err() {
                break;
            }
        }
    });
    let mut response = Response::new(body.boxed());
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response.headers_mut().insert(
        hyper::header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    Ok(response)
}

/// Baut einen SSE-Frame mit echten Zeilenumbrüchen (`\n`). Früher standen im
/// Rust-Literal `\\n`, also Backslash + `n`; kein SSE-Client hat so je ein
/// Event dispatcht (F-046). `data` muss einzeilig sein: kompaktes
/// `serde_json` maskiert Zeilenumbrüche in Strings, daher gilt das hier.
fn sse_frame(event: &str, data: &str) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

/// Tool descriptors filtered to the capabilities the caller's resolved
/// principal actually holds. An unresolved `principal_key` (authenticated
/// but not composed into the registry) sees an empty catalog.
fn tool_catalog(principal: Option<&McpPrincipal>) -> Value {
    let Some(principal) = principal else {
        return json!([]);
    };
    let mut tools = Vec::new();
    if principal
        .capabilities()
        .contains(&McpJobCapability::SubmitOwn)
    {
        tools.push(json!({
            "name": "harw_job_submit",
            "description": "Submit a durable job in the authenticated principal's workspace.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "enum": ["worker", "dream"]},
                    "input": {"type": "object"},
                    "idempotency_key": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 128,
                        "pattern": "^[A-Za-z0-9._:-]+$"
                    },
                    "budget": {
                        "type": "object",
                        "properties": {
                            "max_tokens": {"type": "integer", "minimum": 1},
                            "max_wall_seconds": {"type": "integer", "minimum": 1},
                            "max_tool_calls": {"type": "integer", "minimum": 1}
                        },
                        "additionalProperties": false
                    }
                },
                "required": ["kind", "input"],
                "additionalProperties": false
            }
        }));
    }
    if principal.capabilities().iter().any(|c| {
        matches!(
            c,
            McpJobCapability::ReadOwn | McpJobCapability::ReadWorkspace
        )
    }) {
        tools.push(json!({
            "name": "harw_job_status",
            "description": "Fetch a redacted durable job status by work ID.",
            "inputSchema": {
                "type": "object",
                "properties": {"work_id": {"type": "string"}},
                "required": ["work_id"],
                "additionalProperties": false
            }
        }));
    }
    if principal.capabilities().iter().any(|c| {
        matches!(
            c,
            McpJobCapability::CancelOwn | McpJobCapability::CancelWorkspace
        )
    }) {
        tools.push(json!({
            "name": "harw_job_cancel",
            "description": "Cancel one authorized durable job; never accepts lease tokens.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "work_id": {"type": "string"},
                    "reason": {"type": "string"}
                },
                "required": ["work_id"],
                "additionalProperties": false
            }
        }));
    }
    Value::Array(tools)
}

/// Executes `tools/call`. Tool identity, session id, and principal are all
/// server-resolved; only tool-specific arguments come from the JSON-RPC
/// request and are validated before use.
async fn tools_call(
    id: Option<Value>,
    message: &Value,
    session_id: String,
    principal: Option<&McpPrincipal>,
    supervisor: Option<&dyn McpSupervisor>,
    event_bus: Option<&McpEventBus>,
) -> McpResponse {
    let Some(principal) = principal else {
        return json_rpc_error(id, -32001, "principal is not authorized for any tool", None);
    };
    let tool_name = message.pointer("/params/name").and_then(Value::as_str);
    let Some(supervisor) = supervisor else {
        return json_rpc_error(id, -32601, "no durable job supervisor is configured", None);
    };
    let context = McpRequestContext::from_trusted_ingress(session_id, principal.clone());
    match tool_name {
        Some("harw_job_submit") => {
            let submission = match parse_submit_arguments(message) {
                Ok(submission) => submission,
                Err(message) => return json_rpc_error(id, -32602, message, None),
            };
            match supervisor.submit_job(&context, submission).await {
                Ok(status) => {
                    publish_lifecycle_event(
                        event_bus,
                        context.session_id(),
                        McpLifecycleEventKind::JobUpdated {
                            work_id: status.work_id.clone(),
                            state: status.state,
                            revision: status.revision,
                        },
                    );
                    json_rpc_result(
                        id,
                        json!({"content": [{"type": "text", "text": serde_json::to_string(&status).unwrap_or_default()}]}),
                        None,
                    )
                }
                Err(error) => supervisor_error_response(id, &error),
            }
        }
        Some("harw_job_status") => {
            let Some(work_id) = work_id_argument(message) else {
                return json_rpc_error(id, -32602, "missing or invalid 'work_id' argument", None);
            };
            match supervisor
                .job_status(&context, harw_types::WorkId::from_str(work_id))
                .await
            {
                Ok(status) => json_rpc_result(
                    id,
                    json!({"content": [{"type": "text", "text": serde_json::to_string(&status).unwrap_or_default()}]}),
                    None,
                ),
                Err(error) => supervisor_error_response(id, &error),
            }
        }
        Some("harw_job_cancel") => {
            let Some(work_id) = work_id_argument(message) else {
                return json_rpc_error(id, -32602, "missing or invalid 'work_id' argument", None);
            };
            let reason = message
                .pointer("/params/arguments/reason")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            match supervisor
                .cancel_job(&context, harw_types::WorkId::from_str(work_id), reason)
                .await
            {
                Ok(receipt) => {
                    publish_lifecycle_event(
                        event_bus,
                        context.session_id(),
                        McpLifecycleEventKind::JobCancellationRequested {
                            work_id: receipt.work_id.clone(),
                            revision: receipt.revision,
                        },
                    );
                    publish_lifecycle_event(
                        event_bus,
                        context.session_id(),
                        McpLifecycleEventKind::JobUpdated {
                            work_id: receipt.work_id.clone(),
                            state: harw_job_core::JobState::Cancelled,
                            revision: receipt.revision,
                        },
                    );
                    json_rpc_result(
                        id,
                        json!({"content": [{"type": "text", "text": serde_json::to_string(&receipt).unwrap_or_default()}]}),
                        None,
                    )
                }
                Err(error) => supervisor_error_response(id, &error),
            }
        }
        _ => json_rpc_error(id, -32601, "unknown tool", None),
    }
}

/// Lifecycle delivery is best effort: callers must never see a successful
/// durable operation fail because no SSE subscriber is currently connected.
/// Event kinds carry only redacted identifiers, state, and revision metadata.
fn publish_lifecycle_event(
    event_bus: Option<&McpEventBus>,
    session_id: &str,
    kind: McpLifecycleEventKind,
) {
    if let Some(event_bus) = event_bus {
        let _ = event_bus.publish(session_id, jiff::Timestamp::now(), kind);
    }
}

fn work_id_argument(message: &Value) -> Option<&str> {
    message
        .pointer("/params/arguments/work_id")
        .and_then(Value::as_str)
}

fn parse_submit_arguments(message: &Value) -> Result<McpJobSubmission, &'static str> {
    let Some(arguments) = message
        .pointer("/params/arguments")
        .and_then(Value::as_object)
    else {
        return Err("invalid harw_job_submit arguments: expected an object");
    };
    if arguments.len() < 2
        || arguments.len() > 4
        || !arguments.contains_key("kind")
        || !arguments.contains_key("input")
        || arguments.keys().any(|key| {
            !matches!(
                key.as_str(),
                "kind" | "input" | "idempotency_key" | "budget"
            )
        })
    {
        return Err(
            "invalid harw_job_submit arguments: expected only 'kind', 'input', and optional 'idempotency_key', 'budget'",
        );
    }
    let kind = match arguments.get("kind").and_then(Value::as_str) {
        Some("worker") => McpSubmittedJobKind::Worker,
        Some("dream") => McpSubmittedJobKind::Dream,
        _ => return Err("invalid harw_job_submit arguments: 'kind' must be 'worker' or 'dream'"),
    };
    let input = arguments
        .get("input")
        .filter(|input| input.as_object().is_some_and(|input| !input.is_empty()))
        .cloned()
        .ok_or("invalid harw_job_submit arguments: 'input' must be a non-empty object")?;
    // Grammatik- und Eindeutigkeitsprüfung des Schlüssels liegt bei
    // `McpIdempotencyKey::parse` im Supervisor; hier wird nur der Wire-Typ
    // durchgesetzt (fail closed statt eine Nicht-Zeichenkette stillschweigend
    // zu ignorieren).
    let idempotency_key = match arguments.get("idempotency_key") {
        None => None,
        Some(Value::String(key)) => Some(key.clone()),
        Some(_) => {
            return Err("invalid harw_job_submit arguments: 'idempotency_key' must be a string");
        }
    };
    let budget = arguments
        .get("budget")
        .map(parse_submit_budget)
        .transpose()?;
    Ok(McpJobSubmission {
        kind,
        input,
        idempotency_key,
        budget,
    })
}

fn parse_submit_budget(value: &Value) -> Result<harw_job_core::Budget, &'static str> {
    let Some(budget) = value.as_object() else {
        return Err("invalid harw_job_submit arguments: 'budget' must be an object");
    };
    if budget.is_empty() {
        return Err("invalid harw_job_submit arguments: 'budget' must contain at least one limit");
    }
    if budget.keys().any(|key| {
        !matches!(
            key.as_str(),
            "max_tokens" | "max_wall_seconds" | "max_tool_calls"
        )
    }) {
        return Err("invalid harw_job_submit arguments: budget contains an unknown field");
    }
    let max_tokens = optional_positive_u64(budget.get("max_tokens"), "max_tokens")?;
    if max_tokens == Some(u64::MAX) {
        return Err(
            "invalid harw_job_submit arguments: 'max_tokens' must be a finite positive integer",
        );
    }
    let max_wall_seconds =
        optional_positive_u64(budget.get("max_wall_seconds"), "max_wall_seconds")?;
    let max_wall = max_wall_seconds
        .map(|seconds| {
            i64::try_from(seconds)
                .map(SignedDuration::from_secs)
                .map_err(|_| "invalid harw_job_submit arguments: 'max_wall_seconds' is too large")
        })
        .transpose()?;
    let max_tool_calls = optional_positive_u64(budget.get("max_tool_calls"), "max_tool_calls")?;
    if max_tool_calls == Some(u64::from(u32::MAX)) {
        return Err(
            "invalid harw_job_submit arguments: 'max_tool_calls' must be a finite positive integer",
        );
    }
    let max_tool_calls = max_tool_calls
        .map(|calls| {
            u32::try_from(calls)
                .map_err(|_| "invalid harw_job_submit arguments: 'max_tool_calls' is too large")
        })
        .transpose()?;
    Ok(harw_job_core::Budget {
        max_tokens,
        max_wall,
        max_tool_calls,
    })
}

fn optional_positive_u64(
    value: Option<&Value>,
    field: &'static str,
) -> Result<Option<u64>, &'static str> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(value) = value.as_u64().filter(|value| *value > 0) else {
        return Err(match field {
            "max_tokens" => {
                "invalid harw_job_submit arguments: 'max_tokens' must be a positive integer"
            }
            "max_wall_seconds" => {
                "invalid harw_job_submit arguments: 'max_wall_seconds' must be a positive integer"
            }
            "max_tool_calls" => {
                "invalid harw_job_submit arguments: 'max_tool_calls' must be a positive integer"
            }
            _ => "invalid harw_job_submit budget",
        });
    };
    Ok(Some(value))
}

/// Store-Fehler gehen nur generisch an den Client (R12): Texte wie
/// `symlink_error` oder `CorruptJob.detail` enthalten absolute Pfade bzw.
/// interne Details. Das vollständige Detail landet nur im Server-Log (stderr).
fn supervisor_error_response(id: Option<Value>, error: &McpSupervisorError) -> McpResponse {
    match error {
        McpSupervisorError::NotAuthorized => json_rpc_error(id, -32001, &error.to_string(), None),
        McpSupervisorError::InvalidSubmission(_)
        | McpSupervisorError::InvalidIdempotencyKey { .. }
        | McpSupervisorError::IdempotencyConflict { .. } => {
            json_rpc_error(id, -32602, &error.to_string(), None)
        }
        McpSupervisorError::RateLimited { .. } => {
            json_rpc_error(id, -32003, &error.to_string(), None)
        }
        McpSupervisorError::JobStore(store_error) => {
            eprintln!("harw-mcp-server: {error}");
            json_rpc_error(id, -32000, redacted_store_error_message(store_error), None)
        }
        McpSupervisorError::LimiterUnavailable => {
            eprintln!("harw-mcp-server: {error}");
            json_rpc_error(id, -32000, &error.to_string(), None)
        }
    }
}

fn redacted_store_error_message(error: &SessionStoreError) -> &'static str {
    match error {
        SessionStoreError::JobNotFound { .. } => "job was not found",
        _ => "durable job operation failed",
    }
}

async fn request_session(
    id: Option<&str>,
    protocol: Option<&str>,
    allow_uninitialized: bool,
    state: &Arc<Mutex<McpSessionRegistry>>,
) -> Result<crate::session::McpSession, McpServerError> {
    let (Some(id), Some(protocol)) = (id, protocol) else {
        return Err(McpServerError::InvalidSessionId);
    };
    let mut registry = state.lock().await;
    if allow_uninitialized {
        registry.require(id, protocol, jiff::Timestamp::now())
    } else {
        registry.require_initialized(id, protocol, jiff::Timestamp::now())
    }
}

fn session_error_response(error: &McpServerError) -> McpResponse {
    let status = match error {
        McpServerError::SessionUnknown => StatusCode::NOT_FOUND,
        McpServerError::InvalidSessionId
        | McpServerError::ProtocolMismatch { .. }
        | McpServerError::SessionNotInitialized
        | McpServerError::SessionPrincipalMismatch => StatusCode::BAD_REQUEST,
        McpServerError::SessionCapacityExceeded => StatusCode::SERVICE_UNAVAILABLE,
    };
    empty_response(status)
}

fn method_not_allowed() -> McpResponse {
    let mut response = empty_response(StatusCode::METHOD_NOT_ALLOWED);
    response
        .headers_mut()
        .insert(ALLOW, HeaderValue::from_static("GET, POST, DELETE"));
    response
}

fn empty_response(status: StatusCode) -> McpResponse {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::new()).boxed())
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()).boxed()))
}
fn response(status: StatusCode, body: Value, session: Option<String>) -> McpResponse {
    let mut response = Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body.to_string())).boxed())
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()).boxed()));
    if let Some(session) = session {
        if let Ok(value) = HeaderValue::from_str(&session) {
            response.headers_mut().insert(SESSION_HEADER, value);
        }
    }
    response
}
fn json_rpc_result(id: Option<Value>, result: Value, session: Option<String>) -> McpResponse {
    response(
        StatusCode::OK,
        json!({"jsonrpc":"2.0","id":id,"result":result}),
        session,
    )
}
fn json_rpc_error(
    id: Option<Value>,
    code: i64,
    message: &str,
    session: Option<String>,
) -> McpResponse {
    response(
        StatusCode::OK,
        json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}),
        session,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use harw_job_core::{Budget, Job, JobKind, JobScope, JobState, RetryPolicy};
    use harw_session_store::JobStore;
    use harw_types::ApprovalActor;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    use crate::supervisor::DurableMcpSupervisor;
    use crate::test_support::{TestError, TestResult, ctx};

    fn post_request(body: &Value, extra_headers: &str) -> String {
        post_request_as("test-token", body, extra_headers)
    }

    fn post_request_as(bearer: &str, body: &Value, extra_headers: &str) -> String {
        let encoded = body.to_string();
        format!(
            "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {bearer}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nConnection: close\r\n{extra_headers}Content-Length: {}\r\n\r\n{encoded}",
            encoded.len()
        )
    }

    async fn round_trip(address: SocketAddr, request: String) -> TestResult<String> {
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .map_err(ctx("listener accepts test connection"))?;
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(ctx("test request writes"))?;
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .await
            .map_err(ctx("test response reads"))?;
        Ok(response)
    }

    fn session_id(response: &str) -> TestResult<String> {
        response
            .lines()
            .find_map(|line| line.strip_prefix("mcp-session-id: "))
            .map(str::to_owned)
            .ok_or(TestError::Missing("initialize returns a session header"))
    }

    /// Reads back the `protocolVersion` the server actually advertised in an
    /// `initialize` response body, instead of assuming it matches the local
    /// `VERSION` constant (Z1-R3-04: `VERSION` and the session registry's own
    /// protocol constant used to be defined twice and could drift apart).
    fn advertised_protocol_version(response: &str) -> TestResult<String> {
        const MARKER: &str = "\"protocolVersion\":\"";
        let start = response.find(MARKER).ok_or(TestError::Missing(
            "initialize response advertises a protocol version",
        ))? + MARKER.len();
        let rest = &response[start..];
        let end = rest.find('"').ok_or(TestError::Missing(
            "advertised protocol version is a quoted string",
        ))?;
        Ok(rest[..end].to_owned())
    }

    /// Schreibt den Request und liest bis EOF; ein Reset nach der Antwort
    /// (Server schließt mit ungelesenem Rest-Body) beendet das Lesen, ohne
    /// bereits Gelesenes zu verwerfen.
    async fn round_trip_bytes_lenient(address: SocketAddr, request: &[u8]) -> TestResult<String> {
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .map_err(ctx("listener accepts test connection"))?;
        // Schreibfehler sind hier erwartbar: Der Server darf nach dem 413
        // schließen, während noch Rest-Body unterwegs ist.
        let _ = stream.write_all(request).await;
        let mut response = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => response.extend_from_slice(&buffer[..read]),
            }
        }
        Ok(String::from_utf8_lossy(&response).into_owned())
    }

    /// Kodiert `body` als HTTP/1.1-chunked-Request mit den angegebenen
    /// Chunk-Größen (Summe muss `body.len()` ergeben).
    fn chunked_post_request(bearer: &str, body: &[u8], chunk_sizes: &[usize]) -> Vec<u8> {
        let mut request = format!(
            "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {bearer}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n"
        )
        .into_bytes();
        let mut offset = 0;
        for size in chunk_sizes {
            request.extend_from_slice(format!("{size:X}\r\n").as_bytes());
            request.extend_from_slice(&body[offset..offset + size]);
            request.extend_from_slice(b"\r\n");
            offset += size;
        }
        assert_eq!(offset, body.len(), "chunk sizes cover the whole body");
        request.extend_from_slice(b"0\r\n\r\n");
        request
    }

    /// Liest genau einen HTTP/1.1-Chunk (Größenzeile, Nutzdaten, CRLF).
    async fn read_http_chunk<R>(reader: &mut R) -> TestResult<Vec<u8>>
    where
        R: tokio::io::AsyncBufRead + Unpin,
    {
        let mut size_line = Vec::new();
        reader
            .read_until(b'\n', &mut size_line)
            .await
            .map_err(ctx("chunk size line reads"))?;
        let size_text = String::from_utf8(size_line).map_err(ctx("chunk size is ASCII"))?;
        let size =
            usize::from_str_radix(size_text.trim_end(), 16).map_err(ctx("chunk size is hex"))?;
        let mut payload = vec![0_u8; size];
        reader
            .read_exact(&mut payload)
            .await
            .map_err(ctx("chunk payload reads"))?;
        let mut crlf = [0_u8; 2];
        reader
            .read_exact(&mut crlf)
            .await
            .map_err(ctx("chunk trailer reads"))?;
        assert_eq!(&crlf, b"\r\n");
        Ok(payload)
    }

    /// Entfernt den `date`-Header, damit zwei Antworten byteweise vergleichbar sind.
    fn without_date_header(response: &str) -> String {
        response
            .split("\r\n")
            .filter(|line| !line.to_ascii_lowercase().starts_with("date:"))
            .collect::<Vec<_>>()
            .join("\r\n")
    }

    /// Opens and initializes a session, returning its id together with the
    /// protocol version the server actually advertised in the `initialize`
    /// response (see `advertised_protocol_version`).
    async fn open_initialized_session(
        address: SocketAddr,
        bearer: &str,
    ) -> TestResult<(String, String)> {
        let initialize = round_trip(
            address,
            post_request_as(
                bearer,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {"protocolVersion": VERSION}
                }),
                "",
            ),
        )
        .await?;
        assert!(initialize.starts_with("HTTP/1.1 200"), "{initialize}");
        let id = session_id(&initialize)?;
        let advertised = advertised_protocol_version(&initialize)?;
        let initialized = round_trip(
            address,
            post_request_as(
                bearer,
                &json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {advertised}\r\n"),
            ),
        )
        .await?;
        assert!(initialized.starts_with("HTTP/1.1 202"), "{initialized}");
        Ok((id, advertised))
    }

    fn delete_request(bearer: &str, session: &str, protocol: &str) -> String {
        format!(
            "DELETE /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {bearer}\r\nMcp-Session-Id: {session}\r\nMcp-Protocol-Version: {protocol}\r\nConnection: close\r\n\r\n"
        )
    }

    fn test_principal(actor: &str, capabilities: Vec<McpJobCapability>) -> McpPrincipal {
        McpPrincipal::from_trusted_ingress(
            ApprovalActor::Operator {
                id: actor.to_owned(),
            },
            harw_types::TenantId::from_str("test-tenant"),
            harw_types::WorkspaceId::from_str("test-workspace"),
            capabilities,
        )
    }

    #[test]
    fn origin_validation_accepts_native_and_loopback_clients() -> TestResult {
        assert!(origin_is_loopback(&hyper::HeaderMap::new()));

        for origin in [
            "http://localhost",
            "https://LOCALHOST:8443",
            "http://127.0.0.1:3000",
            "https://[::1]",
            "https://[::1]:8443",
        ] {
            let mut headers = hyper::HeaderMap::new();
            headers.insert(
                ORIGIN,
                HeaderValue::from_str(origin).map_err(ctx("valid Origin header value"))?,
            );
            assert!(origin_is_loopback(&headers), "{origin}");
        }
        Ok(())
    }

    #[test]
    fn origin_validation_rejects_malformed_or_deceptive_authorities() -> TestResult {
        for origin in [
            "http://[::1]untrusted.example",
            "http://[::1].untrusted.example",
            "http://[::1]:8443untrusted",
            "http://[::1]:",
            "http://[::1]:65536",
            "http://localhost/attacker",
            "http://localhost:8443/attacker",
            "http://localhost@untrusted.example",
            "http://127.0.0.1:bad",
            "http://::1",
            "ftp://127.0.0.1",
        ] {
            let mut headers = hyper::HeaderMap::new();
            headers.insert(
                ORIGIN,
                HeaderValue::from_str(origin).map_err(ctx("valid Origin header value"))?,
            );
            assert!(!origin_is_loopback(&headers), "{origin}");
        }
        Ok(())
    }

    #[test]
    fn submit_arguments_reject_empty_or_unsafe_payloads() {
        let cases = [
            (
                json!({"kind": "worker", "input": {}}),
                "invalid harw_job_submit arguments: 'input' must be a non-empty object",
            ),
            (
                json!({"kind": "worker", "input": {"task": "review"}, "budget": {}}),
                "invalid harw_job_submit arguments: 'budget' must contain at least one limit",
            ),
            (
                json!({
                    "kind": "worker",
                    "input": {"task": "review"},
                    "budget": {"max_tokens": u64::MAX}
                }),
                "invalid harw_job_submit arguments: 'max_tokens' must be a finite positive integer",
            ),
            (
                json!({
                    "kind": "worker",
                    "input": {"task": "review"},
                    "budget": {"max_tool_calls": u32::MAX}
                }),
                "invalid harw_job_submit arguments: 'max_tool_calls' must be a finite positive integer",
            ),
            (
                json!({
                    "kind": "worker",
                    "input": {"task": "review"},
                    "budget": {"max_wall_seconds": u64::MAX}
                }),
                "invalid harw_job_submit arguments: 'max_wall_seconds' is too large",
            ),
            (
                json!({
                    "kind": "worker",
                    "input": {"task": "review"},
                    "budget": {"max_tokens": 1.5}
                }),
                "invalid harw_job_submit arguments: 'max_tokens' must be a positive integer",
            ),
        ];

        for (arguments, expected_error) in cases {
            let message = json!({"params": {"arguments": arguments}});
            assert_eq!(parse_submit_arguments(&message), Err(expected_error));
        }
    }

    #[tokio::test]
    async fn streamable_http_session_lifecycle_is_enforced_over_tcp() -> TestResult {
        let listener = BoundMcpListener::bind_authenticated(
            McpListenerConfig {
                address: "127.0.0.1:0"
                    .parse()
                    .map_err(ctx("valid loopback address"))?,
                max_sessions: 4,
                session_ttl: SignedDuration::from_secs(60),
            },
            Arc::new(StaticBearerAuthenticator::new(vec![(
                "test-principal".to_owned(),
                b"test-token".to_vec(),
            )])),
        )
        .await
        .map_err(ctx("loopback listener binds"))?;
        let address = listener
            .local_addr()
            .map_err(ctx("bound listener has address"))?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server = tokio::spawn(listener.serve_until(shutdown_rx));

        let initialize = round_trip(
            address,
            post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": VERSION,
                        "clientInfo": {"name": "contract-test"}
                    }
                }),
                "",
            ),
        )
        .await?;
        assert!(initialize.starts_with("HTTP/1.1 200"), "{initialize}");
        assert!(initialize.contains(&format!("\"protocolVersion\":\"{VERSION}\"")));
        let id = session_id(&initialize)?;

        let initialized = round_trip(
            address,
            post_request(
                &json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await?;
        assert!(initialized.starts_with("HTTP/1.1 202"));

        let empty_tool_catalog = round_trip(
            address,
            post_request(
                &json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await?;
        assert!(empty_tool_catalog.starts_with("HTTP/1.1 200"));
        assert!(empty_tool_catalog.contains("\"tools\":[]"));

        let missing_version = round_trip(
            address,
            format!(
                "DELETE /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-token\r\nMcp-Session-Id: {id}\r\nConnection: close\r\n\r\n"
            ),
        )
        .await?;
        assert!(missing_version.starts_with("HTTP/1.1 400"));

        let deleted = round_trip(
            address,
            format!(
                "DELETE /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-token\r\nMcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\nConnection: close\r\n\r\n"
            ),
        )
        .await?;
        assert!(deleted.starts_with("HTTP/1.1 204"));

        let stale = round_trip(
            address,
            post_request(
                &json!({"jsonrpc":"2.0", "id":3, "method":"tools/list"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await?;
        assert!(stale.starts_with("HTTP/1.1 404"));

        let foreign_origin = round_trip(
            address,
            post_request(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "initialize",
                    "params": {"protocolVersion": VERSION}
                }),
                "Origin: https://untrusted.example\r\n",
            ),
        )
        .await?;
        assert!(foreign_origin.starts_with("HTTP/1.1 403"));

        shutdown_tx
            .send(true)
            .map_err(ctx("server accepts shutdown"))?;
        server
            .await
            .map_err(ctx("server task joins"))?
            .map_err(ctx("server shuts down cleanly"))?;
        Ok(())
    }

    #[tokio::test]
    async fn session_rejects_requests_authenticated_as_a_different_principal() -> TestResult {
        let listener = BoundMcpListener::bind_authenticated(
            McpListenerConfig {
                address: "127.0.0.1:0"
                    .parse()
                    .map_err(ctx("valid loopback address"))?,
                max_sessions: 4,
                session_ttl: SignedDuration::from_secs(60),
            },
            Arc::new(StaticBearerAuthenticator::new(vec![
                ("principal-a".to_owned(), b"token-a".to_vec()),
                ("principal-b".to_owned(), b"token-b".to_vec()),
            ])),
        )
        .await
        .map_err(ctx("loopback listener binds"))?;
        let address = listener
            .local_addr()
            .map_err(ctx("bound listener has address"))?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server = tokio::spawn(listener.serve_until(shutdown_rx));

        let initialize = round_trip(
            address,
            post_request_as(
                "token-a",
                &json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": VERSION,
                        "clientInfo": {"name": "cross-principal-test"}
                    }
                }),
                "",
            ),
        )
        .await?;
        assert!(initialize.starts_with("HTTP/1.1 200"), "{initialize}");
        let id = session_id(&initialize)?;

        let initialized = round_trip(
            address,
            post_request_as(
                "token-a",
                &json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await?;
        assert!(initialized.starts_with("HTTP/1.1 202"));

        // A follow-up request bearing a *different but valid* principal's
        // credential must not be able to use principal A's session.
        let cross_principal = round_trip(
            address,
            post_request_as(
                "token-b",
                &json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await?;
        assert!(
            cross_principal.starts_with("HTTP/1.1 400"),
            "{cross_principal}"
        );

        // The original principal's session remains valid and unaffected.
        let same_principal = round_trip(
            address,
            post_request_as(
                "token-a",
                &json!({"jsonrpc":"2.0", "id":3, "method":"tools/list"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await?;
        assert!(
            same_principal.starts_with("HTTP/1.1 200"),
            "{same_principal}"
        );

        shutdown_tx
            .send(true)
            .map_err(ctx("server accepts shutdown"))?;
        server
            .await
            .map_err(ctx("server task joins"))?
            .map_err(ctx("server shuts down cleanly"))?;
        Ok(())
    }

    #[tokio::test]
    async fn initialized_get_streams_bounded_lifecycle_events() -> TestResult {
        let event_bus = Arc::new(crate::events::McpEventBus::new(2).map_err(ctx("event bus"))?);
        let listener = BoundMcpListener::bind_with_supervisor_and_events(
            McpListenerConfig {
                address: "127.0.0.1:0"
                    .parse()
                    .map_err(ctx("valid loopback address"))?,
                max_sessions: 4,
                session_ttl: SignedDuration::from_secs(60),
            },
            Arc::new(StaticBearerAuthenticator::new(vec![(
                "test-principal".to_owned(),
                b"test-token".to_vec(),
            )])),
            PrincipalRegistry::new(),
            None,
            Some(Arc::clone(&event_bus)),
        )
        .await
        .map_err(ctx("loopback listener binds"))?;
        let address = listener
            .local_addr()
            .map_err(ctx("bound listener has address"))?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server = tokio::spawn(listener.serve_until(shutdown_rx));

        let initialize = round_trip(
            address,
            post_request(
                &json!({
                    "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {"protocolVersion": VERSION}
                }),
                "",
            ),
        )
        .await?;
        let id = session_id(&initialize)?;
        let initialized = round_trip(
            address,
            post_request(
                &json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await?;
        assert!(initialized.starts_with("HTTP/1.1 202"));

        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .map_err(ctx("GET connects"))?;
        stream
            .write_all(format!(
                "GET /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-token\r\nAccept: text/event-stream\r\nMcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n\r\n"
            ).as_bytes())
            .await
            .map_err(ctx("GET writes"))?;
        let mut reader = BufReader::new(stream);
        let mut headers = Vec::new();
        loop {
            let mut line = Vec::new();
            reader
                .read_until(b'\n', &mut line)
                .await
                .map_err(ctx("headers read"))?;
            headers.extend_from_slice(&line);
            if headers.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let headers = String::from_utf8_lossy(&headers);
        assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
        assert!(headers.to_ascii_lowercase().contains("text/event-stream"));

        let event = event_bus
            .publish(
                &id,
                jiff::Timestamp::now(),
                crate::events::McpLifecycleEventKind::JobCancellationRequested {
                    work_id: harw_types::WorkId::new(),
                    revision: 7,
                },
            )
            .map_err(ctx("event is delivered to GET subscriber"))?;
        // Erwartete Bytes bewusst unabhängig von `sse_frame` gebaut: echte
        // Zeilenumbrüche, kein literales Backslash-n.
        let expected = format!(
            "event: lifecycle\ndata: {}\n\n",
            serde_json::to_string(&event).map_err(ctx("event serializes"))?
        );
        let payload = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            read_http_chunk(&mut reader),
        )
        .await
        .map_err(ctx("event arrives"))??;
        assert_eq!(
            payload,
            expected.as_bytes(),
            "{}",
            String::from_utf8_lossy(&payload)
        );
        assert!(payload.ends_with(b"\n\n"));
        assert!(!payload.windows(2).any(|pair| pair == b"\\n"));

        shutdown_tx
            .send(true)
            .map_err(ctx("server accepts shutdown"))?;
        server
            .await
            .map_err(ctx("server task joins"))?
            .map_err(ctx("server shuts down cleanly"))?;
        Ok(())
    }

    /// Minimal directory guard so a test-local `JobStore` root is removed
    /// even if an assertion panics partway through.
    struct TempJobStoreDir(std::path::PathBuf);
    impl TempJobStoreDir {
        fn new(label: &str) -> TestResult<Self> {
            let path = std::env::temp_dir().join(format!(
                "harw-mcp-server-test-{label}-{}-{}",
                std::process::id(),
                jiff::Timestamp::now().as_nanosecond()
            ));
            std::fs::create_dir_all(&path).map_err(ctx("temp job store dir creates"))?;
            Ok(Self(path))
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TempJobStoreDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn admitted_job_store(work_id: &str) -> TestResult<(TempJobStoreDir, Arc<JobStore>)> {
        let dir = TempJobStoreDir::new(work_id)?;
        let store = JobStore::new(dir.path());
        let now = jiff::Timestamp::now();
        let mut job = Job::new(
            harw_types::WorkId::from_str(work_id),
            JobKind::Worker,
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: 2,
                base_delay: SignedDuration::from_secs(1),
                factor: 2.0,
                max_delay: SignedDuration::from_secs(10),
                jitter: 0.0,
            },
            now,
        );
        job.mark_ready(now)
            .map_err(ctx("job transitions to ready"))?;
        let record = harw_job_core::StoredJob {
            job,
            scope: JobScope::new(
                harw_types::TenantId::from_str("test-tenant"),
                harw_types::WorkspaceId::from_str("test-workspace"),
                ApprovalActor::Operator {
                    id: "alice".to_owned(),
                },
            ),
            input: json!({"task": "review"}),
            submitted_at: now,
            not_before: now,
            lease: None,
            lease_epoch: 0,
            completion: None,
            cancellation: None,
            revision: 0,
            trace: None,
        };
        store.admit(&record).map_err(ctx("job admits"))?;
        Ok((dir, Arc::new(store)))
    }

    #[tokio::test]
    async fn tools_are_capability_filtered_and_authorized_end_to_end() -> TestResult {
        let (_dir, store) = admitted_job_store("work-mcp-tools-1")?;
        let supervisor: Arc<dyn McpSupervisor> =
            Arc::new(DurableMcpSupervisor::new(Arc::clone(&store)));
        let event_bus = Arc::new(McpEventBus::new(4).map_err(ctx("event bus"))?);

        let mut principals = PrincipalRegistry::new();
        principals
            .try_insert(
                "alice-owner".to_owned(),
                test_principal(
                    "alice",
                    vec![
                        McpJobCapability::SubmitOwn,
                        McpJobCapability::ReadOwn,
                        McpJobCapability::CancelOwn,
                    ],
                ),
            )
            .map_err(ctx("unique principal id registers"))?;
        // A different, unrelated actor: authenticates but has no capability
        // over this job's scope.
        principals
            .try_insert(
                "someone-else".to_owned(),
                test_principal("not-alice", vec![McpJobCapability::ReadOwn]),
            )
            .map_err(ctx("unique principal id registers"))?;

        let listener = BoundMcpListener::bind_with_supervisor_and_events(
            McpListenerConfig {
                address: "127.0.0.1:0"
                    .parse()
                    .map_err(ctx("valid loopback address"))?,
                max_sessions: 4,
                session_ttl: SignedDuration::from_secs(60),
            },
            Arc::new(StaticBearerAuthenticator::new(vec![
                ("alice-owner".to_owned(), b"alice-token".to_vec()),
                ("someone-else".to_owned(), b"other-token".to_vec()),
            ])),
            principals,
            Some(supervisor),
            Some(Arc::clone(&event_bus)),
        )
        .await
        .map_err(ctx("loopback listener binds"))?;
        let address = listener
            .local_addr()
            .map_err(ctx("bound listener has address"))?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server = tokio::spawn(listener.serve_until(shutdown_rx));

        let initialize = round_trip(
            address,
            post_request_as(
                "alice-token",
                &json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": VERSION,
                        "clientInfo": {"name": "tools-test"}
                    }
                }),
                "",
            ),
        )
        .await?;
        let id = session_id(&initialize)?;
        round_trip(
            address,
            post_request_as(
                "alice-token",
                &json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await?;
        let mut lifecycle_events = event_bus
            .subscribe(&id)
            .map_err(ctx("session receives lifecycle events"))?;

        let tool_catalog = round_trip(
            address,
            post_request_as(
                "alice-token",
                &json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await?;
        assert!(tool_catalog.contains("harw_job_submit"), "{tool_catalog}");
        assert!(tool_catalog.contains("harw_job_status"), "{tool_catalog}");
        assert!(tool_catalog.contains("harw_job_cancel"), "{tool_catalog}");

        let submitted = round_trip(
            address,
            post_request_as(
                "alice-token",
                &json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/call",
                    "params": {
                        "name": "harw_job_submit",
                        "arguments": {
                            "kind": "worker",
                            "input": {"task": "durable MCP review"},
                            "budget": {
                                "max_tokens": 4096,
                                "max_wall_seconds": 120,
                                "max_tool_calls": 8
                            }
                        }
                    }
                }),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await?;
        assert!(submitted.starts_with("HTTP/1.1 200"), "{submitted}");
        let (_, submitted_body) = submitted
            .split_once("\r\n\r\n")
            .ok_or(TestError::Missing("submit response has HTTP body"))?;
        let submitted_envelope: Value =
            serde_json::from_str(submitted_body).map_err(ctx("submit response is JSON"))?;
        let submitted_status: Value = serde_json::from_str(
            submitted_envelope
                .pointer("/result/content/0/text")
                .and_then(Value::as_str)
                .ok_or(TestError::Missing(
                    "submit result contains serialized status",
                ))?,
        )
        .map_err(ctx("submit status is JSON"))?;
        assert_eq!(submitted_status["kind"], "worker");
        assert_eq!(submitted_status["state"], "ready");
        let submitted_work_id = submitted_status["work_id"]
            .as_str()
            .ok_or(TestError::Missing("submit status contains work ID"))?;
        let persisted = store
            .get(&harw_types::WorkId::from_str(submitted_work_id))
            .map_err(ctx("submitted job is durably stored"))?;
        assert_eq!(persisted.input, json!({"task": "durable MCP review"}));
        assert_eq!(persisted.job.budget.max_tokens, Some(4096));
        assert_eq!(
            persisted.job.budget.max_wall,
            Some(SignedDuration::from_secs(120))
        );
        assert_eq!(persisted.job.budget.max_tool_calls, Some(8));
        let submitted_event = lifecycle_events
            .try_recv()
            .map_err(ctx("successful submission publishes an event"))?;
        match &submitted_event.kind {
            McpLifecycleEventKind::JobUpdated {
                work_id,
                state,
                revision,
            } => {
                assert_eq!(*work_id, harw_types::WorkId::from_str(submitted_work_id));
                assert_eq!(*state, JobState::Ready);
                assert_eq!(
                    *revision,
                    submitted_status["revision"]
                        .as_u64()
                        .ok_or(TestError::Missing("submission response contains revision"))?
                );
            }
            event => {
                return Err(TestError::Unexpected(format!(
                    "unexpected submission lifecycle event: {event:?}"
                )));
            }
        }
        let submitted_event_json = serde_json::to_string(&submitted_event)
            .map_err(ctx("submission lifecycle event serializes"))?;
        assert!(!submitted_event_json.contains("durable MCP review"));
        assert!(!submitted_event_json.contains("4096"));
        assert!(!submitted_event_json.contains("lease"));

        for (request_id, arguments, expected_error) in [
            (
                4,
                json!({"kind": "worker", "input": {}, "extra": true}),
                "expected only 'kind', 'input', and optional 'idempotency_key', 'budget'",
            ),
            (
                5,
                json!({"kind": "worker", "input": {}}),
                "'input' must be a non-empty object",
            ),
            (
                6,
                json!({"kind": "worker", "input": {"task": "review"}, "budget": {}}),
                "'budget' must contain at least one limit",
            ),
            (
                7,
                json!({
                    "kind": "worker",
                    "input": {"task": "review"},
                    "budget": {"max_tokens": u64::MAX}
                }),
                "'max_tokens' must be a finite positive integer",
            ),
        ] {
            let invalid_submit = round_trip(
                address,
                post_request_as(
                    "alice-token",
                    &json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "method": "tools/call",
                        "params": {
                            "name": "harw_job_submit",
                            "arguments": arguments
                        }
                    }),
                    &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
                ),
            )
            .await?;
            assert!(
                invalid_submit.contains("\"code\":-32602"),
                "{invalid_submit}"
            );
            assert!(invalid_submit.contains(expected_error), "{invalid_submit}");
        }

        let status_call = round_trip(
            address,
            post_request_as(
                "alice-token",
                &json!({
                    "jsonrpc": "2.0",
                    "id": 8,
                    "method": "tools/call",
                    "params": {"name": "harw_job_status", "arguments": {"work_id": "work-mcp-tools-1"}}
                }),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await?;
        assert!(status_call.starts_with("HTTP/1.1 200"), "{status_call}");
        assert!(status_call.contains("work-mcp-tools-1"), "{status_call}");
        assert!(!status_call.contains("lease"), "{status_call}");

        let cancelled = round_trip(
            address,
            post_request_as(
                "alice-token",
                &json!({
                    "jsonrpc": "2.0",
                    "id": 9,
                    "method": "tools/call",
                    "params": {
                        "name": "harw_job_cancel",
                        "arguments": {
                            "work_id": submitted_work_id,
                            "reason": "no longer needed"
                        }
                    }
                }),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await?;
        assert!(cancelled.starts_with("HTTP/1.1 200"), "{cancelled}");
        let cancellation_requested = lifecycle_events
            .try_recv()
            .map_err(ctx("successful cancellation publishes request event"))?;
        let cancelled_update = lifecycle_events
            .try_recv()
            .map_err(ctx("successful cancellation publishes state update"))?;
        let cancellation_revision = match cancellation_requested.kind {
            McpLifecycleEventKind::JobCancellationRequested { work_id, revision } => {
                assert_eq!(work_id, harw_types::WorkId::from_str(submitted_work_id));
                revision
            }
            event => {
                return Err(TestError::Unexpected(format!(
                    "unexpected cancellation lifecycle event: {event:?}"
                )));
            }
        };
        match cancelled_update.kind {
            McpLifecycleEventKind::JobUpdated {
                work_id,
                state,
                revision,
            } => {
                assert_eq!(work_id, harw_types::WorkId::from_str(submitted_work_id));
                assert_eq!(state, JobState::Cancelled);
                assert_eq!(revision, cancellation_revision);
            }
            event => {
                return Err(TestError::Unexpected(format!(
                    "unexpected cancellation update event: {event:?}"
                )));
            }
        }

        // A disconnected event subscriber must not turn a durable success
        // into a JSON-RPC failure.
        drop(lifecycle_events);
        let submitted_without_subscriber = round_trip(
            address,
            post_request_as(
                "alice-token",
                &json!({
                    "jsonrpc": "2.0",
                    "id": 10,
                    "method": "tools/call",
                    "params": {
                        "name": "harw_job_submit",
                        "arguments": {
                            "kind": "dream",
                            "input": {"task": "subscriber-free submission"}
                        }
                    }
                }),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await?;
        assert!(
            submitted_without_subscriber.starts_with("HTTP/1.1 200"),
            "{submitted_without_subscriber}"
        );

        // A principal outside the job's ownership sees the tool in its
        // capability-filtered catalog but is denied at the supervisor.
        let foreign_init = round_trip(
            address,
            post_request_as(
                "other-token",
                &json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": VERSION,
                        "clientInfo": {"name": "tools-test-foreign"}
                    }
                }),
                "",
            ),
        )
        .await?;
        let foreign_id = session_id(&foreign_init)?;
        round_trip(
            address,
            post_request_as(
                "other-token",
                &json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
                &format!("Mcp-Session-Id: {foreign_id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await?;
        let denied_call = round_trip(
            address,
            post_request_as(
                "other-token",
                &json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {"name": "harw_job_status", "arguments": {"work_id": "work-mcp-tools-1"}}
                }),
                &format!(
                    "Mcp-Session-Id: {foreign_id}\r\nMcp-Protocol-Version: {VERSION}\r\n"
                ),
            ),
        )
        .await?;
        assert!(denied_call.contains("\"code\":-32001"), "{denied_call}");

        shutdown_tx
            .send(true)
            .map_err(ctx("server accepts shutdown"))?;
        server
            .await
            .map_err(ctx("server task joins"))?
            .map_err(ctx("server shuts down cleanly"))?;
        Ok(())
    }

    #[test]
    fn sse_frames_use_real_line_breaks() {
        let lagged = sse_frame("lagged", &json!({"skipped": 3}).to_string());
        assert_eq!(&lagged[..], b"event: lagged\ndata: {\"skipped\":3}\n\n");
    }

    #[test]
    fn principal_registry_rejects_duplicate_ids_and_keeps_first_entry() -> TestResult {
        let mut registry = PrincipalRegistry::new();
        registry
            .try_insert(
                "alice".to_owned(),
                test_principal("alice", vec![McpJobCapability::ReadOwn]),
            )
            .map_err(ctx("first id registers"))?;
        let duplicate = registry.try_insert(
            "alice".to_owned(),
            test_principal("alice", vec![McpJobCapability::CancelWorkspace]),
        );
        assert_eq!(
            duplicate,
            Err(DuplicatePrincipalId {
                principal_key: "alice".to_owned(),
            })
        );
        let kept = registry
            .get("alice")
            .ok_or(TestError::Missing("first entry is kept"))?;
        assert!(kept.capabilities().contains(&McpJobCapability::ReadOwn));
        assert!(
            !kept
                .capabilities()
                .contains(&McpJobCapability::CancelWorkspace)
        );
        assert_eq!(registry.len(), 1);
        Ok(())
    }

    #[test]
    fn principal_registry_compat_insert_disables_duplicate_ids_fail_closed() {
        let mut registry = PrincipalRegistry::new();
        registry.insert(
            "bob".to_owned(),
            test_principal("bob", vec![McpJobCapability::ReadOwn]),
        );
        registry.insert(
            "bob".to_owned(),
            test_principal("bob", vec![McpJobCapability::CancelWorkspace]),
        );
        assert!(registry.get("bob").is_none());
        // Ein dritter Eintrag darf die gesperrte ID nicht wiederbeleben.
        registry.insert(
            "bob".to_owned(),
            test_principal("bob", vec![McpJobCapability::SubmitOwn]),
        );
        assert!(registry.get("bob").is_none());
        assert!(registry.is_empty());
        assert!(
            registry
                .try_insert("bob".to_owned(), test_principal("bob", Vec::new()))
                .is_err()
        );
    }

    #[tokio::test]
    async fn store_error_details_are_redacted_for_clients() -> TestResult {
        async fn body_of(response: McpResponse) -> TestResult<String> {
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(ctx("infallible body collects"))?
                .to_bytes();
            String::from_utf8(bytes.to_vec()).map_err(ctx("JSON body is UTF-8"))
        }

        let corrupt = supervisor_error_response(
            Some(json!(1)),
            &McpSupervisorError::JobStore(SessionStoreError::CorruptJob {
                work_id: harw_types::WorkId::from_str("work-secret-path"),
                detail: "/home/user/.harw/profiles/default/jobs/work-secret-path.json".to_owned(),
            }),
        );
        let corrupt = body_of(corrupt).await?;
        assert!(corrupt.contains("\"code\":-32000"), "{corrupt}");
        assert!(
            corrupt.contains("durable job operation failed"),
            "{corrupt}"
        );
        assert!(!corrupt.contains("/home"), "{corrupt}");
        assert!(!corrupt.contains("corrupt"), "{corrupt}");

        let io = supervisor_error_response(
            Some(json!(2)),
            &McpSupervisorError::JobStore(SessionStoreError::Io(std::io::Error::other(
                "refusing symlink at /var/lib/harw/jobs/x.json",
            ))),
        );
        let io = body_of(io).await?;
        assert!(!io.contains("/var/lib"), "{io}");
        assert!(!io.contains("symlink"), "{io}");

        let missing = supervisor_error_response(
            Some(json!(3)),
            &McpSupervisorError::JobStore(SessionStoreError::JobNotFound {
                work_id: harw_types::WorkId::from_str("work-unknown"),
            }),
        );
        let missing = body_of(missing).await?;
        assert!(missing.contains("job was not found"), "{missing}");
        assert!(!missing.contains("work-unknown"), "{missing}");

        // Nicht-Store-Fehler behalten ihre (pfadfreie) Meldung.
        let denied = body_of(supervisor_error_response(
            Some(json!(4)),
            &McpSupervisorError::NotAuthorized,
        ))
        .await?;
        assert!(denied.contains("\"code\":-32001"), "{denied}");
        Ok(())
    }

    #[tokio::test]
    async fn oversized_bodies_are_rejected_with_413_even_when_chunked() -> TestResult {
        let listener = BoundMcpListener::bind_authenticated(
            McpListenerConfig {
                address: "127.0.0.1:0"
                    .parse()
                    .map_err(ctx("valid loopback address"))?,
                max_sessions: 4,
                session_ttl: SignedDuration::from_secs(60),
            },
            Arc::new(StaticBearerAuthenticator::new(vec![(
                "test-principal".to_owned(),
                b"test-token".to_vec(),
            )])),
        )
        .await
        .map_err(ctx("loopback listener binds"))?;
        let address = listener
            .local_addr()
            .map_err(ctx("bound listener has address"))?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server = tokio::spawn(listener.serve_until(shutdown_rx));

        // Grenzfall: chunked genau MAX_BODY_BYTES (gültiges, mit Leerzeichen
        // aufgefülltes JSON) wird angenommen.
        let mut at_limit = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": VERSION}
        })
        .to_string()
        .into_bytes();
        at_limit.resize(MAX_BODY_BYTES, b' ');
        let accepted = round_trip_bytes_lenient(
            address,
            &chunked_post_request("test-token", &at_limit, &[8192; 8]),
        )
        .await?;
        assert!(accepted.starts_with("HTTP/1.1 200"), "{accepted}");

        // chunked ohne Content-Length: ein Byte über dem Limit, verteilt auf
        // viele Chunks; das letzte Byte kippt `Limited`.
        let over_limit = vec![b' '; MAX_BODY_BYTES + 1];
        let mut chunks = vec![8192; 8];
        chunks.push(1);
        let rejected = round_trip_bytes_lenient(
            address,
            &chunked_post_request("test-token", &over_limit, &chunks),
        )
        .await?;
        assert!(rejected.starts_with("HTTP/1.1 413"), "{rejected}");

        // Weit über dem Limit, ebenfalls chunked.
        let far_over = vec![b' '; MAX_BODY_BYTES * 4];
        let rejected_far = round_trip_bytes_lenient(
            address,
            &chunked_post_request("test-token", &far_over, &[MAX_BODY_BYTES; 4]),
        )
        .await?;
        assert!(rejected_far.starts_with("HTTP/1.1 413"), "{rejected_far}");

        // Content-Length über dem Limit greift schon vor dem Lesen.
        let declared = format!(
            "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-token\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        let rejected_declared = round_trip_bytes_lenient(address, declared.as_bytes()).await?;
        assert!(
            rejected_declared.starts_with("HTTP/1.1 413"),
            "{rejected_declared}"
        );

        shutdown_tx
            .send(true)
            .map_err(ctx("server accepts shutdown"))?;
        server
            .await
            .map_err(ctx("server task joins"))?
            .map_err(ctx("server shuts down cleanly"))?;
        Ok(())
    }

    #[tokio::test]
    async fn foreign_delete_is_indistinguishable_from_unknown_session_and_keeps_it() -> TestResult {
        let listener = BoundMcpListener::bind_authenticated(
            McpListenerConfig {
                address: "127.0.0.1:0"
                    .parse()
                    .map_err(ctx("valid loopback address"))?,
                max_sessions: 4,
                session_ttl: SignedDuration::from_secs(60),
            },
            Arc::new(StaticBearerAuthenticator::new(vec![
                ("principal-a".to_owned(), b"token-a".to_vec()),
                ("principal-b".to_owned(), b"token-b".to_vec()),
            ])),
        )
        .await
        .map_err(ctx("loopback listener binds"))?;
        let address = listener
            .local_addr()
            .map_err(ctx("bound listener has address"))?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server = tokio::spawn(listener.serve_until(shutdown_rx));

        let (id, advertised) = open_initialized_session(address, "token-a").await?;

        let foreign = round_trip(address, delete_request("token-b", &id, &advertised)).await?;
        assert!(foreign.starts_with("HTTP/1.1 404"), "{foreign}");
        let foreign_wrong_protocol =
            round_trip(address, delete_request("token-b", &id, "1999-01-01")).await?;
        assert!(
            foreign_wrong_protocol.starts_with("HTTP/1.1 404"),
            "{foreign_wrong_protocol}"
        );
        let unknown = round_trip(
            address,
            delete_request("token-b", "harw-no-such-session", &advertised),
        )
        .await?;
        assert!(unknown.starts_with("HTTP/1.1 404"), "{unknown}");
        // Keine Existenz-Leckage: fremd und unbekannt sind byteidentisch.
        assert_eq!(without_date_header(&foreign), without_date_header(&unknown));
        assert_eq!(
            without_date_header(&foreign_wrong_protocol),
            without_date_header(&unknown)
        );

        // Die Session von A besteht weiter.
        let still_usable = round_trip(
            address,
            post_request_as(
                "token-a",
                &json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {advertised}\r\n"),
            ),
        )
        .await?;
        assert!(still_usable.starts_with("HTTP/1.1 200"), "{still_usable}");

        // Der Eigentümer erhält weiterhin Protokollfehler und darf löschen —
        // und zwar mit genau der Protokollversion, die der Server im
        // `initialize`-Handschlag beworben hat (Z1-R3-04: `VERSION` und die
        // Protokollkonstante der Session-Registrierung liefen früher als
        // getrennte Literale und konnten auseinanderlaufen; DELETE hätte dann
        // still mit 404 statt 204 geantwortet).
        let owner_wrong_protocol =
            round_trip(address, delete_request("token-a", &id, "1999-01-01")).await?;
        assert!(
            owner_wrong_protocol.starts_with("HTTP/1.1 400"),
            "{owner_wrong_protocol}"
        );
        let owner = round_trip(address, delete_request("token-a", &id, &advertised)).await?;
        assert!(owner.starts_with("HTTP/1.1 204"), "{owner}");
        let again = round_trip(address, delete_request("token-a", &id, &advertised)).await?;
        assert!(again.starts_with("HTTP/1.1 404"), "{again}");

        shutdown_tx
            .send(true)
            .map_err(ctx("server accepts shutdown"))?;
        server
            .await
            .map_err(ctx("server task joins"))?
            .map_err(ctx("server shuts down cleanly"))?;
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_principal_ids_never_inherit_foreign_rights_over_tcp() -> TestResult {
        let mut principals = PrincipalRegistry::new();
        // Strikter Pfad: das Duplikat wird abgewiesen, der erste Eintrag gilt.
        principals
            .try_insert(
                "strict".to_owned(),
                test_principal("strict", vec![McpJobCapability::ReadOwn]),
            )
            .map_err(ctx("first strict id registers"))?;
        assert!(
            principals
                .try_insert(
                    "strict".to_owned(),
                    test_principal("strict", vec![McpJobCapability::CancelWorkspace]),
                )
                .is_err()
        );
        // Kompatibilitätspfad: das Duplikat sperrt die ID komplett.
        principals.insert(
            "compat".to_owned(),
            test_principal("compat", vec![McpJobCapability::ReadOwn]),
        );
        principals.insert(
            "compat".to_owned(),
            test_principal("compat", vec![McpJobCapability::CancelWorkspace]),
        );

        let listener = BoundMcpListener::bind_with_supervisor(
            McpListenerConfig {
                address: "127.0.0.1:0"
                    .parse()
                    .map_err(ctx("valid loopback address"))?,
                max_sessions: 4,
                session_ttl: SignedDuration::from_secs(60),
            },
            Arc::new(StaticBearerAuthenticator::new(vec![
                ("strict".to_owned(), b"strict-token".to_vec()),
                ("compat".to_owned(), b"compat-token".to_vec()),
            ])),
            principals,
            None,
        )
        .await
        .map_err(ctx("loopback listener binds"))?;
        let address = listener
            .local_addr()
            .map_err(ctx("bound listener has address"))?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server = tokio::spawn(listener.serve_until(shutdown_rx));

        let (strict_id, _) = open_initialized_session(address, "strict-token").await?;
        let strict_catalog = round_trip(
            address,
            post_request_as(
                "strict-token",
                &json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}),
                &format!("Mcp-Session-Id: {strict_id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await?;
        assert!(
            strict_catalog.starts_with("HTTP/1.1 200"),
            "{strict_catalog}"
        );
        assert!(
            strict_catalog.contains("harw_job_status"),
            "{strict_catalog}"
        );
        assert!(
            !strict_catalog.contains("harw_job_cancel"),
            "{strict_catalog}"
        );

        let (compat_id, _) = open_initialized_session(address, "compat-token").await?;
        let compat_catalog = round_trip(
            address,
            post_request_as(
                "compat-token",
                &json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}),
                &format!("Mcp-Session-Id: {compat_id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await?;
        assert!(
            compat_catalog.starts_with("HTTP/1.1 200"),
            "{compat_catalog}"
        );
        assert!(compat_catalog.contains("\"tools\":[]"), "{compat_catalog}");

        shutdown_tx
            .send(true)
            .map_err(ctx("server accepts shutdown"))?;
        server
            .await
            .map_err(ctx("server task joins"))?
            .map_err(ctx("server shuts down cleanly"))?;
        Ok(())
    }
}
