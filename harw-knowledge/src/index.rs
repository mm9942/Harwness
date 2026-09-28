//! [`KnowledgeIndex`] — the typed, in-memory, rebuildable index (§1.3).
//!
//! Spec: `docs/design/knowledge-surfaces.md` §1.3. Answers four query shapes
//! without a directory walk: by id, by tag/kind, by link (backlinks, powering
//! the palace graph), and by visibility-filtered scope. It is a cache, not a
//! source of truth — the markdown files remain durable, so a corrupt cache is
//! always regenerable. In-memory operations, the JSON cache, and full-disk
//! rebuilds over the bounded surface layout are implemented here.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

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

        for (directory, kind, prefix) in SURFACES {
            index_surface(
                &mut index,
                &mut report,
                store,
                &surface_path(root, directory),
                &canonical_root,
                *kind,
                prefix,
            )?;
        }

        Ok((index, report))
    }
}

/// Die bekannten Wissensflächen: (Verzeichnis relativ zur Wurzel, Art,
/// Id-Präfix). Der Verzeichnisaufbau ist der vertrauenswürdige
/// Typ-Diskriminator; [`KnowledgeIndex::rebuild_with_report`] und die
/// Cache-Signatur ([`KnowledgeIndex::cached`]) laufen über genau diese Liste.
///
/// AW5-09: `ContextProposal`-Artefakte (siehe `crate::context_proposal`)
/// liegen unter `context-proposals/`; ein Speicher ohne dieses Verzeichnis
/// ist unberührt (`index_surface` kehrt für fehlende Verzeichnisse sofort
/// mit `Ok(())` zurück).
const SURFACES: &[(&str, ArtifactKind, &str)] = &[
    ("topics", ArtifactKind::TopicMemory, "topic"),
    ("palace", ArtifactKind::PalaceNode, "palace"),
    ("diary", ArtifactKind::DiaryEntry, "diary"),
    ("dreams", ArtifactKind::DreamReport, "dream"),
    ("workbench", ArtifactKind::WorkbenchNote, "workbench"),
    ("kanban/boards", ArtifactKind::KanbanCard, "kanban"),
    (
        "context-proposals",
        ArtifactKind::ContextProposal,
        "context-proposal",
    ),
];

/// `root` plus ein `/`-getrennter Flächenpfad aus [`SURFACES`].
fn surface_path(root: &Path, directory: &str) -> PathBuf {
    directory
        .split('/')
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

/// Ein Eintrag des prozessweiten Index-Caches ([`KnowledgeIndex::cached`]).
struct CacheEntry {
    signature: u64,
    index: Arc<KnowledgeIndex>,
}

/// Prozessweiter Index-Cache, geschlüsselt nach Speicherwurzel.
fn index_cache() -> &'static Mutex<HashMap<PathBuf, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

impl KnowledgeIndex {
    /// Liefert den Index des Speichers aus dem prozessweiten Cache und baut
    /// ihn nur neu, wenn sich der Bestand geändert hat.
    ///
    /// # Beschreibung
    /// Der Cache ist nach der Speicherwurzel geschlüsselt. Vor jeder Antwort
    /// wird eine billige Signatur des Bestands berechnet — Pfad, Größe und
    /// Änderungszeit (mtime, Nanosekunden) jeder Markdown-Datei derselben
    /// Flächen, die [`Self::rebuild_with_report`] einliest, plus
    /// `core/MEMORY.md`. Dafür wird nur `stat` gelesen, nichts geparst.
    /// Stimmt die Signatur mit dem gecachten Eintrag überein, kommt derselbe
    /// `Arc` zurück; sonst wird neu aufgebaut und der Eintrag ersetzt. Neue,
    /// gelöschte, umbenannte und geänderte Dateien invalidieren den Eintrag
    /// damit zuverlässig; eine Änderung, die weder Größe noch mtime
    /// verschiebt, bliebe bis zur nächsten sichtbaren Änderung unbemerkt
    /// (auf Dateisystemen mit Nanosekunden-mtime praktisch ausgeschlossen).
    ///
    /// Ein vergifteter Mutex wird übernommen statt zu paniken — der Cache ist
    /// nur eine Beschleunigung, sein Inhalt bleibt aus der Platte ableitbar.
    ///
    /// # Errors
    /// Wie [`Self::rebuild`]: nur bei Fehlern des Verzeichnisdurchlaufs.
    pub fn cached(store: &KnowledgeStore) -> KnowledgeResult<Arc<Self>> {
        let key =
            std::fs::canonicalize(store.root()).unwrap_or_else(|_| store.root().to_path_buf());
        let signature = store_signature(store)?;
        {
            let cache = index_cache().lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(entry) = cache.get(&key)
                && entry.signature == signature
            {
                return Ok(Arc::clone(&entry.index));
            }
        }
        let index = Arc::new(Self::rebuild(store)?);
        let mut cache = index_cache().lock().unwrap_or_else(PoisonError::into_inner);
        cache.insert(
            key,
            CacheEntry {
                signature,
                index: Arc::clone(&index),
            },
        );
        Ok(index)
    }

    /// Verwirft den gecachten Index eines Speichers (z. B. nach einem
    /// Schreibvorgang, der die mtime-Auflösung unterlaufen könnte).
    pub fn invalidate_cached(store: &KnowledgeStore) {
        let key =
            std::fs::canonicalize(store.root()).unwrap_or_else(|_| store.root().to_path_buf());
        index_cache()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key);
    }
}

