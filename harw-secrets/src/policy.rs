//! Crypto preset selection (spec §2.3). All non-legacy levels are usable;
//! `strongest()` is the sensible default. `CryptoPolicy` governs only *new*
//! seals — existing envelopes keep decrypting under whatever
//! `kem_algo`/`aead_algo` they recorded at seal time, **sofern** dieser KEM
//! ein hybrider ist.
//!
//! Hybrid-KEM-Umstellung (W0b): `harw-secrets` leitet deterministische
//! Empfängerschlüssel aus dem 32-Byte-Seed ausschließlich für die hybriden
//! draft-ietf-hpke-pq-05-KEMs ab. Das ist eine Harwness-Richtlinie, keine
//! Grenze von `crypt_guard` (3.0.2 könnte über `derive_recipient_key_pair`
//! auch reine ML-KEM-Schlüssel ableiten). Die reinen ML-KEM-Stufen bleiben als
//! `Legacy*`-Varianten ausschließlich lesbar (Deserialisierung alter Datensätze),
//! können aber weder serialisiert noch zum Siegeln oder Öffnen verwendet werden —
//! jede solche Verwendung endet in [`SecretsError::UnsupportedLegacyKem`].

use crypt_guard::pq_hpke::Kem;
use serde::{Deserialize, Serialize};

use crate::error::{SecretsError, SecretsResult};

/// Hybrid ML-KEM KEM for new envelopes, plus read-only legacy levels.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum KemAlgo {
    /// Hybrid ML-KEM-768 + P-256 (draft KEM id `0x0050`).
    #[serde(rename = "ml_kem_768_p256")]
    MlKem768P256,
    /// Hybrid ML-KEM-1024 + P-384 (draft KEM id `0x0051`) — highest margin
    /// (default).
    #[serde(rename = "ml_kem_1024_p384")]
    MlKem1024P384,
    /// Hybrid ML-KEM-768 + X25519 (draft KEM id `0x647a`).
    #[serde(rename = "ml_kem_768_x25519")]
    MlKem768X25519,
    /// Veraltet: reines ML-KEM-512. Nur deserialisierbar; Siegeln/Öffnen
    /// schlägt mit [`SecretsError::UnsupportedLegacyKem`] fehl.
    #[serde(rename = "ml_kem_512", skip_serializing)]
    LegacyMlKem512,
    /// Veraltet: reines ML-KEM-768. Nur deserialisierbar; Siegeln/Öffnen
    /// schlägt mit [`SecretsError::UnsupportedLegacyKem`] fehl.
    #[serde(rename = "ml_kem_768", skip_serializing)]
    LegacyMlKem768,
    /// Veraltet: reines ML-KEM-1024. Nur deserialisierbar; Siegeln/Öffnen
    /// schlägt mit [`SecretsError::UnsupportedLegacyKem`] fehl.
    #[serde(rename = "ml_kem_1024", skip_serializing)]
    LegacyMlKem1024,
}

impl KemAlgo {
    /// Whether this is a retired pure ML-KEM level that can no longer seal or
    /// open envelopes.
    #[must_use]
    pub const fn is_legacy(self) -> bool {
        matches!(
            self,
            Self::LegacyMlKem512 | Self::LegacyMlKem768 | Self::LegacyMlKem1024
        )
    }

    /// Stabiler Wire-Name (identisch zum serde-Namen). Enthält nur den
    /// Algorithmusnamen und ist damit sicher für Fehlermeldungen. Er ist
    /// außerdem Teil des Domänentrennungs-Kontexts der KEM-Seed-Ableitung
    /// (`kek::derive_kem_seed`) und darf deshalb nie geändert werden.
    pub(crate) const fn wire_name(self) -> &'static str {
        match self {
            Self::MlKem768P256 => "ml_kem_768_p256",
            Self::MlKem1024P384 => "ml_kem_1024_p384",
            Self::MlKem768X25519 => "ml_kem_768_x25519",
            Self::LegacyMlKem512 => "ml_kem_512",
            Self::LegacyMlKem768 => "ml_kem_768",
            Self::LegacyMlKem1024 => "ml_kem_1024",
        }
    }

    /// Bildet den Richtlinien-KEM auf den exakten `crypt_guard`-HPKE-KEM ab.
    ///
    /// # Errors
    /// - [`SecretsError::UnsupportedLegacyKem`]: der KEM ist eine veraltete reine
    ///   ML-KEM-Stufe; `harw-secrets` lässt sie per Richtlinie nur noch als
    ///   lesbare Metadaten zu (keine Seed-Ableitung, kein Siegeln/Öffnen).
    pub(crate) fn hpke_kem(self) -> SecretsResult<Kem> {
        match self {
            Self::MlKem768P256 => Ok(Kem::MlKem768P256),
            Self::MlKem1024P384 => Ok(Kem::MlKem1024P384),
            Self::MlKem768X25519 => Ok(Kem::MlKem768X25519),
            Self::LegacyMlKem512 | Self::LegacyMlKem768 | Self::LegacyMlKem1024 => {
                Err(SecretsError::UnsupportedLegacyKem {
                    algo: self.wire_name().to_owned(),
                })
            }
        }
    }
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
    /// Hybrid ML-KEM KEM applied to new seals.
    pub kem: KemAlgo,
    /// AEAD cipher applied to new seals.
    pub aead: AeadAlgo,
}

