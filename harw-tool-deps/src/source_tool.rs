//! `deps.source_read`, `deps.source_search`, `deps.source_list` — **Lesezugriff
//! auf Dependency-Quellcode außerhalb des Workspace**.
//!
//! # Verantwortung
//! Dieses Modul ist der sicherheitskritische Teil von `harw-tool-deps`: es ist
//! die einzige Stelle im Crate, die Pfade *außerhalb* der Workspace-Wurzel
//! öffnet. Grundlage ist [`harw_authority::Permission::ReadCargoRegistry`], die
//! genau einen fest verdrahteten, nur lesbaren Pfadbaum benennt:
//! `$CARGO_HOME/registry/src`.
//!
//! # Sicherheitskontrakt
//! 1. **Permission zuerst.** Der von `#[harw_macros::tool]` erzeugte Prolog
//!    prüft `read_cargo_registry`, bevor die vom Modell kontrollierten
//!    Argumente überhaupt deserialisiert werden.
//! 2. **Kein frei wählbarer Pfad.** Ein Aufrufer benennt Crate, Version und
//!    einen *relativen* Pfad; die Wurzel stammt ausschließlich aus
//!    [`RegistrySourceLocator`].
//! 3. **Zweistufiges Containment.** `resolve_contained` weist lexikalisches
//!    `../`-Traversal gegen die Registry-Wurzel ab (ohne Dateisystemzugriff);
//!    anschließend prüft [`RegistryAccess::resolve_checked`] den
//!    **kanonisierten** Pfad gegen das kanonisierte Crate-Verzeichnis, das
//!    seinerseits unter der kanonisierten Registry-Wurzel liegen muss — das ist
//!    der Symlink-Schutz und zugleich die schärfere Crate-Grenze.
//! 4. **Traversierung folgt keinem Symlink.** `deps.source_search` und
//!    `deps.source_list` überspringen Symlinks vollständig, statt sie
//!    aufzulösen.
//! 5. **Harte Limits.** Dateigröße, Trefferzahl, Verzeichnistiefe und
//!    Einträge pro Ebene sind gedeckelt; ein vom Modell übergebenes Limit kann
//!    das harte Maximum nie überschreiten.
//! 6. **Read-only.** Es gibt keinen schreibenden Pfad in diesem Modul.
//! 7. **Exaktheit.** Nicht-UTF-8 wird als [`DepsToolError::NotUtf8`] gemeldet
//!    statt verlustbehaftet dekodiert: zitierter Quellcode muss exakt sein.
//!
//! # Schlüsseltypen
//! - [`RegistryAccess`] — Locator plus Registry-Wurzel, testbar über
//!   [`RegistryAccess::with_home`].
//! - [`SourceMatch`], [`SearchOutcome`], [`SourceEntry`] — Ergebnisformen.
//! - [`DepsSourceReadTool`], [`DepsSourceSearchTool`], [`DepsSourceListTool`] —
//!   die von `#[harw_macros::tool]` erzeugten Executor-Typen.
//!
//! # Fehler
//! Alle fehlbaren Hilfsfunktionen liefern [`DepsToolError`]; die Tool-Funktionen
//! wandeln jeden Fehler in `Ok(ToolOutput::error(...))` (fail closed, kein Panic).
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Send + Sync`; es wird nur gelesen, kein globaler Zustand
//! gehalten. Alle drei Tools sind `parallel_safe`.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_tool_deps::source_tool::{RegistryAccess, read_source_file};
//! use std::path::Path;
//!
//! # fn demo() -> Result<(), harw_tool_deps::DepsToolError> {
//! let access = RegistryAccess::from_env()?;
//! let file = read_source_file(&access, "serde", "1.0.228", Path::new("src/lib.rs"), None)?;
//! println!("{}", file.content);
//! # Ok(())
//! # }
//! ```

use std::collections::VecDeque;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use harw_code_graph::{CodeGraphError, RegistrySourceLocator, find_locked, parse_lockfile};
use harw_tools::{Permission, ToolOutput, ToolsError, executor::ToolExecutionContext};
use serde::{Deserialize, Serialize};

use crate::error::{DepsToolError, DepsToolResult};

/// Standard-Byte-Limit für `deps.source_read` (256 KiB).
pub const DEFAULT_MAX_SOURCE_BYTES: u64 = 256 * 1024;

/// Hartes Byte-Limit für `deps.source_read` (1 MiB). Ein vom Aufrufer
/// übergebenes `max_bytes` wird hierauf gedeckelt und kann es nie überschreiten.
pub const HARD_MAX_SOURCE_BYTES: u64 = 1024 * 1024;

/// Standard-Trefferlimit für `deps.source_search`.
pub const DEFAULT_MAX_MATCHES: usize = 50;

/// Hartes Trefferlimit für `deps.source_search`.
pub const HARD_MAX_MATCHES: usize = 500;

/// Maximale Verzeichnistiefe, die `deps.source_search` unterhalb der
/// Crate-Wurzel absteigt.
pub const MAX_SEARCH_DEPTH: usize = 8;

/// Maximale Anzahl Einträge, die `deps.source_list` pro Ebene liefert.
pub const MAX_LIST_ENTRIES: usize = 200;

/// Maximale Zeichenzahl pro Trefferzeile; längere Zeilen werden gekürzt, damit
/// eine einzelne minifizierte Zeile die Antwort nicht sprengt.
pub const MAX_MATCH_LINE_CHARS: usize = 300;

/// Dateiendungen, die `deps.source_search` durchsucht. Bewusst klein gehalten:
/// die Suche soll Quellcode, Manifeste und Dokumentation abdecken, nicht
/// Testdaten oder Binärartefakte.
pub const SEARCHABLE_EXTENSIONS: &[&str] = &["rs", "toml", "md"];

/// Verzeichnisnamen, die bei der Suche nie betreten werden.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules"];

// ---------------------------------------------------------------------------
// Registry-Zugriff
// ---------------------------------------------------------------------------

/// Bündelt [`RegistrySourceLocator`] und die zugehörige Registry-Wurzel.
///
/// # Description
/// [`RegistrySourceLocator`] löst `<crate>-<version>`-Verzeichnisse auf, legt
/// seine Wurzel aber nicht offen. Für die zweite, kanonisierende
/// Containment-Prüfung (Symlink-Schutz) wird sie hier zusätzlich gehalten.
/// Beide werden aus **derselben** `CARGO_HOME`-Entscheidung abgeleitet, damit
/// lexikalische und kanonische Prüfung nie auf verschiedene Wurzeln zeigen.
///
/// # Concurrency
/// `Send + Sync`; enthält nur unveränderliche Pfade.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_deps::source_tool::RegistryAccess;
/// use std::path::PathBuf;
///
/// let access = RegistryAccess::with_home(PathBuf::from("/tmp/fixture-cargo-home"));
/// assert!(access.src_root().ends_with("registry/src"));
/// ```
#[derive(Debug, Clone)]
pub struct RegistryAccess {
    /// Auflöser für `<crate>-<version>`-Verzeichnisse.
    locator: RegistrySourceLocator,
    /// `<cargo_home>/registry/src` — die einzige erlaubte Wurzel.
    src_root: PathBuf,
}

