//! Unix-Socket-HTTP-Transport — bindet, nimmt Verbindungen an, reicht an
//! [`crate::router::decide_resolved_route`] weiter.
//!
//! # Verantwortungsbereich
//! Dieses Modul enthält den einzigen Ort dieser Crate, der `hyper` und
//! einen echten Socket berührt. Es **entscheidet nichts selbst** — jede
//! Zugriffsentscheidung kommt von [`crate::router::decide_resolved_route`], jede
//! Berechtigungsstufe von [`crate::authz::PeerAuthorizer`], jede Identität
//! von [`crate::peer::read_peer_credentials`]. Dieses Modul übersetzt nur
//! zwischen HTTP-Semantik (Methode, Statuscode, JSON-Rumpf, SSE-Rahmen) und
//! den bereits getroffenen Entscheidungen der übrigen Module.
//!
//! # HTTP-Stack
//! `hyper` 1.x + `hyper-util` + `http-body-util`, exakt wie
//! `harw_mcp_server::transport` (Vorbild dieses Knotens) — dieselben Bausteine
//! (`hyper_util::server::conn::auto::Builder`, `TokioExecutor`,
//! `service_fn`, `tokio::task::JoinSet` für den geordneten Verbindungsabbau,
//! `http_body_util::Channel` für den SSE-Strom).
//!
//! # Warum keine dritte HTTP-Bibliothek
//! Ein zweiter HTTP-Stack im Workspace wäre genau die Vervielfachung, die
//! der Leitfaden verbietet — siehe `harw-web/Cargo.toml` für die exakt aus
//! `harw-mcp-server/Cargo.toml` übernommenen Versionsangaben.
//!
//! # `SO_PEERCRED` statt Bearer-Token
//! Jede angenommene Verbindung wird **einmal**, direkt nach `accept()`, über
//! [`crate::peer::read_peer_credentials`] identifiziert (siehe dortige
//! Moduldoku). Scheitert das, wird die Verbindung verworfen, bevor
//! irgendeine HTTP-Anfrage darauf bearbeitet wird — es gibt keinen Pfad, auf
//! dem eine Anfrage ohne bekannte Peer-Identität eine Route erreicht.
//!
//! # SSE mit Sequenznummer
//! `GET /events` liefert einen `text/event-stream` mit einem
//! [`crate::events::WebEvent`] je `data:`-Zeile — jedes trägt
//! `sequence: u64` (siehe `crate::events`-Moduldoku für die Begründung). Ein
//! periodischer Heartbeat (kein `Surface::Web`, keine Operation — reines
//! Betriebssignal dieses Moduls) hält die Sequenz auch ohne
//! Operationsaufrufe sichtbar am Leben.
//!
//! # Metarouten `/v1/*` (H9)
//! `GET /v1/health`, `/v1/version` und `/v1/capabilities` beantwortet
//! [`handle`] vor der Routentabelle über [`crate::meta`] — sie sind keine
//! Operationen. `/v1/health` braucht keine Stufe; die beiden anderen die
//! niedrigste ([`crate::meta::MINIMUM_DESCRIPTIVE_TIER`]) über denselben
//! [`crate::identity::LocalPeerIdentityResolver`] wie jede Route. Jede andere
//! Methode als `GET` → `405`.
//!
//! # Identitätsauflösung (H12)
//! Jede autorisierungspflichtige Anfrage (Operationsroute, `GET /events`,
//! `/v1/version`, `/v1/capabilities`) löst die Identität über den
//! [`crate::identity::LocalPeerIdentityResolver`] des Servers auf: aus den
//! `SO_PEERCRED` der Verbindung plus dem optionalen Kontextverweis
//! ([`crate::identity::SECURITY_CONTEXT_HEADER`], gelesen über
//! [`crate::identity::presented_context`]). Vorgabe ist
//! [`crate::identity::TierMapResolver`] über dem übergebenen
//! [`crate::authz::PeerAuthorizer`] — exakt das Verhalten vor H12;
//! [`BoundWebServer::with_identity_resolver`] ersetzt ihn (produktiv durch
//! [`crate::identity::WebIdentityConfig::build_resolver`]). Die
//! Routenentscheidung trifft [`crate::router::decide_resolved_route`];
//! scheitert die Auflösung, trägt die `403` den Grund
//! [`crate::identity::IdentityError::code`]. Bei `Execute` fädelt
//! [`scoped_context`] über
//! [`crate::identity::ResolvedPeer::scope_op_context`] Mandant und
//! Kontextzusammenfassung in den `OpContext` — nie aus dem Rumpf. Fehlt die
//! aufgelöste Identität, lehnt [`scoped_context`] mit `403` ab (R15/M1:
//! Rückfallpfade lehnen ab), statt den `OpContext` ungeschützt
//! (mandantenübergreifend) durchzureichen. Für unbekannte Pfade und falsche
//! Methoden (`404`/`405`) wird keine Identität aufgelöst: dort gibt es
//! nichts zu autorisieren, und ein Hub-Aufruf wäre reine Last.
//!
//! # Zwei Wege zum Listener
//! [`BoundWebServer::bind`] bindet einen Pfad selbst (siehe
//! „Socket-Pfad" unten); [`BoundWebServer::from_std_listener`] übernimmt
//! einen bereits lauschenden Listener — den von systemd übergebenen
//! ([`crate::systemd::listener_from_systemd`], `harw web --systemd-socket`).
//! Beide münden in dieselbe Annahmeschleife.
//!
//! # Zwei CI-Gates
//! „Keine Route ohne `OperationMeta`" und „Tier-Ablehnungsmatrix" (siehe
//! `crate::router`-Moduldoku) werden von diesem Modul **konsumiert**, nicht
//! implementiert: [`handle`] fragt ausschließlich
//! [`crate::router::decide_resolved_route`] (derselbe Prüfpfad wie
//! [`crate::router::decide_route`]) und führt nie eine Operation aus, die
//! dieses nicht als [`crate::router::RouteDecision::Execute`] freigegeben hat.
//!
//! # Nebenläufigkeit
//! Jede angenommene Verbindung läuft in einer eigenen `tokio::task` (über
//! ein `JoinSet`, damit [`BoundWebServer::serve_until`] beim Herunterfahren
//! alle Verbindungs-Tasks geordnet abbricht und einsammelt statt sie
//! herrenlos weiterlaufen zu lassen).
//!
//! # Belastbarkeit (P0.12, F-184/F-185)
//! - **Rumpfgrenze:** Jeder Anfragerumpf wird über
//!   [`http_body_util::Limited`] gelesen — die Grenze greift damit auch bei
//!   `Transfer-Encoding: chunked` ohne `Content-Length`, bevor mehr als
//!   `MAX_BODY_BYTES` im Speicher liegen (→ `413`).
//! - **Timeouts:** `header_read_timeout` am hyper-HTTP/1-Builder (mit
//!   [`hyper_util::rt::TokioTimer`]) schließt Verbindungen, die ihre Kopfzeilen
//!   nicht rechtzeitig senden — das gilt auch für untätige Keep-Alive-
//!   Verbindungen, weil hyper den Zeitgeber bei jedem neuen Kopf startet.
//!   Das Lesen des Rumpfs ist zusätzlich durch `tokio::time::timeout`
//!   begrenzt (→ `408`). Die Laufzeit einer Operation selbst und der
//!   SSE-Strom sind bewusst **nicht** begrenzt.
//! - **`accept()`-Fehler:** Vorübergehende Fehler (`EMFILE`, `ECONNABORTED`,
//!   `ENOBUFS`, …) werden protokolliert; die Annahme pausiert mit
//!   exponentiellem, gedeckeltem Backoff, der Server läuft weiter. Nur Fehler,
//!   die einen unbrauchbaren Listener belegen (`EBADF`, `ENOTSOCK`, `EINVAL`,
//!   `EFAULT`, `EOPNOTSUPP`), beenden die Schleife.
//! - **Socket-Pfad:** Eine vorhandene Datei wird nur entfernt, wenn sie ein
//!   Socket ist **und** ein `connect` mit `ECONNREFUSED` belegt, dass niemand
//!   mehr lauscht. Reguläre Dateien, Symlinks und lebende Sockets werden nie
//!   angefasst.
//!
//! # Fehlertypen
//! [`crate::error::WebError::Bind`], [`crate::error::WebError::SocketInUse`]
//! und [`crate::error::WebError::SocketPathOccupied`] beim Binden,
//! [`crate::error::WebError::Accept`] bei einem fatalen `accept()`-Fehler.
//! Fehler einzelner Anfragen
//! (JSON-Parsing, Operationsfehler) werden nie als [`crate::error::WebError`]
//! propagiert — sie werden als HTTP-Antwort mit passendem Statuscode
//! beantwortet (`handle` ist `Result<_, Infallible>`, wie bei
//! `harw_mcp_server::transport::handle`).
//!
//! # Beispiel
//! ```rust,no_run
//! use std::path::PathBuf;
//! use std::sync::Arc;
//!
//! use harw_operations::operation::PermissionTier;
//! use harw_operations::registry::OperationRegistry;
//! use harw_web::authz::StaticUidTierMap;
//! use harw_web::events::WebEventBus;
//! use harw_web::router::WebRouteTable;
//! use harw_web::server::{BoundWebServer, WebServerConfig};
//!
//! # async fn run() -> Result<(), harw_web::error::WebError> {
//! let registry = OperationRegistry::new();
//! let routes = WebRouteTable::from_registry(&registry)?;
//! let authz = Arc::new(StaticUidTierMap::with_default(vec![], PermissionTier::Observer));
//! let events = Arc::new(WebEventBus::new(64)?);
//! let context_factory: Arc<harw_web::server::WebContextFactory> =
//!     Arc::new(|_peer: &harw_web::peer::PeerCredentials, _tier: PermissionTier| {
//!         todo!("echte SandboxSpec/ServiceMap-Auflösung, nie aus dem Request selbst")
//!     });
//! let server = BoundWebServer::bind(
//!     WebServerConfig { socket_path: PathBuf::from("/run/harw/web.sock") },
//!     routes,
//!     authz,
//!     context_factory,
//!     events,
//! )
//! .await?;
//! server.serve().await
//! # }
//! ```

