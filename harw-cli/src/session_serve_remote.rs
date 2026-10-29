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
//!   |-- SessionHost  own state dir  <profile>/session-host/remote/host
//!   |-- driver       CoreTurnDriver + GatewayCoreFactory (tier = config.tier)
//!   |-- identity     RegistryIdentityMapper (<state>/remote/node-devices.conf)
//!   `-- listener     NodeListener::serve (revocation closes live connections)
//! ```
//!
//! # Why a separate host
//! A hosted session's rights come from the factory, which does not see who
//! created the session. The local socket's peer is the gateway's own uid
//! (an operator); remote devices are different principals. They therefore get
//! their **own host, state and factory**: the sandbox rights are cut to
//! [`RemoteServeConfig::tier`] (default observer), and a device's registry tier
//! only decides its host caps. Sessions are not shared between the local and
//! the remote host.
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
use harw_session_host::{DurableApprovals, DurableTranscripts, HostConfig, SessionHost};
use harw_session_ws::WsLimits;
use harw_types::{ApprovalActor, DeviceId, PermissionTier};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::session_serve::{GatewayCoreFactory, private_dir};

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
    /// How often registry and pin policy is revalidated (live invalidation).
    /// Default 5 s; values below 10 ms are clamped to 10 ms.
    pub revocation_poll: Duration,
    /// Digest of the exact owner pin file used to build the transport verifier.
    pub(crate) peer_pins_digest: Option<[u8; 32]>,
    #[cfg(test)]
    injected_peer_trust: bool,
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
            revocation_poll: Duration::from_secs(5),
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
    shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
    watcher: JoinHandle<()>,
    addr: SocketAddr,
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

    /// Stops accepting, closes live connections and waits for the listener.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        if let Err(error) = self.watcher.await {
            tracing::error!(%error, "remote revocation watcher failed");
        }
        if let Err(error) = self.task.await {
            tracing::error!(%error, "remote session ingress task failed");
        } else {
            tracing::info!(addr = %self.addr, "remote session ingress stopped");
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
    let sessions_root = config.remote_dir().join(SESSIONS_DIR_NAME);
    private_dir(&sessions_root)?;
    let factory = Arc::new(
        GatewayCoreFactory::new(
            model,
            &sessions_root,
            config.home.clone(),
            config.cwd.clone(),
            0,
        )
        .with_tier(config.tier)
        .with_actor(config.actor()),
    );
    let driver = CoreTurnDriver::with_factory(CoreDriverConfig::new(sessions_root), factory)
        .map_err(|error| format!("remote session driver: {error}"))?;
    start_remote_with_driver(config, Arc::new(driver), local, verifier, listener).await
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
    let remote_dir = config.remote_dir();
    let sessions_root = remote_dir.join(SESSIONS_DIR_NAME);
    private_dir(&sessions_root)?;
    private_dir(&remote_dir.join("host"))?;
    let transcripts = DurableTranscripts::open(&sessions_root)
        .map_err(|error| format!("remote transcripts: {error}"))?;
    let host_dir = remote_dir.join("host");
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
    let host = Arc::new(
        SessionHost::open(
            HostConfig::new(host_dir),
            services.driver,
            services.transcripts,
            services.approvals,
        )
        .map_err(|error| format!("remote host: {error}"))?,
    );
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
    let node_listener = NodeListener::new(server, host, mapper, WsLimits::default());
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
        shutdown,
        task,
        watcher,
        addr,
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
                if applied.insert(record.device.clone()) {
                    if let Err(error) = revoker.revoke_device(&record.device) {
                        tracing::warn!(%error, "device revocation incomplete");
                    }
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
}
