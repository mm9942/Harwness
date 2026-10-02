//! Self-cloud ingress end to end: the real node transport (PQ TLS plus an
//! ML-DSA handshake) carries a WebSocket session into the same `ComServer`
//! stack the local socket uses. PL-68 W02 + W06, built on the integration branch's upgrade capability
//! (`serve_upgradable`, `NodeTransportClient::upgrade`) and device registry.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use harw_node_listener::identity::{
    DeviceRecord, DeviceRegistry, IdentityMapper, RegistryIdentityMapper,
};
use harw_node_transport::{
    AuthenticatedPeer, NodeIdentity, NodeTransportClient, NodeTransportServer, ServerOptions,
    UpgradeError, empty_body,
};
use harw_protocol::methods::{METHOD_GATEWAY_STATUS, METHOD_SESSION_CREATE, METHOD_SESSION_HELLO};
use harw_protocol::{ProtocolVersion, RequestEnvelope, ResponseEnvelope, WireMessage};
use harw_session_com::{ComConfig, ComRefusal, ComServer, HostBinder, PortOffer, RemoteLayer};
use harw_session_host::approvals::MemoryApprovals;
use harw_session_host::driver::{
    CancelSignal, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::replay::MemoryTranscripts;
use harw_session_host::{HostConfig, SessionHost};
use harw_session_ws::upgrade;
use harw_types::{DeviceId, PermissionTier, SessionId, TenantId};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::Role;

mod common;
use common::{Node, TestResult, node, pinned};

struct NoopDriver;

impl TurnDriver for NoopDriver {
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

struct Cloud {
    addr: SocketAddr,
    gateway: Node,
    host: SessionHost,
    registry: DeviceRegistry,
    _shutdown: watch::Sender<bool>,
    _dir: tempfile::TempDir,
}

/// A gateway node serving the Com layer behind the node transport. The
/// transport trusts `trusted` nodes; the device registry (a file, re-read per
/// connection) decides which of them is a device.
async fn cloud(trusted: &[&Node]) -> TestResult<Cloud> {
    let gateway = node("gateway", 1)?;
    let dir = tempfile::tempdir()?;
    let host = SessionHost::open(
        HostConfig::new(dir.path().join("state")),
        Arc::new(NoopDriver),
        Arc::new(MemoryTranscripts::new()),
        Arc::new(MemoryApprovals::new()),
    )?;
    let registry = DeviceRegistry::new(dir.path());
    let mapper = Arc::new(RegistryIdentityMapper::new(dir.path()));

    let (shutdown, rx) = watch::channel(false);
    let binder = Arc::new(HostBinder::new(host.clone(), PortOffer::All));
    let com = ComServer::new(&ComConfig::default(), binder, rx);
    let service = com.service_for(RemoteLayer::<AuthenticatedPeer>::new(
        move |peer, connection| {
            let mapper = Arc::clone(&mapper);
            async move {
                mapper
                    .map(&peer, connection)
                    .await
                    .map_err(|_| ComRefusal::Denied)
            }
        },
    ));

    let identities: Vec<&NodeIdentity> = trusted.iter().map(|n| &n.identity).collect();
    let server = NodeTransportServer::new(
        gateway.local.clone(),
        Arc::new(pinned(&identities)?),
        ServerOptions::default(),
    )?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        server
            .serve_upgradable(listener, service, std::future::pending())
            .await
    });
    Ok(Cloud {
        addr,
        gateway,
        host,
        registry,
        _shutdown: shutdown,
        _dir: dir,
    })
}

fn enroll(cloud: &Cloud, node: &Node, device: &str, tier: PermissionTier) -> TestResult {
    cloud.registry.enroll(&DeviceRecord {
        node_id: node.id.clone(),
        device: DeviceId::try_from_str(device)?,
        tenant: TenantId::try_from_str("tenant-a")?,
        tier,
        revoked: false,
        label: "phone".to_owned(),
    })?;
    Ok(())
}

async fn connect(cloud: &Cloud, from: &Node) -> TestResult<NodeTransportClient> {
    let trust = pinned(&[&cloud.gateway.identity])?;
    Ok(NodeTransportClient::connect(cloud.addr, &from.local, &cloud.gateway.id, &trust).await?)
}

/// Upgrades over the node transport; the WebSocket handshake is checked here.
async fn ws(
    cloud: &Cloud,
    from: &Node,
) -> Result<WebSocketStream<harw_node_transport::UpgradedIo>, UpgradeError> {
    let client = connect(cloud, from)
        .await
        .map_err(|_| UpgradeError::MissingPeer)?;
    let (request, key) =
        upgrade::client_request("gateway").map_err(|_| UpgradeError::MissingPeer)?;
    let up = client.upgrade(request.map(|_| empty_body())).await?;
    upgrade::verify_client_response(&up.response, &key).map_err(|_| UpgradeError::MissingPeer)?;
    Ok(WebSocketStream::from_raw_socket(up.io, Role::Client, None).await)
}

