//! MCP (Model Context Protocol) interface for the embedded agent runner.
//!
//! # Scope
//! Implements the tool-calling half of MCP for a single compiled agent:
//! `initialize`, `tools/list`, `tools/call` (`run`, `status`, `cancel`),
//! `notifications/cancelled` and `ping`. Two transports share one JSON-RPC
//! handler ([`McpServer::handle_message`]):
//! - **stdio**: newline-delimited JSON-RPC 2.0 over stdin/stdout (the
//!   default, and the only transport when `--listen` is not given).
//! - **Streamable HTTP** (`--listen <addr>`): `POST /mcp` plus `GET
//!   /healthz`, this module's own small loopback listener (see below).
//!
//! Both advertise MCP protocol version [`MCP_PROTOCOL_VERSION`].
//!
//! # Why not `harw-mcp-server`'s Streamable HTTP transport
//! `harw-mcp-server::transport::BoundMcpListener` is built around a durable
//! job supervisor (`McpSupervisor`) and tenant/workspace-scoped principals
//! (`McpPrincipal`, `PrincipalRegistry`) exposing exactly three fixed tools
//! (`harw_job_submit/status/cancel`) against that supervisor. A compiled,
//! embedded single-agent runner has none of that: no tenant/workspace
//! authority, no durable job store, and a *different* tool surface (`run`
//! taking a free-form `prompt`, tool descriptions derived from the agent's
//! own IR). Bridging the two would mean reimplementing this module's tool
//! semantics as fake "jobs" underneath that supervisor trait, which buys
//! nothing over a small, self-contained JSON-RPC handler. This module
//! therefore only takes the one thing worth sharing verbatim — the
//! advertised protocol version literal — and implements its own minimal
//! JSON-RPC 2.0 loop plus its own minimal HTTP transport (the "Streamable
//! HTTP transport" section further down in this file), rather than reusing
//! `harw-mcp-server`'s supervisor-backed one. The HTTP transport
//! intentionally mirrors (does not import) the auth/Origin logic of
//! `iface::http` and `harw-mcp-server::transport`: a handful of small,
//! independent functions, not a shared dependency.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::io::{BufRead, Write};
use std::net::{IpAddr, SocketAddr};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderName, HeaderValue, ORIGIN};
use hyper::service::service_fn;
use hyper::{HeaderMap, Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinSet;

use crate::child_protocol::{MAX_JSON_NESTING_DEPTH, json_nesting_too_deep};
use crate::context::RunnerContext;

/// The MCP protocol version this interface implements and advertises during
/// `initialize`. Kept in lock-step with `harw-mcp-server::MCP_PROTOCOL_VERSION`
/// by convention (both name the same wire version), not by a shared
/// dependency — see the module doc for why this module does not depend on
/// `harw-mcp-server`.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

const SERVER_NAME: &str = "harwness-agent-runner";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Entry point for `iface::Interface` (feature `mcp`).
///
/// Without `--listen`, runs the stdio JSON-RPC loop to completion (client
/// closed stdin, or a fatal I/O error). With `--listen <addr>`, runs the
/// Streamable HTTP transport (its own small loopback listener; see the
/// module doc) until `Ctrl-C`/SIGINT. Either way returns the process exit
/// code.
pub fn run(ctx: RunnerContext) -> ExitCode {
    match ctx.args.listen.clone() {
        Some(addr) => run_http(ctx, addr),
        None => run_stdio(ctx),
    }
}

fn run_stdio(ctx: RunnerContext) -> ExitCode {
    let ctx = Arc::new(ctx);
    let server = McpServer::new(Arc::clone(&ctx), Arc::new(HarwnessPromptRunner { ctx }));
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    server.serve_stdio(stdin.lock(), &mut stdout)
}

// ---------------------------------------------------------------------------
// Background run tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum RunState {
    Running,
    Completed,
    Cancelled,
    Failed,
}

impl RunState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

/// One background (`background: true`) run tracked for `status`/`cancel`.
struct TrackedRun {
    state: RunState,
    result: Option<Value>,
    cancel: Option<Box<dyn Fn() + Send + Sync>>,
}

/// Upper bound on the number of background runs [`RunRegistry`] keeps before
/// evicting finished ones — mirrors `iface::http`'s `MAX_RUNS`/`Runs` cap, so
/// an MCP client cannot exhaust memory (and, via `tool_run_background`'s
/// `std::thread::spawn`, OS threads) by starting unbounded `background: true`
/// runs and never calling `status`/`cancel`. A run still `Running` is never
/// evicted, exactly like the HTTP interface's cap.
const MAX_TRACKED_RUNS: usize = 64;

/// Registry of background runs, keyed by a runner-issued `run_id`.
#[derive(Default)]
struct RunRegistry {
    next_id: AtomicU64,
    order: Mutex<VecDeque<String>>,
    runs: Mutex<HashMap<String, TrackedRun>>,
}

impl RunRegistry {
    fn new_id(&self) -> String {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        format!("run-{n}")
    }

    /// Evicts the oldest *finished* runs while the map is over
    /// [`MAX_TRACKED_RUNS`]. A run still [`RunState::Running`] is never
    /// evicted, even if that temporarily leaves the map over the cap (same
    /// contract as `iface::http::Runs::evict_finished_over_cap`).
    fn evict_finished_over_cap(&self) {
        let mut order = self.order.lock().unwrap_or_else(|e| e.into_inner());
        let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        while runs.len() > MAX_TRACKED_RUNS {
            let evictable = order
                .iter()
                .position(|id| runs.get(id).is_some_and(|run| run.state != RunState::Running));
            match evictable {
                Some(index) => {
                    if let Some(id) = order.remove(index) {
                        runs.remove(&id);
                    }
                }
                None => break,
            }
        }
    }

    fn insert_running(&self, id: String, cancel: Box<dyn Fn() + Send + Sync>) {
        self.order
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(id.clone());
        self.runs.lock().unwrap_or_else(|e| e.into_inner()).insert(
            id,
            TrackedRun {
                state: RunState::Running,
                result: None,
                cancel: Some(cancel),
            },
        );
        self.evict_finished_over_cap();
    }

