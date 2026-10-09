//! Host integration tests against the W00 test matrix (§9): identity,
//! tenancy, approvals, arbitration, replay, restart and revocation.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use harw_protocol::approvals::ApprovalKind;
use harw_protocol::items::{ContentPart, TurnItem, UserMessageItem};
use harw_protocol::session_wire::{
    ApprovalRespondParams, AttachParams, CreateParams, HelloParams, InterruptParams, RespondResult,
    SubmitParams, SubmitResult,
};
use harw_protocol::{
    ApprovalRequest, ClientCaps, Cursor, FrameEnvelope, FrameSource, HostedState, PortError,
    SessionFrame, SessionPort, StreamProfile, TurnEvent,
};
use harw_session_store::{RecordKind, TranscriptRecord};
use harw_types::{
    ApprovalId, DeviceId, ItemId, PermissionTier, ReviewDecision, RiskLevel, SessionId, TenantId,
    ThreadId, ThreadRef, TurnId, WorkId,
};
use tokio::sync::Notify;

use super::{HostConfig, HostConnection, SessionHost};
use crate::approvals::MemoryApprovals;
use crate::driver::{
    CancelSignal, DriverEvent, DriverFuture, EventSink, FailureCause, Setting, TurnDriver,
    TurnInput, TurnOutcome,
};
use crate::error::HostError;
use crate::identity::ClientIdentity;
use crate::identity::test_identity::{local, scoped};
use crate::replay::MemoryTranscripts;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Scripted runtime: every turn appends one durable user item, streams one
/// delta and completes; optionally waits on a gate or parks on an approval.
struct FakeDriver {
    transcripts: Arc<MemoryTranscripts>,
    approvals: Arc<MemoryApprovals>,
    turns: AtomicUsize,
    gate: Option<Arc<Notify>>,
    park: bool,
    settings: std::sync::Mutex<Vec<Setting>>,
    /// Results returned (in order) instead of running a normal turn.
    failures: std::sync::Mutex<std::collections::VecDeque<Result<TurnOutcome, HostError>>>,
}

impl FakeDriver {
    fn new(transcripts: Arc<MemoryTranscripts>, approvals: Arc<MemoryApprovals>) -> Self {
        Self {
            transcripts,
            approvals,
            turns: AtomicUsize::new(0),
            gate: None,
            park: false,
            settings: std::sync::Mutex::new(Vec::new()),
            failures: std::sync::Mutex::new(std::collections::VecDeque::new()),
        }
    }

    fn script_failure(&self, result: Result<TurnOutcome, HostError>) {
        if let Ok(mut failures) = self.failures.lock() {
            failures.push_back(result);
        }
    }

    fn append_item(&self, session: &SessionId, text: &str) {
        let head = self.transcripts.head(session).unwrap_or(0);
        let item = TurnItem::UserMessage(UserMessageItem {
            id: ItemId::new(),
            content: vec![ContentPart::Text { text: text.into() }],
        });
        let payload = serde_json::to_value(item).unwrap_or(serde_json::Value::Null);
        self.transcripts.append(TranscriptRecord::new(
            session.clone(),
            ThreadRef::from_str("root"),
            head,
            jiff::Timestamp::now(),
            RecordKind::Item,
            payload,
        ));
    }
}

use crate::replay::TranscriptSource as _;

