//! `harw-job-runtime` — governed background-work primitives.
//!
//! This crate owns the substrate every governed unit of work runs on: a [`Job`]
//! keyed by [`WorkId`], a [`Budget`] (token / wall-time / tool-call ceilings), a
//! [`Lease`] (acquire / renew / expiry), and an exponential [`RetryPolicy`]. It
//! is the dependency consumed by `harw-knowledge`'s Dream feature and by kanban
//! worker lanes, per knowledge-surfaces §4.1 and §6.2.
//!
//! # Scope
//! Pure governance arithmetic (budget charging, lease expiry, backoff delay) is
//! implemented fully. Anything requiring a running async scheduler/executor is
//! deferred via [`JobRuntimeError::NotYetImplemented`].
//!
//! # Errors
//! Every fallible path returns [`JobRuntimeError`] / [`JobRuntimeResult`].
//!
//! # Concurrency
//! The types are plain `Send + Sync` data; this crate spawns no threads and
//! holds no locks. Scheduling across threads/processes is the caller's concern.

#![forbid(unsafe_code)]

pub mod budget;
pub mod error;
pub mod job;
pub mod lease;
pub mod retry;
pub mod stored;

pub use budget::{Budget, BudgetKind, BudgetUsage};
pub use error::{JobRuntimeError, JobRuntimeResult};
pub use job::{Job, JobKind, JobState};
pub use lease::{Lease, LeaseToken};
pub use retry::RetryPolicy;
pub use stored::{
    JobCancellation, JobClaim, JobCompletion, JobOutcome, JobScope, ReclaimOutcome, StoredJob,
};

/// Re-export of the shared work identifier so consumers can write
/// `harw_job_runtime::WorkId` (knowledge-surfaces §6.1, §8.1). It is the *same*
/// newtype defined once in `harw-types`, never a per-crate copy.
pub use harw_types::WorkId;

/// Back-compat alias for the `harw_job_runtime::JobError` name used in the
/// knowledge-surfaces §8.1 error sketch; canonical name is [`JobRuntimeError`].
pub use error::JobRuntimeError as JobError;

#[cfg(test)]
mod tests {
    use super::{Budget, BudgetUsage, JobRuntimeError, Lease, RetryPolicy, WorkId};
    use jiff::{SignedDuration, Timestamp};

    #[test]
    fn public_budget_api_rejects_negative_wall_charges_without_accounting_them() {
        let budget = Budget::unbounded();
        let mut usage = BudgetUsage {
            tokens: 7,
            wall: SignedDuration::from_secs(4),
            tool_calls: 1,
        };
        let original = usage.clone();

        let error = budget
            .charge_wall(&mut usage, SignedDuration::from_millis(-1))
            .expect_err("negative wall charges must fail closed");

        assert!(matches!(error, JobRuntimeError::NegativeWallCharge { .. }));
        assert_eq!(usage, original);
    }

    #[test]
    fn public_lease_api_rejects_non_positive_ttl_before_timestamp_arithmetic() {
        let now = Timestamp::MAX;

        for ttl in [SignedDuration::ZERO, SignedDuration::from_secs(-1)] {
            let error = Lease::acquire(WorkId::from_str("work-1"), "worker-a", now, ttl)
                .expect_err("non-positive lease TTLs must fail closed");

            assert!(matches!(
                error,
                JobRuntimeError::LeaseExpired { expired_at, .. } if expired_at == now
            ));
        }
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
    fn public_retry_api_uses_base_delay_for_the_initial_retry() {
        let retry = RetryPolicy {
            max_attempts: 4,
            base_delay: SignedDuration::from_secs(1),
            factor: 2.0,
            max_delay: SignedDuration::from_secs(30),
        };

        assert_eq!(
            retry
                .next_delay(0)
                .expect("initial retry must be schedulable"),
            SignedDuration::from_secs(1)
        );
        assert_eq!(
            retry
                .next_delay(1)
                .expect("first recorded retry must be schedulable"),
            SignedDuration::from_secs(1)
        );
    }
}
