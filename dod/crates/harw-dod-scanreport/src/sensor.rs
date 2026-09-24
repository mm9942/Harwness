//! `ScanReportSensor`: liest Berichte fremder Scanner, führt keinen aus.
//!
//! # Verantwortungsbereich
//! Implementiert `trait` [`harw_dod_signals::Sensor`] für genau eine Quelle:
//! ein Verzeichnis mit Berichtsdateien fremder Scanner (typischerweise
//! [`harw_home::paths::scan_reports_dir`]). Jeder **Inhaltszugriff** läuft
//! über `harw_dod_readfs` — [`harw_dod_readfs::glob::glob`] zum Auffinden der
//! Berichtsdateien, [`harw_dod_readfs::read_to_string`] zum Lesen ihres
//! Inhalts.
//!
//! **Eine bewusste, eng begrenzte Ausnahme seit F-064:**
//! [`open_if_regular_file`] ruft `std::fs::OpenOptions` mit `O_NONBLOCK`
//! direkt auf, um vor jedem Inhaltszugriff zu prüfen, dass ein Glob-Treffer
//! tatsächlich eine reguläre Datei ist — eine FIFO ohne Schreiber ließ
//! `File::open` (wie es `harw_dod_readfs::read_to_string` innen ausführt)
//! zuvor unbegrenzt blockieren. Diese eine Funktion liest nie Inhalt aus dem
//! geöffneten Deskriptor und trifft keine Bereichsentscheidung (das bleibt
//! Sache von `harw_dod_cap::ReadScope`, wie gehabt über `harw_dod_readfs`)
//! — sie beantwortet ausschließlich „ist das eine reguläre Datei?", siehe
//! deren Dokumentation für die Begründung und das verbleibende Restrisiko.
//!
//! # Warum genau zwei Dateiendungen
//! [`glob_patterns_for_root`] baut Muster für `*.json` und `*.sarif` —
//! nicht für jede beliebige Datei im Verzeichnis. Das ist eine bewusste
//! Einschränkung, keine willkürliche: `scan_reports_dir` ist ein von
//! externen Werkzeugen befülltes Verzeichnis (siehe dessen Moduldoku in
//! `harw-home`), und ein Muster ohne Endungsfilter (`"*"`) würde auch
//! Unterverzeichnisse als Kandidaten auflisten, deren Inhalt als Datei zu
//! öffnen mit einem E/A-Fehler scheitert. Die beiden festen Endungen decken
//! beide unterstützten Formate ab (SARIF üblicherweise `.sarif` oder
//! `.sarif.json`, `cargo audit --json`-Ausgaben üblicherweise `.json`) und
//! vermeiden diesen Fall, ohne eine zusätzliche Dateisystem-Abfrage
//! (`is_dir`) zu brauchen, die über `harw_dod_readfs`s Vertrag hinausginge.
//!
//! # Nebenläufigkeit
//! `ScanReportSensor` hält ausschließlich unveränderliche Daten
//! ([`harw_dod_cap::SensorHandle<harw_dod_cap::Bound>`]); `Send + Sync`
//! automatisch. [`Sensor::poll`] nimmt `&self` und liest bei jedem Aufruf
//! neu — kein Zustand überlebt einen einzelnen Abruf.
//!
//! # Fehler
//! [`harw_dod_cap::SensorError`] — inhaltsfrei, siehe `crate`-Moduldoku.
//! [`map_read_fs_error`] übersetzt die zusätzlichen Fehlerformen von
//! `harw-dod-readfs` (Größengrenze, Glob-Mustergültigkeit) verlustfrei in
//! dieselbe, bereits bestehende Fehlermenge, ohne eine eigene zu erfinden.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
use harw_dod_signals::{Hardness, Sensor, SensorReading};
use harw_types::SensorId;
use jiff::Timestamp;

use crate::report::parse_report;

/// Die Dateiendungen, unter denen diese Crate Berichte sucht.
///
/// Siehe Moduldoku, Abschnitt „Warum genau zwei Dateiendungen".
const REPORT_EXTENSIONS: [&str; 2] = ["json", "sarif"];

