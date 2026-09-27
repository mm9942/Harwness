//! Capability-relative file primitives shared by every store operation.
//!
//! # Responsibility
//! Everything here works on an opened [`cap_std::fs::Dir`]; no function takes
//! an ambient path. cap-std resolves every name beneath its directory handle,
//! so a symbolic link can never lead a store operation outside the store
//! root. In addition, the final component of every record, lock, sidecar and
//! temp name is checked with `symlink_metadata` and rejected when it is a
//! symbolic link (same contract as the former `harw-session-store` code).
//!
//! # Durability
//! A write goes to a fresh sibling temp file (`create_new`, mode `0o600` on
//! Unix — identical to `tempfile::NamedTempFile`), is `sync_all`ed, then
//! renamed (replace) or hard-linked (no-clobber) onto its final name, and the
//! directory itself is `fsync`ed. A crash therefore leaves either the old or
//! the new complete record, plus at most an orphaned temp file whose name
//! never ends in `.json` and which listing ignores.
//!
//! # Known limitation
//! cap-std offers no stable "open without following the last symlink" flag
//! (that lives in `cap-fs-ext`). The symlink check before each open is thus a
//! check-then-open sequence; the race window can only swap in a link that
//! still resolves beneath the store root, never outside it.

use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use cap_std::fs::OpenOptionsExt;
use cap_std::fs::{Dir, File, OpenOptions};

use crate::error::{StoreError, StoreResult};

/// Extension of a durable record file.
pub(crate) const RECORD_EXTENSION: &str = "json";
/// Extension of a per-record lock file.
pub(crate) const LOCK_EXTENSION: &str = "lock";
/// Suffix of an in-flight temp file.
pub(crate) const TEMP_SUFFIX: &str = ".tmp";
/// Marker inside a quarantine name: `<name>.corrupt-<ts>[-<n>]`.
const QUARANTINE_MARKER: &str = "corrupt";
/// Same bound as `harw-session-store::store::QUARANTINE_ATTEMPTS`.
const QUARANTINE_ATTEMPTS: u32 = 16;
/// Attempts to find a free temp name before giving up.
const TEMP_ATTEMPTS: u32 = 16;
/// `tempfile` creates with `0o600`; the store keeps that on-disk contract.
#[cfg(unix)]
const TEMP_MODE: u32 = 0o600;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Validates `id` as one safe path component: non-empty ASCII
/// alphanumerics, `-` and `_` only.
///
/// # Errors
/// [`StoreError::InvalidId`] for any other value.
pub fn validate_id(id: &str) -> StoreResult<&str> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(StoreError::InvalidId { id: id.to_owned() });
    }
    Ok(id)
}

/// `<id>.json`.
pub(crate) fn record_name(id: &str) -> String {
    format!("{id}.{RECORD_EXTENSION}")
}

/// `<id>.lock`.
pub(crate) fn lock_name(id: &str) -> String {
    format!("{id}.{LOCK_EXTENSION}")
}

/// Whether `name` exists in `dir` without following a final symlink.
///
/// # Errors
/// `InvalidInput` if `name` is a symbolic link; other I/O errors verbatim.
pub(crate) fn exists_no_symlink(dir: &Dir, name: &str, description: &str) -> io::Result<bool> {
    match dir.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(symlink_error(name, description)),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn symlink_error(name: &str, description: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{description} must not be a symbolic link: {name}"),
    )
}

/// Result of reading one named file.
pub(crate) enum ReadOutcome {
    /// Complete file contents.
    Bytes(Vec<u8>),
    /// Nothing under that name.
    Missing,
    /// Something that is not a regular file (e.g. a directory).
    NotRegular,
}

