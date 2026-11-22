//! Identity mapping and upgrade ordering (S08, W00 D4, ID-01..ID-05).
//! Driven over in-memory duplex streams; no TCP, no node transport.

use std::sync::Arc;
use std::time::Duration;

use aws_lc_rs::signature::{KeyPair, ML_DSA_65_SIGNING, PqdsaKeyPair};
use futures_util::{SinkExt, StreamExt};
use harw_node_listener::{
    DeviceRecord, DeviceRegistry, IdentityMapper, ListenerError, NodeListener,
    RegistryIdentityMapper, RevocationSink, UpgradeHandler,
};
use harw_node_transport::{
    AuthenticatedPeer, ClientOptions, LocalNode, ML_DSA_65_SIGNATURE_LEN, NodeIdentity, NodeSigner,
    NodeTransportClient, NodeTransportServer, PinnedPeers, ServerOptions, SignFuture, SignerError,
    UpgradeError, UpgradedIo, empty_body,
};
use harw_protocol::methods::METHOD_SESSION_HELLO;
use harw_protocol::session_wire::HelloAck;
use harw_protocol::{ProtocolVersion, RequestEnvelope, ResponseEnvelope, WireMessage};
use harw_session_host::approvals::MemoryApprovals;
use harw_session_host::driver::{
    CancelSignal, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::replay::MemoryTranscripts;
use harw_session_host::{ConnectionId, HostConfig, SessionHost, caps_for_tier};
use harw_session_ws::{WsLimits, upgrade};
use harw_types::{AuthStrength, DeviceId, NodeId, PermissionTier, SessionId, TenantId, TrustZone};
use http_body_util::Empty;
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::Role;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct IdleDriver;

impl TurnDriver for IdleDriver {
    // This test double performs no privileged runtime/tool effects.
    fn prepare_execution(
        &self,
        _: &harw_types::SessionId,
        _: harw_session_host::driver::ExecutionAdmission,
    ) -> harw_session_host::driver::DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn create_session(&self, _: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn run_turn(
        &self,
        _: TurnInput,
        _: CancelSignal,
        _: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        Box::pin(async { Ok(TurnOutcome::Completed) })
    }
    fn resume_after_approval(
        &self,
        _: &SessionId,
        _: CancelSignal,
        _: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        Box::pin(async { Ok(TurnOutcome::Completed) })
    }
    fn apply_setting(&self, _: &SessionId, _: Setting) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn model_name(&self, _: &SessionId) -> Option<String> {
        None
    }
}

fn open_host(dir: &std::path::Path) -> TestResult<Arc<SessionHost>> {
    Ok(Arc::new(SessionHost::open(
        HostConfig::new(dir.join("host")),
        Arc::new(IdleDriver),
        Arc::new(MemoryTranscripts::new()),
        Arc::new(MemoryApprovals::new()),
    )?))
}

fn peer(node: &str) -> AuthenticatedPeer {
    AuthenticatedPeer {
        node_id: NodeId::from_str(node),
        protocol_version: 1,
    }
}

fn record(node: &str, device: &str, tier: PermissionTier) -> DeviceRecord {
    DeviceRecord {
        node_id: NodeId::from_str(node),
        device: DeviceId::from_str(device),
        tenant: TenantId::from_str("acme"),
        tier,
        revoked: false,
        label: format!("label-{device}"),
        approve_optin: false,
    }
}

#[tokio::test]
async fn mapped_identity_is_host_derived_from_the_registry() -> TestResult {
    let dir = tempfile::tempdir()?;
    DeviceRegistry::new(dir.path()).enroll(&record("node-a", "dev-1", PermissionTier::Operator))?;
    let mapper = RegistryIdentityMapper::new(dir.path());
    let connection = ConnectionId::next();

    let identity = mapper.map(&peer("node-a"), connection).await?;

    assert_eq!(identity.tenant, Some(TenantId::from_str("acme")));
    assert_eq!(identity.device, Some(DeviceId::from_str("dev-1")));
    assert_eq!(identity.principal.id(), "device:dev-1");
    assert_eq!(identity.principal.tier(), PermissionTier::Operator);
    // Tier ceiling minus `approve`: remote approval needs the per-device opt-in.
    let mut expected = caps_for_tier(PermissionTier::Operator);
    expected.approve = false;
    assert_eq!(identity.caps, expected);
    assert_eq!(identity.zone, TrustZone::Remote);
    assert_eq!(identity.strength, AuthStrength::MutualTls);
    assert_eq!(identity.connection, connection);
    assert!(identity.agent.is_none());
    assert!(identity.is_remote());
    identity.validate()?;
    Ok(())
}

#[tokio::test]
async fn unknown_revoked_or_ambiguous_key_fails_closed() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mapper = RegistryIdentityMapper::new(dir.path());
    // No registry file at all.
    assert!(matches!(
        mapper.map(&peer("node-a"), ConnectionId::next()).await,
        Err(ListenerError::UnknownPeer)
    ));

    let registry = DeviceRegistry::new(dir.path());
    registry.enroll(&record("node-a", "dev-1", PermissionTier::Owner))?;
    registry.enroll(&record("node-dup", "dev-2", PermissionTier::Owner))?;
    registry.enroll(&record("node-dup", "dev-3", PermissionTier::Owner))?;
    let mut revoked = record("node-r", "dev-4", PermissionTier::Owner);
    revoked.revoked = true;
    registry.enroll(&revoked)?;

    // A known node maps; everything else is refused identically.
    assert!(
        mapper
            .map(&peer("node-a"), ConnectionId::next())
            .await
            .is_ok()
    );
    for node in ["node-unknown", "node-dup", "node-r"] {
        assert!(
            matches!(
                mapper.map(&peer(node), ConnectionId::next()).await,
                Err(ListenerError::UnknownPeer)
            ),
            "{node} must be refused"
        );
    }

    // Revoking in the registry refuses the next mapping.
    assert_eq!(registry.mark_revoked(&DeviceId::from_str("dev-1"))?, 1);
    assert!(matches!(
        mapper.map(&peer("node-a"), ConnectionId::next()).await,
        Err(ListenerError::UnknownPeer)
    ));
    Ok(())
}

#[tokio::test]
async fn a_valid_key_does_not_elevate_the_permission_tier() -> TestResult {
    let dir = tempfile::tempdir()?;
    let registry = DeviceRegistry::new(dir.path());
    registry.enroll(&record("node-obs", "dev-obs", PermissionTier::Observer))?;
    registry.enroll(&record("node-own", "dev-own", PermissionTier::Owner))?;
    let mapper = RegistryIdentityMapper::new(dir.path());

    let observer = mapper.map(&peer("node-obs"), ConnectionId::next()).await?;
    assert_eq!(observer.principal.tier(), PermissionTier::Observer);
    assert!(observer.caps.observe);
    assert!(!observer.caps.steer && !observer.caps.approve && !observer.caps.control);
    assert!(!observer.caps.gateway_admin);

    // Even the owner tier never gets tool_call: that is agent-principal only.
    let owner = mapper.map(&peer("node-own"), ConnectionId::next()).await?;
    assert_eq!(owner.principal.tier(), PermissionTier::Owner);
    assert!(!owner.caps.tool_call);
    assert!(owner.agent.is_none());

    // The peer's own protocol version carries no authority.
    let odd = AuthenticatedPeer {
        node_id: NodeId::from_str("node-obs"),
        protocol_version: u16::MAX,
    };
    let again = mapper.map(&odd, ConnectionId::next()).await?;
    assert_eq!(again.caps, observer.caps);
    Ok(())
}

// --- upgrade ordering over duplex --------------------------------------

fn spawn_server(
    io: tokio::io::DuplexStream,
    handler: Arc<UpgradeHandler>,
    peer: Result<AuthenticatedPeer, ()>,
) {
    tokio::spawn(async move {
        let service = service_fn(move |request: hyper::Request<Incoming>| {
            let handler = Arc::clone(&handler);
            let peer = peer
                .clone()
                .map_err(|()| UpgradeError::NotImplemented("test: no peer"));
            async move { Ok::<_, std::convert::Infallible>(handler.handle(request, peer).await) }
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(io), service)
            .with_upgrades()
            .await;
    });
}

async fn upgrade_client(io: tokio::io::DuplexStream) -> TestResult<hyper::Response<Incoming>> {
    let (request, _key) = upgrade::client_request("localhost")?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io)).await?;
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    let request: hyper::Request<Empty<Bytes>> = request;
    Ok(sender.send_request(request).await?)
}

