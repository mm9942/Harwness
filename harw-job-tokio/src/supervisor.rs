//! One supervisor event loop per process (Job-Runtime-Doc §13, Phase 6).
//!
//! A single task fans in pidfd exit readiness, stdout, stderr, a deadline
//! and cancellation, and reports [`ProcessEvent`]s over a bounded channel.
//! On `Timeout`/`CancelRequested` it applies the [`TerminationPolicy`]
//! asynchronously *inside the same loop* — output keeps being drained while
//! the job shuts down, so a job blocked on a full pipe cannot stall its own
//! termination.
//!
//! # Event contract
//! - `Stdout`/`Stderr` chunks arrive in pipe order per stream (chunk
//!   boundaries are arbitrary; no line framing).
//! - At most one of `Timeout`/`CancelRequested` is sent (the first trigger
//!   wins), and only while the primary is still running.
//! - `SupervisorError` reports a failure. It is terminal (no `Exited`
//!   follows) only when exit observation itself failed; signal and read
//!   failures are reported and supervision continues.
//! - `Exited` is the last event. It is sent after both streams reached EOF
//!   or after [`SupervisorConfig::drain_timeout`] (counted from exit).
//!
//! # Backpressure and abandonment
//! Events are sent with `send().await`: a consumer that stops reading
//! pauses the loop (including its timers). A consumer that drops the
//! receiver abandons the job: the supervisor notices it right away, closes
//! the output pipes, terminates the job with a forced hard kill after the
//! grace period, reaps it and ends.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use harw_job_core::ExitOutcome;
use harw_job_linux::{ProcessError, SignalKind, TerminationPolicy};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::Instant;

use crate::process::{AsyncLinuxProcess, ignore_exited, io_error};

/// What happened to a supervised process (Job-Runtime-Doc §13).
#[derive(Debug)]
#[non_exhaustive]
pub enum ProcessEvent {
    /// A chunk of standard output.
    Stdout(Bytes),
    /// A chunk of standard error.
    Stderr(Bytes),
    /// The primary terminated and was reaped. Always the last event.
    Exited(ExitOutcome),
    /// The deadline elapsed; termination has started.
    Timeout,
    /// Cancellation was requested; termination has started.
    CancelRequested,
    /// A supervision step failed (see the module docs for when it is
    /// terminal).
    SupervisorError(ProcessError),
}

/// Supervisor tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorConfig {
    /// Wall-clock limit counted from [`Supervisor::spawn`]; `None` = none.
    pub deadline: Option<Duration>,
    /// How to terminate on timeout or cancellation.
    pub termination: TerminationPolicy,
    /// How long to keep reading output after the primary exited (a
    /// surviving descendant may hold the pipes open).
    pub drain_timeout: Duration,
    /// Capacity of the event channel (at least 1).
    pub channel_capacity: usize,
    /// Size of one read from stdout/stderr (at least 1).
    pub read_chunk_size: usize,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            deadline: None,
            termination: TerminationPolicy::default(),
            drain_timeout: Duration::from_secs(2),
            channel_capacity: 64,
            read_chunk_size: 8 * 1024,
        }
    }
}

/// Requests cancellation of one supervisor. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Canceller {
    sender: Arc<watch::Sender<bool>>,
}

impl Canceller {
    /// Requests cancellation. Idempotent; ignored once the process exited.
    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }
}

/// Handle to a running supervisor task.
///
/// Dropping it does **not** cancel the job; drop the event receiver (or
/// call [`SupervisorHandle::cancel`]) for that.
#[derive(Debug)]
pub struct SupervisorHandle {
    pid: u32,
    canceller: Canceller,
    task: JoinHandle<AsyncLinuxProcess>,
}

impl SupervisorHandle {
    /// Diagnostic PID of the supervised primary.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Requests cancellation (see [`Canceller::cancel`]).
    pub fn cancel(&self) {
        self.canceller.cancel();
    }

    /// A clonable canceller for this supervisor.
    #[must_use]
    pub fn canceller(&self) -> Canceller {
        self.canceller.clone()
    }

