//! Key-usage policy (Masterplan v2 §5): which operation a key purpose
//! permits, and which operations a grant role receives.
//!
//! # The rules
//! - Lifecycle and metadata (generate, rotate, disable, enable, destroy,
//!   describe) are permitted for every purpose; *who* may run them is a
//!   grant question ([`GrantRole::Admin`]), not a purpose question.
//! - Signing keys sign and verify. They never encrypt, decrypt or wrap.
//! - Channel keys encrypt and decrypt. They never sign or wrap.
//! - Wrap keys (KEKs) wrap, unwrap and rewrap. They never sign and never
//!   encrypt arbitrary data.
//! - Pseudonymization keys have no CryptGuard operation yet; only lifecycle.
//! - Public-key export is permitted for every asymmetric class.
//!
//! # No signing oracle
//! [`KeyUsagePolicy::allows`] answers the payload-free question "may a key of
//! this purpose sign at all". The payload question is answered by
//! [`KeyUsagePolicy::authorize_sign`]: a signing key signs only a
//! [`SignTranscript`] of its own [`SignPurpose`](crate::SignPurpose), never
//! raw bytes. [`crate::cg::sign_operation`] is the only constructor of a
//! crypto-service sign request in this crate and runs that check first.

use crate::error::EncryptError;
use crate::purpose::{HarwKeyPurpose, KeyClass};
use crate::transcript::SignTranscript;

/// A payload-free key operation, Harw's own mirror of the crypto service's
/// operation kinds (converted in [`crate::cg`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HarwKeyOp {
    /// Create a key.
    Generate,
    /// Create a new primary version.
    Rotate,
    /// Disable a version.
    Disable,
    /// Re-enable a version.
    Enable,
    /// Destroy a version.
    Destroy,
    /// Read non-secret metadata.
    Describe,
    /// Export the public key.
    PublicKey,
    /// Encrypt data.
    Encrypt,
    /// Decrypt data (secret egress).
    Decrypt,
    /// Sign a transcript.
    Sign,
    /// Verify a signature.
    Verify,
    /// Wrap key material.
    WrapKey,
    /// Unwrap key material (secret egress).
    UnwrapKey,
    /// Re-wrap key material without egress.
    RewrapKey,
}

impl HarwKeyOp {
    /// Every operation.
    pub const ALL: [Self; 14] = [
        Self::Generate,
        Self::Rotate,
        Self::Disable,
        Self::Enable,
        Self::Destroy,
        Self::Describe,
        Self::PublicKey,
        Self::Encrypt,
        Self::Decrypt,
        Self::Sign,
        Self::Verify,
        Self::WrapKey,
        Self::UnwrapKey,
        Self::RewrapKey,
    ];

    /// Whether the operation is key lifecycle administration.
    #[must_use]
    pub const fn is_lifecycle(self) -> bool {
        matches!(
            self,
            Self::Generate | Self::Rotate | Self::Disable | Self::Enable | Self::Destroy
        )
    }
}

/// The purpose x operation matrix of §5.
///
/// # Examples
/// ```
/// use harw_dod_encrypt::{HarwKeyOp, HarwKeyPurpose, KeyUsagePolicy};
/// assert!(KeyUsagePolicy::allows(HarwKeyPurpose::NodeIdentity, HarwKeyOp::Sign));
/// assert!(!KeyUsagePolicy::allows(HarwKeyPurpose::SecretsKek, HarwKeyOp::Sign));
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyUsagePolicy;

impl KeyUsagePolicy {
    /// Whether a key of `purpose` may perform `op` at all.
    #[must_use]
    pub const fn allows(purpose: HarwKeyPurpose, op: HarwKeyOp) -> bool {
        if op.is_lifecycle() || matches!(op, HarwKeyOp::Describe) {
            return true;
        }
        match purpose.class() {
            KeyClass::Signing => {
                matches!(
                    op,
                    HarwKeyOp::PublicKey | HarwKeyOp::Sign | HarwKeyOp::Verify
                )
            }
            KeyClass::Encryption => {
                matches!(
                    op,
                    HarwKeyOp::PublicKey | HarwKeyOp::Encrypt | HarwKeyOp::Decrypt
                )
            }
            KeyClass::Wrapping => matches!(
                op,
                HarwKeyOp::PublicKey
                    | HarwKeyOp::WrapKey
                    | HarwKeyOp::UnwrapKey
                    | HarwKeyOp::RewrapKey
            ),
            KeyClass::Pseudonymization => false,
        }
    }

