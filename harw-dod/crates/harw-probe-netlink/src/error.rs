//! Fehlertyp der Audit-Netlink-Sonde `harw-probe-netlink`.
//!
//! # Verantwortungsbereich
//! [`ProbeError`] ist der eine Fehlertyp dieses Binaries. Er deckt ab: die
//! Landlock-Durchsetzung ([`crate::landlock`]), das Öffnen und Lesen der
//! Audit-Quelle (über `harw_dod_netlink::NetlinkError`), den Sendeweg zum
//! Sentinel ([`crate::sink`]), eine ungültige `--sensor-id` und den
//! `completions`-Unterbefehl. Kommandozeilenfehler gehören **nicht** hierher
//! — `clap::Error` behandelt `main` direkt.
//!
//! `Display`, `Debug` (delegiert an `Display`) und `std::error::Error` sind
//! von Hand geschrieben, wie bei `harw-probe-fs` — kein `anyhow`, kein
//! `thiserror`.
//!
//! # Inhaltsfreiheit
//! Keine Variante trägt Recordinhalt, einen Benutzer- oder Hostnamen oder
//! einen Pfad: `main` loggt jeden Fehler aus `run` auf `error`-Stufe, und
//! dort hat auch der Socket-Pfad nichts zu suchen (er steht bereits auf
//! `info` im Log, bevor verbunden wird). [`ProbeError::Completions`] ist die
//! Ausnahme — er trägt höchstens Pfade der Shell-Konfiguration des
//! aufrufenden Nutzers und wird deshalb nur auf stderr ausgegeben, nie
//! geloggt.
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

use harw_dod_netlink::NetlinkError;

/// Fehler dieser Sonde.
///
/// # Description
/// Siehe Moduldoku für die Einteilung der Varianten und die
/// Inhaltsfreiheit.
pub enum ProbeError {
    /// Landlock hat die leere Dateisystemsicht nicht vollständig
    /// durchgesetzt (jeder Ausgang außer `RulesetStatus::FullyEnforced`).
    ///
    /// Für dieses Binary immer ein harter Startfehler — siehe
    /// [`crate::landlock`]-Moduldoku.
    LandlockUnavailable,
    /// Der Audit-Netlink-Socket ließ sich nicht öffnen oder nicht an die
    /// Lesegruppe binden (fehlende `CAP_AUDIT_READ`, Kernel ohne Audit,
    /// keine Linux-Plattform).
    ///
    /// # Arguments
    /// - `0` (`harw_dod_netlink::NetlinkError`): die inhaltsfreie Ursache.
    AuditSourceUnavailable(NetlinkError),
    /// Das Lesen der Audit-Quelle ist dauerhaft gescheitert. Ein
    /// `NetlinkError::Overrun` oder `NetlinkError::Truncated` landet
    /// **nicht** hier — beides ist ein gemeldeter Verlust, kein Abbruchgrund
    /// (siehe [`crate::collect`]).
    ///
    /// # Arguments
    /// - `0` (`harw_dod_netlink::NetlinkError`): die inhaltsfreie Ursache.
    AuditRead(NetlinkError),
    /// Der Verbindungsaufbau zum Sentinel-Socket ist gescheitert. Ohne Pfad
    /// (siehe Moduldoku, „Inhaltsfreiheit").
    SentinelConnectFailed,
    /// Das Senden eines Ereignisses an den Sentinel ist gescheitert.
    SentinelSendFailed,
    /// Ein Ereignis ließ sich nicht als JSON kodieren.
    ///
    /// # Arguments
    /// - `0` (`serde_json::Error`): die zugrunde liegende Serde-Ursache.
    EventEncodeFailed(serde_json::Error),
    /// Ein kodiertes Ereignis ist größer, als der Sentinel als eine
    /// Nachricht annimmt; es wird nicht gesendet statt abgeschnitten.
    EventTooLarge {
        /// Länge der Kodierung in Byte.
        len: usize,
        /// Obergrenze in Byte ([`crate::sink::MAX_EVENT_BYTES`]).
        limit: usize,
    },
    /// `--sensor-id` ist leer oder besteht nur aus Leerraum.
    InvalidSensorId,
    /// Der `completions`-Unterbefehl ist gescheitert.
    ///
    /// # Arguments
    /// - `0` (`harw_completions::CompletionError`): die zugrunde liegende
    ///   Ursache.
    Completions(harw_completions::CompletionError),
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LandlockUnavailable => write!(
                f,
                "Landlock did not fully enforce the empty file system scope; refusing to start"
            ),
            Self::AuditSourceUnavailable(source) => {
                write!(f, "audit netlink source is not available: {source}")
            }
            Self::AuditRead(source) => {
                write!(f, "reading the audit netlink source failed: {source}")
            }
            Self::SentinelConnectFailed => write!(f, "failed to connect to the sentinel socket"),
            Self::SentinelSendFailed => write!(f, "failed to send an event to the sentinel"),
            Self::EventEncodeFailed(source) => {
                write!(f, "failed to encode a security event as JSON: {source}")
            }
            Self::EventTooLarge { len, limit } => write!(
                f,
                "encoded security event has {len} bytes, more than the sentinel accepts ({limit})"
            ),
            Self::InvalidSensorId => write!(f, "--sensor-id must not be blank"),
            Self::Completions(source) => write!(f, "shell completions failed: {source}"),
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
            Self::AuditSourceUnavailable(source) | Self::AuditRead(source) => Some(source),
            Self::EventEncodeFailed(source) => Some(source),
            Self::Completions(source) => Some(source),
            Self::LandlockUnavailable
            | Self::SentinelConnectFailed
            | Self::SentinelSendFailed
            | Self::EventTooLarge { .. }
            | Self::InvalidSensorId => None,
        }
    }
}

