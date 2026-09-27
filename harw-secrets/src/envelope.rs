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
use secrecy::zeroize::Zeroizing;
use secrecy::{ExposeSecret, SecretBox};

use crate::dek_wrapper::{DekWrapper, validate_wrapper_identity};
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
/// Domain of the AAD a V3 record hands to [`DekWrapper::wrap_dek`].
const V3_WRAP_AAD_DOMAIN: &[u8] = b"harwness:secrets:dek-wrap:v3";
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
///   retired pure ML-KEM level; such envelopes are rejected by `harw-secrets`
///   policy (legacy pure ML-KEM records are read-only metadata), not by a
///   `crypt_guard` limitation.
/// - [`SecretsError::EnvelopeDecode`], [`SecretsError::Open`],
///   [`SecretsError::PayloadAeadOpen`] and the length errors for malformed or
///   tampered records.
///
/// [`SecretEnvelopeFormat::KmsWrappedV3`] records cannot be opened with a KEK
/// seed alone and fail closed with [`SecretsError::DekWrapperUnavailable`];
/// use [`open_with_wrapper`] or [`open_v3`].
pub fn open(kek_seed: &[u8], record: &SecretRecord) -> SecretsResult<SecretBox<[u8]>> {
    open_with_wrapper(Some(kek_seed), None, record)
}

/// Open a record of any envelope format: V1/V2 with the local root
/// `kek_seed`, V3 with `wrapper`. A missing input for the record's format
/// fails closed; there is no fallback between the two paths.
///
/// # Errors
/// - [`SecretsError::KekUnavailable`]: a V1/V2 record but no `kek_seed`.
/// - [`SecretsError::DekWrapperUnavailable`]: a V3 record but no `wrapper`.
/// - Everything [`open`] and [`open_v3`] report for their formats.
pub fn open_with_wrapper(
    kek_seed: Option<&[u8]>,
    wrapper: Option<&dyn DekWrapper>,
    record: &SecretRecord,
) -> SecretsResult<SecretBox<[u8]>> {
    match record.envelope_format {
        SecretEnvelopeFormat::DekWrappedV2 => open_v2(require_kek_seed(kek_seed)?, record),
        SecretEnvelopeFormat::LegacyDirectHpke => open_v1(require_kek_seed(kek_seed)?, record),
        SecretEnvelopeFormat::KmsWrappedV3 => match wrapper {
            Some(wrapper) => open_v3(wrapper, record),
            None => Err(SecretsError::DekWrapperUnavailable {
                profile: record
                    .crypto_profile_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_owned()),
                reason: "V3 record requires a configured DEK wrapper".to_owned(),
            }),
        },
    }
}

fn require_kek_seed(kek_seed: Option<&[u8]>) -> SecretsResult<&[u8]> {
    kek_seed.ok_or_else(|| SecretsError::KekUnavailable {
        kind: "seed".to_owned(),
        reason: "V1/V2 record requires the local root KEK seed".to_owned(),
    })
}

/// Seal a new [`SecretEnvelopeFormat::KmsWrappedV3`] record: a fresh 32-byte
/// DEK encrypts `plaintext` locally under `policy.aead` with the V2 payload
/// AAD, and `wrapper` wraps only that DEK under its current key generation.
///
/// The wrap AAD binds `"harwness:secrets:dek-wrap:v3" ‖ 0x00 ‖ id ‖ key_id ‖
/// key_generation ‖ crypto_profile_id ‖ KEM id ‖ AEAD id` (strings are
/// length-prefixed). `record.key_version` mirrors the generation.
/// `policy.kem` is recorded (and bound) for the record's suite label; the
/// wrapper's own crypto profile decides how the DEK is protected.
///
/// # Errors
/// - [`SecretsError::InvalidDekWrapperIdentity`]: the wrapper reports a
///   malformed key reference or profile id; nothing is encrypted.
/// - [`SecretsError::UnsupportedLegacyKem`]: `policy` names a retired pure
///   ML-KEM level.
/// - [`SecretsError::PayloadAeadSeal`] and any error of
///   [`DekWrapper::wrap_dek`] (e.g. [`SecretsError::DekWrapperUnavailable`]).
pub fn seal_v3(
    wrapper: &dyn DekWrapper,
    policy: &CryptoPolicy,
    id: SecretId,
    plaintext: &[u8],
) -> SecretsResult<SecretRecord> {
    validate_wrapper_identity(wrapper.key_id(), wrapper.profile_id())?;
    // Refuse a legacy KEM before any key material is generated.
    let _ = suite_for(policy.kem, policy.aead)?;
    let (dek, nonce, ciphertext) = encrypt_payload_under_fresh_dek(policy.aead, id, plaintext)?;
    v3_record(
        wrapper,
        id,
        &dek,
        policy.kem,
        policy.aead,
        ciphertext,
        nonce,
    )
}

