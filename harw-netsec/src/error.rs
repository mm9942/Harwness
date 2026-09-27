//! Error type of the NetSec daemon.
//!
//! # Responsibility
//! One enum for every failure the library can report: validation of wire
//! input, state-machine violations, store integrity, configuration and socket
//! setup. The HTTP layer maps each variant onto a status code in
//! [`crate::server`]; internal variants (I/O, corruption, paths) are never
//! echoed to a client verbatim.

use std::fmt;
use std::path::PathBuf;

use crate::state_machine::{NodeEvent, NodeState};

/// Result alias of this crate.
pub type NetsecResult<T> = Result<T, NetsecError>;

/// Longest value echoed back inside an [`NetsecError::InvalidIdentifier`].
const ECHO_LIMIT: usize = 64;

/// Every failure of the NetSec library.
#[derive(Debug)]
pub enum NetsecError {
    /// An identifier (node, zone, route) is empty, too long or contains
    /// characters outside `[A-Za-z0-9._-]`.
    InvalidIdentifier {
        /// Which identifier kind was rejected.
        kind: &'static str,
        /// The rejected value, truncated to a short prefix.
        value: String,
    },
    /// A request or record field violates its constraints.
    InvalidField {
        /// Field name as it appears on the wire.
        field: &'static str,
        /// Human-readable constraint that was violated.
        reason: &'static str,
    },
    /// The node state machine has no edge for this `(state, event)` pair.
    InvalidTransition {
        /// Current state.
        from: NodeState,
        /// Requested event.
        event: NodeEvent,
    },
    /// A node with this id is already registered.
    NodeExists {
        /// The duplicate id.
        id: String,
    },
    /// No node with this id is known.
    NodeNotFound {
        /// The missing id.
        id: String,
    },
    /// The referenced zone is not configured.
    UnknownZone {
        /// The unknown zone id.
        zone: String,
    },
    /// A route references something that does not exist or is revoked.
    InvalidRoute {
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The configured node limit is reached.
    CapacityExhausted {
        /// Configured `max_nodes`.
        limit: usize,
    },
    /// The persisted state file is unreadable, malformed or inconsistent.
    /// The daemon refuses to start instead of silently resetting topology.
    StoreCorrupt {
        /// The state file.
        path: PathBuf,
        /// Why it was rejected.
        reason: String,
    },
    /// The persisted state file exceeds the size limit.
    StoreTooLarge {
        /// The state file.
        path: PathBuf,
        /// The limit in bytes.
        limit: u64,
    },
    /// Another process holds the store's single-writer lock.
    StoreLocked {
        /// The lock file.
        path: PathBuf,
    },
    /// A path that must be a regular file or directory is a symbolic link.
    SymlinkRejected {
        /// The offending path.
        path: PathBuf,
    },
    /// A filesystem or socket operation failed.
    Io {
        /// What was being attempted.
        context: &'static str,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The configuration is missing, malformed or violates a constraint.
    Config {
        /// Why it was rejected.
        reason: String,
    },
    /// Something other than a socket exists at the socket path.
    SocketPathOccupied {
        /// The socket path.
        path: PathBuf,
    },
    /// A live process already listens on the socket path.
    SocketInUse {
        /// The socket path.
        path: PathBuf,
    },
    /// The systemd socket-activation environment is missing or malformed.
    Activation {
        /// Why activation was rejected.
        reason: String,
    },
    /// An internal lock was poisoned by a panicking thread.
    Poisoned,
    /// A blocking store task could not be joined.
    TaskFailed,
}

impl NetsecError {
    /// Builds an [`NetsecError::InvalidIdentifier`], truncating `value`.
    pub(crate) fn invalid_identifier(kind: &'static str, value: &str) -> Self {
        Self::InvalidIdentifier {
            kind,
            value: value.chars().take(ECHO_LIMIT).collect(),
        }
    }

    /// Returns a `map_err` adapter that wraps an I/O error with `context`.
    pub(crate) fn io(context: &'static str) -> impl FnOnce(std::io::Error) -> Self {
        move |source| Self::Io { context, source }
    }
}

impl fmt::Display for NetsecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentifier { kind, value } => {
                write!(f, "invalid {kind} identifier: {value:?}")
            }
            Self::InvalidField { field, reason } => write!(f, "invalid field `{field}`: {reason}"),
            Self::InvalidTransition { from, event } => write!(
                f,
                "transition `{}` is not allowed from state `{}`",
                event.as_str(),
                from.as_str()
            ),
            Self::NodeExists { id } => write!(f, "node `{id}` is already registered"),
            Self::NodeNotFound { id } => write!(f, "node `{id}` is not registered"),
            Self::UnknownZone { zone } => write!(f, "zone `{zone}` is not configured"),
            Self::InvalidRoute { reason } => write!(f, "invalid route: {reason}"),
            Self::CapacityExhausted { limit } => {
                write!(f, "node capacity exhausted (max_nodes = {limit})")
            }
            Self::StoreCorrupt { path, reason } => {
                write!(f, "state file {} is corrupt: {reason}", path.display())
            }
            Self::StoreTooLarge { path, limit } => write!(
                f,
                "state file {} exceeds the size limit of {limit} bytes",
                path.display()
            ),
            Self::StoreLocked { path } => write!(
                f,
                "state lock {} is held by another process",
                path.display()
            ),
            Self::SymlinkRejected { path } => {
                write!(f, "{} must not be a symbolic link", path.display())
            }
            Self::Io { context, source } => write!(f, "{context}: {source}"),
            Self::Config { reason } => write!(f, "invalid configuration: {reason}"),
            Self::SocketPathOccupied { path } => write!(
                f,
                "socket path {} is occupied by a non-socket file",
                path.display()
            ),
            Self::SocketInUse { path } => write!(
                f,
                "socket path {} is in use by a live listener",
                path.display()
            ),
            Self::Activation { reason } => write!(f, "systemd socket activation: {reason}"),
            Self::Poisoned => f.write_str("internal state lock poisoned"),
            Self::TaskFailed => f.write_str("blocking store task failed"),
        }
    }
}

impl std::error::Error for NetsecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
