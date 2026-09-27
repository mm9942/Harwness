//! `harw-job-core` — the platform-neutral job model.
//!
//! This crate owns the job model and its state machine (Job-Runtime-Doc §6,
//! Phase 1 in §26). It is testable on every platform: it depends on no async
//! runtime, no `rustix`, no `procfs`, no Landlock, no cgroup library.
//!
//! # Contents
//!
//! Generic model (new, free of Harwness semantics):
//! - [`ids`]: [`AttemptId`], [`RunnerId`], [`FencingToken`],
//!   [`IdempotencyKey`], [`JobScopeId`] — validated newtypes.
//! - [`lifecycle`]: [`LifecycleState`] with an explicit, validated transition
//!   table ([`LifecycleState::transition`], [`LifecycleTransition`]).
//! - [`outcome`]: [`ExitOutcome`], [`CancellationCause`], [`Deadline`].
//! - [`spec`]: [`JobSpec`] (with the [`JobSpec::command`] builder) and the
//!   versioned [`JobSpecEnvelope`] (typed payload, no `serde_json::Value`,
//!   Eco-Doc §59).
//! - [`enforcement`]: [`EnforcementState`] and [`SandboxReport`] — sandbox
//!   enforcement is reported per dimension, never as a single `bool`.
//!
//! Governance primitives (moved unchanged from `harw-job-runtime`, which now
//! re-exports this crate):
//! - [`budget`]: [`Budget`], [`BudgetKind`], [`BudgetUsage`].
//! - [`error`]: [`JobRuntimeError`] / [`JobRuntimeResult`].
//! - [`job`]: [`Job`], [`JobKind`], [`JobState`].
//! - [`lease`]: [`Lease`], [`LeaseToken`].
//! - [`retry`][]: [`RetryPolicy`].
//! - [`stored`]: [`StoredJob`], [`JobScope`], [`JobClaim`],
//!   [`JobCompletion`], [`JobCancellation`], [`JobOutcome`],
//!   [`ReclaimOutcome`].
//!
//! # Known inversions (to be removed)
//!
//! Eco-Doc §14/§57/§58/§59 require that the lowest job layer carries no
//! Harwness application semantics. The moved governance modules still do.
//! Each usage below is a dependency inversion that a later phase moves into a
//! Harwness adapter (the generic replacements already exist in this crate):
//!
//! - `harw_types::WorkId` as job identity (generic target: a job-core id):
//!   `src/error.rs:7`, `src/error.rs:32`, `src/error.rs:38`,
//!   `src/error.rs:47`, `src/job.rs:9`, `src/job.rs:52`, `src/job.rs:75`,
//!   `src/lease.rs:7`, `src/lease.rs:18`, `src/lease.rs:27`,
//!   `src/lease.rs:47`, `src/lease.rs:52`, `src/lease.rs:61`,
//!   `src/stored.rs:250` (test), and the root re-export in `src/lib.rs`.
//! - `harw_macros::HarwError` derive for [`JobRuntimeError`]:
//!   `src/error.rs:6`, `src/error.rs:15`.
//! - `harw_types::{TenantId, WorkspaceId, ApprovalActor}` in [`JobScope`]
//!   and [`JobCancellation`] (generic target: [`JobScopeId`], Eco-Doc §57):
//!   `src/stored.rs:12`, `src/stored.rs:26`-`28`, `src/stored.rs:33`,
//!   `src/stored.rs:42`, `src/stored.rs:47`, `src/stored.rs:52`,
//!   `src/stored.rs:82`.
//! - `harw_observe::TraceContext` in [`StoredJob::trace`] (Eco-Doc §58, the
//!   core must carry no trace dependency): `src/stored.rs:11`,
//!   `src/stored.rs:136`.
//! - `serde_json::Value` payloads in [`JobOutcome::Succeeded`] and
//!   [`StoredJob::input`] (generic target: [`JobSpecEnvelope`], Eco-Doc §59):
//!   `src/stored.rs:94`, `src/stored.rs:109`.
//!
//! The generic modules (`ids`, `lifecycle`, `outcome`, `spec`, `enforcement`)
//! use none of these; they depend only on `serde` and `jiff`.
//!
//! # Errors
//! The governance primitives return [`JobRuntimeError`]. The generic model
//! uses small typed errors per domain: [`IdError`], [`TransitionError`],
//! [`DeadlineError`], [`SpecError`] (Job-Runtime-Doc §22).
//!
//! # Concurrency
//! All types are plain `Send + Sync` data; this crate spawns no threads and
//! holds no locks.

#![forbid(unsafe_code)]

