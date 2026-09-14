//! Bounded, deterministic backpressure for the normalized BiDi event journal.
//!
//! # Description
//! [`EventJournalPolicy`] caps the journal both by entry count and by
//! estimated retained bytes; individual event strings are truncated to
//! [`MAX_EVENT_FIELD_BYTES`] and oversized script payloads are replaced by a
//! marker before they are stored. The crate-private `EventJournal` applies
//! the policy: when full, critical events evict the oldest entries (non-critical
//! first) and every other class is accounted in [`BackpressureStats`] instead of
//! being retained. Memory use is therefore bounded regardless of page behavior.
//!
//! # Concurrency
//! `EventJournal` is not synchronized; the runtime guards it with a Tokio mutex.
//!
//! # Errors
//! [`EventJournalPolicy::bounded`] / [`EventJournalPolicy::with_max_bytes`]
//! reject zero or over-limit configurations with `InvalidArgument`.
//!
//! # Examples
//! ```rust,no_run
//! use harw_browser_thirtyfour::journal::EventJournalPolicy;
//!
//! # fn main() -> harw_browser::Result<()> {
//! let policy = EventJournalPolicy::bounded(256)?.with_max_bytes(64 * 1024)?;
//! assert_eq!(policy.max_bytes(), 64 * 1024);
//! # Ok(())
//! # }
//! ```

use harw_browser::error::Error as BrowserError;
use harw_browser::event::{BackpressureStats, BrowserEvent, EventClass, EventEnvelope};
use harw_browser::ids::BrowserEventCursor;
use std::collections::VecDeque;
use std::num::NonZeroUsize;

/// Default number of retained journal entries per session.
pub const DEFAULT_JOURNAL_CAPACITY: usize = 1_024;
/// Default retained-bytes budget per session (1 MiB).
pub const DEFAULT_JOURNAL_MAX_BYTES: usize = 1024 * 1024;
/// Hard ceiling for the retained-bytes budget (16 MiB).
pub const HARD_MAX_JOURNAL_BYTES: usize = 16 * 1024 * 1024;
/// Hard ceiling for the entry capacity.
pub const HARD_MAX_JOURNAL_CAPACITY: usize = 65_536;
/// Maximum bytes kept per string field or serialized script payload.
pub const MAX_EVENT_FIELD_BYTES: usize = 8 * 1024;

// Fixed per-envelope overhead added to the byte estimate (ids, cursor, timestamp).
const ENVELOPE_OVERHEAD_BYTES: usize = 128;

/// Event classes with distinct retention and pressure characteristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BidiEventClass {
    CriticalControl,
    RequestLifecycle,
    Console,
    StaticAsset,
    ArtifactPayload,
}

/// Required behavior when an event class encounters journal pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackpressureDisposition {
    Retain,
    AggregateWhenFull,
    Deduplicate,
    Sample,
    StoreExternally,
}

/// Entry and byte capacity plus deterministic per-class pressure behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventJournalPolicy {
    capacity: NonZeroUsize,
    max_bytes: NonZeroUsize,
}

impl EventJournalPolicy {
    /// Creates a policy with `capacity` entries and [`DEFAULT_JOURNAL_MAX_BYTES`].
    ///
    /// # Errors
    /// - `InvalidArgument`: capacity is zero or above [`HARD_MAX_JOURNAL_CAPACITY`].
    pub fn bounded(capacity: usize) -> harw_browser::Result<Self> {
        let Some(capacity) = NonZeroUsize::new(capacity) else {
            return Err(BrowserError::InvalidArgument {
                detail: "event journal capacity must be greater than zero".to_owned(),
            });
        };
        if capacity.get() > HARD_MAX_JOURNAL_CAPACITY {
            return Err(BrowserError::InvalidArgument {
                detail: format!(
                    "event journal capacity must not exceed {HARD_MAX_JOURNAL_CAPACITY}"
                ),
            });
        }
        let Some(max_bytes) = NonZeroUsize::new(DEFAULT_JOURNAL_MAX_BYTES) else {
            return Err(BrowserError::InvalidArgument {
                detail: "default event journal byte budget must be greater than zero".to_owned(),
            });
        };
        Ok(Self {
            capacity,
            max_bytes,
        })
    }

