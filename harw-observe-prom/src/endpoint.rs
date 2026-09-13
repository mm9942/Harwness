//! Loopback-/Unix-Socket-HTTP-Endpunkt, über den [`crate::PromSink`]
//! gescrapt wird.
//!
//! # Verantwortungsbereich
//! Trägt [`PromEndpoint`] und [`BindAddr`]. Bindet einen Socket, spawnt
//! einen Hintergrund-Thread, der genau einen Pfad (`/metrics`) mit genau
//! einer Textantwort (`PromSink::render()`) beantwortet, und hält den
//! Thread bis zum `Drop` von [`PromEndpoint`] am Leben. Kennt kein anderes
//! Protokoll und keinen anderen Pfad.
//!
//! # Warum kein `hyper`/`tokio` und keine dritte HTTP-Bibliothek
//! `harw-mcp-server` benutzt `hyper` + `hyper-util` + `http-body-util` +
//! `tokio` für ein vollwertiges, sitzungsbehaftetes JSON-RPC-über-HTTP-
//! Protokoll (Streamable HTTP, mehrere Methoden, Server-Sent Events). Dieser
//! Endpunkt beantwortet dagegen **genau einen** Pfad mit **genau einer**
//! Textantwort auf `GET` — kein Request-Body, kein Streaming, keine
//! Sitzungen. Einen async-Runtime-Unterbau (`tokio`) plus einen HTTP/1-
//! Stack (`hyper`) dafür in den Baum zu ziehen, wäre eine vierte
//! Bibliothek für ein Fünfzeiler-Protokoll — und würde jedem Prozess, der
//! diesen Endpunkt nur zum Metrik-Export einbindet, eine async-Runtime
//! aufzwingen, obwohl Teile des Kerns bewusst ohne sie laufen
//! (`harw-observe::sink`-Moduldoc: „der Kern hat Pfade ohne Runtime
//! (`harw-sentinel` läuft ohne `tokio`)"). Die minimale
//! Request-Zeilen-Auswertung in [`build_response`] plus ein
//! `std::thread::spawn`-Hintergrund-Thread pro Socket (Muster: CLAUDE.md-
//! Konzept „bevorzugt `std::thread::spawn` + `JoinHandle` für CPU-arme
//! parallele Arbeit ohne async-Overhead") decken den geforderten Umfang
//! vollständig ab, ohne eine einzige zusätzliche Abhängigkeit.
//!
//! # Warum nur Loopback
//! [`BindAddr`] hat **keine** Variante, die eine externe Adresse
//! ausdrückt — nur `Loopback(u16)` (bindet stets `127.0.0.1:<port>`) und
//! `UnixSocket(PathBuf)` (ein Unix-Domain-Socket ist per Bauart nicht
//! netzwerkerreichbar). Das ist der Typ-Zwang: es gibt keinen Wert von
//! `BindAddr`, der `bind()` dazu brächte, auf `0.0.0.0` oder eine andere
//! externe Schnittstelle zu binden. Ein Metrikendpunkt auf einer externen
//! Adresse würde einem Netzscanner unmoderiert die Innenstruktur des
//! Systems verraten (Metriknamen, Label-Werte, Zählstände); die
//! Voreinstellung entscheidet hier, was in der Praxis passiert, deshalb ist
//! „nur Loopback" keine Option, sondern die einzige Möglichkeit.
//! [`require_loopback`] prüft das zusätzlich zur Laufzeit, bevor `bind()`
//! den Socket tatsächlich öffnet — reine Tiefenverteidigung, die unter den
//! heutigen zwei `BindAddr`-Varianten nie fehlschlagen kann (siehe
//! [`crate::PromError::NonLoopbackBind`]-Doc), aber direkt gegen die private
//! Funktion getestet ist.
//!
//! # Nebenläufigkeit
//! [`PromEndpoint::bind`] spawnt genau einen `std::thread`, der in einer
//! nichtblockierenden Annahme-Schleife auf Verbindungen wartet und den
//! gemeinsamen `Arc<PromSink>` pro Verbindung liest. `Drop` setzt ein
//! `AtomicBool`-Stoppsignal und joint den Thread, damit kein
//! Test/Einbetter einen losgelösten Thread zurücklässt (CLAUDE.md: „immer
//! alle gespawnten Handles joinen").
//!
//! # Fehler
//! [`crate::PromError`] aus [`PromEndpoint::bind`]. Verbindungs- oder
//! Protokollfehler *nach* erfolgreichem Bind (Lesefehler, defektes
//! Request, Schreibfehler) scheitern nie sichtbar — sie werden gezählt
//! ([`PromEndpoint::serve_error_count`]) statt den Hintergrund-Thread zu
//! beenden, nach demselben Muster wie
//! `harw_observe_file::FileSink::write_error_count`.
//!
//! # Examples
//! ```rust,no_run
//! use harw_observe::{Cardinality, MetricKey, MetricKind, MetricValue, TelemetrySink, Unit};
//! use harw_observe_prom::{BindAddr, PromEndpoint};
//!
//! let endpoint = PromEndpoint::bind(BindAddr::Loopback(9327)).expect("port free");
//! const KEY: MetricKey = MetricKey {
//!     name: "harw_jobs_completed_total",
//!     kind: MetricKind::Counter,
//!     unit: Unit::Count,
//!     labels: &[],
//!     cardinality: Cardinality::Single,
//! };
//! endpoint.sink().record(&KEY, MetricValue::Count(1), &[]);
//! // Ein Scraper fragt jetzt `GET http://127.0.0.1:9327/metrics` ab.
//! ```

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::error::PromError;
use crate::sink::PromSink;

