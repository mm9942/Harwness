//! The error type shared by every reader in this crate.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Why a Harw-DoD artefact could not be read.
///
/// Readers never follow symlinks and never read unbounded input; the
/// variants name which of those guards refused the file.
#[derive(Debug)]
pub enum DodClientError {
    /// The file does not exist (DoD not installed, export not enabled yet).
    NotFound {
        /// The path that was looked up.
        path: PathBuf,
    },
    /// The final path component is a symlink; readers refuse to follow it.
    Symlink {
        /// The refused path.
        path: PathBuf,
    },
    /// The path exists but is not a regular file.
    NotRegularFile {
        /// The refused path.
        path: PathBuf,
    },
    /// The file exceeds the reader's size limit.
    TooLarge {
        /// The refused path.
        path: PathBuf,
        /// The limit in bytes.
        limit: u64,
    },
    /// The file content does not match the expected schema.
    Parse {
        /// The file that failed to parse.
        path: PathBuf,
        /// Sanitized, bounded parser message.
        reason: String,
    },
    /// Any other I/O failure (permission denied, EIO, ...).
    Io {
        /// The path being read.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// `systemctl` could not be started for a reason other than absence.
    Systemctl {
        /// Sanitized description.
        reason: String,
    },
}

impl DodClientError {
    /// `true` for [`Self::NotFound`].
    #[must_use]
    pub const fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
    }

    pub(crate) fn io(path: &Path, source: io::Error) -> Self {
        if source.kind() == io::ErrorKind::NotFound {
            Self::NotFound {
                path: path.to_path_buf(),
            }
        } else {
            Self::Io {
                path: path.to_path_buf(),
                source,
            }
        }
    }
}

impl fmt::Display for DodClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { path } => write!(f, "`{}` does not exist", path.display()),
            Self::Symlink { path } => {
                write!(
                    f,
                    "`{}` is a symlink; refusing to follow it",
                    path.display()
                )
            }
            Self::NotRegularFile { path } => {
                write!(f, "`{}` is not a regular file", path.display())
            }
            Self::TooLarge { path, limit } => write!(
                f,
                "`{}` exceeds the read limit of {limit} bytes",
                path.display()
            ),
            Self::Parse { path, reason } => {
                write!(f, "`{}` could not be parsed: {reason}", path.display())
            }
            Self::Io { path, source } => {
                write!(f, "failed to read `{}`: {source}", path.display())
            }
            Self::Systemctl { reason } => write!(f, "systemctl query failed: {reason}"),
        }
    }
}

impl std::error::Error for DodClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
