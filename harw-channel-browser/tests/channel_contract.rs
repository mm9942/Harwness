use harw_browser::selector::Selector;
use harw_channel_browser::connector::{
    CompiledConnector, ConfirmationMode, ConnectorDefinition, ProfileMode,
};
use harw_channel_browser::delivery::{DeliveryEvidence, DeliveryState, PendingDelivery};
use harw_channel_browser::lease::{ConnectorLease, LeaseToken};
use harw_channel_browser::message::{
    ChannelMessage, MessageContent, MessageIdentity, MessageNormalizer, RawChannelMessage,
};
use harw_channel_browser::takeover::{AutonomousSend, TakeoverEvent, TakeoverState};
use time::OffsetDateTime;

const CONNECTOR_TOML: &str = r#"
schema = "harwness.browser-channel/v1"
id = "company.erp.customer-chat@1"
version = "1.0.0"
browser_binding = "thirtyfour.firefox-bidi@1"
start_url = "https://erp.example.com/support"

[origin_policy]
allow = ["https://erp.example.com"]
auth_allow = ["https://login.example.com"]
deny_private_networks = true

[profile]
mode = "persistent"
profile_binding = "erp-support-bot"

[conversation_list]
selector = { test_id = "conversation-list", fallbacks = [{ role = "list" }] }

[message_container]
selector = { test_id = "message-thread" }

[message]
selector = { test_id = "message" }
id_attribute = "data-message-id"

[message.sender]
selector = { test_id = "sender-name" }

[message.body]
selector = { test_id = "message-body" }

[composer]
input = { test_id = "reply-box" }
send = { role = "button", name = "Send" }

[confirmation]
mode = "network-or-dom"
request_path = "/api/messages"
own_message_selector = { test_id = "message-self" }

[activity]
human_takeover_selector = { test_id = "agent-active" }
"#;

fn compiled_connector() -> CompiledConnector {
    let definition = ConnectorDefinition::from_toml(CONNECTOR_TOML)
        .unwrap_or_else(|error| panic!("valid connector definition must parse: {error}"));
    definition
        .compile()
        .unwrap_or_else(|error| panic!("valid connector definition must compile: {error}"))
}

fn timestamp(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(seconds)
        .unwrap_or_else(|error| panic!("test timestamp must be in range: {error}"))
}

#[test]
fn declarative_connector_compiles_into_typed_policy_and_selector_chains() {
    let connector = compiled_connector();

    assert_eq!(connector.schema(), "harwness.browser-channel/v1");
    assert_eq!(connector.id(), "company.erp.customer-chat@1");
    assert_eq!(connector.version(), "1.0.0");
    assert_eq!(connector.profile().mode(), ProfileMode::Persistent);
    assert_eq!(connector.profile().binding(), Some("erp-support-bot"));
    assert_eq!(
        connector.confirmation().mode(),
        ConfirmationMode::NetworkOrDom
    );
    assert_eq!(
        connector.confirmation().request_path(),
        Some("/api/messages")
    );

    let conversation_list = connector.conversation_list().selector();
    assert_eq!(
        conversation_list.primary,
        Selector::TestId("conversation-list".to_owned())
    );
    assert_eq!(
        conversation_list.fallbacks,
        vec![Selector::Role {
            role: "list".to_owned(),
            name: None,
        }]
    );
    assert_eq!(
        connector.composer().send().primary,
        Selector::Role {
            role: "button".to_owned(),
            name: Some("Send".to_owned()),
        }
    );
}

#[test]
fn connector_validation_rejects_unsupported_schema_and_cross_origin_start_url() {
    let unsupported_schema = CONNECTOR_TOML.replace(
        "harwness.browser-channel/v1",
        "harwness.browser-channel/v999",
    );
    let definition = ConnectorDefinition::from_toml(&unsupported_schema)
        .unwrap_or_else(|error| panic!("syntax remains valid TOML: {error}"));
    assert!(definition.compile().is_err());

    let disallowed_start = CONNECTOR_TOML.replace(
        "https://erp.example.com/support",
        "https://untrusted.example.net/support",
    );
    let definition = ConnectorDefinition::from_toml(&disallowed_start)
        .unwrap_or_else(|error| panic!("syntax remains valid TOML: {error}"));
    assert!(definition.compile().is_err());
}

#[test]
fn connector_validation_rejects_ambiguous_and_empty_selectors() {
    let ambiguous = CONNECTOR_TOML.replace(
        "{ test_id = \"message-thread\" }",
        "{ test_id = \"message-thread\", css = \"#thread\" }",
    );
    assert!(ConnectorDefinition::from_toml(&ambiguous).is_err());

    let empty = CONNECTOR_TOML.replace("{ test_id = \"message-thread\" }", "{ test_id = \"\" }");
    let definition = ConnectorDefinition::from_toml(&empty)
        .unwrap_or_else(|error| panic!("empty selector is structurally valid TOML: {error}"));
    assert!(definition.compile().is_err());
}

