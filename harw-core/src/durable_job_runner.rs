//! Durable job execution orchestration with lease heartbeat and commit outbox.
//!
//! [`JobStore`] owns the durable fence and [`JobExecutionRegistry`] owns the
//! in-process cancellation handle.  This module is the seam between them: a
//! claim is registered under the *complete* lease token, the operation runs
//! while a heartbeat renews the lease every `TTL / 3`, the outcome is
//! committed under the same token, and the binding is removed regardless of
//! completion outcome.  No worker name or session identifier is ever used as
//! a key.
//!
//! # Responsibility (A-JOBRUN, F-071 / G-021)
//! - **Heartbeat**: while the operation runs, [`JobStore::renew`] extends the
//!   lease every [`heartbeat_interval`] of the claim TTL, so a job running
//!   longer than its TTL still commits.
//! - **Lease loss**: a renewal rejected by the fence (token mismatch, expired,
//!   terminal, missing) — or a local lease deadline passing while renewals
//!   fail transiently — cancels the job [`CancelToken`] with
//!   [`CancelReason::LeaseLost`], asks the control to stop, and force-aborts
//!   after a bounded grace period.
//! - **Drop guard**: dropping the run future (task abort, shutdown) unregisters
//!   the live execution, cancels the job token with `LeaseLost` and stops the
//!   heartbeat; the unrenewed lease then lapses and `reconcile_expired`
//!   reschedules the job under its retry policy.
//! - **Blocking I/O**: every [`JobStore`] call runs in
//!   [`tokio::task::spawn_blocking`].
//! - **Outbox**: after a durable status commit, [`JobOutbox::on_committed`] is
//!   invoked exactly once for that commit.
//!
//! # Key types
//! [`DurableJobRunner`], [`DurableJobRunnerError`], [`JobOutbox`],
//! [`CommittedJob`], [`NoopJobOutbox`], [`heartbeat_interval`].
//!
//! # Concurrency
//! The runner is `Send + Sync` and may be shared behind an `Arc`; each
//! [`DurableJobRunner::run_with_cancel`] call drives exactly one job on the
//! calling task.  Heartbeat and commit are sequential on that task, so they
//! never contend for the same job lock inside one process.
//!
//! # Errors
//! [`DurableJobRunnerError`] (store, registry, lease loss, blocking-pool join).
//!
//! # Examples
//! ```rust,no_run
//! use std::sync::Arc;
//! use harw_core::durable_job_runner::{DurableJobRunner, NoopJobOutbox};
//! use harw_core::JobExecutionRegistry;
//! use harw_session_store::JobStore;
//!
//! let store = Arc::new(JobStore::new(std::path::Path::new("/tmp/harw")));
//! let runner = DurableJobRunner::new(store, Arc::new(JobExecutionRegistry::new()))
//!     .with_outbox(Arc::new(NoopJobOutbox));
//! # let _ = runner;
//! ```

use crate::cancel::{CancelReason, CancelToken};
use crate::execution_registry::{
    ExecutionControl, ExecutionGuard, ExecutionRegistryError, JobExecutionRegistry,
};
use harw_job_runtime::{JobClaim, JobCompletion, JobKind, JobOutcome, JobScope, Lease, LeaseToken};
use harw_session_store::{
    ClaimRequest, CompleteRequest, JobStore, RenewalRequest, SessionStoreError,
};
use harw_types::WorkId;
use jiff::{SignedDuration, Timestamp};
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

/// Grace period between a detected lease loss and a forced abort.
pub const DEFAULT_LEASE_LOST_GRACE: Duration = Duration::from_secs(5);

/// Lower bound of the heartbeat period, so tiny TTLs cannot spin.
pub const MIN_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(10);

// Bounded retries when an external process (operator cancel, reconciler)
// briefly holds the per-job lock during commit.
const COMPLETE_CONTENTION_RETRIES: u32 = 8;
const COMPLETE_CONTENTION_BACKOFF: Duration = Duration::from_millis(25);

