//! Sign transcripts: determinism, domain separation, length prefixing and
//! the strict validator.

mod common;

use common::{TestError, TestResult, ctx};
use harw_dod_encrypt::{
    EncryptError, MAX_TRANSCRIPT_FIELDS, MAX_TRANSCRIPT_LEN, NodeId, SignPurpose, SignTranscript,
    TRANSCRIPT_MAGIC, TRANSCRIPT_VERSION, UnixMillis,
};

/// Overwrite one byte without panicking indexing.
fn set(bytes: &mut [u8], index: usize, value: u8) -> TestResult {
    let slot = bytes
        .get_mut(index)
        .ok_or(TestError::Missing("byte index inside the transcript"))?;
    *slot = value;
    Ok(())
}

fn nodes() -> TestResult<(NodeId, NodeId)> {
    Ok((
        NodeId::new("node-a").map_err(ctx("node-a"))?,
        NodeId::new("node-b").map_err(ctx("node-b"))?,
    ))
}

#[test]
fn test_transcript_is_deterministic() -> TestResult {
    let (a, b) = nodes()?;
    let first =
        SignTranscript::node_handshake(&a, &[9; 32], &b, UnixMillis(42)).map_err(ctx("first"))?;
    let second =
        SignTranscript::node_handshake(&a, &[9; 32], &b, UnixMillis(42)).map_err(ctx("second"))?;
    assert_eq!(first, second);
    assert_eq!(first.as_bytes(), second.as_bytes());
    Ok(())
}

#[test]
fn test_transcript_has_exact_canonical_layout() -> TestResult {
    let t = SignTranscript::builder(SignPurpose::AuditCheckpoint)
        .field("seq", &[1, 2])
        .build()
        .map_err(ctx("build"))?;

    let domain = b"harw:audit-checkpoint:v1";
    let mut want = Vec::new();
    want.extend_from_slice(&TRANSCRIPT_MAGIC);
    want.push(TRANSCRIPT_VERSION);
    want.push(SignPurpose::AuditCheckpoint.code());
    want.push(u8::try_from(domain.len()).map_err(ctx("domain len"))?);
    want.extend_from_slice(domain);
    want.extend_from_slice(&1u16.to_be_bytes());
    want.push(3);
    want.extend_from_slice(b"seq");
    want.extend_from_slice(&2u32.to_be_bytes());
    want.extend_from_slice(&[1, 2]);

    assert_eq!(t.as_bytes(), want.as_slice());
    Ok(())
}

#[test]
fn test_same_fields_different_purpose_never_collide() -> TestResult {
    let mut seen = Vec::new();
    for purpose in SignPurpose::ALL {
        let t = SignTranscript::builder(purpose)
            .field("x", b"same")
            .build()
            .map_err(ctx("build"))?;
        assert!(
            t.as_bytes()
                .windows(purpose.domain().len())
                .any(|w| w == purpose.domain().as_bytes()),
            "transcript must carry its domain label {}",
            purpose.domain()
        );
        assert!(
            !seen.contains(&t.as_bytes().to_vec()),
            "{purpose:?} collided"
        );
        seen.push(t.into_bytes());
    }
    Ok(())
}

#[test]
fn test_length_prefix_prevents_concatenation_ambiguity() -> TestResult {
    let split_one = SignTranscript::builder(SignPurpose::ArtifactManifest)
        .field("a", b"ab")
        .field("b", b"c")
        .build()
        .map_err(ctx("one"))?;
    let split_two = SignTranscript::builder(SignPurpose::ArtifactManifest)
        .field("a", b"a")
        .field("b", b"bc")
        .build()
        .map_err(ctx("two"))?;
    assert_ne!(split_one.as_bytes(), split_two.as_bytes());
    Ok(())
}