    /// Whether the supervisor task has ended.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    /// Waits for the supervisor task and returns the process handle (reaped
    /// unless a terminal `SupervisorError` was reported).
    ///
    /// # Errors
    /// The task panicked or was aborted.
    pub async fn join(self) -> Result<AsyncLinuxProcess, JoinError> {
        self.task.await
    }
}

/// Spawns supervisor tasks.
#[derive(Debug, Clone, Copy)]
pub struct Supervisor;

impl Supervisor {
    /// Starts supervising `process` with its (optional) output pipes.
    ///
    /// Must be called inside a Tokio runtime with I/O and time drivers
    /// enabled (Tokio panics otherwise). Pipes that cannot be registered
    /// with the reactor are reported as the first `SupervisorError` events
    /// and treated as closed.
    #[must_use]
    pub fn spawn(
        process: AsyncLinuxProcess,
        stdout: Option<std::process::ChildStdout>,
        stderr: Option<std::process::ChildStderr>,
        config: SupervisorConfig,
    ) -> (SupervisorHandle, mpsc::Receiver<ProcessEvent>) {
        let start = Instant::now();
        let mut pending = Vec::new();
        let stdout = stdout.and_then(|pipe| match ChildStdout::from_std(pipe) {
            Ok(pipe) => Some(pipe),
            Err(error) => {
                pending.push(ProcessEvent::SupervisorError(io_error(
                    "register(stdout)",
                    &error,
                )));
                None
            }
        });
        let stderr = stderr.and_then(|pipe| match ChildStderr::from_std(pipe) {
            Ok(pipe) => Some(pipe),
            Err(error) => {
                pending.push(ProcessEvent::SupervisorError(io_error(
                    "register(stderr)",
                    &error,
                )));
                None
            }
        });
        let (events, receiver) = mpsc::channel(config.channel_capacity.max(1));
        let (cancel_sender, cancel) = watch::channel(false);
        let canceller = Canceller {
            sender: Arc::new(cancel_sender),
        };
        let pid = process.pid();
        let deadline = config.deadline.and_then(|limit| start.checked_add(limit));
        let run = Run {
            process,
            stdout,
            stderr,
            cancel,
            deadline,
            control: Control {
                events,
                orphaned: false,
                orphan_handled: false,
                policy: config.termination,
                drain_timeout: config.drain_timeout,
                termination: None,
            },
            chunk: config.read_chunk_size.max(1),
        };
        let task = tokio::spawn(run.run(pending));
        (
            SupervisorHandle {
                pid,
                canceller,
                task,
            },
            receiver,
        )
    }
}

/// Termination in progress.
#[derive(Debug, Clone, Copy)]
struct Termination {
    /// When to escalate to SIGKILL; `None` once escalated (or never).
    grace_until: Option<Instant>,
    /// Whether to SIGKILL after the grace period.
    hard_kill: bool,
}

/// Mutable bookkeeping that the loop touches only between `select!`s.
struct Control {
    events: mpsc::Sender<ProcessEvent>,
    /// The receiver was dropped: nothing is sent any more.
    orphaned: bool,
    /// The abandonment reaction has been applied.
    orphan_handled: bool,
    policy: TerminationPolicy,
    drain_timeout: Duration,
    termination: Option<Termination>,
}

impl Control {
    async fn emit(&mut self, event: ProcessEvent) {
        if self.orphaned {
            return;
        }
        if self.events.send(event).await.is_err() {
            tracing::debug!("event receiver dropped; abandoning supervised job");
            self.orphaned = true;
        }
    }

    /// Sends the graceful signal and arms the grace timer.
    async fn start_termination(&mut self, process: &AsyncLinuxProcess) {
        let signal = self.policy.graceful_signal;
        tracing::debug!(
            pid = process.pid(),
            signal = signal.name(),
            "terminating job"
        );
        if let Err(error) = ignore_exited(process.signal_job(signal)) {
            self.emit(ProcessEvent::SupervisorError(error)).await;
        }
        self.termination = Some(Termination {
            grace_until: Instant::now().checked_add(self.policy.grace_period),
            hard_kill: self.policy.hard_kill || self.orphaned,
        });
    }

