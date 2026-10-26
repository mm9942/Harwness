//! Bounded, epoch-fenced, idempotent outbox state machine (PL-90 W03c-2,
//! `docs/planning/90-intent-driven-composed-agents/05-error-learning-and-run-isolation.md`
//! section 3).
//!
//! The outbox holds entries that still have to be delivered to a downstream
//! store. It is a pure value type: no I/O, no clock, no dependency on other
//! cycle modules. Persistence is the caller's job; the type is `serde`
//! round-trippable so that a crashed writer can reload it and replay.
//!
//! Guarantees:
//!
//! - **Bounded.** At most [`MAX_OUTBOX_PENDING`] entries are pending. A full
//!   outbox refuses new entries with [`OutboxError::Full`]; nothing already
//!   pending is ever dropped.
//! - **Epoch-fenced.** A writer or acker whose epoch is older than the current
//!   epoch is refused with [`OutboxError::StaleEpoch`] and changes nothing.
//! - **Idempotent.** Appending a key that is pending, or that was acked
//!   recently, is a no-op reported as `Ok(false)`. Acking an unknown or
//!   already acked key is likewise `Ok(false)`. After a crash the writer can
//!   therefore replay every append without creating duplicates.
//! - **Ordered.** Pending entries keep their append order.
//!
//! The acked-key tail is capped at [`MAX_ACKED_TAIL`] keys (oldest evicted
//! first), so idempotency of re-appends covers the most recent acked keys
//! only; older acked keys are the downstream store's responsibility.

use std::collections::VecDeque;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Maximum number of entries an [`Outbox`] holds as pending.
pub const MAX_OUTBOX_PENDING: usize = 64;

/// Maximum number of acked keys remembered for idempotent re-appends.
pub const MAX_ACKED_TAIL: usize = 128;

/// One outbox entry: an idempotency key, the epoch of the writer that
/// produced it, a sequence number and the payload to deliver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry<T> {
    /// Idempotency key; an entry with a known key is never added twice.
    pub key: String,
    /// Epoch of the writer that produced the entry.
    pub epoch: u64,
    /// Sequence number assigned by the writer.
    pub sequence: u64,
    /// Payload to deliver.
    pub payload: T,
}

/// Why an outbox operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxError {
    /// The outbox already holds [`MAX_OUTBOX_PENDING`] pending entries.
    Full,
    /// The caller's epoch is older than the current epoch.
    StaleEpoch,
}

impl fmt::Display for OutboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => write!(f, "outbox is full ({MAX_OUTBOX_PENDING} pending entries)"),
            Self::StaleEpoch => write!(f, "caller epoch is older than the current epoch"),
        }
    }
}

impl std::error::Error for OutboxError {}

/// Bounded, epoch-fenced, idempotent outbox (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    deny_unknown_fields,
    bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>")
)]
pub struct Outbox<T> {
    pending: Vec<OutboxEntry<T>>,
    acked_keys_tail: VecDeque<String>,
}

impl<T> Default for Outbox<T> {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            acked_keys_tail: VecDeque::new(),
        }
    }
}

impl<T> Outbox<T> {
    /// Creates an empty outbox.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `entry` unless it is a duplicate or the outbox is full.
    ///
    /// Returns `Ok(true)` when the entry was added, `Ok(false)` when its key
    /// is already pending or in the acked tail (idempotent replay).
    ///
    /// # Errors
    ///
    /// - [`OutboxError::StaleEpoch`] when `writer_epoch < current_epoch`.
    /// - [`OutboxError::Full`] when the key is new and
    ///   [`MAX_OUTBOX_PENDING`] entries are already pending; nothing is
    ///   dropped.
    pub fn append(
        &mut self,
        writer_epoch: u64,
        current_epoch: u64,
        entry: OutboxEntry<T>,
    ) -> Result<bool, OutboxError> {
        if writer_epoch < current_epoch {
            return Err(OutboxError::StaleEpoch);
        }
        if self.knows_key(&entry.key) {
            return Ok(false);
        }
        if self.pending.len() >= MAX_OUTBOX_PENDING {
            return Err(OutboxError::Full);
        }
        self.pending.push(entry);
        Ok(true)
    }