    /// Replaces the retained-bytes budget.
    ///
    /// # Errors
    /// - `InvalidArgument`: zero or above [`HARD_MAX_JOURNAL_BYTES`].
    pub fn with_max_bytes(self, max_bytes: usize) -> harw_browser::Result<Self> {
        let Some(max_bytes) = NonZeroUsize::new(max_bytes)
            .filter(|bytes| bytes.get() <= HARD_MAX_JOURNAL_BYTES)
        else {
            return Err(BrowserError::InvalidArgument {
                detail: format!(
                    "event journal byte budget must be 1..={HARD_MAX_JOURNAL_BYTES}"
                ),
            });
        };
        Ok(Self { max_bytes, ..self })
    }

    /// Returns the entry capacity.
    pub fn capacity(&self) -> usize {
        self.capacity.get()
    }

    /// Returns the retained-bytes budget.
    pub fn max_bytes(&self) -> usize {
        self.max_bytes.get()
    }

    /// Returns the pressure behavior for `event_class`.
    pub fn disposition(&self, event_class: BidiEventClass) -> BackpressureDisposition {
        match event_class {
            BidiEventClass::CriticalControl => BackpressureDisposition::Retain,
            BidiEventClass::RequestLifecycle => BackpressureDisposition::AggregateWhenFull,
            BidiEventClass::Console => BackpressureDisposition::Deduplicate,
            BidiEventClass::StaticAsset => BackpressureDisposition::Sample,
            BidiEventClass::ArtifactPayload => BackpressureDisposition::StoreExternally,
        }
    }
}

// Retained entry with its byte estimate.
struct JournalEntry {
    envelope: EventEnvelope,
    bytes: usize,
}

/// Bounded in-memory event journal of one session.
pub(crate) struct EventJournal {
    policy: EventJournalPolicy,
    entries: VecDeque<JournalEntry>,
    retained_bytes: usize,
    stats: BackpressureStats,
}

impl EventJournal {
    pub(crate) fn new(policy: EventJournalPolicy) -> Self {
        Self {
            policy,
            entries: VecDeque::with_capacity(policy.capacity().min(256)),
            retained_bytes: 0,
            stats: BackpressureStats::default(),
        }
    }

    /// Stores `envelope` or accounts it as pressure; never exceeds either cap.
    pub(crate) fn push(&mut self, mut envelope: EventEnvelope) -> BackpressureDisposition {
        bound_event(&mut envelope.event);
        let bytes = estimate_event_bytes(&envelope.event);
        let fits = |journal: &Self| {
            journal.entries.len() < journal.policy.capacity()
                && journal.retained_bytes + bytes <= journal.policy.max_bytes()
        };

        if fits(self) {
            self.retain(envelope, bytes);
            return BackpressureDisposition::Retain;
        }

        if envelope.class == EventClass::Critical {
            while !fits(self) {
                let position = self
                    .entries
                    .iter()
                    .position(|entry| entry.envelope.class != EventClass::Critical)
                    .unwrap_or(0);
                let Some(evicted) = self.entries.remove(position) else {
                    break;
                };
                self.retained_bytes = self.retained_bytes.saturating_sub(evicted.bytes);
                self.stats.record_drop();
            }
            if fits(self) {
                self.retain(envelope, bytes);
            } else {
                self.stats.record_drop();
            }
            return BackpressureDisposition::Retain;
        }

        let disposition = self.policy.disposition(match envelope.class {
            EventClass::Critical => BidiEventClass::CriticalControl,
            EventClass::RequestLifecycle => BidiEventClass::RequestLifecycle,
            EventClass::ConsoleRepetition => BidiEventClass::Console,
            EventClass::StaticAsset => BidiEventClass::StaticAsset,
            EventClass::Artifact => BidiEventClass::ArtifactPayload,
        });
        match disposition {
            BackpressureDisposition::AggregateWhenFull => self.stats.record_aggregate(),
            BackpressureDisposition::Deduplicate => self.stats.record_deduplicate(),
            BackpressureDisposition::Retain
            | BackpressureDisposition::Sample
            | BackpressureDisposition::StoreExternally => self.stats.record_drop(),
        }
        disposition
    }

