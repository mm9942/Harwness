//! Signed chain-head checkpoints (spec §4.2–§4.3). Every N events or T minutes,
//! the current chain-head hash is ML-DSA-signed and written to a separate
//! append-only checkpoint file. Checkpoints are themselves chained
//! (`prev_checkpoint_hash`) so the checkpoint file can also be verified for
//! gaps. The signing key is distinct from the KEK.
//!
//! Structural verification (monotonic `event_count`, no checkpoint beyond the
//! log) and ML-DSA signing/verification are implemented with `crypt_guard`.

use crypt_guard::sign::{
    ml_dsa::{MlDsa65Impl, MlDsaSignature, MlDsaSigningKey, MlDsaVerifyingKey},
    SignAlgorithm,
};
use jiff::Timestamp;
use secrecy::{ExposeSecret as _, SecretBox};
use sha2::{Digest, Sha256};

use crate::audit::chain::GENESIS_HASH;
use crate::error::{AuditError, AuditResult};

/// One signed checkpoint of the audit chain head.
#[derive(Clone, Debug)]
pub struct Checkpoint {
    /// The audit-log chain head this checkpoint attests to.
    pub chain_head_hash: [u8; 32],
    /// How many events had been recorded when it was signed.
    pub event_count: u64,
    /// When the checkpoint was signed.
    pub signed_at: Timestamp,
    /// ML-DSA signature over the checkpoint's canonical bytes.
    pub signature: Vec<u8>,
    /// SHA-256 of the previous checkpoint's canonical bytes (genesis for first).
    pub prev_checkpoint_hash: [u8; 32],
}

/// Deterministic serialization of a checkpoint for chaining/signing. The
/// `signature` field is intentionally excluded — it is computed *over* these
/// bytes.
#[must_use]
pub fn checkpoint_canonical_bytes(checkpoint: &Checkpoint) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&checkpoint.chain_head_hash);
    out.extend_from_slice(&checkpoint.event_count.to_be_bytes());
    out.extend_from_slice(&checkpoint.signed_at.as_second().to_be_bytes());
    out.extend_from_slice(&checkpoint.signed_at.subsec_nanosecond().to_be_bytes());
    out.extend_from_slice(&checkpoint.prev_checkpoint_hash);
    out
}

/// SHA-256 of a checkpoint's canonical bytes (used for `prev_checkpoint_hash`).
#[must_use]
pub fn checkpoint_hash(checkpoint: &Checkpoint) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(checkpoint_canonical_bytes(checkpoint));
    hasher.finalize().into()
}

/// An in-memory, append-only chain of signed checkpoints.
#[derive(Clone, Debug, Default)]
pub struct CheckpointLog {
    checkpoints: Vec<Checkpoint>,
    head: [u8; 32],
}

impl CheckpointLog {
    /// A fresh checkpoint chain with a genesis head.
    #[must_use]
    pub fn new() -> Self {
        Self {
            checkpoints: Vec::new(),
            head: GENESIS_HASH,
        }
    }

    /// The current checkpoint-chain head.
    #[must_use]
    pub fn chain_head(&self) -> [u8; 32] {
        self.head
    }

