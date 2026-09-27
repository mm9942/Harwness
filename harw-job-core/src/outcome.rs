//! Platform-neutral outcome vocabulary: how a process ended
//! ([`ExitOutcome`]), why an attempt was cancelled ([`CancellationCause`]),
//! and when it must end at the latest ([`Deadline`]).
//!
//! No platform types appear here: a signal is its raw number, not a
//! `rustix`/`nix` enum (Job-Runtime-Doc §2.5). Backends map their native
//! wait status into [`ExitOutcome`].

use std::fmt;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

/// How the primary process of an attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum ExitOutcome {
    /// The process exited normally with this status code.
    Exited(i32),
    /// The process was terminated by a signal.
    Signaled {
        /// Raw signal number.
        signal: i32,
        /// Whether the kernel reported a core dump.
        core_dumped: bool,
    },
    /// The exit status could not be observed (e.g. the process was reaped by
    /// someone else, or the runner restarted and lost the handle).
    Unknown,
}

impl ExitOutcome {
    /// Whether this is a normal exit with status `0`. [`ExitOutcome::Unknown`]
    /// is never a success.
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Exited(0))
    }
}

impl fmt::Display for ExitOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exited(code) => write!(f, "exited with status {code}"),
            Self::Signaled {
                signal,
                core_dumped: true,
            } => write!(f, "killed by signal {signal} (core dumped)"),
            Self::Signaled { signal, .. } => write!(f, "killed by signal {signal}"),
            Self::Unknown => f.write_str("exit status unknown"),
        }
    }
}

/// Why an attempt was cancelled.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum CancellationCause {
    /// A principal asked for cancellation.
    User,
    /// A [`Deadline`] elapsed.
    Deadline,
    /// A budget ceiling was reached.
    Budget,
    /// The runner lost its lease and must stop.
    LeaseLost,
    /// The runner is shutting down.
    Shutdown,
    /// A named policy decision.
    Policy(String),
}

impl fmt::Display for CancellationCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => f.write_str("cancelled by user"),
            Self::Deadline => f.write_str("deadline elapsed"),
            Self::Budget => f.write_str("budget exhausted"),
            Self::LeaseLost => f.write_str("lease lost"),
            Self::Shutdown => f.write_str("runner shutdown"),
            Self::Policy(policy) => write!(f, "policy: {policy}"),
        }
    }
}

/// Why a [`Deadline`] could not be computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeadlineError {
    /// The timeout was zero or negative.
    NonPositiveTimeout {
        /// The rejected timeout.
        timeout: SignedDuration,
    },
    /// `start + timeout` is outside the representable time range.
    Overflow {
        /// Start instant.
        start: Timestamp,
        /// Timeout that overflowed.
        timeout: SignedDuration,
    },
}

impl fmt::Display for DeadlineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonPositiveTimeout { timeout } => {
                write!(f, "deadline timeout must be positive, got {timeout}")
            }
            Self::Overflow { start, timeout } => {
                write!(f, "deadline {start} + {timeout} is out of range")
            }
        }
    }
}

impl std::error::Error for DeadlineError {}

/// An absolute instant by which an attempt must have ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Deadline(Timestamp);

impl Deadline {
    /// A deadline at an absolute instant.
    #[must_use]
    pub const fn at(instant: Timestamp) -> Self {
        Self(instant)
    }

    /// A deadline `timeout` after `start`.
    ///
    /// # Errors
    /// [`DeadlineError::NonPositiveTimeout`] for a zero or negative timeout,
    /// [`DeadlineError::Overflow`] if the sum is out of range. Both fail
    /// closed instead of producing an already-elapsed or saturated deadline.
    pub fn after(start: Timestamp, timeout: SignedDuration) -> Result<Self, DeadlineError> {
        if timeout <= SignedDuration::ZERO {
            return Err(DeadlineError::NonPositiveTimeout { timeout });
        }
        start
            .checked_add(timeout)
            .map(Self)
            .map_err(|_| DeadlineError::Overflow { start, timeout })
    }

    /// The deadline instant.
    #[must_use]
    pub const fn instant(self) -> Timestamp {
        self.0
    }

    /// Whether the deadline has elapsed at `now` (inclusive).
    #[must_use]
    pub fn is_expired(self, now: Timestamp) -> bool {
        now >= self.0
    }

    /// Time left until the deadline, saturating at zero once elapsed.
    #[must_use]
    pub fn remaining(self, now: Timestamp) -> SignedDuration {
        let left = now.duration_until(self.0);
        if left.is_negative() {
            SignedDuration::ZERO
        } else {
            left
        }
    }

    /// The earlier of two deadlines (the one that binds).
    #[must_use]
    pub fn earliest(self, other: Self) -> Self {
        self.min(other)
    }
}