async fn call<S>(
    ws: &mut WebSocketStream<S>,
    id: &str,
    method: &str,
    params: serde_json::Value,
) -> TestResult<ResponseEnvelope>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let envelope = RequestEnvelope {
        jsonrpc: "2.0".into(),
        id: id.into(),
        method: method.into(),
        params,
        protocol: ProtocolVersion::default(),
    };
    ws.send(Message::text(serde_json::to_string(&envelope)?))
        .await?;
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await?
            .ok_or("closed")??;
        if let Message::Text(text) = message {
            if let WireMessage::Response(response) = serde_json::from_str(text.as_str())? {
                if response.id == id {
                    return Ok(response);
                }
            }
        }
    }
}

async fn hello<S>(ws: &mut WebSocketStream<S>) -> TestResult
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let r = call(
        ws,
        "h",
        METHOD_SESSION_HELLO,
        serde_json::json!({"client_label": "phone", "wire_minor": 2}),
    )
    .await?;
    assert!(r.error.is_none(), "{r:?}");
    Ok(())
}

#[tokio::test]
async fn an_enrolled_device_runs_a_session_over_the_node_transport() -> TestResult {
    let phone = node("phone", 2)?;
    let cloud = cloud(&[&phone]).await?;
    enroll(&cloud, &phone, "dev-1", PermissionTier::Operator)?;
    let mut ws = ws(&cloud, &phone).await?;
    hello(&mut ws).await?;
    let created = call(
        &mut ws,
        "1",
        METHOD_SESSION_CREATE,
        serde_json::json!({"title": "t"}),
    )
    .await?;
    assert!(created.error.is_none(), "{created:?}");
    Ok(())
}

#[tokio::test]
async fn gateway_rights_of_a_remote_device_follow_its_tier() -> TestResult {
    let phone = node("phone", 2)?;
    let cloud = cloud(&[&phone]).await?;
    enroll(&cloud, &phone, "dev-1", PermissionTier::Owner)?;
    let mut ws = ws(&cloud, &phone).await?;
    hello(&mut ws).await?;
    let status = call(&mut ws, "1", METHOD_GATEWAY_STATUS, serde_json::json!({})).await?;
    assert!(
        status.error.is_none(),
        "an owner device holds gateway_read: {status:?}"
    );

    let tablet = node("tablet", 4)?;
    let cloud2 = self::cloud(&[&tablet]).await?;
    enroll(&cloud2, &tablet, "dev-2", PermissionTier::Operator)?;
    let mut ws2 = self::ws(&cloud2, &tablet).await?;
    hello(&mut ws2).await?;
    let denied = call(&mut ws2, "1", METHOD_GATEWAY_STATUS, serde_json::json!({})).await?;
    assert!(
        denied.error.is_some(),
        "an operator device has no gateway caps: {denied:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_transport_trusted_node_that_is_not_a_device_is_refused_with_403() -> TestResult {
    let phone = node("phone", 2)?;
    let stranger = node("stranger", 3)?;
    // The transport trusts both nodes; only the phone is enrolled.
    let cloud = cloud(&[&phone, &stranger]).await?;
    enroll(&cloud, &phone, "dev-1", PermissionTier::Operator)?;
    let refused = ws(&cloud, &stranger).await;
    assert!(
        matches!(refused, Err(UpgradeError::NotSwitched(403))),
        "{:?}",
        refused.err()
    );
    Ok(())
}

#[tokio::test]
async fn a_revoked_device_cannot_reconnect_and_its_live_session_is_closed() -> TestResult {
    let phone = node("phone", 2)?;
    let cloud = cloud(&[&phone]).await?;
    enroll(&cloud, &phone, "dev-1", PermissionTier::Operator)?;
    let mut live = ws(&cloud, &phone).await?;
    hello(&mut live).await?;

    let device = DeviceId::try_from_str("dev-1")?;
    assert_eq!(cloud.registry.mark_revoked(&device)?, 1);
    cloud.host.revoke_device(&device);

    let reconnect = ws(&cloud, &phone).await;
    assert!(
        matches!(reconnect, Err(UpgradeError::NotSwitched(403))),
        "{:?}",
        reconnect.err()
    );
    let after = call(
        &mut live,
        "2",
        METHOD_SESSION_CREATE,
        serde_json::json!({"title": "t"}),
    )
    .await;
    assert!(
        after.map(|r| r.error.is_some()).unwrap_or(true),
        "the revoked device's live calls are refused or the stream is closed"
    );
    Ok(())
}