#[test]
fn native_external_id_is_the_authoritative_dedupe_identity() {
    let normalizer = MessageNormalizer::new("company.erp.customer-chat@1");
    let first = RawChannelMessage::new(
        "support",
        "conversation-7",
        "customer-42",
        timestamp(1_750_000_000),
        MessageContent::Text("Need help".to_owned()),
    )
    .with_external_message_id("external-99");
    let replay = RawChannelMessage::new(
        "support",
        "conversation-7",
        "customer-42",
        timestamp(1_750_000_030),
        MessageContent::Text("content changed upstream".to_owned()),
    )
    .with_external_message_id("external-99");

    let first = normalizer.normalize(first);
    let replay = normalizer.normalize(replay);

    assert_eq!(
        first.identity(),
        &MessageIdentity::External("external-99".to_owned())
    );
    assert_eq!(first.identity(), replay.identity());
    assert!(!first.identity_is_uncertain());
}

#[test]
fn absent_external_id_uses_bounded_content_fingerprint_and_marks_uncertainty() {
    let normalizer = MessageNormalizer::with_timestamp_window(
        "company.erp.customer-chat@1",
        time::Duration::minutes(1),
    );
    let raw = || {
        RawChannelMessage::new(
            "support",
            "conversation-7",
            "customer-42",
            timestamp(1_750_000_030),
            MessageContent::Text("Need help".to_owned()),
        )
    };

    let first: ChannelMessage = normalizer.normalize(raw());
    let replay: ChannelMessage = normalizer.normalize(raw());
    let next_window = normalizer.normalize(RawChannelMessage::new(
        "support",
        "conversation-7",
        "customer-42",
        timestamp(1_750_000_091),
        MessageContent::Text("Need help".to_owned()),
    ));

    assert_eq!(first.identity(), replay.identity());
    assert_ne!(first.identity(), next_window.identity());
    assert!(first.identity_is_uncertain());
}

#[test]
fn takeover_state_machine_fences_autonomous_sends() {
    let active = TakeoverState::AutomationActive;
    assert_eq!(active.autonomous_send(), AutonomousSend::Allowed);
    assert_eq!(
        active.transition(TakeoverEvent::HumanActivityDetected),
        TakeoverState::HumanActive
    );

    for state in [
        TakeoverState::SharedDraft,
        TakeoverState::HumanActive,
        TakeoverState::Paused,
        TakeoverState::Escalated,
    ] {
        assert_eq!(state.autonomous_send(), AutonomousSend::Blocked);
    }

    assert_eq!(
        TakeoverState::HumanActive.transition(TakeoverEvent::ExplicitResume),
        TakeoverState::AutomationActive
    );
    assert_eq!(
        TakeoverState::AutomationActive.transition(TakeoverEvent::Escalate),
        TakeoverState::Escalated
    );
}

#[test]
fn connector_lease_rejects_stale_epoch_and_wrong_nonce() {
    let mut lease = ConnectorLease::new("erp-support-bot");
    let first = lease.issue(7, "nonce-a");
    let renewed = lease.issue(8, "nonce-b");

    assert_eq!(first, LeaseToken::new("erp-support-bot", 7, "nonce-a"));
    assert!(renewed.fences(&first));
    assert!(lease.validate(&renewed).is_ok());
    assert!(lease.validate(&first).is_err());
    assert!(
        lease
            .validate(&LeaseToken::new("erp-support-bot", 8, "nonce-wrong"))
            .is_err()
    );
    assert!(
        lease
            .validate(&LeaseToken::new("different-profile", 8, "nonce-b"))
            .is_err()
    );
}

#[test]
fn click_or_submit_alone_never_marks_a_delivery_confirmed() {
    let pending = PendingDelivery::new("conversation-7", "client-intent-11");

    let after_submit = pending.observe(DeliveryEvidence::SubmitAccepted);
    assert_eq!(after_submit.state(), DeliveryState::AwaitingConfirmation);
    assert_eq!(after_submit.external_message_id(), None);

    let uncertain = after_submit.observe(DeliveryEvidence::ConfirmationTimedOut);
    assert_eq!(uncertain.state(), DeliveryState::Uncertain);
    assert_eq!(uncertain.external_message_id(), None);
}

#[test]
fn network_or_dom_evidence_confirms_delivery_with_external_identity() {
    let network_confirmed = PendingDelivery::new("conversation-7", "client-intent-11").observe(
        DeliveryEvidence::NetworkConfirmed {
            external_message_id: "external-100".to_owned(),
        },
    );
    assert_eq!(network_confirmed.state(), DeliveryState::Delivered);
    assert_eq!(
        network_confirmed.external_message_id(),
        Some("external-100")
    );

    let dom_confirmed = PendingDelivery::new("conversation-7", "client-intent-12").observe(
        DeliveryEvidence::DomConfirmed {
            external_message_id: "external-101".to_owned(),
        },
    );
    assert_eq!(dom_confirmed.state(), DeliveryState::Delivered);
    assert_eq!(dom_confirmed.external_message_id(), Some("external-101"));
}
