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
//!
//! Wire minor 3 adds the `projects` feature: `session.projects`,
//! `session.page`, `session.adopt`, `session.create_in`,
//! `session.set_project`, `session.rename`, `session.archive`, the
//! [`SessionFrame::Holder`] frame and the vocabulary types for them
//! ([`ProjectSummary`], [`SessionRow`], [`ShareInfo`], ...). Minor 2 bytes
//! are unchanged; `SessionSummary` is not touched.
//!
//! Wire minor 4 adds the `manage` and `stepup` features: typed, allow-listed
//! operation execution (`command.run`, W00 D5 amendment), read-only
//! catalog/config views, and step-up confirmed global writes. Minor 3 bytes
//! are unchanged.

use harw_types::{
    ApprovalId, DeviceId, PermissionTier, ReviewDecision, SessionId, TenantId, ToolCallId, TurnId,
};
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
/// - 3: projects (`session.projects`, `session.page`, ..., frame `holder`,
///   feature `projects`).
/// - 4: manage (`commands.list`, `command.run`, `catalog.get`, `config.get`),
///   stepup (`config.set`/`reset`, `secret.put`, `stepup.confirm`/`cancel`,
///   frame `stepup_request`).
pub const SESSION_WIRE_MINOR: u32 = 4;

/// First wire minor with the features `manage` and `stepup`.
pub const MANAGE_WIRE_MINOR: u32 = 4;

/// Upper bound for `client_msg_id` of `command.run`, `config.set`, `config.reset` (bytes).
pub const MAX_CLIENT_MSG_ID_BYTES: usize = 128;

/// Upper bound for `command.run.line` (bytes).
pub const MAX_COMMAND_LINE_BYTES: usize = 512;

/// Upper bound for `secret.put.value` (bytes).
pub const MAX_SECRET_VALUE_BYTES: usize = 8194;

/// Upper bound for the number of `config.get.keys`.
pub const MAX_CONFIG_KEYS: usize = 32;

/// First wire minor with feature `projects` and the project/page methods.
pub const PROJECTS_WIRE_MINOR: u32 = 3;

/// Default `limit` of `session.projects` and `session.page`.
pub const DEFAULT_PAGE_LIMIT: u32 = 50;

/// Upper bound for `limit` of `session.projects` and `session.page`.
pub const MAX_PAGE_LIMIT: u32 = 200;

/// Pseudo project key for sessions without a project ("Ohne Projekt").
pub const NO_PROJECT_KEY: &str = "";

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
    /// The host honours [`super::StreamProfile::CompactReasoning`]. A client sends that profile only when the host granted this feature.
    pub const COMPACT_REASONING: &str = "compact_reasoning";
    /// The host emits [`super::SessionFrame::Job`] frames (agent jobs of the session).
    pub const JOB_FRAMES: &str = "job_frames";
    /// The host serves `tool.list`/`tool.call`/`tool.cancel` (R18 D-A).
    pub const TOOLS: &str = "tools";
    /// The host serves the `gateway.*` methods (R18 D-B).
    pub const GATEWAY: &str = "gateway";
    /// Projects and pages (`session.projects`, `session.page`, ...) and the `holder` frame. Minor >= 3.
    pub const PROJECTS: &str = "projects";
    /// `commands.list`, `command.run`, `catalog.get`, `config.get`. Minor >= 4.
    pub const MANAGE: &str = "manage";
    /// `config.set`, `config.reset`, `secret.put`, `stepup.confirm`, `stepup.cancel` and the
    /// `stepup_request` frame. Minor >= 4, only together with `manage`.
    pub const STEPUP: &str = "stepup";
    /// `turn.submit` honours [`super::SubmitParams::images`]. A client sends images only when the host granted this feature (an older host rejects the unknown field).
    pub const IMAGES: &str = "images";
    /// The host serves `media.get`: the stored bytes of a prompt image of a session the caller may observe. A client calls it only when the host granted this feature.
    pub const MEDIA: &str = "media";
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

