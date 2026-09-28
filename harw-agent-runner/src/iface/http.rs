//! HTTP/JSON interface for the compiled agent runner (`--listen <addr>`).
//!
//! # Overview
//! Exposes the embedded agent over a small HTTP/JSON API:
//! - `POST /run` — run a prompt; synchronous by default, or `{"background":
//!   true}` to get a `run_id` back immediately (`202`). An optional
//!   `context` string is prepended to the prompt using the same rule as
//!   the MCP interface's `tools/call run`. The two interfaces diverge on a
//!   non-string `context`, though: this one rejects it with `400`, while
//!   MCP silently drops it (see `tool_run` in `iface::mcp`).
//!   The turn itself always runs in a detached task, so a synchronous
//!   caller that disconnects mid-turn never leaves the run stuck
//!   `"running"`; it still reaches a terminal status.
//! - `GET /runs/{id}` — status/result of a run.
//! - `GET /runs/{id}/events` — an SSE stream of the SDK events for a run,
//!   one JSON object per `data:` line, `event:` set to the event's kind.
//!   The replay buffer for a late subscriber is capped at
//!   [`MAX_BUFFERED_EVENTS`]; once a run has dropped older events to stay
//!   under the cap, a new subscriber's stream opens with a `lagged` marker
//!   before the buffered replay.
//! - `POST /runs/{id}/cancel` — cancel a running (or not-yet-finished) run.
//! - `GET /manifest` — the root IR's permissions, declared interfaces and
//!   artifact digest.
//! - `GET /healthz` — liveness, never behind auth.
//!
//! Runs live in an in-memory map capped at [`MAX_RUNS`]; once over the cap,
//! the oldest *finished* run is evicted to make room (a run still in
//! flight is never evicted).
//!
//! # Auth and request hardening
//! A bearer token from `HARW_AGENT_HTTP_TOKEN` is required to start the
//! listener on a non-loopback address, optional (but checked if the
//! environment variable is set) on loopback. Comparison is constant-time.
//! A browser `Origin` that does not name a loopback host is refused with
//! `403` (an absent `Origin` passes), and a body nested deeper than
//! `child_protocol::MAX_JSON_NESTING_DEPTH` is refused with `400` before
//! it is parsed. `GET /healthz` is exempt so liveness probes don't need
//! the token. The checks themselves live in `iface::net`, shared with the
//! MCP interface's Streamable HTTP transport.
//!
//! # Approvals
//! Like the MCP interface: no interactive approval loop. Whatever
//! [`RunnerContext::harwness`] builds (denied unless the manifest grants
//! `--full-access`) is used as-is; the outcome shows up in the run's
//! `status` (`"cancelled"`/`"failed"` when a held-back tool call could not
//! be approved, `"truncated"` when the model's output was cut off) exactly
//! as `harwness_sdk::TurnReport` reports it.
//!
//! # HTTP stack
//! `hyper` 1.x + `hyper-util` + `http-body-util`, the same stack (and the
//! same versions) as `harw-web`/`harw-mcp-server` — no second HTTP library
//! enters the workspace.
//!
//! # Testability
//! The route logic never touches `hyper::body::Incoming` directly — [`dispatch`]
//! takes a method, path, header map and a decoded JSON body, so tests call
//! it in-process without a real socket. Likewise, driving a turn goes
//! through the small [`AgentBackend`]/[`RunSession`] traits rather than
//! `harwness_sdk::Harwness` directly, so tests can inject a fake session
//! (see `tests::EchoBackend`) instead of needing a real compiled agent
//! bundle.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures_core::Stream;
use http_body_util::{BodyExt, Channel};
use hyper::body::Incoming;
use hyper::header::{CACHE_CONTROL, CONTENT_TYPE, HeaderValue};
use hyper::service::service_fn;
use hyper::{HeaderMap, Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use tokio_stream::StreamExt as _;

use harwness_sdk::{CancelHandle, Harwness};

use super::net::{
    HttpResponse, bearer_authorized, json_response, origin_is_loopback, read_json_body,
    validate_listen_requirements,
};
use crate::context::RunnerContext;

/// Default bind address when `--listen` is not given.
const DEFAULT_LISTEN: &str = "127.0.0.1:8787";
/// Upper bound on a request body (`POST /run` is the only route with one).
const MAX_BODY_BYTES: usize = 256 * 1024;
/// Deadline for reading a (bounded) request body.
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How many runs the in-memory map keeps before evicting finished ones.
const MAX_RUNS: usize = 64;
/// Backlog of a run's live event broadcast channel; a slow SSE reader sees
/// `Lagged` and simply keeps going rather than blocking the run.
const EVENT_CHANNEL_CAPACITY: usize = 256;
/// Upper bound on how many events one run buffers for replay to a new SSE
/// subscriber. A long or chatty turn (one `text_delta`/`reasoning_delta`
/// per token) must not grow this buffer without bound, so once a run has
/// pushed more than this many events, the oldest are dropped; a subscriber
/// that attaches afterwards gets a `lagged` marker first so it knows the
/// replay is incomplete. Events already broadcast to a subscriber that was
/// attached at the time are unaffected — only the replay buffer is capped.
const MAX_BUFFERED_EVENTS: usize = 2048;
/// Small grace window after a turn settles to catch any event that was
/// published but not yet observed by the event pump.
const EVENT_DRAIN_GRACE: Duration = Duration::from_millis(20);

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Runs the HTTP/JSON interface until the process is asked to stop.
///
/// Binds `ctx.args.listen` (default [`DEFAULT_LISTEN`]), builds the
/// embedded agent's `Harwness` once via [`RunnerContext::harwness`], and
/// serves requests until `Ctrl-C`/SIGINT. Any setup failure (bad address,
/// missing token on a non-loopback bind, bind failure, `harwness()`
/// failure) is reported on stderr and yields [`ExitCode::FAILURE`].
pub fn run(ctx: RunnerContext) -> ExitCode {
    let listen = ctx
        .args
        .listen
        .clone()
        .unwrap_or_else(|| DEFAULT_LISTEN.to_owned());
    let addr: SocketAddr = match listen.parse() {
        Ok(addr) => addr,
        Err(error) => {
            eprintln!("harw-agent-runner: invalid --listen address '{listen}': {error}");
            return ExitCode::FAILURE;
        }
    };

    let token = std::env::var("HARW_AGENT_HTTP_TOKEN")
        .ok()
        .filter(|value| !value.is_empty());
    if let Err(reason) = validate_listen_requirements(addr, token.as_deref(), "the HTTP interface")
    {
        eprintln!("harw-agent-runner: {reason}");
        return ExitCode::FAILURE;
    }

    let manifest = build_manifest(&ctx);
    let harwness = match ctx.harwness() {
        Ok(harwness) => harwness,
        Err(error) => {
            eprintln!("harw-agent-runner: could not build the embedded agent: {error}");
            return ExitCode::FAILURE;
        }
    };

    let state = Arc::new(AppState {
        backend: Arc::new(harwness),
        token: token.map(String::into_bytes),
        runs: Mutex::new(Runs::default()),
        manifest,
    });

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("harw-agent-runner: could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(serve(addr, state))
}

/// The manifest surfaced at `GET /manifest`: the root IR's rights manifest,
/// declared interfaces/default interface, and the artifact's digest.
fn build_manifest(ctx: &RunnerContext) -> Value {
    let root_ir = ctx.root_ir();
    json!({
        "permissions": root_ir.permissions,
        "interfaces": root_ir.binary.interfaces,
        "default_interface": root_ir.binary.default_interface,
        "digest": ctx.agent.digest().to_string(),
    })
}

async fn serve(addr: SocketAddr, state: Arc<AppState>) -> ExitCode {
    let listener = match TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("harw-agent-runner: could not bind {addr}: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut connections: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            completed = connections.join_next(), if !connections.is_empty() => {
                let _ = completed;
            }
            accepted = listener.accept() => {
                let Ok((stream, _addr)) = accepted else { continue };
                let state = Arc::clone(&state);
                connections.spawn(async move {
                    let builder = Builder::new(TokioExecutor::new());
                    let _ = builder
                        .serve_connection(
                            TokioIo::new(stream),
                            service_fn(move |request: Request<Incoming>| {
                                let state = Arc::clone(&state);
                                async move { handle(state, request).await }
                            }),
                        )
                        .await;
                });
            }
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    ExitCode::SUCCESS
}

/// Reads a raw hyper request into the pieces [`dispatch`] needs. Generic
/// over the body type (the listener passes hyper's `Incoming`) so the body
/// checks — size cap, JSON depth limit — are testable in-process.
async fn handle<B>(state: Arc<AppState>, request: Request<B>) -> Result<HttpResponse, Infallible>
where
    B: hyper::body::Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let headers = request.headers().clone();
    if path == "/healthz" {
        return Ok(dispatch(&state, &method, &path, &headers, Value::Null).await);
    }
    let body = match read_json_body(request, MAX_BODY_BYTES, BODY_READ_TIMEOUT).await {
        Ok(value) => value,
        Err(response) => return Ok(response),
    };
    Ok(dispatch(&state, &method, &path, &headers, body).await)
}

/// The transport-independent router: `/healthz`, then auth, then Origin,
/// then the route table. Every test in this module calls this directly
/// instead of going through a real socket.
async fn dispatch(
    state: &AppState,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    body: Value,
) -> HttpResponse {
    if path == "/healthz" {
        return json_response(StatusCode::OK, &json!({"status": "ok"}));
    }
    if !bearer_authorized(state.token.as_deref(), headers) {
        return json_response(StatusCode::UNAUTHORIZED, &json!({"error": "unauthorized"}));
    }
    if !origin_is_loopback(headers) {
        return json_response(
            StatusCode::FORBIDDEN,
            &json!({"error": "origin_not_allowed"}),
        );
    }
    route(state, method, path, body).await
}

async fn route(state: &AppState, method: &Method, path: &str, body: Value) -> HttpResponse {
    let segments = path_segments(path);
    match segments.as_slice() {
        ["run"] if *method == Method::POST => handle_run(state, body).await,
        ["runs", id] if *method == Method::GET => handle_get_run(state, id),
        ["runs", id, "events"] if *method == Method::GET => handle_events(state, id),
        ["runs", id, "cancel"] if *method == Method::POST => handle_cancel(state, id),
        ["manifest"] if *method == Method::GET => json_response(StatusCode::OK, &state.manifest),
        _ => json_response(StatusCode::NOT_FOUND, &json!({"error": "not_found"})),
    }
}

fn path_segments(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}

// ── Run request/response bodies ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RunRequest {
    prompt: String,
    #[serde(default)]
    context: Option<Value>,
    #[serde(default)]
    background: bool,
}

