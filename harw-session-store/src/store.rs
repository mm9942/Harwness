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
//!
//! # Integrität (A-STORE: F-068, F-156, F-179, G-020)
//! - **Schreiblimit:** kein Record, dessen JSONL-Zeile (inkl. `\n`) größer als
//!   [`MAX_RECORD_BYTES`] ist, wird geschrieben
//!   ([`SessionStoreError::TranscriptRecordTooLarge`]). Damit ist jede
//!   geschriebene Zeile für [`TranscriptReader`] wieder lesbar.
//! - **Tail-Repair:** eine abgerissene letzte Zeile (Absturz mitten in
//!   `write_all`, nie quittiert) wird **ausschließlich unter dem exklusiven
//!   Transcript-Lock** abgeschnitten — automatisch vor jedem
//!   [`TranscriptStore::append`] und explizit über
//!   [`TranscriptStore::repair_tail`]. Die entfernten Bytes werden vorher
//!   durabel nach `<name>.corrupt-<ts>` gesichert.
//! - **Crate-weite Helfer:** [`persist_noclobber`] (atomar, schlägt fehl, wenn
//!   das Ziel existiert) und `quarantine_file` (defekte Datei per
//!   Hardlink + Unlink nach `<name>.corrupt-<ts>` verschieben, ohne ein
//!   vorhandenes Ziel zu überschreiben) werden von `job_store`, `child_lease`
//!   und `freeze` mitbenutzt.
//!
//! # Nebenläufigkeit
//! `TranscriptStore` hat keinen inneren Zustand (`Send + Sync`). Konkurrierende
//! Schreiber bekommen [`SessionStoreError::LockContended`] statt zu warten.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_session_store::store::TailRepair;
//! use harw_session_store::TranscriptStore;
//! use harw_types::SessionId;
//!
//! # fn demo() -> harw_session_store::SessionStoreResult<()> {
//! let store = TranscriptStore::new(std::path::Path::new("/tmp/transcripts"));
//! let session = SessionId::from_str("session-a");
//! if let TailRepair::Repaired { quarantine, .. } = store.repair_tail(&session)? {
//!     assert!(quarantine.exists());
//! }
//! let _records = store.reader(&session)?;
//! # Ok(())
//! # }
//! ```

use std::fs::File;
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs4::FileExt;
use harw_fsutil::OpenMode;
use harw_types::SessionId;
use tempfile::NamedTempFile;

use crate::error::{SessionStoreError, SessionStoreResult};
pub use crate::reader::MAX_RECORD_BYTES;
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

/// Blockgröße, in der die Tail-Suche rückwärts nach dem letzten `\n` liest.
const TAIL_SCAN_CHUNK: u64 = 64 * 1024;

/// Namensbestandteil zwischen Originalname und Zeitstempel einer Quarantäne-Datei.
const QUARANTINE_MARKER: &str = "corrupt";

/// Wie viele Namenskandidaten (`-<n>`-Suffix) eine Quarantäne höchstens probiert.
const QUARANTINE_ATTEMPTS: u32 = 16;

