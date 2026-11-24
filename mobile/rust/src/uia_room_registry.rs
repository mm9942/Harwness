//! Private process registry for the Android PIN-to-UIA native bridge.

use aws_lc_rs::digest::{SHA256, digest};
use crypt_guard::pq_hpke::generate_recipient_seed;
use harw_fsutil::{
    EntryType, OpenMode, ensure_owned_directory, ensure_private_regular, entry_type_at, mkdir_at,
    open_beneath, open_dir_at, open_dir_nofollow, read_dir_names, same_file_at,
};
use std::{
    fs::File,
    io::{self, Read},
    os::{fd::AsFd, unix::fs::MetadataExt},
    path::Path,
    sync::{Arc, Mutex},
};
use zeroize::{Zeroize, Zeroizing};

use crate::uia_room_enrollment::{
    ENROLLMENT_LOCK, EnrollmentError, EnrollmentStore, OpenedEnrollment, VerifiedDeviceSecrets,
};

const PIN_NAMESPACE: &str = "harw-vault-pin-v1";
const ROOM_DIR: &str = "uia-room-v1";
// Empty and scope-bound by its exclusively opened first-generation parent.
// There is no partially written payload: uncertainty is resolved by re-syncing
// the exact marker, never by replacing or removing it.
const RETIRED_FIRST_ENROLLMENT: &str = ".uia-enrollment-retired-v1";
const MAX_HANDLES: usize = 16;
pub(crate) const FRAME_BYTES: usize = 89;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectedScope {
    uuid: [u8; 16],
    epoch: u64,
}
impl SelectedScope {
    pub(crate) fn new(most: i64, least: i64, generation: i64) -> Option<Self> {
        let epoch = u64::try_from(generation).ok()?;
        let mut uuid = [0; 16];
        uuid[..8].copy_from_slice(&most.to_be_bytes());
        uuid[8..].copy_from_slice(&least.to_be_bytes());
        (epoch > 0 && uuid != [0; 16]).then_some(Self { uuid, epoch })
    }
    fn canonical_uuid(self) -> String {
        let [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] = self.uuid;
        format!(
            "{a:02x}{b:02x}{c:02x}{d:02x}-{e:02x}{f:02x}-{g:02x}{h:02x}-{i:02x}{j:02x}-{k:02x}{l:02x}{m:02x}{n:02x}{o:02x}{p:02x}"
        )
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Mode {
    Provision,
    Resume,
    Existing,
    ExistingBiometric,
}

impl Mode {
    pub(crate) fn enrollment_error(self, error: NativeError, stage: NativeError) -> NativeError {
        if matches!(self, Self::Provision | Self::Resume) { stage } else { error }
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(i64)]
pub(crate) enum NativeError {
    Denied = -1,
    Recovery = -2,
    Busy = -3,
    Unavailable = -4,
    Uncertain = -5,
    EnrollmentApp = -10,
    EnrollmentScope = -11,
    EnrollmentRoomDirectory = -12,
    EnrollmentRoomWrite = -13,
    EnrollmentRoomVerify = -14,
}
impl From<EnrollmentError> for NativeError {
    fn from(err: EnrollmentError) -> Self {
        match err {
            EnrollmentError::Scope | EnrollmentError::Authentication => Self::Denied,
            EnrollmentError::Busy => Self::Busy,
            EnrollmentError::UncertainCommit => Self::Uncertain,
            EnrollmentError::AlreadyProvisioned
            | EnrollmentError::Interrupted
            | EnrollmentError::Missing
            | EnrollmentError::Handoff(_) => Self::Recovery,
            EnrollmentError::Io(_) | EnrollmentError::Crypto => Self::Unavailable,
        }
    }
}
impl From<io::Error> for NativeError {
    fn from(err: io::Error) -> Self {
        if err.kind() == io::ErrorKind::NotFound {
            Self::Recovery
        } else {
            Self::Unavailable
        }
    }
}

struct BorrowedFrame<'a>(&'a mut [u8]);
impl Drop for BorrowedFrame<'_> {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

pub(crate) fn decode_frame(
    scope: SelectedScope,
    direct: &mut [u8],
) -> Result<VerifiedDeviceSecrets, NativeError> {
    let frame = BorrowedFrame(direct);
    let bytes = &*frame.0;
    if bytes.len() != FRAME_BYTES
        || bytes[0] != 1
        || bytes[1..17] != scope.uuid
        || bytes[17..25] != scope.epoch.to_be_bytes()
    {
        return Err(NativeError::Denied);
    }
    let mut root = Zeroizing::new([0; 32]);
    let mut proof = Zeroizing::new([0; 32]);
    root.copy_from_slice(&bytes[25..57]);
    proof.copy_from_slice(&bytes[57..89]);
    Ok(VerifiedDeviceSecrets::from_pin_unwrap(
        scope.uuid,
        scope.epoch,
        root,
        proof,
    )?)
}

fn checked_dir(file: File, exact_private: bool) -> Result<File, NativeError> {
    ensure_owned_directory(&file)?;
    if exact_private && file.metadata()?.mode() & 0o077 != 0 {
        return Err(NativeError::Denied);
    }
    Ok(file)
}

/// Called under Kotlin's existing locked PIN-record callback (both its
/// ProcessLock and FileChannel lock). Do not take a separate fcntl lock:
/// POSIX per-process locks are not independent of that Kotlin descriptor.
fn validate_platform_scope(
    parent: &File,
    scope: SelectedScope,
    mode: Mode,
) -> Result<(), NativeError> {
    for marker in ["state.pending", "state.next", "pin.pending", "pin.next"] {
        match open_beneath(parent.as_fd(), Path::new(marker), OpenMode::read_only()) {
            Ok(_) => return Err(NativeError::Recovery),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(NativeError::Recovery),
        }
    }
    if entry_type_at(parent.as_fd(), Path::new(RETIRED_FIRST_ENROLLMENT))?.is_some() {
        return Err(NativeError::Recovery);
    }
    let ledger = read_platform_ledger(parent, scope)?;
    // Ready with a nonzero resource reservation is required. Merely
    // knowing a registered Ready scope cannot jump ahead of PIN backoff.
    let charged = ledger.revision >= 3 && ledger.valid_charge;
    if !match mode {
        Mode::Provision => ledger.is_first_provision(scope),
        // Resume has its own charged state. It is never a second provision.
        Mode::Resume => scope.epoch == 1 && ledger.phase == 3 && ledger.revision >= 2 && ledger.valid_charge,
        Mode::Existing => ledger.phase == 2 && charged,
        Mode::ExistingBiometric => ledger.phase == 2 && (charged || (ledger.revision >= 2 && ledger.uncharged)),
    } {
        return Err(NativeError::Denied);
    }
    if matches!(mode, Mode::ExistingBiometric) {
        // Independent existing-only journal check; no relaxation of the PIN entry.
        // Root authority still comes from the scope-bound cryptographic envelope.
        for marker in [
            "biometric.pending", "biometric.next", "biometric.revoked", "biometric.cleanup-confirmed",
            "biometric.cancelled",
        ] {
            match open_beneath(parent.as_fd(), Path::new(marker), OpenMode::read_only()) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                _ => return Err(NativeError::Recovery),
            }
        }
        let bio = private_file(parent, "biometric.v1", 61)?;
        let mut wire = [0u8; 61];
        fixed_read(bio, &mut wire)?;
        if wire[0] != 1 {
            return Err(NativeError::Denied);
        }
    }
    let slot = private_file(parent, "pin.v1", 77)?;
    let mut wire = [0u8; 77];
    fixed_read(slot, &mut wire)?;
    if wire[0] != 1 {
        return Err(NativeError::Denied);
    }
    Ok(())
}

fn read_platform_ledger(parent: &File, scope: SelectedScope) -> Result<PlatformLedger, NativeError> {
    let file = private_file(parent, "state.bin", 87)?;
    let mut state = [0u8; 87];
    fixed_read(file, &mut state)?;
    // This existing SHA256 is a disk corruption check, not a PIN/device proof.
    if state[..5] != *b"HPIN\x01"
        || state[5..21] != scope.uuid
        || state[21..29] != scope.epoch.to_be_bytes()
        || digest(&SHA256, &state[..55]).as_ref() != &state[55..]
    {
        return Err(NativeError::Denied);
    }
    let revision = i64::from_be_bytes(state[29..37].try_into().map_err(|_| NativeError::Denied)?);
    let uncharged = state[38..55] == [0; 17];

    // Only the supported charged record formats may reach PIN/biometric open.
    let valid_charge = (1..=10).contains(&state[38]) && state[39] < 128 && state[47] < 128;
    Ok(PlatformLedger { phase: state[37], revision, uncharged, valid_charge })
}

struct PlatformLedger {
    phase: u8,
    revision: i64,
    uncharged: bool,
    valid_charge: bool,
}

impl PlatformLedger {
    fn is_first_provision(&self, scope: SelectedScope) -> bool {
        scope.epoch == 1 && self.phase == 1 && self.revision == 1 && self.uncharged
    }
}

fn private_file(dir: &File, name: &str, len: u64) -> Result<File, NativeError> {
    let file = open_beneath(dir.as_fd(), Path::new(name), OpenMode::read_only())?;
    ensure_private_regular(&file)?;
    let meta = file.metadata()?;
    if meta.len() != len || meta.nlink() != 1 {
        return Err(NativeError::Recovery);
    }
    Ok(file)
}
fn fixed_read<const N: usize>(mut file: File, dest: &mut [u8; N]) -> Result<(), NativeError> {
    file.read_exact(dest)?;
    if file.read(&mut [0u8; 1])? != 0 {
        return Err(NativeError::Recovery);
    }
    Ok(())
}
// Lock the already-existing scope DIRECTORY with an independent flock. This
// does not touch Kotlin's state.lock inode (closing a second descriptor to that
// inode would release its fcntl lock). Every native enrollment path holds this
// guard across the complete room operation; preview and retirement use it too.
fn lock_scope(root: &Path, scope: SelectedScope) -> Result<File, NativeError> {
    if !root.is_absolute() {
        return Err(NativeError::Denied);
    }
    let root_fd = checked_dir(File::from(open_dir_nofollow(root)?), false)?;
    let ns = checked_dir(open_dir_at(root_fd.as_fd(), Path::new(PIN_NAMESPACE))?, true)?;
    let uuid = scope.canonical_uuid();
    let scope_fd = checked_dir(open_dir_at(ns.as_fd(), Path::new(&uuid))?, true)?;
    match scope_fd.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Err(NativeError::Busy),
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    if !same_file_at(ns.as_fd(), Path::new(&uuid), &scope_fd)? {
        return Err(NativeError::Denied);
    }
    Ok(scope_fd)
}

fn room_store(
    root: &Path,
    scope_fd: &File,
    scope: SelectedScope,
    mode: Mode,
) -> Result<EnrollmentStore, NativeError> {
    validate_platform_scope(scope_fd, scope, mode)
        .map_err(|err| mode.enrollment_error(err, NativeError::EnrollmentScope))?;
    if matches!(mode, Mode::Provision) {
        if !mkdir_at(scope_fd.as_fd(), Path::new(ROOM_DIR), 0o700)? {
            return Err(NativeError::Recovery);
        }
        scope_fd.sync_all().map_err(|_| NativeError::Uncertain)?;
    }
    let pinned = checked_dir(open_dir_at(scope_fd.as_fd(), Path::new(ROOM_DIR))?, true)?;
    let path = root.join(PIN_NAMESPACE).join(scope.canonical_uuid()).join(ROOM_DIR);
    let named = File::from(open_dir_nofollow(&path)?);
    let a = pinned.metadata()?;
    let b = named.metadata()?;
    if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
        return Err(NativeError::Denied);
    }
    Ok(EnrollmentStore::open_app_private(path)?)
}

