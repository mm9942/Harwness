//! Approval round trip of `CoreTurnDriver` against a scripted fake core and a
//! real durable `ApprovalStore`: park, durable record, host-side resolution
//! (granted / denied), resume with the stored decision, re-park, cancel of a
//! resumed leg. Panic-free: every check returns an `Err`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use harw_core::cancel::{CancelReason, CancelToken};
use harw_core::{ApprovalResolution, TurnInput as CoreTurnInput, TurnOutcome as CoreOutcome};
use harw_protocol::TurnEvent;
use harw_protocol::approvals::{ApprovalKind, ApprovalRequest};
use harw_session_driver::{
    CoreDriverConfig, CoreEvent, CoreEvents, CoreFuture, CoreRuntime, CoreTurnDriver,
    DriverBridgeError, ParkedApproval,
};
use harw_session_host::HostError;
use harw_session_host::driver::{
    CancelSignal, DriverEvent, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::identity::ConnectionId;
use harw_session_store::{ApprovalRecord, ApprovalStore, SessionStoreError};
use harw_types::{
    ApprovalActor, ApprovalId, Clock, ItemId, ReviewDecision, RiskLevel, SessionId, SystemClock,
    ThreadId, ToolCallId, TurnId, WorkId,
};
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

fn runtime_error(detail: &str) -> DriverBridgeError {
    DriverBridgeError::Runtime(detail.to_owned())
}

fn operator() -> ApprovalActor {
    ApprovalActor::Operator {
        id: "op".to_owned(),
    }
}

// --- fake core -------------------------------------------------------------

/// What the next leg (turn or resume) of the fake core does.
enum Leg {
    /// Park on an approval and record it durably (as `run_turn_durable` does).
    ParkDurable,
    /// Park without a durable record (as the non-consuming resume does).
    ParkUndurable,
    Complete,
    /// Announce itself, then wait for the cancel token of the turn.
    WaitForCancel,
}

struct FakeCore {
    store: ApprovalStore,
    script: Mutex<VecDeque<Leg>>,
    parked: Mutex<Option<ParkedApproval>>,
    last_request: Mutex<Option<ItemId>>,
    token: Mutex<Option<CancelToken>>,
    resumed: Mutex<Vec<(ApprovalActor, ApprovalResolution)>>,
}

impl FakeCore {
    fn new(store: ApprovalStore, legs: Vec<Leg>) -> Arc<Self> {
        Arc::new(Self {
            store,
            script: Mutex::new(legs.into()),
            parked: Mutex::new(None),
            last_request: Mutex::new(None),
            token: Mutex::new(None),
            resumed: Mutex::new(Vec::new()),
        })
    }

    fn last_request(&self) -> Result<ItemId, String> {
        lock(&self.last_request)?
            .clone()
            .ok_or_else(|| "core never parked".to_owned())
    }

    fn next_leg(&self) -> Result<Leg, DriverBridgeError> {
        lock(&self.script)
            .map_err(|e| runtime_error(&e))?
            .pop_front()
            .ok_or_else(|| runtime_error("script exhausted"))
    }

    async fn run_leg(
        &self,
        session: &SessionId,
        events: &CoreEvents,
        leg: Leg,
    ) -> Result<CoreOutcome, DriverBridgeError> {
        let durable = matches!(leg, Leg::ParkDurable);
        match leg {
            Leg::Complete => Ok(CoreOutcome::Completed),
            Leg::WaitForCancel => {
                let _ = events.send(CoreEvent::Turn(TurnEvent::TurnStarted {
                    turn_id: TurnId::new(),
                    thread_id: ThreadId::new(),
                }));
                let token = lock(&self.token)
                    .map_err(|e| runtime_error(&e))?
                    .clone()
                    .ok_or_else(|| runtime_error("no turn token"))?;
                token.cancelled().await;
                Ok(CoreOutcome::Cancelled {
                    reason: token.reason().unwrap_or(CancelReason::User),
                })
            }
            Leg::ParkDurable | Leg::ParkUndurable => {
                let request = ItemId::new();
                let call_id = ToolCallId::new();
                let now = SystemClock.now();
                let wire = ApprovalRequest {
                    // Deliberately not the core's request id: the driver
                    // must align the wire id with the durable one.
                    id: ApprovalId::new(),
                    work_id: WorkId::new(),
                    kind: ApprovalKind::Exec {
                        turn_id: TurnId::new(),
                        command: vec!["rm".to_owned(), "-rf".to_owned(), "build/".to_owned()],
                        cwd: "/workspace".to_owned(),
                        reasoning: None,
                    },
                    summary: "rm -rf build/".to_owned(),
                    risk: RiskLevel::Medium,
                    requested_at: now,
                    timeout_at: ApprovalRequest::default_timeout_at(now),
                    decisions: vec![ReviewDecision::Approved, ReviewDecision::Rejected],
                };
                if durable {
                    self.store
                        .issue(&ApprovalRecord {
                            request: request.clone(),
                            session: session.clone(),
                            call_id: call_id.clone(),
                            actor: operator(),
                            issued_at: now,
                            tenant: None,
                        })
                        .map_err(|e| DriverBridgeError::Store(e.to_string()))?;
                }
                *lock(&self.parked).map_err(|e| runtime_error(&e))? = Some(ParkedApproval {
                    request: wire,
                    actor: operator(),
                });
                *lock(&self.last_request).map_err(|e| runtime_error(&e))? = Some(request.clone());
                Ok(CoreOutcome::AwaitingApproval { call_id, request })
            }
        }
    }
}

impl CoreRuntime for FakeCore {
    fn open_session<'a>(&'a self, _: &'a SessionId, _: Option<&'a str>) -> CoreFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn run_turn<'a>(
        &'a self,
        session_id: &'a SessionId,
        input: CoreTurnInput,
        events: CoreEvents,
    ) -> CoreFuture<'a, CoreOutcome> {
        Box::pin(async move {
            // The core keeps the turn's control block across the park.
            *lock(&self.token).map_err(|e| runtime_error(&e))? =
                Some(input.control.cancel_token().clone());
            let leg = self.next_leg()?;
            self.run_leg(session_id, &events, leg).await
        })
    }

    fn resume_after_approval<'a>(
        &'a self,
        session_id: &'a SessionId,
        actor: ApprovalActor,
        resolution: ApprovalResolution,
        events: CoreEvents,
    ) -> CoreFuture<'a, CoreOutcome> {
        Box::pin(async move {
            lock(&self.resumed)
                .map_err(|e| runtime_error(&e))?
                .push((actor, resolution));
            // The core takes the pending approval when it resumes.
            *lock(&self.parked).map_err(|e| runtime_error(&e))? = None;
            let leg = self.next_leg()?;
            self.run_leg(session_id, &events, leg).await
        })
    }

    fn parked_approval<'a>(
        &'a self,
        _: &'a SessionId,
    ) -> CoreFuture<'a, Option<ParkedApproval>> {
        Box::pin(async move { Ok(lock(&self.parked).map_err(|e| runtime_error(&e))?.clone()) })
    }

    fn apply_setting<'a>(&'a self, _: &'a SessionId, _: Setting) -> CoreFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn model_name(&self, _: &SessionId) -> Option<String> {
        None
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

    fn approval_requests(&self) -> Result<Vec<ApprovalRequest>, String> {
        Ok(self
            .snapshot()?
            .into_iter()
            .filter_map(|event| match event {
                DriverEvent::ApprovalRequested(request) => Some(request),
                _ => None,
            })
            .collect())
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

struct Rig {
    driver: Arc<CoreTurnDriver>,
    core: Arc<FakeCore>,
    /// Host-side view of the same durable store (what `DurableApprovals`
    /// resolves through).
    store: ApprovalStore,
    session: SessionId,
    sink: Arc<RecordingSink>,
    _dir: tempfile::TempDir,
}

fn rig(legs: Vec<Leg>) -> Result<Rig, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let store = ApprovalStore::new(dir.path());
    let core = FakeCore::new(ApprovalStore::new(dir.path()), legs);
    let driver = CoreTurnDriver::with_runtime(
        CoreDriverConfig::new(dir.path().to_path_buf()),
        Arc::clone(&core) as Arc<dyn CoreRuntime>,
    )
    .map_err(|e| e.to_string())?;
    Ok(Rig {
        driver: Arc::new(driver),
        core,
        store,
        session: SessionId::new(),
        sink: Arc::new(RecordingSink::default()),
        _dir: dir,
    })
}

