//! Das Workspace-Inventar: Bezugspunkt und Vergleich für Strukturdrift.
//!
//! # Verantwortungsbereich
//! [`Inventory`] ist eine reine, serde-serialisierbare Momentaufnahme des
//! Cargo-Workspace zum Zeitpunkt von [`Inventory::read`]: welche Member es
//! gibt, welche direkten Abhängigkeitskanten zwischen Crates bestehen, und
//! welche Version jede externe (Nicht-Member-)Abhängigkeit gerade gesperrt
//! hat. [`Inventory::diff`] vergleicht zwei solcher Momentaufnahmen und
//! liefert die Liste der [`StructureChange`]s zwischen ihnen.
//!
//! Diese Crate hält **kein** Inventar über einen Aufruf hinaus — siehe
//! Crate-Moduldoku ([`crate`]) für die Begründung. [`Inventory`] ist bewusst
//! `Serialize`/`Deserialize`, damit der Aufrufer sie z. B. als JSON-Datei
//! zwischen zwei Läufen ablegen kann.
//!
//! # Warum genau diese drei Felder
//! - `members`: neue Workspace-Member sind die sichtbarste Form von
//!   Strukturdrift (jemand hat eine neue Crate ins Programm aufgenommen).
//! - `edges`: eine neue direkte Abhängigkeit *eines bestimmten* Crates —
//!   gleich ob auf ein anderes Member oder auf eine externe Crate — ist die
//!   architektonisch relevante Kante, nicht nur "diese Abhängigkeit existiert
//!   irgendwo im Workspace".
//! - `dependency_versions`: die gesperrte Version jeder Nicht-Member-
//!   Abhängigkeit aus `Cargo.lock`. `Cargo.lock` enthält den **gesamten**
//!   transitiven Abhängigkeitsbaum, nicht nur direkte Abhängigkeiten — genau
//!   das macht diesen Sensor nützlich, denn laut Aufgabenstellung ist eine
//!   neue *transitive* Abhängigkeit der häufigste Weg, auf dem fremder Code
//!   in ein Projekt gelangt.
//!
//! Bewusst außerhalb des Anwendungsbereichs: `[dev-dependencies]` und
//! `[build-dependencies]`. Sie werden nicht ausgeliefert bzw. laufen nur zur
//! Build-Zeit in einem Repository, das man ohnehin kontrolliert — die
//! sicherheitsrelevante Fläche ist die `[dependencies]`-Oberfläche, die
//! `harw_code_graph::CrateNode::deps`/`external_deps` bereits genau abbilden.
//!
//! # Examples
//! ```rust
//! use harw_dod_workspace::{Inventory, StructureChange};
//!
//! let previous = Inventory::default();
//! let mut current = Inventory::default();
//! current.members.insert("harw-new-crate".to_owned());
//!
//! assert_eq!(
//!     current.diff(&previous),
//!     vec![StructureChange::MemberAdded { name: "harw-new-crate".to_owned() }]
//! );
//! // Ein unveränderter Stand meldet nichts.
//! assert!(current.diff(&current).is_empty());
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use harw_code_graph::{CodeGraphError, WorkspaceGraph, parse_lockfile};
use harw_dod_cap::ReadScope;
use serde::{Deserialize, Serialize};

use crate::error::WorkspaceError;

/// Eine direkte Abhängigkeitskante zwischen zwei Crates.
///
/// # Description
/// `from` ist immer ein Workspace-Member (nur Member haben ein eigenes
/// `[dependencies]`, das dieser Sensor liest); `to` ist der Name der
/// Abhängigkeit — ein anderes Member oder eine externe Crate. Named-Field-
/// Struktur statt Tupel, damit die Serde-Form `{"from": ..., "to": ...}`
/// lesbar bleibt, wenn ein Aufrufer ein [`Inventory`] als Datei ablegt.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    /// Das konsumierende Crate (immer ein Workspace-Member).
    pub from: String,
    /// Der Name der Abhängigkeit (Member oder externe Crate).
    pub to: String,
}

