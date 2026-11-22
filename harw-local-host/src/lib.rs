//! In-process harw session host, shared by the gateway and the Android app.
//!
//! # Description
//! The composition that turns a model provider and a state directory into a
//! running [`SessionHost`] with the production core driver: one
//! `RuntimeAssembly` per hosted session ([`LocalCoreFactory`]), durable
//! transcripts and approvals ([`open_durable_stores`]) and the local owner's
//! identity ([`LocalOwner`]). No transport lives here: the gateway puts its
//! Unix socket / node listener in front of the host, the phone talks to the
//! [`HostConnection`] in-process.
//!
//! ```text
//! harw gateway --session-socket          Android app (local host)
//!   `-- GatewayCoreFactory (newtype)       `-- LocalHost::open(LocalHostConfig, model)
//!          \                                     /
//!           `------> LocalCoreFactory <---------'
//!                     |-- RuntimeAssembly per session (EntryKind::SessionHost)
//!                     |-- TranscriptStateStore  <state>/sessions
//!                     `-- approval actor bound in the spawn context
//!   CoreTurnDriver::with_factory(.., LocalCoreFactory)
//!   SessionHost::open(HostConfig(<state>/host), driver, DurableTranscripts, DurableApprovals)
//! ```
//!
//! # Jobs rule
//! [`CoreSessionFactory::build`] is synchronous; `CoreTurnDriver` runs it on a
//! blocking worker. [`LocalHost::open`] is synchronous too, needs no tokio
//! context and spawns no task; async callers wrap it in `spawn_blocking`.
//!
//! # Registry profile
//! The registry profile is fixed to `RegistryProfile::Full`:
//! `EntryKind::SessionHost` exposes the operation model tools and the
//! assembly (`harw-runtime/src/assembly.rs:1065`) rejects any narrower
//! profile for it. Rights are cut through the permission set instead.
//!
//! # Phone defaults
//! [`HostRights::operator`] is the gateway default: `Operator` rights are
//! read + write in the workspace, no `ExecuteProcess`, no `NetworkAccess`.
//!
//! # UIA note
//! `EntryKind::SessionHost` does not apply `harness.active_uia_definition`
//! today: the assembly gates the UIA on `Tui | OneShot`
//! (`harw-runtime/src/assembly.rs:2151` and `:4734`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_authority::FileWriteScope;
use harw_core::{AgentSession, ModelProvider, StateStore, TranscriptStateStore};
use harw_registry_defaults::{IdentityOverrides, RegistryProfile};
use harw_runtime::{
    EntryKind, ModelSource, RuntimeAssembly, RuntimeNarrowing, RuntimeSpec, RuntimeStores,
    permissions_for_tier,
};
use harw_session_driver::{CoreDriverConfig, CoreTurnDriver, DriverBridgeError};
use harw_session_host::driver::ExecutionAdmission;
use harw_session_host::record::RecordStore;
use harw_session_host::{
    ClientIdentity, ConnectionId, DurableApprovals, DurableTranscripts, HostConfig, caps_for_tier,
};
use harw_session_store::TranscriptStore;
use harw_types::{
    ApprovalActor, AuthStrength, IngressSurface, PermissionTier, Principal, PrincipalKind,
    SessionId, ThreadRef, TrustZone,
};

pub use harw_session_driver::{CoreSession, CoreSessionFactory, SessionWiring};
pub use harw_session_host::{HostConnection, SessionHost};

/// Transcripts and approvals of hosted sessions below the state directory.
const SESSIONS_DIR_NAME: &str = "sessions";
/// Host records below the state directory.
const HOST_DIR_NAME: &str = "host";

/// The local owner the hosted sessions belong to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalOwner {
    /// Principal and approval actor id (`uid:<n>`, `phone:owner`, ...).
    pub id: String,
    /// Tier of the owner's connection.
    pub tier: PermissionTier,
}

impl LocalOwner {
    /// Owner `id` at `tier`.
    #[must_use]
    pub fn new(id: impl Into<String>, tier: PermissionTier) -> Self {
        Self {
            id: id.into(),
            tier,
        }
    }

    /// The local user `uid` (`uid:<uid>`) at `tier`.
    #[must_use]
    pub fn for_uid(uid: u32, tier: PermissionTier) -> Self {
        Self::new(format!("uid:{uid}"), tier)
    }

