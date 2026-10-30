//! The CryptGuard boundary (`harw_dod_encrypt::cg`): key-ref naming, op
//! sets, namespace policy, request construction and the usage authorizer.

mod common;

use common::{TestError, TestResult, ctx};
use crypt_guard_service::{
    AllowAll, Authorizer, CiphertextBlob, CryptoContext, CryptoOperation, CryptoServiceError,
    DescribeKey, KeyAlgorithm, KeyId, KeyNamespace, KeyRef, MessageBlob, OpKind,
    Principal as CgPrincipal, RequestContext, RequestId, RewrapKey, SecretBytes, Sign,
    SignatureAlgorithm, SignatureBlob, Verify,
};
use harw_dod_encrypt::cg::{
    self, HarwUsageAuthorizer, allows, generate_operation, harw_key_ref, harw_op, key_ref,
    key_ref_of, namespace_policy_for, op_kind, op_set, role_op_set, sign_operation,
    verify_operation,
};
use harw_dod_encrypt::{
    CG_SIGN_BODY_LIMIT, CGK1_VERIFY_OVERHEAD, EncryptError, GrantRole, HarwCryptoProfile,
    HarwGrant, HarwKeyOp, HarwKeyPurpose, HarwKeyRef, HarwKeyVersion, KeyUsagePolicy,
    MAX_KMS_SIGNATURE_LEN, MAX_SIGNABLE_TRANSCRIPT_LEN, MAX_TRANSCRIPT_LEN, NodeId, SignPurpose,
    SignTranscript,
};

fn node(name: &str) -> TestResult<NodeId> {
    NodeId::new(name).map_err(ctx("node id"))
}

fn transcript(purpose: SignPurpose) -> TestResult<SignTranscript> {
    SignTranscript::builder(purpose)
        .field("x", b"y")
        .build()
        .map_err(ctx("transcript"))
}

fn describe(namespace: &str) -> TestResult<CryptoOperation> {
    Ok(CryptoOperation::Describe(DescribeKey {
        key: KeyRef::latest(
            KeyNamespace::new(namespace).map_err(ctx("namespace"))?,
            KeyId::new("k").map_err(ctx("key id"))?,
        ),
    }))
}

fn ctx0() -> RequestContext {
    RequestContext::anonymous(RequestId(1))
}

#[test]
fn test_key_ref_naming_convention() -> TestResult {
    let r = key_ref(HarwKeyPurpose::NodeIdentity, &node("node-a")?).map_err(ctx("key_ref"))?;
    assert_eq!(r.namespace.as_str(), "harw.node-identity");
    assert_eq!(r.id.as_str(), "node-a");
    assert_eq!(r.version, None);
    assert_eq!(r.to_string(), "harw.node-identity/node-a");

    for purpose in HarwKeyPurpose::ALL {
        let r = key_ref(purpose, &node("n")?).map_err(ctx("key_ref"))?;
        assert_eq!(r.namespace.as_str(), format!("harw.{}", purpose.label()));
    }
    Ok(())
}

#[test]
fn test_versioned_key_ref_and_round_trip() -> TestResult {
    let version = HarwKeyVersion::new(7).map_err(ctx("version"))?;
    let harw = HarwKeyRef::versioned(HarwKeyPurpose::SecretsKek, node("vault")?, version);
    let service = key_ref_of(&harw).map_err(ctx("key_ref_of"))?;
    assert_eq!(service.to_string(), "harw.secrets-kek/vault@7");
    assert_eq!(service.to_string(), harw.to_string());
    assert_eq!(harw_key_ref(&service), Some(harw));
    Ok(())
}

#[test]
fn test_foreign_namespace_is_not_a_harw_key() -> TestResult {
    let foreign = KeyRef::latest(
        KeyNamespace::new("team-a").map_err(ctx("ns"))?,
        KeyId::new("k").map_err(ctx("id"))?,
    );
    assert_eq!(harw_key_ref(&foreign), None);
    Ok(())
}

#[test]
fn test_op_kind_conversion_is_a_bijection() {
    for op in HarwKeyOp::ALL {
        assert_eq!(harw_op(op_kind(op)), Some(op));
    }
}

