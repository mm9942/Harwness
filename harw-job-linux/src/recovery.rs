//! Recovery identity after a runtime restart (Job-Runtime-Doc §15, §21.4).
//!
//! pidfds do not survive a restart of the supervising process, so a
//! previously started job is described by persisted metadata
//! ([`LinuxRecoveryIdentity`]). A PID alone is never proof: an identity
//! without a start time never verifies, and every signal to a recovered
//! process goes through [`LinuxRecoveryIdentity::signal_verified`], which
//!
//! 1. opens a pidfd for the PID,
//! 2. re-reads `/proc/<pid>` *after* the pidfd exists and compares the
//!    start time (and executable / cgroup where both sides are known),
//! 3. signals through that pidfd only on a match.
//!
//! Because the pidfd pins the process that owned the PID at step 1, a
//! match in step 2 proves the pidfd refers to the persisted process; a PID
//! recycled between steps can only produce a mismatch, never a wrong kill.
//!
//! With a job cgroup, prefer killing the whole cgroup
//! (`kill_job_verified`, feature `linux-cgroup-v2`): the cgroup is our own
//! directory and contains the job's descendants as well.
//!
//! Capture the identity *after* cgroup attachment (Job-Runtime-Doc §9
//! order: spawn → pidfd → attach → persist), otherwise the persisted cgroup
//! path is the parent's and verification reports a cgroup mismatch.

use harw_job_core::{AttemptId, RunnerId};
use serde::{Deserialize, Serialize};

use crate::error::{MismatchReason, RecoveryError};
use crate::proc::{ProcSnapshot, read_identity, snapshot};
use crate::process::{LinuxProcess, SignalKind};

/// Persistable identity of a started job process (Job-Runtime-Doc §15).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxRecoveryIdentity {
    /// PID at start (metadata only).
    pub pid: u32,
    /// Start time in clock ticks since boot; `None` never verifies.
    pub process_start_time: Option<u64>,
    /// Unified-hierarchy cgroup path as seen in `/proc/<pid>/cgroup`
    /// (persisted metadata, not an authority).
    pub cgroup_path: Option<String>,
    /// `/proc/<pid>/exe` at capture time.
    pub executable: Option<String>,
    /// Runner that started the process.
    pub runner_id: RunnerId,
    /// Attempt the process belongs to.
    pub attempt_id: AttemptId,
}

/// Result of comparing a persisted identity with the live system.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdentityCheck {
    /// The persisted process is still running.
    Alive,
    /// The persisted process has terminated (gone, or a zombie with a
    /// matching identity).
    Exited,
    /// The PID now belongs to something else, or identity cannot be proven.
    Mismatch(MismatchReason),
}

/// Strips the kernel's ` (deleted)` marker so a replaced binary still
/// compares equal to its persisted path.
fn normalize_exe(path: &str) -> &str {
    path.strip_suffix(" (deleted)").unwrap_or(path)
}

impl LinuxRecoveryIdentity {
    /// Captures the identity of a live process we control.
    ///
    /// # Errors
    /// As [`read_identity`].
    pub fn capture(
        process: &LinuxProcess,
        runner_id: RunnerId,
        attempt_id: AttemptId,
    ) -> Result<Self, RecoveryError> {
        read_identity(process.pid(), runner_id, attempt_id)
    }

    /// Pure comparison against an observed snapshot. `None` means the
    /// snapshot is the persisted process.
    #[must_use]
    pub fn compare(&self, observed: &ProcSnapshot) -> Option<MismatchReason> {
        if let Some(reason) = self.compare_start_time(observed) {
            return Some(reason);
        }
        if let (Some(expected), Some(actual)) = (&self.executable, &observed.executable) {
            if normalize_exe(expected) != normalize_exe(actual) {
                return Some(MismatchReason::ExecutableDiffers {
                    expected: expected.clone(),
                    actual: actual.clone(),
                });
            }
        }
        if let (Some(expected), Some(actual)) = (&self.cgroup_path, &observed.cgroup_v2_path) {
            if expected != actual {
                return Some(MismatchReason::CgroupDiffers {
                    expected: expected.clone(),
                    actual: actual.clone(),
                });
            }
        }
        None
    }

