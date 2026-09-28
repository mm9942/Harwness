//! Host error type and its mapping onto the client-facing port error.

use std::fmt;

use harw_protocol::PortError;

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
            // Storage and driver details stay on the host (logs); the client
            // only learns that the host failed.
            HostError::Storage(_) | HostError::Driver(_) => Self::Transport("host error".into()),
        }
    }
}

impl From<harw_session_store::SessionStoreError> for HostError {
    fn from(error: harw_session_store::SessionStoreError) -> Self {
        Self::Storage(error.to_string())
    }
}

impl From<std::io::Error> for HostError {
    fn from(error: std::io::Error) -> Self {
        Self::Storage(error.to_string())
    }
}
