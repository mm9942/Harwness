//! Verification of a historical, durably published creation receipt.
//!
//! This module does not select an owner, enroll a key, check liveness,
//! authorise a caller, retire a writer, or write a directory. The operator
//! chooses the exact scope and pins keys from a trusted configuration that
//! is independent of the receipt bytes. A valid receipt is replayable
//! evidence of creation, never proof of the current state of the writer.

use std::{fmt, num::NonZeroU64};

use aws_lc_rs::signature::{ML_DSA_65, UnparsedPublicKey};
use harw_session_host::owner_creation::{
    ML_DSA_65_PUBLIC_KEY_LEN, ML_DSA_65_SIGNATURE_LEN, OwnerCreationClaims, OwnerCreationReceipt,
    owner_creation_key_fingerprint,
};

use crate::owner_directory::valid_id;

pub use harw_session_host::owner_creation::MAX_OWNER_CREATION_RECEIPT_WIRE_BYTES;

/// Scope selected by the trusted operator before looking at a receipt.
pub struct OwnerCreationPinScope<'a> {
    pub node: &'a str,
    pub instance: &'a str,
    pub epoch: NonZeroU64,
    pub namespace_id: [u8; 32],
}

/// Observe authority for the exact pinned writer, selected by the operator.
pub enum OwnerCreationObserveKey<'a> {
    /// The isolation-only writer has never been provisioned with an Observe
    /// key. This is a deployment assertion, not something inferred from the
    /// receipt or absence of input.
    Disabled,
    /// Independently pinned raw Observe public key for this writer.
    Pinned(&'a [u8]),
}

/// Offline verification failed. No failure includes receipt contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerCreationVerificationError {
    InvalidPin,
    InvalidReceipt,
    ScopeMismatch,
    InvalidSignature,
}

impl fmt::Display for OwnerCreationVerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidPin => "invalid independent owner creation receipt pin",
            Self::InvalidReceipt => "invalid owner creation receipt encoding",
            Self::ScopeMismatch => "owner creation receipt does not match the exact expected scope",
            Self::InvalidSignature => "owner creation receipt signature did not verify",
        })
    }
}

impl std::error::Error for OwnerCreationVerificationError {}

/// Purpose-specific public key selected by trusted composition. No serde
/// implementation: a peer, receipt, or observed connection cannot supply
/// this trust root.
pub struct OwnerCreationKeyPin {
    node: String,
    instance: String,
    epoch: NonZeroU64,
    namespace_id: [u8; 32],
    public_key: [u8; ML_DSA_65_PUBLIC_KEY_LEN],
    key_fingerprint: [u8; 32],
}

impl OwnerCreationKeyPin {
    /// Bind the operator-selected receipt key to the exact durable store and
    /// incarnation. Compare against known other-purpose keys before use;
    /// no missing policy is inferred to mean Disabled.
    /// Different bytes are only a misuse check; custody and pin selection
    /// remain the responsibility of trusted configuration.
    pub fn new(
        scope: OwnerCreationPinScope<'_>,
        receipt_public_key: &[u8],
        enrolled_node_public_key: &[u8],
        observe_process_key: OwnerCreationObserveKey<'_>,
    ) -> Result<Self, OwnerCreationVerificationError> {
        let public_key = checked_key(receipt_public_key)?;
        let node_key = checked_key(enrolled_node_public_key)?;
        let observe_reuses_a_key = match observe_process_key {
            OwnerCreationObserveKey::Disabled => false,
            OwnerCreationObserveKey::Pinned(bytes) => {
                let key = checked_key(bytes)?;
                key == public_key || key == node_key
            }
        };
        if !valid_id(scope.node)
            || !valid_id(scope.instance)
            || scope.namespace_id == [0; 32]
            || public_key == node_key
            || observe_reuses_a_key
        {
            return Err(OwnerCreationVerificationError::InvalidPin);
        }
        Ok(Self {
            node: scope.node.to_owned(),
            instance: scope.instance.to_owned(),
            epoch: scope.epoch,
            namespace_id: scope.namespace_id,
            key_fingerprint: owner_creation_key_fingerprint(&public_key)
                .ok_or(OwnerCreationVerificationError::InvalidPin)?,
            public_key,
        })
    }

    /// Decode a bounded strict receipt and authenticate its shared canonical
    /// claims. Both tenant and session must come from the caller's expected
    /// operation, not be copied out of the receipt.
    pub fn verify_json(
        &self,
        expected_tenant: &str,
        expected_session: &str,
        bytes: &[u8],
    ) -> Result<VerifiedOwnerCreation<'_>, OwnerCreationVerificationError> {
        if bytes.is_empty() || bytes.len() > MAX_OWNER_CREATION_RECEIPT_WIRE_BYTES {
            return Err(OwnerCreationVerificationError::InvalidReceipt);
        }
        let receipt: OwnerCreationReceipt = serde_json::from_slice(bytes)
            .map_err(|_| OwnerCreationVerificationError::InvalidReceipt)?;
        let claims = receipt.claims();
        if !valid_id(expected_tenant)
            || !valid_id(expected_session)
            || claims.tenant.as_str() != expected_tenant
            || claims.session.as_str() != expected_session
            || claims.node.as_str() != self.node.as_str()
            || claims.instance.as_str() != self.instance.as_str()
            || claims.epoch != self.epoch
            || claims.namespace_id != self.namespace_id
            || claims.key_fingerprint != self.key_fingerprint
        {
            return Err(OwnerCreationVerificationError::ScopeMismatch);
        }
        let canonical = claims.signing_bytes()
            .map_err(|_| OwnerCreationVerificationError::InvalidReceipt)?;
        if receipt.signature().len() != ML_DSA_65_SIGNATURE_LEN {
            return Err(OwnerCreationVerificationError::InvalidReceipt);
        }
        UnparsedPublicKey::new(&ML_DSA_65, self.public_key.as_slice())
            .verify(&canonical, receipt.signature())
            .map_err(|_| OwnerCreationVerificationError::InvalidSignature)?;
        Ok(VerifiedOwnerCreation {
            receipt,
            _pin: self,
        })
    }
}

/// Authenticated historical claims, not an assignment or grant token.
/// This type cannot be constructed by deserialising a receipt.
pub struct VerifiedOwnerCreation<'a> {
    receipt: OwnerCreationReceipt,
    _pin: &'a OwnerCreationKeyPin,
}

impl VerifiedOwnerCreation<'_> {
    pub fn claims(&self) -> &OwnerCreationClaims {
        self.receipt.claims()
    }

    #[cfg(all(target_os = "linux", feature = "hub-owner-readiness"))]
    pub(crate) fn receipt(&self) -> &OwnerCreationReceipt {
        &self.receipt
    }
}

fn checked_key(bytes: &[u8]) -> Result<[u8; ML_DSA_65_PUBLIC_KEY_LEN], OwnerCreationVerificationError> {
    let key: [u8; ML_DSA_65_PUBLIC_KEY_LEN] = bytes
        .try_into()
        .map_err(|_| OwnerCreationVerificationError::InvalidPin)?;
    if key.iter().all(|byte| *byte == 0) {
        return Err(OwnerCreationVerificationError::InvalidPin);
    }
    Ok(key)
}
