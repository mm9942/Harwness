//! The session host: composition of records, replay, live ring, fan-out,
//! arbiter, approvals and the runtime driver (W00 §2.4, PL-65 §2–§3).
//!
//! Locking: one `std::sync::Mutex` per session slot. It is held only for
//! short, non-async sections (state changes, fan-out pushes, the attach
//! replay read), never across an `.await`, so driver events are recorded
//! without waiting on any client.
//!
//! R18 (contract `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md`):
//! [`HostConnection`] also implements [`ToolPort`] (admission §4.2 steps
//! 1-7 here, 8-9 in [`crate::tool_host`]) and [`GatewayPort`] (`gateway.*`,
//! tenant-filtered, reads need `gateway_read`, mutations `gateway_admin`).
//! Hello masks the R18 caps below wire minor 2
//! ([`ClientCaps::for_wire_minor`]). Revoking a connection, device or agent
//! principal cancels its in-flight tool calls; draining refuses new ones and
//! lets in-flight calls finish.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use harw_protocol::session_wire::{
    ApprovalRespondParams, AttachAck, AttachParams, CreateParams, GatewayConnectionInfo,
    GatewayConnectionsResult, GatewayDrainParams, GatewayListenerInfo, GatewayListenerSetParams,
    GatewayListenersResult, GatewayRevokeParams, GatewayRevokeResult, GatewayStatus,
    GatewayToolRights, GatewayToolRightsParams, GatewayToolsResult, HelloAck, HelloParams,
    HistoryParams, InterruptParams, PrincipalSummary, RespondResult, SESSION_WIRE_MINOR,
    SessionSummary, SetEffortParams, SetModeParams, SetModelParams, SubmitParams, SubmitResult,
    TOOL_GATEWAY_WIRE_MINOR, ToolCallParams, ToolCallResultFrame, ToolCancelParams, ToolListParams,
    ToolListResult, features,
};
use harw_protocol::{
    ApprovalRequest, ClientCaps, Cursor, FrameEnvelope, FrameSource, GatewayPort, HostedState,
    PortError, PortFuture, PresenceEntry, SessionEvent, SessionFrame, SessionPort, ToolPort,
    ToolRefusal, TurnEvent,
};
use harw_types::{DeviceId, SessionId, TenantId};
use tokio::sync::watch;

use crate::agents::{AgentRegistry, rights_of};
use crate::approvals::{ApprovalBackend, ResolveOutcome};
use crate::arbiter::{Arbiter, ArbiterLimits};
use crate::driver::{
    DriverEvent, EventSink, MAX_NOTICE_CHARS, Setting, TurnDriver, TurnInput, TurnOutcome,
    sanitize_notice,
};
use crate::error::HostError;
use crate::fanout::{AttachmentQueue, QueueLimits, attachment};
use crate::identity::{
    AgentPrincipal, ClientIdentity, ConnectionId, Need, ToolGrant, admit, admit_tool_call,
    tenant_admits,
};
use crate::live_ring::LiveRing;
use crate::record::{HostedSessionRecord, RecordStore};
use crate::replay::{TranscriptSource, history_before, replay, tail_start};
use crate::tool_host::{SessionScope, ToolEvents, ToolHost};

/// Host configuration.
#[derive(Clone, Debug)]
pub struct HostConfig {
    /// Machine-local state directory (`HARW_STATE_DIR`); records live under
    /// `<state_dir>/hosted-sessions`.
    pub state_dir: PathBuf,
    /// Per-attachment queue bounds.
    pub queue: QueueLimits,
    /// Per-session input arbitration bounds.
    pub arbiter: ArbiterLimits,
    /// Upper bound for `session.history` pages.
    pub max_history_page: u32,
    /// Keep-alive frame interval.
    pub heartbeat: Duration,
}

impl HostConfig {
    /// Defaults from PL-65 for `state_dir`.
    #[must_use]
    pub fn new(state_dir: PathBuf) -> Self {
        Self {
            state_dir,
            queue: QueueLimits::default(),
            arbiter: ArbiterLimits::default(),
            max_history_page: 500,
            heartbeat: Duration::from_secs(15),
        }
    }
}

/// Features this host implements.
const HOST_FEATURES: &[&str] = &[
    features::COMPACT,
    features::HISTORY,
    features::CHILD_FRAMES,
    features::TOOLS,
    features::GATEWAY,
];

/// Features that exist only from [`TOOL_GATEWAY_WIRE_MINOR`] on.
const R18_FEATURES: &[&str] = &[features::TOOLS, features::GATEWAY];

/// One live connection as `gateway.connections.list` shows it.
struct ConnEntry {
    label: String,
    principal: PrincipalSummary,
    tenant: Option<TenantId>,
    device: Option<DeviceId>,
    agent: Option<String>,
    granted: Option<ClientCaps>,
    since: jiff::Timestamp,
}

struct AttachEntry {
    connection: ConnectionId,
    device: Option<DeviceId>,
    queue: AttachmentQueue,
    presence: PresenceEntry,
}

struct SlotState {
    record: HostedSessionRecord,
    arbiter: Arbiter,
    ring: LiveRing,
    /// Transcript sequence of the next durable record (cached head).
    durable_head: u64,
    attachments: Vec<AttachEntry>,
    cancel: Option<watch::Sender<bool>>,
    pending_settings: Vec<Setting>,
    /// A turn is parked on an approval.
    parked: bool,
}

struct Slot {
    id: SessionId,
    state: Mutex<SlotState>,
}

impl Slot {
    fn lock(&self) -> Result<MutexGuard<'_, SlotState>, HostError> {
        self.state
            .lock()
            .map_err(|_| HostError::Storage("session slot poisoned".into()))
    }
}

struct HostInner {
    config: HostConfig,
    epoch: u64,
    records: RecordStore,
    transcripts: Arc<dyn TranscriptSource>,
    approvals: Arc<dyn ApprovalBackend>,
    driver: Arc<dyn TurnDriver>,
    slots: Mutex<HashMap<SessionId, Arc<Slot>>>,
    revoked_devices: Mutex<HashSet<DeviceId>>,
    revoked_connections: Mutex<HashSet<ConnectionId>>,
    draining: std::sync::atomic::AtomicBool,
    /// R18 gateway tool host (tools, agent registry, in-flight calls).
    tools: ToolHost,
    /// R18 live connections (`gateway.connections.list`).
    connections: Mutex<BTreeMap<ConnectionId, ConnEntry>>,
    /// R18 configured listeners (`gateway.listeners.*`).
    listeners: Mutex<Vec<GatewayListenerInfo>>,
}

/// The persistent session host. Cheap to clone.
#[derive(Clone)]
pub struct SessionHost {
    inner: Arc<HostInner>,
}

impl std::fmt::Debug for SessionHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionHost")
            .field("epoch", &self.inner.epoch)
            .field("state_dir", &self.inner.config.state_dir)
            .finish_non_exhaustive()
    }
}

impl SessionHost {
    /// Open the host: bump the host epoch and mark sessions that were
    /// running or queued when the previous host stopped as `Interrupted`
    /// (PL-65 §2.2 restart recovery, RP-03).
    pub fn open(
        config: HostConfig,
        driver: Arc<dyn TurnDriver>,
        transcripts: Arc<dyn TranscriptSource>,
        approvals: Arc<dyn ApprovalBackend>,
    ) -> Result<Self, HostError> {
        Self::open_with_tools(config, driver, transcripts, approvals, ToolHost::disabled())
    }

