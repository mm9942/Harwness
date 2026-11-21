//! Offline-Abgleich gesperrter Abhängigkeiten gegen die RustSec-Datenbank
//! (PL-100 WP1.8, Entscheidung O11: Quelle „Advisory“).
//!
//! # Zweck
//! Ein unprivilegierter Sensor ([`AdvisorySensor`]), der konfigurierte
//! `Cargo.lock`-Dateien liest und jede gesperrte crates.io-Version gegen
//! einen **lokalen** Checkout von `rustsec/advisory-db` prüft. Der Sentinel
//! hat kein Netz; die Datenbank aktuell zu halten (etwa ein zeitgesteuertes
//! `git pull` unter eigener Kennung) ist Aufgabe des Betriebs, nicht dieser
//! Crate. Die Crate startet keinen Prozess und öffnet keinen Socket
//! (belegt durch `invariant_test`).
//!
//! # Verhältnis zu `harw-dod-scanreport`
//! `harw-dod-scanreport` liest fertige `cargo audit --json`-Berichte, die
//! ein Werkzeug außerhalb des Harness erzeugt hat. Diese Crate rechnet
//! selbst und braucht dafür nur Lockfiles und die Datenbank. Beide melden
//! dieselbe Ereignisform ([`harw_dod_signals::EventKind::StructureDrift`]);
//! laufen beide auf demselben Baum, kann dieselbe Lücke zweimal erscheinen
//! (Deduplikation ist Sache des Sentinels, WP0.4).
//!
//! # Semantik (wie `cargo audit`)
//! - Geprüft werden nur crates.io-Pakete; Pfad-, Git- und Fremd-Registry-
//!   Pakete nie (`lockfile`).
//! - Eine Version ist verwundbar, wenn sie in keinem `patched`- und keinem
//!   `unaffected`-Bereich liegt — mit SemVer-Präzedenz wie in `rustsec`,
//!   also auch für Vorabversionen korrekt und nie lexikographisch
//!   (`versions`).
//! - Zurückgezogene Advisories und Advisories für fremde Registries werden
//!   übersprungen; informative (`notice`, `unmaintained`, `unsound`) werden
//!   gemeldet und tragen ihre Art im `detail` (`advisory`).
//! - Die Schwere kommt aus dem CVSS-3.x-Vektor (`cvss`), auch bei
//!   informativen Advisories; ist er unlesbar, ist sie `Unknown` (die Regel
//!   `structure-drift` stuft das als `Medium` ein). Ohne Vektor ist sie
//!   `Low` für `notice` und `unmaintained`, aber `Unknown` für `unsound` —
//!   undefiniertes Verhalten aus sicherem Code ist nie „gering“.
//!
//! # Grenzen und Sichtbarkeit
//! [`AdvisoryConfig::max_advisories`] begrenzt die gelesenen
//! Advisory-Dateien je Lockfile und, zusammen mit
//! [`MAX_EVENTS_PER_POLL`] (`min` aus beiden), die gemeldeten Ereignisse je
//! Prüfung; [`MAX_LOCKED_CRATES`] begrenzt die geprüften Crate-Namen je
//! Lockfile; Lockfiles und Advisory-Dateien sind je auf 1 MiB begrenzt. Was
//! übersprungen oder abgeschnitten wird, steht in vier Zählern
//! ([`LOCKFILE_ERRORS_METRIC`], [`MALFORMED_ADVISORIES_METRIC`],
//! [`TRUNCATED_ADVISORIES_METRIC`], [`TRUNCATED_EVENTS_METRIC`]) und löst
//! ein `SensorDegraded` aus — eine Kürzung bleibt nie still.
//!
//! # Takt
//! Der Sentinel ruft Sensoren alle paar Sekunden ab; geprüft wird aber
//! höchstens einmal je [`RESCAN_INTERVAL`] (an der injizierten Zeit
//! gemessen), dazwischen ist die Lesung leer. Sonst wälzten die stets
//! wiederholten Treffer den Ereignisring des Sentinels um (B2/B10).
//!
//! # Aktualität der Datenbank
//! Der Sensor prüft, dass `<db_path>/crates` nach einer advisory-db
//! aussieht (sonst scheitert der Abruf), und meldet ihr Alter
//! ([`DATABASE_AGE_METRIC`], Näherung über die Änderungszeiten von
//! `crates/` und `.git/FETCH_HEAD`). Ist sie älter als
//! [`STALE_DATABASE_AGE`], meldet er `SensorDegraded` — ein veralteter
//! Checkout ist Blindheit, nicht „keine Lücken“.
//!
//! # Angreiferkontrollierter Inhalt
//! Lockfiles liegen in beschreibbaren Projektverzeichnissen, die Datenbank
//! ist ein Git-Checkout. Deshalb: Lesen ohne Blockieren auf FIFOs
//! (`read`), Crate-Namen und IDs nur aus `[A-Za-z0-9_-]`, Titel durch
//! [`harw_untrusted_text::escape_line`], kein Pfad und kein Inhalt in
//! Ereignissen oder Fehlern.
//!
//! # Nebenläufigkeit
//! [`AdvisorySensor`] ist `Send + Sync`; veränderlich ist nur der Zeitpunkt
//! der letzten Prüfung (hinter einem `Mutex`). Jeder Abruf arbeitet nur mit
//! eigenen Deskriptoren.
//!
//! # Fehler
//! Öffentlich nur [`harw_dod_cap::SensorError`] aus `Sensor::poll` — und
//! nur, wenn die Datenbank selbst fehlt oder nicht nach einer advisory-db
//! aussieht (siehe [`AdvisorySensor`]).
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_advisory::{
//!     AdvisoryConfig, AdvisorySensor, DEFAULT_MAX_ADVISORIES, MAX_EVENTS_PER_POLL,
//! };
//! use harw_dod_signals::{EventKind, Sensor};
//! use harw_types::SensorId;
//! use std::path::PathBuf;
//!
//! let sensor = AdvisorySensor::new(
//!     SensorId::from_str("advisory-0"),
//!     AdvisoryConfig {
//!         lockfiles: vec![PathBuf::from("/srv/projekt/Cargo.lock")],
//!         db_path: PathBuf::from("/var/lib/harw-dod/advisory-db"),
//!         max_advisories: DEFAULT_MAX_ADVISORIES,
//!     },
//! );
//! let reading = sensor.poll(jiff::Timestamp::UNIX_EPOCH)?;
//! let drift = reading
//!     .events
//!     .iter()
//!     .filter(|event| matches!(event.kind, EventKind::StructureDrift { .. }))
//!     .count();
//! assert!(drift <= MAX_EVENTS_PER_POLL);
//! # Ok::<(), harw_dod_cap::SensorError>(())
//! ```

mod advisory;
mod config;
mod cvss;
mod fault;
mod lockfile;
mod read;
mod sensor;
mod versions;

pub use config::{AdvisoryConfig, DEFAULT_MAX_ADVISORIES, MAX_EVENTS_PER_POLL, MAX_LOCKED_CRATES};
pub use sensor::{
    ADVISORY_HARDNESS, AdvisorySensor, DATABASE_AGE_METRIC, DATABASE_PROBE_ENTRIES,
    LOCKFILE_ERRORS_METRIC, MALFORMED_ADVISORIES_METRIC, MAX_DETAIL_CHARS, RESCAN_INTERVAL,
    STALE_DATABASE_AGE, TRUNCATED_ADVISORIES_METRIC, TRUNCATED_EVENTS_METRIC,
};

// Quelltext-Invariante: kein Netz, kein Prozess, keine Uhr.
#[cfg(test)]
mod invariant_test;

// Test-Fehlertyp und Fixtures, nur für Tests.
#[cfg(test)]
mod test_support;
