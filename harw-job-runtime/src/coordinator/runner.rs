//! The coordinator: store + executor + lease + deadline + recovery
//! (Job-Runtime-Doc §14, §15).
//!
//! ```text
//! submit ─create─▶ Queued ─claim(runner, TTL)─▶ Claimed ─▶ Starting
//!        ─executor.start─▶ Running ──┬─ exit 0 ───────────▶ Succeeded
//!                                    ├─ exit ≠ 0 / policy ─▶ Failed
//!                                    ├─ deadline ──────────▶ TimedOut
//!                                    ├─ cancel ────────────▶ Cancelled
//!                                    └─ lease lost ────────▶ Lost
//! ```
//!
//! While an attempt runs, the coordinator renews its lease every TTL/3. A
//! renewal that fails with a lost lease stops the process and records the
//! attempt as `Lost` *in its own attempt sidecar only*: the job record is
//! never written without a valid lease (fencing, Job-Runtime-Doc §7).
//!
//! # Persistence order
//! 1. claim (store) → 2. attempt sidecar `Starting` → 3. spawn →
//! 4. sidecar `Running` with the recovery identity → 5. on exit: job record
//!    completion (fenced) → 6. sidecar with the terminal state.
//!
//! A crash between any two steps leaves enough to decide deterministically
//! in [`Coordinator::recover`]: no identity ⇒ `Lost`; identity ⇒ probe.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use harw_job_core::{
    AttemptId, Budget, CancellationCause, Deadline, ExitOutcome, Job, JobClaim, JobKind,
    JobOutcome, JobScope, JobSpec, JobSpecEnvelope, LifecycleEvent, LifecycleState, RetryPolicy,
    RunnerId, SandboxReport, SandboxRequirement, SpecError, StoredJob,
};
use harw_job_store::{ClaimTerms, JobTransition, StoreResult};
use harw_types::{ApprovalActor, TenantId, WorkId, WorkspaceId};
use jiff::{SignedDuration, Timestamp};
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;

use super::attempt::{AttemptRecord, attempt_id_for};
use super::error::RuntimeError;
use super::executor::{AttemptContext, AttemptEvent, AttemptRun, Executor, Probe};
use super::store::CoordinatorStore;

/// Default lease validity.
pub const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(30);
/// Default number of stdout/stderr bytes kept in a [`JobResult`] per stream.
pub const DEFAULT_OUTPUT_LIMIT: usize = 1024 * 1024;
/// Shortest accepted lease TTL (the heartbeat runs at TTL/3).
pub const MIN_LEASE_TTL: Duration = Duration::from_millis(30);

/// Coordinator configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct CoordinatorConfig {
    /// Identity of this runner; must be stable across restarts for
    /// [`Coordinator::recover`] to find its jobs.
    pub runner_id: RunnerId,
    /// Absolute workspace root every `JobSpec::working_dir` is relative to.
    pub workspace_root: PathBuf,
    /// Lease validity; renewed every TTL/3.
    pub lease_ttl: Duration,
    /// Authority scope recorded on every job record.
    pub scope: JobScope,
    /// Retry policy recorded on every job record (the coordinator runs one
    /// attempt per submission; retries are the caller's decision).
    pub retry: RetryPolicy,
    /// Bytes of stdout/stderr kept per stream in a [`JobResult`].
    pub output_limit: usize,
}

impl CoordinatorConfig {
    /// A configuration with defaults: [`DEFAULT_LEASE_TTL`], a local scope
    /// (`local`/`local`, submitted by the runner as operator), a single
    /// attempt and [`DEFAULT_OUTPUT_LIMIT`].
    #[must_use]
    pub fn new(runner_id: RunnerId, workspace_root: impl Into<PathBuf>) -> Self {
        let submitter = ApprovalActor::Operator {
            id: runner_id.as_str().to_owned(),
        };
        Self {
            runner_id,
            workspace_root: workspace_root.into(),
            lease_ttl: DEFAULT_LEASE_TTL,
            scope: JobScope::new(
                TenantId::from_str("local"),
                WorkspaceId::from_str("local"),
                submitter,
            ),
            retry: RetryPolicy {
                max_attempts: 1,
                base_delay: SignedDuration::from_secs(1),
                factor: 2.0,
                max_delay: SignedDuration::from_secs(1),
            },
            output_limit: DEFAULT_OUTPUT_LIMIT,
        }
    }

    /// Sets the lease TTL (builder style).
    #[must_use]
    pub fn with_lease_ttl(mut self, ttl: Duration) -> Self {
        self.lease_ttl = ttl;
        self
    }

    fn lease_ttl_signed(&self) -> Result<SignedDuration, RuntimeError> {
        if self.lease_ttl < MIN_LEASE_TTL {
            return Err(RuntimeError::InvalidConfig {
                detail: format!(
                    "lease TTL {:?} is below the minimum of {MIN_LEASE_TTL:?}",
                    self.lease_ttl
                ),
            });
        }
        SignedDuration::try_from(self.lease_ttl).map_err(|error| RuntimeError::InvalidConfig {
            detail: format!("lease TTL {:?} is out of range: {error}", self.lease_ttl),
        })
    }

