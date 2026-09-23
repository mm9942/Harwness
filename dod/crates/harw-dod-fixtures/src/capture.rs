//! `capture()` — baut einen Fixture-Fall aus einem laufenden System.
//!
//! # Verantwortungsbereich
//! [`capture`] ist das Gegenstück zum Handschreiben eines Fixture-Falls: sie
//! liest die Quelle eines Sensors von einem echten Host, schreibt eine
//! Momentaufnahme nach `<fall>/tree/` und das dazu gehörige `expect.json`
//! (siehe [`crate::fixture_io`] für dessen Format). Ein Sensor-Autor ruft sie
//! **einmalig** auf seinem eigenen Host auf, prüft `tree/` von Hand und
//! checkt beides ins Repository ein — deshalb steht sie in der normalen API
//! dieser Crate, nicht hinter `#[cfg(test)]`.
//!
//! # Rundlauf-Garantie
//! Das für `expect.json` verwendete `SensorReading` entsteht **nicht** durch
//! einen Poll gegen den echten Host, sondern durch einen Poll gegen die
//! frisch geschriebene `tree/`-Kopie selbst. Damit stimmen `tree/` und
//! `expect.json` per Konstruktion überein, unabhängig davon, ob sich der
//! Host zwischen Snapshot und Poll verändert hat — [`crate::sensor_suite!`]
//! akzeptiert das Ergebnis danach ohne weitere Anpassung (siehe die
//! Rundlauf-Tests in `tests/capture_round_trip.rs`).
//!
//! # Symlinks
//! Ein während des Snapshots angetroffener Symlink wird **aufgelöst und als
//! gewöhnliche Datei bzw. gewöhnliches Verzeichnis kopiert**, nicht als
//! Symlink im Ziel nachgebildet. Ein im Repository eingechecktes Symlink,
//! dessen Ziel außerhalb des Checkouts liegt, wäre auf einer anderen
//! Maschine oder in CI nicht auflösbar — [`harw_dod_cap::ReadScope::open`]
//! würde ihn dort als `SensorError::OutsideScope`/`Io` ablehnen, obwohl der
//! Fixture-Fall inhaltlich gültig ist.
//!
//! # Redaktion — Entscheidung und Begründung
//! Ein auf einem echten Host aufgenommener Baum trägt potenziell Hostnamen,
//! Benutzernamen oder cgroup-Pfade und landet danach im Repository. Zwei
//! Optionen standen offen: verlässliche, aber zwangsläufig unvollständige
//! Muster-Redaktion, oder eine deutliche Warnung. Diese Funktion tut
//! **beides**, mit einer klaren Grenze dazwischen:
//!
//! - **Mechanisch und verlässlich** ersetzt sie genau zwei exakt bekannte
//!   Zeichenketten — den aktuellen Benutzernamen (`$USER`/`$LOGNAME`) und den
//!   aktuellen Hostnamen (`/proc/sys/kernel/hostname`) — überall, wo sie
//!   byteweise als Teilstring auftauchen, sowohl in Dateiinhalten als auch in
//!   Pfadsegmenten, durch `<REDACTED-USER>` bzw. `<REDACTED-HOST>`. Das sind
//!   die einzigen zwei Werte, die diese Funktion mit Sicherheit kennt und
//!   ohne Rateheuristik erkennen kann.
//! - **Bewusst unterlassen** wird jede Muster-Suche nach „sieht aus wie ein
//!   Geheimnis" (IP-Adressen, beliebige cgroup-IDs, eingebettete Tokens):
//!   ein falsch-positiver Treffer würde legitime numerische Sensordaten
//!   verstümmeln, ein falsch-negativer Treffer bliebe unbemerkt — beides
//!   schlimmer als gar keine Heuristik.
//! - Deshalb trägt [`CaptureReport::warning`] **immer** einen unübersehbaren
//!   Hinweistext, unabhängig davon, ob eine Ersetzung stattgefunden hat: der
//!   Sensor-Autor muss `tree/` vor dem Einchecken selbst durchsehen. Diese
//!   Funktion selbst gibt keine Konsolenausgabe aus (kein `println!`/
//!   `eprintln!` — reine Bibliotheksfunktion); der Aufrufer entscheidet, wie
//!   er die Warnung anzeigt (siehe `# Examples`).
//!
//! # Bewusste Ausnahme von „kein direkter `std::fs`-Zugriff"
//! Wie [`harw_dod_readfs::glob::glob`] muss diese Funktion Verzeichnisse
//! auflisten, um den Baum überhaupt zu entdecken — `ReadScope` bietet dafür
//! kein Auflisten an. [`capture`] ruft deshalb `std::fs::read_dir` auf
//! Zwischenverzeichnissen des **Quellbaums** auf; jede so gefundene Datei
//! wird anschließend über [`harw_dod_readfs::read_to_string`] gelesen — also
//! durch denselben `ReadScope`, den auch ein echter Sensor benutzen würde.
//!
//! # Nebenläufigkeit
//! [`capture`] ist nicht für parallele Aufrufe auf **demselben** `case_dir`
//! ausgelegt (sie schreibt dorthin); parallele Aufrufe auf unterschiedlichen
//! `case_dir`-Werten stören sich nicht.
//!
//! # Fehler
//! [`crate::error::FixturesError`] — siehe dessen Dokumentation.
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_cap::{Bound, Capability, SensorHandle};
//! use harw_dod_signals::{Sensor, SensorReading};
//! use std::path::Path;
//!
//! #[derive(Debug)]
//! struct MySensor(SensorHandle<Bound>);
//! impl Sensor for MySensor {
//!     fn handle(&self) -> &SensorHandle<Bound> { &self.0 }
//!     fn poll(&self, _now: jiff::Timestamp) -> Result<SensorReading, harw_dod_cap::SensorError> {
//!         Ok(SensorReading::default())
//!     }
//! }
//!
//! let report = harw_dod_fixtures::capture::capture(
//!     MySensor,
//!     Capability::ReadSysfsThermal,
//!     Path::new("/sys/class/thermal"),
//!     Path::new("harw-dod-thermal/fixtures/this-laptop"),
//!     jiff::Timestamp::UNIX_EPOCH,
//! )?;
//! eprintln!("{}", report.warning);
//! # Ok::<(), harw_dod_fixtures::error::FixturesError>(())
//! ```

