//! SOCKS5-Egress-Proxy (RFC 1928) für Fremdprozesse in einer bwrap-netns.
//!
//! # Verantwortung
//! Fremdprozesse (geckodriver/Firefox, künftige Netz-Shell) haben in
//! `bwrap --unshare-net` kein Netz. Ihr einziger Weg nach draußen ist
//! `harw-netns-relay` → gebundener Unix-Socket → dieser Proxy im Harness (Plan
//! Teil B, Design „Egress ohne Landlock“). Der Proxy setzt **dieselbe**
//! [`EgressPolicy`] durch wie der In-Process-Client aus
//! [`crate::build_client`]:
//!
//! - Nur über einen `tokio::net::UnixListener` erreichbar, **nie** über TCP.
//!   Zugriffskontrolle ist das Einbinden des Sockets in die Sandbox; daher
//!   bietet der Proxy ausschließlich die Methode `NO AUTHENTICATION REQUIRED`
//!   (`0x00`) an.
//! - Nur `CONNECT` (`0x01`); `BIND` und `UDP ASSOCIATE` → Antwort `0x07`.
//! - `ATYP` Domain: Name normalisieren (WHATWG-Host-Parser wie bei URLs) →
//!   Allowlist (`check_host`) **vor** jeder Auflösung → Auflösung →
//!   `filter_resolved` (Adressklassen) → Verbindung **nur** zu einer geprüften
//!   IP (kein DNS-Rebinding-Fenster; der Client im Sandbox-Prozess löst nie
//!   selbst auf).
//! - `ATYP` IPv4/IPv6 (und Domains, die ein IP-Literal sind, z. B. `127.1`):
//!   `check_addr` **und** Allowlist über die kanonische IP-Darstellung — wie
//!   [`EgressPolicy::check_url`] für IP-Literale.
//! - Timeouts für Handshake und Verbindungsaufbau (Auflösung eingeschlossen),
//!   Leerlauf-Timeout und Byte-Obergrenze je Richtung beim Kopieren, Obergrenze
//!   gleichzeitiger Verbindungen.
//! - Strukturierte `tracing`-Events je Sitzung (Sitzungsnummer, Ziel, Port,
//!   Ergebnis, Bytezahlen) — **nie** Nutzdaten.
//!
//! # Antwortcodes
//! `0x00` verbunden · `0x01` allgemeiner Fehler (ungültiges `RSV`, leerer
//! Name, sonstiger Verbindungsfehler) · `0x02` von der Policy abgelehnt
//! (Host nicht erlaubt, Adressklasse unzulässig, ungültiger Name) · `0x03`
//! Netz unerreichbar · `0x04` Host unerreichbar (Auflösung fehlgeschlagen/leer,
//! Timeout) · `0x05` Verbindung abgelehnt · `0x07` Kommando nicht unterstützt
//! · `0x08` Adresstyp nicht unterstützt. `BND.ADDR`/`BND.PORT` sind immer
//! `0.0.0.0:0` (keine lokalen Harness-Adressen in die Sandbox).
//!
//! # Zentrale Typen
//! [`EgressProxy`], [`ProxyLimits`], Funktion [`serve`].
//!
//! # Nebenläufigkeit
//! Eine Accept-Schleife; jede Sitzung ist ein Task in einem `JoinSet`. Beim
//! Shutdown werden keine neuen Verbindungen angenommen und alle laufenden
//! Sitzungen abgebrochen. Policy und Limits werden über `Arc` geteilt.
//!
//! # Fehler
//! Ablehnungen einzelner Verbindungen sind SOCKS-Antwortcodes plus
//! `tracing`-Event; [`serve`] selbst beendet sich nur über `shutdown`.
//!
//! # Beispiele
//! ```rust,no_run
//! use std::sync::Arc;
//! use harw_egress::{EgressPolicy, ProxyLimits, serve};
//!
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let policy = Arc::new(EgressPolicy::new(vec!["docs.rs".to_owned()], false)?);
//! let listener = tokio::net::UnixListener::bind("/run/harw/egress.sock")?;
//! let shutdown = std::future::pending::<()>();
//! serve(listener, policy, ProxyLimits::default(), shutdown).await?;
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, UnixListener, UnixStream};
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::client::{HostLookup, LookupFuture, filter_resolved};
use crate::error::EgressError;
use crate::policy::EgressPolicy;

const SOCKS_VERSION: u8 = 0x05;
const METHOD_NO_AUTH: u8 = 0x00;
const METHOD_NONE_ACCEPTABLE: u8 = 0xFF;
const CMD_CONNECT: u8 = 0x01;
const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;

const REP_SUCCEEDED: u8 = 0x00;
const REP_GENERAL_FAILURE: u8 = 0x01;
const REP_NOT_ALLOWED: u8 = 0x02;
const REP_NETWORK_UNREACHABLE: u8 = 0x03;
const REP_HOST_UNREACHABLE: u8 = 0x04;
const REP_CONNECTION_REFUSED: u8 = 0x05;
const REP_COMMAND_NOT_SUPPORTED: u8 = 0x07;
const REP_ADDRESS_TYPE_NOT_SUPPORTED: u8 = 0x08;

// Puffergröße je Kopierrichtung.
const COPY_BUFFER: usize = 16 * 1024;
// Pause nach einem fehlgeschlagenen `accept` (z. B. EMFILE), damit die
// Schleife nicht heiß läuft.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// Zeit-, Byte- und Verbindungsgrenzen des Egress-Proxys.
///
/// # Description
/// Alle Grenzen gelten je Sitzung außer `max_connections` (gleichzeitig
/// offene Sitzungen). Ein Wert `0` bei `max_connections` lässt keine
/// Verbindung zu; eine Dauer `0` lässt die jeweilige Phase sofort scheitern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProxyLimits {
    /// Frist für Methodenaushandlung und Anfrage (inkl. Antwort schreiben).
    pub handshake_timeout: Duration,
    /// Frist für Namensauflösung plus TCP-Verbindungsaufbau (alle Kandidaten zusammen).
    pub connect_timeout: Duration,
    /// Leerlauf-Frist beim Kopieren: kein Byte in beiden Richtungen bzw. ein
    /// Schreibvorgang, der so lange blockiert → Sitzung endet.
    pub idle_timeout: Duration,
    /// Höchstzahl gleichzeitig offener Sitzungen; darüber wird die neue
    /// Verbindung ohne Antwort geschlossen.
    pub max_connections: usize,
    /// Byte-Obergrenze je Richtung und Sitzung; wird sie überschritten, endet
    /// die Sitzung, ohne den überschießenden Block weiterzugeben.
    pub max_bytes_per_direction: u64,
}

impl Default for ProxyLimits {
    /// Handshake 10 s, Verbindungsaufbau 15 s, Leerlauf 300 s, 256
    /// Sitzungen, 1 GiB je Richtung.
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(15),
            idle_timeout: Duration::from_secs(300),
            max_connections: 256,
            max_bytes_per_direction: 1 << 30,
        }
    }
}