/// Wie stark ein Versionssprung laut SemVer die API bricht.
///
/// # Description
/// Cargos Kompatibilitätsregel für Pre-1.0-Crates (`0.y.z`) behandelt die
/// Minor-Stelle wie sonst die Major-Stelle: `0.2.28 → 0.2.32` ist additiv
/// (Patch), `0.2.x → 0.3.0` bricht die API (Breaking) — siehe
/// [`classify_version_change`] für die vollständige Regel. Prerelease- und
/// Build-Metadaten (`-alpha`, `+build`) fließen nicht gesondert ein, da sie
/// laut SemVer ohnehin keine Kompatibilitätszusage tragen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VersionSeverity {
    /// Nur die Patch-Stelle hat sich geändert (bzw. bei `0.0.z`: siehe
    /// [`Self::Breaking`] — dort gibt es keine Patch-Stufe).
    Patch,
    /// Minor-Bump bei einer stabilisierten Crate (Major ≥ 1): additiv laut
    /// SemVer-Versprechen.
    Minor,
    /// Bricht laut SemVer wahrscheinlich die API: Major-Bump bei einer
    /// stabilen Crate, Minor-Bump bei einer Pre-1.0-Crate (`0.y.z`), jede
    /// Änderung bei einer `0.0.z`-Crate (keine Kompatibilitätszusage), oder
    /// der Übergang von `0.x` auf `≥1.0`.
    Breaking,
    /// Mindestens eine der beiden Versionszeichenketten ist kein gültiges
    /// SemVer. Wird **nicht** stillschweigend als `Patch` behandelt — ein
    /// Sicherheitssensor, der eine unklare Eingabe kleinredet, verfehlt genau
    /// den Zweck, für den er gebaut wurde.
    Unparseable,
}

/// Eine einzelne, konkrete Abweichung zwischen zwei [`Inventory`]-Ständen.
///
/// # Description
/// Von [`Inventory::diff`] erzeugt. Jede Variante entspricht einer der vier
/// im Aufgabenzuschnitt genannten Drift-Formen: neue Abhängigkeit,
/// Versionssprung, neue Kante, neues Member — plus `DependencyRemoved` für
/// den symmetrischen Fall. Es gibt bewusst **kein** `MemberRemoved` und kein
/// `EdgeRemoved`: das Verschwinden eines Members oder einer Kante ist keine
/// neue Angriffsfläche und damit kein Sicherheitssignal in dem Sinn, den
/// dieser Sensor beobachtet (Verkleinerung der Fläche ist nie das Risiko).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "change")]
pub enum StructureChange {
    /// Eine neue (Nicht-Member-)Abhängigkeit ist im Workspace aufgetaucht.
    DependencyAdded {
        /// Name der neu aufgetauchten Abhängigkeit.
        name: String,
    },
    /// Eine zuvor vorhandene (Nicht-Member-)Abhängigkeit ist verschwunden.
    DependencyRemoved {
        /// Name der entfernten Abhängigkeit.
        name: String,
    },
    /// Die gesperrte Version einer Abhängigkeit hat sich geändert.
    VersionChanged {
        /// Name der betroffenen Abhängigkeit.
        name: String,
        /// Vorherige gesperrte Version.
        from: String,
        /// Neue gesperrte Version.
        to: String,
        /// Schwere des Sprungs, siehe [`VersionSeverity`].
        severity: VersionSeverity,
    },
    /// Ein Crate hat eine neue direkte Abhängigkeitskante zu einem anderen
    /// Crate (Member oder extern).
    EdgeAdded {
        /// Das konsumierende Crate.
        from: String,
        /// Der Name der neuen Abhängigkeit.
        to: String,
    },
    /// Ein neues Workspace-Member ist dem Programm beigetreten.
    MemberAdded {
        /// Name des neuen Members.
        name: String,
    },
}

/// Momentaufnahme des Workspace-Abhängigkeitsgraphen zu einem Zeitpunkt.
///
/// # Description
/// Siehe Moduldoku für die Begründung der drei Felder. `Default` liefert das
/// leere Inventar (keine Member, keine Kanten, keine Abhängigkeiten) — nicht
/// zu verwechseln mit "kein Vorgänger vorhanden": Ein `diff` gegen
/// `Inventory::default()` würde *jedes* Member und *jede* Abhängigkeit als
/// neu melden. "Kein Vorgänger" muss der Aufrufer deshalb als `Option<Inventory>`
/// (`None`) modellieren, nicht als `Inventory::default()` — siehe
/// [`crate::sensor::WorkspaceDriftSensor`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    /// Namen aller Workspace-Member.
    pub members: BTreeSet<String>,
    /// Direkte Abhängigkeitskanten: konsumierendes Member → Abhängigkeitsname.
    pub edges: BTreeSet<Edge>,
    /// Gesperrte Version je Nicht-Member-Abhängigkeit, aus `Cargo.lock`.
    pub dependency_versions: BTreeMap<String, String>,
}

