//! Explicit same-UID, local Unix proof-only lane for the guarded writer.
//! No public TCP/Onion inlet, session upgrade, assignment or fallback.

use std::future::Future;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use harw_cloud_gateway::owner_readiness_listener::{
    OwnerReadinessHandler, OwnerReadinessUnixLane, serve_guarded_owner_readiness_io,
};
use harw_node_transport::{
    LocalNode, NodeTransportServer, OwnerProcessChannelBinding, PinnedPeers, ServerOptions,
    TranscriptWrappedVerifier, node_key_fingerprint,
};
use harw_session_host::{
    HostError, SessionHost,
    owner_readiness_contract::{OwnerReadinessChallenge, OwnerReadinessResponse},
};
use harw_types::NodeId;
use tokio::net::UnixListener;
use tokio::sync::watch;
use tokio::task::{AbortHandle, JoinHandle, JoinSet};

use super::writer_creation::readiness::{self, GuardedWriterReadiness};
use super::writer_namespace;

const MAX_CONNECTIONS: usize = 4;

type Answer<'a> = Pin<Box<dyn Future<Output = Result<OwnerReadinessResponse, HostError>> + Send + 'a>>;

struct ProofHandler(Arc<GuardedWriterReadiness>);

impl OwnerReadinessHandler for ProofHandler {
    fn answer<'a>(&'a self, channel: &'a OwnerProcessChannelBinding, challenge: &'a OwnerReadinessChallenge) -> Answer<'a> {
        Box::pin(self.0.answer(channel, challenge))
    }
}

struct LiveProofHandler {
    inner: Arc<dyn OwnerReadinessHandler>,
    bound: Arc<BoundPath>,
    ordinary_host: AbortHandle,
    stop: watch::Receiver<bool>,
}

impl LiveProofHandler {
    fn current(&self) -> bool {
        !self.ordinary_host.is_finished() && !*self.stop.borrow() && self.bound.current()
    }
}

impl OwnerReadinessHandler for LiveProofHandler {
    fn answer<'a>(&'a self, channel: &'a OwnerProcessChannelBinding, challenge: &'a OwnerReadinessChallenge) -> Answer<'a> {
        Box::pin(async move {
            let unavailable = || HostError::Denied("private writer readiness lane unavailable".into());
            if !self.current() { return Err(unavailable()); }
            let response = self.inner.answer(channel, challenge).await?;
            // The stream cancellation watcher is only a backstop. Never
            // release a proof during the interval between ordinary listener
            // loss and the next watcher tick.
            if !self.current() { return Err(unavailable()); }
            Ok(response)
        })
    }
}

struct BoundPath {
    path: PathBuf,
    remote: PathBuf,
    dev: u64,
    ino: u64,
    uid: u32,
}

impl BoundPath {
    fn same_socket(&self) -> bool {
        writer_namespace::readiness_socket_path(&self.remote).ok().as_deref() == Some(self.path.as_path())
            && std::fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
                metadata.file_type().is_socket() && metadata.dev() == self.dev && metadata.ino() == self.ino
                    && metadata.uid() == self.uid && metadata.nlink() == 1
            })
    }

    fn current(&self) -> bool {
        self.same_socket() && std::fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| metadata.mode() & 0o777 == 0o600)
    }
}

