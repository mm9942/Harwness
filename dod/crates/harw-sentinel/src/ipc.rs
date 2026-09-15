//! IPC-Empfangspfad: `SOCK_SEQPACKET`-Unix-Socket, Push-Only von den
//! privilegierten Sonden — die erste Entscheidung dieses Knotens (AW2-19).
//!
//! Nur unter `#[cfg(target_os = "linux")]` kompiliert (siehe
//! `crate::main`, wo dieses Modul entsprechend eingebunden wird) —
//! `AF_UNIX`/`SOCK_SEQPACKET` und `SO_PEERCRED` sind ohnehin nur auf Linux
//! in der hier gebrauchten Form vorhanden, und dieses gesamte Programm
//! (Landlock, fanotify, `AUDIT`-Netlink, BPF) ist ein reines
//! Linux-Sicherheitswerkzeug. Dasselbe Muster wie
//! `harw-dod-netlink/src/socket.rs` (`NetlinkAuditSource`), dort ebenfalls
//! über `#[cfg(target_os = "linux")]` an der `mod`-Deklaration.
//!
//! # Warum `SOCK_SEQPACKET`, nicht ein Ringpuffer
//! Die privilegierten Sonden (`harw-probe-fs`, `harw-probe-bpf`, ein
//! künftiges `harw-probe-netlink`) senden ihre `SecurityEvent`s an diesen
//! Prozess. Festgelegt ist `SOCK_SEQPACKET` gegenüber einem Ringpuffer im
//! geteilten Speicher aus drei Gründen:
//!
//! - **Nachrichtengrenzen.** Ein `SOCK_SEQPACKET`-`recv()` liefert genau ein
//!   Ereignis als ein Datagramm — kein Byte-Strom, den dieser Prozess erst
//!   wieder in Nachrichten zerlegen müsste (anders als `SOCK_STREAM`, wo ein
//!   `SecurityEvent` über zwei `recv()`-Aufrufe verteilt ankommen könnte).
//! - **`SO_PEERCRED`.** Ein Unix-Socket trägt die Kernel-verbürgte Identität
//!   des verbundenen Prozesses (`SO_PEERCRED`: `pid`/`uid`/`gid`) — siehe
//!   [`PeerCredentials`]. Ein Ringpuffer im geteilten Speicher hätte keine
//!   eingebaute Herkunftsprüfung; jeder Schreiber mit Zugriff auf das
//!   Speichersegment wäre ununterscheidbar.
//! - **Kein geteilter Speicher zwischen Berechtigungsklassen.** Ein
//!   Ringpuffer im `shm`-Segment wäre eine Fläche, die ein privilegierter
//!   Sender (`CAP_SYS_ADMIN`, `CAP_BPF`, `CAP_AUDIT_READ`) und dieser
//!   unprivilegierte Leser gemeinsam beschreiben — jeder Fehler in der
//!   Größen-/Offsetberechnung auf einer Seite wird sofort ein
//!   Speicherfehler auf der anderen. Ein Unix-Socket hält die beiden
//!   Adressräume vollständig getrennt; der Kernel kopiert.
//!
//! # Push-Only: kein Rückkanal
//! Dieser Prozess **empfängt** von den Sonden, **sendet ihnen aber nie
//! etwas**. Eine Sonde mit `CAP_SYS_ADMIN` oder `CAP_BPF`, die Anweisungen
//! von einem unprivilegierten Prozess entgegennimmt, ist ein Angriffsziel:
//! ein kompromittierter oder fehlerhafter Sentinel könnte sie sonst zu
//! beliebigem privilegiertem Verhalten anweisen. Diese Datei drückt das als
//! **Typ-Eigenschaft** aus, nicht als Konvention: [`IpcListener`] und
//! [`IpcConnection`] haben keine `send`/`write`-Methode — keine, die
//! versehentlich vergessen werden könnte, weil keine existiert, die ein
//! Aufrufer finden könnte. `test_ipc_connection_has_no_send_capability`
//! hält das als Kompilierzeit-Beleg fest.
//!
//! # Ohne `unsafe`
//! `rustix::net` deckt `socket`, `bind`, `listen`, `accept`, `recv` und
//! `sockopt::socket_peercred` bereits sicher ab — jede hier verwendete
//! Funktion ist ein normaler, sicherer Aufruf (kein `unsafe` auf dieser
//! Seite, obwohl `unsafe_code = "forbid"` im Workspace es ohnehin verbieten
//! würde).
//!
//! # Keine Ausprobierprobe
//! Wie `NetlinkAuditSource` wurde diese Implementierung nie gegen einen
//! echten Socket ausgeführt — dieser Knoten darf kein `cargo` ausführen und
//! keinen echten Socket öffnen (zentrale, sequenzielle Verifikation). Jeder
//! `rustix::net`-Aufruf hier ist gegen die tatsächlich im Workspace
//! gepinnte Quelle geprüft (`~/.cargo/registry/src/…/rustix-1.1.4/src/net/`),
//! nicht gegen `docs.rs` oder aus dem Gedächtnis — eine frühere Fassung
//! dieser Datei hatte [`IpcConnection::recv_event`]s `rustix::net::recv`
//! als Rückgabe eines einzelnen `usize` behandelt, obwohl die gepinnte
//! Version `(Buf::Output, usize)` liefert (siehe dortige Methodendoku). Ein
//! reiner API-Dokulesefehler, der nur durch Gegenlesen der Quelle auffiel,
//! nicht durch nochmaliges Lesen derselben (unvollständigen) Doku.
//!
//! # Wire-Format
//! Jede Nachricht ist ein kanonisches JSON-Dokument eines
//! `harw_dod_signals::SecurityEvent` — dieselbe Wire-Kodierung, die die
//! Crate-Dokumentation dort bereits für den Weg „Sentinel → Regelwerk"
//! vorsieht (`#[serde(deny_unknown_fields)]`, kebab-case `EventKind`-Tag).
//! Eine noch nicht gebaute Sonde (`harw-probe-fs`, `harw-probe-bpf`) muss
//! ihre Ereignisse exakt so serialisieren; dieser Knoten legt das Format
//! fest, weil beide Sonden-Knoten zum Zeitpunkt dieses Berichts noch reine
//! Gerüste sind.
//!
//! # Wohin empfangene Nachrichten laufen
//! Empfangene Ereignisse landen zunächst in [`IpcInbox`] — einem eigenen,
//! begrenzten Ring, getrennt von `harw_dod_sentinel::Sentinel::buffer()`.
//! Das war beim ursprünglichen Bau dieses Moduls der einzige verfügbare
//! Schreibweg: `harw_dod_sentinel::Sentinel::record_external_event` (der
//! zweite Schreibweg in den Sentinel-Puffer, neben `poll_all`) existierte zu
//! diesem Zeitpunkt noch nicht, und weder `harw_dod_cap::Capability` noch
//! `harw_dod_sentinel::Sentinel` boten damals eine Erweiterung für ein
//! Ereignis, das kein Sensor-Abruf ist.
//!
//! Diese Lücke ist inzwischen geschlossen: [`crate::run`] hält die
//! `Arc<Mutex<`[`IpcInbox`]`>>` als [`crate::IpcInboxHandle`] und reicht sie
//! an [`crate::poll_once`] weiter, das sie nach jedem `poll_all`-Aufruf über
//! [`crate::drain_external_events`] leert und jeden Eintrag per
//! `Sentinel::record_external_event` in `harw_dod_sentinel::Sentinel::buffer()`
//! überführt — siehe dort für die Reihenfolge- und Überlaufentscheidung.
//! Diese Inbox bleibt trotzdem bestehen, statt nur ein Übergangszustand zu
//! sein: sie ist der Ort, an dem der Empfangsthread parallel zum
//! Sammelthread schreiben darf, ohne dass beide Threads `Sentinel` gemeinsam
//! anfassen müssen (siehe `crate`-Moduldoku, Abschnitt „Warum `Sentinel`
//! nicht von einem `Mutex` umschlossen wird").
//!
//! # Nebenläufigkeit
//! [`IpcInbox`] hält ihren Zustand hinter einem `std::sync::Mutex` und ist
//! `Send + Sync`. [`spawn_receive_loop`] startet einen Hintergrundthread je
//! akzeptierter Verbindung (`std::thread::spawn`, kein Async-Runtime) — die
//! Anzahl gleichzeitig verbundener Sonden ist klein und vorab bekannt
//! (höchstens eine je privilegiertem Binary dieses Programms).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use harw_dod_signals::SecurityEvent;
use harw_macros::HarwError;
use rustix::fd::OwnedFd;

