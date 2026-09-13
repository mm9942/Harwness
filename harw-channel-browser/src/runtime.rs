use std::fmt;

use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::bridge::BridgeMessage;
use crate::delivery::{DeliveryEvidence, PendingDelivery};
use crate::lease::{ConnectorLease, LeaseToken, LeaseValidationError};
use crate::message::{ChannelMessage, MessageNormalizer, RawChannelMessage};
use crate::takeover::{AutonomousSend, TakeoverState};

/// Boundary used by the runtime to wake the owning Harwness execution.
pub trait WakeSink {
    fn wake(&mut self, notification: WakeNotification);
}

/// Synchronous durability boundary completed before an inbound wake may escape.
pub trait InboundStore {
    fn persist(
        &mut self,
        sequence: u64,
        message: &ChannelMessage,
    ) -> Result<(), PersistenceFailure>;
}

/// Typed persistence failure retaining store-provided context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistenceFailure {
    detail: String,
}

impl PersistenceFailure {
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for PersistenceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for PersistenceFailure {}

/// Deterministic persistence boundary used by the backwards-compatible runtime.
#[derive(Debug, Default)]
pub struct InMemoryInboundStore {
    persisted_sequences: Vec<u64>,
}

impl InboundStore for InMemoryInboundStore {
    fn persist(
        &mut self,
        sequence: u64,
        _message: &ChannelMessage,
    ) -> Result<(), PersistenceFailure> {
        self.persisted_sequences.push(sequence);
        Ok(())
    }
}

/// Durable inbound sequence made ready for downstream processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WakeNotification {
    sequence: u64,
}

impl WakeNotification {
    const fn new(sequence: u64) -> Self {
        Self { sequence }
    }

    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
}

/// Result of classifying one extracted inbound observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundDisposition {
    PersistedAndWoken { sequence: u64 },
    Duplicate { sequence: u64 },
}

/// Deterministic journal of durability and notification decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeEvent {
    InboundPersisted { sequence: u64 },
    WakeRequested { sequence: u64 },
    DuplicateIgnored { sequence: u64 },
}

/// Browser-channel coordination failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    Lease(LeaseValidationError),
    Persistence(PersistenceFailure),
    InvalidBridgeTimestamp { timestamp: String },
    AutonomousSendBlocked { state: TakeoverState },
    InboundSequenceExhausted,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lease(source) => write!(formatter, "connector lease rejected outbound: {source}"),
            Self::Persistence(source) => write!(formatter, "inbound persistence failed: {source}"),
            Self::InvalidBridgeTimestamp { timestamp } => {
                write!(
                    formatter,
                    "bridge message timestamp is not RFC 3339: {timestamp}"
                )
            }
            Self::AutonomousSendBlocked { state } => {
                write!(
                    formatter,
                    "autonomous outbound is blocked in state {state:?}"
                )
            }
            Self::InboundSequenceExhausted => {
                formatter.write_str("inbound durable sequence space is exhausted")
            }
        }
    }
}

impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Lease(source) => Some(source),
            Self::Persistence(source) => Some(source),
            Self::InvalidBridgeTimestamp { .. }
            | Self::AutonomousSendBlocked { .. }
            | Self::InboundSequenceExhausted => None,
        }
    }
}

impl From<LeaseValidationError> for RuntimeError {
    fn from(source: LeaseValidationError) -> Self {
        Self::Lease(source)
    }
}

impl From<PersistenceFailure> for RuntimeError {
    fn from(source: PersistenceFailure) -> Self {
        Self::Persistence(source)
    }
}

/// Durable-in-memory browser channel perimeter.
///
/// The owned vectors are the durable model for this generic runtime. A
/// persistence-backed adapter can preserve the same ordering contract later.
pub struct BrowserChannelRuntime<W, S = InMemoryInboundStore> {
    normalizer: MessageNormalizer,
    lease: ConnectorLease,
    takeover_state: TakeoverState,
    inbound_messages: Vec<ChannelMessage>,
    events: Vec<RuntimeEvent>,
    wake_sink: W,
    store: S,
}

impl<W> BrowserChannelRuntime<W, InMemoryInboundStore>
where
    W: WakeSink,
{
    #[must_use]
    pub fn new(
        connector_id: impl Into<String>,
        profile_binding: impl Into<std::sync::Arc<str>>,
        wake_sink: W,
    ) -> Self {
        Self::with_store(
            connector_id,
            profile_binding,
            wake_sink,
            InMemoryInboundStore::default(),
        )
    }
}

