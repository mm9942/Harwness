//! The `TurnDriver` bridge over `harw-core` (S03).
//!
//! # Shape
//! ```text
//! SessionHost --TurnDriver--> CoreTurnDriver --CoreRuntime--> HarwCoreRuntime --> harw-core
//!                               |  events -> EventSink          (AgentSession, run_turn_durable,
//!                               |  cancel -> CancelToken         resume_after_approval)
//!                               |  approvals <-> ApprovalStore
//! ```
//!
//! [`CoreTurnDriver`] owns everything the host contract needs (event
//! mapping, cancel, outcome mapping, the approval round trip);
//! [`CoreRuntime`] is the narrow port to the core, so tests drive the driver
//! with a scripted fake core and production plugs in [`HarwCoreRuntime`].
//!
//! # Approvals
//! The host resolves an approval in its `ApprovalBackend` (first writer wins)
//! *before* it calls [`TurnDriver::resume_after_approval`]. The core's
//! `resume_after_approval_durable` would try to consume the request a second
//! time and fail with `ApprovalAlreadyResolved`, so the driver instead reads
//! the durable resolution record and resumes through the non-consuming
//! `resume_after_approval` entry point. That entry point does not record
//! approvals the resumed turn parks on again; the driver therefore issues the
//! missing durable record itself whenever a leg parks.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};

use harw_core::cancel::{CancelReason, CancelToken};
use harw_core::mode::InteractionMode;
use harw_core::turn_loop::TurnControl;
use harw_core::{
    AgentSession, ApprovalResolution, ModelProvider, StateStore, TurnInput as CoreTurnInput,
    TurnOutcome as CoreOutcome, resume_after_approval, run_turn_durable,
};
use harw_protocol::approvals::ApprovalKind;
use harw_protocol::{ApprovalRequest, SessionEvent, TurnEvent};
use harw_session_host::HostError;
use harw_session_host::driver::{
    CancelSignal, DriverEvent, DriverFuture, EventSink, Setting, TurnDriver, TurnInput,
    TurnOutcome,
};
use harw_session_store::{
    ApprovalRecord, ApprovalResolutionRecord, ApprovalStore, SessionStoreError,
};
use harw_types::{
    ApprovalActor, ApprovalId, ItemId, ModelId, ReasoningEffort, ReviewDecision, RiskLevel,
    SessionId, ToolCallId, TurnId, WorkId,
};
use tokio::sync::mpsc;

use crate::DriverBridgeError;

/// What the bridge needs to assemble a core runtime.
#[derive(Clone, Debug)]
pub struct CoreDriverConfig {
    /// Session store root (transcripts, meta, approvals).
    pub sessions_root: PathBuf,
}

impl CoreDriverConfig {
    /// Config for `sessions_root`.
    #[must_use]
    pub fn new(sessions_root: PathBuf) -> Self {
        Self { sessions_root }
    }
}

// ---------------------------------------------------------------------------
// Port to the core
// ---------------------------------------------------------------------------

/// Boxed future of a core call.
pub type CoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DriverBridgeError>> + Send + 'a>>;

/// One event the core reported while a leg ran.
#[derive(Clone, Debug)]
pub enum CoreEvent {
    /// A turn event of the session.
    Turn(TurnEvent),
    /// A session lifecycle event.
    Session(SessionEvent),
}

/// Channel a [`CoreRuntime`] pushes its events into during one leg.
pub type CoreEvents = mpsc::UnboundedSender<CoreEvent>;

/// The approval a core session is parked on.
#[derive(Clone, Debug)]
pub struct ParkedApproval {
    /// Wire form shown to clients. The driver overrides `id` with the core's
    /// request id so durable record and wire request always agree.
    pub request: ApprovalRequest,
    /// The actor bound to the request (only it may resolve it).
    pub actor: ApprovalActor,
}

