//! Composition root of the session WebSocket control plane for `harw gateway`.
//!
//! # Description
//! Opt-in (`harw gateway --session-socket[=PATH]`, default off) ingress that
//! the foreground gateway runs next to its agents and channels. This module
//! is composition only; protocol rules live in `harw-session-ws`, session and
//! policy rules in `harw-session-host`, the socket lifecycle (private
//! directory, peer-credential check, stale-socket handling, drain, socket
//! removal) in `harw-session-daemon`.
//!
//! ```text
//! harw gateway --session-socket
//!   `-- Daemon::start(HostConfig, DaemonServices { driver, transcripts, approvals }, UdsConfig)
//!        |-- driver      CoreTurnDriver::with_factory(.., GatewayCoreFactory)
//!        |-- transcripts DurableTranscripts::open(<state>/sessions)
//!        |-- approvals   DurableApprovals::open(<state>/sessions)
//!        `-- socket      0700 dir / 0600 socket, only the gateway's own uid
//! ```
//!
//! # What is wired
//! - Host, durable transcripts/approvals, the production `CoreTurnDriver`
//!   (`harw-core` durable turn loop) and the Unix-socket ingress are real.
//! - The core session factory ([`GatewayCoreFactory`]) assembles one
//!   `RuntimeAssembly` per hosted session (the registry is single-use) with
//!   `EntryKind::Tui` (full registry, sandbox bound to the gateway's cwd and
//!   narrowed by the entry profile, `AskResolution::Interactive`: a tool that
//!   needs approval parks the turn), the local owner as `Principal`
//!   (`uid:<euid>`), the gateway's resolved model (the provider of the
//!   `GatewayDream` assembly) and a durable `TranscriptStateStore`. The
//!   spawn context's approval actor is set to `Operator { id: "uid:<euid>" }`,
//!   exactly the actor the socket's peer identity carries
//!   (`harw_session_com::local::local_identity`), so the durable approval
//!   record binds the actor that may resolve it (a `Principal` of surface
//!   Tui would otherwise yield `local-tui`, which never matches).
//! - Known limitation: `RuntimeAssembly::build` reads config/trust files
//!   synchronously; the driver's factory trait is synchronous, so this happens
//!   on the gateway runtime thread once per newly created hosted session.
//! - TODO(node listener): `harw-node-listener::NodeListener` is not composed
//!   because `harw-config` has no node-transport section yet; no config is
//!   invented here.
//!
//! # Jobs rule
//! Everything on the serve path is async (`Daemon::run` on the gateway's
//! runtime); the only synchronous work is directory creation at start.

use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_core::{AgentSession, ModelProvider, StateStore, TranscriptStateStore};
use harw_runtime::{
    EntryKind, ModelSource, RuntimeAssembly, RuntimeNarrowing, RuntimeStores, permissions_for_tier,
};
use harw_session_daemon::{Daemon, DaemonError, DaemonServices, UdsConfig};
use harw_session_driver::{
    CoreDriverConfig, CoreSession, CoreSessionFactory, CoreTurnDriver, DriverBridgeError,
    SessionWiring,
};
use harw_session_host::driver::TurnDriver;
use harw_session_host::{DurableApprovals, DurableTranscripts, HostConfig};
use harw_session_store::TranscriptStore;
use harw_types::{ApprovalActor, IngressSurface, PermissionTier, SessionId, ThreadRef};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Directory below the profile that holds the session host's state.
const STATE_DIR_NAME: &str = "session-host";
/// Transcripts and approvals of hosted sessions below the state directory.
const SESSIONS_DIR_NAME: &str = "sessions";

/// Where the session ingress keeps its state and socket.
#[derive(Debug, Clone)]
pub struct SessionServeConfig {
    /// State directory (host records, transcripts, approvals).
    pub state_dir: PathBuf,
    /// Socket path; its parent directory is created `0700` if missing.
    pub socket: PathBuf,
    /// The harw home (config, trust) the hosted sessions assemble from.
    pub home: PathBuf,
    /// Workspace the hosted sessions are bound to (the gateway's cwd).
    pub cwd: PathBuf,
    /// Tier the hosted sessions run at; it cuts their sandbox rights
    /// (`permissions_for_tier`). Default: operator, like `local_principal`.
    pub tier: PermissionTier,
}

