//! Filesystem layout, atomic write, and frontmatter parsing (§1.1 `store`).
//!
//! Spec: `docs/design/knowledge-surfaces.md` §1.2 (on-disk layout). This module
//! owns the three functions every surface persists through: `write_atomic`
//! (crash-safe temp-write + rename), `read_artifact` (fence split + YAML
//! parse), and path derivation for each surface. Pure logic is implemented
//! fully with `std::fs`; nothing here needs an external crate. Workspace-root
//! resolution via `etcetera` is deferred to the caller (the root is supplied).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact};
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::visibility::AgentId;

/// Markdown frontmatter fence marker.
const FENCE: &str = "---";

/// Process-local sequence used to make sibling staging paths collision-resistant.
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Number of unique staging paths reserved for one atomic write attempt.
const TEMP_FILE_ATTEMPTS: u64 = 32;

/// Root-anchored knowledge store; mirrors `harw-session-store`'s rooting (§1.2).
#[derive(Debug, Clone)]
pub struct KnowledgeStore {
    /// `<workspace_root>/knowledge` directory.
    root: PathBuf,
}

impl KnowledgeStore {
    /// Create a store rooted at `knowledge_root` (not created eagerly).
    #[must_use]
    pub fn new(knowledge_root: &Path) -> Self {
        Self {
            root: knowledge_root.to_path_buf(),
        }
    }

    /// Borrow the store's root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path to the single core-memory file (`core/MEMORY.md`).
    #[must_use]
    pub fn core_memory_path(&self) -> PathBuf {
        self.root.join("core").join("MEMORY.md")
    }

    /// Path to a topic-memory file (`topics/<slug>.md`).
    #[must_use]
    pub fn topic_path(&self, slug: &str) -> PathBuf {
        self.root.join("topics").join(format!("{slug}.md"))
    }

    /// Path to a palace node file (`palace/<node-id>.md`).
    #[must_use]
    pub fn palace_path(&self, node_id: &ArtifactId) -> PathBuf {
        self.root.join("palace").join(format!("{node_id}.md"))
    }

    /// Path to a diary day file (`diary/<agent-id>/<YYYY-MM-DD>.md`).
    #[must_use]
    pub fn diary_path(&self, agent_id: &AgentId, date: &str) -> PathBuf {
        self.root
            .join("diary")
            .join(agent_id.as_str())
            .join(format!("{date}.md"))
    }

    /// Path to a dream output file (`dreams/<YYYY-MM-DD>/<job-id>.md`).
    #[must_use]
    pub fn dream_path(&self, date: &str, job_id: &str) -> PathBuf {
        self.root
            .join("dreams")
            .join(date)
            .join(format!("{job_id}.md"))
    }

    /// Directory for a workbench scope (`workbench/<scope-id>/`).
    #[must_use]
    pub fn workbench_dir(&self, scope_id: &str) -> PathBuf {
        self.root.join("workbench").join(scope_id)
    }

    /// Directory for a kanban board (`kanban/boards/<board-id>/`).
    #[must_use]
    pub fn kanban_board_dir(&self, board_id: &str) -> PathBuf {
        self.root.join("kanban").join("boards").join(board_id)
    }

    /// Path to a kanban card file (`kanban/boards/<board-id>/cards/<card-id>.md`).
    #[must_use]
    pub fn kanban_card_path(&self, board_id: &str, card_id: &str) -> PathBuf {
        self.kanban_board_dir(board_id)
            .join("cards")
            .join(format!("{card_id}.md"))
    }

    /// Path to the rebuildable index cache (`.index/knowledge.idx`).
    #[must_use]
    pub fn index_cache_path(&self) -> PathBuf {
        self.root.join(".index").join("knowledge.idx")
    }

    /// Path to a context-proposal file (`context-proposals/<slug>.md`, AW5-09).
    ///
    /// Mirrors [`Self::palace_path`]'s convention exactly: `slug` is the
    /// **bare** local name, not the full, surface-prefixed
    /// [`crate::context_proposal::ContextProposal::id`] (which carries the
    /// `context-proposal/` prefix once indexed — see
    /// [`crate::index::KnowledgeIndex::rebuild`]). For example,
    /// `store.context_proposal_path(&ArtifactId::new("promote-history-tail"))`
    /// yields `<root>/context-proposals/promote-history-tail.md`, while the
    /// artifact stored there carries
    /// `id == ArtifactId::new("context-proposal/promote-history-tail")`.
    #[must_use]
    pub fn context_proposal_path(&self, slug: &ArtifactId) -> PathBuf {
        self.root
            .join("context-proposals")
            .join(format!("{slug}.md"))
    }

