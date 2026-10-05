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
//!
//! # Retries (Job-Runtime-Doc §6)
//! When an attempt ends `Failed`, `TimedOut` or `Lost` (never `Cancelled`,
//! never a policy refusal, never after the lease was lost) and the job's
//! [`RetryPolicy`] admits another attempt, the coordinator does not
//! finalize the job record. It closes the attempt sidecar, persists the
//! next attempt (`<job>-e<epoch>-r<n>`) whose
//! history starts with the lifecycle `Retry` transition, waits the backoff
//! of [`RetryPolicy::next_delay`] while it keeps heartbeating the lease and
//! watching for cancellation, and starts the attempt again. Each attempt
//! gets its own wall-clock deadline; the job's wall-time [`Budget`] caps
//! the attempts and backoffs together. The default policy
//! ([`CoordinatorConfig::new`]) allows a single attempt.
//!
//! All attempts of one claim run under the same lease; the job record is
//! written once, by the attempt that decides the outcome. The store-level
//! attempt counter (`Job::attempts`) is only advanced by the store's own
//! reclaim path (lease expiry), which the in-lease retries do not replace.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use harw_job_core::{
    AttemptId, Budget, BudgetUsage, CancellationCause, Deadline, ExitOutcome, Job, JobClaim,
    JobKind, JobOutcome, JobScope, JobSpec, JobSpecEnvelope, LifecycleEvent, LifecycleState,
    LifecycleTransition, RetryPolicy, RunnerId, SandboxReport, SandboxRequirement, SpecError,
    StoredJob,
};
use harw_job_store::{ClaimTerms, JobTransition, StoreResult};
use harw_types::{ApprovalActor, TenantId, WorkId, WorkspaceId};
use jiff::{SignedDuration, Timestamp};
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;

use super::attempt::{AttemptRecord, attempt_id_for};
use super::capture::{Output, OutputCapture};
use super::error::RuntimeError;
use super::executor::{
    AttemptContext, AttemptEvent, AttemptRun, Executor, OutputFiles, Probe, StdioHandoff,
};
use super::frames::{DEFAULT_FRAME_BUFFER, FrameTap, JobFrame, JobFrames};
use super::store::CoordinatorStore;

/// Default lease validity.
pub const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(30);
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
    /// Retry policy recorded on every job record. The coordinator retries a
    /// retryable attempt outcome while the policy admits another attempt
    /// (see the module docs); `max_attempts: 1` runs exactly one attempt.
    pub retry: RetryPolicy,
    /// How much stdout/stderr a [`JobResult`] keeps per stream (first
    /// `head_bytes` + last `tail_bytes`).
    pub output_capture: OutputCapture,
    /// Frames of a job kept in flight per subscriber (see
    /// [`JobFrames`](super::JobFrames)); a subscriber that falls further
    /// behind lags instead of slowing the job.
    pub frame_buffer: usize,
}

impl CoordinatorConfig {
    /// A configuration with defaults: [`DEFAULT_LEASE_TTL`], a local scope
    /// (`local`/`local`, submitted by the runner as operator), a single
    /// attempt and [`OutputCapture::default`] (16 KiB head + 64 KiB tail).
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
            output_capture: OutputCapture::default(),
            frame_buffer: DEFAULT_FRAME_BUFFER,
        }
    }

    /// Sets the lease TTL (builder style).
    #[must_use]
    pub fn with_lease_ttl(mut self, ttl: Duration) -> Self {
        self.lease_ttl = ttl;
        self
    }

    /// Sets the retry policy recorded on new jobs (builder style).
    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
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

/// Final result of a job: the attempt that decided its outcome.
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
    /// Reason of a non-success state (with the attempt number when retries
    /// ran), or supervision notes of a recovered attempt (for example a job
    /// cgroup that could not be reopened).
    pub reason: Option<String>,
    /// Achieved sandbox enforcement, if known.
    pub sandbox: Option<SandboxReport>,
    /// Whether a restarted coordinator finished this attempt.
    pub recovered: bool,
    /// Captured standard output: the first `head_bytes` followed directly
    /// by the last `tail_bytes` (see [`OutputCapture`]). The whole output
    /// when [`Self::stdout_omitted`] is 0.
    pub stdout: Vec<u8>,
    /// Bytes of standard output dropped between head and tail.
    pub stdout_omitted: u64,
    /// Captured standard error, like [`Self::stdout`].
    pub stderr: Vec<u8>,
    /// Bytes of standard error dropped between head and tail.
    pub stderr_omitted: u64,
    /// Whether any output was dropped (`stdout_omitted` or
    /// `stderr_omitted` is non-zero).
    pub output_truncated: bool,
    /// The capture policy the output was taken with; locates the gap in
    /// [`Self::stdout`] / [`Self::stderr`].
    pub output_capture: OutputCapture,
}

impl JobResult {
    /// Whether the attempt succeeded.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.state == LifecycleState::Succeeded
    }

    /// Standard output before the omitted gap; all of it when nothing was
    /// omitted.
    #[must_use]
    pub fn stdout_head(&self) -> &[u8] {
        self.output_capture
            .split(&self.stdout, self.stdout_omitted)
            .0
    }

    /// Standard output after the omitted gap (the end of the stream); all
    /// of it when nothing was omitted.
    #[must_use]
    pub fn stdout_tail(&self) -> &[u8] {
        self.output_capture
            .split(&self.stdout, self.stdout_omitted)
            .1
    }

    /// Standard error before the omitted gap; all of it when nothing was
    /// omitted.
    #[must_use]
    pub fn stderr_head(&self) -> &[u8] {
        self.output_capture
            .split(&self.stderr, self.stderr_omitted)
            .0
    }

    /// Standard error after the omitted gap (the end of the stream); all of
    /// it when nothing was omitted.
    #[must_use]
    pub fn stderr_tail(&self) -> &[u8] {
        self.output_capture
            .split(&self.stderr, self.stderr_omitted)
            .1
    }

    /// Bytes the job wrote to standard output in total.
    #[must_use]
    pub fn stdout_total_bytes(&self) -> u64 {
        total_bytes(&self.stdout, self.stdout_omitted)
    }

    /// Bytes the job wrote to standard error in total.
    #[must_use]
    pub fn stderr_total_bytes(&self) -> u64 {
        total_bytes(&self.stderr, self.stderr_omitted)
    }
}

