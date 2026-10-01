//! Closed, server-authoritative durable-job admission.
//!
//! `JobIntent` is deliberately smaller than `StoredJob`: callers can name a
//! configured workspace and provide task data, but cannot provide identity,
//! scheduling, resource, sandbox, or credential authority.  Those values are
//! resolved by the trusted policy implementation at this boundary.
//!
//! # Responsibility (A-JOBRUN, F-158)
//! [`JobAdmissionService::submit`] is the single submission path for durable
//! jobs. In order it enforces:
//! 1. **Scope**: workspace alias grammar, tenant-bound workspace resolution,
//!    reserved task fields, policy resolution, sandbox binding.
//! 2. **Budget**: an optional requested budget may only tighten the policy
//!    ceiling; exceeding it is rejected, never silently capped.
//! 3. **Idempotency**: with an [`IdempotencyKey`] the work id is derived from
//!    `(tenant, workspace, submitter, key)` via BLAKE3; a repeat submission
//!    returns the existing job ([`AdmissionDisposition::Duplicate`]) and a
//!    repeat with a different task or kind is an
//!    [`JobAdmissionError::IdempotencyConflict`].
//! 4. **Rate limit**: a sliding window per `(tenant, submitter)`; duplicates
//!    are not charged.
//!
//! Admitted jobs are persisted `Ready`, i.e. immediately claimable.
//!
//! # Capacity queue (opt-in)
//! [`JobAdmissionService::submit_queued_async`] is an additive, opt-in
//! alternative to [`JobAdmissionService::submit_async`] for callers that
//! would rather wait for rate-limit capacity than fail outright (e.g. a
//! `UserInterface`-role delegation). It never changes the rate limiter
//! itself — [`JobAdmissionService::submit`]/`submit_async`/`admit` keep
//! rejecting exactly as before — it only adds a background retry loop that
//! is woken early via [`tokio::sync::Notify`] whenever a slot frees up, or
//! otherwise sleeps out the estimated retry delay, until it succeeds, a
//! non-capacity error occurs, or a caller-supplied `max_wait` elapses. See
//! [`QueuedAdmission`].
//!
//! # Concurrency
//! The service is `Send + Sync` when `P` is. The rate limiter uses one
//! `std::sync::Mutex` held only for window bookkeeping. [`JobAdmissionService::submit`]
//! performs blocking store I/O; async callers use
//! [`JobAdmissionService::submit_async`], which runs it on `spawn_blocking`.
//! [`JobAdmissionService::submit_queued_async`] additionally spawns at most
//! one background `tokio::task` per call, holding only an `Arc` clone of the
//! service; the caller-side `oneshot::Receiver` may be dropped without
//! cancelling that task.
//!
//! # Errors
//! [`JobAdmissionError`].
//!
//! # Examples
//! ```rust,no_run
//! # fn demo<P: harw_core::JobAdmissionPolicy>(
//! #     service: &harw_core::JobAdmissionService<P>,
//! #     context: &harw_core::AdmissionContext,
//! # ) -> Result<(), harw_core::JobAdmissionError> {
//! use harw_core::admission::{IdempotencyKey, SubmitOptions};
//! let options = SubmitOptions {
//!     idempotency_key: Some(IdempotencyKey::parse("plan-7-node-3")?),
//!     requested_budget: None,
//! };
//! let intent = harw_core::JobIntent { workspace: "safe".into(), task: serde_json::json!({}) };
//! let outcome = service.submit(intent, &options, context, jiff::Timestamp::now())?;
//! # let _ = outcome;
//! # Ok(())
//! # }
//! ```

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};

use harw_authority::{AuthorityError, SandboxSpec, WorkspaceBinding, WorkspaceRegistry};
use harw_job_core::{
    Budget, BudgetKind, Job, JobKind, JobRuntimeError, JobScope, RetryPolicy, StoredJob,
};
use harw_sandbox::SandboxError;
use harw_session_store::{JobStore, SessionStoreError};
use harw_types::{ApprovalActor, ContentDigest, TenantId, WorkId, WorkspaceId};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, oneshot};
use tracing::{debug, info, warn};

/// Maximum length of an [`IdempotencyKey`] in bytes.
pub const IDEMPOTENCY_KEY_MAX_LEN: usize = 128;

/// Prefix of work ids derived from an idempotency key.
pub const IDEMPOTENT_WORK_ID_PREFIX: &str = "idem-";

// Domain separator of the idempotency derivation; bump on format change.
const IDEMPOTENCY_DOMAIN: &[u8] = b"harw-core/job-admission/idempotency/v1";

// Upper bound of tracked submitters before idle windows are swept.
const LIMITER_SWEEP_THRESHOLD: usize = 4096;

/// Untrusted request accepted at the admission edge. Unknown fields are
/// rejected so authority-bearing wire fields cannot be silently ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobIntent {
    pub workspace: String,
    pub task: Value,
}

/// Trusted context supplied by an authenticated ingress resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionContext {
    tenant: TenantId,
    submitter: ApprovalActor,
}

impl AdmissionContext {
    /// Creates a context from an authenticated tenant and submitter.
    #[must_use]
    pub fn new(tenant: TenantId, submitter: ApprovalActor) -> Self {
        Self { tenant, submitter }
    }

    /// Returns the authenticated tenant.
    #[must_use]
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    /// Returns the authenticated submitter.
    #[must_use]
    pub fn submitter(&self) -> &ApprovalActor {
        &self.submitter
    }
}

/// Server-resolved policy. No field can be supplied by [`JobIntent`].
///
/// `budget` is the **ceiling**: a requested budget may only tighten it.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAdmission {
    pub kind: JobKind,
    pub budget: Budget,
    pub retry: RetryPolicy,
    pub sandbox: SandboxSpec,
}

/// Policy/catalog/sandbox resolver owned by the trusted composition layer.
pub trait JobAdmissionPolicy: Send + Sync {
    /// Resolves kind, budget ceiling, retry and sandbox for one submission.
    ///
    /// # Errors
    /// Implementations return [`JobAdmissionError::PolicyRejected`] to refuse.
    fn resolve(
        &self,
        context: &AdmissionContext,
        workspace: &WorkspaceBinding,
        task: &Value,
    ) -> Result<ResolvedAdmission, JobAdmissionError>;
}

