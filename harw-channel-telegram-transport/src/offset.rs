//! Durable persistence for a Telegram long-poll update offset.
//!
//! The caller supplies a profile/channel-specific root. This module owns only
//! the `offset.json` projection and its sidecar lock; it deliberately does not
//! depend on the session store.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs4::FileExt;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::error::{TelegramTransportError, TransportResult};

const OFFSET_FILE: &str = "offset.json";
const LOCK_FILE: &str = "offset.lock";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OffsetRecord {
    offset: i64,
}

/// A restart-safe Telegram long-poll offset rooted at a caller-provided path.
///
/// The store writes `<root>/offset.json` while holding an exclusive lock on
/// `<root>/offset.lock`. Writes are staged in the same directory, synced, and
/// atomically persisted over the live file, so readers observe either the old
/// complete record or the new complete record.
#[derive(Debug, Clone)]
pub struct TelegramOffsetStore {
    root: PathBuf,
}

impl TelegramOffsetStore {
    /// Creates an offset store rooted at `root`.
    ///
    /// The root is created lazily by [`Self::load`] or [`Self::store`], and is
    /// expected to already be scoped by the caller to one profile and channel.
    #[must_use]
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Loads the last persisted offset, or `None` when no offset exists yet.
    ///
    /// Malformed JSON and negative offsets are rejected as invalid local data;
    /// callers must fail closed rather than silently restarting from an
    /// untrusted cursor.
    pub fn load(&self) -> TransportResult<Option<i64>> {
        self.with_lock(|| {
            let path = self.offset_path();
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(TelegramTransportError::Io(error)),
            };

            let record: OffsetRecord = serde_json::from_slice(&bytes).map_err(invalid_data)?;
            validate_offset(record.offset)?;
            Ok(Some(record.offset))
        })
    }

    /// Persists the next Telegram long-poll offset atomically.
    ///
    /// # Errors
    /// Returns an invalid-data error for negative offsets and an I/O error for
    /// lock, staging, syncing, or atomic-replacement failures.
    pub fn store(&self, next_offset: i64) -> TransportResult<()> {
        validate_offset(next_offset)?;
        self.with_lock(|| {
            let record = OffsetRecord {
                offset: next_offset,
            };
            let bytes = serde_json::to_vec(&record).map_err(invalid_data)?;
            let mut temporary = NamedTempFile::new_in(&self.root)?;
            temporary.write_all(&bytes)?;
            temporary.as_file().sync_all()?;
            temporary
                .persist(self.offset_path())
                .map_err(|error| TelegramTransportError::Io(error.error))?;
            Ok(())
        })
    }

    fn offset_path(&self) -> PathBuf {
        self.root.join(OFFSET_FILE)
    }

    fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    fn with_lock<T>(&self, operation: impl FnOnce() -> TransportResult<T>) -> TransportResult<T> {
        fs::create_dir_all(&self.root)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.lock_path())?;
        // `std::fs::File::lock`/`unlock` are only stable since Rust 1.89.0;
        // the workspace MSRV is 1.85.0. `fs4::FileExt` (already a workspace
        // dependency, used the same way in harw-session-store/harw-channel)
        // provides the identical blocking-exclusive-lock semantics on stable
        // 1.85 via a platform syscall shim, returning the same
        // `std::io::Result<()>` shape so `?` and the `unlock_result` match
        // below are unchanged.
        FileExt::lock(&lock)?;

        let result = operation();
        let unlock_result = FileExt::unlock(&lock);
        match (result, unlock_result) {
            (Err(error), _) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
            (Ok(_), Err(error)) => Err(TelegramTransportError::Io(error)),
        }
    }
}

fn validate_offset(offset: i64) -> TransportResult<()> {
    if offset < 0 {
        return Err(invalid_data(
            "Telegram long-poll offset must be non-negative",
        ));
    }
    Ok(())
}

fn invalid_data(error: impl std::fmt::Display) -> TelegramTransportError {
    TelegramTransportError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        error.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_offset_loads_as_none() {
        let root = tempfile::tempdir().expect("temporary root");
        let store = TelegramOffsetStore::new(root.path());

        assert_eq!(store.load().expect("load missing offset"), None);
    }

    #[test]
    fn store_reopen_roundtrip() {
        let root = tempfile::tempdir().expect("temporary root");
        TelegramOffsetStore::new(root.path())
            .store(42)
            .expect("store offset");

        assert_eq!(
            TelegramOffsetStore::new(root.path())
                .load()
                .expect("reopen offset"),
            Some(42)
        );
    }

    #[test]
    fn malformed_and_negative_offsets_reject() {
        let root = tempfile::tempdir().expect("temporary root");
        let path = root.path().join(OFFSET_FILE);
        fs::write(&path, br#"{"offset":"not-an-integer"}"#).expect("write malformed offset");
        let store = TelegramOffsetStore::new(root.path());

        assert!(
            matches!(store.load(), Err(TelegramTransportError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData)
        );

        fs::write(&path, br#"{"offset":-1}"#).expect("write negative offset");
        assert!(
            matches!(store.load(), Err(TelegramTransportError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData)
        );
        assert!(
            matches!(store.store(-1), Err(TelegramTransportError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData)
        );
    }

    #[test]
    fn final_file_is_always_a_complete_json_record() {
        let root = tempfile::tempdir().expect("temporary root");
        let store = TelegramOffsetStore::new(root.path());
        store.store(7).expect("initial store");
        store.store(8).expect("replacement store");

        let bytes = fs::read(root.path().join(OFFSET_FILE)).expect("read final offset");
        let record: OffsetRecord = serde_json::from_slice(&bytes).expect("complete offset JSON");
        assert_eq!(record.offset, 8);
        assert!(!bytes.is_empty());
    }
}
