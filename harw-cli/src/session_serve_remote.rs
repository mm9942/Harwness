//! Composition root of the own-cloud (remote) session ingress.
//!
//! # Description
//! The counterpart of [`crate::session_serve`] for remote devices: a node
//! transport server (mutual, pinned ML-DSA-65 handshake) plus the
//! `harw-node-listener` upgrade handler on a TCP listener. Composition only;
//! identity rules live in `harw-node-listener`, session rules in
//! `harw-session-host`.
//!
//! ```text
//! start_remote(config, model, LocalNode, verifier, TcpListener)
//!   |-- SessionHost  own records    <profile>/session-host/remote/host
//!   |-- transcripts  share off:     <profile>/session-host/remote/sessions
//!   |                share on:      <profile>/sessions (the TUI's store)
//!   |-- driver       CoreTurnDriver + GatewayCoreFactory (tier = config.tier)
//!   |-- identity     RegistryIdentityMapper (<state>/remote/node-devices.conf)
//!   `-- listener     NodeListener::serve (revocation closes live connections)
//! ```
//!
//! # Why a separate host
//! A hosted session's rights come from the factory, which does not see who
//! created the session. The local socket's peer is the gateway's own uid
//! (an operator); remote devices are different principals. They therefore get
//! their **own host, records and factory**: the sandbox rights are cut to
//! [`RemoteServeConfig::tier`] (default observer), and a device's registry tier
//! only decides its host caps.
//!
//! # Sharing the TUI's sessions
//! [`RemoteServeConfig::share`] (`[session_listener] share_profile_sessions`,
//! default `off`) decides whether remote devices see the profile's own
//! sessions. With `off` the remote host keeps its private transcript store and
//! nothing of the profile is visible. With `read` or `continue` the transcript
//! and approval store is the profile store `<profile>/sessions`, the core
//! writes with the `cli-session:<id>` thread (the same one `harw` uses, so a
//! continued TUI session sees its earlier entries), a read-only
//! [`crate::profile_index::FsProfileIndex`] lists the profile's sessions and
//! projects for the host, and every session runs in its checked working
//! directory (`project_root`/`cwd` of its meta, only inside the known project
//! roots, narrowed by `share_roots`). Host records stay in the remote
//! directory. Leases, meta writes on adopt and turn end, and the read/continue
//! gates are the host's (`harw-session-host`); this module only composes.
//! Startup tail repair runs for the host's own record ids only, never for all
//! profile sessions (another process may be writing them).
//!
//! # Approvals
//! A parked approval is bound to one actor. Remote sessions bind it to
//! [`RemoteServeConfig::approval_device`] (`device:<id>`), which must also be
//! enrolled with the `approve` opt-in in the device registry. Without it the
//! actor is a placeholder nobody carries and a tool needing approval parks
//! forever (fail closed): a remote tier or key never grants approval by itself.
//!
//! # Wiring
//! `harw gateway` starts this from `[session_listener]` (global-only, default
//! off) via `crate::session_listener::start_from_config`: the node signer is
//! the AuthHub key `harw.node-identity/<node_id>`, trust comes from
//! `node-peers.conf`, authorisation from `node-devices.conf`. This module
//! itself takes the node, verifier and listener as parameters so the chain is
//! tested over a real loopback socket with in-process keys.
//!
//! # Live owner policy
//!
//! Registry rights and retained creator identities are revalidated every
//! `revocation_poll` (default five seconds). Device removal, ambiguous or
//! unreadable policy invalidates existing authority without banning a future
//! valid authentication. Explicit `revoked` rows remain permanent host bans.
//! Peer pins are bound to the verifier's exact owner file at startup. Any pin
//! file edit or read failure cuts retained authority and blocks authentication
//! until the ingress restarts with verified fresh pins.
//!
//! # Jobs rule
//! Everything on the serve path is async on the caller's runtime.

use std::collections::HashSet;
use std::io::Read as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use harw_core::ModelProvider;
use harw_node_listener::{
    DeviceRecord, DeviceRegistry, HostRevoker, IdentityMapper, NodeListener,
    RegistryIdentityMapper, RevocationSink,
};
use harw_node_transport::{LocalNode, NodeTransportServer, NodeVerifier, ServerOptions};
use harw_session_daemon::DaemonServices;
use harw_session_driver::{CoreDriverConfig, CoreTurnDriver};
use harw_session_host::driver::TurnDriver;
use harw_session_host::index::{ProfileIndex, ProfileShare, ShareMode, ShareSettings};
use harw_session_host::{DurableApprovals, DurableTranscripts, HostConfig, SessionHost, ToolHost};
use harw_session_ws::WsLimits;
use harw_types::{ApprovalActor, DeviceId, PermissionTier};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::profile_index::{FsProfileIndex, IndexOptions};
use crate::session_listener::SharePlan;
use crate::session_serve::{GatewayCoreFactory, SessionCwdResolver, private_dir};

/// Directory below the profile's session-host state that belongs to the
/// remote ingress.
const REMOTE_DIR_NAME: &str = "remote";

/// The remote ingress state directory of the active profile:
/// `<profile>/session-host/remote/` (where `node-devices.conf` and
/// `node-peers.conf` live). Resolved exactly as `harw gateway` does; nothing
/// is created.
///
/// # Errors
/// An invalid active profile name.
pub(crate) fn remote_state_dir(home: &Path) -> Result<PathBuf, String> {
    let profile = harw_home::active_profile_name(home);
    let dir = harw_home::profile_dir(home, &profile)
        .map_err(|error| format!("Profilverzeichnis '{profile}' nicht auflösbar: {error}"))?;
    Ok(dir
        .join(crate::session_serve::STATE_DIR_NAME)
        .join(REMOTE_DIR_NAME))
}
/// Hosted sessions' transcripts and approvals below the remote directory.
const SESSIONS_DIR_NAME: &str = "sessions";
/// Actor of remote sessions when no approving device is configured: no
/// connection ever carries it.
const NO_APPROVER: &str = "remote:no-approver";

/// Where the remote ingress keeps its state and what it may do.
#[derive(Debug, Clone)]
pub struct RemoteServeConfig {
    /// `<profile>/session-host` (the remote ingress uses `<state_dir>/remote`).
    pub state_dir: PathBuf,
    /// The harw home hosted sessions assemble from.
    pub home: PathBuf,
    /// Workspace hosted sessions are bound to.
    pub cwd: PathBuf,
    /// Tier the remote sessions' sandbox rights are cut to. Default observer.
    pub tier: PermissionTier,
    /// The one device allowed to resolve approvals of remote sessions (it must
    /// also carry the registry `approve` opt-in). `None`: approvals park.
    pub approval_device: Option<DeviceId>,
    /// Device permitted to link the gateway provider account.
    pub provider_link_device: Option<DeviceId>,
    /// How often registry and pin policy is revalidated (live invalidation).
    /// Default 5 s; values below 10 ms are clamped to 10 ms.
    pub revocation_poll: Duration,
    /// Sharing of the profile's own sessions (`[session_listener]
    /// share_*`). Default off: the remote host keeps its private store.
    pub share: SharePlan,
    /// Digest of the exact owner pin file used to build the transport verifier.
    pub(crate) peer_pins_digest: Option<[u8; 32]>,
    #[cfg(test)]
    injected_peer_trust: bool,
}

struct AccountLink {
    owner: DeviceId,
    home: PathBuf,
}

