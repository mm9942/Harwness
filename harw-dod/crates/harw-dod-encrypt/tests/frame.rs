//! `HarwSecureFrameV1` header: round trip and every parse rejection.

mod common;

use common::{TestError, TestResult, ctx};
use harw_dod_encrypt::{
    EncryptError, FRAME_MAGIC, FRAME_VERSION, HarwCryptoProfile, HarwKeyPurpose, HarwKeyRef,
    HarwKeyVersion, IdempotencyKey, KeyUsagePolicy, MAX_PAYLOAD_LEN, NodeId, SecureFrameHeader,
    SignPurpose, SignTranscript, UnixMillis,
};

// Offsets in the encoded header of `header()` (key id and sender are both
// six bytes long).
const OFF_VERSION: usize = 4;
const OFF_PROFILE: usize = 6;
const OFF_PURPOSE: usize = 8;
const OFF_KEY_VERSION: usize = 9;
const OFF_KEY_ID_LEN: usize = 13;
const OFF_KEY_ID: usize = 14;
const OFF_PAYLOAD_LEN_FROM_END: usize = 4;

fn header() -> TestResult<SecureFrameHeader> {
    Ok(SecureFrameHeader {
        profile: HarwCryptoProfile::MlDsa65,
        purpose: HarwKeyPurpose::RequestAuthentication,
        key_id: NodeId::new("node-a").map_err(ctx("key id"))?,
        key_version: HarwKeyVersion::new(3).map_err(ctx("version"))?,
        sender: NodeId::new("node-a").map_err(ctx("sender"))?,
        sequence: 0x0102_0304_0506_0708,
        issued_at: UnixMillis(1_700_000_000_000),
        idempotency_key: IdempotencyKey::from_bytes([0xab; 16]),
        payload_len: 5,
    })
}

fn set(bytes: &mut [u8], index: usize, value: &[u8]) -> TestResult {
    let end = index + value.len();
    let slot = bytes
        .get_mut(index..end)
        .ok_or(TestError::Missing("offset inside the header"))?;
    slot.copy_from_slice(value);
    Ok(())
}

fn encoded() -> TestResult<Vec<u8>> {
    header()?.encode().map_err(ctx("encode"))
}

#[test]
fn test_header_round_trip() -> TestResult {
    let h = header()?;
    let bytes = h.encode().map_err(ctx("encode"))?;
    assert_eq!(bytes.get(..4), Some(FRAME_MAGIC.as_slice()));
    assert_eq!(SecureFrameHeader::decode(&bytes).map_err(ctx("decode"))?, h);
    Ok(())
}

#[test]
fn test_header_encoding_is_deterministic() -> TestResult {
    assert_eq!(encoded()?, encoded()?);
    Ok(())
}

#[test]
fn test_frame_round_trip_returns_exact_payload() -> TestResult {
    let h = header()?;
    let frame = h.encode_frame(b"hello").map_err(ctx("encode frame"))?;
    let (parsed, payload) = SecureFrameHeader::decode_frame(&frame).map_err(ctx("decode frame"))?;
    assert_eq!(parsed, h);
    assert_eq!(payload, b"hello");
    Ok(())
}

#[test]
fn test_encode_frame_rejects_payload_length_lie() -> TestResult {
    assert_eq!(
        header()?.encode_frame(b"four").err(),
        Some(EncryptError::FramePayloadLengthMismatch)
    );
    Ok(())
}

#[test]
fn test_decode_rejects_trailing_bytes() -> TestResult {
    let mut bytes = encoded()?;
    bytes.push(0);
    assert_eq!(
        SecureFrameHeader::decode(&bytes).err(),
        Some(EncryptError::FrameTrailingBytes)
    );
    Ok(())
}

#[test]
fn test_decode_frame_rejects_short_and_long_payload() -> TestResult {
    let frame = header()?.encode_frame(b"hello").map_err(ctx("frame"))?;
    let short = frame
        .get(..frame.len() - 1)
        .ok_or(TestError::Missing("short"))?;
    assert_eq!(
        SecureFrameHeader::decode_frame(short).err(),
        Some(EncryptError::FrameTruncated)
    );
    let mut long = frame.clone();
    long.push(0);
    assert_eq!(
        SecureFrameHeader::decode_frame(&long).err(),
        Some(EncryptError::FrameTrailingBytes)
    );
    Ok(())
}

