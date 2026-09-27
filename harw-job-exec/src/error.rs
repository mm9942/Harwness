//! Typed failures of the trampoline and its parent-side helpers.
//!
//! Errors from `harw-job-linux` (cgroup, rlimit, sandbox) are carried as
//! rendered messages so this type stays platform-neutral; the dimension
//! that failed is still encoded in the variant.

use std::fmt;
use std::io;
use std::path::PathBuf;

use harw_job_core::SandboxRequirement;

use crate::exit_code;

/// A failure of `harw-job-exec`. [`ExecError::exit_code`] maps it to the
/// binary's exit status.
#[derive(Debug)]
#[non_exhaustive]
pub enum ExecError {
    /// Bad command line.
    Usage {
        /// What is wrong.
        message: String,
    },
    /// The trampoline only works on Linux.
    UnsupportedPlatform,
    /// The plan file could not be opened, read, created or removed.
    PlanIo {
        /// Plan file path.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
    /// The plan file is not a private regular file of the effective user.
    PlanInsecure {
        /// Plan file path.
        path: PathBuf,
        /// Which check failed.
        reason: String,
    },
    /// The serialized plan exceeds [`crate::plan::MAX_PLAN_BYTES`].
    PlanTooLarge {
        /// The cap in bytes.
        limit: usize,
    },
    /// The plan is not valid JSON for its declared version.
    PlanParse {
        /// Rendered `serde_json` error.
        message: String,
    },
    /// The plan declares a version this trampoline does not understand.
    UnsupportedVersion {
        /// Declared version.
        found: u64,
    },
    /// The plan is well-formed but semantically invalid.
    InvalidPlan {
        /// Which rule is violated.
        reason: String,
    },
    /// The report file could not be created, written or read.
    ReportIo {
        /// Report file path.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
    /// A complete report file does not contain a valid report.
    ReportParse {
        /// Report file path.
        path: PathBuf,
        /// Rendered `serde_json` error.
        message: String,
    },
    /// Joining the job cgroup failed.
    Cgroup {
        /// Rendered `CgroupError`.
        message: String,
    },
    /// Applying the rlimits failed.
    Rlimit {
        /// Rendered `ProcessError`.
        message: String,
    },
    /// Applying the sandbox failed; the process may already be partially
    /// restricted and never `exec`s the job.
    Sandbox {
        /// Rendered `SandboxError`.
        message: String,
    },
    /// The achieved report does not satisfy the plan's requirement.
    RequirementNotMet {
        /// The requirement of the plan.
        requirement: SandboxRequirement,
        /// Dimensions that are not fully enforced.
        shortfalls: Vec<&'static str>,
    },
    /// `execve` of the job program failed.
    Exec {
        /// Program that was to be executed.
        program: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
}

impl ExecError {
    /// Exit status of the binary for this error (see [`crate::exit_code`]).
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Sandbox { .. } | Self::RequirementNotMet { .. } => exit_code::SANDBOX_REFUSED,
            Self::Exec { .. } => exit_code::EXEC_FAILED,
            Self::Usage { .. }
            | Self::UnsupportedPlatform
            | Self::PlanIo { .. }
            | Self::PlanInsecure { .. }
            | Self::PlanTooLarge { .. }
            | Self::PlanParse { .. }
            | Self::UnsupportedVersion { .. }
            | Self::InvalidPlan { .. }
            | Self::ReportIo { .. }
            | Self::ReportParse { .. }
            | Self::Cgroup { .. }
            | Self::Rlimit { .. } => exit_code::SETUP_FAILED,
        }
    }
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage { message } => write!(
                f,
                "usage error: {message} (usage: harw-job-exec --plan <path> [--report <path>])"
            ),
            Self::UnsupportedPlatform => f.write_str("only supported on Linux"),
            Self::PlanIo { path, source } => write!(f, "plan file {}: {source}", path.display()),
            Self::PlanInsecure { path, reason } => {
                write!(f, "plan file {} rejected: {reason}", path.display())
            }
            Self::PlanTooLarge { limit } => write!(f, "plan exceeds {limit} bytes"),
            Self::PlanParse { message } => write!(f, "plan parse error: {message}"),
            Self::UnsupportedVersion { found } => write!(f, "unsupported plan version {found}"),
            Self::InvalidPlan { reason } => write!(f, "invalid plan: {reason}"),
            Self::ReportIo { path, source } => {
                write!(f, "report file {}: {source}", path.display())
            }
            Self::ReportParse { path, message } => {
                write!(f, "report file {}: {message}", path.display())
            }
            Self::Cgroup { message } => write!(f, "cgroup join failed: {message}"),
            Self::Rlimit { message } => write!(f, "rlimits failed: {message}"),
            Self::Sandbox { message } => write!(f, "sandbox refused: {message}"),
            Self::RequirementNotMet {
                requirement,
                shortfalls,
            } => write!(
                f,
                "sandbox requirement {requirement:?} not met; not enforced: {}",
                shortfalls.join(", ")
            ),
            Self::Exec { program, source } => {
                write!(f, "exec {} failed: {source}", program.display())
            }
        }
    }
}

impl std::error::Error for ExecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PlanIo { source, .. }
            | Self::ReportIo { source, .. }
            | Self::Exec { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_codes_per_class() {
        let usage = ExecError::Usage {
            message: "x".to_owned(),
        };
        assert_eq!(usage.exit_code(), exit_code::SETUP_FAILED);
        assert_eq!(
            ExecError::UnsupportedPlatform.exit_code(),
            exit_code::SETUP_FAILED
        );
        let refused = ExecError::RequirementNotMet {
            requirement: SandboxRequirement::Required,
            shortfalls: vec!["network"],
        };
        assert_eq!(refused.exit_code(), exit_code::SANDBOX_REFUSED);
        assert!(refused.to_string().contains("network"));
        let sandbox = ExecError::Sandbox {
            message: "landlock".to_owned(),
        };
        assert_eq!(sandbox.exit_code(), exit_code::SANDBOX_REFUSED);
        let exec = ExecError::Exec {
            program: PathBuf::from("/nope"),
            source: io::Error::from(io::ErrorKind::NotFound),
        };
        assert_eq!(exec.exit_code(), exit_code::EXEC_FAILED);
        assert!(std::error::Error::source(&exec).is_some());
    }
}