    /// The grace period elapsed while the primary still runs.
    async fn grace_elapsed(&mut self, process: &AsyncLinuxProcess) {
        let Some(termination) = self.termination.as_mut() else {
            return;
        };
        termination.grace_until = None;
        if !termination.hard_kill {
            return;
        }
        tracing::info!(
            pid = process.pid(),
            "grace period elapsed; hard-killing job"
        );
        if let Err(error) = ignore_exited(process.signal_job(SignalKind::Kill)) {
            self.emit(ProcessEvent::SupervisorError(error)).await;
        }
    }

    /// Reaction to a dropped receiver while the primary still runs: make
    /// sure the job ends (forced hard kill after the grace period).
    async fn abandon(&mut self, process: &AsyncLinuxProcess) {
        match self.termination.as_mut() {
            None => self.start_termination(process).await,
            Some(termination) => {
                if !termination.hard_kill {
                    termination.hard_kill = true;
                    if termination.grace_until.is_none() {
                        // Grace already spent without a kill: escalate now.
                        termination.grace_until = Some(Instant::now());
                    }
                }
            }
        }
    }
}

/// Which `select!` branch fired.
enum Wake {
    Exit(Result<(), ProcessError>),
    Cancel(bool),
    Deadline,
    Grace,
    Drain,
    Stdout(io::Result<usize>),
    Stderr(io::Result<usize>),
    Abandoned,
    Idle,
}

/// State of one supervisor task.
struct Run {
    process: AsyncLinuxProcess,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    cancel: watch::Receiver<bool>,
    deadline: Option<Instant>,
    control: Control,
    chunk: usize,
}

impl Run {
    async fn run(self, pending: Vec<ProcessEvent>) -> AsyncLinuxProcess {
        let Self {
            mut process,
            mut stdout,
            mut stderr,
            mut cancel,
            deadline,
            mut control,
            chunk,
        } = self;
        for event in pending {
            control.emit(event).await;
        }
        let mut out_buf = vec![0_u8; chunk];
        let mut err_buf = vec![0_u8; chunk];
        let mut cancel_open = true;
        let mut outcome: Option<ExitOutcome> = None;
        let mut drain_until: Option<Instant> = None;
        loop {
            if control.orphaned && !control.orphan_handled {
                control.orphan_handled = true;
                stdout = None;
                stderr = None;
                if outcome.is_none() {
                    control.abandon(&process).await;
                }
            }
            let stdout_open = stdout.is_some();
            let stderr_open = stderr.is_some();
            if outcome.is_some() && !stdout_open && !stderr_open {
                break;
            }
            let running = outcome.is_none();
            let triggers_armed = running && control.termination.is_none();
            let grace_until = control.termination.and_then(|t| t.grace_until);
            let watching_receiver = !control.orphaned;
            let wake = tokio::select! {
                ready = process.exit_ready(), if running => Wake::Exit(ready),
                changed = cancel.changed(), if triggers_armed && cancel_open => {
                    Wake::Cancel(changed.is_ok())
                }
                () = sleep_until(deadline), if triggers_armed && deadline.is_some() => Wake::Deadline,
                () = sleep_until(grace_until), if running && grace_until.is_some() => Wake::Grace,
                () = sleep_until(drain_until), if drain_until.is_some() => Wake::Drain,
                read = read_some(stdout.as_mut(), &mut out_buf), if stdout_open => Wake::Stdout(read),
                read = read_some(stderr.as_mut(), &mut err_buf), if stderr_open => Wake::Stderr(read),
                () = control.events.closed(), if watching_receiver => Wake::Abandoned,
                else => Wake::Idle,
            };
            match wake {
                Wake::Exit(Ok(())) => {
                    let hard_kill = control.termination.is_some_and(|t| t.hard_kill);
                    if hard_kill && process.is_group() {
                        // Primary exited but is unreaped: the group id is still
                        // ours, so surviving descendants can be killed safely.
                        if let Err(error) = ignore_exited(process.signal_job(SignalKind::Kill)) {
                            control.emit(ProcessEvent::SupervisorError(error)).await;
                        }
                    }
                    match process.try_status() {
                        Ok(Some(exit)) => {
                            tracing::debug!(pid = process.pid(), %exit, "supervised process exited");
                            outcome = Some(exit);
                            drain_until = Instant::now().checked_add(control.drain_timeout);
                        }
                        Ok(None) => tokio::task::yield_now().await,
                        Err(error) => {
                            control.emit(ProcessEvent::SupervisorError(error)).await;
                            break;
                        }
                    }
                }
                Wake::Exit(Err(error)) => {
                    control.emit(ProcessEvent::SupervisorError(error)).await;
                    break;
                }
                Wake::Cancel(false) => cancel_open = false,
                Wake::Cancel(true) => {
                    // Copy out first: the watch guard must not live across
                    // an await (it is not `Send`).
                    let requested = *cancel.borrow();
                    if requested {
                        control.emit(ProcessEvent::CancelRequested).await;
                        control.start_termination(&process).await;
                    }
                }
                Wake::Deadline => {
                    control.emit(ProcessEvent::Timeout).await;
                    control.start_termination(&process).await;
                }
                Wake::Grace => control.grace_elapsed(&process).await,
                Wake::Drain => {
                    tracing::debug!(
                        pid = process.pid(),
                        "output drain timeout; closing pipes held open by descendants"
                    );
                    stdout = None;
                    stderr = None;
                }
                Wake::Stdout(read) => {
                    let keep = forward(
                        &mut control,
                        read,
                        &out_buf,
                        "read(stdout)",
                        ProcessEvent::Stdout,
                    )
                    .await;
                    if !keep {
                        stdout = None;
                    }
                }
                Wake::Stderr(read) => {
                    let keep = forward(
                        &mut control,
                        read,
                        &err_buf,
                        "read(stderr)",
                        ProcessEvent::Stderr,
                    )
                    .await;
                    if !keep {
                        stderr = None;
                    }
                }
                Wake::Abandoned => {
                    tracing::debug!(
                        pid = process.pid(),
                        "event receiver dropped; abandoning job"
                    );
                    control.orphaned = true;
                }
                Wake::Idle => break,
            }
        }
        if let Some(exit) = outcome {
            control.emit(ProcessEvent::Exited(exit)).await;
        }
        process
    }
}

