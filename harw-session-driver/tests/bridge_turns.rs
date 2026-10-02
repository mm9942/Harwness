//! Turn behaviour of `CoreTurnDriver` against a scripted fake core: normal
//! turn, cancel mid-turn, driver errors surfaced, settings, typed
//! not-composed errors. Panic-free: every check returns an `Err`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use harw_core::cancel::CancelReason;
use harw_core::{ApprovalResolution, TurnInput as CoreTurnInput, TurnOutcome as CoreOutcome};
use harw_protocol::TurnEvent;
use harw_session_driver::{
    CoreDriverConfig, CoreEvent, CoreEvents, CoreFuture, CoreRuntime, CoreTurnDriver,
    DriverBridgeError, ParkedApproval,
};
use harw_session_host::HostError;
use harw_session_host::driver::{
    CancelSignal, DriverEvent, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::identity::ConnectionId;
use harw_types::{ApprovalActor, SessionId, ThreadId, TurnId};
use tokio::sync::watch;

type TestResult = Result<(), Box<dyn std::error::Error>>;

macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err(format!($($arg)+).into());
        }
    };
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, String> {
    mutex.lock().map_err(|_| "test mutex poisoned".to_owned())
}

// --- fake core -------------------------------------------------------------

enum Step {
    /// Emit `TurnStarted`/`TurnCompleted` and complete.
    Complete,
    /// Emit `TurnStarted`, then wait for the cancel token and report it.
    WaitForCancel,
    /// Return this outcome right away.
    Outcome(CoreOutcome),
    /// Fail the core call.
    Error(DriverBridgeError),
}

#[derive(Default)]
struct FakeCore {
    script: Mutex<VecDeque<Step>>,
    seen_text: Mutex<Vec<String>>,
    settings: Mutex<Vec<Setting>>,
    opened: Mutex<Vec<(String, Option<String>)>>,
}

impl FakeCore {
    fn scripted(steps: Vec<Step>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(steps.into()),
            ..Self::default()
        })
    }
}

fn runtime_error(detail: &str) -> DriverBridgeError {
    DriverBridgeError::Runtime(detail.to_owned())
}

impl CoreRuntime for FakeCore {
    fn open_session<'a>(
        &'a self,
        session_id: &'a SessionId,
        title: Option<&'a str>,
    ) -> CoreFuture<'a, ()> {
        Box::pin(async move {
            lock(&self.opened)
                .map_err(|e| runtime_error(&e))?
                .push((session_id.as_str().to_owned(), title.map(str::to_owned)));
            Ok(())
        })
    }

    fn run_turn<'a>(
        &'a self,
        _session_id: &'a SessionId,
        input: CoreTurnInput,
        events: CoreEvents,
    ) -> CoreFuture<'a, CoreOutcome> {
        Box::pin(async move {
            if let Some(text) = input.user_text.clone() {
                lock(&self.seen_text)
                    .map_err(|e| runtime_error(&e))?
                    .push(text);
            }
            let step = lock(&self.script)
                .map_err(|e| runtime_error(&e))?
                .pop_front()
                .ok_or_else(|| runtime_error("script exhausted"))?;
            let turn_id = TurnId::new();
            let started = CoreEvent::Turn(TurnEvent::TurnStarted {
                turn_id: turn_id.clone(),
                thread_id: ThreadId::new(),
            });
            match step {
                Step::Complete => {
                    let _ = events.send(started);
                    let _ = events.send(CoreEvent::Turn(TurnEvent::TurnCompleted {
                        turn_id,
                        usage: None,
                    }));
                    Ok(CoreOutcome::Completed)
                }
                Step::WaitForCancel => {
                    let _ = events.send(started);
                    input.control.cancel_token().cancelled().await;
                    Ok(CoreOutcome::Cancelled {
                        reason: input
                            .control
                            .cancel_token()
                            .reason()
                            .unwrap_or(CancelReason::User),
                    })
                }
                Step::Outcome(outcome) => Ok(outcome),
                Step::Error(error) => Err(error),
            }
        })
    }

    fn resume_after_approval<'a>(
        &'a self,
        _session_id: &'a SessionId,
        _actor: ApprovalActor,
        _resolution: ApprovalResolution,
        _events: CoreEvents,
    ) -> CoreFuture<'a, CoreOutcome> {
        Box::pin(async { Err(runtime_error("not used in turn tests")) })
    }

    fn parked_approval<'a>(
        &'a self,
        _session_id: &'a SessionId,
    ) -> CoreFuture<'a, Option<ParkedApproval>> {
        Box::pin(async { Ok(None) })
    }

    fn apply_setting<'a>(
        &'a self,
        _session_id: &'a SessionId,
        setting: Setting,
    ) -> CoreFuture<'a, ()> {
        Box::pin(async move {
            if let Setting::Mode(mode) = &setting
                && mode == "bogus"
            {
                return Err(DriverBridgeError::InvalidSetting("unknown mode".to_owned()));
            }
            lock(&self.settings)
                .map_err(|e| runtime_error(&e))?
                .push(setting);
            Ok(())
        })
    }

    fn model_name(&self, _session_id: &SessionId) -> Option<String> {
        Some("fake-model".to_owned())
    }
}

