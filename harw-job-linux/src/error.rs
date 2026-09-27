//! Typed error domains of the Linux layer (Job-Runtime-Doc §22).
//!
//! Raw OS errors are mapped here, at the Linux boundary, so higher layers can
//! distinguish *unsupported feature*, *permission denied*, *resource
//! exhausted*, *identity mismatch*, *process already exited*, *cgroup
//! unavailable* and *sandbox partially enforced* without parsing strings.
//! No third-party type (rustix, procfs, landlock, cap-std) appears in any
//! variant; OS errors are carried as the raw `errno` number or as
//! [`std::io::Error`].

use std::fmt;
use std::io;
use std::path::PathBuf;

use harw_job_core::EnforcementState;

/// Coarse classification of a raw `errno`, shared by all error domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrnoClass {
    /// `ENOSYS`, `EOPNOTSUPP`, `EINVAL` on feature probes.
    Unsupported,
    /// `EPERM`, `EACCES`.
    PermissionDenied,
    /// `EMFILE`, `ENFILE`, `ENOMEM`, `EAGAIN`, `ENOSPC`.
    Exhausted,
    /// `ESRCH`.
    NoSuchProcess,
    /// `ECHILD`.
    NotAChild,
    /// Anything else.
    Other,
}

/// Classifies a raw OS error number.
pub(crate) fn classify_errno(errno: i32) -> ErrnoClass {
    // Values from the Linux UAPI (asm-generic/errno-base.h, errno.h); they
    // are identical on every Linux architecture this crate targets.
    const EPERM: i32 = 1;
    const ESRCH: i32 = 3;
    const ECHILD: i32 = 10;
    const EAGAIN: i32 = 11;
    const ENOMEM: i32 = 12;
    const EACCES: i32 = 13;
    const ENFILE: i32 = 23;
    const EMFILE: i32 = 24;
    const ENOSPC: i32 = 28;
    const ENOSYS: i32 = 38;
    const EOPNOTSUPP: i32 = 95;
    match errno {
        EPERM | EACCES => ErrnoClass::PermissionDenied,
        ESRCH => ErrnoClass::NoSuchProcess,
        ECHILD => ErrnoClass::NotAChild,
        EAGAIN | ENOMEM | ENFILE | EMFILE | ENOSPC => ErrnoClass::Exhausted,
        ENOSYS | EOPNOTSUPP => ErrnoClass::Unsupported,
        _ => ErrnoClass::Other,
    }
}

// ── ProcessError ─────────────────────────────────────────────────────────────

/// Failure of a live-process operation (spawn, pidfd, signal, wait, rlimit).
#[derive(Debug)]
#[non_exhaustive]
pub enum ProcessError {
    /// The kernel lacks the primitive (e.g. `pidfd_open` before Linux 5.3,
    /// `waitid(P_PIDFD)` before 5.4).
    Unsupported {
        /// Operation that was attempted.
        operation: &'static str,
    },
    /// `EPERM`/`EACCES`.
    PermissionDenied {
        /// Operation that was attempted.
        operation: &'static str,
    },
    /// File descriptors, memory or process slots are exhausted.
    Exhausted {
        /// Operation that was attempted.
        operation: &'static str,
    },
    /// The process has already exited (and possibly been reaped).
    AlreadyExited {
        /// Diagnostic PID.
        pid: u32,
    },
    /// `waitid` on a process that is not a child of this process.
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
    /// `Command::spawn` failed.
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
            Self::Spawn(source) => write!(f, "spawn failed: {source}"),
            Self::Os { operation, errno } => write!(f, "{operation}: os error {errno}"),
        }
    }
}

impl std::error::Error for ProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(source) => Some(source),
            _ => None,
        }
    }
}

// ── CgroupError ──────────────────────────────────────────────────────────────

/// Why cgroup v2 cannot be used at all.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CgroupUnavailableReason {
    /// The delegated root does not exist.
    RootMissing,
    /// The delegated root is not on a `cgroup2` filesystem (cgroup v1 or a
    /// plain directory).
    NotCgroup2,
    /// The delegated root exists but is not writable by this process
    /// (no delegation).
    NotDelegated,
}

