//! `harw-job-linux` — the Linux mechanics of the job runtime
//! (Job-Runtime-Doc §8-§12, §15, Phases 2-5), without a single line of
//! `unsafe`.
//!
//! # Contents
//! - [`process`]: [`LinuxProcess`] — pidfd authority over one process
//!   (`pidfd_open`, `pidfd_send_signal`, `waitid(P_PIDFD)`, `poll`).
//! - [`group`]: [`LinuxJobGroup`] — primary + process group (+ job cgroup)
//!   with layered [`TerminationPolicy`] (graceful → grace → hard kill).
//! - [`resources`]: [`JobResources`] (cgroup limits) and [`RlimitSet`].
//! - [`capabilities`]: capability drop via rustix (no second cap crate).
//! - [`sandbox`]: [`SandboxPolicy`] model, profiles, Landlock planning;
//!   enforcement with feature `linux-sandbox`; side-effect-free host
//!   probes ([`LandlockSupport`], user namespaces).
//! - `proc` *(feature `linux-basic`)*: `/proc` observation via `procfs`.
//! - `recovery` *(feature `linux-basic`)*: `LinuxRecoveryIdentity` —
//!   verified control of processes after a restart; a PID alone never
//!   authorises a signal.
//! - `cgroup` *(feature `linux-cgroup-v2`)*: project-owned `CgroupBackend`,
//!   the in-house cgroupfs backend `CgroupV2Fs` and the read-only
//!   `cgroup::detect` probe.
//! - `filesystem` *(features `linux-sandbox` / `linux-cgroup-v2`)*:
//!   `CapDir` — directory capabilities via `cap-std`.
//! - [`error`]: typed error domains (§22).
//!
//! # Platform
//! Everything is Linux-only. On other targets this crate compiles to an
//! empty library, so the workspace still builds on macOS/Windows CI.
//!
//! # Public API boundary
//! No rustix, procfs, landlock or cap-std type appears in the public API
//! (§2.5). PIDs are plain `u32` diagnostics; descriptors stay private.
//!
//! # Features
//! - `linux-basic` (default): `procfs` for `proc` and `recovery`.
//! - `linux-sandbox`: Landlock enforcement and `filesystem`.
//! - `linux-cgroup-v2`: cgroup v2 backend.
//!
//! # Concurrency
//! All handles are `Send`. Nothing here holds locks, and the only thread
//! ever spawned is the short-lived, joined Landlock probe thread of
//! `sandbox::landlock_support`;
//! blocking waits are explicit (`wait`, `wait_timeout`, `terminate`).
//! Sandbox and capability operations act on the **calling thread** only.

#![forbid(unsafe_code)]

#[cfg(target_os = "linux")]
pub mod capabilities;
#[cfg(all(target_os = "linux", feature = "linux-cgroup-v2"))]
pub mod cgroup;
#[cfg(target_os = "linux")]
pub mod error;
#[cfg(all(
    target_os = "linux",
    any(feature = "linux-sandbox", feature = "linux-cgroup-v2")
))]
pub mod filesystem;
#[cfg(target_os = "linux")]
pub mod group;
#[cfg(all(target_os = "linux", feature = "linux-basic"))]
pub mod proc;
#[cfg(target_os = "linux")]
pub mod process;
#[cfg(all(target_os = "linux", feature = "linux-basic"))]
pub mod recovery;
#[cfg(target_os = "linux")]
pub mod resources;
#[cfg(target_os = "linux")]
pub mod sandbox;

#[cfg(all(test, target_os = "linux"))]
mod test_support;

#[cfg(target_os = "linux")]
pub use error::{
    CgroupError, CgroupUnavailableReason, LaunchError, MismatchReason, ProcessError, RecoveryError,
    SandboxComponent, SandboxError,
};
#[cfg(target_os = "linux")]
pub use group::{LinuxJobGroup, TerminationPolicy, TerminationReport};
#[cfg(target_os = "linux")]
pub use process::{ChildStdio, LinuxProcess, SignalKind};
#[cfg(all(target_os = "linux", feature = "linux-basic"))]
pub use recovery::{IdentityCheck, LinuxRecoveryIdentity};
#[cfg(target_os = "linux")]
pub use resources::{
    CgroupController, CpuQuota, IoDeviceLimit, IoLimits, JobResources, RlimitResource, RlimitSet,
    RlimitValue,
};
#[cfg(target_os = "linux")]
pub use sandbox::{
    CapabilityPolicy, FilesystemPolicy, LandlockMode, LandlockSupport, NetworkPolicy, SandboxPolicy,
};

/// Job-Runtime-Doc §8 names the resource type `LinuxResources`.
#[cfg(target_os = "linux")]
pub type LinuxResources = JobResources;
/// Job-Runtime-Doc §8 names the sandbox type `LinuxSandbox`.
#[cfg(target_os = "linux")]
pub type LinuxSandbox = SandboxPolicy;
