//! Canonical, domain-separated sign transcripts (Masterplan v2 §5).
//!
//! # Encoding (format version 1)
//! ```text
//! magic        8 bytes  "HARWSIG\0"
//! version      u8       1
//! purpose      u8       SignPurpose::code
//! domain_len   u8       length of the domain label
//! domain       bytes    SignPurpose::domain, e.g. "harw:node-handshake:v1"
//! field_count  u16 BE
//! field*       tag_len u8 || tag || value_len u32 BE || value
//! ```
//!
//! Every variable-length item is length-prefixed, so no two different field
//! lists encode to the same bytes (no concatenation ambiguity). The purpose
//! code *and* its domain label lead the transcript, so two transcripts of
//! different purposes never collide either. Integers are big-endian. The
//! encoding is a pure function of the builder calls: same inputs, same
//! bytes.
//!
//! A [`SignTranscript`] can only be *built* from a purpose plus tagged
//! fields; there is no constructor from raw bytes. That is what keeps a
//! signing key from becoming a signing oracle: whatever a caller asks to be
//! signed is framed by the key's own purpose.

use crate::error::EncryptError;
use crate::frame::{SecureFrameHeader, UnixMillis};
use crate::names::NodeId;
use crate::purpose::SignPurpose;

/// Leading magic of every transcript.
pub const TRANSCRIPT_MAGIC: [u8; 8] = *b"HARWSIG\0";
/// Current transcript format version.
pub const TRANSCRIPT_VERSION: u8 = 1;
/// Maximum number of fields.
pub const MAX_TRANSCRIPT_FIELDS: usize = 64;
/// Maximum encoded transcript size in bytes (a full secure frame fits).
pub const MAX_TRANSCRIPT_LEN: usize = (1 << 20) + (64 << 10);
/// Maximum tag length in bytes.
pub const MAX_TAG_LEN: usize = 32;

/// CryptGuard's default HTTP body limit for sign and verify requests
/// (`crypt_guard_hyper::BodyLimits::default().sign`, 64 KiB). Mirrored
/// here because this crate depends on the service layer only.
pub const CG_SIGN_BODY_LIMIT: usize = 64 * 1024;
/// Largest signature a Harw signing profile produces: ML-DSA-87, 4627
/// bytes (FIPS 204, table 2). ML-DSA-65 signatures (3309 bytes) are
/// smaller.
pub const MAX_KMS_SIGNATURE_LEN: usize = 4627;
/// `CGK1` framing a verify request adds around the transcript: magic (4)
/// and version (1), then a `u32` length prefix for `message` and one for
/// `signature`, plus the signature itself. A sign request (`message` only,
/// 9 bytes of framing) is strictly smaller, so this bound covers both.
pub const CGK1_VERIFY_OVERHEAD: usize = 4 + 1 + 4 + 4 + MAX_KMS_SIGNATURE_LEN;
/// Maximum encoded size of a transcript that is signed or verified through
/// the CryptGuard KMS: [`CG_SIGN_BODY_LIMIT`] minus [`CGK1_VERIFY_OVERHEAD`]
/// (60 896 bytes).
///
/// A transcript up to [`MAX_TRANSCRIPT_LEN`] is well-formed, but one above
/// this limit would be refused by the KMS HTTP adapter (`413`) on sign, or
/// could be signed yet never verified over the same route. Every KMS path
/// in this crate therefore enforces this smaller limit:
/// [`SignTranscriptBuilder::build_for_kms`], [`SignTranscript::validate_for_kms`],
/// [`crate::cg::sign_operation`], [`crate::cg::verify_operation`] and
/// [`crate::cg::HarwUsageAuthorizer`]. Secure frames with a large
/// ciphertext must be signed locally, not through the KMS.
pub const MAX_SIGNABLE_TRANSCRIPT_LEN: usize = CG_SIGN_BODY_LIMIT - CGK1_VERIFY_OVERHEAD;

/// A canonical transcript ready to be signed or verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignTranscript {
    purpose: SignPurpose,
    bytes: Vec<u8>,
}

impl SignTranscript {
    /// Start a transcript for `purpose`.
    #[must_use]
    pub fn builder(purpose: SignPurpose) -> SignTranscriptBuilder {
        SignTranscriptBuilder {
            purpose,
            fields: Vec::new(),
            len: header_len(purpose),
            error: None,
        }
    }

    /// Node handshake transcript (§5 example):
    /// `"harw:node-handshake:v1" || node || challenge || peer || issued_at`.
    ///
    /// # Errors
    /// Never in practice (bounded inputs); the `Result` is the builder's.
    pub fn node_handshake(
        node: &NodeId,
        challenge: &[u8; 32],
        peer: &NodeId,
        issued_at: UnixMillis,
    ) -> Result<Self, EncryptError> {
        Self::builder(SignPurpose::NodeHandshake)
            .field("node", node.as_str().as_bytes())
            .field("challenge", challenge)
            .field("peer", peer.as_str().as_bytes())
            .u64_field("issued_at", issued_at.0)
            .build()
    }