use std::path::{Path, PathBuf};

use harw_dod_cap::{Bound, Capability, ReadScope, SensorHandle};
use harw_dod_signals::Sensor;
use jiff::Timestamp;

use crate::error::FixturesResult;
use crate::fixture_io::write_expected;

/// Der Hinweistext, den jeder [`capture`]-Aufruf in [`CaptureReport::warning`]
/// zurückgibt.
const CAPTURE_WARNING: &str = "ACHTUNG: capture() hat nur den bekannten Benutzer- und Hostnamen \
mechanisch redigiert. Prüfe tree/ von Hand auf weitere host-identifizierende \
Inhalte (IP-Adressen, cgroup-IDs, Interna), bevor du es ins Repository \
eincheckst.";

/// Das Ergebnis eines [`capture`]-Aufrufs.
///
/// # Description
/// Reiner Berichtstyp — keine Methoden, nur Felder zur Diagnose, was
/// `capture()` getan hat. `warning` ist immer belegt (siehe
/// [`crate::capture`]-Moduldoku, Abschnitt „Redaktion").
///
/// # Arguments
/// Kein Konstruktor außerhalb dieses Moduls; entsteht ausschließlich als
/// Rückgabewert von [`capture`].
///
/// # Returns
/// Nicht zutreffend — dies ist ein Datentyp, keine Funktion.
///
/// # Errors
/// Keine eigenen Fehler; ein gescheiterter [`capture`]-Aufruf liefert
/// stattdessen `Err(`[`crate::error::FixturesError`]`)` und **kein**
/// `CaptureReport`.
///
/// # Examples
/// ```rust
/// let report = harw_dod_fixtures::CaptureReport {
///     case_dir: std::path::PathBuf::from("fixtures/typical"),
///     files_copied: 3,
///     redactions_applied: 0,
///     warning: "Prüfe tree/ von Hand.",
/// };
/// assert_eq!(report.files_copied, 3);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureReport {
    /// Das Fallverzeichnis, in das `tree/` und `expect.json` geschrieben
    /// wurden.
    pub case_dir: PathBuf,
    /// Anzahl der kopierten regulären Dateien.
    pub files_copied: usize,
    /// Anzahl der Stellen (Dateiinhalt oder Pfadsegment), an denen der
    /// aktuelle Benutzer- oder Hostname ersetzt wurde.
    pub redactions_applied: usize,
    /// Der unveränderliche Hinweistext zur manuellen Nachprüfung. Siehe
    /// [`crate::capture`]-Moduldoku, Abschnitt „Redaktion".
    pub warning: &'static str,
}

