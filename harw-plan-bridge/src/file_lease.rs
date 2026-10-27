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
//! # Reservation membership
//! Every [`FileLeaseRegistry::reserve`] call draws a fresh opaque id (returned
//! as [`LeaseSet::reservation`]). A record keeps the ids of **every** call
//! that currently holds it ([`FileLeaseRecord::reservations`]): creating a
//! record stores the call's id, a same-holder refresh adds the call's id and
//! keeps the ids already there. A call's id leaves the set only when that
//! call fails and rolls back; a successful call stays a member until the
//! record is released. Two interleaved calls of the same holder (even at the
//! same epoch) therefore share a record that lives as long as one of them
//! succeeds and disappears when all of them have failed.
//!
//! A record that cannot name its holders is never deleted by a rollback. That
//! covers a record written before membership existed (empty set: held by an
//! unknown reservation) and the calls beyond a small cap on the member list
//! (64 ids); both are represented by the member `"untracked"`, which the next
//! same-holder reserve adds and no rollback removes. Only
//! [`FileLeaseRegistry::release`] frees such a record.
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
//! 4. on a refusal midway, the call leaves every record it touched, created
//!    **or** refreshed, before the refusal is returned (rollback): its id is
//!    removed from the record's members, or, if it is the only member, the
//!    record is deleted. A record that another call of the same holder still
//!    claims (a successful one, or one still in flight) is kept; the last
//!    member to fail deletes it. So all-or-nothing also holds for interleaved calls of
//!    the same holder: if every call that touched a path fails, the path is
//!    free again. The deletion is fenced with the epoch of the call being
//!    undone, as `release` is: a call never removes the last claim of a
//!    record that a newer epoch has advanced (it presents a stale epoch); the
//!    holder of that epoch frees it with `release`.
//!
//! What is *not* atomic: other readers can briefly observe a half-reserved
//! set while a call is in progress, and a crash (or an I/O failure during
//! rollback, which is logged) can leave part of a set behind, or an id in a
//! record's members that no live call owns any more. Those records belong to
//! the same holder, so repeating the reserve is idempotent and
//! [`FileLeaseRegistry::release`] with the same set cleans them up. At no
//! point do two holders own the same path.
//!
//! [`FileLeaseRegistry::release`] works on keys, not on membership: it frees
//! every record of the presenting holder that the set names, whoever its
//! members are. Membership could not replace that: a crashed call leaves an
//! id behind that no later set can name, and records without members
//! (`"untracked"`) would never be freeable.
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
use harw_types::{ContentDigest, SessionId};
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
/// Most reservation ids one record lists. Further calls are represented by
/// [`UNTRACKED_MEMBER`], which keeps the record from being rolled back.
const MAX_RESERVATION_MEMBERS: usize = 64;
/// Member standing for reservations the record cannot name (written before
/// membership existed, or beyond [`MAX_RESERVATION_MEMBERS`]). Never a UUID,
/// so no live call id equals it, and no rollback removes it.
const UNTRACKED_MEMBER: &str = "untracked";

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
    /// Ids ([`LeaseSet::reservation`]) of the reserve calls that currently
    /// hold this record: a create stores the call's id, a same-holder refresh
    /// adds it and keeps the others. A failed call's rollback removes its own
    /// id, and the record is deleted when the last member rolls back; a
    /// successful call stays a member until release. A record written before this field
    /// existed loads with an empty set, which counts as held by an unknown
    /// reservation: no rollback deletes it, and the next same-holder reserve
    /// adds the member `"untracked"` that no rollback removes (the same member
    /// stands for calls beyond a cap of 64 ids).
    #[serde(default)]
    pub reservations: BTreeSet<String>,
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
    /// Opaque id of the [`FileLeaseRegistry::reserve`] call that produced
    /// this set; a member of [`FileLeaseRecord::reservations`] of every
    /// record the call created or refreshed (beyond the member cap of a
    /// record it is represented by the member `"untracked"` instead).
    /// Informational for [`FileLeaseRegistry::release`], which works on keys.
    pub reservation: String,
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
    /// on a refusal the call leaves every record it created or refreshed:
    /// its reservation id is removed from the record's members, and a record
    /// with no other member is deleted (fenced with this call's own `epoch`).
    /// A record that another call of the same holder still claims is kept.
    /// See the module docs for what is and is not atomic.
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
        // `harw_types` has no dedicated reservation id; a v4 UUID is opaque
        // and unique across calls and processes, which is all that is needed.
        let reservation = SessionId::new().0;
        // Every key this call decided, created or refreshed: the rollback
        // leaves all of them, not only the ones it created.
        let mut touched: Vec<Entry> = Vec::new();
        for key in &keys {
            let entry = Entry::new(key);
            match self.reserve_key(&entry, &holder, epoch, &reservation) {
                Ok(()) => touched.push(entry),
                Err(refusal) => {
                    self.roll_back(&touched, &holder, epoch, &reservation);
                    return Err(refusal);
                }
            }
        }
        Ok(LeaseSet {
            cycle_id: cycle_id.to_owned(),
            owner: owner.to_owned(),
            epoch,
            reservation,
            keys,
        })
    }

    /// Releases the set. A record of this holder whose epoch is newer than
    /// `epoch` refuses ([`ReserveRefusal::StaleEpoch`]) before anything is
    /// removed. Unknown keys, and keys now held by a different holder, are
    /// ignored, so a repeated release is a no-op and can never free someone
    /// else's reservation. The set's own `epoch` and `reservation` fields are
    /// informational; `epoch` is the fencing epoch the caller presents now.
    ///
    /// Release works on keys, not on membership: it frees every record of
    /// this holder that the set names, also one that a later reserve call of
    /// the same holder shares (the holder is the unit of ownership; the
    /// reservation ids only decide when a failed call's rollback may delete).
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
            self.remove_held(entry, &holder, epoch, None)?;
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
    // refresh it if (and only if) the same holder already owns it. Either way
    // the record lists `reservation` among its members afterwards.
    fn reserve_key(
        &self,
        entry: &Entry,
        holder: &Holder<'_>,
        epoch: u64,
        reservation: &str,
    ) -> Result<(), ReserveRefusal> {
        for _ in 0..RESERVE_ATTEMPTS {
            let record = FileLeaseRecord {
                schema_version: FILE_LEASE_SCHEMA,
                key_id: entry.id.clone(),
                key: entry.key.clone(),
                owner: holder.owner.to_owned(),
                cycle_id: holder.cycle_id.to_owned(),
                epoch,
                acquired_at: jiff::Timestamp::now(),
                reservations: BTreeSet::from([reservation.to_owned()]),
            };
            match self.records.create(&record) {
                Ok(()) => return Ok(()),
                Err(StoreError::AlreadyExists { .. }) => {}
                Err(error) => return Err(error.into()),
            }
            let refreshed = self.records.update_locked(&entry.id, |existing| {
                refresh(existing, entry, holder, epoch, reservation)
            });
            match refreshed {
                Ok(()) => return Ok(()),
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

    // Undoes this call on every record it touched (created or refreshed):
    // its id leaves the members, and a record nobody else claims is deleted
    // (`leave_key`). A record still claimed by another call of the same
    // holder, successful or in flight, is kept (`Removal::Claimed`), so the
    // path never goes to someone else while a `LeaseSet` covers it, and when
    // every call that touched it has failed the path is free again. The
    // deletion is fenced with the epoch this call reserved under (never
    // `u64::MAX`): the last claim on a record that a newer epoch advanced is
    // not this stale call's to remove, reported as `StaleEpoch` and kept.
    // Any other failure is logged, not returned: the original refusal is the
    // more useful answer, and the leftovers belong to the same holder
    // (idempotent re-reserve / release).
    fn roll_back(&self, touched: &[Entry], holder: &Holder<'_>, epoch: u64, reservation: &str) {
        for entry in touched.iter().rev() {
            match self.leave_key(entry, holder, epoch, reservation) {
                Ok(Removal::Done) => {}
                Ok(Removal::Claimed) => {
                    tracing::debug!(
                        key = %entry.key,
                        "file lease rollback kept a record other reservations still claim"
                    );
                }
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

    // Takes `reservation` out of the record's members, or deletes the record
    // if the call is its only member. The set is never written empty, so a
    // record without members always means "written before membership
    // existed". Two steps, each under the record lock: `leave` removes the
    // call unless it is the last member; for the last member the record is
    // deleted under its own lock after re-checking that the set still is
    // exactly this call. If another call joined in between, the call is no
    // longer the last member and the next round removes it from the set.
    fn leave_key(
        &self,
        entry: &Entry,
        holder: &Holder<'_>,
        epoch: u64,
        reservation: &str,
    ) -> Result<Removal, ReserveRefusal> {
        for _ in 0..RESERVE_ATTEMPTS {
            let left = self.records.update_locked(&entry.id, |existing| {
                leave(existing, entry, holder, epoch, reservation)
            });
            match left {
                Ok(Membership::Foreign) => return Ok(Removal::Done),
                Ok(Membership::Claimed) => return Ok(Removal::Claimed),
                Ok(Membership::Sole) => {
                    let removal = self.remove_held(entry, holder, epoch, Some(reservation))?;
                    if matches!(removal, Removal::Done) {
                        return Ok(Removal::Done);
                    }
                }
                Err(Failure::Refused(refusal)) => return Err(refusal),
                Err(Failure::Store(StoreError::NotFound { .. })) => return Ok(Removal::Done),
                Err(Failure::Store(error)) => return Err(error.into()),
            }
        }
        Err(ReserveRefusal::Store(format!(
            "lease record for '{}' kept changing during rollback",
            entry.key
        )))
    }

    // `only_member`: `Some(id)` deletes the record only while `id` is its one
    // and only member (rollback); `None` removes by holder and epoch alone
    // (release).
    fn remove_held(
        &self,
        entry: &Entry,
        holder: &Holder<'_>,
        epoch: u64,
        only_member: Option<&str>,
    ) -> Result<Removal, ReserveRefusal> {
        let lock = self.records.lock(&entry.id)?;
        let removed = self.remove_under_lock(entry, holder, epoch, only_member);
        let unlocked = lock.release();
        match (removed, unlocked) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error.into()),
            (Ok(removal), Ok(())) => Ok(removal),
        }
    }

    fn remove_under_lock(
        &self,
        entry: &Entry,
        holder: &Holder<'_>,
        epoch: u64,
        only_member: Option<&str>,
    ) -> Result<Removal, ReserveRefusal> {
        let record = match self.records.load(&entry.id) {
            Ok(record) => record,
            Err(StoreError::NotFound { .. }) => return Ok(Removal::Done),
            Err(error) => return Err(error.into()),
        };
        check_consistent(&record, entry)?;
        if !holder.matches(&record) {
            return Ok(Removal::Done);
        }
        // Anybody else among the members (or nobody known, as for a record
        // written before membership existed) keeps the record.
        if only_member
            .is_some_and(|id| record.reservations.len() != 1 || !record.reservations.contains(id))
        {
            return Ok(Removal::Claimed);
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
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Removal::Done),
            Err(error) => return Err(io_refusal(error)),
        }
        // Without the directory sync a crash could resurrect the lease.
        records
            .open(".")
            .and_then(|directory| directory.sync_all())
            .map_err(io_refusal)?;
        Ok(Removal::Done)
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

// What `remove_under_lock` / `leave_key` did with the record they looked at.
enum Removal {
    // Removed, already gone, or held by a different holder (nothing to do).
    Done,
    // Kept: other reserve calls of the holder still claim the record.
    Claimed,
}

// Outcome of `leave`, decided inside the record lock.
enum Membership {
    // The record belongs to a different holder; untouched.
    Foreign,
    // Other claims remain (or the record cannot name its members).
    Claimed,
    // The call is the record's only member (nothing was changed): the record
    // may be deleted.
    Sole,
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
    reservation: &str,
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
    join(&mut existing.reservations, reservation);
    Ok(())
}

// Adds a refreshing call to the members, keeping the ones already there. An
// empty set means the record predates membership: its holders are unknown, so
// the `UNTRACKED_MEMBER` is added first and keeps it alive through any
// rollback. The same member stands in for the call once the list is full, so
// the record stays bounded and is never deleted on behalf of a call it could
// not name.
fn join(reservations: &mut BTreeSet<String>, reservation: &str) {
    if reservations.is_empty() {
        reservations.insert(UNTRACKED_MEMBER.to_owned());
    }
    if reservations.len() < MAX_RESERVATION_MEMBERS {
        reservations.insert(reservation.to_owned());
    } else {
        reservations.insert(UNTRACKED_MEMBER.to_owned());
    }
}

// Runs under the record lock: takes the rolling-back call out of the members
// unless it is the only one (then the record is the caller's to delete).
fn leave(
    existing: &mut FileLeaseRecord,
    entry: &Entry,
    holder: &Holder<'_>,
    epoch: u64,
    reservation: &str,
) -> Result<Membership, Failure> {
    check_consistent(existing, entry).map_err(Failure::Refused)?;
    if !holder.matches(existing) {
        return Ok(Membership::Foreign);
    }
    // Not a member: the record predates membership or the list overflowed.
    // Its claimants are unknown, so it stays.
    if !existing.reservations.contains(reservation) {
        return Ok(Membership::Claimed);
    }
    // The last claim of a record that a newer epoch advanced is not this
    // stale call's to give up; nothing is written.
    if existing.reservations.len() == 1 && existing.epoch > epoch {
        return Err(Failure::Refused(ReserveRefusal::StaleEpoch));
    }
    if existing.reservations.len() == 1 {
        return Ok(Membership::Sole);
    }
    existing.reservations.remove(reservation);
    Ok(Membership::Claimed)
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

    use super::{
        Entry, FileLeaseRecord, FileLeaseRegistry, Holder, LeaseSet, MAX_RESERVATION_MEMBERS,
        ReserveRefusal, UNTRACKED_MEMBER, lease_id_for,
    };
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

    fn members_of(fx: &Fixture, path: &str) -> TestResult<BTreeSet<String>> {
        let record = fx
            .registry
            .owner_of(path)
            .map_err(ctx("owner_of"))?
            .ok_or_else(missing("the path must be reserved"))?;
        Ok(record.reservations)
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
        // `a.rs` (it is in the call's `touched` list) and has not reached
        // `z.rs` yet.
        let older = reserve(&fx, &["a.rs"], "worker-x", "cycle-x", 1)?;
        let touched = [Entry::new("a.rs")];
        // Meanwhile epoch 2 of the same holder refreshes `a.rs` and receives
        // a successful `LeaseSet`.
        let newer = reserve(&fx, &["a.rs"], "worker-x", "cycle-x", 2)?;
        assert_eq!(newer.epoch, 2);
        assert_ne!(older.reservation, newer.reservation);
        assert_eq!(
            members_of(&fx, "a.rs")?,
            set(&[older.reservation.as_str(), newer.reservation.as_str()])
        );

        // Sequential public calls cannot reproduce this interleaving: once
        // epoch 2 holds `a.rs`, an epoch-1 `reserve` is fenced out at `a.rs`
        // and never creates anything. The rollback step is therefore driven
        // directly with the epoch-1 call's own epoch and reservation id: the
        // epoch-2 record, still claimed by the epoch-2 call, must survive it.
        let fenced = fx
            .registry
            .reserve(&set(&["a.rs", "z.rs"]), "worker-x", "cycle-x", 1);
        assert_eq!(fenced, Err(ReserveRefusal::StaleEpoch));
        fx.registry
            .roll_back(&touched, &holder, 1, &older.reservation);
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
        assert_eq!(record.reservations, set(&[newer.reservation.as_str()]));
        // The epoch fence holds on its own as well: the epoch-2 call's id is
        // the last claim, and an epoch-1 presenter may not give it up.
        fx.registry
            .roll_back(&touched, &holder, 1, &newer.reservation);
        assert_eq!(members_of(&fx, "a.rs")?, set(&[newer.reservation.as_str()]));
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

        // A rollback at the record's own epoch by its last member still
        // removes it, and a rollback for a record that is gone is a no-op.
        fx.registry
            .roll_back(&touched, &holder, 2, &newer.reservation);
        assert_eq!(holder_of(&fx, "a.rs")?, None);
        fx.registry
            .roll_back(&touched, &holder, 2, &newer.reservation);
        assert_eq!(holder_of(&fx, "a.rs")?, None);
        Ok(())
    }

    #[test]
    fn rollback_keeps_a_record_adopted_by_a_same_epoch_reservation() -> TestResult {
        let fx = fixture()?;
        let holder = Holder {
            owner: "worker-x",
            cycle_id: "cycle-x",
        };
        // `z.rs` belongs to another holder, so call A below is doomed to be
        // refused there after it has decided `a.rs` (sorted before `z.rs`).
        reserve(&fx, &["z.rs"], "worker-y", "cycle-y", 1)?;

        // Call A: `reserve({a.rs, z.rs}, X, cycle, 1)` has created `a.rs`
        // (it is in A's `touched` list) and has not reached `z.rs` yet. The
        // record it wrote is the one a one-path reserve writes, so that call
        // stands in for A's creation and yields A's reservation id.
        let call_a = reserve(&fx, &["a.rs"], "worker-x", "cycle-x", 1)?;
        let touched = [Entry::new("a.rs")];

        // Call B: `reserve({a.rs}, X, cycle, 1)` at the SAME epoch finds the
        // record already held by X, joins its members and returns a
        // successful set.
        let call_b = reserve(&fx, &["a.rs"], "worker-x", "cycle-x", 1)?;
        assert_eq!(call_b.epoch, call_a.epoch);
        assert_ne!(call_a.reservation, call_b.reservation);
        assert!(!call_b.reservation.is_empty());
        assert_eq!(
            members_of(&fx, "a.rs")?,
            set(&[call_a.reservation.as_str(), call_b.reservation.as_str()])
        );

        // A now runs into the foreign `z.rs` and is refused (the refusal
        // itself touches nothing of X); its rollback, driven directly because
        // the public calls cannot be interleaved, must not take `a.rs` away
        // from B's successful set.
        let refused = fx
            .registry
            .reserve(&set(&["z.rs"]), "worker-x", "cycle-x", 1);
        assert_eq!(
            refused,
            Err(ReserveRefusal::AlreadyOwned {
                path: "z.rs".to_owned(),
                owner: "worker-y".to_owned(),
            })
        );
        fx.registry
            .roll_back(&touched, &holder, 1, &call_a.reservation);
        let record = fx
            .registry
            .owner_of("a.rs")
            .map_err(ctx("owner_of"))?
            .ok_or_else(missing(
                "a.rs must still be reserved after A's rollback, B holds it",
            ))?;
        assert_eq!(record.owner, "worker-x");
        assert_eq!(record.cycle_id, "cycle-x");
        assert_eq!(record.epoch, 1);
        assert_eq!(record.reservations, set(&[call_b.reservation.as_str()]));
        // Another holder, even at a higher epoch, cannot take the path while
        // B's set is alive.
        let takeover = fx
            .registry
            .reserve(&set(&["a.rs"]), "worker-y", "cycle-y", 2);
        assert_eq!(
            takeover,
            Err(ReserveRefusal::AlreadyOwned {
                path: "a.rs".to_owned(),
                owner: "worker-x".to_owned(),
            })
        );
        // The foreign record is untouched as well.
        assert_eq!(holder_of(&fx, "z.rs")?.as_deref(), Some("worker-y"));

        // Control: once B's id is the last member and rolls back too, the
        // record is removed and the path is free for others again.
        fx.registry
            .roll_back(&touched, &holder, 1, &call_b.reservation);
        assert_eq!(holder_of(&fx, "a.rs")?, None);
        reserve(&fx, &["a.rs"], "worker-y", "cycle-y", 2)?;
        assert_eq!(holder_of(&fx, "a.rs")?.as_deref(), Some("worker-y"));
        Ok(())
    }

    #[test]
    fn both_failing_calls_leave_the_shared_record_unowned() -> TestResult {
        let fx = fixture()?;
        let holder = Holder {
            owner: "worker-x",
            cycle_id: "cycle-x",
        };
        // `y.rs` and `z.rs` belong to another holder. `a.rs` sorts before
        // both, so each of the two calls below decides `a.rs` first.
        reserve(&fx, &["y.rs", "z.rs"], "worker-y", "cycle-y", 1)?;

        // Call A: `reserve({a.rs, z.rs}, X, cycle, 1)` has created `a.rs`
        // (it is in A's `touched` list) and has not reached `z.rs` yet.
        let call_a = reserve(&fx, &["a.rs"], "worker-x", "cycle-x", 1)?;
        let touched = [Entry::new("a.rs")];

        // Call B: `reserve({a.rs, y.rs}, X, cycle, 1)` refreshes `a.rs`
        // (joining A's record without creating it) and is then refused at the
        // foreign `y.rs`. Its own rollback runs inside this public call.
        let call_b = fx
            .registry
            .reserve(&set(&["a.rs", "y.rs"]), "worker-x", "cycle-x", 1);
        assert_eq!(
            call_b,
            Err(ReserveRefusal::AlreadyOwned {
                path: "y.rs".to_owned(),
                owner: "worker-y".to_owned(),
            })
        );
        // B has left again, but A is still in flight and claims `a.rs`.
        assert_eq!(
            members_of(&fx, "a.rs")?,
            set(&[call_a.reservation.as_str()])
        );
        assert_eq!(holder_of(&fx, "a.rs")?.as_deref(), Some("worker-x"));

        // A is refused at `z.rs` as well (its rollback is driven directly,
        // the public calls cannot be interleaved). Neither call returned a
        // `LeaseSet`, so nobody may keep `a.rs`.
        fx.registry
            .roll_back(&touched, &holder, 1, &call_a.reservation);
        assert_eq!(holder_of(&fx, "a.rs")?, None);
        // Another holder can reserve it right away.
        reserve(&fx, &["a.rs"], "worker-y", "cycle-y", 1)?;
        assert_eq!(holder_of(&fx, "a.rs")?.as_deref(), Some("worker-y"));
        // The foreign records were never touched.
        assert_eq!(holder_of(&fx, "y.rs")?.as_deref(), Some("worker-y"));
        assert_eq!(holder_of(&fx, "z.rs")?.as_deref(), Some("worker-y"));
        Ok(())
    }

    #[test]
    fn rollback_never_removes_a_record_without_a_reservation_id() -> TestResult {
        let fx = fixture()?;
        let holder = Holder {
            owner: "worker-x",
            cycle_id: "cycle-x",
        };
        // A record written before `reservations` existed has no such key.
        let json = format!(
            r#"{{"schema_version":1,"key_id":"{}","key":"a.rs","owner":"worker-x","cycle_id":"cycle-x","epoch":1,"acquired_at":"2026-01-01T00:00:00Z"}}"#,
            lease_id_for("a.rs")
        );
        let legacy: FileLeaseRecord = serde_json::from_str(&json).map_err(ctx("legacy record"))?;
        assert!(legacy.reservations.is_empty());
        fx.registry
            .records
            .create(&legacy)
            .map_err(ctx("store legacy record"))?;
        let touched = [Entry::new("a.rs")];
        // No live call id (nor an empty one) is allowed to roll it back.
        fx.registry
            .roll_back(&touched, &holder, 1, "some-live-call");
        fx.registry.roll_back(&touched, &holder, 1, "");
        assert_eq!(holder_of(&fx, "a.rs")?.as_deref(), Some("worker-x"));
        // The legacy record is still the holder's: a new reserve adopts it,
        // and the unknown earlier reservation stays represented.
        let adopting = reserve(&fx, &["a.rs"], "worker-x", "cycle-x", 1)?;
        assert_eq!(
            members_of(&fx, "a.rs")?,
            set(&[UNTRACKED_MEMBER, adopting.reservation.as_str()])
        );
        // If the adopting call fails, its rollback gives up only its own
        // claim: the record the legacy reservation held is kept.
        fx.registry
            .roll_back(&touched, &holder, 1, &adopting.reservation);
        assert_eq!(members_of(&fx, "a.rs")?, set(&[UNTRACKED_MEMBER]));
        assert_eq!(holder_of(&fx, "a.rs")?.as_deref(), Some("worker-x"));
        // Only release (by key) frees such a record.
        fx.registry
            .release(&adopting, 1)
            .map_err(ctx("release legacy record"))?;
        assert_eq!(holder_of(&fx, "a.rs")?, None);
        Ok(())
    }

    #[test]
    fn member_list_is_bounded_and_overflow_is_never_rolled_back() -> TestResult {
        let fx = fixture()?;
        let holder = Holder {
            owner: "worker-x",
            cycle_id: "cycle-x",
        };
        // One call more than the list holds.
        let mut calls = Vec::new();
        for _ in 0..=MAX_RESERVATION_MEMBERS {
            calls.push(reserve(&fx, &["a.rs"], "worker-x", "cycle-x", 1)?);
        }
        let last = calls
            .last()
            .ok_or_else(missing("the loop above reserved at least once"))?;
        let members = members_of(&fx, "a.rs")?;
        assert_eq!(members.len(), MAX_RESERVATION_MEMBERS + 1);
        assert!(members.contains(UNTRACKED_MEMBER));
        assert!(!members.contains(&last.reservation));
        // The overflowing call is not a member, so its rollback cannot
        // delete the record (nor shrink the list).
        let touched = [Entry::new("a.rs")];
        fx.registry
            .roll_back(&touched, &holder, 1, &last.reservation);
        assert_eq!(members_of(&fx, "a.rs")?, members);
        assert_eq!(holder_of(&fx, "a.rs")?.as_deref(), Some("worker-x"));
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
        // Same holder, epoch and keys; every call carries its own
        // reservation id.
        assert_eq!(first.keys, again.keys);
        assert_eq!(first.epoch, again.epoch);
        assert_eq!(first.owner, again.owner);
        assert_eq!(first.cycle_id, again.cycle_id);
        assert_ne!(first.reservation, again.reservation);
        // The record is shared by both calls: the set grows by the new id.
        for path in ["a.rs", "b.rs"] {
            assert_eq!(
                members_of(&fx, path)?,
                set(&[first.reservation.as_str(), again.reservation.as_str()])
            );
        }
        let newer = reserve(&fx, &["a.rs", "b.rs"], "worker-a", "cycle-a", 2)?;
        assert_eq!(newer.epoch, 2);
        assert_eq!(
            members_of(&fx, "a.rs")?,
            set(&[
                first.reservation.as_str(),
                again.reservation.as_str(),
                newer.reservation.as_str(),
            ])
        );
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
