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
//! - **Lease liveness:** the epoch fence only refuses an *older*
//!   incarnation. Between the expiry of a job lease and the reclaim that
//!   rebinds the cycle, the expired lease's epoch is still the record's
//!   epoch. A store built [`with_lease_liveness`](CycleStore::with_lease_liveness)
//!   therefore *holds* the job lease for every fenced operation
//!   ([`LeaseLivenessGuard`]): it verifies that the presented
//!   [`LeaseToken`] is the current, non-expired claim and keeps the job
//!   record from being reclaimed, cancelled or renewed until the cycle
//!   operation — read or locked write — has finished. Because a lock cannot
//!   stop the clock, the operation's very last step asks the hold whether
//!   its deadline has passed ([`LeaseHold::still_live`]): for a write after
//!   the new record is synced to its temp file, right before the atomic
//!   rename that publishes it. Lock order: job lease
//!   first, cycle record second, released in reverse. A refusal is
//!   [`CycleStoreError::LeaseNotLive`] and reads or writes nothing.
//! - **Effect journal:** before an admitted step runs, it is journaled as
//!   [`InFlightStep`] with a stable idempotency key. A [`JournaledStep`]
//!   ([`CycleStore::journaled_step`]) is the store's proof that a step and
//!   its checkpoint are the ones journaled under the current lease; trusted
//!   follow-up admissions (`PatchProposal::from_admitted`) take only that
//!   proof, never caller-built steps or checkpoints. After a crash the next
//!   lease asks the executor to [reconcile](CycleStepExecutor::reconcile) it:
//!   not started → dropped, completed → its observations are committed,
//!   uncertain → the cycle escalates instead of guessing or repeating.
//!   An executor reporting [`StepFailure::uncertain`] also retains the journal
//!   without requiring a crash or cancellation: the job blocks, and an explicit
//!   retry must reconcile the original key before proposing anything else.
//! - **Driver:** [`CycleDriver`] separates the model ([`CycleProposer`],
//!   proposes only) from trusted execution ([`CycleStepExecutor`]). Refused
//!   proposals are fed back a bounded number of times, then the cycle
//!   escalates; an unavailable proposer (provider 403, offline model) is a
//!   capability blocker and ends the cycle as `Blocked`, never a retry loop.
//!   The job's cancel token is handed to the proposer (so an in-flight model
//!   call aborts) and additionally raced by the driver as a second guard.
//! - **Jobs:** [`cycle_job_operation`] plugs a driver into
//!   `harw_core::DurableJobRunner`, which owns claim, heartbeat, lease-loss
//!   cancellation and the terminal job commit.
//! - **Observations (W03c):** the driver classifies what went wrong in a
//!   step (refused proposals, executor and proposer failures, an uncertain
//!   recovery, unmet or stale verification claims) into typed, text-free
//!   [`RunObservationV1`]s. They are written into the record's bounded
//!   [`Outbox`] in the *same locked write* as the step's checkpoint
//!   ([`CycleStore::commit_step_observed`]), so an observation exists exactly
//!   when its transition does. Append and acknowledgement are epoch-fenced
//!   like every other write; at most
//!   [`MAX_OUTBOX_PENDING`](crate::cycle_outbox::MAX_OUTBOX_PENDING) are
//!   pending, newer ones are dropped and counted in
//!   [`CycleRecord::observations_dropped`] but never refuse the checkpoint
//!   commit. The consumer is a job-local drain (wave 4) that reads
//!   [`CycleStore::pending_observations`], records its decision with
//!   [`CycleStore::decide_observations`] (two-phase delivery: entries it
//!   will deliver are marked [`DeliveryState::Accepted`] and stay until
//!   their delivery is durable, the rest is acknowledged at once) and
//!   acknowledges delivered keys with [`CycleStore::ack_observations`], all
//!   under its lease: the read is
//!   fenced like a write, so a stale, foreign or not yet rebound lease never
//!   sees a payload (and, with a lease guard, neither does an expired,
//!   cancelled or reclaimed one). Redelivery after a crash is idempotent. A lease that finds the
//!   cycle already terminal still takes the record over (only the fence
//!   epoch moves, see [`CycleDriver::run`]), so observations committed with
//!   the terminal step but never acknowledged are drained by it rather than
//!   left pending forever. Proposer
//!   failures are classified apart from executor failures
//!   ([`classify_proposer_failure`]): no executor ran, so they are never a
//!   tool failure. Fence refusals cannot be recorded in the record they are
//!   refused for, so the driver only logs them (classified via
//!   [`classify_fence`]).
//!
//! Not in this module: which tools, child agents or model routes an executor
//! uses (W03+), and goal acceptance — a `CompletionProposed` cycle is a
//! succeeded *job*, not an achieved goal.

use std::fmt;
use std::future::Future;
use std::path::Path;
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

use crate::cycle_observation::{
    CauseCode, ObservationCategory, RunObservationV1, build_observation, classify_fence,
    classify_proposer_failure, classify_reconciliation, classify_refusal, classify_step_failure,
    classify_verification,
};
use crate::cycle_outbox::{DeliveryState, Outbox, OutboxEntry, OutboxError};
use crate::intent_cycle::{
    AdmittedCycle, CycleAdmission, CycleCheckpoint, CycleObservations, CycleProposal, CycleRefusal,
    CycleTerminal, FenceRefusal, ObservationError, ResumeRefusal, admit_cycle, apply_observations,
    check_commit, check_rebind, check_state, resume_admission,
};

/// Schema version of [`CycleRecord`].
pub const CYCLE_RECORD_SCHEMA: u32 = 1;

/// `JobKind::Custom` discriminator of an intent-cycle job. Shared by
/// `intent_cycle.start` (harw-ops), the job worker (harw-cli) and
/// `harw_runtime::job_ledger`.
pub const INTENT_CYCLE_JOB_KIND: &str = "intent_cycle";

/// Directory of the [`CycleStore`] below the durable job root
/// (`<job-root>/intent_cycle/`), next to the job store's own sidecars.
pub const CYCLE_STORE_DIR: &str = "intent_cycle";

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

/// Proof that `step` is the in-flight step the store journaled for `cycle_id`
/// under the caller's current lease. Only [`CycleStore::journaled_step`] builds it.
///
/// Every field is private and there is no `new`, no `Default` and no
/// `Deserialize`: an [`InFlightStep`] or [`CycleCheckpoint`] a caller built
/// (or deserialized) cannot be turned into one, so evidence that only the
/// caller holds never reaches a consumer of this proof. `Clone` merely copies
/// a proof the store already issued. It is a snapshot taken at read time:
/// consumers that act on it still present their lease to every store write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournaledStep {
    cycle_id: String,
    step: InFlightStep,
    checkpoint: CycleCheckpoint,
}

impl JournaledStep {
    /// The cycle the step belongs to.
    #[must_use]
    pub fn cycle_id(&self) -> &str {
        &self.cycle_id
    }

    /// The step as the store journaled it.
    #[must_use]
    pub fn step(&self) -> &InFlightStep {
        &self.step
    }

    /// The committed checkpoint the step was admitted against, as stored.
    #[must_use]
    pub fn checkpoint(&self) -> &CycleCheckpoint {
        &self.checkpoint
    }
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
    /// Typed run observations committed together with the checkpoint and not
    /// yet acknowledged by the downstream drain (W03c). Records written
    /// before W03c have no such field and load with an empty outbox.
    #[serde(default)]
    pub outbox: Outbox<RunObservationV1>,
    /// Observations dropped because the outbox was full at commit time.
    #[serde(default)]
    pub observations_dropped: u32,
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
            outbox: Outbox::new(),
            observations_dropped: 0,
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
            pending_observations: self.outbox.len(),
            observations_dropped: self.observations_dropped,
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
    /// Observations committed but not yet acknowledged (W03c).
    pub pending_observations: usize,
    /// Observations dropped because the outbox was full (W03c).
    pub observations_dropped: u32,
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
    /// The job store reports that the lease is not the current, non-expired
    /// claim, or it could not be held (see [`LeaseLivenessGuard`]). Nothing
    /// was read out or written.
    LeaseNotLive(String),
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
            Self::LeaseNotLive(reason) => write!(f, "job lease is not live: {reason}"),
        }
    }
}

impl std::error::Error for CycleStoreError {}

/// Holds a job lease live for the duration of one cycle-store operation.
///
/// The epoch fence protects against an *older* incarnation: once a reclaim
/// has rebound the cycle to a newer epoch, every write of the superseded
/// epoch is refused. It cannot see that the *same* incarnation's job lease
/// expired, or was cancelled or reclaimed, while no reclaim has rebound the
/// cycle yet — in that window the old epoch is still the record's epoch. A
/// point-in-time check would not close it either: the job record could be
/// replaced between the check and the cycle write. The guard therefore
/// *holds* the lease: [`CycleStore`] calls [`LeaseLivenessGuard::hold`]
/// before every fenced operation, runs the operation (its locked write or
/// fenced read) while the returned hold lives, and drops the hold
/// afterwards.
///
/// A record lock cannot stop the wall clock: the held lease still expires.
/// Every fenced operation therefore asks the hold ([`LeaseHold::still_live`])
/// once more as its very last step — for a write after the new record is
/// synced to its temp file and immediately before the atomic rename that
/// publishes it — and refuses with [`CycleStoreError::LeaseNotLive`] once
/// the deadline has passed.
///
/// Lock order: the job lease (whatever `hold` takes) first, the cycle
/// record lock second; released in reverse. An implementation must never
/// take the cycle store's locks. Production
/// (`harw-cli`'s intent-cycle worker) wraps
/// `harw_session_store::JobStore::hold_live_lease`, which renews the lease
/// durably (a fresh, bounded commit window) and holds the job's per-record
/// lock, so no claim, renewal, cancellation or reclaim of the job can commit
/// in between; keep holds short — the job's own heartbeat waits for them.
pub trait LeaseLivenessGuard: Send + Sync {
    /// Verifies that `token` is the current, non-expired lease of its job
    /// and keeps it from being replaced until the returned hold is dropped.
    ///
    /// # Errors
    /// A rendered reason when the lease is not live or cannot be held.
    fn hold(&self, token: &LeaseToken) -> Result<Box<dyn LeaseHold>, String>;
}

/// One held job lease (see [`LeaseLivenessGuard`]); dropping it releases
/// the lease.
pub trait LeaseHold: Send {
    /// `Ok` while the held lease has not passed its deadline.
    ///
    /// # Errors
    /// A rendered reason once the deadline has passed: the operation must
    /// not publish anything.
    fn still_live(&self) -> Result<(), String>;
}

/// Fenced persistence of [`CycleRecord`]s.
pub struct CycleStore {
    records: RecordStore<CycleRecord>,
    /// Holds the job lease around every fenced operation; `None` keeps the
    /// epoch fence only.
    liveness: Option<Arc<dyn LeaseLivenessGuard>>,
}

impl CycleStore {
    /// A store with the epoch fence only (no [`LeaseLivenessGuard`]).
    #[must_use]
    pub fn new(records: RecordStore<CycleRecord>) -> Self {
        Self {
            records,
            liveness: None,
        }
    }

