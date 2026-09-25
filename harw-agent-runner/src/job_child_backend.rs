//! `JobChildBackend`: [`harw_core::child_backend::ChildBackend`] over a real
//! OS process speaking `harwness.agent-child/v1` on its stdio (plan
//! `docs/plans/r10-agent-compiler.md`, §3C).
//!
//! # Why this does not call `harw_tool_job::JobManager` directly
//! The plan asks a job-backed child to get the same process-group/logs
//! machinery as any other job, via `JobManager::start`. Two things in
//! `harw-tool-job` as it stands today make that impossible without changing
//! it:
//!
//! - `harw_tool_job::launcher::{ShellJobLauncher, DirectLauncher}` both
//!   hardcode `stdin(Stdio::null())` on the [`PreparedJob`](harw_tool_job::launcher::PreparedJob)
//!   they build — a job's stdin is never meant to receive anything.
//! - `JobManager::start` always does `command.stdout(Stdio::from(stdout_log_file))`
//!   and `.stderr(Stdio::from(stderr_log_file))`, then moves the spawned
//!   `tokio::process::Child` **into its own monitor task**
//!   (`tokio::spawn(monitor.run(child))`) and returns only a `JobStatus`.
//!   There is no way for a caller to get the child's `ChildStdin`/
//!   `ChildStdout` back — they are consumed before `start` even returns.
//!
//! **The needed `harw-tool-job` change**, so a future wave can route this
//! through `JobManager` properly (process listing, `job.logs`, `job.stop`
//! parity with every other job): add a `StartRequest` flag (or a sibling
//! method, e.g. `JobManager::start_piped`) that (a) leaves `stdin` piped
//! instead of null on the prepared command, (b) still tees stdout into
//! `STDOUT_LOG` for `job.logs`/`job.status` (e.g. by reading through a
//! `tokio::io::split` and fanning out to both the log file and a returned
//! channel) instead of redirecting the raw fd there, and (c) returns a
//! small handle (`ChildStdin` plus a `Receiver<String>` of stdout lines)
//! alongside the `JobId`, while the existing monitor task keeps doing exit
//! detection/logging exactly as it does today.
//!
//! Until that lands, this module spawns the process itself against
//! [`ChildProcessSpawner`], a small trait scoped to exactly what this
//! backend needs (own process group, piped stdio) — the same process-group
//! setup `harw_tool_job::launcher::DirectLauncher` already uses for its own
//! tests, minus the shell-tool permission gate a job-backed child does not
//! need (its rights come from the protocol's `Rights` frame, enforced
//! inside the child by `crate::child::run_child`, not by the launcher).
//!
//! # Concurrency
//! [`JobChildBackend::run`] drives one child process per call; several may
//! run concurrently (one per admitted child), each with its own process,
//! stdio pipes and stderr tail buffer.

use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use harw_core::child_backend::{
    ChildBackend, ChildBackendFuture, ChildIo, ChildRunOutcome, ChildRunSpec, ChildRunStatus,
};
use harw_core::child_controller::ChildUsage;
use harw_types::cancel::CancelReason;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};

use crate::child_protocol::{
    ChildResultStatus, ChildRights, ChildToParent, ChildUsage as WireUsage, ParentToChild,
    decode_line, encode_line, verify_protocol,
};

/// How many trailing stderr lines a crashed child reports in
/// [`ChildRunStatus::Crashed::stderr_tail`].
const STDERR_TAIL_LINES: usize = 32;

/// Builds the not-yet-spawned command for one child. Production always uses
/// [`CurrentExeSpawner`]; tests substitute a plain command (`/bin/sh -c
/// ...`) that stands in for a real compiled child and speaks the protocol by
/// hand, so the backend's framing/relay/crash/cancel logic is exercised
/// against a real process without a real bundle.
pub trait ChildProcessSpawner: Send + Sync {
    /// Returns a `Command` for `agent_id`, with stdin/stdout/stderr left for
    /// [`JobChildBackend`] to pipe and its own process group already set.
    fn command_for(&self, agent_id: &str) -> Command;
}

/// Spawns `std::env::current_exe()` with `--child <agent_id> --child-protocol
/// stdio`, in its own process group so [`JobChildBackend`] can kill the
/// whole group on cancel instead of leaking grandchildren.
#[derive(Debug, Clone, Default)]
pub struct CurrentExeSpawner;

