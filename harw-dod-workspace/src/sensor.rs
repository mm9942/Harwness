//! `WorkspaceDriftSensor`: der `harw_dod_signals::Sensor`, der [`Inventory`]
//! gegen eine mitgegebene Referenz vergleicht und Abweichungen meldet.
//!
//! # Warum der Sensor kein eigenes Gedächtnis hat
//! `harw_dod_signals::Sensor::poll` nimmt `&self` — bewusst, damit der
//! Sentinel Sensoren hinter `Arc` halten und konkurrierend pollen kann (siehe
//! `harw_dod_signals::sensor`-Moduldoku). Ein Sensor, der sein zuletzt
//! gesehenes Inventar nach jedem Poll selbst überschriebe, bräuchte entweder
//! `&mut self` (verletzt die Trait-Signatur) oder ein `Mutex`/`RefCell`
//! (macht `poll` nicht mehr deterministisch prüfbar: zwei aufeinanderfolgende
//! Aufrufe eines Fixtures lieferten unterschiedliche Ergebnisse, obwohl die
//! Eingabedatei sich nicht geändert hat — genau das, was die
//! "Determinismus"-Prüfung des Fixture-Harness ausschließen soll).
//!
//! [`WorkspaceDriftSensor`] löst das, indem es `baseline: Option<Inventory>`
//! **einmalig bei der Konstruktion** entgegennimmt und danach nie mehr
//! verändert — architektonisch identisch zu `handle`, das jeder Sensor in
//! diesem Programm bereits unveränderlich hält. Der Aufrufer (nicht dieser
//! Sensor) ist dafür verantwortlich, das zuletzt akzeptierte [`Inventory`]
//! zu persistieren (`Inventory` ist dafür serde-serialisierbar) und bei
//! Bedarf einen neuen Sensor mit aktualisierter `baseline` zu bauen.
//! `baseline: None` modelliert den allerersten Lauf: es gibt keinen
//! Bezugspunkt, also meldet [`Sensor::poll`] keine Drift — ein Sensor, der
//! hier stattdessen den gesamten Workspace als "neu" meldete, würde nach dem
//! zweiten Mal ignoriert, und dann fiele auch eine echte Meldung durch.
//!
//! # Warum `harw_dod_fixtures::sensor_suite!` hier nicht greift (Korrektur K39)
//! `sensor_suite!` verlangt `From<harw_dod_cap::SensorHandle<harw_dod_cap::Bound>>`
//! als einzigen Konstruktionsweg — ein gebundener Griff hinein, eine
//! Sensor-Instanz heraus. Das lässt sich für [`WorkspaceDriftSensor`] nicht
//! sinnvoll implementieren: die Konstruktion braucht zusätzlich
//! `baseline: Option<Inventory>`, das aus einem bloßen Griff nicht ableitbar
//! ist. Ein `From`-Impl, der `baseline: None` annimmt, ließe **jede** der
//! sechs Pflichtprüfungen des Harness leer durchlaufen — null Ereignisse,
//! immer grün — ohne die Drifterkennung dieses Sensors je auszuführen. Diese
//! Crate verzichtet deshalb bewusst auf `sensor_suite!` und schreibt ihre
//! Prüfungen von Hand (siehe Testmodul unten), statt einen scheinbar
//! bestandenen, tatsächlich nie ausgeführten Harness-Lauf zu erkaufen.
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_cap::{Capability, ReadScope, SensorHandle};
//! use harw_dod_signals::Sensor;
//! use harw_dod_workspace::WorkspaceDriftSensor;
//! use harw_types::SensorId;
//! use std::path::PathBuf;
//!
//! let root = PathBuf::from(".");
//! let scope = ReadScope::from_roots([root.clone()]);
//! let handle = SensorHandle::new(SensorId::from_str("workspace-drift"), Capability::ReadWorkspaceGraph)
//!     .bind(scope);
//! let sensor = WorkspaceDriftSensor::new(handle, root, None);
//! let reading = sensor.poll(jiff::Timestamp::UNIX_EPOCH)?;
//! assert!(reading.events.is_empty(), "erster Lauf ohne Vorgänger meldet nichts");
//! # Ok::<(), harw_dod_cap::SensorError>(())
//! ```

