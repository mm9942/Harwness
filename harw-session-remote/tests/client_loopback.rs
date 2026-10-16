//! Client core against the real server stack: `harw-session-remote` →
//! HTTP/1 upgrade (`harw.session.v1`) → `harw-session-ws` → `SessionHost`
//! (memory transcripts and approvals) → a scripted turn driver. The transport
//! is an in-memory duplex (`connect_io`) or a real Unix socket
//! (`connect_unix`). No test panics: every failure is an `Err`.

use std::sync::Arc;
use std::time::Duration;

use harw_protocol::session_wire::{
    ApprovalRespondParams, AttachParams, CreateParams, DEFAULT_TAIL_ITEMS, HelloParams,
    HistoryParams, InterruptParams, SetEffortParams, SetModeParams, SetModelParams, StreamProfile,
    SubmitParams, SubmitResult,
};
use harw_protocol::{
    ClientCaps, FrameEnvelope, FrameSource, PortError, SessionFrame, SessionPort, TurnEvent,
};
use harw_session_host::approvals::MemoryApprovals;
use harw_session_host::driver::{
    CancelSignal, DriverEvent, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::replay::MemoryTranscripts;
use harw_session_host::{ClientIdentity, ConnectionId, HostConfig, SessionHost, caps_for_tier};
use harw_session_remote::{ConnectOptions, RemotePort, connect_io, connect_unix};
use harw_session_ws::{WsLimits, serve_connection, upgrade};
use harw_types::{
    ApprovalActor, ApprovalId, AuthStrength, IngressSurface, PermissionTier, Principal,
    PrincipalKind, ReviewDecision, SessionId, ThreadId, TrustZone, TurnId,
};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::watch;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const WAIT: Duration = Duration::from_secs(10);

/// Emits `TurnStarted` and then `deltas` assistant deltas whose text is
/// `<submitted text>#<index>`.
struct ScriptDriver {
    deltas: usize,
}

impl TurnDriver for ScriptDriver {
    fn create_session(&self, _: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn run_turn(
        &self,
        input: TurnInput,
        _: CancelSignal,
        sink: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        let deltas = self.deltas;
        Box::pin(async move {
            let turn_id = TurnId::new();
            sink.emit(DriverEvent::Turn(TurnEvent::TurnStarted {
                turn_id: turn_id.clone(),
                thread_id: ThreadId::new(),
            }));
            for index in 0..deltas {
                sink.emit(DriverEvent::Turn(TurnEvent::AssistantDelta {
                    turn_id: turn_id.clone(),
                    text: format!("{}#{index}", input.text),
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
        label: "loopback".into(),
        zone: TrustZone::Local,
        strength: AuthStrength::PeerCredential,
        connection: ConnectionId::next(),
        agent: None,
    }
}

fn open_host(dir: &std::path::Path, deltas: usize) -> TestResult<SessionHost> {
    Ok(SessionHost::open(
        HostConfig::new(dir.to_path_buf()),
        Arc::new(ScriptDriver { deltas }),
        Arc::new(MemoryTranscripts::new()),
        Arc::new(MemoryApprovals::new()),
    )?)
}

/// Serve one HTTP/1 connection whose only route upgrades into the session
/// control plane for a fixed, already-authenticated identity.
fn serve<S>(io: S, host: SessionHost, shutdown: watch::Receiver<bool>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
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

/// A client port over an in-memory duplex to a fresh host.
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

fn titled(title: &str) -> CreateParams {
    CreateParams {
        workspace: None,
        title: Some(title.to_owned()),
    }
}

async fn next_frame(frames: &mut Box<dyn FrameSource>) -> TestResult<FrameEnvelope> {
    let frame = tokio::time::timeout(WAIT, frames.next()).await??;
    Ok(frame.ok_or("frame stream ended")?)
}

/// Read frames of `session` until the assistant delta `expected` shows up.
/// Any frame of another session on this stream is an error.
async fn wait_delta(
    frames: &mut Box<dyn FrameSource>,
    session: &SessionId,
    expected: &str,
) -> TestResult<FrameEnvelope> {
    for _ in 0..512 {
        let frame = next_frame(frames).await?;
        if &frame.session_id != session {
            return Err(format!(
                "frame of session {} on the stream of {}",
                frame.session_id.as_str(),
                session.as_str()
            )
            .into());
        }
        if let SessionFrame::Turn(TurnEvent::AssistantDelta { text, .. }) = &frame.frame
            && text == expected
        {
            return Ok(frame);
        }
    }
    Err(format!("delta {expected:?} never arrived").into())
}

/// The call reached the host and was answered (success or a host refusal),
/// i.e. it is neither a transport failure nor a client wire error.
fn answered<T>(result: &Result<T, PortError>) -> bool {
    !matches!(
        result,
        Err(PortError::Transport(_) | PortError::Protocol(_))
    )
}

/// One of the overlapping callers: its response must be its own.
async fn create_then_set_model(port: Arc<RemotePort>, index: usize) -> TestResult {
    let title = format!("t{index}");
    let created = port.create(titled(&title)).await?;
    if created.title.as_deref() != Some(title.as_str()) {
        return Err(format!(
            "response mixed up: asked for {title:?}, got {:?}",
            created.title
        )
        .into());
    }
    // A second, dependent call on the new session.
    let model = port
        .set_model(SetModelParams {
            session_id: created.session_id,
            model: format!("m{index}"),
        })
        .await;
    if answered(&model) {
        Ok(())
    } else {
        Err(format!("set_model: {model:?}").into())
    }
}

#[tokio::test]
async fn hello_reports_granted_caps_and_epoch() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (port, _shutdown) = connect(open_host(dir.path(), 0)?, ConnectOptions::new("t")).await?;
    let ack = port.connection().hello_ack().clone();
    assert!(ack.granted.observe);
    assert!(ack.wire_minor >= 1);
    // `hello` on the port is the acknowledgement of the connect-time hello.
    let again = port
        .hello(HelloParams {
            client_label: "ignored".into(),
            wire_minor: 1,
            features: Vec::new(),
            requested_caps: None,
        })
        .await?;
    assert_eq!(again, ack);
    Ok(())
}

#[tokio::test]
async fn requested_caps_can_only_narrow() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut options = ConnectOptions::new("observer");
    options.requested_caps = Some(ClientCaps::OBSERVE);
    let (port, _shutdown) = connect(open_host(dir.path(), 0)?, options).await?;
    let granted = port.connection().hello_ack().granted;
    assert!(granted.observe);
    assert!(!granted.steer);
    assert!(!granted.control);
    Ok(())
}

#[tokio::test]
async fn unix_socket_connect_hello_create_list() -> TestResult {
    let dir = tempfile::tempdir()?;
    let host = open_host(dir.path(), 0)?;
    let path = dir.path().join("s.sock");
    let listener = tokio::net::UnixListener::bind(&path)?;
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            serve(stream, host.clone(), shutdown_rx.clone());
        }
    });

    let connection = connect_unix(path, ConnectOptions::new("uds")).await?;
    let port = RemotePort::new(connection);
    assert!(port.list().await?.is_empty());
    let created = port.create(titled("over uds")).await?;
    assert_eq!(created.title.as_deref(), Some("over uds"));
    let listed = port.list().await?;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].session_id, created.session_id);
    Ok(())
}

#[tokio::test]
async fn attach_submit_receives_turn_events_then_detach_ends_the_stream() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (port, _shutdown) = connect(open_host(dir.path(), 1)?, ConnectOptions::new("t")).await?;
    let created = port.create(titled("turns")).await?;
    let session = created.session_id.clone();

    let (ack, mut frames) = port.attach(attach_params(&session)).await?;
    assert_eq!(ack.session.session_id, session);

    let submitted = port
        .submit(SubmitParams {
            session_id: session.clone(),
            text: "hi".into(),
            expect_head: ack.head,
            client_msg_id: "m1".into(),
            force: false,
        })
        .await?;
    assert!(
        matches!(submitted, SubmitResult::Accepted { .. }),
        "{submitted:?}"
    );
    wait_delta(&mut frames, &session, "hi#0").await?;

    port.detach(session.clone()).await?;
    // Frames queued before the detach drain first; then the stream ends.
    for _ in 0..256 {
        if tokio::time::timeout(WAIT, frames.next()).await??.is_none() {
            return Ok(());
        }
    }
    Err("stream did not end after detach".into())
}

#[tokio::test]
async fn many_sessions_share_one_connection() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (port, _shutdown) = connect(open_host(dir.path(), 1)?, ConnectOptions::new("t")).await?;

    let mut attached = Vec::new();
    for name in ["a", "b", "c"] {
        let created = port.create(titled(name)).await?;
        let (ack, frames) = port.attach(attach_params(&created.session_id)).await?;
        attached.push((name, created.session_id, ack.head, frames));
    }
    for (name, session, head, _) in &attached {
        let result = port
            .submit(SubmitParams {
                session_id: session.clone(),
                text: (*name).to_owned(),
                expect_head: *head,
                client_msg_id: format!("m-{name}"),
                force: false,
            })
            .await?;
        assert!(
            matches!(result, SubmitResult::Accepted { .. }),
            "{result:?}"
        );
    }
    for (name, session, _, frames) in &mut attached {
        wait_delta(frames, session, &format!("{name}#0")).await?;
    }
    assert_eq!(port.list().await?.len(), 3);
    Ok(())
}

