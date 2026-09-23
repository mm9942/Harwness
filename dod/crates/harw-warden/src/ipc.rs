//! IPC-Empfangspfad: `SOCK_SEQPACKET` auf dem von systemd bereits gebundenen
//! und lauschenden Socket. `SO_PEERCRED` ist Pflicht.
//!
//! # Warum `SO_PEERCRED`, nicht ein Token im Rumpf
//! `harw_dod_warden`s eigene Moduldoku (`lib.rs`, Abschnitt „Warum der Beleg
//! nachgeprüft wird, statt ihm zu glauben") benennt die eigentliche
//! Vertrauensgrenze dieses Systems bereits ausdrücklich: „ein
//! `SOCK_SEQPACKET`-Unix-Socket mit `SO_PEERCRED`, AW5-04b". Ein Feld im
//! Nachrichtenrumpf, das „ich bin der Eskalationsleiter" behauptet, wäre
//! eine Behauptung der Gegenseite — genau das, dem der Warden laut seiner
//! eigenen Dokumentation nirgends glaubt. `SO_PEERCRED` liefert stattdessen
//! die vom Kernel selbst festgestellte Identität des verbundenen Prozesses
//! (`pid`/`uid`/`gid`), gefüllt aus dem tatsächlichen Verbindungsaufbau, nie
//! aus fälschbaren Nutzdaten. Diese Datei liest sie deshalb bei jeder
//! angenommenen Verbindung, bevor auch nur ein Byte Nutzlast gelesen wird.
//!
//! # Was mit der Peer-Identität geschieht — und was bewusst nicht
//! Wie `harw-sentinel::ipc` (dort ausdrücklich dokumentiert: „prüft diese
//! Werte heute nicht gegen eine Zulassungsliste — keine ist Teil dieses
//! Knotens; sie werden geloggt") liest und protokolliert diese Datei
//! [`PeerCredentials`] bei jeder Verbindung, führt aber **keine**
//! Zulassungsprüfung gegen eine erwartete `uid`/`pid` durch: der Auftrag
//! dieses Knotens spezifiziert keine solche Liste, und eine hier erfundene
//! Prüfung wäre eine neue, nicht mandatierte Geschäftsregel (dieselbe
//! Zurückhaltung, die `harw_dod_warden::warden`-Moduldoku für die
//! `authorized_by`-Prüfung an anderer Stelle bereits begründet: „ein
//! zweiter Identitätscheck hier wäre eine Prüfung gegen denselben Wert, den
//! man prüfen wollte, sobald keine unabhängige Quelle für „wer darf"
//! existiert"). `SO_PEERCRED` bleibt damit **Pflicht** (gelesen, geloggt,
//! über [`PeerCredentials`] typisiert — nie eine Behauptung im Rumpf), aber
//! diese Datei erfindet keine Allowlist, die der Auftrag nicht verlangt —
//! ein Befund für einen künftigen Knoten, kein stillschweigend getroffener
//! Entwurf hier.
//!
//! # Ohne `unsafe`
//! `rustix::net` deckt `accept`, `sockopt::socket_peercred`, `recv` und
//! `send` bereits sicher ab. Jeder hier verwendete Aufruf ist gegen die
//! tatsächlich im Workspace gepinnte Quelle geprüft
//! (`~/.cargo/registry/src/…/rustix-1.1.4/src/net/`), nicht gegen docs.rs
//! oder aus dem Gedächtnis — dieselbe Prüfung, die `harw-sentinel::ipc`
//! bereits einmal die Falle gefunden hat, in die zwei Agenten zuvor beim
//! bloßen Gegenlesen der Datei getreten waren: `rustix::net::recv` liefert
//! für einen Array-Puffer `(Buf::Output, usize)`, kein Skalar. Diese Datei
//! übernimmt exakt dasselbe Musters wie `harw-sentinel::ipc::IpcConnection::recv_event`.
//!
//! # Ein Austausch pro Verbindung
//! Jede angenommene Verbindung trägt genau eine Anfrage und höchstens eine
//! Antwort — kein dauerhaft offener Kanal, keine Mehrfachnutzung. Das
//! entspricht dem einfachsten RPC-Muster, das für einen seltenen,
//! sitzungsgebundenen Eskalationsaufruf ausreicht (kein Hochfrequenz-Feed
//! wie bei `harw-sentinel`).
//!
//! # Inhaltsfreie Antworten
//! [`harw_dod_warden::warden::WardenOutcome::to_wire`] liefert `None` für
//! [`harw_dod_warden::warden::WardenOutcome::ExecutionFailed`] und
//! [`harw_dod_warden::warden::WardenOutcome::VerificationFailed`] — für
//! beide gibt es keine verlustfreie Wire-Entsprechung (siehe dessen
//! Moduldoku). [`handle_connection`] sendet in diesem Fall **keine**
//! Antwort und schließt die Verbindung; der Peer liest eine geordnete
//! Verbindungsschließung ohne Nutzlast, kein Fehlerdetail. Für
//! [`harw_dod_warden::warden::WardenOutcome::Executed`]/[`harw_dod_warden::warden::WardenOutcome::Denied`]
//! wird die von der Bibliothek gelieferte, bereits inhaltsfrei begrenzte
//! [`harw_dod_warden_proto::WardenResponse`] unverändert gesendet — dieses
//! Binary fügt kein zusätzliches Feld hinzu und entfernt keines.
//!
//! # Nebenläufigkeit
//! [`serve_forever`] startet `std::thread::spawn` je angenommener
//! Verbindung (Muster: `harw-sentinel::ipc::spawn_receive_loop`). `warden`
//! wird als `Arc<Warden>` geteilt; `Warden` selbst trägt keinen
//! veränderlichen inneren Zustand außerhalb seiner Ausführer/seines
//! Audit-Ziels (siehe `harw_dod_warden::warden`-Moduldoku), die ihrerseits
//! entweder zustandslos sind (`CgroupV2Executor`,
//! `crate::isolation::NftNetworkIsolator`,
//! `crate::audit::TracingAuditSink`) oder — in Produktion nicht verwendet —
//! ihre eigene Synchronisation mitbringen (`RecordingExecutor`,
//! `RecordingAuditSink`, nur in Tests der Bibliothek). Gleichzeitige
//! Anfragen sind deshalb ohne zusätzliches Locking sicher.

