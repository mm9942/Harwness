//! Integrationstests der Identitätsauflösung im Server (H12).
//!
//! Prüft über einen **echten** Unix-Socket und den produktiven
//! [`BoundWebServer`] mit injiziertem [`LocalPeerIdentityResolver`]:
//! - der vom Resolver aufgelöste Mandant landet über
//!   `ResolvedPeer::scope_op_context` im `OpContext` einer ausgeführten
//!   Operation (die Testoperation meldet `ctx.tenant()` zurück);
//! - ohne injizierten Resolver (Vorgabe `TierMapResolver`) bleibt der
//!   `OpContext` ohne Mandanten — das Verhalten vor H12;
//! - scheitert die Auflösung, trägt die `403` den Grund des Resolvers
//!   (`IdentityError::code`) — auf der Operationsroute, auf `GET /events`
//!   und auf `/v1/version`/`/v1/capabilities`; `/v1/health` bleibt ohne
//!   Auflösung erreichbar;
//! - ein doppelter Kontext-Header wird vor dem Resolver abgelehnt;
//! - unbekannte Pfade lösen keine Identität auf (`404` trotz kaputtem Header).
//!
//! Die Ausführung braucht einen echten `OpContext`, also eine echte
//! `SandboxSpec` über einer realen `WorkspaceBinding` (Testabhängigkeit
//! `harw-authority`, siehe `harw-web/Cargo.toml`).

mod common;

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::{TestError, TestResult, ctx};
use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
use harw_operations::context::{OpContext, ServiceMap};
use harw_operations::operation::{
    ApprovalPolicy, BusyAvailability, OpFuture, OpInput, OpOutput, Operation, OperationCategory,
    OperationDomain, OperationMeta, PermissionTier, Surface, WebMethod,
};
use harw_operations::registry::OperationRegistry;
use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
use harw_web::authz::{PeerAuthorizer, StaticUidTierMap};
use harw_web::events::WebEventBus;
use harw_web::identity::{
    IdentityError, IdentityFuture, LocalPeerIdentityResolver, ResolvedPeer,
    SECURITY_CONTEXT_HEADER, TierMapResolver, UidTenantMap,
};
use harw_web::peer::PeerCredentials;
use harw_web::router::WebRouteTable;
use harw_web::server::{BoundWebServer, WebContextFactory, WebServerConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Obergrenze, die ein einzelner Socket-Test insgesamt warten darf.
const TEST_DEADLINE: Duration = Duration::from_secs(10);

/// Pfad der Testoperation.
const PROBE_PATH: &str = "/api/tenant-probe";

/// Kontextverweis, den der Test-Resolver als widerrufen ablehnt.
const REVOKED_CONTEXT: &str = "ctx-revoked";

/// Mandant, den der Test-Resolver der eigenen UID zuordnet.
const TENANT: &str = "tenant-a";

/// Meldet den Mandanten des `OpContext` als Text (`"none"` ohne Mandant).
struct TenantProbe {
    meta: OperationMeta,
}

impl Operation for TenantProbe {
    fn meta(&self) -> &OperationMeta {
        &self.meta
    }

    fn run<'a>(&'a self, ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
        let tenant = ctx
            .tenant()
            .map_or_else(|| "none".to_owned(), |tenant| tenant.as_str().to_owned());
        Box::pin(async move {
            Ok(OpOutput {
                text: tenant,
                data: None,
            })
        })
    }
}

fn registry() -> OperationRegistry {
    let mut registry = OperationRegistry::new();
    registry.register(Arc::new(TenantProbe {
        meta: OperationMeta {
            name: "tenant.probe",
            summary: "Meldet den Mandanten des Operationskontexts.",
            domain: OperationDomain::Misc,
            permission: PermissionTier::Observer,
            surfaces: vec![Surface::Web {
                path: PROBE_PATH,
                method: WebMethod::Get,
                approval: ApprovalPolicy::None,
            }],
            aliases: &[],
            category: OperationCategory::Misc,
            args_schema: None,
            output_schema: None,
            busy: BusyAvailability::DeferredUntilTurnEnd,
        },
    }));
    registry
}