impl RegistryAccess {
    /// Baut den Zugriff aus der Umgebung (`CARGO_HOME`, sonst `$HOME/.cargo`).
    ///
    /// # Returns
    /// Einen [`RegistryAccess`] auf `$CARGO_HOME/registry/src`.
    ///
    /// # Errors
    /// - [`DepsToolError::CodeGraph`] mit [`CodeGraphError::CargoHomeUnavailable`],
    ///   wenn weder `CARGO_HOME` noch `HOME` gesetzt ist.
    ///
    /// # Concurrency
    /// Liest nur Umgebungsvariablen; sicher aus jedem Thread aufrufbar.
    pub fn from_env() -> DepsToolResult<Self> {
        Ok(Self::with_home(cargo_home_from_env()?))
    }

    /// Baut den Zugriff über ein explizit angegebenes `CARGO_HOME` — der
    /// Einstiegspunkt für Tests mit Fixture-Verzeichnissen.
    ///
    /// # Arguments
    /// - `cargo_home` (`PathBuf`): Wurzel, unterhalb derer `registry/src` liegt.
    ///
    /// # Returns
    /// Einen [`RegistryAccess`] ohne jeden Dateisystemzugriff (das Verzeichnis
    /// muss beim Bau noch nicht existieren).
    #[must_use]
    pub fn with_home(cargo_home: PathBuf) -> Self {
        let src_root = cargo_home.join("registry").join("src");
        Self {
            locator: RegistrySourceLocator::with_home(cargo_home),
            src_root,
        }
    }

    /// Die Registry-Wurzel `<cargo_home>/registry/src`.
    #[must_use]
    pub fn src_root(&self) -> &Path {
        &self.src_root
    }

    /// Der zugrunde liegende Locator (für `available_versions`, `resolve`).
    #[must_use]
    pub fn locator(&self) -> &RegistrySourceLocator {
        &self.locator
    }

    /// Löst `<registry>/<index>/<crate>-<version>/<relative>` auf und prüft
    /// **zweistufig**, dass das Ergebnis die Registry nicht verlässt.
    ///
    /// # Description
    /// Stufe 1 ist [`RegistrySourceLocator::resolve_contained`]: rein
    /// lexikalische Normalisierung gegen die Registry-Wurzel, die
    /// `../`-Traversal abweist, ohne das Dateisystem zu befragen. Stufe 2
    /// kanonisiert Registry-Wurzel, Crate-Verzeichnis und Ziel und verlangt
    /// zusätzlich, dass das Ziel unterhalb des **Crate-Verzeichnisses** bleibt.
    /// Erst dadurch wird ein Symlink erkannt, der aus dem Crate herausführt —
    /// und ein `../..`, das zwar in der Registry, aber in einem *anderen* Crate
    /// landen würde, ebenfalls abgelehnt.
    ///
    /// # Arguments
    /// - `crate_name` (`&str`): Crate-Name wie im Registry-Verzeichnisnamen.
    /// - `version` (`&str`): exakte Version wie im Registry-Verzeichnisnamen.
    /// - `relative` (`&Path`): Pfad relativ zur Crate-Wurzel; `"."` meint die
    ///   Crate-Wurzel selbst.
    ///
    /// # Returns
    /// Den kanonisierten, garantiert innerhalb der Registry liegenden Pfad.
    ///
    /// # Errors
    /// - [`DepsToolError::PathEscapesRegistry`]: lexikalisches Traversal, ein
    ///   Symlink mit Ziel außerhalb der Registry, oder ein Ziel außerhalb des
    ///   angefragten Crate-Verzeichnisses.
    /// - [`DepsToolError::CodeGraph`] mit `RegistrySourceNotFound`: das
    ///   `<crate>-<version>`-Verzeichnis existiert lokal nicht.
    /// - [`DepsToolError::Io`]: der Zielpfad oder die Registry-Wurzel lässt sich
    ///   nicht kanonisieren (z. B. weil er nicht existiert).
    ///
    /// # Concurrency
    /// Rein lesend; sicher aus mehreren Threads.
    pub fn resolve_checked(
        &self,
        crate_name: &str,
        version: &str,
        relative: &Path,
    ) -> DepsToolResult<PathBuf> {
        // Stufe 1: lexikalisches Containment gegen die Registry-Wurzel
        // (kein Dateisystemzugriff, der Zielpfad muss nicht existieren).
        let lexical = self
            .locator
            .resolve_contained(crate_name, version, relative)
            .map_err(|error| match error {
                CodeGraphError::InvalidPath { path } => DepsToolError::PathEscapesRegistry { path },
                other => DepsToolError::CodeGraph(other),
            })?;

        // Stufe 2a: das Crate-Verzeichnis selbst muss nach der Kanonisierung
        // noch in der Registry liegen — sonst ist bereits die Wurzel verbogen.
        let canonical_root = self.src_root.canonicalize()?;
        let canonical_crate = self.locator.resolve(crate_name, version)?.canonicalize()?;
        if !canonical_crate.starts_with(&canonical_root) {
            return Err(DepsToolError::PathEscapesRegistry {
                path: canonical_crate.display().to_string(),
            });
        }

        // Stufe 2b: das Ziel muss unterhalb des Crate-Verzeichnisses bleiben.
        // Das ist der Symlink-Schutz und zugleich die schärfere Grenze: ein
        // '../..' bliebe in der Registry, verließe aber das angefragte Crate.
        let canonical = lexical.canonicalize()?;
        if !canonical.starts_with(&canonical_crate) {
            return Err(DepsToolError::PathEscapesRegistry {
                path: canonical.display().to_string(),
            });
        }
        Ok(canonical)
    }
}

/// Ermittelt `CARGO_HOME` mit derselben Regel wie
/// [`RegistrySourceLocator::from_env`], damit beide Prüfstufen dieselbe Wurzel
/// sehen.
fn cargo_home_from_env() -> DepsToolResult<PathBuf> {
    if let Ok(cargo_home) = env::var("CARGO_HOME") {
        return Ok(PathBuf::from(cargo_home));
    }
    let home = env::var("HOME").map_err(|_| CodeGraphError::CargoHomeUnavailable)?;
    Ok(PathBuf::from(home).join(".cargo"))
}

