//! `AdvisoryConfig`: was der Advisory-Sensor liest und wie viel.
//!
//! # Zweck
//! Eine schlichte Konfiguration ohne Serde: welche `Cargo.lock`-Dateien
//! geprüft werden, wo der lokale Checkout der RustSec-Datenbank liegt und
//! wie viel eine Prüfung höchstens liest und meldet
//! ([`AdvisoryConfig::max_advisories`]: Advisory-Dateien je Lockfile und,
//! zusätzlich durch [`MAX_EVENTS_PER_POLL`] begrenzt, gemeldete Treffer).
//! Woher diese Werte kommen (etwa `/etc/harw-dod/config.toml`, Prinzip I6),
//! ist Sache des Sentinels.
//!
//! # Pfade
//! Alle Pfade müssen **absolut und kanonisch** sein (ohne Symlink in einem
//! Vorfahren): [`harw_dod_cap::ReadScope`] vergleicht seine Wurzeln mit dem
//! kanonischen Ziel jedes Zugriffs und kanonisiert die Wurzeln selbst nicht.
//! Ein relativer oder nicht kanonischer Pfad führt nicht zu einem
//! Fehlzugriff, sondern zu einem gezählten bzw. gemeldeten Fehler (siehe
//! [`crate::AdvisorySensor`]).
//!
//! # Nebenläufigkeit
//! Reiner Werttyp; `Send + Sync`.
//!
//! # Fehler
//! Keine; geprüft wird beim Abruf.
//!
//! # Examples
//! ```rust
//! use harw_dod_advisory::{AdvisoryConfig, DEFAULT_MAX_ADVISORIES, MAX_EVENTS_PER_POLL};
//! use std::path::{Path, PathBuf};
//!
//! let config = AdvisoryConfig {
//!     lockfiles: vec![PathBuf::from("/srv/projekt/Cargo.lock")],
//!     db_path: PathBuf::from("/var/lib/harw-dod/advisory-db"),
//!     max_advisories: DEFAULT_MAX_ADVISORIES,
//! };
//! let scope = config.read_scope();
//! assert!(scope.allows(Path::new("/var/lib/harw-dod/advisory-db/crates/tokio/RUSTSEC-2021-0124.md")));
//! assert!(scope.allows(Path::new("/srv/projekt/Cargo.lock")));
//! assert!(!scope.allows(Path::new("/srv/projekt/Cargo.toml")));
//! // Mit der Vorgabe begrenzt `MAX_EVENTS_PER_POLL` die Ereignisse.
//! assert_eq!(MAX_EVENTS_PER_POLL.min(config.max_advisories), MAX_EVENTS_PER_POLL);
//! ```

use std::path::PathBuf;

use harw_dod_cap::ReadScope;

/// Vorgabe für [`AdvisoryConfig::max_advisories`].
///
/// # Description
/// Ein `Cargo.lock` mit rund 800 Paketen (dieser Workspace) berührt in der
/// RustSec-Datenbank etwa 100 bis 200 Advisory-Dateien. 1024 je Lockfile
/// lässt dafür reichlich Luft und begrenzt trotzdem die Arbeit, die ein
/// einzelnes (womöglich von einem Nutzer geschriebenes) Lockfile auslösen
/// kann. Mit dieser Vorgabe begrenzt [`MAX_EVENTS_PER_POLL`] die Zahl der
/// gemeldeten Ereignisse, nicht dieser Wert.
pub const DEFAULT_MAX_ADVISORIES: usize = 1024;

/// Feste Obergrenze der `StructureDrift`-Ereignisse einer Prüfung: **64**;
/// die wirksame Grenze ist `min(MAX_EVENTS_PER_POLL, max_advisories)`.
///
/// # Description
/// Jede Prüfung meldet alle Treffer erneut; der Ereignisring des Sentinels
/// fasst `EvidenceBuffer::DEFAULT_EVENT_CAPACITY` = 4096 Ereignisse. Ein
/// Lockfile in einem beschreibbaren Projektverzeichnis (A1) könnte sonst
/// mit vielen verwundbaren Versionen je Prüfung Hunderte Ereignisse
/// erzeugen und die anderer Sensoren und Sonden aus dem Ring drängen
/// (B2/B10). 64 deckt einen echten Bestand (selten mehr als ein Dutzend
/// Treffer) mit Abstand ab; was darüber liegt, wird gezählt
/// (`advisory.truncated_events`) und als `SensorDegraded` gemeldet — die
/// schwersten Treffer bleiben. Wie oft geprüft wird, begrenzt
/// `harw_dod_advisory::RESCAN_INTERVAL`.
pub const MAX_EVENTS_PER_POLL: usize = 64;

