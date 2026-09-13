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
/// records. New records must explicitly use [`Self::DekWrappedV2`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecretEnvelopeFormat {
    /// Legacy record whose payload is sealed directly to the KEK with HPKE.
    #[default]
    #[serde(rename = "direct_hpke_v1")]
    LegacyDirectHpke,
    /// Two-layer record: payload under a per-secret DEK, then DEK under the KEK.
    #[serde(rename = "dek_wrapped_v2")]
    DekWrappedV2,
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
    /// Complete `crypt_guard` CGv2 envelope. It contains the ML-KEM
    /// encapsulation ciphertext, nonce, and authenticated payload; its session
    /// key is a fresh per-secret derived DEK and is never exported.
    pub ciphertext: Vec<u8>,
    /// Bare AEAD nonce, populated only on a future non-`crypt_guard` path
    /// (CGv2 embeds its own nonce inside `ciphertext`).
    pub nonce: Vec<u8>,
    /// ML-KEM encapsulation ciphertext duplicated from the CGv2 envelope. It
    /// binds the record-level metadata to the envelope's derived DEK reference.
    pub wrapped_dek: Vec<u8>,
    /// KEM level that produced `wrapped_dek`.
    pub kem_algo: KemAlgo,
    /// AEAD cipher that produced `ciphertext`.
    pub aead_algo: AeadAlgo,
    /// Which KEK generation wrapped `wrapped_dek`; drives rotation detection.
    pub key_version: KeyVersion,
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

    #[test]
    fn sealed_record_round_trips_without_plaintext_fields() {
        let record = SecretRecord {
            id: SecretId::new(),
            envelope_format: SecretEnvelopeFormat::DekWrappedV2,
            ciphertext: vec![0xde, 0xad, 0xbe, 0xef],
            nonce: vec![0x01, 0x02, 0x03],
            wrapped_dek: vec![0xca, 0xfe, 0xba, 0xbe],
            kem_algo: KemAlgo::MlKem768,
            aead_algo: AeadAlgo::XChaCha20Poly1305,
            key_version: KeyVersion(7),
        };

        let encoded = serde_json::to_string(&record).expect("sealed record serializes");
        assert!(!encoded.contains("raw-secret-plaintext"));
        assert!(!encoded.contains("dek-plaintext"));

        let decoded: SecretRecord =
            serde_json::from_str(&encoded).expect("sealed record deserializes");
        assert_eq!(decoded, record);
    }

    #[test]
    fn envelope_format_uses_stable_v2_wire_name() {
        assert_eq!(
            serde_json::to_string(&SecretEnvelopeFormat::DekWrappedV2)
                .expect("serialize two-layer envelope format"),
            r#""dek_wrapped_v2""#
        );
        assert_eq!(
            serde_json::to_string(&SecretEnvelopeFormat::LegacyDirectHpke)
                .expect("serialize legacy envelope format"),
            r#""direct_hpke_v1""#
        );
    }

    #[test]
    fn legacy_record_without_envelope_format_defaults_to_direct_hpke() {
        let legacy_json = r#"{
            "id":"018f1c2e-4d5a-7b80-9123-456789abcdef",
            "ciphertext":[222,173,190,239],
            "nonce":[1,2,3],
            "wrapped_dek":[202,254,186,190],
            "kem_algo":"ml_kem_768",
            "aead_algo":"x_chacha20_poly1305",
            "key_version":7
        }"#;

        let decoded: SecretRecord =
            serde_json::from_str(legacy_json).expect("legacy record deserializes");

        assert_eq!(
            decoded.envelope_format,
            SecretEnvelopeFormat::LegacyDirectHpke
        );
        assert_eq!(
            serde_json::to_string(&decoded).expect("legacy record reserializes"),
            r#"{"id":"018f1c2e-4d5a-7b80-9123-456789abcdef","envelope_format":"direct_hpke_v1","ciphertext":[222,173,190,239],"nonce":[1,2,3],"wrapped_dek":[202,254,186,190],"kem_algo":"ml_kem_768","aead_algo":"x_chacha20_poly1305","key_version":7}"#
        );
    }

    #[test]
    fn metadata_round_trips_unchanged() {
        let now = Timestamp::now();
        let metadata = SecretMetadata {
            id: SecretId::new(),
            name: "openai-api-key".to_owned(),
            purpose: "provider-auth".to_owned(),
            key_version: KeyVersion(7),
            created_at: now,
            updated_at: now,
        };

        let encoded = serde_json::to_string(&metadata).expect("metadata serializes");
        let decoded: SecretMetadata =
            serde_json::from_str(&encoded).expect("metadata deserializes");
        assert_eq!(decoded, metadata);
    }
}
