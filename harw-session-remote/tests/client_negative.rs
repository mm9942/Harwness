//! Negative paths of the client core against the real server stack:
//! oversized messages, a host that picks the wrong subprotocol or refuses the
//! upgrade, a host that closes the connection, and a client that is too slow
//! for the live stream (lossy marker). Every failure is a typed error; no
//! test panics.

use std::sync::Arc;
use std::time::Duration;

use harw_protocol::session_wire::{
    AttachParams, CreateParams, DEFAULT_TAIL_ITEMS, StreamProfile, SubmitParams,
};
use harw_protocol::{PortError, SessionFrame, SessionPort, TurnEvent};
use harw_session_host::approvals::MemoryApprovals;
use harw_session_host::driver::{
    CancelSignal, DriverEvent, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::fanout::QueueLimits;
use harw_session_host::replay::MemoryTranscripts;
use harw_session_host::{ClientIdentity, ConnectionId, HostConfig, SessionHost, caps_for_tier};
use harw_session_remote::{ConnectOptions, RemoteError, RemotePort, connect_io};
use harw_session_ws::{WsLimits, serve_connection, upgrade};
use harw_types::{
    ApprovalActor, AuthStrength, IngressSurface, PermissionTier, Principal, PrincipalKind,
    SessionId, ThreadId, TrustZone, TurnId,
};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::header::HeaderValue;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::sync::watch;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const WAIT: Duration = Duration::from_secs(10);

/// Emits `TurnStarted`, then `pairs` alternating assistant and reasoning
/// deltas (alternation keeps the host from coalescing them, so a small host
/// queue overflows deterministically).
struct FloodDriver {
    pairs: usize,
}

impl TurnDriver for FloodDriver {
    fn create_session(&self, _: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn run_turn(
        &self,
        input: TurnInput,
        _: CancelSignal,
        sink: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        let pairs = self.pairs;
        Box::pin(async move {
            let turn_id = TurnId::new();
            sink.emit(DriverEvent::Turn(TurnEvent::TurnStarted {
                turn_id: turn_id.clone(),
                thread_id: ThreadId::new(),
            }));
            for index in 0..pairs {
                sink.emit(DriverEvent::Turn(TurnEvent::AssistantDelta {
                    turn_id: turn_id.clone(),
                    text: format!("{}#{index}", input.text),
                }));
                sink.emit(DriverEvent::Turn(TurnEvent::ReasoningDelta {
                    turn_id: turn_id.clone(),
                    text: format!("r#{index}"),
                }));
            }
            Ok(TurnOutcome::Completed)
        })
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

fn identity() -> ClientIdentity {
    ClientIdentity {
        principal: Principal::trusted_ingress(
            PrincipalKind::Human,
            "uid:1000",
            IngressSurface::Tui,
            PermissionTier::Operator,
        ),
        tenant: None,
        caps: caps_for_tier(PermissionTier::Operator),
        device: None,
        actor: ApprovalActor::Operator {
            id: "uid:1000".into(),
        },
        label: "negative".into(),
        zone: TrustZone::Local,
        strength: AuthStrength::PeerCredential,
        connection: ConnectionId::next(),
        agent: None,
    }
}

fn open_host(
    dir: &std::path::Path,
    pairs: usize,
    queue: Option<QueueLimits>,
) -> TestResult<SessionHost> {
    let mut config = HostConfig::new(dir.to_path_buf());
    if let Some(queue) = queue {
        config.queue = queue;
    }
    Ok(SessionHost::open(
        config,
        Arc::new(FloodDriver { pairs }),
        Arc::new(MemoryTranscripts::new()),
        Arc::new(MemoryApprovals::new()),
    )?)
}

/// Serve one HTTP/1 connection that upgrades into the session control plane.
fn serve(io: tokio::io::DuplexStream, host: SessionHost, shutdown: watch::Receiver<bool>) {
    tokio::spawn(async move {
        let service = service_fn(move |request: hyper::Request<Incoming>| {
            let host = host.clone();
            let shutdown = shutdown.clone();
            async move {
                let limits = WsLimits::default();
                let upgraded = upgrade::upgrade_server(
                    request,
                    limits.tungstenite_config(),
                    move |ws| async move {
                        let Ok(port) = host.connect(identity()) else {
                            return;
                        };
                        let _ = serve_connection(ws, Arc::new(port), limits, shutdown).await;
                    },
                );
                let response = match upgraded {
                    Ok(response) => response.map(|_| Full::new(Bytes::new())),
                    Err(rejection) => upgrade::rejection_response(&rejection),
                };
                Ok::<_, std::convert::Infallible>(response)
            }
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(io), service)
            .with_upgrades()
            .await;
    });
}

/// A misbehaving server: runs `respond` on every upgrade request.
fn serve_with<F>(io: tokio::io::DuplexStream, respond: F)
where
    F: Fn(hyper::Request<Incoming>) -> hyper::Response<Full<Bytes>> + Send + Sync + 'static,
{
    tokio::spawn(async move {
        let respond = Arc::new(respond);
        let service = service_fn(move |request: hyper::Request<Incoming>| {
            let respond = Arc::clone(&respond);
            async move { Ok::<_, std::convert::Infallible>(respond(request)) }
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(io), service)
            .with_upgrades()
            .await;
    });
}

async fn connect(
    host: SessionHost,
    options: ConnectOptions,
) -> TestResult<(Arc<RemotePort>, watch::Sender<bool>)> {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    serve(server_io, host, shutdown_rx);
    let connection = connect_io(client_io, options).await?;
    Ok((Arc::new(RemotePort::new(connection)), shutdown_tx))
}

fn attach_params(session: &SessionId) -> AttachParams {
    AttachParams {
        session_id: session.clone(),
        from: None,
        profile: StreamProfile::Full,
        tail_items: DEFAULT_TAIL_ITEMS,
    }
}

fn submit_params(session: &SessionId, text: String, head: harw_protocol::Cursor) -> SubmitParams {
    SubmitParams {
        session_id: session.clone(),
        text,
        expect_head: head,
        client_msg_id: "m".into(),
        force: false,
    }
}

#[tokio::test]
async fn oversized_message_ends_the_connection_with_a_typed_error() -> TestResult {
    let dir = tempfile::tempdir()?;
    // The client allows 8 MiB; the server (default 1 MiB) does not.
    let mut options = ConnectOptions::new("big");
    options.limits = WsLimits {
        max_message_bytes: 8 * 1024 * 1024,
        max_frame_bytes: 8 * 1024 * 1024,
        ..WsLimits::default()
    };
    let (port, _shutdown) = connect(open_host(dir.path(), 0, None)?, options).await?;
    let created = port.create(CreateParams::default()).await?;
    let (ack, _frames) = port.attach(attach_params(&created.session_id)).await?;

    let huge = "x".repeat(2 * 1024 * 1024);
    let result = tokio::time::timeout(
        WAIT,
        port.submit(submit_params(&created.session_id, huge, ack.head)),
    )
    .await?;
    assert!(matches!(result, Err(PortError::Transport(_))), "{result:?}");

    // The connection is gone, and every later call says so.
    let later = tokio::time::timeout(WAIT, port.list()).await?;
    assert!(matches!(later, Err(PortError::Transport(_))), "{later:?}");
    Ok(())
}

#[tokio::test]
async fn client_refuses_to_send_above_its_own_limit_and_stays_usable() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (port, _shutdown) = connect(
        open_host(dir.path(), 0, None)?,
        ConnectOptions::new("small"),
    )
    .await?;
    let created = port.create(CreateParams::default()).await?;
    let (ack, _frames) = port.attach(attach_params(&created.session_id)).await?;

    let huge = "x".repeat(2 * 1024 * 1024);
    let result = port
        .submit(submit_params(&created.session_id, huge, ack.head))
        .await;
    assert!(matches!(result, Err(PortError::Protocol(_))), "{result:?}");
    // Nothing was sent, so the connection is unharmed.
    assert_eq!(port.list().await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn host_with_the_wrong_subprotocol_is_refused() -> TestResult {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    serve_with(server_io, |request| {
        // A valid 101 in every respect except the selected subprotocol.
        match upgrade::accept_response(&request) {
            Ok(mut response) => {
                response
                    .headers_mut()
                    .insert("sec-websocket-protocol", HeaderValue::from_static("chat"));
                response.map(|_| Full::new(Bytes::new()))
            }
            Err(rejection) => upgrade::rejection_response(&rejection),
        }
    });
    let result = connect_io(client_io, ConnectOptions::new("t")).await;
    assert!(matches!(result, Err(RemoteError::Connect(_))), "{result:?}");
    Ok(())
}

#[tokio::test]
async fn host_that_refuses_the_upgrade_is_a_connect_error() -> TestResult {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    serve_with(server_io, |_| {
        upgrade::rejection_response(&upgrade::UpgradeRejection::OriginRefused)
    });
    let result = connect_io(client_io, ConnectOptions::new("t")).await;
    match result {
        Err(RemoteError::Connect(detail)) => {
            assert!(detail.contains("403"), "{detail}");
            Ok(())
        }
        other => Err(format!("expected a connect error, got {other:?}").into()),
    }
}

#[tokio::test]
async fn server_close_fails_streams_and_calls_with_a_typed_error() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (port, shutdown) =
        connect(open_host(dir.path(), 0, None)?, ConnectOptions::new("t")).await?;
    let created = port.create(CreateParams::default()).await?;
    let (_ack, mut frames) = port.attach(attach_params(&created.session_id)).await?;

    // The host drains: the connection is closed by the server.
    shutdown.send(true)?;

    // Frames queued before the close drain first; then the stream reports the
    // failure once, and is over afterwards.
    let mut failure = None;
    for _ in 0..64 {
        match tokio::time::timeout(WAIT, frames.next()).await? {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    assert!(
        matches!(failure, Some(PortError::Transport(_))),
        "{failure:?}"
    );
    assert!(tokio::time::timeout(WAIT, frames.next()).await??.is_none());

    let list = tokio::time::timeout(WAIT, port.list()).await?;
    assert!(matches!(list, Err(PortError::Transport(_))), "{list:?}");
    let create = tokio::time::timeout(WAIT, port.create(CreateParams::default())).await?;
    assert!(matches!(create, Err(PortError::Transport(_))), "{create:?}");
    Ok(())
}

#[tokio::test]
async fn slow_client_gets_a_lagged_marker_and_the_stream_ends() -> TestResult {
    let dir = tempfile::tempdir()?;
    // A tiny host queue and 400 unmergeable deltas, emitted without yielding.
    let queue = QueueLimits {
        max_frames: 4,
        ..QueueLimits::default()
    };
    let (port, _shutdown) = connect(
        open_host(dir.path(), 400, Some(queue))?,
        ConnectOptions::new("slow"),
    )
    .await?;
    let created = port.create(CreateParams::default()).await?;
    let (ack, mut frames) = port.attach(attach_params(&created.session_id)).await?;
    port.submit(submit_params(&created.session_id, "flood".into(), ack.head))
        .await?;

    let mut resume_from = None;
    for _ in 0..2048 {
        let frame = tokio::time::timeout(WAIT, frames.next())
            .await??
            .ok_or("stream ended before a Lagged marker")?;
        if let SessionFrame::Lagged { resume_from: at } = frame.frame {
            resume_from = Some(at);
            break;
        }
    }
    resume_from.ok_or("no Lagged marker arrived")?;
    // Lagged ends the attachment; the connection itself stays usable.
    assert!(tokio::time::timeout(WAIT, frames.next()).await??.is_none());
    assert_eq!(port.list().await?.len(), 1);
    Ok(())
}
