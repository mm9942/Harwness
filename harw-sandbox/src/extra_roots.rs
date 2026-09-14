//! Zusätzliche Workspace-Wurzeln für `/add-workdir` (`ExtraRootsCell`).
//!
//! # Herkunft
//! Slice A8 des Scope-Contracts
//! (`~/.claude/workspace/coding/planning/harw-scopes-contract.md`, Zeile 86)
//! und Schritt 6 des TUI-Plans
//! (`~/.claude/plans/nope-permissions-gibt-es-wild-lobster.md`).
//!
//! # Verantwortung
//! Dieses Modul hält den Sitzungszustand einer zusätzlich freigegebenen Menge
//! von Verzeichniswurzeln ([`ExtraRootsCell`]) und die Validierung, die einen
//! Kandidaten dafür qualifiziert ([`validate_extra_root`]). Es erweitert
//! niemals implizit die Rechte einer Sandbox: [`ExtraRootsCell`] wird
//! ausschließlich über [`crate::SandboxSpec::with_extra_roots`] an eine
//! konkrete Spezifikation gebunden, und Kind-Sandboxen erben sie nicht
//! automatisch (siehe dortige Dokumentation). Die eigentliche
//! Containment-Prüfung gegen primäre und zusätzliche Wurzeln übernehmen
//! [`crate::WorkspaceBinding::resolve_existing_with_extra_roots`] und
//! [`crate::WorkspaceBinding::resolve_for_create_with_extra_roots`]; das
//! bwrap-Binden übernimmt `bwrap.rs`.
//!
//! # Exportierte Typen
//! [`ExtraRoot`], [`ExtraRootsCell`], [`ExtraRootError`], [`validate_extra_root`],
//! [`MAX_EXTRA_ROOTS`].
//!
//! # Nebenläufigkeit
//! [`ExtraRootsCell`] ist ein `Arc<RwLock<Vec<ExtraRoot>>>` und beliebig
//! klonbar; jeder Klon teilt denselben Zustand (`Send + Sync`). Ein
//! vergifteter Lock (nach einem Panic während des Haltens) wird
//! fail-closed behandelt: Lesungen liefern dann so, als sei die Liste leer
//! (`snapshot`, `contains_path`, `is_subset_of`), Schreibungen
//! (`add`) liefern [`ExtraRootError::Poisoned`]; `remove` liefert `false`.
//!
//! # Fehler
//! [`ExtraRootError`] — Ein- und Ausgabefehler beim Kanonisieren, sowie jede
//! abgelehnte Validierungsregel.
//!
//! # Examples
//! ```rust,no_run
//! use std::path::Path;
//! use harw_sandbox::{ExtraRootsCell, validate_extra_root};
//!
//! let cell = ExtraRootsCell::new();
//! let primary = Path::new("/home/user/projects/harwness");
//! let candidate = Path::new("/home/user/projects/apicon");
//! match validate_extra_root(candidate, primary, None) {
//!     Ok(_) => {
//!         let added = cell
//!             .add(candidate, false, primary, None)
//!             .expect("Validierung wurde bereits oben geprüft");
//!         assert!(added || !added);
//!     }
//!     Err(error) => eprintln!("abgelehnt: {error}"),
//! }
//! ```

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Maximale Anzahl gleichzeitig registrierter Extra-Roots pro Sitzung.
pub const MAX_EXTRA_ROOTS: usize = 8;

/// Eine zusätzliche, bereits validierte und kanonisierte Workspace-Wurzel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraRoot {
    /// Kanonischer Pfad (aufgelöst über [`Path::canonicalize`], symlink-frei).
    pub path: PathBuf,
    /// `true`, wenn die Wurzel zusätzlich dauerhaft gemerkt wurde (Schritt 6,
    /// „Für dieses Projekt merken“ → `[[workspace.extra_roots]]` in
    /// `.harw/config.toml`). Rein informativ für die Anzeige; die Persistenz
    /// selbst übernehmen `harw-ops`/`harw-config`, nicht dieses Modul.
    pub persisted: bool,
}

