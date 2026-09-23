//! Process metadata and serializable outcomes; no I/O, threads or locks.
//! Kernel handles remain separate from JSON snapshots; reporting allocates owned data.
//! Errors are represented as report data; serialization is handled by callers.
//! Binary-crate examples are illustrative and are not run as Cargo doctests.
//! # Examples
//! ```
//! let report = serde_json::json!({"outcome":"killed"});
//! assert_eq!(report["outcome"], "killed");
//! ```
use crate::pidfd::PidFd;
use serde::{Deserialize, Serialize};

/// Metadata observed during selection; not a substitute for a pidfd.
// Expands to Serde object serialization and ordinary Debug formatting.
#[derive(Debug, Serialize)]
pub(crate) struct Process {
    /// Positive process identifier at selection time.
    pub(crate) pid: u32,
    /// Parent process identifier in the current namespace.
    pub(crate) ppid: u32,
    /// Effective user identifier at selection time.
    pub(crate) uid: u32,
    /// Kernel start clock ticks since boot.
    pub(crate) start_ticks: u64,
    /// Executable basename, falling back to the truncated kernel comm name.
    pub(crate) name: String,
    /// Executable path when procfs permits reading it.
    pub(crate) executable: Option<String>,
    /// Lossy UTF-8 rendering of argv with spaces between arguments.
    pub(crate) command: String,
}
/// Metadata plus the non-cloneable kernel identity selected for termination.
pub(crate) struct Target {
    /// Preview metadata owned by this selection.
    pub(crate) process: Process,
    /// Identity handle retained through confirmation and helper completion.
    pub(crate) fd: PidFd,
}
/// Exhaustive externally visible completion states; no stringly typed state machine.
// Expands to Serde snake_case enum serialization/deserialization and value traits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Completion {
    /// Exit observed after the second KILL attempt.
    KilledAfterRetry,
    /// Exit observed after KILL was sent.
    Killed,
    /// Exit was already observed, or signalling reported ESRCH.
    AlreadyExited,
    /// Exit was not observed within the requested wait bounds.
    Survived,
    /// An operation failed for this target.
    Error,
}
/// Final result for one target; error details are rendered at the report boundary.
// Expands to Serde object serialization/deserialization and Debug formatting.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Outcome {
    /// PID from the original selection, used for reporting only.
    pub(crate) pid: u32,
    /// Typed completion classification.
    pub(crate) outcome: Completion,
    /// Optional human-readable diagnostic.
    pub(crate) detail: Option<String>,
}
/// Classifies reports without discarding failure states.
impl Outcome {
    /// Render a borrowed structured error into an owned final report for `pid`.
    /// Copies the reporting PID and formats the borrowed `error`; the original
    /// error is retained by its caller. Returns an `Error` completion with owned
    /// diagnostic text. Infallible; no I/O, locks or threads.
    /// # Examples
    /// ```no_run
    /// let error = crate::error::Error::Permission { reason: "denied" };
    /// let outcome = crate::types::Outcome::error(42, &error);
    /// assert!(!outcome.success());
    /// ```
    pub(crate) fn error(pid: u32, error: &crate::error::Error) -> Self {
        Self {
            pid,
            outcome: Completion::Error,
            detail: Some(error.to_string()),
        }
    }
    /// True only when process exit was observed or the kernel reported exit.
    /// Borrows `self`; returns false for `Survived` and `Error`. Infallible,
    /// allocation-free and safe to call concurrently on shared report references.
    /// # Examples
    /// ```no_run
    /// let report = crate::types::Outcome {
    ///     pid: 42, outcome: crate::types::Completion::AlreadyExited, detail: None,
    /// };
    /// assert!(report.success());
    /// ```
    pub(crate) fn success(&self) -> bool {
        match self.outcome {
            Completion::KilledAfterRetry | Completion::Killed | Completion::AlreadyExited => true,
            Completion::Survived | Completion::Error => false,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    // Completion and error classification must agree with the public report schema.
    #[test]
    fn test_success_distinguishes_survivors_and_errors() {
        for (outcome, expected) in [
            (Completion::KilledAfterRetry, true),
            (Completion::Killed, true),
            (Completion::AlreadyExited, true),
            (Completion::Survived, false),
            (Completion::Error, false),
        ] {
            assert_eq!(
                Outcome {
                    pid: 123,
                    outcome,
                    detail: None
                }
                .success(),
                expected
            );
        }
        let error = crate::error::Error::Permission { reason: "denied" };
        let result = Outcome::error(123, &error);
        assert!(!result.success());
        assert_eq!(result.detail, Some(error.to_string()));
        assert_eq!(serde_json::to_value(result).unwrap()["outcome"], "error");
    }
}
