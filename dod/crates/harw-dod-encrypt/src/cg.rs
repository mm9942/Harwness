//! The CryptGuard boundary: every conversion between Harw types and
//! `crypt_guard_service` types lives here, and nowhere else.
//!
//! # Why a single module
//! The rest of this crate's public API names only Harw types
//! ([`HarwKeyPurpose`], [`HarwKeyOp`], [`HarwKeyRef`], …). Functions here
//! return or accept CryptGuard types (`KeyRef`, `OpKind`, `OpSet`,
//! `NamespacePolicy`, `CryptoOperation`, `KeyAlgorithm`) because their
//! callers — the Auth/Crypto Hub (H3) and its clients — hand them to the
//! crypto service. Keeping them in one module makes the dependency surface
//! auditable and replaceable.
//!
//! CryptGuard's `Principal` is used only under the alias `CgPrincipal` and
//! never glob-imported, so it cannot be confused with
//! `harw_types::Principal` (drift report, decision D5).
//!
//! # What this module enforces
//! - [`key_ref`]: the `harw.<purpose>/<owner>[@version]` naming convention.
//! - [`sign_operation`]: the only sign-request constructor; it refuses any
//!   transcript whose purpose is not the key's (no signing oracle, §5) or
//!   that exceeds [`MAX_SIGNABLE_TRANSCRIPT_LEN`](crate::MAX_SIGNABLE_TRANSCRIPT_LEN).
//! - [`HarwUsageAuthorizer`]: a CryptGuard `Authorizer` that applies
//!   [`KeyUsagePolicy`] to every operation on a `harw.*` namespace before an
//!   inner authorizer (for example [`namespace_policy_for`]) runs. An
//!   unknown `harw.*` namespace or unknown operation kind is rejected.
//!
//! # Transcript binding on every route
//! CryptGuard hands the authorizer the whole `CryptoOperation`, including
//! the `message` of a `Sign` (a `SecretBytes`, readable through
//! `AsRef<[u8]>`) and of a `Verify`. [`HarwUsageAuthorizer`] therefore
//! enforces transcript binding itself, independent of how the request was
//! built: on a `harw.*` namespace, a `Sign` or `Verify` message must pass
//! [`SignTranscript::validate_for_kms`] and carry exactly the key purpose's
//! own [`SignPurpose`](crate::SignPurpose); raw bytes, a transcript of a
//! foreign purpose or an oversize transcript are `Forbidden`. This holds
//! for requests arriving over the HTTP adapter as well as for requests
//! built with [`sign_operation`] / [`verify_operation`], which apply the
//! same checks early so a caller gets a precise [`EncryptError`].

use crypt_guard_service::pq_hpke::{Aead, Kdf, Kem, Suite};
use crypt_guard_service::{
    Authorizer, CryptoOperation, CryptoServiceError, GenerateKey, KeyAlgorithm, KeyId,
    KeyNamespace, KeyRef, KeyVersion, MessageBlob, NamespacePolicy, OpKind, OpSet,
    Principal as CgPrincipal, RequestContext, SecretBytes, Sign, SignatureAlgorithm, SignatureBlob,
    Verify,
};

use crate::error::EncryptError;
use crate::names::{HarwKeyRef, HarwKeyVersion, NodeId};
use crate::policy::{GrantRole, HarwGrant, HarwKeyOp, KeyUsagePolicy};
use crate::profile::HarwCryptoProfile;
use crate::purpose::HarwKeyPurpose;
use crate::transcript::SignTranscript;

/// Prefix of every Harw namespace in the crypto service.
pub const HARW_NAMESPACE_PREFIX: &str = "harw.";

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

/// The service namespace of `purpose`, `harw.<purpose>`.
///
/// # Errors
/// [`EncryptError::InvalidName`]; unreachable for the static labels, kept
/// instead of a panic.
pub fn namespace(purpose: HarwKeyPurpose) -> Result<KeyNamespace, EncryptError> {
    KeyNamespace::new(purpose.namespace()).map_err(|_| EncryptError::InvalidName)
}