/// Bestimmt die zu verwendende Crate-Version.
///
/// # Description
/// Eine explizit übergebene Version gewinnt immer. Sonst wird die Version aus
/// `Cargo.lock` der Workspace-Wurzel aufgelöst — was einen Workspace-Lesezugriff
/// bedeutet und deshalb zusätzlich [`Permission::ReadWorkspace`] voraussetzt.
/// Fehlt diese Berechtigung, verhält sich das Tool so, als sei das Crate nicht
/// gesperrt: es verlangt eine explizite Version, statt still auf eine geratene
/// zu fallen.
///
/// # Errors
/// - [`DepsToolError::CrateNotLocked`]: kein Lockfile-Eintrag, oder der
///   Lockfile-Blick ist mangels `ReadWorkspace` nicht erlaubt.
/// - [`DepsToolError::CodeGraph`]: das Lockfile ist unlesbar oder kein gültiges TOML.
fn resolve_version(
    context: &ToolExecutionContext,
    crate_name: &str,
    requested: Option<&str>,
) -> DepsToolResult<String> {
    if let Some(version) = requested {
        return Ok(version.to_owned());
    }
    if !context
        .sandbox()
        .permissions()
        .contains(Permission::ReadWorkspace)
    {
        tracing::warn!(
            crate_name,
            "Versionsauflösung übersprungen: ReadWorkspace fehlt, Cargo.lock bleibt ungelesen"
        );
        return Err(DepsToolError::CrateNotLocked {
            crate_name: crate_name.to_owned(),
        });
    }
    let root = context.sandbox().workspace().canonical_root();
    let packages = parse_lockfile(root)?;
    find_locked(&packages, crate_name)
        .map(|package| package.version.to_owned())
        .ok_or_else(|| DepsToolError::CrateNotLocked {
            crate_name: crate_name.to_owned(),
        })
}

// ---------------------------------------------------------------------------
// deps.source_read
// ---------------------------------------------------------------------------

/// Eine gelesene Quelldatei samt Herkunftsangabe.
///
/// # Description
/// Die Herkunft (`crate_name`, `version`) gehört ins Ergebnis, weil die Version
/// bei fehlendem `version`-Argument aus `Cargo.lock` aufgelöst wurde — ein Agent
/// muss belegen können, welche Version er tatsächlich zitiert.
#[derive(Debug, Clone, Serialize)]
pub struct SourceFile {
    /// Name des Crates.
    pub crate_name: String,
    /// Tatsächlich gelesene Version.
    pub version: String,
    /// Pfad relativ zur Crate-Wurzel.
    pub path: String,
    /// Größe der Datei in Bytes.
    pub bytes: u64,
    /// Der exakte UTF-8-Inhalt.
    pub content: String,
}

/// Liest genau **eine** Datei aus dem lokalen Registry-Cache.
///
/// # Description
/// Führt die zweistufige Containment-Prüfung aus, verlangt eine reguläre Datei,
/// prüft die Größe gegen das wirksame Limit und dekodiert strikt als UTF-8.
///
/// # Arguments
/// - `access` (`&RegistryAccess`): Registry-Wurzel und Locator.
/// - `crate_name` (`&str`): Crate-Name.
/// - `version` (`&str`): exakte Version.
/// - `relative` (`&Path`): Pfad relativ zur Crate-Wurzel.
/// - `max_bytes` (`Option<u64>`): Aufruf-Limit; `None` bedeutet
///   [`DEFAULT_MAX_SOURCE_BYTES`]. Der Wert wird stets auf
///   [`HARD_MAX_SOURCE_BYTES`] gedeckelt.
///
/// # Returns
/// Ein [`SourceFile`] mit exaktem Inhalt und Herkunftsangabe.
///
/// # Errors
/// - [`DepsToolError::PathEscapesRegistry`]: Traversal oder Symlink nach außen.
/// - [`DepsToolError::FileTooLarge`]: Datei größer als das wirksame Limit.
/// - [`DepsToolError::NotUtf8`]: Datei ist kein gültiges UTF-8.
/// - [`DepsToolError::Io`]: Pfad existiert nicht oder ist keine reguläre Datei.
/// - [`DepsToolError::CodeGraph`]: `<crate>-<version>` liegt nicht im Cache.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Rein lesend; sicher aus mehreren Threads.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_deps::source_tool::{RegistryAccess, read_source_file};
/// use std::path::{Path, PathBuf};
///
/// # fn demo() -> Result<(), harw_tool_deps::DepsToolError> {
/// let access = RegistryAccess::with_home(PathBuf::from("/tmp/fixture"));
/// let file = read_source_file(&access, "demo", "1.0.0", Path::new("src/lib.rs"), None)?;
/// assert!(!file.content.is_empty());
/// # Ok(())
/// # }
/// ```
pub fn read_source_file(
    access: &RegistryAccess,
    crate_name: &str,
    version: &str,
    relative: &Path,
    max_bytes: Option<u64>,
) -> DepsToolResult<SourceFile> {
    let limit = effective_byte_limit(max_bytes);
    let resolved = access.resolve_checked(crate_name, version, relative)?;

    let metadata = fs::metadata(&resolved)?;
    if !metadata.is_file() {
        return Err(DepsToolError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("'{}' ist keine reguläre Datei", resolved.display()),
        )));
    }
    let size = metadata.len();
    if size > limit {
        return Err(DepsToolError::FileTooLarge {
            path: relative.display().to_string(),
            limit,
        });
    }

    let raw = fs::read(&resolved)?;
    let content = String::from_utf8(raw).map_err(|_| DepsToolError::NotUtf8 {
        path: relative.display().to_string(),
    })?;

    Ok(SourceFile {
        crate_name: crate_name.to_owned(),
        version: version.to_owned(),
        path: relative.display().to_string(),
        bytes: size,
        content,
    })
}

/// Deckelt ein optionales Aufruf-Limit auf [`HARD_MAX_SOURCE_BYTES`].
#[must_use]
pub fn effective_byte_limit(max_bytes: Option<u64>) -> u64 {
    // `clamp(1, HARD_MAX_SOURCE_BYTES)` ist sicher: beide Grenzen sind
    // `const`-Literale (siehe oben), `1 <= HARD_MAX_SOURCE_BYTES` gilt also
    // zur Compile-Zeit unabhängig vom Aufrufer-Argument.
    max_bytes
        .unwrap_or(DEFAULT_MAX_SOURCE_BYTES)
        .clamp(1, HARD_MAX_SOURCE_BYTES)
}

// ---------------------------------------------------------------------------
// deps.source_search
// ---------------------------------------------------------------------------

/// Ein Substring-Treffer in einer Datei des Crate-Verzeichnisses.
#[derive(Debug, Clone, Serialize)]
pub struct SourceMatch {
    /// Pfad relativ zur Crate-Wurzel.
    pub file: String,
    /// Zeilennummer, 1-basiert.
    pub line: u32,
    /// Der (rechts getrimmte, ggf. gekürzte) Zeileninhalt.
    pub text: String,
}

