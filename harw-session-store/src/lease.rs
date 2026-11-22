//! Besitz-Sperre je Session (Plan §3.5).
//!
//! # Verantwortungsbereich
//! Eine Session hat zu jeder Zeit höchstens **einen schreibenden Prozess**
//! (TUI, Gateway oder Job). Die Sperre ist eine Datei `<root>/<id>.lease`
//! neben dem Transcript, gehalten über einen exklusiven `fs4`-Lock
//! (`flock`). Der Lock ist die Wahrheit; der JSON-Inhalt der Datei
//! (`{"holder":"tui","pid":…,"since":…}`) ist nur die Auskunft, **wer** hält.
//!
//! # Prozess-Registry
//! `flock` gilt je offenem Datei-Deskriptor, nicht je Prozess. Damit sich ein
//! Prozess nicht selbst aussperrt, führt dieses Modul eine prozessweite
//! Registry `Pfad -> Weak<LeaseInner>`: ein zweites [`SessionLease::acquire`]
//! im selben Prozess bekommt einen Klon des vorhandenen Handles, egal mit
//! welcher [`HolderKind`]. Das letzte gedroppte Handle leert die Datei und
//! gibt den Lock frei.
//!
//! # Absturz
//! Stirbt der haltende Prozess, schließt der Kernel den Deskriptor und gibt
//! den Lock frei. Die Datei behält dann ihren alten Inhalt; [`peek_holder`]
//! meldet trotzdem `None`, weil es den Lock prüft, nicht den Inhalt.
//!
//! # Lock-Reihenfolge
//! `SessionLease` vor Transcript-Lock (`<id>.jsonl.lock`). Die Meta-Sperre ist
//! ein Blatt und darf unter einer Lease genommen werden, nie umgekehrt.
//!
//! # Datenschutz
//! Kein Hostname in der Datei. `pid` verlässt nie den Server (das Mapping auf
//! den Draht liegt in `harw-session-host`). Pfade werden nie geloggt und
//! stehen nicht im `Debug` des Handles.

use std::collections::HashMap;
use std::fs::File;
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use fs4::FileExt;
use harw_fsutil::OpenMode;
use harw_types::SessionId;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::{SessionStoreError, SessionStoreResult};
use crate::reader::MAX_RECORD_BYTES;
use crate::record::TranscriptRecord;
use crate::store::{TranscriptStore, safe_session_component};

/// Dateiendung der Lease neben dem Transcript: `<root>/<id>.lease`.
pub const LEASE_EXT: &str = "lease";

/// Wie lange `acquire` bei fremd belegtem Lock höchstens erneut probiert
/// (Abfrageabstand 2 ms). Deckt kurze Short-Takes und `peek_holder` ab. [K-W1]
pub const LEASE_RETRY_WINDOW: Duration = Duration::from_millis(25);

/// Abfrageabstand innerhalb von [`LEASE_RETRY_WINDOW`].
const LEASE_RETRY_STEP: Duration = Duration::from_millis(2);

/// Rechte einer neu angelegten Lease-Datei.
const LEASE_CREATE_MODE: u32 = 0o600;

/// Obergrenze für das Lesen des Holder-JSON (die echte Zeile ist < 100 Bytes).
const MAX_HOLDER_BYTES: u64 = 4096;

/// Blockgröße der Rückwärtssuche in [`head`].
const HEAD_CHUNK: u64 = 64 * 1024;

/// Wer die Lease hält. Datei-JSON: `"tui" | "gateway" | "job"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HolderKind {
    Tui,
    Gateway,
    Job,
    /// Unbekannter Wert aus einer neueren Fassung.
    #[serde(other)]
    Unknown,
}

/// Inhalt der Lease-Datei. Kein Hostname. `pid` verlässt nie den Server.
/// JSON: `{"holder":"tui","pid":4242,"since":"2026-11-22T10:00:00Z"}` [Quelle §3.5]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseHolder {
    #[serde(rename = "holder")]
    pub kind: HolderKind,
    pub pid: u32,
    pub since: Timestamp,
}

