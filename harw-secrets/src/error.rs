//! Error types for `harw-secrets` (spec §5.3).
//!
//! `#[derive(HarwError)]` emits `Display`, `std::error::Error`, the `#[from]`
//! conversions, and the `SecretsResult<T>` / `AuditResult<T>` aliases. No
//! `anyhow`/`thiserror`. Only one `#[from]` per inner type per enum, matching
//! the `harw-macros` single-`From`-per-inner-type rule.

use harw_macros::HarwError;

use crate::id::{KeyVersion, SecretId};
use crate::policy::AeadAlgo;

/// Failure modes of the secret store and envelope layer.
#[derive(Debug, HarwError)]
pub enum SecretsError {
    /// No stored secret with the requested id.
    #[msg("secret '{id:?}' not found")]
    NotFound { id: SecretId },

    /// A secret with the requested id already exists.
    #[msg("secret '{id:?}' already exists")]
    AlreadyExists { id: SecretId },

    /// No stored secret has the human-readable name selected by a `secrets:`
    /// reference. The name is safe metadata; secret contents must not enter
    /// this error.
    #[msg("no stored secret named '{name}' found")]
    SecretNameNotFound { name: String },

    /// More than one stored secret has the human-readable name selected by a
    /// `secrets:` reference. Only the name and match count are safe to expose.
    #[msg("stored secret name '{name}' is ambiguous: {matches} matching secrets")]
    AmbiguousSecretName { name: String, matches: usize },

    /// The configured KEK provenance could not supply key material.
    #[msg("KEK provenance '{kind}' unavailable: {reason}")]
    KekUnavailable { kind: String, reason: String },

    /// Deterministic derivation of the KEK's hybrid ML-KEM keypair failed. The
    /// underlying error is retained for typed handling and its safe diagnostic
    /// text is included without exposing seed or derived key material.
    #[msg("crypt_guard KEK-to-hybrid-ML-KEM keypair derivation failed: {source}")]
    KekDerivation { source: crypt_guard::pq_hpke::Error },

    /// A policy or durable record names a retired pure ML-KEM level
    /// (`ml_kem_512`/`ml_kem_768`/`ml_kem_1024`). This is a `harw-secrets`
    /// policy, not a `crypt_guard` limit (3.0.2 can derive pure ML-KEM keys
    /// too): legacy pure ML-KEM records stay read-only metadata, so such
    /// envelopes can neither be sealed nor opened. Only the algorithm wire
    /// name is retained; record contents and key material must never enter
    /// this error.
    #[msg(
        "KEM algorithm '{algo}' is a retired pure ML-KEM level and is no longer supported; re-create the secret under a hybrid KEM"
    )]
    UnsupportedLegacyKem { algo: String },

    /// A deterministic HPKE seed has a length other than the required 32
    /// bytes. Only the length is retained; seed bytes must never enter errors.
    #[msg("deterministic HPKE seed has invalid length {actual}; expected 32 bytes")]
    InvalidHpkeSeedLength { actual: usize },

    /// A per-secret data-encryption key has a length other than the required
    /// 32 bytes. Only the observed length is retained; DEK bytes must never
    /// enter errors.
    #[msg("payload DEK has invalid length {actual}; expected 32 bytes")]
    InvalidDekLength { actual: usize },

    /// The durable nonce length is not valid for the recorded payload AEAD.
    /// The algorithm and length are safe metadata; nonce bytes must never enter
    /// errors.
    #[msg("payload nonce for {algorithm:?} has invalid length {actual}")]
    InvalidPayloadNonceLength { algorithm: AeadAlgo, actual: usize },

    /// The KEK key file exists but is group/other-accessible (§3).
    #[msg("KEK secret file at '{path}' has unsafe permissions {mode:o}, refusing to start")]
    UnsafeKeyFilePermissions { path: String, mode: u32 },

    /// A record still points at an old key generation mid-rotation (§2.4).
    #[msg(
        "record for '{id:?}' references key_version {found:?} but rotation to {expected:?} is in progress"
    )]
    RotationIncomplete {
        id: SecretId,
        found: KeyVersion,
        expected: KeyVersion,
    },

    /// A V3 (KMS-wrapped) record names a key generation the configured
    /// [`crate::dek_wrapper::DekWrapper`] cannot unwrap (wrong, revoked, or
    /// not yet available generation). Key reference and generations are safe
    /// metadata; wrapped or unwrapped DEK bytes never enter this error.
    #[msg(
        "DEK wrapper key '{key_id}' cannot unwrap generation {found}; available generation is {expected}"
    )]
    KeyGenerationMismatch {
        key_id: String,
        expected: u32,
        found: u32,
    },

    /// A V3 record was wrapped under a different key reference or crypto
    /// profile than the configured [`crate::dek_wrapper::DekWrapper`] serves.
    /// Only the name of the mismatching field is retained.
    #[msg("V3 record {field} does not match the configured DEK wrapper")]
    DekWrapperMismatch { field: String },

    /// The configured [`crate::dek_wrapper::DekWrapper`] reported an invalid
    /// identity (key reference not of the form `namespace/id`, or an empty
    /// crypto profile id). Nothing is sealed in that case.
    #[msg("DEK wrapper identity is invalid: {reason}")]
    InvalidDekWrapperIdentity { reason: String },

    /// The DEK wrapper (e.g. the AuthHub KMS) is unreachable or refused the
    /// request. Always fail closed: no fallback to another wrapping path. The
    /// reason must be a safe diagnostic without key or DEK material.
    #[msg("DEK wrapper '{profile}' unavailable: {reason}")]
    DekWrapperUnavailable { profile: String, reason: String },

    /// `crypt_guard` PQ HPKE sealing failure (carries the underlying crypto error).
    #[msg("crypt_guard sealing failed: {0}")]
    #[from]
    Seal(crypt_guard::pq_hpke::Error),

    /// `crypt_guard` PQ HPKE opening failure (constructed explicitly via
    /// `.map_err(SecretsError::Open)` — only one `#[from]` per inner type).
    #[msg("crypt_guard unsealing failed: {0}")]
    Open(crypt_guard::pq_hpke::Error),

    /// An opaque AEAD error occurred while sealing the payload under its DEK.
    /// Only the selected algorithm is retained; the foreign AEAD error is not
    /// a source because it does not implement `std::error::Error`.
    #[msg("payload AEAD sealing failed for {0:?}")]
    PayloadAeadSeal(AeadAlgo),

    /// An opaque AEAD error occurred while authenticating or opening the
    /// payload under its DEK. Only the selected algorithm is retained.
    #[msg("payload AEAD authentication failed for {0:?}")]
    PayloadAeadOpen(AeadAlgo),

    /// A serialized `crypt_guard` PQ HPKE envelope could not be decoded.
    #[msg("crypt_guard PQ HPKE envelope decode failed: {0}")]
    #[from]
    EnvelopeDecode(crypt_guard::pq_hpke::EnvelopeError),

    /// I/O error accessing the secret store on disk.
    #[msg("i/o error accessing secret store: {0}")]
    #[from]
    Io(std::io::Error),

    /// A durable `SecretRecord` could not be encoded or decoded in its JSON
    /// persistence format. The operation and reason must be safe diagnostics;
    /// callers should not include record contents or secret material.
    #[msg("secret record persistence format {operation} failed: {reason}")]
    PersistenceFormat { operation: String, reason: String },
}