fn total_bytes(captured: &[u8], omitted: u64) -> u64 {
    u64::try_from(captured.len())
        .unwrap_or(u64::MAX)
        .saturating_add(omitted)
}

type ResultSender = oneshot::Sender<Result<JobResult, RuntimeError>>;

/// What the durable job record keeps of a job's [`JobSpec`].
///
/// The record is written to the job store and read back by
/// [`Coordinator::recover`]; a spec holds the command line and the
/// environment, which can carry secrets. The class decides how much of it is
/// written. The job itself always runs from the spec held in memory; a
/// record that does not carry the full spec is **never re-run** from the
/// store after a restart (recovery can reattach to a verified process or
/// finalize the record, nothing more).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Persistence {
    /// The full spec, recoverable and retryable after a restart. For
    /// background jobs and workers.
    #[default]
    Durable,
    /// The program name, the argument **count**, the environment variable
    /// **names** and the lifecycle; no argument and no environment value.
    MetadataOnly,
    /// Nothing about the command is written; only that a job ran. For
    /// interactive commands whose input may hold anything.
    Ephemeral,
}

/// Options of [`Coordinator::submit_with`].
#[derive(Debug, Clone, Default)]
pub struct SubmitOptions {
    /// Append standard output and error to these files instead of piping them
    /// (no frames, no captured output in the result).
    pub output_files: Option<OutputFiles>,
    /// Hand pipes of the first attempt to the submitter (see [`StdioHandoff`]).
    pub stdio_handoff: Option<StdioHandoff>,
    /// How much of the spec the job record keeps.
    pub persistence: Persistence,
    /// Also return a subscription that sees the job from its very first
    /// frame.
    pub stream: bool,
}

/// Handle to a submitted (or recovered) job.
#[derive(Debug)]
pub struct JobHandle {
    job_id: WorkId,
    attempt_id: AttemptId,
    cancel: Arc<watch::Sender<bool>>,
    result: oneshot::Receiver<Result<JobResult, RuntimeError>>,
    tap: FrameTap,
}

impl JobHandle {
    /// The job id.
    #[must_use]
    pub fn id(&self) -> &WorkId {
        &self.job_id
    }

    /// The attempt id this handle was created with (the first attempt, or
    /// the recovered one). Retries run under new ids;
    /// [`JobResult::attempt_id`] names the attempt that decided the outcome.
    #[must_use]
    pub fn attempt_id(&self) -> &AttemptId {
        &self.attempt_id
    }

    /// Subscribes to the job's live frames from now on. A subscriber that
    /// must not miss the start asks for it at submit time
    /// ([`Coordinator::submit_with`]). The stream ends when the job is
    /// finished.
    #[must_use]
    pub fn subscribe(&self) -> JobFrames {
        self.tap.subscribe()
    }

