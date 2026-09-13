use harw_channel_browser::delivery::{DeliveryEvidence, DeliveryState};
use harw_channel_browser::lease::LeaseToken;
use harw_channel_browser::message::{ChannelMessage, MessageContent, RawChannelMessage};
use harw_channel_browser::runtime::{
    BrowserChannelRuntime, InboundDisposition, InboundStore, PersistenceFailure, RuntimeError,
    RuntimeEvent, WakeNotification, WakeSink,
};
use harw_channel_browser::takeover::TakeoverState;
use harw_channel_browser::{
    BridgeDefinition, BridgeIngestOutcome, BridgeMessage, BridgePolicy, PageBridgeIngestor,
};
use time::{Duration, OffsetDateTime};

#[derive(Debug, Default)]
struct RecordingWakeSink {
    notifications: Vec<WakeNotification>,
}

impl WakeSink for RecordingWakeSink {
    fn wake(&mut self, notification: WakeNotification) {
        self.notifications.push(notification);
    }
}

#[derive(Debug, Default)]
struct FailingStore {
    attempts: usize,
}

impl InboundStore for FailingStore {
    fn persist(
        &mut self,
        _sequence: u64,
        _message: &ChannelMessage,
    ) -> Result<(), PersistenceFailure> {
        self.attempts += 1;
        Err(PersistenceFailure::new("durable store unavailable"))
    }
}

fn timestamp(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(seconds)
        .unwrap_or_else(|error| panic!("test timestamp must be valid: {error}"))
}

fn inbound(external_message_id: &str) -> RawChannelMessage {
    RawChannelMessage::new(
        "support",
        "conversation-7",
        "customer-42",
        timestamp(1_750_000_000),
        MessageContent::Text("Need help".to_owned()),
    )
    .with_external_message_id(external_message_id)
}

fn runtime() -> BrowserChannelRuntime<RecordingWakeSink> {
    BrowserChannelRuntime::new(
        "company.erp.customer-chat@1",
        "erp-support-bot",
        RecordingWakeSink::default(),
    )
}

fn bridge_message(message_timestamp: &str, external_message_id: &str) -> BridgeMessage {
    let definition = BridgeDefinition::new(
        "support-bridge",
        "v1",
        "window.harwness = window.harwness || {};",
        BridgePolicy::new(4096, 8, Duration::minutes(1))
            .unwrap_or_else(|error| panic!("test bridge policy must be valid: {error}")),
    )
    .unwrap_or_else(|error| panic!("test bridge definition must be valid: {error}"));
    let payload = format!(
        r#"{{"schema":"harwness.browser-bridge-message/v1","channel_id":"support","conversation_id":"conversation-7","external_message_id":"{external_message_id}","sender":"customer-42","timestamp":"{message_timestamp}","content":{{"type":"text","text":"Need help"}}}}"#
    );
    let mut ingestor = PageBridgeIngestor::new(definition);
    match ingestor.ingest(timestamp(1_750_000_000), payload.as_bytes()) {
        BridgeIngestOutcome::Accepted(message) => message,
        BridgeIngestOutcome::Rejected(rejection) => {
            panic!("test bridge payload must be accepted: {rejection:?}")
        }
    }
}

#[test]
fn inbound_is_normalized_and_persisted_before_wake_notification() {
    let mut runtime = runtime();

    let disposition = runtime
        .ingest(inbound("external-99"))
        .unwrap_or_else(|error| panic!("valid inbound message must ingest: {error}"));

    assert_eq!(
        disposition,
        InboundDisposition::PersistedAndWoken { sequence: 1 }
    );
    assert_eq!(runtime.inbound_messages().len(), 1);
    assert_eq!(runtime.wake_sink().notifications.len(), 1);
    assert_eq!(runtime.wake_sink().notifications[0].sequence(), 1);
    assert_eq!(
        runtime.events(),
        &[
            RuntimeEvent::InboundPersisted { sequence: 1 },
            RuntimeEvent::WakeRequested { sequence: 1 },
        ]
    );
}

#[test]
fn persistence_failure_emits_neither_wake_nor_success_event() {
    let mut runtime = BrowserChannelRuntime::with_store(
        "company.erp.customer-chat@1",
        "erp-support-bot",
        RecordingWakeSink::default(),
        FailingStore::default(),
    );

    let error = runtime
        .ingest(inbound("external-store-failure"))
        .expect_err("failed durability must abort inbound ingestion");

    match error {
        RuntimeError::Persistence(source) => {
            assert_eq!(source.detail(), "durable store unavailable");
        }
        other => panic!("expected typed persistence failure, got {other}"),
    }
    assert_eq!(runtime.store().attempts, 1);
    assert!(runtime.inbound_messages().is_empty());
    assert!(runtime.events().is_empty());
    assert!(runtime.wake_sink().notifications.is_empty());
}

#[test]
fn stale_or_invalid_lease_blocks_outbound_before_delivery_state_is_created() {
    let mut runtime = runtime();
    let stale = runtime.issue_lease(7, "nonce-a");
    let current = runtime.issue_lease(8, "nonce-b");

    let stale_error = runtime
        .begin_outbound(&stale, "conversation-7", "intent-stale")
        .expect_err("renewal must fence the stale lease token");
    assert!(matches!(stale_error, RuntimeError::Lease(_)));

    let wrong_nonce = LeaseToken::new("erp-support-bot", 8, "wrong-nonce");
    let invalid_error = runtime
        .begin_outbound(&wrong_nonce, "conversation-7", "intent-invalid")
        .expect_err("a wrong nonce must not exercise connector ownership");
    assert!(matches!(invalid_error, RuntimeError::Lease(_)));

    assert!(
        runtime
            .begin_outbound(&current, "conversation-7", "intent-current")
            .is_ok()
    );
}