    /// Additionally fences every lease-bound operation to the live job claim
    /// (see [`LeaseLivenessGuard`]): `rebind`, `begin_step`, `commit_step`,
    /// `commit_step_observed`, `clear_step`, `decide_observations` and
    /// `ack_observations` hold the lease across their locked write, and the
    /// fenced reads `pending_observations` and `journaled_step` across the
    /// read. The hold is taken before the cycle record lock and released
    /// after it. Unfenced [`CycleStore::load`] and [`CycleStore::create`] do
    /// not take a lease and are unaffected.
    #[must_use]
    pub fn with_lease_liveness(mut self, guard: Arc<dyn LeaseLivenessGuard>) -> Self {
        self.liveness = Some(guard);
        self
    }

    /// Holds the job lease for one fenced operation (`None` without a
    /// guard). The caller keeps the result alive until its record lock is
    /// released (job lease first, cycle record second) and checks it with
    /// [`lease_still_live`] — for writes as the pre-rename commit authority
    /// of [`RecordStore::update_locked_checked`], so an expired deadline
    /// refuses the write before anything is published.
    fn hold_lease(
        &self,
        token: &LeaseToken,
    ) -> Result<Option<Box<dyn LeaseHold>>, CycleStoreError> {
        match &self.liveness {
            Some(guard) => guard
                .hold(token)
                .map(Some)
                .map_err(CycleStoreError::LeaseNotLive),
            None => Ok(None),
        }
    }

    /// Opens (creating if needed) the store at `<job_root>/`[`CYCLE_STORE_DIR`].
    ///
    /// # Errors
    /// The underlying store error.
    pub fn open_in_job_root(job_root: &Path) -> Result<Self, CycleStoreError> {
        Ok(Self::new(RecordStore::create_ambient(
            &job_root.join(CYCLE_STORE_DIR),
        )?))
    }

    /// Opens the store at `<job_root>/`[`CYCLE_STORE_DIR`] if it exists;
    /// creates nothing (status reads must not create directories).
    ///
    /// # Errors
    /// The underlying store error.
    pub fn open_existing_in_job_root(job_root: &Path) -> Result<Option<Self>, CycleStoreError> {
        Ok(RecordStore::open_existing_ambient(&job_root.join(CYCLE_STORE_DIR))?.map(Self::new))
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

    /// Loads one record without locking or fencing: for status projections
    /// ([`CycleRecord::status`]) and the driver's own state. Anything that
    /// hands outbox payloads on (a drain writing project memory) reads them
    /// through the fenced [`CycleStore::pending_observations`] instead, and
    /// trusted follow-up admissions take a [`CycleStore::journaled_step`]
    /// proof rather than this record's in-flight step.
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
    /// candidate is not fenced under `token`, [`check_rebind`] refuses, or
    /// the lease cannot be held live ([`CycleStoreError::LeaseNotLive`]).
    pub fn rebind(
        &self,
        cycle_id: &str,
        token: &LeaseToken,
        resumed: &CycleCheckpoint,
    ) -> Result<CycleRecord, CycleStoreError> {
        let lease = self.hold_lease(token)?;
        self.records.update_locked_checked(
            cycle_id,
            |record| {
                check_schema_and_job(record, token)?;
                if resumed.fence.epoch != token.epoch {
                    return Err(CycleStoreError::EpochMismatch);
                }
                check_rebind(&record.checkpoint, resumed).map_err(CycleStoreError::Fence)?;
                record.checkpoint = resumed.clone();
                lease_still_live(lease.as_deref())?;
                Ok(record.clone())
            },
            || lease_still_live(lease.as_deref()),
        )
    }

    /// Journals `admitted` as the in-flight step under `token`.
    ///
    /// # Errors
    /// [`CycleStoreError`] for a foreign, stale or no longer live lease, an
    /// existing in-flight step, or an admission decided against another
    /// sequence.
    pub fn begin_step(
        &self,
        cycle_id: &str,
        token: &LeaseToken,
        admitted: &AdmittedCycle,
    ) -> Result<InFlightStep, CycleStoreError> {
        let lease = self.hold_lease(token)?;
        self.records.update_locked_checked(
            cycle_id,
            |record| {
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
                lease_still_live(lease.as_deref())?;
                Ok(step)
            },
            || lease_still_live(lease.as_deref()),
        )
    }

    /// Loads the record under `token` (fenced) and returns its journaled step.
    ///
    /// The step must have been begun under `token`'s epoch: a step a
    /// superseded lease left in flight is reconciled, never handed out as
    /// proof to the lease that took over.
    ///
    /// # Errors
    /// Fence/job errors as every fenced read ([`CycleStoreError::Fence`],
    /// [`CycleStoreError::WrongJob`], [`CycleStoreError::NotRebound`],
    /// [`CycleStoreError::LeaseNotLive`]);
    /// [`CycleStoreError::NoStepInFlight`] when nothing is journaled;
    /// [`CycleStoreError::StepMismatch`] when the journaled step was begun
    /// under an earlier epoch or against another sequence.
    pub fn journaled_step(
        &self,
        cycle_id: &str,
        token: &LeaseToken,
    ) -> Result<JournaledStep, CycleStoreError> {
        let lease = self.hold_lease(token)?;
        let record = self.load(cycle_id)?;
        check_fenced(&record, token)?;
        let step = record.in_flight.ok_or(CycleStoreError::NoStepInFlight)?;
        if step.epoch != token.epoch || step.sequence != record.checkpoint.fence.sequence {
            return Err(CycleStoreError::StepMismatch);
        }
        lease_still_live(lease.as_deref())?;
        Ok(JournaledStep {
            cycle_id: record.cycle_id,
            step,
            checkpoint: record.checkpoint,
        })
    }

    /// Commits the outcome of the journaled step and clears the journal.
    ///
    /// Same as [`CycleStore::commit_step_observed`] without observations.
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
        self.commit_step_observed(cycle_id, token, next, &[])
    }

    /// [`CycleStore::commit_step`] plus the step's `observations`, in the
    /// same locked write.
    ///
    /// Each observation is appended to the record's outbox under its
    /// [`dedupe_key`](crate::cycle_observation::ObservationEventId::dedupe_key),
    /// so replaying a commit never duplicates one. A full outbox drops the
    /// newest observations (counted in [`CycleRecord::observations_dropped`])
    /// but never refuses the checkpoint commit.
    ///
    /// # Errors
    /// [`CycleStoreError`] for a foreign, stale or no longer live lease, a
    /// missing or mismatching journaled step, or a [`check_commit`] refusal.
    pub fn commit_step_observed(
        &self,
        cycle_id: &str,
        token: &LeaseToken,
        next: &CycleCheckpoint,
        observations: &[RunObservationV1],
    ) -> Result<(), CycleStoreError> {
        let lease = self.hold_lease(token)?;
        self.records.update_locked_checked(
            cycle_id,
            |record| {
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
                for observation in observations {
                    let entry = OutboxEntry {
                        key: observation.event_id.dedupe_key(),
                        epoch: token.epoch,
                        sequence: next.fence.sequence,
                        payload: observation.clone(),
                        state: DeliveryState::Pending,
                        materialization: None,
                    };
                    match record
                        .outbox
                        .append(token.epoch, record.checkpoint.fence.epoch, entry)
                    {
                        Ok(_) => {}
                        Err(OutboxError::Full) => {
                            record.observations_dropped =
                                record.observations_dropped.saturating_add(1);
                            tracing::warn!(
                                cycle_id,
                                "intent cycle observation dropped: outbox full"
                            );
                        }
                        Err(OutboxError::StaleEpoch) => {
                            // Unreachable after `check_fenced`; counted like a
                            // full outbox so the loss stays visible.
                            record.observations_dropped =
                                record.observations_dropped.saturating_add(1);
                        }
                    }
                }
                lease_still_live(lease.as_deref())?;
                Ok(())
            },
            || lease_still_live(lease.as_deref()),
        )
    }

    /// Observations committed but not yet acknowledged, in append order,
    /// read under `token`.
    ///
    /// The read is fenced like every write ([`CycleStore::ack_observations`]):
    /// the record is checked against `token` *before* any payload leaves the
    /// store, so a superseded epoch or another job's lease cannot copy the
    /// current owner's observations anywhere (e.g. into project memory)
    /// before its acknowledgement would be refused. Accepted entries
    /// ([`DeliveryState::Accepted`]) are pending too: their delivery is
    /// committed but not yet confirmed. The read is a snapshot: a consumer
    /// with external side effects commits its decision first
    /// ([`CycleStore::decide_observations`], which holds the lease across
    /// its locked write) and only then acts on the payloads.
    ///
    /// # Errors
    /// The underlying store error; [`CycleStoreError::Fence`] with
    /// [`FenceRefusal::StaleEpoch`] for a superseded lease,
    /// [`CycleStoreError::WrongJob`] for another job's lease,
    /// [`CycleStoreError::NotRebound`] for a newer lease that has not rebound
    /// the cycle yet and [`CycleStoreError::LeaseNotLive`] for a lease the
    /// guard cannot hold (expired, cancelled or reclaimed before a rebind).
    pub fn pending_observations(
        &self,
        cycle_id: &str,
        token: &LeaseToken,
    ) -> Result<Vec<OutboxEntry<RunObservationV1>>, CycleStoreError> {
        let lease = self.hold_lease(token)?;
        let record = self.load(cycle_id)?;
        check_fenced(&record, token)?;
        let pending = record.outbox.pending().to_vec();
        lease_still_live(lease.as_deref())?;
        Ok(pending)
    }

    /// Acknowledges delivered observations under `token`, fenced like every
    /// other write. Returns how many pending entries were removed; unknown or
    /// already acknowledged keys count as 0, so redelivery after a crash is
    /// idempotent.
    ///
    /// The lease is held across the locked write, so the acknowledgement is
    /// committed under a lease that cannot be replaced in between.
    ///
    /// # Errors
    /// [`CycleStoreError`] for a foreign, stale, not yet rebound or no longer
    /// live lease.
    pub fn ack_observations(
        &self,
        cycle_id: &str,
        token: &LeaseToken,
        keys: &[String],
    ) -> Result<usize, CycleStoreError> {
        let lease = self.hold_lease(token)?;
        self.records.update_locked_checked(
            cycle_id,
            |record| {
                check_fenced(record, token)?;
                let current = record.checkpoint.fence.epoch;
                let mut removed = 0;
                for key in keys {
                    if matches!(record.outbox.ack(token.epoch, current, key), Ok(true)) {
                        removed += 1;
                    }
                }
                lease_still_live(lease.as_deref())?;
                Ok(removed)
            },
            || lease_still_live(lease.as_deref()),
        )
    }

