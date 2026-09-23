//! Runtime-Admission-Gate für Worker-Patches.
//!
//! Verantwortungsbereich: Prüft, ob ein Worker-Patch innerhalb des deklarierten
//! [`MutationContract`] bleibt — d.h. alle geänderten Pfade sind in
//! `allowed_paths` enthalten und kein Pfad matcht `forbidden_paths`.
//!
//! Exportierte Typen: [`RepoRevision`], [`PathRule`], [`MutationContract`],
//! [`PatchFile`], [`FileChange`], [`UnifiedDiff`], [`AdmissionReport`],
//! [`ScopeViolation`], [`ScopeMatcher`].
//!
//! Exportierte Funktionen: [`validate_patch`], [`contract_from_node`].
//!
//! Concurrency: Alle Typen sind `Send + Sync`. Keine Mutex-Nutzung.
//!
//! Fehlertypen: Diese Datei produziert keine `Result`-Fehler; Regelverstöße
//! werden als [`ScopeViolation`]-Einträge im [`AdmissionReport`] kodiert.
//!
//! Spezifikation: Design-Doc §5 (Validation-Kontext), §7 (Mutation Lease);
//! coding-philosophy §8 (WriteSet-Enforcement), §10 (Mutation Lease), §19 (Untrusted Inputs).
//!
//! # Examples
//!
//! ```rust,no_run
//! use harw_plan::admission::{
//!     validate_patch, MutationContract, PathRule, RepoRevision,
//!     UnifiedDiff, PatchFile, FileChange,
//! };
//! use harw_plan::ids::{RevisionId, TaskId};
//!
//! let contract = MutationContract {
//!     base_revision: RepoRevision("abc123".to_owned()),
//!     plan_revision: RevisionId::new(1),
//!     task_id: TaskId::new("t-1"),
//!     allowed_paths: vec![PathRule::Exact("src/lib.rs".to_owned())],
//!     forbidden_paths: vec![],
//! };
//! let diff = UnifiedDiff {
//!     base_revision: RepoRevision("abc123".to_owned()),
//!     files: vec![PatchFile {
//!         path: "src/lib.rs".to_owned(),
//!         source_path: None,
//!         change: FileChange::Modified,
//!     }],
//! };
//! let report = validate_patch(&contract, &diff);
//! assert!(report.admitted);
//! ```

use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────────────
// Grundtypen
// ──────────────────────────────────────────────────────────────────────────────

/// Repository-Revision (Git-SHA oder abstrakter Marker).
///
/// # Description
/// Newtype über `String`. Wird als opakes Token behandelt — kein Parsing,
/// kein Vergleich anhand von Semantik, nur exakter String-Vergleich.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::admission::RepoRevision;
/// let r = RepoRevision("deadbeef".to_owned());
/// assert_eq!(r.0, "deadbeef");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepoRevision(pub String);

// ──────────────────────────────────────────────────────────────────────────────
// PathRule
// ──────────────────────────────────────────────────────────────────────────────

/// Regel für erlaubte oder verbotene Pfade.
///
/// # Description
/// Vier Varianten: exakter Pfad-Match, Verzeichnis-Prefix, Dateiendungs-
/// Suffix und Glob-Muster. Path-Vergleiche operieren auf Repo-relativen,
/// vorwärts-Slash-normalisierten Pfaden (keine `std::path::PathBuf`-Nutzung
/// in Serde-Feldern).
///
/// Gemäß Design-Doc §5 und coding-philosophy §19 (Untrusted Inputs) werden alle
/// eingehenden Pfade ohne Filesystem-Zugriff rein als Strings geprüft.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::admission::PathRule;
/// assert!(PathRule::Exact("src/lib.rs".to_owned()).matches("src/lib.rs"));
/// assert!(PathRule::DirectoryPrefix("src".to_owned()).matches("src/foo.rs"));
/// assert!(PathRule::ExtensionSuffix(".toml".to_owned()).matches("Cargo.toml"));
/// assert!(PathRule::Glob("src/**/*.rs".to_owned()).matches("src/a/b.rs"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PathRule {
    /// Exakter Pfad-Match (relativ zum Repo-Root).
    Exact(String),
    /// Verzeichnis-Prefix — jeder Pfad, der mit `<dir>/` beginnt, matcht.
    DirectoryPrefix(String),
    /// Dateiendungs-Suffix — Match, wenn der Pfad-Basename auf den Suffix endet.
    ExtensionSuffix(String),
    /// Glob-Muster nach der Mini-Syntax aus [`glob_matches`]: `*` (beliebig
    /// viele Zeichen außer `/`), `**` (beliebig viele Segmente inkl. `/`),
    /// `?` (genau ein Zeichen außer `/`), sonst Literale. Kein
    /// Brace-Expansion, keine Zeichenklassen.
    Glob(String),
}

