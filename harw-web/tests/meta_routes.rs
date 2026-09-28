//! Integrationstests der Metafläche `/v1/health`, `/v1/version`,
//! `/v1/capabilities` (Crypto-Masterplan v2 §38, H9).
//!
//! Prüft über einen **echten** Unix-Socket und den produktiven
//! [`BoundWebServer`]:
//! - die drei Dokumente tragen den Vertrag von `harw-infra-client::info`
//!   (`health {status, service}`, `version {service, version, protocol}`,
//!   `capabilities {service, protocol, operations}`);
//! - `operations` sind die registrierten Web-Operationen (sortiert, ohne
//!   Duplikate);
//! - `/v1/health` antwortet auch einem unbekannten Peer, `/v1/version` und
//!   `/v1/capabilities` nicht (`403 unknown_peer`);
//! - nur `GET` (`POST` → `405` mit `Allow: GET`);
//! - ein über [`BoundWebServer::from_std_listener`] übernommener Listener
//!   (der systemd-Aktivierungspfad) bedient dieselbe Fläche.
//!
//! Keine der Anfragen erreicht `Execute`; die Kontextfabrik ist daher
//! `unreachable!` — dasselbe begründete Muster wie in `method_admission.rs`.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{TestError, TestResult, ctx};
use harw_operations::context::OpContext;
use harw_operations::operation::{
    ApprovalPolicy, BusyAvailability, OpFuture, OpInput, OpOutput, Operation, OperationCategory,
    OperationDomain, OperationMeta, PermissionTier, Surface, WebMethod,
};
use harw_operations::registry::OperationRegistry;
use harw_web::authz::{PeerAuthorizer, StaticUidTierMap};
use harw_web::events::WebEventBus;
use harw_web::meta::{PROTOCOL_VERSION, SERVICE_NAME};
use harw_web::peer::PeerCredentials;
use harw_web::router::WebRouteTable;
use harw_web::server::{BoundWebServer, WebContextFactory, WebServerConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Obergrenze, die ein einzelner Socket-Test insgesamt warten darf.
const TEST_DEADLINE: Duration = Duration::from_secs(10);

/// Testoperation mit einer oder mehreren `Surface::Web`-Deklarationen.
struct WebOp {
    meta: OperationMeta,
}

impl Operation for WebOp {
    fn meta(&self) -> &OperationMeta {
        &self.meta
    }

    fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
        Box::pin(async {
            Ok(OpOutput {
                text: "ausgeführt".to_owned(),
                data: None,
            })
        })
    }
}

fn web_op(name: &'static str, paths: &[&'static str]) -> Arc<dyn Operation> {
    Arc::new(WebOp {
        meta: OperationMeta {
            name,
            summary: "Metaflächen-Testoperation.",
            domain: OperationDomain::Misc,
            permission: PermissionTier::Observer,
            surfaces: paths
                .iter()
                .copied()
                .map(|path| Surface::Web {
                    path,
                    method: WebMethod::Get,
                    approval: ApprovalPolicy::None,
                })
                .collect(),
            aliases: &[],
            category: OperationCategory::Misc,
            args_schema: None,
            output_schema: None,
            busy: BusyAvailability::DeferredUntilTurnEnd,
        },
    })
}

/// `zeta.op` mit zwei Pfaden, `alpha.op` mit einem — prüft Sortierung und
/// Deduplizierung.
fn registry() -> OperationRegistry {
    let mut registry = OperationRegistry::new();
    registry.register(web_op("zeta.op", &["/api/zeta", "/api/zeta-alt"]));
    registry.register(web_op("alpha.op", &["/api/alpha"]));
    registry
}

fn unreachable_factory() -> Arc<WebContextFactory> {
    // Metarouten sind keine Operationen; `Execute` wird nie erreicht. Ein
    // neutraler `OpContext` ist ohne `SandboxSpec` (`harw-authority`, keine
    // Abhängigkeit von `harw-web`) nicht baubar — siehe method_admission.rs.
    Arc::new(
        |_peer: &PeerCredentials, _tier: PermissionTier| -> OpContext {
            unreachable!("Metarouten dürfen nie bis zur Ausführung gelangen")
        },
    )
}

