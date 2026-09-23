//! [`KnowledgeIndex`] — the typed, in-memory, rebuildable index (§1.3).
//!
//! Spec: `docs/design/knowledge-surfaces.md` §1.3. Answers four query shapes
//! without a directory walk: by id, by tag/kind, by link (backlinks, powering
//! the palace graph), and by visibility-filtered scope. It is a cache, not a
//! source of truth — the markdown files remain durable, so a corrupt cache is
//! always regenerable. In-memory operations, the JSON cache, and full-disk
//! rebuilds over the bounded surface layout are implemented here.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::artifact::{ArtifactId, ArtifactKind, KnowledgeArtifact};
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::memory::core::core_memory_id;
use crate::store::KnowledgeStore;
use crate::visibility::VisibilityScope;

/// A lightweight projection of an artifact returned by index queries (§1.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// Artifact id.
    pub id: ArtifactId,
    /// Artifact kind.
    pub kind: ArtifactKind,
    /// Topical tags.
    pub tags: Vec<String>,
    /// Visibility scope.
    pub visibility: VisibilityScope,
}

impl ArtifactRef {
    /// Project a full artifact into a lightweight reference.
    #[must_use]
    pub fn from_artifact(artifact: &KnowledgeArtifact) -> Self {
        Self {
            id: artifact.id.clone(),
            kind: artifact.kind,
            tags: artifact.frontmatter.tags.clone(),
            visibility: artifact.frontmatter.visibility.clone(),
        }
    }
}

/// Ergebnisbericht eines [`KnowledgeIndex::rebuild_with_report`]-Laufs.
///
/// Ein einzelnes defektes Artefakt (unlesbar, fehlender Fence, ungültiges
/// YAML) bricht den Rebuild nicht ab: es wird übersprungen und hier mit Pfad
/// und Fehlertext festgehalten, damit Doctor/CLI es sichtbar machen können.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexReport {
    /// Anzahl erfolgreich eingelesener Artefakte.
    pub indexed: usize,
    /// Übersprungene Dateien mit dem Grund (gerenderter [`KnowledgeError`]).
    pub skipped: Vec<(PathBuf, String)>,
}

impl IndexReport {
    /// `true`, wenn kein Artefakt übersprungen wurde.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.skipped.is_empty()
    }
}

/// In-memory index over the knowledge store; rebuildable from disk (§1.3).
#[derive(Debug, Clone, Default)]
pub struct KnowledgeIndex {
    /// id -> full artifact.
    artifacts: HashMap<ArtifactId, KnowledgeArtifact>,
    /// target id -> ids of artifacts linking to it (backlink adjacency).
    backlinks: HashMap<ArtifactId, Vec<ArtifactId>>,
}

impl KnowledgeIndex {
    /// Create an empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) an artifact, updating the backlink adjacency.
    pub fn insert(&mut self, artifact: KnowledgeArtifact) {
        let id = artifact.id.clone();
        // Drop stale backlink edges if this id was previously indexed.
        if let Some(prev) = self.artifacts.get(&id) {
            for target in &prev.frontmatter.links {
                if let Some(sources) = self.backlinks.get_mut(target) {
                    sources.retain(|source| source != &id);
                }
            }
        }
        for target in &artifact.frontmatter.links {
            self.backlinks
                .entry(target.clone())
                .or_default()
                .push(id.clone());
        }
        self.artifacts.insert(id, artifact);
    }

    /// Look up an artifact by id.
    #[must_use]
    pub fn get(&self, id: &ArtifactId) -> Option<&KnowledgeArtifact> {
        self.artifacts.get(id)
    }