/// Höchstzahl verschiedener Crate-Namen, die eine Prüfung je Lockfile
/// berücksichtigt: **4096**.
///
/// # Description
/// Für jeden Namen listet eine Prüfung ein Verzeichnis der Datenbank auf
/// (ein Glob, mehrere Systemaufrufe). Ein beschreibbares Lockfile (A1) mit
/// 1 MiB fasst rund 10 000 minimale `[[package]]`-Einträge; ohne Grenze
/// bestimmte es die Arbeit des Sentinels. Ein großer echter Workspace sperrt
/// kaum mehr als 1000 bis 2000 Crates. Weitere Namen werden in
/// Dateireihenfolge nicht geprüft und das Lockfile in
/// `advisory.lockfile_errors` gezählt.
pub const MAX_LOCKED_CRATES: usize = 4096;

/// Die Konfiguration von [`crate::AdvisorySensor`].
///
/// # Description
/// Siehe Moduldoku, insbesondere zu den Pfadanforderungen.
///
/// # Concurrency
/// Reiner Werttyp; `Send + Sync`.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisoryConfig {
    /// Die zu prüfenden `Cargo.lock`-Dateien (Dateipfade, nicht
    /// Verzeichnisse).
    pub lockfiles: Vec<PathBuf>,
    /// Wurzel des lokalen `rustsec/advisory-db`-Checkouts (das Verzeichnis,
    /// das `crates/` enthält). Der Sensor greift nie aufs Netz zu; die
    /// Datenbank aktuell zu halten ist Aufgabe des Betriebs.
    pub db_path: PathBuf,
    /// Die Obergrenze einer Prüfung, in zwei Rollen:
    /// - Höchstzahl der Advisory-Dateien, die eine Prüfung **je Lockfile**
    ///   neu auswertet. Jedes Lockfile hat sein eigenes Budget, damit eines
    ///   mit vielen Crates die Prüfung der übrigen nicht kürzt; ein bereits
    ///   vollständig gelesenes Crate-Verzeichnis kostet ein weiteres
    ///   Lockfile nichts. Was darüber liegt, wird gezählt
    ///   (`advisory.truncated_advisories`).
    /// - Höchstzahl der gemeldeten Treffer, zusätzlich durch
    ///   [`MAX_EVENTS_PER_POLL`] begrenzt: `min(MAX_EVENTS_PER_POLL,
    ///   max_advisories)`. Was darüber liegt, wird gezählt
    ///   (`advisory.truncated_events`).
    ///
    /// Jede Kürzung wird als `SensorDegraded` gemeldet.
    pub max_advisories: usize,
}

impl AdvisoryConfig {
    /// Der Lesebereich, den diese Konfiguration braucht: jedes Lockfile und
    /// die Datenbankwurzel.
    ///
    /// # Description
    /// [`crate::AdvisorySensor::new`] bindet seinen Griff an genau diesen
    /// Bereich. Ein Sentinel kann daraus auch seine Landlock-Leseregeln
    /// ableiten (`ReadScope::roots`).
    ///
    /// # Returns
    /// Einen [`ReadScope`] mit den Lockfile-Pfaden und `db_path` als
    /// Wurzeln.
    ///
    /// # Examples
    /// Siehe Moduldoku.
    #[must_use]
    pub fn read_scope(&self) -> ReadScope {
        ReadScope::from_roots(
            self.lockfiles
                .iter()
                .cloned()
                .chain(std::iter::once(self.db_path.clone())),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_read_scope_contains_exactly_the_lockfiles_and_the_database() {
        let config = AdvisoryConfig {
            lockfiles: vec![
                PathBuf::from("/srv/a/Cargo.lock"),
                PathBuf::from("/srv/b/Cargo.lock"),
            ],
            db_path: PathBuf::from("/var/lib/advisory-db"),
            max_advisories: DEFAULT_MAX_ADVISORIES,
        };
        let scope = config.read_scope();
        let roots: Vec<&Path> = scope.roots().collect();
        assert_eq!(
            roots,
            [
                Path::new("/srv/a/Cargo.lock"),
                Path::new("/srv/b/Cargo.lock"),
                Path::new("/var/lib/advisory-db"),
            ]
        );
        assert!(!scope.allows(Path::new("/srv/a/src/main.rs")));
        assert!(!scope.allows(Path::new("/var/lib/advisory-db/../secret")));
    }
}