impl ChildProcessSpawner for CurrentExeSpawner {
    fn command_for(&self, agent_id: &str) -> Command {
        let exe = std::env::current_exe()
            .unwrap_or_else(|_| std::path::PathBuf::from("harw-agent-runner"));
        let mut command = Command::new(exe);
        command
            .arg("--child")
            .arg(agent_id)
            .arg("--child-protocol")
            .arg("stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);
        #[cfg(unix)]
        {
            // Own process group (pgid == pid): `cancel` kills the whole
            // group, the same guarantee `harw_tool_job` gives every job.
            command.process_group(0);
        }
        command
    }
}

/// [`ChildBackend`] that runs each child as a separate OS process speaking
/// `harwness.agent-child/v1` (see module docs for why it does not currently
/// go through `harw_tool_job::JobManager`).
pub struct JobChildBackend<S: ChildProcessSpawner = CurrentExeSpawner> {
    spawner: S,
}

impl JobChildBackend<CurrentExeSpawner> {
    /// The production backend: spawns the current executable.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spawner: CurrentExeSpawner,
        }
    }
}

impl Default for JobChildBackend<CurrentExeSpawner> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: ChildProcessSpawner> JobChildBackend<S> {
    /// Builds a backend with a custom spawner (tests use this with a plain
    /// `/bin/sh` command).
    #[must_use]
    pub fn with_spawner(spawner: S) -> Self {
        Self { spawner }
    }
}

impl<S: ChildProcessSpawner> ChildBackend for JobChildBackend<S> {
    fn run<'a>(&'a self, spec: ChildRunSpec, io: &'a dyn ChildIo) -> ChildBackendFuture<'a> {
        Box::pin(async move { run_one(&self.spawner, spec, io).await })
    }
}

/// Short label mirroring `harwness_sdk::session::cancel_reason` (private to
/// that crate), used for [`ChildRunStatus::Cancelled::reason`].
fn cancel_reason_label(reason: Option<CancelReason>) -> String {
    match reason {
        Some(CancelReason::User) => "user",
        Some(CancelReason::Parent) => "parent",
        Some(CancelReason::Budget) => "budget",
        Some(CancelReason::LeaseLost) => "lease_lost",
        Some(CancelReason::Shutdown) => "shutdown",
        None => "cancelled",
    }
    .to_owned()
}

/// Kills the child's whole process group — best-effort, and via `rustix`
/// like every other process signal in this codebase
/// (`harw_tool_job::procfs::signal_group`), never raw `libc`/`unsafe`. A
/// process that already exited is not an error.
#[cfg(unix)]
fn kill_process_group(child: &Child) {
    let Some(pid) = child.id().and_then(|pid| {
        i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
    }) else {
        return;
    };
    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
}

#[cfg(not(unix))]
fn kill_process_group(_child: &Child) {}

async fn run_one<S: ChildProcessSpawner>(
    spawner: &S,
    spec: ChildRunSpec,
    io: &dyn ChildIo,
) -> ChildRunOutcome {
    let start = std::time::Instant::now();
    let mut command = spawner.command_for(&spec.agent_id);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ChildRunOutcome {
                status: ChildRunStatus::Crashed {
                    exit_code: None,
                    stderr_tail: format!("failed to start child process: {error}"),
                },
                text: None,
                usage: ChildUsage::default(),
                continuation: None,
            };
        }
    };
    let Some(stdin) = child.stdin.take() else {
        return outcome_from_missing_pipe(&mut child, "stdin").await;
    };
    let Some(stdout) = child.stdout.take() else {
        return outcome_from_missing_pipe(&mut child, "stdout").await;
    };
    let stderr = child.stderr.take();

    let stderr_tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
    let stderr_task = stderr.map(|stderr| {
        let tail = Arc::clone(&stderr_tail);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut tail = tail.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if tail.len() >= STDERR_TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        })
    });

    let mut stdin = stdin;
    let mut stdout_lines = BufReader::new(stdout).lines();

    let outcome = tokio::select! {
        biased;
        () = spec.cancel.cancelled() => {
            kill_process_group(&child);
            let _ = child.wait().await;
            ChildRunOutcome {
                status: ChildRunStatus::Cancelled { reason: cancel_reason_label(spec.cancel.reason()) },
                text: None,
                usage: ChildUsage::default(),
                continuation: None,
            }
        }
        outcome = drive_protocol(&mut stdin, &mut stdout_lines, &spec, io) => outcome,
    };

    if let Some(stderr_task) = stderr_task {
        stderr_task.abort();
    }

    // A `Crashed`/protocol-`Failed` outcome from `drive_protocol` already
    // observed the process ending; everything else (`Completed`,
    // `BudgetExhausted`, `Cancelled`) leaves the child running its own
    // shutdown, which we do not block on.
    let _ = start.elapsed();
    if matches!(outcome.status, ChildRunStatus::Crashed { .. }) {
        return fill_stderr_tail(outcome, &stderr_tail);
    }
    outcome
}

