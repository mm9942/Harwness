//! Contextual application errors and explicit conversions from external errors.
//!
//! Owns diagnostic context and preserves source chains; does not perform I/O,
//! spawn threads or acquire locks. Formatting can fail only through the supplied
//! formatter. This binary crate's documentation examples are illustrative and
//! are not executed by Cargo doctests.
//!
//! # Examples
//! ```no_run
//! let error = crate::error::Error::io("read metadata", None,
//!     std::io::Error::from(std::io::ErrorKind::NotFound));
//! assert!(error.is_gone());
//! ```

use std::fmt;
use std::io;
use std::num::ParseIntError;
use std::path::{Path, PathBuf};

/// A failure while selecting, identifying, or terminating a process.
pub enum Error {
    /// An operating-system operation failed.
    Io {
        /// Operation attempted at the failing boundary.
        operation: &'static str,
        /// Filesystem object involved, when applicable.
        path: Option<PathBuf>,
        /// Original operating-system error, preserved for classification.
        source: io::Error,
    },
    /// A numeric field could not be parsed.
    Parse {
        /// Name of the field being parsed.
        field: &'static str,
        /// Original field contents.
        input: String,
        /// Original integer parser error.
        source: ParseIntError,
    },
    /// A supplied value does not satisfy the application's input contract.
    InvalidInput {
        /// Name of the invalid input.
        field: &'static str,
        /// Explanation of the violated constraint.
        reason: String,
    },
    /// A process metadata record is missing or has an invalid structure.
    ProcFormat {
        /// Process whose record was read.
        pid: u32,
        /// Missing or malformed metadata field.
        field: &'static str,
    },
    /// The process no longer matches the identity captured during selection.
    IdentityChanged {
        /// Numeric process identifier whose identity changed.
        pid: u32,
    },
    /// An authorization requirement was not met.
    Permission {
        /// Authorization condition that prevented the operation.
        reason: &'static str,
    },
    /// The privileged helper could not complete its request.
    Helper {
        /// Context describing the helper failure.
        reason: String,
    },
    /// JSON serialization or deserialization failed.
    Json(serde_json::Error),
}

/// Application result retaining the concrete error domain.
pub type Result<T> = std::result::Result<T, Error>;

/// Constructs contextual errors and classifies disappearance without side effects.
impl Error {
    /// Attach operation and optional path context to an operating-system error.
    ///
    /// # Arguments and ownership
    /// `operation` is a borrowed static diagnostic label. `path` is an optional
    /// borrowed path copied into the error. `source` transfers its ownership.
    /// # Returns
    /// An owned `Io` variant; construction itself is infallible and does no I/O.
    /// # Examples
    /// ```no_run
    /// let error = crate::error::Error::io("poll", None,
    ///     std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    /// assert!(!error.is_gone());
    /// ```
    pub fn io(operation: &'static str, path: Option<&Path>, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.map(Path::to_path_buf),
            source,
        }
    }

    /// Whether the operation failed because its target disappeared.
    ///
    /// Linux reports a missing process as `ESRCH` (3); missing `/proc` entries
    /// are reported as `NotFound`. Permission errors deliberately return false.
    /// Borrows `self`; returns a classification without consuming its source.
    /// This is infallible, creates no resources and performs no synchronization.
    /// # Examples
    /// ```no_run
    /// let error = crate::error::Error::IdentityChanged { pid: 42 };
    /// assert!(!error.is_gone());
    /// ```
    pub fn is_gone(&self) -> bool {
        match self {
            Self::Io { source, .. } => {
                source.kind() == io::ErrorKind::NotFound || source.raw_os_error() == Some(3)
            }
            Self::Parse { .. }
            | Self::InvalidInput { .. }
            | Self::ProcFormat { .. }
            | Self::IdentityChanged { .. }
            | Self::Permission { .. }
            | Self::Helper { .. }
            | Self::Json(_) => false,
        }
    }
}

/// Borrows context and writes it to the caller-owned formatter.
impl fmt::Display for Error {
    /// Format a human-readable failure including the captured context.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => {
                if let Some(path) = path.as_ref() {
                    write!(formatter, "{operation} ({path:?}): {source}")
                } else {
                    write!(formatter, "{operation}: {source}")
                }
            }
            Self::Parse {
                field,
                input,
                source,
            } => write!(formatter, "invalid {field} {input:?}: {source}"),
            Self::InvalidInput { field, reason } => {
                write!(formatter, "invalid {field}: {reason}")
            }
            Self::ProcFormat { pid, field } => {
                write!(formatter, "process {pid}: missing or malformed {field}")
            }
            Self::IdentityChanged { pid } => {
                write!(formatter, "process {pid}: identity changed since selection")
            }
            Self::Permission { reason } => write!(formatter, "permission denied: {reason}"),
            Self::Helper { reason } => write!(formatter, "privileged helper failed: {reason}"),
            Self::Json(source) => write!(formatter, "JSON processing failed: {source}"),
        }
    }
}

/// Uses the same diagnostic contract for developer-facing formatting.
impl fmt::Debug for Error {
    /// Keep debug and user-facing error formatting consistent.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

/// Borrows original external causes without manufacturing causes for policy errors.
impl std::error::Error for Error {
    /// Expose the original cause for wrapped external errors.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            Self::Json(source) => Some(source),
            Self::InvalidInput { .. }
            | Self::ProcFormat { .. }
            | Self::IdentityChanged { .. }
            | Self::Permission { .. }
            | Self::Helper { .. } => None,
        }
    }
}

/// Takes ownership of an I/O cause when no richer boundary context is available.
impl From<io::Error> for Error {
    /// Preserve an I/O error where no additional operation context is available.
    fn from(source: io::Error) -> Self {
        Self::io("I/O operation failed", None, source)
    }
}

/// Takes ownership of a JSON cause without discarding its source chain.
impl From<serde_json::Error> for Error {
    /// Preserve a JSON error and its source chain.
    fn from(source: serde_json::Error) -> Self {
        Self::Json(source)
    }
}

#[cfg(test)]
mod tests {
    //! Error classification and diagnostic-context regression tests.

    use super::Error;
    use std::error::Error as _;
    use std::io;
    use std::path::Path;

    /// Disappearance is distinct from permission failure and PID replacement.
    #[test]
    fn test_gone_classification() {
        let missing = Error::from(io::Error::from(io::ErrorKind::NotFound));
        let exited = Error::from(io::Error::from_raw_os_error(3));
        let denied = Error::from(io::Error::from(io::ErrorKind::PermissionDenied));
        assert!(missing.is_gone());
        assert!(exited.is_gone());
        assert!(!denied.is_gone());
        assert!(!Error::IdentityChanged { pid: 42 }.is_gone());
    }

    /// Filesystem diagnostics retain operation, path, and original cause.
    #[test]
    fn test_io_context_and_source_are_preserved() {
        let error = Error::io(
            "read process metadata",
            Some(Path::new("/proc/42/stat")),
            io::Error::from(io::ErrorKind::PermissionDenied),
        );
        let text = error.to_string();
        assert!(text.contains("read process metadata"));
        assert!(text.contains("/proc/42/stat"));
        assert!(error.source().is_some());
        assert_eq!(format!("{error:?}"), text);
    }

    /// Structured policy failures do not invent an underlying source.
    #[test]
    fn test_policy_failure_has_no_source() {
        let error = Error::Permission {
            reason: "foreign owner requires elevation",
        };
        assert!(error.source().is_none());
        assert!(
            error
                .to_string()
                .contains("foreign owner requires elevation")
        );
    }
}
