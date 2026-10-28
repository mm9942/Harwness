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
//! as [`LeaseSet::reservation`]). A record keeps a **claim** for every call
//! that currently holds it ([`FileLeaseRecord::reservations`]: call id to the
//! epoch the call reserved under). Creating a record stores the call's claim;
//! a same-holder refresh adds the call's claim and keeps the others. A claim
//! ends only when its call fails and rolls back; a successful call keeps its
//! claim until the record is released. Two interleaved calls of the same
//! holder (at any epochs) therefore share a record that lives as long as one
//! of them succeeds and disappears when all of them have failed.
//!
//! The record's epoch is the highest epoch among its claims. A refresh raises
//! it; when a failed call's claim ends it falls back to the highest remaining
//! one, so a sole surviving claim with an older epoch can roll itself back.
//!
//! A record names at most 64 claims. Further calls are only counted, per
//! epoch ([`FileLeaseRecord::overflow`]: epoch to number of calls); their
//! rollback decrements the count of their own epoch, so they end like named
//! claims, and the record is deleted when no named claim and no counted one is
//! left. The record's epoch is the highest over named claims *and* counted
//! epochs, recomputed after every rollback. A counted call can only end its
//! claim on the incarnation of the record it touched
//! ([`FileLeaseRecord::generation`], a fresh id for every created record): a
//! delayed rollback of an old call never decrements the count of a record that
//! was released and created anew.
//!
//! One claim cannot be named: a record written before membership existed has
//! holders that no caller can ever name. The next same-holder reserve adds the
//! single claim `"untracked"` (at the record's epoch), which no rollback ends;
//! only [`FileLeaseRegistry::release`] frees such a record. Records of the
//! earlier layouts (a single `reservation` id; a list of `reservations`; a
//! numeric `overflow` count shared by all counted calls) are read as claims or
//! counts at the record's epoch and rewritten in the current layout by
//! the next mutation.
//!
//! # Holders and fencing
//! A holder is the pair `(owner, cycle_id)`. The same holder re-reserving a
//! path is idempotent and may only move the recorded epoch forward (it falls
//! back only when a failed call's claim ends, see above); a holder
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
//!    **or** refreshed, before the refusal is returned (rollback): its claim
//!    ends, or, if it is the only claim, the record is deleted. A record that
//!    another call of the same holder still claims (a successful one, or one
//!    still in flight) is kept; the last claim to fail deletes it. So
//!    all-or-nothing also holds for interleaved calls of the same holder: if
//!    every call that touched a path fails, the path is free again. The
//!    deletion is fenced with the epoch of the call being undone, as
//!    `release` is: a record whose epoch is higher than that (a stale
//!    presenter) is kept, and the holder of that epoch frees it with
//!    `release`.
//!
//! What is *not* atomic: other readers can briefly observe a half-reserved
//! set while a call is in progress, and a crash (or an I/O failure during
//! rollback, which is logged) can leave part of a set behind, or a claim in a
//! record that no live call owns any more. Those records belong to the same
//! holder, so repeating the reserve is idempotent and
//! [`FileLeaseRegistry::release`] with the same set cleans them up. At no
//! point do two holders own the same path.
//!
//! [`FileLeaseRegistry::release`] works on keys, not on claims: it frees
//! every record of the presenting holder that the set names, whoever its
//! claimants are. Claims could not replace that: a crashed call leaves a claim
//! behind that no later set can name, and an `"untracked"` claim would never
//! be freeable.
//!
//! # Store mechanics
//! Creation, refresh and lookup reuse `RecordStore` (per-record try-lock,
//! temp + rename + directory `fsync`, corrupt records quarantined). Locks
//! never block: a record locked by a concurrent writer surfaces as
//! [`ReserveRefusal::Store`] and the caller may retry. `RecordStore` has no
//! delete, so removal takes the public record lock itself, re-checks the
//! holder under it and unlinks `records/<id>.json` (the documented, stable
//! layout) followed by a directory `fsync`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Component, Path};

use harw_job_store::{RECORDS_DIR, RecordStore, StoreError, StoreRecord};
use harw_types::{ContentDigest, SessionId};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

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
/// Most claims one record names. Further calls are only counted in
/// [`FileLeaseRecord::overflow`].
const MAX_RESERVATION_MEMBERS: usize = 64;
/// Claim standing for the unknown holders of a record written before
/// membership existed. Never a UUID, so no live call id equals it, and no
/// rollback ends it.
const UNTRACKED_MEMBER: &str = "untracked";

/// One reserved path.
///
/// Deserialization accepts the current layout and the three earlier ones (see
/// the module docs); anything else, including unknown fields, is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    /// Highest lease epoch among the claims on this record. A refresh moves
    /// it forward; when a failed call's claim ends it falls back to the
    /// highest epoch of the remaining claims (named and counted).
    pub epoch: u64,
    pub acquired_at: jiff::Timestamp,
    /// Claims of the reserve calls that currently hold this record: the id of
    /// the call ([`LeaseSet::reservation`]) to the epoch it reserved under. A
    /// create stores the call's claim, a same-holder refresh adds it and keeps
    /// the others. A failed call's rollback ends its own claim, and the record
    /// is deleted when the last claim ends; a successful call keeps its claim
    /// until release. A record written before membership existed has none, and
    /// the next same-holder reserve adds the claim `"untracked"` (at the
    /// record's epoch), which no rollback ends.
    pub reservations: BTreeMap<String, u64>,
    /// Calls beyond the cap of 64 named claims, counted instead of named:
    /// epoch the call reserved under to the number of such calls. A call's
    /// rollback decrements the count of its own epoch (a zero count is
    /// dropped); the record is deleted when this and `reservations` are both
    /// empty. Only the incarnation of the record the call touched
    /// ([`FileLeaseRecord::generation`]) counts its rollback.
    pub overflow: BTreeMap<u64, u32>,
    /// Id of this incarnation of the record, drawn when it is created and kept
    /// by every refresh. A counted call remembers the generation it touched,
    /// so its delayed rollback cannot decrement the count of a record that was
    /// released and created anew. A record written before generations
    /// existed has none (empty); the next same-holder reserve gives it one,
    /// and no call that touched it earlier can match it.
    pub generation: String,
}

