//! Typed error domains of the Darwin layer (Job-Runtime-Doc §22).
//!
//! Raw OS errors are classified here so higher layers can distinguish
//! *permission denied*, *resource exhausted*, *process already exited*,
//! *identity unverifiable* and *sandbox path not representable* without
//! parsing strings. No third-party type (rustix) appears in any variant; OS
//! errors are carried as the raw `errno` or as [`std::io::Error`].

use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::recovery::UnverifiableReason;

/// Coarse classification of a raw Darwin `errno`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum ErrnoClass {
    /// `ENOSYS`, `ENOTSUP`, `EOPNOTSUPP`.
    Unsupported,
    /// `EPERM`, `EACCES`.
    PermissionDenied,
    /// `EAGAIN`, `ENOMEM`, `ENFILE`, `EMFILE`, `ENOSPC`.
    Exhausted,
    /// `ESRCH`.
    NoSuchProcess,
    /// `ECHILD`.
    NotAChild,
    /// Anything else.
    Other,
}

/// Classifies a raw Darwin `errno`.
///
/// The values are those of XNU's `<sys/errno.h>`; several differ from Linux
/// (`EAGAIN` is 35, `ENOSYS` 78, `EOPNOTSUPP` 102 on Darwin), which is why
/// this table is not shared with `harw-job-linux`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn classify_errno(errno: i32) -> ErrnoClass {
    const EPERM: i32 = 1;
    const ESRCH: i32 = 3;
    const ECHILD: i32 = 10;
    const ENOMEM: i32 = 12;
    const EACCES: i32 = 13;
    const ENFILE: i32 = 23;
    const EMFILE: i32 = 24;
    const ENOSPC: i32 = 28;
    const EAGAIN: i32 = 35;
    const ENOTSUP: i32 = 45;
    const ENOSYS: i32 = 78;
    const EOPNOTSUPP: i32 = 102;
    match errno {
        EPERM | EACCES => ErrnoClass::PermissionDenied,
        ESRCH => ErrnoClass::NoSuchProcess,
        ECHILD => ErrnoClass::NotAChild,
        EAGAIN | ENOMEM | ENFILE | EMFILE | ENOSPC => ErrnoClass::Exhausted,
        ENOSYS | ENOTSUP | EOPNOTSUPP => ErrnoClass::Unsupported,
        _ => ErrnoClass::Other,
    }
}

// ── ProcessError ─────────────────────────────────────────────────────────────

/// Failure of a live-process operation (spawn, signal, wait, rlimit,
/// recovery).
#[derive(Debug)]
#[non_exhaustive]
pub enum ProcessError {
    /// The running kernel lacks the primitive.
    Unsupported {
        /// Operation that was attempted.
        operation: &'static str,
    },
    /// `EPERM`/`EACCES`.
    PermissionDenied {
        /// Operation that was attempted.
        operation: &'static str,
    },
    /// File descriptors, memory, threads or process slots are exhausted.
    Exhausted {
        /// Operation that was attempted.
        operation: &'static str,
    },
    /// The process has already exited (and possibly been reaped); nothing was
    /// signalled.
    AlreadyExited {
        /// Diagnostic PID.
        pid: u32,
    },
    /// A wait on a process that is not (or no longer) a child of this process.
    NotAChild {
        /// Diagnostic PID.
        pid: u32,
    },
    /// A PID of `0` or above `i32::MAX` was passed.
    InvalidPid {
        /// The rejected value.
        pid: u32,
    },
    /// An argument outside the accepted domain (e.g. soft rlimit above hard).
    InvalidArgument {
        /// What was wrong.
        reason: String,
    },
    /// A recovered PID could not be tied to the persisted process; it was
    /// **not** signalled (a PID is never sole authority).
    IdentityUnverifiable {
        /// Diagnostic PID.
        pid: u32,
        /// Why the identity cannot be verified.
        reason: UnverifiableReason,
    },
    /// The exit watcher thread ended without reporting (should not happen;
    /// the handle can no longer wait with a timeout).
    WatcherLost {
        /// Diagnostic PID.
        pid: u32,
    },
    /// An I/O error without a raw `errno`.
    Io {
        /// Operation that was attempted.
        operation: &'static str,
        /// Underlying error.
        source: io::Error,
    },
    /// `Command::spawn` (or spawning the exit watcher) failed.
    Spawn(io::Error),
    /// Any other OS error.
    Os {
        /// Operation that was attempted.
        operation: &'static str,
        /// Raw `errno`.
        errno: i32,
    },
}

