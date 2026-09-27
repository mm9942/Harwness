//! pidfd-backed live process handle (Job-Runtime-Doc §3.1, §9, Phase 2).
//!
//! # Authority model
//! A numeric PID is metadata. The authority for every live operation
//! (signal, wait, exit readiness) is the pidfd held inside
//! [`LinuxProcess`]. The pidfd is private: there is no accessor for the raw
//! descriptor and the type is not `Clone`, so exactly one owner controls the
//! process. The one exception is [`LinuxProcess::readiness_fd`], a duplicate
//! handed to an async reactor (`harw-job-tokio`) purely to learn *when* the
//! process exited; signalling and reaping still go through this handle.
//!
//! # The spawn → pidfd window
//! [`LinuxProcess::spawn`] uses `std::process::Command` and opens the pidfd
//! with `pidfd_open(child.id())` right after `spawn` returns. This is not a
//! PID-reuse race: the child is *our own unreaped child*, so its PID stays
//! allocated (at worst as a zombie) until we reap it — and we only ever reap
//! through `waitid(P_PIDFD)` on the pidfd itself. `std` never reaps a
//! `Child` on drop. The only remaining window is that the child already runs
//! (after `execve`) for the few microseconds before cgroup attachment in
//! [`crate::group::LinuxJobGroup`]; closing it requires the child to attach
//! itself pre-exec (the planned `harw-job-exec` trampoline), because doing it
//! in a `pre_exec` hook would need `unsafe` (Job-Runtime-Doc §24).
//!
//! `std::process::Command::spawn` returns only after the child's `execve`
//! succeeded (std reports exec failures through a CLOEXEC pipe), so
//! observations taken after [`LinuxProcess::spawn`] (e.g. `/proc/<pid>/exe`)
//! already describe the job program, not a pre-exec copy of the parent.

use std::os::fd::OwnedFd;
use std::process::{ChildStderr, ChildStdin, ChildStdout, Command};
use std::time::{Duration, Instant};

use harw_job_core::ExitOutcome;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::process::{
    Pid, PidfdFlags, Signal, WaitId, WaitIdOptions, WaitIdStatus, pidfd_open, pidfd_send_signal,
    waitid,
};

use crate::error::ProcessError;

/// Signals the job layer sends. A closed set: no arbitrary signal numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SignalKind {
    /// `SIGTERM` (15).
    Term,
    /// `SIGKILL` (9).
    Kill,
    /// `SIGINT` (2).
    Int,
    /// `SIGHUP` (1).
    Hup,
    /// `SIGQUIT` (3).
    Quit,
    /// `SIGUSR1`.
    Usr1,
    /// `SIGUSR2`.
    Usr2,
    /// `SIGSTOP`.
    Stop,
    /// `SIGCONT`.
    Cont,
}

impl SignalKind {
    pub(crate) fn to_rustix(self) -> Signal {
        match self {
            Self::Term => Signal::TERM,
            Self::Kill => Signal::KILL,
            Self::Int => Signal::INT,
            Self::Hup => Signal::HUP,
            Self::Quit => Signal::QUIT,
            Self::Usr1 => Signal::USR1,
            Self::Usr2 => Signal::USR2,
            Self::Stop => Signal::STOP,
            Self::Cont => Signal::CONT,
        }
    }

    /// Raw signal number on Linux (e.g. `15` for [`SignalKind::Term`]).
    #[must_use]
    pub fn as_raw(self) -> i32 {
        self.to_rustix().as_raw()
    }

    /// Conventional name such as `SIGTERM`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Term => "SIGTERM",
            Self::Kill => "SIGKILL",
            Self::Int => "SIGINT",
            Self::Hup => "SIGHUP",
            Self::Quit => "SIGQUIT",
            Self::Usr1 => "SIGUSR1",
            Self::Usr2 => "SIGUSR2",
            Self::Stop => "SIGSTOP",
            Self::Cont => "SIGCONT",
        }
    }
}

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

/// Validates a diagnostic PID and converts it for rustix.
pub(crate) fn to_pid(raw: u32) -> Result<Pid, ProcessError> {
    i32::try_from(raw)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or(ProcessError::InvalidPid { pid: raw })
}

