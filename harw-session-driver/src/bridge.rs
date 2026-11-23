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
//!
//! When the core attributes the ask to the DoD taint of the turn (PL-100
//! decision O1), the parked request's `summary` carries that reason; the
//! wire shape of `ApprovalRequest` is unchanged. Surfaces that render
//! `summary` show the reason; `harw_session_client::ApprovalSummary` does not
//! carry `summary` yet, so remote SDK hosts miss it.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};

use harw_core::cancel::{CancelReason, CancelToken};
use harw_core::dod_taint::pending_ask_reason;
use harw_core::mode::InteractionMode;
use harw_core::turn_loop::TurnControl;
use harw_core::{
    AgentEvent, AgentEventKind, AgentSession, ApprovalResolution, ModelProvider, StateStore,
    TurnInput as CoreTurnInput, TurnOutcome as CoreOutcome, resume_after_approval,
    run_turn_durable,
};
use harw_protocol::approvals::ApprovalKind;
use harw_protocol::{AgentOrchestrationStatus, ApprovalRequest, SessionEvent, TurnEvent};
use harw_session_host::HostError;
use harw_session_host::driver::{
    CancelSignal, DriverEvent, DriverFuture, EventSink, ExecutionAdmission, LiveAgent, Setting,
    TurnDriver, TurnInput, TurnOutcome,
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
    /// Trusted host admission; production runtimes must use it in assembly.
    fn prepare_execution(
        &self,
        _session_id: &SessionId,
        _admission: ExecutionAdmission,
    ) -> Result<(), DriverBridgeError> {
        Err(DriverBridgeError::Runtime(
            "runtime does not support creator execution admission".into(),
        ))
    }

    /// Roll back an unpublished creation's process-local state.
    fn discard_unpublished_session(
        &self,
        _session_id: &SessionId,
    ) -> Result<(), DriverBridgeError> {
        Ok(())
    }

    /// Drop the cached core of an established session; the next leg hydrates from the transcript again. No tombstone, admission stays.
    fn forget_session(&self, _session_id: &SessionId) -> Result<(), DriverBridgeError> {
        Ok(())
    }

    /// Copy the history of `source` before its user message `before_item` into the new session `new`.
    fn fork_history<'a>(
        &'a self,
        _new: &'a SessionId,
        _source: &'a SessionId,
        _before_item: &'a str,
    ) -> CoreFuture<'a, ()> {
        Box::pin(async { Err(DriverBridgeError::Runtime("runtime does not support fork".into())) })
    }

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

    /// Destination for live events of the session's child agents and agent
    /// jobs, also between legs. The driver calls it at the start of every leg
    /// with that leg's sink; the runtime keeps using the most recent sink
    /// until the next call. Default: ignored.
    fn bind_live_sink(&self, _session_id: &SessionId, _sink: Arc<dyn EventSink>) {}

    /// Child agents admitted below any hosted session. Default: none.
    fn live_agents(&self) -> Vec<LiveAgent> {
        Vec::new()
    }

    /// Requests cancellation of a child agent below `session_id`; `true` when
    /// a running agent was signalled. Default: no agent known.
    fn stop_agent(&self, _session_id: &SessionId, _agent_id: &str) -> bool {
        false
    }
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
    /// Where prompt images are checked and stored; `None` = images refused.
    media: Option<Arc<harw_media::MediaStore>>,
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
            media: None,
        })
    }

    /// Store prompt images (`turn.submit` `images`) in `store`. The caller
    /// installs the same store as the process media source so the provider
    /// can read the images back.
    #[must_use]
    pub fn with_media_store(mut self, store: Arc<harw_media::MediaStore>) -> Self {
        self.media = Some(store);
        self
    }

    /// Open (creating) the media store at `root` (`<home>/media`), install it
    /// as the process media source and use it for prompt images.
    ///
    /// # Errors
    /// [`DriverBridgeError::Runtime`] when the store cannot be opened.
    pub fn with_media_root(self, root: &std::path::Path) -> Result<Self, DriverBridgeError> {
        let store = Arc::new(harw_media::MediaStore::open(root).map_err(|error| {
            DriverBridgeError::Runtime(format!("media store {}: {error}", root.display()))
        })?);
        harw_core::install_media_source(Arc::clone(&store) as _);
        Ok(self.with_media_store(store))
    }

    /// Decode, check and store the images of a submission.
    fn ingest_images(
        &self,
        images: &[harw_protocol::session_wire::SubmitImage],
    ) -> Result<Vec<harw_protocol::MediaRef>, DriverBridgeError> {
        use base64::Engine as _;
        if images.is_empty() {
            return Ok(Vec::new());
        }
        let store = self.media.as_ref().ok_or_else(|| {
            DriverBridgeError::Runtime("this host has no media store for prompt images".into())
        })?;
        images
            .iter()
            .map(|image| {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(image.data_b64.as_bytes())
                    .map_err(|_| DriverBridgeError::Runtime("image is not valid base64".into()))?;
                store
                    .put(&bytes)
                    .map_err(|error| DriverBridgeError::Runtime(format!("image refused: {error}")))
            })
            .collect()
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

    fn controls(
        &self,
    ) -> Result<MutexGuard<'_, HashMap<SessionId, TurnControl>>, DriverBridgeError> {
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
                let parked = self
                    .runtime
                    .parked_approval(session)
                    .await?
                    .ok_or_else(|| {
                        DriverBridgeError::Approval(
                            "session is not parked on an approval".to_owned(),
                        )
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

/// Shared cell holding the sink of the most recent leg of one session.
type LiveSink = Arc<Mutex<Option<Arc<dyn EventSink>>>>;

/// Aborts the per-slot forwarder when its slot is dropped.
struct LiveForwarder(tokio::task::AbortHandle);

impl Drop for LiveForwarder {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Wire state name of an orchestration status; `None` for pure progress.
fn job_state(status: AgentOrchestrationStatus) -> Option<&'static str> {
    match status {
        AgentOrchestrationStatus::Progress => None,
        AgentOrchestrationStatus::Admitted => Some("admitted"),
        AgentOrchestrationStatus::Running => Some("running"),
        AgentOrchestrationStatus::Paused => Some("paused"),
        AgentOrchestrationStatus::Completed => Some("completed"),
        AgentOrchestrationStatus::Failed => Some("failed"),
        AgentOrchestrationStatus::Cancelled => Some("cancelled"),
    }
}

/// Maps a hub event of the session tree rooted at `root` to a live driver
/// event. Root turn events already arrive through the leg's channel.
fn live_driver_event(root: &SessionId, event: AgentEvent) -> Option<DriverEvent> {
    match event.kind {
        AgentEventKind::Turn(ev) if event.agent != *root => Some(DriverEvent::Child {
            agent: event.agent,
            parent: event.parent,
            role: event.role,
            event: ev,
        }),
        AgentEventKind::Turn(_) => None,
        AgentEventKind::Orchestration(o) => job_state(o.status).map(|state| DriverEvent::Job {
            work_id: o.child_session_id.as_str().to_owned(),
            kind: "agent".to_owned(),
            state: state.to_owned(),
            summary: Some(o.role),
        }),
        _ => None,
    }
}

/// Forwards hub events into the currently bound sink (dropped while none).
async fn forward_live(
    mut rx: tokio::sync::broadcast::Receiver<AgentEvent>,
    root: SessionId,
    sink: LiveSink,
) {
    loop {
        match rx.recv().await {
            Ok(event) => {
                if let Some(mapped) = live_driver_event(&root, event) {
                    let bound = sink.lock().ok().and_then(|cell| cell.clone());
                    if let Some(bound) = bound {
                        bound.emit(mapped);
                    }
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::debug!(skipped, "session driver: live agent events lagged");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Human-readable summary of a parked tool approval.
///
/// # Description
/// `reason` is the core's attribution of the ask, today only the DoD taint
/// (PL-100 decision O1, `harw_core::dod_taint::pending_ask_reason`, already
/// escaped). It is appended to the summary because the wire
/// `ApprovalRequest` denies unknown fields: older peers keep parsing the
/// request and simply show the longer text.
///
/// # Arguments
/// - `tool` (`&str`): the parked tool's name.
/// - `reason` (`Option<&str>`): the attributed reason, if any.
///
/// # Returns
/// `tool '<tool>' requires approval`, plus `; <reason>` when a reason exists.
fn approval_summary(tool: &str, reason: Option<&str>) -> String {
    match reason {
        Some(reason) => format!("tool '{tool}' requires approval; {reason}"),
        None => format!("tool '{tool}' requires approval"),
    }
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
    fn prepare_execution(
        &self,
        session_id: &SessionId,
        admission: ExecutionAdmission,
    ) -> DriverFuture<'_, ()> {
        let result = self
            .runtime
            .prepare_execution(session_id, admission)
            .map_err(HostError::from);
        Box::pin(async move { result })
    }

    fn discard_unpublished_session(&self, session_id: &SessionId) -> Result<(), HostError> {
        self.controls()?.remove(session_id);
        self.parked
            .lock()
            .map_err(|_| HostError::Driver("parked approval table poisoned".into()))?
            .remove(session_id);
        self.runtime
            .discard_unpublished_session(session_id)
            .map_err(HostError::from)
    }

    fn forget_session(&self, session_id: &SessionId) -> Result<(), HostError> {
        self.runtime
            .forget_session(session_id)
            .map_err(HostError::from)
    }

    fn fork_history(
        &self,
        new: &SessionId,
        source: &SessionId,
        before_item: &str,
    ) -> DriverFuture<'_, ()> {
        let (new, source, before) = (new.clone(), source.clone(), before_item.to_owned());
        Box::pin(async move {
            self.runtime
                .fork_history(&new, &source, &before)
                .await
                .map_err(HostError::from)
        })
    }

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
            let media = self.ingest_images(&input.images).map_err(HostError::from)?;
            let core_input = CoreTurnInput::user(input.text)
                .with_images(media)
                .with_rewind_before(input.rewind_before.clone())
                .with_control(control);
            self.runtime
                .bind_live_sink(&input.session_id, Arc::clone(&sink));
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
            // The core only accepts the actor bound when it parked; the
            // resolver (`record.actor`) stays in the durable record as audit.
            // Who may resolve is decided by the host (caps), not here.
            let bound = match self.runtime.parked_approval(&session_id).await? {
                Some(parked) => parked.actor,
                None => record.actor,
            };
            let (tx, rx) = mpsc::unbounded_channel();
            self.runtime.bind_live_sink(&session_id, Arc::clone(&sink));
            let leg = self
                .runtime
                .resume_after_approval(&session_id, bound, resolution, tx);
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

    fn live_agents(&self) -> Vec<LiveAgent> {
        self.runtime.live_agents()
    }

    fn stop_agent(&self, session_id: &SessionId, agent_id: &str) -> DriverFuture<'_, bool> {
        let stopped = self.runtime.stop_agent(session_id, agent_id);
        Box::pin(async move { Ok(stopped) })
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
    /// The session's child-agent spawner, when the assembly has one. Lets the
    /// driver list and stop child agents across sessions.
    pub spawner: Option<Arc<harw_core::ManagedAgentSpawner>>,
}

/// Gateway-supplied assembly of one core session (registry, sandbox,
/// approval actor in the spawn context, provider, transcript store).
///
/// The approval actor in the session's spawn context is what the durable
/// approval record binds; the responding connection's actor must match it.
pub trait CoreSessionFactory: Send + Sync + 'static {
    /// Build using the host's current, authenticated creator ceiling. An
    /// effectful factory must override this instead of discarding authority.
    fn build_with_execution(
        &self,
        _session_id: &SessionId,
        _title: Option<&str>,
        _wiring: SessionWiring,
        _admission: &ExecutionAdmission,
    ) -> Result<CoreSession, DriverBridgeError> {
        Err(DriverBridgeError::Runtime(
            "factory does not support creator execution admission".into(),
        ))
    }

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

/// Per-session slots, keyed by host session id.
type SlotMap = HashMap<SessionId, Arc<tokio::sync::Mutex<Slot>>>;

struct Slot {
    core: CoreSession,
    session_events: mpsc::UnboundedReceiver<SessionEvent>,
    turn_events: mpsc::UnboundedReceiver<TurnEvent>,
    _live: Option<LiveForwarder>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AdmissionState {
    generation: u64,
    admission: Option<ExecutionAdmission>,
}

/// [`CoreRuntime`] over `harw-core`: `run_turn_durable` for turns,
/// `resume_after_approval` for resumes.
pub struct HarwCoreRuntime {
    factory: Arc<dyn CoreSessionFactory>,
    admissions: Mutex<HashMap<SessionId, AdmissionState>>,
    approvals: ApprovalStore,
    slots: Mutex<SlotMap>,
    models: Mutex<HashMap<SessionId, String>>,
    live_sinks: Mutex<HashMap<SessionId, LiveSink>>,
    /// Child-agent spawner of every built session (global agent views).
    spawners: Mutex<HashMap<SessionId, Arc<harw_core::ManagedAgentSpawner>>>,
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
            admissions: Mutex::new(HashMap::new()),
            approvals: ApprovalStore::new(sessions_root),
            slots: Mutex::new(HashMap::new()),
            models: Mutex::new(HashMap::new()),
            live_sinks: Mutex::new(HashMap::new()),
            spawners: Mutex::new(HashMap::new()),
        }
    }

    fn note_spawner(&self, session: &SessionId, spawner: Option<Arc<harw_core::ManagedAgentSpawner>>) {
        if let Ok(mut spawners) = self.spawners.lock() {
            match spawner {
                Some(spawner) => {
                    spawners.insert(session.clone(), spawner);
                }
                None => {
                    spawners.remove(session);
                }
            }
        }
    }

    /// The shared live-sink cell of `session` (fresh and unshared when the
    /// table is poisoned).
    fn live_sink(&self, session: &SessionId) -> LiveSink {
        match self.live_sinks.lock() {
            Ok(mut table) => Arc::clone(table.entry(session.clone()).or_default()),
            Err(_) => LiveSink::default(),
        }
    }

    fn slots(&self) -> Result<MutexGuard<'_, SlotMap>, DriverBridgeError> {
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
        let admission = self
            .admissions
            .lock()
            .map_err(|_| DriverBridgeError::Runtime("admission table poisoned".into()))?
            .get(session_id)
            .cloned();
        // A recorded rollback is not a never-admitted legacy session. Never
        // recreate a wider legacy runtime from the durable tombstone.
        if admission
            .as_ref()
            .is_some_and(|state| state.admission.is_none())
        {
            return Err(DriverBridgeError::Runtime(
                "session admission was discarded; trusted re-admission required".into(),
            ));
        }
        let wiring = SessionWiring { event_tx, turn_tx };
        let factory = Arc::clone(&self.factory);
        let id = session_id.clone();
        let title = title.map(str::to_owned);
        let execution = admission.as_ref().and_then(|state| state.admission.clone());
        // Building the runtime reads workspace configuration and trusted policy from disk.
        // Leave socket handling and heartbeat timers on the async workers.
        let mut core = tokio::task::spawn_blocking(move || match execution {
            Some(execution) => {
                factory.build_with_execution(&id, title.as_deref(), wiring, &execution)
            }
            None => factory.build(&id, title.as_deref(), wiring),
        })
        .await
        .map_err(|error| {
            DriverBridgeError::Runtime(format!("session builder stopped: {error}"))
        })??;
        core.session
            .hydrate_from_store(core.store.as_ref())
            .await
            .map_err(|error| DriverBridgeError::Core(format!("hydrate: {error}")))?;
        self.note_model(session_id, core.session.active_model());
        self.note_spawner(session_id, core.spawner.clone());
        let live = core.session.agent_events().map(|hub| {
            LiveForwarder(
                tokio::spawn(forward_live(
                    hub.subscribe(),
                    session_id.clone(),
                    self.live_sink(session_id),
                ))
                .abort_handle(),
            )
        });
        let slot = Arc::new(tokio::sync::Mutex::new(Slot {
            core,
            session_events,
            turn_events,
            _live: live,
        }));
        // Hydration awaits external storage. Admission may have changed while
        // it ran; never publish a legacy/wider core under a newer admission.
        // Keep the admission lock through insertion (the same lock order as
        // prepare_execution) so installation and publication are atomic.
        let admissions = self
            .admissions
            .lock()
            .map_err(|_| DriverBridgeError::Runtime("admission table poisoned".into()))?;
        if admissions.get(session_id) != admission.as_ref() {
            return Err(DriverBridgeError::Runtime(
                "execution admission changed during session hydration; retry with current authority".into(),
            ));
        }
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
    fn prepare_execution(
        &self,
        session_id: &SessionId,
        admission: ExecutionAdmission,
    ) -> Result<(), DriverBridgeError> {
        let mut admissions = self
            .admissions
            .lock()
            .map_err(|_| DriverBridgeError::Runtime("admission table poisoned".into()))?;
        match admissions
            .get(session_id)
            .and_then(|state| state.admission.as_ref())
        {
            Some(previous) if previous == &admission => return Ok(()),
            Some(_) => {
                return Err(DriverBridgeError::Runtime(
                    "creator authority changed; cached runtime cannot safely reuse its permissions"
                        .into(),
                ));
            }
            None => {}
        }
        // Recovery may already have hydrated a dedicated-host session for
        // transcript/approval inspection. Rehydrate it with trusted authority
        // before allowing new effects; the persisted record grants no rights.
        let generation = admissions
            .get(session_id)
            .map_or(0, |state| state.generation)
            .checked_add(1)
            .ok_or_else(|| DriverBridgeError::Runtime("admission generation exhausted".into()))?;
        self.slots()?.remove(session_id);
        self.note_spawner(session_id, None);
        admissions.insert(
            session_id.clone(),
            AdmissionState {
                generation,
                admission: Some(admission),
            },
        );
        Ok(())
    }

    fn discard_unpublished_session(&self, session_id: &SessionId) -> Result<(), DriverBridgeError> {
        let mut admissions = self
            .admissions
            .lock()
            .map_err(|_| DriverBridgeError::Runtime("admission table poisoned".into()))?;
        let generation = admissions
            .get(session_id)
            .map_or(0, |state| state.generation)
            .checked_add(1)
            .ok_or_else(|| DriverBridgeError::Runtime("admission generation exhausted".into()))?;
        self.slots()?.remove(session_id);
        self.note_spawner(session_id, None);
        // Keep a tombstone so an older asynchronous hydration cannot publish
        // after rollback (including the None -> scoped -> None ABA case).
        admissions.insert(
            session_id.clone(),
            AdmissionState {
                generation,
                admission: None,
            },
        );
        self.models
            .lock()
            .map_err(|_| DriverBridgeError::Runtime("model table poisoned".into()))?
            .remove(session_id);
        if let Ok(mut sinks) = self.live_sinks.lock() {
            sinks.remove(session_id);
        }
        Ok(())
    }

    fn forget_session(&self, session_id: &SessionId) -> Result<(), DriverBridgeError> {
        // Same lock order as prepare_execution: admissions before slots.
        let mut admissions = self
            .admissions
            .lock()
            .map_err(|_| DriverBridgeError::Runtime("admission table poisoned".into()))?;
        // Bump the generation so an in-flight hydration cannot publish the
        // forgotten core; the admission itself stays (no tombstone).
        if let Some(state) = admissions.get_mut(session_id) {
            state.generation = state.generation.checked_add(1).ok_or_else(|| {
                DriverBridgeError::Runtime("admission generation exhausted".into())
            })?;
        }
        self.slots()?.remove(session_id);
        self.note_spawner(session_id, None);
        Ok(())
    }

    fn open_session<'a>(
        &'a self,
        session_id: &'a SessionId,
        title: Option<&'a str>,
    ) -> CoreFuture<'a, ()> {
        Box::pin(async move { self.slot(session_id, title).await.map(|_| ()) })
    }

    fn fork_history<'a>(
        &'a self,
        new: &'a SessionId,
        source: &'a SessionId,
        before_item: &'a str,
    ) -> CoreFuture<'a, ()> {
        Box::pin(async move {
            let source_slot = self.slot(source, None).await?;
            let items: Vec<harw_protocol::items::TurnItem> =
                source_slot.lock().await.core.session.history().items().to_vec();
            let Some(index) = items.iter().position(|item| {
                matches!(item, harw_protocol::items::TurnItem::UserMessage(m) if m.id.as_str() == before_item)
            }) else {
                return Err(DriverBridgeError::Runtime(
                    "fork target is not a user message of the source session".into(),
                ));
            };
            let target_slot = self.slot(new, None).await?;
            let mut guard = target_slot.lock().await;
            *guard.core.session.history_mut() =
                harw_core::ConversationHistory::from_items(items[..index].to_vec());
            guard
                .core
                .store
                .save_history(new, guard.core.session.history())
                .await
                .map_err(|error| DriverBridgeError::Core(error.to_string()))
        })
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
                ..
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
                ..
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
            // PL-100 O1: the reason rides in `summary`; the wire struct stays
            // unchanged because it denies unknown fields. Only consumers that
            // render `summary` show it: `harw_session_client::describe` drops
            // the field today, so remote SDK hosts do not see the reason yet.
            let reason = pending_ask_reason(session);
            Ok(Some(ParkedApproval {
                request: ApprovalRequest {
                    id: ApprovalId::from_str(pending.request.as_str()),
                    work_id: WorkId::new(),
                    kind: ApprovalKind::DynamicTool {
                        turn_id,
                        tool_name: tool.clone(),
                        arguments: pending.call.arguments.clone(),
                    },
                    summary: approval_summary(&tool, reason.as_deref()),
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

    fn live_agents(&self) -> Vec<LiveAgent> {
        let Ok(spawners) = self.spawners.lock() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (session, spawner) in spawners.iter() {
            for record in spawner.list_descendants_for(session) {
                let state = match record.status {
                    harw_core::child_controller::ChildStatus::Running => "running",
                    harw_core::child_controller::ChildStatus::Admitted | harw_core::child_controller::ChildStatus::Paused => "waiting",
                    harw_core::child_controller::ChildStatus::Completed => "done",
                    harw_core::child_controller::ChildStatus::Failed | harw_core::child_controller::ChildStatus::Cancelled => "failed",
                };
                out.push(LiveAgent {
                    agent_id: record.child.as_str().to_owned(),
                    session_id: session.clone(),
                    parent: Some(record.parent.as_str().to_owned()),
                    role: record.role.clone(),
                    model: record.model_route(),
                    state: state.to_owned(),
                    current_tool: None,
                    started_ms: u64::try_from(record.admitted_at.as_millisecond()).unwrap_or(0),
                    tool_calls: record.live.tool_calls,
                });
            }
        }
        out
    }

    fn stop_agent(&self, session_id: &SessionId, agent_id: &str) -> bool {
        let spawner = self
            .spawners
            .lock()
            .ok()
            .and_then(|spawners| spawners.get(session_id).cloned());
        let Some(spawner) = spawner else {
            return false;
        };
        let child = SessionId::from_str(agent_id.to_owned());
        spawner.owns_descendant(session_id, &child) && spawner.request_cancellation(&child)
    }

    fn bind_live_sink(&self, session_id: &SessionId, sink: Arc<dyn EventSink>) {
        let cell = self.live_sink(session_id);
        if let Ok(mut bound) = cell.lock() {
            *bound = Some(sink);
        }
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

    /// PL-100 O1: the DoD-taint reason rides in the summary; without one
    /// the summary keeps its previous text.
    #[test]
    fn the_summary_carries_the_taint_reason_only_when_there_is_one() {
        assert_eq!(
            approval_summary("job.stop", None),
            "tool 'job.stop' requires approval"
        );
        let reason = harw_core::dod_taint::ask_reason("job.stop");
        let summary = approval_summary("job.stop", Some(&reason));
        assert!(
            summary.starts_with("tool 'job.stop' requires approval; "),
            "{summary}"
        );
        assert!(
            summary.ends_with(harw_core::dod_taint::TAINT_REASON),
            "{summary}"
        );
    }
}

#[cfg(test)]
mod admission_races {
    use super::*;
    use harw_core::ConversationHistory;
    use harw_core::state_store::StateStoreResult;
    use harw_protocol::items::TurnItem;
    use tokio::sync::Notify;

    struct DelayedHistory {
        entered: Notify,
        release: Notify,
    }

    impl StateStore for DelayedHistory {
        fn save_turn<'a>(
            &'a self,
            _: &'a SessionId,
            _: &'a TurnItem,
        ) -> Pin<Box<dyn Future<Output = StateStoreResult<()>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }

        fn load_history<'a>(
            &'a self,
            _: &'a SessionId,
        ) -> Pin<Box<dyn Future<Output = StateStoreResult<ConversationHistory>> + Send + 'a>>
        {
            Box::pin(async move {
                self.entered.notify_one();
                self.release.notified().await;
                Ok(ConversationHistory::new())
            })
        }
    }

    struct DelayedFactory(Arc<DelayedHistory>);

    impl CoreSessionFactory for DelayedFactory {
        fn build(
            &self,
            session_id: &SessionId,
            _: Option<&str>,
            wiring: SessionWiring,
        ) -> Result<CoreSession, DriverBridgeError> {
            Ok(CoreSession {
                session: AgentSession::new_with_id(
                    session_id.clone(),
                    harw_types::roles::AgentRole::Assistant,
                    None,
                    harw_extension_api::ExtensionRegistryBuilder::default().build(),
                    wiring.event_tx,
                ),
                model: Arc::new(harw_core::EchoModelProvider::new("unused")),
                store: Arc::clone(&self.0) as Arc<dyn StateStore>,
                spawner: None,
            })
        }
    }

    #[tokio::test]
    async fn rollback_during_legacy_hydration_cannot_publish_stale_runtime()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let dir = tempfile::tempdir()?;
        let history = Arc::new(DelayedHistory {
            entered: Notify::new(),
            release: Notify::new(),
        });
        let runtime = Arc::new(HarwCoreRuntime::new(
            Arc::new(DelayedFactory(Arc::clone(&history))),
            dir.path(),
        ));
        let session_id = SessionId::new();
        let loading = {
            let runtime = Arc::clone(&runtime);
            let session_id = session_id.clone();
            tokio::spawn(async move { runtime.open_session(&session_id, None).await })
        };
        history.entered.notified().await;
        // Installing/clearing admission while legacy hydration awaits storage
        // changes its generation. The discarded core must never be published.
        runtime.discard_unpublished_session(&session_id)?;
        history.release.notify_one();
        assert!(loading.await?.is_err());
        assert!(runtime.slots()?.get(&session_id).is_none());
        // The rollback tombstone also refuses later lazy legacy recreation.
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                runtime.open_session(&session_id, None),
            )
            .await?
            .is_err()
        );
        Ok(())
    }
}

#[cfg(test)]
mod live_events {
    use super::*;
    use harw_core::state_store::StateStoreResult;
    use harw_core::{AgentEventHub, ConversationHistory};
    use harw_protocol::AgentOrchestrationEvent;
    use harw_protocol::items::TurnItem;
    use harw_session_host::identity::ConnectionId;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<DriverEvent>>);

    impl RecordingSink {
        fn count(&self) -> usize {
            self.0.lock().map_or(0, |events| events.len())
        }

        fn child_agents(&self) -> Vec<SessionId> {
            self.0.lock().map_or_else(
                |_| Vec::new(),
                |events| {
                    events
                        .iter()
                        .filter_map(|event| match event {
                            DriverEvent::Child { agent, .. } => Some(agent.clone()),
                            _ => None,
                        })
                        .collect()
                },
            )
        }
    }

    impl EventSink for RecordingSink {
        fn emit(&self, event: DriverEvent) {
            if let Ok(mut events) = self.0.lock() {
                events.push(event);
            }
        }
    }

    fn reasoning(agent: &SessionId, parent: Option<&SessionId>) -> AgentEvent {
        AgentEvent {
            agent: agent.clone(),
            parent: parent.cloned(),
            role: "explorer".to_owned(),
            kind: AgentEventKind::Turn(TurnEvent::ReasoningDelta {
                turn_id: TurnId::new(),
                text: "thinking".to_owned(),
            }),
        }
    }

    fn orchestration(child: &SessionId, status: AgentOrchestrationStatus) -> AgentEvent {
        AgentEvent {
            agent: child.clone(),
            parent: None,
            role: "explorer".to_owned(),
            kind: AgentEventKind::Orchestration(AgentOrchestrationEvent {
                schema_version: AgentOrchestrationEvent::CURRENT_SCHEMA_VERSION,
                event_id: "evt-1".to_owned(),
                root_session_id: SessionId::new(),
                parent_session_id: SessionId::new(),
                child_session_id: child.clone(),
                turn_id: None,
                role: "explorer".to_owned(),
                depth: 1,
                task: None,
                status,
                usage: None,
                duration_ms: None,
                progress: None,
                detail: None,
                tool_calls: None,
                model: None,
                provider: None,
            }),
        }
    }

    #[test]
    fn child_turn_events_map_to_child_frames_and_root_events_are_skipped() -> TestResult {
        let root = SessionId::new();
        let child = SessionId::new();
        let mapped = live_driver_event(&root, reasoning(&child, Some(&root)));
        match mapped {
            Some(DriverEvent::Child {
                agent,
                parent,
                role,
                event: TurnEvent::ReasoningDelta { text, .. },
            }) => {
                assert_eq!(agent, child);
                assert_eq!(parent, Some(root.clone()));
                assert_eq!(role, "explorer");
                assert_eq!(text, "thinking");
            }
            other => return Err(format!("unexpected mapping: {other:?}").into()),
        }
        assert!(live_driver_event(&root, reasoning(&root, None)).is_none());
        let knowledge = AgentEvent {
            agent: child,
            parent: None,
            role: "knowledge".to_owned(),
            kind: AgentEventKind::Knowledge {
                area: "jobs".into(),
                id: None,
            },
        };
        assert!(live_driver_event(&root, knowledge).is_none());
        Ok(())
    }

    #[test]
    fn orchestration_maps_to_job_states_without_progress() -> TestResult {
        let root = SessionId::new();
        let child = SessionId::new();
        match live_driver_event(
            &root,
            orchestration(&child, AgentOrchestrationStatus::Completed),
        ) {
            Some(DriverEvent::Job {
                work_id,
                kind,
                state,
                summary,
            }) => {
                assert_eq!(work_id, child.as_str());
                assert_eq!(kind, "agent");
                assert_eq!(state, "completed");
                assert_eq!(summary.as_deref(), Some("explorer"));
            }
            other => return Err(format!("unexpected mapping: {other:?}").into()),
        }
        assert!(
            live_driver_event(
                &root,
                orchestration(&child, AgentOrchestrationStatus::Progress)
            )
            .is_none()
        );
        Ok(())
    }

    struct InstantStore;

    impl StateStore for InstantStore {
        fn save_turn<'a>(
            &'a self,
            _: &'a SessionId,
            _: &'a TurnItem,
        ) -> Pin<Box<dyn Future<Output = StateStoreResult<()>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }

        fn load_history<'a>(
            &'a self,
            _: &'a SessionId,
        ) -> Pin<Box<dyn Future<Output = StateStoreResult<ConversationHistory>> + Send + 'a>>
        {
            Box::pin(async { Ok(ConversationHistory::new()) })
        }
    }

    struct HubFactory {
        hub: AgentEventHub,
    }

    impl CoreSessionFactory for HubFactory {
        fn build(
            &self,
            session_id: &SessionId,
            _: Option<&str>,
            wiring: SessionWiring,
        ) -> Result<CoreSession, DriverBridgeError> {
            Ok(CoreSession {
                session: AgentSession::new_with_id(
                    session_id.clone(),
                    harw_types::roles::AgentRole::Assistant,
                    None,
                    harw_extension_api::ExtensionRegistryBuilder::default().build(),
                    wiring.event_tx,
                )
                .with_agent_events(self.hub.clone()),
                model: Arc::new(harw_core::EchoModelProvider::new("unused")),
                store: Arc::new(InstantStore),
                spawner: None,
            })
        }
    }

    async fn wait_for(sink: &RecordingSink, count: usize) -> TestResult {
        for _ in 0..200 {
            if sink.count() >= count {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Err("timed out waiting for live events".into())
    }

    #[tokio::test]
    async fn child_events_reach_the_bound_sink_between_legs() -> TestResult {
        let dir = tempfile::tempdir()?;
        let hub = AgentEventHub::default();
        let runtime = HarwCoreRuntime::new(Arc::new(HubFactory { hub: hub.clone() }), dir.path());
        let sid = SessionId::new();
        let sink_a = Arc::new(RecordingSink::default());
        let sink_b = Arc::new(RecordingSink::default());
        runtime.bind_live_sink(&sid, Arc::clone(&sink_a) as Arc<dyn EventSink>);
        runtime.open_session(&sid, None).await?;

        let child_one = SessionId::new();
        hub.publish(reasoning(&child_one, Some(&sid)));
        wait_for(&sink_a, 1).await?;
        assert_eq!(sink_a.child_agents(), vec![child_one]);

        // Root events never show up; the ordered child event after it proves
        // the forwarder already processed the root one.
        hub.publish(reasoning(&sid, None));
        let child_two = SessionId::new();
        hub.publish(reasoning(&child_two, Some(&sid)));
        wait_for(&sink_a, 2).await?;
        assert_eq!(sink_a.count(), 2);
        assert!(!sink_a.child_agents().contains(&sid));

        runtime.bind_live_sink(&sid, Arc::clone(&sink_b) as Arc<dyn EventSink>);
        let child_three = SessionId::new();
        hub.publish(reasoning(&child_three, Some(&sid)));
        wait_for(&sink_b, 1).await?;
        assert_eq!(sink_b.child_agents(), vec![child_three]);
        assert_eq!(sink_a.count(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn forget_session_drops_slot_without_tombstone() -> TestResult {
        let dir = tempfile::tempdir()?;
        let hub = AgentEventHub::default();
        let runtime = HarwCoreRuntime::new(Arc::new(HubFactory { hub }), dir.path());
        let sid = SessionId::new();
        runtime.open_session(&sid, None).await?;
        assert!(runtime.slots()?.get(&sid).is_some());
        runtime.forget_session(&sid)?;
        assert!(runtime.slots()?.get(&sid).is_none());
        // No tombstone: the next leg hydrates lazily again.
        assert!(
            runtime
                .admissions
                .lock()
                .map_err(|_| "admission table poisoned")?
                .get(&sid)
                .is_none()
        );
        runtime.open_session(&sid, None).await?;
        assert!(runtime.slots()?.get(&sid).is_some());

        // With a recorded admission state: the admission is kept unchanged,
        // only the generation moves. `ExecutionAdmission::new` is crate-private
        // to the host, so the state is seeded directly here.
        let other = SessionId::new();
        let seeded = AdmissionState {
            generation: 3,
            admission: None,
        };
        runtime
            .admissions
            .lock()
            .map_err(|_| "admission table poisoned")?
            .insert(other.clone(), seeded.clone());
        runtime.forget_session(&other)?;
        let after = runtime
            .admissions
            .lock()
            .map_err(|_| "admission table poisoned")?
            .get(&other)
            .cloned()
            .ok_or("admission state vanished")?;
        assert_eq!(after.admission, seeded.admission);
        assert!(after.generation > seeded.generation);
        Ok(())
    }

    #[derive(Default)]
    struct BindRecorder(Mutex<Vec<SessionId>>);

    impl CoreRuntime for BindRecorder {
        fn open_session<'a>(&'a self, _: &'a SessionId, _: Option<&'a str>) -> CoreFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }

        fn run_turn<'a>(
            &'a self,
            _: &'a SessionId,
            _: CoreTurnInput,
            _: CoreEvents,
        ) -> CoreFuture<'a, CoreOutcome> {
            Box::pin(async { Ok(CoreOutcome::Completed) })
        }

        fn resume_after_approval<'a>(
            &'a self,
            _: &'a SessionId,
            _: ApprovalActor,
            _: ApprovalResolution,
            _: CoreEvents,
        ) -> CoreFuture<'a, CoreOutcome> {
            Box::pin(async { Ok(CoreOutcome::Completed) })
        }

        fn parked_approval<'a>(
            &'a self,
            _: &'a SessionId,
        ) -> CoreFuture<'a, Option<ParkedApproval>> {
            Box::pin(async { Ok(None) })
        }

        fn apply_setting<'a>(&'a self, _: &'a SessionId, _: Setting) -> CoreFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }

        fn model_name(&self, _: &SessionId) -> Option<String> {
            None
        }

        fn bind_live_sink(&self, session_id: &SessionId, _: Arc<dyn EventSink>) {
            if let Ok(mut bound) = self.0.lock() {
                bound.push(session_id.clone());
            }
        }
    }

    #[tokio::test]
    async fn run_turn_binds_the_leg_sink() -> TestResult {
        let dir = tempfile::tempdir()?;
        let runtime = Arc::new(BindRecorder::default());
        let driver = CoreTurnDriver::with_runtime(
            CoreDriverConfig::new(dir.path().to_path_buf()),
            Arc::clone(&runtime) as Arc<dyn CoreRuntime>,
        )?;
        let session = SessionId::new();
        let input = TurnInput {
            session_id: session.clone(),
            text: "hello".to_owned(),
            images: Vec::new(),
            client_msg_id: "msg-1".to_owned(),
            submitted_by: "tester".to_owned(),
            actor: ApprovalActor::Operator {
                id: "op".to_owned(),
            },
            origin: ConnectionId::next(),
            rewind_before: None,
        };
        let cancel = tokio::sync::watch::channel(false).1;
        let sink: Arc<dyn EventSink> = Arc::new(RecordingSink::default());
        let outcome = TurnDriver::run_turn(&driver, input, cancel, sink).await?;
        assert!(matches!(outcome, TurnOutcome::Completed));
        let bound = runtime.0.lock().map_err(|_| "poisoned")?.clone();
        assert_eq!(bound, vec![session]);
        Ok(())
    }
}