/// Eine Antwort: Statuszeile, Kopfzeilen, Rumpf.
struct Reply {
    head: String,
    body: Vec<u8>,
}

impl Reply {
    fn status_is(&self, code: u16) -> bool {
        self.head.starts_with(&format!("HTTP/1.1 {code}"))
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then_some(value.trim())
        })
    }

    fn json(&self) -> TestResult<serde_json::Value> {
        serde_json::from_slice(&self.body).map_err(ctx("Rumpf ist JSON"))
    }
}

/// Schickt `method path` mit `Connection: close` und liest bis EOF.
async fn request(socket: &Path, method: &str, path: &str) -> TestResult<Reply> {
    let mut stream = UnixStream::connect(socket)
        .await
        .map_err(ctx("Testserver lauscht am Tempdir-Socket"))?;
    let raw = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(raw.as_bytes())
        .await
        .map_err(ctx("Anfrage schreibbar"))?;
    let mut buffer = Vec::new();
    stream
        .read_to_end(&mut buffer)
        .await
        .map_err(ctx("Antwort lesbar"))?;
    let split = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or(TestError::Missing("Kopfende der Antwort"))?;
    let head = String::from_utf8_lossy(&buffer[..split]).into_owned();
    let body = buffer[split + 4..].to_vec();
    Ok(Reply { head, body })
}

/// Bedient `server` und schickt nacheinander `requests`.
async fn run(
    server: BoundWebServer,
    socket: &Path,
    requests: &[(&'static str, &'static str)],
) -> TestResult<Vec<Reply>> {
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let client = async {
        let mut replies = Vec::with_capacity(requests.len());
        for (method, path) in requests {
            replies.push(request(socket, method, path).await?);
        }
        shutdown_tx
            .send(true)
            .map_err(ctx("Server hält den Empfänger"))?;
        Ok::<Vec<Reply>, TestError>(replies)
    };
    let (served, replies) = tokio::time::timeout(TEST_DEADLINE, async {
        tokio::join!(server.serve_until(shutdown_rx), client)
    })
    .await
    .map_err(ctx("Server endet nach Shutdown-Signal"))?;
    assert!(served.is_ok(), "serve_until muss sauber enden");
    replies
}

async fn bound_server(
    socket: &Path,
    authorizer: Arc<dyn PeerAuthorizer>,
) -> TestResult<BoundWebServer> {
    let routes = WebRouteTable::from_registry(&registry()).map_err(ctx("Routentabelle baubar"))?;
    BoundWebServer::bind(
        WebServerConfig {
            socket_path: socket.to_path_buf(),
        },
        routes,
        authorizer,
        unreachable_factory(),
        Arc::new(WebEventBus::new(8).map_err(ctx("Kapazität > 0"))?),
    )
    .await
    .map_err(ctx("Tempdir-Socket bindbar"))
}

fn assert_contract(replies: &[Reply]) -> TestResult {
    let [health, version, capabilities] = replies else {
        return Err(TestError::Unexpected(format!(
            "drei Antworten erwartet, erhalten: {}",
            replies.len()
        )));
    };
    for reply in replies {
        assert!(reply.status_is(200), "erwartet 200: {:?}", reply.head);
        assert_eq!(reply.header("content-type"), Some("application/json"));
    }

    let health = health.json()?;
    assert_eq!(health["status"], "ok");
    assert_eq!(health["service"], SERVICE_NAME);

    let version = version.json()?;
    assert_eq!(version["service"], SERVICE_NAME);
    assert_eq!(version["protocol"], PROTOCOL_VERSION);
    assert!(
        version["version"].as_str().is_some_and(|v| !v.is_empty()),
        "version ist eine nicht-leere Zeichenkette: {version}"
    );

    let capabilities = capabilities.json()?;
    assert_eq!(capabilities["service"], SERVICE_NAME);
    assert_eq!(capabilities["protocol"], PROTOCOL_VERSION);
    assert_eq!(
        capabilities["operations"],
        serde_json::json!(["alpha.op", "zeta.op"]),
        "registrierte Web-Operationen, sortiert und ohne Duplikate"
    );
    Ok(())
}

