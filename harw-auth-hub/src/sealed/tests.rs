//! Tests of the sealed provider: persistence across restarts, rotation
//! compare-and-set, the lifecycle/error table (checked step by step against
//! CryptGuard's `InMemoryProvider`), ML-DSA, fail-closed loading, file
//! permissions, locking, rollback on persist failure and byte-format
//! compatibility with `InMemoryProvider`.
//!
//! # Cross-compatibility and its limit
//! `InMemoryProvider` cannot import a seed, so a ciphertext of *this*
//! provider cannot be opened by it under the same key. Instead the tests
//! seal `CGKC` frames with this module's framing and `info` binding to an
//! `InMemoryProvider` **public key** and let `InMemoryProvider` decrypt,
//! unwrap and rewrap them — which only succeeds if frame header and binding
//! are byte-identical. Frames produced by both providers are additionally
//! compared header by header, and `bind_info` is pinned to golden bytes.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crypt_guard_service::pq_hpke::{DEFAULT_SUITE, HpkeEnvelope, RecipientPublicKey};
use crypt_guard_service::{
    CiphertextBlob, CryptoContext, CryptoOperation, CryptoProvider, CryptoRequest, CryptoResponse,
    CryptoServiceError, Decrypt, DescribeKey, DestroyKey, DisableKey, EnableKey, Encrypt,
    GenerateKey, GetPublicKey, InMemoryProvider, KeyAlgorithm, KeyId, KeyMetadata, KeyNamespace,
    KeyRef, KeyState, KeyVersion, MessageBlob, PublicBlob, RequestId, RewrapKey, RotateKey,
    SecretBytes, Sign, SignatureAlgorithm, SignatureBlob, UnwrapKey, VerificationResult, Verify,
    WrapKey,
};
use zeroize::Zeroizing;

use super::format::{
    self, DOMAIN_LABEL, FILE_HEADER_LEN, FRAME_HEADER_LEN, Purpose, bind_info, decode_frame,
    encode_frame,
};
use super::provider::{FrameTarget, epoch_path, seal_frame};
use super::store::{KeyTable, Material, lock_path};
use super::{KEK_LEN, Kek, SealedProvider, SealedStoreError};
use crate::test_support::{TestError, TestResult, ctx};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const HPKE: KeyAlgorithm = KeyAlgorithm::Hpke {
    suite: DEFAULT_SUITE,
};

fn kek() -> Kek {
    Kek::from_bytes(Zeroizing::new([0x42; KEK_LEN]))
}

fn other_kek() -> Kek {
    Kek::from_bytes(Zeroizing::new([0x24; KEK_LEN]))
}

struct Fixture {
    dir: tempfile::TempDir,
    path: PathBuf,
}

fn fixture() -> TestResult<Fixture> {
    let dir = tempfile::tempdir().map_err(ctx("create tempdir"))?;
    let path = dir.path().join("keys.sealed");
    Ok(Fixture { dir, path })
}

fn ns(name: &str) -> TestResult<KeyNamespace> {
    KeyNamespace::new(name).map_err(ctx("namespace"))
}

fn kid(name: &str) -> TestResult<KeyId> {
    KeyId::new(name).map_err(ctx("key id"))
}

fn ver(version: u32) -> TestResult<KeyVersion> {
    KeyVersion::new(version).map_err(ctx("key version"))
}

fn latest(namespace: &str, id: &str) -> TestResult<KeyRef> {
    Ok(KeyRef::latest(ns(namespace)?, kid(id)?))
}

fn versioned(namespace: &str, id: &str, version: u32) -> TestResult<KeyRef> {
    Ok(KeyRef::versioned(ns(namespace)?, kid(id)?, ver(version)?))
}

fn context(info: &[u8], aad: &[u8]) -> CryptoContext {
    CryptoContext {
        info: Box::from(info),
        aad: Box::from(aad),
    }
}

fn ensure(condition: bool, what: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(TestError::Unexpected(what.to_owned()))
    }
}

fn run(
    provider: &mut impl CryptoProvider,
    operation: CryptoOperation,
) -> Result<CryptoResponse, CryptoServiceError> {
    provider.execute(CryptoRequest::new(RequestId(1), operation))
}

fn expect_err(
    result: Result<CryptoResponse, CryptoServiceError>,
    want: CryptoServiceError,
    what: &str,
) -> TestResult {
    match result {
        Err(got) if got == want => Ok(()),
        Err(got) => Err(TestError::Unexpected(format!(
            "{what}: expected {want}, got {got}"
        ))),
        Ok(response) => Err(TestError::Unexpected(format!(
            "{what}: expected {want}, got Ok({response:?})"
        ))),
    }
}

fn expect_store_err(
    result: Result<SealedProvider, SealedStoreError>,
    matches: impl FnOnce(&SealedStoreError) -> bool,
    what: &str,
) -> TestResult {
    match result {
        Err(error) if matches(&error) => Ok(()),
        Err(error) => Err(TestError::Unexpected(format!("{what}: got {error}"))),
        Ok(provider) => Err(TestError::Unexpected(format!(
            "{what}: unexpectedly opened {provider:?}"
        ))),
    }
}

fn unexpected(what: &str, response: &CryptoResponse) -> TestError {
    TestError::Unexpected(format!("{what}: unexpected response {response:?}"))
}

fn key_created(
    result: Result<CryptoResponse, CryptoServiceError>,
) -> TestResult<(KeyRef, PublicBlob)> {
    match result.map_err(ctx("key created"))? {
        CryptoResponse::KeyCreated {
            key,
            public: Some(public),
        } => Ok((key, public)),
        other => Err(unexpected("key created", &other)),
    }
}

fn ciphertext(result: Result<CryptoResponse, CryptoServiceError>) -> TestResult<CiphertextBlob> {
    match result.map_err(ctx("ciphertext"))? {
        CryptoResponse::Ciphertext(blob) => Ok(blob),
        other => Err(unexpected("ciphertext", &other)),
    }
}

fn plaintext(result: Result<CryptoResponse, CryptoServiceError>) -> TestResult<Vec<u8>> {
    match result.map_err(ctx("plaintext"))? {
        CryptoResponse::Plaintext(secret) => Ok(secret.as_ref().to_vec()),
        other => Err(unexpected("plaintext", &other)),
    }
}

fn metadata(result: Result<CryptoResponse, CryptoServiceError>) -> TestResult<KeyMetadata> {
    match result.map_err(ctx("metadata"))? {
        CryptoResponse::Metadata(metadata) => Ok(metadata),
        other => Err(unexpected("metadata", &other)),
    }
}

