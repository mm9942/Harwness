//! Per-job file reservations for write-capable steps: one writer per
//! canonical path, epoch-fenced, all-or-nothing sets. No file I/O on the
//! reserved paths themselves.
//!
//! # Model
//! A reservation is a [`FileLeaseRecord`] stored in a
//! `harw_job_store::RecordStore` below `<job-root>/`[`FILE_LEASE_DIR`], one
//! record per reserved path. The record id is [`lease_id_for`] (a truncated
//! blake3 digest of the normalized path) because store ids are restricted to
//! `[A-Za-z0-9_-]`; the readable path lives in [`FileLeaseRecord::key`].
//! Paths are compared **lexically** after normalization (`a//b` and `a/b` are
//! the same key; `.`/`..`/absolute/empty/NUL paths are refused). The registry
//! never touches the filesystem entries it names, so it neither resolves
//! symlinks nor folds case: the trusted caller that maps a write target to a
//! workspace-relative path (see `Scope::rel_readable`) owns that.
//!
//! # Holders and fencing
//! A holder is the pair `(owner, cycle_id)`. The same holder re-reserving a
//! path is idempotent and may only move the recorded epoch forward; a holder
//! presenting an older epoch than the record is refused
//! ([`ReserveRefusal::StaleEpoch`]). A path held by another holder is never
//! taken over, whatever the epochs (takeover of dead holders belongs to the
//! cycle lease rebind, not to this registry).
//!
//! # Atomicity of a set
//! [`FileLeaseRegistry::reserve`] is all-or-nothing:
//!
//! 1. every path is validated and normalized before any record is written;
//! 2. the records are visited in **sorted key order**, so two reservers with
//!    overlapping sets always meet at the same first shared key and one of
//!    them refuses before it has created anything beyond it;
//! 3. each key is decided **inside the record lock** (`create` for a free key,
//!    otherwise `update_locked`, which re-reads the owner under the lock), so
//!    no check-then-create window exists for a single path;
//! 4. on a refusal midway, the records this very call created are removed
//!    again (rollback) before the refusal is returned. Records the holder
//!    already had keep their lease; only their epoch may have moved forward.
//!    The rollback is fenced with the epoch of the call being undone: a
//!    record that a newer epoch of the same holder has refreshed in the
//!    meantime belongs to that newer reservation and is kept.
//!
//! What is *not* atomic: other readers can briefly observe a half-reserved
//! set while a call is in progress, and a crash (or an I/O failure during
//! rollback, which is logged) can leave part of a set behind. Those records
//! belong to the same holder, so repeating the reserve is idempotent and
//! [`FileLeaseRegistry::release`] with the same set cleans them up. At no
//! point do two holders own the same path.
//!
//! # Store mechanics
//! Creation, refresh and lookup reuse `RecordStore` (per-record try-lock,
//! temp + rename + directory `fsync`, corrupt records quarantined). Locks
//! never block: a record locked by a concurrent writer surfaces as
//! [`ReserveRefusal::Store`] and the caller may retry. `RecordStore` has no
//! delete, so removal takes the public record lock itself, re-checks the
//! holder under it and unlinks `records/<id>.json` (the documented, stable
//! layout) followed by a directory `fsync`.

use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::{Component, Path};

use harw_job_store::{RECORDS_DIR, RecordStore, StoreError, StoreRecord};
use harw_types::ContentDigest;
use serde::{Deserialize, Serialize};

/// Schema version of [`FileLeaseRecord`].
pub const FILE_LEASE_SCHEMA: u32 = 1;

/// Directory of the [`FileLeaseRegistry`] below the durable job root
/// (`<job-root>/intent_cycle/leases/`), next to
/// [`CYCLE_STORE_DIR`](crate::cycle_runtime::CYCLE_STORE_DIR).
pub const FILE_LEASE_DIR: &str = "intent_cycle/leases";