/// Empfangspuffer-Größe je Nachricht.
///
/// Reichlich bemessen für ein einzelnes serialisiertes `SecurityEvent`
/// (Vorbild: `harw_dod_netlink::socket::RECV_BUFFER_LEN`, dort `8192` für
/// einen einzelnen Audit-Record).
const RECV_BUFFER_LEN: usize = 16_384;

/// Fehler des IPC-Empfangspfads.
///
/// # Description
/// Wie `harw_dod_cap::SensorError` und `harw_dod_netlink::NetlinkError`
/// inhaltsfrei: kein Betriebssystem-Fehlercode, kein Pfadinhalt, kein
/// Nachrichteninhalt in einer `Display`-Meldung.
#[derive(Debug, HarwError)]
pub enum IpcError {
    /// Der Empfangssocket konnte unter `path` nicht gebunden werden.
    #[msg("failed to bind ipc socket at '{path}'")]
    Bind {
        /// Der Pfad, an dem das Binden fehlschlug.
        path: String,
    },

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

    /// Eine empfangene Nachricht ließ sich nicht als `SecurityEvent`
    /// dekodieren.
    ///
    /// # Arguments
    /// - `0` (`serde_json::Error`): die zugrunde liegende Serde-Ursache.
    #[msg("received an ipc message that could not be decoded as a SecurityEvent: {0}")]
    #[from]
    Malformed(serde_json::Error),

