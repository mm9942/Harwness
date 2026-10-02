//! W07 and the tenant half of W06 across the real boundary: two clients on
//! separate connections race to answer one approval; the actor comes from the
//! transport, never from a frame; an observer cannot answer; and a remote
//! device of one tenant cannot reach a local session.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{TestResult, node, pinned};
use harw_node_listener::identity::{
    DeviceRecord, DeviceRegistry, IdentityMapper, RegistryIdentityMapper,
};
use harw_node_transport::{AuthenticatedPeer, NodeTransportServer, ServerOptions};
use harw_protocol::approvals::ApprovalKind;
use harw_protocol::session_wire::{
    ApprovalRespondParams, AttachParams, CreateParams, DEFAULT_TAIL_ITEMS, RespondResult,
    StreamProfile, SubmitParams, SubmitResult,
};
use harw_protocol::{
    ApprovalRequest, Cursor, FrameSource, PortError, SessionFrame, SessionPort, TurnEvent,
};
use harw_session_com::local::local_identity;
use harw_session_com::{ComConfig, ComRefusal, ComServer, HostBinder, PortOffer, RemoteLayer};
use harw_session_host::approvals::MemoryApprovals;
use harw_session_host::driver::{
    CancelSignal, DriverEvent, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::replay::MemoryTranscripts;
use harw_session_host::{HostConfig, SessionHost};
use harw_session_remote::{ConnectOptions, NodeEndpoint, RemotePort, connect_io, connect_node};
use harw_types::{
    ApprovalId, DeviceId, PermissionTier, ReviewDecision, RiskLevel, SessionId, TenantId, ThreadId,
    TurnId, WorkId,
};
use tokio::net::TcpListener;
use tokio::sync::watch;

const WAIT: Duration = Duration::from_secs(10);

/// Every turn issues one approval and parks on it.
struct ParkingDriver {
    approvals: Arc<MemoryApprovals>,
}

impl TurnDriver for ParkingDriver {
    fn create_session(&self, _: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn run_turn(
        &self,
        input: TurnInput,
        _: CancelSignal,
        sink: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        let approvals = Arc::clone(&self.approvals);
        Box::pin(async move {
            let turn_id = TurnId::new();
            sink.emit(DriverEvent::Turn(TurnEvent::TurnStarted {
                turn_id: turn_id.clone(),
                thread_id: ThreadId::new(),
            }));
            let now = jiff::Timestamp::now();
            let request = ApprovalRequest {
                id: ApprovalId::new(),
                work_id: WorkId::new(),
                kind: ApprovalKind::Exec {
                    turn_id,
                    command: vec!["true".into()],
                    cwd: "/".into(),
                    reasoning: None,
                },
                summary: "run true".into(),
                risk: RiskLevel::Low,
                requested_at: now,
                timeout_at: ApprovalRequest::default_timeout_at(now),
                decisions: vec![ReviewDecision::Approved, ReviewDecision::Rejected],
            };
            approvals.issue(&input.session_id, request.clone())?;
            sink.emit(DriverEvent::ApprovalRequested(request));
            Ok(TurnOutcome::AwaitingApproval)
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
    _shutdown: watch::Sender<bool>,
    dir: tempfile::TempDir,
}

fn rig() -> TestResult<Rig> {
    let dir = tempfile::tempdir()?;
    let approvals = Arc::new(MemoryApprovals::new());
    let host = SessionHost::open(
        HostConfig::new(dir.path().join("state")),
        Arc::new(ParkingDriver {
            approvals: Arc::clone(&approvals),
        }),
        Arc::new(MemoryTranscripts::new()),
        approvals,
    )?;
    let (shutdown, rx) = watch::channel(false);
    let com = Arc::new(ComServer::new(
        &ComConfig::default(),
        Arc::new(HostBinder::new(host, PortOffer::All)),
        rx,
    ));
    Ok(Rig {
        com,
        _shutdown: shutdown,
        dir,
    })
}

async fn local_port(rig: &Rig, uid: u32, tier: PermissionTier) -> TestResult<RemotePort> {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    rig.com.serve_io(server_io, local_identity(uid, tier))?;
    Ok(RemotePort::new(
        connect_io(client_io, ConnectOptions::new(format!("uid-{uid}"))).await?,
    ))
}

fn attach(session: &SessionId) -> AttachParams {
    AttachParams {
        session_id: session.clone(),
        from: None,
        profile: StreamProfile::Full,
        tail_items: DEFAULT_TAIL_ITEMS,
    }
}

async fn approval_frame(frames: &mut Box<dyn FrameSource>) -> TestResult<ApprovalRequest> {
    for _ in 0..64 {
        let frame = tokio::time::timeout(WAIT, frames.next())
            .await??
            .ok_or("frame stream ended")?;
        if let SessionFrame::ApprovalRequested(request) = frame.frame {
            return Ok(request);
        }
    }
    Err("approval frame not seen".into())
}

async fn resolved_by(frames: &mut Box<dyn FrameSource>) -> TestResult<String> {
    for _ in 0..64 {
        let frame = tokio::time::timeout(WAIT, frames.next())
            .await??
            .ok_or("frame stream ended")?;
        if let SessionFrame::ApprovalResolved { by, .. } = frame.frame {
            return Ok(by);
        }
    }
    Err("approval.resolved frame not seen".into())
}

fn respond(request: &ApprovalRequest) -> ApprovalRespondParams {
    ApprovalRespondParams {
        request_id: request.id.clone(),
        decision: ReviewDecision::Approved,
        reason: None,
    }
}

#[tokio::test]
async fn two_clients_race_for_one_approval_and_exactly_one_wins() -> TestResult {
    let rig = rig()?;
    let a = local_port(&rig, 1000, PermissionTier::Operator).await?;
    let b = local_port(&rig, 1001, PermissionTier::Operator).await?;
    let session = a
        .create(CreateParams {
            workspace: None,
            title: Some("race".into()),
        })
        .await?
        .session_id;
    let (ack, mut frames_a) = a.attach(attach(&session)).await?;
    let (_, mut frames_b) = b.attach(attach(&session)).await?;

    let submitted = a
        .submit(SubmitParams {
            session_id: session.clone(),
            text: "do it".into(),
            expect_head: ack.head,
            client_msg_id: "m1".into(),
            force: false,
        })
        .await?;
    assert!(
        matches!(submitted, SubmitResult::Accepted { .. }),
        "{submitted:?}"
    );

    let request = approval_frame(&mut frames_a).await?;
    let same = approval_frame(&mut frames_b).await?;
    assert_eq!(request.id, same.id, "both clients see the same request");

    // Both answer at once, from two different connections.
    let (ra, rb) = tokio::join!(a.respond(respond(&request)), b.respond(respond(&request)));
    let (ra, rb) = (ra?, rb?);
    let resolved = [&ra, &rb]
        .iter()
        .filter(|r| matches!(r, RespondResult::Resolved))
        .count();
    assert_eq!(resolved, 1, "exactly one writer wins: {ra:?} / {rb:?}");
    let loser_saw = [&ra, &rb]
        .iter()
        .find_map(|r| match r {
            RespondResult::AlreadyResolved { by } => Some(by.clone()),
            _ => None,
        })
        .ok_or("the loser must be told the request was already resolved")?;

    // The actor is the one the transport authenticated, in both views.
    let by_a = resolved_by(&mut frames_a).await?;
    let by_b = resolved_by(&mut frames_b).await?;
    assert_eq!(by_a, by_b);
    assert_eq!(loser_saw, by_a, "the loser is told who won");
    assert!(
        by_a == "operator:uid:1000" || by_a == "operator:uid:1001",
        "actor derived from the peer credentials: {by_a}"
    );
    Ok(())
}

#[tokio::test]
async fn an_observer_can_watch_but_not_answer_or_submit() -> TestResult {
    let rig = rig()?;
    let operator = local_port(&rig, 1000, PermissionTier::Operator).await?;
    let observer = local_port(&rig, 1002, PermissionTier::Observer).await?;
    let session = operator.create(CreateParams::default()).await?.session_id;
    let (ack, mut op_frames) = operator.attach(attach(&session)).await?;
    let (_, mut obs_frames) = observer.attach(attach(&session)).await?;

    let denied = observer
        .submit(SubmitParams {
            session_id: session.clone(),
            text: "x".into(),
            expect_head: Cursor::default(),
            client_msg_id: "o1".into(),
            force: false,
        })
        .await;
    assert!(matches!(denied, Err(PortError::Denied(_))), "{denied:?}");

    operator
        .submit(SubmitParams {
            session_id: session.clone(),
            text: "run".into(),
            expect_head: ack.head,
            client_msg_id: "a1".into(),
            force: false,
        })
        .await?;
    let request = approval_frame(&mut op_frames).await?;
    let seen = approval_frame(&mut obs_frames).await?;
    assert_eq!(request.id, seen.id, "an observer sees the request");
    let answer = observer.respond(respond(&request)).await;
    assert!(matches!(answer, Err(PortError::Denied(_))), "{answer:?}");
    // The request is still open: the operator can answer it.
    assert!(matches!(
        operator.respond(respond(&request)).await?,
        RespondResult::Resolved
    ));
    Ok(())
}

#[tokio::test]
async fn a_remote_device_of_another_tenant_cannot_reach_a_local_session() -> TestResult {
    let rig = rig()?;
    let local = local_port(&rig, 1000, PermissionTier::Owner).await?;
    let session = local.create(CreateParams::default()).await?.session_id;

    let phone = node("phone", 2)?;
    let gateway = node("gateway", 1)?;
    DeviceRegistry::new(rig.dir.path()).enroll(&DeviceRecord {
        node_id: phone.id.clone(),
        device: DeviceId::try_from_str("dev-1")?,
        tenant: TenantId::try_from_str("tenant-a")?,
        tier: PermissionTier::Operator,
        revoked: false,
        label: "phone".to_owned(),
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
        Arc::new(pinned(&[&phone.identity])?),
        ServerOptions::default(),
    )?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        server
            .serve_upgradable(listener, service, std::future::pending())
            .await
    });
    let verifier = pinned(&[&gateway.identity])?;
    let remote = RemotePort::new(
        connect_node(
            NodeEndpoint {
                addr,
                expected_node: gateway.id.clone(),
            },
            &phone.local,
            &verifier,
            ConnectOptions::new("phone"),
        )
        .await?,
    );

    // The remote device is tenant-scoped: it neither lists nor attaches the
    // local session, and the host does not reveal whether the id exists.
    assert!(remote.list().await?.is_empty());
    let attached = remote.attach(attach(&session)).await;
    assert!(
        matches!(attached, Err(PortError::NotFound | PortError::Denied(_))),
        "cross-tenant attach: {:?}",
        attached.err()
    );
    // It can still create and use its own session.
    let own = remote.create(CreateParams::default()).await?;
    assert!(remote.attach(attach(&own.session_id)).await.is_ok());
    Ok(())
}
