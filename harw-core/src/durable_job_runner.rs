//! Durable job execution orchestration.
//!
//! [`JobStore`] owns the durable fence and [`JobExecutionRegistry`] owns the
//! in-process cancellation handle.  This module is the deliberately small
//! seam between them: a claim is registered under the *complete* lease token,
//! the operation runs, and the token is removed regardless of completion
//! outcome.  No worker name or session identifier is ever used as a key.

use crate::execution_registry::{ExecutionControl, ExecutionRegistryError, JobExecutionRegistry};
use harw_job_runtime::{JobClaim, JobCompletion, JobOutcome, LeaseToken};
use harw_session_store::{ClaimRequest, CompleteRequest, JobStore, SessionStoreError};
use harw_types::WorkId;
use jiff::Timestamp;
use std::fmt;
use std::future::Future;
use std::sync::Arc;

/// Errors emitted by the durable runner boundary.
#[derive(Debug)]
pub enum DurableJobRunnerError {
    Store(SessionStoreError),
    Registry(ExecutionRegistryError),
}

impl fmt::Display for DurableJobRunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(f, "durable job store error: {error}"),
            Self::Registry(error) => write!(f, "live execution registry error: {error}"),
        }
    }
}

impl std::error::Error for DurableJobRunnerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Registry(error) => Some(error),
        }
    }
}

impl From<SessionStoreError> for DurableJobRunnerError {
    fn from(error: SessionStoreError) -> Self {
        Self::Store(error)
    }
}

impl From<ExecutionRegistryError> for DurableJobRunnerError {
    fn from(error: ExecutionRegistryError) -> Self {
        Self::Registry(error)
    }
}

/// A composable owner of durable job execution.
pub struct DurableJobRunner {
    store: Arc<JobStore>,
    executions: Arc<JobExecutionRegistry>,
}

impl DurableJobRunner {
    #[must_use]
    pub fn new(store: Arc<JobStore>, executions: Arc<JobExecutionRegistry>) -> Self {
        Self { store, executions }
    }

    #[must_use]
    pub fn store(&self) -> &Arc<JobStore> {
        &self.store
    }

    #[must_use]
    pub fn executions(&self) -> &Arc<JobExecutionRegistry> {
        &self.executions
    }