impl StoreRecord for FileLeaseRecord {
    fn record_id(&self) -> &str {
        &self.key_id
    }
}

impl<'de> Deserialize<'de> for FileLeaseRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        FileLeaseRecordWire::deserialize(deserializer).map(Self::from)
    }
}

// Everything `FileLeaseRecord` reads from disk. Besides the current fields it
// takes the layouts that were written under the same schema version before:
// a single `reservation` id (one claim), a plain list of `reservations` ids
// (claims) and a single `overflow` count; all of them become claims or counts
// at the record's epoch. Records from before `generation` existed keep it
// empty. Unknown fields stay refused.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLeaseRecordWire {
    schema_version: u32,
    key_id: String,
    key: String,
    owner: String,
    cycle_id: String,
    epoch: u64,
    acquired_at: jiff::Timestamp,
    #[serde(default)]
    reservations: WireClaims,
    #[serde(default)]
    overflow: WireOverflow,
    #[serde(default)]
    reservation: String,
    #[serde(default)]
    generation: String,
}

// `reservations` as a map (current layout) or as a list of ids (earlier one).
#[derive(Deserialize)]
#[serde(untagged)]
enum WireClaims {
    Epochs(BTreeMap<String, u64>),
    Ids(BTreeSet<String>),
}

impl Default for WireClaims {
    fn default() -> Self {
        Self::Epochs(BTreeMap::new())
    }
}

// `overflow` as a map of epoch to count (current layout) or as a single count
// (earlier layout, where every counted call shared one number). Not an
// untagged enum: serde buffers untagged input and then cannot read the
// integer map keys back from JSON strings.
enum WireOverflow {
    Count(u32),
    Epochs(BTreeMap<u64, u32>),
}

impl Default for WireOverflow {
    fn default() -> Self {
        Self::Count(0)
    }
}

impl<'de> Deserialize<'de> for WireOverflow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OverflowVisitor;

        impl<'de> Visitor<'de> for OverflowVisitor {
            type Value = WireOverflow;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a count or a map of epoch to count")
            }

            fn visit_u64<E>(self, count: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                u32::try_from(count)
                    .map(WireOverflow::Count)
                    .map_err(E::custom)
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut epochs = BTreeMap::new();
                while let Some((epoch, count)) = map.next_entry::<u64, u32>()? {
                    epochs.insert(epoch, count);
                }
                Ok(WireOverflow::Epochs(epochs))
            }
        }

        deserializer.deserialize_any(OverflowVisitor)
    }
}

