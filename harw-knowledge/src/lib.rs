//! `harw-knowledge` — unified markdown+frontmatter knowledge & work store.
//!
//! Spec: `docs/design/knowledge-surfaces.md`. This crate owns **artifacts**
//! (curated, promotion-gated, durable-by-design material) as opposed to the
//! append-only **transcripts** owned by `harw-session-store` (§1.4). Every
//! surface — memory, palace, diary, dream, workbench, kanban — is one
//! [`artifact::ArtifactKind`] persisted through the same store, indexed by the
//! same [`index::KnowledgeIndex`], and gated by the same
//! [`visibility::VisibilityScope`] (§1.1).
//!
//! # Module layout (mirrors §1.1)
//! - [`error`]   — `KnowledgeError` via `harw_macros::HarwError`
//! - [`store`]   — filesystem layout, atomic write, frontmatter parse
//! - [`index`]   — typed, in-memory, rebuildable `KnowledgeIndex`
//! - [`artifact`]— `KnowledgeArtifact`, `ArtifactKind`, `Frontmatter`, `RecallQuery`
//! - [`memory`]  — core / topic / palace / recall
//! - [`diary`]   — append-only daily journal
//! - [`dream`]   — dream job payload/output types (the `Job` lives in `harw-job-runtime`)
//! - [`workbench`]— per-session/per-project scratch surface
//! - [`lock`]    — prozessübergreifende Dateisperre ([`lock::KnowledgeLock`])
//!   für Read-Modify-Write-Zyklen (Diary, Kanban-Karten, Workbench)
//! - [`kanban`]  — typed board / lane / card model + lifecycle
//! - [`security`]— `SecurityFinding` / `Baseline` durable security artifacts (AW4-05)
//! - [`visibility`]— `VisibilityScope`, applied uniformly
//! - [`context_provider`]— `KnowledgeContextProvider` (AW3-03): translates
//!   visibility-filtered recall hits into `harw_context::Fragment`s. Enforces
//!   `VisibilityScope` by construction — it reads exclusively through
//!   `memory::recall::search`, never through `KnowledgeIndex::iter`/`backlinks`
//!   directly. See that module's doc for exactly where and why.
//! - [`context_proposal`]— `ContextProposal` (AW5-09): a Layer-4 proposal to
//!   change a context program. Schlägt vor, committet nie — trägt keine
//!   `apply`-Methode, and nothing in this crate writes a context-program
//!   definition. See that module's doc for the heuristic, the chosen
//!   `VisibilityScope`, and where its observed-data input does (and does
//!   not) reach this crate.
//! - [`model_behavior_proposal`]— `ModelBehaviorProposal` (AW6-06): the
//!   Layer-4 back-channel for `harw-model-catalog`. Same "proposes, never
//!   commits" rule and lifecycle mechanics as `context_proposal`, applied to
//!   catalog claims about a model instead of context-program sections. See
//!   that module's doc for the heuristic, why it is a separate `ArtifactKind`
//!   rather than a `ContextProposal` variant, and where its observed-data
//!   input does (and does not) reach this crate.
//! - [`context_steward`]— the Context Steward (AW6-08): curates, orders and
//!   summarizes already-fetched [`context_proposal::ContextProposal`] and
//!   [`model_behavior_proposal::ModelBehaviorProposal`] batches into a
//!   [`context_steward::StewardDigest`] — never applies either. Also carries
//!   this crate's three `steward_*` null counters (`harw-observe`, AW1-07's
//!   deferred mechanic), each documented with the invariant it watches and
//!   the production path it hangs on.
//!
//! # Errors
//! Every fallible path returns [`error::KnowledgeError`] / [`error::KnowledgeResult`].
//!
//! # Concurrency
//! The types are plain `Send + Sync` serde data. This crate spawns no threads;
//! file-watching (workbench) is driven by the parent build; the dream job's
//! cron schedule lives in [`context_steward`].

#![forbid(unsafe_code)]

/// Generates a `#[serde(transparent)]` `String` newtype id with the crate's
/// standard surface (`new`, `as_str`, `Display`, `From<String>`/`From<&str>`).
macro_rules! id_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord,
            ::serde::Serialize, ::serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Construct an id from an existing string value.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Borrow the inner string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl ::std::convert::From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl ::std::convert::From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}
pub mod artifact;
pub mod context_proposal;
pub mod context_provider;
pub mod context_steward;
pub mod diary;
pub mod dream;
pub mod error;
pub mod index;
pub mod kanban;
pub mod lock;
pub mod memory;
pub mod model_behavior_proposal;
pub mod security;
pub mod store;
#[cfg(test)]
mod test_support;
pub mod visibility;
pub mod workbench;

pub use artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact, RecallQuery};
pub use context_provider::{
    KNOWLEDGE_CONTEXT_MAX_TRUST, KNOWLEDGE_CONTEXT_MAY_CARRY_USER_CONTENT,
    KNOWLEDGE_CONTEXT_NAMESPACE, KnowledgeContextProvider,
};
pub use dream::{
    DreamReport, DreamReportData, DreamRunStatus, DreamSchedulerState, DreamSuggestion,
    DreamSuggestionKind, DreamSuggestionStatus,
};
pub use error::{KnowledgeError, KnowledgeResult};
pub use index::{ArtifactRef, KnowledgeIndex};
pub use kanban::board::{
    BlockKind, Board, BoardId, Card, CardId, CardState, Lane, LaneId, LaneKind,
};
pub use lock::KnowledgeLock;
pub use store::KnowledgeStore;
pub use visibility::{AgentId, AgentRoleRef, VisibilityScope};