    /// Like [`SessionHost::open`], with the R18 gateway tool host. Tool
    /// approvals must be issued into the same store as `approvals`
    /// ([`crate::tool_host::ToolHostBuilder::approvals`]).
    pub fn open_with_tools(
        config: HostConfig,
        driver: Arc<dyn TurnDriver>,
        transcripts: Arc<dyn TranscriptSource>,
        approvals: Arc<dyn ApprovalBackend>,
        tools: ToolHost,
    ) -> Result<Self, HostError> {
        let records = RecordStore::new(&config.state_dir);
        let epoch = records.next_epoch()?;
        let interrupted = records.recover_after_restart(epoch)?;
        if !interrupted.is_empty() {
            tracing::info!(
                count = interrupted.len(),
                epoch,
                "session host: interrupted sessions from previous epoch"
            );
        }
        Ok(Self {
            inner: Arc::new(HostInner {
                config,
                epoch,
                records,
                transcripts,
                approvals,
                driver,
                slots: Mutex::new(HashMap::new()),
                revoked_devices: Mutex::new(HashSet::new()),
                revoked_connections: Mutex::new(HashSet::new()),
                draining: std::sync::atomic::AtomicBool::new(false),
                tools,
                connections: Mutex::new(BTreeMap::new()),
                listeners: Mutex::new(Vec::new()),
            }),
        })
    }

    /// Host epoch (increments on every open).
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.inner.epoch
    }

    /// A port bound to one authenticated client. The identity is fixed for
    /// the lifetime of the connection.
    pub fn connect(&self, identity: ClientIdentity) -> Result<HostConnection, HostError> {
        identity.validate()?;
        if self.inner.is_revoked(&identity) {
            return Err(HostError::Revoked);
        }
        if self.inner.is_draining() {
            return Err(HostError::Denied("host is draining".into()));
        }
        if let Some(agent) = &identity.agent {
            self.inner
                .check_agent_registration(agent, identity.tenant.as_ref())?;
        }
        let entry = ConnEntry {
            label: identity.label.clone(),
            principal: identity.principal_summary(),
            tenant: identity.tenant.clone(),
            device: identity.device.clone(),
            agent: identity.agent.as_ref().map(|agent| agent.id.clone()),
            granted: None,
            since: jiff::Timestamp::now(),
        };
        self.inner
            .connections
            .lock()
            .map_err(|_| HostError::Storage("connection table poisoned".into()))?
            .insert(identity.connection, entry);
        Ok(HostConnection {
            host: Arc::clone(&self.inner),
            identity,
            granted: Mutex::new(None),
        })
    }

    /// Revoke a device: refuse new calls, close its streams with `Revoked`,
    /// drop its queued inputs and cancel its in-flight tool calls (W00 §7,
    /// REV-01, R18 §7).
    pub fn revoke_device(&self, device: &DeviceId) {
        if let Ok(mut revoked) = self.inner.revoked_devices.lock() {
            revoked.insert(device.clone());
        }
        for connection in self
            .inner
            .connections_where(|entry| entry.device.as_ref() == Some(device))
        {
            self.inner
                .tools
                .cancel_connection(connection, "device revoked");
        }
        self.inner
            .close_matching(|entry| entry.device.as_ref() == Some(device), revoked_frame);
    }

    /// Revoke one connection (for example when its security context
    /// expired). Also cancels its in-flight tool calls (R18 §7).
    pub fn revoke_connection(&self, connection: ConnectionId) {
        self.inner.revoke_connection(connection);
    }

    /// Revoke an agent principal and every agent it delegated to: refuse
    /// their calls, close their streams with `Revoked` and cancel their
    /// in-flight tool calls (R18 §7). Returns the ids revoked by this call.
    pub fn revoke_agent(&self, agent: &str) -> Result<Vec<String>, HostError> {
        let revoked = self.inner.tools.agents().revoke(agent)?;
        self.inner
            .tools
            .cancel_agents(&revoked, "agent principal revoked");
        let connections: HashSet<ConnectionId> = self
            .inner
            .connections_where(|entry| {
                entry
                    .agent
                    .as_ref()
                    .is_some_and(|agent| revoked.contains(agent))
            })
            .into_iter()
            .collect();
        self.inner.close_matching(
            |entry| connections.contains(&entry.connection),
            revoked_frame,
        );
        Ok(revoked)
    }

    /// Drain: refuse new connections, turns and tool calls, tell every
    /// client when to come back, cancel running turns (they end
    /// `Interrupted` and resume after the restart by policy). In-flight
    /// tool calls finish (R18 §7).
    pub fn drain(&self, retry_after_ms: u64) {
        self.inner.drain(retry_after_ms);
    }

    /// True once [`SessionHost::drain`] ran.
    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.inner.is_draining()
    }

    /// The R18 gateway tool host.
    #[must_use]
    pub fn tool_host(&self) -> &ToolHost {
        &self.inner.tools
    }

    /// The R18 agent principal registry (listeners resolve agent
    /// credentials here).
    #[must_use]
    pub fn agents(&self) -> &AgentRegistry {
        self.inner.tools.agents()
    }

    /// Bind the active agent principal `agent` (and with it every agent it
    /// delegates to) to `session` for gateway tool calls (R18 §4).
    ///
    /// A session admits `tool.list`/`tool.call`/`tool.cancel` only from the
    /// agent that created it, its delegates, and agents bound here — never
    /// from any other agent of the same tenant. This is a composition API
    /// (for example: a human creates the session, the host launches the UIA
    /// process for it), not a wire method; no payload can bind an agent.
    ///
    /// # Errors
    /// - [`HostError::NotFound`]: unknown session or agent.
    /// - [`HostError::Revoked`]: the agent is revoked.
    /// - [`HostError::Denied`]: the agent's tenant differs from the
    ///   session's tenant.
    pub fn bind_tool_agent(&self, session: &SessionId, agent: &str) -> Result<(), HostError> {
        let state = self.inner.tools.agents().state(agent)?;
        if state.revoked {
            return Err(HostError::Revoked);
        }
        let slot = self.inner.slot(session)?;
        let mut slot_state = slot.lock()?;
        if slot_state.record.tenant != state.tenant {
            return Err(HostError::Denied(
                "agent principal and session belong to different tenants".into(),
            ));
        }
        if !slot_state
            .record
            .tool_agents
            .iter()
            .any(|bound| bound == agent)
        {
            slot_state.record.tool_agents.push(agent.to_owned());
            self.inner.save(&mut slot_state);
        }
        Ok(())
    }

    /// Announce a configured listener (`gateway.listeners.*`). A listener
    /// with the same name is replaced.
    pub fn register_listener(&self, listener: GatewayListenerInfo) {
        if let Ok(mut listeners) = self.inner.listeners.lock() {
            listeners.retain(|known| known.name != listener.name);
            listeners.push(listener);
        }
    }

    /// Whether the listener `name` is enabled (`None`: unknown listener).
    /// Listener tasks poll this after `gateway.listeners.set`.
    #[must_use]
    pub fn listener_enabled(&self, name: &str) -> Option<bool> {
        self.inner.listeners.lock().ok().and_then(|listeners| {
            listeners
                .iter()
                .find(|listener| listener.name == name)
                .map(|listener| listener.enabled)
        })
    }

    /// Push a heartbeat frame to every attachment. Call from a timer task
    /// (see [`SessionHost::spawn_heartbeat`]).
    pub fn heartbeat(&self) {
        for slot in self.inner.all_slots() {
            if let Ok(state) = slot.lock() {
                let cursor = state.head_cursor();
                for entry in &state.attachments {
                    entry.queue.push(FrameEnvelope {
                        session_id: slot.id.clone(),
                        cursor,
                        frame: SessionFrame::Heartbeat,
                    });
                }
            }
        }
    }

    /// Spawn the heartbeat timer on the current Tokio runtime.
    #[must_use]
    pub fn spawn_heartbeat(&self) -> tokio::task::JoinHandle<()> {
        let host = self.clone();
        let every = self.inner.config.heartbeat;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.tick().await;
            loop {
                tick.tick().await;
                host.heartbeat();
            }
        })
    }
}

fn revoked_frame(_: &SessionId) -> SessionFrame {
    SessionFrame::Revoked
}

impl HostInner {
    fn is_revoked(&self, identity: &ClientIdentity) -> bool {
        let device = identity.device.as_ref().is_some_and(|device| {
            self.revoked_devices
                .lock()
                .map(|revoked| revoked.contains(device))
                .unwrap_or(true)
        });
        let connection = self
            .revoked_connections
            .lock()
            .map(|revoked| revoked.contains(&identity.connection))
            .unwrap_or(true);
        let agent = identity
            .agent
            .as_ref()
            .is_some_and(|agent| self.tools.agents().is_revoked(&agent.id));
        device || connection || agent
    }

