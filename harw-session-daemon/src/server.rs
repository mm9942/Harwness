//! Unix-socket listener, peer authentication and per-connection upgrade.

use std::fmt;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use harw_session_host::{ClientIdentity, ConnectionId, SessionHost, caps_for_tier};
use harw_session_ws::{WsLimits, serve_connection, upgrade};
use harw_types::{
    ApprovalActor, AuthStrength, IngressSurface, PermissionTier, Principal, PrincipalKind,
    TrustZone,
};
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

/// Why the daemon could not start or stop cleanly.
#[derive(Debug)]
pub enum DaemonError {
    /// Another process is accepting on the socket.
    AlreadyRunning(PathBuf),
    /// The path exists and is not a socket; it is left untouched.
    NotASocket(PathBuf),
    /// The socket's directory is accessible to group or others.
    InsecureDirectory(PathBuf),
    /// The socket path has no parent directory.
    NoParent(PathBuf),
    /// I/O failure.
    Io(io::Error),
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning(p) => {
                write!(f, "a daemon is already listening on {}", p.display())
            }
            Self::NotASocket(p) => write!(f, "{} exists and is not a socket", p.display()),
            Self::InsecureDirectory(p) => {
                write!(
                    f,
                    "{} must not be accessible to group or others",
                    p.display()
                )
            }
            Self::NoParent(p) => write!(f, "{} has no parent directory", p.display()),
            Self::Io(e) => write!(f, "i/o error: {e}"),
        }
    }
}

impl std::error::Error for DaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for DaemonError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Listener configuration.
#[derive(Debug, Clone)]
pub struct UdsConfig {
    /// Socket path; its parent directory must be private (mode `0700`).
    pub socket_path: PathBuf,
    /// The only uid that is served. Everything else is dropped.
    pub allowed_uid: u32,
    /// Permission tier granted to the served uid.
    pub tier: PermissionTier,
    /// Maximum simultaneous connections; excess connections are dropped.
    pub max_connections: usize,
    /// Time allowed to send the HTTP request headers.
    pub header_timeout: Duration,
    /// Retry hint sent to clients while draining.
    pub drain_retry_after_ms: u64,
    /// How long shutdown waits for live connections to finish after the
    /// drain signal before giving up on them.
    pub shutdown_grace: Duration,
    /// How long a stale-socket probe may take. A timeout counts as "alive".
    pub probe_timeout: Duration,
    /// WebSocket limits.
    pub limits: WsLimits,
}

impl UdsConfig {
    /// Defaults: operator tier, 16 connections, 5 s header timeout.
    #[must_use]
    pub fn new(socket_path: impl Into<PathBuf>, allowed_uid: u32) -> Self {
        Self {
            socket_path: socket_path.into(),
            allowed_uid,
            tier: PermissionTier::Operator,
            max_connections: 16,
            header_timeout: Duration::from_secs(5),
            drain_retry_after_ms: 1000,
            shutdown_grace: Duration::from_secs(5),
            probe_timeout: Duration::from_secs(1),
            limits: WsLimits::default(),
        }
    }
}

/// A bound, not yet running listener.
#[derive(Debug)]
pub struct UdsServer {
    listener: UnixListener,
    config: UdsConfig,
    /// Device and inode of the socket this server created, so shutdown only
    /// unlinks that exact socket.
    socket_id: (u64, u64),
}

fn check_private_dir(dir: &Path) -> Result<(), DaemonError> {
    let meta = std::fs::metadata(dir)?;
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(DaemonError::InsecureDirectory(dir.to_path_buf()));
    }
    Ok(())
}

impl UdsServer {
    /// Bind the socket. Must run inside a Tokio runtime.
    pub async fn bind(config: UdsConfig) -> Result<Self, DaemonError> {
        let path = config.socket_path.clone();
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| DaemonError::NoParent(path.clone()))?;
        check_private_dir(dir)?;
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_socket() => {
                // Fail closed: only an explicit "connection refused" proves the
                // socket is stale. A success, a timeout (e.g. a full backlog)
                // or any other error is treated as a live daemon.
                let probe =
                    tokio::time::timeout(config.probe_timeout, UnixStream::connect(&path)).await;
                match probe {
                    Ok(Err(e)) if e.kind() == io::ErrorKind::ConnectionRefused => {
                        std::fs::remove_file(&path)?;
                    }
                    _ => return Err(DaemonError::AlreadyRunning(path)),
                }
            }
            Ok(_) => return Err(DaemonError::NotASocket(path)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let meta = std::fs::symlink_metadata(&path)?;
        Ok(Self {
            listener,
            config,
            socket_id: (meta.dev(), meta.ino()),
        })
    }

    /// The uid that owns the bound socket file, which is this process's
    /// effective uid. Lets a caller default `allowed_uid` to "myself" without
    /// unsafe code.
    pub fn own_uid(path: &Path) -> Result<u32, DaemonError> {
        Ok(std::fs::metadata(path)?.uid())
    }

