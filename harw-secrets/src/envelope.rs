//! Versioned secret-envelope boundary.
//!
//! V2 keeps the payload and KEK lifecycle independent: a fresh random DEK
//! encrypts the payload locally, while crypt_guard's v3 PQ HPKE transports
//! only that DEK.  The legacy direct-HPKE record remains readable through its
//! explicit record discriminator.
//!
//! KEM layer: recipient keys come exclusively from hybrid
//! draft-ietf-hpke-pq-05 KEMs, built from the per-KEM seed that is
//! domain-separated from the 32-byte root KEK seed (`kek::derive_kem_seed`,
//! then `RecipientPrivateKey::from_seed_bytes`). Records under a retired pure
//! ML-KEM level are refused with [`SecretsError::UnsupportedLegacyKem`] before
//! any decoding or cryptographic step.

use aes_gcm_siv::{
    Aes256GcmSiv, Nonce as AesNonce,
    aead::{Aead as _, KeyInit as _, Payload as AesPayload},
};
use chacha20poly1305::{XChaCha20Poly1305, XNonce, aead::Payload as ChaChaPayload};
use crypt_guard::pq_hpke::{
    Aead as HpkeAead, HpkeEnvelope, Kdf, RecipientPrivateKey, RecipientPublicKey, Suite,
};
use secrecy::SecretBox;

use crate::error::{SecretsError, SecretsResult};
use crate::id::{KeyVersion, SecretId};
use crate::kek::{KEK_SEED_LEN, derive_kem_seed};
use crate::policy::{AeadAlgo, CryptoPolicy, KemAlgo};
use crate::record::{SecretEnvelopeFormat, SecretRecord};

const HPKE_INFO_V1: &[u8] = b"harwness:secrets:hpke:v1";
const HPKE_INFO_V2: &[u8] = b"harwness:secrets:dek-wrap:v2";
const V1_AAD_DOMAIN: &[u8] = b"harwness:secrets:hpke:aad:v1";
const V2_WRAPPER_AAD_DOMAIN: &[u8] = b"harwness:secrets:wrapper:aad:v2";
const V2_PAYLOAD_AAD_DOMAIN: &[u8] = b"harwness:secrets:payload:aad:v2";
const DEK_LEN: usize = 32;

/// The byte outputs of a single seal operation, before they are joined with
/// their corresponding record metadata into a [`SecretRecord`].
#[derive(Clone, Debug)]
pub struct SealedSecret {
    /// Locally AEAD-encrypted secret bytes for V2, or a direct HPKE envelope
    /// for the retained V1 compatibility path.
    pub ciphertext: Vec<u8>,
    /// Local payload AEAD nonce for V2; V1 envelopes derive their nonce.
    pub nonce: Vec<u8>,
    /// Serialized v3 PQ HPKE envelope containing only the V2 DEK. For V1,
    /// this remains the encapsulation duplicated from `ciphertext`.
    pub wrapped_dek: Vec<u8>,
    /// Durable layout discriminator for the resulting record.
    pub envelope_format: SecretEnvelopeFormat,
    /// Hybrid KEM encoded by the HPKE wrapper suite.
    pub kem_algo: KemAlgo,
    /// AEAD algorithm selected for the local payload and wrapper suite.
    pub aead_algo: AeadAlgo,
}

