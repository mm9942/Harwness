//! The Root-Hub's durable generation authority for one explicitly selected store.
//!
//! This is not a location cache, an authenticator, or a network service. The
//! operator provisions it on a trusted local filesystem with private, stable
//! ancestry. Never share a store across hubs or restore an older snapshot as a
//! live authority. An existing missing/corrupt store is an error, not an empty
//! directory. Keep the lock sidecar; unlinking it splits the writer lock.

use std::collections::BTreeSet;
use std::fs::{self, DirBuilder, File};
use std::io::{self, Read};
use std::num::NonZeroU64;
use std::os::fd::AsFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use harw_fsutil::{
    AtomicWriteOptions, AtomicWriteOutcome, OpenMode, ensure_owned_directory,
    ensure_private_regular, open_beneath, open_dir_nofollow, same_file_at,
    write_atomic_with_outcome,
};
use serde::{Deserialize, Serialize};

use crate::route_admission::{CallerGrant, OwnerFence, RouteAdmission, RouteRefusal};

const RECORD: &str = "owners.yaml";
const LOCK: &str = ".owners.lock";
const VERSION: u32 = 1;
const MAX_ENTRIES: usize = 1024;
const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_ID_BYTES: usize = 255;

/// The separately authorized initial owner of a newly assigned session.
/// The Hub chooses the generation; callers cannot supply one.
#[derive(Debug)]
pub struct InitialOwner<'a> {
    pub tenant: &'a str,
    pub session_id: &'a str,
    pub node_id: &'a str,
    pub host_instance: &'a str,
    pub owner_epoch: u64,
}

/// Storage refusal. The selected authority must stop instead of choosing an
/// alternate owner on a storage failure or ambiguous publication.
#[derive(Debug)]
pub enum DirectoryError {
    Io(io::Error),
    Busy,
    InvalidState,
    Capacity,
    Conflict,
    GenerationExhausted,
    DurabilityUncertain(io::Error),
}

impl std::fmt::Display for DirectoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Io(_) => "owner directory I/O failed",
            Self::Busy => "owner directory is busy; retry with fresh state",
            Self::InvalidState => "owner directory state or scope is invalid",
            Self::Capacity => "owner directory reached its bound",
            Self::Conflict => "owner directory compare-and-set conflict",
            Self::GenerationExhausted => "owner generation is exhausted",
            Self::DurabilityUncertain(_) => "owner directory write durability is uncertain",
        })
    }
}

