//! `ScanReportSensor`: liest Berichte fremder Scanner, führt keinen aus.
//!
//! # Verantwortungsbereich
//! Implementiert `trait` [`harw_dod_signals::Sensor`] für genau eine Quelle:
//! ein Verzeichnis mit Berichtsdateien fremder Scanner (typischerweise
//! [`harw_home::paths::scan_reports_dir`]). Jeder Dateizugriff läuft über
//! `harw_dod_readfs` — [`harw_dod_readfs::glob::glob`] zum Auffinden der
//! Berichtsdateien, [`harw_dod_readfs::read_to_string`] zum Lesen ihres
//! Inhalts. Diese Datei ruft **niemals** `std::fs` direkt auf.
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
    /// [`SensorReading`], **keinen** Fehler.
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
/// `harw_dod_readfs::MAX_READ_BYTES`) wird als
/// [`SensorError::MalformedSource`] gewertet — eine Berichtsdatei dieser
/// Größe hat für diesen Sensor keine erwartbare Form. Die beiden
/// Glob-Musterfehler betreffen ausschließlich von dieser Crate selbst
/// gebaute Muster ([`glob_patterns_for_root`]), nie einen vom Bericht
/// gelieferten Wert; ihr Auftreten wäre ein interner Konstruktionsfehler
/// dieser Crate, kein Aussagefehler über die Quelle — sie werden konservativ
/// als [`SensorError::SourceUnavailable`] gemeldet.
fn map_read_fs_error(err: harw_dod_readfs::ReadFsError) -> SensorError {
    match err {
        harw_dod_readfs::ReadFsError::Scope(inner) => inner,
        harw_dod_readfs::ReadFsError::TooLarge { .. } => SensorError::MalformedSource,
        harw_dod_readfs::ReadFsError::GlobPatternAbsolute { .. }
        | harw_dod_readfs::ReadFsError::GlobPatternTraversal { .. } => {
            SensorError::SourceUnavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    // --- Explizit geforderte Einzelfälle ------------------------------------

    #[test]
    fn test_valid_report_yields_expected_security_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("report.sarif"), VALID_SARIF).expect("write fixture");

        let sensor = sensor_for(dir.path());
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("gültiger Bericht muss erfolgreich gelesen werden");

        assert_eq!(reading.samples.len(), 0);
        assert_eq!(reading.events.len(), 2);
        for event in &reading.events {
            assert_eq!(event.sensor, sensor_id());
            assert_eq!(event.observed_at, Timestamp::UNIX_EPOCH);
            assert!(event.actor.is_none());
        }
    }

    #[test]
    fn test_empty_directory_yields_empty_reading_without_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sensor = sensor_for(dir.path());

        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("ein leeres Verzeichnis darf keinen Fehler auslösen");

        assert!(reading.samples.is_empty());
        assert!(reading.events.is_empty());
    }

    #[test]
    fn test_missing_directory_yields_empty_reading_without_error() {
        // `scan_reports_dir` wird nur benannt, nie angelegt (siehe
        // `harw-home`-Moduldoku) -- ein Sensor muss auch dann funktionieren,
        // wenn noch kein Scanner je einen Bericht abgelegt hat.
        let parent = tempfile::tempdir().expect("tempdir");
        let missing = parent.path().join("scan_reports");
        let sensor = sensor_for(&missing);

        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("ein fehlendes Verzeichnis darf keinen Fehler auslösen");
        assert!(reading.events.is_empty());
    }

    #[test]
    fn test_oversized_description_field_is_truncated_not_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
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
        fs::write(dir.path().join("audit.json"), json).expect("write fixture");

        let sensor = sensor_for(dir.path());
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("übergroßes Feld darf den Bericht nicht scheitern lassen");

        assert_eq!(reading.events.len(), 1);
        let harw_dod_signals::EventKind::StructureDrift { detail, .. } = &reading.events[0].kind
        else {
            panic!("erwartete StructureDrift");
        };
        assert!(detail.len() < huge_description.len());
        assert!(!detail.is_empty());
    }

    #[test]
    fn test_control_characters_in_free_text_are_neutralized() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut text_with_control = String::from("zeile eins");
        text_with_control.push('\n');
        text_with_control.push_str("zeile zwei");
        text_with_control.push('\u{1b}');
        text_with_control.push_str("[31mrot");
        text_with_control.push('\u{1b}');
        text_with_control.push_str("[0m");

        let json = sarif_with_message(&text_with_control);
        fs::write(dir.path().join("report.json"), json).expect("write fixture");

        let sensor = sensor_for(dir.path());
        let reading = sensor.poll(Timestamp::UNIX_EPOCH).expect("muss gelingen");

        assert_eq!(reading.events.len(), 1);
        let harw_dod_signals::EventKind::StructureDrift { detail, .. } = &reading.events[0].kind
        else {
            panic!("erwartete StructureDrift");
        };
        assert!(!detail.contains('\n'));
        assert!(!detail.contains('\u{1b}'));
    }

    #[test]
    fn test_malformed_json_error_message_does_not_contain_report_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let secret_marker = "GEHEIMNIS-1234567890";
        fs::write(
            dir.path().join("broken.json"),
            format!("{{ das ist kaputt: {secret_marker}"),
        )
        .expect("write fixture");

        let sensor = sensor_for(dir.path());
        let err = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect_err("kaputtes JSON muss scheitern");

        let message = err.to_string();
        assert!(!message.contains(secret_marker));
        assert!(matches!(err, SensorError::MalformedSource));
    }

    #[test]
    fn test_report_hardness_is_not_observed() {
        assert_ne!(REPORT_HARDNESS, Hardness::Observed);
        assert_eq!(REPORT_HARDNESS, Hardness::Inferred);
    }

    // --- Die sechs Fixture-Prüfungen (von Hand, siehe crate-Moduldoku) ------

    /// 1. Determinismus: derselbe Bericht ergibt zweimal dasselbe Ergebnis.
    #[test]
    fn test_check_determinism_same_input_yields_equal_reading() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("report.sarif"), VALID_SARIF).expect("write fixture");
        let sensor = sensor_for(dir.path());

        let first = sensor.poll(Timestamp::UNIX_EPOCH).expect("erster Abruf");
        let second = sensor.poll(Timestamp::UNIX_EPOCH).expect("zweiter Abruf");
        assert_eq!(first, second);
    }

    /// 2. Inhaltsfreiheit: der Fehlertext bei kaputtem JSON nennt weder den
    ///    Berichtsinhalt noch den aufgelösten Pfad der betroffenen Datei.
    #[test]
    fn test_check_content_freedom_error_never_echoes_source_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("broken.json"), "{ nicht valide").expect("write fixture");
        let sensor = sensor_for(dir.path());

        let err = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect_err("muss scheitern");
        let message = err.to_string();
        assert!(!message.contains(dir.path().to_string_lossy().as_ref()));
    }

    /// 3. Scope-Dichtheit: eine Datei außerhalb des konfigurierten Bereichs
    ///    darf niemals in das Ergebnis einfließen, selbst wenn sie im
    ///    unmittelbaren Nachbarverzeichnis liegt und denselben Namen trägt.
    #[test]
    fn test_check_scope_tightness_ignores_files_outside_configured_root() {
        let base = tempfile::tempdir().expect("tempdir");
        let inside = base.path().join("scan_reports");
        let outside = base.path().join("other");
        fs::create_dir(&inside).expect("create inside dir");
        fs::create_dir(&outside).expect("create outside dir");
        fs::write(inside.join("report.sarif"), VALID_SARIF).expect("write inside fixture");
        fs::write(outside.join("report.sarif"), VALID_SARIF).expect("write outside fixture");

        let sensor = sensor_for(&inside);
        let reading = sensor.poll(Timestamp::UNIX_EPOCH).expect("muss gelingen");

        // Nur die zwei Befunde aus der EINEN im Bereich liegenden Datei --
        // nicht vier, was der Fall wäre, würde auch die Nachbardatei
        // gelesen.
        assert_eq!(reading.events.len(), 2);
    }

    /// 4. Redaktion: Steuerzeichen werden entfernt UND ein übergroßes Feld
    ///    wird gekappt, in derselben Nachricht.
    #[test]
    fn test_check_redaction_combines_truncation_and_control_char_removal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut huge_with_control = "y".repeat(crate::redact::MAX_DETAIL_LEN * 4);
        huge_with_control.push('\n');
        huge_with_control.push_str("END");
        huge_with_control.push('\u{1b}');
        huge_with_control.push_str("[0m");

        let json = sarif_with_message(&huge_with_control);
        fs::write(dir.path().join("report.json"), json).expect("write fixture");

        let sensor = sensor_for(dir.path());
        let reading = sensor.poll(Timestamp::UNIX_EPOCH).expect("muss gelingen");

        let harw_dod_signals::EventKind::StructureDrift { detail, .. } = &reading.events[0].kind
        else {
            panic!("erwartete StructureDrift");
        };
        assert!(!detail.contains('\n'));
        assert!(!detail.contains('\u{1b}'));
        assert!(detail.len() < huge_with_control.len());
    }

    /// 5. Kardinalität: N Befunde über mehrere Dateien hinweg ergeben genau
    ///    N Ereignisse -- keine Verdopplung, kein Verlust.
    #[test]
    fn test_check_cardinality_events_sum_across_multiple_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("a.sarif"), VALID_SARIF).expect("write fixture a"); // 2 Treffer
        let audit_json = r#"{"vulnerabilities": {"list": [
            {"advisory": {"id": "RUSTSEC-2099-0001"}, "package": {"name": "p", "version": "1"}}
        ]}}"#; // 1 Treffer
        fs::write(dir.path().join("b.json"), audit_json).expect("write fixture b");

        let sensor = sensor_for(dir.path());
        let reading = sensor.poll(Timestamp::UNIX_EPOCH).expect("muss gelingen");

        assert_eq!(reading.events.len(), 3);
    }

    /// 6. Fehlerfall: eine erkennbare, aber nicht unterstützte Form
    ///    scheitert mit `MalformedSource`, nicht mit Panik oder stillem
    ///    Verwerfen.
    #[test]
    fn test_check_error_case_unrecognized_shape_fails_with_malformed_source() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("unknown.json"), r#"{"not_a_known_format": 1}"#)
            .expect("write fixture");

        let sensor = sensor_for(dir.path());
        let err = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect_err("unbekannte Form muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
    }
}
