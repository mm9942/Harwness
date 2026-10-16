//! Frozen wire shapes of the session control plane (W01-03). A change here
//! is a wire change: bump `SESSION_WIRE_MINOR` or keep the old shape.

use harw_protocol::items::ToolPlacement;
use harw_protocol::session_wire::{
    ApprovalRespondParams, AttachAck, AttachParams, GatewayStatus, HelloAck, HelloParams,
    PlacementGeneration, RespondResult, SESSION_WIRE_MINOR, SESSION_WS_PATH,
    SESSION_WS_SUBPROTOCOL, SubmitResult, TOOL_GATEWAY_WIRE_MINOR, ToolCallParams,
    ToolCallResultFrame, ToolListResult, error_codes,
};
use harw_protocol::{AgentRole, ClientCaps, Cursor, FrameEnvelope, SessionFrame, ToolApproval};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

#[test]
fn frozen_v1_frame_envelope_decodes() -> Result<(), serde_json::Error> {
    let json = r#"{"session_id":"s-1","cursor":{"generation":0,"durable":4,"live":0},"frame":{"kind":"resync","data":{"reason":"generation","head":{"generation":1,"durable":0,"live":0}}}}"#;
    let envelope: FrameEnvelope = serde_json::from_str(json)?;
    assert_eq!(envelope.cursor.durable, 4);
    assert!(matches!(
        envelope.frame,
        SessionFrame::Resync { ref reason, head } if reason == "generation" && head == Cursor::start(1)
    ));
    Ok(())
}

#[test]
fn frozen_v1_unit_frames_encode_without_data() -> Result<(), serde_json::Error> {
    assert_eq!(
        serde_json::to_string(&SessionFrame::Revoked)?,
        r#"{"kind":"revoked"}"#
    );
    assert_eq!(
        serde_json::to_string(&SessionFrame::Heartbeat)?,
        r#"{"kind":"heartbeat"}"#
    );
    Ok(())
}