    fn validate(&self) -> Result<SignedDuration, RuntimeError> {
        if !self.workspace_root.is_absolute() {
            return Err(RuntimeError::InvalidConfig {
                detail: format!(
                    "workspace root '{}' must be absolute",
                    self.workspace_root.display()
                ),
            });
        }
        self.lease_ttl_signed()
    }
}

/// Final result of one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobResult {
    /// The job.
    pub job_id: WorkId,
    /// The attempt.
    pub attempt_id: AttemptId,
    /// Terminal lifecycle state.
    pub state: LifecycleState,
    /// How the primary ended, if observed.
    pub exit: Option<ExitOutcome>,
    /// Why the attempt was cancelled, if it was.
    pub cancellation: Option<CancellationCause>,
    /// Reason of a non-success state.
    pub reason: Option<String>,
    /// Achieved sandbox enforcement, if known.
    pub sandbox: Option<SandboxReport>,
    /// Whether a restarted coordinator finished this attempt.
    pub recovered: bool,
    /// Captured standard output (first `output_limit` bytes).
    pub stdout: Vec<u8>,
    /// Captured standard error (first `output_limit` bytes).
    pub stderr: Vec<u8>,
    /// Whether output beyond the limit was dropped.
    pub output_truncated: bool,
}

impl JobResult {
    /// Whether the attempt succeeded.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.state == LifecycleState::Succeeded
    }
}

type ResultSender = oneshot::Sender<Result<JobResult, RuntimeError>>;

/// Handle to a submitted (or recovered) job.
#[derive(Debug)]
pub struct JobHandle {
    job_id: WorkId,
    attempt_id: AttemptId,
    cancel: Arc<watch::Sender<bool>>,
    result: oneshot::Receiver<Result<JobResult, RuntimeError>>,
}

impl JobHandle {
    /// The job id.
    #[must_use]
    pub fn id(&self) -> &WorkId {
        &self.job_id
    }

    /// The attempt id.
    #[must_use]
    pub fn attempt_id(&self) -> &AttemptId {
        &self.attempt_id
    }

    /// Requests cancellation ([`CancellationCause::User`]). Idempotent;
    /// without effect once the attempt ended.
    pub fn cancel(&self) {
        self.cancel.send_replace(true);
    }

    /// Waits for the attempt to end.
    ///
    /// # Errors
    /// The error the attempt ended with (store failure while finalizing,
    /// ...), or [`RuntimeError::TaskFailed`] if the coordinator task
    /// vanished (runtime shut down).
    pub async fn wait(self) -> Result<JobResult, RuntimeError> {
        match self.result.await {
            Ok(result) => result,
            Err(_) => Err(RuntimeError::task("attempt task ended without a result")),
        }
    }
}

/// What [`Coordinator::recover`] decided for one job (Job-Runtime-Doc §15).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecoveryDecision {
    /// The verified process is still running and is supervised again.
    Reattached,
    /// The process ended while the runner was down; its recorded exit
    /// status finalized the attempt.
    ExitedWhileDown,
    /// The attempt is `Lost`: the process vanished without a recorded
    /// status, its identity did not match (it was **not** signalled), or no
    /// identity was persisted.
    Lost(String),
    /// The attempt sidecar was already terminal; only the job record was
    /// finalized.
    AlreadyFinal,
    /// Recovery of this job failed; the record is untouched.
    Failed(String),
}

/// One job handled by [`Coordinator::recover`].
#[derive(Debug)]
pub struct RecoveredJob {
    /// The job.
    pub job_id: WorkId,
    /// What was decided.
    pub decision: RecoveryDecision,
    /// Handle to wait for the final result (`None` if recovery failed).
    pub handle: Option<JobHandle>,
}

struct Inner<S, E> {
    store: Arc<S>,
    executor: E,
    config: CoordinatorConfig,
    lease_ttl: SignedDuration,
    active: Mutex<HashMap<String, Arc<watch::Sender<bool>>>>,
}

/// Coordinates a job store and an executor (Job-Runtime-Doc §14).
///
/// Cheap to clone; all clones share the same store, executor and registry
/// of active jobs.
pub struct Coordinator<S, E> {
    inner: Arc<Inner<S, E>>,
}

impl<S, E> Clone for Coordinator<S, E> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<S, E> std::fmt::Debug for Coordinator<S, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Coordinator")
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

/// Runs a store operation on Tokio's blocking pool.
async fn blocking<S, T, F>(store: &Arc<S>, operation: F) -> Result<T, RuntimeError>
where
    S: CoordinatorStore,
    T: Send + 'static,
    F: FnOnce(&S) -> StoreResult<T> + Send + 'static,
{
    let store = Arc::clone(store);
    tokio::task::spawn_blocking(move || operation(&*store))
        .await
        .map_err(RuntimeError::task)?
        .map_err(RuntimeError::from_store)
}