/// Fencing token of a session's owner-host placement (W00 R2).
///
/// A session has exactly one owner host (node id plus this generation); v1 has
/// no cross-node failover, so the generation only grows when the owner
/// placement is re-established (restart or re-placement). It is a different
/// axis from [`Cursor::generation`], which tracks transcript rewrites. A
/// client holding a lower value than the host's must resync instead of
/// trusting a cached [`Cursor`]. Not yet carried by a minor-2 message: adding
/// it to a message is a wire-minor bump.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct PlacementGeneration(pub u64);

impl PlacementGeneration {
    /// First placement of a session.
    pub const FIRST: Self = Self(1);

    /// The next placement, or `None` on overflow (never wraps).
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// True when `self` (a client's remembered value) is older than the
    /// host's `current` placement and so forces a resync.
    #[must_use]
    pub const fn is_stale_against(self, current: Self) -> bool {
        self.0 < current.0
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
    /// Phone profile with live reasoning: like `Compact` (coalesced assistant text, no child assistant/usage/context deltas) but delivers reasoning deltas of the root and of child agents and child progress. Only sent when `HelloAck::features` contains [`features::COMPACT_REASONING`]; an older host cannot decode it.
    CompactReasoning,
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

/// Share mode of the gateway for TUI (profile) sessions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareMode {
    #[default]
    Off,
    Read,
    Continue,
}

impl ShareMode {
    /// `"off" | "read" | "continue"`, otherwise `None`.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "off" => Some(Self::Off),
            "read" => Some(Self::Read),
            "continue" => Some(Self::Continue),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Read => "read",
            Self::Continue => "continue",
        }
    }
}

/// Holder of a session lease as a client sees it. Never a pid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HolderKind {
    Tui,
    Gateway,
    Job,
    #[serde(other)]
    Unknown,
}

/// Current lease holder of a session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolderInfo {
    pub kind: HolderKind,
    pub since: jiff::Timestamp,
}

/// Where the last turn of a session ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LastOrigin {
    Tui,
    Gateway,
    #[serde(other)]
    Unknown,
}

/// Where a row of `session.page` comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowOrigin {
    /// Has a host record.
    Hosted,
    /// Only in the profile store (TUI session without a record).
    Profile,
    #[serde(other)]
    Unknown,
}

/// Why a row cannot be continued. Clients translate it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadOnlyReason {
    /// The gateway shares read-only.
    ShareReadOnly,
    /// Origin is not a CLI session (web, run, gateway channel).
    ForeignOrigin,
    /// No valid working directory.
    NoWorkdir,
    /// The connection has no `steer` cap.
    NoSteer,
    /// The session is closed.
    Closed,
    #[serde(other)]
    Unknown,
}

/// What a client needs to know about sharing (tier badge).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareInfo {
    pub mode: ShareMode,
    /// Effective tier: min(device tier, listener tier).
    pub tier: PermissionTier,
}

/// One project in `session.projects`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    /// `""` = sessions without a project.
    pub project_key: String,
    pub label: String,
    /// Only on basename collisions: shortened parent path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_hint: Option<String>,
    pub session_count: u32,
    pub last_activity: jiff::Timestamp,
    pub active_count: u32,
}

/// One row in `session.page`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRow {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_key: Option<String>,
    pub updated_at: jiff::Timestamp,
    /// Only when greater than zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<u64>,
    pub state: HostedState,
    pub attached: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub origin: RowOrigin,
    pub continuable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<ReadOnlyReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<HolderInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_origin: Option<LastOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_message: Option<String>,
}

/// `session.projects`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectsParams {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Result of `session.projects`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectsResult {
    pub projects: Vec<ProjectSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub share: ShareInfo,
}

/// `session.page`. `project_key`: `None` = all projects, `Some("")` = no project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageParams {
    #[serde(default)]
    pub project_key: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub include_archived: bool,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Result of `session.page`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageResult {
    pub sessions: Vec<SessionRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// `session.create_in`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateInParams {
    pub project_key: String,
    #[serde(default)]
    pub title: Option<String>,
}

/// `session.set_project`. `project_key = ""` removes the assignment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetProjectParams {
    pub session_id: SessionId,
    pub project_key: String,
}

