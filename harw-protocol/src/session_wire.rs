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
//!
//! Wire minor 2 adds the R18 tool gateway vocabulary (contract
//! `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md`):
//! `tool.*` methods for agent principals, `gateway.*` methods for gateway
//! inspection/administration and the caps `tool_call`, `gateway_read`,
//! `gateway_admin`. There is still no generic operation execution (W00 D5).

use harw_types::{ApprovalId, DeviceId, ReviewDecision, SessionId, TenantId, ToolCallId, TurnId};
use serde::{Deserialize, Serialize};

use crate::approvals::ApprovalRequest;
use crate::events::{SessionEvent, TurnEvent};
use crate::items::{ResultTrust, ToolCallResult, ToolPlacement};

/// Minor version of the session wire. Major stays at the
/// [`crate::ProtocolVersion`] major (1).
///
/// - 1: W00 session control plane.
/// - 2: R18 tool gateway (`tool.*`, `gateway.*`, caps `tool_call`,
///   `gateway_read`, `gateway_admin`).
pub const SESSION_WIRE_MINOR: u32 = 2;

/// First wire minor that knows the R18 caps and methods. A host masks the
/// R18 caps out of every grant negotiated below this minor
/// ([`ClientCaps::for_wire_minor`]), so a minor-1 client never sees a caps
/// field it cannot decode.
pub const TOOL_GATEWAY_WIRE_MINOR: u32 = 2;

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
    /// The host serves `tool.list`/`tool.call`/`tool.cancel` (R18 D-A).
    pub const TOOLS: &str = "tools";
    /// The host serves the `gateway.*` methods (R18 D-B).
    pub const GATEWAY: &str = "gateway";
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

// serde `skip_serializing_if` needs a `fn(&T) -> bool`.
const fn is_false(value: &bool) -> bool {
    !*value
}

/// Capabilities of one client on one host. Effective caps are always an
/// intersection, never a union (see [`ClientCaps::intersect`]).
///
/// The three R18 caps (`tool_call`, `gateway_read`, `gateway_admin`) are
/// `serde(default)` and are not serialized while `false`, so the minor-1
/// wire shape stays byte-identical for every client that is not granted
/// one of them.
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
    /// `tool.list`, `tool.call`, `tool.cancel` (R18 D-A). Admission
    /// additionally requires an agent principal with a tool grant; the cap
    /// alone never admits a call. Never part of a tier ceiling.
    #[serde(default, skip_serializing_if = "is_false")]
    pub tool_call: bool,
    /// Read-only `gateway.*` methods (R18 D-B).
    #[serde(default, skip_serializing_if = "is_false")]
    pub gateway_read: bool,
    /// Mutating `gateway.*` methods (R18 D-B). Implies nothing about
    /// `gateway_read`; a host grants both explicitly.
    #[serde(default, skip_serializing_if = "is_false")]
    pub gateway_admin: bool,
}

impl ClientCaps {
    /// No capability at all.
    pub const NONE: Self = Self {
        observe: false,
        steer: false,
        approve: false,
        control: false,
        tool_call: false,
        gateway_read: false,
        gateway_admin: false,
    };

    /// Watch only.
    pub const OBSERVE: Self = Self {
        observe: true,
        ..Self::NONE
    };

    /// Observe, steer and approve (operator).
    pub const OPERATE: Self = Self {
        observe: true,
        steer: true,
        approve: true,
        ..Self::NONE
    };

    /// Every W00 session capability (observe, steer, approve, control).
    ///
    /// Deliberately **without** the R18 caps: tier ceilings are built from
    /// this constant, and a human tier must never carry `tool_call`. Gateway
    /// caps are added to a ceiling explicitly ([`Self::with`]).
    pub const ALL: Self = Self {
        observe: true,
        steer: true,
        approve: true,
        control: true,
        ..Self::NONE
    };

    /// Only `tool_call` (R18 D-A).
    pub const TOOL_CALL: Self = Self {
        tool_call: true,
        ..Self::NONE
    };

    /// Only `gateway_read` (R18 D-B).
    pub const GATEWAY_READ: Self = Self {
        gateway_read: true,
        ..Self::NONE
    };

    /// `gateway_read` and `gateway_admin` (R18 D-B).
    pub const GATEWAY_ADMIN: Self = Self {
        gateway_read: true,
        gateway_admin: true,
        ..Self::NONE
    };

