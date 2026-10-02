//! Self-cloud ingress end to end: the real node transport (PQ TLS plus an
//! ML-DSA handshake) carries a WebSocket session into the same `ComServer`
//! stack the local socket uses. PL-68 W02 + W06.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use aws_lc_rs::signature::{KeyPair, ML_DSA_65_SIGNING, PqdsaKeyPair};
use futures_util::{SinkExt, StreamExt};
use harw_node_transport::{
    AuthenticatedPeer, LocalNode, ML_DSA_65_SIGNATURE_LEN, NodeIdentity, NodeSigner,
    NodeTransportClient, NodeTransportServer, PinnedPeers, ServerOptions, SignFuture, empty_body,
};
use harw_protocol::methods::{METHOD_GATEWAY_STATUS, METHOD_SESSION_CREATE, METHOD_SESSION_HELLO};
use harw_protocol::{ProtocolVersion, RequestEnvelope, ResponseEnvelope, WireMessage};
use harw_session_com::{
    ComConfig, ComServer, DeviceRecord, DeviceRegistry, HostBinder, PortOffer, RemoteLayer,
};
use harw_session_host::approvals::MemoryApprovals;
use harw_session_host::driver::{
    CancelSignal, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::replay::MemoryTranscripts;
use harw_session_host::{HostConfig, SessionHost};
use harw_session_ws::upgrade;
use harw_types::{DeviceId, NodeId, PermissionTier, SessionId, TenantId};
use hyper::StatusCode;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::Role;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

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

/// **TEST ONLY.** Deterministic ML-DSA-65 key from a seed.
struct SeedSigner {
    key: PqdsaKeyPair,
}

impl SeedSigner {
    fn new(seed: u8) -> TestResult<Self> {
        Ok(Self {
            key: PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, &[seed; 32])
                .map_err(|_| "ml-dsa seed")?,
        })
    }
}

impl NodeSigner for SeedSigner {
    fn sign<'a>(&'a self, transcript: &'a [u8]) -> SignFuture<'a> {
        let mut signature = vec![0u8; ML_DSA_65_SIGNATURE_LEN];
        let result = self
            .key
            .sign(transcript, &mut signature)
            .map(|n| {
                signature.truncate(n);
                signature
            })
            .map_err(|_| harw_node_transport::SignerError::new("test signer failed"));
        Box::pin(std::future::ready(result))
    }
}

struct Node {
    id: NodeId,
    identity: NodeIdentity,
    local: LocalNode,
}

fn node(name: &str, seed: u8) -> TestResult<Node> {
    let signer = SeedSigner::new(seed)?;
    let id = NodeId::try_from_str(name)?;
    let identity = NodeIdentity {
        node_id: id.clone(),
        public_key: signer.key.public_key().as_ref().to_vec(),
        key_ref: format!("test-only/{name}"),
    };
    let local = LocalNode::new(identity.clone(), Arc::new(signer));
    Ok(Node {
        id,
        identity,
        local,
    })
}

fn pinned(nodes: &[&NodeIdentity]) -> TestResult<PinnedPeers> {
    let mut peers = PinnedPeers::new();
    for n in nodes {
        peers.pin_identity(n)?;
    }
    Ok(peers)
}

struct Cloud {
    addr: SocketAddr,
    gateway: Node,
    host: SessionHost,
    _shutdown: watch::Sender<bool>,
    _dir: tempfile::TempDir,
}

/// A gateway node serving the Com layer behind the node transport. Only
/// `phone` is trusted at the transport; `registry` decides who is a device.
async fn cloud(phone: &Node, other: Option<&Node>, registry: DeviceRegistry) -> TestResult<Cloud> {
    let gateway = node("gateway", 1)?;
    let dir = tempfile::tempdir()?;
    let host = SessionHost::open(
        HostConfig::new(dir.path().join("state")),
        Arc::new(NoopDriver),
        Arc::new(MemoryTranscripts::new()),
        Arc::new(MemoryApprovals::new()),
    )?;
    let (shutdown, rx) = watch::channel(false);
    let binder = Arc::new(HostBinder::new(host.clone(), PortOffer::All));
    let com = ComServer::new(&ComConfig::default(), binder, rx);
    let service = com.service_for(RemoteLayer::<AuthenticatedPeer>::from_registry(
        Arc::new(registry),
        |peer| peer.node_id.clone(),
    ));

    let mut trusted = vec![&phone.identity];
    if let Some(o) = other {
        trusted.push(&o.identity);
    }
    let server = NodeTransportServer::new(
        gateway.local.clone(),
        Arc::new(pinned(&trusted)?),
        ServerOptions::default(),
    )?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move { server.serve(listener, service).await });
    Ok(Cloud {
        addr,
        gateway,
        host,
        _shutdown: shutdown,
        _dir: dir,
    })
}

