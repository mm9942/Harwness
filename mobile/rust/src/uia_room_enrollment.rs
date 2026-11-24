//! Dormant local enrollment anchor. The root and device proof are delivered
//! only by a future trusted Android PIN/Keystore backend; no FFI exports here.

use std::{
    fs::File,
    io::{self, Read},
    os::{fd::AsFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

use crypt_guard::{
    kem::backend::OsRng,
    pq_hpke::{
        Aead, HpkeEnvelope, Kdf, Kem, RecipientKeyPair, Suite, derive_recipient_key_pair,
        generate_recipient_seed,
    },
    sign::{SignAlgorithm, ml_dsa::MlDsa65Impl},
};
use harw_fsutil::{
    AtomicWriteOptions, AtomicWriteOutcome, OpenMode, ensure_owned_directory,
    ensure_private_regular, open_beneath, open_dir_nofollow, rename_at, write_atomic_with_outcome,
};
use zeroize::Zeroizing;

use crate::{
    uia_room_handoff::{
        AuthenticatedRoomLease, DIRECT_HANDOFF_BYTES, RoomHandoffError, UiaRoomHandoff,
        VerifiedRoomEnrollment,
    },
    uia_room_store::{RoomId, RoomVault as _, open_room},
    uia_room_vault::{SealedRoomVault, UnwrappedMaterial},
};

const MAGIC: &[u8; 8] = b"HUAM\x01\0\0\0";
const INFO: &[u8] = b"harw/android/uia/enrollment-manifest/cgh3/v1";
const DEVICE_DOMAIN: &[u8] = b"harw/android/uia/enrollment-device-mac/v1";
const SUITE: Suite = Suite::new(Kem::MlKem1024, Kdf::Shake256, Aead::XChaCha20Poly1305);
const FINAL: &str = "enrollment-v1.cge";
const STAGING: &str = "enrollment-pending-v1.cge";
pub(crate) const ENROLLMENT_LOCK: &str = ".enrollment-lock";
const LEASE_LOCK: &str = ".session-lock";
const MAX_WIRE: usize = 8192;
const ROOM_ID_BYTES: usize = "uia-local-v1:".len() + 32;
const VERIFIER_BYTES: usize = 1952;
const PAYLOAD_BYTES: usize = ROOM_ID_BYTES + 32 + VERIFIER_BYTES;

#[derive(Debug)]
pub(crate) enum EnrollmentError {
    Io(io::ErrorKind),
    UncertainCommit,
    Scope,
    Missing,
    AlreadyProvisioned,
    Interrupted,
    Busy,
    Authentication,
    Crypto,
    Handoff(RoomHandoffError),
}
impl From<io::Error> for EnrollmentError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

struct Scope {
    vault: [u8; 16],
    generation: u64,
}

/// Already validated PIN slot output and non-exportable hardware MAC proof.
/// Private to the trusted app process. Not itself a proof verifier or JNI API.
pub(crate) struct VerifiedDeviceSecrets {
    scope: Scope,
    root: Zeroizing<[u8; 32]>,
    device_proof: Zeroizing<[u8; 32]>,
}

impl VerifiedDeviceSecrets {
    /// Expected hardware operation: HMAC-SHA256(context || PIN-unsealed root)
    /// via the exact device-bound Keystore slot. Separate purpose domain.
    pub(crate) fn device_mac_context(vault: [u8; 16], generation: u64) -> Vec<u8> {
        [DEVICE_DOMAIN, vault.as_slice(), &generation.to_be_bytes()].concat()
    }

    pub(crate) fn from_pin_unwrap(
        vault: [u8; 16],
        generation: u64,
        root: Zeroizing<[u8; 32]>,
        device_proof: Zeroizing<[u8; 32]>,
    ) -> Result<Self, EnrollmentError> {
        if vault == [0; 16]
            || generation == 0
            || generation > i64::MAX as u64
            || *root == [0; 32]
            || *device_proof == [0; 32]
            || *root == *device_proof
        {
            return Err(EnrollmentError::Scope);
        }
        Ok(Self {
            scope: Scope { vault, generation },
            root,
            device_proof,
        })
    }

    fn aad(&self) -> [u8; 32] {
        let mut aad = [0; 32];
        aad[..8].copy_from_slice(MAGIC);
        aad[8..24].copy_from_slice(&self.scope.vault);
        aad[24..].copy_from_slice(&self.scope.generation.to_be_bytes());
        aad
    }

    fn derive(&self, purpose: &'static str) -> Zeroizing<[u8; 32]> {
        let mut kdf = Zeroizing::new(blake3::Hasher::new_derive_key(purpose));
        kdf.update(&self.aad());
        kdf.update(&self.root[..]);
        kdf.update(&self.device_proof[..]);
        Zeroizing::new(*kdf.finalize().as_bytes())
    }

    #[cfg(feature = "vault-uia-chat")]
    pub(crate) fn library_authority(
        &self, profile: [u8; 32],
    ) -> Result<crate::private_library_owner::LibraryAuthority, crate::private_library::LibraryError> {
        crate::private_library_owner::LibraryAuthority::new(
            profile, self.scope.vault, self.scope.generation,
            self.derive("harw android library assets recipient material v1"),
            self.derive("harw android library purposes recipient material v1"),
            self.derive("harw android library purposes authentication material v1"),
        )
    }

    fn recipient(&self) -> Result<RecipientKeyPair, EnrollmentError> {
        let seed = self.derive("harw android uia enrollment recipient v1");
        derive_recipient_key_pair(SUITE.kem(), &seed[..]).map_err(|_| EnrollmentError::Crypto)
    }

    fn authenticate_wire(&self, bytes: &[u8]) -> blake3::Hash {
        let auth = self.derive("harw android uia enrollment manifest authentication v1");
        let mut mac = Zeroizing::new(blake3::Hasher::new_keyed(&auth));
        mac.update(&self.aad());
        mac.update(bytes);
        mac.finalize()
    }
}

struct Anchor {
    room: RoomId,
    sign_seed: Zeroizing<[u8; 32]>,
    verifier: Vec<u8>,
}

pub(crate) struct EnrollmentStore {
    root: PathBuf,
    dir: File,
}

impl EnrollmentStore {
    /// Path is supplied by Android's registered app-private resolver, never
    /// by a request or from the manifest. No directory creation or migration.
    pub(crate) fn open_app_private(root: PathBuf) -> Result<Self, EnrollmentError> {
        if !root.is_absolute() {
            return Err(EnrollmentError::Scope);
        }
        let dir = Self::open_checked(&root)?;
        Ok(Self { root, dir })
    }

    fn open_checked(root: &Path) -> Result<File, EnrollmentError> {
        let dir = File::from(open_dir_nofollow(root)?);
        ensure_owned_directory(&dir)?;
        if dir.metadata()?.mode() & 0o077 != 0 {
            return Err(EnrollmentError::Scope);
        }
        Ok(dir)
    }

    fn confirm_path(&self) -> Result<(), EnrollmentError> {
        let pinned = self.dir.metadata()?;
        let checked = Self::open_checked(&self.root)?.metadata()?;
        if (pinned.dev(), pinned.ino()) != (checked.dev(), checked.ino()) {
            return Err(EnrollmentError::Scope);
        }
        Ok(())
    }

    fn read_private(&self, name: &str) -> Result<Option<File>, EnrollmentError> {
        match open_beneath(self.dir.as_fd(), Path::new(name), OpenMode::read_only()) {
            Ok(file) => {
                Self::confirm_file(&file)?;
                Ok(Some(file))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn confirm_file(file: &File) -> Result<(), EnrollmentError> {
        ensure_private_regular(file)?;
        if file.metadata()?.nlink() != 1 {
            return Err(EnrollmentError::Scope);
        }
        Ok(())
    }

    fn try_lock(&self, name: &str, create: bool) -> Result<File, EnrollmentError> {
        let file = open_beneath(
            self.dir.as_fd(),
            Path::new(name),
            OpenMode {
                create,
                ..OpenMode::read_write()
            },
        )
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                EnrollmentError::Missing
            } else {
                error.into()
            }
        })?;
        Self::confirm_file(&file)?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => Err(EnrollmentError::Busy),
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }

    fn lock(&self, create: bool) -> Result<File, EnrollmentError> {
        self.confirm_path()?;
        self.try_lock(ENROLLMENT_LOCK, create)
    }

    fn generate_manifest(
        &self,
        authority: &VerifiedDeviceSecrets,
    ) -> Result<(Vec<u8>, Anchor), EnrollmentError> {
        let room_entropy = generate_recipient_seed().map_err(|_| EnrollmentError::Crypto)?;
        let room_hash = blake3::hash(&room_entropy[..]).to_hex();
        let room = RoomId::parse(&format!("uia-local-v1:{}", &room_hash[..32]))
            .map_err(|_| EnrollmentError::Crypto)?;
        let (signing, verifier) =
            MlDsa65Impl::keypair(&mut OsRng).map_err(|_| EnrollmentError::Crypto)?;
        if signing.as_bytes().len() != 32 || verifier.as_bytes().len() != VERIFIER_BYTES {
            return Err(EnrollmentError::Crypto);
        }
        let mut seed = Zeroizing::new([0; 32]);
        seed.copy_from_slice(signing.as_bytes());
        let anchor = Anchor {
            room,
            sign_seed: seed,
            verifier: verifier.as_bytes().to_vec(),
        };
        let mut plaintext = Zeroizing::new(Vec::with_capacity(PAYLOAD_BYTES));
        plaintext.extend_from_slice(anchor.room.as_str().as_bytes());
        plaintext.extend_from_slice(&anchor.sign_seed[..]);
        plaintext.extend_from_slice(&anchor.verifier);
        let recipient = authority.recipient()?;
        let envelope = HpkeEnvelope::seal(
            SUITE,
            recipient.public_key(),
            INFO,
            &authority.aad(),
            &plaintext,
        )
        .map_err(|_| EnrollmentError::Crypto)?
        .try_to_bytes()
        .map_err(|_| EnrollmentError::Crypto)?;
        let tag = authority.authenticate_wire(&envelope);
        let wire = [MAGIC.as_slice(), tag.as_bytes(), &envelope].concat();
        if wire.len() > MAX_WIRE {
            return Err(EnrollmentError::Crypto);
        }
        Ok((wire, anchor))
    }

    fn unwrap_manifest(
        file: File,
        authority: &VerifiedDeviceSecrets,
    ) -> Result<Anchor, EnrollmentError> {
        let length =
            usize::try_from(file.metadata()?.len()).map_err(|_| EnrollmentError::Authentication)?;
        if !(41..=MAX_WIRE).contains(&length) {
            return Err(EnrollmentError::Authentication);
        }
        let mut bytes = Vec::with_capacity(length);
        file.take(MAX_WIRE as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() != length || !bytes.starts_with(MAGIC) {
            return Err(EnrollmentError::Authentication);
        }
        let expected = authority.authenticate_wire(&bytes[40..]);
        // blake3::Hash comparison is constant-time for exactly 32 bytes.
        if expected != bytes[8..40] {
            return Err(EnrollmentError::Authentication);
        }
        let env =
            HpkeEnvelope::from_bytes(&bytes[40..]).map_err(|_| EnrollmentError::Authentication)?;
        if env.suite() != SUITE {
            return Err(EnrollmentError::Authentication);
        }
        let recipient = authority.recipient()?;
        let plain = env
            .open_zeroizing(recipient.private_key(), INFO, &authority.aad())
            .map_err(|_| EnrollmentError::Authentication)?;
        if plain.len() != PAYLOAD_BYTES {
            return Err(EnrollmentError::Authentication);
        }
        let room = std::str::from_utf8(&plain[..ROOM_ID_BYTES])
            .ok()
            .and_then(|id| RoomId::parse(id).ok())
            .ok_or(EnrollmentError::Authentication)?;
        let mut sign_seed = Zeroizing::new([0; 32]);
        sign_seed.copy_from_slice(&plain[ROOM_ID_BYTES..ROOM_ID_BYTES + 32]);
        Ok(Anchor {
            room,
            sign_seed,
            verifier: plain[ROOM_ID_BYTES + 32..].to_vec(),
        })
    }

    fn write_staged(&self, wire: &[u8]) -> Result<(), EnrollmentError> {
        self.confirm_path()?;
        match write_atomic_with_outcome(
            &self.root.join(STAGING),
            wire,
            AtomicWriteOptions::private(),
        )? {
            AtomicWriteOutcome::Committed => self
                .dir
                .sync_all()
                .map_err(|_| EnrollmentError::UncertainCommit),
            AtomicWriteOutcome::DurabilityUncertain(_) => Err(EnrollmentError::UncertainCommit),
        }
    }

    /// New-only, invoked after the platform authorizes explicit fresh
    /// registration. Any unrecognized/partial files prevent a second genesis.
    pub(crate) fn provision_new(
        &self,
        authority: &VerifiedDeviceSecrets,
    ) -> Result<(), EnrollmentError> {
        let _guard = self.lock(true)?;
        if self.read_private(FINAL)?.is_some() {
            return Err(EnrollmentError::AlreadyProvisioned);
        }
        if self.read_private(STAGING)?.is_some() {
            return Err(EnrollmentError::Interrupted);
        }
        for entry in std::fs::read_dir(&self.root)? {
            if entry?.file_name() != ENROLLMENT_LOCK {
                return Err(EnrollmentError::Interrupted);
            }
        }
        let (wire, anchor) = self.generate_manifest(authority)?;
        self.write_staged(&wire)?;
        let lease_lock = self.try_lock(LEASE_LOCK, true)?;
        lease_lock.sync_all()?;
        let material = UnwrappedMaterial::from_authenticated_bytes(
            authority.root.clone(),
            authority.scope.vault,
            authority.scope.generation,
            anchor.sign_seed,
            anchor.verifier,
        )
        .map_err(|error| EnrollmentError::Handoff(RoomHandoffError::Vault(error)))?;
        let store =
            SealedRoomVault::open_bound_room(self.root.clone(), anchor.room.clone(), material)
                .map_err(|error| EnrollmentError::Handoff(RoomHandoffError::Vault(error)))?;
        let genesis = open_room(&store, &anchor.room).map_err(|_| EnrollmentError::Interrupted)?;
        if genesis.revision() != 0 {
            return Err(EnrollmentError::Interrupted);
        }
        self.confirm_path()?;
        // Atomic publication only after sealed genesis and lock are durable.
        rename_at(self.dir.as_fd(), Path::new(STAGING), Path::new(FINAL))?;
        self.dir
            .sync_all()
            .map_err(|_| EnrollmentError::UncertainCommit)?;
        Ok(())
    }

    /// Existing-only. No new seed, manifest, or content writes.
    pub(crate) fn open_existing(
        &self,
        authority: VerifiedDeviceSecrets,
    ) -> Result<OpenedEnrollment, EnrollmentError> {
        let _guard = self.lock(false)?;
        if self.read_private(STAGING)?.is_some() {
            return Err(EnrollmentError::Interrupted);
        }
        let current = self.read_private(FINAL)?.ok_or(EnrollmentError::Missing)?;
        self.open_authenticated_anchor(current, &authority)
    }

    /// Dedicated, charged-PIN restart path. A staged but unpublished anchor
    /// can finish only if its exact already-written genesis is authenticated.
    /// Never generate a new anchor, room, signer or lock on this path.
    pub(crate) fn resume_enrollment(
        &self,
        authority: VerifiedDeviceSecrets,
    ) -> Result<OpenedEnrollment, EnrollmentError> {
        let _guard = self.lock(false)?;
        let final_manifest = self.read_private(FINAL)?;
        let staged = self.read_private(STAGING)?;
        match (final_manifest, staged) {
            (Some(finalized), None) => self.open_authenticated_anchor(finalized, &authority),
            (None, Some(staged)) => {
                let opened = self.open_authenticated_anchor(staged, &authority)?;
                let existing = opened.lease().load(opened.room_id())
                    .map_err(EnrollmentError::Handoff)?
                    .ok_or(EnrollmentError::Interrupted)?;
                if existing.revision() != 0 { return Err(EnrollmentError::Interrupted); }
                self.confirm_path()?;
                self.dir.sync_all().map_err(|_| EnrollmentError::UncertainCommit)?;
                rename_at(self.dir.as_fd(), Path::new(STAGING), Path::new(FINAL))?;
                self.dir.sync_all().map_err(|_| EnrollmentError::UncertainCommit)?;
                Ok(opened)
            }
            (None, None) => Err(EnrollmentError::Missing),
            (Some(_), Some(_)) => Err(EnrollmentError::Interrupted),
        }
    }

    fn open_authenticated_anchor(
        &self, manifest: File, authority: &VerifiedDeviceSecrets,
    ) -> Result<OpenedEnrollment, EnrollmentError> {
        let anchor = Self::unwrap_manifest(manifest, authority)?;
        let lease_lock = self.try_lock(LEASE_LOCK, false)?;
        let expected = VerifiedRoomEnrollment::from_authenticated_anchor(
            self.root.clone(),
            anchor.room.clone(),
            authority.scope.vault,
            authority.scope.generation,
            anchor.verifier,
        )
        .map_err(EnrollmentError::Handoff)?;
        let owner =
            UiaRoomHandoff::from_enrollment(Some(expected)).map_err(EnrollmentError::Handoff)?;
        let ticket = owner.reserve().map_err(EnrollmentError::Handoff)?;
        let mut frame = Zeroizing::new([0u8; DIRECT_HANDOFF_BYTES]);
        frame[0] = 1;
        frame[1..17].copy_from_slice(&authority.scope.vault);
        frame[17..25].copy_from_slice(&authority.scope.generation.to_be_bytes());
        frame[25..57].copy_from_slice(&authority.root[..]);
        frame[57..89].copy_from_slice(&anchor.sign_seed[..]);
        // Native-only path uses the same zeroizing binder without crossing JNI.
        let lease = owner
            .accept_authenticated_frame(&ticket, &mut frame[..])
            .map_err(EnrollmentError::Handoff)?;
        Ok(OpenedEnrollment {
            room: anchor.room,
            owner,
            lease,
            lock: Mutex::new(Some(lease_lock)),
        })
    }
}

pub(crate) struct OpenedEnrollment {
    room: RoomId,
    owner: UiaRoomHandoff,
    lease: Arc<AuthenticatedRoomLease>,
    lock: Mutex<Option<File>>,
}
impl OpenedEnrollment {
    pub(crate) fn room_id(&self) -> &RoomId {
        &self.room
    }
    pub(crate) fn lease(&self) -> Arc<AuthenticatedRoomLease> {
        Arc::clone(&self.lease)
    }
    pub(crate) fn revoke(&self) {
        drop(self.revoke_and_take_owner_lock());
    }

    /// The OS file lock is not decryption or signing authority. Keep it
    /// with the detached cancellation entry while its exact worker exits,
    /// fencing authenticated opens in other app processes too.
    pub(crate) fn revoke_and_take_owner_lock(&self) -> Option<File> {
        self.owner.revoke();
        self.lock.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}
impl Drop for OpenedEnrollment {
    fn drop(&mut self) {
        self.revoke();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uia_room_store::{RoomVault, commit_turn, open_room};
    use std::os::unix::fs::PermissionsExt;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    struct Fixture {
        dir: tempfile::TempDir,
        root: Zeroizing<[u8; 32]>,
        device: Zeroizing<[u8; 32]>,
    }
    impl Fixture {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            let dir = tempfile::tempdir()?;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
            Ok(Self {
                dir,
                root: generate_recipient_seed()?,
                device: generate_recipient_seed()?,
            })
        }
        fn authority(&self) -> VerifiedDeviceSecrets {
            VerifiedDeviceSecrets::from_pin_unwrap(
                [1u8; 16],
                1,
                self.root.clone(),
                self.device.clone(),
            )
            .expect("scoped unseal")
        }
        fn enrollment(&self) -> EnrollmentStore {
            EnrollmentStore::open_app_private(self.dir.path().to_owned()).expect("app directory")
        }
    }

    #[test]
    fn provision_then_exact_existing_reopen_and_revoke() -> TestResult {
        let fixture = Fixture::new()?;
        let store = fixture.enrollment();
        assert!(matches!(
            store.open_existing(fixture.authority()),
            Err(EnrollmentError::Missing)
        ));
        assert_eq!(std::fs::read_dir(fixture.dir.path())?.count(), 0);
        store
            .provision_new(&fixture.authority())
            .expect("durable enrollment");
        assert!(matches!(
            store.provision_new(&fixture.authority()),
            Err(EnrollmentError::AlreadyProvisioned)
        ));
        let opened = store
            .open_existing(fixture.authority())
            .expect("existing encrypted room");
        let scope = opened.room_id().clone();
        let arc = opened.lease();
        let empty = open_room(arc.as_ref(), &scope).expect("genesis");
        commit_turn(arc.as_ref(), &empty, "Canary", "Answer").expect("append");
        drop(opened);
        assert!(arc.load(&scope).is_err());
        let reopened = fixture
            .enrollment()
            .open_existing(fixture.authority())
            .expect("authentic re-open");
        assert!(reopened.room_id() == &scope);
        let recovered = open_room(reopened.lease().as_ref(), &scope).expect("history");
        assert_eq!(recovered.completed_turns()[0].answer(), "Answer");
        Ok(())
    }

    #[test]
    fn interrupted_staging_is_not_existing_and_never_restages() -> TestResult {
        let fixture = Fixture::new()?;
        let store = fixture.enrollment();
        let _guard = store.lock(true).expect("authorized fresh setup");
        let (stage, _) = store
            .generate_manifest(&fixture.authority())
            .expect("real manifest");
        store.write_staged(&stage).expect("crash after staging");
        drop(_guard);
        assert!(matches!(
            fixture.enrollment().open_existing(fixture.authority()),
            Err(EnrollmentError::Interrupted)
        ));
        assert!(matches!(
            fixture.enrollment().provision_new(&fixture.authority()),
            Err(EnrollmentError::Interrupted)
        ));
        let wire = std::fs::read(fixture.dir.path().join(STAGING))?;
        assert_eq!(wire, stage);
        Ok(())
    }

    #[test]
    fn tampered_anchor_and_wrong_device_never_create_new_keys() -> TestResult {
        let fixture = Fixture::new()?;
        let store = fixture.enrollment();
        store.provision_new(&fixture.authority()).expect("durable");
        let wrong = VerifiedDeviceSecrets::from_pin_unwrap(
            [1; 16],
            1,
            fixture.root.clone(),
            generate_recipient_seed()?,
        )
        .expect("different device");
        assert!(matches!(
            store.open_existing(wrong),
            Err(EnrollmentError::Authentication)
        ));
        let wrong_epoch = VerifiedDeviceSecrets::from_pin_unwrap(
            [1; 16],
            2,
            fixture.root.clone(),
            fixture.device.clone(),
        )
        .expect("different epoch");
        assert!(matches!(
            store.open_existing(wrong_epoch),
            Err(EnrollmentError::Authentication)
        ));
        let path = fixture.dir.path().join(FINAL);
        let mut wire = std::fs::read(&path)?;
        wire[40] ^= 1;
        std::fs::write(&path, &wire)?;
        assert!(matches!(
            store.open_existing(fixture.authority()),
            Err(EnrollmentError::Authentication)
        ));
        assert!(matches!(
            store.provision_new(&fixture.authority()),
            Err(EnrollmentError::AlreadyProvisioned)
        ));
        assert_eq!(std::fs::read(path)?, wire);
        Ok(())
    }

    #[test]
    fn seal_present_but_crash_before_publication_is_not_an_enrolled_room() -> TestResult {
        let fixture = Fixture::new()?;
        let store = fixture.enrollment();
        let authority = fixture.authority();
        let guard = store.lock(true).expect("fresh claim");
        let (wire, anchor) = store.generate_manifest(&authority).expect("real manifest");
        store.write_staged(&wire).expect("durable stage");
        store
            .try_lock(LEASE_LOCK, true)
            .expect("lease lock")
            .sync_all()?;
        let material = UnwrappedMaterial::from_authenticated_bytes(
            authority.root.clone(),
            authority.scope.vault,
            authority.scope.generation,
            anchor.sign_seed,
            anchor.verifier,
        )
        .expect("fresh independent keys");
        let genesis = SealedRoomVault::open_bound_room(
            fixture.dir.path().to_owned(),
            anchor.room.clone(),
            material,
        )
        .expect("authorized fixture");
        assert_eq!(
            open_room(&genesis, &anchor.room)
                .expect("durable genesis")
                .revision(),
            0
        );
        drop(guard);
        drop(genesis);
        let reopened = fixture.enrollment();
        assert!(matches!(
            reopened.open_existing(fixture.authority()),
            Err(EnrollmentError::Interrupted)
        ));
        assert!(matches!(
            reopened.provision_new(&fixture.authority()),
            Err(EnrollmentError::Interrupted)
        ));
        assert_eq!(std::fs::read(fixture.dir.path().join(STAGING))?, wire);
        assert!(!fixture.dir.path().join(FINAL).exists());
        Ok(())
    }

    #[test]
    fn a_second_native_owner_waits_for_shared_lease_revocation() -> TestResult {
        let fixture = Fixture::new()?;
        let first = fixture.enrollment();
        first.provision_new(&fixture.authority()).expect("new");
        let owner = first
            .open_existing(fixture.authority())
            .expect("first owner");
        let old_lease = owner.lease();
        let second = fixture.enrollment();
        assert!(matches!(
            second.open_existing(fixture.authority()),
            Err(EnrollmentError::Busy)
        ));
        owner.revoke();
        assert!(old_lease.load(owner.room_id()).is_err());
        assert!(second.open_existing(fixture.authority()).is_ok());
        Ok(())
    }

    #[test]
    fn committed_anchor_with_missing_room_is_recovery_only() -> TestResult {
        let fixture = Fixture::new()?;
        let store = fixture.enrollment();
        store.provision_new(&fixture.authority()).expect("durable");
        let record = std::fs::read_dir(fixture.dir.path())?
            .filter_map(Result::ok)
            .find(|e| e.path().extension().is_some_and(|x| x == "cgr"))
            .ok_or("room")?;
        std::fs::remove_file(record.path())?;
        assert!(matches!(
            store.open_existing(fixture.authority()),
            Err(EnrollmentError::Handoff(RoomHandoffError::RecoveryRequired))
        ));
        assert!(!record.path().exists());
        Ok(())
    }
}
