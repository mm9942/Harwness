//! Session control plane wire vocabulary (PL-65 §3, W00 contract §2.1).
//!
//! Transport independent: the same types travel over an in-process
//! [`crate::session_port::SessionPort`], a WebSocket on a local Unix socket
//! and a WebSocket over the authenticated node transport. A transport never
//! changes their meaning.
//!
//! Rules that hold for every type here:
//! - Request parameters use `deny_unknown_fields`, so a client cannot smuggle
//!   identity (actor, tenant, principal, uid) into a request. Identity comes
//!   from the transport, never from a payload.
//! - [`SessionFrame`] is additive: an unknown `kind` decodes to
//!   [`SessionFrame::Unknown`] so older clients render a neutral line instead
//!   of dropping the stream.

use harw_types::{ApprovalId, DeviceId, ReviewDecision, SessionId, TenantId, TurnId};
use serde::{Deserialize, Serialize};

use crate::approvals::ApprovalRequest;
use crate::events::{SessionEvent, TurnEvent};

/// Minor version of the session wire. Major stays at the
/// [`crate::ProtocolVersion`] major (1).
pub const SESSION_WIRE_MINOR: u32 = 1;

/// Exact WebSocket subprotocol for the session control plane.
pub const SESSION_WS_SUBPROTOCOL: &str = "harw.session.v1";

/// HTTP path of the WebSocket upgrade endpoint.
pub const SESSION_WS_PATH: &str = "/v1/session-ws";

/// Feature names a host may grant in [`HelloAck::features`].
pub mod features {
    /// The host honours [`super::StreamProfile::Compact`].
    pub const COMPACT: &str = "compact";
    /// The host serves `session.history` paging.
    pub const HISTORY: &str = "history";
    /// The host emits [`super::SessionFrame::Child`] frames.
    pub const CHILD_FRAMES: &str = "child_frames";
}

/// Position in a session stream.
///
/// `generation` changes when the durable transcript is rewritten (retention),
/// `durable` is the transcript `sequence` of the next record the client has
/// not seen, `live` indexes the live ring of the running turn.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub generation: u32,
    pub durable: u64,
    pub live: u32,
}

impl Cursor {
    /// Cursor at the start of a transcript generation.
    #[must_use]
    pub const fn start(generation: u32) -> Self {
        Self {
            generation,
            durable: 0,
            live: 0,
        }
    }

    /// True when `self` has seen durable state beyond `other` in the same
    /// generation, or `other` belongs to an older generation.
    #[must_use]
    pub fn is_durably_ahead_of(&self, other: &Self) -> bool {
        self.generation > other.generation
            || (self.generation == other.generation && self.durable > other.durable)
    }
}

/// Capabilities of one client on one host. Effective caps are always an
/// intersection, never a union (see [`ClientCaps::intersect`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientCaps {
    /// attach, history, presence.
    pub observe: bool,
    /// `turn.submit`, `turn.interrupt`, `session.resume`.
    pub steer: bool,
    /// `approval.respond`.
    pub approve: bool,
    /// settings, `session.close`, creating sessions for others.
    pub control: bool,
}

impl ClientCaps {
    /// No capability at all.
    pub const NONE: Self = Self {
        observe: false,
        steer: false,
        approve: false,
        control: false,
    };

    /// Watch only.
    pub const OBSERVE: Self = Self {
        observe: true,
        steer: false,
        approve: false,
        control: false,
    };

    /// Observe, steer and approve (operator).
    pub const OPERATE: Self = Self {
        observe: true,
        steer: true,
        approve: true,
        control: false,
    };

    /// Every capability.
    pub const ALL: Self = Self {
        observe: true,
        steer: true,
        approve: true,
        control: true,
    };

    /// Capabilities present in both sets. Requested caps can only narrow.
    #[must_use]
    pub const fn intersect(self, other: Self) -> Self {
        Self {
            observe: self.observe && other.observe,
            steer: self.steer && other.steer,
            approve: self.approve && other.approve,
            control: self.control && other.control,
        }
    }

