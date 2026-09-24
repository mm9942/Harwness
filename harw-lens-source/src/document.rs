//! Quellenerfassung: welche Dateien/Artefakte zu welchem Index gehören,
//! bevor sie zerlegt werden.
//!
//! # Verantwortungsbereich
//! Besitzt [`RawDocument`] (ein noch nicht zerlegtes Dokument samt der
//! Sichtbarkeit, in deren physischen Index es gehört),
//! [`visibility_of_scope`] (die einzige Stelle, die eine
//! [`harw_knowledge::VisibilityScope`] auf einen Sichtbarkeits-Bucket-Namen
//! abbildet) sowie vier Quellenerfassungen: [`collect_design_docs`]
//! ([`crate::DOCS_DESIGN_INDEX`]), [`collect_palace_documents`]
//! ([`crate::KNOWLEDGE_PALACE_INDEX`]), [`collect_diary_documents`]
//! ([`crate::KNOWLEDGE_DIARY_INDEX`], Knoten AW7-05 — wörtlich
//! [`collect_palace_documents`] mit vertauschtem `ArtifactKind`-Filter) und
//! [`collect_code_sources`] (Quelltext aller gängigen Sprachen, Endungen aus
//! [`CODE_SOURCE_EXTENSIONS`], Build-/Abhängigkeitsverzeichnisse aus
//! [`SKIPPED_SOURCE_DIRS`] ausgeschlossen; die Zerlegungsstrategie je Datei
//! liefert [`chunker_for_path`]). Der veraltete Einstiegspunkt
//! [`collect_rust_sources`] ([`crate::CODE_RUST_INDEX`], Knoten AW7-05)
//! ist dieselbe Erfassung, eingeschränkt auf `.rs`. Dieses Modul tut kein
//! Chunking und kein Einbetten — das übernimmt [`crate::build_index`] mit
//! dem Ergebnis dieses Moduls als Eingabe.
//!
//! # Warum `visibility_of_scope` nur zwei Buckets kennt
//! [`harw_knowledge::VisibilityScope`] hat vier Varianten
//! (`SelfOnly`, `DescendantTree`, `ExplicitlyGranted`, `OperatorOnly`); die
//! vollständige Durchsetzung aller vier ist laut `harw-knowledge`s eigener
//! Modul-Dokumentation (`harw-knowledge/src/visibility.rs`) explizit
//! `harw-policy`s Aufgabe, die zum Zeitpunkt dieses Knotens (AW5-08) noch
//! nicht existiert. Diese Funktion trifft deshalb bewusst nur die eine
//! Unterscheidung, die der Knoten beweisen muss: `OperatorOnly` landet im
//! getrennten `operator-only`-Index, alles andere im gewöhnlichen
//! `workspace`-Index. Das ist eine bewusste Verengung, keine versehentliche
//! Lücke — sie ist konservativ in die sichere Richtung (mehr Material im
//! restriktiveren Bucket wäre der falsche Fehler; hier gilt das Gegenteil
//! nicht: nur explizit `OperatorOnly` erhält den Schutz). Eine feinere
//! Aufteilung (etwa ein eigener Bucket je `ExplicitlyGranted`-Rollenliste)
//! bleibt einem späteren Knoten vorbehalten, sobald `harw-policy` die
//! zugehörige Zugriffsprüfung liefert.
//!
//! # Nebenläufigkeit
//! [`RawDocument`] ist reine Daten (`Clone`, `PartialEq`) ohne interne
//! Veränderlichkeit. [`visibility_of_scope`], [`collect_design_docs`] und
//! [`collect_palace_documents`] sind reine bzw. rein lesende Funktionen ohne
//! geteilten veränderlichen Zustand.
//!
//! # Fehler
//! [`collect_design_docs`] liefert [`crate::SourceError::Io`] bei
//! Dateisystemfehlern. [`collect_palace_documents`] und
//! [`visibility_of_scope`] erzeugen keine Fehler.
//!
//! # Examples
//! ```rust,no_run
//! use harw_lens_source::collect_design_docs;
//!
//! let docs_root = tempfile::tempdir()?;
//! std::fs::write(docs_root.path().join("intro.md"), "# Intro\n\nText.\n")?;
//! let documents = collect_design_docs(docs_root.path())?;
//! assert_eq!(documents.len(), 1);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::path::{Path, PathBuf};

use harw_knowledge::{ArtifactKind, KnowledgeIndex, VisibilityScope};
use harw_lens_types::SourceRef;

use crate::error::SourceResult;
use crate::{DEFAULT_VISIBILITY, OPERATOR_ONLY_VISIBILITY};