impl SessionServeConfig {
    /// Layout for `profile_dir`; `socket` overrides the default location.
    ///
    /// Default socket: `$XDG_RUNTIME_DIR/harw/session.sock` (the place
    /// `harw attach` looks first), else `<profile>/session-host/run/session.sock`.
    #[must_use]
    pub fn for_profile(
        home: &Path,
        cwd: &Path,
        profile_dir: &Path,
        socket: Option<PathBuf>,
        xdg_runtime_dir: Option<&str>,
    ) -> Self {
        let state_dir = profile_dir.join(STATE_DIR_NAME);
        let socket =
            socket.unwrap_or_else(|| match xdg_runtime_dir.filter(|dir| !dir.is_empty()) {
                Some(dir) => Path::new(dir).join("harw").join("session.sock"),
                None => state_dir.join("run").join("session.sock"),
            });
        Self {
            state_dir,
            socket,
            home: home.to_path_buf(),
            cwd: cwd.to_path_buf(),
            tier: PermissionTier::Operator,
        }
    }
}

/// Core session factory over the gateway runtime assembly (see module docs).
pub struct GatewayCoreFactory {
    model: Arc<dyn ModelProvider>,
    store: Arc<dyn StateStore>,
    home: PathBuf,
    cwd: PathBuf,
    uid: u32,
    /// Tier of the connection the sessions are hosted for; it cuts the
    /// session's sandbox rights (`permissions_for_tier`), never widens them.
    tier: PermissionTier,
    /// Approval actor override (remote ingress: the one device allowed to
    /// resolve approvals). `None` = the local owner's `uid:<euid>`.
    actor: Option<ApprovalActor>,
}

impl std::fmt::Debug for GatewayCoreFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayCoreFactory").finish_non_exhaustive()
    }
}

/// Binds the approval actor of the root session (see module docs).
///
/// Fails closed: a session without a spawn context would keep the core's own
/// actor, which differs from the socket peer's, and every approval would hang.
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

impl GatewayCoreFactory {
    /// Factory over `model`, persisting transcripts under `sessions_root`.
    /// `home` is the harw home, `cwd` the workspace the sessions are bound
    /// to and `uid` the effective uid of the socket's only permitted peer.
    #[must_use]
    pub fn new(
        model: Arc<dyn ModelProvider>,
        sessions_root: &Path,
        home: PathBuf,
        cwd: PathBuf,
        uid: u32,
    ) -> Self {
        Self {
            model,
            store: Arc::new(TranscriptStateStore::new(
                TranscriptStore::new(sessions_root),
                hosted_thread,
            )),
            home,
            cwd,
            uid,
            // The local socket's peer is the gateway's own uid, mapped like
            // `local_principal`: an operator.
            tier: PermissionTier::Operator,
            actor: None,
        }
    }

    /// Binds the sessions' approval actor to `actor` instead of the local
    /// owner (remote ingress, see `crate::session_serve_remote`).
    #[must_use]
    #[allow(dead_code)] // used by the remote ingress composition (not wired yet)
    pub fn with_actor(mut self, actor: ApprovalActor) -> Self {
        self.actor = Some(actor);
        self
    }

    /// Hosts the sessions for a connection of `tier` (rights are cut to
    /// `permissions_for_tier(tier)`).
    #[must_use]
    pub fn with_tier(mut self, tier: PermissionTier) -> Self {
        self.tier = tier;
        self
    }

    /// The actor the socket's peer identity carries for `uid`.
    fn operator_actor(&self) -> ApprovalActor {
        self.actor
            .clone()
            .unwrap_or_else(|| ApprovalActor::Operator {
                id: format!("uid:{}", self.uid),
            })
    }
}

