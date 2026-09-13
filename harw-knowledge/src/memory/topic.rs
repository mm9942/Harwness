//! Topic memory: one file per coherent subject, `topics/<slug>.md` (§2.1).
//!
//! Spec: `docs/design/knowledge-surfaces.md` §2.1, §2.4. Detailed, not
//! bootstrap-loaded, retrieved on demand via recall. Freely agent-writable
//! within the writer's own scope (no review gate to *write*; only to *promote
//! out of* topic memory into the palace, §2.4). Slug derivation and read/write
//! are implemented fully.

use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact};
use crate::error::KnowledgeResult;
use crate::store::KnowledgeStore;

/// Derive a filesystem-safe slug from a topic title (lowercase, dash-joined).
#[must_use]
pub fn slugify(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    let mut prev_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

/// Stable id for a topic file (`topic/<slug>`).
#[must_use]
pub fn topic_id(slug: &str) -> ArtifactId {
    ArtifactId::new(format!("topic/{slug}"))
}

/// Read a topic-memory artifact by slug.
///
/// # Errors
/// [`crate::error::KnowledgeError::Io`] if missing/unreadable, parse errors on
/// malformed frontmatter.
pub fn read(store: &KnowledgeStore, slug: &str) -> KnowledgeResult<KnowledgeArtifact> {
    store.read_artifact(
        &store.topic_path(slug),
        topic_id(slug),
        ArtifactKind::TopicMemory,
    )
}

/// Write (create or replace) a topic-memory artifact under `slug`.
///
/// # Errors
/// [`crate::error::KnowledgeError::Frontmatter`]/[`crate::error::KnowledgeError::Io`] on write.
pub fn write(
    store: &KnowledgeStore,
    slug: &str,
    frontmatter: Frontmatter,
    body: impl Into<String>,
) -> KnowledgeResult<()> {
    let artifact =
        KnowledgeArtifact::new(topic_id(slug), ArtifactKind::TopicMemory, frontmatter, body);
    store.write_artifact(&store.topic_path(slug), &artifact)
}