/// Ein noch nicht zerlegtes Dokument, gebunden an die Sichtbarkeit seines
/// künftigen physischen Index.
///
/// # Description
/// Das gemeinsame Eingabeformat für [`crate::build_index`], unabhängig davon,
/// ob es aus [`collect_design_docs`] oder [`collect_palace_documents`]
/// stammt.
#[derive(Debug, Clone, PartialEq)]
pub struct RawDocument {
    /// Woher der Text stammt; wird unverändert in jeden aus diesem Dokument
    /// erzeugten [`harw_lens_types::Chunk`] übernommen.
    pub source: SourceRef,
    /// Der vollständige, noch nicht zerlegte Text.
    pub text: String,
    /// Der Sichtbarkeits-Bucket-Name, dessen physischer Index dieses
    /// Dokument aufnimmt (siehe [`harw_home::paths::visibility_index_dir`]).
    pub visibility: String,
}

/// Bildet eine [`VisibilityScope`] auf einen Sichtbarkeits-Bucket-Namen ab.
///
/// # Description
/// Siehe den `# Warum visibility_of_scope nur zwei Buckets kennt`-Abschnitt
/// der Moduldokumentation für die Begründung dieser bewussten Verengung auf
/// zwei Buckets.
///
/// # Arguments
/// - `scope` (`&VisibilityScope`): die Sichtbarkeit eines
///   [`harw_knowledge::KnowledgeArtifact`].
///
/// # Returns
/// [`OPERATOR_ONLY_VISIBILITY`](crate::OPERATOR_ONLY_VISIBILITY) für
/// [`VisibilityScope::OperatorOnly`], sonst
/// [`DEFAULT_VISIBILITY`](crate::DEFAULT_VISIBILITY).
///
/// # Examples
/// ```rust
/// use harw_knowledge::VisibilityScope;
/// use harw_lens_source::{visibility_of_scope, DEFAULT_VISIBILITY, OPERATOR_ONLY_VISIBILITY};
///
/// assert_eq!(visibility_of_scope(&VisibilityScope::OperatorOnly), OPERATOR_ONLY_VISIBILITY);
/// assert_eq!(visibility_of_scope(&VisibilityScope::SelfOnly), DEFAULT_VISIBILITY);
/// ```
#[must_use]
pub fn visibility_of_scope(scope: &VisibilityScope) -> &'static str {
    match scope {
        VisibilityScope::OperatorOnly => OPERATOR_ONLY_VISIBILITY,
        VisibilityScope::SelfOnly
        | VisibilityScope::DescendantTree
        | VisibilityScope::ExplicitlyGranted(_) => DEFAULT_VISIBILITY,
    }
}

/// Erfasst alle Markdown-Dateien unterhalb von `root` als Quellen für
/// [`crate::DOCS_DESIGN_INDEX`].
///
/// # Description
/// Läuft `root` rekursiv ab und sammelt jede reguläre `.md`-Datei; Symlinks
/// (Datei- wie Verzeichnis-Symlinks) werden nie befolgt — ein
/// Verzeichnis-Symlink könnte sonst aus `root` hinausführen oder einen Zyklus
/// einführen, ein Datei-Symlink könnte eine fremde Datei als Quelle
/// erscheinen lassen. Jede gefundene Datei wird vollständig als UTF-8
/// eingelesen; das Ergebnis ist nach dem `root`-relativen Pfad sortiert, für
/// deterministische Build-Reihenfolge unabhängig von der
/// Verzeichnis-Iterationsreihenfolge des Betriebssystems. Jedes
/// [`RawDocument`] erhält
/// [`DEFAULT_VISIBILITY`](crate::DEFAULT_VISIBILITY) — Design-Dokumente
/// tragen in diesem Knoten keine artefaktweise Sichtbarkeit.
///
/// # Arguments
/// - `root` (`&Path`): das Wurzelverzeichnis der Design-Dokumente. Ein nicht
///   existierendes oder kein Verzeichnis darstellendes `root` liefert eine
///   leere Liste, keinen Fehler.
///
/// # Returns
/// `Vec<RawDocument>`, sortiert nach `root`-relativem Pfad (Vorwärtsschrägstriche,
/// unabhängig von der Plattform).
///
/// # Errors
/// - [`crate::SourceError::Io`]: ein Dateisystemzugriff (Lesen des
///   Verzeichnisses, einer Datei, oder `canonicalize`) schlägt fehl, oder
///   eine Datei ist kein valides UTF-8.
///
/// # Examples
/// ```rust,no_run
/// use harw_lens_source::collect_design_docs;
///
/// let docs_root = tempfile::tempdir()?;
/// std::fs::write(docs_root.path().join("intro.md"), "# Intro\n")?;
/// let documents = collect_design_docs(docs_root.path())?;
/// assert_eq!(documents.len(), 1);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn collect_design_docs(root: &Path) -> SourceResult<Vec<RawDocument>> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let canonical_root = std::fs::canonicalize(root)?;
    let mut files = Vec::new();
    walk_markdown_files(root, &canonical_root, &mut files)?;
    files.sort();

    let mut documents = Vec::with_capacity(files.len());
    for path in files {
        let text = std::fs::read_to_string(&path)?;
        let relative = path.strip_prefix(root).unwrap_or(path.as_path());
        let display_path = relative
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        documents.push(RawDocument {
            source: SourceRef::File { path: display_path },
            text,
            visibility: DEFAULT_VISIBILITY.to_owned(),
        });
    }
    Ok(documents)
}