use std::convert::Infallible;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Channel, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::header::{ACCEPT, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use hyper::service::service_fn;
use hyper::{HeaderMap, Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tokio::task::JoinSet;

use harw_operations::context::OpContext;
use harw_operations::error::OpError;
use harw_operations::operation::PermissionTier;

use crate::authz::{PeerAuthorizer, tier_permits};
use crate::error::WebError;
use crate::events::{WebEventBus, WebEventKind, WebEventReceiveError};
use crate::identity::{
    IdentityError, LocalPeerIdentityResolver, ResolvedPeer, TierMapResolver, presented_context,
};
use crate::meta::MetaRoute;
use crate::peer::{PeerCredentials, read_peer_credentials};
use crate::router::{
    ForbiddenReason, RouteDecision, WebMethod, WebRouteTable, decide_resolved_route, method_name,
    parse_web_method,
};

/// Obergrenze eines HTTP-Rumpfs (Anfrage-JSON für eine `Surface::Web`-Route).
const MAX_BODY_BYTES: usize = 64 * 1024;
/// Der reservierte Pfad des SSE-Ereignisstroms — kollidiert nie mit einer
/// `Surface::Web`-Route, da `Surface::Web::path` von der jeweiligen
/// Operation kommt und `harw-web` diesen Pfad nie an
/// [`crate::router::WebRouteTable::from_registry`] übergibt.
pub(crate) const EVENTS_PATH: &str = "/events";
/// Abstand zwischen zwei [`WebEventKind::Heartbeat`]-Ereignissen.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// Frist, in der ein Client die Kopfzeilen einer Anfrage vollständig senden
/// muss — auch die nächste Anfrage einer untätigen Keep-Alive-Verbindung.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Frist, in der der (durch [`MAX_BODY_BYTES`] begrenzte) Rumpf vollständig
/// eingetroffen sein muss.
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Erste Pause nach einem vorübergehenden `accept()`-Fehler.
const ACCEPT_BACKOFF_BASE: Duration = Duration::from_millis(10);
/// Obergrenze der Pause nach wiederholten `accept()`-Fehlern.
const ACCEPT_BACKOFF_MAX: Duration = Duration::from_secs(1);
/// Frist für den Lebendigkeits-`connect` auf einen vorgefundenen Socket.
/// Läuft sie ab, gilt der Socket als lebend (volle Warteschlange) und wird
/// nicht entfernt.
const STALE_SOCKET_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

type WebResponse = Response<BoxBody<Bytes, Infallible>>;

/// Baut den [`OpContext`] für eine freigegebene Ausführung.
///
/// # Description
/// Wird **einmal pro Aufruf** unmittelbar vor [`harw_operations::adapter::WebAdapter::invoke`]
/// aufgerufen — nie vorher, damit kein `OpContext` für eine Route gebaut
/// wird, die [`crate::router::decide_route`] gar nicht freigegeben hat. Die
/// Implementierung MUSS `sandbox`/`services` aus serverseitig vertrauter
/// Konfiguration ableiten, niemals aus dem HTTP-Request.
pub type WebContextFactory =
    dyn Fn(&PeerCredentials, PermissionTier) -> OpContext + Send + Sync + 'static;

/// Konfiguration für [`BoundWebServer::bind`].
#[derive(Debug, Clone)]
pub struct WebServerConfig {
    /// Pfad des zu bindenden Unix-Sockets.
    pub socket_path: PathBuf,
}

/// Ein gebundener, noch nicht bedienender Unix-Socket-HTTP-Server.
///
/// # Description
/// `serve`/`serve_until` übernehmen die Annahmeschleife — der Aufrufer
/// (typischerweise ein Binary) entscheidet, unter welcher
/// Supervision/Cancellation-Policy sie läuft.
pub struct BoundWebServer {
    listener: UnixListener,
    socket_path: PathBuf,
    routes: Arc<WebRouteTable>,
    identity: Arc<dyn LocalPeerIdentityResolver>,
    context_factory: Arc<WebContextFactory>,
    events: Arc<WebEventBus>,
}

impl BoundWebServer {
    /// Bindet den Unix-Socket und komponiert Routentabelle, Autorisierung,
    /// Kontext-Fabrik und Ereignisbus zu einem bereiten Server.
    ///
    /// # Arguments
    /// - `config` (`WebServerConfig`): Ziel-Socket-Pfad.
    /// - `routes` (`WebRouteTable`): über [`WebRouteTable::from_registry`] gebaut.
    /// - `authorizer` (`Arc<dyn PeerAuthorizer>`): serverseitig vertraute
    ///   Peer→Tier-Richtlinie; Grundlage des vorgegebenen
    ///   [`TierMapResolver`] (siehe [`Self::with_identity_resolver`]).
    /// - `context_factory` (`Arc<WebContextFactory>`): baut `OpContext` je freigegebenem Aufruf.
    /// - `events` (`Arc<WebEventBus>`): geteilter Ereignisbus für `GET /events`.
    ///
    /// # Returns
    /// Einen gebundenen, noch nicht bedienenden `BoundWebServer`.
    ///
    /// # Errors
    /// - [`WebError::SocketInUse`], wenn unter dem Pfad ein Socket liegt, auf
    ///   dem noch jemand lauscht (oder dessen Lebendigkeit sich nicht
    ///   widerlegen lässt).
    /// - [`WebError::SocketPathOccupied`], wenn unter dem Pfad etwas anderes
    ///   als ein Socket liegt (reguläre Datei, Verzeichnis, Symlink, …).
    /// - [`WebError::Bind`], wenn der Socket-Pfad aus anderen Gründen nicht
    ///   gebunden werden kann (Verzeichnis fehlt, keine Berechtigung).
    ///
    /// # Concurrency
    /// Bindet einmalig; keine Nebenläufigkeit vor dem ersten `serve`-Aufruf.
    pub async fn bind(
        config: WebServerConfig,
        routes: WebRouteTable,
        authorizer: Arc<dyn PeerAuthorizer>,
        context_factory: Arc<WebContextFactory>,
        events: Arc<WebEventBus>,
    ) -> Result<Self, WebError> {
        let listener = bind_unix_listener(&config.socket_path).await?;
        Ok(Self {
            listener,
            socket_path: config.socket_path,
            routes: Arc::new(routes),
            identity: Arc::new(TierMapResolver::new(authorizer)),
            context_factory,
            events,
        })
    }

    /// Übernimmt einen bereits lauschenden Unix-Listener (systemd-Socket-
    /// Aktivierung) statt selbst einen Pfad zu binden.
    ///
    /// # Arguments
    /// - `listener` (`std::os::unix::net::UnixListener`): lauschender,
    ///   nicht-blockierender Listener, typischerweise aus
    ///   [`crate::systemd::listener_from_systemd`].
    /// - übrige Argumente wie bei [`Self::bind`].
    ///
    /// # Returns
    /// Einen bereiten `BoundWebServer`; [`Self::socket_path`] ist der Pfad,
    /// an den der Listener gebunden ist (leer bei einem unbenannten Socket).
    ///
    /// # Errors
    /// [`WebError::Activation`], wenn der Listener nicht nicht-blockierend
    /// geschaltet oder nicht an die `tokio`-Runtime übergeben werden kann.
    ///
    /// # Concurrency
    /// Muss innerhalb einer `tokio`-Runtime mit aktiviertem I/O-Treiber
    /// aufgerufen werden (`tokio::net::UnixListener::from_std`).
    pub fn from_std_listener(
        listener: std::os::unix::net::UnixListener,
        routes: WebRouteTable,
        authorizer: Arc<dyn PeerAuthorizer>,
        context_factory: Arc<WebContextFactory>,
        events: Arc<WebEventBus>,
    ) -> Result<Self, WebError> {
        let socket_path = listener
            .local_addr()
            .ok()
            .and_then(|addr| addr.as_pathname().map(Path::to_path_buf))
            .unwrap_or_default();
        listener
            .set_nonblocking(true)
            .map_err(|error| WebError::Activation {
                reason: format!("could not set listener non-blocking: {error}"),
            })?;
        let listener = UnixListener::from_std(listener).map_err(|error| WebError::Activation {
            reason: format!("could not register listener with the runtime: {error}"),
        })?;
        Ok(Self {
            listener,
            socket_path,
            routes: Arc::new(routes),
            identity: Arc::new(TierMapResolver::new(authorizer)),
            context_factory,
            events,
        })
    }

    /// Ersetzt den Identitätsauflöser (Builder, H12).
    ///
    /// # Description
    /// Ohne diesen Aufruf gilt [`TierMapResolver::new`] über dem an
    /// [`Self::bind`]/[`Self::from_std_listener`] übergebenen
    /// `PeerAuthorizer` — exakt das Verhalten vor H12. Der neue Resolver
    /// **ersetzt** diesen vollständig; er muss seinen Tier-Deckel deshalb
    /// selbst aus derselben Richtlinie beziehen (produktiv:
    /// [`crate::identity::WebIdentityConfig::build_resolver`] mit demselben
    /// `authorizer`).
    ///
    /// # Arguments
    /// - `resolver` (`Arc<dyn LocalPeerIdentityResolver>`): serverseitig aus
    ///   vertrauter Konfiguration gebaut, nie aus einer Anfrage.
    ///
    /// # Returns
    /// `self` mit dem neuen Resolver.
    #[must_use]
    pub fn with_identity_resolver(mut self, resolver: Arc<dyn LocalPeerIdentityResolver>) -> Self {
        self.identity = resolver;
        self
    }

    /// Der Pfad, unter dem dieser Server gebunden wurde.
    #[must_use]
    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket_path
    }

    /// Bedient Verbindungen, bis der Prozess beendet wird.
    ///
    /// # Returns
    /// `Ok(())`, wenn die Annahmeschleife regulär endet (siehe [`Self::serve_until`]).
    ///
    /// # Errors
    /// [`WebError::Accept`], wenn `accept()` fatal scheitert (siehe
    /// [`Self::serve_until`]).
    pub async fn serve(self) -> Result<(), WebError> {
        let (_tx, rx) = watch::channel(false);
        self.serve_until(rx).await
    }

    /// Bedient Verbindungen, bis `shutdown` `true` signalisiert.
    ///
    /// # Description
    /// Jede angenommene Verbindung liest zuerst [`PeerCredentials`] über
    /// [`read_peer_credentials`]; scheitert das, wird die Verbindung
    /// stillschweigend verworfen, ohne dass irgendeine HTTP-Anfrage darauf
    /// bearbeitet wird. Ein periodischer Heartbeat
    /// ([`HEARTBEAT_INTERVAL`]) hält die SSE-Sequenz auch ohne
    /// Operationsaufrufe sichtbar am Leben. Beim Herunterfahren werden alle
    /// noch laufenden Verbindungs-Tasks abgebrochen und eingesammelt, statt
    /// sie herrenlos weiterlaufen zu lassen (dasselbe Muster wie
    /// `harw_mcp_server::transport::BoundMcpListener::serve_until`).
    ///
    /// Ein vorübergehender `accept()`-Fehler (z. B. `EMFILE` bei erschöpften
    /// Dateideskriptoren, `ECONNABORTED`) beendet den Server **nicht**: er wird
    /// über `tracing` protokolliert, und die Annahme pausiert für
    /// `accept_backoff` (exponentiell ab `ACCEPT_BACKOFF_BASE`, gedeckelt
    /// bei `ACCEPT_BACKOFF_MAX`). Während der Pause laufen Heartbeat,
    /// Shutdown-Überwachung und das Einsammeln beendeter Verbindungen weiter.
    /// Die erste erfolgreiche Annahme setzt den Backoff zurück.
    ///
    /// # Arguments
    /// - `shutdown` (`tokio::sync::watch::Receiver<bool>`): wird auf `true`
    ///   gesetzt, um die Schleife geordnet zu beenden.
    ///
    /// # Errors
    /// [`WebError::Accept`], wenn `accept()` fatal scheitert
    /// (siehe `is_fatal_accept_error`: der Listener selbst ist unbrauchbar).
    /// Auch dann werden alle laufenden Verbindungs-Tasks zuvor abgebrochen
    /// und eingesammelt.
    ///
    /// # Concurrency
    /// Jede Verbindung läuft in einer eigenen `tokio::task`, gesammelt in
    /// einem `JoinSet`.
    pub async fn serve_until(self, mut shutdown: watch::Receiver<bool>) -> Result<(), WebError> {
        let mut connections: JoinSet<()> = JoinSet::new();
        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Der erste Tick von `interval` feuert sofort; das ist für einen
        // Heartbeat unerwünscht (kein Ereignis nötig, bevor überhaupt Zeit
        // vergangen ist).
        heartbeat.tick().await;

        // Anzahl unmittelbar aufeinanderfolgender vorübergehender
        // `accept()`-Fehler — steuert die Länge der nächsten Pause.
        let mut accept_failures: u32 = 0;
        // Solange gesetzt, ist der `accept`-Zweig deaktiviert und der
        // Pausen-Zweig wartet bis zu diesem Zeitpunkt.
        let mut accept_paused_until: Option<tokio::time::Instant> = None;
        let mut outcome: Result<(), WebError> = Ok(());

        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                _ = heartbeat.tick() => {
                    self.events.publish(WebEventKind::Heartbeat);
                }
                completed = connections.join_next(), if !connections.is_empty() => {
                    let _ = completed;
                }
                // `select!` wertet den Ausdruck auch bei deaktiviertem Zweig
                // aus (pollt ihn aber nicht) — daher `unwrap_or_else` statt
                // eines Panik-Pfads.
                () = tokio::time::sleep_until(
                    accept_paused_until.unwrap_or_else(tokio::time::Instant::now)
                ), if accept_paused_until.is_some() => {
                    accept_paused_until = None;
                }
                accepted = self.listener.accept(), if accept_paused_until.is_none() => {
                    let stream = match accepted {
                        Ok((stream, _addr)) => {
                            accept_failures = 0;
                            stream
                        }
                        Err(error) => {
                            if is_fatal_accept_error(&error) {
                                tracing::error!(
                                    error = %error,
                                    "fataler accept()-Fehler am Web-Listener — Server wird beendet"
                                );
                                outcome = Err(WebError::Accept);
                                break;
                            }
                            let delay = accept_backoff(accept_failures);
                            accept_failures = accept_failures.saturating_add(1);
                            tracing::warn!(
                                error = %error,
                                consecutive_failures = accept_failures,
                                backoff_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                                "vorübergehender accept()-Fehler am Web-Listener — Annahme pausiert, Server läuft weiter"
                            );
                            accept_paused_until = Some(tokio::time::Instant::now() + delay);
                            continue;
                        }
                    };
                    let Ok(peer) = read_peer_credentials(&stream) else {
                        // Ohne Peer-Identität keine einzige HTTP-Anfrage —
                        // die Verbindung wird beim Drop des Streams geschlossen.
                        continue;
                    };
                    let routes = Arc::clone(&self.routes);
                    let identity = Arc::clone(&self.identity);
                    let context_factory = Arc::clone(&self.context_factory);
                    let events = Arc::clone(&self.events);
                    connections.spawn(async move {
                        let builder = http_connection_builder(HEADER_READ_TIMEOUT);
                        let _ = builder
                            .serve_connection(
                                TokioIo::new(stream),
                                service_fn(move |request| {
                                    handle(
                                        request,
                                        peer,
                                        Arc::clone(&routes),
                                        Arc::clone(&identity),
                                        Arc::clone(&context_factory),
                                        Arc::clone(&events),
                                    )
                                }),
                            )
                            .await;
                    });
                }
            }
        }
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        outcome
    }
}