impl PathRule {
    /// Prüft, ob `path` von dieser Regel erfasst wird.
    ///
    /// # Arguments
    /// - `path` (`&str`): Repo-relativer, Slash-normalisierter Pfad.
    ///
    /// # Returns
    /// `true` wenn die Regel auf `path` zutrifft, sonst `false`.
    ///
    /// # Description
    /// Regeln werden ohne Filesystem-Zugriff rein als String-Operationen
    /// ausgewertet (coding-philosophy §19).
    ///
    /// - `Exact(s)`: `path == s`.
    /// - `DirectoryPrefix(dir)`: `path` beginnt mit `dir + "/"`.
    /// - `ExtensionSuffix(ext)`: der Basename-Teil (nach dem letzten `/`)
    ///   endet auf `ext`.
    /// - `Glob(pattern)`: siehe [`glob_matches`].
    pub fn matches(&self, path: &str) -> bool {
        let Some(path) = normalize_repo_path(path) else {
            return false;
        };

        match self {
            PathRule::Exact(expected) => {
                normalize_repo_path(expected).is_some_and(|expected| path == expected)
            }
            PathRule::DirectoryPrefix(dir) => normalize_repo_path(dir).is_some_and(|dir| {
                path.strip_prefix(dir.as_str())
                    .is_some_and(|suffix| suffix.starts_with('/'))
            }),
            PathRule::ExtensionSuffix(ext) => {
                // Basename = Teil nach letztem `/`, oder gesamter String.
                let basename = path.rsplit('/').next().unwrap_or(path.as_str());
                basename.ends_with(ext.as_str())
            }
            PathRule::Glob(pattern) => glob_matches(pattern.as_str(), path.as_str()),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// ScopeMatcher — öffentliche Fassade
// ──────────────────────────────────────────────────────────────────────────────

/// Öffentliche Fassade für Scope- und Pfad-Operationen.
///
/// # Description
/// Bündelt Pfad-Normalisierung, Scope-Konflikt-Erkennung, Containment-Prüfung
/// und Glob-Matching für Orchestratoren, die Write-Scopes prüfen müssen, ohne
/// die crate-internen Helfer (`normalize_repo_path`, `write_scopes_conflict`,
/// `glob_matches`) zu duplizieren. Die Helfer bleiben die Implementierung;
/// `ScopeMatcher` delegiert nur (Design-Doc §5, §7).
///
/// # Concurrency
/// Zustandslos — alle Methoden sind assoziierte Funktionen ohne `&self`.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::admission::ScopeMatcher;
/// assert!(ScopeMatcher::conflicts("src", "src/lib.rs"));
/// assert_eq!(ScopeMatcher::normalize("./src//lib.rs"), Some("src/lib.rs".to_owned()));
/// assert!(ScopeMatcher::matches_glob("src/**/*.rs", "src/a/b.rs"));
/// assert!(ScopeMatcher::contains("src", "src/lib.rs"));
/// ```
pub struct ScopeMatcher;

impl ScopeMatcher {
    /// Prüft, ob zwei Repo-relative Write-Scopes kollidieren (ein Pfad enthält
    /// seine Nachfahren).
    ///
    /// # Arguments
    /// - `left` (`&str`): erster Scope-Pfad.
    /// - `right` (`&str`): zweiter Scope-Pfad.
    ///
    /// # Returns
    /// `true`, wenn beide Pfade gültig normalisierbar sind und identisch sind
    /// oder einer ein Vorfahre des anderen ist; sonst `false`.
    ///
    /// # Concurrency
    /// Rein funktional, keine Seiteneffekte.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_plan::admission::ScopeMatcher;
    /// assert!(ScopeMatcher::conflicts("src", "src/lib.rs"));
    /// assert!(!ScopeMatcher::conflicts("src/a.rs", "src/b.rs"));
    /// ```
    pub fn conflicts(left: &str, right: &str) -> bool {
        write_scopes_conflict(left, right)
    }

    /// Normalisiert einen Repo-relativen Pfad rein lexikalisch.
    ///
    /// # Arguments
    /// - `path` (`&str`): der zu normalisierende Pfad.
    ///
    /// # Returns
    /// `Some(String)` mit dem normalisierten Pfad (führende `./`, doppelte
    /// Slashes entfernt); `None` bei Traversal (`..`), absoluten Pfaden
    /// (Unix oder Windows), Backslashes oder leeren Eingaben.
    ///
    /// # Concurrency
    /// Rein funktional, keine Seiteneffekte.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_plan::admission::ScopeMatcher;
    /// assert_eq!(ScopeMatcher::normalize("./src//lib.rs"), Some("src/lib.rs".to_owned()));
    /// assert_eq!(ScopeMatcher::normalize("../secret"), None);
    /// ```
    pub fn normalize(path: &str) -> Option<String> {
        normalize_repo_path(path)
    }

    /// Glob-Match nach der Mini-Syntax (`*`, `**`, `?`, Literale).
    ///
    /// # Arguments
    /// - `pattern` (`&str`): das Glob-Muster.
    /// - `path` (`&str`): der zu prüfende Pfad.
    ///
    /// # Returns
    /// `true`, wenn `path` (nach Normalisierung beider Seiten) auf `pattern`
    /// passt; `false` bei ungültigen Eingaben oder fehlendem Match.
    ///
    /// # Concurrency
    /// Rein funktional, keine Seiteneffekte.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_plan::admission::ScopeMatcher;
    /// assert!(ScopeMatcher::matches_glob("src/**/*.rs", "src/a/b.rs"));
    /// assert!(!ScopeMatcher::matches_glob("src/**/*.rs", "tests/a.rs"));
    /// ```
    pub fn matches_glob(pattern: &str, path: &str) -> bool {
        glob_matches(pattern, path)
    }

    /// Prüft, ob `path` innerhalb von `scope` liegt (`scope == path` oder
    /// `path` ist ein Nachfahre von `scope`).
    ///
    /// # Arguments
    /// - `scope` (`&str`): der Scope-Pfad (Verzeichnis oder Datei).
    /// - `path` (`&str`): der zu prüfende Pfad.
    ///
    /// # Returns
    /// `true`, wenn beide Pfade gültig normalisierbar sind und `path` mit
    /// `scope` übereinstimmt oder darunter liegt; sonst `false`. Anders als
    /// [`ScopeMatcher::conflicts`] ist dies gerichtet: `contains("src/lib.rs",
    /// "src")` ist `false`.
    ///
    /// # Concurrency
    /// Rein funktional, keine Seiteneffekte.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_plan::admission::ScopeMatcher;
    /// assert!(ScopeMatcher::contains("src", "src/lib.rs"));
    /// assert!(!ScopeMatcher::contains("src/lib.rs", "src"));
    /// ```
    pub fn contains(scope: &str, path: &str) -> bool {
        let (Some(scope), Some(path)) = (normalize_repo_path(scope), normalize_repo_path(path))
        else {
            return false;
        };

        path == scope
            || path
                .strip_prefix(scope.as_str())
                .is_some_and(|suffix| suffix.starts_with('/'))
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// MutationContract
// ──────────────────────────────────────────────────────────────────────────────

/// Kontrakt, der einem Worker übergeben wird und den er einhalten muss.
///
/// # Description
/// Enthält die Basis-Revision, Plan-Revision, Task-ID sowie die Listen
/// erlaubter und verbotener Pfadregeln. Der Kontrakt wird vor Patch-
/// Ausführung an [`validate_patch`] übergeben (Design-Doc §7, §5).
///
/// # Concurrency
/// `Clone + Send + Sync`.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::admission::{MutationContract, PathRule, RepoRevision};
/// use harw_plan::ids::{RevisionId, TaskId};
///
/// let c = MutationContract {
///     base_revision: RepoRevision("sha1".to_owned()),
///     plan_revision: RevisionId::new(0),
///     task_id: TaskId::new("t-1"),
///     allowed_paths: vec![PathRule::DirectoryPrefix("src".to_owned())],
///     forbidden_paths: vec![PathRule::Exact("src/secret.rs".to_owned())],
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MutationContract {
    /// Erwartetete Basis-Revision des Repositorys.
    pub base_revision: RepoRevision,
    /// Revision des zugehörigen Plans.
    pub plan_revision: crate::ids::RevisionId,
    /// ID des ausführenden Tasks.
    pub task_id: crate::ids::TaskId,
    /// Regeln für erlaubte Schreibpfade.
    pub allowed_paths: Vec<PathRule>,
    /// Regeln für explizit verbotene Pfade.
    pub forbidden_paths: Vec<PathRule>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Diff-Typen
// ──────────────────────────────────────────────────────────────────────────────

/// Art der Dateiänderung in einem Patch.
///
/// # Description
/// Vier Varianten entsprechend typischen Git-Diff-Ausgaben.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChange {
    /// Datei neu hinzugefügt.
    Added,
    /// Datei geändert.
    Modified,
    /// Datei gelöscht.
    Deleted,
    /// Datei umbenannt.
    Renamed,
}

/// Eine einzelne geänderte Datei in einem Patch.
///
/// # Description
/// Enthält den Repo-relativen Zielpfad und die Änderungsart. Bei einer
/// Umbenennung trägt `source_path` zusätzlich den bisherigen Pfad, damit die
/// Admission beide mutierten Endpunkte der Umbenennung prüfen kann.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchFile {
    /// Repo-relativer, Slash-normalisierter Pfad.
    pub path: String,
    /// Bisheriger Repo-relativer Pfad einer Umbenennung.
    ///
    /// Nur für [`FileChange::Renamed`] gesetzt. `#[serde(default)]` erhält die
    /// Kompatibilität mit bereits serialisierten Diffs ohne Sourcepfad.
    #[serde(default)]
    pub source_path: Option<String>,
    /// Art der Änderung.
    pub change: FileChange,
}

/// Vereinfachter Diff — enthält Basis-Revision und alle geänderten Dateien.
///
/// # Description
/// Wird von [`validate_patch`] gegen einen [`MutationContract`] geprüft.
/// Felder sind Strings (kein `PathBuf`), damit JSON/TOML sauber bleibt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnifiedDiff {
    /// Basis-Revision, auf der der Patch aufbaut.
    pub base_revision: RepoRevision,
    /// Alle im Patch geänderten Dateien.
    pub files: Vec<PatchFile>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Admission-Ergebnis
// ──────────────────────────────────────────────────────────────────────────────

/// Verstoß gegen den Mutation-Kontrakt.
///
/// # Description
/// Drei Varianten: Datei außerhalb erlaubter Pfade, Datei in verbotenen Pfaden,
/// und Basis-Revisions-Mismatch. Alle Varianten tragen den vollständigen Kontext
/// (coding-philosophy §Error Handling).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScopeViolation {
    /// Datei ist von keiner `allowed_paths`-Regel erfasst.
    OutsideAllowed {
        /// Der nicht abgedeckte Pfad.
        path: String,
    },
    /// Datei matcht eine `forbidden_paths`-Regel.
    Forbidden {
        /// Der verbotene Pfad.
        path: String,
        /// Die matchende Regel.
        rule: PathRule,
    },
    /// Basis-Revision im Diff stimmt nicht mit der Kontrakt-Revision überein.
    BaseRevisionMismatch {
        /// Erwartete Revision aus dem Kontrakt.
        contract: RepoRevision,
        /// Tatsächliche Revision aus dem Patch.
        patch: RepoRevision,
    },
}

/// Ergebnis der Admission-Prüfung.
///
/// # Description
/// `admitted = true` gdw. `violations` leer ist. `scanned_files` gibt an,
/// wie viele Dateien aus dem Diff geprüft wurden (0 bei leerem Diff oder
/// nach Revisions-Mismatch-Kurzschluss).
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::admission::AdmissionReport;
/// let r = AdmissionReport { admitted: true, violations: vec![], scanned_files: 3 };
/// assert!(r.admitted);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdmissionReport {
    /// `true` gdw. keine Verstöße gefunden wurden.
    pub admitted: bool,
    /// Alle gefundenen Verstöße.
    pub violations: Vec<ScopeViolation>,
    /// Anzahl der geprüften Dateien aus dem Diff.
    pub scanned_files: usize,
}

// ──────────────────────────────────────────────────────────────────────────────
// Öffentliche Funktionen
// ──────────────────────────────────────────────────────────────────────────────

/// Prüft, ob ein Patch die [`MutationContract`]-Grenzen einhält.
///
/// # Description
/// Prüfreihenfolge (Design-Doc §5, coding-philosophy §8 WriteSet-Enforcement):
///
/// 1. Wenn `diff.base_revision != contract.base_revision` → eine
///    [`ScopeViolation::BaseRevisionMismatch`]-Violation, `admitted=false`,
///    keine weiteren Pfadprüfungen.
/// 2. Für jede `diff.files[i].path`:
///    a. Wenn irgendeine `contract.forbidden_paths`-Regel matcht → `Forbidden`.
///    b. Sonst: wenn keine `contract.allowed_paths`-Regel matcht → `OutsideAllowed`.
/// 3. `admitted = violations.is_empty()`.
///
/// # Arguments
/// - `contract` (`&MutationContract`): der Kontrakt, der die Grenzen festlegt.
/// - `diff` (`&UnifiedDiff`): der Worker-Patch, der geprüft wird.
///
/// # Returns
/// [`AdmissionReport`] mit Ergebnis und allen Verstößen.
///
/// # Concurrency
/// Rein funktional — keine Seiteneffekte, keine Locks.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::admission::{validate_patch, MutationContract, PathRule, RepoRevision, UnifiedDiff, PatchFile, FileChange};
/// use harw_plan::ids::{RevisionId, TaskId};
///
/// let contract = MutationContract {
///     base_revision: RepoRevision("abc".to_owned()),
///     plan_revision: RevisionId::new(1),
///     task_id: TaskId::new("t-1"),
///     allowed_paths: vec![PathRule::Exact("src/lib.rs".to_owned())],
///     forbidden_paths: vec![],
/// };
/// let diff = UnifiedDiff {
///     base_revision: RepoRevision("abc".to_owned()),
///     files: vec![PatchFile {
///         path: "src/lib.rs".to_owned(),
///         source_path: None,
///         change: FileChange::Modified,
///     }],
/// };
/// let report = validate_patch(&contract, &diff);
/// assert!(report.admitted);
/// assert_eq!(report.scanned_files, 1);
/// ```
pub fn validate_patch(contract: &MutationContract, diff: &UnifiedDiff) -> AdmissionReport {
    // Schritt 1: Basis-Revisions-Mismatch → Kurzschluss.
    if diff.base_revision != contract.base_revision {
        return AdmissionReport {
            admitted: false,
            violations: vec![ScopeViolation::BaseRevisionMismatch {
                contract: contract.base_revision.clone(),
                patch: diff.base_revision.clone(),
            }],
            scanned_files: 0,
        };
    }

    // Schritt 2: Pfadprüfung.
    let mut violations: Vec<ScopeViolation> = Vec::new();

    for patch_file in &diff.files {
        validate_path(contract, patch_file.path.as_str(), &mut violations);

        if patch_file.change == FileChange::Renamed {
            match patch_file.source_path.as_deref() {
                Some(source_path) => validate_path(contract, source_path, &mut violations),
                // Ein Rename ohne Sourcepfad kann nicht sicher admitted werden:
                // der Patch könnte sonst aus einem fremden Scope heraus verschieben.
                None => violations.push(ScopeViolation::OutsideAllowed {
                    path: "<missing rename source>".to_owned(),
                }),
            }
        }
    }

    AdmissionReport {
        admitted: violations.is_empty(),
        scanned_files: diff.files.len(),
        violations,
    }
}

/// Erstellt einen [`MutationContract`] aus einem [`crate::types::PlanNode`].
///
/// # Description
/// Mappt `node.write_scope` auf `allowed_paths` und `node.forbidden_scope`
/// auf `forbidden_paths`. Nur Einträge, deren String-Repräsentation mindestens
/// einen `.` oder `/` enthält, werden als [`PathRule::Exact`] übernommen —
/// reine Symbol-Einträge (z.B. `TraitName`, `CONSTANT`) werden ignoriert
/// (Symbol-Enforcement kommt in einem Folge-Wave, Design-Doc §5).
///
/// # Arguments
/// - `node` (`&crate::types::PlanNode`): der Plan-Knoten.
/// - `base_revision` (`RepoRevision`): aktuelle Basis-Revision des Repos.
/// - `plan_revision` (`crate::ids::RevisionId`): aktuelle Plan-Revision.
///
/// # Returns
/// Einen neuen [`MutationContract`], der dem Knoten entspricht.
///
/// # Concurrency
/// Rein funktional.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::admission::{contract_from_node, PathRule, RepoRevision};
/// use harw_plan::ids::{RevisionId, TaskId, PathOrSymbol};
/// use harw_plan::types::{PlanNode, PlanNodeKind, PlanNodeStatus};
/// use time::OffsetDateTime;
///
/// let node = PlanNode {
///     id: TaskId::new("t-1"),
///     objective: String::new(),
///     dependencies: vec![],
///     input_contracts: vec![],
///     output_contracts: vec![],
///     read_scope: vec![],
///     write_scope: vec![PathOrSymbol::new("src/lib.rs")],
///     forbidden_scope: vec![],
///     acceptance_criteria: vec![],
///     invalidation_conditions: vec![],
///     status: PlanNodeStatus::Draft,
///     evidence: vec![],
///     kind: PlanNodeKind::Coding,
///     wave: None,
///     assignment: None,
///     parent: None,
///     created_at: OffsetDateTime::UNIX_EPOCH,
///     updated_at: OffsetDateTime::UNIX_EPOCH,
/// };
/// let contract = contract_from_node(&node, RepoRevision("sha".to_owned()), RevisionId::new(1));
/// assert!(contract.allowed_paths.contains(&PathRule::Exact("src/lib.rs".to_owned())));
/// ```
pub fn contract_from_node(
    node: &crate::types::PlanNode,
    base_revision: RepoRevision,
    plan_revision: crate::ids::RevisionId,
) -> MutationContract {
    let allowed_paths = path_or_symbol_to_rules(&node.write_scope);
    let forbidden_paths = path_or_symbol_to_rules(&node.forbidden_scope);

    MutationContract {
        base_revision,
        plan_revision,
        task_id: node.id.clone(),
        allowed_paths,
        forbidden_paths,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Interne Hilfsfunktionen
// ──────────────────────────────────────────────────────────────────────────────

/// Wandelt eine Liste von [`crate::ids::PathOrSymbol`] in [`PathRule`]-Regeln um.
///
/// Einträge mit Glob-Metazeichen (`*` oder `?`) werden zu [`PathRule::Glob`];
/// Einträge mit `.` oder `/` (ohne Metazeichen) zu [`PathRule::Exact`]. Reine
/// Symbol-Einträge (kein `.`, `/`, `*`, `?` — z.B. `TraitName`, `CONSTANT`)
/// sind dateiweise nicht referenzierbar und werden ignoriert: Enforcement
/// bleibt dateiweise, Symbol-Granularität ist Gegenstand einer Folge-Welle
/// (Design-Doc §5).
fn path_or_symbol_to_rules(entries: &[crate::ids::PathOrSymbol]) -> Vec<PathRule> {
    entries
        .iter()
        .filter_map(|pos| {
            let s = pos.as_str();
            if s.contains('*') || s.contains('?') {
                Some(PathRule::Glob(s.to_owned()))
            } else if s.contains('.') || s.contains('/') {
                Some(PathRule::Exact(s.to_owned()))
            } else {
                // Symbolischer Eintrag — Enforcement in Folge-Wave.
                None
            }
        })
        .collect()
}

/// Prüft einen einzelnen Patchpfad gegen einen Mutation-Kontrakt.
///
/// Eingaben werden rein lexikalisch normalisiert. Absolute Pfade, Backslashes,
/// leere Pfade und Eltern-Traversal werden verworfen, statt sie auf einen Pfad
/// außerhalb des Repository-Roots abzubilden.
fn validate_path(contract: &MutationContract, path: &str, violations: &mut Vec<ScopeViolation>) {
    let Some(normalized_path) = normalize_repo_path(path) else {
        violations.push(ScopeViolation::OutsideAllowed {
            path: path.to_owned(),
        });
        return;
    };

    // Forbidden-Check hat Vorrang.
    if let Some(rule) = contract
        .forbidden_paths
        .iter()
        .find(|rule| rule.matches(normalized_path.as_str()))
    {
        violations.push(ScopeViolation::Forbidden {
            path: path.to_owned(),
            rule: rule.clone(),
        });
        return;
    }

    if !contract
        .allowed_paths
        .iter()
        .any(|rule| rule.matches(normalized_path.as_str()))
    {
        violations.push(ScopeViolation::OutsideAllowed {
            path: path.to_owned(),
        });
    }
}

/// Normalisiert einen Repository-relativen Pfad ohne Filesystem-Zugriff.
///
/// `.`-Segmente und wiederholte Slashes sind rein syntaktisch und werden
/// entfernt. Ein `..`-Segment wird dagegen nicht aufgelöst: Der Eingabepfad
/// wird vollständig abgewiesen, weil er die Root-Containment-Garantie verletzt.
fn normalize_repo_path(path: &str) -> Option<String> {
    if path.is_empty()
        || path.starts_with('/')
        || is_windows_absolute_path(path)
        || path.contains('\\')
        || path.contains('\0')
    {
        return None;
    }

    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => return None,
            component => components.push(component),
        }
    }

    (!components.is_empty()).then(|| components.join("/"))
}

/// Erkennt einen absoluten Windows-Pfad in der vorwärts-Slash-Repräsentation.
///
/// Diff-Pfade werden plattformübergreifend transportiert. Deshalb reicht eine
/// Prüfung auf einen führenden Slash nicht aus: `C:/...` wäre auf Unix zwar
/// kein absoluter [`std::path::Path`], darf aber niemals als Repo-relativer
/// Pfad admission erhalten.
fn is_windows_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

/// Prüft, ob `path` das Glob-Muster `pattern` erfüllt.
///
/// Unterstützte Syntax: `*` (beliebig viele Zeichen außer `/`), `**`
/// (beliebig viele Segmente inkl. `/`), `?` (genau ein Zeichen außer `/`),
/// sonst Literale. Kein Brace-Expansion, keine Zeichenklassen. Beide Seiten
/// werden vor dem Vergleich über [`normalize_repo_path`] normalisiert;
/// ungültige Eingaben (Traversal, absolute Pfade, Backslashes, …) liefern
/// `false`.
///
/// Implementiert als Segment-DP über die durch `/` getrennten Komponenten:
/// `dp[i][j]` ist wahr, wenn die restlichen Pattern-Segmente ab Index `i`
/// die restlichen Pfad-Segmente ab Index `j` matchen. `**` darf dabei null
/// oder mehr vollständige Segmente konsumieren (`dp[i+1][j] || dp[i][j+1]`),
/// alle anderen Segmente werden einzeln über [`segment_matches`] verglichen.
/// Die DP-Tabelle vermeidet das exponentielle Backtracking einer naiven
/// Rekursion bei mehreren `**`-Segmenten.
fn glob_matches(pattern: &str, path: &str) -> bool {
    let (Some(pattern), Some(path)) = (normalize_repo_path(pattern), normalize_repo_path(path))
    else {
        return false;
    };

    let pattern_segments: Vec<&str> = pattern.split('/').collect();
    let path_segments: Vec<&str> = path.split('/').collect();

    let p_len = pattern_segments.len();
    let t_len = path_segments.len();

    // dp[i][j] = pattern_segments[i..] matcht path_segments[j..].
    let mut dp = vec![vec![false; t_len + 1]; p_len + 1];
    dp[p_len][t_len] = true;

    for i in (0..=p_len).rev() {
        for j in (0..=t_len).rev() {
            if i == p_len && j == t_len {
                continue; // Basisfall bereits gesetzt.
            }
            dp[i][j] = if i == p_len {
                false
            } else if pattern_segments[i] == "**" {
                dp[i + 1][j] || (j < t_len && dp[i][j + 1])
            } else {
                j < t_len
                    && segment_matches(pattern_segments[i], path_segments[j])
                    && dp[i + 1][j + 1]
            };
        }
    }

    dp[0][0]
}

/// Prüft ein einzelnes Pfadsegment gegen ein Pattern-Segment mit `*`/`?`.
///
/// Da Segmente bereits an `/` gesplittet sind, kann `*` innerhalb dieser
/// Funktion beliebig viele (auch null) Zeichen matchen und `?` genau ein
/// Zeichen — beide implizit ohne `/`, weil das Segment selbst keinen `/`
/// enthält. Klassischer Zwei-Zeiger-Algorithmus mit Rücksprungpunkt für den
/// letzten `*`: linear in der Segmentlänge, kein exponentielles Backtracking.
fn segment_matches(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star_idx: Option<usize> = None;
    let mut match_idx = 0usize;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_idx = Some(pi);
            match_idx = ti;
            pi += 1;
        } else if let Some(si) = star_idx {
            pi = si + 1;
            match_idx += 1;
            ti = match_idx;
        } else {
            return false;
        }
    }

    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }

    pi == p.len()
}

