//! Frozen wire shapes of the session control plane (W01-03). A change here
//! is a wire change: bump `SESSION_WIRE_MINOR` or keep the old shape.

use harw_protocol::items::ToolPlacement;
use harw_protocol::session_port::PortError;
use harw_protocol::session_wire::{
    ApprovalRespondParams, ArchiveParams, AttachAck, AttachParams, CatalogGetParams,
    CatalogProvider, CatalogResult, CommandEffect, CommandEntry, CommandOrigin, CommandRunParams,
    CommandRunResult, CommandsListParams, CommandsListResult, ConfigGetParams, ConfigGetResult,
    ConfigResetParams, ConfigSetParams, CreateInParams, GatewayStatus, HelloAck, HelloParams,
    HolderInfo, HolderKind, LastOrigin, MANAGE_WIRE_MINOR, MAX_CLIENT_MSG_ID_BYTES,
    MAX_COMMAND_LINE_BYTES, MAX_CONFIG_KEYS, MAX_SECRET_VALUE_BYTES, PROJECTS_WIRE_MINOR,
    PageParams, PageResult, PlacementGeneration, ProjectsParams, ProjectsResult, ProviderAuthKind,
    ReadOnlyReason, RenameParams, RespondResult, RowOrigin, SESSION_WIRE_MINOR, SESSION_WS_PATH,
    SESSION_WS_SUBPROTOCOL, SecretPutParams, SecretPutResult, SessionRow, SessionSummary,
    SetProjectParams, StepUpCancelParams, StepUpChallenge, StepUpConfirmParams, SubmitResult,
    TOOL_GATEWAY_WIRE_MINOR, ToolCallParams, ToolCallResultFrame, ToolListResult, WireSecret,
    error_codes,
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
        number("projects_wire_minor"),
        Some(u64::from(PROJECTS_WIRE_MINOR))
    );
    assert_eq!(
        number("manage_wire_minor"),
        Some(u64::from(MANAGE_WIRE_MINOR))
    );
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

// ---------------------------------------------------------------------------
// Wire minor 3 (feature `projects`). All ids are synthetic.
// ---------------------------------------------------------------------------

fn sorted_keys(value: &Value) -> TestResult<Vec<String>> {
    let object = value.as_object().ok_or("value is not an object")?;
    let mut keys: Vec<String> = object.keys().cloned().collect();
    keys.sort();
    Ok(keys)
}

/// The minor-2 hello, its ack and `SessionSummary` keep their exact bytes:
/// the minor-3 additions never leak into them.
#[test]
fn frozen_v2_messages_are_byte_identical_at_minor_3() -> TestResult {
    let hello: HelloParams = round_trip("hello_params")?;
    assert_eq!(hello.wire_minor, 2);
    let ack: HelloAck = round_trip("hello_ack")?;
    assert_eq!(ack.wire_minor, 2);
    let hello_text = serde_json::to_string(&hello)?;
    assert!(!hello_text.contains("projects"), "{hello_text}");

    let summary_json = r#"{"session_id":"sess-a1","title":"t","tenant":null,"state":{"state":"idle"},"attached":0,"updated_at":"2026-01-01T00:00:00Z","model":null}"#;
    let summary: SessionSummary = serde_json::from_str(summary_json)?;
    let value = serde_json::to_value(&summary)?;
    assert_eq!(value, serde_json::from_str::<Value>(summary_json)?);
    assert_eq!(
        sorted_keys(&value)?,
        [
            "attached",
            "model",
            "session_id",
            "state",
            "tenant",
            "title",
            "updated_at"
        ]
    );
    Ok(())
}