    /// Records a consumer's delivery decision in one fenced write held under
    /// the live lease (two-phase delivery, see [`DeliveryState`]): every key
    /// in `accept` becomes [`DeliveryState::Accepted`] together with its
    /// materialisation — what the consumer will deliver, stored so that a
    /// replay delivers exactly that — and stays pending until it is
    /// acknowledged after its delivery is durable; every key in `ack` is
    /// acknowledged (removed) directly. A key in both lists is
    /// acknowledged. Returns `(accepted, acked)`: entries newly accepted and
    /// entries removed; unknown, already accepted or already acknowledged
    /// keys count as 0, so a replayed decision is idempotent.
    ///
    /// # Errors
    /// [`CycleStoreError`] for a foreign, stale, not yet rebound or no longer
    /// live lease; nothing is changed then.
    pub fn decide_observations(
        &self,
        cycle_id: &str,
        token: &LeaseToken,
        accept: &[(String, serde_json::Value)],
        ack: &[String],
    ) -> Result<(usize, usize), CycleStoreError> {
        let lease = self.hold_lease(token)?;
        self.records.update_locked_checked(
            cycle_id,
            |record| {
                check_fenced(record, token)?;
                let current = record.checkpoint.fence.epoch;
                let mut accepted = 0;
                for (key, materialization) in accept {
                    let marked =
                        record
                            .outbox
                            .accept(token.epoch, current, key, materialization.clone());
                    if matches!(marked, Ok(true)) {
                        accepted += 1;
                    }
                }
                let mut acked = 0;
                for key in ack {
                    if matches!(record.outbox.ack(token.epoch, current, key), Ok(true)) {
                        acked += 1;
                    }
                }
                lease_still_live(lease.as_deref())?;
                Ok((accepted, acked))
            },
            || lease_still_live(lease.as_deref()),
        )
    }

    /// Drops a journaled step the executor proved never started.
    ///
    /// # Errors
    /// [`CycleStoreError`] for a foreign, stale or no longer live lease or
    /// another step.
    pub fn clear_step(
        &self,
        cycle_id: &str,
        token: &LeaseToken,
        step: &InFlightStep,
    ) -> Result<(), CycleStoreError> {
        let lease = self.hold_lease(token)?;
        self.records.update_locked_checked(
            cycle_id,
            |record| {
                check_fenced(record, token)?;
                if record.in_flight.as_ref() != Some(step) {
                    return Err(CycleStoreError::StepMismatch);
                }
                record.in_flight = None;
                lease_still_live(lease.as_deref())?;
                Ok(())
            },
            || lease_still_live(lease.as_deref()),
        )
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

/// The deadline check of every fenced operation: the held lease (if any) has
/// not passed its deadline. A write calls it twice: at the end of its
/// mutation closure (cheap early refusal) and as the commit authority of
/// [`RecordStore::update_locked_checked`], i.e. after the new record is
/// written and `sync_all`ed to its temp file and immediately before the
/// rename that publishes it — the last point at which a refusal is
/// possible, so a stalled fsync cannot carry a write past the deadline. The
/// rename itself is atomic. A fenced read calls it before it hands out its
/// payload.
fn lease_still_live(lease: Option<&dyn LeaseHold>) -> Result<(), CycleStoreError> {
    match lease {
        Some(hold) => hold.still_live().map_err(CycleStoreError::LeaseNotLive),
        None => Ok(()),
    }
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
    /// The executor cannot prove that no external effect happened. The driver
    /// retains its journal and stops; only reconciliation may settle the step.
    pub effect_uncertain: bool,
}

impl StepFailure {
    /// A permanent failure with no external effect (missing capability, auth, bad output).
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retryable: false,
            effect_uncertain: false,
        }
    }