async fn outcome_from_missing_pipe(child: &mut Child, which: &str) -> ChildRunOutcome {
    kill_process_group(child);
    let _ = child.wait().await;
    ChildRunOutcome {
        status: ChildRunStatus::Crashed {
            exit_code: None,
            stderr_tail: format!("child process has no {which} pipe"),
        },
        text: None,
        usage: ChildUsage::default(),
        continuation: None,
    }
}

fn fill_stderr_tail(
    outcome: ChildRunOutcome,
    stderr_tail: &Arc<Mutex<VecDeque<String>>>,
) -> ChildRunOutcome {
    let ChildRunStatus::Crashed { exit_code, .. } = outcome.status else {
        return outcome;
    };
    let tail = stderr_tail
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    ChildRunOutcome {
        status: ChildRunStatus::Crashed {
            exit_code,
            stderr_tail: tail,
        },
        ..outcome
    }
}

/// The handshake plus the frame-relay loop, run against whichever process
/// [`run_one`] spawned. Returns once the child sends `Result`, the process
/// exits without one (a crash), or a protocol violation makes the stream
/// unusable.
async fn drive_protocol(
    stdin: &mut ChildStdin,
    stdout_lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    spec: &ChildRunSpec,
    io: &dyn ChildIo,
) -> ChildRunOutcome {
    macro_rules! crash_if_stream_ended {
        () => {
            return ChildRunOutcome {
                status: ChildRunStatus::Crashed {
                    exit_code: None,
                    stderr_tail: String::new(),
                },
                text: None,
                usage: ChildUsage::default(),
                continuation: None,
            };
        };
    }

    // Handshake: the child's first line must be `Hello` with a matching
    // protocol version.
    let Ok(Some(line)) = stdout_lines.next_line().await else {
        crash_if_stream_ended!();
    };
    match decode_line::<ChildToParent>(&line) {
        Ok(ChildToParent::Hello { protocol, .. }) => {
            if let Err(error) = verify_protocol(&protocol) {
                return ChildRunOutcome {
                    status: ChildRunStatus::Failed {
                        reason: error.to_string(),
                    },
                    text: None,
                    usage: ChildUsage::default(),
                    continuation: None,
                };
            }
        }
        Ok(_) | Err(_) => {
            return ChildRunOutcome {
                status: ChildRunStatus::Failed {
                    reason: format!("expected Hello as the child's first frame, got: {line}"),
                },
                text: None,
                usage: ChildUsage::default(),
                continuation: None,
            };
        }
    }

    // Rights, then the task itself.
    if write_frame(
        stdin,
        &ParentToChild::Rights {
            rights: to_wire_rights(&spec.rights),
        },
    )
    .await
    .is_err()
    {
        crash_if_stream_ended!();
    }
    if let Some(budget) = &spec.budget
        && write_frame(
            stdin,
            &ParentToChild::Budget {
                max_tokens: budget.max_tokens,
                max_tool_calls: budget.max_tool_calls.map(u64::from),
                max_wall_time_ms: budget.max_wall_secs.map(|secs| secs.saturating_mul(1_000)),
            },
        )
        .await
        .is_err()
    {
        crash_if_stream_ended!();
    }
    if write_frame(
        stdin,
        &ParentToChild::Task {
            task: spec.task.clone(),
            context: spec.context.clone(),
            continue_from: spec.continue_from.clone(),
        },
    )
    .await
    .is_err()
    {
        crash_if_stream_ended!();
    }

    loop {
        tokio::select! {
            biased;
            () = spec.cancel.cancelled() => {
                let _ = write_frame(stdin, &ParentToChild::Cancel).await;
                return ChildRunOutcome {
                    status: ChildRunStatus::Cancelled { reason: cancel_reason_label(spec.cancel.reason()) },
                    text: None,
                    usage: ChildUsage::default(),
                    continuation: None,
                };
            }
            line = stdout_lines.next_line() => {
                let Ok(Some(line)) = line else {
                    crash_if_stream_ended!();
                };
                match decode_line::<ChildToParent>(&line) {
                    Ok(ChildToParent::Hello { .. }) => {
                        // A second Hello is a protocol violation, not a
                        // reason to restart the handshake.
                        return ChildRunOutcome {
                            status: ChildRunStatus::Failed { reason: "unexpected second Hello".to_owned() },
                            text: None,
                            usage: ChildUsage::default(),
                            continuation: None,
                        };
                    }
                    Ok(ChildToParent::Event { sdk_event }) => io.on_event(sdk_event),
                    Ok(ChildToParent::Usage { .. }) => {}
                    Ok(ChildToParent::Question { id, text }) => {
                        let answer = io.on_question(id.clone(), text).await;
                        if write_frame(stdin, &ParentToChild::Answer { question_id: id, text: answer }).await.is_err() {
                            crash_if_stream_ended!();
                        }
                    }
                    Ok(ChildToParent::ApprovalRequest { id, tool, args_summary }) => {
                        let answer = io.on_approval_request(id.clone(), tool, args_summary).await;
                        if write_frame(stdin, &ParentToChild::Answer { question_id: id, text: answer }).await.is_err() {
                            crash_if_stream_ended!();
                        }
                    }
                    Ok(ChildToParent::Error { message }) => {
                        return ChildRunOutcome {
                            status: ChildRunStatus::Failed { reason: message },
                            text: None,
                            usage: ChildUsage::default(),
                            continuation: None,
                        };
                    }
                    Ok(ChildToParent::Result { status, text, usage }) => {
                        return ChildRunOutcome {
                            status: from_wire_status(status),
                            text,
                            usage: from_wire_usage(usage),
                            continuation: None,
                        };
                    }
                    Err(error) => {
                        return ChildRunOutcome {
                            status: ChildRunStatus::Failed { reason: error.to_string() },
                            text: None,
                            usage: ChildUsage::default(),
                            continuation: None,
                        };
                    }
                }
            }
        }
    }
}