/// Die effektive UID dieses Testprozesses — sie ist die `SO_PEERCRED`-UID
/// jeder Testverbindung. Aus dem Eigentümer eines frisch angelegten
/// Verzeichnisses gelesen (kein `unsafe`, keine zusätzliche Abhängigkeit).
fn own_uid(dir: &Path) -> TestResult<u32> {
    Ok(std::fs::metadata(dir)
        .map_err(ctx("Metadaten des Tempdirs"))?
        .uid())
}

/// Jede UID ist `Observer` — dieselbe Tier-Tabelle für Vorgabe- und
/// Test-Resolver.
fn authorizer() -> Arc<dyn PeerAuthorizer> {
    Arc::new(StaticUidTierMap::with_default(
        vec![],
        PermissionTier::Observer,
    ))
}

/// Test-Resolver: `TierMapResolver` mit `uid → tenant-a`, lehnt aber den
/// Kontextverweis [`REVOKED_CONTEXT`] mit `ContextGone` ab. Zählt Aufrufe.
struct RevokingResolver {
    inner: TierMapResolver,
    calls: AtomicUsize,
}

impl RevokingResolver {
    fn new(uid: u32) -> TestResult<Arc<Self>> {
        let tenant = TenantId::try_from_str(TENANT).map_err(ctx("gültiger Mandant"))?;
        Ok(Arc::new(Self {
            inner: TierMapResolver::with_tenants(authorizer(), UidTenantMap::new([(uid, tenant)])),
            calls: AtomicUsize::new(0),
        }))
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl LocalPeerIdentityResolver for RevokingResolver {
    fn resolve<'a>(
        &'a self,
        peer: &'a PeerCredentials,
        presented_context: Option<&'a str>,
    ) -> IdentityFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if presented_context == Some(REVOKED_CONTEXT) {
            return Box::pin(async { Err::<ResolvedPeer, _>(IdentityError::ContextGone) });
        }
        self.inner.resolve(peer, presented_context)
    }
}

/// Kontextfabrik mit echter Sandbox über `<root>/ws` — der Mandant der
/// Sandbox-Bindung (`t`) ist bewusst ein anderer als der aufgelöste, damit
/// der Test belegt, dass `OpContext::tenant` aus dem Resolver stammt.
fn context_factory(root: &Path) -> TestResult<Arc<WebContextFactory>> {
    std::fs::create_dir_all(root.join("ws")).map_err(ctx("Workspace anlegbar"))?;
    let tenant = TenantId::try_from_str("t").map_err(ctx("Sandbox-Mandant"))?;
    let workspace = WorkspaceId::try_from_str("w").map_err(ctx("Workspace-Id"))?;
    let registry = WorkspaceRegistry::build(
        root,
        [WorkspaceRegistration {
            tenant: tenant.clone(),
            workspace: workspace.clone(),
            root: PathBuf::from("ws"),
        }],
    )
    .map_err(ctx("WorkspaceRegistry baubar"))?;
    let binding = registry
        .resolve(&tenant, &workspace)
        .map_err(ctx("Workspace auflösbar"))?;
    let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
    Ok(Arc::new(
        move |_peer: &PeerCredentials, _tier: PermissionTier| -> OpContext {
            OpContext::new(
                SessionId::new(),
                TurnId::new(),
                sandbox.clone(),
                ServiceMap::new(),
            )
        },
    ))
}

async fn bound_server(socket: &Path, root: &Path) -> TestResult<BoundWebServer> {
    let routes = WebRouteTable::from_registry(&registry()).map_err(ctx("Routentabelle baubar"))?;
    BoundWebServer::bind(
        WebServerConfig {
            socket_path: socket.to_path_buf(),
        },
        routes,
        authorizer(),
        context_factory(root)?,
        Arc::new(WebEventBus::new(8).map_err(ctx("Kapazität > 0"))?),
    )
    .await
    .map_err(ctx("Tempdir-Socket bindbar"))
}