    /// Requests cancellation ([`CancellationCause::User`]). Idempotent;
    /// without effect once the job ended. Also stops a pending retry.
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
    /// The attempt sidecar was already terminal; the job record was
    /// finalized, or the retry the policy admits was scheduled.
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
    taps: Mutex<HashMap<String, FrameTap>>,
    /// Output files of jobs submitted with [`SubmitOptions::output_files`].
    outputs: Mutex<HashMap<String, OutputFiles>>,
    /// Stdio handoffs of jobs submitted with [`SubmitOptions::stdio_handoff`].
    handoffs: Mutex<HashMap<String, StdioHandoff>>,
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
                taps: Mutex::new(HashMap::new()),
                outputs: Mutex::new(HashMap::new()),
                handoffs: Mutex::new(HashMap::new()),
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
        self.submit_with(envelope, SubmitOptions::default())
            .await
            .map(|(handle, _)| handle)
    }

    /// [`Coordinator::submit`] with options: how much of the spec the job
    /// record keeps, and optionally a live subscription taken before the job
    /// starts (so it cannot miss the first frame).
    ///
    /// # Errors
    /// As [`Coordinator::submit`].
    pub async fn submit_with(
        &self,
        envelope: JobSpecEnvelope,
        options: SubmitOptions,
    ) -> Result<(JobHandle, Option<JobFrames>), RuntimeError> {
        let spec = envelope.clone().into_spec()?;
        let now = Timestamp::now();
        let record = self.new_record_with(&envelope, now, options.persistence)?;
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
        if let Some(files) = options.output_files.clone() {
            self.inner
                .outputs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(job_id.as_str().to_owned(), files);
        }
        if let Some(handoff) = options.stdio_handoff.clone() {
            self.inner
                .handoffs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(job_id.as_str().to_owned(), handoff);
        }
        let (handle, mut cancel, done) = self.register(&job_id, &attempt_id);
        let frames = options.stream.then(|| handle.subscribe());
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let result = run_job(&inner, claim, attempt_id, spec, &mut cancel).await;
            inner.unregister(&job_id);
            let _ = done.send(result);
        });
        Ok((handle, frames))
    }

    /// Recovers every running job whose lease `runner` holds
    /// (Job-Runtime-Doc §15, §21.4).
    ///
    /// Per job: load the latest attempt sidecar of the claim (retries
    /// included) → probe the persisted identity →
    /// - verified alive: reattach and supervise again (heartbeats resume);
    /// - exited: finalize with the recorded exit status, else `Lost`;
    /// - mismatch / unverifiable / no identity: `Lost`. **Nothing is
    ///   signalled** — a PID alone never authorizes a signal.
    ///
    /// An outcome decided here is retried like any other when the job's
    /// retry policy admits another attempt; the handle then waits for the
    /// retry.
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

    #[cfg(test)]
    pub(crate) fn new_record(
        &self,
        envelope: &JobSpecEnvelope,
        now: Timestamp,
    ) -> Result<StoredJob, RuntimeError> {
        self.new_record_with(envelope, now, Persistence::Durable)
    }

    pub(crate) fn new_record_with(
        &self,
        envelope: &JobSpecEnvelope,
        now: Timestamp,
        persistence: Persistence,
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
        let input = match persistence {
            Persistence::Durable => serde_json::to_value(envelope).map_err(|error| {
                RuntimeError::InvalidSpec(SpecError::InvalidId {
                    field: "envelope",
                    reason: error.to_string(),
                })
            })?,
            // Deliberately **not** a `JobSpecEnvelope`: recovery then finds no
            // spec and can never re-run the command from this record.
            Persistence::MetadataOnly | Persistence::Ephemeral => {
                redacted_input(&envelope.clone().into_spec()?, persistence)
            }
        };
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
        let tap = FrameTap::new(self.inner.config.frame_buffer);
        self.inner
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(job_id.as_str().to_owned(), Arc::clone(&cancel_sender));
        self.inner
            .taps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(job_id.as_str().to_owned(), tap.clone());
        (
            JobHandle {
                job_id: job_id.clone(),
                attempt_id: attempt_id.clone(),
                cancel: cancel_sender,
                result,
                tap,
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
            tap: FrameTap::closed(),
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
        let spec = serde_json::from_value::<JobSpecEnvelope>(stored.input.clone())
            .ok()
            .and_then(|envelope| envelope.into_spec().ok());
        let requirement = spec.as_ref().map(|spec| spec.sandbox).unwrap_or_default();
        let (number, attempt_id, bytes) =
            latest_attempt(inner, &job_id, lease.epoch, stored.job.retry.max_attempts).await?;
        let ctx = AttemptContext {
            job_id: job_id.clone(),
            attempt_id: attempt_id.clone(),
            runner_id: runner.clone(),
            lease_epoch: lease.epoch,
            workspace_root: inner.config.workspace_root.clone(),
            output_files: None,
            stdio_handoff: None,
        };
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
        let recovering = Recovering {
            claim,
            ctx,
            spec,
            requirement,
            number,
        };

        if record.is_terminal() {
            return self.recover_terminal(recovering, record).await;
        }

        let identity = match (record.state, record.identity.clone()) {
            (LifecycleState::Running, Some(identity)) => identity,
            (state, _) => {
                let reason = format!(
                    "attempt was '{state}' when the runner stopped and no recovery identity was persisted; \
                     the process (if any) was not signalled"
                );
                return self.finish_lost(recovering, record, reason).await;
            }
        };

        let probe = match inner.executor.probe(&identity) {
            Ok(probe) => probe,
            Err(error) => Probe::Unverifiable(error.to_string()),
        };
        tracing::info!(job = %job_id, attempt = %attempt_id, ?probe, "recovery probe");
        match probe {
            Probe::Alive => match inner.executor.reattach(&identity, &recovering.ctx) {
                Ok(run) => {
                    let (handle, mut cancel, done) = self.register(&job_id, &attempt_id);
                    let task_inner = Arc::clone(inner);
                    let task_job = job_id.clone();
                    tokio::spawn(async move {
                        let Recovering {
                            claim,
                            ctx,
                            spec,
                            requirement,
                            number,
                        } = recovering;
                        let usage = claim.job.usage.clone();
                        let ended = supervise(
                            &task_inner,
                            &claim,
                            ctx,
                            record,
                            run,
                            requirement,
                            &mut cancel,
                        )
                        .await;
                        let result = conclude(
                            &task_inner,
                            &claim,
                            spec.as_ref(),
                            ended,
                            number,
                            &mut cancel,
                            usage,
                        )
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
                    self.finish_exited(recovering, record).await
                }
                Err(error) => {
                    let reason = format!("cannot reattach: {error}; the process was not signalled");
                    self.finish_lost(recovering, record, reason).await
                }
            },
            Probe::Exited => self.finish_exited(recovering, record).await,
            Probe::Mismatch(detail) => {
                let reason =
                    format!("recovery identity mismatch ({detail}); the process was not signalled");
                self.finish_lost(recovering, record, reason).await
            }
            Probe::Unverifiable(detail) => {
                let reason = format!(
                    "recovery identity cannot be verified ({detail}); the process was not signalled"
                );
                self.finish_lost(recovering, record, reason).await
            }
        }
    }

    /// The latest attempt sidecar was already terminal: the runner stopped
    /// after closing an attempt and before persisting the next one (or the
    /// sidecar was written by an older build that finalized the sidecar
    /// first). Continue with the retry the policy admits, else finalize the
    /// job record with the recorded outcome.
    async fn recover_terminal(
        &self,
        recovering: Recovering,
        record: AttemptRecord<E::Identity>,
    ) -> Result<RecoveredJob, RuntimeError> {
        let Recovering {
            claim,
            ctx,
            spec,
            number,
            ..
        } = recovering;
        let job_id = ctx.job_id.clone();
        let attempt_id = ctx.attempt_id.clone();
        let mut usage = claim.job.usage.clone();
        let delay = match &spec {
            Some(_) => retry_delay(
                &claim,
                record.state,
                record.reason.as_deref(),
                record.started_at,
                number,
                &mut usage,
            ),
            None => None,
        };
        if let (Some(spec), Some(delay)) = (spec, delay) {
            let handle = self.spawn_retry(
                claim,
                spec,
                Pending::Closed(record.state),
                &attempt_id,
                number,
                delay,
                usage,
            );
            return Ok(RecoveredJob {
                job_id,
                decision: RecoveryDecision::AlreadyFinal,
                handle: Some(handle),
            });
        }
        let outcome = job_outcome(&record);
        let result = finalize_record(&self.inner, &claim, outcome).await;
        let result = result.map(|()| result_of(&record, Output::default()));
        Ok(RecoveredJob {
            job_id: job_id.clone(),
            decision: RecoveryDecision::AlreadyFinal,
            handle: Some(Self::resolved(&job_id, &attempt_id, result)),
        })
    }

    async fn finish_exited(
        &self,
        recovering: Recovering,
        mut record: AttemptRecord<E::Identity>,
    ) -> Result<RecoveredJob, RuntimeError> {
        let Some(exit) = self.inner.executor.recorded_exit(&recovering.ctx) else {
            let reason =
                "process exited while the runner was down; its exit status is unknown".to_owned();
            return self.finish_lost(recovering, record, reason).await;
        };
        record.exit = Some(exit);
        let ending = Ending {
            exit,
            cause: None,
            lease_lost: None,
        };
        let (event, reason) = decide(&ending, recovering.requirement, record.sandbox.as_ref());
        let ended = AttemptEnd {
            ctx: recovering.ctx.clone(),
            record,
            output: Output::default(),
            event,
            reason,
            write_record: true,
        };
        self.conclude_recovered(recovering, ended, RecoveryDecision::ExitedWhileDown)
            .await
    }

    async fn finish_lost(
        &self,
        recovering: Recovering,
        record: AttemptRecord<E::Identity>,
        reason: String,
    ) -> Result<RecoveredJob, RuntimeError> {
        tracing::warn!(
            job = %recovering.ctx.job_id,
            attempt = %recovering.ctx.attempt_id,
            %reason,
            "recovered attempt is lost"
        );
        let ended = AttemptEnd {
            ctx: recovering.ctx.clone(),
            record,
            output: Output::default(),
            event: LifecycleEvent::LoseLease,
            reason: Some(reason.clone()),
            write_record: true,
        };
        self.conclude_recovered(recovering, ended, RecoveryDecision::Lost(reason))
            .await
    }

    /// Finalizes a recovered attempt that ended while the runner was down,
    /// or schedules the retry the policy admits.
    async fn conclude_recovered(
        &self,
        recovering: Recovering,
        ended: AttemptEnd<E::Identity>,
        decision: RecoveryDecision,
    ) -> Result<RecoveredJob, RuntimeError> {
        let Recovering {
            claim,
            spec,
            number,
            ..
        } = recovering;
        let job_id = ended.ctx.job_id.clone();
        let attempt_id = ended.ctx.attempt_id.clone();
        let mut usage = claim.job.usage.clone();
        let delay = match &spec {
            Some(_) => ended.retry_delay(&claim, number, &mut usage, false),
            None => None,
        };
        let (Some(spec), Some(delay)) = (spec, delay) else {
            let result = finalize(&self.inner, &claim, ended, number).await;
            return Ok(RecoveredJob {
                job_id: job_id.clone(),
                decision,
                handle: Some(Self::resolved(&job_id, &attempt_id, result)),
            });
        };
        let handle = self.spawn_retry(
            claim,
            spec,
            Pending::Open(Box::new(ended)),
            &attempt_id,
            number,
            delay,
            usage,
        );
        Ok(RecoveredJob {
            job_id,
            decision,
            handle: Some(handle),
        })
    }

    /// Runs the retry of attempt `number` (closing it first if it is still
    /// open) and every further attempt in a background task.
    #[allow(clippy::too_many_arguments)]
    fn spawn_retry(
        &self,
        claim: JobClaim,
        spec: JobSpec,
        pending: Pending<E::Identity>,
        attempt_id: &AttemptId,
        number: u32,
        delay: SignedDuration,
        usage: BudgetUsage,
    ) -> JobHandle {
        let job_id = claim.job.id.clone();
        let (handle, mut cancel, done) = self.register(&job_id, attempt_id);
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let closed = match pending {
                Pending::Open(ended) => close(&inner, &claim, *ended).await,
                Pending::Closed(state) => Ok(state),
            };
            let result = match closed {
                Ok(closed) => {
                    retry_then_conclude(
                        &inner,
                        &claim,
                        &spec,
                        closed,
                        number,
                        delay,
                        &mut cancel,
                        usage,
                    )
                    .await
                }
                Err(error) => Err(error),
            };
            inner.unregister(&job_id);
            let _ = done.send(result);
        });
        handle
    }
}

/// The job record's input for a class that does not keep the full spec.
fn redacted_input(spec: &JobSpec, persistence: Persistence) -> serde_json::Value {
    let class = match persistence {
        Persistence::Ephemeral => "ephemeral",
        Persistence::Durable | Persistence::MetadataOnly => "metadata_only",
    };
    match persistence {
        Persistence::MetadataOnly => serde_json::json!({
            "harw_redacted": {
                "class": class,
                "program": spec.program,
                "arg_count": spec.args.len(),
                "env_names": spec.env.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
            }
        }),
        _ => serde_json::json!({ "harw_redacted": { "class": class } }),
    }
}

impl<S, E> Inner<S, E> {
    fn unregister(&self, job: &WorkId) {
        self.handoffs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(job.as_str());
        self.outputs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(job.as_str());
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(job.as_str());
        // Ends every subscription of the job.
        if let Some(tap) = self
            .taps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(job.as_str())
        {
            tap.close();
        }
    }

    fn tap_for(&self, job: &WorkId) -> Option<FrameTap> {
        self.taps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(job.as_str())
            .cloned()
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
    let output = output.finish();
    JobResult {
        job_id: record.job_id.clone(),
        attempt_id: record.attempt_id.clone(),
        state: record.state,
        exit: record.exit,
        cancellation: record.cancellation.clone(),
        reason: record.reason.clone(),
        sandbox: record.sandbox,
        recovered: record.recovered,
        output_truncated: output.stdout_omitted > 0 || output.stderr_omitted > 0,
        stdout: output.stdout,
        stdout_omitted: output.stdout_omitted,
        stderr: output.stderr,
        stderr_omitted: output.stderr_omitted,
        output_capture: output.policy,
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

/// Prefix of every reason that records a policy refusal (sandbox
/// requirement not met). Such outcomes are deterministic: never retried.
const POLICY_PREFIX: &str = "policy:";

/// At most this many supervision notes of a recovered attempt are recorded
/// in its reason.
const MAX_RECOVERY_NOTES: usize = 4;

/// The attempt id of attempt `number` (1-based) of the claim with fencing
/// `epoch` on `job`: attempt 1 is [`attempt_id_for`] (`<job>-e<epoch>`),
/// every retry under the same claim is `<job>-e<epoch>-r<number>`.
///
/// # Errors
/// As [`attempt_id_for`].
pub(crate) fn retry_attempt_id(
    job: &WorkId,
    epoch: u64,
    number: u32,
) -> Result<AttemptId, RuntimeError> {
    if number <= 1 {
        return attempt_id_for(job, epoch);
    }
    let raw = format!("{}-e{epoch}-r{number}", job.as_str());
    AttemptId::new(raw.clone()).map_err(|error| RuntimeError::AttemptRecord {
        attempt: raw,
        detail: error.to_string(),
    })
}

/// How one attempt ended, before the job-level decision (retry or
/// finalize).
struct AttemptEnd<I> {
    ctx: AttemptContext,
    record: AttemptRecord<I>,
    output: Output,
    event: LifecycleEvent,
    reason: Option<String>,
    /// `false` once the lease is known to be lost: the job record must not
    /// be written (and nothing may be retried under that lease).
    write_record: bool,
}

impl<I> AttemptEnd<I> {
    /// The backoff before the next attempt, or `None` if this attempt
    /// decides the job's outcome.
    fn retry_delay(
        &self,
        claim: &JobClaim,
        attempt: u32,
        usage: &mut BudgetUsage,
        cancelled: bool,
    ) -> Option<SignedDuration> {
        if !self.write_record || cancelled {
            return None;
        }
        let state = self.record.state.transition(self.event).ok()?;
        retry_delay(
            claim,
            state,
            self.reason.as_deref(),
            self.record.started_at,
            attempt,
            usage,
        )
    }
}

/// An attempt a retry follows: still to be closed, or already terminal.
enum Pending<I> {
    Open(Box<AttemptEnd<I>>),
    Closed(LifecycleState),
}

/// What [`next_attempt`] led to.
enum Next<I> {
    /// The next attempt ran and ended.
    Ended(Box<AttemptEnd<I>>),
    /// The job ended before the next attempt ran (cancel, lease loss).
    Final(JobResult),
}

/// A recovered job's context.
struct Recovering {
    claim: JobClaim,
    ctx: AttemptContext,
    spec: Option<JobSpec>,
    requirement: SandboxRequirement,
    /// 1-based number of the recovered attempt.
    number: u32,
}

/// Whether attempt `attempt` (1-based), which ended in the terminal
/// `state`, is followed by another one; the backoff if so.
///
/// Retried are only retryable states (`Failed`, `TimedOut`, `Lost`; never
/// `Cancelled`) that are not policy refusals, while the job's retry policy
/// admits another attempt and the job's wall-time budget covers the
/// attempt's run time plus the backoff (charged onto `usage`).
fn retry_delay(
    claim: &JobClaim,
    state: LifecycleState,
    reason: Option<&str>,
    started_at: Option<Timestamp>,
    attempt: u32,
    usage: &mut BudgetUsage,
) -> Option<SignedDuration> {
    if !state.is_retryable() || reason.is_some_and(|reason| reason.starts_with(POLICY_PREFIX)) {
        return None;
    }
    let delay = claim.job.retry.next_delay(attempt).ok()?;
    let budget = &claim.job.budget;
    let ran = started_at.map_or(SignedDuration::ZERO, |started| {
        Timestamp::now()
            .duration_since(started)
            .max(SignedDuration::ZERO)
    });
    let charged = budget
        .charge_wall(usage, ran)
        .and_then(|()| budget.charge_wall(usage, delay));
    if let Err(error) = charged {
        tracing::info!(job = %claim.job.id, attempt, %error, "no retry: wall-time budget exhausted");
        return None;
    }
    Some(delay)
}

fn attempt_context<S, E>(
    inner: &Inner<S, E>,
    job_id: &WorkId,
    attempt_id: &AttemptId,
    lease_epoch: u64,
) -> AttemptContext {
    AttemptContext {
        job_id: job_id.clone(),
        attempt_id: attempt_id.clone(),
        runner_id: inner.config.runner_id.clone(),
        lease_epoch,
        workspace_root: inner.config.workspace_root.clone(),
        output_files: inner
            .outputs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(job_id.as_str())
            .cloned(),
        stdio_handoff: inner
            .handoffs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(job_id.as_str())
            .cloned(),
    }
}

/// The heartbeat period of a lease: TTL/3, at least 10 ms.
fn heartbeat_period(config: &CoordinatorConfig) -> Duration {
    (config.lease_ttl / 3).max(Duration::from_millis(10))
}

/// The latest persisted attempt of the claim with fencing `epoch`: its
/// 1-based number, id and sidecar bytes (`None` if not even the first
/// attempt wrote one).
async fn latest_attempt<S, E>(
    inner: &Arc<Inner<S, E>>,
    job_id: &WorkId,
    epoch: u64,
    max_attempts: u32,
) -> Result<(u32, AttemptId, Option<Vec<u8>>), RuntimeError>
where
    S: CoordinatorStore,
    E: Executor,
{
    let mut number = 1;
    let mut attempt_id = attempt_id_for(job_id, epoch)?;
    let key = attempt_id.to_string();
    let mut bytes = blocking(&inner.store, move |store| store.read_attempt(&key)).await?;
    if bytes.is_none() {
        return Ok((number, attempt_id, None));
    }
    for next in 2..=max_attempts {
        let candidate = retry_attempt_id(job_id, epoch, next)?;
        let key = candidate.to_string();
        match blocking(&inner.store, move |store| store.read_attempt(&key)).await? {
            Some(found) => {
                number = next;
                attempt_id = candidate;
                bytes = Some(found);
            }
            None => break,
        }
    }
    Ok((number, attempt_id, bytes))
}

/// Runs a freshly claimed job: its first attempt and every retry.
async fn run_job<S, E>(
    inner: &Arc<Inner<S, E>>,
    claim: JobClaim,
    attempt_id: AttemptId,
    spec: JobSpec,
    cancel: &mut watch::Receiver<bool>,
) -> Result<JobResult, RuntimeError>
where
    S: CoordinatorStore,
    E: Executor,
{
    let record = AttemptRecord::<E::Identity>::claimed(
        claim.job.id.clone(),
        attempt_id,
        inner.config.runner_id.clone(),
        claim.lease.epoch,
        Timestamp::now(),
    )?;
    let ended = run_attempt(inner, &claim, record, &spec, cancel).await?;
    let usage = claim.job.usage.clone();
    conclude(inner, &claim, Some(&spec), ended, 1, cancel, usage).await
}

/// Decides after attempt `attempt` ended: retry (as long as the policy
/// admits) or finalize the job with the attempt's outcome.
async fn conclude<S, E>(
    inner: &Arc<Inner<S, E>>,
    claim: &JobClaim,
    spec: Option<&JobSpec>,
    ended: AttemptEnd<E::Identity>,
    attempt: u32,
    cancel: &mut watch::Receiver<bool>,
    usage: BudgetUsage,
) -> Result<JobResult, RuntimeError>
where
    S: CoordinatorStore,
    E: Executor,
{
    let mut ended = ended;
    let mut attempt = attempt;
    let mut usage = usage;
    loop {
        let cancelled = *cancel.borrow();
        let delay = match spec {
            Some(_) => ended.retry_delay(claim, attempt, &mut usage, cancelled),
            None => None,
        };
        let (Some(spec), Some(delay)) = (spec, delay) else {
            return finalize(inner, claim, ended, attempt).await;
        };
        let closed = close(inner, claim, ended).await?;
        match next_attempt(inner, claim, spec, closed, attempt, delay, cancel).await? {
            Next::Ended(next) => {
                ended = *next;
                attempt = attempt.saturating_add(1);
            }
            Next::Final(result) => return Ok(result),
        }
    }
}

/// Starts the retry after the closed attempt `attempt`, then continues as
/// [`conclude`].
#[allow(clippy::too_many_arguments)]
async fn retry_then_conclude<S, E>(
    inner: &Arc<Inner<S, E>>,
    claim: &JobClaim,
    spec: &JobSpec,
    closed: LifecycleState,
    attempt: u32,
    delay: SignedDuration,
    cancel: &mut watch::Receiver<bool>,
    usage: BudgetUsage,
) -> Result<JobResult, RuntimeError>
where
    S: CoordinatorStore,
    E: Executor,
{
    match next_attempt(inner, claim, spec, closed, attempt, delay, cancel).await? {
        Next::Ended(ended) => {
            conclude(
                inner,
                claim,
                Some(spec),
                *ended,
                attempt.saturating_add(1),
                cancel,
                usage,
            )
            .await
        }
        Next::Final(result) => Ok(result),
    }
}

/// Closes an attempt that is followed by a retry: its sidecar records the
/// terminal state, the job record stays open. Returns the terminal state.
async fn close<S, E>(
    inner: &Arc<Inner<S, E>>,
    claim: &JobClaim,
    ended: AttemptEnd<E::Identity>,
) -> Result<LifecycleState, RuntimeError>
where
    S: CoordinatorStore,
    E: Executor,
{
    let AttemptEnd {
        ctx,
        record,
        output,
        event,
        reason,
        ..
    } = ended;
    finish(inner, claim, &ctx, record, output, event, reason, false)
        .await
        .map(|result| result.state)
}

/// Finalizes the job with the outcome of attempt `attempt`.
async fn finalize<S, E>(
    inner: &Arc<Inner<S, E>>,
    claim: &JobClaim,
    ended: AttemptEnd<E::Identity>,
    attempt: u32,
) -> Result<JobResult, RuntimeError>
where
    S: CoordinatorStore,
    E: Executor,
{
    let AttemptEnd {
        ctx,
        record,
        output,
        event,
        reason,
        write_record,
    } = ended;
    let reason = if attempt > 1 {
        reason.map(|reason| format!("{reason} (attempt {attempt})"))
    } else {
        reason
    };
    finish(
        inner,
        claim,
        &ctx,
        record,
        output,
        event,
        reason,
        write_record,
    )
    .await
}

/// Persists attempt `attempt + 1` (history: `Retry`, `Claim`), waits the
/// backoff while heartbeating, and runs it.
async fn next_attempt<S, E>(
    inner: &Arc<Inner<S, E>>,
    claim: &JobClaim,
    spec: &JobSpec,
    closed: LifecycleState,
    attempt: u32,
    delay: SignedDuration,
    cancel: &mut watch::Receiver<bool>,
) -> Result<Next<E::Identity>, RuntimeError>
where
    S: CoordinatorStore,
    E: Executor,
{
    let number = attempt.saturating_add(1);
    let attempt_id = retry_attempt_id(&claim.job.id, claim.lease.epoch, number)?;
    let retry = LifecycleTransition::apply(closed, LifecycleEvent::Retry)?;
    let mut record = AttemptRecord::<E::Identity>::claimed(
        claim.job.id.clone(),
        attempt_id,
        inner.config.runner_id.clone(),
        claim.lease.epoch,
        Timestamp::now(),
    )?;
    record.transitions.insert(0, retry);
    persist(inner, &record).await?;
    tracing::info!(
        job = %claim.job.id,
        attempt = %record.attempt_id,
        number,
        delay_ms = i64::try_from(delay.as_millis()).unwrap_or(i64::MAX),
        "retrying job"
    );
    let ctx = attempt_context(inner, &claim.job.id, &record.attempt_id, claim.lease.epoch);
    match backoff(inner, claim, delay, cancel).await {
        Backoff::Elapsed => {}
        Backoff::Cancelled => {
            record.cancellation = Some(CancellationCause::User);
            let reason = Some(CancellationCause::User.to_string());
            return finish(
                inner,
                claim,
                &ctx,
                record,
                Output::default(),
                LifecycleEvent::Cancel,
                reason,
                true,
            )
            .await
            .map(Next::Final);
        }
        Backoff::LeaseLost(detail) => {
            record.cancellation = Some(CancellationCause::LeaseLost);
            let reason = Some(format!("lease lost: {detail}"));
            return finish(
                inner,
                claim,
                &ctx,
                record,
                Output::default(),
                LifecycleEvent::LoseLease,
                reason,
                false,
            )
            .await
            .map(Next::Final);
        }
    }
    run_attempt(inner, claim, record, spec, cancel)
        .await
        .map(|ended| Next::Ended(Box::new(ended)))
}

/// How a retry backoff ended.
enum Backoff {
    Elapsed,
    Cancelled,
    LeaseLost(String),
}

/// Renews the lease of `claim` once.
async fn renew<S, E>(inner: &Arc<Inner<S, E>>, claim: &JobClaim) -> Result<(), RuntimeError>
where
    S: CoordinatorStore,
    E: Executor,
{
    let renew_claim = claim.clone();
    blocking(&inner.store, move |store| {
        store.transition(
            &renew_claim,
            JobTransition::Renew {
                now: Timestamp::now(),
            },
        )
    })
    .await
    .map(|_| ())
}

/// Waits `delay` while renewing the lease every TTL/3 and watching for
/// cancellation; at the end renews once more, so that no attempt starts
/// without a verified lease.
async fn backoff<S, E>(
    inner: &Arc<Inner<S, E>>,
    claim: &JobClaim,
    delay: SignedDuration,
    cancel: &mut watch::Receiver<bool>,
) -> Backoff
where
    S: CoordinatorStore,
    E: Executor,
{
    if *cancel.borrow_and_update() {
        return Backoff::Cancelled;
    }
    // `None` (unrepresentable instant): wait for cancellation or lease loss.
    let until = Instant::now().checked_add(delay.unsigned_abs());
    let period = heartbeat_period(&inner.config);
    let mut heartbeat = tokio::time::interval_at(Instant::now() + period, period);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut cancel_open = true;
    loop {
        tokio::select! {
            () = sleep_until(until) => break,
            _ = heartbeat.tick() => match renew(inner, claim).await {
                Ok(()) => {}
                Err(error) if error.is_lease_lost() => {
                    tracing::warn!(job = %claim.job.id, %error, "lease lost during retry backoff");
                    return Backoff::LeaseLost(error.to_string());
                }
                Err(error) => {
                    tracing::warn!(job = %claim.job.id, %error, "lease renewal failed; retrying");
                }
            },
            changed = cancel.changed(), if cancel_open => {
                if changed.is_err() {
                    cancel_open = false;
                } else if *cancel.borrow_and_update() {
                    return Backoff::Cancelled;
                }
            }
        }
    }
    match renew(inner, claim).await {
        Err(error) if error.is_lease_lost() => {
            tracing::warn!(job = %claim.job.id, %error, "lease lost before the retry");
            Backoff::LeaseLost(error.to_string())
        }
        Err(error) => {
            // Transient: the attempt's own heartbeat decides.
            tracing::warn!(job = %claim.job.id, %error, "lease renewal before the retry failed");
            Backoff::Elapsed
        }
        Ok(()) => Backoff::Elapsed,
    }
}

/// Drives one claimed attempt (`record` is `Claimed`) to its end.
async fn run_attempt<S, E>(
    inner: &Arc<Inner<S, E>>,
    claim: &JobClaim,
    record: AttemptRecord<E::Identity>,
    spec: &JobSpec,
    cancel: &mut watch::Receiver<bool>,
) -> Result<AttemptEnd<E::Identity>, RuntimeError>
where
    S: CoordinatorStore,
    E: Executor,
{
    let mut record = record;
    let ctx = attempt_context(inner, &claim.job.id, &record.attempt_id, claim.lease.epoch);
    record.apply(LifecycleEvent::Start, Timestamp::now())?;
    persist(inner, &record).await?;

    if *cancel.borrow_and_update() {
        record.cancellation = Some(CancellationCause::User);
        let reason = Some(CancellationCause::User.to_string());
        return Ok(AttemptEnd {
            ctx,
            record,
            output: Output::default(),
            event: LifecycleEvent::Cancel,
            reason,
            write_record: true,
        });
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

    let started = match inner.executor.start(spec, &ctx) {
        Ok(started) => started,
        Err(error) => {
            let reason = match &error {
                RuntimeError::SandboxRequirementNotMet { report, .. } => {
                    record.sandbox = Some(*report);
                    format!("{POLICY_PREFIX} {error}; the job body did not run")
                }
                _ => format!("start failed: {error}"),
            };
            tracing::warn!(job = %ctx.job_id, %error, "attempt start failed");
            return Ok(AttemptEnd {
                ctx,
                record,
                output: Output::default(),
                event: LifecycleEvent::Fail,
                reason: Some(reason),
                write_record: true,
            });
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
                "{POLICY_PREFIX} sandbox requirement not met (not fully enforced: {}); the job was killed before it counted as running",
                report.shortfalls().join(", ")
            );
            return Ok(AttemptEnd {
                ctx,
                record,
                output: Output::default(),
                event: LifecycleEvent::Fail,
                reason: Some(reason),
                write_record: true,
            });
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
    if let Some(tap) = inner.tap_for(&ctx.job_id) {
        tap.emit_with(|| JobFrame::AttemptStarted {
            attempt_id: ctx.attempt_id.clone(),
            pid: record.pid,
            sandbox: record.sandbox,
        });
    }
    Ok(supervise(inner, claim, ctx, record, run, spec.sandbox, cancel).await)
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
/// cancellation, lease loss; then reports how it ended.
///
/// For a recovered attempt, supervision notes of the executor
/// ([`AttemptEvent::Error`], e.g. a job cgroup that could not be reopened)
/// are recorded in the attempt's reason.
async fn supervise<S, E>(
    inner: &Arc<Inner<S, E>>,
    claim: &JobClaim,
    ctx: AttemptContext,
    mut record: AttemptRecord<E::Identity>,
    mut run: AttemptRun,
    requirement: SandboxRequirement,
    cancel: &mut watch::Receiver<bool>,
) -> AttemptEnd<E::Identity>
where
    S: CoordinatorStore,
    E: Executor,
{
    let mut output = Output::new(inner.config.output_capture);
    let tap = inner.tap_for(&ctx.job_id);
    let emit = |frame: &dyn Fn() -> JobFrame| {
        if let Some(tap) = &tap {
            tap.emit_with(frame);
        }
    };
    let period = heartbeat_period(&inner.config);
    let mut heartbeat = tokio::time::interval_at(Instant::now() + period, period);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let deadline_at = record.deadline.map(|deadline| {
        let remaining = deadline.remaining(Timestamp::now()).unsigned_abs();
        Instant::now() + remaining
    });
    let mut cause: Option<CancellationCause> = None;
    let mut lease_lost: Option<String> = None;
    let mut notes: Vec<String> = Vec::new();
    let mut cancel_open = true;
    if *cancel.borrow_and_update() {
        cause = Some(CancellationCause::User);
        run.cancel();
    }
    let (exit, late_report) = loop {
        let armed = cause.is_none();
        tokio::select! {
            event = run.next_event() => match event {
                Some(AttemptEvent::Stdout(chunk)) => {
                    emit(&|| JobFrame::Stdout(Arc::from(chunk.as_slice())));
                    output.stdout(&chunk);
                }
                Some(AttemptEvent::Stderr(chunk)) => {
                    emit(&|| JobFrame::Stderr(Arc::from(chunk.as_slice())));
                    output.stderr(&chunk);
                }
                Some(AttemptEvent::Error(message)) => {
                    tracing::warn!(job = %ctx.job_id, %message, "supervision error");
                    emit(&|| JobFrame::Note(message.clone()));
                    if record.recovered && notes.len() < MAX_RECOVERY_NOTES {
                        notes.push(message);
                    }
                }
                Some(AttemptEvent::Exited { outcome, sandbox }) => {
                    emit(&|| JobFrame::AttemptEnded { outcome, sandbox });
                    break (outcome, sandbox)
                }
                None => {
                    emit(&|| JobFrame::AttemptEnded {
                        outcome: ExitOutcome::Unknown,
                        sandbox: None,
                    });
                    break (ExitOutcome::Unknown, None)
                }
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
    let reason = if notes.is_empty() {
        reason
    } else {
        let notes = notes.join("; ");
        Some(match reason {
            Some(reason) => format!("{reason}; {notes}"),
            None => notes,
        })
    };
    let write_record = ending.lease_lost.is_none();
    AttemptEnd {
        ctx,
        record,
        output,
        event,
        reason,
        write_record,
    }
}

#[cfg(test)]
mod tests {
    use super::{Ending, decide, retry_attempt_id};
    use crate::test_support::{TestResult, ctx};
    use harw_job_core::{
        CancellationCause, EnforcementState, ExitOutcome, LifecycleEvent, SandboxReport,
        SandboxRequirement,
    };
    use harw_types::WorkId;

    #[test]
    fn retry_attempt_ids_extend_the_claim_attempt_id() -> TestResult {
        let job = WorkId::from_str("job-1");
        let first = retry_attempt_id(&job, 4, 1).map_err(ctx("first"))?;
        assert_eq!(first.as_str(), "job-1-e4");
        assert_eq!(
            retry_attempt_id(&job, 4, 0).map_err(ctx("zero"))?.as_str(),
            "job-1-e4"
        );
        let second = retry_attempt_id(&job, 4, 2).map_err(ctx("second"))?;
        assert_eq!(second.as_str(), "job-1-e4-r2");
        assert_ne!(first, second);
        Ok(())
    }

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
}
