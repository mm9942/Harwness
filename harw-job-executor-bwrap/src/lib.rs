//! `harw-job-executor-bwrap` — Bubblewrap as an optional, stronger Linux job
//! executor backend (planning README §16, PL-40).
//!
//! ```text
//! generic SandboxPolicy (harw-job-linux)
//!         │
//!         ├── native safe-Rust Linux enforcement (Landlock, harw-job-linux)
//!         │
//!         └── Bubblewrap executor (this crate)
//!               namespaces · mount isolation · no network namespace sharing
//! ```
//!
//! This crate adds **no new mechanism**. It translates a generic
//! [`harw_job_core::JobSpec`] plus a [`harw_job_linux::SandboxPolicy`] into
//! a launch of the existing `harw-sandbox` Bubblewrap backend
//! ([`harw_sandbox::BwrapLauncher`]). `bwrap` stays an external executable,
//! found only at fixed root-owned paths (never via `PATH`); no C enters the
//! Rust dependency graph.
//!
//! # Flow
//! 1. [`BwrapExecutor::discover`] finds a trusted `bwrap`
//!    ([`BwrapExecutorError::Unsupported`] with
//!    [`Dimension::Backend`] when there is none).
//! 2. [`BwrapExecutor::plan`] maps the policy onto the launcher (filesystem
//!    lists → binds, network → [`harw_sandbox::NetworkMode`], capabilities,
//!    `no_new_privs`) and returns a [`BwrapJobPlan`]: the full `bwrap` argv
//!    and a **predicted** [`harw_job_core::SandboxReport`].
//! 3. [`BwrapJobPlan::into_command`] yields a plain
//!    [`std::process::Command`] that `harw_job_linux::LinuxProcess::spawn`
//!    or `harw_job_linux::LinuxJobGroup::spawn` can run (pidfd, process
//!    group, cgroup — the job boundary stays with `harw-job-linux`).
//!
//! # What is refused instead of silently widened
//! - Network [`harw_job_linux::NetworkPolicy::Allow`] and
//!   [`harw_job_linux::NetworkPolicy::ConnectTcpPorts`] need a configured
//!   egress relay ([`BwrapExecutor::with_proxy_relay`]): the Bubblewrap
//!   backend only knows *no network* or *proxy-only egress*. It never shares
//!   the host network namespace.
//! - Read-write paths outside the workspace (other than the sandbox's own
//!   `/tmp` and `/dev`): `harw-sandbox` can only make the workspace writable.
//! - Capability keep-lists: the launcher offers no `--cap-add`.
//!
//! # Future: trampoline inside the sandbox
//! Resource limits (`rlimit`s) and a Landlock layer inside the namespace can
//! later be added by running the `harw-job-exec` trampoline as the sandboxed
//! command (`bwrap … -- harw-job-exec … -- <program>`). Until then
//! `resource_limits` is predicted as `NotEnforced`; cgroup limits come from
//! `harw-job-linux` around the `bwrap` process.
//!
//! On targets other than Linux this crate is empty.

#![forbid(unsafe_code)]

#[cfg(target_os = "linux")]
mod error;
#[cfg(target_os = "linux")]
mod executor;

#[cfg(all(test, target_os = "linux"))]
mod test_support;

#[cfg(target_os = "linux")]
pub use error::{BwrapExecutorError, Dimension};
#[cfg(target_os = "linux")]
pub use executor::{BwrapExecutor, BwrapJobPlan};
