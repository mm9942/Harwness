//! MCP (Model Context Protocol) interface for the embedded agent runner.
//!
//! # Scope
//! Implements the tool-calling half of MCP for a single compiled agent:
//! `initialize`, `tools/list`, `tools/call` (`run`, `status`, `cancel`),
//! `notifications/cancelled` and `ping`. Only the **stdio** transport is
//! implemented here (newline-delimited JSON-RPC 2.0 over stdin/stdout, MCP
//! protocol version [`MCP_PROTOCOL_VERSION`]).
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
//! JSON-RPC 2.0 loop.
//!
//! `--listen <addr>` (Streamable HTTP) is consequently **not implemented**:
//! [`run`] prints why and exits with failure instead of silently ignoring
//! the flag. See the module doc above and the wave-3B report for the
//! rationale; a future HTTP transport for this interface should be its own
//! loopback listener, not a reuse of `harw-mcp-server`'s.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

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
/// Runs the stdio JSON-RPC loop to completion (client closed stdin, or a
/// fatal I/O error) and returns the process exit code.
pub fn run(ctx: RunnerContext) -> ExitCode {
    if let Some(addr) = ctx.args.listen.clone() {
        eprintln!(
            "harw-agent-runner: MCP Streamable HTTP transport (--listen {addr}) is not \
             implemented in this build. harw-mcp-server's Streamable HTTP transport is built \
             around a durable job supervisor and tenant/workspace principals that this \
             embedded, single-agent runner does not have (see harw-agent-runner/src/iface/mcp.rs \
             module docs). Re-run without --listen to use the stdio transport."
        );
        return ExitCode::FAILURE;
    }

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

/// Registry of background runs, keyed by a runner-issued `run_id`.
#[derive(Default)]
struct RunRegistry {
    next_id: AtomicU64,
    runs: Mutex<HashMap<String, TrackedRun>>,
}

impl RunRegistry {
    fn new_id(&self) -> String {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        format!("run-{n}")
    }

    fn insert_running(&self, id: String, cancel: Box<dyn Fn() + Send + Sync>) {
        self.runs.lock().unwrap_or_else(|e| e.into_inner()).insert(
            id,
            TrackedRun {
                state: RunState::Running,
                result: None,
                cancel: Some(cancel),
            },
        );
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

    /// Handles one decoded JSON-RPC message. `emit` is called with any
    /// out-of-band notifications the call produces (`notifications/progress`
    /// during a foreground `run`). Returns `None` for a notification the
    /// spec forbids a response to; otherwise the JSON-RPC response object.
    fn handle(&self, message: &Value, emit: &mut dyn FnMut(Value)) -> Option<Value> {
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
    fn serve_stdio<R: BufRead, W: Write>(&self, mut input: R, output: &mut W) -> ExitCode {
        let mut line = String::new();
        loop {
            line.clear();
            match input.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) => {
                    eprintln!("harw-agent-runner: stdio read error: {error}");
                    return ExitCode::FAILURE;
                }
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
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
            if let Some(response) = self.handle(&message, &mut emit) {
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

fn result_response(id: Option<Value>, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

// ---------------------------------------------------------------------------
// Tests: pure JSON-RPC handler, no stdio, no real Harwness/tokio runtime.
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
        server.handle(&initialize, &mut no_emit);
        server
    }

    fn no_emit(_: Value) {}

    #[test]
    fn initialize_advertises_the_protocol_version_and_server_info() {
        let server = McpServer::with_config(test_config(), FakeRunner::echoing("hi"));
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        let response = server.handle(&request, &mut no_emit).expect("a response");
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
        let response = server.handle(&request, &mut no_emit).expect("a response");
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
        let response = server.handle(&request, &mut no_emit).expect("a response");
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
        let response = server.handle(&request, &mut no_emit).expect("a response");
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
        let response = server.handle(&request, &mut no_emit).expect("a response");
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
        let started = server.handle(&start, &mut no_emit).expect("a response");
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
        let status_response = server.handle(&status, &mut no_emit).expect("a response");
        assert_eq!(status_response["result"]["status"], json!("running"));

        let cancel = json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {"name": "cancel", "arguments": {"run_id": run_id}}
        });
        let cancel_response = server.handle(&cancel, &mut no_emit).expect("a response");
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
        let response = server.handle(&request, &mut no_emit).expect("a response");
        assert_eq!(response["error"]["code"], json!(-32602));
    }
}