    /// Accept connections until `shutdown` flips to `true`, then drain the
    /// host, wait up to `shutdown_grace` for live connections to finish, and
    /// remove the socket file, but only if the path still names the socket
    /// this server created.
    ///
    /// Connections that outlive the grace period are logged; they are not
    /// aborted here, so the caller's runtime shutdown ends them.
    pub async fn run(
        self,
        host: SessionHost,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), DaemonError> {
        let max = self.config.max_connections.max(1);
        let permits = Arc::new(Semaphore::new(max));
        let socket_id = self.socket_id;
        let config = Arc::new(self.config);
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                accepted = self.listener.accept() => {
                    match accepted {
                        Ok((stream, _)) => {
                            handle_accept(stream, &host, &config, &permits, &shutdown);
                        }
                        Err(error) => {
                            // Persistent errors such as EMFILE keep the listener
                            // readable: log and back off instead of spinning.
                            tracing::error!(%error, "session daemon: accept failed");
                            tokio::time::sleep(ACCEPT_BACKOFF).await;
                        }
                    }
                }
            }
        }
        host.drain(config.drain_retry_after_ms);
        // Every connection (HTTP phase and upgraded session) holds a permit,
        // so owning all permits means nothing is left.
        let all = u32::try_from(max).unwrap_or(u32::MAX);
        let waited = tokio::time::timeout(config.shutdown_grace, permits.acquire_many(all)).await;
        if waited.is_err() {
            let live = max.saturating_sub(permits.available_permits());
            tracing::warn!(
                live,
                "session daemon: connections still open after the grace period"
            );
        }
        remove_own_socket(&config.socket_path, socket_id)
    }
}

/// Pause after a failed `accept`.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// Unlink `path` only if it still is the socket with `id`; report failures.
fn remove_own_socket(path: &Path, id: (u64, u64)) -> Result<(), DaemonError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() && (meta.dev(), meta.ino()) == id => {
            std::fs::remove_file(path)?;
            Ok(())
        }
        Ok(_) => {
            tracing::warn!(
                path = %path.display(),
                "session daemon: socket path no longer names our socket, not removing it"
            );
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn local_identity(uid: u32, tier: PermissionTier) -> ClientIdentity {
    ClientIdentity {
        principal: Principal::trusted_ingress(
            PrincipalKind::Human,
            format!("uid:{uid}"),
            IngressSurface::Tui,
            tier,
        ),
        tenant: None,
        caps: caps_for_tier(tier),
        device: None,
        actor: ApprovalActor::Operator {
            id: format!("uid:{uid}"),
        },
        label: format!("local-{uid}"),
        zone: TrustZone::Local,
        strength: AuthStrength::PeerCredential,
        connection: ConnectionId::next(),
        agent: None,
    }
}

fn handle_accept(
    stream: UnixStream,
    host: &SessionHost,
    config: &Arc<UdsConfig>,
    permits: &Arc<Semaphore>,
    shutdown: &watch::Receiver<bool>,
) {
    let Ok(cred) = stream.peer_cred() else {
        tracing::warn!("session daemon: peer credentials unavailable, dropping");
        return;
    };
    if cred.uid() != config.allowed_uid {
        tracing::warn!(
            uid = cred.uid(),
            "session daemon: peer uid not allowed, dropping"
        );
        return;
    }
    let Ok(permit) = Arc::clone(permits).try_acquire_owned() else {
        tracing::warn!("session daemon: connection limit reached, dropping");
        return;
    };
    let identity = local_identity(cred.uid(), config.tier);
    tokio::spawn(serve_stream(
        stream,
        host.clone(),
        identity,
        Arc::clone(config),
        permit,
        shutdown.clone(),
    ));
}

async fn serve_stream(
    stream: UnixStream,
    host: SessionHost,
    identity: ClientIdentity,
    config: Arc<UdsConfig>,
    permit: OwnedSemaphorePermit,
    shutdown: watch::Receiver<bool>,
) {
    let limits = config.limits;
    let permit = Arc::new(permit);
    let service = service_fn(move |request: hyper::Request<Incoming>| {
        let host = host.clone();
        let identity = identity.clone();
        let shutdown = shutdown.clone();
        let permit = Arc::clone(&permit);
        async move {
            let ws_limits = limits;
            let upgraded = upgrade::upgrade_server(
                request,
                limits.tungstenite_config(),
                move |ws| async move {
                    // The permit lives as long as the session connection.
                    let _permit = permit;
                    let Ok(port) = host.connect(identity) else {
                        return;
                    };
                    serve_connection(ws, Arc::new(port), ws_limits, shutdown).await;
                },
            );
            let response = match upgraded {
                Ok(response) => response.map(|_| http_body_util::Full::new(Bytes::new())),
                Err(rejection) => upgrade::rejection_response(&rejection),
            };
            Ok::<_, std::convert::Infallible>(response)
        }
    });
    let _ = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(config.header_timeout)
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades()
        .await;
}
