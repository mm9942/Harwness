//! # event
//!
//! ## Responsibility
//! This module owns the **async event stream** vocabulary: the individual
//! event payloads a backend may emit ([`BrowserEvent`]), their
//! classification for backpressure purposes ([`EventClass`],
//! [`NetworkClass`]), the envelope that timestamps and orders them
//! ([`EventEnvelope`]), and counters for events a backend summarized or
//! dropped instead of delivering verbatim ([`BackpressureStats`]). It does
//! not own the transport (polling vs. push, channel implementation) used to
//! deliver events — see [`crate::host::BrowserRuntime::events`] and
//! [`crate::session::BrowserSessionHandle::events`] for that.
//!
//! ## Key types exported
//! - [`EventClass`] — the backpressure class of an event (critical events
//!   are never dropped; others may be aggregated/deduplicated/dropped).
//! - [`NetworkClass`] — the semantic class of a network request/response.
//! - [`BrowserEvent`] — the payload of one emitted event (navigation,
//!   network, console, script message, or driver log).
//! - [`EventEnvelope`] — a [`BrowserEvent`] with its id, cursor position,
//!   owning session, timestamp, and backpressure class.
//! - [`BackpressureStats`] — counts of events summarized/dropped rather than
//!   delivered verbatim.
//!
//! ## Concurrency
//! Single-threaded, pure data types; `Send + Sync` follows automatically from
//! the fields (`serde_json::Value` and `time::OffsetDateTime` are both
//! `Send + Sync`). No locking or shared state is introduced here.
//!
//! ## Errors
//! This module defines no fallible operations itself; failures retrieving
//! events are reported by the backend as [`crate::error::Error`] from
//! [`crate::host::BrowserRuntime::events`].
//!
//! ## Examples
//! ```rust,no_run
//! use harw_browser::event::{BrowserEvent, EventClass, EventEnvelope};
//! use harw_browser::ids::{BrowserEventCursor, BrowserSessionId, EventId};
//!
//! let envelope = EventEnvelope::new(
//!     EventId::new(),
//!     BrowserEventCursor::zero(),
//!     BrowserSessionId::new(),
//!     EventClass::Critical,
//!     BrowserEvent::DriverLog {
//!         level: "info".to_owned(),
//!         message: "driver started".to_owned(),
//!     },
//! );
//! assert_eq!(envelope.class, EventClass::Critical);
//! ```

// Spec source: CONTRACT_harw_browser.md, section `event.rs`.

/// The backpressure class of an emitted event.
///
/// # Description
/// Used by a backend's event pipeline to decide what to do under load:
/// `Critical` events must always be delivered verbatim, while the other
/// classes may be aggregated, deduplicated, or dropped (see
/// [`BackpressureStats`]).
///
/// # Concurrency
/// `Copy`, plain data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EventClass {
    Critical,
    RequestLifecycle,
    ConsoleRepetition,
    StaticAsset,
    Artifact,
}

/// The semantic class of a network request or response event.
///
/// # Concurrency
/// `Copy`, plain data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NetworkClass {
    Navigation,
    XhrFetch,
    StaticAsset,
    Prefetch,
    Telemetry,
    BackgroundSync,
    ServiceWorker,
    AuthTokenRefresh,
    MessageIngress,
    MessageEgress,
    Unknown,
}

/// The payload of a single event emitted by a browser backend.
///
/// # Description
/// Each variant carries exactly the data relevant to that kind of event:
/// navigation completions, network request/response pairs (correlated via
/// `request_id`), console entries, script messages posted from page content
/// (carrying an arbitrary `serde_json::Value` payload), and driver-level log
/// lines that are not tied to any browsing context.
///
/// # Concurrency
/// `Send + Sync` (all fields are `Send + Sync`); cannot derive `Eq` because
/// `serde_json::Value` implements only `PartialEq`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum BrowserEvent {
    Navigation {
        context_id: crate::ids::BrowserContextId,
        url: String,
        title: Option<String>,
    },
    NetworkRequest {
        context_id: crate::ids::BrowserContextId,
        request_id: String,
        method: String,
        url: String,
        class: NetworkClass,
    },
    NetworkResponse {
        context_id: crate::ids::BrowserContextId,
        request_id: String,
        status: u16,
        class: NetworkClass,
    },
    ConsoleEntry {
        context_id: crate::ids::BrowserContextId,
        level: String,
        message: String,
    },
    ScriptMessage {
        context_id: crate::ids::BrowserContextId,
        channel: String,
        payload: serde_json::Value,
    },
    DriverLog {
        level: String,
        message: String,
    },
}