#[derive(Default)]
struct CaptureStats {
    files_copied: usize,
    redactions: usize,
}

/// Baut einen Fixture-Fall aus einem laufenden System.
///
/// # Description
/// Liest den Verzeichnisbaum unter `source_root` (begrenzt auf einen
/// [`harw_dod_cap::ReadScope`], der genau auf `source_root` zeigt) und
/// schreibt eine redigierte Kopie nach `case_dir/tree/`. Pollt anschließend
/// den mit `build` erzeugten Sensor gegen **diese Kopie** (nicht gegen den
/// echten Host — siehe Moduldoku, Abschnitt „Rundlauf-Garantie") und
/// schreibt das Ergebnis nach `case_dir/expect.json`.
///
/// # Arguments
/// - `build` (`impl Fn(SensorHandle<Bound>) -> S`): baut die konkrete
///   Sensor-Instanz aus einem gebundenen Griff.
/// - `capability` (`Capability`): die Fähigkeit, mit der der Griff gebaut
///   wird — üblicherweise dieselbe, mit der der Sensor produktiv läuft.
/// - `source_root` (`&Path`): die reale, auf dem aufrufenden Host
///   existierende Wurzel, die der Sensor produktiv liest (z. B.
///   `/sys/class/thermal`).
/// - `case_dir` (`&Path`): das Zielverzeichnis für diesen Fixture-Fall,
///   üblicherweise `harw-dod-<sensor>/fixtures/<fall-name>`. Wird angelegt,
///   falls nicht vorhanden; ein bereits vorhandenes `tree/` oder
///   `expect.json` wird überschrieben.
/// - `now` (`jiff::Timestamp`): die injizierte Zeit für den Probe-Poll.
///
/// # Returns
/// Ein [`CaptureReport`] mit Statistik und Redaktions-Warnung.
///
/// # Errors
/// - [`crate::error::FixturesError::Io`]: Lesen von `source_root` oder
///   Schreiben nach `case_dir` ist fehlgeschlagen.
/// - [`crate::error::FixturesError::Sensor`]: der Probe-Poll gegen die
///   frisch geschriebene Kopie ist mit einem [`harw_dod_cap::SensorError`]
///   gescheitert (z. B. weil `source_root` leer war).
/// - [`crate::error::FixturesError::Json`]: `expect.json` ließ sich nicht
///   kodieren.
///
/// # Concurrency
/// Nicht für parallele Aufrufe auf demselben `case_dir` ausgelegt (siehe
/// Moduldoku).
///
/// # Examples
/// Siehe [`crate::capture`]-Moduldoku.
pub fn capture<S, F>(
    build: F,
    capability: Capability,
    source_root: &Path,
    case_dir: &Path,
    now: Timestamp,
) -> FixturesResult<CaptureReport>
where
    S: Sensor,
    F: Fn(SensorHandle<Bound>) -> S,
{
    let tree_dir = case_dir.join("tree");
    std::fs::create_dir_all(&tree_dir)?;

    let source_scope = ReadScope::from_roots([source_root.to_path_buf()]);
    let pairs = redaction_pairs();
    let mut stats = CaptureStats::default();
    capture_tree(&source_scope, source_root, &tree_dir, &pairs, &mut stats)?;

    let capture_scope = ReadScope::from_roots([tree_dir.clone()]);
    let handle = crate::build_handle(capability, capture_scope);
    let sensor = build(handle);
    let reading = sensor
        .poll(now)
        .map_err(crate::error::FixturesError::from)?;

    write_expected(case_dir, now, &reading)?;

    Ok(CaptureReport {
        case_dir: case_dir.to_path_buf(),
        files_copied: stats.files_copied,
        redactions_applied: stats.redactions,
        warning: CAPTURE_WARNING,
    })
}