/// `session.rename`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenameParams {
    pub session_id: SessionId,
    pub title: String,
}

/// `session.archive`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveParams {
    pub session_id: SessionId,
    pub archived: bool,
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
    /// Lease change of a session (feature `projects`). `holder: None` = free.
    Holder {
        session_id: SessionId,
        holder: Option<HolderInfo>,
    },
    /// A step-up challenge (feature `stepup`). Only to the requesting connection.
    StepupRequest(StepUpChallenge),
    /// A frame kind this client does not know (additive evolution). Must stay last.
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
    "holder",
    "stepup_request",
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
            Self::Holder { .. } => "holder",
            Self::StepupRequest(_) => "stepup_request",
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
    /// Images attached to the prompt (feature `images`). Empty = none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<SubmitImage>,
}

/// Largest image (stored bytes) `media.get` returns; bigger ones are refused so the answer fits the 1 MiB transport message.
pub const MAX_MEDIA_GET_BYTES: u64 = 512 * 1024;

/// `media.get`: the stored bytes of one image a transcript item of the session references.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaGetParams {
    pub session_id: SessionId,
    /// The reference as carried by the transcript item.
    pub media: crate::media::MediaRef,
}

/// Result of `media.get`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaGetResult {
    /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
    pub mime: String,
    /// Standard base64 (with padding) of the stored image.
    pub data_b64: String,
}

/// Most images one `turn.submit` may carry.
pub const MAX_SUBMIT_IMAGES: usize = 4;
/// Most base64 bytes over all images of one `turn.submit` (the transport message limit is 1 MiB).
pub const MAX_SUBMIT_IMAGE_B64_BYTES: usize = 800 * 1024;

/// One image of a `turn.submit`: the bytes travel inline (base64), the host
/// checks, sanitizes and stores them; transcripts only carry a media reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitImage {
    /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
    pub mime: String,
    /// Standard base64 (RFC 4648, with padding) of the image bytes.
    pub data_b64: String,
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

// ---------------------------------------------------------------------------
// Wire minor 4: `manage` and `stepup` features.
// ---------------------------------------------------------------------------

/// Origin of a command (mirror of the ops command metadata kind).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandOrigin {
    Operation,
    TuiLocal,
    #[serde(other)]
    Unknown,
}

/// Effect of `command.run` / `config.set` / `stepup.confirm`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandEffect {
    /// Effective immediately.
    Applied,
    /// Effective from the next turn of the session.
    Staged,
    /// Effective from the next session (roles, default model, provider).
    NextSession,
    /// Not executed; `reason_code`/`text` say why. `ok = false`.
    Refused,
    /// Nothing written yet; `stepup` carries the challenge. `ok = true`.
    StepupRequired,
    #[serde(other)]
    Unknown,
}

/// How a provider authenticates (mirror of the ops `AuthKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuthKind {
    None,
    Env,
    File,
    Keyring,
    Secrets,
    FileJson,
    InlineKey,
    #[serde(other)]
    Unknown,
}

/// `commands.list` parameters.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandsListParams {
    /// Session for which `available` is computed. `None` = profile wide.
    #[serde(default)]
    pub session_id: Option<SessionId>,
}

/// A sub-command hint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandHint {
    pub name: String,
    pub args: String,
    pub summary: String,
}

/// A command as shown in the TUI popup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEntry {
    /// Without the leading `/`.
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub summary: String,
    pub usage: String,
    /// Operation domain in snake_case. A string so new domains do not break old clients.
    pub domain: String,
    pub permission: PermissionTier,
    /// `immediate` | `staged` | `deferred`.
    pub busy: String,
    pub origin: CommandOrigin,
    pub remote: bool,
    pub available: bool,
    /// Reason text, only when `!available`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Stable machine id, only when `!available`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(default)]
    pub subcommands: Vec<CommandHint>,
}

/// `commands.list` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandsListResult {
    /// Sorted by `name`.
    pub commands: Vec<CommandEntry>,
    /// 16 hex digits; a string because a u64 may lose precision in JSON parsers.
    pub rev: String,
}