/// The agent runtime as the driver sees it.
///
/// [`HarwCoreRuntime`] implements it over `harw-core`; tests implement it
/// with a scripted fake. All calls are per session; the driver guarantees one
/// leg (turn or resume) at a time per session (host arbiter).
pub trait CoreRuntime: Send + Sync + 'static {
    /// Prepare the session (idempotent).
    fn open_session<'a>(
        &'a self,
        session_id: &'a SessionId,
        title: Option<&'a str>,
    ) -> CoreFuture<'a, ()>;

    /// Run one turn. `input.control` carries the cancel token: the runtime
    /// must observe it and return [`CoreOutcome::Cancelled`] when it flips.
    /// Events go to `events` while the turn runs.
    fn run_turn<'a>(
        &'a self,
        session_id: &'a SessionId,
        input: CoreTurnInput,
        events: CoreEvents,
    ) -> CoreFuture<'a, CoreOutcome>;

    /// Continue a parked turn with the (already durably resolved) decision.
    fn resume_after_approval<'a>(
        &'a self,
        session_id: &'a SessionId,
        actor: ApprovalActor,
        resolution: ApprovalResolution,
        events: CoreEvents,
    ) -> CoreFuture<'a, CoreOutcome>;

    /// The approval the session is parked on, if any.
    fn parked_approval<'a>(
        &'a self,
        session_id: &'a SessionId,
    ) -> CoreFuture<'a, Option<ParkedApproval>>;

    /// Apply a setting at a turn boundary.
    fn apply_setting<'a>(
        &'a self,
        session_id: &'a SessionId,
        setting: Setting,
    ) -> CoreFuture<'a, ()>;

    /// Model name for summaries, when known.
    fn model_name(&self, session_id: &SessionId) -> Option<String>;
}

/// Runtime of a driver built by [`CoreTurnDriver::new`]: nothing composed
/// yet, every call answers a typed error.
#[derive(Debug)]
struct UnconfiguredRuntime;

const UNCONFIGURED: &str =
    "no core runtime composed: build the driver with CoreTurnDriver::with_runtime/with_factory";

impl CoreRuntime for UnconfiguredRuntime {
    fn open_session<'a>(&'a self, _: &'a SessionId, _: Option<&'a str>) -> CoreFuture<'a, ()> {
        Box::pin(async { Err(DriverBridgeError::Runtime(UNCONFIGURED.to_owned())) })
    }

    fn run_turn<'a>(
        &'a self,
        _: &'a SessionId,
        _: CoreTurnInput,
        _: CoreEvents,
    ) -> CoreFuture<'a, CoreOutcome> {
        Box::pin(async { Err(DriverBridgeError::Runtime(UNCONFIGURED.to_owned())) })
    }

    fn resume_after_approval<'a>(
        &'a self,
        _: &'a SessionId,
        _: ApprovalActor,
        _: ApprovalResolution,
        _: CoreEvents,
    ) -> CoreFuture<'a, CoreOutcome> {
        Box::pin(async { Err(DriverBridgeError::Runtime(UNCONFIGURED.to_owned())) })
    }

    fn parked_approval<'a>(&'a self, _: &'a SessionId) -> CoreFuture<'a, Option<ParkedApproval>> {
        Box::pin(async { Err(DriverBridgeError::Runtime(UNCONFIGURED.to_owned())) })
    }

    fn apply_setting<'a>(&'a self, _: &'a SessionId, _: Setting) -> CoreFuture<'a, ()> {
        Box::pin(async { Err(DriverBridgeError::Runtime(UNCONFIGURED.to_owned())) })
    }

    fn model_name(&self, _: &SessionId) -> Option<String> {
        None
    }
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

/// Production [`TurnDriver`] wrapping the `harw-core` durable turn loop.
pub struct CoreTurnDriver {
    runtime: Arc<dyn CoreRuntime>,
    approvals: ApprovalStore,
    /// Control block of the leg that is (or was last) running per session;
    /// kept across an approval park so a resumed leg stays cancellable.
    controls: Mutex<HashMap<SessionId, TurnControl>>,
    /// Request id each parked session waits on (set when a leg parks).
    parked: Mutex<HashMap<SessionId, ItemId>>,
}

impl std::fmt::Debug for CoreTurnDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreTurnDriver").finish_non_exhaustive()
    }
}

impl CoreTurnDriver {
    /// Build the driver from `config`.
    ///
    /// The result has no core runtime composed: its calls answer a typed
    /// [`DriverBridgeError::Runtime`]. The gateway composition (S05) uses
    /// [`Self::with_factory`] (production) or [`Self::with_runtime`].
    ///
    /// # Errors
    /// [`DriverBridgeError::Runtime`] when `sessions_root` cannot be created.
    pub fn new(config: CoreDriverConfig) -> Result<Self, DriverBridgeError> {
        Self::with_runtime(config, Arc::new(UnconfiguredRuntime))
    }