/// Builds the text actually sent to the model: `context`, if present and a
/// non-empty string, is prepended to `prompt` — the same rule the MCP
/// interface's `tools/call run` applies
/// (`HarwnessPromptRunner::run_prompt` in `iface::mcp`) — so the two
/// interfaces treat the same request the same way instead of one of them
/// silently dropping `context`.
fn prompt_with_context(context: Option<&str>, prompt: &str) -> String {
    match context.filter(|value| !value.is_empty()) {
        Some(context) => format!("{context}\n\n{prompt}"),
        None => prompt.to_owned(),
    }
}

async fn handle_run(state: &AppState, body: Value) -> HttpResponse {
    let request: RunRequest = match serde_json::from_value(body) {
        Ok(request) => request,
        Err(error) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error": "invalid_request", "detail": error.to_string()}),
            );
        }
    };
    if request.prompt.trim().is_empty() {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error": "invalid_request", "detail": "prompt must not be empty"}),
        );
    }
    let context = match &request.context {
        None => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error": "invalid_request", "detail": "context must be a string"}),
            );
        }
    };
    let prompt = prompt_with_context(context.as_deref(), &request.prompt);

    let session = match state.backend.open() {
        Ok(session) => session,
        Err(error) => {
            return json_response(StatusCode::INTERNAL_SERVER_ERROR, &json!({"error": error}));
        }
    };
    let cancel = session.cancel_handle();

    let run_id = state
        .runs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .alloc_id();
    let record = Arc::new(Mutex::new(RunRecord::new(cancel)));
    state
        .runs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(run_id.clone(), Arc::clone(&record));

    // The turn always runs in its own detached task, in both modes. A
    // synchronous caller only *awaits* `handle`'s `JoinHandle` below; if
    // that outer future is ever dropped (the client disconnected), the
    // task keeps running and still reaches a terminal status — dropping a
    // `JoinHandle` does not abort the task it names.
    let handle = tokio::spawn(drive_run(Arc::clone(&record), session, prompt));
    if request.background {
        json_response(StatusCode::ACCEPTED, &json!({"run_id": run_id}))
    } else {
        let _ = handle.await;
        let snapshot = record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshot(&run_id);
        json_response(StatusCode::OK, &snapshot)
    }
}