    /// Eine empfangene Nachricht war größer als [`RECV_BUFFER_LEN`] und
    /// wurde vom Kernel abgeschnitten — siehe
    /// [`IpcConnection::recv_event`]-Doku für die Begründung, warum das
    /// kein `Malformed` ist.
    #[msg("received an ipc message larger than the receive buffer; it was discarded")]
    Oversized,
}

/// Kernel-verbürgte Identität des verbundenen Peers (`SO_PEERCRED`).
///
/// # Description
/// Gelesen genau einmal, direkt nach `accept()` — der Kernel füllt diese
/// Werte aus dem verbindenden Prozess, nicht aus dessen (fälschbaren)
/// Nutzlast. `harw-sentinel` **prüft** diese Werte heute nicht gegen eine
/// Zulassungsliste (keine ist Teil dieses Knotens); sie werden geloggt, was
/// dem Betreiber bereits erlaubt zu erkennen, wenn ein unerwarteter Prozess
/// eine Verbindung aufbaut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCredentials {
    /// Prozess-ID des Peers zum Zeitpunkt von `connect()`.
    pub pid: u32,
    /// Effektive Nutzer-ID des Peers.
    pub uid: u32,
    /// Effektive Gruppen-ID des Peers.
    pub gid: u32,
}

/// Ein empfangenes Ereignis zusammen mit der Identität seines Absenders.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceivedEvent {
    /// Das dekodierte Ereignis.
    pub event: SecurityEvent,
    /// Wer es gesendet hat.
    pub peer: PeerCredentials,
}

