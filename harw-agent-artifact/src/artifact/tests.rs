//! Codec tests: roundtrip, determinism, tamper detection, structure, limits.

use super::{
    ARTIFACT_MAGIC, Artifact, ArtifactBuilder, FORMAT_VERSION, HASH_LEN, Limits, MAX_HEADER_LEN,
    MAX_PAYLOAD_COUNT, Payload, encode, parse,
};
use crate::error::{ArtifactError, Limit, PathRejection, TamperScope};
use crate::kind::PayloadKind;
use crate::test_support::{TestResult, ctx};
use serde_json::{Value, json};

fn header() -> Value {
    json!({
        "schema": "harwness.agent-ir/v2",
        "name": "reviewer",
        "permissions": {"tools": ["fs.read", "web.fetch"], "network": false},
        "limits": {"max_turns": 12}
    })
}

fn sample() -> Result<Artifact, ArtifactError> {
    ArtifactBuilder::new(&header())
        .add_payload(
            PayloadKind::Skill,
            "skills/review/instructions.md",
            b"# Review\n".to_vec(),
        )
        .add_payload(
            PayloadKind::Instructions,
            "instructions/system.md",
            b"Be precise.".to_vec(),
        )
        .add_payload(PayloadKind::Knowledge, "knowledge/a.txt", Vec::new())
        .add_payload(
            PayloadKind::Other("mcp.config".into()),
            "mcp/servers.json",
            b"{}".to_vec(),
        )
        .add_payload(PayloadKind::Template, "templates/out.md", b"{{x}}".to_vec())
        .build()
}

/// Recomputes the trailer so a structural edit gets past the hash check.
fn reseal(bytes: &mut [u8]) {
    let body_len = bytes.len() - HASH_LEN;
    let hash = *blake3::hash(&bytes[..body_len]).as_bytes();
    bytes[body_len..].copy_from_slice(&hash);
}

fn flip(bytes: &[u8], index: usize) -> Vec<u8> {
    let mut out = bytes.to_vec();
    out[index] ^= 0x01;
    out
}

fn raw(kind: PayloadKind, path: &str, data: &[u8]) -> Payload {
    Payload::new(kind, path.to_owned(), data.to_vec())
}

// --- roundtrip and layout ------------------------------------------------

#[test]
fn test_roundtrip_preserves_header_and_payloads() -> TestResult {
    let artifact = sample().map_err(ctx("build sample"))?;
    let parsed = Artifact::from_bytes(&artifact.to_bytes()).map_err(ctx("parse"))?;
    assert_eq!(parsed, artifact);
    assert_eq!(parsed.header_json(), crate::canonical_json(&header()));
    assert_eq!(
        parsed.header_value().map_err(ctx("header value"))?,
        header()
    );
    assert_eq!(parsed.payloads().len(), 5);
    let skill = parsed.payload(&PayloadKind::Skill, "skills/review/instructions.md");
    assert_eq!(skill.map(Payload::bytes), Some(&b"# Review\n"[..]));
    let empty = parsed.payload(&PayloadKind::Knowledge, "knowledge/a.txt");
    assert!(empty.is_some_and(Payload::is_empty));
    Ok(())
}

#[test]
fn test_payloads_are_sorted_by_kind_tag_then_path() -> TestResult {
    let artifact = sample().map_err(ctx("build sample"))?;
    let kinds: Vec<&PayloadKind> = artifact.payloads().iter().map(Payload::kind).collect();
    assert_eq!(
        kinds,
        [
            &PayloadKind::Instructions,
            &PayloadKind::Skill,
            &PayloadKind::Knowledge,
            &PayloadKind::Template,
            &PayloadKind::Other("mcp.config".into()),
        ]
    );
    Ok(())
}

