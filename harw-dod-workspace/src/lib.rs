//! Strukturdrift im eigenen Cargo-Abhängigkeitsgraphen — Sicherheitssensor
//! für Knoten AW2-17.
//!
//! # Warum Strukturdrift ein Sicherheitssignal ist
//! Dieser Sensor beobachtet nicht den Host, sondern das Projekt selbst: eine
//! neue Abhängigkeit, ein Versionssprung, eine neue Kante zwischen Crates,
//! ein neues Workspace-Member. Eine neue **transitive** Abhängigkeit ist der
//! häufigste Weg, auf dem fremder Code in ein Projekt gelangt — niemand
//! entscheidet bewusst "ich füge jetzt zwölf zusätzliche Crates hinzu", es
//! passiert als Nebenwirkung eines einzigen `Cargo.toml`-Eintrags. Ein
//! Versionssprung von `0.2.28` auf `0.3.0` kann Verhalten ändern, das niemand
//! geprüft hat — und beides passiert leise, in einer Zeile `Cargo.lock`, die
//! in einem Diff niemand liest. Dieser Sensor macht genau diese leise
//! Änderung laut: als [`harw_dod_signals::EventKind::StructureDrift`].
//!
//! # Verantwortungsbereich
//! - [`Inventory`]: eine serde-serialisierbare Momentaufnahme des Workspace
//!   (Member, direkte Abhängigkeitskanten, gesperrte Versionen), gelesen über
//!   [`harw_code_graph::WorkspaceGraph`] und [`harw_code_graph::parse_lockfile`].
//! - [`Inventory::diff`]: reiner Mengenvergleich zweier Momentaufnahmen,
//!   liefert [`StructureChange`].
//! - [`WorkspaceDriftSensor`]: die `harw_dod_signals::Sensor`-Implementierung,
//!   die ein aktuelles [`Inventory`] gegen eine unveränderlich mitgegebene
//!   `baseline` vergleicht und jede Abweichung als ein
//!   `EventKind::StructureDrift`-Ereignis meldet.
//!
//! # Der Bezugspunkt liegt beim Aufrufer
//! Ein Drift-Sensor braucht ein zuletzt gesehenes Inventar, um überhaupt
//! etwas als "neu" erkennen zu können. Diese Crate führt dafür **keinen
//! eigenen Speicher**: [`Inventory`] ist serde-serialisierbar und wird vom
//! Aufrufer gehalten (z. B. als Datei zwischen zwei Läufen). Ein Sensor mit
//! Zustand über den Abruf hinaus verletzt die `&self`-Zusage von
//! `harw_dod_signals::Sensor::poll` und ist gegen kein Fixture mehr
//! deterministisch prüfbar — siehe [`sensor`]-Moduldoku für die volle
//! Begründung. Eine unmittelbare Folge: **der allererste Lauf, ohne
//! Vorgänger, meldet keine Drift.** Ein Sensor, der beim ersten Start den
//! gesamten Workspace als Verstoß meldete, würde nach dem zweiten Mal
//! ignoriert — und dann fiele auch die echte Meldung durch.
//!
//! # Was dieser Sensor NICHT tut
//! - **Kein Subprozess.** Kein `cargo metadata`, kein `std::process::Command`.
//!   Alles läuft über [`harw_code_graph`] (liest ausschließlich
//!   `Cargo.toml`/`Cargo.lock`) und [`harw_dod_cap::ReadScope`].
//! - **Kein Netz.** Keine Abfrage bei crates.io, keine
//!   Sicherheits-Advisory-Datenbank. Der Abgleich mit bekannten
//!   Schwachstellen ist ein anderer Knoten mit eigener Netzberechtigung.
//! - **Kein eigener Speicher über den Abruf hinaus** (siehe oben).
//!
//! # Fehler
//! [`WorkspaceError`] — siehe [`error`]-Moduldoku, insbesondere die
//! Begründung, warum `MalformedSource` bewusst keinen inneren Fehlerwert
//! trägt.
//!
//! # Nebenläufigkeit
//! Alle Typen sind reine Daten bzw. zustandslose Leser: `Send + Sync`, ohne
//! innere Veränderlichkeit. [`WorkspaceDriftSensor`] hält seine `baseline`
//! unveränderlich ab der Konstruktion — konkurrierende `poll`-Aufrufe auf
//! derselben Instanz sind sicher und deterministisch, solange sich die
//! gelesenen Dateien nicht ändern.
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_cap::{Capability, ReadScope, SensorHandle};
//! use harw_dod_signals::Sensor;
//! use harw_dod_workspace::{Inventory, WorkspaceDriftSensor};
//! use harw_types::SensorId;
//! use std::path::PathBuf;
//!
//! let root = PathBuf::from(".");
//! let scope = ReadScope::from_roots([root.clone()]);
//! let handle = SensorHandle::new(SensorId::from_str("workspace-drift"), Capability::ReadWorkspaceGraph)
//!     .bind(scope.clone());
//!
//! // Erster Lauf: kein Vorgänger, keine Drift-Meldung.
//! let first = WorkspaceDriftSensor::new(handle, root.clone(), None);
//! let baseline = Inventory::read(&scope, &root)?;
//! assert!(first.poll(jiff::Timestamp::UNIX_EPOCH)?.events.is_empty());
//!
//! // Späterer Lauf mit demselben Inventar als Referenz: ebenfalls keine Drift.
//! let handle_again = SensorHandle::new(SensorId::from_str("workspace-drift"), Capability::ReadWorkspaceGraph)
//!     .bind(scope);
//! let second = WorkspaceDriftSensor::new(handle_again, root, Some(baseline));
//! assert!(second.poll(jiff::Timestamp::UNIX_EPOCH)?.events.is_empty());
//! # Ok::<(), harw_dod_cap::SensorError>(())
//! ```

#![forbid(unsafe_code)]

pub mod error;
pub mod inventory;
pub mod sensor;

#[cfg(test)]
mod test_support;

pub use error::{WorkspaceError, WorkspaceResult};
pub use inventory::{Edge, Inventory, StructureChange, VersionSeverity, classify_version_change};
pub use sensor::WorkspaceDriftSensor;
