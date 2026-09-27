//! Group termination policy and its report (platform-neutral data; the
//! macOS implementation is `DarwinProcess::terminate_group`).

use std::time::Duration;

use harw_job_core::ExitOutcome;

use crate::signal::SignalKind;

/// How a job's process group is terminated: send `graceful_signal` to the
/// whole group, wait up to `grace_period` for the leader to exit, then (if
/// `hard_kill`) send `SIGKILL` to the group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminationPolicy {
    /// First signal sent to the process group.
    pub graceful_signal: SignalKind,
    /// How long the leader may take to exit after `graceful_signal`.
    pub grace_period: Duration,
    /// Whether to escalate to `SIGKILL` for the whole group.
    pub hard_kill: bool,
}

impl TerminationPolicy {
    /// Default grace period (10 s).
    pub const DEFAULT_GRACE: Duration = Duration::from_secs(10);
}

impl Default for TerminationPolicy {
    fn default() -> Self {
        Self {
            graceful_signal: SignalKind::Term,
            grace_period: Self::DEFAULT_GRACE,
            hard_kill: true,
        }
    }
}

/// What the final `SIGKILL` sweep over the process group achieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GroupSweep {
    /// No sweep was sent (`hard_kill == false`, or the process was already
    /// reaped before termination began).
    NotSent,
    /// `SIGKILL` was delivered to the group.
    Delivered,
    /// The group had no signallable members left (`ESRCH`).
    NothingLeft,
    /// The kernel refused (`EPERM`) after the leader had already exited.
    /// On Darwin `kill(2)` on a group that only holds zombies can report
    /// `EPERM`; a still-running member owned by another user would too. The
    /// two cannot be told apart without `sysctl`, so this is reported
    /// rather than hidden.
    DeniedAfterLeaderExit,
}

/// Result of a group termination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminationReport {
    /// Exit of the group leader; `None` only when `hard_kill == false` and
    /// the leader outlived the grace period.
    pub outcome: Option<ExitOutcome>,
    /// Whether the leader did not exit within the grace period (so the
    /// `SIGKILL` sweep, if any, was what ended it).
    pub escalated: bool,
    /// Result of the `SIGKILL` sweep over the group.
    pub sweep: GroupSweep,
}

#[cfg(test)]
mod tests {
    use super::{SignalKind, TerminationPolicy};
    use std::time::Duration;

    #[test]
    fn test_default_policy_is_term_then_kill_after_ten_seconds() {
        let policy = TerminationPolicy::default();
        assert_eq!(policy.graceful_signal, SignalKind::Term);
        assert_eq!(policy.grace_period, Duration::from_secs(10));
        assert!(policy.hard_kill);
    }
}