/// SOCKS5-Egress-Proxy mit fester Policy und festen Limits.
///
/// # Description
/// Hält [`EgressPolicy`], [`ProxyLimits`] und den Namensauflöser (System über
/// `tokio::net::lookup_host`). [`EgressProxy::run`] bedient einen
/// Unix-Listener bis zum Shutdown.
///
/// # Concurrency
/// `Send + Sync`; der Zustand liegt hinter einem `Arc` und wird je Sitzung
/// per `Arc::clone` geteilt.
pub struct EgressProxy {
    shared: Arc<Shared>,
}

// Gemeinsamer, unveränderlicher Zustand aller Sitzungen.
struct Shared {
    policy: Arc<EgressPolicy>,
    limits: ProxyLimits,
    lookup: Arc<dyn HostLookup>,
}

impl fmt::Debug for EgressProxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EgressProxy")
            .field("policy", &self.shared.policy)
            .field("limits", &self.shared.limits)
            .finish_non_exhaustive()
    }
}

impl EgressProxy {
    /// Baut einen Proxy mit System-Namensauflösung.
    ///
    /// # Arguments
    /// - `policy` (`Arc<EgressPolicy>`): geteilte Policy.
    /// - `limits` (`ProxyLimits`): Grenzen je Sitzung.
    ///
    /// # Returns
    /// Den [`EgressProxy`].
    ///
    /// # Concurrency
    /// Reine Konstruktion.
    ///
    /// # Examples
    /// ```rust
    /// use std::sync::Arc;
    /// use harw_egress::{EgressPolicy, EgressProxy, ProxyLimits};
    ///
    /// let policy = Arc::new(EgressPolicy::new(vec!["docs.rs".into()], false).unwrap());
    /// let proxy = EgressProxy::new(policy, ProxyLimits::default());
    /// assert_eq!(proxy.limits().max_connections, 256);
    /// ```
    #[must_use]
    pub fn new(policy: Arc<EgressPolicy>, limits: ProxyLimits) -> Self {
        Self::with_lookup(policy, limits, Arc::new(SystemLookup))
    }

    // Wie `new`, aber mit austauschbarem Auflöser (Tests: netzfreier Stub).
    pub(crate) fn with_lookup(
        policy: Arc<EgressPolicy>,
        limits: ProxyLimits,
        lookup: Arc<dyn HostLookup>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                policy,
                limits,
                lookup,
            }),
        }
    }

    /// Die durchgesetzte Policy.
    #[must_use]
    pub fn policy(&self) -> &EgressPolicy {
        &self.shared.policy
    }

    /// Die Grenzen je Sitzung.
    #[must_use]
    pub fn limits(&self) -> ProxyLimits {
        self.shared.limits
    }

    /// Bedient `listener`, bis `shutdown` fertig ist.
    ///
    /// # Description
    /// Nimmt Verbindungen an und startet je Verbindung eine Sitzung (siehe
    /// Moduldoku). Ist `max_connections` erreicht, wird die neue Verbindung
    /// sofort geschlossen. `accept`-Fehler werden protokolliert; die Schleife
    /// macht nach kurzer Pause weiter. Nach `shutdown` werden alle laufenden
    /// Sitzungen abgebrochen und abgewartet.
    ///
    /// # Arguments
    /// - `listener` (`UnixListener`): gebundener Unix-Listener (Eigentum).
    /// - `shutdown` (`impl Future<Output = ()>`): beendet den Proxy.
    ///
    /// # Returns
    /// `Ok(())` nach dem Shutdown.
    ///
    /// # Errors
    /// Derzeit keine: Fehler einzelner Verbindungen und `accept`-Fehler sind
    /// nicht fatal. Der `Result`-Typ hält den Vertrag für künftige fatale
    /// Fehler offen.
    ///
    /// # Concurrency
    /// Muss innerhalb einer Tokio-Runtime laufen (Tasks per `JoinSet::spawn`).
    ///
    /// # Examples
    /// Siehe Moduldoku ([`serve`]).
    pub async fn run<F>(self, listener: UnixListener, shutdown: F) -> Result<(), EgressError>
    where
        F: Future<Output = ()>,
    {
        let mut shutdown = std::pin::pin!(shutdown);
        let mut sessions: JoinSet<()> = JoinSet::new();
        let mut next_session: u64 = 0;
        let limits = self.shared.limits;
        tracing::info!(
            allow_hosts = self.shared.policy.allow_hosts().len(),
            allow_private = self.shared.policy.allow_private(),
            max_connections = limits.max_connections,
            "egress-proxy: gestartet"
        );
        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                Some(joined) = sessions.join_next(), if !sessions.is_empty() => {
                    log_join(joined);
                }
                accepted = listener.accept() => match accepted {
                    Ok((stream, _peer)) => {
                        while let Some(joined) = sessions.try_join_next() {
                            log_join(joined);
                        }
                        next_session = next_session.wrapping_add(1);
                        if sessions.len() >= limits.max_connections {
                            tracing::warn!(
                                session = next_session,
                                active = sessions.len(),
                                max_connections = limits.max_connections,
                                "egress-proxy: Verbindungslimit erreicht, Verbindung geschlossen"
                            );
                            drop(stream);
                        } else {
                            sessions.spawn(run_session(
                                Arc::clone(&self.shared),
                                next_session,
                                stream,
                            ));
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "egress-proxy: accept fehlgeschlagen");
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                    }
                },
            }
        }
        let aborted = sessions.len();
        sessions.shutdown().await;
        tracing::info!(aborted, "egress-proxy: beendet");
        Ok(())
    }
}

/// Bedient einen Unix-Listener als SOCKS5-Egress-Proxy bis zum Shutdown.
///
/// # Description
/// Kurzform für `EgressProxy::new(policy, limits).run(listener, shutdown)`.
/// Der Aufrufer bindet den Socket (Pfad unter dem Harness-Laufzeitverzeichnis,
/// Rechte `0600`/`0700`-Verzeichnis) und bindet ihn in die Sandbox ein.
///
/// # Arguments
/// - `listener` (`tokio::net::UnixListener`): gebundener Listener (Eigentum).
/// - `policy` (`Arc<EgressPolicy>`): dieselbe Policy wie für In-Process-Tools.
/// - `limits` (`ProxyLimits`): Grenzen je Sitzung.
/// - `shutdown` (`impl Future<Output = ()>`): beendet den Proxy.
///
/// # Returns
/// `Ok(())` nach dem Shutdown.
///
/// # Errors
/// Derzeit keine (siehe [`EgressProxy::run`]).
///
/// # Concurrency
/// Muss innerhalb einer Tokio-Runtime laufen; startet einen Task je Sitzung.
///
/// # Examples
/// Siehe Moduldoku.
pub async fn serve(
    listener: UnixListener,
    policy: Arc<EgressPolicy>,
    limits: ProxyLimits,
    shutdown: impl Future<Output = ()>,
) -> Result<(), EgressError> {
    EgressProxy::new(policy, limits)
        .run(listener, shutdown)
        .await
}

// Protokolliert abgestürzte Sitzungs-Tasks (Abbruch beim Shutdown ist normal).
fn log_join(joined: Result<(), tokio::task::JoinError>) {
    match joined {
        Err(err) if err.is_panic() => {
            tracing::error!(error = %err, "egress-proxy: Sitzung abgestürzt");
        }
        Ok(()) | Err(_) => {}
    }
}

