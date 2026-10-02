//! `harw-job-runtime` — the job coordinator, plus the governed
//! background-work primitives it builds on.
//!
//! # Coordinator (Job-Runtime-Doc §14, §15)
//! [`coordinator`] claims jobs from a fenced store (`harw-job-store`),
//! starts attempts through a platform [`coordinator::Executor`]
//! (`LinuxExecutor` on Linux: pidfd process group, optional
//! cgroup v2 boundary, sandbox backend none / Landlock trampoline /
//! Bubblewrap; `DarwinExecutor` on macOS), supervises them through
//! `harw-job-tokio`, renews the lease every TTL/3, enforces deadline and
//! cancellation, finalizes outcomes and recovers attempts after a restart
//! without ever signalling an unverified process.
//!
//! # Host capability probe (PL-90)
//! [`host::HostReport::probe`] reports, without side effects, which
//! sandbox backends this host offers and the strongest per-dimension
//! [`SandboxReport`] a job can get here (runtime admission).
//!
//! # Compatibility re-exports
//! Since Job-Doc Phase 1 the job model (budget, lease, retry, lifecycle,
//! stored record, ids, spec, outcomes) lives in [`harw_job_core`]. This crate
//! re-exports it unchanged so every existing path — `harw_job_runtime::Budget`,
//! `harw_job_runtime::stored::StoredJob`, `harw_job_runtime::WorkId`, … —
//! keeps resolving for the eight existing dependents.
//!
//! # Errors
//! Governance paths return [`JobRuntimeError`] / [`JobRuntimeResult`]; the
//! coordinator returns the typed [`coordinator::RuntimeError`] (§22).
//!
//! # Concurrency
//! The model types are plain `Send + Sync` data. The coordinator runs on
//! Tokio: one task per active attempt, store I/O on the blocking pool.

#![forbid(unsafe_code)]

pub use harw_job_core::*;

// Module paths (`harw_job_runtime::budget::Budget`, ...). The glob above
// already re-exports public modules; these lines make the contract explicit.
pub use harw_job_core::{budget, error, job, lease, retry, stored};

/// Re-export of the shared work identifier so consumers can write
/// `harw_job_runtime::WorkId` (knowledge-surfaces §6.1, §8.1). It is the *same*
/// newtype defined once in `harw-types`, never a per-crate copy.
pub use harw_types::WorkId;

/// Back-compat alias for the `harw_job_runtime::JobError` name used in the
/// knowledge-surfaces §8.1 error sketch; canonical name is [`JobRuntimeError`].
pub use harw_job_core::JobRuntimeError as JobError;

pub mod coordinator;
pub mod host;
pub mod permits;

pub use host::{HostFacts, HostLandlock, HostReport};
pub use permits::{MAX_PERMITS, MIN_PERMITS, Permit, PermitStatus, ResizablePermits};

#[cfg(target_os = "macos")]
pub use coordinator::DarwinExecutor;
pub use coordinator::{
    Coordinator, CoordinatorConfig, CoordinatorStore, Executor, JobHandle, JobResult, RecoveredJob,
    RecoveryDecision, RuntimeError,
};
#[cfg(target_os = "linux")]
pub use coordinator::{LinuxExecutor, LinuxExecutorOptions, LinuxSandboxBackend};

