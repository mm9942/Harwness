//! Job-wide boundary and layered termination (Job-Runtime-Doc §2.2, §10).
//!
//! A [`LinuxJobGroup`] is the primary process (pidfd authority) plus its
//! own process group and, with feature `linux-cgroup-v2`, an optional job
//! cgroup. The process group is a convenience for graceful signals; it is
//! *not* the job boundary — a job can leave it with `setsid`/`setpgid`. The
//! cgroup is the boundary where available.
//!
//! # PID safety of process-group signals
//! Group signals use `kill(-pgid)`, i.e. a number. They are only sent while
//! the primary is **unreaped**: as long as the primary (the group leader) is
//! alive or a zombie, its PID — and therefore the group id — cannot be
//! allocated to anyone else. Once the primary has been reaped through the
//! pidfd, group signals are refused with [`ProcessError::AlreadyExited`].
//!
//! # Synchronous by design
//! Waiting uses `poll(2)` on the pidfd with a timeout. The async variant
//! (`AsyncFd<OwnedFd>`) belongs to the planned `harw-job-tokio` crate.

use std::os::unix::process::CommandExt as _;
use std::process::Command;
use std::time::Duration;

use harw_job_core::ExitOutcome;
use rustix::process::kill_process_group;

use crate::error::ProcessError;
use crate::process::{ChildStdio, LinuxProcess, SignalKind, to_pid};

/// How to terminate a job (Job-Runtime-Doc §10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminationPolicy {
    /// First signal, sent to the whole process group.
    pub graceful_signal: SignalKind,
    /// How long to wait for the primary to exit after the graceful signal.
    pub grace_period: Duration,
    /// Whether to SIGKILL the whole job (cgroup, else process group) after
    /// the grace period — and to clean up surviving descendants even when
    /// the primary exited gracefully.
    pub hard_kill: bool,
}

impl Default for TerminationPolicy {
    fn default() -> Self {
        Self {
            graceful_signal: SignalKind::Term,
            grace_period: Duration::from_secs(5),
            hard_kill: true,
        }
    }
}

/// What [`LinuxJobGroup::terminate`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminationReport {
    /// Final outcome of the primary; `None` only if `hard_kill` was off and
    /// the primary outlived the grace period (it is still running).
    pub outcome: Option<ExitOutcome>,
    /// Whether the primary ignored the graceful signal and was hard-killed.
    pub escalated: bool,
}

/// Job cgroup attached to a [`LinuxJobGroup`].
#[cfg(feature = "linux-cgroup-v2")]
pub struct JobCgroup {
    backend: std::sync::Arc<dyn crate::cgroup::CgroupBackend>,
    handle: crate::cgroup::CgroupHandle,
}

#[cfg(feature = "linux-cgroup-v2")]
impl std::fmt::Debug for JobCgroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobCgroup")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "linux-cgroup-v2")]
impl JobCgroup {
    /// Pairs a backend with one of its handles.
    #[must_use]
    pub fn new(
        backend: std::sync::Arc<dyn crate::cgroup::CgroupBackend>,
        handle: crate::cgroup::CgroupHandle,
    ) -> Self {
        Self { backend, handle }
    }

    /// The cgroup handle.
    #[must_use]
    pub fn handle(&self) -> &crate::cgroup::CgroupHandle {
        &self.handle
    }

    /// Splits into backend and handle (e.g. to `remove` after termination).
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        std::sync::Arc<dyn crate::cgroup::CgroupBackend>,
        crate::cgroup::CgroupHandle,
    ) {
        (self.backend, self.handle)
    }
}

/// Primary process + process group (+ optional job cgroup).
#[derive(Debug)]
pub struct LinuxJobGroup {
    primary: LinuxProcess,
    /// Process group id; equals the primary's PID (`process_group(0)`).
    pgid: u32,
    #[cfg(feature = "linux-cgroup-v2")]
    cgroup: Option<JobCgroup>,
}

impl LinuxJobGroup {
    /// Spawns `command` as leader of a new process group and takes pidfd
    /// authority over it.
    ///
    /// Overrides any `process_group` setting on `command`.
    ///
    /// # Errors
    /// As [`LinuxProcess::spawn`].
    pub fn spawn(command: &mut Command) -> Result<(Self, ChildStdio), ProcessError> {
        command.process_group(0);
        let (primary, stdio) = LinuxProcess::spawn(command)?;
        let pgid = primary.pid();
        Ok((
            Self {
                primary,
                pgid,
                #[cfg(feature = "linux-cgroup-v2")]
                cgroup: None,
            },
            stdio,
        ))
    }