    fn complete(&self, id: &str, state: RunState, result: Value) {
        if let Some(run) = self
            .runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(id)
        {
            run.state = state;
            run.result = Some(result);
            run.cancel = None;
        }
        self.evict_finished_over_cap();
    }

    fn status(&self, id: &str) -> Option<(RunState, Option<Value>)> {
        self.runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|run| (run.state.clone(), run.result.clone()))
    }

    /// Requests cancellation. Returns `true` if a running entry was found
    /// (regardless of whether cancellation lands before the run already
    /// finished on its own — that race is inherent to cooperative
    /// cancellation and is reported truthfully by a subsequent `status`).
    fn cancel(&self, id: &str) -> bool {
        let cancel = {
            let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
            match runs.get_mut(id) {
                Some(run) if run.state == RunState::Running => run.cancel.take(),
                Some(_) => return true,
                None => return false,
            }
        };
        if let Some(cancel) = cancel {
            cancel();
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Prompt execution, abstracted for pure handler tests
// ---------------------------------------------------------------------------

/// Outcome of running one prompt to completion (or to cancellation/failure).
#[derive(Debug, Clone, Default)]
struct PromptOutcome {
    status: &'static str,
    text: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    tool_calls: u32,
    approvals: u32,
}

impl PromptOutcome {
    fn to_json(&self) -> Value {
        json!({
            "status": self.status,
            "text": self.text,
            "usage": {
                "input_tokens": self.input_tokens,
                "output_tokens": self.output_tokens,
            },
            "tool_calls": self.tool_calls,
        })
    }
}

/// A handle that can cancel a run in progress, from another thread.
type CancelFn = Box<dyn Fn() + Send + Sync>;

/// Abstraction over "run one prompt through a session", with progress
/// callbacks for streamed assistant text. Lets the JSON-RPC handler be
/// exercised in tests without a real `Harwness`/tokio runtime.
trait PromptRunner: Send + Sync {
    /// Runs `prompt` (optionally prefixed by `context`) to completion,
    /// invoking `on_progress` with each incremental text chunk. Returns the
    /// final outcome, or an error message safe to send to the MCP client.
    ///
    /// `register_cancel` is invoked once, synchronously, before the prompt
    /// starts running, with a closure the caller can invoke from another
    /// thread to request cancellation.
    fn run_prompt(
        &self,
        prompt: &str,
        context: Option<&str>,
        on_progress: &mut dyn FnMut(&str),
        register_cancel: &mut dyn FnMut(CancelFn),
    ) -> Result<PromptOutcome, String>;
}

/// Awaits the next event from an [`harwness_sdk::EventStream`] without
/// pulling in a `StreamExt` crate: it only implements `futures_core::Stream`
/// (see `harwness-sdk/src/event.rs`), so a bare `poll_fn` is enough for the
/// one thing this module needs from it.
async fn next_event(stream: &mut harwness_sdk::EventStream) -> Option<harwness_sdk::SdkEvent> {
    std::future::poll_fn(|cx| futures_core::Stream::poll_next(std::pin::Pin::new(stream), cx)).await
}

/// Real [`PromptRunner`] backed by [`RunnerContext::harwness`].
struct HarwnessPromptRunner {
    ctx: Arc<RunnerContext>,
}

impl PromptRunner for HarwnessPromptRunner {
    fn run_prompt(
        &self,
        prompt: &str,
        context: Option<&str>,
        on_progress: &mut dyn FnMut(&str),
        register_cancel: &mut dyn FnMut(CancelFn),
    ) -> Result<PromptOutcome, String> {
        let harwness = self.ctx.harwness().map_err(|error| error.to_string())?;
        let full_prompt = match context.filter(|c| !c.is_empty()) {
            Some(context) => format!("{context}\n\n{prompt}"),
            None => prompt.to_owned(),
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;

        runtime.block_on(async move {
            let mut session = harwness.session().map_err(|error| error.to_string())?;
            let cancel_handle = session.cancel_handle();
            register_cancel(Box::new(move || {
                cancel_handle.cancel();
            }));

            let mut events = session.events();
            let progress_task = tokio::spawn(async move {
                let mut chunks = Vec::new();
                loop {
                    match next_event(&mut events).await {
                        Some(harwness_sdk::SdkEvent::TextDelta { text, source })
                            if source.is_root() =>
                        {
                            chunks.push(text);
                        }
                        Some(event) if event.is_root_finish() => break,
                        Some(_) => {}
                        None => break,
                    }
                }
                chunks
            });

            let report = session.send(full_prompt).await.map_err(|error| error.to_string())?;

            // The progress task owns the only remaining event receiver end
            // once `send` has returned (the turn is over), so this always
            // resolves promptly; a panic there must not sink the whole run.
            if let Ok(chunks) = progress_task.await {
                for chunk in chunks {
                    on_progress(&chunk);
                }
            }

            let status = match &report.status {
                harwness_sdk::TurnStatus::Completed => "completed",
                harwness_sdk::TurnStatus::Cancelled { .. } => "cancelled",
                harwness_sdk::TurnStatus::Truncated => "truncated",
                harwness_sdk::TurnStatus::Refused { .. } => "refused",
                harwness_sdk::TurnStatus::Failed { .. } => "failed",
            };
            Ok(PromptOutcome {
                status,
                text: report.text,
                input_tokens: report.usage.input_tokens,
                output_tokens: report.usage.output_tokens,
                tool_calls: report.tool_calls,
                approvals: report.approvals,
            })
        })
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC handler (pure; no I/O)
// ---------------------------------------------------------------------------

/// The bits of a [`RunnerContext`] the JSON-RPC handler actually needs,
/// pulled out once at construction time so the handler (and its tests) never
/// have to build or hold a real [`RunnerContext`].
#[derive(Debug, Clone)]
struct McpConfig {
    agent_name: String,
    agent_description: String,
    full_access: bool,
}

impl McpConfig {
    fn from_ctx(ctx: &RunnerContext) -> Self {
        let root = ctx.root_ir();
        let agent_name = root.name.clone().unwrap_or_else(|| root.id.name.clone());
        let agent_description = root
            .description
            .clone()
            .unwrap_or_else(|| format!("Run the compiled agent '{agent_name}'."));
        Self {
            agent_name,
            agent_description,
            full_access: ctx.args.flags.full_access,
        }
    }
}

struct McpServer {
    config: McpConfig,
    runner: Arc<dyn PromptRunner>,
    runs: Arc<RunRegistry>,
    initialized: Mutex<bool>,
}

impl McpServer {
    fn new(ctx: Arc<RunnerContext>, runner: Arc<dyn PromptRunner>) -> Self {
        Self::with_config(McpConfig::from_ctx(&ctx), runner)
    }

    fn with_config(config: McpConfig, runner: Arc<dyn PromptRunner>) -> Self {
        Self {
            config,
            runner,
            runs: Arc::new(RunRegistry::default()),
            initialized: Mutex::new(false),
        }
    }

    fn full_access(&self) -> bool {
        self.config.full_access
    }

    fn tool_descriptors(&self) -> Value {
        let name = &self.config.agent_name;
        let description = &self.config.agent_description;
        json!([
            {
                "name": "run",
                "description": description,
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "prompt": {
                            "type": "string",
                            "description": format!("The task or message to send to '{name}'.")
                        },
                        "context": {
                            "type": "string",
                            "description": "Optional extra context prepended to the prompt."
                        },
                        "background": {
                            "type": "boolean",
                            "description": "Run in the background; returns a run_id for status/cancel instead of waiting.",
                            "default": false
                        }
                    },
                    "required": ["prompt"],
                    "additionalProperties": false
                }
            },
            {
                "name": "status",
                "description": "Check the status of a background run started by 'run'.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "run_id": {"type": "string"}
                    },
                    "required": ["run_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "cancel",
                "description": "Cancel a background run started by 'run'.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "run_id": {"type": "string"}
                    },
                    "required": ["run_id"],
                    "additionalProperties": false
                }
            }
        ])
    }

    /// Handles one decoded JSON-RPC message. Shared by both transports
    /// (`serve_stdio` below and the HTTP `POST /mcp` handler in
    /// `http_transport`). `emit` is called with any out-of-band
    /// notifications the call produces (`notifications/progress` during a
    /// foreground `run`); stdio writes each straight to stdout as its own
    /// line, HTTP buffers them (see `http_transport::handle_post_mcp`).
    /// Returns `None` for a notification the spec forbids a response to;
    /// otherwise the JSON-RPC response object.
    fn handle_message(&self, message: &Value, emit: &mut dyn FnMut(Value)) -> Option<Value> {
        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str);

        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || method.is_none() {
            return Some(error_response(id, -32600, "invalid JSON-RPC request"));
        }
        let method = method.unwrap_or_default();
        let is_notification = id.is_none();

        let initialized = *self.initialized.lock().unwrap_or_else(|e| e.into_inner());
        if !initialized && matches!(method, "tools/list" | "tools/call") {
            return Some(error_response(id, -32002, "server not initialized"));
        }

        match method {
            "initialize" => Some(self.handle_initialize(id)),
            "notifications/initialized" => None,
            "ping" => Some(result_response(id, json!({}))),
            "tools/list" => Some(result_response(id, json!({"tools": self.tool_descriptors()}))),
            "tools/call" => {
                let response = self.handle_tools_call(id.clone(), message, emit);
                if is_notification { None } else { Some(response) }
            }
            "notifications/cancelled" => {
                if let Some(run_id) = message
                    .pointer("/params/requestId")
                    .and_then(Value::as_str)
                {
                    self.runs.cancel(run_id);
                }
                None
            }
            _ if is_notification => None,
            _ => Some(error_response(id, -32601, "method not found")),
        }
    }

    fn handle_initialize(&self, id: Option<Value>) -> Value {
        *self.initialized.lock().unwrap_or_else(|e| e.into_inner()) = true;
        result_response(
            id,
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {
                    "tools": {"listChanged": false}
                },
                "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION}
            }),
        )
    }

    fn handle_tools_call(
        &self,
        id: Option<Value>,
        message: &Value,
        emit: &mut dyn FnMut(Value),
    ) -> Value {
        let tool_name = message.pointer("/params/name").and_then(Value::as_str);
        let arguments = message
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let progress_token = message.pointer("/params/_meta/progressToken").cloned();

        match tool_name {
            Some("run") => self.tool_run(id, &arguments, progress_token, emit),
            Some("status") => self.tool_status(id, &arguments),
            Some("cancel") => self.tool_cancel(id, &arguments),
            Some(_) => error_response(id, -32601, "unknown tool"),
            None => error_response(id, -32602, "missing tool name"),
        }
    }

    fn tool_run(
        &self,
        id: Option<Value>,
        arguments: &Value,
        progress_token: Option<Value>,
        emit: &mut dyn FnMut(Value),
    ) -> Value {
        let Some(prompt) = arguments.get("prompt").and_then(Value::as_str) else {
            return error_response(id, -32602, "'prompt' is required and must be a string");
        };
        let context = arguments.get("context").and_then(Value::as_str);
        let background = arguments
            .get("background")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if background {
            return self.tool_run_background(id, prompt, context);
        }

        let runner = Arc::clone(&self.runner);
        let prompt_owned = prompt.to_owned();
        let context_owned = context.map(str::to_owned);
        let mut on_progress = |chunk: &str| {
            if let Some(token) = progress_token.clone() {
                emit(json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/progress",
                    "params": {
                        "progressToken": token,
                        "progress": 0,
                        "message": chunk,
                    }
                }));
            }
        };
        let mut register_cancel = |_cancel: CancelFn| {};
        match runner.run_prompt(
            &prompt_owned,
            context_owned.as_deref(),
            &mut on_progress,
            &mut register_cancel,
        ) {
            Ok(outcome) => result_response(id, self.run_result_content(&outcome)),
            Err(message) => error_response(id, -32000, &message),
        }
    }

    fn tool_run_background(
        &self,
        id: Option<Value>,
        prompt: &str,
        context: Option<&str>,
    ) -> Value {
        let run_id = self.runs.new_id();
        let runner = Arc::clone(&self.runner);
        let runs = Arc::clone(&self.runs);
        let prompt = prompt.to_owned();
        let context = context.map(str::to_owned);
        let run_id_for_thread = run_id.clone();

        // `register_cancel` is called synchronously before `run_prompt`
        // starts driving the turn, so the registry always has a cancel
        // closure installed before this call returns.
        let (cancel_tx, cancel_rx) = std::sync::mpsc::channel::<CancelFn>();
        std::thread::spawn(move || {
            let mut on_progress = |_: &str| {};
            let mut register_cancel = |cancel: CancelFn| {
                let _ = cancel_tx.send(cancel);
            };
            let outcome = runner.run_prompt(&prompt, context.as_deref(), &mut on_progress, &mut register_cancel);
            match outcome {
                Ok(outcome) => {
                    let state = if outcome.status == "cancelled" {
                        RunState::Cancelled
                    } else {
                        RunState::Completed
                    };
                    runs.complete(&run_id_for_thread, state, outcome.to_json());
                }
                Err(message) => {
                    runs.complete(
                        &run_id_for_thread,
                        RunState::Failed,
                        json!({"error": message}),
                    );
                }
            }
        });

        // Block briefly for the cancel closure so `cancel` works immediately
        // after `run` returns; a run that never registers one (e.g. it
        // failed before starting a session) simply has no way to be
        // cancelled, which `cancel` reports as "not found".
        if let Ok(cancel) = cancel_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            self.runs.insert_running(run_id.clone(), cancel);
        }

        result_response(
            id,
            json!({
                "content": [{"type": "text", "text": format!("started background run {run_id}")}],
                "run_id": run_id,
                "status": "running",
            }),
        )
    }

    fn run_result_content(&self, outcome: &PromptOutcome) -> Value {
        let mut structured = outcome.to_json();
        let mut text = outcome.text.clone().unwrap_or_default();
        if outcome.approvals > 0 && !self.full_access() {
            let note = "\n\n[harw-agent-runner] one or more tool calls required approval and \
                         were denied: MCP has no interactive approval channel. Re-run the \
                         compiled agent with --full-access to allow tools already permitted by \
                         its manifest.";
            text.push_str(note);
            if let Value::Object(map) = &mut structured {
                map.insert("approvals_denied".to_owned(), json!(true));
            }
        }
        json!({
            "content": [{"type": "text", "text": text}],
            "structuredContent": structured,
        })
    }

    fn tool_status(&self, id: Option<Value>, arguments: &Value) -> Value {
        let Some(run_id) = arguments.get("run_id").and_then(Value::as_str) else {
            return error_response(id, -32602, "'run_id' is required and must be a string");
        };
        match self.runs.status(run_id) {
            Some((state, result)) => result_response(
                id,
                json!({
                    "content": [{"type": "text", "text": format!("run {run_id}: {}", state.as_str())}],
                    "run_id": run_id,
                    "status": state.as_str(),
                    "result": result,
                }),
            ),
            None => error_response(id, -32602, "unknown run_id"),
        }
    }

    fn tool_cancel(&self, id: Option<Value>, arguments: &Value) -> Value {
        let Some(run_id) = arguments.get("run_id").and_then(Value::as_str) else {
            return error_response(id, -32602, "'run_id' is required and must be a string");
        };
        if self.runs.cancel(run_id) {
            result_response(
                id,
                json!({
                    "content": [{"type": "text", "text": format!("cancel requested for run {run_id}")}],
                    "run_id": run_id,
                }),
            )
        } else {
            error_response(id, -32602, "unknown run_id")
        }
    }

    /// Runs the newline-delimited JSON-RPC stdio loop until stdin closes.
    ///
    /// Each line is read through [`read_line_bounded`], so a peer that never
    /// sends `\n` (or sends one absurdly long line) cannot grow `line`
    /// without bound: [`MCP_STDIO_MAX_LINE_BYTES`] caps it, mirroring the
    /// HTTP transport's [`MCP_MAX_BODY_BYTES`].
    fn serve_stdio<R: BufRead, W: Write>(&self, mut input: R, output: &mut W) -> ExitCode {
        let mut line = String::new();
        loop {
            match read_line_bounded(&mut input, &mut line, MCP_STDIO_MAX_LINE_BYTES) {
                Ok(LineOutcome::Eof) => break,
                Ok(LineOutcome::Line) => {}
                Ok(LineOutcome::TooLong) => {
                    write_line(output, &error_response(None, -32700, "parse error: line too long"));
                    continue;
                }
                Err(error) => {
                    eprintln!("harw-agent-runner: stdio read error: {error}");
                    return ExitCode::FAILURE;
                }
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if json_nesting_too_deep(trimmed, MAX_JSON_NESTING_DEPTH) {
                write_line(output, &error_response(None, -32700, "parse error: too deeply nested"));
                continue;
            }
            let message: Value = match serde_json::from_str(trimmed) {
                Ok(value) => value,
                Err(_) => {
                    write_line(output, &error_response(None, -32700, "parse error"));
                    continue;
                }
            };
            let mut emit = |notification: Value| write_line(output, &notification);
            if let Some(response) = self.handle_message(&message, &mut emit) {
                write_line(output, &response);
            }
        }
        ExitCode::SUCCESS
    }
}

