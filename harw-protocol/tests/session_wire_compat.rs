//! Frozen wire shapes of the session control plane (W01-03). A change here
//! is a wire change: bump `SESSION_WIRE_MINOR` or keep the old shape.

use harw_protocol::session_wire::{AttachParams, HelloParams, SubmitResult};
use harw_protocol::{Cursor, FrameEnvelope, SessionFrame};

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