impl<W, S> BrowserChannelRuntime<W, S>
where
    W: WakeSink,
    S: InboundStore,
{
    #[must_use]
    pub fn with_store(
        connector_id: impl Into<String>,
        profile_binding: impl Into<std::sync::Arc<str>>,
        wake_sink: W,
        store: S,
    ) -> Self {
        Self {
            normalizer: MessageNormalizer::new(connector_id),
            lease: ConnectorLease::new(profile_binding),
            takeover_state: TakeoverState::AutomationActive,
            inbound_messages: Vec::new(),
            events: Vec::new(),
            wake_sink,
            store,
        }
    }

    pub fn ingest(&mut self, raw: RawChannelMessage) -> Result<InboundDisposition, RuntimeError> {
        let message = self.normalizer.normalize(raw);
        if let Some(index) = self
            .inbound_messages
            .iter()
            .position(|persisted| persisted.identity() == message.identity())
        {
            let sequence = sequence_for_index(index)?;
            self.events
                .push(RuntimeEvent::DuplicateIgnored { sequence });
            return Ok(InboundDisposition::Duplicate { sequence });
        }

        let sequence = sequence_for_index(self.inbound_messages.len())?;

        // Commit through the explicit durability boundary before the runtime
        // records success locally or allows any wake/event to escape.
        self.store.persist(sequence, &message)?;
        self.inbound_messages.push(message);
        self.events
            .push(RuntimeEvent::InboundPersisted { sequence });

        self.wake_sink.wake(WakeNotification::new(sequence));
        self.events.push(RuntimeEvent::WakeRequested { sequence });

        Ok(InboundDisposition::PersistedAndWoken { sequence })
    }

    /// Normalizes an accepted page-bridge observation through the regular
    /// inbound durability, deduplication, and wake path.
    ///
    /// `BridgeMessage` text remains untrusted external content. The runtime
    /// deliberately performs no instruction interpretation at this boundary.
    pub fn ingest_bridge_message(
        &mut self,
        bridge_message: BridgeMessage,
    ) -> Result<InboundDisposition, RuntimeError> {
        let timestamp_text = bridge_message.timestamp();
        let timestamp = OffsetDateTime::parse(timestamp_text, &Rfc3339).map_err(|_| {
            RuntimeError::InvalidBridgeTimestamp {
                timestamp: timestamp_text.to_owned(),
            }
        })?;

        let mut raw = RawChannelMessage::new(
            bridge_message.channel_id(),
            bridge_message.conversation_id(),
            bridge_message.sender(),
            timestamp,
            crate::message::MessageContent::Text(bridge_message.text().to_owned()),
        );
        if let Some(external_message_id) = bridge_message.external_message_id() {
            raw = raw.with_external_message_id(external_message_id);
        }

        self.ingest(raw)
    }

    #[must_use]
    pub fn inbound_messages(&self) -> &[ChannelMessage] {
        &self.inbound_messages
    }

    #[must_use]
    pub fn events(&self) -> &[RuntimeEvent] {
        &self.events
    }

    #[must_use]
    pub fn wake_sink(&self) -> &W {
        &self.wake_sink
    }

    #[must_use]
    pub fn store(&self) -> &S {
        &self.store
    }

    #[must_use]
    pub fn issue_lease(&mut self, epoch: u64, nonce: impl Into<std::sync::Arc<str>>) -> LeaseToken {
        self.lease.issue(epoch, nonce)
    }

    pub fn set_takeover_state(&mut self, state: TakeoverState) {
        self.takeover_state = state;
    }

    pub fn begin_outbound(
        &self,
        token: &LeaseToken,
        conversation_id: impl Into<String>,
        client_intent_id: impl Into<String>,
    ) -> Result<PendingDelivery, RuntimeError> {
        self.lease.validate(token)?;
        if self.takeover_state.autonomous_send() == AutonomousSend::Blocked {
            return Err(RuntimeError::AutonomousSendBlocked {
                state: self.takeover_state,
            });
        }

        Ok(PendingDelivery::new(conversation_id, client_intent_id))
    }

    #[must_use]
    pub fn observe_delivery(
        &mut self,
        delivery: PendingDelivery,
        evidence: DeliveryEvidence,
    ) -> PendingDelivery {
        delivery.observe(evidence)
    }
}

fn sequence_for_index(index: usize) -> Result<u64, RuntimeError> {
    u64::try_from(index)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(RuntimeError::InboundSequenceExhausted)
}