/// Rekursive Hilfsfunktion für [`collect_design_docs`]: sammelt reguläre
/// `.md`-Dateien unterhalb von `dir`, ohne Symlinks zu befolgen.
fn walk_markdown_files(
    dir: &Path,
    canonical_root: &Path,
    out: &mut Vec<PathBuf>,
) -> SourceResult<()> {
    let canonical_dir = std::fs::canonicalize(dir)?;
    if !canonical_dir.starts_with(canonical_root) {
        return Ok(());
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = std::fs::symlink_metadata(&path)?.file_type();
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            walk_markdown_files(&path, canonical_root, out)?;
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
    Ok(())
}

/// Dateiendungen (klein geschrieben, ohne Punkt), die [`collect_code_sources`]
/// als Quelltext erfasst.
///
/// # Description
/// Deckt die gängigen Programmiersprachen (Rust, TypeScript/JavaScript,
/// Python, Go, JVM, C/C++, C#, Ruby, PHP, Swift, Shell) sowie
/// Konfigurationsformate ab, die typischerweise neben Quelltext liegen
/// (`toml`, `yaml`/`yml`). Markdown ist bewusst **nicht** enthalten: Prosa
/// gehört in [`crate::DOCS_DESIGN_INDEX`] ([`collect_design_docs`]); ein
/// Code-Walk über dieselbe Wurzel würde sonst jede `.md`-Datei doppelt
/// indizieren. Der Vergleich erfolgt ohne Beachtung der Groß-/Kleinschreibung.
pub const CODE_SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "kt", "kts", "c", "h", "cc",
    "cpp", "hpp", "cs", "rb", "php", "swift", "sh", "bash", "toml", "yaml", "yml",
];

/// Verzeichnisnamen, die [`collect_code_sources`] (und
/// [`collect_rust_sources`]) nie betreten.
///
/// # Description
/// Build-Ausgaben (`target`, `dist`, `build`), heruntergeladene
/// Abhängigkeiten (`node_modules`, virtuelle Python-Umgebungen `.venv`/`venv`),
/// Bytecode-Caches (`__pycache__`) und VCS-Metadaten (`.git`, `.hg`, `.svn`):
/// kein Quelltext des Projekts selbst, aber potenziell riesig.
pub const SKIPPED_SOURCE_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    ".hg",
    ".svn",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
];

/// Obergrenze in Bytes für eine einzelne von [`collect_code_sources`]
/// erfasste Datei (1 MiB).
///
/// # Description
/// Größere Dateien sind in aller Regel generiert (minifizierte Bundles,
/// Lockfile-artige YAML-Dumps, eingebettete Daten) und werden übersprungen,
/// statt den Index mit wenig aussagekräftigen Chunks zu fluten.
pub const CODE_SOURCE_MAX_BYTES: u64 = 1024 * 1024;

/// Zerlegungsstrategie aus `harw-lens-chunk`, die zu einer erfassten
/// Quelldatei passt (siehe [`chunker_for_path`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeChunker {
    /// `harw_lens_chunk::chunk_rust` — Elementgrenzen (`fn`, `struct`, …).
    Rust,
    /// `harw_lens_chunk::chunk_markdown` — Überschriftengrenzen.
    Markdown,
    /// `harw_lens_chunk::chunk_plain` — Fenster mit Überlappung; für jeden
    /// Quelltext ohne eigene Strategie.
    Plain,
}

/// Ordnet einem Dateipfad die passende Zerlegungsstrategie zu.
///
/// # Description
/// `harw-lens-chunk` kennt genau drei Strategien: `chunk_rust`,
/// `chunk_markdown` und `chunk_plain`. `.rs` → [`CodeChunker::Rust`],
/// `.md`/`.markdown` → [`CodeChunker::Markdown`], alles andere (TypeScript,
/// Python, Go, …, auch Dateien ohne Endung) → [`CodeChunker::Plain`].
/// Der Vergleich der Endung ignoriert Groß-/Kleinschreibung.
///
/// # Arguments
/// - `path` (`&Path`): Pfad der Datei (relativ oder absolut; nur die Endung
///   zählt).
///
/// # Returns
/// Die passende [`CodeChunker`]-Variante.
///
/// # Examples
/// ```rust,ignore
/// use std::path::Path;
/// assert_eq!(chunker_for_path(Path::new("src/lib.rs")), CodeChunker::Rust);
/// assert_eq!(chunker_for_path(Path::new("app/main.ts")), CodeChunker::Plain);
/// ```
#[must_use]
pub fn chunker_for_path(path: &Path) -> CodeChunker {
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase());
    match extension.as_deref() {
        Some("rs") => CodeChunker::Rust,
        Some("md" | "markdown") => CodeChunker::Markdown,
        _ => CodeChunker::Plain,
    }
}

