//! Durable runtime for adaptive intent cycles (PL-90 W02).
//!
//! Connects the pure transition core in [`crate::intent_cycle`] to the
//! existing job substrate instead of introducing a parallel one:
//!
//! - **Persistence:** [`CycleStore`] keeps one [`CycleRecord`] per cycle in a
//!   `harw_job_store::RecordStore` (per-record lock, temp + rename + fsync,
//!   corrupt records quarantined). Every mutation is a fenced
//!   compare-and-swap under the record lock: [`check_commit`] for steps,
//!   [`check_rebind`] for lease takeovers.
//! - **Lease fencing:** the checkpoint epoch *is* the job lease epoch
//!   ([`LeaseToken::epoch`]). A claim with a newer epoch must first reissue
//!   the persisted `AuthoritySnapshot` through an [`AuthorityReissuer`]
//!   (production: [`PolicyReissuer`] → `PolicyBootstrap::reissue`), narrow
//!   via [`resume_admission`] and rebind. A writer with an older epoch is
//!   refused at every store operation.
//! - **Effect journal:** before an admitted step runs, it is journaled as
//!   [`InFlightStep`] with a stable idempotency key. After a crash the next
//!   lease asks the executor to [reconcile](CycleStepExecutor::reconcile) it:
//!   not started → dropped, completed → its observations are committed,
//!   uncertain → the cycle escalates instead of guessing or repeating.
//! - **Driver:** [`CycleDriver`] separates the model ([`CycleProposer`],
//!   proposes only) from trusted execution ([`CycleStepExecutor`]). Refused
//!   proposals are fed back a bounded number of times, then the cycle
//!   escalates; an unavailable proposer (provider 403, offline model) is a
//!   capability blocker and ends the cycle as `Blocked`, never a retry loop.
//! - **Jobs:** [`cycle_job_operation`] plugs a driver into
//!   `harw_core::DurableJobRunner`, which owns claim, heartbeat, lease-loss
//!   cancellation and the terminal job commit.
//!
//! Not in this module: which tools, child agents or model routes an executor
//! uses (W03+), and goal acceptance — a `CompletionProposed` cycle is a
//! succeeded *job*, not an achieved goal.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use harw_authority::{AuthorityContext, AuthoritySnapshot, PolicyBootstrap, WorkspaceBinding};
use harw_core::ExecutionControl;
use harw_core::cancel::{CancelReason, CancelToken};
use harw_job_core::{JobClaim, JobOutcome, LeaseToken};
use harw_job_store::{RecordStore, StoreError, StoreRecord, validate_id};
use harw_types::WorkId;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::intent_cycle::{
    AdmittedCycle, CycleAdmission, CycleCheckpoint, CycleObservations, CycleProposal, CycleRefusal,
    CycleTerminal, FenceRefusal, ObservationError, ResumeRefusal, admit_cycle, apply_observations,
    check_commit, check_rebind, check_state, resume_admission,
};

/// Schema version of [`CycleRecord`].
pub const CYCLE_RECORD_SCHEMA: u32 = 1;

/// Default number of consecutive admission refusals before escalation.
pub const DEFAULT_MAX_REFUSALS_PER_STEP: u8 = 3;

/// A step journaled before execution; present until its outcome is committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InFlightStep {
    /// Lease epoch that started the step.
    pub epoch: u64,
    /// Checkpoint sequence the step was admitted against.
    pub sequence: u64,
    /// Stable per cycle and sequence; executors use it to deduplicate
    /// external effects and to answer [`CycleStepExecutor::reconcile`].
    pub idempotency_key: String,
    pub proposal: CycleProposal,
}

/// Durable record of one cycle, bound to exactly one job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CycleRecord {
    pub schema_version: u32,
    pub cycle_id: String,
    pub work_id: WorkId,
    pub checkpoint: CycleCheckpoint,
    pub in_flight: Option<InFlightStep>,
}

impl StoreRecord for CycleRecord {
    fn record_id(&self) -> &str {
        &self.cycle_id
    }
}

impl CycleRecord {
    /// A fresh record for `checkpoint` (sequence 0, not terminal).
    ///
    /// Create the checkpoint with the epoch the job had *before* its first
    /// claim (normally 0): the first claim then takes the regular
    /// reissue-and-rebind path like every later one.
    ///
    /// # Errors
    /// [`CycleStoreError::InvalidRecord`] for an unsafe id or a checkpoint
    /// that already ran.
    pub fn new(
        cycle_id: impl Into<String>,
        work_id: WorkId,
        checkpoint: CycleCheckpoint,
    ) -> Result<Self, CycleStoreError> {
        let cycle_id = cycle_id.into();
        if validate_id(&cycle_id).is_err()
            || checkpoint.fence.sequence != 0
            || checkpoint.terminal.is_some()
        {
            return Err(CycleStoreError::InvalidRecord);
        }
        Ok(Self {
            schema_version: CYCLE_RECORD_SCHEMA,
            cycle_id,
            work_id,
            checkpoint,
            in_flight: None,
        })
    }

    /// Compact, parent-safe projection (no evidence bodies, no claims).
    #[must_use]
    pub fn status(&self) -> CycleStatus {
        let checkpoint = &self.checkpoint;
        CycleStatus {
            cycle_id: self.cycle_id.clone(),
            work_id: self.work_id.clone(),
            intent_id: checkpoint.intent_id.clone(),
            intent_revision: checkpoint.intent_revision,
            epoch: checkpoint.fence.epoch,
            sequence: checkpoint.fence.sequence,
            transitions_used: checkpoint.transitions_used,
            stall_transitions: checkpoint.stall_transitions,
            evidence: checkpoint.evidence.len(),
            criteria_covered: checkpoint.criteria_met.keys().cloned().collect(),
            in_flight: self
                .in_flight
                .as_ref()
                .map(|step| step.idempotency_key.clone()),
            terminal: checkpoint.terminal,
        }
    }
}

/// Bounded status view for `status` surfaces (TUI, Web, parent agents).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CycleStatus {
    pub cycle_id: String,
    pub work_id: WorkId,
    pub intent_id: String,
    pub intent_revision: u64,
    pub epoch: u64,
    pub sequence: u64,
    pub transitions_used: u32,
    pub stall_transitions: u32,
    pub evidence: usize,
    pub criteria_covered: Vec<String>,
    pub in_flight: Option<String>,
    pub terminal: Option<CycleTerminal>,
}

/// Failures of [`CycleStore`] operations.
#[derive(Debug)]
pub enum CycleStoreError {
    Store(StoreError),
    /// The fence refused the write (stale epoch, reordering, widening).
    Fence(FenceRefusal),
    UnsupportedSchema,
    /// The record belongs to another job.
    WrongJob,
    /// The lease is newer than the record: rebind before stepping.
    NotRebound,
    /// A rebind candidate is not fenced under the presenting lease's epoch.
    EpochMismatch,
    /// A step is already journaled; reconcile it first.
    StepInFlight,
    /// Commit without a journaled step.
    NoStepInFlight,
    /// The journaled step does not match the commit or clear request.
    StepMismatch,
    InvalidRecord,
}

impl From<StoreError> for CycleStoreError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl fmt::Display for CycleStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(f, "cycle store: {error}"),
            Self::Fence(refusal) => write!(f, "cycle fence refused write: {refusal:?}"),
            Self::UnsupportedSchema => f.write_str("unsupported cycle record schema"),
            Self::WrongJob => f.write_str("cycle record belongs to another job"),
            Self::NotRebound => f.write_str("lease epoch is newer than the cycle; rebind first"),
            Self::EpochMismatch => f.write_str("rebind candidate epoch differs from lease epoch"),
            Self::StepInFlight => f.write_str("a cycle step is already in flight"),
            Self::NoStepInFlight => f.write_str("no cycle step is in flight"),
            Self::StepMismatch => f.write_str("in-flight cycle step does not match"),
            Self::InvalidRecord => f.write_str("invalid cycle record"),
        }
    }
}