    fn is_draining(&self) -> bool {
        self.draining.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// An agent identity must match its registration: known, not revoked,
    /// same role, parent and tenant (the listener built it from a
    /// credential; anything else is a listener bug, fail closed).
    fn check_agent_registration(
        &self,
        agent: &AgentPrincipal,
        tenant: Option<&TenantId>,
    ) -> Result<(), HostError> {
        let state = match self.tools.agents().state(&agent.id) {
            Ok(state) => state,
            Err(HostError::NotFound) => {
                return Err(HostError::Denied("unknown agent principal".into()));
            }
            Err(error) => return Err(error),
        };
        if state.revoked {
            return Err(HostError::Revoked);
        }
        if state.principal.role != agent.role
            || state.principal.parent != agent.parent
            || state.tenant.as_ref() != tenant
        {
            return Err(HostError::Denied(
                "agent principal does not match its registration".into(),
            ));
        }
        Ok(())
    }

    /// Connections whose table entry matches `pick`.
    fn connections_where(&self, pick: impl Fn(&ConnEntry) -> bool) -> Vec<ConnectionId> {
        self.connections
            .lock()
            .map(|connections| {
                connections
                    .iter()
                    .filter(|(_, entry)| pick(entry))
                    .map(|(id, _)| *id)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn revoke_connection(&self, connection: ConnectionId) {
        if let Ok(mut revoked) = self.revoked_connections.lock() {
            revoked.insert(connection);
        }
        self.tools
            .cancel_connection(connection, "connection revoked");
        self.close_matching(|entry| entry.connection == connection, revoked_frame);
    }

    fn drain(&self, retry_after_ms: u64) {
        self.draining
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.close_matching(
            |_| true,
            move |_| SessionFrame::HostDraining { retry_after_ms },
        );
        for slot in self.all_slots() {
            if let Ok(state) = slot.lock() {
                if let Some(cancel) = &state.cancel {
                    let _ = cancel.send(true);
                }
            }
        }
    }

    fn all_slots(&self) -> Vec<Arc<Slot>> {
        self.slots
            .lock()
            .map(|slots| slots.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Close every attachment matching `pick` with a final frame and drop the
    /// queued inputs of the affected connections.
    fn close_matching(
        &self,
        pick: impl Fn(&AttachEntry) -> bool,
        final_frame: impl Fn(&SessionId) -> SessionFrame,
    ) {
        for slot in self.all_slots() {
            let Ok(mut state) = slot.lock() else { continue };
            let cursor = state.head_cursor();
            let mut kept = Vec::with_capacity(state.attachments.len());
            let mut closed_connections = Vec::new();
            for entry in state.attachments.drain(..) {
                if pick(&entry) {
                    entry.queue.close(Some(FrameEnvelope {
                        session_id: slot.id.clone(),
                        cursor,
                        frame: final_frame(&slot.id),
                    }));
                    closed_connections.push(entry.connection);
                } else {
                    kept.push(entry);
                }
            }
            state.attachments = kept;
            for connection in closed_connections {
                state.arbiter.drop_origin(connection);
            }
            state.broadcast_presence(&slot.id);
        }
    }

    /// Load (or build from the record store) the slot of a session.
    fn slot(&self, id: &SessionId) -> Result<Arc<Slot>, HostError> {
        if let Some(slot) = self
            .slots
            .lock()
            .map_err(|_| HostError::Storage("slot table poisoned".into()))?
            .get(id)
        {
            return Ok(Arc::clone(slot));
        }
        let record = self.records.load(id)?.ok_or(HostError::NotFound)?;
        let head = self.transcripts.head(id)?;
        let slot = Arc::new(Slot {
            id: id.clone(),
            state: Mutex::new(SlotState {
                record,
                arbiter: Arbiter::new(self.config.arbiter),
                ring: LiveRing::default(),
                durable_head: head,
                attachments: Vec::new(),
                cancel: None,
                pending_settings: Vec::new(),
                parked: false,
            }),
        });
        let mut slots = self
            .slots
            .lock()
            .map_err(|_| HostError::Storage("slot table poisoned".into()))?;
        Ok(Arc::clone(slots.entry(id.clone()).or_insert(slot)))
    }

    fn insert_slot(&self, record: HostedSessionRecord) -> Result<Arc<Slot>, HostError> {
        let id = record.session_id.clone();
        let slot = Arc::new(Slot {
            id: id.clone(),
            state: Mutex::new(SlotState {
                record,
                arbiter: Arbiter::new(self.config.arbiter),
                ring: LiveRing::default(),
                durable_head: 0,
                attachments: Vec::new(),
                cancel: None,
                pending_settings: Vec::new(),
                parked: false,
            }),
        });
        self.slots
            .lock()
            .map_err(|_| HostError::Storage("slot table poisoned".into()))?
            .insert(id, Arc::clone(&slot));
        Ok(slot)
    }

    fn summary(&self, state: &SlotState) -> SessionSummary {
        SessionSummary {
            session_id: state.record.session_id.clone(),
            title: state.record.title.clone(),
            tenant: state.record.tenant.clone(),
            state: state.record.state.clone(),
            attached: u32::try_from(state.attachments.len()).unwrap_or(u32::MAX),
            updated_at: state.record.updated_at,
            model: self.driver.model_name(&state.record.session_id),
        }
    }

    fn save(&self, state: &mut SlotState) {
        state.record.updated_at = jiff::Timestamp::now();
        if let Err(error) = self.records.save(&state.record) {
            tracing::warn!(%error, session = %state.record.session_id.as_str(), "session host: record save failed");
        }
    }
}

impl SlotState {
    fn head_cursor(&self) -> Cursor {
        Cursor {
            generation: self.record.generation,
            durable: self.durable_head,
            live: self.ring.next_index(),
        }
    }

    /// Record a frame of the running turn and fan it out.
    fn publish(&mut self, session_id: &SessionId, frame: SessionFrame) {
        let live = if frame.is_disposable()
            || matches!(frame, SessionFrame::Turn(_) | SessionFrame::Child { .. })
        {
            self.ring.push(frame.clone())
        } else {
            self.ring.next_index()
        };
        let envelope = FrameEnvelope {
            session_id: session_id.clone(),
            cursor: Cursor {
                generation: self.record.generation,
                durable: self.durable_head,
                live,
            },
            frame,
        };
        self.attachments.retain(|entry| !entry.queue.is_closed());
        for entry in &self.attachments {
            entry.queue.push(envelope.clone());
        }
    }

    fn broadcast_presence(&mut self, session_id: &SessionId) {
        self.attachments.retain(|entry| !entry.queue.is_closed());
        let attached: Vec<PresenceEntry> = self
            .attachments
            .iter()
            .map(|entry| entry.presence.clone())
            .collect();
        let envelope = FrameEnvelope {
            session_id: session_id.clone(),
            cursor: self.head_cursor(),
            frame: SessionFrame::Presence { attached },
        };
        for entry in &self.attachments {
            entry.queue.push(envelope.clone());
        }
    }

    fn set_state(&mut self, state: HostedState) {
        self.record.state = state;
    }

    /// Fan out a frame that does not belong to a driver turn (gateway tool
    /// events of an agent-run turn). Buffered in the live ring only while a
    /// driver turn is active there, so it never outlives the ring's turn.
    fn publish_side(&mut self, session_id: &SessionId, frame: SessionFrame) {
        if self.ring.active_turn().is_some() {
            self.publish(session_id, frame);
            return;
        }
        let envelope = FrameEnvelope {
            session_id: session_id.clone(),
            cursor: self.head_cursor(),
            frame,
        };
        self.attachments.retain(|entry| !entry.queue.is_closed());
        for entry in &self.attachments {
            entry.queue.push(envelope.clone());
        }
    }
}

/// Gateway tool events of one session.
struct SlotEvents {
    slot: Arc<Slot>,
}

impl ToolEvents for SlotEvents {
    fn turn_event(&self, event: TurnEvent) {
        if let Ok(mut state) = self.slot.lock() {
            state.publish_side(&self.slot.id, SessionFrame::Turn(event));
        }
    }

    fn approval_requested(&self, request: ApprovalRequest) {
        if let Ok(mut state) = self.slot.lock() {
            state.publish_side(&self.slot.id, SessionFrame::ApprovalRequested(request));
        }
    }
}

/// Event sink of one session: maps driver events to frames without waiting.
struct SlotSink {
    host: Weak<HostInner>,
    slot: Arc<Slot>,
}

impl EventSink for SlotSink {
    fn emit(&self, event: DriverEvent) {
        let Ok(mut state) = self.slot.lock() else {
            return;
        };
        let id = &self.slot.id;
        match event {
            DriverEvent::DurableAdvanced => {
                if let Some(host) = self.host.upgrade()
                    && let Ok(head) = host.transcripts.head(id)
                {
                    state.durable_head = head;
                }
            }
            DriverEvent::Turn(event) => {
                if let TurnEvent::TurnStarted { turn_id, .. } = &event {
                    state.ring.begin_turn(turn_id.clone());
                }
                state.publish(id, SessionFrame::Turn(event));
            }
            DriverEvent::Session(event) => state.publish(id, SessionFrame::Session(event)),
            DriverEvent::Child {
                agent,
                parent,
                role,
                event,
            } => state.publish(
                id,
                SessionFrame::Child {
                    agent,
                    parent,
                    role,
                    event,
                },
            ),
            DriverEvent::ApprovalRequested(request) => {
                state.set_state(HostedState::WaitingForApproval);
                state.publish(id, SessionFrame::ApprovalRequested(request));
            }
        }
    }
}

/// Run queued turns of one session until its queue is empty or a turn parks.
fn pump(host: Arc<HostInner>, slot: Arc<Slot>) {
    tokio::spawn(async move {
        loop {
            let (input, settings, cancel_rx) = {
                let Ok(mut state) = slot.lock() else { return };
                if state.parked || state.record.state.is_terminal() {
                    return;
                }
                let Some(input) = state.arbiter.next_input() else {
                    return;
                };
                let (cancel_tx, cancel_rx) = watch::channel(false);
                state.cancel = Some(cancel_tx);
                let depth = state.arbiter.depth();
                state.set_state(if depth > 0 {
                    HostedState::Queued { depth }
                } else {
                    HostedState::Running
                });
                host.save(&mut state);
                (
                    input,
                    std::mem::take(&mut state.pending_settings),
                    cancel_rx,
                )
            };
            for setting in settings {
                if let Err(error) = host.driver.apply_setting(&slot.id, setting).await {
                    tracing::warn!(%error, "session host: setting failed");
                }
            }
            let sink: Arc<dyn EventSink> = Arc::new(SlotSink {
                host: Arc::downgrade(&host),
                slot: Arc::clone(&slot),
            });
            let outcome = host
                .driver
                .run_turn(input, cancel_rx, sink)
                .await
                .unwrap_or_else(|error| TurnOutcome::Failed(error.to_string()));
            if !settle(&host, &slot, &outcome) {
                return;
            }
        }
    });
}

/// Shown to a client that attaches to a session a host restart interrupted.
const INTERRUPTED_NOTICE: &str = "session was interrupted by a host restart; the turn that was running is lost. \
     Resume the session (session.resume) to continue, then submit your message again";

/// Reason of a submit refused because the session is `Interrupted`.
const INTERRUPTED_DENIED: &str = "session interrupted by a host restart; session.resume first";

/// Apply a turn outcome. Returns `true` when the pump should continue with
/// the next queued input.
fn settle(host: &HostInner, slot: &Slot, outcome: &TurnOutcome) -> bool {
    let Ok(mut state) = slot.lock() else {
        return false;
    };
    if let Ok(head) = host.transcripts.head(&slot.id) {
        state.durable_head = head;
    }
    state.cancel = None;
    match outcome {
        TurnOutcome::AwaitingApproval => {
            state.parked = true;
            state.set_state(HostedState::WaitingForApproval);
            host.save(&mut state);
            false
        }
        TurnOutcome::Completed
        | TurnOutcome::Interrupted
        | TurnOutcome::Failed(_)
        | TurnOutcome::FailedWith { .. } => {
            // Liveness contract: a failed turn is reported as a visible
            // `SessionError` frame with its cause, and the session returns
            // to `Idle` below so the next submit works. `HostedState::Failed`
            // is intentionally never produced (see `FailureCause`).
            if let Some((cause, reason)) = outcome.failure() {
                tracing::warn!(session = %slot.id.as_str(), %reason, "session host: turn failed");
                let message = format!(
                    "turn failed ({}): {}",
                    cause.label(),
                    sanitize_notice(reason, MAX_NOTICE_CHARS)
                );
                state.publish(
                    &slot.id,
                    SessionFrame::Session(SessionEvent::SessionError {
                        session_id: slot.id.clone(),
                        message,
                        retryable: true,
                    }),
                );
            }
            state.parked = false;
            state.arbiter.finish();
            state.ring.end_turn();
            let closed = state.record.state.is_terminal();
            if !closed {
                let depth = state.arbiter.depth();
                state.set_state(if depth > 0 {
                    HostedState::Queued { depth }
                } else {
                    HostedState::Idle
                });
            }
            host.save(&mut state);
            !closed
        }
    }
}

/// Resume a parked turn after its approval was resolved.
fn resume_parked(host: Arc<HostInner>, slot: Arc<Slot>) {
    tokio::spawn(async move {
        let cancel_rx = {
            let Ok(mut state) = slot.lock() else { return };
            if !state.parked {
                return;
            }
            state.parked = false;
            let (cancel_tx, cancel_rx) = watch::channel(false);
            state.cancel = Some(cancel_tx);
            state.set_state(HostedState::Running);
            host.save(&mut state);
            cancel_rx
        };
        let sink: Arc<dyn EventSink> = Arc::new(SlotSink {
            host: Arc::downgrade(&host),
            slot: Arc::clone(&slot),
        });
        let outcome = host
            .driver
            .resume_after_approval(&slot.id, cancel_rx, sink)
            .await
            .unwrap_or_else(|error| TurnOutcome::Failed(error.to_string()));
        if settle(&host, &slot, &outcome) {
            pump(host, slot);
        }
    });
}

/// One client's view of the host: a [`SessionPort`] (and, R18, a
/// [`ToolPort`] and [`GatewayPort`]) bound to an identity.
pub struct HostConnection {
    host: Arc<HostInner>,
    identity: ClientIdentity,
    granted: Mutex<Option<ClientCaps>>,
}

impl Drop for HostConnection {
    fn drop(&mut self) {
        if let Ok(mut connections) = self.host.connections.lock() {
            connections.remove(&self.identity.connection);
        }
    }
}

/// A `tool.*` call that passed admission steps 1-6.
struct ToolAdmission {
    slot: Arc<Slot>,
    principal: AgentPrincipal,
    tenant: Option<TenantId>,
    workspace: Option<String>,
}

impl std::fmt::Debug for HostConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostConnection")
            .field("connection", &self.identity.connection)
            .field("label", &self.identity.label)
            .finish_non_exhaustive()
    }
}

impl HostConnection {
    /// The identity this connection is bound to.
    #[must_use]
    pub fn identity(&self) -> &ClientIdentity {
        &self.identity
    }

    /// Caps granted at hello; hello is required before anything else.
    fn granted(&self) -> Result<ClientCaps, HostError> {
        if self.host.is_revoked(&self.identity) {
            return Err(HostError::Revoked);
        }
        self.granted
            .lock()
            .map_err(|_| HostError::Storage("connection state poisoned".into()))?
            .ok_or(HostError::HelloRequired)
    }

    fn admitted_slot(&self, id: &SessionId, need: Need) -> Result<Arc<Slot>, HostError> {
        let granted = self.granted()?;
        let slot = self.host.slot(id)?;
        let tenant = slot.lock()?.record.tenant.clone();
        admit(&self.identity, granted, tenant.as_ref(), need)?;
        Ok(slot)
    }

    fn hello_sync(&self, params: HelloParams) -> Result<HelloAck, HostError> {
        self.identity.validate()?;
        if self.host.is_revoked(&self.identity) {
            return Err(HostError::Revoked);
        }
        let mut granted = self
            .granted
            .lock()
            .map_err(|_| HostError::Storage("connection state poisoned".into()))?;
        if granted.is_some() {
            return Err(HostError::Protocol(
                "session.hello already completed".into(),
            ));
        }
        let ceiling = self.identity.caps;
        let wire_minor = params.wire_minor.min(SESSION_WIRE_MINOR);
        // Below minor 2 the R18 caps are masked: an older client never
        // receives a caps field it cannot decode (R18 §2.3).
        let caps = params
            .requested_caps
            .map_or(ceiling, |requested| requested.intersect(ceiling))
            .for_wire_minor(wire_minor);
        *granted = Some(caps);
        if let Ok(mut connections) = self.host.connections.lock() {
            if let Some(entry) = connections.get_mut(&self.identity.connection) {
                entry.granted = Some(caps);
            }
        }
        let features = params
            .features
            .iter()
            .filter(|feature| HOST_FEATURES.contains(&feature.as_str()))
            .filter(|feature| {
                wire_minor >= TOOL_GATEWAY_WIRE_MINOR || !R18_FEATURES.contains(&feature.as_str())
            })
            .cloned()
            .collect();
        Ok(HelloAck {
            wire_minor,
            features,
            host_epoch: self.host.epoch,
            granted: caps,
        })
    }

    fn list_sync(&self) -> Result<Vec<SessionSummary>, HostError> {
        let granted = self.granted()?;
        if !granted.observe {
            return Err(HostError::Denied("capability `observe` not granted".into()));
        }
        self.summaries()
    }

    /// Summaries of every session visible to this caller's tenant.
    fn summaries(&self) -> Result<Vec<SessionSummary>, HostError> {
        let mut out = Vec::new();
        for record in self.host.records.list()? {
            if !crate::identity::tenant_admits(
                self.identity.tenant.as_ref(),
                record.tenant.as_ref(),
            ) {
                continue;
            }
            let loaded = self
                .host
                .slots
                .lock()
                .ok()
                .and_then(|slots| slots.get(&record.session_id).cloned());
            let summary = match loaded {
                Some(slot) => self.host.summary(&*slot.lock()?),
                None => SessionSummary {
                    session_id: record.session_id.clone(),
                    title: record.title.clone(),
                    tenant: record.tenant.clone(),
                    state: record.state.clone(),
                    attached: 0,
                    updated_at: record.updated_at,
                    model: self.host.driver.model_name(&record.session_id),
                },
            };
            out.push(summary);
        }
        Ok(out)
    }

    fn attach_sync(
        &self,
        params: &AttachParams,
    ) -> Result<(AttachAck, crate::fanout::AttachmentStream), HostError> {
        let granted = self.granted()?;
        let slot = self.admitted_slot(&params.session_id, Need::Observe)?;
        let mut state = slot.lock()?;
        let generation = state.record.generation;
        let head = self.host.transcripts.head(&slot.id)?;
        state.durable_head = head;
        let (queue, stream) = attachment(slot.id.clone(), params.profile, self.host.config.queue);

        // Durable replay, at most half a queue so the stream starts healthy;
        // the rest arrives through `Lagged` + re-attach from the cursor.
        let budget = (self.host.config.queue.max_frames / 2).max(1);
        let (start, resync) = match params.from {
            Some(cursor) if cursor.generation == generation => (cursor.durable.min(head), false),
            Some(_) => (tail_start(head, params.tail_items), true),
            None => (tail_start(head, params.tail_items), false),
        };
        let replay_from = Cursor {
            generation,
            durable: start,
            live: 0,
        };
        if resync {
            queue.push(FrameEnvelope {
                session_id: slot.id.clone(),
                cursor: replay_from,
                frame: SessionFrame::Resync {
                    reason: "transcript generation changed".into(),
                    head: state.head_cursor(),
                },
            });
        }
        let frames = replay(
            self.host.transcripts.as_ref(),
            &slot.id,
            generation,
            start,
            budget,
        )?;
        let replayed_to = frames.last().map_or(start, |frame| frame.cursor.durable);
        for frame in frames {
            queue.push(frame);
        }
        if replayed_to < head {
            // More durable history than fits: let the client page on.
            queue.push(FrameEnvelope {
                session_id: slot.id.clone(),
                cursor: Cursor {
                    generation,
                    durable: replayed_to,
                    live: 0,
                },
                frame: SessionFrame::Lagged {
                    resume_from: Cursor {
                        generation,
                        durable: replayed_to,
                        live: 0,
                    },
                },
            });
        } else {
            // Pending approvals come from the approval store, never from the
            // transcript (PL-65 §3.4 step 3).
            for request in self.host.approvals.pending(&slot.id)? {
                queue.push(FrameEnvelope {
                    session_id: slot.id.clone(),
                    cursor: state.head_cursor(),
                    frame: SessionFrame::ApprovalRequested(request),
                });
            }
            // Live tail of the running turn.
            let live_from = params
                .from
                .filter(|c| c.generation == generation)
                .map(|c| c.live);
            if let Some(turn_id) = state.ring.active_turn().cloned() {
                match live_from.and_then(|live| state.ring.since(live)) {
                    Some(frames) => {
                        for (live, frame) in frames {
                            queue.push(FrameEnvelope {
                                session_id: slot.id.clone(),
                                cursor: Cursor {
                                    generation,
                                    durable: head,
                                    live,
                                },
                                frame,
                            });
                        }
                    }
                    None => {
                        let frame = state.ring.snapshot().unwrap_or(SessionFrame::Snapshot {
                            turn_id,
                            assistant_text: String::new(),
                            reasoning_collapsed: false,
                        });
                        queue.push(FrameEnvelope {
                            session_id: slot.id.clone(),
                            cursor: state.head_cursor(),
                            frame,
                        });
                    }
                }
            }
        }

        if state.record.state == HostedState::Interrupted {
            // The host restarted while a turn was in flight: tell the
            // attaching client why submits are refused and how to continue.
            queue.push(FrameEnvelope {
                session_id: slot.id.clone(),
                cursor: state.head_cursor(),
                frame: SessionFrame::Session(SessionEvent::SessionError {
                    session_id: slot.id.clone(),
                    message: INTERRUPTED_NOTICE.to_owned(),
                    retryable: true,
                }),
            });
        }

        let presence = PresenceEntry {
            label: self.identity.label.clone(),
            device: self.identity.device.clone(),
            caps: granted,
            since: jiff::Timestamp::now(),
        };
        // Replace an older attachment of the same connection.
        let connection = self.identity.connection;
        state.attachments.retain(|entry| {
            if entry.connection == connection {
                entry.queue.close(None);
                false
            } else {
                true
            }
        });
        state.attachments.push(AttachEntry {
            connection,
            device: self.identity.device.clone(),
            queue,
            presence,
        });
        state.broadcast_presence(&slot.id);
        let ack = AttachAck {
            session: self.host.summary(&state),
            granted,
            head: state.head_cursor(),
            replay_from,
            host_epoch: self.host.epoch,
        };
        Ok((ack, stream))
    }

    fn detach_sync(&self, id: &SessionId) -> Result<(), HostError> {
        self.granted()?;
        let slot = self.host.slot(id)?;
        let mut state = slot.lock()?;
        let connection = self.identity.connection;
        state.attachments.retain(|entry| {
            if entry.connection == connection {
                entry.queue.close(None);
                false
            } else {
                true
            }
        });
        state.broadcast_presence(&slot.id);
        Ok(())
    }

    fn create_sync(&self, params: &CreateParams) -> Result<HostedSessionRecord, HostError> {
        let granted = self.granted()?;
        if !granted.steer {
            return Err(HostError::Denied("capability `steer` not granted".into()));
        }
        let now = jiff::Timestamp::now();
        let session_id = SessionId::new();
        let title = params
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(|title| title.chars().take(200).collect::<String>());
        Ok(HostedSessionRecord {
            session_id,
            tenant: self.identity.tenant.clone(),
            owner_principal: self.identity.principal.id().to_owned(),
            workspace: params.workspace.clone(),
            title,
            created_at: now,
            updated_at: now,
            state: HostedState::Idle,
            host_epoch: self.host.epoch,
            generation: 0,
            // R18 §4: the creating agent (if any) owns the session's tool
            // calls; its delegates inherit access, other agents do not.
            owner_agent: self.identity.agent.as_ref().map(|agent| agent.id.clone()),
            tool_agents: Vec::new(),
        })
    }

    fn submit_sync(
        &self,
        params: SubmitParams,
    ) -> Result<(SubmitResult, Option<Arc<Slot>>), HostError> {
        let slot = self.admitted_slot(&params.session_id, Need::Steer)?;
        if self.host.is_draining() {
            return Ok((
                SubmitResult::Denied {
                    reason: "host draining".into(),
                },
                None,
            ));
        }
        let mut state = slot.lock()?;
        match state.record.state {
            HostedState::Closed => {
                return Ok((
                    SubmitResult::Denied {
                        reason: "session closed".into(),
                    },
                    None,
                ));
            }
            HostedState::Interrupted => {
                return Ok((
                    SubmitResult::Denied {
                        reason: INTERRUPTED_DENIED.into(),
                    },
                    None,
                ));
            }
            _ => {}
        }
        let head = Cursor {
            generation: state.record.generation,
            durable: state.durable_head,
            live: 0,
        };
        let input = TurnInput {
            session_id: params.session_id.clone(),
            text: params.text,
            client_msg_id: params.client_msg_id,
            submitted_by: self.identity.label.clone(),
            actor: self.identity.actor.clone(),
            origin: self.identity.connection,
        };
        let result = state
            .arbiter
            .submit(input, params.expect_head, params.force, head);
        let run = matches!(result, SubmitResult::Accepted { .. })
            && !state.arbiter.is_running()
            && !state.parked;
        if matches!(result, SubmitResult::Accepted { .. }) {
            // Visible immediately: the input waits in the queue until the
            // pump (or the running turn) picks it up.
            let depth = state.arbiter.depth();
            if depth > 0 {
                state.set_state(HostedState::Queued { depth });
            }
        }
        drop(state);
        Ok((result, run.then_some(slot)))
    }

    fn interrupt_sync(&self, params: &InterruptParams) -> Result<(), HostError> {
        let slot = self.admitted_slot(&params.session_id, Need::Steer)?;
        let state = slot.lock()?;
        if let Some(cancel) = &state.cancel {
            let _ = cancel.send(true);
        }
        Ok(())
    }

    fn resume_sync(&self, id: &SessionId) -> Result<Option<Arc<Slot>>, HostError> {
        let slot = self.admitted_slot(id, Need::Steer)?;
        let mut state = slot.lock()?;
        if state.record.state == HostedState::Interrupted {
            state.set_state(HostedState::Idle);
            self.host.save(&mut state);
        }
        let run = state.arbiter.depth() > 0 && !state.arbiter.is_running();
        drop(state);
        Ok(run.then_some(slot))
    }

    fn close_sync(&self, id: &SessionId) -> Result<(), HostError> {
        let granted = self.granted()?;
        let slot = self.host.slot(id)?;
        let mut state = slot.lock()?;
        let owner = state.record.owner_principal == self.identity.principal.id();
        let need = if owner { Need::Steer } else { Need::Control };
        admit(&self.identity, granted, state.record.tenant.as_ref(), need)?;
        state.arbiter.clear();
        if let Some(cancel) = &state.cancel {
            let _ = cancel.send(true);
        }
        state.set_state(HostedState::Closed);
        self.host.save(&mut state);
        let cursor = state.head_cursor();
        for entry in state.attachments.drain(..) {
            entry.queue.close(Some(FrameEnvelope {
                session_id: slot.id.clone(),
                cursor,
                frame: SessionFrame::Session(SessionEvent::SessionClosed {
                    session_id: slot.id.clone(),
                    reason: Some(format!("closed by {}", self.identity.label)),
                }),
            }));
        }
        Ok(())
    }

    fn respond_sync(
        &self,
        params: &ApprovalRespondParams,
    ) -> Result<(RespondResult, Option<Arc<Slot>>), HostError> {
        let granted = self.granted()?;
        // R18: approvals ask a human. An agent principal never resolves
        // one, not even with the `approve` cap (it could approve its own
        // gateway tool call).
        if self.identity.agent.is_some() {
            return Err(HostError::Denied(
                "agent principals cannot resolve approvals".into(),
            ));
        }
        let Some(session) = self.host.approvals.session_of(&params.request_id)? else {
            return Err(HostError::NotFound);
        };
        let slot = self.host.slot(&session)?;
        let tenant = slot.lock()?.record.tenant.clone();
        admit(&self.identity, granted, tenant.as_ref(), Need::Approve)?;
        let outcome = self.host.approvals.resolve(
            &params.request_id,
            params.decision,
            params.reason.clone(),
            &self.identity.actor,
            jiff::Timestamp::now(),
        )?;
        let result = match outcome {
            ResolveOutcome::Resolved => {
                // A gateway tool approval wakes its waiting call, never a
                // parked driver turn.
                let tool_approval = self
                    .host
                    .tools
                    .resolve_approval(&params.request_id, params.decision);
                let mut state = slot.lock()?;
                state.publish(
                    &slot.id,
                    SessionFrame::ApprovalResolved {
                        request_id: params.request_id.clone(),
                        decision: params.decision,
                        by: self.identity.actor_label(),
                    },
                );
                let resume = state.parked && !tool_approval;
                drop(state);
                return Ok((RespondResult::Resolved, resume.then_some(slot)));
            }
            ResolveOutcome::AlreadyResolved { by } => RespondResult::AlreadyResolved { by },
            ResolveOutcome::Expired => RespondResult::Expired,
            ResolveOutcome::NotFound => return Err(HostError::NotFound),
        };
        Ok((result, None))
    }

    fn setting_sync(&self, id: &SessionId, setting: Setting) -> Result<bool, HostError> {
        let slot = self.admitted_slot(id, Need::Control)?;
        let mut state = slot.lock()?;
        if state.arbiter.is_running() || state.parked {
            state.pending_settings.push(setting);
            Ok(false)
        } else {
            state.pending_settings.push(setting);
            Ok(true)
        }
    }

    async fn apply_settings_now(&self, id: &SessionId) -> Result<(), HostError> {
        let slot = self.host.slot(id)?;
        let settings = {
            let mut state = slot.lock()?;
            if state.arbiter.is_running() || state.parked {
                return Ok(());
            }
            std::mem::take(&mut state.pending_settings)
        };
        for setting in settings {
            self.host.driver.apply_setting(id, setting).await?;
        }
        Ok(())
    }

    async fn set(&self, id: SessionId, setting: Setting) -> Result<(), HostError> {
        if self.setting_sync(&id, setting)? {
            self.apply_settings_now(&id).await?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// R18: `tool.*` and `gateway.*`
// ---------------------------------------------------------------------------

impl HostConnection {
    /// Admission steps 1-6 of R18 §4.2 for a call in `session_id`
    /// (`tool = None` for `tool.list`/`tool.cancel`). The agent principal is
    /// re-read from the registry, so grant changes and revocations apply to
    /// the next call.
    fn tool_admission(
        &self,
        session_id: &SessionId,
        tool: Option<&str>,
    ) -> Result<ToolAdmission, HostError> {
        let granted = self.granted()?;
        let slot = self.host.slot(session_id)?;
        let record = slot.lock()?.record.clone();
        let tenant = record.tenant.clone();
        let workspace = record.workspace.clone();
        let live = match &self.identity.agent {
            Some(agent) => match self.host.tools.agents().state(&agent.id) {
                Ok(state) => Some(state),
                Err(HostError::NotFound) => {
                    return Err(HostError::Denied("unknown agent principal".into()));
                }
                Err(error) => return Err(error),
            },
            None => None,
        };
        // Session binding (R18 §4.2 step 1, same tenant is not enough): an
        // agent reaches only sessions created by itself or an ancestor in
        // its delegation chain, or bound to one of them
        // (`SessionHost::bind_tool_agent`). Anything else answers exactly
        // like an unknown session, so the refusal is no existence oracle
        // for other sessions of the tenant (and their workspaces).
        if let Some(state) = &live {
            let lineage = self.host.tools.agents().lineage(&state.principal.id)?;
            if !record.admits_tool_agent(&lineage) {
                return Err(HostError::NotFound);
            }
        }
        let mut effective = self.identity.clone();
        effective.agent = live.as_ref().map(|state| state.principal.clone());
        let known = tool.map(|name| (name, self.host.tools.serves(name)));
        let principal = admit_tool_call(&effective, granted, tenant.as_ref(), known)?.clone();
        // Step 6: revocation of the connection, device or agent principal.
        if live.as_ref().is_some_and(|state| state.revoked) || self.host.is_revoked(&self.identity)
        {
            return Err(HostError::Revoked);
        }
        Ok(ToolAdmission {
            slot,
            principal,
            tenant,
            workspace,
        })
    }

    fn list_tools_sync(&self, params: &ToolListParams) -> Result<ToolListResult, HostError> {
        let admission = self.tool_admission(&params.session_id, None)?;
        Ok(ToolListResult {
            tools: self.host.tools.descriptors_for(&admission.principal.tools),
        })
    }

    async fn call_tool_inner(
        &self,
        params: ToolCallParams,
    ) -> Result<ToolCallResultFrame, HostError> {
        let admission = self.tool_admission(&params.session_id, Some(&params.tool_name))?;
        // Step 7.
        if self.host.is_draining() {
            return Err(HostError::ToolRefused {
                refusal: ToolRefusal::Draining,
                detail: "host draining".into(),
            });
        }
        // Step 8.
        let scope = SessionScope {
            session_id: &params.session_id,
            tenant: admission.tenant.as_ref(),
            workspace: admission.workspace.as_deref(),
        };
        let sandbox = self
            .host
            .tools
            .sandbox_for(&scope, &admission.principal.id)?;
        // Step 9.
        let call = self.host.tools.reserve(
            params,
            sandbox,
            &admission.principal.id,
            self.identity.connection,
        )?;
        // A revocation between step 6 and the reservation found nothing to
        // cancel; re-check now that the call is visible to revocation.
        if self.host.is_revoked(&self.identity) {
            return Err(HostError::Revoked);
        }
        let events = SlotEvents {
            slot: admission.slot,
        };
        Ok(self.host.tools.run(call, &events).await)
    }

    fn cancel_tool_sync(&self, params: &ToolCancelParams) -> Result<(), HostError> {
        let admission = self.tool_admission(&params.session_id, None)?;
        self.host
            .tools
            .cancel_call(&params.session_id, &params.call_id, &admission.principal.id);
        Ok(())
    }

    /// Caps check for `gateway.*` (not bound to a session).
    fn gateway_admit(&self, need: Need) -> Result<(), HostError> {
        need.require(self.granted()?)
    }

    /// Host-wide mutations (`gateway.drain`, `gateway.listeners.set`) are
    /// for unscoped (local operator) callers only.
    fn require_unscoped(&self) -> Result<(), HostError> {
        if self.identity.tenant.is_some() {
            return Err(HostError::Denied(
                "host-wide gateway operation needs an unscoped caller".into(),
            ));
        }
        Ok(())
    }

    fn caller_tenant(&self) -> Option<&TenantId> {
        self.identity.tenant.as_ref()
    }

    fn status_view(&self) -> Result<GatewayStatus, HostError> {
        let caller = self.caller_tenant();
        let connections = self
            .host
            .connections_where(|entry| tenant_admits(caller, entry.tenant.as_ref()))
            .len();
        let sessions = self.summaries()?;
        let running_turns = self
            .host
            .all_slots()
            .iter()
            .filter(|slot| {
                slot.lock().is_ok_and(|state| {
                    state.arbiter.is_running()
                        && tenant_admits(caller, state.record.tenant.as_ref())
                })
            })
            .count();
        let listeners = self
            .host
            .listeners
            .lock()
            .map(|listeners| listeners.len())
            .unwrap_or(0);
        Ok(GatewayStatus {
            host_epoch: self.host.epoch,
            node: self.host.tools.node().map(str::to_owned),
            draining: self.host.is_draining(),
            connections: count(connections),
            sessions: count(sessions.len()),
            running_turns: count(running_turns),
            listeners: count(listeners),
            tools: count(self.host.tools.len()),
            sandbox_available: self.host.tools.sandbox_available(),
        })
    }

    fn status_sync(&self) -> Result<GatewayStatus, HostError> {
        self.gateway_admit(Need::GatewayRead)?;
        self.status_view()
    }

    fn connections_sync(&self) -> Result<GatewayConnectionsResult, HostError> {
        self.gateway_admit(Need::GatewayRead)?;
        let caller = self.caller_tenant();
        let mut attached: HashMap<ConnectionId, u32> = HashMap::new();
        for slot in self.host.all_slots() {
            if let Ok(state) = slot.lock() {
                for entry in &state.attachments {
                    let slot_count = attached.entry(entry.connection).or_insert(0);
                    *slot_count = slot_count.saturating_add(1);
                }
            }
        }
        let connections = self
            .host
            .connections
            .lock()
            .map_err(|_| HostError::Storage("connection table poisoned".into()))?
            .iter()
            .filter(|(_, entry)| tenant_admits(caller, entry.tenant.as_ref()))
            .map(|(id, entry)| GatewayConnectionInfo {
                connection: id.0,
                label: entry.label.clone(),
                principal: entry.principal.clone(),
                tenant: entry.tenant.clone(),
                granted: entry.granted,
                attached: attached.get(id).copied().unwrap_or(0),
                since: entry.since,
            })
            .collect();
        Ok(GatewayConnectionsResult { connections })
    }

    fn sessions_sync(&self) -> Result<Vec<SessionSummary>, HostError> {
        self.gateway_admit(Need::GatewayRead)?;
        self.summaries()
    }

    fn listeners_sync(&self) -> Result<GatewayListenersResult, HostError> {
        self.gateway_admit(Need::GatewayRead)?;
        let listeners = self
            .host
            .listeners
            .lock()
            .map_err(|_| HostError::Storage("listener table poisoned".into()))?
            .clone();
        Ok(GatewayListenersResult { listeners })
    }

    fn tools_sync(&self) -> Result<GatewayToolsResult, HostError> {
        self.gateway_admit(Need::GatewayRead)?;
        Ok(GatewayToolsResult {
            tools: self.host.tools.descriptors(),
            grants: self.host.tools.agents().rights(self.caller_tenant()),
        })
    }

    fn revoke_connection_sync(
        &self,
        params: &GatewayRevokeParams,
    ) -> Result<GatewayRevokeResult, HostError> {
        self.gateway_admit(Need::GatewayAdmin)?;
        let target = ConnectionId(params.connection);
        let visible = self
            .host
            .connections
            .lock()
            .map_err(|_| HostError::Storage("connection table poisoned".into()))?
            .get(&target)
            .is_some_and(|entry| tenant_admits(self.caller_tenant(), entry.tenant.as_ref()));
        if !visible {
            return Err(HostError::NotFound);
        }
        let already = self
            .host
            .revoked_connections
            .lock()
            .map(|revoked| revoked.contains(&target))
            .unwrap_or(false);
        if already {
            return Ok(GatewayRevokeResult { revoked: false });
        }
        let reason: String = params.reason.chars().take(200).collect();
        tracing::info!(
            connection = target.0,
            by = %self.identity.label,
            %reason,
            "session host: connection revoked through gateway.connections.revoke"
        );
        self.host.revoke_connection(target);
        Ok(GatewayRevokeResult { revoked: true })
    }

    fn drain_sync(&self, params: &GatewayDrainParams) -> Result<GatewayStatus, HostError> {
        self.gateway_admit(Need::GatewayAdmin)?;
        self.require_unscoped()?;
        tracing::info!(
            by = %self.identity.label,
            retry_after_ms = params.retry_after_ms,
            "session host: drain through gateway.drain"
        );
        self.host.drain(params.retry_after_ms);
        self.status_view()
    }

    fn set_listener_sync(
        &self,
        params: &GatewayListenerSetParams,
    ) -> Result<GatewayListenerInfo, HostError> {
        self.gateway_admit(Need::GatewayAdmin)?;
        self.require_unscoped()?;
        let mut listeners = self
            .host
            .listeners
            .lock()
            .map_err(|_| HostError::Storage("listener table poisoned".into()))?;
        let listener = listeners
            .iter_mut()
            .find(|listener| listener.name == params.name)
            .ok_or(HostError::NotFound)?;
        listener.enabled = params.enabled;
        Ok(listener.clone())
    }

    /// Admission of `gateway.tools.*`: admin cap and the agent visible to
    /// the caller's tenant (a foreign agent reads as `NotFound`).
    fn tool_rights_target(&self, params: &GatewayToolRightsParams) -> Result<ToolGrant, HostError> {
        self.gateway_admit(Need::GatewayAdmin)?;
        let tenant = self.host.tools.agents().tenant_of(&params.agent)?;
        if !tenant_admits(self.caller_tenant(), tenant.as_ref()) {
            return Err(HostError::NotFound);
        }
        Ok(ToolGrant::from_names(params.tools.iter()))
    }

    fn grant_tools_sync(
        &self,
        params: &GatewayToolRightsParams,
    ) -> Result<GatewayToolRights, HostError> {
        let tools = self.tool_rights_target(params)?;
        let principal = self.host.tools.agents().grant(&params.agent, &tools)?;
        Ok(rights_of(&principal))
    }

    fn narrow_tools_sync(
        &self,
        params: &GatewayToolRightsParams,
    ) -> Result<GatewayToolRights, HostError> {
        let tools = self.tool_rights_target(params)?;
        let principal = self.host.tools.agents().narrow(&params.agent, &tools)?;
        Ok(rights_of(&principal))
    }
}

fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn port<T: Send + 'static>(result: Result<T, HostError>) -> PortFuture<'static, T> {
    Box::pin(std::future::ready(result.map_err(PortError::from)))
}

impl SessionPort for HostConnection {
    fn hello(&self, params: HelloParams) -> PortFuture<'_, HelloAck> {
        port(self.hello_sync(params))
    }

    fn list(&self) -> PortFuture<'_, Vec<SessionSummary>> {
        port(self.list_sync())
    }

    fn create(&self, params: CreateParams) -> PortFuture<'_, SessionSummary> {
        Box::pin(
            async move {
                let record = self.create_sync(&params)?;
                self.host
                    .driver
                    .create_session(&record.session_id, record.title.as_deref())
                    .await?;
                self.host.records.save(&record)?;
                let slot = self.host.insert_slot(record)?;
                let state = slot.lock()?;
                Ok(self.host.summary(&state))
            }
            .map_err_port(),
        )
    }

    fn attach(&self, params: AttachParams) -> PortFuture<'_, (AttachAck, Box<dyn FrameSource>)> {
        port(
            self.attach_sync(&params)
                .map(|(ack, stream)| (ack, Box::new(stream) as Box<dyn FrameSource>)),
        )
    }

    fn detach(&self, session: SessionId) -> PortFuture<'_, ()> {
        port(self.detach_sync(&session))
    }

    fn history(&self, params: HistoryParams) -> PortFuture<'_, Vec<FrameEnvelope>> {
        let result = (|| {
            let slot = self.admitted_slot(&params.session_id, Need::Observe)?;
            let generation = slot.lock()?.record.generation;
            if params.before.generation != generation {
                return Err(HostError::Protocol(
                    "cursor from another transcript generation".into(),
                ));
            }
            let limit = params.limit.min(self.host.config.max_history_page) as usize;
            history_before(
                self.host.transcripts.as_ref(),
                &slot.id,
                generation,
                params.before.durable,
                limit,
            )
        })();
        port(result)
    }

    fn submit(&self, params: SubmitParams) -> PortFuture<'_, SubmitResult> {
        let result = self.submit_sync(params).map(|(result, run)| {
            if let Some(slot) = run {
                pump(Arc::clone(&self.host), slot);
            }
            result
        });
        port(result)
    }

