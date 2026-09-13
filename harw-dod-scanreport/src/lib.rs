//! Auswertung fremder Scanner-Berichte — liest, ruft nichts auf (Knoten AW2-16).
//!
//! # Zweck
//! Ein Sensor, der Berichte fremder Scanner **liest** — und sonst nichts.
//! Diese Crate ruft keinen Scanner auf; sie wertet aus, was einer bereits
//! hinterlassen hat. Die einzige Quelle ist ein Verzeichnis mit
//! Berichtsdateien (siehe [`harw_home::paths::scan_reports_dir`]), das von
//! externen Werkzeugen befüllt wird — nie von dieser Crate selbst.
//!
//! # Die Kommandozeilen-Invariante
//! **Kein Codepfad in dieser Crate bildet eine Kommandozeile.** Kein
//! `std::process::Command`, kein `exec`, kein `spawn`, keine `sh -c`-Shell,
//! und kein `format!`, dessen Ergebnis irgendwo ausgeführt werden könnte.
//!
//! Der Grund ist die Berechtigungsgrenze aus dem Vertrag (Abschnitt F): ein
//! Sensor, der einen Scanner startet, braucht Ausführungsrechte und erbt
//! dessen Berechtigungen — ein SAST-Werkzeug etwa liest denselben Code, den
//! es analysiert, unter Umständen mit weitergehenden Rechten als ein reiner
//! Leser je bräuchte. Ein Sensor, der nur liest, ist unprivilegiert
//! ([`harw_dod_cap::CapabilityClass::Unprivileged`], siehe
//! [`harw_dod_cap::Capability::ReadScanReports`]). Die Rechtematrix
//! behauptet das Zweite; der Code dieser Crate muss es einhalten, nicht nur
//! im Bewusstsein der Autorin, sondern nachprüfbar. Belegt zweifach:
//!
//! 1. Als Test: [`test_source_contains_no_command_execution`] (Modul
//!    `invariant_test`, nur unter `#[cfg(test)]` eingebunden) durchsucht den
//!    eigenen Quelltext dieser Crate nach `Command`, `exec`, `spawn` und
//!    `sh -c` und schlägt bei einem Treffer fehl — die einzige Prüfung, die
//!    auch einen später versehentlich hinzugefügten Aufruf fängt.
//! 2. Als dieser Absatz, damit ein späterer Autor die Regel liest, bevor er
//!    sie bricht.
//!
//! [`test_source_contains_no_command_execution`]: crate::invariant_test::test_source_contains_no_command_execution
//!
//! # Gelesene Formate
//! Genau zwei, statt fünf halb:
//!
//! - **SARIF** ([`sarif`]-Modul, intern): das De-facto-Austauschformat für
//!   Analysewerkzeuge (u. a. `clippy-sarif`, CodeQL, semgrep) — ein Sensor,
//!   der SARIF liest, profitiert von jedem Werkzeug, das SARIF ausgibt.
//! - **`cargo audit --json`** ([`cargo_audit`]-Modul, intern): passt zum
//!   Rust-Baum dieses Workspace und liefert die Advisory-/Paket-Kennungen,
//!   die der spätere Advisory-Abgleich in Knoten AW7-06 braucht.
//!
//! Ein gelesener Dateiinhalt wird anhand seiner JSON-Struktur einem der
//! beiden Formate zugeordnet ([`report::parse_report`]), nicht anhand des
//! Dateinamens; Dateien mit den Endungen `.json` und `.sarif` werden im
//! konfigurierten Bereich gesucht ([`sensor::glob_patterns_for_root`]).
//!
//! # Zuordnung auf `EventKind` — eine eingestandene Notlösung
//! Ein Scanner-Befund ist die **Behauptung eines fremden Werkzeugs**, kein
//! vom Harness selbst beobachtetes Ereignis. [`harw_dod_signals::EventKind`]
//! ist geschlossen und hat keine Variante für „Scanner-Befund"; diese Crate
//! darf `harw-dod-signals` nicht ändern (fremder Schreibbereich) und ordnet
//! deshalb jeden Befund [`harw_dod_signals::EventKind::StructureDrift`] zu.
//!
//! Das ist **kein sauberer Treffer, sondern eine bewusste, offen benannte
//! Fehlbelegung** — ehrlicher als eine stillschweigende. Für einen
//! `cargo-audit`-Treffer passt sie noch gut: eine verwundbare Abhängigkeit
//! *ist* im wörtlichen Sinn eine abgewichene Struktur des Abhängigkeitsbaums.
//! Für einen SARIF-Befund über Code (ein Muster in einer Funktion, keine
//! Struktur im Sinne von Konfiguration oder Workspace-Layout) passt sie
//! spürbar schlechter — sie wird hier trotzdem verwendet, weil es in der
//! aktuellen, geschlossenen Aufzählung keine bessere Variante gibt. Ein
//! künftiger Konsument, der beide Formate unterscheidbar behandeln will,
//! muss dafür wissen: diese Crate braucht eigentlich eine dritte,
//! scanner-spezifische `EventKind`-Variante; das ist ein offener Punkt
//! dieses Knotens, kein gelöstes Problem.
//!
//! [`harw_dod_signals::EventKind::StructureDrift::severity`] wird dagegen
//! sauber befüllt: SARIFs `level` bzw. die informelle CVSS-Einstufung einer
//! RustSec-Advisory werden an je einer Stelle strukturiert auf
//! [`harw_dod_signals::DriftSeverity`] abgebildet ([`sarif::map_level`],
//! [`cargo_audit::map_severity`]) — nicht aus dem `detail`-Text zurückgelesen.
//!
//! Dieselbe Zurückhaltung gilt für [`harw_dod_signals::Hardness`]: ein
//! fremder Bericht ist nicht [`harw_dod_signals::Hardness::Observed`] — siehe
//! [`sensor::REPORT_HARDNESS`] für die vollständige Begründung der Wahl.
//!
//! # Angreiferkontrollierter Inhalt
//! Ein Scanner-Bericht enthält Freitext — Beschreibungen, Regelnamen,
//! Pfade, Codeausschnitte, URLs —, den der Autor des gescannten Codes
//! mitkontrolliert. Diese Crate zieht daraus drei Konsequenzen:
//!
//! - **Kein Berichtsinhalt landet in einer Fehlermeldung.**
//!   [`harw_dod_cap::SensorError::MalformedSource`] sagt „unerwartete Form",
//!   nie, was genau unerwartet war — siehe [`report::parse_report`] und die
//!   zugehörigen Tests, die das ausdrücklich prüfen.
//! - **Längen sind begrenzt.** Jeder Freitext, der in ein `SecurityEvent`
//!   einfließt, läuft durch [`redact::sanitize_and_truncate`] und wird auf
//!   [`redact::MAX_DETAIL_LEN`] Bytes gekappt — ein zehn Megabyte großes
//!   Beschreibungsfeld ist damit kein Speicherangriff. Die Gesamtgröße
//!   jeder gelesenen Datei ist zusätzlich bereits durch
//!   `harw_dod_readfs::MAX_READ_BYTES` (1 MiB) begrenzt.
//! - **Steuerzeichen werden entschärft.** [`redact::sanitize_and_truncate`]
//!   ersetzt jedes Steuerzeichen (Zeilenumbrüche, den ANSI-Einleiter `ESC`,
//!   den gesamten C0/C1-Bereich) durch ein Leerzeichen, bevor gekürzt wird —
//!   ein eingebetteter Zeilenumbruch kann sonst eine Logzeile fälschen, eine
//!   ANSI-Sequenz eine Terminalausgabe manipulieren.
//!
//! # Nebenläufigkeit
//! Alle Typen dieser Crate sind reine, unveränderliche Werte oder
//! zustandslose Funktionen: `Send + Sync`, ohne inneres Locking.
//! [`sensor::ScanReportSensor::poll`] nimmt `&self` und öffnet für jeden
//! Aufruf neue Datei-Handles über `harw_dod_readfs`; parallele Aufrufe
//! verschiedener Threads stören sich nicht.
//!
//! # Fehler
//! [`harw_dod_cap::SensorError`] — der einzige Fehlertyp, den diese Crate
//! zurückgibt. Sie erfindet keine eigene Fehlermenge: die vier bestehenden
//! Varianten (`OutsideScope`, `SourceUnavailable`, `MalformedSource`, `Io`)
//! decken jeden Fehlerfall dieser Crate bereits inhaltsfrei ab (siehe
//! [`sensor::map_read_fs_error`] für die vollständige Übersetzungstabelle
//! aus `harw_dod_readfs::ReadFsError`).
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_cap::ReadScope;
//! use harw_dod_scanreport::ScanReportSensor;
//! use harw_dod_signals::Sensor;
//! use harw_types::SensorId;
//! use std::path::PathBuf;
//!
//! let scope = ReadScope::from_roots([PathBuf::from("/var/lib/harw/scan_reports")]);
//! let sensor = ScanReportSensor::new(SensorId::from_str("scanreport-0"), scope);
//! let reading = sensor.poll(jiff::Timestamp::UNIX_EPOCH)?;
//! for event in reading.events {
//!     println!("{event:?}");
//! }
//! # Ok::<(), harw_dod_cap::SensorError>(())
//! ```

mod cargo_audit;
mod redact;
mod report;
mod sarif;
mod sensor;

#[cfg(test)]
mod invariant_test;

pub use sensor::{ScanReportSensor, REPORT_HARDNESS};
