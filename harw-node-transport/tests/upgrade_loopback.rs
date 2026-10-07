//! S02 loopback tests of the authenticated HTTP/1 upgrade (W00 §2.3, ID-02).
//!
//! The service below plays the role of the session layer's HTTP gate: it
//! applies the WebSocket handshake rules the session layer enforces
//! (subprotocol `harw.session.v1`, `Origin` refused) and takes the peer only
//! from [`peer_of`]. The raw upgraded I/O is exercised with an echo; the
//! WebSocket framing itself (`harw-session-ws`) is out of this crate's
//! scope: this crate has no dev-dependency on it.
//!
//! Panic-free: every test returns a `Result`.

use std::convert::Infallible;
use std::error::Error;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use aws_lc_rs::signature::{KeyPair, ML_DSA_65_SIGNING, PqdsaKeyPair};
use bytes::Bytes;
use harw_node_transport::{
    LocalNode, ML_DSA_65_SIGNATURE_LEN, NodeBody, NodeIdentity, NodeSigner, NodeTransportClient,
    NodeTransportServer, PinnedPeers, ServerOptions, SignFuture, SignerError, UpgradeError,
    accept_upgrade, empty_body, peer_of,
};
use harw_types::NodeId;
use http::header::{CONNECTION, ORIGIN, SEC_WEBSOCKET_PROTOCOL, UPGRADE};
use http::{Method, Request, Response, StatusCode};
use http_body_util::Full;
use hyper::body::Incoming;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const SUBPROTOCOL: &str = "harw.session.v1";
const PATH: &str = "/v1/session-ws";

// --- test-only deterministic node keys ------------------------------------------

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

struct Node {
    id: NodeId,
    identity: NodeIdentity,
    local: LocalNode,
}

fn node(name: &str, seed: u8) -> TestResult<Node> {
    let id = NodeId::try_from_str(name)?;
    let key = PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, &[seed; 32])
        .map_err(|_| "ML-DSA-65 key from seed")?;
    let identity = NodeIdentity {
        node_id: id.clone(),
        public_key: key.public_key().as_ref().to_vec(),
        key_ref: format!("test-only/{name}"),
    };
    let local = LocalNode::new(identity.clone(), Arc::new(TestSigner { key }));
    Ok(Node {
        id,
        identity,
        local,
    })
}

fn pinned(nodes: &[&NodeIdentity]) -> TestResult<PinnedPeers> {
    let mut peers = PinnedPeers::new();
    for node in nodes {
        peers.pin_identity(node)?;
    }
    Ok(peers)
}

// --- the service under test --------------------------------------------------------

/// What the service observed for one accepted upgrade.
#[derive(Debug)]
struct Seen {
    extension_peer: NodeId,
    upgraded_peer: NodeId,
    forged_header: Option<String>,
}

#[derive(Clone)]
struct GateService {
    calls: Arc<AtomicUsize>,
    seen: mpsc::UnboundedSender<Seen>,
}

fn reply(status: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
    response
}

impl GateService {
    async fn handle(self, request: Request<Incoming>) -> Response<Full<Bytes>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let seen = self.seen;
        let headers = request.headers();
        let offers = headers
            .get_all(SEC_WEBSOCKET_PROTOCOL)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|token| token.trim() == SUBPROTOCOL);
        if !offers {
            return reply(StatusCode::BAD_REQUEST);
        }
        if headers.contains_key(ORIGIN) {
            return reply(StatusCode::FORBIDDEN);
        }
        let Ok(extension_peer) = peer_of(&request) else {
            return reply(StatusCode::UNAUTHORIZED);
        };
        let forged_header = headers
            .get("x-harw-node-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        tokio::spawn(async move {
            let Ok(upgraded) = accept_upgrade(request).await else {
                return;
            };
            let upgraded_peer = upgraded.peer().node_id.clone();
            let _ = seen.send(Seen {
                extension_peer: extension_peer.node_id,
                upgraded_peer,
                forged_header,
            });
            let (_, mut io) = upgraded.into_parts();
            let mut buf = [0u8; 64];
            while let Ok(read) = io.read(&mut buf).await {
                let Some(chunk) = buf.get(..read) else { break };
                if read == 0 || io.write_all(chunk).await.is_err() {
                    break;
                }
            }
        });
        let mut response = reply(StatusCode::SWITCHING_PROTOCOLS);
        let out = response.headers_mut();
        out.insert(CONNECTION, http::HeaderValue::from_static("Upgrade"));
        out.insert(UPGRADE, http::HeaderValue::from_static("websocket"));
        out.insert(
            SEC_WEBSOCKET_PROTOCOL,
            http::HeaderValue::from_static(SUBPROTOCOL),
        );
        response
    }
}

impl tower_service::Service<Request<Incoming>> for GateService {
    type Response = Response<Full<Bytes>>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Incoming>) -> Self::Future {
        let this = self.clone();
        Box::pin(async move { Ok(this.handle(request).await) })
    }
}

