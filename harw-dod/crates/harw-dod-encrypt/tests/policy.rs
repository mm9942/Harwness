//! Key-usage policy matrix (§5), grant roles, profiles and purpose codes.

mod common;

use common::{TestResult, ctx};
use harw_dod_encrypt::{
    EncryptError, GrantRole, HarwCryptoProfile, HarwKeyOp, HarwKeyPurpose, KeyClass,
    KeyUsagePolicy, NodeId, ProfileKind, SignPurpose, SignTranscript, UnixMillis,
};

use harw_dod_encrypt::HarwKeyOp as Op;

/// The full expected matrix, written out by class so a change to the
/// policy has to change this table too.
fn expected(purpose: HarwKeyPurpose, op: HarwKeyOp) -> bool {
    let lifecycle_or_describe = matches!(
        op,
        Op::Generate | Op::Rotate | Op::Disable | Op::Enable | Op::Destroy | Op::Describe
    );
    lifecycle_or_describe
        || match purpose.class() {
            KeyClass::Signing => matches!(op, Op::PublicKey | Op::Sign | Op::Verify),
            KeyClass::Encryption => matches!(op, Op::PublicKey | Op::Encrypt | Op::Decrypt),
            KeyClass::Wrapping => {
                matches!(
                    op,
                    Op::PublicKey | Op::WrapKey | Op::UnwrapKey | Op::RewrapKey
                )
            }
            KeyClass::Pseudonymization => false,
        }
}

#[test]
fn test_policy_matrix_matches_the_table() {
    for purpose in HarwKeyPurpose::ALL {
        for op in HarwKeyOp::ALL {
            assert_eq!(
                KeyUsagePolicy::allows(purpose, op),
                expected(purpose, op),
                "{purpose:?} x {op:?}"
            );
        }
    }
}

#[test]
fn test_policy_named_invariants() {
    // Wrap keys never sign and never encrypt arbitrary data.
    assert!(!KeyUsagePolicy::allows(
        HarwKeyPurpose::SecretsKek,
        Op::Sign
    ));
    assert!(!KeyUsagePolicy::allows(
        HarwKeyPurpose::SecretsKek,
        Op::Encrypt
    ));
    assert!(!KeyUsagePolicy::allows(
        HarwKeyPurpose::SecretsKek,
        Op::Decrypt
    ));
    // Identity keys never decrypt or unwrap (no secret egress).
    for purpose in [
        HarwKeyPurpose::NodeIdentity,
        HarwKeyPurpose::ServiceIdentity,
        HarwKeyPurpose::DeviceIdentity,
        HarwKeyPurpose::UserIdentity,
    ] {
        assert!(!KeyUsagePolicy::allows(purpose, Op::Decrypt));
        assert!(!KeyUsagePolicy::allows(purpose, Op::UnwrapKey));
        assert!(!KeyUsagePolicy::allows(purpose, Op::WrapKey));
    }
    // Channel keys never sign or wrap.
    assert!(!KeyUsagePolicy::allows(
        HarwKeyPurpose::ChannelBinding,
        Op::Sign
    ));
    assert!(!KeyUsagePolicy::allows(
        HarwKeyPurpose::ChannelBinding,
        Op::WrapKey
    ));
    // Pseudonymization has no public key.
    assert!(!KeyUsagePolicy::allows(
        HarwKeyPurpose::Pseudonymization,
        Op::PublicKey
    ));
    assert_eq!(
        KeyUsagePolicy::authorize(HarwKeyPurpose::SecretsKek, Op::Sign),
        Err(EncryptError::OperationNotAllowed)
    );
}

#[test]
fn test_sign_purpose_mapping_is_one_to_one() {
    for sign in SignPurpose::ALL {
        assert_eq!(sign.key_purpose().sign_purpose(), Some(sign));
    }
    for purpose in HarwKeyPurpose::ALL {
        let signs = purpose.sign_purpose().is_some();
        assert_eq!(signs, purpose.class() == KeyClass::Signing, "{purpose:?}");
        assert_eq!(
            signs,
            KeyUsagePolicy::allows(purpose, Op::Sign),
            "{purpose:?}"
        );
    }
}

#[test]
fn test_signing_key_only_signs_its_own_transcripts() -> TestResult {
    for sign in SignPurpose::ALL {
        let t = SignTranscript::builder(sign)
            .field("x", b"y")
            .build()
            .map_err(ctx("build"))?;
        for purpose in HarwKeyPurpose::ALL {
            let got = KeyUsagePolicy::authorize_sign(purpose, &t);
            let want = if purpose.sign_purpose().is_none() {
                Err(EncryptError::OperationNotAllowed)
            } else if purpose == sign.key_purpose() {
                Ok(())
            } else {
                Err(EncryptError::TranscriptPurposeMismatch)
            };
            assert_eq!(got, want, "{purpose:?} signing {sign:?}");
        }
    }
    Ok(())
}