/// Seal a new V2 record. A fresh 32-byte DEK encrypts `plaintext` locally;
/// crypt_guard v3 PQ HPKE only wraps that DEK for `kek_public`.
///
/// # Errors
/// - [`SecretsError::UnsupportedLegacyKem`]: `policy` names a retired pure
///   ML-KEM level; nothing is encrypted in that case.
/// - [`SecretsError::Seal`]: `kek_public` is not a valid key for the policy's
///   hybrid KEM, or HPKE sealing failed.
pub fn seal(
    policy: &CryptoPolicy,
    id: SecretId,
    key_version: KeyVersion,
    kek_public: &[u8],
    plaintext: &[u8],
) -> SecretsResult<SealedSecret> {
    let suite = suite_for(policy.kem, policy.aead)?;
    let recipient =
        RecipientPublicKey::from_bytes(suite.kem(), kek_public).map_err(SecretsError::Seal)?;

    let mut dek = [0_u8; DEK_LEN];
    getrandom::fill(&mut dek).map_err(|_| SecretsError::Seal(hpke_internal_failure()))?;

    let mut nonce = vec![0_u8; nonce_len(policy.aead)];
    getrandom::fill(&mut nonce).map_err(|_| SecretsError::Seal(hpke_internal_failure()))?;

    let payload_aad = payload_aad(id, policy.aead);
    let ciphertext = encrypt_payload(policy.aead, &dek, &nonce, &payload_aad, plaintext)?;

    let wrapper_aad = wrapper_aad(id, key_version, suite);
    let wrapped_dek = HpkeEnvelope::seal(suite, &recipient, HPKE_INFO_V2, &wrapper_aad, &dek)
        .map_err(SecretsError::Seal)?
        .to_bytes();

    Ok(SealedSecret {
        ciphertext,
        nonce,
        wrapped_dek,
        envelope_format: SecretEnvelopeFormat::DekWrappedV2,
        kem_algo: policy.kem,
        aead_algo: policy.aead,
    })
}

/// Open a record according to its durable envelope discriminator.
///
/// # Errors
/// - [`SecretsError::UnsupportedLegacyKem`]: the record was sealed under a
///   retired pure ML-KEM level; such envelopes are unreadable with
///   `crypt_guard` 3.0.1.
/// - [`SecretsError::EnvelopeDecode`], [`SecretsError::Open`],
///   [`SecretsError::PayloadAeadOpen`] and the length errors for malformed or
///   tampered records.
pub fn open(kek_seed: &[u8], record: &SecretRecord) -> SecretsResult<SecretBox<[u8]>> {
    match record.envelope_format {
        SecretEnvelopeFormat::DekWrappedV2 => open_v2(kek_seed, record),
        SecretEnvelopeFormat::LegacyDirectHpke => open_v1(kek_seed, record),
    }
}

/// Rewrap the existing V2 DEK for the next KEK generation without touching
/// the durable payload ciphertext or nonce.
///
/// The caller must persist the returned wrapper together with `next_key_version`.
/// Legacy direct-HPKE records deliberately fail closed: their payload is not a
/// separately rewrappable DEK. Records under a retired pure ML-KEM level fail
/// with [`SecretsError::UnsupportedLegacyKem`].
pub fn rewrap_v2(
    old_kek_seed: &[u8],
    record: &SecretRecord,
    next_kek_public: &[u8],
    next_key_version: KeyVersion,
) -> SecretsResult<Vec<u8>> {
    if record.envelope_format != SecretEnvelopeFormat::DekWrappedV2 {
        return Err(SecretsError::PersistenceFormat {
            operation: "rewrap V2 DEK".to_owned(),
            reason: "legacy direct-HPKE records cannot be rewrapped".to_owned(),
        });
    }

    let suite = suite_for(record.kem_algo, record.aead_algo)?;
    let dek = unwrap_v2_dek(old_kek_seed, record, suite)?;
    let next_recipient =
        RecipientPublicKey::from_bytes(suite.kem(), next_kek_public).map_err(SecretsError::Seal)?;
    let next_wrapper_aad = wrapper_aad(record.id, next_key_version, suite);
    HpkeEnvelope::seal(
        suite,
        &next_recipient,
        HPKE_INFO_V2,
        &next_wrapper_aad,
        &dek,
    )
    .map(|envelope| envelope.to_bytes())
    .map_err(SecretsError::Seal)
}

fn open_v2(kek_seed: &[u8], record: &SecretRecord) -> SecretsResult<SecretBox<[u8]>> {
    let suite = suite_for(record.kem_algo, record.aead_algo)?;
    let dek = unwrap_v2_dek(kek_seed, record, suite)?;
    let aad = payload_aad(record.id, record.aead_algo);
    let plaintext = decrypt_payload(
        record.aead_algo,
        &dek,
        &record.nonce,
        &aad,
        &record.ciphertext,
    )?;

    Ok(SecretBox::new(plaintext.into_boxed_slice()))
}