#[test]
fn test_decode_rejects_every_truncation() -> TestResult {
    let bytes = encoded()?;
    for cut in 0..bytes.len() {
        let prefix = bytes.get(..cut).ok_or(TestError::Missing("prefix"))?;
        let err = SecureFrameHeader::decode(prefix).err();
        // A cut inside the magic cannot be told apart from a wrong magic.
        assert!(
            err == Some(EncryptError::FrameTruncated)
                || (cut < FRAME_MAGIC.len() && err == Some(EncryptError::FrameBadMagic)),
            "cut at {cut}: {err:?}"
        );
    }
    Ok(())
}

#[test]
fn test_decode_rejects_bad_magic() -> TestResult {
    let mut bytes = encoded()?;
    set(&mut bytes, 0, b"XWSF")?;
    assert_eq!(
        SecureFrameHeader::decode(&bytes).err(),
        Some(EncryptError::FrameBadMagic)
    );
    Ok(())
}

#[test]
fn test_decode_rejects_unknown_version() -> TestResult {
    for version in [0u16, FRAME_VERSION + 1, u16::MAX] {
        let mut bytes = encoded()?;
        set(&mut bytes, OFF_VERSION, &version.to_be_bytes())?;
        assert_eq!(
            SecureFrameHeader::decode(&bytes).err(),
            Some(EncryptError::FrameUnsupportedVersion(version))
        );
    }
    Ok(())
}

#[test]
fn test_decode_rejects_unknown_version_before_anything_else() -> TestResult {
    // A future version may change the layout: only magic + version are read.
    let mut bytes = FRAME_MAGIC.to_vec();
    bytes.extend_from_slice(&2u16.to_be_bytes());
    assert_eq!(
        SecureFrameHeader::decode(&bytes).err(),
        Some(EncryptError::FrameUnsupportedVersion(2))
    );
    Ok(())
}

#[test]
fn test_decode_rejects_unknown_profile() -> TestResult {
    let mut bytes = encoded()?;
    set(&mut bytes, OFF_PROFILE, &0x0999u16.to_be_bytes())?;
    assert_eq!(
        SecureFrameHeader::decode(&bytes).err(),
        Some(EncryptError::FrameUnknownProfile(0x0999))
    );
    Ok(())
}

#[test]
fn test_decode_rejects_unknown_purpose() -> TestResult {
    for code in [0u8, 11, u8::MAX] {
        let mut bytes = encoded()?;
        set(&mut bytes, OFF_PURPOSE, &[code])?;
        assert_eq!(
            SecureFrameHeader::decode(&bytes).err(),
            Some(EncryptError::FrameUnknownPurpose(code))
        );
    }
    Ok(())
}

#[test]
fn test_decode_rejects_profile_that_does_not_fit_purpose() -> TestResult {
    let mut bytes = encoded()?;
    let hpke = HarwCryptoProfile::HpkeMlKem1024P384Shake256ChaCha20Poly1305.id();
    set(&mut bytes, OFF_PROFILE, &hpke.to_be_bytes())?;
    assert_eq!(
        SecureFrameHeader::decode(&bytes).err(),
        Some(EncryptError::ProfileMismatch)
    );
    Ok(())
}

#[test]
fn test_decode_rejects_zero_key_version() -> TestResult {
    let mut bytes = encoded()?;
    set(&mut bytes, OFF_KEY_VERSION, &0u32.to_be_bytes())?;
    assert_eq!(
        SecureFrameHeader::decode(&bytes).err(),
        Some(EncryptError::InvalidKeyVersion)
    );
    Ok(())
}