impl CoreSessionFactory for GatewayCoreFactory {
    fn build(
        &self,
        session_id: &SessionId,
        _title: Option<&str>,
        wiring: SessionWiring,
    ) -> Result<CoreSession, DriverBridgeError> {
        let runtime = |error: String| DriverBridgeError::Runtime(error);
        let spec = crate::runtime_entry::runtime_spec(
            EntryKind::SessionHost,
            &self.home,
            &self.cwd,
            crate::runtime_entry::local_principal(IngressSurface::Tui),
        );
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
                registry_profile: harw_registry_defaults::RegistryProfile::Full,
                identity: harw_registry_defaults::IdentityOverrides::default(),
                permissions: permissions_for_tier(self.tier),
                workspace_root: None,
            })
            .build()
            .map_err(|error| runtime(error.to_string()))?;
        let root = assembly
            .new_root_session(session_id.clone(), wiring.event_tx, wiring.turn_tx, None)
            .map_err(|error| runtime(error.to_string()))?;
        let session = with_operator_actor(root.session, self.operator_actor())?;
        Ok(CoreSession {
            session,
            model: Arc::clone(&self.model),
            store: Arc::clone(&self.store),
        })
    }
}

/// A running session ingress.
#[derive(Debug)]
pub struct SessionIngress {
    shutdown: watch::Sender<bool>,
    task: JoinHandle<Result<(), DaemonError>>,
    socket: PathBuf,
}

impl SessionIngress {
    /// The bound socket path.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Stop accepting, drain the host (live sessions get the `Draining`
    /// reason) and remove the socket; logs instead of failing.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        match self.task.await {
            Ok(Ok(())) => {
                tracing::info!(socket = %self.socket.display(), "session ingress stopped");
            }
            Ok(Err(error)) => tracing::warn!(%error, "session ingress stopped with an error"),
            Err(error) => tracing::error!(%error, "session ingress task failed"),
        }
    }
}

/// The gateway's effective uid: the only peer the socket serves.
fn own_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

pub(crate) fn private_dir(dir: &Path) -> Result<(), String> {
    if dir.as_os_str().is_empty() {
        return Ok(());
    }
    // `recursive` applies the mode to every directory it creates; an
    // existing directory is left as it is (the daemon refuses an insecure one).
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|error| format!("{}: {error}", dir.display()))
}

/// Compose and start the ingress with the production driver over `model`.
///
/// # Errors
/// A readable message when a directory, the durable adapters, the driver, the
/// host or the socket cannot be set up.
pub async fn start(
    config: &SessionServeConfig,
    model: Arc<dyn ModelProvider>,
) -> Result<SessionIngress, String> {
    let sessions_root = config.state_dir.join(SESSIONS_DIR_NAME);
    private_dir(&sessions_root)?;
    let factory = Arc::new(
        GatewayCoreFactory::new(
            model,
            &sessions_root,
            config.home.clone(),
            config.cwd.clone(),
            own_uid(),
        )
        .with_tier(config.tier),
    );
    let driver = CoreTurnDriver::with_factory(CoreDriverConfig::new(sessions_root), factory)
        .map_err(|error| format!("session driver: {error}"))?;
    start_with_driver(config, Arc::new(driver)).await
}

