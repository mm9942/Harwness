//! Internes Ein-/Ausgabeformat für `expect.json`.
//!
//! # Verantwortungsbereich
//! `expect.json` ist das einzige Dateiformat dieser Crate. Es steht in jedem
//! normalen Fixture-Fall (`fixtures/<fall>/expect.json`, siehe
//! [`crate`]-Moduldoku für die Verzeichniskonvention) und wird sowohl von
//! [`crate::capture::capture`] geschrieben als auch von
//! [`crate::harness`] gelesen. Dieses Modul besitzt die eine Stelle, die
//! beide Richtungen kennt — ein zweiter Erzeuger soll das Format nicht
//! abweichend neu erfinden.
//!
//! `harw_dod_signals::SensorReading` selbst trägt kein `serde`-Derive (es ist
//! ein reiner Laufzeit-Ergebnistyp, kein Dateiformat); [`ReadingDto`]
//! spiegelt seine beiden öffentlichen Felder eins zu eins für die
//! JSON-Kodierung.
//!
//! # Format
//! ```json
//! {
//!   "now": "1970-01-01T00:00:00Z",
//!   "reading": {
//!     "samples": [
//!       {
//!         "sensor": "harw-dod-fixtures-suite",
//!         "observed_at": "1970-01-01T00:00:00Z",
//!         "metric": "temperature_celsius",
//!         "value": 42.5
//!       }
//!     ],
//!     "events": []
//!   }
//! }
//! ```
//! `now` ist die injizierte Zeit, mit der [`crate::harness`] den Sensor
//! gegen `tree/` pollt (siehe [`crate::FIXTURE_SENSOR_ID`] für den Wert, den
//! `sensor` in einem von Hand geschriebenen `expect.json` tragen muss).
//! `samples`/`events` sind `Vec<harw_dod_signals::HostSample>` bzw.
//! `Vec<harw_dod_signals::SecurityEvent>`, beide mit `#[serde(deny_unknown_fields)]`
//! — ein zusätzliches Feld lässt das Parsen bewusst scheitern statt es
//! stillschweigend zu ignorieren.
//!
//! # Nebenläufigkeit
//! Zustandslose freie Funktionen über `&Path`; `Send + Sync`, sicher aus
//! jedem Thread aufrufbar. Jeder Aufruf öffnet und schließt seine eigene
//! Datei.
//!
//! # Fehler
//! [`crate::error::FixturesError`] — I/O beim Lesen/Schreiben der Datei,
//! JSON-Kodierung beim (De-)Serialisieren.

use std::path::Path;

use harw_dod_signals::{HostSample, SecurityEvent, SensorReading};
use jiff::Timestamp;

use crate::error::FixturesResult;

/// Der Dateiname des erwarteten `SensorReading` in jedem normalen
/// Fixture-Fall.
pub(crate) const EXPECT_FILE_NAME: &str = "expect.json";

/// Serialisierbares Spiegelbild von `harw_dod_signals::SensorReading`.
///
/// Nicht `pub`: reines Kodierungsdetail dieses Moduls. `SensorReading`
/// selbst trägt kein `serde`-Derive (siehe Moduldoku), aber beide Felder
/// sind `pub`, sodass die Konvertierung in beide Richtungen verlustfrei ist.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadingDto {
    pub(crate) samples: Vec<HostSample>,
    pub(crate) events: Vec<SecurityEvent>,
}

impl From<&SensorReading> for ReadingDto {
    fn from(reading: &SensorReading) -> Self {
        Self {
            samples: reading.samples.clone(),
            events: reading.events.clone(),
        }
    }
}

impl ReadingDto {
    /// Baut das laufzeitseitige `SensorReading` aus dem gelesenen Spiegelbild.
    pub(crate) fn into_reading(self) -> SensorReading {
        SensorReading {
            samples: self.samples,
            events: self.events,
        }
    }
}

/// Der vollständige Inhalt einer `expect.json`-Datei.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpectedFixture {
    pub(crate) now: Timestamp,
    pub(crate) reading: ReadingDto,
}