/// Client-chosen deduplication key for one logical submission.
///
/// # Description
/// 1 to [`IDEMPOTENCY_KEY_MAX_LEN`] bytes of `[A-Za-z0-9._:-]`. The key is
/// scoped by tenant, workspace and submitter, so equal keys of different
/// submitters never meet.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Validates and wraps a raw key.
    ///
    /// # Errors
    /// - [`JobAdmissionError::InvalidIdempotencyKey`]: empty, too long, or a
    ///   byte outside `[A-Za-z0-9._:-]`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_core::admission::IdempotencyKey;
    /// assert!(IdempotencyKey::parse("node-1").is_ok());
    /// assert!(IdempotencyKey::parse("a b").is_err());
    /// ```
    pub fn parse(raw: &str) -> Result<Self, JobAdmissionError> {
        if raw.is_empty()
            || raw.len() > IDEMPOTENCY_KEY_MAX_LEN
            || !raw
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        {
            return Err(JobAdmissionError::InvalidIdempotencyKey { length: raw.len() });
        }
        Ok(Self(raw.to_owned()))
    }

    /// Returns the validated key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Per-submitter sliding-window submission limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionLimits {
    max_submissions: u32,
    window: SignedDuration,
}

impl AdmissionLimits {
    /// Default number of admitted submissions per window.
    pub const DEFAULT_MAX_SUBMISSIONS: u32 = 30;
    /// Default window length in seconds.
    pub const DEFAULT_WINDOW_SECONDS: i64 = 60;

    /// Creates limits; `None` when `max_submissions == 0` or `window <= 0`.
    #[must_use]
    pub fn new(max_submissions: u32, window: SignedDuration) -> Option<Self> {
        (max_submissions > 0 && window > SignedDuration::ZERO).then_some(Self {
            max_submissions,
            window,
        })
    }

    /// Returns the maximum admissions per window.
    #[must_use]
    pub fn max_submissions(&self) -> u32 {
        self.max_submissions
    }

    /// Returns the window length.
    #[must_use]
    pub fn window(&self) -> SignedDuration {
        self.window
    }
}

impl Default for AdmissionLimits {
    fn default() -> Self {
        Self {
            max_submissions: Self::DEFAULT_MAX_SUBMISSIONS,
            window: SignedDuration::from_secs(Self::DEFAULT_WINDOW_SECONDS),
        }
    }
}

/// Optional, non-authoritative submission parameters.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SubmitOptions {
    /// Deduplication key; `None` admits a fresh job every time.
    pub idempotency_key: Option<IdempotencyKey>,
    /// Requested budget; each field must not exceed the policy ceiling.
    /// `None` (or a `None` field) takes the ceiling.
    pub requested_budget: Option<Budget>,
}

/// Whether a submission created a job or matched an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionDisposition {
    /// A new `Ready` job was persisted.
    Admitted,
    /// The idempotency key matched an existing, equivalent job.
    Duplicate,
}

/// Result of [`JobAdmissionService::submit`].
#[derive(Debug, Clone, PartialEq)]
pub struct AdmissionOutcome {
    /// The persisted record (for a duplicate: its current durable state).
    pub record: StoredJob,
    /// Admitted or duplicate.
    pub disposition: AdmissionDisposition,
}

/// Derives the deterministic work id for an idempotent submission.
///
/// # Description
/// `idem-` followed by the hex BLAKE3 digest of a domain separator and the
/// length-prefixed tenant, workspace, submitter and key. Length prefixes make
/// the encoding injective, so distinct scopes cannot collide by concatenation.
///
/// # Returns
/// A filesystem-safe [`WorkId`] of 69 bytes.
#[must_use]
pub fn idempotent_work_id(
    tenant: &TenantId,
    workspace: &WorkspaceId,
    submitter: &ApprovalActor,
    key: &IdempotencyKey,
) -> WorkId {
    let mut material = Vec::with_capacity(256);
    push_field(&mut material, IDEMPOTENCY_DOMAIN);
    push_field(&mut material, tenant.as_str().as_bytes());
    push_field(&mut material, workspace.as_str().as_bytes());
    match submitter {
        ApprovalActor::Operator { id } => {
            push_field(&mut material, b"operator");
            push_field(&mut material, id.as_bytes());
        }
        ApprovalActor::ChannelPeer { channel, peer } => {
            push_field(&mut material, b"channel_peer");
            push_field(&mut material, channel.as_str().as_bytes());
            push_field(&mut material, peer.as_str().as_bytes());
        }
    }
    push_field(&mut material, key.as_str().as_bytes());
    WorkId::from_str(format!(
        "{IDEMPOTENT_WORK_ID_PREFIX}{}",
        ContentDigest::of(&material)
    ))
}

// Appends one length-prefixed field.
fn push_field(material: &mut Vec<u8>, bytes: &[u8]) {
    material.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    material.extend_from_slice(bytes);
}

/// Durable admission facade and the only supported job submission path.
pub struct JobAdmissionService<P> {
    store: Arc<JobStore>,
    workspaces: Arc<WorkspaceRegistry>,
    policy: P,
    limiter: SubmissionLimiter,
}

impl<P: JobAdmissionPolicy> JobAdmissionService<P> {
    /// Creates a service with [`AdmissionLimits::default`].
    #[must_use]
    pub fn new(store: Arc<JobStore>, workspaces: Arc<WorkspaceRegistry>, policy: P) -> Self {
        Self {
            store,
            workspaces,
            policy,
            limiter: SubmissionLimiter::new(AdmissionLimits::default()),
        }
    }

    /// Replaces the per-submitter rate limit (resets all windows).
    #[must_use]
    pub fn with_limits(mut self, limits: AdmissionLimits) -> Self {
        self.limiter = SubmissionLimiter::new(limits);
        self
    }

    /// Returns the active rate limit.
    #[must_use]
    pub fn limits(&self) -> AdmissionLimits {
        self.limiter.limits
    }

    /// Returns the durable job store.
    #[must_use]
    pub fn store(&self) -> &Arc<JobStore> {
        &self.store
    }