    /// Build the driver over an explicit [`CoreRuntime`].
    ///
    /// # Errors
    /// [`DriverBridgeError::Runtime`] when `sessions_root` cannot be created.
    pub fn with_runtime(
        config: CoreDriverConfig,
        runtime: Arc<dyn CoreRuntime>,
    ) -> Result<Self, DriverBridgeError> {
        std::fs::create_dir_all(&config.sessions_root).map_err(|error| {
            DriverBridgeError::Runtime(format!(
                "sessions root {}: {error}",
                config.sessions_root.display()
            ))
        })?;
        Ok(Self {
            runtime,
            approvals: ApprovalStore::new(&config.sessions_root),
            controls: Mutex::new(HashMap::new()),
            parked: Mutex::new(HashMap::new()),
        })
    }

    /// Build the production driver: [`HarwCoreRuntime`] over `factory`.
    ///
    /// # Errors
    /// See [`Self::with_runtime`].
    pub fn with_factory(
        config: CoreDriverConfig,
        factory: Arc<dyn CoreSessionFactory>,
    ) -> Result<Self, DriverBridgeError> {
        let runtime = HarwCoreRuntime::new(factory, &config.sessions_root);
        Self::with_runtime(config, Arc::new(runtime))
    }

    fn controls(&self) -> Result<MutexGuard<'_, HashMap<SessionId, TurnControl>>, DriverBridgeError> {
        self.controls
            .lock()
            .map_err(|_| DriverBridgeError::Runtime("turn control table poisoned".to_owned()))
    }

    fn remember_control(
        &self,
        session: &SessionId,
        control: TurnControl,
    ) -> Result<(), DriverBridgeError> {
        self.controls()?.insert(session.clone(), control);
        Ok(())
    }

    /// Drop the per-session leg state (control block, parked request).
    fn forget_control(&self, session: &SessionId) {
        if let Ok(mut controls) = self.controls() {
            controls.remove(session);
        }
        if let Ok(mut parked) = self.parked.lock() {
            parked.remove(session);
        }
    }

    fn remember_parked(
        &self,
        session: &SessionId,
        request: &ItemId,
    ) -> Result<(), DriverBridgeError> {
        self.parked
            .lock()
            .map_err(|_| DriverBridgeError::Runtime("parked table poisoned".to_owned()))?
            .insert(session.clone(), request.clone());
        Ok(())
    }

    fn remembered_parked(&self, session: &SessionId) -> Option<ItemId> {
        self.parked.lock().ok()?.get(session).cloned()
    }

    /// Run one leg to its end, streaming events and honouring `cancel`.
    async fn pump(
        runtime_leg: CoreFuture<'_, CoreOutcome>,
        mut events: mpsc::UnboundedReceiver<CoreEvent>,
        cancel: &mut CancelSignal,
        token: &CancelToken,
        sink: &Arc<dyn EventSink>,
    ) -> Result<CoreOutcome, DriverBridgeError> {
        let mut leg = runtime_leg;
        let mut watching = true;
        let mut open = true;
        if *cancel.borrow() {
            token.cancel(CancelReason::User);
            watching = false;
        }
        let result = loop {
            tokio::select! {
                biased;
                event = events.recv(), if open => match event {
                    Some(event) => forward(sink, event),
                    None => open = false,
                },
                changed = cancel.changed(), if watching => {
                    watching = false;
                    // A dropped sender means "no cancel will ever come".
                    if changed.is_ok() && *cancel.borrow_and_update() {
                        token.cancel(CancelReason::User);
                    } else if changed.is_ok() {
                        watching = true;
                    }
                }
                result = &mut leg => break result,
            }
        };
        while let Ok(event) = events.try_recv() {
            forward(sink, event);
        }
        sink.emit(DriverEvent::DurableAdvanced);
        result
    }

    /// Map the core's outcome onto the host's, parking durably on approvals.
    async fn settle(
        &self,
        session: &SessionId,
        sink: &Arc<dyn EventSink>,
        result: Result<CoreOutcome, DriverBridgeError>,
    ) -> Result<TurnOutcome, DriverBridgeError> {
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                self.forget_control(session);
                return Err(error);
            }
        };
        match outcome {
            CoreOutcome::Completed => {
                self.forget_control(session);
                Ok(TurnOutcome::Completed)
            }
            CoreOutcome::Cancelled { .. } => {
                self.forget_control(session);
                Ok(TurnOutcome::Interrupted)
            }
            CoreOutcome::AwaitingApproval { call_id, request } => {
                match self.park(session, sink, &call_id, &request).await {
                    Ok(()) => Ok(TurnOutcome::AwaitingApproval),
                    Err(error) => {
                        self.forget_control(session);
                        Err(error)
                    }
                }
            }
            CoreOutcome::AwaitingChild { role, .. } => {
                self.forget_control(session);
                Ok(TurnOutcome::Failed(format!(
                    "turn parked on a child handoff to '{role}', which the session host does not resume"
                )))
            }
            CoreOutcome::Truncated => {
                self.forget_control(session);
                Ok(TurnOutcome::Failed("model output was truncated".to_owned()))
            }
            CoreOutcome::Refused { detail } => {
                self.forget_control(session);
                Ok(TurnOutcome::Failed(match detail {
                    Some(detail) => format!("model refused: {detail}"),
                    None => "model refused".to_owned(),
                }))
            }
            CoreOutcome::Failed { reason } => {
                self.forget_control(session);
                Ok(TurnOutcome::Failed(reason))
            }
        }
    }

    /// Make the parked approval durable and announce it to the host.
    async fn park(
        &self,
        session: &SessionId,
        sink: &Arc<dyn EventSink>,
        call_id: &ToolCallId,
        request: &ItemId,
    ) -> Result<(), DriverBridgeError> {
        let parked = self
            .runtime
            .parked_approval(session)
            .await?
            .ok_or_else(|| {
                DriverBridgeError::Approval(
                    "core reported a parked approval but holds no pending request".to_owned(),
                )
            })?;
        let mut wire = parked.request;
        wire.id = ApprovalId::from_str(request.as_str());
        match self.approvals.pending(session, request) {
            Ok(_) => {}
            Err(SessionStoreError::ApprovalNotFound { .. }) => {
                self.approvals
                    .issue(&ApprovalRecord {
                        request: request.clone(),
                        session: session.clone(),
                        call_id: call_id.clone(),
                        actor: parked.actor,
                        issued_at: wire.requested_at,
                        tenant: None,
                    })
                    .map_err(|error| DriverBridgeError::Store(error.to_string()))?;
            }
            Err(error) => return Err(DriverBridgeError::Store(error.to_string())),
        }
        self.remember_parked(session, request)?;
        sink.emit(DriverEvent::ApprovalRequested(wire));
        Ok(())
    }

    /// The durable decision of the approval `session` is parked on.
    async fn resolved_decision(
        &self,
        session: &SessionId,
    ) -> Result<ApprovalResolutionRecord, DriverBridgeError> {
        let request = match self.remembered_parked(session) {
            Some(request) => request,
            // Host restart between park and resume: ask the core.
            None => {
                let parked = self.runtime.parked_approval(session).await?.ok_or_else(|| {
                    DriverBridgeError::Approval("session is not parked on an approval".to_owned())
                })?;
                ItemId::from_str(parked.request.id.as_str())
            }
        };
        self.approvals
            .resolution(session, &request)
            .map_err(|error| DriverBridgeError::Store(error.to_string()))?
            .ok_or_else(|| {
                DriverBridgeError::Approval(format!(
                    "approval {} is not resolved yet",
                    request.as_str()
                ))
            })
    }
}

