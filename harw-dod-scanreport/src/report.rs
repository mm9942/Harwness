//! Formaterkennung: ein Berichtsinhalt wird einem Parser zugeordnet.
//!
//! # Verantwortungsbereich
//! Besitzt genau eine Funktion, [`parse_report`], die den einzigen
//! Verzweigungspunkt zwischen den beiden unterstützten Formaten bildet
//! ([`crate::sarif`], [`crate::cargo_audit`]). Kein Format wird anhand des
//! Dateinamens erkannt — ausschließlich anhand der Struktur des geparsten
//! JSON-Dokuments (`runs` für SARIF, `vulnerabilities` für `cargo audit`),
//! damit eine falsch benannte, aber inhaltlich gültige Datei trotzdem
//! gelesen wird.
//!
//! # Fehler
//! [`harw_dod_cap::SensorError::MalformedSource`] in drei Fällen, allesamt
//! ohne den Berichtsinhalt in der Fehlermeldung zu nennen (siehe
//! `crate`-Moduldoku, Abschnitt „Angreiferkontrollierter Inhalt"):
//! - `content` ist kein syntaktisch gültiges JSON.
//! - `content` ist gültiges JSON, aber weder `runs` noch `vulnerabilities`
//!   sind als Feld auf oberster Ebene vorhanden — ein von dieser Crate nicht
//!   unterstütztes oder nicht erkennbares Format.
//! - `content` hat das erkannte Top-Level-Feld, aber die genauere,
//!   typisierte Struktur darunter passt nicht zum jeweiligen Schema.
//!
//! # Nebenläufigkeit
//! Zustandslos; `Send + Sync`, von jedem Thread parallel aufrufbar.

use harw_dod_cap::SensorError;
use harw_dod_signals::SecurityEvent;
use harw_types::SensorId;
use jiff::Timestamp;

use crate::cargo_audit::{self, CargoAuditDocument};
use crate::sarif::{self, SarifDocument};

/// Erkennt das Format von `content` und wandelt es in `SecurityEvent`s.
///
/// # Description
/// Parst `content` zunächst als generisches JSON, prüft dann auf das
/// Vorhandensein des Feldes `runs` (SARIF) bzw. `vulnerabilities`
/// (`cargo audit --json`) auf oberster Ebene und deserialisiert erst danach
/// in die passende, genauer typisierte Struktur. Ein Dokument, das beide
/// oder keines der beiden Felder trägt, wird als nicht unterstütztes Format
/// abgelehnt.
///
/// # Arguments
/// - `content` (`&str`): der vollständige, bereits gelesene Dateiinhalt
///   eines Berichts. Angreiferkontrolliert (siehe `crate`-Moduldoku).
/// - `sensor` (`&SensorId`): die Kennung des aufrufenden Sensors.
/// - `now` (`jiff::Timestamp`): injizierte Erfassungszeit für jedes
///   entstehende Ereignis.
///
/// # Returns
/// Die aus `content` abgeleiteten `SecurityEvent`s, in Dokumentreihenfolge.
///
/// # Errors
/// [`SensorError::MalformedSource`] — siehe Moduldoku für die drei
/// auslösenden Fälle. Nennt in keinem Fall `content` selbst.
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
pub(crate) fn parse_report(
    content: &str,
    sensor: &SensorId,
    now: Timestamp,
) -> Result<Vec<SecurityEvent>, SensorError> {
    let value: serde_json::Value =
        serde_json::from_str(content).map_err(|_| SensorError::MalformedSource)?;

    if value.get("runs").is_some() {
        let document: SarifDocument =
            serde_json::from_value(value).map_err(|_| SensorError::MalformedSource)?;
        Ok(sarif::to_events(&document, sensor, now))
    } else if value.get("vulnerabilities").is_some() {
        let document: CargoAuditDocument =
            serde_json::from_value(value).map_err(|_| SensorError::MalformedSource)?;
        Ok(cargo_audit::to_events(&document, sensor, now))
    } else {
        Err(SensorError::MalformedSource)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sensor_id() -> SensorId {
        SensorId::from_str("scanreport-0")
    }

    #[test]
    fn test_parse_report_detects_sarif_by_runs_field() {
        let json = r#"{"runs": [{"results": [{"ruleId": "R1", "message": {"text": "x"}}]}]}"#;
        let events =
            parse_report(json, &sensor_id(), Timestamp::UNIX_EPOCH).expect("SARIF muss parsen");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_parse_report_detects_cargo_audit_by_vulnerabilities_field() {
        let json = r#"{"vulnerabilities": {"list": [{"advisory": {"id": "RUSTSEC-2021-0001"}, "package": {"name": "p", "version": "1"}}]}}"#;
        let events = parse_report(json, &sensor_id(), Timestamp::UNIX_EPOCH)
            .expect("cargo-audit-Dokument muss parsen");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_parse_report_rejects_syntactically_invalid_json_without_leaking_content() {
        let broken = "{ das ist kein json, sondern ein GEHEIMNIS-Marker";
        let err = parse_report(broken, &sensor_id(), Timestamp::UNIX_EPOCH)
            .expect_err("kaputtes JSON muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
        let message = err.to_string();
        assert!(!message.contains("GEHEIMNIS"));
        assert!(!message.contains(broken));
    }

    #[test]
    fn test_parse_report_rejects_unrecognized_json_shape() {
        let json = r#"{"unrelated_field": true}"#;
        let err = parse_report(json, &sensor_id(), Timestamp::UNIX_EPOCH)
            .expect_err("unbekannte Form muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
    }

    #[test]
    fn test_parse_report_rejects_runs_field_with_wrong_inner_shape() {
        // `runs` ist vorhanden, aber kein Array von Läufen -- die
        // Top-Level-Erkennung greift, die genauere Typprüfung scheitert.
        let json = r#"{"runs": "nicht-ein-array"}"#;
        let err = parse_report(json, &sensor_id(), Timestamp::UNIX_EPOCH)
            .expect_err("falsch geformtes SARIF muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
    }

    #[test]
    fn test_parse_report_prefers_sarif_when_both_fields_present() {
        // Randfall: beide Erkennungsfelder vorhanden -- SARIF hat Vorrang,
        // weil die Prüfung darauf zuerst greift. Dokumentiertes, getestetes
        // Verhalten statt stillschweigender Zufälligkeit.
        let json = r#"{"runs": [], "vulnerabilities": {"list": []}}"#;
        let events = parse_report(json, &sensor_id(), Timestamp::UNIX_EPOCH)
            .expect("Dokument mit beiden Feldern muss als SARIF parsen");
        assert!(events.is_empty());
    }
}