    fn compare_start_time(&self, observed: &ProcSnapshot) -> Option<MismatchReason> {
        let Some(expected) = self.process_start_time else {
            return Some(MismatchReason::StartTimeUnknown);
        };
        (expected != observed.start_time_ticks).then_some(MismatchReason::StartTimeDiffers {
            expected,
            actual: observed.start_time_ticks,
        })
    }

    /// Checks whether the persisted process still exists.
    ///
    /// # Errors
    /// [`RecoveryError::PermissionDenied`] / [`RecoveryError::Unreadable`]
    /// when `/proc/<pid>` cannot be inspected — identity is then not
    /// established and the attempt must be treated as unrecoverable, never
    /// signalled.
    pub fn verify(&self) -> Result<IdentityCheck, RecoveryError> {
        if self.process_start_time.is_none() {
            return Ok(IdentityCheck::Mismatch(MismatchReason::StartTimeUnknown));
        }
        let observed = match snapshot(self.pid) {
            Ok(observed) => observed,
            Err(RecoveryError::AlreadyExited { .. }) => return Ok(IdentityCheck::Exited),
            Err(other) => return Err(other),
        };
        if observed.state.has_terminated() {
            // Zombies expose no `exe` and may report a released cgroup:
            // only the start time is meaningful for them.
            return Ok(match self.compare_start_time(&observed) {
                Some(reason) => IdentityCheck::Mismatch(reason),
                None => IdentityCheck::Exited,
            });
        }
        Ok(match self.compare(&observed) {
            Some(reason) => IdentityCheck::Mismatch(reason),
            None => IdentityCheck::Alive,
        })
    }

    /// Opens a pidfd for the persisted process and proves, *after* opening,
    /// that it is the persisted process.
    ///
    /// The returned handle is a non-child handle: it can signal and observe
    /// exit, but not read an exit status.
    ///
    /// # Errors
    /// [`RecoveryError::IdentityMismatch`] (nothing opened is used),
    /// [`RecoveryError::AlreadyExited`], or inspection/pidfd errors.
    pub fn open_verified(&self) -> Result<LinuxProcess, RecoveryError> {
        if self.process_start_time.is_none() {
            return Err(RecoveryError::IdentityMismatch {
                pid: self.pid,
                reason: MismatchReason::StartTimeUnknown,
            });
        }
        let process = match LinuxProcess::open(self.pid) {
            Ok(process) => process,
            Err(crate::error::ProcessError::AlreadyExited { pid }) => {
                return Err(RecoveryError::AlreadyExited { pid });
            }
            Err(other) => return Err(RecoveryError::Process(other)),
        };
        // Re-verify with the pidfd held: the pidfd pins the process that
        // owned the PID when it was opened.
        match self.verify()? {
            IdentityCheck::Alive => Ok(process),
            IdentityCheck::Exited => Err(RecoveryError::AlreadyExited { pid: self.pid }),
            IdentityCheck::Mismatch(reason) => {
                tracing::warn!(
                    pid = self.pid,
                    attempt_id = self.attempt_id.as_str(),
                    runner_id = self.runner_id.as_str(),
                    recovery_result = "identity_mismatch",
                    %reason,
                    "refusing to control recovered process"
                );
                Err(RecoveryError::IdentityMismatch {
                    pid: self.pid,
                    reason,
                })
            }
        }
    }

    /// Signals the persisted process only after identity verification.
    ///
    /// # Errors
    /// As [`LinuxRecoveryIdentity::open_verified`], plus the signal error.
    pub fn signal_verified(&self, signal: SignalKind) -> Result<(), RecoveryError> {
        let process = self.open_verified()?;
        tracing::info!(
            pid = self.pid,
            attempt_id = self.attempt_id.as_str(),
            signal = signal.name(),
            recovery_result = "signalled",
            "signalling verified recovered process"
        );
        process.signal(signal).map_err(|error| match error {
            crate::error::ProcessError::AlreadyExited { pid } => {
                RecoveryError::AlreadyExited { pid }
            }
            other => RecoveryError::Process(other),
        })
    }