/// The service key reference of the latest version of `node`'s key of
/// `purpose`: namespace `harw.<purpose>`, key id `<node>`.
///
/// # Errors
/// [`EncryptError::InvalidName`]; unreachable because [`NodeId`] is a
/// subset of CryptGuard's key-id grammar.
pub fn key_ref(purpose: HarwKeyPurpose, node: &NodeId) -> Result<KeyRef, EncryptError> {
    key_ref_of(&HarwKeyRef::latest(purpose, node.clone()))
}

/// Convert a [`HarwKeyRef`] (with its version, if any).
///
/// # Errors
/// As [`key_ref`].
pub fn key_ref_of(key: &HarwKeyRef) -> Result<KeyRef, EncryptError> {
    let namespace = namespace(key.purpose())?;
    let id = KeyId::new(key.owner().as_str()).map_err(|_| EncryptError::InvalidName)?;
    Ok(match key.version() {
        Some(version) => KeyRef::versioned(namespace, id, key_version(version)?),
        None => KeyRef::latest(namespace, id),
    })
}

/// Convert a service key reference back, if it follows the Harw
/// convention; `None` for foreign namespaces.
#[must_use]
pub fn harw_key_ref(key: &KeyRef) -> Option<HarwKeyRef> {
    let purpose = HarwKeyPurpose::from_namespace(key.namespace.as_str())?;
    let owner = NodeId::new(key.id.as_str()).ok()?;
    Some(match key.version {
        Some(version) => {
            HarwKeyRef::versioned(purpose, owner, HarwKeyVersion::new(version.get()).ok()?)
        }
        None => HarwKeyRef::latest(purpose, owner),
    })
}

/// Convert a key version.
///
/// # Errors
/// [`EncryptError::InvalidKeyVersion`]; unreachable (both types are
/// non-zero).
pub fn key_version(version: HarwKeyVersion) -> Result<KeyVersion, EncryptError> {
    KeyVersion::new(version.get()).map_err(|_| EncryptError::InvalidKeyVersion)
}

// ---------------------------------------------------------------------------
// Algorithms
// ---------------------------------------------------------------------------

/// The CryptGuard algorithm of a profile.
#[must_use]
pub fn key_algorithm(profile: HarwCryptoProfile) -> KeyAlgorithm {
    match profile {
        HarwCryptoProfile::MlDsa65 => KeyAlgorithm::Signature(SignatureAlgorithm::MlDsa65),
        HarwCryptoProfile::MlDsa87 => KeyAlgorithm::Signature(SignatureAlgorithm::MlDsa87),
        HarwCryptoProfile::HpkeMlKem1024P384Shake256ChaCha20Poly1305 => KeyAlgorithm::Hpke {
            suite: Suite::new(Kem::MlKem1024P384, Kdf::Shake256, Aead::ChaCha20Poly1305),
        },
        HarwCryptoProfile::HpkeMlKem768X25519HkdfSha256ChaCha20Poly1305 => KeyAlgorithm::Hpke {
            suite: Suite::new(Kem::MlKem768X25519, Kdf::HkdfSha256, Aead::ChaCha20Poly1305),
        },
    }
}

// ---------------------------------------------------------------------------
// Operations and grants
// ---------------------------------------------------------------------------

/// Harw operation -> service operation kind.
#[must_use]
pub const fn op_kind(op: HarwKeyOp) -> OpKind {
    match op {
        HarwKeyOp::Generate => OpKind::Generate,
        HarwKeyOp::Rotate => OpKind::Rotate,
        HarwKeyOp::Disable => OpKind::Disable,
        HarwKeyOp::Enable => OpKind::Enable,
        HarwKeyOp::Destroy => OpKind::Destroy,
        HarwKeyOp::Describe => OpKind::Describe,
        HarwKeyOp::PublicKey => OpKind::PublicKey,
        HarwKeyOp::Encrypt => OpKind::Encrypt,
        HarwKeyOp::Decrypt => OpKind::Decrypt,
        HarwKeyOp::Sign => OpKind::Sign,
        HarwKeyOp::Verify => OpKind::Verify,
        HarwKeyOp::WrapKey => OpKind::WrapKey,
        HarwKeyOp::UnwrapKey => OpKind::UnwrapKey,
        HarwKeyOp::RewrapKey => OpKind::RewrapKey,
    }
}