struct Harness {
    addr: SocketAddr,
    calls: Arc<AtomicUsize>,
    seen: mpsc::UnboundedReceiver<Seen>,
    server: JoinHandle<()>,
    a: Node,
    b: Node,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// Node A serves and pins only node B.
async fn harness() -> TestResult<Harness> {
    let a = node("node-a", 1)?;
    let b = node("node-b", 2)?;
    let server_node = NodeTransportServer::new(
        a.local.clone(),
        Arc::new(pinned(&[&b.identity])?),
        ServerOptions::default(),
    )?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let calls = Arc::new(AtomicUsize::new(0));
    let (tx, seen) = mpsc::unbounded_channel();
    let service = GateService {
        calls: Arc::clone(&calls),
        seen: tx,
    };
    let server = tokio::spawn(async move {
        let _ = server_node
            .serve_upgradable(listener, service, std::future::pending())
            .await;
    });
    Ok(Harness {
        addr,
        calls,
        seen,
        server,
        a,
        b,
    })
}

async fn connect(h: &Harness) -> TestResult<NodeTransportClient> {
    let trust = pinned(&[&h.a.identity])?;
    Ok(NodeTransportClient::connect(h.addr, &h.b.local, &h.a.id, &trust).await?)
}

fn upgrade_request(
    protocol: Option<&str>,
    extra: &[(&'static str, &str)],
) -> TestResult<Request<NodeBody>> {
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri(PATH)
        .header("host", "node-a")
        .header(CONNECTION, "Upgrade")
        .header(UPGRADE, "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
    if let Some(protocol) = protocol {
        builder = builder.header(SEC_WEBSOCKET_PROTOCOL, protocol);
    }
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    Ok(builder.body(empty_body())?)
}

async fn within<T>(future: impl Future<Output = T>) -> TestResult<T> {
    Ok(tokio::time::timeout(Duration::from_secs(20), future).await?)
}

fn ensure(condition: bool, detail: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(detail.to_owned().into())
    }
}

/// Upgrade attempt that must be refused with `status` and must not complete
/// an upgrade.
async fn expect_refused(request: Request<NodeBody>, status: StatusCode) -> TestResult {
    let mut h = harness().await?;
    let client = connect(&h).await?;
    match within(client.upgrade(request)).await? {
        Err(UpgradeError::NotSwitched(code)) if code == status.as_u16() => {}
        other => return Err(format!("expected NotSwitched({status}), got {other:?}").into()),
    }
    ensure(
        h.seen.try_recv().is_err(),
        "refused request must not upgrade",
    )
}

// --- tests ---------------------------------------------------------------------------

#[tokio::test]
async fn loopback_upgrade_exposes_authenticated_peer_and_carries_bytes() -> TestResult {
    let mut h = harness().await?;
    let client = connect(&h).await?;
    let upgraded = within(client.upgrade(upgrade_request(Some(SUBPROTOCOL), &[])?)).await??;

    ensure(upgraded.peer.node_id == h.a.id, "client sees server node-a")?;
    ensure(
        upgraded.response.status() == StatusCode::SWITCHING_PROTOCOLS,
        "101",
    )?;
    let mut io = upgraded.io;
    io.write_all(b"ping").await?;
    let mut echoed = [0u8; 4];
    within(io.read_exact(&mut echoed)).await??;
    ensure(&echoed == b"ping", "echo over the upgraded stream")?;

    let seen = within(h.seen.recv())
        .await?
        .ok_or("service saw no upgrade")?;
    ensure(seen.extension_peer == h.b.id, "extension peer is node-b")?;
    ensure(seen.upgraded_peer == h.b.id, "upgraded io peer is node-b")
}

#[tokio::test]
async fn wrong_or_missing_subprotocol_is_rejected() -> TestResult {
    expect_refused(
        upgrade_request(Some("harw.session.v2"), &[])?,
        StatusCode::BAD_REQUEST,
    )
    .await?;
    expect_refused(upgrade_request(None, &[])?, StatusCode::BAD_REQUEST).await
}

#[tokio::test]
async fn origin_header_is_rejected() -> TestResult {
    expect_refused(
        upgrade_request(Some(SUBPROTOCOL), &[("origin", "https://evil.example")])?,
        StatusCode::FORBIDDEN,
    )
    .await
}

#[tokio::test]
async fn unauthenticated_peers_never_reach_the_service() -> TestResult {
    let h = harness().await?;

    // A node the server has not pinned: the handshake fails.
    let stranger = node("node-c", 3)?;
    let trust = pinned(&[&h.a.identity])?;
    let verdict = within(NodeTransportClient::connect(
        h.addr,
        &stranger.local,
        &h.a.id,
        &trust,
    ))
    .await?;
    ensure(verdict.is_err(), "unpinned node must not connect")?;

    // Plaintext HTTP without TLS / handshake.
    let mut tcp = TcpStream::connect(h.addr).await?;
    let raw = format!(
        "GET {PATH} HTTP/1.1\r\nHost: x\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
         Sec-WebSocket-Protocol: {SUBPROTOCOL}\r\n\r\n"
    );
    tcp.write_all(raw.as_bytes()).await?;
    let mut reply_bytes = Vec::new();
    // Closed or errored, never an HTTP reply from the service.
    let _ = within(tcp.read_to_end(&mut reply_bytes)).await?;
    ensure(
        !reply_bytes.starts_with(b"HTTP/1.1"),
        "no HTTP answer without handshake",
    )?;

    ensure(
        h.calls.load(Ordering::SeqCst) == 0,
        "service was never called",
    )
}

#[tokio::test]
async fn forged_identity_header_is_ignored() -> TestResult {
    let mut h = harness().await?;
    let client = connect(&h).await?;
    let request = upgrade_request(
        Some(SUBPROTOCOL),
        &[
            ("x-harw-node-id", "node-a"),
            ("x-forwarded-for", "10.0.0.1"),
        ],
    )?;
    let _upgraded = within(client.upgrade(request)).await??;
    let seen = within(h.seen.recv())
        .await?
        .ok_or("service saw no upgrade")?;
    ensure(
        seen.forged_header.as_deref() == Some("node-a"),
        "header reached the service",
    )?;
    ensure(
        seen.extension_peer == h.b.id,
        "identity is the authenticated node-b",
    )?;
    ensure(
        seen.upgraded_peer == h.b.id,
        "upgraded peer is node-b, not the header",
    )
}