/// Bindet den Unix-Socket, ohne je fremde Dateien zu löschen.
///
/// # Description
/// Liegt unter `path` bereits etwas, entscheidet `symlink_metadata` (folgt
/// **keinem** Symlink):
/// - kein Socket (reguläre Datei, Verzeichnis, Symlink, …) →
///   [`WebError::SocketPathOccupied`], nichts wird angefasst;
/// - ein Socket → ein Lebendigkeits-`connect` (begrenzt durch
///   [`STALE_SOCKET_PROBE_TIMEOUT`]). Nur `ECONNREFUSED` belegt einen toten
///   Socket (Rest eines nicht sauber beendeten Laufs); nur dann wird er
///   entfernt. Gelingt der `connect`, läuft die Frist ab oder scheitert er
///   anders (z. B. `EACCES`), gilt der Socket als belegt →
///   [`WebError::SocketInUse`].
///
/// Zwischen Prüfung und `remove_file` bleibt ein Zeitfenster (TOCTOU), in dem
/// ein Dritter den Pfad austauschen könnte; das Verzeichnis des Sockets muss
/// deshalb dem Betreiber gehören (Deploy-Sache, nicht dieses Moduls).
///
/// # Errors
/// [`WebError::SocketPathOccupied`], [`WebError::SocketInUse`] oder
/// [`WebError::Bind`] (Metadaten nicht lesbar, Entfernen oder Binden
/// gescheitert).
async fn bind_unix_listener(path: &Path) -> Result<UnixListener, WebError> {
    let path_display = || path.display().to_string();
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() {
                return Err(WebError::SocketPathOccupied {
                    path: path_display(),
                });
            }
            let probe =
                tokio::time::timeout(STALE_SOCKET_PROBE_TIMEOUT, UnixStream::connect(path)).await;
            match probe {
                Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    // Toter Socket: niemand lauscht mehr — nur dieser Fall
                    // darf die Datei entfernen.
                    std::fs::remove_file(path).map_err(|_| WebError::Bind {
                        path: path_display(),
                    })?;
                }
                // Verbindung gelungen, Frist abgelaufen oder ein anderer
                // Fehler: Lebendigkeit nicht widerlegt → nie löschen.
                _ => {
                    return Err(WebError::SocketInUse {
                        path: path_display(),
                    });
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            return Err(WebError::Bind {
                path: path_display(),
            });
        }
    }
    UnixListener::bind(path).map_err(|_| WebError::Bind {
        path: path_display(),
    })
}

