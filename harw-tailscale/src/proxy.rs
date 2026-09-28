//! TCP proxy from the node's tailnet address to the control plane's
//! dedicated Unix socket.
//!
//! # Description
//! Each accepted connection first passes the [`Admission`] gate; refused
//! peers are closed without a byte of HTTP reaching the control plane. An
//! admitted connection is copied byte for byte to the upstream socket; the
//! login and node are logged. At most [`MAX_CONNECTIONS`] connections run at
//! once; admission has a time limit.

use crate::gate::Admission;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, UnixStream};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;

/// Default TCP port on the tailnet address.
pub const DEFAULT_TAILNET_PORT: u16 = 8443;

/// Upper bound for concurrently proxied connections.
pub const MAX_CONNECTIONS: usize = 64;

/// Time budget for admitting one peer.
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);

/// Serves `listener` until `shutdown` turns `true`.
///
/// # Errors
/// Only a failing `accept()` on the listener itself; per-connection errors
/// are logged and close that connection.
pub async fn serve_tailnet(
    listener: TcpListener,
    upstream: PathBuf,
    gate: Arc<dyn Admission>,
    mut shutdown: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut connections = JoinSet::new();
    let outcome = loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break Ok(());
                }
            }
            completed = connections.join_next(), if !connections.is_empty() => {
                let _ = completed;
            }
            accepted = listener.accept() => {
                let (mut stream, remote) = match accepted {
                    Ok(pair) => pair,
                    Err(error) => break Err(error),
                };
                let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
                    tracing::warn!(%remote, "tailnet.proxy.busy");
                    continue;
                };
                let gate = Arc::clone(&gate);
                let upstream = upstream.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    let admitted = tokio::time::timeout(ADMISSION_TIMEOUT, gate.admit(remote)).await;
                    let peer = match admitted {
                        Ok(Ok(peer)) => peer,
                        Ok(Err(error)) => {
                            tracing::warn!(%remote, %error, "tailnet.proxy.refused");
                            return;
                        }
                        Err(_) => {
                            tracing::warn!(%remote, "tailnet.proxy.admission_timeout");
                            return;
                        }
                    };
                    let mut upstream_stream = match UnixStream::connect(&upstream).await {
                        Ok(stream) => stream,
                        Err(error) => {
                            tracing::error!(%error, path = %upstream.display(), "tailnet.proxy.upstream_unreachable");
                            return;
                        }
                    };
                    tracing::info!(%remote, login = %peer.login, node = %peer.node, "tailnet.proxy.admitted");
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream_stream).await;
                });
            }
        }
    };
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::{AdmissionFuture, GateError, TailnetPeer};
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    /// Admits exactly one remote port; refuses every other peer.
    struct OnlyPort(u16);

    impl Admission for OnlyPort {
        fn admit(&self, addr: SocketAddr) -> AdmissionFuture<'_> {
            let allowed = addr.port() == self.0;
            Box::pin(async move {
                if allowed {
                    Ok(TailnetPeer {
                        addr,
                        login: "mia@example.com".to_owned(),
                        node: "s24".to_owned(),
                    })
                } else {
                    Err(GateError::NotTailnet(addr.ip()))
                }
            })
        }
    }

    #[tokio::test]
    async fn admitted_peers_reach_upstream_and_refused_peers_do_not() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let upstream_path = dir.path().join("tailnet.sock");
        let upstream =
            tokio::net::UnixListener::bind(&upstream_path).map_err(|error| error.to_string())?;
        let (hits_tx, mut hits_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = upstream.accept().await {
                let hits = hits_tx.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 64];
                    let read = stream.read(&mut buf).await.unwrap_or(0);
                    let _ = hits.send(buf[..read].to_vec());
                    let _ = stream.write_all(b"pong").await;
                });
            }
        });

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|error| error.to_string())?;
        let proxy_addr = listener.local_addr().map_err(|error| error.to_string())?;
        // Bind the admitted client first so the gate knows its port.
        let admitted_socket = tokio::net::TcpSocket::new_v4().map_err(|error| error.to_string())?;
        admitted_socket
            .bind("127.0.0.1:0".parse().map_err(|error| format!("{error}"))?)
            .map_err(|error| error.to_string())?;
        let admitted_port = admitted_socket
            .local_addr()
            .map_err(|error| error.to_string())?
            .port();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let proxy = tokio::spawn(serve_tailnet(
            listener,
            upstream_path,
            Arc::new(OnlyPort(admitted_port)),
            shutdown_rx,
        ));

        let mut admitted = admitted_socket
            .connect(proxy_addr)
            .await
            .map_err(|error| error.to_string())?;
        admitted
            .write_all(b"ping")
            .await
            .map_err(|error| error.to_string())?;
        let mut reply = [0u8; 4];
        admitted
            .read_exact(&mut reply)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(&reply, b"pong");
        assert_eq!(hits_rx.recv().await.as_deref(), Some(&b"ping"[..]));

        let mut refused = tokio::net::TcpStream::connect(proxy_addr)
            .await
            .map_err(|error| error.to_string())?;
        let _ = refused.write_all(b"GET / HTTP/1.1\r\n\r\n").await;
        let mut rest = Vec::new();
        let _ = refused.read_to_end(&mut rest).await;
        assert!(rest.is_empty(), "refused peer got bytes: {rest:?}");
        assert!(hits_rx.try_recv().is_err(), "refused peer reached upstream");

        shutdown_tx.send(true).map_err(|error| error.to_string())?;
        proxy
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())
    }
}