impl harw_node_listener::ProviderLink for AccountLink {
    fn handle<'a>(
        &'a self,
        identity: harw_session_host::ClientIdentity,
        method: http::Method,
        body: bytes::Bytes,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = http::StatusCode> + Send + 'a>> {
        Box::pin(async move {
            use harw_oauth::chatgpt_plan::{ChatGptPlanError, ChatGptPlanStore};
            use http::StatusCode;
            if identity.device.as_ref() != Some(&self.owner) {
                return StatusCode::FORBIDDEN;
            }
            let store = ChatGptPlanStore::new(&self.home);
            let result = match method {
                http::Method::GET if body.is_empty() => store.linked().await.map(|linked| {
                    if linked {
                        StatusCode::NO_CONTENT
                    } else {
                        StatusCode::NOT_FOUND
                    }
                }),
                http::Method::DELETE if body.is_empty() => {
                    store.disconnect().await.map(|confirmed| {
                        if confirmed {
                            StatusCode::NO_CONTENT
                        } else {
                            StatusCode::ACCEPTED
                        }
                    })
                }
                http::Method::POST => {
                    let Ok(record) = serde_json::from_slice(&body) else {
                        return StatusCode::BAD_REQUEST;
                    };
                    store
                        .store_verified(&record)
                        .await
                        .map(|()| StatusCode::NO_CONTENT)
                }
                _ => return StatusCode::BAD_REQUEST,
            };
            result.unwrap_or_else(|error| match error {
                ChatGptPlanError::DifferentRegistration => StatusCode::CONFLICT,
                ChatGptPlanError::OpenAiUnavailable => StatusCode::SERVICE_UNAVAILABLE,
                ChatGptPlanError::Storage => StatusCode::INTERNAL_SERVER_ERROR,
                _ => StatusCode::UNPROCESSABLE_ENTITY,
            })
        })
    }
}

impl RemoteServeConfig {
    /// Conservative defaults: observer sandbox, nobody approves.
    #[must_use]
    pub fn new(state_dir: &Path, home: &Path, cwd: &Path) -> Self {
        Self {
            state_dir: state_dir.to_path_buf(),
            home: home.to_path_buf(),
            cwd: cwd.to_path_buf(),
            tier: PermissionTier::Observer,
            approval_device: None,
            provider_link_device: None,
            revocation_poll: Duration::from_secs(5),
            share: SharePlan::off(),
            peer_pins_digest: None,
            #[cfg(test)]
            injected_peer_trust: false,
        }
    }

    fn remote_dir(&self) -> PathBuf {
        self.state_dir.join(REMOTE_DIR_NAME)
    }

    fn actor(&self) -> ApprovalActor {
        let id = self.approval_device.as_ref().map_or_else(
            || NO_APPROVER.to_owned(),
            |device| format!("device:{}", device.as_str()),
        );
        ApprovalActor::Operator { id }
    }
}

/// The profile store shared with remote devices (`share.mode != off`).
struct SharedProfile {
    /// `<profile>/sessions` (transcripts, metas, approvals, leases).
    sessions_root: PathBuf,
    index: Arc<FsProfileIndex>,
    settings: ShareSettings,
}

/// Resolves the shared profile store of `config`; `None` when sharing is off.
///
/// # Errors
/// Sharing without a tenant (never a panic, although `SharePlan` validation
/// already rejects it) or an unresolvable profile sessions directory.
fn open_share(config: &RemoteServeConfig) -> Result<Option<SharedProfile>, String> {
    let share = &config.share;
    if matches!(share.mode, ShareMode::Off) {
        return Ok(None);
    }
    let tenant = share.tenant.clone().ok_or_else(|| {
        "[session_listener] share_tenant fehlt für share_profile_sessions".to_owned()
    })?;
    let sessions_root = crate::runtime_entry::profile_sessions_root(&config.home)?;
    let index = Arc::new(FsProfileIndex::open(
        sessions_root.clone(),
        IndexOptions {
            since: share.since,
            share_roots: share.roots.clone(),
            new_project_root: share.new_project_root.clone(),
        },
    ));
    Ok(Some(SharedProfile {
        sessions_root,
        index,
        settings: ShareSettings {
            mode: share.mode,
            tenant,
            legacy_rows: share.legacy_rows,
            listener_tier: config.tier,
        },
    }))
}

/// Working directory of a shared session: the index's checked
/// `project_root`/`cwd`. `None` (unknown, outside the known roots, lookup
/// error) falls back to the factory's default `cwd`; the host itself refuses
/// to continue such a session. Paths are never logged.
fn workdir_resolver(index: Arc<dyn ProfileIndex>) -> SessionCwdResolver {
    Arc::new(move |id: &harw_types::SessionId| match index.workdir(id) {
        Ok(dir) => dir,
        Err(_) => {
            tracing::warn!(session = %id, "profile index: working directory lookup failed");
            None
        }
    })
}

/// Owner policy files are bounded regular files and are never followed through
/// symlinks. Pin changes require restart because the verifier is immutable.
const POLICY_MAX_BYTES: usize = 1024 * 1024;

pub(crate) fn read_peer_pins(path: &Path) -> Result<String, String> {
    let file = harw_fsutil::open_nofollow(path, harw_fsutil::OpenMode::read_only())
        .map_err(|error| format!("remote peer pins unavailable: {error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > POLICY_MAX_BYTES as u64 {
        return Err("remote peer pins exceed their read limit or are not a regular file".into());
    }
    let mut text = String::new();
    file.take(POLICY_MAX_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    if text.len() > POLICY_MAX_BYTES {
        return Err("remote peer pins exceed their read limit".into());
    }
    let (peers, skipped) = crate::session_listener::parse_peers(&text);
    let lines = text
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with('#')
        })
        .count();
    if skipped != 0 || peers.len() != lines {
        return Err("remote peer pins are malformed or ambiguous".into());
    }
    Ok(text)
}

struct PeerPolicyGate {
    path: PathBuf,
    expected: Option<[u8; 32]>,
    fingerprints: std::collections::BTreeMap<String, [u8; 32]>,
    stale: AtomicBool,
}

impl PeerPolicyGate {
    fn open(config: &RemoteServeConfig) -> Result<Self, String> {
        let path = config
            .remote_dir()
            .join(crate::session_listener::PEERS_FILE);
        #[cfg(test)]
        if config.injected_peer_trust {
            return Ok(Self {
                path,
                expected: None,
                fingerprints: Default::default(),
                stale: AtomicBool::new(false),
            });
        }
        let text = read_peer_pins(&path)?;
        let digest = *blake3::hash(text.as_bytes()).as_bytes();
        if config.peer_pins_digest != Some(digest) {
            return Err("remote peer pins are not bound to the transport verifier".into());
        }
        Ok(Self {
            path,
            expected: Some(digest),
            fingerprints: crate::session_listener::peer_key_fingerprints(&text),
            stale: AtomicBool::new(false),
        })
    }

    fn current(&self) -> bool {
        self.current_with(|| read_peer_pins(&self.path))
    }

    fn current_with(&self, read: impl FnOnce() -> Result<String, String>) -> bool {
        if self.stale.load(Ordering::Acquire) {
            return false;
        }
        let Some(expected) = self.expected else {
            // Only an explicit cfg(test) fixture can omit disk trust.
            return true;
        };
        let current =
            read().is_ok_and(|text| *blake3::hash(text.as_bytes()).as_bytes() == expected);
        if !current {
            self.stale.store(true, Ordering::Release);
        }
        current && !self.stale.load(Ordering::Acquire)
    }
}

struct PolicyIdentityMapper {
    mapper: RegistryIdentityMapper,
    pins: Arc<PeerPolicyGate>,
}

impl IdentityMapper for PolicyIdentityMapper {
    fn map(
        &self,
        peer: &harw_node_transport::AuthenticatedPeer,
        connection: harw_session_host::ConnectionId,
    ) -> harw_node_listener::MapFuture<'_> {
        if !self.pins.current() {
            return Box::pin(async { Err(harw_node_listener::ListenerError::UnknownPeer) });
        }
        let fingerprint = self.pins.fingerprints.get(peer.node_id.as_str()).copied();
        if self.pins.expected.is_some() && fingerprint.is_none() {
            return Box::pin(async { Err(harw_node_listener::ListenerError::UnknownPeer) });
        }
        let mapped = self.mapper.map(peer, connection);
        Box::pin(async move {
            let mut identity = mapped.await?;
            if !self.pins.current() {
                return Err(harw_node_listener::ListenerError::UnknownPeer);
            }
            identity.authenticated_key_fingerprint = fingerprint;
            Ok(identity)
        })
    }
}