/// Baut den HTTP/1-Verbindungs-Builder mit Kopfzeilen-Timeout.
///
/// # Description
/// `http1_only` überspringt die Protokollerkennung des `auto`-Builders (die
/// ohne Zeitgeber auf die ersten Bytes warten würde), sodass der Timeout ab
/// Verbindungsbeginn gilt. `header_read_timeout` verlangt einen gesetzten
/// Timer (hyper-util-Doku: ohne Timer Panik) — daher immer zusammen mit
/// [`TokioTimer`].
fn http_connection_builder(header_read_timeout: Duration) -> Builder<TokioExecutor> {
    let mut builder = Builder::new(TokioExecutor::new()).http1_only();
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(header_read_timeout);
    builder
}

/// Ob ein `accept()`-Fehler den Listener selbst als unbrauchbar ausweist.
///
/// # Description
/// Fatal sind nur Fehler, die sich durch Warten nie beheben: ungültiger oder
/// fremder Deskriptor (`EBADF`, `ENOTSOCK`), Socket lauscht nicht (`EINVAL`),
/// ungültige Adresse (`EFAULT`), Operation nicht unterstützt (`EOPNOTSUPP`).
/// Alles andere — insbesondere `EMFILE`/`ENFILE`/`ENOBUFS`/`ENOMEM`
/// (Ressourcenknappheit), `ECONNABORTED`/`EPROTO` (Client brach ab),
/// `EINTR`/`EAGAIN` — ist vorübergehend, ebenso ein Fehler ohne
/// Betriebssystem-Fehlernummer.
fn is_fatal_accept_error(error: &std::io::Error) -> bool {
    use rustix::io::Errno;
    const FATAL: [Errno; 5] = [
        Errno::BADF,
        Errno::NOTSOCK,
        Errno::INVAL,
        Errno::FAULT,
        Errno::OPNOTSUPP,
    ];
    Errno::from_io_error(error).is_some_and(|errno| FATAL.contains(&errno))
}

/// Pause nach dem `failures`-ten aufeinanderfolgenden vorübergehenden
/// `accept()`-Fehler (0-basiert): `ACCEPT_BACKOFF_BASE · 2^failures`,
/// gedeckelt bei [`ACCEPT_BACKOFF_MAX`].
fn accept_backoff(failures: u32) -> Duration {
    ACCEPT_BACKOFF_BASE
        .saturating_mul(1_u32 << failures.min(16))
        .min(ACCEPT_BACKOFF_MAX)
}

/// Bearbeitet eine einzelne HTTP-Anfrage einer bereits identifizierten Verbindung.
///
/// # Description
/// Trifft selbst keine Zugriffsentscheidung — löst die Identität über
/// `identity` auf (siehe Moduldoku „Identitätsauflösung"), delegiert die
/// Entscheidung vollständig an [`decide_resolved_route`] und übersetzt deren
/// Ergebnis in eine HTTP-Antwort. `GET /events` und die Metarouten sind die
/// einzigen Pfade, die diese Funktion ohne Umweg über die Routentabelle
/// behandelt (sie sind keine Operationen).
async fn handle(
    request: Request<Incoming>,
    peer: PeerCredentials,
    routes: Arc<WebRouteTable>,
    identity: Arc<dyn LocalPeerIdentityResolver>,
    context_factory: Arc<WebContextFactory>,
    events: Arc<WebEventBus>,
) -> Result<WebResponse, Infallible> {
    let path = request.uri().path().to_owned();
    // Über `await` hinweg werden nur `&Method`/`&HeaderMap` gehalten, nie
    // ein `&Request<Incoming>` (dessen Rumpf nicht `Sync` sein muss).
    if path == EVENTS_PATH {
        let http_method = request.method();
        let headers = request.headers();
        return Ok(handle_events(http_method, headers, &peer, identity.as_ref(), &events).await);
    }
    if let Some(meta) = MetaRoute::from_path(&path) {
        let http_method = request.method();
        let headers = request.headers();
        return Ok(handle_meta(
            http_method,
            headers,
            meta,
            &peer,
            identity.as_ref(),
            &routes,
        )
        .await);
    }

    // Nur exakt GET/POST; HEAD/OPTIONS/… werden nie implizit auf eine
    // deklarierte Methode abgebildet und enden in 405 (F-031).
    let method = parse_web_method(request.method().as_str());
    // Identität nur auflösen, wenn Route und Methode passen — sonst endet
    // `decide_resolved_route` ohnehin in 404/405, bevor sie die Stufe fragt.
    let resolution = if method.is_some() && routes.method_for(&path) == method {
        let headers = request.headers();
        Some(resolve_identity(identity.as_ref(), &peer, headers).await)
    } else {
        None
    };
    let resolved = match &resolution {
        Some(Ok(resolved)) => Some(resolved),
        Some(Err(_)) | None => None,
    };
    let decision = decide_resolved_route(&routes, resolved, &path, method);

    match decision {
        RouteDecision::NotFound => Ok(json_response(
            StatusCode::NOT_FOUND,
            &serde_json::json!({"error": "not_found"}),
        )),
        RouteDecision::MethodNotAllowed { expected } => Ok(method_not_allowed_response(expected)),
        RouteDecision::Forbidden { reason } => {
            // Scheiterte die Auflösung, nennt der Resolver den konkreten Grund.
            let reason = match (&resolution, reason) {
                (Some(Err(error)), ForbiddenReason::UnknownPeer) => error.code(),
                _ => forbidden_reason_str(reason),
            };
            Ok(forbidden_response(reason))
        }
        RouteDecision::ApprovalRequired { route } => Ok(json_response(
            StatusCode::FORBIDDEN,
            &serde_json::json!({
                "error": "forbidden",
                "reason": "approval_required",
                "operation": route.operation_name(),
            }),
        )),
        RouteDecision::Execute { route, caller_tier } => {
            let args = match read_json_body(request, MAX_BODY_BYTES, BODY_READ_TIMEOUT).await {
                Ok(value) => value,
                Err(response) => return Ok(response),
            };
            let ctx = (context_factory)(&peer, caller_tier);
            // Mandant und Kontextzusammenfassung kommen ausschließlich aus der
            // aufgelösten Identität (H12), nie aus Rumpf oder Headern. Ohne
            // aufgelöste Identität gibt es nichts, worauf sich die Sicht
            // einschränken ließe — R15/M1 verlangt, dann abzulehnen statt
            // ungeschützt (mandantenübergreifend) auszuführen.
            let ctx = match scoped_context(resolved, ctx) {
                Ok(ctx) => ctx,
                Err(response) => return Ok(response),
            };
            let operation_name = route.operation_name().to_owned();
            let result = route.invoke(&ctx, args).await;
            let ok = result.is_ok();
            // `OpOutput` trägt keine `TrustClass` (siehe
            // `crate::events`-Moduldoku „Woher die TrustClass kommt — und wo
            // sie verloren geht") — die Vorgabe ist die niedrigste Klasse,
            // nie eine erfundene.
            events.publish(WebEventKind::OperationCompleted {
                operation: operation_name,
                ok,
                trust: harw_context::TrustClass::Data,
            });
            Ok(op_result_response(result))
        }
    }
}

/// Löst die Identität des Peers einer Anfrage auf.
///
/// # Description
/// Liest ausschließlich den Kontextverweis
/// ([`crate::identity::SECURITY_CONTEXT_HEADER`]) aus den Headern — kein
/// anderer Header und nie der Rumpf trägt Identität — und reicht ihn mit den
/// `SO_PEERCRED` der Verbindung an den Resolver.
///
/// # Errors
/// [`IdentityError::InvalidContextReference`] für einen unlesbaren oder
/// mehrfachen Header, sonst jeder Fehler des Resolvers.
async fn resolve_identity(
    identity: &dyn LocalPeerIdentityResolver,
    peer: &PeerCredentials,
    headers: &HeaderMap,
) -> Result<ResolvedPeer, IdentityError> {
    let presented = presented_context(headers)?;
    identity.resolve(peer, presented).await
}

/// Schränkt den `OpContext` einer freigegebenen Ausführung auf die
/// aufgelöste Identität ein — oder lehnt ab, wenn keine vorliegt.
///
/// # Description
/// Mandant und Kontextzusammenfassung kommen ausschließlich aus
/// [`ResolvedPeer::scope_op_context`] (H12), nie aus Rumpf oder Headern.
/// [`decide_resolved_route`] liefert `Execute` heute nur, wenn die Identität
/// aufgelöst wurde — `resolved` ist hier also stets `Some`. Diese Funktion
/// behandelt `None` trotzdem explizit als Ablehnung, damit eine künftige
/// Änderung der Entscheidungslogik (z. B. ein Wechsel auf `decide_route`)
/// nie versehentlich einen ungeschützten, mandantenübergreifenden
/// `OpContext` erzeugt (R15/M1: Rückfallpfade lehnen ab, statt weiter
/// auszuführen).
///
/// # Errors
/// `403` mit [`ForbiddenReason::UnknownPeer`], wenn `resolved` `None` ist.
// Wie bei `read_json_body`: der Fehlerfall ist die fertige `WebResponse`, die
// der Aufrufer unverändert zurückgibt; sie lebt nur einen Request lang.
#[allow(clippy::result_large_err)]
fn scoped_context(
    resolved: Option<&ResolvedPeer>,
    ctx: OpContext,
) -> Result<OpContext, WebResponse> {
    match resolved {
        Some(resolved) => Ok(resolved.scope_op_context(ctx)),
        None => Err(forbidden_response(forbidden_reason_str(
            ForbiddenReason::UnknownPeer,
        ))),
    }
}

