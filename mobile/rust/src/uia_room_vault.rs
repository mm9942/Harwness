//! Android-private UIA vault adapter (non-public, explicit opt-in).
//! Production construction requires authenticated device-sealed key custody;
//! this module does not provision keys, authorize PINs or expose an FFI.

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

use crypt_guard::{
    pq_hpke::{Aead, HpkeEnvelope, Kdf, Kem, RecipientKeyPair, RecipientPublicKey, Suite, derive_recipient_key_pair},
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

use crate::uia_room_store::{CompletedTurn, RoomId, RoomLog, RoomVault};

#[cfg(any(feature = "vault-uia-chat", test))]
mod memory;
#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
mod turn_attempt;
#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
pub(crate) use turn_attempt::RoomCompletionMailbox;
#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
mod cloud_work;
#[cfg(any(feature = "vault-uia-chat", test))]
pub(crate) use memory::{MEMORY_WIRE, MemoryChange};

const SUITE: Suite = Suite::new(Kem::MlKem1024, Kdf::Shake256, Aead::XChaCha20Poly1305);
const INFO: &[u8] = b"harw/android/vault/uia-room/cgh3/v1";
const MAGIC: &[u8; 8] = b"HUAR\x01\0\0\0";
const SIGNATURE_BYTES: usize = 3309; // FIPS 204 ML-DSA-65.
const MAX_TURNS: usize = 128;
// Old text-only records do not contain an attachment extension. The
// optional, encrypted sparse extension stores at most one Library ID per turn.
const PHOTO_FINGERPRINTED: u16 = 1 << 15;
const MAX_PLAIN_BYTES: usize = (256 * 1024) + 8 + 2 + (8 * MAX_TURNS) + 2 + 50 * MAX_TURNS;
const MAX_FILE_BYTES: usize = MAX_PLAIN_BYTES + 8 * 1024;
const UNCHANGED_SIGNATURE_KEY_DOMAIN: &[u8] = b"harw/android/vault/uia-room/signer-binding/v1";

#[derive(Debug)]
pub(crate) enum VaultError {
    Io(io::ErrorKind),
    UncertainCommit,
    InvalidKeyScope,
    Locked,
    Busy,
    Crypto,
    Corrupt,
}

impl From<io::Error> for VaultError {
    fn from(value: io::Error) -> Self {
        // Do not expose paths, crypto internals or decrypted content.
        Self::Io(value.kind())
    }
}

/// Trusted opaque values supplied by a future local PIN-gated component.
/// Do not expose a bridge which accepts an arbitrary caller-supplied value.
pub(crate) struct UnwrappedMaterial {
    root_secret: Zeroizing<[u8; 32]>,
    vault_id: [u8; 16],
    generation: u64,
    // Fresh signer generated separately and wrapped by the pinned PIN vault.
    // Never derive a signature key from the KEM key or trust keys in a record.
    signing_key: MlDsaSigningKey,
    trusted_verifier: MlDsaVerifyingKey,
}

impl UnwrappedMaterial {
    /// Trusted native owner handoff, not proof of authentication. Inputs
    /// must have been authenticated for the same device/vault scope. Do not
    /// expose this constructor or caller-supplied key bytes in an FFI.
    pub(crate) fn from_authenticated_bytes(
        root_secret: Zeroizing<[u8; 32]>,
        vault_id: [u8; 16],
        generation: u64,
        signing_seed: Zeroizing<[u8; 32]>,
        trusted_verifier: Vec<u8>,
    ) -> Result<Self, VaultError> {
        if *root_secret == [0; 32]
            || *signing_seed == [0; 32]
            || vault_id == [0; 16]
            || generation == 0
            || trusted_verifier.len() != 1952
        // FIPS 204 ML-DSA-65.
        {
            return Err(VaultError::InvalidKeyScope);
        }
        Ok(Self {
            root_secret,
            vault_id,
            generation,
            signing_key: MlDsaSigningKey::from_bytes(signing_seed.to_vec()),
            trusted_verifier: MlDsaVerifyingKey::from_bytes(trusted_verifier),
        })
    }
}

pub(crate) struct SealedRoomVault {
    root: PathBuf,
    root_fd: File,
    id: RoomId,
    scope: [u8; 24],
    revoked: AtomicBool,
    keys: Mutex<Option<Arc<SealingKeys>>>,
}

struct SealingKeys {
    recipient: RecipientKeyPair,
    signing_key: Arc<MlDsaSigningKey>,
    trusted_verifier: MlDsaVerifyingKey,
}

impl SealedRoomVault {
    /// The directory must be already provisioned inside the app-private store,
    /// under a trusted path immutable to any guest or delegated worker.
    /// The caller must revoke/drop the returned handle on Harw lock and never
    /// issue it except after cryptographic PIN (or enrolled biometric) unwrap.
    pub(crate) fn open_bound_room(
        root: PathBuf,
        id: RoomId,
        material: UnwrappedMaterial,
    ) -> Result<Self, VaultError> {
        if !root.is_absolute() || material.vault_id == [0; 16] || material.generation == 0 {
            return Err(VaultError::InvalidKeyScope);
        }
        let dir = File::from(open_dir_nofollow(&root)?);
        ensure_owned_directory(&dir)?;
        if dir.metadata()?.permissions().mode() & 0o077 != 0 {
            return Err(VaultError::InvalidKeyScope);
        }
        let signature = MlDsa65Impl::sign(&material.signing_key, UNCHANGED_SIGNATURE_KEY_DOMAIN)
            .map_err(|_| VaultError::InvalidKeyScope)?;
        MlDsa65Impl::verify(
            &material.trusted_verifier,
            UNCHANGED_SIGNATURE_KEY_DOMAIN,
            &signature,
        )
        .map_err(|_| VaultError::InvalidKeyScope)?;
        let mut scope = [0; 24];
        scope[..16].copy_from_slice(&material.vault_id);
        scope[16..].copy_from_slice(&material.generation.to_be_bytes());
        let mut derivation = Zeroizing::new(blake3::Hasher::new_derive_key(
            "harw android vault v1 UIA ML-KEM-1024 seed",
        ));
        derivation.update(material.root_secret.as_ref());
        derivation.update(&scope);
        let seed = Zeroizing::new(*derivation.finalize().as_bytes());
        let recipient = derive_recipient_key_pair(SUITE.kem(), seed.as_ref())
            .map_err(|_| VaultError::Crypto)?;
        Ok(Self {
            root,
            root_fd: dir,
            id,
            scope,
            revoked: AtomicBool::new(false),
            keys: Mutex::new(Some(Arc::new(SealingKeys {
                recipient,
                signing_key: Arc::new(material.signing_key),
                trusted_verifier: material.trusted_verifier,
            }))),
        })
    }

    /// Immediately revoke every reference to this handle. Cancel associated
    /// jobs and discard late results in the controller first. A crypto op
    /// that already pinned keys drops its short-lived Arc on return; this
    /// method does not wait for any crypto or file operation. No re-unlock.
    pub(crate) fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
        let mut keys = self.keys.lock().unwrap_or_else(PoisonError::into_inner);
        *keys = None;
    }

    fn require_active(&self) -> Result<(), VaultError> {
        if self.revoked.load(Ordering::Acquire) {
            return Err(VaultError::Locked);
        }
        Ok(())
    }

    fn active_keys(&self) -> Result<Arc<SealingKeys>, VaultError> {
        self.require_active()?;
        self.keys
            .lock()
            .map_err(|_| VaultError::Locked)?
            .as_ref()
            .map(Arc::clone)
            .ok_or(VaultError::Locked)
    }

    fn aad(&self, id: &RoomId) -> Vec<u8> {
        [MAGIC.as_slice(), &self.scope, id.as_str().as_bytes()].concat()
    }

    fn record_name(id: &RoomId) -> PathBuf {
        // Only the authenticated private room handle supplies IDs. On-disk
        // names are opaque to readers without that random room ID.
        PathBuf::from(format!("{}.cgr", blake3::hash(id.as_str().as_bytes())))
    }

    fn path(&self, id: &RoomId) -> PathBuf {
        self.root.join(Self::record_name(id))
    }

    fn confirm_private_root(&self) -> Result<(), VaultError> {
        Self::confirm_pinned_root(&self.root, &self.root_fd)
    }

    fn confirm_pinned_root(path: &Path, pinned: &File) -> Result<(), VaultError> {
        let expected = pinned.metadata()?;
        let candidate = File::from(open_dir_nofollow(path)?);
        ensure_owned_directory(&candidate)?;
        let candidate = candidate.metadata()?;
        if candidate.dev() != expected.dev()
            || candidate.ino() != expected.ino()
            || candidate.mode() & 0o077 != 0
        {
            return Err(VaultError::InvalidKeyScope);
        }
        Ok(())
    }

    fn lock(&self) -> Result<File, VaultError> {
        self.lock_mode(true)
    }

    fn lock_mode(&self, create: bool) -> Result<File, VaultError> {
        Self::lock_pinned(&self.root_fd, create)
    }

    fn lock_pinned(dir: &File, create: bool) -> Result<File, VaultError> {
        let lock = open_beneath(
            dir.as_fd(),
            Path::new(".txn"),
            OpenMode {
                create,
                ..OpenMode::read_write()
            },
        )?;
        check_private_file(&lock)?;
        match lock.try_lock() {
            Ok(()) => Ok(lock),
            Err(TryLockError::WouldBlock) => Err(VaultError::Busy),
            Err(TryLockError::Error(err)) => Err(err.into()),
        }
    }

    /// Existing PIN/recovery handles never create a missing transaction lock.
    pub(crate) fn load_existing(&self, id: &RoomId) -> Result<Option<RoomLog>, VaultError> {
        self.load_with_lock(id, false)
    }

    fn load_with_lock(&self, id: &RoomId, allow_initial_lock: bool) -> Result<Option<RoomLog>, VaultError> {
        if id != &self.id {
            return Err(VaultError::InvalidKeyScope);
        }
        let keys = self.active_keys()?;
        self.confirm_private_root()?;
        let _exclusive = self.lock_mode(allow_initial_lock)?;
        self.require_active()?;
        let record = self.load_record(&keys, id);
        self.require_active()?;
        record
    }

    fn load_record(&self, keys: &SealingKeys, id: &RoomId) -> Result<Option<RoomLog>, VaultError> {
        self.read_record_frame(id)?
            .map(|sealed| self.decode_record(keys, id, &sealed))
            .transpose()
    }

    /// Bounded private ciphertext read. No transcript authority: callers
    /// wanting data must authenticate it with `decode_record`.
    fn read_record_frame(&self, id: &RoomId) -> Result<Option<Vec<u8>>, VaultError> {
        let file = match open_beneath(
            self.root_fd.as_fd(),
            &Self::record_name(id),
            OpenMode::read_only(),
        ) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        check_private_file(&file)?;
        let size = usize::try_from(file.metadata()?.len()).map_err(|_| VaultError::Corrupt)?;
        if !(MAGIC.len() + SIGNATURE_BYTES + 1..=MAX_FILE_BYTES).contains(&size) {
            return Err(VaultError::Corrupt);
        }
        let mut bytes = Vec::with_capacity(size);
        file.take(MAX_FILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() != size {
            return Err(VaultError::Corrupt);
        }
        Ok(Some(bytes))
    }

    fn decode_record(
        &self,
        keys: &SealingKeys,
        id: &RoomId,
        bytes: &[u8],
    ) -> Result<RoomLog, VaultError> {
        let frame = bytes.strip_prefix(MAGIC).ok_or(VaultError::Corrupt)?;
        let (sig, wire) = frame
            .split_at_checked(SIGNATURE_BYTES)
            .ok_or(VaultError::Corrupt)?;
        let aad = self.aad(id);
        let signed = signature_digest(&aad, wire);
        let signature = MlDsaSignature::from_bytes(sig.to_vec());
        MlDsa65Impl::verify(&keys.trusted_verifier, signed.as_bytes(), &signature)
            .map_err(|_| VaultError::Corrupt)?;
        let envelope = HpkeEnvelope::from_bytes(wire).map_err(|_| VaultError::Corrupt)?;
        if envelope.suite() != SUITE {
            return Err(VaultError::Corrupt);
        }
        let payload = envelope
            .open_zeroizing(keys.recipient.private_key(), INFO, &aad)
            .map_err(|_| VaultError::Corrupt)?;
        decode_room(id, &payload)
    }

    fn encode_record(&self, keys: &SealingKeys, room: &RoomLog) -> Result<Vec<u8>, VaultError> {
        Self::seal_room_record(keys.recipient.public_key(), &keys.signing_key, &self.aad(room.id()), room)
    }

    // Identical authenticated frame for foreground and restricted completion.
    // In particular no alternate signer or verifier is accepted on disk.
    fn seal_room_record(
        recipient: &RecipientPublicKey, signer: &MlDsaSigningKey, aad: &[u8], room: &RoomLog,
    ) -> Result<Vec<u8>, VaultError> {
        let attachment_count = room.completed_turns().iter().filter(|turn| turn.library_image().is_some()).count();
        let attachment_bytes = if attachment_count == 0 { 0 } else { 2 + 50 * attachment_count };
        let payload_size = 10 + attachment_bytes
            + room
                .completed_turns()
                .iter()
                .map(|turn| 8 + turn.user().len() + turn.answer().len())
                .sum::<usize>();
        if payload_size > MAX_PLAIN_BYTES {
            return Err(VaultError::Corrupt);
        }
        let mut payload = Zeroizing::new(Vec::with_capacity(payload_size));
        payload.extend_from_slice(&room.revision().to_be_bytes());
        let turn_count =
            u16::try_from(room.completed_turns().len()).map_err(|_| VaultError::Corrupt)?;
        payload.extend_from_slice(&turn_count.to_be_bytes());
        for turn in room.completed_turns() {
            for field in [turn.user(), turn.answer()] {
                let len = u32::try_from(field.len()).map_err(|_| VaultError::Corrupt)?;
                payload.extend_from_slice(&len.to_be_bytes());
                payload.extend_from_slice(field.as_bytes());
            }
        }
        if attachment_count != 0 {
            payload.extend_from_slice(&((attachment_count as u16) | PHOTO_FINGERPRINTED).to_be_bytes());
            for (index, turn) in room.completed_turns().iter().enumerate() {
                if let Some(photo) = turn.library_image() {
                    payload.extend_from_slice(&(index as u16).to_be_bytes());
                    payload.extend_from_slice(&photo.item());
                    // Legacy ID-only entries remain readable, but cannot be previewed.
                    payload.extend_from_slice(&photo.digest().unwrap_or([0; 32]));
                }
            }
        }
        if payload.len() > MAX_PLAIN_BYTES {
            return Err(VaultError::Corrupt);
        }
        // Revision stays encrypted and authenticated inside the payload. It
        // is not exposed as clear AAD. The AAD pins format, vault generation,
        // record kind (the frame magic and INFO), and exact room.
        let wire = HpkeEnvelope::seal(SUITE, recipient, INFO, aad, &payload)
            .map_err(|_| VaultError::Crypto)?
            .try_to_bytes()
            .map_err(|_| VaultError::Crypto)?;
        let sig = MlDsa65Impl::sign(signer, signature_digest(aad, &wire).as_bytes())
            .map_err(|_| VaultError::Crypto)?;
        if sig.as_ref().len() != SIGNATURE_BYTES
            || MAGIC.len() + SIGNATURE_BYTES + wire.len() > MAX_FILE_BYTES
        {
            return Err(VaultError::Crypto);
        }
        Ok([MAGIC.as_slice(), sig.as_ref(), &wire].concat())
    }
}

fn check_private_file(file: &File) -> Result<(), VaultError> {
    ensure_private_regular(file)?;
    if file.metadata()?.nlink() != 1 {
        return Err(VaultError::InvalidKeyScope);
    }
    Ok(())
}

fn signature_digest(aad: &[u8], wire: &[u8]) -> blake3::Hash {
    let mut transcript =
        blake3::Hasher::new_derive_key("harw android vault v1 UIA ML-DSA-65 signature");
    transcript.update(&(aad.len() as u64).to_be_bytes());
    transcript.update(aad);
    transcript.update(wire);
    transcript.finalize()
}

fn decode_room(id: &RoomId, mut payload: &[u8]) -> Result<RoomLog, VaultError> {
    fn bytes<'a, const N: usize>(source: &mut &'a [u8]) -> Result<[u8; N], VaultError> {
        let (value, rest) = source.split_at_checked(N).ok_or(VaultError::Corrupt)?;
        *source = rest;
        value.try_into().map_err(|_| VaultError::Corrupt)
    }
    fn field<'a>(source: &mut &'a [u8], max: usize) -> Result<&'a str, VaultError> {
        let count =
            usize::try_from(u32::from_be_bytes(bytes(source)?)).map_err(|_| VaultError::Corrupt)?;
        if count > max {
            return Err(VaultError::Corrupt);
        }
        let (value, rest) = source.split_at_checked(count).ok_or(VaultError::Corrupt)?;
        *source = rest;
        std::str::from_utf8(value).map_err(|_| VaultError::Corrupt)
    }
    if payload.len() > MAX_PLAIN_BYTES {
        return Err(VaultError::Corrupt);
    }
    let revision = u64::from_be_bytes(bytes(&mut payload)?);
    let count = usize::from(u16::from_be_bytes(bytes(&mut payload)?));
    if count > MAX_TURNS {
        return Err(VaultError::Corrupt);
    }
    let mut text = Vec::with_capacity(count);
    for _ in 0..count {
        text.push((field(&mut payload, 8 * 1024)?, field(&mut payload, 16 * 1024)?));
    }
    let mut attachments = [None; MAX_TURNS];
    if !payload.is_empty() {
        let tagged = u16::from_be_bytes(bytes(&mut payload)?);
        let fingerprinted = tagged & PHOTO_FINGERPRINTED != 0;
        let attached = usize::from(tagged & !PHOTO_FINGERPRINTED);
        if !(1..=count).contains(&attached) { return Err(VaultError::Corrupt); }
        let mut next_index = 0;
        for _ in 0..attached {
            let index = usize::from(u16::from_be_bytes(bytes(&mut payload)?));
            if index < next_index || index >= count { return Err(VaultError::Corrupt); }
            let item = bytes::<16>(&mut payload)?;
            let digest = if fingerprinted {
                let value = bytes::<32>(&mut payload)?;
                (value != [0; 32]).then_some(value)
            } else { None };
            attachments[index] = Some(crate::uia_room_store::RoomPhotoIdentity::new(item, digest)
                .map_err(|_| VaultError::Corrupt)?);
            next_index = index + 1;
        }
    }
    if !payload.is_empty() { return Err(VaultError::Corrupt); }
    let turns = text.into_iter().zip(attachments)
        .map(|((user, answer), image)| CompletedTurn::with_library_image(user, answer, image))
        .collect::<Result<_, _>>().map_err(|_| VaultError::Corrupt)?;
    RoomLog::restore(id.clone(), revision, turns).map_err(|_| VaultError::Corrupt)
}