    /// Acknowledges the pending entry with `key`, moving its key to the
    /// acked tail (the oldest tail key is evicted beyond [`MAX_ACKED_TAIL`]).
    ///
    /// Returns `Ok(true)` when a pending entry was removed, `Ok(false)` for
    /// an unknown or already acked key.
    ///
    /// # Errors
    ///
    /// [`OutboxError::StaleEpoch`] when `acker_epoch < current_epoch`.
    pub fn ack(
        &mut self,
        acker_epoch: u64,
        current_epoch: u64,
        key: &str,
    ) -> Result<bool, OutboxError> {
        if acker_epoch < current_epoch {
            return Err(OutboxError::StaleEpoch);
        }
        let Some(index) = self.pending.iter().position(|entry| entry.key == key) else {
            return Ok(false);
        };
        let removed = self.pending.remove(index);
        self.acked_keys_tail.push_back(removed.key);
        while self.acked_keys_tail.len() > MAX_ACKED_TAIL {
            self.acked_keys_tail.pop_front();
        }
        Ok(true)
    }

    /// All pending entries in append order.
    #[must_use]
    pub fn pending(&self) -> &[OutboxEntry<T>] {
        &self.pending
    }

    /// The first `max` pending entries in append order.
    #[must_use]
    pub fn drain_batch(&self, max: usize) -> Vec<&OutboxEntry<T>> {
        self.pending.iter().take(max).collect()
    }

