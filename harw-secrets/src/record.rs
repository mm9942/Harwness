//! On-disk record shape (spec §2.3). `SecretRecord` is what gets serialized to
//! the secrets store; `SecretMetadata` is the safe-to-log, safe-to-display
//! subset. Raw secret bytes and the DEK never appear in `SecretMetadata`, never
//! appear in a `Debug` impl, and never appear in an `AuditEvent`.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::id::{KeyVersion, SecretId};
use crate::policy::{AeadAlgo, KemAlgo};

/// Durable envelope layout used to seal a [`SecretRecord`].
///
/// The missing-field default is deliberately the legacy direct-HPKE layout:
/// records written before this discriminator existed were all direct HPKE
/// records. New records must explicitly use [`Self::DekWrappedV2`] or, when a
/// [`crate::dek_wrapper::DekWrapper`] (KMS) is configured,
/// [`Self::KmsWrappedV3`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecretEnvelopeFormat {
    /// Legacy record whose payload is sealed directly to the KEK with HPKE.
    #[default]
    #[serde(rename = "direct_hpke_v1")]
    LegacyDirectHpke,
    /// Two-layer record: payload under a per-secret DEK, then DEK under the KEK.
    #[serde(rename = "dek_wrapped_v2")]
    DekWrappedV2,
    /// KMS record (Crypto-Masterplan v2 §16.4): payload under a per-secret DEK
    /// exactly as in V2 (same payload AAD, so a V2 record migrates without
    /// re-encrypting its payload), DEK wrapped by a
    /// [`crate::dek_wrapper::DekWrapper`] (AuthHub KMS or the local HPKE
    /// wrapper). The record additionally carries [`SecretRecord::key_id`],
    /// [`SecretRecord::key_generation`] and [`SecretRecord::crypto_profile_id`].
    #[serde(rename = "kms_wrapped_v3")]
    KmsWrappedV3,
}

/// Full on-disk record for one sealed secret. Every byte field here is
/// ciphertext, a nonce, or a KEM-wrapped DEK — none of it is meaningful without
/// the KEK's secret half.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRecord {
    /// Stable id across rotations.
    pub id: SecretId,
    /// Durable envelope-layout discriminator. Missing values from pre-v2
    /// records safely decode as [`SecretEnvelopeFormat::LegacyDirectHpke`].
    #[serde(default)]
    pub envelope_format: SecretEnvelopeFormat,
    /// Payload ciphertext. [`SecretEnvelopeFormat::DekWrappedV2`]: the secret
    /// bytes encrypted locally under a fresh random 32-byte per-secret DEK with
    /// `aead_algo` (AAD binds the secret id and AEAD). Legacy
    /// [`SecretEnvelopeFormat::LegacyDirectHpke`]: the complete serialized
    /// `crypt_guard` v3 PQ HPKE envelope sealing the secret directly to the KEK.
    pub ciphertext: Vec<u8>,
    /// V2: the local payload AEAD nonce (24 bytes XChaCha20-Poly1305, 12 bytes
    /// AES-GCM-SIV). V1: empty — the HPKE envelope derives its own nonce.
    pub nonce: Vec<u8>,
    /// V2: the serialized `crypt_guard` v3 PQ HPKE envelope (`CGH3`) that wraps
    /// only the DEK for the KEK; its AAD binds id, `key_version`, KEM and AEAD,
    /// and rotation rewraps only this field. V1: the HPKE encapsulation
    /// duplicated from `ciphertext`, cross-checked on open. V3: the opaque
    /// bytes returned by [`crate::dek_wrapper::DekWrapper::wrap_dek`]; their
    /// layout belongs to the wrapper's crypto profile.
    pub wrapped_dek: Vec<u8>,
    /// Hybrid KEM that produced `wrapped_dek`. Retired pure ML-KEM levels
    /// (`KemAlgo::Legacy*`) still deserialize, but such records can no longer
    /// be opened or re-serialized.
    pub kem_algo: KemAlgo,
    /// AEAD cipher that produced `ciphertext`.
    pub aead_algo: AeadAlgo,
    /// Which KEK generation wrapped `wrapped_dek`; drives rotation detection.
    /// V3: mirrors [`Self::key_generation`] (checked on open) so metadata and
    /// listings show the wrapping generation; the local-KEK rotation and
    /// generation checks of the store do not apply to V3 records.
    pub key_version: KeyVersion,
    /// V3 only: KMS key reference `"namespace/id"` that wrapped the DEK.
    /// Absent (and not serialized) for V1/V2, so their JSON stays
    /// byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    /// V3 only: generation of [`Self::key_id`] that wrapped the DEK, with
    /// Harwness [`KeyVersion`] semantics (0-based, `0` = genesis). The
    /// CryptGuard service `KeyVersion` is 1-based (`NonZeroU32`): CryptGuard
    /// version = `key_generation + 1`, see
    /// [`crate::dek_wrapper::crypt_guard_key_version`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_generation: Option<u32>,
    /// V3 only: crypto profile of the wrapper that produced `wrapped_dek`
    /// (e.g. [`crate::dek_wrapper::LOCAL_HPKE_PROFILE_ID`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crypto_profile_id: Option<String>,
}

