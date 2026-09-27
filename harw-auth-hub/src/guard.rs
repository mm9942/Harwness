//! Version guards for key lifecycle and wrap operations.
//!
//! CryptGuard's `InMemoryProvider` has three behaviours that break the
//! Harwness rule "CryptGuard key version = Harwness key generation + 1"
//! (`harw-infra-client` `AuthHubDekWrapper`):
//!
//! - `rotate` ignores the version in the request and always creates
//!   `highest + 1`. A client that retries a rotate after a lost response
//!   silently skips a version.
//! - Old versions stay `Enabled` after a rotation, and `encrypt`/`wrap`
//!   accept any enabled pinned version, so a process still running on a
//!   stale generation keeps wrapping *new* DEKs under the old key.
//! - `generate` of an existing key is a bare `Conflict`, so a retried
//!   generate after a lost response cannot tell "mine" from "someone
//!   else's".
//!
//! [`KmsGuardProvider`] wraps a provider and closes these gaps. It relies on
//! `CryptoProvider::execute(&mut self)` being serialized (the hub runs the
//! provider stack behind one buffer worker), so "describe, then act" inside
//! one `execute` is atomic with respect to every other request.
//!
//! # Rules
//!
//! | Operation | Key version in the request | Outcome |
//! |---|---|---|
//! | `rotate` | `Some(v)`, `v` == current primary | forwarded: creates `v + 1` |
//! | `rotate` | `Some(v)`, `v` != current primary | `Conflict`, nothing created |
//! | `rotate` | `None` | forwarded unchecked (legacy "rotate latest"; not retry-safe) |
//! | `encrypt`, `wrap`, `rewrap` target | `None` or current primary | forwarded |
//! | `encrypt`, `wrap`, `rewrap` target | older version | `Conflict` |
//! | `decrypt`, `unwrap`, `rewrap` source | any enabled version | forwarded |
//! | `generate` | — | see below |
//!
//! Rotate is a compare-and-set: the pinned version is the version the
//! caller *expects to be current*. A retry of a rotate that already
//! committed therefore gets `Conflict` (the primary is now `v + 1`) instead
//! of creating `v + 2`; the caller must describe the key to learn the
//! outcome. `harw-infra-client` always pins the expected version.
//!
//! # Generate idempotency
//!
//! When the inner `generate` fails with `Conflict`, the guard describes the
//! key. If the key's current primary is still version 1, enabled, and has
//! the requested algorithm, the result is exactly what the original
//! `generate` returned (`KeyCreated { key: ns/id@1, public }`) and is
//! returned again, synthesized from `describe` + `public_key`. Otherwise
//! (different algorithm, key already rotated, version 1 disabled or
//! destroyed) the `Conflict` stands. Note that idempotency is by
//! `(namespace, id, algorithm)`: a second caller that generates the same key
//! with the same algorithm also gets the existing key. The policy layer has
//! already authorized that caller to generate in this namespace.
//!
//! No mutation is ever retried: the guard issues only read operations
//! (`describe`, `public_key`) in addition to the forwarded request.
//!
//! # Placement
//!
//! Put the guard *inside* the policy layer, directly around the key store:
//! `AuditProvider<PolicyProvider<KmsGuardProvider<InMemoryProvider>, _>>`.
//! Its internal `describe`/`public_key` calls then run on behalf of an
//! operation the policy has already authorized and are not policy-checked a
//! second time (a principal with a `wrap` grant but no `describe` grant must
//! still be able to wrap), and they are not audited as separate operations.

use core::task::{Context, Poll};

use crypt_guard_service::{
    CryptoOperation, CryptoProvider, CryptoRequest, CryptoResponse, CryptoServiceError,
    DescribeKey, GetPublicKey, KeyAlgorithm, KeyId, KeyMetadata, KeyNamespace, KeyRef, KeyState,
    KeyVersion, RequestContext,
};

/// A provider wrapper that enforces rotate compare-and-set, primary-only
/// wrapping and idempotent generate (see the module docs).
#[derive(Debug)]
pub struct KmsGuardProvider<P> {
    inner: P,
}

impl<P> KmsGuardProvider<P> {
    /// Guard every operation executed by `inner`.
    pub fn new(inner: P) -> Self {
        Self { inner }
    }

    /// Borrow the wrapped provider.
    pub fn inner(&self) -> &P {
        &self.inner
    }
}