    /// Spawns `command` and moves it into `cgroup` right after `pidfd_open`.
    ///
    /// Ordering (Job-Runtime-Doc §9, §24): the cgroup is created *before*
    /// spawn (by the caller), the child is attached immediately after the
    /// pidfd exists. The child may run for a few microseconds outside the
    /// cgroup; closing that window needs the child to attach itself before
    /// `exec` (`CgroupBackend::attach_self` in the planned trampoline).
    ///
    /// # Errors
    /// [`crate::error::LaunchError::Process`] for spawn/pidfd failures;
    /// [`crate::error::LaunchError::Cgroup`] if attaching fails — the child
    /// is then SIGKILLed through its pidfd and reaped before returning.
    #[cfg(feature = "linux-cgroup-v2")]
    pub fn spawn_in_cgroup(
        command: &mut Command,
        cgroup: JobCgroup,
    ) -> Result<(Self, ChildStdio), crate::error::LaunchError> {
        let (mut group, stdio) = Self::spawn(command)?;
        if let Err(error) = cgroup.backend.attach(&cgroup.handle, &group.primary) {
            let _ = group.primary.signal(SignalKind::Kill);
            let _ = group.primary.wait();
            return Err(error.into());
        }
        tracing::debug!(
            pid = group.primary.pid(),
            cgroup = cgroup.handle.proc_path(),
            "job attached to cgroup"
        );
        group.cgroup = Some(cgroup);
        Ok((group, stdio))
    }

    /// The primary process.
    #[must_use]
    pub fn primary(&self) -> &LinuxProcess {
        &self.primary
    }

    /// The primary process, mutably (for `wait`/`try_status`).
    pub fn primary_mut(&mut self) -> &mut LinuxProcess {
        &mut self.primary
    }

    /// Process group id (diagnostics).
    #[must_use]
    pub fn pgid(&self) -> u32 {
        self.pgid
    }

    /// The attached job cgroup, if any.
    #[cfg(feature = "linux-cgroup-v2")]
    #[must_use]
    pub fn cgroup(&self) -> Option<&JobCgroup> {
        self.cgroup.as_ref()
    }

    /// Detaches the job cgroup from this group (e.g. to remove it after
    /// termination).
    #[cfg(feature = "linux-cgroup-v2")]
    pub fn take_cgroup(&mut self) -> Option<JobCgroup> {
        self.cgroup.take()
    }

    /// Sends `signal` to the whole process group.
    ///
    /// # Errors
    /// [`ProcessError::AlreadyExited`] once the primary has been reaped
    /// (the group id is no longer ours) or no member is left.
    pub fn signal_group(&self, signal: SignalKind) -> Result<(), ProcessError> {
        if self.primary.is_reaped() {
            return Err(ProcessError::AlreadyExited { pid: self.pgid });
        }
        let pgid = to_pid(self.pgid)?;
        kill_process_group(pgid, signal.to_rustix()).map_err(|errno| {
            ProcessError::from_errno("kill(-pgid)", self.pgid, errno.raw_os_error())
        })
    }

    /// Waits for the primary and returns its outcome (see
    /// [`LinuxProcess::wait`]). Descendants are not waited for.
    ///
    /// # Errors
    /// As [`LinuxProcess::wait`].
    pub fn wait(&mut self) -> Result<ExitOutcome, ProcessError> {
        self.primary.wait()
    }

    /// Kills the attached job cgroup, if any; failures are logged and the
    /// caller falls back to the process group.
    #[cfg(feature = "linux-cgroup-v2")]
    fn kill_cgroup_best_effort(&self) {
        if let Some(cgroup) = &self.cgroup {
            if let Err(error) = cgroup.backend.kill(&cgroup.handle) {
                tracing::warn!(
                    pid = self.pgid,
                    cgroup = cgroup.handle.proc_path(),
                    %error,
                    "cgroup kill failed; falling back to process group"
                );
            }
        }
    }

    /// Without cgroup support there is no cgroup to kill.
    #[cfg(not(feature = "linux-cgroup-v2"))]
    fn kill_cgroup_best_effort(&self) {}

