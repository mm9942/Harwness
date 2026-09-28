//! The session host: composition of records, replay, live ring, fan-out,
//! arbiter, approvals and the runtime driver (W00 §2.4, PL-65 §2–§3).
//!
//! Locking: one `std::sync::Mutex` per session slot. It is held only for
//! short, non-async sections (state changes, fan-out pushes, the attach
//! replay read), never across an `.await`, so driver events are recorded
//! without waiting on any client.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use harw_protocol::session_wire::{
    ApprovalRespondParams, AttachAck, AttachParams, CreateParams, HelloAck, HelloParams,
    HistoryParams, InterruptParams, RespondResult, SESSION_WIRE_MINOR, SessionSummary,
    SetEffortParams, SetModeParams, SetModelParams, SubmitParams, SubmitResult, features,
};
use harw_protocol::{
    ClientCaps, Cursor, FrameEnvelope, FrameSource, HostedState, PortError, PortFuture,
    PresenceEntry, SessionEvent, SessionFrame, SessionPort, TurnEvent,
};
use harw_types::{DeviceId, SessionId};
use tokio::sync::watch;

use crate::approvals::{ApprovalBackend, ResolveOutcome};
use crate::arbiter::{Arbiter, ArbiterLimits};
use crate::driver::{DriverEvent, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome};
use crate::error::HostError;
use crate::fanout::{AttachmentQueue, QueueLimits, attachment};
use crate::identity::{ClientIdentity, ConnectionId, Need, admit};
use crate::live_ring::LiveRing;
use crate::record::{HostedSessionRecord, RecordStore};
use crate::replay::{TranscriptSource, history_before, replay, tail_start};

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
const HOST_FEATURES: &[&str] = &[features::COMPACT, features::HISTORY, features::CHILD_FRAMES];

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
        if self
            .inner
            .draining
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(HostError::Denied("host is draining".into()));
        }
        Ok(HostConnection {
            host: Arc::clone(&self.inner),
            identity,
            granted: Mutex::new(None),
        })
    }

    /// Revoke a device: refuse new calls, close its streams with `Revoked`,
    /// drop its queued inputs (W00 §7, REV-01).
    pub fn revoke_device(&self, device: &DeviceId) {
        if let Ok(mut revoked) = self.inner.revoked_devices.lock() {
            revoked.insert(device.clone());
        }
        self.inner
            .close_matching(|entry| entry.device.as_ref() == Some(device), revoked_frame);
    }

    /// Revoke one connection (for example when its security context expired).
    pub fn revoke_connection(&self, connection: ConnectionId) {
        if let Ok(mut revoked) = self.inner.revoked_connections.lock() {
            revoked.insert(connection);
        }
        self.inner
            .close_matching(|entry| entry.connection == connection, revoked_frame);
    }

    /// Drain: refuse new connections, tell every client when to come back,
    /// cancel running turns (they end `Interrupted` and resume after the
    /// restart by policy).
    pub fn drain(&self, retry_after_ms: u64) {
        self.inner
            .draining
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.inner.close_matching(
            |_| true,
            move |_| SessionFrame::HostDraining { retry_after_ms },
        );
        for slot in self.inner.all_slots() {
            if let Ok(state) = slot.lock()
                && let Some(cancel) = &state.cancel
            {
                let _ = cancel.send(true);
            }
        }
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
        device || connection
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
        TurnOutcome::Completed | TurnOutcome::Interrupted | TurnOutcome::Failed(_) => {
            if let TurnOutcome::Failed(reason) = outcome {
                tracing::warn!(session = %slot.id.as_str(), %reason, "session host: turn failed");
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

/// One client's view of the host: a [`SessionPort`] bound to an identity.
pub struct HostConnection {
    host: Arc<HostInner>,
    identity: ClientIdentity,
    granted: Mutex<Option<ClientCaps>>,
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
        let caps = params
            .requested_caps
            .map_or(ceiling, |requested| requested.intersect(ceiling));
        *granted = Some(caps);
        let features = params
            .features
            .iter()
            .filter(|feature| HOST_FEATURES.contains(&feature.as_str()))
            .cloned()
            .collect();
        Ok(HelloAck {
            wire_minor: params.wire_minor.min(SESSION_WIRE_MINOR),
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
        })
    }

    fn submit_sync(
        &self,
        params: SubmitParams,
    ) -> Result<(SubmitResult, Option<Arc<Slot>>), HostError> {
        let slot = self.admitted_slot(&params.session_id, Need::Steer)?;
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
                        reason: "session interrupted; session.resume first".into(),
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
                let mut state = slot.lock()?;
                state.publish(
                    &slot.id,
                    SessionFrame::ApprovalResolved {
                        request_id: params.request_id.clone(),
                        decision: params.decision,
                        by: self.identity.actor_label(),
                    },
                );
                let resume = state.parked;
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