// System-Namensauflösung (blockierend in `spawn_blocking` über tokio).
struct SystemLookup;

impl HostLookup for SystemLookup {
    fn lookup(&self, host: &str) -> LookupFuture {
        let host = host.to_owned();
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            Ok::<Vec<SocketAddr>, io::Error>(addrs.collect())
        })
    }
}

// ---------------------------------------------------------------------------
// Protokoll (RFC 1928)
// ---------------------------------------------------------------------------

// Zieladresse, wie sie im Request stand.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TargetAddr {
    Ip(IpAddr),
    // Rohbytes des Domain-Felds (1..=255 Bytes, noch nicht validiert).
    Domain(Vec<u8>),
}

// Geparster SOCKS5-Request.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Request {
    command: u8,
    target: TargetAddr,
    port: u16,
}

// Fehler in Methodenaushandlung oder Request.
#[derive(Debug)]
enum HandshakeError {
    Io(io::Error),
    Version(u8),
    NoAcceptableMethod,
    Reserved(u8),
    AddressType(u8),
    EmptyDomain,
    UnsupportedCommand(u8),
}

impl HandshakeError {
    // Antwortcode, der vor dem Schließen gesendet wird (`None`: ohne Antwort
    // schließen, weil kein SOCKS5-Gegenüber bzw. Verbindung kaputt).
    const fn reply_code(&self) -> Option<u8> {
        match self {
            Self::Io(_) | Self::Version(_) | Self::NoAcceptableMethod => None,
            Self::Reserved(_) | Self::EmptyDomain => Some(REP_GENERAL_FAILURE),
            Self::AddressType(_) => Some(REP_ADDRESS_TYPE_NOT_SUPPORTED),
            Self::UnsupportedCommand(_) => Some(REP_COMMAND_NOT_SUPPORTED),
        }
    }
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O-Fehler: {err}"),
            Self::Version(version) => write!(f, "keine SOCKS5-Verbindung (Version {version})"),
            Self::NoAcceptableMethod => f.write_str("Client bietet keine Methode NO AUTH an"),
            Self::Reserved(value) => write!(f, "RSV-Feld ist {value:#04x} statt 0"),
            Self::AddressType(atyp) => write!(f, "Adresstyp {atyp:#04x} nicht unterstützt"),
            Self::EmptyDomain => f.write_str("leerer Domain-Name"),
            Self::UnsupportedCommand(cmd) => write!(f, "Kommando {cmd:#04x} nicht unterstützt"),
        }
    }
}

impl From<io::Error> for HandshakeError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

// Liest die Methodenauswahl; `true`, wenn NO AUTH angeboten wird.
async fn read_greeting<R>(reader: &mut R) -> Result<bool, HandshakeError>
where
    R: AsyncRead + Unpin,
{
    let mut head = [0_u8; 2];
    reader.read_exact(&mut head).await?;
    if head[0] != SOCKS_VERSION {
        return Err(HandshakeError::Version(head[0]));
    }
    let mut methods = [0_u8; 255];
    let methods = &mut methods[..usize::from(head[1])];
    reader.read_exact(methods).await?;
    Ok(methods.contains(&METHOD_NO_AUTH))
}

// Liest einen Request (ohne das Kommando zu bewerten).
async fn read_request<R>(reader: &mut R) -> Result<Request, HandshakeError>
where
    R: AsyncRead + Unpin,
{
    let mut head = [0_u8; 4];
    reader.read_exact(&mut head).await?;
    let [version, command, reserved, atyp] = head;
    if version != SOCKS_VERSION {
        return Err(HandshakeError::Version(version));
    }
    if reserved != 0 {
        return Err(HandshakeError::Reserved(reserved));
    }
    let target = match atyp {
        ATYP_IPV4 => {
            let mut octets = [0_u8; 4];
            reader.read_exact(&mut octets).await?;
            TargetAddr::Ip(IpAddr::V4(Ipv4Addr::from(octets)))
        }
        ATYP_IPV6 => {
            let mut octets = [0_u8; 16];
            reader.read_exact(&mut octets).await?;
            TargetAddr::Ip(IpAddr::V6(Ipv6Addr::from(octets)))
        }
        ATYP_DOMAIN => {
            let mut len = [0_u8; 1];
            reader.read_exact(&mut len).await?;
            if len[0] == 0 {
                return Err(HandshakeError::EmptyDomain);
            }
            let mut name = vec![0_u8; usize::from(len[0])];
            reader.read_exact(&mut name).await?;
            TargetAddr::Domain(name)
        }
        other => return Err(HandshakeError::AddressType(other)),
    };
    let mut port = [0_u8; 2];
    reader.read_exact(&mut port).await?;
    Ok(Request {
        command,
        target,
        port: u16::from_be_bytes(port),
    })
}

// Methodenaushandlung plus Request; sendet bei Ablehnung die passende Antwort.
async fn handshake<S>(stream: &mut S) -> Result<Request, HandshakeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if !read_greeting(stream).await? {
        stream
            .write_all(&[SOCKS_VERSION, METHOD_NONE_ACCEPTABLE])
            .await?;
        return Err(HandshakeError::NoAcceptableMethod);
    }
    stream.write_all(&[SOCKS_VERSION, METHOD_NO_AUTH]).await?;
    let request = match read_request(stream).await {
        Ok(request) => request,
        Err(err) => {
            if let Some(code) = err.reply_code() {
                write_reply(stream, code).await?;
            }
            return Err(err);
        }
    };
    if request.command != CMD_CONNECT {
        let err = HandshakeError::UnsupportedCommand(request.command);
        if let Some(code) = err.reply_code() {
            write_reply(stream, code).await?;
        }
        return Err(err);
    }
    Ok(request)
}

// Antwort mit festem `BND.ADDR` 0.0.0.0 und `BND.PORT` 0.
fn reply_bytes(code: u8) -> [u8; 10] {
    [SOCKS_VERSION, code, 0, ATYP_IPV4, 0, 0, 0, 0, 0, 0]
}

async fn write_reply<W>(writer: &mut W, code: u8) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    writer.write_all(&reply_bytes(code)).await?;
    writer.flush().await
}

// ---------------------------------------------------------------------------
// Ziel prüfen und verbinden
// ---------------------------------------------------------------------------

// Normalisiertes Ziel.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Ip(IpAddr),
    // Domain in Normalform (IDNA-ASCII, klein, ohne abschließenden Punkt).
    Domain(String),
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ip(ip) => write!(f, "{ip}"),
            Self::Domain(name) => f.write_str(name),
        }
    }
}

// Normalisiert die Zieladresse; `None` bei syntaktisch unzulässigem Namen.
fn normalize_target(target: &TargetAddr) -> Option<Target> {
    match target {
        TargetAddr::Ip(ip) => Some(Target::Ip(*ip)),
        TargetAddr::Domain(raw) => normalize_domain(raw),
    }
}

