//! `harw-explorer` — universeller Explorer über einen beliebigen Verzeichnisbaum.
//!
//! Anders als `harw-code-graph` (reine Cargo-Workspaces) ist der Explorer an
//! **keine** Projektart gebunden: er läuft ab einer Wurzel durch den gesamten
//! Baum (unter Beachtung von `.gitignore`, abschaltbar), klassifiziert jede
//! Datei ([`filetype`]), erkennt darin beliebig verschachtelte Projekte
//! ([`projects`]: Cargo, npm/pnpm, Python, Go, Git, Dokumentsammlungen) und die
//! Relationen, die er zwischen ihnen findet ([`relations`]: Workspace-Mitglied,
//! Pfad-/Crate-Abhängigkeit, verschachteltes Projekt, Dokument-Link).
//!
//! Einstieg ist [`ExplorerIndex::build`] (in [`index`]).

pub mod filetype;
pub mod index;
pub mod projects;
pub mod relations;
pub mod walk;

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Art eines Eintrags im Baum.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileKind {
    Dir,
    /// Quelltext; `lang` ist ein kurzer, kleingeschriebener Sprachname
    /// (`rust`, `typescript`, `python`, …).
    Source { lang: String },
    Markdown,
    Pdf,
    Image,
    Archive,
    /// Konfiguration/Manifeste (`toml`, `yaml`, `json`, `ini`, Lockfiles …).
    Config,
    /// Strukturierte Daten (`csv`, `parquet`, `sqlite` …).
    Data,
    /// Sonstiger Klartext.
    Text,
    /// Nicht-textuelle Binärdatei.
    Binary,
}

impl FileKind {
    /// Kurzes Anzeige-Label (`rust`, `pdf`, `dir`, …).
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Dir => "dir",
            Self::Source { lang } => lang,
            Self::Markdown => "markdown",
            Self::Pdf => "pdf",
            Self::Image => "image",
            Self::Archive => "archive",
            Self::Config => "config",
            Self::Data => "data",
            Self::Text => "text",
            Self::Binary => "binary",
        }
    }
}

/// Ein Eintrag im Baum. `path` ist **relativ** zur Explorer-Wurzel und nutzt
/// `/` als Trenner-unabhängige `PathBuf`-Komponenten; die Wurzel selbst ist
/// nicht enthalten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub path: PathBuf,
    pub kind: FileKind,
    /// Größe in Bytes (0 für Verzeichnisse).
    pub size: u64,
    /// Tiefe relativ zur Wurzel (direkte Kinder = 1).
    pub depth: u16,
    /// `true`, wenn der Eintrag nur durch `include_ignored` sichtbar ist.
    pub ignored: bool,
}

/// Erkannte Projektart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectKind {
    /// `Cargo.toml` mit `[workspace]`.
    CargoWorkspace,
    /// `Cargo.toml` mit `[package]` (Mitglied oder eigenständig).
    CargoCrate,
    /// `package.json` (npm/yarn/pnpm, inkl. Workspaces).
    Node,
    /// `pyproject.toml`, `setup.py` oder `setup.cfg`.
    Python,
    /// `go.mod`.
    Go,
    /// Ein Git-Repository (`.git` vorhanden).
    Git,
    /// Ordner, der überwiegend aus Dokumenten (PDF/Markdown) besteht.
    Documents,
}

impl ProjectKind {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::CargoWorkspace => "cargo-workspace",
            Self::CargoCrate => "cargo-crate",
            Self::Node => "node",
            Self::Python => "python",
            Self::Go => "go",
            Self::Git => "git",
            Self::Documents => "documents",
        }
    }
}

/// Ein erkanntes Projekt. `root` ist relativ zur Explorer-Wurzel (leer =
/// die Wurzel selbst).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub root: PathBuf,
    pub kind: ProjectKind,
    /// Name aus dem Manifest, sonst der Ordnername.
    pub name: String,
    /// Deklarierte Mitglieder (Cargo-/npm-Workspaces), relativ zur
    /// Explorer-Wurzel, bereits gegen existierende Verzeichnisse aufgelöst.
    pub members: Vec<PathBuf>,
    /// Manifest-Datei relativ zur Explorer-Wurzel, falls vorhanden.
    pub manifest: Option<PathBuf>,
}

/// Art einer gefundenen Beziehung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    /// Workspace → Mitglied.
    WorkspaceMember,
    /// Projekt → Projekt über eine Pfad-Abhängigkeit.
    PathDependency,
    /// Projekt → Projekt über eine namentliche Abhängigkeit, die im Baum
    /// als Projekt existiert.
    CrateDependency,
    /// Äußeres → inneres Projekt (Verschachtelung ohne Mitgliedschaft).
    NestedProject,
    /// Markdown-Dokument → lokales Ziel (Datei/Ordner) über einen Link.
    DocLink,
}

impl RelationKind {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::WorkspaceMember => "member",
            Self::PathDependency => "path-dep",
            Self::CrateDependency => "dep",
            Self::NestedProject => "nested",
            Self::DocLink => "link",
        }
    }
}

/// Gerichtete Beziehung zwischen zwei Pfaden (relativ zur Wurzel).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Relation {
    pub from: PathBuf,
    pub to: PathBuf,
    pub kind: RelationKind,
    /// Optionaler Kontext (Abhängigkeitsname, Linktext …).
    pub label: Option<String>,
}

/// Steuerung eines Durchlaufs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplorerOptions {
    /// Auch von `.gitignore`/`.ignore`/versteckt ausgeschlossene Einträge
    /// aufnehmen (markiert als [`Node::ignored`]). `.git/` selbst wird nie
    /// betreten.
    pub include_ignored: bool,
    /// Harte Obergrenze an Einträgen; darüber wird abgebrochen und
    /// [`ExplorerIndex::truncated`] gesetzt.
    pub max_nodes: usize,
    /// Maximale Tiefe (direkte Kinder der Wurzel = 1).
    pub max_depth: usize,
}

impl Default for ExplorerOptions {
    fn default() -> Self {
        Self {
            include_ignored: false,
            max_nodes: 200_000,
            max_depth: 64,
        }
    }
}

/// Vollständiger Index eines Baums.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplorerIndex {
    /// Absolute Wurzel.
    pub root: PathBuf,
    /// Alle Einträge, sortiert nach Pfad (Verzeichnisse vor ihrem Inhalt).
    pub nodes: Vec<Node>,
    pub projects: Vec<Project>,
    pub relations: Vec<Relation>,
    /// `true`, wenn [`ExplorerOptions::max_nodes`] erreicht wurde.
    pub truncated: bool,
}

/// Fehler des Explorers.
#[derive(Debug)]
pub enum ExplorerError {
    /// Die Wurzel existiert nicht oder ist kein Verzeichnis.
    InvalidRoot(PathBuf),
    /// E/A-Fehler beim Lesen.
    Io(std::io::Error),
    /// Ein Pfad liegt außerhalb der Wurzel.
    OutsideRoot(PathBuf),
}

impl fmt::Display for ExplorerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRoot(path) => write!(f, "explorer root is not a directory: {}", path.display()),
            Self::Io(error) => write!(f, "explorer i/o error: {error}"),
            Self::OutsideRoot(path) => write!(f, "path is outside the explorer root: {}", path.display()),
        }
    }
}

impl std::error::Error for ExplorerError {}

impl From<std::io::Error> for ExplorerError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub type ExplorerResult<T> = Result<T, ExplorerError>;