/// Service operation kind -> Harw operation; `None` for a kind this crate
/// does not know (`OpKind` is `#[non_exhaustive]`), which callers must
/// treat as "deny".
#[must_use]
pub fn harw_op(kind: OpKind) -> Option<HarwKeyOp> {
    HarwKeyOp::ALL.into_iter().find(|op| op_kind(*op) == kind)
}

/// [`KeyUsagePolicy::allows`] over a service operation kind. Unknown kinds
/// are denied.
#[must_use]
pub fn allows(purpose: HarwKeyPurpose, kind: OpKind) -> bool {
    harw_op(kind).is_some_and(|op| KeyUsagePolicy::allows(purpose, op))
}

/// Every operation the usage policy permits for `purpose`, as an `OpSet`.
#[must_use]
pub fn op_set(purpose: HarwKeyPurpose) -> OpSet {
    to_op_set(KeyUsagePolicy::allowed_ops(purpose))
}

/// The operations of `role` on keys of `purpose`, as an `OpSet`.
#[must_use]
pub fn role_op_set(purpose: HarwKeyPurpose, role: GrantRole) -> OpSet {
    to_op_set(role.ops_for(purpose))
}

fn to_op_set(ops: Vec<HarwKeyOp>) -> OpSet {
    ops.into_iter()
        .fold(OpSet::NONE, |set, op| set.with(op_kind(op)))
}

/// Build a deny-by-default CryptGuard `NamespacePolicy` from Harw grants.
/// Each grant gives its principal [`role_op_set`] in `harw.<purpose>`;
/// several grants for the same principal and purpose are merged.
///
/// # Errors
/// [`EncryptError::InvalidName`] (unreachable, see [`namespace`]).
pub fn namespace_policy_for(grants: &[HarwGrant]) -> Result<NamespacePolicy, EncryptError> {
    let mut policy = NamespacePolicy::new();
    for grant in grants {
        policy.grant(
            CgPrincipal::new(&grant.principal),
            namespace(grant.purpose)?,
            role_op_set(grant.purpose, grant.role),
        );
    }
    Ok(policy)
}

// ---------------------------------------------------------------------------
// Request construction
// ---------------------------------------------------------------------------

/// Build a sign operation for `transcript` under `key`.
///
/// # Description
/// The only way this crate produces a `CryptoOperation::Sign`. The message
/// is always a canonical [`SignTranscript`] of the key's own sign purpose;
/// raw bytes cannot be signed through this API.
///
/// # Errors
/// - [`KeyUsagePolicy::authorize_sign`]'s errors.
/// - [`EncryptError::TranscriptTooLarge`] above
///   [`MAX_SIGNABLE_TRANSCRIPT_LEN`](crate::MAX_SIGNABLE_TRANSCRIPT_LEN)
///   (the KMS would refuse the request).
/// - A name conversion error.
pub fn sign_operation(
    key: &HarwKeyRef,
    transcript: SignTranscript,
) -> Result<CryptoOperation, EncryptError> {
    KeyUsagePolicy::authorize_sign(key.purpose(), &transcript)?;
    transcript.ensure_kms_size()?;
    Ok(CryptoOperation::Sign(Sign {
        key: key_ref_of(key)?,
        message: SecretBytes::from_vec(transcript.into_bytes()),
    }))
}

/// Build a verify operation for `signature` over `transcript`.
///
/// # Errors
/// As [`sign_operation`]: verifying a transcript of a foreign purpose is
/// refused as well, so a signature can never be "re-purposed" by a verifier
/// that picked the wrong key.
pub fn verify_operation(
    key: &HarwKeyRef,
    transcript: &SignTranscript,
    signature: &[u8],
) -> Result<CryptoOperation, EncryptError> {
    KeyUsagePolicy::authorize_sign(key.purpose(), transcript)?;
    transcript.ensure_kms_size()?;
    Ok(CryptoOperation::Verify(Verify {
        key: key_ref_of(key)?,
        message: MessageBlob::new(transcript.as_bytes().to_vec()),
        signature: SignatureBlob::new(signature.to_vec()),
    }))
}

