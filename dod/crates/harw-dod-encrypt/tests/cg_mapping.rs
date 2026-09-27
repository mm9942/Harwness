//! The CryptGuard boundary (`harw_dod_encrypt::cg`): key-ref naming, op
//! sets, namespace policy, request construction and the usage authorizer.

mod common;

use common::{TestError, TestResult, ctx};
use crypt_guard_service::{
    AllowAll, Authorizer, CiphertextBlob, CryptoContext, CryptoOperation, CryptoServiceError,
    DescribeKey, KeyAlgorithm, KeyId, KeyNamespace, KeyRef, OpKind, Principal as CgPrincipal,
    RequestContext, RequestId, RewrapKey, SignatureAlgorithm,
};
use harw_dod_encrypt::cg::{
    self, HarwUsageAuthorizer, allows, generate_operation, harw_key_ref, harw_op, key_ref,
    key_ref_of, namespace_policy_for, op_kind, op_set, role_op_set, sign_operation,
    verify_operation,
};
use harw_dod_encrypt::{
    EncryptError, GrantRole, HarwCryptoProfile, HarwGrant, HarwKeyOp, HarwKeyPurpose, HarwKeyRef,
    HarwKeyVersion, KeyUsagePolicy, NodeId, SignPurpose, SignTranscript,
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