/// Die epistemische Härte, mit der Ereignisse dieses Sensors in einen
/// [`harw_dod_signals::SecurityEvidence`]-Beleg einfließen sollten.
///
/// # Description
/// [`harw_dod_signals::SecurityEvent`] trägt selbst kein Härte-Feld — Härte
/// ist Sache der Beleg- und Befundbildung oberhalb des Sensors (künftig
/// `harw-dod-rules::Finding<S>`, Knoten AW4-03). Diese Konstante ist die
/// verbindliche Antwort dieser Crate auf die Frage, welche Härte ein
/// Konsument ihren Ereignissen zuschreiben soll, solange keine
/// feldbasierte Übertragung existiert.
///
/// Ein Scanner-Befund ist die Behauptung eines fremden Werkzeugs, keine vom
/// Harness selbst beobachtete Tatsache — [`Hardness::Observed`] scheidet
/// damit aus. Zwischen [`Hardness::Correlated`] und [`Hardness::Inferred`]
/// fällt die Wahl auf **`Inferred`**: sowohl ein SARIF-Regeltreffer (das
/// Werkzeug zieht aus einem Codemuster einen Schluss auf einen Defekt) als
/// auch ein `cargo audit`-Treffer (das Werkzeug zieht aus einer
/// Versionsübereinstimmung einen Schluss auf Verwundbarkeit) ziehen einen
/// Schluss, der über das hinausgeht, was diese Crate selbst beobachtet hat
/// — sie hat nur eine Berichtsdatei gelesen, nicht den behaupteten
/// Sachverhalt selbst verifiziert. Das ist exakt die in
/// `harw-dod-signals::evidence`-Moduldoku gegebene Definition von
/// `Inferred`, und es ist die ehrlichere der beiden verbleibenden Stufen:
/// ein falsch-positiver Scanner-Befund bleibt möglich, solange niemand ihn
/// bestätigt hat.
pub const REPORT_HARDNESS: Hardness = Hardness::Inferred;

/// Ein Sensor, der Berichte fremder Scanner liest.
///
/// # Description
/// Siehe `crate`-Moduldoku für die vollständige Beschreibung, insbesondere
/// die Kommandozeilen-Invariante. Trägt genau die Fähigkeit
/// [`Capability::ReadScanReports`].
#[derive(Debug)]
pub struct ScanReportSensor {
    handle: SensorHandle<Bound>,
}

impl ScanReportSensor {
    /// Baut einen Sensor, dessen Lesebereich exakt `scope` ist.
    ///
    /// # Description
    /// Allgemeiner Konstruktor für einen bereits aufgebauten [`ReadScope`] —
    /// etwa einen Testbereich, der auf ein temporäres Verzeichnis zeigt.
    /// Für den üblichen Fall, den Root-Space-Berichtsordner zu verwenden,
    /// siehe [`Self::for_home`].
    ///
    /// # Arguments
    /// - `id` (`SensorId`): Kennung dieses Sensors.
    /// - `scope` (`ReadScope`): der Lesebereich, an den der Griff gebunden wird.
    ///
    /// # Returns
    /// Einen gebundenen `ScanReportSensor`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::ReadScope;
    /// use harw_dod_scanreport::ScanReportSensor;
    /// use harw_types::SensorId;
    /// use std::path::PathBuf;
    ///
    /// let scope = ReadScope::from_roots([PathBuf::from("/tmp/scan-reports")]);
    /// let _sensor = ScanReportSensor::new(SensorId::from_str("scanreport-0"), scope);
    /// ```
    #[must_use]
    pub fn new(id: SensorId, scope: ReadScope) -> Self {
        Self {
            handle: SensorHandle::new(id, Capability::ReadScanReports).bind(scope),
        }
    }

    /// Baut einen Sensor, dessen Lesebereich
    /// [`harw_home::paths::scan_reports_dir`] von `home` ist.
    ///
    /// # Description
    /// Bequemlichkeitskonstruktor für den Regelfall: der Root-Space-Ordner
    /// für Fremdscanner-Berichte. Löst den Pfad nur auf, legt ihn nicht an
    /// (siehe die Moduldoku von `harw_home::paths::scan_reports_dir`) — ein
    /// noch nicht existierendes Verzeichnis führt beim ersten
    /// [`Sensor::poll`] zu einem leeren [`SensorReading`], nicht zu einem
    /// Fehler (siehe `crate`-Moduldoku).
    ///
    /// # Arguments
    /// - `id` (`SensorId`): Kennung dieses Sensors.
    /// - `home` (`&std::path::Path`): Root-Space, dessen Berichte gelesen werden.
    ///
    /// # Returns
    /// Einen gebundenen `ScanReportSensor`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_dod_scanreport::ScanReportSensor;
    /// use harw_types::SensorId;
    /// use std::path::Path;
    ///
    /// let home = Path::new("/tmp/harw-home-example");
    /// let _sensor = ScanReportSensor::for_home(SensorId::from_str("scanreport-0"), home);
    /// ```
    #[must_use]
    pub fn for_home(id: SensorId, home: &Path) -> Self {
        let dir = harw_home::paths::scan_reports_dir(home);
        Self::new(id, ReadScope::from_roots([dir]))
    }
}