/// `command.run` parameters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRunParams {
    #[serde(default)]
    pub session_id: Option<SessionId>,
    /// Whole line with `/`, at most [`MAX_COMMAND_LINE_BYTES`].
    pub line: String,
    /// Idempotency across reconnects, 1..=[`MAX_CLIENT_MSG_ID_BYTES`].
    pub client_msg_id: String,
}

/// A step-up challenge. Contains nothing secret. Also the payload of the
/// `stepup_request` frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepUpChallenge {
    /// 32 lowercase hex.
    pub stepup_id: String,
    /// For example `config.set`, `models.set`, `provider.add`.
    pub action: String,
    pub target: String,
    /// Canonical text the app shows completely before the biometric prompt.
    pub text: String,
    /// 64 lowercase hex.
    pub nonce: String,
    /// `hex(sha256(b"harw-stepup-v1\0" || nonce_bytes || 0x00 || text_utf8))`, 64 lowercase hex.
    pub hash: String,
    pub expires_at: jiff::Timestamp,
}

/// Result of `command.run`, `config.set`, `config.reset`, `stepup.confirm`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandRunResult {
    pub ok: bool,
    /// Human readable text.
    pub text: String,
    /// Operation output data, unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    pub effect: CommandEffect,
    /// With `effect = refused`: a refusal code (see [`refusal_codes`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// With `effect = stepup_required`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stepup: Option<StepUpChallenge>,
}

/// Refusal codes that do not come from the ops allow-list (gateway side). Stable.
pub mod refusal_codes {
    /// Session command without a `session_id`.
    pub const SESSION_REQUIRED: &str = "session_required";
    /// Session action on a profile session without a host record.
    pub const SESSION_NOT_HOSTED: &str = "session_not_hosted";
    /// The operation reports `NotAvailable` (service missing in the gateway).
    pub const NOT_AVAILABLE: &str = "not_available";
    /// The operation failed.
    pub const OP_FAILED: &str = "op_failed";
    /// The value changed since the confirmation.
    pub const STALE: &str = "stale";
    /// Step-up writes are not enabled on this gateway.
    pub const STEPUP_DISABLED: &str = "stepup_disabled";
}

/// `catalog.get` parameters.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogGetParams {
    /// For `live` (model of the session). `None` = profile only.
    #[serde(default)]
    pub session_id: Option<SessionId>,
}

/// A redacted provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogProvider {
    pub name: String,
    pub api: String,
    /// `scheme://host[:port]/path` without userinfo/query/fragment; empty if unparsable.
    pub base_url: String,
    pub enabled: bool,
    pub auth_kind: ProviderAuthKind,
    /// Only a name/entry for `env`/`keyring`/`secrets`, else `None`. Never a path, never a value.
    #[serde(default)]
    pub auth_name: Option<String>,
    pub has_secret: bool,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub max_concurrency: Option<u64>,
}

/// A model of the catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogModel {
    pub key: String,
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub provider: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    pub reasoning: bool,
    #[serde(default)]
    pub input_types: Vec<String>,
    /// `active` | `deprecated` | `retirement_window` | `retired` | `unknown` (string, additive).
    pub lifecycle: String,
}

/// A role binding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogRole {
    pub role: String,
    pub label: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub source: String,
    #[serde(default)]
    pub effort: Option<String>,
}

/// The live model of the session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogLive {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

/// `catalog.get` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogResult {
    pub providers: Vec<CatalogProvider>,
    pub models: Vec<CatalogModel>,
    pub roles: Vec<CatalogRole>,
    pub live: CatalogLive,
    /// For example "applies from the next session".
    pub note: String,
}

/// `config.get` parameters.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigGetParams {
    /// `None` = all allow-listed keys. At most [`MAX_CONFIG_KEYS`].
    #[serde(default)]
    pub keys: Option<Vec<String>>,
}

/// One config row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigEntry {
    pub key: String,
    pub value: String,
    #[serde(default)]
    pub choices: Vec<String>,
    /// Always `profile`.
    pub scope: String,
    /// `now` | `next_session`.
    pub apply: String,
}