#[test]
fn holder_frame_round_trips_and_is_unknown_to_v2_kinds() -> TestResult {
    let json = r#"{"session_id":"sess-a1","cursor":{"generation":1,"durable":2,"live":0},"frame":{"kind":"holder","data":{"session_id":"sess-a1","holder":{"kind":"tui","since":"2026-01-01T00:00:00Z"}}}}"#;
    let envelope: FrameEnvelope = serde_json::from_str(json)?;
    assert!(matches!(
        envelope.frame,
        SessionFrame::Holder { holder: Some(ref h), .. } if h.kind == HolderKind::Tui
    ));
    assert_eq!(
        serde_json::to_value(&envelope)?,
        serde_json::from_str::<Value>(json)?
    );

    let free = r#"{"kind":"holder","data":{"session_id":"sess-a1","holder":null}}"#;
    let frame: SessionFrame = serde_json::from_str(free)?;
    assert!(matches!(frame, SessionFrame::Holder { holder: None, .. }));

    let future = r#"{"session_id":"s","cursor":{"generation":1,"durable":1,"live":0},"frame":{"kind":"holder_v9","data":{"a":1}}}"#;
    let envelope: FrameEnvelope = serde_json::from_str(future)?;
    assert!(matches!(envelope.frame, SessionFrame::Unknown));
    Ok(())
}