impl Sensor for ScanReportSensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    /// Liest jede passende Berichtsdatei im Lesebereich genau einmal aus.
    ///
    /// # Errors
    /// [`SensorError::MalformedSource`], wenn eine gefundene Datei kein
    /// syntaktisch gültiges JSON ist oder keiner der beiden unterstützten
    /// Formen entspricht ([`crate::report::parse_report`]);
    /// [`SensorError::OutsideScope`]/[`SensorError::Io`], wenn das Lesen
    /// selbst scheitert ([`harw_dod_readfs::read_to_string`]). Ein leerer
    /// oder (noch) nicht existierender Berichtsordner liefert ein leeres
    /// [`SensorReading`], **keinen** Fehler. Ein Kandidat, der sich beim
    /// Öffnen nicht als reguläre Datei erweist (FIFO, Gerätedatei oder ein
    /// Verzeichnis, dessen Name zufällig auf `.json`/`.sarif` endet — F-064),
    /// wird stillschweigend übersprungen statt den Abruf scheitern zu
    /// lassen oder unbegrenzt zu blockieren; siehe [`open_if_regular_file`].
    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let scope = self.handle.scope();

        let mut paths: BTreeSet<PathBuf> = BTreeSet::new();
        for root in scope.roots() {
            for pattern in glob_patterns_for_root(root) {
                let matches =
                    harw_dod_readfs::glob::glob(scope, &pattern).map_err(map_read_fs_error)?;
                paths.extend(matches);
            }
        }

        let mut events = Vec::new();
        for path in paths {
            // F-064: vor jedem Lesen erst per nicht-blockierendem Öffnen +
            // `fstat` sicherstellen, dass der Kandidat tatsächlich eine
            // reguläre Datei ist. Ein `File::open` (wie es
            // `harw_dod_readfs::read_to_string` unten ausführt) blockiert
            // sonst unbegrenzt auf einer FIFO ohne Schreiber — ein
            // unprivilegierter Nutzer könnte damit gezielt jeden künftigen
            // Abruf dieses Sensors dauerhaft einfrieren.
            if open_if_regular_file(&path).is_none() {
                continue;
            }

            let content =
                harw_dod_readfs::read_to_string(scope, &path).map_err(map_read_fs_error)?;
            let mut parsed = parse_report(&content, self.handle.id(), now)?;
            events.append(&mut parsed);
        }

        Ok(SensorReading {
            samples: Vec::new(),
            events,
        })
    }
}

