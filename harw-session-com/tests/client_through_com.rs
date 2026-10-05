//! The session client (`harw-session-remote`) through the Com layer, over all
//! three ingresses: an in-memory stream, a real Unix socket (`serve_unix`) and
//! the real node transport. The same server stack, the same client: this is
//! the conformance test for W07/W08 behaviour that matters across transports
//! (turn frames, idempotent submit across a reconnect, bounded drain).

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::{TestResult, node, pinned};
use harw_node_listener::identity::{
    DeviceRecord, DeviceRegistry, IdentityMapper, RegistryIdentityMapper,
};
use harw_node_transport::{AuthenticatedPeer, NodeTransportServer, ServerOptions};
use harw_protocol::session_wire::{
    AttachParams, CreateParams, DEFAULT_TAIL_ITEMS, StreamProfile, SubmitParams, SubmitResult,
};
use harw_protocol::{FrameSource, PortError, SessionFrame, SessionPort, TurnEvent};
use harw_session_com::local::{local_identity, serve_unix};
use harw_session_com::{ComConfig, ComRefusal, ComServer, HostBinder, PortOffer, RemoteLayer};
use harw_session_host::approvals::MemoryApprovals;
use harw_session_host::driver::{
    CancelSignal, DriverEvent, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::replay::MemoryTranscripts;
use harw_session_host::{HostConfig, SessionHost};
use harw_session_remote::{
    ConnectOptions, NodeEndpoint, RemotePort, connect_io, connect_node, connect_unix,
};
use harw_types::{DeviceId, PermissionTier, SessionId, TenantId, ThreadId, TurnId};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::watch;

const WAIT: Duration = Duration::from_secs(10);

/// Emits `TurnStarted` and one delta `<text>#0` per turn; counts the turns.
struct CountingDriver {
    turns: Arc<AtomicUsize>,
}

impl TurnDriver for CountingDriver {
    fn create_session(&self, _: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn run_turn(
        &self,
        input: TurnInput,
        _: CancelSignal,
        sink: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        self.turns.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let turn_id = TurnId::new();
            sink.emit(DriverEvent::Turn(TurnEvent::TurnStarted {
                turn_id: turn_id.clone(),
                thread_id: ThreadId::new(),
            }));
            sink.emit(DriverEvent::Turn(TurnEvent::AssistantDelta {
                turn_id,
                text: format!("{}#0", input.text),
            }));
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

struct Rig {
    com: Arc<ComServer>,
    host: SessionHost,
    shutdown: watch::Sender<bool>,
    stopped: watch::Receiver<bool>,
    turns: Arc<AtomicUsize>,
    dir: tempfile::TempDir,
}

fn rig(grace: Duration) -> TestResult<Rig> {
    let dir = tempfile::tempdir()?;
    let turns = Arc::new(AtomicUsize::new(0));
    let host = SessionHost::open(
        HostConfig::new(dir.path().join("state")),
        Arc::new(CountingDriver {
            turns: Arc::clone(&turns),
        }),
        Arc::new(MemoryTranscripts::new()),
        Arc::new(MemoryApprovals::new()),
    )?;
    let (shutdown, stopped) = watch::channel(false);
    let config = ComConfig {
        shutdown_grace: grace,
        ..ComConfig::default()
    };
    let com = Arc::new(ComServer::new(
        &config,
        Arc::new(HostBinder::new(host.clone(), PortOffer::All)),
        stopped.clone(),
    ));
    Ok(Rig {
        com,
        host,
        shutdown,
        stopped,
        turns,
        dir,
    })
}

fn titled(title: &str) -> CreateParams {
    CreateParams {
        workspace: None,
        title: Some(title.to_owned()),
    }
}

fn attach_params(session: &SessionId) -> AttachParams {
    AttachParams {
        session_id: session.clone(),
        from: None,
        profile: StreamProfile::Full,
        tail_items: DEFAULT_TAIL_ITEMS,
    }
}

async fn wait_delta(frames: &mut Box<dyn FrameSource>, expected: &str) -> TestResult {
    for _ in 0..256 {
        let frame = tokio::time::timeout(WAIT, frames.next())
            .await??
            .ok_or("frame stream ended")?;
        if let SessionFrame::Turn(TurnEvent::AssistantDelta { text, .. }) = &frame.frame {
            if text == expected {
                return Ok(());
            }
        }
    }
    Err(format!("delta {expected:?} never arrived").into())
}

/// The scenario every ingress must satisfy.
async fn turn_round_trip(port: &RemotePort, title: &str) -> TestResult {
    let created = port.create(titled(title)).await?;
    let (ack, mut frames) = port.attach(attach_params(&created.session_id)).await?;
    let submitted = port
        .submit(SubmitParams {
            session_id: created.session_id.clone(),
            text: "hi".into(),
            expect_head: ack.head,
            client_msg_id: format!("m-{title}"),
            force: false,
        })
        .await?;
    assert!(
        matches!(submitted, SubmitResult::Accepted { .. }),
        "{submitted:?}"
    );
    wait_delta(&mut frames, "hi#0").await
}

#[tokio::test]
async fn a_turn_round_trips_over_an_in_memory_stream() -> TestResult {
    let rig = rig(Duration::from_secs(5))?;
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    rig.com
        .serve_io(server_io, local_identity(1000, PermissionTier::Operator))?;
    let port = RemotePort::new(connect_io(client_io, ConnectOptions::new("mem")).await?);
    turn_round_trip(&port, "mem").await
}

#[tokio::test]
async fn a_turn_round_trips_over_a_real_unix_socket() -> TestResult {
    let rig = rig(Duration::from_secs(5))?;
    let uid = std::fs::metadata(rig.dir.path())?;
    let uid = std::os::unix::fs::MetadataExt::uid(&uid);
    let path = rig.dir.path().join("com.sock");
    let listener = UnixListener::bind(&path)?;
    let com = Arc::clone(&rig.com);
    let stopped = rig.stopped.clone();
    tokio::spawn(async move {
        serve_unix(&com, listener, uid, PermissionTier::Operator, stopped).await;
    });
    let port = RemotePort::new(connect_unix(path, ConnectOptions::new("uds")).await?);
    turn_round_trip(&port, "uds").await
}

async fn node_rig(
    rig: &Rig,
    trusted_client: &common::Node,
) -> TestResult<(NodeEndpoint, common::Node)> {
    let gateway = node("gateway", 1)?;
    let registry = DeviceRegistry::new(rig.dir.path());
    registry.enroll(&DeviceRecord {
        node_id: trusted_client.id.clone(),
        device: DeviceId::try_from_str("dev-1")?,
        tenant: TenantId::try_from_str("tenant-a")?,
        tier: PermissionTier::Operator,
        revoked: false,
        label: "phone".to_owned(),
        approve_optin: true,
    })?;
    let mapper = Arc::new(RegistryIdentityMapper::new(rig.dir.path()));
    let service = rig.com.service_for(RemoteLayer::<AuthenticatedPeer>::new(
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
    let server = NodeTransportServer::new(
        gateway.local.clone(),
        Arc::new(pinned(&[&trusted_client.identity])?),
        ServerOptions::default(),
    )?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        server
            .serve_upgradable(listener, service, std::future::pending())
            .await
    });
    Ok((
        NodeEndpoint {
            addr,
            expected_node: gateway.id.clone(),
        },
        gateway,
    ))
}

#[tokio::test]
async fn a_turn_round_trips_over_the_real_node_transport() -> TestResult {
    let rig = rig(Duration::from_secs(5))?;
    let phone = node("phone", 2)?;
    let (endpoint, gateway) = node_rig(&rig, &phone).await?;
    let verifier = pinned(&[&gateway.identity])?;
    let connection = connect_node(
        endpoint,
        &phone.local,
        &verifier,
        ConnectOptions::new("node"),
    )
    .await?;
    let port = RemotePort::new(connection);
    turn_round_trip(&port, "node").await
}

#[tokio::test]
async fn a_resubmitted_message_after_a_reconnect_does_not_run_twice() -> TestResult {
    let rig = rig(Duration::from_secs(5))?;
    let identity = || local_identity(1000, PermissionTier::Operator);

    let (io1, srv1) = tokio::io::duplex(64 * 1024);
    rig.com.serve_io(srv1, identity())?;
    let first = RemotePort::new(connect_io(io1, ConnectOptions::new("one")).await?);
    let created = first.create(titled("resume")).await?;
    let (ack, mut frames) = first.attach(attach_params(&created.session_id)).await?;
    let submit = |head, id: &str| SubmitParams {
        session_id: created.session_id.clone(),
        text: "once".into(),
        expect_head: head,
        client_msg_id: id.to_owned(),
        force: false,
    };
    let accepted = first.submit(submit(ack.head, "same-id")).await?;
    assert!(
        matches!(accepted, SubmitResult::Accepted { .. }),
        "{accepted:?}"
    );
    wait_delta(&mut frames, "once#0").await?;
    drop(frames);
    drop(first); // the connection ends; the session and the turn stay

    let (io2, srv2) = tokio::io::duplex(64 * 1024);
    rig.com.serve_io(srv2, identity())?;
    let second = RemotePort::new(connect_io(io2, ConnectOptions::new("two")).await?);
    let (ack2, _frames) = second.attach(attach_params(&created.session_id)).await?;
    // The client does not know whether its message arrived, so it resends it.
    let again = second.submit(submit(ack2.head, "same-id")).await?;
    assert!(!matches!(again, SubmitResult::Denied { .. }), "{again:?}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        rig.turns.load(Ordering::SeqCst),
        1,
        "the same client_msg_id must not start a second turn"
    );
    Ok(())
}

#[tokio::test]
async fn a_draining_gateway_ends_streams_with_a_typed_error_and_drain_reports_none_left()
-> TestResult {
    let rig = rig(Duration::from_secs(5))?;
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    rig.com
        .serve_io(server_io, local_identity(1000, PermissionTier::Operator))?;
    let port = RemotePort::new(connect_io(client_io, ConnectOptions::new("drain")).await?);
    let created = port.create(titled("drain")).await?;
    let (_ack, mut frames) = port.attach(attach_params(&created.session_id)).await?;

    rig.shutdown.send(true)?;
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
    assert_eq!(
        rig.com.drain().await,
        0,
        "nothing left after the grace period"
    );
    let create = tokio::time::timeout(WAIT, port.create(CreateParams::default())).await?;
    assert!(matches!(create, Err(PortError::Transport(_))), "{create:?}");
    // A new connection to a draining host is refused with an HTTP status.
    rig.host.drain(1000);
    let (c2, s2) = tokio::io::duplex(64 * 1024);
    rig.com
        .serve_io(s2, local_identity(1000, PermissionTier::Operator))?;
    let refused = connect_io(c2, ConnectOptions::new("late")).await;
    assert!(refused.is_err(), "no new session while draining");
    Ok(())
}