use std::path::PathBuf;

use harw_dod_cap::{Bound, SensorError, SensorHandle};
use harw_dod_signals::{DriftSeverity, EventKind, SecurityEvent, Sensor, SensorReading};
use jiff::Timestamp;

use crate::inventory::{Inventory, StructureChange, VersionSeverity};

/// Sensor für Strukturdrift im eigenen Cargo-Abhängigkeitsgraphen.
///
/// # Description
/// Liest bei jedem [`Sensor::poll`] das aktuelle [`Inventory`] unter `root`
/// (über den im `handle` gebundenen `ReadScope`) und vergleicht es — falls
/// vorhanden — gegen `baseline`. Jede gefundene [`StructureChange`] wird als
/// ein `EventKind::StructureDrift`-Ereignis gemeldet. Siehe Moduldoku für die
/// Begründung, warum `baseline` nach der Konstruktion nie mehr verändert
/// wird.
#[derive(Debug)]
pub struct WorkspaceDriftSensor {
    handle: SensorHandle<Bound>,
    root: PathBuf,
    baseline: Option<Inventory>,
}

impl WorkspaceDriftSensor {
    /// Baut einen Sensor für einen Vergleichslauf.
    ///
    /// # Arguments
    /// - `handle` (`harw_dod_cap::SensorHandle<harw_dod_cap::Bound>`): Griff
    ///   mit `Capability::ReadWorkspaceGraph`, gebunden an einen `ReadScope`,
    ///   der mindestens `root` umfasst.
    /// - `root` (`std::path::PathBuf`): Wurzelverzeichnis des zu
    ///   beobachtenden Cargo-Workspace.
    /// - `baseline` (`Option<Inventory>`): das zuletzt akzeptierte Inventar,
    ///   vom Aufrufer gehalten. `None` beim allerersten Lauf — dann meldet
    ///   [`Sensor::poll`] keine Drift (siehe Moduldoku).
    ///
    /// # Returns
    /// Einen neuen, unveränderlichen `WorkspaceDriftSensor`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    /// use harw_dod_workspace::WorkspaceDriftSensor;
    /// use harw_types::SensorId;
    /// use std::path::PathBuf;
    ///
    /// let scope = ReadScope::from_roots([PathBuf::from(".")]);
    /// let handle = SensorHandle::new(SensorId::from_str("workspace-drift"), Capability::ReadWorkspaceGraph)
    ///     .bind(scope);
    /// let sensor = WorkspaceDriftSensor::new(handle, PathBuf::from("."), None);
    /// let _ = sensor;
    /// ```
    #[must_use]
    pub fn new(handle: SensorHandle<Bound>, root: PathBuf, baseline: Option<Inventory>) -> Self {
        Self {
            handle,
            root,
            baseline,
        }
    }
}

impl Sensor for WorkspaceDriftSensor {
    /// Der gebundene Griff dieses Sensors.
    ///
    /// # Returns
    /// Referenz auf den bei [`WorkspaceDriftSensor::new`] übergebenen Griff.
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    /// Liest das aktuelle Inventar und meldet Abweichungen gegen `baseline`.
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): injizierte Zeit für jedes erzeugte
    ///   Ereignis. Dieser Sensor liest nie die Systemuhr selbst.
    ///
    /// # Returns
    /// Ein [`SensorReading`] ohne Messwerte (`samples` bleibt leer — dieser
    /// Sensor beobachtet Struktur, keine Zahlen) und einem
    /// `EventKind::StructureDrift`-Ereignis je gefundener [`StructureChange`].
    /// Leer, wenn `baseline` `None` ist (erster Lauf) oder der Workspace
    /// unverändert ist.
    ///
    /// # Errors
    /// [`harw_dod_cap::SensorError`] — siehe
    /// [`crate::error::WorkspaceError`] für die Fehlerfälle, die hierauf
    /// abgebildet werden.
    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let current = Inventory::read(self.handle.scope(), &self.root)?;

        let events = match &self.baseline {
            None => Vec::new(),
            Some(previous) => current
                .diff(previous)
                .iter()
                .map(|change| SecurityEvent {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    actor: None,
                    kind: EventKind::StructureDrift {
                        severity: drift_severity(change),
                        detail: describe(change),
                    },
                })
                .collect(),
        };

        Ok(SensorReading {
            samples: Vec::new(),
            events,
        })
    }
}