/// Longest accepted path, same bound as the read/write target maps.
const MAX_PATH_BYTES: usize = 4096;
/// Longest accepted owner or cycle id.
const MAX_HOLDER_BYTES: usize = 256;
/// Characters of a rejected path echoed back in [`ReserveRefusal::InvalidPath`].
const MAX_ECHOED_PATH_CHARS: usize = 200;
/// Attempts per key when a record appears or vanishes between `create` and
/// `update_locked` (another holder releasing at the same moment).
const RESERVE_ATTEMPTS: u8 = 3;
/// Hex characters of the path digest used as record id (128 bits).
const LEASE_ID_HEX_CHARS: usize = 32;

/// One reserved path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileLeaseRecord {
    pub schema_version: u32,
    /// Store id of this record: [`lease_id_for`] of [`FileLeaseRecord::key`].
    pub key_id: String,
    /// Normalized workspace-relative path (the readable form of the key).
    pub key: String,
    /// Worker or agent that holds the reservation.
    pub owner: String,
    /// Cycle the reservation belongs to; `(owner, cycle_id)` is the holder.
    pub cycle_id: String,
    /// Lease epoch the holder reserved under (only ever moves forward).
    pub epoch: u64,
    pub acquired_at: jiff::Timestamp,
}

impl StoreRecord for FileLeaseRecord {
    fn record_id(&self) -> &str {
        &self.key_id
    }
}

/// The reservation a successful [`FileLeaseRegistry::reserve`] returns and
/// [`FileLeaseRegistry::release`] consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseSet {
    pub cycle_id: String,
    pub owner: String,
    pub epoch: u64,
    /// Normalized paths, in sorted order.
    pub keys: BTreeSet<String>,
}

/// Why a reserve, release or lookup was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReserveRefusal {
    /// `path` is held by another holder (`owner`); nothing was reserved.
    AlreadyOwned { path: String, owner: String },
    /// A record carries a newer epoch than the presenter's.
    StaleEpoch,
    /// The path is empty, absolute, too long, or contains `.`/`..`/NUL.
    InvalidPath(String),
    /// The underlying store failed (I/O, contention, corruption, invalid
    /// holder identity); rendered cause.
    Store(String),
}

impl fmt::Display for ReserveRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyOwned { path, owner } => {
                write!(f, "file '{path}' is already reserved by '{owner}'")
            }
            Self::StaleEpoch => f.write_str("file lease epoch is older than the recorded lease"),
            Self::InvalidPath(path) => write!(f, "invalid file lease path '{path}'"),
            Self::Store(detail) => write!(f, "file lease store: {detail}"),
        }
    }
}

impl std::error::Error for ReserveRefusal {}

impl From<StoreError> for ReserveRefusal {
    fn from(error: StoreError) -> Self {
        Self::Store(error.to_string())
    }
}

/// Record id for `path`: the first 32 hex characters of the blake3 digest of
/// the normalized path. Always a valid store id (`[0-9a-f]{32}`). Equivalent
/// spellings (`a//b`, `a/b`) map to one id; a path that cannot be normalized
/// is hashed verbatim (the registry refuses such paths before using the id).
#[must_use]
pub fn lease_id_for(path: &str) -> String {
    match normalize_key(path) {
        Ok(key) => id_of_normalized(&key),
        Err(_) => id_of_normalized(path),
    }
}

fn id_of_normalized(key: &str) -> String {
    let mut id = ContentDigest::of(key.as_bytes()).to_string();
    id.truncate(LEASE_ID_HEX_CHARS);
    id
}

/// Per-job registry of file reservations.
pub struct FileLeaseRegistry {
    records: RecordStore<FileLeaseRecord>,
}

impl FileLeaseRegistry {
    /// Opens (creating if needed) the registry at
    /// `<job_root>/`[`FILE_LEASE_DIR`].
    ///
    /// # Errors
    /// [`ReserveRefusal::Store`] if the directory cannot be created or opened.
    pub fn open_in_job_root(job_root: &Path) -> Result<Self, ReserveRefusal> {
        let records = RecordStore::create_ambient(&job_root.join(FILE_LEASE_DIR))?;
        Ok(Self { records })
    }