    /// Validates, resolves, deduplicates, rate-limits and durably admits one job.
    ///
    /// # Description
    /// See the module docs for the exact order. The job is persisted `Ready`.
    /// A duplicate is detected before the rate limiter is charged and again
    /// on a lost admission race (`JobAlreadyExists`).
    ///
    /// # Arguments
    /// - `intent` (`JobIntent`): untrusted workspace alias and task, moved in.
    /// - `options` (`&SubmitOptions`): idempotency key and requested budget.
    /// - `context` (`&AdmissionContext`): authenticated tenant and submitter.
    /// - `now` (`Timestamp`): server clock for submission and rate window.
    ///
    /// # Returns
    /// [`AdmissionOutcome`] with the persisted record and disposition.
    ///
    /// # Errors
    /// - [`JobAdmissionError::InvalidWorkspaceAlias`], [`JobAdmissionError::ReservedTaskField`],
    ///   [`JobAdmissionError::TaskTooDeep`], [`JobAdmissionError::Sandbox`],
    ///   [`JobAdmissionError::SandboxBindingMismatch`], [`JobAdmissionError::PolicyRejected`]: scope.
    /// - [`JobAdmissionError::BudgetExceedsCeiling`], [`JobAdmissionError::InvalidBudget`]: budget.
    /// - [`JobAdmissionError::IdempotencyConflict`]: key reused for another task.
    /// - [`JobAdmissionError::RateLimited`], [`JobAdmissionError::LimiterUnavailable`]: rate limit.
    /// - [`JobAdmissionError::JobRuntime`], [`JobAdmissionError::Store`]: persistence.
    ///
    /// # Concurrency
    /// Blocking (filesystem I/O). Safe from many threads; use
    /// [`JobAdmissionService::submit_async`] from async code.
    pub fn submit(
        &self,
        intent: JobIntent,
        options: &SubmitOptions,
        context: &AdmissionContext,
        now: Timestamp,
    ) -> Result<AdmissionOutcome, JobAdmissionError> {
        let workspace_id = validate_workspace_alias(&intent.workspace)?;
        let binding = self.workspaces.resolve(context.tenant(), &workspace_id)?;
        let task = sanitize_task(intent.task)?;
        let resolved = self.policy.resolve(context, &binding, &task)?;
        if resolved.sandbox.workspace() != &binding {
            return Err(JobAdmissionError::SandboxBindingMismatch);
        }
        let budget = effective_budget(&resolved.budget, options.requested_budget.as_ref())?;
        let scope = JobScope::new(
            context.tenant().clone(),
            workspace_id,
            context.submitter().clone(),
        );

        let id = match &options.idempotency_key {
            Some(key) => {
                let id =
                    idempotent_work_id(scope.tenant(), scope.workspace(), scope.submitter(), key);
                match self.store.get(&id) {
                    Ok(existing) => {
                        return duplicate_of(existing, &scope, &resolved.kind, &task);
                    }
                    Err(SessionStoreError::JobNotFound { .. }) => id,
                    Err(error) => return Err(error.into()),
                }
            }
            None => WorkId::new(),
        };

        let limiter_key = (context.tenant().clone(), context.submitter().clone());
        self.limiter.try_acquire(&limiter_key, now)?;

        let mut job = Job::new(
            id.clone(),
            resolved.kind.clone(),
            budget,
            resolved.retry,
            now,
        );
        if let Err(error) = job.mark_ready(now) {
            self.limiter.release(&limiter_key, now);
            return Err(error.into());
        }
        let record = StoredJob {
            job,
            scope: scope.clone(),
            input: task,
            submitted_at: now,
            not_before: now,
            lease: None,
            lease_epoch: 0,
            completion: None,
            cancellation: None,
            revision: 0,
            // AdmissionContext (tenant + submitter) carries no trace context.
            trace: None,
        };
        match self.store.admit(&record) {
            Ok(()) => {
                info!(work_id = %id, tenant = %scope.tenant(), workspace = %scope.workspace(), "job admitted");
                Ok(AdmissionOutcome {
                    record,
                    disposition: AdmissionDisposition::Admitted,
                })
            }
            Err(SessionStoreError::JobAlreadyExists { .. })
                if options.idempotency_key.is_some() =>
            {
                // Lost a race against an identical concurrent submission.
                self.limiter.release(&limiter_key, now);
                let existing = self.store.get(&id)?;
                duplicate_of(existing, &scope, &resolved.kind, &record.input)
            }
            Err(error) => {
                self.limiter.release(&limiter_key, now);
                Err(error.into())
            }
        }
    }

    /// Admits one job without options (no idempotency key, ceiling budget).
    ///
    /// # Errors
    /// See [`JobAdmissionService::submit`].
    pub fn admit(
        &self,
        intent: JobIntent,
        context: &AdmissionContext,
        now: Timestamp,
    ) -> Result<StoredJob, JobAdmissionError> {
        self.submit(intent, &SubmitOptions::default(), context, now)
            .map(|outcome| outcome.record)
    }
}

impl<P: JobAdmissionPolicy + 'static> JobAdmissionService<P> {
    /// Runs [`JobAdmissionService::submit`] on the blocking pool.
    ///
    /// # Errors
    /// Same as `submit`, plus [`JobAdmissionError::Blocking`] when the
    /// blocking task panicked or was cancelled.
    ///
    /// # Concurrency
    /// Requires a Tokio runtime; clones the `Arc` pointer only.
    pub async fn submit_async(
        self: &Arc<Self>,
        intent: JobIntent,
        options: SubmitOptions,
        context: AdmissionContext,
        now: Timestamp,
    ) -> Result<AdmissionOutcome, JobAdmissionError> {
        let service = Arc::clone(self);
        tokio::task::spawn_blocking(move || service.submit(intent, &options, &context, now))
            .await
            .map_err(|error| JobAdmissionError::Blocking(error.to_string()))?
    }

    /// Admits a job, queueing instead of rejecting when the submitter's
    /// window is exhausted.
    ///
    /// # Description
    /// A capacity-queue addendum to [`Self::submit_async`] (development task
    /// "UIA-Delegation nicht mehr blockierend"): the sliding-window rate
    /// limiter (`SubmissionLimiter::try_acquire`) is left completely
    /// unchanged (`submit`/`submit_async`/`admit` keep rejecting exactly as
    /// before). This method only adds a second, opt-in path for callers that
    /// would rather wait for capacity than fail outright — e.g. a
    /// `UserInterface`-role delegation, which wants to tell its user "queued,
    /// will start once capacity frees" instead of a hard error.
    ///
    /// One attempt is made immediately. If it is rejected for any reason
    /// other than [`JobAdmissionError::RateLimited`], that error is returned
    /// unchanged — queueing only ever applies to capacity pressure, never to
    /// a scope/budget/idempotency rejection. On `RateLimited`, a background
    /// task is spawned that keeps retrying [`Self::submit_async`] (with a
    /// fresh timestamp on every attempt) until it succeeds, a different
    /// error occurs, or `max_wait` elapses; the caller gets the estimated
    /// wait immediately and a receiver for the eventual outcome.
    ///
    /// # Arguments
    /// - `intent` (`JobIntent`): see [`Self::submit`].
    /// - `options` (`SubmitOptions`): see [`Self::submit`].
    /// - `context` (`AdmissionContext`): see [`Self::submit`].
    /// - `now` (`Timestamp`): clock for the first attempt.
    /// - `max_wait` (`std::time::Duration`): upper bound on total queued
    ///   waiting time; the background task gives up and reports the last
    ///   [`JobAdmissionError::RateLimited`] once exceeded.
    ///
    /// # Returns
    /// [`QueuedAdmission::Admitted`] if the first attempt succeeded, or
    /// [`QueuedAdmission::Queued`] with the estimated retry delay and a
    /// [`oneshot::Receiver`] that resolves once the background retry
    /// finishes (successfully or not).
    ///
    /// # Errors
    /// Any [`JobAdmissionError`] other than `RateLimited` from the first
    /// attempt is returned immediately without queueing.
    ///
    /// # Concurrency
    /// Requires a Tokio runtime. Spawns at most one background task per
    /// call; that task holds only an `Arc` clone of `self`.
    pub async fn submit_queued_async(
        self: &Arc<Self>,
        intent: JobIntent,
        options: SubmitOptions,
        context: AdmissionContext,
        now: Timestamp,
        max_wait: std::time::Duration,
    ) -> Result<QueuedAdmission, JobAdmissionError> {
        match self
            .submit_async(intent.clone(), options.clone(), context.clone(), now)
            .await
        {
            Ok(outcome) => Ok(QueuedAdmission::Admitted(Box::new(outcome))),
            Err(JobAdmissionError::RateLimited { retry_after }) => {
                let (tx, rx) = oneshot::channel();
                let service = Arc::clone(self);
                tokio::spawn(async move {
                    let deadline = tokio::time::Instant::now() + max_wait;
                    let mut last_retry_after = retry_after;
                    let outcome = loop {
                        service.limiter.wait_before_retry(last_retry_after).await;
                        let attempt = service
                            .submit_async(
                                intent.clone(),
                                options.clone(),
                                context.clone(),
                                Timestamp::now(),
                            )
                            .await;
                        match attempt {
                            Err(JobAdmissionError::RateLimited { retry_after }) => {
                                last_retry_after = retry_after;
                                if tokio::time::Instant::now() >= deadline {
                                    break Err(JobAdmissionError::RateLimited { retry_after });
                                }
                            }
                            other => break other,
                        }
                    };
                    // Best-effort: nothing to do if the caller dropped `rx`
                    // (e.g. the UIA session ended before capacity freed).
                    let _ = tx.send(outcome);
                });
                Ok(QueuedAdmission::Queued {
                    estimated_retry_after: retry_after,
                    result: rx,
                })
            }
            Err(other) => Err(other),
        }
    }
}