    /// Read a stored markdown artifact: split its frontmatter fence and parse.
    ///
    /// # Errors
    /// [`KnowledgeError::Io`] on read failure, [`KnowledgeError::MalformedFrontmatter`]
    /// on a missing/broken fence, [`KnowledgeError::Frontmatter`] on YAML errors.
    pub fn read_artifact(
        &self,
        path: &Path,
        id: ArtifactId,
        kind: ArtifactKind,
    ) -> KnowledgeResult<KnowledgeArtifact> {
        let content = std::fs::read_to_string(path)?;
        let (frontmatter, body) = parse_frontmatter(&content)?;
        Ok(KnowledgeArtifact::new(id, kind, frontmatter, body))
    }

    /// Render and atomically write an artifact to `path`.
    ///
    /// # Errors
    /// [`KnowledgeError::Frontmatter`] on YAML encode failure, [`KnowledgeError::Io`]
    /// on write/rename failure.
    pub fn write_artifact(&self, path: &Path, artifact: &KnowledgeArtifact) -> KnowledgeResult<()> {
        let rendered = render_frontmatter(&artifact.frontmatter, &artifact.body)?;
        write_atomic(path, &rendered)
    }
}

/// Atomically write `contents` to `path` via a sibling temp file + rename.
///
/// The rename is atomic on the same filesystem; the temp file is synced before
/// rename so a crash never leaves a partially-written live file.
///
/// # Errors
/// [`KnowledgeError::Io`] on any create/write/sync/rename failure.
pub fn write_atomic(path: &Path, contents: &str) -> KnowledgeResult<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Reserve a block so concurrent writers never select the same initial
    // staging path. `create_new` below remains the authority: it refuses a
    // pre-existing regular file, hard link, or symlink rather than following it.
    let first_sequence = TEMP_FILE_SEQUENCE.fetch_add(TEMP_FILE_ATTEMPTS, Ordering::Relaxed);
    let (tmp, file) = create_temp_sibling(path, first_sequence)?;
    let write_result = {
        let mut file = file;
        (|| -> std::io::Result<()> {
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&tmp, path)?;
            Ok(())
        })()
    };

    if let Err(error) = write_result {
        // This path was created exclusively by us. Do not remove a path when
        // creation itself failed, since that could delete a caller's artifact.
        let _ = std::fs::remove_file(&tmp);
        return Err(KnowledgeError::Io(error));
    }

    Ok(())
}

/// Create a new hidden sibling staging file without following existing entries.
fn create_temp_sibling(
    path: &Path,
    first_sequence: u64,
) -> std::io::Result<(PathBuf, std::fs::File)> {
    use std::fs::OpenOptions;

    for offset in 0..TEMP_FILE_ATTEMPTS {
        let tmp = tmp_sibling(path, first_sequence.wrapping_add(offset));
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique knowledge staging file",
    ))
}

/// Derive a hidden sibling temp path used for an atomic-write staging attempt.
fn tmp_sibling(path: &Path, sequence: u64) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "artifact".to_owned());
    let tmp_name = format!(".{name}.knowledge-tmp-{}-{sequence}", std::process::id());
    match path.parent() {
        Some(parent) => parent.join(tmp_name),
        None => PathBuf::from(tmp_name),
    }
}

/// Split a `---`-fenced markdown document into `(frontmatter_yaml, body)`.
///
/// # Errors
/// [`KnowledgeError::MalformedFrontmatter`] when the leading fence or the
/// closing fence is absent.
pub fn split_frontmatter(content: &str) -> KnowledgeResult<(&str, &str)> {
    let stripped = content.strip_prefix('\u{feff}').unwrap_or(content);
    let after_open = stripped
        .strip_prefix("---\n")
        .or_else(|| stripped.strip_prefix("---\r\n"))
        .ok_or_else(|| KnowledgeError::MalformedFrontmatter {
            detail: "missing opening '---' frontmatter fence".to_owned(),
        })?;

    // Find a line that is exactly the fence, terminating the frontmatter block.
    let mut offset = 0usize;
    for line in after_open.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == FENCE {
            let yaml = &after_open[..offset];
            let body = &after_open[offset + line.len()..];
            return Ok((yaml, body));
        }
        offset += line.len();
    }
    Err(KnowledgeError::MalformedFrontmatter {
        detail: "missing closing '---' frontmatter fence".to_owned(),
    })
}