    /// Reserves every path or nothing. A path already owned by another
    /// `(owner, cycle)` refuses the whole set; the same owner and cycle
    /// re-reserving is idempotent (the epoch is moved forward if newer).
    ///
    /// Keys are visited in sorted order and decided inside each record lock;
    /// on a refusal the records created by this call are removed again,
    /// fenced with this call's own `epoch` (a record a newer epoch has
    /// refreshed meanwhile is kept). See the module docs for what is and is
    /// not atomic.
    ///
    /// # Errors
    /// [`ReserveRefusal::InvalidPath`] for a path that cannot be a plain
    /// workspace-relative key, [`ReserveRefusal::AlreadyOwned`] for a path
    /// held by another holder, [`ReserveRefusal::StaleEpoch`] if one of the
    /// holder's own records carries a newer epoch, [`ReserveRefusal::Store`]
    /// for an invalid holder identity or a store failure (including a record
    /// locked by a concurrent writer).
    pub fn reserve(
        &self,
        paths: &BTreeSet<String>,
        owner: &str,
        cycle_id: &str,
        epoch: u64,
    ) -> Result<LeaseSet, ReserveRefusal> {
        check_holder(owner, cycle_id)?;
        let mut keys = BTreeSet::new();
        for path in paths {
            keys.insert(normalize_key(path)?);
        }
        let holder = Holder { owner, cycle_id };
        let mut created: Vec<Entry> = Vec::new();
        for key in &keys {
            let entry = Entry::new(key);
            match self.reserve_key(&entry, &holder, epoch) {
                Ok(Reserved::Created) => created.push(entry),
                Ok(Reserved::Held) => {}
                Err(refusal) => {
                    self.roll_back(&created, &holder, epoch);
                    return Err(refusal);
                }
            }
        }
        Ok(LeaseSet {
            cycle_id: cycle_id.to_owned(),
            owner: owner.to_owned(),
            epoch,
            keys,
        })
    }