    /// Returns clones of all retained events after `since`.
    pub(crate) fn since(&self, since: BrowserEventCursor) -> Vec<EventEnvelope> {
        self.entries
            .iter()
            .filter(|entry| entry.envelope.cursor.value() > since.value())
            .map(|entry| entry.envelope.clone())
            .collect()
    }

    /// Returns the cursor of the newest retained event.
    pub(crate) fn last_cursor(&self) -> Option<BrowserEventCursor> {
        self.entries.back().map(|entry| entry.envelope.cursor)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> &BackpressureStats {
        &self.stats
    }

    fn retain(&mut self, envelope: EventEnvelope, bytes: usize) {
        self.retained_bytes += bytes;
        self.entries.push_back(JournalEntry { envelope, bytes });
    }
}

// Truncates oversized strings and replaces oversized script payloads in place.
fn bound_event(event: &mut BrowserEvent) {
    match event {
        BrowserEvent::Navigation { url, title, .. } => {
            truncate_utf8(url, MAX_EVENT_FIELD_BYTES);
            if let Some(title) = title {
                truncate_utf8(title, MAX_EVENT_FIELD_BYTES);
            }
        }
        BrowserEvent::NetworkRequest {
            request_id,
            method,
            url,
            ..
        } => {
            truncate_utf8(request_id, MAX_EVENT_FIELD_BYTES);
            truncate_utf8(method, MAX_EVENT_FIELD_BYTES);
            truncate_utf8(url, MAX_EVENT_FIELD_BYTES);
        }
        BrowserEvent::NetworkResponse { request_id, .. } => {
            truncate_utf8(request_id, MAX_EVENT_FIELD_BYTES);
        }
        BrowserEvent::ConsoleEntry { level, message, .. } => {
            truncate_utf8(level, MAX_EVENT_FIELD_BYTES);
            truncate_utf8(message, MAX_EVENT_FIELD_BYTES);
        }
        BrowserEvent::ScriptMessage {
            channel, payload, ..
        } => {
            truncate_utf8(channel, MAX_EVENT_FIELD_BYTES);
            let size = payload_bytes(payload);
            if size > MAX_EVENT_FIELD_BYTES {
                *payload = serde_json::Value::String(format!(
                    "[harwness: script payload of {size} bytes dropped, limit {MAX_EVENT_FIELD_BYTES}]"
                ));
            }
        }
        BrowserEvent::DriverLog { level, message } => {
            truncate_utf8(level, MAX_EVENT_FIELD_BYTES);
            truncate_utf8(message, MAX_EVENT_FIELD_BYTES);
        }
    }
}

// Estimates retained heap bytes of a (bounded) event.
fn estimate_event_bytes(event: &BrowserEvent) -> usize {
    let fields = match event {
        BrowserEvent::Navigation { url, title, .. } => {
            url.len() + title.as_ref().map_or(0, String::len)
        }
        BrowserEvent::NetworkRequest {
            request_id,
            method,
            url,
            ..
        } => request_id.len() + method.len() + url.len(),
        BrowserEvent::NetworkResponse { request_id, .. } => request_id.len(),
        BrowserEvent::ConsoleEntry { level, message, .. }
        | BrowserEvent::DriverLog { level, message } => level.len() + message.len(),
        BrowserEvent::ScriptMessage {
            channel, payload, ..
        } => channel.len() + payload_bytes(payload),
    };
    fields + ENVELOPE_OVERHEAD_BYTES
}

fn payload_bytes(payload: &serde_json::Value) -> usize {
    serde_json::to_vec(payload).map_or(usize::MAX, |bytes| bytes.len())
}

fn truncate_utf8(value: &mut String, max: usize) {
    if value.len() <= max {
        return;
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_browser::ids::{BrowserContextId, BrowserSessionId, EventId};

    fn envelope(cursor: u64, class: EventClass, message: String) -> EventEnvelope {
        let mut next = BrowserEventCursor::zero();
        for _ in 0..cursor {
            next = next.next();
        }
        EventEnvelope::new(
            EventId::new(),
            next,
            BrowserSessionId::new(),
            class,
            BrowserEvent::ConsoleEntry {
                context_id: BrowserContextId::new(),
                level: "info".to_owned(),
                message,
            },
        )
    }

    #[test]
    fn test_event_journal_policy_bounded_rejects_zero_and_oversize() {
        assert!(EventJournalPolicy::bounded(0).is_err());
        assert!(EventJournalPolicy::bounded(HARD_MAX_JOURNAL_CAPACITY + 1).is_err());
        let policy = EventJournalPolicy::bounded(8).expect("valid capacity");
        assert_eq!(policy.max_bytes(), DEFAULT_JOURNAL_MAX_BYTES);
        assert!(policy.with_max_bytes(0).is_err());
        assert!(policy.with_max_bytes(HARD_MAX_JOURNAL_BYTES + 1).is_err());
        assert_eq!(policy.with_max_bytes(4_096).expect("valid bytes").max_bytes(), 4_096);
    }

    #[test]
    fn test_event_journal_push_caps_entries_for_critical_flood() {
        let policy = EventJournalPolicy::bounded(4).expect("valid capacity");
        let mut journal = EventJournal::new(policy);
        for cursor in 1..=50 {
            journal.push(envelope(cursor, EventClass::Critical, "c".to_owned()));
        }
        assert_eq!(journal.len(), 4);
        assert_eq!(journal.stats().dropped, 46);
        assert_eq!(journal.last_cursor().map(|c| c.value()), Some(50));
        assert_eq!(journal.since(BrowserEventCursor::zero()).len(), 4);
    }

    #[test]
    fn test_event_journal_push_caps_retained_bytes() {
        let policy = EventJournalPolicy::bounded(1_000)
            .and_then(|policy| policy.with_max_bytes(4 * 1024))
            .expect("valid policy");
        let mut journal = EventJournal::new(policy);
        for cursor in 1..=100 {
            journal.push(envelope(cursor, EventClass::ConsoleRepetition, "x".repeat(500)));
        }
        assert!(journal.retained_bytes() <= 4 * 1024);
        assert!(journal.len() < 100);
        assert!(journal.stats().deduplicated > 0);
    }

    #[test]
    fn test_event_journal_push_truncates_oversized_fields() {
        let policy = EventJournalPolicy::bounded(4).expect("valid capacity");
        let mut journal = EventJournal::new(policy);
        journal.push(envelope(1, EventClass::Critical, "é".repeat(MAX_EVENT_FIELD_BYTES)));
        let events = journal.since(BrowserEventCursor::zero());
        match &events[0].event {
            BrowserEvent::ConsoleEntry { message, .. } => {
                assert!(message.len() <= MAX_EVENT_FIELD_BYTES);
                assert!(message.chars().all(|c| c == 'é'));
            }
            other => panic!("unexpected event {other:?}"),
        }
        assert!(journal.retained_bytes() <= MAX_EVENT_FIELD_BYTES + 256);
    }

    #[test]
    fn test_event_journal_push_critical_evicts_non_critical_first() {
        let policy = EventJournalPolicy::bounded(2).expect("valid capacity");
        let mut journal = EventJournal::new(policy);
        journal.push(envelope(1, EventClass::Critical, "keep".to_owned()));
        journal.push(envelope(2, EventClass::RequestLifecycle, "evict".to_owned()));
        journal.push(envelope(3, EventClass::Critical, "new".to_owned()));
        let cursors: Vec<u64> = journal
            .since(BrowserEventCursor::zero())
            .iter()
            .map(|event| event.cursor.value())
            .collect();
        assert_eq!(cursors, vec![1, 3]);
    }
}