pub mod budget;
pub mod enforcement;
pub mod error;
pub mod ids;
pub mod job;
pub mod lease;
pub mod lifecycle;
pub mod outcome;
pub mod retry;
pub mod spec;
pub mod stored;

pub use budget::{Budget, BudgetKind, BudgetUsage};
pub use enforcement::{EnforcementState, SandboxReport};
pub use error::{JobRuntimeError, JobRuntimeResult};
pub use ids::{AttemptId, FencingToken, IdError, IdempotencyKey, JobScopeId, RunnerId};
pub use job::{Job, JobKind, JobState};
pub use lease::{Lease, LeaseToken};
pub use lifecycle::{
    InvalidTransitionRecord, LifecycleEvent, LifecycleState, LifecycleTransition, TransitionError,
};
pub use outcome::{CancellationCause, Deadline, DeadlineError, ExitOutcome};
pub use retry::RetryPolicy;
pub use spec::{
    DEFAULT_SCOPE, JobSpec, JobSpecBuilder, JobSpecEnvelope, ResourceRequest, SandboxProfileName,
    SandboxRequirement, SpecError, WorkspacePath,
};
pub use stored::{
    JobCancellation, JobClaim, JobCompletion, JobOutcome, JobScope, ReclaimOutcome, StoredJob,
};

/// Re-export of the shared work identifier so consumers can write
/// `harw_job_core::WorkId` (knowledge-surfaces §6.1, §8.1). It is the *same*
/// newtype defined once in `harw-types`, never a per-crate copy.
///
/// Known inversion (see crate docs): the generic core should not name a
/// Harwness identifier.
pub use harw_types::WorkId;

/// Back-compat alias for the `JobError` name used in the knowledge-surfaces
/// §8.1 error sketch; canonical name is [`JobRuntimeError`].
pub use error::JobRuntimeError as JobError;

#[cfg(test)]
mod tests {
    use super::{Budget, BudgetUsage, JobRuntimeError, Lease, RetryPolicy, WorkId};
    use crate::test_support::{TestError, TestResult, ctx};
    use jiff::{SignedDuration, Timestamp};

    #[test]
    fn public_budget_api_rejects_negative_wall_charges_without_accounting_them() -> TestResult {
        let budget = Budget::unbounded();
        let mut usage = BudgetUsage {
            tokens: 7,
            wall: SignedDuration::from_secs(4),
            tool_calls: 1,
        };
        let original = usage.clone();

        let Err(error) = budget.charge_wall(&mut usage, SignedDuration::from_millis(-1)) else {
            return Err(TestError::Unexpected(
                "negative wall charges must fail closed".into(),
            ));
        };

        assert!(matches!(error, JobRuntimeError::NegativeWallCharge { .. }));
        assert_eq!(usage, original);
        Ok(())
    }

    #[test]
    fn public_lease_api_rejects_non_positive_ttl_before_timestamp_arithmetic() -> TestResult {
        let now = Timestamp::MAX;

        for ttl in [SignedDuration::ZERO, SignedDuration::from_secs(-1)] {
            let Err(error) = Lease::acquire(WorkId::from_str("work-1"), "worker-a", now, ttl)
            else {
                return Err(TestError::Unexpected(
                    "non-positive lease TTLs must fail closed".into(),
                ));
            };

            assert!(matches!(
                error,
                JobRuntimeError::LeaseExpired { expired_at, .. } if expired_at == now
            ));
        }
        Ok(())
    }

    #[test]
    fn public_retry_api_rejects_non_finite_and_non_positive_factors() {
        for factor in [f64::NAN, f64::NEG_INFINITY, -1.0, 0.0, f64::INFINITY] {
            let retry = RetryPolicy {
                max_attempts: 4,
                base_delay: SignedDuration::from_secs(1),
                factor,
                max_delay: SignedDuration::from_secs(30),
            };

            assert!(matches!(
                retry.next_delay(0),
                Err(JobRuntimeError::RetryExhausted { attempts: 0 })
            ));
        }
    }

    #[test]
    fn public_retry_api_uses_base_delay_for_the_initial_retry() -> TestResult {
        let retry = RetryPolicy {
            max_attempts: 4,
            base_delay: SignedDuration::from_secs(1),
            factor: 2.0,
            max_delay: SignedDuration::from_secs(30),
        };

        assert_eq!(
            retry
                .next_delay(0)
                .map_err(ctx("initial retry must be schedulable"))?,
            SignedDuration::from_secs(1)
        );
        assert_eq!(
            retry
                .next_delay(1)
                .map_err(ctx("first recorded retry must be schedulable"))?,
            SignedDuration::from_secs(1)
        );
        Ok(())
    }
}

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