impl std::error::Error for CycleStoreError {}

/// Fenced persistence of [`CycleRecord`]s.
pub struct CycleStore {
    records: RecordStore<CycleRecord>,
}

impl CycleStore {
    #[must_use]
    pub fn new(records: RecordStore<CycleRecord>) -> Self {
        Self { records }
    }

    /// Persists a new record; an existing id is never overwritten.
    ///
    /// # Errors
    /// [`CycleStoreError::InvalidRecord`] or the underlying store error.
    pub fn create(&self, record: &CycleRecord) -> Result<(), CycleStoreError> {
        if record.schema_version != CYCLE_RECORD_SCHEMA
            || record.in_flight.is_some()
            || record.checkpoint.fence.sequence != 0
        {
            return Err(CycleStoreError::InvalidRecord);
        }
        Ok(self.records.create(record)?)
    }

    /// Loads one record without locking.
    ///
    /// # Errors
    /// The underlying store error.
    pub fn load(&self, cycle_id: &str) -> Result<CycleRecord, CycleStoreError> {
        Ok(self.records.load(cycle_id)?)
    }

    /// Takes over a cycle under a newer lease with the output of
    /// [`resume_admission`]. The journaled step (if any) is kept for
    /// reconciliation.
    ///
    /// # Errors
    /// [`CycleStoreError`] if the record belongs to another job, the
    /// candidate is not fenced under `token`, or [`check_rebind`] refuses.
    pub fn rebind(
        &self,
        cycle_id: &str,
        token: &LeaseToken,
        resumed: &CycleCheckpoint,
    ) -> Result<CycleRecord, CycleStoreError> {
        self.records.update_locked(cycle_id, |record| {
            check_schema_and_job(record, token)?;
            if resumed.fence.epoch != token.epoch {
                return Err(CycleStoreError::EpochMismatch);
            }
            check_rebind(&record.checkpoint, resumed).map_err(CycleStoreError::Fence)?;
            record.checkpoint = resumed.clone();
            Ok(record.clone())
        })
    }

    /// Journals `admitted` as the in-flight step under `token`.
    ///
    /// # Errors
    /// [`CycleStoreError`] for a foreign or stale lease, an existing
    /// in-flight step, or an admission decided against another sequence.
    pub fn begin_step(
        &self,
        cycle_id: &str,
        token: &LeaseToken,
        admitted: &AdmittedCycle,
    ) -> Result<InFlightStep, CycleStoreError> {
        self.records.update_locked(cycle_id, |record| {
            check_fenced(record, token)?;
            if record.in_flight.is_some() {
                return Err(CycleStoreError::StepInFlight);
            }
            let sequence = record.checkpoint.fence.sequence;
            if admitted.expected_sequence() != sequence {
                return Err(CycleStoreError::Fence(FenceRefusal::OutOfOrder));
            }
            let step = InFlightStep {
                epoch: token.epoch,
                sequence,
                idempotency_key: format!("{}-{sequence}", record.cycle_id),
                proposal: admitted.proposal().clone(),
            };
            record.in_flight = Some(step.clone());
            Ok(step)
        })
    }

    /// Commits the outcome of the journaled step and clears the journal.
    ///
    /// # Errors
    /// [`CycleStoreError`] for a foreign or stale lease, a missing or
    /// mismatching journaled step, or a [`check_commit`] refusal.
    pub fn commit_step(
        &self,
        cycle_id: &str,
        token: &LeaseToken,
        next: &CycleCheckpoint,
    ) -> Result<(), CycleStoreError> {
        self.records.update_locked(cycle_id, |record| {
            check_fenced(record, token)?;
            let step = record
                .in_flight
                .as_ref()
                .ok_or(CycleStoreError::NoStepInFlight)?;
            if step.sequence != record.checkpoint.fence.sequence {
                return Err(CycleStoreError::StepMismatch);
            }
            check_commit(&record.checkpoint, next).map_err(CycleStoreError::Fence)?;
            record.checkpoint = next.clone();
            record.in_flight = None;
            Ok(())
        })
    }

    /// Drops a journaled step the executor proved never started.
    ///
    /// # Errors
    /// [`CycleStoreError`] for a foreign or stale lease or another step.
    pub fn clear_step(
        &self,
        cycle_id: &str,
        token: &LeaseToken,
        step: &InFlightStep,
    ) -> Result<(), CycleStoreError> {
        self.records.update_locked(cycle_id, |record| {
            check_fenced(record, token)?;
            if record.in_flight.as_ref() != Some(step) {
                return Err(CycleStoreError::StepMismatch);
            }
            record.in_flight = None;
            Ok(())
        })
    }
}

fn check_schema_and_job(record: &CycleRecord, token: &LeaseToken) -> Result<(), CycleStoreError> {
    if record.schema_version != CYCLE_RECORD_SCHEMA {
        return Err(CycleStoreError::UnsupportedSchema);
    }
    if record.work_id != token.work_id {
        return Err(CycleStoreError::WrongJob);
    }
    Ok(())
}

fn check_fenced(record: &CycleRecord, token: &LeaseToken) -> Result<(), CycleStoreError> {
    check_schema_and_job(record, token)?;
    match token.epoch.cmp(&record.checkpoint.fence.epoch) {
        std::cmp::Ordering::Less => Err(CycleStoreError::Fence(FenceRefusal::StaleEpoch)),
        std::cmp::Ordering::Greater => Err(CycleStoreError::NotRebound),
        std::cmp::Ordering::Equal => Ok(()),
    }
}

/// A proposer or executor that could not do its part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepFailure {
    pub reason: String,
    /// `true` when the failure is temporary (provider overloaded, rate
    /// limited): the cycle must stay non-terminal so a new lease can retry.
    pub retryable: bool,
}

impl StepFailure {
    /// A permanent failure (missing capability, auth, bad output).
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retryable: false,
        }
    }

    /// A temporary failure; the driver commits no terminal state for it.
    #[must_use]
    pub fn retryable(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retryable: true,
        }
    }
}

/// The model side: proposes the next transition, nothing else.
pub trait CycleProposer: Send {
    /// Proposes the next transition for `state`. `last_refusal` is the
    /// admission refusal of the previous proposal for the same step, so the
    /// model can correct course instead of repeating it.
    ///
    /// Returning `Err` means the proposer itself is unavailable. A permanent
    /// failure (missing route, auth) ends the cycle as `Blocked`; a
    /// [`StepFailure::retryable`] one leaves the cycle non-terminal and
    /// surfaces as [`CycleRunError::ProposerUnavailable`], which the job
    /// runner retries under a new lease.
    fn propose(
        &mut self,
        admission: &CycleAdmission,
        state: &CycleCheckpoint,
        last_refusal: Option<CycleRefusal>,
    ) -> impl Future<Output = Result<CycleProposal, StepFailure>> + Send;
}

/// Outcome of reconciling a journaled step after a crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepReconciliation {
    /// No effect happened; the step can be dropped.
    NotStarted,
    /// The step finished; these are its observations.
    Completed(CycleObservations),
    /// The effect cannot be established either way.
    Uncertain,
}

/// The trusted side: runs admitted, non-terminal steps.
pub trait CycleStepExecutor: Send {
    /// Executes `step` (already admitted and journaled). The executor still
    /// enforces its own authority, approval and write-path checks; an
    /// admitted `ProposePatch` is not a write grant.
    ///
    /// `Err` must mean that no effect happened (e.g. a capability failure);
    /// the step is then committed without observations and counts as a
    /// stall. If `cancel` fires while an effect may be partial, return `Ok`
    /// with what was observed, or `Err` and let reconciliation decide.
    fn execute(
        &mut self,
        step: &InFlightStep,
        admission: &CycleAdmission,
        state: &CycleCheckpoint,
        cancel: &CancelToken,
    ) -> impl Future<Output = Result<CycleObservations, StepFailure>> + Send;