/// Parse a fenced markdown document into typed [`Frontmatter`] plus its body.
///
/// # Errors
/// [`KnowledgeError::MalformedFrontmatter`] on a bad fence,
/// [`KnowledgeError::Frontmatter`] on a YAML decode error.
pub fn parse_frontmatter(content: &str) -> KnowledgeResult<(Frontmatter, String)> {
    let (yaml, body) = split_frontmatter(content)?;
    let frontmatter: Frontmatter = serde_norway::from_str(yaml)?;
    Ok((frontmatter, body.to_owned()))
}

/// Render typed [`Frontmatter`] and a body back into a `---`-fenced document.
///
/// # Errors
/// [`KnowledgeError::Frontmatter`] on a YAML encode error.
pub fn render_frontmatter(frontmatter: &Frontmatter, body: &str) -> KnowledgeResult<String> {
    let yaml = serde_norway::to_string(frontmatter)?;
    let mut out = String::with_capacity(yaml.len() + body.len() + 8);
    out.push_str(FENCE);
    out.push('\n');
    out.push_str(&yaml);
    if !yaml.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(FENCE);
    out.push('\n');
    out.push_str(body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_root(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create temporary root");
        root
    }

    #[test]
    fn write_atomic_replaces_an_existing_regular_file() {
        let root = temporary_root("harw-knowledge-atomic-replace");
        let path = root.join("artifact.md");
        std::fs::write(&path, "old contents").expect("write original artifact");

        write_atomic(&path, "new contents").expect("replace regular artifact");

        assert_eq!(
            std::fs::read_to_string(&path).expect("read replaced artifact"),
            "new contents"
        );
        std::fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[cfg(unix)]
    #[test]
    fn temp_creation_skips_preexisting_files_and_symlinks_without_writing_through_them() {
        let root = temporary_root("harw-knowledge-atomic-symlink");
        let path = root.join("artifact.md");
        let redirected = root.join("redirected.md");
        let first_sequence = 41;
        let existing_temp = tmp_sibling(&path, first_sequence);
        let linked_temp = tmp_sibling(&path, first_sequence + 1);
        std::fs::write(&existing_temp, "must remain unchanged")
            .expect("write pre-existing staging file");
        std::fs::write(&redirected, "must remain unchanged").expect("write redirected artifact");
        std::os::unix::fs::symlink(&redirected, &linked_temp).expect("create staging symlink");

        let (created_temp, file) = create_temp_sibling(&path, first_sequence)
            .expect("allocate staging file after symlink collision");
        drop(file);

        assert_ne!(created_temp, existing_temp);
        assert_ne!(created_temp, linked_temp);
        assert_eq!(
            std::fs::read_to_string(&existing_temp).expect("read pre-existing staging file"),
            "must remain unchanged"
        );
        assert_eq!(
            std::fs::read_to_string(&redirected).expect("read redirected artifact"),
            "must remain unchanged"
        );
        assert!(
            std::fs::symlink_metadata(&linked_temp)
                .expect("inspect staging symlink")
                .file_type()
                .is_symlink()
        );

        std::fs::remove_file(created_temp).expect("remove owned staging file");
        std::fs::remove_file(existing_temp).expect("remove pre-existing staging file");
        std::fs::remove_file(linked_temp).expect("remove staging symlink");
        std::fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_removes_its_staging_file_when_rename_fails() {
        let root = temporary_root("harw-knowledge-atomic-cleanup");
        let destination_directory = root.join("artifact.md");
        std::fs::create_dir(&destination_directory).expect("create destination directory");

        assert!(write_atomic(&destination_directory, "new contents").is_err());

        let temporary_entries = std::fs::read_dir(&root)
            .expect("read temporary root")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".knowledge-tmp-")
            })
            .count();
        assert_eq!(temporary_entries, 0);

        std::fs::remove_dir_all(root).expect("remove temporary root");
    }
}
