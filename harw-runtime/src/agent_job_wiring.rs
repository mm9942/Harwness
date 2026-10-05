//! Durable agent-job adapter.
//!
//! Agent delegation is a logical job, not a UI concern. This adapter admits
//! every detached agent run into the shared durable job ledger and drives it
//! through `DurableJobRunner` so WorkId, fencing, heartbeat and cancellation
//! are identical to other governed work.

#![forbid(unsafe_code)]

use std::sync::Arc;
use harw_core::{
    BackgroundStatus, DurableJobRunner, ExecutionControl, JobExecutionRegistry,
    ManagedAgentSpawner, StateStore, TurnInput, TurnOutcome,
};
use harw_extension_api::{
    AgentJobFuture, AgentJobHandle, AgentJobSubmitter, AgentSpawnError,
};
use harw_job_core::{
    Budget, Job, JobKind, JobOutcome, JobScope, RetryPolicy, StoredJob,
};
use harw_session_store::{ApprovalStore, ClaimRequest, JobStore};
use harw_types::{ApprovalActor, TenantId, WorkId, WorkspaceId};
use jiff::{SignedDuration, Timestamp};
use tokio::sync::watch;

const AGENT_JOB_KIND: &str = "agent";
const AGENT_JOB_WORKER: &str = "harw-agent-runtime";
const AGENT_JOB_LEASE_SECS: i64 = 30;

struct AgentExecutionControl {
    spawner: Arc<ManagedAgentSpawner>,
    child: harw_types::SessionId,
    completion_rx: watch::Receiver<bool>,
}

impl ExecutionControl for AgentExecutionControl {
    fn request_graceful_cancel(&self) {
        let _ = self.spawner.request_cancellation(&self.child);
    }

    fn force_abort(&self) {
        // ManagedAgentSpawner owns the child session. Its cancellation token
        // is the only safe abort primitive; bypassing it would strand manager
        // state or violate the child lease.
        let _ = self.spawner.request_cancellation(&self.child);
    }

    fn completion(&self) -> watch::Receiver<bool> {
        self.completion_rx.clone()
    }
}

/// Runtime-wide durable submitter for admitted child agents.
pub struct RuntimeAgentJobSubmitter {
    spawner: Arc<ManagedAgentSpawner>,
    state_store: Arc<dyn StateStore>,
    approval_store: Option<Arc<ApprovalStore>>,
    job_store: Arc<JobStore>,
    runner: Arc<DurableJobRunner>,
    scope: JobScope,
}

impl RuntimeAgentJobSubmitter {
    #[must_use]
    pub fn new(
        spawner: Arc<ManagedAgentSpawner>,
        state_store: Arc<dyn StateStore>,
        approval_store: Option<Arc<ApprovalStore>>,
        job_store: Arc<JobStore>,
        actor: ApprovalActor,
    ) -> Self {
        let executions = Arc::new(JobExecutionRegistry::new());
        let runner = Arc::new(DurableJobRunner::new(
            Arc::clone(&job_store),
            executions,
        ));
        Self {
            spawner,
            state_store,
            approval_store,
            job_store,
            runner,
            scope: JobScope::new(
                TenantId::from_str("local"),
                WorkspaceId::from_str("agents"),
                actor,
            ),
        }
    }

    fn rejection(detail: impl Into<String>) -> AgentSpawnError {
        AgentSpawnError {
            message: detail.into(),
        }
    }
}

