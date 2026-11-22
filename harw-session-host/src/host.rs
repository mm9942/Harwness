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
//!
//! Sharing profile sessions (plan §3.2-§3.5, contract K-W1 §8.3): with a
//! [`ProfileShare`] ([`SessionHost::open_with_profile`]) the host lists, pages
//! and reads TUI sessions of the profile store without writing anything.
//! Steering one adopts it first (a hosted record with the caller's creator
//! authority, created at most once). Every turn of a shared session runs
//! under the session lease (`HolderKind::Gateway`); a lease held elsewhere
//! answers `BUSY` (or `SubmitResult::Denied` for a minor-2 client). Attached
//! profile sessions without a host turn are watched for transcript growth and
//! lease changes every [`HostConfig::profile_head_poll`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use harw_protocol::methods::{
    METHOD_CATALOG_GET, METHOD_COMMAND_RUN, METHOD_COMMANDS_LIST, METHOD_CONFIG_GET,
    METHOD_CONFIG_RESET, METHOD_CONFIG_SET, METHOD_SECRET_PUT, METHOD_STEPUP_CANCEL,
    METHOD_STEPUP_CONFIRM,
};
use harw_protocol::session_wire::{
    ApprovalRespondParams, ArchiveParams, AttachAck, AttachParams, CatalogGetParams, CatalogResult,
    CommandEffect, CommandRunParams, CommandRunResult, CommandsListParams, CommandsListResult,
    ConfigGetParams, ConfigGetResult, ConfigResetParams, ConfigSetParams, CreateInParams,
    CreateParams, DEFAULT_PAGE_LIMIT, GatewayConnectionInfo, GatewayConnectionsResult,
    GatewayDrainParams, GatewayListenerInfo, GatewayListenerSetParams, GatewayListenersResult,
    GatewayRevokeParams, GatewayRevokeResult, GatewayStatus, GatewayToolRights,
    GatewayToolRightsParams, GatewayToolsResult, HelloAck, HelloParams, HistoryParams, HolderInfo,
    InterruptParams, MANAGE_WIRE_MINOR, MAX_PAGE_LIMIT, NO_PROJECT_KEY, PROJECTS_WIRE_MINOR,
    PageParams, PageResult, PrincipalSummary, ProjectsParams, ProjectsResult, ReadOnlyReason,
    RenameParams, RespondResult, RowOrigin, SESSION_WIRE_MINOR, SecretPutParams, SecretPutResult,
    SessionRow, SessionSummary, SetEffortParams, SetModeParams, SetModelParams, SetProjectParams,
    ShareInfo, StepUpCancelParams, StepUpConfirmParams, SubmitParams, SubmitResult,
    TOOL_GATEWAY_WIRE_MINOR, ToolCallParams, ToolCallResultFrame, ToolCancelParams, ToolListParams,
    ToolListResult, features, refusal_codes,
};
use harw_protocol::{
    ApprovalRequest, ClientCaps, Cursor, FrameEnvelope, FrameSource, GatewayPort, HostedState,
    PortError, PortFuture, PresenceEntry, SessionEvent, SessionFrame, SessionPort, ToolPort,
    ToolRefusal, TurnEvent,
};
use harw_session_store::{
    HolderKind, LastOrigin, SessionLease, SessionStoreError, TitleSource, meta, peek_holder,
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
use crate::index::{
    DENIED_NOT_SHARED, DENIED_READ_ONLY, ProfileRow, ProfileShare, ShareMode, busy_reason,
    decode_cursor, encode_cursor, wire_holder, wire_last_origin,
};
use crate::live_ring::LiveRing;
use crate::ops::{OpsCaller, OpsPort, OpsRun, OpsSession, SessionAction, tier_for_caps};
use crate::record::{CreateOutcome, HostedSessionRecord, RecordStore};
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
    /// Interval of the head watch of attached profile sessions (plan §3.2
    /// step 10).
    pub profile_head_poll: Duration,
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
            profile_head_poll: Duration::from_secs(2),
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
    features::COMPACT_REASONING,
    features::JOB_FRAMES,
    features::PROJECTS,
    features::MANAGE,
    features::STEPUP,
    features::IMAGES,
];

/// Features that exist only from [`TOOL_GATEWAY_WIRE_MINOR`] on.
const R18_FEATURES: &[&str] = &[features::TOOLS, features::GATEWAY];

/// Features that exist only from [`PROJECTS_WIRE_MINOR`] on. Granted with or
/// without sharing (a client then shows its empty state).
const PROJECT_FEATURES: &[&str] = &[features::PROJECTS];

/// Features that exist only from [`MANAGE_WIRE_MINOR`] on (contract K-W3c
/// §8.2 no. 9). `manage` needs installed ops, `stepup` additionally
/// `manage` and [`OpsPort::stepup_enabled`].
const MANAGE_FEATURES: &[&str] = &[features::MANAGE, features::STEPUP];

/// Answer of a session action on a profile session without a host record.
const TEXT_SESSION_NOT_HOSTED: &str = "Session läuft nicht auf dem Gateway; erst übernehmen";
/// Answer of a session action without a session (normally refused by the ops backend already).
const TEXT_SESSION_REQUIRED: &str = "braucht eine Session";

/// Denied: the project key is unknown or its root failed validation.
const DENIED_UNKNOWN_PROJECT: &str = "unbekanntes Projekt";
/// Denied: the session's first record is not a CLI session.
const DENIED_FOREIGN_ORIGIN: &str = "Session stammt nicht aus der TUI";
/// Denied: the session has no valid working directory.
const DENIED_NO_WORKDIR: &str = "Session hat keinen gültigen Arbeitsordner";

/// One live connection as `gateway.connections.list` shows it.
struct ConnEntry {
    identity: ClientIdentity,
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
    /// The connection negotiated feature `projects` (receives `holder`).
    projects: bool,
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
    /// Profile session read without a hosted record: `record` is a
    /// synthetic in-memory view and is never saved.
    ephemeral: bool,
    /// Session lease held for the host's turns (shared profile store only),
    /// from the submit until the final turn outcome.
    lease: Option<SessionLease>,
    /// Transcript head the driver's cached state reflects (set when a host
    /// turn ends). `None`: the driver hydrates from the transcript.
    driver_head: Option<u64>,
    /// Lease holder last announced to `projects` attachments.
    seen_holder: Option<HolderInfo>,
}