    /// Releases the set. A record of this holder whose epoch is newer than
    /// `epoch` refuses ([`ReserveRefusal::StaleEpoch`]) before anything is
    /// removed. Unknown keys, and keys now held by a different holder, are
    /// ignored, so a repeated release is a no-op and can never free someone
    /// else's reservation. The set's own `epoch` field is informational;
    /// `epoch` is the fencing epoch the caller presents now.
    ///
    /// Each record is removed under its own lock; if a store failure stops
    /// the call midway, the keys already removed stay removed and calling
    /// again with the same set finishes the job.
    ///
    /// # Errors
    /// [`ReserveRefusal::StaleEpoch`], [`ReserveRefusal::InvalidPath`] for a
    /// hand-built set with an unusable key, [`ReserveRefusal::Store`].
    pub fn release(&self, set: &LeaseSet, epoch: u64) -> Result<(), ReserveRefusal> {
        let holder = Holder {
            owner: &set.owner,
            cycle_id: &set.cycle_id,
        };
        let mut entries = BTreeSet::new();
        for key in &set.keys {
            entries.insert(normalize_key(key)?);
        }
        let entries: Vec<Entry> = entries.iter().map(|key| Entry::new(key.as_str())).collect();
        // Pass 1 (lock-free): a stale presenter changes nothing at all.
        for entry in &entries {
            match self.records.load(&entry.id) {
                Ok(record) => {
                    check_consistent(&record, entry)?;
                    if holder.matches(&record) && record.epoch > epoch {
                        return Err(ReserveRefusal::StaleEpoch);
                    }
                }
                Err(StoreError::NotFound { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        // Pass 2: remove under each record's lock, re-checking the holder.
        for entry in &entries {
            self.remove_held(entry, &holder, epoch)?;
        }
        Ok(())
    }

    /// The current reservation of `path`, if any. Lock-free snapshot.
    ///
    /// # Errors
    /// [`ReserveRefusal::InvalidPath`], [`ReserveRefusal::Store`] (including
    /// a corrupt or inconsistent record: ownership is then unknown).
    pub fn owner_of(&self, path: &str) -> Result<Option<FileLeaseRecord>, ReserveRefusal> {
        let entry = Entry::new(&normalize_key(path)?);
        match self.records.load(&entry.id) {
            Ok(record) => {
                check_consistent(&record, &entry)?;
                Ok(Some(record))
            }
            Err(StoreError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    // Decides one key inside the record lock: create it if free, otherwise
    // refresh it if (and only if) the same holder already owns it.
    fn reserve_key(
        &self,
        entry: &Entry,
        holder: &Holder<'_>,
        epoch: u64,
    ) -> Result<Reserved, ReserveRefusal> {
        for _ in 0..RESERVE_ATTEMPTS {
            let record = FileLeaseRecord {
                schema_version: FILE_LEASE_SCHEMA,
                key_id: entry.id.clone(),
                key: entry.key.clone(),
                owner: holder.owner.to_owned(),
                cycle_id: holder.cycle_id.to_owned(),
                epoch,
                acquired_at: jiff::Timestamp::now(),
            };
            match self.records.create(&record) {
                Ok(()) => return Ok(Reserved::Created),
                Err(StoreError::AlreadyExists { .. }) => {}
                Err(error) => return Err(error.into()),
            }
            let refreshed = self.records.update_locked(&entry.id, |existing| {
                refresh(existing, entry, holder, epoch)
            });
            match refreshed {
                Ok(()) => return Ok(Reserved::Held),
                Err(Failure::Refused(refusal)) => return Err(refusal),
                // Released between `create` and `update_locked`: decide again.
                Err(Failure::Store(StoreError::NotFound { .. })) => {}
                Err(Failure::Store(error)) => return Err(error.into()),
            }
        }
        Err(ReserveRefusal::Store(format!(
            "lease record for '{}' kept changing; try again",
            entry.key
        )))
    }

    // Removes the records this call created, fenced with the epoch this call
    // reserved under (never `u64::MAX`: that would also delete a record that
    // a newer epoch of the same holder refreshed in the meantime, and the
    // path could then be reserved by someone else although that newer epoch
    // received a successful `LeaseSet`). A record advanced beyond `epoch` is
    // reported by `remove_under_lock` as `StaleEpoch`; it belongs to the
    // newer reservation, so it is kept and is no failure of the rollback.
    // Any other failure is logged, not returned: the original refusal is the
    // more useful answer, and the leftovers belong to the same holder
    // (idempotent re-reserve / release).
    fn roll_back(&self, created: &[Entry], holder: &Holder<'_>, epoch: u64) {
        for entry in created.iter().rev() {
            match self.remove_held(entry, holder, epoch) {
                Ok(()) => {}
                Err(ReserveRefusal::StaleEpoch) => {
                    tracing::debug!(
                        key = %entry.key,
                        "file lease rollback kept a record advanced by a newer epoch"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        key = %entry.key,
                        error = %error,
                        "file lease rollback failed; the holder keeps the lease until release"
                    );
                }
            }
        }
    }

    fn remove_held(
        &self,
        entry: &Entry,
        holder: &Holder<'_>,
        epoch: u64,
    ) -> Result<(), ReserveRefusal> {
        let lock = self.records.lock(&entry.id)?;
        let removed = self.remove_under_lock(entry, holder, epoch);
        let unlocked = lock.release();
        match (removed, unlocked) {
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error.into()),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    fn remove_under_lock(
        &self,
        entry: &Entry,
        holder: &Holder<'_>,
        epoch: u64,
    ) -> Result<(), ReserveRefusal> {
        let record = match self.records.load(&entry.id) {
            Ok(record) => record,
            Err(StoreError::NotFound { .. }) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        check_consistent(&record, entry)?;
        if !holder.matches(&record) {
            return Ok(());
        }
        if record.epoch > epoch {
            return Err(ReserveRefusal::StaleEpoch);
        }
        let records = self
            .records
            .root_dir()
            .open_dir(RECORDS_DIR)
            .map_err(io_refusal)?;
        // `<id>.json` is the documented, stable record layout of RecordStore.
        match records.remove_file(format!("{}.json", entry.id)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io_refusal(error)),
        }
        // Without the directory sync a crash could resurrect the lease.
        records
            .open(".")
            .and_then(|directory| directory.sync_all())
            .map_err(io_refusal)
    }
}

/// `(owner, cycle)` presenting a reservation.
struct Holder<'a> {
    owner: &'a str,
    cycle_id: &'a str,
}

impl Holder<'_> {
    fn matches(&self, record: &FileLeaseRecord) -> bool {
        record.owner == self.owner && record.cycle_id == self.cycle_id
    }
}

/// A normalized key together with its record id.
struct Entry {
    key: String,
    id: String,
}

impl Entry {
    fn new(key: &str) -> Self {
        Self {
            key: key.to_owned(),
            id: id_of_normalized(key),
        }
    }
}

enum Reserved {
    Created,
    Held,
}

// Error type of the `update_locked` closure: a refusal from our own decision
// or a store failure surfaced by the store itself.
enum Failure {
    Refused(ReserveRefusal),
    Store(StoreError),
}

impl From<StoreError> for Failure {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

// Runs under the record lock: the only place a held record is judged.
fn refresh(
    existing: &mut FileLeaseRecord,
    entry: &Entry,
    holder: &Holder<'_>,
    epoch: u64,
) -> Result<(), Failure> {
    check_consistent(existing, entry).map_err(Failure::Refused)?;
    if !holder.matches(existing) {
        return Err(Failure::Refused(ReserveRefusal::AlreadyOwned {
            path: entry.key.clone(),
            owner: existing.owner.clone(),
        }));
    }
    if existing.epoch > epoch {
        return Err(Failure::Refused(ReserveRefusal::StaleEpoch));
    }
    existing.epoch = epoch;
    Ok(())
}

// A record that is not the one `entry` names (unsupported schema, id or key
// mismatch such as a digest collision or a hand-edited file) leaves ownership
// unknown, so every decision fails closed.
fn check_consistent(record: &FileLeaseRecord, entry: &Entry) -> Result<(), ReserveRefusal> {
    if record.schema_version == FILE_LEASE_SCHEMA
        && record.key_id == entry.id
        && record.key == entry.key
    {
        return Ok(());
    }
    Err(ReserveRefusal::Store(format!(
        "lease record '{}' is inconsistent or has an unsupported schema",
        entry.id
    )))
}

fn check_holder(owner: &str, cycle_id: &str) -> Result<(), ReserveRefusal> {
    let usable = |value: &str| {
        !value.trim().is_empty()
            && value.len() <= MAX_HOLDER_BYTES
            && !value.chars().any(char::is_control)
    };
    if usable(owner) && usable(cycle_id) {
        return Ok(());
    }
    Err(ReserveRefusal::Store(
        "lease holder needs a non-empty owner and cycle id without control characters".to_owned(),
    ))
}

/// Lexical normalization: only normal components, joined by `/`.
fn normalize_key(path: &str) -> Result<String, ReserveRefusal> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.contains('\0') {
        return Err(invalid_path(path));
    }
    let mut parts: Vec<&str> = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => match part.to_str() {
                Some(part) => parts.push(part),
                None => return Err(invalid_path(path)),
            },
            _ => return Err(invalid_path(path)),
        }
    }
    if parts.is_empty() {
        return Err(invalid_path(path));
    }
    Ok(parts.join("/"))
}

fn invalid_path(path: &str) -> ReserveRefusal {
    ReserveRefusal::InvalidPath(path.chars().take(MAX_ECHOED_PATH_CHARS).collect())
}

fn io_refusal(error: io::Error) -> ReserveRefusal {
    ReserveRefusal::from(StoreError::Io(error))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{Entry, FileLeaseRegistry, Holder, LeaseSet, ReserveRefusal, lease_id_for};
    use crate::test_support::{TestResult, ctx, missing};

    struct Fixture {
        _temp: tempfile::TempDir,
        registry: FileLeaseRegistry,
    }

    fn fixture() -> TestResult<Fixture> {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let registry = FileLeaseRegistry::open_in_job_root(temp.path()).map_err(ctx("open"))?;
        Ok(Fixture {
            _temp: temp,
            registry,
        })
    }

    fn set(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    fn reserve(
        fx: &Fixture,
        paths: &[&str],
        owner: &str,
        cycle_id: &str,
        epoch: u64,
    ) -> TestResult<LeaseSet> {
        fx.registry
            .reserve(&set(paths), owner, cycle_id, epoch)
            .map_err(ctx("reserve"))
    }

    fn holder_of(fx: &Fixture, path: &str) -> TestResult<Option<String>> {
        let record = fx.registry.owner_of(path).map_err(ctx("owner_of"))?;
        Ok(record.map(|record| record.owner))
    }

    #[test]
    fn two_owners_cannot_reserve_the_same_file() -> TestResult {
        let fx = fixture()?;
        let first = reserve(&fx, &["src/lib.rs"], "worker-a", "cycle-a", 1)?;
        assert_eq!(first.keys, set(&["src/lib.rs"]));
        let refused = fx
            .registry
            .reserve(&set(&["src/lib.rs"]), "worker-b", "cycle-b", 1);
        assert_eq!(
            refused,
            Err(ReserveRefusal::AlreadyOwned {
                path: "src/lib.rs".to_owned(),
                owner: "worker-a".to_owned(),
            })
        );
        // Same owner id in another cycle is a different holder as well.
        let other_cycle = fx
            .registry
            .reserve(&set(&["src/lib.rs"]), "worker-a", "cycle-b", 1);
        assert!(matches!(
            other_cycle,
            Err(ReserveRefusal::AlreadyOwned { .. })
        ));
        assert_eq!(holder_of(&fx, "src/lib.rs")?.as_deref(), Some("worker-a"));
        Ok(())
    }

    #[test]
    fn overlapping_sets_are_refused_atomically() -> TestResult {
        let fx = fixture()?;
        reserve(&fx, &["a.rs", "b.rs"], "worker-x", "cycle-x", 1)?;
        let refused = fx
            .registry
            .reserve(&set(&["b.rs", "c.rs"]), "worker-y", "cycle-y", 1);
        assert_eq!(
            refused,
            Err(ReserveRefusal::AlreadyOwned {
                path: "b.rs".to_owned(),
                owner: "worker-x".to_owned(),
            })
        );
        assert_eq!(holder_of(&fx, "c.rs")?, None);
        assert_eq!(holder_of(&fx, "a.rs")?.as_deref(), Some("worker-x"));
        assert_eq!(holder_of(&fx, "b.rs")?.as_deref(), Some("worker-x"));
        Ok(())
    }

    #[test]
    fn mid_set_refusal_rolls_back_this_calls_creations() -> TestResult {
        let fx = fixture()?;
        reserve(&fx, &["m.rs"], "worker-x", "cycle-x", 1)?;
        // Sorted order: `a.rs` and `b.rs` are created before `m.rs` refuses.
        let refused =
            fx.registry
                .reserve(&set(&["a.rs", "b.rs", "m.rs"]), "worker-y", "cycle-y", 1);
        assert!(matches!(refused, Err(ReserveRefusal::AlreadyOwned { .. })));
        assert_eq!(holder_of(&fx, "a.rs")?, None);
        assert_eq!(holder_of(&fx, "b.rs")?, None);
        assert_eq!(holder_of(&fx, "m.rs")?.as_deref(), Some("worker-x"));
        // The rolled-back paths are free for anyone again.
        reserve(&fx, &["a.rs", "b.rs"], "worker-z", "cycle-z", 1)?;
        Ok(())
    }

    #[test]
    fn rollback_keeps_records_advanced_by_a_newer_epoch() -> TestResult {
        let fx = fixture()?;
        let holder = Holder {
            owner: "worker-x",
            cycle_id: "cycle-x",
        };
        // `z.rs` belongs to another holder: a set containing it is refused
        // after `a.rs` (sorted before `z.rs`) has been decided.
        reserve(&fx, &["z.rs"], "worker-y", "cycle-y", 1)?;

        // Epoch 1 of X: its in-flight `reserve({a.rs, z.rs})` has created
        // `a.rs` (it is in the call's `created` list) and has not reached
        // `z.rs` yet.
        reserve(&fx, &["a.rs"], "worker-x", "cycle-x", 1)?;
        let created = [Entry::new("a.rs")];
        // Meanwhile epoch 2 of the same holder refreshes `a.rs` and receives
        // a successful `LeaseSet`.
        let newer = reserve(&fx, &["a.rs"], "worker-x", "cycle-x", 2)?;
        assert_eq!(newer.epoch, 2);

        // Sequential public calls cannot reproduce this interleaving: once
        // epoch 2 holds `a.rs`, an epoch-1 `reserve` is fenced out at `a.rs`
        // and never creates anything. The rollback step is therefore driven
        // directly with the epoch-1 call's own epoch: the epoch-2 record
        // must survive it.
        let fenced = fx
            .registry
            .reserve(&set(&["a.rs", "z.rs"]), "worker-x", "cycle-x", 1);
        assert_eq!(fenced, Err(ReserveRefusal::StaleEpoch));
        fx.registry.roll_back(&created, &holder, 1);
        let record = fx
            .registry
            .owner_of("a.rs")
            .map_err(ctx("owner_of"))?
            .ok_or_else(missing(
                "a.rs must still be reserved after the epoch-1 rollback",
            ))?;
        assert_eq!(record.owner, "worker-x");
        assert_eq!(record.cycle_id, "cycle-x");
        assert_eq!(record.epoch, 2);
        // Nobody else can take the path over while epoch 2 holds it.
        let takeover = fx
            .registry
            .reserve(&set(&["a.rs"]), "worker-y", "cycle-y", 3);
        assert_eq!(
            takeover,
            Err(ReserveRefusal::AlreadyOwned {
                path: "a.rs".to_owned(),
                owner: "worker-x".to_owned(),
            })
        );
        // The foreign record is untouched as well.
        assert_eq!(holder_of(&fx, "z.rs")?.as_deref(), Some("worker-y"));

        // A rollback at the record's own epoch still removes it, and a
        // rollback for a record that is gone is a no-op.
        fx.registry.roll_back(&created, &holder, 2);
        assert_eq!(holder_of(&fx, "a.rs")?, None);
        fx.registry.roll_back(&created, &holder, 2);
        assert_eq!(holder_of(&fx, "a.rs")?, None);
        Ok(())
    }

    #[test]
    fn stale_epoch_cannot_release() -> TestResult {
        let fx = fixture()?;
        let leased = reserve(&fx, &["a.rs", "b.rs"], "worker-a", "cycle-a", 5)?;
        assert_eq!(
            fx.registry.release(&leased, 4),
            Err(ReserveRefusal::StaleEpoch)
        );
        assert_eq!(holder_of(&fx, "a.rs")?.as_deref(), Some("worker-a"));
        assert_eq!(holder_of(&fx, "b.rs")?.as_deref(), Some("worker-a"));
        fx.registry.release(&leased, 5).map_err(ctx("release"))?;
        assert_eq!(holder_of(&fx, "a.rs")?, None);
        assert_eq!(holder_of(&fx, "b.rs")?, None);
        Ok(())
    }

    #[test]
    fn release_is_idempotent() -> TestResult {
        let fx = fixture()?;
        let first = reserve(&fx, &["a.rs"], "worker-a", "cycle-a", 1)?;
        fx.registry
            .release(&first, 1)
            .map_err(ctx("first release"))?;
        fx.registry
            .release(&first, 1)
            .map_err(ctx("repeated release"))?;
        // A repeated release never frees a reservation that is held by
        // someone else by now.
        reserve(&fx, &["a.rs"], "worker-b", "cycle-b", 2)?;
        fx.registry
            .release(&first, 1)
            .map_err(ctx("late release of a foreign key"))?;
        assert_eq!(holder_of(&fx, "a.rs")?.as_deref(), Some("worker-b"));
        Ok(())
    }

    #[test]
    fn same_owner_reserve_is_idempotent() -> TestResult {
        let fx = fixture()?;
        let first = reserve(&fx, &["a.rs", "b.rs"], "worker-a", "cycle-a", 1)?;
        let acquired = fx
            .registry
            .owner_of("a.rs")
            .map_err(ctx("owner_of"))?
            .map(|record| record.acquired_at);
        let again = reserve(&fx, &["a.rs", "b.rs"], "worker-a", "cycle-a", 1)?;
        assert_eq!(first, again);
        let newer = reserve(&fx, &["a.rs", "b.rs"], "worker-a", "cycle-a", 2)?;
        assert_eq!(newer.epoch, 2);
        let record = fx.registry.owner_of("a.rs").map_err(ctx("owner_of"))?;
        assert_eq!(record.as_ref().map(|record| record.epoch), Some(2));
        assert_eq!(record.map(|record| record.acquired_at), acquired);
        // An older presenter is fenced out, and the refusal is all-or-nothing.
        let stale = fx
            .registry
            .reserve(&set(&["a.rs", "z.rs"]), "worker-a", "cycle-a", 1);
        assert_eq!(stale, Err(ReserveRefusal::StaleEpoch));
        assert_eq!(holder_of(&fx, "z.rs")?, None);
        Ok(())
    }

    #[test]
    fn invalid_paths_are_refused_before_any_record_is_written() -> TestResult {
        let fx = fixture()?;
        for bad in ["", "/etc/passwd", "../outside.rs", "src/../../x", "./a.rs"] {
            let refused = fx
                .registry
                .reserve(&set(&["ok.rs", bad]), "worker-a", "cycle-a", 1);
            assert!(
                matches!(refused, Err(ReserveRefusal::InvalidPath(_))),
                "{bad:?} must be refused, got {refused:?}"
            );
        }
        assert_eq!(holder_of(&fx, "ok.rs")?, None);
        let empty_owner = fx.registry.reserve(&set(&["ok.rs"]), " ", "cycle-a", 1);
        assert!(matches!(empty_owner, Err(ReserveRefusal::Store(_))));
        assert_eq!(holder_of(&fx, "ok.rs")?, None);
        Ok(())
    }

    #[test]
    fn equivalent_spellings_share_one_reservation() -> TestResult {
        let fx = fixture()?;
        assert_eq!(lease_id_for("src//lib.rs"), lease_id_for("src/lib.rs"));
        assert_eq!(lease_id_for("src/lib.rs").len(), 32);
        let first = reserve(&fx, &["src//lib.rs"], "worker-a", "cycle-a", 1)?;
        assert_eq!(first.keys, set(&["src/lib.rs"]));
        let refused = fx
            .registry
            .reserve(&set(&["src/lib.rs"]), "worker-b", "cycle-b", 1);
        assert!(matches!(refused, Err(ReserveRefusal::AlreadyOwned { .. })));
        assert_eq!(holder_of(&fx, "src//lib.rs")?.as_deref(), Some("worker-a"));
        Ok(())
    }
}
