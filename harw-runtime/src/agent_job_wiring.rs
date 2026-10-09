//! Durable agent-job adapter.
//!
//! Agent delegation is a logical job, not a UI concern. This adapter admits
//! every detached agent run into the shared durable job ledger and drives it
//! through `DurableJobRunner` so WorkId, fencing, heartbeat and cancellation
//! are identical to other governed work.

#![forbid(unsafe_code)]

use harw_core::{
    BackgroundStatus, ChildRecoveryView, DurableJobRunner, ExecutionControl, JobExecutionRegistry,
    ManagedAgentSpawner, RecoveredChildDisposition, StateStore, TurnInput, TurnOutcome,
};
use harw_extension_api::{AgentJobFuture, AgentJobHandle, AgentJobSubmitter, AgentSpawnError};
use harw_job_core::{Budget, Job, JobKind, JobOutcome, JobScope, JobState, RetryPolicy, StoredJob};
use harw_session_store::{ApprovalStore, CancelRequest, ClaimRequest, JobListQuery, JobStore};
use harw_types::{ApprovalActor, SessionId, TenantId, WorkId, WorkspaceId};
use jiff::{SignedDuration, Timestamp};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

const AGENT_JOB_KIND: &str = "agent";
const AGENT_RECOVERY_SCHEMA: &str = "harw.agent-job.recovery/v1";
const AGENT_JOB_WORKER: &str = "harw-agent-runtime";
const AGENT_JOB_LEASE_SECS: i64 = 30;
const AGENT_JOB_MAX_ATTEMPTS: u32 = 3;
const AGENT_RECOVERY_POLL: Duration = Duration::from_secs(5);
const AGENT_RECOVERY_PAGE: usize = 100;
const AGENT_PENDING_STALE_SECS: i64 = 60;

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
        executions: Arc<JobExecutionRegistry>,
        actor: ApprovalActor,
        tenant: TenantId,
        workspace: WorkspaceId,
    ) -> Self {
        let runner = Arc::new(DurableJobRunner::new(Arc::clone(&job_store), executions));
        Self {
            spawner,
            state_store,
            approval_store,
            job_store,
            runner,
            scope: JobScope::new(tenant, workspace, actor),
        }
    }

    /// Starts the process-local supervisor for durable agent work.
    ///
    /// The loop reconciles expired JobStore leases, claims Ready agent jobs,
    /// and rehydrates their child sessions. The WorkId fence is acquired by
    /// DurableJobRunner before any child recovery occurs.
    pub fn start_recovery_worker(self: &Arc<Self>) {
        if let Err(error) = tokio::runtime::Handle::try_current() {
            tracing::debug!(%error, "agent_job.recovery_worker.no_tokio_runtime");
            return;
        }
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(AGENT_RECOVERY_POLL);
            loop {
                ticker.tick().await;
                if let Err(error) = runtime.recovery_pass().await {
                    tracing::warn!(%error, "agent_job.recovery_pass_failed");
                }
            }
        });
    }

    async fn recovery_pass(self: &Arc<Self>) -> Result<(), String> {
        self.reconcile_expired_jobs().await?;
        self.cleanup_terminal_child_leases().await?;
        self.cleanup_stale_pending().await?;
        let ready = self.list_jobs_in_states(vec![JobState::Ready]).await?;
        let now = Timestamp::now();
        for record in ready {
            if !self.owns_record(&record) || !Self::is_agent_record(&record) {
                continue;
            }
            if record.not_before > now {
                continue;
            }
            let snapshot = match Self::recovery_snapshot(&record) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    tracing::error!(
                        work_id = %record.job.id,
                        %error,
                        "agent_job.recovery_snapshot_invalid"
                    );
                    self.fail_unrecoverable_ready(&record, &error).await?;
                    continue;
                }
            };
            if self.spawner.child_record(&snapshot.child).is_some() {
                // Fresh same-process submission: its local runner owns this
                // already-admitted child and will claim this Ready job.
                continue;
            }
            match self
                .spawner
                .child_lease_owned_by(&snapshot.child, &record.job.id)
            {
                Ok(true) => {}
                Ok(false) => {
                    let reason = "durable child lease is not owned by this WorkId";
                    tracing::error!(
                        work_id = %record.job.id,
                        child = %snapshot.child,
                        "agent_job.recovery_owner_mismatch"
                    );
                    self.fail_unrecoverable_ready(&record, reason).await?;
                    continue;
                }
                Err(error) => {
                    tracing::warn!(
                        work_id = %record.job.id,
                        child = %snapshot.child,
                        error = %error,
                        "agent_job.recovery_owner_probe_failed"
                    );
                    continue;
                }
            }
            self.spawn_recovery_run(record.job.id.clone(), snapshot);
        }
        Ok(())
    }

    async fn reconcile_expired_jobs(&self) -> Result<(), String> {
        let store = Arc::clone(&self.job_store);
        tokio::task::spawn_blocking(move || {
            let mut cursor = None;
            loop {
                let page = store
                    .reconcile_expired(Timestamp::now(), AGENT_RECOVERY_PAGE, cursor.as_ref())
                    .map_err(|error| error.to_string())?;
                cursor = page.next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
            Ok::<(), String>(())
        })
        .await
        .map_err(|error| format!("reconcile task failed: {error}"))?
    }

    async fn list_jobs_in_states(&self, states: Vec<JobState>) -> Result<Vec<StoredJob>, String> {
        let store = Arc::clone(&self.job_store);
        tokio::task::spawn_blocking(move || {
            let mut cursor = None;
            let mut records = Vec::new();
            loop {
                let page = store
                    .list(&JobListQuery {
                        states: Some(states.clone()),
                        holder: None,
                        limit: AGENT_RECOVERY_PAGE,
                        cursor: cursor.clone(),
                    })
                    .map_err(|error| error.to_string())?;
                records.extend(page.jobs);
                cursor = page.next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
            Ok::<Vec<StoredJob>, String>(records)
        })
        .await
        .map_err(|error| format!("list task failed: {error}"))?
    }

    async fn cleanup_terminal_child_leases(&self) -> Result<(), String> {
        let terminal = self
            .list_jobs_in_states(vec![
                JobState::Completed,
                JobState::Failed,
                JobState::Cancelled,
            ])
            .await?;
        for record in terminal {
            if !self.owns_record(&record) || !Self::is_agent_record(&record) {
                continue;
            }
            let Some(child) = Self::stored_child_id(&record) else {
                continue;
            };
            if self.spawner.child_record(&child).is_some() {
                // The same-process runner has committed and still owns final
                // projection/lease cleanup. Never race that finalizer.
                continue;
            }
            if self
                .spawner
                .child_lease_owned_by(&child, &record.job.id)
                .unwrap_or(false)
            {
                let _ = self.spawner.close_child_durable(&child, Timestamp::now());
            }
        }
        Ok(())
    }

    async fn cleanup_stale_pending(&self) -> Result<(), String> {
        let pending = self.list_jobs_in_states(vec![JobState::Pending]).await?;
        let now = Timestamp::now();
        for record in pending {
            if !self.owns_record(&record) || !Self::is_agent_record(&record) {
                continue;
            }
            let stale = record
                .submitted_at
                .checked_add(SignedDuration::from_secs(AGENT_PENDING_STALE_SECS))
                .is_ok_and(|deadline| deadline <= now);
            if !stale {
                continue;
            }
            if let Some(child) = Self::stored_child_id(&record)
                && self.spawner.child_record(&child).is_some()
            {
                continue;
            }
            let snapshot = Self::recovery_snapshot(&record).ok();
            let store = Arc::clone(&self.job_store);
            let work_id = record.job.id.clone();
            let cancelled_by = self.scope.submitter().clone();
            tokio::task::spawn_blocking(move || {
                store.cancel(
                    &work_id,
                    &CancelRequest {
                        cancelled_at: Timestamp::now(),
                        cancelled_by,
                        reason: "stale pending agent job after process interruption".to_owned(),
                    },
                )
            })
            .await
            .map_err(|error| format!("pending cleanup task failed: {error}"))?
            .map_err(|error| error.to_string())?;
            if let Some(snapshot) = snapshot {
                let _ = self
                    .spawner
                    .close_child_durable(&snapshot.child, Timestamp::now());
            }
        }
        Ok(())
    }

    async fn fail_unrecoverable_ready(
        &self,
        record: &StoredJob,
        detail: &str,
    ) -> Result<(), String> {
        let store = Arc::clone(&self.job_store);
        let work_id = record.job.id.clone();
        let cancelled_by = self.scope.submitter().clone();
        let reason = format!("unrecoverable durable agent job: {detail}");
        tokio::task::spawn_blocking(move || {
            store.cancel(
                &work_id,
                &CancelRequest {
                    cancelled_at: Timestamp::now(),
                    cancelled_by,
                    reason,
                },
            )
        })
        .await
        .map_err(|error| format!("unrecoverable cleanup task failed: {error}"))?
        .map_err(|error| error.to_string())?;

        if let Some(child) = Self::stored_child_id(record)
            && self
                .spawner
                .child_lease_owned_by(&child, &record.job.id)
                .unwrap_or(false)
        {
            let _ = self.spawner.close_child_durable(&child, Timestamp::now());
        }
        Ok(())
    }

    fn stored_child_id(record: &StoredJob) -> Option<SessionId> {
        record
            .input
            .get("child_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(|value| SessionId::from_str(value.to_owned()))
    }

    fn owns_record(&self, record: &StoredJob) -> bool {
        record.scope.tenant() == self.scope.tenant()
            && record.scope.workspace() == self.scope.workspace()
    }

    fn is_agent_record(record: &StoredJob) -> bool {
        matches!(&record.job.kind, JobKind::Custom(kind) if kind == AGENT_JOB_KIND)
    }

    fn recovery_snapshot(record: &StoredJob) -> Result<ChildRecoveryView, String> {
        if record.input.get("kind").and_then(serde_json::Value::as_str) != Some(AGENT_JOB_KIND) {
            return Err("stored agent job kind marker is missing".to_owned());
        }
        let recovery = record
            .input
            .get("recovery")
            .ok_or_else(|| "stored agent job has no recovery envelope".to_owned())?;
        if recovery.get("schema").and_then(serde_json::Value::as_str) != Some(AGENT_RECOVERY_SCHEMA)
        {
            return Err("unsupported agent recovery schema".to_owned());
        }
        let value = recovery
            .get("snapshot")
            .cloned()
            .ok_or_else(|| "stored agent job has no recovery snapshot".to_owned())?;
        let snapshot: ChildRecoveryView = serde_json::from_value(value)
            .map_err(|error| format!("invalid agent recovery snapshot: {error}"))?;
        let child_id = record
            .input
            .get("child_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "stored agent job has no child_id".to_owned())?;
        if snapshot.child.as_str() != child_id {
            return Err("stored child_id does not match recovery snapshot child".to_owned());
        }
        Ok(snapshot)
    }

    fn spawn_recovery_run(self: &Arc<Self>, work_id: WorkId, snapshot: ChildRecoveryView) {
        let spawner = Arc::clone(&self.spawner);
        let state_store = Arc::clone(&self.state_store);
        let approvals = self.approval_store.clone();
        let runner = Arc::clone(&self.runner);
        tokio::spawn(async move {
            let child = snapshot.child.clone();
            let request = ClaimRequest {
                worker_id: AGENT_JOB_WORKER.to_owned(),
                lease_ttl: SignedDuration::from_secs(AGENT_JOB_LEASE_SECS),
                now: Timestamp::now(),
            };
            let spawner_for_run = Arc::clone(&spawner);
            let child_for_run = child.clone();
            let result = runner
                .run_with_cancel(&work_id, &request, move |_claim, job_cancel| {
                    let (completion_tx, completion_rx) = watch::channel(false);
                    let control: Arc<dyn ExecutionControl> =
                        Arc::new(AgentExecutionControl {
                            spawner: Arc::clone(&spawner_for_run),
                            child: child_for_run.clone(),
                            completion_rx,
                        });
                    let future = async move {
                        let outcome = match spawner_for_run
                            .recover_root_child(&snapshot, state_store.as_ref())
                            .await
                        {
                            Err(error) => JobOutcome::Failed {
                                reason: format!("agent recovery rejected: {error}"),
                            },
                            Ok(recovered) => {
                                // Rebuild the background projection before
                                // interpreting the recovered transcript so
                                // every terminal disposition can emit the same
                                // parent/UI notice as a non-restarted run.
                                if let Err(error) = spawner_for_run
                                    .detach_for_background(&recovered.child, None)
                                {
                                    JobOutcome::Failed {
                                        reason: format!(
                                            "agent recovery could not restore background ownership: {}",
                                            error.message
                                        ),
                                    }
                                } else {
                                    match recovered.disposition {
                                        RecoveredChildDisposition::UnsafeInterruptedToolCalls { count } => {
                                            JobOutcome::Failed {
                                                reason: format!(
                                                    "agent recovery found {count} interrupted tool call(s); side effects are ambiguous, so automatic continuation is refused"
                                                ),
                                            }
                                        }
                                        RecoveredChildDisposition::AlreadyCompleted { text } => {
                                            JobOutcome::Succeeded {
                                                result: serde_json::json!({
                                                    "child_id": recovered.child.as_str(),
                                                    "text": text,
                                                    "budget_exhausted": false,
                                                    "recovered_without_replay": true,
                                                }),
                                            }
                                        }
                                        RecoveredChildDisposition::Continue => {
                                            if job_cancel.is_cancelled() {
                                                JobOutcome::Cancelled {
                                                    reason: "agent recovery cancelled before continuation".to_owned(),
                                                }
                                            } else {
                                                match spawner_for_run
                                                    .run_child_with_declared_budget(
                                                        &recovered.child,
                                                        state_store.as_ref(),
                                                        approvals.as_deref(),
                                                        TurnInput::default(),
                                                    )
                                                    .await
                                                {
                                                    Ok(run) => match run.outcome {
                                                        TurnOutcome::Completed => JobOutcome::Succeeded {
                                                            result: serde_json::json!({
                                                                "child_id": recovered.child.as_str(),
                                                                "text": run.full_text.unwrap_or_else(|| "completed".to_owned()),
                                                                "budget_exhausted": run.budget_exhausted,
                                                                "recovered": true,
                                                            }),
                                                        },
                                                        TurnOutcome::Cancelled { reason } => {
                                                            JobOutcome::Cancelled {
                                                                reason: format!("agent cancelled: {reason:?}"),
                                                            }
                                                        }
                                                        other => JobOutcome::Failed {
                                                            reason: format!(
                                                                "recovered agent ended without terminal completion: {other:?}"
                                                            ),
                                                        },
                                                    },
                                                    Err(error) => JobOutcome::Failed {
                                                        reason: error.to_string(),
                                                    },
                                                }
                                            }
                                        }
                                    }
                                }
                            },
                        };
                        completion_tx.send_replace(true);
                        outcome
                    };
                    (control, future)
                })
                .await;

            match result {
                Ok(completion) => {
                    let projection = match &completion.outcome {
                        JobOutcome::Succeeded { result } => Some((
                            BackgroundStatus::Completed,
                            result
                                .get("text")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("agent completed")
                                .to_owned(),
                        )),
                        JobOutcome::Failed { reason } => {
                            Some((BackgroundStatus::Failed, reason.clone()))
                        }
                        JobOutcome::Cancelled { reason } => {
                            Some((BackgroundStatus::Cancelled, reason.clone()))
                        }
                        JobOutcome::Blocked { reason } => {
                            tracing::error!(
                                work_id = %work_id,
                                child = %child,
                                reason,
                                "agent_job.recovery_blocked_without_resume_owner"
                            );
                            None
                        }
                    };
                    if let Some((status, text)) = projection {
                        if let Err(error) =
                            spawner.close_child_durable(&child, completion.completed_at)
                        {
                            tracing::error!(
                                work_id = %work_id,
                                child = %child,
                                error = %error,
                                "agent_job.recovery_child_lease_completion_failed"
                            );
                        } else {
                            let _ = spawner.finish_background_child(&child, status, text);
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        work_id = %work_id,
                        child = %child,
                        error = %error,
                        "agent_job.recovery_run_failed"
                    );
                }
            }
        });
    }

    fn rejection(detail: impl Into<String>) -> AgentSpawnError {
        AgentSpawnError {
            kind: Default::default(),
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

            let recovery = match self.spawner.child_recovery_view(&child) {
                Some(recovery) => recovery,
                None => {
                    let reason = format!(
                        "child {child} has no complete trusted recovery view before durable admission"
                    );
                    if let Err(error) = self.spawner.release_child(&child) {
                        tracing::error!(
                            child = %child,
                            error = %error,
                            "agent_job.recovery_snapshot_release_failed"
                        );
                    }
                    return Err(Self::rejection(reason));
                }
            };
            let trace = recovery.trace.clone();

            let work_id = WorkId::new();
            let now = Timestamp::now();
            let retry = RetryPolicy::try_new(
                AGENT_JOB_MAX_ATTEMPTS,
                SignedDuration::from_secs(1),
                2.0,
                SignedDuration::from_secs(10),
            )
            .map_err(|error| Self::rejection(error.to_string()))?;
            // Phase 1: persist the work as Pending. A Pending record is
            // durable but not claimable, so a crash before ownership transfer
            // cannot start an agent whose background lifecycle was never
            // established.
            let job = Job::new(
                work_id.clone(),
                JobKind::Custom(AGENT_JOB_KIND.to_owned()),
                Budget::unbounded(),
                retry,
                now,
            );
            let record = StoredJob {
                job,
                scope: self.scope.clone(),
                input: serde_json::json!({
                    "kind": AGENT_JOB_KIND,
                    "child_id": child.as_str(),
                    "task": task,
                    "recovery": {
                        "schema": AGENT_RECOVERY_SCHEMA,
                        "snapshot": recovery,
                    },
                }),
                submitted_at: now,
                not_before: now,
                lease: None,
                lease_epoch: 0,
                completion: None,
                cancellation: None,
                revision: 0,
                trace,
            };

            let store = Arc::clone(&self.job_store);
            let record_for_admit = record.clone();
            tokio::task::spawn_blocking(move || store.admit(&record_for_admit))
                .await
                .map_err(|error| {
                    Self::rejection(format!("agent job admission task failed: {error}"))
                })?
                .map_err(|error| Self::rejection(format!("agent job admission failed: {error}")))?;

            // Phase 2: transfer the already-admitted child to the background
            // owner. Only after this succeeds may the durable record become
            // claimable.
            if let Err(error) = self.spawner.detach_for_background(&child, Some(&task)) {
                let reason = format!(
                    "agent background ownership transfer failed: {}",
                    error.message
                );
                let store = Arc::clone(&self.job_store);
                let work_id_for_cancel = work_id.clone();
                let cancelled_by = self.scope.submitter().clone();
                let reason_for_cancel = reason.clone();
                let cleanup = tokio::task::spawn_blocking(move || {
                    store.cancel(
                        &work_id_for_cancel,
                        &CancelRequest {
                            cancelled_at: Timestamp::now(),
                            cancelled_by,
                            reason: reason_for_cancel,
                        },
                    )
                })
                .await;
                match cleanup {
                    Err(join_error) => tracing::error!(
                        work_id = %work_id,
                        error = %join_error,
                        "agent_job.pending_cleanup_task_failed"
                    ),
                    Ok(Err(cancel_error)) => tracing::error!(
                        work_id = %work_id,
                        error = %cancel_error,
                        "agent_job.pending_cleanup_failed"
                    ),
                    Ok(Ok(_)) => {
                        if let Err(release_error) = self.spawner.release_child(&child) {
                            tracing::error!(
                                work_id = %work_id,
                                child = %child,
                                error = %release_error,
                                "agent_job.pending_child_release_failed"
                            );
                        }
                    }
                }
                return Err(Self::rejection(reason));
            }

            // Once background ownership exists, transfer restart ownership of
            // the child lease to this WorkId. From this point the generic child
            // reaper must not race the job runtime for recovery.
            if let Err(error) = self.spawner.bind_child_job_owner(&child, &work_id) {
                let reason = format!(
                    "agent child/job ownership binding failed: {}",
                    error.message
                );
                let store = Arc::clone(&self.job_store);
                let work_id_for_cancel = work_id.clone();
                let cancelled_by = self.scope.submitter().clone();
                let reason_for_cancel = reason.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    store.cancel(
                        &work_id_for_cancel,
                        &CancelRequest {
                            cancelled_at: Timestamp::now(),
                            cancelled_by,
                            reason: reason_for_cancel,
                        },
                    )
                })
                .await;
                let _ = self.spawner.release_child(&child);
                return Err(Self::rejection(reason));
            }

            // Phase 3: Ready means durable admission, background ownership and
            // durable child/job correlation are established.
            let store = Arc::clone(&self.job_store);
            let work_id_for_ready = work_id.clone();
            let activation = tokio::task::spawn_blocking(move || {
                store.mark_ready(&work_id_for_ready, Timestamp::now())
            })
            .await;
            let activation_error = match activation {
                Ok(Ok(_)) => None,
                Ok(Err(error)) => Some(format!("agent job activation failed: {error}")),
                Err(error) => Some(format!("agent job activation task failed: {error}")),
            };
            if let Some(reason) = activation_error {
                let store = Arc::clone(&self.job_store);
                let work_id_for_cancel = work_id.clone();
                let cancelled_by = self.scope.submitter().clone();
                let reason_for_cancel = reason.clone();
                let cleanup = tokio::task::spawn_blocking(move || {
                    store.cancel(
                        &work_id_for_cancel,
                        &CancelRequest {
                            cancelled_at: Timestamp::now(),
                            cancelled_by,
                            reason: reason_for_cancel,
                        },
                    )
                })
                .await;
                match cleanup {
                    Err(join_error) => tracing::error!(
                        work_id = %work_id,
                        error = %join_error,
                        "agent_job.activation_cleanup_task_failed"
                    ),
                    Ok(Err(cancel_error)) => tracing::error!(
                        work_id = %work_id,
                        error = %cancel_error,
                        "agent_job.activation_cleanup_failed"
                    ),
                    Ok(Ok(completion)) => {
                        if let Err(release_error) = self
                            .spawner
                            .close_child_durable(&child, completion.completed_at)
                        {
                            tracing::error!(
                                work_id = %work_id,
                                child = %child,
                                error = %release_error,
                                "agent_job.activation_child_lease_completion_failed"
                            );
                        } else {
                            let _ = self.spawner.finish_background_child(
                                &child,
                                BackgroundStatus::Failed,
                                reason.clone(),
                            );
                        }
                    }
                }
                return Err(Self::rejection(reason));
            }

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
                let spawner_for_run = Arc::clone(&spawner);
                let child_for_cleanup = child_for_run.clone();
                let result = runner
                    .run_with_cancel(
                        &work_id_for_run,
                        &request,
                        move |_claim, _job_cancel| {
                            let (completion_tx, completion_rx) = watch::channel(false);
                            let control: Arc<dyn ExecutionControl> =
                                Arc::new(AgentExecutionControl {
                                    spawner: Arc::clone(&spawner_for_run),
                                    child: child_for_run.clone(),
                                    completion_rx,
                                });
                            let future = async move {
                                let run = spawner_for_run
                                    .run_child_with_declared_budget(
                                        &child_for_run,
                                        state_store.as_ref(),
                                        approvals.as_deref(),
                                        TurnInput::user(task_for_run),
                                    )
                                    .await;

                                // This future returns only the durable job
                                // outcome. Background projection and child
                                // lease finalization happen *after* the runner
                                // has fenced and committed that outcome.
                                let outcome = match run {
                                    Ok(result) => match result.outcome {
                                        TurnOutcome::Completed => {
                                            let text = result
                                                .full_text
                                                .unwrap_or_else(|| "completed".to_owned());
                                            JobOutcome::Succeeded {
                                                result: serde_json::json!({
                                                    "child_id": child_for_run.as_str(),
                                                    "text": text,
                                                    "budget_exhausted": result.budget_exhausted,
                                                }),
                                            }
                                        }
                                        TurnOutcome::Cancelled { reason } => {
                                            JobOutcome::Cancelled {
                                                reason: format!("agent cancelled: {reason:?}"),
                                            }
                                        }
                                        TurnOutcome::AwaitingApproval { .. } => {
                                            JobOutcome::Failed {
                                                reason: "agent paused on an unresolved approval after the durable approval relay exhausted; no resumable agent-job dependency owner is installed"
                                                    .to_owned(),
                                            }
                                        }
                                        TurnOutcome::AwaitingChild {
                                            child: nested_child,
                                            role,
                                            ..
                                        } => {
                                            // Synchronous nested delegation is
                                            // an explicit compatibility mode.
                                            // The durable agent-job driver has
                                            // no dependency re-driver yet, so
                                            // never publish a fake Blocked
                                            // record that can be set Ready with
                                            // nobody left to claim it.
                                            let cleanup = spawner_for_run
                                                .release_child(&nested_child)
                                                .err()
                                                .map(|error| format!("; nested child cleanup failed: {error}"))
                                                .unwrap_or_default();
                                            JobOutcome::Failed {
                                                reason: format!(
                                                    "agent paused on inline handoff to '{role}'; nested inline pauses do not yet have a durable resume owner—use async delegation or a durable work graph{cleanup}"
                                                ),
                                            }
                                        }
                                        other => JobOutcome::Failed {
                                            reason: format!(
                                                "agent ended without terminal completion: {other:?}"
                                            ),
                                        },
                                    },
                                    Err(error) => JobOutcome::Failed {
                                        reason: error.to_string(),
                                    },
                                };
                                completion_tx.send_replace(true);
                                outcome
                            };
                            (control, future)
                        },
                    )
                    .await;

                match result {
                    Ok(completion) => {
                        // WorkId is the source of truth. Only after its
                        // terminal outcome is durable may the subordinate
                        // child lease be completed and the in-memory
                        // background projection emit its terminal notice.
                        let projection = match &completion.outcome {
                            JobOutcome::Succeeded { result } => Some((
                                BackgroundStatus::Completed,
                                result
                                    .get("text")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("agent completed")
                                    .to_owned(),
                            )),
                            JobOutcome::Failed { reason } => {
                                Some((BackgroundStatus::Failed, reason.clone()))
                            }
                            JobOutcome::Cancelled { reason } => {
                                Some((BackgroundStatus::Cancelled, reason.clone()))
                            }
                            JobOutcome::Blocked { reason } => {
                                tracing::error!(
                                    work_id = %work_id_for_run,
                                    child = %child_for_cleanup,
                                    reason,
                                    "agent_job.blocked_without_resume_owner"
                                );
                                None
                            }
                        };

                        if let Some((status, text)) = projection {
                            if let Err(error) = spawner
                                .close_child_durable(
                                    &child_for_cleanup,
                                    completion.completed_at,
                                )
                            {
                                tracing::error!(
                                    work_id = %work_id_for_run,
                                    child = %child_for_cleanup,
                                    error = %error,
                                    "agent_job.child_lease_completion_failed"
                                );
                            } else {
                                let _ = spawner.finish_background_child(
                                    &child_for_cleanup,
                                    status,
                                    text,
                                );
                            }
                        }
                    }
                    Err(error) => {
                        // A failed runner may mean lease loss or a rejected
                        // fenced commit. Do not synthesize a terminal child
                        // projection here: recovery owns that decision.
                        tracing::error!(
                            work_id = %work_id_for_run,
                            child = %child_for_cleanup,
                            error = %error,
                            "agent_job.runtime_failed"
                        );
                    }
                }
            });

            Ok(AgentJobHandle { work_id, child })
        })
    }
}