/// Bounded capture of stdout/stderr.
struct Output {
    limit: usize,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
}

impl Output {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            stdout: Vec::new(),
            stderr: Vec::new(),
            truncated: false,
        }
    }

    fn push(limit: usize, buffer: &mut Vec<u8>, chunk: &[u8]) -> bool {
        let room = limit.saturating_sub(buffer.len());
        let take = chunk.len().min(room);
        buffer.extend_from_slice(chunk.get(..take).unwrap_or_default());
        take < chunk.len()
    }

    fn stdout(&mut self, chunk: &[u8]) {
        self.truncated |= Self::push(self.limit, &mut self.stdout, chunk);
    }

    fn stderr(&mut self, chunk: &[u8]) {
        self.truncated |= Self::push(self.limit, &mut self.stderr, chunk);
    }
}

/// Outcome decision inputs.
struct Ending {
    exit: ExitOutcome,
    cause: Option<CancellationCause>,
    lease_lost: Option<String>,
}

/// Maps what happened to the lifecycle event and reason.
fn decide(
    ending: &Ending,
    requirement: SandboxRequirement,
    sandbox: Option<&SandboxReport>,
) -> (LifecycleEvent, Option<String>) {
    if let Some(detail) = &ending.lease_lost {
        return (
            LifecycleEvent::LoseLease,
            Some(format!("lease lost: {detail}")),
        );
    }
    match &ending.cause {
        Some(CancellationCause::Deadline) => {
            return (
                LifecycleEvent::TimeOut,
                Some(CancellationCause::Deadline.to_string()),
            );
        }
        Some(cause) => return (LifecycleEvent::Cancel, Some(cause.to_string())),
        None => {}
    }
    if ending.exit.is_success() {
        return (LifecycleEvent::Succeed, None);
    }
    if let Some(report) = sandbox {
        if !report.satisfies(requirement) {
            return (
                LifecycleEvent::Fail,
                Some(format!(
                    "policy: sandbox requirement not met (not fully enforced: {}); {}",
                    report.shortfalls().join(", "),
                    ending.exit
                )),
            );
        }
    }
    if ending.exit == ExitOutcome::Unknown {
        return (
            LifecycleEvent::LoseLease,
            Some("exit status of the process could not be observed".to_owned()),
        );
    }
    (LifecycleEvent::Fail, Some(ending.exit.to_string()))
}

/// The governance outcome recorded on the job record.
fn job_outcome<I>(record: &AttemptRecord<I>) -> JobOutcome {
    let reason = record
        .reason
        .clone()
        .unwrap_or_else(|| record.state.to_string());
    match record.state {
        LifecycleState::Succeeded => {
            let mut result = serde_json::Map::new();
            result.insert(
                "attempt_id".to_owned(),
                serde_json::Value::String(record.attempt_id.to_string()),
            );
            if let Some(exit) = record.exit {
                result.insert(
                    "exit".to_owned(),
                    serde_json::Value::String(exit.to_string()),
                );
            }
            JobOutcome::Succeeded {
                result: serde_json::Value::Object(result),
            }
        }
        LifecycleState::Cancelled => JobOutcome::Cancelled { reason },
        state => JobOutcome::Failed {
            reason: format!("{state}: {reason}"),
        },
    }
}