/// `suite` must be the (already legacy-checked) suite recorded in `record`.
fn unwrap_v2_dek(
    kek_seed: &[u8],
    record: &SecretRecord,
    suite: Suite,
) -> SecretsResult<[u8; DEK_LEN]> {
    let wrapper =
        HpkeEnvelope::from_bytes(&record.wrapped_dek).map_err(SecretsError::EnvelopeDecode)?;
    if wrapper.suite() != suite {
        return Err(SecretsError::Open(hpke_authentication_failed()));
    }

    let recipient = recipient_private_key(record.kem_algo, suite, kek_seed)?;
    let wrapper_aad = wrapper_aad(record.id, record.key_version, suite);
    let dek = wrapper
        .open(&recipient, HPKE_INFO_V2, &wrapper_aad)
        .map_err(SecretsError::Open)?;
    dek.try_into()
        .map_err(|dek: Vec<u8>| SecretsError::InvalidDekLength { actual: dek.len() })
}

/// Retained direct-HPKE V1 compatibility path. New records are always V2.
fn open_v1(kek_seed: &[u8], record: &SecretRecord) -> SecretsResult<SecretBox<[u8]>> {
    let suite = suite_for(record.kem_algo, record.aead_algo)?;
    let envelope =
        HpkeEnvelope::from_bytes(&record.ciphertext).map_err(SecretsError::EnvelopeDecode)?;
    if !v1_record_matches_envelope(record, &envelope, suite) {
        return Err(SecretsError::Open(hpke_authentication_failed()));
    }

    let recipient = recipient_private_key(record.kem_algo, suite, kek_seed)?;
    let aad = v1_record_aad(record.id, record.key_version, suite);
    let plaintext = envelope
        .open(&recipient, HPKE_INFO_V1, &aad)
        .map_err(SecretsError::Open)?;

    Ok(SecretBox::new(plaintext.into_boxed_slice()))
}

fn encrypt_payload(
    aead: AeadAlgo,
    dek: &[u8; DEK_LEN],
    nonce: &[u8],
    aad: &[u8],
    plaintext: &[u8],
) -> SecretsResult<Vec<u8>> {
    match aead {
        AeadAlgo::XChaCha20Poly1305 => {
            let nonce = xchacha_nonce(nonce)?;
            XChaCha20Poly1305::new_from_slice(dek)
                .map_err(|_| SecretsError::InvalidDekLength { actual: dek.len() })?
                .encrypt(
                    &nonce,
                    ChaChaPayload {
                        msg: plaintext,
                        aad,
                    },
                )
                .map_err(|_| SecretsError::PayloadAeadSeal(aead))
        }
        AeadAlgo::AesGcmSiv => {
            let nonce = aes_nonce(nonce)?;
            Aes256GcmSiv::new_from_slice(dek)
                .map_err(|_| SecretsError::InvalidDekLength { actual: dek.len() })?
                .encrypt(
                    &nonce,
                    AesPayload {
                        msg: plaintext,
                        aad,
                    },
                )
                .map_err(|_| SecretsError::PayloadAeadSeal(aead))
        }
    }
}

fn decrypt_payload(
    aead: AeadAlgo,
    dek: &[u8; DEK_LEN],
    nonce: &[u8],
    aad: &[u8],
    ciphertext: &[u8],
) -> SecretsResult<Vec<u8>> {
    match aead {
        AeadAlgo::XChaCha20Poly1305 => {
            let nonce = xchacha_nonce(nonce)?;
            XChaCha20Poly1305::new_from_slice(dek)
                .map_err(|_| SecretsError::InvalidDekLength { actual: dek.len() })?
                .decrypt(
                    &nonce,
                    ChaChaPayload {
                        msg: ciphertext,
                        aad,
                    },
                )
                .map_err(|_| SecretsError::PayloadAeadOpen(aead))
        }
        AeadAlgo::AesGcmSiv => {
            let nonce = aes_nonce(nonce)?;
            Aes256GcmSiv::new_from_slice(dek)
                .map_err(|_| SecretsError::InvalidDekLength { actual: dek.len() })?
                .decrypt(
                    &nonce,
                    AesPayload {
                        msg: ciphertext,
                        aad,
                    },
                )
                .map_err(|_| SecretsError::PayloadAeadOpen(aead))
        }
    }
}