    /// Kills the whole job cgroup of a recovered attempt.
    ///
    /// The cgroup is safe to kill regardless of what the primary PID is now:
    /// `handle` must be the job's own cgroup, proven by `handle`'s path
    /// matching the persisted [`LinuxRecoveryIdentity::cgroup_path`]. An
    /// unrelated process that reused the primary PID is not a member of
    /// that cgroup, so it is never hit. Returns the primary's identity check
    /// for the caller's transition decision (e.g. "cgroup exists but
    /// primary exited", Job-Runtime-Doc §21.4).
    ///
    /// # Errors
    /// [`RecoveryError::IdentityMismatch`] with
    /// [`MismatchReason::CgroupDiffers`] if `handle` is not the persisted
    /// cgroup (nothing is killed); cgroup errors from the backend.
    #[cfg(feature = "linux-cgroup-v2")]
    pub fn kill_job_verified(
        &self,
        backend: &dyn crate::cgroup::CgroupBackend,
        handle: &crate::cgroup::CgroupHandle,
    ) -> Result<IdentityCheck, RecoveryError> {
        let persisted = self.cgroup_path.as_deref().unwrap_or_default();
        if persisted != handle.proc_path() {
            return Err(RecoveryError::IdentityMismatch {
                pid: self.pid,
                reason: MismatchReason::CgroupDiffers {
                    expected: persisted.to_owned(),
                    actual: handle.proc_path().to_owned(),
                },
            });
        }
        let primary = self.verify()?;
        backend.kill(handle)?;
        tracing::info!(
            pid = self.pid,
            attempt_id = self.attempt_id.as_str(),
            cgroup = handle.proc_path(),
            recovery_result = "cgroup_killed",
            "killed recovered job cgroup"
        );
        Ok(primary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::process::Command;

    fn ids() -> TestResult<(RunnerId, AttemptId)> {
        Ok((
            RunnerId::new("runner-test").map_err(ctx("runner id"))?,
            AttemptId::new("attempt-test").map_err(ctx("attempt id"))?,
        ))
    }

    fn self_identity() -> TestResult<LinuxRecoveryIdentity> {
        let (runner, attempt) = ids()?;
        read_identity(std::process::id(), runner, attempt).map_err(ctx("read identity of self"))
    }

    #[test]
    fn test_identity_of_self_verifies_alive() -> TestResult {
        let identity = self_identity()?;
        assert!(identity.process_start_time.is_some());
        assert_eq!(
            identity.verify().map_err(ctx("verify"))?,
            IdentityCheck::Alive
        );
        Ok(())
    }

    #[test]
    fn test_wrong_start_time_is_mismatch() -> TestResult {
        let mut identity = self_identity()?;
        let real = identity
            .process_start_time
            .ok_or(TestError::Missing("start time"))?;
        identity.process_start_time = Some(real + 1);
        match identity.verify().map_err(ctx("verify"))? {
            IdentityCheck::Mismatch(MismatchReason::StartTimeDiffers { expected, actual }) => {
                assert_eq!(expected, real + 1);
                assert_eq!(actual, real);
                Ok(())
            }
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn test_missing_start_time_never_verifies() -> TestResult {
        let mut identity = self_identity()?;
        identity.process_start_time = None;
        assert_eq!(
            identity.verify().map_err(ctx("verify"))?,
            IdentityCheck::Mismatch(MismatchReason::StartTimeUnknown)
        );
        Ok(())
    }

    #[test]
    fn test_pid_reuse_is_never_signalled() -> TestResult {
        // Simulates PID reuse: the PID exists (it is this test process), but
        // the persisted start time belongs to an earlier process. The
        // signal must be refused before anything is sent — if it were sent,
        // SIGKILL would end this test run.
        let mut identity = self_identity()?;
        let real = identity
            .process_start_time
            .ok_or(TestError::Missing("start time"))?;
        identity.process_start_time = Some(real.saturating_sub(1));
        let result = identity.signal_verified(SignalKind::Kill);
        assert!(
            matches!(result, Err(RecoveryError::IdentityMismatch { .. })),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn test_exited_process_verifies_exited() -> TestResult {
        let (runner, attempt) = ids()?;
        let (mut process, _stdio) =
            LinuxProcess::spawn(Command::new("sleep").arg("30")).map_err(ctx("spawn sleep"))?;
        let identity =
            LinuxRecoveryIdentity::capture(&process, runner, attempt).map_err(ctx("capture"))?;
        assert_eq!(
            identity.verify().map_err(ctx("verify live"))?,
            IdentityCheck::Alive
        );
        process.signal(SignalKind::Kill).map_err(ctx("kill"))?;
        // Before reaping, the child is a zombie with a matching identity.
        process
            .wait_exit_ready(None)
            .map_err(ctx("wait exit ready"))?;
        assert_eq!(
            identity.verify().map_err(ctx("verify zombie"))?,
            IdentityCheck::Exited
        );
        process.wait().map_err(ctx("reap"))?;
        // After reaping the PID is free. It could in theory be reused by an
        // unrelated process within this window; the safety property is that
        // it never verifies as Alive.
        match identity.verify().map_err(ctx("verify reaped"))? {
            IdentityCheck::Exited | IdentityCheck::Mismatch(_) => {}
            IdentityCheck::Alive => {
                return Err(TestError::Unexpected(
                    "reaped process verified alive".into(),
                ));
            }
        }
        assert!(matches!(
            identity.signal_verified(SignalKind::Kill),
            Err(RecoveryError::AlreadyExited { .. } | RecoveryError::IdentityMismatch { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_signal_verified_hits_matching_child() -> TestResult {
        let (runner, attempt) = ids()?;
        let (mut process, _stdio) =
            LinuxProcess::spawn(Command::new("sleep").arg("30")).map_err(ctx("spawn sleep"))?;
        let identity =
            LinuxRecoveryIdentity::capture(&process, runner, attempt).map_err(ctx("capture"))?;
        identity
            .signal_verified(SignalKind::Term)
            .map_err(ctx("signal verified"))?;
        let outcome = process.wait().map_err(ctx("wait"))?;
        assert!(matches!(
            outcome,
            harw_job_core::ExitOutcome::Signaled { signal: 15, .. }
        ));
        Ok(())
    }

    #[test]
    fn test_compare_executable_and_cgroup() -> TestResult {
        let identity = self_identity()?;
        let base = ProcSnapshot {
            pid: identity.pid,
            start_time_ticks: identity
                .process_start_time
                .ok_or(TestError::Missing("start time"))?,
            state: crate::proc::ProcessState::Running,
            executable: Some("/usr/bin/other".to_owned()),
            cgroup_v2_path: identity.cgroup_path.clone(),
        };
        let with_exe = LinuxRecoveryIdentity {
            executable: Some("/usr/bin/job".to_owned()),
            ..identity.clone()
        };
        assert!(matches!(
            with_exe.compare(&base),
            Some(MismatchReason::ExecutableDiffers { .. })
        ));
        let deleted = ProcSnapshot {
            executable: Some("/usr/bin/job (deleted)".to_owned()),
            ..base.clone()
        };
        assert_eq!(with_exe.compare(&deleted), None);
        let other_cgroup = ProcSnapshot {
            executable: None,
            cgroup_v2_path: Some("/elsewhere".to_owned()),
            ..base
        };
        let with_cgroup = LinuxRecoveryIdentity {
            cgroup_path: Some("/harw/job-1".to_owned()),
            ..identity
        };
        assert!(matches!(
            with_cgroup.compare(&other_cgroup),
            Some(MismatchReason::CgroupDiffers { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_identity_serde_roundtrip() -> TestResult {
        let identity = self_identity()?;
        let json = serde_json::to_string(&identity).map_err(ctx("serialize"))?;
        assert!(json.contains("process_start_time"));
        let back: LinuxRecoveryIdentity =
            serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, identity);
        Ok(())
    }
}
