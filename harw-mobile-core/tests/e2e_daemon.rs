//! The phone controller against the real stack: a session daemon on a real
//! Unix socket, the real `RemotePort`, and a driver that parks an approval.
//!
//! The only fake is the turn driver (no model). Everything the controller
//! relies on is real: the hello and the caps the host grants, the compact
//! attach, frame delivery with cursors, `approval.respond` and its effect.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::Arc;
use std::time::Duration;

use harw_mobile_core::{Activity, Controller};
use harw_protocol::session_wire::{CreateParams, SubmitResult};
use harw_protocol::{ApprovalKind, ApprovalRequest, SessionPort, TurnEvent};
use harw_session_daemon::{DaemonError, UdsConfig, UdsServer};
use harw_session_host::approvals::MemoryApprovals;
use harw_session_host::driver::{
    CancelSignal, DriverEvent, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::replay::MemoryTranscripts;
use harw_session_host::{HostConfig, SessionHost};
use harw_session_remote::{ConnectOptions, RemotePort, connect_unix};
use harw_types::{ApprovalId, ReviewDecision, RiskLevel, SessionId, ThreadId, TurnId, WorkId};
use tokio::sync::watch;

harw_test_support::define_test_error!(
    pub(crate),
    Io(std::io::Error) => "io",
    Host(harw_session_host::HostError) => "host",
    Daemon(DaemonError) => "daemon",
    Port(harw_protocol::PortError) => "port",
    Remote(harw_session_remote::RemoteError) => "remote"
);

fn ensure(condition: bool, what: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(TestError::Unexpected(what.to_owned()))
    }
}

const WAIT: Duration = Duration::from_secs(10);

/// Parks an approval on the first turn and completes after the resume.
struct ParkingDriver {
    approvals: Arc<MemoryApprovals>,
}

fn request() -> ApprovalRequest {
    let now = jiff::Timestamp::now();
    ApprovalRequest {
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
    }
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
            let parked = request();
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

async fn connect(daemon: &Daemon, label: &str) -> TestResult<Arc<dyn SessionPort>> {
    let connection = connect_unix(daemon.socket.clone(), ConnectOptions::new(label)).await?;
    Ok(Arc::new(RemotePort::new(connection)))
}

/// Drive the controller until `done` holds, failing after [`WAIT`].
async fn pump_until(
    controller: &mut Controller,
    what: &str,
    done: impl Fn(&Controller) -> bool,
) -> TestResult {
    let work = async {
        while !done(controller) {
            if !controller.next().await? {
                return Err(TestError::Unexpected(format!(
                    "stream ended before: {what}"
                )));
            }
        }
        Ok(())
    };
    match tokio::time::timeout(WAIT, work).await {
        Ok(result) => result,
        Err(_) => Err(TestError::Unexpected(format!("timed out: {what}"))),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_phone_resolves_a_parked_approval_over_the_real_daemon() -> TestResult {
    let daemon = start().await?;
    let port = connect(&daemon, "phone").await?;
    let session = port
        .create(CreateParams {
            workspace: None,
            title: Some("phone e2e".to_owned()),
        })
        .await?
        .session_id;

    let mut phone = Controller::new(Arc::clone(&port), "phone");
    phone.attach(session, None).await?;
    ensure(
        phone
            .granted()
            .is_some_and(|caps| caps.observe && caps.steer),
        "the local owner may observe and steer",
    )?;
    ensure(
        phone.granted().is_some_and(|caps| caps.approve),
        "the local owner tier carries approve",
    )?;

    let sent = phone.submit("list the files").await?;
    ensure(
        matches!(sent, SubmitResult::Accepted { .. }),
        "the prompt is accepted",
    )?;

    pump_until(&mut phone, "the approval reaches the inbox", |c| {
        c.view().pending_approvals().count() == 1
    })
    .await?;
    ensure(
        matches!(phone.view().activity(), Activity::Running(_)),
        "a turn is running while it waits",
    )?;
    let asked = phone
        .view()
        .pending_approvals()
        .next()
        .map(|a| a.summary.clone());
    ensure(asked.as_deref() == Some("run ls"), "the summary is shown")?;

    let outcome = phone
        .decide(ApprovalId::from_str("a1"), ReviewDecision::Approved, None)
        .await?;
    ensure(
        outcome == harw_protocol::session_wire::RespondResult::Resolved,
        "the host resolves it",
    )?;

    pump_until(&mut phone, "the turn completes", |c| {
        *c.view().activity() == Activity::Idle
            && c.view().pending_approvals().count() == 0
            && c.view().decided().len() == 1
    })
    .await?;
    ensure(phone.view().last_failure().is_none(), "no failure")?;
    ensure(phone.view().cursor().is_some(), "a resume cursor is known")?;

    let _ = daemon.shutdown.send(true);
    let _ = daemon.task.await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_device_sees_the_approval_and_the_first_decision_wins() -> TestResult {
    let daemon = start().await?;
    let port_a = connect(&daemon, "phone").await?;
    let port_b = connect(&daemon, "tablet").await?;
    let session = port_a
        .create(CreateParams {
            workspace: None,
            title: None,
        })
        .await?
        .session_id;

    let mut phone = Controller::new(Arc::clone(&port_a), "phone");
    let mut tablet = Controller::new(Arc::clone(&port_b), "tablet");
    phone.attach(session.clone(), None).await?;
    tablet.attach(session, None).await?;

    phone.submit("go").await?;
    pump_until(&mut phone, "phone sees the approval", |c| {
        c.view().pending_approvals().count() == 1
    })
    .await?;
    pump_until(&mut tablet, "tablet sees the approval", |c| {
        c.view().pending_approvals().count() == 1
    })
    .await?;

    let first = phone
        .decide(ApprovalId::from_str("a1"), ReviewDecision::Approved, None)
        .await?;
    let second = tablet
        .decide(ApprovalId::from_str("a1"), ReviewDecision::Rejected, None)
        .await?;
    ensure(
        first == harw_protocol::session_wire::RespondResult::Resolved,
        "the first writer wins",
    )?;
    ensure(
        matches!(
            second,
            harw_protocol::session_wire::RespondResult::AlreadyResolved { .. }
        ),
        "the second decision is told it lost",
    )?;

    pump_until(&mut tablet, "tablet sees the decision", |c| {
        c.view().pending_approvals().count() == 0 && c.view().decided().len() == 1
    })
    .await?;
    let _ = daemon.shutdown.send(true);
    let _ = daemon.task.await;
    Ok(())
}