    /// Capabilities present in both sets. Requested caps can only narrow.
    #[must_use]
    pub const fn intersect(self, other: Self) -> Self {
        Self {
            observe: self.observe && other.observe,
            steer: self.steer && other.steer,
            approve: self.approve && other.approve,
            control: self.control && other.control,
            tool_call: self.tool_call && other.tool_call,
            gateway_read: self.gateway_read && other.gateway_read,
            gateway_admin: self.gateway_admin && other.gateway_admin,
        }
    }

    /// Adds `other` to a host-side **ceiling**.
    ///
    /// Only for composing a ceiling from fixed constants when a listener
    /// builds an identity (e.g. `caps_for_tier(tier).with(GATEWAY_READ)`).
    /// Never apply it to client-requested caps: effective caps stay
    /// `requested ∩ ceiling`.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self {
            observe: self.observe || other.observe,
            steer: self.steer || other.steer,
            approve: self.approve || other.approve,
            control: self.control || other.control,
            tool_call: self.tool_call || other.tool_call,
            gateway_read: self.gateway_read || other.gateway_read,
            gateway_admin: self.gateway_admin || other.gateway_admin,
        }
    }

    /// Caps as they may be granted at the negotiated wire `minor`: below
    /// [`TOOL_GATEWAY_WIRE_MINOR`] the R18 caps are masked out, so an older
    /// client never receives a caps field it cannot decode and never holds
    /// a right it cannot name.
    #[must_use]
    pub const fn for_wire_minor(self, minor: u32) -> Self {
        if minor >= TOOL_GATEWAY_WIRE_MINOR {
            return self;
        }
        Self {
            tool_call: false,
            gateway_read: false,
            gateway_admin: false,
            ..self
        }
    }

    /// True when every capability in `self` is also in `ceiling`.
    #[must_use]
    pub const fn is_within(self, ceiling: Self) -> bool {
        (!self.observe || ceiling.observe)
            && (!self.steer || ceiling.steer)
            && (!self.approve || ceiling.approve)
            && (!self.control || ceiling.control)
            && (!self.tool_call || ceiling.tool_call)
            && (!self.gateway_read || ceiling.gateway_read)
            && (!self.gateway_admin || ceiling.gateway_admin)
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

// ---------------------------------------------------------------------------
// R18 tool gateway: `tool.*` (D-A) and `gateway.*` (D-B)
// ---------------------------------------------------------------------------

/// Organizational role of an agent principal on the wire (R18 §3).
///
/// Mirrors `harw_agent_dsl::roles::AgentRoleId` value for value (same
/// kebab-case names); `harw-protocol` cannot depend on the DSL crate, so the
/// mapping lives at the composition root and is pinned by a test there. An
/// unknown role decodes to [`AgentRole::Unknown`], and admission treats
/// `Unknown` like "no agent" (fail closed).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentRole {
    /// The User Interface Agent: the only role that holds `tool.call`
    /// rights of its own.
    UserInterface,
    RootOrchestrator,
    ChildOrchestrator,
    Worker,
    UiaWorker,
    AgentSteward,
    /// A role this version does not know. Never admitted to `tool.*`.
    #[serde(other)]
    Unknown,
}

/// Whether the gateway asks a human before running a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolApproval {
    /// Runs without asking (read-only tools, free `gateway.*` reads).
    Never,
    /// The gateway approval policy decides per call (scope, effect, risk,
    /// auto mode).
    Policy,
    /// Every call asks (`approval = "always"`, e.g. `gateway.*` mutations).
    Always,
    /// A requirement this version does not know. A client must treat it
    /// like [`ToolApproval::Always`] when it renders or plans.
    #[serde(other)]
    Unknown,
}

/// One tool as the gateway offers it to an agent principal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDescriptor {
    /// Stable tool name (`fs.read`, `shell.exec`, `gateway.status`, ...).
    pub name: String,
    /// Model-facing description.
    pub description: String,
    /// JSON schema of the `arguments` object.
    pub input_schema: serde_json::Value,
    /// Approval requirement applied by the gateway.
    pub approval: ToolApproval,
    /// Where calls of this tool run. Always [`ToolPlacement::Gateway`] for
    /// tools served by the gateway tool host.
    pub placement: ToolPlacement,
    /// Independent calls may run concurrently.
    #[serde(default)]
    pub parallel_safe: bool,
}

