//! TCP→Unix-Socket-Relay für Fremdprozesse in einer Netz-Namespace ohne Netz.
//!
//! # Verantwortung
//! Fremdprozesse (geckodriver/Firefox, künftige Netz-Shell) laufen in
//! `bwrap --unshare-net` und haben dort nur ein Loopback-Interface (Plan
//! Teil B, Design „Egress ohne Landlock“). Das Binary `harw-netns-relay` läuft
//! **innerhalb** dieser Namespace, lauscht auf `127.0.0.1:<port>` und leitet
//! jede TCP-Verbindung Byte für Byte an den per bwrap eingebundenen
//! Unix-Socket des [`crate::EgressProxy`] weiter. Das Relay enthält **keine**
//! Policy-Logik; Allowlist, Adressklassen, Timeouts und Limits setzt allein
//! der Proxy im Harness durch.
//!
//! # Aufruf (argv-Semantik)
//! ```text
//! harw-netns-relay <tcp-port> <unix-socket-path>                    # reiner Weiterleitungsmodus
//! harw-netns-relay <tcp-port> <unix-socket-path> -- <cmd> [args…]  # Kindmodus
//! ```
//! - `<tcp-port>`: Dezimalzahl `1..=65535`; gebunden wird immer `127.0.0.1`.
//! - `<unix-socket-path>`: nicht leerer Pfad des Proxy-Sockets (nicht
//!   interpretiert, erst beim Verbinden je Anfrage geöffnet).
//! - `--` beendet die Relay-Argumente; alles danach ist argv des Kindes,
//!   1:1 ohne Shell. Nach `--` muss mindestens `<cmd>` folgen.
//! - Kindmodus: erst nach erfolgreichem Bind startet `<cmd>` (Umgebung und
//!   stdin/stdout/stderr geerbt); weitergeleitet wird im Hintergrund, bis das
//!   Kind endet. Exit-Code = Exit-Code des Kindes, bei Signal `128 + Signal`.
//! - Exit-Codes des Relays selbst: [`EXIT_USAGE`] (64), [`EXIT_BIND_FAILED`]
//!   (70), Startfehler des Kindes siehe [`RelayError::exit_code`].
//!
//! # Zentrale Typen
//! [`RelayConfig`], [`ChildCommand`], [`RelayError`], [`RelayReporter`].
//!
//! # Nebenläufigkeit
//! Synchron mit `std::thread`: ein Thread je Verbindung plus ein Hilfsthread
//! für die Gegenrichtung; die Zahl gleichzeitiger Verbindungen ist begrenzt.
//! Kein async-Runtime, kein `Mutex` auf dem Datenpfad.
//!
//! # Fehler
//! Alle Fehler sind Varianten von [`RelayError`]; Fehler einzelner
//! Verbindungen gehen an den [`RelayReporter`] (im Binary: stderr).
//!
//! # Beispiele
//! ```rust,no_run
//! use std::sync::Arc;
//! use harw_egress::{RelayConfig, RelayError, RelayReporter, bind_relay, run_relay};
//!
//! let args = ["1080", "/run/harw/egress.sock"].map(std::ffi::OsString::from);
//! let config = RelayConfig::from_args(args)?;
//! let listener = bind_relay(config.port)?;
//! let report: RelayReporter = Arc::new(|err: &RelayError| eprintln!("{err}"));
//! match run_relay(listener, &config.proxy_socket, 128, report) {}
//! # Ok::<(), RelayError>(())
//! ```

use std::convert::Infallible;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

/// Exit-Code bei ungültigen Argumenten (BSD `EX_USAGE`).
pub const EXIT_USAGE: u8 = 64;
/// Exit-Code, wenn `127.0.0.1:<port>` nicht gebunden werden kann (`EX_SOFTWARE`).
pub const EXIT_BIND_FAILED: u8 = 70;
/// Exit-Code, wenn das Kind nicht gestartet oder nicht abgewartet werden kann (`EX_OSERR`).
pub const EXIT_CHILD_FAILED: u8 = 71;
/// Exit-Code, wenn `<cmd>` nicht existiert (Shell-Konvention).
pub const EXIT_CHILD_NOT_FOUND: u8 = 127;
/// Exit-Code, wenn `<cmd>` nicht ausführbar ist (Shell-Konvention).
pub const EXIT_CHILD_NOT_EXECUTABLE: u8 = 126;
/// Standardgrenze gleichzeitiger Verbindungen im Binary.
pub const DEFAULT_RELAY_MAX_CONNECTIONS: usize = 128;