#[test]
fn test_op_set_matches_policy_for_every_purpose_and_op() {
    for purpose in HarwKeyPurpose::ALL {
        let set = op_set(purpose);
        for op in HarwKeyOp::ALL {
            let allowed = KeyUsagePolicy::allows(purpose, op);
            assert_eq!(set.contains(op_kind(op)), allowed, "{purpose:?} {op:?}");
            assert_eq!(allows(purpose, op_kind(op)), allowed, "{purpose:?} {op:?}");
        }
    }
}

#[test]
fn test_namespace_policy_for_grants_role_ops_only() -> TestResult {
    let policy = namespace_policy_for(&[
        HarwGrant {
            principal: "node-a".into(),
            purpose: HarwKeyPurpose::NodeIdentity,
            role: GrantRole::Owner,
        },
        HarwGrant {
            principal: "auth-hub".into(),
            purpose: HarwKeyPurpose::NodeIdentity,
            role: GrantRole::Admin,
        },
    ])
    .map_err(ctx("policy"))?;
    let ns = cg::namespace(HarwKeyPurpose::NodeIdentity).map_err(ctx("ns"))?;

    let owner = policy.allowed(&CgPrincipal::new("node-a"), &ns);
    assert_eq!(
        owner,
        role_op_set(HarwKeyPurpose::NodeIdentity, GrantRole::Owner)
    );
    assert!(owner.contains(OpKind::Sign));
    assert!(!owner.contains(OpKind::Rotate));
    assert!(!owner.contains(OpKind::Decrypt));

    let admin = policy.allowed(&CgPrincipal::new("auth-hub"), &ns);
    assert!(admin.contains(OpKind::Rotate));
    assert!(!admin.contains(OpKind::Sign));

    // Deny by default: other principals and other namespaces get nothing.
    let other_ns = cg::namespace(HarwKeyPurpose::SecretsKek).map_err(ctx("ns"))?;
    assert!(
        !policy
            .allowed(&CgPrincipal::new("node-a"), &other_ns)
            .contains(OpKind::Describe)
    );
    assert!(
        !policy
            .allowed(&CgPrincipal::new("mallory"), &ns)
            .contains(OpKind::Describe)
    );
    Ok(())
}

#[test]
fn test_sign_operation_requires_matching_transcript() -> TestResult {
    let key = HarwKeyRef::latest(HarwKeyPurpose::NodeIdentity, node("node-a")?);
    let op = sign_operation(&key, transcript(SignPurpose::NodeHandshake)?)
        .map_err(ctx("own purpose"))?;
    assert_eq!(op.kind(), OpKind::Sign);
    match op {
        CryptoOperation::Sign(sign) => {
            assert_eq!(sign.key.to_string(), "harw.node-identity/node-a");
        }
        _ => {
            return Err(TestError::Unexpected(
                "expected a sign operation".to_owned(),
            ));
        }
    }

    assert_eq!(
        sign_operation(&key, transcript(SignPurpose::ArtifactManifest)?).err(),
        Some(EncryptError::TranscriptPurposeMismatch)
    );
    let kek = HarwKeyRef::latest(HarwKeyPurpose::SecretsKek, node("vault")?);
    assert_eq!(
        sign_operation(&kek, transcript(SignPurpose::NodeHandshake)?).err(),
        Some(EncryptError::OperationNotAllowed)
    );
    Ok(())
}

#[test]
fn test_verify_operation_requires_matching_transcript() -> TestResult {
    let key = HarwKeyRef::latest(HarwKeyPurpose::AuditCheckpoint, node("audit")?);
    let op = verify_operation(&key, &transcript(SignPurpose::AuditCheckpoint)?, &[1, 2, 3])
        .map_err(ctx("verify"))?;
    assert_eq!(op.kind(), OpKind::Verify);
    assert_eq!(
        verify_operation(&key, &transcript(SignPurpose::SecureFrame)?, &[1]).err(),
        Some(EncryptError::TranscriptPurposeMismatch)
    );
    Ok(())
}

