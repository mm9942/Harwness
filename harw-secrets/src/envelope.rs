//! Versioned secret-envelope boundary.
//!
//! V2 keeps the payload and KEK lifecycle independent: a fresh random DEK
//! encrypts the payload locally, while crypt_guard's v3 PQ HPKE transports
//! only that DEK.  The legacy direct-HPKE record remains readable through its
//! explicit record discriminator.

use aes_gcm_siv::{
    aead::{Aead as _, KeyInit as _, Payload as AesPayload},
    Aes256GcmSiv, Nonce as AesNonce,
};
use chacha20poly1305::{aead::Payload as ChaChaPayload, XChaCha20Poly1305, XNonce};
use crypt_guard::pq_hpke::{
    derive_recipient_key_pair, Aead as HpkeAead, HpkeEnvelope, Kdf, Kem, RecipientPublicKey, Suite,
};
use secrecy::SecretBox;

use crate::error::{SecretsError, SecretsResult};
use crate::id::{KeyVersion, SecretId};
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
    /// KEM level encoded by the HPKE wrapper suite.
    pub kem_algo: KemAlgo,
    /// AEAD algorithm selected for the local payload and wrapper suite.
    pub aead_algo: AeadAlgo,
}

/// Seal a new V2 record. A fresh 32-byte DEK encrypts `plaintext` locally;
/// crypt_guard v3 PQ HPKE only wraps that DEK for `kek_public`.
pub fn seal(
    policy: &CryptoPolicy,
    id: SecretId,
    key_version: KeyVersion,
    kek_public: &[u8],
    plaintext: &[u8],
) -> SecretsResult<SealedSecret> {
    let mut dek = [0_u8; DEK_LEN];
    getrandom::fill(&mut dek).map_err(|_| SecretsError::Seal(hpke_internal_failure()))?;

    let mut nonce = vec![0_u8; nonce_len(policy.aead)];
    getrandom::fill(&mut nonce).map_err(|_| SecretsError::Seal(hpke_internal_failure()))?;

    let payload_aad = payload_aad(id, policy.aead);
    let ciphertext = encrypt_payload(policy.aead, &dek, &nonce, &payload_aad, plaintext)?;

    let suite = suite_for(policy.kem, policy.aead);
    let recipient =
        RecipientPublicKey::from_bytes(suite.kem(), kek_public).map_err(SecretsError::Seal)?;
    let wrapper_aad = wrapper_aad(id, key_version, policy.kem, policy.aead);
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
/// separately rewrappable DEK.
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

    let dek = unwrap_v2_dek(old_kek_seed, record)?;
    let suite = suite_for(record.kem_algo, record.aead_algo);
    let next_recipient =
        RecipientPublicKey::from_bytes(suite.kem(), next_kek_public).map_err(SecretsError::Seal)?;
    let next_wrapper_aad = wrapper_aad(
        record.id,
        next_key_version,
        record.kem_algo,
        record.aead_algo,
    );
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
    let dek = unwrap_v2_dek(kek_seed, record)?;
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

fn unwrap_v2_dek(kek_seed: &[u8], record: &SecretRecord) -> SecretsResult<[u8; DEK_LEN]> {
    let wrapper =
        HpkeEnvelope::from_bytes(&record.wrapped_dek).map_err(SecretsError::EnvelopeDecode)?;
    if wrapper.suite() != suite_for(record.kem_algo, record.aead_algo) {
        return Err(SecretsError::Open(hpke_authentication_failed()));
    }

    let recipient = derive_recipient_key_pair(kem_for(record.kem_algo), kek_seed)
        .map_err(SecretsError::Open)?;
    let wrapper_aad = wrapper_aad(
        record.id,
        record.key_version,
        record.kem_algo,
        record.aead_algo,
    );
    let dek = wrapper
        .open(recipient.private_key(), HPKE_INFO_V2, &wrapper_aad)
        .map_err(SecretsError::Open)?;
    dek.try_into()
        .map_err(|dek: Vec<u8>| SecretsError::InvalidDekLength { actual: dek.len() })
}

/// Retained direct-HPKE V1 compatibility path. New records are always V2.
fn open_v1(kek_seed: &[u8], record: &SecretRecord) -> SecretsResult<SecretBox<[u8]>> {
    let envelope =
        HpkeEnvelope::from_bytes(&record.ciphertext).map_err(SecretsError::EnvelopeDecode)?;
    if !v1_record_matches_envelope(record, &envelope) {
        return Err(SecretsError::Open(hpke_authentication_failed()));
    }

    let recipient = derive_recipient_key_pair(kem_for(record.kem_algo), kek_seed)
        .map_err(SecretsError::Open)?;
    let aad = v1_record_aad(
        record.id,
        record.key_version,
        record.kem_algo,
        record.aead_algo,
    );
    let plaintext = envelope
        .open(recipient.private_key(), HPKE_INFO_V1, &aad)
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

fn suite_for(kem: KemAlgo, aead: AeadAlgo) -> Suite {
    Suite::new(kem_for(kem), Kdf::Shake256, hpke_aead_for(aead))
}

const fn kem_for(kem: KemAlgo) -> Kem {
    match kem {
        KemAlgo::MlKem512 => Kem::MlKem512,
        KemAlgo::MlKem768 => Kem::MlKem768,
        KemAlgo::MlKem1024 => Kem::MlKem1024,
    }
}

const fn hpke_aead_for(aead: AeadAlgo) -> HpkeAead {
    match aead {
        AeadAlgo::XChaCha20Poly1305 => HpkeAead::XChaCha20Poly1305,
        AeadAlgo::AesGcmSiv => HpkeAead::Aes256GcmSiv,
    }
}

fn wrapper_aad(id: SecretId, key_version: KeyVersion, kem: KemAlgo, aead: AeadAlgo) -> Vec<u8> {
    let suite = suite_for(kem, aead);
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

fn v1_record_aad(id: SecretId, key_version: KeyVersion, kem: KemAlgo, aead: AeadAlgo) -> Vec<u8> {
    let suite = suite_for(kem, aead);
    let mut aad = Vec::with_capacity(V1_AAD_DOMAIN.len() + 1 + 16 + 4 + 2 + 2);
    aad.extend_from_slice(V1_AAD_DOMAIN);
    aad.push(0);
    aad.extend_from_slice(id.as_bytes());
    aad.extend_from_slice(&key_version.0.to_be_bytes());
    aad.extend_from_slice(&suite.kem().id().to_be_bytes());
    aad.extend_from_slice(&suite.aead().id().to_be_bytes());
    aad
}

fn v1_record_matches_envelope(record: &SecretRecord, envelope: &HpkeEnvelope) -> bool {
    record.nonce.is_empty()
        && envelope.suite() == suite_for(record.kem_algo, record.aead_algo)
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
    use crypt_guard::pq_hpke::{derive_recipient_key_pair, Kem};
    use secrecy::ExposeSecret;

    use super::*;

    const PROVENANCE_SEED: [u8; 32] = [0xA5; 32];

    fn sealed_record(aead: AeadAlgo) -> (Vec<u8>, SecretRecord) {
        let policy = CryptoPolicy {
            kem: KemAlgo::MlKem512,
            aead,
        };
        let keys = derive_recipient_key_pair(Kem::MlKem512, &PROVENANCE_SEED)
            .expect("deterministic ML-KEM keypair");
        let id = SecretId::new();
        let key_version = KeyVersion::initial();
        let sealed = seal(
            &policy,
            id,
            key_version,
            keys.public_key().as_bytes(),
            b"api-token",
        )
        .expect("V2 seal");
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
        (PROVENANCE_SEED.to_vec(), record)
    }

    #[test]
    fn v2_round_trips_for_each_policy_aead() {
        for aead in [AeadAlgo::XChaCha20Poly1305, AeadAlgo::AesGcmSiv] {
            let (seed, record) = sealed_record(aead);
            let plaintext = open(&seed, &record).expect("V2 open");

            assert_eq!(plaintext.expose_secret().as_ref(), b"api-token");
            assert_eq!(record.envelope_format, SecretEnvelopeFormat::DekWrappedV2);
            assert_eq!(record.nonce.len(), nonce_len(aead));
            assert_eq!(&record.wrapped_dek[..4], b"CGH3");
            assert_ne!(record.ciphertext, record.wrapped_dek);
        }
    }

    #[test]
    fn v2_tampering_is_rejected_at_each_layer() {
        let (seed, record) = sealed_record(AeadAlgo::XChaCha20Poly1305);
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
    }

    #[test]
    fn v2_payload_authentication_failure_uses_the_typed_local_aead_error() {
        let (seed, mut record) = sealed_record(AeadAlgo::XChaCha20Poly1305);
        let last = record.ciphertext.len() - 1;
        record.ciphertext[last] ^= 1;

        assert!(matches!(
            open(&seed, &record),
            Err(SecretsError::PayloadAeadOpen(AeadAlgo::XChaCha20Poly1305))
        ));
    }

    #[test]
    fn rewrap_v2_preserves_payload_bytes_and_changes_only_wrapper() {
        const ROTATED_SEED: [u8; 32] = [0x5A; 32];

        let (seed, record) = sealed_record(AeadAlgo::XChaCha20Poly1305);
        let rotated_keys =
            derive_recipient_key_pair(Kem::MlKem512, &ROTATED_SEED).expect("rotated keypair");

        let mut rotated = record.clone();
        rotated.key_version = record.key_version.next();
        rotated.wrapped_dek = rewrap_v2(
            &seed,
            &record,
            rotated_keys.public_key().as_bytes(),
            rotated.key_version,
        )
        .expect("rewrap V2 DEK");

        assert_eq!(rotated.ciphertext, record.ciphertext);
        assert_eq!(rotated.nonce, record.nonce);
        assert_ne!(rotated.wrapped_dek, record.wrapped_dek);
        assert_eq!(
            open(&ROTATED_SEED, &rotated)
                .expect("open rewrapped record")
                .expose_secret()
                .as_ref(),
            b"api-token"
        );
    }

    #[test]
    fn legacy_direct_hpke_records_still_open() {
        let policy = CryptoPolicy::ml_kem_512();
        let keys = derive_recipient_key_pair(Kem::MlKem512, &PROVENANCE_SEED)
            .expect("deterministic ML-KEM keypair");
        let id = SecretId::new();
        let key_version = KeyVersion::initial();
        let envelope = HpkeEnvelope::seal(
            suite_for(policy.kem, policy.aead),
            keys.public_key(),
            HPKE_INFO_V1,
            &v1_record_aad(id, key_version, policy.kem, policy.aead),
            b"legacy-api-token",
        )
        .expect("seal legacy envelope");
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
            open(&PROVENANCE_SEED, &record)
                .expect("open legacy record")
                .expose_secret()
                .as_ref(),
            b"legacy-api-token"
        );

        let next_keys = derive_recipient_key_pair(Kem::MlKem512, &[0x5A; 32])
            .expect("next deterministic ML-KEM keypair");
        assert!(matches!(
            rewrap_v2(
                &PROVENANCE_SEED,
                &record,
                next_keys.public_key().as_bytes(),
                key_version.next(),
            ),
            Err(SecretsError::PersistenceFormat { .. })
        ));
    }

    #[test]
    fn v2_rejects_invalid_payload_nonce_with_typed_error() {
        let (seed, mut record) = sealed_record(AeadAlgo::AesGcmSiv);
        record.nonce.pop();

        assert!(matches!(
            open(&seed, &record),
            Err(SecretsError::InvalidPayloadNonceLength {
                algorithm: AeadAlgo::AesGcmSiv,
                actual: 11,
            })
        ));
    }

    #[test]
    fn v2_rejects_a_wrapped_dek_with_invalid_length() {
        let (seed, mut record) = sealed_record(AeadAlgo::XChaCha20Poly1305);
        let keys = derive_recipient_key_pair(Kem::MlKem512, &seed).expect("current keypair");
        let malformed_dek = [0xC3; DEK_LEN - 1];
        record.wrapped_dek = HpkeEnvelope::seal(
            suite_for(record.kem_algo, record.aead_algo),
            keys.public_key(),
            HPKE_INFO_V2,
            &wrapper_aad(
                record.id,
                record.key_version,
                record.kem_algo,
                record.aead_algo,
            ),
            &malformed_dek,
        )
        .expect("seal malformed wrapper payload")
        .to_bytes();

        assert!(matches!(
            open(&seed, &record),
            Err(SecretsError::InvalidDekLength { actual: 31 })
        ));
    }
}