/// `config.get` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigGetResult {
    pub entries: Vec<ConfigEntry>,
}

/// `config.set` (feature `stepup`). Result: [`CommandRunResult`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSetParams {
    pub key: String,
    pub value: String,
    pub client_msg_id: String,
}

/// `config.reset` (feature `stepup`). Result: [`CommandRunResult`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigResetParams {
    pub key: String,
    pub client_msg_id: String,
}

/// A secret on its way to the server. `Debug` redacts; no `Display`,
/// no `PartialEq`, no `Clone`. Serde transparent (JSON string).
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct WireSecret(String);

impl WireSecret {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The only access; the server moves the value into a secret type at once.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Length in bytes (for the upper bound without reading the value).
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for WireSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WireSecret([redacted])")
    }
}

/// `secret.put` (feature `stepup`).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretPutParams {
    /// Name in the secret store, `[A-Za-z0-9._-]`, 1..=64.
    pub name: String,
    /// The NEW provider of this operation the key is bound to.
    pub for_provider: String,
    pub value: WireSecret,
}

/// `secret.put` result. Never contains the value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretPutResult {
    /// 32 hex; for `--key <handle>` of a following provider add.
    pub handle: String,
    pub key_name: String,
    pub for_provider: String,
    pub expires_at: jiff::Timestamp,
}

/// `stepup.confirm` parameters. Result: [`CommandRunResult`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepUpConfirmParams {
    pub stepup_id: String,
    /// The hash of the challenge (64 hex), sent after a fresh biometric prompt.
    pub hash: String,
}

