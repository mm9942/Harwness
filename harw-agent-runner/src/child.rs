//! `--child <id> --child-protocol stdio`: this process is one child agent of
//! a compiled parent (`docs/adr/0001-agent-compiler.md`).
//!
//! # Description
//! [`run_child`] drives the named agent's own `harwness_sdk::Session`,
//! translating its live events, approvals and result into
//! `harwness.agent-child/v1` frames (`crate::child_protocol`) written to
//! stdout, and applying the parent's `Task`/`Message`/`Answer`/`Cancel`/
//! `Budget`/`Mode`/`Rights` frames read from stdin. stderr carries logs only
//! — never a protocol frame.
//!
//! The counterpart that starts this process and speaks the other end of the
//! protocol is `crate::job_child_backend::JobChildBackend`.
//!
//! # Rights
//! A child's effective rights are `child manifest ∩ this process's own
//! flags ∩ the parent's Rights frame(s)` ([`own_ceiling`], then
//! [`effective_child_rights`], both built on
//! `harw_runtime::embedded::EffectiveRights::{narrowed_by, intersect}`).
//! This only ever narrows: a parent can restrict a child further than its
//! manifest, never grant it more, and several `Rights` frames before the
//! `Task` narrow each other ([`narrow_rights`]). Automatic approval
//! (`full_access`) is the one right the child's own command line never
//! carries (a job child is started without `--full-access`), so the ceiling
//! allows it and only the parent's grant switches it on; when effective, the
//! child's session runs with `ApprovalPolicy::FullAccess` instead of relaying
//! approvals.
//!
//! # Continuation
//! A turn the budget ends (`TurnStatus::Cancelled { reason: "budget" }`)
//! reports `BudgetExhausted` with its SDK session id as the `Result` frame's
//! `continuation`. A later `Task` naming that token in `continue_from`
//! resumes the stored session (`Harwness::resume`, same `HARW_HOME`) instead
//! of starting a fresh one.
//!
//! # Frame size
//! stdin is read through `crate::child_protocol::FrameReader`, bounded by
//! `MAX_FRAME_BYTES`: an oversized (or non-UTF-8) frame is answered with an
//! `Error` frame and the process exits with a failure, without a `Result`.
//!
//! # Testability
//! The pure translation ([`translate_event`], [`translate_turn_report`],
//! [`narrow_rights`]) is factored out of the stdio loop so it can be
//! exercised against a real (but offline) `harwness_sdk::Session` without a
//! child process, a parent, or a bundle on disk — see the tests below.

#[cfg(test)]
use std::collections::BTreeSet;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use harw_agent_dsl::ir_v2::{Budget, Permissions};
use harw_runtime::embedded::{EffectiveRights, RightsFlags};
#[cfg(test)]
use harwness_sdk::Harwness;
use harwness_sdk::{
    ApprovalHandler, ApprovalPolicy, ApprovalRequest, BoxFuture, Decision, FinishStatus, SdkEvent,
    TurnReport, TurnStatus,
};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::child_protocol::{
    ChildResultStatus, ChildRights, ChildToParent, ChildUsage, FrameReadError, FrameReader,
    PROTOCOL_VERSION, ParentToChild, decode_line, encode_line,
};

/// The child's stdin as a bounded frame reader.
type StdinFrames = FrameReader<BufReader<tokio::io::Stdin>>;
use crate::context::RunnerContext;