    /// Called by the driver with the freshly reissued authority whenever a
    /// new lease epoch takes the cycle over, before any step of that lease
    /// runs. Executors that keep their own [`AuthorityContext`] replace it
    /// here so permission checks use the current lease's authority. The
    /// default does nothing (for executors that hold no authority).
    fn rebind_authority(&mut self, _authority: &AuthorityContext) {}

    /// Establishes what happened to a step journaled by an earlier lease,
    /// typically by looking up `step.idempotency_key`.
    fn reconcile(&mut self, step: &InFlightStep)
    -> impl Future<Output = StepReconciliation> + Send;
}

/// Turns a persisted, non-authorizing snapshot back into a live context.
pub trait AuthorityReissuer: Send + Sync {
    /// # Errors
    /// A rendered reason when the snapshot cannot be reissued.
    fn reissue(&self, snapshot: &AuthoritySnapshot) -> Result<AuthorityContext, String>;
}

/// Production reissuer: `PolicyBootstrap::reissue` against the freshly
/// resolved workspace binding.
#[derive(Debug, Clone)]
pub struct PolicyReissuer {
    bootstrap: PolicyBootstrap,
    workspace: WorkspaceBinding,
}

impl PolicyReissuer {
    #[must_use]
    pub fn new(bootstrap: PolicyBootstrap, workspace: WorkspaceBinding) -> Self {
        Self {
            bootstrap,
            workspace,
        }
    }
}

impl AuthorityReissuer for PolicyReissuer {
    fn reissue(&self, snapshot: &AuthoritySnapshot) -> Result<AuthorityContext, String> {
        self.bootstrap
            .reissue(self.workspace.clone(), snapshot)
            .map_err(|error| error.to_string())
    }
}

/// How a driver run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CycleRunOutcome {
    Terminal {
        terminal: CycleTerminal,
        checkpoint: CycleCheckpoint,
    },
    /// Stopped between steps (or with a step left for reconciliation).
    Cancelled { checkpoint: CycleCheckpoint },
}

/// Why a driver run could not continue.
#[derive(Debug)]
pub enum CycleRunError {
    Store(CycleStoreError),
    Reissue(String),
    Resume(ResumeRefusal),
    Admission(CycleRefusal),
    Observation(ObservationError),
    Join(String),
    /// Temporary proposer outage; nothing terminal was committed.
    ProposerUnavailable(String),
}

impl fmt::Display for CycleRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(f, "{error}"),
            Self::Reissue(reason) => write!(f, "authority reissue failed: {reason}"),
            Self::Resume(refusal) => write!(f, "cycle resume refused: {refusal:?}"),
            Self::Admission(refusal) => write!(f, "cycle admission refused: {refusal:?}"),
            Self::Observation(error) => write!(f, "cycle observation rejected: {error:?}"),
            Self::Join(reason) => write!(f, "blocking store task failed: {reason}"),
            Self::ProposerUnavailable(reason) => {
                write!(f, "proposer temporarily unavailable (retryable): {reason}")
            }
        }
    }
}

impl std::error::Error for CycleRunError {}

impl From<CycleStoreError> for CycleRunError {
    fn from(error: CycleStoreError) -> Self {
        Self::Store(error)
    }
}

/// Driver limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleDriverConfig {
    /// Consecutive admission refusals before the cycle escalates.
    pub max_refusals_per_step: u8,
}

impl Default for CycleDriverConfig {
    fn default() -> Self {
        Self {
            max_refusals_per_step: DEFAULT_MAX_REFUSALS_PER_STEP,
        }
    }
}

/// Drives one cycle under one lease: propose → admit → journal → execute →
/// observe → commit, until a terminal state, cancellation or an error.
pub struct CycleDriver<P, E> {
    store: Arc<CycleStore>,
    proposer: P,
    executor: E,
    reissuer: Arc<dyn AuthorityReissuer>,
    config: CycleDriverConfig,
}

impl<P: CycleProposer, E: CycleStepExecutor> CycleDriver<P, E> {
    #[must_use]
    pub fn new(
        store: Arc<CycleStore>,
        proposer: P,
        executor: E,
        reissuer: Arc<dyn AuthorityReissuer>,
    ) -> Self {
        Self {
            store,
            proposer,
            executor,
            reissuer,
            config: CycleDriverConfig::default(),
        }
    }

    #[must_use]
    pub fn with_config(mut self, config: CycleDriverConfig) -> Self {
        self.config = config;
        self
    }