impl From<serde_json::Error> for ProbeError {
    fn from(err: serde_json::Error) -> Self {
        Self::EventEncodeFailed(err)
    }
}

impl From<harw_completions::CompletionError> for ProbeError {
    fn from(err: harw_completions::CompletionError) -> Self {
        Self::Completions(err)
    }
}

#[cfg(test)]
mod tests {
    use harw_dod_cap::SensorError;
    use harw_dod_netlink::NetlinkError;

    use super::ProbeError;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn test_landlock_unavailable_display_is_exact() {
        assert_eq!(
            ProbeError::LandlockUnavailable.to_string(),
            "Landlock did not fully enforce the empty file system scope; refusing to start"
        );
    }

    #[test]
    fn test_sentinel_connect_failed_display_names_no_path() {
        let message = ProbeError::SentinelConnectFailed.to_string();
        assert_eq!(message, "failed to connect to the sentinel socket");
        assert!(!message.contains('/'));
    }

    #[test]
    fn test_audit_errors_delegate_to_the_content_free_netlink_error() {
        let unavailable =
            ProbeError::AuditSourceUnavailable(NetlinkError::from(SensorError::SourceUnavailable));
        assert_eq!(
            unavailable.to_string(),
            "audit netlink source is not available: sensor source is unavailable on this host"
        );
        assert!(std::error::Error::source(&unavailable).is_some());

        let read = ProbeError::AuditRead(NetlinkError::MalformedRecord);
        assert_eq!(
            read.to_string(),
            "reading the audit netlink source failed: audit record is malformed"
        );
    }

    #[test]
    fn test_event_too_large_display_names_sizes_only() {
        let err = ProbeError::EventTooLarge {
            len: 20_000,
            limit: 16_384,
        };
        assert_eq!(
            err.to_string(),
            "encoded security event has 20000 bytes, more than the sentinel accepts (16384)"
        );
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn test_debug_delegates_to_display() {
        let err = ProbeError::InvalidSensorId;
        assert_eq!(format!("{err:?}"), err.to_string());
    }

    #[test]
    fn test_from_serde_json_error_wraps_into_event_encode_failed() -> TestResult {
        let result = serde_json::from_str::<serde_json::Value>("{not valid json");
        let Err(json_err) = result else {
            return Err(TestError::Unexpected(
                "absichtlich kaputtes JSON: Err erwartet".to_owned(),
            ));
        };
        let mapped: ProbeError = json_err.into();
        assert!(matches!(mapped, ProbeError::EventEncodeFailed(_)));
        assert!(std::error::Error::source(&mapped).is_some());
        Ok(())
    }
}