/// Kurzhilfe für stderr bei [`RelayError::Usage`].
pub const RELAY_USAGE: &str =
    "Aufruf: harw-netns-relay <tcp-port> <unix-socket-path> [-- <cmd> [args…]]";

/// Rückkanal für Fehler, die nicht zum Abbruch des Relays führen.
///
/// # Concurrency
/// Wird aus Verbindungsthreads aufgerufen und muss daher `Send + Sync` sein.
pub type RelayReporter = Arc<dyn Fn(&RelayError) + Send + Sync>;

/// Argv eines Kindprozesses im Kindmodus (nach `--`).
///
/// # Description
/// Wird unverändert an [`std::process::Command`] übergeben — kein `sh -c`,
/// keine Expansion, kein Quoting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildCommand {
    /// Programm (`<cmd>`), wie übergeben; Suche über `PATH` wie bei `Command::new`.
    pub program: OsString,
    /// Argumente (`[args…]`) in Originalreihenfolge.
    pub args: Vec<OsString>,
}

/// Geparste Relay-Konfiguration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayConfig {
    /// TCP-Port auf `127.0.0.1` (`1..=65535`).
    pub port: u16,
    /// Pfad des Unix-Sockets des Egress-Proxys.
    pub proxy_socket: PathBuf,
    /// Kindprozess im Kindmodus; `None` = reiner Weiterleitungsmodus.
    pub command: Option<ChildCommand>,
}

impl RelayConfig {
    /// Parst die Relay-Argumente ohne Programmnamen.
    ///
    /// # Description
    /// Erwartet genau `<tcp-port> <unix-socket-path>`, optional gefolgt von
    /// `--` und mindestens einem Kind-Argument (siehe Moduldoku). Weitere
    /// Relay-Argumente, Optionen oder ein leeres Kommando nach `--` sind
    /// Fehler. Argumente nach `--` bleiben `OsString` (auch Nicht-UTF-8).
    ///
    /// # Arguments
    /// - `args` (`IntoIterator<Item = OsString>`): z. B.
    ///   `std::env::args_os().skip(1)`.
    ///
    /// # Returns
    /// Die [`RelayConfig`].
    ///
    /// # Errors
    /// - [`RelayError::Usage`]: fehlende/überzählige Argumente, Port ungültig
    ///   oder `0`, leerer Socket-Pfad, leeres Kommando nach `--`.
    ///
    /// # Concurrency
    /// Reine Funktion.
    ///
    /// # Examples
    /// ```rust
    /// use harw_egress::RelayConfig;
    ///
    /// let cfg = RelayConfig::from_args(
    ///     ["4444", "/run/egress.sock", "--", "geckodriver", "--port", "4445"]
    ///         .map(std::ffi::OsString::from),
    /// )
    /// .unwrap();
    /// assert_eq!(cfg.port, 4444);
    /// assert_eq!(cfg.command.unwrap().args.len(), 2);
    /// ```
    pub fn from_args<I>(args: I) -> Result<Self, RelayError>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut args = args.into_iter();
        let port_arg = args.next().ok_or_else(|| usage("<tcp-port> fehlt"))?;
        let port = parse_port(&port_arg)?;
        let socket_arg = args
            .next()
            .ok_or_else(|| usage("<unix-socket-path> fehlt"))?;
        if socket_arg.is_empty() || socket_arg == "--" {
            return Err(usage("<unix-socket-path> fehlt oder ist leer"));
        }
        let command = match args.next() {
            None => None,
            Some(separator) if separator == "--" => {
                let program = args.next().ok_or_else(|| usage("nach `--` fehlt <cmd>"))?;
                if program.is_empty() {
                    return Err(usage("<cmd> nach `--` ist leer"));
                }
                Some(ChildCommand {
                    program,
                    args: args.collect(),
                })
            }
            Some(_) => {
                return Err(usage("unerwartetes Argument; Kindprozess nur nach `--`"));
            }
        };
        Ok(Self {
            port,
            proxy_socket: PathBuf::from(socket_arg),
            command,
        })
    }
}