// Domain-Feld → Ziel. Erlaubt nur `[A-Za-z0-9._-]` (plus `[`, `]`, `:` für
// IPv6 in Klammern); kein Prozent, kein Leerraum, kein Nicht-ASCII. Danach
// derselbe WHATWG-Host-Parser wie für Allowlist und URLs, so dass `127.1`
// als IP-Literal und `Docs.RS.` als `docs.rs` gelten.
fn normalize_domain(raw: &[u8]) -> Option<Target> {
    let allowed =
        |b: &u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'[' | b']' | b':');
    if raw.is_empty() || !raw.iter().all(allowed) {
        return None;
    }
    let text = std::str::from_utf8(raw).ok()?;
    match url::Host::parse(text).ok()? {
        url::Host::Ipv4(v4) => Some(Target::Ip(IpAddr::V4(v4))),
        url::Host::Ipv6(v6) => Some(Target::Ip(IpAddr::V6(v6))),
        url::Host::Domain(domain) => {
            let name = domain.strip_suffix('.').unwrap_or(&domain);
            let valid = !name.is_empty()
                && name.split('.').all(|label| !label.is_empty())
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
            valid.then(|| Target::Domain(name.to_ascii_lowercase()))
        }
    }
}

// Warum kein Upstream zustande kam.
#[derive(Debug)]
enum ConnectFailure {
    // Name syntaktisch unzulässig.
    InvalidName,
    // Policy-Ablehnung oder Auflösungsfehler.
    Egress(EgressError),
    // Alle geprüften Kandidaten scheiterten; letzter Fehler.
    Connect(io::Error),
    // `connect_timeout` abgelaufen.
    Timeout,
}

impl ConnectFailure {
    // SOCKS5-Antwortcode.
    fn reply_code(&self) -> u8 {
        match self {
            Self::InvalidName => REP_NOT_ALLOWED,
            Self::Egress(EgressError::Lookup { .. }) | Self::Timeout => REP_HOST_UNREACHABLE,
            Self::Egress(EgressError::NoPermittedAddress { denied, .. }) if denied.is_empty() => {
                REP_HOST_UNREACHABLE
            }
            Self::Egress(_) => REP_NOT_ALLOWED,
            Self::Connect(err) => match err.kind() {
                io::ErrorKind::ConnectionRefused => REP_CONNECTION_REFUSED,
                io::ErrorKind::NetworkUnreachable => REP_NETWORK_UNREACHABLE,
                io::ErrorKind::HostUnreachable | io::ErrorKind::TimedOut => REP_HOST_UNREACHABLE,
                _ => REP_GENERAL_FAILURE,
            },
        }
    }
}

impl fmt::Display for ConnectFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName => f.write_str("unzulässiger Zielname"),
            Self::Egress(err) => write!(f, "{err}"),
            Self::Connect(err) => write!(f, "Verbindungsaufbau fehlgeschlagen: {err}"),
            Self::Timeout => f.write_str("Verbindungsaufbau-Timeout"),
        }
    }
}

// Prüft das Ziel gegen die Policy und liefert die zulässigen Kandidaten.
async fn checked_candidates(
    shared: &Shared,
    target: &Target,
    port: u16,
) -> Result<Vec<SocketAddr>, ConnectFailure> {
    let policy = &shared.policy;
    match target {
        Target::Ip(ip) => {
            let addr = SocketAddr::new(*ip, port);
            policy.check_addr(addr).map_err(ConnectFailure::Egress)?;
            policy
                .check_host(&ip.to_string())
                .map_err(ConnectFailure::Egress)?;
            Ok(vec![addr])
        }
        Target::Domain(host) => {
            policy.check_host(host).map_err(ConnectFailure::Egress)?;
            let resolved = shared.lookup.lookup(host).await.map_err(|source| {
                ConnectFailure::Egress(EgressError::Lookup {
                    host: host.to_owned(),
                    source,
                })
            })?;
            let with_port = resolved
                .into_iter()
                .map(|addr| SocketAddr::new(addr.ip(), port));
            filter_resolved(policy, host, with_port).map_err(ConnectFailure::Egress)
        }
    }
}

// Prüft und verbindet; ohne Zeitgrenze (der Aufrufer setzt `connect_timeout`).
async fn open_upstream(
    shared: &Shared,
    target: &Target,
    port: u16,
) -> Result<(TcpStream, SocketAddr), ConnectFailure> {
    let candidates = checked_candidates(shared, target, port).await?;
    let mut last_error = None;
    for addr in candidates {
        match TcpStream::connect(addr).await {
            Ok(stream) => return Ok((stream, addr)),
            Err(err) => {
                tracing::debug!(addr = %addr, error = %err, "egress-proxy: Kandidat gescheitert");
                last_error = Some(err);
            }
        }
    }
    Err(ConnectFailure::Connect(last_error.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "keine Zieladresse")
    })))
}

// ---------------------------------------------------------------------------
// Sitzung
// ---------------------------------------------------------------------------

// Eine Sitzung von der Aushandlung bis zum Ende des Kopierens; Ergebnis nur
// als tracing-Event.
async fn run_session(shared: Arc<Shared>, session: u64, mut client: UnixStream) {
    let limits = shared.limits;
    let request = match timeout(limits.handshake_timeout, handshake(&mut client)).await {
        Ok(Ok(request)) => request,
        Ok(Err(err)) => {
            tracing::warn!(session, error = %err, "egress-proxy: Handshake abgelehnt");
            return;
        }
        Err(_elapsed) => {
            tracing::warn!(session, "egress-proxy: Handshake-Timeout");
            return;
        }
    };
    let port = request.port;
    let Some(target) = normalize_target(&request.target) else {
        let len = match &request.target {
            TargetAddr::Domain(raw) => raw.len(),
            TargetAddr::Ip(_) => 0,
        };
        let failure = ConnectFailure::InvalidName;
        deny(
            &mut client,
            session,
            None,
            port,
            &failure,
            len,
            limits.handshake_timeout,
        )
        .await;
        return;
    };

    let opened = match timeout(
        limits.connect_timeout,
        open_upstream(&shared, &target, port),
    )
    .await
    {
        Ok(result) => result,
        Err(_elapsed) => Err(ConnectFailure::Timeout),
    };
    let (mut upstream, addr) = match opened {
        Ok(opened) => opened,
        Err(failure) => {
            let reply_timeout = limits.handshake_timeout;
            deny(
                &mut client,
                session,
                Some(&target),
                port,
                &failure,
                0,
                reply_timeout,
            )
            .await;
            return;
        }
    };

    match timeout(
        limits.handshake_timeout,
        write_reply(&mut client, REP_SUCCEEDED),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::debug!(session, error = %err, "egress-proxy: Erfolgsantwort nicht zustellbar");
            return;
        }
        Err(_elapsed) => {
            tracing::debug!(session, "egress-proxy: Erfolgsantwort-Timeout");
            return;
        }
    }
    tracing::info!(session, host = %target, port, addr = %addr, "egress-proxy: verbunden");

    let transfer = pump(&mut client, &mut upstream, &limits).await;
    tracing::info!(
        session,
        host = %target,
        port,
        bytes_up = transfer.bytes_up,
        bytes_down = transfer.bytes_down,
        end = %transfer.end,
        "egress-proxy: Sitzung beendet"
    );
}