/// A running remote ingress.
pub struct RemoteIngress {
    pub(crate) host: SessionHost,
    shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
    watcher: JoinHandle<()>,
    addr: SocketAddr,
    intake: harw_node_listener::StreamIntake,
    guards: Vec<Box<dyn std::any::Any + Send + Sync>>,
}

impl std::fmt::Debug for RemoteIngress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteIngress")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

impl RemoteIngress {
    /// The bound address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// A cloneable handle that feeds already-established byte streams
    /// (for example from an onion service) into the same node listener.
    #[must_use]
    pub(crate) fn intake(&self) -> harw_node_listener::StreamIntake {
        self.intake.clone()
    }

    /// Keeps `guard` alive until shutdown; dropping it stops that extra
    /// ingress, for example the onion service.
    pub(crate) fn attach(&mut self, guard: Box<dyn std::any::Any + Send + Sync>) {
        self.guards.push(guard);
    }

    /// Stops accepting, closes live connections and waits for the listener.
    pub async fn shutdown(self) {
        let Self {
            shutdown,
            task,
            watcher,
            addr,
            guards,
            ..
        } = self;
        drop(guards);
        let _ = shutdown.send(true);
        if let Err(error) = watcher.await {
            tracing::error!(%error, "remote revocation watcher failed");
        }
        if let Err(error) = task.await {
            tracing::error!(%error, "remote session ingress task failed");
        } else {
            tracing::info!(addr = %addr, "remote session ingress stopped");
        }
    }
}

/// Compose and start the remote ingress with the production driver over
/// `model`.
///
/// # Errors
/// A readable message when a directory, an adapter, the driver, the host or
/// the transport cannot be set up.
pub async fn start_remote(
    config: &RemoteServeConfig,
    model: Arc<dyn ModelProvider>,
    local: LocalNode,
    verifier: Arc<dyn NodeVerifier>,
    listener: TcpListener,
) -> Result<RemoteIngress, String> {
    let share = open_share(config)?;
    let sessions_root = transcript_root(config, share.as_ref())?;
    let factory = GatewayCoreFactory::new(
        model,
        &sessions_root,
        config.home.clone(),
        config.cwd.clone(),
        0,
    )
    .with_tier(config.tier)
    .with_actor(config.actor());
    let factory = match &share {
        // The TUI's thread and store, and the session's own project folder.
        Some(shared) => factory
            .with_thread_mapper(
                crate::session_serve::cli_session_thread,
                &shared.sessions_root,
            )
            .with_session_cwd(workdir_resolver(shared.index.clone())),
        None => factory,
    };
    let driver =
        CoreTurnDriver::with_factory(CoreDriverConfig::new(sessions_root), Arc::new(factory))
            .and_then(|driver| driver.with_media_root(&config.home.join("media")))
            .map_err(|error| format!("remote session driver: {error}"))?;
    start_remote_composed(config, share, Arc::new(driver), local, verifier, listener).await
}

/// Like [`start_remote`] around an injected `driver` (tests use a fake one).
///
/// # Errors
/// See [`start_remote`].
pub async fn start_remote_with_driver(
    config: &RemoteServeConfig,
    driver: Arc<dyn TurnDriver>,
    local: LocalNode,
    verifier: Arc<dyn NodeVerifier>,
    listener: TcpListener,
) -> Result<RemoteIngress, String> {
    let share = open_share(config)?;
    start_remote_composed(config, share, driver, local, verifier, listener).await
}

/// Transcript and approval root: the profile store when sharing, otherwise
/// the remote ingress's private `<remote>/sessions` (created owner-only).
fn transcript_root(
    config: &RemoteServeConfig,
    share: Option<&SharedProfile>,
) -> Result<PathBuf, String> {
    match share {
        Some(shared) => Ok(shared.sessions_root.clone()),
        None => {
            let root = config.remote_dir().join(SESSIONS_DIR_NAME);
            private_dir(&root)?;
            Ok(root)
        }
    }
}

async fn start_remote_composed(
    config: &RemoteServeConfig,
    share: Option<SharedProfile>,
    driver: Arc<dyn TurnDriver>,
    local: LocalNode,
    verifier: Arc<dyn NodeVerifier>,
    listener: TcpListener,
) -> Result<RemoteIngress, String> {
    let remote_dir = config.remote_dir();
    let sessions_root = transcript_root(config, share.as_ref())?;
    // Host records stay in the remote directory in both modes.
    let host_dir = remote_dir.join("host");
    private_dir(&host_dir)?;
    let transcripts = DurableTranscripts::open(&sessions_root)
        .map_err(|error| format!("remote transcripts: {error}"))?;
    // Only the host's own records: profile sessions may have live writers.
    let persisted_ids: Vec<_> = harw_session_host::record::RecordStore::new(&host_dir)
        .list()
        .map_err(|error| format!("remote hosted session records: {error}"))?
        .into_iter()
        .map(|record| record.session_id)
        .collect();
    transcripts
        .repair_tails(&persisted_ids)
        .map_err(|error| format!("remote transcript startup recovery: {error}"))?;
    let approvals = DurableApprovals::open(&sessions_root)
        .map_err(|error| format!("remote approvals: {error}"))?;
    let services = DaemonServices::new(driver, Arc::new(transcripts), Arc::new(approvals));
    let manage = share.is_some();
    let host = match share {
        None => SessionHost::open(
            HostConfig::new(host_dir),
            services.driver,
            services.transcripts,
            services.approvals,
        ),
        Some(shared) => SessionHost::open_with_profile(
            HostConfig::new(host_dir),
            services.driver,
            services.transcripts,
            services.approvals,
            ToolHost::disabled(),
            ProfileShare {
                settings: shared.settings,
                index: shared.index,
            },
        ),
    };
    let host = Arc::new(host.map_err(|error| format!("remote host: {error}"))?);
    // Management ops only with a shared profile (contract K-W3c WR-7).
    if manage {
        host.install_ops(Arc::new(crate::session_ops::HostedOps::open(config.home.clone())?))
            .map_err(|error| format!("remote ops: {error}"))?;
        // Global jobs and agents views over the profile's durable job store and
        // agent library (feature `overview`); tenants are checked by the host.
        let profile = harw_home::active_profile_name(&config.home);
        let profile_dir = harw_home::profile_dir(&config.home, &profile)
            .map_err(|error| format!("remote overview: profile directory: {error}"))?;
        let layers = vec![config.home.clone(), profile_dir.clone()];
        let library: harw_session_host::overview::AgentLibrary =
            Arc::new(move || harw_local_host::agent_library(&layers));
        host.install_overview(Arc::new(harw_session_host::overview::Overview::new(
            Some(Arc::new(harw_session_store::JobStore::new(&profile_dir))),
            Some(library),
        )))
        .map_err(|error| format!("remote overview: {error}"))?;
    }
    let addr = listener
        .local_addr()
        .map_err(|error| format!("remote listener address: {error}"))?;
    let server = NodeTransportServer::new(local, verifier, ServerOptions::default())
        .map_err(|error| format!("node transport: {error}"))?;
    let pins = Arc::new(PeerPolicyGate::open(config)?);
    // Validate bounded owner policy before accepting authenticated connections.
    DeviceRegistry::new(&remote_dir)
        .scan_bounded(POLICY_MAX_BYTES)
        .map_err(|error| format!("remote device policy: {error}"))?;
    let mapper: Arc<dyn IdentityMapper> = Arc::new(PolicyIdentityMapper {
        mapper: RegistryIdentityMapper::new(&remote_dir),
        pins: Arc::clone(&pins),
    });
    let node_listener = NodeListener::new(server, Arc::clone(&host), mapper, WsLimits::default());
    let node_listener = match &config.provider_link_device {
        Some(owner) => node_listener.with_provider_link(Arc::new(AccountLink {
            owner: owner.clone(),
            home: config.home.clone(),
        })),
        None => node_listener,
    };
    let intake = node_listener.intake();
    let revoker = node_listener.revoker().with_registry(&remote_dir);
    let (shutdown, mut rx) = watch::channel(false);
    let watcher = tokio::spawn(watch_revocations(
        revoker,
        remote_dir.clone(),
        config.revocation_poll,
        shutdown.subscribe(),
        pins,
    ));
    let task = tokio::spawn(async move {
        let stop = async move {
            let _ = rx.wait_for(|stop| *stop).await;
        };
        if let Err(error) = node_listener.serve(listener, stop).await {
            tracing::warn!(%error, "remote session ingress ended with an error");
        }
    });
    Ok(RemoteIngress {
        host: host.as_ref().clone(),
        shutdown,
        task,
        watcher,
        addr,
        intake,
        guards: Vec::new(),
    })
}

