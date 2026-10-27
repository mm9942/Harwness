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
//! - **Two-phase delivery.** A consumer whose delivery has an external side
//!   effect first [accepts](Outbox::accept) an entry ([`DeliveryState`]),
//!   storing *what* it will deliver (an opaque, consumer-versioned
//!   materialisation) with it: the entry stays pending, marked as committed
//!   to delivery, until the consumer acknowledges it after the side effect
//!   is durable. A crash in between leaves it accepted, so it is delivered
//!   again (at least once) — from the stored materialisation, so a newer
//!   consumer replays exactly what was decided instead of rendering it
//!   anew; the side effect must therefore be idempotent. Entries the
//!   consumer decides not to deliver are acknowledged directly.
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

/// Delivery state of a pending [`OutboxEntry`] (see the module docs,
/// "Two-phase delivery").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    /// Not decided by the consumer yet.
    #[default]
    Pending,
    /// The consumer committed to delivering it; it stays pending until it is
    /// acknowledged after the delivery is durable.
    Accepted,
}

/// One outbox entry: an idempotency key, the epoch of the writer that
/// produced it, a sequence number, the payload to deliver and its delivery
/// state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboxEntry<T> {
    /// Idempotency key; an entry with a known key is never added twice.
    pub key: String,
    /// Epoch of the writer that produced the entry.
    pub epoch: u64,
    /// Sequence number assigned by the writer.
    pub sequence: u64,
    /// Payload to deliver.
    pub payload: T,
    /// Where the consumer stands with it. Entries written before two-phase
    /// delivery have no such field and load as [`DeliveryState::Pending`].
    #[serde(default)]
    pub state: DeliveryState,
    /// What the consumer committed to deliver, stored by [`Outbox::accept`]:
    /// opaque to the outbox and versioned by the consumer. `None` while
    /// pending, and for entries accepted before materialisations were
    /// stored. Not serialised when absent, so pending entries keep their
    /// former shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization: Option<serde_json::Value>,
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

    /// Marks the pending entry with `key` as [`DeliveryState::Accepted`] and
    /// stores `materialization` — what the consumer committed to deliver —
    /// with it. The entry stays pending (and counts against
    /// [`MAX_OUTBOX_PENDING`]) until [`Outbox::ack`].
    ///
    /// Returns `Ok(true)` when a pending entry changed to accepted,
    /// `Ok(false)` for an unknown, already acked or already accepted key; an
    /// accepted entry keeps the materialisation it was accepted with.
    ///
    /// # Errors
    ///
    /// [`OutboxError::StaleEpoch`] when `acceptor_epoch < current_epoch`.
    pub fn accept(
        &mut self,
        acceptor_epoch: u64,
        current_epoch: u64,
        key: &str,
        materialization: serde_json::Value,
    ) -> Result<bool, OutboxError> {
        if acceptor_epoch < current_epoch {
            return Err(OutboxError::StaleEpoch);
        }
        let Some(entry) = self.pending.iter_mut().find(|entry| entry.key == key) else {
            return Ok(false);
        };
        if entry.state == DeliveryState::Accepted {
            return Ok(false);
        }
        entry.state = DeliveryState::Accepted;
        entry.materialization = Some(materialization);
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
            state: DeliveryState::Pending,
            materialization: None,
        }
    }

    fn materialization_of(outbox: &Outbox<String>, key: &str) -> Option<serde_json::Value> {
        outbox
            .pending()
            .iter()
            .find(|entry| entry.key == key)
            .and_then(|entry| entry.materialization.clone())
    }

    fn state_of(outbox: &Outbox<String>, key: &str) -> Option<DeliveryState> {
        outbox
            .pending()
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.state)
    }

    #[test]
    fn accept_marks_once_keeps_the_entry_pending_and_is_fenced() -> TestResult {
        let mut outbox = Outbox::new();
        check(outbox.append(2, 2, entry("a", 0)) == Ok(true), "append a")?;
        check(outbox.append(2, 2, entry("b", 1)) == Ok(true), "append b")?;
        check(
            outbox.accept(1, 2, "a", serde_json::json!({"v": "a"})) == Err(OutboxError::StaleEpoch),
            "stale accept refused",
        )?;
        check(
            state_of(&outbox, "a") == Some(DeliveryState::Pending),
            "stale accept changed nothing",
        )?;
        check(
            outbox.accept(2, 2, "a", serde_json::json!({"v": "a"})) == Ok(true),
            "first accept marks",
        )?;
        check(
            outbox.accept(2, 2, "a", serde_json::json!({"v": "a"})) == Ok(false),
            "second accept is a no-op",
        )?;
        check(
            outbox.accept(2, 2, "unknown", serde_json::json!({"v": "unknown"})) == Ok(false),
            "unknown key is a no-op",
        )?;
        check(
            state_of(&outbox, "a") == Some(DeliveryState::Accepted),
            "a is accepted",
        )?;
        check(
            materialization_of(&outbox, "a") == Some(serde_json::json!({"v": "a"})),
            "the materialisation is stored with the decision",
        )?;
        check(
            materialization_of(&outbox, "b").is_none(),
            "a pending entry has none",
        )?;
        check(
            state_of(&outbox, "b") == Some(DeliveryState::Pending),
            "b is untouched",
        )?;
        check(
            keys(&outbox) == ["a", "b"],
            "accepted stays pending, in order",
        )?;
        check(
            outbox.append(2, 2, entry("a", 9)) == Ok(false),
            "re-append of an accepted key is a no-op",
        )?;
        check(
            state_of(&outbox, "a") == Some(DeliveryState::Accepted),
            "re-append does not reset the state",
        )?;
        check(
            outbox.accept(2, 2, "a", serde_json::json!({"v": "other"})) == Ok(false),
            "a second accept is a no-op",
        )?;
        check(
            materialization_of(&outbox, "a") == Some(serde_json::json!({"v": "a"})),
            "the first materialisation is kept",
        )?;
        check(
            outbox.ack(2, 2, "a") == Ok(true),
            "ack removes the accepted",
        )?;
        check(
            outbox.accept(2, 2, "a", serde_json::json!({"v": "a"})) == Ok(false),
            "an acked key cannot be accepted",
        )?;
        check(keys(&outbox) == ["b"], "only b is left")
    }

    #[test]
    fn accepted_state_survives_a_round_trip_and_legacy_entries_load_pending() -> TestResult {
        let mut outbox = Outbox::new();
        check(outbox.append(1, 1, entry("a", 0)) == Ok(true), "append")?;
        check(
            outbox.accept(1, 1, "a", serde_json::json!({"v": "a"})) == Ok(true),
            "accept",
        )?;
        let json = serde_json::to_string(&outbox).map_err(ctx("serialize outbox"))?;
        let restored: Outbox<String> =
            serde_json::from_str(&json).map_err(ctx("deserialize outbox"))?;
        check(restored == outbox, "accepted survives a crash")?;

        let legacy = r#"{"pending":[{"key":"k","epoch":1,"sequence":0,"payload":"p"}],"acked_keys_tail":[]}"#;
        let loaded: Outbox<String> =
            serde_json::from_str(legacy).map_err(ctx("pre two-phase outbox"))?;
        check(
            state_of(&loaded, "k") == Some(DeliveryState::Pending),
            "an entry without a state loads as pending",
        )?;
        check(
            !serde_json::to_string(&loaded)
                .map_err(ctx("serialize legacy outbox"))?
                .contains("materialization"),
            "a pending entry keeps its former shape",
        )?;
        let accepted_legacy = r#"{"pending":[{"key":"k","epoch":1,"sequence":0,"payload":"p","state":"accepted"}],"acked_keys_tail":[]}"#;
        let accepted: Outbox<String> = serde_json::from_str(accepted_legacy)
            .map_err(ctx("accepted without materialisation"))?;
        check(
            state_of(&accepted, "k") == Some(DeliveryState::Accepted)
                && materialization_of(&accepted, "k").is_none(),
            "an entry accepted before materialisations were stored has none",
        )?;
        let tampered = r#"{"pending":[{"key":"k","epoch":1,"sequence":0,"payload":"p","extra":1}],"acked_keys_tail":[]}"#;
        check(
            serde_json::from_str::<Outbox<String>>(tampered).is_err(),
            "unknown entry fields are rejected",
        )
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