fn xchacha_nonce(nonce: &[u8]) -> SecretsResult<XNonce> {
    let nonce: [u8; 24] =
        nonce
            .try_into()
            .map_err(|_| SecretsError::InvalidPayloadNonceLength {
                algorithm: AeadAlgo::XChaCha20Poly1305,
                actual: nonce.len(),
            })?;
    Ok(nonce.into())
}

fn aes_nonce(nonce: &[u8]) -> SecretsResult<AesNonce> {
    let nonce: [u8; 12] =
        nonce
            .try_into()
            .map_err(|_| SecretsError::InvalidPayloadNonceLength {
                algorithm: AeadAlgo::AesGcmSiv,
                actual: nonce.len(),
            })?;
    Ok(nonce.into())
}

const fn nonce_len(aead: AeadAlgo) -> usize {
    match aead {
        AeadAlgo::XChaCha20Poly1305 => 24,
        AeadAlgo::AesGcmSiv => 12,
    }
}

/// Exact HPKE suite for a record/policy: hybrid KEM, SHAKE256, and the
/// matching (crypt_guard private-extension) AEAD.
///
/// # Errors
/// - [`SecretsError::UnsupportedLegacyKem`]: `kem` is a retired pure ML-KEM
///   level.
fn suite_for(kem: KemAlgo, aead: AeadAlgo) -> SecretsResult<Suite> {
    Ok(Suite::new(
        kem.hpke_kem()?,
        Kdf::Shake256,
        hpke_aead_for(aead),
    ))
}

/// Reconstruct the hybrid recipient private key for `kem` from the 32-byte
/// root KEK seed via the per-KEM domain-separated seed.
///
/// `suite` must be the (already legacy-checked) suite derived from `kem`.
///
/// # Errors
/// - [`SecretsError::Open`] with `InvalidRecipientPrivateKey`: `kek_seed` is
///   not exactly 32 bytes (checked here because the domain separation would
///   otherwise hash any length into a valid-looking seed).
fn recipient_private_key(
    kem: KemAlgo,
    suite: Suite,
    kek_seed: &[u8],
) -> SecretsResult<RecipientPrivateKey> {
    if kek_seed.len() != KEK_SEED_LEN {
        return Err(SecretsError::Open(
            crypt_guard::pq_hpke::Error::InvalidRecipientPrivateKey,
        ));
    }
    let kem_seed = derive_kem_seed(kem, kek_seed);
    RecipientPrivateKey::from_seed_bytes(suite.kem(), kem_seed.as_slice())
        .map_err(SecretsError::Open)
}

const fn hpke_aead_for(aead: AeadAlgo) -> HpkeAead {
    match aead {
        AeadAlgo::XChaCha20Poly1305 => HpkeAead::XChaCha20Poly1305,
        AeadAlgo::AesGcmSiv => HpkeAead::Aes256GcmSiv,
    }
}

fn wrapper_aad(id: SecretId, key_version: KeyVersion, suite: Suite) -> Vec<u8> {
    let mut aad = Vec::with_capacity(V2_WRAPPER_AAD_DOMAIN.len() + 1 + 16 + 4 + 2 + 2);
    aad.extend_from_slice(V2_WRAPPER_AAD_DOMAIN);
    aad.push(0);
    aad.extend_from_slice(id.as_bytes());
    aad.extend_from_slice(&key_version.0.to_be_bytes());
    aad.extend_from_slice(&suite.kem().id().to_be_bytes());
    aad.extend_from_slice(&suite.aead().id().to_be_bytes());
    aad
}

/// This intentionally excludes `key_version` and KEM choice: rewrapping a DEK
/// changes only the HPKE layer, never the locally encrypted payload.
fn payload_aad(id: SecretId, aead: AeadAlgo) -> Vec<u8> {
    let mut aad = Vec::with_capacity(V2_PAYLOAD_AAD_DOMAIN.len() + 1 + 16 + 2);
    aad.extend_from_slice(V2_PAYLOAD_AAD_DOMAIN);
    aad.push(0);
    aad.extend_from_slice(id.as_bytes());
    aad.extend_from_slice(&hpke_aead_for(aead).id().to_be_bytes());
    aad
}