fn forward(sink: &Arc<dyn EventSink>, event: CoreEvent) {
    sink.emit(match event {
        CoreEvent::Turn(event) => DriverEvent::Turn(event),
        CoreEvent::Session(event) => DriverEvent::Session(event),
    });
}

fn resolution_of(record: &ApprovalResolutionRecord) -> ApprovalResolution {
    match record.decision {
        ReviewDecision::Approved | ReviewDecision::ApprovedOnce => ApprovalResolution::Approve,
        ReviewDecision::Rejected => ApprovalResolution::Reject {
            reason: record
                .comment
                .clone()
                .unwrap_or_else(|| "rejected by user".to_owned()),
        },
    }
}

impl TurnDriver for CoreTurnDriver {
    fn create_session(&self, session_id: &SessionId, title: Option<&str>) -> DriverFuture<'_, ()> {
        let session_id = session_id.clone();
        let title = title.map(str::to_owned);
        Box::pin(async move {
            self.runtime
                .open_session(&session_id, title.as_deref())
                .await
                .map_err(HostError::from)
        })
    }

    fn run_turn(
        &self,
        input: TurnInput,
        mut cancel: CancelSignal,
        sink: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        Box::pin(async move {
            tracing::debug!(
                session = %input.session_id.as_str(),
                client_msg_id = %input.client_msg_id,
                submitted_by = %input.submitted_by,
                "session driver: turn start"
            );
            let control = TurnControl::new();
            let token = control.cancel_token().clone();
            self.remember_control(&input.session_id, control.clone())?;
            let (tx, rx) = mpsc::unbounded_channel();
            let core_input = CoreTurnInput::user(input.text).with_control(control);
            let leg = self.runtime.run_turn(&input.session_id, core_input, tx);
            let result = Self::pump(leg, rx, &mut cancel, &token, &sink).await;
            self.settle(&input.session_id, &sink, result)
                .await
                .map_err(HostError::from)
        })
    }

    fn resume_after_approval(
        &self,
        session_id: &SessionId,
        mut cancel: CancelSignal,
        sink: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        let session_id = session_id.clone();
        Box::pin(async move {
            let record = self.resolved_decision(&session_id).await?;
            let resolution = resolution_of(&record);
            let token = match self.controls()?.get(&session_id) {
                Some(control) => control.cancel_token().clone(),
                // Host restart between park and resume: no live token.
                None => CancelToken::new(),
            };
            let (tx, rx) = mpsc::unbounded_channel();
            let leg = self
                .runtime
                .resume_after_approval(&session_id, record.actor, resolution, tx);
            let result = Self::pump(leg, rx, &mut cancel, &token, &sink).await;
            self.settle(&session_id, &sink, result)
                .await
                .map_err(HostError::from)
        })
    }

    fn apply_setting(&self, session_id: &SessionId, setting: Setting) -> DriverFuture<'_, ()> {
        let session_id = session_id.clone();
        Box::pin(async move {
            self.runtime
                .apply_setting(&session_id, setting)
                .await
                .map_err(HostError::from)
        })
    }

    fn model_name(&self, session_id: &SessionId) -> Option<String> {
        self.runtime.model_name(session_id)
    }
}