    /// True when every capability in `self` is also in `ceiling`.
    #[must_use]
    pub const fn is_within(self, ceiling: Self) -> bool {
        (!self.observe || ceiling.observe)
            && (!self.steer || ceiling.steer)
            && (!self.approve || ceiling.approve)
            && (!self.control || ceiling.control)
    }
}

/// How much live traffic a client wants.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamProfile {
    /// Every frame.
    #[default]
    Full,
    /// Phone profile: no reasoning/child deltas, coalesced assistant text.
    Compact,
}

/// Durable state of a hosted session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HostedState {
    Idle,
    Running,
    WaitingForApproval,
    WaitingForChild,
    Queued { depth: u32 },
    Interrupted,
    Failed,
    Closed,
}

impl HostedState {
    /// True for states in which no further turn may start.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Closed)
    }
}

/// One attached client as other clients see it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresenceEntry {
    pub label: String,
    pub device: Option<DeviceId>,
    pub caps: ClientCaps,
    pub since: jiff::Timestamp,
}

/// One frame of a session stream.
///
/// The derive uses `remote = "Self"` so the generated (strict) serde code
/// becomes inherent functions; the trait impls below wrap them and map an
/// unknown `kind` to [`SessionFrame::Unknown`]. Plain `#[serde(other)]` does
/// not work here because an adjacently tagged unit variant rejects `data`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    content = "data",
    rename_all = "snake_case"
)]
pub enum SessionFrame {
    /// A turn event of the root session.
    Turn(TurnEvent),
    /// A session lifecycle event.
    Session(SessionEvent),
    /// A turn event of a child agent.
    Child {
        agent: SessionId,
        parent: Option<SessionId>,
        role: String,
        event: TurnEvent,
    },
    /// A pending approval (from the approval store, never from the transcript).
    ApprovalRequested(ApprovalRequest),
    /// An approval was decided. `by` is the host-derived actor label.
    ApprovalResolved {
        request_id: ApprovalId,
        decision: ReviewDecision,
        by: String,
    },
    /// Job state change of a job this session enqueued.
    Job {
        work_id: String,
        kind: String,
        state: String,
        summary: Option<String>,
    },
    /// Who is attached right now.
    Presence { attached: Vec<PresenceEntry> },
    /// Partial turn text after the live ring lost the client's position.
    Snapshot {
        turn_id: TurnId,
        assistant_text: String,
        reasoning_collapsed: bool,
    },
    /// The durable history was replaced (compaction marker).
    HistoryReplaced { item_count: u32 },
    /// The client must drop its view and re-attach from `head`.
    Resync { reason: String, head: Cursor },
    /// The attachment overflowed; re-attach from `resume_from`.
    Lagged { resume_from: Cursor },
    /// The host is shutting down; reconnect after the delay.
    HostDraining { retry_after_ms: u64 },
    /// The client's authority was revoked; the stream ends.
    Revoked,
    /// Keep-alive.
    Heartbeat,
    /// A frame kind this client does not know (additive evolution).
    Unknown,
}

/// Every `kind` tag this version understands. Unknown tags decode to
/// [`SessionFrame::Unknown`]; known tags decode strictly.
const KNOWN_FRAME_KINDS: &[&str] = &[
    "turn",
    "session",
    "child",
    "approval_requested",
    "approval_resolved",
    "job",
    "presence",
    "snapshot",
    "history_replaced",
    "resync",
    "lagged",
    "host_draining",
    "revoked",
    "heartbeat",
    "unknown",
];

impl SessionFrame {
    /// The wire `kind` tag of this frame.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Turn(_) => "turn",
            Self::Session(_) => "session",
            Self::Child { .. } => "child",
            Self::ApprovalRequested(_) => "approval_requested",
            Self::ApprovalResolved { .. } => "approval_resolved",
            Self::Job { .. } => "job",
            Self::Presence { .. } => "presence",
            Self::Snapshot { .. } => "snapshot",
            Self::HistoryReplaced { .. } => "history_replaced",
            Self::Resync { .. } => "resync",
            Self::Lagged { .. } => "lagged",
            Self::HostDraining { .. } => "host_draining",
            Self::Revoked => "revoked",
            Self::Heartbeat => "heartbeat",
            Self::Unknown => "unknown",
        }
    }
}