/// Admin-visible metadata for a stored secret. Never contains raw secret
/// material, the DEK, or any derived key — only fields safe to render in a
/// `harw secrets list` table or to attach to an `AuditEvent`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretMetadata {
    /// Stable id, shared with the corresponding [`SecretRecord`].
    pub id: SecretId,
    /// Human label, e.g. `"openai-api-key"`, `"telegram-bot-token"`.
    pub name: String,
    /// Free-text purpose, e.g. `"provider-auth"`, `"channel-auth"`.
    pub purpose: String,
    /// KEK generation currently wrapping this secret's DEK.
    pub key_version: KeyVersion,
    /// When the secret was first created.
    pub created_at: Timestamp,
    /// When the secret was last updated or rewrapped.
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn sealed_record_round_trips_without_plaintext_fields() -> TestResult {
        let record = SecretRecord {
            id: SecretId::new(),
            envelope_format: SecretEnvelopeFormat::DekWrappedV2,
            ciphertext: vec![0xde, 0xad, 0xbe, 0xef],
            nonce: vec![0x01, 0x02, 0x03],
            wrapped_dek: vec![0xca, 0xfe, 0xba, 0xbe],
            kem_algo: KemAlgo::MlKem768X25519,
            aead_algo: AeadAlgo::XChaCha20Poly1305,
            key_version: KeyVersion(7),
            key_id: None,
            key_generation: None,
            crypto_profile_id: None,
        };

        let encoded = serde_json::to_string(&record)?;
        assert!(!encoded.contains("raw-secret-plaintext"));
        assert!(!encoded.contains("dek-plaintext"));

        let decoded: SecretRecord = serde_json::from_str(&encoded)?;
        assert_eq!(decoded, record);
        Ok(())
    }

    #[test]
    fn envelope_format_uses_stable_v2_wire_name() -> TestResult {
        assert_eq!(
            serde_json::to_string(&SecretEnvelopeFormat::DekWrappedV2)?,
            r#""dek_wrapped_v2""#
        );
        assert_eq!(
            serde_json::to_string(&SecretEnvelopeFormat::LegacyDirectHpke)?,
            r#""direct_hpke_v1""#
        );
        assert_eq!(
            serde_json::to_string(&SecretEnvelopeFormat::KmsWrappedV3)?,
            r#""kms_wrapped_v3""#
        );
        Ok(())
    }

    #[test]
    fn v3_record_serializes_its_kms_fields_and_round_trips() -> TestResult {
        let record = SecretRecord {
            id: SecretId::parse("018f1c2e-4d5a-7b80-9123-456789abcdef")
                .map_err(crate::test_support::ctx("parse id"))?,
            envelope_format: SecretEnvelopeFormat::KmsWrappedV3,
            ciphertext: vec![1],
            nonce: vec![2],
            wrapped_dek: vec![3],
            kem_algo: KemAlgo::MlKem768X25519,
            aead_algo: AeadAlgo::XChaCha20Poly1305,
            key_version: KeyVersion(4),
            key_id: Some("secrets/dek-wrap".to_owned()),
            key_generation: Some(4),
            crypto_profile_id: Some("local-hpke-v1".to_owned()),
        };

        let encoded = serde_json::to_string(&record)?;
        assert_eq!(
            encoded,
            r#"{"id":"018f1c2e-4d5a-7b80-9123-456789abcdef","envelope_format":"kms_wrapped_v3","ciphertext":[1],"nonce":[2],"wrapped_dek":[3],"kem_algo":"ml_kem_768_x25519","aead_algo":"x_chacha20_poly1305","key_version":4,"key_id":"secrets/dek-wrap","key_generation":4,"crypto_profile_id":"local-hpke-v1"}"#
        );
        let decoded: SecretRecord = serde_json::from_str(&encoded)?;
        assert_eq!(decoded, record);
        Ok(())
    }

    #[test]
    fn legacy_record_without_envelope_format_defaults_to_direct_hpke() -> TestResult {
        let legacy_json = r#"{
            "id":"018f1c2e-4d5a-7b80-9123-456789abcdef",
            "ciphertext":[222,173,190,239],
            "nonce":[1,2,3],
            "wrapped_dek":[202,254,186,190],
            "kem_algo":"ml_kem_768_x25519",
            "aead_algo":"x_chacha20_poly1305",
            "key_version":7
        }"#;

        let decoded: SecretRecord = serde_json::from_str(legacy_json)?;

        assert_eq!(
            decoded.envelope_format,
            SecretEnvelopeFormat::LegacyDirectHpke
        );
        assert_eq!(
            serde_json::to_string(&decoded)?,
            r#"{"id":"018f1c2e-4d5a-7b80-9123-456789abcdef","envelope_format":"direct_hpke_v1","ciphertext":[222,173,190,239],"nonce":[1,2,3],"wrapped_dek":[202,254,186,190],"kem_algo":"ml_kem_768_x25519","aead_algo":"x_chacha20_poly1305","key_version":7}"#
        );
        Ok(())
    }

    #[test]
    fn record_with_legacy_pure_ml_kem_deserializes_but_does_not_reserialize() -> TestResult {
        let legacy_kem_json = r#"{
            "id":"018f1c2e-4d5a-7b80-9123-456789abcdef",
            "envelope_format":"dek_wrapped_v2",
            "ciphertext":[222,173,190,239],
            "nonce":[1,2,3],
            "wrapped_dek":[202,254,186,190],
            "kem_algo":"ml_kem_1024",
            "aead_algo":"aes_gcm_siv",
            "key_version":3
        }"#;

        let decoded: SecretRecord = serde_json::from_str(legacy_kem_json)?;

        assert_eq!(decoded.kem_algo, KemAlgo::LegacyMlKem1024);
        assert!(decoded.kem_algo.is_legacy());
        assert!(serde_json::to_string(&decoded).is_err());
        Ok(())
    }

    #[test]
    fn metadata_round_trips_unchanged() -> TestResult {
        let now = Timestamp::now();
        let metadata = SecretMetadata {
            id: SecretId::new(),
            name: "openai-api-key".to_owned(),
            purpose: "provider-auth".to_owned(),
            key_version: KeyVersion(7),
            created_at: now,
            updated_at: now,
        };

        let encoded = serde_json::to_string(&metadata)?;
        let decoded: SecretMetadata = serde_json::from_str(&encoded)?;
        assert_eq!(decoded, metadata);
        Ok(())
    }
}