/// A [`BrowserEvent`] with its identity, ordering cursor, owning session,
/// timestamp, and backpressure class.
///
/// # Description
/// `cursor` establishes a total order over events within a session, letting
/// [`crate::host::BrowserRuntime::events`] be called repeatedly with
/// `since` set to the last-seen cursor to resume the stream without gaps or
/// duplicates.
///
/// # Concurrency
/// `Send + Sync` (all fields are `Send + Sync`); cannot derive `Eq` because
/// `BrowserEvent` cannot derive `Eq` and `time::OffsetDateTime` implements
/// only `PartialEq`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EventEnvelope {
    pub id: crate::ids::EventId,
    pub cursor: crate::ids::BrowserEventCursor,
    pub session_id: crate::ids::BrowserSessionId,
    pub timestamp: time::OffsetDateTime,
    pub class: EventClass,
    pub event: BrowserEvent,
}

impl EventEnvelope {
    /// Builds a new envelope, stamping `timestamp` with the current UTC time.
    ///
    /// # Arguments
    /// - `id` (`crate::ids::EventId`): the unique identifier for this event.
    ///   Owned (`Copy`).
    /// - `cursor` (`crate::ids::BrowserEventCursor`): this event's position in
    ///   the session's event stream. Owned (`Copy`).
    /// - `session_id` (`crate::ids::BrowserSessionId`): the session that
    ///   produced the event. Owned (`Copy`).
    /// - `class` (`EventClass`): the backpressure class of the event. Owned
    ///   (`Copy`).
    /// - `event` (`BrowserEvent`): the event payload. Ownership is
    ///   transferred into the envelope.
    ///
    /// # Returns
    /// `EventEnvelope` — with `timestamp` set to `time::OffsetDateTime::now_utc()`
    /// at the moment of the call.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::event::{BrowserEvent, EventClass, EventEnvelope};
    /// use harw_browser::ids::{BrowserEventCursor, BrowserSessionId, EventId};
    ///
    /// let envelope = EventEnvelope::new(
    ///     EventId::new(),
    ///     BrowserEventCursor::zero(),
    ///     BrowserSessionId::new(),
    ///     EventClass::StaticAsset,
    ///     BrowserEvent::DriverLog { level: "debug".to_owned(), message: "tick".to_owned() },
    /// );
    /// assert!(envelope.timestamp.unix_timestamp() > 0);
    /// ```
    pub fn new(
        id: crate::ids::EventId,
        cursor: crate::ids::BrowserEventCursor,
        session_id: crate::ids::BrowserSessionId,
        class: EventClass,
        event: BrowserEvent,
    ) -> Self {
        Self {
            id,
            cursor,
            session_id,
            timestamp: time::OffsetDateTime::now_utc(),
            class,
            event,
        }
    }
}

/// Counts of events that were summarized or dropped instead of delivered
/// verbatim, per plan.md 17.3.
///
/// # Description
/// A backend's event pipeline increments these counters when it applies
/// backpressure to non-[`EventClass::Critical`] events, so callers can
/// observe how much of the stream was elided rather than silently losing
/// events with no visibility.
///
/// # Concurrency
/// `Copy`, plain data; a caller sharing one `BackpressureStats` across
/// threads must provide its own synchronization (e.g. behind a `Mutex`) since
/// this type has no interior mutability of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct BackpressureStats {
    pub dropped: u64,
    pub aggregated: u64,
    pub deduplicated: u64,
}

impl BackpressureStats {
    /// Increments `dropped` by one.
    ///
    /// # Panics
    /// Never (wraps only at `u64::MAX`, which is not reachable in practice).
    pub fn record_drop(&mut self) {
        self.dropped += 1;
    }

    /// Increments `aggregated` by one.
    ///
    /// # Panics
    /// Never (wraps only at `u64::MAX`, which is not reachable in practice).
    pub fn record_aggregate(&mut self) {
        self.aggregated += 1;
    }