#[test]
fn holder_info_has_no_pid_field() -> TestResult {
    let info: HolderInfo =
        serde_json::from_str(r#"{"kind":"gateway","since":"2026-01-01T00:00:00Z","pid":4242}"#)?;
    assert_eq!(info.kind, HolderKind::Gateway);
    let value = serde_json::to_value(&info)?;
    assert_eq!(sorted_keys(&value)?, ["kind", "since"]);
    Ok(())
}

#[test]
fn minor3_params_reject_unknown_fields() {
    fn rejects<T: DeserializeOwned>(base: &str) -> bool {
        let text = format!("{},\"tenant\":\"x\"}}", base.trim_end_matches('}'));
        serde_json::from_str::<T>(&text).is_err()
    }
    assert!(rejects::<ProjectsParams>("{}"));
    assert!(rejects::<PageParams>("{}"));
    assert!(rejects::<CreateInParams>(r#"{"project_key":"p"}"#));
    assert!(rejects::<SetProjectParams>(
        r#"{"session_id":"sess-a1","project_key":"p"}"#
    ));
    assert!(rejects::<RenameParams>(
        r#"{"session_id":"sess-a1","title":"t"}"#
    ));
    assert!(rejects::<ArchiveParams>(
        r#"{"session_id":"sess-a1","archived":true}"#
    ));
}

#[test]
fn minor3_params_accept_their_plain_shape() -> TestResult {
    let page: PageParams = serde_json::from_str("{}")?;
    assert_eq!(page.project_key, None);
    assert!(!page.include_archived);
    let none_project: PageParams = serde_json::from_str(r#"{"project_key":""}"#)?;
    assert_eq!(none_project.project_key.as_deref(), Some(""));
    let projects: ProjectsParams = serde_json::from_str(r#"{"limit":50}"#)?;
    assert_eq!(projects.limit, Some(50));
    Ok(())
}

#[test]
fn minor3_results_accept_additional_fields() -> TestResult {
    let row = r#"{"session_id":"sess-a1","updated_at":"2026-01-01T00:00:00Z","state":{"state":"idle"},"attached":0,"origin":"hosted","continuable":true,"future_field":1}"#;
    let parsed: SessionRow = serde_json::from_str(row)?;
    assert!(parsed.continuable);
    assert_eq!(parsed.origin, RowOrigin::Hosted);

    let page = format!(r#"{{"sessions":[{row}],"extra":true}}"#);
    let parsed: PageResult = serde_json::from_str(&page)?;
    assert_eq!(parsed.sessions.len(), 1);
    assert_eq!(parsed.next_cursor, None);

    let projects = r#"{"projects":[{"project_key":"","label":"x","session_count":1,"last_activity":"2026-01-01T00:00:00Z","active_count":0,"more":1}],"share":{"mode":"off","tier":"observer"},"extra":[]}"#;
    let parsed: ProjectsResult = serde_json::from_str(projects)?;
    assert_eq!(parsed.projects.len(), 1);
    Ok(())
}

#[test]
fn row_enums_decode_unknown() -> TestResult {
    assert_eq!(
        serde_json::from_str::<RowOrigin>(r#""future""#)?,
        RowOrigin::Unknown
    );
    assert_eq!(
        serde_json::from_str::<ReadOnlyReason>(r#""future""#)?,
        ReadOnlyReason::Unknown
    );
    assert_eq!(
        serde_json::from_str::<HolderKind>(r#""future""#)?,
        HolderKind::Unknown
    );
    assert_eq!(
        serde_json::from_str::<LastOrigin>(r#""future""#)?,
        LastOrigin::Unknown
    );
    Ok(())
}

#[test]
fn busy_code_maps_to_port_busy() {
    assert_eq!(
        PortError::from_code(error_codes::BUSY, "x".into()),
        PortError::Busy("x".into())
    );
}

// ---------------------------------------------------------------------------
// Wire minor 4 (features `manage`, `stepup`). All ids are synthetic.
// ---------------------------------------------------------------------------

/// Minor-3 messages keep their exact bytes at minor 4.
#[test]
fn frozen_v3_messages_are_byte_identical_at_minor_4() -> TestResult {
    let row_json = r#"{"session_id":"sess-a1","updated_at":"2026-01-01T00:00:00Z","state":{"state":"idle"},"attached":0,"origin":"hosted","continuable":true}"#;
    let row: SessionRow = serde_json::from_str(row_json)?;
    let value = serde_json::to_value(&row)?;
    assert_eq!(value, serde_json::from_str::<Value>(row_json)?);
    assert!(!row_json.contains("stepup"));

    let projects_json = r#"{"projects":[{"project_key":"","label":"x","session_count":1,"last_activity":"2026-01-01T00:00:00Z","active_count":0}],"share":{"mode":"off","tier":"observer"}}"#;
    let projects: ProjectsResult = serde_json::from_str(projects_json)?;
    assert_eq!(
        serde_json::to_value(&projects)?,
        serde_json::from_str::<Value>(projects_json)?
    );

    let holder_json = r#"{"kind":"tui","since":"2026-01-01T00:00:00Z"}"#;
    let holder: HolderInfo = serde_json::from_str(holder_json)?;
    assert_eq!(
        serde_json::to_value(&holder)?,
        serde_json::from_str::<Value>(holder_json)?
    );

    let frame_json = r#"{"session_id":"sess-a1","cursor":{"generation":1,"durable":2,"live":0},"frame":{"kind":"holder","data":{"session_id":"sess-a1","holder":{"kind":"tui","since":"2026-01-01T00:00:00Z"}}}}"#;
    let envelope: FrameEnvelope = serde_json::from_str(frame_json)?;
    assert_eq!(
        serde_json::to_value(&envelope)?,
        serde_json::from_str::<Value>(frame_json)?
    );
    Ok(())
}

const CHALLENGE_JSON: &str = r#"{"stepup_id":"00000000000000000000000000000001","action":"config.set","target":"effort","text":"Set effort to high","nonce":"0000000000000000000000000000000000000000000000000000000000000002","hash":"0000000000000000000000000000000000000000000000000000000000000003","expires_at":"2026-01-01T00:03:00Z"}"#;

#[test]
fn stepup_frame_round_trips_and_is_unknown_to_v3_kinds() -> TestResult {
    let challenge: StepUpChallenge = serde_json::from_str(CHALLENGE_JSON)?;
    let frame = SessionFrame::StepupRequest(challenge.clone());
    let value = serde_json::to_value(&frame)?;
    assert_eq!(value.get("kind"), Some(&Value::from("stepup_request")));
    assert_eq!(
        value.get("data"),
        Some(&serde_json::from_str::<Value>(CHALLENGE_JSON)?)
    );
    let back: SessionFrame = serde_json::from_value(value)?;
    assert!(matches!(back, SessionFrame::StepupRequest(ref c) if *c == challenge));

    let future: SessionFrame = serde_json::from_str(r#"{"kind":"stepup_request_v9","data":{}}"#)?;
    assert!(matches!(future, SessionFrame::Unknown));
    Ok(())
}

/// Merges `"<key>":<value>` into a JSON object text and reports whether decoding fails.
fn rejects_extra<T: DeserializeOwned>(base: &str, key: &str, value: &str) -> TestResult<bool> {
    let mut object: Value = serde_json::from_str(base)?;
    let map = object.as_object_mut().ok_or("base is not an object")?;
    map.insert(key.to_owned(), serde_json::from_str(value)?);
    Ok(serde_json::from_value::<T>(object).is_err())
}

#[test]
fn minor4_params_reject_unknown_fields() -> TestResult {
    let tenant = || -> TestResult<[bool; 9]> {
        Ok([
            rejects_extra::<CommandsListParams>("{}", "tenant", r#""tenant-test""#)?,
            rejects_extra::<CommandRunParams>(
                r#"{"line":"/status","client_msg_id":"m-1"}"#,
                "tenant",
                r#""tenant-test""#,
            )?,
            rejects_extra::<CatalogGetParams>("{}", "tenant", r#""tenant-test""#)?,
            rejects_extra::<ConfigGetParams>("{}", "tenant", r#""tenant-test""#)?,
            rejects_extra::<ConfigSetParams>(
                r#"{"key":"effort","value":"high","client_msg_id":"m-1"}"#,
                "tenant",
                r#""tenant-test""#,
            )?,
            rejects_extra::<ConfigResetParams>(
                r#"{"key":"effort","client_msg_id":"m-1"}"#,
                "tenant",
                r#""tenant-test""#,
            )?,
            rejects_extra::<SecretPutParams>(
                r#"{"name":"k","for_provider":"p","value":"fake-SECRET-marker"}"#,
                "tenant",
                r#""tenant-test""#,
            )?,
            rejects_extra::<StepUpConfirmParams>(
                r#"{"stepup_id":"a","hash":"b"}"#,
                "tenant",
                r#""tenant-test""#,
            )?,
            rejects_extra::<StepUpCancelParams>(
                r#"{"stepup_id":"a"}"#,
                "tenant",
                r#""tenant-test""#,
            )?,
        ])
    };
    assert_eq!(tenant()?, [true; 9]);
    assert!(rejects_extra::<CommandRunParams>(
        r#"{"line":"/status","client_msg_id":"m-1"}"#,
        "tier",
        r#""owner""#
    )?);
    assert!(rejects_extra::<StepUpConfirmParams>(
        r#"{"stepup_id":"a","hash":"b"}"#,
        "actor",
        r#""x""#
    )?);
    Ok(())
}

#[test]
fn minor4_params_accept_plain_shape() -> TestResult {
    let list: CommandsListParams = serde_json::from_str("{}")?;
    assert_eq!(list.session_id, None);
    let catalog: CatalogGetParams = serde_json::from_str("{}")?;
    assert_eq!(catalog.session_id, None);
    let config: ConfigGetParams = serde_json::from_str("{}")?;
    assert_eq!(config.keys, None);
    let run: CommandRunParams =
        serde_json::from_str(r#"{"line":"/status","client_msg_id":"m-1"}"#)?;
    assert_eq!(run.line, "/status");
    assert_eq!(run.client_msg_id, "m-1");
    assert_eq!(run.session_id, None);
    Ok(())
}

#[test]
fn minor4_results_accept_additional_fields() -> TestResult {
    let entry = r#"{"name":"status","aliases":[],"summary":"s","usage":"/status","domain":"session","permission":"observer","busy":"immediate","origin":"operation","remote":true,"available":true,"subcommands":[],"future":1}"#;
    let parsed: CommandEntry = serde_json::from_str(entry)?;
    assert_eq!(parsed.origin, CommandOrigin::Operation);
    assert_eq!(parsed.reason, None);

    let list = format!(r#"{{"commands":[{entry}],"rev":"0000000000000001","future":1}}"#);
    let parsed: CommandsListResult = serde_json::from_str(&list)?;
    assert_eq!(parsed.commands.len(), 1);

    let run = r#"{"ok":true,"text":"done","effect":"applied","future":1}"#;
    let parsed: CommandRunResult = serde_json::from_str(run)?;
    assert_eq!(parsed.effect, CommandEffect::Applied);
    assert_eq!(parsed.stepup, None);

    let provider = r#"{"name":"p","api":"a","base_url":"https://api.example.test/v1","enabled":true,"auth_kind":"env","auth_name":"X","has_secret":false,"models":[],"max_concurrency":null,"future":1}"#;
    let parsed: CatalogProvider = serde_json::from_str(provider)?;
    assert_eq!(parsed.auth_kind, ProviderAuthKind::Env);

    let catalog = format!(
        r#"{{"providers":[{provider}],"models":[],"roles":[],"live":{{}},"note":"n","future":1}}"#
    );
    let parsed: CatalogResult = serde_json::from_str(&catalog)?;
    assert_eq!(parsed.providers.len(), 1);

    let config = r#"{"entries":[{"key":"effort","value":"high","choices":[],"scope":"profile","apply":"now","future":1}],"future":1}"#;
    let parsed: ConfigGetResult = serde_json::from_str(config)?;
    assert_eq!(parsed.entries.len(), 1);

    let put = r#"{"handle":"00000000000000000000000000000001","key_name":"k","for_provider":"p","expires_at":"2026-01-01T00:03:00Z","future":1}"#;
    let parsed: SecretPutResult = serde_json::from_str(put)?;
    assert_eq!(parsed.key_name, "k");

    let challenge = CHALLENGE_JSON.replace('}', r#","future":1}"#);
    let parsed: StepUpChallenge = serde_json::from_str(&challenge)?;
    assert_eq!(parsed.action, "config.set");
    Ok(())
}

#[test]
fn minor4_enums_decode_unknown() -> TestResult {
    assert_eq!(
        serde_json::from_str::<CommandOrigin>(r#""future""#)?,
        CommandOrigin::Unknown
    );
    assert_eq!(
        serde_json::from_str::<CommandEffect>(r#""future""#)?,
        CommandEffect::Unknown
    );
    assert_eq!(
        serde_json::from_str::<ProviderAuthKind>(r#""future""#)?,
        ProviderAuthKind::Unknown
    );
    Ok(())
}

#[test]
fn command_effect_names_are_stable() -> TestResult {
    for (effect, name) in [
        (CommandEffect::Applied, "applied"),
        (CommandEffect::Staged, "staged"),
        (CommandEffect::NextSession, "next_session"),
        (CommandEffect::Refused, "refused"),
        (CommandEffect::StepupRequired, "stepup_required"),
    ] {
        assert_eq!(serde_json::to_value(effect)?, Value::from(name));
    }
    Ok(())
}

#[test]
fn wire_secret_debug_is_redacted() -> TestResult {
    let params = SecretPutParams {
        name: "k".to_owned(),
        for_provider: "p".to_owned(),
        value: WireSecret::new("sk-test-FAKE-0000000000000000".to_owned()),
    };
    let debug = format!("{params:?}");
    assert!(!debug.contains("FAKE"), "{debug}");
    assert!(debug.contains("[redacted]"), "{debug}");
    // The wire needs the value.
    assert!(serde_json::to_string(&params)?.contains("sk-test-FAKE-0000000000000000"));

    let result_json = r#"{"handle":"00000000000000000000000000000001","key_name":"k","for_provider":"p","expires_at":"2026-01-01T00:03:00Z"}"#;
    let result: SecretPutResult = serde_json::from_str(result_json)?;
    let value = serde_json::to_value(&result)?;
    assert_eq!(
        sorted_keys(&value)?,
        ["expires_at", "for_provider", "handle", "key_name"]
    );
    assert!(!serde_json::to_string(&result)?.contains("FAKE"));
    Ok(())
}

#[test]
fn catalog_shapes_have_no_secret_fields() -> TestResult {
    let provider = CatalogProvider {
        name: "p".to_owned(),
        api: "a".to_owned(),
        base_url: "https://api.example.test/v1".to_owned(),
        enabled: true,
        auth_kind: ProviderAuthKind::Secrets,
        auth_name: Some("k".to_owned()),
        has_secret: true,
        models: vec!["m".to_owned()],
        max_concurrency: Some(2),
    };
    let value = serde_json::to_value(&provider)?;
    let keys = sorted_keys(&value)?;
    assert_eq!(
        keys,
        [
            "api",
            "auth_kind",
            "auth_name",
            "base_url",
            "enabled",
            "has_secret",
            "max_concurrency",
            "models",
            "name"
        ]
    );
    for forbidden in ["api_key", "headers", "auth_header", "auth"] {
        assert!(!keys.iter().any(|k| k == forbidden), "{forbidden}");
    }
    Ok(())
}

#[test]
fn client_msg_id_limit_constant() {
    assert_eq!(MAX_CLIENT_MSG_ID_BYTES, 128);
    assert_eq!(MAX_COMMAND_LINE_BYTES, 512);
    assert_eq!(MAX_SECRET_VALUE_BYTES, 8194);
    assert_eq!(MAX_CONFIG_KEYS, 32);
}
