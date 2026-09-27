//! Async pidfd exit readiness (Job-Runtime-Doc §3.2).
//!
//! Lifecycle: spawn → `pidfd_open` (in `harw-job-linux`) → `AsyncFd` waits
//! for readiness on a duplicate of the pidfd → `waitid(P_PIDFD)` through
//! the `harw-job-linux` handle → typed [`ExitOutcome`].
//!
//! The duplicate is registered with the Tokio reactor only to learn *when*
//! the process exited. The authority for signals and reaping stays with the
//! [`LinuxProcess`] inside [`SupervisedTarget`].

use std::fmt;
use std::io;
use std::os::fd::OwnedFd;
use std::time::Duration;

use harw_job_core::ExitOutcome;
use harw_job_linux::{
    LinuxJobGroup, LinuxProcess, ProcessError, SignalKind, TerminationPolicy, TerminationReport,
};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;

/// What an [`AsyncLinuxProcess`] controls.
#[derive(Debug)]
pub enum SupervisedTarget {
    /// A single process: every signal goes through its pidfd.
    Process(LinuxProcess),
    /// A process-group job: job-wide signals go to the whole process group
    /// *and* to the primary through its pidfd (in case it left the group).
    Group(LinuxJobGroup),
}

impl SupervisedTarget {
    /// The primary process.
    #[must_use]
    pub fn primary(&self) -> &LinuxProcess {
        match self {
            Self::Process(process) => process,
            Self::Group(group) => group.primary(),
        }
    }

    /// The primary process, mutably.
    pub fn primary_mut(&mut self) -> &mut LinuxProcess {
        match self {
            Self::Process(process) => process,
            Self::Group(group) => group.primary_mut(),
        }
    }
}

/// Registering a target with the Tokio reactor failed. The untouched
/// target is handed back, so no process is left without an owner.
#[derive(Debug)]
pub struct AdoptError {
    /// Why registration failed.
    pub error: ProcessError,
    /// The target that could not be adopted (still alive, not reaped).
    /// Boxed to keep the `Result` small.
    pub target: Box<SupervisedTarget>,
}

impl fmt::Display for AdoptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cannot adopt process {} for async supervision: {}",
            self.target.primary().pid(),
            self.error
        )
    }
}

impl std::error::Error for AdoptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Maps a Tokio/std I/O error to the Linux error domain (§22).
pub(crate) fn io_error(operation: &'static str, error: &io::Error) -> ProcessError {
    // Linux UAPI errno values (identical on every supported architecture).
    const EPERM: i32 = 1;
    const EIO: i32 = 5;
    const ENOMEM: i32 = 12;
    const EACCES: i32 = 13;
    const ENFILE: i32 = 23;
    const EMFILE: i32 = 24;
    const ENOSPC: i32 = 28;
    const ENOSYS: i32 = 38;
    const EOPNOTSUPP: i32 = 95;
    match error.raw_os_error() {
        Some(EPERM | EACCES) => ProcessError::PermissionDenied { operation },
        Some(ENOMEM | ENFILE | EMFILE | ENOSPC) => ProcessError::Exhausted { operation },
        Some(ENOSYS | EOPNOTSUPP) => ProcessError::Unsupported { operation },
        Some(errno) => ProcessError::Os { operation, errno },
        None => ProcessError::Os {
            operation,
            errno: EIO,
        },
    }
}

/// Treats "already exited" as success: signalling a job that is gone is not
/// a failure of termination.
pub(crate) fn ignore_exited(result: Result<(), ProcessError>) -> Result<(), ProcessError> {
    match result {
        Ok(()) | Err(ProcessError::AlreadyExited { .. }) => Ok(()),
        Err(other) => Err(other),
    }
}

/// A Linux process (or process-group job) with async exit readiness.
///
/// Dropping it closes both descriptors but neither kills nor reaps the
/// process (same contract as [`LinuxProcess`]).
#[derive(Debug)]
pub struct AsyncLinuxProcess {
    target: SupervisedTarget,
    /// Duplicate of the primary's pidfd, registered for readability only.
    readiness: AsyncFd<OwnedFd>,
}

impl AsyncLinuxProcess {
    /// Adopts a single process.
    ///
    /// Must be called inside a Tokio runtime with the I/O driver enabled
    /// (Tokio panics otherwise).
    ///
    /// # Errors
    /// [`AdoptError`] (with the process handed back) if the pidfd cannot be
    /// duplicated or registered with the reactor.
    pub fn new(process: LinuxProcess) -> Result<Self, AdoptError> {
        Self::from_target(SupervisedTarget::Process(process))
    }

    /// Adopts a process-group job; termination then signals the whole group.
    ///
    /// # Errors
    /// As [`AsyncLinuxProcess::new`].
    pub fn from_group(group: LinuxJobGroup) -> Result<Self, AdoptError> {
        Self::from_target(SupervisedTarget::Group(group))
    }

