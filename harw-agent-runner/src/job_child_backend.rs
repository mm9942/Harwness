//! `JobChildBackend`: [`harw_core::child_backend::ChildBackend`] over a real
//! OS process speaking `harwness.agent-child/v1` on its stdio (see
//! `docs/adr/0001-agent-compiler.md`).
//!
//! # Two ways to run the child process
//! - **Production** ([`JobChildBackend::new`], `S = `[`CurrentExeSpawner`]):
//!   goes through [`harw_tool_job::JobManager::start_piped`] — the child is a
//!   real job (own process group, `stdout.log`/`stderr.log`,
//!   [`harw_tool_job::JobManager::stop`] on cancel), the same machinery
//!   every other job gets, minus the shell-tool permission gate a
//!   job-backed child does not need (its rights come from the protocol's
//!   `Rights` frame, enforced inside the child by `crate::child::run_child`,
//!   not by the launcher). `start_piped` leaves stdin piped instead of null
//!   and tees stdout into `STDOUT_LOG` while also handing this backend a
//!   line stream of it; stderr goes straight into `STDERR_LOG` as for any
//!   other job.
//! - **Tests** ([`JobChildBackend::with_spawner`], no `JobManager`): spawns
//!   [`ChildProcessSpawner::command_for`]'s command itself — a plain
//!   `/bin/sh -c <script>` standing in for a real compiled child, so the
//!   backend's framing/relay/crash/cancel logic is exercised against a real
//!   process without a real bundle or a `JobManager`.
//!
//! # Concurrency
//! [`JobChildBackend::run`] drives one child process per call; several may
//! run concurrently (one per admitted child), each with its own process,
//! stdio pipes and stderr tail buffer.

use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harw_core::child_backend::{
    ChildBackend, ChildBackendFuture, ChildIo, ChildRunOutcome, ChildRunSpec, ChildRunStatus,
};
use harw_core::child_controller::ChildUsage;
use harw_tool_job::{
    Caller, JobId, JobManager, JobOwner, JobSignal, PipedJob, PreparedJob, StartRequest,
};
use harw_types::cancel::CancelReason;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use crate::child_protocol::{
    ChildResultStatus, ChildRights, ChildToParent, ChildUsage as WireUsage, ParentToChild,
    decode_line, encode_line, verify_protocol,
};

/// How long the job-managed path waits for the job's own monitor to record
/// an exit code after the protocol stream ends, before giving up on it.
const JOB_EXIT_WAIT: Duration = Duration::from_secs(5);

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
/// `harwness.agent-child/v1` — as a real job through `harw_tool_job::JobManager`
/// in production, or spawned directly in tests (see module docs).
pub struct JobChildBackend<S: ChildProcessSpawner = CurrentExeSpawner> {
    spawner: S,
    /// `Some`: run through `JobManager::start_piped` (production — every
    /// child is a job, visible in `job.status`/`job.logs`, `job.stop` kills
    /// its whole process group). `None`: spawn `spawner`'s command directly
    /// and drive/kill it here (the test seam).
    job_manager: Option<Arc<JobManager>>,
}

impl JobChildBackend<CurrentExeSpawner> {
    /// The production backend: runs each child as a job through `job_manager`.
    #[must_use]
    pub fn new(job_manager: Arc<JobManager>) -> Self {
        Self {
            spawner: CurrentExeSpawner,
            job_manager: Some(job_manager),
        }
    }
}

impl<S: ChildProcessSpawner> JobChildBackend<S> {
    /// Builds a backend that spawns `spawner`'s command itself, without a
    /// `JobManager` (tests use this with a plain `/bin/sh` command).
    #[must_use]
    pub fn with_spawner(spawner: S) -> Self {
        Self {
            spawner,
            job_manager: None,
        }
    }

    /// Builds a backend that runs `spawner`'s command as a job through
    /// `job_manager` (tests exercising the job-managed path against a
    /// substitute spawner instead of the real [`CurrentExeSpawner`]).
    #[must_use]
    pub fn with_spawner_and_job_manager(spawner: S, job_manager: Arc<JobManager>) -> Self {
        Self {
            spawner,
            job_manager: Some(job_manager),
        }
    }
}