// Port-Argument: Dezimalzahl 1..=65535.
fn parse_port(raw: &OsStr) -> Result<u16, RelayError> {
    let text = raw
        .to_str()
        .ok_or_else(|| usage("<tcp-port> ist kein UTF-8"))?;
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(usage("<tcp-port> muss eine Dezimalzahl sein"));
    }
    match text.parse::<u16>() {
        Ok(0) | Err(_) => Err(usage("<tcp-port> muss zwischen 1 und 65535 liegen")),
        Ok(port) => Ok(port),
    }
}

fn usage(reason: &str) -> RelayError {
    RelayError::Usage {
        reason: reason.to_owned(),
    }
}

/// Bindet den Relay-Listener auf `127.0.0.1:<port>`.
///
/// # Description
/// Die Adresse ist fest Loopback; das Relay lauscht nie auf anderen
/// Interfaces.
///
/// # Arguments
/// - `port` (`u16`): TCP-Port.
///
/// # Returns
/// Den gebundenen `std::net::TcpListener`.
///
/// # Errors
/// - [`RelayError::Bind`]: Bind fehlgeschlagen (Port belegt, keine Rechte).
///
/// # Concurrency
/// Blockierungsfreier Systemaufruf.
///
/// # Examples
/// ```rust,no_run
/// let listener = harw_egress::bind_relay(1080).unwrap();
/// # drop(listener);
/// ```
pub fn bind_relay(port: u16) -> Result<TcpListener, RelayError> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpListener::bind(addr).map_err(|source| RelayError::Bind { addr, source })
}

/// Leitet Verbindungen des Listeners dauerhaft an den Proxy-Socket weiter.
///
/// # Description
/// Nimmt Verbindungen in einer Schleife an. Je Verbindung startet ein Thread
/// [`relay_connection`]. Ist `max_connections` erreicht, wird die neue
/// Verbindung sofort geschlossen und [`RelayError::ConnectionLimit`]
/// gemeldet. `accept`-Fehler werden gemeldet; danach wartet die Schleife
/// kurz und macht weiter.
///
/// # Arguments
/// - `listener` (`TcpListener`): gebundener Loopback-Listener (Eigentum).
/// - `proxy_socket` (`&Path`): Unix-Socket des Egress-Proxys.
/// - `max_connections` (`usize`): Obergrenze gleichzeitiger Verbindungen.
/// - `report` ([`RelayReporter`]): Empfänger nicht fataler Fehler.
///
/// # Returns
/// Kehrt nie zurück ([`Infallible`]).
///
/// # Concurrency
/// Blockiert den aufrufenden Thread; startet Threads je Verbindung.
///
/// # Examples
/// Siehe Moduldoku.
pub fn run_relay(
    listener: TcpListener,
    proxy_socket: &Path,
    max_connections: usize,
    report: RelayReporter,
) -> Infallible {
    let proxy_socket: Arc<Path> = Arc::from(proxy_socket);
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        let client = match listener.accept() {
            Ok((client, _peer)) => client,
            Err(source) => {
                report(&RelayError::Accept(source));
                thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
        };
        let Some(slot) = ConnectionSlot::acquire(&active, max_connections) else {
            report(&RelayError::ConnectionLimit {
                max: max_connections,
            });
            drop(client);
            continue;
        };
        let socket = Arc::clone(&proxy_socket);
        let conn_report = Arc::clone(&report);
        let spawned = thread::Builder::new()
            .name("harw-relay-conn".to_owned())
            .spawn(move || {
                let _slot = slot;
                if let Err(err) = relay_connection(client, &socket) {
                    conn_report(&err);
                }
            });
        if let Err(source) = spawned {
            report(&RelayError::Thread { source });
        }
    }
}

