use std::fmt;

use harw_authority::AuthorityError;
use harw_channel::ChannelError;
use harw_types::{ChannelId, PeerId, TenantId, WorkId};

pub type TelegramChannelResult<T> = Result<T, TelegramChannelError>;

/// Failures local to the Telegram binding's boundary and eventual transport.
pub enum TelegramChannelError {
    ChannelMismatch {
        expected: ChannelId,
        actual: ChannelId,
    },
    IngressUnavailable,
    UnpairedPeer {
        peer: PeerId,
    },
    Pairing(ChannelError),
    /// `/request`'s workspace alias did not resolve through the authoritative
    /// `harw_authority::WorkspaceRegistry` for the requester's paired tenant
    /// (docs/design/telegram-sandbox-work-requests.md, "Typed request boundary").
    WorkspaceUnresolved {
        alias: String,
        tenant: TenantId,
        source: AuthorityError,
    },
    /// `/request`'s role argument failed the closed-grammar atom check.
    InvalidWorkRequestRole {
        role: String,
    },
    /// `/review`, `/approve`, `/deny`, or `/cancel` named a `WorkId` this
    /// binding's durable store has no record of.
    WorkRequestNotFound {
        work_id: WorkId,
    },
    /// A lifecycle command was attempted from a state that does not permit
    /// it (e.g. `/approve` on an already-`Denied` request).
    WorkRequestInvalidTransition {
        work_id: WorkId,
        from: &'static str,
        action: &'static str,
    },
    /// An approved work request cannot launch because no
    /// [`crate::WorkLauncher`] is installed on the store.
    LaunchNotYetAvailable {
        work_id: WorkId,
    },
    /// The durable work-request store could not read/write its backing file.
    Io(std::io::Error),
    /// The durable work-request store could not (de)serialize a record.
    Serde(serde_json::Error),
}

impl fmt::Display for TelegramChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChannelMismatch { expected, actual } => {
                write!(
                    f,
                    "event for channel '{actual}' cannot enter Telegram binding '{expected}'"
                )
            }
            Self::IngressUnavailable => f.write_str("Telegram HTTP ingress is not configured"),
            Self::UnpairedPeer { peer } => write!(f, "Telegram peer '{peer}' is not paired"),
            Self::Pairing(err) => write!(f, "pairing store error: {err}"),
            Self::WorkspaceUnresolved {
                alias,
                tenant,
                source,
            } => write!(
                f,
                "workspace alias '{alias}' does not resolve for tenant '{tenant}': {source}"
            ),
            Self::InvalidWorkRequestRole { role } => {
                write!(f, "work-request role '{role}' is not a valid atom")
            }
            Self::WorkRequestNotFound { work_id } => {
                write!(f, "work request '{work_id}' is unknown")
            }
            Self::WorkRequestInvalidTransition {
                work_id,
                from,
                action,
            } => write!(
                f,
                "work request '{work_id}' cannot be {action} from state '{from}'"
            ),
            Self::LaunchNotYetAvailable { work_id } => write!(
                f,
                "work request '{work_id}' was approved, but no work launcher is installed"
            ),
            Self::Io(err) => write!(f, "work-request store I/O error: {err}"),
            Self::Serde(err) => write!(f, "work-request store serialization error: {err}"),
        }
    }
}

impl fmt::Debug for TelegramChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TelegramChannelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Pairing(err) => Some(err),
            Self::WorkspaceUnresolved { source, .. } => Some(source),
            Self::Io(err) => Some(err),
            Self::Serde(err) => Some(err),
            _ => None,
        }
    }
}

impl From<ChannelError> for TelegramChannelError {
    fn from(value: ChannelError) -> Self {
        Self::Pairing(value)
    }
}

impl From<std::io::Error> for TelegramChannelError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for TelegramChannelError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value)
    }
}
