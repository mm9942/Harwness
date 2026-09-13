//! Crypto preset selection (spec §2.3). All levels are usable; `strongest()` is
//! the sensible default. `CryptoPolicy` governs only *new* seals — existing
//! envelopes keep decrypting under whatever `kem_algo`/`aead_algo` they
//! recorded at seal time.

use serde::{Deserialize, Serialize};

/// ML-KEM security level for new envelopes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum KemAlgo {
    /// ML-KEM-512 — lowest margin, constrained hardware.
    #[serde(rename = "ml_kem_512")]
    MlKem512,
    /// ML-KEM-768 — middle ground.
    #[serde(rename = "ml_kem_768")]
    MlKem768,
    /// ML-KEM-1024 — highest margin (default).
    #[serde(rename = "ml_kem_1024")]
    MlKem1024,
}

/// AEAD cipher for new envelopes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AeadAlgo {
    /// XChaCha20-Poly1305 (default).
    #[serde(rename = "x_chacha20_poly1305")]
    XChaCha20Poly1305,
    /// AES-GCM-SIV.
    #[serde(rename = "aes_gcm_siv")]
    AesGcmSiv,
}

/// A named preset bundling a KEM level and an AEAD choice for new seals.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CryptoPolicy {
    /// ML-KEM security level applied to new seals.
    pub kem: KemAlgo,
    /// AEAD cipher applied to new seals.
    pub aead: AeadAlgo,
}

impl CryptoPolicy {
    /// Highest supported level: ML-KEM-1024 + XChaCha20-Poly1305 (the default).
    #[must_use]
    pub fn strongest() -> Self {
        Self {
            kem: KemAlgo::MlKem1024,
            aead: AeadAlgo::XChaCha20Poly1305,
        }
    }

    /// ML-KEM-512 + XChaCha20-Poly1305 preset.
    #[must_use]
    pub fn ml_kem_512() -> Self {
        Self {
            kem: KemAlgo::MlKem512,
            aead: AeadAlgo::XChaCha20Poly1305,
        }
    }

    /// ML-KEM-768 + XChaCha20-Poly1305 preset.
    #[must_use]
    pub fn ml_kem_768() -> Self {
        Self {
            kem: KemAlgo::MlKem768,
            aead: AeadAlgo::XChaCha20Poly1305,
        }
    }
}

impl Default for CryptoPolicy {
    fn default() -> Self {
        Self::strongest()
    }
}

#[cfg(test)]
mod tests {
    use super::{AeadAlgo, CryptoPolicy, KemAlgo};

    #[test]
    fn algorithms_use_explicit_snake_case_wire_names() {
        assert_eq!(
            serde_json::to_string(&KemAlgo::MlKem512).expect("serialize KEM"),
            r#""ml_kem_512""#
        );
        assert_eq!(
            serde_json::to_string(&KemAlgo::MlKem768).expect("serialize KEM"),
            r#""ml_kem_768""#
        );
        assert_eq!(
            serde_json::to_string(&KemAlgo::MlKem1024).expect("serialize KEM"),
            r#""ml_kem_1024""#
        );
        assert_eq!(
            serde_json::to_string(&AeadAlgo::XChaCha20Poly1305).expect("serialize AEAD"),
            r#""x_chacha20_poly1305""#
        );
        assert_eq!(
            serde_json::to_string(&AeadAlgo::AesGcmSiv).expect("serialize AEAD"),
            r#""aes_gcm_siv""#
        );
    }

    #[test]
    fn crypto_policy_round_trips_with_stable_field_names() {
        let policy = CryptoPolicy {
            kem: KemAlgo::MlKem768,
            aead: AeadAlgo::AesGcmSiv,
        };

        let encoded = serde_json::to_string(&policy).expect("serialize policy");
        assert_eq!(encoded, r#"{"kem":"ml_kem_768","aead":"aes_gcm_siv"}"#);

        let decoded: CryptoPolicy = serde_json::from_str(&encoded).expect("deserialize policy");
        assert_eq!(decoded, policy);
    }
}
