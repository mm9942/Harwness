//! Typed store errors (Job-Runtime-Doc §22).
//!
//! Higher layers must be able to tell a *store conflict* (the record is in a
//! state that does not admit the mutation) from a *lost lease* (the caller's
//! fencing credential is stale) from plain I/O or corruption. Every variant
//! carries the record id as a plain string so an adapter can rebuild its own
//! identifier type without this crate knowing it.

use std::fmt;
use std::io;

use harw_job_core::JobState;
use jiff::Timestamp;

/// Result alias of this crate.
pub type StoreResult<T> = Result<T, StoreError>;

/// Why a lease credential no longer authorizes a mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleLease {
    /// The record carries no active lease (released, reclaimed, cancelled).
    Missing,
    /// The presented token does not match the current lease, or its fencing
    /// epoch is older than the record's current epoch.
    TokenMismatch,
    /// The token matches, but the lease expired at `expired_at`.
    Expired {
        /// Expiry instant of the lease.
        expired_at: Timestamp,
    },
}

/// Why a record's current state rejects the requested mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conflict {
    /// Only a `Ready` job can be claimed.
    NotClaimable {
        /// Current state.
        state: JobState,
    },
    /// The job is `Ready` but not yet eligible.
    NotEligible {
        /// Earliest claim instant.
        not_before: Timestamp,
    },
    /// The job already reached a terminal state.
    AlreadyTerminal {
        /// Current state.
        state: JobState,
    },
    /// Only `Pending`, `Ready` or `Running` jobs can be cancelled.
    NotCancellable {
        /// Current state.
        state: JobState,
    },
    /// A lease TTL must be strictly positive.
    InvalidLeaseTtl,
    /// A cancellation needs a non-empty reason.
    EmptyReason,
    /// The fencing epoch cannot be advanced any further (fails closed).
    EpochExhausted,
    /// The job model rejected the transition (state machine, time arithmetic).
    Rejected {
        /// Rendered cause.
        detail: String,
    },
}

/// Error of every store operation.
#[derive(Debug)]
pub enum StoreError {
    /// No record exists under `id`.
    NotFound {
        /// Record id.
        id: String,
    },
    /// A record already exists under `id`; it is never overwritten.
    AlreadyExists {
        /// Record id.
        id: String,
    },
    /// Another writer holds the record lock (try-lock, never blocks).
    Contended {
        /// Record id.
        id: String,
    },
    /// `id` is not a safe single path component (`[A-Za-z0-9_-]+`).
    InvalidId {
        /// The rejected id.
        id: String,
    },
    /// The record exists but cannot be decoded or is keyed to another id.
    Corrupt {
        /// Record id.
        id: String,
        /// Rendered cause.
        detail: String,
    },
    /// The record state rejects the mutation (store conflict).
    Conflict {
        /// Record id.
        id: String,
        /// Precise reason.
        conflict: Conflict,
    },
    /// The caller's lease no longer authorizes a mutation (lease lost).
    StaleLease {
        /// Record id.
        id: String,
        /// Precise reason.
        reason: StaleLease,
    },
    /// The record could not be encoded.
    Encode {
        /// Record id.
        id: String,
        /// Encoder error.
        source: serde_json::Error,
    },
    /// Filesystem failure (including a rejected symbolic link).
    Io(io::Error),
    /// The platform or filesystem lacks a required primitive.
    Unsupported {
        /// The operation that needed it.
        operation: &'static str,
        /// Rendered cause.
        detail: String,
    },
}

impl StoreError {
    pub(crate) fn conflict(id: &str, conflict: Conflict) -> Self {
        Self::Conflict {
            id: id.to_owned(),
            conflict,
        }
    }

    pub(crate) fn stale(id: &str, reason: StaleLease) -> Self {
        Self::StaleLease {
            id: id.to_owned(),
            reason,
        }
    }

    /// Whether the error is a lost/stale lease (Job-Runtime-Doc §22).
    #[must_use]
    pub fn is_lease_lost(&self) -> bool {
        matches!(self, Self::StaleLease { .. })
    }

    /// Whether the error is a store conflict (state, existence or lock).
    #[must_use]
    pub fn is_conflict(&self) -> bool {
        matches!(
            self,
            Self::Conflict { .. } | Self::AlreadyExists { .. } | Self::Contended { .. }
        )
    }
}

impl fmt::Display for StaleLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("record carries no active lease"),
            Self::TokenMismatch => f.write_str("lease token does not match the current lease"),
            Self::Expired { expired_at } => write!(f, "lease expired at {expired_at}"),
        }
    }
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotClaimable { state } => write!(f, "not claimable from state {state:?}"),
            Self::NotEligible { not_before } => write!(f, "not eligible until {not_before}"),
            Self::AlreadyTerminal { state } => write!(f, "already terminal in state {state:?}"),
            Self::NotCancellable { state } => write!(f, "not cancellable from state {state:?}"),
            Self::InvalidLeaseTtl => f.write_str("lease TTL must be positive"),
            Self::EmptyReason => f.write_str("reason must not be empty"),
            Self::EpochExhausted => f.write_str("fencing epoch is exhausted"),
            Self::Rejected { detail } => write!(f, "transition rejected: {detail}"),
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { id } => write!(f, "record '{id}' was not found"),
            Self::AlreadyExists { id } => write!(f, "record '{id}' already exists"),
            Self::Contended { id } => write!(f, "record '{id}' is locked by another writer"),
            Self::InvalidId { id } => {
                write!(f, "record id is unsafe for filesystem storage: '{id}'")
            }
            Self::Corrupt { id, detail } => write!(f, "record '{id}' is corrupt: {detail}"),
            Self::Conflict { id, conflict } => write!(f, "record '{id}': {conflict}"),
            Self::StaleLease { id, reason } => write!(f, "record '{id}': stale lease: {reason}"),
            Self::Encode { id, source } => write!(f, "record '{id}' cannot be encoded: {source}"),
            Self::Io(error) => fmt::Display::fmt(error, f),
            Self::Unsupported { operation, detail } => {
                write!(f, "unsupported operation '{operation}': {detail}")
            }
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Encode { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