// Sendet den Ablehnungscode und protokolliert die Ablehnung.
async fn deny(
    client: &mut UnixStream,
    session: u64,
    target: Option<&Target>,
    port: u16,
    failure: &ConnectFailure,
    raw_name_len: usize,
    reply_timeout: Duration,
) {
    let code = failure.reply_code();
    match target {
        Some(target) => tracing::warn!(
            session,
            host = %target,
            port,
            reply = code,
            reason = %failure,
            "egress-proxy: Verbindung abgelehnt"
        ),
        None => tracing::warn!(
            session,
            name_len = raw_name_len,
            port,
            reply = code,
            reason = %failure,
            "egress-proxy: Verbindung abgelehnt"
        ),
    }
    match timeout(reply_timeout, write_reply(client, code)).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::debug!(session, error = %err, "egress-proxy: Ablehnung nicht zustellbar");
        }
        Err(_elapsed) => tracing::debug!(session, "egress-proxy: Ablehnung-Timeout"),
    }
}

// Wie das Kopieren endete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PumpEnd {
    // Beide Richtungen haben EOF gesehen.
    Closed,
    // Leerlauf- oder Schreib-Timeout.
    Idle,
    // Byte-Obergrenze einer Richtung überschritten.
    ByteLimit(Direction),
    // I/O-Fehler beim Lesen oder Schreiben.
    Io(Direction, io::ErrorKind),
}

impl fmt::Display for PumpEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("closed"),
            Self::Idle => f.write_str("idle-timeout"),
            Self::ByteLimit(direction) => write!(f, "byte-limit-{direction}"),
            Self::Io(direction, kind) => write!(f, "io-error-{direction}: {kind}"),
        }
    }
}

// Kopierrichtung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    // Sandbox → Ziel.
    Up,
    // Ziel → Sandbox.
    Down,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Up => f.write_str("up"),
            Self::Down => f.write_str("down"),
        }
    }
}

// Ergebnis des Kopierens (nur Zähler, keine Daten).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Transfer {
    bytes_up: u64,
    bytes_down: u64,
    end: PumpEnd,
}

// Kopiert bidirektional mit Leerlauf-Timeout und Byte-Obergrenze je Richtung.
// EOF einer Richtung schließt die Schreibseite der Gegenseite halb.
async fn pump(client: &mut UnixStream, upstream: &mut TcpStream, limits: &ProxyLimits) -> Transfer {
    let (mut client_rd, mut client_wr) = client.split();
    let (mut up_rd, mut up_wr) = upstream.split();
    let mut to_up = vec![0_u8; COPY_BUFFER];
    let mut to_client = vec![0_u8; COPY_BUFFER];
    let mut transfer = Transfer {
        bytes_up: 0,
        bytes_down: 0,
        end: PumpEnd::Closed,
    };
    let mut up_open = true;
    let mut down_open = true;

    while up_open || down_open {
        tokio::select! {
            read = client_rd.read(&mut to_up), if up_open => match read {
                Ok(0) => {
                    up_open = false;
                    half_close(&mut up_wr, limits.idle_timeout).await;
                }
                Ok(n) => {
                    let sent = forward(
                        &mut up_wr,
                        &to_up[..n],
                        &mut transfer.bytes_up,
                        limits,
                        Direction::Up,
                    )
                    .await;
                    if let Err(end) = sent {
                        transfer.end = end;
                        return transfer;
                    }
                }
                Err(err) => {
                    transfer.end = PumpEnd::Io(Direction::Up, err.kind());
                    return transfer;
                }
            },
            read = up_rd.read(&mut to_client), if down_open => match read {
                Ok(0) => {
                    down_open = false;
                    half_close(&mut client_wr, limits.idle_timeout).await;
                }
                Ok(n) => {
                    let sent = forward(
                        &mut client_wr,
                        &to_client[..n],
                        &mut transfer.bytes_down,
                        limits,
                        Direction::Down,
                    )
                    .await;
                    if let Err(end) = sent {
                        transfer.end = end;
                        return transfer;
                    }
                }
                Err(err) => {
                    transfer.end = PumpEnd::Io(Direction::Down, err.kind());
                    return transfer;
                }
            },
            () = tokio::time::sleep(limits.idle_timeout) => {
                transfer.end = PumpEnd::Idle;
                return transfer;
            }
        }
    }
    transfer
}

// Schreibt einen Block, sofern die Byte-Obergrenze nicht überschritten wird.
async fn forward<W>(
    writer: &mut W,
    data: &[u8],
    counter: &mut u64,
    limits: &ProxyLimits,
    direction: Direction,
) -> Result<(), PumpEnd>
where
    W: AsyncWrite + Unpin,
{
    let len = u64::try_from(data.len()).unwrap_or(u64::MAX);
    let total = counter.saturating_add(len);
    if total > limits.max_bytes_per_direction {
        return Err(PumpEnd::ByteLimit(direction));
    }
    match timeout(limits.idle_timeout, writer.write_all(data)).await {
        Ok(Ok(())) => {
            *counter = total;
            Ok(())
        }
        Ok(Err(err)) => Err(PumpEnd::Io(direction, err.kind())),
        Err(_elapsed) => Err(PumpEnd::Idle),
    }
}