/// Outcome of [`JobAdmissionService::submit_queued_async`].
pub enum QueuedAdmission {
    /// Capacity was available on the first attempt; behaves exactly like
    /// [`JobAdmissionService::submit_async`].
    Admitted(Box<AdmissionOutcome>),
    /// No capacity yet. The submission was handed to a background retry
    /// loop; `result` resolves once it finishes.
    Queued {
        /// Estimate from the first rejected attempt, for an immediate
        /// user-facing "starts in about Xs" message. The background retry
        /// may finish sooner (an earlier release woke it) or later (the
        /// window stayed contended).
        estimated_retry_after: SignedDuration,
        /// Resolves to the eventual [`Self::Admitted`]-equivalent outcome or
        /// the terminal error. Dropping it does not cancel the retry.
        result: oneshot::Receiver<Result<AdmissionOutcome, JobAdmissionError>>,
    },
}

// Accepts an existing record only if it is the same logical submission.
fn duplicate_of(
    existing: StoredJob,
    scope: &JobScope,
    kind: &JobKind,
    task: &Value,
) -> Result<AdmissionOutcome, JobAdmissionError> {
    if existing.scope != *scope || existing.job.kind != *kind || existing.input != *task {
        warn!(work_id = %existing.job.id, "idempotency key reused for a different submission");
        return Err(JobAdmissionError::IdempotencyConflict {
            work_id: existing.job.id,
        });
    }
    debug!(work_id = %existing.job.id, state = ?existing.job.state, "duplicate submission");
    Ok(AdmissionOutcome {
        record: existing,
        disposition: AdmissionDisposition::Duplicate,
    })
}

// Tightens the policy ceiling with the requested budget; never loosens it.
fn effective_budget(
    ceiling: &Budget,
    requested: Option<&Budget>,
) -> Result<Budget, JobAdmissionError> {
    let Some(requested) = requested else {
        return Ok(ceiling.clone());
    };
    if requested
        .max_wall
        .is_some_and(|wall| wall <= SignedDuration::ZERO)
    {
        return Err(JobAdmissionError::InvalidBudget {
            kind: BudgetKind::WallTime,
        });
    }
    Ok(Budget {
        max_tokens: tighten(ceiling.max_tokens, requested.max_tokens, BudgetKind::Tokens)?,
        max_wall: tighten(ceiling.max_wall, requested.max_wall, BudgetKind::WallTime)?,
        max_tool_calls: tighten(
            ceiling.max_tool_calls,
            requested.max_tool_calls,
            BudgetKind::ToolCalls,
        )?,
    })
}

// One budget dimension: None request → ceiling; above ceiling → rejected.
fn tighten<T: PartialOrd + Copy>(
    ceiling: Option<T>,
    requested: Option<T>,
    kind: BudgetKind,
) -> Result<Option<T>, JobAdmissionError> {
    match (ceiling, requested) {
        (ceiling, None) => Ok(ceiling),
        (Some(limit), Some(value)) if value > limit => {
            Err(JobAdmissionError::BudgetExceedsCeiling { kind })
        }
        (_, Some(value)) => Ok(Some(value)),
    }
}

// Key of one rate window.
type SubmitterKey = (TenantId, ApprovalActor);

// Sliding-window limiter keyed by tenant and submitter.
struct SubmissionLimiter {
    limits: AdmissionLimits,
    windows: Mutex<HashMap<SubmitterKey, VecDeque<Timestamp>>>,
    // Additive queueing support (development-orchestrator task: UIA
    // delegation must wait for capacity instead of being rejected outright).
    // Woken on every `release` so a waiter does not have to sleep out the
    // full window when a slot frees up early.
    freed: Notify,
}

impl SubmissionLimiter {
    fn new(limits: AdmissionLimits) -> Self {
        Self {
            limits,
            windows: Mutex::new(HashMap::new()),
            freed: Notify::new(),
        }
    }