/// A live Linux process controlled through a pidfd.
///
/// Not `Clone`; the pidfd is never exposed. Dropping the handle closes the
/// pidfd but neither kills nor reaps the process — an unreaped child stays a
/// zombie until this process exits. Owners are expected to [`wait`] (or
/// terminate through [`crate::group::LinuxJobGroup`]).
///
/// [`wait`]: LinuxProcess::wait
#[derive(Debug)]
pub struct LinuxProcess {
    pid: u32,
    pidfd: OwnedFd,
    /// `true` when this process is our child (spawned here): only then can
    /// `waitid` reap it and report a status.
    is_child: bool,
    /// Cached outcome once reaped; `waitid` succeeds only once.
    outcome: Option<ExitOutcome>,
}

impl LinuxProcess {
    /// Spawns `command` and takes pidfd authority over the child.
    ///
    /// Stdio configured as piped on `command` is returned in [`ChildStdio`].
    ///
    /// # Errors
    /// [`ProcessError::Spawn`] if `spawn` fails; if `pidfd_open` fails (e.g.
    /// [`ProcessError::Unsupported`] before Linux 5.3) the child is killed
    /// and reaped through `std` before the error is returned, so no
    /// uncontrolled child is left behind.
    pub fn spawn(command: &mut Command) -> Result<(Self, ChildStdio), ProcessError> {
        let child = command.spawn().map_err(ProcessError::Spawn)?;
        Self::from_child(child)
    }

