//! Key table (same lifecycle and error semantics as CryptGuard's
//! `InMemoryProvider` key store) and the sealed store file.
//!
//! Lifecycle per key version:
//!
//! ```text
//! Enabled ⇄ Disabled
//!    │         │
//!    └────┬────┘
//!         ▼
//!     Destroyed   (seed dropped + zeroized; tombstone persisted; terminal)
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crypt_guard::kem::backend::OsRng;
use crypt_guard::sign::SignAlgorithm;
use crypt_guard::sign::ml_dsa::{
    MlDsa44Impl, MlDsa65Impl, MlDsa87Impl, MlDsaSigningKey, MlDsaVerifyingKey,
};
use crypt_guard_service::pq_hpke::{self, Capability, RECIPIENT_SEED_LEN, RecipientKeyPair, Suite};
use crypt_guard_service::{
    CryptoServiceError, KeyAlgorithm, KeyId, KeyMetadata, KeyNamespace, KeyRef, KeyState,
    KeyVersion, PublicBlob, SignatureAlgorithm,
};
use zeroize::Zeroizing;

use super::{Kek, SealedStoreError, format};

/// Length of every persisted seed (HPKE provenance seed, ML-DSA signing
/// seed).
pub(super) const SEED_LEN: usize = 32;

// `RECIPIENT_SEED_LEN` is CryptGuard's constant; the store format fixes 32.
const _: () = assert!(RECIPIENT_SEED_LEN == SEED_LEN);

/// Upper bound for a store file (fail closed on anything larger).
const MAX_STORE_FILE_LEN: u64 = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Key material
// ---------------------------------------------------------------------------

/// Secret material of one key version. Never `Clone`, never printed.
pub(super) enum Material {
    /// PQ HPKE recipient key: the persisted provenance seed and the key pair
    /// derived from it.
    Hpke {
        /// Suite the key was generated for.
        suite: Suite,
        /// 32-byte provenance seed (`generate_recipient_seed`).
        seed: Zeroizing<[u8; SEED_LEN]>,
        /// `derive_recipient_key_pair(suite.kem(), seed)`.
        keys: RecipientKeyPair,
    },
    /// ML-DSA signing key.
    MlDsa {
        /// Parameter set.
        algorithm: SignatureAlgorithm,
        /// 32-byte FIPS 204 seed (zeroized on drop).
        signing: MlDsaSigningKey,
        /// Encoded verifying key.
        verifying: MlDsaVerifyingKey,
    },
}

impl fmt::Debug for Material {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let variant = match self {
            Self::Hpke { .. } => "Hpke",
            Self::MlDsa { .. } => "MlDsa",
        };
        write!(f, "Material::{variant}([REDACTED])")
    }
}

/// Encoded verifying-key length per ML-DSA parameter set (FIPS 204).
fn ml_dsa_verifying_len(algorithm: SignatureAlgorithm) -> Option<usize> {
    match algorithm {
        SignatureAlgorithm::MlDsa44 => Some(1312),
        SignatureAlgorithm::MlDsa65 => Some(1952),
        SignatureAlgorithm::MlDsa87 => Some(2592),
        _ => None,
    }
}

impl Material {
    /// Fresh material for `algorithm`.
    ///
    /// Errors: suite not available / unknown algorithm → `Unsupported`;
    /// RNG or core failures via CryptGuard's `From` impls.
    pub(super) fn generate(algorithm: KeyAlgorithm) -> Result<Self, CryptoServiceError> {
        match algorithm {
            KeyAlgorithm::Hpke { suite } => {
                let seed = pq_hpke::generate_recipient_seed()?;
                Self::hpke_from_seed(suite, seed)
            }
            KeyAlgorithm::Signature(algorithm) => {
                let mut rng = OsRng;
                let (signing, verifying) = match algorithm {
                    SignatureAlgorithm::MlDsa44 => MlDsa44Impl::keypair(&mut rng)?,
                    SignatureAlgorithm::MlDsa65 => MlDsa65Impl::keypair(&mut rng)?,
                    SignatureAlgorithm::MlDsa87 => MlDsa87Impl::keypair(&mut rng)?,
                    _ => return Err(CryptoServiceError::Unsupported),
                };
                Ok(Self::MlDsa {
                    algorithm,
                    signing,
                    verifying,
                })
            }
            _ => Err(CryptoServiceError::Unsupported),
        }
    }