/// Transcript-Kopf für die Kopfprüfung vor jedem Turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptHead {
    /// Dateilänge in Bytes (0 ohne Transcript).
    pub len: u64,
    /// `sequence` des letzten vollständigen Datensatzes + 1; 0 ohne Datensatz.
    /// Gleiche Bedeutung wie `TranscriptSource::head` (`harw-session-host/src/replay.rs:51`).
    pub next_sequence: u64,
}

/// Geteilter Zustand eines gehaltenen Leases. Lebt, solange ein
/// [`SessionLease`]-Klon lebt; sein `Drop` gibt die Lease frei.
struct LeaseInner {
    /// Registry-Schlüssel (kanonischer Lease-Pfad). Nie geloggt.
    key: PathBuf,
    /// Deskriptor, auf dem der exklusive `flock` liegt.
    file: File,
    session: SessionId,
    holder: LeaseHolder,
}

impl Drop for LeaseInner {
    fn drop(&mut self) {
        // Erst Datei leeren und Lock freigeben, DANN die Registry sperren:
        // so kann ein gleichzeitiges `acquire` desselben Prozesses (das die
        // Registry-Mutex während seines Wiederholfensters hält) den Lock
        // bekommen, statt bis zum Fensterende auf diesen Drop zu warten.
        // Fehler hier sind nicht meldbar; das Schließen des Deskriptors gibt
        // den Lock spätestens frei.
        let _ = self.file.set_len(0);
        let _ = FileExt::unlock(&self.file);

        let mut registry = registry();
        let own = std::ptr::from_ref::<LeaseInner>(self);
        if registry
            .get(&self.key)
            .is_some_and(|entry| std::ptr::eq(entry.as_ptr(), own))
        {
            registry.remove(&self.key);
        }
    }
}

/// Prozessweit geteiltes Lease-Handle. `Clone` teilt dasselbe `Arc`.
/// Das letzte gedroppte Handle leert die Datei (`set_len(0)`) und gibt den fs4-Lock frei.
#[derive(Clone)]
pub struct SessionLease {
    inner: Arc<LeaseInner>,
}

impl std::fmt::Debug for SessionLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionLease")
            .field("session", &self.inner.session)
            .field("holder", &self.inner.holder)
            .finish()
    }
}

impl SessionLease {
    /// Nimmt die Lease für `session` unter `root` (das Transcript-Wurzelverzeichnis).
    ///
    /// - Hält DIESER Prozess sie schon (Registry): gibt einen Klon des vorhandenen Handles zurück,
    ///   egal welches `kind` (gleicher Prozess sperrt sich nicht selbst) [Quelle §3.5].
    /// - Sonst: `<root>/<id>.lease` öffnen (`open_nofollow`, read+write+create, mode 0o600),
    ///   `try_lock` bis `LEASE_RETRY_WINDOW`, dann Datei kürzen und Holder-JSON schreiben, `sync_data`.
    /// - Fremd belegt: `Err(SessionStoreError::LeaseHeld { session, holder })`, `holder` aus der Datei
    ///   (`None` wenn leer oder nicht lesbar).
    ///
    /// Die Registry-Mutex wird während Öffnen+try_lock gehalten (kein Selbst-Wettlauf zweier Threads).
    ///
    /// # Errors
    /// - [`SessionStoreError::UnsafeTranscriptPath`]: unsichere Session-Id.
    /// - `SessionStoreError::LeaseHeld`: ein anderer Prozess hält die Lease.
    /// - [`SessionStoreError::Io`], [`SessionStoreError::Serde`]: Datei-Fehler
    ///   (auch ein Symlink an Stelle der Lease-Datei).
    pub fn acquire(root: &Path, session: &SessionId, kind: HolderKind) -> SessionStoreResult<Self> {
        let path = lease_path(root, session)?;
        let key = registry_key(root, &path);

        let mut registry = registry();
        if let Some(existing) = registry.get(&key).and_then(Weak::upgrade) {
            return Ok(Self { inner: existing });
        }

        std::fs::create_dir_all(root).map_err(SessionStoreError::Io)?;
        let mut file = open_lease_rw(&path)?;
        lock_exclusive_with_retry(&mut file, session)?;

        let holder = LeaseHolder {
            kind,
            pid: std::process::id(),
            since: Timestamp::now(),
        };
        // Schlägt das Schreiben fehl, schließt das Droppen von `file` den
        // Deskriptor und gibt den Lock wieder frei.
        write_holder(&mut file, &holder)?;

        let inner = Arc::new(LeaseInner {
            key: key.clone(),
            file,
            session: session.clone(),
            holder,
        });
        registry.insert(key, Arc::downgrade(&inner));
        drop(registry);
        tracing::debug!(session = %session, holder = ?kind, "session lease acquired");
        Ok(Self { inner })
    }