// ---------------------------------------------------------------------------
// Production runtime over harw-core
// ---------------------------------------------------------------------------

/// Channels a factory must wire into the session it builds.
#[derive(Debug)]
pub struct SessionWiring {
    /// Pass to `AgentSession::new_with_id` (session lifecycle events).
    pub event_tx: mpsc::UnboundedSender<SessionEvent>,
    /// Pass to `AgentSession::with_turn_event_sink` (turn events).
    pub turn_tx: mpsc::UnboundedSender<TurnEvent>,
}

/// One assembled core session.
pub struct CoreSession {
    /// The session, built with the id and [`SessionWiring`] it was given.
    pub session: AgentSession,
    /// Model provider for its turns.
    pub model: Arc<dyn ModelProvider>,
    /// Durable state store (transcript) for its turns.
    pub store: Arc<dyn StateStore>,
}

/// Gateway-supplied assembly of one core session (registry, sandbox,
/// approval actor in the spawn context, provider, transcript store).
///
/// The approval actor in the session's spawn context is what the durable
/// approval record binds; the responding connection's actor must match it.
pub trait CoreSessionFactory: Send + Sync + 'static {
    /// Build the session `session_id`.
    ///
    /// # Errors
    /// [`DriverBridgeError::Runtime`] when the session cannot be assembled.
    fn build(
        &self,
        session_id: &SessionId,
        title: Option<&str>,
        wiring: SessionWiring,
    ) -> Result<CoreSession, DriverBridgeError>;
}

