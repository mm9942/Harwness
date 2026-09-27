//! `harw-job-tokio` — the async runtime layer of the job system
//! (Job-Runtime-Doc §3.2, §13, Phase 6), without a single line of `unsafe`.
//!
//! It adapts the synchronous pidfd handles of `harw-job-linux` to Tokio:
//!
//! - [`AsyncLinuxProcess`]: a [`harw_job_linux::LinuxProcess`] (or a whole
//!   [`harw_job_linux::LinuxJobGroup`]) whose exit readiness is awaited
//!   through `tokio::io::unix::AsyncFd` on a duplicate of its pidfd
//!   ([`harw_job_linux::LinuxProcess::readiness_fd`]). Reaping and
//!   signalling still go through the `harw-job-linux` handle; async layered
//!   termination mirrors [`harw_job_linux::LinuxJobGroup::terminate`].
//! - [`Supervisor`]: one task that fans in exit readiness, stdout, stderr,
//!   a deadline and cancellation, and emits [`ProcessEvent`]s over a
//!   bounded channel.
//!
//! # Events, not state
//! The async layer does not own the durable job state machine
//! (Job-Runtime-Doc §13): it only *reports* what happened. The runtime
//! decides transitions.
//!
//! # Ordering guarantee
//! [`ProcessEvent::Exited`] is always the **last** event of a supervisor:
//! it is sent only after both output streams reached EOF — or after the
//! bounded [`SupervisorConfig::drain_timeout`] elapsed, when a surviving
//! descendant keeps a pipe open. `Timeout` and `CancelRequested` are sent
//! at most once in total (the first trigger wins) and always before
//! `Exited`.
//!
//! # Platform
//! Linux only. On other targets this crate compiles to an empty library.

#![forbid(unsafe_code)]

#[cfg(target_os = "linux")]
mod process;
#[cfg(target_os = "linux")]
mod supervisor;

#[cfg(all(test, target_os = "linux"))]
mod test_support;

#[cfg(target_os = "linux")]
pub use process::{AdoptError, AsyncLinuxProcess, SupervisedTarget};
#[cfg(target_os = "linux")]
pub use supervisor::{Canceller, ProcessEvent, Supervisor, SupervisorConfig, SupervisorHandle};