    /// Re-derive an HPKE key pair from its persisted provenance seed.
    pub(super) fn hpke_from_seed(
        suite: Suite,
        seed: Zeroizing<[u8; SEED_LEN]>,
    ) -> Result<Self, CryptoServiceError> {
        if let Capability::Unavailable(_) = suite.capability() {
            return Err(CryptoServiceError::Unsupported);
        }
        let keys = pq_hpke::derive_recipient_key_pair(suite.kem(), &seed[..])?;
        Ok(Self::Hpke { suite, seed, keys })
    }

    /// Rebuild an ML-DSA key from its persisted seed and verifying key.
    /// Errors: wrong lengths → `Malformed`; unknown parameter set →
    /// `Unsupported`.
    pub(super) fn ml_dsa_from_parts(
        algorithm: SignatureAlgorithm,
        seed: &[u8; SEED_LEN],
        verifying: Vec<u8>,
    ) -> Result<Self, CryptoServiceError> {
        let expected = ml_dsa_verifying_len(algorithm).ok_or(CryptoServiceError::Unsupported)?;
        if verifying.len() != expected {
            return Err(CryptoServiceError::Malformed);
        }
        Ok(Self::MlDsa {
            algorithm,
            signing: MlDsaSigningKey::from_bytes(seed.to_vec()),
            verifying: MlDsaVerifyingKey::from_bytes(verifying),
        })
    }

    /// The 32 seed bytes that are persisted for this material.
    pub(super) fn seed_bytes(&self) -> Result<&[u8], SealedStoreError> {
        let bytes = match self {
            Self::Hpke { seed, .. } => &seed[..],
            Self::MlDsa { signing, .. } => signing.as_bytes(),
        };
        if bytes.len() == SEED_LEN {
            Ok(bytes)
        } else {
            Err(SealedStoreError::Corrupt("seed length"))
        }
    }

    /// Encoded public key.
    pub(super) fn public_blob(&self) -> PublicBlob {
        match self {
            Self::Hpke { keys, .. } => PublicBlob::new(keys.public_key().as_bytes().to_vec()),
            Self::MlDsa { verifying, .. } => PublicBlob::new(verifying.as_bytes().to_vec()),
        }
    }
}

// ---------------------------------------------------------------------------
// Key table
// ---------------------------------------------------------------------------

/// One version of a key.
#[derive(Debug)]
pub(super) struct VersionEntry {
    /// This version.
    pub(super) version: KeyVersion,
    /// Lifecycle state.
    pub(super) state: KeyState,
    /// `None` once destroyed.
    pub(super) material: Option<Material>,
}

impl VersionEntry {
    /// The material, if this version may be used.
    ///
    /// Errors: `Disabled`/`PendingDestruction` → `Conflict`;
    /// `Destroyed` / no material → `NotFound`.
    pub(super) fn usable(&self) -> Result<&Material, CryptoServiceError> {
        match self.state {
            KeyState::Enabled => self.material.as_ref().ok_or(CryptoServiceError::NotFound),
            KeyState::Disabled | KeyState::PendingDestruction => Err(CryptoServiceError::Conflict),
            _ => Err(CryptoServiceError::NotFound),
        }
    }
}

/// All versions of one key.
#[derive(Debug)]
pub(super) struct KeyEntry {
    /// Algorithm shared by all versions (rotation keeps it).
    pub(super) algorithm: KeyAlgorithm,
    /// Current primary version (used when a `KeyRef` has no version).
    pub(super) primary: KeyVersion,
    /// All versions, including tombstones.
    pub(super) versions: BTreeMap<KeyVersion, VersionEntry>,
}

/// The key table, ordered for a deterministic store encoding.
#[derive(Debug, Default)]
pub(super) struct KeyTable {
    keys: BTreeMap<(KeyNamespace, KeyId), KeyEntry>,
}

fn name_of(key: &KeyRef) -> (KeyNamespace, KeyId) {
    (key.namespace.clone(), key.id.clone())
}

impl KeyTable {
    /// Number of keys.
    pub(super) fn len(&self) -> usize {
        self.keys.len()
    }

    /// All keys in ascending `(namespace, id)` order.
    pub(super) fn entries(&self) -> impl Iterator<Item = (&(KeyNamespace, KeyId), &KeyEntry)> {
        self.keys.iter()
    }

    /// Insert a key decoded from the store file.
    pub(super) fn insert_loaded(&mut self, name: (KeyNamespace, KeyId), entry: KeyEntry) {
        self.keys.insert(name, entry);
    }

