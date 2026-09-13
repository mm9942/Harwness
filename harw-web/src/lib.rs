//! Transport und `Surface::Web` der Control Plane — die vierte
//! Expositionsfläche neben Command (`harw-tui`), ModelTool (`harw-core`) und
//! AgentTool (`harw-core-bridge`).
//!
//! # Verantwortungsbereich
//! `harw-web` vermittelt **ausschließlich vorhandene Operationen** — es
//! definiert selbst keine. Es bindet auf einen Unix-Socket, nimmt
//! HTTP-Anfragen entgegen, identifiziert den Aufrufer über die
//! Kernel-verbürgten Peer-Credentials dieses Sockets, prüft dessen
//! Berechtigungsstufe gegen die von der aufgerufenen Operation verlangte
//! Mindeststufe, und reicht freigegebene Aufrufe an
//! [`harw_operations::adapter::WebAdapter::invoke`] weiter. Es führt selbst
//! keine Geschäftslogik aus, rendert kein Markdown und erzeugt keine aktiven
//! Links (das ist UI-01s Sache) und baut keine eigene Genehmigungsfläche
//! (das ist UI-06s Sache).
//!
//! # Warum es keinen zweiten Autoritätspfad gibt — und wie das erzwungen wird
//! Jede über `harw-web` erreichbare Operation **muss** eine
//! `Surface::Web`-Deklaration in ihrer
//! [`harw_operations::operation::OperationMeta`] tragen. Das ist keine
//! Konvention, sondern strukturell erzwungen:
//!
//! - [`router::WebRouteTable`] hat **einen** öffentlichen Konstruktor,
//!   [`router::WebRouteTable::from_registry`], der ausschließlich eine
//!   [`harw_operations::registry::OperationRegistry`] entgegennimmt.
//! - Er baut Einträge intern nur über
//!   [`harw_operations::adapter::WebAdapter::from_operation`], und dieser
//!   wiederum nur aus `Arc<dyn harw_operations::operation::Operation>` — der
//!   `Operation`-Trait erzwingt `meta()`, es gibt also keinen Wert dieses
//!   Typs ohne `OperationMeta`.
//! - Es gibt in dieser Crate **keine** Methode, die einen rohen Pfad-String
//!   und eine Callback-Funktion entgegennimmt und daraus eine Route baut.
//!
//! Eine Route ohne Metadaten ist damit nicht nur ungeprüft, sondern
//! **unausdrückbar** — siehe `router`-Moduldoku für die Belegtests. Genau
//! dieselbe Disziplin gilt für die Ausführung selbst:
//! [`server::handle`] (privat) trifft **keine eigene** Zugriffsentscheidung
//! — jede kommt aus [`router::decide_route`], das ausschließlich
//! [`authz::PeerAuthorizer`] (serverseitig vertraute, zur Komposition
//! übergebene Konfiguration — nie aus dem Request abgeleitet) und
//! [`router::WebRouteTable`] befragt.
//!
//! # `SO_PEERCRED` statt Bearer-Token
//! Die Bindung ist ein Unix-Socket, kein TCP-Port. [`peer::read_peer_credentials`]
//! liest die Kernel-verbürgte Identität (`pid`/`uid`/`gid`) einer
//! angenommenen Verbindung über `rustix::net::sockopt::socket_peercred` —
//! dasselbe Verfahren wie `harw_sentinel::ipc` (siehe dortige Moduldoku).
//! Es gibt keinen Token im HTTP-Rumpf oder -Header, der die Identität eines
//! Aufrufers bestimmt.
//!
//! # Sequenznummer im Ereignisstrom
//! `GET /events` liefert Server-Sent Events; jedes [`events::WebEvent`]
//! trägt ein monoton steigendes `sequence: u64` (siehe `events`-Moduldoku).
//! Ein Client, dessen Verbindung abreißt und neu verbindet, erkennt an
//! einer Lücke in der Sequenz sofort, dass er Ereignisse verpasst hat — ein
//! Strom ohne Nummern wäre von einem lückenhaften nicht zu unterscheiden.
//!
//! # Verhältnis zu den zwei CI-Gates
//! Ab diesem Knoten gelten zwei Gates: *keine Route ohne `OperationMeta`*
//! (siehe oben — strukturell erzwungen, nicht nur geprüft) und *die
//! Tier-Ablehnungsmatrix* (siehe `authz`- und `router`-Moduldoku:
//! [`authz::tier_permits`] und [`router::decide_route`] sind die einzigen
//! Stellen, die diese Entscheidung treffen).
//!
//! # Nebenläufigkeit
//! [`server::BoundWebServer::serve_until`] bedient jede angenommene
//! Verbindung in einer eigenen `tokio::task`, gesammelt in einem
//! `tokio::task::JoinSet`, damit ein Herunterfahren alle Verbindungs-Tasks
//! geordnet abbricht statt sie herrenlos weiterlaufen zu lassen.
//! [`events::WebEventBus`] ist `Clone + Send + Sync` und über `Arc` geteilt.
//! [`authz::PeerAuthorizer`]-Implementierungen müssen `Send + Sync` sein.
//!
//! # Fehlertypen
//! [`error::WebError`] — Binde-, Annahme-, Peer-Credential-, Routen- und
//! Ereignisbus-Fehler. Fehler einzelner HTTP-Anfragen (ungültiges JSON,
//! `OpError` einer Operation) werden **nie** als `WebError` propagiert —
//! sie werden als HTTP-Antwort mit passendem Statuscode beantwortet (siehe
//! `server`-Moduldoku).
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
//! // `registry` wird vom Binary befüllt (z. B. via `harw_ops::register_all`) —
//! // harw-web selbst hängt von keiner konkreten Operations-Crate ab.
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
//!
//! # Was `harw-web` nicht tut
//! - **Keine Ausführung.** Es nimmt entgegen und reicht weiter — die
//!   eigentliche Ausführung bleibt in [`harw_operations::operation::Operation::run`].
//! - **Keine eigene Genehmigungsfläche.** Eine Route mit
//!   `approval != ApprovalPolicy::None` wird nie ausgeführt — sie liefert
//!   `403` mit `reason: "approval_required"` (siehe `router::RouteDecision::ApprovalRequired`).
//!   Die tatsächliche Genehmigung ist UI-06s Sache.
//! - **Kein Markdown-Rendering, keine aktiven Links.** `OpOutput::text`
//!   wird unverändert als JSON-Zeichenkette weitergereicht.
//!
//! # Stand
//! Gerüst aus Knoten AW0-00 (Workspace-Fundament); Inhalt aus Knoten
//! **UI-00**; Ebene **L6** im Zielgraphen.

#![forbid(unsafe_code)]

pub mod authz;
pub mod error;
pub mod events;
pub mod peer;
pub mod router;
pub mod security;
pub mod server;

pub use authz::{PeerAuthorizer, StaticUidTierMap, tier_permits};
pub use error::{WebError, WebResult};
pub use events::{WebEvent, WebEventBus, WebEventKind, WebEventReceiveError, WebEventSubscription};
pub use peer::{PeerCredentials, read_peer_credentials};
pub use router::{ForbiddenReason, RouteDecision, WebMethod, WebRouteTable, decide_route};
pub use security::{ApprovalActorResolver, StaticUidApprovalActorMap, resolve_approval};
pub use server::{BoundWebServer, WebContextFactory, WebServerConfig};