    fn interrupt(&self, params: InterruptParams) -> PortFuture<'_, ()> {
        port(self.interrupt_sync(&params))
    }

    fn resume(&self, session: SessionId) -> PortFuture<'_, ()> {
        let result = self.resume_sync(&session).map(|run| {
            if let Some(slot) = run {
                pump(Arc::clone(&self.host), slot);
            }
        });
        port(result)
    }

    fn close(&self, session: SessionId) -> PortFuture<'_, ()> {
        port(self.close_sync(&session))
    }

    fn respond(&self, params: ApprovalRespondParams) -> PortFuture<'_, RespondResult> {
        let result = self.respond_sync(&params).map(|(result, resume)| {
            if let Some(slot) = resume {
                resume_parked(Arc::clone(&self.host), slot);
            }
            result
        });
        port(result)
    }

    fn set_model(&self, params: SetModelParams) -> PortFuture<'_, ()> {
        Box::pin(
            self.set(params.session_id, Setting::Model(params.model))
                .map_err_port(),
        )
    }

    fn set_mode(&self, params: SetModeParams) -> PortFuture<'_, ()> {
        Box::pin(
            self.set(params.session_id, Setting::Mode(params.mode))
                .map_err_port(),
        )
    }

    fn set_effort(&self, params: SetEffortParams) -> PortFuture<'_, ()> {
        Box::pin(
            self.set(params.session_id, Setting::Effort(params.effort))
                .map_err_port(),
        )
    }
}