    /// Create a new key with version 1 as primary.
    /// Errors: key exists → `Conflict`.
    pub(super) fn insert_new(
        &mut self,
        namespace: KeyNamespace,
        id: KeyId,
        algorithm: KeyAlgorithm,
        material: Material,
    ) -> Result<KeyRef, CryptoServiceError> {
        use std::collections::btree_map::Entry;

        let version = KeyVersion::FIRST;
        match self.keys.entry((namespace.clone(), id.clone())) {
            Entry::Occupied(_) => Err(CryptoServiceError::Conflict),
            Entry::Vacant(slot) => {
                let mut versions = BTreeMap::new();
                versions.insert(
                    version,
                    VersionEntry {
                        version,
                        state: KeyState::Enabled,
                        material: Some(material),
                    },
                );
                slot.insert(KeyEntry {
                    algorithm,
                    primary: version,
                    versions,
                });
                Ok(KeyRef::versioned(namespace, id, version))
            }
        }
    }

    /// Undo [`Self::insert_new`] (persist failed).
    pub(super) fn remove_key(&mut self, key: &KeyRef) {
        self.keys.remove(&name_of(key));
    }

    /// Rotation precondition: the key exists and, when `key.version` is set,
    /// it names the current primary (compare-and-set). Returns the algorithm
    /// the new version must use.
    ///
    /// Errors: unknown key → `NotFound`; `Some(v) != primary` → `Conflict`.
    pub(super) fn rotation_algorithm(
        &self,
        key: &KeyRef,
    ) -> Result<KeyAlgorithm, CryptoServiceError> {
        let entry = self
            .keys
            .get(&name_of(key))
            .ok_or(CryptoServiceError::NotFound)?;
        if let Some(expected) = key.version {
            if expected != entry.primary {
                return Err(CryptoServiceError::Conflict);
            }
        }
        Ok(entry.algorithm)
    }

    /// Add version `highest + 1` and make it primary, after re-checking the
    /// compare-and-set precondition. Returns the new versioned `KeyRef` and
    /// the previous primary (for [`Self::undo_rotate`]).
    ///
    /// Errors: as [`Self::rotation_algorithm`]; version overflow → `Conflict`.
    pub(super) fn rotate(
        &mut self,
        key: &KeyRef,
        material: Material,
    ) -> Result<(KeyRef, KeyVersion), CryptoServiceError> {
        self.rotation_algorithm(key)?;
        let entry = self
            .keys
            .get_mut(&name_of(key))
            .ok_or(CryptoServiceError::NotFound)?;
        let highest = entry
            .versions
            .keys()
            .next_back()
            .copied()
            .unwrap_or(KeyVersion::FIRST);
        let new_version = highest
            .get()
            .checked_add(1)
            .and_then(|v| KeyVersion::new(v).ok())
            .ok_or(CryptoServiceError::Conflict)?;

        entry.versions.insert(
            new_version,
            VersionEntry {
                version: new_version,
                state: KeyState::Enabled,
                material: Some(material),
            },
        );
        let previous = entry.primary;
        entry.primary = new_version;
        Ok((
            KeyRef::versioned(key.namespace.clone(), key.id.clone(), new_version),
            previous,
        ))
    }

    /// Undo [`Self::rotate`]: drop the new version, restore the primary.
    pub(super) fn undo_rotate(&mut self, new: &KeyRef, previous_primary: KeyVersion) {
        if let Some(entry) = self.keys.get_mut(&name_of(new)) {
            if let Some(version) = new.version {
                entry.versions.remove(&version);
            }
            entry.primary = previous_primary;
        }
    }

    /// Algorithm of a key. Errors: unknown key → `NotFound`.
    pub(super) fn algorithm_of(&self, key: &KeyRef) -> Result<KeyAlgorithm, CryptoServiceError> {
        self.keys
            .get(&name_of(key))
            .map(|entry| entry.algorithm)
            .ok_or(CryptoServiceError::NotFound)
    }

    /// Move a version (primary when unversioned) between `Enabled` and
    /// `Disabled`. Returns the metadata and the previous state (for
    /// [`Self::restore_state`]).
    ///
    /// Errors: other target state or destroyed version → `Conflict`;
    /// unknown key/version → `NotFound`.
    pub(super) fn set_state(
        &mut self,
        key: &KeyRef,
        state: KeyState,
    ) -> Result<(KeyMetadata, KeyState), CryptoServiceError> {
        if !matches!(state, KeyState::Enabled | KeyState::Disabled) {
            return Err(CryptoServiceError::Conflict);
        }
        let entry = self
            .keys
            .get_mut(&name_of(key))
            .ok_or(CryptoServiceError::NotFound)?;
        let version = key.version.unwrap_or(entry.primary);
        let algorithm = entry.algorithm;
        let version_entry = entry
            .versions
            .get_mut(&version)
            .ok_or(CryptoServiceError::NotFound)?;
        if matches!(version_entry.state, KeyState::Destroyed) {
            return Err(CryptoServiceError::Conflict);
        }
        let previous = version_entry.state;
        version_entry.state = state;
        Ok((
            KeyMetadata {
                key: KeyRef::versioned(key.namespace.clone(), key.id.clone(), version),
                algorithm,
                state,
            },
            previous,
        ))
    }