// Test error type (Bible R087/R165/R182), tests only.
#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests {
    use std::any::TypeId;

    /// Every name the eight dependents import must resolve through this
    /// crate to the very same type as in `harw-job-core`.
    #[test]
    fn root_re_exports_are_the_core_types() {
        assert_eq!(
            TypeId::of::<super::Budget>(),
            TypeId::of::<harw_job_core::Budget>()
        );
        assert_eq!(
            TypeId::of::<super::BudgetKind>(),
            TypeId::of::<harw_job_core::BudgetKind>()
        );
        assert_eq!(
            TypeId::of::<super::BudgetUsage>(),
            TypeId::of::<harw_job_core::BudgetUsage>()
        );
        assert_eq!(
            TypeId::of::<super::JobRuntimeError>(),
            TypeId::of::<harw_job_core::JobRuntimeError>()
        );
        assert_eq!(
            TypeId::of::<super::JobError>(),
            TypeId::of::<harw_job_core::JobRuntimeError>()
        );
        assert_eq!(
            TypeId::of::<super::JobRuntimeResult<()>>(),
            TypeId::of::<Result<(), harw_job_core::JobRuntimeError>>()
        );
        assert_eq!(
            TypeId::of::<super::Job>(),
            TypeId::of::<harw_job_core::Job>()
        );
        assert_eq!(
            TypeId::of::<super::JobKind>(),
            TypeId::of::<harw_job_core::JobKind>()
        );
        assert_eq!(
            TypeId::of::<super::JobState>(),
            TypeId::of::<harw_job_core::JobState>()
        );
        assert_eq!(
            TypeId::of::<super::Lease>(),
            TypeId::of::<harw_job_core::Lease>()
        );
        assert_eq!(
            TypeId::of::<super::LeaseToken>(),
            TypeId::of::<harw_job_core::LeaseToken>()
        );
        assert_eq!(
            TypeId::of::<super::RetryPolicy>(),
            TypeId::of::<harw_job_core::RetryPolicy>()
        );
        assert_eq!(
            TypeId::of::<super::JobScope>(),
            TypeId::of::<harw_job_core::JobScope>()
        );
        assert_eq!(
            TypeId::of::<super::JobClaim>(),
            TypeId::of::<harw_job_core::JobClaim>()
        );
        assert_eq!(
            TypeId::of::<super::JobCompletion>(),
            TypeId::of::<harw_job_core::JobCompletion>()
        );
        assert_eq!(
            TypeId::of::<super::JobCancellation>(),
            TypeId::of::<harw_job_core::JobCancellation>()
        );
        assert_eq!(
            TypeId::of::<super::JobOutcome>(),
            TypeId::of::<harw_job_core::JobOutcome>()
        );
        assert_eq!(
            TypeId::of::<super::StoredJob>(),
            TypeId::of::<harw_job_core::StoredJob>()
        );
        assert_eq!(
            TypeId::of::<super::ReclaimOutcome>(),
            TypeId::of::<harw_job_core::ReclaimOutcome>()
        );
        assert_eq!(
            TypeId::of::<super::WorkId>(),
            TypeId::of::<harw_types::WorkId>()
        );
    }

    #[test]
    fn module_paths_still_resolve() {
        assert_eq!(
            TypeId::of::<super::budget::Budget>(),
            TypeId::of::<harw_job_core::budget::Budget>()
        );
        assert_eq!(
            TypeId::of::<super::error::JobRuntimeError>(),
            TypeId::of::<harw_job_core::error::JobRuntimeError>()
        );
        assert_eq!(
            TypeId::of::<super::job::Job>(),
            TypeId::of::<harw_job_core::job::Job>()
        );
        assert_eq!(
            TypeId::of::<super::lease::Lease>(),
            TypeId::of::<harw_job_core::lease::Lease>()
        );
        assert_eq!(
            TypeId::of::<super::retry::RetryPolicy>(),
            TypeId::of::<harw_job_core::retry::RetryPolicy>()
        );
        assert_eq!(
            TypeId::of::<super::stored::StoredJob>(),
            TypeId::of::<harw_job_core::stored::StoredJob>()
        );
    }

    #[test]
    fn re_exported_api_still_behaves() {
        let budget = super::Budget::unbounded();
        let mut usage = super::BudgetUsage::default();
        let result = budget.charge_wall(&mut usage, jiff::SignedDuration::from_millis(-1));
        assert!(matches!(
            result,
            Err(super::JobError::NegativeWallCharge { .. })
        ));
        assert_eq!(usage, super::BudgetUsage::default());
    }
}