    /// Takes pidfd authority over an already spawned, not yet reaped child.
    ///
    /// Consumes `child`: afterwards only this handle may wait for it.
    ///
    /// # Errors
    /// As [`LinuxProcess::spawn`] for the `pidfd_open` step.
    pub fn from_child(mut child: std::process::Child) -> Result<(Self, ChildStdio), ProcessError> {
        let pid = child.id();
        let opened = to_pid(pid).and_then(|rustix_pid| {
            pidfd_open(rustix_pid, PidfdFlags::empty())
                .map_err(|errno| ProcessError::from_errno("pidfd_open", pid, errno.raw_os_error()))
        });
        let pidfd = match opened {
            Ok(fd) => fd,
            Err(error) => {
                // Never leave an uncontrolled child behind; best effort.
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let stdio = ChildStdio {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
        };
        // `child` goes out of scope here: `Child` has no Drop side effects
        // (no kill, no reap), so the pidfd stays the only authority.
        Ok((
            Self {
                pid,
                pidfd,
                is_child: true,
                outcome: None,
            },
            stdio,
        ))
    }

    /// Opens a pidfd for an existing process that is *not* our child.
    ///
    /// Such a handle can signal and observe exit readiness, but `wait`
    /// reports [`ExitOutcome::Unknown`] (only the parent can read the
    /// status). A PID alone is no identity proof: callers that recover a
    /// process from persisted metadata must verify its identity *after*
    /// opening the pidfd — see the `recovery` module.
    ///
    /// # Errors
    /// [`ProcessError::AlreadyExited`] if no such process exists,
    /// [`ProcessError::InvalidPid`], or the classified `pidfd_open` error.
    pub fn open(pid: u32) -> Result<Self, ProcessError> {
        let rustix_pid = to_pid(pid)?;
        let pidfd = pidfd_open(rustix_pid, PidfdFlags::empty())
            .map_err(|errno| ProcessError::from_errno("pidfd_open", pid, errno.raw_os_error()))?;
        Ok(Self {
            pid,
            pidfd,
            is_child: false,
            outcome: None,
        })
    }

    /// Diagnostic PID (display/persistence only — not an authority).
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Whether the process has been reaped through this handle (its PID
    /// may be recycled from then on).
    pub(crate) fn is_reaped(&self) -> bool {
        self.outcome.is_some()
    }

    /// Whether this handle was created from our own child.
    #[must_use]
    pub fn is_child(&self) -> bool {
        self.is_child
    }

    /// Duplicates the pidfd for **async exit readiness only**
    /// (Job-Runtime-Doc §3.2, §13).
    ///
    /// An async reactor (Tokio's `AsyncFd` in `harw-job-tokio`) must own the
    /// descriptor it registers, so it gets a close-on-exec duplicate. The
    /// duplicate refers to this very process — a pidfd cannot be re-pointed,
    /// so PID reuse cannot redirect it. Callers use it only to wait for
    /// readability (readable ⇔ exited) and keep signalling and reaping through
    /// this handle ([`LinuxProcess::signal`], [`LinuxProcess::try_status`]),
    /// which is the only place that caches the exit outcome.
    ///
    /// # Errors
    /// [`ProcessError::Exhausted`] when no descriptor is available, otherwise
    /// the classified `fcntl(F_DUPFD_CLOEXEC)` error.
    pub fn readiness_fd(&self) -> Result<OwnedFd, ProcessError> {
        self.pidfd
            .try_clone()
            .map_err(|error| match error.raw_os_error() {
                Some(errno) => ProcessError::from_errno("dup(pidfd)", self.pid, errno),
                None => ProcessError::Os {
                    operation: "dup(pidfd)",
                    errno: 5, // EIO: std reported no OS error number.
                },
            })
    }

    /// Sends `signal` through the pidfd (`pidfd_send_signal`).
    ///
    /// # Errors
    /// [`ProcessError::AlreadyExited`] once the process has terminated
    /// (`ESRCH`), otherwise the classified OS error.
    pub fn signal(&self, signal: SignalKind) -> Result<(), ProcessError> {
        if self.outcome.is_some() {
            return Err(ProcessError::AlreadyExited { pid: self.pid });
        }
        pidfd_send_signal(&self.pidfd, signal.to_rustix()).map_err(|errno| {
            ProcessError::from_errno("pidfd_send_signal", self.pid, errno.raw_os_error())
        })
    }

    /// Whether the process has terminated, without reaping it (pidfd
    /// readable ⇔ exited; a zombie counts as exited).
    ///
    /// # Errors
    /// Classified `poll` error.
    pub fn has_exited(&self) -> Result<bool, ProcessError> {
        if self.outcome.is_some() {
            return Ok(true);
        }
        self.wait_exit_ready(Some(Duration::ZERO))
    }

    /// Blocks until the process has terminated or `timeout` elapsed, without
    /// reaping it. `None` waits indefinitely. Returns whether it exited.
    ///
    /// # Errors
    /// Classified `poll` error.
    pub fn wait_exit_ready(&self, timeout: Option<Duration>) -> Result<bool, ProcessError> {
        if self.outcome.is_some() {
            return Ok(true);
        }
        let deadline = timeout.map(|limit| Instant::now() + limit);
        loop {
            let remaining =
                deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
            let timespec = remaining
                .map(|left| {
                    Timespec::try_from(left).map_err(|_| ProcessError::InvalidArgument {
                        reason: "timeout does not fit a timespec".to_owned(),
                    })
                })
                .transpose()?;
            let mut fds = [PollFd::new(&self.pidfd, PollFlags::IN)];
            match poll(&mut fds, timespec.as_ref()) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(errno) => {
                    return Err(ProcessError::from_errno(
                        "poll(pidfd)",
                        self.pid,
                        errno.raw_os_error(),
                    ));
                }
            }
            let events = fds
                .first()
                .map(|fd| fd.revents())
                .unwrap_or(PollFlags::empty());
            if events.intersects(PollFlags::NVAL | PollFlags::ERR) {
                return Err(ProcessError::Os {
                    operation: "poll(pidfd)",
                    errno: 9, // EBADF: the pidfd is no longer valid.
                });
            }
            if events.intersects(PollFlags::IN | PollFlags::HUP) {
                return Ok(true);
            }
            if remaining.is_some_and(|left| left.is_zero()) {
                return Ok(false);
            }
        }
    }

    /// Blocks until the process terminates and returns its outcome
    /// (`waitid(P_PIDFD, WEXITED)`; reaps our child).
    ///
    /// Idempotent: after the first successful wait the cached outcome is
    /// returned. For a non-child handle the outcome is
    /// [`ExitOutcome::Unknown`] after exit readiness.
    ///
    /// # Errors
    /// Classified `waitid`/`poll` error.
    pub fn wait(&mut self) -> Result<ExitOutcome, ProcessError> {
        if let Some(outcome) = self.outcome {
            return Ok(outcome);
        }
        if !self.is_child {
            self.wait_exit_ready(None)?;
            self.outcome = Some(ExitOutcome::Unknown);
            return Ok(ExitOutcome::Unknown);
        }
        loop {
            match waitid(WaitId::PidFd(self.pidfd_borrow()), WaitIdOptions::EXITED) {
                Ok(Some(status)) => return Ok(self.record(&status)),
                // Without NOHANG `None` should not happen; poll again.
                Ok(None) => {
                    self.wait_exit_ready(None)?;
                }
                Err(rustix::io::Errno::INTR) => {}
                Err(errno) => {
                    return Err(ProcessError::from_errno(
                        "waitid(P_PIDFD)",
                        self.pid,
                        errno.raw_os_error(),
                    ));
                }
            }
        }
    }

    /// Non-blocking status: `Some(outcome)` once terminated (reaps our
    /// child), `None` while running.
    ///
    /// # Errors
    /// Classified `waitid`/`poll` error.
    pub fn try_status(&mut self) -> Result<Option<ExitOutcome>, ProcessError> {
        if let Some(outcome) = self.outcome {
            return Ok(Some(outcome));
        }
        if !self.is_child {
            return if self.has_exited()? {
                self.outcome = Some(ExitOutcome::Unknown);
                Ok(Some(ExitOutcome::Unknown))
            } else {
                Ok(None)
            };
        }
        loop {
            match waitid(
                WaitId::PidFd(self.pidfd_borrow()),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG,
            ) {
                Ok(Some(status)) => return Ok(Some(self.record(&status))),
                Ok(None) => return Ok(None),
                Err(rustix::io::Errno::INTR) => {}
                Err(errno) => {
                    return Err(ProcessError::from_errno(
                        "waitid(P_PIDFD)",
                        self.pid,
                        errno.raw_os_error(),
                    ));
                }
            }
        }
    }

    /// Waits at most `timeout` for termination; returns the outcome if the
    /// process exited in time (and reaps our child).
    ///
    /// # Errors
    /// Classified `waitid`/`poll` error.
    pub fn wait_timeout(&mut self, timeout: Duration) -> Result<Option<ExitOutcome>, ProcessError> {
        if self.wait_exit_ready(Some(timeout))? {
            // Exit readiness observed: a blocking wait returns immediately.
            self.wait().map(Some)
        } else {
            Ok(None)
        }
    }

    fn pidfd_borrow(&self) -> std::os::fd::BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.pidfd.as_fd()
    }

    fn record(&mut self, status: &WaitIdStatus) -> ExitOutcome {
        let outcome = outcome_from_status(status);
        self.outcome = Some(outcome);
        outcome
    }
}

/// Maps a `waitid` status to the platform-neutral [`ExitOutcome`].
fn outcome_from_status(status: &WaitIdStatus) -> ExitOutcome {
    if let Some(code) = status.exit_status() {
        return ExitOutcome::Exited(code);
    }
    if let Some(signal) = status.terminating_signal() {
        return ExitOutcome::Signaled {
            signal,
            core_dumped: status.dumped(),
        };
    }
    ExitOutcome::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    fn sh(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);
        command
    }