#[test]
fn test_field_order_is_significant() -> TestResult {
    let ab = SignTranscript::builder(SignPurpose::SecureFrame)
        .field("a", b"1")
        .field("b", b"2")
        .build()
        .map_err(ctx("ab"))?;
    let ba = SignTranscript::builder(SignPurpose::SecureFrame)
        .field("b", b"2")
        .field("a", b"1")
        .build()
        .map_err(ctx("ba"))?;
    assert_ne!(ab.as_bytes(), ba.as_bytes());
    Ok(())
}

#[test]
fn test_invalid_tags_are_rejected() {
    for tag in [
        "",
        "Upper",
        "with space",
        "a:b",
        "0123456789012345678901234567890123",
    ] {
        let result = SignTranscript::builder(SignPurpose::NodeHandshake)
            .field(tag, b"v")
            .build();
        assert_eq!(
            result.err(),
            Some(EncryptError::InvalidTranscriptField),
            "tag {tag:?}"
        );
    }
}

#[test]
fn test_too_many_fields_are_rejected() {
    let mut builder = SignTranscript::builder(SignPurpose::NodeHandshake);
    for _ in 0..=MAX_TRANSCRIPT_FIELDS {
        builder = builder.field("f", b"");
    }
    assert_eq!(
        builder.build().err(),
        Some(EncryptError::TranscriptTooLarge)
    );
}

#[test]
fn test_oversize_transcript_is_rejected() {
    let big = vec![0u8; MAX_TRANSCRIPT_LEN];
    let result = SignTranscript::builder(SignPurpose::SecureFrame)
        .field("ciphertext", &big)
        .build();
    assert_eq!(result.err(), Some(EncryptError::TranscriptTooLarge));
}

#[test]
fn test_validate_accepts_built_transcripts_and_reports_purpose() -> TestResult {
    let (a, b) = nodes()?;
    let t =
        SignTranscript::node_handshake(&a, &[1; 32], &b, UnixMillis(7)).map_err(ctx("build"))?;
    assert_eq!(
        SignTranscript::validate(t.as_bytes()).map_err(ctx("validate"))?,
        SignPurpose::NodeHandshake
    );
    let empty = SignTranscript::builder(SignPurpose::UserAssertion)
        .build()
        .map_err(ctx("empty"))?;
    assert_eq!(
        SignTranscript::validate(empty.as_bytes()).map_err(ctx("validate empty"))?,
        SignPurpose::UserAssertion
    );
    Ok(())
}

#[test]
fn test_validate_rejects_every_malformation() -> TestResult {
    let t = SignTranscript::builder(SignPurpose::ServiceHandshake)
        .field("k", b"value")
        .build()
        .map_err(ctx("build"))?;
    let good = t.as_bytes().to_vec();
    let bad = Some(EncryptError::MalformedTranscript);

    // Raw bytes that are not a transcript at all (the "please sign this
    // blob" case) never validate.
    assert_eq!(SignTranscript::validate(b"arbitrary blob").err(), bad);

    let mut wrong_magic = good.clone();
    set(&mut wrong_magic, 0, b'X')?;
    assert_eq!(SignTranscript::validate(&wrong_magic).err(), bad);

    let mut wrong_version = good.clone();
    set(&mut wrong_version, 8, TRANSCRIPT_VERSION + 1)?;
    assert_eq!(SignTranscript::validate(&wrong_version).err(), bad);

    let mut unknown_purpose = good.clone();
    set(&mut unknown_purpose, 9, 0)?;
    assert_eq!(SignTranscript::validate(&unknown_purpose).err(), bad);

    // Purpose code swapped but domain label kept: label/code mismatch.
    let mut swapped = good.clone();
    set(&mut swapped, 9, SignPurpose::NodeHandshake.code())?;
    assert_eq!(SignTranscript::validate(&swapped).err(), bad);

    let mut trailing = good.clone();
    trailing.push(0);
    assert_eq!(SignTranscript::validate(&trailing).err(), bad);

    for cut in 0..good.len() {
        assert_eq!(
            SignTranscript::validate(good.get(..cut).unwrap_or_default()).err(),
            bad,
            "truncated at {cut}"
        );
    }
    Ok(())
}