/// Failure modes of the append-only, hash-chained audit log (§4).
#[derive(Debug, HarwError)]
pub enum AuditError {
    /// An event's `prev_hash` does not match the recomputed hash of its
    /// predecessor (§4.3 step 1).
    #[msg("hash chain broken at event {index}: expected prev_hash {expected:?}, found {found:?}")]
    ChainBroken {
        index: u64,
        expected: [u8; 32],
        found: [u8; 32],
    },

    /// A checkpoint's ML-DSA signature failed verification (§4.3 step 2).
    #[msg("checkpoint at event_count {event_count} has invalid signature")]
    InvalidCheckpointSignature { event_count: u64 },

    /// `crypt_guard` failed while signing an ML-DSA checkpoint. The typed
    /// source is retained for callers without exposing checkpoint contents.
    #[msg("crypt_guard ML-DSA checkpoint signing failed: {0}")]
    #[from]
    CheckpointSigning(crypt_guard::error::CryptError),

    /// A checkpoint claims more events than the log actually contains,
    /// indicating post-checkpoint truncation (§4.3 step 3).
    #[msg("checkpoint references event_count {referenced} beyond log length {actual}")]
    CheckpointBeyondLog { referenced: u64, actual: u64 },

    /// Checkpoint `event_count` values are not strictly increasing (§4.3 step 3).
    #[msg("checkpoint sequence is not monotonically increasing at index {index}")]
    NonMonotonicCheckpoint { index: u64 },

    /// I/O error accessing the audit log or checkpoint file.
    #[msg("i/o error accessing audit log: {0}")]
    #[from]
    Io(std::io::Error),
}

/// Fehlermodi des hostexternen Audit-Spiegels ([`crate::audit::mirror`]).
///
/// Beide Varianten tragen nur eine aufrufer-gelieferte `reason`; eine
/// [`MirrorTransport`]-Implementierung darf dort **keinen** Protokollinhalt
/// (Actor, Action, Subjects) hineinlegen — die Meldungen des Spiegels selbst
/// enthalten das ohnehin nicht, siehe die Moduldoku dort.
///
/// [`MirrorTransport`]: crate::audit::mirror::MirrorTransport
#[derive(Debug, HarwError)]
pub enum MirrorError {
    /// Der Transport hat ein Lebenszeichen
    /// ([`crate::audit::mirror::MirrorEntry`]) abgelehnt oder nicht zustellen
    /// können.
    #[msg("out-of-band audit mirror rejected a heartbeat entry: {reason}")]
    EntryRejected { reason: String },

    /// Der Transport hat eine Kettenbruch-Meldung
    /// ([`crate::audit::mirror::ChainBreakAlert`]) abgelehnt oder nicht
    /// zustellen können.
    #[msg("out-of-band audit mirror rejected a chain-break alert: {reason}")]
    AlertRejected { reason: String },
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::{AeadAlgo, AuditError, SecretsError};

