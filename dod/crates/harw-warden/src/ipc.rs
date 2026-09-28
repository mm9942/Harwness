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
//! `rustix::net` deckt `accept`, `sockopt::socket_peercred`,
//! `sockopt::set_socket_timeout`, `recv` und `send` bereits sicher ab. Jeder
//! hier verwendete Aufruf ist gegen die
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
//! Verbindung (Muster: `harw-sentinel::ipc::spawn_receive_loop`), aber
//! **nicht unbegrenzt**: [`accept_connection`] setzt ein Empfangs-Timeout
//! (`SO_RCVTIMEO`, [`RECV_TIMEOUT`]), und [`serve_forever`] reserviert vor
//! jedem Thread-Start einen Platz in einem geteilten Zähler
//! ([`try_acquire_slot`], Obergrenze [`MAX_CONCURRENT_CONNECTIONS`]) — ist
//! keiner mehr frei, schließt es die neu angenommene Verbindung sofort
//! wieder. Beides zusammen begrenzt, was ein Peer anrichten kann, der
//! verbindet und nie sendet. `warden` wird als `Arc<Warden>` geteilt;
//! `Warden` selbst trägt keinen veränderlichen inneren Zustand außerhalb
//! seiner Ausführer/seines Audit-Ziels (siehe
//! `harw_dod_warden::warden`-Moduldoku), die ihrerseits
//! entweder zustandslos sind (`CgroupV2Executor`,
//! `crate::isolation::NftNetworkIsolator`,
//! `crate::audit::TracingAuditSink`) oder — in Produktion nicht verwendet —
//! ihre eigene Synchronisation mitbringen (`RecordingExecutor`,
//! `RecordingAuditSink`, nur in Tests der Bibliothek). Gleichzeitige
//! Anfragen sind deshalb ohne zusätzliches Locking sicher.

use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use harw_dod_warden::Warden;
use harw_macros::HarwError;

use crate::protocol::WardenRequestEnvelope;

/// Empfangspuffer-Größe je Nachricht. Reichlich bemessen für ein einzelnes
/// serialisiertes [`WardenRequestEnvelope`] (Vorbild:
/// `harw-sentinel::ipc::RECV_BUFFER_LEN`, dort `16_384` für ein einzelnes
/// `SecurityEvent`).
const RECV_BUFFER_LEN: usize = 16_384;

/// Empfangs-Timeout je angenommener Verbindung (`SO_RCVTIMEO`, gesetzt in
/// [`accept_connection`]). Verhindert, dass ein Peer, der verbindet und nie
/// sendet, seinen Bedienungs-Thread für immer in `recv()` blockiert (siehe
/// Moduldoku, Abschnitt „Nebenläufigkeit").
const RECV_TIMEOUT: Duration = Duration::from_secs(5);

/// Obergrenze gleichzeitig bedienter Verbindungen (siehe
/// [`try_acquire_slot`]). Über dieser Grenze schließt [`serve_forever`]
/// eine neu angenommene Verbindung sofort wieder, statt einen weiteren
/// Thread zu starten.
const MAX_CONCURRENT_CONNECTIONS: usize = 8;

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
    /// Für eine angenommene Verbindung ließ sich kein Empfangs-Timeout
    /// (`SO_RCVTIMEO`) setzen.
    #[msg("failed to set a receive timeout on an ipc connection")]
    Timeout,
    /// Ein Lesevorgang auf einer angenommenen Verbindung ist fehlgeschlagen
    /// — eingeschlossen das Ablaufen des in [`accept_connection`] gesetzten
    /// Empfangs-Timeouts ([`RECV_TIMEOUT`]).
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

/// RAII-Platzhalter im Verbindungszähler von [`serve_forever`]: reserviert
/// beim Erzeugen (über [`try_acquire_slot`]) einen Platz, gibt ihn beim
/// `Drop` wieder frei.
///
/// # Description
/// Trägt einen eigenen [`Arc`]-Klon des Zählers statt einer Referenz: Der
/// Verbindungs-Thread, in den dieser Platzhalter hineinbewegt wird, braucht
/// wegen `std::thread::spawn`s `'static`-Grenze eine vom Aufrufer-Stack
/// unabhängige Kopie — eine Referenz auf eine in [`serve_forever`] lokale
/// Variable wäre das nicht mehr, sobald diese Funktion nach einem
/// endgültigen `accept()`-Fehler zurückkehrt, während bediente Threads noch
/// liefen.
struct ConnectionSlot {
    active: Arc<AtomicUsize>,
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Versucht, im Zähler `active` einen Platz unterhalb von `max` zu
/// reservieren.
///
/// # Description
/// Reines Zählen, keine E/A, kein Warten: `fetch_update` erhöht `active`
/// genau dann um 1, wenn der zuvor gelesene Wert kleiner als `max` war —
/// atomar, keine Race zwischen zwei Threads, die beide denselben alten Wert
/// unterhalb von `max` sehen und beide gleichzeitig erhöhen wollen.
///
/// # Arguments
/// - `active`: der geteilte Verbindungszähler.
/// - `max`: die Obergrenze, ab der kein weiterer Platz vergeben wird.
///
/// # Returns
/// - `Some(slot)`: ein Platz war frei; `active` wurde um 1 erhöht. Der
///   Platz wird wieder frei, sobald `slot` fällt (`Drop`).
/// - `None`: `active` war bereits `>= max`; unverändert, nichts reserviert.
fn try_acquire_slot(active: &Arc<AtomicUsize>, max: usize) -> Option<ConnectionSlot> {
    active
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            if current < max { Some(current + 1) } else { None }
        })
        .ok()
        .map(|_| ConnectionSlot {
            active: Arc::clone(active),
        })
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
/// Startet `std::thread::spawn` je angenommener Verbindung, aber höchstens
/// [`MAX_CONCURRENT_CONNECTIONS`] gleichzeitig ([`try_acquire_slot`]); über
/// dieser Grenze wird eine neu angenommene Verbindung sofort wieder
/// geschlossen, ohne einen weiteren Thread zu starten. Siehe Moduldoku,
/// Abschnitt „Nebenläufigkeit".
pub fn serve_forever(listener: OwnedFd, warden: Arc<Warden>) {
    let active_connections = Arc::new(AtomicUsize::new(0));
    loop {
        match accept_connection(&listener) {
            Ok((connection, peer)) => {
                match try_acquire_slot(&active_connections, MAX_CONCURRENT_CONNECTIONS) {
                    Some(slot) => {
                        let warden = Arc::clone(&warden);
                        std::thread::spawn(move || {
                            let _slot = slot;
                            handle_connection(connection, peer, &warden);
                        });
                    }
                    None => {
                        tracing::warn!(
                            ?peer,
                            max = MAX_CONCURRENT_CONNECTIONS,
                            "rejecting ipc connection: already at the concurrent connection limit"
                        );
                        // `connection` fällt hier ohne Antwort aus dem
                        // Gültigkeitsbereich — schließt den angenommenen
                        // Socket, ohne einen weiteren Thread zu starten.
                    }
                }
            }
            Err(error) => {
                tracing::error!(error = %error, "ipc accept failed; ceasing to serve");
                break;
            }
        }
    }
}