    /// The owner as a trusted human principal arriving through `surface`.
    #[must_use]
    pub fn principal(&self, surface: IngressSurface) -> Principal {
        Principal::trusted_ingress(PrincipalKind::Human, self.id.clone(), surface, self.tier)
    }

    /// The approval actor the owner's connection carries.
    #[must_use]
    pub fn actor(&self) -> ApprovalActor {
        ApprovalActor::Operator {
            id: self.id.clone(),
        }
    }

    /// The owner's host identity: the tier's session ceiling only (no
    /// gateway caps, never `tool_call`), local zone, peer-credential strength.
    #[must_use]
    pub fn identity(&self) -> ClientIdentity {
        ClientIdentity {
            principal: self.principal(IngressSurface::Tui),
            tenant: None,
            caps: caps_for_tier(self.tier),
            device: None,
            authenticated_node: None,
            authenticated_key_fingerprint: None,
            actor: self.actor(),
            label: format!("local-{}", self.id),
            zone: TrustZone::Local,
            strength: AuthStrength::PeerCredential,
            connection: ConnectionId::next(),
            agent: None,
        }
    }
}

/// Rights ceiling for hosted sessions. The registry profile is always
/// `RegistryProfile::Full`: `EntryKind::SessionHost` exposes operation model
/// tools, and `harw-runtime/src/assembly.rs:1065` rejects any narrower profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostRights {
    /// Tier the sessions run at; rights are
    /// `permissions_for_tier(min(tier, connection maximum tier))`.
    pub tier: PermissionTier,
    /// Workspace-relative write ceiling.
    pub write_scope: FileWriteScope,
    /// Binds the root sandbox to this directory (only ever narrower).
    pub workspace_root: Option<PathBuf>,
}

impl HostRights {
    /// The gateway default: operator, workspace writes, detected root.
    #[must_use]
    pub fn operator() -> Self {
        Self {
            tier: PermissionTier::Operator,
            write_scope: FileWriteScope::workspace(),
            workspace_root: None,
        }
    }
}

impl Default for HostRights {
    fn default() -> Self {
        Self::operator()
    }
}

/// A [`RuntimeSpec`] without overrides (no embedded agent, no child backend,
/// no mode, agent, effort, approval or model override).
#[must_use]
pub fn runtime_spec(
    entry: EntryKind,
    home: &Path,
    cwd: &Path,
    principal: Principal,
) -> RuntimeSpec {
    RuntimeSpec {
        entry,
        home: home.to_path_buf(),
        cwd: cwd.to_path_buf(),
        principal,
        mode_override: None,
        active_agent: None,
        reasoning_effort: None,
        approval_override: None,
        model_override: None,
        embedded: None,
        child_backend: None,
    }
}

/// Core session factory over one runtime assembly per hosted session (see
/// module docs).
pub struct LocalCoreFactory {
    model: Arc<dyn ModelProvider>,
    store: Arc<dyn StateStore>,
    base_spec: RuntimeSpec,
    /// Actor of the local owner; the fallback approval actor.
    owner_actor: ApprovalActor,
    /// Rights ceiling; the connection's tier cuts it, never widens it.
    rights: HostRights,
    /// Approval actor override (remote ingress: the one device allowed to
    /// resolve approvals). `None` = the creator's actor, else the owner's.
    actor: Option<ApprovalActor>,
}

impl std::fmt::Debug for LocalCoreFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalCoreFactory").finish_non_exhaustive()
    }
}

/// Binds the approval actor of the root session (see module docs).
///
/// Fails closed: a session without a spawn context would keep the core's own
/// actor, which differs from the connection's, and every approval would hang.
fn with_operator_actor(
    session: AgentSession,
    actor: ApprovalActor,
) -> Result<AgentSession, DriverBridgeError> {
    let mut context = session.spawn_context().cloned().ok_or_else(|| {
        DriverBridgeError::Runtime(
            "hosted session has no spawn context to bind the approval actor to".to_owned(),
        )
    })?;
    context.approval_actor = Some(actor);
    Ok(session.with_spawn_context(context))
}

fn hosted_thread(session_id: &SessionId) -> ThreadRef {
    ThreadRef::from_str(format!("session-host:{}", session_id.as_str()))
}

