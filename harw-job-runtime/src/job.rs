//! The governed unit of work: a [`Job`] keyed by [`WorkId`], carrying a
//! [`Budget`], a [`RetryPolicy`] and lifecycle timestamps (knowledge-surfaces
//! §4.1, §6.2, §6.3). The host claims a job and supplies a lease before it
//! invokes the typed, governed execution closure.

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use harw_types::WorkId;

use crate::budget::{Budget, BudgetUsage};
use crate::error::{JobRuntimeError, JobRuntimeResult};
use crate::lease::Lease;
use crate::retry::RetryPolicy;

/// Discriminates a job's policy profile; dream and worker jobs share one type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Idle-time consolidation job (knowledge-surfaces §4).
    Dream,
    /// A kanban worker-lane job claimed by a worker role (§6.2).
    Worker,
    /// Any other caller-defined kind.
    Custom(String),
}

/// Governance lifecycle state of a [`Job`], mirrored by kanban card states (§6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Created, not yet ready to be claimed.
    Pending,
    /// Eligible to be claimed by a worker.
    Ready,
    /// Claimed and actively running (a [`Lease`] is held).
    Running,
    /// Finished successfully.
    Completed,
    /// Stalled pending some external unblock.
    Blocked,
    /// Terminally failed (retries exhausted).
    Failed,
    /// Terminally cancelled by a trusted supervisor or approval boundary.
    Cancelled,
}

/// A governed unit of background work tracked by the runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    /// Stable governance handle.
    pub id: WorkId,
    /// Policy profile of this job.
    pub kind: JobKind,
    /// Current lifecycle state.
    pub state: JobState,
    /// Ceilings this job runs under.
    pub budget: Budget,
    /// Resources consumed so far.
    pub usage: BudgetUsage,
    /// Retry/backoff schedule.
    pub retry: RetryPolicy,
    /// Number of failed attempts recorded so far.
    pub attempts: u32,
    /// Creation instant.
    pub created_at: Timestamp,
    /// Instant of the most recent state change.
    pub updated_at: Timestamp,
}

