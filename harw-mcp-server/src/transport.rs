//! Loopback-only HTTP/1 adapter for the Streamable HTTP MCP session boundary.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Channel, Full, combinators::BoxBody};
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
const VERSION: &str = "2025-06-18";
const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";
const MAX_BODY_BYTES: usize = 64 * 1024;

type McpResponse = Response<BoxBody<Bytes, Infallible>>;

/// Maps an authenticated `principal_key` to its resolved workspace authority.
/// Built once at composition time from server-trusted configuration; never
/// derived from an MCP request.
pub type PrincipalRegistry = HashMap<String, McpPrincipal>;

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
        Method::DELETE => delete(request, state).await,
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

async fn delete(
    request: Request<Incoming>,
    state: Arc<Mutex<McpSessionRegistry>>,
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
    match sessions.require(id, protocol, jiff::Timestamp::now()) {
        Ok(_) => match sessions.remove(id) {
            Ok(()) => Ok(empty_response(StatusCode::NO_CONTENT)),
            Err(error) => Ok(session_error_response(&error)),
        },
        Err(error) => Ok(session_error_response(&error)),
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
    if request
        .body()
        .size_hint()
        .upper()
        .is_some_and(|size| size > MAX_BODY_BYTES as u64)
    {
        return Ok(response(StatusCode::PAYLOAD_TOO_LARGE, Value::Null, None));
    }
    let bytes = match request.into_body().collect().await {
        Ok(body) => body.to_bytes(),
        Err(_) => return Ok(response(StatusCode::BAD_REQUEST, Value::Null, None)),
    };
    if bytes.len() > MAX_BODY_BYTES {
        return Ok(response(StatusCode::PAYLOAD_TOO_LARGE, Value::Null, None));
    }
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
                Ok(event) => format!(
                    "event: lifecycle\\ndata: {}\\n\\n",
                    serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_owned())
                ),
                Err(McpEventReceiveError::Lagged { skipped }) => format!(
                    "event: lagged\\ndata: {}\\n\\n",
                    json!({"skipped": skipped})
                ),
                Err(McpEventReceiveError::Closed) => break,
                Err(McpEventReceiveError::Empty) => continue,
            };
            if sender.send_data(Bytes::from(frame)).await.is_err() {
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
                            state: harw_job_runtime::JobState::Cancelled,
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
        || arguments.len() > 3
        || !arguments.contains_key("kind")
        || !arguments.contains_key("input")
        || arguments
            .keys()
            .any(|key| !matches!(key.as_str(), "kind" | "input" | "budget"))
    {
        return Err(
            "invalid harw_job_submit arguments: expected only 'kind', 'input', and optional 'budget'",
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
    let budget = arguments
        .get("budget")
        .map(parse_submit_budget)
        .transpose()?;
    Ok(McpJobSubmission {
        kind,
        input,
        budget,
    })
}

fn parse_submit_budget(value: &Value) -> Result<harw_job_runtime::Budget, &'static str> {
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
    Ok(harw_job_runtime::Budget {
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

fn supervisor_error_response(id: Option<Value>, error: &McpSupervisorError) -> McpResponse {
    let code = match error {
        McpSupervisorError::NotAuthorized => -32001,
        McpSupervisorError::InvalidSubmission(_) => -32602,
        McpSupervisorError::JobStore(_) => -32000,
    };
    json_rpc_error(id, code, &error.to_string(), None)
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

    use harw_job_runtime::{Budget, Job, JobKind, JobScope, JobState, RetryPolicy};
    use harw_session_store::JobStore;
    use harw_types::ApprovalActor;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    use crate::supervisor::DurableMcpSupervisor;

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

    async fn round_trip(address: SocketAddr, request: String) -> String {
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("listener accepts test connection");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("test request writes");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .await
            .expect("test response reads");
        response
    }

    fn session_id(response: &str) -> String {
        response
            .lines()
            .find_map(|line| line.strip_prefix("mcp-session-id: "))
            .expect("initialize returns a session header")
            .to_owned()
    }

    #[test]
    fn origin_validation_accepts_native_and_loopback_clients() {
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
                HeaderValue::from_str(origin).expect("valid Origin header value"),
            );
            assert!(origin_is_loopback(&headers), "{origin}");
        }
    }

    #[test]
    fn origin_validation_rejects_malformed_or_deceptive_authorities() {
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
                HeaderValue::from_str(origin).expect("valid Origin header value"),
            );
            assert!(!origin_is_loopback(&headers), "{origin}");
        }
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
    async fn streamable_http_session_lifecycle_is_enforced_over_tcp() {
        let listener = BoundMcpListener::bind_authenticated(
            McpListenerConfig {
                address: "127.0.0.1:0".parse().expect("valid loopback address"),
                max_sessions: 4,
                session_ttl: SignedDuration::from_secs(60),
            },
            Arc::new(StaticBearerAuthenticator::new(vec![(
                "test-principal".to_owned(),
                b"test-token".to_vec(),
            )])),
        )
        .await
        .expect("loopback listener binds");
        let address = listener.local_addr().expect("bound listener has address");
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
        .await;
        assert!(initialize.starts_with("HTTP/1.1 200"), "{initialize}");
        assert!(initialize.contains("\"protocolVersion\":\"2025-06-18\""));
        let id = session_id(&initialize);

        let initialized = round_trip(
            address,
            post_request(
                &json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await;
        assert!(initialized.starts_with("HTTP/1.1 202"));

        let empty_tool_catalog = round_trip(
            address,
            post_request(
                &json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await;
        assert!(empty_tool_catalog.starts_with("HTTP/1.1 200"));
        assert!(empty_tool_catalog.contains("\"tools\":[]"));

        let missing_version = round_trip(
            address,
            format!(
                "DELETE /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-token\r\nMcp-Session-Id: {id}\r\nConnection: close\r\n\r\n"
            ),
        )
        .await;
        assert!(missing_version.starts_with("HTTP/1.1 400"));

        let deleted = round_trip(
            address,
            format!(
                "DELETE /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-token\r\nMcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\nConnection: close\r\n\r\n"
            ),
        )
        .await;
        assert!(deleted.starts_with("HTTP/1.1 204"));

        let stale = round_trip(
            address,
            post_request(
                &json!({"jsonrpc":"2.0", "id":3, "method":"tools/list"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await;
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
        .await;
        assert!(foreign_origin.starts_with("HTTP/1.1 403"));

        shutdown_tx.send(true).expect("server accepts shutdown");
        server
            .await
            .expect("server task joins")
            .expect("server shuts down cleanly");
    }

    #[tokio::test]
    async fn session_rejects_requests_authenticated_as_a_different_principal() {
        let listener = BoundMcpListener::bind_authenticated(
            McpListenerConfig {
                address: "127.0.0.1:0".parse().expect("valid loopback address"),
                max_sessions: 4,
                session_ttl: SignedDuration::from_secs(60),
            },
            Arc::new(StaticBearerAuthenticator::new(vec![
                ("principal-a".to_owned(), b"token-a".to_vec()),
                ("principal-b".to_owned(), b"token-b".to_vec()),
            ])),
        )
        .await
        .expect("loopback listener binds");
        let address = listener.local_addr().expect("bound listener has address");
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
        .await;
        assert!(initialize.starts_with("HTTP/1.1 200"), "{initialize}");
        let id = session_id(&initialize);

        let initialized = round_trip(
            address,
            post_request_as(
                "token-a",
                &json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await;
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
        .await;
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
        .await;
        assert!(
            same_principal.starts_with("HTTP/1.1 200"),
            "{same_principal}"
        );

        shutdown_tx.send(true).expect("server accepts shutdown");
        server
            .await
            .expect("server task joins")
            .expect("server shuts down cleanly");
    }

    #[tokio::test]
    async fn initialized_get_streams_bounded_lifecycle_events() {
        let event_bus = Arc::new(crate::events::McpEventBus::new(2).expect("event bus"));
        let listener = BoundMcpListener::bind_with_supervisor_and_events(
            McpListenerConfig {
                address: "127.0.0.1:0".parse().expect("valid loopback address"),
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
        .expect("loopback listener binds");
        let address = listener.local_addr().expect("bound listener has address");
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
        .await;
        let id = session_id(&initialize);
        let initialized = round_trip(
            address,
            post_request(
                &json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await;
        assert!(initialized.starts_with("HTTP/1.1 202"));

        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("GET connects");
        stream
            .write_all(format!(
                "GET /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-token\r\nAccept: text/event-stream\r\nMcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n\r\n"
            ).as_bytes())
            .await
            .expect("GET writes");
        let mut reader = BufReader::new(stream);
        let mut headers = Vec::new();
        loop {
            let mut line = Vec::new();
            reader
                .read_until(b'\n', &mut line)
                .await
                .expect("headers read");
            headers.extend_from_slice(&line);
            if headers.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let headers = String::from_utf8_lossy(&headers);
        assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
        assert!(headers.to_ascii_lowercase().contains("text/event-stream"));

        event_bus
            .publish(
                &id,
                jiff::Timestamp::now(),
                crate::events::McpLifecycleEventKind::JobCancellationRequested {
                    work_id: harw_types::WorkId::new(),
                    revision: 7,
                },
            )
            .expect("event is delivered to GET subscriber");
        let mut data = [0_u8; 1024];
        let read = tokio::time::timeout(std::time::Duration::from_secs(2), reader.read(&mut data))
            .await
            .expect("event arrives")
            .expect("event read");
        let data = String::from_utf8_lossy(&data[..read]);
        assert!(data.contains("event: lifecycle"), "{data}");
        assert!(data.contains("sequence"), "{data}");

        shutdown_tx.send(true).expect("server accepts shutdown");
        server
            .await
            .expect("server task joins")
            .expect("server shuts down cleanly");
    }

    /// Minimal directory guard so a test-local `JobStore` root is removed
    /// even if an assertion panics partway through.
    struct TempJobStoreDir(std::path::PathBuf);
    impl TempJobStoreDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "harw-mcp-server-test-{label}-{}-{}",
                std::process::id(),
                jiff::Timestamp::now().as_nanosecond()
            ));
            std::fs::create_dir_all(&path).expect("temp job store dir creates");
            Self(path)
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

    fn admitted_job_store(work_id: &str) -> (TempJobStoreDir, Arc<JobStore>) {
        let dir = TempJobStoreDir::new(work_id);
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
            },
            now,
        );
        job.mark_ready(now).expect("job transitions to ready");
        let record = harw_job_runtime::StoredJob {
            job,
            scope: JobScope::new(
                harw_types::TenantId::from_str("test-tenant"),
                harw_types::WorkspaceId::from_str("test-workspace"),
                ApprovalActor::Operator {
                    id: "mia".to_owned(),
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
        store.admit(&record).expect("job admits");
        (dir, Arc::new(store))
    }

    #[tokio::test]
    async fn tools_are_capability_filtered_and_authorized_end_to_end() {
        let (_dir, store) = admitted_job_store("work-mcp-tools-1");
        let supervisor: Arc<dyn McpSupervisor> =
            Arc::new(DurableMcpSupervisor::new(Arc::clone(&store)));
        let event_bus = Arc::new(McpEventBus::new(4).expect("event bus"));

        let mut principals = PrincipalRegistry::new();
        principals.insert(
            "mia-owner".to_owned(),
            McpPrincipal::from_trusted_ingress(
                ApprovalActor::Operator {
                    id: "mia".to_owned(),
                },
                harw_types::TenantId::from_str("test-tenant"),
                harw_types::WorkspaceId::from_str("test-workspace"),
                vec![
                    McpJobCapability::SubmitOwn,
                    McpJobCapability::ReadOwn,
                    McpJobCapability::CancelOwn,
                ],
            ),
        );
        // A different, unrelated actor: authenticates but has no capability
        // over this job's scope.
        principals.insert(
            "someone-else".to_owned(),
            McpPrincipal::from_trusted_ingress(
                ApprovalActor::Operator {
                    id: "not-mia".to_owned(),
                },
                harw_types::TenantId::from_str("test-tenant"),
                harw_types::WorkspaceId::from_str("test-workspace"),
                vec![McpJobCapability::ReadOwn],
            ),
        );

        let listener = BoundMcpListener::bind_with_supervisor_and_events(
            McpListenerConfig {
                address: "127.0.0.1:0".parse().expect("valid loopback address"),
                max_sessions: 4,
                session_ttl: SignedDuration::from_secs(60),
            },
            Arc::new(StaticBearerAuthenticator::new(vec![
                ("mia-owner".to_owned(), b"mia-token".to_vec()),
                ("someone-else".to_owned(), b"other-token".to_vec()),
            ])),
            principals,
            Some(supervisor),
            Some(Arc::clone(&event_bus)),
        )
        .await
        .expect("loopback listener binds");
        let address = listener.local_addr().expect("bound listener has address");
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server = tokio::spawn(listener.serve_until(shutdown_rx));

        let initialize = round_trip(
            address,
            post_request_as(
                "mia-token",
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
        .await;
        let id = session_id(&initialize);
        round_trip(
            address,
            post_request_as(
                "mia-token",
                &json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await;
        let mut lifecycle_events = event_bus
            .subscribe(&id)
            .expect("session receives lifecycle events");

        let tool_catalog = round_trip(
            address,
            post_request_as(
                "mia-token",
                &json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await;
        assert!(tool_catalog.contains("harw_job_submit"), "{tool_catalog}");
        assert!(tool_catalog.contains("harw_job_status"), "{tool_catalog}");
        assert!(tool_catalog.contains("harw_job_cancel"), "{tool_catalog}");

        let submitted = round_trip(
            address,
            post_request_as(
                "mia-token",
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
        .await;
        assert!(submitted.starts_with("HTTP/1.1 200"), "{submitted}");
        let (_, submitted_body) = submitted
            .split_once("\r\n\r\n")
            .expect("submit response has HTTP body");
        let submitted_envelope: Value =
            serde_json::from_str(submitted_body).expect("submit response is JSON");
        let submitted_status: Value = serde_json::from_str(
            submitted_envelope
                .pointer("/result/content/0/text")
                .and_then(Value::as_str)
                .expect("submit result contains serialized status"),
        )
        .expect("submit status is JSON");
        assert_eq!(submitted_status["kind"], "worker");
        assert_eq!(submitted_status["state"], "ready");
        let submitted_work_id = submitted_status["work_id"]
            .as_str()
            .expect("submit status contains work ID");
        let persisted = store
            .get(&harw_types::WorkId::from_str(submitted_work_id))
            .expect("submitted job is durably stored");
        assert_eq!(persisted.input, json!({"task": "durable MCP review"}));
        assert_eq!(persisted.job.budget.max_tokens, Some(4096));
        assert_eq!(
            persisted.job.budget.max_wall,
            Some(SignedDuration::from_secs(120))
        );
        assert_eq!(persisted.job.budget.max_tool_calls, Some(8));
        let submitted_event = lifecycle_events
            .try_recv()
            .expect("successful submission publishes an event");
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
                        .expect("submission response contains revision")
                );
            }
            event => panic!("unexpected submission lifecycle event: {event:?}"),
        }
        let submitted_event_json =
            serde_json::to_string(&submitted_event).expect("submission lifecycle event serializes");
        assert!(!submitted_event_json.contains("durable MCP review"));
        assert!(!submitted_event_json.contains("4096"));
        assert!(!submitted_event_json.contains("lease"));

        for (request_id, arguments, expected_error) in [
            (
                4,
                json!({"kind": "worker", "input": {}, "extra": true}),
                "expected only 'kind', 'input', and optional 'budget'",
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
                    "mia-token",
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
            .await;
            assert!(
                invalid_submit.contains("\"code\":-32602"),
                "{invalid_submit}"
            );
            assert!(invalid_submit.contains(expected_error), "{invalid_submit}");
        }

        let status_call = round_trip(
            address,
            post_request_as(
                "mia-token",
                &json!({
                    "jsonrpc": "2.0",
                    "id": 8,
                    "method": "tools/call",
                    "params": {"name": "harw_job_status", "arguments": {"work_id": "work-mcp-tools-1"}}
                }),
                &format!("Mcp-Session-Id: {id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await;
        assert!(status_call.starts_with("HTTP/1.1 200"), "{status_call}");
        assert!(status_call.contains("work-mcp-tools-1"), "{status_call}");
        assert!(!status_call.contains("lease"), "{status_call}");

        let cancelled = round_trip(
            address,
            post_request_as(
                "mia-token",
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
        .await;
        assert!(cancelled.starts_with("HTTP/1.1 200"), "{cancelled}");
        let cancellation_requested = lifecycle_events
            .try_recv()
            .expect("successful cancellation publishes request event");
        let cancelled_update = lifecycle_events
            .try_recv()
            .expect("successful cancellation publishes state update");
        let cancellation_revision = match cancellation_requested.kind {
            McpLifecycleEventKind::JobCancellationRequested { work_id, revision } => {
                assert_eq!(work_id, harw_types::WorkId::from_str(submitted_work_id));
                revision
            }
            event => panic!("unexpected cancellation lifecycle event: {event:?}"),
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
            event => panic!("unexpected cancellation update event: {event:?}"),
        }

        // A disconnected event subscriber must not turn a durable success
        // into a JSON-RPC failure.
        drop(lifecycle_events);
        let submitted_without_subscriber = round_trip(
            address,
            post_request_as(
                "mia-token",
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
        .await;
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
        .await;
        let foreign_id = session_id(&foreign_init);
        round_trip(
            address,
            post_request_as(
                "other-token",
                &json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
                &format!("Mcp-Session-Id: {foreign_id}\r\nMcp-Protocol-Version: {VERSION}\r\n"),
            ),
        )
        .await;
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
        .await;
        assert!(denied_call.contains("\"code\":-32001"), "{denied_call}");

        shutdown_tx.send(true).expect("server accepts shutdown");
        server
            .await
            .expect("server task joins")
            .expect("server shuts down cleanly");
    }
}