/// Geteilter, sitzungsweiter Satz zusätzlicher Workspace-Wurzeln.
///
/// # Beschreibung
/// Reine Sitzungszelle im Stil von `ApprovalModeCell`/`AllowRuleSet`: sie
/// trägt keine eigene Autorität, sondern nur den aktuell freigegebenen
/// Zustand. Ob eine gebundene [`crate::SandboxSpec`] diese Zelle überhaupt
/// berücksichtigt, entscheidet ausschließlich
/// [`crate::SandboxSpec::with_extra_roots`].
#[derive(Debug, Clone)]
pub struct ExtraRootsCell(Arc<RwLock<Vec<ExtraRoot>>>);

impl Default for ExtraRootsCell {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for ExtraRootsCell {
    /// Vergleicht Momentaufnahmen statt Zeigeridentität, damit
    /// `SandboxSpec: PartialEq` unabhängig davon gilt, ob zwei Zellen
    /// dieselbe Instanz oder nur denselben Inhalt teilen.
    fn eq(&self, other: &Self) -> bool {
        self.snapshot() == other.snapshot()
    }
}

impl Eq for ExtraRootsCell {}

impl ExtraRootsCell {
    /// Erstellt eine leere, unabhängige Zelle.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(Vec::new())))
    }

    /// Momentaufnahme aller aktuell registrierten Extra-Roots.
    ///
    /// # Returns
    /// Eine Kopie der aktuellen Liste. Ein vergifteter Lock liefert `[]`
    /// (fail-closed: eine Sitzung mit fragwürdigem Zustand soll eher zu
    /// wenig als zu viel Zugriff berichten).
    #[must_use]
    pub fn snapshot(&self) -> Vec<ExtraRoot> {
        self.0.read().map(|guard| guard.clone()).unwrap_or_default()
    }

    /// Prüft, ob der bereits kanonische Pfad `p` unter einer registrierten
    /// Extra-Root liegt (gleich oder darunter).
    ///
    /// # Arguments
    /// - `p` (`&Path`): ein bereits kanonisierter Pfad; diese Methode
    ///   kanonisiert selbst nicht erneut.
    ///
    /// # Returns
    /// `true`, wenn eine passende Wurzel existiert. Ein vergifteter Lock
    /// liefert `false` (fail-closed).
    #[must_use]
    pub fn contains_path(&self, p: &Path) -> bool {
        match self.0.read() {
            Ok(guard) => guard.iter().any(|root| p.starts_with(&root.path)),
            Err(_) => false,
        }
    }

    /// Prüft, ob jede eigene Extra-Root unter einer Extra-Root von `parent`
    /// liegt. Dient [`crate::SandboxSpec::ensure_child_of`] als dritte,
    /// Extra-Root-spezifische Verengungsbedingung.
    ///
    /// # Returns
    /// `true`, wenn diese Zelle leer ist oder jede ihrer Wurzeln von
    /// mindestens einer Wurzel aus `parent` abgedeckt wird. Ein vergifteter
    /// Lock auf einer der beiden Seiten führt fail-closed zu `false`, sobald
    /// diese Zelle nicht leer ist.
    #[must_use]
    pub fn is_subset_of(&self, parent: &Self) -> bool {
        let mine = self.snapshot();
        if mine.is_empty() {
            return true;
        }
        let theirs = parent.snapshot();
        mine.iter()
            .all(|root| theirs.iter().any(|candidate| root.path.starts_with(&candidate.path)))
    }

