//! The backend-independent job store contract (Job-Runtime-Doc §7) and its
//! file-backed implementation.
//!
//! The trait is named `JobRecordStore` because `harw-session-store` already
//! exports a `JobStore` (the Harwness adapter with approvals, trusted-actor
//! provenance and lifecycle event publication). That adapter delegates its
//! file mechanics to [`RecordStore`] and [`crate::mechanics`].
//!
//! # State model
//! Records are [`StoredJob`]s whose state is the governance
//! [`harw_job_core::JobState`]. The finer attempt lifecycle
//! ([`harw_job_core::LifecycleState`]) is not persisted here yet; the
//! transitions this contract offers ([`JobTransition`]) are exactly the ones
//! a lease holder may perform, each gated by its fencing token.

use std::path::Path;

use cap_std::fs::Dir;
use harw_job_core::{JobCancellation, JobClaim, JobOutcome, JobState, RunnerId, StoredJob, WorkId};
use jiff::{SignedDuration, Timestamp};

use crate::error::{Conflict, StoreError, StoreResult};
use crate::mechanics::{self, Cancelled, Reclaimed};
use crate::record_store::RecordStore;

/// Failure reason recorded when an expired lease exhausts the retry budget.
pub const LEASE_EXPIRED_EXHAUSTED_REASON: &str =
    "worker lease expired and retry budget is exhausted";

/// Lease terms of a claim; the store, never the runner, allocates the epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimTerms {
    /// Validity window of the lease (must be positive).
    pub lease_ttl: SignedDuration,
    /// Server time of the claim (injected for testability).
    pub now: Timestamp,
}

/// A mutation a lease holder performs on its claimed job.
#[derive(Debug, Clone, PartialEq)]
pub enum JobTransition {
    /// Heartbeat: extend the lease by its TTL from `now`.
    Renew {
        /// Server time of the heartbeat.
        now: Timestamp,
    },
    /// Record the terminal (or blocked) outcome and drop the lease.
    Complete {
        /// Server time of completion.
        completed_at: Timestamp,
        /// The outcome.
        outcome: JobOutcome,
    },
}

/// Durable job state, independent of the execution backend
/// (Job-Runtime-Doc §7). A stale runner can never mutate a job after losing
/// its lease: every transition is checked against the record's fencing epoch.
pub trait JobRecordStore {
    /// Persists a new job record; an existing id is never overwritten.
    ///
    /// # Errors
    /// [`StoreError::AlreadyExists`], [`StoreError::InvalidId`], I/O.
    fn create_job(&self, record: StoredJob) -> StoreResult<StoredJob>;

    /// Atomically claims a `Ready`, eligible job for `runner`.
    ///
    /// # Errors
    /// [`StoreError::Conflict`] (state, eligibility, TTL, epoch),
    /// [`StoreError::NotFound`], [`StoreError::Contended`], I/O.
    fn claim_job(
        &self,
        job: &WorkId,
        runner: &RunnerId,
        terms: ClaimTerms,
    ) -> StoreResult<JobClaim>;

    /// Applies `transition` if `claim` still holds the current lease.
    ///
    /// # Errors
    /// [`StoreError::StaleLease`] for a lost/superseded/expired lease
    /// (nothing is written), [`StoreError::Conflict`], I/O.
    fn transition(&self, claim: &JobClaim, transition: JobTransition) -> StoreResult<StoredJob>;

    /// Every `Running` job whose lease is held by `runner` — the set a
    /// restarted runner must reconcile (Job-Runtime-Doc §15).
    ///
    /// # Errors
    /// I/O on the records directory.
    fn load_recovery_set(&self, runner: &RunnerId) -> StoreResult<Vec<StoredJob>>;
}

/// One page of [`FsJobRecordStore::reconcile_expired`].
#[derive(Debug, Clone, PartialEq)]
pub struct ExpiryPage {
    /// Revoked leases on this page, with the job id and new state.
    pub expired: Vec<(WorkId, JobState, Reclaimed)>,
    /// Cursor over *scanned* `Running` jobs; `None` when done.
    pub next_cursor: Option<WorkId>,
}

/// [`JobRecordStore`] over a [`RecordStore`] of [`StoredJob`]s.
pub struct FsJobRecordStore {
    records: RecordStore<StoredJob>,
}

impl FsJobRecordStore {
    /// Uses an opened capability directory as store root.
    ///
    /// # Errors
    /// [`StoreError::Io`] if the layout cannot be created.
    pub fn from_dir(root: Dir) -> StoreResult<Self> {
        let records = RecordStore::from_dir(root);
        records.ensure_layout()?;
        Ok(Self { records })
    }