    /// Claim, register, run, complete, and unregister one job.
    ///
    /// The operation receives the immutable [`JobClaim`] produced by the
    /// store and returns its exact [`ExecutionControl`] together with a
    /// future for the governed result.  Completion is attempted before the
    /// in-memory binding is removed; the binding is nevertheless unregistered
    /// if durable completion rejects a stale token (for example after an
    /// operator cancellation or lease reconciliation).
    pub async fn run<F, Fut>(
        &self,
        work_id: &WorkId,
        request: &ClaimRequest,
        operation: F,
    ) -> Result<JobCompletion, DurableJobRunnerError>
    where
        F: FnOnce(JobClaim) -> (Arc<dyn ExecutionControl>, Fut),
        Fut: Future<Output = JobOutcome> + Send,
    {
        let claim = self.store.claim(work_id, request)?;
        let token: LeaseToken = claim.token.clone();
        let (control, operation) = operation(claim);

        if let Err(error) = self.executions.register(token.clone(), control) {
            // A registration failure means this claim cannot safely run.  It
            // is terminally failed under the same fence rather than left as a
            // durable zombie in Running state.
            let _ = self.store.complete(
                work_id,
                &CompleteRequest {
                    token: token.clone(),
                    completed_at: Timestamp::now(),
                    outcome: JobOutcome::Failed {
                        reason: "execution registration rejected".to_owned(),
                    },
                },
            );
            return Err(error.into());
        }

        let outcome = operation.await;
        let completion_result = self.store.complete(
            work_id,
            &CompleteRequest {
                token: token.clone(),
                completed_at: Timestamp::now(),
                outcome,
            },
        );
        let unregister_result = self.executions.unregister(&token);

        // Always surface the durable result first: a stale completion is the
        // security-relevant outcome.  Registry cleanup errors are returned
        // only when the durable transition itself succeeded.
        match (completion_result, unregister_result) {
            (Err(error), _) => Err(error.into()),
            (Ok(_completion), Err(error)) => Err(error.into()),
            (Ok(completion), Ok(_)) => Ok(completion),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_registry::ExecutionControl;
    use harw_job_runtime::{Budget, Job, JobKind, RetryPolicy, StoredJob};
    use harw_types::{ApprovalActor, TenantId, WorkspaceId};
    use jiff::SignedDuration;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{oneshot, watch};

    struct FakeControl {
        completion: watch::Sender<bool>,
        receiver: watch::Receiver<bool>,
        graceful: AtomicUsize,
        forced: AtomicUsize,
    }

    impl FakeControl {
        fn new() -> Arc<Self> {
            let (completion, receiver) = watch::channel(false);
            Arc::new(Self {
                completion,
                receiver,
                graceful: AtomicUsize::new(0),
                forced: AtomicUsize::new(0),
            })
        }
    }

    impl ExecutionControl for FakeControl {
        fn request_graceful_cancel(&self) {
            self.graceful.fetch_add(1, Ordering::SeqCst);
            self.completion.send_replace(true);
        }

        fn force_abort(&self) {
            self.forced.fetch_add(1, Ordering::SeqCst);
            self.completion.send_replace(true);
        }

        fn completion(&self) -> watch::Receiver<bool> {
            self.receiver.clone()
        }
    }

    fn record(id: &str) -> StoredJob {
        let now = Timestamp::now();
        let mut job = Job::new(
            WorkId::from_str(id),
            JobKind::Worker,
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: 2,
                base_delay: SignedDuration::from_secs(1),
                factor: 2.0,
                max_delay: SignedDuration::from_secs(10),
            },
            now,
        );
        job.mark_ready(now).expect("new job is pending");
        StoredJob {
            job,
            scope: harw_job_runtime::JobScope::new(
                TenantId::from_str("tenant"),
                WorkspaceId::from_str("workspace"),
                ApprovalActor::Operator {
                    id: "operator".to_owned(),
                },
            ),
            input: serde_json::json!({"task": "test"}),
            submitted_at: now,
            not_before: now,
            lease: None,
            lease_epoch: 0,
            completion: None,
            cancellation: None,
            revision: 0,
            trace: None,
        }
    }

    fn runner(id: &str) -> (tempfile::TempDir, Arc<JobStore>, DurableJobRunner, WorkId) {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(JobStore::new(temp.path()));
        let work_id = WorkId::from_str(id);
        store.admit(&record(id)).expect("admit record");
        let executions = Arc::new(JobExecutionRegistry::new());
        let runner = DurableJobRunner::new(store.clone(), executions);
        (temp, store, runner, work_id)
    }

    fn request(now: Timestamp) -> ClaimRequest {
        ClaimRequest {
            worker_id: "runner".to_owned(),
            lease_ttl: SignedDuration::from_secs(60),
            now,
        }
    }

    #[tokio::test]
    async fn happy_completion_unregisters_exact_execution() {
        let (_temp, store, runner, work_id) = runner("work-happy");
        let control = FakeControl::new();
        let expected = control.clone();
        let completion = runner
            .run(&work_id, &request(Timestamp::now()), move |_claim| {
                let control_for_operation = control.clone();
                (control, async move {
                    control_for_operation.completion.send_replace(true);
                    JobOutcome::Succeeded {
                        result: serde_json::json!({"ok": true}),
                    }
                })
            })
            .await
            .expect("run succeeds");
        assert!(matches!(completion.outcome, JobOutcome::Succeeded { .. }));
        assert_eq!(expected.graceful.load(Ordering::SeqCst), 0);
        assert!(runner.executions().is_empty());
        assert_eq!(
            store.get(&work_id).expect("record").job.state,
            harw_job_runtime::JobState::Completed
        );
    }

    #[tokio::test]
    async fn failed_outcome_still_unregisters_execution() {
        let (_temp, store, runner, work_id) = runner("work-failed");
        let control = FakeControl::new();
        let completion = runner
            .run(&work_id, &request(Timestamp::now()), move |_claim| {
                (control, async {
                    JobOutcome::Failed {
                        reason: "operation failed".to_owned(),
                    }
                })
            })
            .await
            .expect("durable failure is a completion");
        assert!(matches!(completion.outcome, JobOutcome::Failed { .. }));
        assert!(runner.executions().is_empty());
        assert_eq!(
            store.get(&work_id).expect("record").job.state,
            harw_job_runtime::JobState::Failed
        );
    }

    #[tokio::test]
    async fn durable_cancellation_fences_runner_before_completion() {
        let (_temp, store, _unused_runner, work_id) = runner("work-cancel");
        let executions = Arc::new(JobExecutionRegistry::new());
        let runner = DurableJobRunner::new(store.clone(), executions.clone());
        let control = FakeControl::new();
        let operation_control = control.clone();
        let (claimed, claimed_rx) = oneshot::channel();
        let run_work_id = work_id.clone();
        let run = tokio::spawn(async move {
            runner
                .run(&run_work_id, &request(Timestamp::now()), move |claim| {
                    let _ = claimed.send(claim.token.clone());
                    let mut done = operation_control.completion();
                    (operation_control, async move {
                        while !*done.borrow() {
                            if done.changed().await.is_err() {
                                break;
                            }
                        }
                        JobOutcome::Succeeded {
                            result: serde_json::json!({"late": true}),
                        }
                    })
                })
                .await
        });
        let token = claimed_rx.await.expect("claim reached operation");
        let transition = store
            .cancel(
                &work_id,
                &harw_session_store::CancelRequest {
                    cancelled_at: Timestamp::now(),
                    cancelled_by: ApprovalActor::Operator {
                        id: "operator".to_owned(),
                    },
                    reason: "operator cancellation".to_owned(),
                },
            )
            .expect("durable cancellation");
        assert_eq!(
            transition.prior_lease.as_ref().map(|lease| lease.token()),
            Some(token.clone())
        );
        assert_eq!(
            executions
                .cancel(&token, std::time::Duration::from_secs(1))
                .await
                .expect("live cancellation"),
            crate::execution_registry::CancellationResult::Graceful
        );
        assert_eq!(control.graceful.load(Ordering::SeqCst), 1);
        let result = run.await.expect("runner task");
        assert!(matches!(
            result,
            Err(DurableJobRunnerError::Store(
                SessionStoreError::JobAlreadyTerminal {
                    state: harw_job_runtime::JobState::Cancelled,
                    ..
                }
            ))
        ));
        assert!(executions.is_empty());
        assert!(matches!(
            store.get(&work_id).expect("record").job.state,
            harw_job_runtime::JobState::Cancelled
        ));
    }
}