impl fmt::Display for Deadline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{CancellationCause, Deadline, DeadlineError, ExitOutcome};
    use crate::test_support::{TestResult, ctx};
    use jiff::{SignedDuration, Timestamp};

    fn ts(seconds: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(seconds).map_err(ctx("timestamp"))
    }

    #[test]
    fn exit_outcome_success_is_exactly_status_zero() {
        assert!(ExitOutcome::Exited(0).is_success());
        assert!(!ExitOutcome::Exited(1).is_success());
        assert!(!ExitOutcome::Exited(-1).is_success());
        assert!(
            !ExitOutcome::Signaled {
                signal: 9,
                core_dumped: false
            }
            .is_success()
        );
        assert!(!ExitOutcome::Unknown.is_success());
    }

    #[test]
    fn exit_outcome_round_trips_through_serde() -> TestResult {
        let cases = [
            (ExitOutcome::Exited(3), r#"{"kind":"exited","detail":3}"#),
            (
                ExitOutcome::Signaled {
                    signal: 15,
                    core_dumped: true,
                },
                r#"{"kind":"signaled","detail":{"signal":15,"core_dumped":true}}"#,
            ),
            (ExitOutcome::Unknown, r#"{"kind":"unknown"}"#),
        ];
        for (outcome, expected) in cases {
            let json = serde_json::to_string(&outcome).map_err(ctx("serialize"))?;
            assert_eq!(json, expected);
            let back: ExitOutcome = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
            assert_eq!(back, outcome);
        }
        Ok(())
    }

    #[test]
    fn exit_outcome_display() {
        assert_eq!(ExitOutcome::Exited(2).to_string(), "exited with status 2");
        assert_eq!(
            ExitOutcome::Signaled {
                signal: 11,
                core_dumped: true
            }
            .to_string(),
            "killed by signal 11 (core dumped)"
        );
        assert_eq!(
            ExitOutcome::Signaled {
                signal: 9,
                core_dumped: false
            }
            .to_string(),
            "killed by signal 9"
        );
        assert_eq!(ExitOutcome::Unknown.to_string(), "exit status unknown");
    }

    #[test]
    fn cancellation_cause_round_trips_through_serde() -> TestResult {
        for cause in [
            CancellationCause::User,
            CancellationCause::Deadline,
            CancellationCause::Budget,
            CancellationCause::LeaseLost,
            CancellationCause::Shutdown,
            CancellationCause::Policy("quota".into()),
        ] {
            let json = serde_json::to_string(&cause).map_err(ctx("serialize"))?;
            let back: CancellationCause =
                serde_json::from_str(&json).map_err(ctx("deserialize"))?;
            assert_eq!(back, cause);
        }
        assert_eq!(
            serde_json::to_string(&CancellationCause::LeaseLost).map_err(ctx("lease"))?,
            r#"{"kind":"lease_lost"}"#
        );
        assert_eq!(
            CancellationCause::Policy("quota".into()).to_string(),
            "policy: quota"
        );
        Ok(())
    }

    #[test]
    fn deadline_after_computes_expiry_and_remaining_time() -> TestResult {
        let start = ts(1_000)?;
        let deadline =
            Deadline::after(start, SignedDuration::from_secs(30)).map_err(ctx("deadline"))?;
        assert_eq!(deadline.instant(), ts(1_030)?);
        assert!(!deadline.is_expired(start));
        assert!(!deadline.is_expired(ts(1_029)?));
        assert!(deadline.is_expired(ts(1_030)?), "expiry is inclusive");
        assert!(deadline.is_expired(ts(2_000)?));
        assert_eq!(deadline.remaining(start), SignedDuration::from_secs(30));
        assert_eq!(deadline.remaining(ts(1_030)?), SignedDuration::ZERO);
        assert_eq!(
            deadline.remaining(ts(5_000)?),
            SignedDuration::ZERO,
            "remaining saturates at zero"
        );
        Ok(())
    }

    #[test]
    fn deadline_rejects_non_positive_and_overflowing_timeouts() -> TestResult {
        let start = ts(0)?;
        for timeout in [SignedDuration::ZERO, SignedDuration::from_secs(-5)] {
            assert_eq!(
                Deadline::after(start, timeout),
                Err(DeadlineError::NonPositiveTimeout { timeout })
            );
        }
        let timeout = SignedDuration::from_secs(1);
        assert_eq!(
            Deadline::after(Timestamp::MAX, timeout),
            Err(DeadlineError::Overflow {
                start: Timestamp::MAX,
                timeout
            })
        );
        Ok(())
    }

    #[test]
    fn earliest_deadline_binds_and_serde_is_transparent() -> TestResult {
        let early = Deadline::at(ts(10)?);
        let late = Deadline::at(ts(20)?);
        assert_eq!(early.earliest(late), early);
        assert_eq!(late.earliest(early), early);

        let json = serde_json::to_string(&early).map_err(ctx("serialize"))?;
        let expected = serde_json::to_string(&ts(10)?).map_err(ctx("timestamp json"))?;
        assert_eq!(json, expected);
        let back: Deadline = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, early);
        Ok(())
    }
}
