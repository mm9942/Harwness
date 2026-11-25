//! Metadata-only index for the first three actual completed hosted pairs.
//! A reservation is durable before the first typed item. An abandoned pending
//! turn invalidates the title source rather than substituting a later message.
use std::fs::{File, symlink_metadata};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use harw_fsutil::{OpenMode, open_nofollow};
use harw_types::{ItemId, SessionId, ThreadRef, TurnId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use crate::durability::sync_parent_directory;
use crate::store::{lock_transcript, persist_noclobber, safe_session_component};
use crate::{RecordKind, SessionStoreError, SessionStoreResult, TranscriptRecord, TranscriptStore};

const COUNT: usize = 3;
const INDEX_BYTES: u64 = 16 * 1024;
const ROW_BYTES: usize = 128 * 1024;
/// Maximum UTF-8 prefix per authenticated row.
pub const MAX_TITLE_TEXT_BYTES: usize = 3072;

/// Returned only to an already-authorized host caller; intentionally not Debug.
pub struct TitleSeedPair {
    /// User-authored text from the exact first-three completed exchange.
    pub user: String,
    /// Final accepted assistant text for that exchange.
    pub assistant: String,
}

/// Exact durable journal location, not a display row or replay ID.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TitleSeedLocation {
    offset: u64,
    length: usize,
    sequence: u64,
    item: ItemId,
    digest: [u8; 32],
}
impl TitleSeedLocation {
    pub(crate) fn from_line(record: &TranscriptRecord, offset: u64, line: &str) -> Option<Self> {
        let id = record.payload.get("id")?.as_str()?;
        if line.len() > ROW_BYTES || id.is_empty() || id.len() > 128 { return None; }
        Some(Self { offset, length: line.len(), sequence: record.sequence, item: ItemId::from_str(id),
            digest: Sha256::digest(line.as_bytes()).into() })
    }
    fn end(&self) -> Option<u64> {
        if self.length == 0 || self.length > ROW_BYTES { return None; }
        self.offset.checked_add(self.length as u64)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Locations { user: TitleSeedLocation, assistant: TitleSeedLocation }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending { turn: TurnId, user: Option<TitleSeedLocation> }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    version: u8, session: SessionId, thread: Option<ThreadRef>, disabled: bool,
    pairs: Vec<Locations>, pending: Option<Pending>,
}
impl Index {
    fn retire(&mut self) { self.disabled = true; self.pairs.clear(); self.pending = None; }
}
fn invalid() -> SessionStoreError {
    SessionStoreError::CorruptRecord { detail: "title source invalid".into() }
}
fn path(root: &Path, sid: &SessionId) -> SessionStoreResult<PathBuf> {
    Ok(root.join(format!("{}.title-seed-v1.json", safe_session_component(sid)?)))
}
fn revoked_path(index: &Path) -> PathBuf {
    index.with_extension("revoked")
}

// Probe entry existence without opening or inspecting the optional index.
fn present(path: &Path) -> SessionStoreResult<bool> {
    match symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn file(path: &Path) -> SessionStoreResult<Option<File>> {
    match open_nofollow(path, OpenMode::read_only()) {
        Ok(file) if file.metadata()?.is_file() => Ok(Some(file)),
        Ok(_) => Err(invalid()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn load(path: &Path, sid: &SessionId) -> SessionStoreResult<Option<Index>> {
    let Some(file) = file(path)? else { return Ok(None); };
    let mut wire = Vec::new();
    file.take(INDEX_BYTES + 1).read_to_end(&mut wire)?;
    if wire.len() as u64 > INDEX_BYTES { return Err(invalid()); }
    let index: Index = serde_json::from_slice(&wire).map_err(|_| invalid())?;
    if index.version != 1 || index.session != *sid || index.pairs.len() > COUNT ||
        (index.disabled && (!index.pairs.is_empty() || index.pending.is_some())) ||
        (index.pairs.len() == COUNT && index.pending.is_some())
    { return Err(invalid()); }
    Ok(Some(index))
}
fn encoded(index: &Index) -> SessionStoreResult<Vec<u8>> {
    let wire = serde_json::to_vec(index).map_err(|_| invalid())?;
    if wire.len() as u64 > INDEX_BYTES { return Err(invalid()); }
    Ok(wire)
}
fn save(path: &Path, index: &Index) -> SessionStoreResult<()> {
    let parent = path.parent().ok_or_else(invalid)?;
    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(&encoded(index)?)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error| error.error)?;
    sync_parent_directory(parent)
}
fn change<T: Default>(root: &Path, sid: &SessionId,
    f: impl FnOnce(&mut Index) -> SessionStoreResult<(T, bool)>,
) -> SessionStoreResult<T> {
    let path = path(root, sid)?;
    if !present(&path)? { return Ok(T::default()); }
    let _lock = lock_transcript(&path, sid)?;
    if present(&revoked_path(&path))? { return Ok(T::default()); }
    let Some(mut index) = load(&path, sid)? else { return Ok(T::default()); };
    let (value, changed) = f(&mut index)?;
    if changed { save(&path, &index)?; }
    Ok(value)
}
/// Fresh ordinary hosted sessions only, before publication/any user input.
pub fn initialize_title_seed(root: &Path, sid: &SessionId) -> SessionStoreResult<()> {
    let p = path(root, sid)?;
    // Same order as the certified reader: title fence, then transcript fence.
    let _title = lock_transcript(&p, sid)?;
    if present(&revoked_path(&p))? { return Err(invalid()); }
    let transcript = TranscriptStore::new(root).transcript_path(sid)?;
    let _journal = lock_transcript(&transcript, sid)?;
    if let Some(file) = file(&transcript)? && file.metadata()?.len() != 0 { return Err(invalid()); }
    persist_noclobber(&p, &encoded(&Index {
        version: 1, session: sid.clone(), thread: None, disabled: false, pairs: Vec::new(), pending: None,
    })?)
}
/// Reserve before writing the actual user row.
pub fn begin_title_seed(
    root: &Path, sid: &SessionId, thread: &ThreadRef, turn: &TurnId,
    preceding_users: usize, durable_users: Option<usize>,
) -> SessionStoreResult<bool>
{
    change(root, sid, |index| {
        if index.disabled || index.pairs.len() == COUNT { return Ok((false, false)); }
        // The hydrated active history and the independently observed append-only prefix
        // must both agree with the indexed pairs. In particular, a skipped user stays a
        // gap even if a later compaction removed it from the active history.
        if preceding_users != index.pairs.len() || durable_users != Some(index.pairs.len())
            || index.pending.is_some() || index.thread.as_ref().is_some_and(|t| t != thread)
        {
            index.retire();
            return Ok((false, true));
        }
        index.thread = Some(thread.clone());
        index.pending = Some(Pending { turn: turn.clone(), user: None });
        Ok((true, true))
    })
}
/// Bind the reservation to the exact already-durable user row.
pub fn attach_title_user(root: &Path, sid: &SessionId, turn: &TurnId, user: Option<TitleSeedLocation>)
    -> SessionStoreResult<()>
{
    change(root, sid, |index| {
        let Some(pending) = index.pending.as_mut().filter(|p| p.turn == *turn) else {
            return Ok(((), false));
        };
        if pending.user.is_some() || user.is_none() { index.retire(); }
        else { pending.user = user; }
        Ok(((), true))
    })
}
/// An unsuccessful or unprovable first-three user turn retires capture; do not skip it.
pub fn finish_title_seed(root: &Path, sid: &SessionId, turn: &TurnId, completed: bool,
    assistant: Option<TitleSeedLocation>,
) -> SessionStoreResult<()> {
    change(root, sid, |index| {
        if !index.pending.as_ref().is_some_and(|p| p.turn == *turn) { return Ok(((), false)); }
        let pending = index.pending.take().ok_or_else(invalid)?;
        match (completed, pending.user, assistant) {
            (true, Some(user), Some(assistant)) if user.end().is_some_and(|e| e <= assistant.offset)
                && user.sequence < assistant.sequence && user.item != assistant.item =>
            {
                index.pairs.push(Locations { user, assistant });
            }
            _ => index.retire(),
        }
        Ok(((), true))
    })
}
/// Durable revocation before authoritative rewind or nonhuman input.
///
/// The old index may be malformed. Do not deserialize, repair, remove, or
/// authorize it. A separate, immutable empty marker wins while the same title
/// lock excludes readers and publishers. The caller must not rewrite history
/// if this marker could not be made durable.
pub fn invalidate_title_seed(root: &Path, sid: &SessionId) -> SessionStoreResult<()> {
    let index = path(root, sid)?;
    let marker = revoked_path(&index);
    // A session with any history cannot be initialized for capture later.
    if !present(&index)? && !present(&marker)? { return Ok(()); }
    let _title = lock_transcript(&index, sid)?;
    if let Some(file) = file(&marker)? {
        if file.metadata()?.len() != 0 { return Err(invalid()); }
        // Recover a prior uncertainty only by establishing durability now.
        file.sync_all()?;
        return sync_parent_directory(root);
    }
    persist_noclobber(&marker, &[])
}
/// Read exactly six authenticated journal rows through an exclusive short-lived fence.
/// The caller can perform a metadata CAS in the callback under the same fence.
pub fn with_title_seed_pairs<T>(root: &Path, sid: &SessionId,
    f: impl FnOnce(Option<[TitleSeedPair; COUNT]>) -> T,
) -> SessionStoreResult<T> {
    let path = path(root, sid)?;
    if !present(&path)? { return Ok(f(None)); }
    let _lock = lock_transcript(&path, sid)?;
    // Any revocation entry is an unconditional denial. A malformed marker
    // cannot restore authority; only the writer validates it for durability.
    if present(&revoked_path(&path))? { return Ok(f(None)); }
    let Some(index) = load(&path, sid)? else { return Ok(f(None)); };
    if index.disabled || index.pending.is_some() || index.pairs.len() != COUNT { return Ok(f(None)); }
    let Some(thread) = index.thread.as_ref() else { return Err(invalid()); };
    let journal_path = TranscriptStore::new(root).transcript_path(sid)?;
    let _journal = lock_transcript(&journal_path, sid)?;
    let Some(mut journal) = file(&journal_path)? else { return Ok(f(None)); };
    let mut pairs = Vec::with_capacity(COUNT);
    let mut previous_end = 0;
    let mut previous_seq = None;
    for pair in index.pairs {
        if pair.user.offset < previous_end || previous_seq.is_some_and(|seq| seq >= pair.user.sequence) ||
            pair.user.end().is_none_or(|end| end > pair.assistant.offset) ||
            pair.user.sequence >= pair.assistant.sequence || pair.user.item == pair.assistant.item
        { return Err(invalid()); }
        let Some(user) = text(&mut journal, &pair.user, sid, thread, false)? else { return Ok(f(None)); };
        let Some(assistant) = text(&mut journal, &pair.assistant, sid, thread, true)? else { return Ok(f(None)); };
        previous_end = pair.assistant.end().ok_or_else(invalid)?;
        previous_seq = Some(pair.assistant.sequence);
        pairs.push(TitleSeedPair { user, assistant });
    }
    Ok(f(Some(pairs.try_into().map_err(|_| invalid())?)))
}
/// No replay, search, model or runtime access.
pub fn read_title_seed_pairs(root: &Path, sid: &SessionId) -> SessionStoreResult<Option<[TitleSeedPair; COUNT]>> {
    with_title_seed_pairs(root, sid, |pairs| pairs)
}
fn text(file: &mut File, loc: &TitleSeedLocation, sid: &SessionId, thread: &ThreadRef,
    assistant: bool,
) -> SessionStoreResult<Option<String>> {
    if loc.end().ok_or_else(invalid)? > file.metadata()?.len() { return Ok(None); }
    file.seek(SeekFrom::Start(loc.offset))?;
    let mut line = vec![0; loc.length];
    file.read_exact(&mut line)?;
    let digest: [u8; 32] = Sha256::digest(&line).into();
    if digest != loc.digest || line.last() != Some(&b'\n') { return Ok(None); }
    let record: TranscriptRecord = serde_json::from_slice(&line).map_err(|_| invalid())?;
    let data = &record.payload;
    let expected = if assistant { "assistant_message" } else { "user_message" };
    if record.kind != RecordKind::Item || record.session_id != *sid || record.thread != *thread ||
        record.sequence != loc.sequence || data.get("type").and_then(|v| v.as_str()) != Some(expected) ||
        data.get("id").and_then(|v| v.as_str()) != Some(loc.item.as_str()) ||
        (assistant && data.get("phase").and_then(|v| v.as_str()) != Some("final_answer"))
    { return Ok(None); }
    let mut parts = data.get("content").and_then(|v| v.as_array()).ok_or_else(invalid)?
        .iter().filter(|v| v.get("type").and_then(|v| v.as_str()) == Some("text"));
    let Some(part) = parts.next() else { return Ok(None); };
    if parts.next().is_some() { return Ok(None); }
    let Some(value) = part.get("text").and_then(|v| v.as_str()) else { return Ok(None); };
    if value.trim().is_empty() { return Ok(None); }
    let mut length = value.len().min(MAX_TITLE_TEXT_BYTES);
    while !value.is_char_boundary(length) { length -= 1; }
    Ok(Some(value[..length].to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;
    use serde_json::json;

    fn row(
        sid: &SessionId,
        thread: &ThreadRef,
        sequence: u64,
        assistant: bool,
        text: &str,
    ) -> TranscriptRecord {
        TranscriptRecord::new(
            sid.clone(),
            thread.clone(),
            sequence,
            jiff::Timestamp::now(),
            RecordKind::Item,
            json!({
                "type": if assistant { "assistant_message" } else { "user_message" },
                "id": ItemId::new().as_str(),
                "phase": if assistant { "final_answer" } else { "unused" },
                "content": [{"type":"text", "text":text}],
            }),
        )
    }

    #[test]
    fn exact_three_pairs_then_lock_the_scope_against_rewrite() -> TestResult {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        let store = TranscriptStore::new(root);
        let sid = SessionId::new();
        let thread = ThreadRef::from_str("title-seed");
        initialize_title_seed(root, &sid)?;
        let user = "é".repeat(MAX_TITLE_TEXT_BYTES);
        for (index, sequence) in [0u64, 2, 4].into_iter().enumerate() {
            let turn = TurnId::new();
            assert!(begin_title_seed(root, &sid, &thread, &turn, index, Some(index))?);
            let location = store.append_title_item(&row(&sid, &thread, sequence, false, &user))?;
            attach_title_user(root, &sid, &turn, location)?;
            assert!(read_title_seed_pairs(root, &sid)?.is_none());
            let assistant = format!("answer {}", index + 1);
            let location = store.append_title_item(&row(&sid, &thread, sequence + 1, true, &assistant))?;
            finish_title_seed(root, &sid, &turn, true, location)?;
            if index < 2 {
                assert!(read_title_seed_pairs(root, &sid)?.is_none());
            }
        }
        let pairs = read_title_seed_pairs(root, &sid)?.ok_or_else(invalid)?;
        assert_eq!(pairs[0].user.len(), MAX_TITLE_TEXT_BYTES);
        assert_eq!(pairs[2].assistant, "answer 3");
        assert!(!begin_title_seed(root, &sid, &thread, &TurnId::new(), 3, Some(3))?);
        invalidate_title_seed(root, &sid)?;
        assert!(read_title_seed_pairs(root, &sid)?.is_none());
        Ok(())
    }

    #[test]
    fn an_early_failure_or_corrupt_optional_index_cannot_authorize_new_pairs() -> TestResult {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        let sid = SessionId::new();
        let thread = ThreadRef::from_str("failure");
        let first = TurnId::new();
        initialize_title_seed(root, &sid)?;
        assert!(begin_title_seed(root, &sid, &thread, &first, 0, Some(0))?);
        finish_title_seed(root, &sid, &first, false, None)?;
        assert!(!begin_title_seed(root, &sid, &thread, &TurnId::new(), 1, Some(1))?);
        assert!(read_title_seed_pairs(root, &sid)?.is_none());

        let index = path(root, &sid)?;
        std::fs::write(&index, b"{malformed")?;
        assert!(read_title_seed_pairs(root, &sid).is_err());
        invalidate_title_seed(root, &sid)?;
        assert!(read_title_seed_pairs(root, &sid)?.is_none());
        assert_eq!(std::fs::read(&index)?.as_slice(), b"{malformed");
        Ok(())
    }
}