impl CryptoPolicy {
    /// Highest supported level: hybrid ML-KEM-1024/P-384 + XChaCha20-Poly1305
    /// (the default).
    #[must_use]
    pub fn strongest() -> Self {
        Self {
            kem: KemAlgo::MlKem1024P384,
            aead: AeadAlgo::XChaCha20Poly1305,
        }
    }

    /// Hybrid ML-KEM-768/P-256 + XChaCha20-Poly1305 preset.
    #[must_use]
    pub fn ml_kem_768_p256() -> Self {
        Self {
            kem: KemAlgo::MlKem768P256,
            aead: AeadAlgo::XChaCha20Poly1305,
        }
    }

    /// Hybrid ML-KEM-768/X25519 + XChaCha20-Poly1305 preset.
    #[must_use]
    pub fn ml_kem_768_x25519() -> Self {
        Self {
            kem: KemAlgo::MlKem768X25519,
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
    use crypt_guard::pq_hpke::Kem;

    use crate::error::SecretsError;
    use crate::test_support::TestResult;

    use super::{AeadAlgo, CryptoPolicy, KemAlgo};

    const HYBRID_KEMS: [KemAlgo; 3] = [
        KemAlgo::MlKem768P256,
        KemAlgo::MlKem1024P384,
        KemAlgo::MlKem768X25519,
    ];
    const LEGACY_KEMS: [KemAlgo; 3] = [
        KemAlgo::LegacyMlKem512,
        KemAlgo::LegacyMlKem768,
        KemAlgo::LegacyMlKem1024,
    ];

    #[test]
    fn algorithms_use_explicit_snake_case_wire_names() -> TestResult {
        assert_eq!(
            serde_json::to_string(&KemAlgo::MlKem768P256)?,
            r#""ml_kem_768_p256""#
        );
        assert_eq!(
            serde_json::to_string(&KemAlgo::MlKem1024P384)?,
            r#""ml_kem_1024_p384""#
        );
        assert_eq!(
            serde_json::to_string(&KemAlgo::MlKem768X25519)?,
            r#""ml_kem_768_x25519""#
        );
        assert_eq!(
            serde_json::to_string(&AeadAlgo::XChaCha20Poly1305)?,
            r#""x_chacha20_poly1305""#
        );
        assert_eq!(
            serde_json::to_string(&AeadAlgo::AesGcmSiv)?,
            r#""aes_gcm_siv""#
        );
        Ok(())
    }

    #[test]
    fn hybrid_wire_names_round_trip_and_match_the_internal_name() -> TestResult {
        for kem in HYBRID_KEMS {
            let encoded = serde_json::to_string(&kem)?;
            assert_eq!(encoded, format!("\"{}\"", kem.wire_name()));
            let decoded: KemAlgo = serde_json::from_str(&encoded)?;
            assert_eq!(decoded, kem);
            assert!(!kem.is_legacy());
        }
        Ok(())
    }

    #[test]
    fn legacy_wire_names_deserialize_but_never_serialize() -> TestResult {
        for (wire, expected) in [
            ("ml_kem_512", KemAlgo::LegacyMlKem512),
            ("ml_kem_768", KemAlgo::LegacyMlKem768),
            ("ml_kem_1024", KemAlgo::LegacyMlKem1024),
        ] {
            let decoded: KemAlgo = serde_json::from_str(&format!("\"{wire}\""))?;
            assert_eq!(decoded, expected);
            assert!(decoded.is_legacy());
            assert_eq!(decoded.wire_name(), wire);
            assert!(serde_json::to_string(&decoded).is_err());
        }
        Ok(())
    }

    #[test]
    fn hybrid_kems_map_to_the_exact_crypt_guard_kem() -> TestResult {
        assert_eq!(KemAlgo::MlKem768P256.hpke_kem()?, Kem::MlKem768P256);
        assert_eq!(KemAlgo::MlKem1024P384.hpke_kem()?, Kem::MlKem1024P384);
        assert_eq!(KemAlgo::MlKem768X25519.hpke_kem()?, Kem::MlKem768X25519);
        Ok(())
    }

    #[test]
    fn legacy_kems_are_refused_with_a_typed_error_naming_only_the_algorithm() {
        for kem in LEGACY_KEMS {
            assert!(matches!(
                kem.hpke_kem(),
                Err(SecretsError::UnsupportedLegacyKem { algo }) if algo == kem.wire_name()
            ));
        }
    }

    #[test]
    fn strongest_is_the_default_and_uses_hybrid_ml_kem_1024_p384() {
        assert_eq!(CryptoPolicy::default(), CryptoPolicy::strongest());
        assert_eq!(CryptoPolicy::strongest().kem, KemAlgo::MlKem1024P384);
        assert_eq!(CryptoPolicy::strongest().aead, AeadAlgo::XChaCha20Poly1305);
        assert_eq!(CryptoPolicy::ml_kem_768_p256().kem, KemAlgo::MlKem768P256);
        assert_eq!(
            CryptoPolicy::ml_kem_768_x25519().kem,
            KemAlgo::MlKem768X25519
        );
    }

    #[test]
    fn crypto_policy_round_trips_with_stable_field_names() -> TestResult {
        let policy = CryptoPolicy {
            kem: KemAlgo::MlKem768X25519,
            aead: AeadAlgo::AesGcmSiv,
        };

        let encoded = serde_json::to_string(&policy)?;
        assert_eq!(
            encoded,
            r#"{"kem":"ml_kem_768_x25519","aead":"aes_gcm_siv"}"#
        );

        let decoded: CryptoPolicy = serde_json::from_str(&encoded)?;
        assert_eq!(decoded, policy);
        Ok(())
    }
}
