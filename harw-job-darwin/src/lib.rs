//! `harw-job-darwin` — Darwin/macOS-native job mechanics.
//!
//! This crate is the Darwin counterpart of `harw-job-linux` (Eco-Doc §15,
//! §37, §62). It exposes what Darwin actually offers and reports honestly
//! what it does **not** enforce. It never pretends Darwin has a Linux
//! primitive under another name: there is no pidfd, no cgroup, no Landlock,
//! no `no_new_privs`, no capability sets here.
//!
//! # What exists on which target
//!
//! Pure, platform-independent parts compile on every target so Linux CI tests
//! them:
//! - [`sbpl`]: SBPL (seatbelt profile language) generator for
//!   `sandbox-exec` and the command wrapper [`sbpl::wrap_with_sandbox_exec`].
//! - [`report`]: [`DarwinSandbox`] and its per-dimension
//!   [`harw_job_core::SandboxReport`] mapping.
//! - [`rlimit`]: [`RlimitSet`] (validated rlimit plan) and the mapping from a
//!   [`harw_job_core::ResourceRequest`].
//! - [`recovery`]: [`DarwinRecoveryIdentity`] (persisted metadata) and
//!   [`IdentityCheck`].
//! - [`signal`], [`termination`], [`error`]: closed signal set, termination
//!   policy/report, typed errors.
//!
//! macOS-only (`#[cfg(target_os = "macos")]`, module `macos`):
//! - `DarwinProcess`: spawn in an own process group, signal, wait,
//!   wait-with-timeout, group termination with escalation.
//! - `apply_rlimits_to_current_process` for a future exec trampoline.
//! - `DarwinRecoveryIdentity::check` / `signal` (liveness only; signalling
//!   an unverified recovered PID is refused).
//!
//! On every other target those items do not exist; the crate is then only
//! the pure model above.
//!
//! # Exit watching without kqueue (deviation, documented)
//!
//! The natural Darwin primitive for "wait for exit with a timeout" is
//! `kqueue` with `EVFILT_PROC`/`NOTE_EXIT`. In rustix 1.1 `kevent` is an
//! `unsafe fn` (the caller must keep every fd named in the change list valid),
//! and this workspace sets `unsafe_code = "forbid"` crate-wide, which cannot
//! be overridden locally. Instead each `DarwinProcess` has one watcher thread
//! blocked in `waitid(P_PID, WEXITED | WNOWAIT)`: it observes the exit
//! **without reaping**, then notifies through a channel; `wait_timeout` is a
//! `recv_timeout` on that channel. No busy polling, no `unsafe`, and the PID
//! stays pinned (unreaped) until the owner reaps it.
//!
//! # Authority rule
//! A PID is never sole authority. A live `DarwinProcess` may signal because
//! its child is unreaped (the PID cannot be reused while it is a zombie of
//! ours). A *recovered* PID has no such guarantee, and Darwin start-time
//! lookup needs `sysctl(KERN_PROC)`, which rustix does not expose safely —
//! so recovered processes are [`IdentityCheck::Unverifiable`] and are never
//! signalled.

#![forbid(unsafe_code)]

pub mod error;
pub mod recovery;
pub mod report;
pub mod rlimit;
pub mod sbpl;
pub mod signal;
pub mod termination;

#[cfg(target_os = "macos")]
mod macos;

pub use error::{ProcessError, SandboxError};
pub use recovery::{DarwinRecoveryIdentity, IdentityCheck, UnverifiableReason};
pub use report::{DarwinSandbox, DarwinSandboxMode};
pub use rlimit::{RlimitPlan, RlimitResource, RlimitSet, RlimitValue};
pub use sbpl::{SANDBOX_EXEC_PATH, sbpl_profile_for, wrap_with_sandbox_exec};
pub use signal::SignalKind;
pub use termination::{GroupSweep, TerminationPolicy, TerminationReport};

#[cfg(target_os = "macos")]
pub use macos::{ChildStdio, DarwinProcess, apply_rlimits_to_current_process};

#[cfg(test)]
mod test_support;