impl std::error::Error for DirectoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) | Self::DurabilityUncertain(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for DirectoryError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

type Result<T> = std::result::Result<T, DirectoryError>;

#[derive(Deserialize, Serialize)]
#[serde(remote = "OwnerFence", deny_unknown_fields)]
struct DiskFence {
    tenant: String,
    session_id: String,
    node_id: String,
    host_instance: String,
    generation: NonZeroU64,
    owner_epoch: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(transparent)]
struct Entry(#[serde(with = "DiskFence")] OwnerFence);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    owners: Vec<Entry>,
}

/// Versioned, private local file store. Writes are serialized across processes
/// by a fixed lock inode. It does not provide multi-Hub consensus or failover.
#[derive(Debug)]
pub struct OwnerDirectory {
    root: PathBuf,
    dir: File,
    lock: File,
}

impl OwnerDirectory {
    /// Explicitly provision a previously nonexistent absolute directory under
    /// an operator-controlled parent. Never replaces an existing directory.
    /// If provisioning fails after creation, repair is an operator action;
    /// reopening cannot silently initialize a fresh store.
    pub fn provision(root: &Path) -> Result<Self> {
        let parent = root
            .parent()
            .filter(|_| root.is_absolute())
            .ok_or(DirectoryError::InvalidState)?;
        let parent = File::from(open_dir_nofollow(parent)?);
        ensure_owned_directory(&parent)?;
        DirBuilder::new().mode(0o700).create(root)?;
        let store = Self::open_handles(root, true)?;
        {
            let _locked = store.lock()?;
            store.lock.sync_all()?;
            store.persist(&Document {
                version: VERSION,
                owners: Vec::new(),
            })?;
            parent.sync_all().map_err(DirectoryError::DurabilityUncertain)?;
        }
        Ok(store)
    }

    /// Open an already provisioned store, refusing missing or invalid state.
    pub fn open(root: &Path) -> Result<Self> {
        let store = Self::open_handles(root, false)?;
        {
            let _locked = store.lock()?;
            store.load()?;
        }
        Ok(store)
    }

    /// Read one exact tenant/session fence. Does not list or guess owners.
    /// This is an internal directory call, not an application grant.
    pub fn current(&self, tenant: &str, session_id: &str) -> Result<Option<OwnerFence>> {
        if !valid_id(tenant) || !valid_id(session_id) {
            return Err(DirectoryError::InvalidState);
        }
        let _locked = self.lock()?;
        Ok(self.load()?.owners.into_iter().find_map(|Entry(fence)| {
            (fence.tenant == tenant && fence.session_id == session_id).then_some(fence)
        }))
    }

    /// Apply the same durable lookup at the destination before ordinary owner
    /// attachment. The caller and local destination coordinates must be freshly
    /// authenticated (not replayed from routing headers).
    pub fn revalidate_at_owner(
        &self,
        decision: &RouteAdmission,
        local_node: &str,
        local_instance: &str,
        grant: Option<&CallerGrant>,
    ) -> std::result::Result<(), RouteRefusal> {
        let grant = decision.validate_caller(grant)?;
        let fence = self
            .current(&grant.tenant, decision.session_id())
            .map_err(|_| RouteRefusal::OwnerUnavailable)?;
        decision.revalidate_at_owner(local_node, local_instance, Some(grant), fence.as_ref())
    }

    /// Assign a never-before-owned tenant/session, only after the trusted
    /// control plane authorizes initial ownership. An existing row is never
    /// replaced here, even when the owner is offline.
    pub fn assign_initial(&self, owner: InitialOwner<'_>) -> Result<OwnerFence> {
        let candidate = OwnerFence {
            tenant: owner.tenant.to_owned(),
            session_id: owner.session_id.to_owned(),
            node_id: owner.node_id.to_owned(),
            host_instance: owner.host_instance.to_owned(),
            generation: NonZeroU64::MIN,
            owner_epoch: owner.owner_epoch,
        };
        if !valid_fence(&candidate) {
            return Err(DirectoryError::InvalidState);
        }
        let _locked = self.lock()?;
        let mut data = self.load()?;
        if data
            .owners
            .iter()
            .any(|Entry(fence)| same_scope(fence, &candidate))
        {
            return Err(DirectoryError::Conflict);
        }
        if data.owners.len() >= MAX_ENTRIES {
            return Err(DirectoryError::Capacity);
        }
        data.owners.push(Entry(candidate.clone()));
        self.persist(&data)?;
        Ok(candidate)
    }

    /// Explicitly change the incarnation on the same node after control-plane
    /// reconciliation. Compare the complete old fence and increment the
    /// Hub-issued generation under the lock. There is no move/failover API;
    /// the caller must establish that the previous owner is fenced before use.
    pub fn refresh_same_node(
        &self,
        expected: &OwnerFence,
        next_instance: &str,
        next_epoch: u64,
    ) -> Result<OwnerFence> {
        if !valid_fence(expected) || !valid_id(next_instance) {
            return Err(DirectoryError::InvalidState);
        }
        let _locked = self.lock()?;
        let mut data = self.load()?;
        let current = data
            .owners
            .iter_mut()
            .map(|entry| &mut entry.0)
            .find(|row| same_scope(row, expected))
            .ok_or(DirectoryError::Conflict)?;
        // An incarnation with the same instance must move its own epoch
        // forward. Epochs across distinct instances are not comparable.
        if &*current != expected
            || (current.host_instance == next_instance && next_epoch <= current.owner_epoch)
        {
            return Err(DirectoryError::Conflict);
        }
        current.generation = current
            .generation
            .checked_add(1)
            .ok_or(DirectoryError::GenerationExhausted)?;
        current.host_instance = next_instance.to_owned();
        current.owner_epoch = next_epoch;
        let result = current.clone();
        self.persist(&data)?;
        Ok(result)
    }

    fn open_handles(root: &Path, create: bool) -> Result<Self> {
        if !root.is_absolute() {
            return Err(DirectoryError::InvalidState);
        }
        let dir = File::from(open_dir_nofollow(root)?);
        ensure_owned_directory(&dir)?;
        if dir.metadata()?.mode() & 0o077 != 0 {
            return Err(DirectoryError::InvalidState);
        }
        let mode = if create {
            OpenMode {
                read: true,
                ..OpenMode::write_create_new(0o600)
            }
        } else {
            OpenMode::read_write()
        };
        let lock = open_beneath(dir.as_fd(), Path::new(LOCK), mode)?;
        check_file(&lock)?;
        Ok(Self {
            root: root.to_owned(),
            dir,
            lock,
        })
    }

    fn lock(&self) -> Result<File> {
        self.ensure_root_current()?;
        // A new open file description is essential: reusing the pinned
        // handle can make a same-process lock request appear already held.
        let lock = open_beneath(self.dir.as_fd(), Path::new(LOCK), OpenMode::read_write())?;
        check_file(&lock)?;
        lock.try_lock().map_err(|error| match error {
            fs::TryLockError::WouldBlock => DirectoryError::Busy,
            fs::TryLockError::Error(error) => DirectoryError::Io(error),
        })?;
        let pinned = self.lock.metadata()?;
        let locked = lock.metadata()?;
        if (pinned.dev(), pinned.ino()) != (locked.dev(), locked.ino())
            || !same_file_at(self.dir.as_fd(), Path::new(LOCK), &lock)?
        {
            return Err(DirectoryError::InvalidState);
        }
        Ok(lock)
    }

    fn ensure_root_current(&self) -> Result<()> {
        let current_dir = File::from(open_dir_nofollow(&self.root)?);
        ensure_owned_directory(&current_dir)?;
        let current = current_dir.metadata()?;
        let pinned = self.dir.metadata()?;
        if current.mode() & 0o077 != 0
            || (current.dev(), current.ino()) != (pinned.dev(), pinned.ino())
        {
            return Err(DirectoryError::InvalidState);
        }
        Ok(())
    }

    fn load(&self) -> Result<Document> {
        let file = open_beneath(self.dir.as_fd(), Path::new(RECORD), OpenMode::read_only())?;
        check_file(&file)?;
        if file.metadata()?.len() > MAX_BYTES as u64 {
            return Err(DirectoryError::Capacity);
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_BYTES {
            return Err(DirectoryError::Capacity);
        }
        let doc: Document =
            serde_yaml::from_slice(&bytes).map_err(|_| DirectoryError::InvalidState)?;
        if doc.version != VERSION || doc.owners.len() > MAX_ENTRIES {
            return Err(DirectoryError::InvalidState);
        }
        let mut scopes = BTreeSet::new();
        for Entry(fence) in &doc.owners {
            if !valid_fence(fence) || !scopes.insert((&fence.tenant, &fence.session_id)) {
                return Err(DirectoryError::InvalidState);
            }
        }
        // A visible rename can have followed a reported durability failure.
        // Establish the current directory entry before routing or changing it.
        self.dir.sync_all().map_err(DirectoryError::DurabilityUncertain)?;
        Ok(doc)
    }

    fn persist(&self, doc: &Document) -> Result<()> {
        let data = serde_yaml::to_string(doc).map_err(|_| DirectoryError::InvalidState)?;
        if data.len() > MAX_BYTES {
            return Err(DirectoryError::Capacity);
        }
        self.ensure_root_current()?;
        match write_atomic_with_outcome(
            &self.root.join(RECORD),
            data.as_bytes(),
            AtomicWriteOptions::private(),
        )? {
            AtomicWriteOutcome::Committed => self
                .dir
                .sync_all()
                .map_err(DirectoryError::DurabilityUncertain),
            AtomicWriteOutcome::DurabilityUncertain(error) => {
                Err(DirectoryError::DurabilityUncertain(error))
            }
        }
    }
}

fn check_file(file: &File) -> Result<()> {
    ensure_private_regular(file)?;
    if file.metadata()?.nlink() != 1 {
        return Err(DirectoryError::InvalidState);
    }
    Ok(())
}

pub(crate) fn valid_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_ID_BYTES
        && !value.chars().any(char::is_control)
}

fn valid_fence(fence: &OwnerFence) -> bool {
    [&fence.tenant, &fence.session_id, &fence.node_id, &fence.host_instance]
        .into_iter()
        .all(|id| valid_id(id))
}

fn same_scope(a: &OwnerFence, b: &OwnerFence) -> bool {
    a.tenant == b.tenant && a.session_id == b.session_id
}
