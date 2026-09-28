//! What the coordinator needs from a job store beyond [`JobRecordStore`].
//!
//! [`JobRecordStore`] covers the fenced record mutations. The coordinator
//! additionally reads a single record (to rebuild a claim after a restart)
//! and keeps one attempt sidecar per attempt (see [`super::attempt`]).
//! [`FsJobRecordStore`] implements both on top of
//! [`harw_job_store::RecordStore::write_sidecar`].

use harw_job_core::StoredJob;
use harw_job_store::{FsJobRecordStore, JobRecordStore, StoreResult};
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

    fn write_attempt(&self, attempt: &str, bytes: &[u8]) -> StoreResult<()> {
        self.records()
            .write_sidecar(ATTEMPTS_SIDECAR, attempt, bytes)
    }

    fn read_attempt(&self, attempt: &str) -> StoreResult<Option<Vec<u8>>> {
        self.records().read_sidecar(ATTEMPTS_SIDECAR, attempt)
    }
}