/// Build a key generation for `key` with `profile`.
///
/// # Errors
/// - [`EncryptError::UnexpectedKeyVersion`] if `key` names a version
///   (generation creates version 1).
/// - [`EncryptError::ProfileMismatch`] if the profile does not fit the
///   purpose.
pub fn generate_operation(
    key: &HarwKeyRef,
    profile: HarwCryptoProfile,
) -> Result<CryptoOperation, EncryptError> {
    if key.version().is_some() {
        return Err(EncryptError::UnexpectedKeyVersion);
    }
    if !profile.supports(key.purpose()) {
        return Err(EncryptError::ProfileMismatch);
    }
    Ok(CryptoOperation::Generate(GenerateKey {
        namespace: namespace(key.purpose())?,
        id: KeyId::new(key.owner().as_str()).map_err(|_| EncryptError::InvalidName)?,
        algorithm: key_algorithm(profile),
    }))
}

// ---------------------------------------------------------------------------
// Authorizer
// ---------------------------------------------------------------------------

/// A CryptGuard `Authorizer` that enforces [`KeyUsagePolicy`] on `harw.*`
/// namespaces and then delegates to `inner`.
///
/// On a `harw.*` namespace it rejects (`Forbidden`), before `inner` runs:
/// - an unknown `harw.*` namespace or an operation kind the key purpose
///   does not permit;
/// - a `Sign` or `Verify` whose message is not a well-formed transcript
///   within [`MAX_SIGNABLE_TRANSCRIPT_LEN`](crate::MAX_SIGNABLE_TRANSCRIPT_LEN)
///   ([`SignTranscript::validate_for_kms`]) of exactly the key purpose's
///   own sign purpose (transcript binding, §5; see the module docs).
///
/// Operations on namespaces outside `harw.*` are judged by `inner` alone.
#[derive(Debug)]
pub struct HarwUsageAuthorizer<A> {
    inner: A,
}

impl<A> HarwUsageAuthorizer<A> {
    /// Guard `inner`.
    pub const fn new(inner: A) -> Self {
        Self { inner }
    }

    /// Borrow the inner authorizer.
    pub const fn inner(&self) -> &A {
        &self.inner
    }
}

impl<A: Authorizer> Authorizer for HarwUsageAuthorizer<A> {
    fn authorize(
        &self,
        ctx: &RequestContext,
        op: &CryptoOperation,
    ) -> Result<(), CryptoServiceError> {
        let kind = op.kind();
        for ns in op.namespaces().into_iter().flatten() {
            let name = ns.as_str();
            if !name.starts_with(HARW_NAMESPACE_PREFIX) {
                continue;
            }
            let Some(purpose) = HarwKeyPurpose::from_namespace(name) else {
                return Err(CryptoServiceError::Forbidden);
            };
            if !allows(purpose, kind) {
                return Err(CryptoServiceError::Forbidden);
            }
            // Transcript binding: the payload itself, not only the kind.
            let message: Option<&[u8]> = match op {
                CryptoOperation::Sign(sign) => Some(sign.message.as_ref()),
                CryptoOperation::Verify(verify) => Some(verify.message.as_bytes()),
                _ => None,
            };
            if message.is_some_and(|m| !is_own_transcript(purpose, m)) {
                return Err(CryptoServiceError::Forbidden);
            }
        }
        self.inner.authorize(ctx, op)
    }
}

/// Whether `message` is a KMS-sized, well-formed transcript of exactly
/// `purpose`'s own sign purpose. `false` for purposes that do not sign.
fn is_own_transcript(purpose: HarwKeyPurpose, message: &[u8]) -> bool {
    let Some(own) = purpose.sign_purpose() else {
        return false;
    };
    SignTranscript::validate_for_kms(message).is_ok_and(|found| found == own)
}
