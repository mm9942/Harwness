//! Errors of [`crate::ComServer`].

use std::fmt;

/// Why a connection was not served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComError {
    /// `max_connections` are in use; the connection is dropped unread.
    Busy,
    /// The listener built an identity that fails
    /// [`harw_session_host::ClientIdentity::validate`] (for example a remote
    /// caller without a tenant). This is a listener bug, so the connection is
    /// dropped instead of being served with a guess.
    InvalidIdentity,
}

impl fmt::Display for ComError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("connection limit reached"),
            Self::InvalidIdentity => f.write_str("listener built an invalid client identity"),
        }
    }
}

impl std::error::Error for ComError {}