/// Failure of a cgroup v2 operation.
#[derive(Debug)]
#[non_exhaustive]
pub enum CgroupError {
    /// cgroup v2 is not usable here (typed reason).
    CgroupUnavailable {
        /// Delegated root that was probed.
        root: PathBuf,
        /// Why it is not usable.
        reason: CgroupUnavailableReason,
    },
    /// A kernel interface file is missing (e.g. `cgroup.kill` before 5.14
    /// and `cgroup.freeze` before 5.2).
    Unsupported {
        /// The missing interface.
        feature: &'static str,
    },
    /// `EPERM`/`EACCES` on a cgroup file or directory.
    PermissionDenied {
        /// Path relative to the delegated root.
        path: String,
    },
    /// The kernel refused because of exhausted resources.
    Exhausted {
        /// Path relative to the delegated root.
        path: String,
    },
    /// The cgroup still has member processes (`EBUSY` on removal).
    Busy {
        /// Path relative to the delegated root.
        path: String,
    },
    /// A cgroup with this name already exists.
    AlreadyExists {
        /// Path relative to the delegated root.
        path: String,
    },
    /// The cgroup does not exist (any more).
    NotFound {
        /// Path relative to the delegated root.
        path: String,
    },
    /// The cgroup name is not a single safe path component.
    InvalidName {
        /// The rejected name.
        name: String,
    },
    /// A resource limit is outside its accepted domain.
    InvalidLimit {
        /// Limit field (e.g. `cpu_weight`).
        field: &'static str,
        /// What was wrong.
        reason: String,
    },
    /// A cgroup file had unexpected content.
    Parse {
        /// File relative to the job cgroup.
        file: &'static str,
        /// The raw content (trimmed, truncated).
        content: String,
    },
    /// Any other I/O error.
    Io {
        /// Operation that was attempted.
        operation: &'static str,
        /// Path relative to the delegated root.
        path: String,
        /// Underlying error.
        source: io::Error,
    },
}

impl CgroupError {
    /// Maps an I/O error of `operation` on `path` to its typed class.
    #[cfg_attr(not(feature = "linux-cgroup-v2"), allow(dead_code))]
    pub(crate) fn from_io(operation: &'static str, path: &str, source: io::Error) -> Self {
        let path = path.to_owned();
        if source.kind() == io::ErrorKind::NotFound {
            return Self::NotFound { path };
        }
        const EBUSY: i32 = 16;
        match source.raw_os_error() {
            Some(EBUSY) => Self::Busy { path },
            Some(errno) => match classify_errno(errno) {
                ErrnoClass::PermissionDenied => Self::PermissionDenied { path },
                ErrnoClass::Exhausted => Self::Exhausted { path },
                _ => Self::Io {
                    operation,
                    path,
                    source,
                },
            },
            None => Self::Io {
                operation,
                path,
                source,
            },
        }
    }
}

impl fmt::Display for CgroupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CgroupUnavailable { root, reason } => {
                write!(f, "cgroup v2 unavailable at {}: {reason:?}", root.display())
            }
            Self::Unsupported { feature } => {
                write!(f, "cgroup interface {feature} not supported by the kernel")
            }
            Self::PermissionDenied { path } => write!(f, "cgroup {path}: permission denied"),
            Self::Exhausted { path } => write!(f, "cgroup {path}: resource exhausted"),
            Self::Busy { path } => write!(f, "cgroup {path} still has member processes"),
            Self::AlreadyExists { path } => write!(f, "cgroup {path} already exists"),
            Self::NotFound { path } => write!(f, "cgroup {path} not found"),
            Self::InvalidName { name } => write!(f, "invalid cgroup name {name:?}"),
            Self::InvalidLimit { field, reason } => write!(f, "invalid limit {field}: {reason}"),
            Self::Parse { file, content } => {
                write!(f, "unexpected content in {file}: {content:?}")
            }
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "{operation} {path}: {source}"),
        }
    }
}

impl std::error::Error for CgroupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

// ── LaunchError ──────────────────────────────────────────────────────────────