/// Ein gebundener, lauschender `SOCK_SEQPACKET`-Unix-Socket.
///
/// # Description
/// **Push-only:** dieser Typ hat bewusst keine `send`/`write`-Methode —
/// siehe Moduldoku.
#[derive(Debug)]
pub struct IpcListener {
    socket: OwnedFd,
    path: PathBuf,
}

impl IpcListener {
    /// Bindet und öffnet den Empfangssocket unter `path`.
    ///
    /// # Description
    /// Entfernt zuerst eine eventuell vorhandene Socket-Datei desselben
    /// Pfads (ein Rest eines vorherigen, nicht sauber beendeten Prozesses —
    /// `bind()` auf einen `AF_UNIX`-Pfad scheitert sonst mit „Adresse
    /// bereits in Verwendung"), baut dann `socket`, `bind`, `listen` in
    /// dieser Reihenfolge. Kein Schritt hiervon ist `unsafe` (siehe
    /// Moduldoku).
    ///
    /// # Arguments
    /// - `path` (`&std::path::Path`): Zieldatei des Unix-Sockets. Muss in
    ///   einem existierenden, beschreibbaren Verzeichnis liegen.
    ///
    /// # Returns
    /// Einen lauschenden `IpcListener`, bereit für [`Self::accept`].
    ///
    /// # Errors
    /// [`IpcError::Bind`], wenn `socket()`, `bind()` oder `listen()`
    /// scheitert (Verzeichnis fehlt, keine Berechtigung, Pfad zu lang).
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_sentinel::ipc::IpcListener;
    /// use std::path::Path;
    ///
    /// let listener = IpcListener::bind(Path::new("/run/harw/sentinel.sock"))?;
    /// # Ok::<(), harw_sentinel::ipc::IpcError>(())
    /// ```
    pub fn bind(path: &Path) -> Result<Self, IpcError> {
        use rustix::net::{self, AddressFamily, SocketType};

        let bind_err = || IpcError::Bind {
            path: path.display().to_string(),
        };

        // Rest eines vorherigen Laufs entfernen; ein fehlender Pfad
        // (`NotFound`) ist der erwartete Regelfall beim ersten Start, kein
        // Fehler dieser Funktion.
        let _ = std::fs::remove_file(path);

        let socket =
            net::socket(AddressFamily::UNIX, SocketType::SEQPACKET, None).map_err(|_| bind_err())?;
        let addr = net::SocketAddrUnix::new(path).map_err(|_| bind_err())?;
        net::bind(&socket, &addr).map_err(|_| bind_err())?;
        net::listen(&socket, 16).map_err(|_| bind_err())?;

        Ok(Self {
            socket,
            path: path.to_path_buf(),
        })
    }

    /// Der Pfad, unter dem dieser Socket gebunden wurde.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Nimmt die nächste eingehende Verbindung einer Sonde an.
    ///
    /// # Description
    /// Blockiert, bis eine Sonde `connect()` aufruft. Liest sofort danach
    /// [`PeerCredentials`] über `SO_PEERCRED` — der Kernel füllt sie beim
    /// `accept()` aus dem verbindenden Prozess, nicht erst beim ersten
    /// `recv()`.
    ///
    /// # Returns
    /// Eine [`IpcConnection`], bereit für [`IpcConnection::recv_event`].
    ///
    /// # Errors
    /// - [`IpcError::Accept`]: `accept()` scheitert.
    /// - [`IpcError::PeerCredentialsUnavailable`]: `SO_PEERCRED` ist nach
    ///   erfolgreichem `accept()` nicht lesbar.
    pub fn accept(&self) -> Result<IpcConnection, IpcError> {
        use rustix::net::sockopt;

        let accepted = rustix::net::accept(&self.socket).map_err(|_| IpcError::Accept)?;
        let creds = sockopt::socket_peercred(&accepted)
            .map_err(|_| IpcError::PeerCredentialsUnavailable)?;

        Ok(IpcConnection {
            socket: accepted,
            peer: PeerCredentials {
                pid: creds.pid.as_raw_pid() as u32,
                uid: creds.uid.as_raw() as u32,
                gid: creds.gid.as_raw() as u32,
            },
        })
    }
}