// Zählt aktive Verbindungen; gibt den Platz beim Drop frei.
struct ConnectionSlot(Arc<AtomicUsize>);

impl ConnectionSlot {
    fn acquire(active: &Arc<AtomicUsize>, max: usize) -> Option<Self> {
        // Compare-and-swap von Hand statt `fetch_update`/`try_update`: die
        // eine Methode ist auf der aktuellen Toolchain veraltet, die andere
        // erst ab Rust 1.95 stabil — die MSRV des Projekts ist 1.85.
        let mut current = active.load(Ordering::SeqCst);
        loop {
            if current >= max {
                return None;
            }
            match active.compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return Some(Self(Arc::clone(active))),
                Err(observed) => current = observed,
            }
        }
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Leitet eine einzelne TCP-Verbindung 1:1 an den Proxy-Socket weiter.
///
/// # Description
/// Verbindet sich mit `proxy_socket` und kopiert in beide Richtungen. Endet
/// eine Richtung (EOF), wird die Schreibseite des Gegenübers halb
/// geschlossen (`shutdown(Write)`), die andere Richtung läuft weiter. Die
/// Funktion kehrt zurück, wenn beide Richtungen beendet sind. Nutzdaten
/// werden weder gelesen noch protokolliert.
///
/// # Arguments
/// - `client` (`TcpStream`): angenommene Verbindung (Eigentum).
/// - `proxy_socket` (`&Path`): Unix-Socket des Egress-Proxys.
///
/// # Returns
/// `Ok(())`, wenn beide Richtungen ohne I/O-Fehler endeten.
///
/// # Errors
/// - [`RelayError::ProxyConnect`]: Proxy-Socket nicht erreichbar (Client wird geschlossen).
/// - [`RelayError::Copy`]: I/O-Fehler in einer Richtung (erster Fehler gewinnt).
/// - [`RelayError::Thread`] / [`RelayError::ThreadPanicked`]: Hilfsthread.
///
/// # Concurrency
/// Blockiert; startet genau einen Hilfsthread.
///
/// # Examples
/// ```rust,no_run
/// use std::net::TcpListener;
/// use std::path::Path;
///
/// let listener = TcpListener::bind("127.0.0.1:0").unwrap();
/// let (client, _) = listener.accept().unwrap();
/// harw_egress::relay_connection(client, Path::new("/run/harw/egress.sock")).unwrap();
/// ```
pub fn relay_connection(client: TcpStream, proxy_socket: &Path) -> Result<(), RelayError> {
    let proxy = UnixStream::connect(proxy_socket).map_err(|source| RelayError::ProxyConnect {
        path: proxy_socket.to_path_buf(),
        source,
    })?;
    let mut client_read = client.try_clone().map_err(|source| RelayError::Copy {
        direction: RelayDirection::ClientToProxy,
        source,
    })?;
    let mut proxy_write = proxy.try_clone().map_err(|source| RelayError::Copy {
        direction: RelayDirection::ClientToProxy,
        source,
    })?;
    let upstream = thread::Builder::new()
        .name("harw-relay-up".to_owned())
        .spawn(move || {
            let copied = io::copy(&mut client_read, &mut proxy_write);
            half_close_unix(&proxy_write);
            copied.map(|_| ()).map_err(|source| RelayError::Copy {
                direction: RelayDirection::ClientToProxy,
                source,
            })
        })
        .map_err(|source| RelayError::Thread { source })?;

    let mut proxy_read = proxy;
    let mut client_write = client;
    let downstream = io::copy(&mut proxy_read, &mut client_write)
        .map(|_| ())
        .map_err(|source| RelayError::Copy {
            direction: RelayDirection::ProxyToClient,
            source,
        });
    half_close_tcp(&client_write);

    let upstream = upstream.join().map_err(|_| RelayError::ThreadPanicked)?;
    downstream.and(upstream)
}

// Halb-Schließen nach EOF. Ein Fehler bedeutet, dass die Gegenseite bereits
// geschlossen ist (`NotConnected`); dann gibt es nichts mehr zu signalisieren,
// deshalb wird das Ergebnis bewusst verworfen.
fn half_close_tcp(stream: &TcpStream) {
    let _ = stream.shutdown(Shutdown::Write);
}

fn half_close_unix(stream: &UnixStream) {
    let _ = stream.shutdown(Shutdown::Write);
}

/// Startet das Kind und liefert seinen Exit-Code für das Relay.
///
/// # Description
/// `Command::new(program).args(args)` mit geerbter Umgebung und geerbtem
/// stdin/stdout/stderr, dann `wait`. Der Aufrufer muss den Listener **vorher**
/// gebunden und die Weiterleitung im Hintergrund gestartet haben.
///
/// # Arguments
/// - `command` (`&ChildCommand`): argv des Kindes.
///
/// # Returns
/// Exit-Code nach [`exit_code_from_status`].
///
/// # Errors
/// - [`RelayError::Spawn`]: Start fehlgeschlagen.
/// - [`RelayError::Wait`]: Warten fehlgeschlagen.
///
/// # Concurrency
/// Blockiert bis zum Ende des Kindes.
///
/// # Examples
/// ```rust,no_run
/// use harw_egress::{ChildCommand, run_child};
///
/// let code = run_child(&ChildCommand { program: "true".into(), args: Vec::new() }).unwrap();
/// assert_eq!(code, 0);
/// ```
pub fn run_child(command: &ChildCommand) -> Result<u8, RelayError> {
    let program = command.program.to_string_lossy().into_owned();
    let mut child = Command::new(&command.program)
        .args(&command.args)
        .spawn()
        .map_err(|source| RelayError::Spawn {
            program: program.clone(),
            source,
        })?;
    let status = child
        .wait()
        .map_err(|source| RelayError::Wait { program, source })?;
    Ok(exit_code_from_status(status))
}

/// Übersetzt den Status des Kindes in den Exit-Code des Relays.
///
/// # Description
/// Normales Ende → Exit-Code des Kindes; Ende durch Signal → `128 + Signal`
/// (gekappt auf 255); sonst `1`.
///
/// # Arguments
/// - `status` (`ExitStatus`): Status aus `wait`.
///
/// # Returns
/// Exit-Code `0..=255`.
///
/// # Concurrency
/// Reine Funktion.
///
/// # Examples
/// ```rust
/// use std::os::unix::process::ExitStatusExt;
/// use std::process::ExitStatus;
///
/// assert_eq!(harw_egress::exit_code_from_status(ExitStatus::from_raw(9)), 137);
/// assert_eq!(harw_egress::exit_code_from_status(ExitStatus::from_raw(3 << 8)), 3);
/// ```
#[must_use]
pub fn exit_code_from_status(status: ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        return u8::try_from(code).unwrap_or(1);
    }
    match status.signal() {
        Some(signal) => u8::try_from(128_i32.saturating_add(signal)).unwrap_or(u8::MAX),
        None => 1,
    }
}