impl ProcessError {
    /// Maps a raw `errno` of `operation` on process `pid`.
    // Only the macOS backend (and tests) map raw errnos.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn from_errno(operation: &'static str, pid: u32, errno: i32) -> Self {
        match classify_errno(errno) {
            ErrnoClass::Unsupported => Self::Unsupported { operation },
            ErrnoClass::PermissionDenied => Self::PermissionDenied { operation },
            ErrnoClass::Exhausted => Self::Exhausted { operation },
            ErrnoClass::NoSuchProcess => Self::AlreadyExited { pid },
            ErrnoClass::NotAChild => Self::NotAChild { pid },
            ErrnoClass::Other => Self::Os { operation, errno },
        }
    }

    /// Maps an [`io::Error`] of `operation` on process `pid`.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn from_io(operation: &'static str, pid: u32, source: io::Error) -> Self {
        match source.raw_os_error() {
            Some(errno) => Self::from_errno(operation, pid, errno),
            None => Self::Io { operation, source },
        }
    }
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { operation } => {
                write!(f, "{operation}: not supported by the running kernel")
            }
            Self::PermissionDenied { operation } => write!(f, "{operation}: permission denied"),
            Self::Exhausted { operation } => write!(f, "{operation}: resource exhausted"),
            Self::AlreadyExited { pid } => write!(f, "process {pid} has already exited"),
            Self::NotAChild { pid } => write!(f, "process {pid} is not a child of this process"),
            Self::InvalidPid { pid } => write!(f, "invalid pid {pid}"),
            Self::InvalidArgument { reason } => write!(f, "invalid argument: {reason}"),
            Self::IdentityUnverifiable { pid, reason } => write!(
                f,
                "process {pid}: identity unverifiable ({reason}); refusing to signal"
            ),
            Self::WatcherLost { pid } => {
                write!(f, "process {pid}: exit watcher ended unexpectedly")
            }
            Self::Io { operation, source } => write!(f, "{operation}: {source}"),
            Self::Spawn(source) => write!(f, "spawn failed: {source}"),
            Self::Os { operation, errno } => write!(f, "{operation}: os error {errno}"),
        }
    }
}

impl std::error::Error for ProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(source) | Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

// ── SandboxError ─────────────────────────────────────────────────────────────

/// Failure to build a `sandbox-exec` invocation.
#[derive(Debug)]
#[non_exhaustive]
pub enum SandboxError {
    /// The workspace root is not an absolute path (SBPL `subpath` needs one).
    RelativeWorkspaceRoot {
        /// The rejected path.
        path: PathBuf,
    },
    /// The path cannot be written into an SBPL string literal (not UTF-8, or
    /// contains control characters).
    UnrepresentablePath {
        /// The rejected path.
        path: PathBuf,
        /// What was wrong.
        reason: &'static str,
    },
    /// The program name cannot be passed to `sandbox-exec` safely.
    InvalidProgram {
        /// The rejected program.
        program: PathBuf,
        /// What was wrong.
        reason: &'static str,
    },
    /// The workspace root could not be canonicalized (seatbelt matches
    /// resolved paths, e.g. `/private/tmp` rather than `/tmp`).
    Canonicalize {
        /// The path.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RelativeWorkspaceRoot { path } => {
                write!(f, "workspace root {} is not absolute", path.display())
            }
            Self::UnrepresentablePath { path, reason } => {
                write!(
                    f,
                    "path {} not representable in SBPL: {reason}",
                    path.display()
                )
            }
            Self::InvalidProgram { program, reason } => {
                write!(f, "program {}: {reason}", program.display())
            }
            Self::Canonicalize { path, source } => {
                write!(f, "canonicalize {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for SandboxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Canonicalize { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ErrnoClass, ProcessError, SandboxError, classify_errno};
    use crate::recovery::UnverifiableReason;
    use std::path::PathBuf;

    #[test]
    fn test_classify_errno_uses_darwin_numbers() {
        assert_eq!(classify_errno(1), ErrnoClass::PermissionDenied);
        assert_eq!(classify_errno(13), ErrnoClass::PermissionDenied);
        assert_eq!(classify_errno(3), ErrnoClass::NoSuchProcess);
        assert_eq!(classify_errno(10), ErrnoClass::NotAChild);
        assert_eq!(classify_errno(35), ErrnoClass::Exhausted, "Darwin EAGAIN");
        assert_eq!(classify_errno(24), ErrnoClass::Exhausted);
        assert_eq!(classify_errno(78), ErrnoClass::Unsupported, "Darwin ENOSYS");
        assert_eq!(classify_errno(102), ErrnoClass::Unsupported);
        // Linux ENOSYS (38) is Darwin ENOTSOCK: not an "unsupported" signal here.
        assert_eq!(classify_errno(38), ErrnoClass::Other);
    }

    #[test]
    fn test_process_error_from_errno_maps_classes() {
        assert!(matches!(
            ProcessError::from_errno("kill", 42, 3),
            ProcessError::AlreadyExited { pid: 42 }
        ));
        assert!(matches!(
            ProcessError::from_errno("waitid", 42, 10),
            ProcessError::NotAChild { pid: 42 }
        ));
        assert!(matches!(
            ProcessError::from_errno("kill", 42, 1),
            ProcessError::PermissionDenied { operation: "kill" }
        ));
        assert!(matches!(
            ProcessError::from_errno("setrlimit", 0, 22),
            ProcessError::Os {
                operation: "setrlimit",
                errno: 22
            }
        ));
    }

    #[test]
    fn test_error_display_names_the_refusal() {
        let error = ProcessError::IdentityUnverifiable {
            pid: 7,
            reason: UnverifiableReason::StartTimeUnknown,
        };
        assert!(error.to_string().contains("refusing to signal"), "{error}");
        let error = SandboxError::RelativeWorkspaceRoot {
            path: PathBuf::from("ws"),
        };
        assert!(error.to_string().contains("not absolute"), "{error}");
    }
}