/// Ordnet jede `harw_code_graph::CodeGraphError`-Variante einzeln einer
/// [`WorkspaceError`] zu, statt sie (wie vor dem K79-Vorfall) blind auf
/// [`WorkspaceError::MalformedSource`] abzubilden.
///
/// # Description
/// Die Leitfrage für jede Variante: sagt dieser Fehler etwas über den
/// überwachten Baum oder über unser Werkzeug? Siehe [`crate::error`]
/// (Moduldoku) für die vollständige Zuordnungstabelle samt Begründung je
/// Variante. Kurzfassung: `Toml`, `ManifestMissing`, `NoWorkspaceSection`,
/// `MemberMissing` und `CycleDetected` sind strukturelle bzw.
/// Existenz-Fakten über die aktuell im Baum liegenden Dateien — ein Befund
/// über den Baum, [`WorkspaceError::MalformedSource`]. `InvalidPath` (auf
/// diesem Aufrufpfad ausschließlich ein von `resolve_member_dirs` nicht
/// unterstütztes, aber gültiges Glob-Muster), `CargoHomeUnavailable` und
/// `RegistrySourceNotFound` sagen nichts über den Baum aus, sondern über
/// eine Fähigkeitslücke bzw. Umgebungsvoraussetzung unseres eigenen
/// Ladewerkzeugs — [`WorkspaceError::ToolFault`]. `Io` behält seine eigene,
/// bereits vorhandene Variante.
///
/// Verwirft den Ursprungsfehler bewusst (keinen Dateiinhalt, keinen Pfad in
/// der Meldung) — dieselbe inhaltsfrei-Zusicherung wie
/// [`harw_dod_cap::SensorError`].
///
/// # Arguments
/// - `err` (`harw_code_graph::CodeGraphError`): der von
///   `harw_code_graph::WorkspaceGraph::load` bzw.
///   `harw_code_graph::parse_lockfile` gelieferte Ladefehler.
///
/// # Returns
/// Die zugeordnete [`WorkspaceError`].
fn map_code_graph_error(err: CodeGraphError) -> WorkspaceError {
    match err {
        CodeGraphError::Io(io) => WorkspaceError::Io(io),
        CodeGraphError::Toml(_)
        | CodeGraphError::ManifestMissing { .. }
        | CodeGraphError::NoWorkspaceSection { .. }
        | CodeGraphError::MemberMissing { .. }
        | CodeGraphError::CycleDetected { .. } => WorkspaceError::MalformedSource,
        CodeGraphError::InvalidPath { .. }
        | CodeGraphError::CargoHomeUnavailable
        | CodeGraphError::RegistrySourceNotFound { .. } => WorkspaceError::ToolFault,
    }
}

