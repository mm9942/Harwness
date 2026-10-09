//! The phone terminal loop against a real session daemon: typed lines in,
//! text out, over a real Unix socket and the real `RemotePort`. Only the turn
//! driver is a fake: it parks an approval, and answers after the resume.

use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harw_mobile_core::Controller;
use harw_mobile_term::{RunEnd, run};
use harw_protocol::items::{AssistantMessageItem, ContentPart, TurnItem};
use harw_protocol::session_wire::CreateParams;
use harw_protocol::{ApprovalKind, ApprovalRequest, SessionPort, TurnEvent};
use harw_session_daemon::{DaemonError, UdsConfig, UdsServer};
use harw_session_host::approvals::MemoryApprovals;
use harw_session_host::driver::{
    CancelSignal, DriverEvent, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::replay::MemoryTranscripts;
use harw_session_host::{HostConfig, SessionHost};
use harw_session_remote::{ConnectOptions, RemotePort, connect_unix};
use harw_types::{
    ApprovalId, ItemId, ReviewDecision, RiskLevel, SessionId, ThreadId, TurnId, WorkId,
};
use tokio::sync::{mpsc, watch};

harw_test_support::define_test_error!(
    pub(crate),
    Io(std::io::Error) => "io",
    Host(harw_session_host::HostError) => "host",
    Daemon(DaemonError) => "daemon",
    Port(harw_protocol::PortError) => "port",
    Remote(harw_session_remote::RemoteError) => "remote",
    Join(tokio::task::JoinError) => "join"
);

fn ensure(condition: bool, what: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(TestError::Unexpected(what.to_owned()))
    }
}

const WAIT: Duration = Duration::from_secs(10);

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
        Box::pin(async move {
            let now = jiff::Timestamp::now();
            let parked = ApprovalRequest {
                id: ApprovalId::from_str("a1"),
                work_id: WorkId::from_str("w1"),
                kind: ApprovalKind::DynamicTool {
                    turn_id: TurnId::from_str("t1"),
                    tool_name: "shell".to_owned(),
                    arguments: serde_json::Value::Null,
                },
                summary: "run ls".to_owned(),
                risk: RiskLevel::Low,
                requested_at: now,
                timeout_at: ApprovalRequest::default_timeout_at(now),
                decisions: vec![ReviewDecision::Approved, ReviewDecision::Rejected],
            };
            self.approvals.issue(&input.session_id, parked.clone())?;
            sink.emit(DriverEvent::Turn(TurnEvent::TurnStarted {
                turn_id: TurnId::from_str("t1"),
                thread_id: ThreadId::from_str("th1"),
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
            sink.emit(DriverEvent::Turn(TurnEvent::ItemAdded {
                turn_id: TurnId::from_str("t1"),
                item: TurnItem::AssistantMessage(AssistantMessageItem {
                    id: ItemId::from_str("m1"),
                    content: vec![ContentPart::Text {
                        text: "all done".to_owned(),
                    }],
                    phase: None,
                }),
            }));
            sink.emit(DriverEvent::Turn(TurnEvent::TurnCompleted {
                turn_id: TurnId::from_str("t1"),
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

/// An output the test can read while the loop keeps writing.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut inner) = self.0.lock() {
            inner.extend_from_slice(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Shared {
    fn text(&self) -> String {
        self.0
            .lock()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    }

    async fn contains(&self, needle: &str) -> TestResult {
        let wait = async {
            while !self.text().contains(needle) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        match tokio::time::timeout(WAIT, wait).await {
            Ok(()) => Ok(()),
            Err(_) => Err(TestError::Unexpected(format!(
                "timed out waiting for {needle:?}; output so far:\n{}",
                self.text()
            ))),
        }
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
        Arc::new(ParkingDriver {
            approvals: Arc::clone(&approvals),
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

/// What a test script gets: the line input and the output so far.
struct Script {
    input: mpsc::Sender<String>,
    output: Shared,
}

impl Script {
    async fn say(&self, line: &str) -> TestResult {
        self.input
            .send(line.to_owned())
            .await
            .map_err(|e| TestError::Unexpected(e.to_string()))
    }
}

/// Starts a daemon, attaches a controller and runs the loop next to
/// `script` (on one task: the loop borrows the controller across awaits).
async fn session<F, Fut>(script: F) -> TestResult<RunEnd>
where
    F: FnOnce(Script) -> Fut,
    Fut: std::future::Future<Output = TestResult>,
{
    let daemon = start().await?;
    let connection = connect_unix(daemon.socket.clone(), ConnectOptions::new("term-test")).await?;
    let port: Arc<dyn SessionPort> = Arc::new(RemotePort::new(connection));
    let session = port
        .create(CreateParams {
            workspace: None,
            title: Some("term e2e".to_owned()),
        })
        .await?
        .session_id;
    let mut controller = Controller::new(Arc::clone(&port), "term-test");
    controller.attach(session, None).await?;

    let (input, rx) = mpsc::channel::<String>(8);
    let output = Shared::default();
    let mut sink = output.clone();
    let scripted = script(Script { input, output });
    let both = async { tokio::join!(run(&mut controller, rx, &mut sink), scripted) };
    let (end, outcome) = tokio::time::timeout(WAIT * 2, both)
        .await
        .map_err(|_| TestError::Unexpected("the session did not finish".to_owned()))?;
    let _ = daemon.shutdown.send(true);
    let _ = daemon.task.await;
    outcome?;
    Ok(end?)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_an_approval_and_the_answer_over_the_real_daemon() -> TestResult {
    let end = session(|term| async move {
        term.say("list the files").await?;
        term.output.contains("? [1] run ls (risk low)").await?;
        term.output.contains("/y approve, /n reject").await?;
        term.say("/y").await?;
        term.output.contains("agent: all done").await?;
        term.output.contains("- done").await?;
        ensure(
            term.output.text().contains("- a1 approved by"),
            "who decided is shown",
        )?;
        term.say("/q").await
    })
    .await?;
    ensure(end == RunEnd::Quit, "/q leaves")
}

#[tokio::test(flavor = "multi_thread")]
async fn commands_without_a_target_explain_themselves_and_a_rejection_goes_through() -> TestResult {
    let end = session(|term| async move {
        term.say("/y").await?;
        term.output.contains("? no open approval").await?;
        term.say("/frobnicate").await?;
        term.output
            .contains("? unknown command /frobnicate")
            .await?;
        term.say("/help").await?;
        term.output.contains("/y [n]").await?;
        term.say("/p").await?;
        term.output.contains("no open approvals").await?;

        term.say("go").await?;
        term.output.contains("? [1] run ls").await?;
        term.say("/n not now").await?;
        term.output.contains("- a1 rejected by").await?;
        term.say("/q").await
    })
    .await?;
    ensure(end == RunEnd::Quit, "/q leaves")
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_the_input_ends_the_loop_cleanly() -> TestResult {
    let end = session(|term| async move {
        drop(term.input);
        Ok(())
    })
    .await?;
    ensure(end == RunEnd::InputClosed, "input closed")
}
