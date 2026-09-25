//! `--child <id> --child-protocol stdio`: this process is one child agent of
//! a compiled parent (plan `docs/plans/r10-agent-compiler.md`, §3C).
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
//! A child's effective rights are `min(child manifest, the parent's current
//! rights)` (plan §3C): [`narrow_rights`] does the intersection/AND once the
//! parent's `Rights` frame arrives, over the child's own manifest rights
//! ([`manifest_rights_of`], derived from its `AgentIr`). This only ever
//! narrows — a parent can restrict a child further than its manifest, never
//! grant it more.
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

use harw_agent_dsl::ir_v2::AgentIr;
#[cfg(test)]
use harwness_sdk::Harwness;
use harwness_sdk::{
    ApprovalHandler, ApprovalRequest, BoxFuture, Decision, FinishStatus, SdkEvent, TurnReport,
    TurnStatus,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::child_protocol::{
    ChildResultStatus, ChildRights, ChildToParent, ChildUsage, PROTOCOL_VERSION, ParentToChild,
    decode_line, encode_line,
};
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

    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    let manifest_rights = manifest_rights_of(&child_ir);
    let mut rights = manifest_rights.clone();
    let mut received_rights = false;
    let mut budget = harw_agent_dsl::ir_v2::Budget::default();
    let mut mode = harwness_sdk::Mode::Plan;

    // The first `Task` starts the run; `Rights`/`Budget`/`Mode` may arrive
    // before it and are applied once the session exists. `Cancel` or a
    // closed stdin before any `Task` ends this process cleanly — there is
    // nothing to report a result for.
    let (task, context, continue_from) = loop {
        match stdin.next_line().await {
            Ok(Some(line)) => match decode_line::<ParentToChild>(&line) {
                Ok(ParentToChild::Task {
                    task,
                    context,
                    continue_from,
                }) if received_rights => break (task, context, continue_from),
                Ok(ParentToChild::Task { .. }) => {
                    let _ = outbound_tx.send(ChildToParent::Error {
                        message: "Rights must precede Task".to_owned(),
                    });
                    drop(outbound_tx);
                    let _ = writer.await;
                    return ExitCode::FAILURE;
                }
                Ok(ParentToChild::Rights {
                    rights: from_parent,
                }) => {
                    rights = narrow_rights(&rights, &from_parent);
                    received_rights = true;
                }
                Ok(ParentToChild::Cancel) => return ExitCode::SUCCESS,
                Ok(ParentToChild::Budget {
                    max_tokens,
                    max_tool_calls,
                    max_wall_time_ms,
                }) => {
                    budget.max_tokens = min_limit(budget.max_tokens, max_tokens);
                    budget.max_tool_calls = min_limit(
                        budget.max_tool_calls,
                        max_tool_calls.map(|v| u32::try_from(v).unwrap_or(u32::MAX)),
                    );
                    budget.max_wall_secs =
                        min_limit(budget.max_wall_secs, max_wall_time_ms.map(|v| v / 1_000));
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
            },
            Ok(None) => return ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("harw-agent-runner: failed reading stdin: {error}");
                return ExitCode::FAILURE;
            }
        }
    };
    if continue_from.is_some() {
        let _ = outbound_tx.send(ChildToParent::Error {
            message: "job child continuation is not supported".to_owned(),
        });
        drop(outbound_tx);
        let _ = writer.await;
        return ExitCode::FAILURE;
    }
    let selected = match ctx.agent.for_agent(&agent_id) {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("harw-agent-runner: cannot select child: {error}");
            return ExitCode::FAILURE;
        }
    };
    ctx.agent = Arc::new(
        selected.with_rights(harw_runtime::embedded::EffectiveRights {
            tools: rights.tools,
            network_hosts: rights.network_hosts,
            network_open: rights.network_open,
            write: rights.write,
            shell: rights.shell,
            host: rights.host,
            full_access: rights.full_access,
            budget: Some(budget),
        }),
    );
    let approvals = Arc::new(RelayApprovals::new(outbound_tx.clone()));
    let harwness = match ctx.builder().and_then(|builder| {
        builder
            .mode(mode)
            .approval_handler_arc(approvals.clone())
            .build()
            .map_err(crate::error::RunnerError::from)
    }) {
        Ok(harwness) => harwness,
        Err(error) => {
            let _ = outbound_tx.send(ChildToParent::Error {
                message: format!("failed to build the child session: {error}"),
            });
            writer.abort();
            return ExitCode::FAILURE;
        }
    };
    let mut session = match harwness.session() {
        Ok(session) => session,
        Err(error) => {
            let _ = outbound_tx.send(ChildToParent::Error {
                message: format!("failed to start the child session: {error}"),
            });
            writer.abort();
            return ExitCode::FAILURE;
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
    // independent `BufReader` over the same fd would race it for bytes.
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
    let _ = reader.await;
    drop(session);
    drop(harwness);

    let frame = match result {
        Ok(report) => translate_turn_report(&report),
        Err(error) => ChildToParent::Result {
            status: ChildResultStatus::Failed,
            text: Some(error.to_string()),
            usage: ChildUsage::default(),
        },
    };
    let _ = outbound_tx.send(frame);
    drop(outbound_tx);
    let _ = writer.await;
    ExitCode::SUCCESS
}

/// Drains the rest of `stdin` for `Answer`/`Cancel` frames that arrive while
/// the turn runs, resolving pending approvals and cancelling the session.
async fn run_reader(
    mut stdin: tokio::io::Lines<BufReader<tokio::io::Stdin>>,
    approvals: Arc<RelayApprovals>,
    cancel: harwness_sdk::CancelHandle,
    outbound: mpsc::UnboundedSender<ChildToParent>,
) {
    loop {
        match stdin.next_line().await {
            Ok(Some(line)) => match decode_line::<ParentToChild>(&line) {
                Ok(ParentToChild::Answer { question_id, text }) => {
                    approvals.resolve(&question_id, text);
                }
                Ok(ParentToChild::Cancel) => {
                    cancel.cancel();
                    break;
                }
                Ok(_) | Err(_) => {
                    let _ = outbound.send(ChildToParent::Error {
                        message: "unsupported or malformed mid-turn control frame; child cancelled"
                            .to_owned(),
                    });
                    cancel.cancel();
                    break;
                }
            },
            _ => {
                cancel.cancel();
                break;
            }
        }
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

/// Derives protocol rights from the typed permissions manifest.
fn manifest_rights_of(ir: &AgentIr) -> ChildRights {
    let rights = harw_runtime::embedded::EffectiveRights::from_manifest(&ir.permissions);
    ChildRights {
        tools: rights.tools,
        network_hosts: rights.network_hosts,
        network_open: rights.network_open,
        write: rights.write,
        shell: rights.shell,
        host: rights.host,
        // Automatic approval is a parent choice, not a tool permission.
        full_access: true,
    }
}

/// `min(manifest, from_parent)`: tools/hosts intersect, every boolean right
/// is AND'd. A child never ends up with more than either side allows.
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
pub(crate) fn translate_turn_report(report: &TurnReport) -> ChildToParent {
    let status = match &report.status {
        TurnStatus::Completed => ChildResultStatus::Completed,
        TurnStatus::Cancelled { .. } => ChildResultStatus::Cancelled,
        TurnStatus::Truncated | TurnStatus::Refused { .. } | TurnStatus::Failed { .. } => {
            ChildResultStatus::Failed
        }
        // `TurnStatus` is `#[non_exhaustive]`; treat anything future as the
        // same conservative outcome as the other named failure variants
        // above, consistent with `iface::mcp`'s equivalent match.
        _ => ChildResultStatus::Failed,
    };
    ChildToParent::Result {
        status,
        text: report.text.clone(),
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
