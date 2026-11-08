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
//! # Zulassung per `SO_PEERCRED` (PL-100 WP0.5)
//! Der Socket ist für jedes Mitglied von `harw-ipc` erreichbar; die
//! Dateirechte allein trennen also nicht zwischen einer Sonde und jedem
//! anderen Prozess dieser Gruppe. [`spawn_receive_loop`] prüft deshalb jede
//! angenommene Verbindung gegen eine [`crate::peer_allow::PeerAllowlist`]
//! (die konfigurierten Sonden-Konten plus uid `0`), **bevor** auch nur eine
//! Nachricht gelesen wird ([`admit_connection`]). Ein anderer Peer wird
//! ohne Antwort geschlossen — Push-only gilt auch hier —, in
//! [`IpcInbox::rejected_total`] gezählt (Gesundheitsexport
//! `ipc.peers_rejected_total`) und nur auf `debug` geloggt, damit ein
//! lokaler Angreifer mit Verbindungsversuchen nicht das Journal flutet.
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

use crate::peer_allow::PeerAllowlist;

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
/// Nutzlast. [`admit_connection`] prüft `uid` gegen die
/// [`PeerAllowlist`] (PL-100 WP0.5), bevor eine Nachricht gelesen wird.
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

        let socket = net::socket(AddressFamily::UNIX, SocketType::SEQPACKET, None)
            .map_err(|_| bind_err())?;
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
    /// Total number of events pushed into this inbox since start, including
    /// the ones later evicted (see [`Self::dropped`]).
    received: u64,
    /// Number of probe connections currently open (accepted and not yet
    /// closed), maintained by the receive loop.
    connected: u64,
    /// Verbindungen, die seit dem Start wegen einer uid außerhalb der
    /// [`PeerAllowlist`] geschlossen wurden (PL-100 WP0.5).
    rejected: u64,
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
            received: 0,
            connected: 0,
            rejected: 0,
        }
    }

    /// Nimmt ein empfangenes Ereignis auf; verdrängt bei voller Kapazität
    /// das älteste.
    pub fn push(&mut self, event: ReceivedEvent) {
        self.received = self.received.saturating_add(1);
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

    /// Total number of events received since start (every [`Self::push`],
    /// including events that were later evicted).
    ///
    /// # Returns
    /// A monotonically increasing count; the `ipc.events_total` field of the
    /// health export (`crate::health_export`).
    #[must_use]
    pub fn received_total(&self) -> u64 {
        self.received
    }

    /// Number of probe connections that are currently open.
    ///
    /// # Returns
    /// The `ipc.probes_connected` field of the health export
    /// (`crate::health_export`).
    #[must_use]
    pub fn connected(&self) -> u64 {
        self.connected
    }

    /// Records that the receive loop accepted a probe connection.
    pub fn peer_connected(&mut self) {
        self.connected = self.connected.saturating_add(1);
    }

    /// Records that a previously accepted probe connection closed.
    pub fn peer_disconnected(&mut self) {
        self.connected = self.connected.saturating_sub(1);
    }

    /// Hält fest, dass eine Verbindung wegen ihrer uid abgewiesen wurde
    /// (siehe [`admit_connection`]).
    pub fn peer_rejected(&mut self) {
        self.rejected = self.rejected.saturating_add(1);
    }

    /// Anzahl der seit dem Start abgewiesenen Verbindungen.
    ///
    /// # Returns
    /// Eine monoton wachsende Zahl; das Feld `ipc.peers_rejected_total` des
    /// Gesundheitsexports (`crate::health_export`). **Erwartet wird null**;
    /// jeder Wert darüber heißt, dass ein Prozess außerhalb der
    /// Sonden-Konten Ereignisse einzuspeisen versucht hat.
    #[must_use]
    pub fn rejected_total(&self) -> u64 {
        self.rejected
    }
}

