//! Append-only transcript store: path derivation, durable append, replay open.
//!
//! Spec: `docs/design/knowledge-surfaces.md` §1.4 + the `harw-session-store`
//! row of `docs/design/crates-inventory.md` (hand-rolled JSONL append +
//! `fs4` advisory lock + `tempfile` `persist()` atomic rename).
//!
//! Every write uses an `fs4` advisory exclusive sidecar lock. Appends sync
//! both the transcript and its parent directory before releasing the lock;
//! compaction writes to a sibling `tempfile`, atomically persists it over the
//! live file, then syncs that parent directory.

use std::fs::File;
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use fs4::FileExt;
use harw_fsutil::OpenMode;
use harw_types::SessionId;
use tempfile::NamedTempFile;

use crate::error::{SessionStoreError, SessionStoreResult};
use crate::reader::TranscriptReader;
use crate::record::TranscriptRecord;

/// File extension used for every session transcript.
const TRANSCRIPT_EXT: &str = "jsonl";

/// Historical TUI session IDs persisted before transcript names were restricted.
const LEGACY_TUI_ID_PREFIX: &str = "local-tui:";
const LEGACY_TUI_ID_HEX_LEN: usize = 16;

/// Default create-mode for `create`-Aufrufe, identisch zum bisherigen Verhalten:
/// `std::fs::OpenOptions` legt ohne `.mode(...)` mit `0o666` (abzüglich `umask`) an.
const DEFAULT_CREATE_MODE: u32 = 0o666;

/// Root-anchored, append-only transcript store keyed by `SessionId`.
pub struct TranscriptStore {
    /// Directory under which per-session `<session_id>.jsonl` files live.
    root: PathBuf,
}

impl TranscriptStore {
    /// Creates a store rooted at `root` (the directory is not created eagerly).
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// Borrows the store's root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Derives the transcript file path for a session (`<root>/<id>.jsonl`).
    pub fn transcript_path(&self, session_id: &SessionId) -> SessionStoreResult<PathBuf> {
        Ok(self
            .root
            .join(safe_session_component(session_id)?)
            .with_extension(TRANSCRIPT_EXT))
    }

    /// Appends one record durably to its session transcript.
    ///
    /// The writer holds an advisory exclusive lock through `write_all` and
    /// `sync_data`, followed by a parent-directory sync, preventing
    /// interleaved JSONL records from cooperating processes.
    pub fn append(&self, record: &TranscriptRecord) -> SessionStoreResult<()> {
        let path = self.transcript_path(&record.session_id)?;
        let line = record.to_jsonl_line()?;
        let parent = transcript_parent(&path)?;
        std::fs::create_dir_all(parent).map_err(SessionStoreError::Io)?;
        let lock = lock_transcript(&path, &record.session_id)?;
        reject_symlink(&path, "transcript")?;
        let mut file = open_transcript_for_append(&path)?;

        let write_result = (|| -> SessionStoreResult<()> {
            file.write_all(line.as_bytes())
                .map_err(SessionStoreError::Io)?;
            file.sync_data().map_err(SessionStoreError::Io)?;
            sync_parent_directory(parent)?;
            Ok(())
        })();
        let unlock_result = FileExt::unlock(&lock).map_err(SessionStoreError::Io);
        write_result?;
        unlock_result
    }

    /// Atomically rewrites a whole session transcript (compaction/retention).
    ///
    /// Each record must belong to `session_id`; the replacement only becomes
    /// visible after the synced sibling temp file is atomically persisted.
    pub fn rewrite(
        &self,
        session_id: &SessionId,
        records: &[TranscriptRecord],
    ) -> SessionStoreResult<()> {
        let path = self.transcript_path(session_id)?;
        let mut buf = String::new();
        for record in records {
            if record.session_id != *session_id {
                return Err(SessionStoreError::SessionMismatch {
                    expected: session_id.clone(),
                    actual: record.session_id.clone(),
                });
            }
            buf.push_str(&record.to_jsonl_line()?);
        }
        let parent = transcript_parent(&path)?;
        std::fs::create_dir_all(parent).map_err(SessionStoreError::Io)?;
        let lock = lock_transcript(&path, session_id)?;
        reject_symlink(&path, "transcript")?;

        let rewrite_result = (|| -> SessionStoreResult<()> {
            let mut temp = NamedTempFile::new_in(parent).map_err(SessionStoreError::Io)?;
            temp.write_all(buf.as_bytes())
                .map_err(SessionStoreError::Io)?;
            temp.as_file().sync_all().map_err(SessionStoreError::Io)?;
            temp.persist(&path)
                .map_err(|error| SessionStoreError::Io(error.error))?;
            sync_parent_directory(parent)?;
            Ok(())
        })();
        let unlock_result = FileExt::unlock(&lock).map_err(SessionStoreError::Io);
        rewrite_result?;
        unlock_result
    }

    /// Opens a session transcript for sequential replay.
    pub fn reader(&self, session_id: &SessionId) -> SessionStoreResult<TranscriptReader> {
        let path = self.transcript_path(session_id)?;
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(symlink_error(&path, "transcript"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(SessionStoreError::NotFound {
                    session: session_id.clone(),
                });
            }
            Err(error) => return Err(SessionStoreError::Io(error)),
        }
        TranscriptReader::open(&path)
    }
}