    /// [`Self::allows`] as a `Result`.
    ///
    /// # Errors
    /// [`EncryptError::OperationNotAllowed`] when the matrix denies `op`.
    pub const fn authorize(purpose: HarwKeyPurpose, op: HarwKeyOp) -> Result<(), EncryptError> {
        if Self::allows(purpose, op) {
            Ok(())
        } else {
            Err(EncryptError::OperationNotAllowed)
        }
    }

    /// Whether a key of `purpose` may sign `transcript`.
    ///
    /// # Errors
    /// - [`EncryptError::OperationNotAllowed`] if the purpose never signs.
    /// - [`EncryptError::TranscriptPurposeMismatch`] if the transcript's
    ///   sign purpose belongs to another key purpose.
    pub fn authorize_sign(
        purpose: HarwKeyPurpose,
        transcript: &SignTranscript,
    ) -> Result<(), EncryptError> {
        let Some(own) = purpose.sign_purpose() else {
            return Err(EncryptError::OperationNotAllowed);
        };
        if transcript.purpose() == own {
            Ok(())
        } else {
            Err(EncryptError::TranscriptPurposeMismatch)
        }
    }

    /// Every operation [`Self::allows`] for `purpose`.
    #[must_use]
    pub fn allowed_ops(purpose: HarwKeyPurpose) -> Vec<HarwKeyOp> {
        HarwKeyOp::ALL
            .into_iter()
            .filter(|op| Self::allows(purpose, *op))
            .collect()
    }
}

/// The role of a principal on the keys of one purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GrantRole {
    /// The key's owner (e.g. the node for its identity key): private-key
    /// operations plus public reads.
    Owner,
    /// A peer: public operations only (describe, public key, verify, encrypt
    /// to, wrap to). Never secret egress, never signing.
    Peer,
    /// Key administrator: lifecycle plus describe. Never uses the key.
    Admin,
}

impl GrantRole {
    /// Whether this role receives `op` (before intersecting with the
    /// purpose matrix).
    #[must_use]
    pub const fn permits(self, op: HarwKeyOp) -> bool {
        match self {
            Self::Owner => !op.is_lifecycle(),
            Self::Peer => matches!(
                op,
                HarwKeyOp::Describe
                    | HarwKeyOp::PublicKey
                    | HarwKeyOp::Verify
                    | HarwKeyOp::Encrypt
                    | HarwKeyOp::WrapKey
            ),
            Self::Admin => op.is_lifecycle() || matches!(op, HarwKeyOp::Describe),
        }
    }

    /// Operations of this role on keys of `purpose`: the role's operations
    /// intersected with [`KeyUsagePolicy::allows`].
    #[must_use]
    pub fn ops_for(self, purpose: HarwKeyPurpose) -> Vec<HarwKeyOp> {
        HarwKeyOp::ALL
            .into_iter()
            .filter(|op| self.permits(*op) && KeyUsagePolicy::allows(purpose, *op))
            .collect()
    }
}

/// One grant: `principal` has `role` on all keys of `purpose`.
///
/// The principal is the service-level identity string as the transport
/// established it; mapping from `harw_types::Principal` happens at the
/// AuthHub client boundary (drift report, decision D5), not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarwGrant {
    /// Authenticated caller identity.
    pub principal: Box<str>,
    /// Key purpose (selects the namespace `harw.<purpose>`).
    pub purpose: HarwKeyPurpose,
    /// Role on those keys.
    pub role: GrantRole,
}