    /// Klon des Handles, wenn dieser Prozess die Lease hält; sonst `None`. Keine I/O außer canonicalize.
    #[must_use]
    pub fn held_here(root: &Path, session: &SessionId) -> Option<Self> {
        let path = lease_path(root, session).ok()?;
        let key = registry_key(root, &path);
        registry()
            .get(&key)
            .and_then(Weak::upgrade)
            .map(|inner| Self { inner })
    }

    /// Wer die Lease hält (bei Klonen: der erste Nehmer in diesem Prozess).
    #[must_use]
    pub fn holder(&self) -> &LeaseHolder {
        &self.inner.holder
    }

    /// Session, für die diese Lease gilt.
    #[must_use]
    pub fn session(&self) -> &SessionId {
        &self.inner.session
    }
}

/// `<root>/<safe_session_component(id)>.lease`. Fehler wie `transcript_path` (unsichere Id).
///
/// # Errors
/// [`SessionStoreError::UnsafeTranscriptPath`] für eine unsichere Session-Id.
pub fn lease_path(root: &Path, session: &SessionId) -> SessionStoreResult<PathBuf> {
    Ok(root
        .join(safe_session_component(session)?)
        .with_extension(LEASE_EXT))
}

/// Wer hält die Lease gerade? Für Listen und das Handy.
/// - Hält dieser Prozess sie: dessen Holder aus der Registry, ohne Lock-Versuch.
/// - Sonst: `try_lock_shared`. Gelingt das, ist niemand Halter -> `Ok(None)`
///   (sofort `unlock`). Scheitert es mit WouldBlock -> Datei lesen, `Ok(Some(holder))` bzw. bei
///   leerer oder unlesbarer Datei `Ok(Some(LeaseHolder{kind: Unknown, pid: 0, since: <mtime der Datei>}))`.
/// - Keine Lease-Datei -> `Ok(None)`. Nie schreibend.
///
/// Der Inhalt wird erst NACH dem Lock-Versuch gelesen, damit er möglichst
/// frisch ist (der Vertrag lässt die Reihenfolge offen; das Ergebnis ist
/// dasselbe bis auf ein engeres Zeitfenster für `Unknown`).
///
/// # Errors
/// [`SessionStoreError::UnsafeTranscriptPath`], [`SessionStoreError::Io`].
pub fn peek_holder(root: &Path, session: &SessionId) -> SessionStoreResult<Option<LeaseHolder>> {
    let path = lease_path(root, session)?;
    let key = registry_key(root, &path);
    if let Some(inner) = registry().get(&key).and_then(Weak::upgrade) {
        return Ok(Some(inner.holder.clone()));
    }

    let mut file = match open_lease_read(&path) {
        Ok(file) => file,
        Err(SessionStoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    match FileExt::try_lock_shared(&file) {
        Ok(()) => {
            FileExt::unlock(&file).map_err(SessionStoreError::Io)?;
            Ok(None)
        }
        Err(fs4::TryLockError::WouldBlock) => {
            if let Some(holder) = read_holder(&mut file) {
                return Ok(Some(holder));
            }
            let since = file
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| Timestamp::try_from(modified).ok())
                .unwrap_or(Timestamp::UNIX_EPOCH);
            Ok(Some(LeaseHolder {
                kind: HolderKind::Unknown,
                pid: 0,
                since,
            }))
        }
        Err(fs4::TryLockError::Error(error)) => Err(SessionStoreError::Io(error)),
    }
}

/// Liest den Transcript-Kopf: `stat` + Rückwärtssuche des letzten vollständigen `\n`-Datensatzes,
/// `sequence` daraus. Kein Lock. Abgerissene letzte Zeile wird ignoriert. Leerzeilen werden
/// übersprungen wie im Reader. Kein Transcript -> `TranscriptHead { len: 0, next_sequence: 0 }`.
///
/// Die Suche reicht höchstens `2 * (MAX_RECORD_BYTES + 1)` Bytes zurück: ein abgerissener
/// Rest ist kürzer als eine Zeile, eine vollständige Zeile höchstens `MAX_RECORD_BYTES` lang.
/// Gelesen wird in 64-KiB-Blöcken, im Normalfall also ein Block.
///
/// # Errors
/// - [`SessionStoreError::UnsafeTranscriptPath`]: unsichere Session-Id.
/// - [`SessionStoreError::CorruptRecord`]: die letzte vollständige Zeile ist kein Datensatz.
/// - [`SessionStoreError::Io`]: Lesefehler, auch ein Symlink an Stelle des Transcripts.
pub fn head(store: &TranscriptStore, session: &SessionId) -> SessionStoreResult<TranscriptHead> {
    let path = store.transcript_path(session)?;
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(SessionStoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "transcript must not be a symbolic link",
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TranscriptHead {
                len: 0,
                next_sequence: 0,
            });
        }
        Err(error) => return Err(SessionStoreError::Io(error)),
    }
    let mut file = match open_nofollow(&path, OpenMode::read_only()) {
        Ok(file) => file,
        Err(SessionStoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TranscriptHead {
                len: 0,
                next_sequence: 0,
            });
        }
        Err(error) => return Err(error),
    };
    let len = file.metadata().map_err(SessionStoreError::Io)?.len();
    let next_sequence = match last_complete_line(&mut file, len)? {
        None => 0,
        Some(line) => {
            let text =
                std::str::from_utf8(&line).map_err(|error| SessionStoreError::CorruptRecord {
                    detail: error.to_string(),
                })?;
            TranscriptRecord::from_jsonl_line(text)?
                .sequence
                .saturating_add(1)
        }
    };
    Ok(TranscriptHead { len, next_sequence })
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