#[tokio::test]
async fn unknown_peer_is_refused_before_any_upgrade() -> TestResult {
    let dir = tempfile::tempdir()?;
    let host = open_host(dir.path())?;
    let mapper: Arc<dyn IdentityMapper> = Arc::new(RegistryIdentityMapper::new(dir.path()));
    let handler = Arc::new(UpgradeHandler::new(host, mapper, WsLimits::default()));

    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    spawn_server(server_io, Arc::clone(&handler), Ok(peer("node-unknown")));
    let response = upgrade_client(client_io).await?;
    assert_eq!(response.status(), hyper::StatusCode::FORBIDDEN);
    assert!(handler.live().is_empty());

    // No authenticated peer at all: fail closed, unauthenticated.
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    spawn_server(server_io, Arc::clone(&handler), Err(()));
    let response = upgrade_client(client_io).await?;
    assert_eq!(response.status(), hyper::StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn mapped_peer_upgrades_and_hello_grants_only_the_registry_ceiling() -> TestResult {
    let dir = tempfile::tempdir()?;
    DeviceRegistry::new(dir.path()).enroll(&record("node-a", "dev-1", PermissionTier::Observer))?;
    let host = open_host(dir.path())?;
    let mapper: Arc<dyn IdentityMapper> = Arc::new(RegistryIdentityMapper::new(dir.path()));
    let handler = Arc::new(UpgradeHandler::new(host, mapper, WsLimits::default()));

    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    spawn_server(server_io, Arc::clone(&handler), Ok(peer("node-a")));
    let response = upgrade_client(client_io).await?;
    assert_eq!(response.status(), hyper::StatusCode::SWITCHING_PROTOCOLS);
    let upgraded = hyper::upgrade::on(response).await?;
    let mut ws = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, None).await;

    // The client claims everything; the host-fixed ceiling wins.
    let envelope = RequestEnvelope {
        jsonrpc: "2.0".into(),
        id: "1".into(),
        method: METHOD_SESSION_HELLO.into(),
        params: serde_json::json!({"client_label": "claims-owner", "wire_minor": 1}),
        protocol: ProtocolVersion::default(),
    };
    ws.send(Message::text(serde_json::to_string(&envelope)?))
        .await?;
    let response: ResponseEnvelope = loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await?
            .ok_or("closed")??;
        if let Message::Text(text) = message
            && let WireMessage::Response(response) = serde_json::from_str(text.as_str())?
        {
            break response;
        }
    };
    let ack: HelloAck = serde_json::from_value(response.result.ok_or("no hello result")?)?;
    assert!(ack.granted.observe);
    assert!(!ack.granted.steer && !ack.granted.control && !ack.granted.approve);
    assert_eq!(handler.live().len(), 1);
    Ok(())
}