/// Failure to start a job inside its boundary (spawn + pidfd + cgroup).
#[derive(Debug)]
#[non_exhaustive]
pub enum LaunchError {
    /// Spawning or taking pidfd authority failed.
    Process(ProcessError),
    /// Attaching to the job cgroup failed; the child was killed and reaped.
    Cgroup(CgroupError),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Process(source) => write!(f, "launch: {source}"),
            Self::Cgroup(source) => write!(f, "launch: {source}"),
        }
    }
}

impl std::error::Error for LaunchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Process(source) => Some(source),
            Self::Cgroup(source) => Some(source),
        }
    }
}

impl From<ProcessError> for LaunchError {
    fn from(source: ProcessError) -> Self {
        Self::Process(source)
    }
}

impl From<CgroupError> for LaunchError {
    fn from(source: CgroupError) -> Self {
        Self::Cgroup(source)
    }
}

// ── SandboxError ─────────────────────────────────────────────────────────────

/// Sandbox dimension that an error or report entry refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SandboxComponent {
    /// Landlock filesystem rules.
    Filesystem,
    /// Landlock network rules.
    Network,
    /// `PR_SET_NO_NEW_PRIVS`.
    NoNewPrivs,
    /// Capability sets.
    Capabilities,
}

/// Failure to apply a sandbox policy.
#[derive(Debug)]
#[non_exhaustive]
pub enum SandboxError {
    /// A hard requirement cannot be met on this kernel at all.
    Unsupported {
        /// Which dimension.
        component: SandboxComponent,
    },
    /// A hard requirement was only partially met. The process may already be
    /// partially restricted; the caller must not `exec` the job.
    PartiallyEnforced {
        /// Which dimension fell short.
        component: SandboxComponent,
        /// What was actually achieved.
        state: EnforcementState,
    },
    /// The kernel refused a restriction because of missing privilege.
    PermissionDenied {
        /// Which dimension.
        component: SandboxComponent,
    },
    /// A capability name in [`crate::sandbox::CapabilityPolicy::Keep`] is
    /// unknown.
    UnknownCapability {
        /// The rejected name.
        name: String,
    },
    /// A policy path could not be opened for a Landlock rule.
    InvalidPath {
        /// The path.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
    /// Landlock ruleset construction or `restrict_self` failed.
    Landlock {
        /// Rendered library error (the library type is not exposed).
        message: String,
    },
    /// Any other OS error.
    Os {
        /// Operation that was attempted.
        operation: &'static str,
        /// Raw `errno`.
        errno: i32,
    },
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { component } => {
                write!(
                    f,
                    "sandbox {component:?}: not supported by the running kernel"
                )
            }
            Self::PartiallyEnforced { component, state } => {
                write!(f, "sandbox {component:?}: hard requirement only {state:?}")
            }
            Self::PermissionDenied { component } => {
                write!(f, "sandbox {component:?}: permission denied")
            }
            Self::UnknownCapability { name } => write!(f, "unknown capability {name:?}"),
            Self::InvalidPath { path, source } => {
                write!(f, "sandbox path {}: {source}", path.display())
            }
            Self::Landlock { message } => write!(f, "landlock: {message}"),
            Self::Os { operation, errno } => write!(f, "{operation}: os error {errno}"),
        }
    }
}

impl std::error::Error for SandboxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidPath { source, .. } => Some(source),
            _ => None,
        }
    }
}

// ── RecoveryError ────────────────────────────────────────────────────────────

/// Why a recovered process is not the persisted one (Job-Runtime-Doc §15).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MismatchReason {
    /// The persisted identity has no start time; a PID alone is never proof.
    StartTimeUnknown,
    /// The PID exists but was started at a different time (PID reuse).
    StartTimeDiffers {
        /// Persisted start time (clock ticks since boot).
        expected: u64,
        /// Observed start time.
        actual: u64,
    },
    /// The executable differs (both sides known).
    ExecutableDiffers {
        /// Persisted executable.
        expected: String,
        /// Observed executable.
        actual: String,
    },
    /// The process lives in a different cgroup (both sides known).
    CgroupDiffers {
        /// Persisted cgroup path.
        expected: String,
        /// Observed cgroup path.
        actual: String,
    },
}

