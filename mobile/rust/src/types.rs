use harw_protocol::session_wire::{
    HolderInfo, HolderKind, HostedState, LastOrigin, ProjectSummary, ReadOnlyReason, RowOrigin,
    SessionRow, ShareInfo, ShareMode,
};
use harw_protocol::{Cursor, PortError};
use harw_types::PermissionTier;
use serde::{Deserialize, Serialize};

#[derive(uniffi::Record)]
pub struct IdentityMaterial {
    pub private_key: Vec<u8>,
    pub enrollment: DeviceEnrollment,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
#[serde(deny_unknown_fields)]
pub struct DeviceEnrollment {
    pub version: u32,
    pub node_id: String,
    pub public_key_hex: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
#[serde(deny_unknown_fields)]
pub struct ServerProfile {
    pub version: u32,
    pub label: String,
    pub endpoint: String,
    pub server_node_id: String,
    pub server_public_key_hex: String,
    /// Optional Tor onion route `<56>.onion:<port>`; absent in profiles without onion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[uniffi(default = None)]
    pub onion: Option<String>,
}

/// Which network route a connection preference allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RoutePreference {
    /// Onion first when the profile has one, else direct.
    Auto,
    /// Tor (.onion) only; fails when the profile has no onion route.
    Onion,
    /// Direct TCP only.
    Direct,
}

/// The network route a connection uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ConnectionRoute {
    /// Direct TCP to the profile endpoint.
    Direct,
    /// Tor onion service at the profile's `.onion` locator.
    Onion,
}

