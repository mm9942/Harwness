//! End to end through the communication layer: a real `SessionHost`, a real
//! Hyper HTTP/1 connection, a real WebSocket client, and both an in-memory
//! stream and a real Unix socket as the ingress.

use std::convert::Infallible;
use std::future::{Ready, ready};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use harw_protocol::methods::{METHOD_GATEWAY_STATUS, METHOD_SESSION_CREATE, METHOD_SESSION_HELLO};
use harw_protocol::{ProtocolVersion, RequestEnvelope, ResponseEnvelope, WireMessage};
use harw_session_com::local::{local_identity, peer_identity};
use harw_session_com::{
    BoxedService, ComBinder, ComConfig, ComError, ComRefusal, ComServer, HostBinder, PortOffer,
    TrustedPeer,
};
use harw_session_host::approvals::MemoryApprovals;
use harw_session_host::driver::{
    CancelSignal, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::replay::MemoryTranscripts;
use harw_session_host::{ClientIdentity, HostConfig, SessionHost};
use harw_session_ws::{Ports, upgrade};
use harw_types::{PermissionTier, SessionId, TrustZone};
use http_body_util::{Empty, Full};
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::Role;
use tower::{Layer, Service};

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

struct Rig {
    server: ComServer,
    host: SessionHost,
    shutdown: watch::Sender<bool>,
    dir: tempfile::TempDir,
}

fn private_tempdir() -> TestResult<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

fn rig(offer: PortOffer, tweak: impl FnOnce(&mut ComConfig)) -> TestResult<Rig> {
    let dir = private_tempdir()?;
    let host = SessionHost::open(
        HostConfig::new(dir.path().join("state")),
        Arc::new(NoopDriver),
        Arc::new(MemoryTranscripts::new()),
        Arc::new(MemoryApprovals::new()),
    )?;
    let mut config = ComConfig::default();
    tweak(&mut config);
    let (shutdown, rx) = watch::channel(false);
    let binder: Arc<dyn ComBinder> = Arc::new(HostBinder::new(host.clone(), offer));
    Ok(Rig {
        server: ComServer::new(&config, binder, rx),
        host,
        shutdown,
        dir,
    })
}

fn duplex_to(rig: &Rig, identity: ClientIdentity) -> TestResult<DuplexStream> {
    let (client, server_io) = tokio::io::duplex(1 << 16);
    rig.server.serve_io(server_io, identity)?;
    Ok(client)
}

async fn http_client<IO>(
    io: IO,
) -> TestResult<hyper::client::conn::http1::SendRequest<Empty<Bytes>>>
where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io)).await?;
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    Ok(sender)
}

async fn upgrade_ws<IO>(io: IO) -> TestResult<WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>>
where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut sender = http_client(io).await?;
    let (request, key) = upgrade::client_request("localhost")?;
    let response: Response<Incoming> = sender.send_request(request).await?;
    upgrade::verify_client_response(&response, &key)?;
    let upgraded = hyper::upgrade::on(response).await?;
    Ok(WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, None).await)
}

async fn status_of<IO>(io: IO, request: Request<Empty<Bytes>>) -> TestResult<StatusCode>
where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut sender = http_client(io).await?;
    let response =
        tokio::time::timeout(Duration::from_secs(5), sender.send_request(request)).await??;
    Ok(response.status())
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

/// Wire minor that carries the R18 (gateway and tool) caps.
const R18_MINOR: u32 = 2;

async fn hello<S>(ws: &mut WebSocketStream<S>) -> TestResult
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    hello_minor(ws, R18_MINOR).await
}

async fn hello_minor<S>(ws: &mut WebSocketStream<S>, minor: u32) -> TestResult
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let response = call(
        ws,
        "h",
        METHOD_SESSION_HELLO,
        serde_json::json!({"client_label": "com-test", "wire_minor": minor}),
    )
    .await?;
    assert!(response.error.is_none(), "{response:?}");
    Ok(())
}