struct Slot {
    core: CoreSession,
    session_events: mpsc::UnboundedReceiver<SessionEvent>,
    turn_events: mpsc::UnboundedReceiver<TurnEvent>,
}

/// [`CoreRuntime`] over `harw-core`: `run_turn_durable` for turns,
/// `resume_after_approval` for resumes.
pub struct HarwCoreRuntime {
    factory: Arc<dyn CoreSessionFactory>,
    approvals: ApprovalStore,
    slots: Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<Slot>>>>,
    models: Mutex<HashMap<SessionId, String>>,
}

impl std::fmt::Debug for HarwCoreRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarwCoreRuntime").finish_non_exhaustive()
    }
}

impl HarwCoreRuntime {
    /// Runtime over `factory`; approvals are recorded under `sessions_root`.
    #[must_use]
    pub fn new(factory: Arc<dyn CoreSessionFactory>, sessions_root: &std::path::Path) -> Self {
        Self {
            factory,
            approvals: ApprovalStore::new(sessions_root),
            slots: Mutex::new(HashMap::new()),
            models: Mutex::new(HashMap::new()),
        }
    }

    fn slots(
        &self,
    ) -> Result<MutexGuard<'_, HashMap<SessionId, Arc<tokio::sync::Mutex<Slot>>>>, DriverBridgeError>
    {
        self.slots
            .lock()
            .map_err(|_| DriverBridgeError::Runtime("session table poisoned".to_owned()))
    }

    fn note_model(&self, session: &SessionId, model: Option<&ModelId>) {
        if let (Some(model), Ok(mut models)) = (model, self.models.lock()) {
            models.insert(session.clone(), model.as_str().to_owned());
        }
    }

    /// The slot of `session_id`, built and hydrated on first use (also after
    /// a host restart, when only the durable transcript survived).
    async fn slot(
        &self,
        session_id: &SessionId,
        title: Option<&str>,
    ) -> Result<Arc<tokio::sync::Mutex<Slot>>, DriverBridgeError> {
        if let Some(slot) = self.slots()?.get(session_id) {
            return Ok(Arc::clone(slot));
        }
        let (event_tx, session_events) = mpsc::unbounded_channel();
        let (turn_tx, turn_events) = mpsc::unbounded_channel();
        let mut core = self
            .factory
            .build(session_id, title, SessionWiring { event_tx, turn_tx })?;
        core.session
            .hydrate_from_store(core.store.as_ref())
            .await
            .map_err(|error| DriverBridgeError::Core(format!("hydrate: {error}")))?;
        self.note_model(session_id, core.session.active_model());
        let slot = Arc::new(tokio::sync::Mutex::new(Slot {
            core,
            session_events,
            turn_events,
        }));
        Ok(Arc::clone(
            self.slots()?
                .entry(session_id.clone())
                .or_insert_with(|| slot),
        ))
    }

    async fn existing_slot(
        &self,
        session_id: &SessionId,
    ) -> Result<Arc<tokio::sync::Mutex<Slot>>, DriverBridgeError> {
        self.slot(session_id, None).await
    }
}

/// Drive `leg` while forwarding the session's event channels into `events`.
async fn drive<F>(
    leg: F,
    session_events: &mut mpsc::UnboundedReceiver<SessionEvent>,
    turn_events: &mut mpsc::UnboundedReceiver<TurnEvent>,
    events: &CoreEvents,
) -> F::Output
where
    F: Future,
{
    let mut leg = std::pin::pin!(leg);
    let output = loop {
        tokio::select! {
            biased;
            Some(event) = turn_events.recv() => {
                let _ = events.send(CoreEvent::Turn(event));
            }
            Some(event) = session_events.recv() => {
                let _ = events.send(CoreEvent::Session(event));
            }
            output = &mut leg => break output,
        }
    };
    while let Ok(event) = turn_events.try_recv() {
        let _ = events.send(CoreEvent::Turn(event));
    }
    while let Ok(event) = session_events.try_recv() {
        let _ = events.send(CoreEvent::Session(event));
    }
    output
}

