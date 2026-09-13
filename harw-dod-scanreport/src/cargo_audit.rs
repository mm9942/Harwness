//! `cargo audit --json`-Teilmenge: nur die Felder, die diese Crate braucht.
//!
//! # Verantwortungsbereich
//! `cargo audit` gleicht `Cargo.lock` gegen die RustSec-Advisory-Datenbank
//! ab und meldet Treffer über `--json` als ein Dokument mit
//! `vulnerabilities.list`. Diese Crate bildet nur den Ausschnitt ab, den sie
//! in ein [`harw_dod_signals::SecurityEvent`] überführt; unbekannte Felder
//! werden von `serde_json` beim Deserialisieren stillschweigend ignoriert
//! (kein `deny_unknown_fields` — ein fremdes, nicht selbst kontrolliertes
//! Format, siehe `sarif`-Moduldoku für dieselbe Begründung).
//!
//! # Warum dieses Format
//! `cargo audit` ist das Standardwerkzeug für Advisory-Abgleich im
//! Rust-Ökosystem dieses Workspace und liefert genau die Kennungen
//! (RustSec-ID, Paketname, Version), die der spätere Advisory-Abgleich in
//! Knoten AW7-06 braucht — ein zweites Format speziell für Rust-Abhängigkeiten
//! wäre redundant.
//!
//! # Schwere strukturiert, nicht aus Prosa
//! Wie in `sarif` ([`crate::sarif::map_level`]) verlangt
//! [`harw_dod_signals::EventKind::StructureDrift::severity`] eine
//! strukturierte Einstufung. [`map_severity`] ist die eine Stelle in diesem
//! Modul, die die informelle CVSS-artige Einstufung einer RustSec-Advisory
//! auf [`harw_dod_signals::DriftSeverity`] abbildet.
//!
//! # Nebenläufigkeit
//! Reine Datentypen (`Deserialize`) und zustandslose Funktionen
//! ([`to_events`], [`map_severity`]); `Send + Sync`, von jedem Thread
//! parallel aufrufbar.
//!
//! # Fehler
//! Keine eigenen — siehe `sarif`-Moduldoku, Abschnitt „Fehler" (identische
//! Zuständigkeitsteilung mit [`crate::report::parse_report`]).

use harw_dod_signals::{DriftSeverity, EventKind, SecurityEvent};
use harw_types::SensorId;
use jiff::Timestamp;
use serde::Deserialize;

use crate::redact::sanitize_and_truncate;

/// Wurzel eines `cargo audit --json`-Dokuments.
#[derive(Debug, Deserialize)]
pub(crate) struct CargoAuditDocument {
    #[serde(default)]
    pub(crate) vulnerabilities: CargoAuditVulnerabilities,
}

/// Der `vulnerabilities`-Block eines `cargo audit`-Dokuments.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct CargoAuditVulnerabilities {
    #[serde(default)]
    pub(crate) list: Vec<CargoAuditVulnerability>,
}

/// Ein einzelner Advisory-Treffer.
#[derive(Debug, Deserialize)]
pub(crate) struct CargoAuditVulnerability {
    #[serde(default)]
    pub(crate) advisory: Option<CargoAuditAdvisory>,
    #[serde(default)]
    pub(crate) package: Option<CargoAuditPackage>,
}

/// Die Advisory-Metadaten eines Treffers.
#[derive(Debug, Deserialize)]
pub(crate) struct CargoAuditAdvisory {
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    /// Informelle Schwereangabe der Advisory (`"critical"`, `"high"`,
    /// `"medium"`, `"low"`, o. ä.), soweit die RustSec-Datenbank für diesen
    /// Eintrag eine CVSS-Einstufung hinterlegt hat — nicht jede Advisory
    /// trägt eine. Wird über [`map_severity`] auf [`DriftSeverity`]
    /// abgebildet.
    #[serde(default)]
    pub(crate) severity: Option<String>,
}

/// Das betroffene Paket eines Treffers.
#[derive(Debug, Deserialize)]
pub(crate) struct CargoAuditPackage {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) version: Option<String>,
}

