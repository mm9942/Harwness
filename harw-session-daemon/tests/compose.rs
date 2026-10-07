//! Composition entry (`Daemon::start` + `DaemonServices`) over a real Unix
//! socket with injected fakes: ports reach the host, peer uid mismatch is
//! refused, the connection cap holds and shutdown drains.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use harw_protocol::methods::{METHOD_SESSION_CREATE, METHOD_SESSION_HELLO};
use harw_protocol::{ProtocolVersion, RequestEnvelope, WireMessage};
use harw_session_daemon::{Daemon, DaemonError, DaemonServices, UdsConfig};
use harw_session_host::HostConfig;
use harw_session_host::driver::{
    CancelSignal, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
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

/// Fake driver that counts the sessions the host asked it to create.
#[derive(Default)]
struct CountingDriver {
    created: AtomicUsize,
}

impl TurnDriver for CountingDriver {
    fn create_session(&self, _: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
        self.created.fetch_add(1, Ordering::SeqCst);
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

struct Running {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), DaemonError>>,
    driver: Arc<CountingDriver>,
}

fn private_tempdir() -> TestResult<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

async fn start(tweak: impl FnOnce(&mut UdsConfig, u32)) -> TestResult<Running> {
    let dir = private_tempdir()?;
    let uid = std::fs::metadata(dir.path())?.uid();
    let socket = dir.path().join("session.sock");
    let mut uds = UdsConfig::new(&socket, uid);
    tweak(&mut uds, uid);
    let driver = Arc::new(CountingDriver::default());
    let services = DaemonServices::in_memory(driver.clone());
    let daemon = Daemon::start(HostConfig::new(dir.path().join("state")), services, uds).await?;
    let (shutdown, rx) = watch::channel(false);
    let task = tokio::spawn(daemon.run(rx));
    Ok(Running {
        _dir: dir,
        socket,
        shutdown,
        task,
        driver,
    })
}

async fn upgrade_ws(
    socket: &Path,
) -> TestResult<WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>> {
    let stream = UnixStream::connect(socket).await?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
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
) -> TestResult<()>
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
                    if let Some(error) = response.error {
                        return Err(format!("{error:?}").into());
                    }
                    return Ok(());
                }
            }
        }
    }
}

async fn hello(ws: &mut WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>) -> TestResult {
    call(
        ws,
        "h",
        METHOD_SESSION_HELLO,
        serde_json::json!({"client_label": "t", "wire_minor": 1}),
    )
    .await
}

#[tokio::test]
async fn injected_driver_is_reached_through_the_socket() -> TestResult {
    let d = start(|_, _| {}).await?;
    let mut ws = upgrade_ws(&d.socket).await?;
    hello(&mut ws).await?;
    call(
        &mut ws,
        "c",
        METHOD_SESSION_CREATE,
        serde_json::json!({"title": "t"}),
    )
    .await?;
    assert_eq!(d.driver.created.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn peer_uid_mismatch_is_refused_without_http() -> TestResult {
    let d = start(|config, uid| config.allowed_uid = uid.wrapping_add(1)).await?;
    let result = tokio::time::timeout(Duration::from_secs(5), upgrade_ws(&d.socket)).await?;
    assert!(result.is_err(), "a foreign uid must not get an upgrade");
    assert_eq!(d.driver.created.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn connection_cap_holds_and_frees_on_disconnect() -> TestResult {
    let d = start(|config, _| config.max_connections = 2).await?;
    let mut a = upgrade_ws(&d.socket).await?;
    let mut b = upgrade_ws(&d.socket).await?;
    hello(&mut a).await?;
    hello(&mut b).await?;
    let third = tokio::time::timeout(Duration::from_secs(5), upgrade_ws(&d.socket)).await?;
    assert!(third.is_err(), "the third connection exceeds the cap");
    drop(a);
    // The permit returns once the daemon notices the close; retry briefly.
    let mut reconnected = false;
    for _ in 0..50 {
        if let Ok(Ok(mut ws)) =
            tokio::time::timeout(Duration::from_secs(1), upgrade_ws(&d.socket)).await
        {
            hello(&mut ws).await?;
            reconnected = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(reconnected, "a slot must free after a disconnect");
    Ok(())
}

#[tokio::test]
async fn shutdown_drains_live_sessions_and_removes_the_socket() -> TestResult {
    let d = start(|_, _| {}).await?;
    let mut ws = upgrade_ws(&d.socket).await?;
    hello(&mut ws).await?;
    d.shutdown.send(true)?;
    tokio::time::timeout(Duration::from_secs(10), d.task).await???;
    assert!(!d.socket.exists());
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_) | Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(end.is_ok(), "live connections end while draining");
    Ok(())
}

#[tokio::test]
async fn symlinked_socket_directory_is_refused() -> TestResult {
    let dir = private_tempdir()?;
    let uid = std::fs::metadata(dir.path())?.uid();
    let real = dir.path().join("real");
    std::fs::create_dir(&real)?;
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700))?;
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link)?;
    let services = DaemonServices::in_memory(Arc::new(CountingDriver::default()));
    let result = Daemon::start(
        HostConfig::new(dir.path().join("state")),
        services,
        UdsConfig::new(link.join("s.sock"), uid),
    )
    .await;
    assert!(
        matches!(result, Err(DaemonError::NotADirectory(_))),
        "{result:?}"
    );
    Ok(())
}

/// While a connection holds the daemon in its grace period, the socket must
/// stop accepting: a client that connects then used to be taken into the
/// kernel backlog and never served (found by an external review).
#[tokio::test]
async fn a_new_client_is_refused_at_once_while_the_daemon_drains() -> TestResult {
    let d = start(|c, _| {
        c.shutdown_grace = Duration::from_secs(5);
        c.header_timeout = Duration::from_secs(30);
    })
    .await?;
    // A stuck peer: connected, never sends a byte, holds its permit for the
    // whole header timeout, so the drain really waits.
    let stuck = UnixStream::connect(&d.socket).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    d.shutdown.send(true)?;
    let gone = tokio::time::timeout(Duration::from_secs(2), async {
        while UnixStream::connect(&d.socket).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(gone.is_ok(), "new connections must fail while draining");
    assert!(!d.socket.exists(), "the path is removed before the drain");
    drop(stuck);
    tokio::time::timeout(Duration::from_secs(10), d.task).await???;
    Ok(())
}