/// Der einzige Pfad, den dieser Endpunkt beantwortet.
const METRICS_PATH: &str = "/metrics";
/// Wartezeit zwischen zwei Annahme-Versuchen, solange kein Client wartet —
/// hält die Reaktionszeit auf ein `Drop`-Stoppsignal klein, ohne den
/// Hintergrund-Thread aktiv im Kreis laufen zu lassen.
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Zeitlimit für Lesen/Schreiben je Verbindung — ein hängender Client darf
/// den Hintergrund-Thread nicht unbegrenzt blockieren.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
/// Obergrenze für eine einzelne Request-Leseoperation. Ein `GET
/// /metrics`-Scrape ohne Body passt bequem hinein; diese minimale
/// Implementierung liest bewusst nur einmal (kein Nachlesen bei
/// Teil-Requests) — angemessen für den vorgesehenen Anwendungsfall
/// (Scraper, kein allgemeiner HTTP-Client).
const REQUEST_BUFFER_BYTES: usize = 8 * 1024;

/// Die Bindeadresse eines [`PromEndpoint`].
///
/// Hat bewusst **keine** Variante, die eine externe (Nicht-Loopback-)
/// Adresse ausdrücken kann (siehe Moduldoc, Abschnitt „Warum nur
/// Loopback"). Ein `match` über `BindAddr` ist erschöpfend mit genau diesen
/// zwei Armen — das ist der Beweis, dass keine dritte, externe Variante
/// existiert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindAddr {
    /// TCP auf `127.0.0.1:<port>`. Port `0` lässt das Betriebssystem einen
    /// freien Port wählen ([`PromEndpoint::local_addr`] liefert den
    /// tatsächlich gebundenen danach).
    Loopback(u16),
    /// Ein Unix-Domain-Socket unter diesem Dateisystempfad. Ein bereits
    /// vorhandener, verwaister Socket an diesem Pfad wird vor dem Binden
    /// entfernt (siehe [`PromEndpoint::bind`]); eine gewöhnliche Datei an
    /// diesem Pfad bleibt unangetastet und lässt das Binden fehlschlagen.
    UnixSocket(PathBuf),
}

