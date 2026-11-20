//! Push-only-Senke: verbindet sich zum Sentinel-Socket und sendet, ohne je
//! zurückzulesen.
//!
//! # Die tragende Eigenschaft
//! [`EventSink`] hat **genau eine** Methode, [`EventSink::send`], und diese
//! nimmt ausschließlich Daten **entgegen** — sie liefert nichts vom Sentinel
//! zurück, außer einem `Result`, das nur Erfolg/Fehler des eigenen
//! Sendevorgangs meldet. Es gibt keine Methode, über die der Sentinel diesem
//! Prozess etwas mitteilen könnte. Ein Prozess mit `CAP_AUDIT_READ`, der
//! Nachrichten annimmt, ist ein Angriffsziel; einer, der nur sendet, ist
//! keins. `src/push_only_guard.rs` prüft das am Quelltext dieser Crate.
//!
//! # Die Gegenstelle: `harw-sentinel::ipc`
//! Der Sentinel hört auf diesem Socket (`SOCK_SEQPACKET`, ein kanonisches
//! JSON-Dokument je `harw_dod_signals::SecurityEvent` als eine Nachricht).
//! [`SentinelSink`] ist die Gegenstelle: `connect()` statt
//! `bind()`/`listen()`, `send()` statt einer Empfangsfunktion. Dieselbe
//! Gegenstelle, dasselbe Wire-Format und dieselbe Bauweise wie
//! `harw-probe-bpf::sink` und `harw-probe-fs::sink` — diese Datei ist
//! absichtlich strukturell identisch dazu; es gibt keine gemeinsame Crate,
//! weil jede privilegierte Sonde ihre eigene, im eigenen Quelltext geprüfte
//! Kopie trägt.
//!
//! # Wire-Format und Größengrenze
//! Jede Nachricht ist [`serde_json::to_vec`] eines `SecurityEvent`, ohne
//! Längenpräfix; `SOCK_SEQPACKET` überträgt jede [`EventSink::send`]-Nutzlast
//! als eine eigenständige Nachricht. Der Sentinel nimmt höchstens
//! [`MAX_EVENT_BYTES`] je Nachricht an und verwirft längere. Diese Sonde
//! prüft die Grenze **vor** dem Senden und meldet
//! [`crate::error::ProbeError::EventTooLarge`], statt eine Nachricht zu
//! schicken, die drüben still verschwände. Ein `AuthEvent` ist weit kleiner
//! (rund 200 Byte); die Grenze greift nur bei einer absurd langen
//! `--sensor-id`.
//!
//! # Ohne `unsafe`
//! `rustix::net` deckt `socket`, `connect` und `send` sicher ab. Diese Datei
//! wurde gegen die API in `rustix-1.1.5` geschrieben, aber **nie gegen einen
//! echten Socket ausgeführt**.
//!
//! # Exportierte Typen
//! [`EventSink`], [`SentinelSink`], [`build_sentinel_sink`],
//! [`MAX_EVENT_BYTES`].
//!
//! # Nebenläufigkeit
//! `EventSink: Send + Sync` — [`SentinelSink`] hält nur einen
//! `rustix::fd::OwnedFd` ohne innere Veränderlichkeit.
//!
//! # Fehler
//! [`crate::error::ProbeError::SentinelConnectFailed`] aus
//! [`SentinelSink::connect`]; [`crate::error::ProbeError::SentinelSendFailed`],
//! [`crate::error::ProbeError::EventEncodeFailed`] und
//! [`crate::error::ProbeError::EventTooLarge`] aus [`EventSink::send`].
//!
//! # Examples
//! ```rust,ignore
//! use crate::sink::build_sentinel_sink;
//! use std::path::Path;
//!
//! let sink = build_sentinel_sink(Path::new("/run/harw/sentinel.sock"))?;
//! # Ok::<(), crate::error::ProbeError>(())
//! ```

use std::path::Path;