    // Reserves one slot at `now` or reports when the next slot frees up.
    fn try_acquire(&self, key: &SubmitterKey, now: Timestamp) -> Result<(), JobAdmissionError> {
        let window = self.limits.window;
        let mut windows = self
            .windows
            .lock()
            .map_err(|_| JobAdmissionError::LimiterUnavailable)?;
        if windows.len() > LIMITER_SWEEP_THRESHOLD {
            windows.retain(|_, entries| {
                entries
                    .back()
                    .is_some_and(|last| last.duration_until(now) < window)
            });
        }
        let entries = windows.entry(key.clone()).or_default();
        while entries
            .front()
            .is_some_and(|first| first.duration_until(now) >= window)
        {
            entries.pop_front();
        }
        if entries.len() >= self.limits.max_submissions as usize {
            let retry_after = entries.front().map_or(window, |first| {
                window.saturating_sub(first.duration_until(now))
            });
            warn!(tenant = %key.0, retry_after = %retry_after, "job submission rate limited");
            return Err(JobAdmissionError::RateLimited { retry_after });
        }
        entries.push_back(now);
        Ok(())
    }

    // Returns a reservation made at `at` whose admission did not persist.
    fn release(&self, key: &SubmitterKey, at: Timestamp) {
        let Ok(mut windows) = self.windows.lock() else {
            warn!("job submission limiter lock poisoned during release");
            return;
        };
        if let Some(entries) = windows.get_mut(key)
            && let Some(position) = entries.iter().rposition(|entry| *entry == at)
        {
            entries.remove(position);
        }
        drop(windows);
        // A slot may now be free; wake anyone waiting in `acquire_or_wait`.
        self.freed.notify_waiters();
    }

    // Sleeps until either `retry_after` elapses or `release` woke a waiter
    // early, whichever comes first. Used by `AdmissionQueue`'s background
    // retry loop between attempts; does not itself touch the window, so it
    // never double-charges a slot the way calling `try_acquire` twice would.
    async fn wait_before_retry(&self, retry_after: SignedDuration) {
        let sleep_for = retry_after
            .unsigned_abs()
            .max(std::time::Duration::from_millis(50));
        let notified = self.freed.notified();
        tokio::select! {
            () = tokio::time::sleep(sleep_for) => {},
            () = notified => {},
        }
    }
}

fn validate_workspace_alias(raw: &str) -> Result<WorkspaceId, JobAdmissionError> {
    if raw.is_empty()
        || raw == "."
        || raw == ".."
        || raw.len() > 128
        || !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(JobAdmissionError::InvalidWorkspaceAlias(raw.to_owned()));
    }
    Ok(WorkspaceId::from_str(raw))
}

const RESERVED: &[&str] = &[
    "tenant",
    "budget",
    "retry",
    "sandbox",
    "capabilities",
    "credentials",
    "work_id",
];

fn sanitize_task(value: Value) -> Result<Value, JobAdmissionError> {
    fn walk(value: Value, depth: usize) -> Result<Value, JobAdmissionError> {
        if depth > 32 {
            return Err(JobAdmissionError::TaskTooDeep);
        }
        match value {
            Value::Object(object) => {
                for key in object.keys() {
                    if RESERVED.contains(&key.as_str()) {
                        return Err(JobAdmissionError::ReservedTaskField(key.clone()));
                    }
                }
                let entries = object.into_iter().collect::<Vec<_>>();
                let mut out = serde_json::Map::new();
                for (key, value) in entries {
                    out.insert(key, walk(value, depth + 1)?);
                }
                Ok(Value::Object(out))
            }
            Value::Array(values) => values
                .into_iter()
                .map(|v| walk(v, depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            other => Ok(other),
        }
    }
    walk(value, 0)
}

/// Errors of the admission boundary.
#[derive(Debug)]
pub enum JobAdmissionError {
    /// Workspace alias violates the alias grammar.
    InvalidWorkspaceAlias(String),
    /// Task contains an authority-bearing reserved field.
    ReservedTaskField(String),
    /// Task nesting exceeds 32 levels.
    TaskTooDeep,
    /// Policy sandbox is not bound to the resolved workspace.
    SandboxBindingMismatch,
    /// Policy refused the submission.
    PolicyRejected(String),
    /// Workspace resolution failed.
    Workspace(AuthorityError),
    /// Sandbox construction or validation failed.
    Sandbox(SandboxError),
    /// Durable persistence failed.
    Store(SessionStoreError),
    /// Idempotency key violates the key grammar (key itself is not echoed).
    InvalidIdempotencyKey { length: usize },
    /// Idempotency key already names a job with a different task, kind or scope.
    IdempotencyConflict { work_id: WorkId },
    /// A requested budget dimension exceeds the policy ceiling.
    BudgetExceedsCeiling { kind: BudgetKind },
    /// A requested budget dimension is not positive.
    InvalidBudget { kind: BudgetKind },
    /// The submitter exhausted its submission window.
    RateLimited { retry_after: SignedDuration },
    /// The rate limiter lock is poisoned.
    LimiterUnavailable,
    /// The job lifecycle rejected the `Ready` transition.
    JobRuntime(JobRuntimeError),
    /// The blocking admission task panicked or was cancelled.
    Blocking(String),
}

impl fmt::Display for JobAdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidWorkspaceAlias(v) => write!(f, "invalid workspace alias '{v}'"),
            Self::ReservedTaskField(v) => write!(f, "reserved task field '{v}' is not admissible"),
            Self::TaskTooDeep => f.write_str("task nesting exceeds admission limit"),
            Self::SandboxBindingMismatch => {
                f.write_str("policy sandbox is not bound to resolved workspace")
            }
            Self::PolicyRejected(v) => write!(f, "admission policy rejected task: {v}"),
            Self::Workspace(v) => write!(f, "workspace resolution failed: {v}"),
            Self::Sandbox(v) => write!(f, "sandbox validation failed: {v}"),
            Self::Store(v) => write!(f, "job admission persistence failed: {v}"),
            Self::InvalidIdempotencyKey { length } => write!(
                f,
                "invalid idempotency key of {length} bytes: expected 1-{IDEMPOTENCY_KEY_MAX_LEN} bytes of [A-Za-z0-9._:-]"
            ),
            Self::IdempotencyConflict { work_id } => write!(
                f,
                "idempotency key already used for a different submission (job {work_id})"
            ),
            Self::BudgetExceedsCeiling { kind } => {
                write!(f, "requested {kind:?} budget exceeds the server ceiling")
            }
            Self::InvalidBudget { kind } => write!(f, "requested {kind:?} budget must be positive"),
            Self::RateLimited { retry_after } => {
                write!(
                    f,
                    "submission rate limit reached; retry after {retry_after}"
                )
            }
            Self::LimiterUnavailable => f.write_str("submission rate limiter is unavailable"),
            Self::JobRuntime(v) => write!(f, "job could not be made ready: {v}"),
            Self::Blocking(v) => write!(f, "blocking admission task failed: {v}"),
        }
    }
}

