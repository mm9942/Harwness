//! Identity mapping: `AuthenticatedPeer` -> host `ClientIdentity` (S08).
//!
//! Never from the wire payload (W00 D4, ID-02, ID-04, ID-05): the mapper
//! reads only the already-authenticated node id and the device registry. The
//! tenant and the cap ceiling are host-fixed per device; remote identities
//! always carry a tenant (`ClientIdentity::validate` refuses otherwise).
//!
//! Skeleton: the registry mapper answers [`ListenerError::NotImplemented`].

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use harw_node_transport::AuthenticatedPeer;
use harw_session_host::{ClientIdentity, ConnectionId};

use crate::ListenerError;

/// Boxed future of one mapping.
pub type MapFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ClientIdentity, ListenerError>> + Send + 'a>>;

/// Maps an authenticated node peer to the identity the host admits against.
pub trait IdentityMapper: Send + Sync {
    /// Resolve `peer` for the new `connection`. Unknown, revoked or
    /// unenrolled peers fail with [`ListenerError::UnknownPeer`] (default
    /// deny); the result's tenant, caps, device and actor are host-derived.
    fn map(&self, peer: &AuthenticatedPeer, connection: ConnectionId) -> MapFuture<'_>;
}

/// Mapper backed by the machine's device registry.
#[derive(Debug)]
pub struct RegistryIdentityMapper {
    _state_dir: std::path::PathBuf,
}

impl RegistryIdentityMapper {
    /// Mapper reading the device registry under `state_dir`.
    #[must_use]
    pub fn new(state_dir: &Path) -> Self {
        Self {
            _state_dir: state_dir.to_path_buf(),
        }
    }
}

impl IdentityMapper for RegistryIdentityMapper {
    fn map(&self, peer: &AuthenticatedPeer, connection: ConnectionId) -> MapFuture<'_> {
        let _ = (peer, connection);
        Box::pin(async {
            Err(ListenerError::NotImplemented(
                "RegistryIdentityMapper::map (S08)",
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn skeleton_is_typed_not_implemented() {
        let mapper = RegistryIdentityMapper::new(Path::new("."));
        let peer = AuthenticatedPeer {
            node_id: harw_types::NodeId::from_str("node-a"),
            protocol_version: 1,
        };
        let result = mapper.map(&peer, ConnectionId::next()).await;
        assert!(matches!(result, Err(ListenerError::NotImplemented(_))));
    }
}