/// Ein gebundener, laufender Prometheus-Scrape-Endpunkt.
///
/// Besitzt einen eigenen [`PromSink`] (über [`PromEndpoint::sink`]
/// erreichbar, um ihn als [`harw_observe::TelemetrySink`] an den Rest des
/// Prozesses zu reichen) und einen Hintergrund-Thread, der `GET /metrics`
/// mit dessen [`PromSink::render`]-Ausgabe beantwortet. `Drop` stoppt den
/// Thread und joint ihn.
pub struct PromEndpoint {
    sink: Arc<PromSink>,
    local_tcp_addr: Option<SocketAddr>,
    stop: Arc<AtomicBool>,
    serve_errors: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl PromEndpoint {
    /// Bindet und startet den Endpunkt.
    ///
    /// # Description
    /// Baut einen leeren [`PromSink`], bindet den durch `addr`
    /// beschriebenen Socket synchron (der Aufruf kehrt erst zurück, wenn
    /// das Betriebssystem den Socket tatsächlich angenommen hat) und
    /// spawnt danach genau einen Hintergrund-Thread mit der
    /// Annahme-Schleife.
    ///
    /// # Arguments
    /// - `addr` (`BindAddr`): TCP-Loopback mit Port oder ein
    ///   Unix-Socket-Pfad.
    ///
    /// # Returns
    /// Den laufenden `PromEndpoint`.
    ///
    /// # Errors
    /// - [`PromError::NonLoopbackBind`]: Tiefenverteidigung, unter den
    ///   heutigen `BindAddr`-Varianten praktisch unerreichbar (siehe
    ///   Moduldoc, Abschnitt „Warum nur Loopback").
    /// - [`PromError::Bind`]: die TCP-Adresse ist belegt, der
    ///   Unix-Socket-Pfad ist nicht anlegbar, oder der
    ///   Nichtblockierend-Modus ließ sich nicht setzen.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_observe_prom::{BindAddr, PromEndpoint};
    ///
    /// let endpoint = PromEndpoint::bind(BindAddr::Loopback(0)).unwrap();
    /// assert!(endpoint.local_addr().is_some());
    /// ```
    pub fn bind(addr: BindAddr) -> Result<Self, PromError> {
        let sink = Arc::new(PromSink::new());
        let stop = Arc::new(AtomicBool::new(false));
        let serve_errors = Arc::new(AtomicU64::new(0));

        match addr {
            BindAddr::Loopback(port) => {
                let socket_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
                require_loopback(socket_addr)?;

                let listener = TcpListener::bind(socket_addr).map_err(|source| PromError::Bind {
                    address: socket_addr.to_string(),
                    source,
                })?;
                let local_tcp_addr = listener.local_addr().map_err(|source| PromError::Bind {
                    address: socket_addr.to_string(),
                    source,
                })?;
                listener
                    .set_nonblocking(true)
                    .map_err(|source| PromError::Bind {
                        address: local_tcp_addr.to_string(),
                        source,
                    })?;

                let handle = spawn_tcp_server(listener, Arc::clone(&sink), Arc::clone(&stop), Arc::clone(&serve_errors));

                Ok(Self {
                    sink,
                    local_tcp_addr: Some(local_tcp_addr),
                    stop,
                    serve_errors,
                    handle: Some(handle),
                })
            }
            BindAddr::UnixSocket(path) => {
                remove_stale_socket(&path);

                let listener = UnixListener::bind(&path).map_err(|source| PromError::Bind {
                    address: path.display().to_string(),
                    source,
                })?;
                listener
                    .set_nonblocking(true)
                    .map_err(|source| PromError::Bind {
                        address: path.display().to_string(),
                        source,
                    })?;

                let handle = spawn_unix_server(listener, Arc::clone(&sink), Arc::clone(&stop), Arc::clone(&serve_errors));

                Ok(Self {
                    sink,
                    local_tcp_addr: None,
                    stop,
                    serve_errors,
                    handle: Some(handle),
                })
            }
        }
    }

    /// Der Sink dieses Endpunkts.
    ///
    /// # Returns
    /// Einen geteilten Zeiger (`Arc::clone`, keine Kopie des Zustands) auf
    /// den [`PromSink`], den dieser Endpunkt ausliefert — als
    /// [`harw_observe::TelemetrySink`] an den Rest des Prozesses reichbar.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_observe_prom::{BindAddr, PromEndpoint};
    ///
    /// let endpoint = PromEndpoint::bind(BindAddr::Loopback(0)).unwrap();
    /// let _sink = endpoint.sink();
    /// ```
    #[must_use]
    pub fn sink(&self) -> Arc<PromSink> {
        Arc::clone(&self.sink)
    }

    /// Die tatsächlich gebundene TCP-Adresse.
    ///
    /// # Returns
    /// `Some(addr)` bei `BindAddr::Loopback` (nützlich, wenn Port `0` das
    /// Betriebssystem einen freien Port wählen ließ); `None` bei
    /// `BindAddr::UnixSocket`.
    #[must_use]
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.local_tcp_addr
    }

    /// Anzahl interner Verbindungs-/Protokollfehler seit `bind()`.
    ///
    /// # Returns
    /// Die kumulierte Fehlerzahl (siehe Moduldoc, Abschnitt „Fehler"):
    /// Annahme-, Lese- oder Schreibfehler nach dem Bind scheitern nie
    /// sichtbar, werden aber hier gezählt.
    #[must_use]
    pub fn serve_error_count(&self) -> u64 {
        self.serve_errors.load(Ordering::Relaxed)
    }
}

impl Drop for PromEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

// Tiefenverteidigung gegen eine Nicht-Loopback-Adresse (siehe Moduldoc,
// Abschnitt „Warum nur Loopback"). Unter den heutigen zwei
// `BindAddr`-Varianten immer `Ok`, weil `Loopback(port)` stets
// `127.0.0.1:<port>` baut — direkt getestet, weil `bind()` selbst diesen
// Zweig nie mit einem fehlschlagenden Wert erreicht.
fn require_loopback(address: SocketAddr) -> Result<(), PromError> {
    if address.ip().is_loopback() {
        Ok(())
    } else {
        Err(PromError::NonLoopbackBind {
            address: address.to_string(),
        })
    }
}