/// Ergebnis einer Suche samt Kappungs- und Auslassungs-Information.
///
/// # Description
/// `truncated` und `skipped_files` sind Teil des Ergebnisses, damit ein Agent
/// nie eine unvollständige Suche für vollständig hält.
#[derive(Debug, Clone, Serialize)]
pub struct SearchOutcome {
    /// Die gefundenen Treffer, in Traversierungsreihenfolge.
    pub matches: Vec<SourceMatch>,
    /// `true`, wenn das Trefferlimit erreicht und die Suche abgebrochen wurde.
    pub truncated: bool,
    /// Anzahl der Dateien, die wegen Größe oder fehlender UTF-8-Dekodierbarkeit
    /// übersprungen wurden.
    pub skipped_files: usize,
}

/// Durchsucht das Crate-Verzeichnis nach einem **Substring** (kein Regex).
///
/// # Description
/// Breitensuche ab der Crate-Wurzel bis [`MAX_SEARCH_DEPTH`]. Es werden nur
/// Dateien mit den Endungen aus [`SEARCHABLE_EXTENSIONS`] gelesen; Symlinks
/// werden **nie** verfolgt, `SKIP_DIRS`-Verzeichnisse nie betreten. Dateien
/// über [`DEFAULT_MAX_SOURCE_BYTES`] und nicht UTF-8-dekodierbare Dateien
/// werden übersprungen und in `skipped_files` gezählt, statt die gesamte Suche
/// scheitern zu lassen.
///
/// Bewusst kein Regex: die Substring-Suche hält die Dependency-Liste des Crates
/// klein und kann nicht in katastrophales Backtracking laufen.
///
/// # Arguments
/// - `access` (`&RegistryAccess`): Registry-Wurzel und Locator.
/// - `crate_name` (`&str`), `version` (`&str`): identifizieren das Crate-Verzeichnis.
/// - `pattern` (`&str`): gesuchter Substring, case-sensitiv.
/// - `max_matches` (`Option<usize>`): Trefferlimit; `None` bedeutet
///   [`DEFAULT_MAX_MATCHES`], gedeckelt auf [`HARD_MAX_MATCHES`].
///
/// # Returns
/// Ein [`SearchOutcome`] mit Treffern, Kappungs-Flag und Skip-Zähler.
///
/// # Errors
/// - [`DepsToolError::PathEscapesRegistry`]: die Crate-Wurzel selbst liegt
///   (nach Kanonisierung) außerhalb der Registry.
/// - [`DepsToolError::CodeGraph`]: `<crate>-<version>` liegt nicht im Cache.
/// - [`DepsToolError::Io`]: die Crate-Wurzel ist nicht lesbar.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Rein lesend; sicher aus mehreren Threads.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_deps::source_tool::{RegistryAccess, search_source};
/// use std::path::PathBuf;
///
/// # fn demo() -> Result<(), harw_tool_deps::DepsToolError> {
/// let access = RegistryAccess::with_home(PathBuf::from("/tmp/fixture"));
/// let found = search_source(&access, "demo", "1.0.0", "pub fn", None)?;
/// println!("{} Treffer", found.matches.len());
/// # Ok(())
/// # }
/// ```
pub fn search_source(
    access: &RegistryAccess,
    crate_name: &str,
    version: &str,
    pattern: &str,
    max_matches: Option<usize>,
) -> DepsToolResult<SearchOutcome> {
    // `clamp(1, HARD_MAX_MATCHES)` ist sicher: beide Grenzen sind `const`-
    // Literale (siehe oben), `1 <= HARD_MAX_MATCHES` gilt also zur
    // Compile-Zeit unabhängig vom Aufrufer-Argument.
    let limit = max_matches
        .unwrap_or(DEFAULT_MAX_MATCHES)
        .clamp(1, HARD_MAX_MATCHES);
    let crate_root = access.resolve_checked(crate_name, version, Path::new("."))?;

    let mut matches: Vec<SourceMatch> = Vec::new();
    let mut skipped_files = 0usize;
    let mut truncated = false;

    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((crate_root.clone(), 0));

    while let Some((dir, depth)) = queue.pop_front() {
        if truncated {
            break;
        }
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            // Ein unlesbares Unterverzeichnis darf die gesamte Suche nicht kippen.
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // Defensiv: nur Pfade unterhalb der kanonisierten Crate-Wurzel.
            if !path.starts_with(&crate_root) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            // Symlinks werden nie verfolgt — weder in Verzeichnisse noch in Dateien.
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                if SKIP_DIRS.iter().any(|skip| *skip == name) || depth + 1 > MAX_SEARCH_DEPTH {
                    continue;
                }
                queue.push_back((path, depth + 1));
                continue;
            }
            if !file_type.is_file() || !is_searchable(&path) {
                continue;
            }
            match read_searchable_file(&path) {
                Some(content) => {
                    let label = path
                        .strip_prefix(&crate_root)
                        .unwrap_or(path.as_path())
                        .display()
                        .to_string();
                    for (index, line) in content.lines().enumerate() {
                        if !line.contains(pattern) {
                            continue;
                        }
                        if matches.len() >= limit {
                            truncated = true;
                            break;
                        }
                        matches.push(SourceMatch {
                            file: label.clone(),
                            line: (index + 1) as u32,
                            text: shorten_line(line),
                        });
                    }
                }
                None => skipped_files += 1,
            }
            if truncated {
                break;
            }
        }
    }

    Ok(SearchOutcome {
        matches,
        truncated,
        skipped_files,
    })
}

/// Ob eine Datei anhand ihrer Endung durchsucht wird.
fn is_searchable(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| SEARCHABLE_EXTENSIONS.contains(&ext))
}

/// Liest eine durchsuchbare Datei; `None` bedeutet "übersprungen" (zu groß,
/// unlesbar oder kein UTF-8) — nie ein Abbruch der gesamten Suche.
fn read_searchable_file(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > DEFAULT_MAX_SOURCE_BYTES {
        return None;
    }
    let raw = fs::read(path).ok()?;
    String::from_utf8(raw).ok()
}