impl Inventory {
    /// Liest das aktuelle Inventar aus dem Workspace unter `root`.
    ///
    /// # Description
    /// Nutzt ausschließlich `harw_code_graph::WorkspaceGraph::load` (Member,
    /// Namen, direkte `[dependencies]`-Kanten) und
    /// `harw_code_graph::parse_lockfile` (gesperrte Versionen, inklusive
    /// transitiver Abhängigkeiten) — beide starten keinen Subprozess. Prüft
    /// `root` **vor** dem Laden und jedes gelesene Member-Manifest **nach**
    /// dem Laden gegen `scope`, jeweils erst kanonisiert (`Path::canonicalize`)
    /// und dann geprüft — derselbe "erst auflösen, dann prüfen"-Grundsatz wie
    /// [`harw_dod_cap::ReadScope::open`]: `harw_code_graph::WorkspaceGraph::load`
    /// kennt selbst keinen `ReadScope` (es ist ein allgemeines Werkzeug, kein
    /// Sensor), und ein `[workspace].members`-Eintrag mit `../`-Traversal
    /// könnte sonst ein Manifest außerhalb des erlaubten Bereichs einlesen —
    /// ein unkanonisierter Vergleich würde einen solchen Pfad fälschlich als
    /// "innerhalb" durchlassen, weil er rein lexikalisch mit `root` beginnt,
    /// bevor das `..`-Segment ihn wieder verlässt. Diese Zwei-Punkt-Prüfung
    /// stellt sicher, dass kein Ergebnis, das außerhalb von `scope` liegt,
    /// jemals in das zurückgegebene [`Inventory`] einfließt — auch wenn die
    /// zugrunde liegende Leseoperation selbst schon stattgefunden hat.
    ///
    /// # Arguments
    /// - `scope` (`&harw_dod_cap::ReadScope`): der erlaubte Lesebereich.
    /// - `root` (`&std::path::Path`): Wurzelverzeichnis des Cargo-Workspace.
    ///
    /// # Returns
    /// Das [`Inventory`] des Workspace zum Zeitpunkt des Aufrufs.
    ///
    /// # Errors
    /// - [`WorkspaceError::Io`]: `root` lässt sich nicht kanonisieren.
    /// - [`WorkspaceError::OutsideScope`]: `root` oder ein Member-Manifest
    ///   liegt außerhalb von `scope`. Nennt nie den Pfad.
    /// - [`WorkspaceError::MalformedSource`]: `Cargo.toml` oder `Cargo.lock`
    ///   **im überwachten Baum** ist syntaktisch oder strukturell unlesbar,
    ///   ein Manifest fehlt physisch, oder der Abhängigkeitsgraph ist
    ///   inkonsistent (fehlender Knoten, echter Zyklus). Nennt nie den
    ///   Dateiinhalt oder Pfad. Ein Befund über den Baum.
    /// - [`WorkspaceError::ToolFault`]: unser eigenes Ladewerkzeug
    ///   (`harw_code_graph::WorkspaceGraph::load`) konnte den Baum nicht
    ///   verarbeiten —
    ///   etwa ein von unserem Auflöser nicht unterstütztes, aber gültiges
    ///   `[workspace].members`-Glob-Muster. Kein Befund über den Baum, siehe
    ///   [`crate::error`] für die vollständige Begründung (K79-Vorfall).
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_dod_cap::ReadScope;
    /// use harw_dod_workspace::Inventory;
    /// use std::path::{Path, PathBuf};
    ///
    /// let scope = ReadScope::from_roots([PathBuf::from("/srv/workspace")]);
    /// let inventory = Inventory::read(&scope, Path::new("/srv/workspace"))?;
    /// assert!(inventory.members.is_empty() || !inventory.members.is_empty());
    /// # Ok::<(), harw_dod_workspace::error::WorkspaceError>(())
    /// ```
    pub fn read(scope: &ReadScope, root: &Path) -> Result<Self, WorkspaceError> {
        let canonical_root = root.canonicalize()?;
        if !scope.allows(&canonical_root) {
            return Err(WorkspaceError::OutsideScope);
        }

        let graph = WorkspaceGraph::load(&canonical_root).map_err(map_code_graph_error)?;

        for node in &graph.crates {
            // Kanonisieren vor der Prüfung, exakt wie `ReadScope::open`: ein
            // `[workspace].members`-Eintrag mit `../`-Traversal (z. B.
            // `"../evil"`) würde `allows()` sonst fälschlich passieren, weil
            // der unaufgelöste Pfad rein lexikalisch mit `root` beginnt, bevor
            // das `..`-Segment ihn wieder verlässt (siehe `allows`-Doku:
            // reine, syntaktische Präfixprüfung ohne Symlink-/`..`-Auflösung).
            let canonical_manifest = node.manifest_path.canonicalize()?;
            if !scope.allows(&canonical_manifest) {
                return Err(WorkspaceError::OutsideScope);
            }
        }

        let locked = parse_lockfile(&canonical_root).map_err(map_code_graph_error)?;

        let members: BTreeSet<String> = graph.crates.iter().map(|node| node.name.clone()).collect();

        let mut edges = BTreeSet::new();
        for node in &graph.crates {
            for dep in node.deps.iter().chain(node.external_deps.iter()) {
                edges.insert(Edge {
                    from: node.name.clone(),
                    to: dep.clone(),
                });
            }
        }

        let mut dependency_versions = BTreeMap::new();
        for package in &locked {
            if !members.contains(&package.name) {
                dependency_versions.insert(package.name.clone(), package.version.clone());
            }
        }

        Ok(Self {
            members,
            edges,
            dependency_versions,
        })
    }