use std::os::fd::OwnedFd;
use std::sync::Arc;

use harw_dod_warden::Warden;
use harw_macros::HarwError;

use crate::protocol::WardenRequestEnvelope;

/// Empfangspuffer-Größe je Nachricht. Reichlich bemessen für ein einzelnes
/// serialisiertes [`WardenRequestEnvelope`] (Vorbild:
/// `harw-sentinel::ipc::RECV_BUFFER_LEN`, dort `16_384` für ein einzelnes
/// `SecurityEvent`).
const RECV_BUFFER_LEN: usize = 16_384;

/// Fehler des IPC-Empfangspfads — je Verbindung, nicht prozessfatal (siehe
/// `crate::error`-Moduldoku für die Abgrenzung zu [`crate::error::WardenBinError`]).
///
/// # Description
/// Wie `harw-sentinel::ipc::IpcError` inhaltsfrei: kein
/// Betriebssystem-Fehlercode, kein Nachrichteninhalt in einer
/// `Display`-Meldung.
#[derive(Debug, HarwError)]
pub enum IpcError {
    /// Eine eingehende Verbindung konnte nicht angenommen werden.
    #[msg("failed to accept an ipc connection")]
    Accept,
    /// Die Peer-Identität (`SO_PEERCRED`) einer angenommenen Verbindung
    /// konnte nicht gelesen werden.
    #[msg("failed to read peer credentials of an ipc connection")]
    PeerCredentialsUnavailable,
    /// Ein Lesevorgang auf einer angenommenen Verbindung ist fehlgeschlagen.
    #[msg("failed to receive a message from an ipc peer")]
    Recv,
    /// Eine empfangene Nachricht war größer als [`RECV_BUFFER_LEN`] und
    /// wurde vom Kernel abgeschnitten.
    #[msg("received an ipc message larger than the receive buffer; it was discarded")]
    Oversized,
    /// Eine empfangene Nachricht ließ sich nicht als
    /// [`WardenRequestEnvelope`] dekodieren.
    ///
    /// # Arguments
    /// - `0` (`serde_json::Error`): die zugrunde liegende Serde-Ursache.
    #[msg("received an ipc message that could not be decoded as a WardenRequestEnvelope: {0}")]
    #[from]
    Malformed(serde_json::Error),
    /// Eine `WardenResponse` ließ sich nicht als JSON kodieren.
    ///
    /// # Arguments
    /// - `0` (`serde_json::Error`): die zugrunde liegende Serde-Ursache.
    #[msg("failed to encode a warden response for the ipc peer: {0}")]
    ResponseEncode(serde_json::Error),
    /// Das Senden einer Antwort an den Peer ist fehlgeschlagen.
    #[msg("failed to send a response to an ipc peer")]
    Send,
}