type Registry = HashMap<PathBuf, Weak<LeaseInner>>;

/// Sperrt die prozessweite Registry; eine vergiftete Mutex wird übernommen
/// (die Map bleibt gültig, ein Eintrag ist höchstens tot).
fn registry() -> MutexGuard<'static, Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// `canonicalize(root).join(<id>.lease)`, Fallback `path` wie gegeben.
fn registry_key(root: &Path, path: &Path) -> PathBuf {
    match (std::fs::canonicalize(root), path.file_name()) {
        (Ok(canonical), Some(name)) => canonical.join(name),
        _ => path.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
// Datei-Helfer
// ---------------------------------------------------------------------------

fn open_lease_rw(path: &Path) -> SessionStoreResult<File> {
    open_nofollow(
        path,
        OpenMode {
            read: true,
            write: true,
            create: true,
            create_new: false,
            truncate: false,
            append: false,
            mode: LEASE_CREATE_MODE,
        },
    )
}

fn open_lease_read(path: &Path) -> SessionStoreResult<File> {
    open_nofollow(path, OpenMode::read_only())
}

/// Öffnet ohne dem letzten Pfadglied als Symlink zu folgen (wie `store.rs`).
fn open_nofollow(path: &Path, mode: OpenMode) -> SessionStoreResult<File> {
    #[cfg(unix)]
    {
        harw_fsutil::open_nofollow(path, mode).map_err(SessionStoreError::Io)
    }
    #[cfg(not(unix))]
    {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(SessionStoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "lease or transcript must not be a symbolic link",
                )));
            }
            _ => {}
        }
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

/// Exklusiver `try_lock`, wiederholt bis [`LEASE_RETRY_WINDOW`]. Fremd belegt
/// -> `LeaseHeld` mit dem Holder aus der Datei.
fn lock_exclusive_with_retry(file: &mut File, session: &SessionId) -> SessionStoreResult<()> {
    let deadline = Instant::now() + LEASE_RETRY_WINDOW;
    loop {
        match FileExt::try_lock(&*file) {
            Ok(()) => return Ok(()),
            Err(fs4::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return Err(SessionStoreError::LeaseHeld {
                        session: session.clone(),
                        holder: read_holder(file),
                    });
                }
                std::thread::sleep(LEASE_RETRY_STEP);
            }
            Err(fs4::TryLockError::Error(error)) => return Err(SessionStoreError::Io(error)),
        }
    }
}

