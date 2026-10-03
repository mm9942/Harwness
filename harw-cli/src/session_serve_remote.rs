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
//! # Jobs rule
//! Everything on the serve path is async on the caller's runtime.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use harw_core::ModelProvider;
use harw_node_listener::{
    DeviceRegistry, HostRevoker, IdentityMapper, NodeListener, RegistryIdentityMapper,
    RevocationSink,
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
    /// How often `node-devices.conf` is scanned for newly revoked devices
    /// (live revocation). Default 5 s.
    pub revocation_poll: Duration,
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
    let approvals = DurableApprovals::open(&sessions_root)
        .map_err(|error| format!("remote approvals: {error}"))?;
    let services = DaemonServices::new(driver, Arc::new(transcripts), Arc::new(approvals));
    let host = Arc::new(
        SessionHost::open(
            HostConfig::new(remote_dir.join("host")),
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
    let mapper: Arc<dyn IdentityMapper> = Arc::new(RegistryIdentityMapper::new(&remote_dir));
    let node_listener = NodeListener::new(server, host, mapper, WsLimits::default());
    let revoker = node_listener.revoker().with_registry(&remote_dir);
    let (shutdown, mut rx) = watch::channel(false);
    let watcher = tokio::spawn(watch_revocations(
        revoker,
        remote_dir.clone(),
        config.revocation_poll,
        shutdown.subscribe(),
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

/// Live revocation: a device whose record in `node-devices.conf` is marked
/// `revoked` loses its authority in the host at once and its open connections
/// are closed. The file is the operator's control surface (the same file
/// refuses the device's next handshake); no network endpoint is involved.
/// Devices already revoked at start are applied on the first scan.
async fn watch_revocations(
    revoker: HostRevoker,
    registry_dir: PathBuf,
    every: Duration,
    mut stop: watch::Receiver<bool>,
) {
    let registry = DeviceRegistry::new(&registry_dir);
    let mut applied: HashSet<DeviceId> = HashSet::new();
    let mut ticker = tokio::time::interval(every.max(Duration::from_millis(10)));
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = stop.wait_for(|stop| *stop) => return,
        }
        let records = match registry.records() {
            Ok(records) => records,
            Err(error) => {
                tracing::warn!(%error, "revocation scan: device registry unreadable");
                continue;
            }
        };
        for record in records.into_iter().filter(|record| record.revoked) {
            if !applied.insert(record.device.clone()) {
                continue;
            }
            match revoker.revoke_device(&record.device) {
                Ok(report) => tracing::info!(
                    device = record.device.as_str(),
                    connections_closed = report.connections_closed,
                    "device revoked"
                ),
                Err(error) => tracing::warn!(%error, "device revocation incomplete"),
            }
        }
    }
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
    async fn an_enrolled_device_lists_sessions_over_the_node_transport_and_revocation_cuts_it()
    -> TestResult {
        let temp = tempfile::tempdir()?;
        let mut config = RemoteServeConfig::new(temp.path(), temp.path(), temp.path());
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
        let config = RemoteServeConfig::new(temp.path(), temp.path(), temp.path());
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
        // A key that signs the raw (unwrapped) transcript is refused in a
        // wrapped fleet, although it is pinned and enrolled.
        assert!(connect(&plain).await.is_err());
        ingress.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn an_unenrolled_node_is_refused() -> TestResult {
        let temp = tempfile::tempdir()?;
        let config = RemoteServeConfig::new(temp.path(), temp.path(), temp.path());
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