fn v1_record_aad(id: SecretId, key_version: KeyVersion, suite: Suite) -> Vec<u8> {
    let mut aad = Vec::with_capacity(V1_AAD_DOMAIN.len() + 1 + 16 + 4 + 2 + 2);
    aad.extend_from_slice(V1_AAD_DOMAIN);
    aad.push(0);
    aad.extend_from_slice(id.as_bytes());
    aad.extend_from_slice(&key_version.0.to_be_bytes());
    aad.extend_from_slice(&suite.kem().id().to_be_bytes());
    aad.extend_from_slice(&suite.aead().id().to_be_bytes());
    aad
}

fn v1_record_matches_envelope(
    record: &SecretRecord,
    envelope: &HpkeEnvelope,
    suite: Suite,
) -> bool {
    record.nonce.is_empty()
        && envelope.suite() == suite
        && envelope.encapsulation() == record.wrapped_dek
}

const fn hpke_authentication_failed() -> crypt_guard::pq_hpke::Error {
    crypt_guard::pq_hpke::Error::AuthenticationFailed
}

const fn hpke_internal_failure() -> crypt_guard::pq_hpke::Error {
    crypt_guard::pq_hpke::Error::InternalInvariant
}

#[cfg(test)]
mod tests {
    use crypt_guard::pq_hpke::{Kem, generate_recipient_key_pair};
    use secrecy::ExposeSecret;

    use super::*;
    use crate::test_support::{TestResult, ctx};

    const PROVENANCE_SEED: [u8; 32] = [0xA5; 32];
    const ROTATED_SEED: [u8; 32] = [0x5A; 32];
    const HYBRID_KEMS: [KemAlgo; 3] = [
        KemAlgo::MlKem768P256,
        KemAlgo::MlKem1024P384,
        KemAlgo::MlKem768X25519,
    ];
    const AEADS: [AeadAlgo; 2] = [AeadAlgo::XChaCha20Poly1305, AeadAlgo::AesGcmSiv];

    /// Recipient public key exactly as production derives it: root seed ->
    /// per-KEM domain-separated seed -> crypt_guard hybrid key.
    fn hybrid_public_key(kem: KemAlgo, seed: &[u8; 32]) -> TestResult<RecipientPublicKey> {
        raw_hybrid_public_key(kem, &derive_kem_seed(kem, seed))
    }

    /// crypt_guard hybrid key for `seed` used verbatim (no domain separation).
    fn raw_hybrid_public_key(kem: KemAlgo, seed: &[u8; 32]) -> TestResult<RecipientPublicKey> {
        let public_key = RecipientPrivateKey::from_seed_bytes(kem.hpke_kem()?, seed)
            .map_err(ctx("hybrid recipient private key from seed"))?
            .public_key()
            .map_err(ctx("hybrid recipient public key"))?;
        Ok(public_key)
    }

    fn sealed_record_with(kem: KemAlgo, aead: AeadAlgo) -> TestResult<SecretRecord> {
        let policy = CryptoPolicy { kem, aead };
        let id = SecretId::new();
        let key_version = KeyVersion::initial();
        let kek_public = hybrid_public_key(kem, &PROVENANCE_SEED)?;
        let sealed = seal(
            &policy,
            id,
            key_version,
            kek_public.as_bytes(),
            b"api-token",
        )?;
        Ok(SecretRecord {
            id,
            envelope_format: sealed.envelope_format,
            ciphertext: sealed.ciphertext,
            nonce: sealed.nonce,
            wrapped_dek: sealed.wrapped_dek,
            kem_algo: sealed.kem_algo,
            aead_algo: sealed.aead_algo,
            key_version,
        })
    }

    fn sealed_record(aead: AeadAlgo) -> TestResult<(Vec<u8>, SecretRecord)> {
        Ok((
            PROVENANCE_SEED.to_vec(),
            sealed_record_with(CryptoPolicy::strongest().kem, aead)?,
        ))
    }

