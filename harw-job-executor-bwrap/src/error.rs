//! Errors of the Bubblewrap executor.

use std::fmt;
use std::path::PathBuf;

use harw_job_core::SpecError;
use harw_sandbox::SandboxError;

/// The part of the launch a [`BwrapExecutorError::Unsupported`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dimension {
    /// The backend itself (no trusted `bwrap` on this host).
    Backend,
    /// Filesystem access lists.
    Filesystem,
    /// Network policy.
    Network,
    /// Capability policy.
    Capabilities,
}

impl fmt::Display for Dimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Backend => "backend",
            Self::Filesystem => "filesystem",
            Self::Network => "network",
            Self::Capabilities => "capabilities",
        })
    }
}

/// Why a job could not be planned for Bubblewrap.
#[derive(Debug)]
#[non_exhaustive]
pub enum BwrapExecutorError {
    /// The request cannot be expressed with the Bubblewrap backend. It is
    /// refused rather than silently widened.
    Unsupported {
        /// Affected part of the launch.
        dimension: Dimension,
        /// Human-readable explanation.
        reason: String,
    },
    /// The job spec failed validation.
    InvalidSpec(SpecError),
    /// A policy path is relative or contains `.`/`..` components.
    InvalidPath {
        /// The rejected path.
        path: PathBuf,
    },
    /// The workspace root could not be resolved (missing, not a directory,
    /// not canonicalizable).
    Workspace {
        /// The workspace root as passed in.
        path: PathBuf,
        /// Rendered cause.
        reason: String,
    },
    /// The configured `bwrap` path is not absolute (it would be resolved via
    /// `PATH`).
    RelativeExecutable {
        /// The rejected path.
        path: PathBuf,
    },
    /// `harw-sandbox` rejected the launch plan.
    Sandbox(SandboxError),
    /// The argv produced by `harw-sandbox` did not have the documented
    /// `--chdir <workspace> -- <command>` tail (internal invariant).
    PlanShape,
}

impl fmt::Display for BwrapExecutorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { dimension, reason } => {
                write!(
                    f,
                    "bubblewrap executor cannot enforce {dimension}: {reason}"
                )
            }
            Self::InvalidSpec(error) => write!(f, "invalid job spec: {error}"),
            Self::InvalidPath { path } => write!(
                f,
                "sandbox policy path '{}' must be absolute without '.' or '..'",
                path.display()
            ),
            Self::Workspace { path, reason } => {
                write!(f, "workspace root '{}': {reason}", path.display())
            }
            Self::RelativeExecutable { path } => write!(
                f,
                "bubblewrap executable '{}' must be an absolute path",
                path.display()
            ),
            Self::Sandbox(error) => write!(f, "bubblewrap plan rejected: {error}"),
            Self::PlanShape => {
                f.write_str("bubblewrap plan did not end in '--chdir <workspace> -- <command>'")
            }
        }
    }
}

impl std::error::Error for BwrapExecutorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidSpec(error) => Some(error),
            Self::Sandbox(error) => Some(error),
            _ => None,
        }
    }
}

impl From<SandboxError> for BwrapExecutorError {
    fn from(error: SandboxError) -> Self {
        Self::Sandbox(error)
    }
}

impl From<SpecError> for BwrapExecutorError {
    fn from(error: SpecError) -> Self {
        Self::InvalidSpec(error)
    }
}