#[test]
fn test_layout_preamble_and_trailer() -> TestResult {
    let artifact = sample().map_err(ctx("build sample"))?;
    let bytes = artifact.as_bytes();
    assert_eq!(&bytes[..8], ARTIFACT_MAGIC);
    assert_eq!(&bytes[8..10], &FORMAT_VERSION.to_le_bytes());
    assert_eq!(&bytes[10..12], &[0u8, 0]);
    let header_len = crate::canonical_json(&header()).len() as u32;
    assert_eq!(&bytes[12..16], &header_len.to_le_bytes());
    let body_len = bytes.len() - HASH_LEN;
    assert_eq!(artifact.digest().as_bytes(), &bytes[body_len..]);
    assert_eq!(
        artifact.digest().as_bytes(),
        blake3::hash(&bytes[..body_len]).as_bytes()
    );
    Ok(())
}

#[test]
fn test_payload_lookup_normalizes_the_query_path() -> TestResult {
    let artifact = sample().map_err(ctx("build sample"))?;
    assert!(
        artifact
            .payload(&PayloadKind::Template, "./templates//out.md")
            .is_some()
    );
    assert!(
        artifact
            .payload(&PayloadKind::Skill, "templates/out.md")
            .is_none()
    );
    assert!(
        artifact
            .payload(&PayloadKind::Template, "../out.md")
            .is_none()
    );
    Ok(())
}

#[test]
fn test_builder_normalizes_paths() -> TestResult {
    let artifact = ArtifactBuilder::new(&json!({}))
        .add_payload(PayloadKind::Knowledge, "./docs//a.md", b"a".to_vec())
        .build()
        .map_err(ctx("build"))?;
    let paths: Vec<&str> = artifact.payloads().iter().map(Payload::path).collect();
    assert_eq!(paths, ["docs/a.md"]);
    Ok(())
}

// --- determinism ---------------------------------------------------------

#[test]
fn test_insertion_order_does_not_change_bytes() -> TestResult {
    let mut reordered_header = serde_json::Map::new();
    reordered_header.insert("limits".into(), json!({"max_turns": 12}));
    reordered_header.insert(
        "permissions".into(),
        json!({"network": false, "tools": ["fs.read", "web.fetch"]}),
    );
    reordered_header.insert("name".into(), json!("reviewer"));
    reordered_header.insert("schema".into(), json!("harwness.agent-ir/v2"));

    let reversed = ArtifactBuilder::new(&Value::Object(reordered_header))
        .add_payload(PayloadKind::Template, "templates/out.md", b"{{x}}".to_vec())
        .add_payload(
            PayloadKind::Other("mcp.config".into()),
            "mcp/servers.json",
            b"{}".to_vec(),
        )
        .add_payload(PayloadKind::Knowledge, "knowledge/a.txt", Vec::new())
        .add_payload(
            PayloadKind::Instructions,
            "instructions/system.md",
            b"Be precise.".to_vec(),
        )
        .add_payload(
            PayloadKind::Skill,
            "./skills/review/instructions.md",
            b"# Review\n".to_vec(),
        )
        .build()
        .map_err(ctx("build reversed"))?;
    let original = sample().map_err(ctx("build sample"))?;
    assert_eq!(reversed.as_bytes(), original.as_bytes());
    assert_eq!(reversed.digest(), original.digest());
    Ok(())
}

#[test]
fn test_same_inputs_build_identical_bytes() -> TestResult {
    let first = sample().map_err(ctx("first build"))?;
    let second = sample().map_err(ctx("second build"))?;
    assert_eq!(first.to_bytes(), second.to_bytes());
    Ok(())
}

#[test]
fn test_different_content_changes_digest() -> TestResult {
    let first = sample().map_err(ctx("build sample"))?;
    let other = ArtifactBuilder::new(&header())
        .add_payload(
            PayloadKind::Instructions,
            "instructions/system.md",
            b"Be precise!".to_vec(),
        )
        .build()
        .map_err(ctx("build other"))?;
    assert_ne!(first.digest(), other.digest());
    Ok(())
}

// --- tamper detection ----------------------------------------------------

#[test]
fn test_flipped_header_byte_is_tampered() -> TestResult {
    let bytes = sample().map_err(ctx("build sample"))?.to_bytes();
    assert!(matches!(
        Artifact::from_bytes(&flip(&bytes, 17)),
        Err(ArtifactError::Tampered(TamperScope::Artifact))
    ));
    Ok(())
}