    /// Secure frame sender-authentication transcript (§8):
    /// `"harw:secure-frame:v1" || canonical_header || encrypted_frame_bytes`.
    ///
    /// # Errors
    /// - Any [`SecureFrameHeader::encode`] error.
    /// - [`EncryptError::TranscriptTooLarge`] for an oversize ciphertext.
    pub fn secure_frame(
        header: &SecureFrameHeader,
        ciphertext: &[u8],
    ) -> Result<Self, EncryptError> {
        let header_bytes = header.encode()?;
        Self::builder(SignPurpose::SecureFrame)
            .field("header", &header_bytes)
            .field("ciphertext", ciphertext)
            .build()
    }

    /// The sign purpose.
    #[must_use]
    pub const fn purpose(&self) -> SignPurpose {
        self.purpose
    }

    /// The canonical bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consume into the canonical bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Check that this transcript fits a KMS sign/verify request
    /// ([`MAX_SIGNABLE_TRANSCRIPT_LEN`]).
    ///
    /// # Errors
    /// [`EncryptError::TranscriptTooLarge`] if it does not.
    pub fn ensure_kms_size(&self) -> Result<(), EncryptError> {
        if self.bytes.len() > MAX_SIGNABLE_TRANSCRIPT_LEN {
            Err(EncryptError::TranscriptTooLarge)
        } else {
            Ok(())
        }
    }

    /// [`Self::validate`] plus the KMS size limit
    /// ([`MAX_SIGNABLE_TRANSCRIPT_LEN`]); what the crypto-service authorizer
    /// checks on every sign and verify payload.
    ///
    /// # Errors
    /// [`EncryptError::TranscriptTooLarge`] above the KMS limit, otherwise
    /// [`Self::validate`]'s errors.
    pub fn validate_for_kms(bytes: &[u8]) -> Result<SignPurpose, EncryptError> {
        if bytes.len() > MAX_SIGNABLE_TRANSCRIPT_LEN {
            return Err(EncryptError::TranscriptTooLarge);
        }
        Self::validate(bytes)
    }

    /// Check that `bytes` is a complete, well-formed transcript and return
    /// its purpose. For verifiers and tests; it does **not** yield a
    /// `SignTranscript` (no raw-bytes constructor, see module docs).
    ///
    /// # Errors
    /// [`EncryptError::MalformedTranscript`] for any deviation: wrong magic,
    /// unknown version or purpose, a domain label that does not match the
    /// purpose code, a bad tag, a short field or trailing bytes.
    pub fn validate(bytes: &[u8]) -> Result<SignPurpose, EncryptError> {
        let bad = EncryptError::MalformedTranscript;
        if bytes.len() > MAX_TRANSCRIPT_LEN {
            return Err(bad);
        }
        let mut r = Cursor { rest: bytes };
        if r.take(TRANSCRIPT_MAGIC.len()).ok_or(bad)? != TRANSCRIPT_MAGIC {
            return Err(bad);
        }
        if r.u8().ok_or(bad)? != TRANSCRIPT_VERSION {
            return Err(bad);
        }
        let purpose = SignPurpose::from_code(r.u8().ok_or(bad)?).ok_or(bad)?;
        let domain_len = usize::from(r.u8().ok_or(bad)?);
        if r.take(domain_len).ok_or(bad)? != purpose.domain().as_bytes() {
            return Err(bad);
        }
        let count = usize::from(r.u16().ok_or(bad)?);
        if count > MAX_TRANSCRIPT_FIELDS {
            return Err(bad);
        }
        for _ in 0..count {
            let tag_len = usize::from(r.u8().ok_or(bad)?);
            if !is_valid_tag(r.take(tag_len).ok_or(bad)?) {
                return Err(bad);
            }
            let value_len = usize::try_from(r.u32().ok_or(bad)?).map_err(|_| bad)?;
            r.take(value_len).ok_or(bad)?;
        }
        if r.rest.is_empty() {
            Ok(purpose)
        } else {
            Err(bad)
        }
    }
}