fn cancel_pair() -> (watch::Sender<bool>, CancelSignal) {
    watch::channel(false)
}

impl Rig {
    fn input(&self) -> TurnInput {
        TurnInput {
            session_id: self.session.clone(),
            text: "do it".to_owned(),
            client_msg_id: "msg-1".to_owned(),
            submitted_by: "tester".to_owned(),
            actor: operator(),
            origin: ConnectionId::next(),
        }
    }

    async fn park(&self) -> Result<ItemId, Box<dyn std::error::Error>> {
        let (_tx, rx) = cancel_pair();
        let outcome = self
            .driver
            .run_turn(self.input(), rx, self.sink.clone())
            .await?;
        ensure!(
            outcome == TurnOutcome::AwaitingApproval,
            "expected a park, got {outcome:?}"
        );
        Ok(self.core.last_request()?)
    }

    fn host_resolve(
        &self,
        request: &ItemId,
        decision: ReviewDecision,
        comment: Option<&str>,
    ) -> Result<(), SessionStoreError> {
        self.store
            .resolve(
                &self.session,
                request,
                decision,
                comment.map(str::to_owned),
                &operator(),
                &SystemClock,
            )
            .map(|_| ())
    }

    async fn resume(&self) -> Result<TurnOutcome, HostError> {
        let (_tx, rx) = cancel_pair();
        self.driver
            .resume_after_approval(&self.session, rx, self.sink.clone())
            .await
    }
}