    #[test]
    fn persistence_format_display_uses_only_safe_diagnostics() {
        let error = SecretsError::PersistenceFormat {
            operation: "decode JSON".to_owned(),
            reason: "invalid key_version type".to_owned(),
        };

        assert_eq!(
            error.to_string(),
            "secret record persistence format decode JSON failed: invalid key_version type"
        );
        assert!(!error.to_string().contains("plaintext-secret"));
    }

    #[test]
    fn envelope_decode_display_preserves_typed_failure() {
        let error =
            SecretsError::EnvelopeDecode(crypt_guard::pq_hpke::EnvelopeError::UnsupportedVersion {
                actual: 2,
            });

        assert_eq!(
            error.to_string(),
            "crypt_guard PQ HPKE envelope decode failed: unsupported crypt_guard PQ HPKE envelope version 2"
        );
        assert!(error.source().is_some());
    }

    #[test]
    fn invalid_hpke_seed_length_display_contains_only_length() {
        let error = SecretsError::InvalidHpkeSeedLength { actual: 31 };
        let display = error.to_string();

        assert_eq!(
            display,
            "deterministic HPKE seed has invalid length 31; expected 32 bytes"
        );
        assert!(!display.contains("seed bytes"));
        assert!(!format!("{error:?}").contains("seed bytes"));
    }

    #[test]
    fn unsupported_legacy_kem_display_contains_only_the_algorithm_name() {
        let error = SecretsError::UnsupportedLegacyKem {
            algo: "ml_kem_768".to_owned(),
        };
        let display = error.to_string();

        assert_eq!(
            display,
            "KEM algorithm 'ml_kem_768' is a retired pure ML-KEM level and is no longer supported; re-create the secret under a hybrid KEM"
        );
        assert!(error.source().is_none());
        assert!(!display.contains("seed bytes"));
        assert!(!display.contains("plaintext-secret"));
    }

    #[test]
    fn invalid_dek_length_display_contains_only_length() {
        let error = SecretsError::InvalidDekLength { actual: 31 };
        let display = error.to_string();

        assert_eq!(
            display,
            "payload DEK has invalid length 31; expected 32 bytes"
        );
        assert!(!display.contains("DEK bytes"));
        assert!(!format!("{error:?}").contains("DEK bytes"));
    }

    #[test]
    fn invalid_payload_nonce_length_display_identifies_algorithm_and_length() {
        let error = SecretsError::InvalidPayloadNonceLength {
            algorithm: AeadAlgo::XChaCha20Poly1305,
            actual: 23,
        };
        let display = error.to_string();

        assert_eq!(
            display,
            "payload nonce for XChaCha20Poly1305 has invalid length 23"
        );
        assert!(!display.contains("nonce bytes"));
    }

    #[test]
    fn payload_aead_failures_are_opaque_and_typed() {
        let seal = SecretsError::PayloadAeadSeal(AeadAlgo::XChaCha20Poly1305);
        let open = SecretsError::PayloadAeadOpen(AeadAlgo::AesGcmSiv);

        assert_eq!(
            seal.to_string(),
            "payload AEAD sealing failed for XChaCha20Poly1305"
        );
        assert_eq!(
            open.to_string(),
            "payload AEAD authentication failed for AesGcmSiv"
        );
        assert!(seal.source().is_none());
        assert!(open.source().is_none());
        assert!(!format!("{seal:?}").contains("plaintext-secret"));
        assert!(!format!("{open:?}").contains("plaintext-secret"));
    }

    #[test]
    fn secret_name_not_found_display_contains_only_safe_metadata() {
        let error = SecretsError::SecretNameNotFound {
            name: "provider-token".to_owned(),
        };
        let display = error.to_string();

        assert_eq!(display, "no stored secret named 'provider-token' found");
        assert!(!display.contains("plaintext-secret"));
        assert!(!display.contains("seed bytes"));
        assert!(!display.contains("key bytes"));
    }

    #[test]
    fn ambiguous_secret_name_display_contains_only_safe_metadata() {
        let error = SecretsError::AmbiguousSecretName {
            name: "provider-token".to_owned(),
            matches: 2,
        };
        let display = error.to_string();

        assert_eq!(
            display,
            "stored secret name 'provider-token' is ambiguous: 2 matching secrets"
        );
        assert!(!display.contains("plaintext-secret"));
        assert!(!display.contains("seed bytes"));
        assert!(!display.contains("key bytes"));
    }

    #[test]
    fn checkpoint_signing_preserves_typed_crypt_guard_source() {
        let error = AuditError::CheckpointSigning(crypt_guard::error::CryptError::SigningFailed);

        assert_eq!(
            error.to_string(),
            "crypt_guard ML-DSA checkpoint signing failed: digital signing operation failed"
        );
        assert!(matches!(
            error.source(),
            Some(source)
                if matches!(
                    source.downcast_ref::<crypt_guard::error::CryptError>(),
                    Some(crypt_guard::error::CryptError::SigningFailed)
                )
        ));
    }
}
