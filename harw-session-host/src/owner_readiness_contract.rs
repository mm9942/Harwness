//! The fixed pre-assignment readiness proof format. This contract grants
//! nothing on its own. Trusted composition must verify the exact receipt,
//! independent channel and live activation before consuming a proof.

use std::num::NonZeroU64;

use harw_types::NodeId;
use serde::{Deserialize, Serialize};

use crate::HostError;
use crate::owner_creation::{
    ML_DSA_65_SIGNATURE_LEN, OwnerCreationClaims, OwnerCreationReceipt,
};

const READINESS_DOMAIN: &[u8] = b"HARW-OWNER-READINESS-V2\0";
const RECEIPT_DIGEST_DOMAIN: &[u8] = b"HARW-OWNER-CREATION-RECEIPT-DIGEST-V1\0";

/// Fixed proof-only HTTP endpoint, separate from assignment and Observe.
pub const OWNER_READINESS_PATH: &str = "/v2/owner-creation-readiness";
pub const MAX_OWNER_READINESS_REQUEST_BYTES: usize = 1024;
pub const MAX_OWNER_READINESS_RESPONSE_BYTES: usize = 16 * 1024;

/// Fixed request with no arbitrary node, tenant, session or address selector.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerReadinessChallenge {
    pub nonce: [u8; 32],
    pub receipt_digest: [u8; 32],
    pub netsec_epoch: NonZeroU64,
}

/// Fixed output. No owner, liveness estimation, transcript, or coordinates
/// are returned in cleartext by this operation.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerReadinessResponse {
    pub signature: Vec<u8>,
}

fn invalid() -> HostError {
    HostError::Denied("invalid owner creation readiness proof scope".into())
}

/// Canonical digest of the exact claims and signature; JSON key ordering,
/// whitespace and wire encoding are not part of the digest.
pub fn owner_creation_receipt_digest(
    receipt: &OwnerCreationReceipt,
) -> Result<[u8; 32], HostError> {
    canonical_receipt_digest(receipt.claims(), receipt.signature())
}

pub(crate) fn canonical_receipt_digest(
    claims: &OwnerCreationClaims,
    signature: &[u8],
) -> Result<[u8; 32], HostError> {
    if signature.len() != ML_DSA_65_SIGNATURE_LEN {
        return Err(invalid());
    }
    let canonical = claims.signing_bytes()?;
    let mut digest = blake3::Hasher::new();
    digest.update(RECEIPT_DIGEST_DOMAIN);
    digest.update(&u16::try_from(canonical.len()).map_err(|_| invalid())?.to_be_bytes());
    digest.update(&canonical);
    digest.update(signature);
    Ok(*digest.finalize().as_bytes())
}

impl OwnerReadinessChallenge {
    /// Signature input, using the producer's real current receipt and the
    /// exporter and Hub identity obtained from the exact transport channel.
    /// Neither peer's wire data establishes this channel provenance. Each
    /// endpoint independently reads the NetSec authority marker from its
    /// authenticated local daemon, never from the peer's HTTP message.
    pub fn signing_bytes(
        &self,
        hub_node: &NodeId,
        hub_key_fingerprint: [u8; 32],
        exporter: [u8; 32],
        netsec_authority_instance: [u8; 32],
        claims: &OwnerCreationClaims,
        readiness_key_fingerprint: [u8; 32],
    ) -> Result<Vec<u8>, HostError> {
        let hub = hub_node.as_str();
        if hub == claims.node.as_str() || hub.is_empty() || hub.len() > 128
            || hub.starts_with('.')
            || !hub.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
            })
            || [
                self.nonce, self.receipt_digest, hub_key_fingerprint, exporter,
                readiness_key_fingerprint, netsec_authority_instance,
            ].contains(&[0; 32])
            || readiness_key_fingerprint == claims.key_fingerprint
            || readiness_key_fingerprint == hub_key_fingerprint
        {
            return Err(invalid());
        }
        let canonical = claims.signing_bytes()?;
        let mut output = Vec::from(READINESS_DOMAIN);
        output.extend_from_slice(&u16::try_from(hub.len()).map_err(|_| invalid())?.to_be_bytes());
        output.extend_from_slice(hub.as_bytes());
        output.extend_from_slice(&hub_key_fingerprint);
        output.extend_from_slice(&exporter);
        output.extend_from_slice(&netsec_authority_instance);
        output.extend_from_slice(&u16::try_from(canonical.len()).map_err(|_| invalid())?.to_be_bytes());
        output.extend_from_slice(&canonical);
        output.extend_from_slice(&self.receipt_digest);
        output.extend_from_slice(&self.netsec_epoch.get().to_be_bytes());
        output.extend_from_slice(&self.nonce);
        output.extend_from_slice(&readiness_key_fingerprint);
        Ok(output)
    }
}