use harw_dod_signals::SecurityEvent;
use rustix::fd::OwnedFd;

use crate::error::ProbeError;

/// Größte Nachricht in Byte, die der Sentinel annimmt (sein Empfangspuffer in
/// `harw-sentinel/src/ipc.rs`).
pub const MAX_EVENT_BYTES: usize = 16_384;

/// Eine Senke, die Sicherheitsereignisse ausschließlich sendet.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Die tragende Eigenschaft": genau eine
/// Methode, kein Empfangspfad ist über diese Schnittstelle ausdrückbar.
pub trait EventSink: Send + Sync {
    /// Sendet ein geformtes Ereignis an den Sentinel.
    ///
    /// # Arguments
    /// - `event` (`&harw_dod_signals::SecurityEvent`): das geformte
    ///   Ereignis (`AuthEvent` oder `SensorDegraded`). Trägt nie
    ///   Recordinhalt außer den getypten Feldern.
    ///
    /// # Returns
    /// `Ok(())`, wenn das Ereignis erfolgreich übertragen wurde.
    ///
    /// # Errors
    /// Implementierungsabhängig; [`SentinelSink`] meldet
    /// [`ProbeError::EventEncodeFailed`], [`ProbeError::EventTooLarge`] und
    /// [`ProbeError::SentinelSendFailed`].
    fn send(&self, event: &SecurityEvent) -> Result<(), ProbeError>;
}

/// Kodiert ein Ereignis als das kanonische Wire-Format (JSON).
///
/// # Description
/// Eigene Funktion statt eines Inline-Aufrufs, damit sie ohne Socket
/// getestet werden kann.
///
/// # Returns
/// Die UTF-8-kodierten JSON-Bytes, höchstens [`MAX_EVENT_BYTES`] lang.
///
/// # Errors
/// [`ProbeError::EventEncodeFailed`], wenn die Serialisierung scheitert;
/// [`ProbeError::EventTooLarge`], wenn die Kodierung die Grenze überschreitet.
fn encode_event(event: &SecurityEvent) -> Result<Vec<u8>, ProbeError> {
    let bytes = serde_json::to_vec(event)?;
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(ProbeError::EventTooLarge {
            len: bytes.len(),
            limit: MAX_EVENT_BYTES,
        });
    }
    Ok(bytes)
}

/// Eine verbundene Senke, die Ereignisse über einen `SOCK_SEQPACKET`-Unix-
/// Socket an den Sentinel sendet.
///
/// # Description
/// **Push-only als Typ-Eigenschaft:** dieser Typ hat bewusst keine
/// Empfangsmethode — siehe Moduldoku.
pub struct SentinelSink {
    /// Der verbundene Socket.
    socket: OwnedFd,
}

impl SentinelSink {
    /// Baut den Socket und verbindet sich mit dem Sentinel unter `path`.
    ///
    /// # Description
    /// `socket()` dann `connect()` — kein `bind()`, kein `listen()`: diese
    /// Sonde ist die Gegenstelle eines bereits lauschenden Sentinels.
    ///
    /// # Arguments
    /// - `path` (`&std::path::Path`): Pfad des `SOCK_SEQPACKET`-Sockets
    ///   (`--sentinel-socket`).
    ///
    /// # Returns
    /// Eine verbundene `SentinelSink`.
    ///
    /// # Errors
    /// [`ProbeError::SentinelConnectFailed`], wenn `socket()`,
    /// `SocketAddrUnix::new()` oder `connect()` scheitert (Sentinel läuft
    /// nicht, Pfad fehlt, keine Berechtigung, Pfad zu lang).
    pub fn connect(path: &Path) -> Result<Self, ProbeError> {
        use rustix::net::{self, AddressFamily, SocketType};

        let socket = net::socket(AddressFamily::UNIX, SocketType::SEQPACKET, None)
            .map_err(|_| ProbeError::SentinelConnectFailed)?;
        let addr = net::SocketAddrUnix::new(path).map_err(|_| ProbeError::SentinelConnectFailed)?;
        net::connect(&socket, &addr).map_err(|_| ProbeError::SentinelConnectFailed)?;

        Ok(Self { socket })
    }
}