/// Builder for a [`SignTranscript`]. Field order is significant and kept.
#[derive(Debug)]
pub struct SignTranscriptBuilder {
    purpose: SignPurpose,
    fields: Vec<(&'static str, Vec<u8>)>,
    /// Encoded length so far; checked on every field so an oversize
    /// transcript is rejected before it is copied.
    len: usize,
    error: Option<EncryptError>,
}

impl SignTranscriptBuilder {
    /// Append a tagged byte field. Tags are 1..=32 bytes of `[a-z0-9_-]`.
    #[must_use]
    pub fn field(mut self, tag: &'static str, value: &[u8]) -> Self {
        if self.error.is_some() {
            return self;
        }
        let encoded = 1 + tag.len() + 4 + value.len();
        if !is_valid_tag(tag.as_bytes()) {
            self.error = Some(EncryptError::InvalidTranscriptField);
        } else if self.fields.len() >= MAX_TRANSCRIPT_FIELDS
            || value.len() > MAX_TRANSCRIPT_LEN
            || self.len + encoded > MAX_TRANSCRIPT_LEN
        {
            self.error = Some(EncryptError::TranscriptTooLarge);
        } else {
            self.len += encoded;
            self.fields.push((tag, value.to_vec()));
        }
        self
    }

    /// Append a tagged big-endian `u64` field.
    #[must_use]
    pub fn u64_field(self, tag: &'static str, value: u64) -> Self {
        self.field(tag, &value.to_be_bytes())
    }

    /// Encode.
    ///
    /// # Errors
    /// The first field error, or [`EncryptError::TranscriptTooLarge`] if
    /// the encoding exceeds [`MAX_TRANSCRIPT_LEN`].
    pub fn build(self) -> Result<SignTranscript, EncryptError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let domain = self.purpose.domain().as_bytes();
        let count =
            u16::try_from(self.fields.len()).map_err(|_| EncryptError::TranscriptTooLarge)?;
        let domain_len =
            u8::try_from(domain.len()).map_err(|_| EncryptError::TranscriptTooLarge)?;

        let mut bytes = Vec::with_capacity(self.len);
        bytes.extend_from_slice(&TRANSCRIPT_MAGIC);
        bytes.push(TRANSCRIPT_VERSION);
        bytes.push(self.purpose.code());
        bytes.push(domain_len);
        bytes.extend_from_slice(domain);
        bytes.extend_from_slice(&count.to_be_bytes());
        for (tag, value) in &self.fields {
            let tag_len =
                u8::try_from(tag.len()).map_err(|_| EncryptError::InvalidTranscriptField)?;
            let value_len =
                u32::try_from(value.len()).map_err(|_| EncryptError::TranscriptTooLarge)?;
            bytes.push(tag_len);
            bytes.extend_from_slice(tag.as_bytes());
            bytes.extend_from_slice(&value_len.to_be_bytes());
            bytes.extend_from_slice(value);
        }
        Ok(SignTranscript {
            purpose: self.purpose,
            bytes,
        })
    }

    /// Encode a transcript meant to be signed or verified through the
    /// CryptGuard KMS: as [`Self::build`], but with the smaller
    /// [`MAX_SIGNABLE_TRANSCRIPT_LEN`] limit.
    ///
    /// # Errors
    /// As [`Self::build`], and [`EncryptError::TranscriptTooLarge`] above
    /// [`MAX_SIGNABLE_TRANSCRIPT_LEN`] (checked before encoding).
    pub fn build_for_kms(self) -> Result<SignTranscript, EncryptError> {
        if self.error.is_none() && self.len > MAX_SIGNABLE_TRANSCRIPT_LEN {
            return Err(EncryptError::TranscriptTooLarge);
        }
        let transcript = self.build()?;
        transcript.ensure_kms_size()?;
        Ok(transcript)
    }
}

/// Encoded length of magic, version, purpose, domain and field count.
fn header_len(purpose: SignPurpose) -> usize {
    TRANSCRIPT_MAGIC.len() + 1 + 1 + 1 + purpose.domain().len() + 2
}

fn is_valid_tag(tag: &[u8]) -> bool {
    !tag.is_empty()
        && tag.len() <= MAX_TAG_LEN
        && tag
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// Minimal big-endian reader shared with the frame parser.
pub(crate) struct Cursor<'a> {
    pub(crate) rest: &'a [u8],
}

impl<'a> Cursor<'a> {
    pub(crate) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.rest.len() < n {
            return None;
        }
        let (head, tail) = self.rest.split_at(n);
        self.rest = tail;
        Some(head)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N).and_then(|s| <[u8; N]>::try_from(s).ok())
    }

    pub(crate) fn u8(&mut self) -> Option<u8> {
        self.array::<1>().map(|[b]| b)
    }

    pub(crate) fn u16(&mut self) -> Option<u16> {
        self.array().map(u16::from_be_bytes)
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        self.array().map(u32::from_be_bytes)
    }

    pub(crate) fn u64(&mut self) -> Option<u64> {
        self.array().map(u64::from_be_bytes)
    }
}