// Halb-Schließen der Schreibseite. Ein Fehler heißt, dass die Gegenseite
// bereits weg ist; das Lesen der anderen Richtung meldet das ohnehin.
async fn half_close<W>(writer: &mut W, limit: Duration)
where
    W: AsyncWrite + Unpin,
{
    match timeout(limit, writer.shutdown()).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::debug!(error = %err, "egress-proxy: Halb-Schließen fehlgeschlagen");
        }
        Err(_elapsed) => tracing::debug!("egress-proxy: Halb-Schließen-Timeout"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;

    use crate::test_support::{TestError, TestResult, ctx};

    // Netzfreier Resolver mit festen Antworten; zählt Aufrufe.
    struct StubLookup {
        answers: HashMap<String, Vec<SocketAddr>>,
        calls: AtomicUsize,
    }

    impl StubLookup {
        fn new(entries: &[(&str, &[&str])]) -> TestResult<Arc<Self>> {
            let mut answers = HashMap::new();
            for (host, addrs) in entries {
                let mut parsed = Vec::with_capacity(addrs.len());
                for raw_ip in *addrs {
                    let ip: IpAddr = raw_ip.parse().map_err(ctx("Test-IP"))?;
                    parsed.push(SocketAddr::new(ip, 0));
                }
                answers.insert((*host).to_owned(), parsed);
            }
            Ok(Arc::new(Self {
                answers,
                calls: AtomicUsize::new(0),
            }))
        }
    }

    impl HostLookup for StubLookup {
        fn lookup(&self, host: &str) -> LookupFuture {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let result =
                self.answers.get(host).cloned().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "stub: unbekannter Host")
                });
            Box::pin(std::future::ready(result))
        }
    }

    fn policy(hosts: &[&str], allow_private: bool) -> TestResult<Arc<EgressPolicy>> {
        let hosts = hosts.iter().map(|h| (*h).to_owned()).collect();
        let policy = EgressPolicy::new(hosts, allow_private).map_err(ctx("gültige Test-Policy"))?;
        Ok(Arc::new(policy))
    }

    fn shared(policy: Arc<EgressPolicy>, lookup: Arc<StubLookup>) -> Shared {
        Shared {
            policy,
            limits: ProxyLimits::default(),
            lookup,
        }
    }

    fn request_bytes(command: u8, target: &TargetAddr, port: u16) -> TestResult<Vec<u8>> {
        let mut bytes = vec![SOCKS_VERSION, command, 0];
        match target {
            TargetAddr::Ip(IpAddr::V4(v4)) => {
                bytes.push(ATYP_IPV4);
                bytes.extend_from_slice(&v4.octets());
            }
            TargetAddr::Ip(IpAddr::V6(v6)) => {
                bytes.push(ATYP_IPV6);
                bytes.extend_from_slice(&v6.octets());
            }
            TargetAddr::Domain(name) => {
                bytes.push(ATYP_DOMAIN);
                let len = u8::try_from(name.len()).map_err(ctx("Name ≤ 255"))?;
                bytes.push(len);
                bytes.extend_from_slice(name);
            }
        }
        bytes.extend_from_slice(&port.to_be_bytes());
        Ok(bytes)
    }

    #[tokio::test]
    async fn test_read_greeting_detects_no_auth() -> TestResult {
        let mut offered: &[u8] = &[5, 2, 0x02, 0x00];
        assert!(read_greeting(&mut offered).await.map_err(ctx("gültig"))?);
        let mut password_only: &[u8] = &[5, 1, 0x02];
        assert!(
            !read_greeting(&mut password_only)
                .await
                .map_err(ctx("gültig"))?
        );
        let mut none: &[u8] = &[5, 0];
        assert!(!read_greeting(&mut none).await.map_err(ctx("gültig"))?);
        let mut socks4: &[u8] = &[4, 1, 0x00];
        assert!(matches!(
            read_greeting(&mut socks4).await,
            Err(HandshakeError::Version(4))
        ));
        let mut truncated: &[u8] = &[5, 3, 0x00];
        assert!(matches!(
            read_greeting(&mut truncated).await,
            Err(HandshakeError::Io(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn test_read_request_parses_domain_ipv4_ipv6() -> TestResult {
        let cases = [
            TargetAddr::Domain(b"docs.rs".to_vec()),
            TargetAddr::Ip("93.184.216.34".parse().map_err(ctx("IPv4"))?),
            TargetAddr::Ip("2606:4700::1111".parse().map_err(ctx("IPv6"))?),
        ];
        for target in cases {
            let bytes = request_bytes(CMD_CONNECT, &target, 443)?;
            let mut reader: &[u8] = &bytes;
            let request = read_request(&mut reader).await.map_err(ctx("gültig"))?;
            assert_eq!(
                request,
                Request {
                    command: CMD_CONNECT,
                    target,
                    port: 443
                }
            );
            assert!(reader.is_empty(), "Request vollständig gelesen");
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_read_request_rejects_malformed() -> TestResult {
        let mut bad_atyp: &[u8] = &[5, 1, 0, 0x09, 1, 2, 3, 4, 0, 80];
        let Err(err) = read_request(&mut bad_atyp).await else {
            return Err(TestError::Unexpected(
                "ATYP 9 hätte scheitern müssen".into(),
            ));
        };
        assert!(matches!(err, HandshakeError::AddressType(0x09)));
        assert_eq!(err.reply_code(), Some(REP_ADDRESS_TYPE_NOT_SUPPORTED));

        let mut empty_name: &[u8] = &[5, 1, 0, ATYP_DOMAIN, 0, 0, 80];
        let Err(err) = read_request(&mut empty_name).await else {
            return Err(TestError::Unexpected(
                "leerer Name hätte scheitern müssen".into(),
            ));
        };
        assert!(matches!(err, HandshakeError::EmptyDomain));
        assert_eq!(err.reply_code(), Some(REP_GENERAL_FAILURE));

        let mut reserved: &[u8] = &[5, 1, 1, ATYP_IPV4, 1, 2, 3, 4, 0, 80];
        let Err(err) = read_request(&mut reserved).await else {
            return Err(TestError::Unexpected("RSV hätte scheitern müssen".into()));
        };
        assert!(matches!(err, HandshakeError::Reserved(1)));
        Ok(())
    }

    #[tokio::test]
    async fn test_handshake_bind_and_udp_rejected_with_0x07() -> TestResult {
        for command in [0x02_u8, 0x03] {
            let (mut client, mut server) = tokio::io::duplex(1024);
            let target = TargetAddr::Ip("93.184.216.34".parse().map_err(ctx("IPv4"))?);
            let mut input = vec![5, 1, 0];
            input.extend(request_bytes(command, &target, 80)?);
            client.write_all(&input).await.map_err(ctx("schreiben"))?;
            let Err(err) = handshake(&mut server).await else {
                return Err(TestError::Unexpected(
                    "nicht CONNECT hätte scheitern müssen".into(),
                ));
            };
            assert!(matches!(err, HandshakeError::UnsupportedCommand(c) if c == command));
            let mut answer = [0_u8; 12];
            client
                .read_exact(&mut answer)
                .await
                .map_err(ctx("Antwort"))?;
            assert_eq!(&answer[..2], &[5, 0]);
            assert_eq!(answer[2..], reply_bytes(REP_COMMAND_NOT_SUPPORTED));
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_handshake_without_no_auth_gets_0xff() -> TestResult {
        let (mut client, mut server) = tokio::io::duplex(64);
        client
            .write_all(&[5, 1, 0x02])
            .await
            .map_err(ctx("schreiben"))?;
        let Err(err) = handshake(&mut server).await else {
            return Err(TestError::Unexpected(
                "keine Methode hätte scheitern müssen".into(),
            ));
        };
        assert!(matches!(err, HandshakeError::NoAcceptableMethod));
        let mut answer = [0_u8; 2];
        client
            .read_exact(&mut answer)
            .await
            .map_err(ctx("Antwort"))?;
        assert_eq!(answer, [5, 0xFF]);
        Ok(())
    }

    #[test]
    fn test_normalize_domain_cases() -> TestResult {
        let domain = |s: &str| Some(Target::Domain(s.to_owned()));
        let localhost: IpAddr = "127.0.0.1".parse().map_err(ctx("IP"))?;
        let ipv6_loopback: IpAddr = "::1".parse().map_err(ctx("IP"))?;
        let ip = |addr: IpAddr| Some(Target::Ip(addr));
        assert_eq!(normalize_domain(b"Docs.RS."), domain("docs.rs"));
        assert_eq!(
            normalize_domain(b"xn--bcher-kva.de"),
            domain("xn--bcher-kva.de")
        );
        assert_eq!(normalize_domain(b"127.1"), ip(localhost));
        assert_eq!(normalize_domain(b"2130706433"), ip(localhost));
        assert_eq!(normalize_domain(b"[::1]"), ip(ipv6_loopback));
        let bad_names: [&[u8]; 10] = [
            b"a..b",
            b".docs.rs",
            b"docs.rs..",
            b"%64ocs.rs",
            b"docs rs",
            b"evil.com\\@docs.rs",
            b"user@docs.rs",
            b"docs.rs:443",
            "bücher.de".as_bytes(),
            b"\x00",
        ];
        for bad in bad_names {
            assert_eq!(normalize_domain(bad), None, "{bad:?}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_checked_candidates_domain_not_allowed_is_0x02_without_lookup() -> TestResult {
        let lookup = StubLookup::new(&[("evil.example", &["93.184.216.34"])])?;
        let state = shared(policy(&["docs.rs"], false)?, Arc::clone(&lookup));
        let target = Target::Domain("evil.example".to_owned());
        let Err(failure) = checked_candidates(&state, &target, 443).await else {
            return Err(TestError::Unexpected(
                "nicht erlaubt hätte scheitern müssen".into(),
            ));
        };
        assert!(matches!(
            failure,
            ConnectFailure::Egress(EgressError::HostNotAllowed { .. })
        ));
        assert_eq!(failure.reply_code(), REP_NOT_ALLOWED);
        assert_eq!(
            lookup.calls.load(Ordering::SeqCst),
            0,
            "keine Auflösung vor Allowlist"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_checked_candidates_private_ip_is_0x02() -> TestResult {
        let lookup = StubLookup::new(&[])?;
        let state = shared(policy(&["10.0.0.1", "169.254.169.254"], false)?, lookup);
        for raw in ["10.0.0.1", "169.254.169.254", "::ffff:10.0.0.1"] {
            let ip: IpAddr = raw.parse().map_err(ctx("IP"))?;
            let target = Target::Ip(ip);
            let Err(failure) = checked_candidates(&state, &target, 80).await else {
                return Err(TestError::Unexpected(format!(
                    "{raw} hätte scheitern müssen"
                )));
            };
            assert!(
                matches!(
                    failure,
                    ConnectFailure::Egress(EgressError::AddressDenied { .. })
                ),
                "{raw}: {failure:?}"
            );
            assert_eq!(failure.reply_code(), REP_NOT_ALLOWED);
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_checked_candidates_public_ip_requires_allowlist() -> TestResult {
        let state = shared(policy(&["docs.rs"], false)?, StubLookup::new(&[])?);
        let ip: IpAddr = "93.184.216.34".parse().map_err(ctx("IP"))?;
        let target = Target::Ip(ip);
        let Err(failure) = checked_candidates(&state, &target, 80).await else {
            return Err(TestError::Unexpected(
                "nicht gelistet hätte scheitern müssen".into(),
            ));
        };
        assert!(matches!(
            failure,
            ConnectFailure::Egress(EgressError::HostNotAllowed { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn test_checked_candidates_rebinding_answer_is_0x02() -> TestResult {
        let lookup = StubLookup::new(&[("rebind.example", &["127.0.0.1", "169.254.169.254"])])?;
        let state = shared(policy(&["rebind.example"], false)?, lookup);
        let target = Target::Domain("rebind.example".to_owned());
        let Err(failure) = checked_candidates(&state, &target, 80).await else {
            return Err(TestError::Unexpected(
                "nur privat hätte scheitern müssen".into(),
            ));
        };
        assert!(matches!(
            failure,
            ConnectFailure::Egress(EgressError::NoPermittedAddress { ref denied, .. })
                if denied.len() == 2
        ));
        assert_eq!(failure.reply_code(), REP_NOT_ALLOWED);
        Ok(())
    }

    #[tokio::test]
    async fn test_checked_candidates_keeps_only_permitted_and_sets_port() -> TestResult {
        let lookup = StubLookup::new(&[("mixed.example", &["10.0.0.5", "93.184.216.34"])])?;
        let state = shared(policy(&["mixed.example"], false)?, lookup);
        let target = Target::Domain("mixed.example".to_owned());
        let addrs = checked_candidates(&state, &target, 8443)
            .await
            .map_err(ctx("eine zulässig"))?;
        let expected: SocketAddr = "93.184.216.34:8443".parse().map_err(ctx("Adresse"))?;
        assert_eq!(addrs, vec![expected]);
        Ok(())
    }

    #[test]
    fn test_connect_failure_reply_codes() {
        let lookup = ConnectFailure::Egress(EgressError::Lookup {
            host: "x.example".to_owned(),
            source: io::Error::new(io::ErrorKind::NotFound, "nx"),
        });
        assert_eq!(lookup.reply_code(), REP_HOST_UNREACHABLE);
        let empty = ConnectFailure::Egress(EgressError::NoPermittedAddress {
            host: "x.example".to_owned(),
            denied: Vec::new(),
        });
        assert_eq!(empty.reply_code(), REP_HOST_UNREACHABLE);
        assert_eq!(ConnectFailure::Timeout.reply_code(), REP_HOST_UNREACHABLE);
        assert_eq!(ConnectFailure::InvalidName.reply_code(), REP_NOT_ALLOWED);
        let refused = ConnectFailure::Connect(io::Error::from(io::ErrorKind::ConnectionRefused));
        assert_eq!(refused.reply_code(), REP_CONNECTION_REFUSED);
        let net = ConnectFailure::Connect(io::Error::from(io::ErrorKind::NetworkUnreachable));
        assert_eq!(net.reply_code(), REP_NETWORK_UNREACHABLE);
    }

    // ---- End-to-End über tempdir-Unix-Socket und lokalen Echo-Server ----

    struct Harness {
        _dir: tempfile::TempDir,
        socket: PathBuf,
        stop: Option<oneshot::Sender<()>>,
        task: JoinHandle<Result<(), EgressError>>,
    }

    fn start_proxy(
        policy: Arc<EgressPolicy>,
        lookup: Arc<StubLookup>,
        limits: ProxyLimits,
    ) -> TestResult<Harness> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let socket = dir.path().join("egress.sock");
        let listener = UnixListener::bind(&socket).map_err(ctx("Unix-Socket binden"))?;
        let (stop, stopped) = oneshot::channel::<()>();
        let proxy = EgressProxy::with_lookup(policy, limits, lookup);
        let task = tokio::spawn(proxy.run(listener, async move {
            // Senden oder Fallenlassen des Senders beendet den Proxy.
            let _ = stopped.await;
        }));
        Ok(Harness {
            _dir: dir,
            socket,
            stop: Some(stop),
            task,
        })
    }

    async fn echo_server() -> TestResult<SocketAddr> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(ctx("Loopback binden"))?;
        let addr = listener.local_addr().map_err(ctx("lokale Adresse"))?;
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let (mut rd, mut wr) = stream.split();
                    // Echo bis EOF; Fehler beenden nur diese Testverbindung.
                    let _ = tokio::io::copy(&mut rd, &mut wr).await;
                });
            }
        });
        Ok(addr)
    }

    async fn socks_connect(
        socket: &Path,
        target: &TargetAddr,
        port: u16,
    ) -> TestResult<(UnixStream, u8)> {
        let mut stream = UnixStream::connect(socket)
            .await
            .map_err(ctx("Proxy verbinden"))?;
        stream
            .write_all(&[5, 1, 0])
            .await
            .map_err(ctx("Greeting"))?;
        let mut method = [0_u8; 2];
        stream
            .read_exact(&mut method)
            .await
            .map_err(ctx("Methodenantwort"))?;
        assert_eq!(method, [5, 0]);
        stream
            .write_all(&request_bytes(CMD_CONNECT, target, port)?)
            .await
            .map_err(ctx("Request"))?;
        let mut reply = [0_u8; 10];
        stream
            .read_exact(&mut reply)
            .await
            .map_err(ctx("Antwort"))?;
        assert_eq!(reply[0], 5);
        assert_eq!(reply[2..], [0, ATYP_IPV4, 0, 0, 0, 0, 0, 0]);
        Ok((stream, reply[1]))
    }

    async fn read_eof_within(stream: &mut UnixStream, limit: Duration) -> bool {
        let mut buf = [0_u8; 64];
        matches!(
            timeout(limit, stream.read(&mut buf)).await,
            Ok(Ok(0) | Err(_))
        )
    }

    #[tokio::test]
    async fn test_serve_end_to_end_ipv4_echo() -> TestResult {
        let echo = echo_server().await?;
        let harness = start_proxy(
            policy(&["127.0.0.1"], true)?,
            StubLookup::new(&[])?,
            ProxyLimits::default(),
        )?;
        let target = TargetAddr::Ip(echo.ip());
        let (mut stream, code) = socks_connect(&harness.socket, &target, echo.port()).await?;
        assert_eq!(code, REP_SUCCEEDED);
        stream.write_all(b"ping").await.map_err(ctx("schreiben"))?;
        let mut answer = [0_u8; 4];
        timeout(Duration::from_secs(5), stream.read_exact(&mut answer))
            .await
            .map_err(ctx("rechtzeitig"))?
            .map_err(ctx("Echo"))?;
        assert_eq!(&answer, b"ping");
        Ok(())
    }

    #[tokio::test]
    async fn test_serve_end_to_end_domain_uses_checked_address() -> TestResult {
        let echo = echo_server().await?;
        let lookup = StubLookup::new(&[("echo.test", &["127.0.0.1"])])?;
        let harness = start_proxy(
            policy(&["echo.test"], true)?,
            lookup,
            ProxyLimits::default(),
        )?;
        let target = TargetAddr::Domain(b"ECHO.test.".to_vec());
        let (mut stream, code) = socks_connect(&harness.socket, &target, echo.port()).await?;
        assert_eq!(code, REP_SUCCEEDED);
        stream.write_all(b"hallo").await.map_err(ctx("schreiben"))?;
        stream.shutdown().await.map_err(ctx("halb schließen"))?;
        let mut answer = Vec::new();
        timeout(Duration::from_secs(5), stream.read_to_end(&mut answer))
            .await
            .map_err(ctx("rechtzeitig"))?
            .map_err(ctx("Echo bis EOF"))?;
        assert_eq!(answer, b"hallo");
        Ok(())
    }

    #[tokio::test]
    async fn test_serve_end_to_end_denials() -> TestResult {
        let echo = echo_server().await?;
        let lookup = StubLookup::new(&[("echo.test", &["127.0.0.1"])])?;
        // Strikte Policy: Loopback ist eine private Klasse.
        let strict = policy(&["echo.test", "127.0.0.1"], false)?;
        let harness = start_proxy(strict, lookup, ProxyLimits::default())?;
        let cases = [
            TargetAddr::Ip(echo.ip()),
            TargetAddr::Domain(b"echo.test".to_vec()),
            TargetAddr::Domain(b"other.test".to_vec()),
            TargetAddr::Domain(b"bad name".to_vec()),
        ];
        for target in cases {
            let (mut stream, code) = socks_connect(&harness.socket, &target, echo.port()).await?;
            assert_eq!(code, REP_NOT_ALLOWED, "{target:?}");
            assert!(
                read_eof_within(&mut stream, Duration::from_secs(5)).await,
                "{target:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_serve_idle_timeout_closes_session() -> TestResult {
        let echo = echo_server().await?;
        let limits = ProxyLimits {
            idle_timeout: Duration::from_millis(200),
            ..ProxyLimits::default()
        };
        let harness = start_proxy(policy(&["127.0.0.1"], true)?, StubLookup::new(&[])?, limits)?;
        let target = TargetAddr::Ip(echo.ip());
        let (mut stream, code) = socks_connect(&harness.socket, &target, echo.port()).await?;
        assert_eq!(code, REP_SUCCEEDED);
        assert!(read_eof_within(&mut stream, Duration::from_secs(5)).await);
        Ok(())
    }

    #[tokio::test]
    async fn test_serve_byte_limit_closes_without_forwarding() -> TestResult {
        let echo = echo_server().await?;
        let limits = ProxyLimits {
            max_bytes_per_direction: 4,
            ..ProxyLimits::default()
        };
        let harness = start_proxy(policy(&["127.0.0.1"], true)?, StubLookup::new(&[])?, limits)?;
        let target = TargetAddr::Ip(echo.ip());
        let (mut stream, code) = socks_connect(&harness.socket, &target, echo.port()).await?;
        assert_eq!(code, REP_SUCCEEDED);
        stream
            .write_all(b"zu lang")
            .await
            .map_err(ctx("schreiben"))?;
        let mut buf = Vec::new();
        let read = timeout(Duration::from_secs(5), stream.read_to_end(&mut buf)).await;
        // Innerhalb der Frist: EOF oder Reset, beides ohne weitergeleitete Daten.
        assert!(read.is_ok(), "Timeout: {read:?}");
        assert!(buf.is_empty(), "nichts weitergegeben: {buf:?}");
        Ok(())
    }

    #[tokio::test]
    async fn test_serve_max_connections_rejects_extra_client() -> TestResult {
        let echo = echo_server().await?;
        let limits = ProxyLimits {
            max_connections: 1,
            ..ProxyLimits::default()
        };
        let harness = start_proxy(policy(&["127.0.0.1"], true)?, StubLookup::new(&[])?, limits)?;
        let target = TargetAddr::Ip(echo.ip());
        let (_first, code) = socks_connect(&harness.socket, &target, echo.port()).await?;
        assert_eq!(code, REP_SUCCEEDED);
        let mut second = UnixStream::connect(&harness.socket)
            .await
            .map_err(ctx("verbinden"))?;
        // Schreiben kann schon scheitern, wenn der Proxy schneller schließt.
        let _ = second.write_all(&[5, 1, 0]).await;
        assert!(read_eof_within(&mut second, Duration::from_secs(5)).await);
        Ok(())
    }

    #[tokio::test]
    async fn test_serve_shutdown_returns_ok_and_aborts_sessions() -> TestResult {
        let echo = echo_server().await?;
        let mut harness = start_proxy(
            policy(&["127.0.0.1"], true)?,
            StubLookup::new(&[])?,
            ProxyLimits::default(),
        )?;
        let (mut stream, code) =
            socks_connect(&harness.socket, &TargetAddr::Ip(echo.ip()), echo.port()).await?;
        assert_eq!(code, REP_SUCCEEDED);
        let sender = harness.stop.take().ok_or(TestError::Missing("Sender"))?;
        sender.send(()).map_err(|_| {
            TestError::Unexpected("Proxy läuft nicht mehr: send() schlug fehl".into())
        })?;
        let result = timeout(Duration::from_secs(5), &mut harness.task)
            .await
            .map_err(ctx("rechtzeitig"))?
            .map_err(ctx("Task nicht abgestürzt"))?;
        assert!(result.is_ok(), "{result:?}");
        assert!(read_eof_within(&mut stream, Duration::from_secs(5)).await);
        Ok(())
    }
}