/// Native restart proof deliberately recognizes only the first ledger and a
/// room directory that was never written beyond creating its enrollment lock.
/// Staged anchors, vault artifacts, journals, arbitrary directories, and even
/// a conflicting artifact next to a previously valid fence fail closed.
fn uncommitted_retirement(scope_fd: &File, scope: SelectedScope) -> Result<Option<File>, NativeError> {
    if !read_platform_ledger(scope_fd, scope)?.is_first_provision(scope) {
        return Err(NativeError::Denied);
    }
    let mut retired = None;
    for name in read_dir_names(scope_fd.as_fd())? {
        match name.to_str() {
            // Kotlin holds and verifies state.lock. Never open that inode here.
            Some("state.lock" | "state.bin") => {}
            Some("pin.pending") => { private_file(scope_fd, "pin.pending", 0)?; }
            Some("pin.next") => {
                let file = open_beneath(scope_fd.as_fd(), Path::new("pin.next"), OpenMode::read_only())?;
                ensure_private_regular(&file)?;
                let meta = file.metadata()?;
                // An interrupted first write can contain only part of this slot.
                if meta.nlink() != 1 || meta.len() > 77 {
                    return Err(NativeError::Recovery);
                }
            }
            Some("pin.v1") => {
                let mut wire = [0; 77];
                fixed_read(private_file(scope_fd, "pin.v1", 77)?, &mut wire)?;
                if wire[0] != 1 {
                    return Err(NativeError::Recovery);
                }
            }
            Some(ROOM_DIR) => {
                let room = checked_dir(open_dir_at(scope_fd.as_fd(), Path::new(ROOM_DIR))?, true)?;
                for entry in read_dir_names(room.as_fd())? {
                    if entry != ENROLLMENT_LOCK {
                        return Err(NativeError::Recovery);
                    }
                    private_file(&room, ENROLLMENT_LOCK, 0)?;
                }
            }
            Some(RETIRED_FIRST_ENROLLMENT) => {
                retired = Some(private_file(scope_fd, RETIRED_FIRST_ENROLLMENT, 0)?);
            }
            _ => return Err(NativeError::Recovery),
        }
    }
    // Kotlin already checked the stable lock. Verify its type without opening
    // a second file descriptor, also making direct native calls fail closed.
    if entry_type_at(scope_fd.as_fd(), Path::new("state.lock"))? != Some(EntryType::File) {
        return Err(NativeError::Recovery);
    }
    Ok(retired)
}

