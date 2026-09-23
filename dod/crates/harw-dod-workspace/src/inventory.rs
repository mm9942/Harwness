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
//! - `dependency_versions`: **alle** gleichzeitig gesperrten Versionen jeder
//!   Nicht-Member-Abhängigkeit aus `Cargo.lock`, je Version mit Herkunft
//!   (`source`) und Prüfsumme (`checksum`). `Cargo.lock` enthält den
//!   **gesamten** transitiven Abhängigkeitsbaum, nicht nur direkte
//!   Abhängigkeiten — genau das macht diesen Sensor nützlich, denn laut
//!   Aufgabenstellung ist eine neue *transitive* Abhängigkeit der häufigste
//!   Weg, auf dem fremder Code in ein Projekt gelangt.
//!
//! # F-096/F-097: warum eine Version allein nicht reicht (Register
//! `x-findings-register-w1-w3.md`)
//! Vor dieser Korrektur war dieses Feld `BTreeMap<String, String>` — ein
//! einziger Versions-String je Abhängigkeitsname. Das hatte zwei blinde
//! Flecken:
//! - **F-097**: `Cargo.lock` kann *mehrere* Versionen derselben Abhängigkeit
//!   gleichzeitig sperren (unversöhnliche SemVer-Anforderungen im
//!   Abhängigkeitsbaum sind der Normalfall, kein Fehlerzustand). Eine
//!   `BTreeMap<name, version>` behält beim Einfügen nur die zuletzt
//!   gesehene — jede zusätzliche, unter Umständen ältere und verwundbare
//!   Version verschwand kommentarlos.
//! - **F-096**: `harw_code_graph::LockedPackage` trägt `source` und
//!   `checksum`, aber nur `version` wanderte je Eintrag ins Inventar. Ein
//!   Wechsel der Paketquelle (Registry → Git) oder eine geänderte Prüfsumme
//!   **bei unveränderter Versionsnummer** blieb dadurch für [`Inventory::diff`]
//!   unsichtbar — genau die Form, in der eine kompromittierte oder
//!   nachträglich ausgetauschte Abhängigkeit auffiele, ohne dass die
//!   Versionszeile sich ändert.
//!
//! `dependency_versions` ist deshalb jetzt `BTreeMap<String,
//! BTreeSet<LockedDependency>>`: je Abhängigkeitsname die vollständige Menge
//! ihrer gleichzeitig gesperrten `(version, source, checksum)`-Tripel. Siehe
//! [`diff_versions`] für die daraus abgeleiteten [`StructureChange`]-Formen
//! ([`StructureChange::DependencyVersionAdded`],
//! [`StructureChange::DependencyVersionRemoved`],
//! [`StructureChange::ProvenanceChanged`]).
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