impl<P: CryptoProvider> CryptoProvider for KmsGuardProvider<P> {
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), CryptoServiceError>> {
        self.inner.poll_ready(cx)
    }

    fn execute(&mut self, request: CryptoRequest) -> Result<CryptoResponse, CryptoServiceError> {
        self.check(&request)?;
        let replay = GenerateReplay::of(&request);
        let result = self.inner.execute(request);
        match (result, replay) {
            (Err(CryptoServiceError::Conflict), Some(replay)) => self.replay_generate(replay),
            (result, _) => result,
        }
    }
}

impl<P: CryptoProvider> KmsGuardProvider<P> {
    /// Version preconditions of `request` (rotate CAS, primary-only seal).
    fn check(&mut self, request: &CryptoRequest) -> Result<(), CryptoServiceError> {
        let must_be_primary = match &request.operation {
            CryptoOperation::Rotate(op) => Some(&op.key),
            CryptoOperation::Encrypt(op) => Some(&op.key),
            CryptoOperation::WrapKey(op) => Some(&op.key),
            CryptoOperation::RewrapKey(op) => Some(&op.to),
            _ => None,
        };
        match must_be_primary {
            Some(key) => self.require_primary(&request.context, key),
            None => Ok(()),
        }
    }

    /// `Ok` if `key` is unpinned or pinned to the current primary version;
    /// `Conflict` for any other version; the inner error (e.g. `NotFound`)
    /// if the key cannot be described.
    fn require_primary(
        &mut self,
        context: &RequestContext,
        key: &KeyRef,
    ) -> Result<(), CryptoServiceError> {
        let Some(pinned) = key.version else {
            return Ok(());
        };
        let metadata = self.describe_latest(context, &key.namespace, &key.id)?;
        let primary = metadata.key.version.ok_or(CryptoServiceError::Internal)?;
        if primary == pinned {
            Ok(())
        } else {
            Err(CryptoServiceError::Conflict)
        }
    }

    /// Metadata of the current primary version of `namespace/id`.
    fn describe_latest(
        &mut self,
        context: &RequestContext,
        namespace: &KeyNamespace,
        id: &KeyId,
    ) -> Result<KeyMetadata, CryptoServiceError> {
        let request = CryptoRequest::with_context(
            context.clone(),
            CryptoOperation::Describe(DescribeKey {
                key: KeyRef::latest(namespace.clone(), id.clone()),
            }),
        );
        match self.inner.execute(request)? {
            CryptoResponse::Metadata(metadata) => Ok(metadata),
            _ => Err(CryptoServiceError::Internal),
        }
    }

    /// Turn a `generate` conflict into the original `KeyCreated` if the
    /// existing key is exactly what that generate created; otherwise keep
    /// the `Conflict`. Only read operations are issued.
    fn replay_generate(
        &mut self,
        replay: GenerateReplay,
    ) -> Result<CryptoResponse, CryptoServiceError> {
        let Ok(metadata) = self.describe_latest(&replay.context, &replay.namespace, &replay.id)
        else {
            return Err(CryptoServiceError::Conflict);
        };
        let unchanged = metadata.algorithm == replay.algorithm
            && metadata.key.version == Some(KeyVersion::FIRST)
            && matches!(metadata.state, KeyState::Enabled);
        if !unchanged {
            return Err(CryptoServiceError::Conflict);
        }
        let key = KeyRef::versioned(replay.namespace, replay.id, KeyVersion::FIRST);
        let request = CryptoRequest::with_context(
            replay.context,
            CryptoOperation::PublicKey(GetPublicKey { key: key.clone() }),
        );
        match self.inner.execute(request) {
            Ok(CryptoResponse::PublicKey(public)) => Ok(CryptoResponse::KeyCreated {
                key,
                public: Some(public),
            }),
            _ => Err(CryptoServiceError::Conflict),
        }
    }
}

/// What a `generate` asked for, kept to recognize a replayed request.
struct GenerateReplay {
    context: RequestContext,
    namespace: KeyNamespace,
    id: KeyId,
    algorithm: KeyAlgorithm,
}