#[cfg(feature = "vault-uia-chat")]
use crate::uia_room_operations::{ParkedExpiry, RoomOperations};
#[cfg(feature = "vault-uia-chat")]
pub(crate) struct ChatLease {
    pub(crate) owner: Arc<OpenedEnrollment>,
    pub(crate) ops: Arc<RoomOperations>,
}

struct Entry {
    token: i64,
    scope: SelectedScope,
    // Taken and revoked on park. The registry retains only a key-free
    // cancellation fence and the old exact scope/attempt until run exits.
    owner: Option<Arc<OpenedEnrollment>>,
    #[cfg(feature = "vault-uia-chat")]
    parked: Option<i64>,
    #[cfg(feature = "vault-uia-chat")]
    parked_owner_lock: Option<File>,
    #[cfg(feature = "vault-uia-chat")]
    ops: Arc<RoomOperations>,
    #[cfg(feature = "vault-uia-chat")]
    library: Option<([u8; 32], Arc<crate::private_library_owner::PrivateLibraryOwner>)>,
}
#[derive(Default)]
pub(crate) struct Registry {
    entries: Mutex<Vec<Entry>>,
    open_gate: Mutex<()>,
}
impl Registry {
    /// The path is produced only by native OS-backed application lookup;
    /// this function is NOT directly exported via JNI.
    pub(crate) fn open(
        &self,
        root: &Path,
        scope: SelectedScope,
        mode: Mode,
        proof: VerifiedDeviceSecrets,
    ) -> Result<i64, NativeError> {
        let _open = self.open_gate.try_lock().map_err(|_| NativeError::Busy)?;
        {
            let guard = self.entries.lock().map_err(|_| NativeError::Unavailable)?;
            if guard.len() >= MAX_HANDLES || guard.iter().any(|x| x.scope == scope) {
                return Err(NativeError::Busy);
            }
        }
        // Pair future handle before writing any provisioning data.
        let token = self.allocate_token()?;
        let locked_scope = lock_scope(root, scope)
            .map_err(|err| mode.enrollment_error(err, NativeError::EnrollmentRoomDirectory))?;
        let store = room_store(root, &locked_scope, scope, mode).map_err(|err| match err {
            NativeError::EnrollmentScope => err,
            other => mode.enrollment_error(other, NativeError::EnrollmentRoomDirectory),
        })?;
        if matches!(mode, Mode::Provision) {
            store.provision_new(&proof)
                .map_err(|err| mode.enrollment_error(err.into(), NativeError::EnrollmentRoomWrite))?;
        }
        // Only the freshly charged resume mode can publish an already-authenticated
        // staged genesis. All other existing opens must use a committed anchor.
        let opened = if matches!(mode, Mode::Resume) {
            store.resume_enrollment(proof)
        } else {
            store.open_existing(proof)
        };
        let owner = Arc::new(opened
            .map_err(|err| mode.enrollment_error(err.into(), NativeError::EnrollmentRoomVerify))?);
        self.entries
            .lock()
            .map_err(|_| NativeError::Unavailable)?
            .push(Entry {
                token,
                scope,
                owner: Some(owner),
                #[cfg(feature = "vault-uia-chat")]
                parked: None,
                #[cfg(feature = "vault-uia-chat")]
                parked_owner_lock: None,
                #[cfg(feature = "vault-uia-chat")]
                ops: Arc::new(RoomOperations::default()),
                #[cfg(feature = "vault-uia-chat")]
                library: None,
            });
        Ok(token)
    }
    /// Caller holds the Kotlin record lock and has resolved the exact selected
    /// profile. We independently revalidate the initial phase and artifacts
    /// under the cross-process native lock, with no credentials or deletions.
    pub(crate) fn restart_uncommitted(
        &self,
        root: &Path,
        scope: SelectedScope,
        retire: bool,
    ) -> Result<(), NativeError> {
        let _open = self.open_gate.try_lock().map_err(|_| NativeError::Busy)?;
        if self.entries.lock().map_err(|_| NativeError::Unavailable)?
            .iter().any(|entry| entry.scope == scope)
        {
            return Err(NativeError::Busy);
        }
        let locked_scope = lock_scope(root, scope)?;
        let previously_retired = uncommitted_retirement(&locked_scope, scope)?;
        if !retire {
            return Ok(());
        }
        let marker = match previously_retired {
            Some(marker) => marker,
            None => open_beneath(
                locked_scope.as_fd(),
                Path::new(RETIRED_FIRST_ENROLLMENT),
                OpenMode::write_create_new(0o600),
            )?,
        };
        // Reconfirm the ordinary private, single-link marker before certifying
        // the fence. An earlier uncertain commit is retried without rewriting.
        let meta = marker.metadata()?;
        ensure_private_regular(&marker)?;
        if meta.nlink() != 1 || meta.len() != 0 {
            return Err(NativeError::Recovery);
        }
        marker.sync_all().map_err(|_| NativeError::Uncertain)?;
        locked_scope.sync_all().map_err(|_| NativeError::Uncertain)?;
        Ok(())
    }