/// `tool.list`: the tools the calling agent principal may call in `session_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolListParams {
    pub session_id: SessionId,
}

/// Result of `tool.list`: exactly the caller's granted tool set, never more.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolListResult {
    pub tools: Vec<ToolDescriptor>,
}

/// `tool.call`: run one tool in the gateway tool host.
///
/// Carries no identity: the principal, its role, parent and grant come from
/// the connection ([`crate::session_port::ToolPort`] implementations read
/// them from the authenticated identity).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallParams {
    /// Session the call belongs to; must be bound to the calling principal.
    pub session_id: SessionId,
    /// Turn the call belongs to (execution context, event correlation).
    pub turn_id: TurnId,
    /// Model-issued call id; unique per session while in flight.
    pub call_id: ToolCallId,
    pub tool_name: String,
    /// Tool arguments as the model produced them. Validated against the
    /// tool's `input_schema` by the gateway; a schema violation is a
    /// completed call with [`ToolCallResult::Error`], not a wire error.
    pub arguments: serde_json::Value,
    /// Call of the parent agent this call is delegated under (UIA
    /// delegation chain), if any.
    #[serde(default)]
    pub parent_call_id: Option<ToolCallId>,
}

/// Result of `tool.call`: the call ran (successfully or not).
///
/// Refusals before execution (unknown tool, not granted, sandbox
/// unavailable, draining, duplicate call id) are wire errors
/// ([`error_codes`]), never a result frame, and never fall back to host
/// execution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallResultFrame {
    pub call_id: ToolCallId,
    pub result: ToolCallResult,
    /// Set by the gateway tool host; the caller copies it into
    /// `TurnEvent::ToolCallCompleted::placement`.
    pub placement: ToolPlacement,
    pub duration_ms: u64,
    /// Provenance of `result`; defaults to `Untrusted` (fail closed).
    #[serde(default)]
    pub trust: ResultTrust,
}

/// `tool.cancel`: cancel an in-flight `tool.call` of the same principal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCancelParams {
    pub session_id: SessionId,
    pub call_id: ToolCallId,
}

/// Result of `gateway.status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayStatus {
    pub host_epoch: u64,
    /// Gateway node label, when configured.
    #[serde(default)]
    pub node: Option<String>,
    /// The host is draining: new turns and tool calls are refused.
    pub draining: bool,
    pub connections: u32,
    pub sessions: u32,
    pub running_turns: u32,
    pub listeners: u32,
    /// Tools the gateway tool host serves (before per-principal grants).
    pub tools: u32,
    /// A sandbox backend for gateway-side tool execution is available.
    /// `false` means every `tool.call` is refused with
    /// [`error_codes::TOOL_SANDBOX_UNAVAILABLE`].
    pub sandbox_available: bool,
}

/// Who is behind a connection, as `gateway.connections.list` shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrincipalSummary {
    /// A human client (local uid or enrolled device).
    Device {
        #[serde(default)]
        device: Option<DeviceId>,
    },
    /// An agent principal (R18 §3).
    Agent {
        /// Gateway-issued agent principal id.
        agent: String,
        role: AgentRole,
        /// Agent principal id of the delegating parent, if any.
        #[serde(default)]
        parent: Option<String>,
    },
    /// A principal kind this version does not know.
    #[serde(other)]
    Unknown,
}

/// One live connection (`gateway.connections.list`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConnectionInfo {
    /// Process-unique connection id (the host's `ConnectionId`).
    pub connection: u64,
    pub label: String,
    pub principal: PrincipalSummary,
    #[serde(default)]
    pub tenant: Option<TenantId>,
    /// Granted caps after hello (`None` before hello).
    #[serde(default)]
    pub granted: Option<ClientCaps>,
    /// Number of attached sessions.
    pub attached: u32,
    pub since: jiff::Timestamp,
}

/// Result of `gateway.connections.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConnectionsResult {
    pub connections: Vec<GatewayConnectionInfo>,
}

/// Kind of a gateway listener.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListenerKind {
    /// AF_UNIX socket with `SO_PEERCRED` (W00 D3).
    LocalUds,
    /// WebSocket over the authenticated node transport (W00 D4).
    Node,
    #[serde(other)]
    Unknown,
}

/// One listener (`gateway.listeners.list`, `gateway.listeners.set`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayListenerInfo {
    /// Stable listener name from the gateway configuration.
    pub name: String,
    pub kind: ListenerKind,
    /// Socket path or node locator. Never contains a secret.
    pub address: String,
    pub enabled: bool,
}