/// Ergebnis einer Tail-Prüfung eines Transcripts.
///
/// # Description
/// [`TailRepair::Clean`]: die Datei ist leer oder endet auf `\n`; nichts
/// wurde verändert. [`TailRepair::Repaired`]: eine abgerissene letzte Zeile
/// wurde nach `quarantine` gesichert und aus dem Transcript abgeschnitten.
///
/// # Concurrency
/// Reiner Wert (`Send + Sync`).
///
/// # Examples
/// ```rust
/// use harw_session_store::store::TailRepair;
/// assert_eq!(TailRepair::Clean, TailRepair::Clean);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TailRepair {
    /// Kein abgerissener Tail vorhanden.
    Clean,
    /// Ein abgerissener Tail wurde entfernt.
    Repaired {
        /// Anzahl der abgeschnittenen Bytes.
        removed_bytes: u64,
        /// Pfad der durablen Sicherung der abgeschnittenen Bytes.
        quarantine: PathBuf,
    },
}

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
    /// The writer holds an advisory exclusive lock through tail repair,
    /// `write_all` and `sync_data`, followed by a parent-directory sync,
    /// preventing interleaved JSONL records from cooperating processes.
    ///
    /// # Description
    /// A-STORE (F-068/F-156): the encoded line (including its `\n`) must not
    /// exceed [`MAX_RECORD_BYTES`]. Under the lock, a torn final line left by
    /// an earlier crash is quarantined and cut off first, so the new record
    /// never fuses with a broken one.
    ///
    /// # Errors
    /// - [`SessionStoreError::TranscriptRecordTooLarge`]: record over the limit
    ///   (nothing is written, no lock is taken).
    /// - [`SessionStoreError::LockContended`]: another writer holds the lock.
    /// - [`SessionStoreError::UnsafeTranscriptPath`], [`SessionStoreError::Io`],
    ///   [`SessionStoreError::Serde`].
    ///
    /// # Concurrency
    /// Exclusive `fs4` try-lock on `<id>.jsonl.lock`; never blocks.
    pub fn append(&self, record: &TranscriptRecord) -> SessionStoreResult<()> {
        let path = self.transcript_path(&record.session_id)?;
        let line = record.to_jsonl_line()?;
        ensure_record_within_limit(&record.session_id, &line)?;
        let parent = transcript_parent(&path)?;
        std::fs::create_dir_all(parent).map_err(SessionStoreError::Io)?;
        let lock = lock_transcript(&path, &record.session_id)?;

        let write_result = (|| -> SessionStoreResult<()> {
            reject_symlink(&path, "transcript")?;
            let mut file = open_transcript_for_append(&path)?;
            repair_open_tail(&mut file, &path)?;
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

    /// Cuts off a torn final JSONL line under the exclusive transcript lock.
    ///
    /// # Description
    /// A-STORE (F-068/F-156): a crash in the middle of an append can leave a
    /// final line without `\n`. Such a line was never acknowledged (append
    /// returns only after `sync_data`), so dropping it loses no durable
    /// record. The removed bytes are first persisted no-clobber to
    /// `<id>.jsonl.corrupt-<ts>`; then the transcript is truncated to the end
    /// of its last complete line and synced. Call this before
    /// [`TranscriptStore::reader`] when hydrating a session.
    ///
    /// # Arguments
    /// - `session_id` (`&SessionId`): the session whose transcript is checked.
    ///
    /// # Returns
    /// [`TailRepair::Clean`] if nothing had to change, otherwise
    /// [`TailRepair::Repaired`] with the removed byte count and quarantine path.
    ///
    /// # Errors
    /// - [`SessionStoreError::NotFound`]: no transcript exists.
    /// - [`SessionStoreError::LockContended`]: another writer or repairer holds
    ///   the lock — the repair is **not** attempted without the lock.
    /// - [`SessionStoreError::UnsafeTranscriptPath`], [`SessionStoreError::Io`]
    ///   (including symlinked transcript or lock paths).
    ///
    /// # Concurrency
    /// Exclusive `fs4` try-lock, the same lock `append`/`rewrite` use; of
    /// several concurrent callers at most one repairs, the others observe
    /// `LockContended` or, afterwards, `Clean`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # use harw_session_store::TranscriptStore;
    /// # use harw_types::SessionId;
    /// # fn demo(store: &TranscriptStore) -> harw_session_store::SessionStoreResult<()> {
    /// let outcome = store.repair_tail(&SessionId::from_str("session-a"))?;
    /// # let _ = outcome;
    /// # Ok(())
    /// # }
    /// ```
    pub fn repair_tail(&self, session_id: &SessionId) -> SessionStoreResult<TailRepair> {
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
        let lock = lock_transcript(&path, session_id)?;
        let repair_result = (|| -> SessionStoreResult<TailRepair> {
            reject_symlink(&path, "transcript")?;
            let mut file = match open_without_following_symlinks(OpenMode::read_write(), &path) {
                Ok(file) => file,
                Err(SessionStoreError::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound =>
                {
                    return Err(SessionStoreError::NotFound {
                        session: session_id.clone(),
                    });
                }
                Err(error) => return Err(error),
            };
            repair_open_tail(&mut file, &path)
        })();
        let unlock_result = FileExt::unlock(&lock).map_err(SessionStoreError::Io);
        let outcome = repair_result?;
        unlock_result?;
        Ok(outcome)
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
            let line = record.to_jsonl_line()?;
            ensure_record_within_limit(session_id, &line)?;
            buf.push_str(&line);
        }
        let parent = transcript_parent(&path)?;
        std::fs::create_dir_all(parent).map_err(SessionStoreError::Io)?;
        let lock = lock_transcript(&path, session_id)?;

        let rewrite_result = (|| -> SessionStoreResult<()> {
            reject_symlink(&path, "transcript")?;
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

/// Öffnet das Transcript lesend + anhängend: Lesen braucht die Tail-Prüfung,
/// `O_APPEND` garantiert, dass jeder Schreibvorgang am (ggf. gekürzten) Ende landet.
fn open_transcript_for_append(path: &Path) -> SessionStoreResult<File> {
    open_without_following_symlinks(
        OpenMode {
            read: true,
            ..OpenMode::append_create(DEFAULT_CREATE_MODE)
        },
        path,
    )
}

// Lehnt eine JSONL-Zeile ab, die das Leselimit des Readers überschreiten würde.
fn ensure_record_within_limit(session_id: &SessionId, line: &str) -> SessionStoreResult<()> {
    if line.len() > MAX_RECORD_BYTES {
        return Err(SessionStoreError::TranscriptRecordTooLarge {
            session: session_id.clone(),
            size: line.len(),
            limit: MAX_RECORD_BYTES,
        });
    }
    Ok(())
}

// Kern der Tail-Reparatur. Vorbedingung: der Aufrufer hält den exklusiven
// Transcript-Lock und `file` ist lesbar und schreibbar.
fn repair_open_tail(file: &mut File, path: &Path) -> SessionStoreResult<TailRepair> {
    let len = file.metadata().map_err(SessionStoreError::Io)?.len();
    if len == 0 {
        return Ok(TailRepair::Clean);
    }
    let mut last = [0_u8; 1];
    file.seek(SeekFrom::Start(len - 1))
        .map_err(SessionStoreError::Io)?;
    file.read_exact(&mut last).map_err(SessionStoreError::Io)?;
    if last[0] == b'\n' {
        return Ok(TailRepair::Clean);
    }

    let keep = last_line_end(file, len)?;
    let removed_len = len - keep;
    let capacity = usize::try_from(removed_len).map_err(|_| {
        SessionStoreError::Io(std::io::Error::other(
            "torn transcript tail exceeds the addressable size",
        ))
    })?;
    let mut removed = Vec::with_capacity(capacity);
    file.seek(SeekFrom::Start(keep))
        .map_err(SessionStoreError::Io)?;
    Read::by_ref(file)
        .take(removed_len)
        .read_to_end(&mut removed)
        .map_err(SessionStoreError::Io)?;

    // Erst sichern, dann kürzen: ein Absturz dazwischen verliert nichts.
    let quarantine = quarantine_bytes(path, &removed)?;
    file.set_len(keep).map_err(SessionStoreError::Io)?;
    file.sync_all().map_err(SessionStoreError::Io)?;
    tracing::warn!(
        path = %path.display(),
        removed_bytes = removed_len,
        quarantine = %quarantine.display(),
        "torn transcript tail repaired under exclusive lock"
    );
    Ok(TailRepair::Repaired {
        removed_bytes: removed_len,
        quarantine,
    })
}

// Liefert den Offset direkt hinter dem letzten `\n` (0, wenn keines existiert).
fn last_line_end(file: &mut File, len: u64) -> SessionStoreResult<u64> {
    let mut buffer = Vec::new();
    let mut end = len;
    while end > 0 {
        let start = end.saturating_sub(TAIL_SCAN_CHUNK);
        let size = usize::try_from(end - start).map_err(|_| {
            SessionStoreError::Io(std::io::Error::other("tail scan chunk exceeds usize"))
        })?;
        buffer.resize(size, 0);
        file.seek(SeekFrom::Start(start))
            .map_err(SessionStoreError::Io)?;
        file.read_exact(&mut buffer)
            .map_err(SessionStoreError::Io)?;
        if let Some(index) = buffer.iter().rposition(|byte| *byte == b'\n') {
            let index = u64::try_from(index).map_err(|_| {
                SessionStoreError::Io(std::io::Error::other("tail scan index exceeds u64"))
            })?;
            return Ok(start + index + 1);
        }
        end = start;
    }
    Ok(0)
}

/// Schreibt `bytes` atomar nach `path` und schlägt fehl, wenn `path` existiert.
///
/// # Description
/// A-STORE (F-179): Inhalt geht in eine Geschwister-`tempfile`, wird mit
/// `sync_all` gesichert und per `NamedTempFile::persist_noclobber`
/// (`renameat2(RENAME_NOREPLACE)`, sonst `link`+`unlink`) eingehängt; danach
/// wird das Elternverzeichnis gesynct. Ein Leser sieht nie eine leere oder
/// halbe Zieldatei, und ein vorhandenes Ziel (auch ein Symlink) wird nie
/// überschrieben.
///
/// # Arguments
/// - `path` (`&Path`): Zieldatei; ihr Elternverzeichnis muss existieren.
/// - `bytes` (`&[u8]`): vollständiger Dateiinhalt.
///
/// # Returns
/// `Ok(())`, sobald Datei und Verzeichniseintrag durabel sind.
///
/// # Errors
/// - [`SessionStoreError::PersistTargetExists`]: am Ziel liegt bereits ein
///   Eintrag (die temporäre Datei wird verworfen).
/// - [`SessionStoreError::Io`]: Anlegen, Schreiben, Sync oder Persist schlug fehl.
///
/// # Concurrency
/// Von beliebig vielen Threads/Prozessen aufrufbar; von mehreren Aufrufern
/// für dasselbe Ziel gewinnt genau einer.
///
/// # Examples
/// ```rust,no_run
/// use harw_session_store::store::persist_noclobber;
/// # fn demo() -> harw_session_store::SessionStoreResult<()> {
/// persist_noclobber(std::path::Path::new("/tmp/record.json"), b"{}")?;
/// # Ok(())
/// # }
/// ```
pub fn persist_noclobber(path: &Path, bytes: &[u8]) -> SessionStoreResult<()> {
    let parent = path.parent().ok_or_else(|| {
        SessionStoreError::Io(std::io::Error::other("store file path has no parent"))
    })?;
    let mut temp = NamedTempFile::new_in(parent).map_err(SessionStoreError::Io)?;
    temp.write_all(bytes).map_err(SessionStoreError::Io)?;
    temp.as_file().sync_all().map_err(SessionStoreError::Io)?;
    temp.persist_noclobber(path).map_err(|error| {
        if error.error.kind() == std::io::ErrorKind::AlreadyExists {
            SessionStoreError::PersistTargetExists {
                path: path.display().to_string(),
            }
        } else {
            SessionStoreError::Io(error.error)
        }
    })?;
    sync_parent_directory(parent)
}

// Sichert entfernte Bytes unter `<name>.corrupt-<ts>[-<n>]` neben `path`.
fn quarantine_bytes(path: &Path, bytes: &[u8]) -> SessionStoreResult<PathBuf> {
    let (parent, file_name) = quarantine_parts(path)?;
    let stamp = jiff::Timestamp::now().as_nanosecond();
    let mut last_candidate = PathBuf::new();
    for attempt in 0..QUARANTINE_ATTEMPTS {
        let candidate = parent.join(quarantine_name(file_name, stamp, attempt));
        match persist_noclobber(&candidate, bytes) {
            Ok(()) => return Ok(candidate),
            Err(SessionStoreError::PersistTargetExists { .. }) => last_candidate = candidate,
            Err(error) => return Err(error),
        }
    }
    Err(SessionStoreError::PersistTargetExists {
        path: last_candidate.display().to_string(),
    })
}

/// Verschiebt eine defekte reguläre Datei nach `<name>.corrupt-<ts>[-<n>]`.
///
/// Crate-intern (A-STORE, F-179/G-020): statt einen ganzen Store-Scan
/// abzubrechen, wird die Datei beiseitegelegt. Der neue Name entsteht per
/// `hard_link` (schlägt fehl, wenn er existiert — nie ein Überschreiben),
/// danach wird der alte Name entfernt und das Verzeichnis gesynct. Kein
/// Symlink wird verschoben oder verfolgt. `Ok(None)`: am Pfad liegt (nicht
/// mehr) eine reguläre Datei. Der Aufrufer muss sicherstellen, dass kein
/// Schreiber die Datei zwischenzeitlich atomar ersetzt (Record-Lock bzw.
/// No-Clobber-Schreibdisziplin).
pub(crate) fn quarantine_file(path: &Path) -> SessionStoreResult<Option<PathBuf>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SessionStoreError::Io(error)),
    }
    let (parent, file_name) = quarantine_parts(path)?;
    let stamp = jiff::Timestamp::now().as_nanosecond();
    let mut last_candidate = PathBuf::new();
    for attempt in 0..QUARANTINE_ATTEMPTS {
        let candidate = parent.join(quarantine_name(file_name, stamp, attempt));
        match std::fs::hard_link(path, &candidate) {
            Ok(()) => {
                match std::fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(SessionStoreError::Io(error)),
                }
                sync_parent_directory(parent)?;
                return Ok(Some(candidate));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                last_candidate = candidate;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(SessionStoreError::Io(error)),
        }
    }
    Err(SessionStoreError::PersistTargetExists {
        path: last_candidate.display().to_string(),
    })
}

fn quarantine_parts(path: &Path) -> SessionStoreResult<(&Path, &str)> {
    let parent = path.parent().ok_or_else(|| {
        SessionStoreError::Io(std::io::Error::other("quarantine path has no parent"))
    })?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            SessionStoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "quarantine path has no UTF-8 file name",
            ))
        })?;
    Ok((parent, file_name))
}