// --- tests -----------------------------------------------------------------

#[tokio::test]
async fn approval_granted_round_trip_resumes_the_turn() -> TestResult {
    let rig = rig(vec![Leg::ParkDurable, Leg::Complete])?;
    let request = rig.park().await?;

    // The host got the request frame, aligned with the durable id, and the
    // durable record exists for the host's backend to resolve.
    let announced = rig.sink.approval_requests()?;
    ensure!(announced.len() == 1, "one request frame, got {announced:?}");
    ensure!(
        announced[0].id.as_str() == request.as_str(),
        "wire id {} must equal durable id {}",
        announced[0].id.as_str(),
        request.as_str()
    );
    ensure!(
        rig.store.pending(&rig.session, &request).is_ok(),
        "durable pending record exists"
    );

    rig.host_resolve(&request, ReviewDecision::Approved, None)?;
    let outcome = rig.resume().await?;
    ensure!(outcome == TurnOutcome::Completed, "outcome {outcome:?}");

    let resumed = lock(&rig.core.resumed)?.clone();
    ensure!(
        resumed == vec![(operator(), ApprovalResolution::Approve)],
        "core resumed with {resumed:?}"
    );
    Ok(())
}

#[tokio::test]
async fn approval_denied_resumes_with_the_recorded_reason() -> TestResult {
    let rig = rig(vec![Leg::ParkDurable, Leg::Complete])?;
    let request = rig.park().await?;
    rig.host_resolve(&request, ReviewDecision::Rejected, Some("too risky"))?;

    let outcome = rig.resume().await?;
    ensure!(outcome == TurnOutcome::Completed, "outcome {outcome:?}");
    let resumed = lock(&rig.core.resumed)?.clone();
    ensure!(
        resumed
            == vec![(
                operator(),
                ApprovalResolution::Reject {
                    reason: "too risky".to_owned()
                }
            )],
        "core resumed with {resumed:?}"
    );
    Ok(())
}

#[tokio::test]
async fn denial_without_a_comment_gets_the_default_reason() -> TestResult {
    let rig = rig(vec![Leg::ParkDurable, Leg::Complete])?;
    let request = rig.park().await?;
    rig.host_resolve(&request, ReviewDecision::Rejected, None)?;

    rig.resume().await?;
    let resumed = lock(&rig.core.resumed)?.clone();
    ensure!(
        resumed
            == vec![(
                operator(),
                ApprovalResolution::Reject {
                    reason: "rejected by user".to_owned()
                }
            )],
        "core resumed with {resumed:?}"
    );
    Ok(())
}

#[tokio::test]
async fn approved_once_counts_as_a_grant() -> TestResult {
    let rig = rig(vec![Leg::ParkDurable, Leg::Complete])?;
    let request = rig.park().await?;
    rig.host_resolve(&request, ReviewDecision::ApprovedOnce, None)?;

    rig.resume().await?;
    let resumed = lock(&rig.core.resumed)?.clone();
    ensure!(
        resumed == vec![(operator(), ApprovalResolution::Approve)],
        "core resumed with {resumed:?}"
    );
    Ok(())
}