/// Richtung einer Relay-Kopie (für Fehlermeldungen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayDirection {
    /// TCP-Client → Proxy-Socket.
    ClientToProxy,
    /// Proxy-Socket → TCP-Client.
    ProxyToClient,
}

impl fmt::Display for RelayDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClientToProxy => f.write_str("Client→Proxy"),
            Self::ProxyToClient => f.write_str("Proxy→Client"),
        }
    }
}

/// Fehler des netns-Relays.
///
/// # Description
/// Jede Variante trägt Adresse, Pfad oder Programm als Kontext, nie Nutzdaten.
#[derive(Debug)]
pub enum RelayError {
    /// Ungültige Kommandozeile.
    Usage {
        /// Was fehlt oder falsch ist.
        reason: String,
    },
    /// `127.0.0.1:<port>` konnte nicht gebunden werden.
    Bind {
        /// Die Bind-Adresse.
        addr: SocketAddr,
        /// Ursache.
        source: io::Error,
    },
    /// `accept` schlug fehl (nicht fatal).
    Accept(io::Error),
    /// Neue Verbindung verworfen, weil das Limit erreicht war.
    ConnectionLimit {
        /// Das Limit.
        max: usize,
    },
    /// Der Proxy-Socket war nicht erreichbar.
    ProxyConnect {
        /// Pfad des Sockets.
        path: PathBuf,
        /// Ursache.
        source: io::Error,
    },
    /// I/O-Fehler beim Kopieren.
    Copy {
        /// Betroffene Richtung.
        direction: RelayDirection,
        /// Ursache.
        source: io::Error,
    },
    /// Ein Thread konnte nicht gestartet werden.
    Thread {
        /// Ursache.
        source: io::Error,
    },
    /// Ein Kopier-Thread ist abgestürzt.
    ThreadPanicked,
    /// Das Kind konnte nicht gestartet werden.
    Spawn {
        /// Programmname (verlustbehaftet nach UTF-8).
        program: String,
        /// Ursache.
        source: io::Error,
    },
    /// Auf das Kind konnte nicht gewartet werden.
    Wait {
        /// Programmname (verlustbehaftet nach UTF-8).
        program: String,
        /// Ursache.
        source: io::Error,
    },
}