impl LocalCoreFactory {
    /// Factory over `model`, persisting transcripts under `sessions_root`.
    /// `base_spec` is the spec every session starts from (its principal is
    /// the fallback when no execution admission is given), `owner_actor` the
    /// local owner's approval actor.
    #[must_use]
    pub fn new(
        model: Arc<dyn ModelProvider>,
        sessions_root: &Path,
        base_spec: RuntimeSpec,
        owner_actor: ApprovalActor,
    ) -> Self {
        Self {
            model,
            store: Arc::new(TranscriptStateStore::new(
                TranscriptStore::new(sessions_root),
                hosted_thread,
            )),
            base_spec,
            owner_actor,
            rights: HostRights::operator(),
            actor: None,
        }
    }

    /// Binds the sessions' approval actor to `actor` (designated approver of
    /// a remote ingress) instead of the creator or the local owner.
    #[must_use]
    pub fn with_actor(mut self, actor: ApprovalActor) -> Self {
        self.actor = Some(actor);
        self
    }

    /// Hosts the sessions for a connection of `tier` (rights are cut to
    /// `permissions_for_tier(tier)`).
    #[must_use]
    pub fn with_tier(mut self, tier: PermissionTier) -> Self {
        self.rights.tier = tier;
        self
    }

    /// Replaces the whole rights ceiling.
    #[must_use]
    pub fn with_rights(mut self, rights: HostRights) -> Self {
        self.rights = rights;
        self
    }

    fn build_authorized(
        &self,
        session_id: &SessionId,
        _title: Option<&str>,
        wiring: SessionWiring,
        admission: Option<&ExecutionAdmission>,
    ) -> Result<CoreSession, DriverBridgeError> {
        let runtime = |error: String| DriverBridgeError::Runtime(error);
        let principal = admission
            .map(|admission| admission.principal_with_ceiling(self.rights.tier))
            .unwrap_or_else(|| self.base_spec.principal.clone());
        let spec = RuntimeSpec {
            principal,
            ..self.base_spec.clone()
        };
        let assembly = RuntimeAssembly::builder(spec)
            .model(ModelSource::Override(Arc::clone(&self.model)))
            .stores(RuntimeStores {
                state_store: Arc::clone(&self.store),
                job_store: None,
                approval_store: None,
            })
            .session_events(wiring.event_tx.clone())
            .root_session_id(session_id.clone())
            // Rights follow the connection's tier: the entry's `{R, W, X}`
            // ceiling is cut, never widened.
            .narrowing(RuntimeNarrowing {
                write_scope: self.rights.write_scope.clone(),
                registry_profile: RegistryProfile::Full,
                identity: IdentityOverrides::default(),
                permissions: permissions_for_tier(
                    admission.map_or(self.rights.tier, |admission| {
                        self.rights.tier.min(admission.maximum_tier())
                    }),
                ),
                workspace_root: self.rights.workspace_root.clone(),
            })
            .build()
            .map_err(|error| runtime(error.to_string()))?;
        let root = assembly
            .new_root_session(session_id.clone(), wiring.event_tx, wiring.turn_tx, None)
            .map_err(|error| runtime(error.to_string()))?;
        // A configured designated approver is host policy (remote ingress).
        // Otherwise bind approvals to the authenticated creator actor, and
        // without an admission to the local owner.
        let actor = self
            .actor
            .clone()
            .or_else(|| admission.map(|admission| admission.approval_actor().clone()))
            .unwrap_or_else(|| self.owner_actor.clone());
        let session = with_operator_actor(root.session, actor)?;
        Ok(CoreSession {
            session,
            model: Arc::clone(&self.model),
            store: Arc::clone(&self.store),
        })
    }
}

impl CoreSessionFactory for LocalCoreFactory {
    fn build(
        &self,
        session_id: &SessionId,
        title: Option<&str>,
        wiring: SessionWiring,
    ) -> Result<CoreSession, DriverBridgeError> {
        self.build_authorized(session_id, title, wiring, None)
    }

    fn build_with_execution(
        &self,
        session_id: &SessionId,
        title: Option<&str>,
        wiring: SessionWiring,
        admission: &ExecutionAdmission,
    ) -> Result<CoreSession, DriverBridgeError> {
        self.build_authorized(session_id, title, wiring, Some(admission))
    }
}

