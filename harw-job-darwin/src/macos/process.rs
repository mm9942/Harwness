//! Live Darwin process handle: own process group, signals, waiting with a
//! timeout, group termination with escalation.
//!
//! # Authority model
//! Darwin has no pidfd. The authority for signalling is that the child is
//! **our own unreaped child**: its PID (and, as group leader, its process
//! group id) cannot be reused until we reap it. Reaping happens only in
//! `&mut self` methods of [`DarwinProcess`]; [`DarwinProcess::signal`] and
//! [`DarwinProcess::signal_group`] take `&self`, so the borrow checker rules
//! out a signal racing a reap on the same handle, and after the reap both
//! refuse with [`ProcessError::AlreadyExited`] instead of signalling a
//! possibly reused PID.
//!
//! # Exit watching
//! One watcher thread per process blocks in
//! `waitid(P_PID, pid, WEXITED | WNOWAIT)`, which observes the exit
//! **without reaping** and sends a token over a bounded channel;
//! [`DarwinProcess::wait_timeout`] is a `recv_timeout` on it. No polling
//! loop, no `unsafe` (kqueue's `kevent` is an `unsafe fn` in rustix 1.1 and
//! the workspace forbids unsafe code).
//!
//! Darwin quirk: `waitid` may also return for a **stopped** child even with
//! only `WEXITED` (the reason Go stopped using `waitid` on Darwin, golang
//! issue 19314). With `WNOWAIT` such a report is not consumed, so the
//! watcher re-checks at most every `STOPPED_RECHECK` (100 ms) while the child is
//! stopped. Tokens are hints: the owner confirms every exit by reaping.
//!
//! # Limits (Darwin has no cgroups)
//! The job boundary is the process group. A descendant that calls
//! `setsid(2)`/`setpgid(2)` leaves the group and escapes group signals.
//! [`DarwinProcess::terminate_group`] still kills the leader directly.

use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use harw_job_core::ExitOutcome;
use rustix::io::Errno;
use rustix::process::{Pid, WaitId, WaitIdOptions, kill_process, kill_process_group, waitid};

use super::{to_pid, to_rustix};
use crate::error::ProcessError;
use crate::recovery::DarwinRecoveryIdentity;
use crate::signal::SignalKind;
use crate::termination::{GroupSweep, TerminationPolicy, TerminationReport};

/// Minimum spacing of watcher re-checks while `waitid` keeps reporting a
/// non-exit state (stopped child, see module docs).
const STOPPED_RECHECK: Duration = Duration::from_millis(100);

/// What the watcher reports: `Ok(true)` exit observed, `Ok(false)` a
/// non-exit state change, `Err(errno)` waitid failed (watcher ends).
type WatchMessage = Result<bool, i32>;

/// The piped standard streams of a spawned child (only those configured as
/// `Stdio::piped()` are `Some`).
#[derive(Debug, Default)]
pub struct ChildStdio {
    /// Child's stdin.
    pub stdin: Option<ChildStdin>,
    /// Child's stdout.
    pub stdout: Option<ChildStdout>,
    /// Child's stderr.
    pub stderr: Option<ChildStderr>,
}

/// A live Darwin job process: leader of its own process group.
///
/// Not `Clone`. Dropping the handle neither kills nor reaps the process; an
/// unreaped child stays a zombie until this process exits, and its watcher
/// thread ends when the child exits. Owners are expected to
/// [`wait`](DarwinProcess::wait) or
/// [`terminate_group`](DarwinProcess::terminate_group).
#[derive(Debug)]
pub struct DarwinProcess {
    child: Child,
    pid: u32,
    raw_pid: Pid,
    watch: Watch,
    reaped: Option<ExitOutcome>,
}

/// State of the exit watcher as seen by the owner.
#[derive(Debug)]
enum Watch {
    /// Watcher running; tokens arrive on the receiver.
    Pending(Receiver<WatchMessage>),
    /// An exit was observed (not necessarily reaped yet).
    Exited,
    /// The watcher ended without observing an exit.
    Lost,
}