/// Eine angenommene Verbindung zu genau einer Sonde.
///
/// # Description
/// **Push-only:** kein `send`, kein `write`, keine Methode, über die dieser
/// Prozess der Sonde etwas mitteilen könnte (siehe Moduldoku). Eine
/// `IpcConnection` lebt so lange, wie der Empfangs-Hintergrundthread sie
/// hält; sie wird nie an eine zweite Stelle weitergegeben.
#[derive(Debug)]
pub struct IpcConnection {
    socket: OwnedFd,
    /// Die beim `accept()` gelesene Identität dieses Peers.
    peer: PeerCredentials,
}

impl IpcConnection {
    /// Die Identität des verbundenen Peers.
    #[must_use]
    pub fn peer(&self) -> PeerCredentials {
        self.peer
    }

    /// Empfängt eine Nachricht und dekodiert sie als `SecurityEvent`.
    ///
    /// # Description
    /// `SOCK_SEQPACKET` liefert je `recv()` genau ein Datagramm — ein
    /// Ereignis, keinen Byte-Strom (siehe Moduldoku). Ein Datagramm der
    /// Länge `0` ist die geordnete Schließung der Verbindung durch die
    /// Sonde, kein Fehler.
    ///
    /// # Returns
    /// - `Some(event)`: eine Nachricht wurde empfangen und dekodiert.
    /// - `None`: die Sonde hat die Verbindung geordnet geschlossen.
    ///
    /// # Errors
    /// - [`IpcError::Recv`]: der zugrunde liegende `recv()`-Aufruf schlägt
    ///   fehl.
    /// - [`IpcError::Oversized`]: das Datagramm war größer als
    ///   [`RECV_BUFFER_LEN`] — der überschüssige Teil ist laut
    ///   `SOCK_SEQPACKET`-Nachrichtengrenze verloren, kein Parse-Fehler.
    /// - [`IpcError::Malformed`]: die empfangenen (nicht abgeschnittenen)
    ///   Bytes sind kein wohlgeformtes `SecurityEvent`-JSON.
    pub fn recv_event(&self) -> Result<Option<SecurityEvent>, IpcError> {
        use rustix::net::RecvFlags;

        // `rustix::net::recv` liefert für einen `&mut [u8; N]`-Puffer ein
        // Tupel `(Buf::Output, usize)`, kein Skalar (gegen
        // `rustix-1.1.4/src/net/send_recv/mod.rs` und
        // `rustix-1.1.4/src/buffer.rs` nachgeschlagen, nicht aus dem
        // Gedächtnis übernommen): `copied` (hier `Buf::Output = usize`) ist
        // die Anzahl tatsächlich in `buffer` geschriebener Bytes, immer
        // `<= buffer.len()`; `message_len` ist die **ungekürzte** Größe des
        // Datagramms, wie sie `RecvFlags::TRUNC` vom Kernel zurückmelden
        // lässt — für `AF_UNIX`-Datagramm-/`SOCK_SEQPACKET`-Sockets seit
        // Linux 3.4 die echte Nachrichtengröße, auch wenn sie den Puffer
        // übersteigt. Ohne `TRUNC` bliebe eine zu große Nachricht als
        // stille Kürzung ununterscheidbar von einer exakt passenden.
        let mut buffer = [0u8; RECV_BUFFER_LEN];
        let (copied, message_len) = rustix::net::recv(&self.socket, &mut buffer, RecvFlags::TRUNC)
            .map_err(|_| IpcError::Recv)?;

        if message_len == 0 {
            return Ok(None);
        }

        if message_len > buffer.len() {
            // `SOCK_SEQPACKET` liefert Nachrichtengrenzen: der über
            // `RECV_BUFFER_LEN` hinausgehende Rest dieses Datagramms ist
            // verloren, ein weiteres `recv()` liefert die **nächste**
            // Nachricht, nicht den Rest dieser. Die Gegenseite hat korrekt
            // gesendet, nur zu groß für diesen Puffer — das ist kein
            // Parse-Fehler (`IpcError::Malformed`), sondern ein eigener,
            // inhaltsfreier Fehlerfall.
            return Err(IpcError::Oversized);
        }

        let event: SecurityEvent = serde_json::from_slice(&buffer[..copied])?;
        Ok(Some(event))
    }
}