#[test]
fn test_decode_rejects_invalid_names() -> TestResult {
    // Zero-length key id.
    let mut empty = encoded()?;
    set(&mut empty, OFF_KEY_ID_LEN, &[0])?;
    assert_eq!(
        SecureFrameHeader::decode(&empty).err(),
        Some(EncryptError::InvalidName)
    );
    // Forbidden byte inside the key id.
    let mut slash = encoded()?;
    set(&mut slash, OFF_KEY_ID, b"/")?;
    assert_eq!(
        SecureFrameHeader::decode(&slash).err(),
        Some(EncryptError::InvalidName)
    );
    // Leading dot.
    let mut dot = encoded()?;
    set(&mut dot, OFF_KEY_ID, b".")?;
    assert_eq!(
        SecureFrameHeader::decode(&dot).err(),
        Some(EncryptError::InvalidName)
    );
    Ok(())
}

#[test]
fn test_decode_rejects_oversize_payload_len() -> TestResult {
    let mut bytes = encoded()?;
    let at = bytes.len() - OFF_PAYLOAD_LEN_FROM_END;
    set(&mut bytes, at, &(MAX_PAYLOAD_LEN + 1).to_be_bytes())?;
    assert_eq!(
        SecureFrameHeader::decode(&bytes).err(),
        Some(EncryptError::FramePayloadTooLarge)
    );
    Ok(())
}

#[test]
fn test_encode_refuses_inconsistent_header() -> TestResult {
    let mut h = header()?;
    h.profile = HarwCryptoProfile::HpkeMlKem768X25519HkdfSha256ChaCha20Poly1305;
    assert_eq!(h.encode().err(), Some(EncryptError::ProfileMismatch));

    let mut h = header()?;
    h.payload_len = MAX_PAYLOAD_LEN + 1;
    assert_eq!(h.encode().err(), Some(EncryptError::FramePayloadTooLarge));
    Ok(())
}

/// One header mutation for the field-coverage test.
type Mutation = fn(&mut SecureFrameHeader) -> TestResult;

#[test]
fn test_every_header_field_changes_the_encoding() -> TestResult {
    let base = encoded()?;
    let variants: [Mutation; 9] = [
        |h| {
            h.profile = HarwCryptoProfile::MlDsa87;
            Ok(())
        },
        |h| {
            h.purpose = HarwKeyPurpose::NodeIdentity;
            Ok(())
        },
        |h| {
            h.key_id = NodeId::new("node-z").map_err(ctx("key id"))?;
            Ok(())
        },
        |h| {
            h.key_version = HarwKeyVersion::new(4).map_err(ctx("version"))?;
            Ok(())
        },
        |h| {
            h.sender = NodeId::new("node-z").map_err(ctx("sender"))?;
            Ok(())
        },
        |h| {
            h.sequence += 1;
            Ok(())
        },
        |h| {
            h.issued_at = UnixMillis(h.issued_at.0 + 1);
            Ok(())
        },
        |h| {
            h.idempotency_key = IdempotencyKey::from_bytes([0xac; 16]);
            Ok(())
        },
        |h| {
            h.payload_len += 1;
            Ok(())
        },
    ];
    for (i, mutate) in variants.iter().enumerate() {
        let mut h = header()?;
        mutate(&mut h)?;
        assert_ne!(h.encode().map_err(ctx("encode"))?, base, "field #{i}");
    }
    Ok(())
}

#[test]
fn test_frame_transcript_binds_header_and_ciphertext() -> TestResult {
    let h = header()?;
    let t = SignTranscript::secure_frame(&h, b"ct").map_err(ctx("transcript"))?;
    assert_eq!(t.purpose(), SignPurpose::SecureFrame);
    KeyUsagePolicy::authorize_sign(h.key_ref().purpose(), &t).map_err(ctx("own purpose"))?;

    let other = SignTranscript::secure_frame(&h, b"cu").map_err(ctx("other"))?;
    assert_ne!(t.as_bytes(), other.as_bytes());
    Ok(())
}

#[test]
fn test_header_key_ref_is_versioned() -> TestResult {
    let h = header()?;
    let key: HarwKeyRef = h.key_ref();
    assert_eq!(key.to_string(), "harw.request-authentication/node-a@3");
    Ok(())
}

#[test]
fn test_idempotency_key_prints_hex() {
    let key = IdempotencyKey::from_bytes([0x0f; 16]);
    assert_eq!(key.to_string(), "0f".repeat(16));
    assert_eq!(key.as_bytes(), &[0x0f; 16]);
}