    /// Undo [`Self::set_state`]. `key` must be versioned.
    pub(super) fn restore_state(&mut self, key: &KeyRef, state: KeyState) {
        if let Some(version_entry) = self.version_mut(key) {
            version_entry.state = state;
        }
    }

    /// Destroy one explicit version and leave a tombstone. The primary is
    /// not re-pointed. Returns the metadata, the previous state and the
    /// removed material (for [`Self::undo_destroy`]; drop it to zeroize).
    ///
    /// Errors: `key.version == None` → `Malformed`; unknown → `NotFound`;
    /// already destroyed → `Conflict`.
    pub(super) fn destroy(
        &mut self,
        key: &KeyRef,
    ) -> Result<(KeyMetadata, KeyState, Option<Material>), CryptoServiceError> {
        let version = key.version.ok_or(CryptoServiceError::Malformed)?;
        let entry = self
            .keys
            .get_mut(&name_of(key))
            .ok_or(CryptoServiceError::NotFound)?;
        let algorithm = entry.algorithm;
        let version_entry = entry
            .versions
            .get_mut(&version)
            .ok_or(CryptoServiceError::NotFound)?;
        if matches!(version_entry.state, KeyState::Destroyed) {
            return Err(CryptoServiceError::Conflict);
        }
        let previous = version_entry.state;
        let material = version_entry.material.take();
        version_entry.state = KeyState::Destroyed;
        Ok((
            KeyMetadata {
                key: KeyRef::versioned(key.namespace.clone(), key.id.clone(), version),
                algorithm,
                state: KeyState::Destroyed,
            },
            previous,
            material,
        ))
    }

    /// Undo [`Self::destroy`]. `key` must be versioned.
    pub(super) fn undo_destroy(
        &mut self,
        key: &KeyRef,
        state: KeyState,
        material: Option<Material>,
    ) {
        if let Some(version_entry) = self.version_mut(key) {
            version_entry.state = state;
            version_entry.material = material;
        }
    }

    fn version_mut(&mut self, key: &KeyRef) -> Option<&mut VersionEntry> {
        let version = key.version?;
        self.keys
            .get_mut(&name_of(key))
            .and_then(|entry| entry.versions.get_mut(&version))
    }

    /// Resolve a `KeyRef` (primary when unversioned), whatever its state.
    /// Errors: unknown key/version → `NotFound`.
    pub(super) fn resolve(&self, key: &KeyRef) -> Result<&VersionEntry, CryptoServiceError> {
        let entry = self
            .keys
            .get(&name_of(key))
            .ok_or(CryptoServiceError::NotFound)?;
        let version = key.version.unwrap_or(entry.primary);
        entry
            .versions
            .get(&version)
            .ok_or(CryptoServiceError::NotFound)
    }

    /// Resolve an explicit version. Errors: unknown → `NotFound`.
    pub(super) fn resolve_version(
        &self,
        namespace: &KeyNamespace,
        id: &KeyId,
        version: KeyVersion,
    ) -> Result<&VersionEntry, CryptoServiceError> {
        self.keys
            .get(&(namespace.clone(), id.clone()))
            .and_then(|entry| entry.versions.get(&version))
            .ok_or(CryptoServiceError::NotFound)
    }

    /// Metadata of a version (primary when unversioned) with a versioned
    /// `KeyRef`. Errors: unknown → `NotFound`.
    pub(super) fn metadata(&self, key: &KeyRef) -> Result<KeyMetadata, CryptoServiceError> {
        let entry = self
            .keys
            .get(&name_of(key))
            .ok_or(CryptoServiceError::NotFound)?;
        let version = key.version.unwrap_or(entry.primary);
        let version_entry = entry
            .versions
            .get(&version)
            .ok_or(CryptoServiceError::NotFound)?;
        Ok(KeyMetadata {
            key: KeyRef::versioned(key.namespace.clone(), key.id.clone(), version),
            algorithm: entry.algorithm,
            state: version_entry.state,
        })
    }
}

// ---------------------------------------------------------------------------
// Store file
// ---------------------------------------------------------------------------

/// The on-disk half of a sealed provider: path, derived file key, write
/// generation and the exclusive lock.
pub(super) struct StoreFile {
    path: PathBuf,
    file_key: Zeroizing<[u8; 32]>,
    generation: u64,
    /// Held for the provider's lifetime; dropping it releases the `flock`.
    _lock: File,
}