impl GenerateReplay {
    fn of(request: &CryptoRequest) -> Option<Self> {
        match &request.operation {
            CryptoOperation::Generate(op) => Some(Self {
                context: request.context.clone(),
                namespace: op.namespace.clone(),
                id: op.id.clone(),
                algorithm: op.algorithm,
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crypt_guard_service::{
        CiphertextBlob, CryptoContext, CryptoOperation, CryptoProvider, CryptoRequest,
        CryptoResponse, CryptoServiceError, DescribeKey, GenerateKey, InMemoryProvider,
        KeyAlgorithm, KeyId, KeyMetadata, KeyNamespace, KeyRef, KeyState, KeyVersion, RequestId,
        RewrapKey, RotateKey, SecretBytes, SignatureAlgorithm, UnwrapKey, WrapKey, pq_hpke,
    };

    use super::KmsGuardProvider;
    use crate::test_support::{TestError, TestResult, ctx};

    type Guard = KmsGuardProvider<InMemoryProvider>;

    const DEK: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn hpke() -> KeyAlgorithm {
        KeyAlgorithm::Hpke {
            suite: pq_hpke::DEFAULT_SUITE,
        }
    }

    fn names() -> TestResult<(KeyNamespace, KeyId)> {
        Ok((
            KeyNamespace::new("secrets").map_err(ctx("namespace"))?,
            KeyId::new("dek-kek").map_err(ctx("key id"))?,
        ))
    }

    fn key(version: Option<u32>) -> TestResult<KeyRef> {
        let (namespace, id) = names()?;
        match version {
            None => Ok(KeyRef::latest(namespace, id)),
            Some(v) => Ok(KeyRef::versioned(
                namespace,
                id,
                KeyVersion::new(v).map_err(ctx("version"))?,
            )),
        }
    }

    fn run(
        guard: &mut impl CryptoProvider,
        operation: CryptoOperation,
    ) -> Result<CryptoResponse, CryptoServiceError> {
        guard.execute(CryptoRequest::new(RequestId(7), operation))
    }

    fn generate_op() -> TestResult<CryptoOperation> {
        let (namespace, id) = names()?;
        Ok(CryptoOperation::Generate(GenerateKey {
            namespace,
            id,
            algorithm: hpke(),
        }))
    }

    fn created_version(
        result: Result<CryptoResponse, CryptoServiceError>,
    ) -> TestResult<(u32, bool)> {
        match result {
            Ok(CryptoResponse::KeyCreated { key, public }) => Ok((
                key.version
                    .ok_or(TestError::Missing("created version"))?
                    .get(),
                public.is_some(),
            )),
            other => Err(TestError::Unexpected(format!(
                "expected KeyCreated, got {other:?}"
            ))),
        }
    }

    fn expect_conflict<T: core::fmt::Debug>(
        result: Result<T, CryptoServiceError>,
        what: &str,
    ) -> TestResult {
        match result {
            Err(CryptoServiceError::Conflict) => Ok(()),
            other => Err(TestError::Unexpected(format!(
                "{what}: expected Conflict, got {other:?}"
            ))),
        }
    }

    /// A guard over a fresh store holding `secrets/dek-kek@1`.
    fn guard_with_key() -> TestResult<Guard> {
        let mut guard = KmsGuardProvider::new(InMemoryProvider::new());
        let (version, _) = created_version(run(&mut guard, generate_op()?))?;
        assert_eq!(version, 1);
        Ok(guard)
    }

    fn rotate(
        guard: &mut Guard,
        expected: Option<u32>,
    ) -> TestResult<Result<CryptoResponse, CryptoServiceError>> {
        Ok(run(
            guard,
            CryptoOperation::Rotate(RotateKey {
                key: key(expected)?,
            }),
        ))
    }

    fn primary(guard: &mut Guard) -> TestResult<u32> {
        match run(
            guard,
            CryptoOperation::Describe(DescribeKey { key: key(None)? }),
        ) {
            Ok(CryptoResponse::Metadata(metadata)) => Ok(metadata
                .key
                .version
                .ok_or(TestError::Missing("primary version"))?
                .get()),
            other => Err(TestError::Unexpected(format!(
                "expected Metadata, got {other:?}"
            ))),
        }
    }

    /// The highest existing version (probes `describe` upwards).
    fn highest(guard: &mut Guard) -> TestResult<u32> {
        let mut highest = 0;
        for v in 1..=8 {
            let op = CryptoOperation::Describe(DescribeKey { key: key(Some(v))? });
            if run(guard, op).is_ok() {
                highest = v;
            }
        }
        Ok(highest)
    }

    fn wrap(
        guard: &mut Guard,
        version: Option<u32>,
    ) -> TestResult<Result<CiphertextBlob, CryptoServiceError>> {
        let op = CryptoOperation::WrapKey(WrapKey {
            key: key(version)?,
            material: SecretBytes::copy_from_slice(DEK),
            context: CryptoContext::default(),
        });
        Ok(match run(guard, op) {
            Ok(CryptoResponse::Ciphertext(blob)) => Ok(blob),
            Ok(_) => Err(CryptoServiceError::Internal),
            Err(error) => Err(error),
        })
    }

    fn unwrap_ok(guard: &mut Guard, version: Option<u32>, wrapped: CiphertextBlob) -> TestResult {
        let op = CryptoOperation::UnwrapKey(UnwrapKey {
            key: key(version)?,
            wrapped,
            context: CryptoContext::default(),
        });
        match run(guard, op) {
            Ok(CryptoResponse::Plaintext(plain)) => {
                assert_eq!(plain.as_ref(), DEK);
                Ok(())
            }
            other => Err(TestError::Unexpected(format!(
                "expected Plaintext, got {other:?}"
            ))),
        }
    }

    #[test]
    fn rotate_with_current_version_creates_next() -> TestResult {
        let mut guard = guard_with_key()?;
        let (version, has_public) = created_version(rotate(&mut guard, Some(1))?)?;
        assert_eq!(version, 2);
        assert!(has_public);
        assert_eq!(primary(&mut guard)?, 2);
        Ok(())
    }

    #[test]
    fn rotate_with_stale_version_conflicts_and_creates_nothing() -> TestResult {
        let mut guard = guard_with_key()?;
        created_version(rotate(&mut guard, Some(1))?)?;
        // Expecting v1 while v2 is primary: someone else rotated.
        expect_conflict(rotate(&mut guard, Some(1))?, "stale rotate")?;
        // Expecting a version that never existed.
        expect_conflict(rotate(&mut guard, Some(5))?, "future rotate")?;
        assert_eq!(primary(&mut guard)?, 2);
        assert_eq!(highest(&mut guard)?, 2);
        Ok(())
    }

    #[test]
    fn retried_rotate_after_success_conflicts_instead_of_skipping() -> TestResult {
        let mut guard = guard_with_key()?;
        // First attempt commits; its response is "lost" and the caller
        // retries the identical request.
        created_version(rotate(&mut guard, Some(1))?)?;
        expect_conflict(rotate(&mut guard, Some(1))?, "retried rotate")?;
        assert_eq!(highest(&mut guard)?, 2, "no v3 may exist");
        Ok(())
    }

    #[test]
    fn unpinned_rotate_is_forwarded() -> TestResult {
        let mut guard = guard_with_key()?;
        let (version, _) = created_version(rotate(&mut guard, None)?)?;
        assert_eq!(version, 2);
        Ok(())
    }

    #[test]
    fn rotate_of_unknown_key_is_not_found() -> TestResult {
        let mut guard = KmsGuardProvider::new(InMemoryProvider::new());
        match rotate(&mut guard, Some(1))? {
            Err(CryptoServiceError::NotFound) => Ok(()),
            other => Err(TestError::Unexpected(format!(
                "expected NotFound, got {other:?}"
            ))),
        }
    }

    #[test]
    fn wrap_is_primary_only_but_old_versions_still_unwrap() -> TestResult {
        let mut guard = guard_with_key()?;
        let old_blob = wrap(&mut guard, Some(1))?.map_err(ctx("wrap v1 while primary"))?;
        created_version(rotate(&mut guard, Some(1))?)?;

        // A stale generation must not wrap new DEKs under v1 any more.
        expect_conflict(wrap(&mut guard, Some(1))?, "wrap on non-primary")?;
        // The current primary, pinned or not, wraps.
        let new_blob = wrap(&mut guard, Some(2))?.map_err(ctx("wrap v2"))?;
        wrap(&mut guard, None)?.map_err(ctx("wrap latest"))?;

        // Existing v1 blobs stay readable.
        unwrap_ok(&mut guard, Some(1), old_blob)?;
        unwrap_ok(&mut guard, Some(2), new_blob)
    }

    #[test]
    fn encrypt_and_rewrap_target_are_primary_only() -> TestResult {
        let mut guard = guard_with_key()?;
        let blob = wrap(&mut guard, Some(1))?.map_err(ctx("wrap v1"))?;
        created_version(rotate(&mut guard, Some(1))?)?;

        let encrypt = CryptoOperation::Encrypt(crypt_guard_service::Encrypt {
            key: key(Some(1))?,
            plaintext: SecretBytes::copy_from_slice(b"x"),
            context: CryptoContext::default(),
        });
        expect_conflict(run(&mut guard, encrypt), "encrypt on non-primary")?;

        let rewrap = |to: Option<u32>, wrapped: CiphertextBlob| -> TestResult<CryptoOperation> {
            Ok(CryptoOperation::RewrapKey(RewrapKey {
                from: key(Some(1))?,
                from_context: CryptoContext::default(),
                to: key(to)?,
                to_context: CryptoContext::default(),
                wrapped,
            }))
        };
        let stale = rewrap(Some(1), CiphertextBlob::new(blob.as_bytes().to_vec()))?;
        expect_conflict(run(&mut guard, stale), "rewrap to non-primary")?;
        // Rewrap *from* the old version to the primary is the migration path.
        match run(&mut guard, rewrap(Some(2), blob)?) {
            Ok(CryptoResponse::Ciphertext(rewrapped)) => unwrap_ok(&mut guard, Some(2), rewrapped),
            other => Err(TestError::Unexpected(format!(
                "expected Ciphertext, got {other:?}"
            ))),
        }
    }

    #[test]
    fn generate_twice_with_same_algorithm_is_idempotent() -> TestResult {
        let mut guard = KmsGuardProvider::new(InMemoryProvider::new());
        let first = run(&mut guard, generate_op()?);
        let second = run(&mut guard, generate_op()?);
        match (first, second) {
            (
                Ok(CryptoResponse::KeyCreated {
                    key: first_key,
                    public: Some(first_public),
                }),
                Ok(CryptoResponse::KeyCreated {
                    key: second_key,
                    public: Some(second_public),
                }),
            ) => {
                assert_eq!(first_key, key(Some(1))?);
                assert_eq!(second_key, first_key);
                assert_eq!(second_public.as_bytes(), first_public.as_bytes());
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected two KeyCreated, got {other:?}"
                )));
            }
        }
        assert_eq!(highest(&mut guard)?, 1);
        Ok(())
    }

    #[test]
    fn generate_after_rotation_is_a_conflict() -> TestResult {
        let mut guard = guard_with_key()?;
        created_version(rotate(&mut guard, Some(1))?)?;
        expect_conflict(run(&mut guard, generate_op()?), "generate of rotated key")?;
        assert_eq!(highest(&mut guard)?, 2);
        Ok(())
    }

    /// Scripted inner provider: `generate` always conflicts, `describe`
    /// reports an existing v1 with a signature algorithm. Counts calls.
    #[derive(Default)]
    struct ExistingSigningKey {
        generates: usize,
        others: usize,
    }

    impl CryptoProvider for ExistingSigningKey {
        fn execute(
            &mut self,
            request: CryptoRequest,
        ) -> Result<CryptoResponse, CryptoServiceError> {
            match request.operation {
                CryptoOperation::Generate(_) => {
                    self.generates += 1;
                    Err(CryptoServiceError::Conflict)
                }
                CryptoOperation::Describe(op) => {
                    self.others += 1;
                    Ok(CryptoResponse::Metadata(KeyMetadata {
                        key: KeyRef::versioned(op.key.namespace, op.key.id, KeyVersion::FIRST),
                        algorithm: KeyAlgorithm::Signature(SignatureAlgorithm::MlDsa65),
                        state: KeyState::Enabled,
                    }))
                }
                _ => {
                    self.others += 1;
                    Err(CryptoServiceError::Unsupported)
                }
            }
        }
    }

    #[test]
    fn generate_with_other_algorithm_conflicts_without_retry() -> TestResult {
        let mut guard = KmsGuardProvider::new(ExistingSigningKey::default());
        expect_conflict(run(&mut guard, generate_op()?), "algorithm mismatch")?;
        assert_eq!(guard.inner().generates, 1, "a mutation is never retried");
        assert_eq!(guard.inner().others, 1, "only one describe");
        Ok(())
    }
}