/// Eine Anfrage: Methode, Pfad, zusätzliche Kopfzeilen.
struct Call {
    method: &'static str,
    path: &'static str,
    headers: Vec<(&'static str, &'static str)>,
}

fn call(method: &'static str, path: &'static str) -> Call {
    Call {
        method,
        path,
        headers: Vec::new(),
    }
}

fn call_with_context(method: &'static str, path: &'static str, context: &'static str) -> Call {
    Call {
        method,
        path,
        headers: vec![(SECURITY_CONTEXT_HEADER, context)],
    }
}

/// Eine Antwort: Statuszeile + Kopfzeilen, Rumpf.
struct Reply {
    head: String,
    body: Vec<u8>,
}

impl Reply {
    fn status_is(&self, code: u16) -> bool {
        self.head.starts_with(&format!("HTTP/1.1 {code}"))
    }

    fn json(&self) -> TestResult<serde_json::Value> {
        serde_json::from_slice(&self.body).map_err(ctx("Rumpf ist JSON"))
    }
}

/// Schickt `call` mit `Connection: close` und liest bis EOF.
async fn request(socket: &Path, call: &Call) -> TestResult<Reply> {
    let mut stream = UnixStream::connect(socket)
        .await
        .map_err(ctx("Testserver lauscht am Tempdir-Socket"))?;
    let extra: String = call
        .headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect();
    let raw = format!(
        "{} {} HTTP/1.1\r\nHost: localhost\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n",
        call.method, call.path
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

/// Bedient `server` und schickt nacheinander `calls`.
async fn run(server: BoundWebServer, socket: &Path, calls: &[Call]) -> TestResult<Vec<Reply>> {
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let client = async {
        let mut replies = Vec::with_capacity(calls.len());
        for call in calls {
            replies.push(request(socket, call).await?);
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

fn assert_forbidden(reply: &Reply, reason: &str) -> TestResult {
    if !reply.status_is(403) {
        return Err(TestError::Unexpected(format!(
            "erwartet 403: {:?}",
            reply.head
        )));
    }
    assert_eq!(reply.json()?["reason"], reason, "403-Grund");
    Ok(())
}

#[tokio::test]
async fn test_resolved_tenant_is_scoped_into_the_op_context() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("Tempdir anlegbar"))?;
    let socket = dir.path().join("control.sock");
    let resolver = RevokingResolver::new(own_uid(dir.path())?)?;
    let server = bound_server(&socket, dir.path())
        .await?
        .with_identity_resolver(Arc::clone(&resolver) as Arc<dyn LocalPeerIdentityResolver>);
    let replies = run(server, &socket, &[call("GET", PROBE_PATH)]).await?;
    let [probe] = replies.as_slice() else {
        return Err(TestError::Missing("eine Antwort"));
    };
    assert!(probe.status_is(200), "erwartet 200: {:?}", probe.head);
    assert_eq!(
        probe.json()?["text"],
        TENANT,
        "der Mandant des OpContext stammt aus dem Resolver"
    );
    assert_eq!(resolver.calls(), 1);
    Ok(())
}

#[tokio::test]
async fn test_default_resolver_leaves_the_op_context_without_tenant() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("Tempdir anlegbar"))?;
    let socket = dir.path().join("control.sock");
    // Kein `with_identity_resolver`: Vorgabe `TierMapResolver` ohne Mandanten.
    let server = bound_server(&socket, dir.path()).await?;
    let replies = run(
        server,
        &socket,
        &[
            call("GET", PROBE_PATH),
            // Im Modus tier_map wird ein Kontext-Header ignoriert.
            call_with_context("GET", PROBE_PATH, REVOKED_CONTEXT),
        ],
    )
    .await?;
    for reply in &replies {
        assert!(reply.status_is(200), "erwartet 200: {:?}", reply.head);
        assert_eq!(reply.json()?["text"], "none");
    }
    Ok(())
}

#[tokio::test]
async fn test_resolver_error_code_is_the_forbidden_reason_everywhere() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("Tempdir anlegbar"))?;
    let socket = dir.path().join("control.sock");
    let resolver = RevokingResolver::new(own_uid(dir.path())?)?;
    let server = bound_server(&socket, dir.path())
        .await?
        .with_identity_resolver(Arc::clone(&resolver) as Arc<dyn LocalPeerIdentityResolver>);
    let replies = run(
        server,
        &socket,
        &[
            call_with_context("GET", PROBE_PATH, REVOKED_CONTEXT),
            call_with_context("GET", "/events", REVOKED_CONTEXT),
            call_with_context("GET", "/v1/version", REVOKED_CONTEXT),
            call_with_context("GET", "/v1/capabilities", REVOKED_CONTEXT),
            call_with_context("GET", "/v1/health", REVOKED_CONTEXT),
        ],
    )
    .await?;
    let [probe, events, version, capabilities, health] = replies.as_slice() else {
        return Err(TestError::Missing("fünf Antworten"));
    };
    let gone = IdentityError::ContextGone.code();
    for reply in [probe, events, version, capabilities] {
        assert_forbidden(reply, gone)?;
    }
    assert!(
        health.status_is(200),
        "health ohne Auflösung: {:?}",
        health.head
    );
    assert_eq!(resolver.calls(), 4, "health löst keine Identität auf");
    Ok(())
}

