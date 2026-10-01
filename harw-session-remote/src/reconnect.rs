//! Reconnect with backoff and jitter, and resume cursors (S07).
//!
//! Skeleton: signatures are frozen, bodies answer
//! [`RemoteError::NotImplemented`].

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
    /// # Errors
    /// [`RemoteError::NotImplemented`] in the skeleton.
    pub fn delay(&self, attempt: u32, entropy: u64) -> Result<Duration, RemoteError> {
        let _ = (attempt, entropy);
        Err(RemoteError::NotImplemented("BackoffPolicy::delay (S07)"))
    }
}

/// Last seen cursor per attached session, used to resume after a reconnect
/// (W00 RP-01/RP-02). A bad or stale cursor makes the host answer with a
/// resync; the client never fabricates cursors.
#[derive(Clone, Debug, Default)]
pub struct ResumeCursors {
    _private: (),
}

impl ResumeCursors {
    /// Empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the newest cursor seen for `session`.
    ///
    /// # Errors
    /// [`RemoteError::NotImplemented`] in the skeleton.
    pub fn record(&mut self, session: &SessionId, cursor: Cursor) -> Result<(), RemoteError> {
        let _ = (session, cursor);
        Err(RemoteError::NotImplemented("ResumeCursors::record (S07)"))
    }

    /// Cursor to resume `session` from, if one was recorded.
    ///
    /// # Errors
    /// [`RemoteError::NotImplemented`] in the skeleton.
    pub fn cursor(&self, session: &SessionId) -> Result<Option<Cursor>, RemoteError> {
        let _ = session;
        Err(RemoteError::NotImplemented("ResumeCursors::cursor (S07)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_is_typed_not_implemented() {
        let result = BackoffPolicy::default().delay(0, 0);
        assert!(matches!(result, Err(RemoteError::NotImplemented(_))));
    }
}
