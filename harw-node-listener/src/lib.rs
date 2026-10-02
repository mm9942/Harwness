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
//! File
//! ownership (see `W00-ws-integration-map.md`): `lib.rs` (the
//! [`NodeListener`] service) is shared with `identity.rs`/`revoke.rs` under
//! S08 alone.

#![forbid(unsafe_code)]

pub mod identity;
pub mod revoke;

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use harw_node_transport::{AuthenticatedPeer, NodeTransportServer, UpgradeError, peer_of};
use harw_session_host::{ConnectionId, HostError, SessionHost};
use harw_session_ws::dispatch::Ports;
use harw_session_ws::{WsLimits, serve_connection_with, upgrade};
use http::{Request, Response, StatusCode};
use http_body_util::{Empty, Full};
use hyper::body::Incoming;
use tokio::net::TcpListener;

pub use identity::{
    DeviceRecord, DeviceRegistry, IdentityMapper, MapFuture, RegistryIdentityMapper,
};
pub use revoke::{HostRevoker, LiveConnections, RevocationReport, RevocationSink};

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

/// Per-request upgrade logic: identity first, then the HTTP/1 upgrade, then
/// the session protocol (W00 D4). Independent of the transport so it can be
/// driven with any already-authenticated peer.
pub struct UpgradeHandler {
    host: Arc<SessionHost>,
    mapper: Arc<dyn IdentityMapper>,
    limits: WsLimits,
    live: LiveConnections,
}

/// Removes a connection from the live table when dropped: after the session
/// ends, or when the upgrade never completes.
struct LiveGuard {
    live: LiveConnections,
    connection: ConnectionId,
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.live.unregister(self.connection);
    }
}

fn status(code: StatusCode, reason: &'static str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from_static(reason.as_bytes())));
    *response.status_mut() = code;
    response
}

impl UpgradeHandler {
    /// Handler admitting through `host` with identities from `mapper`.
    #[must_use]
    pub fn new(
        host: Arc<SessionHost>,
        mapper: Arc<dyn IdentityMapper>,
        limits: WsLimits,
    ) -> Self {
        Self {
            host,
            mapper,
            limits,
            live: LiveConnections::new(),
        }
    }

    /// The live-connection table (share it with a [`HostRevoker`]).
    #[must_use]
    pub fn live(&self) -> &LiveConnections {
        &self.live
    }

    /// Answer one upgrade request of `peer` (the transport-authenticated
    /// peer; `Err` when the transport attached none, which fails closed).
    ///
    /// Refusals never reveal why: `401` no peer, `403` unknown/revoked
    /// device or host refusal, the upgrade module's own status for a bad
    /// handshake shape, `503` for host failures.
    pub async fn handle<B>(
        &self,
        request: Request<B>,
        peer: Result<AuthenticatedPeer, UpgradeError>,
    ) -> Response<Full<Bytes>>
    where
        B: Send + 'static,
    {
        let Ok(peer) = peer else {
            return status(StatusCode::UNAUTHORIZED, "unauthenticated");
        };
        let connection = ConnectionId::next();
        let identity = match self.mapper.map(&peer, connection).await {
            Ok(identity) => identity,
            Err(ListenerError::UnknownPeer) => {
                return status(StatusCode::FORBIDDEN, "not an authorised device");
            }
            Err(error) => {
                tracing::warn!(%error, "identity mapping failed");
                return status(StatusCode::SERVICE_UNAVAILABLE, "identity unavailable");
            }
        };
        // The mapper must only hand out its own connection id and a device.
        let Some(device) = identity.device.clone() else {
            return status(StatusCode::FORBIDDEN, "not an authorised device");
        };
        if identity.connection != connection {
            return status(StatusCode::FORBIDDEN, "not an authorised device");
        }
        if let Err(rejection) = upgrade::validate_upgrade(&request) {
            return upgrade::rejection_response(&rejection);
        }
        // Register before `connect`: a revocation then either sees the entry
        // or `connect` sees the revoked device.
        let closed = self.live.register(connection, device);
        let guard = LiveGuard {
            live: self.live.clone(),
            connection,
        };
        let port = match self.host.connect(identity) {
            Ok(port) => port,
            Err(HostError::Revoked | HostError::Denied(_)) => {
                return status(StatusCode::FORBIDDEN, "refused by host");
            }
            Err(error) => {
                tracing::warn!(%error, "host refused connection");
                return status(StatusCode::SERVICE_UNAVAILABLE, "host unavailable");
            }
        };
        let limits = self.limits;
        match upgrade::upgrade_server(request, limits.tungstenite_config(), move |ws| async move {
            let _guard = guard;
            let ports = Ports::all(Arc::new(port));
            let end = serve_connection_with(ws, ports, limits, closed).await;
            tracing::debug!(?end, "session connection ended");
        }) {
            Ok(response) => response.map(|_: Empty<Bytes>| Full::new(Bytes::new())),
            Err(rejection) => upgrade::rejection_response(&rejection),
        }
    }
}

/// Tower service mounted on the node transport: takes the peer the transport
/// injected and hands it to the [`UpgradeHandler`].
#[derive(Clone)]
pub struct ListenerService {
    handler: Arc<UpgradeHandler>,
}

impl tower_service::Service<Request<Incoming>> for ListenerService {
    type Response = Response<Full<Bytes>>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Incoming>) -> Self::Future {
        let peer = peer_of(&request);
        let handler = Arc::clone(&self.handler);
        Box::pin(async move { Ok(handler.handle(request, peer).await) })
    }
}

/// The own-cloud session listener.
pub struct NodeListener {
    server: NodeTransportServer,
    handler: Arc<UpgradeHandler>,
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
            server,
            handler: Arc::new(UpgradeHandler::new(host, mapper, limits)),
        }
    }

    /// The per-request handler.
    #[must_use]
    pub fn handler(&self) -> &Arc<UpgradeHandler> {
        &self.handler
    }

    /// A revoker whose live-connection table is this listener's, so
    /// revoking a device closes its open connections.
    #[must_use]
    pub fn revoker(&self) -> HostRevoker {
        HostRevoker::new(Arc::clone(&self.handler.host)).with_live(self.handler.live.clone())
    }

    /// Serve `listener` until `shutdown` completes; shutdown closes every
    /// live connection.
    ///
    /// # Errors
    /// [`ListenerError::NotImplemented`] while the transport upgrade (S02)
    /// is a stub; [`ListenerError::Io`] for other transport failures.
    pub async fn serve<F>(&self, listener: TcpListener, shutdown: F) -> Result<(), ListenerError>
    where
        F: Future<Output = ()>,
    {
        let live = self.handler.live.clone();
        let shutdown = async move {
            shutdown.await;
            live.close_all();
        };
        let service = ListenerService {
            handler: Arc::clone(&self.handler),
        };
        self.server
            .serve_upgradable(listener, service, shutdown)
            .await
            .map_err(|error| match error {
                UpgradeError::NotImplemented(what) => ListenerError::NotImplemented(what),
                other => ListenerError::Io(other.to_string()),
            })
    }
}

