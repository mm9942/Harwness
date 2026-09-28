//! Typed coordinator errors (Job-Runtime-Doc §22).
//!
//! Platform failures are mapped at the executor boundary into the
//! categories a caller has to tell apart: unsupported feature, permission
//! denied, resource exhausted, identity mismatch, process already exited,
//! cgroup unavailable, sandbox not (fully) enforced, store conflict, lease
//! lost. No Linux or Darwin error type crosses this boundary, so the
//! coordinator stays platform-neutral.

use std::fmt;
use std::io;

use harw_job_core::{SandboxReport, SpecError, TransitionError};
use harw_job_store::StoreError;

/// Error of every coordinator and executor operation.
#[derive(Debug)]
#[non_exhaustive]
pub enum RuntimeError {
    /// The submitted spec (or envelope) is invalid.
    InvalidSpec(SpecError),
    /// The coordinator configuration is invalid.
    InvalidConfig {
        /// What is wrong.
        detail: String,
    },
    /// The job record's state rejects the operation (store conflict:
    /// already exists, not claimable, already terminal, lock contended).
    StoreConflict(StoreError),
    /// The coordinator's lease no longer authorizes a mutation (lost,
    /// superseded or expired). Nothing was written.
    LeaseLost(StoreError),
    /// Any other store failure (I/O, corrupt record, encoding).
    Store(StoreError),
    /// An attempt sidecar could not be encoded or decoded.
    AttemptRecord {
        /// Attempt id.
        attempt: String,
        /// Rendered cause.
        detail: String,
    },
    /// A lifecycle transition was rejected by the state machine.
    Transition(TransitionError),
    /// The platform or configuration lacks a required feature.
    Unsupported {
        /// The operation that needed it.
        operation: &'static str,
        /// Rendered cause.
        detail: String,
    },
    /// The OS denied an operation.
    PermissionDenied {
        /// The denied operation.
        operation: &'static str,
        /// Rendered cause.
        detail: String,
    },
    /// A kernel or process resource ran out (fds, memory, pids).
    ResourceExhausted {
        /// The failed operation.
        operation: &'static str,
        /// Rendered cause.
        detail: String,
    },
    /// A persisted process identity does not match the live process (or
    /// cannot be proven). The process was not signalled.
    IdentityMismatch {
        /// Diagnostic PID.
        pid: u32,
        /// Why the identity did not match.
        detail: String,
    },
    /// The process had already exited.
    ProcessAlreadyExited {
        /// Diagnostic PID.
        pid: u32,
    },
    /// The configured cgroup root cannot be used.
    CgroupUnavailable {
        /// Rendered cause.
        detail: String,
    },
    /// The job's [`harw_job_core::SandboxRequirement::Required`] cannot be
    /// met by the achieved enforcement (sandbox only partially enforced).
    /// The job body did not run.
    SandboxRequirementNotMet {
        /// What the executor could enforce.
        report: SandboxReport,
        /// Dimensions that are not fully enforced.
        shortfalls: Vec<&'static str>,
    },
    /// The sandbox backend rejected the policy or spec.
    Sandbox {
        /// Rendered cause.
        detail: String,
    },
    /// Spawning the job failed.
    Spawn {
        /// Program that could not be started.
        program: String,
        /// OS error.
        source: io::Error,
    },
    /// Any other OS-level failure.
    Os {
        /// The failed operation.
        operation: &'static str,
        /// Rendered cause.
        detail: String,
    },
    /// A background task of the coordinator ended without a result
    /// (panicked, aborted, or the runtime shut down).
    TaskFailed {
        /// Rendered cause.
        detail: String,
    },
}

impl RuntimeError {
    /// Classifies a store error: conflicts and lost leases get their own
    /// variants so callers can react without string matching.
    #[must_use]
    pub fn from_store(error: StoreError) -> Self {
        match error {
            StoreError::StaleLease { .. } => Self::LeaseLost(error),
            StoreError::Conflict { .. }
            | StoreError::AlreadyExists { .. }
            | StoreError::Contended { .. } => Self::StoreConflict(error),
            StoreError::Unsupported { operation, detail } => {
                Self::Unsupported { operation, detail }
            }
            other => Self::Store(other),
        }
    }