#[tokio::test]
async fn responses_correlate_under_interleaving() -> TestResult {
    let dir = tempfile::tempdir()?;
    // Every turn emits 300 deltas: a busy stream runs while requests overlap.
    let (port, _shutdown) = connect(open_host(dir.path(), 300)?, ConnectOptions::new("t")).await?;
    let busy = port.create(titled("busy")).await?;
    let busy_session = busy.session_id.clone();
    let (ack, mut frames) = port.attach(attach_params(&busy_session)).await?;

    let reader = tokio::spawn(async move {
        wait_delta(&mut frames, &busy_session, "flood#299")
            .await
            .map(|_| ())
    });
    port.submit(SubmitParams {
        session_id: busy.session_id.clone(),
        text: "flood".into(),
        expect_head: ack.head,
        client_msg_id: "flood".into(),
        force: false,
    })
    .await?;

    let mut tasks = Vec::new();
    for index in 0..16 {
        tasks.push(tokio::spawn(create_then_set_model(
            Arc::clone(&port),
            index,
        )));
    }
    for task in tasks {
        let outcome: TestResult = tokio::time::timeout(WAIT, task).await??;
        outcome?;
    }
    tokio::time::timeout(WAIT, reader).await???;
    assert_eq!(port.list().await?.len(), 17);
    Ok(())
}