impl<S: ChildProcessSpawner> ChildBackend for JobChildBackend<S> {
    fn run<'a>(&'a self, spec: ChildRunSpec, io: &'a dyn ChildIo) -> ChildBackendFuture<'a> {
        Box::pin(async move {
            match &self.job_manager {
                Some(job_manager) => run_job_managed(job_manager, &self.spawner, spec, io).await,
                None => run_direct(&self.spawner, spec, io).await,
            }
        })
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

/// Source of the child's stdout lines, unified so [`drive_protocol`] does
/// not care whether it runs against a directly-spawned [`Child`] or a
/// [`PipedJob`] from `JobManager::start_piped` — in both cases an error or a
/// closed stream just means "no more lines" (already how the direct path
/// treated a read error before this was introduced).
enum StdoutSource {
    Direct(tokio::io::Lines<BufReader<tokio::process::ChildStdout>>),
    JobManaged(mpsc::UnboundedReceiver<String>),
}

impl StdoutSource {
    async fn next_line(&mut self) -> Option<String> {
        match self {
            Self::Direct(lines) => lines.next_line().await.ok().flatten(),
            Self::JobManaged(receiver) => receiver.recv().await,
        }
    }
}

/// Runs `spec` as a job through `job_manager` (production path): the child
/// is `spawner`'s command, started with `JobManager::start_piped` so it gets
/// the same process-group/log/monitor machinery as any other job.
async fn run_job_managed<S: ChildProcessSpawner>(
    job_manager: &Arc<JobManager>,
    spawner: &S,
    spec: ChildRunSpec,
    io: &dyn ChildIo,
) -> ChildRunOutcome {
    let command = spawner.command_for(&spec.agent_id);
    let command_text = describe_command(&command);
    let prepared = PreparedJob {
        command,
        executed_on_host: true,
    };
    let request = StartRequest {
        name: format!("agent-child-{}", spec.agent_id),
        command: command_text,
        cwd: None,
        env_keys: Vec::new(),
        notify_every: Duration::ZERO,
        owner: JobOwner::new(spec.parent.as_str(), Vec::new()),
    };
    let piped = match job_manager.start_piped(request, prepared) {
        Ok(piped) => piped,
        Err(err) => {
            return ChildRunOutcome {
                status: ChildRunStatus::Crashed {
                    exit_code: None,
                    stderr_tail: format!("failed to start child job: {err}"),
                },
                text: None,
                usage: ChildUsage::default(),
                continuation: None,
            };
        }
    };
    let PipedJob {
        job_id,
        mut stdin,
        stdout_lines,
        ..
    } = piped;
    let caller_session = spec.parent.as_str().to_owned();
    let mut source = StdoutSource::JobManaged(stdout_lines);

    let outcome = tokio::select! {
        biased;
        () = spec.cancel.cancelled() => {
            let _ = job_manager
                .stop(&job_id, Caller::Agent(&caller_session), JobSignal::Term)
                .await;
            ChildRunOutcome {
                status: ChildRunStatus::Cancelled { reason: cancel_reason_label(spec.cancel.reason()) },
                text: None,
                usage: ChildUsage::default(),
                continuation: None,
            }
        }
        outcome = drive_protocol(&mut stdin, &mut source, &spec, io) => outcome,
    };

    if matches!(outcome.status, ChildRunStatus::Failed { .. }) {
        let _ = job_manager
            .stop(&job_id, Caller::Agent(&caller_session), JobSignal::Term)
            .await;
    }
    if matches!(outcome.status, ChildRunStatus::Crashed { .. }) {
        let exit_code = match job_manager
            .wait(&job_id, Caller::Agent(&caller_session), JOB_EXIT_WAIT, None)
            .await
        {
            Ok((_, status)) => status.meta.exit_code,
            Err(_) => None,
        };
        let stderr_tail = job_managed_stderr_tail(job_manager, &job_id, &caller_session);
        return ChildRunOutcome {
            status: ChildRunStatus::Crashed {
                exit_code,
                stderr_tail,
            },
            ..outcome
        };
    }
    outcome
}

/// The last few lines of a job's `STDERR_LOG`, best-effort (an unreadable
/// log or an unknown job just yields an empty tail — `job.logs` remains the
/// authoritative way to inspect it).
fn job_managed_stderr_tail(
    job_manager: &JobManager,
    job_id: &JobId,
    caller_session: &str,
) -> String {
    let Ok(dir) = job_manager.log_dir(job_id, Caller::Agent(caller_session)) else {
        return String::new();
    };
    let query = harw_tool_job::LogQuery {
        tail: Some(STDERR_TAIL_LINES),
        max_lines: STDERR_TAIL_LINES,
        max_bytes: 64 * 1024,
        ..harw_tool_job::LogQuery::default()
    };
    harw_tool_job::logs::read_log(&dir.join(harw_tool_job::model::STDERR_LOG), &query)
        .unwrap_or_default()
        .lines
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// A short, human-readable rendering of `command`'s program and arguments
/// (`meta.json`'s `command` field; not meant to be re-parsed/re-executed).
fn describe_command(command: &Command) -> String {
    let std_command = command.as_std();
    let mut parts = vec![std_command.get_program().to_string_lossy().into_owned()];
    parts.extend(
        std_command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned()),
    );
    parts.join(" ")
}

/// Runs `spec` by spawning `spawner`'s command directly and driving/killing
/// it here (the test seam — production goes through [`run_job_managed`]).
async fn run_direct<S: ChildProcessSpawner>(
    spawner: &S,
    spec: ChildRunSpec,
    io: &dyn ChildIo,
) -> ChildRunOutcome {
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
                let mut tail = tail
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if tail.len() >= STDERR_TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        })
    });

    let mut stdin = stdin;
    let mut source = StdoutSource::Direct(BufReader::new(stdout).lines());

    let outcome = tokio::select! {
        biased;
        () = spec.cancel.cancelled() => {
            kill_process_group(&child);
            let _ = child.kill().await;
            let _ = child.wait().await;
            ChildRunOutcome {
                status: ChildRunStatus::Cancelled { reason: cancel_reason_label(spec.cancel.reason()) },
                text: None,
                usage: ChildUsage::default(),
                continuation: None,
            }
        }
        outcome = drive_protocol(&mut stdin, &mut source, &spec, io) => outcome,
    };

    if matches!(outcome.status, ChildRunStatus::Failed { .. }) {
        kill_process_group(&child);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    // `Completed`/`BudgetExhausted`/`Cancelled` leave the child running its
    // own shutdown, so the stderr tail is not worth waiting for — abort the
    // collector right away. A crash (the stream ended without a `Result`) is
    // worth an exit code and a tail: the process has already exited by then
    // (its stdout pipe only closes at exit), so its stderr pipe closes too
    // and the collector task finishes on its own almost immediately.
    if matches!(outcome.status, ChildRunStatus::Crashed { .. }) {
        let exit_code = child.wait().await.ok().and_then(|status| status.code());
        if let Some(stderr_task) = stderr_task {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), stderr_task).await;
        }
        let outcome = ChildRunOutcome {
            status: ChildRunStatus::Crashed {
                exit_code,
                stderr_tail: String::new(),
            },
            ..outcome
        };
        return fill_stderr_tail(outcome, &stderr_tail);
    }
    if let Some(stderr_task) = stderr_task {
        stderr_task.abort();
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
/// [`run_direct`] or [`run_job_managed`] spawned. Returns once the child
/// sends `Result`, the process exits without one (a crash), or a protocol
/// violation makes the stream unusable.
async fn drive_protocol(
    stdin: &mut ChildStdin,
    stdout_lines: &mut StdoutSource,
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
    let Some(line) = stdout_lines.next_line().await else {
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
        &ParentToChild::Mode {
            mode: if spec.live_mode {
                crate::child_protocol::ChildMode::Live
            } else {
                crate::child_protocol::ChildMode::Plan
            },
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

    // Cancellation while this loop runs is handled by the caller's own
    // `tokio::select!` around this whole function (`run_direct` kills the
    // process group right after; `run_job_managed` calls `JobManager::stop`):
    // dropping this future ends the loop without a graceful `Cancel` frame.
    // That keeps exactly one place responsible for "is this run cancelled".
    loop {
        let Some(line) = stdout_lines.next_line().await else {
            crash_if_stream_ended!();
        };
        match decode_line::<ChildToParent>(&line) {
            Ok(ChildToParent::Hello { .. }) => {
                // A second Hello is a protocol violation, not a reason to
                // restart the handshake.
                return ChildRunOutcome {
                    status: ChildRunStatus::Failed {
                        reason: "unexpected second Hello".to_owned(),
                    },
                    text: None,
                    usage: ChildUsage::default(),
                    continuation: None,
                };
            }
            Ok(ChildToParent::Event { sdk_event }) => io.on_event(sdk_event),
            Ok(ChildToParent::Usage { .. }) => {}
            Ok(ChildToParent::Question { id, text }) => {
                let answer = io.on_question(id.clone(), text).await;
                if write_frame(
                    stdin,
                    &ParentToChild::Answer {
                        question_id: id,
                        text: answer,
                    },
                )
                .await
                .is_err()
                {
                    crash_if_stream_ended!();
                }
            }
            Ok(ChildToParent::ApprovalRequest {
                id,
                tool,
                args_summary,
            }) => {
                let answer = io.on_approval_request(id.clone(), tool, args_summary).await;
                if write_frame(
                    stdin,
                    &ParentToChild::Answer {
                        question_id: id,
                        text: answer,
                    },
                )
                .await
                .is_err()
                {
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
            Ok(ChildToParent::Result {
                status,
                text,
                usage,
            }) => {
                return ChildRunOutcome {
                    status: from_wire_status(status),
                    text,
                    usage: from_wire_usage(usage),
                    continuation: None,
                };
            }
            Err(error) => {
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
    }
}

async fn write_frame(stdin: &mut ChildStdin, frame: &ParentToChild) -> std::io::Result<()> {
    let line = encode_line(frame).map_err(|error| std::io::Error::other(error.to_string()))?;
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
            r#"printf '{{"type":"hello","agent_id":"fixture-child","protocol":"{version}","digest":"d"}}\n'; read _rights; read _mode; read _task; printf '{{"type":"result","status":"completed","text":"done","usage":{{"input_tokens":1,"output_tokens":2,"cached_input_tokens":0,"tool_calls":0,"wall_time_ms":0}}}}\n'"#,
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
    async fn questions_and_approvals_are_relayed() {
        let script = format!(
            r#"printf '{{"type":"hello","agent_id":"fixture-child","protocol":"{version}","digest":"d"}}\n'; read _rights; read _mode; read _task; printf '{{"type":"question","id":"question-1","text":"Proceed?"}}\n'; read answer; case "$answer" in *question-1*approve*) ;; *) exit 3;; esac; printf '{{"type":"approval_request","id":"approval-1","tool":"fs.write","args_summary":"file"}}\n'; read answer; case "$answer" in *approval-1*approve*) ;; *) exit 4;; esac; printf '{{"type":"result","status":"completed","text":"relayed","usage":{{"input_tokens":0,"output_tokens":0,"cached_input_tokens":0,"tool_calls":0,"wall_time_ms":0}}}}\n'"#,
            version = crate::child_protocol::PROTOCOL_VERSION,
        );
        let backend = JobChildBackend::with_spawner(ShellSpawner { script });
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            backend.run(spec(harw_types::cancel::CancelToken::new()), &RecordingIo),
        )
        .await
        .expect("relay must complete");
        assert_eq!(outcome.status, ChildRunStatus::Completed);
        assert_eq!(outcome.text.as_deref(), Some("relayed"));
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
            r#"printf '{{"type":"hello","agent_id":"fixture-child","protocol":"{version}","digest":"d"}}\n'; read _rights; read _mode; read _task; >&2 echo "boom: out of memory"; exit 1"#,
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

    /// Same fixture script as [`hello_then_result_completes`], but driven
    /// through [`JobChildBackend::with_spawner_and_job_manager`] instead of
    /// the direct-spawn path: the child runs as a real `harw_tool_job` job
    /// (its own process group, `stdout.log`/`stderr.log`, `job.status`), and
    /// `run_job_managed` still completes the handshake and relay over the
    /// piped stdin/stdout `start_piped` hands back.
    #[tokio::test]
    async fn job_managed_path_completes_through_job_manager() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = harw_tool_job::JobManagerConfig::new(dir.path());
        let job_manager =
            harw_tool_job::JobManager::new(config, Arc::new(harw_tool_job::NoopNotifier))
                .expect("job manager");
        let backend = JobChildBackend::with_spawner_and_job_manager(
            ShellSpawner {
                script: hello_then_result_script(),
            },
            job_manager,
        );
        let io = RecordingIo;
        let outcome = backend
            .run(spec(harw_types::cancel::CancelToken::new()), &io)
            .await;
        assert_eq!(outcome.status, ChildRunStatus::Completed);
        assert_eq!(outcome.text.as_deref(), Some("done"));
        assert_eq!(outcome.usage.tokens, 3);
    }

    #[tokio::test]
    async fn cancel_kills_the_child_before_a_result() {
        // Prints Hello, reads its two frames, then sleeps far longer than
        // the test waits — only `cancel` should end this run.
        let script = format!(
            r#"printf '{{"type":"hello","agent_id":"fixture-child","protocol":"{version}","digest":"d"}}\n'; read _rights; read _mode; read _task; sleep 30"#,
            version = crate::child_protocol::PROTOCOL_VERSION,
        );
        let backend = JobChildBackend::with_spawner(ShellSpawner { script });
        let io = RecordingIo;
        let cancel = harw_types::cancel::CancelToken::new();

        // Cancels concurrently with the run below, from a task that touches
        // nothing but the (cheaply cloneable) token — `backend.run`'s future
        // borrows `io`/`backend` and so is not `'static`, but is awaited
        // directly here rather than spawned, which needs no such bound.
        let canceller = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                cancel.cancel(CancelReason::User);
            })
        };

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            backend.run(spec(cancel), &io),
        )
        .await
        .expect("cancel must end the run promptly");
        canceller.await.expect("canceller task did not panic");
        match outcome.status {
            ChildRunStatus::Cancelled { reason } => assert_eq!(reason, "user"),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