    /// Validiert und registriert `path` als zusätzliche Wurzel.
    ///
    /// # Beschreibung
    /// Kanonisiert und validiert `path` über [`validate_extra_root`] und
    /// fügt das Ergebnis der Liste hinzu. Liegt der kanonische Pfad bereits
    /// unter einer registrierten Wurzel (identisch oder darunter), wird
    /// nichts geändert außer optional `persisted` zu setzen, und die Methode
    /// liefert `Ok(false)` (Dedupe statt Fehler). Diese Methode erweitert nie
    /// implizit eine andere Sandbox: der Aufrufer muss die Zelle ausdrücklich
    /// über [`crate::SandboxSpec::with_extra_roots`] binden.
    ///
    /// # Arguments
    /// - `path` (`&Path`): Kandidat, muss nicht bereits kanonisch sein.
    /// - `persisted` (`bool`): ob die Wurzel zusätzlich dauerhaft gemerkt wird.
    /// - `primary_root` (`&Path`): kanonische primäre Workspace-Wurzel.
    /// - `user_home` (`Option<&Path>`): Home-Verzeichnis des Nutzers, falls
    ///   bekannt.
    ///
    /// # Returns
    /// `Ok(true)`, wenn die Wurzel neu hinzugefügt wurde; `Ok(false)` bei
    /// Dedupe.
    ///
    /// # Errors
    /// Siehe [`ExtraRootError`], insbesondere [`ExtraRootError::TooMany`] und
    /// [`ExtraRootError::Poisoned`] bei vergiftetem Lock.
    pub fn add(
        &self,
        path: &Path,
        persisted: bool,
        primary_root: &Path,
        user_home: Option<&Path>,
    ) -> Result<bool, ExtraRootError> {
        let canonical = validate_extra_root(path, primary_root, user_home)?;
        let mut guard = self.0.write().map_err(|_| ExtraRootError::Poisoned)?;

        if let Some(existing) = guard
            .iter_mut()
            .find(|root| canonical == root.path || canonical.starts_with(&root.path))
        {
            existing.persisted = existing.persisted || persisted;
            return Ok(false);
        }

        if guard.len() >= MAX_EXTRA_ROOTS {
            return Err(ExtraRootError::TooMany {
                max: MAX_EXTRA_ROOTS,
            });
        }

        guard.push(ExtraRoot {
            path: canonical,
            persisted,
        });
        Ok(true)
    }

    /// Entfernt eine registrierte Wurzel.
    ///
    /// # Arguments
    /// - `path` (`&Path`): muss dem in [`ExtraRoot::path`] gespeicherten
    ///   kanonischen Pfad entsprechen (z. B. aus [`Self::snapshot`]).
    ///
    /// # Returns
    /// `true`, wenn eine Wurzel entfernt wurde. Ein vergifteter Lock liefert
    /// `false` (fail-closed: kein stiller Erfolg über einen zweifelhaften
    /// Zustand).
    pub fn remove(&self, path: &Path) -> bool {
        match self.0.write() {
            Ok(mut guard) => {
                let before = guard.len();
                guard.retain(|root| root.path != path);
                guard.len() != before
            }
            Err(_) => false,
        }
    }
}

