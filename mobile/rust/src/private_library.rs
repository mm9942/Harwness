//! A local, bounded Library catalogue for an authenticated native vault owner.
//! No default path, caller-provided FFI key, plaintext fallback, or cloud adapter.

use std::{
    fs::{File, TryLockError},
    io::{self, Read},
    os::{
        fd::AsFd,
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use crypt_guard::{
    pq_hpke::{Aead, HpkeEnvelope, Kdf, Kem, RecipientKeyPair, Suite, derive_recipient_key_pair},
    sign::{
        SignAlgorithm,
        ml_dsa::{MlDsa65Impl, MlDsaSignature, MlDsaSigningKey, MlDsaVerifyingKey},
    },
};
use harw_fsutil::{
    AtomicWriteOptions, AtomicWriteOutcome, OpenMode, ensure_owned_directory,
    ensure_private_regular, open_beneath, open_dir_nofollow, write_atomic_with_outcome,
};
use zeroize::Zeroizing;

const SUITE: Suite = Suite::new(Kem::MlKem1024, Kdf::Shake256, Aead::XChaCha20Poly1305);
const INFO: &[u8] = b"harw/android/vault/private-library/cgh3/v1";
const MAGIC: &[u8; 8] = b"HLIB\x01\0\0\0";
const KEY_PROOF: &[u8] = b"harw/android/vault/private-library/signer-binding/v1";
const SIG_BYTES: usize = 3309;
const BODY_LIMIT: usize = 12 * 1024 * 1024;
const FILE_LIMIT: usize = BODY_LIMIT + 8 * 1024;
const MAX_ITEMS: usize = 64;
const MAX_IMAGE: usize = 180_000;
const MAX_NAME: usize = 160;
const MAX_DESCRIPTION: usize = 4096;
const CATALOGUE: &str = "sealed-library-v1.cgr";
const TX_LOCK: &str = ".library-txn-v1";

#[derive(Debug)]
pub(crate) enum LibraryError {
    Io(io::ErrorKind),
    Locked,
    Busy,
    BadScope,
    InvalidInput,
    Corrupt,
    Crypto,
    NotFound,
    Conflict,
    Full,
    UncertainCommit,
}

impl From<io::Error> for LibraryError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct LibraryScope {
    profile: [u8; 32],
    vault: [u8; 16],
    generation: u64,
}

impl LibraryScope {
    /// Only the native authenticated owner can select the current profile/generation.
    pub(crate) fn authenticated(
        profile: [u8; 32], vault: [u8; 16], generation: u64,
    ) -> Result<Self, LibraryError> {
        if profile == [0; 32] || vault == [0; 16] || generation == 0 {
            return Err(LibraryError::BadScope);
        }
        Ok(Self { profile, vault, generation })
    }

    fn aad(self) -> [u8; 64] {
        let mut aad = [0; 64];
        aad[..8].copy_from_slice(MAGIC);
        aad[8..40].copy_from_slice(&self.profile);
        aad[40..56].copy_from_slice(&self.vault);
        aad[56..].copy_from_slice(&self.generation.to_be_bytes());
        aad
    }
}

/// This material is separate from UIA room seal keys. The native owner supplies
/// an independently enrolled Library signing seed and its authenticated pinned
/// verifier. Neither is ever read from the untrusted catalogue or from Kotlin.
pub(crate) struct AuthenticatedLibraryKeys {
    scope: LibraryScope,
    recipient: RecipientKeyPair,
    signer: MlDsaSigningKey,
    verifier: MlDsaVerifyingKey,
}

impl AuthenticatedLibraryKeys {
    /// The authenticated high-entropy vault root is borrowed only to derive a
    /// distinctly scoped Library recipient key; it is not retained.
    pub(crate) fn from_owner(
        scope: LibraryScope,
        unlock_root: &Zeroizing<[u8; 32]>,
        library_signing_seed: Zeroizing<[u8; 32]>,
        pinned_library_verifier: Vec<u8>,
    ) -> Result<Self, LibraryError> {
        if **unlock_root == [0; 32] || *library_signing_seed == [0; 32] ||
            pinned_library_verifier.len() != 1952
        {
            return Err(LibraryError::BadScope);
        }
        let signer = MlDsaSigningKey::from_bytes(library_signing_seed.to_vec());
        let verifier = MlDsaVerifyingKey::from_bytes(pinned_library_verifier);
        let proof = MlDsa65Impl::sign(&signer, KEY_PROOF).map_err(|_| LibraryError::BadScope)?;
        MlDsa65Impl::verify(&verifier, KEY_PROOF, &proof).map_err(|_| LibraryError::BadScope)?;
        let mut derive = Zeroizing::new(blake3::Hasher::new_derive_key(
            "harw android vault v1 Library ML-KEM-1024 seed",
        ));
        derive.update(unlock_root.as_ref());
        derive.update(&scope.aad());
        let seed = Zeroizing::new(*derive.finalize().as_bytes());
        let recipient =
            derive_recipient_key_pair(SUITE.kem(), seed.as_ref()).map_err(|_| LibraryError::Crypto)?;
        Ok(Self { scope, recipient, signer, verifier })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct LibraryItemId([u8; 16]);

impl LibraryItemId {
    pub(crate) fn parse(bytes: [u8; 16]) -> Result<Self, LibraryError> {
        if bytes == [0; 16] { Err(LibraryError::InvalidInput) } else { Ok(Self(bytes)) }
    }

    pub(crate) fn bytes(self) -> [u8; 16] {
        self.0
    }
}

pub(crate) struct LibraryInfo {
    pub id: LibraryItemId,
    pub name: Zeroizing<String>,
    pub description: Option<Zeroizing<String>>,
}

pub(crate) struct LibrarySnapshot {
    pub revision: u64,
    pub items: Vec<LibraryInfo>,
}

/// Opaque native selection, redeemable once by the same authorised owner.
/// Contains no image or caption bytes and is not a normal chat draft.
pub(crate) struct ScopedLibrarySelection {
    scope: LibraryScope,
    revision: u64,
    ids: Vec<LibraryItemId>,
}


struct Asset {
    info: LibraryInfo,
    jpeg: Zeroizing<Vec<u8>>,
}

struct Catalogue {
    revision: u64,
    items: Vec<Asset>,
}

/// A handle for one authenticated local profile and one key generation.
/// Root must be independently provisioned for Library inside credential-
/// protected/no-backup storage. Do not point it at an existing UIA directory.
pub(crate) struct PrivateLibraryStore {
    root: PathBuf,
    root_fd: File,
    keys: Mutex<Option<Arc<AuthenticatedLibraryKeys>>>,
    revoked: AtomicBool,
}

impl PrivateLibraryStore {
    pub(crate) fn open(root: PathBuf, keys: AuthenticatedLibraryKeys) -> Result<Self, LibraryError> {
        let store = Self::open_owner_directory(root, keys)?;
        // Missing, corrupt or wrong-scope storage is never an empty library.
        store.with_catalogue(|_, _| Ok(()))?;
        Ok(store)
    }

    /// Explicit authenticated new-only enrollment. The owner must hold its
    /// own durable provisioning reservation and must never fall back to this
    /// after failure opening an existing profile.
    pub(crate) fn provision_new(
        root: PathBuf, keys: AuthenticatedLibraryKeys,
    ) -> Result<Self, LibraryError> {
        let store = Self::open_owner_directory(root, keys)?;
        store.with_transaction(|keys| {
            if store.load(keys)?.is_some() { return Err(LibraryError::Conflict); }
            store.commit(keys, &Catalogue { revision: 0, items: Vec::new() })
        })?;
        Ok(store)
    }

    fn open_owner_directory(root: PathBuf, keys: AuthenticatedLibraryKeys) -> Result<Self, LibraryError> {
        if !root.is_absolute() {
            return Err(LibraryError::BadScope);
        }
        let root_fd = owned_private_root(&root)?;
        Ok(Self {
            root, root_fd,
            keys: Mutex::new(Some(Arc::new(keys))),
            revoked: AtomicBool::new(false),
        })
    }

    /// Prevent new work and result delivery. Owner also discards late results
    /// and revokes the draft destination. A begun commit can already be visible.
    pub(crate) fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
        *self.keys.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    pub(crate) fn snapshot(&self) -> Result<LibrarySnapshot, LibraryError> {
        self.with_catalogue(|_, catalogue| {
            Ok(LibrarySnapshot {
                revision: catalogue.revision,
                items: catalogue.items.into_iter().map(|asset| asset.info).collect(),
            })
        })
    }

    /// A bridge should decode purpose-sized preview bytes directly into memory
    /// and never pass them to the normal-chat thumbnail cache or a disk cache.
    pub(crate) fn jpeg(
        &self, expected: u64, id: LibraryItemId,
    ) -> Result<Zeroizing<Vec<u8>>, LibraryError> {
        self.with_catalogue(|_, catalogue| {
            if catalogue.revision != expected { return Err(LibraryError::Conflict); }
            catalogue.items.into_iter().find(|item| item.info.id == id)
                .map(|item| item.jpeg).ok_or(LibraryError::NotFound)
        })
    }

    /// This is not the catalogue preview operation: call only with identity
    /// obtained from the exact authenticated room turn. Library metadata edits
    /// need not invalidate old turns; replacement bytes can never be returned.
    pub(crate) fn jpeg_for_room_photo(
        &self, photo: crate::uia_room_store::RoomPhotoIdentity,
    ) -> Result<Zeroizing<Vec<u8>>, LibraryError> {
        let id = LibraryItemId::parse(photo.item())?;
        let expected = photo.digest().ok_or(LibraryError::NotFound)?;
        self.with_catalogue(|_, catalogue| {
            let asset = catalogue.items.into_iter().find(|asset| asset.info.id == id)
                .ok_or(LibraryError::NotFound)?;
            if blake3::hash(&asset.jpeg).as_bytes() != &expected { return Err(LibraryError::Conflict); }
            self.require_active()?;
            Ok(asset.jpeg)
        })
    }

    /// Read only the chosen saved description at its exact catalogue revision.
    /// No JPEG, path, provider request, or queued assistant turn leaves this store.
    pub(crate) fn saved_description(
        &self, expected: u64, id: LibraryItemId,
    ) -> Result<Option<Zeroizing<String>>, LibraryError> {
        self.with_catalogue(|_, catalogue| {
            if catalogue.revision != expected { return Err(LibraryError::Conflict); }
            let asset = catalogue.items.into_iter().find(|asset| asset.info.id == id)
                .ok_or(LibraryError::NotFound)?;
            self.require_active()?;
            Ok(asset.info.description)
        })
    }

    pub(crate) fn select_for_private_draft(
        &self, expected: u64, ids: &[LibraryItemId],
    ) -> Result<ScopedLibrarySelection, LibraryError> {
        if !(1..=4).contains(&ids.len()) ||
            ids.iter().enumerate().any(|(index, id)| ids[..index].contains(id))
        {
            return Err(LibraryError::InvalidInput);
        }
        self.with_catalogue(|keys, catalogue| {
            if catalogue.revision != expected { return Err(LibraryError::Conflict); }
            if ids.iter().any(|id| !catalogue.items.iter().any(|item| &item.info.id == id)) {
                return Err(LibraryError::NotFound);
            }
            self.require_active()?;
            Ok(ScopedLibrarySelection { scope: keys.scope, revision: expected, ids: ids.to_vec() })
        })
    }

    /// Consumes the native selection under the exact vault owner's scope. Only
    /// a vault-local sealed chat importer should call this, never Android JNI.
    pub(crate) fn consume_for_private_draft(
        &self, selection: ScopedLibrarySelection,
        accept: impl FnOnce(Vec<(LibraryInfo, Zeroizing<Vec<u8>>)>) -> Result<(), LibraryError>,
    ) -> Result<(), LibraryError> {
        self.with_catalogue(|keys, mut catalogue| {
            if keys.scope != selection.scope { return Err(LibraryError::BadScope); }
            if catalogue.revision != selection.revision { return Err(LibraryError::Conflict); }
            let mut media = Vec::with_capacity(selection.ids.len());
            for id in selection.ids {
                let index = catalogue.items.iter().position(|item| item.info.id == id)
                    .ok_or(LibraryError::NotFound)?;
                let asset = catalogue.items.remove(index);
                media.push((asset.info, asset.jpeg));
            }
            self.require_active()?;
            accept(media)
        })
    }

    pub(crate) fn insert(
        &self, expected: u64, name: &str, jpeg: Zeroizing<Vec<u8>>,
    ) -> Result<LibraryItemId, LibraryError> {
        let name = validated_name(name)?;
        validate_jpeg(&jpeg)?;
        self.mutate(expected, |catalogue| {
            if catalogue.items.len() >= MAX_ITEMS { return Err(LibraryError::Full); }
            let mut raw = [0; 16];
            SystemRandom::new().fill(&mut raw).map_err(|_| LibraryError::Crypto)?;
            let id = LibraryItemId::parse(raw).map_err(|_| LibraryError::Crypto)?;
            if catalogue.items.iter().any(|item| item.info.id == id) {
                return Err(LibraryError::Crypto);
            }
            catalogue.items.push(Asset { info: LibraryInfo { id, name, description: None }, jpeg });
            Ok(id)
        })
    }

    pub(crate) fn rename(
        &self, expected: u64, id: LibraryItemId, name: &str,
    ) -> Result<(), LibraryError> {
        let name = validated_name(name)?;
        self.mutate(expected, |catalogue| {
            asset(catalogue, id)?.info.name = name;
            Ok(())
        })
    }

    pub(crate) fn describe(
        &self, expected: u64, id: LibraryItemId, description: Option<&str>,
    ) -> Result<(), LibraryError> {
        let description = description.map(validated_description).transpose()?;
        self.mutate(expected, |catalogue| {
            asset(catalogue, id)?.info.description = description;
            Ok(())
        })
    }

    pub(crate) fn remove(&self, expected: u64, id: LibraryItemId) -> Result<(), LibraryError> {
        self.mutate(expected, |catalogue| {
            let at = catalogue.items.iter().position(|item| item.info.id == id)
                .ok_or(LibraryError::NotFound)?;
            catalogue.items.remove(at);
            Ok(())
        })
    }

    fn mutate<T>(
        &self,
        expected: u64,
        operation: impl FnOnce(&mut Catalogue) -> Result<T, LibraryError>,
    ) -> Result<T, LibraryError> {
        self.with_catalogue(|keys, mut catalogue| {
            if catalogue.revision != expected { return Err(LibraryError::Conflict); }
            let result = operation(&mut catalogue)?;
            catalogue.revision = catalogue.revision.checked_add(1).ok_or(LibraryError::Full)?;
            self.commit(keys, &catalogue)?;
            Ok(result)
        })
    }

    fn commit(
        &self, keys: &AuthenticatedLibraryKeys, catalogue: &Catalogue,
    ) -> Result<(), LibraryError> {
        let bytes = encode(keys, catalogue)?;
        self.confirm_root()?;
        self.require_active()?;
        match write_atomic_with_outcome(&self.root.join(CATALOGUE), &bytes, AtomicWriteOptions::private())? {
            AtomicWriteOutcome::Committed => self.root_fd.sync_all().map_err(|_| LibraryError::UncertainCommit)?,
            AtomicWriteOutcome::DurabilityUncertain(_) => return Err(LibraryError::UncertainCommit),
        }
        // Do not report an ordinary cancellation after a possible commit.
        self.require_active().map_err(|_| LibraryError::UncertainCommit)
    }

    fn with_catalogue<T>(
        &self,
        operation: impl FnOnce(&AuthenticatedLibraryKeys, Catalogue) -> Result<T, LibraryError>,
    ) -> Result<T, LibraryError> {
        self.with_transaction(|keys| {
            let catalogue = self.load(keys)?.ok_or(LibraryError::Corrupt)?;
            self.require_active()?;
            operation(keys, catalogue)
        })
    }

    fn with_transaction<T>(
        &self,
        operation: impl FnOnce(&AuthenticatedLibraryKeys) -> Result<T, LibraryError>,
    ) -> Result<T, LibraryError> {
        self.require_active()?;
        let keys = self.keys.lock().map_err(|_| LibraryError::Locked)?
            .as_ref().map(Arc::clone).ok_or(LibraryError::Locked)?;
        self.confirm_root()?;
        let lock = open_beneath(
            self.root_fd.as_fd(), Path::new(TX_LOCK),
            OpenMode { create: true, ..OpenMode::read_write() },
        )?;
        private_file(&lock)?;
        match lock.try_lock() {
            Ok(()) => (),
            Err(TryLockError::WouldBlock) => return Err(LibraryError::Busy),
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
        self.require_active()?;
        operation(&keys)
    }

    fn load(&self, keys: &AuthenticatedLibraryKeys) -> Result<Option<Catalogue>, LibraryError> {
        let file = match open_beneath(self.root_fd.as_fd(), Path::new(CATALOGUE), OpenMode::read_only()) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        private_file(&file)?;
        let size = usize::try_from(file.metadata()?.len()).map_err(|_| LibraryError::Corrupt)?;
        if !(MAGIC.len() + SIG_BYTES + 1..=FILE_LIMIT).contains(&size) {
            return Err(LibraryError::Corrupt);
        }
        let mut bytes = Vec::with_capacity(size);
        file.take(FILE_LIMIT as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() != size { return Err(LibraryError::Corrupt); }
        decode(keys, &bytes).map(Some)
    }

    fn require_active(&self) -> Result<(), LibraryError> {
        if self.revoked.load(Ordering::Acquire) { Err(LibraryError::Locked) } else { Ok(()) }
    }

    fn confirm_root(&self) -> Result<(), LibraryError> {
        let original = self.root_fd.metadata()?;
        let current = owned_private_root(&self.root)?.metadata()?;
        if current.dev() != original.dev() || current.ino() != original.ino() {
            return Err(LibraryError::BadScope);
        }
        Ok(())
    }
}

fn owned_private_root(path: &Path) -> Result<File, LibraryError> {
    let root = File::from(open_dir_nofollow(path)?);
    ensure_owned_directory(&root)?;
    if root.metadata()?.permissions().mode() & 0o077 != 0 { return Err(LibraryError::BadScope); }
    Ok(root)
}

fn private_file(file: &File) -> Result<(), LibraryError> {
    ensure_private_regular(file)?;
    if file.metadata()?.nlink() != 1 { return Err(LibraryError::BadScope); }
    Ok(())
}

fn asset(catalogue: &mut Catalogue, id: LibraryItemId) -> Result<&mut Asset, LibraryError> {
    catalogue.items.iter_mut().find(|item| item.info.id == id).ok_or(LibraryError::NotFound)
}

fn validated_name(raw: &str) -> Result<Zeroizing<String>, LibraryError> {
    let name = raw.trim();
    if name.is_empty() || name.len() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(LibraryError::InvalidInput);
    }
    Ok(Zeroizing::new(name.to_owned()))
}

fn validated_description(raw: &str) -> Result<Zeroizing<String>, LibraryError> {
    let clean = raw.trim();
    if clean.is_empty() || clean.len() > MAX_DESCRIPTION ||
        clean.chars().any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
    {
        return Err(LibraryError::InvalidInput);
    }
    Ok(Zeroizing::new(clean.to_owned()))
}

fn validate_jpeg(jpeg: &[u8]) -> Result<(), LibraryError> {
    if !(4..=MAX_IMAGE).contains(&jpeg.len()) || !jpeg.starts_with(&[0xff, 0xd8]) ||
        !jpeg.ends_with(&[0xff, 0xd9])
    {
        return Err(LibraryError::InvalidInput);
    }
    Ok(())
}

fn signing_digest(aad: &[u8], wire: &[u8]) -> blake3::Hash {
    let mut digest = blake3::Hasher::new_derive_key("harw android vault v1 Library ML-DSA-65 signature");
    digest.update(aad);
    digest.update(wire);
    digest.finalize()
}

fn encode(keys: &AuthenticatedLibraryKeys, value: &Catalogue) -> Result<Vec<u8>, LibraryError> {
    let body_len = value.items.iter().fold(10usize, |size, asset| {
        size.saturating_add(16 + 12)
            .saturating_add(asset.info.name.len())
            .saturating_add(asset.info.description.as_ref().map_or(0, |v| v.len()))
            .saturating_add(asset.jpeg.len())
    });
    if value.items.len() > MAX_ITEMS || body_len > BODY_LIMIT { return Err(LibraryError::Full); }
    // Preallocate exactly once; reallocation could leave a freed plaintext copy.
    let mut payload = Zeroizing::new(Vec::with_capacity(body_len));
    payload.extend_from_slice(&value.revision.to_be_bytes());
    let count = u16::try_from(value.items.len()).map_err(|_| LibraryError::Full)?;
    payload.extend_from_slice(&count.to_be_bytes());
    for asset in &value.items {
        payload.extend_from_slice(&asset.info.id.bytes());
        write_field(&mut payload, asset.info.name.as_bytes())?;
        write_field(&mut payload, asset.info.description.as_ref().map_or(&[][..], |v| v.as_bytes()))?;
        write_field(&mut payload, &asset.jpeg)?;
    }
    if payload.len() > BODY_LIMIT { return Err(LibraryError::Full); }
    let aad = keys.scope.aad();
    let wire = HpkeEnvelope::seal(SUITE, keys.recipient.public_key(), INFO, &aad, &payload)
        .map_err(|_| LibraryError::Crypto)?.try_to_bytes().map_err(|_| LibraryError::Crypto)?;
    let signature = MlDsa65Impl::sign(&keys.signer, signing_digest(&aad, &wire).as_bytes())
        .map_err(|_| LibraryError::Crypto)?;
    if signature.as_ref().len() != SIG_BYTES || MAGIC.len() + SIG_BYTES + wire.len() > FILE_LIMIT {
        return Err(LibraryError::Crypto);
    }
    Ok([MAGIC.as_slice(), signature.as_ref(), &wire].concat())
}

fn decode(keys: &AuthenticatedLibraryKeys, encrypted: &[u8]) -> Result<Catalogue, LibraryError> {
    let frame = encrypted.strip_prefix(MAGIC).ok_or(LibraryError::Corrupt)?;
    let (signature, wire) = frame.split_at_checked(SIG_BYTES).ok_or(LibraryError::Corrupt)?;
    let aad = keys.scope.aad();
    MlDsa65Impl::verify(&keys.verifier, signing_digest(&aad, wire).as_bytes(), &MlDsaSignature::from_bytes(signature.to_vec()))
        .map_err(|_| LibraryError::Corrupt)?;
    let envelope = HpkeEnvelope::from_bytes(wire).map_err(|_| LibraryError::Corrupt)?;
    if envelope.suite() != SUITE { return Err(LibraryError::Corrupt); }
    let plaintext = envelope.open_zeroizing(keys.recipient.private_key(), INFO, &aad)
        .map_err(|_| LibraryError::Corrupt)?;
    if plaintext.len() > BODY_LIMIT { return Err(LibraryError::Corrupt); }
    let mut cursor: &[u8] = &plaintext;
    let revision = u64::from_be_bytes(take(&mut cursor, 8)?.try_into().map_err(|_| LibraryError::Corrupt)?);
    let count = u16::from_be_bytes(take(&mut cursor, 2)?.try_into().map_err(|_| LibraryError::Corrupt)?) as usize;
    if count > MAX_ITEMS || (revision == 0 && count != 0) { return Err(LibraryError::Corrupt); }
    let mut items: Vec<Asset> = Vec::with_capacity(count);
    for _ in 0..count {
        let id = LibraryItemId::parse(take(&mut cursor, 16)?.try_into().map_err(|_| LibraryError::Corrupt)?)
            .map_err(|_| LibraryError::Corrupt)?;
        if items.iter().any(|item| item.info.id == id) { return Err(LibraryError::Corrupt); }
        let name = validated_name(read_text(&mut cursor, MAX_NAME)?).map_err(|_| LibraryError::Corrupt)?;
        let caption = read_text(&mut cursor, MAX_DESCRIPTION)?;
        let description = if caption.is_empty() { None } else {
            Some(validated_description(caption).map_err(|_| LibraryError::Corrupt)?)
        };
        let jpeg = read_field(&mut cursor, MAX_IMAGE)?;
        validate_jpeg(jpeg).map_err(|_| LibraryError::Corrupt)?;
        items.push(Asset { info: LibraryInfo { id, name, description }, jpeg: Zeroizing::new(jpeg.to_vec()) });
    }
    if !cursor.is_empty() { return Err(LibraryError::Corrupt); }
    Ok(Catalogue { revision, items })
}

fn write_field(payload: &mut Vec<u8>, bytes: &[u8]) -> Result<(), LibraryError> {
    if payload.len().saturating_add(4).saturating_add(bytes.len()) > BODY_LIMIT {
        return Err(LibraryError::Full);
    }
    payload.extend_from_slice(&u32::try_from(bytes.len()).map_err(|_| LibraryError::Full)?.to_be_bytes());
    payload.extend_from_slice(bytes);
    Ok(())
}

fn take<'a>(cursor: &mut &'a [u8], size: usize) -> Result<&'a [u8], LibraryError> {
    let (part, remaining) = cursor.split_at_checked(size).ok_or(LibraryError::Corrupt)?;
    *cursor = remaining;
    Ok(part)
}

fn read_field<'a>(cursor: &mut &'a [u8], max: usize) -> Result<&'a [u8], LibraryError> {
    let size = u32::from_be_bytes(take(cursor, 4)?.try_into().map_err(|_| LibraryError::Corrupt)?) as usize;
    if size > max { return Err(LibraryError::Corrupt); }
    take(cursor, size)
}

fn read_text<'a>(cursor: &mut &'a [u8], max: usize) -> Result<&'a str, LibraryError> {
    std::str::from_utf8(read_field(cursor, max)?).map_err(|_| LibraryError::Corrupt)
}