    /// Runs `cycle_id` under the lease `token`.
    ///
    /// `current` is the admission the trusted runtime derives *now* (role
    /// matrix, effective IR, policy); the driver never uses more than the
    /// intersection of it with the persisted ceiling and the reissued
    /// authority.
    ///
    /// # Errors
    /// [`CycleRunError`] for store/fence failures (including a lost lease),
    /// reissue or resume refusals and rejected observations.
    pub async fn run(
        &mut self,
        cycle_id: &str,
        token: &LeaseToken,
        current: &CycleAdmission,
        cancel: &CancelToken,
    ) -> Result<CycleRunOutcome, CycleRunError> {
        let id = cycle_id.to_owned();
        let mut record = self.blocking(move |store| store.load(&id)).await?;
        if let Some(terminal) = record.checkpoint.terminal {
            return Ok(CycleRunOutcome::Terminal {
                terminal,
                checkpoint: record.checkpoint,
            });
        }
        let admission = if token.epoch == record.checkpoint.fence.epoch {
            // Same lease continuing: `admit_cycle` refuses a `current`
            // broader than the persisted ceiling (CeilingMismatch).
            current.clone()
        } else {
            let reissued = self
                .reissuer
                .reissue(&record.checkpoint.authority)
                .map_err(CycleRunError::Reissue)?;
            let (admission, resumed) =
                resume_admission(&record.checkpoint, current, &reissued, token.epoch)
                    .map_err(CycleRunError::Resume)?;
            let (id, lease) = (cycle_id.to_owned(), token.clone());
            record = self
                .blocking(move |store| store.rebind(&id, &lease, &resumed))
                .await?;
            self.executor.rebind_authority(&reissued);
            admission
        };
        if let Some(step) = record.in_flight.clone() {
            record.checkpoint = self
                .recover(cycle_id, token, &admission, &record.checkpoint, step)
                .await?;
            record.in_flight = None;
            if let Some(terminal) = record.checkpoint.terminal {
                return Ok(CycleRunOutcome::Terminal {
                    terminal,
                    checkpoint: record.checkpoint,
                });
            }
        }
        let mut checkpoint = record.checkpoint;
        // A state the policy refuses outright (narrowed chain depth, ceiling
        // mismatch) refuses every proposal: fail closed before any model call.
        check_state(&admission, &checkpoint).map_err(CycleRunError::Admission)?;
        let mut last_refusal = None;
        let mut refusals: u8 = 0;
        loop {
            if cancel.is_cancelled() {
                return Ok(CycleRunOutcome::Cancelled { checkpoint });
            }
            let proposed = tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    return Ok(CycleRunOutcome::Cancelled { checkpoint });
                }
                proposed = self.proposer.propose(&admission, &checkpoint, last_refusal) => proposed,
            };
            let proposal = match proposed {
                Ok(proposal) => proposal,
                Err(failure) if failure.retryable => {
                    return Err(CycleRunError::ProposerUnavailable(failure.reason));
                }
                Err(failure) => CycleProposal::Blocked {
                    reason: format!(
                        "proposer unavailable: {}",
                        failure.reason.chars().take(400).collect::<String>()
                    ),
                },
            };
            let admitted = match admit_cycle(&admission, &checkpoint, proposal) {
                Ok(admitted) => {
                    last_refusal = None;
                    refusals = 0;
                    admitted
                }
                Err(refusal) => {
                    refusals = refusals.saturating_add(1);
                    last_refusal = Some(refusal);
                    tracing::debug!(
                        cycle_id,
                        ?refusal,
                        refusals,
                        "intent cycle proposal refused"
                    );
                    if refusals < self.config.max_refusals_per_step {
                        continue;
                    }
                    let escalate = CycleProposal::Escalate {
                        reason: format!(
                            "admission refused {refusals} times in a row; last refusal: {refusal:?}"
                        ),
                    };
                    admit_cycle(&admission, &checkpoint, escalate)
                        .map_err(CycleRunError::Admission)?
                }
            };
            let (id, lease, journal) = (cycle_id.to_owned(), token.clone(), admitted.clone());
            let step = self
                .blocking(move |store| store.begin_step(&id, &lease, &journal))
                .await?;
            let observations = if admitted.terminal().is_some() {
                CycleObservations::default()
            } else {
                match self
                    .executor
                    .execute(&step, &admission, &checkpoint, cancel)
                    .await
                {
                    Ok(observations) => observations,
                    Err(failure) if cancel.is_cancelled() => {
                        // Leave the journal entry: the next lease reconciles.
                        tracing::info!(cycle_id, reason = %failure.reason, "intent cycle step interrupted");
                        return Ok(CycleRunOutcome::Cancelled { checkpoint });
                    }
                    Err(failure) => {
                        tracing::warn!(cycle_id, reason = %failure.reason, "intent cycle step failed without effect");
                        CycleObservations::default()
                    }
                }
            };
            checkpoint = self
                .commit(
                    cycle_id,
                    token,
                    &admission,
                    &checkpoint,
                    &admitted,
                    observations,
                )
                .await?;
            if let Some(terminal) = checkpoint.terminal {
                return Ok(CycleRunOutcome::Terminal {
                    terminal,
                    checkpoint,
                });
            }
        }
    }

    async fn recover(
        &mut self,
        cycle_id: &str,
        token: &LeaseToken,
        admission: &CycleAdmission,
        state: &CycleCheckpoint,
        step: InFlightStep,
    ) -> Result<CycleCheckpoint, CycleRunError> {
        match self.executor.reconcile(&step).await {
            StepReconciliation::NotStarted => {
                let (id, lease) = (cycle_id.to_owned(), token.clone());
                self.blocking(move |store| store.clear_step(&id, &lease, &step))
                    .await?;
                Ok(state.clone())
            }
            StepReconciliation::Completed(observations) => {
                let admitted = AdmittedCycle::recovered(step.proposal, step.sequence);
                self.commit(cycle_id, token, admission, state, &admitted, observations)
                    .await
            }
            StepReconciliation::Uncertain => {
                let escalate = CycleProposal::Escalate {
                    reason: format!(
                        "effect of journaled step {} is uncertain after recovery",
                        step.idempotency_key
                    ),
                };
                let admitted = AdmittedCycle::recovered(escalate, step.sequence);
                self.commit(
                    cycle_id,
                    token,
                    admission,
                    state,
                    &admitted,
                    CycleObservations::default(),
                )
                .await
            }
        }
    }

    async fn commit(
        &mut self,
        cycle_id: &str,
        token: &LeaseToken,
        admission: &CycleAdmission,
        state: &CycleCheckpoint,
        admitted: &AdmittedCycle,
        observations: CycleObservations,
    ) -> Result<CycleCheckpoint, CycleRunError> {
        let next = apply_observations(admission.intent(), state, admitted, observations)
            .map_err(CycleRunError::Observation)?;
        let (id, lease, candidate) = (cycle_id.to_owned(), token.clone(), next.clone());
        self.blocking(move |store| store.commit_step(&id, &lease, &candidate))
            .await?;
        tracing::info!(
            cycle_id,
            epoch = next.fence.epoch,
            sequence = next.fence.sequence,
            transitions = next.transitions_used,
            stall = next.stall_transitions,
            terminal = ?next.terminal,
            "intent cycle step committed"
        );
        Ok(next)
    }

    async fn blocking<T, F>(&mut self, operation: F) -> Result<T, CycleRunError>
    where
        T: Send + 'static,
        F: FnOnce(&CycleStore) -> Result<T, CycleStoreError> + Send + 'static,
    {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || operation(&store))
            .await
            .map_err(|error| CycleRunError::Join(error.to_string()))?
            .map_err(CycleRunError::Store)
    }
}

/// Future type driven by `DurableJobRunner` for a cycle job.
pub type CycleJobFuture = Pin<Box<dyn Future<Output = JobOutcome> + Send>>;

/// Cancellation control registered with the job execution registry.
struct CycleExecutionControl {
    cancel: CancelToken,
    completion: watch::Receiver<bool>,
}

impl ExecutionControl for CycleExecutionControl {
    fn request_graceful_cancel(&self) {
        self.cancel.cancel(CancelReason::User);
    }

    fn force_abort(&self) {
        // The driver stops at its next await point; a step left in flight
        // is reconciled by the next lease, so no state is stranded.
        self.cancel.cancel(CancelReason::User);
    }

    fn completion(&self) -> watch::Receiver<bool> {
        self.completion.clone()
    }
}

/// Builds the operation closure for `DurableJobRunner::run_with_cancel`.
///
/// The runner owns claim, heartbeat, lease-loss cancellation and the job
/// commit; the cycle's own fence is the claim's [`LeaseToken`].
pub fn cycle_job_operation<P, E>(
    mut driver: CycleDriver<P, E>,
    cycle_id: String,
    current: CycleAdmission,
) -> impl FnOnce(JobClaim, CancelToken) -> (Arc<dyn ExecutionControl>, CycleJobFuture)
where
    P: CycleProposer + 'static,
    E: CycleStepExecutor + 'static,
{
    move |claim, cancel| {
        let (done, completion) = watch::channel(false);
        let control: Arc<dyn ExecutionControl> = Arc::new(CycleExecutionControl {
            cancel: cancel.clone(),
            completion,
        });
        let future: CycleJobFuture = Box::pin(async move {
            let outcome = driver.run(&cycle_id, &claim.token, &current, &cancel).await;
            done.send_replace(true);
            job_outcome(&cycle_id, outcome)
        });
        (control, future)
    }
}

/// Maps a driver result onto the job ledger. `CompletionProposed` becomes a
/// succeeded job carrying a bounded summary; goal acceptance is not implied.
#[must_use]
pub fn job_outcome(cycle_id: &str, outcome: Result<CycleRunOutcome, CycleRunError>) -> JobOutcome {
    match outcome {
        Ok(CycleRunOutcome::Terminal {
            terminal,
            checkpoint,
        }) => {
            let reason = decision_reason(&checkpoint);
            match terminal {
                CycleTerminal::CompletionProposed => JobOutcome::Succeeded {
                    result: serde_json::json!({
                        "cycle_id": cycle_id,
                        "terminal": "completion_proposed",
                        "intent_id": checkpoint.intent_id,
                        "intent_revision": checkpoint.intent_revision,
                        "sequence": checkpoint.fence.sequence,
                        "criteria_met": checkpoint.criteria_met,
                        "requires_owner_acceptance": true,
                    }),
                },
                CycleTerminal::Escalated => JobOutcome::Blocked {
                    reason: format!("intent cycle escalated: {reason}"),
                },
                CycleTerminal::Blocked => JobOutcome::Blocked {
                    reason: format!("intent cycle blocked: {reason}"),
                },
                CycleTerminal::Failed => JobOutcome::Failed {
                    reason: format!("intent cycle failed: {reason}"),
                },
            }
        }
        Ok(CycleRunOutcome::Cancelled { .. }) => JobOutcome::Cancelled {
            reason: "intent cycle cancelled".to_owned(),
        },
        Err(error) => JobOutcome::Failed {
            reason: error.to_string(),
        },
    }
}