/// Kernel-verbürgte Identität des verbundenen Peers (`SO_PEERCRED`).
///
/// # Description
/// Gelesen genau einmal, direkt nach `accept()` — der Kernel füllt diese
/// Werte aus dem verbindenden Prozess, nicht aus dessen (fälschbaren)
/// Nutzlast. Siehe Moduldoku, Abschnitt „Was mit der Peer-Identität
/// geschieht", für die bewusste Auslassung einer Zulassungsprüfung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCredentials {
    /// Prozess-ID des Peers zum Zeitpunkt von `connect()`.
    pub pid: u32,
    /// Effektive Nutzer-ID des Peers.
    pub uid: u32,
    /// Effektive Gruppen-ID des Peers.
    pub gid: u32,
}

/// Nimmt Verbindungen auf dem von systemd übergebenen, bereits lauschenden
/// Socket entgegen und bedient jede in einem eigenen Thread.
///
/// # Description
/// Endlosschleife über `accept()`; scheitert `accept()` dauerhaft (z. B.
/// weil systemd den Socket geschlossen hat), endet diese Funktion — der
/// Aufrufer (`crate::main::run`) behandelt das als
/// [`crate::error::WardenBinError::IpcAcceptLoopTerminated`], kein stiller
/// Rückkehrwert.
///
/// # Arguments
/// - `listener` (`std::os::fd::OwnedFd`): der von
///   [`crate::systemd::acquire_listen_socket`] gelieferte, bereits gebundene
///   und lauschende Socket. Wird von dieser Funktion übernommen.
/// - `warden` (`std::sync::Arc<harw_dod_warden::Warden>`): der produktive
///   Warden (siehe `crate::warden_factory::build_production_warden`),
///   geteilt über jeden Verbindungs-Thread.
///
/// # Concurrency
/// Startet `std::thread::spawn` je angenommener Verbindung, siehe
/// Moduldoku.
pub fn serve_forever(listener: OwnedFd, warden: Arc<Warden>) {
    loop {
        match accept_connection(&listener) {
            Ok((connection, peer)) => {
                let warden = Arc::clone(&warden);
                std::thread::spawn(move || handle_connection(connection, peer, &warden));
            }
            Err(error) => {
                tracing::error!(error = %error, "ipc accept failed; ceasing to serve");
                break;
            }
        }
    }
}

/// Nimmt genau eine Verbindung an und liest sofort ihre `SO_PEERCRED`-Identität.
fn accept_connection(listener: &OwnedFd) -> Result<(OwnedFd, PeerCredentials), IpcError> {
    use rustix::net::sockopt;

    let accepted = rustix::net::accept(listener).map_err(|_| IpcError::Accept)?;
    let creds =
        sockopt::socket_peercred(&accepted).map_err(|_| IpcError::PeerCredentialsUnavailable)?;
    let peer = PeerCredentials {
        pid: creds.pid.as_raw_pid() as u32,
        uid: creds.uid.as_raw() as u32,
        gid: creds.gid.as_raw() as u32,
    };
    Ok((accepted, peer))
}

/// Bedient genau eine angenommene Verbindung: eine Anfrage lesen, an
/// `Warden::handle` weiterreichen (unverändert, siehe Moduldoku), das
/// Ergebnis inhaltsfrei beantworten oder — wo keine Wire-Entsprechung
/// existiert — die Verbindung ohne Antwort schließen.
fn handle_connection(connection: OwnedFd, peer: PeerCredentials, warden: &Warden) {
    tracing::info!(?peer, "ipc connection accepted");
    match recv_envelope(&connection) {
        Ok(Some(envelope)) => {
            let outcome = warden.handle(&envelope.finding, &envelope.request);
            match outcome.to_wire() {
                Some(response) => {
                    if let Err(error) = send_response(&connection, &response) {
                        tracing::warn!(?peer, error = %error, "failed to send warden response to peer");
                    }
                }
                None => {
                    tracing::warn!(
                        ?peer,
                        ?outcome,
                        "no wire representation for this outcome; closing without a reply"
                    );
                }
            }
        }
        Ok(None) => {
            tracing::debug!(
                ?peer,
                "ipc peer closed the connection without sending a request"
            );
        }
        Err(error) => {
            tracing::warn!(?peer, error = %error, "failed to receive a well-formed request from ipc peer");
        }
    }
}