async fn write_frame(stdin: &mut ChildStdin, frame: &ParentToChild) -> std::io::Result<()> {
    let line = encode_line(frame)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await
}

fn to_wire_rights(rights: &harw_core::child_backend::ChildBackendRights) -> ChildRights {
    ChildRights {
        tools: rights.tools.clone(),
        network_hosts: rights.network_hosts.clone(),
        network_open: rights.network_open,
        write: rights.write,
        shell: rights.shell,
        host: rights.host,
        full_access: rights.full_access,
    }
}

fn from_wire_status(status: ChildResultStatus) -> ChildRunStatus {
    match status {
        ChildResultStatus::Completed => ChildRunStatus::Completed,
        ChildResultStatus::BudgetExhausted => ChildRunStatus::BudgetExhausted,
        ChildResultStatus::Cancelled => ChildRunStatus::Cancelled {
            reason: "child".to_owned(),
        },
        ChildResultStatus::Failed => ChildRunStatus::Failed {
            reason: "child reported a failed turn".to_owned(),
        },
    }
}

fn from_wire_usage(usage: WireUsage) -> ChildUsage {
    ChildUsage {
        tokens: usage.fresh_tokens(),
        tool_calls: u32::try_from(usage.tool_calls).unwrap_or(u32::MAX),
        wall_time_ms: usage.wall_time_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_types::SessionId;

    /// Spawns `/bin/sh -c <script>` — a stand-in child that speaks
    /// `harwness.agent-child/v1` by hand, exercising the real stdio pipes,
    /// process group and (in the crash test) a real non-zero exit.
    struct ShellSpawner {
        script: String,
    }

    impl ChildProcessSpawner for ShellSpawner {
        fn command_for(&self, _agent_id: &str) -> Command {
            let mut command = Command::new("/bin/sh");
            command
                .arg("-c")
                .arg(&self.script)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(false);
            #[cfg(unix)]
            command.process_group(0);
            command
        }
    }

    fn spec(cancel: harw_types::cancel::CancelToken) -> ChildRunSpec {
        ChildRunSpec {
            child: SessionId::new(),
            parent: SessionId::new(),
            agent_id: "fixture-child".to_owned(),
            task: "do the thing".to_owned(),
            context: None,
            continue_from: None,
            rights: harw_core::child_backend::ChildBackendRights::default(),
            budget: None,
            live_mode: true,
            cancel,
        }
    }

    struct RecordingIo;
    impl ChildIo for RecordingIo {
        fn on_event(&self, _event_json: serde_json::Value) {}
        fn on_question<'a>(
            &'a self,
            _id: String,
            _text: String,
        ) -> harw_core::child_backend::ChildAnswerFuture<'a> {
            Box::pin(async { "approve".to_owned() })
        }
        fn on_approval_request<'a>(
            &'a self,
            _id: String,
            _tool: String,
            _args_summary: String,
        ) -> harw_core::child_backend::ChildAnswerFuture<'a> {
            Box::pin(async { "approve".to_owned() })
        }
    }

    /// A one-line shell script that reads exactly one stdin line (the
    /// `Rights` frame this backend always sends first), discards it, then
    /// prints `Hello` followed by `Result`.
    fn hello_then_result_script() -> String {
        format!(
            r#"printf '{{"type":"hello","agent_id":"fixture-child","protocol":"{version}","digest":"d"}}\n'; read _rights; read _task; printf '{{"type":"result","status":"completed","text":"done","usage":{{"input_tokens":1,"output_tokens":2,"cached_input_tokens":0,"tool_calls":0,"wall_time_ms":0}}}}\n'"#,
            version = crate::child_protocol::PROTOCOL_VERSION,
        )
    }

    #[tokio::test]
    async fn hello_then_result_completes() {
        let backend = JobChildBackend::with_spawner(ShellSpawner {
            script: hello_then_result_script(),
        });
        let io = RecordingIo;
        let outcome = backend
            .run(spec(harw_types::cancel::CancelToken::new()), &io)
            .await;
        assert_eq!(outcome.status, ChildRunStatus::Completed);
        assert_eq!(outcome.text.as_deref(), Some("done"));
        assert_eq!(outcome.usage.tokens, 3);
    }

    #[tokio::test]
    async fn a_crash_before_hello_is_reported_as_crashed() {
        // Exits immediately without printing anything: no Hello ever
        // arrives, so this must not hang and must be reported as a crash.
        let backend = JobChildBackend::with_spawner(ShellSpawner {
            script: "exit 7".to_owned(),
        });
        let io = RecordingIo;
        let outcome = backend
            .run(spec(harw_types::cancel::CancelToken::new()), &io)
            .await;
        assert!(matches!(outcome.status, ChildRunStatus::Crashed { .. }));
    }

    #[tokio::test]
    async fn a_crash_after_hello_reports_a_stderr_tail() {
        let script = format!(
            r#"printf '{{"type":"hello","agent_id":"fixture-child","protocol":"{version}","digest":"d"}}\n'; read _rights; read _task; >&2 echo "boom: out of memory"; exit 1"#,
            version = crate::child_protocol::PROTOCOL_VERSION,
        );
        let backend = JobChildBackend::with_spawner(ShellSpawner { script });
        let io = RecordingIo;
        let outcome = backend
            .run(spec(harw_types::cancel::CancelToken::new()), &io)
            .await;
        match outcome.status {
            ChildRunStatus::Crashed { stderr_tail, .. } => {
                assert!(stderr_tail.contains("boom: out of memory"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancel_kills_the_child_before_a_result() {
        // Prints Hello, reads its two frames, then sleeps far longer than
        // the test waits — only `cancel` should end this run.
        let script = format!(
            r#"printf '{{"type":"hello","agent_id":"fixture-child","protocol":"{version}","digest":"d"}}\n'; read _rights; read _task; sleep 30"#,
            version = crate::child_protocol::PROTOCOL_VERSION,
        );
        let backend = JobChildBackend::with_spawner(ShellSpawner { script });
        let io = RecordingIo;
        let cancel = harw_types::cancel::CancelToken::new();
        let cancel_clone = cancel.clone();

        let run = tokio::spawn(async move { backend.run(spec(cancel_clone), &io).await });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        cancel.cancel(CancelReason::User);

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), run)
            .await
            .expect("cancel must end the run promptly")
            .expect("task did not panic");
        match outcome.status {
            ChildRunStatus::Cancelled { reason } => assert_eq!(reason, "user"),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