/// Errors emitted by the durable runner boundary.
#[derive(Debug)]
pub enum DurableJobRunnerError {
    /// A durable store operation (claim, complete) failed.
    Store(SessionStoreError),
    /// The live execution registry rejected or failed a mutation.
    Registry(ExecutionRegistryError),
    /// The lease was lost while the job ran; nothing was committed by this
    /// runner. `cause` is the store error that proved the loss, if any.
    LeaseLost {
        work_id: WorkId,
        cause: Option<SessionStoreError>,
    },
    /// A blocking store task panicked or was cancelled by the runtime.
    Blocking {
        operation: &'static str,
        detail: String,
    },
}

impl fmt::Display for DurableJobRunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(f, "durable job store error: {error}"),
            Self::Registry(error) => write!(f, "live execution registry error: {error}"),
            Self::LeaseLost {
                work_id,
                cause: Some(cause),
            } => write!(f, "lease for job {work_id} was lost: {cause}"),
            Self::LeaseLost {
                work_id,
                cause: None,
            } => write!(f, "lease for job {work_id} was lost"),
            Self::Blocking { operation, detail } => {
                write!(f, "blocking job store task '{operation}' failed: {detail}")
            }
        }
    }
}

impl std::error::Error for DurableJobRunnerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Registry(error) => Some(error),
            Self::LeaseLost {
                cause: Some(cause), ..
            } => Some(cause),
            Self::LeaseLost { cause: None, .. } | Self::Blocking { .. } => None,
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

/// A durable status transition that has already been committed.
///
/// # Description
/// Passed to [`JobOutbox::on_committed`]. The record is already persisted,
/// so a consumer (plan bridge, MCP notification) can act on it without
/// becoming a second source of truth.
#[derive(Debug, Clone, PartialEq)]
pub struct CommittedJob {
    /// The committed job.
    pub work_id: WorkId,
    /// Job kind from the claim.
    pub kind: JobKind,
    /// Immutable authority boundary from the claim.
    pub scope: JobScope,
    /// The fencing token the commit was made under.
    pub token: LeaseToken,
    /// The persisted terminal (or blocked) completion.
    pub completion: JobCompletion,
}

/// Outbox hook invoked after a durable job status commit.
///
/// # Description
/// Called exactly once per successful commit made by [`DurableJobRunner`],
/// never for a rejected commit. It runs on the blocking pool, so a short
/// blocking write is acceptable; a panic is logged and does not change the
/// run result.
///
/// # Concurrency
/// Implementations must be `Send + Sync`; hooks of different jobs may run
/// concurrently.
pub trait JobOutbox: Send + Sync {
    /// Handles one committed status transition.
    fn on_committed(&self, committed: &CommittedJob);
}

/// Outbox that ignores every commit.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopJobOutbox;

impl JobOutbox for NoopJobOutbox {
    fn on_committed(&self, _committed: &CommittedJob) {}
}

/// Returns the heartbeat period for a lease TTL: `ttl / 3`.
///
/// # Arguments
/// - `lease_ttl` (`SignedDuration`): the TTL requested at claim time.
///
/// # Returns
/// A third of the TTL, never below [`MIN_HEARTBEAT_INTERVAL`] (non-positive
/// TTLs also yield the minimum).
///
/// # Examples
/// ```rust,no_run
/// use harw_core::durable_job_runner::heartbeat_interval;
/// let beat = heartbeat_interval(jiff::SignedDuration::from_secs(90));
/// assert_eq!(beat, std::time::Duration::from_secs(30));
/// ```
#[must_use]
pub fn heartbeat_interval(lease_ttl: SignedDuration) -> Duration {
    let third = lease_ttl.as_nanos() / 3;
    if third <= 0 {
        return MIN_HEARTBEAT_INTERVAL;
    }
    Duration::from_nanos(u64::try_from(third).unwrap_or(u64::MAX)).max(MIN_HEARTBEAT_INTERVAL)
}

/// A composable owner of durable job execution.
///
/// # Concurrency
/// `Send + Sync`; share with `Arc`. All store calls use `spawn_blocking`.
pub struct DurableJobRunner {
    store: Arc<JobStore>,
    executions: Arc<JobExecutionRegistry>,
    outbox: Arc<dyn JobOutbox>,
    cancel_root: CancelToken,
    lease_lost_grace: Duration,
}