/// Validiert einen Extra-Root-Kandidaten, ohne ihn zu registrieren.
///
/// # Beschreibung
/// Kanonisiert `candidate` (muss existieren und ein Verzeichnis sein, auch
/// nach Auflösung eines Symlinks) und prüft ihn gegen vier Regeln:
/// 1. Nicht `/` (das Wurzelverzeichnis selbst).
/// 2. Nicht exakt `user_home` (kanonisch verglichen), falls angegeben.
/// 3. Kein Vorfahre von `primary_root` — das würde die primäre Wurzel
///    stillschweigend erweitern.
/// 4. Nicht bereits innerhalb `primary_root` — das wäre redundant.
///
/// `primary_root` wird selbst ebenfalls kanonisiert, damit ein nicht
/// vorkanonisierter Aufrufer nicht versehentlich eine der obigen Prüfungen
/// umgeht.
///
/// # Arguments
/// - `candidate` (`&Path`): der zu prüfende Pfad.
/// - `primary_root` (`&Path`): die primäre Workspace-Wurzel.
/// - `user_home` (`Option<&Path>`): Home-Verzeichnis des Nutzers, falls
///   bekannt; ein nicht auflösbares Home wird roh verglichen statt die
///   Prüfung ganz zu überspringen.
///
/// # Returns
/// Den kanonischen Pfad, wenn alle Regeln bestehen.
///
/// # Errors
/// - [`ExtraRootError::Io`]: `candidate` oder `primary_root` lässt sich
///   nicht kanonisieren (z. B. existiert nicht).
/// - [`ExtraRootError::NotDirectory`]: `candidate` ist keine Verzeichnis.
/// - [`ExtraRootError::RootDirectory`]: `candidate` kanonisiert zu `/`.
/// - [`ExtraRootError::UserHome`]: `candidate` ist exakt `user_home`.
/// - [`ExtraRootError::AncestorOfPrimary`]: `candidate` liegt oberhalb von
///   `primary_root`.
/// - [`ExtraRootError::AlreadyContained`]: `candidate` liegt bereits
///   innerhalb von `primary_root`.
pub fn validate_extra_root(
    candidate: &Path,
    primary_root: &Path,
    user_home: Option<&Path>,
) -> Result<PathBuf, ExtraRootError> {
    let canonical = candidate.canonicalize().map_err(|error| ExtraRootError::Io {
        path: candidate.to_path_buf(),
        reason: error.to_string(),
    })?;
    if !canonical.is_dir() {
        return Err(ExtraRootError::NotDirectory { path: canonical });
    }
    // Ein kanonischer Pfad ohne Elternverzeichnis ist exakt `/`.
    if canonical.parent().is_none() {
        return Err(ExtraRootError::RootDirectory);
    }

    let canonical_primary = primary_root
        .canonicalize()
        .map_err(|error| ExtraRootError::Io {
            path: primary_root.to_path_buf(),
            reason: error.to_string(),
        })?;

    if let Some(home) = user_home {
        let canonical_home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
        if canonical == canonical_home {
            return Err(ExtraRootError::UserHome { path: canonical });
        }
    }

    if canonical_primary.starts_with(&canonical) && canonical != canonical_primary {
        return Err(ExtraRootError::AncestorOfPrimary { path: canonical });
    }
    if canonical.starts_with(&canonical_primary) {
        return Err(ExtraRootError::AlreadyContained { path: canonical });
    }

    Ok(canonical)
}

/// Fehler bei Validierung oder Verwaltung zusätzlicher Workspace-Wurzeln.
pub enum ExtraRootError {
    /// Kanonisierung ist fehlgeschlagen (z. B. Pfad existiert nicht).
    Io { path: PathBuf, reason: String },
    /// Kandidat existiert, ist aber kein Verzeichnis.
    NotDirectory { path: PathBuf },
    /// Kandidat kanonisiert zum Wurzelverzeichnis `/`.
    RootDirectory,
    /// Kandidat ist exakt das Home-Verzeichnis des Nutzers.
    UserHome { path: PathBuf },
    /// Kandidat liegt oberhalb der primären Workspace-Wurzel und würde deren
    /// Rechte erweitern.
    AncestorOfPrimary { path: PathBuf },
    /// Kandidat liegt bereits innerhalb der primären Workspace-Wurzel.
    AlreadyContained { path: PathBuf },
    /// Die maximale Anzahl `max` gleichzeitiger Extra-Roots ist erreicht.
    TooMany { max: usize },
    /// Der interne Lock der [`ExtraRootsCell`] ist vergiftet (nach einem
    /// Panic während des Haltens); die Schreiboperation wurde fail-closed
    /// abgelehnt.
    Poisoned,
}