impl CoreRuntime for HarwCoreRuntime {
    fn open_session<'a>(
        &'a self,
        session_id: &'a SessionId,
        title: Option<&'a str>,
    ) -> CoreFuture<'a, ()> {
        Box::pin(async move { self.slot(session_id, title).await.map(|_| ()) })
    }

    fn run_turn<'a>(
        &'a self,
        session_id: &'a SessionId,
        input: CoreTurnInput,
        events: CoreEvents,
    ) -> CoreFuture<'a, CoreOutcome> {
        Box::pin(async move {
            let slot = self.existing_slot(session_id).await?;
            let mut guard = slot.lock().await;
            let Slot {
                core,
                session_events,
                turn_events,
            } = &mut *guard;
            let leg = run_turn_durable(
                &mut core.session,
                core.model.as_ref(),
                core.store.as_ref(),
                &self.approvals,
                input,
            );
            drive(leg, session_events, turn_events, &events)
                .await
                .map_err(|error| DriverBridgeError::Core(error.to_string()))
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
            let slot = self.existing_slot(session_id).await?;
            let mut guard = slot.lock().await;
            let Slot {
                core,
                session_events,
                turn_events,
            } = &mut *guard;
            // Non-consuming entry point: the host's backend already resolved
            // the durable request (see the module docs).
            let leg = resume_after_approval(
                &mut core.session,
                core.model.as_ref(),
                core.store.as_ref(),
                actor,
                resolution,
            );
            drive(leg, session_events, turn_events, &events)
                .await
                .map_err(|error| DriverBridgeError::Core(error.to_string()))
        })
    }

    fn parked_approval<'a>(
        &'a self,
        session_id: &'a SessionId,
    ) -> CoreFuture<'a, Option<ParkedApproval>> {
        Box::pin(async move {
            let slot = self.existing_slot(session_id).await?;
            let guard = slot.lock().await;
            let session = &guard.core.session;
            let Some(pending) = session.pending_approval() else {
                return Ok(None);
            };
            let tool = pending.call.name.as_str().to_owned();
            let turn_id = session.current_turn().cloned().unwrap_or_else(TurnId::new);
            Ok(Some(ParkedApproval {
                request: ApprovalRequest {
                    id: ApprovalId::from_str(pending.request.as_str()),
                    work_id: WorkId::new(),
                    kind: ApprovalKind::DynamicTool {
                        turn_id,
                        tool_name: tool.clone(),
                        arguments: pending.call.arguments.clone(),
                    },
                    summary: format!("tool '{tool}' requires approval"),
                    risk: RiskLevel::Medium,
                    requested_at: pending.requested_at,
                    timeout_at: pending.timeout_at,
                    decisions: vec![ReviewDecision::Approved, ReviewDecision::Rejected],
                },
                actor: pending.actor.clone(),
            }))
        })
    }

    fn apply_setting<'a>(
        &'a self,
        session_id: &'a SessionId,
        setting: Setting,
    ) -> CoreFuture<'a, ()> {
        Box::pin(async move {
            let slot = self.existing_slot(session_id).await?;
            let mut guard = slot.lock().await;
            let session = &mut guard.core.session;
            match setting {
                Setting::Model(name) => {
                    let model = ModelId::try_from_str(name.as_str())
                        .map_err(|error| DriverBridgeError::InvalidSetting(error.to_string()))?;
                    session.set_active_model(Some(model.clone()));
                    self.note_model(session_id, Some(&model));
                }
                Setting::Mode(name) => {
                    let mode = InteractionMode::parse(&name).ok_or_else(|| {
                        DriverBridgeError::InvalidSetting(format!("unknown mode '{name}'"))
                    })?;
                    session.set_mode(mode);
                }
                Setting::Effort(name) => {
                    let effort = name.parse::<ReasoningEffort>().map_err(|_| {
                        DriverBridgeError::InvalidSetting(format!("unknown effort '{name}'"))
                    })?;
                    session.set_reasoning_effort(Some(effort));
                }
            }
            Ok(())
        })
    }

    fn model_name(&self, session_id: &SessionId) -> Option<String> {
        self.models.lock().ok()?.get(session_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfigured_constructor_builds_and_answers_typed_errors() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let root = dir.path().to_path_buf();
        let driver = CoreTurnDriver::new(CoreDriverConfig::new(root)).map_err(|e| e.to_string())?;
        assert_eq!(driver.model_name(&SessionId::new()), None);
        Ok(())
    }
}