    fn allocate_token(&self) -> Result<i64, NativeError> {
        let guard = self.entries.lock().map_err(|_| NativeError::Unavailable)?;
        for _ in 0..8 {
            let seed = generate_recipient_seed().map_err(|_| NativeError::Unavailable)?;
            let token =
                i64::from_be_bytes(seed[..8].try_into().map_err(|_| NativeError::Unavailable)?)
                    & i64::MAX;
            if token > 0 && !guard.iter().any(|x| x.token == token) {
                return Ok(token);
            }
        }
        Err(NativeError::Unavailable)
    }
    pub(crate) fn get(&self, token: i64, scope: SelectedScope) -> Option<Arc<OpenedEnrollment>> {
        let entries = self.entries.lock().ok()?;
        entries
            .iter()
            .find(|entry| entry.token == token && entry.scope == scope)
            .and_then(|entry| entry.owner.clone())
    }
    #[cfg(feature = "vault-uia-chat")]
    pub(crate) fn chat(&self, token: i64, scope: SelectedScope) -> Option<ChatLease> {
        let entries = self.entries.lock().ok()?;
        entries
            .iter()
            .find(|e| e.token == token && e.scope == scope)
            .and_then(|e| Some(ChatLease {
                owner: e.owner.clone()?,
                ops: Arc::clone(&e.ops),
            }))
    }

