use harw_protocol::{Cursor, PortError};
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
    #[error("mobile core: {detail}")]
    Internal { detail: String },
}

impl From<PortError> for MobileError {
    fn from(error: PortError) -> Self {
        match error {
            PortError::Transport(detail) => Self::Transport { detail },
            PortError::Denied(detail) => Self::Denied { detail },
            PortError::Revoked => Self::Revoked,
            other => Self::Session {
                detail: other.to_string(),
            },
        }
    }
}