impl DurableJobRunner {
    /// Creates a runner with a no-op outbox, a fresh cancel root and
    /// [`DEFAULT_LEASE_LOST_GRACE`].
    #[must_use]
    pub fn new(store: Arc<JobStore>, executions: Arc<JobExecutionRegistry>) -> Self {
        Self {
            store,
            executions,
            outbox: Arc::new(NoopJobOutbox),
            cancel_root: CancelToken::new(),
            lease_lost_grace: DEFAULT_LEASE_LOST_GRACE,
        }
    }

    /// Replaces the commit outbox hook.
    #[must_use]
    pub fn with_outbox(mut self, outbox: Arc<dyn JobOutbox>) -> Self {
        self.outbox = outbox;
        self
    }

    /// Sets the parent token; every job token is a child of it (e.g. a
    /// process shutdown token).
    #[must_use]
    pub fn with_cancel_root(mut self, cancel_root: CancelToken) -> Self {
        self.cancel_root = cancel_root;
        self
    }

    /// Sets the grace period between lease loss and forced abort.
    #[must_use]
    pub fn with_lease_lost_grace(mut self, grace: Duration) -> Self {
        self.lease_lost_grace = grace;
        self
    }

    /// Returns the durable job store.
    #[must_use]
    pub fn store(&self) -> &Arc<JobStore> {
        &self.store
    }

    /// Returns the live execution registry.
    #[must_use]
    pub fn executions(&self) -> &Arc<JobExecutionRegistry> {
        &self.executions
    }

    /// Returns the parent of all job cancel tokens.
    #[must_use]
    pub fn cancel_root(&self) -> &CancelToken {
        &self.cancel_root
    }

