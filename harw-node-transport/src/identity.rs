//! Node identities, the signer seam and the pinned-key verifier.
//!
//! The long-term node key is an ML-DSA-65 key held by the Auth/Crypto Hub
//! (masterplan §13, §28). This crate never sees the private half: it asks a
//! [`NodeSigner`] to sign a handshake transcript and checks the peer's
//! signature with a [`NodeVerifier`]. The production adapter implements
//! [`NodeSigner`] on top of the AuthHub client; [`PinnedPeers`] is the
//! verifier over an explicit trust store of pinned public keys.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use aws_lc_rs::signature::{ML_DSA_65, UnparsedPublicKey};
use harw_types::NodeId;
use serde::{Deserialize, Serialize};

use crate::error::{PinError, SignerError, VerifyError};

/// Length of a raw (FIPS 204) ML-DSA-65 public key.
pub const ML_DSA_65_PUBLIC_KEY_LEN: usize = 1952;

/// Length of an ML-DSA-65 signature.
pub const ML_DSA_65_SIGNATURE_LEN: usize = 3309;

/// The public identity of one node.
///
/// `public_key` is the raw FIPS 204 ML-DSA-65 public key (1952 bytes);
/// `key_ref` names the private key inside the AuthHub (never the key itself).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeIdentity {
    /// The node this key belongs to.
    pub node_id: NodeId,
    /// Raw ML-DSA-65 public key.
    pub public_key: Vec<u8>,
    /// AuthHub key reference of the private half.
    pub key_ref: String,
}

impl fmt::Debug for NodeIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeIdentity")
            .field("node_id", &self.node_id)
            .field("public_key_len", &self.public_key.len())
            .field("key_ref", &self.key_ref)
            .finish()
    }
}

/// Future returned by [`NodeSigner::sign`].
pub type SignFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<u8>, SignerError>> + Send + 'a>>;

/// Signs node-transport handshake transcripts with the node's long-term key.
///
/// Every payload handed to [`NodeSigner::sign`] starts with
/// [`crate::TRANSCRIPT_DOMAIN`]. An AuthHub adapter should enforce that
/// prefix in its key-usage policy so the node-identity key can not be used
/// as a general signing oracle (masterplan §5).
pub trait NodeSigner: Send + Sync {
    /// Signs `transcript` and returns the ML-DSA-65 signature.
    fn sign<'a>(&'a self, transcript: &'a [u8]) -> SignFuture<'a>;
}

/// Verifies a peer's transcript signature against a trust store.
pub trait NodeVerifier: Send + Sync {
    /// Whether `node` has a pinned key at all. Used to refuse unknown
    /// clients before the server spends a signature on them.
    fn is_known(&self, node: &NodeId) -> bool;

    /// Verifies `signature` over `transcript` under the key pinned for `node`.
    fn verify(&self, node: &NodeId, transcript: &[u8], signature: &[u8])
    -> Result<(), VerifyError>;
}

/// The local node: its public identity plus the signer for its private key.
#[derive(Clone)]
pub struct LocalNode {
    /// Public identity.
    pub identity: NodeIdentity,
    /// Signer for the private key named by `identity.key_ref`.
    pub signer: Arc<dyn NodeSigner>,
}

impl LocalNode {
    /// Bundles an identity with its signer.
    #[must_use]
    pub fn new(identity: NodeIdentity, signer: Arc<dyn NodeSigner>) -> Self {
        Self { identity, signer }
    }

    /// This node's id.
    #[must_use]
    pub fn node_id(&self) -> &NodeId {
        &self.identity.node_id
    }
}

impl fmt::Debug for LocalNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalNode")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

/// Trust store: node id → pinned ML-DSA-65 public key.
///
/// Identity comes only from this map. Neither the peer's IP address nor
/// the (ephemeral, unauthenticated) TLS key is ever used as identity
/// (masterplan §28, H11 exit criterion).
#[derive(Clone, Default)]
pub struct PinnedPeers {
    keys: HashMap<NodeId, Vec<u8>>,
}

impl PinnedPeers {
    /// An empty trust store: every peer is unknown.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pins `public_key` for `node`, replacing an earlier pin.
    ///
    /// # Errors
    /// [`PinError`] when the key is not an ML-DSA-65 public key by length.
    pub fn pin(&mut self, node: NodeId, public_key: Vec<u8>) -> Result<(), PinError> {
        if public_key.len() != ML_DSA_65_PUBLIC_KEY_LEN {
            return Err(PinError {
                node,
                expected: ML_DSA_65_PUBLIC_KEY_LEN,
                actual: public_key.len(),
            });
        }
        self.keys.insert(node, public_key);
        Ok(())
    }

    /// Pins the public key of `identity`.
    ///
    /// # Errors
    /// See [`Self::pin`].
    pub fn pin_identity(&mut self, identity: &NodeIdentity) -> Result<(), PinError> {
        self.pin(identity.node_id.clone(), identity.public_key.clone())
    }

    /// Removes the pin for `node` (revocation). Returns whether one existed.
    pub fn unpin(&mut self, node: &NodeId) -> bool {
        self.keys.remove(node).is_some()
    }

    /// Number of pinned nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether no node is pinned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

impl fmt::Debug for PinnedPeers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut nodes: Vec<&str> = self.keys.keys().map(NodeId::as_str).collect();
        nodes.sort_unstable();
        f.debug_struct("PinnedPeers")
            .field("nodes", &nodes)
            .finish()
    }
}

impl NodeVerifier for PinnedPeers {
    fn is_known(&self, node: &NodeId) -> bool {
        self.keys.contains_key(node)
    }

    fn verify(
        &self,
        node: &NodeId,
        transcript: &[u8],
        signature: &[u8],
    ) -> Result<(), VerifyError> {
        let Some(key) = self.keys.get(node) else {
            return Err(VerifyError::UnknownPeer(node.clone()));
        };
        if signature.len() != ML_DSA_65_SIGNATURE_LEN {
            return Err(VerifyError::BadSignature(node.clone()));
        }
        UnparsedPublicKey::new(&ML_DSA_65, key.as_slice())
            .verify(transcript, signature)
            .map_err(|_| VerifyError::BadSignature(node.clone()))
    }
}