    /// Number of indexed artifacts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.artifacts.len()
    }

    /// Whether the index holds no artifacts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.artifacts.is_empty()
    }

    /// Iterate over every indexed artifact.
    pub fn iter(&self) -> impl Iterator<Item = &KnowledgeArtifact> {
        self.artifacts.values()
    }

    /// Find artifacts by optional kind and required tags (AND semantics).
    #[must_use]
    pub fn find(&self, kind: Option<ArtifactKind>, tags: &[String]) -> Vec<ArtifactRef> {
        self.artifacts
            .values()
            .filter(|artifact| kind.is_none_or(|k| k == artifact.kind))
            .filter(|artifact| {
                tags.iter()
                    .all(|tag| artifact.frontmatter.tags.contains(tag))
            })
            .map(ArtifactRef::from_artifact)
            .collect()
    }

    /// Return the artifacts that link *to* `node_id` (§1.3 palace backlinks).
    #[must_use]
    pub fn backlinks(&self, node_id: &ArtifactId) -> Vec<ArtifactRef> {
        self.backlinks
            .get(node_id)
            .into_iter()
            .flatten()
            .filter_map(|source| self.artifacts.get(source))
            .map(ArtifactRef::from_artifact)
            .collect()
    }

    /// Iterate over artifacts visible to a caller with the given scope (§2.3).
    pub fn visible_to<'a>(
        &'a self,
        caller: &'a VisibilityScope,
    ) -> impl Iterator<Item = &'a KnowledgeArtifact> {
        self.artifacts
            .values()
            .filter(move |artifact| artifact.frontmatter.visibility.visible_to_caller(caller))
    }

    /// Serialize the index contents to a JSON cache blob.
    ///
    /// # Errors
    /// [`KnowledgeError::Json`] on serialization failure.
    pub fn to_cache_json(&self) -> KnowledgeResult<String> {
        let snapshot: Vec<&KnowledgeArtifact> = self.artifacts.values().collect();
        Ok(serde_json::to_string(&snapshot)?)
    }

    /// Rebuild an index from a JSON cache blob (inverse of [`Self::to_cache_json`]).
    ///
    /// # Errors
    /// [`KnowledgeError::Json`] on deserialization failure.
    pub fn from_cache_json(blob: &str) -> KnowledgeResult<Self> {
        let artifacts: Vec<KnowledgeArtifact> = serde_json::from_str(blob)?;
        let mut index = Self::new();
        for artifact in artifacts {
            index.insert(artifact);
        }
        Ok(index)
    }

    /// Write the index cache to `<root>/.index/knowledge.idx` atomically.
    ///
    /// # Errors
    /// [`KnowledgeError::Json`] on encode failure, [`KnowledgeError::Io`] on write.
    pub fn write_cache(&self, store: &KnowledgeStore) -> KnowledgeResult<()> {
        let blob = self.to_cache_json()?;
        crate::store::write_atomic(&store.index_cache_path(), &blob)
    }

    /// Load the index cache from disk if present.
    ///
    /// # Errors
    /// [`KnowledgeError::Io`] on read failure, [`KnowledgeError::Json`] on decode.
    pub fn load_cache(store: &KnowledgeStore) -> KnowledgeResult<Self> {
        let blob = std::fs::read_to_string(store.index_cache_path())?;
        Self::from_cache_json(&blob)
    }

    /// Rebuild the index from the durable markdown tree (§1.3 doctor path).
    ///
    /// Wie [`Self::rebuild_with_report`], verwirft aber den [`IndexReport`]:
    /// defekte Einzelartefakte werden übersprungen, nicht als Fehler gemeldet.
    ///
    /// # Errors
    /// Nur bei Fehlern des Verzeichnisdurchlaufs selbst (Root/Surface nicht
    /// lesbar), nie wegen eines einzelnen defekten Artefakts.
    pub fn rebuild(store: &KnowledgeStore) -> KnowledgeResult<Self> {
        Self::rebuild_with_report(store).map(|(index, _report)| index)
    }

    /// Rebuild the index from the durable markdown tree and report skipped files.
    ///
    /// The directory layout is the trusted type discriminator. Unknown and
    /// non-Markdown files are ignored so editor state and index cache files
    /// never become searchable artifacts. A missing root is a valid empty
    /// knowledge store. Ein Artefakt, das sich nicht lesen oder parsen lässt,
    /// landet in [`IndexReport::skipped`]; der Rebuild läuft weiter.
    ///
    /// # Errors
    /// [`KnowledgeError::Io`], wenn Root oder ein Surface-Verzeichnis nicht
    /// kanonisiert/gelesen werden kann.
    pub fn rebuild_with_report(store: &KnowledgeStore) -> KnowledgeResult<(Self, IndexReport)> {
        let mut index = Self::new();
        let mut report = IndexReport::default();
        if !store.root().exists() {
            return Ok((index, report));
        }

        let root = store.root();
        let canonical_root = std::fs::canonicalize(root)?;

        let core = store.core_memory_path();
        if core.is_file() {
            index_file(
                &mut index,
                &mut report,
                store,
                &core,
                core_memory_id(),
                ArtifactKind::CoreMemory,
            );
        }

        index_surface(
            &mut index,
            &mut report,
            store,
            &root.join("topics"),
            &canonical_root,
            ArtifactKind::TopicMemory,
            "topic",
        )?;
        index_surface(
            &mut index,
            &mut report,
            store,
            &root.join("palace"),
            &canonical_root,
            ArtifactKind::PalaceNode,
            "palace",
        )?;
        index_surface(
            &mut index,
            &mut report,
            store,
            &root.join("diary"),
            &canonical_root,
            ArtifactKind::DiaryEntry,
            "diary",
        )?;
        index_surface(
            &mut index,
            &mut report,
            store,
            &root.join("dreams"),
            &canonical_root,
            ArtifactKind::DreamReport,
            "dream",
        )?;
        index_surface(
            &mut index,
            &mut report,
            store,
            &root.join("workbench"),
            &canonical_root,
            ArtifactKind::WorkbenchNote,
            "workbench",
        )?;
        index_surface(
            &mut index,
            &mut report,
            store,
            &root.join("kanban").join("boards"),
            &canonical_root,
            ArtifactKind::KanbanCard,
            "kanban",
        )?;
        // AW5-09: `ContextProposal` artifacts (see `crate::context_proposal`)
        // live under `context-proposals/`, mirroring the six surfaces above.
        // Purely additive: a store without this directory is unaffected
        // (`index_surface` returns `Ok(())` immediately for a missing
        // directory, exactly as it already does for every other surface).
        index_surface(
            &mut index,
            &mut report,
            store,
            &root.join("context-proposals"),
            &canonical_root,
            ArtifactKind::ContextProposal,
            "context-proposal",
        )?;

        Ok((index, report))
    }
}