    /// Claims, registers, runs, completes, and unregisters one job.
    ///
    /// # Description
    /// Compatibility entry point for operations that do not observe a
    /// [`CancelToken`]; identical to [`DurableJobRunner::run_with_cancel`]
    /// (including heartbeat, lease-loss handling and outbox) with the token
    /// ignored by the operation.
    ///
    /// # Errors
    /// See [`DurableJobRunner::run_with_cancel`].
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
        self.run_with_cancel(work_id, request, move |claim, _token| operation(claim))
            .await
    }

    /// Claims, registers, runs with heartbeat, commits, and unregisters one job.
    ///
    /// # Description
    /// 1. Claims `work_id` (blocking pool).
    /// 2. Calls `operation` with the claim and a job [`CancelToken`] (child of
    ///    [`DurableJobRunner::cancel_root`]); it returns its
    ///    [`ExecutionControl`] and the outcome future.
    /// 3. Registers the control under the exact lease token via an
    ///    [`ExecutionGuard`]. Registry cancellation also cancels the job token
    ///    with [`CancelReason::User`].
    /// 4. Polls the future while renewing the lease every
    ///    [`heartbeat_interval`]. On lease loss the token is cancelled with
    ///    [`CancelReason::LeaseLost`] and, after the grace period, the control
    ///    is force-aborted and the future dropped.
    /// 5. Commits the outcome under the same token, then invokes
    ///    [`JobOutbox::on_committed`] once, then unregisters.
    ///
    /// If registration fails the claim is committed as `Failed` (and that
    /// commit is published to the outbox) so it never lingers as `Running`.
    ///
    /// # Arguments
    /// - `work_id` (`&WorkId`): the job to claim.
    /// - `request` (`&ClaimRequest`): worker id, TTL and claim instant.
    /// - `operation` (`F`): builds the control and outcome future.
    ///
    /// # Returns
    /// The persisted [`JobCompletion`].
    ///
    /// # Errors
    /// - [`DurableJobRunnerError::Store`]: claim or commit rejected (e.g.
    ///   `JobAlreadyTerminal` after an operator cancellation).
    /// - [`DurableJobRunnerError::Registry`]: registration or cleanup failed.
    /// - [`DurableJobRunnerError::LeaseLost`]: the heartbeat detected lease
    ///   loss and the outcome could not be committed.
    /// - [`DurableJobRunnerError::Blocking`]: a blocking store task failed.
    ///
    /// # Concurrency
    /// Drives the job on the calling task; the returned future is `Send` when
    /// `F` is `Send`. Dropping it releases the execution (see module docs).
    ///
    /// # Examples
    /// ```rust,no_run
    /// # async fn demo(runner: &harw_core::DurableJobRunner,
    /// #   work_id: &harw_types::WorkId,
    /// #   request: &harw_session_store::ClaimRequest,
    /// #   control: std::sync::Arc<dyn harw_core::ExecutionControl>) {
    /// let _ = runner
    ///     .run_with_cancel(work_id, request, move |_claim, token| {
    ///         (control, async move {
    ///             token.cancelled().await;
    ///             harw_job_runtime::JobOutcome::Cancelled { reason: "stopped".to_owned() }
    ///         })
    ///     })
    ///     .await;
    /// # }
    /// ```
    pub async fn run_with_cancel<F, Fut>(
        &self,
        work_id: &WorkId,
        request: &ClaimRequest,
        operation: F,
    ) -> Result<JobCompletion, DurableJobRunnerError>
    where
        F: FnOnce(JobClaim, CancelToken) -> (Arc<dyn ExecutionControl>, Fut),
        Fut: Future<Output = JobOutcome> + Send,
    {
        let claim = {
            let store = Arc::clone(&self.store);
            let work_id = work_id.clone();
            let request = request.clone();
            blocking("claim", move || store.claim(&work_id, &request)).await??
        };
        let lease_token: LeaseToken = claim.token.clone();
        let mut lease_expires_at = claim.lease.expires_at;
        let scope = claim.scope.clone();
        let kind = claim.job.kind.clone();
        info!(
            work_id = %work_id,
            epoch = lease_token.epoch,
            ttl = %request.lease_ttl,
            "durable job claimed"
        );

        let job_token = self.cancel_root.child();
        let (control, operation) = operation(claim, job_token.clone());
        let linked: Arc<dyn ExecutionControl> = Arc::new(CancelLinkedControl {
            inner: Arc::clone(&control),
            token: job_token.clone(),
        });

        let execution = match self
            .executions
            .register_guarded(lease_token.clone(), linked)
        {
            Ok(guard) => guard,
            Err(registry_error) => {
                // A registration failure means this claim cannot safely run.
                // It is terminally failed under the same fence rather than
                // left as a durable zombie in Running state.
                let failed = JobOutcome::Failed {
                    reason: "execution registration rejected".to_owned(),
                };
                if let Err(commit_error) = self
                    .commit(work_id, &lease_token, failed, &scope, &kind)
                    .await
                {
                    warn!(
                        work_id = %work_id,
                        error = %commit_error,
                        "could not fail a claim whose registration was rejected"
                    );
                }
                return Err(registry_error.into());
            }
        };
        let guard = RunGuard {
            execution: Some(execution),
            token: job_token.clone(),
            control: Arc::clone(&control),
            finished: false,
        };

        let interval = heartbeat_interval(request.lease_ttl);
        let mut operation = std::pin::pin!(operation);
        let mut next_beat = tokio::time::Instant::now() + interval;
        let mut lost: Option<LeaseLoss> = None;

        let outcome = loop {
            let wake_at = lost.as_ref().map_or(next_beat, |loss| loss.deadline);
            tokio::select! {
                biased;
                outcome = &mut operation => break Some(outcome),
                () = tokio::time::sleep_until(wake_at) => {
                    if lost.is_some() {
                        warn!(work_id = %work_id, "operation ignored lease loss; force-aborting");
                        control.force_abort();
                        break None;
                    }
                    match self.renew(&lease_token).await {
                        RenewOutcome::Renewed(lease) => {
                            lease_expires_at = lease.expires_at;
                            next_beat = tokio::time::Instant::now() + interval;
                            debug!(work_id = %work_id, expires_at = %lease.expires_at, "lease renewed");
                        }
                        RenewOutcome::Transient(cause) => {
                            if Timestamp::now() >= lease_expires_at {
                                lost = Some(self.lose_lease(work_id, &job_token, &control, cause));
                            } else {
                                warn!(
                                    work_id = %work_id,
                                    error = ?cause.as_ref().map(ToString::to_string),
                                    "transient lease renewal failure; retrying"
                                );
                                next_beat = tokio::time::Instant::now()
                                    + (interval / 4).max(MIN_HEARTBEAT_INTERVAL);
                            }
                        }
                        RenewOutcome::Lost(cause) => {
                            lost = Some(self.lose_lease(work_id, &job_token, &control, Some(cause)));
                        }
                    }
                }
            }
        };

        let Some(outcome) = outcome else {
            if let Err(registry_error) = guard.finish() {
                warn!(work_id = %work_id, error = %registry_error, "unregister after lease loss failed");
            }
            return Err(DurableJobRunnerError::LeaseLost {
                work_id: work_id.clone(),
                cause: lost.and_then(|loss| loss.cause),
            });
        };

        let commit = self
            .commit(work_id, &lease_token, outcome, &scope, &kind)
            .await;
        let release = guard.finish();

        // Always surface the durable result first: a stale completion is the
        // security-relevant outcome.  Registry cleanup errors are returned
        // only when the durable transition itself succeeded.
        match (commit, release) {
            (Err(DurableJobRunnerError::Store(cause)), _) if lost.is_some() => {
                Err(DurableJobRunnerError::LeaseLost {
                    work_id: work_id.clone(),
                    cause: Some(cause),
                })
            }
            (Err(commit_error), _) => Err(commit_error),
            (Ok(_completion), Err(registry_error)) => Err(registry_error.into()),
            (Ok(completion), Ok(_)) => Ok(completion),
        }
    }

    // Renews the lease on the blocking pool and classifies the result.
    async fn renew(&self, token: &LeaseToken) -> RenewOutcome {
        let store = Arc::clone(&self.store);
        let request = RenewalRequest {
            token: token.clone(),
            now: Timestamp::now(),
        };
        match blocking("renew", move || store.renew(&request)).await {
            Ok(Ok(lease)) => RenewOutcome::Renewed(lease),
            Ok(Err(cause)) if is_lease_lost(&cause) => RenewOutcome::Lost(cause),
            Ok(Err(cause)) => RenewOutcome::Transient(Some(cause)),
            Err(join_error) => {
                warn!(token_epoch = token.epoch, error = %join_error, "renew task failed");
                RenewOutcome::Transient(None)
            }
        }
    }

    // Cancels the job token with LeaseLost and asks the child to stop.
    fn lose_lease(
        &self,
        work_id: &WorkId,
        job_token: &CancelToken,
        control: &Arc<dyn ExecutionControl>,
        cause: Option<SessionStoreError>,
    ) -> LeaseLoss {
        error!(
            work_id = %work_id,
            error = ?cause.as_ref().map(ToString::to_string),
            "job lease lost; cancelling execution"
        );
        job_token.cancel(CancelReason::LeaseLost);
        control.request_graceful_cancel();
        LeaseLoss {
            deadline: tokio::time::Instant::now() + self.lease_lost_grace,
            cause,
        }
    }

    // Commits under the fence (bounded retry on lock contention), then
    // publishes the commit to the outbox exactly once.
    async fn commit(
        &self,
        work_id: &WorkId,
        token: &LeaseToken,
        outcome: JobOutcome,
        scope: &JobScope,
        kind: &JobKind,
    ) -> Result<JobCompletion, DurableJobRunnerError> {
        let mut attempt: u32 = 0;
        let completion = loop {
            let store = Arc::clone(&self.store);
            let target = work_id.clone();
            let request = CompleteRequest {
                token: token.clone(),
                completed_at: Timestamp::now(),
                outcome: outcome.clone(),
            };
            match blocking("complete", move || store.complete(&target, &request)).await? {
                Ok(completion) => break completion,
                Err(SessionStoreError::JobLockContended { .. })
                    if attempt < COMPLETE_CONTENTION_RETRIES =>
                {
                    attempt += 1;
                    tokio::time::sleep(COMPLETE_CONTENTION_BACKOFF).await;
                }
                Err(cause) => return Err(cause.into()),
            }
        };
        info!(work_id = %work_id, "durable job outcome committed");

        let committed = CommittedJob {
            work_id: work_id.clone(),
            kind: kind.clone(),
            scope: scope.clone(),
            token: token.clone(),
            completion: completion.clone(),
        };
        let outbox = Arc::clone(&self.outbox);
        if let Err(join_error) =
            tokio::task::spawn_blocking(move || outbox.on_committed(&committed)).await
        {
            warn!(work_id = %work_id, error = %join_error, "job outbox hook failed after commit");
        }
        Ok(completion)
    }
}