/// Nimmt genau eine Verbindung an, setzt sofort ihr Empfangs-Timeout
/// (`SO_RCVTIMEO`, [`RECV_TIMEOUT`], siehe Moduldoku „Nebenläufigkeit") und
/// liest ihre `SO_PEERCRED`-Identität.
///
/// # Errors
/// - [`IpcError::Accept`]: `accept()` selbst ist fehlgeschlagen.
/// - [`IpcError::Timeout`]: das Empfangs-Timeout ließ sich nicht setzen.
/// - [`IpcError::PeerCredentialsUnavailable`]: `SO_PEERCRED` ließ sich nicht
///   lesen.
fn accept_connection(listener: &OwnedFd) -> Result<(OwnedFd, PeerCredentials), IpcError> {
    use rustix::net::sockopt;

    let accepted = rustix::net::accept(listener).map_err(|_| IpcError::Accept)?;
    sockopt::set_socket_timeout(&accepted, sockopt::Timeout::Recv, Some(RECV_TIMEOUT))
        .map_err(|_| IpcError::Timeout)?;
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
/// - [`IpcError::Recv`]: der zugrunde liegende `recv()`-Aufruf schlägt fehl
///   — eingeschlossen das Ablaufen des in [`accept_connection`] gesetzten
///   Empfangs-Timeouts ([`RECV_TIMEOUT`]): der Peer hat verbunden, aber
///   innerhalb der Frist nichts gesendet.
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
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    use super::{IpcError, try_acquire_slot};
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn test_oversized_message_error_is_content_free() {
        assert_eq!(
            IpcError::Oversized.to_string(),
            "received an ipc message larger than the receive buffer; it was discarded"
        );
    }

    #[test]
    fn test_timeout_error_is_content_free() {
        assert_eq!(
            IpcError::Timeout.to_string(),
            "failed to set a receive timeout on an ipc connection"
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

    #[test]
    fn test_try_acquire_slot_refuses_at_the_cap_and_releases_on_drop() -> TestResult {
        let active = Arc::new(AtomicUsize::new(0));

        let Some(first) = try_acquire_slot(&active, 2) else {
            return Err(TestError::Missing("first of two slots"));
        };
        let Some(second) = try_acquire_slot(&active, 2) else {
            return Err(TestError::Missing("second of two slots"));
        };
        assert!(
            try_acquire_slot(&active, 2).is_none(),
            "a third slot must be refused once the cap of 2 is reached"
        );

        drop(first);
        let Some(third) = try_acquire_slot(&active, 2) else {
            return Err(TestError::Unexpected(
                "dropping a reserved slot must free it for reuse".into(),
            ));
        };

        drop(second);
        drop(third);
        assert_eq!(
            active.load(std::sync::atomic::Ordering::Acquire),
            0,
            "every reserved slot must have released its count after being dropped"
        );
        Ok(())
    }

    // `serve_forever`/`accept_connection`/`handle_connection` werden hier
    // bewusst nicht getestet: jede würde einen echten Socket annehmen oder
    // öffnen — nach Aufgabenstellung ausdrücklich untersagt. Die Anfrage-
    // Verarbeitung selbst (`Warden::handle` unverändert erreicht, inhalts-
    // freie Antwort) ist in `crate::warden_factory`s Tests direkt gegen den
    // produktiven Warden geprüft, ohne einen Socket zu benötigen. Das
    // Umschlag-Format ist in `crate::protocol`s Tests geprüft. Der
    // Verbindungszähler, den `serve_forever` zur Begrenzung gleichzeitiger
    // Threads nutzt (`try_acquire_slot`/`ConnectionSlot`), braucht dagegen
    // keinen Socket — reines Zählen auf einem `AtomicUsize` — und wird
    // deshalb oben direkt getestet.
}
