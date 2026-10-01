//! Connecting, upgrade, hello and request correlation (S06).
//!
//! Skeleton: signatures are frozen, bodies answer
//! [`RemoteError::NotImplemented`].

use std::net::SocketAddr;
use std::path::PathBuf;

use harw_node_transport::{LocalNode, NodeVerifier};
use harw_protocol::ClientCaps;
use harw_session_ws::WsLimits;
use harw_types::NodeId;

use crate::RemoteError;

/// Where a host is reached. An endpoint is a locator only; identity comes
/// from the transport (`SO_PEERCRED` on the host side for UDS, the pinned
/// node key for the node transport).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// Local AF_UNIX socket path.
    Unix(PathBuf),
    /// Own-cloud node.
    Node(NodeEndpoint),
}

/// Own-cloud node locator plus the node identity it must prove.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeEndpoint {
    /// Where to dial (not identity).
    pub addr: SocketAddr,
    /// Node that must authenticate there (pinned key via the verifier).
    pub expected_node: NodeId,
}

/// Options shared by every connect path.
#[derive(Clone, Debug)]
pub struct ConnectOptions {
    /// Presence label sent in `session.hello` (audit/display only).
    pub client_label: String,
    /// Caps to request; `None` asks for the full ceiling (can only narrow).
    pub requested_caps: Option<ClientCaps>,
    /// Client-side WebSocket limits.
    pub limits: WsLimits,
}

impl ConnectOptions {
    /// Defaults for `client_label`.
    #[must_use]
    pub fn new(client_label: impl Into<String>) -> Self {
        Self {
            client_label: client_label.into(),
            requested_caps: None,
            limits: WsLimits::default(),
        }
    }
}

/// An upgraded, hello-completed connection. Owned by [`crate::RemotePort`].
#[derive(Debug)]
pub struct RemoteConnection {
    _private: (),
}

/// Connect to a local host socket, upgrade and complete `session.hello`.
///
/// # Errors
/// [`RemoteError::NotImplemented`] in the skeleton.
pub async fn connect_unix(
    path: PathBuf,
    options: ConnectOptions,
) -> Result<RemoteConnection, RemoteError> {
    let _ = (path, options);
    Err(RemoteError::NotImplemented("connect_unix (S06)"))
}

/// Connect to an own-cloud node over the authenticated node transport,
/// upgrade (via `NodeTransportClient::upgrade`, S02) and complete
/// `session.hello`.
///
/// # Errors
/// [`RemoteError::NotImplemented`] in the skeleton.
pub async fn connect_node(
    endpoint: NodeEndpoint,
    local: &LocalNode,
    verifier: &dyn NodeVerifier,
    options: ConnectOptions,
) -> Result<RemoteConnection, RemoteError> {
    let _ = (endpoint, local, verifier, options);
    Err(RemoteError::NotImplemented("connect_node (S06)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn skeleton_connect_is_typed_not_implemented() {
        let result = connect_unix(PathBuf::from("/nonexistent"), ConnectOptions::new("t")).await;
        assert!(matches!(result, Err(RemoteError::NotImplemented(_))));
    }
}