// --- harness ---------------------------------------------------------------

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<DriverEvent>>,
}

impl EventSink for RecordingSink {
    fn emit(&self, event: DriverEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }
}

impl RecordingSink {
    fn snapshot(&self) -> Result<Vec<DriverEvent>, String> {
        Ok(lock(&self.events)?.clone())
    }

    async fn wait_for(&self, what: &str, pred: impl Fn(&DriverEvent) -> bool) -> TestResult {
        for _ in 0..400 {
            if self.snapshot()?.iter().any(&pred) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Err(format!("timed out waiting for {what}").into())
    }
}

fn input(session: &SessionId, text: &str) -> TurnInput {
    TurnInput {
        session_id: session.clone(),
        text: text.to_owned(),
        client_msg_id: "msg-1".to_owned(),
        submitted_by: "tester".to_owned(),
        actor: ApprovalActor::Operator {
            id: "op".to_owned(),
        },
        origin: ConnectionId::next(),
    }
}

fn driver_over(core: Arc<FakeCore>) -> Result<(Arc<CoreTurnDriver>, tempfile::TempDir), String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let driver = CoreTurnDriver::with_runtime(CoreDriverConfig::new(dir.path().to_path_buf()), core)
        .map_err(|e| e.to_string())?;
    Ok((Arc::new(driver), dir))
}

fn cancel_pair() -> (watch::Sender<bool>, CancelSignal) {
    watch::channel(false)
}

// --- tests -----------------------------------------------------------------

#[tokio::test]
async fn normal_turn_streams_events_and_completes() -> TestResult {
    let core = FakeCore::scripted(vec![Step::Complete]);
    let (driver, _dir) = driver_over(Arc::clone(&core))?;
    let session = SessionId::new();
    let sink = Arc::new(RecordingSink::default());
    let (_tx, rx) = cancel_pair();

    driver.create_session(&session, Some("title")).await?;
    let outcome = driver
        .run_turn(input(&session, "hello"), rx, sink.clone())
        .await?;
    ensure!(outcome == TurnOutcome::Completed, "outcome {outcome:?}");

    let events = sink.snapshot()?;
    ensure!(events.len() == 3, "expected 3 events, got {events:?}");
    ensure!(
        matches!(events[0], DriverEvent::Turn(TurnEvent::TurnStarted { .. })),
        "first event {:?}",
        events[0]
    );
    ensure!(
        matches!(events[1], DriverEvent::Turn(TurnEvent::TurnCompleted { .. })),
        "second event {:?}",
        events[1]
    );
    ensure!(
        matches!(events[2], DriverEvent::DurableAdvanced),
        "last event {:?}",
        events[2]
    );
    ensure!(
        *lock(&core.seen_text)? == vec!["hello".to_owned()],
        "core saw the user text"
    );
    ensure!(
        *lock(&core.opened)? == vec![(session.as_str().to_owned(), Some("title".to_owned()))],
        "session opened with its title"
    );
    Ok(())
}

#[tokio::test]
async fn cancel_mid_turn_interrupts_through_the_core_token() -> TestResult {
    let core = FakeCore::scripted(vec![Step::WaitForCancel]);
    let (driver, _dir) = driver_over(core)?;
    let session = SessionId::new();
    let sink = Arc::new(RecordingSink::default());
    let (tx, rx) = cancel_pair();

    let task = {
        let driver = Arc::clone(&driver);
        let sink = Arc::clone(&sink);
        let input = input(&session, "long");
        tokio::spawn(async move { driver.run_turn(input, rx, sink).await })
    };
    sink.wait_for("turn start", |event| {
        matches!(event, DriverEvent::Turn(TurnEvent::TurnStarted { .. }))
    })
    .await?;
    ensure!(!task.is_finished(), "turn must still be running");
    tx.send(true)?;
    let outcome = tokio::time::timeout(Duration::from_secs(5), task).await???;
    ensure!(outcome == TurnOutcome::Interrupted, "outcome {outcome:?}");
    Ok(())
}

#[tokio::test]
async fn cancel_flipped_before_the_turn_starts_interrupts_immediately() -> TestResult {
    let core = FakeCore::scripted(vec![Step::WaitForCancel]);
    let (driver, _dir) = driver_over(core)?;
    let session = SessionId::new();
    let sink = Arc::new(RecordingSink::default());
    let (tx, rx) = cancel_pair();
    tx.send(true)?;

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        driver.run_turn(input(&session, "late"), rx, sink),
    )
    .await??;
    ensure!(outcome == TurnOutcome::Interrupted, "outcome {outcome:?}");
    Ok(())
}