/// Signatur des Bestands für [`KnowledgeIndex::cached`]: Hash über Pfad,
/// Größe und mtime aller indexierten Markdown-Dateien.
fn store_signature(store: &KnowledgeStore) -> KnowledgeResult<u64> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let root = store.root();
    if !root.exists() {
        0u8.hash(&mut hasher);
        return Ok(hasher.finish());
    }
    let canonical_root = std::fs::canonicalize(root)?;
    hash_file_stamp(&mut hasher, &store.core_memory_path());
    for (directory, _kind, _prefix) in SURFACES {
        let surface = surface_path(root, directory);
        directory.hash(&mut hasher);
        if !surface.is_dir() {
            continue;
        }
        for file in markdown_files(&surface, &canonical_root)? {
            hash_file_stamp(&mut hasher, &file);
        }
    }
    Ok(hasher.finish())
}

/// Hasht Pfad, Größe und mtime einer Datei; eine fehlende Datei zählt als
/// eigener Zustand.
fn hash_file_stamp(hasher: &mut impl Hasher, path: &Path) {
    path.hash(hasher);
    match std::fs::metadata(path) {
        Ok(metadata) => {
            metadata.len().hash(hasher);
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|elapsed| elapsed.as_nanos());
            modified.hash(hasher);
        }
        Err(_) => u64::MAX.hash(hasher),
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

    /// Regression (R2/W1): ein über `KnowledgeStore::write_dream_report`
    /// geschriebener Traumbericht trägt gültiges Frontmatter und wird von
    /// `rebuild` als `DreamReport` indexiert, statt den Rebuild abzubrechen.
    #[test]
    fn rebuild_indexes_a_written_dream_report() -> TestResult {
        use crate::dream::DreamReport;

        let root = temporary_root("harw-knowledge-rebuild-dream")?;
        let store = KnowledgeStore::new(&root);
        let report = DreamReport {
            work_id: harw_job_core::WorkId::from_str("dream-20260923T010203"),
            created_at: jiff::Timestamp::UNIX_EPOCH,
            summary: "Konsolidierte Reflexion".to_owned(),
            proposed_topic_updates: Vec::new(),
            proposed_palace_promotions: Vec::new(),
            follow_ups: vec!["Deploy prüfen".to_owned()],
        };

        let path = store.write_dream_report(&report)?;
        assert_eq!(
            path,
            store.dream_path("1970-01-01", "dream-20260923T010203")
        );

        let (rebuilt, index_report) = KnowledgeIndex::rebuild_with_report(&store)?;

        assert!(
            index_report.is_clean(),
            "skipped: {:?}",
            index_report.skipped
        );
        assert_eq!(index_report.indexed, 1);
        let indexed = rebuilt
            .get(&report.artifact_id())
            .ok_or(TestError::Missing("dream report is indexed"))?;
        assert_eq!(indexed.kind, ArtifactKind::DreamReport);
        assert_eq!(
            indexed.frontmatter.visibility,
            VisibilityScope::OperatorOnly
        );
        assert!(indexed.body.contains("Konsolidierte Reflexion"));

        std::fs::remove_dir_all(root)
            .map_err(crate::test_support::ctx("remove temporary knowledge root"))?;
        Ok(())
    }

    /// Eine einzelne defekte Datei (hier: Markdown ohne Frontmatter, wie der
    /// frühere Gateway-Traumbericht) wird übersprungen und berichtet; gültige
    /// Artefakte daneben werden weiterhin indexiert.
    #[test]
    fn rebuild_skips_and_reports_a_malformed_artifact() -> TestResult {
        let root = temporary_root("harw-knowledge-rebuild-malformed")?;
        let store = KnowledgeStore::new(&root);

        let malformed = store.dream_path("2026-09-23", "legacy");
        std::fs::create_dir_all(
            malformed
                .parent()
                .ok_or(TestError::Missing("dream path has a parent"))?,
        )?;
        std::fs::write(&malformed, "# Traumbericht ohne Frontmatter\n")?;

        let palace = KnowledgeArtifact::new(
            ArtifactId::new("palace/deploy"),
            ArtifactKind::PalaceNode,
            frontmatter(),
            "deployment detail",
        );
        store.write_artifact(&store.palace_path(&ArtifactId::new("deploy")), &palace)?;

        let (rebuilt, index_report) = KnowledgeIndex::rebuild_with_report(&store)?;

        assert_eq!(rebuilt.len(), 1);
        assert!(rebuilt.get(&ArtifactId::new("palace/deploy")).is_some());
        assert_eq!(index_report.indexed, 1);
        assert_eq!(index_report.skipped.len(), 1);
        assert_eq!(index_report.skipped[0].0, malformed);
        assert!(index_report.skipped[0].1.contains("frontmatter"));

        // Die bequeme `rebuild`-Variante bricht ebenfalls nicht ab.
        assert_eq!(KnowledgeIndex::rebuild(&store)?.len(), 1);

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

    /// D4: `cached` liefert bei unverändertem Bestand denselben Index und
    /// baut bei neuer, geänderter oder gelöschter Datei neu auf.
    #[test]
    fn cached_index_is_reused_until_the_store_changes() -> TestResult {
        use crate::memory::topic;

        let root = temporary_root("harw-knowledge-index-cache")?;
        let store = KnowledgeStore::new(&root);
        topic::write(&store, "a", frontmatter(), "eins")
            .map_err(crate::test_support::ctx("write topic a"))?;

        let first =
            KnowledgeIndex::cached(&store).map_err(crate::test_support::ctx("first cached"))?;
        let again =
            KnowledgeIndex::cached(&store).map_err(crate::test_support::ctx("second cached"))?;
        assert!(
            Arc::ptr_eq(&first, &again),
            "unchanged store reuses the index"
        );
        assert_eq!(first.len(), 1);

        // Neue Datei → neu aufbauen.
        topic::write(&store, "b", frontmatter(), "zwei")
            .map_err(crate::test_support::ctx("write topic b"))?;
        let added =
            KnowledgeIndex::cached(&store).map_err(crate::test_support::ctx("cached after add"))?;
        assert!(!Arc::ptr_eq(&first, &added));
        assert_eq!(added.len(), 2);

        // Geänderter Inhalt → neu aufbauen, neuer Body sichtbar.
        topic::write(&store, "a", frontmatter(), "eins, jetzt deutlich länger")
            .map_err(crate::test_support::ctx("rewrite topic a"))?;
        let changed = KnowledgeIndex::cached(&store)
            .map_err(crate::test_support::ctx("cached after change"))?;
        assert!(!Arc::ptr_eq(&added, &changed));
        let body = &changed
            .get(&ArtifactId::new("topic/a"))
            .ok_or(TestError::Missing("topic/a indexed"))?
            .body;
        assert!(body.contains("deutlich"), "{body}");

        // Gelöschte Datei → neu aufbauen.
        std::fs::remove_file(store.topic_path("b"))
            .map_err(crate::test_support::ctx("remove topic b"))?;
        let removed = KnowledgeIndex::cached(&store)
            .map_err(crate::test_support::ctx("cached after remove"))?;
        assert_eq!(removed.len(), 1);

        // Ausdrückliches Verwerfen erzwingt einen frischen Aufbau.
        KnowledgeIndex::invalidate_cached(&store);
        let fresh = KnowledgeIndex::cached(&store)
            .map_err(crate::test_support::ctx("cached after invalidate"))?;
        assert!(!Arc::ptr_eq(&removed, &fresh));

        std::fs::remove_dir_all(root)
            .map_err(crate::test_support::ctx("remove temporary knowledge root"))?;
        Ok(())
    }

    /// Zwei Speicher im selben Prozess teilen sich keinen Cache-Eintrag.
    #[test]
    fn cached_index_is_keyed_by_store_root() -> TestResult {
        use crate::memory::topic;

        let first_root = temporary_root("harw-knowledge-index-cache-a")?;
        let second_root = temporary_root("harw-knowledge-index-cache-b")?;
        let first = KnowledgeStore::new(&first_root);
        let second = KnowledgeStore::new(&second_root);
        topic::write(&first, "only-here", frontmatter(), "x")
            .map_err(crate::test_support::ctx("write topic"))?;
        let a = KnowledgeIndex::cached(&first).map_err(crate::test_support::ctx("cached a"))?;
        let b = KnowledgeIndex::cached(&second).map_err(crate::test_support::ctx("cached b"))?;
        assert_eq!(a.len(), 1);
        assert!(b.is_empty());
        std::fs::remove_dir_all(first_root).ok();
        std::fs::remove_dir_all(second_root).ok();
        Ok(())
    }
}