#[test]
fn every_non_automation_takeover_state_blocks_outbound() {
    for state in [
        TakeoverState::SharedDraft,
        TakeoverState::HumanActive,
        TakeoverState::Paused,
        TakeoverState::Escalated,
    ] {
        let mut runtime = runtime();
        let token = runtime.issue_lease(1, "nonce-current");
        runtime.set_takeover_state(state);

        let error = runtime
            .begin_outbound(&token, "conversation-7", "intent-11")
            .expect_err("human takeover policy must fence autonomous sends");
        assert_eq!(error, RuntimeError::AutonomousSendBlocked { state });
    }
}

#[test]
fn submit_evidence_remains_pending_until_external_confirmation() {
    let mut runtime = runtime();
    let token = runtime.issue_lease(1, "nonce-current");
    let delivery = runtime
        .begin_outbound(&token, "conversation-7", "intent-11")
        .unwrap_or_else(|error| panic!("current automated lease may send: {error}"));

    let submitted = runtime.observe_delivery(delivery, DeliveryEvidence::SubmitAccepted);
    assert_eq!(submitted.state(), DeliveryState::AwaitingConfirmation);
    assert_eq!(submitted.external_message_id(), None);

    let confirmed = runtime.observe_delivery(
        submitted,
        DeliveryEvidence::NetworkConfirmed {
            external_message_id: "external-100".to_owned(),
        },
    );
    assert_eq!(confirmed.state(), DeliveryState::Delivered);
    assert_eq!(confirmed.external_message_id(), Some("external-100"));
}

#[test]
fn reconnect_replay_does_not_persist_or_wake_a_duplicate() {
    let mut runtime = runtime();
    let first = runtime
        .ingest(inbound("external-99"))
        .unwrap_or_else(|error| panic!("first observation must ingest: {error}"));
    let replay = runtime
        .ingest(inbound("external-99"))
        .unwrap_or_else(|error| panic!("replay classification must succeed: {error}"));

    assert_eq!(first, InboundDisposition::PersistedAndWoken { sequence: 1 });
    assert_eq!(replay, InboundDisposition::Duplicate { sequence: 1 });
    assert_eq!(runtime.inbound_messages().len(), 1);
    assert_eq!(runtime.wake_sink().notifications.len(), 1);
    assert_eq!(
        runtime.events(),
        &[
            RuntimeEvent::InboundPersisted { sequence: 1 },
            RuntimeEvent::WakeRequested { sequence: 1 },
            RuntimeEvent::DuplicateIgnored { sequence: 1 },
        ]
    );
}

#[test]
fn accepted_bridge_message_uses_the_normal_inbound_durability_path() {
    let mut runtime = runtime();

    let disposition = runtime
        .ingest_bridge_message(bridge_message("2025-06-15T15:06:40Z", "bridge-99"))
        .unwrap_or_else(|error| panic!("valid bridge message must ingest: {error}"));

    assert_eq!(
        disposition,
        InboundDisposition::PersistedAndWoken { sequence: 1 }
    );
    assert_eq!(runtime.inbound_messages().len(), 1);
    let persisted = &runtime.inbound_messages()[0];
    assert_eq!(persisted.channel_id(), "support");
    assert_eq!(persisted.conversation_id(), "conversation-7");
    assert_eq!(persisted.sender(), "customer-42");
    assert_eq!(persisted.timestamp(), timestamp(1_750_000_000));
    assert_eq!(
        persisted.content(),
        &MessageContent::Text("Need help".to_owned())
    );
    assert_eq!(runtime.wake_sink().notifications.len(), 1);
}

#[test]
fn duplicate_bridge_message_does_not_persist_or_wake_again() {
    let mut runtime = runtime();

    let first = runtime
        .ingest_bridge_message(bridge_message("2025-06-15T15:06:40Z", "bridge-99"))
        .unwrap_or_else(|error| panic!("first bridge message must ingest: {error}"));
    let replay = runtime
        .ingest_bridge_message(bridge_message("2025-06-15T15:06:40Z", "bridge-99"))
        .unwrap_or_else(|error| panic!("bridge replay classification must succeed: {error}"));

    assert_eq!(first, InboundDisposition::PersistedAndWoken { sequence: 1 });
    assert_eq!(replay, InboundDisposition::Duplicate { sequence: 1 });
    assert_eq!(runtime.inbound_messages().len(), 1);
    assert_eq!(runtime.wake_sink().notifications.len(), 1);
}

#[test]
fn malformed_bridge_timestamp_has_no_persistence_or_wake_side_effect() {
    let mut runtime = runtime();

    let error = runtime
        .ingest_bridge_message(bridge_message("definitely-not-rfc3339", "bridge-invalid"))
        .expect_err("invalid bridge timestamp must be rejected before persistence");

    assert_eq!(
        error,
        RuntimeError::InvalidBridgeTimestamp {
            timestamp: "definitely-not-rfc3339".to_owned(),
        }
    );
    assert!(runtime.inbound_messages().is_empty());
    assert!(runtime.events().is_empty());
    assert!(runtime.wake_sink().notifications.is_empty());
}
