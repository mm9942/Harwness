//! Frozen wire shapes of the session control plane (W01-03). A change here
//! is a wire change: bump `SESSION_WIRE_MINOR` or keep the old shape.

use harw_protocol::items::ToolPlacement;
use harw_protocol::session_wire::{
    AttachParams, GatewayStatus, HelloAck, HelloParams, SubmitResult, ToolCallParams,
    ToolCallResultFrame, ToolListResult, error_codes,
};
use harw_protocol::{AgentRole, ClientCaps, Cursor, FrameEnvelope, SessionFrame, ToolApproval};

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