impl RoomVault for SealedRoomVault {
    type Error = VaultError;

    fn load(&self, id: &RoomId) -> Result<Option<RoomLog>, Self::Error> {
        self.load_with_lock(id, true)
    }

    fn compare_and_swap(
        &self,
        id: &RoomId,
        expected_revision: Option<u64>,
        next: &RoomLog,
    ) -> Result<bool, Self::Error> {
        if id != &self.id {
            return Err(VaultError::InvalidKeyScope);
        }
        if next.id() != id
            || next.revision()
                != expected_revision
                    .map_or(Some(0), |n| n.checked_add(1))
                    .ok_or(VaultError::Corrupt)?
        {
            return Err(VaultError::Corrupt);
        }
        let keys = self.active_keys()?;
        self.confirm_private_root()?;
        let _exclusive = if expected_revision.is_none() { self.lock()? } else { self.lock_mode(false)? };
        self.require_active()?;
        let current = self.load_record(&keys, id)?;
        self.require_active()?;
        if current.as_ref().map(RoomLog::revision) != expected_revision {
            return Ok(false);
        }
        if let Some(current) = current
            && !next
                .completed_turns()
                .starts_with(current.completed_turns())
        {
            return Err(VaultError::Corrupt);
        }
        let bytes = self.encode_record(&keys, next)?;
        // The helper's write path opens its parent; refuse stale roots before
        // writing. App-private path ownership is still a trust prerequisite.
        self.confirm_private_root()?;
        // The permission check precedes the irreversible commit. If a lock
        // follows it, the owner suppresses this turn and reconciles on reopen;
        // the store never returns a false cancellation after rename.
        self.require_active()?;
        match write_atomic_with_outcome(&self.path(id), &bytes, AtomicWriteOptions::private())? {
            AtomicWriteOutcome::Committed => {
                // Fail closed on filesystems that can't fsync the directory;
                // the shared atomic helper normally tolerates EINVAL here.
                self.root_fd
                    .sync_all()
                    .map_err(|_| VaultError::UncertainCommit)?;
                Ok(true)
            }
            AtomicWriteOutcome::DurabilityUncertain(_) => Err(VaultError::UncertainCommit),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uia_room_store::{commit_turn, open_room};
    use crypt_guard::kem::backend::OsRng;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fixture_dir() -> io::Result<tempfile::TempDir> {
        let temp = tempfile::tempdir()?;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
        Ok(temp)
    }

    fn material(
        secret: [u8; 32],
        id: [u8; 16],
        epoch: u64,
        signer: &[u8],
        verifier: &[u8],
    ) -> UnwrappedMaterial {
        UnwrappedMaterial {
            root_secret: Zeroizing::new(secret),
            vault_id: id,
            generation: epoch,
            signing_key: MlDsaSigningKey::from_bytes(signer.to_vec()),
            trusted_verifier: MlDsaVerifyingKey::from_bytes(verifier.to_vec()),
        }
    }

    fn room(a: bool) -> RoomId {
        RoomId::parse(if a {
            "uia-local-v1:12345678000000000000000000000000"
        } else {
            "uia-local-v1:abcdef98000000000000000000000000"
        })
        .expect("fixture room")
    }

    #[test]
    fn manual_memory_pins_only_a_committed_source_and_revokes_all_access() -> TestResult {
        let dir = fixture_dir()?;
        let (signing, verifier) = MlDsa65Impl::keypair(&mut OsRng)?;
        let id = room(true);
        let open = || SealedRoomVault::open_bound_room(
            dir.path().to_owned(), id.clone(),
            material([7; 32], [9; 16], 1, signing.as_bytes(), verifier.as_bytes()),
        ).expect("authorized store");
        let store = open();
        let empty = open_room(&store, &id).expect("initial room");
        assert!(store.manual_memory(0, Some(MemoryChange { expected_memory_revision: 0, ordinal: 1, save: true })).expect("bounded admission").is_none());
        commit_turn(&store, &empty, "secret source question", "secret source answer").expect("durable pair");
        let saved = store.manual_memory(1, Some(MemoryChange { expected_memory_revision: 0, ordinal: 1, save: true })).expect("save").expect("confirmed");
        assert_eq!(saved[8], 1);
        store.revoke();
        assert!(matches!(store.manual_memory(1, None), Err(VaultError::Locked)));
        let next = open();
        assert_eq!(next.manual_memory(1, None).expect("restart"), Some(saved));
        assert!(next.manual_memory(1, Some(MemoryChange { expected_memory_revision: 0, ordinal: 1, save: false })).expect("stale").is_none());
        let removed = next.manual_memory(1, Some(MemoryChange { expected_memory_revision: 1, ordinal: 1, save: false })).expect("delete").expect("confirmed");
        assert_eq!(removed[8], 0);
        assert_eq!(open().manual_memory(1, None).expect("deleted reference stays deleted"), Some(removed));
        let source = next.load(&id).expect("authorized").expect("still exists");
        assert_eq!(source.completed_turns()[0].answer(), "secret source answer");
        Ok(())
    }

    #[test]
    fn identity_bound_handle_cannot_open_another_room_even_in_same_vault() -> TestResult {
        let dir = fixture_dir()?;
        let (signing, verifier) = MlDsa65Impl::keypair(&mut OsRng)?;
        let seed: [u8; 32] = signing.as_bytes().try_into()?;
        let keys = UnwrappedMaterial::from_authenticated_bytes(
            Zeroizing::new([7; 32]),
            [9; 16],
            1,
            Zeroizing::new(seed),
            verifier.as_bytes().to_vec(),
        )
        .expect("valid key inputs");
        let bound = room(true);
        let other = room(false);
        let store = SealedRoomVault::open_bound_room(dir.path().to_owned(), bound.clone(), keys)
            .expect("create authorized handle");
        assert!(store.load(&bound).expect("belongs to this scope").is_none());
        assert!(matches!(
            store.load(&other),
            Err(VaultError::InvalidKeyScope)
        ));
        Ok(())
    }

    #[test]
    fn a_contended_store_fails_without_waiting_for_exclusive_file_lock() -> TestResult {
        let dir = fixture_dir()?;
        let (signing, verifier) = MlDsa65Impl::keypair(&mut OsRng)?;
        let store = SealedRoomVault::open_bound_room(
            dir.path().to_owned(),
            room(true),
            material([7; 32], [9; 16], 1, signing.as_bytes(), verifier.as_bytes()),
        )
        .expect("create authorized handle");
        let _held = store.lock().expect("holder");
        assert!(matches!(store.load(&room(true)), Err(VaultError::Busy)));
        Ok(())
    }

    #[test]
    fn roundtrip_restart_and_cas() -> TestResult {
        let dir = fixture_dir()?;
        let (signer, verifier) = MlDsa65Impl::keypair(&mut OsRng)?;
        let open = || {
            SealedRoomVault::open_bound_room(
                dir.path().to_owned(),
                room(true),
                material([7; 32], [9; 16], 1, signer.as_bytes(), verifier.as_bytes()),
            )
            .expect("open fixture vault")
        };
        let id = room(true);
        let stale = {
            let store = open();
            let empty = open_room(&store, &id).expect("new room");
            commit_turn(&store, &empty, "Canary question 💬", "Canary answer").expect("commit");
            empty
        };
        let path = open().path(&id);
        let wire = std::fs::read(&path)?;
        assert!(!wire.windows(6).any(|chunk| chunk == b"Canary"));
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
        let reopened = open();
        let loaded = open_room(&reopened, &id).expect("decrypt after reopen");
        assert_eq!(loaded.revision(), 1);
        assert_eq!(loaded.completed_turns()[0].answer(), "Canary answer");
        assert!(matches!(
            commit_turn(&reopened, &stale, "Stale", "Cannot replace"),
            Err(crate::uia_room_store::RoomFailure::Conflict)
        ));
        let (): () = reopened.revoke();
        reopened.revoke();
        assert!(matches!(reopened.load(&id), Err(VaultError::Locked)));
        assert_eq!(
            open()
                .load(&id)
                .expect("reauthorization")
                .expect("record")
                .revision(),
            1
        );
        Ok(())
    }

    #[test]
    fn wrong_key_scope_or_room_cannot_read_and_tamper_is_not_missing() -> TestResult {
        let dir = fixture_dir()?;
        let (signer, verifier) = MlDsa65Impl::keypair(&mut OsRng)?;
        let open = |secret, scope, epoch| {
            SealedRoomVault::open_bound_room(
                dir.path().to_owned(),
                room(true),
                material(secret, scope, epoch, signer.as_bytes(), verifier.as_bytes()),
            )
            .expect("open fixture vault")
        };
        let id = room(true);
        let original = open([7; 32], [9; 16], 1);
        open_room(&original, &id).expect("new room");
        let original_path = original.path(&id);
        let mut wire = std::fs::read(&original_path)?;
        for other in [
            open([8; 32], [9; 16], 1),
            open([7; 32], [8; 16], 1),
            open([7; 32], [9; 16], 2),
        ] {
            assert!(matches!(other.load(&id), Err(VaultError::Corrupt)));
        }
        let other_id = room(false);
        write_atomic_with_outcome(
            &original.path(&other_id),
            &wire,
            AtomicWriteOptions::private(),
        )?;
        let other_bound = SealedRoomVault::open_bound_room(
            dir.path().to_owned(),
            other_id.clone(),
            material([7; 32], [9; 16], 1, signer.as_bytes(), verifier.as_bytes()),
        )
        .expect("authorized separate room");
        assert!(matches!(
            other_bound.load(&other_id),
            Err(VaultError::Corrupt)
        ));

        *wire.last_mut().ok_or("empty record")? ^= 0x80;
        write_atomic_with_outcome(&original_path, &wire, AtomicWriteOptions::private())?;
        assert!(matches!(original.load(&id), Err(VaultError::Corrupt)));
        assert!(matches!(
            open_room(&original, &id),
            Err(crate::uia_room_store::RoomFailure::Vault(
                VaultError::Corrupt
            ))
        ));
        assert_eq!(std::fs::read(&original_path)?, wire);
        Ok(())
    }

    #[test]
    fn independent_handles_cannot_both_append_at_the_same_revision() -> TestResult {
        let dir = fixture_dir()?;
        let (signer, verifier) = MlDsa65Impl::keypair(&mut OsRng)?;
        let open = || {
            SealedRoomVault::open_bound_room(
                dir.path().to_owned(),
                room(true),
                material([7; 32], [9; 16], 1, signer.as_bytes(), verifier.as_bytes()),
            )
            .expect("open fixture vault")
        };
        let first = open();
        let second = open();
        let id = room(true);
        let empty = open_room(&first, &id).expect("new room");
        let gate = std::sync::Barrier::new(2);
        let (a, b) = std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                gate.wait();
                commit_turn(&first, &empty, "A", "Answer A")
            });
            let b = scope.spawn(|| {
                gate.wait();
                commit_turn(&second, &empty, "B", "Answer B")
            });
            (a.join().expect("writer A"), b.join().expect("writer B"))
        });
        assert_ne!(a.is_ok(), b.is_ok());
        let loser = if a.is_ok() { b } else { a };
        assert!(matches!(
            loser,
            Err(crate::uia_room_store::RoomFailure::Conflict)
                | Err(crate::uia_room_store::RoomFailure::Vault(VaultError::Busy))
        ));
        assert_eq!(
            open_room(&first, &id)
                .expect("stable persisted history")
                .revision(),
            1
        );
        Ok(())
    }

    #[test]
    fn refuses_symlink_and_mismatched_signer() -> TestResult {
        let dir = fixture_dir()?;
        let (signer, verifier) = MlDsa65Impl::keypair(&mut OsRng)?;
        let (other_signer, _) = MlDsa65Impl::keypair(&mut OsRng)?;
        assert!(matches!(
            SealedRoomVault::open_bound_room(
                dir.path().to_owned(),
                room(true),
                material(
                    [7; 32],
                    [9; 16],
                    1,
                    other_signer.as_bytes(),
                    verifier.as_bytes()
                )
            ),
            Err(VaultError::InvalidKeyScope)
        ));
        let store = SealedRoomVault::open_bound_room(
            dir.path().to_owned(),
            room(true),
            material([7; 32], [9; 16], 1, signer.as_bytes(), verifier.as_bytes()),
        )
        .expect("open fixture vault");
        let id = room(true);
        std::os::unix::fs::symlink(dir.path().join("absent"), store.path(&id))?;
        assert!(store.load(&id).is_err());
        Ok(())
    }
}