/// Kürzt eine Trefferzeile auf [`MAX_MATCH_LINE_CHARS`] Zeichen (nie mitten in
/// einem Unicode-Zeichen) und entfernt rechtsseitigen Leerraum.
fn shorten_line(line: &str) -> String {
    let trimmed = line.trim_end();
    match trimmed.char_indices().nth(MAX_MATCH_LINE_CHARS) {
        Some((byte_index, _)) => {
            let mut out = String::with_capacity(byte_index + 1);
            out.push_str(&trimmed[..byte_index]);
            out.push('…');
            out
        }
        None => trimmed.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// deps.source_list
// ---------------------------------------------------------------------------

/// Ein Eintrag einer Verzeichnisebene im Crate-Verzeichnis.
#[derive(Debug, Clone, Serialize)]
pub struct SourceEntry {
    /// Dateiname ohne Pfad.
    pub name: String,
    /// `"dir"`, `"file"`, `"symlink"` oder `"other"`. Symlinks werden gemeldet,
    /// aber nie aufgelöst.
    pub kind: &'static str,
    /// Dateigröße in Bytes; nur für reguläre Dateien gesetzt.
    pub size: Option<u64>,
}

/// Die Antwort von `deps.source_list`: eine Verzeichnisebene samt Herkunft.
#[derive(Debug, Clone, Serialize)]
pub struct SourceListing {
    /// Name des Crates.
    pub crate_name: String,
    /// Tatsächlich verwendete Version.
    pub version: String,
    /// Gelistetes Unterverzeichnis relativ zur Crate-Wurzel.
    pub path: String,
    /// `true`, wenn auf [`MAX_LIST_ENTRIES`] gekürzt wurde.
    pub truncated: bool,
    /// Die Einträge dieser Ebene, sortiert nach Name.
    pub entries: Vec<SourceEntry>,
}

/// Listet eine Verzeichnisebene innerhalb des Crate-Verzeichnisses.
///
/// # Description
/// Ermöglicht einem Agenten, im Dependency-Quellbaum zu navigieren, statt
/// Pfade zu raten. Die Liste ist nach Name sortiert und auf
/// [`MAX_LIST_ENTRIES`] Einträge gekappt.
///
/// # Arguments
/// - `access` (`&RegistryAccess`): Registry-Wurzel und Locator.
/// - `crate_name` (`&str`), `version` (`&str`): identifizieren das Crate-Verzeichnis.
/// - `relative` (`&Path`): Unterverzeichnis relativ zur Crate-Wurzel; `"."` für
///   die Wurzel selbst.
///
/// # Returns
/// Ein `(Vec<SourceEntry>, bool)`-Paar: die Einträge und ob gekappt wurde.
///
/// # Errors
/// - [`DepsToolError::PathEscapesRegistry`]: Traversal oder Symlink nach außen.
/// - [`DepsToolError::Io`]: Pfad existiert nicht oder ist kein Verzeichnis.
/// - [`DepsToolError::CodeGraph`]: `<crate>-<version>` liegt nicht im Cache.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Rein lesend; sicher aus mehreren Threads.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_deps::source_tool::{RegistryAccess, list_source};
/// use std::path::{Path, PathBuf};
///
/// # fn demo() -> Result<(), harw_tool_deps::DepsToolError> {
/// let access = RegistryAccess::with_home(PathBuf::from("/tmp/fixture"));
/// let (entries, truncated) = list_source(&access, "demo", "1.0.0", Path::new("src"))?;
/// assert!(!truncated || entries.len() == 200);
/// # Ok(())
/// # }
/// ```
pub fn list_source(
    access: &RegistryAccess,
    crate_name: &str,
    version: &str,
    relative: &Path,
) -> DepsToolResult<(Vec<SourceEntry>, bool)> {
    let resolved = access.resolve_checked(crate_name, version, relative)?;
    let metadata = fs::metadata(&resolved)?;
    if !metadata.is_dir() {
        return Err(DepsToolError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("'{}' ist kein Verzeichnis", relative.display()),
        )));
    }

    let mut entries: Vec<SourceEntry> = Vec::new();
    for entry in fs::read_dir(&resolved)?.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let (kind, size) = if file_type.is_symlink() {
            ("symlink", None)
        } else if file_type.is_dir() {
            ("dir", None)
        } else if file_type.is_file() {
            ("file", entry.metadata().ok().map(|meta| meta.len()))
        } else {
            ("other", None)
        };
        entries.push(SourceEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind,
            size,
        });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));

    let truncated = entries.len() > MAX_LIST_ENTRIES;
    entries.truncate(MAX_LIST_ENTRIES);
    Ok((entries, truncated))
}

// ---------------------------------------------------------------------------
// Tool-Argumente und Tool-Funktionen
// ---------------------------------------------------------------------------

/// Argumente für `deps.source_read`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
pub struct DepsSourceReadArgs {
    /// Name des Crates, z. B. "serde".
    pub crate_name: String,
    /// Exakte Version. Ohne Angabe wird sie aus der Cargo.lock des Workspace aufgelöst.
    pub version: Option<String>,
    /// Pfad relativ zur Crate-Wurzel, z. B. "src/lib.rs". Kein "../" erlaubt.
    pub path: String,
    /// Byte-Limit für diesen Aufruf. Standard 262144, hartes Maximum 1048576.
    pub max_bytes: Option<u64>,
}

/// Argumente für `deps.source_search`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
pub struct DepsSourceSearchArgs {
    /// Name des Crates, z. B. "serde".
    pub crate_name: String,
    /// Exakte Version. Ohne Angabe wird sie aus der Cargo.lock des Workspace aufgelöst.
    pub version: Option<String>,
    /// Gesuchter Substring, case-sensitiv. Kein regulärer Ausdruck.
    pub pattern: String,
    /// Maximale Trefferzahl. Standard 50, hartes Maximum 500.
    pub max_matches: Option<usize>,
}

/// Argumente für `deps.source_list`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
pub struct DepsSourceListArgs {
    /// Name des Crates, z. B. "serde".
    pub crate_name: String,
    /// Exakte Version. Ohne Angabe wird sie aus der Cargo.lock des Workspace aufgelöst.
    pub version: Option<String>,
    /// Unterverzeichnis relativ zur Crate-Wurzel. Standard "." (die Wurzel).
    pub path: Option<String>,
}

/// Liest eine einzelne Datei aus dem lokalen Cargo-Registry-Cache.
///
/// # Description
/// Siehe [`read_source_file`] für die Auflösungs- und Containment-Regeln. Der
/// von `#[harw_macros::tool]` erzeugte Prolog prüft `read_cargo_registry`,
/// bevor die Argumente deserialisiert werden.
///
/// # Errors
/// Liefert nie `Err`; jeder Fehler wird als `Ok(ToolOutput::Error)` gemeldet.
#[harw_macros::tool(
    name = "deps.source_read",
    description = "Liest genau eine Datei aus dem entpackten Quellcode einer Dependency im \
                   lokalen Cargo-Registry-Cache ($CARGO_HOME/registry/src). Belegt, wie eine \
                   Dependency tatsächlich funktioniert, statt sie zu erraten. 'path' ist relativ \
                   zur Crate-Wurzel; '../' und Symlinks aus der Registry heraus werden abgelehnt. \
                   Ohne 'version' wird die im Cargo.lock gesperrte Version verwendet. Der Inhalt \
                   muss gültiges UTF-8 sein und unterliegt einem Byte-Limit (Standard 262144, \
                   hartes Maximum 1048576). Nur lesend.",
    permission = "read_cargo_registry",
    parallel_safe
)]
async fn deps_source_read(
    context: &ToolExecutionContext,
    args: DepsSourceReadArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "deps.source_read";

    let access = match RegistryAccess::from_env() {
        Ok(access) => access,
        Err(error) => return Ok(error_output(TOOL, &error)),
    };
    let version = match resolve_version(context, &args.crate_name, args.version.as_deref()) {
        Ok(version) => version,
        Err(error) => return Ok(error_output(TOOL, &error)),
    };

    tracing::debug!(
        crate_name = args.crate_name.as_str(),
        version = version.as_str(),
        path = args.path.as_str(),
        "deps.source_read"
    );

    match read_source_file(
        &access,
        &args.crate_name,
        &version,
        Path::new(&args.path),
        args.max_bytes,
    ) {
        Ok(file) => Ok(json_output(TOOL, &file)),
        Err(error) => Ok(error_output(TOOL, &error)),
    }
}