/// Erfasst alle Quelltextdateien unterhalb von `root` (Endungen aus
/// [`CODE_SOURCE_EXTENSIONS`]) als Code-Quellen.
///
/// # Description
/// Dieselbe symlink-sichere, deterministisch sortierte Erfassung wie
/// [`collect_design_docs`], angewendet auf alle Endungen aus
/// [`CODE_SOURCE_EXTENSIONS`]. Verzeichnisse aus [`SKIPPED_SOURCE_DIRS`]
/// werden nie betreten; Dateien über [`CODE_SOURCE_MAX_BYTES`] sowie
/// Dateien, die kein valides UTF-8 sind (etwa Latin-1-kodierte C-Quellen),
/// werden übersprungen statt die ganze Erfassung abzubrechen. Jedes
/// [`RawDocument`] erhält [`DEFAULT_VISIBILITY`](crate::DEFAULT_VISIBILITY).
///
/// Die passende Zerlegungsstrategie je Datei liefert [`chunker_for_path`]
/// (`.rs` → `chunk_rust`, übrige Endungen → `chunk_plain`).
///
/// # Arguments
/// - `root` (`&Path`): das Wurzelverzeichnis des Quellbaums. Ein nicht
///   existierendes oder kein Verzeichnis darstellendes `root` liefert eine
///   leere Liste, keinen Fehler.
///
/// # Returns
/// `Vec<RawDocument>`, sortiert nach `root`-relativem Pfad
/// (Vorwärtsschrägstriche, unabhängig von der Plattform).
///
/// # Errors
/// - [`crate::SourceError::Io`]: ein Verzeichnis- oder Dateizugriff schlägt
///   fehl (nicht aber ungültiges UTF-8 — solche Dateien werden übersprungen).
///
/// # Examples
/// ```rust,ignore
/// let src_root = tempfile::tempdir()?;
/// std::fs::write(src_root.path().join("lib.rs"), "pub fn f() {}\n")?;
/// std::fs::write(src_root.path().join("app.ts"), "export const x = 1;\n")?;
/// let documents = collect_code_sources(src_root.path())?;
/// assert_eq!(documents.len(), 2);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn collect_code_sources(root: &Path) -> SourceResult<Vec<RawDocument>> {
    collect_sources_with_extensions(root, CODE_SOURCE_EXTENSIONS)
}

/// Erfasst ausschließlich Rust-Quelldateien unterhalb von `root` als Quellen
/// für [`crate::CODE_RUST_INDEX`] (Knoten AW7-05).
///
/// # Description
/// Veralteter Einstiegspunkt: entspricht [`collect_code_sources`], jedoch
/// auf die Endung `.rs` eingeschränkt. Die Einschränkung bleibt bewusst
/// erhalten, weil [`crate::CODE_RUST_INDEX`] jedes Dokument mit
/// `harw_lens_chunk::chunk_rust` zerlegt — TypeScript oder Python dort
/// hineinzugeben wäre falsch. Neue Aufrufer verwenden
/// [`collect_code_sources`] und wählen die Zerlegung je Datei über
/// [`chunker_for_path`].
///
/// # Arguments
/// - `root` (`&Path`): das Wurzelverzeichnis des Quellbaums. Ein nicht
///   existierendes oder kein Verzeichnis darstellendes `root` liefert eine
///   leere Liste, keinen Fehler.
///
/// # Returns
/// `Vec<RawDocument>`, sortiert nach `root`-relativem Pfad.
///
/// # Errors
/// - [`crate::SourceError::Io`]: ein Dateisystemzugriff schlägt fehl.
#[deprecated(
    note = "use `collect_code_sources` (all common source extensions) together with `chunker_for_path`"
)]
pub fn collect_rust_sources(root: &Path) -> SourceResult<Vec<RawDocument>> {
    collect_sources_with_extensions(root, &["rs"])
}

/// Gemeinsamer Kern von [`collect_code_sources`] und
/// [`collect_rust_sources`]: Walk, Sortierung, Einlesen.
fn collect_sources_with_extensions(
    root: &Path,
    extensions: &[&str],
) -> SourceResult<Vec<RawDocument>> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let canonical_root = std::fs::canonicalize(root)?;
    let mut files = Vec::new();
    walk_code_files(root, &canonical_root, extensions, &mut files)?;
    files.sort();

    let mut documents = Vec::with_capacity(files.len());
    for path in files {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => continue,
            Err(error) => return Err(error.into()),
        };
        let relative = path.strip_prefix(root).unwrap_or(path.as_path());
        let display_path = relative
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        documents.push(RawDocument {
            source: SourceRef::File { path: display_path },
            text,
            visibility: DEFAULT_VISIBILITY.to_owned(),
        });
    }
    Ok(documents)
}

/// `true`, wenn die Endung von `path` (ohne Beachtung der
/// Groß-/Kleinschreibung) in `extensions` enthalten ist.
fn has_source_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension().is_some_and(|ext| {
        let ext = ext.to_string_lossy();
        extensions
            .iter()
            .any(|candidate| ext.eq_ignore_ascii_case(candidate))
    })
}