/// Liest `expect.json` aus einem Fixture-Fallverzeichnis.
///
/// # Arguments
/// - `case_dir` (`&Path`): das Fallverzeichnis, z. B.
///   `fixtures/typical-laptop`. Erwartet `case_dir/expect.json`.
///
/// # Returns
/// Die injizierte Zeit und das erwartete `SensorReading` dieses Falls.
///
/// # Errors
/// [`crate::error::FixturesError::Io`], wenn die Datei fehlt oder nicht
/// lesbar ist; [`crate::error::FixturesError::Json`], wenn der Inhalt kein
/// gültiges `expect.json` gemäß Moduldoku ist.
pub(crate) fn read_expected(case_dir: &Path) -> FixturesResult<ExpectedFixture> {
    let raw = std::fs::read_to_string(case_dir.join(EXPECT_FILE_NAME))?;
    let parsed: ExpectedFixture = serde_json::from_str(&raw)?;
    Ok(parsed)
}

/// Schreibt `expect.json` in ein Fixture-Fallverzeichnis.
///
/// # Arguments
/// - `case_dir` (`&Path`): das Zielverzeichnis; muss bereits existieren.
/// - `now` (`jiff::Timestamp`): die injizierte Zeit, mit der `reading`
///   erzeugt wurde.
/// - `reading` (`&harw_dod_signals::SensorReading`): das einzufrierende
///   Ergebnis.
///
/// # Returns
/// `()` bei Erfolg.
///
/// # Errors
/// [`crate::error::FixturesError::Json`], wenn `reading` nicht kodierbar
/// ist; [`crate::error::FixturesError::Io`], wenn das Schreiben scheitert.
pub(crate) fn write_expected(
    case_dir: &Path,
    now: Timestamp,
    reading: &SensorReading,
) -> FixturesResult<()> {
    let expected = ExpectedFixture {
        now,
        reading: ReadingDto::from(reading),
    };
    let json = serde_json::to_string_pretty(&expected)?;
    std::fs::write(case_dir.join(EXPECT_FILE_NAME), json)?;
    Ok(())
}

/// Serialisiert ein `SensorReading` als kompakte JSON-Zeichenkette.
///
/// # Description
/// Wird von [`crate::harness`] benutzt, um das tatsächliche Poll-Ergebnis
/// nach Kanarienmarkierungen bzw. rohen Host-Pfaden zu durchsuchen (siehe
/// dortige Moduldoku, Prüfungen „Inhaltsfreiheit" und „Redaktion").
///
/// # Arguments
/// - `reading` (`&harw_dod_signals::SensorReading`): das zu serialisierende
///   Ergebnis.
///
/// # Returns
/// Die kompakte JSON-Darstellung von `reading`.
///
/// # Errors
/// [`crate::error::FixturesError::Json`], wenn `reading` nicht kodierbar ist
/// — in der Praxis nicht erreichbar für die heute definierten Feldtypen von
/// `HostSample`/`SecurityEvent`.
pub(crate) fn reading_to_json(reading: &SensorReading) -> FixturesResult<String> {
    Ok(serde_json::to_string(&ReadingDto::from(reading))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_types::SensorId;

    fn sample_reading() -> SensorReading {
        SensorReading {
            samples: vec![HostSample {
                sensor: SensorId::from_str("fixture-io-test"),
                observed_at: Timestamp::UNIX_EPOCH,
                metric: std::borrow::Cow::Borrowed("temperature_celsius"),
                value: 42.5,
            }],
            events: vec![],
        }
    }

    #[test]
    fn test_write_then_read_round_trips_reading() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_expected(dir.path(), Timestamp::UNIX_EPOCH, &sample_reading())
            .expect("write_expected muss gelingen");

        let parsed = read_expected(dir.path()).expect("read_expected muss gelingen");
        assert_eq!(parsed.now, Timestamp::UNIX_EPOCH);
        assert_eq!(parsed.reading.into_reading(), sample_reading());
    }

    #[test]
    fn test_read_expected_missing_file_is_io_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = read_expected(dir.path()).expect_err("fehlende Datei muss scheitern");
        assert!(matches!(err, crate::error::FixturesError::Io(_)));
    }

    #[test]
    fn test_read_expected_rejects_unknown_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(EXPECT_FILE_NAME),
            r#"{"now":"1970-01-01T00:00:00Z","reading":{"samples":[],"events":[]},"extra":true}"#,
        )
        .expect("write");

        let err = read_expected(dir.path()).expect_err("unbekanntes Feld muss scheitern");
        assert!(matches!(err, crate::error::FixturesError::Json(_)));
    }

    #[test]
    fn test_reading_to_json_contains_metric_name() {
        let json = reading_to_json(&sample_reading()).expect("serialisiert");
        assert!(json.contains("temperature_celsius"));
    }
}
