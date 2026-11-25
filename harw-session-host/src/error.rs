//! Host error type and its mapping onto the client-facing port error.

use std::fmt;

use harw_protocol::{PortError, ToolRefusal};

/// Why a host operation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostError {
    /// A call other than hello arrived before a successful hello.
    HelloRequired,
    /// Capability or tenant admission refused the call.
    Denied(String),
    /// Unknown session or approval (also used for another tenant's session,
    /// so existence does not leak across tenants).
    NotFound,
    /// The caller's authority was revoked.
    Revoked,
    /// Malformed or contradictory request.
    Protocol(String),
    /// Durable state could not be read or written.
    Storage(String),
    /// The runtime driver failed.
    Driver(String),
    /// R18: a `tool.call` was refused before it ran (unknown tool, not
    /// granted, sandbox unavailable, draining, duplicate call id).
    ToolRefused {
        refusal: ToolRefusal,
        detail: String,
    },
    /// Plan §3.5: Lease belegt; Anzeigetext.
    Busy(String),
    /// Skeleton stub: the named entry point is not implemented yet (W00
    /// integration map). Never returned by finished code.
    NotImplemented(&'static str),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HelloRequired => f.write_str("session.hello required first"),
            Self::Denied(reason) => write!(f, "denied: {reason}"),
            Self::NotFound => f.write_str("not found"),
            Self::Revoked => f.write_str("revoked"),
            Self::Protocol(detail) => write!(f, "protocol: {detail}"),
            Self::Storage(detail) => write!(f, "storage: {detail}"),
            Self::Driver(detail) => write!(f, "driver: {detail}"),
            Self::Busy(reason) => write!(f, "busy: {reason}"),
            Self::NotImplemented(what) => write!(f, "not implemented: {what}"),
            Self::ToolRefused { refusal, detail } => {
                write!(f, "tool refused ({}): {detail}", refusal.code())
            }
        }
    }
}

impl std::error::Error for HostError {}

impl From<HostError> for PortError {
    fn from(error: HostError) -> Self {
        match error {
            HostError::HelloRequired => Self::Denied("session.hello required first".into()),
            HostError::Denied(reason) => Self::Denied(reason),
            HostError::NotFound => Self::NotFound,
            HostError::Revoked => Self::Revoked,
            HostError::Protocol(detail) => Self::Protocol(detail),
            HostError::ToolRefused { refusal, detail } => Self::ToolRefused { refusal, detail },
            HostError::Busy(reason) => Self::Busy(reason),
            // Storage and driver details stay on the host (logs); the client
            // only learns that the host failed.
            HostError::Storage(_) | HostError::Driver(_) | HostError::NotImplemented(_) => {
                Self::Transport("host error".into())
            }
        }
    }
}

impl From<harw_session_store::SessionStoreError> for HostError {
    fn from(error: harw_session_store::SessionStoreError) -> Self {
        match error {
            harw_session_store::SessionStoreError::LeaseHeld { holder, .. } => {
                Self::Busy(crate::index::busy_reason(holder.map(|holder| holder.kind)).to_owned())
            }
            other => Self::Storage(other.to_string()),
        }
    }
}

impl From<std::io::Error> for HostError {
    fn from(error: std::io::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use harw_protocol::PortError;
    use harw_session_store::{HolderKind, LeaseHolder, SessionStoreError};
    use harw_types::SessionId;

    use super::HostError;

    #[test]
    fn lease_held_maps_to_busy_with_display_text() {
        let session = SessionId::from_str("sess-a1");
        let none = HostError::from(SessionStoreError::LeaseHeld {
            session: session.clone(),
            holder: None,
        });
        assert!(matches!(none, HostError::Busy(ref r) if r == "Session ist gerade belegt"));

        let tui = HostError::from(SessionStoreError::LeaseHeld {
            session,
            holder: Some(LeaseHolder {
                kind: HolderKind::Tui,
                pid: 1,
                since: jiff::Timestamp::UNIX_EPOCH,
            }),
        });
        assert!(matches!(tui, HostError::Busy(ref r) if r == "Läuft gerade in der TUI"));
    }

    #[test]
    fn busy_reaches_port_unchanged() {
        assert_eq!(
            PortError::from(HostError::Busy("x".into())),
            PortError::Busy("x".into())
        );
    }
}