impl TurnDriver for FakeDriver {
    fn create_session(&self, _: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn run_turn(
        &self,
        input: TurnInput,
        mut cancel: CancelSignal,
        sink: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        Box::pin(async move {
            self.turns.fetch_add(1, Ordering::SeqCst);
            let scripted = self.failures.lock().ok().and_then(|mut f| f.pop_front());
            if let Some(result) = scripted {
                return result;
            }
            let turn_id = TurnId::new();
            sink.emit(DriverEvent::Turn(TurnEvent::TurnStarted {
                turn_id: turn_id.clone(),
                thread_id: ThreadId::new(),
            }));
            self.append_item(&input.session_id, &input.text);
            sink.emit(DriverEvent::DurableAdvanced);
            sink.emit(DriverEvent::Turn(TurnEvent::AssistantDelta {
                turn_id: turn_id.clone(),
                text: format!("echo {}", input.text),
            }));
            if let Some(gate) = &self.gate {
                tokio::select! {
                    () = gate.notified() => {}
                    _ = cancel.changed() => return Ok(TurnOutcome::Interrupted),
                }
            }
            if self.park {
                let now = jiff::Timestamp::now();
                let request = ApprovalRequest {
                    id: ApprovalId::new(),
                    work_id: WorkId::new(),
                    kind: ApprovalKind::Exec {
                        turn_id: turn_id.clone(),
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
                self.approvals.issue(&input.session_id, request.clone())?;
                sink.emit(DriverEvent::ApprovalRequested(request));
                return Ok(TurnOutcome::AwaitingApproval);
            }
            sink.emit(DriverEvent::Turn(TurnEvent::TurnAborted { turn_id }));
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

    fn apply_setting(&self, _: &SessionId, setting: Setting) -> DriverFuture<'_, ()> {
        if let Ok(mut settings) = self.settings.lock() {
            settings.push(setting);
        }
        Box::pin(async { Ok(()) })
    }

    fn model_name(&self, _: &SessionId) -> Option<String> {
        Some("fake-model".into())
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    host: SessionHost,
    driver: Arc<FakeDriver>,
}

fn fixture_with(configure: impl FnOnce(&mut FakeDriver)) -> TestResult<Fixture> {
    let dir = tempfile::tempdir()?;
    let transcripts = Arc::new(MemoryTranscripts::new());
    let approvals = Arc::new(MemoryApprovals::new());
    let mut driver = FakeDriver::new(Arc::clone(&transcripts), Arc::clone(&approvals));
    configure(&mut driver);
    let driver = Arc::new(driver);
    let host = SessionHost::open(
        HostConfig::new(dir.path().to_path_buf()),
        Arc::clone(&driver) as Arc<dyn TurnDriver>,
        transcripts,
        approvals,
    )?;
    Ok(Fixture {
        _dir: dir,
        host,
        driver,
    })
}

fn fixture() -> TestResult<Fixture> {
    fixture_with(|_| {})
}

async fn hello(connection: &HostConnection) -> TestResult<ClientCaps> {
    let ack = connection
        .hello(HelloParams {
            client_label: "test".into(),
            wire_minor: 1,
            features: vec!["compact".into(), "bogus".into()],
            requested_caps: None,
        })
        .await?;
    assert_eq!(ack.features, vec!["compact".to_owned()]);
    Ok(ack.granted)
}

async fn connect(host: &SessionHost, identity: ClientIdentity) -> TestResult<HostConnection> {
    let connection = host.connect(identity)?;
    hello(&connection).await?;
    Ok(connection)
}

fn submit(session: &SessionId, text: &str, msg: &str, head: Cursor) -> SubmitParams {
    SubmitParams {
        session_id: session.clone(),
        text: text.into(),
        expect_head: head,
        client_msg_id: msg.into(),
        force: false,
    }
}

fn attach(session: &SessionId, from: Option<Cursor>) -> AttachParams {
    AttachParams {
        session_id: session.clone(),
        from,
        profile: StreamProfile::Full,
        tail_items: 200,
    }
}

async fn next_frame(source: &mut Box<dyn FrameSource>) -> TestResult<FrameEnvelope> {
    let frame = tokio::time::timeout(Duration::from_secs(5), source.next())
        .await??
        .ok_or("stream ended")?;
    Ok(frame)
}

/// Read frames until `pick` matches (bounded).
async fn wait_for(
    source: &mut Box<dyn FrameSource>,
    pick: impl Fn(&SessionFrame) -> bool,
) -> TestResult<FrameEnvelope> {
    for _ in 0..64 {
        let frame = next_frame(source).await?;
        if pick(&frame.frame) {
            return Ok(frame);
        }
    }
    Err("frame not seen".into())
}

async fn wait_idle(connection: &HostConnection, session: &SessionId) -> TestResult {
    for _ in 0..200 {
        let list = connection.list().await?;
        if list
            .iter()
            .any(|s| &s.session_id == session && s.state == HostedState::Idle)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    Err("session never became idle".into())
}

#[tokio::test]
async fn calls_before_hello_are_denied() -> TestResult {
    let fx = fixture()?;
    let connection = fx.host.connect(local(1, PermissionTier::Owner))?;
    assert!(matches!(connection.list().await, Err(PortError::Denied(_))));
    assert!(matches!(
        connection.create(CreateParams::default()).await,
        Err(PortError::Denied(_))
    ));
    hello(&connection).await?;
    assert!(connection.list().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn requested_caps_only_narrow() -> TestResult {
    // ID-04
    let fx = fixture()?;
    let observer = fx.host.connect(local(1, PermissionTier::Observer))?;
    let ack = observer
        .hello(HelloParams {
            client_label: "phone".into(),
            wire_minor: 9,
            features: vec![],
            requested_caps: Some(ClientCaps::ALL),
        })
        .await?;
    assert_eq!(ack.granted, ClientCaps::OBSERVE);
    assert_eq!(
        ack.wire_minor,
        harw_protocol::session_wire::SESSION_WIRE_MINOR
    );

    let owner = fx.host.connect(local(2, PermissionTier::Owner))?;
    let ack = owner
        .hello(HelloParams {
            client_label: "watch".into(),
            wire_minor: 1,
            features: vec![],
            requested_caps: Some(ClientCaps::OBSERVE),
        })
        .await?;
    assert_eq!(ack.granted, ClientCaps::OBSERVE);
    assert!(matches!(
        owner.create(CreateParams::default()).await,
        Err(PortError::Denied(_))
    ));
    Ok(())
}

#[tokio::test]
async fn cross_tenant_list_attach_and_history_are_denied() -> TestResult {
    // ID-03
    let fx = fixture()?;
    let a = TenantId::try_from_str("tenant-a")?;
    let b = TenantId::try_from_str("tenant-b")?;
    let alice = connect(&fx.host, scoped(1, PermissionTier::Operator, &a)).await?;
    let bob = connect(&fx.host, scoped(2, PermissionTier::Operator, &b)).await?;
    let session = alice.create(CreateParams::default()).await?;
    assert_eq!(session.tenant.as_ref(), Some(&a));

    assert!(bob.list().await?.is_empty());
    assert!(matches!(
        bob.attach(attach(&session.session_id, None)).await,
        Err(PortError::NotFound)
    ));
    assert!(matches!(
        bob.submit(submit(&session.session_id, "hi", "m1", Cursor::default()))
            .await,
        Err(PortError::NotFound)
    ));
    assert_eq!(alice.list().await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn duplicate_client_msg_id_runs_one_model_call() -> TestResult {
    // SES-01
    let fx = fixture()?;
    let client = connect(&fx.host, local(1, PermissionTier::Operator)).await?;
    let session = client.create(CreateParams::default()).await?.session_id;
    let first = client
        .submit(submit(&session, "hello", "same", Cursor::default()))
        .await?;
    let second = client
        .submit(submit(&session, "hello", "same", Cursor::default()))
        .await?;
    assert!(matches!(first, SubmitResult::Accepted { .. }));
    assert!(matches!(second, SubmitResult::Accepted { .. }));
    wait_idle(&client, &session).await?;
    assert_eq!(fx.driver.turns.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn stale_head_is_reported() -> TestResult {
    // SES-02
    let fx = fixture()?;
    let client = connect(&fx.host, local(1, PermissionTier::Operator)).await?;
    let session = client.create(CreateParams::default()).await?.session_id;
    client
        .submit(submit(&session, "one", "m1", Cursor::default()))
        .await?;
    wait_idle(&client, &session).await?;
    let stale = client
        .submit(submit(&session, "two", "m2", Cursor::default()))
        .await?;
    assert!(matches!(stale, SubmitResult::Stale { head } if head.durable == 1));
    let forced = client
        .submit(SubmitParams {
            force: true,
            ..submit(&session, "two", "m3", Cursor::default())
        })
        .await?;
    assert!(matches!(forced, SubmitResult::Accepted { .. }));
    Ok(())
}

#[tokio::test]
async fn two_clients_share_one_writer_and_both_see_frames() -> TestResult {
    // SES-03 + multiplex semantics at the host level
    let gate = Arc::new(Notify::new());
    let gate_for_driver = Arc::clone(&gate);
    let fx = fixture_with(move |driver| driver.gate = Some(gate_for_driver))?;
    let laptop = connect(&fx.host, local(1, PermissionTier::Operator)).await?;
    let phone = connect(&fx.host, local(2, PermissionTier::Operator)).await?;
    let session = laptop.create(CreateParams::default()).await?.session_id;
    let (_, mut laptop_frames) = laptop.attach(attach(&session, None)).await?;
    let (_, mut phone_frames) = phone.attach(attach(&session, None)).await?;

    let a = laptop
        .submit(submit(&session, "from laptop", "l1", Cursor::default()))
        .await?;
    let b = phone
        .submit(SubmitParams {
            force: true,
            ..submit(&session, "from phone", "p1", Cursor::default())
        })
        .await?;
    assert_eq!(a, SubmitResult::Accepted { position: 0 });
    assert_eq!(b, SubmitResult::Accepted { position: 1 });

    let is_delta = |frame: &SessionFrame| matches!(frame, SessionFrame::Turn(TurnEvent::AssistantDelta { text, .. }) if text == "echo from laptop");
    wait_for(&mut laptop_frames, is_delta).await?;
    wait_for(&mut phone_frames, is_delta).await?;
    assert_eq!(
        fx.driver.turns.load(Ordering::SeqCst),
        1,
        "second turn waits its turn"
    );
    gate.notify_one();
    gate.notify_one();
    wait_idle(&laptop, &session).await?;
    assert_eq!(fx.driver.turns.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn detach_does_not_cancel_the_running_turn() -> TestResult {
    // SES-04
    let gate = Arc::new(Notify::new());
    let gate_for_driver = Arc::clone(&gate);
    let fx = fixture_with(move |driver| driver.gate = Some(gate_for_driver))?;
    let client = connect(&fx.host, local(1, PermissionTier::Operator)).await?;
    let session = client.create(CreateParams::default()).await?.session_id;
    let (_, frames) = client.attach(attach(&session, None)).await?;
    client
        .submit(submit(&session, "work", "w1", Cursor::default()))
        .await?;
    tokio::time::sleep(Duration::from_millis(20)).await;
    client.detach(session.clone()).await?;
    drop(frames);
    gate.notify_one();
    wait_idle(&client, &session).await?;
    assert_eq!(fx.driver.turns.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn interrupt_cancels_the_running_turn() -> TestResult {
    let gate = Arc::new(Notify::new());
    let gate_for_driver = Arc::clone(&gate);
    let fx = fixture_with(move |driver| driver.gate = Some(gate_for_driver))?;
    let client = connect(&fx.host, local(1, PermissionTier::Operator)).await?;
    let session = client.create(CreateParams::default()).await?.session_id;
    client
        .submit(submit(&session, "long", "x1", Cursor::default()))
        .await?;
    tokio::time::sleep(Duration::from_millis(20)).await;
    client
        .interrupt(InterruptParams {
            session_id: session.clone(),
            turn_id: None,
        })
        .await?;
    wait_idle(&client, &session).await?;
    Ok(())
}

#[tokio::test]
async fn approval_race_has_one_winner_and_actor_is_server_derived() -> TestResult {
    // APP-01 + ID-05
    let fx = fixture_with(|driver| driver.park = true)?;
    let first = connect(&fx.host, local(1, PermissionTier::Operator)).await?;
    let second = connect(&fx.host, local(2, PermissionTier::Operator)).await?;
    let session = first.create(CreateParams::default()).await?.session_id;
    let (_, mut frames) = first.attach(attach(&session, None)).await?;
    first
        .submit(submit(&session, "rm", "a1", Cursor::default()))
        .await?;
    let requested = wait_for(&mut frames, |f| {
        matches!(f, SessionFrame::ApprovalRequested(_))
    })
    .await?;
    let SessionFrame::ApprovalRequested(request) = requested.frame else {
        return Err("expected approval".into());
    };
    let respond = |decision| ApprovalRespondParams {
        request_id: request.id.clone(),
        decision,
        reason: None,
    };
    let (a, b) = tokio::join!(
        first.respond(respond(ReviewDecision::Approved)),
        second.respond(respond(ReviewDecision::Rejected))
    );
    let results = [a?, b?];
    let winners = results
        .iter()
        .filter(|result| **result == RespondResult::Resolved)
        .count();
    assert_eq!(winners, 1);
    let loser = results
        .iter()
        .find(|result| **result != RespondResult::Resolved)
        .ok_or("no loser")?;
    assert!(
        matches!(loser, RespondResult::AlreadyResolved { by } if by.starts_with("operator:uid:"))
    );
    let resolved = wait_for(&mut frames, |f| {
        matches!(f, SessionFrame::ApprovalResolved { .. })
    })
    .await?;
    assert!(
        matches!(resolved.frame, SessionFrame::ApprovalResolved { by, .. } if by.starts_with("operator:uid:"))
    );
    wait_idle(&first, &session).await?;
    Ok(())
}

#[tokio::test]
async fn observer_cannot_approve_or_submit() -> TestResult {
    let fx = fixture_with(|driver| driver.park = true)?;
    let operator = connect(&fx.host, local(1, PermissionTier::Operator)).await?;
    let observer = connect(&fx.host, local(2, PermissionTier::Observer)).await?;
    let session = operator.create(CreateParams::default()).await?.session_id;
    let (_, mut frames) = observer.attach(attach(&session, None)).await?;
    assert!(matches!(
        observer
            .submit(submit(&session, "x", "o1", Cursor::default()))
            .await,
        Err(PortError::Denied(_))
    ));
    operator
        .submit(submit(&session, "rm", "a1", Cursor::default()))
        .await?;
    let requested = wait_for(&mut frames, |f| {
        matches!(f, SessionFrame::ApprovalRequested(_))
    })
    .await?;
    let SessionFrame::ApprovalRequested(request) = requested.frame else {
        return Err("expected approval".into());
    };
    assert!(matches!(
        observer
            .respond(ApprovalRespondParams {
                request_id: request.id,
                decision: ReviewDecision::Approved,
                reason: None,
            })
            .await,
        Err(PortError::Denied(_))
    ));
    Ok(())
}

#[tokio::test]
async fn reattach_replays_the_exact_durable_suffix() -> TestResult {
    // RP-01
    let fx = fixture()?;
    let client = connect(&fx.host, local(1, PermissionTier::Operator)).await?;
    let session = client.create(CreateParams::default()).await?.session_id;
    let mut head = Cursor::default();
    for (index, text) in ["one", "two", "three"].iter().enumerate() {
        let result = client
            .submit(submit(&session, text, &format!("m{index}"), head))
            .await?;
        assert!(
            matches!(result, SubmitResult::Accepted { .. }),
            "{result:?}"
        );
        wait_idle(&client, &session).await?;
        head.durable += 1;
    }
    let from = Cursor {
        generation: 0,
        durable: 1,
        live: 0,
    };
    let (ack, mut frames) = client.attach(attach(&session, Some(from))).await?;
    assert_eq!(ack.head.durable, 3);
    assert_eq!(ack.replay_from.durable, 1);
    let mut durable = Vec::new();
    while durable.len() < 2 {
        let frame = next_frame(&mut frames).await?;
        if let SessionFrame::Turn(TurnEvent::ItemAdded { .. }) = frame.frame {
            durable.push(frame.cursor.durable);
        }
    }
    assert_eq!(durable, vec![2, 3]);
    Ok(())
}

#[tokio::test]
async fn old_generation_forces_resync() -> TestResult {
    // RP-02
    let fx = fixture()?;
    let client = connect(&fx.host, local(1, PermissionTier::Operator)).await?;
    let session = client.create(CreateParams::default()).await?.session_id;
    let from = Cursor {
        generation: 7,
        durable: 0,
        live: 0,
    };
    let (_, mut frames) = client.attach(attach(&session, Some(from))).await?;
    let first = next_frame(&mut frames).await?;
    assert!(matches!(first.frame, SessionFrame::Resync { .. }));
    Ok(())
}

#[tokio::test]
async fn restart_bumps_epoch_and_interrupts_running_sessions() -> TestResult {
    // RP-03
    let dir = tempfile::tempdir()?;
    let transcripts = Arc::new(MemoryTranscripts::new());
    let approvals = Arc::new(MemoryApprovals::new());
    let gate = Arc::new(Notify::new());
    let mut driver = FakeDriver::new(Arc::clone(&transcripts), Arc::clone(&approvals));
    driver.gate = Some(Arc::clone(&gate));
    let driver = Arc::new(driver);
    let first = SessionHost::open(
        HostConfig::new(dir.path().to_path_buf()),
        Arc::clone(&driver) as Arc<dyn TurnDriver>,
        Arc::clone(&transcripts) as Arc<dyn crate::replay::TranscriptSource>,
        Arc::clone(&approvals) as Arc<dyn crate::approvals::ApprovalBackend>,
    )?;
    let client = connect(&first, local(1, PermissionTier::Operator)).await?;
    let session = client.create(CreateParams::default()).await?.session_id;
    client
        .submit(submit(&session, "long", "r1", Cursor::default()))
        .await?;
    tokio::time::sleep(Duration::from_millis(20)).await;

    // A second host on the same state dir models the restart.
    let second = SessionHost::open(
        HostConfig::new(dir.path().to_path_buf()),
        driver as Arc<dyn TurnDriver>,
        transcripts,
        approvals,
    )?;
    assert_eq!(second.epoch(), first.epoch() + 1);
    let client = connect(&second, local(1, PermissionTier::Operator)).await?;
    let listed = client.list().await?;
    assert_eq!(
        listed.first().map(|s| s.state.clone()),
        Some(HostedState::Interrupted)
    );
    let denied = client
        .submit(submit(&session, "again", "r2", Cursor::default()))
        .await?;
    assert!(matches!(denied, SubmitResult::Denied { .. }));
    client.resume(session.clone()).await?;
    let (ack, _) = client.attach(attach(&session, None)).await?;
    assert_eq!(ack.host_epoch, second.epoch());
    assert_eq!(ack.head.durable, 1, "durable data survives the restart");
    gate.notify_waiters();
    Ok(())
}

#[tokio::test]
async fn revoked_device_stream_closes_and_calls_fail() -> TestResult {
    // REV-01 / REV-02 at the host level
    let fx = fixture()?;
    let device = DeviceId::try_from_str("phone-1")?;
    let tenant = TenantId::try_from_str("t")?;
    let mut identity = scoped(5, PermissionTier::Operator, &tenant);
    identity.device = Some(device.clone());
    let phone = connect(&fx.host, identity.clone()).await?;
    let session = phone.create(CreateParams::default()).await?.session_id;
    let (_, mut frames) = phone.attach(attach(&session, None)).await?;
    fx.host.revoke_device(&device);
    wait_for(&mut frames, |f| matches!(f, SessionFrame::Revoked)).await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(5), frames.next())
            .await??
            .is_none()
    );
    assert!(matches!(phone.list().await, Err(PortError::Revoked)));
    assert!(matches!(
        fx.host.connect(identity),
        Err(crate::HostError::Revoked)
    ));
    Ok(())
}

#[tokio::test]
async fn drain_tells_clients_to_come_back() -> TestResult {
    let fx = fixture()?;
    let client = connect(&fx.host, local(1, PermissionTier::Operator)).await?;
    let session = client.create(CreateParams::default()).await?.session_id;
    let (_, mut frames) = client.attach(attach(&session, None)).await?;
    fx.host.drain(1500);
    wait_for(&mut frames, |f| {
        matches!(
            f,
            SessionFrame::HostDraining {
                retry_after_ms: 1500
            }
        )
    })
    .await?;
    assert!(fx.host.connect(local(2, PermissionTier::Operator)).is_err());
    Ok(())
}

/// R18 §7: while draining, new submissions are refused in band; the
/// W00 caps and tenant admission still come first.
#[tokio::test]
async fn drain_refuses_new_submissions() -> TestResult {
    let fx = fixture()?;
    let client = connect(&fx.host, local(1, PermissionTier::Operator)).await?;
    let session = client.create(CreateParams::default()).await?.session_id;
    fx.host.drain(10);
    assert!(fx.host.is_draining());
    let result = client
        .submit(submit(&session, "hi", "m1", Cursor::default()))
        .await?;
    assert!(matches!(result, SubmitResult::Denied { .. }));
    assert_eq!(fx.driver.turns.load(Ordering::SeqCst), 0);
    Ok(())
}

/// R18 §2.3: a minor-1 hello never carries an R18 cap, even when the
/// ceiling holds one; features of minor 2 are not negotiated at minor 1.
#[tokio::test]
async fn minor_one_hello_masks_r18_caps_and_features() -> TestResult {
    let fx = fixture()?;
    let mut identity = local(1, PermissionTier::Owner);
    identity.caps = identity.caps.with(crate::identity::gateway_caps_for_tier(
        PermissionTier::Owner,
    ));
    let old = fx.host.connect(identity.clone())?;
    let ack = old
        .hello(HelloParams {
            client_label: "old".into(),
            wire_minor: 1,
            features: vec!["gateway".into(), "compact".into()],
            requested_caps: None,
        })
        .await?;
    assert_eq!(ack.granted, ClientCaps::ALL);
    assert_eq!(ack.features, vec!["compact".to_owned()]);
    let new = fx.host.connect(ClientIdentity {
        connection: crate::identity::ConnectionId::next(),
        ..identity
    })?;
    let ack = new
        .hello(HelloParams {
            client_label: "new".into(),
            wire_minor: 2,
            features: vec!["gateway".into()],
            requested_caps: None,
        })
        .await?;
    assert!(ack.granted.gateway_read && ack.granted.gateway_admin);
    assert!(!ack.granted.tool_call);
    assert_eq!(ack.features, vec!["gateway".to_owned()]);
    Ok(())
}

#[tokio::test]
async fn presence_lists_attached_clients() -> TestResult {
    let fx = fixture()?;
    let laptop = connect(&fx.host, local(1, PermissionTier::Operator)).await?;
    let phone = connect(&fx.host, local(2, PermissionTier::Observer)).await?;
    let session = laptop.create(CreateParams::default()).await?.session_id;
    let (_, mut frames) = laptop.attach(attach(&session, None)).await?;
    let (_, _phone_frames) = phone.attach(attach(&session, None)).await?;
    let presence = wait_for(
        &mut frames,
        |f| matches!(f, SessionFrame::Presence { attached } if attached.len() == 2),
    )
    .await?;
    let SessionFrame::Presence { attached } = presence.frame else {
        return Err("expected presence".into());
    };
    assert!(
        attached
            .iter()
            .any(|entry| entry.caps == ClientCaps::OBSERVE)
    );
    Ok(())
}

#[tokio::test]
async fn settings_need_control_and_apply_when_idle() -> TestResult {
    let fx = fixture()?;
    let operator = connect(&fx.host, local(1, PermissionTier::Operator)).await?;
    let maintainer = connect(&fx.host, local(2, PermissionTier::Maintainer)).await?;
    let session = operator.create(CreateParams::default()).await?.session_id;
    let params = harw_protocol::session_wire::SetModelParams {
        session_id: session.clone(),
        model: "other".into(),
    };
    assert!(matches!(
        operator.set_model(params.clone()).await,
        Err(PortError::Denied(_))
    ));
    maintainer.set_model(params).await?;
    let applied = fx.driver.settings.lock().map_err(|_| "poisoned")?.clone();
    assert_eq!(applied, vec![Setting::Model("other".into())]);
    Ok(())
}

fn session_error_text(frame: &SessionFrame) -> Option<&str> {
    match frame {
        SessionFrame::Session(harw_protocol::SessionEvent::SessionError { message, .. }) => {
            Some(message.as_str())
        }
        _ => None,
    }
}

#[tokio::test]
async fn driver_error_is_visible_and_next_submit_works() -> TestResult {
    let fx = fixture()?;
    let client = connect(&fx.host, local(1, PermissionTier::Operator)).await?;
    let session = client.create(CreateParams::default()).await?.session_id;
    let (_ack, mut frames) = client.attach(attach(&session, None)).await?;

    let long = "x".repeat(2_000);
    fx.driver.script_failure(Err(HostError::Driver(format!(
        "boom\u{1b}[31m\n\tred {long}"
    ))));
    let first = client
        .submit(submit(&session, "one", "m1", Cursor::default()))
        .await?;
    assert!(matches!(first, SubmitResult::Accepted { .. }));
    let notice = wait_for(&mut frames, |f| session_error_text(f).is_some()).await?;
    let text = session_error_text(&notice.frame).ok_or("no notice")?;
    assert!(
        text.starts_with("turn failed (internal error): driver: boom"),
        "{text}"
    );
    assert!(!text.contains('\u{1b}') && !text.contains('\n'), "{text:?}");
    assert!(
        text.chars().count() < 400,
        "bounded: {}",
        text.chars().count()
    );
    wait_idle(&client, &session).await?;

    // The session is alive: the next submit runs a normal turn.
    let second = client
        .submit(submit(&session, "two", "m2", Cursor::default()))
        .await?;
    assert!(matches!(second, SubmitResult::Accepted { .. }), "{second:?}");
    wait_for(&mut frames, |f| {
        matches!(f, SessionFrame::Turn(TurnEvent::AssistantDelta { text, .. }) if text == "echo two")
    })
    .await?;
    wait_idle(&client, &session).await?;
    assert_eq!(fx.driver.turns.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn typed_failure_carries_its_cause_and_never_sets_failed_state() -> TestResult {
    let fx = fixture()?;
    let client = connect(&fx.host, local(1, PermissionTier::Operator)).await?;
    let session = client.create(CreateParams::default()).await?.session_id;
    let (_ack, mut frames) = client.attach(attach(&session, None)).await?;
    for (n, (cause, label)) in [
        (FailureCause::ProviderAuth, "provider authentication failed"),
        (FailureCause::Quota, "provider quota or budget exhausted"),
        (FailureCause::ContextLength, "context length exceeded"),
        (FailureCause::Refusal, "model refused"),
        (FailureCause::RequestFailed, "model request failed"),
    ]
    .into_iter()
    .enumerate()
    {
        fx.driver.script_failure(Ok(TurnOutcome::FailedWith {
            cause,
            reason: "provider said no".into(),
        }));
        let msg = format!("m{n}");
        client
            .submit(submit(&session, "x", &msg, Cursor::default()))
            .await?;
        let notice = wait_for(&mut frames, |f| session_error_text(f).is_some()).await?;
        let text = session_error_text(&notice.frame).ok_or("no notice")?;
        assert_eq!(text, format!("turn failed ({label}): provider said no"));
        wait_idle(&client, &session).await?;
        let listed = client.list().await?;
        assert!(listed.iter().all(|s| s.state != HostedState::Failed));
    }
    Ok(())
}

#[tokio::test]
async fn interrupted_after_restart_explains_itself_and_resume_works() -> TestResult {
    let dir = tempfile::tempdir()?;
    let transcripts = Arc::new(MemoryTranscripts::new());
    let approvals = Arc::new(MemoryApprovals::new());
    let gate = Arc::new(Notify::new());
    let mut driver = FakeDriver::new(Arc::clone(&transcripts), Arc::clone(&approvals));
    driver.gate = Some(Arc::clone(&gate));
    let driver = Arc::new(driver);
    let first = SessionHost::open(
        HostConfig::new(dir.path().to_path_buf()),
        Arc::clone(&driver) as Arc<dyn TurnDriver>,
        Arc::clone(&transcripts) as Arc<dyn crate::replay::TranscriptSource>,
        Arc::clone(&approvals) as Arc<dyn crate::approvals::ApprovalBackend>,
    )?;
    let client = connect(&first, local(1, PermissionTier::Operator)).await?;
    let session = client.create(CreateParams::default()).await?.session_id;
    client
        .submit(submit(&session, "long", "r1", Cursor::default()))
        .await?;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let second = SessionHost::open(
        HostConfig::new(dir.path().to_path_buf()),
        Arc::clone(&driver) as Arc<dyn TurnDriver>,
        transcripts,
        approvals,
    )?;
    let client = connect(&second, local(1, PermissionTier::Operator)).await?;

    // Attaching explains the state.
    let (_ack, mut frames) = client.attach(attach(&session, None)).await?;
    let notice = wait_for(&mut frames, |f| session_error_text(f).is_some()).await?;
    let text = session_error_text(&notice.frame).ok_or("no notice")?;
    assert!(
        text.contains("interrupted") && text.contains("resume"),
        "{text}"
    );

    // Submitting is refused with a clear reason.
    let denied = client
        .submit(submit(&session, "again", "r2", Cursor::default()))
        .await?;
    let SubmitResult::Denied { reason } = denied else {
        return Err("submit must be denied while interrupted".into());
    };
    assert!(
        reason.contains("interrupted") && reason.contains("resume"),
        "{reason}"
    );

    // Resume returns the session to Idle; the next submit is accepted.
    client.resume(session.clone()).await?;
    wait_idle(&client, &session).await?;
    let mut accepted = client
        .submit(submit(&session, "again", "r3", Cursor::default()))
        .await?;
    if let SubmitResult::Stale { head } = accepted {
        accepted = client.submit(submit(&session, "again", "r3", head)).await?;
    }
    assert!(
        matches!(accepted, SubmitResult::Accepted { .. }),
        "{accepted:?}"
    );
    gate.notify_waiters();
    Ok(())
}

#[test]
fn sanitize_notice_strips_controls_and_bounds_length() {
    use crate::driver::sanitize_notice;
    assert_eq!(sanitize_notice("a\u{1b}[0m\n\t b", 50), "a [0m b");
    assert_eq!(sanitize_notice("   ", 50), "");
    let cut = sanitize_notice(&"é".repeat(10), 4);
    assert_eq!(cut, "éééé\u{2026}");
}