#[test]
fn frozen_v1_hello_and_attach_params_decode() -> Result<(), serde_json::Error> {
    let hello: HelloParams = serde_json::from_str(r#"{"client_label":"phone","wire_minor":1}"#)?;
    assert!(hello.features.is_empty());
    assert_eq!(hello.requested_caps, None);
    let attach: AttachParams = serde_json::from_str(
        r#"{"session_id":"s","from":{"generation":0,"durable":2,"live":1},"profile":"compact","tail_items":5}"#,
    )?;
    assert_eq!(attach.tail_items, 5);
    let stale: SubmitResult =
        serde_json::from_str(r#"{"result":"stale","head":{"generation":0,"durable":9,"live":0}}"#)?;
    assert!(matches!(stale, SubmitResult::Stale { head } if head.durable == 9));
    Ok(())
}

/// R18: a minor-1 `HelloAck` (no R18 caps fields) still decodes, and a
/// grant without R18 caps encodes exactly the minor-1 shape.
#[test]
fn frozen_v1_hello_ack_is_unchanged_by_r18_caps() -> Result<(), serde_json::Error> {
    let json = r#"{"wire_minor":1,"features":[],"host_epoch":3,"granted":{"observe":true,"steer":true,"approve":true,"control":false}}"#;
    let ack: HelloAck = serde_json::from_str(json)?;
    assert_eq!(ack.granted, ClientCaps::OPERATE);
    assert_eq!(serde_json::to_string(&ack)?, json);
    Ok(())
}

/// R18 frozen shapes (wire minor 2).
#[test]
fn frozen_v2_tool_call_shapes_decode() -> Result<(), serde_json::Error> {
    let call: ToolCallParams = serde_json::from_str(
        r#"{"session_id":"s","turn_id":"t","call_id":"c","tool_name":"fs.read","arguments":{"path":"a"},"parent_call_id":"p"}"#,
    )?;
    assert_eq!(call.tool_name, "fs.read");
    assert_eq!(
        call.parent_call_id.as_ref().map(|id| id.as_str()),
        Some("p")
    );

    let frame: ToolCallResultFrame = serde_json::from_str(
        r#"{"call_id":"c","result":{"status":"error","message":"nope"},"placement":{"kind":"sandbox"},"duration_ms":2,"trust":"runtime"}"#,
    )?;
    assert_eq!(frame.placement, ToolPlacement::Sandbox);

    let list: ToolListResult = serde_json::from_str(
        r#"{"tools":[{"name":"gateway.status","description":"d","input_schema":{"type":"object"},"approval":"never","placement":{"kind":"gateway"},"parallel_safe":true}]}"#,
    )?;
    assert_eq!(list.tools.len(), 1);
    assert_eq!(
        list.tools.first().map(|tool| tool.approval),
        Some(ToolApproval::Never)
    );

    let status: GatewayStatus = serde_json::from_str(
        r#"{"host_epoch":1,"node":null,"draining":false,"connections":2,"sessions":1,"running_turns":0,"listeners":1,"tools":12,"sandbox_available":true}"#,
    )?;
    assert!(status.sandbox_available);
    Ok(())
}

/// R18 additive evolution: unknown placements, approvals and roles decode
/// to their `Unknown` variant instead of failing the whole message.
#[test]
fn r18_unknown_variants_decode_to_unknown() -> Result<(), serde_json::Error> {
    let frame: ToolCallResultFrame = serde_json::from_str(
        r#"{"call_id":"c","result":{"status":"success","value":null},"placement":{"kind":"zellhost","pod":"p"},"duration_ms":0}"#,
    )?;
    assert_eq!(frame.placement, ToolPlacement::Unknown);
    let approval: ToolApproval = serde_json::from_str(r#""two_person""#)?;
    assert_eq!(approval, ToolApproval::Unknown);
    let role: AgentRole = serde_json::from_str(r#""hub-steward""#)?;
    assert_eq!(role, AgentRole::Unknown);
    Ok(())
}

/// R18 error codes are frozen and do not collide with W00 codes.
#[test]
fn frozen_v2_error_codes() {
    assert_eq!(error_codes::TOOL_UNKNOWN, -32010);
    assert_eq!(error_codes::TOOL_NOT_GRANTED, -32011);
    assert_eq!(error_codes::TOOL_SANDBOX_UNAVAILABLE, -32012);
    assert_eq!(error_codes::HOST_DRAINING, -32013);
    assert_eq!(error_codes::TOOL_CALL_DUPLICATE, -32014);
}

/// ARC-01 (direct part): the protocol crate declares no async runtime or
/// network dependency itself. The transitive part is the arch gate rule
/// `[[forbidden_crates]] packages = ["harw-protocol"]`.
#[test]
fn protocol_crate_declares_no_runtime_or_transport_dependency() {
    let manifest = include_str!("../Cargo.toml");
    let dependencies = manifest
        .split("[dependencies]")
        .nth(1)
        .and_then(|rest| rest.split("\n[").next())
        .unwrap_or_default();
    for forbidden in [
        "tokio",
        "hyper",
        "tungstenite",
        "harw-node-transport",
        "harw-web",
        "harw-session-host",
        "harw-session-ws",
    ] {
        assert!(
            !dependencies
                .lines()
                .any(|line| line.trim_start().starts_with(forbidden)),
            "harw-protocol must not depend on {forbidden}"
        );
    }
}

// ---------------------------------------------------------------------------
// Golden fixture (tests/fixtures/session_wire_golden.json). Panic-free: every
// lookup and decode returns an error instead of unwrapping.
// ---------------------------------------------------------------------------

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const GOLDEN: &str = include_str!("fixtures/session_wire_golden.json");

fn golden(key: &str) -> TestResult<Value> {
    let root: Value = serde_json::from_str(GOLDEN)?;
    root.get(key)
        .cloned()
        .ok_or_else(|| format!("golden fixture has no entry `{key}`").into())
}

/// Decode the golden entry, re-encode it and require the identical JSON value:
/// nothing dropped, renamed or added.
fn round_trip<T: Serialize + DeserializeOwned>(key: &str) -> TestResult<T> {
    let value = golden(key)?;
    let decoded: T = serde_json::from_value(value.clone())?;
    assert_eq!(serde_json::to_value(&decoded)?, value, "entry `{key}`");
    Ok(decoded)
}

#[test]
fn golden_messages_round_trip_exactly() -> TestResult {
    let hello: HelloParams = round_trip("hello_params")?;
    assert_eq!(hello.client_label, "phone");
    let ack: HelloAck = round_trip("hello_ack")?;
    assert_eq!(ack.host_epoch, 7);
    let attach: AttachParams = round_trip("attach_params")?;
    assert_eq!(attach.tail_items, 50);
    let attach_ack: AttachAck = round_trip("attach_ack")?;
    assert_eq!(attach_ack.head.durable, 14);
    assert_eq!(attach_ack.replay_from.durable, 12);
    let stale: SubmitResult = round_trip("submit_stale")?;
    assert!(matches!(stale, SubmitResult::Stale { head } if head.durable == 14));
    let respond: ApprovalRespondParams = round_trip("approval_respond_params")?;
    assert_eq!(respond.reason.as_deref(), Some("ok"));
    let already: RespondResult = round_trip("respond_already_resolved")?;
    assert!(matches!(already, RespondResult::AlreadyResolved { ref by } if by == "device:laptop"));
    Ok(())
}

#[test]
fn golden_envelopes_round_trip_and_carry_cursor_triple() -> TestResult {
    let lagged: FrameEnvelope = round_trip("envelope_lagged")?;
    let expected = Cursor {
        generation: 1,
        durable: 14,
        live: 0,
    };
    assert!(matches!(
        lagged.frame,
        SessionFrame::Lagged { resume_from } if resume_from == expected
    ));
    let resync: FrameEnvelope = round_trip("envelope_resync")?;
    assert!(matches!(resync.frame, SessionFrame::Resync { .. }));
    let resolved: FrameEnvelope = round_trip("envelope_approval_resolved")?;
    assert!(matches!(
        resolved.frame,
        SessionFrame::ApprovalResolved { ref by, .. } if by == "device:laptop"
    ));
    let beat: FrameEnvelope = round_trip("envelope_heartbeat")?;
    assert!(matches!(beat.frame, SessionFrame::Heartbeat));
    Ok(())
}

/// Request params use `deny_unknown_fields`: a client cannot smuggle identity
/// (principal, tenant, actor) through an extra field.
#[test]
fn unknown_fields_are_rejected_on_requests_and_cursors() -> TestResult {
    for (key, extra) in [
        ("hello_params", "tenant"),
        ("attach_params", "principal"),
        ("approval_respond_params", "actor"),
        ("hello_ack", "granted_by"),
    ] {
        let mut value = golden(key)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| format!("entry `{key}` is not an object"))?;
        object.insert(extra.to_owned(), Value::String("x".into()));
        let rejected = match key {
            "hello_params" => serde_json::from_value::<HelloParams>(value).is_err(),
            "attach_params" => serde_json::from_value::<AttachParams>(value).is_err(),
            "approval_respond_params" => {
                serde_json::from_value::<ApprovalRespondParams>(value).is_err()
            }
            _ => serde_json::from_value::<HelloAck>(value).is_err(),
        };
        assert!(rejected, "`{key}` accepted unknown field `{extra}`");
    }
    assert!(
        serde_json::from_str::<Cursor>(r#"{"generation":0,"durable":0,"live":0,"extra":1}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<ClientCaps>(
            r#"{"observe":true,"steer":false,"approve":false,"control":false,"root":true}"#
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<FrameEnvelope>(
            r#"{"session_id":"s","cursor":{"generation":0,"durable":0,"live":0},"frame":{"kind":"heartbeat"},"extra":1}"#
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn cursor_requires_all_three_components() {
    assert!(serde_json::from_str::<Cursor>(r#"{"generation":0,"durable":1}"#).is_err());
    assert!(serde_json::from_str::<Cursor>(r#"{"durable":1,"live":0}"#).is_err());
}

#[test]
fn unknown_frame_kind_in_envelope_decodes_to_unknown() -> TestResult {
    let json = r#"{"session_id":"s","cursor":{"generation":1,"durable":1,"live":0},"frame":{"kind":"from_the_future","data":{"a":1}}}"#;
    let envelope: FrameEnvelope = serde_json::from_str(json)?;
    assert!(matches!(envelope.frame, SessionFrame::Unknown));
    Ok(())
}

#[test]
fn version_and_subprotocol_constants_match_fixture() -> TestResult {
    let constants = golden("constants")?;
    let text = |key: &str| {
        constants
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let number = |key: &str| constants.get(key).and_then(Value::as_u64);
    assert_eq!(text("subprotocol").as_deref(), Some(SESSION_WS_SUBPROTOCOL));
    assert_eq!(text("path").as_deref(), Some(SESSION_WS_PATH));
    assert_eq!(number("wire_minor"), Some(u64::from(SESSION_WIRE_MINOR)));
    assert_eq!(
        number("tool_gateway_wire_minor"),
        Some(u64::from(TOOL_GATEWAY_WIRE_MINOR))
    );
    assert_eq!(
        number("default_tail_items"),
        Some(u64::from(harw_protocol::session_wire::DEFAULT_TAIL_ITEMS))
    );
    Ok(())
}

#[test]
fn error_codes_match_fixture() -> TestResult {
    let codes = golden("error_codes")?;
    let expected = [
        ("HELLO_REQUIRED", error_codes::HELLO_REQUIRED),
        ("DENIED", error_codes::DENIED),
        ("NOT_FOUND", error_codes::NOT_FOUND),
        ("REVOKED", error_codes::REVOKED),
        ("BUSY", error_codes::BUSY),
        ("TOOL_UNKNOWN", error_codes::TOOL_UNKNOWN),
        ("TOOL_NOT_GRANTED", error_codes::TOOL_NOT_GRANTED),
        (
            "TOOL_SANDBOX_UNAVAILABLE",
            error_codes::TOOL_SANDBOX_UNAVAILABLE,
        ),
        ("HOST_DRAINING", error_codes::HOST_DRAINING),
        ("TOOL_CALL_DUPLICATE", error_codes::TOOL_CALL_DUPLICATE),
    ];
    for (name, value) in expected {
        assert_eq!(
            codes.get(name).and_then(Value::as_i64),
            Some(i64::from(value)),
            "{name}"
        );
    }
    Ok(())
}

/// R2: the placement generation is a transparent integer, so adding it to a
/// message later is a plain additive field.
#[test]
fn placement_generation_is_a_transparent_integer() -> TestResult {
    assert_eq!(serde_json::to_string(&PlacementGeneration(3))?, "3");
    let decoded: PlacementGeneration = serde_json::from_str("9")?;
    assert_eq!(decoded, PlacementGeneration(9));
    assert!(serde_json::from_str::<PlacementGeneration>("-1").is_err());
    Ok(())
}
