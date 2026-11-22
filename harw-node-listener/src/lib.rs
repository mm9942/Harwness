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
//! S08 alone. [`StreamIntake`] (also in `lib.rs`) feeds already-accepted
//! byte streams (onion) into the same service.

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
use harw_protocol::session_wire::{GatewayListenerInfo, ListenerKind};
use harw_session_host::{ConnectionId, HostError, SessionHost};
use harw_session_ws::dispatch::Ports;
use harw_session_ws::{WsLimits, serve_connection_with, upgrade};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Empty, Full, Limited};
use hyper::body::Incoming;
use tokio::net::TcpListener;

pub use identity::{
    DeviceRecord, DeviceRegistry, IdentityMapper, MapFuture, RegistryIdentityMapper, RegistryScan,
    RejectedLine, TierChange, TierChangeError,
};
pub use revoke::{HostRevoker, LiveConnections, RevocationReport, RevocationSink};

const NODE_LISTENER: &str = "node";

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

const PROVIDER_LINK_HEADER: &str = "x-harw-provider-link";
const PROVIDER_LINK_UNAVAILABLE: &str = "unavailable";

/// 404 with an empty body and a marker header: this listener has no provider link configured.
fn provider_link_unavailable() -> Response<Full<Bytes>> {
    let mut response = status(StatusCode::NOT_FOUND, "");
    response.headers_mut().insert(
        http::HeaderName::from_static(PROVIDER_LINK_HEADER),
        http::HeaderValue::from_static(PROVIDER_LINK_UNAVAILABLE),
    );
    response
}

impl UpgradeHandler {
    /// Handler admitting through `host` with identities from `mapper`.
    #[must_use]
    pub fn new(host: Arc<SessionHost>, mapper: Arc<dyn IdentityMapper>, limits: WsLimits) -> Self {
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

/// Owner-authorized account operations on the authenticated node listener.
pub trait ProviderLink: Send + Sync {
    /// Check the operator's device grant and apply a bounded account request.
    fn handle<'a>(
        &'a self,
        identity: harw_session_host::ClientIdentity,
        method: http::Method,
        body: Bytes,
    ) -> Pin<Box<dyn Future<Output = StatusCode> + Send + 'a>>;
}

/// Tower service mounted on the node transport: takes the peer the transport
/// injected and hands it to the [`UpgradeHandler`].
#[derive(Clone)]
pub struct ListenerService {
    provider_link: Option<Arc<dyn ProviderLink>>,
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
        if self.handler.host.listener_enabled(NODE_LISTENER) != Some(true) {
            return Box::pin(async {
                Ok(status(StatusCode::SERVICE_UNAVAILABLE, "listener disabled"))
            });
        }
        let peer = peer_of(&request);
        let handler = Arc::clone(&self.handler);
        if request.uri().path() == "/v1/provider-link/chatgpt" {
            let link = self.provider_link.clone();
            return Box::pin(async move {
                let Some(link) = link else {
                    return Ok(provider_link_unavailable());
                };
                let Ok(peer) = peer else {
                    return Ok(status(StatusCode::NOT_FOUND, ""));
                };
                let (parts, body) = request.into_parts();
                if !matches!(
                    parts.method,
                    http::Method::POST | http::Method::GET | http::Method::DELETE
                ) {
                    return Ok(status(StatusCode::METHOD_NOT_ALLOWED, ""));
                }
                let collect = Limited::new(body, 64 * 1024).collect();
                let body =
                    match tokio::time::timeout(std::time::Duration::from_secs(5), collect).await {
                        Ok(Ok(collected)) => collected.to_bytes(),
                        Ok(Err(_)) => return Ok(status(StatusCode::PAYLOAD_TOO_LARGE, "")),
                        Err(_) => return Ok(status(StatusCode::REQUEST_TIMEOUT, "")),
                    };
                let connection = ConnectionId::next();
                let Ok(identity) = handler.mapper.map(&peer, connection).await else {
                    return Ok(status(StatusCode::FORBIDDEN, ""));
                };
                if identity.connection != connection || identity.device.is_none() {
                    return Ok(status(StatusCode::FORBIDDEN, ""));
                }
                Ok(status(link.handle(identity, parts.method, body).await, ""))
            });
        }
        Box::pin(async move { Ok(handler.handle(request, peer).await) })
    }
}

/// The own-cloud session listener.
pub struct NodeListener {
    provider_link: Option<Arc<dyn ProviderLink>>,
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
            provider_link: None,
            handler: Arc::new(UpgradeHandler::new(host, mapper, limits)),
        }
    }

    /// Enable account requests on this listener with a device-specific owner policy.
    #[must_use]
    pub fn with_provider_link(mut self, link: Arc<dyn ProviderLink>) -> Self {
        self.provider_link = Some(link);
        self
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
        let addr = listener
            .local_addr()
            .map_err(|error| ListenerError::Io(error.to_string()))?;
        self.handler.host.register_listener(GatewayListenerInfo {
            name: NODE_LISTENER.to_owned(),
            kind: ListenerKind::Node,
            address: addr.to_string(),
            enabled: true,
        });
        let live = self.handler.live.clone();
        let shutdown = async move {
            shutdown.await;
            live.close_all();
        };
        let service = ListenerService {
            provider_link: self.provider_link.clone(),
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

    /// A handle that serves already-accepted byte streams (for example an
    /// onion-service stream) through this listener's service.
    #[must_use]
    pub fn intake(&self) -> StreamIntake {
        StreamIntake {
            server: self.server.clone(),
            service: ListenerService {
                provider_link: self.provider_link.clone(),
                handler: Arc::clone(&self.handler),
            },
        }
    }
}

/// Serves already-accepted byte streams (onion) with the same node handshake,
/// identity mapping and live-connection table as [`NodeListener::serve`].
/// Revocation and the shutdown of `serve` close these connections too, and the
/// `node` listener_enabled gate applies, so `NodeListener::serve` must be
/// running (it registers the listener); otherwise requests get 503.
#[derive(Clone)]
pub struct StreamIntake {
    server: NodeTransportServer,
    service: ListenerService,
}

impl StreamIntake {
    /// Serve one connection over `io` until it ends.
    ///
    /// # Errors
    /// [`ListenerError::NotImplemented`] while the transport upgrade is a
    /// stub; [`ListenerError::Io`] for other transport failures.
    pub async fn serve<I>(
        &self,
        io: I,
        handshake_timeout: std::time::Duration,
    ) -> Result<(), ListenerError>
    where
        I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        self.server
            .serve_upgradable_io(io, self.service.clone(), handshake_timeout)
            .await
            .map_err(|error| match error {
                UpgradeError::NotImplemented(what) => ListenerError::NotImplemented(what),
                other => ListenerError::Io(other.to_string()),
            })
    }
}

#[cfg(test)]
mod provider_link_tests {
    use super::*;
    use hyper::body::Body as _;

    #[test]
    fn unavailable_is_404_with_header_and_empty_body() {
        let response = provider_link_unavailable();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.headers().get(PROVIDER_LINK_HEADER),
            Some(&http::HeaderValue::from_static(PROVIDER_LINK_UNAVAILABLE))
        );
        assert!(response.body().is_end_stream());
    }

    #[test]
    fn plain_not_found_has_no_provider_link_header() {
        let response = status(StatusCode::NOT_FOUND, "");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(response.headers().get(PROVIDER_LINK_HEADER).is_none());
    }
}
