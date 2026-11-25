//! Transport-owned provenance for the Root-Hub owner-process observation proof.
//! The node handshake proves the channel's node, not a distinct owner process.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use harw_types::NodeId;
use http::Request;

use crate::handshake::{AuthenticatedPeer, CHANNEL_BINDING_LEN, Role};

struct ChannelState {
    exporter: [u8; CHANNEL_BINDING_LEN],
    peer_key_fingerprint: Option<[u8; 32]>,
    revoked: AtomicBool,
}

/// A connection-bound owner-process exporter witness, minted only by the
/// transport after pinned node authentication. It is not wire data and does
/// not authorize an owner observation or identify the owner process by itself.
/// A clone does not retain the live connection.
#[derive(Clone)]
pub struct OwnerProcessChannelBinding {
    peer: AuthenticatedPeer,
    role: Role,
    state: Weak<ChannelState>,
}

impl fmt::Debug for OwnerProcessChannelBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnerProcessChannelBinding")
            .field("peer", &self.peer)
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

impl OwnerProcessChannelBinding {
    /// Identity verified on this exact connection, not from a request header.
    #[must_use]
    pub fn peer(&self) -> &AuthenticatedPeer {
        &self.peer
    }

    /// Whether this process dialled the peer, rather than accepting its request.
    /// Fixed by the transport, not by the remote node.
    #[must_use]
    pub fn is_dialer(&self) -> bool {
        self.role == Role::Client
    }

    /// Whether the local HTTP connection is still running without withdrawal.
    /// This does not prove current enrollment or application authorization.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.state
            .upgrade()
            .is_some_and(|state| !state.revoked.load(Ordering::Acquire))
    }

    /// Compare two handles in this process, never the node label alone.
    /// This is a point-in-time check; recheck before releasing a proof result.
    #[must_use]
    pub fn same_live_channel(&self, other: &Self) -> bool {
        self.state.ptr_eq(&other.state) && self.is_live()
    }

    /// Return the proof-specific TLS exporter only for the exact authenticated
    /// peer and while the local connection is live. Do not cache it as an
    /// authorization or supply bytes from a network message instead.
    #[must_use]
    pub fn export_for_peer(&self, expected: &NodeId) -> Option<[u8; CHANNEL_BINDING_LEN]> {
        if &self.peer.node_id != expected {
            return None;
        }
        let state = self.state.upgrade()?;
        if state.revoked.load(Ordering::Acquire) || state.peer_key_fingerprint.is_none() {
            return None;
        }
        Some(state.exporter)
    }

    /// Full BLAKE3 of the raw ML-DSA key that verified the remote signature
    /// in this exact handshake. Available for either role with sealed pin
    /// provenance, never after close or upgrade. Verify the local role
    /// separately, then check the fingerprint against current authorization.
    #[must_use]
    pub fn verified_peer_key_fingerprint(&self) -> Option<[u8; 32]> {
        let state = self.state.upgrade()?;
        if state.revoked.load(Ordering::Acquire) {
            return None;
        }
        state.peer_key_fingerprint
    }

    pub(crate) fn revoke(&self) {
        if let Some(state) = self.state.upgrade() {
            state.revoked.store(true, Ordering::Release);
        }
    }
}

/// Held only by the task driving the authenticated HTTP connection. Dropping
/// this guard invalidates all witnesses, including those in detached handlers.
pub(crate) struct LiveOwnerProcessChannel(Arc<ChannelState>);

impl LiveOwnerProcessChannel {
    pub(crate) fn new(
        peer: AuthenticatedPeer,
        exporter: [u8; CHANNEL_BINDING_LEN],
        role: Role,
        peer_key_fingerprint: Option<[u8; 32]>,
    ) -> (Self, OwnerProcessChannelBinding) {
        let state = Arc::new(ChannelState {
            exporter,
            peer_key_fingerprint,
            revoked: AtomicBool::new(false),
        });
        let binding = OwnerProcessChannelBinding {
            peer,
            role,
            state: Arc::downgrade(&state),
        };
        (Self(state), binding)
    }
}

impl Drop for LiveOwnerProcessChannel {
    fn drop(&mut self) {
        self.0.revoked.store(true, Ordering::Release);
    }
}

// Separate from the public copyable peer extension and from client witnesses.
// Only the server's injection middleware can install this type.
#[derive(Clone)]
pub(crate) struct ServerOwnerProcessBinding(pub(crate) OwnerProcessChannelBinding);

/// The transport-owned witness of a server request while its authenticated
/// HTTP connection is live and used a sealed pin verifier. No header or
/// public peer extension is read.
#[must_use]
pub fn owner_process_channel_binding_of<B>(
    request: &Request<B>,
) -> Option<OwnerProcessChannelBinding> {
    let binding = &request.extensions().get::<ServerOwnerProcessBinding>()?.0;
    binding.verified_peer_key_fingerprint().map(|_| binding.clone())
}