/// Runs this process as the child named `agent_id` in `ctx`'s bundle,
/// speaking `harwness.agent-child/v1` over stdio.
///
/// # Returns
/// `ExitCode::SUCCESS` after a regular `Result` frame (whatever its
/// [`ChildResultStatus`]) or a clean `Cancel`/stdin close; `ExitCode::FAILURE`
/// if `agent_id` is not in the bundle, the runtime cannot start, or the
/// session cannot be built.
#[must_use]
pub fn run_child(ctx: RunnerContext, agent_id: &str) -> ExitCode {
    let agent_id = agent_id.to_owned();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("harw-agent-runner: child runtime failed to start: {error}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(run_child_async(ctx, agent_id))
}

async fn run_child_async(mut ctx: RunnerContext, agent_id: String) -> ExitCode {
    let Some(child_ir) = ctx.agent.agent_ir(&agent_id).cloned() else {
        eprintln!("harw-agent-runner: unknown child agent id '{agent_id}' in this bundle");
        return ExitCode::FAILURE;
    };

    let stdout = tokio::io::BufWriter::new(tokio::io::stdout());
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel::<ChildToParent>();
    let writer = tokio::spawn(run_writer(stdout, outbound_rx));

    if outbound_tx
        .send(ChildToParent::Hello {
            agent_id: agent_id.clone(),
            protocol: PROTOCOL_VERSION.to_owned(),
            digest: ctx.agent.digest().to_string(),
        })
        .is_err()
    {
        eprintln!("harw-agent-runner: child writer died before hello");
        return ExitCode::FAILURE;
    }

    let mut stdin: StdinFrames = FrameReader::new(BufReader::new(tokio::io::stdin()));
    let mut parent_rights: Option<ChildRights> = None;
    let mut budget: Option<Budget> = None;
    let mut mode = harwness_sdk::Mode::Plan;

    // The first `Task` starts the run; `Rights`/`Budget`/`Mode` may arrive
    // before it and are applied once the session exists. `Cancel` or a
    // closed stdin before any `Task` ends this process cleanly — there is
    // nothing to report a result for.
    let (task, context, continue_from) = loop {
        let line = match stdin.next_frame().await {
            Ok(Some(line)) => line,
            Ok(None) => return ExitCode::SUCCESS,
            Err(FrameReadError::Io(error)) => {
                eprintln!("harw-agent-runner: failed reading stdin: {error}");
                return ExitCode::FAILURE;
            }
            Err(error) => {
                // Oversized or non-UTF-8: the stream is out of sync, so
                // report it once and stop instead of guessing where the next
                // frame starts.
                return fail_with(outbound_tx, writer, error.to_string()).await;
            }
        };
        match decode_line::<ParentToChild>(&line) {
            Ok(ParentToChild::Task {
                task,
                context,
                continue_from,
            }) => {
                if parent_rights.is_none() {
                    return fail_with(outbound_tx, writer, "Rights must precede Task".to_owned())
                        .await;
                }
                break (task, context, continue_from);
            }
            Ok(ParentToChild::Rights {
                rights: from_parent,
            }) => {
                // Several frames only ever narrow each other.
                parent_rights = Some(match parent_rights.take() {
                    Some(previous) => narrow_rights(&previous, &from_parent),
                    None => from_parent,
                });
            }
            Ok(ParentToChild::Cancel) => return ExitCode::SUCCESS,
            Ok(ParentToChild::Budget {
                max_tokens,
                max_tool_calls,
                max_wall_time_ms,
            }) => {
                let mut next = budget.take().unwrap_or_default();
                next.max_tokens = min_limit(next.max_tokens, max_tokens);
                next.max_tool_calls = min_limit(
                    next.max_tool_calls,
                    max_tool_calls.map(|v| u32::try_from(v).unwrap_or(u32::MAX)),
                );
                next.max_wall_secs =
                    min_limit(next.max_wall_secs, max_wall_time_ms.map(|v| v / 1_000));
                budget = Some(next);
            }
            Ok(ParentToChild::Mode { mode: next }) => {
                mode = match next {
                    crate::child_protocol::ChildMode::Plan => harwness_sdk::Mode::Plan,
                    crate::child_protocol::ChildMode::Live => harwness_sdk::Mode::Work,
                };
            }
            Ok(_) => {}
            Err(error) => {
                let _ = outbound_tx.send(ChildToParent::Error {
                    message: error.to_string(),
                });
            }
        }
    };
    // `parent_rights` is always `Some` here: the loop refuses a `Task`
    // before any `Rights` frame.
    let Some(parent_rights) = parent_rights else {
        return fail_with(outbound_tx, writer, "Rights must precede Task".to_owned()).await;
    };
    let rights = effective_child_rights(
        own_ceiling(&child_ir.permissions, &ctx.args.flags),
        &parent_rights,
        budget.as_ref(),
    );
    let selected = match ctx.agent.for_agent(&agent_id) {
        Ok(agent) => agent,
        Err(error) => {
            return fail_with(outbound_tx, writer, format!("cannot select child: {error}")).await;
        }
    };
    let auto_approve = rights.full_access;
    // `with_rights` intersects with what `for_agent` already carries, so
    // this can only narrow further.
    ctx.agent = Arc::new(selected.with_rights(rights));
    let approvals = Arc::new(RelayApprovals::new(outbound_tx.clone()));
    let harwness = match ctx.builder().and_then(|builder| {
        let builder = builder
            .mode(mode)
            .approval_handler_arc(approvals.clone());
        let builder = if auto_approve {
            builder.approval_policy(ApprovalPolicy::FullAccess)
        } else {
            builder
        };
        builder.build().map_err(crate::error::RunnerError::from)
    }) {
        Ok(harwness) => harwness,
        Err(error) => {
            // `approvals` holds a sender too; the writer only ends once
            // every sender is gone.
            drop(approvals);
            return fail_with(
                outbound_tx,
                writer,
                format!("failed to build the child session: {error}"),
            )
            .await;
        }
    };
    let opened = match continue_from.as_deref() {
        Some(token) => match token.parse::<harwness_sdk::SessionId>() {
            Ok(id) => harwness.resume(&id).await,
            Err(error) => Err(error),
        },
        None => harwness.session(),
    };
    let mut session = match opened {
        Ok(session) => session,
        Err(error) => {
            drop(harwness);
            drop(approvals);
            let what = if continue_from.is_some() {
                "resume"
            } else {
                "start"
            };
            return fail_with(
                outbound_tx,
                writer,
                format!("failed to {what} the child session: {error}"),
            )
            .await;
        }
    };

    let mut events = session.events();
    let event_forwarder = {
        let outbound_tx = outbound_tx.clone();
        tokio::spawn(async move {
            while let Some(event) = events.next().await {
                if outbound_tx
                    .send(ChildToParent::Event {
                        sdk_event: translate_event(&event),
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
    };

    let turn_text = if let Some(context) = &context {
        format!("{task}\n\n{context}")
    } else {
        task
    };
    let cancel_handle = session.cancel_handle();
    // Reuses the same `stdin` the handshake loop read from — a second,
    // independent reader over the same fd would race it for bytes.
    let reader = tokio::spawn(run_reader(
        stdin,
        approvals,
        cancel_handle,
        outbound_tx.clone(),
    ));

    let result = session.send(turn_text).await;
    event_forwarder.abort();
    reader.abort();
    let _ = event_forwarder.await;
    // `Ok(true)`: the reader already reported a broken stream with an
    // `Error` frame and cancelled the turn — no `Result` follows it.
    let protocol_broken = matches!(reader.await, Ok(true));
    drop(session);
    drop(harwness);
    if protocol_broken {
        drop(outbound_tx);
        let _ = writer.await;
        return ExitCode::FAILURE;
    }

    let frame = match result {
        Ok(report) => translate_turn_report(&report),
        Err(error) => ChildToParent::Result {
            status: ChildResultStatus::Failed,
            text: Some(error.to_string()),
            usage: ChildUsage::default(),
            continuation: None,
        },
    };
    let _ = outbound_tx.send(frame);
    drop(outbound_tx);
    let _ = writer.await;
    ExitCode::SUCCESS
}

/// Sends one `Error` frame, lets the writer flush it, and returns
/// `ExitCode::FAILURE` — the child's single way out once it cannot produce a
/// `Result`.
async fn fail_with(
    outbound: mpsc::UnboundedSender<ChildToParent>,
    writer: tokio::task::JoinHandle<()>,
    message: String,
) -> ExitCode {
    eprintln!("harw-agent-runner: {message}");
    let _ = outbound.send(ChildToParent::Error { message });
    drop(outbound);
    let _ = writer.await;
    ExitCode::FAILURE
}

/// Drains the rest of `stdin` for `Answer`/`Cancel` frames that arrive while
/// the turn runs, resolving pending approvals and cancelling the session.
///
/// # Returns
/// `true` if it stopped on a protocol violation (oversized, non-UTF-8,
/// malformed or unsupported frame), after sending an `Error` frame and
/// cancelling the turn; `false` on `Cancel` or a closed/failed stdin.
async fn run_reader(
    mut stdin: StdinFrames,
    approvals: Arc<RelayApprovals>,
    cancel: harwness_sdk::CancelHandle,
    outbound: mpsc::UnboundedSender<ChildToParent>,
) -> bool {
    loop {
        let message = match stdin.next_frame().await {
            Ok(Some(line)) => match decode_line::<ParentToChild>(&line) {
                Ok(ParentToChild::Answer { question_id, text }) => {
                    approvals.resolve(&question_id, text);
                    continue;
                }
                Ok(ParentToChild::Cancel) => {
                    cancel.cancel();
                    return false;
                }
                Ok(_) | Err(_) => {
                    "unsupported or malformed mid-turn control frame; child cancelled".to_owned()
                }
            },
            Ok(None) | Err(FrameReadError::Io(_)) => {
                cancel.cancel();
                return false;
            }
            Err(error) => format!("{error}; child cancelled"),
        };
        let _ = outbound.send(ChildToParent::Error { message });
        cancel.cancel();
        return true;
    }
}

/// Writes every frame it receives as one protocol line to `stdout`, flushing
/// after each — the parent must see a `Question`/`ApprovalRequest` before it
/// can answer it.
async fn run_writer(
    mut stdout: tokio::io::BufWriter<tokio::io::Stdout>,
    mut inbound: mpsc::UnboundedReceiver<ChildToParent>,
) {
    while let Some(frame) = inbound.recv().await {
        let Ok(line) = encode_line(&frame) else {
            continue;
        };
        if stdout.write_all(line.as_bytes()).await.is_err() {
            break;
        }
        if stdout.flush().await.is_err() {
            break;
        }
    }
}

fn min_limit<T: Ord>(left: Option<T>, right: Option<T>) -> Option<T> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// This process's own ceiling for the child: its manifest narrowed by the
/// command-line flags (`--deny-tool`, `--no-network`, `--read-only`,
/// `--max-tokens`).
///
/// `full_access` is taken from the manifest ceiling (`true`), not from the
/// own flags: a job child is never started with `--full-access`, so ANDing
/// that flag in here would make the parent's grant unreachable. Whether
/// automatic approval is effective is decided by the parent's `Rights`
/// frame in [`effective_child_rights`] — never wider than the parent.
fn own_ceiling(permissions: &Permissions, flags: &RightsFlags) -> EffectiveRights {
    let flags = RightsFlags {
        full_access: true,
        ..flags.clone()
    };
    EffectiveRights::from_manifest(permissions).narrowed_by(&flags)
}

/// `ceiling ∩ from_parent`, with `budget` (the parent's `Budget` frames) as
/// the parent's resource limits: sets intersect, every boolean — including
/// `full_access` — is AND'd, and each budget limit takes the stricter value
/// (`EffectiveRights::intersect`).
fn effective_child_rights(
    ceiling: EffectiveRights,
    from_parent: &ChildRights,
    budget: Option<&Budget>,
) -> EffectiveRights {
    ceiling.intersect(&EffectiveRights {
        tools: from_parent.tools.clone(),
        network_hosts: from_parent.network_hosts.clone(),
        network_open: from_parent.network_open,
        write: from_parent.write,
        shell: from_parent.shell,
        host: from_parent.host,
        full_access: from_parent.full_access,
        budget: budget.cloned(),
    })
}

/// `min(previous, next)` over two parent `Rights` frames: tools/hosts
/// intersect, every boolean right is AND'd. A later frame can only narrow.
fn narrow_rights(manifest: &ChildRights, from_parent: &ChildRights) -> ChildRights {
    ChildRights {
        tools: manifest
            .tools
            .intersection(&from_parent.tools)
            .cloned()
            .collect(),
        network_hosts: manifest
            .network_hosts
            .intersection(&from_parent.network_hosts)
            .cloned()
            .collect(),
        network_open: manifest.network_open && from_parent.network_open,
        write: manifest.write && from_parent.write,
        shell: manifest.shell && from_parent.shell,
        host: manifest.host && from_parent.host,
        full_access: manifest.full_access && from_parent.full_access,
    }
}

/// Translates one live [`SdkEvent`] into the JSON `crate::child_protocol`
/// carries as `Event.sdk_event`. Pure and side-effect free.
///
/// # Description
/// `harwness_sdk::SdkEvent` does not implement `Serialize` (it is
/// `#[non_exhaustive]` and stays that way on purpose), so this is a manual,
/// explicit mapping rather than a derive — every variant produces a small
/// JSON object tagged by `"kind"`.
pub(crate) fn translate_event(event: &SdkEvent) -> serde_json::Value {
    fn source_json(source: &harwness_sdk::EventSource) -> serde_json::Value {
        serde_json::json!({
            "session_id": source.session_id.to_string(),
            "parent": source.parent.as_ref().map(ToString::to_string),
            "role": source.role,
        })
    }
    match event {
        SdkEvent::TurnStarted { source, turn_id } => serde_json::json!({
            "kind": "turn_started", "source": source_json(source), "turn_id": turn_id,
        }),
        SdkEvent::TextDelta { source, text } => serde_json::json!({
            "kind": "text_delta", "source": source_json(source), "text": text,
        }),
        SdkEvent::ReasoningDelta { source, text } => serde_json::json!({
            "kind": "reasoning_delta", "source": source_json(source), "text": text,
        }),
        SdkEvent::Message {
            source,
            text,
            final_answer,
        } => serde_json::json!({
            "kind": "message", "source": source_json(source), "text": text,
            "final_answer": final_answer,
        }),
        SdkEvent::ToolCall {
            source,
            call_id,
            tool,
            arguments,
        } => serde_json::json!({
            "kind": "tool_call", "source": source_json(source), "call_id": call_id,
            "tool": tool, "arguments": arguments,
        }),
        SdkEvent::ToolResult {
            source,
            call_id,
            output,
            duration,
        } => serde_json::json!({
            "kind": "tool_result", "source": source_json(source), "call_id": call_id,
            "success": output.is_success(), "duration_ms": duration.as_millis() as u64,
        }),
        SdkEvent::ChildSpawned {
            source,
            child,
            role,
            task,
        } => serde_json::json!({
            "kind": "child_spawned", "source": source_json(source), "child": child.to_string(),
            "role": role, "task": task,
        }),
        SdkEvent::ChildCompleted {
            source,
            child,
            outcome,
            duration,
        } => serde_json::json!({
            "kind": "child_completed", "source": source_json(source), "child": child.to_string(),
            "outcome": outcome, "duration_ms": duration.as_millis() as u64,
        }),
        SdkEvent::Usage {
            source,
            round,
            turn_total,
            final_round,
        } => serde_json::json!({
            "kind": "usage", "source": source_json(source),
            "round_tokens": round.total(), "turn_total_tokens": turn_total.total(),
            "final_round": final_round,
        }),
        SdkEvent::Context {
            source,
            used_tokens,
            window_tokens,
        } => serde_json::json!({
            "kind": "context", "source": source_json(source), "used_tokens": used_tokens,
            "window_tokens": window_tokens,
        }),
        SdkEvent::Error {
            source,
            message,
            retryable,
        } => serde_json::json!({
            "kind": "error", "source": source_json(source), "message": message,
            "retryable": retryable,
        }),
        SdkEvent::Finished {
            source,
            status,
            usage,
        } => serde_json::json!({
            "kind": "finished", "source": source_json(source),
            "status": matches!(status, FinishStatus::Completed).then_some("completed").unwrap_or("aborted"),
            "usage_tokens": usage.as_ref().map(harwness_sdk::Usage::total),
        }),
        SdkEvent::Lagged { skipped } => serde_json::json!({
            "kind": "lagged", "skipped": skipped,
        }),
        // `SdkEvent` is `#[non_exhaustive]`: a later SDK version's new
        // variant still becomes a valid (if uninformative) JSON frame,
        // matching `iface::cli`/`iface::http`'s equivalent translators.
        _ => serde_json::json!({"kind": "unknown"}),
    }
}

/// Translates a completed [`TurnReport`] into the child's `Result` frame.
/// Pure and side-effect free.
///
/// A turn the budget cancelled becomes `BudgetExhausted` and carries the
/// session id as its `continuation` token (see the module doc); every other
/// end carries none.
pub(crate) fn translate_turn_report(report: &TurnReport) -> ChildToParent {
    let status = match &report.status {
        TurnStatus::Completed => ChildResultStatus::Completed,
        TurnStatus::Cancelled { reason } if reason == "budget" => {
            ChildResultStatus::BudgetExhausted
        }
        TurnStatus::Cancelled { .. } => ChildResultStatus::Cancelled,
        TurnStatus::Truncated | TurnStatus::Refused { .. } | TurnStatus::Failed { .. } => {
            ChildResultStatus::Failed
        }
        // `TurnStatus` is `#[non_exhaustive]`; treat anything future as the
        // same conservative outcome as the other named failure variants
        // above, consistent with `iface::mcp`'s equivalent match.
        _ => ChildResultStatus::Failed,
    };
    let continuation =
        (status == ChildResultStatus::BudgetExhausted).then(|| report.session_id.to_string());
    ChildToParent::Result {
        status,
        text: report.text.clone(),
        continuation,
        usage: ChildUsage {
            input_tokens: report.usage.input_tokens,
            output_tokens: report.usage.output_tokens,
            cached_input_tokens: report.usage.cached_tokens.unwrap_or(0),
            tool_calls: u64::from(report.tool_calls),
            wall_time_ms: 0,
        },
    }
}

/// Bridges `harwness_sdk::ApprovalHandler` to the protocol: every held tool
/// call becomes an `ApprovalRequest` frame, resolved by a matching `Answer`.
struct RelayApprovals {
    outbound: mpsc::UnboundedSender<ChildToParent>,
    pending: Mutex<std::collections::HashMap<String, oneshot::Sender<Decision>>>,
    next_id: AtomicU64,
}

impl RelayApprovals {
    fn new(outbound: mpsc::UnboundedSender<ChildToParent>) -> Self {
        Self {
            outbound,
            pending: Mutex::new(std::collections::HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Resolves a pending approval by id. `text == "approve"` approves;
    /// anything else denies with `text` as the reason (the convention
    /// documented on `ParentToChild::Answer`).
    fn resolve(&self, id: &str, text: String) {
        let sender = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
        if let Some(sender) = sender {
            let decision = if text == "approve" {
                Decision::Approve
            } else {
                Decision::deny(text)
            };
            let _ = sender.send(decision);
        }
    }
}

impl ApprovalHandler for RelayApprovals {
    fn decide<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Decision> {
        Box::pin(async move {
            let id = format!("appr-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
            let (tx, rx) = oneshot::channel();
            self.pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id.clone(), tx);
            let sent = self.outbound.send(ChildToParent::ApprovalRequest {
                id: id.clone(),
                tool: request.tool.clone(),
                args_summary: summarize_arguments(&request.arguments),
            });
            if sent.is_err() {
                return Decision::deny("parent channel closed");
            }
            rx.await
                .unwrap_or_else(|_| Decision::deny("parent closed without answering"))
        })
    }
}

/// A short, human-readable summary of a tool call's arguments — never the
/// raw JSON verbatim, to keep the frame small.
fn summarize_arguments(arguments: &serde_json::Value) -> String {
    const MAX_LEN: usize = 200;
    let mut text = arguments.to_string();
    if text.len() > MAX_LEN {
        text.truncate(MAX_LEN);
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use harwness_sdk::Session;
    use std::path::{Path, PathBuf};

    struct Fixture {
        _dir: tempfile::TempDir,
        home: PathBuf,
        project: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
        write_fixture_uia(&home);
        Fixture {
            _dir: dir,
            home,
            project,
        }
    }

    fn write_fixture_uia(home: &Path) {
        let profile_dir = home.join("profiles").join("default");
        let agent_dir = profile_dir.join("agents").join("fixture-uia");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("definition.toml"),
            "schema = \"harwness.agent/v1\"\nid = \"harwness.agent.fixture-uia@1\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\n",
        )
        .unwrap();
        std::fs::write(
            profile_dir.join("config.toml"),
            "active_uia_definition = \"harwness.agent.fixture-uia@1\"\n",
        )
        .unwrap();
    }

    /// Builds a real (offline) session so `translate_event`/
    /// `translate_turn_report` are exercised against genuine
    /// `harwness_sdk` types — those are `#[non_exhaustive]` and cannot be
    /// hand-built outside their own crate, so an "injected session" here
    /// means an offline-echo backend standing in for a model, not a mock.
    fn offline_session(fixture: &Fixture) -> Session {
        Harwness::builder()
            .home(&fixture.home)
            .cwd(&fixture.project)
            .scaffold_home(false)
            .ephemeral(true)
            .offline_echo("pong")
            .build()
            .expect("build")
            .session()
            .expect("session")
    }

    #[tokio::test]
    async fn translate_turn_report_maps_a_completed_turn() {
        let fixture = fixture();
        let mut session = offline_session(&fixture);
        let report = session.send("ping").await.expect("send");

        let frame = translate_turn_report(&report);
        match frame {
            ChildToParent::Result { status, text, .. } => {
                assert_eq!(status, ChildResultStatus::Completed);
                assert_eq!(text.as_deref(), Some("pong"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn translate_event_covers_a_real_finished_event() {
        let fixture = fixture();
        let mut session = offline_session(&fixture);
        let mut events = session.events();
        let _report = session.send("ping").await.expect("send");

        let mut saw_finished = false;
        while let Ok(Some(event)) =
            tokio::time::timeout(std::time::Duration::from_millis(200), events.next()).await
        {
            let json = translate_event(&event);
            assert!(json.get("kind").is_some(), "every event carries a kind");
            if json["kind"] == "finished" {
                assert_eq!(json["status"], "completed");
                saw_finished = true;
            }
        }
        assert!(saw_finished, "expected a translated Finished event");
    }

    #[test]
    fn narrow_rights_intersects_tools_and_ands_booleans() {
        let manifest = ChildRights {
            tools: BTreeSet::from(["fs.read".to_owned(), "fs.write".to_owned()]),
            write: true,
            shell: true,
            ..Default::default()
        };
        let from_parent = ChildRights {
            tools: BTreeSet::from(["fs.read".to_owned()]),
            write: true,
            shell: false,
            ..Default::default()
        };
        let narrowed = narrow_rights(&manifest, &from_parent);
        assert_eq!(narrowed.tools, BTreeSet::from(["fs.read".to_owned()]));
        assert!(narrowed.write);
        assert!(!narrowed.shell);
    }

    #[test]
    fn narrow_rights_never_widens_full_access() {
        let manifest = ChildRights {
            full_access: true,
            ..Default::default()
        };
        let from_parent = ChildRights {
            full_access: false,
            ..Default::default()
        };
        assert!(!narrow_rights(&manifest, &from_parent).full_access);
        assert!(!narrow_rights(&from_parent, &manifest).full_access);
    }

    #[test]
    fn summarize_arguments_truncates_long_json() {
        let long = serde_json::json!({"data": "x".repeat(500)});
        let summary = summarize_arguments(&long);
        assert!(summary.chars().count() <= 201);
    }
}
