//! Hostmesswerte: der langweilige, häufige Beobachtungsstrom (Contract-Master §G).
//!
//! # Verantwortungsbereich
//! Trägt ausschließlich [`HostSample`] — Zahlen über den Hostzustand, keine
//! Interpretation, kein Befund.
//!
//! # Warum getrennt von `SecurityEvent`
//! Messwerte sind langweilig und häufig, Ereignisse (siehe [`crate::event`])
//! sind selten und wichtig. Sie in einem gemeinsamen Typ zu führen hieße,
//! jeden Verbraucher bei jedem Zugriff zu zwingen, das eine vom anderen zu
//! unterscheiden — statt einmal am Strom-Eingang. Ein Konsument, der nur
//! Trends beobachten will (etwa eine Zeitreihen-Anzeige), kann
//! [`crate::event::SecurityEvent`] vollständig ignorieren; ein Konsument, der
//! nur auf Ereignisse reagiert (etwa das Regelwerk), kann `HostSample`
//! vollständig ignorieren.
//!
//! # Nebenläufigkeit
//! Reiner Datentyp: `Send + Sync` automatisch (alle Felder sind es), kein
//! internes Locking, keine geteilten Ressourcen.
//!
//! # Fehler
//! Keine eigenen — dieses Modul definiert nur Daten und liest keine Quelle.
//! `#[serde(deny_unknown_fields)]` lässt `serde_json` beim Deserialisieren
//! mit einem Serde-Fehler scheitern, wenn ein unbekanntes Feld auftaucht.
//!
//! # Examples
//! ```rust
//! use harw_dod_signals::HostSample;
//! use harw_types::SensorId;
//!
//! let sample = HostSample {
//!     sensor: SensorId::from_str("thermal-0"),
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     metric: std::borrow::Cow::Borrowed("temperature_celsius"),
//!     value: 42.5,
//! };
//! assert_eq!(sample.metric, "temperature_celsius");
//! ```

use harw_types::SensorId;
use jiff::Timestamp;

/// Ein Messwert über den Hostzustand. Zahlen, keine Inhalte.
///
/// # Description
/// `metric` benennt die Messgröße und stammt beim Erzeuger immer aus einer
/// Sensor-Crate selbst deklariert (z. B. `"temperature_celsius"`), keine zur
/// Laufzeit gebaute Zeichenkette — ein Messwertname braucht keinen Heap.
///
/// **Warum `Cow<'static, str>` und nicht `&'static str`:** ein `&'static str`
/// in einem serde-Typ erzeugt `impl Deserialize<'static>` statt
/// `impl<'de> Deserialize<'de>`. Der Typ wäre damit nur aus einer wirklich
/// statischen Quelle lesbar — nie aus einem zur Laufzeit gelesenen Puffer.
/// Da [`crate::SecurityEvidence`] auf Platte geschrieben und zurückgelesen
/// wird, wäre das ein Typ, den man schreiben, aber nicht öffnen kann.
///
/// **Warum nicht schlicht `String`:** ein Erzeuger nennt seine Messgröße immer
/// über eine Konstante aus `metrics!`. `Cow::Borrowed(KONSTANTE)` kostet dort
/// nichts, während `String` bei jedem Messwert allozierte — und ein Sensor
/// liefert viele. Die `Owned`-Variante entsteht ausschließlich beim
/// Zurücklesen von Platte.
///
/// # Errors
/// Keine eigenen Fehler; siehe Moduldoku.
///
/// # Examples
/// ```rust
/// use harw_dod_signals::HostSample;
/// use harw_types::SensorId;
///
/// let json = serde_json::to_string(&HostSample {
///     sensor: SensorId::from_str("proc-stat"),
///     observed_at: jiff::Timestamp::UNIX_EPOCH,
///     metric: std::borrow::Cow::Borrowed("cpu_util_percent"),
///     value: 12.5,
/// })
/// .expect("HostSample serializes");
/// assert!(json.contains("cpu_util_percent"));
/// ```
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSample {
    /// Welcher Sensor den Wert erzeugt hat.
    pub sensor: SensorId,
    /// Wann der Wert erfasst wurde — injizierte Zeit, siehe
    /// [`crate::sensor::Sensor::poll`]. Nie die Systemuhr des Sensors selbst.
    pub observed_at: Timestamp,
    /// Interniertes Metrik-Kürzel, z. B. `"temperature_celsius"`.
    pub metric: std::borrow::Cow<'static, str>,
    /// Der Messwert selbst.
    pub value: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> HostSample {
        HostSample {
            sensor: SensorId::from_str("thermal-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            metric: std::borrow::Cow::Borrowed("temperature_celsius"),
            value: 42.5,
        }
    }

    #[test]
    fn test_host_sample_serializes_metric_and_sensor_as_plain_strings() {
        let json = serde_json::to_string(&sample()).expect("HostSample serializes");
        assert!(json.contains(r#""sensor":"thermal-0""#));
        assert!(json.contains(r#""metric":"temperature_celsius""#));
    }

    #[test]
    fn test_host_sample_deserialize_accepts_well_formed_static_fixture() {
        // Literal, damit die Eingabe eine `'static`-Lebensdauer trägt (siehe
        // Typ-Doku oben) — kein Umweg über einen `String`-Puffer.
        let fixture: &'static str = r#"{
            "sensor": "thermal-0",
            "observed_at": "1970-01-01T00:00:00Z",
            "metric": "temperature_celsius",
            "value": 42.5
        }"#;
        let parsed: HostSample = serde_json::from_str(fixture).expect("fixture deserializes");
        assert_eq!(parsed, sample());
    }

    #[test]
    fn test_host_sample_deserialize_rejects_unknown_field() {
        let fixture: &'static str = r#"{
            "sensor": "thermal-0",
            "observed_at": "1970-01-01T00:00:00Z",
            "metric": "temperature_celsius",
            "value": 42.5,
            "unexpected": true
        }"#;
        assert!(serde_json::from_str::<HostSample>(fixture).is_err());
    }
}
