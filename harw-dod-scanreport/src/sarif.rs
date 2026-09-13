//! SARIF-Teilmenge: nur die Felder, die diese Crate tatsächlich braucht.
//!
//! # Verantwortungsbereich
//! SARIF (Static Analysis Results Interchange Format, OASIS-Standard) ist
//! das De-facto-Austauschformat für Analysewerkzeuge (u. a. `cargo clippy
//! --message-format=sarif` über `clippy-sarif`, CodeQL, semgrep). Ein
//! vollständiges SARIF-Dokument hat weit mehr Felder als hier modelliert
//! (Regelkataloge, eingebettete Artefakte, Fingerprints, Suppressions); diese
//! Crate bildet bewusst nur den Ausschnitt ab, den sie in ein
//! [`harw_dod_signals::SecurityEvent`] überführt — jedes unbekannte Feld
//! wird von `serde_json` beim Deserialisieren stillschweigend ignoriert
//! (kein `deny_unknown_fields`; anders als bei `harw-dod-signals`s eigenen
//! Wire-Typen ist dies hier ein **fremdes**, nicht selbst kontrolliertes
//! Format).
//!
//! # Warum dieses Format
//! SARIF ist das verbreitetste maschinenlesbare Berichtsformat für
//! Analysewerkzeuge im Rust- und Nicht-Rust-Ökosystem gleichermaßen — ein
//! Sensor, der SARIF liest, profitiert von jedem Werkzeug, das SARIF
//! ausgibt, ohne für jedes einen eigenen Parser zu brauchen.
//!
//! # Schwere strukturiert, nicht aus Prosa
//! [`harw_dod_signals::EventKind::StructureDrift::severity`] verlangt
//! ausdrücklich eine **strukturierte** Einstufung statt einer aus Freitext
//! zurückgewonnenen. [`map_level`] ist die eine Stelle in dieser Crate, die
//! SARIFs `level`-Feld (`error`/`warning`/`note`/`none`) auf
//! [`harw_dod_signals::DriftSeverity`] abbildet — eine ausdrückliche
//! Entscheidung dieses Moduls, kein aus dem `detail`-Text zurückgelesener
//! Wert.
//!
//! # Nebenläufigkeit
//! Reine Datentypen (`Deserialize`) und zustandslose Funktionen
//! ([`to_events`], [`map_level`]); `Send + Sync`, von jedem Thread parallel
//! aufrufbar.
//!
//! # Fehler
//! Keine eigenen — Deserialisierungsfehler entstehen beim Aufrufer
//! ([`crate::report::parse_report`]) über `serde_json::from_value` und
//! werden dort auf [`harw_dod_cap::SensorError::MalformedSource`]
//! abgebildet, ohne den nicht parsbaren Inhalt zu nennen.

use harw_dod_signals::{DriftSeverity, EventKind, SecurityEvent};
use harw_types::SensorId;
use jiff::Timestamp;
use serde::Deserialize;

use crate::redact::sanitize_and_truncate;

/// Wurzel eines SARIF-Dokuments: `{ "runs": [...] }`.
#[derive(Debug, Deserialize)]
pub(crate) struct SarifDocument {
    #[serde(default)]
    pub(crate) runs: Vec<SarifRun>,
}

/// Ein Analyselauf innerhalb eines SARIF-Dokuments.
#[derive(Debug, Deserialize)]
pub(crate) struct SarifRun {
    #[serde(default)]
    pub(crate) results: Vec<SarifResult>,
}

/// Ein einzelner Befund innerhalb eines Laufs.
#[derive(Debug, Deserialize)]
pub(crate) struct SarifResult {
    #[serde(default, rename = "ruleId")]
    pub(crate) rule_id: Option<String>,
    #[serde(default)]
    pub(crate) level: Option<String>,
    #[serde(default)]
    pub(crate) message: Option<SarifMessage>,
    #[serde(default)]
    pub(crate) locations: Vec<SarifLocation>,
}

/// Die Freitext-Nachricht eines Befunds.
#[derive(Debug, Deserialize)]
pub(crate) struct SarifMessage {
    #[serde(default)]
    pub(crate) text: Option<String>,
}

/// Eine Fundstelle: verweist über `physicalLocation` auf ein Artefakt.
#[derive(Debug, Deserialize)]
pub(crate) struct SarifLocation {
    #[serde(default, rename = "physicalLocation")]
    pub(crate) physical_location: Option<SarifPhysicalLocation>,
}