#[tokio::test]
async fn a_dropped_cancel_sender_does_not_cancel_the_turn() -> TestResult {
    let core = FakeCore::scripted(vec![Step::Complete]);
    let (driver, _dir) = driver_over(core)?;
    let session = SessionId::new();
    let sink = Arc::new(RecordingSink::default());
    let (tx, rx) = cancel_pair();
    drop(tx);

    let outcome = driver
        .run_turn(input(&session, "hi"), rx, sink)
        .await?;
    ensure!(outcome == TurnOutcome::Completed, "outcome {outcome:?}");
    Ok(())
}

#[tokio::test]
async fn core_errors_surface_as_typed_host_errors() -> TestResult {
    let core = FakeCore::scripted(vec![
        Step::Error(DriverBridgeError::Core("model unavailable".to_owned())),
        Step::Error(DriverBridgeError::Store("disk gone".to_owned())),
        Step::Error(DriverBridgeError::UnknownSession("s".to_owned())),
    ]);
    let (driver, _dir) = driver_over(core)?;
    let session = SessionId::new();

    for expect in ["driver", "storage", "not found"] {
        let (_tx, rx) = cancel_pair();
        let sink = Arc::new(RecordingSink::default());
        let result = driver.run_turn(input(&session, "x"), rx, sink).await;
        let Err(error) = result else {
            return Err("an erroring core must surface an error".into());
        };
        ensure!(
            error.to_string().starts_with(expect),
            "error {error} should start with {expect}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn model_level_failures_become_failed_outcomes() -> TestResult {
    let core = FakeCore::scripted(vec![
        Step::Outcome(CoreOutcome::Failed {
            reason: "budget".to_owned(),
        }),
        Step::Outcome(CoreOutcome::Truncated),
        Step::Outcome(CoreOutcome::Refused {
            detail: Some("policy".to_owned()),
        }),
    ]);
    let (driver, _dir) = driver_over(core)?;
    let session = SessionId::new();
    let mut seen = Vec::new();
    for _ in 0..3 {
        let (_tx, rx) = cancel_pair();
        let sink = Arc::new(RecordingSink::default());
        seen.push(driver.run_turn(input(&session, "x"), rx, sink).await?);
    }
    ensure!(
        seen[0] == TurnOutcome::Failed("budget".to_owned()),
        "failed: {:?}",
        seen[0]
    );
    ensure!(
        matches!(&seen[1], TurnOutcome::Failed(reason) if reason.contains("truncated")),
        "truncated: {:?}",
        seen[1]
    );
    ensure!(
        matches!(&seen[2], TurnOutcome::Failed(reason) if reason.contains("policy")),
        "refused: {:?}",
        seen[2]
    );
    Ok(())
}

#[tokio::test]
async fn settings_and_model_name_go_through_the_core() -> TestResult {
    let core = FakeCore::scripted(Vec::new());
    let (driver, _dir) = driver_over(Arc::clone(&core))?;
    let session = SessionId::new();

    driver
        .apply_setting(&session, Setting::Effort("high".to_owned()))
        .await?;
    ensure!(
        *lock(&core.settings)? == vec![Setting::Effort("high".to_owned())],
        "setting reached the core"
    );
    let Err(error) = driver
        .apply_setting(&session, Setting::Mode("bogus".to_owned()))
        .await
    else {
        return Err("an invalid setting must be refused".into());
    };
    ensure!(
        matches!(error, HostError::Protocol(_)),
        "invalid setting maps to Protocol, got {error:?}"
    );
    ensure!(
        driver.model_name(&session).as_deref() == Some("fake-model"),
        "model name comes from the core"
    );
    Ok(())
}

#[tokio::test]
async fn a_driver_without_a_composed_runtime_answers_typed_errors() -> TestResult {
    let dir = tempfile::tempdir()?;
    let driver = CoreTurnDriver::new(CoreDriverConfig::new(dir.path().to_path_buf()))?;
    let session = SessionId::new();
    let sink = Arc::new(RecordingSink::default());
    let (_tx, rx) = cancel_pair();

    let Err(error) = driver.run_turn(input(&session, "x"), rx, sink).await else {
        return Err("an uncomposed driver must not run turns".into());
    };
    ensure!(
        matches!(error, HostError::Driver(_)),
        "uncomposed turn error {error:?}"
    );
    let Err(error) = driver.create_session(&session, None).await else {
        return Err("an uncomposed driver must not create sessions".into());
    };
    ensure!(
        matches!(error, HostError::Driver(_)),
        "uncomposed create error {error:?}"
    );
    ensure!(driver.model_name(&session).is_none(), "no model name");
    Ok(())
}