/// `403` mit stabilem Grund.
fn forbidden_response(reason: &str) -> WebResponse {
    json_response(
        StatusCode::FORBIDDEN,
        &serde_json::json!({"error": "forbidden", "reason": reason}),
    )
}

/// Bearbeitet `GET /v1/health`, `/v1/version`, `/v1/capabilities` — keine
/// Operation, keine Route (siehe [`crate::meta`]).
///
/// # Description
/// Nur `GET` (sonst `405` mit `Allow: GET`). `/v1/health` antwortet jedem
/// Peer, der den Socket erreicht, ohne Identitätsauflösung;
/// `/v1/version`/`/v1/capabilities` verlangen [`MetaRoute::required_tier`]
/// über denselben Resolver wie jede Route (Auflösung gescheitert → `403` mit
/// [`IdentityError::code`], z. B. `unknown_peer`).
async fn handle_meta(
    method: &Method,
    headers: &HeaderMap,
    meta: MetaRoute,
    peer: &PeerCredentials,
    identity: &dyn LocalPeerIdentityResolver,
    routes: &WebRouteTable,
) -> WebResponse {
    if *method != Method::GET {
        return method_not_allowed_response(WebMethod::Get);
    }
    if let Some(required) = meta.required_tier() {
        let resolved = match resolve_identity(identity, peer, headers).await {
            Ok(resolved) => resolved,
            Err(error) => return forbidden_response(error.code()),
        };
        if !tier_permits(resolved.tier(), required) {
            return forbidden_response(forbidden_reason_str(ForbiddenReason::InsufficientTier));
        }
    }
    let mut response = json_response(StatusCode::OK, &meta.document(routes));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Bearbeitet `GET /events` — keine Operation, keine Route, nur der
/// Ereignisstrom.
async fn handle_events(
    method: &Method,
    headers: &HeaderMap,
    peer: &PeerCredentials,
    identity: &dyn LocalPeerIdentityResolver,
    events: &Arc<WebEventBus>,
) -> WebResponse {
    if *method != Method::GET {
        return method_not_allowed_response(WebMethod::Get);
    }
    let accepts_event_stream = headers
        .get(ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| {
            value
                .split(',')
                .map(str::trim)
                .any(|value| value == "text/event-stream" || value == "*/*")
        });
    if !accepts_event_stream {
        return json_response(
            StatusCode::NOT_ACCEPTABLE,
            &serde_json::json!({"error": "not_acceptable"}),
        );
    }
    // Ein nicht auflösbarer Peer sieht keinen Ereignisstrom — dieselbe Regel
    // wie für jede andere Route: keine Autorisierung ohne aufgelöste Identität.
    if let Err(error) = resolve_identity(identity, peer, headers).await {
        return forbidden_response(error.code());
    }
    sse_response(Arc::clone(events))
}

/// Baut die SSE-Streaming-Antwort für `GET /events`.
///
/// # Description
/// Spawnt eine Hintergrund-Task, die jedes über [`WebEventBus::subscribe`]
/// empfangene [`WebEvent`] als `data:`-Zeile in einen
/// [`http_body_util::Channel`]-Rumpf schreibt — dasselbe Muster wie
/// `harw_mcp_server::transport`s SSE-`GET`-Handler. Ein
/// [`WebEventReceiveError::Lagged`] wird selbst als sichtbares Ereignis
/// ausgeliefert (`event: lagged`), damit ein Client die Lücke ohne eigene
/// Sequenzbuchhaltung erkennt.
fn sse_response(events: Arc<WebEventBus>) -> WebResponse {
    let mut subscription = events.subscribe();
    let (mut sender, body) = Channel::<Bytes, Infallible>::new(8);
    tokio::spawn(async move {
        loop {
            let frame = match subscription.recv().await {
                Ok(event) => match event.to_sse_frame() {
                    Ok(frame) => frame,
                    Err(_) => continue,
                },
                Err(WebEventReceiveError::Lagged { skipped }) => {
                    format!("event: lagged\ndata: {{\"skipped\":{skipped}}}\n\n")
                }
                Err(WebEventReceiveError::Closed) => break,
            };
            if sender.send_data(Bytes::from(frame)).await.is_err() {
                break;
            }
        }
    });
    let mut response = Response::new(body.boxed());
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// Liest und dekodiert den JSON-Rumpf eines freigegebenen Aufrufs.
///
/// # Description
/// Prüft zuerst einen angegebenen `Content-Length` gegen `max_bytes`
/// (schnelle Ablehnung ohne Lesen). Danach wird der Rumpf **ausschließlich**
/// über [`Limited`] gelesen: sobald die tatsächlich eingetroffenen
/// Datenrahmen `max_bytes` überschreiten, bricht das Lesen mit
/// [`LengthLimitError`] ab — unabhängig davon, ob der Client
/// `Content-Length` oder `Transfer-Encoding: chunked` benutzt. Es liegen also
/// nie mehr als `max_bytes` Rumpfdaten im Speicher. Das gesamte Lesen ist
/// durch `read_timeout` begrenzt. Ein leerer Rumpf (typisch für `GET`) wird
/// als `serde_json::Value::Null` behandelt — dieselbe Bedeutung wie bei einer
/// `Surface::ModelTool`-Fläche ohne Argumente.
///
/// # Arguments
/// - `request` (`Request<Incoming>`): die freigegebene Anfrage.
/// - `max_bytes` (`usize`): Rumpfobergrenze (produktiv [`MAX_BODY_BYTES`]).
/// - `read_timeout` (`Duration`): Lesefrist (produktiv [`BODY_READ_TIMEOUT`]).
///
/// # Returns
/// - `Ok(value)`: dekodierte JSON-Argumente.
/// - `Err(response)`: eine fertige Fehlerantwort (400/408/413), die `handle`
///   unverändert zurückgibt.
// Der Fehlerfall trägt bewusst eine fertige `WebResponse` (rund 128 Bytes),
// die `handle` unverändert zurückgibt — das ist der Sinn dieser Funktion. Sie
// lebt genau einen Request lang auf dem Stack; ein `Box` würde nur jede
// Aufrufstelle verumständlichen.
#[allow(clippy::result_large_err)]
async fn read_json_body(
    request: Request<Incoming>,
    max_bytes: usize,
    read_timeout: Duration,
) -> Result<serde_json::Value, WebResponse> {
    let payload_too_large = || {
        json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            &serde_json::json!({"error": "payload_too_large"}),
        )
    };
    let declared_len = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared_len.is_some_and(|len| len > max_bytes as u64) {
        return Err(payload_too_large());
    }
    let limited = Limited::new(request.into_body(), max_bytes);
    let bytes = match tokio::time::timeout(read_timeout, limited.collect()).await {
        Ok(Ok(collected)) => collected.to_bytes(),
        Ok(Err(error)) if error.downcast_ref::<LengthLimitError>().is_some() => {
            return Err(payload_too_large());
        }
        Ok(Err(_)) => {
            return Err(json_response(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({"error": "bad_request"}),
            ));
        }
        Err(_elapsed) => {
            return Err(json_response(
                StatusCode::REQUEST_TIMEOUT,
                &serde_json::json!({"error": "request_timeout"}),
            ));
        }
    };
    if bytes.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        json_response(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({"error": "invalid_json"}),
        )
    })
}

/// Übersetzt das Ergebnis von [`harw_operations::adapter::WebAdapter::invoke`]
/// in eine HTTP-Antwort.
///
/// # Description
/// Reicht `OpOutput::text` unverändert als JSON-Zeichenkette weiter — kein
/// Markdown-Rendering, keine aktiven Links (das ist UI-01s Sache, siehe
/// crate-Moduldoku). Trägt `OpOutput::data` eine strukturierte Nutzlast, wird
/// sie additiv als Feld `data` ausgeliefert; ohne Nutzlast bleibt das
/// Antwortobjekt unverändert `{"text", "trust"}` (F-222). Trägt zusätzlich `trust`: `OpOutput` liefert keine
/// `TrustClass` (siehe `crate::events`-Moduldoku), daher immer
/// [`harw_context::TrustClass::Data`] — die niedrigste Klasse, nie eine
/// erfundene.
fn op_result_response(
    result: Result<harw_operations::operation::OpOutput, OpError>,
) -> WebResponse {
    match result {
        Ok(output) => {
            let mut body = serde_json::json!({
                "text": output.text,
                "trust": harw_context::TrustClass::Data,
            });
            if let (Some(data), Some(object)) = (output.data, body.as_object_mut()) {
                object.insert("data".to_owned(), data);
            }
            json_response(StatusCode::OK, &body)
        }
        Err(error) => {
            let status = status_for_op_error(&error);
            json_response(status, &serde_json::json!({"error": error.to_string()}))
        }
    }
}