fn quarantine_name(file_name: &str, stamp: i128, attempt: u32) -> String {
    if attempt == 0 {
        format!("{file_name}.{QUARANTINE_MARKER}-{stamp}")
    } else {
        format!("{file_name}.{QUARANTINE_MARKER}-{stamp}-{attempt}")
    }
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
    use crate::test_support::{TestError, TestResult};
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
    fn append_is_durable_and_replayable() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        store.append(&record("session-a", 0))?;
        store.append(&record("session-a", 1))?;

        let replayed = store
            .reader(&SessionId::from_str("session-a"))?
            .collect::<SessionStoreResult<Vec<_>>>()?;
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[1].sequence, 1);
        Ok(())
    }

    #[test]
    fn rewrite_replaces_a_session_atomically() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        store.append(&record("session-a", 0))?;
        store.rewrite(&SessionId::from_str("session-a"), &[record("session-a", 9)])?;

        let replayed = store
            .reader(&SessionId::from_str("session-a"))?
            .collect::<SessionStoreResult<Vec<_>>>()?;
        assert_eq!(
            replayed
                .iter()
                .map(|entry| entry.sequence)
                .collect::<Vec<_>>(),
            vec![9]
        );
        Ok(())
    }

    #[test]
    fn rewrite_rejects_cross_session_records() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        let Err(error) =
            store.rewrite(&SessionId::from_str("session-a"), &[record("session-b", 0)])
        else {
            return Err(TestError::Unexpected(
                "expected a cross-session rewrite to be rejected".to_owned(),
            ));
        };
        assert!(matches!(error, SessionStoreError::SessionMismatch { .. }));
        Ok(())
    }

    #[test]
    fn rewrite_respects_the_same_lock_as_append() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        let session = SessionId::from_str("session-a");
        let path = store.transcript_path(&session)?;
        let lock = lock_transcript(&path, &session)?;

        let Err(error) = store.rewrite(&session, &[record("session-a", 0)]) else {
            return Err(TestError::Unexpected(
                "expected rewrite to observe the append lock".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            SessionStoreError::LockContended { session: contended } if contended == session
        ));
        FileExt::unlock(&lock)?;
        Ok(())
    }

    #[test]
    fn transcript_path_keeps_safe_ids_under_the_store_root() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());

        let path = store.transcript_path(&SessionId::from_str("session_123-ABC"))?;

        assert_eq!(path, temp.path().join("session_123-ABC.jsonl"));
        assert!(path.starts_with(temp.path()));
        Ok(())
    }

    #[test]
    fn transcript_path_accepts_exact_legacy_tui_id() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        let session = "local-tui:0123456789abcdef";

        let path = store.transcript_path(&SessionId::from_str(session))?;

        assert_eq!(path, temp.path().join(format!("{session}.jsonl")));
        assert!(path.starts_with(temp.path()));
        Ok(())
    }

    #[test]
    fn legacy_tui_transcript_is_replayable() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        let session = "local-tui:0123456789abcdef";
        store.append(&record(session, 0))?;

        let replayed = store
            .reader(&SessionId::from_str(session))?
            .collect::<SessionStoreResult<Vec<_>>>()?;

        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].session_id.as_str(), session);
        Ok(())
    }

    #[test]
    fn transcript_path_rejects_unsafe_session_ids() -> TestResult {
        let temp = tempfile::tempdir()?;
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
        Ok(())
    }

    #[test]
    fn transcript_operations_fail_closed_for_unsafe_session_ids() -> TestResult {
        let temp = tempfile::tempdir()?;
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
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn transcript_symlink_is_rejected_for_append_rewrite_and_replay() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        let session = SessionId::from_str("session-a");
        let transcript_path = store.transcript_path(&session)?;
        let target = temp.path().join("outside-transcript.jsonl");
        std::fs::write(&target, "outside transcript")?;
        symlink(&target, &transcript_path)?;

        assert_symlink_rejected(store.append(&record("session-a", 0)))?;
        assert_symlink_rejected(store.rewrite(&session, &[record("session-a", 1)]))?;
        assert_symlink_rejected(store.reader(&session))?;
        assert_eq!(std::fs::read_to_string(target)?, "outside transcript");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn transcript_lock_symlink_is_rejected_for_append_and_rewrite() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        let session = SessionId::from_str("session-a");
        let transcript_path = store.transcript_path(&session)?;
        let lock_path = transcript_path.with_extension(format!("{TRANSCRIPT_EXT}.lock"));
        let target = temp.path().join("outside-transcript.lock");
        std::fs::write(&target, "outside lock")?;
        symlink(&target, &lock_path)?;

        assert_symlink_rejected(store.append(&record("session-a", 0)))?;
        assert_symlink_rejected(store.rewrite(&session, &[record("session-a", 1)]))?;
        assert_eq!(std::fs::read_to_string(target)?, "outside lock");
        assert!(!transcript_path.exists());
        Ok(())
    }

    #[cfg(unix)]
    fn assert_symlink_rejected<T>(result: SessionStoreResult<T>) -> TestResult {
        match result {
            Err(SessionStoreError::Io(error)) => {
                assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
                Ok(())
            }
            Err(error) => Err(TestError::Unexpected(format!(
                "expected a symlink rejection, got {error:?}"
            ))),
            Ok(_) => Err(TestError::Unexpected(
                "expected a symlink rejection".to_owned(),
            )),
        }
    }
}