/// Wandelt ein bereits geparstes `cargo audit`-Dokument in `SecurityEvent`s.
///
/// # Description
/// Jeder Eintrag in `vulnerabilities.list` wird zu genau einem
/// [`SecurityEvent`] mit [`EventKind::StructureDrift`] — dieselbe Zuordnung
/// und dieselbe Begründung wie in [`crate::sarif::to_events`]; für einen
/// Abhängigkeits-CVE passt „eine überwachte Struktur ist abgewichen"
/// deutlich besser als für einen SARIF-Codebefund (siehe `crate`-Moduldoku,
/// Abschnitt „Zuordnung auf `EventKind`"). `severity` kommt aus
/// [`map_severity`], strukturiert und unabhängig vom Text. `detail` fasst
/// Advisory-ID, Paket, Version, Titel und Beschreibung in einer Zeile für
/// Menschen zusammen und läuft vollständig durch [`sanitize_and_truncate`].
///
/// # Arguments
/// - `document` (`&CargoAuditDocument`): das geparste `cargo audit`-Dokument.
/// - `sensor` (`&SensorId`): die Kennung dieses Sensors; wird pro Ereignis
///   geklont.
/// - `now` (`jiff::Timestamp`): injizierte Erfassungszeit, für jedes
///   entstehende Ereignis identisch.
///
/// # Returns
/// Einen `SecurityEvent` je Eintrag in `vulnerabilities.list`, in
/// Dokumentreihenfolge — deterministisch für ein gegebenes Dokument.
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
pub(crate) fn to_events(
    document: &CargoAuditDocument,
    sensor: &SensorId,
    now: Timestamp,
) -> Vec<SecurityEvent> {
    document
        .vulnerabilities
        .list
        .iter()
        .map(|vulnerability| build_event(vulnerability, sensor, now))
        .collect()
}

/// Bildet die informelle Schwereangabe einer RustSec-Advisory auf
/// [`DriftSeverity`] ab.
///
/// # Description
/// Die eine Stelle in diesem Modul, die `cargo audit`-Schwere in
/// [`DriftSeverity`] übersetzt (siehe Moduldoku, Abschnitt „Schwere
/// strukturiert, nicht aus Prosa"). Der Vergleich ist
/// Groß-/Kleinschreibung-unempfindlich, weil weder RustSec noch `cargo
/// audit` eine feste Schreibweise vertraglich zusichern.
///
/// - `"critical"` und `"high"` → [`DriftSeverity::High`].
/// - `"medium"` und `"moderate"` → [`DriftSeverity::Medium`].
/// - `"low"` → [`DriftSeverity::Low`].
/// - Fehlt die Angabe, oder trägt sie einen anderen Wert →
///   [`DriftSeverity::Unknown`]: viele RustSec-Advisories tragen überhaupt
///   keine CVSS-Einstufung, und das ist ein anderer Zustand als „gering
///   eingestuft" — siehe [`DriftSeverity`]-Dokumentation.
///
/// # Arguments
/// - `severity` (`Option<&str>`): der Rohwert des Advisory-`severity`-Feldes.
///
/// # Returns
/// Die zugeordnete [`DriftSeverity`].
///
/// # Concurrency
/// Reine, zustandslose Funktion; von jedem Thread parallel aufrufbar.
fn map_severity(severity: Option<&str>) -> DriftSeverity {
    let Some(raw) = severity else {
        return DriftSeverity::Unknown;
    };
    match raw.to_ascii_lowercase().as_str() {
        "critical" | "high" => DriftSeverity::High,
        "medium" | "moderate" => DriftSeverity::Medium,
        "low" => DriftSeverity::Low,
        _ => DriftSeverity::Unknown,
    }
}