fn valid_upgrade_request() -> TestResult<Request<Empty<Bytes>>> {
    Ok(upgrade::client_request("localhost")?.0)
}

#[tokio::test]
async fn an_owner_reaches_gateway_status_through_the_com_layer() -> TestResult {
    let rig = rig(PortOffer::All, |_| {})?;
    let io = duplex_to(&rig, local_identity(1000, PermissionTier::Owner))?;
    let mut ws = upgrade_ws(io).await?;
    hello(&mut ws).await?;
    let status = call(&mut ws, "1", METHOD_GATEWAY_STATUS, serde_json::json!({})).await?;
    assert!(
        status.error.is_none() && status.result.is_some(),
        "{status:?}"
    );
    let created = call(
        &mut ws,
        "2",
        METHOD_SESSION_CREATE,
        serde_json::json!({"title": "t"}),
    )
    .await?;
    assert!(created.error.is_none(), "{created:?}");
    Ok(())
}

#[tokio::test]
async fn an_old_wire_minor_never_receives_the_gateway_caps() -> TestResult {
    let rig = rig(PortOffer::All, |_| {})?;
    let io = duplex_to(&rig, local_identity(1000, PermissionTier::Owner))?;
    let mut ws = upgrade_ws(io).await?;
    hello_minor(&mut ws, 1).await?;
    let status = call(&mut ws, "1", METHOD_GATEWAY_STATUS, serde_json::json!({})).await?;
    assert!(
        status.error.is_some(),
        "the host masks the R18 caps below minor 2: {status:?}"
    );
    Ok(())
}

#[tokio::test]
async fn an_operator_is_denied_the_gateway_but_keeps_its_session() -> TestResult {
    let rig = rig(PortOffer::All, |_| {})?;
    let io = duplex_to(&rig, local_identity(1000, PermissionTier::Operator))?;
    let mut ws = upgrade_ws(io).await?;
    hello(&mut ws).await?;
    let status = call(&mut ws, "1", METHOD_GATEWAY_STATUS, serde_json::json!({})).await?;
    assert!(status.error.is_some(), "no gateway_read cap: {status:?}");
    let created = call(
        &mut ws,
        "2",
        METHOD_SESSION_CREATE,
        serde_json::json!({"title": "t"}),
    )
    .await?;
    assert!(
        created.error.is_none(),
        "the connection stays usable: {created:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_session_only_offer_hides_the_gateway_tables() -> TestResult {
    let rig = rig(PortOffer::SessionOnly, |_| {})?;
    let io = duplex_to(&rig, local_identity(1000, PermissionTier::Owner))?;
    let mut ws = upgrade_ws(io).await?;
    hello(&mut ws).await?;
    let status = call(&mut ws, "1", METHOD_GATEWAY_STATUS, serde_json::json!({})).await?;
    assert!(status.error.is_some(), "not offered: {status:?}");
    Ok(())
}

#[tokio::test]
async fn bad_handshakes_get_http_statuses_and_the_connection_closes() -> TestResult {
    let rig = rig(PortOffer::All, |_| {})?;
    let ident = || local_identity(1000, PermissionTier::Owner);

    let wrong_path = Request::builder().uri("/nope").body(Empty::new())?;
    assert_eq!(
        status_of(duplex_to(&rig, ident())?, wrong_path).await?,
        StatusCode::NOT_FOUND
    );

    let mut post = valid_upgrade_request()?;
    *post.method_mut() = hyper::Method::POST;
    assert_eq!(
        status_of(duplex_to(&rig, ident())?, post).await?,
        StatusCode::METHOD_NOT_ALLOWED
    );

    let mut origin = valid_upgrade_request()?;
    origin
        .headers_mut()
        .insert(hyper::header::ORIGIN, "https://evil.example".parse()?);
    assert_eq!(
        status_of(duplex_to(&rig, ident())?, origin).await?,
        StatusCode::FORBIDDEN
    );

    let path = valid_upgrade_request()?.uri().clone();
    let plain = Request::builder().uri(path).body(Empty::new())?;
    assert_eq!(
        status_of(duplex_to(&rig, ident())?, plain).await?,
        StatusCode::UPGRADE_REQUIRED
    );

    // keep-alive is off: after a refusal the server closes the connection.
    let mut raw = duplex_to(&rig, ident())?;
    raw.write_all(b"GET /nope HTTP/1.1\r\nHost: x\r\n\r\n")
        .await?;
    let mut reply = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), raw.read_to_end(&mut reply)).await??;
    assert!(
        String::from_utf8_lossy(&reply).contains("404"),
        "answered, then closed"
    );
    Ok(())
}