    #[cfg(feature = "vault-uia-chat")]
    pub(crate) fn register_library(
        &self, token: i64, scope: SelectedScope, root: &Path, profile: [u8; 32],
        authority: crate::private_library_owner::LibraryAuthority,
    ) {
        let directory = root.join(PIN_NAMESPACE).join(scope.canonical_uuid());
        let Ok(library) = crate::private_library_owner::PrivateLibraryOwner::new(directory, authority) else { return; };
        let Ok(mut entries) = self.entries.lock() else { return; };
        if let Some(entry) = entries.iter_mut().find(|entry| entry.token == token && entry.scope == scope) {
            if entry.owner.is_some() && entry.library.is_none() {
                entry.library = Some((profile, Arc::new(library)));
            }
        }
    }

    #[cfg(feature = "vault-uia-chat")]
    pub(crate) fn library(
        &self, token: i64, scope: SelectedScope, profile: [u8; 32],
    ) -> Option<Arc<crate::private_library_owner::PrivateLibraryOwner>> {
        let entries = self.entries.lock().ok()?;
        let entry = entries.iter().find(|entry| entry.token == token && entry.scope == scope)?;
        let (bound, owner) = entry.library.as_ref()?;
        (*bound == profile && owner.active()).then(|| Arc::clone(owner))
    }