impl std::error::Error for JobAdmissionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Workspace(v) => Some(v),
            Self::Sandbox(v) => Some(v),
            Self::Store(v) => Some(v),
            Self::JobRuntime(v) => Some(v),
            _ => None,
        }
    }
}

impl From<AuthorityError> for JobAdmissionError {
    fn from(v: AuthorityError) -> Self {
        Self::Workspace(v)
    }
}

impl From<SandboxError> for JobAdmissionError {
    fn from(v: SandboxError) -> Self {
        Self::Sandbox(v)
    }
}

impl From<SessionStoreError> for JobAdmissionError {
    fn from(v: SessionStoreError) -> Self {
        Self::Store(v)
    }
}

impl From<JobRuntimeError> for JobAdmissionError {
    fn from(v: JobRuntimeError) -> Self {
        Self::JobRuntime(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{PermissionSet, WorkspaceRegistration};
    use harw_types::{ChannelId, PeerId};
    use std::path::Path;
    use tempfile::tempdir;

    struct Policy {
        ceiling: Budget,
    }

    impl JobAdmissionPolicy for Policy {
        fn resolve(
            &self,
            _: &AdmissionContext,
            workspace: &WorkspaceBinding,
            _: &Value,
        ) -> Result<ResolvedAdmission, JobAdmissionError> {
            Ok(ResolvedAdmission {
                kind: JobKind::Worker,
                budget: self.ceiling.clone(),
                retry: RetryPolicy {
                    max_attempts: 2,
                    base_delay: jiff::SignedDuration::ZERO,
                    factor: 1.0,
                    max_delay: jiff::SignedDuration::ZERO,
                    jitter: 0.0,
                },
                sandbox: SandboxSpec::from_resolved(workspace.clone(), PermissionSet::empty()),
            })
        }
    }

    fn service_with(root: &Path, ceiling: Budget) -> TestResult<JobAdmissionService<Policy>> {
        let tenant = TenantId::from_str("tenant");
        let workspace = WorkspaceId::from_str("safe");
        let registry = WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant,
                workspace,
                root: root.to_path_buf(),
            }],
        )
        .map_err(ctx("registry"))?;
        Ok(JobAdmissionService::new(
            Arc::new(JobStore::new(root)),
            Arc::new(registry),
            Policy { ceiling },
        ))
    }

    fn service(root: &Path) -> TestResult<JobAdmissionService<Policy>> {
        service_with(root, Budget::unbounded())
    }

    fn context() -> AdmissionContext {
        AdmissionContext::new(
            TenantId::from_str("tenant"),
            ApprovalActor::ChannelPeer {
                channel: ChannelId::from_str("telegram"),
                peer: PeerId::from_str("42"),
            },
        )
    }

    fn operator_context(id: &str) -> AdmissionContext {
        AdmissionContext::new(
            TenantId::from_str("tenant"),
            ApprovalActor::Operator { id: id.to_owned() },
        )
    }

    fn intent(task: Value) -> JobIntent {
        JobIntent {
            workspace: "safe".into(),
            task,
        }
    }

    fn keyed(key: &str) -> TestResult<SubmitOptions> {
        Ok(SubmitOptions {
            idempotency_key: Some(IdempotencyKey::parse(key).map_err(ctx("valid key"))?),
            requested_budget: None,
        })
    }

    #[test]
    fn closed_intent_rejects_authority_fields() {
        let parsed = serde_json::from_value::<JobIntent>(
            serde_json::json!({"workspace":"safe","task":{},"budget":{}}),
        );
        assert!(parsed.is_err());
    }

    #[test]
    fn admission_generates_identity_and_scope() -> TestResult {
        let root = tempdir().map_err(ctx("tempdir"))?;
        let record = service(root.path())?
            .admit(
                intent(serde_json::json!({"prompt":"hi"})),
                &context(),
                Timestamp::now(),
            )
            .map_err(ctx("admit"))?;
        assert!(!record.job.id.as_str().is_empty());
        assert_eq!(record.scope.tenant().as_str(), "tenant");
        assert_eq!(record.scope.workspace().as_str(), "safe");
        Ok(())
    }

    #[test]
    fn test_submit_persists_ready_job() -> TestResult {
        let root = tempdir().map_err(ctx("tempdir"))?;
        let svc = service(root.path())?;
        let outcome = svc
            .submit(
                intent(Value::Null),
                &SubmitOptions::default(),
                &context(),
                Timestamp::now(),
            )
            .map_err(ctx("submit"))?;
        assert_eq!(outcome.disposition, AdmissionDisposition::Admitted);
        assert_eq!(
            svc.store()
                .get(&outcome.record.job.id)
                .map_err(ctx("stored"))?
                .job
                .state,
            harw_job_core::JobState::Ready
        );
        Ok(())
    }

    #[test]
    fn traversal_and_nested_authority_are_rejected() -> TestResult {
        let root = tempdir().map_err(ctx("tempdir"))?;
        let svc = service(root.path())?;
        assert!(matches!(
            svc.admit(
                JobIntent {
                    workspace: "../safe".into(),
                    task: Value::Null
                },
                &context(),
                Timestamp::now()
            ),
            Err(JobAdmissionError::InvalidWorkspaceAlias(_))
        ));
        assert!(matches!(
            svc.admit(
                intent(serde_json::json!({"nested":{"credentials":"x"}})),
                &context(),
                Timestamp::now()
            ),
            Err(JobAdmissionError::ReservedTaskField(_))
        ));
        assert!(matches!(
            svc.admit(
                JobIntent {
                    workspace: "unknown".into(),
                    task: Value::Null
                },
                &context(),
                Timestamp::now()
            ),
            // Workspace resolution (WorkspaceRegistry::resolve) now rejects an
            // unbound workspace before the sandbox is ever constructed, so the
            // fail-closed rejection surfaces as `Workspace` (wrapping
            // `AuthorityError::WorkspaceNotBound`) rather than `Sandbox`. The
            // job is still refused — only the error taxonomy moved.
            Err(JobAdmissionError::Workspace(_))
        ));
        Ok(())
    }

    #[test]
    fn test_submit_same_idempotency_key_returns_same_job() -> TestResult {
        let root = tempdir().map_err(ctx("tempdir"))?;
        let svc = service(root.path())?;
        let task = serde_json::json!({"prompt": "same"});
        let first = svc
            .submit(
                intent(task.clone()),
                &keyed("node-1")?,
                &context(),
                Timestamp::now(),
            )
            .map_err(ctx("first submit"))?;
        let second = svc
            .submit(
                intent(task),
                &keyed("node-1")?,
                &context(),
                Timestamp::now(),
            )
            .map_err(ctx("second submit"))?;
        assert_eq!(first.disposition, AdmissionDisposition::Admitted);
        assert_eq!(second.disposition, AdmissionDisposition::Duplicate);
        assert_eq!(first.record.job.id, second.record.job.id);
        assert!(
            first
                .record
                .job
                .id
                .as_str()
                .starts_with(IDEMPOTENT_WORK_ID_PREFIX)
        );
        Ok(())
    }

    #[test]
    fn test_submit_same_key_with_different_task_conflicts() -> TestResult {
        let root = tempdir().map_err(ctx("tempdir"))?;
        let svc = service(root.path())?;
        svc.submit(
            intent(serde_json::json!({"a": 1})),
            &keyed("k")?,
            &context(),
            Timestamp::now(),
        )
        .map_err(ctx("first submit"))?;
        assert!(matches!(
            svc.submit(
                intent(serde_json::json!({"a": 2})),
                &keyed("k")?,
                &context(),
                Timestamp::now()
            ),
            Err(JobAdmissionError::IdempotencyConflict { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_idempotent_work_id_is_scoped_by_submitter() -> TestResult {
        let key = IdempotencyKey::parse("shared").map_err(ctx("key"))?;
        let tenant = TenantId::from_str("tenant");
        let workspace = WorkspaceId::from_str("safe");
        let a = idempotent_work_id(&tenant, &workspace, operator_context("a").submitter(), &key);
        let b = idempotent_work_id(&tenant, &workspace, operator_context("b").submitter(), &key);
        assert_ne!(a, b);
        assert_eq!(
            a,
            idempotent_work_id(&tenant, &workspace, operator_context("a").submitter(), &key)
        );
        Ok(())
    }

    #[test]
    fn test_idempotency_key_parse_rejects_invalid_keys() {
        assert!(IdempotencyKey::parse("plan-1:node.2_x").is_ok());
        assert!(matches!(
            IdempotencyKey::parse(""),
            Err(JobAdmissionError::InvalidIdempotencyKey { length: 0 })
        ));
        assert!(IdempotencyKey::parse("a/b").is_err());
        assert!(IdempotencyKey::parse(&"x".repeat(IDEMPOTENCY_KEY_MAX_LEN + 1)).is_err());
    }

    #[test]
    fn test_submit_rate_limits_per_submitter_and_skips_duplicates() -> TestResult {
        let root = tempdir().map_err(ctx("tempdir"))?;
        let svc = service(root.path())?.with_limits(
            AdmissionLimits::new(2, SignedDuration::from_secs(60))
                .ok_or(TestError::Missing("limits"))?,
        );
        let now = Timestamp::now();
        let alice = operator_context("alice");
        svc.submit(intent(Value::Null), &keyed("one")?, &alice, now)
            .map_err(ctx("first"))?;
        svc.submit(intent(Value::Null), &keyed("one")?, &alice, now)
            .map_err(ctx("duplicate is not charged"))?;
        svc.submit(intent(Value::Null), &SubmitOptions::default(), &alice, now)
            .map_err(ctx("second"))?;
        assert!(matches!(
            svc.submit(intent(Value::Null), &SubmitOptions::default(), &alice, now),
            Err(JobAdmissionError::RateLimited { .. })
        ));
        svc.submit(
            intent(Value::Null),
            &SubmitOptions::default(),
            &operator_context("bob"),
            now,
        )
        .map_err(ctx("other submitter has its own window"))?;
        Ok(())
    }

    #[test]
    fn test_admission_limits_new_rejects_zero() {
        assert!(AdmissionLimits::new(0, SignedDuration::from_secs(1)).is_none());
        assert!(AdmissionLimits::new(1, SignedDuration::ZERO).is_none());
        assert_eq!(
            AdmissionLimits::default().max_submissions(),
            AdmissionLimits::DEFAULT_MAX_SUBMISSIONS
        );
    }

    #[test]
    fn test_submit_budget_may_only_tighten_the_ceiling() -> TestResult {
        let root = tempdir().map_err(ctx("tempdir"))?;
        let ceiling = Budget {
            max_tokens: Some(100),
            max_wall: Some(SignedDuration::from_secs(60)),
            max_tool_calls: None,
        };
        let svc = service_with(root.path(), ceiling)?;
        let over = SubmitOptions {
            idempotency_key: None,
            requested_budget: Some(Budget {
                max_tokens: Some(200),
                max_wall: None,
                max_tool_calls: None,
            }),
        };
        assert!(matches!(
            svc.submit(intent(Value::Null), &over, &context(), Timestamp::now()),
            Err(JobAdmissionError::BudgetExceedsCeiling {
                kind: BudgetKind::Tokens
            })
        ));
        let under = SubmitOptions {
            idempotency_key: None,
            requested_budget: Some(Budget {
                max_tokens: Some(50),
                max_wall: None,
                max_tool_calls: Some(3),
            }),
        };
        let outcome = svc
            .submit(intent(Value::Null), &under, &context(), Timestamp::now())
            .map_err(ctx("tighter budget"))?;
        assert_eq!(outcome.record.job.budget.max_tokens, Some(50));
        assert_eq!(
            outcome.record.job.budget.max_wall,
            Some(SignedDuration::from_secs(60))
        );
        assert_eq!(outcome.record.job.budget.max_tool_calls, Some(3));
        Ok(())
    }

    #[tokio::test]
    async fn test_submit_async_admits_on_blocking_pool() -> TestResult {
        let root = tempdir().map_err(ctx("tempdir"))?;
        let svc = Arc::new(service(root.path())?);
        let outcome = svc
            .submit_async(
                intent(Value::Null),
                keyed("async")?,
                context(),
                Timestamp::now(),
            )
            .await
            .map_err(ctx("async submit"))?;
        assert_eq!(outcome.disposition, AdmissionDisposition::Admitted);
        Ok(())
    }

    // ── SubmissionLimiter::wait_before_retry / Notify wake ──────────────────

    #[tokio::test]
    async fn test_wait_before_retry_wakes_early_on_release() -> TestResult {
        let key: SubmitterKey = (
            TenantId::from_str("tenant"),
            ApprovalActor::Operator { id: "op".into() },
        );

        // Shared so `release()` (called from this task) reaches the same
        // `Notify` that the spawned task's `wait_before_retry` awaits on.
        let limiter = Arc::new(SubmissionLimiter::new(AdmissionLimits::default()));
        let waiter_limiter = Arc::clone(&limiter);
        let started = std::time::Instant::now();
        let waiter = tokio::spawn(async move {
            waiter_limiter
                .wait_before_retry(SignedDuration::from_secs(30))
                .await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        limiter.release(&key, Timestamp::now());
        waiter.await.map_err(ctx("waiter task does not panic"))?;

        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "release() must wake wait_before_retry long before the 30s retry_after elapses, took {:?}",
            started.elapsed()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_wait_before_retry_sleeps_out_retry_after_without_release() {
        let limiter = SubmissionLimiter::new(AdmissionLimits::default());
        let started = std::time::Instant::now();
        limiter
            .wait_before_retry(SignedDuration::from_millis(80))
            .await;
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(70),
            "without a release(), wait_before_retry must sleep out retry_after, took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn test_wait_before_retry_enforces_minimum_sleep_floor() {
        let limiter = SubmissionLimiter::new(AdmissionLimits::default());
        let started = std::time::Instant::now();
        // A non-positive retry_after must still sleep the 50ms floor, not
        // return instantly.
        limiter.wait_before_retry(SignedDuration::ZERO).await;
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(45),
            "a zero retry_after must still respect the minimum sleep floor, took {:?}",
            started.elapsed()
        );
    }

    // ── JobAdmissionService::submit_queued_async ────────────────────────────

    #[tokio::test]
    async fn test_submit_queued_async_admits_immediately_when_capacity_is_free() -> TestResult {
        let root = tempdir().map_err(ctx("tempdir"))?;
        let svc = Arc::new(service(root.path())?);
        let outcome = svc
            .submit_queued_async(
                intent(Value::Null),
                SubmitOptions::default(),
                operator_context("queue-immediate"),
                Timestamp::now(),
                std::time::Duration::from_secs(5),
            )
            .await
            .map_err(ctx("first attempt succeeds outright"))?;

        match outcome {
            QueuedAdmission::Admitted(admission) => {
                assert_eq!(admission.disposition, AdmissionDisposition::Admitted);
            }
            QueuedAdmission::Queued { .. } => {
                return Err(TestError::Unexpected(
                    "free capacity must not be queued".to_owned(),
                ));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_submit_queued_async_returns_non_rate_limit_errors_immediately() -> TestResult {
        let root = tempdir().map_err(ctx("tempdir"))?;
        let svc = Arc::new(service(root.path())?);
        let result = svc
            .submit_queued_async(
                JobIntent {
                    workspace: "unknown-workspace".into(),
                    task: Value::Null,
                },
                SubmitOptions::default(),
                operator_context("queue-scope-error"),
                Timestamp::now(),
                std::time::Duration::from_secs(5),
            )
            .await;

        // Same fail-closed rejection as `traversal_and_nested_authority_are_rejected`:
        // an unbound workspace is refused during workspace resolution, so the
        // error variant is `Workspace`, not `Sandbox`.
        assert!(matches!(result, Err(JobAdmissionError::Workspace(_))));
        Ok(())
    }

    #[tokio::test]
    async fn test_submit_queued_async_queues_and_resolves_once_capacity_frees() -> TestResult {
        let root = tempdir().map_err(ctx("tempdir"))?;
        let svc = Arc::new(
            service(root.path())?.with_limits(
                AdmissionLimits::new(1, SignedDuration::from_millis(150))
                    .ok_or(TestError::Missing("limits"))?,
            ),
        );
        let alice = operator_context("queue-resolves");
        svc.submit(
            intent(Value::Null),
            &SubmitOptions::default(),
            &alice,
            Timestamp::now(),
        )
        .map_err(ctx("fills the single-slot window"))?;

        let queued = svc
            .submit_queued_async(
                intent(Value::Null),
                SubmitOptions::default(),
                alice,
                Timestamp::now(),
                std::time::Duration::from_secs(5),
            )
            .await
            .map_err(ctx("second attempt is rejected only by rate limiting"))?;

        let QueuedAdmission::Queued {
            estimated_retry_after,
            result,
        } = queued
        else {
            return Err(TestError::Unexpected(
                "an exhausted window must be queued, not rejected outright".to_owned(),
            ));
        };
        assert!(estimated_retry_after > SignedDuration::ZERO);

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(3), result)
            .await
            .map_err(ctx(
                "background retry finishes well within the window + margin",
            ))?
            .map_err(ctx("the sender side is not dropped without sending"))?;
        match outcome {
            Ok(admission) => assert_eq!(admission.disposition, AdmissionDisposition::Admitted),
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "expected the retry to succeed once the window elapsed: {error}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_submit_queued_async_gives_up_after_max_wait() -> TestResult {
        let root = tempdir().map_err(ctx("tempdir"))?;
        let svc = Arc::new(
            service(root.path())?.with_limits(
                AdmissionLimits::new(1, SignedDuration::from_secs(30))
                    .ok_or(TestError::Missing("limits"))?,
            ),
        );
        let alice = operator_context("queue-timeout");
        svc.submit(
            intent(Value::Null),
            &SubmitOptions::default(),
            &alice,
            Timestamp::now(),
        )
        .map_err(ctx("fills the single-slot window for the whole test"))?;

        let queued = svc
            .submit_queued_async(
                intent(Value::Null),
                SubmitOptions::default(),
                alice.clone(),
                Timestamp::now(),
                std::time::Duration::from_millis(80),
            )
            .await
            .map_err(ctx("second attempt is rejected only by rate limiting"))?;

        let QueuedAdmission::Queued { result, .. } = queued else {
            return Err(TestError::Unexpected(
                "an exhausted window must be queued, not rejected outright".to_owned(),
            ));
        };

        // The 30s window never naturally frees within this test. Repeatedly
        // wake the background retry loop early (as a real `release()` from
        // an unrelated submission would) so it re-checks `max_wait` quickly
        // instead of sleeping out the full (30s) retry_after once.
        let waker_service = Arc::clone(&svc);
        let waker = tokio::spawn(async move {
            let unrelated_key: SubmitterKey = (
                TenantId::from_str("tenant"),
                ApprovalActor::Operator {
                    id: "unrelated-waker".into(),
                },
            );
            for _ in 0..60 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                waker_service
                    .limiter
                    .release(&unrelated_key, Timestamp::now());
            }
        });

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(3), result)
            .await
            .map_err(ctx("the retry loop must give up once max_wait elapses"))?
            .map_err(ctx("the sender side is not dropped without sending"))?;
        waker.abort();

        assert!(matches!(
            outcome,
            Err(JobAdmissionError::RateLimited { .. })
        ));
        Ok(())
    }
}