/// Prüft eine angenommene Verbindung gegen die Zulassungsliste.
///
/// # Description
/// Läuft direkt nach [`IpcListener::accept`] und **vor** dem ersten
/// [`IpcConnection::recv_event`]. Ist die `SO_PEERCRED`-uid des Peers nicht
/// zugelassen, wird die Verbindung geschlossen (Drop des Deskriptors, keine
/// Antwort an den Peer), [`IpcInbox::peer_rejected`] gezählt und nur auf
/// `debug` geloggt (uid und pid, kein Inhalt).
///
/// # Arguments
/// - `connection` (`IpcConnection`): die frisch angenommene Verbindung.
/// - `allowlist` (`&PeerAllowlist`): die beim Start aufgelöste Liste.
/// - `inbox` (`&std::sync::Mutex<IpcInbox>`): Ziel des Abweisungszählers.
///
/// # Returns
/// `Some(connection)` für einen zugelassenen Peer, sonst `None` (die
/// Verbindung ist dann bereits geschlossen).
///
/// # Concurrency
/// Sperrt `inbox` nur für das Hochzählen.
pub fn admit_connection(
    connection: IpcConnection,
    allowlist: &PeerAllowlist,
    inbox: &Mutex<IpcInbox>,
) -> Option<IpcConnection> {
    let peer = connection.peer();
    if allowlist.permits(peer.uid) {
        return Some(connection);
    }
    match inbox.lock() {
        Ok(mut inbox) => inbox.peer_rejected(),
        Err(poisoned) => poisoned.into_inner().peer_rejected(),
    }
    tracing::debug!(
        peer_uid = peer.uid,
        peer_pid = peer.pid,
        "ipc peer is not on the probe allowlist; connection closed without reading"
    );
    drop(connection);
    None
}

/// Startet einen Hintergrundthread, der Verbindungen annimmt und ihre
/// Ereignisse in `inbox` einträgt.
///
/// # Description
/// Jede über [`IpcListener::accept`] angenommene Verbindung läuft zuerst
/// durch [`admit_connection`]: ein Peer außerhalb von `allowlist` wird
/// geschlossen und gezählt, ohne dass eine Nachricht gelesen wird, und die
/// Annahmeschleife läuft weiter. Für jede zugelassene Verbindung startet
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
/// - `allowlist` (`PeerAllowlist`): die beim Start aufgelösten
///   Sonden-uids plus uid `0` (PL-100 WP0.5). Wird von diesem Thread
///   übernommen.
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
pub fn spawn_receive_loop(
    listener: IpcListener,
    inbox: Arc<Mutex<IpcInbox>>,
    allowlist: PeerAllowlist,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok(connection) => {
                    let Some(connection) = admit_connection(connection, &allowlist, &inbox) else {
                        continue;
                    };
                    let inbox = Arc::clone(&inbox);
                    std::thread::spawn(move || receive_until_closed(connection, &inbox));
                }
                Err(error) => {
                    tracing::warn!(error = %error, "ipc accept failed; receive loop is stopping");
                    break;
                }
            }
        }
    })
}

/// Liest wiederholt Ereignisse einer angenommenen Verbindung, bis sie
/// schließt oder ein `recv()` selbst fehlschlägt.
fn receive_until_closed(connection: IpcConnection, inbox: &Mutex<IpcInbox>) {
    let peer = connection.peer();
    // Verbindungszähler für den Gesundheitsexport (`ipc.probes_connected`).
    if let Ok(mut inbox) = inbox.lock() {
        inbox.peer_connected();
    }
    receive_events(&connection, peer, inbox);
    if let Ok(mut inbox) = inbox.lock() {
        inbox.peer_disconnected();
    }
}