    /// SIGKILLs the whole job: the cgroup if attached (falling back to the
    /// process group on cgroup failure), the process group, and the
    /// primary through its pidfd (in case it left its group).
    fn hard_kill_all(&self) -> Result<(), ProcessError> {
        self.kill_cgroup_best_effort();
        match self.signal_group(SignalKind::Kill) {
            Ok(()) | Err(ProcessError::AlreadyExited { .. }) => {}
            Err(other) => return Err(other),
        }
        match self.primary.signal(SignalKind::Kill) {
            Ok(()) | Err(ProcessError::AlreadyExited { .. }) => Ok(()),
            Err(other) => Err(other),
        }
    }

    /// Layered termination (Job-Runtime-Doc §10): graceful signal to the
    /// process group → wait up to the grace period → hard-kill the whole job
    /// (cgroup, else process group) → reap the primary.
    ///
    /// With `hard_kill`, surviving descendants are killed even when the
    /// primary exited within the grace period. If the primary was already
    /// reaped, only the cgroup (if any) is killed — the process group id is
    /// no longer ours.
    ///
    /// # Errors
    /// Signal, poll or wait errors of the primary.
    pub fn terminate(
        &mut self,
        policy: &TerminationPolicy,
    ) -> Result<TerminationReport, ProcessError> {
        if self.primary.is_reaped() {
            if policy.hard_kill {
                self.kill_cgroup_best_effort();
            }
            return Ok(TerminationReport {
                outcome: Some(self.primary.wait()?),
                escalated: false,
            });
        }
        let mut escalated = false;
        if !self.primary.has_exited()? {
            tracing::debug!(
                pid = self.pgid,
                signal = policy.graceful_signal.name(),
                "graceful job termination"
            );
            match self.signal_group(policy.graceful_signal) {
                Ok(()) | Err(ProcessError::AlreadyExited { .. }) => {}
                Err(other) => return Err(other),
            }
            // Also through the pidfd, in case the primary left its group.
            match self.primary.signal(policy.graceful_signal) {
                Ok(()) | Err(ProcessError::AlreadyExited { .. }) => {}
                Err(other) => return Err(other),
            }
            let exited = self.primary.wait_exit_ready(Some(policy.grace_period))?;
            if !exited {
                if !policy.hard_kill {
                    return Ok(TerminationReport {
                        outcome: None,
                        escalated: false,
                    });
                }
                escalated = true;
                tracing::info!(
                    pid = self.pgid,
                    grace_ms = u64::try_from(policy.grace_period.as_millis()).unwrap_or(u64::MAX),
                    "grace period elapsed; hard-killing job"
                );
            }
        }
        if policy.hard_kill {
            // Primary still unreaped here: group id is still ours.
            self.hard_kill_all()?;
        }
        let outcome = self.primary.wait()?;
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
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    fn sh_piped(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script).stdout(Stdio::piped());
        command
    }

    fn read_line(stdio: ChildStdio) -> TestResult<String> {
        let stdout = stdio.stdout.ok_or(TestError::Missing("stdout pipe"))?;
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .map_err(ctx("read line"))?;
        Ok(line.trim().to_owned())
    }

    #[test]
    fn test_default_policy() {
        let policy = TerminationPolicy::default();
        assert_eq!(policy.graceful_signal, SignalKind::Term);
        assert!(policy.hard_kill);
    }