impl AgentJobSubmitter for RuntimeAgentJobSubmitter {
    fn submit_child<'a>(
        &'a self,
        child: &'a harw_types::SessionId,
        task: Option<&'a str>,
    ) -> AgentJobFuture<'a> {
        Box::pin(async move {
            let child = child.clone();
            let task = task
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("Continue the delegated task.")
                .to_owned();

            // Detach before the parent continues so parent completion cannot
            // release the child admission while its durable job is running.
            self.spawner
                .detach_for_background(&child, Some(&task))
                .map_err(|error| Self::rejection(error.message))?;

            let work_id = WorkId::new();
            let now = Timestamp::now();
            let retry = RetryPolicy::try_new(
                1,
                SignedDuration::ZERO,
                1.0,
                SignedDuration::ZERO,
            )
            .map_err(|error| Self::rejection(error.to_string()))?;
            let mut job = Job::new(
                work_id.clone(),
                JobKind::Custom(AGENT_JOB_KIND.to_owned()),
                Budget::unbounded(),
                retry,
                now,
            );
            job.mark_ready(now)
                .map_err(|error| Self::rejection(error.to_string()))?;
            let record = StoredJob {
                job,
                scope: self.scope.clone(),
                input: serde_json::json!({
                    "kind": AGENT_JOB_KIND,
                    "child_id": child.as_str(),
                    "task": task,
                }),
                submitted_at: now,
                not_before: now,
                lease: None,
                lease_epoch: 0,
                completion: None,
                cancellation: None,
                revision: 0,
                trace: None,
            };

            let store = Arc::clone(&self.job_store);
            let record_for_admit = record.clone();
            tokio::task::spawn_blocking(move || store.admit(&record_for_admit))
                .await
                .map_err(|error| Self::rejection(format!("agent job admission task failed: {error}")))?
                .map_err(|error| Self::rejection(format!("agent job admission failed: {error}")))?;

            let spawner = Arc::clone(&self.spawner);
            let state_store = Arc::clone(&self.state_store);
            let approvals = self.approval_store.clone();
            let runner = Arc::clone(&self.runner);
            let work_id_for_run = work_id.clone();
            let child_for_run = child.clone();
            let task_for_run = task.clone();

            tokio::spawn(async move {
                let request = ClaimRequest {
                    worker_id: AGENT_JOB_WORKER.to_owned(),
                    lease_ttl: SignedDuration::from_secs(AGENT_JOB_LEASE_SECS),
                    now: Timestamp::now(),
                };
                let result = runner
                    .run_with_cancel(
                        &work_id_for_run,
                        &request,
                        move |_claim, _job_cancel| {
                            let (completion_tx, completion_rx) = watch::channel(false);
                            let control: Arc<dyn ExecutionControl> =
                                Arc::new(AgentExecutionControl {
                                    spawner: Arc::clone(&spawner),
                                    child: child_for_run.clone(),
                                    completion_rx,
                                });
                            let future = async move {
                                let run = spawner
                                    .run_child_with_declared_budget(
                                        &child_for_run,
                                        state_store.as_ref(),
                                        approvals.as_deref(),
                                        TurnInput::user(task_for_run),
                                    )
                                    .await;

                                let outcome = match run {
                                    Ok(result) => match result.outcome {
                                        TurnOutcome::Completed => {
                                            let text = result
                                                .full_text
                                                .unwrap_or_else(|| "completed".to_owned());
                                            let _ = spawner.finish_background_child(
                                                &child_for_run,
                                                BackgroundStatus::Completed,
                                                text.clone(),
                                            );
                                            JobOutcome::Succeeded {
                                                result: serde_json::json!({
                                                    "child_id": child_for_run.as_str(),
                                                    "text": text,
                                                    "budget_exhausted": result.budget_exhausted,
                                                }),
                                            }
                                        }
                                        TurnOutcome::Cancelled { reason } => {
                                            let text = format!("agent cancelled: {reason:?}");
                                            let _ = spawner.finish_background_child(
                                                &child_for_run,
                                                BackgroundStatus::Cancelled,
                                                text.clone(),
                                            );
                                            JobOutcome::Cancelled { reason: text }
                                        }
                                        TurnOutcome::AwaitingApproval { .. }
                                        | TurnOutcome::AwaitingChild { .. } => JobOutcome::Blocked {
                                            reason: "agent paused on durable dependency".to_owned(),
                                        },
                                        other => {
                                            let text = format!("agent ended without terminal completion: {other:?}");
                                            let _ = spawner.finish_background_child(
                                                &child_for_run,
                                                BackgroundStatus::Failed,
                                                text.clone(),
                                            );
                                            JobOutcome::Failed { reason: text }
                                        }
                                    },
                                    Err(error) => {
                                        let text = error.to_string();
                                        let _ = spawner.finish_background_child(
                                            &child_for_run,
                                            BackgroundStatus::Failed,
                                            text.clone(),
                                        );
                                        JobOutcome::Failed { reason: text }
                                    }
                                };
                                completion_tx.send_replace(true);
                                outcome
                            };
                            (control, future)
                        },
                    )
                    .await;
                if let Err(error) = result {
                    tracing::error!(
                        work_id = %work_id_for_run,
                        child = %child,
                        error = %error,
                        "agent_job.runtime_failed"
                    );
                }
            });

            Ok(AgentJobHandle { work_id, child })
        })
    }
}
