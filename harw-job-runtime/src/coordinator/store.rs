//! What the coordinator needs from a job store beyond [`JobRecordStore`].
//!
//! [`JobRecordStore`] covers the fenced record mutations. The coordinator
//! additionally reads a single record (to rebuild a claim after a restart)
//! and keeps one attempt sidecar per attempt (see [`super::attempt`]).
//! [`FsJobRecordStore`] implements both on top of
//! [`harw_job_store::RecordStore::write_sidecar`].

use harw_job_core::{JobClaim, RunnerId, StoredJob};
use harw_job_store::{
    ClaimTerms, Conflict, FsJobRecordStore, JobRecordStore, StoreError, StoreResult,
};
use harw_types::WorkId;

use super::attempt::ATTEMPTS_SIDECAR;

/// A [`JobRecordStore`] the coordinator can run on.
///
/// Implementations are shared between coordinator tasks and called from
/// Tokio's blocking pool.
pub trait CoordinatorStore: JobRecordStore + Send + Sync + 'static {
    /// Loads one job record.
    ///
    /// # Errors
    /// [`harw_job_store::StoreError::NotFound`], corrupt record, I/O.
    fn load_job(&self, job: &WorkId) -> StoreResult<StoredJob>;

    /// Claims exactly the record the caller validated, under the mutation lock.
    /// Any change (including input, scope, scheduling or cancellation) refuses
    /// the claim without allocating a lease. Backends must opt in atomically;
    /// a separate load followed by `claim_job` cannot satisfy this contract.
    ///
    /// # Errors
    /// Unsupported backend, changed record, or the normal claim errors.
    fn claim_existing_job(
        &self,
        _expected: StoredJob,
        _runner: &RunnerId,
        _terms: ClaimTerms,
    ) -> StoreResult<JobClaim> {
        Err(StoreError::Unsupported {
            operation: "claim_existing_job",
            detail: "store does not implement atomic validated-record claiming".to_owned(),
        })
    }

    /// Atomically writes (or replaces) the attempt sidecar `attempt`.
    ///
    /// # Errors
    /// Invalid id, I/O.
    fn write_attempt(&self, attempt: &str, bytes: &[u8]) -> StoreResult<()>;

    /// Reads the attempt sidecar `attempt`; `Ok(None)` if absent.
    ///
    /// # Errors
    /// Invalid id, I/O.
    fn read_attempt(&self, attempt: &str) -> StoreResult<Option<Vec<u8>>>;
}

impl CoordinatorStore for FsJobRecordStore {
    fn load_job(&self, job: &WorkId) -> StoreResult<StoredJob> {
        self.load(job)
    }

    fn claim_existing_job(
        &self,
        expected: StoredJob,
        runner: &RunnerId,
        terms: ClaimTerms,
    ) -> StoreResult<JobClaim> {
        self.records()
            .update_locked(expected.job.id.as_str(), |record| {
                if *record != expected {
                    return Err(StoreError::Conflict {
                        id: expected.job.id.as_str().to_owned(),
                        conflict: Conflict::Rejected {
                            detail: "job changed after process dispatch validation".to_owned(),
                        },
                    });
                }
                let nonce = WorkId::new().as_str().to_owned();
                harw_job_store::mechanics::claim(
                    record,
                    runner.as_str(),
                    terms.lease_ttl,
                    terms.now,
                    nonce,
                )
            })
    }

    fn write_attempt(&self, attempt: &str, bytes: &[u8]) -> StoreResult<()> {
        self.records()
            .write_sidecar(ATTEMPTS_SIDECAR, attempt, bytes)
    }

    fn read_attempt(&self, attempt: &str) -> StoreResult<Option<Vec<u8>>> {
        self.records().read_sidecar(ATTEMPTS_SIDECAR, attempt)
    }
}
