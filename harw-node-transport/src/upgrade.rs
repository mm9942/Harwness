//! HTTP/1 upgrade over the authenticated node channel (W00 §2.3, W02-01/02,
//! S02 entry points).
//!
//! Skeleton: signatures only; every body answers
//! [`UpgradeError::NotImplemented`]. S02 implements them in this file plus
//! `server.rs` / `client.rs` and keeps the public signatures.
//!
//! Contract (W00 D4, ID-02):
//! - the [`AuthenticatedPeer`] is finalized before HTTP is served and is
//!   injected as a request extension before the upgrade request reaches the
//!   service;
//! - the upgraded I/O keeps an immutable copy of that identity
//!   ([`UpgradedServerIo::peer`]);
//! - the client verifies the expected node before it can return an upgraded
//!   stream ([`NodeTransportClient::upgrade`]).
//!
//! This module is protocol-agnostic: it knows nothing about WebSocket. The
//! session layer (`harw-node-listener`, `harw-session-remote`) wraps the
//! [`UpgradedIo`] with `harw-session-ws`.

use std::error::Error as StdError;
use std::future::Future;

use http::{Request, Response};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use crate::client::{NodeBody, NodeTransportClient};
use crate::handshake::AuthenticatedPeer;
use crate::server::NodeTransportServer;

/// Raw duplex I/O of an upgraded connection (after the HTTP/1 `101`).
pub type UpgradedIo = TokioIo<hyper::upgrade::Upgraded>;

/// Why an upgrade entry point failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UpgradeError {
    /// Skeleton stub: not implemented yet (S02).
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
    /// The request carries no [`AuthenticatedPeer`] extension (a service was
    /// mounted outside the node transport server). Fail closed.
    #[error("request has no authenticated peer")]
    MissingPeer,
    /// The server answered something other than `101 Switching Protocols`.
    #[error("peer did not switch protocols (status {0})")]
    NotSwitched(u16),
    /// The HTTP/1 upgrade itself failed.
    #[error("http upgrade: {0}")]
    Http(#[from] hyper::Error),
}

/// The server side of one upgraded connection: raw I/O plus the immutable,
/// already-authenticated identity (never re-derived from the stream).
#[derive(Debug)]
pub struct UpgradedServerIo {
    peer: AuthenticatedPeer,
    io: UpgradedIo,
}

impl UpgradedServerIo {
    /// The peer authenticated by the node handshake before HTTP was served.
    #[must_use]
    pub fn peer(&self) -> &AuthenticatedPeer {
        &self.peer
    }

    /// Split into identity and raw I/O.
    #[must_use]
    pub fn into_parts(self) -> (AuthenticatedPeer, UpgradedIo) {
        (self.peer, self.io)
    }
}

/// The client side of one upgraded connection.
#[derive(Debug)]
pub struct UpgradedClientIo {
    /// The remote node, verified against the expected node before upgrade.
    pub peer: AuthenticatedPeer,
    /// The server's `101` response (headers such as `Sec-WebSocket-Accept`
    /// are checked by the protocol layer, not here).
    pub response: Response<Incoming>,
    /// Raw duplex I/O after the upgrade.
    pub io: UpgradedIo,
}

/// The authenticated peer of `request` (injected by the node transport
/// server before the service sees the request).
///
/// # Errors
/// [`UpgradeError::MissingPeer`] when absent; [`UpgradeError::NotImplemented`]
/// in the skeleton.
pub fn peer_of<B>(request: &Request<B>) -> Result<AuthenticatedPeer, UpgradeError> {
    let _ = request;
    Err(UpgradeError::NotImplemented("peer_of (S02)"))
}

/// Complete the HTTP/1 upgrade of `request` and return the raw I/O together
/// with the identity the server authenticated before serving HTTP. Call it
/// from the service after answering `101`.
///
/// # Errors
/// [`UpgradeError::MissingPeer`], [`UpgradeError::Http`];
/// [`UpgradeError::NotImplemented`] in the skeleton.
pub async fn accept_upgrade(request: Request<Incoming>) -> Result<UpgradedServerIo, UpgradeError> {
    let _ = request;
    Err(UpgradeError::NotImplemented("accept_upgrade (S02)"))
}

impl NodeTransportServer {
    /// Like [`NodeTransportServer::serve_with_shutdown`] but the Hyper
    /// connection runs `with_upgrades`, so a service can answer `101` and
    /// call [`accept_upgrade`]. Existing [`NodeTransportServer::serve`]
    /// behaviour is unchanged.
    ///
    /// # Errors
    /// Listener failures; [`UpgradeError::NotImplemented`] in the skeleton.
    pub async fn serve_upgradable<S, B, F>(
        &self,
        listener: TcpListener,
        service: S,
        shutdown: F,
    ) -> Result<(), UpgradeError>
    where
        S: tower_service::Service<Request<Incoming>, Response = Response<B>>
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<Box<dyn StdError + Send + Sync>>,
        B: http_body::Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn StdError + Send + Sync>>,
        F: Future<Output = ()>,
    {
        let _ = (listener, service, shutdown);
        Err(UpgradeError::NotImplemented("serve_upgradable (S02)"))
    }
}

impl NodeTransportClient {
    /// Send `request` (an upgrade request), require `101`, and return the
    /// upgraded I/O. The connection was already bound to the expected node by
    /// [`NodeTransportClient::connect`]; no upgraded stream is returned for
    /// any other peer.
    ///
    /// # Errors
    /// [`UpgradeError::NotSwitched`], [`UpgradeError::Http`];
    /// [`UpgradeError::NotImplemented`] in the skeleton.
    pub async fn upgrade(
        self,
        request: Request<NodeBody>,
    ) -> Result<UpgradedClientIo, UpgradeError> {
        let _ = request;
        Err(UpgradeError::NotImplemented(
            "NodeTransportClient::upgrade (S02)",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_is_typed_not_implemented() {
        let request = Request::new(());
        assert!(matches!(
            peer_of(&request),
            Err(UpgradeError::NotImplemented(_))
        ));
    }
}