/// Index every Markdown file below one known knowledge surface.
fn index_surface(
    index: &mut KnowledgeIndex,
    report: &mut IndexReport,
    store: &KnowledgeStore,
    surface_root: &Path,
    configured_root: &Path,
    kind: ArtifactKind,
    id_prefix: &str,
) -> KnowledgeResult<()> {
    if !surface_root.is_dir() {
        return Ok(());
    }

    for file in markdown_files(surface_root, configured_root)? {
        let Ok(relative) = file.strip_prefix(surface_root) else {
            continue;
        };
        let Some(id) = artifact_id_from_relative(id_prefix, relative) else {
            continue;
        };
        index_file(index, report, store, &file, id, kind);
    }
    Ok(())
}

/// Liest ein einzelnes Artefakt ein; ein Lese-/Parse-Fehler wird im Report
/// vermerkt statt den Rebuild abzubrechen.
fn index_file(
    index: &mut KnowledgeIndex,
    report: &mut IndexReport,
    store: &KnowledgeStore,
    path: &Path,
    id: ArtifactId,
    kind: ArtifactKind,
) {
    match store.read_artifact(path, id, kind) {
        Ok(artifact) => {
            index.insert(artifact);
            report.indexed += 1;
        }
        Err(error) => report.skipped.push((path.to_path_buf(), error.to_string())),
    }
}