impl fmt::Display for ExtraRootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, reason } => write!(
                f,
                "zusätzliche Arbeitsverzeichnis-Wurzel '{}' konnte nicht aufgelöst werden: {reason}",
                path.display()
            ),
            Self::NotDirectory { path } => {
                write!(f, "'{}' ist kein Verzeichnis", path.display())
            }
            Self::RootDirectory => write!(
                f,
                "das Wurzelverzeichnis '/' kann nicht als zusätzliche Arbeitsverzeichnis-Wurzel hinzugefügt werden"
            ),
            Self::UserHome { path } => write!(
                f,
                "'{}' ist das Home-Verzeichnis des Nutzers und kann nicht als zusätzliche Wurzel hinzugefügt werden",
                path.display()
            ),
            Self::AncestorOfPrimary { path } => write!(
                f,
                "'{}' liegt oberhalb der primären Workspace-Wurzel und würde deren Rechte erweitern",
                path.display()
            ),
            Self::AlreadyContained { path } => write!(
                f,
                "'{}' liegt bereits innerhalb der primären Workspace-Wurzel",
                path.display()
            ),
            Self::TooMany { max } => write!(
                f,
                "es sind bereits {max} zusätzliche Arbeitsverzeichnis-Wurzeln registriert (Maximum erreicht)"
            ),
            Self::Poisoned => write!(
                f,
                "interner Zustand der zusätzlichen Arbeitsverzeichnis-Wurzeln ist beschädigt (vergifteter Lock)"
            ),
        }
    }
}