/// Baut ein Ereignis aus einem einzelnen Advisory-Treffer.
fn build_event(
    vulnerability: &CargoAuditVulnerability,
    sensor: &SensorId,
    now: Timestamp,
) -> SecurityEvent {
    let id = vulnerability
        .advisory
        .as_ref()
        .and_then(|advisory| advisory.id.as_deref())
        .unwrap_or("<ohne ID>");
    let package = vulnerability
        .package
        .as_ref()
        .and_then(|package| package.name.as_deref())
        .unwrap_or("<unbekanntes Paket>");
    let version = vulnerability
        .package
        .as_ref()
        .and_then(|package| package.version.as_deref())
        .unwrap_or("<unbekannte Version>");
    let title = vulnerability
        .advisory
        .as_ref()
        .and_then(|advisory| advisory.title.as_deref())
        .unwrap_or("");
    let description = vulnerability
        .advisory
        .as_ref()
        .and_then(|advisory| advisory.description.as_deref())
        .unwrap_or("");
    let severity_raw = vulnerability
        .advisory
        .as_ref()
        .and_then(|advisory| advisory.severity.as_deref());

    let raw = format!(
        "cargo-audit {id} {package}@{version}: {title} -- {description}",
    );

    SecurityEvent {
        sensor: sensor.clone(),
        observed_at: now,
        actor: None,
        kind: EventKind::StructureDrift {
            severity: map_severity(severity_raw),
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
    fn test_to_events_maps_one_event_per_vulnerability() {
        let json = r#"{
            "vulnerabilities": {
                "found": true,
                "count": 2,
                "list": [
                    {
                        "advisory": {"id": "RUSTSEC-2021-0001", "title": "t1", "description": "d1"},
                        "package": {"name": "pkg-a", "version": "1.0.0"}
                    },
                    {
                        "advisory": {"id": "RUSTSEC-2021-0002", "title": "t2", "description": "d2"},
                        "package": {"name": "pkg-b", "version": "2.0.0"}
                    }
                ]
            }
        }"#;
        let document: CargoAuditDocument =
            serde_json::from_str(json).expect("valides cargo-audit-Fixture");
        let events = to_events(&document, &sensor_id(), Timestamp::UNIX_EPOCH);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_to_events_no_vulnerabilities_yields_no_events() {
        let json = r#"{"vulnerabilities": {"found": false, "count": 0, "list": []}}"#;
        let document: CargoAuditDocument =
            serde_json::from_str(json).expect("valides cargo-audit-Fixture");
        let events = to_events(&document, &sensor_id(), Timestamp::UNIX_EPOCH);
        assert!(events.is_empty());
    }

    #[test]
    fn test_to_events_missing_vulnerabilities_block_defaults_to_empty() {
        let json = r#"{}"#;
        let document: CargoAuditDocument =
            serde_json::from_str(json).expect("leeres Dokument ist noch valide");
        let events = to_events(&document, &sensor_id(), Timestamp::UNIX_EPOCH);
        assert!(events.is_empty());
    }

    #[test]
    fn test_map_severity_critical_and_high_are_high() {
        assert_eq!(map_severity(Some("critical")), DriftSeverity::High);
        assert_eq!(map_severity(Some("High")), DriftSeverity::High);
    }

    #[test]
    fn test_map_severity_medium_and_moderate_are_medium() {
        assert_eq!(map_severity(Some("medium")), DriftSeverity::Medium);
        assert_eq!(map_severity(Some("Moderate")), DriftSeverity::Medium);
    }

    #[test]
    fn test_map_severity_low_is_low() {
        assert_eq!(map_severity(Some("low")), DriftSeverity::Low);
    }

    #[test]
    fn test_map_severity_missing_or_unrecognized_is_unknown_not_low() {
        assert_eq!(map_severity(None), DriftSeverity::Unknown);
        assert_eq!(map_severity(Some("mystery")), DriftSeverity::Unknown);
        assert_ne!(map_severity(None), DriftSeverity::Low);
    }

    #[test]
    fn test_build_event_includes_advisory_id_package_and_version() {
        let json = r#"{
            "advisory": {"id": "RUSTSEC-2021-0001", "title": "Beispiel", "description": "Details", "severity": "high"},
            "package": {"name": "example-crate", "version": "0.1.0"}
        }"#;
        let vulnerability: CargoAuditVulnerability =
            serde_json::from_str(json).expect("valides Treffer-Fixture");
        let event = build_event(&vulnerability, &sensor_id(), Timestamp::UNIX_EPOCH);
        let harw_dod_signals::EventKind::StructureDrift { severity, detail } = event.kind else {
            panic!("erwartete StructureDrift");
        };
        assert_eq!(severity, DriftSeverity::High);
        assert!(detail.contains("RUSTSEC-2021-0001"));
        assert!(detail.contains("example-crate"));
        assert!(detail.contains("0.1.0"));
    }

    #[test]
    fn test_build_event_falls_back_on_missing_advisory_and_package() {
        let vulnerability = CargoAuditVulnerability {
            advisory: None,
            package: None,
        };
        let event = build_event(&vulnerability, &sensor_id(), Timestamp::UNIX_EPOCH);
        let harw_dod_signals::EventKind::StructureDrift { severity, detail } = event.kind else {
            panic!("erwartete StructureDrift");
        };
        assert_eq!(severity, DriftSeverity::Unknown);
        assert!(detail.contains("<ohne ID>"));
        assert!(detail.contains("<unbekanntes Paket>"));
    }
}