/// Recursively collect regular Markdown files below one surface without leaving the configured root.
///
/// Symlinks are deliberately never followed: a directory symlink can point
/// outside the knowledge root or introduce a traversal cycle, while a file
/// symlink can make an external artifact appear to belong to the surface.
fn markdown_files(root: &Path, configured_root: &Path) -> KnowledgeResult<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut directories = vec![root.to_path_buf()];
    let mut visited_directories = HashSet::new();
    while let Some(directory) = directories.pop() {
        let canonical_directory = std::fs::canonicalize(&directory).map_err(KnowledgeError::Io)?;
        if !canonical_directory.starts_with(configured_root)
            || !visited_directories.insert(canonical_directory)
        {
            continue;
        }

        for entry in std::fs::read_dir(directory).map_err(KnowledgeError::Io)? {
            let entry = entry.map_err(KnowledgeError::Io)?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(KnowledgeError::Io)?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                directories.push(path);
            } else if file_type.is_file()
                && path.extension().is_some_and(|extension| extension == "md")
            {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Convert a surface-relative Markdown path into a stable artifact id.
fn artifact_id_from_relative(prefix: &str, relative: &Path) -> Option<ArtifactId> {
    let stem = relative.with_extension("");
    let suffix = stem.to_str()?.replace(std::path::MAIN_SEPARATOR, "/");
    (!suffix.is_empty()).then(|| ArtifactId::new(format!("{prefix}/{suffix}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::Frontmatter;
    use crate::test_support::{TestError, TestResult};
    use crate::visibility::{AgentId, VisibilityScope};

    fn temporary_root(label: &str) -> TestResult<PathBuf> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(crate::test_support::ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root)
            .map_err(crate::test_support::ctx("create temporary knowledge root"))?;
        Ok(root)
    }

    fn frontmatter() -> Frontmatter {
        Frontmatter::new(
            AgentId::new("agent"),
            VisibilityScope::SelfOnly,
            jiff::Timestamp::now(),
        )
    }

    #[test]
    fn rebuild_reads_the_durable_layout_and_restores_backlinks() -> TestResult {
        let root = temporary_root("harw-knowledge-rebuild")?;
        let store = KnowledgeStore::new(&root);

        let core = KnowledgeArtifact::new(
            core_memory_id(),
            ArtifactKind::CoreMemory,
            frontmatter(),
            "durable core fact",
        );
        store
            .write_artifact(&store.core_memory_path(), &core)
            .map_err(crate::test_support::ctx("write core artifact"))?;

        let mut topic_frontmatter = frontmatter();
        topic_frontmatter
            .links
            .push(ArtifactId::new("palace/deploy"));
        let topic = KnowledgeArtifact::new(
            ArtifactId::new("topic/nested/runtime"),
            ArtifactKind::TopicMemory,
            topic_frontmatter,
            "runtime detail",
        );
        store
            .write_artifact(&root.join("topics/nested/runtime.md"), &topic)
            .map_err(crate::test_support::ctx("write topic artifact"))?;

        let palace = KnowledgeArtifact::new(
            ArtifactId::new("palace/deploy"),
            ArtifactKind::PalaceNode,
            frontmatter(),
            "deployment detail",
        );
        store
            .write_artifact(&store.palace_path(&ArtifactId::new("deploy")), &palace)
            .map_err(crate::test_support::ctx("write palace artifact"))?;

        let rebuilt =
            KnowledgeIndex::rebuild(&store).map_err(crate::test_support::ctx("rebuild index"))?;

        assert_eq!(rebuilt.len(), 3);
        assert!(rebuilt.get(&core_memory_id()).is_some());
        assert_eq!(
            rebuilt.backlinks(&ArtifactId::new("palace/deploy"))[0].id,
            ArtifactId::new("topic/nested/runtime")
        );

        std::fs::remove_dir_all(root)
            .map_err(crate::test_support::ctx("remove temporary knowledge root"))?;
        Ok(())
    }

    /// AW5-09: a `ContextProposal` artifact written under `context-proposals/`
    /// is picked up by `rebuild`, just like the six pre-existing surfaces.
    #[test]
    fn rebuild_indexes_context_proposals() -> TestResult {
        use crate::context_proposal::ContextProposal;
        use harw_agent_dsl::ids::DefinitionId;

        let root = temporary_root("harw-knowledge-rebuild-context-proposal")?;
        let store = KnowledgeStore::new(&root);

        let proposal = ContextProposal::new(
            ArtifactId::new("context-proposal/promote-history-tail"),
            "history.tail wiederholt über Budget ausgelassen",
            DefinitionId::parse("harwness.context.base@1")
                .map_err(crate::test_support::ctx("valid definition id"))?,
            "deadbeef".repeat(8),
            Vec::new(),
            Vec::new(),
            jiff::Timestamp::UNIX_EPOCH,
            "heuristic:must-include-promotion@1",
        );
        let mut proposal_frontmatter = frontmatter();
        proposal_frontmatter.visibility = VisibilityScope::OperatorOnly;
        let artifact = proposal
            .to_artifact(proposal_frontmatter)
            .map_err(crate::test_support::ctx("proposal embeds into an artifact"))?;
        store
            .write_artifact(
                &store.context_proposal_path(&ArtifactId::new("promote-history-tail")),
                &artifact,
            )
            .map_err(crate::test_support::ctx("write context-proposal artifact"))?;

        let rebuilt =
            KnowledgeIndex::rebuild(&store).map_err(crate::test_support::ctx("rebuild index"))?;

        assert_eq!(rebuilt.len(), 1);
        let indexed = rebuilt
            .get(&ArtifactId::new("context-proposal/promote-history-tail"))
            .ok_or(TestError::Missing("context-proposal artifact is indexed"))?;
        assert_eq!(indexed.kind, ArtifactKind::ContextProposal);

        std::fs::remove_dir_all(root)
            .map_err(crate::test_support::ctx("remove temporary knowledge root"))?;
        Ok(())
    }

    #[test]
    fn rebuild_treats_a_missing_root_as_an_empty_store() -> TestResult {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(crate::test_support::ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-knowledge-missing-{}-{}",
            std::process::id(),
            nonce
        ));
        let store = KnowledgeStore::new(&root);

        let rebuilt =
            KnowledgeIndex::rebuild(&store).map_err(crate::test_support::ctx("empty rebuild"))?;

        assert!(rebuilt.is_empty());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn markdown_files_rejects_symlink_escapes_and_cycles_but_keeps_regular_directories()
    -> TestResult {
        let root = temporary_root("harw-knowledge-markdown-files")?;
        let external = temporary_root("harw-knowledge-external")?;
        std::fs::write(root.join("inside.md"), "inside")
            .map_err(crate::test_support::ctx("write in-root markdown"))?;
        std::fs::create_dir(root.join("nested"))
            .map_err(crate::test_support::ctx("create in-root nested directory"))?;
        std::fs::write(root.join("nested/inside.md"), "nested inside")
            .map_err(crate::test_support::ctx("write nested in-root markdown"))?;
        std::fs::write(external.join("outside.md"), "outside")
            .map_err(crate::test_support::ctx("write external markdown"))?;
        std::os::unix::fs::symlink(&external, root.join("external")).map_err(
            crate::test_support::ctx("create external directory symlink"),
        )?;
        std::os::unix::fs::symlink(external.join("outside.md"), root.join("outside.md"))
            .map_err(crate::test_support::ctx("create external markdown symlink"))?;
        std::os::unix::fs::symlink(&root, root.join("cycle"))
            .map_err(crate::test_support::ctx("create cyclic directory symlink"))?;

        let canonical_root = std::fs::canonicalize(&root)
            .map_err(crate::test_support::ctx("canonicalize knowledge root"))?;
        let files = markdown_files(&root, &canonical_root)
            .map_err(crate::test_support::ctx("collect bounded markdown files"))?;

        assert_eq!(
            files,
            vec![root.join("inside.md"), root.join("nested/inside.md")]
        );

        std::fs::remove_dir_all(root)
            .map_err(crate::test_support::ctx("remove temporary knowledge root"))?;
        std::fs::remove_dir_all(external)
            .map_err(crate::test_support::ctx("remove temporary external root"))?;
        Ok(())
    }
}