// Runs one blocking store call on the blocking pool.
async fn blocking<T, Op>(operation: &'static str, call: Op) -> Result<T, DurableJobRunnerError>
where
    Op: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(call)
        .await
        .map_err(|join_error| DurableJobRunnerError::Blocking {
            operation,
            detail: join_error.to_string(),
        })
}

// Store errors that prove this runner no longer holds the fence.
fn is_lease_lost(cause: &SessionStoreError) -> bool {
    matches!(
        cause,
        SessionStoreError::LeaseTokenMismatch { .. }
            | SessionStoreError::JobLeaseExpired { .. }
            | SessionStoreError::JobAlreadyTerminal { .. }
            | SessionStoreError::JobNotFound { .. }
            | SessionStoreError::JobRuntime { .. }
    )
}

// Classified heartbeat result.
enum RenewOutcome {
    Renewed(Lease),
    Transient(Option<SessionStoreError>),
    Lost(SessionStoreError),
}

// Detected lease loss awaiting cooperative stop.
struct LeaseLoss {
    deadline: tokio::time::Instant,
    cause: Option<SessionStoreError>,
}

// Control registered in the registry: external cancellation also cancels the
// job token (reason User) before delegating to the operation's control.
struct CancelLinkedControl {
    inner: Arc<dyn ExecutionControl>,
    token: CancelToken,
}