impl Serialize for SessionFrame {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Self::serialize(self, serializer)
    }
}

impl<'de> Deserialize<'de> for SessionFrame {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let known = value
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| KNOWN_FRAME_KINDS.contains(&kind));
        if !known {
            if value
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .is_none()
            {
                return Err(serde::de::Error::custom(
                    "session frame without a string `kind`",
                ));
            }
            return Ok(Self::Unknown);
        }
        Self::deserialize(value).map_err(serde::de::Error::custom)
    }
}

impl SessionFrame {
    /// Live-only frames may be coalesced or dropped under pressure; the
    /// client recovers them through `Resync`/`Lagged`. Everything else
    /// (approvals, revocation, drain, durable state) must never be dropped
    /// silently.
    #[must_use]
    pub fn is_disposable(&self) -> bool {
        match self {
            Self::Turn(event) | Self::Child { event, .. } => matches!(
                event,
                TurnEvent::AssistantDelta { .. }
                    | TurnEvent::ReasoningDelta { .. }
                    | TurnEvent::UsageUpdated { .. }
                    | TurnEvent::ContextUpdated { .. }
                    | TurnEvent::ChildProgress { .. }
            ),
            Self::Heartbeat | Self::Presence { .. } => true,
            _ => false,
        }
    }
}

/// A frame with its session and stream position.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameEnvelope {
    pub session_id: SessionId,
    pub cursor: Cursor,
    pub frame: SessionFrame,
}

/// Summary of a hosted session for list/attach.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub title: Option<String>,
    pub tenant: Option<TenantId>,
    pub state: HostedState,
    pub attached: u32,
    pub updated_at: jiff::Timestamp,
    pub model: Option<String>,
}

// ---------------------------------------------------------------------------
// Method params and results
// ---------------------------------------------------------------------------

/// `session.hello`: must succeed before any other method.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloParams {
    pub client_label: String,
    pub wire_minor: u32,
    #[serde(default)]
    pub features: Vec<String>,
    /// Caps the client asks for. The host grants at most its ceiling; `None`
    /// asks for the full ceiling.
    #[serde(default)]
    pub requested_caps: Option<ClientCaps>,
}

/// Result of `session.hello`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloAck {
    pub wire_minor: u32,
    pub features: Vec<String>,
    pub host_epoch: u64,
    pub granted: ClientCaps,
}

/// Result of `session.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListResult {
    pub sessions: Vec<SessionSummary>,
}

/// `session.create`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateParams {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// Default number of durable items sent on an attach without a cursor.
pub const DEFAULT_TAIL_ITEMS: u32 = 200;

const fn default_tail_items() -> u32 {
    DEFAULT_TAIL_ITEMS
}

/// `session.attach`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachParams {
    pub session_id: SessionId,
    #[serde(default)]
    pub from: Option<Cursor>,
    #[serde(default)]
    pub profile: StreamProfile,
    #[serde(default = "default_tail_items")]
    pub tail_items: u32,
}

/// Result of `session.attach`. Frames follow as `event.frame` notifications.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachAck {
    pub session: SessionSummary,
    pub granted: ClientCaps,
    pub head: Cursor,
    pub replay_from: Cursor,
    pub host_epoch: u64,
}

/// `session.detach`, `session.resume` and `session.close`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRef {
    pub session_id: SessionId,
}

/// `session.history`: page older durable frames.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryParams {
    pub session_id: SessionId,
    pub before: Cursor,
    pub limit: u32,
}

/// Result of `session.history`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryResult {
    pub frames: Vec<FrameEnvelope>,
}

/// `turn.submit`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitParams {
    pub session_id: SessionId,
    pub text: String,
    /// Durable head the client has seen (compare-and-swap).
    pub expect_head: Cursor,
    /// Idempotency key across reconnects.
    pub client_msg_id: String,
    /// "I have seen the newer head": skip the stale check.
    #[serde(default)]
    pub force: bool,
}

