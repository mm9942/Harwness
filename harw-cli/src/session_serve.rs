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
//! - The core session factory ([`GatewayCoreFactory`]) assembles a
//!   **tool-less** `AgentSession` (empty extension registry, like the Dream
//!   and unbound-Telegram turns) over the model of the gateway's
//!   `GatewayDream` assembly and a durable `TranscriptStateStore`. There is
//!   no `EntryKind` for a session-host entry in the runtime contract table
//!   yet, so tools, sandbox and an approval actor are deliberately NOT wired;
//!   such sessions never park on an approval.
//! - TODO(node listener): `harw-node-listener::NodeListener` is not composed
//!   because `harw-config` has no node-transport section yet; no config is
//!   invented here.
//!
//! # Jobs rule
//! Everything on the serve path is async (`Daemon::run` on the gateway's
//! runtime); the only synchronous work is directory creation at start.

use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_core::{AgentSession, ModelProvider, StateStore, TranscriptStateStore};
use harw_extension_api::empty_extension_registry;
use harw_session_daemon::{Daemon, DaemonError, DaemonServices, UdsConfig};
use harw_session_driver::{
    CoreDriverConfig, CoreSession, CoreSessionFactory, CoreTurnDriver, DriverBridgeError,
    SessionWiring,
};
use harw_session_host::driver::TurnDriver;
use harw_session_host::{DurableApprovals, DurableTranscripts, HostConfig};
use harw_session_store::TranscriptStore;
use harw_types::{AgentRole, SessionId, ThreadRef};
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
}

impl SessionServeConfig {
    /// Layout for `profile_dir`; `socket` overrides the default location.
    ///
    /// Default socket: `$XDG_RUNTIME_DIR/harw/session.sock` (the place
    /// `harw attach` looks first), else `<profile>/session-host/run/session.sock`.
    #[must_use]
    pub fn for_profile(
        profile_dir: &Path,
        socket: Option<PathBuf>,
        xdg_runtime_dir: Option<&str>,
    ) -> Self {
        let state_dir = profile_dir.join(STATE_DIR_NAME);
        let socket = socket.unwrap_or_else(|| {
            match xdg_runtime_dir.filter(|dir| !dir.is_empty()) {
                Some(dir) => Path::new(dir).join("harw").join("session.sock"),
                None => state_dir.join("run").join("session.sock"),
            }
        });
        Self { state_dir, socket }
    }
}

/// Core session factory over the gateway's model (tool-less, see module docs).
pub struct GatewayCoreFactory {
    model: Arc<dyn ModelProvider>,
    store: Arc<dyn StateStore>,
}

impl std::fmt::Debug for GatewayCoreFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayCoreFactory").finish_non_exhaustive()
    }
}

fn hosted_thread(session_id: &SessionId) -> ThreadRef {
    ThreadRef::from_str(format!("session-host:{}", session_id.as_str()))
}

impl GatewayCoreFactory {
    /// Factory over `model`, persisting transcripts under `sessions_root`.
    #[must_use]
    pub fn new(model: Arc<dyn ModelProvider>, sessions_root: &Path) -> Self {
        Self {
            model,
            store: Arc::new(TranscriptStateStore::new(
                TranscriptStore::new(sessions_root),
                hosted_thread,
            )),
        }
    }
}

impl CoreSessionFactory for GatewayCoreFactory {
    fn build(
        &self,
        session_id: &SessionId,
        _title: Option<&str>,
        wiring: SessionWiring,
    ) -> Result<CoreSession, DriverBridgeError> {
        let session = AgentSession::new_with_id(
            session_id.clone(),
            AgentRole::Assistant,
            None,
            empty_extension_registry(),
            wiring.event_tx,
        )
        .with_turn_event_sink(wiring.turn_tx);
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

fn private_dir(dir: &Path) -> Result<(), String> {
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
    let factory = Arc::new(GatewayCoreFactory::new(model, &sessions_root));
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
    // The gateway's own uid: the owner of the directory it just created.
    let uid = std::fs::metadata(&config.state_dir)
        .map_err(|error| format!("{}: {error}", config.state_dir.display()))?
        .uid();
    let transcripts =
        DurableTranscripts::open(&sessions_root).map_err(|error| format!("transcripts: {error}"))?;
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
        let xdg = SessionServeConfig::for_profile(profile, None, Some("/run/user/1"));
        assert_eq!(xdg.socket, Path::new("/run/user/1/harw/session.sock"));
        let fallback = SessionServeConfig::for_profile(profile, None, None);
        assert_eq!(
            fallback.socket,
            Path::new("/p/session-host/run/session.sock")
        );
        let explicit = SessionServeConfig::for_profile(profile, Some("/x/s.sock".into()), None);
        assert_eq!(explicit.socket, Path::new("/x/s.sock"));
        assert_eq!(explicit.state_dir, Path::new("/p/session-host"));
    }

    #[tokio::test]
    async fn composed_ingress_answers_hello_and_list_then_removes_socket() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let config = SessionServeConfig::for_profile(temp.path(), None, None);
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
        let config = SessionServeConfig::for_profile(temp.path(), None, None);
        let first = start_with_driver(&config, Arc::new(NoopDriver)).await?;
        let second = start_with_driver(&config, Arc::new(NoopDriver)).await;
        assert!(second.is_err());
        first.shutdown().await;
        Ok(())
    }
}