#[test]
fn test_generate_operation_checks_profile_and_version() -> TestResult {
    let key = HarwKeyRef::latest(HarwKeyPurpose::NodeIdentity, node("node-a")?);
    let op = generate_operation(&key, HarwCryptoProfile::MlDsa65).map_err(ctx("generate"))?;
    match op {
        CryptoOperation::Generate(g) => {
            assert_eq!(g.namespace.as_str(), "harw.node-identity");
            assert_eq!(g.id.as_str(), "node-a");
            assert_eq!(
                g.algorithm,
                KeyAlgorithm::Signature(SignatureAlgorithm::MlDsa65)
            );
        }
        _ => {
            return Err(TestError::Unexpected(
                "expected a generate operation".to_owned(),
            ));
        }
    }
    assert_eq!(
        generate_operation(
            &key,
            HarwCryptoProfile::HpkeMlKem1024P384Shake256ChaCha20Poly1305
        )
        .err(),
        Some(EncryptError::ProfileMismatch)
    );
    let versioned = HarwKeyRef::versioned(
        HarwKeyPurpose::NodeIdentity,
        node("node-a")?,
        HarwKeyVersion::FIRST,
    );
    assert_eq!(
        generate_operation(&versioned, HarwCryptoProfile::MlDsa65).err(),
        Some(EncryptError::UnexpectedKeyVersion)
    );
    Ok(())
}

#[test]
fn test_default_hpke_profile_is_cryptguard_default_suite() {
    assert_eq!(
        cg::key_algorithm(HarwCryptoProfile::HpkeMlKem1024P384Shake256ChaCha20Poly1305),
        KeyAlgorithm::Hpke {
            suite: crypt_guard_service::pq_hpke::DEFAULT_SUITE
        }
    );
}

#[test]
fn test_authorizer_enforces_usage_policy_on_harw_namespaces() -> TestResult {
    let auth = HarwUsageAuthorizer::new(AllowAll);
    let kek = key_ref(HarwKeyPurpose::SecretsKek, &node("vault")?).map_err(ctx("kek"))?;
    let id = key_ref(HarwKeyPurpose::NodeIdentity, &node("node-a")?).map_err(ctx("id"))?;

    // Describe is allowed everywhere.
    assert_eq!(
        auth.authorize(&ctx0(), &describe("harw.secrets-kek")?),
        Ok(())
    );
    // An unknown harw.* namespace is rejected (fail closed).
    assert_eq!(
        auth.authorize(&ctx0(), &describe("harw.bogus")?),
        Err(CryptoServiceError::Forbidden)
    );
    // Foreign namespaces are left to the inner authorizer.
    assert_eq!(auth.authorize(&ctx0(), &describe("team-a")?), Ok(()));

    // Rewrap between two KEKs is allowed; into an identity namespace it is not.
    let rewrap = |to: KeyRef| {
        CryptoOperation::RewrapKey(RewrapKey {
            from: kek.clone(),
            from_context: CryptoContext::default(),
            to,
            to_context: CryptoContext::default(),
            wrapped: CiphertextBlob::new(Vec::new()),
        })
    };
    assert_eq!(auth.authorize(&ctx0(), &rewrap(kek.clone())), Ok(()));
    assert_eq!(
        auth.authorize(&ctx0(), &rewrap(id.clone())),
        Err(CryptoServiceError::Forbidden)
    );

    // A sign request on the KEK namespace never reaches the provider.
    let kek_harw = HarwKeyRef::latest(HarwKeyPurpose::SecretsKek, node("vault")?);
    assert!(sign_operation(&kek_harw, transcript(SignPurpose::NodeHandshake)?).is_err());
    let raw_sign = CryptoOperation::Sign(crypt_guard_service::Sign {
        key: kek,
        message: crypt_guard_service::SecretBytes::copy_from_slice(b"raw"),
    });
    assert_eq!(
        auth.authorize(&ctx0(), &raw_sign),
        Err(CryptoServiceError::Forbidden)
    );
    Ok(())
}