impl ToolPort for HostConnection {
    fn list_tools(&self, params: ToolListParams) -> PortFuture<'_, ToolListResult> {
        port(self.list_tools_sync(&params))
    }

    fn call_tool(&self, params: ToolCallParams) -> PortFuture<'_, ToolCallResultFrame> {
        Box::pin(self.call_tool_inner(params).map_err_port())
    }

    fn cancel_tool(&self, params: ToolCancelParams) -> PortFuture<'_, ()> {
        port(self.cancel_tool_sync(&params))
    }
}

impl GatewayPort for HostConnection {
    fn status(&self) -> PortFuture<'_, GatewayStatus> {
        port(self.status_sync())
    }

    fn connections(&self) -> PortFuture<'_, GatewayConnectionsResult> {
        port(self.connections_sync())
    }

    fn sessions(&self) -> PortFuture<'_, Vec<SessionSummary>> {
        port(self.sessions_sync())
    }

    fn listeners(&self) -> PortFuture<'_, GatewayListenersResult> {
        port(self.listeners_sync())
    }

    fn tools(&self) -> PortFuture<'_, GatewayToolsResult> {
        port(self.tools_sync())
    }

    fn revoke_connection(
        &self,
        params: GatewayRevokeParams,
    ) -> PortFuture<'_, GatewayRevokeResult> {
        port(self.revoke_connection_sync(&params))
    }

    fn drain(&self, params: GatewayDrainParams) -> PortFuture<'_, GatewayStatus> {
        port(self.drain_sync(&params))
    }

    fn set_listener(
        &self,
        params: GatewayListenerSetParams,
    ) -> PortFuture<'_, GatewayListenerInfo> {
        port(self.set_listener_sync(&params))
    }

    fn grant_tools(&self, params: GatewayToolRightsParams) -> PortFuture<'_, GatewayToolRights> {
        port(self.grant_tools_sync(&params))
    }

    fn narrow_tools(&self, params: GatewayToolRightsParams) -> PortFuture<'_, GatewayToolRights> {
        port(self.narrow_tools_sync(&params))
    }
}

/// Map a host future's error type onto the port error.
trait MapErrPort<T> {
    fn map_err_port(self) -> impl std::future::Future<Output = Result<T, PortError>> + Send;
}

impl<T, F> MapErrPort<T> for F
where
    F: std::future::Future<Output = Result<T, HostError>> + Send,
{
    async fn map_err_port(self) -> Result<T, PortError> {
        self.await.map_err(PortError::from)
    }
}

#[cfg(test)]
mod tests;