/// `stepup.cancel` parameters. Result `()`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepUpCancelParams {
    pub stepup_id: String,
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
    fn holder_frame_round_trips_and_old_clients_see_unknown() -> TestResult {
        let since: jiff::Timestamp = ctx("2026-01-02T03:04:05Z".parse(), "timestamp")?;
        let frame = SessionFrame::Holder {
            session_id: sid("sess-a1")?,
            holder: Some(HolderInfo {
                kind: HolderKind::Tui,
                since,
            }),
        };
        let value = ctx(serde_json::to_value(&frame), "encode")?;
        assert_eq!(value["kind"], "holder");
        assert_eq!(value["data"]["session_id"], "sess-a1");
        assert_eq!(value["data"]["holder"]["kind"], "tui");
        let back: SessionFrame = ctx(serde_json::from_value(value), "decode")?;
        assert_eq!(back.kind(), "holder");
        assert!(!back.is_disposable());
        let free = SessionFrame::Holder {
            session_id: sid("sess-a1")?,
            holder: None,
        };
        let back: SessionFrame = ctx(
            serde_json::from_str(&ctx(serde_json::to_string(&free), "encode free")?),
            "decode free",
        )?;
        assert!(matches!(back, SessionFrame::Holder { holder: None, .. }));
        Ok(())
    }

    fn sample_challenge() -> StepUpChallenge {
        StepUpChallenge {
            stepup_id: "0".repeat(32),
            action: "config.set".into(),
            target: "theme".into(),
            text: "Set theme to dark".into(),
            nonce: "a".repeat(64),
            hash: "b".repeat(64),
            expires_at: jiff::Timestamp::UNIX_EPOCH,
        }
    }

    #[test]
    fn minor_four_constants() {
        assert_eq!(MANAGE_WIRE_MINOR, 4);
        assert_eq!(features::MANAGE, "manage");
        assert_eq!(features::STEPUP, "stepup");
        assert_eq!(MAX_CLIENT_MSG_ID_BYTES, 128);
        assert_eq!(MAX_COMMAND_LINE_BYTES, 512);
        assert_eq!(MAX_SECRET_VALUE_BYTES, 8194);
        assert_eq!(MAX_CONFIG_KEYS, 32);
    }

    #[test]
    fn stepup_frame_round_trips_and_future_kinds_are_unknown() -> TestResult {
        let frame = SessionFrame::StepupRequest(sample_challenge());
        let value = ctx(serde_json::to_value(&frame), "encode")?;
        assert_eq!(value["kind"], "stepup_request");
        assert_eq!(value["data"]["action"], "config.set");
        let back: SessionFrame = ctx(serde_json::from_value(value), "decode")?;
        assert!(matches!(back, SessionFrame::StepupRequest(_)));
        assert!(!back.is_disposable());
        let future: SessionFrame = ctx(
            serde_json::from_str(r#"{"kind":"stepup_request_v9","data":{}}"#),
            "decode future",
        )?;
        assert!(matches!(future, SessionFrame::Unknown));
        Ok(())
    }

    #[test]
    fn minor_four_params_reject_unknown_fields() {
        fn rejects<T: serde::de::DeserializeOwned>(json: &str) -> bool {
            serde_json::from_str::<T>(json).is_err()
        }
        assert!(rejects::<CommandsListParams>(r#"{"tenant":"tenant-test"}"#));
        assert!(rejects::<CommandRunParams>(
            r#"{"line":"/status","client_msg_id":"m-1","tenant":"tenant-test"}"#
        ));
        assert!(rejects::<CommandRunParams>(
            r#"{"line":"/status","client_msg_id":"m-1","tier":"owner"}"#
        ));
        assert!(rejects::<CatalogGetParams>(r#"{"tenant":"tenant-test"}"#));
        assert!(rejects::<ConfigGetParams>(r#"{"tenant":"tenant-test"}"#));
        assert!(rejects::<ConfigSetParams>(
            r#"{"key":"k","value":"v","client_msg_id":"m","tenant":"tenant-test"}"#
        ));
        assert!(rejects::<ConfigResetParams>(
            r#"{"key":"k","client_msg_id":"m","tenant":"tenant-test"}"#
        ));
        assert!(rejects::<SecretPutParams>(
            r#"{"name":"n","for_provider":"p","value":"fake-SECRET-marker","tenant":"tenant-test"}"#
        ));
        assert!(rejects::<StepUpConfirmParams>(
            r#"{"stepup_id":"x","hash":"y","actor":"x"}"#
        ));
        assert!(rejects::<StepUpConfirmParams>(
            r#"{"stepup_id":"x","hash":"y","tenant":"tenant-test"}"#
        ));
        assert!(rejects::<StepUpCancelParams>(
            r#"{"stepup_id":"x","tenant":"tenant-test"}"#
        ));
    }

    #[test]
    fn minor_four_params_accept_plain_shape() -> TestResult {
        let _: CommandsListParams = ctx(serde_json::from_str("{}"), "commands.list")?;
        let _: CatalogGetParams = ctx(serde_json::from_str("{}"), "catalog.get")?;
        let _: ConfigGetParams = ctx(serde_json::from_str("{}"), "config.get")?;
        let run: CommandRunParams = ctx(
            serde_json::from_str(r#"{"line":"/status","client_msg_id":"m-1"}"#),
            "command.run",
        )?;
        assert_eq!(run.line, "/status");
        assert!(run.session_id.is_none());
        Ok(())
    }

    #[test]
    fn minor_four_results_accept_additional_fields() -> TestResult {
        let challenge = serde_json::to_value(sample_challenge());
        let mut challenge = ctx(challenge, "encode challenge")?;
        challenge["future"] = serde_json::json!(1);
        let _: StepUpChallenge = ctx(serde_json::from_value(challenge), "challenge")?;
        let _: CommandsListResult = ctx(
            serde_json::from_str(r#"{"commands":[],"rev":"0000000000000000","future":1}"#),
            "list result",
        )?;
        let _: CommandEntry = ctx(
            serde_json::from_str(
                r#"{"name":"status","summary":"s","usage":"/status","domain":"session","permission":"observer","busy":"immediate","origin":"operation","remote":true,"available":true,"future":1}"#,
            ),
            "entry",
        )?;
        let _: CommandRunResult = ctx(
            serde_json::from_str(r#"{"ok":true,"text":"t","effect":"applied","future":1}"#),
            "run result",
        )?;
        let _: CatalogResult = ctx(
            serde_json::from_str(
                r#"{"providers":[],"models":[],"roles":[],"live":{},"note":"n","future":1}"#,
            ),
            "catalog",
        )?;
        let _: CatalogProvider = ctx(
            serde_json::from_str(
                r#"{"name":"p","api":"a","base_url":"https://x.example.test","enabled":true,"auth_kind":"env","has_secret":false,"future":1}"#,
            ),
            "provider",
        )?;
        let _: ConfigGetResult = ctx(
            serde_json::from_str(r#"{"entries":[],"future":1}"#),
            "config result",
        )?;
        let _: SecretPutResult = ctx(
            serde_json::from_str(
                r#"{"handle":"h","key_name":"k","for_provider":"p","expires_at":"2026-01-01T00:00:00Z","future":1}"#,
            ),
            "secret result",
        )?;
        Ok(())
    }

    #[test]
    fn minor_four_enums_decode_unknown() -> TestResult {
        let origin: CommandOrigin = ctx(serde_json::from_str("\"future\""), "origin")?;
        assert_eq!(origin, CommandOrigin::Unknown);
        let effect: CommandEffect = ctx(serde_json::from_str("\"future\""), "effect")?;
        assert_eq!(effect, CommandEffect::Unknown);
        let auth: ProviderAuthKind = ctx(serde_json::from_str("\"future\""), "auth")?;
        assert_eq!(auth, ProviderAuthKind::Unknown);
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
            assert_eq!(ctx(serde_json::to_value(effect), "encode")?, name);
        }
        Ok(())
    }

    #[test]
    fn wire_secret_debug_is_redacted_but_wire_carries_value() -> TestResult {
        let params = SecretPutParams {
            name: "key-a".into(),
            for_provider: "prov-a".into(),
            value: WireSecret::new("sk-test-FAKE-0000000000000000".into()),
        };
        let debug = format!("{params:?}");
        assert!(!debug.contains("FAKE"));
        assert!(debug.contains("[redacted]"));
        let json = ctx(serde_json::to_string(&params), "encode")?;
        assert!(json.contains("sk-test-FAKE-0000000000000000"));
        assert_eq!(params.value.len(), 29);
        assert!(!params.value.is_empty());
        assert_eq!(params.value.expose().len(), params.value.len());
        let result = SecretPutResult {
            handle: "0".repeat(32),
            key_name: "key-a".into(),
            for_provider: "prov-a".into(),
            expires_at: jiff::Timestamp::UNIX_EPOCH,
        };
        let value = ctx(serde_json::to_value(&result), "encode result")?;
        let mut keys: Vec<&str> = value
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        keys.sort_unstable();
        assert_eq!(keys, ["expires_at", "for_provider", "handle", "key_name"]);
        Ok(())
    }

    #[test]
    fn catalog_provider_has_no_secret_fields() -> TestResult {
        let provider = CatalogProvider {
            name: "p".into(),
            api: "a".into(),
            base_url: "https://x.example.test".into(),
            enabled: true,
            auth_kind: ProviderAuthKind::Env,
            auth_name: None,
            has_secret: false,
            models: vec![],
            max_concurrency: None,
        };
        let value = ctx(serde_json::to_value(&provider), "encode")?;
        let mut keys: Vec<&str> = value
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        keys.sort_unstable();
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
            assert!(!keys.contains(&forbidden));
        }
        Ok(())
    }

    #[test]
    fn minor_three_constants() {
        assert_eq!(SESSION_WIRE_MINOR, 4);
        assert_eq!(PROJECTS_WIRE_MINOR, 3);
        assert_eq!(features::PROJECTS, "projects");
        const { assert!(DEFAULT_PAGE_LIMIT <= MAX_PAGE_LIMIT) };
        assert_eq!(NO_PROJECT_KEY, "");
    }

    #[test]
    fn share_mode_parse_and_default() {
        assert_eq!(ShareMode::default(), ShareMode::Off);
        for mode in [ShareMode::Off, ShareMode::Read, ShareMode::Continue] {
            assert_eq!(ShareMode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(ShareMode::parse("Read"), None);
        assert_eq!(ShareMode::parse(""), None);
    }

    #[test]
    fn unknown_enum_values_decode_to_unknown() -> TestResult {
        let kind: HolderKind = ctx(serde_json::from_str("\"future\""), "holder kind")?;
        assert_eq!(kind, HolderKind::Unknown);
        let reason: ReadOnlyReason = ctx(serde_json::from_str("\"future\""), "reason")?;
        assert_eq!(reason, ReadOnlyReason::Unknown);
        let origin: RowOrigin = ctx(serde_json::from_str("\"future\""), "origin")?;
        assert_eq!(origin, RowOrigin::Unknown);
        let last: LastOrigin = ctx(serde_json::from_str("\"future\""), "last origin")?;
        assert_eq!(last, LastOrigin::Unknown);
        Ok(())
    }

    #[test]
    fn project_params_reject_unknown_fields_results_tolerate_them() -> TestResult {
        assert!(serde_json::from_str::<PageParams>(r#"{"tenant":"x"}"#).is_err());
        assert!(serde_json::from_str::<ProjectsParams>(r#"{"actor":"x"}"#).is_err());
        assert!(serde_json::from_str::<CreateInParams>(r#"{"project_key":"p","uid":1}"#).is_err());
        let page: PageParams = ctx(serde_json::from_str("{}"), "empty page params")?;
        assert_eq!(page, PageParams::default());
        let result: PageResult = ctx(
            serde_json::from_str(r#"{"sessions":[],"added_later":true}"#),
            "result with extra field",
        )?;
        assert!(result.sessions.is_empty());
        Ok(())
    }

    #[test]
    fn session_row_omits_empty_options_and_round_trips() -> TestResult {
        let at: jiff::Timestamp = ctx("2026-01-02T03:04:05Z".parse(), "timestamp")?;
        let row = SessionRow {
            session_id: sid("sess-a1")?,
            title: None,
            project_key: Some(NO_PROJECT_KEY.to_owned()),
            updated_at: at,
            turns: None,
            state: HostedState::Idle,
            attached: 0,
            model: None,
            origin: RowOrigin::Profile,
            continuable: false,
            reason: Some(ReadOnlyReason::ShareReadOnly),
            holder: None,
            last_origin: None,
            first_message: None,
        };
        let value = ctx(serde_json::to_value(&row), "encode row")?;
        for absent in [
            "title",
            "turns",
            "model",
            "holder",
            "last_origin",
            "first_message",
        ] {
            assert!(value.get(absent).is_none(), "{absent} must be omitted");
        }
        assert_eq!(value["reason"], "share_read_only");
        let back: SessionRow = ctx(serde_json::from_value(value), "decode row")?;
        assert_eq!(back, row);
        Ok(())
    }

    #[test]
    fn share_info_round_trips() -> TestResult {
        let info = ShareInfo {
            mode: ShareMode::Continue,
            tier: PermissionTier::Operator,
        };
        let json = ctx(serde_json::to_string(&info), "encode share")?;
        let back: ShareInfo = ctx(serde_json::from_str(&json), "decode share")?;
        assert_eq!(back, info);
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
            SessionFrame::StepupRequest(sample_challenge()),
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
    fn placement_generation_fences_and_never_wraps() {
        let first = PlacementGeneration::FIRST;
        assert_eq!(first.next(), Some(PlacementGeneration(2)));
        assert_eq!(PlacementGeneration(u64::MAX).next(), None);
        assert!(PlacementGeneration(1).is_stale_against(PlacementGeneration(2)));
        assert!(!PlacementGeneration(2).is_stale_against(PlacementGeneration(2)));
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
    fn compact_reasoning_profile_uses_its_snake_case_name() -> TestResult {
        let value = ctx(
            serde_json::to_value(StreamProfile::CompactReasoning),
            "encode profile",
        )?;
        assert_eq!(value, serde_json::json!("compact_reasoning"));
        let json = r#"{"session_id":"s-9","profile":"compact_reasoning"}"#;
        let params: AttachParams = ctx(serde_json::from_str(json), "decode attach")?;
        assert_eq!(params.profile, StreamProfile::CompactReasoning);
        assert_eq!(features::COMPACT_REASONING, "compact_reasoning");
        assert_eq!(features::JOB_FRAMES, "job_frames");
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
