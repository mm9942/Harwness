//! `harw-session-store` — append-only session transcript store.
//!
//! Spec: `docs/design/knowledge-surfaces.md` §1.4 and the
//! `harw-session-store` row of `docs/design/crates-inventory.md`.
//!
//! This crate owns **transcripts**: the sequential, replayable record of turns,
//! tool calls and model outputs for a `SessionId`/`ThreadId`. Transcripts are
//! append-only and high-volume; the sanctioned bridge to `harw-knowledge` is
//! *promotion* (a transcript segment distilled into an artifact, with the
//! artifact carrying a `source_session_id` back-reference — never a wholesale
//! copy). This crate does not depend on `harw-knowledge` or `harw-core`.
//!
//! Storage is hand-rolled JSONL: one `<session_id>.jsonl` file per session,
//! appended under an `fs4` advisory lock, with `tempfile` `persist()` atomic
//! rename for full-file rewrites. Time is `jiff::Timestamp`. IDs are reused
//! from `harw-types` (`SessionId`, `ThreadRef`) — never redefined here.
//!
//! Skeleton status: record codec, path derivation and the replay reader are
//! implemented; the locked-append and atomic-rewrite bodies return
//! [`error::SessionStoreError::NotYetImplemented`] pending `fs4`/`tempfile`
//! wiring by the parent build.

#![forbid(unsafe_code)]

// Intern: die eine Fassung des Eltern-fsync, von allen vier Stores benutzt.
mod durability;

pub mod approval;
pub mod child_lease;
pub mod error;
pub mod freeze;
pub mod job_store;
pub mod meta;
pub mod reader;
pub mod record;
pub mod store;

pub use approval::{
    ApprovalRecord, ApprovalResolutionRecord, ApprovalStore, DEFAULT_APPROVAL_TTL,
};
pub use child_lease::{ChildLeaseCompletionRecord, ChildLeaseRecord, ChildLeaseStore};
pub use error::{SessionStoreError, SessionStoreResult};
pub use freeze::{Freeze, FreezeResolution, FreezeResolutionOutcome, FreezeStore};
pub use job_store::{
    CancelRequest, CancellationTransition, ClaimRequest, CompleteRequest, ExpiredJob, JobEventSink,
    JobLifecycleEvent, JobListQuery, JobPage, JobStore, NoopJobEventSink, RenewalRequest,
};
pub use meta::{SessionMeta, TitleSource, SESSION_META_VERSION};
pub use reader::TranscriptReader;
pub use record::{RecordKind, TranscriptRecord};
pub use store::TranscriptStore;