/// Live Tor bootstrap progress shown while the onion route starts.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct TorBootstrap {
    /// Bootstrap completion, 0 to 100.
    pub percent: u32,
    /// True once Tor can carry traffic.
    pub ready: bool,
    /// Human-readable reason Tor is blocked, if it is.
    pub blocked: Option<String>,
    /// Short status summary from Tor.
    pub summary: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ConnectionPhase {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
    Failed,
    Revoked,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ConnectionStatus {
    pub phase: ConnectionPhase,
    pub node_id: Option<String>,
    pub endpoint: Option<String>,
    pub label: Option<String>,
    pub detail: Option<String>,
    pub can_create: bool,
    pub can_submit: bool,
    pub can_approve: bool,
    /// Route being dialed while connecting, the link's route when connected, else None.
    pub route: Option<ConnectionRoute>,
    /// The route preference the user selected.
    pub route_preference: RoutePreference,
    /// Routes this profile can use, in preference order.
    pub available_routes: Vec<ConnectionRoute>,
    /// Tor bootstrap progress; None until Tor was started.
    pub tor: Option<TorBootstrap>,
    /// Reconnect attempts since the last successful connection; 0 while connected or idle.
    pub attempt: u32,
    /// Unix time in ms of the next scheduled dial while waiting in backoff, else None.
    pub next_retry_at_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MobileSessionState {
    Idle,
    Running,
    WaitingForApproval,
    WaitingForChild,
    Queued,
    Interrupted,
    Failed,
    Closed,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct MobileSession {
    pub session_id: String,
    pub title: Option<String>,
    pub state: MobileSessionState,
    pub attached: u32,
    pub updated_at: String,
    pub model: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SessionCursor {
    pub generation: u32,
    pub durable: u64,
    pub live: u32,
}

impl From<Cursor> for SessionCursor {
    fn from(cursor: Cursor) -> Self {
        Self {
            generation: cursor.generation,
            durable: cursor.durable,
            live: cursor.live,
        }
    }
}

impl From<SessionCursor> for Cursor {
    fn from(cursor: SessionCursor) -> Self {
        Self {
            generation: cursor.generation,
            durable: cursor.durable,
            live: cursor.live,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub head: Option<SessionCursor>,
    pub online: bool,
    pub can_submit: bool,
    pub can_approve: bool,
    pub can_interrupt: bool,
    pub running_turn_id: Option<String>,
    pub transcript: Vec<TranscriptEntry>,
    pub live_text: Option<String>,
    pub approvals: Vec<MobileApproval>,
    pub last_failure: Option<String>,
    pub last_sync_millis: Option<i64>,
    pub detail: Option<String>,
    pub agents: Vec<MobileAgent>,
    pub jobs: Vec<MobileJob>,
    pub reasoning: Vec<ReasoningLine>,
    pub finished: Vec<FinishedCategory>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum TranscriptKind {
    User,
    Assistant,
    Tool,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MobileToolState {
    Running,
    Done,
    Failed,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct TranscriptEntry {
    pub id: String,
    pub kind: TranscriptKind,
    pub text: String,
    pub tool_state: Option<MobileToolState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MobileAgentState {
    Running,
    Waiting,
    Done,
    Failed,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct MobileAgent {
    pub id: String,
    pub parent: Option<String>,
    pub role: String,
    pub model: Option<String>,
    pub state: MobileAgentState,
    pub current_tool: Option<String>,
    pub started_ms: u64,
    pub tool_calls: u32,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct MobileJob {
    pub work_id: String,
    pub kind: String,
    pub state: String,
    pub summary: Option<String>,
    pub updated_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ReasoningKind {
    Thinking,
    ToolStart,
    ToolEnd,
    Finished,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ReasoningLine {
    pub seq: u64,
    pub agent: String,
    pub role: String,
    pub kind: ReasoningKind,
    pub ok: Option<bool>,
    pub text: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FinishedCategory {
    pub label: String,
    pub total: u32,
    pub ok: u32,
    pub failed: u32,
    pub cancelled: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ApprovalRisk {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ApprovalDecision {
    Approved,
    Rejected,
    ApprovedOnce,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct MobileApproval {
    pub id: String,
    pub summary: String,
    pub risk: ApprovalRisk,
    pub requested_at: String,
    pub timeout_at: String,
    pub decisions: Vec<ApprovalDecision>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SubmitOutcome {
    Accepted,
    /// The session is held by another writer (TUI, gateway, job); see `detail`.
    Busy,
    Stale,
    QueueFull,
    Denied,
    InFlight,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SubmitReceipt {
    pub outcome: SubmitOutcome,
    pub position: Option<u32>,
    pub head: Option<SessionCursor>,
    pub detail: Option<String>,
}

impl SubmitReceipt {
    pub(crate) fn new(outcome: SubmitOutcome) -> Self {
        Self {
            outcome,
            position: None,
            head: None,
            detail: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ApprovalOutcome {
    Resolved,
    AlreadyResolved,
    Expired,
    Denied,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct ApprovalReceipt {
    pub outcome: ApprovalOutcome,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, thiserror::Error, uniffi::Error)]
pub enum MobileError {
    #[error("invalid phone identity: {detail}")]
    InvalidIdentity { detail: String },
    #[error("invalid server profile: {detail}")]
    InvalidProfile { detail: String },
    #[error("authentication or connection failed: {detail}")]
    Connect { detail: String },
    #[error("disconnected")]
    Disconnected,
    #[error("the server withdrew this connection's authority")]
    Revoked,
    #[error("session: {detail}")]
    Session { detail: String },
    #[error("denied: {detail}")]
    Denied { detail: String },
    #[error("transport: {detail}")]
    Transport { detail: String },
    #[error("submission may have reached the server: {detail}")]
    Ambiguous { detail: String },
    #[error("busy: {detail}")]
    Busy { detail: String },
    #[error("mobile core: {detail}")]
    Internal { detail: String },
}

impl From<PortError> for MobileError {
    fn from(error: PortError) -> Self {
        match error {
            PortError::Transport(detail) => Self::Transport { detail },
            PortError::Denied(detail) => Self::Denied { detail },
            PortError::Revoked => Self::Revoked,
            PortError::Busy(detail) => Self::Busy { detail },
            other => Self::Session {
                detail: other.to_string(),
            },
        }
    }
}

/// Share mode of the gateway for TUI sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MobileShareMode {
    Off,
    Read,
    Continue,
}

/// Effective permission tier of this connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MobileTier {
    Observer,
    Operator,
    Maintainer,
    Owner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MobileShareInfo {
    pub mode: MobileShareMode,
    pub tier: MobileTier,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MobileProject {
    /// Empty string means "no project".
    pub project_key: String,
    pub label: String,
    pub path_hint: Option<String>,
    pub session_count: u32,
    pub active_count: u32,
    /// RFC 3339, like `MobileSession.updated_at`.
    pub last_activity: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MobileProjectPage {
    pub projects: Vec<MobileProject>,
    pub next_cursor: Option<String>,
    pub share: MobileShareInfo,
    /// True when the host lacks the `projects` feature and the list was built from `session.list`.
    pub legacy: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MobileHolderKind {
    Tui,
    Gateway,
    Job,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MobileHolder {
    pub kind: MobileHolderKind,
    pub since: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MobileRowOrigin {
    Hosted,
    Profile,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MobileLastOrigin {
    Tui,
    Gateway,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MobileReadOnlyReason {
    ShareReadOnly,
    ForeignOrigin,
    NoWorkdir,
    NoSteer,
    Closed,
    Unknown,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct MobileSessionRow {
    pub session_id: String,
    pub title: Option<String>,
    pub project_key: Option<String>,
    pub updated_at: String,
    pub turns: Option<u64>,
    pub state: MobileSessionState,
    pub attached: u32,
    pub model: Option<String>,
    pub origin: MobileRowOrigin,
    pub continuable: bool,
    pub reason: Option<MobileReadOnlyReason>,
    pub holder: Option<MobileHolder>,
    pub last_origin: Option<MobileLastOrigin>,
    pub first_message: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct MobileSessionPage {
    pub sessions: Vec<MobileSessionRow>,
    pub next_cursor: Option<String>,
    pub legacy: bool,
}

/// Maps a wire session state to the mobile state (same mapping as `From<SessionSummary>`).
pub(crate) fn mobile_state(state: &HostedState) -> MobileSessionState {
    match state {
        HostedState::Idle => MobileSessionState::Idle,
        HostedState::Running => MobileSessionState::Running,
        HostedState::WaitingForApproval => MobileSessionState::WaitingForApproval,
        HostedState::WaitingForChild => MobileSessionState::WaitingForChild,
        HostedState::Queued { .. } => MobileSessionState::Queued,
        HostedState::Interrupted => MobileSessionState::Interrupted,
        HostedState::Failed => MobileSessionState::Failed,
        HostedState::Closed => MobileSessionState::Closed,
    }
}

impl From<ShareMode> for MobileShareMode {
    fn from(mode: ShareMode) -> Self {
        match mode {
            ShareMode::Off => Self::Off,
            ShareMode::Read => Self::Read,
            ShareMode::Continue => Self::Continue,
        }
    }
}

impl From<PermissionTier> for MobileTier {
    fn from(tier: PermissionTier) -> Self {
        match tier {
            PermissionTier::Observer => Self::Observer,
            PermissionTier::Operator => Self::Operator,
            PermissionTier::Maintainer => Self::Maintainer,
            PermissionTier::Owner => Self::Owner,
        }
    }
}

impl From<ShareInfo> for MobileShareInfo {
    fn from(info: ShareInfo) -> Self {
        Self {
            mode: info.mode.into(),
            tier: info.tier.into(),
        }
    }
}

impl From<ProjectSummary> for MobileProject {
    fn from(project: ProjectSummary) -> Self {
        Self {
            project_key: project.project_key,
            label: project.label,
            path_hint: project.path_hint,
            session_count: project.session_count,
            active_count: project.active_count,
            last_activity: project.last_activity.to_string(),
        }
    }
}

impl From<HolderKind> for MobileHolderKind {
    fn from(kind: HolderKind) -> Self {
        match kind {
            HolderKind::Tui => Self::Tui,
            HolderKind::Gateway => Self::Gateway,
            HolderKind::Job => Self::Job,
            HolderKind::Unknown => Self::Unknown,
        }
    }
}

impl From<HolderInfo> for MobileHolder {
    fn from(holder: HolderInfo) -> Self {
        Self {
            kind: holder.kind.into(),
            since: holder.since.to_string(),
        }
    }
}

impl From<RowOrigin> for MobileRowOrigin {
    fn from(origin: RowOrigin) -> Self {
        match origin {
            RowOrigin::Hosted => Self::Hosted,
            RowOrigin::Profile => Self::Profile,
            RowOrigin::Unknown => Self::Unknown,
        }
    }
}

impl From<LastOrigin> for MobileLastOrigin {
    fn from(origin: LastOrigin) -> Self {
        match origin {
            LastOrigin::Tui => Self::Tui,
            LastOrigin::Gateway => Self::Gateway,
            LastOrigin::Unknown => Self::Unknown,
        }
    }
}

impl From<ReadOnlyReason> for MobileReadOnlyReason {
    fn from(reason: ReadOnlyReason) -> Self {
        match reason {
            ReadOnlyReason::ShareReadOnly => Self::ShareReadOnly,
            ReadOnlyReason::ForeignOrigin => Self::ForeignOrigin,
            ReadOnlyReason::NoWorkdir => Self::NoWorkdir,
            ReadOnlyReason::NoSteer => Self::NoSteer,
            ReadOnlyReason::Closed => Self::Closed,
            ReadOnlyReason::Unknown => Self::Unknown,
        }
    }
}

impl From<SessionRow> for MobileSessionRow {
    fn from(row: SessionRow) -> Self {
        Self {
            session_id: row.session_id.to_string(),
            title: row.title,
            project_key: row.project_key,
            updated_at: row.updated_at.to_string(),
            turns: row.turns,
            state: mobile_state(&row.state),
            attached: row.attached,
            model: row.model,
            origin: row.origin.into(),
            continuable: row.continuable,
            reason: row.reason.map(Into::into),
            holder: row.holder.map(Into::into),
            last_origin: row.last_origin.map(Into::into),
            first_message: row.first_message,
        }
    }
}

/// Provider family of a configured model route.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ProviderKind {
    Anthropic,
    OpenAi,
    OpenAiCompatible,
    DashScope,
    OpenRouter,
}

/// A model provider route stored in the app home.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ProviderRoute {
    /// Route id, also the provider file stem and the secret slot suffix.
    pub id: String,
    /// Provider family.
    pub kind: ProviderKind,
    /// Base URL of the provider API.
    pub base_url: String,
    /// Model used when the route is activated without an explicit model.
    pub default_model: String,
    /// Whether a secret is stored for this route.
    pub has_secret: bool,
    /// Whether this route is the active one.
    pub active: bool,
}

/// Suggested defaults for a provider family.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ProviderDefaults {
    /// Default base URL for the family.
    pub base_url: String,
    /// Wire API identifier for the family.
    pub api: String,
    /// Whether the family needs a secret to be usable.
    pub secret_required: bool,
    /// Suggested model id for the family.
    pub suggested_model: String,
}

/// Result class of a provider route validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ValidationOutcome {
    Valid,
    DiscoveryUnsupported,
    Rejected,
    Unreachable,
    Failed,
    InvalidRoute,
    NotChecked,
}

/// Outcome of validating a provider route against its endpoint.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ProviderValidation {
    /// Result class of the validation.
    pub outcome: ValidationOutcome,
    /// HTTP status of the failed discovery, if any.
    pub http_status: Option<u16>,
    /// Distinct models reported (capped at 2000).
    pub model_count: u32,
    /// First 200 model ids in provider order.
    pub models: Vec<String>,
    /// Safe English diagnostic, never a body or key.
    pub detail: Option<String>,
    /// Whether the route and secret were persisted.
    pub saved: bool,
}

/// Paths and active selection of the on-device harw home.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AppHomeInfo {
    /// Absolute Android files directory.
    pub files_dir: String,
    /// Absolute harw home directory.
    pub home: String,
    /// Absolute active profile directory.
    pub profile_dir: String,
    /// Absolute workspace directory (the file jail).
    pub workspace: String,
    /// Absolute state directory of the local host.
    pub state_dir: String,
    /// Active UIA definition id, if set.
    pub active_uia: Option<String>,
    /// Active provider route id, if set.
    pub active_provider: Option<String>,
    /// Active model id, if set.
    pub active_model: Option<String>,
}

/// Where a session host runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum HostKind {
    Local,
    Server,
    Cloud,
}

/// A session host the phone can talk to.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct HostRef {
    /// Stable host id.
    pub id: String,
    /// Where the host runs.
    pub kind: HostKind,
    /// Human-readable label.
    pub label: String,
    /// Whether the host is currently reachable.
    pub reachable: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_types::SessionId;

    fn timestamp(raw: &str) -> jiff::Timestamp {
        raw.parse().expect("valid timestamp")
    }

    fn full_row() -> SessionRow {
        SessionRow {
            session_id: SessionId::try_from_str("session-a").expect("id"),
            title: Some("Titel".to_owned()),
            project_key: Some("demo-0123".to_owned()),
            updated_at: timestamp("2026-01-02T03:04:05Z"),
            turns: Some(7),
            state: HostedState::Queued { depth: 2 },
            attached: 3,
            model: Some("model-x".to_owned()),
            origin: RowOrigin::Profile,
            continuable: false,
            reason: Some(ReadOnlyReason::ShareReadOnly),
            holder: Some(HolderInfo {
                kind: HolderKind::Gateway,
                since: timestamp("2026-01-02T03:00:00Z"),
            }),
            last_origin: Some(LastOrigin::Tui),
            first_message: Some("Hallo".to_owned()),
        }
    }

    #[test]
    fn session_row_maps_all_fields() {
        let row: MobileSessionRow = full_row().into();
        assert_eq!(row.session_id, "session-a");
        assert_eq!(row.title.as_deref(), Some("Titel"));
        assert_eq!(row.project_key.as_deref(), Some("demo-0123"));
        assert_eq!(row.updated_at, "2026-01-02T03:04:05Z");
        assert_eq!(row.turns, Some(7));
        assert_eq!(row.state, MobileSessionState::Queued);
        assert_eq!(row.attached, 3);
        assert_eq!(row.model.as_deref(), Some("model-x"));
        assert_eq!(row.origin, MobileRowOrigin::Profile);
        assert!(!row.continuable);
        assert_eq!(row.reason, Some(MobileReadOnlyReason::ShareReadOnly));
        let holder = row.holder.expect("holder");
        assert_eq!(holder.kind, MobileHolderKind::Gateway);
        assert_eq!(holder.since, "2026-01-02T03:00:00Z");
        assert_eq!(row.last_origin, Some(MobileLastOrigin::Tui));
        assert_eq!(row.first_message.as_deref(), Some("Hallo"));
    }

    #[test]
    fn unknown_enums_map_to_unknown() {
        assert_eq!(
            MobileRowOrigin::from(RowOrigin::Unknown),
            MobileRowOrigin::Unknown
        );
        assert_eq!(
            MobileLastOrigin::from(LastOrigin::Unknown),
            MobileLastOrigin::Unknown
        );
        assert_eq!(
            MobileReadOnlyReason::from(ReadOnlyReason::Unknown),
            MobileReadOnlyReason::Unknown
        );
        assert_eq!(
            MobileHolderKind::from(HolderKind::Unknown),
            MobileHolderKind::Unknown
        );
    }

    #[test]
    fn busy_port_error_maps_to_mobile_busy() {
        let error = MobileError::from(PortError::Busy("laeuft".to_owned()));
        assert!(matches!(error, MobileError::Busy { ref detail } if detail == "laeuft"));
        assert_eq!(error.to_string(), "busy: laeuft");
    }

    #[test]
    fn project_and_share_map() {
        let project: MobileProject = ProjectSummary {
            project_key: String::new(),
            label: "Ohne Projekt".to_owned(),
            path_hint: Some("~/a/b".to_owned()),
            session_count: 4,
            last_activity: timestamp("2026-01-02T03:04:05Z"),
            active_count: 1,
        }
        .into();
        assert_eq!(project.project_key, "");
        assert_eq!(project.path_hint.as_deref(), Some("~/a/b"));
        assert_eq!(project.session_count, 4);
        assert_eq!(project.active_count, 1);
        assert_eq!(project.last_activity, "2026-01-02T03:04:05Z");
        let share: MobileShareInfo = ShareInfo {
            mode: ShareMode::Continue,
            tier: PermissionTier::Maintainer,
        }
        .into();
        assert_eq!(share.mode, MobileShareMode::Continue);
        assert_eq!(share.tier, MobileTier::Maintainer);
    }
}