const META_GETS: [(&str, &str); 3] = [
    ("GET", "/v1/health"),
    ("GET", "/v1/version"),
    ("GET", "/v1/capabilities"),
];

#[tokio::test]
async fn test_meta_routes_serve_the_infra_client_contract() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("Tempdir anlegbar"))?;
    let socket = dir.path().join("control.sock");
    let authorizer = Arc::new(StaticUidTierMap::with_default(
        vec![],
        PermissionTier::Observer,
    ));
    let server = bound_server(&socket, authorizer).await?;
    let replies = run(server, &socket, &META_GETS).await?;
    assert_contract(&replies)
}

#[tokio::test]
async fn test_health_needs_no_tier_but_version_and_capabilities_do() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("Tempdir anlegbar"))?;
    let socket = dir.path().join("control.sock");
    // Kein Peer hat eine Stufe.
    let authorizer = Arc::new(StaticUidTierMap::new(vec![]));
    let server = bound_server(&socket, authorizer).await?;
    let replies = run(server, &socket, &META_GETS).await?;
    let [health, version, capabilities] = replies.as_slice() else {
        return Err(TestError::Missing("drei Antworten"));
    };
    assert!(health.status_is(200), "health: {:?}", health.head);
    assert_eq!(health.json()?["status"], "ok");
    for reply in [version, capabilities] {
        assert!(reply.status_is(403), "erwartet 403: {:?}", reply.head);
        assert_eq!(reply.json()?["reason"], "unknown_peer");
    }
    Ok(())
}

#[tokio::test]
async fn test_meta_routes_accept_only_get() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("Tempdir anlegbar"))?;
    let socket = dir.path().join("control.sock");
    let authorizer = Arc::new(StaticUidTierMap::with_default(
        vec![],
        PermissionTier::Owner,
    ));
    let server = bound_server(&socket, authorizer).await?;
    let replies = run(
        server,
        &socket,
        &[
            ("POST", "/v1/health"),
            ("HEAD", "/v1/version"),
            ("DELETE", "/v1/capabilities"),
        ],
    )
    .await?;
    for reply in &replies {
        assert!(reply.status_is(405), "erwartet 405: {:?}", reply.head);
        assert_eq!(reply.header("allow"), Some("GET"));
    }
    Ok(())
}

/// Der Aktivierungspfad: ein bereits lauschender `std`-Listener (wie ihn
/// `harw_web::systemd::listener_from_systemd` liefert) wird übernommen und
/// bedient dieselbe Fläche.
#[tokio::test]
async fn test_adopted_std_listener_serves_meta_routes() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("Tempdir anlegbar"))?;
    let socket = dir.path().join("activated.sock");
    let listener =
        std::os::unix::net::UnixListener::bind(&socket).map_err(ctx("std-Listener bindbar"))?;
    let routes = WebRouteTable::from_registry(&registry()).map_err(ctx("Routentabelle baubar"))?;
    let server = BoundWebServer::from_std_listener(
        listener,
        routes,
        Arc::new(StaticUidTierMap::with_default(
            vec![],
            PermissionTier::Observer,
        )),
        unreachable_factory(),
        Arc::new(WebEventBus::new(8).map_err(ctx("Kapazität > 0"))?),
    )
    .map_err(ctx("Listener übernehmbar"))?;
    assert_eq!(server.socket_path(), socket.as_path());
    let replies = run(server, &socket, &META_GETS).await?;
    assert_contract(&replies)
}
