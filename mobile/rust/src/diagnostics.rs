//! Process-local diagnostics with explicit export. Never record original identifiers or payloads.

use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use harw_protocol::TurnEvent;
use harw_types::TurnId;

pub const MAX_EVENTS: usize = 256;
pub const MAX_EXPORT_BYTES: usize = 64 * 1024;
pub const MAX_TRACE_DURATION: Duration = Duration::from_secs(10 * 60);

const EXPORT_PREFIX: &str = "harw-diagnostics-v1 dropped=";
// Reserve space for a full u64 drop count even when it currently has fewer digits.
const EXPORT_HEADER_MAX_BYTES: usize = EXPORT_PREFIX.len() + 20 + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Domain {
    Chat,
    Stream,
    Onion,
}

impl Domain {
    const fn index(self) -> usize {
        match self {
            Self::Chat => 0,
            Self::Stream => 1,
            Self::Onion => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Severity {
    const fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

/// Trace is intentionally not a persistent level; use `enable_trace`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Level {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum BaseLevel {
    Off,
    Error,
    #[default]
    Warn,
    Info,
    Debug,
}

impl From<BaseLevel> for Level {
    fn from(base: BaseLevel) -> Self {
        match base {
            BaseLevel::Off => Self::Off,
            BaseLevel::Error => Self::Error,
            BaseLevel::Warn => Self::Warn,
            BaseLevel::Info => Self::Info,
            BaseLevel::Debug => Self::Debug,
        }
    }
}

impl Level {
    fn accepts(self, severity: Severity) -> bool {
        let maximum = match self {
            Self::Off => return false,
            Self::Error => Severity::Error,
            Self::Warn => Severity::Warn,
            Self::Info => Severity::Info,
            Self::Debug => Severity::Debug,
            Self::Trace => Severity::Trace,
        };
        severity <= maximum
    }
}

/// Counter allocated by this process, not a hash or encoding of a session/turn ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnAlias(u64);

impl TurnAlias {
    pub const fn from_counter(counter: u64) -> Self {
        Self(counter)
    }
}

/// Counter allocated by this process, not a remote or device identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionEpoch(u64);

impl ConnectionEpoch {
    pub const fn from_counter(counter: u64) -> Self {
        Self(counter)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderOutcome {
    Unknown,
    Completed,
    Length,
    Policy,
    Interrupted,
    Failed,
}

impl ProviderOutcome {
    const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Completed => "completed",
            Self::Length => "length",
            Self::Policy => "policy",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatEvent {
    TurnStarted { turn: TurnAlias, attempt: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamEvent {
    Attached {
        attachment: AttachmentEpoch,
    },
    AttachmentEnded {
        attachment: AttachmentEpoch,
    },
    Signal {
        attachment: AttachmentEpoch,
        signal: StreamSignal,
    },
    /// Counts only contiguous, accepted live events on this attachment.
    ObservedTurnEnd {
        attachment: AttachmentEpoch,
        local_turn: TurnAlias,
        outcome: WireTurnOutcome,
        answer_chunks: u64,
        answer_utf8_bytes: u64,
        reasoning_chunks: u64,
    },
    Finished {
        turn: TurnAlias,
        attempt: u32,
        outcome: ProviderOutcome,
        accepted: u64,
        replayed: u64,
        retained_visible_bytes: u64,
        durable_visible_bytes: Option<u64>,
        live_tail_capped: bool,
    },
}

/// Terminal evidence from the session wire, not an inferred provider reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireTurnOutcome {
    Completed,
    Aborted,
    Failed { retryable: bool },
}

impl WireTurnOutcome {
    const fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Aborted => "aborted",
            Self::Failed { retryable: true } => "failed_retryable",
            Self::Failed { retryable: false } => "failed_not_retryable",
        }
    }
}

/// Closed vocabulary: never put a provider message, cursor or identifier in logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamSignal {
    TurnStarted,
    TurnCompleted,
    TurnAborted,
    TurnFailed,
    AnswerDelta,
    ReasoningDelta,
    Reattach,
    Refresh,
    EndOfStream,
    ConnectionError,
    ProtocolError,
    Revoked,
}

impl StreamSignal {
    const fn label(self) -> &'static str {
        match self {
            Self::TurnStarted => "turn_started",
            Self::TurnCompleted => "turn_completed",
            Self::TurnAborted => "turn_aborted",
            Self::TurnFailed => "turn_failed",
            Self::AnswerDelta => "answer_delta",
            Self::ReasoningDelta => "reasoning_delta",
            Self::Reattach => "reattach",
            Self::Refresh => "refresh",
            Self::EndOfStream => "end_of_stream",
            Self::ConnectionError => "connection_error",
            Self::ProtocolError => "protocol_error",
            Self::Revoked => "revoked",
        }
    }

    const fn severity(self) -> Severity {
        match self {
            Self::TurnFailed | Self::ProtocolError => Severity::Error,
            Self::EndOfStream | Self::ConnectionError | Self::Revoked => Severity::Warn,
            Self::TurnStarted | Self::TurnCompleted | Self::TurnAborted => Severity::Info,
            Self::Reattach | Self::Refresh => Severity::Debug,
            Self::AnswerDelta | Self::ReasoningDelta => Severity::Trace,
        }
    }

    const fn domain(self) -> Domain {
        match self {
            Self::TurnStarted | Self::TurnCompleted | Self::TurnAborted | Self::TurnFailed => Domain::Chat,
            _ => Domain::Stream,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnionEvent {
    Connecting {
        connection: ConnectionEpoch,
    },
    Connected {
        connection: ConnectionEpoch,
        elapsed_ms: u64,
    },
    Disconnected {
        connection: ConnectionEpoch,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticEvent {
    Chat(ChatEvent),
    Stream(StreamEvent),
    Onion(OnionEvent),
}

impl DiagnosticEvent {
    const fn domain(self) -> Domain {
        match self {
            Self::Chat(_) => Domain::Chat,
            Self::Stream(StreamEvent::Signal { signal, .. }) => signal.domain(),
            Self::Stream(_) => Domain::Stream,
            Self::Onion(_) => Domain::Onion,
        }
    }

    fn fields(self) -> String {
        match self {
            Self::Chat(ChatEvent::TurnStarted { turn, attempt }) => {
                format!(
                    "domain=chat event=turn_started turn={} attempt={attempt}",
                    turn.0
                )
            }
            Self::Stream(StreamEvent::Attached { attachment }) => {
                format!("domain=stream event=attached attachment={}", attachment.0)
            }
            Self::Stream(StreamEvent::AttachmentEnded { attachment }) => {
                format!(
                    "domain=stream event=attachment_ended attachment={}",
                    attachment.0
                )
            }
            Self::Stream(StreamEvent::Signal { attachment, signal }) => {
                let domain = if signal.domain() == Domain::Chat { "chat" } else { "stream" };
                format!("domain={domain} event={} attachment={}", signal.label(), attachment.0)
            }
            Self::Stream(StreamEvent::ObservedTurnEnd {
                attachment, local_turn, outcome, answer_chunks, answer_utf8_bytes, reasoning_chunks,
            }) => {
                format!(
                    "domain=stream event=observed_turn_end attachment={} turn={} outcome={} answer_chunks={answer_chunks} answer_utf8_bytes={answer_utf8_bytes} reasoning_chunks={reasoning_chunks}",
                    attachment.0, local_turn.0, outcome.label(),
                )
            }
            Self::Stream(StreamEvent::Finished {
                turn,
                attempt,
                outcome,
                accepted,
                replayed,
                retained_visible_bytes,
                durable_visible_bytes,
                live_tail_capped,
            }) => {
                let durable =
                    durable_visible_bytes.map_or_else(|| "unknown".into(), |n| n.to_string());
                format!(
                    "domain=stream event=finished turn={} attempt={attempt} outcome={} accepted={accepted} replayed={replayed} retained_visible_bytes={retained_visible_bytes} durable_visible_bytes={durable} live_tail_capped={live_tail_capped}",
                    turn.0,
                    outcome.label(),
                )
            }
            Self::Onion(OnionEvent::Connecting { connection }) => {
                format!("domain=onion event=connecting connection={}", connection.0)
            }
            Self::Onion(OnionEvent::Connected {
                connection,
                elapsed_ms,
            }) => {
                format!(
                    "domain=onion event=connected connection={} elapsed_ms={elapsed_ms}",
                    connection.0
                )
            }
            Self::Onion(OnionEvent::Disconnected { connection }) => {
                format!(
                    "domain=onion event=disconnected connection={}",
                    connection.0
                )
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordResult {
    Filtered,
    Stored(u64),
    Dropped,
}

#[derive(Clone, Copy, Debug, Default)]
struct DomainSetting {
    base: BaseLevel,
    trace_until: Option<Duration>,
}

struct Entry {
    number: u64,
    elapsed: Duration,
    severity: Severity,
    event: DiagnosticEvent,
    bytes: usize,
}

impl Entry {
    fn serialize(&self) -> String {
        format!(
            "n={} t_ms={} severity={} {}\n",
            self.number,
            self.elapsed.as_millis(),
            self.severity.label(),
            self.event.fields(),
        )
    }
}

/// A bounded, RAM-only collector. The owner supplies elapsed time from one
/// process-local monotonic origin (for example, `Instant::elapsed`).
/// Synchronization and the explicit policy for when to reveal/share an export
/// belong to its owner; this type never writes files, logs, or emits I/O.
pub struct DiagnosticRing {
    entries: VecDeque<Entry>,
    max_events: usize,
    max_event_bytes: usize,
    event_bytes: usize,
    dropped: u64,
    last_number: u64,
    last_elapsed: Duration,
    settings: [DomainSetting; 3],
}

impl Default for DiagnosticRing {
    fn default() -> Self {
        Self::with_limits(MAX_EVENTS, MAX_EXPORT_BYTES)
    }
}

impl DiagnosticRing {
    /// Bounds are clamped to the product caps. The minimum export budget reserves
    /// the header alone; zero event capacity is allowed.
    pub fn with_limits(events: usize, export_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max_events: events.min(MAX_EVENTS),
            max_event_bytes: export_bytes.clamp(EXPORT_HEADER_MAX_BYTES, MAX_EXPORT_BYTES)
                - EXPORT_HEADER_MAX_BYTES,
            event_bytes: 0,
            dropped: 0,
            last_number: 0,
            last_elapsed: Duration::ZERO,
            settings: [DomainSetting::default(); 3],
        }
    }

    pub fn set_level(&mut self, domain: Domain, level: BaseLevel) {
        self.settings[domain.index()] = DomainSetting {
            base: level,
            trace_until: None,
        };
    }

    pub fn enable_trace(&mut self, domain: Domain, elapsed: Duration, duration: Duration) {
        let now = self.advance_clock(elapsed);
        let deadline = now.saturating_add(duration.min(MAX_TRACE_DURATION));
        self.settings[domain.index()].trace_until = (deadline > now).then_some(deadline);
    }

    pub fn level(&mut self, domain: Domain, elapsed: Duration) -> Level {
        let now = self.advance_clock(elapsed);
        let setting = &mut self.settings[domain.index()];
        if setting.trace_until.is_some_and(|until| now < until) {
            return Level::Trace;
        }
        setting.trace_until = None;
        setting.base.into()
    }

    fn advance_clock(&mut self, elapsed: Duration) -> Duration {
        self.last_elapsed = self.last_elapsed.max(elapsed);
        self.last_elapsed
    }

    pub fn record(
        &mut self,
        elapsed: Duration,
        severity: Severity,
        event: DiagnosticEvent,
    ) -> RecordResult {
        if !self.level(event.domain(), elapsed).accepts(severity) {
            return RecordResult::Filtered;
        }
        let Some(number) = self.last_number.checked_add(1) else {
            self.count_drop();
            return RecordResult::Dropped;
        };
        self.last_number = number;
        let mut entry = Entry {
            number,
            elapsed: self.last_elapsed,
            severity,
            event,
            bytes: 0,
        };
        entry.bytes = entry.serialize().len();
        if self.max_events == 0 || entry.bytes > self.max_event_bytes {
            self.count_drop();
            return RecordResult::Dropped;
        }
        while self.entries.len() >= self.max_events
            || self.event_bytes + entry.bytes > self.max_event_bytes
        {
            let Some(evicted) = self.entries.pop_front() else {
                break;
            };
            self.event_bytes -= evicted.bytes;
            self.count_drop();
        }
        self.event_bytes += entry.bytes;
        self.entries.push_back(entry);
        RecordResult::Stored(number)
    }

    fn count_drop(&mut self) {
        self.dropped = self.dropped.saturating_add(1);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Remove retained events and reset the drop count, without reusing event
    /// numbers or changing the user's selected levels.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.event_bytes = 0;
        self.dropped = 0;
    }

    /// The same bounded versioned text serves preview and explicit export.
    pub fn export_text(&self) -> String {
        let mut text = String::with_capacity(EXPORT_HEADER_MAX_BYTES + self.event_bytes);
        text.push_str(&format!("{EXPORT_PREFIX}{}\n", self.dropped));
        for entry in &self.entries {
            text.push_str(&entry.serialize());
        }
        text
    }
}

/// Process-local attachment ordinal, never the host's session ID or cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttachmentEpoch(u64);

/// Generated labels and numbers only. This is an explicit snapshot, not a log sink.
#[derive(uniffi::Record)]
pub struct DiagnosticSnapshot {
    pub text: String,
    pub retained: u32,
    pub dropped: u64,
}

#[derive(Default)]
struct DiagnosticState {
    ring: DiagnosticRing,
    last_scope: u64,
    summary_barrier: u64,
}

/// One owner for the lifetime of a mobile core. Never persists or emits to logging.
pub(crate) struct DiagnosticLog {
    origin: Instant,
    state: Mutex<DiagnosticState>,
}

impl Default for DiagnosticLog {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
            state: Mutex::default(),
        }
    }
}

impl DiagnosticLog {
    fn state(&self) -> MutexGuard<'_, DiagnosticState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn snapshot(&self) -> DiagnosticSnapshot {
        let state = self.state();
        DiagnosticSnapshot {
            text: state.ring.export_text(),
            // The ring is bounded to MAX_EVENTS, far below u32::MAX.
            retained: state.ring.len() as u32,
            dropped: state.ring.dropped(),
        }
    }

    pub(crate) fn clear(&self) {
        let mut state = self.state();
        state.ring.clear();
        state.summary_barrier = state.last_scope;
    }

    pub(crate) fn level(&self, domain: Domain) -> Level {
        self.state().ring.level(domain, self.origin.elapsed())
    }

    pub(crate) fn set_level(&self, domain: Domain, level: BaseLevel) {
        let mut state = self.state();
        state.ring.set_level(domain, level);
        if domain == Domain::Stream && level == BaseLevel::Off {
            state.summary_barrier = state.last_scope;
        }
    }

    pub(crate) fn enable_trace(&self, domain: Domain) {
        self.state()
            .ring
            .enable_trace(domain, self.origin.elapsed(), MAX_TRACE_DURATION);
    }

    fn next_scope(&self) -> Option<u64> {
        let mut state = self.state();
        let next = state.last_scope.checked_add(1)?;
        state.last_scope = next;
        Some(next)
    }

    fn begin_turn_alias(&self) -> Option<TurnAlias> {
        let mut state = self.state();
        if state.ring.level(Domain::Stream, self.origin.elapsed()) == Level::Off {
            return None;
        }
        let next = state.last_scope.checked_add(1)?;
        state.last_scope = next;
        Some(TurnAlias::from_counter(next))
    }

    fn info(&self, event: DiagnosticEvent) {
        self.state()
            .ring
            .record(self.origin.elapsed(), Severity::Info, event);
    }

    pub(crate) fn begin_onion(&self) -> Option<OnionDiagnostic<'_>> {
        let connection = ConnectionEpoch::from_counter(self.next_scope()?);
        let started = self.origin.elapsed();
        self.info(DiagnosticEvent::Onion(OnionEvent::Connecting {
            connection,
        }));
        Some(OnionDiagnostic {
            log: self,
            connection,
            started,
            connected: false,
        })
    }

    pub(crate) fn begin_attachment(&self) -> Option<AttachmentDiagnostic<'_>> {
        let attachment = AttachmentEpoch(self.next_scope()?);
        self.info(DiagnosticEvent::Stream(StreamEvent::Attached {
            attachment,
        }));
        Some(AttachmentDiagnostic {
            log: self,
            attachment,
            observed_turn: None,
        })
    }
}

/// Records only observed route lifetime; a failed/cancelled dial is not a disconnect.
pub(crate) struct OnionDiagnostic<'a> {
    log: &'a DiagnosticLog,
    connection: ConnectionEpoch,
    started: Duration,
    connected: bool,
}

impl OnionDiagnostic<'_> {
    pub(crate) fn connected(&mut self) {
        if self.connected {
            return;
        }
        self.connected = true;
        let elapsed = self.log.origin.elapsed().saturating_sub(self.started);
        self.log.info(DiagnosticEvent::Onion(OnionEvent::Connected {
            connection: self.connection,
            elapsed_ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
        }));
    }
}

impl Drop for OnionDiagnostic<'_> {
    fn drop(&mut self) {
        if self.connected {
            self.log
                .info(DiagnosticEvent::Onion(OnionEvent::Disconnected {
                    connection: self.connection,
                }));
        }
    }
}

/// Tracks only the observed attachment lifecycle, without guessing why it ended.
pub(crate) struct AttachmentDiagnostic<'a> {
    log: &'a DiagnosticLog,
    attachment: AttachmentEpoch,
    observed_turn: Option<ObservedTurn>,
}

struct ObservedTurn {
    // Only used for in-memory equality; never sent to the ring or formatted.
    wire_id: TurnId,
    alias: TurnAlias,
    answer_chunks: u64,
    answer_utf8_bytes: u64,
    reasoning_chunks: u64,
}

impl AttachmentDiagnostic<'_> {
    pub(crate) fn observe_turn(&mut self, event: &TurnEvent) {
        let (wire_id, outcome) = match event {
            TurnEvent::TurnStarted { turn_id, .. } => {
                if self.observed_turn.as_ref().is_some_and(|turn| &turn.wire_id == turn_id) {
                    return;
                }
                self.observed_turn = self.log.begin_turn_alias().map(|alias| ObservedTurn {
                    wire_id: turn_id.clone(),
                    alias,
                    answer_chunks: 0,
                    answer_utf8_bytes: 0,
                    reasoning_chunks: 0,
                });
                return;
            }
            TurnEvent::AssistantDelta { turn_id, text } => {
                if let Some(turn) = self.observed_turn.as_mut().filter(|turn| &turn.wire_id == turn_id) {
                    turn.answer_chunks = turn.answer_chunks.saturating_add(1);
                    turn.answer_utf8_bytes = turn.answer_utf8_bytes
                        .saturating_add(u64::try_from(text.len()).unwrap_or(u64::MAX));
                }
                return;
            }
            TurnEvent::ReasoningDelta { turn_id, .. } => {
                if let Some(turn) = self.observed_turn.as_mut().filter(|turn| &turn.wire_id == turn_id) {
                    turn.reasoning_chunks = turn.reasoning_chunks.saturating_add(1);
                }
                return;
            }
            TurnEvent::TurnCompleted { turn_id, .. } => (turn_id, WireTurnOutcome::Completed),
            TurnEvent::TurnAborted { turn_id } => (turn_id, WireTurnOutcome::Aborted),
            TurnEvent::TurnFailed { turn_id, retryable, .. } => {
                (turn_id, WireTurnOutcome::Failed { retryable: *retryable })
            }
            _ => return,
        };
        if !self.observed_turn.as_ref().is_some_and(|turn| &turn.wire_id == wire_id) {
            return;
        }
        let Some(turn) = self.observed_turn.take() else { return };
        let severity = if matches!(outcome, WireTurnOutcome::Failed { .. }) {
            Severity::Error
        } else {
            Severity::Info
        };
        let mut state = self.log.state();
        if turn.alias.0 <= state.summary_barrier {
            return;
        }
        state.ring.record(
            self.log.origin.elapsed(),
            severity,
            DiagnosticEvent::Stream(StreamEvent::ObservedTurnEnd {
                attachment: self.attachment,
                local_turn: turn.alias,
                outcome,
                answer_chunks: turn.answer_chunks,
                answer_utf8_bytes: turn.answer_utf8_bytes,
                reasoning_chunks: turn.reasoning_chunks,
            }),
        );
    }

    pub(crate) fn signal(&self, signal: StreamSignal) {
        self.log.state().ring.record(
            self.log.origin.elapsed(),
            signal.severity(),
            DiagnosticEvent::Stream(StreamEvent::Signal {
                attachment: self.attachment,
                signal,
            }),
        );
    }
}

impl Drop for AttachmentDiagnostic<'_> {
    fn drop(&mut self) {
        self.log
            .info(DiagnosticEvent::Stream(StreamEvent::AttachmentEnded {
                attachment: self.attachment,
            }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn chat() -> DiagnosticEvent {
        DiagnosticEvent::Chat(ChatEvent::TurnStarted {
            turn: TurnAlias::from_counter(1),
            attempt: 2,
        })
    }

    fn onion() -> DiagnosticEvent {
        DiagnosticEvent::Onion(OnionEvent::Connected {
            connection: ConnectionEpoch::from_counter(3),
            elapsed_ms: 5,
        })
    }

    #[test]
    fn defaults_warn_and_filters_each_domain_independently() {
        let mut ring = DiagnosticRing::default();
        assert_eq!(ring.level(Domain::Chat, at(0)), Level::Warn);
        assert_eq!(ring.level(Domain::Onion, at(0)), Level::Warn);
        assert_eq!(ring.level(Domain::Stream, at(0)), Level::Warn);
        assert_eq!(
            ring.record(at(1), Severity::Error, chat()),
            RecordResult::Stored(1)
        );
        assert_eq!(
            ring.record(at(2), Severity::Info, chat()),
            RecordResult::Filtered
        );
        assert_eq!(
            ring.record(at(3), Severity::Warn, chat()),
            RecordResult::Stored(2)
        );
        ring.set_level(Domain::Onion, BaseLevel::Info);
        assert_eq!(
            ring.record(at(4), Severity::Info, onion()),
            RecordResult::Stored(3)
        );
        assert_eq!(
            ring.record(at(5), Severity::Debug, onion()),
            RecordResult::Filtered
        );
        ring.set_level(Domain::Chat, BaseLevel::Off);
        assert_eq!(
            ring.record(at(6), Severity::Error, chat()),
            RecordResult::Filtered
        );
        assert_eq!(ring.dropped(), 0);
    }

    #[test]
    fn default_count_limit_is_256() {
        let mut ring = DiagnosticRing::default();
        for n in 1..=257 {
            assert_eq!(
                ring.record(at(n), Severity::Warn, chat()),
                RecordResult::Stored(n)
            );
        }
        assert_eq!(ring.len(), 256);
        assert_eq!(ring.dropped(), 1);
    }

    #[test]
    fn trace_expires_at_deadline_and_restores_domain_level() {
        let mut ring = DiagnosticRing::default();
        ring.set_level(Domain::Chat, BaseLevel::Info);
        ring.enable_trace(Domain::Chat, at(100), at(10));
        assert_eq!(ring.level(Domain::Chat, at(109)), Level::Trace);
        assert_eq!(ring.level(Domain::Onion, at(109)), Level::Warn);
        assert_eq!(ring.level(Domain::Chat, at(110)), Level::Info);
        assert_eq!(ring.level(Domain::Chat, at(105)), Level::Info);
        assert_eq!(
            ring.record(at(111), Severity::Trace, chat()),
            RecordResult::Filtered
        );
        assert_eq!(
            ring.record(at(112), Severity::Info, chat()),
            RecordResult::Stored(1)
        );
    }

    #[test]
    fn trace_is_capped_and_a_manual_selection_cancels_it() {
        let mut ring = DiagnosticRing::default();
        ring.enable_trace(Domain::Chat, at(0), Duration::MAX);
        assert_eq!(ring.level(Domain::Chat, at(599_999)), Level::Trace);
        assert_eq!(ring.level(Domain::Chat, at(600_000)), Level::Warn);
        ring.enable_trace(Domain::Chat, at(600_001), at(500));
        assert_eq!(ring.level(Domain::Chat, at(600_002)), Level::Trace);
        ring.set_level(Domain::Chat, BaseLevel::Error);
        assert_eq!(ring.level(Domain::Chat, at(600_002)), Level::Error);
    }

    #[test]
    fn evicts_oldest_on_count_limit_and_clear_does_not_reuse_numbers() {
        let mut ring = DiagnosticRing::with_limits(2, 1024);
        ring.set_level(Domain::Chat, BaseLevel::Info);
        for n in 1..=3 {
            assert_eq!(
                ring.record(at(n), Severity::Info, chat()),
                RecordResult::Stored(n)
            );
        }
        assert_eq!(ring.len(), 2);
        assert_eq!(ring.dropped(), 1);
        let export = ring.export_text();
        assert!(!export.lines().any(|line| line.starts_with("n=1 ")));
        assert!(export.lines().any(|line| line.starts_with("n=2 ")));
        assert!(export.lines().any(|line| line.starts_with("n=3 ")));
        ring.clear();
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.dropped(), 0);
        assert_eq!(
            ring.record(at(4), Severity::Info, chat()),
            RecordResult::Stored(4)
        );
    }

    #[test]
    fn exact_byte_bound_includes_header_and_counts_unretained_events() {
        let mut ring = DiagnosticRing::with_limits(9_999, 170);
        ring.set_level(Domain::Chat, BaseLevel::Info);
        for n in 1..=40 {
            ring.record(at(n), Severity::Info, chat());
            assert!(ring.export_text().len() <= 170);
        }
        assert!(ring.len() > 0);
        assert_eq!(ring.len() as u64 + ring.dropped(), 40);

        let mut too_small = DiagnosticRing::with_limits(2, EXPORT_HEADER_MAX_BYTES);
        too_small.set_level(Domain::Chat, BaseLevel::Info);
        assert_eq!(
            too_small.record(at(1), Severity::Info, chat()),
            RecordResult::Dropped
        );
        assert_eq!(too_small.dropped(), 1);
        assert!(too_small.export_text().len() <= EXPORT_HEADER_MAX_BYTES);
    }

    #[test]
    fn wire_exports_only_fixed_labels_and_explicit_numeric_facts() {
        let mut ring = DiagnosticRing::default();
        ring.set_level(Domain::Stream, BaseLevel::Info);
        ring.record(
            at(4),
            Severity::Info,
            DiagnosticEvent::Stream(StreamEvent::Finished {
                turn: TurnAlias::from_counter(7),
                attempt: 2,
                outcome: ProviderOutcome::Unknown,
                accepted: 9,
                replayed: 2,
                retained_visible_bytes: 256,
                durable_visible_bytes: None,
                live_tail_capped: false,
            }),
        );
        assert_eq!(
            ring.export_text(),
            "harw-diagnostics-v1 dropped=0\nn=1 t_ms=4 severity=info domain=stream event=finished turn=7 attempt=2 outcome=unknown accepted=9 replayed=2 retained_visible_bytes=256 durable_visible_bytes=unknown live_tail_capped=false\n"
        );
    }

    #[test]
    fn exhausted_sequence_drops_instead_of_wrapping_or_reusing() {
        let mut ring = DiagnosticRing::default();
        ring.last_number = u64::MAX - 1;
        ring.set_level(Domain::Chat, BaseLevel::Info);
        assert_eq!(
            ring.record(at(1), Severity::Info, chat()),
            RecordResult::Stored(u64::MAX)
        );
        assert_eq!(
            ring.record(at(2), Severity::Info, chat()),
            RecordResult::Dropped
        );
        assert_eq!(ring.dropped(), 1);
    }
}

#[cfg(test)]
mod owner_tests {
    use super::*;

    #[test]
    fn owner_keeps_state_isolated_and_exports_only_explicit_stage_facts() {
        let first = DiagnosticLog::default();
        let second = DiagnosticLog::default();
        first.set_level(Domain::Onion, BaseLevel::Info);
        first.set_level(Domain::Stream, BaseLevel::Info);

        let mut onion = first.begin_onion().expect("first onion attempt");
        onion.connected();
        onion.connected(); // Duplicate notification is not a second connection.
        {
            let _attachment = first.begin_attachment().expect("first attachment");
        }
        drop(onion);
        let snapshot = first.snapshot();
        assert_eq!(snapshot.retained, 5);
        assert_eq!(snapshot.dropped, 0);
        assert!(snapshot.text.contains("event=connecting connection=1\n"));
        assert_eq!(
            snapshot
                .text
                .matches("event=connected connection=1 ")
                .count(),
            1
        );
        assert!(snapshot.text.contains("event=disconnected connection=1\n"));
        assert!(snapshot.text.contains("event=attached attachment=2\n"));
        assert!(
            snapshot
                .text
                .contains("event=attachment_ended attachment=2\n")
        );
        assert!(!snapshot.text.contains("outcome="));

        assert_eq!(second.snapshot().retained, 0);
        first.clear();
        assert_eq!(first.snapshot().retained, 0);
        assert_eq!(first.level(Domain::Stream), Level::Info);
    }

    #[test]
    fn cancelled_dial_is_not_a_disconnect_or_guessed_provider_stop() {
        let log = DiagnosticLog::default();
        log.set_level(Domain::Onion, BaseLevel::Info);
        {
            let _attempt = log.begin_onion().expect("first onion attempt");
        }
        let snapshot = log.snapshot();
        assert_eq!(snapshot.retained, 1);
        assert!(snapshot.text.contains("event=connecting "));
        assert!(!snapshot.text.contains("event=disconnected "));
        assert!(!snapshot.text.contains("outcome="));
    }

    #[test]
    fn explicit_trace_does_not_change_the_base_level() {
        let log = DiagnosticLog::default();
        assert_eq!(log.level(Domain::Chat), Level::Warn);
        log.enable_trace(Domain::Chat);
        assert_eq!(log.level(Domain::Chat), Level::Trace);
        assert_eq!(log.level(Domain::Onion), Level::Warn);
        log.set_level(Domain::Chat, BaseLevel::Warn);
        assert_eq!(log.level(Domain::Chat), Level::Warn);
    }
}