#[tokio::test]
async fn resume_before_the_host_resolved_is_refused_and_stays_parked() -> TestResult {
    let rig = rig(vec![Leg::ParkDurable, Leg::Complete])?;
    let request = rig.park().await?;

    let Err(error) = rig.resume().await else {
        return Err("an unresolved approval must not resume".into());
    };
    ensure!(
        matches!(&error, HostError::Driver(detail) if detail.contains("not resolved")),
        "unexpected error {error:?}"
    );
    ensure!(
        lock(&rig.core.resumed)?.is_empty(),
        "the core must not have been resumed"
    );

    // Still parked: once the host resolves, the resume goes through.
    rig.host_resolve(&request, ReviewDecision::Approved, None)?;
    let outcome = rig.resume().await?;
    ensure!(outcome == TurnOutcome::Completed, "outcome {outcome:?}");
    Ok(())
}

#[tokio::test]
async fn resume_without_a_parked_turn_is_a_typed_error() -> TestResult {
    let rig = rig(vec![Leg::Complete])?;
    let Err(error) = rig.resume().await else {
        return Err("nothing is parked".into());
    };
    ensure!(
        matches!(&error, HostError::Driver(detail) if detail.contains("not parked")),
        "unexpected error {error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_second_park_after_resume_gets_its_durable_record_from_the_driver() -> TestResult {
    let rig = rig(vec![Leg::ParkDurable, Leg::ParkUndurable, Leg::Complete])?;
    let first = rig.park().await?;
    rig.host_resolve(&first, ReviewDecision::Approved, None)?;

    let outcome = rig.resume().await?;
    ensure!(
        outcome == TurnOutcome::AwaitingApproval,
        "second park, got {outcome:?}"
    );
    let second = rig.core.last_request()?;
    ensure!(second != first, "a new request id");
    ensure!(
        rig.store.pending(&rig.session, &second).is_ok(),
        "the driver issued the durable record the core skipped"
    );
    let announced = rig.sink.approval_requests()?;
    ensure!(announced.len() == 2, "two request frames, got {announced:?}");
    ensure!(
        announced[1].id.as_str() == second.as_str(),
        "second wire id matches the durable id"
    );

    rig.host_resolve(&second, ReviewDecision::Approved, None)?;
    let outcome = rig.resume().await?;
    ensure!(outcome == TurnOutcome::Completed, "outcome {outcome:?}");
    Ok(())
}

#[tokio::test]
async fn cancelling_a_resumed_leg_interrupts_it() -> TestResult {
    let rig = rig(vec![Leg::ParkDurable, Leg::WaitForCancel])?;
    let request = rig.park().await?;
    rig.host_resolve(&request, ReviewDecision::Approved, None)?;

    let (tx, rx) = cancel_pair();
    let task = {
        let driver = Arc::clone(&rig.driver);
        let sink = Arc::clone(&rig.sink);
        let session = rig.session.clone();
        tokio::spawn(async move { driver.resume_after_approval(&session, rx, sink).await })
    };
    rig.sink
        .wait_for("resumed leg start", |event| {
            matches!(event, DriverEvent::Turn(TurnEvent::TurnStarted { .. }))
        })
        .await?;
    tx.send(true)?;
    let outcome = tokio::time::timeout(Duration::from_secs(5), task).await???;
    ensure!(outcome == TurnOutcome::Interrupted, "outcome {outcome:?}");
    Ok(())
}

#[tokio::test]
async fn first_writer_wins_in_the_durable_backend() -> TestResult {
    let rig = rig(vec![Leg::ParkDurable, Leg::Complete])?;
    let request = rig.park().await?;
    rig.host_resolve(&request, ReviewDecision::Rejected, Some("first"))?;

    let late = rig.host_resolve(&request, ReviewDecision::Approved, None);
    ensure!(
        matches!(late, Err(SessionStoreError::ApprovalAlreadyResolved { .. })),
        "second writer must lose, got {late:?}"
    );
    rig.resume().await?;
    let resumed = lock(&rig.core.resumed)?.clone();
    ensure!(
        resumed
            == vec![(
                operator(),
                ApprovalResolution::Reject {
                    reason: "first".to_owned()
                }
            )],
        "the first decision is the one the core sees: {resumed:?}"
    );
    Ok(())
}