/// Reads a regular file, rejecting a symlinked final component.
///
/// # Errors
/// `InvalidInput` for a symlink, other I/O errors verbatim.
pub(crate) fn read_regular(dir: &Dir, name: &str, description: &str) -> io::Result<ReadOutcome> {
    if !exists_no_symlink(dir, name, description)? {
        return Ok(ReadOutcome::Missing);
    }
    let mut file = match dir.open(name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(ReadOutcome::Missing),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Ok(ReadOutcome::NotRegular);
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(ReadOutcome::Bytes(bytes))
}

/// Opens (creating if needed) the lock file `name` for read+write with the
/// platform default create mode (`0o666` minus umask, like `std`).
///
/// # Errors
/// `InvalidInput` for a symlinked lock file, other I/O errors verbatim.
pub(crate) fn open_lock_file(dir: &Dir, name: &str) -> io::Result<std::fs::File> {
    exists_no_symlink(dir, name, "record lock")?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    Ok(dir.open_with(name, &options)?.into_std())
}

/// `fsync`s the directory entries of `dir`.
///
/// cap-std opens directory handles with `O_PATH` on Linux, which cannot be
/// synced; a readable handle to `.` is opened beneath `dir` instead.
///
/// # Errors
/// Any open/sync failure is returned, never swallowed.
pub(crate) fn sync_dir(dir: &Dir) -> io::Result<()> {
    dir.open(".")?.sync_all()
}

/// Parses the owning record id out of a temp name `.<id>.<unique>.tmp`.
pub(crate) fn temp_owner(name: &str) -> Option<&str> {
    let inner = name.strip_prefix('.')?.strip_suffix(TEMP_SUFFIX)?;
    let (owner, unique) = inner.split_once('.')?;
    if unique.is_empty() {
        return None;
    }
    validate_id(owner).ok()
}

fn temp_name(owner: &str) -> String {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let stamp = jiff::Timestamp::now().as_nanosecond();
    format!(
        ".{owner}.{}-{stamp}-{counter}{TEMP_SUFFIX}",
        std::process::id()
    )
}

fn create_temp(dir: &Dir, owner: &str) -> io::Result<(String, File)> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(TEMP_MODE);
    let mut last_error = None;
    for _ in 0..TEMP_ATTEMPTS {
        let name = temp_name(owner);
        match dir.open_with(&name, &options) {
            Ok(file) => return Ok((name, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                last_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::other("no free temp file name")))
}

/// Writes `bytes` to a new, synced temp file and returns its name. The temp
/// file is removed again on failure.
fn write_temp(dir: &Dir, owner: &str, bytes: &[u8]) -> io::Result<String> {
    let (name, mut file) = create_temp(dir, owner)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    match written {
        Ok(()) => Ok(name),
        Err(error) => {
            remove_quietly(dir, &name);
            Err(error)
        }
    }
}

fn remove_quietly(dir: &Dir, name: &str) {
    match dir.remove_file(name) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(name, error = %error, "temp file could not be removed"),
    }
}

/// Atomically replaces (or creates) `name` with `bytes`: temp + `sync_all` +
/// rename + directory `fsync`.
///
/// # Errors
/// I/O errors; a symlinked target is rejected with `InvalidInput`.
pub(crate) fn replace_atomic(
    dir: &Dir,
    name: &str,
    owner: &str,
    bytes: &[u8],
    description: &str,
) -> io::Result<()> {
    exists_no_symlink(dir, name, description)?;
    let temp = write_temp(dir, owner, bytes)?;
    if let Err(error) = dir.rename(&temp, dir, name) {
        remove_quietly(dir, &temp);
        return Err(error);
    }
    // The rename replaced a directory entry; without this sync a crash could
    // resurrect the previous record (e.g. an already revoked lease).
    sync_dir(dir)
}

/// Atomically creates `name` with `bytes`, never replacing an existing entry:
/// temp + `sync_all` + hard link + unlink temp + directory `fsync`.
///
/// # Errors
/// [`StoreError::AlreadyExists`] if `name` exists, [`StoreError::Unsupported`]
/// if the filesystem has no hard links, otherwise [`StoreError::Io`].
pub(crate) fn create_noclobber(
    dir: &Dir,
    name: &str,
    owner: &str,
    bytes: &[u8],
) -> StoreResult<()> {
    let temp = write_temp(dir, owner, bytes)?;
    let linked = dir.hard_link(&temp, dir, name);
    remove_quietly(dir, &temp);
    match linked {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return Err(StoreError::AlreadyExists {
                id: owner.to_owned(),
            });
        }
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            return Err(StoreError::Unsupported {
                operation: "no-clobber persist (hard link)",
                detail: error.to_string(),
            });
        }
        Err(error) => return Err(StoreError::Io(error)),
    }
    sync_dir(dir)?;
    Ok(())
}

/// Moves a corrupt regular file aside to `<name>.corrupt-<ts>[-<n>]` (hard
/// link, then unlink, then directory `fsync`). Never overwrites, never
/// follows a symlink. `Ok(None)`: no regular file (any more) under `name`.
///
/// The caller must hold the record lock so no writer replaces the file
/// concurrently.
///
/// # Errors
/// I/O errors; `AlreadyExists` if every candidate name is taken.
pub(crate) fn quarantine(dir: &Dir, name: &str) -> io::Result<Option<String>> {
    match dir.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let stamp = jiff::Timestamp::now().as_nanosecond();
    let mut last_candidate = String::new();
    for attempt in 0..QUARANTINE_ATTEMPTS {
        let candidate = if attempt == 0 {
            format!("{name}.{QUARANTINE_MARKER}-{stamp}")
        } else {
            format!("{name}.{QUARANTINE_MARKER}-{stamp}-{attempt}")
        };
        match dir.hard_link(name, dir, &candidate) {
            Ok(()) => {
                match dir.remove_file(name) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                sync_dir(dir)?;
                return Ok(Some(candidate));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                last_candidate = candidate;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("quarantine name '{last_candidate}' already exists"),
    ))
}

/// Opens a subdirectory, `Ok(None)` if it does not exist.
///
/// # Errors
/// Any other I/O error.
pub(crate) fn open_subdir(dir: &Dir, name: &str) -> io::Result<Option<Dir>> {
    match dir.open_dir(name) {
        Ok(subdir) => Ok(Some(subdir)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Stem of `<stem>.json`, mirroring `Path::extension`/`Path::file_stem`.
pub(crate) fn record_stem(name: &str) -> Option<&str> {
    let path = Path::new(name);
    if path.extension().and_then(|extension| extension.to_str()) != Some(RECORD_EXTENSION) {
        return None;
    }
    path.file_stem().and_then(|stem| stem.to_str())
}

#[cfg(test)]
mod tests {
    use super::{record_stem, temp_owner, validate_id};
    use crate::error::StoreError;

    #[test]
    fn ids_are_single_safe_components() {
        assert!(validate_id("work-1_A").is_ok());
        for bad in ["", "..", "a/b", "a.b", "a b", "ä"] {
            assert!(matches!(
                validate_id(bad),
                Err(StoreError::InvalidId { .. })
            ));
        }
    }

    #[test]
    fn temp_names_parse_back_to_their_owner_and_are_never_records() {
        let name = super::temp_name("work-7");
        assert_eq!(temp_owner(&name), Some("work-7"));
        assert_eq!(record_stem(&name), None);
        assert_eq!(temp_owner(".tmpAbC123"), None);
        assert_eq!(temp_owner("work-7.json"), None);
    }

    #[test]
    fn record_stems_follow_path_extension_rules() {
        assert_eq!(record_stem("work-1.json"), Some("work-1"));
        assert_eq!(record_stem("work-1.json.corrupt-12"), None);
        assert_eq!(record_stem("work-1.lock"), None);
    }
}