fn safe_session_component(session_id: &SessionId) -> SessionStoreResult<&str> {
    let value = session_id.as_str();
    let is_safe_current_id = !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    let is_legacy_tui_id = value
        .strip_prefix(LEGACY_TUI_ID_PREFIX)
        .is_some_and(|suffix| {
            suffix.len() == LEGACY_TUI_ID_HEX_LEN
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        });

    if !is_safe_current_id && !is_legacy_tui_id {
        return Err(SessionStoreError::UnsafeTranscriptPath(value.to_owned()));
    }
    Ok(value)
}

fn transcript_parent(path: &Path) -> SessionStoreResult<&Path> {
    path.parent().ok_or_else(|| {
        SessionStoreError::Io(std::io::Error::other("transcript path has no parent"))
    })
}

fn lock_transcript(path: &Path, session_id: &SessionId) -> SessionStoreResult<File> {
    let lock_path = path.with_extension(format!("{TRANSCRIPT_EXT}.lock"));
    reject_symlink(&lock_path, "transcript lock")?;
    let lock = open_lock_file(&lock_path)?;
    FileExt::try_lock(&lock).map_err(|error| match error {
        fs4::TryLockError::WouldBlock => SessionStoreError::LockContended {
            session: session_id.clone(),
        },
        fs4::TryLockError::Error(error) => SessionStoreError::Io(error),
    })?;
    Ok(lock)
}

fn open_transcript_for_append(path: &Path) -> SessionStoreResult<File> {
    open_without_following_symlinks(OpenMode::append_create(DEFAULT_CREATE_MODE), path)
}

fn open_lock_file(path: &Path) -> SessionStoreResult<File> {
    open_without_following_symlinks(
        OpenMode {
            read: false,
            write: true,
            create: true,
            create_new: false,
            truncate: false,
            append: false,
            mode: DEFAULT_CREATE_MODE,
        },
        path,
    )
}

/// Öffnet `path` ohne dem letzten Pfadglied als Symlink zu folgen
/// (F-006: das architekturabhängig falsche `O_NOFOLLOW` wurde durch
/// `harw_fsutil::open_nofollow` ersetzt, das die Konstante über
/// `rustix::fs::OFlags::NOFOLLOW` plattformkorrekt bezieht).
fn open_without_following_symlinks(mode: OpenMode, path: &Path) -> SessionStoreResult<File> {
    #[cfg(unix)]
    {
        harw_fsutil::open_nofollow(path, mode).map_err(SessionStoreError::Io)
    }
    #[cfg(not(unix))]
    {
        let mut options = OpenOptions::new();
        options
            .read(mode.read)
            .write(mode.write || mode.append)
            .create(mode.create)
            .create_new(mode.create_new)
            .truncate(mode.truncate)
            .append(mode.append);
        options.open(path).map_err(SessionStoreError::Io)
    }
}

fn reject_symlink(path: &Path, description: &str) -> SessionStoreResult<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(symlink_error(path, description)),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SessionStoreError::Io(error)),
    }
}

fn symlink_error(path: &Path, description: &str) -> SessionStoreError {
    SessionStoreError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "{description} must not be a symbolic link: {}",
            path.display()
        ),
    ))
}

