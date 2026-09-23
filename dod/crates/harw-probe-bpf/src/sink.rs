//! Push-only-Senke: verbindet sich zum Sentinel-Socket und sendet, ohne je
//! zurückzulesen.
//!
//! # Die tragende Eigenschaft
//! [`EventSink`] hat **genau eine** Methode, [`EventSink::send`], und diese
//! nimmt ausschließlich Daten **entgegen** — sie liefert nichts vom Sentinel
//! zurück, außer einem `Result`, das nur Erfolg/Fehler des eigenen
//! Sendevorgangs meldet. Es gibt kein `poll_command`, kein
//! `next_instruction`, keine Methode, über die der Sentinel diesem Prozess
//! irgendetwas mitteilen könnte. Ein Prozess mit `CAP_BPF`, der Nachrichten
//! annimmt, ist ein Angriffsziel; einer, der nur sendet, ist keins. Siehe
//! `crate`-Moduldoku, Abschnitt „Push-Only", für die vollständige
//! Begründung dieser Entscheidung als Konsument.
//!
//! **Woran ein späterer Leser das prüft, ohne diese Datei zu lesen:**
//! `src/push_only_guard.rs` durchsucht den Quelltext jeder
//! Implementierungsdatei dieser Crate (außer sich selbst) nach den beiden
//! Bezeichnern, die einen Empfangspfad ausmachen würden — der
//! Unix-Socket-Empfangsfunktion und der Funktion, die einen Puffer bis zum
//! Streamende einliest. Keiner der beiden kommt in dieser Crate vor; der
//! Test schlägt fehl, sobald das nicht mehr stimmt.
//!
//! # Die Gegenstelle: `harw-sentinel::ipc`
//! Der Sentinel (Knoten AW2-19) hört bereits auf genau diesem Socket
//! (`harw-sentinel/src/ipc.rs::IpcListener`, `SOCK_SEQPACKET`, ein
//! kanonisches JSON-Dokument je `harw_dod_signals::SecurityEvent` als eine
//! Nachricht). [`SentinelSink`] ist die Gegenstelle: `connect()` statt
//! `bind()`/`listen()`/`accept()`, `send()` statt der entsprechenden
//! Empfangsfunktion der Gegenstelle (siehe `src/push_only_guard.rs` für den
//! genauen Bezeichner, den diese Crate nicht verwenden darf). Dieselbe
//! Gegenstelle, dasselbe Wire-Format, dieselbe Bauweise, die
//! `harw-probe-fs::sink` bereits für ihren eigenen Socket zu diesem Sentinel
//! verwendet — diese Datei ist absichtlich strukturell identisch dazu.
//!
//! # Ohne `unsafe`
//! `rustix::net` deckt `socket`, `connect` und `send` bereits sicher ab —
//! jede hier verwendete Funktion ist ein normaler, sicherer Aufruf. Diese
//! Implementierung wurde gegen dieselbe, bereits im Workspace verwendete
//! `rustix`-API geschrieben (`docs.rs/rustix/1.1.4`, Muster:
//! `harw-sentinel/src/ipc.rs`, `harw-probe-fs/src/sink.rs`), aber **nie
//! gegen einen echten Socket ausgeführt** — diese Aufgabe verbietet jeden
//! `cargo`-Aufruf und das Öffnen eines echten Sockets in einem Test
//! (zentrale, sequenzielle Verifikation).
//!
//! # Wire-Format
//! Jede Nachricht ist ein kanonisches JSON-Dokument eines
//! `harw_dod_signals::SecurityEvent`, über [`serde_json::to_vec`] kodiert —
//! derselbe Vertrag, den die Ereignis-Entgegennahme von
//! `harw-sentinel::ipc::IpcConnection` auf der Empfangsseite voraussetzt.
//! `SOCK_SEQPACKET` überträgt jede [`EventSink::send`]-Nutzlast als eine
//! eigenständige Nachricht; diese Sonde reicht deshalb nie mehr als ein
//! Ereignis in einem Aufruf durch.
//!
//! # Exportierte Typen
//! [`EventSink`], [`SentinelSink`], [`build_sentinel_sink`].
//!
//! # Nebenläufigkeit
//! `EventSink: Send + Sync` — [`SentinelSink`] hält nur einen
//! `rustix::fd::OwnedFd` ohne innere Veränderlichkeit und kann hinter `Arc`
//! gehalten werden.
//!
//! # Fehler
//! [`crate::error::ProbeError::SentinelConnectFailed`] aus
//! [`SentinelSink::connect`]/[`build_sentinel_sink`];
//! [`crate::error::ProbeError::SentinelSendFailed`] und
//! [`crate::error::ProbeError::EventEncodeFailed`] aus
//! [`EventSink::send`].
//!
//! # Examples
//! ```rust,ignore
//! use crate::sink::build_sentinel_sink;
//! use std::path::Path;
//!
//! let sink = build_sentinel_sink(Path::new("/run/harw-sentinel.sock"))?;
//! # Ok::<(), crate::error::ProbeError>(())
//! ```

