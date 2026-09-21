//! Integrationstests für die Methoden-Zulassung von `harw-web` (F-031).
//!
//! Prüft über einen **echten** Unix-Socket und den produktiven
//! [`BoundWebServer`], dass die HTTP-Methode ausschließlich aus
//! `Surface::Web { method, .. }` stammt:
//! - `GET` auf eine `POST`-Route → `405` mit `Allow: POST`, die Operation läuft nie;
//! - `POST` auf eine `GET`-Route → `405` mit `Allow: GET`;
//! - `HEAD`/`OPTIONS` werden nie implizit auf eine Route abgebildet → `405`;
//! - die Routentabelle übernimmt die deklarierte Methode aus der Registry, und
//!   die korrekte Methode wird bis zur Ausführungsentscheidung durchgelassen.
//!
//! Eine vollständige Ausführung über den Server ist hier nicht möglich
//! (`OpContext` braucht eine `SandboxSpec` aus `harw-sandbox`, keine
//! Abhängigkeit von `harw-web`, siehe Ledger W1-11). Die Kontextfabrik der
//! Socket-Tests ist deshalb `unreachable!`: würde eine abgelehnte Methode doch
//! dispatcht, bräche die Verbindungs-Task ab und der Test erhielte keine
//! `405`-Statuszeile. Der positive Dispatch wird über
//! [`harw_web::router::decide_route`] belegt.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use harw_operations::context::OpContext;
use harw_operations::operation::{
    ApprovalPolicy, BusyAvailability, OpFuture, OpInput, OpOutput, Operation, OperationCategory,
    OperationDomain, OperationMeta, PermissionTier, Surface, WebMethod,
};
use harw_operations::registry::OperationRegistry;
use harw_web::authz::StaticUidTierMap;
use harw_web::events::WebEventBus;
use harw_web::peer::PeerCredentials;
use harw_web::router::{RouteDecision, WebRouteTable, decide_route};
use harw_web::server::{BoundWebServer, WebContextFactory, WebServerConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Obergrenze, die ein einzelner Socket-Test insgesamt warten darf.
const TEST_DEADLINE: Duration = Duration::from_secs(10);

/// Testoperation mit deklarierter Methode; zählt jeden `run`-Aufruf.
struct CountingOp {
    meta: OperationMeta,
    runs: Arc<AtomicUsize>,
}

impl Operation for CountingOp {
    fn meta(&self) -> &OperationMeta {
        &self.meta
    }

    fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(OpOutput {
                text: "ausgeführt".to_owned(),
                data: None,
            })
        })
    }
}

fn counting_op(
    name: &'static str,
    path: &'static str,
    method: WebMethod,
    runs: &Arc<AtomicUsize>,
) -> Arc<dyn Operation> {
    Arc::new(CountingOp {
        meta: OperationMeta {
            name,
            summary: "Methoden-Testoperation.",
            domain: OperationDomain::Misc,
            permission: PermissionTier::Observer,
            surfaces: vec![Surface::Web {
                path,
                method,
                approval: ApprovalPolicy::None,
            }],
            aliases: &[],
            category: OperationCategory::Misc,
            args_schema: None,
            output_schema: None,
            busy: BusyAvailability::DeferredUntilTurnEnd,
        },
        runs: Arc::clone(runs),
    })
}

/// Registry mit `/api/write` (POST) und `/api/read` (GET).
fn registry(runs: &Arc<AtomicUsize>) -> OperationRegistry {
    let mut registry = OperationRegistry::new();
    registry.register(counting_op("write-op", "/api/write", WebMethod::Post, runs));
    registry.register(counting_op("read-op", "/api/read", WebMethod::Get, runs));
    registry
}

/// Liest die Antwortköpfe bis zur Leerzeile (oder bis EOF/Lesefehler).
async fn read_head(stream: &mut UnixStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            return String::from_utf8_lossy(&buffer[..end]).into_owned();
        }
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return String::from_utf8_lossy(&buffer).into_owned(),
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
}

/// Schickt eine Anfrage `method path` (mit optionalem JSON-Rumpf) an den
/// Server unter `socket` und liefert den Antwortkopf als Text.
async fn request_head(socket: &Path, method: &str, path: &str, body: &str) -> String {
    let mut stream = UnixStream::connect(socket)
        .await
        .expect("Testserver lauscht am Tempdir-Socket");
    let raw = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(raw.as_bytes())
        .await
        .expect("Anfrage auf frischen Socket schreibbar");
    read_head(&mut stream).await
}

/// Liefert den Wert des Kopfes `name` (case-insensitiv) aus einem Antwortkopf.
fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if key.trim().eq_ignore_ascii_case(name) {
            Some(value.trim())
        } else {
            None
        }
    })
}