#[tokio::test]
async fn remote_approve_needs_the_per_device_opt_in_not_the_tier() -> TestResult {
    let dir = tempfile::tempdir()?;
    let registry = DeviceRegistry::new(dir.path());
    registry.enroll(&record("node-op", "dev-op", PermissionTier::Operator))?;
    let mut opted = record("node-yes", "dev-yes", PermissionTier::Operator);
    opted.approve_optin = true;
    registry.enroll(&opted)?;
    let mut owner = record("node-own", "dev-own", PermissionTier::Owner);
    owner.approve_optin = false;
    registry.enroll(&owner)?;
    let mapper = RegistryIdentityMapper::new(dir.path());

    let plain = mapper.map(&peer("node-op"), ConnectionId::next()).await?;
    assert!(plain.caps.steer && !plain.caps.approve);

    let allowed = mapper.map(&peer("node-yes"), ConnectionId::next()).await?;
    assert!(allowed.caps.steer && allowed.caps.approve);

    // Even the owner tier does not approve without the opt-in.
    let owner = mapper.map(&peer("node-own"), ConnectionId::next()).await?;
    assert!(!owner.caps.approve);
    Ok(())
}

#[test]
fn opt_in_marker_round_trips_and_other_seventh_fields_fail_closed() -> TestResult {
    let line = "n1|d1|acme|operator|active|phone|approve";
    let rec = DeviceRecord::parse(line).ok_or("opt-in line must parse")?;
    assert!(rec.approve_optin);
    assert_eq!(rec.render(), line);
    let plain = DeviceRecord::parse("n1|d1|acme|operator|active|phone").ok_or("plain line")?;
    assert!(!plain.approve_optin);
    assert_eq!(plain.render(), "n1|d1|acme|operator|active|phone");
    for bad in [
        "n1|d1|acme|operator|active|phone|yes",
        "n1|d1|acme|operator|active|phone|approve|x",
        "n1|d1|acme|operator|active|phone|",
    ] {
        assert!(DeviceRecord::parse(bad).is_none(), "{bad}");
    }
    Ok(())
}