// Entfernt eine verwaiste Unix-Socket-Datei am Zielpfad, falls vorhanden.
// Prüft den Dateityp, bevor sie gelöscht wird: eine gewöhnliche Datei am
// selben Pfad (ein Konfigurationsfehler des Aufrufers) bleibt unangetastet
// und lässt `UnixListener::bind` mit einem klaren I/O-Fehler fehlschlagen,
// statt sie stillschweigend zu überschreiben.
fn remove_stale_socket(path: &std::path::Path) {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_socket() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn spawn_tcp_server(
    listener: TcpListener,
    sink: Arc<PromSink>,
    stop: Arc<AtomicBool>,
    errors: Arc<AtomicU64>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(CONNECTION_TIMEOUT));
                    let _ = stream.set_write_timeout(Some(CONNECTION_TIMEOUT));
                    handle_connection(&mut stream, &sink, &errors);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(POLL_INTERVAL);
                }
                Err(_) => {
                    errors.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(POLL_INTERVAL);
                }
            }
        }
    })
}

fn spawn_unix_server(
    listener: UnixListener,
    sink: Arc<PromSink>,
    stop: Arc<AtomicBool>,
    errors: Arc<AtomicU64>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(CONNECTION_TIMEOUT));
                    let _ = stream.set_write_timeout(Some(CONNECTION_TIMEOUT));
                    handle_connection(&mut stream, &sink, &errors);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(POLL_INTERVAL);
                }
                Err(_) => {
                    errors.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(POLL_INTERVAL);
                }
            }
        }
    })
}