    /// Increments `deduplicated` by one.
    ///
    /// # Panics
    /// Never (wraps only at `u64::MAX`, which is not reachable in practice).
    pub fn record_deduplicate(&mut self) {
        self.deduplicated += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backpressure_stats_record_each_increments_once() {
        let mut stats = BackpressureStats::default();
        assert_eq!(
            stats,
            BackpressureStats {
                dropped: 0,
                aggregated: 0,
                deduplicated: 0
            }
        );

        stats.record_drop();
        stats.record_aggregate();
        stats.record_deduplicate();

        assert_eq!(stats.dropped, 1);
        assert_eq!(stats.aggregated, 1);
        assert_eq!(stats.deduplicated, 1);
    }

    #[test]
    fn test_event_envelope_new_sets_fields_and_stamps_timestamp() {
        let id = crate::ids::EventId::new();
        let cursor = crate::ids::BrowserEventCursor::zero();
        let session_id = crate::ids::BrowserSessionId::new();
        let context_id = crate::ids::BrowserContextId::new();
        let event = BrowserEvent::DriverLog {
            level: "info".to_owned(),
            message: "driver started".to_owned(),
        };

        let envelope =
            EventEnvelope::new(id, cursor, session_id, EventClass::Critical, event.clone());

        assert_eq!(envelope.id, id);
        assert_eq!(envelope.cursor, cursor);
        assert_eq!(envelope.session_id, session_id);
        assert_eq!(envelope.class, EventClass::Critical);
        assert_eq!(envelope.event, event);
        assert!(envelope.timestamp.unix_timestamp() > 0);

        // context_id is exercised through a Navigation event below; keep it used here too.
        let _ = context_id;
    }

    #[test]
    fn test_browser_event_variants_construct_with_realistic_fields() {
        let context_id = crate::ids::BrowserContextId::new();

        let navigation = BrowserEvent::Navigation {
            context_id,
            url: "https://example.com/".to_owned(),
            title: Some("Example Domain".to_owned()),
        };
        assert!(matches!(navigation, BrowserEvent::Navigation { .. }));

        let network_request = BrowserEvent::NetworkRequest {
            context_id,
            request_id: "req-1".to_owned(),
            method: "GET".to_owned(),
            url: "https://example.com/api".to_owned(),
            class: NetworkClass::XhrFetch,
        };
        assert!(matches!(
            network_request,
            BrowserEvent::NetworkRequest { .. }
        ));

        let network_response = BrowserEvent::NetworkResponse {
            context_id,
            request_id: "req-1".to_owned(),
            status: 200,
            class: NetworkClass::XhrFetch,
        };
        assert!(matches!(
            network_response,
            BrowserEvent::NetworkResponse { .. }
        ));

        let console_entry = BrowserEvent::ConsoleEntry {
            context_id,
            level: "warn".to_owned(),
            message: "deprecated API used".to_owned(),
        };
        assert!(matches!(console_entry, BrowserEvent::ConsoleEntry { .. }));

        let script_message = BrowserEvent::ScriptMessage {
            context_id,
            channel: "harwness".to_owned(),
            payload: serde_json::json!({ "ready": true }),
        };
        assert!(matches!(script_message, BrowserEvent::ScriptMessage { .. }));

        let driver_log = BrowserEvent::DriverLog {
            level: "error".to_owned(),
            message: "session crashed".to_owned(),
        };
        assert!(matches!(driver_log, BrowserEvent::DriverLog { .. }));
    }

    #[test]
    fn test_event_envelope_serde_json_round_trip_with_script_message() {
        // ScriptMessage carries a `serde_json::Value` payload and the envelope carries
        // a `time::OffsetDateTime` timestamp; both are protocol-boundary types that must
        // survive a JSON round trip unchanged.
        let envelope = EventEnvelope::new(
            crate::ids::EventId::new(),
            crate::ids::BrowserEventCursor::zero(),
            crate::ids::BrowserSessionId::new(),
            EventClass::Artifact,
            BrowserEvent::ScriptMessage {
                context_id: crate::ids::BrowserContextId::new(),
                channel: "harwness".to_owned(),
                payload: serde_json::json!({ "ready": true, "count": 3 }),
            },
        );

        let json = serde_json::to_string(&envelope).expect("envelope serializes");
        let decoded: EventEnvelope = serde_json::from_str(&json).expect("envelope deserializes");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn test_backpressure_stats_serde_json_round_trip() {
        let mut stats = BackpressureStats::default();
        stats.record_drop();
        stats.record_aggregate();

        let json = serde_json::to_string(&stats).expect("stats serialize");
        let decoded: BackpressureStats = serde_json::from_str(&json).expect("stats deserialize");
        assert_eq!(decoded, stats);
    }
}