// --- stream intake (onion) next to a running NodeListener::serve ---------

/// Test-only deterministic ML-DSA-65 node signer.
struct TestSigner {
    key: PqdsaKeyPair,
}

impl NodeSigner for TestSigner {
    fn sign<'a>(&'a self, transcript: &'a [u8]) -> SignFuture<'a> {
        let mut signature = vec![0u8; ML_DSA_65_SIGNATURE_LEN];
        let result = self
            .key
            .sign(transcript, &mut signature)
            .map(|written| {
                signature.truncate(written);
                signature
            })
            .map_err(|_| SignerError::new("test signer failed"));
        Box::pin(std::future::ready(result))
    }
}

struct TestNode {
    id: NodeId,
    identity: NodeIdentity,
    local: LocalNode,
}

fn test_node(name: &str, seed: u8) -> TestResult<TestNode> {
    let id = NodeId::try_from_str(name)?;
    let key = PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, &[seed; 32])
        .map_err(|_| "ML-DSA-65 key from seed")?;
    let identity = NodeIdentity {
        node_id: id.clone(),
        public_key: key.public_key().as_ref().to_vec(),
        key_ref: format!("test-only/{name}"),
    };
    let local = LocalNode::new(identity.clone(), Arc::new(TestSigner { key }));
    Ok(TestNode {
        id,
        identity,
        local,
    })
}

type IntakeWs = WebSocketStream<UpgradedIo>;

/// A listener whose transport pins an enrolled device (`node-a`, `dev-1`)
/// and a transport-valid device that is not in the registry (`node-x`).
struct Rig {
    _dir: tempfile::TempDir,
    listener: Arc<NodeListener>,
    gateway: TestNode,
    enrolled: TestNode,
    unknown: TestNode,
    client_verifier: PinnedPeers,
}

impl Rig {
    fn new() -> TestResult<Self> {
        let dir = tempfile::tempdir()?;
        DeviceRegistry::new(dir.path()).enroll(&record(
            "node-a",
            "dev-1",
            PermissionTier::Operator,
        ))?;
        let gateway = test_node("gateway", 1)?;
        let enrolled = test_node("node-a", 2)?;
        let unknown = test_node("node-x", 3)?;
        let mut server_peers = PinnedPeers::new();
        server_peers.pin_identity(&enrolled.identity)?;
        server_peers.pin_identity(&unknown.identity)?;
        let mut client_verifier = PinnedPeers::new();
        client_verifier.pin_identity(&gateway.identity)?;
        let server = NodeTransportServer::new(
            gateway.local.clone(),
            Arc::new(server_peers),
            ServerOptions::default(),
        )?;
        let mapper: Arc<dyn IdentityMapper> = Arc::new(RegistryIdentityMapper::new(dir.path()));
        let listener = Arc::new(NodeListener::new(
            server,
            open_host(dir.path())?,
            mapper,
            WsLimits::default(),
        ));
        Ok(Self {
            _dir: dir,
            listener,
            gateway,
            enrolled,
            unknown,
            client_verifier,
        })
    }

    /// Run `NodeListener::serve` (which registers the `node` listener) until
    /// the returned sender fires or is dropped.
    async fn start_serve(
        &self,
    ) -> TestResult<(
        tokio::sync::oneshot::Sender<()>,
        JoinHandle<Result<(), ListenerError>>,
    )> {
        let tcp = TcpListener::bind("127.0.0.1:0").await?;
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let listener = Arc::clone(&self.listener);
        let task = tokio::spawn(async move {
            listener
                .serve(tcp, async {
                    let _ = stopped.await;
                })
                .await
        });
        // `serve` registers the listener before its first await.
        tokio::task::yield_now().await;
        Ok((stop, task))
    }

