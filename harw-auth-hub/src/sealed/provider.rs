//! [`SealedProvider`]: the persistent `CryptoProvider`.
//!
//! Operations, lifecycle and error table mirror CryptGuard's
//! `InMemoryProvider` (`crypt_guard_service/src/memory/{mod,hpke_ops,
//! sign_ops,store}.rs`), with one deliberate addition: `Rotate` is a native
//! compare-and-set — a versioned `KeyRef` that does not name the current
//! primary is `Conflict` (the in-memory store ignores `key.version`).
//!
//! | Situation | Error |
//! |-----------|-------|
//! | unknown key (any op) | `NotFound` |
//! | `Generate` of an existing key | `Conflict` |
//! | `Rotate` with `Some(v) != primary` | `Conflict` |
//! | version disabled (encrypt/decrypt/sign/verify/public key) | `Conflict` |
//! | primary/explicit version destroyed (encrypt/sign/verify/public key) | `NotFound` |
//! | decrypt: frame malformed, purpose or version mismatch, unknown or destroyed version, wrong `info`/`aad`, tampering | `AuthenticationFailed` |
//! | `Destroy` without explicit version | `Malformed` |
//! | `Destroy`/`Enable`/`Disable` of a destroyed version | `Conflict` |
//! | HPKE op on a signing key / `Sign` on an HPKE key | `Unsupported` |
//! | mutation could not be persisted (rolled back in memory) | `Unavailable` |

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use crypt_guard::sign::SignAlgorithm;
use crypt_guard::sign::ml_dsa::{MlDsa44Impl, MlDsa65Impl, MlDsa87Impl, MlDsaSignature};
use crypt_guard_service::pq_hpke::{HpkeEnvelope, RecipientPublicKey, Suite};
use crypt_guard_service::{
    CiphertextBlob, CryptoContext, CryptoOperation, CryptoProvider, CryptoRequest, CryptoResponse,
    CryptoServiceError, KeyId, KeyNamespace, KeyRef, KeyState, KeyVersion, SecretBytes,
    SignatureAlgorithm, SignatureBlob, VerificationResult,
};

use super::format::{Purpose, bind_info, decode_frame, encode_frame};
use super::store::{KeyTable, Material, StoreFile};
use super::{Kek, SealedStoreError};

/// Persistent key provider whose seeds are sealed at rest under a [`Kek`].
///
/// Not `Clone`: it owns all key material and the store lock. Every
/// successful mutation (`Generate`, `Rotate`, `Enable`, `Disable`,
/// `Destroy`) is durably written before it is acknowledged.
pub struct SealedProvider {
    table: KeyTable,
    file: StoreFile,
    /// Stable epoch of this store, persisted at `<path>.epoch`. See
    /// [`Self::store_epoch`].
    epoch: String,
}

impl fmt::Debug for SealedProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SealedProvider")
            .field("file", &self.file)
            .field("keys", &self.table.len())
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

/// Upper bound for the persisted store epoch file (fail closed on anything
/// larger; the content itself is exactly
/// [`crate::meta::EPOCH_HEX_LEN`] bytes).
const MAX_EPOCH_FILE_LEN: u64 = 4096;

/// `<path>.epoch`: where a store's stable epoch is persisted, next to the
/// store file itself (mirrors [`super::store::lock_path`]'s `<path>.lock`).
pub(super) fn epoch_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".epoch");
    PathBuf::from(name)
}

/// Persist a freshly generated epoch at `<path>.epoch` (`0600`, atomic via
/// `harw_fsutil::write_atomic`).
fn write_epoch_file(path: &Path, epoch: &str) -> Result<(), SealedStoreError> {
    let path = epoch_path(path);
    harw_fsutil::write_atomic(
        &path,
        epoch.as_bytes(),
        harw_fsutil::AtomicWriteOptions::private(),
    )
    .map_err(SealedStoreError::io("write store epoch", &path))
}

