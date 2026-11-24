//! Vault-internal purpose enrollment. No callers choose a filesystem path or present key bytes.

use std::{
    fs::File,
    io::{self, Read},
    os::{fd::AsFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

use crypt_guard::{
    kem::backend::OsRng,
    pq_hpke::{Aead, HpkeEnvelope, Kdf, Kem, Suite, derive_recipient_key_pair},
    sign::{SignAlgorithm, ml_dsa::MlDsa65Impl},
};
use harw_fsutil::{
    AtomicWriteOptions, AtomicWriteOutcome, OpenMode, ensure_owned_directory,
    ensure_private_regular, mkdir_at, open_beneath, open_dir_at, open_dir_nofollow,
    write_atomic_with_outcome,
};
use zeroize::Zeroizing;
use harw_types::cancel::{CancelReason, CancelToken};

use crate::private_library::{AuthenticatedLibraryKeys, LibraryError, LibraryScope, PrivateLibraryStore};

const DIR: &str = "private-library-v1";
const MANIFEST: &str = "purpose-keys-v1.cgr";
const INFO: &[u8] = b"harw/android/vault/private-library/purpose-enrollment/v1";
const SUITE: Suite = Suite::new(Kem::MlKem1024, Kdf::Shake256, Aead::XChaCha20Poly1305);
const SEED_BYTES: usize = 32;
const VERIFIER_BYTES: usize = 1952;
const MANIFEST_LIMIT: usize = 8 * 1024;

pub(crate) struct LibraryAuthority {
    scope: LibraryScope,
    manifest_aad: [u8; 56],
    recipient_root: Zeroizing<[u8; 32]>,
    manifest_recipient: Zeroizing<[u8; 32]>,
    manifest_mac: Zeroizing<[u8; 32]>,
}

impl LibraryAuthority {
    pub(crate) fn new(
        profile: [u8; 32], vault: [u8; 16], generation: u64,
        recipient_root: Zeroizing<[u8; 32]>,
        manifest_recipient: Zeroizing<[u8; 32]>,
        manifest_mac: Zeroizing<[u8; 32]>,
    ) -> Result<Self, LibraryError> {
        let scope = LibraryScope::authenticated(profile, vault, generation)?;
        let mut aad = [0; 56];
        aad[..32].copy_from_slice(&profile);
        aad[32..48].copy_from_slice(&vault);
        aad[48..].copy_from_slice(&generation.to_be_bytes());
        Ok(Self {
            scope,
            manifest_aad: aad,
            recipient_root,
            manifest_recipient,
            manifest_mac,
        })
    }

    fn manifest_tag(&self, encrypted: &[u8]) -> blake3::Hash {
        let mut mac = Zeroizing::new(blake3::Hasher::new_keyed(&self.manifest_mac));
        mac.update(INFO);
        mac.update(&self.manifest_aad);
        mac.update(encrypted);
        mac.finalize()
    }

    fn create_manifest(&self) -> Result<(Vec<u8>, AuthenticatedLibraryKeys), LibraryError> {
        let (signing, verifying) = MlDsa65Impl::keypair(&mut OsRng).map_err(|_| LibraryError::Crypto)?;
        if signing.as_bytes().len() != SEED_BYTES || verifying.as_bytes().len() != VERIFIER_BYTES {
            return Err(LibraryError::Crypto);
        }
        let mut seed = Zeroizing::new([0; 32]);
        seed.copy_from_slice(signing.as_bytes());
        let mut body = Zeroizing::new(Vec::with_capacity(SEED_BYTES + VERIFIER_BYTES));
        body.extend_from_slice(&seed[..]);
        body.extend_from_slice(verifying.as_bytes());
        let recipient = derive_recipient_key_pair(SUITE.kem(), &self.manifest_recipient[..])
            .map_err(|_| LibraryError::Crypto)?;
        let encrypted = HpkeEnvelope::seal(SUITE, recipient.public_key(), INFO, &self.manifest_aad, &body)
            .map_err(|_| LibraryError::Crypto)?.try_to_bytes().map_err(|_| LibraryError::Crypto)?;
        let tag = self.manifest_tag(&encrypted);
        let keys = AuthenticatedLibraryKeys::from_owner(
            self.scope, &self.recipient_root, seed, verifying.as_bytes().to_vec(),
        )?;
        Ok(([tag.as_bytes().as_slice(), &encrypted].concat(), keys))
    }

    fn open_manifest(&self, bytes: &[u8]) -> Result<AuthenticatedLibraryKeys, LibraryError> {
        let (expected, encrypted) = bytes.split_at_checked(32).ok_or(LibraryError::Corrupt)?;
        let expected = <[u8; 32]>::try_from(expected).map_err(|_| LibraryError::Corrupt)?;
        if self.manifest_tag(encrypted) != expected { return Err(LibraryError::Corrupt); }
        let envelope = HpkeEnvelope::from_bytes(encrypted).map_err(|_| LibraryError::Corrupt)?;
        if envelope.suite() != SUITE { return Err(LibraryError::Corrupt); }
        let recipient = derive_recipient_key_pair(SUITE.kem(), &self.manifest_recipient[..])
            .map_err(|_| LibraryError::Crypto)?;
        let plain = envelope.open_zeroizing(recipient.private_key(), INFO, &self.manifest_aad)
            .map_err(|_| LibraryError::Corrupt)?;
        if plain.len() != SEED_BYTES + VERIFIER_BYTES { return Err(LibraryError::Corrupt); }
        let mut seed = Zeroizing::new([0; 32]);
        seed.copy_from_slice(&plain[..SEED_BYTES]);
        AuthenticatedLibraryKeys::from_owner(
            self.scope, &self.recipient_root, seed, plain[SEED_BYTES..].to_vec(),
        )
    }
}

fn private_dir(dir: File) -> Result<File, LibraryError> {
    ensure_owned_directory(&dir)?;
    if dir.metadata()?.mode() & 0o077 != 0 { return Err(LibraryError::BadScope); }
    Ok(dir)
}

pub(crate) struct PrivateLibraryOwner {
    pin_dir: PathBuf,
    pin_fd: File,
    active: AtomicBool,
    // Separate from the Library store lock. Covers explicit one-shot setup/open only.
    opening: Mutex<()>,
    caption: Mutex<Option<CancelToken>>,
    state: Mutex<OwnerState>,
}

struct OwnerState {
    authority: Option<Arc<LibraryAuthority>>,
    opened: Option<Arc<PrivateLibraryStore>>,
}

impl PrivateLibraryOwner {
    /// Issued by the registry after its existing PIN+device proof and room
    /// enrollment have both authenticated. Path derived inside the registry.
    pub(crate) fn new(pin_dir: PathBuf, authority: LibraryAuthority) -> Result<Self, LibraryError> {
        let pin_fd = private_dir(File::from(open_dir_nofollow(&pin_dir)?))?;
        Ok(Self {
            pin_dir, pin_fd, active: AtomicBool::new(true), opening: Mutex::new(()),
            caption: Mutex::new(None),
            state: Mutex::new(OwnerState { authority: Some(Arc::new(authority)), opened: None }),
        })
    }

    pub(crate) fn active(&self) -> bool { self.active.load(Ordering::Acquire) }

    pub(crate) fn revoke(&self) {
        self.active.store(false, Ordering::Release);
        if let Some(job) = self.caption.lock().unwrap_or_else(PoisonError::into_inner).take() {
            job.cancel(CancelReason::LeaseLost);
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(store) = state.opened.take() { store.revoke(); }
        state.authority = None;
    }

    pub(crate) fn begin_caption(&self) -> Result<LibraryCaptionJob<'_>, LibraryError> {
        let mut live = self.caption.lock().map_err(|_| LibraryError::Locked)?;
        self.require_active()?;
        if live.is_some() { return Err(LibraryError::Busy); }
        let token = CancelToken::new();
        *live = Some(token.clone());
        Ok(LibraryCaptionJob { owner: self, token })
    }

    fn require_active(&self) -> Result<(), LibraryError> {
        if self.active() { Ok(()) } else { Err(LibraryError::Locked) }
    }

    fn pinned_path(&self) -> Result<(), LibraryError> {
        let named = private_dir(File::from(open_dir_nofollow(&self.pin_dir)?))?.metadata()?;
        let pinned = self.pin_fd.metadata()?;
        if (named.dev(), named.ino()) != (pinned.dev(), pinned.ino()) {
            return Err(LibraryError::BadScope);
        }
        Ok(())
    }

    fn directory(&self) -> Result<Option<File>, LibraryError> {
        self.pinned_path()?;
        match open_dir_at(self.pin_fd.as_fd(), Path::new(DIR)) {
            Ok(dir) => private_dir(dir).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn verify_named_directory(&self, opened: &File) -> Result<PathBuf, LibraryError> {
        self.pinned_path()?;
        let path = self.pin_dir.join(DIR);
        let named = private_dir(File::from(open_dir_nofollow(&path)?))?.metadata()?;
        let pinned = opened.metadata()?;
        if (named.dev(), named.ino()) != (pinned.dev(), pinned.ino()) { return Err(LibraryError::BadScope); }
        Ok(path)
    }

    pub(crate) fn open_existing(&self) -> Result<Option<Arc<PrivateLibraryStore>>, LibraryError> {
        self.require_active()?;
        let _guard = self.opening.try_lock().map_err(|_| LibraryError::Busy)?;
        let authority = {
            let state = self.state.lock().map_err(|_| LibraryError::Locked)?;
            self.require_active()?;
            if let Some(store) = &state.opened { return Ok(Some(Arc::clone(store))); }
            state.authority.clone().ok_or(LibraryError::Locked)?
        };
        let Some(directory) = self.directory()? else { return Ok(None) };
        let path = self.verify_named_directory(&directory)?;
        let file = open_beneath(directory.as_fd(), Path::new(MANIFEST), OpenMode::read_only())?;
        ensure_private_regular(&file)?;
        let metadata = file.metadata()?;
        let size = usize::try_from(metadata.len()).map_err(|_| LibraryError::Corrupt)?;
        if metadata.nlink() != 1 || !(33..=MANIFEST_LIMIT).contains(&size) { return Err(LibraryError::Corrupt); }
        let mut bytes = Vec::with_capacity(size);
        file.take(MANIFEST_LIMIT as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() != size { return Err(LibraryError::Corrupt); }
        let keys = authority.open_manifest(&bytes)?;
        self.publish(PrivateLibraryStore::open(path, keys)?).map(Some)
    }

    pub(crate) fn provision(&self) -> Result<Arc<PrivateLibraryStore>, LibraryError> {
        self.require_active()?;
        let _guard = self.opening.try_lock().map_err(|_| LibraryError::Busy)?;
        let authority = self.state.lock().map_err(|_| LibraryError::Locked)?
            .authority.clone().ok_or(LibraryError::Locked)?;
        // A pre-existing, partially committed, or unrecognised directory is
        // recovery only. Never turn an open error into another creation.
        if self.directory()?.is_some() { return Err(LibraryError::Conflict); }
        let (wire, keys) = authority.create_manifest()?;
        self.pinned_path()?;
        self.require_active()?;
        if !mkdir_at(self.pin_fd.as_fd(), Path::new(DIR), 0o700)? { return Err(LibraryError::Conflict); }
        self.pin_fd.sync_all().map_err(|_| LibraryError::UncertainCommit)?;
        let directory = self.directory()?.ok_or(LibraryError::UncertainCommit)?;
        let path = self.verify_named_directory(&directory)?;
        self.require_active().map_err(|_| LibraryError::UncertainCommit)?;
        match write_atomic_with_outcome(&path.join(MANIFEST), &wire, AtomicWriteOptions::private())? {
            AtomicWriteOutcome::Committed => directory.sync_all().map_err(|_| LibraryError::UncertainCommit)?,
            AtomicWriteOutcome::DurabilityUncertain(_) => return Err(LibraryError::UncertainCommit),
        }
        self.require_active().map_err(|_| LibraryError::UncertainCommit)?;
        self.publish(PrivateLibraryStore::provision_new(path, keys)?)
    }

    fn publish(&self, store: PrivateLibraryStore) -> Result<Arc<PrivateLibraryStore>, LibraryError> {
        let mut state = self.state.lock().map_err(|_| LibraryError::Locked)?;
        if !self.active() {
            store.revoke();
            return Err(LibraryError::Locked);
        }
        let open = Arc::new(store);
        state.opened = Some(Arc::clone(&open));
        Ok(open)
    }
}

impl Drop for PrivateLibraryOwner {
    fn drop(&mut self) { self.revoke(); }
}


pub(crate) struct LibraryCaptionJob<'a> {
    owner: &'a PrivateLibraryOwner,
    token: CancelToken,
}

impl LibraryCaptionJob<'_> {
    pub(crate) fn token(&self) -> &CancelToken { &self.token }
}

impl Drop for LibraryCaptionJob<'_> {
    fn drop(&mut self) {
        self.owner.caption.lock().unwrap_or_else(PoisonError::into_inner).take();
    }
}