    /// Number of pending entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether no entry is pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    fn knows_key(&self, key: &str) -> bool {
        self.pending.iter().any(|entry| entry.key == key)
            || self.acked_keys_tail.iter().any(|acked| acked == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn check(condition: bool, what: &str) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(TestError::Unexpected(what.to_owned()))
        }
    }

    fn entry(key: &str, sequence: u64) -> OutboxEntry<String> {
        OutboxEntry {
            key: key.to_owned(),
            epoch: 1,
            sequence,
            payload: format!("payload-{key}"),
        }
    }

    fn keys(outbox: &Outbox<String>) -> Vec<&str> {
        outbox.pending().iter().map(|e| e.key.as_str()).collect()
    }

    #[test]
    fn append_is_idempotent() -> TestResult {
        let mut outbox = Outbox::new();
        check(
            outbox.append(1, 1, entry("a", 0)) == Ok(true),
            "first append adds",
        )?;
        check(
            outbox.append(1, 1, entry("a", 1)) == Ok(false),
            "duplicate pending key is a no-op",
        )?;
        check(outbox.len() == 1, "one entry pending")?;
        check(!outbox.is_empty(), "outbox is not empty")
    }

    #[test]
    fn stale_epoch_cannot_append_or_ack() -> TestResult {
        let mut outbox = Outbox::new();
        check(
            outbox.append(1, 2, entry("a", 0)) == Err(OutboxError::StaleEpoch),
            "stale append refused",
        )?;
        check(outbox.is_empty(), "stale append changed nothing")?;
        check(
            outbox.append(2, 2, entry("a", 0)) == Ok(true),
            "current epoch appends",
        )?;
        check(
            outbox.ack(1, 2, "a") == Err(OutboxError::StaleEpoch),
            "stale ack refused",
        )?;
        check(outbox.len() == 1, "stale ack changed nothing")?;
        check(
            outbox.append(3, 2, entry("b", 1)) == Ok(true),
            "newer writer epoch appends",
        )
    }

    #[test]
    fn full_leaves_pending_unchanged() -> TestResult {
        let mut outbox = Outbox::new();
        for i in 0..MAX_OUTBOX_PENDING {
            let key = format!("k{i}");
            check(
                outbox.append(1, 1, entry(&key, i as u64)) == Ok(true),
                "fill",
            )?;
        }
        let before = outbox.clone();
        check(
            outbox.append(1, 1, entry("overflow", 999)) == Err(OutboxError::Full),
            "full outbox refuses a new key",
        )?;
        check(outbox == before, "pending unchanged after Full")?;
        check(
            outbox.append(1, 1, entry("k0", 1000)) == Ok(false),
            "duplicate is still idempotent when full",
        )?;
        check(outbox.len() == MAX_OUTBOX_PENDING, "still at the bound")
    }

    #[test]
    fn ack_is_idempotent() -> TestResult {
        let mut outbox = Outbox::new();
        check(outbox.append(1, 1, entry("a", 0)) == Ok(true), "append")?;
        check(outbox.ack(1, 1, "a") == Ok(true), "first ack removes")?;
        check(outbox.ack(1, 1, "a") == Ok(false), "second ack is a no-op")?;
        check(
            outbox.ack(1, 1, "unknown") == Ok(false),
            "unknown key is a no-op",
        )?;
        check(outbox.is_empty(), "nothing pending")
    }

    #[test]
    fn crash_replay_creates_no_duplicates() -> TestResult {
        let mut outbox = Outbox::new();
        for (i, key) in ["a", "b", "c"].iter().enumerate() {
            check(
                outbox.append(1, 1, entry(key, i as u64)) == Ok(true),
                "append",
            )?;
        }
        check(outbox.ack(1, 1, "a") == Ok(true), "ack a")?;

        let json = serde_json::to_string(&outbox).map_err(ctx("serialize outbox"))?;
        let mut restored: Outbox<String> =
            serde_json::from_str(&json).map_err(ctx("deserialize outbox"))?;
        check(restored == outbox, "round trip is lossless")?;

        for (i, key) in ["a", "b", "c"].iter().enumerate() {
            check(
                restored.append(1, 1, entry(key, i as u64)) == Ok(false),
                "replayed key (pending or acked) is not added again",
            )?;
        }
        check(
            keys(&restored) == ["b", "c"],
            "no duplicates, acked stays acked",
        )
    }

    #[test]
    fn order_is_preserved() -> TestResult {
        let mut outbox = Outbox::new();
        for (i, key) in ["x", "y", "z", "w"].iter().enumerate() {
            check(
                outbox.append(1, 1, entry(key, i as u64)) == Ok(true),
                "append",
            )?;
        }
        check(outbox.ack(1, 1, "y") == Ok(true), "ack middle entry")?;
        check(
            keys(&outbox) == ["x", "z", "w"],
            "append order survives ack",
        )?;
        let batch: Vec<&str> = outbox
            .drain_batch(2)
            .into_iter()
            .map(|e| e.key.as_str())
            .collect();
        check(
            batch == ["x", "z"],
            "batch holds the first entries in order",
        )?;
        check(outbox.drain_batch(10).len() == 3, "batch is capped by len")?;
        check(outbox.drain_batch(0).is_empty(), "zero batch is empty")
    }

    #[test]
    fn acked_tail_is_capped() -> TestResult {
        let mut outbox = Outbox::new();
        let total = MAX_ACKED_TAIL + 2;
        for i in 0..total {
            let key = format!("k{i}");
            check(
                outbox.append(1, 1, entry(&key, i as u64)) == Ok(true),
                "append",
            )?;
            check(outbox.ack(1, 1, &key) == Ok(true), "ack")?;
        }
        check(
            outbox.acked_keys_tail.len() == MAX_ACKED_TAIL,
            "tail holds at most the cap",
        )?;
        let newest = format!("k{}", total - 1);
        check(
            outbox.append(1, 1, entry(&newest, 0)) == Ok(false),
            "recent acked key is still remembered",
        )?;
        check(
            outbox.append(1, 1, entry("k0", 0)) == Ok(true),
            "evicted acked key is forgotten",
        )
    }

    #[test]
    fn unknown_fields_are_rejected() -> TestResult {
        let json = r#"{"pending":[],"acked_keys_tail":[],"extra":1}"#;
        check(
            serde_json::from_str::<Outbox<String>>(json).is_err(),
            "extra field must be rejected",
        )?;
        let ok = r#"{"pending":[],"acked_keys_tail":[]}"#;
        let outbox: Outbox<String> = serde_json::from_str(ok).map_err(ctx("valid outbox"))?;
        check(outbox.is_empty(), "valid document loads empty")
    }
}