    /// Vergleicht dieses (neuere) Inventar gegen `previous` (den Vorgänger).
    ///
    /// # Description
    /// Reine Mengendifferenz, keine Dateisystem-I/O. Die Reihenfolge der
    /// zurückgegebenen Liste ist deterministisch: erst neue Member, dann neue
    /// Kanten, dann neue Abhängigkeiten, dann entfernte Abhängigkeiten, dann
    /// Versionssprünge — innerhalb jeder Gruppe sortiert nach Name, weil
    /// `members`/`edges`/`dependency_versions` bereits geordnete Collections
    /// sind (`BTreeSet`/`BTreeMap`). Zwei Aufrufe mit identischen Eingaben
    /// liefern deshalb immer dieselbe Liste in derselben Reihenfolge.
    ///
    /// Meldet **nichts**, wenn `self == previous` — ein unveränderter
    /// Workspace erzeugt keine Meldung.
    ///
    /// # Arguments
    /// - `previous` (`&Inventory`): das zuletzt akzeptierte Inventar.
    ///
    /// # Returns
    /// Die Liste der [`StructureChange`]s, die `previous` von `self`
    /// unterscheiden. Leer, wenn beide Inventare gleich sind.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_workspace::{Inventory, StructureChange};
    ///
    /// let previous = Inventory::default();
    /// let mut current = Inventory::default();
    /// current.members.insert("harw-new-crate".to_owned());
    ///
    /// let changes = current.diff(&previous);
    /// assert_eq!(
    ///     changes,
    ///     vec![StructureChange::MemberAdded { name: "harw-new-crate".to_owned() }]
    /// );
    /// ```
    #[must_use]
    pub fn diff(&self, previous: &Inventory) -> Vec<StructureChange> {
        let mut changes = Vec::new();

        for name in self.members.difference(&previous.members) {
            changes.push(StructureChange::MemberAdded { name: name.clone() });
        }

        for edge in self.edges.difference(&previous.edges) {
            changes.push(StructureChange::EdgeAdded {
                from: edge.from.clone(),
                to: edge.to.clone(),
            });
        }

        for name in self.dependency_versions.keys() {
            if !previous.dependency_versions.contains_key(name) {
                changes.push(StructureChange::DependencyAdded { name: name.clone() });
            }
        }

        for name in previous.dependency_versions.keys() {
            if !self.dependency_versions.contains_key(name) {
                changes.push(StructureChange::DependencyRemoved { name: name.clone() });
            }
        }

        for (name, to_version) in &self.dependency_versions {
            if let Some(from_version) = previous.dependency_versions.get(name) {
                if from_version != to_version {
                    changes.push(StructureChange::VersionChanged {
                        name: name.clone(),
                        from: from_version.clone(),
                        to: to_version.clone(),
                        severity: classify_version_change(from_version, to_version),
                    });
                }
            }
        }

        changes
    }
}

/// Klassifiziert einen Versionssprung nach SemVer- bzw. Cargos
/// Pre-1.0-Kompatibilitätsregel.
///
/// # Description
/// Für Major ≥ 1 gilt Standard-SemVer (Major bricht, Minor ist additiv,
/// Patch ist additiv). Für `0.y.z` behandelt Cargo die Minor-Stelle wie
/// sonst die Major-Stelle (`^0.2.28` erlaubt `0.2.x`, aber nicht `0.3.0`);
/// für `0.0.z` gibt es überhaupt keine Kompatibilitätszusage, jede Änderung
/// zählt als [`VersionSeverity::Breaking`]. Kann keine der beiden
/// Zeichenketten als SemVer geparst werden, liefert diese Funktion
/// [`VersionSeverity::Unparseable`] statt eine Schwere zu erraten.
///
/// # Arguments
/// - `from` (`&str`): die vorherige gesperrte Version.
/// - `to` (`&str`): die neue gesperrte Version.
///
/// # Returns
/// Die [`VersionSeverity`] des Sprungs von `from` nach `to`.
///
/// # Examples
/// ```
/// use harw_dod_workspace::inventory::classify_version_change;
/// use harw_dod_workspace::VersionSeverity;
///
/// assert_eq!(classify_version_change("0.2.28", "0.2.32"), VersionSeverity::Patch);
/// assert_eq!(classify_version_change("0.2.5", "0.3.0"), VersionSeverity::Breaking);
/// assert_eq!(classify_version_change("1.4.0", "1.5.0"), VersionSeverity::Minor);
/// assert_eq!(classify_version_change("1.4.0", "2.0.0"), VersionSeverity::Breaking);
/// ```
#[must_use]
pub fn classify_version_change(from: &str, to: &str) -> VersionSeverity {
    let (Ok(from_version), Ok(to_version)) =
        (semver::Version::parse(from), semver::Version::parse(to))
    else {
        return VersionSeverity::Unparseable;
    };

    if from_version.major != to_version.major {
        // Deckt sowohl den Major-Bruch bei stabilen Crates als auch den
        // Übergang von 0.x auf >=1.0 ab.
        return VersionSeverity::Breaking;
    }

    if from_version.major == 0 {
        if from_version.minor != to_version.minor {
            return VersionSeverity::Breaking;
        }
        if from_version.minor == 0 {
            // 0.0.z: keine Kompatibilitätszusage, jede Änderung zählt.
            return VersionSeverity::Breaking;
        }
        return VersionSeverity::Patch;
    }

    if from_version.minor != to_version.minor {
        return VersionSeverity::Minor;
    }
    VersionSeverity::Patch
}