impl SlotState {
    fn new(record: HostedSessionRecord, limits: ArbiterLimits, head: u64, ephemeral: bool) -> Self {
        Self {
            record,
            arbiter: Arbiter::new(limits),
            ring: LiveRing::default(),
            durable_head: head,
            attachments: Vec::new(),
            cancel: None,
            pending_settings: Vec::new(),
            parked: false,
            ephemeral,
            lease: None,
            driver_head: None,
            seen_holder: None,
        }
    }
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
    execution_creators: Mutex<HashMap<SessionId, ClientIdentity>>,
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
    /// Sharing of profile (TUI) sessions; `None` without sharing.
    profile: Option<ProfileShare>,
    /// Minor-4 operations backend (`manage`/`stepup`); empty until
    /// [`SessionHost::install_ops`].
    ops: std::sync::OnceLock<Arc<dyn OpsPort>>,
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
        Self::open_inner(config, driver, transcripts, approvals, tools, None)
    }

    /// Like [`SessionHost::open_with_tools`], sharing the profile sessions of
    /// `share.index` (plan §3.2). `transcripts` must read the same store as
    /// `share.index.sessions_root()`. [`SessionHost::open`] and
    /// [`SessionHost::open_with_tools`] never share.
    pub fn open_with_profile(
        config: HostConfig,
        driver: Arc<dyn TurnDriver>,
        transcripts: Arc<dyn TranscriptSource>,
        approvals: Arc<dyn ApprovalBackend>,
        tools: ToolHost,
        share: ProfileShare,
    ) -> Result<Self, HostError> {
        Self::open_inner(config, driver, transcripts, approvals, tools, Some(share))
    }

    fn open_inner(
        config: HostConfig,
        driver: Arc<dyn TurnDriver>,
        transcripts: Arc<dyn TranscriptSource>,
        approvals: Arc<dyn ApprovalBackend>,
        tools: ToolHost,
        profile: Option<ProfileShare>,
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
                execution_creators: Mutex::new(HashMap::new()),
                slots: Mutex::new(HashMap::new()),
                revoked_devices: Mutex::new(HashSet::new()),
                revoked_connections: Mutex::new(HashSet::new()),
                draining: std::sync::atomic::AtomicBool::new(false),
                tools,
                connections: Mutex::new(BTreeMap::new()),
                listeners: Mutex::new(Vec::new()),
                profile,
                ops: std::sync::OnceLock::new(),
            }),
        })
    }

    /// Host epoch (increments on every open).
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.inner.epoch
    }

    /// Install the operations backend of the minor-4 `manage`/`stepup`
    /// methods (contract K-W3c §8.2). Without it hello never grants these
    /// features and the methods are denied. Installing twice is refused.
    pub fn install_ops(&self, ops: Arc<dyn OpsPort>) -> Result<(), HostError> {
        self.inner
            .ops
            .set(ops)
            .map_err(|_| HostError::Protocol("ops already installed".into()))
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
            identity: identity.clone(),
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
            wire_minor: AtomicU32::new(0),
            projects: AtomicBool::new(false),
            manage: AtomicBool::new(false),
            stepup: AtomicBool::new(false),
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

    /// Authenticated facts still retained by this host, including detached
    /// session creators. Only trusted policy monitors should consume these.
    #[must_use]
    pub fn authenticated_identities(&self) -> Vec<ClientIdentity> {
        let mut identities = BTreeMap::new();
        if let Ok(connections) = self.inner.connections.lock() {
            identities.extend(
                connections
                    .iter()
                    .map(|(id, entry)| (*id, entry.identity.clone())),
            );
        }
        if let Ok(creators) = self.inner.execution_creators.lock() {
            identities.extend(
                creators
                    .values()
                    .map(|identity| (identity.connection, identity.clone())),
            );
        }
        identities
            .into_values()
            .filter(|identity| !self.inner.is_revoked(identity))
            .collect()
    }

    /// Devices whose authenticated facts remain live in this process, including
    /// creators whose transports detached. Used by trusted policy refresh.
    #[must_use]
    pub fn authenticated_devices(&self) -> Vec<DeviceId> {
        self.authenticated_identities()
            .into_iter()
            .filter_map(|identity| identity.device)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    /// Invalidate old credentials when trusted device policy changes or cannot
    /// be read. This revokes existing connection ids, including detached creator
    /// facts, without banning freshly authenticated connections for the device.
    pub fn invalidate_device_policy(&self, device: &DeviceId) {
        let mut connections = self
            .inner
            .connections_where(|entry| entry.device.as_ref() == Some(device));
        if let Ok(creators) = self.inner.execution_creators.lock() {
            connections.extend(
                creators
                    .values()
                    .filter(|identity| identity.device.as_ref() == Some(device))
                    .map(|identity| identity.connection),
            );
        }
        connections.sort_unstable();
        connections.dedup();
        for connection in connections {
            self.inner.revoke_connection(connection);
        }
        if let Ok(mut creators) = self.inner.execution_creators.lock() {
            creators.retain(|_, identity| identity.device.as_ref() != Some(device));
        }
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

    /// Spawn the heartbeat timer on the current Tokio runtime. With sharing
    /// it also drives the profile head watch
    /// ([`SessionHost::poll_profile_heads`]) every
    /// [`HostConfig::profile_head_poll`].
    #[must_use]
    pub fn spawn_heartbeat(&self) -> tokio::task::JoinHandle<()> {
        let host = self.clone();
        let every = self.inner.config.heartbeat;
        let poll = self
            .inner
            .config
            .profile_head_poll
            .max(Duration::from_millis(100));
        let watch_heads = self.inner.share().is_some();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.tick().await;
            let mut heads = tokio::time::interval(poll);
            heads.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            heads.tick().await;
            loop {
                tokio::select! {
                    _ = tick.tick() => host.heartbeat(),
                    _ = heads.tick(), if watch_heads => host.poll_profile_heads(),
                }
            }
        })
    }

    /// One round of the profile head watch (plan §3.2 step 10): for every
    /// attached shared session without a host turn, send transcript records
    /// written elsewhere (as replay frames) and lease changes (`holder`
    /// frames, `projects` connections only). Never writes.
    pub fn poll_profile_heads(&self) {
        let Some(share) = self.inner.share() else {
            return;
        };
        let root = share.index.sessions_root();
        for slot in self.inner.all_slots() {
            let Ok(mut state) = slot.lock() else { continue };
            state.attachments.retain(|entry| !entry.queue.is_closed());
            if state.attachments.is_empty()
                || state.lease.is_some()
                || state.arbiter.is_running()
                || state.parked
            {
                continue;
            }
            self.inner.observe_head(&slot.id, &mut state);
            match peek_holder(root, &slot.id) {
                Ok(holder) => {
                    let holder = holder.as_ref().map(wire_holder);
                    if holder != state.seen_holder {
                        state.publish_holder(&slot.id, holder);
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, session = %slot.id.as_str(), "session host: lease peek failed");
                }
            }
        }
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

    /// A record supplies a ceiling only. The process keeps authenticated
    /// creator facts across transport detach, but never across a restart.
    /// Every new execution checks live revocation and agent registration.
    fn execution_admission(
        &self,
        record: &HostedSessionRecord,
    ) -> Result<Option<crate::driver::ExecutionAdmission>, HostError> {
        if record.creator_execution.is_none() {
            // Dedicated-host compatibility for legacy transcripts. A shared
            // listener must not admit legacy records without a trusted migration.
            return Ok(None);
        }
        let connections = self
            .connections
            .lock()
            .map_err(|_| HostError::Storage("connection table poisoned".into()))?;
        let mut creators = self
            .execution_creators
            .lock()
            .map_err(|_| HostError::Storage("execution creator table poisoned".into()))?;
        let cached = creators.get(&record.session_id).cloned();
        let identities = connections
            .values()
            .filter(|entry| entry.granted.is_some())
            .map(|entry| &entry.identity)
            .chain(cached.iter());
        let mut admitted: Option<crate::driver::ExecutionAdmission> = None;
        let mut refreshed = None;
        for identity in identities {
            if self.is_revoked(identity) {
                continue;
            }
            if let Some(agent) = &identity.agent
                && self
                    .check_agent_registration(agent, identity.tenant.as_ref())
                    .is_err()
            {
                continue;
            }
            if let Ok(request) =
                record.revalidate_creator_execution(identity, harw_types::PermissionTier::Owner)
            {
                if admitted
                    .as_ref()
                    .is_none_or(|previous| request.maximum_tier <= previous.maximum_tier())
                {
                    refreshed = Some(identity.clone());
                }
                // Multiple authenticated credentials can only narrow.
                admitted = Some(match admitted {
                    Some(previous) => crate::driver::ExecutionAdmission::new(
                        identity.principal.clone(),
                        request.maximum_tier.min(previous.maximum_tier()),
                        request.approval_actor,
                    ),
                    None => crate::driver::ExecutionAdmission::new(
                        identity.principal.clone(),
                        request.maximum_tier,
                        request.approval_actor,
                    ),
                });
            }
        }
        if let Some(identity) = refreshed {
            creators.insert(record.session_id.clone(), identity);
        }
        admitted.map(Some).ok_or_else(|| {
            HostError::Denied("execution requires fresh authenticated creator authority".into())
        })
    }

    async fn prepare_execution(&self, slot: &Slot) -> Result<(), HostError> {
        let admission = self.execution_admission(&slot.lock()?.record)?;
        if let Some(admission) = admission {
            self.driver.prepare_execution(&slot.id, admission).await?;
        }
        Ok(())
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
            if let Ok(state) = slot.lock()
                && let Some(cancel) = &state.cancel
            {
                let _ = cancel.send(true);
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
            // Revocation also invalidates a detached creator's authority.
            // Cancellation does not depend on that creator still having an
            // attachment or on who submitted the current turn.
            if state.record.creator_execution.is_some()
                && self.execution_admission(&state.record).is_err()
                && let Some(cancel) = &state.cancel
            {
                let _ = cancel.send(true);
            }
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
            state: Mutex::new(SlotState::new(record, self.config.arbiter, head, false)),
        });
        let mut slots = self
            .slots
            .lock()
            .map_err(|_| HostError::Storage("slot table poisoned".into()))?;
        Ok(Arc::clone(slots.entry(id.clone()).or_insert(slot)))
    }

    /// The slot already in memory, without touching the record store.
    fn loaded_slot(&self, id: &SessionId) -> Option<Arc<Slot>> {
        self.slots
            .lock()
            .ok()
            .and_then(|slots| slots.get(id).cloned())
    }

    fn insert_slot(&self, record: HostedSessionRecord) -> Result<Arc<Slot>, HostError> {
        let id = record.session_id.clone();
        let slot = Arc::new(Slot {
            id: id.clone(),
            state: Mutex::new(SlotState::new(record, self.config.arbiter, 0, false)),
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
        if state.ephemeral {
            // A profile session read without adoption never gets a record.
            return;
        }
        state.record.updated_at = jiff::Timestamp::now();
        if let Err(error) = self.records.save(&state.record) {
            tracing::warn!(%error, session = %state.record.session_id.as_str(), "session host: record save failed");
        }
    }

    // -----------------------------------------------------------------------
    // Profile sharing (contract K-W1 §8.3)
    // -----------------------------------------------------------------------

    /// The active share (`None` without a profile or in mode `off`).
    fn share(&self) -> Option<&ProfileShare> {
        self.profile
            .as_ref()
            .filter(|share| share.settings.mode != ShareMode::Off)
    }

    /// The active share, if a caller of tenant `caller` may see its rows.
    fn share_for(&self, caller: Option<&TenantId>) -> Option<&ProfileShare> {
        self.share()
            .filter(|share| tenant_admits(caller, Some(&share.settings.tenant)))
    }

    /// The slot of `id`: a hosted one, or a read-only view of a visible
    /// profile session (no record, no file written; plan §3.2 step 5).
    fn profile_slot(
        &self,
        id: &SessionId,
        caller: Option<&TenantId>,
    ) -> Result<Arc<Slot>, HostError> {
        match self.slot(id) {
            Err(HostError::NotFound) => {}
            other => return other,
        }
        let share = self.share_for(caller).ok_or(HostError::NotFound)?;
        let row = share.index.row(id)?.ok_or(HostError::NotFound)?;
        let head = self.transcripts.head(id)?;
        let now = jiff::Timestamp::now();
        let record = HostedSessionRecord {
            session_id: id.clone(),
            tenant: Some(share.settings.tenant.clone()),
            owner_principal: String::new(),
            creator_execution: None,
            workspace: None,
            title: Some(row.title),
            created_at: now,
            updated_at: row.updated_at,
            state: HostedState::Idle,
            host_epoch: self.epoch,
            generation: 0,
            owner_agent: None,
            tool_agents: Vec::new(),
        };
        let slot = Arc::new(Slot {
            id: id.clone(),
            state: Mutex::new(SlotState::new(record, self.config.arbiter, head, true)),
        });
        let mut slots = self
            .slots
            .lock()
            .map_err(|_| HostError::Storage("slot table poisoned".into()))?;
        Ok(Arc::clone(slots.entry(id.clone()).or_insert(slot)))
    }

    /// Install an adopted record: upgrade a read-only view in place (its
    /// attachments stay) or build a new slot at the current head.
    fn publish_record(&self, record: HostedSessionRecord) -> Result<Arc<Slot>, HostError> {
        let id = record.session_id.clone();
        if let Some(slot) = self.loaded_slot(&id) {
            let mut state = slot.lock()?;
            if state.ephemeral {
                state.record = record;
                state.ephemeral = false;
            }
            drop(state);
            return Ok(slot);
        }
        let head = self.transcripts.head(&id)?;
        let slot = Arc::new(Slot {
            id: id.clone(),
            state: Mutex::new(SlotState::new(record, self.config.arbiter, head, false)),
        });
        let mut slots = self
            .slots
            .lock()
            .map_err(|_| HostError::Storage("slot table poisoned".into()))?;
        Ok(Arc::clone(slots.entry(id).or_insert(slot)))
    }

    /// Take the session lease for a host turn (plan §3.5). No-op without
    /// sharing or when the slot holds it already. Afterwards the head check:
    /// a transcript that moved since the driver's last turn here makes the
    /// driver drop its cached state and hydrate again.
    ///
    /// # Errors
    /// [`HostError::Busy`] with the display reason when another process
    /// holds the lease; storage errors otherwise.
    fn ensure_lease(&self, id: &SessionId, state: &mut SlotState) -> Result<(), HostError> {
        let Some(share) = self.share() else {
            return Ok(());
        };
        if state.lease.is_some() {
            return Ok(());
        }
        let lease =
            match SessionLease::acquire(share.index.sessions_root(), id, HolderKind::Gateway) {
                Ok(lease) => lease,
                Err(SessionStoreError::LeaseHeld { holder, .. }) => {
                    return Err(HostError::Busy(
                        busy_reason(holder.map(|holder| holder.kind)).to_owned(),
                    ));
                }
                Err(error) => return Err(error.into()),
            };
        let head = self.transcripts.head(id)?;
        if state.driver_head.is_some_and(|seen| seen != head) {
            tracing::info!(
                session = %id.as_str(),
                "session host: transcript changed outside the host; rehydrating"
            );
            self.forget_driver_state(id)?;
            state.driver_head = None;
        }
        state.durable_head = head;
        let holder = Some(wire_holder(lease.holder()));
        state.lease = Some(lease);
        state.publish_holder(id, holder);
        Ok(())
    }

    /// Drop the driver's cached core state of `id` ([OFFEN-1]).
    fn forget_driver_state(&self, id: &SessionId) -> Result<(), HostError> {
        self.driver.forget_session(id)
    }

    /// Release the turn lease. After a turn, `last_origin = gateway` is
    /// recorded first (still under the lease).
    fn release_lease(&self, id: &SessionId, state: &mut SlotState, turn_ran: bool) {
        let Some(lease) = state.lease.take() else {
            return;
        };
        if turn_ran
            && let Some(share) = self.share()
            && let Err(error) = meta::set_last_origin(
                share.index.sessions_root(),
                id,
                LastOrigin::Gateway,
                jiff::Timestamp::now(),
            )
        {
            tracing::warn!(%error, session = %id.as_str(), "session host: last_origin not recorded");
        }
        drop(lease);
        state.publish_holder(id, None);
    }

    /// Send records another writer appended since the cached head.
    fn observe_head(&self, id: &SessionId, state: &mut SlotState) {
        let head = match self.transcripts.head(id) {
            Ok(head) => head,
            Err(error) => {
                tracing::debug!(%error, session = %id.as_str(), "session host: head read failed");
                return;
            }
        };
        if head == state.durable_head {
            return;
        }
        let generation = state.record.generation;
        if head < state.durable_head {
            // Rewritten elsewhere (retention, repair): start over.
            state.durable_head = head;
            let cursor = state.head_cursor();
            state.publish_side(
                id,
                SessionFrame::Resync {
                    reason: "transcript rewritten outside the host".into(),
                    head: cursor,
                },
            );
            return;
        }
        let budget = (self.config.queue.max_frames / 2).max(1);
        let frames = match replay(
            self.transcripts.as_ref(),
            id,
            generation,
            state.durable_head,
            budget,
        ) {
            Ok(frames) => frames,
            Err(error) => {
                tracing::debug!(%error, session = %id.as_str(), "session host: head replay failed");
                return;
            }
        };
        let reached = frames
            .last()
            .map_or(head, |frame| frame.cursor.durable.max(state.durable_head));
        state.attachments.retain(|entry| !entry.queue.is_closed());
        for frame in frames {
            for entry in &state.attachments {
                entry.queue.push(frame.clone());
            }
        }
        state.durable_head = reached.min(head);
    }

    /// Projects with a running or parked host turn, by project key
    /// (`""` = no project) [OFFEN-8: TUI leases are not counted].
    fn active_by_project(
        &self,
        share: &ProfileShare,
        caller: Option<&TenantId>,
    ) -> HashMap<String, u32> {
        let active: Vec<SessionId> = self
            .all_slots()
            .iter()
            .filter(|slot| {
                slot.lock().is_ok_and(|state| {
                    (state.arbiter.is_running() || state.parked)
                        && tenant_admits(caller, state.record.tenant.as_ref())
                })
            })
            .map(|slot| slot.id.clone())
            .collect();
        let mut out: HashMap<String, u32> = HashMap::new();
        for id in active {
            let key = match share.index.row(&id) {
                Ok(Some(row)) => row.project_key.unwrap_or_default(),
                Ok(None) | Err(_) => continue,
            };
            let entry = out.entry(key).or_insert(0);
            *entry = entry.saturating_add(1);
        }
        out
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

    /// Announce a lease change to the attachments that negotiated
    /// `projects` (older clients would only decode `Unknown`).
    fn publish_holder(&mut self, session_id: &SessionId, holder: Option<HolderInfo>) {
        self.seen_holder.clone_from(&holder);
        self.attachments.retain(|entry| !entry.queue.is_closed());
        let envelope = FrameEnvelope {
            session_id: session_id.clone(),
            cursor: self.head_cursor(),
            frame: SessionFrame::Holder {
                session_id: session_id.clone(),
                holder,
            },
        };
        for entry in self.attachments.iter().filter(|entry| entry.projects) {
            entry.queue.push(envelope.clone());
        }
    }

    /// Fan out a frame that does not belong to a driver turn (gateway tool
    /// events of an agent-run turn, and child-agent and agent-job frames from
    /// the driver). Buffered in the live ring only while a driver turn is
    /// active there, so it never outlives the ring's turn.
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
            } => state.publish_side(
                id,
                SessionFrame::Child {
                    agent,
                    parent,
                    role,
                    event,
                },
            ),
            DriverEvent::Job {
                work_id,
                kind,
                state: job_state,
                summary,
            } => state.publish_side(
                id,
                SessionFrame::Job {
                    work_id,
                    kind,
                    state: job_state,
                    summary,
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
            let next = {
                let Ok(mut state) = slot.lock() else { return };
                if state.parked || state.record.state.is_terminal() {
                    return;
                }
                let Some(input) = state.arbiter.next_input() else {
                    return;
                };
                // Normally taken at submit already; a queue resumed after a
                // restart takes it here (plan §3.5: no turn without lease).
                match host.ensure_lease(&slot.id, &mut state) {
                    Err(error) => Err(error),
                    Ok(()) => {
                        let (cancel_tx, cancel_rx) = watch::channel(false);
                        state.cancel = Some(cancel_tx);
                        let depth = state.arbiter.depth();
                        state.set_state(if depth > 0 {
                            HostedState::Queued { depth }
                        } else {
                            HostedState::Running
                        });
                        host.save(&mut state);
                        Ok((
                            input,
                            std::mem::take(&mut state.pending_settings),
                            cancel_rx,
                        ))
                    }
                }
            };
            let (input, settings, cancel_rx) = match next {
                Ok(next) => next,
                Err(error) => {
                    if !settle(&host, &slot, &TurnOutcome::Failed(error.to_string())) {
                        return;
                    }
                    continue;
                }
            };
            if let Err(error) = host.prepare_execution(&slot).await {
                if !settle(&host, &slot, &TurnOutcome::Failed(error.to_string())) {
                    return;
                }
                continue;
            }
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
    // The driver's state now reflects the transcript up to here (head check
    // before the next leased turn).
    state.driver_head = Some(state.durable_head);
    match outcome {
        TurnOutcome::AwaitingApproval => {
            // The lease stays through the approval pause (plan §3.5).
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
            // Final outcome: keep the lease only for a queued next turn.
            if closed || state.arbiter.depth() == 0 {
                host.release_lease(&slot.id, &mut state, true);
            }
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
            // Held since the turn parked; re-taken only if it was lost.
            if let Err(error) = host.ensure_lease(&slot.id, &mut state) {
                drop(state);
                settle(&host, &slot, &TurnOutcome::Failed(error.to_string()));
                return;
            }
            cancel_rx
        };
        let sink: Arc<dyn EventSink> = Arc::new(SlotSink {
            host: Arc::downgrade(&host),
            slot: Arc::clone(&slot),
        });
        if let Err(error) = host.prepare_execution(&slot).await {
            settle(&host, &slot, &TurnOutcome::Failed(error.to_string()));
            return;
        }
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

/// Synchronous rollback also runs when a caller drops the create future.
/// A creation never executes a model/tool turn. Retained transcript metadata,
/// if a custom driver writes it, has no hosted record and grants no authority.
struct UnpublishedCreation<'a> {
    host: &'a HostInner,
    session_id: SessionId,
    armed: bool,
}

impl Drop for UnpublishedCreation<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(mut slots) = self.host.slots.lock() {
            slots.remove(&self.session_id);
        }
        if let Err(cleanup) = self
            .host
            .driver
            .discard_unpublished_session(&self.session_id)
        {
            tracing::error!(%cleanup, "session host: failed creation runtime cleanup");
        }
        if let Ok(mut creators) = self.host.execution_creators.lock() {
            creators.remove(&self.session_id);
        }
        if let Err(cleanup) = self.host.records.remove(&self.session_id) {
            tracing::error!(%cleanup, "session host: failed creation record cleanup");
        }
    }
}

/// One client's view of the host: a [`SessionPort`] (and, R18, a
/// [`ToolPort`] and [`GatewayPort`]) bound to an identity.
pub struct HostConnection {
    host: Arc<HostInner>,
    identity: ClientIdentity,
    granted: Mutex<Option<ClientCaps>>,
    /// Negotiated wire minor (0 before hello).
    wire_minor: AtomicU32,
    /// Feature `projects` was granted at hello.
    projects: AtomicBool,
    /// Feature `manage` was granted at hello (minor 4).
    manage: AtomicBool,
    /// Feature `stepup` was granted at hello (minor 4).
    stepup: AtomicBool,
}

impl Drop for HostConnection {
    fn drop(&mut self) {
        if let Ok(mut connections) = self.host.connections.lock() {
            connections.remove(&self.identity.connection);
        }
        if let Some(ops) = self.host.ops.get() {
            ops.connection_closed(&self.identity);
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
        if let Ok(mut connections) = self.host.connections.lock()
            && let Some(entry) = connections.get_mut(&self.identity.connection)
        {
            entry.granted = Some(caps);
        }
        let mut granted_features: Vec<String> = params
            .features
            .iter()
            .filter(|feature| HOST_FEATURES.contains(&feature.as_str()))
            .filter(|feature| {
                wire_minor >= TOOL_GATEWAY_WIRE_MINOR || !R18_FEATURES.contains(&feature.as_str())
            })
            .filter(|feature| {
                wire_minor >= PROJECTS_WIRE_MINOR || !PROJECT_FEATURES.contains(&feature.as_str())
            })
            .filter(|feature| {
                wire_minor >= MANAGE_WIRE_MINOR || !MANAGE_FEATURES.contains(&feature.as_str())
            })
            .cloned()
            .collect();
        // Contract K-W3c §8.2 no. 9: `manage` only with installed ops,
        // `stepup` only together with `manage` and an enabled step-up slice.
        let ops = self.host.ops.get();
        if ops.is_none() {
            granted_features.retain(|feature| feature.as_str() != features::MANAGE);
        }
        let manage = granted_features
            .iter()
            .any(|feature| feature.as_str() == features::MANAGE);
        if !manage || !ops.is_some_and(|ops| ops.stepup_enabled()) {
            granted_features.retain(|feature| feature.as_str() != features::STEPUP);
        }
        let stepup = granted_features
            .iter()
            .any(|feature| feature.as_str() == features::STEPUP);
        self.manage.store(manage, Ordering::SeqCst);
        self.stepup.store(stepup, Ordering::SeqCst);
        self.wire_minor.store(wire_minor, Ordering::SeqCst);
        self.projects.store(
            granted_features
                .iter()
                .any(|feature| feature.as_str() == features::PROJECTS),
            Ordering::SeqCst,
        );
        Ok(HelloAck {
            wire_minor,
            features: granted_features,
            host_epoch: self.host.epoch,
            granted: caps,
        })
    }

    fn list_sync(&self) -> Result<Vec<SessionSummary>, HostError> {
        let granted = self.granted()?;
        if !granted.observe {
            return Err(HostError::Denied("capability `observe` not granted".into()));
        }
        let mut out = self.summaries()?;
        // Minor-2 clients see the newest profile sessions in the flat list,
        // prefixed with their project (contract K-W1 §8.3).
        if !self.has_projects()
            && let Some(share) = self.host.share_for(self.identity.tenant.as_ref())
        {
            let hosted: HashSet<SessionId> = out
                .iter()
                .map(|summary| summary.session_id.clone())
                .collect();
            for row in share.index.newest(share.settings.legacy_rows)? {
                if hosted.contains(&row.session_id) {
                    continue;
                }
                let title = match row.project_label.as_deref() {
                    Some(label) if !label.is_empty() => format!("{label} · {}", row.title),
                    _ => row.title,
                };
                out.push(SessionSummary {
                    session_id: row.session_id,
                    title: Some(title),
                    tenant: Some(share.settings.tenant.clone()),
                    state: HostedState::Idle,
                    attached: 0,
                    updated_at: row.updated_at,
                    model: None,
                });
            }
        }
        Ok(out)
    }

    /// Feature `projects` was negotiated at hello.
    fn has_projects(&self) -> bool {
        self.projects.load(Ordering::SeqCst)
    }

    /// Observe admission that also reaches visible profile sessions without
    /// a record (read-only view, nothing written).
    fn observed_slot(&self, id: &SessionId) -> Result<Arc<Slot>, HostError> {
        let granted = self.granted()?;
        Need::Observe.require(granted)?;
        let slot = self.host.profile_slot(id, self.identity.tenant.as_ref())?;
        let tenant = slot.lock()?.record.tenant.clone();
        admit(&self.identity, granted, tenant.as_ref(), Need::Observe)?;
        Ok(slot)
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
        let slot = self.observed_slot(&params.session_id)?;
        let mut state = slot.lock()?;
        let generation = state.record.generation;
        let head = self.host.transcripts.head(&slot.id)?;
        state.durable_head = head;
        let (queue, stream) = attachment(slot.id.clone(), params.profile, self.host.config.queue);

        // Durable replay, at most half a queue so the stream starts healthy;
        // the rest arrives through `Lagged` + re-attach from the cursor.
        // h19: `params.from` comes from the client's recorded resume cursor
        // (`ResumeCursors`, see harw-session-remote reconnect.rs) — the
        // client never fabricates a cursor. A generation change forces a
        // resync below instead of a stale-position resume.
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
        let projects = self.has_projects();
        if projects && let Some(share) = self.host.share() {
            // Current lease holder, so the client starts with the right badge.
            match peek_holder(share.index.sessions_root(), &slot.id) {
                Ok(Some(holder)) => {
                    let _ = queue.push(FrameEnvelope {
                        session_id: slot.id.clone(),
                        cursor: state.head_cursor(),
                        frame: SessionFrame::Holder {
                            session_id: slot.id.clone(),
                            holder: Some(wire_holder(&holder)),
                        },
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(%error, session = %slot.id.as_str(), "session host: lease peek failed");
                }
            }
        }
        state.attachments.push(AttachEntry {
            connection,
            device: self.identity.device.clone(),
            queue,
            presence,
            projects,
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
        Ok(self.new_record(
            SessionId::new(),
            params.title.as_deref(),
            params.workspace.clone(),
            self.identity.tenant.clone(),
        ))
    }

    /// A fresh record owned by this caller (creator authority from the
    /// authenticated identity, never from the wire).
    fn new_record(
        &self,
        session_id: SessionId,
        title: Option<&str>,
        workspace: Option<String>,
        tenant: Option<TenantId>,
    ) -> HostedSessionRecord {
        let now = jiff::Timestamp::now();
        let title = title
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(|title| title.chars().take(200).collect::<String>());
        HostedSessionRecord {
            session_id,
            tenant,
            owner_principal: self.identity.principal.id().to_owned(),
            creator_execution: Some(crate::record::CreatorExecutionRequest {
                binding: Some(crate::record::CreatorExecutionBinding::from_identity(
                    &self.identity,
                )),
                maximum_tier: self.identity.principal.tier(),
                approval_actor: self.identity.actor.clone(),
            }),
            workspace,
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
        }
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
        if let Some(reason) = refuse_images(&params.images) {
            return Ok((SubmitResult::Denied { reason }, None));
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
        if state.ephemeral {
            // Profile sessions are adopted before a submit reaches here.
            return Err(HostError::NotFound);
        }
        // Plan §3.5: no turn without the session lease. Taken only when no
        // host turn holds it already (running or parked).
        let idle = !state.arbiter.is_running() && !state.parked;
        let leased_now = idle && state.lease.is_none();
        if idle && let Err(error) = self.host.ensure_lease(&params.session_id, &mut state) {
            return match error {
                // A minor-2 app maps BUSY onto a broken transport.
                HostError::Busy(reason) if !self.has_projects() => {
                    Ok((SubmitResult::Denied { reason }, None))
                }
                other => Err(other),
            };
        }
        let head = Cursor {
            generation: state.record.generation,
            durable: state.durable_head,
            live: 0,
        };
        let input = TurnInput {
            session_id: params.session_id.clone(),
            text: params.text,
            images: params.images,
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
        if leased_now && !run && !state.arbiter.is_running() && !state.parked {
            // Nothing will run (stale, queue full, duplicate): give it back.
            self.host
                .release_lease(&params.session_id, &mut state, false);
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
        if state.ephemeral {
            // A TUI session read through the share is not the host's to close.
            return Err(HostError::Denied(
                "profile session is not hosted; nothing to close".into(),
            ));
        }
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
// Profile sharing: projects, pages, adoption (contract K-W1 §8.3)
// ---------------------------------------------------------------------------

/// Page size after clamping (`None | 0` -> default, upper bound).
fn page_limit(limit: Option<u32>) -> usize {
    let limit = limit
        .filter(|limit| *limit > 0)
        .unwrap_or(DEFAULT_PAGE_LIMIT)
        .min(MAX_PAGE_LIMIT);
    usize::try_from(limit).unwrap_or(usize::MAX)
}

/// Why a hosted row cannot be continued (`None`: it can).
fn hosted_reason(granted: ClientCaps, state: &HostedState) -> Option<ReadOnlyReason> {
    if !granted.steer {
        Some(ReadOnlyReason::NoSteer)
    } else if *state == HostedState::Closed {
        Some(ReadOnlyReason::Closed)
    } else {
        None
    }
}

impl HostConnection {
    /// The share for a steering method: mode `continue`, else the matching
    /// refusal (`read`: read-only, `off`/no profile: not shared).
    fn share_for_change(&self) -> Result<&ProfileShare, HostError> {
        match self.host.profile.as_ref() {
            Some(share) if share.settings.mode == ShareMode::Continue => Ok(share),
            Some(share) if share.settings.mode == ShareMode::Read => {
                Err(HostError::Denied(DENIED_READ_ONLY.into()))
            }
            _ => Err(HostError::Denied(DENIED_NOT_SHARED.into())),
        }
    }

    /// The share for a steering method on a profile session: mode, `steer`,
    /// then tenant (a foreign tenant reads as `NotFound`).
    fn steering_share(&self) -> Result<&ProfileShare, HostError> {
        let granted = self.granted()?;
        let share = self.share_for_change()?;
        Need::Steer.require(granted)?;
        if !tenant_admits(self.identity.tenant.as_ref(), Some(&share.settings.tenant)) {
            return Err(HostError::NotFound);
        }
        Ok(share)
    }

    /// True when `turn.submit` on `id` must adopt the profile session first.
    fn needs_adoption(&self, id: &SessionId) -> Result<bool, HostError> {
        if self.host.share().is_none() {
            return Ok(false);
        }
        if let Some(slot) = self.host.loaded_slot(id) {
            return Ok(slot.lock()?.ephemeral);
        }
        Ok(self.host.records.load(id)?.is_none())
    }

    /// Adoption, synchronous part (plan §3.2 step 6): the hosted record of a
    /// continuable profile session, created at most once (`O_EXCL`; a second
    /// device gets the first device's record).
    fn adopt_sync(&self, id: &SessionId) -> Result<HostedSessionRecord, HostError> {
        let granted = self.granted()?;
        let share = self.steering_share()?;
        if let Some(record) = self.host.records.load(id)? {
            admit(&self.identity, granted, record.tenant.as_ref(), Need::Steer)?;
            return Ok(record);
        }
        let row = share.index.row(id)?.ok_or(HostError::NotFound)?;
        if !row.origin.is_continuable() {
            return Err(HostError::Denied(DENIED_FOREIGN_ORIGIN.into()));
        }
        if share.index.workdir(id)?.is_none() {
            return Err(HostError::Denied(DENIED_NO_WORKDIR.into()));
        }
        let record = self.new_record(
            id.clone(),
            Some(&row.title),
            None,
            Some(share.settings.tenant.clone()),
        );
        match self.host.records.create_if_absent(&record)? {
            CreateOutcome::Created => {
                tracing::info!(session = %id.as_str(), "session host: profile session adopted");
                Ok(record)
            }
            CreateOutcome::Existing(existing) => {
                admit(
                    &self.identity,
                    granted,
                    existing.tenant.as_ref(),
                    Need::Steer,
                )?;
                Ok(*existing)
            }
        }
    }

    /// Adoption: record, fresh creator admission, `last_origin`, slot. No
    /// `create_session`: the transcript exists.
    async fn adopt_inner(&self, id: &SessionId) -> Result<Arc<Slot>, HostError> {
        self.steering_share()?;
        if let Some(slot) = self.host.loaded_slot(id) {
            let (ephemeral, tenant) = {
                let state = slot.lock()?;
                (state.ephemeral, state.record.tenant.clone())
            };
            if !ephemeral {
                // Already hosted: adoption is idempotent.
                admit(
                    &self.identity,
                    self.granted()?,
                    tenant.as_ref(),
                    Need::Steer,
                )?;
                return Ok(slot);
            }
        }
        let record = self.adopt_sync(id)?;
        if let Some(admission) = self.host.execution_admission(&record)? {
            self.host.driver.prepare_execution(id, admission).await?;
        }
        if let Some(share) = self.host.share()
            && let Err(error) = meta::update(share.index.sessions_root(), id, |meta| {
                meta.last_origin = Some(LastOrigin::Gateway);
            })
        {
            tracing::warn!(%error, session = %id.as_str(), "session host: adoption meta not updated");
        }
        self.host.publish_record(record)
    }

    fn projects_sync(&self, params: &ProjectsParams) -> Result<ProjectsResult, HostError> {
        let granted = self.granted()?;
        Need::Observe.require(granted)?;
        let device_tier = self.identity.principal.tier();
        let caller = self.identity.tenant.as_ref();
        let Some(share) = self.host.share_for(caller) else {
            let tier = self.host.profile.as_ref().map_or(device_tier, |share| {
                device_tier.min(share.settings.listener_tier)
            });
            return Ok(ProjectsResult {
                projects: Vec::new(),
                next_cursor: None,
                share: ShareInfo {
                    mode: ShareMode::Off,
                    tier,
                },
            });
        };
        let page = share.index.projects(params)?;
        let active = self.host.active_by_project(share, caller);
        let projects = page
            .projects
            .into_iter()
            .map(|mut project| {
                let running = active.get(&project.project_key).copied().unwrap_or(0);
                project.active_count = project.active_count.saturating_add(running);
                project
            })
            .collect();
        Ok(ProjectsResult {
            projects,
            next_cursor: page.next_cursor,
            share: ShareInfo {
                mode: share.settings.mode,
                tier: device_tier.min(share.settings.listener_tier),
            },
        })
    }

    fn page_sync(&self, params: &PageParams) -> Result<PageResult, HostError> {
        let granted = self.granted()?;
        Need::Observe.require(granted)?;
        match self.host.share_for(self.identity.tenant.as_ref()) {
            Some(share) => {
                let page = share.index.page(params)?;
                let sessions = page
                    .rows
                    .into_iter()
                    .map(|row| self.profile_row(share, granted, row))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(PageResult {
                    sessions,
                    next_cursor: page.next_cursor,
                })
            }
            None => self.record_page(granted, params),
        }
    }

    /// One `session.page` row of a profile session (hosted when a record
    /// exists; state, attachments and model then come from the host).
    fn profile_row(
        &self,
        share: &ProfileShare,
        granted: ClientCaps,
        row: ProfileRow,
    ) -> Result<SessionRow, HostError> {
        let id = row.session_id.clone();
        let (state, attached, model, hosted) = match self.host.loaded_slot(&id) {
            Some(slot) => {
                let slot_state = slot.lock()?;
                let attached = slot_state
                    .attachments
                    .iter()
                    .filter(|entry| !entry.queue.is_closed())
                    .count();
                let hosted = !slot_state.ephemeral;
                (
                    slot_state.record.state.clone(),
                    count(attached),
                    hosted.then(|| self.host.driver.model_name(&id)).flatten(),
                    hosted,
                )
            }
            None => match self.host.records.load(&id)? {
                Some(record) => (record.state, 0, self.host.driver.model_name(&id), true),
                None => (HostedState::Idle, 0, None, false),
            },
        };
        let reason = if hosted {
            hosted_reason(granted, &state)
        } else if share.settings.mode != ShareMode::Continue {
            Some(ReadOnlyReason::ShareReadOnly)
        } else if !granted.steer {
            Some(ReadOnlyReason::NoSteer)
        } else if !row.origin.is_continuable() {
            Some(ReadOnlyReason::ForeignOrigin)
        } else if share.index.workdir(&id)?.is_none() {
            Some(ReadOnlyReason::NoWorkdir)
        } else if state == HostedState::Closed {
            Some(ReadOnlyReason::Closed)
        } else {
            None
        };
        let holder = match peek_holder(share.index.sessions_root(), &id) {
            Ok(holder) => holder.as_ref().map(wire_holder),
            Err(error) => {
                tracing::debug!(%error, session = %id.as_str(), "session host: lease peek failed");
                None
            }
        };
        Ok(SessionRow {
            session_id: id,
            title: Some(row.title),
            project_key: row.project_key,
            updated_at: row.updated_at,
            turns: (row.turns > 0).then_some(row.turns),
            state,
            attached,
            model,
            origin: if hosted {
                RowOrigin::Hosted
            } else {
                RowOrigin::Profile
            },
            continuable: reason.is_none(),
            reason,
            holder,
            last_origin: row.last_origin.map(wire_last_origin),
            first_message: row.first_message,
        })
    }

    /// A `session.page` row of a hosted session without profile data.
    fn hosted_row(granted: ClientCaps, summary: SessionSummary) -> SessionRow {
        let reason = hosted_reason(granted, &summary.state);
        SessionRow {
            session_id: summary.session_id,
            title: summary.title,
            project_key: None,
            updated_at: summary.updated_at,
            turns: None,
            state: summary.state,
            attached: summary.attached,
            model: summary.model,
            origin: RowOrigin::Hosted,
            continuable: reason.is_none(),
            reason,
            holder: None,
            last_origin: None,
            first_message: None,
        }
    }

    /// `session.page` without sharing: the caller's records as rows, newest
    /// first, paged with the opaque cursor.
    fn record_page(
        &self,
        granted: ClientCaps,
        params: &PageParams,
    ) -> Result<PageResult, HostError> {
        let after = match params.cursor.as_deref() {
            None => None,
            Some(cursor) => Some(
                decode_cursor(cursor)
                    .ok_or_else(|| HostError::Protocol("invalid cursor".into()))?,
            ),
        };
        if params
            .project_key
            .as_deref()
            .is_some_and(|key| key != NO_PROJECT_KEY)
        {
            // Records carry no project.
            return Ok(PageResult {
                sessions: Vec::new(),
                next_cursor: None,
            });
        }
        let query = params
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(str::to_lowercase);
        let mut summaries: Vec<SessionSummary> = self
            .summaries()?
            .into_iter()
            .filter(|summary| {
                query.as_deref().is_none_or(|query| {
                    summary
                        .title
                        .as_deref()
                        .is_some_and(|title| title.to_lowercase().contains(query))
                })
            })
            .collect();
        let sort_key = |summary: &SessionSummary| {
            (
                summary.updated_at.as_millisecond(),
                summary.session_id.as_str().to_owned(),
            )
        };
        summaries.sort_by_key(|summary| std::cmp::Reverse(sort_key(summary)));
        if let Some((at, key)) = after {
            let bound = (at.as_millisecond(), key);
            summaries.retain(|summary| sort_key(summary) < bound);
        }
        let limit = page_limit(params.limit);
        let next_cursor = (summaries.len() > limit)
            .then(|| summaries.get(limit - 1))
            .flatten()
            .map(|last| encode_cursor(last.updated_at, last.session_id.as_str()));
        summaries.truncate(limit);
        Ok(PageResult {
            sessions: summaries
                .into_iter()
                .map(|summary| Self::hosted_row(granted, summary))
                .collect(),
            next_cursor,
        })
    }

    /// The updated row after a profile mutation.
    fn row_for(&self, share: &ProfileShare, id: &SessionId) -> Result<SessionRow, HostError> {
        let granted = self.granted()?;
        if let Some(row) = share.index.row(id)? {
            return self.profile_row(share, granted, row);
        }
        let slot = self.host.slot(id)?;
        let summary = self.host.summary(&*slot.lock()?);
        Ok(Self::hosted_row(granted, summary))
    }

    /// Steering admission of a profile mutation on `id`: the session is a
    /// visible profile session or a hosted one of the caller's tenant.
    fn profile_target(&self, id: &SessionId) -> Result<&ProfileShare, HostError> {
        let share = self.steering_share()?;
        if share.index.row(id)?.is_some() {
            return Ok(share);
        }
        match self.host.records.load(id)? {
            Some(record)
                if tenant_admits(self.identity.tenant.as_ref(), record.tenant.as_ref()) =>
            {
                Ok(share)
            }
            _ => Err(HostError::NotFound),
        }
    }

    async fn create_in_inner(&self, params: CreateInParams) -> Result<SessionSummary, HostError> {
        let share = self.steering_share()?;
        let project = share
            .index
            .resolve_project(&params.project_key)?
            .ok_or_else(|| HostError::Denied(DENIED_UNKNOWN_PROJECT.into()))?;
        let record = self.new_record(
            SessionId::new(),
            params.title.as_deref(),
            None,
            Some(share.settings.tenant.clone()),
        );
        let mut publication = UnpublishedCreation {
            host: &self.host,
            session_id: record.session_id.clone(),
            armed: true,
        };
        if let Some(admission) = self.host.execution_admission(&record)? {
            self.host
                .driver
                .prepare_execution(&record.session_id, admission)
                .await?;
        }
        // The project goes into the meta before the driver writes anything,
        // so the TUI and the index see the session in its project.
        meta::update(share.index.sessions_root(), &record.session_id, |meta| {
            meta.cwd = Some(project.project_root.clone());
            meta.project_root = Some(project.project_root.clone());
            meta.project_key = Some(project.project_key.clone());
            meta.last_origin = Some(LastOrigin::Gateway);
        })?;
        self.host
            .driver
            .create_session(&record.session_id, record.title.as_deref())
            .await?;
        self.host.records.save(&record)?;
        let slot = self.host.insert_slot(record)?;
        let summary = self.host.summary(&*slot.lock()?);
        publication.armed = false;
        Ok(summary)
    }

    fn set_project_sync(&self, params: &SetProjectParams) -> Result<SessionRow, HostError> {
        let id = &params.session_id;
        let share = self.profile_target(id)?;
        let root = share.index.sessions_root();
        if params.project_key == NO_PROJECT_KEY {
            // "Ohne Projekt": drop the assignment, keep the working directory.
            meta::update(root, id, |meta| {
                meta.project_root = None;
                meta.project_key = None;
            })?;
        } else {
            let project = share
                .index
                .resolve_project(&params.project_key)?
                .ok_or_else(|| HostError::Denied(DENIED_UNKNOWN_PROJECT.into()))?;
            meta::set_project(
                root,
                id,
                Some(project.project_root.as_path()),
                Some(project.project_root.as_path()),
                Some(project.project_key.as_str()),
            )?;
        }
        self.row_for(share, id)
    }

    fn rename_sync(&self, params: &RenameParams) -> Result<SessionRow, HostError> {
        let id = &params.session_id;
        let title = params.title.trim();
        if title.is_empty() {
            return Err(HostError::Protocol("empty title".into()));
        }
        let share = self.profile_target(id)?;
        meta::set_title(share.index.sessions_root(), id, title, TitleSource::Manual)?;
        let slot = match self.host.loaded_slot(id) {
            Some(slot) => Some(slot),
            None if self.host.records.load(id)?.is_some() => Some(self.host.slot(id)?),
            None => None,
        };
        if let Some(slot) = slot {
            let mut state = slot.lock()?;
            state.record.title = Some(title.chars().take(200).collect());
            self.host.save(&mut state);
        }
        self.row_for(share, id)
    }

    fn archive_sync(&self, params: &ArchiveParams) -> Result<SessionRow, HostError> {
        let id = &params.session_id;
        let share = self.profile_target(id)?;
        meta::set_archived(share.index.sessions_root(), id, params.archived)?;
        self.row_for(share, id)
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

// ---------------------------------------------------------------------------
// Minor 4: `manage` / `stepup` admission (contract K-W3c §8.2)
// ---------------------------------------------------------------------------

/// The negotiated feature a minor-4 method needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManageFeature {
    /// `commands.list`, `command.run`, `catalog.get`, `config.get`.
    Manage,
    /// `config.set|reset`, `secret.put`, `stepup.confirm|cancel`.
    Stepup,
}

/// Caps of the step-up confirmed global writes (K-W3b 7.4, R8).
const NEED_CONTROL_APPROVE: &[Need] = &[Need::Control, Need::Approve];
/// Caps of the read-only manage methods and `stepup.cancel`.
const NEED_OBSERVE: &[Need] = &[Need::Observe];

/// Why the attached images of a `turn.submit` are refused, `None` when fine.
fn refuse_images(images: &[harw_protocol::session_wire::SubmitImage]) -> Option<String> {
    use harw_protocol::session_wire::{MAX_SUBMIT_IMAGE_B64_BYTES, MAX_SUBMIT_IMAGES};
    if images.is_empty() {
        return None;
    }
    if images.len() > MAX_SUBMIT_IMAGES {
        return Some(format!("at most {MAX_SUBMIT_IMAGES} images per message"));
    }
    let mut total = 0usize;
    for image in images {
        if !matches!(
            image.mime.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) {
            return Some("unsupported image type".into());
        }
        if image.data_b64.is_empty() {
            return Some("empty image".into());
        }
        total = total.saturating_add(image.data_b64.len());
    }
    (total > MAX_SUBMIT_IMAGE_B64_BYTES).then(|| "images too large".to_owned())
}

/// A refused `command.run` answer with a gateway refusal code.
fn refused_run(code: &str, text: &str) -> CommandRunResult {
    CommandRunResult {
        ok: false,
        text: text.to_owned(),
        data: None,
        effect: CommandEffect::Refused,
        reason_code: Some(code.to_owned()),
        stepup: None,
    }
}

impl HostConnection {
    /// Admission steps 1-5 and 7 of contract K-W3c §8.2 (first failure
    /// wins): hello, negotiated feature, installed ops, profile tenant,
    /// caps. Returns the backend and the host-built caller. Logs only the
    /// method and the connection, never parameters.
    fn manage_admit(
        &self,
        method: &'static str,
        feature: ManageFeature,
        needs: &[Need],
    ) -> Result<(&Arc<dyn OpsPort>, OpsCaller), HostError> {
        let granted = self.granted()?;
        tracing::debug!(
            method,
            connection = self.identity.connection.0,
            "manage call"
        );
        match feature {
            ManageFeature::Manage if !self.manage.load(Ordering::SeqCst) => {
                return Err(HostError::Denied("feature `manage` not negotiated".into()));
            }
            ManageFeature::Stepup if !self.stepup.load(Ordering::SeqCst) => {
                return Err(HostError::Denied("feature `stepup` not negotiated".into()));
            }
            ManageFeature::Manage | ManageFeature::Stepup => {}
        }
        let ops =
            self.host.ops.get().ok_or_else(|| {
                HostError::Denied("management not offered by this gateway".into())
            })?;
        let share = self.host.share();
        if self.identity.tenant.is_some()
            && self.host.share_for(self.identity.tenant.as_ref()).is_none()
        {
            return Err(HostError::Denied(
                "management not offered for this tenant".into(),
            ));
        }
        for need in needs {
            need.require(granted)?;
        }
        Ok((ops, self.ops_caller(granted, share)))
    }

    /// Step 7: the caller the ops backend sees. Effective tier is the
    /// granted tier capped by the listener tier of the profile share.
    fn ops_caller(&self, granted: ClientCaps, share: Option<&ProfileShare>) -> OpsCaller {
        let tier = tier_for_caps(granted);
        OpsCaller {
            identity: self.identity.clone(),
            granted,
            tier: share.map_or(tier, |share| tier.min(share.settings.listener_tier)),
            profile_tenant: share.map(|share| share.settings.tenant.clone()),
        }
    }

    /// Step 6: the referenced session after observe admission. A hosted
    /// session needs its record's tenant to admit the caller; a profile
    /// session must be visible to the caller's tenant (else `NotFound`).
    fn ops_session(&self, id: Option<SessionId>) -> Result<Option<OpsSession>, HostError> {
        let Some(id) = id else {
            return Ok(None);
        };
        // `observed_slot`: a hosted slot (record store) first, otherwise a
        // read-only view of a visible profile session; then tenant + observe.
        let slot = self.observed_slot(&id)?;
        let hosted = !slot.lock()?.ephemeral;
        let model = self.host.driver.model_name(&id);
        Ok(Some(OpsSession {
            session_id: id,
            hosted,
            model,
        }))
    }

    /// Step 8: run the typed session way an `OpsRun::Session` asks for,
    /// with the host's own admission (`steer` for interrupt, `control` for
    /// settings). Success answers `result`; a host error is the answer.
    async fn run_session_action(
        &self,
        session: Option<OpsSession>,
        action: SessionAction,
        result: CommandRunResult,
    ) -> Result<CommandRunResult, HostError> {
        let Some(session) = session else {
            return Ok(refused_run(
                refusal_codes::SESSION_REQUIRED,
                TEXT_SESSION_REQUIRED,
            ));
        };
        if !session.hosted {
            return Ok(refused_run(
                refusal_codes::SESSION_NOT_HOSTED,
                TEXT_SESSION_NOT_HOSTED,
            ));
        }
        let id = session.session_id;
        match action {
            SessionAction::Interrupt => self.interrupt_sync(&InterruptParams {
                session_id: id,
                turn_id: None,
            })?,
            SessionAction::SetModel(model) => self.set(id, Setting::Model(model)).await?,
            SessionAction::SetMode(mode) => self.set(id, Setting::Mode(mode)).await?,
            SessionAction::SetEffort(effort) => self.set(id, Setting::Effort(effort)).await?,
        }
        Ok(result)
    }

    async fn commands_list_inner(
        &self,
        params: CommandsListParams,
    ) -> Result<CommandsListResult, HostError> {
        let (ops, caller) =
            self.manage_admit(METHOD_COMMANDS_LIST, ManageFeature::Manage, NEED_OBSERVE)?;
        let session = self.ops_session(params.session_id.clone())?;
        ops.commands_list(caller, session, params).await
    }

    async fn command_run_inner(
        &self,
        params: CommandRunParams,
    ) -> Result<CommandRunResult, HostError> {
        let (ops, caller) =
            self.manage_admit(METHOD_COMMAND_RUN, ManageFeature::Manage, NEED_OBSERVE)?;
        let session = self.ops_session(params.session_id.clone())?;
        match ops.command_run(caller, session.clone(), params).await? {
            OpsRun::Done(result) => Ok(result),
            OpsRun::Session { action, result } => {
                self.run_session_action(session, action, result).await
            }
        }
    }

    async fn catalog_get_inner(
        &self,
        params: CatalogGetParams,
    ) -> Result<CatalogResult, HostError> {
        let (ops, caller) =
            self.manage_admit(METHOD_CATALOG_GET, ManageFeature::Manage, NEED_OBSERVE)?;
        let session = self.ops_session(params.session_id.clone())?;
        ops.catalog_get(caller, session, params).await
    }

    async fn config_get_inner(
        &self,
        params: ConfigGetParams,
    ) -> Result<ConfigGetResult, HostError> {
        let (ops, caller) =
            self.manage_admit(METHOD_CONFIG_GET, ManageFeature::Manage, NEED_OBSERVE)?;
        ops.config_get(caller, params).await
    }

    async fn config_set_inner(
        &self,
        params: ConfigSetParams,
    ) -> Result<CommandRunResult, HostError> {
        let (ops, caller) = self.manage_admit(
            METHOD_CONFIG_SET,
            ManageFeature::Stepup,
            NEED_CONTROL_APPROVE,
        )?;
        ops.config_set(caller, params).await
    }

    async fn config_reset_inner(
        &self,
        params: ConfigResetParams,
    ) -> Result<CommandRunResult, HostError> {
        let (ops, caller) = self.manage_admit(
            METHOD_CONFIG_RESET,
            ManageFeature::Stepup,
            NEED_CONTROL_APPROVE,
        )?;
        ops.config_reset(caller, params).await
    }

    async fn secret_put_inner(
        &self,
        params: SecretPutParams,
    ) -> Result<SecretPutResult, HostError> {
        let (ops, caller) = self.manage_admit(
            METHOD_SECRET_PUT,
            ManageFeature::Stepup,
            NEED_CONTROL_APPROVE,
        )?;
        ops.secret_put(caller, params).await
    }

    async fn stepup_confirm_inner(
        &self,
        params: StepUpConfirmParams,
    ) -> Result<CommandRunResult, HostError> {
        let (ops, caller) = self.manage_admit(
            METHOD_STEPUP_CONFIRM,
            ManageFeature::Stepup,
            NEED_CONTROL_APPROVE,
        )?;
        ops.stepup_confirm(caller, params).await
    }

    async fn stepup_cancel_inner(&self, params: StepUpCancelParams) -> Result<(), HostError> {
        // Observe only: the registry cancels the caller's own step-ups only.
        let (ops, caller) =
            self.manage_admit(METHOD_STEPUP_CANCEL, ManageFeature::Stepup, NEED_OBSERVE)?;
        ops.stepup_cancel(caller, params).await
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
                let mut publication = UnpublishedCreation {
                    host: &self.host,
                    session_id: record.session_id.clone(),
                    armed: true,
                };
                if let Some(admission) = self.host.execution_admission(&record)? {
                    self.host
                        .driver
                        .prepare_execution(&record.session_id, admission)
                        .await?;
                }
                self.host
                    .driver
                    .create_session(&record.session_id, record.title.as_deref())
                    .await?;
                self.host.records.save(&record)?;
                let slot = self.host.insert_slot(record)?;
                let summary = self.host.summary(&*slot.lock()?);
                publication.armed = false;
                Ok(summary)
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
            let slot = self.observed_slot(&params.session_id)?;
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
        Box::pin(
            async move {
                // A profile session without a record is adopted first (same
                // code as `session.adopt`).
                if self.needs_adoption(&params.session_id)? {
                    self.adopt_inner(&params.session_id).await?;
                }
                let (result, run) = self.submit_sync(params)?;
                if let Some(slot) = run {
                    pump(Arc::clone(&self.host), slot);
                }
                Ok(result)
            }
            .map_err_port(),
        )
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

    fn projects(&self, params: ProjectsParams) -> PortFuture<'_, ProjectsResult> {
        port(self.projects_sync(&params))
    }

    fn page(&self, params: PageParams) -> PortFuture<'_, PageResult> {
        port(self.page_sync(&params))
    }

    fn adopt(&self, session: SessionId) -> PortFuture<'_, SessionSummary> {
        Box::pin(
            async move {
                let slot = self.adopt_inner(&session).await?;
                let summary = self.host.summary(&*slot.lock()?);
                Ok(summary)
            }
            .map_err_port(),
        )
    }

    fn create_in(&self, params: CreateInParams) -> PortFuture<'_, SessionSummary> {
        Box::pin(self.create_in_inner(params).map_err_port())
    }

    fn set_project(&self, params: SetProjectParams) -> PortFuture<'_, SessionRow> {
        port(self.set_project_sync(&params))
    }

    fn rename(&self, params: RenameParams) -> PortFuture<'_, SessionRow> {
        port(self.rename_sync(&params))
    }

    fn archive(&self, params: ArchiveParams) -> PortFuture<'_, SessionRow> {
        port(self.archive_sync(&params))
    }

    fn commands_list(&self, params: CommandsListParams) -> PortFuture<'_, CommandsListResult> {
        Box::pin(self.commands_list_inner(params).map_err_port())
    }

    fn command_run(&self, params: CommandRunParams) -> PortFuture<'_, CommandRunResult> {
        Box::pin(self.command_run_inner(params).map_err_port())
    }

    fn catalog_get(&self, params: CatalogGetParams) -> PortFuture<'_, CatalogResult> {
        Box::pin(self.catalog_get_inner(params).map_err_port())
    }

    fn config_get(&self, params: ConfigGetParams) -> PortFuture<'_, ConfigGetResult> {
        Box::pin(self.config_get_inner(params).map_err_port())
    }

    fn config_set(&self, params: ConfigSetParams) -> PortFuture<'_, CommandRunResult> {
        Box::pin(self.config_set_inner(params).map_err_port())
    }

    fn config_reset(&self, params: ConfigResetParams) -> PortFuture<'_, CommandRunResult> {
        Box::pin(self.config_reset_inner(params).map_err_port())
    }

    fn secret_put(&self, params: SecretPutParams) -> PortFuture<'_, SecretPutResult> {
        Box::pin(self.secret_put_inner(params).map_err_port())
    }

    fn stepup_confirm(&self, params: StepUpConfirmParams) -> PortFuture<'_, CommandRunResult> {
        Box::pin(self.stepup_confirm_inner(params).map_err_port())
    }

    fn stepup_cancel(&self, params: StepUpCancelParams) -> PortFuture<'_, ()> {
        Box::pin(self.stepup_cancel_inner(params).map_err_port())
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

#[cfg(test)]
mod ops_tests;

/// Acceptance S3 (contract K-W1 §8.3): sharing profile sessions. Synthetic
/// ids and temp dirs only.
#[cfg(test)]
mod share_tests {
    use std::collections::HashMap;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use harw_protocol::session_wire::{
        AttachParams, CreateInParams, HelloParams, HistoryParams, HolderKind as WireHolderKind,
        PageParams, ProjectsParams, ReadOnlyReason, SubmitParams, SubmitResult, features,
    };
    use harw_protocol::{Cursor, FrameSource, PortError, SessionFrame, SessionPort, StreamProfile};
    use harw_types::{PermissionTier, SessionId, TenantId};

    use super::{HostConfig, HostConnection, SessionHost};
    use crate::approvals::MemoryApprovals;
    use crate::driver::{
        CancelSignal, DriverFuture, EventSink, ExecutionAdmission, Setting, TurnDriver, TurnInput,
        TurnOutcome,
    };
    use crate::error::HostError;
    use crate::identity::ClientIdentity;
    use crate::identity::test_identity::scoped;
    use crate::index::{
        DENIED_READ_ONLY, ProfileIndex, ProfilePage, ProfileRow, ProfileShare, ProjectPage,
        ResolvedProject, ShareMode, ShareSettings, ThreadOrigin,
    };
    use crate::record::CreateOutcome;
    use crate::replay::MemoryTranscripts;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    /// Minimal runtime: admission and creation succeed, turns complete.
    struct QuietDriver;

    impl TurnDriver for QuietDriver {
        fn prepare_execution(&self, _: &SessionId, _: ExecutionAdmission) -> DriverFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn create_session(&self, _: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn run_turn(
            &self,
            _: TurnInput,
            _: CancelSignal,
            _: Arc<dyn EventSink>,
        ) -> DriverFuture<'_, TurnOutcome> {
            Box::pin(async { Ok(TurnOutcome::Completed) })
        }

        fn resume_after_approval(
            &self,
            _: &SessionId,
            _: CancelSignal,
            _: Arc<dyn EventSink>,
        ) -> DriverFuture<'_, TurnOutcome> {
            Box::pin(async { Ok(TurnOutcome::Completed) })
        }

        fn apply_setting(&self, _: &SessionId, _: Setting) -> DriverFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn model_name(&self, _: &SessionId) -> Option<String> {
            None
        }
    }

    /// Scripted profile index over a temp profile store.
    struct FakeIndex {
        root: PathBuf,
        rows: Mutex<Vec<ProfileRow>>,
        workdirs: Mutex<HashMap<SessionId, PathBuf>>,
        projects: HashMap<String, ResolvedProject>,
    }

    impl ProfileIndex for FakeIndex {
        fn sessions_root(&self) -> &Path {
            &self.root
        }

        fn projects(&self, _: &ProjectsParams) -> Result<ProjectPage, HostError> {
            Ok(ProjectPage {
                projects: Vec::new(),
                next_cursor: None,
            })
        }

        fn page(&self, _: &PageParams) -> Result<ProfilePage, HostError> {
            Ok(ProfilePage {
                rows: self.newest(usize::MAX)?,
                next_cursor: None,
            })
        }

        fn row(&self, id: &SessionId) -> Result<Option<ProfileRow>, HostError> {
            Ok(self
                .rows
                .lock()
                .map_err(|_| HostError::Storage("rows poisoned".into()))?
                .iter()
                .find(|row| &row.session_id == id)
                .cloned())
        }

        fn newest(&self, limit: usize) -> Result<Vec<ProfileRow>, HostError> {
            let mut rows: Vec<ProfileRow> = self
                .rows
                .lock()
                .map_err(|_| HostError::Storage("rows poisoned".into()))?
                .iter()
                .filter(|row| !row.archived)
                .cloned()
                .collect();
            rows.sort_by_key(|row| std::cmp::Reverse(row.updated_at));
            rows.truncate(limit);
            Ok(rows)
        }

        fn resolve_project(&self, key: &str) -> Result<Option<ResolvedProject>, HostError> {
            Ok(self.projects.get(key).cloned())
        }

        fn workdir(&self, id: &SessionId) -> Result<Option<PathBuf>, HostError> {
            Ok(self
                .workdirs
                .lock()
                .map_err(|_| HostError::Storage("workdirs poisoned".into()))?
                .get(id)
                .cloned())
        }
    }

    struct Share {
        state: tempfile::TempDir,
        profile: tempfile::TempDir,
        host: SessionHost,
        index: Arc<FakeIndex>,
        tenant: TenantId,
    }

    fn tenant() -> TestResult<TenantId> {
        Ok(TenantId::try_from_str("tenant-test")?)
    }

    fn share_fixture(mode: ShareMode, legacy_rows: usize) -> TestResult<Share> {
        let state = tempfile::tempdir()?;
        let profile = tempfile::tempdir()?;
        let tenant = tenant()?;
        let index = Arc::new(FakeIndex {
            root: profile.path().to_path_buf(),
            rows: Mutex::new(Vec::new()),
            workdirs: Mutex::new(HashMap::new()),
            projects: HashMap::new(),
        });
        let host = SessionHost::open_with_profile(
            HostConfig::new(state.path().to_path_buf()),
            Arc::new(QuietDriver) as Arc<dyn TurnDriver>,
            Arc::new(MemoryTranscripts::new()),
            Arc::new(MemoryApprovals::new()),
            crate::tool_host::ToolHost::disabled(),
            ProfileShare {
                settings: ShareSettings {
                    mode,
                    tenant: tenant.clone(),
                    legacy_rows,
                    listener_tier: PermissionTier::Operator,
                },
                index: Arc::clone(&index) as Arc<dyn ProfileIndex>,
            },
        )?;
        Ok(Share {
            state,
            profile,
            host,
            index,
            tenant,
        })
    }

    impl Share {
        /// A visible profile session; continuable when `origin` is Cli.
        fn add_row(
            &self,
            origin: ThreadOrigin,
            project: Option<&str>,
            title: &str,
        ) -> TestResult<SessionId> {
            let id = SessionId::new();
            let count = self.index.rows.lock().map_err(|_| "rows poisoned")?.len();
            let millis = 1_764_000_000_000_i64 + i64::try_from(count)? * 1_000;
            self.index
                .rows
                .lock()
                .map_err(|_| "rows poisoned")?
                .push(ProfileRow {
                    session_id: id.clone(),
                    title: title.to_owned(),
                    project_key: project.map(|label| format!("{label}-0123456789ab")),
                    project_label: project.map(str::to_owned),
                    updated_at: jiff::Timestamp::from_millisecond(millis)?,
                    turns: 1,
                    first_message: Some("hallo".into()),
                    origin,
                    last_origin: None,
                    archived: false,
                });
            self.index
                .workdirs
                .lock()
                .map_err(|_| "workdirs poisoned")?
                .insert(id.clone(), self.profile.path().to_path_buf());
            Ok(id)
        }
    }

    async fn connect(
        host: &SessionHost,
        identity: ClientIdentity,
        wire_minor: u32,
        with_projects: bool,
    ) -> TestResult<HostConnection> {
        let connection = host.connect(identity)?;
        let features = if with_projects {
            vec![features::PROJECTS.to_owned()]
        } else {
            Vec::new()
        };
        connection
            .hello(HelloParams {
                client_label: "test".into(),
                wire_minor,
                features,
                requested_caps: None,
            })
            .await?;
        Ok(connection)
    }

    fn attach(session: &SessionId) -> AttachParams {
        AttachParams {
            session_id: session.clone(),
            from: None,
            profile: StreamProfile::Full,
            tail_items: 200,
        }
    }

    fn submit(session: &SessionId) -> SubmitParams {
        SubmitParams {
            session_id: session.clone(),
            text: "weiter".into(),
            expect_head: Cursor {
                generation: 0,
                durable: 0,
                live: 0,
            },
            client_msg_id: "msg-1".into(),
            force: true,
            images: Vec::new(),
        }
    }

    /// Simulates a TUI process holding the lease: its own fd, locked, with
    /// holder JSON (flock is per open file description).
    fn hold_lease_as_tui(root: &Path, id: &SessionId) -> TestResult<std::fs::File> {
        let path = harw_session_store::lease_path(root, id)?;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        fs4::FileExt::try_lock(&file).map_err(|_| "lease already locked")?;
        file.set_len(0)?;
        file.write_all(br#"{"holder":"tui","pid":4242,"since":"2026-11-22T10:00:00Z"}"#)?;
        file.sync_data()?;
        Ok(file)
    }

    /// Every file below `dir`, relative, sorted.
    fn files_below(dir: &Path) -> TestResult<Vec<PathBuf>> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(next) = stack.pop() {
            for entry in std::fs::read_dir(&next)? {
                let entry = entry?;
                let path = entry.path();
                if entry.file_type()?.is_dir() {
                    stack.push(path);
                } else {
                    out.push(path.strip_prefix(dir)?.to_path_buf());
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Frames already queued, without waiting for more.
    async fn drain(source: &mut Box<dyn FrameSource>) -> TestResult<Vec<SessionFrame>> {
        let mut frames = Vec::new();
        while let Ok(next) = tokio::time::timeout(Duration::from_millis(50), source.next()).await {
            match next? {
                Some(envelope) => frames.push(envelope.frame),
                None => break,
            }
        }
        Ok(frames)
    }

    #[tokio::test]
    async fn foreign_tenant_sees_no_profile_rows() -> TestResult {
        let fx = share_fixture(ShareMode::Continue, 100)?;
        let id = fx.add_row(ThreadOrigin::Cli, Some("proj"), "titel")?;
        let foreign = TenantId::try_from_str("tenant-other")?;
        let legacy = connect(
            &fx.host,
            scoped(1, PermissionTier::Operator, &foreign),
            2,
            false,
        )
        .await?;
        assert!(legacy.list().await?.is_empty());
        let modern = connect(
            &fx.host,
            scoped(2, PermissionTier::Operator, &foreign),
            3,
            true,
        )
        .await?;
        let projects = modern.projects(ProjectsParams::default()).await?;
        assert!(projects.projects.is_empty());
        assert_eq!(projects.share.mode, ShareMode::Off);
        assert!(
            modern
                .page(PageParams::default())
                .await?
                .sessions
                .is_empty()
        );
        assert!(matches!(
            modern.attach(attach(&id)).await,
            Err(PortError::NotFound)
        ));
        assert!(matches!(
            modern.adopt(id.clone()).await,
            Err(PortError::NotFound)
        ));

        // Control: the share tenant does see the row.
        let own = connect(
            &fx.host,
            scoped(3, PermissionTier::Operator, &fx.tenant),
            3,
            true,
        )
        .await?;
        let page = own.page(PageParams::default()).await?;
        assert_eq!(page.sessions.len(), 1);
        assert!(
            page.sessions[0].continuable,
            "{:?}",
            page.sessions[0].reason
        );
        Ok(())
    }

    #[tokio::test]
    async fn observer_attach_writes_no_record() -> TestResult {
        let fx = share_fixture(ShareMode::Continue, 100)?;
        let id = fx.add_row(ThreadOrigin::Cli, Some("proj"), "titel")?;
        let observer = connect(
            &fx.host,
            scoped(1, PermissionTier::Observer, &fx.tenant),
            3,
            true,
        )
        .await?;
        let state_before = files_below(fx.state.path())?;
        let profile_before = files_below(fx.profile.path())?;

        let (ack, _stream) = observer.attach(attach(&id)).await?;
        assert_eq!(ack.session.session_id, id);
        observer
            .history(HistoryParams {
                session_id: id.clone(),
                before: ack.head,
                limit: 10,
            })
            .await?;
        observer.detach(id.clone()).await?;

        assert_eq!(files_below(fx.state.path())?, state_before);
        assert_eq!(files_below(fx.profile.path())?, profile_before);
        assert!(fx.host.inner.records.load(&id)?.is_none());
        // An observer cannot adopt.
        assert!(matches!(
            observer.adopt(id).await,
            Err(PortError::Denied(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn adopted_record_has_caller_creator_execution() -> TestResult {
        let fx = share_fixture(ShareMode::Continue, 100)?;
        let id = fx.add_row(ThreadOrigin::Cli, Some("proj"), "titel")?;
        let identity = scoped(1, PermissionTier::Operator, &fx.tenant);
        let caller = connect(&fx.host, identity.clone(), 3, true).await?;
        let summary = caller.adopt(id.clone()).await?;
        assert_eq!(summary.session_id, id);
        assert_eq!(summary.tenant.as_ref(), Some(&fx.tenant));

        let record = fx.host.inner.records.load(&id)?.ok_or("record missing")?;
        assert_eq!(record.owner_principal, identity.principal.id());
        assert_eq!(record.title.as_deref(), Some("titel"));
        assert_eq!(record.workspace, None);
        let creator = record.creator_execution.ok_or("creator missing")?;
        assert_eq!(
            creator.binding,
            Some(crate::record::CreatorExecutionBinding::from_identity(
                &identity
            ))
        );
        assert_eq!(creator.approval_actor, identity.actor);
        assert_eq!(creator.maximum_tier, identity.principal.tier());
        Ok(())
    }

    #[tokio::test]
    async fn two_devices_adopt_one_record() -> TestResult {
        let fx = share_fixture(ShareMode::Continue, 100)?;
        let id = fx.add_row(ThreadOrigin::Cli, Some("proj"), "titel")?;
        let first_identity = scoped(1, PermissionTier::Operator, &fx.tenant);
        let first = connect(&fx.host, first_identity.clone(), 3, true).await?;
        let second = connect(
            &fx.host,
            scoped(2, PermissionTier::Operator, &fx.tenant),
            3,
            true,
        )
        .await?;

        let a = first.adopt(id.clone()).await?;
        let b = second.adopt(id.clone()).await?;
        assert_eq!(a.session_id, b.session_id);
        let records: Vec<_> = fx
            .host
            .inner
            .records
            .list()?
            .into_iter()
            .filter(|record| record.session_id == id)
            .collect();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].owner_principal, first_identity.principal.id());

        // The losing side of a race gets the winner's record unchanged.
        let late = second.new_record(id.clone(), Some("anders"), None, Some(fx.tenant.clone()));
        match fx.host.inner.records.create_if_absent(&late)? {
            CreateOutcome::Existing(existing) => assert_eq!(*existing, records[0]),
            CreateOutcome::Created => return Err("second create must not win".into()),
        }
        Ok(())
    }

    #[tokio::test]
    async fn turn_with_held_lease_is_busy_with_reason() -> TestResult {
        let fx = share_fixture(ShareMode::Continue, 100)?;
        let id = fx.add_row(ThreadOrigin::Cli, Some("proj"), "titel")?;
        let caller = connect(
            &fx.host,
            scoped(1, PermissionTier::Operator, &fx.tenant),
            3,
            true,
        )
        .await?;
        let _tui = hold_lease_as_tui(fx.profile.path(), &id)?;
        match caller.submit(submit(&id)).await {
            Err(PortError::Busy(reason)) => assert_eq!(reason, "Läuft gerade in der TUI"),
            other => return Err(format!("expected busy, got {other:?}").into()),
        }
        Ok(())
    }

    #[tokio::test]
    async fn old_client_gets_denied_instead_of_busy() -> TestResult {
        let fx = share_fixture(ShareMode::Continue, 100)?;
        let id = fx.add_row(ThreadOrigin::Cli, Some("proj"), "titel")?;
        let caller = connect(
            &fx.host,
            scoped(1, PermissionTier::Operator, &fx.tenant),
            2,
            false,
        )
        .await?;
        let _tui = hold_lease_as_tui(fx.profile.path(), &id)?;
        match caller.submit(submit(&id)).await? {
            SubmitResult::Denied { reason } => assert_eq!(reason, "Läuft gerade in der TUI"),
            other => return Err(format!("expected denied, got {other:?}").into()),
        }
        Ok(())
    }

    #[tokio::test]
    async fn read_mode_submit_is_denied() -> TestResult {
        let fx = share_fixture(ShareMode::Read, 100)?;
        let id = fx.add_row(ThreadOrigin::Cli, Some("proj"), "titel")?;
        let caller = connect(
            &fx.host,
            scoped(1, PermissionTier::Operator, &fx.tenant),
            3,
            true,
        )
        .await?;
        match caller.submit(submit(&id)).await {
            Err(PortError::Denied(reason)) => assert_eq!(reason, DENIED_READ_ONLY),
            other => return Err(format!("expected denied, got {other:?}").into()),
        }
        assert!(fx.host.inner.records.load(&id)?.is_none());
        // Reading still works, and the row says why it is read-only.
        let page = caller.page(PageParams::default()).await?;
        assert_eq!(page.sessions[0].reason, Some(ReadOnlyReason::ShareReadOnly));
        caller.attach(attach(&id)).await?;
        Ok(())
    }

    #[tokio::test]
    async fn legacy_list_prefixes_project_and_respects_limit() -> TestResult {
        let fx = share_fixture(ShareMode::Read, 2)?;
        fx.add_row(ThreadOrigin::Cli, Some("proj"), "titel eins")?;
        fx.add_row(ThreadOrigin::Cli, Some("proj"), "titel zwei")?;
        fx.add_row(ThreadOrigin::Cli, None, "titel drei")?;
        let legacy = connect(
            &fx.host,
            scoped(1, PermissionTier::Operator, &fx.tenant),
            2,
            false,
        )
        .await?;
        let list = legacy.list().await?;
        let titles: Vec<_> = list.iter().filter_map(|s| s.title.as_deref()).collect();
        assert_eq!(titles, vec!["titel drei", "proj · titel zwei"]);
        assert!(list.iter().all(|s| s.tenant.as_ref() == Some(&fx.tenant)));

        // A `projects` client pages instead and gets no legacy rows.
        let modern = connect(
            &fx.host,
            scoped(2, PermissionTier::Operator, &fx.tenant),
            3,
            true,
        )
        .await?;
        assert!(modern.list().await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn projects_feature_only_from_minor_3() -> TestResult {
        let fx = share_fixture(ShareMode::Continue, 100)?;
        for (minor, granted) in [(2, false), (3, true)] {
            let connection =
                fx.host
                    .connect(scoped(minor, PermissionTier::Operator, &fx.tenant))?;
            let ack = connection
                .hello(HelloParams {
                    client_label: "test".into(),
                    wire_minor: minor,
                    features: vec![features::PROJECTS.to_owned()],
                    requested_caps: None,
                })
                .await?;
            assert_eq!(
                ack.features.iter().any(|f| f == features::PROJECTS),
                granted,
                "minor {minor}"
            );
            assert_eq!(connection.has_projects(), granted);
        }
        Ok(())
    }

    #[tokio::test]
    async fn holder_frame_only_to_projects_connections() -> TestResult {
        let fx = share_fixture(ShareMode::Continue, 100)?;
        let id = fx.add_row(ThreadOrigin::Cli, Some("proj"), "titel")?;
        let modern = connect(
            &fx.host,
            scoped(1, PermissionTier::Operator, &fx.tenant),
            3,
            true,
        )
        .await?;
        let legacy = connect(
            &fx.host,
            scoped(2, PermissionTier::Operator, &fx.tenant),
            2,
            false,
        )
        .await?;
        let (_, mut modern_stream) = modern.attach(attach(&id)).await?;
        let (_, mut legacy_stream) = legacy.attach(attach(&id)).await?;
        drain(&mut modern_stream).await?;
        drain(&mut legacy_stream).await?;

        let tui = hold_lease_as_tui(fx.profile.path(), &id)?;
        fx.host.poll_profile_heads();
        let seen = drain(&mut modern_stream).await?;
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                SessionFrame::Holder { holder: Some(info), .. } if info.kind == WireHolderKind::Tui
            )),
            "{seen:?}"
        );
        let legacy_seen = drain(&mut legacy_stream).await?;
        assert!(
            !legacy_seen
                .iter()
                .any(|frame| matches!(frame, SessionFrame::Holder { .. })),
            "{legacy_seen:?}"
        );

        // Unchanged holder: no repeat. Released: `holder: None`.
        fx.host.poll_profile_heads();
        assert!(drain(&mut modern_stream).await?.is_empty());
        drop(tui);
        fx.host.poll_profile_heads();
        let freed = drain(&mut modern_stream).await?;
        assert!(
            freed
                .iter()
                .any(|frame| matches!(frame, SessionFrame::Holder { holder: None, .. })),
            "{freed:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn web_origin_row_is_not_continuable() -> TestResult {
        let fx = share_fixture(ShareMode::Continue, 100)?;
        let id = fx.add_row(ThreadOrigin::Web, None, "aus dem web")?;
        let caller = connect(
            &fx.host,
            scoped(1, PermissionTier::Operator, &fx.tenant),
            3,
            true,
        )
        .await?;
        let page = caller.page(PageParams::default()).await?;
        let row = page.sessions.first().ok_or("row missing")?;
        assert!(!row.continuable);
        assert_eq!(row.reason, Some(ReadOnlyReason::ForeignOrigin));
        assert!(matches!(
            caller.adopt(id.clone()).await,
            Err(PortError::Denied(_))
        ));
        assert!(fx.host.inner.records.load(&id)?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn create_in_unknown_key_is_denied() -> TestResult {
        let fx = share_fixture(ShareMode::Continue, 100)?;
        let caller = connect(
            &fx.host,
            scoped(1, PermissionTier::Operator, &fx.tenant),
            3,
            true,
        )
        .await?;
        let before = files_below(fx.state.path())?;
        match caller
            .create_in(CreateInParams {
                project_key: "nirgends-0123456789ab".into(),
                title: None,
            })
            .await
        {
            Err(PortError::Denied(reason)) => assert_eq!(reason, super::DENIED_UNKNOWN_PROJECT),
            other => return Err(format!("expected denied, got {other:?}").into()),
        }
        assert_eq!(files_below(fx.state.path())?, before);
        assert!(files_below(fx.profile.path())?.is_empty());
        Ok(())
    }
}