fn handle_get_run(state: &AppState, id: &str) -> HttpResponse {
    match state
        .runs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(id)
    {
        Some(record) => {
            let guard = record
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            json_response(StatusCode::OK, &guard.snapshot(id))
        }
        None => json_response(StatusCode::NOT_FOUND, &json!({"error": "not_found"})),
    }
}

fn handle_cancel(state: &AppState, id: &str) -> HttpResponse {
    match state
        .runs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(id)
    {
        Some(record) => {
            let cancelled = {
                let guard = record
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                guard.cancel.cancel()
            };
            json_response(
                StatusCode::OK,
                &json!({"run_id": id, "cancelled": cancelled}),
            )
        }
        None => json_response(StatusCode::NOT_FOUND, &json!({"error": "not_found"})),
    }
}

fn handle_events(state: &AppState, id: &str) -> HttpResponse {
    let Some(record) = state
        .runs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(id)
    else {
        return json_response(StatusCode::NOT_FOUND, &json!({"error": "not_found"}));
    };
    let (buffered, dropped, mut receiver, already_finished) = {
        let guard = record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            guard.events.iter().cloned().collect::<Vec<_>>(),
            guard.dropped_events,
            guard.event_tx.subscribe(),
            guard.status.is_finished(),
        )
    };

    let (mut sender, body) = Channel::<Bytes, Infallible>::new(8);
    tokio::spawn(async move {
        if dropped > 0 {
            // Older events were evicted from the replay buffer (see
            // `MAX_BUFFERED_EVENTS`); tell this subscriber before replaying
            // what is left, rather than silently starting mid-stream.
            let lagged = json!({"event": "lagged", "skipped": dropped});
            if sender.send_data(sse_frame(&lagged)).await.is_err() {
                return;
            }
        }
        for envelope in buffered {
            if sender.send_data(sse_frame(&envelope)).await.is_err() {
                return;
            }
        }
        if already_finished {
            return;
        }
        loop {
            match receiver.recv().await {
                Ok(envelope) => {
                    if sender.send_data(sse_frame(&envelope)).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    });

    let mut response = Response::new(body.boxed());
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn sse_frame(envelope: &Value) -> Bytes {
    let kind = envelope
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or("message");
    Bytes::from(format!("event: {kind}\ndata: {envelope}\n\n"))
}

// ── Driving a turn ───────────────────────────────────────────────────────

/// Runs a turn to completion, forwarding every SDK event into the run's
/// buffer/broadcast as it happens, then records the final status/text/usage
/// and publishes a terminal `run.finished` marker event.
async fn drive_run(
    record: Arc<Mutex<RunRecord>>,
    mut session: Box<dyn RunSession>,
    prompt: String,
) {
    {
        let mut guard = record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.status = RunStatus::Running;
    }

    let mut events = session.events();
    // `send()` already returns a `Pin<Box<dyn Future>>`; `Box` is
    // unconditionally `Unpin`, so this can be reused across `select!`
    // iterations by `&mut` reference without an extra `tokio::pin!`.
    let mut send_future = session.send(prompt);

    let outcome = loop {
        tokio::select! {
            biased;
            event = events.next() => {
                if let Some(envelope) = event {
                    record_event(&record, envelope);
                }
            }
            result = &mut send_future => break result,
        }
    };
    // A short grace window: the turn settled, but the event that announced
    // it may still be sitting in the channel, not yet observed above.
    while let Ok(Some(envelope)) = tokio::time::timeout(EVENT_DRAIN_GRACE, events.next()).await {
        record_event(&record, envelope);
    }

    let mut guard = record
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match outcome {
        Ok(result) => {
            guard.status = result.status;
            guard.text = result.text;
            guard.usage = Some(usage_json(&result.usage));
        }
        Err(error) => {
            guard.status = RunStatus::Failed;
            guard.error = Some(error);
        }
    }
    let finished = json!({"event": "run.finished", "status": guard.status.as_str()});
    guard.push_event(finished.clone());
    let _ = guard.event_tx.send(finished);
}

fn record_event(record: &Arc<Mutex<RunRecord>>, envelope: Value) {
    let mut guard = record
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.push_event(envelope.clone());
    let _ = guard.event_tx.send(envelope);
}

fn usage_json(usage: &UsageInfo) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "reasoning_tokens": usage.reasoning_tokens,
        "cached_tokens": usage.cached_tokens,
        "cache_write_tokens": usage.cache_write_tokens,
        "total_tokens": usage.input_tokens.saturating_add(usage.output_tokens),
    })
}