impl<S: CoordinatorStore, E: Executor> Coordinator<S, E> {
    /// Creates a coordinator.
    ///
    /// # Errors
    /// [`RuntimeError::InvalidConfig`] for a relative workspace root or a
    /// lease TTL below [`MIN_LEASE_TTL`].
    pub fn new(store: S, executor: E, config: CoordinatorConfig) -> Result<Self, RuntimeError> {
        let lease_ttl = config.validate()?;
        Ok(Self {
            inner: Arc::new(Inner {
                store: Arc::new(store),
                executor,
                config,
                lease_ttl,
                active: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &CoordinatorConfig {
        &self.inner.config
    }

    /// The store.
    #[must_use]
    pub fn store(&self) -> &S {
        &self.inner.store
    }

    /// The executor.
    #[must_use]
    pub fn executor(&self) -> &E {
        &self.inner.executor
    }

    /// Requests cancellation of an active job of this coordinator. Returns
    /// whether the job was active here.
    pub fn cancel(&self, job: &WorkId) -> bool {
        let active = self
            .inner
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match active.get(job.as_str()) {
            Some(sender) => {
                sender.send_replace(true);
                true
            }
            None => false,
        }
    }

    /// Submits a job: creates its record (`Queued`), claims it for this
    /// runner and starts the attempt in a background task.
    ///
    /// Must be called within a Tokio runtime.
    ///
    /// # Errors
    /// [`RuntimeError::InvalidSpec`], store errors on create/claim. Start
    /// and execution failures are reported through [`JobHandle::wait`].
    pub async fn submit(&self, envelope: JobSpecEnvelope) -> Result<JobHandle, RuntimeError> {
        let spec = envelope.clone().into_spec()?;
        let now = Timestamp::now();
        let record = self.new_record(&envelope, now)?;
        let job_id = record.job.id.clone();
        blocking(&self.inner.store, move |store| store.create_job(record)).await?;
        let runner = self.inner.config.runner_id.clone();
        let terms_ttl = self.inner.lease_ttl;
        let claim_job = job_id.clone();
        let claim = blocking(&self.inner.store, move |store| {
            store.claim_job(
                &claim_job,
                &runner,
                ClaimTerms {
                    lease_ttl: terms_ttl,
                    now: Timestamp::now(),
                },
            )
        })
        .await?;
        let attempt_id = attempt_id_for(&job_id, claim.lease.epoch)?;
        let (handle, cancel, done) = self.register(&job_id, &attempt_id);
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let result = run_claimed(&inner, claim, attempt_id, spec, cancel).await;
            inner.unregister(&job_id);
            let _ = done.send(result);
        });
        Ok(handle)
    }

    /// Recovers every running job whose lease `runner` holds
    /// (Job-Runtime-Doc §15, §21.4).
    ///
    /// Per job: load the attempt sidecar → probe the persisted identity →
    /// - verified alive: reattach and supervise again (heartbeats resume);
    /// - exited: finalize with the recorded exit status, else `Lost`;
    /// - mismatch / unverifiable / no identity: `Lost`. **Nothing is
    ///   signalled** — a PID alone never authorizes a signal.
    ///
    /// Must be called within a Tokio runtime.
    ///
    /// # Errors
    /// Store errors while listing the recovery set. Per-job failures are
    /// reported as [`RecoveryDecision::Failed`].
    pub async fn recover(&self, runner: &RunnerId) -> Result<Vec<RecoveredJob>, RuntimeError> {
        let listed = runner.clone();
        let jobs = blocking(&self.inner.store, move |store| {
            store.load_recovery_set(&listed)
        })
        .await?;
        let mut recovered = Vec::with_capacity(jobs.len());
        for stored in jobs {
            let job_id = stored.job.id.clone();
            match self.recover_one(runner, stored).await {
                Ok(job) => recovered.push(job),
                Err(error) => {
                    tracing::warn!(job = %job_id, %error, "job recovery failed");
                    recovered.push(RecoveredJob {
                        job_id,
                        decision: RecoveryDecision::Failed(error.to_string()),
                        handle: None,
                    });
                }
            }
        }
        Ok(recovered)
    }

    pub(crate) fn new_record(
        &self,
        envelope: &JobSpecEnvelope,
        now: Timestamp,
    ) -> Result<StoredJob, RuntimeError> {
        let config = &self.inner.config;
        let mut job = Job::new(
            WorkId::new(),
            JobKind::Custom("process".to_owned()),
            Budget::unbounded(),
            config.retry.clone(),
            now,
        );
        job.mark_ready(now)
            .map_err(|error| RuntimeError::InvalidConfig {
                detail: error.to_string(),
            })?;
        let input = serde_json::to_value(envelope).map_err(|error| {
            RuntimeError::InvalidSpec(SpecError::InvalidId {
                field: "envelope",
                reason: error.to_string(),
            })
        })?;
        Ok(StoredJob {
            job,
            scope: config.scope.clone(),
            input,
            submitted_at: now,
            not_before: now,
            lease: None,
            lease_epoch: 0,
            completion: None,
            cancellation: None,
            revision: 0,
            trace: None,
        })
    }

    fn register(
        &self,
        job_id: &WorkId,
        attempt_id: &AttemptId,
    ) -> (JobHandle, watch::Receiver<bool>, ResultSender) {
        let (cancel_sender, cancel_receiver) = watch::channel(false);
        let cancel_sender = Arc::new(cancel_sender);
        let (done, result) = oneshot::channel();
        self.inner
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(job_id.as_str().to_owned(), Arc::clone(&cancel_sender));
        (
            JobHandle {
                job_id: job_id.clone(),
                attempt_id: attempt_id.clone(),
                cancel: cancel_sender,
                result,
            },
            cancel_receiver,
            done,
        )
    }

    /// A handle whose result is already known.
    fn resolved(
        job_id: &WorkId,
        attempt_id: &AttemptId,
        result: Result<JobResult, RuntimeError>,
    ) -> JobHandle {
        let (cancel, _) = watch::channel(false);
        let (done, receiver) = oneshot::channel();
        let _ = done.send(result);
        JobHandle {
            job_id: job_id.clone(),
            attempt_id: attempt_id.clone(),
            cancel: Arc::new(cancel),
            result: receiver,
        }
    }

    async fn recover_one(
        &self,
        runner: &RunnerId,
        stored: StoredJob,
    ) -> Result<RecoveredJob, RuntimeError> {
        let inner = &self.inner;
        let job_id = stored.job.id.clone();
        let Some(lease) = stored.lease.clone() else {
            return Err(RuntimeError::InvalidConfig {
                detail: format!("job '{job_id}' is running without a lease"),
            });
        };
        let claim = JobClaim {
            job: stored.job.clone(),
            scope: stored.scope.clone(),
            token: lease.token(),
            lease: lease.clone(),
        };
        let attempt_id = attempt_id_for(&job_id, lease.epoch)?;
        let requirement = serde_json::from_value::<JobSpecEnvelope>(stored.input.clone())
            .ok()
            .and_then(|envelope| envelope.into_spec().ok())
            .map(|spec| spec.sandbox)
            .unwrap_or_default();
        let ctx = AttemptContext {
            job_id: job_id.clone(),
            attempt_id: attempt_id.clone(),
            runner_id: runner.clone(),
            workspace_root: inner.config.workspace_root.clone(),
        };
        let key = attempt_id.to_string();
        let bytes = blocking(&inner.store, move |store| store.read_attempt(&key)).await?;
        let mut record = match bytes {
            Some(bytes) => AttemptRecord::<E::Identity>::from_bytes(attempt_id.as_str(), &bytes)?,
            None => AttemptRecord::claimed(
                job_id.clone(),
                attempt_id.clone(),
                runner.clone(),
                lease.epoch,
                Timestamp::now(),
            )?,
        };
        record.recovered = true;

        if record.is_terminal() {
            // Crash between sidecar and record finalization is impossible by
            // the persistence order, but a record may still be open if the
            // sidecar was written by an older build: finalize the record.
            let outcome = job_outcome(&record);
            let result = finalize_record(inner, &claim, outcome).await;
            let result = result.map(|()| result_of(&record, Output::new(0)));
            return Ok(RecoveredJob {
                job_id: job_id.clone(),
                decision: RecoveryDecision::AlreadyFinal,
                handle: Some(Self::resolved(&job_id, &attempt_id, result)),
            });
        }

        let identity = match (record.state, record.identity.clone()) {
            (LifecycleState::Running, Some(identity)) => identity,
            (state, _) => {
                let reason = format!(
                    "attempt was '{state}' when the runner stopped and no recovery identity was persisted; \
                     the process (if any) was not signalled"
                );
                return self.finish_lost(claim, ctx, record, reason).await;
            }
        };

        let probe = match inner.executor.probe(&identity) {
            Ok(probe) => probe,
            Err(error) => Probe::Unverifiable(error.to_string()),
        };
        tracing::info!(job = %job_id, attempt = %attempt_id, ?probe, "recovery probe");
        match probe {
            Probe::Alive => match inner.executor.reattach(&identity, &ctx) {
                Ok(run) => {
                    let (handle, cancel, done) = self.register(&job_id, &attempt_id);
                    let task_inner = Arc::clone(inner);
                    let task_job = job_id.clone();
                    tokio::spawn(async move {
                        let result =
                            supervise(&task_inner, claim, ctx, record, run, requirement, cancel)
                                .await;
                        task_inner.unregister(&task_job);
                        let _ = done.send(result);
                    });
                    Ok(RecoveredJob {
                        job_id,
                        decision: RecoveryDecision::Reattached,
                        handle: Some(handle),
                    })
                }
                Err(RuntimeError::ProcessAlreadyExited { .. }) => {
                    self.finish_exited(claim, ctx, record, requirement).await
                }
                Err(error) => {
                    let reason = format!("cannot reattach: {error}; the process was not signalled");
                    self.finish_lost(claim, ctx, record, reason).await
                }
            },
            Probe::Exited => self.finish_exited(claim, ctx, record, requirement).await,
            Probe::Mismatch(detail) => {
                let reason =
                    format!("recovery identity mismatch ({detail}); the process was not signalled");
                self.finish_lost(claim, ctx, record, reason).await
            }
            Probe::Unverifiable(detail) => {
                let reason = format!(
                    "recovery identity cannot be verified ({detail}); the process was not signalled"
                );
                self.finish_lost(claim, ctx, record, reason).await
            }
        }
    }

    async fn finish_exited(
        &self,
        claim: JobClaim,
        ctx: AttemptContext,
        mut record: AttemptRecord<E::Identity>,
        requirement: SandboxRequirement,
    ) -> Result<RecoveredJob, RuntimeError> {
        let Some(exit) = self.inner.executor.recorded_exit(&ctx) else {
            let reason =
                "process exited while the runner was down; its exit status is unknown".to_owned();
            return self.finish_lost(claim, ctx, record, reason).await;
        };
        record.exit = Some(exit);
        let ending = Ending {
            exit,
            cause: None,
            lease_lost: None,
        };
        let (event, reason) = decide(&ending, requirement, record.sandbox.as_ref());
        let job_id = ctx.job_id.clone();
        let attempt_id = ctx.attempt_id.clone();
        let result = finish(
            &self.inner,
            &claim,
            &ctx,
            record,
            Output::new(0),
            event,
            reason,
            true,
        )
        .await;
        Ok(RecoveredJob {
            job_id: job_id.clone(),
            decision: RecoveryDecision::ExitedWhileDown,
            handle: Some(Self::resolved(&job_id, &attempt_id, result)),
        })
    }

    async fn finish_lost(
        &self,
        claim: JobClaim,
        ctx: AttemptContext,
        record: AttemptRecord<E::Identity>,
        reason: String,
    ) -> Result<RecoveredJob, RuntimeError> {
        tracing::warn!(job = %ctx.job_id, attempt = %ctx.attempt_id, %reason, "recovered attempt is lost");
        let job_id = ctx.job_id.clone();
        let attempt_id = ctx.attempt_id.clone();
        let result = finish(
            &self.inner,
            &claim,
            &ctx,
            record,
            Output::new(0),
            LifecycleEvent::LoseLease,
            Some(reason.clone()),
            true,
        )
        .await;
        Ok(RecoveredJob {
            job_id: job_id.clone(),
            decision: RecoveryDecision::Lost(reason),
            handle: Some(Self::resolved(&job_id, &attempt_id, result)),
        })
    }
}

impl<S, E> Inner<S, E> {
    fn unregister(&self, job: &WorkId) {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(job.as_str());
    }
}

async fn persist<S, E>(
    inner: &Arc<Inner<S, E>>,
    record: &AttemptRecord<E::Identity>,
) -> Result<(), RuntimeError>
where
    S: CoordinatorStore,
    E: Executor,
{
    let bytes = record.to_bytes()?;
    let attempt = record.attempt_id.to_string();
    blocking(&inner.store, move |store| {
        store.write_attempt(&attempt, &bytes)
    })
    .await
}

async fn finalize_record<S, E>(
    inner: &Arc<Inner<S, E>>,
    claim: &JobClaim,
    outcome: JobOutcome,
) -> Result<(), RuntimeError>
where
    S: CoordinatorStore,
    E: Executor,
{
    let claim = claim.clone();
    blocking(&inner.store, move |store| {
        store.transition(
            &claim,
            JobTransition::Complete {
                completed_at: Timestamp::now(),
                outcome,
            },
        )
    })
    .await
    .map(|_| ())
}

fn result_of<I>(record: &AttemptRecord<I>, output: Output) -> JobResult {
    JobResult {
        job_id: record.job_id.clone(),
        attempt_id: record.attempt_id.clone(),
        state: record.state,
        exit: record.exit,
        cancellation: record.cancellation.clone(),
        reason: record.reason.clone(),
        sandbox: record.sandbox,
        recovered: record.recovered,
        stdout: output.stdout,
        stderr: output.stderr,
        output_truncated: output.truncated,
    }
}

/// Applies the terminal `event`, finalizes the job record (fenced) unless
/// the lease is known to be lost, persists the sidecar and builds the
/// result.
#[allow(clippy::too_many_arguments)]
async fn finish<S, E>(
    inner: &Arc<Inner<S, E>>,
    claim: &JobClaim,
    ctx: &AttemptContext,
    mut record: AttemptRecord<E::Identity>,
    output: Output,
    event: LifecycleEvent,
    reason: Option<String>,
    write_record: bool,
) -> Result<JobResult, RuntimeError>
where
    S: CoordinatorStore,
    E: Executor,
{
    let now = Timestamp::now();
    let mut event = event;
    let mut reason = reason;
    let mut store_error = None;
    if write_record {
        // Tentatively apply to derive the governance outcome.
        let mut tentative = record.clone();
        tentative.apply(event, now)?;
        tentative.reason.clone_from(&reason);
        match finalize_record(inner, claim, job_outcome(&tentative)).await {
            Ok(()) => {}
            Err(error) if error.is_lease_lost() => {
                tracing::warn!(job = %ctx.job_id, %error, "lease lost before finalization");
                event = LifecycleEvent::LoseLease;
                reason = Some(format!("lease lost before finalization: {error}"));
                if record.cancellation.is_none() {
                    record.cancellation = Some(CancellationCause::LeaseLost);
                }
            }
            Err(error) => store_error = Some(error),
        }
    }
    record.apply(event, now)?;
    record.reason = reason;
    persist(inner, &record).await?;
    if let Some(error) = store_error {
        return Err(error);
    }
    inner.executor.finished(ctx);
    tracing::info!(
        job = %ctx.job_id,
        attempt = %ctx.attempt_id,
        state = %record.state,
        reason = record.reason.as_deref().unwrap_or(""),
        "attempt finished"
    );
    Ok(result_of(&record, output))
}

/// Drives one freshly claimed attempt from `Claimed` to a terminal state.
async fn run_claimed<S, E>(
    inner: &Arc<Inner<S, E>>,
    claim: JobClaim,
    attempt_id: AttemptId,
    spec: JobSpec,
    mut cancel: watch::Receiver<bool>,
) -> Result<JobResult, RuntimeError>
where
    S: CoordinatorStore,
    E: Executor,
{
    let job_id = claim.job.id.clone();
    let ctx = AttemptContext {
        job_id: job_id.clone(),
        attempt_id: attempt_id.clone(),
        runner_id: inner.config.runner_id.clone(),
        workspace_root: inner.config.workspace_root.clone(),
    };
    let mut record = AttemptRecord::<E::Identity>::claimed(
        job_id,
        attempt_id,
        ctx.runner_id.clone(),
        claim.lease.epoch,
        Timestamp::now(),
    )?;
    record.apply(LifecycleEvent::Start, Timestamp::now())?;
    persist(inner, &record).await?;

    if *cancel.borrow_and_update() {
        record.cancellation = Some(CancellationCause::User);
        let reason = Some(CancellationCause::User.to_string());
        return finish(
            inner,
            &claim,
            &ctx,
            record,
            Output::new(0),
            LifecycleEvent::Cancel,
            reason,
            true,
        )
        .await;
    }

    let deadline = match spec.resources.wall_timeout {
        Some(timeout) => Some(Deadline::after(Timestamp::now(), timeout).map_err(|error| {
            RuntimeError::InvalidSpec(SpecError::InvalidResource {
                resource: "wall_timeout",
                constraint: if matches!(error, harw_job_core::DeadlineError::Overflow { .. }) {
                    "is out of range"
                } else {
                    "must be positive"
                },
            })
        })?),
        None => None,
    };

    let started = match inner.executor.start(&spec, &ctx) {
        Ok(started) => started,
        Err(error) => {
            let reason = match &error {
                RuntimeError::SandboxRequirementNotMet { report, .. } => {
                    record.sandbox = Some(*report);
                    format!("policy: {error}; the job body did not run")
                }
                _ => format!("start failed: {error}"),
            };
            tracing::warn!(job = %ctx.job_id, %error, "attempt start failed");
            return finish(
                inner,
                &claim,
                &ctx,
                record,
                Output::new(0),
                LifecycleEvent::Fail,
                Some(reason),
                true,
            )
            .await;
        }
    };
    record.identity = started.identity;
    record.pid = started.pid;
    record.sandbox = started.sandbox;
    record.deadline = deadline;
    record.started_at = Some(Timestamp::now());
    let mut run = started.run;

    // Defense in depth: never count an attempt as Running whose known
    // enforcement contradicts a hard requirement.
    if let Some(report) = record.sandbox {
        if !report.satisfies(spec.sandbox) {
            run.cancel();
            let exit = drain(&mut run).await;
            record.exit = Some(exit);
            let reason = format!(
                "policy: sandbox requirement not met (not fully enforced: {}); the job was killed before it counted as running",
                report.shortfalls().join(", ")
            );
            return finish(
                inner,
                &claim,
                &ctx,
                record,
                Output::new(0),
                LifecycleEvent::Fail,
                Some(reason),
                true,
            )
            .await;
        }
    }

    record.apply(LifecycleEvent::Spawned, Timestamp::now())?;
    if let Err(error) = persist(inner, &record).await {
        // Without a durable identity the attempt would be unrecoverable:
        // stop it rather than run it unaccounted.
        tracing::error!(job = %ctx.job_id, %error, "cannot persist running attempt; stopping it");
        run.cancel();
        let _ = drain(&mut run).await;
        return Err(error);
    }
    supervise(inner, claim, ctx, record, run, spec.sandbox, cancel).await
}

/// Waits until the attempt reports its exit (or the executor vanished).
async fn drain(run: &mut AttemptRun) -> ExitOutcome {
    while let Some(event) = run.next_event().await {
        if let AttemptEvent::Exited { outcome, .. } = event {
            return outcome;
        }
    }
    ExitOutcome::Unknown
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Supervises a `Running` attempt: output capture, heartbeats, deadline,
/// cancellation, lease loss; then finalizes.
async fn supervise<S, E>(
    inner: &Arc<Inner<S, E>>,
    claim: JobClaim,
    ctx: AttemptContext,
    mut record: AttemptRecord<E::Identity>,
    mut run: AttemptRun,
    requirement: SandboxRequirement,
    mut cancel: watch::Receiver<bool>,
) -> Result<JobResult, RuntimeError>
where
    S: CoordinatorStore,
    E: Executor,
{
    let mut output = Output::new(inner.config.output_limit);
    let period = (inner.config.lease_ttl / 3).max(Duration::from_millis(10));
    let mut heartbeat = tokio::time::interval_at(Instant::now() + period, period);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let deadline_at = record.deadline.map(|deadline| {
        let remaining = deadline.remaining(Timestamp::now()).unsigned_abs();
        Instant::now() + remaining
    });
    let mut cause: Option<CancellationCause> = None;
    let mut lease_lost: Option<String> = None;
    let mut cancel_open = true;
    if *cancel.borrow_and_update() {
        cause = Some(CancellationCause::User);
        run.cancel();
    }
    let (exit, late_report) = loop {
        let armed = cause.is_none();
        tokio::select! {
            event = run.next_event() => match event {
                Some(AttemptEvent::Stdout(chunk)) => output.stdout(&chunk),
                Some(AttemptEvent::Stderr(chunk)) => output.stderr(&chunk),
                Some(AttemptEvent::Error(message)) => {
                    tracing::warn!(job = %ctx.job_id, %message, "supervision error");
                }
                Some(AttemptEvent::Exited { outcome, sandbox }) => break (outcome, sandbox),
                None => break (ExitOutcome::Unknown, None),
            },
            _ = heartbeat.tick(), if lease_lost.is_none() => {
                let renew_claim = claim.clone();
                let renewed = blocking(&inner.store, move |store| {
                    store.transition(&renew_claim, JobTransition::Renew { now: Timestamp::now() })
                })
                .await;
                match renewed {
                    Ok(_) => {}
                    Err(error) if error.is_lease_lost() => {
                        tracing::warn!(job = %ctx.job_id, %error, "lease lost; stopping attempt");
                        lease_lost = Some(error.to_string());
                        if cause.is_none() {
                            cause = Some(CancellationCause::LeaseLost);
                        }
                        run.cancel();
                    }
                    Err(error) => {
                        tracing::warn!(job = %ctx.job_id, %error, "lease renewal failed; retrying");
                    }
                }
            }
            () = sleep_until(deadline_at), if armed && deadline_at.is_some() => {
                cause = Some(CancellationCause::Deadline);
                run.cancel();
            }
            changed = cancel.changed(), if armed && cancel_open => {
                if changed.is_err() {
                    cancel_open = false;
                } else if *cancel.borrow_and_update() {
                    cause = Some(CancellationCause::User);
                    run.cancel();
                }
            }
        }
    };
    if late_report.is_some() {
        record.sandbox = late_report;
    }
    let mut exit = exit;
    if exit == ExitOutcome::Unknown {
        if let Some(recorded) = inner.executor.recorded_exit(&ctx) {
            exit = recorded;
        }
    }
    record.exit = Some(exit);
    record.cancellation.clone_from(&cause);
    let ending = Ending {
        exit,
        cause,
        lease_lost,
    };
    let (event, reason) = decide(&ending, requirement, record.sandbox.as_ref());
    let write_record = ending.lease_lost.is_none();
    finish(
        inner,
        &claim,
        &ctx,
        record,
        output,
        event,
        reason,
        write_record,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::{Ending, Output, decide};
    use harw_job_core::{
        CancellationCause, EnforcementState, ExitOutcome, LifecycleEvent, SandboxReport,
        SandboxRequirement,
    };

    fn ending(exit: ExitOutcome) -> Ending {
        Ending {
            exit,
            cause: None,
            lease_lost: None,
        }
    }

    #[test]
    fn decision_table() {
        let none = SandboxRequirement::BestEffort;
        assert_eq!(
            decide(&ending(ExitOutcome::Exited(0)), none, None).0,
            LifecycleEvent::Succeed
        );
        assert_eq!(
            decide(&ending(ExitOutcome::Exited(3)), none, None).0,
            LifecycleEvent::Fail
        );
        assert_eq!(
            decide(&ending(ExitOutcome::Unknown), none, None).0,
            LifecycleEvent::LoseLease
        );
        let mut deadline = ending(ExitOutcome::Signaled {
            signal: 15,
            core_dumped: false,
        });
        deadline.cause = Some(CancellationCause::Deadline);
        assert_eq!(decide(&deadline, none, None).0, LifecycleEvent::TimeOut);
        deadline.cause = Some(CancellationCause::User);
        assert_eq!(decide(&deadline, none, None).0, LifecycleEvent::Cancel);
        deadline.lease_lost = Some("x".into());
        assert_eq!(decide(&deadline, none, None).0, LifecycleEvent::LoseLease);

        let weak = SandboxReport::uniform(EnforcementState::Partial);
        let (event, reason) = decide(
            &ending(ExitOutcome::Exited(126)),
            SandboxRequirement::Required,
            Some(&weak),
        );
        assert_eq!(event, LifecycleEvent::Fail);
        assert!(reason.is_some_and(|reason| reason.starts_with("policy:")));
    }

    #[test]
    fn output_is_capped() {
        let mut output = Output::new(4);
        output.stdout(b"ab");
        output.stdout(b"cdef");
        output.stderr(b"xy");
        assert_eq!(output.stdout, b"abcd");
        assert_eq!(output.stderr, b"xy");
        assert!(output.truncated);
    }
}