#[tokio::test]
async fn a_draining_host_answers_503_before_the_upgrade() -> TestResult {
    let rig = rig(PortOffer::All, |_| {})?;
    rig.host.drain(1000);
    let io = duplex_to(&rig, local_identity(1000, PermissionTier::Owner))?;
    let status = status_of(io, valid_upgrade_request()?).await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    Ok(())
}

#[tokio::test]
async fn the_connection_limit_covers_the_whole_session() -> TestResult {
    let rig = rig(PortOffer::All, |c| c.max_connections = 1)?;
    let first = duplex_to(&rig, local_identity(1000, PermissionTier::Owner))?;
    let mut ws = upgrade_ws(first).await?;
    hello(&mut ws).await?;
    assert_eq!(rig.server.live(), 1);
    let (_client, server_io) = tokio::io::duplex(1 << 16);
    let second = rig
        .server
        .serve_io(server_io, local_identity(1000, PermissionTier::Owner));
    assert_eq!(second.err(), Some(ComError::Busy));
    Ok(())
}

#[tokio::test]
async fn an_invalid_listener_identity_is_refused_not_guessed() -> TestResult {
    let rig = rig(PortOffer::All, |_| {})?;
    let mut remote_without_tenant = local_identity(1000, PermissionTier::Owner);
    remote_without_tenant.zone = TrustZone::Remote;
    remote_without_tenant.tenant = None;
    let (_client, server_io) = tokio::io::duplex(1 << 10);
    assert_eq!(
        rig.server.serve_io(server_io, remote_without_tenant).err(),
        Some(ComError::InvalidIdentity)
    );
    assert_eq!(
        rig.server.live(),
        0,
        "no permit is held for a refused identity"
    );
    Ok(())
}

/// Refuses callers labelled `blocked`; everything else passes through.
#[derive(Clone)]
struct BlockLabel<S>(S);

impl<S> Service<Request<Incoming>> for BlockLabel<S>
where
    S: Service<Request<Incoming>, Response = Response<Full<Bytes>>, Error = Infallible>,
{
    type Response = Response<Full<Bytes>>;
    type Error = Infallible;
    type Future =
        futures_util::future::Either<Ready<Result<Response<Full<Bytes>>, Infallible>>, S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, request: Request<Incoming>) -> Self::Future {
        let blocked = request
            .extensions()
            .get::<TrustedPeer>()
            .is_some_and(|peer| peer.0.label == "blocked");
        if blocked {
            let mut response = Response::new(Full::new(Bytes::from_static(b"blocked by policy")));
            *response.status_mut() = StatusCode::FORBIDDEN;
            futures_util::future::Either::Left(ready(Ok(response)))
        } else {
            futures_util::future::Either::Right(self.0.call(request))
        }
    }
}

struct BlockLabelLayer;

impl Layer<BoxedService> for BlockLabelLayer {
    type Service = BlockLabel<BoxedService>;
    fn layer(&self, inner: BoxedService) -> Self::Service {
        BlockLabel(inner)
    }
}