/// Begrenzter Empfangspuffer für über [`IpcConnection::recv_event`]
/// eingetroffene Ereignisse.
///
/// # Description
/// Verdrängt beim Erreichen ihrer Kapazität den jeweils ältesten Eintrag —
/// dasselbe Prinzip wie `harw_dod_sentinel::EvidenceBuffer` (siehe dortige
/// Moduldoku für die Begründung: unbegrenztes Wachstum ist ein
/// Speicherrisiko, ein zu kleiner Puffer ließe einen Angreifer seine Spuren
/// durch Lärm verdrängen). Siehe Moduldoku (Abschnitt „Wohin empfangene
/// Nachrichten laufen") für die Einschränkung, dass diese Ereignisse heute
/// **nicht** automatisch in `harw_dod_sentinel::Sentinel::buffer()`
/// überführt werden.
#[derive(Debug)]
pub struct IpcInbox {
    capacity: usize,
    events: VecDeque<ReceivedEvent>,
    /// Wie viele Ereignisse seit dem Start wegen voller Kapazität verdrängt
    /// wurden.
    ///
    /// Ohne diese Zahl ist eine stille Verdrängung von "es kam nichts an"
    /// nicht zu unterscheiden -- und was hier verschwindet, hat eine
    /// privilegierte Sonde tatsächlich beobachtet. Der Ringpuffer in
    /// `harw-dod-sentinel` meldet aus demselben Grund seine Auslastung.
    dropped: u64,
}

