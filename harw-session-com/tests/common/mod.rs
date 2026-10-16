//! Shared test helpers: deterministic ML-DSA test nodes for the node transport.

#![allow(dead_code)] // Each test binary uses a subset.

use std::sync::Arc;

use aws_lc_rs::signature::{KeyPair, ML_DSA_65_SIGNING, PqdsaKeyPair};
use harw_node_transport::{
    LocalNode, ML_DSA_65_SIGNATURE_LEN, NodeIdentity, NodeSigner, PinnedPeers, SignFuture,
};
use harw_types::NodeId;

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// **TEST ONLY.** Deterministic ML-DSA-65 key from a seed.
pub struct SeedSigner {
    key: PqdsaKeyPair,
}

impl SeedSigner {
    fn new(seed: u8) -> TestResult<Self> {
        Ok(Self {
            key: PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, &[seed; 32])
                .map_err(|_| "ml-dsa seed")?,
        })
    }
}

impl NodeSigner for SeedSigner {
    fn sign<'a>(&'a self, transcript: &'a [u8]) -> SignFuture<'a> {
        let mut signature = vec![0u8; ML_DSA_65_SIGNATURE_LEN];
        let result = self
            .key
            .sign(transcript, &mut signature)
            .map(|n| {
                signature.truncate(n);
                signature
            })
            .map_err(|_| harw_node_transport::SignerError::new("test signer failed"));
        Box::pin(std::future::ready(result))
    }
}

pub struct Node {
    pub id: NodeId,
    pub identity: NodeIdentity,
    pub local: LocalNode,
}

pub fn node(name: &str, seed: u8) -> TestResult<Node> {
    let signer = SeedSigner::new(seed)?;
    let id = NodeId::try_from_str(name)?;
    let identity = NodeIdentity {
        node_id: id.clone(),
        public_key: signer.key.public_key().as_ref().to_vec(),
        key_ref: format!("test-only/{name}"),
    };
    let local = LocalNode::new(identity.clone(), Arc::new(signer));
    Ok(Node {
        id,
        identity,
        local,
    })
}

pub fn pinned(nodes: &[&NodeIdentity]) -> TestResult<PinnedPeers> {
    let mut peers = PinnedPeers::new();
    for n in nodes {
        peers.pin_identity(n)?;
    }
    Ok(peers)
}