impl fmt::Debug for ExtraRootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for ExtraRootError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("harwness-extra-roots-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn validate_rejects_root_directory() {
        let base = temp_dir("root-dir");
        let primary = base.join("primary");
        fs::create_dir_all(&primary).unwrap();

        let result = validate_extra_root(Path::new("/"), &primary, None);
        assert!(matches!(result, Err(ExtraRootError::RootDirectory)));
    }

    #[test]
    fn validate_rejects_user_home_exactly() {
        let base = temp_dir("home");
        let home = base.join("home");
        let primary = base.join("primary");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&primary).unwrap();

        let result = validate_extra_root(&home, &primary, Some(&home));
        assert!(matches!(result, Err(ExtraRootError::UserHome { .. })));
    }

    #[test]
    fn validate_rejects_ancestor_of_primary_root() {
        let base = temp_dir("ancestor");
        let primary = base.join("project/nested");
        fs::create_dir_all(&primary).unwrap();
        let ancestor = base.join("project");

        let result = validate_extra_root(&ancestor, &primary, None);
        assert!(matches!(result, Err(ExtraRootError::AncestorOfPrimary { .. })));
    }

    #[test]
    fn validate_rejects_nonexistent_path() {
        let base = temp_dir("nonexistent");
        let primary = base.join("primary");
        fs::create_dir_all(&primary).unwrap();
        let missing = base.join("does-not-exist");

        let result = validate_extra_root(&missing, &primary, None);
        assert!(matches!(result, Err(ExtraRootError::Io { .. })));
    }

    #[test]
    fn validate_rejects_regular_file() {
        let base = temp_dir("file");
        let primary = base.join("primary");
        fs::create_dir_all(&primary).unwrap();
        let file = base.join("not-a-dir.txt");
        fs::write(&file, b"x").unwrap();

        let result = validate_extra_root(&file, &primary, None);
        assert!(matches!(result, Err(ExtraRootError::NotDirectory { .. })));
    }

    #[test]
    fn validate_rejects_path_already_inside_primary() {
        let base = temp_dir("contained");
        let primary = base.join("primary");
        let nested = primary.join("nested");
        fs::create_dir_all(&nested).unwrap();

        let result = validate_extra_root(&nested, &primary, None);
        assert!(matches!(result, Err(ExtraRootError::AlreadyContained { .. })));
    }

    #[test]
    fn validate_accepts_disjoint_directory_and_resolves_symlink_target() {
        let base = temp_dir("symlink");
        let primary = base.join("primary");
        let real_target = base.join("real-extra");
        fs::create_dir_all(&primary).unwrap();
        fs::create_dir_all(&real_target).unwrap();
        let link = base.join("extra-link");
        std::os::unix::fs::symlink(&real_target, &link).unwrap();

        let resolved = validate_extra_root(&link, &primary, None).unwrap();
        assert_eq!(resolved, real_target.canonicalize().unwrap());
    }

    #[test]
    fn add_dedupes_identical_root_and_enforces_max() {
        let base = temp_dir("dedupe");
        let primary = base.join("primary");
        fs::create_dir_all(&primary).unwrap();
        let cell = ExtraRootsCell::new();

        let mut last_added: Option<PathBuf> = None;
        for i in 0..MAX_EXTRA_ROOTS {
            let dir = base.join(format!("root-{i}"));
            fs::create_dir_all(&dir).unwrap();
            assert!(
                cell.add(&dir, false, &primary, None).unwrap(),
                "root {i} should be newly added"
            );
            last_added = Some(dir);
        }
        assert_eq!(cell.snapshot().len(), MAX_EXTRA_ROOTS);
        let last_added = last_added.expect("MAX_EXTRA_ROOTS is greater than zero");

        // Duplikat der zuletzt hinzugefügten Wurzel: kein Fehler, kein neuer Eintrag.
        assert!(!cell.add(&last_added, false, &primary, None).unwrap());
        assert_eq!(cell.snapshot().len(), MAX_EXTRA_ROOTS);

        // Die neunte, tatsächlich neue Wurzel überschreitet das Maximum.
        let overflow = base.join("root-overflow");
        fs::create_dir_all(&overflow).unwrap();
        assert!(matches!(
            cell.add(&overflow, false, &primary, None),
            Err(ExtraRootError::TooMany { max }) if max == MAX_EXTRA_ROOTS
        ));
    }

    #[test]
    fn add_dedupes_path_nested_inside_existing_root() {
        let base = temp_dir("nested-dedupe");
        let primary = base.join("primary");
        let extra = base.join("extra");
        fs::create_dir_all(&primary).unwrap();
        fs::create_dir_all(extra.join("child")).unwrap();
        let cell = ExtraRootsCell::new();

        assert!(cell.add(&extra, false, &primary, None).unwrap());
        assert!(!cell.add(&extra.join("child"), false, &primary, None).unwrap());
        assert_eq!(cell.snapshot().len(), 1);
    }

    #[test]
    fn contains_path_and_remove_reflect_current_state() {
        let base = temp_dir("contains");
        let primary = base.join("primary");
        let extra = base.join("extra");
        fs::create_dir_all(&primary).unwrap();
        fs::create_dir_all(extra.join("child")).unwrap();
        let cell = ExtraRootsCell::new();
        cell.add(&extra, false, &primary, None).unwrap();

        let canonical_extra = extra.canonicalize().unwrap();
        assert!(cell.contains_path(&canonical_extra.join("child")));
        assert!(!cell.contains_path(&primary.canonicalize().unwrap()));

        assert!(cell.remove(&canonical_extra));
        assert!(!cell.contains_path(&canonical_extra));
        assert!(!cell.remove(&canonical_extra));
    }

    #[test]
    fn is_subset_of_holds_for_empty_and_covered_roots() {
        let base = temp_dir("subset");
        let primary = base.join("primary");
        let extra = base.join("extra");
        fs::create_dir_all(&primary).unwrap();
        fs::create_dir_all(extra.join("child")).unwrap();

        let empty = ExtraRootsCell::new();
        let parent = ExtraRootsCell::new();
        parent.add(&extra, false, &primary, None).unwrap();
        assert!(empty.is_subset_of(&parent));

        let child = ExtraRootsCell::new();
        child.add(&extra.join("child"), false, &primary, None).unwrap();
        // `child` selbst liegt nach Dedupe nicht in der Zelle (sie enthält
        // stattdessen nur `extra`, wie `parent`): das prüft is_subset_of
        // trotzdem korrekt.
        assert!(child.is_subset_of(&parent));

        let unrelated_dir = base.join("unrelated");
        fs::create_dir_all(&unrelated_dir).unwrap();
        let unrelated = ExtraRootsCell::new();
        unrelated.add(&unrelated_dir, false, &primary, None).unwrap();
        assert!(!unrelated.is_subset_of(&parent));
    }
}