#[test]
fn test_flipped_payload_byte_is_tampered() -> TestResult {
    let bytes = sample().map_err(ctx("build sample"))?.to_bytes();
    // Last byte of the last payload's data, right before the trailer.
    let index = bytes.len() - HASH_LEN - 1;
    assert!(matches!(
        Artifact::from_bytes(&flip(&bytes, index)),
        Err(ArtifactError::Tampered(TamperScope::Artifact))
    ));
    Ok(())
}

#[test]
fn test_flipped_trailer_byte_is_tampered() -> TestResult {
    let bytes = sample().map_err(ctx("build sample"))?.to_bytes();
    assert!(matches!(
        Artifact::from_bytes(&flip(&bytes, bytes.len() - 1)),
        Err(ArtifactError::Tampered(TamperScope::Artifact))
    ));
    Ok(())
}

#[test]
fn test_resealed_payload_change_fails_payload_hash() -> TestResult {
    let bytes = sample().map_err(ctx("build sample"))?.to_bytes();
    let mut edited = flip(&bytes, bytes.len() - HASH_LEN - 1);
    reseal(&mut edited);
    match Artifact::from_bytes(&edited) {
        Err(ArtifactError::Tampered(TamperScope::Payload { kind, path })) => {
            assert_eq!(kind, PayloadKind::Other("mcp.config".into()));
            assert_eq!(path, "mcp/servers.json");
        }
        other => {
            return Err(ctx("expected payload hash mismatch")(format!("{other:?}")));
        }
    }
    Ok(())
}

#[test]
fn test_every_single_byte_flip_is_rejected() -> TestResult {
    let bytes = ArtifactBuilder::new(&json!({"a": 1}))
        .add_payload(PayloadKind::Skill, "s.md", b"xy".to_vec())
        .build()
        .map_err(ctx("build"))?
        .to_bytes();
    for index in 0..bytes.len() {
        assert!(
            Artifact::from_bytes(&flip(&bytes, index)).is_err(),
            "flip at {index} accepted"
        );
    }
    Ok(())
}

// --- structure -----------------------------------------------------------

#[test]
fn test_rejects_bad_magic_version_flags_and_truncation() -> TestResult {
    let bytes = sample().map_err(ctx("build sample"))?.to_bytes();
    assert!(matches!(
        Artifact::from_bytes(&flip(&bytes, 0)),
        Err(ArtifactError::BadMagic)
    ));
    assert!(matches!(
        Artifact::from_bytes(&flip(&bytes, 8)),
        Err(ArtifactError::UnsupportedVersion { found: 0 })
    ));
    assert!(matches!(
        Artifact::from_bytes(&flip(&bytes, 10)),
        Err(ArtifactError::UnsupportedFlags { flags: 1 })
    ));
    assert!(matches!(
        Artifact::from_bytes(b""),
        Err(ArtifactError::Truncated { .. })
    ));
    assert!(matches!(
        Artifact::from_bytes(&bytes[..12]),
        Err(ArtifactError::Truncated { .. })
    ));
    // Cutting the end off breaks the trailer.
    assert!(matches!(
        Artifact::from_bytes(&bytes[..bytes.len() - 1]),
        Err(ArtifactError::Tampered(TamperScope::Artifact))
    ));
    Ok(())
}

#[test]
fn test_rejects_bytes_between_data_and_trailer() -> TestResult {
    let (mut bytes, _) = encode(b"{}", &[raw(PayloadKind::Skill, "a", b"x")]);
    let trailer_start = bytes.len() - HASH_LEN;
    bytes.insert(trailer_start, 0);
    reseal(&mut bytes);
    assert!(matches!(
        Artifact::from_bytes(&bytes),
        Err(ArtifactError::TrailingBytes { extra: 1 })
    ));
    Ok(())
}

