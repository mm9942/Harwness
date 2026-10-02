//! HTTP/1 upgrade over the authenticated node channel (W00 §2.3, W02-01/02,
//! S02 entry points).
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
use crate::error::TransportError;
use crate::handshake::AuthenticatedPeer;
use crate::server::NodeTransportServer;

/// Raw duplex I/O of an upgraded connection (after the HTTP/1 `101`).
pub type UpgradedIo = TokioIo<hyper::upgrade::Upgraded>;

/// Why an upgrade entry point failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UpgradeError {
    /// Retained from the skeleton so the public enum does not shrink; no
    /// entry point returns it any more.
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
    /// The underlying node transport failed (listener setup, handshake).
    #[error("node transport: {0}")]
    Transport(#[from] TransportError),
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
/// The value comes only from the request extension the server overwrites for
/// every request; no header is ever consulted.
///
/// # Errors
/// [`UpgradeError::MissingPeer`] when absent.
pub fn peer_of<B>(request: &Request<B>) -> Result<AuthenticatedPeer, UpgradeError> {
    request
        .extensions()
        .get::<AuthenticatedPeer>()
        .cloned()
        .ok_or(UpgradeError::MissingPeer)
}

/// Complete the HTTP/1 upgrade of `request` and return the raw I/O together
/// with the identity the server authenticated before serving HTTP. Call it
/// from the service after answering `101` (typically on a spawned task, since
/// it resolves only once the `101` response has been written).
///
/// # Errors
/// [`UpgradeError::MissingPeer`] (checked first, fail closed),
/// [`UpgradeError::Http`] when the upgrade does not complete.
pub async fn accept_upgrade(request: Request<Incoming>) -> Result<UpgradedServerIo, UpgradeError> {
    let peer = peer_of(&request)?;
    let upgraded = hyper::upgrade::on(request).await?;
    Ok(UpgradedServerIo {
        peer,
        io: TokioIo::new(upgraded),
    })
}

impl NodeTransportServer {
    /// Like [`NodeTransportServer::serve_with_shutdown`] but the Hyper
    /// connection runs `with_upgrades`, so a service can answer `101` and
    /// call [`accept_upgrade`]. Existing [`NodeTransportServer::serve`]
    /// behaviour is unchanged.
    ///
    /// # Errors
    /// Listener failures ([`UpgradeError::Transport`]).
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
        Ok(self.serve_loop(listener, service, shutdown, true).await?)
    }
}

impl NodeTransportClient {
    /// Send `request` (an upgrade request), require `101`, and return the
    /// upgraded I/O. The connection was already bound to the expected node by
    /// [`NodeTransportClient::connect`]; no upgraded stream is returned for
    /// any other peer.
    ///
    /// # Errors
    /// [`UpgradeError::NotSwitched`] for any status other than `101`,
    /// [`UpgradeError::Http`] for HTTP or upgrade failures.
    pub async fn upgrade(
        mut self,
        request: Request<NodeBody>,
    ) -> Result<UpgradedClientIo, UpgradeError> {
        let (peer, mut response) = self.send_for_upgrade(request).await?;
        if response.status() != http::StatusCode::SWITCHING_PROTOCOLS {
            return Err(UpgradeError::NotSwitched(response.status().as_u16()));
        }
        // Dropping `self` afterwards aborts the connection task, which has by
        // then handed the raw I/O over to `upgraded`.
        let upgraded = hyper::upgrade::on(&mut response).await?;
        Ok(UpgradedClientIo {
            peer,
            response,
            io: TokioIo::new(upgraded),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_of_without_extension_fails_closed() {
        let request = Request::new(());
        assert!(matches!(peer_of(&request), Err(UpgradeError::MissingPeer)));
    }

    #[test]
    fn peer_of_ignores_identity_headers() {
        let mut request = Request::new(());
        request
            .headers_mut()
            .insert("x-node-id", http::HeaderValue::from_static("forged"));
        assert!(matches!(peer_of(&request), Err(UpgradeError::MissingPeer)));
    }
}