/// Gibt an, ob zwei Repo-relative Write-Scopes sich überschneiden.
///
/// Ein Pfad enthält seine Nachfahren: `src` kollidiert deshalb mit
/// `src/lib.rs`; Geschwister wie `src/a.rs` und `src/b.rs` nicht. Ungültige
/// Pfade haben keinen gültigen Scope und liefern `false`; ihre Abweisung liegt
/// bei der Admission-Prüfung.
pub(crate) fn write_scopes_conflict(left: &str, right: &str) -> bool {
    let (Some(left), Some(right)) = (normalize_repo_path(left), normalize_repo_path(right)) else {
        return false;
    };

    left == right
        || left
            .strip_prefix(right.as_str())
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left.as_str())
            .is_some_and(|suffix| suffix.starts_with('/'))
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{PathOrSymbol, RevisionId, TaskId};
    use crate::test_support::TestResult;
    use crate::types::{PlanNode, PlanNodeStatus};
    use time::OffsetDateTime;

    // ── Hilfsfunktionen ──────────────────────────────────────────────────────

    fn base_rev(s: &str) -> RepoRevision {
        RepoRevision(s.to_owned())
    }

    fn make_contract(allowed: Vec<PathRule>, forbidden: Vec<PathRule>) -> MutationContract {
        MutationContract {
            base_revision: base_rev("sha-001"),
            plan_revision: RevisionId::new(1),
            task_id: TaskId::new("t-1"),
            allowed_paths: allowed,
            forbidden_paths: forbidden,
        }
    }

    fn make_diff(files: Vec<(&str, FileChange)>) -> UnifiedDiff {
        UnifiedDiff {
            base_revision: base_rev("sha-001"),
            files: files
                .into_iter()
                .map(|(p, c)| PatchFile {
                    path: p.to_owned(),
                    source_path: None,
                    change: c,
                })
                .collect(),
        }
    }

    fn make_node(write_scope: Vec<&str>, forbidden_scope: Vec<&str>) -> PlanNode {
        PlanNode {
            id: TaskId::new("t-node"),
            objective: String::new(),
            dependencies: vec![],
            input_contracts: vec![],
            output_contracts: vec![],
            read_scope: vec![],
            write_scope: write_scope.into_iter().map(PathOrSymbol::new).collect(),
            forbidden_scope: forbidden_scope.into_iter().map(PathOrSymbol::new).collect(),
            acceptance_criteria: vec![],
            invalidation_conditions: vec![],
            status: PlanNodeStatus::Draft,
            evidence: vec![],
            kind: crate::types::PlanNodeKind::default(),
            wave: None,
            assignment: None,
            parent: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    // ── Test 1: exakter Match ────────────────────────────────────────────────

    #[test]
    fn exact_rule_matches_exact_path() {
        let rule = PathRule::Exact("a/b.rs".to_owned());
        assert!(rule.matches("a/b.rs"), "exakter Match muss greifen");
        assert!(!rule.matches("a/b.rs2"), "kein Suffix-Toleranz");
        assert!(!rule.matches("a/b"), "Dateiendung ist Teil des Matches");
    }

    // ── Test 2: Verzeichnis-Prefix ───────────────────────────────────────────

    #[test]
    fn directory_prefix_matches_child() {
        let rule = PathRule::DirectoryPrefix("src/foo".to_owned());
        assert!(rule.matches("src/foo/bar.rs"), "Kind muss matchen");
        assert!(
            !rule.matches("src/foobar.rs"),
            "kein false-positive bei Präfix ohne Slash"
        );
        assert!(!rule.matches("src/foo"), "Verzeichnis selbst matcht nicht");
    }

    #[test]
    fn directory_prefix_does_not_match_traversal_or_prefix_lookalikes() {
        let rule = PathRule::DirectoryPrefix("src".to_owned());

        assert!(!rule.matches("src/../secret.rs"));
        assert!(!rule.matches("src-evasion/secret.rs"));
        assert!(!rule.matches("src"));
    }

    // ── Test 3: Dateiendungs-Suffix ──────────────────────────────────────────

    #[test]
    fn extension_suffix_matches() {
        let rule = PathRule::ExtensionSuffix(".toml".to_owned());
        assert!(rule.matches("Cargo.toml"), "Standard-Toml matcht");
        assert!(
            rule.matches("some/path/to/Cargo.toml"),
            "tief verschachtelter Pfad"
        );
        assert!(!rule.matches("toml.txt"), "falsche Endung matcht nicht");
        assert!(!rule.matches("mytoml"), "kein Punkt matcht nicht");
    }

    // ── Test 4: alles erlaubt → admitted ────────────────────────────────────

    #[test]
    fn validate_patch_admits_when_all_allowed() {
        let contract = make_contract(
            vec![
                PathRule::DirectoryPrefix("src".to_owned()),
                PathRule::Exact("Cargo.toml".to_owned()),
            ],
            vec![],
        );
        let diff = make_diff(vec![
            ("src/lib.rs", FileChange::Modified),
            ("src/foo/bar.rs", FileChange::Added),
            ("Cargo.toml", FileChange::Modified),
        ]);
        let report = validate_patch(&contract, &diff);
        assert!(report.admitted, "alle Dateien in allowed → admitted");
        assert!(report.violations.is_empty());
        assert_eq!(report.scanned_files, 3);
    }

    // ── Test 5: Datei außerhalb allowed ─────────────────────────────────────

    #[test]
    fn validate_patch_denies_outside_allowed() {
        let contract = make_contract(vec![PathRule::Exact("src/lib.rs".to_owned())], vec![]);
        let diff = make_diff(vec![("src/other.rs", FileChange::Added)]);
        let report = validate_patch(&contract, &diff);
        assert!(!report.admitted);
        assert_eq!(report.violations.len(), 1);
        assert!(matches!(
            &report.violations[0],
            ScopeViolation::OutsideAllowed { path } if path == "src/other.rs"
        ));
    }

    // ── Test 6: forbidden → Violation ───────────────────────────────────────

    #[test]
    fn validate_patch_denies_forbidden() {
        let forbidden_rule = PathRule::Exact("src/secret.rs".to_owned());
        let contract = make_contract(
            vec![PathRule::DirectoryPrefix("src".to_owned())],
            vec![forbidden_rule.clone()],
        );
        let diff = make_diff(vec![("src/secret.rs", FileChange::Modified)]);
        let report = validate_patch(&contract, &diff);
        assert!(!report.admitted);
        assert_eq!(report.violations.len(), 1);
        assert!(matches!(
            &report.violations[0],
            ScopeViolation::Forbidden { path, rule } if path == "src/secret.rs" && rule == &forbidden_rule
        ));
    }

    // ── Test 7: forbidden schlägt allowed ───────────────────────────────────

    #[test]
    fn forbidden_wins_over_allowed() {
        let contract = make_contract(
            vec![PathRule::DirectoryPrefix("src".to_owned())],
            vec![PathRule::Exact("src/secret.rs".to_owned())],
        );
        // src/secret.rs matcht allowed (via Prefix) UND forbidden (via Exact).
        let diff = make_diff(vec![("src/secret.rs", FileChange::Modified)]);
        let report = validate_patch(&contract, &diff);
        assert!(!report.admitted, "Forbidden muss gewinnen");
        assert_eq!(report.violations.len(), 1);
        assert!(matches!(
            &report.violations[0],
            ScopeViolation::Forbidden { .. }
        ));
    }

    // ── Test 8: Revisions-Mismatch ───────────────────────────────────────────

    #[test]
    fn base_revision_mismatch_produces_violation() {
        let contract = make_contract(vec![PathRule::Exact("src/lib.rs".to_owned())], vec![]);
        // Diff hat andere Revision.
        let diff = UnifiedDiff {
            base_revision: base_rev("sha-OTHER"),
            files: vec![PatchFile {
                path: "src/lib.rs".to_owned(),
                source_path: None,
                change: FileChange::Modified,
            }],
        };
        let report = validate_patch(&contract, &diff);
        assert!(!report.admitted);
        assert_eq!(report.violations.len(), 1);
        assert!(
            matches!(
                &report.violations[0],
                ScopeViolation::BaseRevisionMismatch { contract: c, patch: p }
                if c == &base_rev("sha-001") && p == &base_rev("sha-OTHER")
            ),
            "Mismatch-Violation nicht korrekt"
        );
        // Nach Mismatch keine Pfad-Violations.
        assert_eq!(report.scanned_files, 0, "kein Pfad-Scan nach Mismatch");
    }

    // ── Test 9: contract_from_node — write_scope mappt korrekt ──────────────

    #[test]
    fn contract_from_node_maps_write_scope() {
        let node = make_node(
            vec!["src/lib.rs", "Cargo.toml", "tests/integration.rs"],
            vec![],
        );
        let contract = contract_from_node(&node, base_rev("sha-abc"), RevisionId::new(2));
        assert!(
            contract
                .allowed_paths
                .contains(&PathRule::Exact("src/lib.rs".to_owned())),
            "src/lib.rs muss als Exact-Regel vorhanden sein"
        );
        assert!(
            contract
                .allowed_paths
                .contains(&PathRule::Exact("Cargo.toml".to_owned())),
        );
        assert!(
            contract
                .allowed_paths
                .contains(&PathRule::Exact("tests/integration.rs".to_owned())),
        );
        assert_eq!(contract.allowed_paths.len(), 3);
    }

    // ── Test 10: Symbol-Einträge werden ignoriert ────────────────────────────

    #[test]
    fn contract_from_node_ignores_symbol_entries() {
        // Einträge ohne Slash oder Punkt sind Symbole.
        let node = make_node(vec!["MyTrait", "MY_CONST", "src/lib.rs"], vec![]);
        let contract = contract_from_node(&node, base_rev("sha-abc"), RevisionId::new(1));
        // Nur src/lib.rs (enthält '/') → 1 Regel.
        assert_eq!(contract.allowed_paths.len(), 1);
        assert_eq!(
            contract.allowed_paths[0],
            PathRule::Exact("src/lib.rs".to_owned())
        );
    }

    // ── Test 11: Serde-Roundtrip für AdmissionReport ─────────────────────────

    #[test]
    fn admission_report_serde_roundtrip() -> TestResult {
        let report = AdmissionReport {
            admitted: false,
            violations: vec![
                ScopeViolation::OutsideAllowed {
                    path: "some/path.rs".to_owned(),
                },
                ScopeViolation::Forbidden {
                    path: "bad.rs".to_owned(),
                    rule: PathRule::Exact("bad.rs".to_owned()),
                },
                ScopeViolation::BaseRevisionMismatch {
                    contract: base_rev("a"),
                    patch: base_rev("b"),
                },
            ],
            scanned_files: 5,
        };
        let json = serde_json::to_string(&report)?;
        let back: AdmissionReport = serde_json::from_str(&json)?;
        assert_eq!(report, back, "Serde-Roundtrip muss verlustfrei sein");
        Ok(())
    }

    // ── Test 12: Leerer Diff ist admitted ────────────────────────────────────

    #[test]
    fn empty_diff_is_admitted() {
        let contract = make_contract(vec![], vec![]);
        let diff = UnifiedDiff {
            base_revision: base_rev("sha-001"),
            files: vec![],
        };
        let report = validate_patch(&contract, &diff);
        assert!(report.admitted, "leerer Diff muss admitted sein");
        assert!(report.violations.is_empty());
        assert_eq!(report.scanned_files, 0);
    }

    #[test]
    fn validate_patch_normalizes_benign_relative_components() {
        let contract = make_contract(vec![PathRule::Exact("src/lib.rs".to_owned())], vec![]);
        let diff = make_diff(vec![("./src//lib.rs", FileChange::Modified)]);

        let report = validate_patch(&contract, &diff);

        assert!(
            report.admitted,
            "lexically equivalent paths must be admitted"
        );
    }

    #[test]
    fn validate_patch_normalizes_rule_and_path_before_directory_prefix_matching() {
        let contract = make_contract(
            vec![PathRule::DirectoryPrefix("./plans//active/".to_owned())],
            vec![],
        );
        let diff = make_diff(vec![("./plans/active//goal.md", FileChange::Modified)]);

        let report = validate_patch(&contract, &diff);

        assert!(
            report.admitted,
            "equivalent relative paths must match a normalized directory rule"
        );
    }

    #[test]
    fn validate_patch_rejects_unix_and_windows_absolute_paths() {
        let contract = make_contract(vec![PathRule::DirectoryPrefix("plans".to_owned())], vec![]);
        let diff = make_diff(vec![
            ("/etc/goal.md", FileChange::Modified),
            ("C:/workspace/goal.md", FileChange::Modified),
        ]);

        let report = validate_patch(&contract, &diff);

        assert!(!report.admitted, "absolute paths must never be admitted");
        assert!(matches!(
            &report.violations[..],
            [
                ScopeViolation::OutsideAllowed { path: unix_path },
                ScopeViolation::OutsideAllowed { path: windows_path },
            ] if unix_path == "/etc/goal.md" && windows_path == "C:/workspace/goal.md"
        ));
    }

    #[test]
    fn validate_patch_rejects_traversal_before_directory_rule_matching() {
        let contract = make_contract(vec![PathRule::DirectoryPrefix("src".to_owned())], vec![]);
        let diff = make_diff(vec![("src/../secret.rs", FileChange::Modified)]);

        let report = validate_patch(&contract, &diff);

        assert!(
            !report.admitted,
            "parent traversal must never escape a scope"
        );
        assert!(matches!(
            &report.violations[..],
            [ScopeViolation::OutsideAllowed { path }] if path == "src/../secret.rs"
        ));
    }

    #[test]
    fn validate_patch_rejects_parent_traversal_at_repository_root() {
        let contract = make_contract(vec![PathRule::DirectoryPrefix("src".to_owned())], vec![]);
        let diff = make_diff(vec![("../src/lib.rs", FileChange::Modified)]);

        let report = validate_patch(&contract, &diff);

        assert!(!report.admitted);
        assert!(matches!(
            &report.violations[..],
            [ScopeViolation::OutsideAllowed { path }] if path == "../src/lib.rs"
        ));
    }

    #[test]
    fn validate_patch_checks_rename_source_and_destination() {
        let contract = make_contract(vec![PathRule::DirectoryPrefix("src".to_owned())], vec![]);
        let diff = UnifiedDiff {
            base_revision: base_rev("sha-001"),
            files: vec![PatchFile {
                path: "src/lib.rs".to_owned(),
                source_path: Some("src/../secrets/key.rs".to_owned()),
                change: FileChange::Renamed,
            }],
        };

        let report = validate_patch(&contract, &diff);

        assert!(
            !report.admitted,
            "rename sources need the same admission as targets"
        );
        assert!(matches!(
            &report.violations[..],
            [ScopeViolation::OutsideAllowed { path }] if path == "src/../secrets/key.rs"
        ));
    }

    #[test]
    fn validate_patch_rejects_rename_destination_outside_allowed_scope() {
        let contract = make_contract(vec![PathRule::DirectoryPrefix("src".to_owned())], vec![]);
        let diff = UnifiedDiff {
            base_revision: base_rev("sha-001"),
            files: vec![PatchFile {
                path: "C:/workspace/goal.md".to_owned(),
                source_path: Some("src/goal.md".to_owned()),
                change: FileChange::Renamed,
            }],
        };

        let report = validate_patch(&contract, &diff);

        assert!(
            !report.admitted,
            "rename destinations need the same admission as sources"
        );
        assert!(matches!(
            &report.violations[..],
            [ScopeViolation::OutsideAllowed { path }] if path == "C:/workspace/goal.md"
        ));
    }

    #[test]
    fn validate_patch_rejects_rename_source_outside_allowed_scope() {
        let contract = make_contract(vec![PathRule::DirectoryPrefix("src".to_owned())], vec![]);
        let diff = UnifiedDiff {
            base_revision: base_rev("sha-001"),
            files: vec![PatchFile {
                path: "src/new.rs".to_owned(),
                source_path: Some("vendor/old.rs".to_owned()),
                change: FileChange::Renamed,
            }],
        };

        let report = validate_patch(&contract, &diff);

        assert!(!report.admitted);
        assert!(matches!(
            &report.violations[..],
            [ScopeViolation::OutsideAllowed { path }] if path == "vendor/old.rs"
        ));
    }

    #[test]
    fn validate_patch_rejects_rename_without_source_path() {
        let contract = make_contract(vec![PathRule::DirectoryPrefix("src".to_owned())], vec![]);
        let diff = UnifiedDiff {
            base_revision: base_rev("sha-001"),
            files: vec![PatchFile {
                path: "src/lib.rs".to_owned(),
                source_path: None,
                change: FileChange::Renamed,
            }],
        };

        let report = validate_patch(&contract, &diff);

        assert!(
            !report.admitted,
            "a rename without source cannot be safely admitted"
        );
        assert!(matches!(
            &report.violations[..],
            [ScopeViolation::OutsideAllowed { path }] if path == "<missing rename source>"
        ));
    }

    #[test]
    fn write_scopes_conflict_when_one_scope_contains_the_other() {
        assert!(write_scopes_conflict("src", "src/lib.rs"));
        assert!(write_scopes_conflict("./src/", "src//lib.rs"));
        assert!(!write_scopes_conflict("src/a.rs", "src/b.rs"));
        assert!(!write_scopes_conflict("src/../secret.rs", "secret.rs"));
    }

    // ── Glob-Matcher: die im Auftrag geforderten Beispiele ──────────────────

    #[test]
    fn glob_double_star_matches_nested_rs_files_but_not_other_dirs() {
        let rule = PathRule::Glob("src/**/*.rs".to_owned());
        assert!(rule.matches("src/a/b.rs"), "ein Zwischensegment");
        assert!(rule.matches("src/a/b/c.rs"), "mehrere Zwischensegmente");
        assert!(!rule.matches("tests/a.rs"), "anderes Wurzelverzeichnis");
    }

    #[test]
    fn glob_single_star_does_not_cross_directory_boundary() {
        let rule = PathRule::Glob("Cargo.*".to_owned());
        assert!(rule.matches("Cargo.toml"), "Top-Level-Datei matcht");
        assert!(
            !rule.matches("sub/Cargo.toml"),
            "* darf keine Verzeichnisgrenze überschreiten"
        );
    }

    #[test]
    fn glob_leading_double_star_matches_root_and_nested_segment() {
        let rule = PathRule::Glob("**/mod.rs".to_owned());
        assert!(rule.matches("a/mod.rs"), "ein vorangestelltes Segment");
        assert!(rule.matches("mod.rs"), "** darf null Segmente konsumieren");
    }

    #[test]
    fn glob_question_mark_matches_exactly_one_char() {
        let rule = PathRule::Glob("a?.rs".to_owned());
        assert!(rule.matches("ab.rs"));
        assert!(!rule.matches("a.rs"), "? verlangt genau ein Zeichen");
        assert!(
            !rule.matches("abc.rs"),
            "? darf nicht mehrere Zeichen decken"
        );
    }

    #[test]
    fn glob_matches_rejects_invalid_normalization_on_either_side() {
        assert!(!ScopeMatcher::matches_glob(
            "src/**/*.rs",
            "src/../secret.rs"
        ));
        assert!(!ScopeMatcher::matches_glob("../*.rs", "a.rs"));
        assert!(!ScopeMatcher::matches_glob("*.rs", "/abs/a.rs"));
    }

    #[test]
    fn glob_matches_literal_segments_without_metacharacters() {
        assert!(ScopeMatcher::matches_glob(
            "docs/readme.md",
            "docs/readme.md"
        ));
        assert!(!ScopeMatcher::matches_glob(
            "docs/readme.md",
            "docs/readme2.md"
        ));
    }

    // ── ScopeMatcher-Fassade ─────────────────────────────────────────────────

    #[test]
    fn scope_matcher_conflicts_delegates_to_write_scopes_conflict() {
        assert!(ScopeMatcher::conflicts("src", "src/lib.rs"));
        assert!(!ScopeMatcher::conflicts("src/a.rs", "src/b.rs"));
    }

    #[test]
    fn scope_matcher_normalize_delegates_to_normalize_repo_path() {
        assert_eq!(
            ScopeMatcher::normalize("./src//lib.rs"),
            Some("src/lib.rs".to_owned())
        );
        assert_eq!(ScopeMatcher::normalize("../secret"), None);
    }

    #[test]
    fn scope_matcher_contains_true_for_self_and_descendants() {
        assert!(
            ScopeMatcher::contains("src", "src"),
            "Scope enthält sich selbst"
        );
        assert!(ScopeMatcher::contains("src", "src/lib.rs"), "Nachfahre");
        assert!(
            ScopeMatcher::contains("src", "src/a/b.rs"),
            "tiefer Nachfahre"
        );
    }

    #[test]
    fn scope_matcher_contains_false_for_ancestor_sibling_and_lookalike() {
        assert!(
            !ScopeMatcher::contains("src/lib.rs", "src"),
            "contains ist gerichtet — Vorfahre ist kein Nachfahre"
        );
        assert!(
            !ScopeMatcher::contains("src/a.rs", "src/b.rs"),
            "Geschwister"
        );
        assert!(
            !ScopeMatcher::contains("src", "src-evasion/secret.rs"),
            "Präfix ohne Slash ist kein Nachfahre"
        );
    }

    #[test]
    fn scope_matcher_contains_rejects_invalid_paths() {
        assert!(!ScopeMatcher::contains("src", "src/../secret.rs"));
        assert!(!ScopeMatcher::contains("../src", "src/lib.rs"));
    }

    // ── contract_from_node mit Glob-Scope ────────────────────────────────────

    #[test]
    fn contract_from_node_maps_glob_entries_to_glob_rules() {
        let node = make_node(
            vec!["src/**/*.rs", "Cargo.*", "MyTrait", "src/lib.rs"],
            vec![],
        );
        let contract = contract_from_node(&node, base_rev("sha-abc"), RevisionId::new(1));

        assert!(
            contract
                .allowed_paths
                .contains(&PathRule::Glob("src/**/*.rs".to_owned())),
            "Metazeichen-Eintrag muss zu PathRule::Glob werden"
        );
        assert!(
            contract
                .allowed_paths
                .contains(&PathRule::Glob("Cargo.*".to_owned())),
        );
        assert!(
            contract
                .allowed_paths
                .contains(&PathRule::Exact("src/lib.rs".to_owned())),
            "Eintrag ohne Metazeichen bleibt Exact"
        );
        assert!(
            !contract
                .allowed_paths
                .iter()
                .any(|rule| matches!(rule, PathRule::Exact(s) if s == "MyTrait")),
            "reines Symbol bleibt ignoriert"
        );
        assert_eq!(contract.allowed_paths.len(), 3, "MyTrait wird ignoriert");
    }

    #[test]
    fn validate_patch_admits_file_via_glob_rule_in_contract() {
        let contract = make_contract(vec![PathRule::Glob("src/**/*.rs".to_owned())], vec![]);
        let diff = make_diff(vec![
            ("src/a/b.rs", FileChange::Modified),
            ("src/a/b/c.rs", FileChange::Added),
        ]);

        let report = validate_patch(&contract, &diff);

        assert!(report.admitted, "Glob-Regel muss beide Pfade abdecken");
        assert_eq!(report.scanned_files, 2);
    }

    #[test]
    fn validate_patch_denies_file_outside_glob_rule_in_contract() {
        let contract = make_contract(vec![PathRule::Glob("src/**/*.rs".to_owned())], vec![]);
        let diff = make_diff(vec![("tests/a.rs", FileChange::Added)]);

        let report = validate_patch(&contract, &diff);

        assert!(!report.admitted);
        assert!(matches!(
            &report.violations[..],
            [ScopeViolation::OutsideAllowed { path }] if path == "tests/a.rs"
        ));
    }
}