    /// An executor lost the result of a possibly completed or partial effect.
    /// Never a retry grant: the in-flight journal must survive until a trusted
    /// executor reconciles its idempotency key. Use this for a write timeout,
    /// lost acknowledgement, or any error after an effect may have started.
    #[must_use]
    pub fn uncertain(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retryable: false,
            effect_uncertain: true,
        }
    }

    /// A temporary failure with no external effect. For proposers, the driver
    /// commits no terminal state; executor failures still count as stalls.
    #[must_use]
    pub fn retryable(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retryable: true,
            effect_uncertain: false,
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
    ///
    /// `cancel` is the job's cancel token; proposers pass it to the model
    /// request so a stop or lease loss aborts an in-flight call.
    fn propose(
        &mut self,
        admission: &CycleAdmission,
        state: &CycleCheckpoint,
        last_refusal: Option<CycleRefusal>,
        cancel: &CancelToken,
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
    /// Ordinary `Err` must mean that no effect happened (e.g. a capability
    /// failure); the step is committed without observations and counts as a
    /// stall. If an effect may have happened, return [`StepFailure::uncertain`]
    /// instead: the driver retains the journal and blocks the job until
    /// reconciliation, even without cancellation. Do not return incomplete
    /// observations as success. A cancellation error also retains the journal.
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

/// Reissuer for runtimes whose authority does not come from a
/// [`PolicyBootstrap`] (job workers assembled from the runtime profile
/// table): the persisted snapshot is cut against the *current* trusted
/// context of the same workspace. The result is the intersection, so it can
/// only keep or drop what the snapshot recorded — never add.
#[derive(Debug, Clone)]
pub struct CeilingReissuer {
    ceiling: AuthorityContext,
}

impl CeilingReissuer {
    #[must_use]
    pub fn new(ceiling: AuthorityContext) -> Self {
        Self { ceiling }
    }
}

impl AuthorityReissuer for CeilingReissuer {
    fn reissue(&self, snapshot: &AuthoritySnapshot) -> Result<AuthorityContext, String> {
        if !snapshot.workspace().matches(self.ceiling.workspace()) {
            return Err("snapshot belongs to another workspace".to_owned());
        }
        Ok(self.ceiling.restrict(snapshot.request()))
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
    /// An executor could not establish its effect. The in-flight journal is
    /// retained and the job is blocked, never automatically retried.
    EffectUncertain,
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
            Self::EffectUncertain => f.write_str(
                "intent cycle effect is uncertain; journal retained for reconciliation",
            ),
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
    /// Observations of the step in progress; moved into the step's commit.
    pending: Vec<RunObservationV1>,
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
            pending: Vec::new(),
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
    /// A cycle that is already terminal runs nothing. If `token` is newer
    /// than the record's epoch, the record is first handed over to it (see
    /// [`CycleStore::rebind`]; only the fence epoch moves), so the new lease
    /// can drain and acknowledge observations the earlier incarnation
    /// committed with its terminal step but never acknowledged.
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
        // Observations of an interrupted earlier run were never committed.
        self.pending.clear();
        let id = cycle_id.to_owned();
        let mut record = self.blocking(move |store| store.load(&id)).await?;
        if let Some(terminal) = record.checkpoint.terminal {
            if token.epoch > record.checkpoint.fence.epoch {
                // No step runs any more, but the outbox may still hold what
                // the terminal commit recorded. Hand the record over to this
                // lease so it can drain and acknowledge it: only the epoch
                // moves; state, ceiling and authority stay as recorded, which
                // `check_rebind` verifies (no reissue is needed, nothing is
                // admitted under it). Fenced and lease-held like every
                // rebind; a refusal ends the run (fail closed).
                let mut handover = record.checkpoint.clone();
                handover.fence.epoch = token.epoch;
                let (id, lease) = (cycle_id.to_owned(), token.clone());
                record = self
                    .blocking(move |store| store.rebind(&id, &lease, &handover))
                    .await
                    .inspect_err(|error| log_fence_refusal(cycle_id, error))?;
            }
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
                .await
                .inspect_err(|error| log_fence_refusal(cycle_id, error))?;
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
            // The proposer gets `cancel` so its model call aborts; the race
            // below is the second guard for proposers that ignore it.
            let proposed = tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    return Ok(CycleRunOutcome::Cancelled { checkpoint });
                }
                proposed = self.proposer.propose(&admission, &checkpoint, last_refusal, cancel) => proposed,
            };
            let proposal = match proposed {
                Ok(proposal) => proposal,
                Err(failure) if failure.retryable => {
                    return Err(CycleRunError::ProposerUnavailable(failure.reason));
                }
                Err(failure) => {
                    // Only the proposal kind is used; the reason stays out.
                    let kind = CycleProposal::Blocked {
                        reason: String::new(),
                    };
                    // No executor ran: a proposer failure is never a tool
                    // failure, so it gets its own class.
                    self.observe(
                        cycle_id,
                        &checkpoint,
                        &kind,
                        classify_proposer_failure(&failure),
                        &[],
                    );
                    CycleProposal::Blocked {
                        reason: format!(
                            "proposer unavailable: {}",
                            failure.reason.chars().take(400).collect::<String>()
                        ),
                    }
                }
            };
            // `admit_cycle` consumes the proposal; a refusal is observed
            // against what was proposed.
            let attempted = proposal.clone();
            let admitted = match admit_cycle(&admission, &checkpoint, proposal) {
                Ok(admitted) => {
                    last_refusal = None;
                    refusals = 0;
                    admitted
                }
                Err(refusal) => {
                    refusals = refusals.saturating_add(1);
                    last_refusal = Some(refusal);
                    self.observe(
                        cycle_id,
                        &checkpoint,
                        &attempted,
                        classify_refusal(refusal),
                        &[],
                    );
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
                .await
                .inspect_err(|error| log_fence_refusal(cycle_id, error))?;
            let observations = if admitted.terminal().is_some() {
                CycleObservations::default()
            } else {
                let executed = self
                    .executor
                    .execute(&step, &admission, &checkpoint, cancel)
                    .await;
                match executed {
                    Ok(observations) => {
                        if let Some(class) = classify_verification(&observations) {
                            let evidence_ids = verify_evidence_ids(admitted.proposal());
                            self.observe(
                                cycle_id,
                                &checkpoint,
                                admitted.proposal(),
                                class,
                                &evidence_ids,
                            );
                        }
                        observations
                    }
                    Err(failure) if cancel.is_cancelled() => {
                        // Leave the journal entry: the next lease reconciles.
                        tracing::info!(cycle_id, reason = %failure.reason, "intent cycle step interrupted");
                        return Ok(CycleRunOutcome::Cancelled { checkpoint });
                    }
                    Err(failure) if failure.effect_uncertain => {
                        // A lost acknowledgement is not evidence of no effect.
                        // Do not advance the checkpoint, learn a false failure,
                        // or let the proposer issue another write. Recovery
                        // reuses the journal's original idempotency key.
                        tracing::warn!(cycle_id, "intent cycle effect uncertain; journal retained");
                        return Err(CycleRunError::EffectUncertain);
                    }
                    Err(failure) => {
                        tracing::warn!(cycle_id, reason = %failure.reason, "intent cycle step failed without effect");
                        self.observe(
                            cycle_id,
                            &checkpoint,
                            admitted.proposal(),
                            classify_step_failure(&failure),
                            &[],
                        );
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
        let reconciliation = self.executor.reconcile(&step).await;
        if let Some(class) = classify_reconciliation(&reconciliation) {
            self.observe(cycle_id, state, &step.proposal, class, &[]);
        }
        match reconciliation {
            StepReconciliation::NotStarted => {
                let (id, lease) = (cycle_id.to_owned(), token.clone());
                self.blocking(move |store| store.clear_step(&id, &lease, &step))
                    .await
                    .inspect_err(|error| log_fence_refusal(cycle_id, error))?;
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
        // The step's observations travel with its checkpoint; if the commit
        // fails they are lost with it and the next lease re-runs the step.
        let observed = std::mem::take(&mut self.pending);
        self.blocking(move |store| store.commit_step_observed(&id, &lease, &candidate, &observed))
            .await
            .inspect_err(|error| log_fence_refusal(cycle_id, error))?;
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

    /// Queues one observation of the step in progress. `index` is the
    /// position within the step, so every observation of a commit has a
    /// distinct [`dedupe_key`](crate::cycle_observation::ObservationEventId::dedupe_key).
    fn observe(
        &mut self,
        cycle_id: &str,
        checkpoint: &CycleCheckpoint,
        proposal: &CycleProposal,
        (category, cause): (ObservationCategory, CauseCode),
        evidence_ids: &[String],
    ) {
        let index = u32::try_from(self.pending.len()).unwrap_or(u32::MAX);
        self.pending.push(build_observation(
            cycle_id,
            checkpoint.fence,
            index,
            checkpoint,
            proposal,
            category,
            cause,
            evidence_ids,
        ));
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

/// Evidence id a `Verify` proposal refers to; empty for every other proposal.
fn verify_evidence_ids(proposal: &CycleProposal) -> Vec<String> {
    match proposal {
        CycleProposal::Verify { evidence_id } => vec![evidence_id.clone()],
        _ => Vec::new(),
    }
}

/// A fence refusal cannot be recorded in the record it was refused for, so
/// it is only logged, classified like any other observation. A liveness
/// refusal ([`CycleStoreError::LeaseNotLive`]) is the same kind of refusal —
/// the lease may no longer write — and is handled the same way: logged, not
/// recorded, and the run ends with the error (fail closed; `job_outcome`
/// maps it to a failed job, whose completion the job store then refuses for
/// the dead lease anyway).
fn log_fence_refusal(cycle_id: &str, error: &CycleRunError) {
    match error {
        CycleRunError::Store(CycleStoreError::Fence(refusal)) => {
            let (category, cause) = classify_fence(*refusal);
            tracing::warn!(
                cycle_id,
                ?category,
                ?cause,
                "intent cycle write refused by the epoch fence; not recorded as an observation"
            );
        }
        CycleRunError::Store(CycleStoreError::LeaseNotLive(reason)) => {
            tracing::warn!(
                cycle_id,
                reason = %reason,
                "intent cycle write refused: the job lease is no longer live; not recorded as an observation"
            );
        }
        _ => {}
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
        Err(CycleRunError::EffectUncertain) => JobOutcome::Blocked {
            reason: CycleRunError::EffectUncertain.to_string(),
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_core::{DurableJobRunner, JobExecutionRegistry};
    use harw_job_core::{Budget, Job, JobKind, JobScope, RetryPolicy, StoredJob};
    use harw_session_store::{ClaimRequest, JobStore};
    use harw_types::{ApprovalActor, ContentDigest, TenantId, WorkspaceId};
    use jiff::{SignedDuration, Timestamp};

    use super::*;
    use crate::cycle_observation::ObservationOutcome;
    use crate::cycle_outbox::MAX_OUTBOX_PENDING;
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
        /// Directory of the fixture's record store (to reopen it).
        cycles_dir: PathBuf,
        store: Arc<CycleStore>,
        work_id: WorkId,
        admission: CycleAdmission,
    }

    fn fixture(cycle_id: &str) -> TestResult<Fixture> {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let cycles_dir = temp.path().join("cycles");
        let records =
            RecordStore::create_ambient(&cycles_dir).map_err(ctx("cycle record store"))?;
        let store = Arc::new(CycleStore::new(records));
        let admission = admission()?;
        let work_id = WorkId::from_str("job-w02");
        let checkpoint = CycleCheckpoint::initial(&admission, &rights()?, 0);
        let record = CycleRecord::new(cycle_id, work_id.clone(), checkpoint)
            .map_err(ctx("record builds"))?;
        store.create(&record).map_err(ctx("record persists"))?;
        Ok(Fixture {
            _temp: temp,
            cycles_dir,
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
            _cancel: &CancelToken,
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
        fx.store
            .create(
                &CycleRecord::new("cycle-deep-2", fx.work_id.clone(), record.checkpoint)
                    .map_err(ctx("record builds"))?,
            )
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
            seen.lock()
                .map_err(|_| TestError::Missing("lock"))?
                .is_empty(),
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
            _cancel: &CancelToken,
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
    fn stale_lease_cannot_obtain_a_journaled_step() -> TestResult {
        let fx = fixture("cycle-journaled")?;
        let step = crash_mid_step(&fx, "cycle-journaled")?;
        let fresh = token(&fx.work_id, 1);
        let record = fx.store.load("cycle-journaled").map_err(ctx("load"))?;
        let journaled = fx
            .store
            .journaled_step("cycle-journaled", &fresh)
            .map_err(ctx("journaled step under the fresh lease"))?;
        assert_eq!(journaled.cycle_id(), "cycle-journaled");
        assert_eq!(journaled.step(), &step);
        assert_eq!(journaled.checkpoint(), &record.checkpoint);
        assert!(matches!(
            fx.store
                .journaled_step("cycle-journaled", &token(&fx.work_id, 0)),
            Err(CycleStoreError::Fence(FenceRefusal::StaleEpoch))
        ));
        assert!(matches!(
            fx.store
                .journaled_step("cycle-journaled", &token(&fx.work_id, 2)),
            Err(CycleStoreError::NotRebound)
        ));
        assert!(matches!(
            fx.store
                .journaled_step("cycle-journaled", &token(&WorkId::from_str("other-job"), 1)),
            Err(CycleStoreError::WrongJob)
        ));
        // Epoch 2 takes over: epoch 1 is stale now, and the step it left in
        // flight is reconciled, never handed to the new lease as proof.
        let newer = token(&fx.work_id, 2);
        let (_, taken_over) = resume_admission(&record.checkpoint, &fx.admission, &rights()?, 2)
            .map_err(|_| TestError::Missing("resume"))?;
        fx.store
            .rebind("cycle-journaled", &newer, &taken_over)
            .map_err(ctx("takeover"))?;
        assert!(matches!(
            fx.store.journaled_step("cycle-journaled", &fresh),
            Err(CycleStoreError::Fence(FenceRefusal::StaleEpoch))
        ));
        assert!(matches!(
            fx.store.journaled_step("cycle-journaled", &newer),
            Err(CycleStoreError::StepMismatch)
        ));
        fx.store
            .clear_step("cycle-journaled", &newer, &step)
            .map_err(ctx("clear the reconciled step"))?;
        assert!(matches!(
            fx.store.journaled_step("cycle-journaled", &newer),
            Err(CycleStoreError::NoStepInFlight)
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
    fn ceiling_reissuer_never_widens_and_refuses_foreign_workspace() -> TestResult {
        let read_only = authority(&[Permission::ReadWorkspace])?;
        let read_write = authority(&[Permission::ReadWorkspace, Permission::WriteWorkspace])?;

        // Snapshot wider than the ceiling: the ceiling bounds the result.
        let capped = CeilingReissuer::new(read_only.clone())
            .reissue(&read_write.snapshot())
            .map_err(|_| TestError::Missing("same-workspace reissue succeeds"))?;
        assert!(!capped.permissions().contains(Permission::WriteWorkspace));
        assert!(capped.is_subset_of(&read_only));

        // Ceiling wider than the snapshot: the snapshot bounds the result.
        let narrowed = CeilingReissuer::new(read_write.clone())
            .reissue(&read_only.snapshot())
            .map_err(|_| TestError::Missing("same-workspace reissue succeeds"))?;
        assert!(!narrowed.permissions().contains(Permission::WriteWorkspace));
        assert!(narrowed.is_subset_of(&read_only));

        // Same tenant, different workspace: refused, never rebound.
        let harness_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or(TestError::Missing("harness root"))?
            .to_path_buf();
        let tenant = TenantId::from_str("cycle-tenant");
        let other = WorkspaceId::from_str("cycle-runtime-other");
        let foreign_binding = WorkspaceRegistry::build(
            &harness_root,
            [WorkspaceRegistration {
                tenant: tenant.clone(),
                workspace: other.clone(),
                root: PathBuf::from("harw-plan-bridge"),
            }],
        )
        .map_err(ctx("foreign workspace registers"))?
        .resolve(&tenant, &other)
        .map_err(ctx("foreign workspace resolves"))?;
        let foreign = SandboxSpec::from_resolved(
            foreign_binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        )
        .authority()
        .clone();
        assert!(
            CeilingReissuer::new(read_write)
                .reissue(&foreign.snapshot())
                .is_err(),
            "a snapshot of another workspace must not be reissued"
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
                jitter: 0.0,
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

    fn sample_observation(
        cycle_id: &str,
        checkpoint: &CycleCheckpoint,
        index: u32,
    ) -> RunObservationV1 {
        build_observation(
            cycle_id,
            checkpoint.fence,
            index,
            checkpoint,
            &wait(),
            ObservationCategory::ToolFailedNoEffect,
            CauseCode::ExecutorFailed,
            &[],
        )
    }

    /// Takes the cycle over at epoch 1, journals a `wait` step and commits it
    /// together with `count` observations. Returns their outbox keys.
    fn commit_observed(fx: &Fixture, cycle_id: &str, count: u32) -> TestResult<Vec<String>> {
        crash_mid_step(fx, cycle_id)?;
        let record = fx.store.load(cycle_id).map_err(ctx("load"))?;
        let admitted = AdmittedCycle::recovered(wait(), record.checkpoint.fence.sequence);
        let next = apply_observations(
            fx.admission.intent(),
            &record.checkpoint,
            &admitted,
            CycleObservations::default(),
        )
        .map_err(|_| TestError::Missing("apply"))?;
        let observations: Vec<RunObservationV1> = (0..count)
            .map(|index| sample_observation(cycle_id, &record.checkpoint, index))
            .collect();
        fx.store
            .commit_step_observed(cycle_id, &token(&fx.work_id, 1), &next, &observations)
            .map_err(ctx("commit with observations"))?;
        Ok(observations
            .iter()
            .map(|observation| observation.event_id.dedupe_key())
            .collect())
    }

    #[tokio::test]
    async fn refusal_observations_land_in_the_same_record_write() -> TestResult {
        let fx = fixture("cycle-obs-refused")?;
        let patch = || {
            Ok(CycleProposal::ProposePatch {
                targets: set(&["secrets.toml"]),
                evidence_id: "none".to_owned(),
            })
        };
        let proposer = Script::new(vec![patch(), patch(), patch()]);
        let mut driver = driver(&fx, proposer, Executor::new(vec![]), rights()?);
        let outcome = driver
            .run(
                "cycle-obs-refused",
                &token(&fx.work_id, 1),
                &fx.admission,
                &CancelToken::new(),
            )
            .await
            .map_err(ctx("driver runs"))?;
        assert!(matches!(
            outcome,
            CycleRunOutcome::Terminal {
                terminal: CycleTerminal::Escalated,
                ..
            }
        ));
        let record = fx
            .store
            .load("cycle-obs-refused")
            .map_err(ctx("record reloads"))?;
        assert_eq!(record.outbox.len(), 3);
        assert_eq!(record.observations_dropped, 0);
        assert_eq!(record.status().pending_observations, 3);
        for (position, entry) in record.outbox.pending().iter().enumerate() {
            let payload = &entry.payload;
            assert_eq!(payload.category, ObservationCategory::AdmissionRefused);
            assert_eq!(payload.cause, CauseCode::WriteNotAllowed);
            assert_eq!(payload.transition, "propose_patch");
            assert_eq!(
                entry.sequence, record.checkpoint.fence.sequence,
                "the observation is stamped with the sequence of its own commit"
            );
            assert_eq!(payload.event_id.sequence, 0, "refused against sequence 0");
            assert_eq!(
                u32::try_from(position).ok(),
                Some(payload.event_id.index),
                "indexes count up within the step"
            );
            assert_eq!(entry.key, payload.event_id.dedupe_key());
            assert_eq!(entry.epoch, 1);
        }
        Ok(())
    }

    /// A real external effect followed by a lost acknowledgement. The durable
    /// checkpoint must not describe this as a no-effect failure.
    struct LostAcknowledgement {
        target: PathBuf,
        calls: Arc<AtomicUsize>,
    }

    impl CycleStepExecutor for LostAcknowledgement {
        async fn execute(
            &mut self,
            _step: &InFlightStep,
            _admission: &CycleAdmission,
            _state: &CycleCheckpoint,
            _cancel: &CancelToken,
        ) -> Result<CycleObservations, StepFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::fs::write(&self.target, b"owned edit")
                .map_err(|error| StepFailure::uncertain(error.to_string()))?;
            Err(StepFailure::uncertain("write acknowledgement lost"))
        }

        async fn reconcile(&mut self, _step: &InFlightStep) -> StepReconciliation {
            match std::fs::read(&self.target) {
                Ok(bytes) if bytes == b"owned edit" => {
                    StepReconciliation::Completed(CycleObservations::default())
                }
                _ => StepReconciliation::Uncertain,
            }
        }
    }

    #[tokio::test]
    async fn uncertain_effect_preserves_journal_and_reconciles_without_reexecution() -> TestResult {
        let cycle_id = "cycle-lost-write-ack";
        let fx = fixture(cycle_id)?;
        let calls = Arc::new(AtomicUsize::new(0));
        let target = fx._temp.path().join("owned.rs");
        let mut driver = CycleDriver::new(
            Arc::clone(&fx.store),
            Script::new(vec![Ok(wait())]),
            LostAcknowledgement {
                target: target.clone(),
                calls: Arc::clone(&calls),
            },
            Arc::new(FixedReissuer(rights()?)),
        );
        let result = driver
            .run(cycle_id, &token(&fx.work_id, 1), &fx.admission, &CancelToken::new())
            .await;
        assert!(matches!(result, Err(CycleRunError::EffectUncertain)));
        assert!(matches!(
            job_outcome(cycle_id, result),
            JobOutcome::Blocked { .. }
        ));
        let interrupted = fx.store.load(cycle_id).map_err(ctx("load interrupted"))?;
        assert!(
            interrupted.in_flight.is_some(),
            "unknown effects must retain the journal"
        );
        assert_eq!(interrupted.checkpoint.fence.sequence, 0);
        assert_eq!(interrupted.checkpoint.transitions_used, 0);
        assert!(interrupted.checkpoint.terminal.is_none());
        assert_eq!(
            interrupted.outbox.len(),
            0,
            "must not learn a no-effect failure"
        );
        assert_eq!(
            std::fs::read(&target).map_err(ctx("read effect"))?,
            b"owned edit"
        );
        drop(driver);

        // Reopen durable state, as a new worker after an explicit operator retry.
        let reopened = Arc::new(CycleStore::new(
            RecordStore::create_ambient(&fx.cycles_dir).map_err(ctx("reopen cycle store"))?,
        ));
        let mut resumed = CycleDriver::new(
            Arc::clone(&reopened),
            Script::new(vec![Ok(CycleProposal::Blocked {
                reason: "owner review required".to_owned(),
            })]),
            LostAcknowledgement {
                target,
                calls: Arc::clone(&calls),
            },
            Arc::new(FixedReissuer(rights()?)),
        );
        resumed
            .run(cycle_id, &token(&fx.work_id, 2), &fx.admission, &CancelToken::new())
            .await
            .map_err(ctx("resume reconciles"))?;
        assert_eq!(calls.load(Ordering::SeqCst), 1, "effect must not execute twice");
        let recovered = reopened.load(cycle_id).map_err(ctx("load recovered"))?;
        assert!(recovered.in_flight.is_none());
        assert_eq!(recovered.checkpoint.fence.epoch, 2);
        assert_eq!(recovered.checkpoint.fence.sequence, 2);
        assert_eq!(recovered.checkpoint.terminal, Some(CycleTerminal::Blocked));
        Ok(())
    }

    #[tokio::test]
    async fn uncertain_effect_that_cannot_be_reconciled_escalates_without_another_execution()
    -> TestResult {
        let cycle_id = "cycle-unknown-effect";
        let fx = fixture(cycle_id)?;
        let mut executor = Executor::new(vec![Err(StepFailure::uncertain("lost reply"))]);
        executor.reconciliation = StepReconciliation::Uncertain;
        let executed = Arc::clone(&executor.executed);
        let mut driver = driver(&fx, Script::new(vec![Ok(wait())]), executor, rights()?);
        let lease = token(&fx.work_id, 1);
        assert!(matches!(
            driver
                .run(cycle_id, &lease, &fx.admission, &CancelToken::new())
                .await,
            Err(CycleRunError::EffectUncertain)
        ));
        // Even an explicit retry with the same lease enters reconciliation,
        // not execute or the proposer, and unresolved effects escalate.
        let result = driver
            .run(cycle_id, &lease, &fx.admission, &CancelToken::new())
            .await
            .map_err(ctx("reconciliation escalates"))?;
        assert!(matches!(
            result,
            CycleRunOutcome::Terminal {
                terminal: CycleTerminal::Escalated,
                ..
            }
        ));
        assert_eq!(
            executed.lock().map_err(|_| TestError::Missing("execution counter"))?.len(),
            1
        );
        let record = fx.store.load(cycle_id).map_err(ctx("load escalation"))?;
        assert_eq!(record.checkpoint.fence.sequence, 1);
        assert!(record.in_flight.is_none());
        assert_eq!(record.outbox.len(), 1);
        let entry = record.outbox.pending().first()
            .ok_or(TestError::Missing("uncertain observation"))?;
        assert_ne!(entry.payload.category, ObservationCategory::ToolFailedNoEffect);
        Ok(())
    }

    #[tokio::test]
    async fn executor_failure_without_effect_is_observed_as_tool_failed_no_effect() -> TestResult {
        let fx = fixture("cycle-obs-tool")?;
        let proposer = Script::new(vec![
            Ok(wait()),
            Ok(CycleProposal::Failed {
                reason: "stop".to_owned(),
            }),
        ]);
        let executor = Executor::new(vec![Err(StepFailure::new("egress denied by gateway"))]);
        let mut driver = driver(&fx, proposer, executor, rights()?);
        driver
            .run(
                "cycle-obs-tool",
                &token(&fx.work_id, 1),
                &fx.admission,
                &CancelToken::new(),
            )
            .await
            .map_err(ctx("driver runs"))?;
        let pending = fx
            .store
            .pending_observations("cycle-obs-tool", &token(&fx.work_id, 1))
            .map_err(ctx("pending observations"))?;
        assert_eq!(pending.len(), 1, "only the failed step is observed");
        let entry = pending.first().ok_or(TestError::Missing("one entry"))?;
        let payload = &entry.payload;
        assert_eq!(payload.category, ObservationCategory::ToolFailedNoEffect);
        assert_eq!(payload.cause, CauseCode::ExecutorFailed);
        assert_eq!(payload.outcome, ObservationOutcome::Pending);
        assert_eq!(payload.transition, "wait");
        assert_eq!(payload.event_id.sequence, 0);
        assert_eq!(entry.sequence, 1);
        let json = serde_json::to_string(payload).map_err(ctx("observation serialises"))?;
        assert!(
            !json.contains("egress denied"),
            "observations carry no failure text: {json}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn permanent_proposer_failure_is_provider_unavailable_not_a_tool_failure() -> TestResult {
        // Provider and authentication failures `ModelCycleProposer` reports
        // as permanent: the cycle blocks, no executor runs, and the
        // observation names the proposer, not a tool.
        for (cycle_id, reason) in [
            ("cycle-obs-403", "HTTP 403 model route"),
            ("cycle-obs-noroute", "model route not configured"),
            ("cycle-obs-auth", "authentication circuit open"),
        ] {
            let fx = fixture(cycle_id)?;
            let proposer = Script::new(vec![Err(StepFailure::new(reason))]);
            let executor = Executor::new(vec![]);
            let executed = Arc::clone(&executor.executed);
            let mut driver = driver(&fx, proposer, executor, rights()?);
            let lease = token(&fx.work_id, 1);
            let outcome = driver
                .run(cycle_id, &lease, &fx.admission, &CancelToken::new())
                .await
                .map_err(ctx("driver runs"))?;
            assert!(
                matches!(
                    outcome,
                    CycleRunOutcome::Terminal {
                        terminal: CycleTerminal::Blocked,
                        ..
                    }
                ),
                "{reason}"
            );
            assert!(
                executed
                    .lock()
                    .map_err(|_| TestError::Missing("lock"))?
                    .is_empty(),
                "no executor ran for {reason}"
            );
            let pending = fx
                .store
                .pending_observations(cycle_id, &lease)
                .map_err(ctx("pending observations"))?;
            assert_eq!(pending.len(), 1, "one observation for {reason}");
            let entry = pending.first().ok_or(TestError::Missing("one entry"))?;
            let payload = &entry.payload;
            assert_eq!(payload.category, ObservationCategory::ProviderUnavailable);
            assert_eq!(payload.cause, CauseCode::ProposerFailed);
            assert_eq!(payload.transition, "blocked");
            assert!(
                pending.iter().all(|entry| {
                    entry.payload.category != ObservationCategory::ToolFailedNoEffect
                        && entry.payload.cause != CauseCode::ExecutorFailed
                }),
                "a proposer failure is never a tool failure: {reason}"
            );
            let json = serde_json::to_string(payload).map_err(ctx("observation serialises"))?;
            assert!(
                !json.contains(reason),
                "observations carry no failure text: {json}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn uncertain_reconciliation_is_inconclusive_observation() -> TestResult {
        let fx = fixture("cycle-obs-uncertain")?;
        let step = crash_mid_step(&fx, "cycle-obs-uncertain")?;
        let mut executor = Executor::new(vec![]);
        executor.reconciliation = StepReconciliation::Uncertain;
        let mut driver = driver(&fx, Script::new(vec![]), executor, rights()?);
        let outcome = driver
            .run(
                "cycle-obs-uncertain",
                &token(&fx.work_id, 2),
                &fx.admission,
                &CancelToken::new(),
            )
            .await
            .map_err(ctx("driver resumes"))?;
        assert!(matches!(
            outcome,
            CycleRunOutcome::Terminal {
                terminal: CycleTerminal::Escalated,
                ..
            }
        ));
        let pending = fx
            .store
            .pending_observations("cycle-obs-uncertain", &token(&fx.work_id, 2))
            .map_err(ctx("pending observations"))?;
        assert_eq!(pending.len(), 1);
        let entry = pending.first().ok_or(TestError::Missing("one entry"))?;
        let payload = &entry.payload;
        assert_eq!(payload.category, ObservationCategory::UncertainEffect);
        assert_eq!(payload.cause, CauseCode::ReconcileUncertain);
        assert_eq!(payload.outcome, ObservationOutcome::Inconclusive);
        assert_eq!(payload.transition, "wait", "the journaled step's kind");
        assert_eq!(payload.event_id.sequence, step.sequence);
        assert_eq!(payload.epoch, 2, "recorded under the recovering lease");
        Ok(())
    }

    #[test]
    fn stale_epoch_cannot_ack_observations() -> TestResult {
        let fx = fixture("cycle-obs-stale")?;
        let keys = commit_observed(&fx, "cycle-obs-stale", 2)?;
        assert!(matches!(
            fx.store
                .ack_observations("cycle-obs-stale", &token(&fx.work_id, 0), &keys),
            Err(CycleStoreError::Fence(FenceRefusal::StaleEpoch))
        ));
        assert_eq!(
            fx.store
                .pending_observations("cycle-obs-stale", &token(&fx.work_id, 1))
                .map_err(ctx("pending observations"))?
                .len(),
            2,
            "a refused ack changes nothing"
        );
        Ok(())
    }

    #[test]
    fn stale_epoch_cannot_read_pending_observations() -> TestResult {
        let fx = fixture("cycle-obs-read-stale")?;
        let keys = commit_observed(&fx, "cycle-obs-read-stale", 2)?;
        // Epoch 2 takes the cycle over; the epoch-1 drain is a zombie now.
        let record = fx.store.load("cycle-obs-read-stale").map_err(ctx("load"))?;
        assert_eq!(record.checkpoint.fence.epoch, 1);
        let owner = token(&fx.work_id, 2);
        let (_, taken_over) = resume_admission(&record.checkpoint, &fx.admission, &rights()?, 2)
            .map_err(|_| TestError::Missing("resume"))?;
        fx.store
            .rebind("cycle-obs-read-stale", &owner, &taken_over)
            .map_err(ctx("takeover"))?;
        let stale = fx
            .store
            .pending_observations("cycle-obs-read-stale", &token(&fx.work_id, 1));
        assert!(
            matches!(stale, Err(CycleStoreError::Fence(FenceRefusal::StaleEpoch))),
            "a stale epoch gets no payload: {stale:?}"
        );
        // A newer lease that has not rebound the cycle cannot read either.
        let unbound = fx
            .store
            .pending_observations("cycle-obs-read-stale", &token(&fx.work_id, 3));
        assert!(
            matches!(unbound, Err(CycleStoreError::NotRebound)),
            "a lease that has not rebound gets no payload: {unbound:?}"
        );
        // The current owner still sees every pending observation.
        let current = fx
            .store
            .pending_observations("cycle-obs-read-stale", &owner)
            .map_err(ctx("owner reads"))?;
        assert_eq!(
            current
                .iter()
                .map(|entry| entry.key.clone())
                .collect::<Vec<_>>(),
            keys
        );
        Ok(())
    }

    #[test]
    fn foreign_job_token_cannot_read_pending_observations() -> TestResult {
        let fx = fixture("cycle-obs-read-foreign")?;
        let keys = commit_observed(&fx, "cycle-obs-read-foreign", 1)?;
        let foreign = token(&WorkId::from_str("other-job"), 1);
        let refused = fx
            .store
            .pending_observations("cycle-obs-read-foreign", &foreign);
        assert!(
            matches!(refused, Err(CycleStoreError::WrongJob)),
            "another job's lease gets no payload: {refused:?}"
        );
        let owned = fx
            .store
            .pending_observations("cycle-obs-read-foreign", &token(&fx.work_id, 1))
            .map_err(ctx("owner reads"))?;
        assert_eq!(
            owned
                .iter()
                .map(|entry| entry.key.clone())
                .collect::<Vec<_>>(),
            keys
        );
        Ok(())
    }

    #[test]
    fn ack_is_idempotent_on_redelivery() -> TestResult {
        let fx = fixture("cycle-obs-ack")?;
        let keys = commit_observed(&fx, "cycle-obs-ack", 2)?;
        let lease = token(&fx.work_id, 1);
        let first = keys.get(..1).ok_or(TestError::Missing("first key"))?;
        assert_eq!(
            fx.store
                .ack_observations("cycle-obs-ack", &lease, first)
                .map_err(ctx("first ack"))?,
            1
        );
        assert_eq!(
            fx.store
                .ack_observations("cycle-obs-ack", &lease, first)
                .map_err(ctx("redelivered ack"))?,
            0,
            "an already acknowledged key counts as 0"
        );
        let unknown = ["cycle-obs-ack:9:9".to_owned()];
        assert_eq!(
            fx.store
                .ack_observations("cycle-obs-ack", &lease, &unknown)
                .map_err(ctx("unknown ack"))?,
            0
        );
        assert_eq!(
            fx.store
                .ack_observations("cycle-obs-ack", &lease, &keys)
                .map_err(ctx("full redelivery"))?,
            1,
            "only the still-pending key is removed"
        );
        assert!(
            fx.store
                .pending_observations("cycle-obs-ack", &lease)
                .map_err(ctx("pending observations"))?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn crash_between_commit_and_ack_leaves_pending() -> TestResult {
        let fx = fixture("cycle-obs-crash")?;
        let keys = commit_observed(&fx, "cycle-obs-crash", 2)?;
        let lease = token(&fx.work_id, 1);
        let before = fx
            .store
            .pending_observations("cycle-obs-crash", &lease)
            .map_err(ctx("pending observations"))?;
        // The drain dies before acknowledging: a store reopened over the
        // same directory still holds every observation.
        let reopened = CycleStore::new(
            RecordStore::create_ambient(&fx.cycles_dir).map_err(ctx("store reopens"))?,
        );
        let after = reopened
            .pending_observations("cycle-obs-crash", &lease)
            .map_err(ctx("pending after reopen"))?;
        assert_eq!(after, before);
        assert_eq!(
            after
                .iter()
                .map(|entry| entry.key.clone())
                .collect::<Vec<_>>(),
            keys
        );
        assert_eq!(
            reopened
                .ack_observations("cycle-obs-crash", &lease, &keys)
                .map_err(ctx("ack after reopen"))?,
            2
        );
        Ok(())
    }

    #[test]
    fn full_outbox_drops_newest_but_commits_checkpoint() -> TestResult {
        let fx = fixture("cycle-obs-full")?;
        let capacity = u32::try_from(MAX_OUTBOX_PENDING).unwrap_or(u32::MAX);
        let keys = commit_observed(&fx, "cycle-obs-full", capacity.saturating_add(1))?;
        let record = fx.store.load("cycle-obs-full").map_err(ctx("reload"))?;
        assert_eq!(record.checkpoint.fence.sequence, 1, "checkpoint committed");
        assert_eq!(record.in_flight, None);
        assert_eq!(record.observations_dropped, 1);
        assert_eq!(record.outbox.len(), MAX_OUTBOX_PENDING);
        let kept: Vec<String> = record
            .outbox
            .pending()
            .iter()
            .map(|entry| entry.key.clone())
            .collect();
        assert_eq!(
            kept,
            keys.get(..MAX_OUTBOX_PENDING)
                .ok_or(TestError::Missing("oldest keys"))?
                .to_vec(),
            "the oldest observations stay, the newest is dropped"
        );
        let status = record.status();
        assert_eq!(status.pending_observations, MAX_OUTBOX_PENDING);
        assert_eq!(status.observations_dropped, 1);
        Ok(())
    }

    #[test]
    fn records_without_outbox_field_still_load() -> TestResult {
        let fx = fixture("cycle-obs-legacy")?;
        let record = fx.store.load("cycle-obs-legacy").map_err(ctx("load"))?;
        let mut value = serde_json::to_value(&record).map_err(ctx("record serialises"))?;
        let object = value
            .as_object_mut()
            .ok_or(TestError::Missing("record is a JSON object"))?;
        assert!(object.remove("outbox").is_some(), "outbox is serialised");
        assert!(
            object.remove("observations_dropped").is_some(),
            "observations_dropped is serialised"
        );
        let legacy = value.clone();
        let loaded: CycleRecord =
            serde_json::from_value(legacy).map_err(ctx("pre-W03c record loads"))?;
        assert!(loaded.outbox.is_empty());
        assert_eq!(loaded.observations_dropped, 0);
        assert_eq!(loaded, record);
        // Unknown fields stay refused.
        let mut tampered = value;
        tampered
            .as_object_mut()
            .ok_or(TestError::Missing("record is a JSON object"))?
            .insert("unexpected".to_owned(), serde_json::Value::Bool(true));
        assert!(serde_json::from_value::<CycleRecord>(tampered).is_err());
        Ok(())
    }

    /// A lease guard that refuses every token once `expired` is set and
    /// counts the holds that are still alive in `held`.
    struct FlagGuard {
        expired: Arc<AtomicBool>,
        held: Arc<AtomicUsize>,
        /// Once set, granted holds report their deadline as passed.
        deadline_passed: Arc<AtomicBool>,
    }

    /// One hold of [`FlagGuard`]; dropping it releases it.
    struct FlagHold {
        held: Arc<AtomicUsize>,
        deadline_passed: Arc<AtomicBool>,
    }

    impl Drop for FlagHold {
        fn drop(&mut self) {
            self.held.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl LeaseHold for FlagHold {
        fn still_live(&self) -> Result<(), String> {
            if self.deadline_passed.load(Ordering::SeqCst) {
                Err("lease deadline passed during the operation".to_owned())
            } else {
                Ok(())
            }
        }
    }

    impl LeaseLivenessGuard for FlagGuard {
        fn hold(&self, _token: &LeaseToken) -> Result<Box<dyn LeaseHold>, String> {
            if self.expired.load(Ordering::SeqCst) {
                return Err("lease expired at the job store".to_owned());
            }
            self.held.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FlagHold {
                held: Arc::clone(&self.held),
                deadline_passed: Arc::clone(&self.deadline_passed),
            }))
        }
    }

    fn flag_liveness(expired: &Arc<AtomicBool>, held: &Arc<AtomicUsize>) -> Arc<FlagGuard> {
        Arc::new(FlagGuard {
            expired: Arc::clone(expired),
            held: Arc::clone(held),
            deadline_passed: Arc::new(AtomicBool::new(false)),
        })
    }

    /// `keys` accepted with a stand-in materialisation each.
    fn accepting(keys: &[String]) -> Vec<(String, serde_json::Value)> {
        keys.iter()
            .map(|key| (key.clone(), serde_json::json!({ "fact": key })))
            .collect()
    }

    /// A hold whose deadline holds for its first `live_checks` checks — the
    /// one at the end of the mutation closure — and has passed by the next
    /// one, the pre-rename commit check after the temp file's fsync.
    struct FsyncStallHold {
        checks: AtomicUsize,
        live_checks: usize,
    }

    impl LeaseHold for FsyncStallHold {
        fn still_live(&self) -> Result<(), String> {
            if self.checks.fetch_add(1, Ordering::SeqCst) < self.live_checks {
                Ok(())
            } else {
                Err("lease deadline passed during the fsync".to_owned())
            }
        }
    }

    struct FsyncStallGuard;

    impl LeaseLivenessGuard for FsyncStallGuard {
        fn hold(&self, _token: &LeaseToken) -> Result<Box<dyn LeaseHold>, String> {
            Ok(Box::new(FsyncStallHold {
                checks: AtomicUsize::new(0),
                live_checks: 1,
            }))
        }
    }

    fn file_names(dir: &std::path::Path) -> TestResult<BTreeSet<String>> {
        let mut names = BTreeSet::new();
        for entry in std::fs::read_dir(dir).map_err(ctx("read dir"))? {
            let entry = entry.map_err(ctx("dir entry"))?;
            names.insert(entry.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
    }

    #[test]
    fn deadline_passed_during_fsync_is_refused_before_rename() -> TestResult {
        let id = "cycle-fsync-stall";
        let fx = fixture(id)?;
        let keys = commit_observed(&fx, id, 2)?;
        let lease = token(&fx.work_id, 1);
        let guarded = guarded_store(&fx, Arc::new(FsyncStallGuard))?;
        let record = fx.store.load(id).map_err(ctx("load"))?;
        let records_dir = fx.cycles_dir.join(harw_job_store::RECORDS_DIR);
        let files = file_names(&records_dir)?;
        let admitted = AdmittedCycle::recovered(wait(), record.checkpoint.fence.sequence);
        let (_, taken_over) = resume_admission(&record.checkpoint, &fx.admission, &rights()?, 2)
            .map_err(|_| TestError::Missing("resume"))?;

        // Each mutation closure passes its deadline check; the deadline is
        // gone by the pre-rename check after the fsync: nothing is published.
        let outcomes = [
            guarded.begin_step(id, &lease, &admitted).err(),
            guarded
                .decide_observations(id, &lease, &accepting(&keys), &[])
                .err(),
            guarded.ack_observations(id, &lease, &keys).err(),
            guarded
                .rebind(id, &token(&fx.work_id, 2), &taken_over)
                .err(),
        ];
        for outcome in &outcomes {
            assert!(
                matches!(outcome, Some(CycleStoreError::LeaseNotLive(reason)) if reason.contains("during the fsync")),
                "{outcome:?}"
            );
        }
        assert_eq!(
            fx.store.load(id).map_err(ctx("reload"))?,
            record,
            "nothing was published after the deadline"
        );
        assert_eq!(file_names(&records_dir)?, files, "no temp file remains");
        Ok(())
    }

    /// `true` for a refusal by the hold's final deadline check.
    fn past_deadline<T>(result: &Result<T, CycleStoreError>) -> bool {
        matches!(result, Err(CycleStoreError::LeaseNotLive(reason)) if reason.contains("deadline passed"))
    }

    #[test]
    fn commit_after_the_hold_deadline_is_refused() -> TestResult {
        let id = "cycle-deadline";
        let fx = fixture(id)?;
        let keys = commit_observed(&fx, id, 2)?;
        let lease = token(&fx.work_id, 1);
        let expired = Arc::new(AtomicBool::new(false));
        let held = Arc::new(AtomicUsize::new(0));
        let guard = flag_liveness(&expired, &held);
        let deadline_passed = Arc::clone(&guard.deadline_passed);
        let guarded = guarded_store(&fx, guard)?;
        let record = fx.store.load(id).map_err(ctx("load"))?;
        let admitted = AdmittedCycle::recovered(wait(), record.checkpoint.fence.sequence);
        let (_, taken_over) = resume_admission(&record.checkpoint, &fx.admission, &rights()?, 2)
            .map_err(|_| TestError::Missing("resume"))?;

        // Every hold is granted (the lease was live when it was taken), but
        // its deadline passes while the operation runs: the last check
        // refuses before anything is published or handed out.
        deadline_passed.store(true, Ordering::SeqCst);
        let begin = guarded.begin_step(id, &lease, &admitted);
        assert!(past_deadline(&begin), "{begin:?}");
        let decided = guarded.decide_observations(id, &lease, &accepting(&keys), &[]);
        assert!(past_deadline(&decided), "{decided:?}");
        let acked = guarded.ack_observations(id, &lease, &keys);
        assert!(past_deadline(&acked), "{acked:?}");
        let pending = guarded.pending_observations(id, &lease);
        assert!(past_deadline(&pending), "{pending:?}");
        let rebound = guarded.rebind(id, &token(&fx.work_id, 2), &taken_over);
        assert!(past_deadline(&rebound), "{rebound:?}");
        assert_eq!(
            fx.store.load(id).map_err(ctx("reload"))?,
            record,
            "nothing was published after the deadline"
        );

        // With a step in flight, its commit and clear are refused the same way.
        deadline_passed.store(false, Ordering::SeqCst);
        let step = guarded
            .begin_step(id, &lease, &admitted)
            .map_err(ctx("begin within the deadline"))?;
        let journaled = fx.store.load(id).map_err(ctx("journaled record"))?;
        let next = apply_observations(
            fx.admission.intent(),
            &journaled.checkpoint,
            &admitted,
            CycleObservations::default(),
        )
        .map_err(|_| TestError::Missing("apply"))?;
        deadline_passed.store(true, Ordering::SeqCst);
        let committed = guarded.commit_step(id, &lease, &next);
        assert!(past_deadline(&committed), "{committed:?}");
        let observed = guarded.commit_step_observed(
            id,
            &lease,
            &next,
            &[sample_observation(id, &journaled.checkpoint, 9)],
        );
        assert!(past_deadline(&observed), "{observed:?}");
        let cleared = guarded.clear_step(id, &lease, &step);
        assert!(past_deadline(&cleared), "{cleared:?}");
        let proof = guarded.journaled_step(id, &lease);
        assert!(past_deadline(&proof), "{proof:?}");
        assert_eq!(
            fx.store.load(id).map_err(ctx("reload after the step"))?,
            journaled,
            "the journaled step stays as it was"
        );
        assert_eq!(held.load(Ordering::SeqCst), 0, "every hold was released");
        Ok(())
    }

    #[tokio::test]
    async fn terminal_cycle_is_rebound_so_a_later_lease_can_drain_its_outbox() -> TestResult {
        let id = "cycle-terminal-handover";
        let fx = fixture(id)?;
        // Epoch 1 ends the cycle; the proposer failure is observed and
        // committed together with the terminal step, but never acknowledged.
        let proposer = Script::new(vec![Err(StepFailure::new("HTTP 403 model route"))]);
        let mut first = driver(&fx, proposer, Executor::new(vec![]), rights()?);
        let ended = first
            .run(
                id,
                &token(&fx.work_id, 1),
                &fx.admission,
                &CancelToken::new(),
            )
            .await
            .map_err(ctx("epoch 1 runs"))?;
        assert!(matches!(
            ended,
            CycleRunOutcome::Terminal {
                terminal: CycleTerminal::Blocked,
                ..
            }
        ));
        let terminal = fx.store.load(id).map_err(ctx("terminal record"))?;
        assert_eq!(terminal.checkpoint.fence.epoch, 1);
        assert_eq!(
            terminal.outbox.len(),
            1,
            "the terminal commit's observation"
        );
        let later = token(&fx.work_id, 2);
        assert!(matches!(
            fx.store.pending_observations(id, &later),
            Err(CycleStoreError::NotRebound)
        ));

        // Epoch 2 (a reclaim) finds the cycle terminal: nothing is proposed
        // or executed, but the record is handed over to the new lease.
        let proposer = Script::new(vec![]);
        let calls = Arc::clone(&proposer.refusals);
        let executor = Executor::new(vec![]);
        let executed = Arc::clone(&executor.executed);
        let mut second = driver(&fx, proposer, executor, rights()?);
        let outcome = second
            .run(id, &later, &fx.admission, &CancelToken::new())
            .await
            .map_err(ctx("epoch 2 runs"))?;
        let CycleRunOutcome::Terminal {
            terminal: kind,
            checkpoint,
        } = outcome
        else {
            return Err(TestError::Missing("terminal outcome"));
        };
        assert_eq!(kind, CycleTerminal::Blocked);
        assert_eq!(checkpoint.fence.epoch, 2);
        assert_eq!(
            checkpoint.fence.sequence,
            terminal.checkpoint.fence.sequence
        );
        assert!(
            calls
                .lock()
                .map_err(|_| TestError::Missing("lock"))?
                .is_empty()
        );
        assert!(
            executed
                .lock()
                .map_err(|_| TestError::Missing("lock"))?
                .is_empty()
        );
        let handed = fx.store.load(id).map_err(ctx("handed-over record"))?;
        assert_eq!(handed.checkpoint, checkpoint);
        let mut unchanged = handed.checkpoint.clone();
        unchanged.fence.epoch = terminal.checkpoint.fence.epoch;
        assert_eq!(unchanged, terminal.checkpoint, "only the epoch moved");

        // The new lease drains what the earlier one left; the old one cannot.
        let pending = fx
            .store
            .pending_observations(id, &later)
            .map_err(ctx("pending under the new lease"))?;
        assert_eq!(pending.len(), 1);
        assert!(matches!(
            fx.store.pending_observations(id, &token(&fx.work_id, 1)),
            Err(CycleStoreError::Fence(FenceRefusal::StaleEpoch))
        ));
        let keys: Vec<String> = pending.iter().map(|entry| entry.key.clone()).collect();
        assert_eq!(
            fx.store
                .ack_observations(id, &later, &keys)
                .map_err(ctx("ack under the new lease"))?,
            1
        );
        assert!(
            fx.store
                .pending_observations(id, &later)
                .map_err(ctx("pending after ack"))?
                .is_empty()
        );

        // Running again under the bound epoch changes nothing.
        let mut third = driver(&fx, Script::new(vec![]), Executor::new(vec![]), rights()?);
        third
            .run(id, &later, &fx.admission, &CancelToken::new())
            .await
            .map_err(ctx("epoch 2 runs again"))?;
        assert_eq!(
            fx.store.load(id).map_err(ctx("reload"))?.checkpoint,
            checkpoint
        );
        Ok(())
    }

    /// The fixture's records behind a second store that holds `guard`.
    fn guarded_store(fx: &Fixture, guard: Arc<dyn LeaseLivenessGuard>) -> TestResult<CycleStore> {
        let records =
            RecordStore::create_ambient(&fx.cycles_dir).map_err(ctx("cycle store reopens"))?;
        Ok(CycleStore::new(records).with_lease_liveness(guard))
    }

    fn is_not_live<T>(result: &Result<T, CycleStoreError>) -> bool {
        matches!(result, Err(CycleStoreError::LeaseNotLive(_)))
    }

    #[test]
    fn liveness_check_refuses_every_fenced_operation() -> TestResult {
        let id = "cycle-liveness";
        let fx = fixture(id)?;
        // Epoch 1 owns the cycle and two observations are pending.
        let keys = commit_observed(&fx, id, 2)?;
        let lease = token(&fx.work_id, 1);
        let expired = Arc::new(AtomicBool::new(false));
        let held = Arc::new(AtomicUsize::new(0));
        let guarded = guarded_store(&fx, flag_liveness(&expired, &held))?;
        let record = fx.store.load(id).map_err(ctx("load"))?;
        assert_eq!(record.checkpoint.fence.epoch, lease.epoch);
        let admitted = AdmittedCycle::recovered(wait(), record.checkpoint.fence.sequence);

        // Expired with no step in flight: the journal stays empty.
        expired.store(true, Ordering::SeqCst);
        let begin = guarded.begin_step(id, &lease, &admitted);
        assert!(is_not_live(&begin), "{begin:?}");
        assert_eq!(
            fx.store.load(id).map_err(ctx("reload"))?,
            record,
            "a refused begin writes nothing"
        );

        // Live again: the same store journals the step and serves the reads.
        expired.store(false, Ordering::SeqCst);
        let step = guarded
            .begin_step(id, &lease, &admitted)
            .map_err(ctx("begin while live"))?;
        assert_eq!(
            guarded
                .pending_observations(id, &lease)
                .map_err(ctx("pending while live"))?
                .len(),
            2
        );
        guarded
            .journaled_step(id, &lease)
            .map_err(ctx("journaled step while live"))?;
        assert_eq!(
            held.load(Ordering::SeqCst),
            0,
            "every hold is released when its operation returns"
        );

        // The job lease expires and no reclaim rebinds the cycle: epoch 1 is
        // still the record's epoch, so the epoch fence alone admits it.
        expired.store(true, Ordering::SeqCst);
        let before = fx.store.load(id).map_err(ctx("load before"))?;
        assert!(
            fx.store.pending_observations(id, &lease).is_ok(),
            "the epoch fence alone still admits the expired lease"
        );
        let next = apply_observations(
            fx.admission.intent(),
            &before.checkpoint,
            &admitted,
            CycleObservations::default(),
        )
        .map_err(|_| TestError::Missing("apply"))?;
        let observation = sample_observation(id, &before.checkpoint, 7);
        let (_, taken_over) = resume_admission(&before.checkpoint, &fx.admission, &rights()?, 2)
            .map_err(|_| TestError::Missing("resume"))?;

        let pending = guarded.pending_observations(id, &lease);
        assert!(is_not_live(&pending), "{pending:?}");
        let journaled = guarded.journaled_step(id, &lease);
        assert!(is_not_live(&journaled), "{journaled:?}");
        let acked = guarded.ack_observations(id, &lease, &keys);
        assert!(is_not_live(&acked), "{acked:?}");
        let decided = guarded.decide_observations(id, &lease, &accepting(&keys), &[]);
        assert!(is_not_live(&decided), "{decided:?}");
        let committed = guarded.commit_step(id, &lease, &next);
        assert!(is_not_live(&committed), "{committed:?}");
        let observed = guarded.commit_step_observed(id, &lease, &next, &[observation]);
        assert!(is_not_live(&observed), "{observed:?}");
        let cleared = guarded.clear_step(id, &lease, &step);
        assert!(is_not_live(&cleared), "{cleared:?}");
        let rebound = guarded.rebind(id, &token(&fx.work_id, 2), &taken_over);
        assert!(is_not_live(&rebound), "{rebound:?}");
        let refused = rebound
            .err()
            .ok_or(TestError::Missing("rebind refusal"))?
            .to_string();
        assert_eq!(
            refused,
            "job lease is not live: lease expired at the job store"
        );

        assert_eq!(
            fx.store.load(id).map_err(ctx("reload after refusals"))?,
            before,
            "no refused operation wrote anything"
        );
        // Unfenced reads take no lease and stay available.
        assert_eq!(guarded.load(id).map_err(ctx("unfenced load"))?, before);
        Ok(())
    }

    #[test]
    fn store_without_liveness_check_keeps_the_epoch_fence_only() -> TestResult {
        let id = "cycle-no-liveness";
        let fx = fixture(id)?;
        let keys = commit_observed(&fx, id, 2)?;
        // Neither the fixture's store nor one reopened through the
        // constructor consults a job store: any nonce of the bound epoch
        // passes, older and not yet rebound epochs are still refused.
        let reopened = CycleStore::new(
            RecordStore::create_ambient(&fx.cycles_dir).map_err(ctx("store reopens"))?,
        );
        let shared: &CycleStore = &fx.store;
        let any_nonce = LeaseToken {
            nonce: "never-issued".to_owned(),
            ..token(&fx.work_id, 1)
        };
        for store in [shared, &reopened] {
            assert_eq!(
                store
                    .pending_observations(id, &any_nonce)
                    .map_err(ctx("pending under the bound epoch"))?
                    .len(),
                2
            );
            assert!(matches!(
                store.pending_observations(id, &token(&fx.work_id, 0)),
                Err(CycleStoreError::Fence(FenceRefusal::StaleEpoch))
            ));
            assert!(matches!(
                store.pending_observations(id, &token(&fx.work_id, 2)),
                Err(CycleStoreError::NotRebound)
            ));
        }
        assert_eq!(
            reopened
                .ack_observations(id, &any_nonce, &keys)
                .map_err(ctx("ack under the bound epoch"))?,
            2
        );
        assert!(
            shared
                .pending_observations(id, &any_nonce)
                .map_err(ctx("pending after ack"))?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn decide_observations_accepts_and_acks_in_one_fenced_write() -> TestResult {
        let id = "cycle-decide";
        let fx = fixture(id)?;
        let keys = commit_observed(&fx, id, 3)?;
        let lease = token(&fx.work_id, 1);
        let (first, second, third) = match keys.as_slice() {
            [first, second, third] => (first.clone(), second.clone(), third.clone()),
            _ => return Err(TestError::Missing("three observation keys")),
        };
        let state = |store: &CycleStore, key: &str| -> TestResult<Option<DeliveryState>> {
            Ok(store
                .load(id)
                .map_err(ctx("load"))?
                .outbox
                .pending()
                .iter()
                .find(|entry| entry.key == key)
                .map(|entry| entry.state))
        };

        assert!(matches!(
            fx.store.decide_observations(
                id,
                &token(&fx.work_id, 0),
                &accepting(std::slice::from_ref(&first)),
                &[]
            ),
            Err(CycleStoreError::Fence(FenceRefusal::StaleEpoch))
        ));
        assert_eq!(state(&fx.store, &first)?, Some(DeliveryState::Pending));

        let decided = fx
            .store
            .decide_observations(
                id,
                &lease,
                &accepting(std::slice::from_ref(&first)),
                std::slice::from_ref(&second),
            )
            .map_err(ctx("decide"))?;
        assert_eq!(decided, (1, 1));
        // Durable: a store reopened over the same directory sees the decision.
        let reopened = CycleStore::new(
            RecordStore::create_ambient(&fx.cycles_dir).map_err(ctx("store reopens"))?,
        );
        assert_eq!(state(&reopened, &first)?, Some(DeliveryState::Accepted));
        let stored = reopened
            .load(id)
            .map_err(ctx("load"))?
            .outbox
            .pending()
            .iter()
            .find(|entry| entry.key == first)
            .and_then(|entry| entry.materialization.clone());
        assert_eq!(
            stored,
            Some(serde_json::json!({ "fact": first })),
            "the materialisation is stored with the decision"
        );
        assert_eq!(state(&reopened, &second)?, None, "acknowledged directly");
        assert_eq!(state(&reopened, &third)?, Some(DeliveryState::Pending));
        let pending = reopened
            .pending_observations(id, &lease)
            .map_err(ctx("pending"))?;
        assert_eq!(pending.len(), 2, "accepted entries stay pending");

        // A replayed decision changes nothing; the accepted entry leaves only
        // with its acknowledgement.
        assert_eq!(
            reopened
                .decide_observations(
                    id,
                    &lease,
                    &accepting(std::slice::from_ref(&first)),
                    std::slice::from_ref(&second),
                )
                .map_err(ctx("replayed decision"))?,
            (0, 0)
        );
        assert_eq!(
            reopened
                .ack_observations(id, &lease, std::slice::from_ref(&first))
                .map_err(ctx("ack the delivered entry"))?,
            1
        );
        assert_eq!(state(&reopened, &first)?, None);
        assert_eq!(state(&reopened, &third)?, Some(DeliveryState::Pending));
        Ok(())
    }

    #[tokio::test]
    async fn driver_fails_closed_when_the_job_lease_is_not_live() -> TestResult {
        let id = "cycle-dead-lease";
        let fx = fixture(id)?;
        let expired = Arc::new(AtomicBool::new(true));
        let held = Arc::new(AtomicUsize::new(0));
        let guarded = Arc::new(guarded_store(&fx, flag_liveness(&expired, &held))?);
        let before = fx.store.load(id).map_err(ctx("load"))?;
        let proposer = Script::new(vec![Ok(CycleProposal::Complete {})]);
        let executor = Executor::new(vec![]);
        let executed = Arc::clone(&executor.executed);
        let mut driver = CycleDriver::new(
            guarded,
            proposer,
            executor,
            Arc::new(FixedReissuer(rights()?)),
        );
        let result = driver
            .run(
                id,
                &token(&fx.work_id, 1),
                &fx.admission,
                &CancelToken::new(),
            )
            .await;
        let Err(error) = result else {
            return Err(TestError::Missing("a dead lease must end the run"));
        };
        assert!(
            matches!(
                error,
                CycleRunError::Store(CycleStoreError::LeaseNotLive(_))
            ),
            "{error}"
        );
        let JobOutcome::Failed { reason } = job_outcome(id, Err(error)) else {
            return Err(TestError::Missing("a dead lease fails the job"));
        };
        assert!(reason.contains("job lease is not live"), "{reason}");
        assert_eq!(
            fx.store.load(id).map_err(ctx("reload"))?,
            before,
            "nothing was rebound or committed"
        );
        assert!(
            executed
                .lock()
                .map_err(|_| TestError::Missing("lock"))?
                .is_empty()
        );
        Ok(())
    }
}