#[test]
fn test_authorizer_delegates_to_inner_policy() -> TestResult {
    let inner = namespace_policy_for(&[HarwGrant {
        principal: "node-a".into(),
        purpose: HarwKeyPurpose::NodeIdentity,
        role: GrantRole::Peer,
    }])
    .map_err(ctx("policy"))?;
    let auth = HarwUsageAuthorizer::new(inner);
    // Allowed by usage policy, but anonymous: the inner policy rejects.
    assert_eq!(
        auth.authorize(&ctx0(), &describe("harw.node-identity")?),
        Err(CryptoServiceError::Unauthenticated)
    );
    let known = RequestContext {
        request_id: RequestId(2),
        principal: Some(CgPrincipal::new("node-a")),
    };
    assert_eq!(
        auth.authorize(&known, &describe("harw.node-identity")?),
        Ok(())
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Transcript binding in the authorizer (sign/verify payloads)
// ---------------------------------------------------------------------------

/// An inner authorizer with a distinctive verdict: seeing `Conflict` proves
/// the request got past the Harw checks and reached `inner`.
struct InnerMarker;

impl Authorizer for InnerMarker {
    fn authorize(
        &self,
        _ctx: &RequestContext,
        _op: &CryptoOperation,
    ) -> Result<(), CryptoServiceError> {
        Err(CryptoServiceError::Conflict)
    }
}

fn raw_sign(key: &KeyRef, message: &[u8]) -> CryptoOperation {
    CryptoOperation::Sign(Sign {
        key: key.clone(),
        message: SecretBytes::copy_from_slice(message),
    })
}

fn raw_verify(key: &KeyRef, message: &[u8]) -> CryptoOperation {
    CryptoOperation::Verify(Verify {
        key: key.clone(),
        message: MessageBlob::new(message.to_vec()),
        signature: SignatureBlob::new(vec![0u8; 16]),
    })
}

/// A well-formed transcript of `purpose` whose encoding is exactly `len`
/// bytes (built with the general, non-KMS limit).
fn transcript_of_len(purpose: SignPurpose, len: usize) -> TestResult<SignTranscript> {
    let base = SignTranscript::builder(purpose)
        .field("pad", b"")
        .build()
        .map_err(ctx("base transcript"))?
        .as_bytes()
        .len();
    let pad = len
        .checked_sub(base)
        .ok_or(TestError::Missing("pad length"))?;
    let t = SignTranscript::builder(purpose)
        .field("pad", &vec![0u8; pad])
        .build()
        .map_err(ctx("padded transcript"))?;
    if t.as_bytes().len() != len {
        return Err(TestError::Unexpected(format!(
            "padded transcript has {} bytes, want {len}",
            t.as_bytes().len()
        )));
    }
    Ok(t)
}

fn identity_key() -> TestResult<KeyRef> {
    key_ref(HarwKeyPurpose::NodeIdentity, &node("node-a")?).map_err(ctx("identity key"))
}

#[test]
fn test_authorizer_rejects_raw_sign_bytes_on_harw_namespace() -> TestResult {
    let auth = HarwUsageAuthorizer::new(AllowAll);
    let key = identity_key()?;
    let messages: [&[u8]; 3] = [b"raw", b"", b"HARWSIG\0"];
    for message in messages {
        assert_eq!(
            auth.authorize(&ctx0(), &raw_sign(&key, message)),
            Err(CryptoServiceError::Forbidden)
        );
    }
    // A truncated own-purpose transcript is not a transcript either.
    let own = transcript(SignPurpose::NodeHandshake)?;
    let cut = own
        .as_bytes()
        .split_last()
        .map(|(_, rest)| rest)
        .ok_or(TestError::Missing("transcript bytes"))?;
    assert_eq!(
        auth.authorize(&ctx0(), &raw_sign(&key, cut)),
        Err(CryptoServiceError::Forbidden)
    );
    Ok(())
}

#[test]
fn test_authorizer_rejects_sign_of_foreign_purpose_transcript() -> TestResult {
    let auth = HarwUsageAuthorizer::new(AllowAll);
    let key = identity_key()?;
    for purpose in [
        SignPurpose::ArtifactManifest,
        SignPurpose::SecureFrame,
        SignPurpose::AuditCheckpoint,
        SignPurpose::ServiceHandshake,
    ] {
        let foreign = transcript(purpose)?;
        assert_eq!(
            auth.authorize(&ctx0(), &raw_sign(&key, foreign.as_bytes())),
            Err(CryptoServiceError::Forbidden),
            "{purpose:?}"
        );
    }
    Ok(())
}

#[test]
fn test_authorizer_allows_own_purpose_sign_and_delegates() -> TestResult {
    let key = identity_key()?;
    let own = transcript(SignPurpose::NodeHandshake)?;

    // Passes the Harw checks: AllowAll allows ...
    let allow = HarwUsageAuthorizer::new(AllowAll);
    assert_eq!(
        allow.authorize(&ctx0(), &raw_sign(&key, own.as_bytes())),
        Ok(())
    );
    let harw_key = HarwKeyRef::latest(HarwKeyPurpose::NodeIdentity, node("node-a")?);
    let built = sign_operation(&harw_key, own.clone()).map_err(ctx("sign_operation"))?;
    assert_eq!(allow.authorize(&ctx0(), &built), Ok(()));

    // ... and the verdict really is the inner authorizer's.
    let marker = HarwUsageAuthorizer::new(InnerMarker);
    assert_eq!(
        marker.authorize(&ctx0(), &raw_sign(&key, own.as_bytes())),
        Err(CryptoServiceError::Conflict)
    );
    // A bad payload is refused before the inner authorizer runs.
    assert_eq!(
        marker.authorize(&ctx0(), &raw_sign(&key, b"raw")),
        Err(CryptoServiceError::Forbidden)
    );
    Ok(())
}

#[test]
fn test_authorizer_binds_verify_payload_to_key_purpose() -> TestResult {
    let key = identity_key()?;
    let allow = HarwUsageAuthorizer::new(AllowAll);
    let marker = HarwUsageAuthorizer::new(InnerMarker);

    // Raw bytes and foreign transcripts are refused.
    assert_eq!(
        allow.authorize(&ctx0(), &raw_verify(&key, b"raw")),
        Err(CryptoServiceError::Forbidden)
    );
    let foreign = transcript(SignPurpose::ArtifactManifest)?;
    assert_eq!(
        allow.authorize(&ctx0(), &raw_verify(&key, foreign.as_bytes())),
        Err(CryptoServiceError::Forbidden)
    );

    // The own purpose passes and reaches the inner authorizer.
    let own = transcript(SignPurpose::NodeHandshake)?;
    assert_eq!(
        allow.authorize(&ctx0(), &raw_verify(&key, own.as_bytes())),
        Ok(())
    );
    assert_eq!(
        marker.authorize(&ctx0(), &raw_verify(&key, own.as_bytes())),
        Err(CryptoServiceError::Conflict)
    );
    let harw_key = HarwKeyRef::latest(HarwKeyPurpose::NodeIdentity, node("node-a")?);
    let built = verify_operation(&harw_key, &own, &[1, 2, 3]).map_err(ctx("verify_operation"))?;
    assert_eq!(allow.authorize(&ctx0(), &built), Ok(()));
    Ok(())
}

#[test]
fn test_authorizer_leaves_foreign_namespace_payloads_to_inner() -> TestResult {
    let key = KeyRef::latest(
        KeyNamespace::new("team-a").map_err(ctx("ns"))?,
        KeyId::new("k").map_err(ctx("id"))?,
    );
    // Raw bytes on a non-harw namespace are not the Harw layer's business.
    let allow = HarwUsageAuthorizer::new(AllowAll);
    assert_eq!(allow.authorize(&ctx0(), &raw_sign(&key, b"raw")), Ok(()));
    assert_eq!(allow.authorize(&ctx0(), &raw_verify(&key, b"raw")), Ok(()));
    let marker = HarwUsageAuthorizer::new(InnerMarker);
    assert_eq!(
        marker.authorize(&ctx0(), &raw_sign(&key, b"raw")),
        Err(CryptoServiceError::Conflict)
    );
    assert_eq!(
        marker.authorize(&ctx0(), &raw_verify(&key, b"raw")),
        Err(CryptoServiceError::Conflict)
    );
    Ok(())
}

#[test]
fn test_kms_transcript_limit_matches_cgk1_framing() {
    // 64 KiB sign body = CGK1 magic+version (5) + message len (4)
    // + signature len (4) + ML-DSA-87 signature (4627) + transcript.
    assert_eq!(CG_SIGN_BODY_LIMIT, 64 * 1024);
    assert_eq!(MAX_KMS_SIGNATURE_LEN, 4627);
    assert_eq!(CGK1_VERIFY_OVERHEAD, 5 + 4 + 4 + 4627);
    assert_eq!(MAX_SIGNABLE_TRANSCRIPT_LEN, 60_896);
}

// The KMS limit is a strict tightening of the general transcript limit.
const _: () = assert!(MAX_SIGNABLE_TRANSCRIPT_LEN < MAX_TRANSCRIPT_LEN);

#[test]
fn test_oversize_transcript_is_rejected_for_kms_use() -> TestResult {
    let key = identity_key()?;
    let harw_key = HarwKeyRef::latest(HarwKeyPurpose::NodeIdentity, node("node-a")?);
    let auth = HarwUsageAuthorizer::new(AllowAll);

    // Exactly at the limit: accepted everywhere.
    let fits = transcript_of_len(SignPurpose::NodeHandshake, MAX_SIGNABLE_TRANSCRIPT_LEN)?;
    assert_eq!(fits.ensure_kms_size(), Ok(()));
    assert_eq!(
        SignTranscript::validate_for_kms(fits.as_bytes()),
        Ok(SignPurpose::NodeHandshake)
    );
    assert!(sign_operation(&harw_key, fits.clone()).is_ok());
    assert_eq!(
        auth.authorize(&ctx0(), &raw_sign(&key, fits.as_bytes())),
        Ok(())
    );

    // One byte over: well-formed in general, but not for the KMS.
    let over = transcript_of_len(SignPurpose::NodeHandshake, MAX_SIGNABLE_TRANSCRIPT_LEN + 1)?;
    assert_eq!(
        SignTranscript::validate(over.as_bytes()),
        Ok(SignPurpose::NodeHandshake)
    );
    assert_eq!(
        over.ensure_kms_size(),
        Err(EncryptError::TranscriptTooLarge)
    );
    assert_eq!(
        SignTranscript::validate_for_kms(over.as_bytes()),
        Err(EncryptError::TranscriptTooLarge)
    );
    assert_eq!(
        sign_operation(&harw_key, over.clone()).err(),
        Some(EncryptError::TranscriptTooLarge)
    );
    assert_eq!(
        verify_operation(&harw_key, &over, &[1]).err(),
        Some(EncryptError::TranscriptTooLarge)
    );
    assert_eq!(
        auth.authorize(&ctx0(), &raw_sign(&key, over.as_bytes())),
        Err(CryptoServiceError::Forbidden)
    );
    assert_eq!(
        auth.authorize(&ctx0(), &raw_verify(&key, over.as_bytes())),
        Err(CryptoServiceError::Forbidden)
    );
    Ok(())
}

#[test]
fn test_build_for_kms_enforces_the_kms_limit() -> TestResult {
    let small = SignTranscript::builder(SignPurpose::NodeHandshake)
        .field("x", b"y")
        .build_for_kms()
        .map_err(ctx("small kms transcript"))?;
    assert_eq!(
        Some(small),
        SignTranscript::builder(SignPurpose::NodeHandshake)
            .field("x", b"y")
            .build()
            .ok()
    );

    let big = vec![0u8; MAX_SIGNABLE_TRANSCRIPT_LEN];
    assert_eq!(
        SignTranscript::builder(SignPurpose::NodeHandshake)
            .field("pad", &big)
            .build_for_kms()
            .err(),
        Some(EncryptError::TranscriptTooLarge)
    );
    assert!(
        SignTranscript::builder(SignPurpose::NodeHandshake)
            .field("pad", &big)
            .build()
            .is_ok()
    );
    // Field errors still win over the size check.
    assert_eq!(
        SignTranscript::builder(SignPurpose::NodeHandshake)
            .field("BAD", b"")
            .build_for_kms()
            .err(),
        Some(EncryptError::InvalidTranscriptField)
    );
    Ok(())
}
