//! Error type for the append-only transcript store.
//!
//! Spec: `docs/design/knowledge-surfaces.md` §1.4 and the
//! `harw-session-store` row of `docs/design/crates-inventory.md`.
//! `#[derive(HarwError)]` emits `Display`, `std::error::Error` (with
//! `source()` wired through `#[from]` variants) and the `SessionStoreResult<T>`
//! alias (the enum name ends in `Error`). No `anyhow`/`thiserror`.

use harw_job_runtime::JobState;
use harw_macros::HarwError;
use harw_types::{CgroupId, FindingId, ItemId, SessionId, WorkId};

/// Central error class of the session-store layer (emits `SessionStoreResult<T>`).
#[derive(Debug, HarwError)]
pub enum SessionStoreError {
    /// Filesystem I/O failure; defers `Display`/`source()` to the inner error.
    #[from]
    Io(std::io::Error),

    /// JSON encode/decode failure; defers `Display`/`source()` to the inner error.
    #[from]
    Serde(serde_json::Error),

    /// Another writer holds the advisory lock on this session's transcript file.
    #[msg("transcript for session '{session}' is locked by another writer")]
    LockContended { session: SessionId },

    /// A JSONL line could not be decoded into a `TranscriptRecord`.
    #[msg("corrupt transcript record: {detail}")]
    CorruptRecord { detail: String },

    /// No transcript file exists for the requested session.
    #[msg("no transcript found for session '{session}'")]
    NotFound { session: SessionId },

    /// A record passed to a whole-session rewrite belongs to another session.
    #[msg("rewrite for session '{expected}' received a record for '{actual}'")]
    SessionMismatch {
        expected: SessionId,
        actual: SessionId,
    },

    #[msg("transcript session id component is unsafe for filesystem storage: '{0}'")]
    UnsafeTranscriptPath(String),

    /// A skeleton code path whose durable-I/O body is not yet wired.
    #[msg("session-store operation not yet implemented: {0}")]
    NotYetImplemented(String),

    #[msg("approval '{request}' already exists for session '{session}'")]
    ApprovalAlreadyExists { session: SessionId, request: ItemId },

    #[msg("approval '{request}' was not found for session '{session}'")]
    ApprovalNotFound { session: SessionId, request: ItemId },

    #[msg("approval '{request}' has already been resolved")]
    ApprovalAlreadyResolved { request: ItemId },

    #[msg("approval actor does not match request '{request}'")]
    ApprovalActorMismatch { request: ItemId },

    #[msg("approval id component is unsafe for filesystem storage: '{0}'")]
    UnsafeApprovalPath(String),

    /// C-APPR: the request's TTL elapsed (server clock) before it was resolved.
    #[msg("approval '{request}' issued at {issued_at} expired at {expires_at}")]
    ApprovalExpired {
        request: ItemId,
        issued_at: jiff::Timestamp,
        expires_at: jiff::Timestamp,
    },

    /// C-APPR: a durable approval file exists but cannot be trusted
    /// (undecodable, symlinked, or keyed to another session/request).
    #[msg("approval '{request}' for session '{session}' is corrupt: {detail}")]
    ApprovalCorrupt {
        session: SessionId,
        request: ItemId,
        detail: String,
    },

    #[msg("child lease for '{child}' already exists")]
    ChildLeaseAlreadyExists { child: SessionId },

    #[msg("child lease for '{child}' was not found")]
    ChildLeaseNotFound { child: SessionId },

    #[msg("child lease for '{child}' has already completed")]
    ChildLeaseAlreadyCompleted { child: SessionId },

    #[msg("child lease store lock is contended")]
    ChildLeaseLockContended,

    #[msg("child lease id component is unsafe for filesystem storage: '{0}'")]
    UnsafeChildLeasePath(String),

    /// Node AW5-05 (`FreezeStore`). Keyed by `(cgroup, finding, frozen_at)`,
    /// never `cgroup` alone — see `freeze.rs` module docs.
    #[msg("freeze for cgroup '{cgroup}' / finding '{finding}' at {frozen_at} already exists")]
    FreezeAlreadyExists {
        cgroup: CgroupId,
        finding: FindingId,
        frozen_at: jiff::Timestamp,
    },