    /// Adopts `target`.
    ///
    /// # Errors
    /// As [`AsyncLinuxProcess::new`].
    pub fn from_target(target: SupervisedTarget) -> Result<Self, AdoptError> {
        let duplicate = match target.primary().readiness_fd() {
            Ok(fd) => fd,
            Err(error) => {
                return Err(AdoptError {
                    error,
                    target: Box::new(target),
                });
            }
        };
        match AsyncFd::with_interest(duplicate, Interest::READABLE) {
            Ok(readiness) => Ok(Self { target, readiness }),
            Err(error) => Err(AdoptError {
                error: io_error("epoll_ctl(pidfd)", &error),
                target: Box::new(target),
            }),
        }
    }

    /// Diagnostic PID of the primary.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.target.primary().pid()
    }

    /// The supervised target.
    #[must_use]
    pub fn target(&self) -> &SupervisedTarget {
        &self.target
    }

    /// Deregisters from the reactor and returns the target.
    #[must_use]
    pub fn into_target(self) -> SupervisedTarget {
        self.target
    }

    /// Whether this adopts a process-group job.
    pub(crate) fn is_group(&self) -> bool {
        matches!(self.target, SupervisedTarget::Group(_))
    }

    /// Sends `signal` to the primary through its pidfd.
    ///
    /// # Errors
    /// As [`LinuxProcess::signal`].
    pub fn signal(&self, signal: SignalKind) -> Result<(), ProcessError> {
        self.target.primary().signal(signal)
    }

    /// Sends `signal` to the whole job: the process group (for a group
    /// target, while the primary is unreaped) and the primary via pidfd.
    ///
    /// # Errors
    /// Group-signal errors other than "already exited", or the result of the
    /// pidfd signal to the primary.
    pub fn signal_job(&self, signal: SignalKind) -> Result<(), ProcessError> {
        match &self.target {
            SupervisedTarget::Process(process) => process.signal(signal),
            SupervisedTarget::Group(group) => {
                ignore_exited(group.signal_group(signal))?;
                group.primary().signal(signal)
            }
        }
    }

    /// Whether the primary has terminated (non-blocking, does not reap).
    ///
    /// # Errors
    /// As [`LinuxProcess::has_exited`].
    pub fn has_exited(&self) -> Result<bool, ProcessError> {
        self.target.primary().has_exited()
    }

    /// Waits asynchronously until the primary has terminated, without
    /// reaping it. Cancel-safe.
    ///
    /// # Errors
    /// Reactor or `poll` errors.
    pub async fn exit_ready(&self) -> Result<(), ProcessError> {
        loop {
            if self.has_exited()? {
                return Ok(());
            }
            let mut guard = self
                .readiness
                .readable()
                .await
                .map_err(|error| io_error("epoll_wait(pidfd)", &error))?;
            if self.has_exited()? {
                return Ok(());
            }
            // Spurious wake-up: the reactor reported readiness the pidfd no
            // longer has; wait for the next edge.
            guard.clear_ready();
        }
    }

    /// Non-blocking status: `Some(outcome)` once terminated (reaps our
    /// child), `None` while running.
    ///
    /// # Errors
    /// As [`LinuxProcess::try_status`].
    pub fn try_status(&mut self) -> Result<Option<ExitOutcome>, ProcessError> {
        self.target.primary_mut().try_status()
    }

    /// Waits asynchronously for termination and reaps the primary.
    /// Idempotent and cancel-safe.
    ///
    /// # Errors
    /// Reactor, `poll` or `waitid` errors.
    pub async fn wait(&mut self) -> Result<ExitOutcome, ProcessError> {
        loop {
            self.exit_ready().await?;
            if let Some(outcome) = self.try_status()? {
                return Ok(outcome);
            }
            // Readable but not yet reapable (should not happen); do not spin.
            tokio::task::yield_now().await;
        }
    }

    /// Waits at most `timeout`; returns the outcome if the primary exited in
    /// time (and reaps it).
    ///
    /// # Errors
    /// As [`AsyncLinuxProcess::wait`].
    pub async fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<ExitOutcome>, ProcessError> {
        match tokio::time::timeout(timeout, self.wait()).await {
            Ok(result) => result.map(Some),
            Err(_elapsed) => Ok(None),
        }
    }

    /// Async layered termination (Job-Runtime-Doc §10), mirroring
    /// [`LinuxJobGroup::terminate`]: graceful signal to the job → await exit
    /// readiness up to the grace period → SIGKILL the job → reap.
    ///
    /// With `hard_kill`, a group target's surviving descendants are killed
    /// even when the primary exited gracefully (the primary is still
    /// unreaped then, so the group id is still ours). Job cgroups are not
    /// handled here; use the synchronous `LinuxJobGroup` path for them.
    ///
    /// # Errors
    /// Signal, reactor or wait errors of the primary.
    pub async fn terminate(
        &mut self,
        policy: &TerminationPolicy,
    ) -> Result<TerminationReport, ProcessError> {
        let mut escalated = false;
        if !self.has_exited()? {
            tracing::debug!(
                pid = self.pid(),
                signal = policy.graceful_signal.name(),
                "graceful async job termination"
            );
            ignore_exited(self.signal_job(policy.graceful_signal))?;
            match tokio::time::timeout(policy.grace_period, self.exit_ready()).await {
                Ok(ready) => ready?,
                Err(_elapsed) => {
                    if !policy.hard_kill {
                        return Ok(TerminationReport {
                            outcome: None,
                            escalated: false,
                        });
                    }
                    escalated = true;
                    tracing::info!(pid = self.pid(), "grace period elapsed; hard-killing job");
                }
            }
        }
        if policy.hard_kill {
            ignore_exited(self.signal_job(SignalKind::Kill))?;
        }
        let outcome = self.wait().await?;
        Ok(TerminationReport {
            outcome: Some(outcome),
            escalated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::process::{Command, Stdio};
    use tokio::io::AsyncBufReadExt;

    fn spawn(script: &str) -> TestResult<(AsyncLinuxProcess, Option<std::process::ChildStdout>)> {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script).stdout(Stdio::piped());
        let (process, stdio) = LinuxProcess::spawn(&mut command).map_err(ctx("spawn"))?;
        let adopted = AsyncLinuxProcess::new(process).map_err(ctx("adopt"))?;
        Ok((adopted, stdio.stdout))
    }

    async fn first_line(stdout: Option<std::process::ChildStdout>) -> TestResult<String> {
        let stdout = stdout.ok_or(TestError::Missing("stdout pipe"))?;
        let stdout = tokio::process::ChildStdout::from_std(stdout).map_err(ctx("from_std"))?;
        let mut line = String::new();
        tokio::io::BufReader::new(stdout)
            .read_line(&mut line)
            .await
            .map_err(ctx("read line"))?;
        Ok(line.trim().to_owned())
    }

    #[test]
    fn test_io_error_classification() {
        let exhausted = io_error("op", &io::Error::from_raw_os_error(24));
        assert!(matches!(
            exhausted,
            ProcessError::Exhausted { operation: "op" }
        ));
        let denied = io_error("op", &io::Error::from_raw_os_error(1));
        assert!(matches!(denied, ProcessError::PermissionDenied { .. }));
        let other = io_error("op", &io::Error::from_raw_os_error(9));
        assert!(matches!(other, ProcessError::Os { errno: 9, .. }));
        let synthetic = io_error("op", &io::Error::other("no errno"));
        assert!(matches!(synthetic, ProcessError::Os { errno: 5, .. }));
    }

    #[tokio::test]
    async fn test_wait_reports_exit_code() -> TestResult {
        let (mut process, _stdout) = spawn("exit 7")?;
        let outcome = process.wait().await.map_err(ctx("wait"))?;
        assert_eq!(outcome, ExitOutcome::Exited(7));
        // Idempotent after reaping.
        assert_eq!(
            process.wait().await.map_err(ctx("wait again"))?,
            ExitOutcome::Exited(7)
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_wait_timeout_then_signal() -> TestResult {
        let (mut process, _stdout) = spawn("exec sleep 30")?;
        let early = process
            .wait_timeout(Duration::from_millis(50))
            .await
            .map_err(ctx("wait_timeout"))?;
        assert!(early.is_none());
        process.signal(SignalKind::Term).map_err(ctx("signal"))?;
        let outcome = process
            .wait_timeout(Duration::from_secs(10))
            .await
            .map_err(ctx("wait_timeout after TERM"))?
            .ok_or(TestError::Missing("outcome after SIGTERM"))?;
        assert!(
            matches!(outcome, ExitOutcome::Signaled { signal: 15, .. }),
            "{outcome:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_terminate_escalates_when_term_is_ignored() -> TestResult {
        let (mut process, stdout) = spawn("trap '' TERM; echo ready; exec sleep 30")?;
        assert_eq!(first_line(stdout).await?, "ready");
        let report = process
            .terminate(&TerminationPolicy {
                graceful_signal: SignalKind::Term,
                grace_period: Duration::from_millis(200),
                hard_kill: true,
            })
            .await
            .map_err(ctx("terminate"))?;
        assert!(report.escalated);
        match report.outcome {
            Some(ExitOutcome::Signaled { signal: 9, .. }) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[tokio::test]
    async fn test_terminate_group_graceful() -> TestResult {
        let (group, _stdio) =
            LinuxJobGroup::spawn(Command::new("sleep").arg("30")).map_err(ctx("spawn group"))?;
        let mut process = AsyncLinuxProcess::from_group(group).map_err(ctx("adopt group"))?;
        assert!(process.is_group());
        let report = process
            .terminate(&TerminationPolicy {
                graceful_signal: SignalKind::Term,
                grace_period: Duration::from_secs(10),
                hard_kill: true,
            })
            .await
            .map_err(ctx("terminate"))?;
        assert!(!report.escalated);
        assert!(
            matches!(
                report.outcome,
                Some(ExitOutcome::Signaled { signal: 15, .. })
            ),
            "{report:?}"
        );
        // Terminating again is idempotent.
        let again = process
            .terminate(&TerminationPolicy::default())
            .await
            .map_err(ctx("terminate again"))?;
        assert_eq!(again.outcome, report.outcome);
        Ok(())
    }
}