/// Die physische Fundstelle: ein Artefakt (typischerweise eine Datei).
#[derive(Debug, Deserialize)]
pub(crate) struct SarifPhysicalLocation {
    #[serde(default, rename = "artifactLocation")]
    pub(crate) artifact_location: Option<SarifArtifactLocation>,
}

/// Der URI des Artefakts einer Fundstelle.
#[derive(Debug, Deserialize)]
pub(crate) struct SarifArtifactLocation {
    #[serde(default)]
    pub(crate) uri: Option<String>,
}

/// Wandelt ein bereits geparstes SARIF-Dokument in `SecurityEvent`s.
///
/// # Description
/// Jeder `result` über alle `runs` hinweg wird zu genau einem
/// [`SecurityEvent`] mit [`EventKind::StructureDrift`] — ein Scanner-Befund
/// ist eine Behauptung eines fremden Werkzeugs über den Zustand des
/// gescannten Codes, keine vom Harness selbst beobachtete Struktur, passt
/// aber in kein anderes Feld dieser geschlossenen Aufzählung (siehe
/// `crate`-Moduldoku, Abschnitt „Zuordnung auf `EventKind`" — dort auch der
/// Vermerk, warum diese Zuordnung für einen SARIF-Codebefund schwächer passt
/// als für einen `cargo-audit`-Abhängigkeitstreffer). `severity` kommt aus
/// [`map_level`], strukturiert und unabhängig vom Text. `detail` fasst
/// Regel-ID, Schweregrad, Fundstelle und Nachricht in einer Zeile für
/// Menschen zusammen und läuft **vollständig** durch
/// [`sanitize_and_truncate`] — nicht nur die Nachricht selbst, auch
/// Regel-ID und Fundstellen-URI sind angreiferkontrollierter Text.
///
/// # Arguments
/// - `document` (`&SarifDocument`): das geparste SARIF-Dokument.
/// - `sensor` (`&SensorId`): die Kennung dieses Sensors; wird pro Ereignis
///   geklont.
/// - `now` (`jiff::Timestamp`): injizierte Erfassungszeit, für jedes
///   entstehende Ereignis identisch.
///
/// # Returns
/// Einen `SecurityEvent` je `result`, in der Reihenfolge der `runs` und
/// darin der `results` — deterministisch für ein gegebenes Dokument.
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
pub(crate) fn to_events(
    document: &SarifDocument,
    sensor: &SensorId,
    now: Timestamp,
) -> Vec<SecurityEvent> {
    document
        .runs
        .iter()
        .flat_map(|run| run.results.iter())
        .map(|result| build_event(result, sensor, now))
        .collect()
}

/// Bildet SARIFs `level`-Feld auf [`DriftSeverity`] ab.
///
/// # Description
/// Die eine Stelle, an der diese Crate SARIF-Schwere in
/// [`DriftSeverity`] übersetzt — eine ausdrückliche Entscheidung, keine aus
/// Text zurückgewonnene (siehe Moduldoku, Abschnitt „Schwere strukturiert,
/// nicht aus Prosa"). SARIF kennt vier Stufen: `error`, `warning`, `note`
/// und `none`.
///
/// - `error` → [`DriftSeverity::High`]: das Werkzeug stuft den Befund als
///   Fehler ein.
/// - `warning` → [`DriftSeverity::Medium`].
/// - `note` und `none` → [`DriftSeverity::Low`]: in beiden Fällen hat das
///   Werkzeug **ausdrücklich entschieden**, dass der Befund nicht schwer
///   wiegt (`none` heißt laut SARIF-Spezifikation „zeigt kein Problem an" —
///   eine getroffene, wenn auch niedrige Einstufung), anders als ein
///   **fehlendes** `level`-Feld.
/// - Fehlt `level`, oder trägt es einen unbekannten Wert →
///   [`DriftSeverity::Unknown`]: hier hat niemand entschieden, und
///   `Unknown` darf laut [`DriftSeverity`]-Dokumentation nicht mit `Low`
///   verwechselt werden.
///
/// # Arguments
/// - `level` (`Option<&str>`): der Rohwert des SARIF-`level`-Feldes.
///
/// # Returns
/// Die zugeordnete [`DriftSeverity`].
///
/// # Concurrency
/// Reine, zustandslose Funktion; von jedem Thread parallel aufrufbar.
fn map_level(level: Option<&str>) -> DriftSeverity {
    match level {
        Some("error") => DriftSeverity::High,
        Some("warning") => DriftSeverity::Medium,
        Some("note") | Some("none") => DriftSeverity::Low,
        _ => DriftSeverity::Unknown,
    }
}