// Liest genau einen Request, beantwortet ihn und lässt `stream` danach
// fallen (schließt die Verbindung — jede Antwort trägt `Connection: close`).
fn handle_connection<S: Read + Write>(stream: &mut S, sink: &PromSink, errors: &AtomicU64) {
    let mut buffer = [0_u8; REQUEST_BUFFER_BYTES];
    let read = match stream.read(&mut buffer) {
        Ok(0) => return,
        Ok(read) => read,
        Err(_) => {
            errors.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    let (head, body) = build_response(&buffer[..read], sink);
    if stream.write_all(head.as_bytes()).is_err() || stream.write_all(body.as_bytes()).is_err() {
        errors.fetch_add(1, Ordering::Relaxed);
    }
}

// Wertet nur die Request-Zeile aus (Methode + Pfad); alle Header werden
// ignoriert — angemessen für einen Endpunkt, der keine Authentifizierung,
// keine Inhaltsverhandlung und keinen Request-Body kennt.
fn build_response(request: &[u8], sink: &PromSink) -> (String, String) {
    let text = String::from_utf8_lossy(request);
    let mut parts = text.lines().next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    if method == "GET" && path == METRICS_PATH {
        let body = sink.render();
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        (head, body)
    } else if method == "GET" {
        (
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
            String::new(),
        )
    } else {
        (
            "HTTP/1.1 405 Method Not Allowed\r\nAllow: GET\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_owned(),
            String::new(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_observe::{Cardinality, MetricKey, MetricKind, MetricValue, TelemetrySink, Unit};
    use std::net::{IpAddr, Ipv4Addr as V4, TcpStream};

    const TEST_KEY: MetricKey = MetricKey {
        name: "harw_endpoint_test_total",
        kind: MetricKind::Counter,
        unit: Unit::Count,
        labels: &[],
        cardinality: Cardinality::Single,
    };

    fn read_all(stream: &mut impl Read) -> String {
        let mut buffer = Vec::new();
        let _ = stream.read_to_end(&mut buffer);
        String::from_utf8_lossy(&buffer).into_owned()
    }

    // Ein `match` über alle `BindAddr`-Varianten ohne Wildcard-Arm: kompiliert
    // nur, solange `BindAddr` ausschließlich `Loopback` und `UnixSocket`
    // trägt — der geforderte Beweis, dass keine externe Variante existiert.
    #[test]
    fn test_bind_addr_match_is_exhaustive_over_loopback_and_unix_only() {
        fn describe(addr: &BindAddr) -> &'static str {
            match addr {
                BindAddr::Loopback(_) => "loopback",
                BindAddr::UnixSocket(_) => "unix",
            }
        }
        assert_eq!(describe(&BindAddr::Loopback(0)), "loopback");
        assert_eq!(
            describe(&BindAddr::UnixSocket(PathBuf::from("/tmp/x.sock"))),
            "unix"
        );
    }

    #[test]
    fn test_require_loopback_accepts_ipv4_loopback() {
        let addr = SocketAddr::from((V4::LOCALHOST, 9327));
        assert!(require_loopback(addr).is_ok());
    }

    #[test]
    fn test_require_loopback_rejects_public_address() {
        let addr = SocketAddr::from((IpAddr::V4(V4::new(93, 184, 216, 34)), 80));
        let err = require_loopback(addr).expect_err("public address must be rejected");
        match err {
            PromError::NonLoopbackBind { address } => {
                assert_eq!(address, "93.184.216.34:80");
            }
            other => panic!("expected NonLoopbackBind, got {other}"),
        }
    }

    #[test]
    fn test_bind_loopback_serves_metrics_over_tcp() {
        let endpoint = PromEndpoint::bind(BindAddr::Loopback(0)).expect("loopback binds");
        let addr = endpoint.local_addr().expect("tcp endpoint has a local address");
        endpoint.sink().record(&TEST_KEY, MetricValue::Count(9), &[]);

        let mut stream = TcpStream::connect(addr).expect("connects to bound loopback port");
        stream
            .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("request writes");
        let response = read_all(&mut stream);

        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(response.contains("text/plain; version=0.0.4"), "{response}");
        assert!(response.contains("harw_endpoint_test_total 9"), "{response}");
        assert_eq!(endpoint.serve_error_count(), 0);
    }

    #[test]
    fn test_bind_loopback_returns_404_for_unknown_path() {
        let endpoint = PromEndpoint::bind(BindAddr::Loopback(0)).expect("loopback binds");
        let addr = endpoint.local_addr().expect("tcp endpoint has a local address");

        let mut stream = TcpStream::connect(addr).expect("connects to bound loopback port");
        stream
            .write_all(b"GET /other HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("request writes");
        let response = read_all(&mut stream);

        assert!(response.starts_with("HTTP/1.1 404 Not Found"), "{response}");
    }

    #[test]
    fn test_bind_loopback_returns_405_for_non_get_method() {
        let endpoint = PromEndpoint::bind(BindAddr::Loopback(0)).expect("loopback binds");
        let addr = endpoint.local_addr().expect("tcp endpoint has a local address");

        let mut stream = TcpStream::connect(addr).expect("connects to bound loopback port");
        stream
            .write_all(b"POST /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("request writes");
        let response = read_all(&mut stream);

        assert!(response.starts_with("HTTP/1.1 405 Method Not Allowed"), "{response}");
    }

    #[test]
    fn test_bind_unix_socket_serves_metrics_over_the_socket() {
        use std::os::unix::net::UnixStream;

        let dir = tempfile::tempdir().expect("temp dir creates");
        let path = dir.path().join("prom.sock");
        let endpoint =
            PromEndpoint::bind(BindAddr::UnixSocket(path.clone())).expect("unix socket binds");
        endpoint.sink().record(&TEST_KEY, MetricValue::Count(3), &[]);
        assert!(endpoint.local_addr().is_none());

        let mut stream = UnixStream::connect(&path).expect("connects to bound unix socket");
        stream
            .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("request writes");
        let response = read_all(&mut stream);

        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(response.contains("harw_endpoint_test_total 3"), "{response}");
    }

    #[test]
    fn test_bind_unix_socket_removes_stale_socket_file_left_by_a_previous_run() {
        let dir = tempfile::tempdir().expect("temp dir creates");
        let path = dir.path().join("prom.sock");

        {
            let _first = PromEndpoint::bind(BindAddr::UnixSocket(path.clone()))
                .expect("first bind succeeds");
            // `_first` is dropped at the end of this block, but its socket
            // file is not guaranteed to be unlinked by that alone; the
            // second `bind()` below must still succeed by removing it.
        }

        let _second = PromEndpoint::bind(BindAddr::UnixSocket(path)).expect("second bind succeeds");
    }

    #[test]
    fn test_drop_stops_background_thread_and_releases_the_tcp_port() {
        let endpoint = PromEndpoint::bind(BindAddr::Loopback(0)).expect("loopback binds");
        let addr = endpoint.local_addr().expect("tcp endpoint has a local address");
        drop(endpoint);

        // A listening socket has no lingering TIME_WAIT state of its own;
        // once `Drop` has joined the accept-loop thread, the port must be
        // immediately rebindable.
        let rebound = TcpListener::bind(addr);
        assert!(rebound.is_ok(), "port must be free after PromEndpoint is dropped");
    }
}
