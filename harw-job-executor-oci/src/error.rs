//! Errors of the OCI executor.

use std::fmt;

use harw_job_runtime::RuntimeError;

/// Why an engine interaction was refused or failed.
#[derive(Debug)]
pub enum OciError {
    /// The configuration is invalid (e.g. a tag instead of a digest).
    Config(String),
    /// The engine socket is not trusted (wrong owner).
    UntrustedSocket(String),
    /// Connecting, reading or writing the socket failed.
    Io(String),
    /// The engine answered something this client refuses (size, framing,
    /// status, shape).
    Protocol(String),
    /// The engine reported 404 for the object.
    NotFound,
    /// The image is not present locally (pulling is not done here).
    ImageMissing(String),
    /// The job needs something this backend does not provide.
    Unsupported(String),
}

impl fmt::Display for OciError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(d) => write!(f, "invalid OCI executor configuration: {d}"),
            Self::UntrustedSocket(d) => write!(f, "engine socket not trusted: {d}"),
            Self::Io(d) => write!(f, "engine I/O failed: {d}"),
            Self::Protocol(d) => write!(f, "engine protocol error: {d}"),
            Self::NotFound => f.write_str("engine object not found"),
            Self::ImageMissing(d) => write!(f, "image not available locally: {d}"),
            Self::Unsupported(d) => write!(f, "unsupported by the OCI backend: {d}"),
        }
    }
}

impl std::error::Error for OciError {}

impl From<OciError> for RuntimeError {
    fn from(error: OciError) -> Self {
        match error {
            OciError::UntrustedSocket(detail) => Self::PermissionDenied {
                operation: "oci engine socket",
                detail,
            },
            OciError::Unsupported(detail) | OciError::Config(detail) => Self::Unsupported {
                operation: "oci executor",
                detail,
            },
            other => Self::Os {
                operation: "oci engine",
                detail: other.to_string(),
            },
        }
    }
}
