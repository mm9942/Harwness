//! The one error type of `harw-dod-encrypt`.
//!
//! Every variant is content-free: no key bytes, no names, no payload
//! fragments appear in a message. Numeric fields (an unknown version or
//! code) are safe to log and help diagnose version skew between peers.

/// Errors of this crate.
///
/// # Description
/// Grouped by origin: names and versions, the key-usage policy, sign
/// transcripts, secure frame parsing and the replay window. Frame parse
/// errors are deliberately fine-grained so that each rejection path is
/// testable on its own (Masterplan v2 §36, wire/adversarial tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq, harw_macros::HarwError)]
pub enum EncryptError {
    // --- names and versions ---
    /// A node or key name violates the grammar (1..=64 bytes of
    /// `[A-Za-z0-9._-]`, not starting with `.`).
    #[msg("name is not a valid harw key or node name")]
    InvalidName,
    /// A key version of `0` (versions start at 1).
    #[msg("key version must be non-zero")]
    InvalidKeyVersion,

    // --- key-usage policy ---
    /// The key purpose does not permit this operation (§5).
    #[msg("key purpose does not permit this operation")]
    OperationNotAllowed,
    /// A sign transcript was presented for a key whose purpose is not the
    /// transcript's purpose (signing-oracle guard, §5).
    #[msg("sign transcript purpose does not match the key purpose")]
    TranscriptPurposeMismatch,
    /// The crypto profile does not fit the key purpose (e.g. an HPKE suite
    /// for a signing key).
    #[msg("crypto profile does not fit the key purpose")]
    ProfileMismatch,
    /// The key purpose has no CryptGuard algorithm yet (pseudonymization).
    #[msg("key purpose is not supported by the crypto provider")]
    UnsupportedByProvider,
    /// A key reference names a specific version where only the latest is
    /// meaningful (key generation).
    #[msg("key reference must not name a version here")]
    UnexpectedKeyVersion,

    // --- sign transcripts ---
    /// A transcript field tag violates the grammar (1..=32 bytes of
    /// `[a-z0-9_-]`).
    #[msg("sign transcript field tag is not well-formed")]
    InvalidTranscriptField,
    /// A transcript exceeds the field-count or size limit.
    #[msg("sign transcript exceeds its size limit")]
    TranscriptTooLarge,
    /// Bytes are not a well-formed transcript.
    #[msg("bytes are not a well-formed sign transcript")]
    MalformedTranscript,

    // --- secure frame ---
    /// The input ends before the header or payload is complete.
    #[msg("secure frame is truncated")]
    FrameTruncated,
    /// The input does not start with the frame magic.
    #[msg("secure frame magic is wrong")]
    FrameBadMagic,
    /// The frame version is not supported by this build.
    #[msg("secure frame version {0} is not supported")]
    FrameUnsupportedVersion(u16),
    /// The purpose code is unknown.
    #[msg("secure frame names unknown key purpose code {0}")]
    FrameUnknownPurpose(u8),
    /// The crypto profile id is unknown.
    #[msg("secure frame names unknown crypto profile {0}")]
    FrameUnknownProfile(u16),
    /// The declared payload length exceeds the frame limit.
    #[msg("secure frame payload exceeds the size limit")]
    FramePayloadTooLarge,
    /// A payload handed to the encoder differs from the header's
    /// `payload_len`.
    #[msg("secure frame payload length does not match its header")]
    FramePayloadLengthMismatch,
    /// Bytes remain after the header (or after the declared payload).
    #[msg("secure frame has trailing bytes")]
    FrameTrailingBytes,

    // --- replay window ---
    /// The sequence number was already accepted.
    #[msg("sequence number was already accepted (replay)")]
    ReplayDuplicate,
    /// The sequence number lies below the replay window.
    #[msg("sequence number is older than the replay window")]
    ReplayTooOld,
    /// A new sender would exceed the configured number of tracked senders.
    #[msg("replay window capacity for senders is exhausted")]
    ReplayTooManySenders,
}