    #[test]
    fn v2_round_trips_for_every_hybrid_kem_and_aead() -> TestResult {
        for kem in HYBRID_KEMS {
            for aead in AEADS {
                let record = sealed_record_with(kem, aead)?;
                let plaintext = open(&PROVENANCE_SEED, &record)?;

                assert_eq!(plaintext.expose_secret().as_ref(), b"api-token");
                assert_eq!(record.envelope_format, SecretEnvelopeFormat::DekWrappedV2);
                assert_eq!(record.kem_algo, kem);
                assert_eq!(record.aead_algo, aead);
                assert_eq!(record.nonce.len(), nonce_len(aead));
                assert_eq!(&record.wrapped_dek[..4], b"CGH3");
                assert_ne!(record.ciphertext, record.wrapped_dek);

                let wrapper = HpkeEnvelope::from_bytes(&record.wrapped_dek)
                    .map_err(ctx("decode V2 wrapper"))?;
                assert_eq!(wrapper.suite().kem(), kem.hpke_kem()?);
                assert_eq!(wrapper.suite().kdf(), Kdf::Shake256);
                assert_eq!(wrapper.suite().aead(), hpke_aead_for(aead));
            }
        }
        Ok(())
    }

    #[test]
    fn v2_tampering_is_rejected_at_each_layer() -> TestResult {
        let (seed, record) = sealed_record(AeadAlgo::XChaCha20Poly1305)?;
        for tampered in [
            {
                let mut value = record.clone();
                value.ciphertext[0] ^= 1;
                value
            },
            {
                let mut value = record.clone();
                value.nonce[0] ^= 1;
                value
            },
            {
                let mut value = record.clone();
                value.wrapped_dek[0] ^= 1;
                value
            },
        ] {
            assert!(open(&seed, &tampered).is_err());
        }
        Ok(())
    }

    #[test]
    fn v2_open_rejects_a_record_whose_kem_label_differs_from_the_wrapper_suite() -> TestResult {
        let (seed, mut record) = sealed_record(AeadAlgo::XChaCha20Poly1305)?;
        record.kem_algo = KemAlgo::MlKem768X25519;

        assert!(matches!(
            open(&seed, &record),
            Err(SecretsError::Open(
                crypt_guard::pq_hpke::Error::AuthenticationFailed
            ))
        ));
        Ok(())
    }

    #[test]
    fn v2_open_with_a_different_kek_seed_fails() -> TestResult {
        let (_, record) = sealed_record(AeadAlgo::AesGcmSiv)?;

        assert!(matches!(
            open(&ROTATED_SEED, &record),
            Err(SecretsError::Open(_))
        ));
        Ok(())
    }

    #[test]
    fn open_applies_domain_separation_and_rejects_keys_of_the_raw_root_seed() -> TestResult {
        for kem in HYBRID_KEMS {
            let policy = CryptoPolicy {
                kem,
                aead: AeadAlgo::XChaCha20Poly1305,
            };
            let id = SecretId::new();
            let key_version = KeyVersion::initial();
            // Wrapped for the root seed used verbatim, i.e. without the
            // per-KEM domain separation: the production open path must not
            // find the matching private key.
            let raw_public = raw_hybrid_public_key(kem, &PROVENANCE_SEED)?;
            let sealed = seal(
                &policy,
                id,
                key_version,
                raw_public.as_bytes(),
                b"api-token",
            )?;
            let record = SecretRecord {
                id,
                envelope_format: sealed.envelope_format,
                ciphertext: sealed.ciphertext,
                nonce: sealed.nonce,
                wrapped_dek: sealed.wrapped_dek,
                kem_algo: sealed.kem_algo,
                aead_algo: sealed.aead_algo,
                key_version,
            };

            assert!(matches!(
                open(&PROVENANCE_SEED, &record),
                Err(SecretsError::Open(_))
            ));
        }
        Ok(())
    }

    #[test]
    fn open_rejects_a_kek_seed_that_is_not_32_bytes() -> TestResult {
        let (_, record) = sealed_record(AeadAlgo::XChaCha20Poly1305)?;

        for length in [0, 31, 33, 64] {
            assert!(matches!(
                open(&vec![0xA5_u8; length], &record),
                Err(SecretsError::Open(
                    crypt_guard::pq_hpke::Error::InvalidRecipientPrivateKey
                ))
            ));
        }
        Ok(())
    }