/// Result of `gateway.listeners.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayListenersResult {
    pub listeners: Vec<GatewayListenerInfo>,
}

/// Effective tool grant of one agent principal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayToolRights {
    /// Agent principal id.
    pub agent: String,
    pub role: AgentRole,
    /// Granted tool names, sorted, without duplicates.
    pub tools: Vec<String>,
}

/// Result of `gateway.tools.list`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayToolsResult {
    /// Every tool the gateway tool host serves.
    pub tools: Vec<ToolDescriptor>,
    /// Current grants of the connected agent principals.
    pub grants: Vec<GatewayToolRights>,
}

/// `gateway.connections.revoke`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayRevokeParams {
    pub connection: u64,
    pub reason: String,
}

/// Result of `gateway.connections.revoke`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayRevokeResult {
    /// `false` when the connection was already gone.
    pub revoked: bool,
}

/// `gateway.drain`: stop admitting new turns and tool calls.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayDrainParams {
    /// Sent to clients in `SessionFrame::HostDraining`.
    pub retry_after_ms: u64,
}

/// `gateway.listeners.set`: enable or disable a configured listener.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayListenerSetParams {
    pub name: String,
    pub enabled: bool,
}

/// `gateway.tools.grant` (replace) and `gateway.tools.narrow` (remove).
///
/// `grant` replaces the agent's grant with `tools`, which must lie within
/// the agent's ceiling (its parent's grant, or the configured UIA ceiling);
/// a name outside the ceiling is refused, never silently dropped. `narrow`
/// removes `tools` from the current grant and always succeeds for known
/// agents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayToolRightsParams {
    /// Agent principal id.
    pub agent: String,
    pub tools: Vec<String>,
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
    /// R18: `tool.call` names a tool the gateway tool host does not serve.
    pub const TOOL_UNKNOWN: i32 = -32010;
    /// R18: the tool exists but is outside the caller's tool grant.
    pub const TOOL_NOT_GRANTED: i32 = -32011;
    /// R18: no gateway-side sandbox is available; the call is refused and
    /// never runs on the host instead.
    pub const TOOL_SANDBOX_UNAVAILABLE: i32 = -32012;
    /// R18: the host is draining and admits no new turn or tool call.
    pub const HOST_DRAINING: i32 = -32013;
    /// R18: `call_id` is already in flight in this session.
    pub const TOOL_CALL_DUPLICATE: i32 = -32014;
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
    fn r18_caps_are_never_part_of_all_and_mask_below_minor_two() {
        const { assert!(!ClientCaps::ALL.tool_call) };
        const { assert!(!ClientCaps::ALL.gateway_read) };
        const { assert!(!ClientCaps::ALL.gateway_admin) };
        let ceiling = ClientCaps::ALL.with(ClientCaps::GATEWAY_ADMIN);
        assert!(ceiling.gateway_read && ceiling.gateway_admin && ceiling.control);
        assert!(!ceiling.tool_call);
        // Requested caps still only narrow.
        let requested = ClientCaps::TOOL_CALL.with(ClientCaps::OBSERVE);
        let granted = requested.intersect(ceiling);
        assert_eq!(granted, ClientCaps::OBSERVE);
        assert!(!ClientCaps::TOOL_CALL.is_within(ClientCaps::ALL));
        assert!(ClientCaps::GATEWAY_READ.is_within(ceiling));
        // Minor 1 never sees an R18 cap.
        let agent = ClientCaps::OPERATE.with(ClientCaps::TOOL_CALL);
        assert_eq!(agent.for_wire_minor(1), ClientCaps::OPERATE);
        assert_eq!(agent.for_wire_minor(TOOL_GATEWAY_WIRE_MINOR), agent);
    }

    #[test]
    fn r18_caps_keep_the_minor_one_shape_when_false() -> TestResult {
        let json = ctx(serde_json::to_value(ClientCaps::ALL), "encode")?;
        assert_eq!(
            json,
            serde_json::json!({"observe": true, "steer": true, "approve": true, "control": true})
        );
        let old: ClientCaps = ctx(
            serde_json::from_str(
                r#"{"observe":true,"steer":false,"approve":false,"control":false}"#,
            ),
            "decode minor-1 caps",
        )?;
        assert_eq!(old, ClientCaps::OBSERVE);
        let agent = ClientCaps::OBSERVE.with(ClientCaps::TOOL_CALL);
        let json = ctx(serde_json::to_value(agent), "encode agent caps")?;
        assert_eq!(json["tool_call"], serde_json::json!(true));
        assert!(json.get("gateway_admin").is_none());
        let back: ClientCaps = ctx(serde_json::from_value(json), "decode agent caps")?;
        assert_eq!(back, agent);
        Ok(())
    }

    #[test]
    fn tool_call_params_round_trip_and_reject_identity_fields() -> TestResult {
        let params = ToolCallParams {
            session_id: sid("s-1")?,
            turn_id: ctx(TurnId::try_from_str("t-1"), "turn id")?,
            call_id: ctx(ToolCallId::try_from_str("c-1"), "call id")?,
            tool_name: "fs.read".into(),
            arguments: serde_json::json!({"path": "src/lib.rs"}),
            parent_call_id: None,
        };
        let json = ctx(serde_json::to_value(&params), "encode")?;
        let back: ToolCallParams = ctx(serde_json::from_value(json.clone()), "decode")?;
        assert_eq!(back, params);
        let mut forged = json;
        forged["principal"] = serde_json::json!("agent:uia");
        assert!(serde_json::from_value::<ToolCallParams>(forged).is_err());
        let minimal =
            r#"{"session_id":"s","turn_id":"t","call_id":"c","tool_name":"x","arguments":{}}"#;
        let decoded: ToolCallParams = ctx(serde_json::from_str(minimal), "decode minimal")?;
        assert_eq!(decoded.parent_call_id, None);
        Ok(())
    }

    #[test]
    fn tool_result_frame_round_trips_with_placement_and_default_trust() -> TestResult {
        let json = r#"{"call_id":"c-1","result":{"status":"success","value":1},"placement":{"kind":"gateway","node":"gw"},"duration_ms":4}"#;
        let frame: ToolCallResultFrame = ctx(serde_json::from_str(json), "decode")?;
        assert_eq!(frame.trust, ResultTrust::Untrusted);
        assert_eq!(
            frame.placement,
            ToolPlacement::Gateway {
                node: Some("gw".into())
            }
        );
        let again: ToolCallResultFrame = ctx(
            serde_json::from_value(ctx(serde_json::to_value(&frame), "encode")?),
            "decode again",
        )?;
        assert_eq!(again, frame);
        Ok(())
    }

    #[test]
    fn tool_descriptor_unknown_approval_and_role_decode_to_unknown() -> TestResult {
        let json = r#"{"name":"gateway.drain","description":"d","input_schema":{"type":"object"},"approval":"quorum","placement":{"kind":"gateway"}}"#;
        let descriptor: ToolDescriptor = ctx(serde_json::from_str(json), "decode")?;
        assert_eq!(descriptor.approval, ToolApproval::Unknown);
        assert!(!descriptor.parallel_safe);
        let role: AgentRole = ctx(serde_json::from_str(r#""user-interface""#), "role")?;
        assert_eq!(role, AgentRole::UserInterface);
        let future: AgentRole = ctx(serde_json::from_str(r#""hub-steward""#), "future role")?;
        assert_eq!(future, AgentRole::Unknown);
        let principal: PrincipalSummary = ctx(
            serde_json::from_str(
                r#"{"kind":"agent","agent":"a-1","role":"worker","parent":"a-0"}"#,
            ),
            "principal",
        )?;
        assert!(matches!(
            principal,
            PrincipalSummary::Agent { role: AgentRole::Worker, ref parent, .. } if parent.as_deref() == Some("a-0")
        ));
        let other: PrincipalSummary = ctx(
            serde_json::from_str(r#"{"kind":"service","id":"x"}"#),
            "unknown principal",
        )?;
        assert_eq!(other, PrincipalSummary::Unknown);
        Ok(())
    }

    #[test]
    fn gateway_params_reject_unknown_fields() {
        assert!(
            serde_json::from_str::<GatewayToolRightsParams>(
                r#"{"agent":"a","tools":["fs.read"],"tenant":"t"}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<GatewayRevokeParams>(
                r#"{"connection":1,"reason":"r","actor":"uid:0"}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<GatewayDrainParams>(r#"{"retry_after_ms":10}"#).is_ok());
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