/// Compose and start the ingress around an injected `driver` (tests use a
/// fake one).
///
/// # Errors
/// See [`start`].
pub async fn start_with_driver(
    config: &SessionServeConfig,
    driver: Arc<dyn TurnDriver>,
) -> Result<SessionIngress, String> {
    let sessions_root = config.state_dir.join(SESSIONS_DIR_NAME);
    private_dir(&sessions_root)?;
    if let Some(parent) = config.socket.parent() {
        private_dir(parent)?;
    }
    let uid = own_uid();
    let transcripts = DurableTranscripts::open(&sessions_root)
        .map_err(|error| format!("transcripts: {error}"))?;
    let approvals =
        DurableApprovals::open(&sessions_root).map_err(|error| format!("approvals: {error}"))?;
    let services = DaemonServices::new(driver, Arc::new(transcripts), Arc::new(approvals));
    let daemon = Daemon::start(
        HostConfig::new(config.state_dir.join("host")),
        services,
        UdsConfig::new(&config.socket, uid),
    )
    .await
    .map_err(|error| format!("session ingress {}: {error}", config.socket.display()))?;
    let (shutdown, rx) = watch::channel(false);
    let task = tokio::spawn(daemon.run(rx));
    Ok(SessionIngress {
        shutdown,
        task,
        socket: config.socket.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_protocol::SessionPort;
    use harw_session_host::driver::{
        CancelSignal, DriverFuture, EventSink, Setting, TurnInput, TurnOutcome,
    };
    use harw_session_remote::{ConnectOptions, RemotePort, connect_unix};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    struct NoopDriver;

    impl TurnDriver for NoopDriver {
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

    #[test]
    fn default_layout_prefers_xdg_runtime_dir() {
        let profile = Path::new("/p");
        let xdg =
            SessionServeConfig::for_profile(profile, profile, profile, None, Some("/run/user/1"));
        assert_eq!(xdg.socket, Path::new("/run/user/1/harw/session.sock"));
        let fallback = SessionServeConfig::for_profile(profile, profile, profile, None, None);
        assert_eq!(
            fallback.socket,
            Path::new("/p/session-host/run/session.sock")
        );
        let explicit = SessionServeConfig::for_profile(
            profile,
            profile,
            profile,
            Some("/x/s.sock".into()),
            None,
        );
        assert_eq!(explicit.socket, Path::new("/x/s.sock"));
        assert_eq!(explicit.state_dir, Path::new("/p/session-host"));
    }

    #[tokio::test]
    async fn composed_ingress_answers_hello_and_list_then_removes_socket() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let config =
            SessionServeConfig::for_profile(temp.path(), temp.path(), temp.path(), None, None);
        let ingress = start_with_driver(&config, Arc::new(NoopDriver)).await?;
        let socket = ingress.socket().to_path_buf();

        let socket_mode = std::fs::metadata(&socket)?.permissions().mode() & 0o777;
        assert_eq!(socket_mode, 0o600);
        let dir_mode = std::fs::metadata(socket.parent().ok_or("no parent")?)?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);

        let connection =
            connect_unix(socket.clone(), ConnectOptions::new("session_serve test")).await?;
        // `connect_unix` has completed `session.hello`.
        let _epoch = connection.hello_ack().host_epoch;
        let port = RemotePort::new(connection);
        let sessions = port.list().await?;
        assert!(sessions.is_empty());
        drop(port);

        ingress.shutdown().await;
        assert!(!socket.exists(), "shutdown must remove the socket");
        Ok(())
    }

    #[tokio::test]
    async fn second_start_on_a_live_socket_is_refused() -> TestResult {
        let temp = tempfile::tempdir()?;
        let config =
            SessionServeConfig::for_profile(temp.path(), temp.path(), temp.path(), None, None);
        let first = start_with_driver(&config, Arc::new(NoopDriver)).await?;
        let second = start_with_driver(&config, Arc::new(NoopDriver)).await;
        assert!(second.is_err());
        first.shutdown().await;
        Ok(())
    }

    /// The Tui entry requires an active UIA: write the same fixture the
    /// runtime tests use (`<home>/profiles/default`).
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

    fn factory_in(dir: &Path, uid: u32) -> GatewayCoreFactory {
        GatewayCoreFactory::new(
            Arc::new(harw_core::EchoModelProvider::new("hello from the core")),
            &dir.join("sessions"),
            dir.to_path_buf(),
            dir.to_path_buf(),
            uid,
        )
    }

    fn wiring() -> SessionWiring {
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (turn_tx, _turn_rx) = tokio::sync::mpsc::unbounded_channel();
        SessionWiring { event_tx, turn_tx }
    }

    #[test]
    fn factory_binds_the_peer_identity_actor_and_builds_per_session() -> TestResult {
        let temp = tempfile::tempdir()?;
        write_fixture_uia(temp.path())?;
        let factory = factory_in(temp.path(), 4242);
        let first = SessionId::new();
        let second = SessionId::new();
        let built = factory.build(&first, None, wiring())?;
        // Same actor the socket peer identity carries for this uid.
        let expected = ApprovalActor::Operator {
            id: "uid:4242".to_owned(),
        };
        let actor = built
            .session
            .spawn_context()
            .and_then(|context| context.approval_actor.clone());
        assert_eq!(actor, Some(expected));
        // One assembly per session: a second session builds independently.
        let other = factory.build(&second, None, wiring())?;
        assert_eq!(other.session.id(), &second);
        Ok(())
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
    fn hosted_session_rights_follow_the_connection_tier() -> TestResult {
        use harw_authority::Permission;
        let temp = tempfile::tempdir()?;
        write_fixture_uia(temp.path())?;
        let rights_for = |tier: PermissionTier| -> TestResult<Vec<Permission>> {
            let factory = factory_in(temp.path(), 4242).with_tier(tier);
            let built = factory.build(&SessionId::new(), None, wiring())?;
            let context = built
                .session
                .spawn_context()
                .ok_or("hosted session has a spawn context")?;
            Ok(context.sandbox.permissions().iter().collect())
        };
        // The entry's `{R, W, X}` ceiling is cut, never widened, and never
        // grants network, secrets, plugins or containers.
        let observer = rights_for(PermissionTier::Observer)?;
        assert_eq!(observer, vec![Permission::ReadWorkspace]);
        let operator = rights_for(PermissionTier::Operator)?;
        assert!(operator.contains(&Permission::WriteWorkspace));
        assert!(!operator.contains(&Permission::ExecuteProcess));
        let owner = rights_for(PermissionTier::Owner)?;
        assert!(owner.contains(&Permission::ExecuteProcess));
        for rights in [&observer, &operator, &owner] {
            assert!(!rights.contains(&Permission::NetworkAccess));
            assert!(!rights.contains(&Permission::ManageContainers));
            assert!(!rights.contains(&Permission::ReadSecrets));
        }
        Ok(())
    }

    /// The whole chain over a real socket: production `CoreTurnDriver` +
    /// `GatewayCoreFactory` (scripted echo model), durable adapters; one
    /// client runs a turn, a second client sees the durable transcript.
    #[tokio::test]
    async fn e2e_turn_through_the_core_is_visible_to_a_second_client() -> TestResult {
        use harw_protocol::session_wire::{AttachParams, CreateParams, SubmitParams, SubmitResult};
        let temp = tempfile::tempdir()?;
        write_fixture_uia(temp.path())?;
        let config =
            SessionServeConfig::for_profile(temp.path(), temp.path(), temp.path(), None, None);
        let model: Arc<dyn ModelProvider> =
            Arc::new(harw_core::EchoModelProvider::new("hello from the core"));
        let ingress = start(&config, model).await?;
        let socket = ingress.socket().to_path_buf();

        let first =
            RemotePort::new(connect_unix(socket.clone(), ConnectOptions::new("one")).await?);
        let created = first
            .create(CreateParams {
                workspace: None,
                title: Some("e2e".to_owned()),
            })
            .await?;
        let session = created.session_id;
        let (ack, _frames) = first
            .attach(AttachParams {
                session_id: session.clone(),
                from: None,
                profile: Default::default(),
                tail_items: 50,
            })
            .await?;
        let submitted = first
            .submit(SubmitParams {
                session_id: session.clone(),
                text: "hi".to_owned(),
                expect_head: ack.head,
                client_msg_id: "m1".to_owned(),
                force: false,
            })
            .await?;
        assert!(
            matches!(submitted, SubmitResult::Accepted { .. }),
            "{submitted:?}"
        );

        let second = RemotePort::new(connect_unix(socket, ConnectOptions::new("two")).await?);
        let mut seen = 0_u64;
        for _ in 0..100 {
            let (ack, _frames) = second
                .attach(AttachParams {
                    session_id: session.clone(),
                    from: None,
                    profile: Default::default(),
                    tail_items: 50,
                })
                .await?;
            seen = ack.head.durable;
            let _ = second.detach(session.clone()).await;
            if seen > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            seen > 0,
            "the second client must see the durable transcript"
        );
        drop((first, second));
        ingress.shutdown().await;
        Ok(())
    }
}