// ── Run bookkeeping ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunStatus {
    Pending,
    Running,
    Completed,
    /// The model's output was cut off (e.g. at a length limit). Reported
    /// distinctly from `Completed` — an interrupted turn is not a
    /// successful one — matching the MCP interface's `"truncated"` and the
    /// child protocol's decision to treat it as unsuccessful.
    Truncated,
    Failed,
    Cancelled,
}

impl RunStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Truncated => "truncated",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn is_finished(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Truncated | Self::Failed | Self::Cancelled
        )
    }
}

struct RunRecord {
    status: RunStatus,
    text: Option<String>,
    usage: Option<Value>,
    error: Option<String>,
    /// Replay buffer for a new SSE subscriber, capped at
    /// [`MAX_BUFFERED_EVENTS`] (oldest first out); see `push_event`.
    events: VecDeque<Value>,
    /// How many events have been evicted from `events` to stay under the
    /// cap. A new SSE subscriber that sees this above zero gets a `lagged`
    /// marker before the replay (see `handle_events`).
    dropped_events: u64,
    event_tx: broadcast::Sender<Value>,
    cancel: Arc<dyn Cancellable>,
}

impl RunRecord {
    fn new(cancel: Arc<dyn Cancellable>) -> Self {
        let (event_tx, _receiver) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            status: RunStatus::Pending,
            text: None,
            usage: None,
            error: None,
            events: VecDeque::new(),
            dropped_events: 0,
            event_tx,
            cancel,
        }
    }

    /// Appends one event to the replay buffer, dropping the oldest once
    /// [`MAX_BUFFERED_EVENTS`] is reached so a long or chatty run cannot
    /// grow this buffer without bound. Events already delivered to a
    /// subscriber attached at the time are unaffected — only the replay
    /// buffer for future subscribers shrinks.
    fn push_event(&mut self, envelope: Value) {
        if self.events.len() >= MAX_BUFFERED_EVENTS {
            self.events.pop_front();
            self.dropped_events = self.dropped_events.saturating_add(1);
        }
        self.events.push_back(envelope);
    }

    fn snapshot(&self, run_id: &str) -> Value {
        json!({
            "run_id": run_id,
            "status": self.status.as_str(),
            "text": self.text,
            "usage": self.usage,
            "error": self.error,
        })
    }
}

#[derive(Default)]
struct Runs {
    order: VecDeque<String>,
    map: HashMap<String, Arc<Mutex<RunRecord>>>,
    counter: u64,
}

impl Runs {
    fn alloc_id(&mut self) -> String {
        self.counter = self.counter.wrapping_add(1);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        format!("run-{nanos:x}-{}", self.counter)
    }

    fn insert(&mut self, id: String, record: Arc<Mutex<RunRecord>>) {
        self.order.push_back(id.clone());
        self.map.insert(id, record);
        self.evict_finished_over_cap();
    }

    /// Evicts the oldest *finished* runs while the map is over [`MAX_RUNS`].
    /// A run still in flight is never evicted, even if that leaves the map
    /// temporarily over the cap.
    fn evict_finished_over_cap(&mut self) {
        while self.map.len() > MAX_RUNS {
            let evictable = self.order.iter().position(|id| {
                self.map.get(id).is_some_and(|record| {
                    record
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .status
                        .is_finished()
                })
            });
            match evictable {
                Some(index) => {
                    if let Some(id) = self.order.remove(index) {
                        self.map.remove(&id);
                    }
                }
                None => break,
            }
        }
    }

    fn get(&self, id: &str) -> Option<Arc<Mutex<RunRecord>>> {
        self.map.get(id).cloned()
    }
}

struct AppState {
    backend: Arc<dyn AgentBackend>,
    token: Option<Vec<u8>>,
    runs: Mutex<Runs>,
    manifest: Value,
}

// ── Backend abstraction (real SDK + a fake for tests) ───────────────────

/// A local, non-`#[non_exhaustive]` stand-in for `harwness_sdk::TurnStatus`,
/// so a fake `RunSession` (used in tests) can build one without needing to
/// construct the real, `#[non_exhaustive]` SDK type from outside its crate.
type RunOutcomeStatus = RunStatus;

struct RunOutcome {
    status: RunOutcomeStatus,
    text: Option<String>,
    usage: UsageInfo,
}

#[derive(Debug, Clone, Default)]
struct UsageInfo {
    input_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: Option<u64>,
    cached_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
}

impl UsageInfo {
    fn from_sdk(usage: &harwness_sdk::Usage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            cached_tokens: usage.cached_tokens,
            cache_write_tokens: usage.cache_write_tokens,
        }
    }
}

/// Cancels a run's in-flight turn. A thin trait (rather than
/// `harwness_sdk::CancelHandle` directly) so tests can inject their own
/// implementation without needing a real, running SDK turn to cancel.
trait Cancellable: Send + Sync {
    fn cancel(&self) -> bool;
}

impl Cancellable for CancelHandle {
    fn cancel(&self) -> bool {
        CancelHandle::cancel(self)
    }
}

/// Opens sessions against the embedded agent. Implemented for
/// `harwness_sdk::Harwness` in production; tests implement it for a fake
/// echo backend instead.
trait AgentBackend: Send + Sync + 'static {
    fn open(&self) -> Result<Box<dyn RunSession>, String>;
}

/// One open conversation, abstracted just enough to drive a turn and
/// observe its events without naming `#[non_exhaustive]` SDK types in a
/// way a test would need to construct.
trait RunSession: Send + 'static {
    /// A live stream of already-JSON-encoded SDK events. Must be called at
    /// most once per session ([`drive_run`] calls it exactly once).
    fn events(&mut self) -> Pin<Box<dyn Stream<Item = Value> + Send>>;
    fn cancel_handle(&self) -> Arc<dyn Cancellable>;
    fn send(&mut self, prompt: String) -> BoxFuture<'_, Result<RunOutcome, String>>;
}

struct SdkSession(harwness_sdk::Session);

impl RunSession for SdkSession {
    fn events(&mut self) -> Pin<Box<dyn Stream<Item = Value> + Send>> {
        Box::pin(self.0.events().map(|event| sdk_event_envelope(&event)))
    }