/// Ein einzelner gesperrter Versionseintrag einer Nicht-Member-Abhängigkeit,
/// mit Herkunft und Prüfsumme (F-096, F-097).
///
/// # Description
/// Entspricht 1:1 einem `harw_code_graph::LockedPackage`-Eintrag ohne dessen
/// `name` (der Name ist bereits der Schlüssel in
/// [`Inventory::dependency_versions`]). `Ord` sortiert zuerst nach `version`,
/// damit [`Inventory::dependency_versions`] (`BTreeSet<LockedDependency>` je
/// Name) versionsaufsteigend iteriert — die Reihenfolge ist rein für
/// Determinismus gedacht, keine SemVer-Sortierung.
///
/// # Warum ein `BTreeSet` statt einer `BTreeMap<Version, (Source, Checksum)>`
/// `Cargo.lock` schließt zwei Einträge mit identischem `(name, version)` aber
/// unterschiedlicher `source`/`checksum` nicht grundsätzlich aus (z. B. ein
/// Registry- und ein Git-Eintrag mit zufällig gleicher Versionsnummer). Ein
/// `BTreeSet<LockedDependency>` bildet auch diesen Randfall verlustfrei ab,
/// statt einen der beiden Einträge beim Einfügen stillschweigend zu
/// überschreiben — exakt der Fehler, den diese Korrektur behebt.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedDependency {
    /// Die gesperrte Version.
    pub version: String,
    /// Die Herkunft (z. B. `registry+https://...` oder ein Git-Verweis);
    /// `None` bei Path-Abhängigkeiten. Aus `harw_code_graph::LockedPackage::source`.
    pub source: Option<String>,
    /// Die Prüfsumme, falls im Lockfile vorhanden. Aus
    /// `harw_code_graph::LockedPackage::checksum`.
    pub checksum: Option<String>,
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
/// Von [`Inventory::diff`] erzeugt. Neben den ursprünglichen Drift-Formen
/// (neue Abhängigkeit, einfacher Versionssprung, neue Kante, neues Member,
/// entfernte Abhängigkeit) trägt dieser Typ seit F-096/F-097 drei weitere
/// Varianten für Fälle, die eine einzelne `(name, version)`-Zeile nicht
/// abbilden kann: [`Self::DependencyVersionAdded`]/
/// [`Self::DependencyVersionRemoved`] für zusätzliche, gleichzeitig gesperrte
/// Versionen derselben Abhängigkeit, und [`Self::ProvenanceChanged`] für eine
/// geänderte Herkunft/Prüfsumme **bei unveränderter** Version. Siehe
/// [`diff_versions`] für die genaue Abgrenzung zu [`Self::VersionChanged`].
///
/// Es gibt bewusst **kein** `MemberRemoved` und kein `EdgeRemoved`: das
/// Verschwinden eines Members oder einer Kante ist keine neue Angriffsfläche
/// und damit kein Sicherheitssignal in dem Sinn, den dieser Sensor beobachtet
/// (Verkleinerung der Fläche ist nie das Risiko).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "change")]
pub enum StructureChange {
    /// Eine neue (Nicht-Member-)Abhängigkeit ist im Workspace aufgetaucht.
    DependencyAdded {
        /// Name der neu aufgetauchten Abhängigkeit.
        name: String,
    },
    /// Eine zuvor vorhandene (Nicht-Member-)Abhängigkeit ist vollständig
    /// verschwunden (keine ihrer Versionen ist mehr gesperrt).
    DependencyRemoved {
        /// Name der entfernten Abhängigkeit.
        name: String,
    },
    /// Die einzige gesperrte Version einer bereits bekannten Abhängigkeit hat
    /// sich geändert: genau eine Version vorher, genau eine Version danach,
    /// beide verschieden. Der häufigste, "einfache" Fall eines
    /// Versions-Bumps.
    ///
    /// Trägt bewusst keine `source`/`checksum`-Felder — ein gleichzeitiger
    /// Wechsel von Version **und** Herkunft bei ansonsten unverändertem
    /// Bestand ist eine offene Annahme dieser Korrektur, siehe Ledger
    /// `docs/remediation/ledger/W5/D-SEC.md`, Abschnitt „Offene Annahmen".
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
    /// Eine zusätzliche Version einer bereits bekannten Abhängigkeit ist
    /// **gleichzeitig** mit mindestens einer weiteren Version dieser
    /// Abhängigkeit gesperrt (F-097) — z. B. eine ältere, potenziell
    /// verwundbare Version, die neben der aktuell genutzten im
    /// Abhängigkeitsbaum verbleibt. Auch der Fall, in dem eine von mehreren
    /// bereits gleichzeitig gesperrten Versionen durch eine andere ersetzt
    /// wird, erzeugt dies (statt [`Self::VersionChanged`]) — siehe
    /// [`diff_versions`] für die exakte Abgrenzung.
    DependencyVersionAdded {
        /// Name der betroffenen Abhängigkeit.
        name: String,
        /// Die neu aufgetauchte, zusätzlich gesperrte Version.
        version: String,
        /// Herkunft dieser Version, siehe [`LockedDependency::source`].
        source: Option<String>,
        /// Prüfsumme dieser Version, siehe [`LockedDependency::checksum`].
        checksum: Option<String>,
    },
    /// Eine von mehreren gleichzeitig gesperrten Versionen einer
    /// Abhängigkeit ist verschwunden, während mindestens eine andere Version
    /// derselben Abhängigkeit weiterhin gesperrt bleibt (symmetrisch zu
    /// [`Self::DependencyVersionAdded`]).
    DependencyVersionRemoved {
        /// Name der betroffenen Abhängigkeit.
        name: String,
        /// Die verschwundene Version.
        version: String,
    },
    /// Herkunft und/oder Prüfsumme einer Abhängigkeit haben sich geändert,
    /// **ohne** dass sich ihre Versionsnummer geändert hat (F-096) — z. B.
    /// ein Wechsel der Paketquelle von Registry auf Git, oder eine geänderte
    /// Prüfsumme bei einer nachträglich ausgetauschten Version.
    ProvenanceChanged {
        /// Name der betroffenen Abhängigkeit.
        name: String,
        /// Die unverändert gebliebene Version, bei der sich Herkunft
        /// und/oder Prüfsumme geändert haben.
        version: String,
        /// Vorherige Herkunft.
        from_source: Option<String>,
        /// Neue Herkunft.
        to_source: Option<String>,
        /// Vorherige Prüfsumme.
        from_checksum: Option<String>,
        /// Neue Prüfsumme.
        to_checksum: Option<String>,
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
    /// Alle gleichzeitig gesperrten Versionen je Nicht-Member-Abhängigkeit,
    /// aus `Cargo.lock` (F-096, F-097). Siehe Moduldoku, Abschnitt
    /// „F-096/F-097" für die Begründung, warum eine einzelne Version je Name
    /// nicht ausreicht.
    pub dependency_versions: BTreeMap<String, BTreeSet<LockedDependency>>,
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

        // F-096/F-097: `.entry(..).or_default().insert(..)` statt
        // `.insert(name, version)` — jede gleichzeitig gesperrte Version
        // derselben Abhängigkeit bleibt erhalten (BTreeSet dedupliziert nur
        // exakt gleiche `(version, source, checksum)`-Tripel), und Herkunft
        // sowie Prüfsumme wandern mit ins Inventar statt verworfen zu werden.
        let mut dependency_versions: BTreeMap<String, BTreeSet<LockedDependency>> = BTreeMap::new();
        for package in &locked {
            if !members.contains(&package.name) {
                dependency_versions
                    .entry(package.name.clone())
                    .or_default()
                    .insert(LockedDependency {
                        version: package.version.clone(),
                        source: package.source.clone(),
                        checksum: package.checksum.clone(),
                    });
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

        // Nur Namen, die in **beiden** Ständen vorkommen — ein komplett
        // neuer oder komplett verschwundener Name ist bereits oben als
        // `DependencyAdded`/`DependencyRemoved` gemeldet; `diff_versions`
        // würde für ihn nur redundante `DependencyVersionAdded`/`-Removed`
        // je Einzelversion erzeugen.
        for (name, current_versions) in &self.dependency_versions {
            if let Some(previous_versions) = previous.dependency_versions.get(name) {
                changes.extend(diff_versions(name, previous_versions, current_versions));
            }
        }

        changes
    }
}

/// Vergleicht die Menge der gleichzeitig gesperrten Versionen **einer**
/// Abhängigkeit zwischen zwei [`Inventory`]-Ständen (F-096, F-097).
///
/// # Description
/// Drei sich gegenseitig ausschließende Fälle, in dieser Prüfreihenfolge:
///
/// 1. **Identisch** (`previous == current`): keine Meldung.
/// 2. **Einfacher Ersatz**: vorher genau eine Version, nachher genau eine
///    andere Version — der Normalfall eines Versions-Bumps. Erzeugt genau
///    ein [`StructureChange::VersionChanged`] mit der aus
///    [`classify_version_change`] abgeleiteten Schwere.
/// 3. **Alles andere** (mehr als eine Version gleichzeitig gesperrt, auf
///    einer der beiden Seiten oder beiden): je verschwundener Version ein
///    [`StructureChange::DependencyVersionRemoved`], je neu aufgetauchter
///    Version ein [`StructureChange::DependencyVersionAdded`]. Das deckt
///    sowohl F-097 (eine zusätzliche, weiterhin gleichzeitig gesperrte
///    Version) als auch den Ersatz einer von mehreren Versionen ab.
///
/// Unabhängig davon: für jede Version, die in **beiden** Ständen mit
/// identischer Versionsnummer, aber unterschiedlicher `source` und/oder
/// `checksum` vorkommt, wird zusätzlich ein
/// [`StructureChange::ProvenanceChanged`] erzeugt (F-096). Das ist
/// orthogonal zu den drei Fällen oben, da eine Provenienzänderung auch bei
/// unverändertem Versionsbestand auftreten kann.
///
/// # Arguments
/// - `name` (`&str`): Name der verglichenen Abhängigkeit.
/// - `previous` (`&BTreeSet<LockedDependency>`): ihre Versionen im
///   Vorgänger-Inventar.
/// - `current` (`&BTreeSet<LockedDependency>`): ihre Versionen im aktuellen
///   Inventar.
///
/// # Returns
/// Die abgeleiteten [`StructureChange`]s, in der oben genannten Reihenfolge
/// (Fall 2 oder 3, dann Provenienzänderungen). Leer, wenn `previous ==
/// current`.
fn diff_versions(
    name: &str,
    previous: &BTreeSet<LockedDependency>,
    current: &BTreeSet<LockedDependency>,
) -> Vec<StructureChange> {
    if previous == current {
        return Vec::new();
    }

    let previous_by_version: BTreeMap<&str, &LockedDependency> = previous
        .iter()
        .map(|dep| (dep.version.as_str(), dep))
        .collect();
    let current_by_version: BTreeMap<&str, &LockedDependency> = current
        .iter()
        .map(|dep| (dep.version.as_str(), dep))
        .collect();

    let added_versions: Vec<&str> = current_by_version
        .keys()
        .filter(|version| !previous_by_version.contains_key(*version))
        .copied()
        .collect();
    let removed_versions: Vec<&str> = previous_by_version
        .keys()
        .filter(|version| !current_by_version.contains_key(*version))
        .copied()
        .collect();

    let mut changes = Vec::new();

    let is_simple_replace = previous_by_version.len() == 1
        && current_by_version.len() == 1
        && added_versions.len() == 1
        && removed_versions.len() == 1;

    if is_simple_replace {
        let from = removed_versions[0];
        let to = added_versions[0];
        changes.push(StructureChange::VersionChanged {
            name: name.to_owned(),
            from: from.to_owned(),
            to: to.to_owned(),
            severity: classify_version_change(from, to),
        });
    } else {
        for version in &removed_versions {
            changes.push(StructureChange::DependencyVersionRemoved {
                name: name.to_owned(),
                version: (*version).to_owned(),
            });
        }
        // Über `current` selbst statt über den Index `current_by_version[..]`
        // gesucht — keine Panik möglich, falls das Invariant "jede
        // `added_versions`-Version steht in `current_by_version`" je verletzt
        // würde (siehe Projektregel: keine `unwrap()`/`expect()`/Index-Panik
        // in Produktionspfaden).
        for dep in current
            .iter()
            .filter(|dep| added_versions.contains(&dep.version.as_str()))
        {
            changes.push(StructureChange::DependencyVersionAdded {
                name: name.to_owned(),
                version: dep.version.clone(),
                source: dep.source.clone(),
                checksum: dep.checksum.clone(),
            });
        }
    }

    for (version, current_dep) in &current_by_version {
        if let Some(previous_dep) = previous_by_version.get(version) {
            if previous_dep.source != current_dep.source
                || previous_dep.checksum != current_dep.checksum
            {
                changes.push(StructureChange::ProvenanceChanged {
                    name: name.to_owned(),
                    version: (*version).to_owned(),
                    from_source: previous_dep.source.clone(),
                    to_source: current_dep.source.clone(),
                    from_checksum: previous_dep.checksum.clone(),
                    to_checksum: current_dep.checksum.clone(),
                });
            }
        }
    }

    changes
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
    use super::{
        Edge, Inventory, LockedDependency, StructureChange, VersionSeverity,
        classify_version_change,
    };
    use crate::test_support::{
        TestError, TestResult, ctx, write_lockfile, write_lockfile_with_metadata, write_member,
        write_root,
    };
    use harw_dod_cap::ReadScope;
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;

    /// Baut einen einzelnen, quellenlosen `LockedDependency`-Eintrag — der
    /// häufigste Testfall (nur die Versionsnummer ist relevant).
    fn single_version(version: &str) -> BTreeSet<LockedDependency> {
        BTreeSet::from([LockedDependency {
            version: version.to_owned(),
            source: None,
            checksum: None,
        }])
    }

    fn scratch_dir(label: &str) -> TestResult<PathBuf> {
        let dir =
            std::env::temp_dir().join(format!("harw-dod-workspace-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).map_err(ctx("Scratch-Verzeichnis anlegen"))?;
        dir.canonicalize()
            .map_err(ctx("Scratch-Verzeichnis kanonisieren"))
    }

    #[test]
    fn test_read_and_diff_reports_member_added_for_new_member() -> TestResult {
        let root = scratch_dir("member-added")?;
        write_root(&root, &["a"])?;
        write_member(&root, "a", &[], &[])?;
        write_lockfile(&root, &[("a", "0.1.0", false)])?;
        let scope = ReadScope::from_roots([root.clone()]);

        let previous = Inventory::read(&scope, &root).map_err(ctx("erstes Inventar lesen"))?;

        write_root(&root, &["a", "b"])?;
        write_member(&root, "b", &[], &[])?;
        write_lockfile(&root, &[("a", "0.1.0", false), ("b", "0.1.0", false)])?;

        let current = Inventory::read(&scope, &root).map_err(ctx("zweites Inventar lesen"))?;
        let changes = current.diff(&previous);

        assert_eq!(
            changes,
            vec![StructureChange::MemberAdded {
                name: "b".to_owned()
            }]
        );

        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[test]
    fn test_read_and_diff_reports_dependency_added() -> TestResult {
        let root = scratch_dir("dependency-added")?;
        write_root(&root, &["a"])?;
        write_member(&root, "a", &[], &["serde"])?;
        write_lockfile(&root, &[("a", "0.1.0", false), ("serde", "1.0.228", true)])?;
        let scope = ReadScope::from_roots([root.clone()]);

        let previous = Inventory::read(&scope, &root).map_err(ctx("erstes Inventar lesen"))?;

        write_member(&root, "a", &[], &["serde", "log"])?;
        write_lockfile(
            &root,
            &[
                ("a", "0.1.0", false),
                ("serde", "1.0.228", true),
                ("log", "0.4.22", true),
            ],
        )?;

        let current = Inventory::read(&scope, &root).map_err(ctx("zweites Inventar lesen"))?;
        let changes = current.diff(&previous);

        assert!(changes.contains(&StructureChange::DependencyAdded {
            name: "log".to_owned()
        }));
        assert!(changes.contains(&StructureChange::EdgeAdded {
            from: "a".to_owned(),
            to: "log".to_owned(),
        }));

        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[test]
    fn test_diff_reports_version_changed_with_correct_values_and_severity() {
        let mut previous = Inventory::default();
        previous
            .dependency_versions
            .insert("serde".to_owned(), single_version("1.0.228"));

        let mut current = Inventory::default();
        current
            .dependency_versions
            .insert("serde".to_owned(), single_version("1.0.229"));

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
            .insert("some-crate".to_owned(), single_version("0.2.28"));

        let mut current = Inventory::default();
        current
            .dependency_versions
            .insert("some-crate".to_owned(), single_version("0.3.0"));

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
    fn test_read_without_predecessor_reports_no_drift() -> TestResult {
        // Der wichtigste Test: der erste Lauf hat per Definition kein
        // Vorgänger-Inventar. `Inventory::diff` selbst braucht immer zwei
        // konkrete Inventare — "kein Vorgänger" ist deshalb keine Eigenschaft
        // von `diff`, sondern eine Entscheidung des Aufrufers, `diff`
        // schlicht noch nicht aufzurufen (siehe `crate::sensor`-Tests für den
        // Sensor-seitigen Beleg desselben Verhaltens über `Option<Inventory>`).
        // Hier belegen wir die dafür nötige Grundlage: ein frisch gelesenes
        // Inventar gegen sich selbst (der einzig sinnvolle Standin für "es
        // gab noch keine Abweichung") meldet nichts.
        let root = scratch_dir("no-predecessor")?;
        write_root(&root, &["a"])?;
        write_member(&root, "a", &[], &["serde"])?;
        write_lockfile(&root, &[("a", "0.1.0", false), ("serde", "1.0.228", true)])?;
        let scope = ReadScope::from_roots([root.clone()]);

        let current = Inventory::read(&scope, &root).map_err(ctx("Inventar lesen"))?;
        assert!(current.diff(&current).is_empty());

        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[test]
    fn test_diff_unchanged_workspace_reports_nothing() -> TestResult {
        let root = scratch_dir("unchanged")?;
        write_root(&root, &["a"])?;
        write_member(&root, "a", &[], &["serde"])?;
        write_lockfile(&root, &[("a", "0.1.0", false), ("serde", "1.0.228", true)])?;
        let scope = ReadScope::from_roots([root.clone()]);

        let previous = Inventory::read(&scope, &root).map_err(ctx("erstes Inventar lesen"))?;
        let current = Inventory::read(&scope, &root).map_err(ctx("zweites Inventar lesen"))?;

        assert!(current.diff(&previous).is_empty());

        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[test]
    fn test_inventory_serde_roundtrip() -> TestResult {
        let mut inventory = Inventory::default();
        inventory.members.insert("harw-dod-workspace".to_owned());
        inventory.edges.insert(Edge {
            from: "harw-dod-workspace".to_owned(),
            to: "harw-code-graph".to_owned(),
        });
        inventory.dependency_versions.insert(
            "serde".to_owned(),
            BTreeSet::from([LockedDependency {
                version: "1.0.228".to_owned(),
                source: Some("registry+https://github.com/rust-lang/crates.io-index".to_owned()),
                checksum: Some("abc123".to_owned()),
            }]),
        );

        let json = serde_json::to_string(&inventory).map_err(ctx("Inventory serialisiert"))?;
        let round_tripped: Inventory =
            serde_json::from_str(&json).map_err(ctx("Inventory deserialisiert"))?;
        assert_eq!(inventory, round_tripped);
        Ok(())
    }

    #[test]
    fn test_read_root_outside_scope_returns_outside_scope() -> TestResult {
        let root = scratch_dir("root-outside-scope")?;
        write_root(&root, &["a"])?;
        write_member(&root, "a", &[], &[])?;
        write_lockfile(&root, &[("a", "0.1.0", false)])?;

        // Bereich zeigt absichtlich auf ein anderes, unbeteiligtes Verzeichnis.
        let unrelated = scratch_dir("root-outside-scope-unrelated")?;
        let scope = ReadScope::from_roots([unrelated.clone()]);

        let Err(err) = Inventory::read(&scope, &root) else {
            return Err(TestError::Unexpected(
                "Wurzel außerhalb des Bereichs muss scheitern".to_owned(),
            ));
        };
        assert!(matches!(err, super::WorkspaceError::OutsideScope));

        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&unrelated).ok();
        Ok(())
    }

    #[test]
    fn test_read_member_manifest_traversal_outside_scope_returns_outside_scope() -> TestResult {
        // Scope-Dichtheit: `[workspace].members = ["../evil"]` liegt lexikalisch
        // "unterhalb" der Workspace-Wurzel (Präfix-Treffer vor Auflösung), aber
        // nach Kanonisierung liegt das Ziel außerhalb. Belegt, dass `read`
        // jedes Member-Manifest erst kanonisiert und dann prüft.
        let parent = scratch_dir("traversal-parent")?;
        let workspace_root = parent.join("workspace_root");
        fs::create_dir_all(&workspace_root).map_err(ctx("Workspace-Wurzel anlegen"))?;
        write_root(&workspace_root, &["../evil"])?;
        write_member(&parent, "evil", &[], &[])?;

        let scope = ReadScope::from_roots([workspace_root.clone()]);

        let Err(err) = Inventory::read(&scope, &workspace_root) else {
            return Err(TestError::Unexpected(
                "Member außerhalb des Bereichs (via ../-Traversal) muss scheitern".to_owned(),
            ));
        };
        assert!(matches!(err, super::WorkspaceError::OutsideScope));

        fs::remove_dir_all(&parent).ok();
        Ok(())
    }

    #[test]
    fn test_read_malformed_cargo_toml_returns_malformed_source_without_file_content() -> TestResult
    {
        let root = scratch_dir("malformed")?;
        write_root(&root, &["a"])?;
        fs::create_dir_all(root.join("a")).map_err(ctx("Member-Verzeichnis anlegen"))?;
        // Absichtlich kein gültiges TOML; trägt einen eindeutigen Marker, der
        // in keiner Fehlermeldung auftauchen darf.
        fs::write(
            root.join("a").join("Cargo.toml"),
            "THIS-IS-NOT-VALID-TOML :::: UNIQUE_SECRET_MARKER_98765 [[[",
        )
        .map_err(ctx("kaputtes Cargo.toml schreiben"))?;
        let scope = ReadScope::from_roots([root.clone()]);

        let Err(err) = Inventory::read(&scope, &root) else {
            return Err(TestError::Unexpected(
                "unlesbares Manifest muss scheitern".to_owned(),
            ));
        };
        assert!(matches!(err, super::WorkspaceError::MalformedSource));
        let message = err.to_string();
        assert_eq!(
            message,
            "workspace manifest or lockfile could not be parsed"
        );
        assert!(!message.contains("UNIQUE_SECRET_MARKER_98765"));

        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[test]
    fn test_read_unsupported_member_glob_returns_tool_fault_not_malformed_source() -> TestResult {
        // `harw_code_graph::resolve_member_dirs` unterstützt ausschließlich
        // das Glob-Muster `<prefix>/*`; `"a*"` ist syntaktisch gültiges Cargo,
        // aber von unserem Auflöser nicht unterstützt — eine
        // Fähigkeitslücke unseres Werkzeugs, kein Befund über den Baum. Der
        // wichtigste Test dieser Reparatur: ein Parserfehler darf nicht als
        // `MalformedSource` erscheinen.
        let root = scratch_dir("unsupported-glob")?;
        fs::create_dir_all(&root).map_err(ctx("Workspace-Wurzel anlegen"))?;
        fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = [\"a*\"]\n").map_err(ctx(
            "Wurzel-Cargo.toml mit nicht unterstütztem Glob schreiben",
        ))?;
        let scope = ReadScope::from_roots([root.clone()]);

        let Err(err) = Inventory::read(&scope, &root) else {
            return Err(TestError::Unexpected(
                "nicht unterstütztes Glob-Muster muss scheitern".to_owned(),
            ));
        };
        assert!(
            matches!(err, super::WorkspaceError::ToolFault),
            "ein Werkzeugfehler darf nicht als MalformedSource erscheinen: {err:?}"
        );
        assert!(!matches!(err, super::WorkspaceError::MalformedSource));

        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[test]
    fn test_read_unsupported_member_glob_message_is_content_free() -> TestResult {
        let root = scratch_dir("unsupported-glob-message")?;
        fs::create_dir_all(&root).map_err(ctx("Workspace-Wurzel anlegen"))?;
        fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = [\"a*\"]\n").map_err(ctx(
            "Wurzel-Cargo.toml mit nicht unterstütztem Glob schreiben",
        ))?;
        let scope = ReadScope::from_roots([root.clone()]);

        let Err(err) = Inventory::read(&scope, &root) else {
            return Err(TestError::Unexpected(
                "nicht unterstütztes Glob-Muster muss scheitern".to_owned(),
            ));
        };
        let message = err.to_string();
        assert_eq!(message, "workspace tooling could not process the source");
        assert!(!message.contains("a*"));
        assert!(!message.contains(&root.display().to_string()));

        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[test]
    fn test_read_missing_workspace_section_returns_malformed_source() -> TestResult {
        // Ein Baumbefund: das Wurzel-Manifest existiert und ist gültiges
        // TOML, hat aber schlicht keinen `[workspace]`-Abschnitt — eine
        // reine Vorhandensein-Prüfung, unabhängig von unserem Parser-Schema.
        let root = scratch_dir("no-workspace-section")?;
        fs::create_dir_all(&root).map_err(ctx("Workspace-Wurzel anlegen"))?;
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"not-a-workspace\"\nversion = \"0.1.0\"\n",
        )
        .map_err(ctx("Wurzel-Cargo.toml ohne [workspace] schreiben"))?;
        let scope = ReadScope::from_roots([root.clone()]);

        let Err(err) = Inventory::read(&scope, &root) else {
            return Err(TestError::Unexpected(
                "fehlender [workspace]-Abschnitt muss scheitern".to_owned(),
            ));
        };
        assert!(matches!(err, super::WorkspaceError::MalformedSource));

        fs::remove_dir_all(&root).ok();
        Ok(())
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

    // --- F-096/F-097: mehrere gleichzeitig gesperrte Versionen + Herkunft/Prüfsumme ---

    /// F-097, Kernbeleg: `Cargo.lock` sperrt `libc` gleichzeitig in zwei
    /// Versionen (unversöhnliche SemVer-Anforderungen im Baum) — vor dieser
    /// Korrektur hätte `BTreeMap<name, version>` eine davon stillschweigend
    /// überschrieben.
    #[test]
    fn test_read_keeps_all_simultaneously_locked_versions_of_same_dependency_f097() -> TestResult {
        let root = scratch_dir("multi-version")?;
        write_root(&root, &["a"])?;
        write_member(&root, "a", &[], &["libc"])?;
        write_lockfile_with_metadata(
            &root,
            &[
                ("a", "0.1.0", None, None),
                (
                    "libc",
                    "0.2.150",
                    Some("registry+https://github.com/rust-lang/crates.io-index"),
                    Some("aaa"),
                ),
                (
                    "libc",
                    "0.2.140",
                    Some("registry+https://github.com/rust-lang/crates.io-index"),
                    Some("bbb"),
                ),
            ],
        )?;
        let scope = ReadScope::from_roots([root.clone()]);

        let inventory = Inventory::read(&scope, &root).map_err(ctx("Inventar lesen"))?;

        let libc_versions = inventory
            .dependency_versions
            .get("libc")
            .ok_or(TestError::Missing("libc muss im Inventar auftauchen"))?;
        assert_eq!(
            libc_versions.len(),
            2,
            "beide gleichzeitig gesperrten Versionen müssen erhalten bleiben, keine darf verschwinden"
        );
        let version_strings: BTreeSet<&str> = libc_versions
            .iter()
            .map(|dep| dep.version.as_str())
            .collect();
        assert!(version_strings.contains("0.2.150"));
        assert!(version_strings.contains("0.2.140"));

        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    /// F-096, Kernbeleg: `source` und `checksum` aus `Cargo.lock` landen im
    /// Inventar, statt wie vor dieser Korrektur verworfen zu werden.
    #[test]
    fn test_read_captures_source_and_checksum_f096() -> TestResult {
        let root = scratch_dir("source-checksum")?;
        write_root(&root, &["a"])?;
        write_member(&root, "a", &[], &["serde"])?;
        write_lockfile_with_metadata(
            &root,
            &[
                ("a", "0.1.0", None, None),
                (
                    "serde",
                    "1.0.228",
                    Some("registry+https://github.com/rust-lang/crates.io-index"),
                    Some("deadbeef"),
                ),
            ],
        )?;
        let scope = ReadScope::from_roots([root.clone()]);

        let inventory = Inventory::read(&scope, &root).map_err(ctx("Inventar lesen"))?;

        let serde_versions = inventory
            .dependency_versions
            .get("serde")
            .ok_or(TestError::Missing("serde muss im Inventar auftauchen"))?;
        let entry = serde_versions
            .iter()
            .next()
            .ok_or(TestError::Missing("genau ein Eintrag erwartet"))?;
        assert_eq!(
            entry.source.as_deref(),
            Some("registry+https://github.com/rust-lang/crates.io-index")
        );
        assert_eq!(entry.checksum.as_deref(), Some("deadbeef"));

        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    /// F-097: eine zusätzliche, gleichzeitig gesperrte Version einer bereits
    /// bekannten Abhängigkeit erzeugt `DependencyVersionAdded`, **nicht**
    /// `VersionChanged` (das würde implizieren, die alte Version sei
    /// verschwunden — sie ist es nicht).
    #[test]
    fn test_diff_reports_dependency_version_added_when_additional_version_appears_f097() {
        let mut previous = Inventory::default();
        previous
            .dependency_versions
            .insert("libc".to_owned(), single_version("0.2.150"));

        let mut current = Inventory::default();
        current.dependency_versions.insert(
            "libc".to_owned(),
            BTreeSet::from([
                LockedDependency {
                    version: "0.2.150".to_owned(),
                    source: None,
                    checksum: None,
                },
                LockedDependency {
                    version: "0.2.140".to_owned(),
                    source: None,
                    checksum: None,
                },
            ]),
        );

        let changes = current.diff(&previous);
        assert_eq!(
            changes,
            vec![StructureChange::DependencyVersionAdded {
                name: "libc".to_owned(),
                version: "0.2.140".to_owned(),
                source: None,
                checksum: None,
            }]
        );
    }

    /// Symmetrischer Fall: eine von mehreren gleichzeitig gesperrten
    /// Versionen verschwindet, mindestens eine andere bleibt — das ist
    /// `DependencyVersionRemoved`, nicht das vollständige `DependencyRemoved`
    /// (die Abhängigkeit selbst ist ja weiterhin vorhanden).
    #[test]
    fn test_diff_reports_dependency_version_removed_when_one_of_several_versions_disappears() {
        let mut previous = Inventory::default();
        previous.dependency_versions.insert(
            "libc".to_owned(),
            BTreeSet::from([
                LockedDependency {
                    version: "0.2.150".to_owned(),
                    source: None,
                    checksum: None,
                },
                LockedDependency {
                    version: "0.2.140".to_owned(),
                    source: None,
                    checksum: None,
                },
            ]),
        );

        let mut current = Inventory::default();
        current
            .dependency_versions
            .insert("libc".to_owned(), single_version("0.2.150"));

        let changes = current.diff(&previous);
        assert_eq!(
            changes,
            vec![StructureChange::DependencyVersionRemoved {
                name: "libc".to_owned(),
                version: "0.2.140".to_owned(),
            }]
        );
    }

    /// F-096, Diff-Seite: eine geänderte Prüfsumme **bei unveränderter
    /// Version** erzeugt `ProvenanceChanged` — vor dieser Korrektur gab es
    /// dafür keine Meldung, weil `checksum` gar nicht erst gespeichert wurde.
    #[test]
    fn test_diff_reports_provenance_changed_for_same_version_different_checksum_f096() {
        let mut previous = Inventory::default();
        previous.dependency_versions.insert(
            "serde".to_owned(),
            BTreeSet::from([LockedDependency {
                version: "1.0.228".to_owned(),
                source: Some("registry+https://github.com/rust-lang/crates.io-index".to_owned()),
                checksum: Some("aaa".to_owned()),
            }]),
        );

        let mut current = Inventory::default();
        current.dependency_versions.insert(
            "serde".to_owned(),
            BTreeSet::from([LockedDependency {
                version: "1.0.228".to_owned(),
                source: Some("registry+https://github.com/rust-lang/crates.io-index".to_owned()),
                checksum: Some("bbb".to_owned()),
            }]),
        );

        let changes = current.diff(&previous);
        assert_eq!(
            changes,
            vec![StructureChange::ProvenanceChanged {
                name: "serde".to_owned(),
                version: "1.0.228".to_owned(),
                from_source: Some(
                    "registry+https://github.com/rust-lang/crates.io-index".to_owned()
                ),
                to_source: Some("registry+https://github.com/rust-lang/crates.io-index".to_owned()),
                from_checksum: Some("aaa".to_owned()),
                to_checksum: Some("bbb".to_owned()),
            }]
        );
    }

    /// F-096, Kernszenario aus dem Register: Wechsel der Paketquelle von
    /// Registry auf Git bei unveränderter Versionsnummer.
    #[test]
    fn test_diff_reports_provenance_changed_for_registry_to_git_source_swap_f096() {
        let mut previous = Inventory::default();
        previous.dependency_versions.insert(
            "some-crate".to_owned(),
            BTreeSet::from([LockedDependency {
                version: "1.0.0".to_owned(),
                source: Some("registry+https://github.com/rust-lang/crates.io-index".to_owned()),
                checksum: Some("aaa".to_owned()),
            }]),
        );

        let mut current = Inventory::default();
        current.dependency_versions.insert(
            "some-crate".to_owned(),
            BTreeSet::from([LockedDependency {
                version: "1.0.0".to_owned(),
                source: Some("git+https://example.com/some-crate".to_owned()),
                checksum: None,
            }]),
        );

        let changes = current.diff(&previous);
        assert_eq!(changes.len(), 1, "genau ein Provenienz-Ereignis erwartet");
        assert!(matches!(
            &changes[0],
            StructureChange::ProvenanceChanged { name, version, .. }
                if name == "some-crate" && version == "1.0.0"
        ));
    }

    /// End-to-End über `Inventory::read` (statt handgebauter Inventare):
    /// eine zweite, zusätzliche Version derselben Abhängigkeit taucht im
    /// realen `Cargo.lock` auf und wird über `diff` sichtbar.
    #[test]
    fn test_read_and_diff_reports_additional_locked_version_end_to_end_f097() -> TestResult {
        let root = scratch_dir("additional-version-e2e")?;
        write_root(&root, &["a"])?;
        write_member(&root, "a", &[], &["libc"])?;
        write_lockfile_with_metadata(
            &root,
            &[("a", "0.1.0", None, None), ("libc", "0.2.150", None, None)],
        )?;
        let scope = ReadScope::from_roots([root.clone()]);

        let previous = Inventory::read(&scope, &root).map_err(ctx("erstes Inventar lesen"))?;

        write_lockfile_with_metadata(
            &root,
            &[
                ("a", "0.1.0", None, None),
                ("libc", "0.2.150", None, None),
                ("libc", "0.2.140", None, None),
            ],
        )?;

        let current = Inventory::read(&scope, &root).map_err(ctx("zweites Inventar lesen"))?;
        let changes = current.diff(&previous);

        assert_eq!(
            changes,
            vec![StructureChange::DependencyVersionAdded {
                name: "libc".to_owned(),
                version: "0.2.140".to_owned(),
                source: None,
                checksum: None,
            }]
        );

        fs::remove_dir_all(&root).ok();
        Ok(())
    }
}