impl Job {
    /// Create a fresh `Pending` job at `now`.
    #[must_use]
    pub fn new(
        id: WorkId,
        kind: JobKind,
        budget: Budget,
        retry: RetryPolicy,
        now: Timestamp,
    ) -> Self {
        Self {
            id,
            kind,
            state: JobState::Pending,
            budget,
            usage: BudgetUsage::default(),
            retry,
            attempts: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// Charge tokens against this job's budget.
    pub fn charge_tokens(&mut self, amount: u64) -> JobRuntimeResult<()> {
        self.budget.charge_tokens(&mut self.usage, amount)
    }

    /// Charge one tool call against this job's budget.
    pub fn charge_tool_call(&mut self) -> JobRuntimeResult<()> {
        self.budget.charge_tool_call(&mut self.usage)
    }

    /// Charge elapsed wall-time against this job's budget.
    pub fn charge_wall(&mut self, elapsed: SignedDuration) -> JobRuntimeResult<()> {
        self.budget.charge_wall(&mut self.usage, elapsed)
    }

    /// Make a new or retried job eligible for a worker claim.
    pub fn mark_ready(&mut self, now: Timestamp) -> JobRuntimeResult<()> {
        if !matches!(self.state, JobState::Pending | JobState::Ready) {
            return Err(JobRuntimeError::InvalidState {
                work_id: self.id.clone(),
                expected: JobState::Pending,
                actual: self.state,
            });
        }
        self.state = JobState::Ready;
        self.updated_at = now;
        Ok(())
    }

    /// Claim this job for `holder`, flipping it to `Running` and issuing a [`Lease`].
    pub fn claim(
        &mut self,
        holder: impl Into<String>,
        now: Timestamp,
        ttl: SignedDuration,
    ) -> JobRuntimeResult<Lease> {
        let holder = holder.into();
        if self.state != JobState::Ready {
            return Err(JobRuntimeError::LeaseContended {
                work_id: self.id.clone(),
                holder,
            });
        }
        let lease = Lease::acquire(self.id.clone(), holder, now, ttl)?;
        self.state = JobState::Running;
        self.updated_at = now;
        Ok(lease)
    }

    /// Reject lifecycle transitions that are only legal for an active claim.
    fn require_running(&self) -> JobRuntimeResult<()> {
        if self.state == JobState::Running {
            return Ok(());
        }

        Err(JobRuntimeError::InvalidState {
            work_id: self.id.clone(),
            expected: JobState::Running,
            actual: self.state,
        })
    }

    /// Record a failed attempt; return the backoff, or transition to `Failed` when exhausted.
    pub fn record_failure(&mut self, now: Timestamp) -> JobRuntimeResult<SignedDuration> {
        self.require_running()?;
        self.attempts = self.attempts.saturating_add(1);
        self.updated_at = now;
        match self.retry.next_delay(self.attempts) {
            Ok(delay) => {
                self.state = JobState::Ready;
                Ok(delay)
            }
            Err(err) => {
                self.state = JobState::Failed;
                Err(err)
            }
        }
    }

    /// Mark this job completed at `now`.
    pub fn complete(&mut self, now: Timestamp) -> JobRuntimeResult<()> {
        self.require_running()?;
        self.state = JobState::Completed;
        self.updated_at = now;
        Ok(())
    }

    /// Execute one claimed job under a current lease.
    ///
    /// The closure receives `&mut Job`, so resource charges remain inside this
    /// governance boundary. Success completes the job; failure records the
    /// configured retry transition before the original failure is returned.
    pub fn execute_with<F>(
        &mut self,
        lease: &Lease,
        now: Timestamp,
        operation: F,
    ) -> JobRuntimeResult<()>
    where
        F: FnOnce(&mut Self) -> JobRuntimeResult<()>,
    {
        if self.state != JobState::Running {
            return Err(JobRuntimeError::InvalidState {
                work_id: self.id.clone(),
                expected: JobState::Running,
                actual: self.state,
            });
        }
        if lease.work_id != self.id {
            return Err(JobRuntimeError::LeaseContended {
                work_id: self.id.clone(),
                holder: lease.holder.clone(),
            });
        }
        if lease.is_expired(now) {
            return Err(JobRuntimeError::LeaseExpired {
                work_id: self.id.clone(),
                expired_at: lease.expires_at,
            });
        }

        match operation(self) {
            Ok(()) => {
                self.complete(now)?;
                Ok(())
            }
            Err(error) => {
                self.record_failure(now)?;
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn job() -> Job {
        Job::new(
            WorkId::from_str("work-1"),
            JobKind::Worker,
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: 2,
                base_delay: SignedDuration::from_secs(1),
                factor: 2.0,
                max_delay: SignedDuration::from_secs(10),
            },
            Timestamp::now(),
        )
    }

    #[test]
    fn claimed_job_executes_and_completes() -> TestResult {
        let now = Timestamp::now();
        let mut job = job();
        job.mark_ready(now).map_err(ctx("mark_ready succeeds"))?;
        let lease = job
            .claim("worker-a", now, SignedDuration::from_secs(30))
            .map_err(ctx("claim succeeds"))?;
        job.execute_with(&lease, now, |job| job.charge_tokens(4))
            .map_err(ctx("execute_with succeeds"))?;
        assert_eq!(job.state, JobState::Completed);
        assert_eq!(job.usage.tokens, 4);
        Ok(())
    }

    #[test]
    fn failed_execution_returns_to_ready_for_retry() -> TestResult {
        let now = Timestamp::now();
        let mut job = job();
        job.mark_ready(now).map_err(ctx("mark_ready succeeds"))?;
        let lease = job
            .claim("worker-a", now, SignedDuration::from_secs(30))
            .map_err(ctx("claim succeeds"))?;
        let Err(error) = job.execute_with(&lease, now, |_| {
            Err(JobRuntimeError::RetryExhausted { attempts: 0 })
        }) else {
            return Err(TestError::Unexpected("execute_with must fail".into()));
        };
        assert!(matches!(
            error,
            JobRuntimeError::RetryExhausted { attempts: 0 }
        ));
        assert_eq!(job.state, JobState::Ready);
        assert_eq!(job.attempts, 1);
        Ok(())
    }

    #[test]
    fn completion_requires_a_running_job_without_mutating_it() -> TestResult {
        let mut job = job();
        let previous_updated_at = job.updated_at;

        let Err(error) = job.complete(Timestamp::now()) else {
            return Err(TestError::Unexpected(
                "complete must fail on a pending job".into(),
            ));
        };

        assert!(matches!(
            error,
            JobRuntimeError::InvalidState {
                expected: JobState::Running,
                actual: JobState::Pending,
                ..
            }
        ));
        assert_eq!(job.state, JobState::Pending);
        assert_eq!(job.updated_at, previous_updated_at);
        Ok(())
    }

    #[test]
    fn recording_failure_requires_a_running_job_without_mutating_it() -> TestResult {
        let mut job = job();
        let previous_updated_at = job.updated_at;

        let Err(error) = job.record_failure(Timestamp::now()) else {
            return Err(TestError::Unexpected(
                "record_failure must fail on a pending job".into(),
            ));
        };

        assert!(matches!(
            error,
            JobRuntimeError::InvalidState {
                expected: JobState::Running,
                actual: JobState::Pending,
                ..
            }
        ));
        assert_eq!(job.state, JobState::Pending);
        assert_eq!(job.attempts, 0);
        assert_eq!(job.updated_at, previous_updated_at);
        Ok(())
    }

    #[test]
    fn terminal_transitions_reject_every_non_running_state_without_mutation() -> TestResult {
        let non_running_states = [
            JobState::Pending,
            JobState::Ready,
            JobState::Completed,
            JobState::Blocked,
            JobState::Failed,
            JobState::Cancelled,
        ];

        for state in non_running_states {
            let mut completion = job();
            completion.state = state;
            let completion_updated_at = completion.updated_at;

            let Err(completion_error) = completion.complete(Timestamp::now()) else {
                return Err(TestError::Unexpected(format!(
                    "complete must fail for state {state:?}"
                )));
            };

            assert!(matches!(
                completion_error,
                JobRuntimeError::InvalidState {
                    expected: JobState::Running,
                    actual,
                    ..
                } if actual == state
            ));
            assert_eq!(completion.state, state);
            assert_eq!(completion.attempts, 0);
            assert_eq!(completion.updated_at, completion_updated_at);

            let mut failure = job();
            failure.state = state;
            let failure_updated_at = failure.updated_at;

            let Err(failure_error) = failure.record_failure(Timestamp::now()) else {
                return Err(TestError::Unexpected(format!(
                    "record_failure must fail for state {state:?}"
                )));
            };

            assert!(matches!(
                failure_error,
                JobRuntimeError::InvalidState {
                    expected: JobState::Running,
                    actual,
                    ..
                } if actual == state
            ));
            assert_eq!(failure.state, state);
            assert_eq!(failure.attempts, 0);
            assert_eq!(failure.updated_at, failure_updated_at);
        }
        Ok(())
    }
}