impl ExecutionControl for CancelLinkedControl {
    fn request_graceful_cancel(&self) {
        self.token.cancel(CancelReason::User);
        self.inner.request_graceful_cancel();
    }

    fn force_abort(&self) {
        self.token.cancel(CancelReason::User);
        self.inner.force_abort();
    }

    fn completion(&self) -> watch::Receiver<bool> {
        self.inner.completion()
    }
}

// Drop guard for one run: an unfinished drop (future aborted) cancels the job
// token with LeaseLost, asks the child to stop, and unregisters via the
// execution guard. The heartbeat stops with the dropped future.
struct RunGuard {
    execution: Option<ExecutionGuard>,
    token: CancelToken,
    control: Arc<dyn ExecutionControl>,
    finished: bool,
}

impl RunGuard {
    fn finish(mut self) -> Result<bool, ExecutionRegistryError> {
        self.finished = true;
        match self.execution.take() {
            Some(execution) => execution.release(),
            None => Ok(false),
        }
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let work_id = self
            .execution
            .as_ref()
            .map(|execution| execution.token().work_id.to_string());
        warn!(
            work_id = ?work_id,
            "durable run dropped before commit; releasing execution, lease will lapse"
        );
        self.token.cancel(CancelReason::LeaseLost);
        self.control.request_graceful_cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_registry::ExecutionControl;
    use crate::test_support::{TestResult, ctx};
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

    fn record(id: &str) -> TestResult<StoredJob> {
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
        job.mark_ready(now).map_err(ctx("new job is pending"))?;
        Ok(StoredJob {
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
        })
    }

    fn runner(
        id: &str,
    ) -> TestResult<(tempfile::TempDir, Arc<JobStore>, DurableJobRunner, WorkId)> {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(temp.path()));
        let work_id = WorkId::from_str(id);
        store.admit(&record(id)?).map_err(ctx("admit record"))?;
        let executions = Arc::new(JobExecutionRegistry::new());
        let runner = DurableJobRunner::new(Arc::clone(&store), executions);
        Ok((temp, store, runner, work_id))
    }