    #[msg("freeze for cgroup '{cgroup}' / finding '{finding}' at {frozen_at} was not found")]
    FreezeNotFound {
        cgroup: CgroupId,
        finding: FindingId,
        frozen_at: jiff::Timestamp,
    },

    #[msg(
        "freeze for cgroup '{cgroup}' / finding '{finding}' at {frozen_at} has already been resolved"
    )]
    FreezeAlreadyResolved {
        cgroup: CgroupId,
        finding: FindingId,
        frozen_at: jiff::Timestamp,
    },

    #[msg("freeze store lock is contended")]
    FreezeLockContended,

    #[msg("freeze id component is unsafe for filesystem storage: '{0}'")]
    UnsafeFreezePath(String),

    #[msg("job '{work_id}' already exists")]
    JobAlreadyExists { work_id: WorkId },

    #[msg("job '{work_id}' was not found")]
    JobNotFound { work_id: WorkId },

    #[msg("job '{work_id}' is locked by another writer")]
    JobLockContended { work_id: WorkId },

    #[msg("job id component is unsafe for filesystem storage: '{0}'")]
    UnsafeJobPath(String),

    #[msg("job '{work_id}' record is corrupt: {detail}")]
    CorruptJob { work_id: WorkId, detail: String },

    #[msg("job '{work_id}' is not claimable from state {state:?}")]
    JobNotClaimable { work_id: WorkId, state: JobState },

    #[msg("job '{work_id}' cannot be cancelled from state {state:?}")]
    JobNotCancellable { work_id: WorkId, state: JobState },

    #[msg("job '{work_id}' is not eligible until {not_before}")]
    JobNotEligible {
        work_id: WorkId,
        not_before: jiff::Timestamp,
    },

    #[msg("job '{work_id}' already has a terminal state {state:?}")]
    JobAlreadyTerminal { work_id: WorkId, state: JobState },

    #[msg("lease token does not match current lease for job '{work_id}'")]
    LeaseTokenMismatch { work_id: WorkId },

    #[msg("lease for job '{work_id}' expired at {expired_at}")]
    JobLeaseExpired {
        work_id: WorkId,
        expired_at: jiff::Timestamp,
    },

    #[msg("job '{work_id}' lease epoch is exhausted")]
    JobLeaseEpochExhausted { work_id: WorkId },

    #[msg("job lease TTL must be positive")]
    InvalidJobLeaseTtl,

    #[msg("job cancellation reason must not be empty")]
    InvalidJobCancellationReason,

    /// A-STORE (G-020): `JobStore::unblock` on a job that is not `Blocked`.
    #[msg("job '{work_id}' cannot be unblocked from state {state:?}")]
    JobNotBlocked { work_id: WorkId, state: JobState },

    /// `JobStore::deny_blocked` on a job that is not `Blocked`. `/deny` falls
    /// back to `JobStore::cancel` for every other source state; this variant
    /// only fires for the dedicated Blocked-capable transition.
    #[msg("job '{work_id}' cannot be denied from state {state:?}; only a Blocked job can be denied via deny_blocked")]
    JobNotDeniable { work_id: WorkId, state: JobState },

    /// `JobStore::retry` on a job that is neither `Failed` nor `Cancelled`.
    #[msg("job '{work_id}' cannot be retried from state {state:?}; only Failed or Cancelled jobs are retryable")]
    JobNotRetryable { work_id: WorkId, state: JobState },

    /// `JobStore::retry` refused to requeue because the job's own
    /// [`harw_job_runtime::RetryPolicy`] has no attempts left. The caller must
    /// not silently requeue past this ceiling.
    #[msg("job '{work_id}' retry limit exhausted: {attempts} attempt(s) already recorded against a policy of {max_attempts}")]
    JobRetryLimitExhausted {
        work_id: WorkId,
        attempts: u32,
        max_attempts: u32,
    },

    /// A-STORE (F-156): an encoded transcript line exceeds the per-record write limit.
    #[msg("transcript record for session '{session}' has {size} bytes (limit {limit})")]
    TranscriptRecordTooLarge {
        session: SessionId,
        size: usize,
        limit: usize,
    },

    /// A-STORE (F-179): an atomic no-clobber persist found its target already present.
    #[msg("refusing to overwrite existing store file '{path}'")]
    PersistTargetExists { path: String },

    #[msg("job '{work_id}' lifecycle transition failed: {detail}")]
    JobRuntime { work_id: WorkId, detail: String },
}