impl DarwinProcess {
    /// Spawns `command` as the leader of a new process group
    /// (`process_group(0)`, i.e. `setpgid(0, 0)` in the child — no
    /// `pre_exec`, no `unsafe`) and starts its exit watcher.
    ///
    /// Piped stdio is moved into the returned [`ChildStdio`].
    ///
    /// # Errors
    /// [`ProcessError::Spawn`] if spawning the child or the watcher thread
    /// fails (the child is then killed and reaped before returning).
    pub fn spawn(command: &mut Command) -> Result<(Self, ChildStdio), ProcessError> {
        command.process_group(0);
        let mut child = command.spawn().map_err(ProcessError::Spawn)?;
        let pid = child.id();
        let stdio = ChildStdio {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
        };
        let raw_pid = match to_pid(pid) {
            Ok(raw_pid) => raw_pid,
            Err(error) => {
                abandon(&mut child);
                return Err(error);
            }
        };
        let (sender, receiver) = mpsc::sync_channel::<WatchMessage>(1);
        let watcher = thread::Builder::new()
            .name(format!("harw-darwin-exit-{pid}"))
            .spawn(move || watch_exit(raw_pid, &sender));
        if let Err(source) = watcher {
            abandon(&mut child);
            return Err(ProcessError::Spawn(source));
        }
        Ok((
            Self {
                child,
                pid,
                raw_pid,
                watch: Watch::Pending(receiver),
                reaped: None,
            },
            stdio,
        ))
    }

    /// Diagnostic PID (also the process group id).
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Process group id of the job (equals [`DarwinProcess::pid`]).
    #[must_use]
    pub fn process_group_id(&self) -> u32 {
        self.pid
    }

    /// Identity to persist for recovery after a runner restart. The start
    /// time is `None` on Darwin (see [`crate::recovery`]).
    #[must_use]
    pub fn recovery_identity(&self) -> DarwinRecoveryIdentity {
        DarwinRecoveryIdentity::pid_only(self.pid)
    }

    /// Sends `signal` to the leader only.
    ///
    /// # Errors
    /// [`ProcessError::AlreadyExited`] once reaped (nothing is sent) or on
    /// `ESRCH`; [`ProcessError::PermissionDenied`] on `EPERM`.
    pub fn signal(&self, signal: SignalKind) -> Result<(), ProcessError> {
        if self.reaped.is_some() {
            return Err(ProcessError::AlreadyExited { pid: self.pid });
        }
        kill_process(self.raw_pid, to_rustix(signal))
            .map_err(|errno| ProcessError::from_errno("kill", self.pid, errno.raw_os_error()))
    }

    /// Sends `signal` to the whole process group (`killpg`).
    ///
    /// Only valid while the leader is unreaped: that pins the group id.
    ///
    /// # Errors
    /// [`ProcessError::AlreadyExited`] once reaped (nothing is sent) or when
    /// the group has no members (`ESRCH`); [`ProcessError::PermissionDenied`]
    /// on `EPERM`.
    pub fn signal_group(&self, signal: SignalKind) -> Result<(), ProcessError> {
        if self.reaped.is_some() {
            return Err(ProcessError::AlreadyExited { pid: self.pid });
        }
        kill_process_group(self.raw_pid, to_rustix(signal))
            .map_err(|errno| ProcessError::from_errno("killpg", self.pid, errno.raw_os_error()))
    }

    /// Blocks until the leader exits, reaps it and returns its outcome.
    /// Idempotent: later calls return the cached outcome.
    ///
    /// # Errors
    /// [`ProcessError::NotAChild`] if something else reaped the child.
    pub fn wait(&mut self) -> Result<ExitOutcome, ProcessError> {
        if let Some(outcome) = self.reaped {
            return Ok(outcome);
        }
        let status = self
            .child
            .wait()
            .map_err(|source| ProcessError::from_io("wait", self.pid, source))?;
        Ok(self.record(status))
    }

    /// Non-blocking: reaps and returns the outcome if the leader has exited.
    ///
    /// # Errors
    /// As [`DarwinProcess::wait`].
    pub fn try_status(&mut self) -> Result<Option<ExitOutcome>, ProcessError> {
        if let Some(outcome) = self.reaped {
            return Ok(Some(outcome));
        }
        match self.child.try_wait() {
            Ok(Some(status)) => Ok(Some(self.record(status))),
            Ok(None) => Ok(None),
            Err(source) => Err(ProcessError::from_io("try_wait", self.pid, source)),
        }
    }

    /// Waits up to `timeout` for the leader to exit (kernel-blocking through
    /// the watcher, no polling), then reaps it. `Ok(None)` means it is still
    /// running.
    ///
    /// # Errors
    /// As [`DarwinProcess::wait`], plus [`ProcessError::WatcherLost`] if the
    /// watcher ended without observing an exit.
    pub fn wait_timeout(&mut self, timeout: Duration) -> Result<Option<ExitOutcome>, ProcessError> {
        if let Some(outcome) = self.reaped {
            return Ok(Some(outcome));
        }
        let observed = self.observe_exit(timeout)?;
        if observed {
            // Exit observed: the reaping wait returns immediately.
            return self.wait().map(Some);
        }
        // Timeout. Tokens are hints; reaping is authoritative.
        self.try_status()
    }