    fn request(now: Timestamp) -> ClaimRequest {
        ClaimRequest {
            worker_id: "runner".to_owned(),
            lease_ttl: SignedDuration::from_secs(60),
            now,
        }
    }

    #[test]
    fn test_heartbeat_interval_is_a_third_of_ttl_with_floor() {
        assert_eq!(
            heartbeat_interval(SignedDuration::from_secs(90)),
            Duration::from_secs(30)
        );
        assert_eq!(
            heartbeat_interval(SignedDuration::from_nanos(3)),
            MIN_HEARTBEAT_INTERVAL
        );
        assert_eq!(
            heartbeat_interval(SignedDuration::from_secs(-5)),
            MIN_HEARTBEAT_INTERVAL
        );
    }

    #[test]
    fn test_is_lease_lost_classifies_fence_errors() {
        assert!(is_lease_lost(&SessionStoreError::LeaseTokenMismatch {
            work_id: WorkId::from_str("w"),
        }));
        assert!(!is_lease_lost(&SessionStoreError::JobLockContended {
            work_id: WorkId::from_str("w"),
        }));
    }

    #[tokio::test]
    async fn happy_completion_unregisters_exact_execution() -> TestResult {
        let (_temp, store, runner, work_id) = runner("work-happy")?;
        let control = FakeControl::new();
        let expected = Arc::clone(&control);
        let completion = runner
            .run(&work_id, &request(Timestamp::now()), move |_claim| {
                let control_for_operation = Arc::clone(&control);
                (control, async move {
                    control_for_operation.completion.send_replace(true);
                    JobOutcome::Succeeded {
                        result: serde_json::json!({"ok": true}),
                    }
                })
            })
            .await
            .map_err(ctx("run succeeds"))?;
        assert!(matches!(completion.outcome, JobOutcome::Succeeded { .. }));
        assert_eq!(expected.graceful.load(Ordering::SeqCst), 0);
        assert!(runner.executions().is_empty());
        assert_eq!(
            store.get(&work_id).map_err(ctx("record"))?.job.state,
            harw_job_runtime::JobState::Completed
        );
        Ok(())
    }

    #[tokio::test]
    async fn failed_outcome_still_unregisters_execution() -> TestResult {
        let (_temp, store, runner, work_id) = runner("work-failed")?;
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
            .map_err(ctx("durable failure is a completion"))?;
        assert!(matches!(completion.outcome, JobOutcome::Failed { .. }));
        assert!(runner.executions().is_empty());
        assert_eq!(
            store.get(&work_id).map_err(ctx("record"))?.job.state,
            harw_job_runtime::JobState::Failed
        );
        Ok(())
    }

    #[tokio::test]
    async fn durable_cancellation_fences_runner_before_completion() -> TestResult {
        let (_temp, store, _unused_runner, work_id) = runner("work-cancel")?;
        let executions = Arc::new(JobExecutionRegistry::new());
        let runner = DurableJobRunner::new(Arc::clone(&store), Arc::clone(&executions));
        let control = FakeControl::new();
        let operation_control = Arc::clone(&control);
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
        let token = claimed_rx.await.map_err(ctx("claim reached operation"))?;
        // The operation is built before registration; wait for the binding.
        for _ in 0..200 {
            if executions.contains(&token) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
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
            .map_err(ctx("durable cancellation"))?;
        assert_eq!(
            transition.prior_lease.as_ref().map(|lease| lease.token()),
            Some(token.clone())
        );
        assert_eq!(
            executions
                .cancel(&token, std::time::Duration::from_secs(1))
                .await
                .map_err(ctx("live cancellation"))?,
            crate::execution_registry::CancellationResult::Graceful
        );
        assert_eq!(control.graceful.load(Ordering::SeqCst), 1);
        let result = run.await.map_err(ctx("runner task"))?;
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
            store.get(&work_id).map_err(ctx("record"))?.job.state,
            harw_job_runtime::JobState::Cancelled
        ));
        Ok(())
    }
}