impl fmt::Debug for StoreFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreFile")
            .field("path", &self.path)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// `<path>.lock`.
pub(super) fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Open (creating `0600` if needed) and exclusively lock `<path>.lock`.
fn acquire_lock(path: &Path) -> Result<File, SealedStoreError> {
    use fs4::FileExt;

    let lock_path = lock_path(path);
    let mode = harw_fsutil::OpenMode {
        read: true,
        write: true,
        create: true,
        mode: 0o600,
        ..harw_fsutil::OpenMode::read_only()
    };
    let lock = harw_fsutil::open_nofollow(&lock_path, mode)
        .map_err(SealedStoreError::io("open lock file", &lock_path))?;
    FileExt::try_lock(&lock).map_err(|error| match error {
        fs4::TryLockError::WouldBlock => SealedStoreError::Locked(lock_path.clone()),
        fs4::TryLockError::Error(source) => SealedStoreError::Io {
            context: "lock store",
            path: lock_path.clone(),
            source,
        },
    })?;
    Ok(lock)
}

impl StoreFile {
    /// Path of the store file.
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Lock and load an existing store.
    ///
    /// Errors: locked → `Locked`; missing → `NotFound`; symlink, not a
    /// regular file, foreign owner or group/other bits → `Io`; larger than
    /// 64 MiB → `TooLarge`; header → `UnsupportedFormat`; tampering or
    /// wrong KEK → `Authentication`; inconsistent body → `Corrupt`.
    pub(super) fn open(path: &Path, kek: &Kek) -> Result<(Self, KeyTable), SealedStoreError> {
        let lock = acquire_lock(path)?;
        if let Err(error) = std::fs::symlink_metadata(path) {
            if error.kind() == std::io::ErrorKind::NotFound {
                return Err(SealedStoreError::NotFound(path.to_path_buf()));
            }
        }
        let file_key = kek.file_key();
        let bytes = read_store_file(path)?;
        let body = format::open_file(&file_key, &bytes)?;
        let (generation, table) = format::decode_body(&body)?;
        Ok((
            Self {
                path: path.to_path_buf(),
                file_key,
                generation,
                _lock: lock,
            },
            table,
        ))
    }

    /// Lock and create a new, empty store (written immediately, `0600`).
    ///
    /// Errors: locked → `Locked`; path exists → `AlreadyExists`; write → `Io`.
    pub(super) fn create(path: &Path, kek: &Kek) -> Result<(Self, KeyTable), SealedStoreError> {
        let lock = acquire_lock(path)?;
        if std::fs::symlink_metadata(path).is_ok() {
            return Err(SealedStoreError::AlreadyExists(path.to_path_buf()));
        }
        let mut file = Self {
            path: path.to_path_buf(),
            file_key: kek.file_key(),
            generation: 0,
            _lock: lock,
        };
        let table = KeyTable::default();
        file.save(&table)?;
        Ok((file, table))
    }

    /// Seal and atomically write `table` as the next generation.
    pub(super) fn save(&mut self, table: &KeyTable) -> Result<(), SealedStoreError> {
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(SealedStoreError::Corrupt("generation overflow"))?;
        let body = format::encode_body(table, generation)?;
        let sealed = format::seal_file(&self.file_key, &body)?;
        drop(body);
        harw_fsutil::write_atomic(
            &self.path,
            &sealed,
            harw_fsutil::AtomicWriteOptions::private(),
        )
        .map_err(SealedStoreError::io("write store", &self.path))?;
        self.generation = generation;
        Ok(())
    }
}

/// Read the store file without following a final symlink, after checking
/// type, owner and mode on the opened descriptor.
fn read_store_file(path: &Path) -> Result<Vec<u8>, SealedStoreError> {
    let file = harw_fsutil::open_nofollow(path, harw_fsutil::OpenMode::read_only())
        .map_err(SealedStoreError::io("open store", path))?;
    harw_fsutil::ensure_private_regular(&file)
        .map_err(SealedStoreError::io("check store permissions", path))?;
    let len = file
        .metadata()
        .map_err(SealedStoreError::io("stat store", path))?
        .len();
    if len > MAX_STORE_FILE_LEN {
        return Err(SealedStoreError::TooLarge {
            path: path.to_path_buf(),
            len,
        });
    }
    let mut bytes = Vec::new();
    file.take(MAX_STORE_FILE_LEN.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(SealedStoreError::io("read store", path))?;
    let read = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if read > MAX_STORE_FILE_LEN {
        return Err(SealedStoreError::TooLarge {
            path: path.to_path_buf(),
            len: read,
        });
    }
    Ok(bytes)
}
