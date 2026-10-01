//! End-to-end over a real Unix socket: peer credentials, upgrade, hello,
//! session create, limits, stale-socket handling and graceful shutdown.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use harw_protocol::methods::{METHOD_SESSION_CREATE, METHOD_SESSION_HELLO};
use harw_protocol::{ProtocolVersion, RequestEnvelope, ResponseEnvelope, WireMessage};
use harw_session_daemon::{DaemonError, UdsConfig, UdsServer};
use harw_session_host::approvals::MemoryApprovals;
use harw_session_host::driver::{
    CancelSignal, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::replay::MemoryTranscripts;
use harw_session_host::{HostConfig, SessionHost};
use harw_session_ws::upgrade;
use harw_types::SessionId;
use http_body_util::Empty;
use hyper::body::{Bytes, Incoming};
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
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

struct Daemon {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), DaemonError>>,
    host: SessionHost,
}

fn open_host(dir: &Path) -> TestResult<SessionHost> {
    Ok(SessionHost::open(
        HostConfig::new(dir.join("state")),
        Arc::new(NoopDriver),
        Arc::new(MemoryTranscripts::new()),
        Arc::new(MemoryApprovals::new()),
    )?)
}

/// A fresh private (mode 0700) directory; `tempfile` follows the umask.
fn private_tempdir() -> TestResult<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// This process's uid: the owner of a file it just created.
fn own_uid(dir: &Path) -> TestResult<u32> {
    Ok(std::fs::metadata(dir)?.uid())
}

async fn start(tweak: impl FnOnce(&mut UdsConfig, u32)) -> TestResult<Daemon> {
    let dir = private_tempdir()?;
    let uid = own_uid(dir.path())?;
    let socket = dir.path().join("session.sock");
    let mut config = UdsConfig::new(&socket, uid);
    tweak(&mut config, uid);
    let host = open_host(dir.path())?;
    let server = UdsServer::bind(config).await?;
    let (shutdown, rx) = watch::channel(false);
    let task = tokio::spawn(server.run(host.clone(), rx));
    Ok(Daemon {
        _dir: dir,
        socket,
        shutdown,
        task,
        host,
    })
}

async fn http_client(
    socket: &Path,
) -> TestResult<hyper::client::conn::http1::SendRequest<Empty<Bytes>>> {
    let stream = UnixStream::connect(socket).await?;
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    Ok(sender)
}

async fn upgrade_ws(
    socket: &Path,
) -> TestResult<WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>> {
    let mut sender = http_client(socket).await?;
    let (request, key) = upgrade::client_request("localhost")?;
    let response: hyper::Response<Incoming> = sender.send_request(request).await?;
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

#[tokio::test]
async fn owner_can_hello_and_create_a_session() -> TestResult {
    let d = start(|_, _| {}).await?;
    let mut ws = upgrade_ws(&d.socket).await?;
    let hello = call(
        &mut ws,
        "1",
        METHOD_SESSION_HELLO,
        serde_json::json!({"client_label": "uds", "wire_minor": 1}),
    )
    .await?;
    assert!(hello.error.is_none(), "{hello:?}");
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
async fn socket_file_is_owner_only() -> TestResult {
    let d = start(|_, _| {}).await?;
    let mode = std::fs::metadata(&d.socket)?.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    Ok(())
}

#[tokio::test]
async fn peer_with_other_uid_is_dropped_before_http() -> TestResult {
    // Serve a uid that is not ours: our own connection must be refused.
    let d = start(|config, uid| config.allowed_uid = uid.wrapping_add(1)).await?;
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let mut sender = http_client(&d.socket).await?;
        let (request, _) = upgrade::client_request("localhost")?;
        sender.send_request(request).await?;
        TestResult::Ok(())
    })
    .await?;
    assert!(
        result.is_err(),
        "a disallowed uid must get no HTTP response"
    );
    Ok(())
}

#[tokio::test]
async fn connection_limit_drops_excess_connections() -> TestResult {
    let d = start(|config, _| config.max_connections = 1).await?;
    let mut first = upgrade_ws(&d.socket).await?;
    let hello = call(
        &mut first,
        "1",
        METHOD_SESSION_HELLO,
        serde_json::json!({"client_label": "a", "wire_minor": 1}),
    )
    .await?;
    assert!(hello.error.is_none());
    let second = tokio::time::timeout(Duration::from_secs(5), upgrade_ws(&d.socket)).await?;
    assert!(second.is_err(), "the second connection must be dropped");
    Ok(())
}