impl fmt::Display for MismatchReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StartTimeUnknown => write!(f, "no persisted start time (pid alone is no proof)"),
            Self::StartTimeDiffers { expected, actual } => {
                write!(f, "start time {actual} != persisted {expected}")
            }
            Self::ExecutableDiffers { expected, actual } => {
                write!(f, "executable {actual:?} != persisted {expected:?}")
            }
            Self::CgroupDiffers { expected, actual } => {
                write!(f, "cgroup {actual:?} != persisted {expected:?}")
            }
        }
    }
}

/// Failure while inspecting or acting on a recovered process.
#[derive(Debug)]
#[non_exhaustive]
pub enum RecoveryError {
    /// The PID now belongs to a different process; nothing was signalled.
    IdentityMismatch {
        /// Diagnostic PID.
        pid: u32,
        /// Why the identity does not match.
        reason: MismatchReason,
    },
    /// The persisted process has already exited.
    AlreadyExited {
        /// Diagnostic PID.
        pid: u32,
    },
    /// `/proc/<pid>` exists but is not readable by this process.
    PermissionDenied {
        /// Diagnostic PID.
        pid: u32,
    },
    /// `/proc/<pid>` had unexpected content or another read error occurred.
    Unreadable {
        /// Diagnostic PID.
        pid: u32,
        /// Rendered procfs error (the library type is not exposed).
        message: String,
    },
    /// Opening or signalling through the pidfd failed.
    Process(ProcessError),
    /// A cgroup operation failed.
    Cgroup(CgroupError),
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdentityMismatch { pid, reason } => {
                write!(f, "process {pid}: identity mismatch: {reason}")
            }
            Self::AlreadyExited { pid } => write!(f, "process {pid} has already exited"),
            Self::PermissionDenied { pid } => write!(f, "/proc/{pid}: permission denied"),
            Self::Unreadable { pid, message } => write!(f, "/proc/{pid}: {message}"),
            Self::Process(source) => write!(f, "{source}"),
            Self::Cgroup(source) => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for RecoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Process(source) => Some(source),
            Self::Cgroup(source) => Some(source),
            _ => None,
        }
    }
}

impl From<ProcessError> for RecoveryError {
    fn from(source: ProcessError) -> Self {
        Self::Process(source)
    }
}

impl From<CgroupError> for RecoveryError {
    fn from(source: CgroupError) -> Self {
        Self::Cgroup(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_errno_maps_distinguishable_classes() {
        assert_eq!(classify_errno(1), ErrnoClass::PermissionDenied);
        assert_eq!(classify_errno(13), ErrnoClass::PermissionDenied);
        assert_eq!(classify_errno(3), ErrnoClass::NoSuchProcess);
        assert_eq!(classify_errno(10), ErrnoClass::NotAChild);
        assert_eq!(classify_errno(24), ErrnoClass::Exhausted);
        assert_eq!(classify_errno(38), ErrnoClass::Unsupported);
        assert_eq!(classify_errno(5), ErrnoClass::Other);
    }

    #[test]
    fn test_process_error_from_errno_esrch_is_already_exited() {
        assert!(matches!(
            ProcessError::from_errno("pidfd_send_signal", 42, 3),
            ProcessError::AlreadyExited { pid: 42 }
        ));
        assert!(matches!(
            ProcessError::from_errno("pidfd_open", 42, 38),
            ProcessError::Unsupported {
                operation: "pidfd_open"
            }
        ));
    }

    #[test]
    fn test_cgroup_error_from_io_busy_and_not_found() {
        let busy = CgroupError::from_io("rmdir", "job-1", io::Error::from_raw_os_error(16));
        assert!(matches!(busy, CgroupError::Busy { .. }));
        let gone = CgroupError::from_io("open", "job-1", io::Error::from_raw_os_error(2));
        assert!(matches!(gone, CgroupError::NotFound { .. }));
        let denied = CgroupError::from_io("write", "job-1", io::Error::from_raw_os_error(13));
        assert!(matches!(denied, CgroupError::PermissionDenied { .. }));
    }
}