/// Open a [`SecretEnvelopeFormat::KmsWrappedV3`] record through `wrapper`.
///
/// # Errors
/// - [`SecretsError::PersistenceFormat`]: not a V3 record, a V3 field is
///   missing, or `key_version` does not mirror `key_generation`.
/// - [`SecretsError::DekWrapperMismatch`]: the record names another key
///   reference or crypto profile than `wrapper` serves.
/// - [`SecretsError::KeyGenerationMismatch`] (from the wrapper): the record's
///   generation cannot be unwrapped.
/// - Authentication errors of the wrapper for any tampered bound field
///   (secret id, key id, generation, profile, KEM, AEAD) or wrapped bytes;
///   [`SecretsError::InvalidDekLength`], [`SecretsError::PayloadAeadOpen`].
pub fn open_v3(wrapper: &dyn DekWrapper, record: &SecretRecord) -> SecretsResult<SecretBox<[u8]>> {
    let dek = unwrap_v3_dek(wrapper, record)?;
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

/// Explicitly migrate a V1 or V2 record to [`SecretEnvelopeFormat::KmsWrappedV3`]
/// (never done implicitly; see [`crate::SecretStore::migrate_all_to_v3`]).
///
/// - V2: the DEK is unwrapped once with the existing root `kek_seed` and
///   re-wrapped by `wrapper`; `ciphertext`, `nonce`, KEM and AEAD are kept
///   byte for byte (§16.3).
/// - V1 (direct HPKE) has no separable DEK: the payload is opened with
///   `kek_seed` and re-encrypted under a fresh DEK and nonce.
///
/// The returned record carries the wrapper's key reference, generation and
/// profile; `key_version` mirrors the generation.
///
/// # Errors
/// - [`SecretsError::PersistenceFormat`]: `record` is already V3.
/// - Every open error of the record's format and every seal error of
///   [`seal_v3`].
pub fn rewrap_to_v3(
    kek_seed: &[u8],
    record: &SecretRecord,
    wrapper: &dyn DekWrapper,
) -> SecretsResult<SecretRecord> {
    validate_wrapper_identity(wrapper.key_id(), wrapper.profile_id())?;
    match record.envelope_format {
        SecretEnvelopeFormat::DekWrappedV2 => {
            let suite = suite_for(record.kem_algo, record.aead_algo)?;
            let dek = Zeroizing::new(unwrap_v2_dek(kek_seed, record, suite)?);
            v3_record(
                wrapper,
                record.id,
                &dek,
                record.kem_algo,
                record.aead_algo,
                record.ciphertext.clone(),
                record.nonce.clone(),
            )
        }
        SecretEnvelopeFormat::LegacyDirectHpke => {
            let plaintext = open_v1(kek_seed, record)?;
            let policy = CryptoPolicy {
                kem: record.kem_algo,
                aead: record.aead_algo,
            };
            seal_v3(wrapper, &policy, record.id, plaintext.expose_secret())
        }
        SecretEnvelopeFormat::KmsWrappedV3 => Err(SecretsError::PersistenceFormat {
            operation: "rewrap to V3".to_owned(),
            reason: "record is already KMS-wrapped V3".to_owned(),
        }),
    }
}

/// Re-wrap a V3 record's DEK from `old_wrapper` to `new_wrapper` (a KMS key
/// generation rotation or a key/profile change) without touching the payload
/// ciphertext or nonce.
///
/// # Errors
/// Every error of [`open_v3`] for `old_wrapper` and of
/// [`DekWrapper::wrap_dek`] for `new_wrapper`.
pub fn rewrap_v3(
    old_wrapper: &dyn DekWrapper,
    record: &SecretRecord,
    new_wrapper: &dyn DekWrapper,
) -> SecretsResult<SecretRecord> {
    validate_wrapper_identity(new_wrapper.key_id(), new_wrapper.profile_id())?;
    let dek = unwrap_v3_dek(old_wrapper, record)?;
    v3_record(
        new_wrapper,
        record.id,
        &dek,
        record.kem_algo,
        record.aead_algo,
        record.ciphertext.clone(),
        record.nonce.clone(),
    )
}

/// DEK, nonce and ciphertext produced by sealing a payload.
type SealedPayload = (Zeroizing<[u8; DEK_LEN]>, Vec<u8>, Vec<u8>);

/// Fresh random DEK and nonce, payload encrypted with the V2 payload AAD.
fn encrypt_payload_under_fresh_dek(
    aead: AeadAlgo,
    id: SecretId,
    plaintext: &[u8],
) -> SecretsResult<SealedPayload> {
    let mut dek = Zeroizing::new([0_u8; DEK_LEN]);
    getrandom::fill(&mut *dek).map_err(|_| SecretsError::Seal(hpke_internal_failure()))?;
    let mut nonce = vec![0_u8; nonce_len(aead)];
    getrandom::fill(&mut nonce).map_err(|_| SecretsError::Seal(hpke_internal_failure()))?;
    let ciphertext = encrypt_payload(aead, &dek, &nonce, &payload_aad(id, aead), plaintext)?;
    Ok((dek, nonce, ciphertext))
}

/// Wrap `dek` with `wrapper` and assemble the V3 record around the given
/// payload ciphertext and nonce.
fn v3_record(
    wrapper: &dyn DekWrapper,
    id: SecretId,
    dek: &[u8; DEK_LEN],
    kem: KemAlgo,
    aead: AeadAlgo,
    ciphertext: Vec<u8>,
    nonce: Vec<u8>,
) -> SecretsResult<SecretRecord> {
    let key_id = wrapper.key_id().to_owned();
    let profile_id = wrapper.profile_id().to_owned();
    let key_generation = wrapper.key_generation();
    let aad = v3_wrap_aad(id, &key_id, key_generation, &profile_id, kem, aead)?;
    let wrapped_dek = wrapper.wrap_dek(dek, &aad)?;
    Ok(SecretRecord {
        id,
        envelope_format: SecretEnvelopeFormat::KmsWrappedV3,
        ciphertext,
        nonce,
        wrapped_dek,
        kem_algo: kem,
        aead_algo: aead,
        key_version: KeyVersion(key_generation),
        key_id: Some(key_id),
        key_generation: Some(key_generation),
        crypto_profile_id: Some(profile_id),
    })
}

fn unwrap_v3_dek(
    wrapper: &dyn DekWrapper,
    record: &SecretRecord,
) -> SecretsResult<Zeroizing<[u8; DEK_LEN]>> {
    if record.envelope_format != SecretEnvelopeFormat::KmsWrappedV3 {
        return Err(SecretsError::PersistenceFormat {
            operation: "open V3 DEK".to_owned(),
            reason: "record is not KMS-wrapped V3".to_owned(),
        });
    }
    let (Some(key_id), Some(key_generation), Some(profile_id)) = (
        record.key_id.as_deref(),
        record.key_generation,
        record.crypto_profile_id.as_deref(),
    ) else {
        return Err(SecretsError::PersistenceFormat {
            operation: "open V3 DEK".to_owned(),
            reason: "V3 record lacks key_id, key_generation or crypto_profile_id".to_owned(),
        });
    };
    if record.key_version.0 != key_generation {
        return Err(SecretsError::PersistenceFormat {
            operation: "open V3 DEK".to_owned(),
            reason: "V3 key_version does not mirror key_generation".to_owned(),
        });
    }
    if profile_id != wrapper.profile_id() {
        return Err(SecretsError::DekWrapperMismatch {
            field: "crypto_profile_id".to_owned(),
        });
    }
    if key_id != wrapper.key_id() {
        return Err(SecretsError::DekWrapperMismatch {
            field: "key_id".to_owned(),
        });
    }
    let aad = v3_wrap_aad(
        record.id,
        key_id,
        key_generation,
        profile_id,
        record.kem_algo,
        record.aead_algo,
    )?;
    let unwrapped = wrapper.unwrap_dek(&record.wrapped_dek, key_generation, &aad)?;
    if unwrapped.len() != DEK_LEN {
        return Err(SecretsError::InvalidDekLength {
            actual: unwrapped.len(),
        });
    }
    let mut dek = Zeroizing::new([0_u8; DEK_LEN]);
    dek.copy_from_slice(&unwrapped);
    Ok(dek)
}

/// `domain ‖ 0x00 ‖ id ‖ u32(len) key_id ‖ key_generation ‖ u32(len)
/// profile_id ‖ KEM id ‖ AEAD id`, all integers big endian.
///
/// # Errors
/// [`SecretsError::UnsupportedLegacyKem`] for a retired pure ML-KEM label;
/// [`SecretsError::InvalidDekWrapperIdentity`] for an over-long string.
fn v3_wrap_aad(
    id: SecretId,
    key_id: &str,
    key_generation: u32,
    profile_id: &str,
    kem: KemAlgo,
    aead: AeadAlgo,
) -> SecretsResult<Vec<u8>> {
    let suite = suite_for(kem, aead)?;
    let mut aad = Vec::with_capacity(
        V3_WRAP_AAD_DOMAIN.len() + 1 + 16 + 4 + key_id.len() + 4 + 4 + profile_id.len() + 2 + 2,
    );
    aad.extend_from_slice(V3_WRAP_AAD_DOMAIN);
    aad.push(0);
    aad.extend_from_slice(id.as_bytes());
    push_length_prefixed(&mut aad, key_id)?;
    aad.extend_from_slice(&key_generation.to_be_bytes());
    push_length_prefixed(&mut aad, profile_id)?;
    aad.extend_from_slice(&suite.kem().id().to_be_bytes());
    aad.extend_from_slice(&suite.aead().id().to_be_bytes());
    Ok(aad)
}

fn push_length_prefixed(out: &mut Vec<u8>, value: &str) -> SecretsResult<()> {
    let len = u32::try_from(value.len()).map_err(|_| SecretsError::InvalidDekWrapperIdentity {
        reason: "identity string too long".to_owned(),
    })?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(value.as_bytes());
    Ok(())
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
            reason: "only DEK-wrapped V2 records can be rewrapped under a local KEK".to_owned(),
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
pub(crate) fn suite_for(kem: KemAlgo, aead: AeadAlgo) -> SecretsResult<Suite> {
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
pub(crate) fn recipient_private_key(
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

pub(crate) const fn hpke_authentication_failed() -> crypt_guard::pq_hpke::Error {
    crypt_guard::pq_hpke::Error::AuthenticationFailed
}

const fn hpke_internal_failure() -> crypt_guard::pq_hpke::Error {
    crypt_guard::pq_hpke::Error::InternalInvariant
}

/// Seal a legacy V1 (direct-HPKE) record. Test-only: production never writes
/// V1 records any more, but frozen V1 fixtures and the compatibility tests
/// need the exact layout `open_v1` reads.
///
/// # Errors
/// - [`SecretsError::UnsupportedLegacyKem`]: `policy` names a retired pure
///   ML-KEM level.
/// - [`SecretsError::Seal`]: `kek_public` is not a valid key for the policy's
///   hybrid KEM, or HPKE sealing failed.
#[cfg(test)]
pub(crate) fn seal_legacy_v1_for_tests(
    policy: &CryptoPolicy,
    id: SecretId,
    key_version: KeyVersion,
    kek_public: &[u8],
    plaintext: &[u8],
) -> SecretsResult<SecretRecord> {
    let suite = suite_for(policy.kem, policy.aead)?;
    let recipient =
        RecipientPublicKey::from_bytes(suite.kem(), kek_public).map_err(SecretsError::Seal)?;
    let envelope = HpkeEnvelope::seal(
        suite,
        &recipient,
        HPKE_INFO_V1,
        &v1_record_aad(id, key_version, suite),
        plaintext,
    )
    .map_err(SecretsError::Seal)?;
    Ok(SecretRecord {
        id,
        envelope_format: SecretEnvelopeFormat::LegacyDirectHpke,
        ciphertext: envelope.to_bytes(),
        nonce: Vec::new(),
        wrapped_dek: envelope.encapsulation().to_vec(),
        kem_algo: policy.kem,
        aead_algo: policy.aead,
        key_version,
        key_id: None,
        key_generation: None,
        crypto_profile_id: None,
    })
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

    /// Known-Answer-Vektoren der lokalen Payload-AEADs, erzeugt mit
    /// chacha20poly1305 0.10.1 / aes-gcm-siv 0.11.1 **vor** dem Update auf
    /// die aead-0.6-Generation (Runde 7, Dependabot #10/#12). Sie belegen,
    /// dass bestehende Datensätze nach dem Crate-Update bitgleich
    /// verschlüsselt und weiterhin entschlüsselt werden.
    const KAT_DEK: [u8; DEK_LEN] = [0x42; DEK_LEN];
    const KAT_AAD: &[u8] = b"harw-kat-aad";
    const KAT_PLAINTEXT: &[u8] = b"harw aead known answer";
    const KAT_XCHACHA_CT: &str =
        "cd3fa5124e4529c7c60f545461ff63ea539ffdcd196d2fb45b04f59b649094625e26cf803015";
    const KAT_AES_GCM_SIV_CT: &str =
        "ebd31bb174392830519bb2848c754d5d80adbc418e3e84326944a869c4e28218d22cd62bb95c";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn unhex(text: &str) -> TestResult<Vec<u8>> {
        (0..text.len())
            .step_by(2)
            .map(|i| {
                text.get(i..i + 2)
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                    .ok_or(crate::test_support::TestError::Missing("gültiges Hex"))
            })
            .collect()
    }

    #[test]
    fn test_payload_aeads_match_the_pre_update_known_answers() -> TestResult {
        for (aead, nonce, expected) in [
            (
                AeadAlgo::XChaCha20Poly1305,
                vec![0x24_u8; 24],
                KAT_XCHACHA_CT,
            ),
            (AeadAlgo::AesGcmSiv, vec![0x12_u8; 12], KAT_AES_GCM_SIV_CT),
        ] {
            let ct = encrypt_payload(aead, &KAT_DEK, &nonce, KAT_AAD, KAT_PLAINTEXT)
                .map_err(ctx("encrypt"))?;
            assert_eq!(hex(&ct), expected, "{aead:?}: Chiffrat weicht ab");
            let stored = unhex(expected)?;
            let pt = decrypt_payload(aead, &KAT_DEK, &nonce, KAT_AAD, &stored)
                .map_err(ctx("decrypt"))?;
            assert_eq!(pt, KAT_PLAINTEXT, "{aead:?}: Klartext weicht ab");
        }
        Ok(())
    }

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
            key_id: None,
            key_generation: None,
            crypto_profile_id: None,
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
                key_id: None,
                key_generation: None,
                crypto_profile_id: None,
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
        let kek_public = hybrid_public_key(policy.kem, &PROVENANCE_SEED)?;
        let key_version = KeyVersion::initial();
        let record = seal_legacy_v1_for_tests(
            &policy,
            SecretId::new(),
            key_version,
            kek_public.as_bytes(),
            b"legacy-api-token",
        )?;
        assert_eq!(
            record.envelope_format,
            SecretEnvelopeFormat::LegacyDirectHpke
        );
        assert!(record.nonce.is_empty());

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
            // Ein echter reiner ML-KEM-Umschlag, zur Testlaufzeit mit dem
            // gepinnten crypt_guard erzeugt (ursprünglich unter 3.0.1
            // eingeführt): Das Format wäre dekodierbar, der KEM ist aber nach
            // harw-secrets-Richtlinie nicht mehr zulässig.
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
                key_id: None,
                key_generation: None,
                crypto_profile_id: None,
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

    // ---- V3 (KMS-wrapped) -------------------------------------------------

    use crate::dek_wrapper::{DekWrapper, LOCAL_HPKE_PROFILE_ID, LocalHpkeDekWrapper};
    use crate::test_support::TestError;

    const V3_KEY_ID: &str = "secrets/dek-wrap";

    fn local_wrapper(key_id: &str, generation: u32) -> TestResult<LocalHpkeDekWrapper> {
        Ok(LocalHpkeDekWrapper::new(
            key_id,
            generation,
            CryptoPolicy::strongest(),
            SecretBox::new(PROVENANCE_SEED.to_vec().into_boxed_slice()),
        )?)
    }

    /// Delegates to a local wrapper but reports another crypto profile, to
    /// prove the profile id is bound into the wrap AAD.
    struct RelabeledProfile {
        inner: LocalHpkeDekWrapper,
        profile: &'static str,
    }

    impl DekWrapper for RelabeledProfile {
        fn profile_id(&self) -> &str {
            self.profile
        }
        fn key_id(&self) -> &str {
            self.inner.key_id()
        }
        fn key_generation(&self) -> u32 {
            self.inner.key_generation()
        }
        fn wrap_dek(&self, dek: &[u8], aad: &[u8]) -> SecretsResult<Vec<u8>> {
            self.inner.wrap_dek(dek, aad)
        }
        fn unwrap_dek(
            &self,
            wrapped: &[u8],
            key_generation: u32,
            aad: &[u8],
        ) -> SecretsResult<Zeroizing<Vec<u8>>> {
            self.inner.unwrap_dek(wrapped, key_generation, aad)
        }
    }

    fn v3_record_with(policy: CryptoPolicy, generation: u32) -> TestResult<SecretRecord> {
        let wrapper = local_wrapper(V3_KEY_ID, generation)?;
        Ok(seal_v3(&wrapper, &policy, SecretId::new(), b"api-token")?)
    }

    #[test]
    fn v3_round_trips_with_the_local_hpke_wrapper() -> TestResult {
        let wrapper = local_wrapper(V3_KEY_ID, 4)?;
        for kem in HYBRID_KEMS {
            for aead in AEADS {
                let policy = CryptoPolicy { kem, aead };
                let record = seal_v3(&wrapper, &policy, SecretId::new(), b"api-token")?;

                assert_eq!(record.envelope_format, SecretEnvelopeFormat::KmsWrappedV3);
                assert_eq!(record.key_id.as_deref(), Some(V3_KEY_ID));
                assert_eq!(record.key_generation, Some(4));
                assert_eq!(record.key_version, KeyVersion(4));
                assert_eq!(
                    record.crypto_profile_id.as_deref(),
                    Some(LOCAL_HPKE_PROFILE_ID)
                );
                assert_eq!(record.kem_algo, kem);
                assert_eq!(record.aead_algo, aead);
                assert_eq!(record.nonce.len(), nonce_len(aead));
                assert_eq!(
                    open_v3(&wrapper, &record)?.expose_secret().as_ref(),
                    b"api-token"
                );
                assert_eq!(
                    open_with_wrapper(None, Some(&wrapper as &dyn DekWrapper), &record)?
                        .expose_secret()
                        .as_ref(),
                    b"api-token"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn v3_without_a_wrapper_fails_closed() -> TestResult {
        let record = v3_record_with(CryptoPolicy::strongest(), 0)?;

        assert!(matches!(
            open(&PROVENANCE_SEED, &record),
            Err(SecretsError::DekWrapperUnavailable { profile, .. })
                if profile == LOCAL_HPKE_PROFILE_ID
        ));
        // And a V2 record without a seed fails closed as well.
        let (_, v2) = sealed_record(AeadAlgo::XChaCha20Poly1305)?;
        let wrapper = local_wrapper(V3_KEY_ID, 0)?;
        assert!(matches!(
            open_with_wrapper(None, Some(&wrapper as &dyn DekWrapper), &v2),
            Err(SecretsError::KekUnavailable { .. })
        ));
        Ok(())
    }

    #[test]
    fn v2_to_v3_rewrap_keeps_the_payload_and_the_plaintext() -> TestResult {
        let wrapper = local_wrapper(V3_KEY_ID, 2)?;
        for kem in HYBRID_KEMS {
            for aead in AEADS {
                let v2 = sealed_record_with(kem, aead)?;
                let v3 = rewrap_to_v3(&PROVENANCE_SEED, &v2, &wrapper)?;

                assert_eq!(v3.envelope_format, SecretEnvelopeFormat::KmsWrappedV3);
                assert_eq!(v3.id, v2.id);
                assert_eq!(v3.ciphertext, v2.ciphertext);
                assert_eq!(v3.nonce, v2.nonce);
                assert_eq!(v3.kem_algo, v2.kem_algo);
                assert_eq!(v3.aead_algo, v2.aead_algo);
                assert_ne!(v3.wrapped_dek, v2.wrapped_dek);
                assert_eq!(v3.key_generation, Some(2));
                assert_eq!(
                    open_v3(&wrapper, &v3)?.expose_secret().as_ref(),
                    b"api-token"
                );
                // The source record is untouched and still opens under V2.
                assert_eq!(
                    open(&PROVENANCE_SEED, &v2)?.expose_secret().as_ref(),
                    b"api-token"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn v2_to_v3_rewrap_requires_the_right_kek_seed() -> TestResult {
        let wrapper = local_wrapper(V3_KEY_ID, 0)?;
        let (_, v2) = sealed_record(AeadAlgo::XChaCha20Poly1305)?;

        assert!(matches!(
            rewrap_to_v3(&ROTATED_SEED, &v2, &wrapper),
            Err(SecretsError::Open(_))
        ));
        Ok(())
    }

    #[test]
    fn v1_to_v3_rewrap_reencrypts_and_opens() -> TestResult {
        let wrapper = local_wrapper(V3_KEY_ID, 1)?;
        for kem in HYBRID_KEMS {
            let policy = CryptoPolicy {
                kem,
                aead: AeadAlgo::XChaCha20Poly1305,
            };
            let kek_public = hybrid_public_key(kem, &PROVENANCE_SEED)?;
            let v1 = seal_legacy_v1_for_tests(
                &policy,
                SecretId::new(),
                KeyVersion::initial(),
                kek_public.as_bytes(),
                b"legacy-api-token",
            )?;
            let v3 = rewrap_to_v3(&PROVENANCE_SEED, &v1, &wrapper)?;

            assert_eq!(v3.envelope_format, SecretEnvelopeFormat::KmsWrappedV3);
            assert_eq!(v3.id, v1.id);
            assert_eq!(v3.kem_algo, kem);
            assert_eq!(v3.nonce.len(), nonce_len(AeadAlgo::XChaCha20Poly1305));
            assert_eq!(
                open_v3(&wrapper, &v3)?.expose_secret().as_ref(),
                b"legacy-api-token"
            );
        }
        Ok(())
    }

    #[test]
    fn rewrap_to_v3_refuses_a_record_that_already_is_v3() -> TestResult {
        let wrapper = local_wrapper(V3_KEY_ID, 0)?;
        let v3 = v3_record_with(CryptoPolicy::strongest(), 0)?;

        assert!(matches!(
            rewrap_to_v3(&PROVENANCE_SEED, &v3, &wrapper),
            Err(SecretsError::PersistenceFormat { .. })
        ));
        // Local KEK rotation does not apply to V3 either.
        let next_public = hybrid_public_key(v3.kem_algo, &ROTATED_SEED)?;
        assert!(matches!(
            rewrap_v2(&PROVENANCE_SEED, &v3, next_public.as_bytes(), KeyVersion(1)),
            Err(SecretsError::PersistenceFormat { .. })
        ));
        Ok(())
    }

    #[test]
    fn v3_wrong_key_generation_is_refused_with_a_typed_error() -> TestResult {
        let record = v3_record_with(CryptoPolicy::strongest(), 1)?;

        // A wrapper that only serves generation 2 (e.g. generation 1 revoked).
        let newer = local_wrapper(V3_KEY_ID, 2)?;
        match open_v3(&newer, &record) {
            Err(SecretsError::KeyGenerationMismatch {
                expected: 2,
                found: 1,
                ..
            }) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "KeyGenerationMismatch erwartet: {:?}",
                    other.map(|_| "Ok")
                )));
            }
        }

        // A record relabelled to another generation is refused as well,
        // whether or not key_version still mirrors it.
        let current = local_wrapper(V3_KEY_ID, 1)?;
        let mut relabelled = record.clone();
        relabelled.key_generation = Some(0);
        relabelled.key_version = KeyVersion(0);
        assert!(matches!(
            open_v3(&current, &relabelled),
            Err(SecretsError::KeyGenerationMismatch {
                expected: 1,
                found: 0,
                ..
            })
        ));
        let mut unmirrored = record.clone();
        unmirrored.key_version = KeyVersion(7);
        assert!(matches!(
            open_v3(&current, &unmirrored),
            Err(SecretsError::PersistenceFormat { .. })
        ));
        Ok(())
    }

    #[test]
    fn v3_tampered_aad_fields_are_refused() -> TestResult {
        let wrapper = local_wrapper(V3_KEY_ID, 0)?;
        let record = v3_record_with(CryptoPolicy::strongest(), 0)?;

        // Secret id: bound only via the AAD.
        let mut other_id = record.clone();
        other_id.id = SecretId::new();
        assert!(matches!(
            open_v3(&wrapper, &other_id),
            Err(SecretsError::Open(_))
        ));

        // KEM label: bound via the AAD.
        let mut other_kem = record.clone();
        other_kem.kem_algo = KemAlgo::MlKem768P256;
        assert!(matches!(
            open_v3(&wrapper, &other_kem),
            Err(SecretsError::Open(_))
        ));

        // key_id: refused against the configured wrapper ...
        let mut other_key = record.clone();
        other_key.key_id = Some("secrets/other".to_owned());
        assert!(matches!(
            open_v3(&wrapper, &other_key),
            Err(SecretsError::DekWrapperMismatch { field }) if field == "key_id"
        ));
        // ... and cryptographically: a wrapper over the same KEK that serves
        // the relabelled key id still cannot authenticate the wrap.
        let relabelled_wrapper = local_wrapper("secrets/other", 0)?;
        assert!(matches!(
            open_v3(&relabelled_wrapper, &other_key),
            Err(SecretsError::Open(_))
        ));

        // crypto_profile_id: refused against the configured wrapper ...
        let mut other_profile = record.clone();
        other_profile.crypto_profile_id = Some("kms-profile-x".to_owned());
        assert!(matches!(
            open_v3(&wrapper, &other_profile),
            Err(SecretsError::DekWrapperMismatch { field }) if field == "crypto_profile_id"
        ));
        // ... and cryptographically bound into the AAD.
        let relabelled_profile = RelabeledProfile {
            inner: local_wrapper(V3_KEY_ID, 0)?,
            profile: "kms-profile-x",
        };
        assert!(matches!(
            open_v3(&relabelled_profile, &other_profile),
            Err(SecretsError::Open(_))
        ));

        // Wrapped bytes and payload.
        let mut wrapped = record.clone();
        let last = wrapped.wrapped_dek.len() - 1;
        wrapped.wrapped_dek[last] ^= 1;
        assert!(open_v3(&wrapper, &wrapped).is_err());
        let mut payload = record.clone();
        payload.ciphertext[0] ^= 1;
        assert!(matches!(
            open_v3(&wrapper, &payload),
            Err(SecretsError::PayloadAeadOpen(_))
        ));

        // The untampered record still opens.
        assert_eq!(
            open_v3(&wrapper, &record)?.expose_secret().as_ref(),
            b"api-token"
        );
        Ok(())
    }

    #[test]
    fn v3_record_missing_a_kms_field_is_refused() -> TestResult {
        let wrapper = local_wrapper(V3_KEY_ID, 0)?;
        let record = v3_record_with(CryptoPolicy::strongest(), 0)?;
        let strips: [fn(&mut SecretRecord); 3] = [
            |r| r.key_id = None,
            |r| r.key_generation = None,
            |r| r.crypto_profile_id = None,
        ];
        for strip in strips {
            let mut stripped = record.clone();
            strip(&mut stripped);
            assert!(matches!(
                open_v3(&wrapper, &stripped),
                Err(SecretsError::PersistenceFormat { .. })
            ));
        }
        Ok(())
    }

    #[test]
    fn seal_v3_refuses_a_malformed_wrapper_identity_and_a_legacy_kem() -> TestResult {
        let bad_profile = RelabeledProfile {
            inner: local_wrapper(V3_KEY_ID, 0)?,
            profile: "",
        };
        assert!(matches!(
            seal_v3(
                &bad_profile,
                &CryptoPolicy::strongest(),
                SecretId::new(),
                b"api-token"
            ),
            Err(SecretsError::InvalidDekWrapperIdentity { .. })
        ));
        let wrapper = local_wrapper(V3_KEY_ID, 0)?;
        let legacy = CryptoPolicy {
            kem: KemAlgo::LegacyMlKem768,
            aead: AeadAlgo::XChaCha20Poly1305,
        };
        assert!(matches!(
            seal_v3(&wrapper, &legacy, SecretId::new(), b"api-token"),
            Err(SecretsError::UnsupportedLegacyKem { .. })
        ));
        Ok(())
    }

    #[test]
    fn rewrap_v3_moves_to_a_new_generation_without_touching_the_payload() -> TestResult {
        let old = local_wrapper(V3_KEY_ID, 0)?;
        let new = local_wrapper(V3_KEY_ID, 1)?;
        let record = v3_record_with(CryptoPolicy::strongest(), 0)?;

        let rotated = rewrap_v3(&old, &record, &new)?;

        assert_eq!(rotated.ciphertext, record.ciphertext);
        assert_eq!(rotated.nonce, record.nonce);
        assert_eq!(rotated.key_generation, Some(1));
        assert_eq!(rotated.key_version, KeyVersion(1));
        assert_eq!(
            open_v3(&new, &rotated)?.expose_secret().as_ref(),
            b"api-token"
        );
        assert!(matches!(
            open_v3(&old, &rotated),
            Err(SecretsError::KeyGenerationMismatch { .. })
        ));
        Ok(())
    }

    #[test]
    fn v3_json_carries_the_kms_fields_while_fresh_v2_json_does_not() -> TestResult {
        let v3 = v3_record_with(CryptoPolicy::strongest(), 3)?;
        let v3_json: serde_json::Value = serde_json::to_value(&v3)?;
        let v3_object = v3_json
            .as_object()
            .ok_or(TestError::Missing("V3 JSON object"))?;
        assert_eq!(
            v3_object.get("envelope_format"),
            Some(&serde_json::json!("kms_wrapped_v3"))
        );
        assert_eq!(v3_object.get("key_id"), Some(&serde_json::json!(V3_KEY_ID)));
        assert_eq!(v3_object.get("key_generation"), Some(&serde_json::json!(3)));
        assert_eq!(
            v3_object.get("crypto_profile_id"),
            Some(&serde_json::json!(LOCAL_HPKE_PROFILE_ID))
        );
        let decoded: SecretRecord = serde_json::from_value(v3_json)?;
        assert_eq!(decoded, v3);

        let (_, v2) = sealed_record(AeadAlgo::XChaCha20Poly1305)?;
        let v2_json: serde_json::Value = serde_json::to_value(&v2)?;
        let v2_object = v2_json
            .as_object()
            .ok_or(TestError::Missing("V2 JSON object"))?;
        for absent in ["key_id", "key_generation", "crypto_profile_id"] {
            assert!(
                !v2_object.contains_key(absent),
                "fresh V2 JSON must not contain {absent}"
            );
        }
        let mut v2_keys = v2_object.keys().map(String::as_str).collect::<Vec<_>>();
        v2_keys.sort_unstable();
        assert_eq!(
            v2_keys,
            [
                "aead_algo",
                "ciphertext",
                "envelope_format",
                "id",
                "kem_algo",
                "key_version",
                "nonce",
                "wrapped_dek",
            ]
        );
        Ok(())
    }
}