impl EventSink for SentinelSink {
    fn send(&self, event: &SecurityEvent) -> Result<(), ProbeError> {
        use rustix::net::SendFlags;

        let bytes = encode_event(event)?;
        rustix::net::send(&self.socket, &bytes, SendFlags::empty())
            .map_err(|_| ProbeError::SentinelSendFailed)?;
        Ok(())
    }
}

/// Baut die Senke, die Ereignisse an den Sentinel-Socket überträgt.
///
/// # Arguments
/// - `socket_path` (`&std::path::Path`): der Pfad des
///   `SOCK_SEQPACKET`-Unix-Sockets.
///
/// # Returns
/// Eine verbundene Senke hinter `Box<dyn EventSink>`.
///
/// # Errors
/// [`ProbeError::SentinelConnectFailed`], siehe [`SentinelSink::connect`].
pub fn build_sentinel_sink(socket_path: &Path) -> Result<Box<dyn EventSink>, ProbeError> {
    Ok(Box::new(SentinelSink::connect(socket_path)?))
}

#[cfg(test)]
mod tests {
    use harw_dod_signals::{Actor, AuthOutcome, EventKind, ProcessContext, SecurityEvent};
    use harw_types::SensorId;

    use super::{MAX_EVENT_BYTES, encode_event};
    use crate::error::ProbeError;
    use crate::test_support::{TestError, TestResult, ctx};

    fn auth_event() -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("probe-netlink-auth-0"),
            observed_at: jiff::Timestamp::UNIX_EPOCH,
            actor: Some(Actor {
                uid: 0,
                auid: Some(1000),
                cgroup: None,
                process: ProcessContext::from_pid(4242),
            }),
            kind: EventKind::AuthEvent {
                outcome: AuthOutcome::Failure,
            },
        }
    }

    #[test]
    fn test_encode_event_round_trips_through_serde_json() -> TestResult {
        let event = auth_event();
        let bytes = encode_event(&event).map_err(ctx("SecurityEvent kodiert"))?;
        let decoded: SecurityEvent =
            serde_json::from_slice(&bytes).map_err(ctx("Wire-Format liest sich zurück"))?;
        assert_eq!(decoded, event);
        Ok(())
    }

    #[test]
    fn test_encode_event_uses_the_kebab_case_auth_event_tag_without_length_prefix() -> TestResult {
        let bytes = encode_event(&auth_event()).map_err(ctx("SecurityEvent kodiert"))?;
        let text = String::from_utf8(bytes).map_err(ctx("Wire-Format ist UTF-8"))?;
        assert!(text.starts_with('{'));
        assert!(text.contains(r#""kind":"auth-event""#));
        assert!(text.contains(r#""outcome":"failure""#));
        assert!(text.len() < MAX_EVENT_BYTES);
        Ok(())
    }

    #[test]
    fn test_encode_event_rejects_an_event_above_the_sentinel_limit() -> TestResult {
        let mut event = auth_event();
        event.sensor = SensorId::from_str("x".repeat(MAX_EVENT_BYTES));
        let Err(err) = encode_event(&event) else {
            return Err(TestError::Unexpected("EventTooLarge erwartet".into()));
        };
        assert!(matches!(
            err,
            ProbeError::EventTooLarge {
                limit: MAX_EVENT_BYTES,
                ..
            }
        ));
        Ok(())
    }

    // Bewusst kein Test öffnet einen echten Socket: weder
    // `SentinelSink::connect` noch `build_sentinel_sink` werden hier
    // aufgerufen, wie bei `harw-probe-bpf::sink` und `harw-probe-fs::sink`.
    // Die Push-Only-Zusage prüft `src/push_only_guard.rs` am Quelltext.
}