/// Bildet einen [`OpError`] auf den passenden HTTP-Statuscode ab.
fn status_for_op_error(error: &OpError) -> StatusCode {
    match error {
        OpError::InvalidArguments(_) => StatusCode::BAD_REQUEST,
        OpError::Execution(_) => StatusCode::INTERNAL_SERVER_ERROR,
        OpError::NotAvailable(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// Der stabile, maschinenlesbare Grund-String für eine [`ForbiddenReason`].
fn forbidden_reason_str(reason: ForbiddenReason) -> &'static str {
    match reason {
        ForbiddenReason::UnknownPeer => "unknown_peer",
        ForbiddenReason::InsufficientTier => "insufficient_permission_tier",
    }
}

/// Baut eine `405 Method Not Allowed`-Antwort mit `Allow`-Header.
///
/// `Allow` nennt genau die deklarierte Methode der Route (RFC 9110 §15.5.6).
fn method_not_allowed_response(expected: WebMethod) -> WebResponse {
    let name = method_name(expected);
    let mut response = json_response(
        StatusCode::METHOD_NOT_ALLOWED,
        &serde_json::json!({"error": "method_not_allowed", "expected": name}),
    );
    response
        .headers_mut()
        .insert(hyper::header::ALLOW, HeaderValue::from_static(name));
    response
}

/// Baut eine JSON-Antwort mit gegebenem Statuscode.
fn json_response(status: StatusCode, body: &serde_json::Value) -> WebResponse {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body.to_string())).boxed())
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()).boxed()))
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::os::unix::fs::FileTypeExt;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use super::{
        ACCEPT_BACKOFF_BASE, ACCEPT_BACKOFF_MAX, BoundWebServer, HEADER_READ_TIMEOUT,
        MAX_BODY_BYTES, WebContextFactory, WebServerConfig, accept_backoff, bind_unix_listener,
        forbidden_reason_str, http_connection_builder, is_fatal_accept_error, json_response,
        method_not_allowed_response, op_result_response, read_json_body, scoped_context,
        status_for_op_error,
    };
    use crate::authz::StaticUidTierMap;
    use crate::error::WebError;
    use crate::events::WebEventBus;
    use crate::peer::PeerCredentials;
    use crate::router::{ForbiddenReason, WebMethod, WebRouteTable};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_operations::context::OpContext;
    use harw_operations::error::OpError;
    use harw_operations::operation::PermissionTier;
    use harw_operations::registry::OperationRegistry;
    use hyper::StatusCode;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    // Die ersten Tests berühren nur in-memory `Response`/`StatusCode`-Werte.
    // Die Tests ab „Echter Unix-Socket" binden echte Sockets in einem
    // Tempdir (W1-11, F-184/F-185) — nie an einem geteilten Pfad.

    /// Obergrenze, die ein einzelner Socket-Test insgesamt warten darf.
    const TEST_DEADLINE: Duration = Duration::from_secs(10);

    /// Liest bis zum Ende der ersten Antwortzeile (`HTTP/1.1 413 …`).
    ///
    /// Liefert den bis dahin gelesenen Text; schließt der Server ohne
    /// Antwort, ist das Ergebnis leer. Lesefehler (z. B. `ECONNRESET`, wenn
    /// der Server nach einer Ablehnung ungelesene Daten verwirft) beenden das
    /// Lesen wie ein EOF — was vorher ankam, bleibt erhalten.
    async fn read_status_line(reader: &mut (impl AsyncRead + Unpin)) -> String {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            if let Some(end) = buffer.windows(2).position(|window| window == b"\r\n") {
                return String::from_utf8_lossy(&buffer[..end]).into_owned();
            }
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => return String::from_utf8_lossy(&buffer).into_owned(),
                Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            }
        }
    }

    /// Verbindet sich mit `path`, schreibt `raw` in einer eigenen Task und
    /// liefert die Statuszeile der Antwort.
    ///
    /// Die Schreibhälfte bleibt offen, bis die Antwort gelesen ist — ein
    /// EOF des Clients kann das Ergebnis also nicht verfälschen. Das
    /// Schreiben läuft nebenläufig, damit ein Server, der nach einer
    /// Ablehnung nicht weiterliest, den Test nicht blockiert.
    async fn send_and_read_status_line(path: &Path, raw: Vec<u8>) -> TestResult<String> {
        let stream = UnixStream::connect(path)
            .await
            .map_err(ctx("Testserver lauscht am Tempdir-Socket"))?;
        let (mut reader, mut writer) = stream.into_split();
        let writer_task = tokio::spawn(async move {
            // `EPIPE` ist erwartbar, wenn der Server nach `413` schließt.
            let _ = writer.write_all(&raw).await;
            writer
        });
        let status = tokio::time::timeout(TEST_DEADLINE, read_status_line(&mut reader))
            .await
            .map_err(ctx(
                "Antwort (oder Verbindungsende) innerhalb der Testfrist",
            ))?;
        writer_task.abort();
        Ok(status)
    }

    /// Bedient genau eine Verbindung mit dem **produktiven**
    /// Verbindungs-Builder; der Dienst ruft das produktive
    /// [`read_json_body`] mit [`MAX_BODY_BYTES`] und spiegelt das dekodierte
    /// JSON als `200`.
    ///
    /// Eine vollständige `Execute`-Route über [`BoundWebServer`] ist in
    /// dieser Crate nicht testbar: `OpContext` braucht eine `SandboxSpec`
    /// aus `harw-sandbox`, das keine Abhängigkeit von `harw-web` ist.
    fn spawn_body_reader(
        listener: UnixListener,
        header_read_timeout: Duration,
        body_read_timeout: Duration,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let Ok((stream, _addr)) = listener.accept().await else {
                return;
            };
            let builder = http_connection_builder(header_read_timeout);
            let _ =
                builder
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(move |request| async move {
                            let response =
                                match read_json_body(request, MAX_BODY_BYTES, body_read_timeout)
                                    .await
                                {
                                    Ok(value) => json_response(StatusCode::OK, &value),
                                    Err(response) => response,
                                };
                            Ok::<_, Infallible>(response)
                        }),
                    )
                    .await;
        })
    }

    /// Baut eine vollständige `POST`-Anfrage mit `Transfer-Encoding: chunked`
    /// und **ohne** `Content-Length`.
    fn chunked_post(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut raw = b"POST /api/x HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        for chunk in chunks {
            raw.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            raw.extend_from_slice(chunk);
            raw.extend_from_slice(b"\r\n");
        }
        raw.extend_from_slice(b"0\r\n\r\n");
        raw
    }

    fn socket_tempdir() -> TestResult<tempfile::TempDir> {
        tempfile::tempdir().map_err(ctx("Tempdir für den Test-Socket anlegbar"))
    }

    #[test]
    fn test_status_for_op_error_maps_each_variant() {
        assert_eq!(
            status_for_op_error(&OpError::InvalidArguments(String::new())),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status_for_op_error(&OpError::Execution(String::new())),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            status_for_op_error(&OpError::NotAvailable(String::new())),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn test_forbidden_reason_str_is_stable_and_distinct() {
        assert_eq!(
            forbidden_reason_str(ForbiddenReason::UnknownPeer),
            "unknown_peer"
        );
        assert_eq!(
            forbidden_reason_str(ForbiddenReason::InsufficientTier),
            "insufficient_permission_tier"
        );
        assert_ne!(
            forbidden_reason_str(ForbiddenReason::UnknownPeer),
            forbidden_reason_str(ForbiddenReason::InsufficientTier)
        );
    }

    /// Baut einen echten `OpContext` über eine reale, kanonisierte
    /// `WorkspaceBinding` in `dir` — derselbe Weg wie
    /// `harw-web/tests/identity_routes.rs`, da `OpContext::new` (siehe
    /// `harw-operations/src/context.rs`) eine echte `SandboxSpec` verlangt
    /// und `harw-authority` dafür bereits Testabhängigkeit dieser Crate ist
    /// (siehe `harw-web/Cargo.toml`). Der konkrete Mandant der Sandbox spielt
    /// für [`scoped_context`] keine Rolle — er wird von dessen `None`-Zweig
    /// nie gelesen.
    fn dummy_op_context(dir: &Path) -> TestResult<OpContext> {
        use harw_authority::{
            PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
        };
        use harw_operations::context::ServiceMap;
        use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};

        std::fs::create_dir_all(dir.join("ws")).map_err(ctx("Workspace-Verzeichnis anlegbar"))?;
        let tenant = TenantId::try_from_str("t").map_err(ctx("Sandbox-Mandant"))?;
        let workspace = WorkspaceId::try_from_str("w").map_err(ctx("Workspace-Id"))?;
        let registry = WorkspaceRegistry::build(
            dir,
            [WorkspaceRegistration {
                tenant: tenant.clone(),
                workspace: workspace.clone(),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("WorkspaceRegistry baubar"))?;
        let binding = registry
            .resolve(&tenant, &workspace)
            .map_err(ctx("Workspace auflösbar"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        Ok(OpContext::new(
            SessionId::new(),
            TurnId::new(),
            sandbox,
            ServiceMap::new(),
        ))
    }

    // R15/M1: ohne aufgelöste Identität lehnt `scoped_context` ab, statt den
    // `OpContext` ungeschützt (mandantenübergreifend) durchzureichen — auch
    // wenn `decide_resolved_route` diesen Zweig heute nie mit `Execute`
    // erreicht (siehe `scoped_context`-Doku).
    #[tokio::test]
    async fn test_scoped_context_without_resolved_identity_is_forbidden() -> TestResult {
        use http_body_util::BodyExt;

        let dir = socket_tempdir()?;
        let op_ctx = dummy_op_context(dir.path())?;

        let response = match scoped_context(None, op_ctx) {
            Ok(_) => {
                return Err(TestError::Unexpected(
                    "scoped_context ohne aufgelöste Identität sollte ablehnen".to_owned(),
                ));
            }
            Err(response) => response,
        };
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(ctx("Rumpf sollte sich sammeln lassen"))?
            .to_bytes();
        let body: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(ctx("JSON sollte sich dekodieren lassen"))?;
        assert_eq!(body["reason"], serde_json::json!("unknown_peer"));
        Ok(())
    }

    #[test]
    fn test_json_response_sets_status_and_content_type() {
        let response = json_response(StatusCode::NOT_FOUND, &serde_json::json!({"error": "x"}));
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(hyper::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
    }

    #[test]
    fn test_method_not_allowed_response_allow_header_names_post() {
        let response = method_not_allowed_response(WebMethod::Post);
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response
                .headers()
                .get(hyper::header::ALLOW)
                .and_then(|v| v.to_str().ok()),
            Some("POST")
        );
    }

    #[tokio::test]
    async fn test_op_result_response_without_data_keeps_text_trust_shape() -> TestResult {
        use http_body_util::BodyExt;
        let output = harw_operations::operation::OpOutput {
            text: "hallo".to_owned(),
            data: None,
        };
        let response = op_result_response(Ok(output));
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(ctx("Rumpf sollte sich sammeln lassen"))?
            .to_bytes();
        let body: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(ctx("JSON sollte sich dekodieren lassen"))?;
        let object = body.as_object().ok_or(TestError::Missing("JSON-Objekt"))?;
        assert_eq!(object.get("text"), Some(&serde_json::json!("hallo")));
        assert!(object.contains_key("trust"));
        assert!(
            !object.contains_key("data"),
            "ohne Nutzlast kein data-Feld: {body}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_op_result_response_with_data_adds_data_field() -> TestResult {
        use http_body_util::BodyExt;
        let output = harw_operations::operation::OpOutput {
            text: "2 Einträge".to_owned(),
            data: Some(serde_json::json!({"items": [1, 2]})),
        };
        let response = op_result_response(Ok(output));
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(ctx("Rumpf sollte sich sammeln lassen"))?
            .to_bytes();
        let body: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(ctx("JSON sollte sich dekodieren lassen"))?;
        assert_eq!(body["text"], serde_json::json!("2 Einträge"));
        assert_eq!(body["data"], serde_json::json!({"items": [1, 2]}));
        assert!(body.get("trust").is_some());
        Ok(())
    }

    #[test]
    fn test_method_not_allowed_response_carries_allow_header() {
        let response = method_not_allowed_response(WebMethod::Get);
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response
                .headers()
                .get(hyper::header::ALLOW)
                .and_then(|v| v.to_str().ok()),
            Some("GET")
        );
    }

    #[tokio::test]
    async fn test_json_response_body_round_trips_through_serde() -> TestResult {
        use http_body_util::BodyExt;
        let payload = serde_json::json!({"text": "hallo"});
        let response = json_response(StatusCode::OK, &payload);
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(ctx("in-memory body always resolves"))?
            .to_bytes();
        let decoded: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(ctx("JSON sollte sich dekodieren lassen"))?;
        assert_eq!(decoded, payload);
        Ok(())
    }

    // ── Echter Unix-Socket: Rumpfgrenze und Timeouts (F-184) ────────────────

    #[tokio::test]
    async fn test_chunked_body_over_limit_in_many_chunks_yields_413() -> TestResult {
        let dir = socket_tempdir()?;
        let path = dir.path().join("w.sock");
        let listener =
            UnixListener::bind(&path).map_err(ctx("UnixListener::bind sollte gelingen"))?;
        let server = spawn_body_reader(listener, HEADER_READ_TIMEOUT, TEST_DEADLINE);
        // Kein einzelner Chunk überschreitet die Grenze, erst die Summe —
        // genau der Fall, den eine reine `Content-Length`-Prüfung übersieht.
        let chunk = vec![b'a'; 30 * 1024];
        let raw = chunked_post(&[chunk.clone(), chunk.clone(), chunk]);
        let status = send_and_read_status_line(&path, raw).await?;
        assert!(
            status.starts_with("HTTP/1.1 413"),
            "erwartet 413, erhalten: {status:?}"
        );
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_chunked_single_chunk_one_byte_over_limit_yields_413() -> TestResult {
        let dir = socket_tempdir()?;
        let path = dir.path().join("w.sock");
        let listener =
            UnixListener::bind(&path).map_err(ctx("UnixListener::bind sollte gelingen"))?;
        let server = spawn_body_reader(listener, HEADER_READ_TIMEOUT, TEST_DEADLINE);
        let raw = chunked_post(&[vec![b' '; MAX_BODY_BYTES + 1]]);
        let status = send_and_read_status_line(&path, raw).await?;
        assert!(
            status.starts_with("HTTP/1.1 413"),
            "erwartet 413, erhalten: {status:?}"
        );
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_chunked_body_within_limit_is_decoded() -> TestResult {
        let dir = socket_tempdir()?;
        let path = dir.path().join("w.sock");
        let listener =
            UnixListener::bind(&path).map_err(ctx("UnixListener::bind sollte gelingen"))?;
        let server = spawn_body_reader(listener, HEADER_READ_TIMEOUT, TEST_DEADLINE);
        let raw = chunked_post(&[b"{\"a\":".to_vec(), b"1}".to_vec()]);
        let status = send_and_read_status_line(&path, raw).await?;
        assert!(
            status.starts_with("HTTP/1.1 200"),
            "erwartet 200, erhalten: {status:?}"
        );
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_declared_content_length_over_limit_yields_413_without_body() -> TestResult {
        let dir = socket_tempdir()?;
        let path = dir.path().join("w.sock");
        let listener =
            UnixListener::bind(&path).map_err(ctx("UnixListener::bind sollte gelingen"))?;
        let server = spawn_body_reader(listener, HEADER_READ_TIMEOUT, TEST_DEADLINE);
        let raw = format!(
            "POST /api/x HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        )
        .into_bytes();
        let status = send_and_read_status_line(&path, raw).await?;
        assert!(
            status.starts_with("HTTP/1.1 413"),
            "erwartet 413, erhalten: {status:?}"
        );
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_stalled_body_yields_408_after_body_read_timeout() -> TestResult {
        let dir = socket_tempdir()?;
        let path = dir.path().join("w.sock");
        let listener =
            UnixListener::bind(&path).map_err(ctx("UnixListener::bind sollte gelingen"))?;
        let server = spawn_body_reader(listener, HEADER_READ_TIMEOUT, Duration::from_millis(100));
        // 20 Byte angekündigt, 4 gesendet, Verbindung bleibt offen.
        let raw =
            b"POST /api/x HTTP/1.1\r\nHost: localhost\r\nContent-Length: 20\r\n\r\n{\"a\"".to_vec();
        let status = send_and_read_status_line(&path, raw).await?;
        assert!(
            status.starts_with("HTTP/1.1 408"),
            "erwartet 408, erhalten: {status:?}"
        );
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_incomplete_headers_close_connection_after_header_read_timeout() -> TestResult {
        let dir = socket_tempdir()?;
        let path = dir.path().join("w.sock");
        let listener =
            UnixListener::bind(&path).map_err(ctx("UnixListener::bind sollte gelingen"))?;
        let server = spawn_body_reader(listener, Duration::from_millis(100), TEST_DEADLINE);
        // Kopf nie abgeschlossen (kein Leerzeilen-Ende); die Schreibhälfte
        // bleibt offen — das Verbindungsende kann nur vom Server kommen.
        let raw = b"GET / HTTP/1.1\r\nHost: localhost\r\n".to_vec();
        let status = send_and_read_status_line(&path, raw).await?;
        assert_eq!(
            status, "",
            "hyper schließt nach Header-Timeout ohne Antwort"
        );
        server.abort();
        Ok(())
    }

    // ── Echter Unix-Socket: Socket-Bind (F-185) ───────────────────────────────

    #[tokio::test]
    async fn test_bind_on_fresh_path_creates_socket() -> TestResult {
        let dir = socket_tempdir()?;
        let path = dir.path().join("w.sock");
        let _listener = bind_unix_listener(&path)
            .await
            .map_err(ctx("freier Pfad bindbar"))?;
        assert!(
            std::fs::symlink_metadata(&path)
                .map_err(ctx("symlink_metadata sollte gelingen"))?
                .file_type()
                .is_socket()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_bind_refuses_live_socket_and_leaves_it_serving() -> TestResult {
        let dir = socket_tempdir()?;
        let path = dir.path().join("w.sock");
        let live = UnixListener::bind(&path).map_err(ctx("UnixListener::bind sollte gelingen"))?;

        let result = bind_unix_listener(&path).await;
        assert!(
            matches!(result, Err(WebError::SocketInUse { .. })),
            "erhalten: {result:?}"
        );

        // Die Datei ist noch da und der ursprüngliche Listener nimmt weiter an.
        assert!(
            std::fs::symlink_metadata(&path)
                .map_err(ctx("symlink_metadata sollte gelingen"))?
                .file_type()
                .is_socket()
        );
        let client = UnixStream::connect(&path)
            .await
            .map_err(ctx("lebender Socket erreichbar"))?;
        let accepted = tokio::time::timeout(TEST_DEADLINE, async {
            // Der Lebendigkeits-`connect` von `bind_unix_listener` liegt ggf.
            // zuerst in der Warteschlange — beide Verbindungen annehmen.
            let first = live.accept().await;
            let second = live.accept().await;
            (first.is_ok(), second.is_ok())
        })
        .await
        .map_err(ctx("ursprünglicher Listener nimmt Verbindungen an"))?;
        assert_eq!(accepted, (true, true));
        drop(client);
        Ok(())
    }

    #[tokio::test]
    async fn test_bind_replaces_dead_socket() -> TestResult {
        let dir = socket_tempdir()?;
        let path = dir.path().join("w.sock");
        // Rest eines abgestürzten Laufs: Socket-Datei ohne Lauscher.
        drop(
            std::os::unix::net::UnixListener::bind(&path)
                .map_err(ctx("UnixListener::bind sollte gelingen"))?,
        );
        assert!(
            std::fs::symlink_metadata(&path)
                .map_err(ctx("symlink_metadata sollte gelingen"))?
                .file_type()
                .is_socket()
        );

        let listener = bind_unix_listener(&path)
            .await
            .map_err(ctx("toter Socket wird ersetzt"))?;
        let client = UnixStream::connect(&path)
            .await
            .map_err(ctx("neuer Socket erreichbar"))?;
        let accepted = tokio::time::timeout(TEST_DEADLINE, listener.accept())
            .await
            .map_err(ctx("neuer Listener nimmt an"))?;
        assert!(accepted.is_ok());
        drop(client);
        Ok(())
    }

    #[tokio::test]
    async fn test_bind_refuses_regular_file_and_leaves_content_unchanged() -> TestResult {
        let dir = socket_tempdir()?;
        let path = dir.path().join("w.sock");
        std::fs::write(&path, b"fremde Daten").map_err(ctx("Datei schreiben sollte gelingen"))?;

        let result = bind_unix_listener(&path).await;
        assert!(
            matches!(result, Err(WebError::SocketPathOccupied { .. })),
            "erhalten: {result:?}"
        );
        assert_eq!(
            std::fs::read(&path).map_err(ctx("Datei lesen sollte gelingen"))?,
            b"fremde Daten"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_bind_refuses_symlink_and_leaves_link_and_target_unchanged() -> TestResult {
        let dir = socket_tempdir()?;
        let target = dir.path().join("ziel.txt");
        let path = dir.path().join("w.sock");
        std::fs::write(&target, b"Ziel bleibt").map_err(ctx("Datei schreiben sollte gelingen"))?;
        std::os::unix::fs::symlink(&target, &path)
            .map_err(ctx("symlink anlegen sollte gelingen"))?;

        let result = bind_unix_listener(&path).await;
        assert!(
            matches!(result, Err(WebError::SocketPathOccupied { .. })),
            "erhalten: {result:?}"
        );
        assert!(
            std::fs::symlink_metadata(&path)
                .map_err(ctx("symlink_metadata sollte gelingen"))?
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read(&target).map_err(ctx("Datei lesen sollte gelingen"))?,
            b"Ziel bleibt"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_bind_refuses_symlink_to_dead_socket_and_removes_nothing() -> TestResult {
        let dir = socket_tempdir()?;
        let target = dir.path().join("tot.sock");
        let path = dir.path().join("w.sock");
        drop(
            std::os::unix::net::UnixListener::bind(&target)
                .map_err(ctx("UnixListener::bind sollte gelingen"))?,
        );
        std::os::unix::fs::symlink(&target, &path)
            .map_err(ctx("symlink anlegen sollte gelingen"))?;

        // Ein `connect` durch den Symlink ergäbe `ECONNREFUSED` — trotzdem
        // darf weder der Link noch sein Ziel verschwinden.
        let result = bind_unix_listener(&path).await;
        assert!(
            matches!(result, Err(WebError::SocketPathOccupied { .. })),
            "erhalten: {result:?}"
        );
        assert!(
            std::fs::symlink_metadata(&path)
                .map_err(ctx("symlink_metadata sollte gelingen"))?
                .file_type()
                .is_symlink()
        );
        assert!(
            std::fs::symlink_metadata(&target)
                .map_err(ctx("symlink_metadata sollte gelingen"))?
                .file_type()
                .is_socket()
        );
        Ok(())
    }

    // ── accept()-Fehler (F-185) ───────────────────────────────────────────────

    #[test]
    fn test_accept_backoff_grows_exponentially_and_is_capped() {
        assert_eq!(accept_backoff(0), ACCEPT_BACKOFF_BASE);
        assert_eq!(accept_backoff(1), ACCEPT_BACKOFF_BASE * 2);
        assert_eq!(accept_backoff(2), ACCEPT_BACKOFF_BASE * 4);
        assert_eq!(accept_backoff(u32::MAX), ACCEPT_BACKOFF_MAX);
        let mut previous = Duration::ZERO;
        for failures in 0..64 {
            let delay = accept_backoff(failures);
            assert!(delay >= previous, "Backoff darf nie schrumpfen");
            assert!(delay <= ACCEPT_BACKOFF_MAX, "Backoff hat eine Obergrenze");
            previous = delay;
        }
    }

    #[test]
    fn test_transient_accept_errors_are_not_fatal() {
        use rustix::io::Errno;
        for errno in [
            Errno::MFILE,
            Errno::NFILE,
            Errno::NOBUFS,
            Errno::NOMEM,
            Errno::CONNABORTED,
            Errno::PROTO,
            Errno::INTR,
            Errno::AGAIN,
            Errno::PERM,
        ] {
            let error = std::io::Error::from_raw_os_error(errno.raw_os_error());
            assert!(!is_fatal_accept_error(&error), "vorübergehend: {error}");
        }
        assert!(!is_fatal_accept_error(&std::io::Error::other(
            "ohne Fehlernummer"
        )));
    }

    #[test]
    fn test_unusable_listener_accept_errors_are_fatal() {
        use rustix::io::Errno;
        for errno in [
            Errno::BADF,
            Errno::NOTSOCK,
            Errno::INVAL,
            Errno::FAULT,
            Errno::OPNOTSUPP,
        ] {
            let error = std::io::Error::from_raw_os_error(errno.raw_os_error());
            assert!(is_fatal_accept_error(&error), "fatal: {error}");
        }
    }

    // ── Echter Unix-Socket: Annahmeschleife Ende-zu-Ende ─────────────────────

    #[tokio::test]
    async fn test_serve_until_answers_over_real_socket_and_stops_on_shutdown() -> TestResult {
        let dir = socket_tempdir()?;
        let path = dir.path().join("w.sock");
        let routes = WebRouteTable::from_registry(&OperationRegistry::new())
            .map_err(ctx("WebRouteTable::from_registry sollte gelingen"))?;
        // Leere Routentabelle: `decide_route` liefert nie `Execute`, die
        // Fabrik wird also nie aufgerufen — kein neutraler Dummy-Rückgabewert
        // möglich, um das per Flag/Assert statt Panik zu belegen: der
        // Rückgabetyp `WebContextFactory = dyn Fn(&PeerCredentials,
        // PermissionTier) -> OpContext + Send + Sync + 'static` (Typalias
        // oben) verlangt einen echten `OpContext`, der wiederum eine echte
        // `harw_authority::SandboxSpec` braucht (`OpContext::new`,
        // harw-operations/src/context.rs). Eine `SandboxSpec` lässt sich nur
        // über eine reale, kanonisierte `WorkspaceBinding` bauen
        // (`SandboxSpec::from_resolved`/`from_resolved_for_test`,
        // harw-authority/src/lib.rs) — `harw-authority` ist keine
        // Abhängigkeit dieser Crate (siehe Cargo.toml) und würde als neue
        // Abhängigkeit gegen den Arbeitsauftrag verstoßen. `WebContextFactory`
        // ist zudem Produktionscode ohne `Result`-Variante; eine
        // Signaturänderung bräche alle Aufrufer außerhalb dieser Datei.
        // `unreachable!` bleibt daher bewusst stehen; identisches, ebenso
        // begründetes Muster in `harw-web/tests/method_admission.rs`.
        let context_factory: Arc<WebContextFactory> = Arc::new(
            |_peer: &PeerCredentials, _tier: PermissionTier| -> OpContext {
                unreachable!("leere Routentabelle: decide_route liefert nie Execute")
            },
        );
        let server = BoundWebServer::bind(
            WebServerConfig {
                socket_path: path.clone(),
            },
            routes,
            Arc::new(StaticUidTierMap::with_default(
                vec![],
                PermissionTier::Observer,
            )),
            context_factory,
            Arc::new(WebEventBus::new(8).map_err(ctx("WebEventBus::new sollte gelingen"))?),
        )
        .await
        .map_err(ctx("Tempdir-Socket bindbar"))?;
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let client = async {
            let raw = b"GET /api/gibt-es-nicht HTTP/1.1\r\nHost: localhost\r\n\r\n".to_vec();
            let status = send_and_read_status_line(&path, raw).await?;
            shutdown_tx
                .send(true)
                .map_err(ctx("Server hält den Empfänger"))?;
            Ok(status)
        };
        let (served, status) = tokio::time::timeout(TEST_DEADLINE, async {
            tokio::join!(server.serve_until(shutdown_rx), client)
        })
        .await
        .map_err(ctx("Server endet nach Shutdown-Signal"))?;
        let status = status?;
        assert!(
            status.starts_with("HTTP/1.1 404"),
            "erwartet 404, erhalten: {status:?}"
        );
        assert!(served.is_ok());
        Ok(())
    }
}