#[tokio::test]
async fn test_duplicate_context_header_is_rejected_before_the_resolver() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("Tempdir anlegbar"))?;
    let socket = dir.path().join("control.sock");
    let resolver = RevokingResolver::new(own_uid(dir.path())?)?;
    let server = bound_server(&socket, dir.path())
        .await?
        .with_identity_resolver(Arc::clone(&resolver) as Arc<dyn LocalPeerIdentityResolver>);
    let duplicate = Call {
        method: "GET",
        path: PROBE_PATH,
        headers: vec![
            (SECURITY_CONTEXT_HEADER, "ctx-1"),
            (SECURITY_CONTEXT_HEADER, "ctx-2"),
        ],
    };
    let replies = run(server, &socket, &[duplicate]).await?;
    let [reply] = replies.as_slice() else {
        return Err(TestError::Missing("eine Antwort"));
    };
    assert_forbidden(reply, IdentityError::InvalidContextReference.code())?;
    assert_eq!(
        resolver.calls(),
        0,
        "kaputter Header erreicht den Resolver nie"
    );
    Ok(())
}

#[tokio::test]
async fn test_unknown_path_and_wrong_method_resolve_no_identity() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("Tempdir anlegbar"))?;
    let socket = dir.path().join("control.sock");
    let resolver = RevokingResolver::new(own_uid(dir.path())?)?;
    let server = bound_server(&socket, dir.path())
        .await?
        .with_identity_resolver(Arc::clone(&resolver) as Arc<dyn LocalPeerIdentityResolver>);
    let replies = run(
        server,
        &socket,
        &[
            call_with_context("GET", "/api/gibt-es-nicht", REVOKED_CONTEXT),
            call_with_context("POST", PROBE_PATH, REVOKED_CONTEXT),
        ],
    )
    .await?;
    let [missing, wrong_method] = replies.as_slice() else {
        return Err(TestError::Missing("zwei Antworten"));
    };
    assert!(missing.status_is(404), "erwartet 404: {:?}", missing.head);
    assert!(
        wrong_method.status_is(405),
        "erwartet 405: {:?}",
        wrong_method.head
    );
    assert_eq!(resolver.calls(), 0);
    Ok(())
}