/// Read and validate the epoch persisted at `<path>.epoch`.
///
/// Same no-symlink-following and owner/`0600` discipline as the store file
/// itself (`store.rs`'s `read_store_file`): opened with
/// [`harw_fsutil::open_nofollow`] and checked with
/// [`harw_fsutil::ensure_private_regular`], so a symlink or a file with
/// group/other permission bits fails the same way the store file would
/// (`SealedStoreError::Io`). A missing file, one over the size bound, or
/// content that is not exactly a valid epoch (checked with
/// [`crate::meta::StoreIdentity::sealed_file`]) is
/// `SealedStoreError::Corrupt` — fail closed rather than serve an
/// unauthenticated/unwritten epoch.
fn read_epoch_file(path: &Path) -> Result<String, SealedStoreError> {
    let path = epoch_path(path);
    let file = match harw_fsutil::open_nofollow(&path, harw_fsutil::OpenMode::read_only()) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(SealedStoreError::Corrupt("missing store epoch file"));
        }
        Err(error) => return Err(SealedStoreError::io("open store epoch", &path)(error)),
    };
    harw_fsutil::ensure_private_regular(&file)
        .map_err(SealedStoreError::io("check store epoch permissions", &path))?;
    let len = file
        .metadata()
        .map_err(SealedStoreError::io("stat store epoch", &path))?
        .len();
    if len > MAX_EPOCH_FILE_LEN {
        return Err(SealedStoreError::Corrupt("store epoch file too large"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_EPOCH_FILE_LEN.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(SealedStoreError::io("read store epoch", &path))?;
    let read = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if read > MAX_EPOCH_FILE_LEN {
        return Err(SealedStoreError::Corrupt("store epoch file too large"));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| SealedStoreError::Corrupt("store epoch is not valid UTF-8"))?;
    crate::meta::StoreIdentity::sealed_file(text)
        .map(|identity| identity.epoch().to_owned())
        .map_err(|_| SealedStoreError::Corrupt("store epoch is not a valid epoch"))
}

impl SealedProvider {
    /// Open an existing store at `path` (fail closed on any error).
    ///
    /// Also reads and validates this store's epoch from `<path>.epoch`
    /// ([`Self::store_epoch`]); a missing or invalid epoch file is
    /// `Corrupt`, even if the store file itself opens cleanly.
    ///
    /// # Errors
    /// See [`SealedStoreError`]; notably `NotFound`, `Locked`,
    /// `Authentication` (tampered file or wrong KEK), `Io` for a store or
    /// epoch file with group/other permission bits, and `Corrupt` for a
    /// missing or invalid epoch file.
    pub fn open(path: &Path, kek: &Kek) -> Result<Self, SealedStoreError> {
        let (file, table) = StoreFile::open(path, kek)?;
        let epoch = read_epoch_file(path)?;
        Ok(Self { table, file, epoch })
    }

    /// Create a new, empty store at `path` (mode `0600`).
    ///
    /// A fresh epoch ([`crate::meta::generate_store_epoch`]) is generated
    /// and written to `<path>.epoch` **before** the store file itself, so a
    /// crash between the two writes never leaves a store without an epoch.
    ///
    /// # Errors
    /// `AlreadyExists` if anything exists at `path`; `Locked`; `Io`
    /// (including a failed epoch-file write).
    pub fn create(path: &Path, kek: &Kek) -> Result<Self, SealedStoreError> {
        if std::fs::symlink_metadata(path).is_ok() {
            return Err(SealedStoreError::AlreadyExists(path.to_path_buf()));
        }
        let epoch = crate::meta::generate_store_epoch();
        write_epoch_file(path, &epoch)?;
        let (file, table) = StoreFile::create(path, kek)?;
        Ok(Self { table, file, epoch })
    }

    /// [`Self::open`] if `path` exists, else [`Self::create`].
    ///
    /// # Errors
    /// As [`Self::open`] / [`Self::create`].
    pub fn open_or_create(path: &Path, kek: &Kek) -> Result<Self, SealedStoreError> {
        match Self::open(path, kek) {
            Err(SealedStoreError::NotFound(_)) => Self::create(path, kek),
            other => other,
        }
    }

    /// Path of the store file.
    pub fn path(&self) -> &Path {
        self.file.path()
    }

    /// This store's stable epoch (32 lowercase hex digits), persisted next
    /// to the store file as `<path>.epoch`.
    ///
    /// Generated once, when the store is created ([`Self::create`]), and
    /// unchanged by every later [`Self::open`] of the same store: a client
    /// that sees this value change knows the store was replaced, not merely
    /// restarted (see `crate::meta`'s "Store epoch and boot id").
    #[must_use]
    pub fn store_epoch(&self) -> &str {
        &self.epoch
    }

    /// Write the current table; on failure log and report `Unavailable`
    /// (the caller rolls its in-memory change back).
    fn persist(&mut self) -> Result<(), CryptoServiceError> {
        self.file.save(&self.table).map_err(|error| {
            tracing::error!(
                store = %self.file.path().display(),
                %error,
                "sealed key store: persisting a key mutation failed; change rolled back"
            );
            CryptoServiceError::Unavailable
        })
    }

    fn created(&self, key: KeyRef) -> Result<CryptoResponse, CryptoServiceError> {
        let public = self.table.resolve(&key)?.usable()?.public_blob();
        Ok(CryptoResponse::KeyCreated {
            key,
            public: Some(public),
        })
    }

    fn dispatch(
        &mut self,
        operation: CryptoOperation,
    ) -> Result<CryptoResponse, CryptoServiceError> {
        match operation {
            CryptoOperation::Generate(op) => {
                let material = Material::generate(op.algorithm)?;
                let key = self
                    .table
                    .insert_new(op.namespace, op.id, op.algorithm, material)?;
                if let Err(error) = self.persist() {
                    self.table.remove_key(&key);
                    return Err(error);
                }
                self.created(key)
            }
            CryptoOperation::Rotate(op) => {
                let algorithm = self.table.rotation_algorithm(&op.key)?;
                let material = Material::generate(algorithm)?;
                let (key, previous_primary) = self.table.rotate(&op.key, material)?;
                if let Err(error) = self.persist() {
                    self.table.undo_rotate(&key, previous_primary);
                    return Err(error);
                }
                self.created(key)
            }
            CryptoOperation::Disable(op) => self.change_state(&op.key, KeyState::Disabled),
            CryptoOperation::Enable(op) => self.change_state(&op.key, KeyState::Enabled),
            CryptoOperation::Destroy(op) => {
                let (metadata, previous, material) = self.table.destroy(&op.key)?;
                if let Err(error) = self.persist() {
                    self.table.undo_destroy(&metadata.key, previous, material);
                    return Err(error);
                }
                // The seed is dropped (zeroized) only after the tombstone is
                // durable.
                drop(material);
                Ok(CryptoResponse::Metadata(metadata))
            }
            CryptoOperation::Describe(op) => {
                self.table.metadata(&op.key).map(CryptoResponse::Metadata)
            }
            CryptoOperation::PublicKey(op) => {
                let public = self.table.resolve(&op.key)?.usable()?.public_blob();
                Ok(CryptoResponse::PublicKey(public))
            }
            CryptoOperation::Encrypt(op) => seal(
                &self.table,
                Purpose::Encrypt,
                &op.key,
                op.plaintext.as_ref(),
                &op.context,
            )
            .map(CryptoResponse::Ciphertext),
            CryptoOperation::Decrypt(op) => open(
                &self.table,
                Purpose::Encrypt,
                &op.key,
                op.ciphertext.as_bytes(),
                &op.context,
            )
            .map(CryptoResponse::Plaintext),
            CryptoOperation::WrapKey(op) => seal(
                &self.table,
                Purpose::Wrap,
                &op.key,
                op.material.as_ref(),
                &op.context,
            )
            .map(CryptoResponse::Ciphertext),
            CryptoOperation::UnwrapKey(op) => open(
                &self.table,
                Purpose::Wrap,
                &op.key,
                op.wrapped.as_bytes(),
                &op.context,
            )
            .map(CryptoResponse::Plaintext),
            CryptoOperation::RewrapKey(op) => {
                let secret = open(
                    &self.table,
                    Purpose::Wrap,
                    &op.from,
                    op.wrapped.as_bytes(),
                    &op.from_context,
                )?;
                seal(
                    &self.table,
                    Purpose::Wrap,
                    &op.to,
                    secret.as_ref(),
                    &op.to_context,
                )
                .map(CryptoResponse::Ciphertext)
            }
            CryptoOperation::Sign(op) => {
                let material = self.table.resolve(&op.key)?.usable()?;
                sign(material, op.message.as_ref()).map(CryptoResponse::Signature)
            }
            CryptoOperation::Verify(op) => {
                let material = self.table.resolve(&op.key)?.usable()?;
                verify(material, op.message.as_bytes(), op.signature.as_bytes())
                    .map(CryptoResponse::Verification)
            }
            _ => Err(CryptoServiceError::Unsupported),
        }
    }

    fn change_state(
        &mut self,
        key: &KeyRef,
        state: KeyState,
    ) -> Result<CryptoResponse, CryptoServiceError> {
        let (metadata, previous) = self.table.set_state(key, state)?;
        if let Err(error) = self.persist() {
            self.table.restore_state(&metadata.key, previous);
            return Err(error);
        }
        Ok(CryptoResponse::Metadata(metadata))
    }
}

impl CryptoProvider for SealedProvider {
    fn execute(&mut self, request: CryptoRequest) -> Result<CryptoResponse, CryptoServiceError> {
        // The request (and any secret input it owns) is dropped, and thereby
        // zeroized, when this call returns.
        self.dispatch(request.operation)
    }
}

// ---------------------------------------------------------------------------
// HPKE
// ---------------------------------------------------------------------------

/// Everything the `CGKC` frame and the HPKE `info` binding commit to.
#[derive(Clone, Copy, Debug)]
pub(super) struct FrameTarget<'a> {
    /// Encrypt or wrap.
    pub(super) purpose: Purpose,
    /// Key namespace.
    pub(super) namespace: &'a KeyNamespace,
    /// Key id.
    pub(super) id: &'a KeyId,
    /// Key version written into the frame header.
    pub(super) version: KeyVersion,
    /// HPKE suite of the key.
    pub(super) suite: Suite,
}

/// Seal `plaintext` into a `CGKC` frame for `target` under `public`. Shared
/// by the provider and the cross-compatibility tests (which seal to an
/// `InMemoryProvider` public key).
pub(super) fn seal_frame(
    target: &FrameTarget<'_>,
    public: &RecipientPublicKey,
    context: &CryptoContext,
    plaintext: &[u8],
) -> Result<CiphertextBlob, CryptoServiceError> {
    let info = bind_info(
        target.purpose,
        target.namespace,
        target.id,
        target.version,
        target.suite,
        &context.info,
    )?;
    let envelope = HpkeEnvelope::seal(target.suite, public, &info, &context.aad, plaintext)?;
    Ok(CiphertextBlob::new(encode_frame(
        target.purpose,
        target.version,
        &envelope.to_bytes(),
    )))
}

/// Seal under the version `key` resolves to (primary when unversioned).
fn seal(
    table: &KeyTable,
    purpose: Purpose,
    key: &KeyRef,
    plaintext: &[u8],
    context: &CryptoContext,
) -> Result<CiphertextBlob, CryptoServiceError> {
    let entry = table.resolve(key)?;
    let Material::Hpke { suite, keys, .. } = entry.usable()? else {
        return Err(CryptoServiceError::Unsupported);
    };
    let target = FrameTarget {
        purpose,
        namespace: &key.namespace,
        id: &key.id,
        version: entry.version,
        suite: *suite,
    };
    seal_frame(&target, keys.public_key(), context, plaintext)
}

/// Open a `CGKC` frame made for `purpose`; the key version comes from the
/// frame. Same check order and error table as CryptGuard's `hpke_ops::open`.
fn open(
    table: &KeyTable,
    purpose: Purpose,
    key: &KeyRef,
    ciphertext: &[u8],
    context: &CryptoContext,
) -> Result<SecretBytes, CryptoServiceError> {
    const FAIL: CryptoServiceError = CryptoServiceError::AuthenticationFailed;

    // "Key never existed" (NotFound) vs "this version does not exist"
    // (AuthenticationFailed).
    table.algorithm_of(key)?;

    let frame = decode_frame(ciphertext).map_err(|_| FAIL)?;
    if frame.purpose != purpose {
        return Err(FAIL);
    }
    if let Some(version) = key.version {
        if version != frame.key_version {
            return Err(FAIL);
        }
    }

    let entry = match table.resolve_version(&key.namespace, &key.id, frame.key_version) {
        Ok(entry) => entry,
        Err(CryptoServiceError::NotFound) => return Err(FAIL),
        Err(error) => return Err(error),
    };
    let material = match entry.usable() {
        Ok(material) => material,
        Err(CryptoServiceError::NotFound) => return Err(FAIL),
        Err(error) => return Err(error),
    };
    let Material::Hpke { suite, keys, .. } = material else {
        return Err(CryptoServiceError::Unsupported);
    };

    let envelope = HpkeEnvelope::from_bytes(frame.envelope).map_err(|_| FAIL)?;
    if envelope.suite() != *suite {
        return Err(FAIL);
    }
    let info = bind_info(
        purpose,
        &key.namespace,
        &key.id,
        frame.key_version,
        *suite,
        &context.info,
    )
    .map_err(|_| FAIL)?;
    let mut plaintext = envelope
        .open_zeroizing(keys.private_key(), &info, &context.aad)
        .map_err(|_| FAIL)?;
    // Move the bytes into `SecretBytes` (which zeroizes on drop); the
    // emptied `Zeroizing` buffer is wiped when it drops.
    Ok(SecretBytes::from_vec(std::mem::take(&mut *plaintext)))
}

// ---------------------------------------------------------------------------
// ML-DSA
// ---------------------------------------------------------------------------

fn sign(material: &Material, message: &[u8]) -> Result<SignatureBlob, CryptoServiceError> {
    let Material::MlDsa {
        algorithm, signing, ..
    } = material
    else {
        return Err(CryptoServiceError::Unsupported);
    };
    let signature = match algorithm {
        SignatureAlgorithm::MlDsa44 => MlDsa44Impl::sign(signing, message)?,
        SignatureAlgorithm::MlDsa65 => MlDsa65Impl::sign(signing, message)?,
        SignatureAlgorithm::MlDsa87 => MlDsa87Impl::sign(signing, message)?,
        _ => return Err(CryptoServiceError::Unsupported),
    };
    Ok(SignatureBlob::new(signature.as_ref().to_vec()))
}

/// An invalid or malformed signature is `Ok(Invalid)`, not an error.
fn verify(
    material: &Material,
    message: &[u8],
    signature: &[u8],
) -> Result<VerificationResult, CryptoServiceError> {
    let Material::MlDsa {
        algorithm,
        verifying,
        ..
    } = material
    else {
        return Err(CryptoServiceError::Unsupported);
    };
    let signature = MlDsaSignature::from_bytes(signature.to_vec());
    let result = match algorithm {
        SignatureAlgorithm::MlDsa44 => MlDsa44Impl::verify(verifying, message, &signature),
        SignatureAlgorithm::MlDsa65 => MlDsa65Impl::verify(verifying, message, &signature),
        SignatureAlgorithm::MlDsa87 => MlDsa87Impl::verify(verifying, message, &signature),
        _ => return Err(CryptoServiceError::Unsupported),
    };
    Ok(match result {
        Ok(()) => VerificationResult::Valid,
        Err(_) => VerificationResult::Invalid,
    })
}