fn write_line<W: Write>(output: &mut W, value: &Value) {
    if let Ok(text) = serde_json::to_string(value) {
        let _ = writeln!(output, "{text}");
        let _ = output.flush();
    }
}

/// Upper bound on one stdio JSON-RPC line, mirroring the HTTP transport's
/// [`MCP_MAX_BODY_BYTES`]: without it, a peer that never sends `\n` (or sends
/// one absurdly long line) could grow `serve_stdio`'s buffer without bound —
/// `BufRead::read_line` itself has no length cap.
const MCP_STDIO_MAX_LINE_BYTES: usize = MCP_MAX_BODY_BYTES;

/// How a bounded line read ([`read_line_bounded`]) ended.
enum LineOutcome {
    /// End of input; nothing was read.
    Eof,
    /// A complete line (within the limit) is now in the caller's buffer.
    Line,
    /// A line exceeded the limit. It has already been fully drained from
    /// `input` (up to its newline, or to EOF) so the stream stays in sync;
    /// the caller's buffer is left empty rather than holding a partial,
    /// truncated line that could be mistaken for a complete message.
    TooLong,
}

/// Reads one `\n`-terminated line into `line` (which is cleared first),
/// refusing to grow it past `max_len` bytes. Unlike `BufRead::read_line`,
/// this never buffers more than `max_len` bytes of an oversized line before
/// reporting the problem — it keeps consuming (and discarding) bytes from
/// `input` until the line's terminator or EOF, so the caller can resume
/// reading the next line afterward instead of the stream being left
/// desynchronized.
fn read_line_bounded<R: BufRead>(
    input: &mut R,
    line: &mut String,
    max_len: usize,
) -> std::io::Result<LineOutcome> {
    line.clear();
    let mut buf: Vec<u8> = Vec::new();
    let mut overflowed = false;
    loop {
        let available = match input.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            return Ok(finish_bounded_line(line, buf, overflowed, true));
        }
        if let Some(newline_at) = available.iter().position(|&byte| byte == b'\n') {
            if !overflowed {
                if buf.len() + newline_at <= max_len {
                    buf.extend_from_slice(&available[..newline_at]);
                } else {
                    overflowed = true;
                }
            }
            input.consume(newline_at + 1);
            return Ok(finish_bounded_line(line, buf, overflowed, false));
        }
        if !overflowed {
            if buf.len() + available.len() <= max_len {
                buf.extend_from_slice(available);
            } else {
                overflowed = true;
            }
        }
        let consumed = available.len();
        input.consume(consumed);
    }
}