impl Drop for BoundPath {
    fn drop(&mut self) {
        // Never unlink a pre-existing or replacement socket.
        if self.same_socket() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(super) struct Prepared {
    server: NodeTransportServer,
    listener: UnixListener,
    bound: Arc<BoundPath>,
    hub: NodeId,
    hub_key: [u8; 32],
    handler: Arc<dyn OwnerReadinessHandler>,
}

impl Prepared {
    pub(super) fn hub(&self) -> &NodeId {
        &self.hub
    }

    pub(super) fn start(self, ordinary_host: AbortHandle, mut stop: watch::Receiver<bool>) -> ReadinessLane {
        let bound = Arc::clone(&self.bound);
        let handler: Arc<dyn OwnerReadinessHandler> = Arc::new(LiveProofHandler {
            inner: Arc::clone(&self.handler),
            bound: Arc::clone(&bound),
            ordinary_host: ordinary_host.clone(),
            stop: stop.clone(),
        });
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            let mut poll = tokio::time::interval(Duration::from_millis(100));
            while !ordinary_host.is_finished() && !*stop.borrow() && self.bound.current() {
                tokio::select! {
                    biased;
                    _ = stop.changed() => break,
                    _ = poll.tick() => {},
                    _ = connections.join_next(), if !connections.is_empty() => {},
                    next = self.listener.accept() => {
                        let Ok((stream, _)) = next else { break; };
                        if connections.len() >= MAX_CONNECTIONS || !self.bound.current() {
                            continue;
                        }
                        let mut stop = stop.clone();
                        let server = self.server.clone();
                        let path = Arc::clone(&self.bound);
                        let host = ordinary_host.clone();
                        let hub = self.hub.clone();
                        let hub_key_fingerprint = self.hub_key;
                        let handler = Arc::clone(&handler);
                        connections.spawn(async move {
                            let lane = OwnerReadinessUnixLane {
                                local_socket: &path.path,
                                expected_peer_uid: path.uid,
                                hub_node: hub.as_str(),
                                hub_key_fingerprint,
                            };
                            let shutdown = async {
                                let mut poll = tokio::time::interval(Duration::from_millis(100));
                                while !host.is_finished() && !*stop.borrow() && path.current() {
                                    tokio::select! {
                                        _ = poll.tick() => {},
                                        _ = stop.changed() => break,
                                    }
                                }
                            };
                            let _ = serve_guarded_owner_readiness_io(&server, stream, lane, handler, shutdown).await;
                        });
                    },
                }
            }
            // Dropping the JoinSet cancels all connections, including sealed witnesses.
        });
        ReadinessLane { task, _bound: bound }
    }
}

pub(super) struct ReadinessLane {
    task: JoinHandle<()>,
    _bound: Arc<BoundPath>,
}

impl Drop for ReadinessLane {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) fn prepare(host: SessionHost, remote: &Path, local: &LocalNode) -> Result<Prepared, String> {
    let configured = readiness::open_configured(host, remote, local)?;
    let hub_key = node_key_fingerprint(&configured.hub_public_key)
        .ok_or_else(|| "invalid proof-only Hub transport pin".to_owned())?;
    let mut peers = PinnedPeers::new();
    peers.pin(configured.hub.clone(), configured.hub_public_key).map_err(|error| error.to_string())?;
    let mut options = ServerOptions::default();
    options.max_connections = MAX_CONNECTIONS;
    options.header_read_timeout = Duration::from_secs(2);
    let server = NodeTransportServer::new(
        local.clone(), Arc::new(TranscriptWrappedVerifier::new(peers)), options,
    ).map_err(|error| format!("private writer readiness transport: {error}"))?;
    let path = writer_namespace::readiness_socket_path(remote).map_err(|error| error.to_string())?;
    // bind refuses any existing path. Writer namespace is 0700, and is
    // deliberately inaccessible to another UID even before chmod completes.
    let listener = UnixListener::bind(&path).map_err(|error| format!("private writer readiness socket: {error}"))?;
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    let uid = rustix::process::geteuid().as_raw();
    let bound = Arc::new(BoundPath { path, remote: remote.to_owned(), dev: metadata.dev(), ino: metadata.ino(), uid });
    if !bound.same_socket() {
        return Err("private writer readiness socket changed during bind".into());
    }
    std::fs::set_permissions(&bound.path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    if !bound.current() {
        return Err("private writer readiness socket is not private".into());
    }
    let handler = Arc::new(ProofHandler(configured.producer));
    Ok(Prepared { server, listener, bound, hub: configured.hub, hub_key, handler })
}