    /// Number of checkpoints recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.checkpoints.len()
    }

    /// Whether no checkpoint has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.checkpoints.is_empty()
    }

    /// The recorded checkpoints in append order.
    #[must_use]
    pub fn checkpoints(&self) -> &[Checkpoint] {
        &self.checkpoints
    }

    /// Sign `chain_head_hash` at `event_count` under `signing_key` and append
    /// the resulting checkpoint.
    pub fn emit(
        &mut self,
        chain_head_hash: [u8; 32],
        event_count: u64,
        signing_key: &SecretBox<[u8]>,
    ) -> AuditResult<&Checkpoint> {
        if let Some(previous) = self.checkpoints.last() {
            if event_count <= previous.event_count {
                return Err(AuditError::NonMonotonicCheckpoint {
                    index: self.checkpoints.len() as u64,
                });
            }
        }

        let mut checkpoint = Checkpoint {
            chain_head_hash,
            event_count,
            signed_at: Timestamp::now(),
            signature: Vec::new(),
            prev_checkpoint_hash: self.head,
        };
        let signing_key = MlDsaSigningKey::from_bytes(signing_key.expose_secret().to_vec());
        let signature = MlDsa65Impl::sign(&signing_key, &checkpoint_canonical_bytes(&checkpoint))
            .map_err(AuditError::CheckpointSigning)?;

        checkpoint.signature = signature.as_ref().to_vec();
        self.head = checkpoint_hash(&checkpoint);
        self.checkpoints.push(checkpoint);

        Ok(self
            .checkpoints
            .last()
            .expect("checkpoint was appended before borrowing it"))
    }

    /// Confirm `event_count` values are strictly increasing across checkpoints
    /// (§4.3 step 3). Reports the first non-increasing index.
    pub fn check_monotonic(&self) -> AuditResult<()> {
        for (i, pair) in self.checkpoints.windows(2).enumerate() {
            if pair[1].event_count <= pair[0].event_count {
                return Err(AuditError::NonMonotonicCheckpoint {
                    index: (i + 1) as u64,
                });
            }
        }
        Ok(())
    }

    /// Confirm no checkpoint references more events than the log actually holds
    /// (§4.3 step 3) — a checkpoint beyond `log_len` indicates post-checkpoint
    /// truncation.
    pub fn check_within_log(&self, log_len: u64) -> AuditResult<()> {
        for checkpoint in &self.checkpoints {
            if checkpoint.event_count > log_len {
                return Err(AuditError::CheckpointBeyondLog {
                    referenced: checkpoint.event_count,
                    actual: log_len,
                });
            }
        }
        Ok(())
    }

    /// Verify the checkpoint chaining is internally consistent: each
    /// `prev_checkpoint_hash` equals the recomputed hash of its predecessor.
    pub fn verify_chain(&self) -> AuditResult<()> {
        let mut prev = GENESIS_HASH;
        for (index, checkpoint) in self.checkpoints.iter().enumerate() {
            if checkpoint.prev_checkpoint_hash != prev {
                return Err(AuditError::ChainBroken {
                    index: index as u64,
                    expected: prev,
                    found: checkpoint.prev_checkpoint_hash,
                });
            }
            prev = checkpoint_hash(checkpoint);
        }
        Ok(())
    }

    /// Verify one checkpoint's ML-DSA signature against `verification_key`
    /// (§4.3 step 2).
    pub fn verify_signature(
        &self,
        checkpoint: &Checkpoint,
        verification_key: &[u8],
    ) -> AuditResult<()> {
        let verification_key = MlDsaVerifyingKey::from_bytes(verification_key.to_vec());
        let signature = MlDsaSignature::from_bytes(checkpoint.signature.clone());

        MlDsa65Impl::verify(
            &verification_key,
            &checkpoint_canonical_bytes(checkpoint),
            &signature,
        )
        .map_err(|_| AuditError::InvalidCheckpointSignature {
            event_count: checkpoint.event_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use crypt_guard::{
        kem::backend::OsRng,
        sign::{ml_dsa::MlDsa65Impl, SignAlgorithm},
    };

    use super::*;

    fn signing_keypair() -> (SecretBox<[u8]>, Vec<u8>) {
        let mut rng = OsRng;
        let (signing_key, verification_key) =
            MlDsa65Impl::keypair(&mut rng).expect("generate ML-DSA-65 test keypair");

        (
            SecretBox::new(signing_key.as_bytes().to_vec().into_boxed_slice()),
            verification_key.as_bytes().to_vec(),
        )
    }

    #[test]
    fn emitted_checkpoint_round_trips_through_mldsa_verification() {
        let (signing_seed, verification_key) = signing_keypair();
        let mut checkpoints = CheckpointLog::new();

        let checkpoint = checkpoints
            .emit([0xA5; 32], 4, &signing_seed)
            .expect("emit signed checkpoint")
            .clone();

        checkpoints
            .verify_signature(&checkpoint, &verification_key)
            .expect("verify ML-DSA-65 checkpoint signature");
        checkpoints.verify_chain().expect("verify checkpoint chain");
        assert_eq!(checkpoints.chain_head(), checkpoint_hash(&checkpoint));
    }

    #[test]
    fn tampered_checkpoint_is_rejected_fail_closed() {
        let (signing_seed, verification_key) = signing_keypair();
        let mut checkpoints = CheckpointLog::new();
        let mut checkpoint = checkpoints
            .emit([0x5A; 32], 9, &signing_seed)
            .expect("emit signed checkpoint")
            .clone();
        checkpoint.chain_head_hash[0] ^= 0x01;

        assert!(matches!(
            checkpoints.verify_signature(&checkpoint, &verification_key),
            Err(AuditError::InvalidCheckpointSignature { event_count: 9 })
        ));
    }

    #[test]
    fn emit_rejects_non_monotonic_event_counts_without_advancing_the_chain() {
        let (signing_seed, _) = signing_keypair();
        let mut checkpoints = CheckpointLog::new();
        checkpoints
            .emit([0x11; 32], 3, &signing_seed)
            .expect("emit first checkpoint");
        let head = checkpoints.chain_head();

        assert!(matches!(
            checkpoints.emit([0x22; 32], 3, &signing_seed),
            Err(AuditError::NonMonotonicCheckpoint { index: 1 })
        ));
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints.chain_head(), head);
    }

    #[test]
    fn verify_chain_reports_tampered_predecessor_hash() {
        let (signing_seed, _) = signing_keypair();
        let mut checkpoints = CheckpointLog::new();
        checkpoints
            .emit([0x11; 32], 3, &signing_seed)
            .expect("emit first checkpoint");
        checkpoints
            .emit([0x22; 32], 6, &signing_seed)
            .expect("emit second checkpoint");

        let expected = checkpoint_hash(&checkpoints.checkpoints[0]);
        checkpoints.checkpoints[1].prev_checkpoint_hash = [0xA5; 32];

        assert!(matches!(
            checkpoints.verify_chain(),
            Err(AuditError::ChainBroken {
                index: 1,
                expected: actual_expected,
                found: actual_found,
            }) if actual_expected == expected && actual_found == [0xA5; 32]
        ));
    }
}