fn public_key(result: Result<CryptoResponse, CryptoServiceError>) -> TestResult<PublicBlob> {
    match result.map_err(ctx("public key"))? {
        CryptoResponse::PublicKey(public) => Ok(public),
        other => Err(unexpected("public key", &other)),
    }
}

fn signature(result: Result<CryptoResponse, CryptoServiceError>) -> TestResult<SignatureBlob> {
    match result.map_err(ctx("signature"))? {
        CryptoResponse::Signature(signature) => Ok(signature),
        other => Err(unexpected("signature", &other)),
    }
}

fn verification(
    result: Result<CryptoResponse, CryptoServiceError>,
) -> TestResult<VerificationResult> {
    match result.map_err(ctx("verification"))? {
        CryptoResponse::Verification(result) => Ok(result),
        other => Err(unexpected("verification", &other)),
    }
}

fn op_generate(namespace: &str, id: &str, algorithm: KeyAlgorithm) -> TestResult<CryptoOperation> {
    Ok(CryptoOperation::Generate(GenerateKey {
        namespace: ns(namespace)?,
        id: kid(id)?,
        algorithm,
    }))
}

fn op_encrypt(key: &KeyRef, data: &[u8], ctx: &CryptoContext) -> CryptoOperation {
    CryptoOperation::Encrypt(Encrypt {
        key: key.clone(),
        plaintext: SecretBytes::copy_from_slice(data),
        context: ctx.clone(),
    })
}

fn op_decrypt(key: &KeyRef, blob: &CiphertextBlob, ctx: &CryptoContext) -> CryptoOperation {
    CryptoOperation::Decrypt(Decrypt {
        key: key.clone(),
        ciphertext: blob.clone(),
        context: ctx.clone(),
    })
}

fn op_wrap(key: &KeyRef, material: &[u8], ctx: &CryptoContext) -> CryptoOperation {
    CryptoOperation::WrapKey(WrapKey {
        key: key.clone(),
        material: SecretBytes::copy_from_slice(material),
        context: ctx.clone(),
    })
}

fn op_unwrap(key: &KeyRef, blob: &CiphertextBlob, ctx: &CryptoContext) -> CryptoOperation {
    CryptoOperation::UnwrapKey(UnwrapKey {
        key: key.clone(),
        wrapped: blob.clone(),
        context: ctx.clone(),
    })
}

fn op_rewrap(
    from: &KeyRef,
    to: &KeyRef,
    blob: &CiphertextBlob,
    ctx: &CryptoContext,
) -> CryptoOperation {
    CryptoOperation::RewrapKey(RewrapKey {
        from: from.clone(),
        from_context: ctx.clone(),
        to: to.clone(),
        to_context: ctx.clone(),
        wrapped: blob.clone(),
    })
}

fn op_rotate(key: &KeyRef) -> CryptoOperation {
    CryptoOperation::Rotate(RotateKey { key: key.clone() })
}

fn op_disable(key: &KeyRef) -> CryptoOperation {
    CryptoOperation::Disable(DisableKey { key: key.clone() })
}

fn op_enable(key: &KeyRef) -> CryptoOperation {
    CryptoOperation::Enable(EnableKey { key: key.clone() })
}

fn op_destroy(key: &KeyRef) -> CryptoOperation {
    CryptoOperation::Destroy(DestroyKey { key: key.clone() })
}

fn op_describe(key: &KeyRef) -> CryptoOperation {
    CryptoOperation::Describe(DescribeKey { key: key.clone() })
}

fn op_public(key: &KeyRef) -> CryptoOperation {
    CryptoOperation::PublicKey(GetPublicKey { key: key.clone() })
}

fn op_sign(key: &KeyRef, message: &[u8]) -> CryptoOperation {
    CryptoOperation::Sign(Sign {
        key: key.clone(),
        message: SecretBytes::copy_from_slice(message),
    })
}

fn op_verify(key: &KeyRef, message: &[u8], signature: &SignatureBlob) -> CryptoOperation {
    CryptoOperation::Verify(Verify {
        key: key.clone(),
        message: MessageBlob::new(message.to_vec()),
        signature: signature.clone(),
    })
}

/// Key version from a `CGKC` frame header.
fn frame_version(blob: &CiphertextBlob) -> TestResult<u32> {
    decode_frame(blob.as_bytes())
        .map(|frame| frame.key_version.get())
        .map_err(ctx("decode frame"))
}