fn registry_with(node: &Node, device: &str, tier: PermissionTier) -> TestResult<DeviceRegistry> {
    let mut registry = DeviceRegistry::new();
    registry.enroll(
        node.id.clone(),
        DeviceRecord {
            device: DeviceId::try_from_str(device)?,
            tenant: TenantId::try_from_str("tenant-a")?,
            tier,
            label: "phone".to_owned(),
            may_approve: false,
        },
    )?;
    Ok(registry)
}

async fn connect(cloud: &Cloud, from: &Node) -> TestResult<NodeTransportClient> {
    let trust = pinned(&[&cloud.gateway.identity])?;
    Ok(NodeTransportClient::connect(cloud.addr, &from.local, &cloud.gateway.id, &trust).await?)
}

async fn upgrade_status(client: &mut NodeTransportClient) -> TestResult<StatusCode> {
    let (request, _) = upgrade::client_request("gateway")?;
    let response = client.send_request(request.map(|_| empty_body())).await?;
    Ok(response.status())
}

async fn ws(
    client: &mut NodeTransportClient,
) -> TestResult<WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>> {
    let (request, key) = upgrade::client_request("gateway")?;
    let response = client.send_request(request.map(|_| empty_body())).await?;
    upgrade::verify_client_response(&response, &key)?;
    let upgraded = hyper::upgrade::on(response).await?;
    Ok(WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, None).await)
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
    let cloud = cloud(
        &phone,
        None,
        registry_with(&phone, "dev-1", PermissionTier::Operator)?,
    )
    .await?;
    let mut client = connect(&cloud, &phone).await?;
    let mut ws = ws(&mut client).await?;
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
async fn a_remote_device_never_gets_the_gateway_even_as_owner() -> TestResult {
    let phone = node("phone", 2)?;
    let cloud = cloud(
        &phone,
        None,
        registry_with(&phone, "dev-1", PermissionTier::Owner)?,
    )
    .await?;
    let mut client = connect(&cloud, &phone).await?;
    let mut ws = ws(&mut client).await?;
    hello(&mut ws).await?;
    let status = call(&mut ws, "1", METHOD_GATEWAY_STATUS, serde_json::json!({})).await?;
    assert!(
        status.error.is_some(),
        "remote caps never include gateway_*: {status:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_transport_trusted_node_that_is_not_a_device_is_refused_with_403() -> TestResult {
    let phone = node("phone", 2)?;
    let stranger = node("stranger", 3)?;
    // The transport trusts both nodes; only the phone is enrolled.
    let cloud = cloud(
        &phone,
        Some(&stranger),
        registry_with(&phone, "dev-1", PermissionTier::Operator)?,
    )
    .await?;
    let mut client = connect(&cloud, &stranger).await?;
    assert_eq!(upgrade_status(&mut client).await?, StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn a_revoked_device_cannot_reconnect() -> TestResult {
    let phone = node("phone", 2)?;
    let cloud = cloud(
        &phone,
        None,
        registry_with(&phone, "dev-1", PermissionTier::Operator)?,
    )
    .await?;
    {
        let mut client = connect(&cloud, &phone).await?;
        let mut ws = ws(&mut client).await?;
        hello(&mut ws).await?;
    }
    cloud.host.revoke_device(&DeviceId::try_from_str("dev-1")?);
    let mut client = connect(&cloud, &phone).await?;
    assert_eq!(upgrade_status(&mut client).await?, StatusCode::FORBIDDEN);
    Ok(())
}