impl From<FileLeaseRecordWire> for FileLeaseRecord {
    fn from(wire: FileLeaseRecordWire) -> Self {
        let epoch = wire.epoch;
        let mut reservations = match wire.reservations {
            WireClaims::Epochs(epochs) => epochs,
            WireClaims::Ids(ids) => ids.into_iter().map(|id| (id, epoch)).collect(),
        };
        // An empty id is the earlier layout's "none" (a record that predates
        // reservation ids altogether).
        if !wire.reservation.is_empty() {
            reservations.entry(wire.reservation).or_insert(epoch);
        }
        // The earlier single count belongs to the record's epoch; zero counts
        // are no claims.
        let mut overflow = match wire.overflow {
            WireOverflow::Count(count) => BTreeMap::from([(epoch, count)]),
            WireOverflow::Epochs(epochs) => epochs,
        };
        overflow.retain(|_, count| *count > 0);
        Self {
            schema_version: wire.schema_version,
            key_id: wire.key_id,
            key: wire.key,
            owner: wire.owner,
            cycle_id: wire.cycle_id,
            epoch,
            acquired_at: wire.acquired_at,
            reservations,
            overflow,
            generation: wire.generation,
        }
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
    /// this set; the key of the call's claim in
    /// [`FileLeaseRecord::reservations`] of every record it created or
    /// refreshed (beyond the cap of named claims the call is only counted in
    /// [`FileLeaseRecord::overflow`]). Informational for
    /// [`FileLeaseRegistry::release`], which works on keys.
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
    /// its claim ends, and a record without another claim is deleted (fenced
    /// with this call's own `epoch`). A record that another call of the same
    /// holder still claims is kept. See the module docs for what is and is
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
        // `harw_types` has no dedicated reservation id; a v4 UUID is opaque
        // and unique across calls and processes, which is all that is needed.
        let reservation = SessionId::new().0;
        // Every key this call decided, created or refreshed, and how the
        // record counted the call: the rollback leaves all of them, not only
        // the ones it created.
        let mut touched: Vec<Touched> = Vec::new();
        for key in &keys {
            let entry = Entry::new(key);
            match self.reserve_key(&entry, &holder, epoch, &reservation) {
                Ok(placement) => touched.push(Touched { entry, placement }),
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
    /// Release works on keys, not on claims: it frees every record of this
    /// holder that the set names, also one that a later reserve call of the
    /// same holder shares (the holder is the unit of ownership; the claims
    /// only decide when a failed call's rollback may delete).
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
    // the record holds a claim of this call afterwards; the result says how it
    // was recorded (named, or only counted beyond the cap) and in which
    // incarnation of the record.
    fn reserve_key(
        &self,
        entry: &Entry,
        holder: &Holder<'_>,
        epoch: u64,
        reservation: &str,
    ) -> Result<Placement, ReserveRefusal> {
        for _ in 0..RESERVE_ATTEMPTS {
            let generation = SessionId::new().0;
            let record = FileLeaseRecord {
                schema_version: FILE_LEASE_SCHEMA,
                key_id: entry.id.clone(),
                key: entry.key.clone(),
                owner: holder.owner.to_owned(),
                cycle_id: holder.cycle_id.to_owned(),
                epoch,
                acquired_at: jiff::Timestamp::now(),
                reservations: BTreeMap::from([(reservation.to_owned(), epoch)]),
                overflow: BTreeMap::new(),
                generation: generation.clone(),
            };
            match self.records.create(&record) {
                Ok(()) => {
                    return Ok(Placement {
                        counted: Counted::Named,
                        generation,
                    });
                }
                Err(StoreError::AlreadyExists { .. }) => {}
                Err(error) => return Err(error.into()),
            }
            let refreshed = self.records.update_locked(&entry.id, |existing| {
                refresh(existing, entry, holder, epoch, reservation)
            });
            match refreshed {
                Ok(placement) => return Ok(placement),
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
    // its claim ends, and a record nobody else claims is deleted
    // (`leave_key`). A record still claimed by another call of the same
    // holder, successful or in flight, is kept (`Removal::Claimed`), so the
    // path never goes to someone else while a `LeaseSet` covers it, and when
    // every call that touched it has failed the path is free again. The
    // deletion is fenced with the epoch this call reserved under (never
    // `u64::MAX`): a record whose epoch is higher than that is not this
    // stale call's to remove, reported as `StaleEpoch` and kept. Any other
    // failure is logged, not returned: the original refusal is the more
    // useful answer, and the leftovers belong to the same holder (idempotent
    // re-reserve / release).
    fn roll_back(&self, touched: &[Touched], holder: &Holder<'_>, epoch: u64, reservation: &str) {
        for touch in touched.iter().rev() {
            let claim = Claim {
                reservation,
                epoch,
                counted: touch.placement.counted,
                generation: &touch.placement.generation,
            };
            match self.leave_key(&touch.entry, holder, claim) {
                Ok(Removal::Done) => {}
                Ok(Removal::Claimed) => {
                    tracing::debug!(
                        key = %touch.entry.key,
                        "file lease rollback kept a record other reservations still claim"
                    );
                }
                Err(ReserveRefusal::StaleEpoch) => {
                    tracing::debug!(
                        key = %touch.entry.key,
                        "file lease rollback kept a record advanced by a newer epoch"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        key = %touch.entry.key,
                        error = %error,
                        "file lease rollback failed; the holder keeps the lease until release"
                    );
                }
            }
        }
    }

    // Ends `claim` on the record, or deletes the record if the claim is the
    // only one. The claims are never written empty, so a record without any
    // always means "written before membership existed". Two steps, each
    // under the record lock: `leave` ends the claim unless it is the last
    // one; for the last claim the record is deleted under its own lock after
    // re-checking that it still is the only one. If another call joined in
    // between, the claim is no longer the last and the next round ends it.
    fn leave_key(
        &self,
        entry: &Entry,
        holder: &Holder<'_>,
        claim: Claim<'_>,
    ) -> Result<Removal, ReserveRefusal> {
        for _ in 0..RESERVE_ATTEMPTS {
            let left = self
                .records
                .update_locked(&entry.id, |existing| leave(existing, entry, holder, claim));
            match left {
                Ok(Membership::Foreign) => return Ok(Removal::Done),
                Ok(Membership::Claimed) => return Ok(Removal::Claimed),
                Ok(Membership::Sole) => {
                    let removal = self.remove_held(entry, holder, claim.epoch, Some(claim))?;
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

    // `only_claim`: `Some(claim)` deletes the record only while `claim` is
    // its one and only claim (rollback); `None` removes by holder and epoch
    // alone (release).
    fn remove_held(
        &self,
        entry: &Entry,
        holder: &Holder<'_>,
        epoch: u64,
        only_claim: Option<Claim<'_>>,
    ) -> Result<Removal, ReserveRefusal> {
        let lock = self.records.lock(&entry.id)?;
        let removed = self.remove_under_lock(entry, holder, epoch, only_claim);
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
        only_claim: Option<Claim<'_>>,
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
        // Any other claim (or nobody known, as for a record written before
        // membership existed) keeps the record.
        if only_claim.is_some_and(|claim| !claim.is_sole(&record)) {
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

// A key a reserve call decided, and how that record counted the call.
struct Touched {
    entry: Entry,
    placement: Placement,
}

// How one record counted one reserve call, and which incarnation of the
// record it did so in.
struct Placement {
    counted: Counted,
    generation: String,
}

// How a record counts one reserve call.
#[derive(Clone, Copy)]
enum Counted {
    // As a named claim in `reservations`.
    Named,
    // Only in `overflow` (the record already names the maximum).
    Overflow,
}

// One call's claim on a record, as its rollback presents it: the call's id
// and epoch, how the record counted it, and the generation it touched.
#[derive(Clone, Copy)]
struct Claim<'a> {
    reservation: &'a str,
    epoch: u64,
    counted: Counted,
    generation: &'a str,
}

impl Claim<'_> {
    // Does the record still count this call? A named claim is identified by
    // the call's unique id. A counted claim has no identity of its own, so it
    // only counts in the incarnation of the record it touched; an empty
    // generation (a record from before generations existed) never matches.
    fn is_held_by(&self, record: &FileLeaseRecord) -> bool {
        match self.counted {
            Counted::Named => record.reservations.contains_key(self.reservation),
            Counted::Overflow => {
                !self.generation.is_empty()
                    && record.generation == self.generation
                    && record.overflow.contains_key(&self.epoch)
            }
        }
    }

    // Is this call the only claim on the record?
    fn is_sole(&self, record: &FileLeaseRecord) -> bool {
        self.is_held_by(record)
            && match self.counted {
                Counted::Named => record.overflow.is_empty() && record.reservations.len() == 1,
                Counted::Overflow => {
                    record.reservations.is_empty()
                        && record.overflow.len() == 1
                        && record.overflow.get(&self.epoch) == Some(&1)
                }
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
    // Other claims remain (or the record cannot name its claimants).
    Claimed,
    // The call is the record's only claim (nothing was changed): the record
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
) -> Result<Placement, Failure> {
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
    let counted = join(existing, reservation, epoch);
    Ok(Placement {
        counted,
        generation: existing.generation.clone(),
    })
}

// Adds a refreshing call to the claims, keeping the ones already there, and
// moves the record's epoch forward. A record without any claim predates
// membership: its holders are unknown, so the `UNTRACKED_MEMBER` claim (at the
// record's old epoch) is added first and keeps it alive through any rollback.
// A record without a generation predates generations and gets one now. Beyond
// the cap the call is only counted (under its own epoch), which its rollback
// undoes.
fn join(existing: &mut FileLeaseRecord, reservation: &str, epoch: u64) -> Counted {
    if existing.reservations.is_empty() && existing.overflow.is_empty() {
        existing
            .reservations
            .insert(UNTRACKED_MEMBER.to_owned(), existing.epoch);
    }
    if existing.generation.is_empty() {
        existing.generation = SessionId::new().0;
    }
    let counted = if existing.reservations.len() < MAX_RESERVATION_MEMBERS {
        existing.reservations.insert(reservation.to_owned(), epoch);
        Counted::Named
    } else {
        let count = existing.overflow.entry(epoch).or_insert(0);
        *count = count.saturating_add(1);
        Counted::Overflow
    };
    existing.epoch = epoch;
    counted
}

// Runs under the record lock: ends the rolling-back call's claim unless it is
// the only one (then the record is the caller's to delete).
fn leave(
    existing: &mut FileLeaseRecord,
    entry: &Entry,
    holder: &Holder<'_>,
    claim: Claim<'_>,
) -> Result<Membership, Failure> {
    check_consistent(existing, entry).map_err(Failure::Refused)?;
    if !holder.matches(existing) {
        return Ok(Membership::Foreign);
    }
    // No such claim: the record was released and created anew, or predates
    // membership. Its claimants are unknown, so it stays.
    if !claim.is_held_by(existing) {
        return Ok(Membership::Claimed);
    }
    if claim.is_sole(existing) {
        return Ok(Membership::Sole);
    }
    match claim.counted {
        Counted::Named => {
            existing.reservations.remove(claim.reservation);
        }
        Counted::Overflow => {
            if let Some(count) = existing.overflow.get_mut(&claim.epoch) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    existing.overflow.remove(&claim.epoch);
                }
            }
        }
    }
    // The record's epoch is the highest among the claims that remain, named
    // and counted ones alike.
    let named = existing.reservations.values().copied().max();
    let counted = existing.overflow.keys().next_back().copied();
    if let Some(highest) = named.max(counted) {
        existing.epoch = highest;
    }
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
    use std::collections::{BTreeMap, BTreeSet};

    use harw_job_store::RECORDS_DIR;

    use super::{
        Counted, Entry, FILE_LEASE_DIR, FileLeaseRecord, FileLeaseRegistry, Holder, LeaseSet,
        MAX_RESERVATION_MEMBERS, Placement, ReserveRefusal, Touched, UNTRACKED_MEMBER,
        lease_id_for,
    };
    use crate::test_support::{TestResult, ctx, missing};

    struct Fixture {
        temp: tempfile::TempDir,
        registry: FileLeaseRegistry,
    }

    fn fixture() -> TestResult<Fixture> {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let registry = FileLeaseRegistry::open_in_job_root(temp.path()).map_err(ctx("open"))?;
        Ok(Fixture { temp, registry })
    }

    // The key a call touched, as the record in its current incarnation
    // counts the call (`counted`). Take it right after the call, before the
    // record is released.
    fn touch(fx: &Fixture, key: &str, counted: Counted) -> TestResult<Touched> {
        let generation = record_of(fx, key)?.generation;
        Ok(Touched {
            entry: Entry::new(key),
            placement: Placement {
                counted,
                generation,
            },
        })
    }

    fn counts(pairs: &[(u64, u32)]) -> BTreeMap<u64, u32> {
        pairs.iter().copied().collect()
    }

    fn claims(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
        pairs
            .iter()
            .map(|(id, epoch)| ((*id).to_owned(), *epoch))
            .collect()
    }

    // `<job-root>/intent_cycle/leases/records/<id>.json`, the stable layout
    // of the store, to plant and inspect records as an earlier build wrote them.
    fn record_file(fx: &Fixture, path: &str) -> std::path::PathBuf {
        fx.temp
            .path()
            .join(FILE_LEASE_DIR)
            .join(RECORDS_DIR)
            .join(format!("{}.json", lease_id_for(path)))
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

    fn record_of(fx: &Fixture, path: &str) -> TestResult<FileLeaseRecord> {
        fx.registry
            .owner_of(path)
            .map_err(ctx("owner_of"))?
            .ok_or_else(missing("the path must be reserved"))
    }

    fn members_of(fx: &Fixture, path: &str) -> TestResult<BTreeMap<String, u64>> {
        Ok(record_of(fx, path)?.reservations)
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
        let touched = [touch(&fx, "a.rs", Counted::Named)?];
        // Meanwhile epoch 2 of the same holder refreshes `a.rs` and receives
        // a successful `LeaseSet`.
        let newer = reserve(&fx, &["a.rs"], "worker-x", "cycle-x", 2)?;
        assert_eq!(newer.epoch, 2);
        assert_ne!(older.reservation, newer.reservation);
        assert_eq!(
            members_of(&fx, "a.rs")?,
            claims(&[
                (older.reservation.as_str(), 1),
                (newer.reservation.as_str(), 2)
            ])
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
        let record = record_of(&fx, "a.rs")?;
        assert_eq!(record.owner, "worker-x");
        assert_eq!(record.cycle_id, "cycle-x");
        assert_eq!(record.epoch, 2);
        assert_eq!(
            record.reservations,
            claims(&[(newer.reservation.as_str(), 2)])
        );
        // The epoch fence holds on its own as well: the epoch-2 claim is the
        // last one, and a presenter of epoch 1 may not delete its record.
        fx.registry
            .roll_back(&touched, &holder, 1, &newer.reservation);
        assert_eq!(
            members_of(&fx, "a.rs")?,
            claims(&[(newer.reservation.as_str(), 2)])
        );
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

        // A rollback at the record's own epoch by its last claim still
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
        let touched = [touch(&fx, "a.rs", Counted::Named)?];

        // Call B: `reserve({a.rs}, X, cycle, 1)` at the SAME epoch finds the
        // record already held by X, adds its claim and returns a successful
        // set.
        let call_b = reserve(&fx, &["a.rs"], "worker-x", "cycle-x", 1)?;
        assert_eq!(call_b.epoch, call_a.epoch);
        assert_ne!(call_a.reservation, call_b.reservation);
        assert!(!call_b.reservation.is_empty());
        assert_eq!(
            members_of(&fx, "a.rs")?,
            claims(&[
                (call_a.reservation.as_str(), 1),
                (call_b.reservation.as_str(), 1)
            ])
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
        let record = record_of(&fx, "a.rs")?;
        assert_eq!(record.owner, "worker-x");
        assert_eq!(record.cycle_id, "cycle-x");
        assert_eq!(record.epoch, 1);
        assert_eq!(
            record.reservations,
            claims(&[(call_b.reservation.as_str(), 1)])
        );
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

        // Control: once B's claim is the last one and rolls back too, the
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
        let touched = [touch(&fx, "a.rs", Counted::Named)?];

        // Call B: `reserve({a.rs, y.rs}, X, cycle, 1)` refreshes `a.rs`
        // (adding its claim to A's record without creating it) and is then
        // refused at the foreign `y.rs`. Its own rollback runs inside this
        // public call.
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
        // B's claim has ended, but A is still in flight and claims `a.rs`.
        assert_eq!(
            members_of(&fx, "a.rs")?,
            claims(&[(call_a.reservation.as_str(), 1)])
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
    fn both_failing_calls_across_epochs_leave_the_shared_record_unowned() -> TestResult {
        let fx = fixture()?;
        let holder = Holder {
            owner: "worker-x",
            cycle_id: "cycle-x",
        };
        reserve(&fx, &["y.rs", "z.rs"], "worker-y", "cycle-y", 1)?;

        // Call A at epoch 1: `reserve({a.rs, z.rs})` has created `a.rs` and
        // has not reached `z.rs` yet.
        let call_a = reserve(&fx, &["a.rs"], "worker-x", "cycle-x", 1)?;
        let touched = [touch(&fx, "a.rs", Counted::Named)?];

        // Call B at epoch 2: `reserve({a.rs, y.rs})` refreshes `a.rs`, which
        // moves the record's epoch to 2, and is then refused at the foreign
        // `y.rs`. Its rollback ends its claim and takes the epoch it had
        // advanced along: the record falls back to the epoch of A's claim.
        let call_b = fx
            .registry
            .reserve(&set(&["a.rs", "y.rs"]), "worker-x", "cycle-x", 2);
        assert_eq!(
            call_b,
            Err(ReserveRefusal::AlreadyOwned {
                path: "y.rs".to_owned(),
                owner: "worker-y".to_owned(),
            })
        );
        let record = record_of(&fx, "a.rs")?;
        assert_eq!(record.epoch, 1);
        assert_eq!(
            record.reservations,
            claims(&[(call_a.reservation.as_str(), 1)])
        );

        // A is refused at `z.rs` too. It is the sole claim and presents
        // epoch 1, which no longer is stale: neither call returned a
        // `LeaseSet`, so `a.rs` must be free.
        fx.registry
            .roll_back(&touched, &holder, 1, &call_a.reservation);
        assert_eq!(holder_of(&fx, "a.rs")?, None);
        reserve(&fx, &["a.rs"], "worker-y", "cycle-y", 1)?;
        assert_eq!(holder_of(&fx, "a.rs")?.as_deref(), Some("worker-y"));
        Ok(())
    }

    #[test]
    fn rollback_never_removes_a_record_without_a_reservation_id() -> TestResult {
        let fx = fixture()?;
        let holder = Holder {
            owner: "worker-x",
            cycle_id: "cycle-x",
        };
        // A record written before reservation ids existed has no such key.
        let json = format!(
            r#"{{"schema_version":1,"key_id":"{}","key":"a.rs","owner":"worker-x","cycle_id":"cycle-x","epoch":1,"acquired_at":"2026-01-01T00:00:00Z"}}"#,
            lease_id_for("a.rs")
        );
        std::fs::write(record_file(&fx, "a.rs"), &json).map_err(ctx("plant legacy record"))?;
        let legacy = record_of(&fx, "a.rs")?;
        assert!(legacy.reservations.is_empty());
        assert!(legacy.overflow.is_empty());
        assert!(legacy.generation.is_empty());
        let touched = [touch(&fx, "a.rs", Counted::Named)?];
        // No live call id (nor an empty one) is allowed to roll it back.
        fx.registry
            .roll_back(&touched, &holder, 1, "some-live-call");
        fx.registry.roll_back(&touched, &holder, 1, "");
        assert_eq!(holder_of(&fx, "a.rs")?.as_deref(), Some("worker-x"));
        // The legacy record is still the holder's: a new reserve adopts it,
        // and the unknown earlier holders stay represented at their epoch.
        let adopting = reserve(&fx, &["a.rs"], "worker-x", "cycle-x", 2)?;
        let record = record_of(&fx, "a.rs")?;
        assert_eq!(record.epoch, 2);
        assert_eq!(
            record.reservations,
            claims(&[(UNTRACKED_MEMBER, 1), (adopting.reservation.as_str(), 2)])
        );
        // The adopted record gets its own generation.
        assert!(!record.generation.is_empty());
        // If the adopting call fails, its rollback ends only its own claim:
        // the record the legacy holders kept is kept, at their epoch.
        fx.registry
            .roll_back(&touched, &holder, 2, &adopting.reservation);
        let record = record_of(&fx, "a.rs")?;
        assert_eq!(record.epoch, 1);
        assert_eq!(record.reservations, claims(&[(UNTRACKED_MEMBER, 1)]));
        assert_eq!(record.owner, "worker-x");
        // Only release (by key) frees such a record.
        fx.registry
            .release(&adopting, 1)
            .map_err(ctx("release legacy record"))?;
        assert_eq!(holder_of(&fx, "a.rs")?, None);
        Ok(())
    }

    // A record of epoch 3 with one named claim and the given `overflow` JSON.
    fn single_with_overflow(id: &str, uuid: &str, overflow: &str) -> String {
        format!(
            r#"{{"schema_version":1,"key_id":"{id}","key":"a.rs","owner":"worker-x","cycle_id":"cycle-x","epoch":3,"acquired_at":"2026-01-01T00:00:00Z","reservations":{{"{uuid}":3}},"overflow":{overflow}}}"#
        )
    }

    #[test]
    fn predecessor_record_layouts_are_migrated() -> TestResult {
        let fx = fixture()?;
        let uuid = "2f6c0f0e-1d54-4f0e-9d9e-0a1b2c3d4e5f";
        let id = lease_id_for("a.rs");
        // Exactly what the previous build wrote under the same schema
        // version: one `reservation` id, no `reservations`.
        let parent = format!(
            r#"{{"schema_version":1,"key_id":"{id}","key":"a.rs","owner":"worker-x","cycle_id":"cycle-x","epoch":1,"acquired_at":"2026-01-01T00:00:00Z","reservation":"{uuid}"}}"#
        );
        let parsed: FileLeaseRecord = serde_json::from_str(&parent).map_err(ctx("parent json"))?;
        assert_eq!(parsed.reservations, claims(&[(uuid, 1)]));
        assert!(parsed.overflow.is_empty());
        // No generation: no call that touched the record earlier can match it.
        assert!(parsed.generation.is_empty());
        // The current layout round-trips, counted epochs included.
        let counted = FileLeaseRecord {
            overflow: counts(&[(1, 2), (2, 1)]),
            generation: "g".to_owned(),
            ..parsed.clone()
        };
        for record in [&parsed, &counted] {
            let encoded = serde_json::to_string(record).map_err(ctx("encode"))?;
            let decoded: FileLeaseRecord = serde_json::from_str(&encoded).map_err(ctx("decode"))?;
            assert_eq!(&decoded, record);
        }

        // The store reads that file as a live lease (it is not quarantined)...
        std::fs::write(record_file(&fx, "a.rs"), &parent).map_err(ctx("plant parent record"))?;
        let loaded = record_of(&fx, "a.rs")?;
        assert_eq!(loaded.owner, "worker-x");
        assert_eq!(loaded.reservations, claims(&[(uuid, 1)]));
        // ...so another holder is still refused...
        let takeover = fx
            .registry
            .reserve(&set(&["a.rs"]), "worker-y", "cycle-y", 5);
        assert_eq!(
            takeover,
            Err(ReserveRefusal::AlreadyOwned {
                path: "a.rs".to_owned(),
                owner: "worker-x".to_owned(),
            })
        );
        // ...and the next mutation by the holder rewrites it in the current
        // layout, keeping the earlier call's claim.
        let next = reserve(&fx, &["a.rs"], "worker-x", "cycle-x", 2)?;
        assert_eq!(
            members_of(&fx, "a.rs")?,
            claims(&[(uuid, 1), (next.reservation.as_str(), 2)])
        );
        assert!(!record_of(&fx, "a.rs")?.generation.is_empty());
        let text = std::fs::read_to_string(record_file(&fx, "a.rs")).map_err(ctx("read file"))?;
        assert!(text.contains(r#""reservations""#));
        assert!(!text.contains(r#""reservation":"#));

        // The layout before that (a plain list of ids, no epochs) becomes
        // claims at the record's epoch.
        let list = format!(
            r#"{{"schema_version":1,"key_id":"{id}","key":"a.rs","owner":"worker-x","cycle_id":"cycle-x","epoch":3,"acquired_at":"2026-01-01T00:00:00Z","reservations":["{uuid}","{UNTRACKED_MEMBER}"]}}"#
        );
        let listed: FileLeaseRecord = serde_json::from_str(&list).map_err(ctx("list json"))?;
        assert_eq!(
            listed.reservations,
            claims(&[(uuid, 3), (UNTRACKED_MEMBER, 3)])
        );

        // The previous build's layout (named claims and a single counted
        // number, no generation): the number belongs to the record's epoch.
        let single = format!(
            r#"{{"schema_version":1,"key_id":"{id}","key":"a.rs","owner":"worker-x","cycle_id":"cycle-x","epoch":3,"acquired_at":"2026-01-01T00:00:00Z","reservations":{{"{uuid}":3}},"overflow":2}}"#
        );
        let single: FileLeaseRecord = serde_json::from_str(&single).map_err(ctx("single json"))?;
        assert_eq!(single.overflow, counts(&[(3, 2)]));
        assert!(single.generation.is_empty());
        let none = single_with_overflow(&id, uuid, "0");
        let none: FileLeaseRecord = serde_json::from_str(&none).map_err(ctx("zero json"))?;
        assert!(none.overflow.is_empty());
        let per_epoch = single_with_overflow(&id, uuid, r#"{"1":2,"3":1}"#);
        let per_epoch: FileLeaseRecord =
            serde_json::from_str(&per_epoch).map_err(ctx("map json"))?;
        assert_eq!(per_epoch.overflow, counts(&[(1, 2), (3, 1)]));
        for bad in ["-1", "4294967296", r#""x""#, r#"{"x":1}"#, "[1]"] {
            let text = single_with_overflow(&id, uuid, bad);
            assert!(
                serde_json::from_str::<FileLeaseRecord>(&text).is_err(),
                "overflow {bad} must be refused"
            );
        }

        // Everything else stays refused.
        let unknown = parent.replacen("\"epoch\":1", "\"epoch\":1,\"bogus\":true", 1);
        assert!(serde_json::from_str::<FileLeaseRecord>(&unknown).is_err());
        let wrong_type = parent.replacen("\"reservation\":", "\"reservations\":", 1);
        assert!(serde_json::from_str::<FileLeaseRecord>(&wrong_type).is_err());
        Ok(())
    }

    // One call more than a record names: the last one is only counted.
    fn reserve_beyond_cap(fx: &Fixture) -> TestResult<Vec<LeaseSet>> {
        let mut calls = Vec::new();
        for _ in 0..=MAX_RESERVATION_MEMBERS {
            calls.push(reserve(fx, &["a.rs"], "worker-x", "cycle-x", 1)?);
        }
        Ok(calls)
    }

    #[test]
    fn overflow_members_are_rolled_back_like_named_ones() -> TestResult {
        let fx = fixture()?;
        let holder = Holder {
            owner: "worker-x",
            cycle_id: "cycle-x",
        };

        // 65 calls on one record; the record names 64 of them and counts the
        // last one. Then all of them fail, the named ones first: the record
        // stays, held by the counted call alone...
        let calls = reserve_beyond_cap(&fx)?;
        let (counted, named_calls) = calls
            .split_last()
            .ok_or_else(missing("the loop reserved at least once"))?;
        let named_touch = touch(&fx, "a.rs", Counted::Named)?;
        let counted_touch = touch(&fx, "a.rs", Counted::Overflow)?;
        let record = record_of(&fx, "a.rs")?;
        assert_eq!(record.reservations.len(), MAX_RESERVATION_MEMBERS);
        assert_eq!(record.overflow, counts(&[(1, 1)]));
        assert!(!record.reservations.contains_key(&counted.reservation));
        for call in named_calls {
            fx.registry.roll_back(
                std::slice::from_ref(&named_touch),
                &holder,
                1,
                &call.reservation,
            );
        }
        let record = record_of(&fx, "a.rs")?;
        assert!(record.reservations.is_empty());
        assert_eq!(record.overflow, counts(&[(1, 1)]));
        assert_eq!(record.owner, "worker-x");
        // ...and the counted call's rollback deletes it: all 65 calls failed,
        // so nobody holds the path.
        fx.registry.roll_back(
            std::slice::from_ref(&counted_touch),
            &holder,
            1,
            &counted.reservation,
        );
        assert_eq!(holder_of(&fx, "a.rs")?, None);
        reserve(&fx, &["a.rs"], "worker-y", "cycle-y", 1)?;
        fx.registry
            .release(
                &LeaseSet {
                    cycle_id: "cycle-y".to_owned(),
                    owner: "worker-y".to_owned(),
                    epoch: 1,
                    reservation: String::new(),
                    keys: set(&["a.rs"]),
                },
                1,
            )
            .map_err(ctx("release"))?;

        // The other order: the counted call fails first and the named claims
        // keep the record until the last of them ends.
        let calls = reserve_beyond_cap(&fx)?;
        let (counted, named_calls) = calls
            .split_last()
            .ok_or_else(missing("the loop reserved at least once"))?;
        let named_touch = touch(&fx, "a.rs", Counted::Named)?;
        let counted_touch = touch(&fx, "a.rs", Counted::Overflow)?;
        fx.registry.roll_back(
            std::slice::from_ref(&counted_touch),
            &holder,
            1,
            &counted.reservation,
        );
        let record = record_of(&fx, "a.rs")?;
        assert_eq!(record.reservations.len(), MAX_RESERVATION_MEMBERS);
        assert!(record.overflow.is_empty());
        let (last, rest) = named_calls
            .split_last()
            .ok_or_else(missing("the record names 64 calls"))?;
        for call in rest {
            fx.registry.roll_back(
                std::slice::from_ref(&named_touch),
                &holder,
                1,
                &call.reservation,
            );
        }
        assert_eq!(members_of(&fx, "a.rs")?.len(), 1);
        fx.registry.roll_back(
            std::slice::from_ref(&named_touch),
            &holder,
            1,
            &last.reservation,
        );
        assert_eq!(holder_of(&fx, "a.rs")?, None);
        Ok(())
    }

    #[test]
    fn overflow_claims_across_epochs_roll_back_in_any_order() -> TestResult {
        let fx = fixture()?;
        let holder = Holder {
            owner: "worker-x",
            cycle_id: "cycle-x",
        };
        for counted_c_first in [true, false] {
            // 64 named claims at epoch 1, then two counted calls: B at epoch
            // 1 and C at epoch 2, which moves the record to epoch 2.
            let calls = reserve_beyond_cap(&fx)?;
            let (call_b, named_calls) = calls
                .split_last()
                .ok_or_else(missing("the loop reserved at least once"))?;
            let call_c = reserve(&fx, &["a.rs"], "worker-x", "cycle-x", 2)?;
            let named_touch = touch(&fx, "a.rs", Counted::Named)?;
            let counted_touch = touch(&fx, "a.rs", Counted::Overflow)?;
            let record = record_of(&fx, "a.rs")?;
            assert_eq!(record.epoch, 2);
            assert_eq!(record.reservations.len(), MAX_RESERVATION_MEMBERS);
            assert_eq!(record.overflow, counts(&[(1, 1), (2, 1)]));

            // The counted call of the epoch that is rolled back first takes
            // its epoch with it, whichever one that is.
            let (first, first_epoch, last, last_epoch, epoch_between) = if counted_c_first {
                (&call_c, 2, call_b, 1, 1)
            } else {
                (call_b, 1, &call_c, 2, 2)
            };
            fx.registry.roll_back(
                std::slice::from_ref(&counted_touch),
                &holder,
                first_epoch,
                &first.reservation,
            );
            let record = record_of(&fx, "a.rs")?;
            assert_eq!(record.epoch, epoch_between);
            assert_eq!(record.overflow, counts(&[(last_epoch, 1)]));
            for call in named_calls {
                fx.registry.roll_back(
                    std::slice::from_ref(&named_touch),
                    &holder,
                    1,
                    &call.reservation,
                );
            }
            // Only the other counted call is left. It is the sole claim and
            // its epoch is the record's, so it may delete the record.
            let record = record_of(&fx, "a.rs")?;
            assert!(record.reservations.is_empty());
            assert_eq!(record.epoch, last_epoch);
            assert_eq!(record.owner, "worker-x");
            fx.registry.roll_back(
                std::slice::from_ref(&counted_touch),
                &holder,
                last_epoch,
                &last.reservation,
            );
            assert_eq!(holder_of(&fx, "a.rs")?, None);
        }
        reserve(&fx, &["a.rs"], "worker-y", "cycle-y", 1)?;
        Ok(())
    }

    #[test]
    fn old_overflow_rollback_cannot_touch_a_recreated_record() -> TestResult {
        let fx = fixture()?;
        let holder = Holder {
            owner: "worker-x",
            cycle_id: "cycle-x",
        };

        // An old call X0 is the counted 65th call on the record and touches
        // `a.rs` in this incarnation of it.
        let old_calls = reserve_beyond_cap(&fx)?;
        let (old_counted, old_named) = old_calls
            .split_last()
            .ok_or_else(missing("the loop reserved at least once"))?;
        let old_touch = touch(&fx, "a.rs", Counted::Overflow)?;
        let old_generation = old_touch.placement.generation.clone();
        let first = old_named
            .first()
            .ok_or_else(missing("the record names 64 calls"))?;
        // The holder releases the path and it is created anew.
        fx.registry.release(first, 1).map_err(ctx("release"))?;
        assert_eq!(holder_of(&fx, "a.rs")?, None);

        // The new incarnation again has 64 named claims and a counted call Y
        // that returned a successful `LeaseSet`.
        let new_calls = reserve_beyond_cap(&fx)?;
        let (new_counted, new_named) = new_calls
            .split_last()
            .ok_or_else(missing("the loop reserved at least once"))?;
        let record = record_of(&fx, "a.rs")?;
        assert_ne!(record.generation, old_generation);
        assert_eq!(record.overflow, counts(&[(1, 1)]));
        assert!(!record.reservations.contains_key(&new_counted.reservation));

        // X0's delayed failure must not decrement Y's count.
        fx.registry.roll_back(
            std::slice::from_ref(&old_touch),
            &holder,
            1,
            &old_counted.reservation,
        );
        assert_eq!(record_of(&fx, "a.rs")?.overflow, counts(&[(1, 1)]));

        // The new named calls fail; Y still holds a set, so the record stays
        // and nobody else can take the path.
        let named_touch = touch(&fx, "a.rs", Counted::Named)?;
        for call in new_named {
            fx.registry.roll_back(
                std::slice::from_ref(&named_touch),
                &holder,
                1,
                &call.reservation,
            );
        }
        let record = record_of(&fx, "a.rs")?;
        assert!(record.reservations.is_empty());
        assert_eq!(record.overflow, counts(&[(1, 1)]));
        let takeover = fx
            .registry
            .reserve(&set(&["a.rs"]), "worker-y", "cycle-y", 5);
        assert_eq!(
            takeover,
            Err(ReserveRefusal::AlreadyOwned {
                path: "a.rs".to_owned(),
                owner: "worker-x".to_owned(),
            })
        );
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
        let generation = record_of(&fx, "a.rs")?.generation;
        assert!(!generation.is_empty());
        let again = reserve(&fx, &["a.rs", "b.rs"], "worker-a", "cycle-a", 1)?;
        // A refresh keeps the incarnation of the record.
        assert_eq!(record_of(&fx, "a.rs")?.generation, generation);
        // Same holder, epoch and keys; every call carries its own
        // reservation id.
        assert_eq!(first.keys, again.keys);
        assert_eq!(first.epoch, again.epoch);
        assert_eq!(first.owner, again.owner);
        assert_eq!(first.cycle_id, again.cycle_id);
        assert_ne!(first.reservation, again.reservation);
        // The record is shared by both calls: the claims grow by the new id.
        for path in ["a.rs", "b.rs"] {
            assert_eq!(
                members_of(&fx, path)?,
                claims(&[
                    (first.reservation.as_str(), 1),
                    (again.reservation.as_str(), 1)
                ])
            );
        }
        let newer = reserve(&fx, &["a.rs", "b.rs"], "worker-a", "cycle-a", 2)?;
        assert_eq!(newer.epoch, 2);
        assert_eq!(
            members_of(&fx, "a.rs")?,
            claims(&[
                (first.reservation.as_str(), 1),
                (again.reservation.as_str(), 1),
                (newer.reservation.as_str(), 2),
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
