//! Harw crypto profiles: stable algorithm-suite ids (Masterplan v2 §3.1,
//! §8 `CryptoProfileId`).
//!
//! A profile id is what a [`crate::SecureFrameHeader`] carries on the wire
//! and what a key is generated with. The mapping to CryptGuard's
//! `KeyAlgorithm` lives in [`crate::cg::key_algorithm`]; this module has no
//! CryptGuard types.

use crate::purpose::{HarwKeyPurpose, KeyClass};

/// Which cryptographic family a profile belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProfileKind {
    /// Signature scheme.
    Signature,
    /// PQ HPKE suite (serves both encryption and wrapping keys).
    Hpke,
}

/// A pinned algorithm suite with a stable two-byte id.
///
/// # Description
/// Ids are grouped by family: `0x01xx` signatures, `0x02xx` HPKE suites.
/// `0x0000` is never assigned. Ids never change meaning; a new suite gets a
/// new id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HarwCryptoProfile {
    /// ML-DSA-65 (FIPS 204).
    MlDsa65,
    /// ML-DSA-87 (FIPS 204).
    MlDsa87,
    /// PQ HPKE: ML-KEM-1024/P-384 hybrid, SHAKE256, ChaCha20-Poly1305
    /// (CryptGuard's `pq_hpke::DEFAULT_SUITE`).
    HpkeMlKem1024P384Shake256ChaCha20Poly1305,
    /// PQ HPKE: ML-KEM-768/X25519 hybrid, HKDF-SHA256, ChaCha20-Poly1305.
    HpkeMlKem768X25519HkdfSha256ChaCha20Poly1305,
}

impl HarwCryptoProfile {
    /// Every profile.
    pub const ALL: [Self; 4] = [
        Self::MlDsa65,
        Self::MlDsa87,
        Self::HpkeMlKem1024P384Shake256ChaCha20Poly1305,
        Self::HpkeMlKem768X25519HkdfSha256ChaCha20Poly1305,
    ];

    /// The stable wire id.
    #[must_use]
    pub const fn id(self) -> u16 {
        match self {
            Self::MlDsa65 => 0x0101,
            Self::MlDsa87 => 0x0102,
            Self::HpkeMlKem1024P384Shake256ChaCha20Poly1305 => 0x0201,
            Self::HpkeMlKem768X25519HkdfSha256ChaCha20Poly1305 => 0x0202,
        }
    }

    /// Reverse of [`Self::id`]; `None` for an unknown id.
    #[must_use]
    pub fn from_id(id: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }

    /// The family of this profile.
    #[must_use]
    pub const fn kind(self) -> ProfileKind {
        match self {
            Self::MlDsa65 | Self::MlDsa87 => ProfileKind::Signature,
            Self::HpkeMlKem1024P384Shake256ChaCha20Poly1305
            | Self::HpkeMlKem768X25519HkdfSha256ChaCha20Poly1305 => ProfileKind::Hpke,
        }
    }

    /// Whether a key of `purpose` may use this profile.
    #[must_use]
    pub const fn supports(self, purpose: HarwKeyPurpose) -> bool {
        matches!(
            (self.kind(), purpose.class()),
            (ProfileKind::Signature, KeyClass::Signing)
                | (ProfileKind::Hpke, KeyClass::Encryption | KeyClass::Wrapping)
        )
    }

    /// The default profile for new keys of `purpose`, or `None` when no
    /// CryptGuard algorithm exists for it (pseudonymization).
    ///
    /// Long-lived verifiable signatures (audit checkpoints, artifact
    /// manifests) default to ML-DSA-87, all other signing keys to ML-DSA-65.
    #[must_use]
    pub const fn default_for(purpose: HarwKeyPurpose) -> Option<Self> {
        match purpose {
            HarwKeyPurpose::AuditCheckpoint | HarwKeyPurpose::ArtifactSigning => {
                Some(Self::MlDsa87)
            }
            HarwKeyPurpose::NodeIdentity
            | HarwKeyPurpose::ServiceIdentity
            | HarwKeyPurpose::DeviceIdentity
            | HarwKeyPurpose::UserIdentity
            | HarwKeyPurpose::RequestAuthentication => Some(Self::MlDsa65),
            HarwKeyPurpose::SecretsKek | HarwKeyPurpose::ChannelBinding => {
                Some(Self::HpkeMlKem1024P384Shake256ChaCha20Poly1305)
            }
            HarwKeyPurpose::Pseudonymization => None,
        }
    }
}
