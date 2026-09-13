//! Unix-Socket-HTTP-Transport — bindet, nimmt Verbindungen an, reicht an
//! [`crate::router::decide_route`] weiter.
//!
//! # Verantwortungsbereich
//! Dieses Modul enthält den einzigen Ort dieser Crate, der `hyper` und
//! einen echten Socket berührt. Es **entscheidet nichts selbst** — jede
//! Zugriffsentscheidung kommt von [`crate::router::decide_route`], jede
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
//! # Zwei CI-Gates
//! „Keine Route ohne `OperationMeta`" und „Tier-Ablehnungsmatrix" (siehe
//! `crate::router`-Moduldoku) werden von diesem Modul **konsumiert**, nicht
//! implementiert: [`handle`] fragt ausschließlich
//! [`crate::router::decide_route`] und führt nie eine Operation aus, die
//! dieses nicht als [`crate::router::RouteDecision::Execute`] freigegeben hat.
//!
//! # Nebenläufigkeit
//! Jede angenommene Verbindung läuft in einer eigenen `tokio::task` (über
//! ein `JoinSet`, damit [`BoundWebServer::serve_until`] beim Herunterfahren
//! alle Verbindungs-Tasks geordnet abbricht und einsammelt statt sie
//! herrenlos weiterlaufen zu lassen).
//!
//! # Fehlertypen
//! [`crate::error::WebError::Bind`] beim Binden, [`crate::error::WebError::Accept`]
//! bei einem dauerhaft scheiternden `accept()`. Fehler einzelner Anfragen
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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Channel, Full};
use hyper::body::Incoming;
use hyper::header::{ACCEPT, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use tokio::net::UnixListener;
use tokio::sync::watch;
use tokio::task::JoinSet;

use harw_operations::context::OpContext;
use harw_operations::error::OpError;
use harw_operations::operation::PermissionTier;

use crate::authz::PeerAuthorizer;
use crate::error::WebError;
use crate::events::{WebEventBus, WebEventKind, WebEventReceiveError};
use crate::peer::{PeerCredentials, read_peer_credentials};
use crate::router::{ForbiddenReason, RouteDecision, WebMethod, WebRouteTable, decide_route};

/// Obergrenze eines HTTP-Rumpfs (Anfrage-JSON für eine `Surface::Web`-Route).
const MAX_BODY_BYTES: usize = 64 * 1024;
/// Der reservierte Pfad des SSE-Ereignisstroms — kollidiert nie mit einer
/// `Surface::Web`-Route, da `Surface::Web::path` von der jeweiligen
/// Operation kommt und `harw-web` diesen Pfad nie an
/// [`crate::router::WebRouteTable::from_registry`] übergibt.
const EVENTS_PATH: &str = "/events";
/// Abstand zwischen zwei [`WebEventKind::Heartbeat`]-Ereignissen.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

type WebResponse = Response<BoxBody<Bytes, Infallible>>;

/// Baut den [`OpContext`] für eine freigegebene Ausführung.
///
/// # Description
/// Wird **einmal pro Aufruf** unmittelbar vor [`harw_operations::adapter::WebAdapter::invoke`]
/// aufgerufen — nie vorher, damit kein `OpContext` für eine Route gebaut
/// wird, die [`crate::router::decide_route`] gar nicht freigegeben hat. Die
/// Implementierung MUSS `sandbox`/`services` aus serverseitig vertrauter
/// Konfiguration ableiten, niemals aus dem HTTP-Request.
pub type WebContextFactory = dyn Fn(&PeerCredentials, PermissionTier) -> OpContext + Send + Sync + 'static;

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
    authorizer: Arc<dyn PeerAuthorizer>,
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
    /// - `authorizer` (`Arc<dyn PeerAuthorizer>`): serverseitig vertraute Peer→Tier-Richtlinie.
    /// - `context_factory` (`Arc<WebContextFactory>`): baut `OpContext` je freigegebenem Aufruf.
    /// - `events` (`Arc<WebEventBus>`): geteilter Ereignisbus für `GET /events`.
    ///
    /// # Returns
    /// Einen gebundenen, noch nicht bedienenden `BoundWebServer`.
    ///
    /// # Errors
    /// [`WebError::Bind`], wenn der Socket-Pfad nicht gebunden werden kann
    /// (Verzeichnis fehlt, keine Berechtigung, Pfad bereits durch einen
    /// anderen Prozess belegt).
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
        let path_display = config.socket_path.display().to_string();
        let bind_err = || WebError::Bind {
            path: path_display.clone(),
        };
        // Rest eines vorherigen, nicht sauber beendeten Laufs entfernen —
        // dasselbe Vorgehen wie `harw_sentinel::ipc::IpcListener::bind`.
        let _ = std::fs::remove_file(&config.socket_path);
        let listener = UnixListener::bind(&config.socket_path).map_err(|_| bind_err())?;
        Ok(Self {
            listener,
            socket_path: config.socket_path,
            routes: Arc::new(routes),
            authorizer,
            context_factory,
            events,
        })
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
    /// [`WebError::Accept`], wenn `accept()` dauerhaft scheitert.
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
    /// # Arguments
    /// - `shutdown` (`tokio::sync::watch::Receiver<bool>`): wird auf `true`
    ///   gesetzt, um die Schleife geordnet zu beenden.
    ///
    /// # Errors
    /// [`WebError::Accept`], wenn `accept()` dauerhaft scheitert (z. B. der
    /// Socket extern geschlossen wird).
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
                accepted = self.listener.accept() => {
                    let (stream, _addr) = accepted.map_err(|_| WebError::Accept)?;
                    let Ok(peer) = read_peer_credentials(&stream) else {
                        // Ohne Peer-Identität keine einzige HTTP-Anfrage —
                        // die Verbindung wird beim Drop des Streams geschlossen.
                        continue;
                    };
                    let routes = Arc::clone(&self.routes);
                    let authorizer = Arc::clone(&self.authorizer);
                    let context_factory = Arc::clone(&self.context_factory);
                    let events = Arc::clone(&self.events);
                    connections.spawn(async move {
                        let _ = Builder::new(TokioExecutor::new())
                            .serve_connection(
                                TokioIo::new(stream),
                                service_fn(move |request| {
                                    handle(
                                        request,
                                        peer,
                                        Arc::clone(&routes),
                                        Arc::clone(&authorizer),
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
        Ok(())
    }
}

/// Bearbeitet eine einzelne HTTP-Anfrage einer bereits identifizierten Verbindung.
///
/// # Description
/// Trifft selbst keine Zugriffsentscheidung — delegiert vollständig an
/// [`decide_route`] und übersetzt dessen Ergebnis in eine HTTP-Antwort.
/// `GET /events` ist der einzige Pfad, den diese Funktion ohne Umweg über
/// die Routentabelle behandelt (der Ereignisstrom ist keine Operation).
async fn handle(
    request: Request<Incoming>,
    peer: PeerCredentials,
    routes: Arc<WebRouteTable>,
    authorizer: Arc<dyn PeerAuthorizer>,
    context_factory: Arc<WebContextFactory>,
    events: Arc<WebEventBus>,
) -> Result<WebResponse, Infallible> {
    if request.uri().path() == EVENTS_PATH {
        return Ok(handle_events(&request, &peer, authorizer.as_ref(), &events));
    }

    let method = match *request.method() {
        Method::GET => Some(WebMethod::Get),
        Method::POST => Some(WebMethod::Post),
        _ => None,
    };
    let path = request.uri().path().to_owned();
    let decision = decide_route(&routes, authorizer.as_ref(), &peer, &path, method);

    match decision {
        RouteDecision::NotFound => Ok(json_response(
            StatusCode::NOT_FOUND,
            &serde_json::json!({"error": "not_found"}),
        )),
        RouteDecision::MethodNotAllowed { expected } => Ok(method_not_allowed_response(expected)),
        RouteDecision::Forbidden { reason } => Ok(json_response(
            StatusCode::FORBIDDEN,
            &serde_json::json!({"error": "forbidden", "reason": forbidden_reason_str(reason)}),
        )),
        RouteDecision::ApprovalRequired { route } => Ok(json_response(
            StatusCode::FORBIDDEN,
            &serde_json::json!({
                "error": "forbidden",
                "reason": "approval_required",
                "operation": route.operation_name(),
            }),
        )),
        RouteDecision::Execute { route, caller_tier } => {
            let args = match read_json_body(request).await {
                Ok(value) => value,
                Err(response) => return Ok(response),
            };
            let ctx = (context_factory)(&peer, caller_tier);
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

/// Bearbeitet `GET /events` — keine Operation, keine Route, nur der
/// Ereignisstrom.
fn handle_events(
    request: &Request<Incoming>,
    peer: &PeerCredentials,
    authorizer: &dyn PeerAuthorizer,
    events: &Arc<WebEventBus>,
) -> WebResponse {
    if *request.method() != Method::GET {
        return method_not_allowed_response(WebMethod::Get);
    }
    let accepts_event_stream = request
        .headers()
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
    // Ein unbekannter Peer sieht keinen Ereignisstrom — dieselbe Regel wie
    // für jede andere Route: keine Autorisierung ohne aufgelöste Stufe.
    if authorizer.tier_for(peer).is_none() {
        return json_response(
            StatusCode::FORBIDDEN,
            &serde_json::json!({"error": "forbidden", "reason": forbidden_reason_str(ForbiddenReason::UnknownPeer)}),
        );
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
/// Prüft `Content-Length` (falls vorhanden) und die tatsächliche
/// Rumpfgröße gegen [`MAX_BODY_BYTES`], bevor überhaupt dekodiert wird. Ein
/// leerer Rumpf (typisch für `GET`) wird als `serde_json::Value::Null`
/// behandelt — dieselbe Bedeutung wie bei einer `Surface::ModelTool`-Fläche
/// ohne Argumente.
///
/// # Returns
/// - `Ok(value)`: dekodierte JSON-Argumente.
/// - `Err(response)`: eine fertige Fehlerantwort (400/413), die `handle`
///   unverändert zurückgibt.
async fn read_json_body(request: Request<Incoming>) -> Result<serde_json::Value, WebResponse> {
    let declared_len = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared_len.is_some_and(|len| len > MAX_BODY_BYTES as u64) {
        return Err(json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            &serde_json::json!({"error": "payload_too_large"}),
        ));
    }
    let bytes = match request.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => {
            return Err(json_response(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({"error": "bad_request"}),
            ));
        }
    };
    if bytes.len() > MAX_BODY_BYTES {
        return Err(json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            &serde_json::json!({"error": "payload_too_large"}),
        ));
    }
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
/// crate-Moduldoku). Trägt zusätzlich `trust`: `OpOutput` liefert keine
/// `TrustClass` (siehe `crate::events`-Moduldoku), daher immer
/// [`harw_context::TrustClass::Data`] — die niedrigste Klasse, nie eine
/// erfundene.
fn op_result_response(result: Result<harw_operations::operation::OpOutput, OpError>) -> WebResponse {
    match result {
        Ok(output) => json_response(
            StatusCode::OK,
            &serde_json::json!({
                "text": output.text,
                "trust": harw_context::TrustClass::Data,
            }),
        ),
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
fn method_not_allowed_response(expected: WebMethod) -> WebResponse {
    let mut response = json_response(
        StatusCode::METHOD_NOT_ALLOWED,
        &serde_json::json!({"error": "method_not_allowed", "expected": expected.as_str()}),
    );
    if let Ok(value) = HeaderValue::from_str(expected.as_str()) {
        response.headers_mut().insert(hyper::header::ALLOW, value);
    }
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
    use super::{forbidden_reason_str, json_response, method_not_allowed_response, status_for_op_error};
    use crate::router::{ForbiddenReason, WebMethod};
    use harw_operations::error::OpError;
    use hyper::StatusCode;

    // Diese Tests berühren ausschließlich in-memory `Response`/`StatusCode`-Werte —
    // kein Socket, keine Netzwerkverbindung (siehe Modul- und Crate-Moduldoku).

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
    async fn test_json_response_body_round_trips_through_serde() {
        use http_body_util::BodyExt;
        let payload = serde_json::json!({"text": "hallo"});
        let response = json_response(StatusCode::OK, &payload);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("in-memory body always resolves")
            .to_bytes();
        let decoded: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, payload);
    }
}