#[tokio::test]
async fn stale_socket_is_replaced_live_one_is_not() -> TestResult {
    let dir = private_tempdir()?;
    let uid = own_uid(dir.path())?;
    let socket = dir.path().join("s.sock");
    // A stale socket: bound, then the listener dropped without unlinking.
    drop(std::os::unix::net::UnixListener::bind(&socket)?);
    assert!(socket.exists());
    let server = UdsServer::bind(UdsConfig::new(&socket, uid)).await?;
    // Now live: a second bind must refuse.
    let second = UdsServer::bind(UdsConfig::new(&socket, uid)).await;
    assert!(
        matches!(second, Err(DaemonError::AlreadyRunning(_))),
        "{second:?}"
    );
    drop(server);
    Ok(())
}

#[tokio::test]
async fn non_socket_file_is_never_touched() -> TestResult {
    let dir = private_tempdir()?;
    let uid = own_uid(dir.path())?;
    let path = dir.path().join("precious");
    std::fs::write(&path, b"data")?;
    let result = UdsServer::bind(UdsConfig::new(&path, uid)).await;
    assert!(
        matches!(result, Err(DaemonError::NotASocket(_))),
        "{result:?}"
    );
    assert_eq!(std::fs::read(&path)?, b"data");
    Ok(())
}

#[tokio::test]
async fn directory_open_to_group_or_others_is_refused() -> TestResult {
    let dir = tempfile::tempdir()?;
    let uid = own_uid(dir.path())?;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))?;
    let result = UdsServer::bind(UdsConfig::new(dir.path().join("s.sock"), uid)).await;
    assert!(
        matches!(result, Err(DaemonError::InsecureDirectory(_))),
        "{result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn shutdown_drains_closes_sessions_and_removes_the_socket() -> TestResult {
    let d = start(|_, _| {}).await?;
    let mut ws = upgrade_ws(&d.socket).await?;
    let hello = call(
        &mut ws,
        "1",
        METHOD_SESSION_HELLO,
        serde_json::json!({"client_label": "x", "wire_minor": 1}),
    )
    .await?;
    assert!(hello.error.is_none());
    d.shutdown.send(true)?;
    tokio::time::timeout(Duration::from_secs(5), d.task).await???;
    assert!(!d.socket.exists(), "the socket file is removed on shutdown");
    // The live connection ends (close frame or end of stream).
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_) | Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(end.is_ok(), "live connections are closed while draining");
    assert!(UnixStream::connect(&d.socket).await.is_err());
    let _ = d.host;
    Ok(())
}

#[tokio::test]
async fn incomplete_headers_are_closed_after_the_header_timeout() -> TestResult {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let d = start(|config, _| config.header_timeout = Duration::from_millis(200)).await?;
    let mut stream = UnixStream::connect(&d.socket).await?;
    // A request line and one header, but never the terminating blank line.
    stream.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n").await?;
    let mut sink = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut sink)).await??;
    Ok(())
}

#[tokio::test]
async fn shutdown_does_not_remove_a_replaced_path() -> TestResult {
    let d = start(|_, _| {}).await?;
    // Something else now occupies the pathname.
    std::fs::remove_file(&d.socket)?;
    std::fs::write(&d.socket, b"not ours")?;
    d.shutdown.send(true)?;
    tokio::time::timeout(Duration::from_secs(5), d.task).await???;
    assert_eq!(std::fs::read(&d.socket)?, b"not ours");
    Ok(())
}

#[tokio::test]
async fn shutdown_waits_a_bounded_time_for_open_connections() -> TestResult {
    use tokio::io::AsyncWriteExt;
    let d = start(|config, _| {
        config.header_timeout = Duration::from_secs(30);
        config.shutdown_grace = Duration::from_millis(300);
    })
    .await?;
    // A connection stuck in the HTTP phase keeps its permit for 30 s.
    let mut stuck = UnixStream::connect(&d.socket).await?;
    stuck.write_all(b"GET / HTTP/1.1\r\n").await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = std::time::Instant::now();
    d.shutdown.send(true)?;
    tokio::time::timeout(Duration::from_secs(5), d.task).await???;
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(250),
        "returned too early: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(3),
        "waited past the grace period: {waited:?}"
    );
    Ok(())
}
