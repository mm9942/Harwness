//! Zugriffsvokabular für Sensoren: Berechtigung, Lesebereich, Fehler, Griff.
//!
//! # Verantwortungsbereich
//! Diese Crate ist das *Zugriffs*vokabular, an dem alle Sensor-Crates des
//! AW0-Ausbauprogramms gleichzeitig hängen (Vertrag `docs/aw-contract-master.md`,
//! Abschnitt F). Sie besitzt vier eng zusammenhängende Bausteine:
//!
//! - [`Capability`] / [`CapabilityClass`]: welche Ressource ein Sensor lesen
//!   darf und in welche Berechtigungsklasse das fällt.
//! - [`ReadScope`]: ein Halbverband erlaubter Wurzelverzeichnisse, der nur
//!   schrumpfen kann. [`ReadScope::open`] löst Symlinks **vor** der
//!   Bereichsprüfung auf (TOCTOU-sicher).
//! - [`SensorError`] / [`Permanence`]: der eine, inhaltsfreie Fehlertyp
//!   dieser Crate und ob ein Fehler wiederholbar ist.
//! - [`SensorHandle`] mit den Typestate-Markern [`Unbound`]/[`Bound`]: ein
//!   ungebundener Griff kann nicht lesen — als fehlender Methodenname, nicht
//!   als Laufzeitfehler.
//!
//! **Bewusst nicht hier:** `trait Sensor`. Er müsste `HostSample` und
//! `SecurityEvent` nennen, die in `harw-dod-signals` liegen; da
//! `harw-dod-signals` bereits an `harw-dod-cap` hängt, würde ein Trait hier
//! einen Zyklus erzeugen. `harw-dod-cap` besitzt das *Zugriffs*vokabular,
//! `harw-dod-signals` das *Daten*vokabular und den Trait, der Daten erzeugt
//! (Vertrag Abschnitt F, Abweichungshinweis).
//!
//! # Exportierte Typen
//! [`Capability`], [`CapabilityClass`], [`ReadScope`], [`SensorError`],
//! [`Permanence`], [`SensorHandle`], [`Unbound`], [`Bound`].
//!
//! # Nebenläufigkeit
//! Alle Typen sind reine Werte ohne innere Veränderlichkeit: `Send + Sync`,
//! beliebig zwischen Threads teilbar. Einzig [`ReadScope::open`] führt
//! Betriebssystem-I/O aus; sie ist zustandslos aufrufbar und damit ebenfalls
//! aus mehreren Threads gleichzeitig nutzbar.
//!
//! # Fehler
//! [`SensorError`] ist der einzige Fehlertyp dieser Crate.
//!
//! # Examples
//! ```rust
//! use harw_dod_cap::{Capability, ReadScope, SensorHandle};
//! use harw_types::SensorId;
//! use std::path::PathBuf;
//!
//! let scope = ReadScope::from_roots([PathBuf::from("/proc")]);
//! let handle = SensorHandle::new(SensorId::from_str("proc-stat-0"), Capability::ReadProcStat)
//!     .bind(scope);
//!
//! assert_eq!(handle.capability(), Capability::ReadProcStat);
//! assert_eq!(handle.capability().class(), harw_dod_cap::CapabilityClass::Unprivileged);
//! ```

pub mod capability;
pub mod error;
pub mod handle;
pub mod scope;

pub use capability::{Capability, CapabilityClass};
pub use error::{Permanence, SensorError};
pub use handle::{Bound, SensorHandle, Unbound};
pub use scope::ReadScope;