/// Bildet eine [`StructureChange`] auf die grobe Schwere ab, die
/// `EventKind::StructureDrift` trägt.
///
/// # Description
/// Dies ist die **eine** Stelle, an der die feine Einstufung dieser Crate
/// ([`VersionSeverity`] mit ihren SemVer-Feinheiten) in das grobe Vokabular
/// des Signalstroms übersetzt wird. Vorher gab es diese Übersetzung nicht:
/// die Schwere floss nur in den Freitext, und eine Regel gewann sie durch
/// Präfixvergleich zurück. Eine geänderte Formulierung hätte dort still zu
/// einer falschen Einstufung geführt.
///
/// Die Zuordnung im Einzelnen:
/// - **`MemberAdded`** → `Medium`. Ein neues Workspace-Member ist erwartbar,
///   aber es erweitert die Fläche.
/// - **`DependencyAdded`** → `High`. Der häufigste Weg, auf dem fremder Code
///   in ein Projekt gelangt. Eine hinzugefügte Abhängigkeit verdient einen
///   Blick, auch wenn sie harmlos ist.
/// - **`DependencyRemoved`** → `Low`. Weniger fremder Code ist keine
///   Verschlechterung — aber meldenswert, weil eine Entfernung eine Zusage
///   aufheben kann, auf die sich etwas anderes stützte.
/// - **`EdgeAdded`** → `Medium`. Eine neue interne Kante ändert, wer wen
///   erreichen kann; das ist die Frage, die die Kanten-Gates stellen.
/// - **`VersionChanged`** → aus [`VersionSeverity`]: `Breaking` → `High`,
///   `Unparseable` → `Medium`, `Minor` → `Medium`, `Patch` → `Low`.
///
/// **`Unparseable` wird nicht kleingeredet.** Eine Version, die sich nicht
/// auswerten lässt, ist keine Patchversion — sie ist ein unbekannter Fall,
/// und ein Driftsensor, der Unklares herunterstuft, hebt seinen eigenen Zweck
/// auf.
///
/// # Arguments
/// - `change` (`&StructureChange`): die einzustufende Abweichung.
///
/// # Returns
/// Die grobe Schwere für den Signalstrom.
fn drift_severity(change: &StructureChange) -> DriftSeverity {
    match change {
        StructureChange::DependencyAdded { .. } => DriftSeverity::High,
        StructureChange::DependencyRemoved { .. } => DriftSeverity::Low,
        StructureChange::MemberAdded { .. } | StructureChange::EdgeAdded { .. } => {
            DriftSeverity::Medium
        }
        StructureChange::VersionChanged { severity, .. } => match severity {
            VersionSeverity::Breaking => DriftSeverity::High,
            VersionSeverity::Minor | VersionSeverity::Unparseable => DriftSeverity::Medium,
            VersionSeverity::Patch => DriftSeverity::Low,
        },
    }
}