#[cfg(test)]
mod tests {
    use super::{Edge, Inventory, StructureChange, VersionSeverity, classify_version_change};
    use crate::test_support::{write_lockfile, write_member, write_root};
    use harw_dod_cap::ReadScope;
    use std::fs;
    use std::path::PathBuf;

    fn scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "harw-dod-workspace-{}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("Scratch-Verzeichnis anlegen");
        dir.canonicalize().expect("Scratch-Verzeichnis kanonisieren")
    }

    #[test]
    fn test_read_and_diff_reports_member_added_for_new_member() {
        let root = scratch_dir("member-added");
        write_root(&root, &["a"]);
        write_member(&root, "a", &[], &[]);
        write_lockfile(&root, &[("a", "0.1.0", false)]);
        let scope = ReadScope::from_roots([root.clone()]);

        let previous = Inventory::read(&scope, &root).expect("erstes Inventar lesen");

        write_root(&root, &["a", "b"]);
        write_member(&root, "b", &[], &[]);
        write_lockfile(&root, &[("a", "0.1.0", false), ("b", "0.1.0", false)]);

        let current = Inventory::read(&scope, &root).expect("zweites Inventar lesen");
        let changes = current.diff(&previous);

        assert_eq!(
            changes,
            vec![StructureChange::MemberAdded { name: "b".to_owned() }]
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_read_and_diff_reports_dependency_added() {
        let root = scratch_dir("dependency-added");
        write_root(&root, &["a"]);
        write_member(&root, "a", &[], &["serde"]);
        write_lockfile(&root, &[("a", "0.1.0", false), ("serde", "1.0.228", true)]);
        let scope = ReadScope::from_roots([root.clone()]);

        let previous = Inventory::read(&scope, &root).expect("erstes Inventar lesen");

        write_member(&root, "a", &[], &["serde", "log"]);
        write_lockfile(
            &root,
            &[
                ("a", "0.1.0", false),
                ("serde", "1.0.228", true),
                ("log", "0.4.22", true),
            ],
        );

        let current = Inventory::read(&scope, &root).expect("zweites Inventar lesen");
        let changes = current.diff(&previous);

        assert!(changes.contains(&StructureChange::DependencyAdded { name: "log".to_owned() }));
        assert!(changes.contains(&StructureChange::EdgeAdded {
            from: "a".to_owned(),
            to: "log".to_owned(),
        }));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_diff_reports_version_changed_with_correct_values_and_severity() {
        let mut previous = Inventory::default();
        previous
            .dependency_versions
            .insert("serde".to_owned(), "1.0.228".to_owned());

        let mut current = Inventory::default();
        current
            .dependency_versions
            .insert("serde".to_owned(), "1.0.229".to_owned());

        let changes = current.diff(&previous);
        assert_eq!(
            changes,
            vec![StructureChange::VersionChanged {
                name: "serde".to_owned(),
                from: "1.0.228".to_owned(),
                to: "1.0.229".to_owned(),
                severity: VersionSeverity::Patch,
            }]
        );
    }

    #[test]
    fn test_diff_reports_breaking_version_change_for_pre_1_0_minor_bump() {
        let mut previous = Inventory::default();
        previous
            .dependency_versions
            .insert("some-crate".to_owned(), "0.2.28".to_owned());

        let mut current = Inventory::default();
        current
            .dependency_versions
            .insert("some-crate".to_owned(), "0.3.0".to_owned());

        let changes = current.diff(&previous);
        assert_eq!(
            changes,
            vec![StructureChange::VersionChanged {
                name: "some-crate".to_owned(),
                from: "0.2.28".to_owned(),
                to: "0.3.0".to_owned(),
                severity: VersionSeverity::Breaking,
            }]
        );
    }

    #[test]
    fn test_read_without_predecessor_reports_no_drift() {
        // Der wichtigste Test: der erste Lauf hat per Definition kein
        // Vorgänger-Inventar. `Inventory::diff` selbst braucht immer zwei
        // konkrete Inventare — "kein Vorgänger" ist deshalb keine Eigenschaft
        // von `diff`, sondern eine Entscheidung des Aufrufers, `diff`
        // schlicht noch nicht aufzurufen (siehe `crate::sensor`-Tests für den
        // Sensor-seitigen Beleg desselben Verhaltens über `Option<Inventory>`).
        // Hier belegen wir die dafür nötige Grundlage: ein frisch gelesenes
        // Inventar gegen sich selbst (der einzig sinnvolle Standin für "es
        // gab noch keine Abweichung") meldet nichts.
        let root = scratch_dir("no-predecessor");
        write_root(&root, &["a"]);
        write_member(&root, "a", &[], &["serde"]);
        write_lockfile(&root, &[("a", "0.1.0", false), ("serde", "1.0.228", true)]);
        let scope = ReadScope::from_roots([root.clone()]);

        let current = Inventory::read(&scope, &root).expect("Inventar lesen");
        assert!(current.diff(&current).is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_diff_unchanged_workspace_reports_nothing() {
        let root = scratch_dir("unchanged");
        write_root(&root, &["a"]);
        write_member(&root, "a", &[], &["serde"]);
        write_lockfile(&root, &[("a", "0.1.0", false), ("serde", "1.0.228", true)]);
        let scope = ReadScope::from_roots([root.clone()]);

        let previous = Inventory::read(&scope, &root).expect("erstes Inventar lesen");
        let current = Inventory::read(&scope, &root).expect("zweites Inventar lesen");

        assert!(current.diff(&previous).is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_inventory_serde_roundtrip() {
        let mut inventory = Inventory::default();
        inventory.members.insert("harw-dod-workspace".to_owned());
        inventory.edges.insert(Edge {
            from: "harw-dod-workspace".to_owned(),
            to: "harw-code-graph".to_owned(),
        });
        inventory
            .dependency_versions
            .insert("serde".to_owned(), "1.0.228".to_owned());

        let json = serde_json::to_string(&inventory).expect("Inventory serialisiert");
        let round_tripped: Inventory =
            serde_json::from_str(&json).expect("Inventory deserialisiert");
        assert_eq!(inventory, round_tripped);
    }

    #[test]
    fn test_read_root_outside_scope_returns_outside_scope() {
        let root = scratch_dir("root-outside-scope");
        write_root(&root, &["a"]);
        write_member(&root, "a", &[], &[]);
        write_lockfile(&root, &[("a", "0.1.0", false)]);

        // Bereich zeigt absichtlich auf ein anderes, unbeteiligtes Verzeichnis.
        let unrelated = scratch_dir("root-outside-scope-unrelated");
        let scope = ReadScope::from_roots([unrelated.clone()]);

        let err =
            Inventory::read(&scope, &root).expect_err("Wurzel außerhalb des Bereichs muss scheitern");
        assert!(matches!(err, super::WorkspaceError::OutsideScope));

        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&unrelated).ok();
    }

    #[test]
    fn test_read_member_manifest_traversal_outside_scope_returns_outside_scope() {
        // Scope-Dichtheit: `[workspace].members = ["../evil"]` liegt lexikalisch
        // "unterhalb" der Workspace-Wurzel (Präfix-Treffer vor Auflösung), aber
        // nach Kanonisierung liegt das Ziel außerhalb. Belegt, dass `read`
        // jedes Member-Manifest erst kanonisiert und dann prüft.
        let parent = scratch_dir("traversal-parent");
        let workspace_root = parent.join("workspace_root");
        fs::create_dir_all(&workspace_root).expect("Workspace-Wurzel anlegen");
        write_root(&workspace_root, &["../evil"]);
        write_member(&parent, "evil", &[], &[]);

        let scope = ReadScope::from_roots([workspace_root.clone()]);

        let err = Inventory::read(&scope, &workspace_root)
            .expect_err("Member außerhalb des Bereichs (via ../-Traversal) muss scheitern");
        assert!(matches!(err, super::WorkspaceError::OutsideScope));

        fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn test_read_malformed_cargo_toml_returns_malformed_source_without_file_content() {
        let root = scratch_dir("malformed");
        write_root(&root, &["a"]);
        fs::create_dir_all(root.join("a")).expect("Member-Verzeichnis anlegen");
        // Absichtlich kein gültiges TOML; trägt einen eindeutigen Marker, der
        // in keiner Fehlermeldung auftauchen darf.
        fs::write(
            root.join("a").join("Cargo.toml"),
            "THIS-IS-NOT-VALID-TOML :::: UNIQUE_SECRET_MARKER_98765 [[[",
        )
        .expect("kaputtes Cargo.toml schreiben");
        let scope = ReadScope::from_roots([root.clone()]);

        let err = Inventory::read(&scope, &root).expect_err("unlesbares Manifest muss scheitern");
        assert!(matches!(err, super::WorkspaceError::MalformedSource));
        let message = err.to_string();
        assert_eq!(message, "workspace manifest or lockfile could not be parsed");
        assert!(!message.contains("UNIQUE_SECRET_MARKER_98765"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_read_unsupported_member_glob_returns_tool_fault_not_malformed_source() {
        // `harw_code_graph::resolve_member_dirs` unterstützt ausschließlich
        // das Glob-Muster `<prefix>/*`; `"a*"` ist syntaktisch gültiges Cargo,
        // aber von unserem Auflöser nicht unterstützt — eine
        // Fähigkeitslücke unseres Werkzeugs, kein Befund über den Baum. Der
        // wichtigste Test dieser Reparatur: ein Parserfehler darf nicht als
        // `MalformedSource` erscheinen.
        let root = scratch_dir("unsupported-glob");
        fs::create_dir_all(&root).expect("Workspace-Wurzel anlegen");
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"a*\"]\n",
        )
        .expect("Wurzel-Cargo.toml mit nicht unterstütztem Glob schreiben");
        let scope = ReadScope::from_roots([root.clone()]);

        let err = Inventory::read(&scope, &root)
            .expect_err("nicht unterstütztes Glob-Muster muss scheitern");
        assert!(
            matches!(err, super::WorkspaceError::ToolFault),
            "ein Werkzeugfehler darf nicht als MalformedSource erscheinen: {err:?}"
        );
        assert!(!matches!(err, super::WorkspaceError::MalformedSource));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_read_unsupported_member_glob_message_is_content_free() {
        let root = scratch_dir("unsupported-glob-message");
        fs::create_dir_all(&root).expect("Workspace-Wurzel anlegen");
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"a*\"]\n",
        )
        .expect("Wurzel-Cargo.toml mit nicht unterstütztem Glob schreiben");
        let scope = ReadScope::from_roots([root.clone()]);

        let err = Inventory::read(&scope, &root)
            .expect_err("nicht unterstütztes Glob-Muster muss scheitern");
        let message = err.to_string();
        assert_eq!(message, "workspace tooling could not process the source");
        assert!(!message.contains("a*"));
        assert!(!message.contains(&root.display().to_string()));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_read_missing_workspace_section_returns_malformed_source() {
        // Ein Baumbefund: das Wurzel-Manifest existiert und ist gültiges
        // TOML, hat aber schlicht keinen `[workspace]`-Abschnitt — eine
        // reine Vorhandensein-Prüfung, unabhängig von unserem Parser-Schema.
        let root = scratch_dir("no-workspace-section");
        fs::create_dir_all(&root).expect("Workspace-Wurzel anlegen");
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"not-a-workspace\"\nversion = \"0.1.0\"\n",
        )
        .expect("Wurzel-Cargo.toml ohne [workspace] schreiben");
        let scope = ReadScope::from_roots([root.clone()]);

        let err = Inventory::read(&scope, &root)
            .expect_err("fehlender [workspace]-Abschnitt muss scheitern");
        assert!(matches!(err, super::WorkspaceError::MalformedSource));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_classify_version_change_patch_for_pre_1_0_patch_bump() {
        assert_eq!(
            classify_version_change("0.2.28", "0.2.32"),
            VersionSeverity::Patch
        );
    }

    #[test]
    fn test_classify_version_change_breaking_for_pre_1_0_minor_bump() {
        assert_eq!(
            classify_version_change("0.2.5", "0.3.0"),
            VersionSeverity::Breaking
        );
    }

    #[test]
    fn test_classify_version_change_breaking_for_zero_zero_z_patch() {
        assert_eq!(
            classify_version_change("0.0.1", "0.0.2"),
            VersionSeverity::Breaking
        );
    }

    #[test]
    fn test_classify_version_change_minor_for_stable_minor_bump() {
        assert_eq!(
            classify_version_change("1.4.0", "1.5.0"),
            VersionSeverity::Minor
        );
    }

    #[test]
    fn test_classify_version_change_patch_for_stable_patch_bump() {
        assert_eq!(
            classify_version_change("1.4.0", "1.4.1"),
            VersionSeverity::Patch
        );
    }

    #[test]
    fn test_classify_version_change_breaking_for_stable_major_bump() {
        assert_eq!(
            classify_version_change("1.4.0", "2.0.0"),
            VersionSeverity::Breaking
        );
    }

    #[test]
    fn test_classify_version_change_breaking_for_leaving_pre_1_0() {
        assert_eq!(
            classify_version_change("0.9.0", "1.0.0"),
            VersionSeverity::Breaking
        );
    }

    #[test]
    fn test_classify_version_change_unparseable_for_invalid_semver() {
        assert_eq!(
            classify_version_change("not-a-version", "1.0.0"),
            VersionSeverity::Unparseable
        );
    }
}
