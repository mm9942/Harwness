//! `/proc` observation through the `procfs` crate (Job-Runtime-Doc §3.3,
//! Phase 3).
//!
//! This module *observes*; it never controls. Live control goes through the
//! pidfd in [`crate::process::LinuxProcess`]. What remains project-specific
//! here is only the interpretation: which fields make up a recovery
//! identity ([`read_identity`]) and how `procfs` errors map to our typed
//! [`RecoveryError`]. No `procfs` type crosses the public API.

use harw_job_core::{AttemptId, RunnerId};
use procfs::ProcError;
use procfs::process::Process;

use crate::error::RecoveryError;
use crate::recovery::LinuxRecoveryIdentity;

/// Scheduler state of a process (`/proc/<pid>/stat`, field 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProcessState {
    /// `R`.
    Running,
    /// `S`.
    Sleeping,
    /// `D` (uninterruptible disk sleep).
    DiskSleep,
    /// `Z` — terminated, not yet reaped by its parent.
    Zombie,
    /// `T`.
    Stopped,
    /// `t`.
    TracingStop,
    /// `X`/`x`.
    Dead,
    /// `I`.
    Idle,
    /// Any other (historic) state letter.
    Other(char),
}

impl ProcessState {
    fn from_char(state: char) -> Self {
        match state {
            'R' => Self::Running,
            'S' => Self::Sleeping,
            'D' => Self::DiskSleep,
            'Z' => Self::Zombie,
            'T' => Self::Stopped,
            't' => Self::TracingStop,
            'X' | 'x' => Self::Dead,
            'I' => Self::Idle,
            other => Self::Other(other),
        }
    }

    /// Whether the process has terminated (zombie or dead).
    #[must_use]
    pub fn has_terminated(self) -> bool {
        matches!(self, Self::Zombie | Self::Dead)
    }
}

/// Observed metadata of one process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcSnapshot {
    /// PID the snapshot was taken for.
    pub pid: u32,
    /// Start time in clock ticks since boot (`stat` field 22).
    pub start_time_ticks: u64,
    /// Scheduler state.
    pub state: ProcessState,
    /// `/proc/<pid>/exe` target, if readable (often not for other users'
    /// processes, and not for zombies).
    pub executable: Option<String>,
    /// Path in the unified (v2) hierarchy from `/proc/<pid>/cgroup`
    /// (`0::<path>` line), if present.
    pub cgroup_v2_path: Option<String>,
}

/// Maps a `procfs` error on `pid` to our typed error.
pub(crate) fn map_proc_error(pid: u32, error: ProcError) -> RecoveryError {
    match error {
        ProcError::NotFound(_) => RecoveryError::AlreadyExited { pid },
        ProcError::PermissionDenied(_) => RecoveryError::PermissionDenied { pid },
        other => RecoveryError::Unreadable {
            pid,
            message: other.to_string(),
        },
    }
}

/// Reads a [`ProcSnapshot`] of `pid`.
///
/// Only `stat` is mandatory; `exe` and `cgroup` are best effort.
///
/// # Errors
/// [`RecoveryError::AlreadyExited`] if `/proc/<pid>` does not exist,
/// [`RecoveryError::PermissionDenied`], or [`RecoveryError::Unreadable`].
pub fn snapshot(pid: u32) -> Result<ProcSnapshot, RecoveryError> {
    let raw = i32::try_from(pid).map_err(|_| RecoveryError::Unreadable {
        pid,
        message: "pid exceeds i32::MAX".to_owned(),
    })?;
    if raw <= 0 {
        return Err(RecoveryError::Unreadable {
            pid,
            message: "pid must be positive".to_owned(),
        });
    }
    let process = Process::new(raw).map_err(|error| map_proc_error(pid, error))?;
    let stat = process.stat().map_err(|error| map_proc_error(pid, error))?;
    let executable = process
        .exe()
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    let cgroup_v2_path = process.cgroups().ok().and_then(|groups| {
        groups
            .into_iter()
            .find(|group| group.hierarchy == 0 && group.controllers.is_empty())
            .map(|group| group.pathname)
    });
    Ok(ProcSnapshot {
        pid,
        start_time_ticks: stat.starttime,
        state: ProcessState::from_char(stat.state),
        executable,
        cgroup_v2_path,
    })
}

/// Reads the recovery identity of a running process (Job-Runtime-Doc §15).
///
/// Captures PID, start time, executable and v2 cgroup path, tagged with the
/// owning runner and attempt, for persistence.
///
/// # Errors
/// As [`snapshot`].
pub fn read_identity(
    pid: u32,
    runner_id: RunnerId,
    attempt_id: AttemptId,
) -> Result<LinuxRecoveryIdentity, RecoveryError> {
    let snap = snapshot(pid)?;
    Ok(LinuxRecoveryIdentity {
        pid,
        process_start_time: Some(snap.start_time_ticks),
        cgroup_path: snap.cgroup_v2_path,
        executable: snap.executable,
        runner_id,
        attempt_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_snapshot_of_self() -> TestResult {
        let snap = snapshot(std::process::id()).map_err(ctx("snapshot self"))?;
        assert_eq!(snap.pid, std::process::id());
        assert!(snap.start_time_ticks > 0);
        assert!(!snap.state.has_terminated());
        let exe = snap.executable.ok_or(TestError::Missing("own exe"))?;
        assert!(!exe.is_empty());
        Ok(())
    }

    #[test]
    fn test_snapshot_of_pid_one_readable_or_typed_denial() -> TestResult {
        // In containers PID 1 may be hidden or unreadable; that must surface
        // as a typed error, never as a panic or a fake identity.
        match snapshot(1) {
            Ok(snap) => {
                assert_eq!(snap.pid, 1);
                Ok(())
            }
            Err(
                RecoveryError::PermissionDenied { pid: 1 }
                | RecoveryError::AlreadyExited { pid: 1 },
            ) => Ok(()),
            Err(other) => Err(TestError::Unexpected(other.to_string())),
        }
    }

    #[test]
    fn test_snapshot_rejects_zero_and_missing() {
        assert!(matches!(
            snapshot(0),
            Err(RecoveryError::Unreadable { pid: 0, .. })
        ));
        // PIDs above pid_max (at most 2^22) never exist.
        assert!(matches!(
            snapshot(0x7fff_ffff),
            Err(RecoveryError::AlreadyExited { .. })
        ));
    }

    #[test]
    fn test_state_letters() {
        assert_eq!(ProcessState::from_char('Z'), ProcessState::Zombie);
        assert!(ProcessState::from_char('Z').has_terminated());
        assert!(ProcessState::from_char('X').has_terminated());
        assert!(!ProcessState::from_char('S').has_terminated());
        assert_eq!(ProcessState::from_char('?'), ProcessState::Other('?'));
    }
}