// Die eine Fassung liegt in `crate::durability`; sie stand vorher in fünf
// Dateien byte-gleich. Warum ein Eltern-fsync nötig ist, steht dort.
use crate::durability::sync_parent_directory;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::RecordKind;
    use harw_types::ThreadRef;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn record(session: &str, sequence: u64) -> TranscriptRecord {
        TranscriptRecord::new(
            SessionId::from_str(session),
            ThreadRef::from_str("root"),
            sequence,
            jiff::Timestamp::now(),
            RecordKind::Turn,
            serde_json::json!({ "sequence": sequence }),
        )
    }

    #[test]
    fn append_is_durable_and_replayable() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path());
        store.append(&record("session-a", 0)).unwrap();
        store.append(&record("session-a", 1)).unwrap();

        let replayed = store
            .reader(&SessionId::from_str("session-a"))
            .unwrap()
            .collect::<SessionStoreResult<Vec<_>>>()
            .unwrap();
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[1].sequence, 1);
    }

    #[test]
    fn rewrite_replaces_a_session_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path());
        store.append(&record("session-a", 0)).unwrap();
        store
            .rewrite(&SessionId::from_str("session-a"), &[record("session-a", 9)])
            .unwrap();

        let replayed = store
            .reader(&SessionId::from_str("session-a"))
            .unwrap()
            .collect::<SessionStoreResult<Vec<_>>>()
            .unwrap();
        assert_eq!(
            replayed
                .iter()
                .map(|entry| entry.sequence)
                .collect::<Vec<_>>(),
            vec![9]
        );
    }

    #[test]
    fn rewrite_rejects_cross_session_records() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path());
        let error = store
            .rewrite(&SessionId::from_str("session-a"), &[record("session-b", 0)])
            .unwrap_err();
        assert!(matches!(error, SessionStoreError::SessionMismatch { .. }));
    }

    #[test]
    fn rewrite_respects_the_same_lock_as_append() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path());
        let session = SessionId::from_str("session-a");
        let path = store.transcript_path(&session).unwrap();
        let lock = lock_transcript(&path, &session).unwrap();

        let error = store
            .rewrite(&session, &[record("session-a", 0)])
            .unwrap_err();

        assert!(matches!(
            error,
            SessionStoreError::LockContended { session: contended } if contended == session
        ));
        FileExt::unlock(&lock).unwrap();
    }

    #[test]
    fn transcript_path_keeps_safe_ids_under_the_store_root() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path());

        let path = store
            .transcript_path(&SessionId::from_str("session_123-ABC"))
            .unwrap();

        assert_eq!(path, temp.path().join("session_123-ABC.jsonl"));
        assert!(path.starts_with(temp.path()));
    }

    #[test]
    fn transcript_path_accepts_exact_legacy_tui_id() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path());
        let session = "local-tui:0123456789abcdef";

        let path = store
            .transcript_path(&SessionId::from_str(session))
            .unwrap();

        assert_eq!(path, temp.path().join(format!("{session}.jsonl")));
        assert!(path.starts_with(temp.path()));
    }

    #[test]
    fn legacy_tui_transcript_is_replayable() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path());
        let session = "local-tui:0123456789abcdef";
        store.append(&record(session, 0)).unwrap();

        let replayed = store
            .reader(&SessionId::from_str(session))
            .unwrap()
            .collect::<SessionStoreResult<Vec<_>>>()
            .unwrap();

        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].session_id.as_str(), session);
    }

    #[test]
    fn transcript_path_rejects_unsafe_session_ids() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path());

        for session in [
            "",
            "../escape",
            "nested/session",
            "nested\\session",
            ".",
            "a.b",
            "a\n",
            "local-tui:0123456789abcde",
            "local-tui:0123456789abcdef0",
            "local-tui:0123456789abcdeF",
            "local-tui:0123456789abcdeg",
            "other-tui:0123456789abcdef",
            "safe:id",
            "local-tui:0123456789abcdef/escape",
        ] {
            assert!(matches!(
                store.transcript_path(&SessionId::from_str(session)),
                Err(SessionStoreError::UnsafeTranscriptPath(value)) if value == session
            ));
        }
    }

    #[test]
    fn transcript_operations_fail_closed_for_unsafe_session_ids() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path());
        let unsafe_session = SessionId::from_str("../escape");

        assert!(matches!(
            store.append(&record("../escape", 0)),
            Err(SessionStoreError::UnsafeTranscriptPath(value)) if value == "../escape"
        ));
        assert!(matches!(
            store.rewrite(&unsafe_session, &[]),
            Err(SessionStoreError::UnsafeTranscriptPath(value)) if value == "../escape"
        ));
        assert!(matches!(
            store.reader(&unsafe_session),
            Err(SessionStoreError::UnsafeTranscriptPath(value)) if value == "../escape"
        ));
        assert!(!temp.path().join("escape.jsonl").exists());
    }

    #[cfg(unix)]
    #[test]
    fn transcript_symlink_is_rejected_for_append_rewrite_and_replay() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path());
        let session = SessionId::from_str("session-a");
        let transcript_path = store.transcript_path(&session).unwrap();
        let target = temp.path().join("outside-transcript.jsonl");
        std::fs::write(&target, "outside transcript").unwrap();
        symlink(&target, &transcript_path).unwrap();

        assert_symlink_rejected(store.append(&record("session-a", 0)));
        assert_symlink_rejected(store.rewrite(&session, &[record("session-a", 1)]));
        assert_symlink_rejected(store.reader(&session));
        assert_eq!(
            std::fs::read_to_string(target).unwrap(),
            "outside transcript"
        );
    }

    #[cfg(unix)]
    #[test]
    fn transcript_lock_symlink_is_rejected_for_append_and_rewrite() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path());
        let session = SessionId::from_str("session-a");
        let transcript_path = store.transcript_path(&session).unwrap();
        let lock_path = transcript_path.with_extension(format!("{TRANSCRIPT_EXT}.lock"));
        let target = temp.path().join("outside-transcript.lock");
        std::fs::write(&target, "outside lock").unwrap();
        symlink(&target, &lock_path).unwrap();

        assert_symlink_rejected(store.append(&record("session-a", 0)));
        assert_symlink_rejected(store.rewrite(&session, &[record("session-a", 1)]));
        assert_eq!(std::fs::read_to_string(target).unwrap(), "outside lock");
        assert!(!transcript_path.exists());
    }

    #[cfg(unix)]
    fn assert_symlink_rejected<T>(result: SessionStoreResult<T>) {
        match result {
            Err(SessionStoreError::Io(error)) => {
                assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            }
            Err(error) => panic!("expected a symlink rejection, got {error:?}"),
            Ok(_) => panic!("expected a symlink rejection"),
        }
    }
}