/// Die eigentliche Empfangsschleife einer Verbindung (siehe
/// [`receive_until_closed`]).
fn receive_events(connection: &IpcConnection, peer: PeerCredentials, inbox: &Mutex<IpcInbox>) {
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
    use std::path::Path;
    use std::sync::Mutex;

    use super::{
        IpcConnection, IpcInbox, IpcListener, PeerCredentials, ReceivedEvent, admit_connection,
    };
    use crate::peer_allow::PeerAllowlist;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_signals::{EventKind, SecurityEvent};
    use harw_types::SensorId;
    use rustix::fd::OwnedFd;
    use rustix::net::{
        AddressFamily, RecvFlags, SendFlags, SocketAddrUnix, SocketFlags, SocketType,
    };

    /// Eine Verbindung über ein `socketpair` mit vorgegebener Peer-uid —
    /// für die reine Zulassungsentscheidung ohne lauschenden Socket. Die
    /// zweite Hälfte ist die Seite des (vorgetäuschten) Peers.
    fn connection_with_peer(uid: u32) -> TestResult<(IpcConnection, OwnedFd)> {
        let (ours, theirs) = rustix::net::socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )
        .map_err(ctx("socketpair"))?;
        let connection = IpcConnection {
            socket: ours,
            peer: PeerCredentials {
                pid: 4242,
                uid,
                gid: uid,
            },
        };
        Ok((connection, theirs))
    }

    /// Ob die Peer-Seite ein Schließen ohne jede Nachricht sieht: `recv`
    /// liefert `0` Bytes oder `ECONNRESET` (so meldet der Kernel das
    /// Schließen, wenn beim Gegenüber noch ungelesene Daten lagen,
    /// `unix_release_sock`). Ein noch offener Socket ergäbe `EAGAIN`.
    fn peer_sees_close_without_reply(peer: &OwnedFd) -> TestResult<bool> {
        let mut buffer = [0u8; 64];
        match rustix::net::recv(peer, &mut buffer, RecvFlags::DONTWAIT) {
            Ok((copied, message_len)) => Ok(copied == 0 && message_len == 0),
            Err(error) if error == rustix::io::Errno::CONNRESET => Ok(true),
            Err(error) => Err(TestError::Unexpected(format!(
                "peer recv after close failed: {error}"
            ))),
        }
    }

    fn rejected(inbox: &Mutex<IpcInbox>) -> TestResult<u64> {
        Ok(inbox.lock().map_err(ctx("inbox lock"))?.rejected_total())
    }

    /// Bindet einen echten Empfangssocket unter `/tmp` (kurzer Pfad, weit
    /// unter der `sun_path`-Grenze von 108 Bytes).
    fn bind_in_tempdir() -> TestResult<(tempfile::TempDir, IpcListener)> {
        let dir = tempfile::Builder::new()
            .prefix("hs-ipc")
            .tempdir_in("/tmp")
            .map_err(ctx("tempdir"))?;
        let listener = IpcListener::bind(&dir.path().join("s.sock")).map_err(ctx("bind"))?;
        Ok((dir, listener))
    }

    /// Verbindet sich als Sonde und sendet genau eine Nachricht.
    fn connect_and_send(path: &Path, payload: &[u8]) -> TestResult<OwnedFd> {
        let client = rustix::net::socket(AddressFamily::UNIX, SocketType::SEQPACKET, None)
            .map_err(ctx("client socket"))?;
        let address = SocketAddrUnix::new(path).map_err(ctx("client address"))?;
        rustix::net::connect(&client, &address).map_err(ctx("connect"))?;
        rustix::net::send(&client, payload, SendFlags::empty()).map_err(ctx("send"))?;
        Ok(client)
    }

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
    fn test_ipc_inbox_evicts_oldest_when_full() -> TestResult {
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
            return Err(TestError::Unexpected(format!(
                "expected SensorDegraded, got {:?}",
                remaining[0].event.kind
            )));
        };
        // Der älteste Eintrag (probe-0) wurde verdrängt; die verbleibenden
        // zwei sind probe-1 und probe-2.
        assert_eq!(sensor.as_str(), "probe-1");
        Ok(())
    }

    #[test]
    fn test_ipc_inbox_counts_received_events_including_evicted_ones() {
        let mut inbox = IpcInbox::new(1);
        for _ in 0..3 {
            inbox.push(ReceivedEvent {
                event: sample_event(),
                peer: sample_peer(),
            });
        }
        assert_eq!(inbox.received_total(), 3);
        assert_eq!(inbox.dropped(), 2);
        let _ = inbox.drain();
        assert_eq!(inbox.received_total(), 3, "draining keeps the total");
    }

    #[test]
    fn test_ipc_inbox_connection_counter_never_underflows() {
        let mut inbox = IpcInbox::new(1);
        inbox.peer_connected();
        inbox.peer_connected();
        assert_eq!(inbox.connected(), 2);
        inbox.peer_disconnected();
        inbox.peer_disconnected();
        inbox.peer_disconnected();
        assert_eq!(inbox.connected(), 0);
    }

    #[test]
    fn test_ipc_inbox_capacity_zero_is_treated_as_one() {
        let inbox = IpcInbox::new(0);
        assert_eq!(inbox.capacity(), 1);
    }

    #[test]
    fn test_ipc_inbox_rejection_counter_starts_at_zero_and_counts_each_rejection() {
        let mut inbox = IpcInbox::new(1);
        assert_eq!(inbox.rejected_total(), 0);
        inbox.peer_rejected();
        inbox.peer_rejected();
        assert_eq!(inbox.rejected_total(), 2);
        assert_eq!(inbox.connected(), 0, "a rejection is no connection");
    }

    /// PL-100 WP0.5: ein Peer mit falscher uid wird abgewiesen, gezählt und
    /// ohne Antwort geschlossen.
    #[test]
    fn test_admit_connection_rejects_a_peer_outside_the_allowlist() -> TestResult {
        let (connection, peer) = connection_with_peer(1000)?;
        let inbox = Mutex::new(IpcInbox::new(4));
        let allowlist = PeerAllowlist::with_root([991, 992]);

        let admitted = admit_connection(connection, &allowlist, &inbox);

        assert!(admitted.is_none(), "uid 1000 is no probe account");
        assert_eq!(rejected(&inbox)?, 1);
        assert!(
            peer_sees_close_without_reply(&peer)?,
            "the rejected peer sees an orderly close, never a reply"
        );
        Ok(())
    }

    #[test]
    fn test_admit_connection_passes_a_listed_probe_uid_through() -> TestResult {
        let (connection, _peer) = connection_with_peer(991)?;
        let inbox = Mutex::new(IpcInbox::new(4));
        let allowlist = PeerAllowlist::with_root([991, 992]);

        let admitted = admit_connection(connection, &allowlist, &inbox)
            .ok_or(TestError::Missing("listed probe uid is admitted"))?;

        assert_eq!(admitted.peer().uid, 991);
        assert_eq!(rejected(&inbox)?, 0);
        Ok(())
    }

    #[test]
    fn test_admit_connection_always_admits_root() -> TestResult {
        let (connection, _peer) = connection_with_peer(0)?;
        let inbox = Mutex::new(IpcInbox::new(4));
        let allowlist = PeerAllowlist::with_root(std::iter::empty());

        assert!(admit_connection(connection, &allowlist, &inbox).is_some());
        assert_eq!(rejected(&inbox)?, 0);
        Ok(())
    }

    /// Echter Socket: `bind` → `connect` → `accept` → `SO_PEERCRED` →
    /// Zulassung → `recv_event`. Die zugelassene uid ist die vom Kernel
    /// gemeldete des Testprozesses selbst.
    #[test]
    fn test_real_socket_admits_a_listed_peer_and_reads_its_event() -> TestResult {
        let (_dir, listener) = bind_in_tempdir()?;
        let payload = serde_json::to_vec(&sample_event()).map_err(ctx("encode event"))?;
        let _client = connect_and_send(listener.path(), &payload)?;

        let connection = listener.accept().map_err(ctx("accept"))?;
        let own_uid = connection.peer().uid;
        let inbox = Mutex::new(IpcInbox::new(4));
        let connection = admit_connection(connection, &PeerAllowlist::with_root([own_uid]), &inbox)
            .ok_or(TestError::Missing("the listed peer is admitted"))?;

        let event = connection
            .recv_event()
            .map_err(ctx("recv event"))?
            .ok_or(TestError::Missing("one event was sent"))?;
        assert_eq!(event, sample_event());
        assert_eq!(rejected(&inbox)?, 0);
        Ok(())
    }

    /// Echter Socket: ein Peer außerhalb der Liste wird nach `accept`
    /// geschlossen, bevor seine bereits gesendete Nachricht gelesen wird.
    /// `exactly` statt `with_root`, damit der Test auch als root abweist.
    #[test]
    fn test_real_socket_rejects_an_unlisted_peer_before_reading() -> TestResult {
        let (_dir, listener) = bind_in_tempdir()?;
        let payload = serde_json::to_vec(&sample_event()).map_err(ctx("encode event"))?;
        let client = connect_and_send(listener.path(), &payload)?;

        let connection = listener.accept().map_err(ctx("accept"))?;
        let own_uid = connection.peer().uid;
        let inbox = Mutex::new(IpcInbox::new(4));
        let allowlist = PeerAllowlist::exactly([own_uid.wrapping_add(1)]);

        assert!(admit_connection(connection, &allowlist, &inbox).is_none());
        assert_eq!(rejected(&inbox)?, 1);
        let held = inbox.lock().map_err(ctx("inbox lock"))?.len();
        assert_eq!(held, 0, "nothing of the rejected peer reaches the inbox");
        assert!(peer_sees_close_without_reply(&client)?);
        Ok(())
    }

    #[test]
    fn test_wire_format_roundtrip_matches_security_event_json_contract() -> TestResult {
        let event = sample_event();
        let encoded = serde_json::to_vec(&event).map_err(ctx("SecurityEvent serializes"))?;
        let decoded: SecurityEvent =
            serde_json::from_slice(&encoded).map_err(ctx("round-trips through the wire format"))?;
        assert_eq!(decoded, event);
        Ok(())
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
