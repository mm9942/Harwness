//! Generic, typed record store: one JSON file per record, one lock per record.
//!
//! # On-disk layout (stable; `harw-session-store` data lives in it)
//!
//! ```text
//! <root>/
//! ├── records/<id>.json               the record (compact JSON, mode 0o600)
//! ├── records/<id>.json.corrupt-<ts>  a quarantined, undecodable record
//! ├── records/.<id>.<unique>.tmp      an in-flight (or crash-orphaned) write
//! ├── locks/<id>.lock                 fs4 exclusive advisory lock per record
//! └── <kind>/<id>.json                sidecars, e.g. `approvals/`
//! ```
//!
//! # Concurrency
//! Every mutation takes the record's exclusive fs4 try-lock (never blocks:
//! a held lock is [`StoreError::Contended`]), then loads, mutates, persists
//! atomically and fsyncs the directory, then unlocks. Unrelated records never
//! contend. [`RecordStore::list`] and [`RecordStore::load`] take no lock and
//! observe an eventually-consistent snapshot — but never a half-written
//! record, because records are only ever replaced by rename.
//!
//! # Corruption
//! An undecodable record, or one whose embedded id differs from its file
//! name, is [`StoreError::Corrupt`]. Under the record lock it is moved aside
//! to `<id>.json.corrupt-<ts>` so it can no longer poison later scans;
//! [`RecordStore::list`] skips (and quarantines) such records instead of
//! failing the whole page.

use std::io;
use std::marker::PhantomData;
use std::path::Path;

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use fs4::FileExt;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{StoreError, StoreResult};
use crate::fsops::{
    self, ReadOutcome, lock_name, open_subdir, read_regular, record_name, record_stem, temp_owner,
    validate_id,
};

/// Directory holding the records.
pub const RECORDS_DIR: &str = "records";
/// Directory holding the per-record lock files.
pub const LOCKS_DIR: &str = "locks";
/// Upper bound of one [`RecordStore::list`] page (the limit is clamped to
/// `1..=MAX_PAGE`).
pub const MAX_PAGE: usize = 100;

/// A value the store can persist under its own id.
pub trait StoreRecord: Serialize + DeserializeOwned {
    /// The record id; must be a safe path component
    /// ([`crate::validate_id`]) and must never change over the record's life.
    fn record_id(&self) -> &str;
}

impl StoreRecord for harw_job_core::StoredJob {
    fn record_id(&self) -> &str {
        self.job.id.as_str()
    }
}

/// One page of [`RecordStore::list`], sorted by record id.
#[derive(Debug, Clone, PartialEq)]
pub struct Page<R> {
    /// Matching records after the cursor, at most `limit`.
    pub records: Vec<R>,
    /// Id to pass as `cursor` for the next page; `None` when done.
    pub next_cursor: Option<String>,
}

/// A held exclusive record lock. Dropping it also releases the lock (the
/// file handle closes); [`RecordLock::release`] reports unlock errors.
#[derive(Debug)]
pub struct RecordLock {
    file: std::fs::File,
}

impl RecordLock {
    /// Releases the lock explicitly.
    ///
    /// # Errors
    /// [`StoreError::Io`] if unlocking fails.
    pub fn release(self) -> StoreResult<()> {
        FileExt::unlock(&self.file).map_err(StoreError::Io)
    }

    /// Releases the lock; an operation error wins over an unlock error.
    fn finish<T, E: From<StoreError>>(self, result: Result<T, E>) -> Result<T, E> {
        let unlocked = self.release();
        match (result, unlocked) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(E::from(error)),
            (Ok(value), Ok(())) => Ok(value),
        }
    }
}

/// File-backed record store rooted at an opened capability directory.
pub struct RecordStore<R> {
    root: Dir,
    _record: PhantomData<fn() -> R>,
}

impl<R> RecordStore<R> {
    /// Uses `root` as the store root. No ambient authority is involved: the
    /// store can only ever touch entries beneath `root`.
    #[must_use]
    pub fn from_dir(root: Dir) -> Self {
        Self {
            root,
            _record: PhantomData,
        }
    }

    /// Bootstrap from a path: creates `path` (and the layout) if missing and
    /// opens it. This is the only use of ambient authority in the crate.
    ///
    /// # Errors
    /// [`StoreError::Io`] if the directory cannot be created or opened.
    pub fn create_ambient(path: &Path) -> StoreResult<Self> {
        Dir::create_ambient_dir_all(path, ambient_authority())?;
        let store = Self::from_dir(Dir::open_ambient_dir(path, ambient_authority())?);
        store.ensure_layout()?;
        Ok(store)
    }