    fn cancel_handle(&self) -> Arc<dyn Cancellable> {
        Arc::new(self.0.cancel_handle())
    }

    fn send(&mut self, prompt: String) -> BoxFuture<'_, Result<RunOutcome, String>> {
        Box::pin(async move {
            let report = self
                .0
                .send(prompt)
                .await
                .map_err(|error| error.to_string())?;
            Ok(RunOutcome {
                status: status_from_turn(&report.status),
                text: report.text,
                usage: UsageInfo::from_sdk(&report.usage),
            })
        })
    }
}

impl AgentBackend for Harwness {
    fn open(&self) -> Result<Box<dyn RunSession>, String> {
        self.session()
            .map(|session| Box::new(SdkSession(session)) as Box<dyn RunSession>)
            .map_err(|error| error.to_string())
    }
}

fn status_from_turn(status: &harwness_sdk::TurnStatus) -> RunStatus {
    use harwness_sdk::TurnStatus;
    match status {
        TurnStatus::Completed => RunStatus::Completed,
        TurnStatus::Truncated => RunStatus::Truncated,
        TurnStatus::Cancelled { .. } => RunStatus::Cancelled,
        TurnStatus::Refused { .. } | TurnStatus::Failed { .. } => RunStatus::Failed,
        // `TurnStatus` is `#[non_exhaustive]`; treat anything future as a
        // failure rather than silently reporting success.
        _ => RunStatus::Failed,
    }
}