    #[test]
    fn seal_rejects_a_public_key_of_a_different_hybrid_kem() -> TestResult {
        let policy = CryptoPolicy::ml_kem_768_p256();
        let foreign = hybrid_public_key(KemAlgo::MlKem768X25519, &PROVENANCE_SEED)?;

        assert!(matches!(
            seal(
                &policy,
                SecretId::new(),
                KeyVersion::initial(),
                foreign.as_bytes(),
                b"api-token",
            ),
            Err(SecretsError::Seal(_))
        ));
        Ok(())
    }

    #[test]
    fn v2_payload_authentication_failure_uses_the_typed_local_aead_error() -> TestResult {
        let (seed, mut record) = sealed_record(AeadAlgo::XChaCha20Poly1305)?;
        let last = record.ciphertext.len() - 1;
        record.ciphertext[last] ^= 1;

        assert!(matches!(
            open(&seed, &record),
            Err(SecretsError::PayloadAeadOpen(AeadAlgo::XChaCha20Poly1305))
        ));
        Ok(())
    }

    #[test]
    fn rewrap_v2_preserves_payload_bytes_and_changes_only_wrapper() -> TestResult {
        for kem in HYBRID_KEMS {
            let record = sealed_record_with(kem, AeadAlgo::XChaCha20Poly1305)?;
            let rotated_public = hybrid_public_key(kem, &ROTATED_SEED)?;

            let mut rotated = record.clone();
            rotated.key_version = record.key_version.next();
            rotated.wrapped_dek = rewrap_v2(
                &PROVENANCE_SEED,
                &record,
                rotated_public.as_bytes(),
                rotated.key_version,
            )?;

            assert_eq!(rotated.ciphertext, record.ciphertext);
            assert_eq!(rotated.nonce, record.nonce);
            assert_ne!(rotated.wrapped_dek, record.wrapped_dek);
            assert_eq!(
                open(&ROTATED_SEED, &rotated)?.expose_secret().as_ref(),
                b"api-token"
            );
            assert!(open(&PROVENANCE_SEED, &rotated).is_err());
        }
        Ok(())
    }

    #[test]
    fn legacy_direct_hpke_records_still_open() -> TestResult {
        let policy = CryptoPolicy::ml_kem_768_x25519();
        let suite = suite_for(policy.kem, policy.aead)?;
        let kek_public = hybrid_public_key(policy.kem, &PROVENANCE_SEED)?;
        let id = SecretId::new();
        let key_version = KeyVersion::initial();
        let envelope = HpkeEnvelope::seal(
            suite,
            &kek_public,
            HPKE_INFO_V1,
            &v1_record_aad(id, key_version, suite),
            b"legacy-api-token",
        )
        .map_err(ctx("seal legacy envelope"))?;
        let record = SecretRecord {
            id,
            envelope_format: SecretEnvelopeFormat::LegacyDirectHpke,
            ciphertext: envelope.to_bytes(),
            nonce: Vec::new(),
            wrapped_dek: envelope.encapsulation().to_vec(),
            kem_algo: policy.kem,
            aead_algo: policy.aead,
            key_version,
        };

        assert_eq!(
            open(&PROVENANCE_SEED, &record)?.expose_secret().as_ref(),
            b"legacy-api-token"
        );

        let next_public = hybrid_public_key(policy.kem, &ROTATED_SEED)?;
        assert!(matches!(
            rewrap_v2(
                &PROVENANCE_SEED,
                &record,
                next_public.as_bytes(),
                key_version.next(),
            ),
            Err(SecretsError::PersistenceFormat { .. })
        ));
        Ok(())
    }

    #[test]
    fn seal_refuses_a_legacy_pure_ml_kem_policy() {
        let policy = CryptoPolicy {
            kem: KemAlgo::LegacyMlKem1024,
            aead: AeadAlgo::XChaCha20Poly1305,
        };

        assert!(matches!(
            seal(
                &policy,
                SecretId::new(),
                KeyVersion::initial(),
                &[0_u8; 8],
                b"api-token",
            ),
            Err(SecretsError::UnsupportedLegacyKem { algo }) if algo == "ml_kem_1024"
        ));
    }