use std::path::Path;

use harw_dod_signals::SecurityEvent;
use rustix::fd::OwnedFd;

use crate::error::ProbeError;

/// Eine Senke, die Sicherheitsereignisse ausschließlich sendet.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Die tragende Eigenschaft": genau eine
/// Methode, kein Empfangspfad ist über diese Schnittstelle ausdrückbar.
pub trait EventSink: Send + Sync {
    /// Sendet ein geformtes Ereignis an den Sentinel.
    ///
    /// # Arguments
    /// - `event` (`&harw_dod_signals::SecurityEvent`): das bereits geformte
    ///   Ereignis, wie es `crate::sensors::build_procmon_sensor`/
    ///   `build_flow_sensor` liefern. Trägt nie Rohereignisbytes (siehe
    ///   Typ-Doku von `SecurityEvent` und `crate::sensors`-Moduldoku).
    ///
    /// # Returns
    /// `Ok(())`, wenn das Ereignis erfolgreich übertragen wurde.
    ///
    /// # Errors
    /// Implementierungsabhängig; [`SentinelSink`] meldet
    /// [`crate::error::ProbeError::EventEncodeFailed`] und
    /// [`crate::error::ProbeError::SentinelSendFailed`].
    fn send(&self, event: &SecurityEvent) -> Result<(), ProbeError>;
}

/// Kodiert ein Ereignis als das kanonische Wire-Format (JSON).
///
/// # Description
/// Eigene Funktion statt eines Inline-Aufrufs, damit sie ohne Socket
/// getestet werden kann (siehe Tests unten) — dieselbe Kodierung, die die
/// Ereignis-Entgegennahme von `harw-sentinel::ipc::IpcConnection` auf der
/// Empfangsseite voraussetzt.
///
/// # Arguments
/// - `event` (`&harw_dod_signals::SecurityEvent`): das zu kodierende
///   Ereignis.
///
/// # Returns
/// Die UTF-8-kodierten JSON-Bytes dieses Ereignisses.
///
/// # Errors
/// [`ProbeError::EventEncodeFailed`]: wenn die Serialisierung scheitert
/// (laut `serde_json`-Dokumentation nur bei einem fehlerhaften
/// `Serialize`-Impl — für `SecurityEvent` in der Praxis unerreichbar, aber
/// nicht per Typ ausgeschlossen).
fn encode_event(event: &SecurityEvent) -> Result<Vec<u8>, ProbeError> {
    Ok(serde_json::to_vec(event)?)
}

/// Eine verbundene Senke, die Ereignisse über einen `SOCK_SEQPACKET`-Unix-
/// Socket an den Sentinel sendet.
///
/// # Description
/// **Push-only als Typ-Eigenschaft:** dieser Typ hat bewusst keine
/// Empfangsmethode und keine Möglichkeit, eine bereits gesendete Nachricht
/// zu widerrufen — siehe Moduldoku.
pub struct SentinelSink {
    socket: OwnedFd,
}