#[test]
fn test_rejects_non_canonical_header() {
    let (bytes, _) = encode(br#"{"b":1, "a":2}"#, &[]);
    assert!(matches!(
        Artifact::from_bytes(&bytes),
        Err(ArtifactError::NonCanonicalHeader)
    ));
    let (bytes, _) = encode(br#"{"a":1,"a":2}"#, &[]);
    assert!(matches!(
        Artifact::from_bytes(&bytes),
        Err(ArtifactError::NonCanonicalHeader)
    ));
    let (bytes, _) = encode(b"not json", &[]);
    assert!(matches!(
        Artifact::from_bytes(&bytes),
        Err(ArtifactError::InvalidHeader { .. })
    ));
}

#[test]
fn test_rejects_floats_in_header() {
    assert!(matches!(
        ArtifactBuilder::new(&json!({"temperature": 0.5})).build(),
        Err(ArtifactError::InvalidHeader { .. })
    ));
    let (bytes, _) = encode(br#"{"t":0.5}"#, &[]);
    assert!(matches!(
        Artifact::from_bytes(&bytes),
        Err(ArtifactError::InvalidHeader { .. })
    ));
}

#[test]
fn test_rejects_unsorted_and_duplicate_table() {
    let (bytes, _) = encode(
        b"{}",
        &[
            raw(PayloadKind::Skill, "b", b"1"),
            raw(PayloadKind::Skill, "a", b"2"),
        ],
    );
    assert!(matches!(
        Artifact::from_bytes(&bytes),
        Err(ArtifactError::UnsortedPayloads)
    ));
    let (bytes, _) = encode(
        b"{}",
        &[
            raw(PayloadKind::Skill, "a", b"1"),
            raw(PayloadKind::Instructions, "a", b"2"),
        ],
    );
    assert!(matches!(
        Artifact::from_bytes(&bytes),
        Err(ArtifactError::UnsortedPayloads)
    ));
    let (bytes, _) = encode(
        b"{}",
        &[
            raw(PayloadKind::Skill, "a", b"1"),
            raw(PayloadKind::Skill, "a", b"2"),
        ],
    );
    assert!(matches!(
        Artifact::from_bytes(&bytes),
        Err(ArtifactError::DuplicatePayload { .. })
    ));
}

#[test]
fn test_builder_rejects_duplicates_after_normalization() {
    let result = ArtifactBuilder::new(&json!({}))
        .add_payload(PayloadKind::Skill, "a/b.md", b"1".to_vec())
        .add_payload(PayloadKind::Skill, "./a//b.md", b"2".to_vec())
        .build();
    assert!(matches!(
        result,
        Err(ArtifactError::DuplicatePayload { .. })
    ));
}

#[test]
fn test_rejects_unknown_kind_tag() {
    let (mut bytes, _) = encode(b"{}", &[raw(PayloadKind::Skill, "a", b"x")]);
    // Preamble 12 + header_len 4 + header 2 + count 4 = kind tag at 22.
    bytes[22] = 9;
    reseal(&mut bytes);
    assert!(matches!(
        Artifact::from_bytes(&bytes),
        Err(ArtifactError::InvalidKind { .. })
    ));
}

#[test]
fn test_other_kind_names_are_validated() {
    for name in ["", "Upper", "has space", "skill", "context-program"] {
        let result = ArtifactBuilder::new(&json!({}))
            .add_payload(PayloadKind::Other(name.into()), "x", Vec::new())
            .build();
        assert!(
            matches!(result, Err(ArtifactError::InvalidKind { .. })),
            "{name:?} accepted"
        );
    }
    let long = "a".repeat(crate::kind::MAX_KIND_NAME_LEN + 1);
    assert!(matches!(
        ArtifactBuilder::new(&json!({}))
            .add_payload(PayloadKind::Other(long), "x", Vec::new())
            .build(),
        Err(ArtifactError::InvalidKind { .. })
    ));
}

#[test]
fn test_rejects_unnormalized_stored_path() {
    let (bytes, _) = encode(b"{}", &[raw(PayloadKind::Skill, "a//b", b"x")]);
    assert!(matches!(
        Artifact::from_bytes(&bytes),
        Err(ArtifactError::InvalidPath {
            reason: PathRejection::NotNormalized,
            ..
        })
    ));
    let (bytes, _) = encode(b"{}", &[raw(PayloadKind::Skill, "../x", b"x")]);
    assert!(matches!(
        Artifact::from_bytes(&bytes),
        Err(ArtifactError::InvalidPath {
            reason: PathRejection::ParentSegment,
            ..
        })
    ));
}

#[test]
fn test_builder_rejects_parent_and_absolute_paths() {
    for (path, expected) in [
        ("../escape.md", PathRejection::ParentSegment),
        ("skills/../../x", PathRejection::ParentSegment),
        ("/etc/passwd", PathRejection::Absolute),
        ("C:/x", PathRejection::Absolute),
    ] {
        let result = ArtifactBuilder::new(&json!({}))
            .add_payload(PayloadKind::Knowledge, path, b"x".to_vec())
            .build();
        assert!(
            matches!(result, Err(ArtifactError::InvalidPath { reason, .. }) if reason == expected),
            "{path:?} not rejected as {expected:?}"
        );
    }
}

// --- limits --------------------------------------------------------------

fn limit_of(result: Result<Artifact, ArtifactError>) -> Option<Limit> {
    match result {
        Err(ArtifactError::LimitExceeded { limit, .. }) => Some(limit),
        _ => None,
    }
}

#[test]
fn test_builder_enforces_real_payload_count_limit() {
    let mut builder = ArtifactBuilder::new(&json!({}));
    for index in 0..=MAX_PAYLOAD_COUNT {
        builder = builder.add_payload(PayloadKind::Knowledge, format!("k/{index}"), Vec::new());
    }
    assert_eq!(limit_of(builder.build()), Some(Limit::PayloadCount));
}

#[test]
fn test_builder_enforces_real_header_limit() {
    let big = json!({"blob": "x".repeat(MAX_HEADER_LEN)});
    assert_eq!(
        limit_of(ArtifactBuilder::new(&big).build()),
        Some(Limit::HeaderLen)
    );
}

#[test]
fn test_builder_enforces_payload_and_artifact_limits() {
    let small = Limits {
        header_len: 64,
        payload_count: 4,
        payload_len: 8,
        artifact_len: 200,
    };
    let too_long_payload = ArtifactBuilder::new(&json!({}))
        .add_payload(PayloadKind::Skill, "a", vec![0u8; 9])
        .build_with(small);
    assert_eq!(limit_of(too_long_payload), Some(Limit::PayloadLen));

    let mut builder = ArtifactBuilder::new(&json!({}));
    for index in 0..4 {
        builder = builder.add_payload(PayloadKind::Skill, format!("p{index}"), vec![0u8; 8]);
    }
    // 4 × (1 + 2 + 2 + 8 + 32) table + 32 data + 54 fixed > 200.
    assert_eq!(
        limit_of(builder.build_with(small)),
        Some(Limit::ArtifactLen)
    );
}

#[test]
fn test_parser_enforces_limits() -> TestResult {
    let bytes = sample().map_err(ctx("build sample"))?.to_bytes();
    let defaults = Limits::DEFAULT;
    let cases = [
        (
            Limits {
                artifact_len: bytes.len() - 1,
                ..defaults
            },
            Limit::ArtifactLen,
        ),
        (
            Limits {
                header_len: 10,
                ..defaults
            },
            Limit::HeaderLen,
        ),
        (
            Limits {
                payload_count: 4,
                ..defaults
            },
            Limit::PayloadCount,
        ),
        (
            Limits {
                payload_len: 3,
                ..defaults
            },
            Limit::PayloadLen,
        ),
    ];
    for (limits, expected) in cases {
        assert_eq!(limit_of(parse(&bytes, limits)), Some(expected));
    }
    Ok(())
}

#[test]
fn test_parser_checks_declared_header_len_before_reading() {
    let (mut bytes, _) = encode(b"{}", &[]);
    let declared = (MAX_HEADER_LEN as u32 + 1).to_le_bytes();
    bytes[12..16].copy_from_slice(&declared);
    reseal(&mut bytes);
    assert_eq!(
        limit_of(Artifact::from_bytes(&bytes)),
        Some(Limit::HeaderLen)
    );
}