    /// Opens an existing store root; `Ok(None)` if `path` does not exist.
    /// Creates nothing.
    ///
    /// # Errors
    /// [`StoreError::Io`] for any error other than "not found".
    pub fn open_existing_ambient(path: &Path) -> StoreResult<Option<Self>> {
        match Dir::open_ambient_dir(path, ambient_authority()) {
            Ok(root) => Ok(Some(Self::from_dir(root))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(StoreError::Io(error)),
        }
    }

    /// The root capability.
    #[must_use]
    pub fn root_dir(&self) -> &Dir {
        &self.root
    }

    /// Creates `records/` and `locks/` if missing.
    ///
    /// # Errors
    /// [`StoreError::Io`].
    pub fn ensure_layout(&self) -> StoreResult<()> {
        self.root.create_dir_all(RECORDS_DIR)?;
        self.root.create_dir_all(LOCKS_DIR)?;
        Ok(())
    }

    /// Takes the exclusive lock of record `id` without blocking.
    ///
    /// # Errors
    /// [`StoreError::InvalidId`], [`StoreError::Contended`] if another writer
    /// holds it, [`StoreError::Io`] (including a symlinked lock file).
    pub fn lock(&self, id: &str) -> StoreResult<RecordLock> {
        let id = validate_id(id)?;
        self.ensure_layout()?;
        self.lock_valid(id)
    }

    fn lock_valid(&self, id: &str) -> StoreResult<RecordLock> {
        let locks = self.root.open_dir(LOCKS_DIR)?;
        let file = fsops::open_lock_file(&locks, &lock_name(id))?;
        match FileExt::try_lock(&file) {
            Ok(()) => Ok(RecordLock { file }),
            Err(fs4::TryLockError::WouldBlock) => Err(StoreError::Contended { id: id.to_owned() }),
            Err(fs4::TryLockError::Error(error)) => Err(StoreError::Io(error)),
        }
    }

    fn records_dir(&self) -> StoreResult<Dir> {
        Ok(self.root.open_dir(RECORDS_DIR)?)
    }

    /// Atomically writes (or replaces) sidecar `<kind>/<id>.json`. Sidecars
    /// are not covered by the record lock; they carry audit data that is
    /// written after the owning record transition is already durable.
    ///
    /// # Errors
    /// [`StoreError::InvalidId`] for an unsafe `kind`/`id` (or a reserved
    /// `kind`), [`StoreError::Io`].
    pub fn write_sidecar(&self, kind: &str, id: &str, bytes: &[u8]) -> StoreResult<()> {
        let kind = sidecar_kind(kind)?;
        self.root.create_dir_all(kind)?;
        let id = validate_id(id)?;
        let dir = self.root.open_dir(kind)?;
        fsops::replace_atomic(&dir, &record_name(id), id, bytes, "sidecar")?;
        Ok(())
    }

    /// Reads sidecar `<kind>/<id>.json`; `Ok(None)` if absent. Lock-free.
    ///
    /// # Errors
    /// [`StoreError::InvalidId`], [`StoreError::Io`] (symlink, not a regular
    /// file, read failure).
    pub fn read_sidecar(&self, kind: &str, id: &str) -> StoreResult<Option<Vec<u8>>> {
        let kind = sidecar_kind(kind)?;
        let id = validate_id(id)?;
        let Some(dir) = open_subdir(&self.root, kind)? else {
            return Ok(None);
        };
        match read_regular(&dir, &record_name(id), "sidecar")? {
            ReadOutcome::Bytes(bytes) => Ok(Some(bytes)),
            ReadOutcome::Missing => Ok(None),
            ReadOutcome::NotRegular => Err(StoreError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sidecar is not a regular file",
            ))),
        }
    }

    /// Removes temp files orphaned by a crash between temp write and rename.
    /// A temp is only removed while its owner's record lock is held, so an
    /// in-flight write is never disturbed; owners whose lock is contended are
    /// skipped. Returns the number of removed files.
    ///
    /// # Errors
    /// [`StoreError::Io`] on directory or unlock failures.
    pub fn remove_orphaned_temps(&self) -> StoreResult<usize> {
        let Some(records) = open_subdir(&self.root, RECORDS_DIR)? else {
            return Ok(0);
        };
        self.ensure_layout()?;
        let mut removed = 0_usize;
        for entry in records.entries()? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let Some(owner) = temp_owner(name) else {
                continue;
            };
            let lock = match self.lock_valid(owner) {
                Ok(lock) => lock,
                Err(StoreError::Contended { .. }) => continue,
                Err(error) => return Err(error),
            };
            let result = match records.remove_file(name) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(StoreError::Io(error)),
            };
            if lock.finish(result)? {
                tracing::info!(name, "orphaned record temp file removed");
                removed += 1;
            }
        }
        if removed > 0 {
            fsops::sync_dir(&records)?;
        }
        Ok(removed)
    }
}