/// Finalizes a [`read_line_bounded`] read: fills `line` from `buf` (lossily,
/// same as any other place this crate turns untrusted bytes into a `String`
/// for a JSON-RPC parse attempt) unless the line overflowed, in which case
/// `line` stays empty and [`LineOutcome::TooLong`] is reported instead.
fn finish_bounded_line(line: &mut String, buf: Vec<u8>, overflowed: bool, at_eof: bool) -> LineOutcome {
    if overflowed {
        return LineOutcome::TooLong;
    }
    if at_eof && buf.is_empty() {
        return LineOutcome::Eof;
    }
    line.push_str(&String::from_utf8_lossy(&buf));
    LineOutcome::Line
}

fn result_response(id: Option<Value>, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

// ---------------------------------------------------------------------------
// Streamable HTTP transport (`--listen <addr>`)
// ---------------------------------------------------------------------------
//
// A small, self-contained loopback listener for [`McpServer::handle_message`]
// — not a reuse of `harw-mcp-server`'s supervisor-backed transport (see the
// module doc), and not a shared function with `iface::http` either (that
// module is not depended on from here; the auth/Origin checks below are a
// deliberate, small duplicate of its logic, kept in lock-step by convention).
//
// - `POST /mcp` takes one JSON-RPC request (batches are not supported: the
//   stdio transport this shares a handler with does not support them
//   either, so there is nothing to bridge).
// - `GET /healthz` is exempt from auth, like `iface::http`.
// - `Accept: text/event-stream` gets a (fully buffered, not chunked —
//   simple, since every notification is already known by the time a
//   synchronous `tools/call` returns) SSE response; otherwise the response
//   is always a single JSON object.
// - `initialize` issues an `Mcp-Session-Id`; every other request must
//   present it.

const MCP_MAX_BODY_BYTES: usize = 256 * 1024;
const MCP_BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// `Mcp-Session-Id`, as a `const` `HeaderName` (mirrors
/// `harw-mcp-client`'s `MCP_SESSION_HEADER`, independently — this module
/// does not depend on that crate).
const SESSION_HEADER: HeaderName = HeaderName::from_static("mcp-session-id");

type HttpResponse = Response<BoxBody<Bytes, Infallible>>;

/// Shared state for the HTTP transport: the same [`McpServer`] the stdio
/// loop would have used, plus the bits stdio has no equivalent of (the
/// bearer token and the one issued session id — this is a single-agent,
/// effectively single-client server, so one active session is enough).
struct McpHttpState {
    server: McpServer,
    token: Option<Vec<u8>>,
    session: Mutex<Option<String>>,
    session_counter: AtomicU64,
}

fn run_http(ctx: RunnerContext, listen: String) -> ExitCode {
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
    if let Err(reason) = validate_listen_requirements(addr, token.as_deref()) {
        eprintln!("harw-agent-runner: {reason}");
        return ExitCode::FAILURE;
    }

    let ctx = Arc::new(ctx);
    let server = McpServer::new(Arc::clone(&ctx), Arc::new(HarwnessPromptRunner { ctx }));
    let state = Arc::new(McpHttpState {
        server,
        token: token.map(String::into_bytes),
        session: Mutex::new(None),
        session_counter: AtomicU64::new(0),
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
    runtime.block_on(serve_http(addr, state))
}

/// A bind address requires a configured token unless it is loopback-only.
/// Mirrors `iface::http::validate_listen_requirements` (see this module's
/// doc for why that is a deliberate duplicate, not an import).
fn validate_listen_requirements(addr: SocketAddr, token: Option<&str>) -> Result<(), String> {
    if !addr.ip().is_loopback() && token.is_none() {
        return Err(
            "the MCP Streamable HTTP interface on a non-loopback address requires \
             HARW_AGENT_HTTP_TOKEN"
                .to_owned(),
        );
    }
    Ok(())
}

async fn serve_http(addr: SocketAddr, state: Arc<McpHttpState>) -> ExitCode {
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
                            service_fn(move |request| {
                                let state = Arc::clone(&state);
                                async move { handle_conn(state, request).await }
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

/// Reads a raw hyper request into the pieces [`dispatch_http`] needs.
async fn handle_conn(state: Arc<McpHttpState>, request: Request<Incoming>) -> Result<HttpResponse, Infallible> {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let headers = request.headers().clone();
    if path == "/healthz" {
        return Ok(dispatch_http(&state, &method, &path, &headers, Value::Null).await);
    }
    let body = match read_json_body(request, MCP_MAX_BODY_BYTES, MCP_BODY_READ_TIMEOUT).await {
        Ok(value) => value,
        Err(response) => return Ok(response),
    };
    Ok(dispatch_http(&state, &method, &path, &headers, body).await)
}

/// The transport-independent router: `/healthz`, then auth, then Origin,
/// then the one real route. Every test in this module calls this directly
/// instead of going through a real socket.
async fn dispatch_http(
    state: &McpHttpState,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    body: Value,
) -> HttpResponse {
    if path == "/healthz" {
        return json_response(StatusCode::OK, &json!({"status": "ok"}));
    }
    if !authorize(state, headers) {
        return json_response(StatusCode::UNAUTHORIZED, &json!({"error": "unauthorized"}));
    }
    if !origin_is_loopback(headers) {
        return json_response(StatusCode::FORBIDDEN, &json!({"error": "origin_not_allowed"}));
    }
    if *method == Method::POST && path == "/mcp" {
        handle_post_mcp(state, headers, body)
    } else {
        json_response(StatusCode::NOT_FOUND, &json!({"error": "not_found"}))
    }
}

/// Constant-time bearer check; `None` (no configured token) always passes.
/// Mirrors `iface::http::authorize`.
fn authorize(state: &McpHttpState, headers: &HeaderMap) -> bool {
    let Some(expected) = state.token.as_deref() else {
        return true;
    };
    let Some(header) = headers.get(AUTHORIZATION).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Some(presented) = header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))
    else {
        return false;
    };
    constant_time_eq(expected, presented.as_bytes())
}

/// Mirrors `iface::http::constant_time_eq`.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

/// Rejects a request whose `Origin` header does not name a loopback host —
/// MCP's Streamable HTTP transport recommends this to guard against DNS
/// rebinding attacks from a browser. A native MCP client normally omits
/// `Origin` entirely, so only a *supplied* one is checked (structurally the
/// same policy as `harw-mcp-server::transport::origin_is_loopback`,
/// reimplemented independently here rather than imported — see the module
/// doc).
fn origin_is_loopback(headers: &HeaderMap) -> bool {
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
    let authority = authority.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return false;
    }
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn new_session_id(state: &McpHttpState) -> String {
    let counter = state.session_counter.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("mcp-{nanos:x}-{counter}")
}

/// Requires a matching `Mcp-Session-Id` on every request but `initialize`
/// (which establishes the session in the first place). Returns the ready
/// error response when the check fails.
#[allow(clippy::result_large_err)]
fn check_session(state: &McpHttpState, headers: &HeaderMap, is_initialize: bool) -> Result<(), HttpResponse> {
    if is_initialize {
        return Ok(());
    }
    let provided = headers.get(&SESSION_HEADER).and_then(|value| value.to_str().ok());
    let Some(provided) = provided else {
        return Err(json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error": "missing_session", "detail": "Mcp-Session-Id header is required"}),
        ));
    };
    let current = state.session.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match current {
        Some(expected) if expected == provided => Ok(()),
        _ => Err(json_response(
            StatusCode::NOT_FOUND,
            &json!({"error": "unknown_session"}),
        )),
    }
}

fn handle_post_mcp(state: &McpHttpState, headers: &HeaderMap, body: Value) -> HttpResponse {
    let Some(method) = body.get("method").and_then(Value::as_str) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error": "invalid_request", "detail": "missing 'method'"}),
        );
    };
    let is_initialize = method == "initialize";
    if let Err(response) = check_session(state, headers, is_initialize) {
        return response;
    }

    let mut notifications = Vec::new();
    let mut emit = |notification: Value| notifications.push(notification);
    let response_value = state.server.handle_message(&body, &mut emit);

    let issued_session = if is_initialize && response_value.as_ref().is_some_and(|value| value.get("error").is_none()) {
        let id = new_session_id(state);
        *state.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.clone());
        Some(id)
    } else {
        None
    };

    let wants_sse = headers
        .get(ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"));

    let mut response = if wants_sse {
        sse_response(&notifications, response_value.as_ref())
    } else {
        match &response_value {
            Some(value) => json_response(StatusCode::OK, value),
            // A notification (no `id`): the spec calls for 202 with no body.
            None => empty_response(StatusCode::ACCEPTED),
        }
    };
    if let Some(id) = issued_session {
        if let Ok(value) = HeaderValue::from_str(&id) {
            response.headers_mut().insert(SESSION_HEADER, value);
        }
    }
    response
}

/// Buffers every notification plus the final response as one SSE body.
/// There is no live streaming here: by the time a synchronous
/// `tools/call` returns, every notification it will ever produce has
/// already been collected, so writing them all at once is both simple and
/// exactly as timely as a real stream would have been.
fn sse_response(notifications: &[Value], response: Option<&Value>) -> HttpResponse {
    let mut text = String::new();
    for notification in notifications {
        text.push_str(&format!("event: message\ndata: {notification}\n\n"));
    }
    if let Some(response) = response {
        text.push_str(&format!("event: message\ndata: {response}\n\n"));
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .body(Full::new(Bytes::from(text)).boxed())
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()).boxed()))
}

