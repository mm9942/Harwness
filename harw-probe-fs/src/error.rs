//! Fehlertyp der fanotify-Sonde `harw-probe-fs`.
//!
//! # Verantwortungsbereich
//! [`ProbeError`] ist der eine Fehlertyp dieses Binaries. Er deckt vier
//! Quellen ab: die Landlock-Durchsetzung ([`crate::landlock`]), die
//! (weiterhin unfertige) fanotify-Quelle ([`crate::source`]), den Sendeweg
//! zum Sentinel ([`crate::sink`]) und Sensorfehler aus der Sammelschleife
//! ([`crate::collect`], über `harw_dod_cap::SensorError`). Kommandozeilen-
//! fehler gehören **nicht** hierher — `clap::Error` behandelt `main`
//! direkt, ohne Umweg über diesen Typ (Muster: `harw-sentinel/src/main.rs`).
//!
//! # Warum kein `harw-macros`
//! Die für diesen Knoten ursprünglich festgelegte Abhängigkeitsliste enthält
//! `harw-macros` nicht, und dieser Knoten führt keine zusätzliche
//! Abhängigkeit ein, die nicht durch die während dieses Knotens
//! nachgewiesene Notwendigkeit gedeckt ist (siehe `crate`-Moduldoku,
//! Abschnitt „Was sich gegenüber dem ursprünglichen Auftrag geändert hat").
//! `Display`, `Debug` (delegiert an `Display`) und `std::error::Error` sind
//! deshalb von Hand geschrieben statt über
//! `#[derive(harw_macros::HarwError)]` erzeugt — inhaltlich dieselbe Form
//! wie `harw_dod_cap::SensorError` und `harw-sentinel::ipc::IpcError`, nur
//! ohne das Makro. Kein `anyhow`, kein `thiserror`.
//!
//! # Inhaltsfreiheit
//! [`ProbeError::Sensor`] ist inhaltsfrei (delegiert an
//! `harw_dod_cap::SensorError`, das es selbst ist). Die übrigen Varianten
//! tragen genau so viel Kontext wie `harw-sentinel::ipc::IpcError::Bind`
//! (den vom Betreiber selbst konfigurierten Socket-Pfad) — kein
//! Dateiinhalt, kein vom Host gelesenes Geheimnis.
//!
//! # Exportierte Typen
//! [`ProbeError`].
//!
//! # Nebenläufigkeit
//! Reine Daten ohne innere Veränderlichkeit: `Send + Sync`.
//!
//! # Examples
//! ```rust,ignore
//! use crate::error::ProbeError;
//!
//! let err = ProbeError::LandlockUnavailable;
//! assert!(err.to_string().contains("Landlock"));
//! ```

use std::fmt;