/// Converts one `harwness_sdk::SdkEvent` into the `{"event": <kind>, ...}`
/// JSON envelope this interface streams over SSE.
fn sdk_event_envelope(event: &harwness_sdk::SdkEvent) -> Value {
    use harwness_sdk::{FinishStatus, SdkEvent, ToolOutput};

    fn source_json(source: &harwness_sdk::EventSource) -> Value {
        json!({
            "session_id": source.session_id.to_string(),
            "parent": source.parent.as_ref().map(ToString::to_string),
            "role": source.role,
        })
    }

    fn tool_output_json(output: &ToolOutput) -> Value {
        match output {
            ToolOutput::Success { value } => json!({"success": true, "value": value}),
            ToolOutput::Error { message } => json!({"success": false, "message": message}),
            // `ToolOutput` is `#[non_exhaustive]`.
            _ => json!({"success": false, "message": "unknown tool output"}),
        }
    }

    match event {
        SdkEvent::TurnStarted { source, turn_id } => json!({
            "event": "turn_started",
            "source": source_json(source),
            "turn_id": turn_id,
        }),
        SdkEvent::TextDelta { source, text } => json!({
            "event": "text_delta",
            "source": source_json(source),
            "text": text,
        }),
        SdkEvent::ReasoningDelta { source, text } => json!({
            "event": "reasoning_delta",
            "source": source_json(source),
            "text": text,
        }),
        SdkEvent::Message {
            source,
            text,
            final_answer,
        } => json!({
            "event": "message",
            "source": source_json(source),
            "text": text,
            "final_answer": final_answer,
        }),
        SdkEvent::ToolCall {
            source,
            call_id,
            tool,
            arguments,
        } => json!({
            "event": "tool_call",
            "source": source_json(source),
            "call_id": call_id,
            "tool": tool,
            "arguments": arguments,
        }),
        SdkEvent::ToolResult {
            source,
            call_id,
            output,
            duration,
        } => json!({
            "event": "tool_result",
            "source": source_json(source),
            "call_id": call_id,
            "output": tool_output_json(output),
            "duration_ms": duration.as_millis() as u64,
        }),
        SdkEvent::ChildSpawned {
            source,
            child,
            role,
            task,
        } => json!({
            "event": "child_spawned",
            "source": source_json(source),
            "child": child.to_string(),
            "role": role,
            "task": task,
        }),
        SdkEvent::ChildCompleted {
            source,
            child,
            outcome,
            duration,
        } => json!({
            "event": "child_completed",
            "source": source_json(source),
            "child": child.to_string(),
            "outcome": outcome,
            "duration_ms": duration.as_millis() as u64,
        }),
        SdkEvent::Usage {
            source,
            round,
            turn_total,
            final_round,
        } => json!({
            "event": "usage",
            "source": source_json(source),
            "round": usage_json(&UsageInfo::from_sdk(round)),
            "turn_total": usage_json(&UsageInfo::from_sdk(turn_total)),
            "final_round": final_round,
        }),
        SdkEvent::Context {
            source,
            used_tokens,
            window_tokens,
        } => json!({
            "event": "context",
            "source": source_json(source),
            "used_tokens": used_tokens,
            "window_tokens": window_tokens,
        }),
        SdkEvent::Error {
            source,
            message,
            retryable,
        } => json!({
            "event": "error",
            "source": source_json(source),
            "message": message,
            "retryable": retryable,
        }),
        SdkEvent::Finished {
            source,
            status,
            usage,
        } => json!({
            "event": "finished",
            "source": source_json(source),
            "status": match status {
                FinishStatus::Completed => "completed",
                FinishStatus::Aborted => "aborted",
                // `FinishStatus` is `#[non_exhaustive]`.
                _ => "unknown",
            },
            "usage": usage.as_ref().map(UsageInfo::from_sdk).as_ref().map(usage_json),
        }),
        SdkEvent::Lagged { skipped } => json!({
            "event": "lagged",
            "skipped": skipped,
        }),
        // `SdkEvent` is `#[non_exhaustive]`.
        _ => json!({"event": "unknown"}),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use http_body_util::Full;
    use hyper::header::{AUTHORIZATION, ORIGIN};

    use crate::child_protocol::MAX_JSON_NESTING_DEPTH;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // ── A fake backend: no real agent bundle needed ─────────────────────

    struct FlagCancel(Arc<AtomicBool>);

    impl Cancellable for FlagCancel {
        fn cancel(&self) -> bool {
            self.0.store(true, Ordering::SeqCst);
            true
        }
    }

    struct EchoSession {
        cancel_flag: Arc<AtomicBool>,
        events_tx: tokio::sync::mpsc::UnboundedSender<Value>,
        events_rx: Option<tokio::sync::mpsc::UnboundedReceiver<Value>>,
    }

    impl EchoSession {
        fn new() -> Self {
            let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
            Self {
                cancel_flag: Arc::new(AtomicBool::new(false)),
                events_tx,
                events_rx: Some(events_rx),
            }
        }
    }

    impl RunSession for EchoSession {
        fn events(&mut self) -> Pin<Box<dyn Stream<Item = Value> + Send>> {
            // Called at most once per session (see `RunSession::events`); a
            // second call gets an empty stream rather than a panic.
            let Some(receiver) = self.events_rx.take() else {
                return Box::pin(tokio_stream::empty::<Value>());
            };
            Box::pin(tokio_stream::wrappers::UnboundedReceiverStream::new(
                receiver,
            ))
        }

        fn cancel_handle(&self) -> Arc<dyn Cancellable> {
            Arc::new(FlagCancel(Arc::clone(&self.cancel_flag)))
        }

        fn send(&mut self, prompt: String) -> BoxFuture<'_, Result<RunOutcome, String>> {
            let _ = self
                .events_tx
                .send(json!({"event": "message", "text": prompt, "final_answer": true}));
            let flag = Arc::clone(&self.cancel_flag);
            Box::pin(async move {
                // "block" lets a test cancel a run while it is in flight.
                if prompt == "block" {
                    for _ in 0..200 {
                        if flag.load(Ordering::SeqCst) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
                let cancelled = flag.load(Ordering::SeqCst);
                Ok(RunOutcome {
                    status: if cancelled {
                        RunStatus::Cancelled
                    } else {
                        RunStatus::Completed
                    },
                    text: Some(format!("echo: {prompt}")),
                    usage: UsageInfo::default(),
                })
            })
        }
    }

    struct EchoBackend;

    impl AgentBackend for EchoBackend {
        fn open(&self) -> Result<Box<dyn RunSession>, String> {
            Ok(Box::new(EchoSession::new()))
        }
    }

    fn test_state(token: Option<&str>) -> Arc<AppState> {
        Arc::new(AppState {
            backend: Arc::new(EchoBackend),
            token: token.map(|value| value.as_bytes().to_vec()),
            runs: Mutex::new(Runs::default()),
            manifest: json!({"stub": true}),
        })
    }

    async fn body_json(response: HttpResponse) -> Result<Value, Box<dyn std::error::Error>> {
        let bytes = response.into_body().collect().await?.to_bytes();
        Ok(serde_json::from_slice(&bytes)?)
    }

    fn bearer(token: &str) -> Result<HeaderMap, Box<dyn std::error::Error>> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))?,
        );
        Ok(headers)
    }

    // ── Startup requirements ─────────────────────────────────────────────

    #[test]
    fn non_loopback_bind_without_token_is_rejected() -> TestResult {
        let addr: SocketAddr = "0.0.0.0:8787".parse()?;
        assert!(validate_listen_requirements(addr, None, "the HTTP interface").is_err());
        Ok(())
    }

    #[test]
    fn non_loopback_bind_with_token_is_allowed() -> TestResult {
        let addr: SocketAddr = "0.0.0.0:8787".parse()?;
        assert!(validate_listen_requirements(addr, Some("secret"), "the HTTP interface").is_ok());
        Ok(())
    }

    #[test]
    fn loopback_bind_without_token_is_allowed() -> TestResult {
        let addr: SocketAddr = "127.0.0.1:8787".parse()?;
        assert!(validate_listen_requirements(addr, None, "the HTTP interface").is_ok());
        Ok(())
    }

    // ── Auth ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn missing_token_configuration_allows_any_request() {
        let state = test_state(None);
        let response = dispatch(
            &state,
            &Method::GET,
            "/manifest",
            &HeaderMap::new(),
            Value::Null,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn wrong_token_yields_401() -> TestResult {
        let state = test_state(Some("correct-token"));
        let response = dispatch(
            &state,
            &Method::GET,
            "/manifest",
            &bearer("wrong")?,
            Value::Null,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        Ok(())
    }

    #[tokio::test]
    async fn correct_token_is_accepted() -> TestResult {
        let state = test_state(Some("correct-token"));
        let response = dispatch(
            &state,
            &Method::GET,
            "/manifest",
            &bearer("correct-token")?,
            Value::Null,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        Ok(())
    }

    #[tokio::test]
    async fn healthz_bypasses_auth() {
        let state = test_state(Some("secret"));
        let response = dispatch(
            &state,
            &Method::GET,
            "/healthz",
            &HeaderMap::new(),
            Value::Null,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    // ── Origin guard ─────────────────────────────────────────────────────

    fn with_origin(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, HeaderValue::from_static(value));
        headers
    }

    #[tokio::test]
    async fn a_non_loopback_origin_is_refused() {
        let state = test_state(None);
        let response = dispatch(
            &state,
            &Method::GET,
            "/manifest",
            &with_origin("http://evil.example"),
            Value::Null,
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn loopback_origins_are_accepted() {
        let state = test_state(None);
        for origin in [
            "http://localhost:5173",
            "http://127.0.0.1",
            "http://[::1]:8787",
        ] {
            let response = dispatch(
                &state,
                &Method::GET,
                "/manifest",
                &with_origin(origin),
                Value::Null,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK, "{origin}");
        }
    }

    #[tokio::test]
    async fn a_bad_origin_is_refused_even_with_a_valid_token() -> TestResult {
        let state = test_state(Some("tok"));
        let mut headers = bearer("tok")?;
        headers.insert(ORIGIN, HeaderValue::from_static("https://evil.example"));
        let response = dispatch(&state, &Method::GET, "/manifest", &headers, Value::Null).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    // ── Body hardening (through `handle`, in-memory body) ────────────────

    #[tokio::test]
    async fn an_over_deep_json_body_is_400() -> TestResult {
        let state = test_state(None);
        let depth = MAX_JSON_NESTING_DEPTH + 1;
        let body = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        let request = Request::builder()
            .method(Method::POST)
            .uri("/run")
            .body(Full::new(Bytes::from(body)))?;
        let response = handle(state, request).await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let value = body_json(response).await?;
        assert_eq!(value["error"], "invalid_json");
        Ok(())
    }

    #[tokio::test]
    async fn a_json_body_within_the_depth_limit_reaches_the_route() -> TestResult {
        let state = test_state(None);
        let request = Request::builder()
            .method(Method::POST)
            .uri("/run")
            .body(Full::new(Bytes::from_static(br#"{"prompt":"hi"}"#)))?;
        let response = handle(state, request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        Ok(())
    }

    // ── POST /run (offline echo) ─────────────────────────────────────────

    #[tokio::test]
    async fn run_with_offline_echo_returns_completed_text() -> TestResult {
        let state = test_state(None);
        let response = dispatch(
            &state,
            &Method::POST,
            "/run",
            &HeaderMap::new(),
            json!({"prompt": "hello"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = body_json(response).await?;
        assert_eq!(value["status"], "completed");
        assert_eq!(value["text"], "echo: hello");
        Ok(())
    }

    #[tokio::test]
    async fn empty_prompt_is_rejected() {
        let state = test_state(None);
        let response = dispatch(
            &state,
            &Method::POST,
            "/run",
            &HeaderMap::new(),
            json!({"prompt": "   "}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // ── `context` (prepended like MCP's `tools/call run`) ────────────────

    #[tokio::test]
    async fn a_string_context_is_prepended_to_the_prompt() -> TestResult {
        let state = test_state(None);
        let response = dispatch(
            &state,
            &Method::POST,
            "/run",
            &HeaderMap::new(),
            json!({"prompt": "p", "context": "c"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = body_json(response).await?;
        let text = value["text"].as_str().ok_or("text must be a string")?;
        assert_eq!(text, "echo: c\n\np", "got: {text}");
        Ok(())
    }

    #[tokio::test]
    async fn a_non_string_context_is_400() -> TestResult {
        let state = test_state(None);
        let response = dispatch(
            &state,
            &Method::POST,
            "/run",
            &HeaderMap::new(),
            json!({"prompt": "p", "context": 5}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        Ok(())
    }

    // ── Background run + status ──────────────────────────────────────────

    #[tokio::test]
    async fn background_run_completes_and_is_queryable() -> TestResult {
        let state = test_state(None);
        let response = dispatch(
            &state,
            &Method::POST,
            "/run",
            &HeaderMap::new(),
            json!({"prompt": "hi", "background": true}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let value = body_json(response).await?;
        let run_id = value["run_id"]
            .as_str()
            .ok_or("run_id must be a string")?
            .to_owned();

        let mut status = String::new();
        for _ in 0..100 {
            let poll = dispatch(
                &state,
                &Method::GET,
                &format!("/runs/{run_id}"),
                &HeaderMap::new(),
                Value::Null,
            )
            .await;
            let poll_value = body_json(poll).await?;
            status = poll_value["status"]
                .as_str()
                .ok_or("status must be a string")?
                .to_owned();
            if status == "completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(status, "completed");
        Ok(())
    }

    #[tokio::test]
    async fn unknown_run_id_is_404() {
        let state = test_state(None);
        let response = dispatch(
            &state,
            &Method::GET,
            "/runs/does-not-exist",
            &HeaderMap::new(),
            Value::Null,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    // ── Disconnect safety (synchronous `/run`) ────────────────────────────

    #[tokio::test]
    async fn a_dropped_synchronous_run_still_reaches_a_terminal_status() -> TestResult {
        let state = test_state(None);

        // Simulate a client that disconnects mid-turn: the request future
        // is dropped well before a "block" turn would settle on its own.
        let outcome = tokio::time::timeout(
            Duration::from_millis(10),
            handle_run(&state, json!({"prompt": "block"})),
        )
        .await;
        assert!(
            outcome.is_err(),
            "the request future should still have been in flight at 10ms"
        );

        let run_id = {
            let guard = state.runs.lock().map_err(|_| "poisoned lock")?;
            guard
                .order
                .front()
                .cloned()
                .ok_or("no run was recorded before the request future was dropped")?
        };

        // Unblock the detached turn instead of waiting out its own
        // timeout, then give it a moment to observe the cancellation.
        let cancel = {
            let guard = state.runs.lock().map_err(|_| "poisoned lock")?;
            let record = guard.get(&run_id).ok_or("run vanished from the map")?;
            Arc::clone(&record.lock().map_err(|_| "poisoned lock")?.cancel)
        };
        cancel.cancel();

        let mut status = String::new();
        for _ in 0..200 {
            {
                let guard = state.runs.lock().map_err(|_| "poisoned lock")?;
                let record = guard.get(&run_id).ok_or("run vanished from the map")?;
                status = record
                    .lock()
                    .map_err(|_| "poisoned lock")?
                    .status
                    .as_str()
                    .to_owned();
            }
            if status != "pending" && status != "running" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_ne!(status, "running", "the run must not stay running forever");
        assert_ne!(status, "pending");
        Ok(())
    }

    // ── SSE events ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn events_stream_yields_an_event_and_a_finished_marker() -> TestResult {
        let state = test_state(None);
        let response = dispatch(
            &state,
            &Method::POST,
            "/run",
            &HeaderMap::new(),
            json!({"prompt": "hi", "background": true}),
        )
        .await;
        let value = body_json(response).await?;
        let run_id = value["run_id"]
            .as_str()
            .ok_or("run_id must be a string")?
            .to_owned();

        // Let the background run settle so both the echoed message and the
        // terminal marker are already buffered.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let events_response = dispatch(
            &state,
            &Method::GET,
            &format!("/runs/{run_id}/events"),
            &HeaderMap::new(),
            Value::Null,
        )
        .await;
        assert_eq!(events_response.status(), StatusCode::OK);
        assert_eq!(
            events_response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream; charset=utf-8")
        );

        let mut body = events_response.into_body();
        let mut collected = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if tokio::time::Instant::now() > deadline {
                break;
            }
            match tokio::time::timeout(Duration::from_millis(200), body.frame()).await {
                Ok(Some(Ok(frame))) => {
                    if let Some(data) = frame.data_ref() {
                        collected.push_str(&String::from_utf8_lossy(data));
                    }
                }
                _ => break,
            }
        }
        assert!(collected.contains("event: message"), "got: {collected}");
        assert!(collected.contains("run.finished"), "got: {collected}");
        Ok(())
    }

    #[tokio::test]
    async fn event_buffer_is_capped_and_a_late_subscriber_sees_a_lagged_marker() -> TestResult {
        let state = test_state(None);
        let cancel: Arc<dyn Cancellable> = Arc::new(FlagCancel(Arc::new(AtomicBool::new(false))));
        let record = Arc::new(Mutex::new(RunRecord::new(cancel)));
        for index in 0..(MAX_BUFFERED_EVENTS + 100) {
            record_event(&record, json!({"event": "text_delta", "text": index}));
        }
        {
            let mut guard = record.lock().map_err(|_| "poisoned lock")?;
            guard.status = RunStatus::Completed;
            assert!(
                guard.events.len() <= MAX_BUFFERED_EVENTS,
                "buffer grew to {}",
                guard.events.len()
            );
            assert_eq!(guard.dropped_events, 100);
        }
        state
            .runs
            .lock()
            .map_err(|_| "poisoned lock")?
            .insert("run-cap-test".to_owned(), record);

        let events_response = handle_events(&state, "run-cap-test");
        let mut body = events_response.into_body();
        let frame = tokio::time::timeout(Duration::from_millis(200), body.frame())
            .await
            .map_err(|_| "timed out waiting for the first SSE frame")?
            .ok_or("stream ended before any frame was sent")?
            .map_err(|_| "frame carried an error")?;
        let data = frame.data_ref().ok_or("frame carried no data")?;
        let text = String::from_utf8_lossy(data);
        assert!(text.starts_with("event: lagged"), "got: {text}");
        assert!(text.contains("\"skipped\":100"), "got: {text}");
        Ok(())
    }

    // ── Cancel ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn cancel_marks_the_run_as_cancelled() -> TestResult {
        let state = test_state(None);
        let response = dispatch(
            &state,
            &Method::POST,
            "/run",
            &HeaderMap::new(),
            json!({"prompt": "block", "background": true}),
        )
        .await;
        let value = body_json(response).await?;
        let run_id = value["run_id"]
            .as_str()
            .ok_or("run_id must be a string")?
            .to_owned();

        tokio::time::sleep(Duration::from_millis(20)).await;
        let cancel_response = dispatch(
            &state,
            &Method::POST,
            &format!("/runs/{run_id}/cancel"),
            &HeaderMap::new(),
            Value::Null,
        )
        .await;
        assert_eq!(cancel_response.status(), StatusCode::OK);
        let cancel_value = body_json(cancel_response).await?;
        assert_eq!(cancel_value["cancelled"], true);

        let mut status = String::new();
        for _ in 0..200 {
            let poll = dispatch(
                &state,
                &Method::GET,
                &format!("/runs/{run_id}"),
                &HeaderMap::new(),
                Value::Null,
            )
            .await;
            let poll_value = body_json(poll).await?;
            status = poll_value["status"]
                .as_str()
                .ok_or("status must be a string")?
                .to_owned();
            if status == "cancelled" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(status, "cancelled");
        Ok(())
    }

    // ── Run-cap eviction ─────────────────────────────────────────────────

    #[test]
    fn run_cap_evicts_oldest_finished_runs_first() -> TestResult {
        let mut runs = Runs::default();
        for index in 0..(MAX_RUNS + 10) {
            let record = Arc::new(Mutex::new(RunRecord::new(Arc::new(FlagCancel(Arc::new(
                AtomicBool::new(false),
            ))))));
            record.lock().map_err(|_| "poisoned lock")?.status = RunStatus::Completed;
            runs.insert(format!("run-{index}"), record);
        }
        assert!(runs.map.len() <= MAX_RUNS, "map grew to {}", runs.map.len());
        Ok(())
    }

    #[test]
    fn run_cap_never_evicts_a_run_still_in_flight() -> TestResult {
        let mut runs = Runs::default();
        // One run that never finishes, then enough finished runs to push
        // well past the cap.
        let in_flight = Arc::new(Mutex::new(RunRecord::new(Arc::new(FlagCancel(Arc::new(
            AtomicBool::new(false),
        ))))));
        runs.insert("in-flight".to_owned(), in_flight);
        for index in 0..(MAX_RUNS + 10) {
            let record = Arc::new(Mutex::new(RunRecord::new(Arc::new(FlagCancel(Arc::new(
                AtomicBool::new(false),
            ))))));
            record.lock().map_err(|_| "poisoned lock")?.status = RunStatus::Completed;
            runs.insert(format!("run-{index}"), record);
        }
        assert!(runs.map.contains_key("in-flight"));
        Ok(())
    }

    // ── Turn status mapping ──────────────────────────────────────────────

    #[test]
    fn a_truncated_turn_is_not_reported_as_completed() -> TestResult {
        let status = status_from_turn(&harwness_sdk::TurnStatus::Truncated);
        assert_ne!(status, RunStatus::Completed);
        assert_eq!(status, RunStatus::Truncated);
        assert!(status.is_finished());
        assert_eq!(status.as_str(), "truncated");
        Ok(())
    }
}
