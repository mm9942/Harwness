//! Identity mapping and upgrade ordering (S08, W00 D4, ID-01..ID-05).
//! Driven over in-memory duplex streams; no TCP, no node transport.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use harw_node_listener::{
    DeviceRecord, DeviceRegistry, IdentityMapper, ListenerError, RegistryIdentityMapper,
    UpgradeHandler,
};
use harw_node_transport::{AuthenticatedPeer, UpgradeError};
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
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::Role;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct IdleDriver;

impl TurnDriver for IdleDriver {
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
    assert_eq!(identity.caps, caps_for_tier(PermissionTier::Operator));
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
        if let Message::Text(text) = message {
            if let WireMessage::Response(response) = serde_json::from_str(text.as_str())? {
                break response;
            }
        }
    };
    let ack: HelloAck = serde_json::from_value(response.result.ok_or("no hello result")?)?;
    assert!(ack.granted.observe);
    assert!(!ack.granted.steer && !ack.granted.control && !ack.granted.approve);
    assert_eq!(handler.live().len(), 1);
    Ok(())
}
