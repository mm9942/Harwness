//! Entfernte Sitzungen gegen den echten Host: ein `harw-session-daemon` auf
//! einem echten Unix-Socket, die echte Verbindung und der echte Host. Nur der
//! Turn-Treiber ist eine Attrappe (kein Modell): er parkt je Turn eine
//! Freigabe, setzt nach der Entscheidung fort und antwortet mit "answer N".

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harw_protocol::items::{AssistantMessageItem, ContentPart, TurnItem};
use harw_protocol::{ApprovalKind, ApprovalRequest as Wire, TurnEvent};
use harw_session_daemon::{DaemonError, UdsConfig, UdsServer};
use harw_session_host::approvals::MemoryApprovals;
use harw_session_host::driver::{
    CancelSignal, DriverEvent, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::replay::MemoryTranscripts;
use harw_session_host::{HostConfig, SessionHost};
use harw_types::{
    ApprovalId, ItemId, ReviewDecision, RiskLevel, SessionId, ThreadId, TurnId, WorkId,
};
use harwness_sdk::prelude::*;
use harwness_sdk::serde_json::{self, Value};
use harwness_sdk::{ApprovalRequest, RemoteHarwness};
use tokio::sync::watch;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Driver {
    approvals: Arc<MemoryApprovals>,
    turn: AtomicU32,
}

impl Driver {
    fn current(&self) -> u32 {
        self.turn.load(Ordering::SeqCst)
    }
}

impl TurnDriver for Driver {
    fn create_session(&self, _: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn run_turn(
        &self,
        input: TurnInput,
        _: CancelSignal,
        sink: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        Box::pin(async move {
            let n = self.turn.fetch_add(1, Ordering::SeqCst) + 1;
            let now = jiff::Timestamp::now();
            let parked = Wire {
                id: ApprovalId::from_str(format!("a{n}")),
                work_id: WorkId::from_str(format!("w{n}")),
                kind: ApprovalKind::DynamicTool {
                    turn_id: TurnId::from_str(format!("t{n}")),
                    tool_name: "shell".to_owned(),
                    arguments: serde_json::json!({ "cmd": "ls" }),
                },
                summary: "run ls".to_owned(),
                risk: RiskLevel::Low,
                requested_at: now,
                timeout_at: Wire::default_timeout_at(now),
                decisions: vec![ReviewDecision::Approved, ReviewDecision::Rejected],
            };
            self.approvals.issue(&input.session_id, parked.clone())?;
            sink.emit(DriverEvent::Turn(TurnEvent::TurnStarted {
                turn_id: TurnId::from_str(format!("t{n}")),
                thread_id: ThreadId::from_str("th"),
            }));
            sink.emit(DriverEvent::ApprovalRequested(parked));
            Ok(TurnOutcome::AwaitingApproval)
        })
    }

    fn resume_after_approval(
        &self,
        _: &SessionId,
        _: CancelSignal,
        sink: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        Box::pin(async move {
            let n = self.current();
            sink.emit(DriverEvent::Turn(TurnEvent::ItemAdded {
                turn_id: TurnId::from_str(format!("t{n}")),
                item: TurnItem::AssistantMessage(AssistantMessageItem {
                    id: ItemId::from_str(format!("m{n}")),
                    content: vec![ContentPart::Text {
                        text: format!("answer {n}"),
                    }],
                    phase: Some(harw_types::MessagePhase::FinalAnswer),
                }),
            }));
            sink.emit(DriverEvent::Turn(TurnEvent::TurnCompleted {
                turn_id: TurnId::from_str(format!("t{n}")),
                usage: None,
            }));
            Ok(TurnOutcome::Completed)
        })
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
    socket: std::path::PathBuf,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), DaemonError>>,
}

async fn start() -> TestResult<Daemon> {
    let dir = tempfile::tempdir()?;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
    let uid = std::fs::metadata(dir.path())?.uid();
    let socket = dir.path().join("session.sock");
    let approvals = Arc::new(MemoryApprovals::new());
    let host = SessionHost::open(
        HostConfig::new(dir.path().join("state")),
        Arc::new(Driver {
            approvals: Arc::clone(&approvals),
            turn: AtomicU32::new(0),
        }),
        Arc::new(MemoryTranscripts::new()),
        approvals,
    )?;
    let server = UdsServer::bind(UdsConfig::new(&socket, uid)).await?;
    let (shutdown, rx) = watch::channel(false);
    let task = tokio::spawn(server.run(host, rx));
    Ok(Daemon {
        _dir: dir,
        socket,
        shutdown,
        task,
    })
}

impl Daemon {
    async fn stop(self) {
        let _ = self.shutdown.send(true);
        let _ = self.task.await;
    }
}

fn recording_approver(
    seen: Arc<Mutex<Vec<ApprovalRequest>>>,
    answer: Decision,
) -> impl ApprovalHandler {
    approval_fn(move |request: ApprovalRequest| {
        let seen = Arc::clone(&seen);
        let answer = answer.clone();
        async move {
            if let Ok(mut all) = seen.lock() {
                all.push(request);
            }
            answer
        }
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn send_runs_a_turn_through_an_approval_and_reports_it() -> TestResult {
    let daemon = start().await?;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let harwness = RemoteHarwness::builder()
        .socket(daemon.socket.clone())
        .label("sdk-test")
        .approval_handler(recording_approver(Arc::clone(&seen), Decision::Approve))
        .turn_timeout(Duration::from_secs(10))
        .connect()
        .await?;
    let mut session = harwness.create_session(Some("sdk e2e")).await?;
    assert!(session.may_approve(), "the local owner may approve");

    let mut events = Vec::new();
    let report = session
        .send_with("list the files", |event| events.push(event.clone()))
        .await?;

    assert_eq!(report.status, TurnStatus::Completed);
    assert_eq!(report.text.as_deref(), Some("answer 1"));
    assert_eq!(report.approvals, 1);
    assert_eq!(&report.session_id, session.id());

    let asked = seen.lock().map_err(|e| e.to_string())?.clone();
    assert_eq!(asked.len(), 1, "the handler is asked once");
    assert_eq!(asked[0].tool, "shell");
    assert_eq!(asked[0].arguments, serde_json::json!({ "cmd": "ls" }));
    assert_eq!(
        (asked[0].request_id.as_str(), asked[0].call_id.as_str()),
        ("a1", "w1")
    );

    assert!(
        events
            .iter()
            .any(|e| matches!(e, SdkEvent::TurnStarted { .. })),
        "the turn start is reported"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            SdkEvent::Message { text, final_answer: true, .. } if text == "answer 1"
        )),
        "the answer is reported as a final message"
    );
    assert!(events.last().is_some_and(SdkEvent::is_root_finish));
    daemon.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_default_handler_denies_and_the_turn_still_ends() -> TestResult {
    let daemon = start().await?;
    // No handler set: AutoDeny.
    let harwness = RemoteHarwness::builder()
        .socket(daemon.socket.clone())
        .turn_timeout(Duration::from_secs(10))
        .connect()
        .await?;
    let mut session = harwness.create_session(None).await?;
    let report = session.send("go").await?;
    assert_eq!(report.status, TurnStatus::Completed);
    assert_eq!(report.approvals, 1, "the request was answered (denied)");
    daemon.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_lists_what_the_host_knows() -> TestResult {
    let daemon = start().await?;
    let harwness = RemoteHarwness::builder()
        .socket(daemon.socket.clone())
        .connect()
        .await?;
    let created = harwness.create_session(Some("listed")).await?;
    let all = harwness.sessions().await?;
    let found = all.iter().find(|s| &s.id == created.id());
    assert!(found.is_some(), "the new session is listed");
    let found = found.ok_or("missing")?;
    assert_eq!(found.title.as_deref(), Some("listed"));
    assert_eq!(found.state, "idle");
    daemon.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_client_can_attach_later_and_send_on_the_same_session() -> TestResult {
    let daemon = start().await?;
    let approve = |seen| recording_approver(seen, Decision::Approve);
    let first = RemoteHarwness::builder()
        .socket(daemon.socket.clone())
        .approval_handler(approve(Arc::new(Mutex::new(Vec::new()))))
        .turn_timeout(Duration::from_secs(10))
        .connect()
        .await?;
    let mut a = first.create_session(None).await?;
    let report_a = a.send("one").await?;
    assert_eq!(report_a.text.as_deref(), Some("answer 1"));

    // A second client attaches afterwards and runs its own turn. (That replayed
    // frames never end a turn is covered deterministically by the unit test
    // `replay_before_and_after_the_submit_is_not_the_new_turn`: this test
    // host keeps no transcript, so it sends no replay.)
    let second = RemoteHarwness::builder()
        .socket(daemon.socket.clone())
        .approval_handler(approve(Arc::new(Mutex::new(Vec::new()))))
        .turn_timeout(Duration::from_secs(10))
        .connect()
        .await?;
    let mut b = second.attach(a.id()).await?;
    let report_b = b.send("two").await?;
    assert_eq!(
        report_b.text.as_deref(),
        Some("answer 2"),
        "the report is of the second turn"
    );
    assert_eq!(report_b.status, TurnStatus::Completed);
    daemon.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_that_waits_forever_hits_the_time_limit() -> TestResult {
    let daemon = start().await?;
    let stuck = approval_fn(|_request: ApprovalRequest| async {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        Decision::Approve
    });
    let harwness = RemoteHarwness::builder()
        .socket(daemon.socket.clone())
        .approval_handler(stuck)
        .turn_timeout(Duration::from_millis(300))
        .connect()
        .await?;
    let mut session = harwness.create_session(None).await?;
    let outcome = session.send("go").await;
    assert!(
        matches!(outcome, Err(SdkError::Turn { .. })),
        "the limit ends the wait: {outcome:?}"
    );
    daemon.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn connecting_without_a_socket_or_to_nothing_fails_clearly() -> TestResult {
    let none = RemoteHarwness::builder().connect().await;
    assert!(matches!(none, Err(SdkError::InvalidInput { .. })));
    let dir = tempfile::tempdir()?;
    let gone = RemoteHarwness::builder()
        .socket(dir.path().join("nothing.sock"))
        .connect()
        .await;
    assert!(matches!(gone, Err(SdkError::Session { .. })), "{gone:?}");
    let _: Option<Value> = None;
    Ok(())
}