/// Revalidate owner policy on a bounded poll. Policy edits cut old connection
/// authority, including detached creators; fresh authentication uses the new
/// registry. Explicit revocation remains permanent. Immutable pin changes latch
/// closed until restart, including for future mappings.
async fn watch_revocations(
    revoker: HostRevoker,
    registry_dir: PathBuf,
    every: Duration,
    mut stop: watch::Receiver<bool>,
    pins: Arc<PeerPolicyGate>,
) {
    let registry = DeviceRegistry::new(&registry_dir);
    let mut applied: HashSet<DeviceId> = HashSet::new();
    let mut ticker = tokio::time::interval(every.max(Duration::from_millis(10)));
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = stop.wait_for(|stop| *stop) => return,
        }
        let identities = revoker.authenticated_identities();
        let scan = registry.scan_bounded(POLICY_MAX_BYTES);
        // Extract explicit revocations even if another row is malformed.
        if let Ok(scan) = &scan {
            for record in scan.records.iter().filter(|record| record.revoked) {
                if applied.insert(record.device.clone())
                    && let Err(error) = revoker.revoke_device(&record.device)
                {
                    tracing::warn!(%error, "device revocation incomplete");
                }
            }
        }
        let records = match scan {
            Ok(scan) if scan.rejected.is_empty() && pins.current() => scan.records,
            _ => {
                for identity in &identities {
                    if let Some(device) = &identity.device {
                        revoker.invalidate_device_policy(device);
                    }
                }
                tracing::warn!(
                    "remote policy unavailable or changed pins; cached authority invalidated"
                );
                continue;
            }
        };
        let invalid = invalidated_devices(&records, &identities);
        for device in invalid {
            revoker.invalidate_device_policy(&device);
        }
    }
}