/// Durchsucht den Quellcode einer Dependency nach einem Substring.
///
/// # Description
/// Siehe [`search_source`]. Ein leeres Muster wird abgelehnt, weil es jede
/// Zeile treffen und damit nur das Trefferlimit ausschöpfen würde.
///
/// # Errors
/// Liefert nie `Err`; jeder Fehler wird als `Ok(ToolOutput::Error)` gemeldet.
#[harw_macros::tool(
    name = "deps.source_search",
    description = "Durchsucht den entpackten Quellcode einer Dependency im lokalen \
                   Cargo-Registry-Cache nach einem Substring (kein Regex, case-sensitiv). \
                   Liefert Zeilen im Format 'datei:zeile: inhalt'. Durchsucht nur .rs-, .toml- \
                   und .md-Dateien bis zu einer Verzeichnistiefe von 8; Symlinks werden nie \
                   verfolgt. Standard-Trefferlimit 50, hartes Maximum 500. Nur lesend.",
    permission = "read_cargo_registry",
    parallel_safe
)]
async fn deps_source_search(
    context: &ToolExecutionContext,
    args: DepsSourceSearchArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "deps.source_search";

    if args.pattern.is_empty() {
        return Ok(ToolOutput::error(format!(
            "{TOOL}: 'pattern' darf nicht leer sein — ein leeres Muster trifft jede Zeile"
        )));
    }

    let access = match RegistryAccess::from_env() {
        Ok(access) => access,
        Err(error) => return Ok(error_output(TOOL, &error)),
    };
    let version = match resolve_version(context, &args.crate_name, args.version.as_deref()) {
        Ok(version) => version,
        Err(error) => return Ok(error_output(TOOL, &error)),
    };

    let outcome = match search_source(
        &access,
        &args.crate_name,
        &version,
        &args.pattern,
        args.max_matches,
    ) {
        Ok(outcome) => outcome,
        Err(error) => return Ok(error_output(TOOL, &error)),
    };

    tracing::info!(
        crate_name = args.crate_name.as_str(),
        version = version.as_str(),
        matches = outcome.matches.len(),
        truncated = outcome.truncated,
        "deps.source_search"
    );

    Ok(ToolOutput::text(render_search_outcome(
        &args.crate_name,
        &version,
        &args.pattern,
        &outcome,
    )))
}

/// Listet eine Verzeichnisebene im Quellbaum einer Dependency.
///
/// # Errors
/// Liefert nie `Err`; jeder Fehler wird als `Ok(ToolOutput::Error)` gemeldet.
#[harw_macros::tool(
    name = "deps.source_list",
    description = "Listet Dateien und Verzeichnisse einer Ebene im entpackten Quellcode einer \
                   Dependency im lokalen Cargo-Registry-Cache, damit im Quellbaum navigiert \
                   werden kann, ohne Pfade zu raten. 'path' ist relativ zur Crate-Wurzel \
                   (Standard '.'). Liefert {name, kind, size}; 'kind' ist 'dir', 'file', \
                   'symlink' oder 'other'. Symlinks werden gemeldet, aber nie aufgelöst. \
                   Maximal 200 Einträge. Nur lesend.",
    permission = "read_cargo_registry",
    parallel_safe
)]
async fn deps_source_list(
    context: &ToolExecutionContext,
    args: DepsSourceListArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "deps.source_list";

    let access = match RegistryAccess::from_env() {
        Ok(access) => access,
        Err(error) => return Ok(error_output(TOOL, &error)),
    };
    let version = match resolve_version(context, &args.crate_name, args.version.as_deref()) {
        Ok(version) => version,
        Err(error) => return Ok(error_output(TOOL, &error)),
    };
    let relative = args.path.as_deref().unwrap_or(".").to_owned();

    // Bewusst als eigenes `let`: so endet die Ausleihe von `version` vor dem
    // `match`, und die Ok-Arm darf `version` in die Antwort verschieben.
    let listed = list_source(&access, &args.crate_name, &version, Path::new(&relative));

    match listed {
        Ok((entries, truncated)) => {
            let listing = SourceListing {
                crate_name: args.crate_name,
                version,
                path: relative,
                truncated,
                entries,
            };
            Ok(json_output(TOOL, &listing))
        }
        Err(error) => Ok(error_output(TOOL, &error)),
    }
}

/// Rendert ein [`SearchOutcome`] als `datei:zeile: inhalt`-Zeilen mit einer
/// vorangestellten `#`-Kopfzeile für die Herkunft.
fn render_search_outcome(
    crate_name: &str,
    version: &str,
    pattern: &str,
    outcome: &SearchOutcome,
) -> String {
    let mut out = format!(
        "# {crate_name} {version} — {} Treffer für \"{pattern}\"\n",
        outcome.matches.len()
    );
    for hit in &outcome.matches {
        out.push_str(&format!("{}:{}: {}\n", hit.file, hit.line, hit.text));
    }
    if outcome.truncated {
        out.push_str("# Trefferlimit erreicht — Ergebnis unvollständig\n");
    }
    if outcome.skipped_files > 0 {
        out.push_str(&format!(
            "# {} Datei(en) übersprungen (zu groß oder kein UTF-8)\n",
            outcome.skipped_files
        ));
    }
    out
}

/// Serialisiert einen Wert als [`ToolOutput::Json`]; ein Serialisierungsfehler
/// wird zu einer Fehlerausgabe statt zu einem Panic.
pub(crate) fn json_output<T: Serialize>(tool: &str, value: &T) -> ToolOutput {
    match serde_json::to_value(value) {
        Ok(json) => ToolOutput::json(json),
        Err(error) => error_output(tool, &DepsToolError::Json(error)),
    }
}