/// Baut ein Ereignis aus einem einzelnen SARIF-Befund.
fn build_event(result: &SarifResult, sensor: &SensorId, now: Timestamp) -> SecurityEvent {
    let uri = result
        .locations
        .first()
        .and_then(|location| location.physical_location.as_ref())
        .and_then(|physical| physical.artifact_location.as_ref())
        .and_then(|artifact| artifact.uri.as_deref())
        .unwrap_or("<ohne Fundstelle>");
    let rule = result.rule_id.as_deref().unwrap_or("<ohne Regel>");
    let level = result.level.as_deref();
    let text = result
        .message
        .as_ref()
        .and_then(|message| message.text.as_deref())
        .unwrap_or("");

    let raw = format!(
        "sarif[{}] {rule} @ {uri}: {text}",
        level.unwrap_or("<ohne Stufe>")
    );

    SecurityEvent {
        sensor: sensor.clone(),
        observed_at: now,
        actor: None,
        kind: EventKind::StructureDrift {
            severity: map_level(level),
            detail: sanitize_and_truncate(&raw),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sensor_id() -> SensorId {
        SensorId::from_str("scanreport-0")
    }

    #[test]
    fn test_to_events_maps_one_event_per_result() {
        let json = r#"{
            "runs": [
                {
                    "results": [
                        {"ruleId": "R1", "level": "error", "message": {"text": "a"}},
                        {"ruleId": "R2", "level": "warning", "message": {"text": "b"}}
                    ]
                }
            ]
        }"#;
        let document: SarifDocument = serde_json::from_str(json).expect("valides SARIF-Fixture");
        let events = to_events(&document, &sensor_id(), Timestamp::UNIX_EPOCH);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_to_events_empty_runs_yields_no_events() {
        let json = r#"{"runs": []}"#;
        let document: SarifDocument = serde_json::from_str(json).expect("valides SARIF-Fixture");
        let events = to_events(&document, &sensor_id(), Timestamp::UNIX_EPOCH);
        assert!(events.is_empty());
    }

    #[test]
    fn test_map_level_error_is_high() {
        assert_eq!(map_level(Some("error")), DriftSeverity::High);
    }

    #[test]
    fn test_map_level_warning_is_medium() {
        assert_eq!(map_level(Some("warning")), DriftSeverity::Medium);
    }

    #[test]
    fn test_map_level_note_and_none_are_low() {
        assert_eq!(map_level(Some("note")), DriftSeverity::Low);
        assert_eq!(map_level(Some("none")), DriftSeverity::Low);
    }

    #[test]
    fn test_map_level_missing_or_unknown_is_unknown_not_low() {
        assert_eq!(map_level(None), DriftSeverity::Unknown);
        assert_eq!(map_level(Some("banana")), DriftSeverity::Unknown);
        assert_ne!(map_level(None), DriftSeverity::Low);
    }

    #[test]
    fn test_build_event_falls_back_on_missing_optional_fields() {
        let result = SarifResult {
            rule_id: None,
            level: None,
            message: None,
            locations: Vec::new(),
        };
        let event = build_event(&result, &sensor_id(), Timestamp::UNIX_EPOCH);
        let harw_dod_signals::EventKind::StructureDrift { severity, detail } = event.kind else {
            panic!("erwartete StructureDrift");
        };
        assert_eq!(severity, DriftSeverity::Unknown);
        assert!(detail.contains("<ohne Regel>"));
        assert!(detail.contains("<ohne Fundstelle>"));
    }

    #[test]
    fn test_build_event_includes_rule_level_and_uri() {
        let json = r#"{
            "ruleId": "RUST-001",
            "level": "error",
            "message": {"text": "unsafe pattern"},
            "locations": [
                {"physicalLocation": {"artifactLocation": {"uri": "src/main.rs"}}}
            ]
        }"#;
        let result: SarifResult = serde_json::from_str(json).expect("valides Result-Fixture");
        let event = build_event(&result, &sensor_id(), Timestamp::UNIX_EPOCH);
        let harw_dod_signals::EventKind::StructureDrift { severity, detail } = event.kind else {
            panic!("erwartete StructureDrift");
        };
        assert_eq!(severity, DriftSeverity::High);
        assert!(detail.contains("RUST-001"));
        assert!(detail.contains("error"));
        assert!(detail.contains("src/main.rs"));
        assert!(detail.contains("unsafe pattern"));
    }
}