    /// Creates/opens the store at `path` (ambient bootstrap).
    ///
    /// # Errors
    /// [`StoreError::Io`].
    pub fn create_ambient(path: &Path) -> StoreResult<Self> {
        Ok(Self {
            records: RecordStore::create_ambient(path)?,
        })
    }

    /// The underlying generic record store.
    #[must_use]
    pub fn records(&self) -> &RecordStore<StoredJob> {
        &self.records
    }

    /// Loads one job.
    ///
    /// # Errors
    /// As [`RecordStore::load`].
    pub fn load(&self, job: &WorkId) -> StoreResult<StoredJob> {
        self.records.load(job.as_str())
    }

    /// Cancels a `Pending`, `Ready` or `Running` job, fencing a running
    /// lease before returning it.
    ///
    /// # Errors
    /// [`StoreError::Conflict`], [`StoreError::NotFound`], I/O.
    pub fn cancel_job(
        &self,
        job: &WorkId,
        cancellation: JobCancellation,
    ) -> StoreResult<Cancelled> {
        self.records.update_locked(job.as_str(), |record| {
            mechanics::cancel(record, cancellation)
        })
    }

    /// Revokes expired leases of up to `limit` `Running` jobs after
    /// `cursor`. Lock-contended, vanished, corrupt or unschedulable jobs are
    /// skipped (logged) instead of failing the page.
    ///
    /// # Errors
    /// I/O on the records directory or a persist failure.
    pub fn reconcile_expired(
        &self,
        now: Timestamp,
        limit: usize,
        cursor: Option<&WorkId>,
    ) -> StoreResult<ExpiryPage> {
        let candidates = self
            .records
            .list(cursor.map(WorkId::as_str), limit, |record| {
                record.job.state == JobState::Running
            })?;
        let mut expired = Vec::new();
        for record in candidates.records {
            let id = record.job.id.clone();
            let outcome = self.records.update_locked(id.as_str(), |record| {
                Ok::<_, StoreError>(
                    mechanics::expire_if_due(record, now, LEASE_EXPIRED_EXHAUSTED_REASON)?
                        .map(|reclaimed| (record.job.state, reclaimed)),
                )
            });
            match outcome {
                Ok(Some((state, reclaimed))) => expired.push((id, state, reclaimed)),
                Ok(None) => {}
                Err(
                    error @ (StoreError::Contended { .. }
                    | StoreError::NotFound { .. }
                    | StoreError::Corrupt { .. }
                    | StoreError::Conflict { .. }),
                ) => {
                    tracing::warn!(id = %id, error = %error, "expired-lease reconciliation skipped a job");
                }
                Err(error) => return Err(error),
            }
        }
        Ok(ExpiryPage {
            expired,
            next_cursor: candidates.next_cursor.map(WorkId::from_str),
        })
    }
}

impl JobRecordStore for FsJobRecordStore {
    fn create_job(&self, record: StoredJob) -> StoreResult<StoredJob> {
        self.records.create(&record)?;
        Ok(record)
    }

    fn claim_job(
        &self,
        job: &WorkId,
        runner: &RunnerId,
        terms: ClaimTerms,
    ) -> StoreResult<JobClaim> {
        if terms.lease_ttl <= SignedDuration::ZERO {
            return Err(StoreError::conflict(
                job.as_str(),
                Conflict::InvalidLeaseTtl,
            ));
        }
        self.records.update_locked(job.as_str(), |record| {
            let nonce = WorkId::new().as_str().to_owned();
            mechanics::claim(record, runner.as_str(), terms.lease_ttl, terms.now, nonce)
        })
    }

    fn transition(&self, claim: &JobClaim, transition: JobTransition) -> StoreResult<StoredJob> {
        self.records.update_locked(claim.job.id.as_str(), |record| {
            match transition {
                JobTransition::Renew { now } => {
                    mechanics::renew(record, &claim.token, now)?;
                }
                JobTransition::Complete {
                    completed_at,
                    outcome,
                } => {
                    mechanics::complete(record, &claim.token, completed_at, outcome)?;
                }
            }
            Ok(record.clone())
        })
    }

    fn load_recovery_set(&self, runner: &RunnerId) -> StoreResult<Vec<StoredJob>> {
        self.records.list_all(|record| {
            record.job.state == JobState::Running
                && record
                    .lease
                    .as_ref()
                    .is_some_and(|lease| lease.holder == runner.as_str())
        })
    }
}
