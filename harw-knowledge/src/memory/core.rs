//! Core memory: the single durable `knowledge/core/MEMORY.md` file (§2.1).
//!
//! Spec: `docs/design/knowledge-surfaces.md` §2.1, §2.4. Small by design and
//! bootstrap-loaded in full for the owning agent. Reads are direct; writes are
//! **promotion-gated** — a plain session agent proposes, but only the
//! compaction/dream promotion path (with a recorded review) commits (§2.4,
//! §2.5). The direct write here therefore requires a review token.

use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact};
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::store::KnowledgeStore;

/// Einzige kanonische Id des Core-Memory-Artefakts.
///
/// Sowohl [`read`]/[`commit_promotion`] als auch
/// [`crate::index::KnowledgeIndex::rebuild`] verwenden genau diese Konstante,
/// damit ein über den Index gefundenes Core-Memory dieselbe Id trägt wie ein
/// direkt gelesenes (vorher: `core/memory` im Index vs. `core/MEMORY` hier).
pub const CORE_MEMORY_ID: &str = "core/MEMORY";

/// Stable id of the core-memory artifact ([`CORE_MEMORY_ID`]).
#[must_use]
pub fn core_memory_id() -> ArtifactId {
    ArtifactId::new(CORE_MEMORY_ID)
}

/// Read the core-memory artifact from disk (bootstrap load, §2.1).
///
/// # Errors
/// [`KnowledgeError::Io`] if the file is missing/unreadable,
/// [`KnowledgeError::Frontmatter`]/[`KnowledgeError::MalformedFrontmatter`] on parse.
pub fn read(store: &KnowledgeStore) -> KnowledgeResult<KnowledgeArtifact> {
    store.read_artifact(
        &store.core_memory_path(),
        core_memory_id(),
        ArtifactKind::CoreMemory,
    )
}

/// Commit a core-memory change through the promotion gate (§2.4, §2.5).
///
/// Core memory only changes via the promotion commit step, so this refuses to
/// write unless `reviewed` is `true` (the review having been recorded by the
/// `/dream review` or compaction-commit path).
///
/// # Errors
/// [`KnowledgeError::PromotionNotReviewed`] when `reviewed` is `false`;
/// otherwise [`KnowledgeError::Frontmatter`]/[`KnowledgeError::Io`] on write.
pub fn commit_promotion(
    store: &KnowledgeStore,
    frontmatter: Frontmatter,
    body: impl Into<String>,
    reviewed: bool,
) -> KnowledgeResult<()> {
    if !reviewed {
        return Err(KnowledgeError::PromotionNotReviewed {
            from: "topic".to_owned(),
            to: "core".to_owned(),
        });
    }
    let artifact = KnowledgeArtifact::new(
        core_memory_id(),
        ArtifactKind::CoreMemory,
        frontmatter,
        body,
    );
    store.write_artifact(&store.core_memory_path(), &artifact)
}
