//! Unix-socket listener, peer authentication and per-connection upgrade.

use std::fmt;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use harw_session_com::local::peer_identity;
use harw_session_com::{ComConfig, ComServer, HostBinder, PortOffer};
use harw_session_host::SessionHost;
use harw_session_ws::WsLimits;
use harw_types::PermissionTier;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

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
    /// The socket's directory is a symlink or not a directory.
    NotADirectory(PathBuf),
    /// The session host could not be opened.
    Host(String),
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
            Self::NotADirectory(p) => {
                write!(f, "{} is not a plain directory", p.display())
            }
            Self::Host(e) => write!(f, "session host: {e}"),
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
    /// Exclusive lock on `<socket>.lock`, held for the server's lifetime: two
    /// daemons that start together both see the same stale socket, and
    /// without it the second one would unlink the first one's fresh socket.
    _lock: std::fs::File,
}

/// Takes the start-up lock next to the socket (inside the private directory).
fn lock_socket_dir(socket: &Path) -> Result<std::fs::File, DaemonError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut name = socket.as_os_str().to_owned();
    name.push(".lock");
    let lock_path = PathBuf::from(name);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        // Never follow a planted symlink.
        .custom_flags(i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).unwrap_or(0))
        .open(&lock_path)?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(file),
        Err(rustix::io::Errno::WOULDBLOCK) => {
            Err(DaemonError::AlreadyRunning(socket.to_path_buf()))
        }
        Err(errno) => Err(DaemonError::Io(io::Error::from(errno))),
    }
}

fn check_private_dir(dir: &Path) -> Result<(), DaemonError> {
    // Not `metadata`: a symlinked directory could be swapped under us.
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Err(DaemonError::NotADirectory(dir.to_path_buf()));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(DaemonError::InsecureDirectory(dir.to_path_buf()));
    }
    // Mode alone is not enough: a private directory of another user is not
    // ours to put a socket into.
    if meta.uid() != rustix::process::geteuid().as_raw() {
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
        let lock = lock_socket_dir(&path)?;
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
            _lock: lock,
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
        let Self {
            listener,
            config,
            socket_id,
            _lock,
        } = self;
        let config = Arc::new(config);
        // The per-connection path (peer identity, upgrade, bounded
        // connections, drain) is the Com layer's; this crate keeps the socket
        // lifecycle. `PortOffer::All` also serves `tool.*` and `gateway.*`.
        let com = ComServer::new(
            &ComConfig {
                limits: config.limits,
                max_connections: config.max_connections.clamp(1, MAX_CONNECTIONS_CEILING),
                header_timeout: config.header_timeout,
                shutdown_grace: config.shutdown_grace,
                ..ComConfig::default()
            },
            Arc::new(HostBinder::new(host.clone(), PortOffer::All)),
            shutdown.clone(),
        );
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _)) => {
                            let Some(identity) =
                                peer_identity(&stream, config.allowed_uid, config.tier)
                            else {
                                continue;
                            };
                            if let Err(error) = com.serve_io(stream, identity) {
                                tracing::warn!(%error, "session daemon: connection dropped");
                            }
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
        // Stop being reachable *before* the drain: a client that connects
        // during the grace period would be accepted by the kernel into the
        // backlog and then never served. With the listener closed and the
        // path gone it gets `ENOENT`/`ECONNREFUSED` at once and can retry or
        // restart the daemon; only already connected sessions hear
        // `HostDraining`.
        drop(listener);
        let removed = remove_own_socket(&config.socket_path, socket_id);
        host.drain(config.drain_retry_after_ms);
        // Every connection (HTTP phase and upgraded session) holds a permit.
        com.drain().await;
        removed
    }
}

/// Upper bound for `max_connections`, keeps the permit arithmetic exact.
const MAX_CONNECTIONS_CEILING: usize = 1 << 20;

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