/// Die beiden exakt bekannten Redaktionspaare (Suchtext, Ersatztext) — siehe
/// Moduldoku, Abschnitt „Redaktion", für die Begründung dieser Grenze.
fn redaction_pairs() -> Vec<(String, &'static str)> {
    let mut pairs = Vec::new();

    let user = std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("LOGNAME").ok());
    if let Some(user) = user {
        if !user.trim().is_empty() {
            pairs.push((user, "<REDACTED-USER>"));
        }
    }

    if let Ok(hostname) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let hostname = hostname.trim().to_owned();
        if !hostname.is_empty() {
            pairs.push((hostname, "<REDACTED-HOST>"));
        }
    }

    pairs
}

/// Ersetzt jedes bekannte Redaktionspaar in `input`, byteweise als
/// Teilstring. Liefert den (ggf. unveränderten) Text und die Trefferzahl.
fn redact_text(input: &str, pairs: &[(String, &'static str)]) -> (String, usize) {
    let mut out = input.to_owned();
    let mut hits = 0usize;
    for (needle, replacement) in pairs {
        if needle.is_empty() {
            continue;
        }
        let count = out.matches(needle.as_str()).count();
        if count > 0 {
            hits += count;
            out = out.replace(needle.as_str(), replacement);
        }
    }
    (out, hits)
}

/// Kopiert `root` rekursiv nach `dest`, redigiert Pfadsegmente und
/// Dateiinhalte über `pairs` und liest jede Datei über `scope` (siehe
/// Moduldoku, Abschnitt „Bewusste Ausnahme von 'kein direkter
/// std::fs-Zugriff'").
fn capture_tree(
    scope: &ReadScope,
    root: &Path,
    dest: &Path,
    pairs: &[(String, &'static str)],
    stats: &mut CaptureStats,
) -> FixturesResult<()> {
    let Ok(entries) = std::fs::read_dir(root) else {
        // Ein Zwischenverzeichnis, das während des Snapshots verschwindet
        // oder nicht lesbar ist, bricht die gesamte Aufnahme nicht ab —
        // dieselbe Großzügigkeit wie `harw_dod_readfs::glob::glob`.
        return Ok(());
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !scope.allows(&canonical) {
            continue;
        }

        let name = entry.file_name().to_string_lossy().into_owned();
        let (redacted_name, name_hits) = redact_text(&name, pairs);
        stats.redactions += name_hits;
        let dest_path = dest.join(&redacted_name);

        let is_dir = canonical.is_dir();
        if is_dir {
            std::fs::create_dir_all(&dest_path)?;
            capture_tree(scope, &path, &dest_path, pairs, stats)?;
            continue;
        }

        match harw_dod_readfs::read_to_string(scope, &path) {
            Ok(content) => {
                let (redacted, hits) = redact_text(&content, pairs);
                stats.redactions += hits;
                std::fs::write(&dest_path, redacted)?;
                stats.files_copied += 1;
            }
            Err(_) => {
                // Nicht-UTF-8 oder anderweitig über den typisierten Leser
                // nicht lesbar: bewahre die Rohbytes, ohne Textredaktion —
                // besser eine unredigierte Kopie als ein stiller Datenverlust.
                if let Ok(bytes) = std::fs::read(&path) {
                    std::fs::write(&dest_path, bytes)?;
                    stats.files_copied += 1;
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use harw_dod_cap::SensorError;
    use harw_dod_signals::{HostSample, SensorReading};
    use harw_types::SensorId;

    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[derive(Debug)]
    struct EchoValueSensor {
        handle: SensorHandle<Bound>,
    }

    impl From<SensorHandle<Bound>> for EchoValueSensor {
        fn from(handle: SensorHandle<Bound>) -> Self {
            Self { handle }
        }
    }

    impl Sensor for EchoValueSensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }

        fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
            let Some(root) = self.handle.scope().roots().next() else {
                return Err(SensorError::SourceUnavailable);
            };
            let value = harw_dod_readfs::parse_i64(self.handle.scope(), &root.join("value"))
                .map_err(|_| SensorError::MalformedSource)?;
            Ok(SensorReading {
                samples: vec![HostSample {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    metric: Cow::Borrowed("captured_value"),
                    value: value as f64,
                }],
                events: vec![],
            })
        }
    }

    #[test]
    fn test_capture_writes_tree_and_expect_json() -> TestResult {
        let source = tempfile::tempdir().map_err(ctx("source tempdir"))?;
        std::fs::write(source.path().join("value"), "7\n").map_err(ctx("write source value"))?;

        let target = tempfile::tempdir().map_err(ctx("target tempdir"))?;
        let case_dir = target.path().join("typical");

        let report = capture(
            EchoValueSensor::from,
            Capability::ReadProcStat,
            source.path(),
            &case_dir,
            Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("capture muss gelingen"))?;

        assert_eq!(report.case_dir, case_dir);
        assert_eq!(report.files_copied, 1);
        assert!(!report.warning.is_empty());

        let copied = std::fs::read_to_string(case_dir.join("tree").join("value"))
            .map_err(ctx("kopierte value-Datei lesbar"))?;
        assert_eq!(copied, "7\n");

        let expect_json = std::fs::read_to_string(case_dir.join("expect.json"))
            .map_err(ctx("expect.json lesbar"))?;
        assert!(expect_json.contains("captured_value"));
        assert!(expect_json.contains(&SensorId::from_str(crate::FIXTURE_SENSOR_ID).to_string()));
        Ok(())
    }

    #[test]
    fn test_capture_round_trip_sensor_suite_accepts_output() -> TestResult {
        let source = tempfile::tempdir().map_err(ctx("source tempdir"))?;
        std::fs::write(source.path().join("value"), "99\n").map_err(ctx("write source value"))?;

        let target = tempfile::tempdir().map_err(ctx("target tempdir"))?;
        let case_dir = target.path().join("typical");

        capture(
            EchoValueSensor::from,
            Capability::ReadProcStat,
            source.path(),
            &case_dir,
            Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("capture muss gelingen"))?;

        // Rundlauf: dieselbe tree/-Kopie erneut gepollt muss exakt das in
        // expect.json hinterlegte SensorReading reproduzieren — die
        // Determinismus-Prüfung aus `harness`, hier direkt nachvollzogen.
        let expected =
            crate::fixture_io::read_expected(&case_dir).map_err(ctx("expect.json lesbar"))?;
        let scope = ReadScope::from_roots([case_dir.join("tree")]);
        let sensor = EchoValueSensor::from(crate::build_handle(Capability::ReadProcStat, scope));
        let reading = sensor
            .poll(expected.now)
            .map_err(ctx("erneuter Poll gegen die Kopie muss gelingen"))?;
        assert_eq!(reading, expected.reading.into_reading());
        Ok(())
    }

    #[test]
    fn test_redact_text_replaces_known_needle() {
        let pairs = vec![("geheim".to_owned(), "<REDACTED-USER>")];
        let (out, hits) = redact_text("user=geheim; path=/home/geheim/x", &pairs);
        assert_eq!(hits, 2);
        assert_eq!(out, "user=<REDACTED-USER>; path=/home/<REDACTED-USER>/x");
    }

    #[test]
    fn test_redact_text_is_noop_without_match() {
        let pairs = vec![("nichtvorhanden".to_owned(), "<REDACTED-USER>")];
        let (out, hits) = redact_text("45000", &pairs);
        assert_eq!(hits, 0);
        assert_eq!(out, "45000");
    }

    #[test]
    fn test_capture_report_warning_is_always_present() -> TestResult {
        let source = tempfile::tempdir().map_err(ctx("source tempdir"))?;
        std::fs::write(source.path().join("value"), "1\n").map_err(ctx("write"))?;
        let target = tempfile::tempdir().map_err(ctx("target tempdir"))?;
        let case_dir = target.path().join("case");

        let report = capture(
            EchoValueSensor::from,
            Capability::ReadProcStat,
            source.path(),
            &case_dir,
            Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("capture muss gelingen"))?;

        assert!(report.warning.contains("Prüfe tree/ von Hand"));
        Ok(())
    }
}