    /// Terminates the process group per `policy` (see
    /// [`TerminationPolicy`]): graceful signal to the group, wait up to the
    /// grace period for the leader, then — if `hard_kill` — `SIGKILL` to the
    /// group and to the leader directly, **before** reaping (the unreaped
    /// leader keeps the group id pinned). Finally reaps the leader.
    ///
    /// # Errors
    /// Signal errors other than "already gone"; wait errors.
    pub fn terminate_group(
        &mut self,
        policy: &TerminationPolicy,
    ) -> Result<TerminationReport, ProcessError> {
        if let Some(outcome) = self.reaped {
            return Ok(TerminationReport {
                outcome: Some(outcome),
                escalated: false,
                sweep: GroupSweep::NotSent,
            });
        }
        match self.signal_group(policy.graceful_signal) {
            Ok(()) | Err(ProcessError::AlreadyExited { .. }) => {}
            // Darwin may answer EPERM for a group holding only zombies.
            Err(error @ ProcessError::PermissionDenied { .. }) => {
                if !self.observe_exit(Duration::ZERO)? {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
        let exited = self.observe_exit(policy.grace_period)?;
        let escalated = !exited;
        let sweep = if policy.hard_kill {
            let sweep = match self.signal_group(SignalKind::Kill) {
                Ok(()) => GroupSweep::Delivered,
                Err(ProcessError::AlreadyExited { .. }) => GroupSweep::NothingLeft,
                Err(ProcessError::PermissionDenied { .. }) if exited => {
                    GroupSweep::DeniedAfterLeaderExit
                }
                Err(error) => return Err(error),
            };
            if !exited {
                // The leader may have left the group (setsid); it is still
                // our unreaped child, so signalling it directly is safe.
                match self.signal(SignalKind::Kill) {
                    Ok(()) | Err(ProcessError::AlreadyExited { .. }) => {}
                    Err(error) => return Err(error),
                }
            }
            sweep
        } else {
            GroupSweep::NotSent
        };
        let outcome = if exited || policy.hard_kill {
            Some(self.wait()?)
        } else {
            self.try_status()?
        };
        Ok(TerminationReport {
            outcome,
            escalated,
            sweep,
        })
    }

    /// Waits up to `timeout` for an exit token from the watcher **without
    /// reaping**. `Ok(false)` on timeout.
    fn observe_exit(&mut self, timeout: Duration) -> Result<bool, ProcessError> {
        let receiver = match &self.watch {
            Watch::Pending(receiver) => receiver,
            Watch::Exited => return Ok(true),
            Watch::Lost => return Err(ProcessError::WatcherLost { pid: self.pid }),
        };
        let deadline = Instant::now().checked_add(timeout);
        loop {
            let message = match deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    receiver.recv_timeout(remaining)
                }
                None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match message {
                Ok(Ok(true)) => {
                    self.watch = Watch::Exited;
                    return Ok(true);
                }
                // Non-exit state change (stopped child): keep waiting.
                Ok(Ok(false)) => {}
                Ok(Err(errno)) => {
                    self.watch = Watch::Lost;
                    return Err(ProcessError::from_errno("waitid", self.pid, errno));
                }
                Err(RecvTimeoutError::Timeout) => return Ok(false),
                Err(RecvTimeoutError::Disconnected) => {
                    self.watch = Watch::Lost;
                    return Err(ProcessError::WatcherLost { pid: self.pid });
                }
            }
        }
    }

    fn record(&mut self, status: ExitStatus) -> ExitOutcome {
        let outcome = outcome_from_status(status);
        self.reaped = Some(outcome);
        outcome
    }
}

/// Maps a std exit status to the platform-neutral outcome.
fn outcome_from_status(status: ExitStatus) -> ExitOutcome {
    if let Some(code) = status.code() {
        ExitOutcome::Exited(code)
    } else if let Some(signal) = status.signal() {
        ExitOutcome::Signaled {
            signal,
            core_dumped: status.core_dumped(),
        }
    } else {
        ExitOutcome::Unknown
    }
}

/// Kills and reaps a child whose handle could not be completed.
fn abandon(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Watcher thread body: observe (not reap) the exit of `pid`.
fn watch_exit(pid: Pid, sender: &SyncSender<WatchMessage>) {
    loop {
        match waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        ) {
            Ok(status) => {
                let exited = status
                    .as_ref()
                    .is_some_and(|status| status.exited() || status.killed() || status.dumped());
                match sender.try_send(Ok(exited)) {
                    // Full: an earlier token is still unread; that is enough.
                    Ok(()) | Err(TrySendError::Full(_)) => {}
                    // Owner dropped the handle.
                    Err(TrySendError::Disconnected(_)) => return,
                }
                if exited {
                    return;
                }
                thread::sleep(STOPPED_RECHECK);
            }
            Err(errno) if errno == Errno::INTR => {}
            Err(errno) => {
                // ECHILD after the owner reaped (or another error): report
                // and end. A full channel already holds a token.
                let _ = sender.try_send(Err(errno.raw_os_error()));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ChildStdio, DarwinProcess};
    use crate::error::ProcessError;
    use crate::recovery::IdentityCheck;
    use crate::report::DarwinSandbox;
    use crate::signal::SignalKind;
    use crate::termination::{TerminationPolicy, TerminationReport};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_job_core::{ExitOutcome, SandboxProfileName};
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    fn spawn(program: &str, args: &[&str]) -> TestResult<(DarwinProcess, ChildStdio)> {
        let mut command = Command::new(program);
        command.args(args).stdout(Stdio::piped());
        DarwinProcess::spawn(&mut command).map_err(ctx("spawn"))
    }

    /// Reads one line from the child's stdout (readiness handshake).
    fn read_line(stdio: &mut ChildStdio) -> TestResult<String> {
        let stdout = stdio.stdout.take().ok_or(TestError::Missing("stdout"))?;
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .map_err(ctx("read line"))?;
        Ok(line.trim().to_owned())
    }

    const TERM: ExitOutcome = ExitOutcome::Signaled {
        signal: 15,
        core_dumped: false,
    };
    const KILL: ExitOutcome = ExitOutcome::Signaled {
        signal: 9,
        core_dumped: false,
    };

    #[test]
    fn test_true_exits_zero_and_wait_is_idempotent() -> TestResult {
        let (mut process, _stdio) = spawn("/usr/bin/true", &[])?;
        assert_eq!(process.process_group_id(), process.pid());
        assert_eq!(process.wait().map_err(ctx("wait"))?, ExitOutcome::Exited(0));
        assert_eq!(
            process.wait().map_err(ctx("wait again"))?,
            ExitOutcome::Exited(0)
        );
        Ok(())
    }

    #[test]
    fn test_exit_code_is_reported() -> TestResult {
        let (mut process, _stdio) = spawn("/bin/sh", &["-c", "exit 3"])?;
        let outcome = process
            .wait_timeout(Duration::from_secs(30))
            .map_err(ctx("wait_timeout"))?;
        assert_eq!(outcome, Some(ExitOutcome::Exited(3)));
        Ok(())
    }

    #[test]
    fn test_term_signal_is_reported_as_signaled_15() -> TestResult {
        let (mut process, _stdio) = spawn("/bin/sleep", &["30"])?;
        process.signal(SignalKind::Term).map_err(ctx("signal"))?;
        assert_eq!(process.wait().map_err(ctx("wait"))?, TERM);
        Ok(())
    }

    #[test]
    fn test_wait_timeout_expires_then_kill_ends_the_process() -> TestResult {
        let (mut process, _stdio) = spawn("/bin/sleep", &["30"])?;
        let early = process
            .wait_timeout(Duration::from_millis(200))
            .map_err(ctx("wait_timeout"))?;
        assert_eq!(early, None, "sleep 30 must still run");
        process.signal(SignalKind::Kill).map_err(ctx("kill"))?;
        let outcome = process
            .wait_timeout(Duration::from_secs(30))
            .map_err(ctx("wait after kill"))?;
        assert_eq!(outcome, Some(KILL));
        Ok(())
    }

    #[test]
    fn test_signal_after_reap_is_refused() -> TestResult {
        let (mut process, _stdio) = spawn("/usr/bin/true", &[])?;
        process.wait().map_err(ctx("wait"))?;
        assert!(matches!(
            process.signal(SignalKind::Term),
            Err(ProcessError::AlreadyExited { .. })
        ));
        assert!(matches!(
            process.signal_group(SignalKind::Kill),
            Err(ProcessError::AlreadyExited { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_terminate_group_graceful_within_grace() -> TestResult {
        let (mut process, _stdio) = spawn("/bin/sleep", &["30"])?;
        let policy = TerminationPolicy {
            graceful_signal: SignalKind::Term,
            grace_period: Duration::from_secs(10),
            hard_kill: true,
        };
        let report = process.terminate_group(&policy).map_err(ctx("terminate"))?;
        assert_eq!(report.outcome, Some(TERM));
        assert!(!report.escalated);
        Ok(())
    }

    #[test]
    fn test_terminate_group_escalates_to_kill_when_term_is_ignored() -> TestResult {
        // The shell ignores TERM, the handshake proves the trap is installed,
        // `exec` keeps the ignored disposition for sleep.
        let (mut process, mut stdio) = spawn(
            "/bin/sh",
            &["-c", "trap '' TERM; echo ready; exec /bin/sleep 30"],
        )?;
        assert_eq!(read_line(&mut stdio)?, "ready");
        let policy = TerminationPolicy {
            graceful_signal: SignalKind::Term,
            grace_period: Duration::from_millis(300),
            hard_kill: true,
        };
        let report = process.terminate_group(&policy).map_err(ctx("terminate"))?;
        let TerminationReport {
            outcome, escalated, ..
        } = report;
        assert_eq!(outcome, Some(KILL));
        assert!(escalated);
        Ok(())
    }

    #[test]
    fn test_terminate_without_hard_kill_leaves_a_survivor_running() -> TestResult {
        let (mut process, mut stdio) = spawn(
            "/bin/sh",
            &["-c", "trap '' TERM; echo ready; exec /bin/sleep 30"],
        )?;
        assert_eq!(read_line(&mut stdio)?, "ready");
        let policy = TerminationPolicy {
            graceful_signal: SignalKind::Term,
            grace_period: Duration::from_millis(200),
            hard_kill: false,
        };
        let report = process.terminate_group(&policy).map_err(ctx("terminate"))?;
        assert_eq!(report.outcome, None);
        assert!(report.escalated);
        process
            .signal_group(SignalKind::Kill)
            .map_err(ctx("cleanup"))?;
        assert_eq!(process.wait().map_err(ctx("wait"))?, KILL);
        Ok(())
    }

    #[test]
    fn test_recovered_identity_is_never_signalled() -> TestResult {
        let (mut process, _stdio) = spawn("/bin/sleep", &["30"])?;
        let identity = process.recovery_identity();
        assert_eq!(identity.start_time, None);
        assert!(matches!(
            identity.check().map_err(ctx("check"))?,
            IdentityCheck::Unverifiable { .. }
        ));
        assert!(matches!(
            identity.signal(SignalKind::Kill),
            Err(ProcessError::IdentityUnverifiable { .. })
        ));
        // Refused means refused: the process is still running.
        assert_eq!(
            process
                .wait_timeout(Duration::from_millis(200))
                .map_err(ctx("still running"))?,
            None
        );
        process.signal(SignalKind::Kill).map_err(ctx("cleanup"))?;
        assert_eq!(process.wait().map_err(ctx("wait"))?, KILL);

        // After exit and reap the PID is gone (barring immediate reuse).
        assert!(matches!(
            identity.check().map_err(ctx("check gone"))?,
            IdentityCheck::Gone | IdentityCheck::Unverifiable { .. }
        ));
        Ok(())
    }

    #[test]
    fn test_sandbox_exec_confines_writes_to_the_workspace() -> TestResult {
        let workspace = tempfile::tempdir().map_err(ctx("workspace"))?;
        let outside = tempfile::tempdir().map_err(ctx("outside"))?;
        let workspace_root = std::fs::canonicalize(workspace.path()).map_err(ctx("canon ws"))?;
        let outside_root = std::fs::canonicalize(outside.path()).map_err(ctx("canon out"))?;
        let sandbox = DarwinSandbox::sandbox_exec(&workspace_root);

        let run = |target: &std::path::Path| -> TestResult<ExitOutcome> {
            let mut command = Command::new("/bin/sh");
            command
                .arg("-c")
                .arg("echo x > \"$1\"")
                .arg("sh")
                .arg(target);
            let mut wrapped = sandbox
                .wrap(&SandboxProfileName::NoNetwork, &command)
                .map_err(ctx("wrap"))?;
            wrapped.stderr(Stdio::null());
            let (mut process, _stdio) = DarwinProcess::spawn(&mut wrapped).map_err(ctx("spawn"))?;
            process.wait().map_err(ctx("wait"))
        };

        let inside_file = workspace_root.join("inside.txt");
        assert_eq!(run(&inside_file)?, ExitOutcome::Exited(0));
        assert!(inside_file.exists());

        let outside_file = outside_root.join("outside.txt");
        let denied = run(&outside_file)?;
        assert!(!denied.is_success(), "{denied}");
        assert!(!outside_file.exists());
        Ok(())
    }
}