/// Startet den produktiven Server mit `registry`, führt `requests` nacheinander
/// aus und liefert deren Antwortköpfe.
async fn run_against_server(
    registry: &OperationRegistry,
    requests: &[(&'static str, &'static str, &'static str)],
) -> Vec<String> {
    let dir = tempfile::tempdir().expect("Tempdir für den Test-Socket anlegbar");
    let socket = dir.path().join("m.sock");
    let routes = WebRouteTable::from_registry(registry).expect("Routentabelle baubar");
    // Jede hier geschickte Anfrage muss vor `Execute` abgelehnt werden; ein
    // Aufruf der Fabrik ist ein F-031-Regressionsbefund.
    let context_factory: Arc<WebContextFactory> =
        Arc::new(|_peer: &PeerCredentials, _tier: PermissionTier| -> OpContext {
            unreachable!("abgelehnte Methode darf nie bis zur Ausführung gelangen")
        });
    let server = BoundWebServer::bind(
        WebServerConfig {
            socket_path: socket.clone(),
        },
        routes,
        Arc::new(StaticUidTierMap::with_default(vec![], PermissionTier::Owner)),
        context_factory,
        Arc::new(WebEventBus::new(8).expect("Kapazität > 0")),
    )
    .await
    .expect("Tempdir-Socket bindbar");
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let client = async {
        let mut heads = Vec::with_capacity(requests.len());
        for (method, path, body) in requests {
            heads.push(request_head(&socket, method, path, body).await);
        }
        shutdown_tx.send(true).expect("Server hält den Empfänger");
        heads
    };
    let (served, heads) = tokio::time::timeout(TEST_DEADLINE, async {
        tokio::join!(server.serve_until(shutdown_rx), client)
    })
    .await
    .expect("Server endet nach Shutdown-Signal");
    assert!(served.is_ok(), "serve_until muss sauber enden");
    heads
}

#[tokio::test]
async fn test_get_on_post_route_yields_405_with_allow_post_and_never_runs() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = registry(&runs);
    let heads = run_against_server(&registry, &[("GET", "/api/write", "")]).await;
    let head = &heads[0];
    assert!(head.starts_with("HTTP/1.1 405"), "erwartet 405, erhalten: {head:?}");
    assert_eq!(header_value(head, "allow"), Some("POST"));
    assert_eq!(runs.load(Ordering::SeqCst), 0, "GET darf eine POST-Operation nie ausführen");
}

#[tokio::test]
async fn test_post_on_get_route_yields_405_with_allow_get_and_never_runs() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = registry(&runs);
    let heads = run_against_server(&registry, &[("POST", "/api/read", "{}")]).await;
    let head = &heads[0];
    assert!(head.starts_with("HTTP/1.1 405"), "erwartet 405, erhalten: {head:?}");
    assert_eq!(header_value(head, "allow"), Some("GET"));
    assert_eq!(runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_head_and_options_are_never_mapped_onto_routes() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = registry(&runs);
    let heads = run_against_server(
        &registry,
        &[
            ("HEAD", "/api/write", ""),
            ("OPTIONS", "/api/write", ""),
            ("HEAD", "/api/read", ""),
            ("OPTIONS", "/api/read", ""),
            ("DELETE", "/api/write", ""),
        ],
    )
    .await;
    let expected_allow = ["POST", "POST", "GET", "GET", "POST"];
    for (head, allow) in heads.iter().zip(expected_allow) {
        assert!(head.starts_with("HTTP/1.1 405"), "erwartet 405, erhalten: {head:?}");
        assert_eq!(header_value(head, "allow"), Some(allow), "Kopf: {head:?}");
    }
    assert_eq!(runs.load(Ordering::SeqCst), 0);
}

#[test]
fn test_route_table_from_registry_takes_declared_method() {
    let runs = Arc::new(AtomicUsize::new(0));
    let routes = WebRouteTable::from_registry(&registry(&runs)).expect("Routentabelle baubar");
    assert_eq!(routes.method_for("/api/write"), Some(WebMethod::Post));
    assert_eq!(routes.method_for("/api/read"), Some(WebMethod::Get));
    assert_eq!(harw_web::WebMethod::Post, WebMethod::Post, "harw_web re-exportiert den Vertragstyp");
}

#[test]
fn test_correct_method_is_dispatched_to_declaring_operation() {
    let runs = Arc::new(AtomicUsize::new(0));
    let routes = WebRouteTable::from_registry(&registry(&runs)).expect("Routentabelle baubar");
    let authz = StaticUidTierMap::with_default(vec![], PermissionTier::Observer);
    let peer = PeerCredentials::new(1, 1000, 1000);

    match decide_route(&routes, &authz, &peer, "/api/write", Some(WebMethod::Post)) {
        RouteDecision::Execute { route, .. } => assert_eq!(route.operation_name(), "write-op"),
        other => panic!("POST auf POST-Route muss Execute liefern, erhalten: {other:?}"),
    }
    match decide_route(&routes, &authz, &peer, "/api/read", Some(WebMethod::Get)) {
        RouteDecision::Execute { route, .. } => assert_eq!(route.operation_name(), "read-op"),
        other => panic!("GET auf GET-Route muss Execute liefern, erhalten: {other:?}"),
    }
    assert!(matches!(
        decide_route(&routes, &authz, &peer, "/api/write", Some(WebMethod::Get)),
        RouteDecision::MethodNotAllowed { expected: WebMethod::Post }
    ));
}