/// Empfängt eine Nachricht und dekodiert sie als [`WardenRequestEnvelope`].
///
/// # Description
/// `SOCK_SEQPACKET` liefert je `recv()` genau ein Datagramm — ein
/// Ereignis, keinen Byte-Strom. Ein Datagramm der Länge `0` ist die
/// geordnete Schließung der Verbindung durch den Peer, kein Fehler.
///
/// `rustix::net::recv` liefert für einen `&mut [u8; N]`-Puffer ein Tupel
/// `(Buf::Output, usize)`, kein Skalar (gegen
/// `rustix-1.1.4/src/net/send_recv/mod.rs` nachgeschlagen, nicht aus dem
/// Gedächtnis übernommen — siehe Moduldoku): `copied` ist die Anzahl
/// tatsächlich in `buffer` geschriebener Bytes, `message_len` die
/// **ungekürzte** Größe des Datagramms (`RecvFlags::TRUNC`).
///
/// # Returns
/// - `Some(envelope)`: eine Nachricht wurde empfangen und dekodiert.
/// - `None`: der Peer hat die Verbindung geordnet geschlossen.
///
/// # Errors
/// - [`IpcError::Recv`]: der zugrunde liegende `recv()`-Aufruf schlägt fehl.
/// - [`IpcError::Oversized`]: das Datagramm war größer als
///   [`RECV_BUFFER_LEN`].
/// - [`IpcError::Malformed`]: die empfangenen Bytes sind kein wohlgeformtes
///   [`WardenRequestEnvelope`]-JSON.
fn recv_envelope(connection: &OwnedFd) -> Result<Option<WardenRequestEnvelope>, IpcError> {
    use rustix::net::RecvFlags;

    let mut buffer = [0u8; RECV_BUFFER_LEN];
    let (copied, message_len) =
        rustix::net::recv(connection, &mut buffer, RecvFlags::TRUNC).map_err(|_| IpcError::Recv)?;

    if message_len == 0 {
        return Ok(None);
    }
    if message_len > buffer.len() {
        // Nachrichtengrenze bei `SOCK_SEQPACKET`: der Rest dieses
        // Datagramms ist verloren, aber der Peer hat korrekt gesendet, nur
        // zu groß für diesen Puffer — kein Parse-Fehler.
        return Err(IpcError::Oversized);
    }

    let envelope: WardenRequestEnvelope = serde_json::from_slice(&buffer[..copied])?;
    Ok(Some(envelope))
}

/// Sendet eine bereits inhaltsfrei begrenzte [`harw_dod_warden_proto::WardenResponse`]
/// unverändert an den Peer.
///
/// # Errors
/// - [`IpcError::ResponseEncode`]: `response` ließ sich nicht als JSON
///   kodieren (praktisch unerreichbar — die von der Bibliothek gelieferten
///   Feldtypen sind ausschließlich `harw-types`-Kennungen und feldlose
///   Enums).
/// - [`IpcError::Send`]: der zugrunde liegende `send()`-Aufruf schlägt fehl.
fn send_response(
    connection: &OwnedFd,
    response: &harw_dod_warden_proto::WardenResponse,
) -> Result<(), IpcError> {
    use rustix::net::SendFlags;

    let bytes = serde_json::to_vec(response).map_err(IpcError::ResponseEncode)?;
    rustix::net::send(connection, &bytes, SendFlags::empty()).map_err(|_| IpcError::Send)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::IpcError;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn test_oversized_message_error_is_content_free() {
        assert_eq!(
            IpcError::Oversized.to_string(),
            "received an ipc message larger than the receive buffer; it was discarded"
        );
    }

    #[test]
    fn test_malformed_error_wraps_and_links_the_inner_serde_error() -> TestResult {
        use std::error::Error as _;

        let Err(json_err) = serde_json::from_str::<serde_json::Value>("not json") else {
            return Err(TestError::Unexpected(
                "expected deliberately malformed JSON to fail parsing".into(),
            ));
        };
        let err: IpcError = json_err.into();
        assert!(matches!(err, IpcError::Malformed(_)));
        assert!(err.source().is_some());
        Ok(())
    }

    // `serve_forever`/`accept_connection`/`handle_connection` werden hier
    // bewusst nicht getestet: jede würde einen echten Socket annehmen oder
    // öffnen — nach Aufgabenstellung ausdrücklich untersagt. Die Anfrage-
    // Verarbeitung selbst (`Warden::handle` unverändert erreicht, inhalts-
    // freie Antwort) ist in `crate::warden_factory`s Tests direkt gegen den
    // produktiven Warden geprüft, ohne einen Socket zu benötigen. Das
    // Umschlag-Format ist in `crate::protocol`s Tests geprüft.
}
