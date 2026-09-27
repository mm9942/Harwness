//! DEK-wrapping boundary for [`SecretEnvelopeFormat::KmsWrappedV3`] records
//! (Crypto-Masterplan v2 §16.2–§16.4, work package H6).
//!
//! A V3 record encrypts its payload locally under a fresh per-secret DEK
//! exactly like V2; only the DEK leaves the process, and only through a
//! [`DekWrapper`]. The production wrapper is the AuthHub KMS client
//! (`harw-infra-client`), so the Harw process no longer needs the long-term
//! KEK seed. [`LocalHpkeDekWrapper`] implements the same trait with the
//! existing local KEK HPKE path so V3 works without a hub (tests, single-host
//! deployments, and the explicit migration path).
//!
//! # Key generations
//! [`DekWrapper::key_generation`] uses Harwness [`crate::KeyVersion`]
//! semantics: 0-based, `0` is the genesis generation. The CryptGuard service
//! `KeyVersion` is 1-based (`NonZeroU32`): CryptGuard version =
//! `key_generation + 1`. Use [`crypt_guard_key_version`] and
//! [`key_generation_from_crypt_guard`] for the conversion; never pass one
//! numbering to an API that expects the other.
//!
//! [`SecretEnvelopeFormat::KmsWrappedV3`]: crate::record::SecretEnvelopeFormat::KmsWrappedV3

use std::num::NonZeroU32;

use crypt_guard::pq_hpke::{HpkeEnvelope, RecipientPublicKey};
use secrecy::{ExposeSecret, SecretBox};

pub use secrecy::zeroize::Zeroizing;

use crate::envelope::{hpke_authentication_failed, recipient_private_key, suite_for};
use crate::error::{SecretsError, SecretsResult};
use crate::kek::derive_public_key;
use crate::policy::CryptoPolicy;

/// Crypto profile id of [`LocalHpkeDekWrapper`].
pub const LOCAL_HPKE_PROFILE_ID: &str = "local-hpke-v1";

/// HPKE `info` of the local V3 DEK wrap. Distinct from the V2 wrapper info so
/// a V2 `wrapped_dek` can never be replayed as a V3 one (and vice versa).
const LOCAL_HPKE_INFO_V3: &[u8] = b"harwness:secrets:dek-wrap:v3:local-hpke";

/// Maximum byte length of each half of a key reference and of a profile id
/// (mirrors CryptGuard's `MAX_NAME_LEN`).
pub const MAX_WRAPPER_NAME_LEN: usize = 128;

/// Wraps and unwraps per-secret DEKs for V3 records.
///
/// Implementations are synchronous and object safe (`&dyn DekWrapper`,
/// `Arc<dyn DekWrapper>`); `Send + Sync` so a [`crate::SecretStore`] holding
/// one stays shareable across threads.
///
/// # Contract
/// - [`Self::wrap_dek`] wraps under the *current* [`Self::key_generation`] of
///   [`Self::key_id`] and must authenticate `aad` (the caller binds secret id,
///   key reference, generation, profile and payload suite into it).
/// - [`Self::unwrap_dek`] must refuse, with a typed error, any
///   `key_generation` it cannot serve (wrong, revoked, or unknown generation:
///   [`SecretsError::KeyGenerationMismatch`]) and any `aad` other than the one
///   used at wrap time. It must never fall back to another key.
/// - An unreachable KMS fails closed with
///   [`SecretsError::DekWrapperUnavailable`].
/// - Errors must never contain DEK bytes, wrapped bytes, or key material.
pub trait DekWrapper: Send + Sync {
    /// Crypto profile id persisted as `crypto_profile_id`
    /// (e.g. [`LOCAL_HPKE_PROFILE_ID`]).
    fn profile_id(&self) -> &str;

    /// KMS key reference `"namespace/id"` persisted as `key_id`.
    fn key_id(&self) -> &str;

    /// Current key generation (Harwness numbering, 0-based; CryptGuard
    /// version = this + 1).
    fn key_generation(&self) -> u32;

    /// Wrap `dek` under the current generation, authenticating `aad`.
    ///
    /// # Errors
    /// Implementation specific; see the trait contract.
    fn wrap_dek(&self, dek: &[u8], aad: &[u8]) -> SecretsResult<Vec<u8>>;

    /// Unwrap `wrapped` that was produced under `key_generation` with `aad`.
    ///
    /// # Errors
    /// [`SecretsError::KeyGenerationMismatch`] for a generation this wrapper
    /// cannot serve; an authentication error for a wrong `aad` or tampered
    /// `wrapped`; see the trait contract.
    fn unwrap_dek(
        &self,
        wrapped: &[u8],
        key_generation: u32,
        aad: &[u8],
    ) -> SecretsResult<Zeroizing<Vec<u8>>>;
}