/// Kürzt die Datei und schreibt das Holder-JSON durabel.
fn write_holder(file: &mut File, holder: &LeaseHolder) -> SessionStoreResult<()> {
    let json = serde_json::to_vec(holder)?;
    file.set_len(0).map_err(SessionStoreError::Io)?;
    file.seek(SeekFrom::Start(0))
        .map_err(SessionStoreError::Io)?;
    file.write_all(&json).map_err(SessionStoreError::Io)?;
    file.sync_data().map_err(SessionStoreError::Io)?;
    Ok(())
}

/// Holder aus der Datei; `None` bei leerer, zu großer oder unlesbarer Datei.
fn read_holder(file: &mut File) -> Option<LeaseHolder> {
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_HOLDER_BYTES).read_to_end(&mut bytes).ok()?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

/// Liest `[start, end)` aus `file`.
fn read_range(file: &mut File, start: u64, end: u64) -> SessionStoreResult<Vec<u8>> {
    let size = usize::try_from(end - start)
        .map_err(|error| SessionStoreError::Io(std::io::Error::other(error)))?;
    let mut bytes = vec![0_u8; size];
    file.seek(SeekFrom::Start(start))
        .map_err(SessionStoreError::Io)?;
    file.read_exact(&mut bytes).map_err(SessionStoreError::Io)?;
    Ok(bytes)
}

/// Letzte vollständige, nicht leere Zeile (ohne `\n`) der Datei, oder `None`.
fn last_complete_line(file: &mut File, len: u64) -> SessionStoreResult<Option<Vec<u8>>> {
    let max_record = u64::try_from(MAX_RECORD_BYTES).unwrap_or(u64::MAX);
    let scan_limit = max_record.saturating_add(1).saturating_mul(2);
    let floor = len.saturating_sub(scan_limit);

    // `window` enthält die Bytes `[start, len)`.
    let mut window: Vec<u8> = Vec::new();
    let mut start = len;
    loop {
        if let Some(found) = find_last_line(&window, start == 0) {
            return Ok(found.map(<[u8]>::to_vec));
        }
        if start <= floor {
            // Kein `\n`-Datensatz in der größtmöglichen Spanne: kann kein
            // gültiges Transcript sein (Zeilen sind höchstens MAX_RECORD_BYTES).
            return Err(SessionStoreError::CorruptRecord {
                detail: "no complete transcript record within the head scan window".to_owned(),
            });
        }
        let next_start = start.saturating_sub(HEAD_CHUNK).max(floor);
        let mut chunk = read_range(file, next_start, start)?;
        chunk.extend_from_slice(&window);
        window = chunk;
        start = next_start;
    }
}

