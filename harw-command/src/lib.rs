//! `harw-command` — the command port (PL-93).
//!
//! Harw application code does not spawn Linux workload processes; it submits
//! jobs. This crate gives surfaces (`shell.exec`, the TUI `!` path, `latex.*`)
//! one small vocabulary — [`CommandRequest`] in, [`CommandOutcome`] out — behind
//! the [`CommandPort`] trait, and [`JobCommandPort`], the adapter that turns a
//! request into a [`JobSpec`](harw_job::JobSpec) and runs it through the job
//! runtime (coordinator, executor, pidfd, cgroup, deadline, kill).
//!
//! # What the adapter decides
//! - **Persistence**: [`Persistence::Ephemeral`] unless the request says
//!   otherwise; the environment never reaches the store.
//! - **One attempt**: a command is never retried behind the caller's back.
//! - **Output**: taken from the live frame stream into one shared byte budget;
//!   exceeding it cancels the job ([`CommandEnd::OutputLimit`]). A lagging
//!   consumer is reported as [`CommandOutcome::lagged`], not silently ignored.
//! - **Working directory**: any existing absolute directory; the runtime's
//!   workspace root is `/` for this port. A workspace sandbox request
//!   ([`CommandSandbox::Workspace`], PL-97 E2b) runs on its own root
//!   instead.

#![forbid(unsafe_code)]

mod global;
mod port;

pub use global::{install, install_host, install_host_default, installed, replace};

pub use harw_job::{
    BackendKind, Persistence, ResourceRequest, SandboxProfileName, SandboxRequirement,
};
pub use harw_job_core::spec::JobWriteScope;
#[cfg(target_os = "linux")]
pub use port::HostCommandExecutor;
pub use port::{
    CommandEnd, CommandFrame, CommandFrames, CommandOutcome, CommandOutput, CommandPort,
    CommandRequest, CommandSandbox, CommandStdin, HostPortOptions, JobCommandPort, OutputStream,
    RunFuture, StartFuture, StartedCommand, WorkspaceSandbox,
};