/// Emits one read result; returns whether the stream stays open.
async fn forward(
    control: &mut Control,
    read: io::Result<usize>,
    buf: &[u8],
    operation: &'static str,
    wrap: fn(Bytes) -> ProcessEvent,
) -> bool {
    match read {
        Ok(0) => false,
        Ok(n) => {
            let chunk = buf.get(..n).unwrap_or(buf);
            control.emit(wrap(Bytes::copy_from_slice(chunk))).await;
            true
        }
        Err(error) if error.kind() == io::ErrorKind::Interrupted => true,
        Err(error) => {
            control
                .emit(ProcessEvent::SupervisorError(io_error(operation, &error)))
                .await;
            false
        }
    }
}

/// Sleeps until `at`, or forever for `None`.
async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Reads from `reader`, or never completes for `None`. Cancel-safe.
async fn read_some<R: AsyncRead + Unpin>(
    reader: Option<&mut R>,
    buf: &mut [u8],
) -> io::Result<usize> {
    match reader {
        Some(reader) => reader.read(buf).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_job_linux::{LinuxJobGroup, LinuxProcess};
    use std::process::{Command, Stdio};

    /// Upper bound for any single test; a hang fails instead of blocking CI.
    const TEST_LIMIT: Duration = Duration::from_secs(30);

    fn sh(script: &str) -> Command {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(script)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn start(
        script: &str,
        config: SupervisorConfig,
    ) -> TestResult<(SupervisorHandle, mpsc::Receiver<ProcessEvent>)> {
        let (process, stdio) = LinuxProcess::spawn(&mut sh(script)).map_err(ctx("spawn"))?;
        let adopted = AsyncLinuxProcess::new(process).map_err(ctx("adopt"))?;
        Ok(Supervisor::spawn(
            adopted,
            stdio.stdout,
            stdio.stderr,
            config,
        ))
    }

    async fn collect(mut events: mpsc::Receiver<ProcessEvent>) -> TestResult<Vec<ProcessEvent>> {
        let gather = async {
            let mut all = Vec::new();
            while let Some(event) = events.recv().await {
                all.push(event);
            }
            all
        };
        tokio::time::timeout(TEST_LIMIT, gather)
            .await
            .map_err(ctx("collect events"))
    }

    fn stdout_text(events: &[ProcessEvent]) -> String {
        let mut text = Vec::new();
        for event in events {
            if let ProcessEvent::Stdout(chunk) = event {
                text.extend_from_slice(chunk);
            }
        }
        String::from_utf8_lossy(&text).into_owned()
    }

    fn stderr_text(events: &[ProcessEvent]) -> String {
        let mut text = Vec::new();
        for event in events {
            if let ProcessEvent::Stderr(chunk) = event {
                text.extend_from_slice(chunk);
            }
        }
        String::from_utf8_lossy(&text).into_owned()
    }

    fn last_exit(events: &[ProcessEvent]) -> TestResult<ExitOutcome> {
        match events.last() {
            Some(ProcessEvent::Exited(outcome)) => Ok(*outcome),
            other => Err(TestError::Unexpected(format!("last event: {other:?}"))),
        }
    }

    /// Kinds of control events (everything except output chunks), in order.
    fn control_kinds(events: &[ProcessEvent]) -> Vec<&'static str> {
        events
            .iter()
            .filter_map(|event| match event {
                ProcessEvent::Stdout(_) | ProcessEvent::Stderr(_) => None,
                ProcessEvent::Exited(_) => Some("exited"),
                ProcessEvent::Timeout => Some("timeout"),
                ProcessEvent::CancelRequested => Some("cancel"),
                ProcessEvent::SupervisorError(_) => Some("error"),
            })
            .collect()
    }

    async fn join(handle: SupervisorHandle) -> TestResult<AsyncLinuxProcess> {
        tokio::time::timeout(TEST_LIMIT, handle.join())
            .await
            .map_err(ctx("join timeout"))?
            .map_err(ctx("join"))
    }

    #[tokio::test]
    async fn test_exit_code_event() -> TestResult {
        let (handle, events) = start("exit 7", SupervisorConfig::default())?;
        let events = collect(events).await?;
        assert_eq!(control_kinds(&events), vec!["exited"]);
        assert_eq!(last_exit(&events)?, ExitOutcome::Exited(7));
        join(handle).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_stdout_in_order_and_exited_last() -> TestResult {
        let (handle, events) = start(
            "for i in 1 2 3 4 5; do echo line$i; done; echo oops >&2",
            SupervisorConfig::default(),
        )?;
        let events = collect(events).await?;
        assert_eq!(stdout_text(&events), "line1\nline2\nline3\nline4\nline5\n");
        assert_eq!(stderr_text(&events), "oops\n");
        assert_eq!(control_kinds(&events), vec!["exited"]);
        assert_eq!(last_exit(&events)?, ExitOutcome::Exited(0));
        join(handle).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_large_output_is_fully_drained_before_exit() -> TestResult {
        // More than a pipe buffer (64 KiB) with a small channel: exercises
        // backpressure without losing bytes.
        let config = SupervisorConfig {
            channel_capacity: 2,
            read_chunk_size: 1024,
            ..SupervisorConfig::default()
        };
        let (handle, events) = start("head -c 200000 /dev/zero", config)?;
        let events = collect(events).await?;
        let total: usize = events
            .iter()
            .map(|event| match event {
                ProcessEvent::Stdout(chunk) => chunk.len(),
                _ => 0,
            })
            .sum();
        assert_eq!(total, 200_000);
        assert_eq!(last_exit(&events)?, ExitOutcome::Exited(0));
        join(handle).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_deadline_emits_timeout_then_signaled() -> TestResult {
        let config = SupervisorConfig {
            deadline: Some(Duration::from_millis(200)),
            ..SupervisorConfig::default()
        };
        let (handle, events) = start("exec sleep 30", config)?;
        let events = collect(events).await?;
        assert_eq!(control_kinds(&events), vec!["timeout", "exited"]);
        match last_exit(&events)? {
            ExitOutcome::Signaled { signal: 15, .. } => {}
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        join(handle).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_cancel_emits_cancel_requested_then_termination() -> TestResult {
        let (handle, mut events) = start("echo ready; exec sleep 30", SupervisorConfig::default())?;
        // Wait until the job runs, then cancel.
        let first = tokio::time::timeout(TEST_LIMIT, events.recv())
            .await
            .map_err(ctx("first event"))?
            .ok_or(TestError::Missing("first event"))?;
        assert!(matches!(first, ProcessEvent::Stdout(_)), "{first:?}");
        let canceller = handle.canceller();
        canceller.cancel();
        assert!(canceller.is_cancelled());
        let events = collect(events).await?;
        assert_eq!(control_kinds(&events), vec!["cancel", "exited"]);
        match last_exit(&events)? {
            ExitOutcome::Signaled { signal: 15, .. } => {}
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        join(handle).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_ignored_graceful_signal_escalates_to_kill() -> TestResult {
        let (group, stdio) =
            LinuxJobGroup::spawn(&mut sh("trap '' TERM; echo ready; exec sleep 30"))
                .map_err(ctx("spawn group"))?;
        let adopted = AsyncLinuxProcess::from_group(group).map_err(ctx("adopt group"))?;
        let config = SupervisorConfig {
            termination: TerminationPolicy {
                graceful_signal: SignalKind::Term,
                grace_period: Duration::from_millis(300),
                hard_kill: true,
            },
            ..SupervisorConfig::default()
        };
        let (handle, mut events) = Supervisor::spawn(adopted, stdio.stdout, stdio.stderr, config);
        // "ready" is printed after the trap is installed: TERM cannot race it.
        let first = tokio::time::timeout(TEST_LIMIT, events.recv())
            .await
            .map_err(ctx("first event"))?
            .ok_or(TestError::Missing("first event"))?;
        assert!(matches!(first, ProcessEvent::Stdout(_)), "{first:?}");
        handle.cancel();
        let events = collect(events).await?;
        assert_eq!(control_kinds(&events), vec!["cancel", "exited"]);
        match last_exit(&events)? {
            ExitOutcome::Signaled { signal: 9, .. } => {}
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        join(handle).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_drain_timeout_bounds_pipe_held_by_descendant() -> TestResult {
        // The background `sleep` inherits stdout and keeps it open after the
        // shell exits; Exited must still arrive after the drain timeout.
        let config = SupervisorConfig {
            drain_timeout: Duration::from_millis(100),
            ..SupervisorConfig::default()
        };
        let started = Instant::now();
        let (handle, events) = start("sleep 5 & echo started", config)?;
        let events = collect(events).await?;
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "drain not bounded"
        );
        assert_eq!(stdout_text(&events), "started\n");
        assert_eq!(last_exit(&events)?, ExitOutcome::Exited(0));
        join(handle).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_dropped_receiver_terminates_and_reaps() -> TestResult {
        let config = SupervisorConfig {
            termination: TerminationPolicy {
                graceful_signal: SignalKind::Term,
                grace_period: Duration::from_millis(200),
                hard_kill: false,
            },
            ..SupervisorConfig::default()
        };
        let (handle, mut events) = start("trap '' TERM; echo ready; exec sleep 30", config)?;
        // "ready" is printed after the trap is installed: TERM cannot race it.
        let first = tokio::time::timeout(TEST_LIMIT, events.recv())
            .await
            .map_err(ctx("first event"))?
            .ok_or(TestError::Missing("first event"))?;
        assert!(matches!(first, ProcessEvent::Stdout(_)), "{first:?}");
        // No deadline, no cancel: dropping the receiver alone must end the job.
        drop(events);
        let mut process = join(handle).await?;
        // Abandonment forces the hard kill although the policy has none.
        let outcome = process
            .wait_timeout(Duration::from_secs(1))
            .await
            .map_err(ctx("cached outcome"))?
            .ok_or(TestError::Missing("reaped outcome"))?;
        assert!(
            matches!(outcome, ExitOutcome::Signaled { signal: 9, .. }),
            "{outcome:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_zero_capacity_and_chunk_are_clamped() -> TestResult {
        let config = SupervisorConfig {
            channel_capacity: 0,
            read_chunk_size: 0,
            ..SupervisorConfig::default()
        };
        let (handle, events) = start("printf abc", config)?;
        let events = collect(events).await?;
        assert_eq!(stdout_text(&events), "abc");
        assert_eq!(last_exit(&events)?, ExitOutcome::Exited(0));
        let pid = handle.pid();
        assert_eq!(join(handle).await?.pid(), pid);
        Ok(())
    }
}