/// Sucht in `window` (ein Datei-Suffix) die letzte vollständige, nicht leere Zeile.
///
/// - `Some(Some(line))`: gefunden.
/// - `Some(None)`: sicher keine (Fenster beginnt am Dateianfang).
/// - `None`: mehr Bytes von vorn nötig.
///
/// Bytes nach dem letzten `\n` sind ein abgerissener Rest und zählen nicht.
fn find_last_line(window: &[u8], at_file_start: bool) -> Option<Option<&[u8]>> {
    let mut end = match window.iter().rposition(|byte| *byte == b'\n') {
        Some(end) => end,
        None => return at_file_start.then_some(None),
    };
    loop {
        match window[..end].iter().rposition(|byte| *byte == b'\n') {
            Some(previous) => {
                let line = &window[previous + 1..end];
                if !line.iter().all(u8::is_ascii_whitespace) {
                    return Some(Some(line));
                }
                end = previous;
            }
            None => {
                if !at_file_start {
                    return None;
                }
                let line = &window[..end];
                return if line.iter().all(u8::is_ascii_whitespace) {
                    Some(None)
                } else {
                    Some(Some(line))
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use harw_types::{SessionId, ThreadRef};

    use super::*;
    use crate::record::RecordKind;
    use crate::test_support::{TestError, TestResult, ctx};

    const SESSION: &str = "sess-a1";
    const FOREIGN_JSON: &str = r#"{"holder":"tui","pid":4242,"since":"2026-11-22T10:00:00Z"}"#;

    fn session() -> SessionId {
        SessionId::from_str(SESSION)
    }

    /// Öffnet die Lease-Datei über einen eigenen fd (wie ein fremder Prozess),
    /// lockt sie exklusiv und schreibt `content`.
    fn foreign_holder(root: &Path, content: &str) -> TestResult<File> {
        let path = lease_path(root, &session())?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        FileExt::try_lock(&file).map_err(ctx("fremder try_lock"))?;
        file.set_len(0)?;
        file.write_all(content.as_bytes())?;
        file.sync_data()?;
        Ok(file)
    }

    fn record(sequence: u64) -> TranscriptRecord {
        TranscriptRecord::new(
            session(),
            ThreadRef::from_str("thread-a1"),
            sequence,
            Timestamp::UNIX_EPOCH,
            RecordKind::Turn,
            serde_json::json!({ "sequence": sequence }),
        )
    }

    /// Schreibt Datensätze roh (ohne `append`, also ohne Lease-Short-Take).
    fn write_records(store: &TranscriptStore, sequences: &[u64], tail: &str) -> TestResult<u64> {
        let path = store.transcript_path(&session())?;
        std::fs::create_dir_all(store.root())?;
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        for sequence in sequences {
            file.write_all(record(*sequence).to_jsonl_line()?.as_bytes())?;
        }
        file.write_all(tail.as_bytes())?;
        file.sync_data()?;
        Ok(std::fs::metadata(&path)?.len())
    }

    #[test]
    fn second_process_holder_gets_lease_held_with_holder() -> TestResult {
        let dir = tempfile::tempdir()?;
        let _foreign = foreign_holder(dir.path(), FOREIGN_JSON)?;

        match SessionLease::acquire(dir.path(), &session(), HolderKind::Gateway) {
            Err(SessionStoreError::LeaseHeld {
                session: id,
                holder,
            }) => {
                if id != session() {
                    return Err(TestError::Unexpected(format!("falsche Session: {id}")));
                }
                let holder = holder.ok_or(TestError::Missing("holder"))?;
                if holder.kind != HolderKind::Tui || holder.pid != 4242 {
                    return Err(TestError::Unexpected(format!("{holder:?}")));
                }
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        if SessionLease::held_here(dir.path(), &session()).is_some() {
            return Err(TestError::Unexpected(
                "Registry-Eintrag trotz Fehler".into(),
            ));
        }
        Ok(())
    }

    #[test]
    fn foreign_empty_lease_file_yields_lease_held_without_holder() -> TestResult {
        let dir = tempfile::tempdir()?;
        let _foreign = foreign_holder(dir.path(), "")?;
        match SessionLease::acquire(dir.path(), &session(), HolderKind::Job) {
            Err(SessionStoreError::LeaseHeld { holder: None, .. }) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn same_process_does_not_lock_itself() -> TestResult {
        let dir = tempfile::tempdir()?;
        let first = SessionLease::acquire(dir.path(), &session(), HolderKind::Tui)?;
        let second = SessionLease::acquire(dir.path(), &session(), HolderKind::Gateway)?;

        if !Arc::ptr_eq(&first.inner, &second.inner) {
            return Err(TestError::Unexpected("zweites Handle teilt nicht".into()));
        }
        if second.holder().kind != HolderKind::Tui {
            return Err(TestError::Unexpected(format!("{:?}", second.holder())));
        }
        if second.holder().pid != std::process::id() || second.session() != &session() {
            return Err(TestError::Unexpected(format!("{second:?}")));
        }
        let here = SessionLease::held_here(dir.path(), &session())
            .ok_or(TestError::Missing("held_here"))?;
        if !Arc::ptr_eq(&here.inner, &first.inner) {
            return Err(TestError::Unexpected(
                "held_here liefert anderes Handle".into(),
            ));
        }

        let written: LeaseHolder =
            serde_json::from_slice(&std::fs::read(lease_path(dir.path(), &session())?)?)?;
        if &written != first.holder() {
            return Err(TestError::Unexpected(format!("{written:?}")));
        }
        Ok(())
    }

    #[test]
    fn drop_of_last_handle_releases_and_clears_file() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = lease_path(dir.path(), &session())?;
        let first = SessionLease::acquire(dir.path(), &session(), HolderKind::Gateway)?;
        let second = first.clone();

        drop(first);
        if std::fs::metadata(&path)?.len() == 0 {
            return Err(TestError::Unexpected(
                "Datei geleert, obwohl ein Klon lebt".into(),
            ));
        }
        let probe = OpenOptions::new().read(true).write(true).open(&path)?;
        if FileExt::try_lock(&probe).is_ok() {
            return Err(TestError::Unexpected(
                "Lock frei, obwohl ein Klon lebt".into(),
            ));
        }

        drop(second);
        if std::fs::metadata(&path)?.len() != 0 {
            return Err(TestError::Unexpected("Datei nach Drop nicht leer".into()));
        }
        FileExt::try_lock(&probe).map_err(ctx("fremder try_lock nach Drop"))?;
        FileExt::unlock(&probe)?;
        if SessionLease::held_here(dir.path(), &session()).is_some() {
            return Err(TestError::Unexpected("Registry-Eintrag nach Drop".into()));
        }
        if peek_holder(dir.path(), &session())?.is_some() {
            return Err(TestError::Unexpected("peek_holder nach Drop".into()));
        }
        Ok(())
    }

    #[test]
    fn crash_releases_lock() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = lease_path(dir.path(), &session())?;
        // "Absturz": fd schließen ohne Drop-Logik; die Datei behält ihren Inhalt.
        drop(foreign_holder(dir.path(), FOREIGN_JSON)?);
        if std::fs::read_to_string(&path)? != FOREIGN_JSON {
            return Err(TestError::Unexpected(
                "Inhalt nach Absturz verändert".into(),
            ));
        }

        if let Some(holder) = peek_holder(dir.path(), &session())? {
            return Err(TestError::Unexpected(format!("{holder:?}")));
        }
        let lease = SessionLease::acquire(dir.path(), &session(), HolderKind::Job)?;
        if lease.holder().kind != HolderKind::Job {
            return Err(TestError::Unexpected(format!("{lease:?}")));
        }
        let written: LeaseHolder = serde_json::from_slice(&std::fs::read(&path)?)?;
        if written.kind != HolderKind::Job || written.pid != std::process::id() {
            return Err(TestError::Unexpected(format!("{written:?}")));
        }
        Ok(())
    }

    #[test]
    fn peek_holder_reads_kind_and_since() -> TestResult {
        let dir = tempfile::tempdir()?;
        if peek_holder(dir.path(), &session())?.is_some() {
            return Err(TestError::Unexpected("Holder ohne Lease-Datei".into()));
        }

        let foreign = foreign_holder(dir.path(), FOREIGN_JSON)?;
        let holder = peek_holder(dir.path(), &session())?.ok_or(TestError::Missing("holder"))?;
        let since: Timestamp = "2026-11-22T10:00:00Z".parse().map_err(ctx("since"))?;
        if holder.kind != HolderKind::Tui || holder.since != since {
            return Err(TestError::Unexpected(format!("{holder:?}")));
        }
        // Nie schreibend: Inhalt bleibt.
        if std::fs::read_to_string(lease_path(dir.path(), &session())?)? != FOREIGN_JSON {
            return Err(TestError::Unexpected("peek_holder hat geschrieben".into()));
        }
        drop(foreign);

        // Leere, aber gesperrte Datei -> Unknown, pid 0.
        let empty = foreign_holder(dir.path(), "")?;
        let unknown = peek_holder(dir.path(), &session())?.ok_or(TestError::Missing("unknown"))?;
        if unknown.kind != HolderKind::Unknown || unknown.pid != 0 {
            return Err(TestError::Unexpected(format!("{unknown:?}")));
        }
        drop(empty);

        // Eigener Prozess: aus der Registry.
        let lease = SessionLease::acquire(dir.path(), &session(), HolderKind::Gateway)?;
        let own = peek_holder(dir.path(), &session())?.ok_or(TestError::Missing("own"))?;
        if &own != lease.holder() {
            return Err(TestError::Unexpected(format!("{own:?}")));
        }
        Ok(())
    }

    #[test]
    fn unknown_holder_kind_parses_as_unknown() -> TestResult {
        let holder: LeaseHolder =
            serde_json::from_str(r#"{"holder":"robot","pid":1,"since":"2026-11-22T10:00:00Z"}"#)?;
        if holder.kind != HolderKind::Unknown {
            return Err(TestError::Unexpected(format!("{holder:?}")));
        }
        let json = serde_json::to_value(LeaseHolder {
            kind: HolderKind::Gateway,
            pid: 7,
            since: Timestamp::UNIX_EPOCH,
        })?;
        if json["holder"] != "gateway" {
            return Err(TestError::Unexpected(json.to_string()));
        }
        Ok(())
    }

    #[test]
    fn head_of_missing_transcript_is_zero() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = TranscriptStore::new(dir.path());
        let got = head(&store, &session())?;
        if got
            != (TranscriptHead {
                len: 0,
                next_sequence: 0,
            })
        {
            return Err(TestError::Unexpected(format!("{got:?}")));
        }
        Ok(())
    }

    #[test]
    fn head_counts_next_sequence() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = TranscriptStore::new(dir.path());
        let len = write_records(&store, &[0, 1, 2], "")?;
        let got = head(&store, &session())?;
        if got
            != (TranscriptHead {
                len,
                next_sequence: 3,
            })
        {
            return Err(TestError::Unexpected(format!("{got:?}")));
        }

        // Leerzeilen am Ende werden übersprungen wie im Reader.
        let len = write_records(&store, &[3], "\n\n")?;
        let got = head(&store, &session())?;
        if got
            != (TranscriptHead {
                len,
                next_sequence: 4,
            })
        {
            return Err(TestError::Unexpected(format!("{got:?}")));
        }
        Ok(())
    }

    #[test]
    fn head_ignores_torn_tail() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = TranscriptStore::new(dir.path());
        let len = write_records(&store, &[0, 1], r#"{"session_id":"sess-a1","seq"#)?;
        let got = head(&store, &session())?;
        if got
            != (TranscriptHead {
                len,
                next_sequence: 2,
            })
        {
            return Err(TestError::Unexpected(format!("{got:?}")));
        }

        // Nur ein abgerissener Rest, kein vollständiger Datensatz.
        let only_torn = tempfile::tempdir()?;
        let store = TranscriptStore::new(only_torn.path());
        let len = write_records(&store, &[], r#"{"session_id":"sess-a1""#)?;
        let got = head(&store, &session())?;
        if got
            != (TranscriptHead {
                len,
                next_sequence: 0,
            })
        {
            return Err(TestError::Unexpected(format!("{got:?}")));
        }
        Ok(())
    }

    #[test]
    fn head_finds_line_across_chunk_boundary() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = TranscriptStore::new(dir.path());
        let big = TranscriptRecord::new(
            session(),
            ThreadRef::from_str("thread-a1"),
            41,
            Timestamp::UNIX_EPOCH,
            RecordKind::Item,
            serde_json::json!({ "text": "x".repeat(200 * 1024) }),
        );
        let path = store.transcript_path(&session())?;
        std::fs::write(&path, big.to_jsonl_line()?)?;
        let got = head(&store, &session())?;
        if got.next_sequence != 42 {
            return Err(TestError::Unexpected(format!("{got:?}")));
        }
        Ok(())
    }

    #[test]
    fn lease_path_rejects_unsafe_id() -> TestResult {
        let dir = tempfile::tempdir()?;
        let unsafe_id = SessionId::from_str("../escape");
        match lease_path(dir.path(), &unsafe_id) {
            Err(SessionStoreError::UnsafeTranscriptPath(_)) => {}
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        match SessionLease::acquire(dir.path(), &unsafe_id, HolderKind::Tui) {
            Err(SessionStoreError::UnsafeTranscriptPath(_)) => {}
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        if SessionLease::held_here(dir.path(), &unsafe_id).is_some() {
            return Err(TestError::Unexpected("held_here für unsichere Id".into()));
        }
        let path = lease_path(dir.path(), &session())?;
        if path != dir.path().join("sess-a1.lease") {
            return Err(TestError::Unexpected(path.display().to_string()));
        }
        Ok(())
    }
}