/// Öffnet `path` nicht-blockierend und liefert `Some`, nur wenn `fstat` auf
/// dem bereits offenen Deskriptor bestätigt, dass es sich um eine reguläre
/// Datei handelt — sonst `None`, ohne dass das zugrunde liegende `open()`
/// jemals auf einer FIFO ohne Schreiber blockieren konnte (F-064).
///
/// # Warum `O_NONBLOCK` statt einer vorherigen `stat`/`symlink_metadata`-Prüfung
/// Eine Typprüfung **vor** dem Öffnen hätte eine TOCTOU-Lücke: der Dateityp
/// könnte sich zwischen Prüfung und `open()` ändern (dieselbe Lücke bliebe
/// zwischen einem `stat` hier und dem späteren, tatsächlichen Lesen über
/// [`harw_dod_readfs::read_to_string`] ohnehin bestehen — siehe „Restrisiko"
/// unten). `O_NONBLOCK` lässt `open()` dagegen auf **jedem** Dateityp sofort
/// zurückkehren: bei einer FIFO ohne Schreiber liefert es sofort einen
/// Deskriptor, der (noch) keine Daten trägt, statt zu blockieren, bis ein
/// Schreiber verbindet. Das anschließende `fstat` prüft den Typ der
/// tatsächlich geöffneten Datei anhand ihres Deskriptors — kein erneuter
/// Pfadzugriff, also keine zusätzliche TOCTOU-Lücke zwischen Typprüfung und
/// diesem einen `open()`-Aufruf.
///
/// Kein `harw-fsutil`-`O_NOFOLLOW`-Pfad: `harw-fsutil` ist keine
/// Abhängigkeit dieser Crate; Symlink-Auflösung ist ohnehin
/// bereits Aufgabe von `harw_dod_cap::ReadScope` (über
/// [`harw_dod_readfs::glob::glob`]/[`harw_dod_readfs::read_to_string`]) —
/// diese Funktion prüft ausschließlich den Dateityp, nicht die
/// Bereichszugehörigkeit.
///
/// # Warum das Literal `0o4000` statt einer `libc`/`rustix`-Abhängigkeit
/// `O_NONBLOCK` ist auf Linux ABI-stabil `0o4000` (`<fcntl.h>`, `asm-generic`);
/// dieser Workspace baut ausschließlich für Linux (x86_64/aarch64).
/// Eine neue Abhängigkeit nur für diese eine, plattformweit garantierte
/// Konstante wäre eine unnötige `dep-request`.
///
/// # Restrisiko (TOCTOU)
/// Dieser Aufruf öffnet, prüft den Typ und schließt den Deskriptor sofort
/// wieder (er liest nie Inhalt aus ihm); die eigentliche Leseoperation
/// öffnet `path` danach über [`harw_dod_readfs::read_to_string`] ein
/// zweites Mal. Zwischen beiden Aufrufen könnte ein Angreifer mit
/// Schreibzugriff auf dasselbe Verzeichnis den Pfad erneut gegen eine FIFO
/// austauschen — dieselbe Restlücke, die auch `harw-dod-cap`s
/// `ReadScope::open` zwischen Kanonisierung und `File::open` trägt (offene
/// Annahme). Ein vollständiger Fix bräuchte eine `openat2`-gestützte
/// Grundfunktion in `harw-dod-cap`/`harw-dod-readfs` (außerhalb dieser
/// Zuständigkeit) statt eines zweiten, unabhängigen `open()`-Aufrufs hier.
/// Diese Korrektur schließt den **unbedingten** Block auf einer dauerhaft
/// unbeschriebenen FIFO vollständig — das war die eigentliche Störung
/// („alle 10 Sensoren stehen"), nicht das theoretische Wettlauffenster.
///
/// # Arguments
/// - `path` (`&Path`): der zu prüfende Kandidat, unverändert wie von
///   [`harw_dod_readfs::glob::glob`] geliefert.
///
/// # Returns
/// `Some(File)` (bereits geöffnet, aber ungenutzt — der Aufrufer verwirft
/// ihn und liest über [`harw_dod_readfs::read_to_string`] neu) für eine
/// reguläre Datei; `None`, wenn `path` nicht existiert, sich nicht öffnen
/// lässt, oder etwas anderes als eine reguläre Datei ist (FIFO,
/// Gerätedatei, Socket, Verzeichnis).
fn open_if_regular_file(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    /// `O_NONBLOCK`, siehe Funktionsdokumentation für die Begründung des
    /// Literals.
    const O_NONBLOCK: i32 = 0o4000;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NONBLOCK)
        .open(path)
        .ok()?;

    let metadata = file.metadata().ok()?;
    if metadata.file_type().is_file() {
        Some(file)
    } else {
        None
    }
}

/// Baut die Glob-Muster, mit denen `root` nach Berichtsdateien durchsucht
/// wird — eines je [`REPORT_EXTENSIONS`]-Eintrag.
///
/// [`harw_dod_readfs::glob::glob`] durchsucht immer ab `/` (siehe dessen
/// Moduldoku); ein Muster muss deshalb der vollständige, bereichsrelative
/// (nicht mit `/` beginnende) Pfad sein. Diese Funktion baut ihn aus `root`
/// selbst, nicht aus einer zweiten, unabhängig gepflegten Pfadangabe.
fn glob_patterns_for_root(root: &Path) -> Vec<String> {
    let root_text = root.to_string_lossy();
    let relative = root_text.strip_prefix('/').unwrap_or(&root_text);

    REPORT_EXTENSIONS
        .iter()
        .map(|extension| {
            if relative.is_empty() {
                format!("*.{extension}")
            } else {
                format!("{relative}/*.{extension}")
            }
        })
        .collect()
}