fn decision_reason(checkpoint: &CycleCheckpoint) -> String {
    match &checkpoint.last_decision {
        Some(
            CycleProposal::Escalate { reason }
            | CycleProposal::Blocked { reason }
            | CycleProposal::Failed { reason },
        ) => reason.clone(),
        _ => "no reason recorded".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::path::PathBuf;
    use std::sync::Mutex;

    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_core::{DurableJobRunner, JobExecutionRegistry};
    use harw_job_core::{Budget, Job, JobKind, JobScope, RetryPolicy, StoredJob};
    use harw_session_store::{ClaimRequest, JobStore};
    use harw_types::{ApprovalActor, ContentDigest, TenantId, WorkspaceId};
    use jiff::{SignedDuration, Timestamp};

    use super::*;
    use crate::intent_cycle::{
        CycleLimits, CycleTargets, EvidenceRecord, EvidenceSourceKind, EvidenceTrust,
        IntentBinding, Segment,
    };
    use crate::test_support::{TestError, TestResult, ctx};

    fn authority(permissions: &[Permission]) -> TestResult<AuthorityContext> {
        let harness_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or(TestError::Missing(
                "harw-plan-bridge has a workspace parent",
            ))?
            .to_path_buf();
        let tenant = TenantId::from_str("cycle-tenant");
        let workspace = WorkspaceId::from_str("cycle-runtime-tests");
        let binding = WorkspaceRegistry::build(
            &harness_root,
            [WorkspaceRegistration {
                tenant: tenant.clone(),
                workspace: workspace.clone(),
                root: PathBuf::from("harw-plan-bridge"),
            }],
        )
        .map_err(ctx("workspace registers"))?
        .resolve(&tenant, &workspace)
        .map_err(ctx("workspace resolves"))?;
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy(permissions.iter().copied()),
        )
        .authority()
        .clone())
    }

    /// Stands in for `PolicyBootstrap::reissue`: hands out a fixed context.
    struct FixedReissuer(AuthorityContext);

    impl AuthorityReissuer for FixedReissuer {
        fn reissue(&self, _snapshot: &AuthoritySnapshot) -> Result<AuthorityContext, String> {
            Ok(self.0.clone())
        }
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn admission() -> TestResult<CycleAdmission> {
        CycleAdmission::new(
            IntentBinding {
                id: "intent-w02".to_owned(),
                revision: 1,
                digest: ContentDigest::of(b"intent-w02@1"),
                predecessor: None,
                acceptance: set(&["tested"]),
            },
            CycleLimits {
                max_transitions: 10,
                max_chain_depth: 1,
                max_spawn_depth: 1,
                max_parallel_segments: 2,
                max_stall_transitions: 3,
            },
            CycleTargets {
                read: set(&["source"]),
                write: set(&["code.rs"]),
                children: set(&["explorer"]),
                recipes: set(&["hypothesis"]),
            },
        )
        .map_err(|_| TestError::Missing("admission builds"))
    }

    fn rights() -> TestResult<AuthorityContext> {
        authority(&[Permission::ReadWorkspace, Permission::WriteWorkspace])
    }

    fn token(work_id: &WorkId, epoch: u64) -> LeaseToken {
        LeaseToken {
            work_id: work_id.clone(),
            epoch,
            nonce: format!("nonce-{epoch}"),
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        store: Arc<CycleStore>,
        work_id: WorkId,
        admission: CycleAdmission,
    }

    fn fixture(cycle_id: &str) -> TestResult<Fixture> {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let records = RecordStore::create_ambient(&temp.path().join("cycles"))
            .map_err(ctx("cycle record store"))?;
        let store = Arc::new(CycleStore::new(records));
        let admission = admission()?;
        let work_id = WorkId::from_str("job-w02");
        let checkpoint = CycleCheckpoint::initial(&admission, &rights()?, 0);
        let record = CycleRecord::new(cycle_id, work_id.clone(), checkpoint)
            .map_err(ctx("record builds"))?;
        store.create(&record).map_err(ctx("record persists"))?;
        Ok(Fixture {
            _temp: temp,
            store,
            work_id,
            admission,
        })
    }

    fn verified(id: &str, content: &str) -> EvidenceRecord {
        EvidenceRecord {
            id: id.to_owned(),
            source: EvidenceSourceKind::Verification,
            locator: format!("gate:{id}"),
            digest: ContentDigest::of(content.as_bytes()),
            trust: EvidenceTrust::Verified,
        }
    }

    fn wait() -> CycleProposal {
        CycleProposal::Wait {
            reason: "observe".to_owned(),
        }
    }

    /// Replays scripted proposals; records the refusals it was shown.
    struct Script {
        proposals: VecDeque<Result<CycleProposal, StepFailure>>,
        refusals: Arc<Mutex<Vec<Option<CycleRefusal>>>>,
    }

    impl Script {
        fn new(proposals: Vec<Result<CycleProposal, StepFailure>>) -> Self {
            Self {
                proposals: proposals.into(),
                refusals: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl CycleProposer for Script {
        fn propose(
            &mut self,
            _admission: &CycleAdmission,
            _state: &CycleCheckpoint,
            last_refusal: Option<CycleRefusal>,
        ) -> impl Future<Output = Result<CycleProposal, StepFailure>> + Send {
            if let Ok(mut seen) = self.refusals.lock() {
                seen.push(last_refusal);
            }
            let next = self
                .proposals
                .pop_front()
                .unwrap_or_else(|| Err(StepFailure::new("script exhausted")));
            async move { next }
        }
    }

    /// Executes by handing out scripted observations; records keys.
    struct Executor {
        results: VecDeque<Result<CycleObservations, StepFailure>>,
        reconciliation: StepReconciliation,
        executed: Arc<Mutex<Vec<String>>>,
        rebinds: Arc<Mutex<u32>>,
    }

    impl Executor {
        fn new(results: Vec<Result<CycleObservations, StepFailure>>) -> Self {
            Self {
                results: results.into(),
                reconciliation: StepReconciliation::NotStarted,
                executed: Arc::new(Mutex::new(Vec::new())),
                rebinds: Arc::new(Mutex::new(0)),
            }
        }
    }

    impl CycleStepExecutor for Executor {
        fn rebind_authority(&mut self, _authority: &AuthorityContext) {
            if let Ok(mut rebinds) = self.rebinds.lock() {
                *rebinds += 1;
            }
        }

        fn execute(
            &mut self,
            step: &InFlightStep,
            _admission: &CycleAdmission,
            _state: &CycleCheckpoint,
            _cancel: &CancelToken,
        ) -> impl Future<Output = Result<CycleObservations, StepFailure>> + Send {
            if let Ok(mut executed) = self.executed.lock() {
                executed.push(step.idempotency_key.clone());
            }
            let next = self
                .results
                .pop_front()
                .unwrap_or_else(|| Ok(CycleObservations::default()));
            async move { next }
        }

        fn reconcile(
            &mut self,
            _step: &InFlightStep,
        ) -> impl Future<Output = StepReconciliation> + Send {
            let outcome = self.reconciliation.clone();
            async move { outcome }
        }
    }

    fn tested_observations() -> CycleObservations {
        CycleObservations {
            evidence: vec![verified("gate-1", "cargo test green")],
            claims: BTreeMap::new(),
            criteria: BTreeMap::from([("tested".to_owned(), "gate-1".to_owned())]),
        }
    }

    fn driver(
        fixture: &Fixture,
        proposer: Script,
        executor: Executor,
        reissued: AuthorityContext,
    ) -> CycleDriver<Script, Executor> {
        CycleDriver::new(
            Arc::clone(&fixture.store),
            proposer,
            executor,
            Arc::new(FixedReissuer(reissued)),
        )
    }

    #[tokio::test]
    async fn driver_runs_to_a_proposed_completion_and_persists_it() -> TestResult {
        let fx = fixture("cycle-happy")?;
        let proposer = Script::new(vec![
            Ok(CycleProposal::Advance {
                segments: vec![Segment::Read {
                    target: "source".to_owned(),
                }],
            }),
            Ok(CycleProposal::Complete {}),
        ]);
        let executor = Executor::new(vec![Ok(tested_observations())]);
        let executed = Arc::clone(&executor.executed);
        let mut driver = driver(&fx, proposer, executor, rights()?);
        let outcome = driver
            .run(
                "cycle-happy",
                &token(&fx.work_id, 1),
                &fx.admission,
                &CancelToken::new(),
            )
            .await
            .map_err(ctx("driver runs"))?;
        let CycleRunOutcome::Terminal {
            terminal,
            checkpoint,
        } = outcome
        else {
            return Err(TestError::Missing("terminal outcome"));
        };
        assert_eq!(terminal, CycleTerminal::CompletionProposed);
        assert_eq!(checkpoint.fence.epoch, 1, "first claim rebinds epoch 0 → 1");
        assert_eq!(checkpoint.fence.sequence, 2);
        let stored = fx.store.load("cycle-happy").map_err(ctx("reload"))?;
        assert_eq!(stored.checkpoint, checkpoint);
        assert_eq!(stored.in_flight, None);
        assert_eq!(stored.status().criteria_covered, vec!["tested".to_owned()]);
        let keys = executed
            .lock()
            .map_err(|_| TestError::Missing("lock"))?
            .clone();
        assert_eq!(
            keys,
            vec!["cycle-happy-0".to_owned()],
            "terminal steps do not execute"
        );
        Ok(())
    }

    #[tokio::test]
    async fn repeated_refusals_are_fed_back_then_escalate() -> TestResult {
        let fx = fixture("cycle-refused")?;
        let patch = || {
            Ok(CycleProposal::ProposePatch {
                targets: set(&["secrets.toml"]),
                evidence_id: "none".to_owned(),
            })
        };
        let proposer = Script::new(vec![patch(), patch(), patch()]);
        let seen = Arc::clone(&proposer.refusals);
        let mut driver = driver(&fx, proposer, Executor::new(vec![]), rights()?);
        let outcome = driver
            .run(
                "cycle-refused",
                &token(&fx.work_id, 1),
                &fx.admission,
                &CancelToken::new(),
            )
            .await
            .map_err(ctx("driver runs"))?;
        let job = job_outcome("cycle-refused", Ok(outcome));
        let JobOutcome::Blocked { reason } = job else {
            return Err(TestError::Missing("escalation maps to a blocked job"));
        };
        assert!(reason.contains("WriteNotAllowed"), "{reason}");
        let seen = seen.lock().map_err(|_| TestError::Missing("lock"))?.clone();
        assert_eq!(
            seen,
            vec![
                None,
                Some(CycleRefusal::WriteNotAllowed),
                Some(CycleRefusal::WriteNotAllowed)
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn unavailable_proposer_blocks_instead_of_looping() -> TestResult {
        let fx = fixture("cycle-403")?;
        let proposer = Script::new(vec![Err(StepFailure::new("HTTP 403 model route"))]);
        let mut driver = driver(&fx, proposer, Executor::new(vec![]), rights()?);
        let outcome = driver
            .run(
                "cycle-403",
                &token(&fx.work_id, 1),
                &fx.admission,
                &CancelToken::new(),
            )
            .await
            .map_err(ctx("driver runs"))?;
        assert_eq!(
            job_outcome("cycle-403", Ok(outcome)),
            JobOutcome::Blocked {
                reason: "intent cycle blocked: proposer unavailable: HTTP 403 model route"
                    .to_owned()
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn retryable_proposer_outage_commits_no_terminal_state() -> TestResult {
        let fx = fixture("cycle-503")?;
        let proposer = Script::new(vec![Err(StepFailure::retryable("model route: 503"))]);
        let mut first = driver(&fx, proposer, Executor::new(vec![]), rights()?);
        let result = first
            .run(
                "cycle-503",
                &token(&fx.work_id, 1),
                &fx.admission,
                &CancelToken::new(),
            )
            .await;
        let Err(error) = result else {
            return Err(TestError::Missing("retryable outage must be an error"));
        };
        assert!(matches!(error, CycleRunError::ProposerUnavailable(_)));
        assert!(matches!(
            job_outcome("cycle-503", Err(error)),
            JobOutcome::Failed { .. }
        ));
        let record = fx.store.load("cycle-503").map_err(ctx("record loads"))?;
        assert_eq!(record.checkpoint.terminal, None, "cycle stays resumable");
        // A later lease can still run the cycle to completion.
        let proposer = Script::new(vec![Ok(CycleProposal::Complete {})]);
        let mut again = driver(&fx, proposer, Executor::new(vec![]), rights()?);
        let outcome = again
            .run(
                "cycle-503",
                &token(&fx.work_id, 2),
                &fx.admission,
                &CancelToken::new(),
            )
            .await
            .map_err(ctx("second lease runs"))?;
        assert!(matches!(outcome, CycleRunOutcome::Terminal { .. }));
        Ok(())
    }

    #[tokio::test]
    async fn refused_state_fails_closed_before_any_model_call() -> TestResult {
        let fx = fixture("cycle-deep")?;
        let mut record = fx.store.load("cycle-deep").map_err(ctx("record loads"))?;
        record.checkpoint.chain_depth = fx.admission.limits().max_chain_depth + 1;
        fx.store.create(&CycleRecord::new(
            "cycle-deep-2",
            fx.work_id.clone(),
            record.checkpoint,
        )
        .map_err(ctx("record builds"))?)
        .map_err(ctx("record persists"))?;
        let proposer = Script::new(vec![Ok(wait()), Ok(wait()), Ok(wait())]);
        let seen = Arc::clone(&proposer.refusals);
        let mut driver = driver(&fx, proposer, Executor::new(vec![]), rights()?);
        let result = driver
            .run(
                "cycle-deep-2",
                &token(&fx.work_id, 1),
                &fx.admission,
                &CancelToken::new(),
            )
            .await;
        assert!(matches!(
            result,
            Err(CycleRunError::Admission(CycleRefusal::ChainDepthExceeded))
        ));
        assert!(
            seen.lock().map_err(|_| TestError::Missing("lock"))?.is_empty(),
            "the proposer must not be called"
        );
        Ok(())
    }

    /// A proposer whose model call never resolves.
    struct Hung;

    impl CycleProposer for Hung {
        fn propose(
            &mut self,
            _admission: &CycleAdmission,
            _state: &CycleCheckpoint,
            _last_refusal: Option<CycleRefusal>,
        ) -> impl Future<Output = Result<CycleProposal, StepFailure>> + Send {
            std::future::pending()
        }
    }

    #[tokio::test]
    async fn cancel_interrupts_a_hung_proposer() -> TestResult {
        let fx = fixture("cycle-hung")?;
        let mut driver = CycleDriver::new(
            Arc::clone(&fx.store),
            Hung,
            Executor::new(vec![]),
            Arc::new(FixedReissuer(rights()?)),
        );
        let cancel = CancelToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            trigger.cancel(CancelReason::User);
        });
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            driver.run("cycle-hung", &token(&fx.work_id, 1), &fx.admission, &cancel),
        )
        .await
        .map_err(|_| TestError::Missing("driver must not hang"))?
        .map_err(ctx("driver runs"))?;
        assert!(matches!(outcome, CycleRunOutcome::Cancelled { .. }));
        Ok(())
    }

    #[tokio::test]
    async fn failing_executor_stalls_out_and_escalates() -> TestResult {
        let fx = fixture("cycle-stall")?;
        let proposer = Script::new((0..8).map(|_| Ok(wait())).collect());
        let executor = Executor::new(
            (0..8)
                .map(|_| Err(StepFailure::new("egress denied")))
                .collect(),
        );
        let mut driver = driver(&fx, proposer, executor, rights()?);
        let outcome = driver
            .run(
                "cycle-stall",
                &token(&fx.work_id, 1),
                &fx.admission,
                &CancelToken::new(),
            )
            .await
            .map_err(ctx("driver runs"))?;
        let CycleRunOutcome::Terminal {
            terminal,
            checkpoint,
        } = outcome
        else {
            return Err(TestError::Missing("terminal outcome"));
        };
        assert_eq!(terminal, CycleTerminal::Escalated);
        // Three stalled steps, then the escalation step.
        assert_eq!(checkpoint.transitions_used, 4);
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_stops_between_steps() -> TestResult {
        let fx = fixture("cycle-cancel")?;
        let cancel = CancelToken::new();
        cancel.cancel(CancelReason::User);
        let mut driver = driver(&fx, Script::new(vec![]), Executor::new(vec![]), rights()?);
        let outcome = driver
            .run(
                "cycle-cancel",
                &token(&fx.work_id, 1),
                &fx.admission,
                &cancel,
            )
            .await
            .map_err(ctx("driver runs"))?;
        assert!(matches!(outcome, CycleRunOutcome::Cancelled { .. }));
        assert_eq!(
            job_outcome("cycle-cancel", Ok(outcome)),
            JobOutcome::Cancelled {
                reason: "intent cycle cancelled".to_owned()
            }
        );
        Ok(())
    }

    /// Journals a step under epoch 1 and "crashes" before committing.
    fn crash_mid_step(fx: &Fixture, cycle_id: &str) -> TestResult<InFlightStep> {
        let lease = token(&fx.work_id, 1);
        let record = fx.store.load(cycle_id).map_err(ctx("load"))?;
        let reissued = rights()?;
        let (admission, resumed) =
            resume_admission(&record.checkpoint, &fx.admission, &reissued, 1)
                .map_err(|_| TestError::Missing("resume"))?;
        fx.store
            .rebind(cycle_id, &lease, &resumed)
            .map_err(ctx("rebind"))?;
        let admitted =
            admit_cycle(&admission, &resumed, wait()).map_err(|_| TestError::Missing("admit"))?;
        fx.store
            .begin_step(cycle_id, &lease, &admitted)
            .map_err(ctx("begin"))
    }

    #[tokio::test]
    async fn recovery_commits_a_completed_journaled_step() -> TestResult {
        let fx = fixture("cycle-recover")?;
        crash_mid_step(&fx, "cycle-recover")?;
        let mut executor = Executor::new(vec![]);
        executor.reconciliation = StepReconciliation::Completed(tested_observations());
        let executed = Arc::clone(&executor.executed);
        let proposer = Script::new(vec![Ok(CycleProposal::Complete {})]);
        let mut driver = driver(&fx, proposer, executor, rights()?);
        let outcome = driver
            .run(
                "cycle-recover",
                &token(&fx.work_id, 2),
                &fx.admission,
                &CancelToken::new(),
            )
            .await
            .map_err(ctx("driver resumes"))?;
        assert!(matches!(
            outcome,
            CycleRunOutcome::Terminal {
                terminal: CycleTerminal::CompletionProposed,
                ..
            }
        ));
        assert!(
            executed
                .lock()
                .map_err(|_| TestError::Missing("lock"))?
                .is_empty(),
            "a completed journaled step is never executed twice"
        );
        Ok(())
    }

    #[tokio::test]
    async fn recovery_escalates_an_uncertain_effect() -> TestResult {
        let fx = fixture("cycle-uncertain")?;
        let step = crash_mid_step(&fx, "cycle-uncertain")?;
        let mut executor = Executor::new(vec![]);
        executor.reconciliation = StepReconciliation::Uncertain;
        let mut driver = driver(&fx, Script::new(vec![]), executor, rights()?);
        let outcome = driver
            .run(
                "cycle-uncertain",
                &token(&fx.work_id, 2),
                &fx.admission,
                &CancelToken::new(),
            )
            .await
            .map_err(ctx("driver resumes"))?;
        let JobOutcome::Blocked { reason } = job_outcome("cycle-uncertain", Ok(outcome)) else {
            return Err(TestError::Missing("uncertain effect blocks"));
        };
        assert!(reason.contains(&step.idempotency_key), "{reason}");
        Ok(())
    }

    #[tokio::test]
    async fn recovery_drops_a_step_that_never_started() -> TestResult {
        let fx = fixture("cycle-notstarted")?;
        crash_mid_step(&fx, "cycle-notstarted")?;
        let proposer = Script::new(vec![Ok(CycleProposal::Failed {
            reason: "give up".to_owned(),
        })]);
        let mut driver = driver(&fx, proposer, Executor::new(vec![]), rights()?);
        let outcome = driver
            .run(
                "cycle-notstarted",
                &token(&fx.work_id, 2),
                &fx.admission,
                &CancelToken::new(),
            )
            .await
            .map_err(ctx("driver resumes"))?;
        let CycleRunOutcome::Terminal { checkpoint, .. } = outcome else {
            return Err(TestError::Missing("terminal"));
        };
        assert_eq!(
            checkpoint.transitions_used, 1,
            "the dropped step is not counted"
        );
        assert_eq!(checkpoint.fence.epoch, 2);
        Ok(())
    }

    #[tokio::test]
    async fn takeover_rebinds_the_executor_to_the_reissued_authority_once() -> TestResult {
        let fx = fixture("cycle-rebind")?;
        crash_mid_step(&fx, "cycle-rebind")?;
        let proposer = Script::new(vec![Ok(CycleProposal::Failed {
            reason: "stop".to_owned(),
        })]);
        let executor = Executor::new(vec![]);
        let rebinds = Arc::clone(&executor.rebinds);
        let mut driver = driver(&fx, proposer, executor, rights()?);
        driver
            .run(
                "cycle-rebind",
                &token(&fx.work_id, 2),
                &fx.admission,
                &CancelToken::new(),
            )
            .await
            .map_err(ctx("driver resumes"))?;
        assert_eq!(rebinds.lock().map(|n| *n).ok(), Some(1));
        Ok(())
    }

    #[tokio::test]
    async fn continuing_lease_does_not_rebind() -> TestResult {
        let fx = fixture("cycle-norebind")?;
        let proposer = Script::new(vec![Ok(CycleProposal::Failed {
            reason: "stop".to_owned(),
        })]);
        let executor = Executor::new(vec![]);
        let rebinds = Arc::clone(&executor.rebinds);
        let mut driver = driver(&fx, proposer, executor, rights()?);
        driver
            .run(
                "cycle-norebind",
                &token(&fx.work_id, 0),
                &fx.admission,
                &CancelToken::new(),
            )
            .await
            .map_err(ctx("driver runs"))?;
        assert_eq!(rebinds.lock().map(|n| *n).ok(), Some(0));
        Ok(())
    }

    #[test]
    fn store_fences_stale_and_foreign_leases() -> TestResult {
        let fx = fixture("cycle-fence")?;
        let step = crash_mid_step(&fx, "cycle-fence")?;
        let stale = token(&fx.work_id, 0);
        let fresh = token(&fx.work_id, 1);
        let newer = token(&fx.work_id, 2);
        let foreign = token(&WorkId::from_str("other-job"), 1);
        let record = fx.store.load("cycle-fence").map_err(ctx("load"))?;
        let admitted = AdmittedCycle::recovered(wait(), record.checkpoint.fence.sequence);
        assert!(matches!(
            fx.store.begin_step("cycle-fence", &fresh, &admitted),
            Err(CycleStoreError::StepInFlight)
        ));
        assert!(matches!(
            fx.store.clear_step("cycle-fence", &stale, &step),
            Err(CycleStoreError::Fence(FenceRefusal::StaleEpoch))
        ));
        assert!(matches!(
            fx.store.clear_step("cycle-fence", &newer, &step),
            Err(CycleStoreError::NotRebound)
        ));
        assert!(matches!(
            fx.store.clear_step("cycle-fence", &foreign, &step),
            Err(CycleStoreError::WrongJob)
        ));
        let next = apply_observations(
            fx.admission.intent(),
            &record.checkpoint,
            &admitted,
            CycleObservations::default(),
        )
        .map_err(|_| TestError::Missing("apply"))?;
        fx.store
            .commit_step("cycle-fence", &fresh, &next)
            .map_err(ctx("commit under the fresh lease"))?;
        // The zombie path: epoch 2 took over; epoch 1 can no longer write.
        let reissued = rights()?;
        let (_, taken_over) = resume_admission(&next, &fx.admission, &reissued, 2)
            .map_err(|_| TestError::Missing("resume"))?;
        fx.store
            .rebind("cycle-fence", &newer, &taken_over)
            .map_err(ctx("takeover"))?;
        let late = AdmittedCycle::recovered(wait(), next.fence.sequence);
        assert!(matches!(
            fx.store.begin_step("cycle-fence", &fresh, &late),
            Err(CycleStoreError::Fence(FenceRefusal::StaleEpoch))
        ));
        assert!(matches!(
            fx.store.commit_step("cycle-fence", &fresh, &next),
            Err(CycleStoreError::Fence(FenceRefusal::StaleEpoch))
        ));
        Ok(())
    }

    #[test]
    fn rebind_refuses_tampering_and_widening() -> TestResult {
        let fx = fixture("cycle-rebind")?;
        let lease = token(&fx.work_id, 1);
        let record = fx.store.load("cycle-rebind").map_err(ctx("load"))?;
        let reissued = rights()?;
        let (_, resumed) = resume_admission(&record.checkpoint, &fx.admission, &reissued, 1)
            .map_err(|_| TestError::Missing("resume"))?;
        let mut tampered = resumed.clone();
        tampered.transitions_used = 0;
        tampered.stall_transitions = 0;
        tampered
            .criteria_met
            .insert("tested".to_owned(), "forged".to_owned());
        assert!(matches!(
            fx.store.rebind("cycle-rebind", &lease, &tampered),
            Err(CycleStoreError::Fence(FenceRefusal::StateTampered))
        ));
        let mut widened = resumed.clone();
        widened
            .ceiling
            .children
            .insert("security-reviewer".to_owned());
        assert!(matches!(
            fx.store.rebind("cycle-rebind", &lease, &widened),
            Err(CycleStoreError::Fence(FenceRefusal::CeilingWidened))
        ));
        assert!(matches!(
            fx.store
                .rebind("cycle-rebind", &token(&fx.work_id, 7), &resumed),
            Err(CycleStoreError::EpochMismatch)
        ));
        fx.store
            .rebind("cycle-rebind", &lease, &resumed)
            .map_err(ctx("valid rebind"))?;
        assert!(matches!(
            fx.store.rebind("cycle-rebind", &lease, &resumed),
            Err(CycleStoreError::Fence(FenceRefusal::StaleEpoch))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn narrowed_reissue_drops_write_scope_for_the_driver() -> TestResult {
        let fx = fixture("cycle-narrow")?;
        // Policy was downgraded between submit and claim: reads only.
        let read_only = authority(&[Permission::ReadWorkspace])?;
        let patch = || {
            Ok(CycleProposal::ProposePatch {
                targets: set(&["code.rs"]),
                evidence_id: "gate-1".to_owned(),
            })
        };
        let proposer = Script::new(vec![patch(), patch(), patch()]);
        let seen = Arc::clone(&proposer.refusals);
        let mut driver = driver(&fx, proposer, Executor::new(vec![]), read_only);
        driver
            .run(
                "cycle-narrow",
                &token(&fx.work_id, 1),
                &fx.admission,
                &CancelToken::new(),
            )
            .await
            .map_err(ctx("driver runs"))?;
        let seen = seen.lock().map_err(|_| TestError::Missing("lock"))?.clone();
        assert_eq!(seen.get(1), Some(&Some(CycleRefusal::WriteNotAllowed)));
        let stored = fx.store.load("cycle-narrow").map_err(ctx("reload"))?;
        assert!(stored.checkpoint.ceiling.write.is_empty());
        assert!(
            !stored
                .checkpoint
                .authority
                .request()
                .contains(Permission::WriteWorkspace)
        );
        Ok(())
    }

    #[test]
    fn record_and_store_reject_invalid_input() -> TestResult {
        let fx = fixture("cycle-valid")?;
        let checkpoint = fx
            .store
            .load("cycle-valid")
            .map_err(ctx("load"))?
            .checkpoint;
        assert!(matches!(
            CycleRecord::new("../escape", fx.work_id.clone(), checkpoint.clone()),
            Err(CycleStoreError::InvalidRecord)
        ));
        let record = CycleRecord::new("cycle-valid", fx.work_id.clone(), checkpoint)
            .map_err(ctx("record"))?;
        assert!(matches!(
            fx.store.create(&record),
            Err(CycleStoreError::Store(StoreError::AlreadyExists { .. }))
        ));
        Ok(())
    }

    fn stored_job(id: &str) -> TestResult<StoredJob> {
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
        job.mark_ready(now).map_err(ctx("job is ready"))?;
        Ok(StoredJob {
            job,
            scope: JobScope::new(
                TenantId::from_str("cycle-tenant"),
                WorkspaceId::from_str("cycle-runtime-tests"),
                ApprovalActor::Operator {
                    id: "operator".to_owned(),
                },
            ),
            input: serde_json::json!({"cycle_id": "cycle-job"}),
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

    #[tokio::test]
    async fn durable_job_runner_drives_a_cycle_end_to_end() -> TestResult {
        let fx = fixture("cycle-job")?;
        let jobs_dir = tempfile::tempdir().map_err(ctx("jobs tempdir"))?;
        let jobs = Arc::new(JobStore::new(jobs_dir.path()));
        jobs.admit(&stored_job("job-w02")?)
            .map_err(ctx("admit job"))?;
        let runner =
            DurableJobRunner::new(Arc::clone(&jobs), Arc::new(JobExecutionRegistry::new()));
        let proposer = Script::new(vec![
            Ok(CycleProposal::Fork {
                branches: vec![
                    Segment::Read {
                        target: "source".to_owned(),
                    },
                    Segment::Child {
                        target: "explorer".to_owned(),
                    },
                ],
                join: crate::intent_cycle::JoinPolicy::All {},
            }),
            Ok(CycleProposal::Complete {}),
        ]);
        let executor = Executor::new(vec![Ok(tested_observations())]);
        let operation = cycle_job_operation(
            driver(&fx, proposer, executor, rights()?),
            "cycle-job".to_owned(),
            fx.admission.clone(),
        );
        let request = ClaimRequest {
            worker_id: "cycle-runner".to_owned(),
            lease_ttl: SignedDuration::from_secs(60),
            now: Timestamp::now(),
        };
        let completion = runner
            .run_with_cancel(&fx.work_id, &request, operation)
            .await
            .map_err(ctx("job runs"))?;
        let JobOutcome::Succeeded { result } = completion.outcome else {
            return Err(TestError::Missing("cycle job succeeds"));
        };
        assert_eq!(result["terminal"], "completion_proposed");
        assert_eq!(result["requires_owner_acceptance"], true);
        let stored = fx.store.load("cycle-job").map_err(ctx("reload"))?;
        assert!(
            stored.checkpoint.fence.epoch >= 1,
            "fenced under the job lease epoch"
        );
        Ok(())
    }
}