    /// Dial the intake over a duplex stream as `client`: `Ok(Ok(ws))` when
    /// upgraded, `Ok(Err(status))` when refused with that HTTP status.
    async fn dial(&self, client: &TestNode) -> TestResult<Result<IntakeWs, u16>> {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let intake = self.listener.intake();
        tokio::spawn(async move {
            let _ = intake.serve(server_io, Duration::from_secs(5)).await;
        });
        let transport = NodeTransportClient::connect_over(
            client_io,
            &client.local,
            &self.gateway.id,
            &self.client_verifier,
            &ClientOptions::default(),
        )
        .await?;
        let (request, _key) = upgrade::client_request("localhost")?;
        match transport.upgrade(request.map(|_| empty_body())).await {
            Ok(upgraded) => Ok(Ok(WebSocketStream::from_raw_socket(
                upgraded.io,
                Role::Client,
                None,
            )
            .await)),
            Err(UpgradeError::NotSwitched(status)) => Ok(Err(status)),
            Err(other) => Err(other.into()),
        }
    }
}

async fn session_hello(ws: &mut IntakeWs) -> TestResult {
    let envelope = RequestEnvelope {
        jsonrpc: "2.0".into(),
        id: "h".into(),
        method: METHOD_SESSION_HELLO.into(),
        params: serde_json::json!({"client_label": "t", "wire_minor": 1}),
        protocol: ProtocolVersion::default(),
    };
    ws.send(Message::text(serde_json::to_string(&envelope)?))
        .await?;
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await?
            .ok_or("closed")??;
        if let Message::Text(text) = message
            && let WireMessage::Response(response) = serde_json::from_str(text.as_str())?
            && response.id == "h"
        {
            assert!(response.error.is_none(), "{response:?}");
            return Ok(());
        }
    }
}

/// Read until the socket ends (close frame, error or EOF).
async fn ends(ws: &mut IntakeWs) -> TestResult {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await? {
            None | Some(Err(_) | Ok(Message::Close(_))) => return Ok(()),
            Some(Ok(_)) => {}
        }
    }
}

#[tokio::test]
async fn intake_upgrades_an_enrolled_device_into_the_live_table() -> TestResult {
    let rig = Rig::new()?;
    let (_stop, _task) = rig.start_serve().await?;

    let mut ws = rig
        .dial(&rig.enrolled)
        .await?
        .map_err(|status| format!("upgrade refused with {status}"))?;
    session_hello(&mut ws).await?;

    let live = rig.listener.handler().live();
    assert_eq!(live.len(), 1);
    assert_eq!(live.count_for(&DeviceId::from_str("dev-1")), 1);
    Ok(())
}

#[tokio::test]
async fn revoking_the_device_closes_its_intake_websocket() -> TestResult {
    let rig = Rig::new()?;
    let (_stop, _task) = rig.start_serve().await?;
    let mut ws = rig
        .dial(&rig.enrolled)
        .await?
        .map_err(|status| format!("upgrade refused with {status}"))?;
    session_hello(&mut ws).await?;

    let report = rig
        .listener
        .revoker()
        .revoke_device(&DeviceId::from_str("dev-1"))?;
    assert_eq!(report.connections_closed, 1);

    ends(&mut ws).await?;
    Ok(())
}

#[tokio::test]
async fn serve_shutdown_closes_the_intake_websocket() -> TestResult {
    let rig = Rig::new()?;
    let (stop, task) = rig.start_serve().await?;
    let mut ws = rig
        .dial(&rig.enrolled)
        .await?
        .map_err(|status| format!("upgrade refused with {status}"))?;
    session_hello(&mut ws).await?;

    stop.send(()).map_err(|()| "serve already ended")?;
    ends(&mut ws).await?;
    tokio::time::timeout(Duration::from_secs(5), task).await???;
    Ok(())
}

#[tokio::test]
async fn intake_without_a_running_serve_answers_503() -> TestResult {
    let rig = Rig::new()?;
    let refused = rig.dial(&rig.enrolled).await?;
    assert_eq!(refused.err(), Some(503));
    assert!(rig.listener.handler().live().is_empty());
    Ok(())
}

#[tokio::test]
async fn intake_refuses_a_device_missing_from_the_registry_with_403() -> TestResult {
    let rig = Rig::new()?;
    let (_stop, _task) = rig.start_serve().await?;
    let refused = rig.dial(&rig.unknown).await?;
    assert_eq!(refused.err(), Some(403));
    assert!(rig.listener.handler().live().is_empty());
    Ok(())
}
