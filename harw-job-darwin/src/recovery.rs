//! Recovery identity of a process after a runner restart (Job-Runtime-Doc
//! §15), Darwin edition.
//!
//! # Why recovered processes are never signalled here
//! Linux ties a PID to a process through `/proc/<pid>/stat` start time.
//! Darwin has no `/proc`; the equivalent is `sysctl(CTL_KERN, KERN_PROC,
//! KERN_PROC_PID, pid)` → `kinfo_proc.kp_proc.p_starttime`. rustix 1.1 does
//! not expose that sysctl, and calling it through `libc` would need `unsafe`
//! (forbidden workspace-wide). So this crate cannot **verify** that a
//! recovered PID is still the persisted process.
//!
//! Consequence, consistent with "a PID is never sole authority":
//! - [`DarwinRecoveryIdentity::start_time`] is `None` for identities this
//!   crate records (it is kept as a field so a later, verifiable lookup can
//!   fill it without a format change).
//! - The macOS-only `check` returns [`IdentityCheck::Gone`] (no such PID) or
//!   [`IdentityCheck::Unverifiable`] — never "verified".
//! - The macOS-only `signal` refuses every unverifiable identity with
//!   [`crate::ProcessError::IdentityUnverifiable`]. Recovered Darwin jobs can
//!   be observed (alive/gone) but must be cleaned up by other means (e.g. the
//!   operator, or a future launchd-based supervisor).

use std::fmt;

use serde::{Deserialize, Serialize};

/// Persisted identity of a job process on Darwin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DarwinRecoveryIdentity {
    /// Diagnostic PID (never authority on its own).
    pub pid: u32,
    /// Process start time (`p_starttime`, microseconds since the epoch) if it
    /// could be observed. Always `None` in this version (see module docs).
    #[serde(default)]
    pub start_time: Option<u64>,
}

impl DarwinRecoveryIdentity {
    /// An identity with only a PID (start time unknown).
    #[must_use]
    pub const fn pid_only(pid: u32) -> Self {
        Self {
            pid,
            start_time: None,
        }
    }

    /// Why this identity cannot be verified on this build.
    #[must_use]
    pub const fn unverifiable_reason(&self) -> UnverifiableReason {
        match self.start_time {
            None => UnverifiableReason::StartTimeUnknown,
            Some(_) => UnverifiableReason::StartTimeLookupUnavailable,
        }
    }
}

/// Why a recovered identity cannot be tied to a live process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum UnverifiableReason {
    /// The identity carries no start time; a PID alone is no proof.
    StartTimeUnknown,
    /// A start time is persisted, but this build cannot read the live
    /// process' start time (`sysctl KERN_PROC` is not available without
    /// `unsafe`).
    StartTimeLookupUnavailable,
}

impl fmt::Display for UnverifiableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::StartTimeUnknown => "no persisted start time (pid alone is no proof)",
            Self::StartTimeLookupUnavailable => {
                "live start time not observable (sysctl KERN_PROC unavailable)"
            }
        })
    }
}

/// Result of checking a recovered identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IdentityCheck {
    /// No process with this PID exists any more.
    Gone,
    /// Some process with this PID exists, but it cannot be shown to be the
    /// persisted one. It must not be signalled.
    Unverifiable {
        /// Why.
        reason: UnverifiableReason,
    },
}

#[cfg(test)]
mod tests {
    use super::{DarwinRecoveryIdentity, UnverifiableReason};
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_identity_round_trips_and_reports_unverifiable_reason() -> TestResult {
        let identity = DarwinRecoveryIdentity::pid_only(4242);
        assert_eq!(
            identity.unverifiable_reason(),
            UnverifiableReason::StartTimeUnknown
        );
        let json = serde_json::to_string(&identity).map_err(ctx("serialize"))?;
        assert_eq!(json, r#"{"pid":4242,"start_time":null}"#);
        let back: DarwinRecoveryIdentity =
            serde_json::from_str(r#"{"pid":4242}"#).map_err(ctx("deserialize"))?;
        assert_eq!(back, identity);

        let with_time = DarwinRecoveryIdentity {
            pid: 1,
            start_time: Some(99),
        };
        assert_eq!(
            with_time.unverifiable_reason(),
            UnverifiableReason::StartTimeLookupUnavailable
        );
        assert!(
            serde_json::from_str::<DarwinRecoveryIdentity>(r#"{"pid":1,"exe":"x"}"#).is_err(),
            "unknown fields are rejected"
        );
        Ok(())
    }
}