/// Result of `turn.submit`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum SubmitResult {
    /// Queued at `position` (0 = runs now).
    Accepted { position: u32 },
    /// The durable head moved past `expect_head`.
    Stale { head: Cursor },
    /// The per-session queue is full.
    QueueFull,
    /// Not allowed (caps, tenant, state).
    Denied { reason: String },
}

/// `turn.interrupt`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterruptParams {
    pub session_id: SessionId,
    #[serde(default)]
    pub turn_id: Option<TurnId>,
}

/// `approval.respond`. Deliberately has **no** actor field: the host derives
/// the actor from the authenticated connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRespondParams {
    pub request_id: ApprovalId,
    pub decision: ReviewDecision,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Result of `approval.respond`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum RespondResult {
    Resolved,
    AlreadyResolved { by: String },
    Expired,
    Denied { reason: String },
}

/// `session.set_model`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetModelParams {
    pub session_id: SessionId,
    pub model: String,
}

/// `session.set_mode`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetModeParams {
    pub session_id: SessionId,
    pub mode: String,
}

/// `session.set_effort`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetEffortParams {
    pub session_id: SessionId,
    pub effort: String,
}

/// Stable application error codes carried in [`crate::WireError::code`].
pub mod error_codes {
    /// A method other than `session.hello` arrived before hello.
    pub const HELLO_REQUIRED: i32 = -32001;
    /// Capability or tenant admission refused the call.
    pub const DENIED: i32 = -32002;
    /// Unknown session or approval.
    pub const NOT_FOUND: i32 = -32003;
    /// The client's authority was revoked.
    pub const REVOKED: i32 = -32004;
    /// Too many requests in flight on this connection.
    pub const BUSY: i32 = -32005;
    /// JSON-RPC: method not found.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// JSON-RPC: invalid params.
    pub const INVALID_PARAMS: i32 = -32602;
    /// JSON-RPC: parse error / malformed message.
    pub const PARSE_ERROR: i32 = -32700;
    /// JSON-RPC: internal error.
    pub const INTERNAL: i32 = -32603;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx as with_context};

    fn ctx<T, E: std::fmt::Display>(result: Result<T, E>, context: &'static str) -> TestResult<T> {
        result.map_err(with_context(context))
    }

    fn sid(value: &str) -> TestResult<SessionId> {
        ctx(SessionId::try_from_str(value), "session id")
    }

    #[test]
    fn unknown_frame_kind_decodes_to_unknown() -> TestResult {
        let json = r#"{"kind":"future_thing","data":{"x":1}}"#;
        let frame: SessionFrame = ctx(serde_json::from_str(json), "decode unknown kind")?;
        assert!(matches!(frame, SessionFrame::Unknown));
        let bare = r#"{"kind":"another_new_kind"}"#;
        let frame: SessionFrame = ctx(serde_json::from_str(bare), "decode bare unknown kind")?;
        assert!(matches!(frame, SessionFrame::Unknown));
        Ok(())
    }

    #[test]
    fn known_kind_with_bad_data_is_an_error_not_unknown() {
        let json = r#"{"kind":"lagged","data":{"nope":1}}"#;
        assert!(serde_json::from_str::<SessionFrame>(json).is_err());
        assert!(serde_json::from_str::<SessionFrame>(r#"{"data":1}"#).is_err());
    }

    #[test]
    fn kind_matches_the_serialized_tag() -> TestResult {
        let frames = [
            SessionFrame::Revoked,
            SessionFrame::Heartbeat,
            SessionFrame::Unknown,
            SessionFrame::HostDraining { retry_after_ms: 1 },
            SessionFrame::Lagged {
                resume_from: Cursor::default(),
            },
            SessionFrame::Resync {
                reason: "r".into(),
                head: Cursor::default(),
            },
            SessionFrame::HistoryReplaced { item_count: 3 },
            SessionFrame::Presence { attached: vec![] },
            SessionFrame::Job {
                work_id: "w".into(),
                kind: "work".into(),
                state: "running".into(),
                summary: None,
            },
        ];
        for frame in frames {
            let value = ctx(serde_json::to_value(&frame), "encode")?;
            assert_eq!(value["kind"], frame.kind());
            assert!(KNOWN_FRAME_KINDS.contains(&frame.kind()));
            let back: SessionFrame = ctx(serde_json::from_value(value), "decode")?;
            assert_eq!(back.kind(), frame.kind());
        }
        Ok(())
    }

    #[test]
    fn frame_envelope_round_trips() -> TestResult {
        let envelope = FrameEnvelope {
            session_id: sid("s-1")?,
            cursor: Cursor {
                generation: 2,
                durable: 7,
                live: 3,
            },
            frame: SessionFrame::Lagged {
                resume_from: Cursor::start(2),
            },
        };
        let json = ctx(serde_json::to_string(&envelope), "encode")?;
        let back: FrameEnvelope = ctx(serde_json::from_str(&json), "decode")?;
        assert_eq!(back.cursor, envelope.cursor);
        assert!(
            matches!(back.frame, SessionFrame::Lagged { resume_from } if resume_from == Cursor::start(2))
        );
        Ok(())
    }

    #[test]
    fn approval_respond_params_reject_an_actor_field() {
        let json = r#"{"request_id":"a-1","decision":"approved","actor":"uid:0"}"#;
        assert!(serde_json::from_str::<ApprovalRespondParams>(json).is_err());
    }

    #[test]
    fn submit_params_reject_identity_fields() {
        let json = r#"{"session_id":"s","text":"hi","expect_head":{"generation":0,"durable":0,"live":0},"client_msg_id":"m","tenant":"other"}"#;
        assert!(serde_json::from_str::<SubmitParams>(json).is_err());
    }

    #[test]
    fn caps_intersect_only_narrows() {
        let requested = ClientCaps::ALL;
        let ceiling = ClientCaps::OPERATE;
        let granted = requested.intersect(ceiling);
        assert_eq!(granted, ClientCaps::OPERATE);
        assert!(granted.is_within(ceiling));
        assert!(!ClientCaps::ALL.is_within(ClientCaps::OBSERVE));
        assert!(ClientCaps::NONE.is_within(ClientCaps::NONE));
    }

    #[test]
    fn cursor_ordering_respects_generation() {
        let old = Cursor {
            generation: 1,
            durable: 100,
            live: 0,
        };
        let new = Cursor {
            generation: 2,
            durable: 0,
            live: 0,
        };
        assert!(new.is_durably_ahead_of(&old));
        assert!(!old.is_durably_ahead_of(&new));
        let later = Cursor {
            durable: 101,
            ..old
        };
        assert!(later.is_durably_ahead_of(&old));
        assert!(!old.is_durably_ahead_of(&old));
    }

    #[test]
    fn disposable_frames_are_only_live_deltas() -> TestResult {
        let turn_id = ctx(TurnId::try_from_str("t-1"), "turn id")?;
        let delta = SessionFrame::Turn(TurnEvent::AssistantDelta {
            turn_id: turn_id.clone(),
            text: "x".into(),
        });
        assert!(delta.is_disposable());
        assert!(!SessionFrame::Revoked.is_disposable());
        assert!(!SessionFrame::HostDraining { retry_after_ms: 10 }.is_disposable());
        assert!(!SessionFrame::Turn(TurnEvent::TurnAborted { turn_id }).is_disposable());
        Ok(())
    }

    #[test]
    fn attach_params_default_tail_and_profile() -> TestResult {
        let json = r#"{"session_id":"s-9"}"#;
        let params: AttachParams = ctx(serde_json::from_str(json), "decode attach")?;
        assert_eq!(params.tail_items, DEFAULT_TAIL_ITEMS);
        assert_eq!(params.profile, StreamProfile::Full);
        assert_eq!(params.from, None);
        Ok(())
    }

    #[test]
    fn submit_result_wire_shape_is_tagged() -> TestResult {
        let json = ctx(
            serde_json::to_string(&SubmitResult::Accepted { position: 0 }),
            "encode",
        )?;
        assert_eq!(json, r#"{"result":"accepted","position":0}"#);
        Ok(())
    }
}