impl<R: StoreRecord> RecordStore<R> {
    /// Persists a new record. An existing id is never overwritten.
    ///
    /// # Errors
    /// [`StoreError::InvalidId`], [`StoreError::AlreadyExists`],
    /// [`StoreError::Contended`], [`StoreError::Encode`], [`StoreError::Io`]
    /// (including a symlinked record or lock), [`StoreError::Unsupported`].
    pub fn create(&self, record: &R) -> StoreResult<()> {
        let id = validate_id(record.record_id())?;
        self.ensure_layout()?;
        let lock = self.lock_valid(id)?;
        let result = self.create_under_lock(id, record);
        lock.finish(result)
    }

    fn create_under_lock(&self, id: &str, record: &R) -> StoreResult<()> {
        let records = self.records_dir()?;
        let name = record_name(id);
        if fsops::exists_no_symlink(&records, &name, "record")? {
            return Err(StoreError::AlreadyExists { id: id.to_owned() });
        }
        let bytes = encode(id, record)?;
        fsops::create_noclobber(&records, &name, id, &bytes)
    }

    /// Loads one record without locking.
    ///
    /// # Errors
    /// [`StoreError::InvalidId`], [`StoreError::NotFound`],
    /// [`StoreError::Corrupt`], [`StoreError::Io`] (including a symlink).
    pub fn load(&self, id: &str) -> StoreResult<R> {
        let id = validate_id(id)?;
        let Some(records) = open_subdir(&self.root, RECORDS_DIR)? else {
            return Err(StoreError::NotFound { id: id.to_owned() });
        };
        read_record(&records, id)
    }

    /// Lock → load → mutate → persist (temp + rename) → fsync → unlock.
    ///
    /// `operation` runs on an in-memory copy; if it returns `Err`, nothing
    /// is written. A corrupt record is quarantined under the lock and the
    /// caller still receives [`StoreError::Corrupt`].
    ///
    /// # Errors
    /// Whatever `operation` returns, plus every [`StoreError`] of
    /// [`RecordStore::load`], [`StoreError::Contended`] and write failures,
    /// converted into `E`.
    pub fn update_locked<T, E, F>(&self, id: &str, operation: F) -> Result<T, E>
    where
        E: From<StoreError>,
        F: FnOnce(&mut R) -> Result<T, E>,
    {
        let id = validate_id(id)?;
        self.ensure_layout()?;
        let lock = self.lock_valid(id)?;
        let result = self.update_under_lock(id, operation);
        lock.finish(result)
    }

    fn update_under_lock<T, E, F>(&self, id: &str, operation: F) -> Result<T, E>
    where
        E: From<StoreError>,
        F: FnOnce(&mut R) -> Result<T, E>,
    {
        let records = self.records_dir()?;
        let mut record = match read_record::<R>(&records, id) {
            Ok(record) => record,
            Err(error @ StoreError::Corrupt { .. }) => {
                quarantine_logged(&records, id);
                return Err(E::from(error));
            }
            Err(error) => return Err(E::from(error)),
        };
        let output = operation(&mut record)?;
        let bytes = encode(id, &record)?;
        fsops::replace_atomic(&records, &record_name(id), id, &bytes, "record")
            .map_err(StoreError::Io)?;
        Ok(output)
    }