#[tokio::test]
async fn an_ingress_layer_refuses_before_the_binder_is_reached() -> TestResult {
    let binds = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&binds);
    let binder: Arc<dyn ComBinder> =
        Arc::new(move |_: ClientIdentity| -> Result<Ports, ComRefusal> {
            counter.fetch_add(1, Ordering::SeqCst);
            Err(ComRefusal::Denied)
        });
    let (_tx, rx) = watch::channel(false);
    let server = ComServer::new(&ComConfig::default(), binder, rx).layer(BlockLabelLayer);

    let (client, server_io) = tokio::io::duplex(1 << 16);
    let mut blocked = local_identity(1000, PermissionTier::Owner);
    blocked.label = "blocked".to_owned();
    server.serve_io(server_io, blocked)?;
    assert_eq!(
        status_of(client, valid_upgrade_request()?).await?,
        StatusCode::FORBIDDEN
    );
    assert_eq!(binds.load(Ordering::SeqCst), 0, "the layer answered first");

    let (client, server_io) = tokio::io::duplex(1 << 16);
    server.serve_io(server_io, local_identity(1000, PermissionTier::Owner))?;
    assert_eq!(
        status_of(client, valid_upgrade_request()?).await?,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        binds.load(Ordering::SeqCst),
        1,
        "an allowed caller reaches the binder"
    );
    Ok(())
}

#[tokio::test]
async fn shutdown_closes_live_sessions_and_drain_reports_none_left() -> TestResult {
    let rig = rig(PortOffer::All, |c| {
        c.shutdown_grace = Duration::from_secs(5)
    })?;
    let io = duplex_to(&rig, local_identity(1000, PermissionTier::Owner))?;
    let mut ws = upgrade_ws(io).await?;
    hello(&mut ws).await?;
    rig.shutdown.send(true)?;
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_) | Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(end.is_ok(), "the session ends while draining");
    assert_eq!(rig.server.drain().await, 0);
    assert_eq!(rig.server.live(), 0);
    Ok(())
}

#[tokio::test]
async fn a_stalled_header_read_is_closed_after_the_timeout() -> TestResult {
    let rig = rig(PortOffer::All, |c| {
        c.header_timeout = Duration::from_millis(200)
    })?;
    let mut io = duplex_to(&rig, local_identity(1000, PermissionTier::Owner))?;
    io.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n").await?;
    let mut sink = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), io.read_to_end(&mut sink)).await??;
    Ok(())
}

fn owner_uid(dir: &Path) -> TestResult<u32> {
    Ok(std::fs::metadata(dir)?.uid())
}

#[tokio::test]
async fn a_real_unix_socket_ingress_derives_identity_from_peer_credentials() -> TestResult {
    let rig = rig(PortOffer::All, |_| {})?;
    let uid = owner_uid(rig.dir.path())?;
    let socket = rig.dir.path().join("com.sock");
    let listener = UnixListener::bind(&socket)?;

    let server = Arc::new(rig);
    let accepting = Arc::clone(&server);
    let accept = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let identity = peer_identity(&stream, uid, PermissionTier::Owner).ok_or("uid refused")?;
        accepting.server.serve_io(stream, identity)?;
        TestResult::Ok(())
    });

    let stream = UnixStream::connect(&socket).await?;
    let mut ws = upgrade_ws(stream).await?;
    accept.await??;
    hello(&mut ws).await?;
    let status = call(&mut ws, "1", METHOD_GATEWAY_STATUS, serde_json::json!({})).await?;
    assert!(
        status.error.is_none(),
        "owner over a real socket: {status:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_peer_with_another_uid_gets_no_identity() -> TestResult {
    let dir = private_tempdir()?;
    let uid = owner_uid(dir.path())?;
    let socket = dir.path().join("uid.sock");
    let listener = UnixListener::bind(&socket)?;
    let client = UnixStream::connect(&socket).await?;
    let (accepted, _) = listener.accept().await?;
    assert!(peer_identity(&accepted, uid, PermissionTier::Owner).is_some());
    assert!(peer_identity(&accepted, uid.wrapping_add(1), PermissionTier::Owner).is_none());
    drop(client);
    Ok(())
}