impl RelayError {
    /// Liefert den Exit-Code, mit dem das Binary bei diesem Fehler endet.
    ///
    /// # Returns
    /// [`EXIT_USAGE`], [`EXIT_BIND_FAILED`], [`EXIT_CHILD_NOT_FOUND`],
    /// [`EXIT_CHILD_NOT_EXECUTABLE`] oder [`EXIT_CHILD_FAILED`].
    ///
    /// # Concurrency
    /// Reine Funktion.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage { .. } => EXIT_USAGE,
            Self::Bind { .. } => EXIT_BIND_FAILED,
            Self::Spawn { source, .. } => match source.kind() {
                io::ErrorKind::NotFound => EXIT_CHILD_NOT_FOUND,
                io::ErrorKind::PermissionDenied => EXIT_CHILD_NOT_EXECUTABLE,
                _ => EXIT_CHILD_FAILED,
            },
            Self::Accept(_)
            | Self::ConnectionLimit { .. }
            | Self::ProxyConnect { .. }
            | Self::Copy { .. }
            | Self::Thread { .. }
            | Self::ThreadPanicked
            | Self::Wait { .. } => EXIT_CHILD_FAILED,
        }
    }
}

impl fmt::Display for RelayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage { reason } => write!(f, "ungültige Argumente: {reason}"),
            Self::Bind { addr, source } => write!(f, "Bind auf {addr} fehlgeschlagen: {source}"),
            Self::Accept(source) => write!(f, "accept fehlgeschlagen: {source}"),
            Self::ConnectionLimit { max } => {
                write!(
                    f,
                    "Verbindung verworfen: Limit von {max} gleichzeitigen Verbindungen"
                )
            }
            Self::ProxyConnect { path, source } => {
                write!(
                    f,
                    "Egress-Proxy {} nicht erreichbar: {source}",
                    path.display()
                )
            }
            Self::Copy { direction, source } => {
                write!(f, "Weiterleitung {direction} fehlgeschlagen: {source}")
            }
            Self::Thread { source } => write!(f, "Thread nicht startbar: {source}"),
            Self::ThreadPanicked => f.write_str("Kopier-Thread abgestürzt"),
            Self::Spawn { program, source } => {
                write!(f, "Kindprozess {program:?} nicht startbar: {source}")
            }
            Self::Wait { program, source } => {
                write!(
                    f,
                    "Warten auf Kindprozess {program:?} fehlgeschlagen: {source}"
                )
            }
        }
    }
}