/// Wandelt einen [`DepsToolError`] in eine Tool-Fehlerausgabe und protokolliert
/// ihn strukturiert (ohne Dateiinhalte).
pub(crate) fn error_output(tool: &str, error: &DepsToolError) -> ToolOutput {
    tracing::warn!(tool, error = %error, "deps-Werkzeug abgebrochen");
    ToolOutput::error(format!("{tool}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx, scratch_dir};

    /// Legt ein Fixture-`CARGO_HOME` mit einem Crate `demo-1.0.0` an.
    fn registry_fixture(label: &str) -> TestResult<(PathBuf, RegistryAccess, PathBuf)> {
        let cargo_home = scratch_dir(label)?;
        let crate_dir = cargo_home
            .join("registry")
            .join("src")
            .join("index.crates.io-testhash")
            .join("demo-1.0.0");
        fs::create_dir_all(crate_dir.join("src")).map_err(ctx("Crate-Fixture anlegen"))?;
        fs::write(
            crate_dir.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"1.0.0\"\n",
        )
        .map_err(ctx("Manifest schreiben"))?;
        fs::write(
            crate_dir.join("src").join("lib.rs"),
            "pub fn parse() -> u8 {\n    7\n}\n// parse helper\n",
        )
        .map_err(ctx("lib.rs schreiben"))?;
        let access = RegistryAccess::with_home(cargo_home.clone());
        Ok((cargo_home, access, crate_dir))
    }

    #[test]
    fn test_read_source_file_returns_exact_content() -> TestResult {
        let (home, access, _) = registry_fixture("read-ok")?;

        let file = read_source_file(&access, "demo", "1.0.0", Path::new("src/lib.rs"), None)
            .map_err(ctx("Datei lesbar"))?;

        assert_eq!(file.crate_name, "demo");
        assert_eq!(file.version, "1.0.0");
        assert!(
            file.content.starts_with("pub fn parse()"),
            "Inhalt muss exakt sein: {:?}",
            file.content
        );
        assert_eq!(file.bytes as usize, file.content.len());

        fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_read_source_file_rejects_parent_traversal() -> TestResult {
        let (home, access, _) = registry_fixture("read-traversal")?;

        let result = read_source_file(
            &access,
            "demo",
            "1.0.0",
            Path::new("../../../../../../etc/passwd"),
            None,
        );

        assert!(
            matches!(result, Err(DepsToolError::PathEscapesRegistry { .. })),
            "'../'-Traversal muss abgelehnt werden, war: {result:?}"
        );

        fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_read_source_file_rejects_crate_boundary_escape() -> TestResult {
        let cargo_home = scratch_dir("read-crate-boundary")?;
        let index_dir = cargo_home
            .join("registry")
            .join("src")
            .join("index.crates.io-testhash");
        fs::create_dir_all(index_dir.join("demo-1.0.0")).map_err(ctx("Crate 1 anlegen"))?;
        fs::create_dir_all(index_dir.join("nachbar-2.0.0")).map_err(ctx("Crate 2 anlegen"))?;
        fs::write(
            index_dir.join("nachbar-2.0.0").join("Cargo.toml"),
            "x = 1\n",
        )
        .map_err(ctx("Nachbar-Manifest schreiben"))?;
        let access = RegistryAccess::with_home(cargo_home.clone());

        // Bleibt innerhalb der Registry-Wurzel, verlässt aber das angefragte Crate.
        let result = read_source_file(
            &access,
            "demo",
            "1.0.0",
            Path::new("../nachbar-2.0.0/Cargo.toml"),
            None,
        );

        assert!(
            matches!(result, Err(DepsToolError::PathEscapesRegistry { .. })),
            "ein Ausflug in ein fremdes Crate muss abgelehnt werden, war: {result:?}"
        );

        fs::remove_dir_all(&cargo_home).ok();
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_read_source_file_rejects_symlink_out_of_registry() -> TestResult {
        let (home, access, crate_dir) = registry_fixture("read-symlink")?;

        // Ziel liegt außerhalb der Registry, aber innerhalb des Scratch-Baums,
        // damit der Test ohne Zugriff auf echte Systemdateien auskommt.
        let outside = home.join("geheim.txt");
        fs::write(&outside, "streng geheim\n").map_err(ctx("Zieldatei anlegen"))?;
        std::os::unix::fs::symlink(&outside, crate_dir.join("src").join("escape.rs"))
            .map_err(ctx("Symlink anlegen"))?;

        let result = read_source_file(&access, "demo", "1.0.0", Path::new("src/escape.rs"), None);

        assert!(
            matches!(result, Err(DepsToolError::PathEscapesRegistry { .. })),
            "Symlink aus der Registry heraus muss abgelehnt werden, war: {result:?}"
        );

        fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_read_source_file_accepts_symlink_inside_registry() -> TestResult {
        let (home, access, crate_dir) = registry_fixture("read-symlink-inside")?;

        std::os::unix::fs::symlink(
            crate_dir.join("src").join("lib.rs"),
            crate_dir.join("src").join("alias.rs"),
        )
        .map_err(ctx("Symlink anlegen"))?;

        let file = read_source_file(&access, "demo", "1.0.0", Path::new("src/alias.rs"), None)
            .map_err(ctx("Symlink innerhalb der Registry bleibt erlaubt"))?;
        assert!(file.content.contains("pub fn parse()"));

        fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_read_source_file_rejects_oversized_file() -> TestResult {
        let (home, access, crate_dir) = registry_fixture("read-too-large")?;
        fs::write(crate_dir.join("src").join("big.rs"), "x".repeat(4096))
            .map_err(ctx("große Datei schreiben"))?;

        let result = read_source_file(
            &access,
            "demo",
            "1.0.0",
            Path::new("src/big.rs"),
            Some(1024),
        );

        match result {
            Err(DepsToolError::FileTooLarge { path, limit }) => {
                assert_eq!(path, "src/big.rs");
                assert_eq!(limit, 1024);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "FileTooLarge erwartet, war: {other:?}"
                )));
            }
        }

        fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_read_source_file_rejects_non_utf8() -> TestResult {
        let (home, access, crate_dir) = registry_fixture("read-not-utf8")?;
        fs::write(crate_dir.join("src").join("blob.rs"), [0xffu8, 0xfe, 0x00])
            .map_err(ctx("Binärdatei schreiben"))?;

        let result = read_source_file(&access, "demo", "1.0.0", Path::new("src/blob.rs"), None);

        assert!(
            matches!(result, Err(DepsToolError::NotUtf8 { .. })),
            "Nicht-UTF-8 darf nicht verlustbehaftet dekodiert werden, war: {result:?}"
        );

        fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_read_source_file_reports_missing_crate() -> TestResult {
        let (home, access, _) = registry_fixture("read-missing-crate")?;

        let result = read_source_file(&access, "nicht-da", "9.9.9", Path::new("src/lib.rs"), None);

        assert!(
            matches!(
                result,
                Err(DepsToolError::CodeGraph(
                    CodeGraphError::RegistrySourceNotFound { .. }
                ))
            ),
            "fehlendes Crate muss als RegistrySourceNotFound gemeldet werden, war: {result:?}"
        );

        fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_effective_byte_limit_caps_at_hard_maximum() {
        assert_eq!(effective_byte_limit(None), DEFAULT_MAX_SOURCE_BYTES);
        assert_eq!(effective_byte_limit(Some(1024)), 1024);
        assert_eq!(
            effective_byte_limit(Some(u64::MAX)),
            HARD_MAX_SOURCE_BYTES,
            "ein vom Modell übergebenes Limit darf das harte Maximum nie überschreiten"
        );
        assert_eq!(effective_byte_limit(Some(0)), 1);
    }

    #[test]
    fn test_search_source_finds_matches_with_file_and_line() -> TestResult {
        let (home, access, _) = registry_fixture("search-hit")?;

        let outcome =
            search_source(&access, "demo", "1.0.0", "parse", None).map_err(ctx("Suche läuft"))?;

        assert!(!outcome.truncated);
        assert_eq!(outcome.matches.len(), 2, "zwei Zeilen enthalten 'parse'");
        assert_eq!(outcome.matches[0].file, "src/lib.rs");
        assert_eq!(outcome.matches[0].line, 1);
        assert_eq!(outcome.matches[1].line, 4);

        fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_search_source_truncates_at_max_matches() -> TestResult {
        let (home, access, crate_dir) = registry_fixture("search-truncate")?;
        let many = (0..50)
            .map(|index| format!("// treffer {index}\n"))
            .collect::<String>();
        fs::write(crate_dir.join("src").join("many.rs"), many).map_err(ctx("Datei schreiben"))?;

        let outcome = search_source(&access, "demo", "1.0.0", "treffer", Some(5))
            .map_err(ctx("Suche läuft"))?;

        assert_eq!(outcome.matches.len(), 5);
        assert!(outcome.truncated, "Kappung muss gemeldet werden");

        fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_search_source_ignores_unsupported_extensions() -> TestResult {
        let (home, access, crate_dir) = registry_fixture("search-extension")?;
        fs::write(crate_dir.join("src").join("data.json"), "einhorn\n")
            .map_err(ctx("JSON-Datei schreiben"))?;

        let outcome =
            search_source(&access, "demo", "1.0.0", "einhorn", None).map_err(ctx("Suche läuft"))?;

        assert!(
            outcome.matches.is_empty(),
            ".json wird nicht durchsucht, war: {:?}",
            outcome.matches
        );

        fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_search_source_never_follows_symlinks() -> TestResult {
        let (home, access, crate_dir) = registry_fixture("search-symlink")?;
        let outside_dir = home.join("draußen");
        fs::create_dir_all(&outside_dir).map_err(ctx("Außenverzeichnis anlegen"))?;
        fs::write(outside_dir.join("secret.rs"), "geheimnis\n")
            .map_err(ctx("Zieldatei anlegen"))?;
        std::os::unix::fs::symlink(&outside_dir, crate_dir.join("verlinkt"))
            .map_err(ctx("Verzeichnis-Symlink anlegen"))?;

        let outcome = search_source(&access, "demo", "1.0.0", "geheimnis", None)
            .map_err(ctx("Suche läuft"))?;

        assert!(
            outcome.matches.is_empty(),
            "Symlinks dürfen nie verfolgt werden, war: {:?}",
            outcome.matches
        );

        fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_search_source_skips_oversized_file_and_counts_it() -> TestResult {
        let (home, access, crate_dir) = registry_fixture("search-oversized")?;
        fs::write(
            crate_dir.join("src").join("huge.rs"),
            "nadel\n".repeat((DEFAULT_MAX_SOURCE_BYTES as usize / 6) + 10),
        )
        .map_err(ctx("große Datei schreiben"))?;

        let outcome =
            search_source(&access, "demo", "1.0.0", "nadel", None).map_err(ctx("Suche läuft"))?;

        assert!(outcome.matches.is_empty());
        assert_eq!(outcome.skipped_files, 1, "Auslassung muss gezählt werden");

        fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_list_source_lists_one_level() -> TestResult {
        let (home, access, _) = registry_fixture("list-root")?;

        let (entries, truncated) =
            list_source(&access, "demo", "1.0.0", Path::new(".")).map_err(ctx("Wurzel listbar"))?;

        assert!(!truncated);
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["Cargo.toml", "src"]);

        let manifest = &entries[0];
        assert_eq!(manifest.kind, "file");
        assert!(manifest.size.is_some_and(|size| size > 0));

        let src = &entries[1];
        assert_eq!(src.kind, "dir");
        assert_eq!(src.size, None);

        fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_list_source_rejects_traversal() -> TestResult {
        let (home, access, _) = registry_fixture("list-traversal")?;

        let result = list_source(&access, "demo", "1.0.0", Path::new("../.."));

        assert!(
            matches!(result, Err(DepsToolError::PathEscapesRegistry { .. })),
            "'../'-Traversal muss abgelehnt werden, war: {result:?}"
        );

        fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_list_source_rejects_file_path() -> TestResult {
        let (home, access, _) = registry_fixture("list-file")?;

        let result = list_source(&access, "demo", "1.0.0", Path::new("Cargo.toml"));

        assert!(
            matches!(result, Err(DepsToolError::Io(_))),
            "eine Datei ist kein listbares Verzeichnis, war: {result:?}"
        );

        fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_shorten_line_truncates_on_char_boundary() {
        let long = "ä".repeat(MAX_MATCH_LINE_CHARS + 20);
        let shortened = shorten_line(&long);
        assert_eq!(shortened.chars().count(), MAX_MATCH_LINE_CHARS + 1);
        assert!(shortened.ends_with('…'));
    }

    #[test]
    fn test_is_searchable_matches_only_allowed_extensions() {
        assert!(is_searchable(Path::new("src/lib.rs")));
        assert!(is_searchable(Path::new("Cargo.toml")));
        assert!(is_searchable(Path::new("README.md")));
        assert!(!is_searchable(Path::new("data.json")));
        assert!(!is_searchable(Path::new("LICENSE")));
    }

    #[test]
    fn test_render_search_outcome_uses_file_line_content_format() {
        let outcome = SearchOutcome {
            matches: vec![SourceMatch {
                file: "src/lib.rs".to_owned(),
                line: 12,
                text: "pub fn parse()".to_owned(),
            }],
            truncated: true,
            skipped_files: 2,
        };

        let rendered = render_search_outcome("demo", "1.0.0", "parse", &outcome);

        assert!(rendered.contains("src/lib.rs:12: pub fn parse()"));
        assert!(rendered.contains("Trefferlimit erreicht"));
        assert!(rendered.contains("2 Datei(en) übersprungen"));
    }

    #[test]
    fn test_with_home_derives_registry_src_root() {
        let access = RegistryAccess::with_home(PathBuf::from("/fixture/cargo"));
        assert_eq!(
            access.src_root(),
            Path::new("/fixture/cargo/registry/src"),
            "die Registry-Wurzel muss deterministisch aus CARGO_HOME folgen"
        );
    }
}