    #[test]
    fn records_sealed_under_legacy_pure_ml_kem_are_unreadable() -> TestResult {
        for (legacy, kem, wire) in [
            (KemAlgo::LegacyMlKem512, Kem::MlKem512, "ml_kem_512"),
            (KemAlgo::LegacyMlKem768, Kem::MlKem768, "ml_kem_768"),
            (KemAlgo::LegacyMlKem1024, Kem::MlKem1024, "ml_kem_1024"),
        ] {
            // Ein echter, mit crypt_guard 3.0.1 erzeugter reiner ML-KEM-Umschlag:
            // Das Format wäre dekodierbar, der KEM ist aber nicht mehr zulässig.
            let keys = generate_recipient_key_pair(kem).map_err(ctx("pure ML-KEM keypair"))?;
            let suite = Suite::new(kem, Kdf::Shake256, HpkeAead::XChaCha20Poly1305);
            let envelope = HpkeEnvelope::seal(
                suite,
                keys.public_key(),
                HPKE_INFO_V2,
                b"legacy-wrapper-aad",
                &[0x11; DEK_LEN],
            )
            .map_err(ctx("seal pure ML-KEM envelope"))?;
            let v2_record = SecretRecord {
                id: SecretId::new(),
                envelope_format: SecretEnvelopeFormat::DekWrappedV2,
                ciphertext: vec![0xde, 0xad, 0xbe, 0xef],
                nonce: vec![0_u8; nonce_len(AeadAlgo::XChaCha20Poly1305)],
                wrapped_dek: envelope.to_bytes(),
                kem_algo: legacy,
                aead_algo: AeadAlgo::XChaCha20Poly1305,
                key_version: KeyVersion::initial(),
            };
            let direct_record = SecretRecord {
                envelope_format: SecretEnvelopeFormat::LegacyDirectHpke,
                ciphertext: envelope.to_bytes(),
                nonce: Vec::new(),
                wrapped_dek: envelope.encapsulation().to_vec(),
                ..v2_record.clone()
            };

            for record in [&v2_record, &direct_record] {
                assert!(matches!(
                    open(&PROVENANCE_SEED, record),
                    Err(SecretsError::UnsupportedLegacyKem { algo }) if algo == wire
                ));
            }
            let rotated_public = hybrid_public_key(KemAlgo::MlKem1024P384, &ROTATED_SEED)?;
            assert!(matches!(
                rewrap_v2(
                    &PROVENANCE_SEED,
                    &v2_record,
                    rotated_public.as_bytes(),
                    KeyVersion::initial().next(),
                ),
                Err(SecretsError::UnsupportedLegacyKem { algo }) if algo == wire
            ));
        }
        Ok(())
    }

    #[test]
    fn v2_rejects_invalid_payload_nonce_with_typed_error() -> TestResult {
        let (seed, mut record) = sealed_record(AeadAlgo::AesGcmSiv)?;
        record.nonce.pop();

        assert!(matches!(
            open(&seed, &record),
            Err(SecretsError::InvalidPayloadNonceLength {
                algorithm: AeadAlgo::AesGcmSiv,
                actual: 11,
            })
        ));
        Ok(())
    }

    #[test]
    fn v2_rejects_a_wrapped_dek_with_invalid_length() -> TestResult {
        let (seed, mut record) = sealed_record(AeadAlgo::XChaCha20Poly1305)?;
        let suite = suite_for(record.kem_algo, record.aead_algo)?;
        let kek_public = hybrid_public_key(record.kem_algo, &PROVENANCE_SEED)?;
        let malformed_dek = [0xC3; DEK_LEN - 1];
        record.wrapped_dek = HpkeEnvelope::seal(
            suite,
            &kek_public,
            HPKE_INFO_V2,
            &wrapper_aad(record.id, record.key_version, suite),
            &malformed_dek,
        )
        .map_err(ctx("seal malformed wrapper payload"))?
        .to_bytes();

        assert!(matches!(
            open(&seed, &record),
            Err(SecretsError::InvalidDekLength { actual: 31 })
        ));
        Ok(())
    }
}
