//! `harw-node-listener`: the own-cloud session listener (W00 §2.4
//! `remote_listener.rs`, §6, §7, S08).
//!
//! Order (W00 D4): node transport authenticates the peer, the listener maps
//! the [`harw_node_transport::AuthenticatedPeer`] to a
//! [`harw_session_host::ClientIdentity`] ([`identity`]), only then answers
//! the HTTP/1 upgrade and hands the socket plus the host port to
//! `harw-session-ws`. No application frame establishes identity. Revocation
//! ([`revoke`]) closes live connections, not just future handshakes.
//!
//! Skeleton: entry points answer [`ListenerError::NotImplemented`]. File
//! ownership (see `W00-ws-integration-map.md`): `lib.rs` (the
//! [`NodeListener`] service) is shared with `identity.rs`/`revoke.rs` under
//! S08 alone.

#![forbid(unsafe_code)]

pub mod identity;
pub mod revoke;

use std::future::Future;
use std::sync::Arc;

use harw_node_transport::NodeTransportServer;
use harw_session_host::SessionHost;
use harw_session_ws::WsLimits;
use tokio::net::TcpListener;

pub use identity::{IdentityMapper, MapFuture, RegistryIdentityMapper};
pub use revoke::{HostRevoker, RevocationReport, RevocationSink};

/// Failures of the listener.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ListenerError {
    /// Skeleton stub: the named entry point is not implemented yet.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
    /// The peer is not an enrolled, unrevoked device (fail closed).
    #[error("peer is not an authorised device")]
    UnknownPeer,
    /// The host refused the connection (revoked, draining, invalid identity).
    #[error("host refused: {0}")]
    Host(String),
    /// Listener I/O failed.
    #[error("io: {0}")]
    Io(String),
}

/// The own-cloud session listener.
pub struct NodeListener {
    _server: NodeTransportServer,
    _host: Arc<SessionHost>,
    _mapper: Arc<dyn IdentityMapper>,
    _limits: WsLimits,
}

impl NodeListener {
    /// Compose a listener from an authenticated transport server, the host
    /// and an identity mapper.
    #[must_use]
    pub fn new(
        server: NodeTransportServer,
        host: Arc<SessionHost>,
        mapper: Arc<dyn IdentityMapper>,
        limits: WsLimits,
    ) -> Self {
        Self {
            _server: server,
            _host: host,
            _mapper: mapper,
            _limits: limits,
        }
    }

    /// Serve `listener` until `shutdown` completes.
    ///
    /// # Errors
    /// [`ListenerError::NotImplemented`] in the skeleton.
    pub async fn serve<F>(&self, listener: TcpListener, shutdown: F) -> Result<(), ListenerError>
    where
        F: Future<Output = ()>,
    {
        let _ = (listener, shutdown);
        Err(ListenerError::NotImplemented("NodeListener::serve (S08)"))
    }
}