impl SentinelSink {
    /// Baut den Socket und verbindet sich mit dem Sentinel unter `path`.
    ///
    /// # Description
    /// `socket()` dann `connect()`, in dieser Reihenfolge — kein `bind()`,
    /// kein `listen()`, keine Annahmefunktion für eingehende Verbindungen:
    /// diese Sonde ist die Gegenstelle eines bereits lauschenden Sentinels,
    /// nicht selbst eine Lauschstelle. Kein Schritt hiervon ist `unsafe`
    /// (siehe Moduldoku).
    ///
    /// # Arguments
    /// - `path` (`&std::path::Path`): Pfad des `SOCK_SEQPACKET`-Sockets, an
    ///   dem der Sentinel lauscht (`--sentinel-socket`).
    ///
    /// # Returns
    /// Eine verbundene `SentinelSink`, bereit für [`EventSink::send`].
    ///
    /// # Errors
    /// [`ProbeError::SentinelConnectFailed`], wenn `socket()`,
    /// `SocketAddrUnix::new()` oder `connect()` scheitert (Sentinel läuft
    /// nicht, Pfad fehlt, keine Berechtigung, Pfad zu lang).
    pub fn connect(path: &Path) -> Result<Self, ProbeError> {
        use rustix::net::{self, AddressFamily, SocketType};

        let connect_err = || ProbeError::SentinelConnectFailed {
            path: path.display().to_string(),
        };

        let socket = net::socket(AddressFamily::UNIX, SocketType::SEQPACKET, None)
            .map_err(|_| connect_err())?;
        let addr = net::SocketAddrUnix::new(path).map_err(|_| connect_err())?;
        net::connect(&socket, &addr).map_err(|_| connect_err())?;

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
/// # Description
/// Dünner Konstruktor über [`SentinelSink::connect`], hinter `Box<dyn
/// EventSink>`, damit [`crate::collect`] gegen die Abstraktion arbeitet,
/// nicht gegen den konkreten Transporttyp.
///
/// # Arguments
/// - `socket_path` (`&std::path::Path`): der Pfad des
///   `SOCK_SEQPACKET`-Unix-Sockets, an den sich diese Sonde verbindet.
///
/// # Returns
/// Eine verbundene Senke.
///
/// # Errors
/// [`ProbeError::SentinelConnectFailed`], siehe [`SentinelSink::connect`].
pub fn build_sentinel_sink(socket_path: &Path) -> Result<Box<dyn EventSink>, ProbeError> {
    Ok(Box::new(SentinelSink::connect(socket_path)?))
}

#[cfg(test)]
mod tests {
    use harw_dod_signals::{EventKind, SecurityEvent};
    use harw_types::{ContentDigest, SensorId};

    use super::encode_event;
    use crate::test_support::{TestResult, ctx};

    fn sample_event() -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("probe-bpf-procmon-0"),
            observed_at: jiff::Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::ProcessExec {
                path: "/usr/sbin/sshd".to_owned(),
                argv_digest: ContentDigest::of(b"unused in this test"),
            },
        }
    }

    #[test]
    fn test_encode_event_round_trips_through_serde_json() -> TestResult {
        let event = sample_event();
        let bytes = encode_event(&event).map_err(ctx("SecurityEvent serializes"))?;
        let decoded: SecurityEvent =
            serde_json::from_slice(&bytes).map_err(ctx("round-trips through the wire format"))?;
        assert_eq!(decoded, event);
        Ok(())
    }

    #[test]
    fn test_encode_event_produces_valid_utf8_json() -> TestResult {
        let bytes = encode_event(&sample_event()).map_err(ctx("SecurityEvent serializes"))?;
        let text = String::from_utf8(bytes).map_err(ctx("wire format is valid UTF-8"))?;
        assert!(text.contains("process-exec"));
        Ok(())
    }

    // Bewusst kein Test öffnet einen echten Socket: weder `SentinelSink::connect`
    // noch `build_sentinel_sink` werden hier aufgerufen — nach
    // Aufgabenstellung ausdrücklich untersagt ("öffne … keinen echten
    // Socket"), genau wie `harw-sentinel::ipc` und `harw-probe-fs::sink`
    // keinen Test besitzen, der einen echten Socket bindet oder verbindet.
    // Die Push-Only-Zusage selbst ist über `src/push_only_guard.rs` geprüft,
    // der eigenständigen, quelltextbasierten Prüfung — siehe Moduldoku.
}
