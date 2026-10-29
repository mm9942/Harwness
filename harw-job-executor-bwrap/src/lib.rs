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
//! - A workspace that is (or resolves to) the host root `/`: every policy
//!   path would be a workspace subpath and the whole host would be bound
//!   read-write.
//!
//! # Composition with the trampoline (PL-97 E1)
//! [`BwrapExecutor::plan_with_trampoline`] runs the `harw-job-exec`
//! trampoline as the sandboxed command: the binary is bound read-only at
//! [`SANDBOX_TRAMPOLINE_PATH`], a per-attempt directory read-write at
//! [`SANDBOX_ATTEMPT_DIR`] (for the inner trampoline's report; no inner
//! policy path grants write access there, so Landlock keeps the job out
//! and the caller needs a usable Landlock ABI), and the plan keeps the caller's cgroup
//! namespace (explicit `--unshare-*` list instead of `--unshare-all`). The
//! returned [`BwrapTrampolinePlan`] carries the namespace view
//! ([`harw_job_linux::PathMap`]) and the inner policy planned against it,
//! so Landlock inside the namespace separates read from execute — the
//! distinction the mounts cannot make. The caller (the job runtime's
//! `BwrapTrampoline` backend) writes the inner plan, starts `bwrap` through
//! an outer trampoline that joins the job cgroup first, and merges the
//! predicted report of this crate with the inner trampoline's report
//! (`harw_job_exec::merge_reports`).
//!
//! [`BwrapExecutor::plan`] alone predicts `resource_limits` as
//! `NotEnforced`; cgroup limits and rlimits come from the layers around and
//! inside the `bwrap` process.
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
pub use executor::{
    BwrapExecutor, BwrapJobPlan, BwrapTrampolinePlan, INNER_PLAN_FILE, INNER_REPORT_FILE,
    SANDBOX_ATTEMPT_DIR, SANDBOX_TRAMPOLINE_DIR, SANDBOX_TRAMPOLINE_PATH, TrampolineMount,
};