/// Rekursive Hilfsfunktion für [`collect_sources_with_extensions`]: sammelt
/// reguläre Dateien mit einer Endung aus `extensions` unterhalb von `dir`,
/// ohne Symlinks zu befolgen, ohne je ein Verzeichnis aus
/// [`SKIPPED_SOURCE_DIRS`] zu betreten und ohne Dateien über
/// [`CODE_SOURCE_MAX_BYTES`]. Eigenständig von [`walk_markdown_files`],
/// damit [`collect_design_docs`]s bereits getestetes Verhalten unverändert
/// bleibt.
fn walk_code_files(
    dir: &Path,
    canonical_root: &Path,
    extensions: &[&str],
    out: &mut Vec<PathBuf>,
) -> SourceResult<()> {
    let canonical_dir = std::fs::canonicalize(dir)?;
    if !canonical_dir.starts_with(canonical_root) {
        return Ok(());
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let skipped = path
                .file_name()
                .is_some_and(|name| SKIPPED_SOURCE_DIRS.iter().any(|skip| name == *skip));
            if skipped {
                continue;
            }
            walk_code_files(&path, canonical_root, extensions, out)?;
        } else if file_type.is_file()
            && metadata.len() <= CODE_SOURCE_MAX_BYTES
            && has_source_extension(&path, extensions)
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Erfasst alle Palace-Knoten aus einem [`KnowledgeIndex`] als Quellen für
/// [`crate::KNOWLEDGE_PALACE_INDEX`].
///
/// # Description
/// Filtert auf [`ArtifactKind::PalaceNode`] (Diary-Einträge, Kanban-Karten
/// usw. gehören nicht in diesen Index) und bildet jeden Treffer auf ein
/// [`RawDocument`] ab: `source` wird [`SourceRef::Artifact`] mit der
/// Artefakt-Id, `text` ist der bereits Frontmatter-freie Markdown-Body
/// ([`harw_knowledge::KnowledgeArtifact::body`]), `visibility` kommt aus
/// [`visibility_of_scope`] über
/// [`harw_knowledge::Frontmatter::visibility`]. Das Ergebnis wird nach
/// Artefakt-Id sortiert — [`KnowledgeIndex::iter`] iteriert über eine
/// `HashMap` und liefert deshalb keine stabile Reihenfolge; ohne diese
/// Sortierung wäre die Build-Reihenfolge (und damit die Reihenfolge der
/// später über `suggest_relations` möglichen Vorschläge) von der
/// Hash-Iterationsreihenfolge des Prozesses abhängig statt deterministisch.
///
/// # Arguments
/// - `index` (`&KnowledgeIndex`): der bereits (z. B. über
///   [`KnowledgeIndex::rebuild`]) aufgebaute Wissensindex.
///
/// # Returns
/// `Vec<RawDocument>`, ein Eintrag je Palace-Knoten, sortiert nach
/// Artefakt-Id.
///
/// # Examples
/// ```rust
/// use harw_knowledge::{
///     ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact, KnowledgeIndex, VisibilityScope,
/// };
/// use harw_knowledge::AgentId;
/// use harw_lens_source::collect_palace_documents;
///
/// let mut index = KnowledgeIndex::new();
/// let frontmatter = Frontmatter::new(
///     AgentId::new("agent"),
///     VisibilityScope::SelfOnly,
///     jiff::Timestamp::now(),
/// );
/// index.insert(KnowledgeArtifact::new(
///     ArtifactId::new("palace/deploy"),
///     ArtifactKind::PalaceNode,
///     frontmatter,
///     "deployment detail",
/// ));
///
/// let documents = collect_palace_documents(&index);
/// assert_eq!(documents.len(), 1);
/// assert_eq!(documents[0].text, "deployment detail");
/// ```
#[must_use]
pub fn collect_palace_documents(index: &KnowledgeIndex) -> Vec<RawDocument> {
    let mut documents: Vec<RawDocument> = index
        .iter()
        .filter(|artifact| artifact.kind == ArtifactKind::PalaceNode)
        .map(|artifact| RawDocument {
            source: SourceRef::Artifact {
                id: artifact.id.as_str().to_owned(),
            },
            text: artifact.body.clone(),
            visibility: visibility_of_scope(&artifact.frontmatter.visibility).to_owned(),
        })
        .collect();
    documents.sort_by_key(|d| source_sort_key(&d.source));
    documents
}

/// Erfasst alle Diary-Einträge aus einem [`KnowledgeIndex`] als Quellen für
/// [`crate::KNOWLEDGE_DIARY_INDEX`] (Knoten AW7-05).
///
/// # Description
/// Wörtlich [`collect_palace_documents`] mit vertauschtem
/// `ArtifactKind`-Filter (`DiaryEntry` statt `PalaceNode`): dieselbe
/// Struktur (Frontmatter mit `VisibilityScope`, Frontmatter-freier
/// Markdown-Body), dieselbe [`visibility_of_scope`]-Zuordnung, dieselbe
/// Sortierung nach `SourceRef`, aus demselben Grund (`KnowledgeIndex::iter`
/// iteriert über eine `HashMap`, also ohne stabile Reihenfolge ohne diese
/// Sortierung).
///
/// # Arguments
/// - `index` (`&KnowledgeIndex`): der bereits aufgebaute Wissensindex.
///
/// # Returns
/// `Vec<RawDocument>`, ein Eintrag je Diary-Eintrag, sortiert nach
/// Artefakt-Id.
///
/// # Examples
/// ```rust
/// use harw_knowledge::{
///     ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact, KnowledgeIndex, VisibilityScope,
/// };
/// use harw_knowledge::AgentId;
/// use harw_lens_source::collect_diary_documents;
///
/// let mut index = KnowledgeIndex::new();
/// let frontmatter = Frontmatter::new(
///     AgentId::new("agent"),
///     VisibilityScope::SelfOnly,
///     jiff::Timestamp::now(),
/// );
/// index.insert(KnowledgeArtifact::new(
///     ArtifactId::new("diary/2026-09-01"),
///     ArtifactKind::DiaryEntry,
///     frontmatter,
///     "today's entry",
/// ));
///
/// let documents = collect_diary_documents(&index);
/// assert_eq!(documents.len(), 1);
/// assert_eq!(documents[0].text, "today's entry");
/// ```
#[must_use]
pub fn collect_diary_documents(index: &KnowledgeIndex) -> Vec<RawDocument> {
    let mut documents: Vec<RawDocument> = index
        .iter()
        .filter(|artifact| artifact.kind == ArtifactKind::DiaryEntry)
        .map(|artifact| RawDocument {
            source: SourceRef::Artifact {
                id: artifact.id.as_str().to_owned(),
            },
            text: artifact.body.clone(),
            visibility: visibility_of_scope(&artifact.frontmatter.visibility).to_owned(),
        })
        .collect();
    documents.sort_by_key(|d| source_sort_key(&d.source));
    documents
}

/// Baut einen sortierbaren Schlüssel aus einer [`SourceRef`], weil
/// [`SourceRef`] selbst kein `Ord` ableitet.
fn source_sort_key(source: &SourceRef) -> String {
    match source {
        SourceRef::File { path } => format!("file:{path}"),
        SourceRef::Artifact { id } => format!("artifact:{id}"),
        SourceRef::PlanNode { plan, node } => format!("plan-node:{plan}:{node}"),
        SourceRef::Diary { entry } => format!("diary:{entry}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;
    use harw_knowledge::{AgentId, ArtifactId, Frontmatter, KnowledgeArtifact};

    fn frontmatter(scope: VisibilityScope) -> Frontmatter {
        Frontmatter::new(AgentId::new("agent"), scope, jiff::Timestamp::now())
    }

    #[test]
    fn test_visibility_of_scope_maps_operator_only_to_operator_bucket() -> TestResult {
        assert_eq!(
            visibility_of_scope(&VisibilityScope::OperatorOnly),
            OPERATOR_ONLY_VISIBILITY
        );
        Ok(())
    }

    #[test]
    fn test_visibility_of_scope_maps_every_other_scope_to_default_bucket() -> TestResult {
        assert_eq!(
            visibility_of_scope(&VisibilityScope::SelfOnly),
            DEFAULT_VISIBILITY
        );
        assert_eq!(
            visibility_of_scope(&VisibilityScope::DescendantTree),
            DEFAULT_VISIBILITY
        );
        assert_eq!(
            visibility_of_scope(&VisibilityScope::ExplicitlyGranted(Vec::new())),
            DEFAULT_VISIBILITY
        );
        Ok(())
    }

    #[test]
    fn test_collect_design_docs_missing_root_returns_empty_list() -> TestResult {
        let root = std::path::Path::new("/does/not/exist/harw-lens-source-test");
        assert_eq!(collect_design_docs(root)?, Vec::new());
        Ok(())
    }

    #[test]
    fn test_collect_design_docs_reads_markdown_files_recursively_and_sorted() -> TestResult {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("b.md"), "second")?;
        std::fs::create_dir(dir.path().join("nested"))?;
        std::fs::write(dir.path().join("nested/a.md"), "first")?;
        std::fs::write(dir.path().join("ignore.txt"), "not markdown")?;

        let documents = collect_design_docs(dir.path())?;
        assert_eq!(documents.len(), 2);
        assert_eq!(
            documents[0].source,
            SourceRef::File {
                path: "b.md".to_owned()
            }
        );
        assert_eq!(
            documents[1].source,
            SourceRef::File {
                path: "nested/a.md".to_owned()
            }
        );
        assert_eq!(documents[0].visibility, DEFAULT_VISIBILITY);
        Ok(())
    }

    #[test]
    fn test_collect_palace_documents_filters_to_palace_kind_only() -> TestResult {
        let mut index = KnowledgeIndex::new();
        index.insert(KnowledgeArtifact::new(
            ArtifactId::new("palace/a"),
            ArtifactKind::PalaceNode,
            frontmatter(VisibilityScope::SelfOnly),
            "palace body",
        ));
        index.insert(KnowledgeArtifact::new(
            ArtifactId::new("topic/b"),
            ArtifactKind::TopicMemory,
            frontmatter(VisibilityScope::SelfOnly),
            "topic body",
        ));

        let documents = collect_palace_documents(&index);
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].text, "palace body");
        assert_eq!(
            documents[0].source,
            SourceRef::Artifact {
                id: "palace/a".to_owned()
            }
        );
        Ok(())
    }

    #[test]
    fn test_collect_palace_documents_maps_operator_only_visibility() -> TestResult {
        let mut index = KnowledgeIndex::new();
        index.insert(KnowledgeArtifact::new(
            ArtifactId::new("palace/secret"),
            ArtifactKind::PalaceNode,
            frontmatter(VisibilityScope::OperatorOnly),
            "secret body",
        ));

        let documents = collect_palace_documents(&index);
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].visibility, OPERATOR_ONLY_VISIBILITY);
        Ok(())
    }

    #[test]
    fn test_collect_palace_documents_is_sorted_by_artifact_id() -> TestResult {
        let mut index = KnowledgeIndex::new();
        index.insert(KnowledgeArtifact::new(
            ArtifactId::new("palace/zebra"),
            ArtifactKind::PalaceNode,
            frontmatter(VisibilityScope::SelfOnly),
            "z",
        ));
        index.insert(KnowledgeArtifact::new(
            ArtifactId::new("palace/aardvark"),
            ArtifactKind::PalaceNode,
            frontmatter(VisibilityScope::SelfOnly),
            "a",
        ));

        let documents = collect_palace_documents(&index);
        assert_eq!(
            documents
                .iter()
                .map(|d| d.text.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "z"]
        );
        Ok(())
    }

    #[test]
    fn test_collect_diary_documents_filters_to_diary_kind_only() -> TestResult {
        let mut index = KnowledgeIndex::new();
        index.insert(KnowledgeArtifact::new(
            ArtifactId::new("diary/a"),
            ArtifactKind::DiaryEntry,
            frontmatter(VisibilityScope::SelfOnly),
            "diary body",
        ));
        index.insert(KnowledgeArtifact::new(
            ArtifactId::new("palace/b"),
            ArtifactKind::PalaceNode,
            frontmatter(VisibilityScope::SelfOnly),
            "palace body",
        ));

        let documents = collect_diary_documents(&index);
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].text, "diary body");
        assert_eq!(
            documents[0].source,
            SourceRef::Artifact {
                id: "diary/a".to_owned()
            }
        );
        Ok(())
    }

    #[test]
    fn test_collect_diary_documents_maps_operator_only_visibility() -> TestResult {
        let mut index = KnowledgeIndex::new();
        index.insert(KnowledgeArtifact::new(
            ArtifactId::new("diary/secret"),
            ArtifactKind::DiaryEntry,
            frontmatter(VisibilityScope::OperatorOnly),
            "secret entry",
        ));

        let documents = collect_diary_documents(&index);
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].visibility, OPERATOR_ONLY_VISIBILITY);
        Ok(())
    }

    #[test]
    fn test_collect_diary_documents_is_sorted_by_artifact_id() -> TestResult {
        let mut index = KnowledgeIndex::new();
        index.insert(KnowledgeArtifact::new(
            ArtifactId::new("diary/zebra"),
            ArtifactKind::DiaryEntry,
            frontmatter(VisibilityScope::SelfOnly),
            "z",
        ));
        index.insert(KnowledgeArtifact::new(
            ArtifactId::new("diary/aardvark"),
            ArtifactKind::DiaryEntry,
            frontmatter(VisibilityScope::SelfOnly),
            "a",
        ));

        let documents = collect_diary_documents(&index);
        assert_eq!(
            documents
                .iter()
                .map(|d| d.text.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "z"]
        );
        Ok(())
    }

    #[test]
    #[allow(deprecated)]
    fn test_collect_rust_sources_missing_root_returns_empty_list() -> TestResult {
        let root = std::path::Path::new("/does/not/exist/harw-lens-source-rust-test");
        assert_eq!(collect_rust_sources(root)?, Vec::new());
        Ok(())
    }

    #[test]
    #[allow(deprecated)]
    fn test_collect_rust_sources_reads_rust_files_recursively_and_sorted() -> TestResult {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("b.rs"), "fn b() {}\n")?;
        std::fs::create_dir(dir.path().join("nested"))?;
        std::fs::write(dir.path().join("nested/a.rs"), "fn a() {}\n")?;
        std::fs::write(dir.path().join("ignore.md"), "not rust")?;

        let documents = collect_rust_sources(dir.path())?;
        assert_eq!(documents.len(), 2);
        assert_eq!(
            documents[0].source,
            SourceRef::File {
                path: "b.rs".to_owned()
            }
        );
        assert_eq!(
            documents[1].source,
            SourceRef::File {
                path: "nested/a.rs".to_owned()
            }
        );
        assert_eq!(documents[0].visibility, DEFAULT_VISIBILITY);
        Ok(())
    }

    #[test]
    #[allow(deprecated)]
    fn test_collect_rust_sources_skips_target_directory() -> TestResult {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("lib.rs"), "fn lib() {}\n")?;
        std::fs::create_dir(dir.path().join("target"))?;
        std::fs::write(dir.path().join("target/generated.rs"), "fn gen() {}\n")?;

        let documents = collect_rust_sources(dir.path())?;
        assert_eq!(documents.len(), 1);
        assert_eq!(
            documents[0].source,
            SourceRef::File {
                path: "lib.rs".to_owned()
            }
        );
        Ok(())
    }

    #[test]
    #[allow(deprecated)]
    fn test_collect_rust_sources_stays_rust_only() -> TestResult {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("lib.rs"), "fn lib() {}\n")?;
        std::fs::write(dir.path().join("app.ts"), "export const x = 1;\n")?;

        let documents = collect_rust_sources(dir.path())?;
        assert_eq!(documents.len(), 1);
        assert_eq!(
            documents[0].source,
            SourceRef::File {
                path: "lib.rs".to_owned()
            }
        );
        Ok(())
    }

    #[test]
    fn test_collect_code_sources_missing_root_returns_empty_list() -> TestResult {
        let root = std::path::Path::new("/does/not/exist/harw-lens-source-code-test");
        assert_eq!(collect_code_sources(root)?, Vec::new());
        Ok(())
    }

    #[test]
    fn test_collect_code_sources_reads_common_extensions_sorted() -> TestResult {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("main.go"), "package main\n")?;
        std::fs::write(dir.path().join("lib.rs"), "fn lib() {}\n")?;
        std::fs::create_dir(dir.path().join("web"))?;
        std::fs::write(dir.path().join("web/app.tsx"), "export {};\n")?;
        std::fs::write(dir.path().join("web/Util.PY"), "x = 1\n")?;
        std::fs::write(dir.path().join("config.yaml"), "a: 1\n")?;
        std::fs::write(dir.path().join("README.md"), "# Prosa\n")?;
        std::fs::write(dir.path().join("image.png"), "not text")?;
        std::fs::write(dir.path().join("Makefile"), "all:\n")?;

        let documents = collect_code_sources(dir.path())?;
        let paths: Vec<&str> = documents
            .iter()
            .filter_map(|d| match &d.source {
                SourceRef::File { path } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            paths,
            vec![
                "config.yaml",
                "lib.rs",
                "main.go",
                "web/Util.PY",
                "web/app.tsx"
            ]
        );
        assert!(documents.iter().all(|d| d.visibility == DEFAULT_VISIBILITY));
        Ok(())
    }

    #[test]
    fn test_collect_code_sources_skips_build_and_dependency_directories() -> TestResult {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("index.ts"), "export {};\n")?;
        for skipped in SKIPPED_SOURCE_DIRS {
            let sub = dir.path().join(skipped);
            std::fs::create_dir(&sub)?;
            std::fs::write(sub.join("hidden.js"), "var x;\n")?;
        }
        std::fs::create_dir(dir.path().join("src"))?;
        std::fs::write(dir.path().join("src/kept.js"), "var y;\n")?;

        let documents = collect_code_sources(dir.path())?;
        let paths: Vec<&str> = documents
            .iter()
            .filter_map(|d| match &d.source {
                SourceRef::File { path } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(paths, vec!["index.ts", "src/kept.js"]);
        Ok(())
    }

    #[test]
    fn test_collect_code_sources_skips_invalid_utf8_and_oversized_files() -> TestResult {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("ok.c"), "int main(void) { return 0; }\n")?;
        std::fs::write(dir.path().join("latin1.c"), b"//\xe4\n")?;
        assert_eq!(CODE_SOURCE_MAX_BYTES, 1024 * 1024);
        std::fs::write(dir.path().join("bundle.js"), "a".repeat(1024 * 1024 + 1))?;

        let documents = collect_code_sources(dir.path())?;
        assert_eq!(documents.len(), 1);
        assert_eq!(
            documents[0].source,
            SourceRef::File {
                path: "ok.c".to_owned()
            }
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_collect_code_sources_does_not_follow_symlinks() -> TestResult {
        let dir = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        std::fs::write(outside.path().join("secret.py"), "TOKEN = 1\n")?;
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linked"))?;
        std::fs::write(dir.path().join("real.py"), "x = 1\n")?;

        let documents = collect_code_sources(dir.path())?;
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].text, "x = 1\n");
        Ok(())
    }

    #[test]
    fn test_chunker_for_path_maps_extensions() {
        use std::path::Path;
        assert_eq!(chunker_for_path(Path::new("src/lib.rs")), CodeChunker::Rust);
        assert_eq!(chunker_for_path(Path::new("MOD.RS")), CodeChunker::Rust);
        assert_eq!(
            chunker_for_path(Path::new("docs/a.md")),
            CodeChunker::Markdown
        );
        for plain in [
            "a.ts", "b.py", "c.go", "d.java", "e.toml", "f.yaml", "Makefile",
        ] {
            assert_eq!(
                chunker_for_path(Path::new(plain)),
                CodeChunker::Plain,
                "{plain}"
            );
        }
    }
}