    #[test]
    fn test_spawn_creates_own_process_group() -> TestResult {
        let (mut group, _stdio) =
            LinuxJobGroup::spawn(Command::new("sleep").arg("30")).map_err(ctx("spawn"))?;
        assert_eq!(group.pgid(), group.primary().pid());
        group
            .signal_group(SignalKind::Kill)
            .map_err(ctx("signal group"))?;
        let outcome = group.wait().map_err(ctx("wait"))?;
        assert!(matches!(outcome, ExitOutcome::Signaled { signal: 9, .. }));
        // Reaped: the group id is no longer ours.
        assert!(matches!(
            group.signal_group(SignalKind::Kill),
            Err(ProcessError::AlreadyExited { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_terminate_graceful_without_escalation() -> TestResult {
        let (mut group, _stdio) =
            LinuxJobGroup::spawn(Command::new("sleep").arg("30")).map_err(ctx("spawn"))?;
        let report = group
            .terminate(&TerminationPolicy {
                graceful_signal: SignalKind::Term,
                grace_period: Duration::from_secs(10),
                hard_kill: true,
            })
            .map_err(ctx("terminate"))?;
        assert!(!report.escalated);
        assert!(matches!(
            report.outcome,
            Some(ExitOutcome::Signaled { signal: 15, .. })
        ));
        Ok(())
    }

    #[test]
    fn test_terminate_escalates_when_term_is_ignored() -> TestResult {
        // `trap '' TERM` sets SIG_IGN, which survives `exec`: sleep ignores
        // TERM too. "ready" is printed only after the trap is installed, so
        // the TERM cannot race the trap.
        let (mut group, stdio) =
            LinuxJobGroup::spawn(&mut sh_piped("trap '' TERM; echo ready; exec sleep 30"))
                .map_err(ctx("spawn"))?;
        assert_eq!(read_line(stdio)?, "ready");
        let report = group
            .terminate(&TerminationPolicy {
                graceful_signal: SignalKind::Term,
                grace_period: Duration::from_millis(300),
                hard_kill: true,
            })
            .map_err(ctx("terminate"))?;
        assert!(report.escalated);
        assert!(
            matches!(
                report.outcome,
                Some(ExitOutcome::Signaled { signal: 9, .. })
            ),
            "{report:?}"
        );
        Ok(())
    }

    #[test]
    fn test_terminate_without_hard_kill_leaves_process_running() -> TestResult {
        let (mut group, stdio) =
            LinuxJobGroup::spawn(&mut sh_piped("trap '' TERM; echo ready; exec sleep 30"))
                .map_err(ctx("spawn"))?;
        assert_eq!(read_line(stdio)?, "ready");
        let report = group
            .terminate(&TerminationPolicy {
                graceful_signal: SignalKind::Term,
                grace_period: Duration::from_millis(100),
                hard_kill: false,
            })
            .map_err(ctx("terminate"))?;
        assert_eq!(report.outcome, None);
        assert!(!group.primary().has_exited().map_err(ctx("has_exited"))?);
        group
            .primary()
            .signal(SignalKind::Kill)
            .map_err(ctx("kill"))?;
        group.wait().map_err(ctx("wait"))?;
        Ok(())
    }

    #[test]
    fn test_terminate_reaches_grandchild_in_group() -> TestResult {
        // The shell prints the PID of its background child; we pin that
        // grandchild with a pidfd before terminating.
        let (mut group, stdio) = LinuxJobGroup::spawn(&mut sh_piped("sleep 30 & echo $!; wait"))
            .map_err(ctx("spawn"))?;
        let grandchild_pid: u32 = read_line(stdio)?
            .parse()
            .map_err(ctx("parse grandchild pid"))?;
        let grandchild = LinuxProcess::open(grandchild_pid).map_err(ctx("pin grandchild"))?;
        let report = group
            .terminate(&TerminationPolicy {
                graceful_signal: SignalKind::Term,
                grace_period: Duration::from_secs(5),
                hard_kill: true,
            })
            .map_err(ctx("terminate"))?;
        assert!(report.outcome.is_some());
        let gone = grandchild
            .wait_exit_ready(Some(Duration::from_secs(5)))
            .map_err(ctx("grandchild exit"))?;
        assert!(gone, "grandchild survived job termination");
        Ok(())
    }

    #[test]
    fn test_terminate_after_exit_reports_outcome() -> TestResult {
        let (mut group, _stdio) = LinuxJobGroup::spawn(Command::new("sh").arg("-c").arg("exit 3"))
            .map_err(ctx("spawn"))?;
        group
            .primary()
            .wait_exit_ready(Some(Duration::from_secs(10)))
            .map_err(ctx("exit ready"))?;
        let report = group
            .terminate(&TerminationPolicy::default())
            .map_err(ctx("terminate"))?;
        assert!(!report.escalated);
        assert_eq!(report.outcome, Some(ExitOutcome::Exited(3)));
        // Idempotent after reaping.
        let again = group
            .terminate(&TerminationPolicy::default())
            .map_err(ctx("terminate again"))?;
        assert_eq!(again.outcome, Some(ExitOutcome::Exited(3)));
        Ok(())
    }
}