#[tokio::test]
async fn every_port_method_reaches_the_host() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (port, _shutdown) = connect(open_host(dir.path(), 1)?, ConnectOptions::new("t")).await?;
    let created = port.create(titled("all")).await?;
    let session = created.session_id.clone();
    let (ack, _frames) = port.attach(attach_params(&session)).await?;

    let history = port
        .history(HistoryParams {
            session_id: session.clone(),
            before: ack.head,
            limit: 10,
        })
        .await;
    assert!(answered(&history), "{history:?}");
    let interrupt = port
        .interrupt(InterruptParams {
            session_id: session.clone(),
            turn_id: None,
        })
        .await;
    assert!(answered(&interrupt), "{interrupt:?}");
    let model = port
        .set_model(SetModelParams {
            session_id: session.clone(),
            model: "m".into(),
        })
        .await;
    assert!(answered(&model), "{model:?}");
    let mode = port
        .set_mode(SetModeParams {
            session_id: session.clone(),
            mode: "plan".into(),
        })
        .await;
    assert!(answered(&mode), "{mode:?}");
    let effort = port
        .set_effort(SetEffortParams {
            session_id: session.clone(),
            effort: "high".into(),
        })
        .await;
    assert!(answered(&effort), "{effort:?}");
    let respond = port
        .respond(ApprovalRespondParams {
            request_id: ApprovalId::try_from_str("missing").map_err(|e| e.to_string())?,
            decision: ReviewDecision::Approved,
            reason: None,
        })
        .await;
    assert!(answered(&respond), "{respond:?}");
    let resume = port.resume(session.clone()).await;
    assert!(answered(&resume), "{resume:?}");
    let close = port.close(session.clone()).await;
    assert!(answered(&close), "{close:?}");

    // Host refusals are typed, not transport errors.
    let unknown = SessionId::try_from_str("no-such-session").map_err(|e| e.to_string())?;
    let refused = port
        .set_model(SetModelParams {
            session_id: unknown,
            model: "m".into(),
        })
        .await;
    assert!(
        matches!(refused, Err(PortError::NotFound | PortError::Denied(_))),
        "{refused:?}"
    );
    Ok(())
}
