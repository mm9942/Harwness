//! Errors of the Kubernetes executor.

use std::fmt;

use harw_job_runtime::RuntimeError;

/// Why an API interaction was refused or failed.
#[derive(Debug)]
pub enum K8sError {
    /// The configuration is invalid.
    Config(String),
    /// Transport failure (connect, TLS, timeout).
    Transport(String),
    /// The API answered something this client refuses.
    Protocol(String),
    /// 404.
    NotFound,
    /// 403/401: the credential may not do this.
    Forbidden(String),
    /// The job needs something this backend does not provide.
    Unsupported(String),
}

impl fmt::Display for K8sError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(d) => write!(f, "invalid Kubernetes executor configuration: {d}"),
            Self::Transport(d) => write!(f, "Kubernetes API transport failed: {d}"),
            Self::Protocol(d) => write!(f, "Kubernetes API protocol error: {d}"),
            Self::NotFound => f.write_str("Kubernetes object not found"),
            Self::Forbidden(d) => write!(f, "Kubernetes API refused the request: {d}"),
            Self::Unsupported(d) => write!(f, "unsupported by the Kubernetes backend: {d}"),
        }
    }
}

impl std::error::Error for K8sError {}

impl From<K8sError> for RuntimeError {
    fn from(error: K8sError) -> Self {
        match error {
            K8sError::Forbidden(detail) => Self::PermissionDenied {
                operation: "kubernetes api",
                detail,
            },
            K8sError::Unsupported(detail) | K8sError::Config(detail) => Self::Unsupported {
                operation: "kubernetes executor",
                detail,
            },
            other => Self::Os {
                operation: "kubernetes api",
                detail: other.to_string(),
            },
        }
    }
}