fn mode_of(path: &Path) -> TestResult<u32> {
    Ok(fs::metadata(path)
        .map_err(ctx("stat"))?
        .permissions()
        .mode()
        & 0o777)
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

#[test]
fn hpke_keys_survive_restart() -> TestResult {
    let f = fixture()?;
    let key = latest("app", "k1")?;
    let c = context(b"info", b"aad");

    let (created_ref, created_public, sealed, wrapped) = {
        let mut provider = SealedProvider::create(&f.path, &kek()).map_err(ctx("create"))?;
        let (created_ref, created_public) =
            key_created(run(&mut provider, op_generate("app", "k1", HPKE)?))?;
        let sealed = ciphertext(run(&mut provider, op_encrypt(&key, b"hello", &c)))?;
        let wrapped = ciphertext(run(&mut provider, op_wrap(&key, &[7u8; 32], &c)))?;
        (created_ref, created_public, sealed, wrapped)
    };
    ensure(
        created_ref == versioned("app", "k1", 1)?,
        "generate returns app/k1@1",
    )?;
    ensure(
        sealed.as_bytes().get(..FRAME_HEADER_LEN) == Some(&b"CGKC\x01\x01\x00\x00\x00\x01"[..]),
        "encrypt frame header",
    )?;

    // Restart: a new provider instance over the same file.
    let mut provider = SealedProvider::open(&f.path, &kek()).map_err(ctx("reopen"))?;
    ensure(
        plaintext(run(&mut provider, op_decrypt(&key, &sealed, &c)))? == b"hello",
        "decrypt after restart",
    )?;
    ensure(
        plaintext(run(&mut provider, op_unwrap(&key, &wrapped, &c)))? == [7u8; 32],
        "unwrap after restart",
    )?;
    let public = public_key(run(&mut provider, op_public(&key)))?;
    ensure(
        public == created_public,
        "public key is stable across restart",
    )?;
    let meta = metadata(run(&mut provider, op_describe(&key)))?;
    ensure(
        meta.state == KeyState::Enabled && meta.key == created_ref && meta.algorithm == HPKE,
        "describe after restart",
    )?;
    Ok(())
}

#[test]
fn rotate_is_compare_and_set_and_keeps_algorithm() -> TestResult {
    let f = fixture()?;
    let key = latest("app", "k")?;
    let c = context(b"", b"");
    let mut provider = SealedProvider::create(&f.path, &kek()).map_err(ctx("create"))?;
    key_created(run(&mut provider, op_generate("app", "k", HPKE)?))?;
    let v1_ciphertext = ciphertext(run(&mut provider, op_encrypt(&key, b"v1", &c)))?;

    let (rotated, _) = key_created(run(&mut provider, op_rotate(&versioned("app", "k", 1)?)))?;
    ensure(
        rotated == versioned("app", "k", 2)?,
        "CAS rotate on the primary → v2",
    )?;
    expect_err(
        run(&mut provider, op_rotate(&versioned("app", "k", 1)?)),
        CryptoServiceError::Conflict,
        "stale CAS rotate",
    )?;
    expect_err(
        run(&mut provider, op_rotate(&versioned("app", "k", 9)?)),
        CryptoServiceError::Conflict,
        "CAS rotate on an unknown version",
    )?;
    let (rotated, _) = key_created(run(&mut provider, op_rotate(&key)))?;
    ensure(
        rotated == versioned("app", "k", 3)?,
        "unconditional rotate → v3",
    )?;
    expect_err(
        run(&mut provider, op_rotate(&latest("app", "missing")?)),
        CryptoServiceError::NotFound,
        "rotate unknown key",
    )?;

    key_created(run(
        &mut provider,
        op_generate(
            "app",
            "sig",
            KeyAlgorithm::Signature(SignatureAlgorithm::MlDsa65),
        )?,
    ))?;
    key_created(run(&mut provider, op_rotate(&latest("app", "sig")?)))?;
    drop(provider);

    let mut provider = SealedProvider::open(&f.path, &kek()).map_err(ctx("reopen"))?;
    let meta = metadata(run(&mut provider, op_describe(&key)))?;
    ensure(
        meta.key == versioned("app", "k", 3)? && meta.algorithm == HPKE,
        "primary v3 and algorithm persisted",
    )?;
    let sig_meta = metadata(run(&mut provider, op_describe(&latest("app", "sig")?)))?;
    ensure(
        sig_meta.algorithm == KeyAlgorithm::Signature(SignatureAlgorithm::MlDsa65)
            && sig_meta.key.version == Some(ver(2)?),
        "rotation keeps the signature algorithm",
    )?;
    ensure(
        plaintext(run(&mut provider, op_decrypt(&key, &v1_ciphertext, &c)))? == b"v1",
        "old version still decrypts after rotation and restart",
    )?;
    let fresh = ciphertext(run(&mut provider, op_encrypt(&key, b"v3", &c)))?;
    ensure(frame_version(&fresh)? == 3, "encrypt uses the new primary")?;
    ensure(
        key_created(run(&mut provider, op_rotate(&versioned("app", "k", 3)?)))?.0
            == versioned("app", "k", 4)?,
        "CAS rotate after restart",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Lifecycle and error table, step by step against InMemoryProvider
// ---------------------------------------------------------------------------

/// Render a response without random bytes, so two providers can be compared.
fn outcome(result: Result<CryptoResponse, CryptoServiceError>) -> String {
    match result {
        Err(error) => format!("err:{}", error.name()),
        Ok(CryptoResponse::KeyCreated { key, public }) => {
            format!("created:{key}:{}", public.map_or(0, |p| p.len()))
        }
        Ok(CryptoResponse::Metadata(meta)) => format!("meta:{}:{:?}", meta.key, meta.state),
        Ok(CryptoResponse::PublicKey(public)) => format!("public:{}", public.len()),
        Ok(CryptoResponse::Ciphertext(blob)) => format!(
            "ciphertext:{:?}",
            decode_frame(blob.as_bytes()).map(|f| (f.purpose, f.key_version.get()))
        ),
        Ok(CryptoResponse::Plaintext(secret)) => format!("plaintext:{:?}", secret.as_ref()),
        Ok(other) => format!("other:{other:?}"),
    }
}

/// The shared scenario (HPKE only: this workspace builds `InMemoryProvider`
/// without the `ml-dsa` feature; no CAS rotate: `InMemoryProvider` ignores
/// `key.version` on rotate).
fn lifecycle_scenario(p: &mut impl CryptoProvider) -> TestResult<Vec<String>> {
    let c = context(b"info", b"aad");
    let wrong_aad = context(b"info", b"other");
    let k = latest("app", "k")?;
    let w = latest("app", "w")?;
    let w2 = latest("app", "w2")?;
    let missing = latest("app", "missing")?;
    let mut log = vec![
        outcome(run(p, op_generate("app", "k", HPKE)?)),
        outcome(run(p, op_generate("app", "k", HPKE)?)),
        outcome(run(p, op_generate("app", "w", HPKE)?)),
        outcome(run(p, op_generate("app", "w2", HPKE)?)),
    ];

    let ct1 = ciphertext(run(p, op_encrypt(&k, b"secret", &c)))?;
    log.push(outcome(Ok(CryptoResponse::Ciphertext(ct1.clone()))));
    log.push(outcome(run(p, op_decrypt(&k, &ct1, &c))));
    log.push(outcome(run(p, op_decrypt(&k, &ct1, &wrong_aad))));
    log.push(outcome(run(p, op_unwrap(&k, &ct1, &c))));
    log.push(outcome(run(
        p,
        op_decrypt(&versioned("app", "k", 2)?, &ct1, &c),
    )));
    log.push(outcome(run(
        p,
        op_decrypt(&k, &CiphertextBlob::new(b"garbage".to_vec()), &c),
    )));
    log.push(outcome(run(p, op_decrypt(&missing, &ct1, &c))));
    log.push(outcome(run(p, op_encrypt(&missing, b"x", &c))));
    log.push(outcome(run(p, op_describe(&missing))));
    log.push(outcome(run(p, op_disable(&missing))));
    log.push(outcome(run(p, op_rotate(&missing))));
    log.push(outcome(run(p, op_public(&missing))));
    log.push(outcome(run(p, op_sign(&k, b"m"))));

    // Wrap / rewrap / unwrap.
    let wrapped = ciphertext(run(p, op_wrap(&w, b"dek-bytes", &c)))?;
    log.push(outcome(run(p, op_decrypt(&w, &wrapped, &c))));
    let rewrapped = ciphertext(run(p, op_rewrap(&w, &w2, &wrapped, &c)))?;
    log.push(outcome(Ok(CryptoResponse::Ciphertext(rewrapped.clone()))));
    log.push(outcome(run(p, op_unwrap(&w2, &rewrapped, &c))));
    log.push(outcome(run(p, op_unwrap(&w, &rewrapped, &c))));

    // Disable / enable.
    log.push(outcome(run(p, op_disable(&k))));
    log.push(outcome(run(p, op_encrypt(&k, b"x", &c))));
    log.push(outcome(run(p, op_decrypt(&k, &ct1, &c))));
    log.push(outcome(run(p, op_public(&k))));
    log.push(outcome(run(p, op_describe(&k))));
    log.push(outcome(run(p, op_enable(&k))));
    log.push(outcome(run(p, op_decrypt(&k, &ct1, &c))));

    // Rotate, then disable/destroy the old version.
    log.push(outcome(run(p, op_rotate(&k))));
    let ct2 = ciphertext(run(p, op_encrypt(&k, b"second", &c)))?;
    log.push(outcome(Ok(CryptoResponse::Ciphertext(ct2.clone()))));
    log.push(outcome(run(p, op_decrypt(&k, &ct1, &c))));
    log.push(outcome(run(p, op_disable(&versioned("app", "k", 1)?))));
    log.push(outcome(run(p, op_decrypt(&k, &ct1, &c))));
    log.push(outcome(run(p, op_decrypt(&k, &ct2, &c))));
    log.push(outcome(run(p, op_destroy(&k))));
    log.push(outcome(run(p, op_destroy(&versioned("app", "k", 1)?))));
    log.push(outcome(run(p, op_destroy(&versioned("app", "k", 1)?))));
    log.push(outcome(run(p, op_enable(&versioned("app", "k", 1)?))));
    log.push(outcome(run(p, op_disable(&versioned("app", "k", 1)?))));
    log.push(outcome(run(p, op_decrypt(&k, &ct1, &c))));
    log.push(outcome(run(p, op_describe(&versioned("app", "k", 1)?))));
    log.push(outcome(run(p, op_destroy(&versioned("app", "k", 9)?))));
    log.push(outcome(run(p, op_describe(&versioned("app", "k", 9)?))));

    // Destroying the primary does not re-point it.
    log.push(outcome(run(p, op_destroy(&versioned("app", "k", 2)?))));
    log.push(outcome(run(p, op_encrypt(&k, b"x", &c))));
    log.push(outcome(run(p, op_public(&k))));
    log.push(outcome(run(p, op_describe(&k))));
    log.push(outcome(run(p, op_decrypt(&k, &ct2, &c))));
    Ok(log)
}

#[test]
fn lifecycle_and_error_table_match_in_memory_provider() -> TestResult {
    let mut reference = InMemoryProvider::new();
    let expected = lifecycle_scenario(&mut reference)?;

    let f = fixture()?;
    let mut sealed = SealedProvider::create(&f.path, &kek()).map_err(ctx("create"))?;
    let got = lifecycle_scenario(&mut sealed)?;

    for (step, (want, have)) in expected.iter().zip(&got).enumerate() {
        if want != have {
            return Err(TestError::Unexpected(format!(
                "step {step}: InMemoryProvider {want}, SealedProvider {have}"
            )));
        }
    }
    ensure(expected.len() == got.len(), "same number of steps")?;

    // Spot-check the documented table.
    for (step, want) in [
        (1, "err:conflict"),
        (6, "err:authentication_failed"),
        (7, "err:authentication_failed"),
        (8, "err:authentication_failed"),
        (9, "err:authentication_failed"),
        (10, "err:not_found"),
        (16, "err:unsupported"),
        (17, "err:authentication_failed"),
        (20, "err:authentication_failed"),
        (22, "err:conflict"),
        (23, "err:conflict"),
        (24, "err:conflict"),
    ] {
        let have = got.get(step).ok_or(TestError::Missing("scenario step"))?;
        ensure(
            have == want,
            &format!("step {step}: expected {want}, got {have}"),
        )?;
    }
    Ok(())
}

#[test]
fn lifecycle_state_survives_restart() -> TestResult {
    let f = fixture()?;
    {
        let mut provider = SealedProvider::create(&f.path, &kek()).map_err(ctx("create"))?;
        lifecycle_scenario(&mut provider)?;
    }
    let mut provider = SealedProvider::open(&f.path, &kek()).map_err(ctx("reopen"))?;
    for (key, state) in [
        (versioned("app", "k", 1)?, KeyState::Destroyed),
        (versioned("app", "k", 2)?, KeyState::Destroyed),
        (latest("app", "w")?, KeyState::Enabled),
    ] {
        let meta = metadata(run(&mut provider, op_describe(&key)))?;
        ensure(meta.state == state, &format!("{key}: {state:?} persisted"))?;
    }
    expect_err(
        run(
            &mut provider,
            op_encrypt(&latest("app", "k")?, b"x", &context(b"", b"")),
        ),
        CryptoServiceError::NotFound,
        "destroyed primary stays unusable after restart",
    )?;

    // Disabled state persists too.
    metadata(run(&mut provider, op_disable(&latest("app", "w")?)))?;
    drop(provider);
    let mut provider = SealedProvider::open(&f.path, &kek()).map_err(ctx("reopen 2"))?;
    let meta = metadata(run(&mut provider, op_describe(&latest("app", "w")?)))?;
    ensure(meta.state == KeyState::Disabled, "disabled persisted")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// ML-DSA
// ---------------------------------------------------------------------------

#[test]
fn ml_dsa_sign_verify_survives_restart() -> TestResult {
    let f = fixture()?;
    let algorithms = [
        ("s44", SignatureAlgorithm::MlDsa44),
        ("s65", SignatureAlgorithm::MlDsa65),
        ("s87", SignatureAlgorithm::MlDsa87),
    ];
    let mut before = Vec::new();
    {
        let mut provider = SealedProvider::create(&f.path, &kek()).map_err(ctx("create"))?;
        for (id, algorithm) in algorithms {
            let (_, public) = key_created(run(
                &mut provider,
                op_generate("sig", id, KeyAlgorithm::Signature(algorithm))?,
            ))?;
            let signed = signature(run(&mut provider, op_sign(&latest("sig", id)?, b"message")))?;
            before.push((id, public, signed));
        }
    }

    let mut provider = SealedProvider::open(&f.path, &kek()).map_err(ctx("reopen"))?;
    for (id, public, signed) in &before {
        let key = latest("sig", id)?;
        ensure(
            public_key(run(&mut provider, op_public(&key)))? == *public,
            "verifying key stable across restart",
        )?;
        ensure(
            verification(run(&mut provider, op_verify(&key, b"message", signed)))?
                == VerificationResult::Valid,
            "pre-restart signature verifies",
        )?;
        ensure(
            verification(run(&mut provider, op_verify(&key, b"other", signed)))?
                == VerificationResult::Invalid,
            "tampered message is Invalid",
        )?;
        ensure(
            verification(run(
                &mut provider,
                op_verify(&key, b"message", &SignatureBlob::new(vec![0xAA; 16])),
            ))? == VerificationResult::Invalid,
            "garbage signature is Invalid, not an error",
        )?;
        let fresh = signature(run(&mut provider, op_sign(&key, b"after")))?;
        ensure(
            verification(run(&mut provider, op_verify(&key, b"after", &fresh)))?
                == VerificationResult::Valid,
            "post-restart signature verifies",
        )?;
    }

    let key = latest("sig", "s65")?;
    let (_, _, signed) = before.get(1).ok_or(TestError::Missing("s65 signature"))?;
    expect_err(
        run(&mut provider, op_encrypt(&key, b"x", &context(b"", b""))),
        CryptoServiceError::Unsupported,
        "encrypt with a signing key",
    )?;
    metadata(run(&mut provider, op_disable(&key)))?;
    expect_err(
        run(&mut provider, op_verify(&key, b"message", signed)),
        CryptoServiceError::Conflict,
        "verify requires Enabled",
    )?;
    expect_err(
        run(&mut provider, op_sign(&key, b"message")),
        CryptoServiceError::Conflict,
        "sign requires Enabled",
    )?;
    expect_err(
        run(&mut provider, op_public(&key)),
        CryptoServiceError::Conflict,
        "public key requires Enabled",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Fail-closed loading
// ---------------------------------------------------------------------------

fn store_with_one_key(f: &Fixture) -> TestResult<Vec<u8>> {
    let mut provider = SealedProvider::create(&f.path, &kek()).map_err(ctx("create"))?;
    key_created(run(&mut provider, op_generate("app", "k", HPKE)?))?;
    drop(provider);
    fs::read(&f.path).map_err(ctx("read store"))
}

#[test]
fn tampered_store_file_fails_closed() -> TestResult {
    let f = fixture()?;
    let original = store_with_one_key(&f)?;
    ensure(original.len() > FILE_HEADER_LEN + 16, "store has a body")?;

    let flip = |index: usize| -> TestResult<Vec<u8>> {
        let mut bytes = original.clone();
        let byte = bytes
            .get_mut(index)
            .ok_or(TestError::Missing("byte to flip"))?;
        *byte ^= 0x01;
        Ok(bytes)
    };
    let last = original.len() - 1;
    let middle = FILE_HEADER_LEN + (original.len() - FILE_HEADER_LEN) / 2;

    let mut extended = original.clone();
    extended.push(0);
    let authentication_cases = [
        ("nonce byte", flip(10)?),
        ("body byte", flip(middle)?),
        ("tag byte", flip(last)?),
        (
            "truncated by one",
            original.get(..last).unwrap_or_default().to_vec(),
        ),
        ("extended by one", extended),
    ];
    for (what, bytes) in authentication_cases {
        fs::write(&f.path, bytes).map_err(ctx("write tampered store"))?;
        expect_store_err(
            SealedProvider::open(&f.path, &kek()),
            |e| matches!(e, SealedStoreError::Authentication),
            what,
        )?;
    }

    let format_cases = [
        ("magic byte", flip(0)?),
        ("format version", flip(5)?),
        ("aead id", flip(7)?),
        ("empty file", Vec::new()),
        (
            "header only",
            original.get(..FILE_HEADER_LEN).unwrap_or_default().to_vec(),
        ),
    ];
    for (what, bytes) in format_cases {
        fs::write(&f.path, bytes).map_err(ctx("write malformed store"))?;
        expect_store_err(
            SealedProvider::open(&f.path, &kek()),
            |e| matches!(e, SealedStoreError::UnsupportedFormat(_)),
            what,
        )?;
    }

    // The untouched file still opens.
    fs::write(&f.path, original).map_err(ctx("restore store"))?;
    SealedProvider::open(&f.path, &kek()).map_err(ctx("open restored store"))?;
    Ok(())
}

#[test]
fn wrong_kek_fails_closed() -> TestResult {
    let f = fixture()?;
    store_with_one_key(&f)?;
    expect_store_err(
        SealedProvider::open(&f.path, &other_kek()),
        |e| matches!(e, SealedStoreError::Authentication),
        "wrong KEK",
    )?;
    SealedProvider::open(&f.path, &kek()).map_err(ctx("right KEK still opens"))?;
    Ok(())
}

#[test]
fn authenticated_but_inconsistent_body_is_corrupt() -> TestResult {
    let mut table = KeyTable::default();
    table
        .insert_new(
            ns("app")?,
            kid("k")?,
            HPKE,
            Material::generate(HPKE).map_err(ctx("gen"))?,
        )
        .map_err(ctx("insert"))?;
    let body = format::encode_body(&table, 1).map_err(ctx("encode"))?;

    let mut with_trailing = body.to_vec();
    with_trailing.push(0);
    ensure(
        matches!(
            format::decode_body(&with_trailing),
            Err(SealedStoreError::Corrupt(_))
        ),
        "trailing bytes are Corrupt",
    )?;
    let truncated = body.get(..body.len() - 1).unwrap_or_default();
    ensure(
        matches!(
            format::decode_body(truncated),
            Err(SealedStoreError::Corrupt(_))
        ),
        "truncated body is Corrupt",
    )?;

    // A correctly sealed file with an inconsistent body is refused.
    let f = fixture()?;
    let file_key = kek().file_key();
    let sealed = format::seal_file(&file_key, &with_trailing).map_err(ctx("seal"))?;
    harw_fsutil::write_atomic(&f.path, &sealed, harw_fsutil::AtomicWriteOptions::private())
        .map_err(ctx("write"))?;
    expect_store_err(
        SealedProvider::open(&f.path, &kek()),
        |e| matches!(e, SealedStoreError::Corrupt(_)),
        "sealed inconsistent body",
    )?;
    Ok(())
}

#[test]
fn body_encoding_is_deterministic_and_round_trips() -> TestResult {
    let mut table = KeyTable::default();
    table
        .insert_new(
            ns("b")?,
            kid("hpke")?,
            HPKE,
            Material::generate(HPKE).map_err(ctx("gen"))?,
        )
        .map_err(ctx("insert hpke"))?;
    let signing = KeyAlgorithm::Signature(SignatureAlgorithm::MlDsa44);
    table
        .insert_new(
            ns("a")?,
            kid("sig")?,
            signing,
            Material::generate(signing).map_err(ctx("gen"))?,
        )
        .map_err(ctx("insert sig"))?;
    let hpke_ref = latest("b", "hpke")?;
    table
        .rotate(&hpke_ref, Material::generate(HPKE).map_err(ctx("gen"))?)
        .map_err(ctx("rotate"))?;
    table
        .destroy(&versioned("b", "hpke", 1)?)
        .map_err(ctx("destroy"))?;

    let first = format::encode_body(&table, 5).map_err(ctx("encode"))?;
    let (generation, decoded) = format::decode_body(&first).map_err(ctx("decode"))?;
    ensure(generation == 5, "generation round-trips")?;
    let second = format::encode_body(&decoded, 5).map_err(ctx("re-encode"))?;
    ensure(
        *first == *second,
        "encode → decode → encode is byte-identical",
    )?;
    ensure(
        first.len() == first.capacity(),
        "body buffer has exact capacity",
    )?;

    let meta = decoded.metadata(&hpke_ref).map_err(ctx("metadata"))?;
    ensure(meta.key.version == Some(ver(2)?), "primary round-trips")?;
    let tombstone = decoded
        .resolve(&versioned("b", "hpke", 1)?)
        .map_err(ctx("resolve tombstone"))?;
    ensure(
        tombstone.material.is_none() && tombstone.state == KeyState::Destroyed,
        "tombstone round-trips without material",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Files, permissions, locking
// ---------------------------------------------------------------------------

#[test]
fn store_and_lock_files_are_0600() -> TestResult {
    let f = fixture()?;
    let mut provider = SealedProvider::create(&f.path, &kek()).map_err(ctx("create"))?;
    ensure(mode_of(&f.path)? == 0o600, "new store is 0600")?;
    ensure(mode_of(&lock_path(&f.path))? == 0o600, "lock file is 0600")?;
    key_created(run(&mut provider, op_generate("app", "k", HPKE)?))?;
    ensure(mode_of(&f.path)? == 0o600, "rewritten store is 0600")?;
    ensure(provider.path() == f.path.as_path(), "path accessor")?;
    Ok(())
}

#[test]
fn too_wide_store_permissions_are_refused() -> TestResult {
    let f = fixture()?;
    store_with_one_key(&f)?;
    fs::set_permissions(&f.path, fs::Permissions::from_mode(0o644)).map_err(ctx("chmod"))?;
    expect_store_err(
        SealedProvider::open(&f.path, &kek()),
        |e| {
            matches!(e, SealedStoreError::Io { source, .. }
                if source.kind() == std::io::ErrorKind::PermissionDenied)
        },
        "0644 store",
    )?;
    Ok(())
}

#[test]
fn symlinked_store_is_refused() -> TestResult {
    let f = fixture()?;
    store_with_one_key(&f)?;
    let link = f.dir.path().join("link.sealed");
    std::os::unix::fs::symlink(&f.path, &link).map_err(ctx("symlink"))?;
    expect_store_err(
        SealedProvider::open(&link, &kek()),
        |e| matches!(e, SealedStoreError::Io { .. }),
        "symlinked store",
    )?;
    Ok(())
}

#[test]
fn second_instance_is_locked_out() -> TestResult {
    let f = fixture()?;
    let provider = SealedProvider::create(&f.path, &kek()).map_err(ctx("create"))?;
    expect_store_err(
        SealedProvider::open(&f.path, &kek()),
        |e| matches!(e, SealedStoreError::Locked(_)),
        "second open while locked",
    )?;
    drop(provider);
    SealedProvider::open(&f.path, &kek()).map_err(ctx("open after release"))?;
    Ok(())
}

#[test]
fn create_open_and_open_or_create() -> TestResult {
    let f = fixture()?;
    expect_store_err(
        SealedProvider::open(&f.path, &kek()),
        |e| matches!(e, SealedStoreError::NotFound(_)),
        "open missing store",
    )?;
    let mut provider = SealedProvider::open_or_create(&f.path, &kek()).map_err(ctx("create"))?;
    key_created(run(&mut provider, op_generate("app", "k", HPKE)?))?;
    drop(provider);
    expect_store_err(
        SealedProvider::create(&f.path, &kek()),
        |e| matches!(e, SealedStoreError::AlreadyExists(_)),
        "create over an existing store",
    )?;
    let mut provider = SealedProvider::open_or_create(&f.path, &kek()).map_err(ctx("reopen"))?;
    metadata(run(&mut provider, op_describe(&latest("app", "k")?)))?;
    Ok(())
}

#[test]
fn failed_persist_rolls_back_in_memory_state() -> TestResult {
    let f = fixture()?;
    let dir = f.dir.path().join("gone");
    fs::create_dir(&dir).map_err(ctx("mkdir"))?;
    let path = dir.join("keys.sealed");
    let mut provider = SealedProvider::create(&path, &kek()).map_err(ctx("create"))?;
    let k = latest("app", "k")?;
    let c = context(b"", b"");
    key_created(run(&mut provider, op_generate("app", "k", HPKE)?))?;

    // Every later write fails: the directory is gone.
    fs::remove_dir_all(dir).map_err(ctx("remove store dir"))?;

    expect_err(
        run(&mut provider, op_generate("app", "k2", HPKE)?),
        CryptoServiceError::Unavailable,
        "generate without persistence",
    )?;
    expect_err(
        run(&mut provider, op_describe(&latest("app", "k2")?)),
        CryptoServiceError::NotFound,
        "failed generate rolled back",
    )?;
    expect_err(
        run(&mut provider, op_rotate(&k)),
        CryptoServiceError::Unavailable,
        "rotate without persistence",
    )?;
    ensure(
        metadata(run(&mut provider, op_describe(&k)))?.key.version == Some(ver(1)?),
        "failed rotate rolled back",
    )?;
    expect_err(
        run(&mut provider, op_disable(&k)),
        CryptoServiceError::Unavailable,
        "disable without persistence",
    )?;
    ensure(
        metadata(run(&mut provider, op_describe(&k)))?.state == KeyState::Enabled,
        "failed disable rolled back",
    )?;
    expect_err(
        run(&mut provider, op_destroy(&versioned("app", "k", 1)?)),
        CryptoServiceError::Unavailable,
        "destroy without persistence",
    )?;
    let blob = ciphertext(run(&mut provider, op_encrypt(&k, b"still", &c)))?;
    ensure(
        plaintext(run(&mut provider, op_decrypt(&k, &blob, &c)))? == b"still",
        "failed destroy rolled back (material restored)",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// KEK sources
// ---------------------------------------------------------------------------

#[test]
fn kek_file_create_load_and_permissions() -> TestResult {
    let f = fixture()?;
    let kek_path = f.dir.path().join("kek");
    let created = Kek::create_key_file(&kek_path).map_err(ctx("create KEK file"))?;
    ensure(mode_of(&kek_path)? == 0o600, "KEK file is 0600")?;
    ensure(
        format!("{created:?}") == "Kek([REDACTED])",
        "KEK Debug is redacted",
    )?;
    {
        let mut provider =
            SealedProvider::create(&f.path, &created).map_err(ctx("create store"))?;
        key_created(run(&mut provider, op_generate("app", "k", HPKE)?))?;
    }
    let loaded = Kek::from_key_file(&kek_path).map_err(ctx("load KEK file"))?;
    SealedProvider::open(&f.path, &loaded).map_err(ctx("open with loaded KEK"))?;
    let credential = Kek::from_credentials_dir(f.dir.path(), "kek").map_err(ctx("credential"))?;
    SealedProvider::open(&f.path, &credential).map_err(ctx("open with credential KEK"))?;

    ensure(
        matches!(
            Kek::create_key_file(&kek_path),
            Err(SealedStoreError::Io { ref source, .. })
                if source.kind() == std::io::ErrorKind::AlreadyExists
        ),
        "create_key_file never overwrites",
    )?;

    fs::set_permissions(&kek_path, fs::Permissions::from_mode(0o640)).map_err(ctx("chmod"))?;
    ensure(
        matches!(
            Kek::from_key_file(&kek_path),
            Err(SealedStoreError::Io { .. })
        ),
        "0640 KEK file is refused",
    )?;

    for (name, len) in [("short", KEK_LEN - 1), ("long", KEK_LEN + 1), ("empty", 0)] {
        let path = f.dir.path().join(name);
        fs::write(&path, vec![1u8; len]).map_err(ctx("write KEK"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(ctx("chmod"))?;
        ensure(
            matches!(Kek::from_key_file(&path), Err(SealedStoreError::Kek(_))),
            &format!("{name} KEK file is refused"),
        )?;
    }

    for name in ["", ".", "..", "../kek", "a/b"] {
        ensure(
            matches!(
                Kek::from_credentials_dir(f.dir.path(), name),
                Err(SealedStoreError::Kek(_))
            ),
            &format!("credential name {name:?} is refused"),
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Byte-format compatibility with InMemoryProvider
// ---------------------------------------------------------------------------

#[test]
fn bind_info_and_frame_match_golden_bytes() -> TestResult {
    let info = bind_info(
        Purpose::Encrypt,
        &ns("a")?,
        &kid("b")?,
        ver(1)?,
        DEFAULT_SUITE,
        b"i",
    )
    .map_err(ctx("bind_info"))?;
    let mut expected = Vec::new();
    expected.extend_from_slice(&[0, 0, 0, 18]);
    expected.extend_from_slice(b"crypt_guard/kms/v1");
    expected.push(0x01);
    expected.extend_from_slice(&[0, 0, 0, 1, b'a']);
    expected.extend_from_slice(&[0, 0, 0, 1, b'b']);
    expected.extend_from_slice(&[0, 0, 0, 1]);
    // "HPKE" | ML-KEM-1024/P-384 0x0051 | SHAKE256 0x0011 | ChaCha20-Poly1305 0x0003
    expected.extend_from_slice(b"HPKE\x00\x51\x00\x11\x00\x03");
    expected.extend_from_slice(&[0, 0, 0, 1, b'i']);
    ensure(*info == expected, "bind_info golden bytes")?;
    ensure(DOMAIN_LABEL.len() == 18, "domain label length")?;

    let wrap = bind_info(
        Purpose::Wrap,
        &ns("a")?,
        &kid("b")?,
        ver(1)?,
        DEFAULT_SUITE,
        b"i",
    )
    .map_err(ctx("bind_info wrap"))?;
    ensure(wrap.get(22) == Some(&0x02), "purpose byte position")?;

    let frame = encode_frame(Purpose::Wrap, ver(0x0102_0304)?, b"xy");
    ensure(
        frame == b"CGKC\x01\x02\x01\x02\x03\x04xy",
        "frame golden bytes",
    )?;
    let decoded = decode_frame(&frame).map_err(ctx("decode frame"))?;
    ensure(
        decoded.purpose == Purpose::Wrap
            && decoded.key_version.get() == 0x0102_0304
            && decoded.envelope == b"xy",
        "frame round-trip",
    )?;
    let bad_frames: [&[u8]; 5] = [
        b"CGKC\x01\x01\x00\x00\x00",
        b"CGKX\x01\x01\x00\x00\x00\x01",
        b"CGKC\x02\x01\x00\x00\x00\x01",
        b"CGKC\x01\x03\x00\x00\x00\x01",
        b"CGKC\x01\x01\x00\x00\x00\x00",
    ];
    for bad in bad_frames {
        ensure(
            decode_frame(bad) == Err(CryptoServiceError::AuthenticationFailed),
            "malformed frame is AuthenticationFailed",
        )?;
    }
    Ok(())
}

#[test]
fn in_memory_provider_opens_frames_sealed_by_this_module() -> TestResult {
    let mut reference = InMemoryProvider::new();
    let key = latest("app", "k")?;
    let c = context(b"info", b"aad");
    key_created(run(&mut reference, op_generate("app", "k", HPKE)?))?;
    let public_v1 = public_key(run(&mut reference, op_public(&key)))?;
    let public_v1 = RecipientPublicKey::from_bytes(DEFAULT_SUITE.kem(), public_v1.as_bytes())
        .map_err(ctx("parse public key"))?;
    let (namespace, id) = (ns("app")?, kid("k")?);
    let target = |purpose, version| FrameTarget {
        purpose,
        namespace: &namespace,
        id: &id,
        version,
        suite: DEFAULT_SUITE,
    };

    // Encrypt purpose → InMemoryProvider Decrypt (latest and versioned).
    let sealed = seal_frame(&target(Purpose::Encrypt, ver(1)?), &public_v1, &c, b"cross")
        .map_err(ctx("seal encrypt"))?;
    ensure(
        plaintext(run(&mut reference, op_decrypt(&key, &sealed, &c)))? == b"cross",
        "InMemoryProvider decrypts our frame",
    )?;
    ensure(
        plaintext(run(
            &mut reference,
            op_decrypt(&versioned("app", "k", 1)?, &sealed, &c),
        ))? == b"cross",
        "InMemoryProvider decrypts our frame (versioned)",
    )?;

    // Wrap purpose → UnwrapKey and RewrapKey.
    let wrapped = seal_frame(&target(Purpose::Wrap, ver(1)?), &public_v1, &c, b"dek")
        .map_err(ctx("seal wrap"))?;
    ensure(
        plaintext(run(&mut reference, op_unwrap(&key, &wrapped, &c)))? == b"dek",
        "InMemoryProvider unwraps our frame",
    )?;
    let rewrapped = ciphertext(run(&mut reference, op_rewrap(&key, &key, &wrapped, &c)))?;
    ensure(
        plaintext(run(&mut reference, op_unwrap(&key, &rewrapped, &c)))? == b"dek",
        "InMemoryProvider rewraps our frame",
    )?;
    expect_err(
        run(&mut reference, op_decrypt(&key, &wrapped, &c)),
        CryptoServiceError::AuthenticationFailed,
        "purpose is bound",
    )?;

    // Version 2 after rotation.
    key_created(run(&mut reference, op_rotate(&key)))?;
    let public_v2 = public_key(run(&mut reference, op_public(&versioned("app", "k", 2)?)))?;
    let public_v2 = RecipientPublicKey::from_bytes(DEFAULT_SUITE.kem(), public_v2.as_bytes())
        .map_err(ctx("parse public key v2"))?;
    let sealed_v2 = seal_frame(&target(Purpose::Encrypt, ver(2)?), &public_v2, &c, b"two")
        .map_err(ctx("seal v2"))?;
    ensure(
        plaintext(run(&mut reference, op_decrypt(&key, &sealed_v2, &c)))? == b"two",
        "InMemoryProvider decrypts our v2 frame",
    )?;
    let wrong_version = seal_frame(&target(Purpose::Encrypt, ver(1)?), &public_v2, &c, b"two")
        .map_err(ctx("seal mislabelled"))?;
    expect_err(
        run(&mut reference, op_decrypt(&key, &wrong_version, &c)),
        CryptoServiceError::AuthenticationFailed,
        "version is bound",
    )?;
    Ok(())
}

#[test]
fn frames_of_both_providers_have_the_same_structure() -> TestResult {
    let f = fixture()?;
    let mut reference = InMemoryProvider::new();
    let mut sealed = SealedProvider::create(&f.path, &kek()).map_err(ctx("create"))?;
    let key = latest("app", "k")?;
    let c = context(b"info", b"aad");
    let providers: [&mut dyn CryptoProvider; 2] = [&mut reference, &mut sealed];
    for provider in providers {
        key_created(provider.execute(CryptoRequest::new(
            RequestId(1),
            op_generate("app", "k", HPKE)?,
        )))?;
    }

    type MakeOp = fn(&KeyRef, &[u8], &CryptoContext) -> CryptoOperation;
    let cases: [(Purpose, MakeOp); 2] = [(Purpose::Encrypt, op_encrypt), (Purpose::Wrap, op_wrap)];
    for (purpose, make) in cases {
        let theirs = ciphertext(run(&mut reference, make(&key, b"same plaintext", &c)))?;
        let ours = ciphertext(run(&mut sealed, make(&key, b"same plaintext", &c)))?;
        ensure(
            theirs.as_bytes().get(..FRAME_HEADER_LEN) == ours.as_bytes().get(..FRAME_HEADER_LEN),
            &format!("{purpose:?}: identical CGKC header"),
        )?;
        ensure(
            theirs.len() == ours.len(),
            &format!("{purpose:?}: identical length"),
        )?;
        let theirs_env = HpkeEnvelope::from_bytes(
            theirs
                .as_bytes()
                .get(FRAME_HEADER_LEN..)
                .unwrap_or_default(),
        )
        .map_err(ctx("parse InMemoryProvider envelope"))?;
        let ours_env =
            HpkeEnvelope::from_bytes(ours.as_bytes().get(FRAME_HEADER_LEN..).unwrap_or_default())
                .map_err(ctx("parse SealedProvider envelope"))?;
        ensure(
            theirs_env.suite() == ours_env.suite()
                && theirs_env.encapsulation().len() == ours_env.encapsulation().len()
                && theirs_env.ciphertext().len() == ours_env.ciphertext().len(),
            &format!("{purpose:?}: identical CGH3 envelope structure"),
        )?;
    }
    Ok(())
}

// ── Store-Epoch ──────────────────────────────────────────────────────────────

#[test]
fn store_epoch_is_stable_across_restart() -> TestResult {
    let f = fixture()?;
    let epoch = SealedProvider::create(&f.path, &kek())
        .map_err(ctx("create"))?
        .store_epoch()
        .to_owned();
    for round in 0..2 {
        let provider = SealedProvider::open(&f.path, &kek()).map_err(ctx("reopen"))?;
        ensure(
            provider.store_epoch() == epoch,
            if round == 0 {
                "epoch survives restart"
            } else {
                "epoch survives a second restart"
            },
        )?;
    }
    Ok(())
}

#[test]
fn new_store_gets_a_fresh_epoch() -> TestResult {
    let f1 = fixture()?;
    let f2 = fixture()?;
    let epoch1 = SealedProvider::create(&f1.path, &kek())
        .map_err(ctx("create 1"))?
        .store_epoch()
        .to_owned();
    let epoch2 = SealedProvider::create(&f2.path, &kek())
        .map_err(ctx("create 2"))?
        .store_epoch()
        .to_owned();
    ensure(
        epoch1 != epoch2,
        "each newly created store gets its own epoch",
    )
}

#[test]
fn missing_epoch_file_is_corrupt() -> TestResult {
    let f = fixture()?;
    drop(SealedProvider::create(&f.path, &kek()).map_err(ctx("create"))?);
    fs::remove_file(epoch_path(&f.path)).map_err(ctx("remove epoch file"))?;
    expect_store_err(
        SealedProvider::open(&f.path, &kek()),
        |e| matches!(e, SealedStoreError::Corrupt(_)),
        "missing epoch file",
    )
}

#[test]
fn too_wide_epoch_permissions_are_refused() -> TestResult {
    let f = fixture()?;
    drop(SealedProvider::create(&f.path, &kek()).map_err(ctx("create"))?);
    fs::set_permissions(epoch_path(&f.path), fs::Permissions::from_mode(0o644))
        .map_err(ctx("chmod"))?;
    expect_store_err(
        SealedProvider::open(&f.path, &kek()),
        |e| !matches!(e, SealedStoreError::NotFound(..)),
        "0644 epoch file",
    )
}