    #[cfg(feature = "vault-uia-chat")]
    pub(crate) fn park(&self, token: i64, scope: SelectedScope, attempt: i64) -> Option<ParkedExpiry> {
        let mut entries = self.entries.lock().ok()?;
        let entry = entries.iter_mut().find(|entry| entry.token == token && entry.scope == scope)?;
        if entry.parked.is_some() || entry.owner.is_none() { return None; }
        let expiry = entry.ops.park(attempt)?;
        entry.parked = Some(attempt);
        if let Some((_, library)) = entry.library.take() {
            library.revoke();
        }
        entry.parked_owner_lock = entry.owner.take()
            .and_then(|owner| owner.revoke_and_take_owner_lock());
        if entry.parked_owner_lock.is_none() {
            entry.ops.cancel_parked(attempt);
            return None;
        }
        Some(expiry)
    }

    /// A pre-PIN busy hint scoped only to the nominated vault and generation.
    /// Cancellation/publication does not clear it: native Finish must first
    /// unwind the worker and remove the entry that retains the OS owner lock.
    /// This issues no handle and neither consults nor returns vault contents.
    #[cfg(feature = "vault-uia-chat")]
    pub(crate) fn has_pending_parked(&self, scope: SelectedScope) -> Option<bool> {
        let entries = self.entries.lock().ok()?;
        Some(entries.iter().any(|entry| entry.scope == scope && entry.parked.is_some()))
    }

    #[cfg(feature = "vault-uia-chat")]
    pub(crate) fn cancel_parked(&self, token: i64, scope: SelectedScope, attempt: i64) -> bool {
        let Ok(entries) = self.entries.lock() else { return false; };
        entries.iter().find(|entry| entry.token == token && entry.scope == scope &&
            entry.parked == Some(attempt))
            .is_some_and(|entry| entry.ops.cancel_parked(attempt))
    }

    #[cfg(feature = "vault-uia-chat")]
    pub(crate) fn finish_chat(&self, token: i64, scope: SelectedScope, attempt: i64) {
        let mut entries = self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(index) = entries.iter().position(|entry| entry.token == token && entry.scope == scope)
        else { return; };
        entries[index].ops.finish(attempt);
        if entries[index].parked == Some(attempt) {
            entries.swap_remove(index);
        }
    }