/// Creates `dir` (recursively) owner-only (`0700` on unix). An empty path is
/// a no-op.
///
/// # Errors
/// `"<path>: <io error>"` when the directory cannot be created.
pub fn private_dir(dir: &Path) -> Result<(), String> {
    if dir.as_os_str().is_empty() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        // `recursive` applies the mode to every directory it creates; an
        // existing directory is left as it is (the daemon refuses an insecure one).
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|error| format!("{}: {error}", dir.display()))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))
    }
}

/// The durable transcript and approval adapters of a host.
pub struct DurableStores {
    /// Durable transcripts below the sessions root.
    pub transcripts: Arc<DurableTranscripts>,
    /// Durable approvals below the sessions root.
    pub approvals: Arc<DurableApprovals>,
}

impl std::fmt::Debug for DurableStores {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DurableStores").finish_non_exhaustive()
    }
}

/// Opens the durable stores with the start-up recovery: the transcript tails
/// of every persisted hosted session are repaired before the host opens.
///
/// # Errors
/// A readable message when a store cannot be opened or recovered.
pub fn open_durable_stores(
    sessions_root: &Path,
    host_state_dir: &Path,
) -> Result<DurableStores, String> {
    let transcripts =
        DurableTranscripts::open(sessions_root).map_err(|error| format!("transcripts: {error}"))?;
    let persisted_sessions = RecordStore::new(host_state_dir)
        .list()
        .map_err(|error| format!("hosted session records: {error}"))?;
    let persisted_ids: Vec<_> = persisted_sessions
        .into_iter()
        .map(|record| record.session_id)
        .collect();
    transcripts
        .repair_tails(&persisted_ids)
        .map_err(|error| format!("transcript startup recovery: {error}"))?;
    let approvals =
        DurableApprovals::open(sessions_root).map_err(|error| format!("approvals: {error}"))?;
    Ok(DurableStores {
        transcripts: Arc::new(transcripts),
        approvals: Arc::new(approvals),
    })
}

/// Layout and policy of a local host.
#[derive(Clone, Debug)]
pub struct LocalHostConfig {
    /// The harw home (config, trust) the hosted sessions assemble from.
    pub home: PathBuf,
    /// Workspace the hosted sessions are bound to.
    pub workspace: PathBuf,
    /// State directory (host records, transcripts, approvals).
    pub state_dir: PathBuf,
    /// The owner the sessions belong to.
    pub owner: LocalOwner,
    /// Rights ceiling of the hosted sessions.
    pub rights: HostRights,
}

impl LocalHostConfig {
    /// Transcripts and approvals: `<state_dir>/sessions`.
    #[must_use]
    pub fn sessions_root(&self) -> PathBuf {
        self.state_dir.join(SESSIONS_DIR_NAME)
    }

    /// Host records: `<state_dir>/host`.
    #[must_use]
    pub fn host_state_dir(&self) -> PathBuf {
        self.state_dir.join(HOST_DIR_NAME)
    }

    /// The core session factory of this layout over `model`.
    #[must_use]
    pub fn core_factory(&self, model: Arc<dyn ModelProvider>) -> LocalCoreFactory {
        LocalCoreFactory::new(
            model,
            &self.sessions_root(),
            runtime_spec(
                EntryKind::SessionHost,
                &self.home,
                &self.workspace,
                self.owner.principal(IngressSurface::Tui),
            ),
            self.owner.actor(),
        )
        .with_rights(self.rights.clone())
    }
}

/// Failures of the local host.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LocalHostError {
    /// A directory, a durable store or the driver could not be set up.
    #[error("local host setup: {0}")]
    Setup(String),
    /// The session host refused to open or to connect.
    #[error("local host: {0}")]
    Host(String),
}

/// A running in-process session host.
pub struct LocalHost {
    host: SessionHost,
    owner: LocalOwner,
    state_dir: PathBuf,
}

impl std::fmt::Debug for LocalHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalHost").finish_non_exhaustive()
    }
}