    #[test]
    fn test_signal_kind_raw_numbers() {
        assert_eq!(SignalKind::Term.as_raw(), 15);
        assert_eq!(SignalKind::Kill.as_raw(), 9);
        assert_eq!(SignalKind::Int.as_raw(), 2);
        assert_eq!(SignalKind::Hup.as_raw(), 1);
        assert_eq!(SignalKind::Term.name(), "SIGTERM");
    }

    #[test]
    fn test_to_pid_rejects_zero_and_overflow() {
        assert!(matches!(
            to_pid(0),
            Err(ProcessError::InvalidPid { pid: 0 })
        ));
        assert!(matches!(
            to_pid(u32::MAX),
            Err(ProcessError::InvalidPid { .. })
        ));
        assert!(matches!(
            LinuxProcess::open(0),
            Err(ProcessError::InvalidPid { .. })
        ));
    }

    #[test]
    fn test_wait_true_exits_zero() -> TestResult {
        let (mut process, _stdio) =
            LinuxProcess::spawn(&mut Command::new("true")).map_err(ctx("spawn true"))?;
        assert!(process.is_child());
        let outcome = process.wait().map_err(ctx("wait"))?;
        assert!(matches!(outcome, ExitOutcome::Exited(0)), "{outcome:?}");
        // Idempotent after reaping.
        let again = process.wait().map_err(ctx("wait again"))?;
        assert!(matches!(again, ExitOutcome::Exited(0)));
        Ok(())
    }

    #[test]
    fn test_wait_exit_code_three() -> TestResult {
        let (mut process, _stdio) = LinuxProcess::spawn(&mut sh("exit 3")).map_err(ctx("spawn"))?;
        let outcome = process.wait().map_err(ctx("wait"))?;
        assert!(matches!(outcome, ExitOutcome::Exited(3)), "{outcome:?}");
        Ok(())
    }

