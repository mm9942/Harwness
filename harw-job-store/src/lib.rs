//! `harw-job-store` — durable, fenced job state (Job-Runtime-Doc §7,
//! Eco-Doc §33).
//!
//! Generic store mechanics extracted from `harw-session-store`'s job store,
//! free of Harwness session semantics (approval policy, event publication,
//! tenants/workspaces as authority): `harw-session-store::JobStore` is now a
//! thin adapter over this crate with an unchanged API and on-disk format.
//!
//! # Contents
//! - [`RecordStore`]: a generic typed record store over a cap-std root
//!   ([`cap_std::fs::Dir`]): `records/<id>.json`, `locks/<id>.lock`,
//!   `<kind>/<id>.json` sidecars (e.g. `approvals/`). Per-record fs4
//!   exclusive try-locks; atomic temp + rename + directory `fsync`;
//!   corrupt records are quarantined, never silently lost.
//! - [`mechanics`]: pure lease/lifecycle mutations of one
//!   [`harw_job_core::StoredJob`] (claim, renew, complete, cancel, reclaim,
//!   expire) with fencing checks.
//! - [`JobRecordStore`] / [`FsJobRecordStore`]: the backend-independent
//!   store contract (`create_job`, `claim_job`, `transition`,
//!   `load_recovery_set`).
//! - [`StoreError`]: typed failures — conflict, stale/lost lease, not found,
//!   corrupt, I/O, unsupported (Job-Runtime-Doc §22).
//!
//! # Ambient authority
//! Only [`RecordStore::create_ambient`] / [`RecordStore::open_existing_ambient`]
//! (and the `FsJobRecordStore` equivalent) touch an ambient path, once, to
//! open the root. Every later file operation is relative to that `Dir`.
//! See `fsops` for the one documented check-then-open limitation.
//!
//! # Concurrency
//! Lock granularity is one record; locks never block (contention is
//! [`StoreError::Contended`]). Readers are lock-free and see complete
//! records only.

#![forbid(unsafe_code)]

pub mod error;
mod fsops;
pub mod job_record_store;
pub mod mechanics;
pub mod record_store;

pub use error::{Conflict, StaleLease, StoreError, StoreResult};
pub use fsops::validate_id;
pub use job_record_store::{
    ClaimTerms, ExpiryPage, FsJobRecordStore, JobRecordStore, JobTransition,
    LEASE_EXPIRED_EXHAUSTED_REASON,
};
pub use mechanics::{Cancelled, Reclaimed};
pub use record_store::{
    LOCKS_DIR, MAX_PAGE, Page, RECORDS_DIR, RecordLock, RecordStore, StoreRecord,
};

// Test error type (Bible R087/R165/R182), tests only.
#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests;
