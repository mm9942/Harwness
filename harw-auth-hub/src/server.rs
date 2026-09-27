//! Listener setup and the accept loop.
//!
//! Two ways to obtain the listening socket:
//!
//! - [`HubListener::bind`]: create the parent directory (`0750`) if it is
//!   missing, remove a *stale* socket (a socket file nobody accepts on; a
//!   live socket or a non-socket file is an error and never touched), bind,
//!   then `chmod 0660`. The file is removed again when [`serve`] returns.
//! - [`HubListener::from_systemd`]: take the one `ListenStream=` socket
//!   systemd passed (`--systemd-socket`); systemd owns path, mode and
//!   cleanup (`SocketMode=0660`, `DirectoryMode=0750`, `RemoveOnStop=yes`).
//!
//! [`serve`] accepts until `shutdown` resolves, reads `SO_PEERCRED` of every
//! connection (fail closed: a connection without credentials is dropped),
//! serves each with Hyper HTTP/1, and on shutdown stops accepting, asks
//! every open connection to finish its in-flight request
//! (`graceful_shutdown`) and waits up to [`DRAIN_TIMEOUT`].

use std::fs::{self, DirBuilder};
use std::future::Future;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use hyper_util::rt::TokioIo;
use tokio::net::UnixListener;
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::auth::PeerCred;
use crate::error::HubError;
use crate::service::HubService;
use crate::systemd;

/// Mode of a socket the hub binds itself.
pub const SOCKET_MODE: u32 = 0o660;

/// Mode of a socket parent directory the hub creates itself.
pub const SOCKET_DIR_MODE: u32 = 0o750;

/// How long [`serve`] waits for open connections after shutdown.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Back-off after a failed `accept` (e.g. `EMFILE`), so the loop does not spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// A bound `AF_UNIX` stream listener.
#[derive(Debug)]
pub struct HubListener {
    listener: UnixListener,
    /// Socket file to remove on shutdown (only for sockets we bound).
    owned_path: Option<PathBuf>,
}

impl HubListener {
    /// Bind `path` (see module docs). Must run inside a Tokio runtime.
    ///
    /// # Errors
    ///
    /// [`HubError::Socket`] on I/O failure, [`HubError::SocketInUse`] if a
    /// process accepts on `path`, [`HubError::NotASocket`] if `path` is some
    /// other file.
    pub fn bind(path: &Path) -> Result<Self, HubError> {
        let socket_error = |source| HubError::Socket {
            path: path.to_path_buf(),
            source,
        };
        let missing_parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty() && !parent.exists());
        if let Some(parent) = missing_parent {
            DirBuilder::new()
                .recursive(true)
                .mode(SOCKET_DIR_MODE)
                .create(parent)
                .map_err(socket_error)?;
        }
        remove_stale_socket(path)?;
        let listener = UnixListener::bind(path).map_err(socket_error)?;
        fs::set_permissions(path, fs::Permissions::from_mode(SOCKET_MODE)).map_err(socket_error)?;
        Ok(Self {
            listener,
            owned_path: Some(path.to_path_buf()),
        })
    }

    /// Adopt the socket systemd passed. Must run inside a Tokio runtime.
    ///
    /// # Errors
    ///
    /// [`HubError::Systemd`] for an invalid activation environment,
    /// [`HubError::Socket`] if the descriptor cannot be registered.
    pub fn from_systemd() -> Result<Self, HubError> {
        let fd = systemd::acquire_listen_socket()?;
        let socket_error = |source| HubError::Socket {
            path: PathBuf::from("<systemd>"),
            source,
        };
        let std_listener = std::os::unix::net::UnixListener::from(fd);
        std_listener.set_nonblocking(true).map_err(socket_error)?;
        let listener = UnixListener::from_std(std_listener).map_err(socket_error)?;
        Ok(Self {
            listener,
            owned_path: None,
        })
    }

    /// The socket file this listener owns (removed on shutdown), if any.
    #[must_use]
    pub fn owned_path(&self) -> Option<&Path> {
        self.owned_path.as_deref()
    }
}

/// Remove `path` if it is a socket nobody accepts on.
fn remove_stale_socket(path: &Path) -> Result<(), HubError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(HubError::Socket {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.file_type().is_socket() {
        return Err(HubError::NotASocket(path.to_path_buf()));
    }
    if std::os::unix::net::UnixStream::connect(path).is_ok() {
        return Err(HubError::SocketInUse(path.to_path_buf()));
    }
    tracing::info!(path = %path.display(), "removing stale socket");
    fs::remove_file(path).map_err(|source| HubError::Socket {
        path: path.to_path_buf(),
        source,
    })
}

/// Accept and serve connections until `shutdown` resolves, then drain.
///
/// # Errors
///
/// Currently infallible after the listener is bound (accept errors are
/// logged and retried); the `Result` keeps room for fatal listener errors.
pub async fn serve<F>(listener: HubListener, hub: HubService, shutdown: F) -> Result<(), HubError>
where
    F: Future<Output = ()>,
{
    let HubListener {
        listener,
        owned_path,
    } = listener;
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _addr)) => {
                    let peer = match stream.peer_cred() {
                        Ok(cred) => PeerCred {
                            uid: cred.uid(),
                            gid: cred.gid(),
                            pid: cred.pid(),
                        },
                        Err(error) => {
                            tracing::warn!(%error, "SO_PEERCRED unavailable; dropping connection");
                            continue;
                        }
                    };
                    tracing::debug!(uid = peer.uid, pid = peer.pid, "connection accepted");
                    let service = hub.for_connection(Some(peer));
                    let mut stop = stop_rx.clone();
                    connections.spawn(async move {
                        let connection = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), service);
                        tokio::pin!(connection);
                        let result = tokio::select! {
                            result = connection.as_mut() => result,
                            _ = stop.changed() => {
                                connection.as_mut().graceful_shutdown();
                                connection.as_mut().await
                            }
                        };
                        if let Err(error) = result {
                            tracing::debug!(%error, uid = peer.uid, "connection ended with error");
                        }
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "accept failed");
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }

    // Stop accepting first, then remove our socket file, then drain.
    drop(listener);
    if let Some((path, Err(error))) = owned_path
        .as_deref()
        .map(|path| (path, fs::remove_file(path)))
    {
        tracing::warn!(%error, path = %path.display(), "cannot remove socket file");
    }
    // A send error only means no connection task holds a receiver.
    let _ = stop_tx.send(true);
    let drain = async { while connections.join_next().await.is_some() {} };
    if tokio::time::timeout(DRAIN_TIMEOUT, drain).await.is_err() {
        tracing::warn!("drain timeout; aborting remaining connections");
        connections.abort_all();
    }
    tracing::info!("auth hub stopped");
    Ok(())
}

/// Resolve on the first `SIGTERM` or `SIGINT`.
///
/// # Errors
///
/// [`HubError::Signal`] if a handler cannot be registered. Must run inside a
/// Tokio runtime.
pub fn shutdown_signal() -> Result<impl Future<Output = ()>, HubError> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate()).map_err(HubError::Signal)?;
    let mut interrupt = signal(SignalKind::interrupt()).map_err(HubError::Signal)?;
    Ok(async move {
        tokio::select! {
            _ = terminate.recv() => tracing::info!("SIGTERM received; shutting down"),
            _ = interrupt.recv() => tracing::info!("SIGINT received; shutting down"),
        }
    })
}