#[test]
fn test_node_identity_is_not_a_document_signer() -> TestResult {
    let a = NodeId::new("node-a").map_err(ctx("a"))?;
    let b = NodeId::new("node-b").map_err(ctx("b"))?;
    let handshake =
        SignTranscript::node_handshake(&a, &[0; 32], &b, UnixMillis(0)).map_err(ctx("hs"))?;
    let manifest = SignTranscript::builder(SignPurpose::ArtifactManifest)
        .field("digest", &[0; 32])
        .build()
        .map_err(ctx("manifest"))?;
    KeyUsagePolicy::authorize_sign(HarwKeyPurpose::NodeIdentity, &handshake)
        .map_err(ctx("own handshake"))?;
    assert_eq!(
        KeyUsagePolicy::authorize_sign(HarwKeyPurpose::NodeIdentity, &manifest),
        Err(EncryptError::TranscriptPurposeMismatch)
    );
    Ok(())
}

#[test]
fn test_grant_roles_never_exceed_the_policy() {
    for purpose in HarwKeyPurpose::ALL {
        for role in [GrantRole::Owner, GrantRole::Peer, GrantRole::Admin] {
            for op in role.ops_for(purpose) {
                assert!(
                    KeyUsagePolicy::allows(purpose, op),
                    "{role:?} {purpose:?} {op:?}"
                );
                assert!(role.permits(op));
            }
        }
    }
}

#[test]
fn test_grant_role_separation() {
    for purpose in HarwKeyPurpose::ALL {
        let peer = GrantRole::Peer.ops_for(purpose);
        let admin = GrantRole::Admin.ops_for(purpose);
        let owner = GrantRole::Owner.ops_for(purpose);
        // Peers never sign and never receive secret egress.
        for op in [Op::Sign, Op::Decrypt, Op::UnwrapKey, Op::RewrapKey] {
            assert!(!peer.contains(&op), "peer {purpose:?} {op:?}");
        }
        // Admins never use a key.
        for op in [
            Op::Sign,
            Op::Encrypt,
            Op::Decrypt,
            Op::WrapKey,
            Op::UnwrapKey,
        ] {
            assert!(!admin.contains(&op), "admin {purpose:?} {op:?}");
        }
        // Owners never run lifecycle.
        assert!(
            owner.iter().all(|op| !op.is_lifecycle()),
            "owner {purpose:?}"
        );
    }
    assert!(
        GrantRole::Owner
            .ops_for(HarwKeyPurpose::NodeIdentity)
            .contains(&Op::Sign)
    );
    assert!(
        GrantRole::Owner
            .ops_for(HarwKeyPurpose::SecretsKek)
            .contains(&Op::UnwrapKey)
    );
    assert!(
        GrantRole::Peer
            .ops_for(HarwKeyPurpose::ChannelBinding)
            .contains(&Op::Encrypt)
    );
}

#[test]
fn test_purpose_codes_and_namespaces_round_trip() {
    let mut codes = Vec::new();
    for purpose in HarwKeyPurpose::ALL {
        assert_eq!(HarwKeyPurpose::from_code(purpose.code()), Some(purpose));
        assert_eq!(
            HarwKeyPurpose::from_namespace(purpose.namespace()),
            Some(purpose)
        );
        assert_eq!(purpose.namespace(), format!("harw.{}", purpose.label()));
        assert!(!codes.contains(&purpose.code()));
        codes.push(purpose.code());
    }
    assert_eq!(HarwKeyPurpose::from_code(0), None);
    assert_eq!(HarwKeyPurpose::from_namespace("harw.unknown"), None);
    for sign in SignPurpose::ALL {
        assert_eq!(SignPurpose::from_code(sign.code()), Some(sign));
        assert!(sign.domain().starts_with("harw:") && sign.domain().ends_with(":v1"));
    }
}

#[test]
fn test_profiles_fit_their_purposes() {
    for profile in HarwCryptoProfile::ALL {
        assert_eq!(HarwCryptoProfile::from_id(profile.id()), Some(profile));
    }
    assert_eq!(HarwCryptoProfile::from_id(0), None);
    for purpose in HarwKeyPurpose::ALL {
        match HarwCryptoProfile::default_for(purpose) {
            Some(profile) => assert!(profile.supports(purpose), "{purpose:?}"),
            None => assert_eq!(purpose.class(), KeyClass::Pseudonymization),
        }
    }
    assert!(!HarwCryptoProfile::MlDsa65.supports(HarwKeyPurpose::SecretsKek));
    assert_eq!(
        HarwCryptoProfile::HpkeMlKem1024P384Shake256ChaCha20Poly1305.kind(),
        ProfileKind::Hpke
    );
    assert!(
        !HarwCryptoProfile::HpkeMlKem1024P384Shake256ChaCha20Poly1305
            .supports(HarwKeyPurpose::NodeIdentity)
    );
}

#[test]
fn test_node_id_grammar() {
    let longest = "x".repeat(64);
    let too_long = "x".repeat(65);
    for good in ["a", "node-a", "node_a.b", "A9", longest.as_str()] {
        assert!(NodeId::new(good).is_ok(), "{good:?}");
    }
    for bad in ["", ".hidden", "a/b", "a b", "ä", too_long.as_str()] {
        assert_eq!(
            NodeId::new(bad).err(),
            Some(EncryptError::InvalidName),
            "{bad:?}"
        );
    }
}