/// Übersetzt einen [`harw_dod_readfs::ReadFsError`] verlustfrei in einen
/// [`SensorError`], ohne eine eigene Fehlermenge einzuführen.
///
/// `Scope` reicht den bereits inhaltsfreien inneren [`SensorError`]
/// unverändert durch. `TooLarge` (die Datei überschreitet
/// `harw_dod_readfs::MAX_READ_BYTES`) und `GlobLimitExceeded` (das
/// Berichtsverzeichnis überschreitet `harw_dod_readfs::glob`s Kandidaten-
/// oder Mustergrenze — z. B. mehr als `MAX_GLOB_CANDIDATES` Dateien) werden
/// beide als [`SensorError::MalformedSource`] gewertet: eine Quelle dieser
/// Größe hat für diesen Sensor keine erwartbare Form, unabhängig davon, ob
/// die Grenze am Dateiinhalt oder an der Verzeichnisgröße greift. Die beiden
/// Glob-Musterfehler (`GlobPatternAbsolute`, `GlobPatternTraversal`)
/// betreffen ausschließlich von dieser Crate selbst gebaute Muster
/// ([`glob_patterns_for_root`]), nie einen vom Bericht gelieferten Wert;
/// ihr Auftreten wäre ein interner Konstruktionsfehler dieser Crate, kein
/// Aussagefehler über die Quelle — sie werden konservativ als
/// [`SensorError::SourceUnavailable`] gemeldet.
fn map_read_fs_error(err: harw_dod_readfs::ReadFsError) -> SensorError {
    match err {
        harw_dod_readfs::ReadFsError::Scope(inner) => inner,
        harw_dod_readfs::ReadFsError::TooLarge { .. }
        | harw_dod_readfs::ReadFsError::GlobLimitExceeded { .. } => SensorError::MalformedSource,
        harw_dod_readfs::ReadFsError::GlobPatternAbsolute { .. }
        | harw_dod_readfs::ReadFsError::GlobPatternTraversal { .. } => {
            SensorError::SourceUnavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::fs;

    fn sensor_id() -> SensorId {
        SensorId::from_str("scanreport-0")
    }

    fn sensor_for(dir: &Path) -> ScanReportSensor {
        let scope = ReadScope::from_roots([dir.to_path_buf()]);
        ScanReportSensor::new(sensor_id(), scope)
    }

    const VALID_SARIF: &str = r#"{
        "runs": [
            {
                "results": [
                    {"ruleId": "R1", "level": "error", "message": {"text": "erster Befund"}},
                    {"ruleId": "R2", "level": "warning", "message": {"text": "zweiter Befund"}}
                ]
            }
        ]
    }"#;

    /// Baut ein einzelnes SARIF-Ergebnis mit `text` als Nachricht, korrekt
    /// JSON-kodiert über `serde_json`. Genutzt statt handgeschriebener
    /// Roh-String-Fixtures, wenn `text` selbst Steuerzeichen enthalten soll
    /// (Zeilenumbrüche, ANSI-Escape) — `serde_json` übernimmt die nach
    /// JSON-Spezifikation korrekte Eskapierung, statt sie von Hand im
    /// Testquelltext nachzubilden.
    fn sarif_with_message(text: &str) -> String {
        serde_json::json!({
            "runs": [
                {"results": [{"ruleId": "R", "message": {"text": text}}]}
            ]
        })
        .to_string()
    }

    /// Test-Hilfsfunktion: erzeugt ein temporäres Verzeichnis, meldet einen
    /// Fehlschlag als [`TestError`] statt zu paniken (Bible R087/R165).
    fn temp_dir() -> TestResult<tempfile::TempDir> {
        tempfile::tempdir().map_err(ctx("tempdir"))
    }

    /// Test-Hilfsfunktion: schreibt eine Fixture-Datei, meldet einen
    /// Fehlschlag mit dem übergebenen Kontext als [`TestError`].
    fn write_fixture(path: &Path, contents: impl AsRef<[u8]>, context: &'static str) -> TestResult {
        fs::write(path, contents).map_err(ctx(context))
    }

    /// Test-Hilfsfunktion: legt ein Verzeichnis an, meldet einen Fehlschlag
    /// mit dem übergebenen Kontext als [`TestError`].
    fn make_dir(path: &Path, context: &'static str) -> TestResult {
        fs::create_dir(path).map_err(ctx(context))
    }

    // --- Explizit geforderte Einzelfälle ------------------------------------

    #[test]
    fn test_valid_report_yields_expected_security_events() -> TestResult {
        let dir = temp_dir()?;
        write_fixture(
            &dir.path().join("report.sarif"),
            VALID_SARIF,
            "write fixture",
        )?;

        let sensor = sensor_for(dir.path());
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("gültiger Bericht muss erfolgreich gelesen werden"))?;

        assert_eq!(reading.samples.len(), 0);
        assert_eq!(reading.events.len(), 2);
        for event in &reading.events {
            assert_eq!(event.sensor, sensor_id());
            assert_eq!(event.observed_at, Timestamp::UNIX_EPOCH);
            assert!(event.actor.is_none());
        }
        Ok(())
    }

    #[test]
    fn test_empty_directory_yields_empty_reading_without_error() -> TestResult {
        let dir = temp_dir()?;
        let sensor = sensor_for(dir.path());

        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("ein leeres Verzeichnis darf keinen Fehler auslösen"))?;

        assert!(reading.samples.is_empty());
        assert!(reading.events.is_empty());
        Ok(())
    }

    #[test]
    fn test_missing_directory_yields_empty_reading_without_error() -> TestResult {
        // `scan_reports_dir` wird nur benannt, nie angelegt (siehe
        // `harw-home`-Moduldoku) -- ein Sensor muss auch dann funktionieren,
        // wenn noch kein Scanner je einen Bericht abgelegt hat.
        let parent = temp_dir()?;
        let missing = parent.path().join("scan_reports");
        let sensor = sensor_for(&missing);

        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("ein fehlendes Verzeichnis darf keinen Fehler auslösen"))?;
        assert!(reading.events.is_empty());
        Ok(())
    }

    #[test]
    fn test_oversized_description_field_is_truncated_not_rejected() -> TestResult {
        let dir = temp_dir()?;
        let huge_description = "X".repeat(crate::redact::MAX_DETAIL_LEN * 10);
        let json = serde_json::json!({
            "vulnerabilities": {
                "list": [{
                    "advisory": {
                        "id": "RUSTSEC-2099-0001",
                        "title": "t",
                        "description": huge_description,
                    },
                    "package": {"name": "p", "version": "1.0.0"},
                }]
            }
        })
        .to_string();
        write_fixture(&dir.path().join("audit.json"), json, "write fixture")?;

        let sensor = sensor_for(dir.path());
        let reading = sensor.poll(Timestamp::UNIX_EPOCH).map_err(ctx(
            "übergroßes Feld darf den Bericht nicht scheitern lassen",
        ))?;

        assert_eq!(reading.events.len(), 1);
        let harw_dod_signals::EventKind::StructureDrift { detail, .. } = &reading.events[0].kind
        else {
            return Err(TestError::Unexpected("erwartete StructureDrift".to_owned()));
        };
        assert!(detail.len() < huge_description.len());
        assert!(!detail.is_empty());
        Ok(())
    }

    #[test]
    fn test_control_characters_in_free_text_are_neutralized() -> TestResult {
        let dir = temp_dir()?;
        let mut text_with_control = String::from("zeile eins");
        text_with_control.push('\n');
        text_with_control.push_str("zeile zwei");
        text_with_control.push('\u{1b}');
        text_with_control.push_str("[31mrot");
        text_with_control.push('\u{1b}');
        text_with_control.push_str("[0m");

        let json = sarif_with_message(&text_with_control);
        write_fixture(&dir.path().join("report.json"), json, "write fixture")?;

        let sensor = sensor_for(dir.path());
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("muss gelingen"))?;

        assert_eq!(reading.events.len(), 1);
        let harw_dod_signals::EventKind::StructureDrift { detail, .. } = &reading.events[0].kind
        else {
            return Err(TestError::Unexpected("erwartete StructureDrift".to_owned()));
        };
        assert!(!detail.contains('\n'));
        assert!(!detail.contains('\u{1b}'));
        Ok(())
    }

    #[test]
    fn test_malformed_json_error_message_does_not_contain_report_content() -> TestResult {
        let dir = temp_dir()?;
        let secret_marker = "GEHEIMNIS-1234567890";
        write_fixture(
            &dir.path().join("broken.json"),
            format!("{{ das ist kaputt: {secret_marker}"),
            "write fixture",
        )?;

        let sensor = sensor_for(dir.path());
        let Err(err) = sensor.poll(Timestamp::UNIX_EPOCH) else {
            return Err(TestError::Unexpected(
                "kaputtes JSON muss scheitern".to_owned(),
            ));
        };

        let message = err.to_string();
        assert!(!message.contains(secret_marker));
        assert!(matches!(err, SensorError::MalformedSource));
        Ok(())
    }

    #[test]
    fn test_report_hardness_is_not_observed() {
        assert_ne!(REPORT_HARDNESS, Hardness::Observed);
        assert_eq!(REPORT_HARDNESS, Hardness::Inferred);
    }

    // --- Die sechs Fixture-Prüfungen (von Hand, siehe crate-Moduldoku) ------

    /// 1. Determinismus: derselbe Bericht ergibt zweimal dasselbe Ergebnis.
    #[test]
    fn test_check_determinism_same_input_yields_equal_reading() -> TestResult {
        let dir = temp_dir()?;
        write_fixture(
            &dir.path().join("report.sarif"),
            VALID_SARIF,
            "write fixture",
        )?;
        let sensor = sensor_for(dir.path());

        let first = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("erster Abruf"))?;
        let second = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("zweiter Abruf"))?;
        assert_eq!(first, second);
        Ok(())
    }

    /// 2. Inhaltsfreiheit: der Fehlertext bei kaputtem JSON nennt weder den
    ///    Berichtsinhalt noch den aufgelösten Pfad der betroffenen Datei.
    #[test]
    fn test_check_content_freedom_error_never_echoes_source_paths() -> TestResult {
        let dir = temp_dir()?;
        write_fixture(
            &dir.path().join("broken.json"),
            "{ nicht valide",
            "write fixture",
        )?;
        let sensor = sensor_for(dir.path());

        let Err(err) = sensor.poll(Timestamp::UNIX_EPOCH) else {
            return Err(TestError::Unexpected("muss scheitern".to_owned()));
        };
        let message = err.to_string();
        assert!(!message.contains(dir.path().to_string_lossy().as_ref()));
        Ok(())
    }

    /// 3. Scope-Dichtheit: eine Datei außerhalb des konfigurierten Bereichs
    ///    darf niemals in das Ergebnis einfließen, selbst wenn sie im
    ///    unmittelbaren Nachbarverzeichnis liegt und denselben Namen trägt.
    #[test]
    fn test_check_scope_tightness_ignores_files_outside_configured_root() -> TestResult {
        let base = temp_dir()?;
        let inside = base.path().join("scan_reports");
        let outside = base.path().join("other");
        make_dir(&inside, "create inside dir")?;
        make_dir(&outside, "create outside dir")?;
        write_fixture(
            &inside.join("report.sarif"),
            VALID_SARIF,
            "write inside fixture",
        )?;
        write_fixture(
            &outside.join("report.sarif"),
            VALID_SARIF,
            "write outside fixture",
        )?;

        let sensor = sensor_for(&inside);
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("muss gelingen"))?;

        // Nur die zwei Befunde aus der EINEN im Bereich liegenden Datei --
        // nicht vier, was der Fall wäre, würde auch die Nachbardatei
        // gelesen.
        assert_eq!(reading.events.len(), 2);
        Ok(())
    }

    /// 4. Redaktion: Steuerzeichen werden entfernt UND ein übergroßes Feld
    ///    wird gekappt, in derselben Nachricht.
    #[test]
    fn test_check_redaction_combines_truncation_and_control_char_removal() -> TestResult {
        let dir = temp_dir()?;
        let mut huge_with_control = "y".repeat(crate::redact::MAX_DETAIL_LEN * 4);
        huge_with_control.push('\n');
        huge_with_control.push_str("END");
        huge_with_control.push('\u{1b}');
        huge_with_control.push_str("[0m");

        let json = sarif_with_message(&huge_with_control);
        write_fixture(&dir.path().join("report.json"), json, "write fixture")?;

        let sensor = sensor_for(dir.path());
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("muss gelingen"))?;

        let harw_dod_signals::EventKind::StructureDrift { detail, .. } = &reading.events[0].kind
        else {
            return Err(TestError::Unexpected("erwartete StructureDrift".to_owned()));
        };
        assert!(!detail.contains('\n'));
        assert!(!detail.contains('\u{1b}'));
        assert!(detail.len() < huge_with_control.len());
        Ok(())
    }

    /// 5. Kardinalität: N Befunde über mehrere Dateien hinweg ergeben genau
    ///    N Ereignisse -- keine Verdopplung, kein Verlust.
    #[test]
    fn test_check_cardinality_events_sum_across_multiple_files() -> TestResult {
        let dir = temp_dir()?;
        write_fixture(&dir.path().join("a.sarif"), VALID_SARIF, "write fixture a")?; // 2 Treffer
        let audit_json = r#"{"vulnerabilities": {"list": [
            {"advisory": {"id": "RUSTSEC-2099-0001"}, "package": {"name": "p", "version": "1"}}
        ]}}"#; // 1 Treffer
        write_fixture(&dir.path().join("b.json"), audit_json, "write fixture b")?;

        let sensor = sensor_for(dir.path());
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("muss gelingen"))?;

        assert_eq!(reading.events.len(), 3);
        Ok(())
    }

    /// 6. Fehlerfall: eine erkennbare, aber nicht unterstützte Form
    ///    scheitert mit `MalformedSource`, nicht mit Panik oder stillem
    ///    Verwerfen.
    #[test]
    fn test_check_error_case_unrecognized_shape_fails_with_malformed_source() -> TestResult {
        let dir = temp_dir()?;
        write_fixture(
            &dir.path().join("unknown.json"),
            r#"{"not_a_known_format": 1}"#,
            "write fixture",
        )?;

        let sensor = sensor_for(dir.path());
        let Err(err) = sensor.poll(Timestamp::UNIX_EPOCH) else {
            return Err(TestError::Unexpected(
                "unbekannte Form muss scheitern".to_owned(),
            ));
        };
        assert!(matches!(err, SensorError::MalformedSource));
        Ok(())
    }

    // --- F-064: reguläre-Datei-Prüfung vor jedem Lesezugriff ---------------

    #[test]
    fn test_open_if_regular_file_accepts_regular_file() -> TestResult {
        let dir = temp_dir()?;
        let file = dir.path().join("report.json");
        write_fixture(&file, "{}", "write fixture")?;

        assert!(
            open_if_regular_file(&file).is_some(),
            "eine reguläre Datei muss als solche erkannt werden"
        );
        Ok(())
    }

    /// F-064, Kernbeleg: ein Verzeichnis, dessen Name auf `.json` endet
    /// (deshalb ein Glob-Treffer), ist keine reguläre Datei.
    #[test]
    fn test_open_if_regular_file_rejects_directory() -> TestResult {
        let dir = temp_dir()?;
        let fake_report_dir = dir.path().join("looks-like-a-report.json");
        make_dir(&fake_report_dir, "Verzeichnis anlegen")?;

        assert!(
            open_if_regular_file(&fake_report_dir).is_none(),
            "ein Verzeichnis darf nicht als reguläre Datei durchgehen"
        );
        Ok(())
    }

    #[test]
    fn test_open_if_regular_file_rejects_missing_path() -> TestResult {
        let dir = temp_dir()?;
        let missing = dir.path().join("fehlt.json");

        assert!(open_if_regular_file(&missing).is_none());
        Ok(())
    }

    /// F-064, Abrufebene: ein Verzeichnis, das wie ein Bericht benannt ist,
    /// darf weder den Abruf scheitern lassen noch einen Treffer für seinen
    /// eigenen Inhalt erzeugen — die Treffer eines echten Nachbarn müssen
    /// trotzdem ankommen. Steht stellvertretend für die FIFO-Variante aus dem
    /// Befund (siehe `open_if_regular_file`-Unit-Tests oben) — eine echte
    /// FIFO kann ohne neue Testabhängigkeit hier nicht angelegt werden.
    #[test]
    fn test_poll_skips_directory_shaped_like_a_report_but_still_reports_sibling_file() -> TestResult
    {
        let dir = temp_dir()?;
        make_dir(
            &dir.path().join("looks-like-a-report.json"),
            "Verzeichnis anlegen",
        )?;
        write_fixture(
            &dir.path().join("real-report.sarif"),
            VALID_SARIF,
            "write fixture",
        )?;

        let sensor = sensor_for(dir.path());
        let reading = sensor.poll(Timestamp::UNIX_EPOCH).map_err(ctx(
            "ein gleichnamiges Verzeichnis darf den Abruf nicht scheitern lassen",
        ))?;

        assert_eq!(
            reading.events.len(),
            2,
            "die zwei Treffer aus der echten Nachbardatei müssen trotzdem ankommen"
        );
        Ok(())
    }

    // --- F-064: `GlobLimitExceeded` ist erschöpfend behandelt --------------

    #[test]
    fn test_map_read_fs_error_glob_limit_exceeded_maps_to_malformed_source() {
        let err = harw_dod_readfs::ReadFsError::GlobLimitExceeded {
            pattern: "scan_reports/*.json".to_owned(),
            limit_name: "candidates",
            limit: 4096,
        };
        assert!(matches!(
            map_read_fs_error(err),
            SensorError::MalformedSource
        ));
    }
}
