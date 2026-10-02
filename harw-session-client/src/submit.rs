//! What to do with the answer to `turn.submit`.
//!
//! The host answers one of four ways. [`interpret_submit`] turns the answer
//! into the step a client takes, so every client reacts the same way:
//! queue position becomes a number of turns to wait for, a moved head means
//! retry with the new head, anything else is a refusal with a message.

use std::fmt;

use harw_protocol::session_wire::{Cursor, SubmitResult};

/// How often a client retries a prompt whose head was stale. The same
/// idempotency key is used for every attempt.
pub const MAX_SUBMIT_ATTEMPTS: usize = 3;

/// Why the host did not take the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitRefusal {
    /// The per-session queue is full.
    QueueFull,
    /// Not allowed (capabilities, tenant, state), with the host's reason.
    Denied(String),
    /// An answer this client does not know (a newer host).
    Unknown,
}

impl fmt::Display for SubmitRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueFull => f.write_str("the host's queue for this session is full"),
            Self::Denied(reason) => write!(f, "not allowed: {reason}"),
            Self::Unknown => f.write_str("the host answered with an unknown submit result"),
        }
    }
}

/// The step after one `turn.submit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitStep {
    /// The prompt is queued. `ahead` is how many turns run or wait before
    /// it; the client waits for `ahead + 1` turn ends.
    Accepted {
        /// Turns before this one.
        ahead: usize,
    },
    /// The transcript moved on: send again with this head.
    Retry {
        /// The head to expect now.
        head: Cursor,
    },
    /// The host refused.
    Refused(SubmitRefusal),
}

/// Interprets one answer.
#[must_use]
pub fn interpret_submit(result: SubmitResult) -> SubmitStep {
    match result {
        SubmitResult::Accepted { position } => SubmitStep::Accepted {
            ahead: usize::try_from(position).unwrap_or(usize::MAX),
        },
        SubmitResult::Stale { head } => SubmitStep::Retry { head },
        SubmitResult::QueueFull => SubmitStep::Refused(SubmitRefusal::QueueFull),
        SubmitResult::Denied { reason } => SubmitStep::Refused(SubmitRefusal::Denied(reason)),
        #[allow(unreachable_patterns)]
        _ => SubmitStep::Refused(SubmitRefusal::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    #[test]
    fn the_queue_position_becomes_the_number_of_turns_ahead() -> TestResult {
        ensure(
            interpret_submit(SubmitResult::Accepted { position: 0 })
                == SubmitStep::Accepted { ahead: 0 },
            "runs now",
        )?;
        ensure(
            interpret_submit(SubmitResult::Accepted { position: 2 })
                == SubmitStep::Accepted { ahead: 2 },
            "two ahead",
        )
    }

    #[test]
    fn a_moved_head_means_retry_with_it() -> TestResult {
        let head = Cursor {
            generation: 1,
            durable: 7,
            live: 0,
        };
        ensure(
            interpret_submit(SubmitResult::Stale { head }) == SubmitStep::Retry { head },
            "retry",
        )
    }

    #[test]
    fn refusals_carry_a_readable_message() -> TestResult {
        ensure(
            interpret_submit(SubmitResult::QueueFull)
                == SubmitStep::Refused(SubmitRefusal::QueueFull),
            "queue full",
        )?;
        let denied = interpret_submit(SubmitResult::Denied {
            reason: "tenant".to_owned(),
        });
        ensure(
            matches!(&denied, SubmitStep::Refused(r) if r.to_string() == "not allowed: tenant"),
            "denied with the reason",
        )
    }
}