impl LocalHost {
    /// Opens the host of `config` over `model`.
    ///
    /// Synchronous. Needs no tokio context and spawns no task. Async callers
    /// use `spawn_blocking`.
    ///
    /// # Errors
    /// [`LocalHostError::Setup`] when a directory, a store or the driver
    /// cannot be set up, [`LocalHostError::Host`] when the host does not open.
    pub fn open(
        config: LocalHostConfig,
        model: Arc<dyn ModelProvider>,
    ) -> Result<Self, LocalHostError> {
        std::fs::create_dir_all(&config.workspace).map_err(|error| {
            LocalHostError::Setup(format!("{}: {error}", config.workspace.display()))
        })?;
        let sessions_root = config.sessions_root();
        let host_state_dir = config.host_state_dir();
        private_dir(&config.state_dir).map_err(LocalHostError::Setup)?;
        private_dir(&sessions_root).map_err(LocalHostError::Setup)?;
        private_dir(&host_state_dir).map_err(LocalHostError::Setup)?;
        let stores =
            open_durable_stores(&sessions_root, &host_state_dir).map_err(LocalHostError::Setup)?;
        let factory = config.core_factory(model);
        let driver =
            CoreTurnDriver::with_factory(CoreDriverConfig::new(sessions_root), Arc::new(factory))
                .map_err(|error| LocalHostError::Setup(format!("session driver: {error}")))?;
        let host = SessionHost::open(
            HostConfig::new(host_state_dir),
            Arc::new(driver),
            stores.transcripts,
            stores.approvals,
        )
        .map_err(|error| LocalHostError::Host(error.to_string()))?;
        Ok(Self {
            host,
            owner: config.owner,
            state_dir: config.state_dir,
        })
    }

    /// A connection with the owner's identity.
    ///
    /// # Errors
    /// [`LocalHostError::Host`] when the host refuses (e.g. draining).
    pub fn connect_owner(&self) -> Result<HostConnection, LocalHostError> {
        self.host
            .connect(self.owner.identity())
            .map_err(|error| LocalHostError::Host(error.to_string()))
    }

    /// The session host.
    #[must_use]
    pub fn host(&self) -> &SessionHost {
        &self.host
    }

    /// The owner the sessions belong to.
    #[must_use]
    pub fn owner(&self) -> &LocalOwner {
        &self.owner
    }