/// Fehler dieser Sonde.
///
/// # Description
/// Siehe Moduldoku für die Einteilung der Varianten und die
/// Inhaltsfreiheits-Ausnahme.
pub enum ProbeError {
    /// Landlock hat den konfigurierten Lesebereich nicht vollständig
    /// durchgesetzt (jeder Ausgang außer `RulesetStatus::FullyEnforced`).
    ///
    /// Für dieses Binary immer ein harter Startfehler — siehe
    /// [`crate::landlock`]-Moduldoku, Abschnitt „Die harte Entscheidung".
    LandlockUnavailable,
    /// Die reale fanotify-Ereignisquelle ist nicht verfügbar (siehe
    /// [`crate::source`]).
    FanotifySourceUnavailable,
    /// Der Verbindungsaufbau zum Sentinel-Socket ist gescheitert.
    ///
    /// # Arguments
    /// - `path`: der konfigurierte Socket-Pfad, so wie der Betreiber ihn
    ///   über `--sentinel-socket` angegeben hat (kein Hostgeheimnis).
    SentinelConnectFailed {
        /// Der Pfad, unter dem der Verbindungsaufbau fehlschlug.
        path: String,
    },
    /// Das Senden eines Ereignisses an den Sentinel ist gescheitert.
    SentinelSendFailed,
    /// Ein geformtes Ereignis ließ sich nicht als JSON kodieren.
    ///
    /// # Arguments
    /// - `source` (`serde_json::Error`): die zugrunde liegende Serde-Ursache.
    ///   Über `std::error::Error::source()` verlinkt.
    EventEncodeFailed(serde_json::Error),
    /// Ein Sensorfehler aus der Sammelschleife.
    ///
    /// # Arguments
    /// - `source` (`harw_dod_cap::SensorError`): der zugrunde liegende,
    ///   inhaltsfreie Sensorfehler. Über `std::error::Error::source()`
    ///   verlinkt.
    Sensor(harw_dod_cap::SensorError),
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LandlockUnavailable => write!(
                f,
                "Landlock did not fully enforce the configured read scope; refusing to start"
            ),
            Self::FanotifySourceUnavailable => write!(
                f,
                "fanotify event source is not available; see the harw-probe-fs source module documentation"
            ),
            Self::SentinelConnectFailed { path } => {
                write!(f, "failed to connect to sentinel socket at '{path}'")
            }
            Self::SentinelSendFailed => write!(f, "failed to send an event to the sentinel"),
            Self::EventEncodeFailed(source) => {
                write!(f, "failed to encode a security event as JSON: {source}")
            }
            Self::Sensor(source) => write!(f, "sensor error: {source}"),
        }
    }
}

impl fmt::Debug for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for ProbeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::EventEncodeFailed(source) => Some(source),
            Self::Sensor(source) => Some(source),
            _ => None,
        }
    }
}

impl From<harw_dod_cap::SensorError> for ProbeError {
    fn from(err: harw_dod_cap::SensorError) -> Self {
        Self::Sensor(err)
    }
}

impl From<serde_json::Error> for ProbeError {
    fn from(err: serde_json::Error) -> Self {
        Self::EventEncodeFailed(err)
    }
}

#[cfg(test)]
mod tests {
    use super::ProbeError;

    #[test]
    fn test_landlock_unavailable_display_is_exact() {
        assert_eq!(
            ProbeError::LandlockUnavailable.to_string(),
            "Landlock did not fully enforce the configured read scope; refusing to start"
        );
    }

    #[test]
    fn test_fanotify_source_unavailable_display_is_exact() {
        assert_eq!(
            ProbeError::FanotifySourceUnavailable.to_string(),
            "fanotify event source is not available; see the harw-probe-fs source module documentation"
        );
    }

    #[test]
    fn test_sentinel_connect_failed_display_includes_the_configured_path() {
        let err = ProbeError::SentinelConnectFailed {
            path: "/run/harw-sentinel.sock".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "failed to connect to sentinel socket at '/run/harw-sentinel.sock'"
        );
    }

    #[test]
    fn test_sentinel_send_failed_display_is_exact() {
        assert_eq!(
            ProbeError::SentinelSendFailed.to_string(),
            "failed to send an event to the sentinel"
        );
    }

    #[test]
    fn test_debug_delegates_to_display() {
        let err = ProbeError::LandlockUnavailable;
        assert_eq!(format!("{err:?}"), err.to_string());
    }

    #[test]
    fn test_sensor_error_source_links_to_underlying_sensor_error() {
        let err = ProbeError::Sensor(harw_dod_cap::SensorError::OutsideScope);
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn test_from_sensor_error_wraps_into_sensor_variant() {
        let mapped: ProbeError = harw_dod_cap::SensorError::MalformedSource.into();
        assert!(matches!(
            mapped,
            ProbeError::Sensor(harw_dod_cap::SensorError::MalformedSource)
        ));
    }

    #[test]
    fn test_from_serde_json_error_wraps_into_event_encode_failed() {
        let json_err = serde_json::from_str::<serde_json::Value>("{not valid json")
            .expect_err("deliberately malformed JSON");
        let mapped: ProbeError = json_err.into();
        assert!(matches!(mapped, ProbeError::EventEncodeFailed(_)));
        assert!(std::error::Error::source(&mapped).is_some());
    }
}