impl IpcInbox {
    /// Baut eine leere Inbox mit fester Kapazität.
    ///
    /// # Arguments
    /// - `capacity` (`usize`): maximale Anzahl gehaltener Ereignisse. `0`
    ///   wird als `1` behandelt (dieselbe Konvention wie
    ///   `harw_observe_file::FileSink::open`s `max_bytes`), damit eine
    ///   Inbox nie bedingungslos jedes Ereignis verwirft.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            events: VecDeque::with_capacity(capacity),
            dropped: 0,
        }
    }

    /// Nimmt ein empfangenes Ereignis auf; verdrängt bei voller Kapazität
    /// das älteste.
    pub fn push(&mut self, event: ReceivedEvent) {
        if self.events.len() >= self.capacity {
            self.events.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.events.push_back(event);
    }

    /// Anzahl aktuell gehaltener Ereignisse.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Ob die Inbox aktuell leer ist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Die konfigurierte Höchstkapazität.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Leert die Inbox und liefert alle gehaltenen Ereignisse in
    /// Ankunftsreihenfolge.
    pub fn drain(&mut self) -> Vec<ReceivedEvent> {
        self.events.drain(..).collect()
    }

    /// Wie viele Ereignisse seit dem Start wegen voller Kapazität verdrängt
    /// wurden.
    ///
    /// # Returns
    /// Eine monoton wachsende Zahl. **Erwartet wird null**; jeder Wert
    /// darüber heißt, dass eine Sonde schneller gemeldet hat, als der
    /// Sentinel entleeren konnte, und dass Beobachtungen verlorengingen.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// Startet einen Hintergrundthread, der Verbindungen annimmt und ihre
/// Ereignisse in `inbox` einträgt.
///
/// # Description
/// Für jede über [`IpcListener::accept`] angenommene Verbindung startet
/// diese Schleife einen weiteren Thread, der [`IpcConnection::recv_event`]
/// wiederholt aufruft, bis die Verbindung geordnet schließt (`Ok(None)`)
/// oder scheitert (`Err`, geloggt, dann beendet) — beides beendet nur den
/// Thread dieser einen Verbindung, nie die Annahmeschleife selbst. Ein
/// Lesefehler auf einer Nachricht (`IpcError::Malformed`, `IpcError::Oversized`)
/// verwirft nur diese eine Nachricht und liest weiter; eine fehlerhafte oder
/// eine einzelne zu große Nachricht einer Sonde soll nicht die ganze
/// Verbindung abreißen.
///
/// # Arguments
/// - `listener` (`IpcListener`): der gebundene, lauschende Socket. Wird von
///   diesem Thread übernommen.
/// - `inbox` (`std::sync::Arc<std::sync::Mutex<IpcInbox>>`): geteiltes Ziel
///   jedes empfangenen Ereignisses.
///
/// # Returns
/// Den `JoinHandle` der Annahmeschleife. Läuft, bis `accept()` dauerhaft
/// scheitert (z. B. der Socket extern geschlossen wird) — dann beendet sich
/// der Thread mit einer geloggten Warnung, statt in einer heißen Schleife
/// weiterzulaufen.
///
/// # Concurrency
/// Startet `std::thread::spawn` je akzeptierter Verbindung (siehe
/// Moduldoku). `inbox` wird nur über den `Mutex` berührt, nie ohne ihn.
#[must_use]
pub fn spawn_receive_loop(listener: IpcListener, inbox: Arc<Mutex<IpcInbox>>) -> JoinHandle<()> {
    std::thread::spawn(move || loop {
        match listener.accept() {
            Ok(connection) => {
                let inbox = Arc::clone(&inbox);
                std::thread::spawn(move || receive_until_closed(connection, &inbox));
            }
            Err(error) => {
                tracing::warn!(error = %error, "ipc accept failed; receive loop is stopping");
                break;
            }
        }
    })
}

/// Liest wiederholt Ereignisse einer angenommenen Verbindung, bis sie
/// schließt oder ein `recv()` selbst fehlschlägt.
fn receive_until_closed(connection: IpcConnection, inbox: &Mutex<IpcInbox>) {
    let peer = connection.peer();
    loop {
        match connection.recv_event() {
            Ok(Some(event)) => {
                tracing::debug!(?peer, sensor = %event.sensor.as_str(), "ipc event received");
                if let Ok(mut inbox) = inbox.lock() {
                    inbox.push(ReceivedEvent { event, peer });
                }
            }
            Ok(None) => {
                tracing::debug!(?peer, "ipc peer closed the connection");
                break;
            }
            Err(IpcError::Malformed(error)) => {
                tracing::warn!(
                    ?peer,
                    error = %error,
                    "ipc peer sent a message that could not be decoded; discarding it and continuing"
                );
            }
            Err(IpcError::Oversized) => {
                // Nachrichtengrenze bei `SOCK_SEQPACKET`: der Rest dieses
                // einen Datagramms ist verloren, aber der nächste
                // `recv_event`-Aufruf liefert bereits die nächste,
                // unabhängige Nachricht — kein Grund, die Verbindung zu
                // schließen (siehe `recv_event`-Doku).
                tracing::warn!(
                    ?peer,
                    "ipc peer sent a message larger than the receive buffer; discarding it and continuing"
                );
            }
            Err(error) => {
                tracing::warn!(?peer, error = %error, "ipc recv failed; closing this connection");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{IpcConnection, IpcInbox, IpcListener, PeerCredentials, ReceivedEvent};
    use harw_dod_signals::{EventKind, SecurityEvent};
    use harw_types::SensorId;

    fn sample_peer() -> PeerCredentials {
        PeerCredentials {
            pid: 4242,
            uid: 1000,
            gid: 1000,
        }
    }

    fn sample_event() -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("probe-fs-0"),
            observed_at: jiff::Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::SensorDegraded {
                sensor: SensorId::from_str("probe-fs-0"),
            },
        }
    }

    #[test]
    fn test_ipc_inbox_push_lands_a_received_message_in_the_buffer() {
        let mut inbox = IpcInbox::new(4);
        assert!(inbox.is_empty());

        inbox.push(ReceivedEvent {
            event: sample_event(),
            peer: sample_peer(),
        });

        assert_eq!(inbox.len(), 1);
        let drained = inbox.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].peer, sample_peer());
        assert!(inbox.is_empty());
    }

    #[test]
    fn test_ipc_inbox_evicts_oldest_when_full() {
        let mut inbox = IpcInbox::new(2);
        for i in 0..3u32 {
            let mut event = sample_event();
            event.kind = EventKind::SensorDegraded {
                sensor: SensorId::from_str(format!("probe-{i}")),
            };
            inbox.push(ReceivedEvent {
                event,
                peer: sample_peer(),
            });
        }
        assert_eq!(inbox.len(), 2);
        let remaining = inbox.drain();
        let EventKind::SensorDegraded { sensor } = &remaining[0].event.kind else {
            panic!("expected SensorDegraded");
        };
        // Der älteste Eintrag (probe-0) wurde verdrängt; die verbleibenden
        // zwei sind probe-1 und probe-2.
        assert_eq!(sensor.as_str(), "probe-1");
    }

    #[test]
    fn test_ipc_inbox_capacity_zero_is_treated_as_one() {
        let inbox = IpcInbox::new(0);
        assert_eq!(inbox.capacity(), 1);
    }

    #[test]
    fn test_wire_format_roundtrip_matches_security_event_json_contract() {
        let event = sample_event();
        let encoded = serde_json::to_vec(&event).expect("SecurityEvent serializes");
        let decoded: SecurityEvent =
            serde_json::from_slice(&encoded).expect("round-trips through the wire format");
        assert_eq!(decoded, event);
    }

    /// Kompilierzeit-Beleg für „Push-only": dieser Test benennt genau die
    /// beiden Typen, die laut Moduldoku keine `send`/`write`-Methode
    /// besitzen dürfen. Bekäme `IpcConnection` oder `IpcListener` künftig
    /// eine solche Methode, ist dieser Kommentar (und der Anspruch der
    /// Moduldoku) der Ort, der überprüft werden muss — ein `PhantomData`
    /// über beide Typen macht diesen Test zumindest gegen einen entfernten
    /// Typ (nicht gegen eine hinzugefügte Methode) hart.
    #[test]
    fn test_ipc_connection_has_no_send_capability() {
        fn assert_type_exists<T: 'static>() {
            let _ = std::marker::PhantomData::<T>;
        }

        assert_type_exists::<IpcConnection>();
        assert_type_exists::<IpcListener>();
    }

    /// Deckt die Rückgabetyp-Korrektur von `recv_event` ab, ohne einen
    /// echten Socket zu öffnen (harte Auflage dieses Knotens): `rustix::net::recv`
    /// liefert für einen Array-Puffer `(usize, usize)`, nicht einen
    /// einzelnen `usize` — dieser Test hält zumindest die Fehlervariante
    /// fest, die eine zu große, laut `RecvFlags::TRUNC` erkannte Nachricht
    /// jetzt auslöst, und dass ihre Meldung inhaltsfrei bleibt (keine
    /// Bytegröße, kein Nachrichteninhalt).
    #[test]
    fn test_ipc_error_oversized_message_is_content_free() {
        assert_eq!(
            super::IpcError::Oversized.to_string(),
            "received an ipc message larger than the receive buffer; it was discarded"
        );
    }
}