    /// The state directory.
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Drains the host; the durable state stays on disk.
    pub fn shutdown(&self) {
        self.host.drain(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_authority::Permission;
    use harw_protocol::SessionPort;
    use harw_protocol::session_wire::{CreateParams, HelloParams, SESSION_WIRE_MINOR};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    /// The runtime tests' fixture UIA (`<home>/profiles/default`).
    fn write_fixture_uia(home: &Path) -> TestResult {
        let profile_dir = home.join("profiles").join("default");
        let agent_dir = profile_dir.join("agents").join("fixture-uia");
        std::fs::create_dir_all(&agent_dir)?;
        std::fs::write(
            agent_dir.join("definition.toml"),
            "schema = \"harwness.agent/v1\"\nid = \"harwness.agent.fixture-uia@1\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\n",
        )?;
        std::fs::write(
            profile_dir.join("config.toml"),
            "active_uia_definition = \"harwness.agent.fixture-uia@1\"\n",
        )?;
        Ok(())
    }

    /// A phone-like config below `root` with the fixture UIA in its home.
    fn config_in(root: &Path) -> TestResult<LocalHostConfig> {
        let home = root.join("home");
        write_fixture_uia(&home)?;
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace)?;
        Ok(LocalHostConfig {
            home,
            workspace,
            state_dir: root.join("state"),
            owner: LocalOwner::new("phone:owner", PermissionTier::Owner),
            rights: HostRights::operator(),
        })
    }

    fn echo_model() -> Arc<dyn ModelProvider> {
        Arc::new(harw_core::EchoModelProvider::new("hi"))
    }

    fn wiring() -> SessionWiring {
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (turn_tx, _turn_rx) = tokio::sync::mpsc::unbounded_channel();
        SessionWiring { event_tx, turn_tx }
    }

    fn permissions_of(built: &CoreSession) -> TestResult<Vec<Permission>> {
        let context = built
            .session
            .spawn_context()
            .ok_or("hosted session has a spawn context")?;
        Ok(context.sandbox.permissions().iter().collect())
    }

    #[test]
    fn a_session_without_spawn_context_fails_closed() -> TestResult {
        let (event_tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let session = harw_core::AgentSession::new(
            harw_types::roles::AgentRole::Assistant,
            None,
            harw_extension_api::ExtensionRegistryBuilder::default().build(),
            event_tx,
        );
        let actor = ApprovalActor::Operator {
            id: "uid:1".to_owned(),
        };
        // No spawn context: binding must fail instead of silently keeping the
        // core's own actor (every approval would then hang).
        assert!(matches!(
            with_operator_actor(session, actor),
            Err(DriverBridgeError::Runtime(_))
        ));
        Ok(())
    }

    #[test]
    fn factory_binds_owner_actor_and_cuts_rights() -> TestResult {
        let temp = tempfile::tempdir()?;
        let config = config_in(temp.path())?;
        let built = config
            .core_factory(echo_model())
            .build(&SessionId::new(), None, wiring())?;
        let actor = built
            .session
            .spawn_context()
            .and_then(|context| context.approval_actor.clone());
        assert_eq!(
            actor,
            Some(ApprovalActor::Operator {
                id: "phone:owner".to_owned(),
            })
        );
        // Operator rights: read + write, no process execution, no network.
        let rights = permissions_of(&built)?;
        assert!(rights.contains(&Permission::WriteWorkspace));
        assert!(!rights.contains(&Permission::ExecuteProcess));
        assert!(!rights.contains(&Permission::NetworkAccess));
        Ok(())
    }

    #[test]
    fn with_tier_observer_is_read_only() -> TestResult {
        let temp = tempfile::tempdir()?;
        let config = config_in(temp.path())?;
        let built = config
            .core_factory(echo_model())
            .with_tier(PermissionTier::Observer)
            .build(&SessionId::new(), None, wiring())?;
        assert_eq!(permissions_of(&built)?, vec![Permission::ReadWorkspace]);
        Ok(())
    }

    #[test]
    fn owner_identity_is_local_without_tool_or_gateway_caps() -> TestResult {
        let owner = LocalOwner::new("phone:owner", PermissionTier::Owner);
        let identity = owner.identity();
        identity.validate()?;
        assert!(matches!(identity.zone, TrustZone::Local));
        assert!(!identity.caps.tool_call);
        assert_eq!(identity.caps, caps_for_tier(PermissionTier::Owner));
        assert_eq!(identity.actor, owner.actor());
        assert_eq!(identity.label, "local-phone:owner");
        assert!(identity.tenant.is_none());
        assert_eq!(LocalOwner::for_uid(7, PermissionTier::Operator).id, "uid:7");
        Ok(())
    }

    #[test]
    fn runtime_spec_has_no_overrides() -> TestResult {
        let owner = LocalOwner::new("phone:owner", PermissionTier::Owner);
        let principal = owner.principal(IngressSurface::Tui);
        let spec = runtime_spec(
            EntryKind::SessionHost,
            Path::new("/h"),
            Path::new("/w"),
            principal.clone(),
        );
        assert_eq!(spec.entry, EntryKind::SessionHost);
        assert_eq!(spec.home, Path::new("/h"));
        assert_eq!(spec.cwd, Path::new("/w"));
        assert_eq!(spec.principal, principal);
        assert!(spec.mode_override.is_none());
        assert!(spec.active_agent.is_none());
        assert!(spec.reasoning_effort.is_none());
        assert!(spec.approval_override.is_none());
        assert!(spec.model_override.is_none());
        assert!(spec.embedded.is_none());
        assert!(spec.child_backend.is_none());
        Ok(())
    }

    fn hello() -> HelloParams {
        HelloParams {
            client_label: "test".into(),
            wire_minor: SESSION_WIRE_MINOR,
            features: vec![],
            requested_caps: None,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_host_persists_sessions_across_restarts() -> TestResult {
        let temp = tempfile::tempdir()?;
        let config = config_in(temp.path())?;

        let host = LocalHost::open(config.clone(), echo_model())?;
        let port = host.connect_owner()?;
        let ack = port.hello(hello()).await?;
        assert!(ack.granted.steer && ack.granted.approve);
        let created = port
            .create(CreateParams {
                workspace: None,
                title: Some("t".into()),
            })
            .await?;
        drop(port);
        host.shutdown();
        drop(host);

        let host = LocalHost::open(config, echo_model())?;
        let port = host.connect_owner()?;
        port.hello(hello()).await?;
        let sessions = port.list().await?;
        assert!(
            sessions
                .iter()
                .any(|summary| summary.session_id == created.session_id)
        );
        drop(port);
        host.shutdown();
        Ok(())
    }
}
