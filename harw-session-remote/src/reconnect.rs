//! Reconnect with backoff and jitter, and resume cursors (S07).
//!
//! Both types are pure and clock-free: jitter entropy is supplied by the
//! caller, so every result is deterministic and unit-testable.

use std::collections::HashMap;
use std::time::Duration;

use harw_protocol::Cursor;
use harw_types::SessionId;

use crate::RemoteError;

/// Exponential backoff with jitter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackoffPolicy {
    /// Delay before the first retry.
    pub initial: Duration,
    /// Upper bound of any delay.
    pub max: Duration,
    /// Jitter as a fraction in thousandths of the delay (0..=1000).
    pub jitter_permille: u16,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(250),
            max: Duration::from_secs(30),
            jitter_permille: 200,
        }
    }
}

impl BackoffPolicy {
    /// Delay before retry number `attempt` (0-based). `entropy` supplies the
    /// jitter so the function stays deterministic and testable.
    ///
    /// The raw delay is `initial * 2^attempt`, saturating and then capped at
    /// `max` (an `initial` above `max` is treated as `max`). Jitter only
    /// shortens: the result lies in `[capped - capped * jitter, capped]`, so
    /// it never exceeds `max`. `jitter_permille` above 1000 is clamped to 1000.
    /// `entropy` is interpreted as a fraction `entropy / 2^64`. All arithmetic
    /// is in `u128` nanoseconds with checked/saturating steps: no input can
    /// overflow or panic.
    ///
    /// # Errors
    /// Never fails today; the `Result` is the frozen signature.
    pub fn delay(&self, attempt: u32, entropy: u64) -> Result<Duration, RemoteError> {
        let cap = self.max.as_nanos();
        let initial = self.initial.as_nanos().min(cap);
        // 2^100 * (initial < 2^94) can still overflow u128; saturate to cap.
        let factor = 1u128 << attempt.min(100);
        let capped = initial.checked_mul(factor).map_or(cap, |raw| raw.min(cap));
        let permille = u128::from(self.jitter_permille.min(1000));
        let span = capped.saturating_mul(permille) / 1000;
        let frac = u128::from(entropy);
        // span * frac / 2^64 without overflowing u128 (span < 2^104).
        let reduction = (span >> 64)
            .saturating_mul(frac)
            .saturating_add(((span & u128::from(u64::MAX)) * frac) >> 64);
        let nanos = capped.saturating_sub(reduction);
        Ok(duration_from_nanos(nanos))
    }
}

/// `Duration` from u128 nanoseconds, saturating at `Duration::MAX`.
fn duration_from_nanos(nanos: u128) -> Duration {
    const NANOS_PER_SEC: u128 = 1_000_000_000;
    let Ok(secs) = u64::try_from(nanos / NANOS_PER_SEC) else {
        return Duration::MAX;
    };
    let sub = u32::try_from(nanos % NANOS_PER_SEC).unwrap_or(0);
    Duration::new(secs, sub)
}

/// Last seen cursor per attached session, used to resume after a reconnect
/// (W00 RP-01/RP-02). A bad or stale cursor makes the host answer with a
/// resync; the client never fabricates cursors.
#[derive(Clone, Debug, Default)]
pub struct ResumeCursors {
    cursors: HashMap<SessionId, Cursor>,
}

impl ResumeCursors {
    /// Empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the newest cursor seen for `session`.
    ///
    /// Monotonic per session in `(generation, durable, live)` order: an older
    /// or equal cursor is ignored. A higher generation replaces the stored
    /// cursor outright even when its `durable` is lower, because a new
    /// generation rewrote the transcript and old positions no longer apply.
    /// The live index only counts within one `(generation, durable)` pair.
    ///
    /// # Errors
    /// Never fails today; the `Result` is the frozen signature.
    pub fn record(&mut self, session: &SessionId, cursor: Cursor) -> Result<(), RemoteError> {
        match self.cursors.get_mut(session) {
            Some(current) => {
                if cursor > *current {
                    *current = cursor;
                }
            }
            None => {
                self.cursors.insert(session.clone(), cursor);
            }
        }
        Ok(())
    }

    /// Cursor to resume `session` from, if one was recorded.
    ///
    /// # Errors
    /// Never fails today; the `Result` is the frozen signature.
    pub fn cursor(&self, session: &SessionId) -> Result<Option<Cursor>, RemoteError> {
        Ok(self.cursors.get(session).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// h19: the attach/resume path records every seen frame cursor
    /// monotonically — the value a re-attach later presents as
    /// `AttachParams::from` is always the newest actually observed cursor.
    #[test]
    fn resume_cursors_are_monotonic_for_the_attach_path() {
        let session = SessionId::try_from_str("s-resume").expect("valid session id");
        let mut cursors = ResumeCursors::new();
        assert!(cursors.cursor(&session).ok().flatten().is_none());

        let first = Cursor {
            generation: 0,
            durable: 3,
            live: 1,
        };
        cursors.record(&session, first).expect("record first");
        assert_eq!(cursors.cursor(&session).ok().flatten(), Some(first));

        // Older or equal cursor: ignored.
        let older = Cursor {
            generation: 0,
            durable: 2,
            live: 9,
        };
        cursors.record(&session, older).expect("record older");
        assert_eq!(cursors.cursor(&session).ok().flatten(), Some(first));

        // Newer cursor replaces the stored one.
        let newer = Cursor {
            generation: 0,
            durable: 3,
            live: 5,
        };
        cursors.record(&session, newer).expect("record newer");
        assert_eq!(cursors.cursor(&session).ok().flatten(), Some(newer));

        // Higher generation replaces outright even with lower durable.
        let regen = Cursor {
            generation: 1,
            durable: 0,
            live: 0,
        };
        cursors.record(&session, regen).expect("record regen");
        assert_eq!(cursors.cursor(&session).ok().flatten(), Some(regen));
    }

    #[test]
    fn default_policy_first_delay_is_bounded() {
        let delay = BackoffPolicy::default().delay(0, 0);
        assert!(matches!(delay, Ok(d) if d == Duration::from_millis(250)));
    }
}