    /// Whether this error means the lease was lost.
    #[must_use]
    pub fn is_lease_lost(&self) -> bool {
        matches!(self, Self::LeaseLost(_))
    }

    /// Whether this is a store conflict.
    #[must_use]
    pub fn is_store_conflict(&self) -> bool {
        matches!(self, Self::StoreConflict(_))
    }

    pub(crate) fn task(detail: impl fmt::Display) -> Self {
        Self::TaskFailed {
            detail: detail.to_string(),
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSpec(error) => write!(f, "invalid job spec: {error}"),
            Self::InvalidConfig { detail } => {
                write!(f, "invalid coordinator configuration: {detail}")
            }
            Self::StoreConflict(error) => write!(f, "store conflict: {error}"),
            Self::LeaseLost(error) => write!(f, "lease lost: {error}"),
            Self::Store(error) => write!(f, "job store error: {error}"),
            Self::AttemptRecord { attempt, detail } => {
                write!(f, "attempt record '{attempt}' is unusable: {detail}")
            }
            Self::Transition(error) => write!(f, "{error}"),
            Self::Unsupported { operation, detail } => {
                write!(f, "{operation} is not supported: {detail}")
            }
            Self::PermissionDenied { operation, detail } => {
                write!(f, "{operation}: permission denied: {detail}")
            }
            Self::ResourceExhausted { operation, detail } => {
                write!(f, "{operation}: resource exhausted: {detail}")
            }
            Self::IdentityMismatch { pid, detail } => {
                write!(
                    f,
                    "process {pid} does not match its recovery identity: {detail}"
                )
            }
            Self::ProcessAlreadyExited { pid } => write!(f, "process {pid} has already exited"),
            Self::CgroupUnavailable { detail } => write!(f, "cgroup unavailable: {detail}"),
            Self::SandboxRequirementNotMet { shortfalls, .. } => write!(
                f,
                "sandbox requirement not met: not fully enforced: {}",
                shortfalls.join(", ")
            ),
            Self::Sandbox { detail } => write!(f, "sandbox backend rejected the job: {detail}"),
            Self::Spawn { program, source } => write!(f, "cannot spawn '{program}': {source}"),
            Self::Os { operation, detail } => write!(f, "{operation} failed: {detail}"),
            Self::TaskFailed { detail } => write!(f, "coordinator task failed: {detail}"),
        }
    }
}

impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidSpec(error) => Some(error),
            Self::StoreConflict(error) | Self::LeaseLost(error) | Self::Store(error) => Some(error),
            Self::Transition(error) => Some(error),
            Self::Spawn { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<SpecError> for RuntimeError {
    fn from(error: SpecError) -> Self {
        Self::InvalidSpec(error)
    }
}

impl From<TransitionError> for RuntimeError {
    fn from(error: TransitionError) -> Self {
        Self::Transition(error)
    }
}

impl From<StoreError> for RuntimeError {
    fn from(error: StoreError) -> Self {
        Self::from_store(error)
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeError;
    use harw_job_core::{EnforcementState, SandboxReport};
    use harw_job_store::{Conflict, StaleLease, StoreError};

    #[test]
    fn store_errors_are_classified() {
        let lost = RuntimeError::from_store(StoreError::StaleLease {
            id: "j".into(),
            reason: StaleLease::Missing,
        });
        assert!(lost.is_lease_lost());
        let conflict = RuntimeError::from_store(StoreError::Conflict {
            id: "j".into(),
            conflict: Conflict::InvalidLeaseTtl,
        });
        assert!(conflict.is_store_conflict());
        assert!(
            RuntimeError::from_store(StoreError::Contended { id: "j".into() }).is_store_conflict()
        );
        assert!(matches!(
            RuntimeError::from_store(StoreError::NotFound { id: "j".into() }),
            RuntimeError::Store(_)
        ));
    }

    #[test]
    fn requirement_error_names_shortfalls() {
        let error = RuntimeError::SandboxRequirementNotMet {
            report: SandboxReport::uniform(EnforcementState::NotEnforced),
            shortfalls: vec!["filesystem", "network"],
        };
        assert_eq!(
            error.to_string(),
            "sandbox requirement not met: not fully enforced: filesystem, network"
        );
    }
}
