//! Harw-specific crypto semantics over CryptGuard's service layer
//! (Crypto Masterplan v2 §3, wave H2).
//!
//! # What this crate owns (§3.1)
//! - [`HarwKeyPurpose`] — what a key is for (§4), with its [`KeyClass`],
//!   namespace `harw.<purpose>` and single [`SignPurpose`].
//! - [`HarwCryptoProfile`] — pinned algorithm suites with stable ids.
//! - [`KeyUsagePolicy`] — the purpose x operation matrix (§5); signing keys
//!   sign only [`SignTranscript`]s of their own purpose, wrap keys never
//!   sign, channel keys never wrap.
//! - [`SignTranscript`] — canonical, versioned, domain-separated,
//!   length-prefixed bytes to sign.
//! - [`SecureFrameHeader`] — the `HarwSecureFrameV1` header (§8) with a
//!   strict parser; [`IdempotencyKey`]; [`ReplayWindow`] /
//!   [`SenderReplayWindows`] (§9).
//! - [`cg`] — the one module that maps all of the above onto
//!   `crypt_guard_service` (`KeyRef`, `OpSet`, `NamespacePolicy`,
//!   `CryptoOperation`, an `Authorizer`).
//!
//! # What it does not own (§3.2)
//! No KEM, AEAD or signature implementation, no key provider, no Tower
//! stack, no Hyper server, no HTTP codec, no key storage. Those belong to
//! CryptGuard and to the Auth/Crypto Hub (H3).
//!
//! # Boundaries
//! - Not re-exported by the `harw-dod` facade (§3.3); `harw-dod` carries a
//!   `compile_fail` doctest and a manifest test for that, and
//!   `xtask gates edges` forbids the edge `harw-dod -> harw-dod-encrypt`.
//! - Must not be reachable from the Warden, the probes or any sensor
//!   (§20, §21): `xtask gates edges` forbids `crypt_guard*`, `hyper`, `tower`
//!   and this crate in their normal-dependency hulls.
//!
//! # Clone discipline (§10)
//! Every type here is a name, a policy value or public wire data, so the
//! ones that derive `Clone` carry no secret. Secret-bearing requests are
//! CryptGuard's `CryptoOperation`s, which are not `Clone`.
//!
//! # Example
//! ```
//! use harw_dod_encrypt::{
//!     HarwKeyPurpose, HarwKeyRef, KeyUsagePolicy, NodeId, SignTranscript, UnixMillis,
//! };
//!
//! # fn main() -> Result<(), harw_dod_encrypt::EncryptError> {
//! let node = NodeId::new("node-a")?;
//! let peer = NodeId::new("node-b")?;
//! let transcript = SignTranscript::node_handshake(&node, &[7; 32], &peer, UnixMillis(1))?;
//!
//! // The node identity key signs its own handshake transcript ...
//! KeyUsagePolicy::authorize_sign(HarwKeyPurpose::NodeIdentity, &transcript)?;
//! // ... and nothing else: the artifact-signing key refuses it.
//! assert!(KeyUsagePolicy::authorize_sign(HarwKeyPurpose::ArtifactSigning, &transcript).is_err());
//!
//! let key = HarwKeyRef::latest(HarwKeyPurpose::NodeIdentity, node);
//! assert_eq!(key.to_string(), "harw.node-identity/node-a");
//! # Ok(())
//! # }
//! ```

pub mod cg;
mod error;
mod frame;
mod names;
mod policy;
mod profile;
mod purpose;
mod replay;
mod transcript;

pub use error::{EncryptError, EncryptResult};
pub use frame::{
    FRAME_MAGIC, FRAME_VERSION, IDEMPOTENCY_KEY_LEN, IdempotencyKey, MAX_PAYLOAD_LEN,
    SecureFrameHeader, UnixMillis,
};
pub use names::{HarwKeyRef, HarwKeyVersion, MAX_NODE_ID_LEN, NodeId};
pub use policy::{GrantRole, HarwGrant, HarwKeyOp, KeyUsagePolicy};
pub use profile::{HarwCryptoProfile, ProfileKind};
pub use purpose::{HarwKeyPurpose, KeyClass, SignPurpose};
pub use replay::{REPLAY_WINDOW_BITS, ReplayWindow, SenderReplayWindows};
pub use transcript::{
    CG_SIGN_BODY_LIMIT, CGK1_VERIFY_OVERHEAD, MAX_KMS_SIGNATURE_LEN, MAX_SIGNABLE_TRANSCRIPT_LEN,
    MAX_TAG_LEN, MAX_TRANSCRIPT_FIELDS, MAX_TRANSCRIPT_LEN, SignTranscript, SignTranscriptBuilder,
    TRANSCRIPT_MAGIC, TRANSCRIPT_VERSION,
};