    /// Lists records matching `filter` with id strictly after `cursor`,
    /// sorted by id, at most `limit` (clamped to `1..=MAX_PAGE`). Lock-free.
    /// Symlinks, non-`.json` names (temps, quarantined files) and unreadable
    /// entries are skipped; corrupt records are quarantined and skipped.
    ///
    /// # Errors
    /// [`StoreError::Io`] if the records directory cannot be read.
    pub fn list<F>(&self, cursor: Option<&str>, limit: usize, mut filter: F) -> StoreResult<Page<R>>
    where
        F: FnMut(&R) -> bool,
    {
        let Some(records) = open_subdir(&self.root, RECORDS_DIR)? else {
            return Ok(Page {
                records: Vec::new(),
                next_cursor: None,
            });
        };
        let mut found = Vec::new();
        for entry in records.entries()? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::warn!(error = %error, "record entry unreadable; skipped");
                    continue;
                }
            };
            match entry.file_type() {
                Ok(file_type) if file_type.is_symlink() => continue,
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(error = %error, "record type unreadable; skipped");
                    continue;
                }
            }
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let Some(stem) = record_stem(name) else {
                continue;
            };
            let Some(record) = self.load_listed(&records, name, stem) else {
                continue;
            };
            if !filter(&record) || cursor.is_some_and(|cursor| record.record_id() <= cursor) {
                continue;
            }
            found.push(record);
        }
        found.sort_by(|left, right| left.record_id().cmp(right.record_id()));
        let limit = limit.clamp(1, MAX_PAGE);
        let next_cursor = if found.len() > limit {
            found
                .get(limit - 1)
                .map(|record| record.record_id().to_owned())
        } else {
            None
        };
        found.truncate(limit);
        Ok(Page {
            records: found,
            next_cursor,
        })
    }

    /// Collects every matching record by walking all pages.
    ///
    /// # Errors
    /// As [`RecordStore::list`].
    pub fn list_all<F>(&self, mut filter: F) -> StoreResult<Vec<R>>
    where
        F: FnMut(&R) -> bool,
    {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = self.list(cursor.as_deref(), MAX_PAGE, &mut filter)?;
            all.extend(page.records);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(all),
            }
        }
    }

    // Reads one listed record lock-free; corrupt records are quarantined.
    fn load_listed(&self, records: &Dir, name: &str, stem: &str) -> Option<R> {
        let bytes = match read_regular(records, name, "record") {
            Ok(ReadOutcome::Bytes(bytes)) => bytes,
            Ok(ReadOutcome::Missing) => return None,
            Ok(ReadOutcome::NotRegular) => {
                tracing::warn!(name, "record is not a regular file; skipped");
                return None;
            }
            Err(error) => {
                tracing::warn!(name, error = %error, "record unreadable; skipped");
                return None;
            }
        };
        match serde_json::from_slice::<R>(&bytes) {
            Ok(record) if record.record_id() == stem => Some(record),
            Ok(_) => {
                self.quarantine_listed(stem, "record id does not match its canonical path");
                None
            }
            Err(error) => {
                self.quarantine_listed(stem, &error.to_string());
                None
            }
        }
    }

    // Moves a corrupt listed record aside, but only under its record lock and
    // only after re-confirming it is still corrupt.
    fn quarantine_listed(&self, stem: &str, detail: &str) {
        if validate_id(stem).is_err() {
            tracing::warn!(
                name = stem,
                detail,
                "corrupt record has an unsafe name; skipped without quarantine"
            );
            return;
        }
        if let Err(error) = self.ensure_layout() {
            tracing::warn!(error = %error, "record store layout unavailable");
            return;
        }
        let lock = match self.lock_valid(stem) {
            Ok(lock) => lock,
            Err(error) => {
                tracing::warn!(
                    id = stem,
                    error = %error,
                    detail,
                    "corrupt record skipped; quarantine deferred"
                );
                return;
            }
        };
        let result = self.requarantine(stem);
        match lock.finish(result) {
            Ok(Some(quarantine)) => tracing::warn!(
                id = stem,
                quarantine = %quarantine,
                detail,
                "corrupt record quarantined"
            ),
            Ok(None) => {}
            Err(error) => tracing::warn!(
                id = stem,
                error = %error,
                "corrupt record could not be quarantined; skipped"
            ),
        }
    }

    fn requarantine(&self, id: &str) -> StoreResult<Option<String>> {
        let records = self.records_dir()?;
        match read_record::<R>(&records, id) {
            Err(StoreError::Corrupt { .. }) => {
                fsops::quarantine(&records, &record_name(id)).map_err(StoreError::Io)
            }
            Ok(_) | Err(StoreError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

fn sidecar_kind(kind: &str) -> StoreResult<&str> {
    let kind = validate_id(kind)?;
    if kind == RECORDS_DIR || kind == LOCKS_DIR {
        return Err(StoreError::InvalidId {
            id: kind.to_owned(),
        });
    }
    Ok(kind)
}

fn encode<T: Serialize>(id: &str, value: &T) -> StoreResult<Vec<u8>> {
    serde_json::to_vec(value).map_err(|source| StoreError::Encode {
        id: id.to_owned(),
        source,
    })
}

fn read_record<R: StoreRecord>(records: &Dir, id: &str) -> StoreResult<R> {
    let bytes = match read_regular(records, &record_name(id), "record")? {
        ReadOutcome::Bytes(bytes) => bytes,
        ReadOutcome::Missing | ReadOutcome::NotRegular => {
            return Err(StoreError::NotFound { id: id.to_owned() });
        }
    };
    let record: R = serde_json::from_slice(&bytes).map_err(|error| StoreError::Corrupt {
        id: id.to_owned(),
        detail: error.to_string(),
    })?;
    if record.record_id() != id {
        return Err(StoreError::Corrupt {
            id: id.to_owned(),
            detail: "record id does not match its canonical path".to_owned(),
        });
    }
    Ok(record)
}

fn quarantine_logged(records: &Dir, id: &str) {
    match fsops::quarantine(records, &record_name(id)) {
        Ok(Some(quarantine)) => tracing::warn!(
            id,
            quarantine = %quarantine,
            "corrupt record quarantined"
        ),
        Ok(None) => {}
        Err(error) => tracing::warn!(
            id,
            error = %error,
            "corrupt record could not be quarantined"
        ),
    }
}