fn json_response(status: StatusCode, body: &Value) -> HttpResponse {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body.to_string())).boxed())
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()).boxed()))
}

fn empty_response(status: StatusCode) -> HttpResponse {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::new()).boxed())
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()).boxed()))
}

// The `Err` variant carries a fully-built `HttpResponse` (as
// `iface::http`'s equivalent helper does) so callers return it unchanged;
// it outlives this one call on the stack and is never cloned, so the size
// is not a real cost.
#[allow(clippy::result_large_err)]
async fn read_json_body(
    request: Request<Incoming>,
    max_bytes: usize,
    read_timeout: Duration,
) -> Result<Value, HttpResponse> {
    let declared_len = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared_len.is_some_and(|len| len > max_bytes as u64) {
        return Err(json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            &json!({"error": "payload_too_large"}),
        ));
    }
    let limited = Limited::new(request.into_body(), max_bytes);
    let bytes = match tokio::time::timeout(read_timeout, limited.collect()).await {
        Ok(Ok(collected)) => collected.to_bytes(),
        Ok(Err(error)) if error.downcast_ref::<LengthLimitError>().is_some() => {
            return Err(json_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                &json!({"error": "payload_too_large"}),
            ));
        }
        Ok(Err(_)) => {
            return Err(json_response(
                StatusCode::BAD_REQUEST,
                &json!({"error": "bad_request"}),
            ));
        }
        Err(_elapsed) => {
            return Err(json_response(
                StatusCode::REQUEST_TIMEOUT,
                &json!({"error": "request_timeout"}),
            ));
        }
    };
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    // Reject a shallow-but-deeply-nested body (e.g. megabytes of `[[[[...`)
    // before handing it to `serde_json`'s recursive-descent parser, which has
    // no depth limit of its own and can exhaust the stack on such input —
    // even within the `max_bytes` cap already enforced above.
    if crate::child_protocol::json_nesting_too_deep(&bytes, crate::child_protocol::MAX_JSON_NESTING_DEPTH) {
        return Err(json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error": "invalid_json", "detail": "too deeply nested"}),
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        json_response(
            StatusCode::BAD_REQUEST,
            &json!({"error": "invalid_json"}),
        )
    })
}