impl std::error::Error for RelayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Bind { source, .. }
            | Self::ProxyConnect { source, .. }
            | Self::Copy { source, .. }
            | Self::Thread { source }
            | Self::Spawn { source, .. }
            | Self::Wait { source, .. }
            | Self::Accept(source) => Some(source),
            Self::Usage { .. } | Self::ConnectionLimit { .. } | Self::ThreadPanicked => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::time::Duration;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn test_from_args_forward_mode_without_separator() -> TestResult {
        let cfg =
            RelayConfig::from_args(os(&["1080", "/run/egress.sock"])).map_err(ctx("gültig"))?;
        assert_eq!(cfg.port, 1080);
        assert_eq!(cfg.proxy_socket, PathBuf::from("/run/egress.sock"));
        assert_eq!(cfg.command, None);
        Ok(())
    }

    #[test]
    fn test_from_args_child_mode_passes_argv_verbatim() -> TestResult {
        let cfg = RelayConfig::from_args(os(&[
            "4444",
            "/run/egress.sock",
            "--",
            "geckodriver",
            "--",
            "a b",
            "$HOME",
        ]))
        .map_err(ctx("gültig"))?;
        let command = cfg.command.ok_or(TestError::Missing("Kindmodus"))?;
        assert_eq!(command.program, OsString::from("geckodriver"));
        assert_eq!(command.args, os(&["--", "a b", "$HOME"]));
        Ok(())
    }

    #[test]
    fn test_from_args_rejects_invalid_invocations() {
        let cases: &[&[&str]] = &[
            &[],
            &["1080"],
            &["1080", "/run/egress.sock", "--"],
            &["1080", "/run/egress.sock", "--", ""],
            &["1080", "/run/egress.sock", "extra"],
            &["1080", "--", "cmd"],
            &["1080", ""],
            &["0", "/run/egress.sock"],
            &["65536", "/run/egress.sock"],
            &["+80", "/run/egress.sock"],
            &["--help"],
        ];
        for args in cases {
            let result = RelayConfig::from_args(os(args));
            assert!(
                matches!(result, Err(RelayError::Usage { .. })),
                "{args:?}: {result:?}"
            );
        }
    }

    #[test]
    fn test_exit_code_from_status_code_and_signal() {
        assert_eq!(exit_code_from_status(ExitStatus::from_raw(0)), 0);
        assert_eq!(exit_code_from_status(ExitStatus::from_raw(42 << 8)), 42);
        assert_eq!(exit_code_from_status(ExitStatus::from_raw(9)), 137);
        assert_eq!(exit_code_from_status(ExitStatus::from_raw(15)), 143);
    }

    #[test]
    fn test_run_child_forwards_exit_code() -> TestResult {
        // Laufzeitprüfung: ohne /bin/true bzw. /bin/false ist nichts prüfbar.
        let (true_bin, false_bin) = (Path::new("/bin/true"), Path::new("/bin/false"));
        if !true_bin.exists() || !false_bin.exists() {
            eprintln!("übersprungen: /bin/true oder /bin/false fehlt");
            return Ok(());
        }
        let ok = ChildCommand {
            program: true_bin.into(),
            args: Vec::new(),
        };
        let fail = ChildCommand {
            program: false_bin.into(),
            args: Vec::new(),
        };
        assert_eq!(run_child(&ok).map_err(ctx("startbar"))?, 0);
        assert_eq!(run_child(&fail).map_err(ctx("startbar"))?, 1);
        Ok(())
    }

    #[test]
    fn test_run_child_missing_program_maps_to_127() -> TestResult {
        let missing = ChildCommand {
            program: "/nonexistent/harw-relay-test-binary".into(),
            args: Vec::new(),
        };
        let Err(err) = run_child(&missing) else {
            return Err(TestError::Unexpected(
                "existiert nicht: Err erwartet".into(),
            ));
        };
        assert!(matches!(err, RelayError::Spawn { .. }), "{err:?}");
        assert_eq!(err.exit_code(), EXIT_CHILD_NOT_FOUND);
        Ok(())
    }

    #[test]
    fn test_bind_relay_binds_loopback_only() -> TestResult {
        let port = bind_relay_any_port()?
            .local_addr()
            .map_err(ctx("lokale Adresse"))?
            .port();
        let listener = bind_relay(port).map_err(ctx("freier Port"))?;
        let addr = listener.local_addr().map_err(ctx("lokale Adresse"))?;
        assert_eq!(addr, SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
        let Err(err) = bind_relay(port) else {
            return Err(TestError::Unexpected("Port belegt: Err erwartet".into()));
        };
        assert!(matches!(err, RelayError::Bind { .. }), "{err:?}");
        assert_eq!(err.exit_code(), EXIT_BIND_FAILED);
        Ok(())
    }

    // Ephemerer Loopback-Listener für Tests (fester Port wäre nicht parallel-sicher).
    fn bind_relay_any_port() -> TestResult<TcpListener> {
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(ctx("ephemerer Loopback-Port"))
    }

    // Unix-Socket-Gegenstelle: liest bis EOF und antwortet mit `pong:` + Daten.
    fn fake_proxy(path: &Path) -> TestResult<thread::JoinHandle<TestResult>> {
        let listener = UnixListener::bind(path).map_err(ctx("Unix-Socket binden"))?;
        Ok(thread::spawn(move || {
            let (mut stream, _) = listener.accept().map_err(ctx("accept"))?;
            let mut received = Vec::new();
            stream
                .read_to_end(&mut received)
                .map_err(ctx("lesen bis EOF"))?;
            stream.write_all(b"pong:").map_err(ctx("schreiben"))?;
            stream.write_all(&received).map_err(ctx("schreiben"))?;
            Ok(())
        }))
    }

    #[test]
    fn test_run_relay_forwards_with_half_close() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let socket = dir.path().join("egress.sock");
        let proxy = fake_proxy(&socket)?;
        let listener = bind_relay_any_port()?;
        let addr = listener.local_addr().map_err(ctx("lokale Adresse"))?;
        let (tx, rx) = mpsc::channel::<String>();
        let report: RelayReporter = Arc::new(move |err: &RelayError| {
            // Testkanal; ein geschlossener Empfänger ist hier bedeutungslos.
            let _ = tx.send(err.to_string());
        });
        let relay_socket = socket.clone();
        thread::spawn(move || run_relay(listener, &relay_socket, 4, report));

        let mut client = TcpStream::connect(addr).map_err(ctx("verbinden"))?;
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(ctx("Timeout setzen"))?;
        client.write_all(b"ping").map_err(ctx("schreiben"))?;
        client
            .shutdown(Shutdown::Write)
            .map_err(ctx("halb schließen"))?;
        let mut answer = Vec::new();
        client.read_to_end(&mut answer).map_err(ctx("lesen"))?;
        assert_eq!(answer, b"pong:ping");
        proxy
            .join()
            .map_err(|_| TestError::Unexpected("Fake-Proxy: Thread panicked".into()))??;
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "kein Fehler erwartet"
        );
        Ok(())
    }

    #[test]
    fn test_relay_connection_missing_proxy_socket_is_error() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let socket = dir.path().join("missing.sock");
        let listener = bind_relay_any_port()?;
        let addr = listener.local_addr().map_err(ctx("lokale Adresse"))?;
        let client = thread::spawn(move || -> TestResult<io::Result<usize>> {
            let mut stream = TcpStream::connect(addr).map_err(ctx("verbinden"))?;
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .map_err(ctx("Timeout setzen"))?;
            let mut buf = Vec::new();
            Ok(stream.read_to_end(&mut buf).map(|_| buf.len()))
        });
        let (accepted, _) = listener.accept().map_err(ctx("accept"))?;
        let Err(err) = relay_connection(accepted, &socket) else {
            return Err(TestError::Unexpected("Socket fehlt: Err erwartet".into()));
        };
        assert!(matches!(err, RelayError::ProxyConnect { .. }), "{err:?}");
        let read = client
            .join()
            .map_err(|_| TestError::Unexpected("Client-Thread: Thread panicked".into()))??;
        assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
        Ok(())
    }

    #[test]
    fn test_connection_slot_enforces_limit() -> TestResult {
        let active = Arc::new(AtomicUsize::new(0));
        let first = ConnectionSlot::acquire(&active, 1).ok_or(TestError::Missing("frei"))?;
        assert!(ConnectionSlot::acquire(&active, 1).is_none());
        drop(first);
        assert!(ConnectionSlot::acquire(&active, 1).is_some());
        assert_eq!(active.load(Ordering::SeqCst), 0);
        Ok(())
    }
}