/// CryptGuard service key version (1-based) for a Harwness key generation
/// (0-based). `None` only for `u32::MAX`, which has no CryptGuard counterpart.
#[must_use]
pub fn crypt_guard_key_version(key_generation: u32) -> Option<NonZeroU32> {
    key_generation.checked_add(1).and_then(NonZeroU32::new)
}

/// Harwness key generation (0-based) for a CryptGuard service key version
/// (1-based). Total: every CryptGuard version maps to exactly one generation.
#[must_use]
pub fn key_generation_from_crypt_guard(version: NonZeroU32) -> u32 {
    version.get() - 1
}

/// Validate a wrapper's identity before it is bound into AAD or persisted.
///
/// `key_id` must be `namespace/id`, each half non-empty, at most
/// [`MAX_WRAPPER_NAME_LEN`] bytes of `[A-Za-z0-9._-]` and not starting with
/// `.` (CryptGuard `KeyNamespace`/`KeyId` rules). `profile_id` follows the same
/// name rule.
///
/// # Errors
/// [`SecretsError::InvalidDekWrapperIdentity`] naming the offending part.
pub fn validate_wrapper_identity(key_id: &str, profile_id: &str) -> SecretsResult<()> {
    let Some((namespace, id)) = key_id.split_once('/') else {
        return Err(invalid_identity("key_id must have the form namespace/id"));
    };
    if !valid_name(namespace) {
        return Err(invalid_identity("key_id namespace is not a valid key name"));
    }
    if !valid_name(id) {
        return Err(invalid_identity("key_id id is not a valid key name"));
    }
    if !valid_name(profile_id) {
        return Err(invalid_identity("crypto_profile_id is not a valid name"));
    }
    Ok(())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_WRAPPER_NAME_LEN
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn invalid_identity(reason: &str) -> SecretsError {
    SecretsError::InvalidDekWrapperIdentity {
        reason: reason.to_owned(),
    }
}

/// [`DekWrapper`] over the local KEK HPKE path (profile
/// [`LOCAL_HPKE_PROFILE_ID`]): the DEK is sealed with crypt_guard v3 PQ HPKE
/// to the hybrid recipient key derived from a 32-byte root KEK seed, exactly
/// like V2, but with V3 `info` and the caller's V3 AAD.
///
/// It serves exactly one generation; unwrapping any other generation is
/// refused with [`SecretsError::KeyGenerationMismatch`].
pub struct LocalHpkeDekWrapper {
    key_id: String,
    key_generation: u32,
    policy: CryptoPolicy,
    public_key: Vec<u8>,
    root_seed: SecretBox<[u8]>,
}

impl LocalHpkeDekWrapper {
    /// Build a local wrapper for `key_id` (`namespace/id`) at
    /// `key_generation` from the 32-byte **root** KEK seed; the hybrid KEM and
    /// HPKE AEAD come from `policy`.
    ///
    /// # Errors
    /// - [`SecretsError::InvalidDekWrapperIdentity`]: malformed `key_id`.
    /// - [`SecretsError::KekUnavailable`]: `root_seed` is not 32 bytes.
    /// - [`SecretsError::UnsupportedLegacyKem`]: `policy` names a retired pure
    ///   ML-KEM level.
    /// - [`SecretsError::KekDerivation`]: `crypt_guard` rejected the derivation.
    pub fn new(
        key_id: impl Into<String>,
        key_generation: u32,
        policy: CryptoPolicy,
        root_seed: SecretBox<[u8]>,
    ) -> SecretsResult<Self> {
        let key_id = key_id.into();
        validate_wrapper_identity(&key_id, LOCAL_HPKE_PROFILE_ID)?;
        let public_key = derive_public_key(&policy, &root_seed)?;
        Ok(Self {
            key_id,
            key_generation,
            policy,
            public_key,
            root_seed,
        })
    }

    /// The hybrid KEM / AEAD policy this wrapper seals DEKs with.
    #[must_use]
    pub fn policy(&self) -> CryptoPolicy {
        self.policy
    }
}

impl std::fmt::Debug for LocalHpkeDekWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalHpkeDekWrapper")
            .field("key_id", &self.key_id)
            .field("key_generation", &self.key_generation)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl DekWrapper for LocalHpkeDekWrapper {
    fn profile_id(&self) -> &str {
        LOCAL_HPKE_PROFILE_ID
    }

    fn key_id(&self) -> &str {
        &self.key_id
    }

    fn key_generation(&self) -> u32 {
        self.key_generation
    }

    fn wrap_dek(&self, dek: &[u8], aad: &[u8]) -> SecretsResult<Vec<u8>> {
        let suite = suite_for(self.policy.kem, self.policy.aead)?;
        let recipient = RecipientPublicKey::from_bytes(suite.kem(), &self.public_key)
            .map_err(SecretsError::Seal)?;
        HpkeEnvelope::seal(suite, &recipient, LOCAL_HPKE_INFO_V3, aad, dek)
            .map(|envelope| envelope.to_bytes())
            .map_err(SecretsError::Seal)
    }

    fn unwrap_dek(
        &self,
        wrapped: &[u8],
        key_generation: u32,
        aad: &[u8],
    ) -> SecretsResult<Zeroizing<Vec<u8>>> {
        if key_generation != self.key_generation {
            return Err(SecretsError::KeyGenerationMismatch {
                key_id: self.key_id.clone(),
                expected: self.key_generation,
                found: key_generation,
            });
        }
        let suite = suite_for(self.policy.kem, self.policy.aead)?;
        let envelope = HpkeEnvelope::from_bytes(wrapped).map_err(SecretsError::EnvelopeDecode)?;
        if envelope.suite() != suite {
            return Err(SecretsError::Open(hpke_authentication_failed()));
        }
        let recipient =
            recipient_private_key(self.policy.kem, suite, self.root_seed.expose_secret())?;
        envelope
            .open(&recipient, LOCAL_HPKE_INFO_V3, aad)
            .map(Zeroizing::new)
            .map_err(SecretsError::Open)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn wrapper(generation: u32) -> TestResult<LocalHpkeDekWrapper> {
        Ok(LocalHpkeDekWrapper::new(
            "secrets/dek-wrap",
            generation,
            CryptoPolicy::strongest(),
            SecretBox::new(vec![0xA5_u8; 32].into_boxed_slice()),
        )?)
    }

    #[test]
    fn local_wrapper_round_trips_and_binds_aad() -> TestResult {
        let wrapper = wrapper(3)?;
        let dek = [0x42_u8; 32];
        let wrapped = wrapper.wrap_dek(&dek, b"aad")?;

        assert_eq!(wrapper.unwrap_dek(&wrapped, 3, b"aad")?.as_slice(), dek);
        assert!(matches!(
            wrapper.unwrap_dek(&wrapped, 3, b"other-aad"),
            Err(SecretsError::Open(_))
        ));
        Ok(())
    }

    #[test]
    fn local_wrapper_refuses_a_foreign_generation_with_a_typed_error() -> TestResult {
        let wrapper = wrapper(3)?;
        let wrapped = wrapper.wrap_dek(&[0x42_u8; 32], b"aad")?;

        match wrapper.unwrap_dek(&wrapped, 2, b"aad") {
            Err(SecretsError::KeyGenerationMismatch {
                key_id,
                expected: 3,
                found: 2,
            }) if key_id == "secrets/dek-wrap" => Ok(()),
            other => Err(TestError::Unexpected(format!(
                "KeyGenerationMismatch erwartet, erhalten: {:?}",
                other.map(|_| "Ok")
            ))),
        }
    }

    #[test]
    fn wrapper_identity_validation_follows_crypt_guard_key_names() {
        assert!(validate_wrapper_identity("secrets/dek-wrap", "local-hpke-v1").is_ok());
        for (key_id, profile) in [
            ("no-slash", "local-hpke-v1"),
            ("/id", "local-hpke-v1"),
            ("ns/", "local-hpke-v1"),
            ("ns/a/b", "local-hpke-v1"),
            (".ns/id", "local-hpke-v1"),
            ("ns/id with space", "local-hpke-v1"),
            ("ns/id", ""),
        ] {
            assert!(
                matches!(
                    validate_wrapper_identity(key_id, profile),
                    Err(SecretsError::InvalidDekWrapperIdentity { .. })
                ),
                "{key_id:?}/{profile:?} must be refused"
            );
        }
        assert!(matches!(
            LocalHpkeDekWrapper::new(
                "not-a-key-ref",
                0,
                CryptoPolicy::strongest(),
                SecretBox::new(vec![0xA5_u8; 32].into_boxed_slice()),
            ),
            Err(SecretsError::InvalidDekWrapperIdentity { .. })
        ));
    }

    #[test]
    fn crypt_guard_key_version_is_generation_plus_one() {
        assert_eq!(crypt_guard_key_version(0).map(NonZeroU32::get), Some(1));
        assert_eq!(crypt_guard_key_version(6).map(NonZeroU32::get), Some(7));
        assert_eq!(crypt_guard_key_version(u32::MAX), None);
        assert_eq!(key_generation_from_crypt_guard(NonZeroU32::MIN), 0);
        for generation in [0, 1, 41, u32::MAX - 1] {
            assert_eq!(
                crypt_guard_key_version(generation).map(key_generation_from_crypt_guard),
                Some(generation)
            );
        }
    }
}