    #[test]
    fn test_signal_through_pidfd_yields_signaled_term() -> TestResult {
        let (mut process, _stdio) =
            LinuxProcess::spawn(Command::new("sleep").arg("30")).map_err(ctx("spawn sleep"))?;
        assert!(process.try_status().map_err(ctx("try_status"))?.is_none());
        process.signal(SignalKind::Term).map_err(ctx("signal"))?;
        let outcome = process.wait().map_err(ctx("wait"))?;
        match outcome {
            ExitOutcome::Signaled {
                signal,
                core_dumped,
            } => {
                assert_eq!(signal, 15);
                assert!(!core_dumped);
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        // Signalling a reaped process is a typed AlreadyExited.
        assert!(matches!(
            process.signal(SignalKind::Kill),
            Err(ProcessError::AlreadyExited { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_try_status_and_wait_timeout() -> TestResult {
        let (mut process, _stdio) =
            LinuxProcess::spawn(Command::new("sleep").arg("30")).map_err(ctx("spawn sleep"))?;
        let early = process
            .wait_timeout(Duration::from_millis(50))
            .map_err(ctx("wait_timeout"))?;
        assert!(early.is_none());
        assert!(!process.has_exited().map_err(ctx("has_exited"))?);
        process.signal(SignalKind::Kill).map_err(ctx("kill"))?;
        let outcome = process
            .wait_timeout(Duration::from_secs(10))
            .map_err(ctx("wait_timeout after kill"))?
            .ok_or(TestError::Missing("outcome after SIGKILL"))?;
        assert!(matches!(outcome, ExitOutcome::Signaled { signal: 9, .. }));
        Ok(())
    }

    #[test]
    fn test_spawn_returns_piped_stdout() -> TestResult {
        let mut command = sh("echo hello");
        command.stdout(Stdio::piped());
        let (mut process, stdio) = LinuxProcess::spawn(&mut command).map_err(ctx("spawn"))?;
        let stdout = stdio.stdout.ok_or(TestError::Missing("stdout pipe"))?;
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .map_err(ctx("read line"))?;
        assert_eq!(line.trim(), "hello");
        assert!(matches!(
            process.wait().map_err(ctx("wait"))?,
            ExitOutcome::Exited(0)
        ));
        Ok(())
    }

    #[test]
    fn test_readiness_fd_becomes_readable_on_exit() -> TestResult {
        let (mut process, _stdio) =
            LinuxProcess::spawn(Command::new("sleep").arg("30")).map_err(ctx("spawn sleep"))?;
        let readiness = process.readiness_fd().map_err(ctx("readiness_fd"))?;
        let zero =
            Timespec::try_from(Duration::ZERO).map_err(|_| TestError::Missing("timespec"))?;
        let mut fds = [PollFd::new(&readiness, PollFlags::IN)];
        poll(&mut fds, Some(&zero)).map_err(ctx("poll running"))?;
        let running = fds
            .first()
            .map(|fd| fd.revents())
            .unwrap_or(PollFlags::empty());
        assert!(!running.contains(PollFlags::IN), "readable while running");
        process.signal(SignalKind::Kill).map_err(ctx("kill"))?;
        let ten = Timespec::try_from(Duration::from_secs(10))
            .map_err(|_| TestError::Missing("timespec"))?;
        let mut fds = [PollFd::new(&readiness, PollFlags::IN)];
        poll(&mut fds, Some(&ten)).map_err(ctx("poll exited"))?;
        let exited = fds
            .first()
            .map(|fd| fd.revents())
            .unwrap_or(PollFlags::empty());
        assert!(
            exited.contains(PollFlags::IN),
            "duplicate not readable after exit"
        );
        // Reaping still goes through the handle, not the duplicate.
        let outcome = process.wait().map_err(ctx("wait"))?;
        assert!(matches!(outcome, ExitOutcome::Signaled { signal: 9, .. }));
        Ok(())
    }

    #[test]
    fn test_open_non_child_self_is_alive() -> TestResult {
        let process = LinuxProcess::open(std::process::id()).map_err(ctx("open self"))?;
        assert!(!process.is_child());
        assert!(!process.has_exited().map_err(ctx("has_exited"))?);
        Ok(())
    }

    #[test]
    fn test_spawn_missing_program_is_spawn_error() {
        let result = LinuxProcess::spawn(&mut Command::new("/nonexistent/harw-job-linux-test"));
        assert!(matches!(result, Err(ProcessError::Spawn(_))));
    }
}