fn invalidated_devices(
    records: &[DeviceRecord],
    identities: &[harw_session_host::ClientIdentity],
) -> HashSet<DeviceId> {
    let mut invalid = HashSet::new();
    for identity in identities {
        let Some(device) = &identity.device else {
            continue;
        };
        let mut matching = records.iter().filter(|record| &record.device == device);
        let valid = matching.next().is_some_and(|record| {
            record.authorizes_identity(identity)
                && records
                    .iter()
                    .filter(|other| other.node_id == record.node_id)
                    .count()
                    == 1
        }) && matching.next().is_none();
        if !valid {
            invalid.insert(device.clone());
        }
    }
    invalid
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::signature::{KeyPair as _, ML_DSA_65_SIGNING, PqdsaKeyPair};
    use harw_node_listener::{DeviceRecord, DeviceRegistry};
    use harw_node_transport::{
        ML_DSA_65_SIGNATURE_LEN, NodeIdentity, NodeSigner, PinnedPeers, SignFuture,
        TranscriptWrappedSigner, TranscriptWrappedVerifier,
    };
    use harw_protocol::SessionPort;
    use harw_session_host::driver::{
        CancelSignal, DriverFuture, EventSink, Setting, TurnInput, TurnOutcome,
    };
    use harw_session_remote::{ConnectOptions, NodeEndpoint, RemotePort, connect_node};
    use harw_types::{NodeId, SessionId, TenantId};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    fn injected_config(path: &Path) -> RemoteServeConfig {
        let mut config = RemoteServeConfig::new(path, path, path);
        config.injected_peer_trust = true;
        config
    }

    struct SeedSigner(PqdsaKeyPair);

    impl NodeSigner for SeedSigner {
        fn sign<'a>(&'a self, transcript: &'a [u8]) -> SignFuture<'a> {
            let mut signature = vec![0u8; ML_DSA_65_SIGNATURE_LEN];
            let result = self
                .0
                .sign(transcript, &mut signature)
                .map(|n| {
                    signature.truncate(n);
                    signature
                })
                .map_err(|_| harw_node_transport::SignerError::new("test signer failed"));
            Box::pin(std::future::ready(result))
        }
    }

    struct TestNode {
        id: NodeId,
        identity: NodeIdentity,
        local: LocalNode,
    }

    fn node(name: &str, seed: u8) -> TestResult<TestNode> {
        let key = PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, &[seed; 32]).map_err(|_| "seed")?;
        let id = NodeId::try_from_str(name)?;
        let identity = NodeIdentity {
            node_id: id.clone(),
            public_key: key.public_key().as_ref().to_vec(),
            key_ref: format!("test-only/{name}"),
        };
        let local = LocalNode::new(identity.clone(), Arc::new(SeedSigner(key)));
        Ok(TestNode {
            id,
            identity,
            local,
        })
    }

    struct NoopDriver;

    impl TurnDriver for NoopDriver {
        // This test double performs no privileged runtime/tool effects.
        fn prepare_execution(
            &self,
            _: &harw_types::SessionId,
            _: harw_session_host::driver::ExecutionAdmission,
        ) -> harw_session_host::driver::DriverFuture<'_, ()> {
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

    fn pinned(identity: &NodeIdentity) -> TestResult<PinnedPeers> {
        let mut peers = PinnedPeers::new();
        peers.pin_identity(identity)?;
        Ok(peers)
    }

    async fn dial(
        addr: SocketAddr,
        gateway: &TestNode,
        phone: &TestNode,
    ) -> Result<RemotePort, harw_session_remote::RemoteError> {
        let verifier = pinned(&gateway.identity)
            .map_err(|error| harw_session_remote::RemoteError::Connect(error.to_string()))?;
        let connection = connect_node(
            NodeEndpoint {
                addr,
                expected_node: gateway.id.clone(),
            },
            &phone.local,
            &verifier,
            ConnectOptions::new("phone"),
        )
        .await?;
        Ok(RemotePort::new(connection))
    }

    #[test]
    fn defaults_are_conservative_and_the_actor_is_a_placeholder_without_an_approver() -> TestResult
    {
        let config = RemoteServeConfig::new(Path::new("/s"), Path::new("/h"), Path::new("/w"));
        assert_eq!(config.tier, PermissionTier::Observer);
        assert_eq!(
            config.actor(),
            ApprovalActor::Operator {
                id: NO_APPROVER.to_owned()
            }
        );
        let mut with = config.clone();
        with.approval_device = Some(DeviceId::try_from_str("dev-1")?);
        assert_eq!(
            with.actor(),
            ApprovalActor::Operator {
                id: "device:dev-1".to_owned()
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn remote_start_repairs_torn_transcript_for_its_host_record() -> TestResult {
        use harw_session_host::record::{HostedSessionRecord, RecordStore};
        use harw_session_store::{RecordKind, TranscriptRecord, TranscriptStore};
        use harw_types::ThreadRef;
        use std::io::Write as _;

        let temp = tempfile::tempdir()?;
        let config = injected_config(temp.path());
        let remote_dir = config.remote_dir();
        let id = SessionId::try_from_str("repair-remote")?;
        let now = jiff::Timestamp::now();
        RecordStore::new(&remote_dir.join("host")).save(&HostedSessionRecord {
            session_id: id.clone(),
            tenant: None,
            owner_principal: "operator".to_owned(),
            creator_execution: None,
            workspace: None,
            title: None,
            created_at: now,
            updated_at: now,
            state: harw_protocol::HostedState::Idle,
            host_epoch: 0,
            generation: 0,
            owner_agent: None,
            tool_agents: Vec::new(),
        })?;
        let sessions = remote_dir.join("sessions");
        std::fs::create_dir_all(&sessions)?;
        let store = TranscriptStore::new(&sessions);
        store.append(&TranscriptRecord::new(
            id.clone(),
            ThreadRef::from_str("root"),
            0,
            now,
            RecordKind::Item,
            serde_json::json!({"type":"user_message","id":"i","content":[]}),
        ))?;
        let path = store.transcript_path(&id)?;
        let valid = std::fs::read(&path)?;
        let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
        file.write_all(b"{\"torn\"")?;
        drop(file);
        let gateway = node("gateway-repair", 1)?;
        let phone = node("phone-repair", 2)?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let ingress = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local,
            Arc::new(pinned(&phone.identity)?),
            listener,
        )
        .await?;
        assert_eq!(std::fs::read(&path)?, valid);
        assert!(
            std::fs::read_dir(&sessions)?.any(|entry| entry.ok().is_some_and(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("repair-remote.jsonl.corrupt-")))
        );
        ingress.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn phone_and_unix_client_share_sessions_in_the_selected_tenant() -> TestResult {
        use crate::session_serve::{SessionServeConfig, start_shared};
        use harw_protocol::session_wire::CreateParams;
        use harw_session_remote::connect_unix;
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        let config = injected_config(dir);
        let gateway = node("gateway", 1)?;
        let phone = node("phone", 2)?;
        let tenant = TenantId::try_from_str("shared")?;
        std::fs::create_dir_all(config.remote_dir())?;
        DeviceRegistry::new(&config.remote_dir()).enroll(&DeviceRecord {
            node_id: phone.id.clone(),
            device: DeviceId::try_from_str("phone")?,
            tenant: tenant.clone(),
            tier: PermissionTier::Operator,
            revoked: false,
            label: "phone".into(),
            approve_optin: false,
        })?;
        let ingress = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local.clone(),
            Arc::new(pinned(&phone.identity)?),
            TcpListener::bind("127.0.0.1:0").await?,
        )
        .await?;
        let local_config = SessionServeConfig::for_profile(dir, dir, dir, None, None);
        let unix = start_shared(&local_config, ingress.host.clone(), tenant.clone()).await?;
        let desktop = RemotePort::new(
            connect_unix(unix.socket().to_owned(), ConnectOptions::new("desktop")).await?,
        );
        let mobile = dial(ingress.addr(), &gateway, &phone).await?;
        let session = desktop
            .create(CreateParams {
                title: Some("desktop".into()),
                workspace: None,
            })
            .await?;
        assert_eq!(session.tenant.as_ref(), Some(&tenant));
        assert!(
            mobile
                .list()
                .await?
                .iter()
                .any(|s| s.session_id == session.session_id)
        );
        let second = mobile
            .create(CreateParams {
                title: Some("phone".into()),
                workspace: None,
            })
            .await?;
        assert!(
            desktop
                .list()
                .await?
                .iter()
                .any(|s| s.session_id == second.session_id)
        );
        assert_eq!(ingress.host.listener_enabled("local"), Some(true));
        assert_eq!(ingress.host.listener_enabled("node"), Some(true));
        // Disabling node ingress stops new handshakes. The local recovery path remains available.
        ingress
            .host
            .register_listener(harw_protocol::session_wire::GatewayListenerInfo {
                name: "node".into(),
                kind: harw_protocol::session_wire::ListenerKind::Node,
                address: ingress.addr().to_string(),
                enabled: false,
            });
        assert!(dial(ingress.addr(), &gateway, &phone).await.is_err());
        let local_reconnect =
            connect_unix(unix.socket().to_owned(), ConnectOptions::new("desktop")).await?;
        drop(local_reconnect);
        drop((desktop, mobile));
        unix.shutdown().await;
        ingress.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn only_designated_enrolled_device_can_link_the_account() -> TestResult {
        use harw_node_transport::{NodeTransportClient, full_body};
        let temp = tempfile::tempdir()?;
        let mut config = injected_config(temp.path());
        let gateway = node("gateway", 1)?;
        let owner = node("owner-phone", 2)?;
        let guest = node("other-phone", 3)?;
        let mut peers = pinned(&owner.identity)?;
        peers.pin_identity(&guest.identity)?;
        config.provider_link_device = Some(DeviceId::try_from_str("owner-phone")?);
        let remote_dir = config.remote_dir();
        std::fs::create_dir_all(&remote_dir)?;
        for phone in [&owner, &guest] {
            DeviceRegistry::new(&remote_dir).enroll(&DeviceRecord {
                node_id: phone.id.clone(),
                device: DeviceId::try_from_str(phone.id.as_str())?,
                tenant: TenantId::try_from_str("tenant")?,
                tier: PermissionTier::Operator,
                revoked: false,
                label: "phone".into(),
                approve_optin: false,
            })?;
        }
        let ingress = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local.clone(),
            Arc::new(peers),
            TcpListener::bind("127.0.0.1:0").await?,
        )
        .await?;
        for (phone, expected) in [
            (&guest, http::StatusCode::FORBIDDEN),
            (&owner, http::StatusCode::BAD_REQUEST),
        ] {
            let mut client = NodeTransportClient::connect(
                ingress.addr(),
                &phone.local,
                &gateway.id,
                &pinned(&gateway.identity)?,
            )
            .await?;
            let request = http::Request::post("/v1/provider-link/chatgpt")
                .header("host", "gateway")
                .body(full_body("invalid"))?;
            assert_eq!(client.send_request(request).await?.status(), expected);
        }
        ingress.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn an_enrolled_device_lists_sessions_over_the_node_transport_and_revocation_cuts_it()
    -> TestResult {
        let temp = tempfile::tempdir()?;
        let mut config = injected_config(temp.path());
        config.revocation_poll = Duration::from_millis(50);
        let gateway = node("gateway", 1)?;
        let phone = node("phone", 2)?;
        let device = DeviceId::try_from_str("dev-1")?;
        // The registry lives in the remote directory; enrol the phone.
        let remote_dir = config.remote_dir();
        std::fs::create_dir_all(&remote_dir)?;
        DeviceRegistry::new(&remote_dir).enroll(&DeviceRecord {
            node_id: phone.id.clone(),
            device: device.clone(),
            tenant: TenantId::try_from_str("tenant-a")?,
            tier: PermissionTier::Operator,
            revoked: false,
            label: "phone".to_owned(),
            approve_optin: false,
        })?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let ingress = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local.clone(),
            Arc::new(pinned(&phone.identity)?),
            listener,
        )
        .await?;

        let port = dial(ingress.addr(), &gateway, &phone).await?;
        assert!(port.list().await?.is_empty());

        // Live revocation by the operator's file edit: the open connection is
        // cut and the next handshake is refused.
        DeviceRegistry::new(&remote_dir).mark_revoked(&device)?;
        let mut cut = false;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if port.list().await.is_err() {
                cut = true;
                break;
            }
        }
        assert!(cut, "the open connection of a revoked device is closed");
        drop(port);
        assert!(
            dial(ingress.addr(), &gateway, &phone).await.is_err(),
            "a revoked device must not connect again"
        );
        ingress.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn live_policy_edits_cut_old_authority_and_valid_registry_recovery_reconnects()
    -> TestResult {
        for change in [
            "tier",
            "tenant",
            "approval",
            "removed",
            "ambiguous-device",
            "ambiguous-node",
            "unreadable",
            "malformed",
            "node",
        ] {
            let temp = tempfile::tempdir()?;
            let mut config = injected_config(temp.path());
            config.revocation_poll = Duration::from_millis(10);
            let gateway = node("gateway-policy", 1)?;
            let phone = node("phone-policy", 2)?;
            let remote_dir = config.remote_dir();
            std::fs::create_dir_all(&remote_dir)?;
            let registry = DeviceRegistry::new(&remote_dir);
            let original = DeviceRecord {
                node_id: phone.id.clone(),
                device: DeviceId::try_from_str("dev-policy")?,
                tenant: TenantId::try_from_str("tenant-a")?,
                tier: PermissionTier::Owner,
                revoked: false,
                label: "phone".into(),
                approve_optin: true,
            };
            registry.enroll(&original)?;
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let ingress = start_remote_with_driver(
                &config,
                Arc::new(NoopDriver),
                gateway.local.clone(),
                Arc::new(pinned(&phone.identity)?),
                listener,
            )
            .await?;
            let port = dial(ingress.addr(), &gateway, &phone).await?;
            assert!(port.list().await?.is_empty());
            let path = remote_dir.join("node-devices.conf");
            let mut changed = original.clone();
            match change {
                "tier" => changed.tier = PermissionTier::Observer,
                "tenant" => changed.tenant = TenantId::try_from_str("tenant-b")?,
                "approval" => changed.approve_optin = false,
                "node" => changed.node_id = NodeId::try_from_str("replacement-node")?,
                _ => {}
            }
            match change {
                "removed" => std::fs::remove_file(&path)?,
                "ambiguous-device" => {
                    changed.node_id = NodeId::try_from_str("another-node")?;
                    registry.enroll(&changed)?;
                }
                "ambiguous-node" => {
                    changed.device = DeviceId::try_from_str("another-device")?;
                    registry.enroll(&changed)?;
                }
                "unreadable" => {
                    std::fs::remove_file(&path)?;
                    std::fs::create_dir(&path)?;
                }
                "malformed" => {
                    std::fs::write(&path, format!("{}\nmalformed-row\n", original.render()))?
                }
                _ => std::fs::write(&path, format!("{}\n", changed.render()))?,
            }
            let cut = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if port.list().await.is_err() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await;
            assert!(cut.is_ok(), "stale authority survived {change}");
            drop(port);
            // Errors/removal are temporary policy invalidation, never a device ban.
            if path.is_dir() {
                std::fs::remove_dir(&path)?;
            }
            let mut recovered = original.clone();
            recovered.tier = PermissionTier::Observer;
            recovered.approve_optin = false;
            std::fs::write(&path, format!("{}\n", recovered.render()))?;
            let fresh = dial(ingress.addr(), &gateway, &phone).await?;
            assert!(
                fresh.list().await?.is_empty(),
                "fresh recovery failed after {change}"
            );
            ingress.shutdown().await;
        }
        Ok(())
    }

    #[tokio::test]
    async fn pin_removal_cuts_live_authority_and_rejects_reconnect_until_restart() -> TestResult {
        let temp = tempfile::tempdir()?;
        let mut config = RemoteServeConfig::new(temp.path(), temp.path(), temp.path());
        config.revocation_poll = Duration::from_millis(10);
        let remote_dir = config.remote_dir();
        std::fs::create_dir_all(&remote_dir)?;
        let gateway = node("gateway-pins", 1)?;
        let phone = node("phone-pins", 2)?;
        DeviceRegistry::new(&remote_dir).enroll(&DeviceRecord {
            node_id: phone.id.clone(),
            device: DeviceId::try_from_str("dev-pins")?,
            tenant: TenantId::try_from_str("tenant-a")?,
            tier: PermissionTier::Observer,
            revoked: false,
            label: "phone".into(),
            approve_optin: false,
        })?;
        let key: String = phone
            .identity
            .public_key
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let text = format!("{}|{key}\n", phone.id.as_str());
        let path = remote_dir.join(crate::session_listener::PEERS_FILE);
        std::fs::write(&path, &text)?;
        config.peer_pins_digest = Some(*blake3::hash(text.as_bytes()).as_bytes());
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let ingress = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local.clone(),
            Arc::new(pinned(&phone.identity)?),
            listener,
        )
        .await?;
        let port = dial(ingress.addr(), &gateway, &phone).await?;
        assert!(port.list().await?.is_empty());
        std::fs::remove_file(&path)?;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if port.list().await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        std::fs::write(&path, &text)?;
        assert!(dial(ingress.addr(), &gateway, &phone).await.is_err());
        ingress.shutdown().await;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let restarted = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local.clone(),
            Arc::new(pinned(&phone.identity)?),
            listener,
        )
        .await?;
        assert!(
            dial(restarted.addr(), &gateway, &phone)
                .await?
                .list()
                .await?
                .is_empty()
        );
        restarted.shutdown().await;
        Ok(())
    }

    #[test]
    fn owner_pin_gate_requires_exact_bounded_unambiguous_file_and_latches_until_restart()
    -> TestResult {
        let temp = tempfile::tempdir()?;
        let mut config = RemoteServeConfig::new(temp.path(), temp.path(), temp.path());
        std::fs::create_dir_all(config.remote_dir())?;
        let path = config
            .remote_dir()
            .join(crate::session_listener::PEERS_FILE);
        assert!(
            PeerPolicyGate::open(&config).is_err(),
            "missing pins must fail closed"
        );
        let phone = node("phone-pin", 2)?;
        let key: String = phone
            .identity
            .public_key
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let text = format!("{}|{key}\n", phone.id.as_str());
        std::fs::write(&path, &text)?;
        assert!(
            PeerPolicyGate::open(&config).is_err(),
            "unbound verifier must fail closed"
        );
        config.peer_pins_digest = Some(*blake3::hash(text.as_bytes()).as_bytes());
        let gate = PeerPolicyGate::open(&config)?;
        assert!(gate.current());
        let concurrent = PeerPolicyGate::open(&config)?;
        assert!(!concurrent.current_with(|| {
            // Another lookup detects a change while this lookup is reading.
            concurrent.stale.store(true, Ordering::Release);
            Ok(text.clone())
        }));
        std::fs::write(&path, format!("{text}{text}"))?;
        assert!(
            read_peer_pins(&path).is_err(),
            "duplicate node pins are ambiguous"
        );
        assert!(!gate.current());
        std::fs::write(&path, &text)?;
        assert!(!gate.current(), "restoring static trust requires a restart");
        let restarted = PeerPolicyGate::open(&config)?;
        assert!(restarted.current());
        let replacement = node("phone-pin", 3)?;
        let key: String = replacement
            .identity
            .public_key
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        std::fs::write(&path, format!("{}|{key}\n", phone.id.as_str()))?;
        assert!(
            !restarted.current(),
            "changed key cannot keep static verifier authority"
        );
        std::fs::write(&path, vec![b'x'; POLICY_MAX_BYTES + 1])?;
        assert!(read_peer_pins(&path).is_err());
        std::fs::remove_file(&path)?;
        std::fs::create_dir(&path)?;
        assert!(read_peer_pins(&path).is_err());
        Ok(())
    }

    #[test]
    fn creator_key_fingerprint_survives_unrelated_pin_addition_but_changes_on_rotation()
    -> TestResult {
        let phone = node("phone-history", 2)?;
        let other = node("other-history", 3)?;
        let replacement = node("phone-history", 4)?;
        let row = |identity: &NodeIdentity| {
            let key: String = identity
                .public_key
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("{}|{key}\n", identity.node_id.as_str())
        };
        let original = crate::session_listener::peer_key_fingerprints(&row(&phone.identity));
        let unrelated = crate::session_listener::peer_key_fingerprints(&format!(
            "{}{}",
            row(&phone.identity),
            row(&other.identity)
        ));
        let rotated = crate::session_listener::peer_key_fingerprints(&row(&replacement.identity));
        assert_eq!(
            original.get(phone.id.as_str()),
            unrelated.get(phone.id.as_str())
        );
        assert_ne!(
            original.get(phone.id.as_str()),
            rotated.get(phone.id.as_str())
        );
        assert_eq!(
            original.get(phone.id.as_str()),
            Some(blake3::hash(&phone.identity.public_key).as_bytes())
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn owner_pins_refuse_symlinks() -> TestResult {
        let temp = tempfile::tempdir()?;
        let target = temp.path().join("target");
        std::fs::write(&target, "")?;
        let link = temp.path().join("pins");
        std::os::unix::fs::symlink(&target, &link)?;
        assert!(read_peer_pins(&link).is_err());
        Ok(())
    }

    /// A node whose key signs wrapped transcripts (what `AuthHubNodeSigner`
    /// produces in production).
    fn wrapped_node(name: &str, seed: u8) -> TestResult<TestNode> {
        let key = PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, &[seed; 32]).map_err(|_| "seed")?;
        let id = NodeId::try_from_str(name)?;
        let identity = NodeIdentity {
            node_id: id.clone(),
            public_key: key.public_key().as_ref().to_vec(),
            key_ref: format!("test-only/{name}"),
        };
        let local = LocalNode::new(
            identity.clone(),
            Arc::new(TranscriptWrappedSigner::new(SeedSigner(key))),
        );
        Ok(TestNode {
            id,
            identity,
            local,
        })
    }

    #[tokio::test]
    async fn a_wrapped_fleet_connects_and_a_plain_peer_is_refused() -> TestResult {
        let temp = tempfile::tempdir()?;
        let config = injected_config(temp.path());
        let gateway = wrapped_node("gateway", 1)?;
        let phone = wrapped_node("phone", 2)?;
        let plain = node("plain", 3)?;
        let remote_dir = config.remote_dir();
        std::fs::create_dir_all(&remote_dir)?;
        let registry = DeviceRegistry::new(&remote_dir);
        for (peer, device) in [(&phone, "dev-1"), (&plain, "dev-2")] {
            registry.enroll(&DeviceRecord {
                node_id: peer.id.clone(),
                device: DeviceId::try_from_str(device)?,
                tenant: TenantId::try_from_str("tenant-a")?,
                tier: PermissionTier::Operator,
                revoked: false,
                label: device.to_owned(),
                approve_optin: false,
            })?;
        }
        // The server pins both and verifies wrapped transcripts (fleet-wide).
        let mut peers = PinnedPeers::new();
        peers.pin_identity(&phone.identity)?;
        peers.pin_identity(&plain.identity)?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let ingress = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local.clone(),
            Arc::new(TranscriptWrappedVerifier::new(peers)),
            listener,
        )
        .await?;
        let client_verifier = TranscriptWrappedVerifier::new(pinned(&gateway.identity)?);
        let connect = |peer: &TestNode| {
            let endpoint = NodeEndpoint {
                addr: ingress.addr(),
                expected_node: gateway.id.clone(),
            };
            let local = peer.local.clone();
            let verifier = client_verifier.clone();
            async move {
                connect_node(endpoint, &local, &verifier, ConnectOptions::new("t"))
                    .await
                    .map(RemotePort::new)
            }
        };
        assert!(connect(&phone).await?.list().await?.is_empty());
        // Exercise the attach connector with the pinned identity and a wrong alias pin.
        let endpoint = ingress.addr().to_string();
        let attached = crate::attach_cmd::connect_authenticated_node(
            "lab",
            &endpoint,
            gateway.id.as_str(),
            &phone.local,
            &client_verifier,
            ConnectOptions::new("attach test"),
        )
        .await?;
        assert!(RemotePort::new(attached).list().await?.is_empty());
        assert!(
            crate::attach_cmd::connect_authenticated_node(
                "lab",
                &endpoint,
                "different-gateway",
                &phone.local,
                &client_verifier,
                ConnectOptions::new("wrong identity"),
            )
            .await
            .is_err()
        );
        // A key that signs the raw (unwrapped) transcript is refused in a
        // wrapped fleet, although it is pinned and enrolled.
        assert!(connect(&plain).await.is_err());
        ingress.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn an_unenrolled_node_is_refused() -> TestResult {
        let temp = tempfile::tempdir()?;
        let config = injected_config(temp.path());
        let gateway = node("gateway", 1)?;
        let stranger = node("stranger", 3)?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let ingress = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local.clone(),
            Arc::new(pinned(&stranger.identity)?),
            listener,
        )
        .await?;
        // Pinned by the transport but not enrolled in the device registry.
        assert!(dial(ingress.addr(), &gateway, &stranger).await.is_err());
        ingress.shutdown().await;
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Sharing the profile's sessions (plan Welle 1, acceptance S4)
    // ---------------------------------------------------------------------

    /// The Tui entry requires an active UIA: the same fixture the runtime
    /// tests use (`<home>/profiles/default`).
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

    /// Shared-profile mode `continue` over `dir` (home = state = cwd).
    fn share_config(dir: &Path, tenant: &TenantId) -> RemoteServeConfig {
        let mut config = injected_config(dir);
        config.tier = PermissionTier::Operator;
        config.share = SharePlan {
            mode: ShareMode::Continue,
            tenant: Some(tenant.clone()),
            ..SharePlan::off()
        };
        config
    }

    fn enroll(config: &RemoteServeConfig, phone: &TestNode, tenant: &TenantId) -> TestResult {
        std::fs::create_dir_all(config.remote_dir())?;
        DeviceRegistry::new(&config.remote_dir()).enroll(&DeviceRecord {
            node_id: phone.id.clone(),
            device: DeviceId::try_from_str("dev-test-1")?,
            tenant: tenant.clone(),
            tier: PermissionTier::Operator,
            revoked: false,
            label: "phone".into(),
            approve_optin: false,
        })?;
        Ok(())
    }

    /// Like [`dial`], asking for the `projects` feature (a minor-3 app).
    async fn dial_projects(
        addr: SocketAddr,
        gateway: &TestNode,
        phone: &TestNode,
    ) -> Result<RemotePort, harw_session_remote::RemoteError> {
        let verifier = pinned(&gateway.identity)
            .map_err(|error| harw_session_remote::RemoteError::Connect(error.to_string()))?;
        let mut options = ConnectOptions::new("phone");
        options.features = vec![harw_protocol::session_wire::features::PROJECTS.to_owned()];
        let connection = connect_node(
            NodeEndpoint {
                addr,
                expected_node: gateway.id.clone(),
            },
            &phone.local,
            &verifier,
            options,
        )
        .await?;
        Ok(RemotePort::new(connection))
    }

    /// A canonical, existing project folder below `dir`.
    fn project_dir(dir: &Path, name: &str) -> TestResult<PathBuf> {
        let project = dir.join(name);
        std::fs::create_dir_all(&project)?;
        Ok(std::fs::canonicalize(project)?)
    }

    fn text_item(user: bool, text: &str) -> harw_protocol::TurnItem {
        use harw_protocol::{AssistantMessageItem, ContentPart, TurnItem, UserMessageItem};
        let content = vec![ContentPart::Text {
            text: text.to_owned(),
        }];
        if user {
            TurnItem::UserMessage(UserMessageItem {
                id: harw_types::ItemId::new(),
                content,
            })
        } else {
            TurnItem::AssistantMessage(AssistantMessageItem {
                id: harw_types::ItemId::new(),
                content,
                phase: None,
            })
        }
    }

    /// A TUI session in the profile store, written as `harw` writes it
    /// (`cli-session:` thread), with its meta bound to `project`. Returns
    /// the project key.
    async fn seed_tui_session(
        root: &Path,
        id: &SessionId,
        project: &Path,
        text: &str,
    ) -> TestResult<String> {
        use harw_core::{StateStore as _, TranscriptStateStore};
        let store = TranscriptStateStore::new(
            harw_session_store::TranscriptStore::new(root),
            crate::session_serve::cli_session_thread,
        );
        store.save_turn(id, &text_item(true, text)).await?;
        store
            .save_turn(id, &text_item(false, "earlier answer"))
            .await?;
        let key = harw_home::project::project_key(project);
        let bound = key.clone();
        harw_session_store::meta::update(root, id, |meta| {
            meta.turns = meta.turns.max(1);
            if meta.first_user_message.is_none() {
                meta.first_user_message = Some(text.to_owned());
            }
            meta.cwd = Some(project.to_path_buf());
            meta.project_root = Some(project.to_path_buf());
            meta.project_key = Some(bound);
        })?;
        Ok(key)
    }

    /// Fake driver that, like the core, writes the first transcript record
    /// of a new session into the profile store (`cli-session:` thread).
    struct ProfileWritingDriver {
        root: PathBuf,
    }

    impl TurnDriver for ProfileWritingDriver {
        fn prepare_execution(
            &self,
            _: &SessionId,
            _: harw_session_host::driver::ExecutionAdmission,
        ) -> DriverFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }

        fn create_session(&self, id: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
            let id = id.clone();
            Box::pin(async move {
                use harw_core::{StateStore as _, TranscriptStateStore};
                TranscriptStateStore::new(
                    harw_session_store::TranscriptStore::new(&self.root),
                    crate::session_serve::cli_session_thread,
                )
                .save_turn(&id, &text_item(true, "first words from the phone"))
                .await
                .map_err(|error| harw_session_host::HostError::Storage(error.to_string()))
            })
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

    /// Acceptance S4: a TUI session continued over the remote host hydrates
    /// from the profile store with the TUI's thread, so the model sees the
    /// earlier entries; nothing lands in the private remote store.
    #[tokio::test]
    async fn tui_session_continued_over_remote_host_sees_earlier_entries() -> TestResult {
        use harw_protocol::session_wire::{AttachParams, SubmitParams, SubmitResult};
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        write_fixture_uia(dir)?;
        let tenant = TenantId::try_from_str("tenant-test")?;
        let config = share_config(dir, &tenant);
        let gateway = node("gateway-share", 1)?;
        let phone = node("phone-share", 2)?;
        enroll(&config, &phone, &tenant)?;
        let root = crate::runtime_entry::profile_sessions_root(dir)?;
        let project = project_dir(dir, "proj")?;
        let id = SessionId::try_from_str("sess-a1")?;
        seed_tui_session(&root, &id, &project, "earlier-entry-marker").await?;

        let model = harw_core::RecordingModelProvider::new();
        let ingress = start_remote(
            &config,
            Arc::new(model.clone()),
            gateway.local.clone(),
            Arc::new(pinned(&phone.identity)?),
            TcpListener::bind("127.0.0.1:0").await?,
        )
        .await?;
        let mobile = dial_projects(ingress.addr(), &gateway, &phone).await?;
        let (ack, frames) = mobile
            .attach(AttachParams {
                session_id: id.clone(),
                from: None,
                profile: Default::default(),
                tail_items: 50,
            })
            .await?;
        assert!(ack.head.durable > 0, "the TUI transcript is readable");
        let submitted = mobile
            .submit(SubmitParams {
                session_id: id.clone(),
                text: "continue-from-phone".to_owned(),
                expect_head: ack.head,
                client_msg_id: "m1".to_owned(),
                force: false,
                images: Vec::new(),
                rewind_before: None,
            })
            .await?;
        assert!(
            matches!(submitted, SubmitResult::Accepted { .. }),
            "{submitted:?}"
        );
        let mut request = None;
        for _ in 0..100 {
            request = model.last();
            if request.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let request = request.ok_or("the model was never called")?;
        let history = format!("{:?}", request.history.items());
        assert!(
            history.contains("earlier-entry-marker"),
            "the model must see the TUI's earlier entries"
        );
        assert!(history.contains("continue-from-phone"));
        assert!(
            !config
                .remote_dir()
                .join(SESSIONS_DIR_NAME)
                .join("sess-a1.jsonl")
                .exists(),
            "a shared session never moves into the private remote store"
        );
        drop((mobile, frames));
        ingress.shutdown().await;
        Ok(())
    }

    /// Acceptance S4: a session the phone creates in a project is a normal
    /// profile session: `harw -r` discovers it, with its project.
    #[tokio::test]
    async fn phone_session_is_discoverable_with_project() -> TestResult {
        use harw_protocol::session_wire::CreateInParams;
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        let tenant = TenantId::try_from_str("tenant-test")?;
        let config = share_config(dir, &tenant);
        let gateway = node("gateway-create", 1)?;
        let phone = node("phone-create", 2)?;
        enroll(&config, &phone, &tenant)?;
        let root = crate::runtime_entry::profile_sessions_root(dir)?;
        let project = project_dir(dir, "proj")?;
        // A known project: one TUI session already ran there.
        let key = seed_tui_session(
            &root,
            &SessionId::try_from_str("sess-a1")?,
            &project,
            "tui work",
        )
        .await?;

        let ingress = start_remote_with_driver(
            &config,
            Arc::new(ProfileWritingDriver { root: root.clone() }),
            gateway.local.clone(),
            Arc::new(pinned(&phone.identity)?),
            TcpListener::bind("127.0.0.1:0").await?,
        )
        .await?;
        let mobile = dial_projects(ingress.addr(), &gateway, &phone).await?;
        let created = mobile
            .create_in(CreateInParams {
                project_key: key.clone(),
                title: Some("from the phone".to_owned()),
            })
            .await?;
        assert_eq!(created.tenant.as_ref(), Some(&tenant));

        let discovered = crate::resume::discover_sessions(&root)?;
        let found = discovered
            .iter()
            .find(|session| session.id == created.session_id)
            .ok_or("the phone session is not discoverable from the TUI")?;
        let meta = found.meta.as_ref().ok_or("the phone session has no meta")?;
        assert_eq!(meta.project_key.as_deref(), Some(key.as_str()));
        assert_eq!(meta.project_root.as_deref(), Some(project.as_path()));
        drop(mobile);
        ingress.shutdown().await;
        Ok(())
    }

    /// Acceptance S4: the factory's per-session working directory is the
    /// session's checked project root; unknown sessions fall back (`None`).
    #[tokio::test]
    async fn session_runs_in_project_dir() -> TestResult {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("sessions");
        std::fs::create_dir_all(&root)?;
        let project = project_dir(temp.path(), "proj")?;
        let id = SessionId::try_from_str("sess-a1")?;
        seed_tui_session(&root, &id, &project, "work here").await?;
        let index = Arc::new(FsProfileIndex::open(root, IndexOptions::default()));
        let resolve = workdir_resolver(index);
        assert_eq!(resolve(&id), Some(project));
        assert_eq!(resolve(&SessionId::try_from_str("sess-unknown")?), None);
        Ok(())
    }

    /// Regression guard: with sharing off the remote host keeps its private
    /// store and never touches the profile's sessions directory.
    #[tokio::test]
    async fn share_off_keeps_remote_store() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        let config = injected_config(dir);
        assert!(matches!(config.share.mode, ShareMode::Off));
        assert!(open_share(&config)?.is_none());
        assert_eq!(
            transcript_root(&config, None)?,
            config.remote_dir().join(SESSIONS_DIR_NAME)
        );
        let gateway = node("gateway-off", 1)?;
        let phone = node("phone-off", 2)?;
        let ingress = start_remote_with_driver(
            &config,
            Arc::new(NoopDriver),
            gateway.local.clone(),
            Arc::new(pinned(&phone.identity)?),
            TcpListener::bind("127.0.0.1:0").await?,
        )
        .await?;
        assert!(config.remote_dir().join(SESSIONS_DIR_NAME).is_dir());
        assert!(
            !dir.join("profiles")
                .join("default")
                .join("sessions")
                .exists(),
            "share off must not create or use the profile store"
        );
        ingress.shutdown().await;
        Ok(())
    }

    /// Sharing without a tenant is a startup error, never a panic.
    #[test]
    fn share_without_tenant_is_an_error() -> TestResult {
        let temp = tempfile::tempdir()?;
        let mut config = injected_config(temp.path());
        config.share = SharePlan {
            mode: ShareMode::Read,
            tenant: None,
            ..SharePlan::off()
        };
        assert!(open_share(&config).is_err());
        Ok(())
    }
}
