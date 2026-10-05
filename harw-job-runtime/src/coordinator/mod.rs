//! The job coordinator (Job-Runtime-Doc §14, §15, §22).
//!
//! ```text
//!                 ┌──────────────────────┐
//!                 │ CoordinatorStore     │  harw-job-store (fenced records
//!                 │ (JobRecordStore +    │  + attempt sidecars)
//!                 │  attempt sidecars)   │
//!                 └──────────┬───────────┘
//!                claim / renew / complete
//!                            ▼
//!                 ┌──────────────────────┐
//!                 │     Coordinator      │  lifecycle, lease heartbeat,
//!                 └───┬──────────────┬───┘  deadline, cancel, recovery
//!                     │              │
//!                     ▼              ▼
//!             Executor::start   AttemptEvent stream
//!         (LinuxExecutor: pidfd group + cgroup + sandbox backend,
//!          supervised by harw-job-tokio; DarwinExecutor on macOS)
//! ```
//!
//! - [`Executor`]: platform-neutral contract (start, probe, reattach,
//!   recorded exit). `LinuxExecutor` (Linux) and `DarwinExecutor` (macOS)
//!   implement it.
//! - [`Coordinator`]: `submit` → `Queued` record → claim with the runner's
//!   [`harw_job_core::RunnerId`] and a lease TTL → `Starting` → spawn →
//!   `Running` → heartbeat every TTL/3 → deadline / cancel / lease loss →
//!   finalize (`Succeeded`, `Failed`, `TimedOut`, `Cancelled`, `Lost`).
//!   [`Coordinator::recover`] resumes after a restart.
//! - [`AttemptRecord`]: the per-attempt sidecar with lifecycle, recovery
//!   identity and enforcement report.
//! - [`RuntimeError`]: typed failures (§22).
//! - [`OutputCapture`]: bounded head + tail capture of stdout/stderr.

pub mod attempt;
pub mod capture;
#[cfg(target_os = "macos")]
pub mod darwin;
pub mod error;
pub mod executor;
pub mod frames;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod runner;
pub mod store;

#[cfg(all(test, target_os = "linux"))]
mod tests;

pub use attempt::{ATTEMPT_RECORD_VERSION, ATTEMPTS_SIDECAR, AttemptRecord, attempt_id_for};
pub use capture::{DEFAULT_OUTPUT_HEAD_BYTES, DEFAULT_OUTPUT_TAIL_BYTES, OutputCapture};
#[cfg(target_os = "macos")]
pub use darwin::DarwinExecutor;
pub use error::RuntimeError;
pub use executor::{
    AttemptContext, AttemptControl, AttemptEvent, AttemptEventSender, AttemptEvents, AttemptRun,
    Executor, HandedStdio, OutputFiles, Probe, StartedAttempt, StdioHandoff, check_requirement,
    requests_resource_limits, unsandboxed_report,
};
pub use frames::{DEFAULT_FRAME_BUFFER, FrameEvent, JobFrame, JobFrames};
#[cfg(target_os = "linux")]
pub use linux::{LinuxExecutor, LinuxExecutorOptions, LinuxSandboxBackend};
pub use runner::{
    Coordinator, CoordinatorConfig, DEFAULT_LEASE_TTL, JobHandle, JobResult, MIN_LEASE_TTL,
    Persistence, RecoveredJob, RecoveryDecision, SubmitOptions,
};
pub use store::CoordinatorStore;