/// Baut die Freitext-Beschreibung für `EventKind::StructureDrift::detail` aus
/// einer [`StructureChange`].
///
/// # Description
/// Enthält ausschließlich öffentliche Dependency-Metadaten (Crate-Namen,
/// SemVer-Versionen), die bereits in `Cargo.toml`/`Cargo.lock` stehen — keine
/// Geheimnisse, keine Rohdaten aus dem Dateiinhalt selbst.
///
/// # Arguments
/// - `change` (`&StructureChange`): die zu beschreibende Abweichung.
///
/// # Returns
/// Eine deutschsprachige, einzeilige Beschreibung.
fn describe(change: &StructureChange) -> String {
    match change {
        StructureChange::MemberAdded { name } => format!("neues Workspace-Member: {name}"),
        StructureChange::EdgeAdded { from, to } => {
            format!("neue Abhängigkeitskante: {from} -> {to}")
        }
        StructureChange::DependencyAdded { name } => format!("neue Abhängigkeit: {name}"),
        StructureChange::DependencyRemoved { name } => format!("Abhängigkeit entfernt: {name}"),
        StructureChange::VersionChanged {
            name,
            from,
            to,
            severity,
        } => format!("Versionssprung ({severity:?}) bei {name}: {from} -> {to}"),
    }
}

#[cfg(test)]
mod tests {
    use super::WorkspaceDriftSensor;
    use crate::inventory::Inventory;
    use crate::test_support::{write_lockfile, write_member, write_root};
    use harw_dod_cap::{Bound, Capability, ReadScope, SensorHandle};
    use harw_dod_signals::{DriftSeverity, EventKind, Sensor};
    use harw_types::SensorId;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "harw-dod-workspace-sensor-{}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("Scratch-Verzeichnis anlegen");
        dir.canonicalize().expect("Scratch-Verzeichnis kanonisieren")
    }

    fn bound_handle(root: &Path) -> SensorHandle<Bound> {
        let scope = ReadScope::from_roots([root.to_path_buf()]);
        SensorHandle::new(SensorId::from_str("workspace-drift-test"), Capability::ReadWorkspaceGraph)
            .bind(scope)
    }

    #[test]
    fn test_poll_without_baseline_reports_no_drift() {
        // Der wichtigste Test dieses Knotens: ohne Vorgänger-Inventar meldet
        // der Sensor nichts, obwohl der Workspace durchaus Member und
        // Abhängigkeiten enthält.
        let root = scratch_dir("first-run");
        write_root(&root, &["a"]);
        write_member(&root, "a", &[], &["serde"]);
        write_lockfile(&root, &[("a", "0.1.0", false), ("serde", "1.0.228", true)]);

        let sensor = WorkspaceDriftSensor::new(bound_handle(&root), root.clone(), None);
        let reading = sensor
            .poll(jiff::Timestamp::UNIX_EPOCH)
            .expect("erster Poll darf nicht scheitern");

        assert!(reading.events.is_empty());
        assert!(reading.samples.is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_poll_with_baseline_reports_structure_drift_event_for_new_member() {
        let root = scratch_dir("second-run");
        write_root(&root, &["a"]);
        write_member(&root, "a", &[], &[]);
        write_lockfile(&root, &[("a", "0.1.0", false)]);

        let scope = ReadScope::from_roots([root.clone()]);
        let baseline = Inventory::read(&scope, &root).expect("Baseline-Inventar lesen");

        write_root(&root, &["a", "b"]);
        write_member(&root, "b", &[], &[]);
        write_lockfile(&root, &[("a", "0.1.0", false), ("b", "0.1.0", false)]);

        let sensor = WorkspaceDriftSensor::new(bound_handle(&root), root.clone(), Some(baseline));
        let reading = sensor
            .poll(jiff::Timestamp::UNIX_EPOCH)
            .expect("zweiter Poll darf nicht scheitern");

        assert_eq!(reading.events.len(), 1);
        match &reading.events[0].kind {
            EventKind::StructureDrift { severity, detail } => {
                assert!(detail.contains('b'));
                // Ein neues Member erweitert die Flaeche: mittlere Schwere,
                // nicht aus dem Text abgeleitet.
                assert_eq!(*severity, DriftSeverity::Medium);
            }
            other => panic!("erwartetes StructureDrift-Ereignis, bekam: {other:?}"),
        }

        fs::remove_dir_all(&root).ok();
    }
}
