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
//! # Concurrency
//! The service is `Send + Sync` when `P` is. The rate limiter uses one
//! `std::sync::Mutex` held only for window bookkeeping. [`JobAdmissionService::submit`]
//! performs blocking store I/O; async callers use
//! [`JobAdmissionService::submit_async`], which runs it on `spawn_blocking`.
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

use harw_job_runtime::{
    Budget, BudgetKind, Job, JobKind, JobRuntimeError, JobScope, RetryPolicy, StoredJob,
};
use harw_sandbox::{SandboxError, SandboxSpec, WorkspaceBinding, WorkspaceRegistry};
use harw_session_store::{JobStore, SessionStoreError};
use harw_types::{ApprovalActor, ContentDigest, TenantId, WorkId, WorkspaceId};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
            return Err(JobAdmissionError::InvalidIdempotencyKey {
                length: raw.len(),
            });
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
                let id = idempotent_work_id(scope.tenant(), scope.workspace(), scope.submitter(), key);
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

        let mut job = Job::new(id.clone(), resolved.kind.clone(), budget, resolved.retry, now);
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
            Err(SessionStoreError::JobAlreadyExists { .. }) if options.idempotency_key.is_some() => {
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
    if requested.max_wall.is_some_and(|wall| wall <= SignedDuration::ZERO) {
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
}

impl SubmissionLimiter {
    fn new(limits: AdmissionLimits) -> Self {
        Self {
            limits,
            windows: Mutex::new(HashMap::new()),
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
            Self::Sandbox(v) => write!(f, "workspace resolution failed: {v}"),
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
                write!(f, "submission rate limit reached; retry after {retry_after}")
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
            Self::Sandbox(v) => Some(v),
            Self::Store(v) => Some(v),
            Self::JobRuntime(v) => Some(v),
            _ => None,
        }
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
    use harw_sandbox::{PermissionSet, WorkspaceRegistration};
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
                },
                sandbox: SandboxSpec::from_resolved(workspace.clone(), PermissionSet::empty()),
            })
        }
    }

    fn service_with(root: &Path, ceiling: Budget) -> JobAdmissionService<Policy> {
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
        .expect("registry");
        JobAdmissionService::new(
            Arc::new(JobStore::new(root)),
            Arc::new(registry),
            Policy { ceiling },
        )
    }

    fn service(root: &Path) -> JobAdmissionService<Policy> {
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

    fn keyed(key: &str) -> SubmitOptions {
        SubmitOptions {
            idempotency_key: Some(IdempotencyKey::parse(key).expect("valid key")),
            requested_budget: None,
        }
    }

    #[test]
    fn closed_intent_rejects_authority_fields() {
        let parsed = serde_json::from_value::<JobIntent>(
            serde_json::json!({"workspace":"safe","task":{},"budget":{}}),
        );
        assert!(parsed.is_err());
    }

    #[test]
    fn admission_generates_identity_and_scope() {
        let root = tempdir().expect("tempdir");
        let record = service(root.path())
            .admit(
                intent(serde_json::json!({"prompt":"hi"})),
                &context(),
                Timestamp::now(),
            )
            .expect("admit");
        assert!(!record.job.id.as_str().is_empty());
        assert_eq!(record.scope.tenant().as_str(), "tenant");
        assert_eq!(record.scope.workspace().as_str(), "safe");
    }

    #[test]
    fn test_submit_persists_ready_job() {
        let root = tempdir().expect("tempdir");
        let svc = service(root.path());
        let outcome = svc
            .submit(intent(Value::Null), &SubmitOptions::default(), &context(), Timestamp::now())
            .expect("submit");
        assert_eq!(outcome.disposition, AdmissionDisposition::Admitted);
        assert_eq!(
            svc.store().get(&outcome.record.job.id).expect("stored").job.state,
            harw_job_runtime::JobState::Ready
        );
    }

    #[test]
    fn traversal_and_nested_authority_are_rejected() {
        let root = tempdir().expect("tempdir");
        let svc = service(root.path());
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
            Err(JobAdmissionError::Sandbox(_))
        ));
    }

    #[test]
    fn test_submit_same_idempotency_key_returns_same_job() {
        let root = tempdir().expect("tempdir");
        let svc = service(root.path());
        let task = serde_json::json!({"prompt": "same"});
        let first = svc
            .submit(intent(task.clone()), &keyed("node-1"), &context(), Timestamp::now())
            .expect("first submit");
        let second = svc
            .submit(intent(task), &keyed("node-1"), &context(), Timestamp::now())
            .expect("second submit");
        assert_eq!(first.disposition, AdmissionDisposition::Admitted);
        assert_eq!(second.disposition, AdmissionDisposition::Duplicate);
        assert_eq!(first.record.job.id, second.record.job.id);
        assert!(first.record.job.id.as_str().starts_with(IDEMPOTENT_WORK_ID_PREFIX));
    }

    #[test]
    fn test_submit_same_key_with_different_task_conflicts() {
        let root = tempdir().expect("tempdir");
        let svc = service(root.path());
        svc.submit(intent(serde_json::json!({"a": 1})), &keyed("k"), &context(), Timestamp::now())
            .expect("first submit");
        assert!(matches!(
            svc.submit(intent(serde_json::json!({"a": 2})), &keyed("k"), &context(), Timestamp::now()),
            Err(JobAdmissionError::IdempotencyConflict { .. })
        ));
    }

    #[test]
    fn test_idempotent_work_id_is_scoped_by_submitter() {
        let key = IdempotencyKey::parse("shared").expect("key");
        let tenant = TenantId::from_str("tenant");
        let workspace = WorkspaceId::from_str("safe");
        let a = idempotent_work_id(&tenant, &workspace, operator_context("a").submitter(), &key);
        let b = idempotent_work_id(&tenant, &workspace, operator_context("b").submitter(), &key);
        assert_ne!(a, b);
        assert_eq!(
            a,
            idempotent_work_id(&tenant, &workspace, operator_context("a").submitter(), &key)
        );
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
    fn test_submit_rate_limits_per_submitter_and_skips_duplicates() {
        let root = tempdir().expect("tempdir");
        let svc = service(root.path()).with_limits(
            AdmissionLimits::new(2, SignedDuration::from_secs(60)).expect("limits"),
        );
        let now = Timestamp::now();
        let alice = operator_context("alice");
        svc.submit(intent(Value::Null), &keyed("one"), &alice, now)
            .expect("first");
        svc.submit(intent(Value::Null), &keyed("one"), &alice, now)
            .expect("duplicate is not charged");
        svc.submit(intent(Value::Null), &SubmitOptions::default(), &alice, now)
            .expect("second");
        assert!(matches!(
            svc.submit(intent(Value::Null), &SubmitOptions::default(), &alice, now),
            Err(JobAdmissionError::RateLimited { .. })
        ));
        svc.submit(intent(Value::Null), &SubmitOptions::default(), &operator_context("bob"), now)
            .expect("other submitter has its own window");
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
    fn test_submit_budget_may_only_tighten_the_ceiling() {
        let root = tempdir().expect("tempdir");
        let ceiling = Budget {
            max_tokens: Some(100),
            max_wall: Some(SignedDuration::from_secs(60)),
            max_tool_calls: None,
        };
        let svc = service_with(root.path(), ceiling);
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
            .expect("tighter budget");
        assert_eq!(outcome.record.job.budget.max_tokens, Some(50));
        assert_eq!(
            outcome.record.job.budget.max_wall,
            Some(SignedDuration::from_secs(60))
        );
        assert_eq!(outcome.record.job.budget.max_tool_calls, Some(3));
    }

    #[tokio::test]
    async fn test_submit_async_admits_on_blocking_pool() {
        let root = tempdir().expect("tempdir");
        let svc = Arc::new(service(root.path()));
        let outcome = svc
            .submit_async(intent(Value::Null), keyed("async"), context(), Timestamp::now())
            .await
            .expect("async submit");
        assert_eq!(outcome.disposition, AdmissionDisposition::Admitted);
    }
}