    pub(crate) fn revoke(&self, token: i64, scope: SelectedScope) -> bool {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = entries
            .iter()
            .position(|x| x.token == token && x.scope == scope)
        {
            // A hard lock still closes a parked writer. Keep its key-free
            // tombstone until the native worker returns, fencing re-open.
            #[cfg(feature = "vault-uia-chat")]
            if let Some(attempt) = entries[index].parked {
                entries[index].ops.cancel_parked(attempt);
                return true;
            }
            // Invalidate all old Arc clones before another admission can proceed.
            #[cfg(feature = "vault-uia-chat")]
            entries[index].ops.revoke();
            #[cfg(feature = "vault-uia-chat")]
            if let Some((_, library)) = &entries[index].library {
                library.revoke();
            }
            if let Some(owner) = entries[index].owner.take() {
                owner.revoke();
            }
            entries.swap_remove(index);
            true
        } else {
            false
        }
    }
}
impl Drop for Registry {
    fn drop(&mut self) {
        for entry in self
            .entries
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            #[cfg(feature = "vault-uia-chat")]
            entry.ops.revoke();
            #[cfg(feature = "vault-uia-chat")]
            if let Some((_, library)) = &entry.library {
                library.revoke();
            }
            if let Some(owner) = entry.owner.take() {
                owner.revoke();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uia_room_store::RoomVault;
    use std::os::unix::fs::PermissionsExt;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    struct Fixture {
        dir: tempfile::TempDir,
        scope: SelectedScope,
        input: [u8; 89],
    }
    impl Fixture {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            let dir = tempfile::tempdir()?;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
            let scope = SelectedScope::new(0x0102030405060708, -2, 1).expect("selected");
            let mut input = [0; 89];
            input[0] = 1;
            input[1..17].copy_from_slice(&scope.uuid);
            input[17..25].copy_from_slice(&scope.epoch.to_be_bytes());
            input[25..57].copy_from_slice(&generate_recipient_seed()?[..]);
            input[57..].copy_from_slice(&generate_recipient_seed()?[..]);
            Ok(Self { dir, scope, input })
        }
        fn platform(&self, mode: u8) -> TestResult {
            let parent = self.dir.path().join(PIN_NAMESPACE);
            let scoped = parent.join(self.scope.canonical_uuid());
            for part in [&parent, &scoped] {
                std::fs::create_dir_all(part)?;
                std::fs::set_permissions(part, std::fs::Permissions::from_mode(0o700))?;
            }
            let mut bytes = [0u8; 87];
            bytes[..5].copy_from_slice(b"HPIN\x01");
            bytes[5..21].copy_from_slice(&self.scope.uuid);
            bytes[21..29].copy_from_slice(&self.scope.epoch.to_be_bytes());
            let revision = if mode == 2 { 3u64 } else { u64::from(mode) };
            bytes[29..37].copy_from_slice(&revision.to_be_bytes());
            bytes[37] = mode;
            if mode == 2 {
                bytes[38] = 1;
            }
            let mac = digest(&SHA256, &bytes[..55]);
            bytes[55..].copy_from_slice(mac.as_ref());
            let state = scoped.join("state.bin");
            std::fs::write(&state, bytes)?;
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o600))?;
            // Ledger fixture represents the structural slot, not an actual JVM PIN proof.
            let slot = scoped.join("pin.v1");
            let mut payload = [0u8; 77];
            payload[0] = 1;
            payload[1..33].copy_from_slice(&generate_recipient_seed()?[..]);
            std::fs::write(&slot, payload)?;
            std::fs::set_permissions(&slot, std::fs::Permissions::from_mode(0o600))?;
            Ok(())
        }
        fn open(&self, registry: &Registry, mode: Mode) -> Result<i64, NativeError> {
            let mut input = self.input;
            let proof = decode_frame(self.scope, &mut input).expect("real-sized typed input");
            assert_eq!(input, [0; 89]);
            registry.open(self.dir.path(), self.scope, mode, proof)
        }
    }

    #[test]
    fn native_handle_scoped_and_revoked_before_reuse() -> TestResult {
        let f = Fixture::new()?;
        let reg = Registry::default();
        f.platform(1)?;
        let h = f.open(&reg, Mode::Provision).expect("native new");
        let other = SelectedScope::new(1, -3, 1).expect("other scope");
        assert!(reg.get(h, other).is_none());
        assert!(!reg.revoke(h, other));
        let lease_holder = reg.get(h, f.scope).expect("exact scope");
        let lease = lease_holder.lease();
        let room = lease_holder.room_id().clone();
        assert!(lease.load(&room).is_ok());
        assert!(reg.revoke(h, f.scope));
        assert!(lease.load(&room).is_err());
        assert!(reg.get(h, f.scope).is_none());
        f.platform(2)?;
        let next = f.open(&reg, Mode::Existing).expect("real existing");
        assert_ne!(h, next);
        Ok(())
    }

    #[test]
    fn platform_phase_and_missing_ledger_must_not_open_or_create() -> TestResult {
        let f = Fixture::new()?;
        let registry = Registry::default();
        assert!(f.open(&registry, Mode::Provision).is_err());
        f.platform(0)?;
        assert!(matches!(
            f.open(&registry, Mode::Provision),
            Err(NativeError::EnrollmentScope)
        ));
        f.platform(2)?;
        assert!(matches!(
            f.open(&registry, Mode::Provision),
            Err(NativeError::EnrollmentScope)
        ));
        assert!(f.open(&registry, Mode::Existing).is_err());
        assert!(
            !f.dir
                .path()
                .join(PIN_NAMESPACE)
                .join(f.scope.canonical_uuid())
                .join(ROOM_DIR)
                .exists()
        );
        Ok(())
    }

    #[cfg(feature = "vault-uia-chat")]
    #[test]
    fn relock_of_real_store_fences_a_late_completed_turn() -> TestResult {
        use crate::uia_room_store::{commit_turn, open_room};
        let f = Fixture::new()?;
        let registry = Registry::default();
        f.platform(1)?;
        let first = f
            .open(&registry, Mode::Provision)
            .expect("native enrollment");
        let chat = registry.chat(first, f.scope).expect("authorized chat");
        chat.ops.reserve(1).expect("reserve request");
        let task = chat.ops.take(1).expect("worker running");
        let room = chat.owner.room_id();
        let lease = chat.owner.lease();
        let before = open_room(lease.as_ref(), room).expect("committed history");
        assert!(registry.revoke(first, f.scope));
        assert!(task.is_cancelled());
        assert!(commit_turn(lease.as_ref(), &before, "private", "late answer").is_err());
        assert!(!chat.ops.finish(1));
        f.platform(2)?;
        let next = f
            .open(&registry, Mode::Existing)
            .expect("re-open real vault");
        let registered = registry.chat(next, f.scope).expect("new authority");
        assert_eq!(
            open_room(registered.owner.lease().as_ref(), room)
                .expect("verify stored")
                .revision(),
            0
        );
        Ok(())
    }

    #[test]
    fn existing_uncharged_and_pending_pin_journal_fail_closed() -> TestResult {
        let f = Fixture::new()?;
        let registry = Registry::default();
        f.platform(1)?;
        let first = f
            .open(&registry, Mode::Provision)
            .expect("native enrollment");
        assert!(registry.revoke(first, f.scope));
        f.platform(2)?;
        let scoped = f
            .dir
            .path()
            .join(PIN_NAMESPACE)
            .join(f.scope.canonical_uuid());
        std::fs::File::create_new(scoped.join("pin.pending"))?;
        assert!(matches!(
            f.open(&registry, Mode::Existing),
            Err(NativeError::Recovery)
        ));
        std::fs::remove_file(scoped.join("pin.pending"))?;
        let state = scoped.join("state.bin");
        let mut wire = std::fs::read(&state)?;
        wire[38] = 0;
        let tag = digest(&SHA256, &wire[..55]);
        wire[55..].copy_from_slice(tag.as_ref());
        std::fs::write(&state, &wire)?;
        assert!(matches!(
            f.open(&registry, Mode::Existing),
            Err(NativeError::Denied)
        ));
        Ok(())
    }

    #[test]
    fn wrong_frame_scope_wiped_and_no_claim() -> TestResult {
        let f = Fixture::new()?;
        let mut bytes = f.input;
        bytes[25] ^= 0x70;
        bytes[24] ^= 1;
        assert!(matches!(
            decode_frame(f.scope, &mut bytes),
            Err(NativeError::Denied)
        ));
        assert_eq!(bytes, [0; 89]);
        Ok(())
    }
}