// ---------------------------------------------------------------------------
// Tests: pure JSON-RPC handler, no stdio, no real Harwness/tokio runtime;
// HTTP transport tests dispatch in-process (no real socket), same style as
// `iface::http`'s tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// A [`PromptRunner`] double that returns a canned outcome, records
    /// whether a cancel closure was registered, and lets tests simulate a
    /// long-running prompt by blocking on a flag.
    struct FakeRunner {
        outcome: Mutex<Option<Result<PromptOutcome, String>>>,
        cancel_registered: Arc<AtomicBool>,
        cancelled: Arc<AtomicBool>,
        block: Arc<AtomicBool>,
    }

    impl FakeRunner {
        fn echoing(reply: &str) -> Arc<Self> {
            Arc::new(Self {
                outcome: Mutex::new(Some(Ok(PromptOutcome {
                    status: "completed",
                    text: Some(reply.to_owned()),
                    input_tokens: 3,
                    output_tokens: 5,
                    tool_calls: 0,
                    approvals: 0,
                }))),
                cancel_registered: Arc::new(AtomicBool::new(false)),
                cancelled: Arc::new(AtomicBool::new(false)),
                block: Arc::new(AtomicBool::new(false)),
            })
        }

        fn blocking() -> Arc<Self> {
            let runner = Self::echoing("unused");
            runner.block.store(true, Ordering::SeqCst);
            runner
        }
    }

    impl PromptRunner for FakeRunner {
        fn run_prompt(
            &self,
            _prompt: &str,
            _context: Option<&str>,
            _on_progress: &mut dyn FnMut(&str),
            register_cancel: &mut dyn FnMut(CancelFn),
        ) -> Result<PromptOutcome, String> {
            self.cancel_registered.store(true, Ordering::SeqCst);
            let cancelled = Arc::clone(&self.cancelled);
            register_cancel(Box::new(move || cancelled.store(true, Ordering::SeqCst)));
            while self.block.load(Ordering::SeqCst) && !self.cancelled.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            self.outcome
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Err("outcome already taken".to_owned()))
        }
    }

    fn test_config() -> McpConfig {
        McpConfig {
            agent_name: "demo-agent".to_owned(),
            agent_description: String::new(),
            full_access: false,
        }
    }

    /// A server that has already completed the `initialize` handshake, for
    /// tests that only care about `tools/*` behavior.
    fn server_with(runner: Arc<dyn PromptRunner>) -> McpServer {
        let server = McpServer::with_config(test_config(), runner);
        let initialize = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}});
        server.handle_message(&initialize, &mut no_emit);
        server
    }

    fn no_emit(_: Value) {}

    #[test]
    fn initialize_advertises_the_protocol_version_and_server_info() {
        let server = McpServer::with_config(test_config(), FakeRunner::echoing("hi"));
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        let response = server.handle_message(&request, &mut no_emit).expect("a response");
        assert_eq!(response["id"], json!(1));
        assert_eq!(
            response["result"]["protocolVersion"],
            json!(MCP_PROTOCOL_VERSION)
        );
        assert_eq!(response["result"]["serverInfo"]["name"], json!(SERVER_NAME));
    }

    #[test]
    fn tools_list_exposes_run_status_and_cancel_using_the_root_ir() {
        let server = server_with(FakeRunner::echoing("hi"));
        let request = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let response = server.handle_message(&request, &mut no_emit).expect("a response");
        let tools = response["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(names, vec!["run", "status", "cancel"]);
        // Description comes straight from the root IR's own
        // name/description (via `McpConfig::from_ctx`); here it is whatever
        // `test_config()` set.
        assert_eq!(tools[0]["description"], json!(""));
        let prompt_description = tools[0]["inputSchema"]["properties"]["prompt"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(
            prompt_description.contains("demo-agent"),
            "prompt schema should mention the agent name: {prompt_description}"
        );
    }

    #[test]
    fn tools_call_run_with_the_offline_echo_returns_the_text() {
        let server = server_with(FakeRunner::echoing("echoed reply"));
        let request = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "run", "arguments": {"prompt": "hello"}}
        });
        let response = server.handle_message(&request, &mut no_emit).expect("a response");
        let content = &response["result"]["content"][0]["text"];
        assert_eq!(content, "echoed reply");
        assert_eq!(
            response["result"]["structuredContent"]["status"],
            json!("completed")
        );
        assert_eq!(
            response["result"]["structuredContent"]["tool_calls"],
            json!(0)
        );
    }

    #[test]
    fn tools_call_run_reports_denied_approvals_without_full_access() {
        let runner = FakeRunner::echoing("done");
        *runner.outcome.lock().unwrap() = Some(Ok(PromptOutcome {
            status: "completed",
            text: Some("done".to_owned()),
            input_tokens: 1,
            output_tokens: 1,
            tool_calls: 1,
            approvals: 1,
        }));
        let server = server_with(runner);
        let request = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {"name": "run", "arguments": {"prompt": "do the risky thing"}}
        });
        let response = server.handle_message(&request, &mut no_emit).expect("a response");
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(text.contains("--full-access"), "{text}");
        assert_eq!(
            response["result"]["structuredContent"]["approvals_denied"],
            json!(true)
        );
    }

    #[test]
    fn unknown_method_returns_method_not_found() {
        let server = server_with(FakeRunner::echoing("hi"));
        let request = json!({"jsonrpc": "2.0", "id": 5, "method": "not/a/real/method"});
        let response = server.handle_message(&request, &mut no_emit).expect("a response");
        assert_eq!(response["error"]["code"], json!(-32601));
    }

    #[test]
    fn background_run_can_be_cancelled_via_the_cancel_tool() {
        let runner = FakeRunner::blocking();
        let cancelled = Arc::clone(&runner.cancelled);
        let server = server_with(runner);

        let start = json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {"name": "run", "arguments": {"prompt": "long task", "background": true}}
        });
        let started = server.handle_message(&start, &mut no_emit).expect("a response");
        let run_id = started["result"]["run_id"]
            .as_str()
            .expect("run_id in background start response")
            .to_owned();
        assert_eq!(started["result"]["status"], json!("running"));

        // The background run's status is "running" until cancelled.
        let status = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {"name": "status", "arguments": {"run_id": run_id}}
        });
        let status_response = server.handle_message(&status, &mut no_emit).expect("a response");
        assert_eq!(status_response["result"]["status"], json!("running"));

        let cancel = json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {"name": "cancel", "arguments": {"run_id": run_id}}
        });
        let cancel_response = server.handle_message(&cancel, &mut no_emit).expect("a response");
        assert!(cancel_response.get("error").is_none(), "{cancel_response}");

        // Give the background thread a moment to observe the cancellation
        // flag and record the final state.
        for _ in 0..200 {
            if cancelled.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(cancelled.load(Ordering::SeqCst), "cancel closure should run");
    }

    #[test]
    fn status_for_an_unknown_run_id_is_an_error() {
        let server = server_with(FakeRunner::echoing("hi"));
        let request = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {"name": "status", "arguments": {"run_id": "no-such-run"}}
        });
        let response = server.handle_message(&request, &mut no_emit).expect("a response");
        assert_eq!(response["error"]["code"], json!(-32602));
    }

    // ── HTTP transport: in-process dispatch, no real socket ─────────────

    fn test_http_state(token: Option<&str>) -> McpHttpState {
        McpHttpState {
            server: McpServer::with_config(test_config(), FakeRunner::echoing("hi")),
            token: token.map(|value| value.as_bytes().to_vec()),
            session: Mutex::new(None),
            session_counter: AtomicU64::new(0),
        }
    }

    async fn body_bytes(response: HttpResponse) -> Value {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("in-memory body always resolves")
            .to_bytes();
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("handlers return JSON when a body is present")
        }
    }

    fn bearer(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        headers
    }

    #[test]
    fn non_loopback_bind_without_token_is_refused() {
        let addr: SocketAddr = "0.0.0.0:8787".parse().unwrap();
        assert!(validate_listen_requirements(addr, None).is_err());
    }

    #[test]
    fn non_loopback_bind_with_token_is_allowed() {
        let addr: SocketAddr = "0.0.0.0:8787".parse().unwrap();
        assert!(validate_listen_requirements(addr, Some("secret")).is_ok());
    }

    #[test]
    fn loopback_bind_without_token_is_allowed() {
        let addr: SocketAddr = "127.0.0.1:8787".parse().unwrap();
        assert!(validate_listen_requirements(addr, None).is_ok());
    }

    #[tokio::test]
    async fn initialize_over_http_issues_a_session_id() {
        let state = test_http_state(None);
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        let response = dispatch_http(&state, &Method::POST, "/mcp", &HeaderMap::new(), request).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers().get(&SESSION_HEADER).is_some(),
            "initialize should issue an Mcp-Session-Id"
        );
    }

    #[tokio::test]
    async fn tools_list_without_the_session_id_is_rejected() {
        let state = test_http_state(None);
        // Establish a session first, as a real client would, so the
        // rejection below is specifically about the missing header, not
        // about there being no session at all yet.
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        let init_response = dispatch_http(&state, &Method::POST, "/mcp", &HeaderMap::new(), init).await;
        assert_eq!(init_response.status(), StatusCode::OK);

        let request = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let response = dispatch_http(&state, &Method::POST, "/mcp", &HeaderMap::new(), request).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn tools_list_with_the_matching_session_id_succeeds() {
        let state = test_http_state(None);
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        let init_response = dispatch_http(&state, &Method::POST, "/mcp", &HeaderMap::new(), init).await;
        let session_id = init_response
            .headers()
            .get(&SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
            .expect("session id header")
            .to_owned();

        let mut headers = HeaderMap::new();
        headers.insert(SESSION_HEADER, HeaderValue::from_str(&session_id).unwrap());
        let request = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let response = dispatch_http(&state, &Method::POST, "/mcp", &headers, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = body_bytes(response).await;
        assert!(value["result"]["tools"].is_array());
    }

    #[tokio::test]
    async fn wrong_token_yields_401() {
        let state = test_http_state(Some("correct-token"));
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        let response = dispatch_http(&state, &Method::POST, "/mcp", &bearer("wrong"), request).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn correct_token_is_accepted() {
        let state = test_http_state(Some("correct-token"));
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        let response = dispatch_http(
            &state,
            &Method::POST,
            "/mcp",
            &bearer("correct-token"),
            request,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn healthz_bypasses_auth() {
        let state = test_http_state(Some("secret"));
        let response = dispatch_http(&state, &Method::GET, "/healthz", &HeaderMap::new(), Value::Null).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_bad_origin_is_refused() {
        let state = test_http_state(None);
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, HeaderValue::from_static("http://evil.example"));
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        let response = dispatch_http(&state, &Method::POST, "/mcp", &headers, request).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_loopback_origin_is_accepted() {
        let state = test_http_state(None);
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, HeaderValue::from_static("http://localhost:5173"));
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        let response = dispatch_http(&state, &Method::POST, "/mcp", &headers, request).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn sse_accept_header_returns_an_event_stream_body() {
        let state = test_http_state(None);
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json, text/event-stream"));
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        let response = dispatch_http(&state, &Method::POST, "/mcp", &headers, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("in-memory body always resolves")
            .to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("event: message"), "got: {text}");
        assert!(text.contains("\"protocolVersion\""), "got: {text}");
    }
}
