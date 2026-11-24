//! Transcript records → durable session frames (PL-65 §3.4).
//!
//! The durable part of a session stream is the session's transcript. Every
//! record keeps its transcript `sequence`; a frame built from it carries the
//! cursor `Cursor { generation, durable: sequence + 1, live: 0 }`, i.e. the
//! position of the next record the client has not seen yet.
//!
//! Mapping (derived from the runtime writer `harw_core::TranscriptStateStore`):
//! - `RecordKind::Item`: the payload is a serialized [`TurnItem`] (internally
//!   tagged by `"type"`) → [`SessionFrame::Turn`] with
//!   [`TurnEvent::ItemAdded`]. The writer stores no turn id, so the turn id is
//!   the deterministic fallback `replay-<sequence>` (see [`replay_turn_id`]).
//!   A payload that does not decode as a [`TurnItem`] is skipped.
//! - `RecordKind::Lifecycle` with `{"marker": "history_replaced",
//!   "item_count": N}` → [`SessionFrame::HistoryReplaced`] (`N` saturates at
//!   `u32::MAX`, which still means "keep everything").
//! - Every other record is skipped: `core_session_state` snapshots, `drift`
//!   and `agent_orchestration` markers, gateway `{"event": …}` lifecycle
//!   records, `RecordKind::Turn` usage rounds and `RecordKind::ToolCall`.
//!
//! Skipped records still advance reading progress: [`replay`] reads at most
//! `limit` records, so a page may carry fewer than `limit` frames.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use harw_protocol::items::TurnItem;
use harw_protocol::{Cursor, FrameEnvelope, SessionFrame, TurnEvent};
use harw_session_store::{RecordKind, SessionStoreError, TranscriptRecord, TranscriptStore};
use harw_types::{SessionId, TurnId};

use crate::error::HostError;

/// Upper bound for one page, whatever the caller asks for.
pub const MAX_PAGE: usize = 10_000;

/// Lifecycle marker the runtime writes after a history replacement.
const HISTORY_REPLACED_MARKER: &str = "history_replaced";

/// Read access to session transcripts, in transcript order.
pub trait TranscriptSource: Send + Sync {
    /// Records with sequence >= from_sequence, at most `limit`, ascending.
    fn read_from(
        &self,
        session: &SessionId,
        from_sequence: u64,
        limit: usize,
    ) -> Result<Vec<TranscriptRecord>, HostError>;

    /// An authoritative read of the visible user/assistant prefix, stopping
    /// before an exact canonical user item. Unsupported stores fail closed.
    fn fork_prefix(
        &self,
        _session: &SessionId,
        _before: &str,
        _committed_head: u64,
    ) -> Result<Vec<TurnItem>, HostError> {
        Err(HostError::NotImplemented("durable session fork"))
    }

    /// Exactly the ordered displayed User/Assistant rows under the attached canonical cursor.
    fn displayed_fork_prefix(
        &self,
        _session: &SessionId,
        _selection: &harw_protocol::session_wire::DisplayedForkV1,
        _thread: &harw_types::ThreadRef,
    ) -> Result<Vec<TurnItem>, HostError> {
        Err(HostError::NotImplemented("displayed session fork"))
    }

    /// The first sequence belonging to the current canonical transcript.
    /// Archives keep the preceding records but chat replay must never mix them in.
    fn canonical_floor(&self, _session: &SessionId, _head: u64) -> Result<u64, HostError> { Ok(0) }

    /// Sequence the next appended record will get (0 for no transcript).
    fn head(&self, session: &SessionId) -> Result<u64, HostError>;
}

/// [`TranscriptSource`] over the durable JSONL [`TranscriptStore`].
///
/// Reads are synchronous file I/O; a missing transcript is an empty one.
pub struct StoreTranscripts {
    store: TranscriptStore,
}

impl StoreTranscripts {
    /// Source reading from `store`.
    #[must_use]
    pub fn new(store: TranscriptStore) -> Self {
        Self { store }
    }

    /// Reader of `session`, or `None` when no transcript exists yet.
    fn reader(
        &self,
        session: &SessionId,
    ) -> Result<Option<harw_session_store::TranscriptReader>, HostError> {
        match self.store.reader(session) {
            Ok(reader) => Ok(Some(reader)),
            Err(SessionStoreError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

impl TranscriptSource for StoreTranscripts {
    fn fork_prefix(
        &self,
        session: &SessionId,
        before: &str,
        committed_head: u64,
    ) -> Result<Vec<TurnItem>, HostError> {
        let mut reader = self
            .store
            .snapshot_reader(session, FORK_SCAN_MAX_BYTES)
            .map_err(|error| match error {
                SessionStoreError::CorruptRecord { .. } => HostError::Denied(
                    "fork source journal is incomplete or exceeds its byte limit".into(),
                ),
                other => HostError::from(other),
            })?;
        let items = canonical_fork_prefix(
            reader.by_ref().map(|record| {
                record.map_err(|_| HostError::Denied("fork source journal is incomplete".into()))
            }),
            session,
            before,
            committed_head,
        )?;
        if !self.store.snapshot_is_current(session, &reader)? {
            return Err(HostError::Busy(
                "fork source journal has changed; try again".into(),
            ));
        }
        Ok(items)
    }

    fn displayed_fork_prefix(
        &self, session: &SessionId, selection: &harw_protocol::session_wire::DisplayedForkV1,
        thread: &harw_types::ThreadRef,
    ) -> Result<Vec<TurnItem>, HostError> {
        let mut reader = self.store.snapshot_reader(session, FORK_SCAN_MAX_BYTES)
            .map_err(|_| HostError::Denied("Displayed fork source unavailable or above the safe size limit".into()))?;
        let items = canonical_fork_items(
            reader.by_ref().map(|record| {
                record.map_err(|_| HostError::Denied("Displayed fork source is incomplete".into()))
            }), session, selection.head.durable, Some(thread),
        )?;
        let result = verified_displayed_fork(items, selection)?;
        if !self.store.snapshot_is_current(session, &reader)? {
            return Err(HostError::Busy("Displayed chat changed; refresh it and try Fork again".into()));
        }
        Ok(result)
    }

    fn read_from(
        &self,
        session: &SessionId,
        from_sequence: u64,
        limit: usize,
    ) -> Result<Vec<TranscriptRecord>, HostError> {
        let limit = limit.min(MAX_PAGE);
        let mut records = Vec::new();
        if limit == 0 {
            return Ok(records);
        }
        let Some(reader) = self.reader(session)? else {
            return Ok(records);
        };
        for record in reader {
            let record = record?;
            if record.sequence < from_sequence {
                continue;
            }
            records.push(record);
            if records.len() >= limit {
                break;
            }
        }
        Ok(records)
    }

    fn canonical_floor(&self, session: &SessionId, head: u64) -> Result<u64, HostError> {
        let Some(reader) = self.reader(session)? else { return Ok(0); };
        canonical_floor(reader.map(|record| record.map_err(HostError::from)), head)
    }

    fn head(&self, session: &SessionId) -> Result<u64, HostError> {
        let Some(reader) = self.reader(session)? else {
            return Ok(0);
        };
        // Same rule as the runtime's sequence recovery: max(sequence) + 1.
        let mut next = 0_u64;
        for record in reader {
            let record = record?;
            let candidate = record.sequence.checked_add(1).ok_or_else(|| {
                HostError::Storage(format!("transcript sequence exhausted for {session}"))
            })?;
            next = next.max(candidate);
        }
        Ok(next)
    }
}

const FORK_SCAN_MAX_RECORDS: usize = 32_768;
const FORK_SCAN_MAX_BYTES: u64 = 24 * 1024 * 1024;
const FORK_MAX_ITEMS: usize = 512;
const FORK_PREFIX_MAX_BYTES: usize = 2 * 1024 * 1024;
const HISTORY_REPLACE_STARTED: &str = "history_replace_started";

fn canonical_floor(
    records: impl Iterator<Item = Result<TranscriptRecord, HostError>>,
    head: u64,
) -> Result<u64, HostError> {
    let mut floor = 0;
    let mut pending = None::<(u64, u64)>;
    for record in records {
        let record = record?;
        if record.sequence >= head { break; }
        if record.kind != RecordKind::Lifecycle { continue; }
        let marker = record.payload.get("marker").and_then(serde_json::Value::as_str);
        let count = record.payload.get("item_count").and_then(serde_json::Value::as_u64);
        match (marker, count) {
            (Some(HISTORY_REPLACE_STARTED), Some(count)) if pending.is_none() => {
                pending = Some((record.sequence, count));
            }
            (Some(HISTORY_REPLACED_MARKER), Some(count)) => {
                let first = record.sequence.checked_sub(count).ok_or_else(|| HostError::Storage("invalid replacement extent".into()))?;
                if let Some((start, claimed_count)) = pending.take()
                    && (claimed_count != count || Some(first) != start.checked_add(1))
                {
                    return Err(HostError::Storage("incomplete history replacement".into()));
                }
                floor = first;
            }
            (Some(HISTORY_REPLACE_STARTED | HISTORY_REPLACED_MARKER), _) =>
                return Err(HostError::Storage("invalid history replacement".into())),
            _ => {}
        }
    }
    if pending.is_some() {
        return Err(HostError::Storage("uncommitted history replacement".into()));
    }
    Ok(floor)
}

fn canonical_fork_items(
    records: impl Iterator<Item = Result<TranscriptRecord, HostError>>,
    session: &SessionId,
    committed_head: u64,
    expected_thread: Option<&harw_types::ThreadRef>,
) -> Result<Vec<TurnItem>, HostError> {
    let ambiguous = || HostError::Denied("fork source snapshot is incomplete or ambiguous".into());
    let large = || HostError::Denied("fork source exceeds the snapshot limit".into());
    let mut items = Vec::<TurnItem>::new();
    let mut thread = None;
    let mut pending = None;
    let mut previous_committed = 0usize;
    let mut scanned = 0u64;

    for (index, record) in records.enumerate() {
        if index >= FORK_SCAN_MAX_RECORDS {
            return Err(large());
        }
        let record = record?;
        if index as u64 >= committed_head {
            return Err(HostError::Busy(
                "fork source journal has changed; try again".into(),
            ));
        }
        scanned += 1;
        if record.session_id != *session || record.sequence != index as u64
            || expected_thread.is_some_and(|thread| *thread != record.thread)
        {
            return Err(ambiguous());
        }
        let marker = record
            .payload
            .get("marker")
            .and_then(serde_json::Value::as_str);
        if record.kind != RecordKind::Item
            && !(record.kind == RecordKind::Lifecycle
                && matches!(
                    marker,
                    Some(HISTORY_REPLACE_STARTED | HISTORY_REPLACED_MARKER)
                ))
        {
            continue;
        }
        let seen = thread.get_or_insert_with(|| record.thread.clone());
        if seen.as_str() != record.thread.as_str() {
            return Err(ambiguous());
        }
        if record.kind == RecordKind::Item {
            items.push(serde_json::from_value(record.payload).map_err(|_| ambiguous())?);
            continue;
        }
        let count = record
            .payload
            .get("item_count")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(ambiguous)?;
        if marker == Some(HISTORY_REPLACE_STARTED) {
            if pending.replace((items.len(), count)).is_some() || count > FORK_SCAN_MAX_RECORDS {
                return Err(ambiguous());
            }
        } else if marker == Some(HISTORY_REPLACED_MARKER) {
            if let Some((start, expected)) = pending.take() {
                if expected != count || items.len().saturating_sub(start) != count {
                    return Err(ambiguous());
                }
            } else if items.len().saturating_sub(previous_committed) < count {
                return Err(ambiguous());
            }
            items.drain(..items.len() - count);
            previous_committed = items.len();
        }
    }
    if pending.is_some() || scanned != committed_head {
        return Err(ambiguous());
    }
    Ok(items)
}

fn canonical_fork_prefix(
    records: impl Iterator<Item = Result<TranscriptRecord, HostError>>,
    session: &SessionId, before: &str, committed_head: u64,
) -> Result<Vec<TurnItem>, HostError> {
    use harw_protocol::items::ContentPart;
    let ambiguous = || HostError::Denied("fork source snapshot is incomplete or ambiguous".into());
    let large = || HostError::Denied("fork source exceeds the snapshot limit".into());
    let items = canonical_fork_items(records, session, committed_head, None)?;
    let mut unique = std::collections::HashSet::with_capacity(items.len());
    let mut target = None;
    for (offset, item) in items.iter().enumerate() {
        let id = match item {
            TurnItem::UserMessage(m) => m.id.as_str(),
            TurnItem::AssistantMessage(m) => m.id.as_str(),
            TurnItem::Reasoning(m) => m.id.as_str(),
            TurnItem::ToolCall(m) => m.id.as_str(),
            TurnItem::ToolResult(m) => m.id.as_str(),
            TurnItem::Error(m) => m.id.as_str(),
        };
        if !unique.insert(id) {
            return Err(ambiguous());
        }
        if id == before && matches!(item, TurnItem::UserMessage(_)) {
            target = Some(offset);
        }
    }
    let target = target.ok_or_else(|| {
        HostError::Denied(
            "fork target is not an exact canonical user message; archived suffixes are unsupported"
                .into(),
        )
    })?;
    let mut prefix = Vec::new();
    let mut prefix_bytes = 0usize;
    for item in items.into_iter().take(target) {
        let item = match item {
            TurnItem::UserMessage(mut m) => {
                m.content.retain(|part| {
                    matches!(part, ContentPart::Text { .. } | ContentPart::Media { .. })
                });
                TurnItem::UserMessage(m)
            }
            TurnItem::AssistantMessage(mut m) => {
                m.content
                    .retain(|part| matches!(part, ContentPart::Text { .. }));
                TurnItem::AssistantMessage(m)
            }
            _ => continue,
        };
        prefix_bytes =
            prefix_bytes.saturating_add(serde_json::to_vec(&item).map_err(|_| ambiguous())?.len());
        if prefix.len() >= FORK_MAX_ITEMS || prefix_bytes > FORK_PREFIX_MAX_BYTES {
            return Err(large());
        }
        prefix.push(item);
    }
    Ok(prefix)
}

fn verified_displayed_fork(
    items: Vec<TurnItem>,
    selection: &harw_protocol::session_wire::DisplayedForkV1,
) -> Result<Vec<TurnItem>, HostError> {
    use harw_protocol::items::ContentPart;
    use harw_protocol::session_wire::{DISPLAYED_FORK_MAX_ROWS, DisplayedForkRoleV1, displayed_fork_body_v1};
    let changed = || HostError::Denied("Displayed chat changed; refresh it and try Fork again".into());
    let large = || HostError::Denied("The displayed prefix exceeds the safe fork limit; nothing was copied".into());
    if !(1..=DISPLAYED_FORK_MAX_ROWS).contains(&selection.rows.len()) { return Err(large()); }
    let mut actual = std::collections::HashMap::new();
    for (position, row) in items.into_iter().enumerate() {
        let id = match &row {
            TurnItem::UserMessage(m) => m.id.as_str(),
            TurnItem::AssistantMessage(m) => m.id.as_str(),
            _ => continue,
        };
        if actual.insert(id.to_owned(), (position, row)).is_some() { return Err(changed()); }
    }
    let mut previous = None;
    let mut prefix = Vec::with_capacity(selection.rows.len());
    let mut total_bytes = 0usize;
    for wanted in &selection.rows {
        let (position, row) = actual.remove(&wanted.item_id).ok_or_else(changed)?;
        if previous.is_some_and(|p| p >= position) { return Err(changed()); }
        previous = Some(position);
        let (role, content) = match &row {
            TurnItem::UserMessage(m) => (DisplayedForkRoleV1::User, &m.content),
            TurnItem::AssistantMessage(m) => (DisplayedForkRoleV1::Assistant, &m.content),
            _ => return Err(changed()),
        };
        if role != wanted.role { return Err(changed()); }
        let mut media = Vec::new();
        let mut parts = Vec::with_capacity(content.len());
        for part in content {
            match part {
                ContentPart::Text { text } => parts.push(text.as_str()),
                ContentPart::Media { media: reference, .. } if role == DisplayedForkRoleV1::User => {
                    media.push(reference.clone());
                    parts.push("[image]");
                }
                _ => return Err(HostError::Denied("An attachment in the displayed chat is not available for a verified fork".into())),
            }
        }
        // Must be the very row the UI drew, not an older/newer row that reused an ID.
        let body = displayed_fork_body_v1(&parts.join("
"), &media).map_err(|_| changed())?;
        if blake3::hash(&body).as_bytes() != &wanted.body_digest { return Err(changed()); }
        total_bytes = total_bytes.saturating_add(body.len());
        if total_bytes > FORK_PREFIX_MAX_BYTES { return Err(large()); }
        prefix.push(row);
    }
    Ok(prefix)
}

#[cfg(test)]
mod fork_tests;

/// In-memory [`TranscriptSource`] for host tests and fakes.
#[derive(Debug, Default)]
pub struct MemoryTranscripts {
    records: Mutex<HashMap<SessionId, Vec<TranscriptRecord>>>,
}

impl MemoryTranscripts {
    /// Empty source.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `record` to the transcript of `record.session_id`. Records are
    /// kept ordered by sequence even if appended out of order.
    pub fn append(&self, record: TranscriptRecord) {
        let mut records = self.lock();
        let transcript = records.entry(record.session_id.clone()).or_default();
        let out_of_order = transcript
            .last()
            .is_some_and(|last| last.sequence > record.sequence);
        transcript.push(record);
        if out_of_order {
            transcript.sort_by_key(|record| record.sequence);
        }
    }

    // A poisoned lock only means another test thread panicked mid-append;
    // the vector itself is still consistent, so keep serving it.
    fn lock(&self) -> MutexGuard<'_, HashMap<SessionId, Vec<TranscriptRecord>>> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl TranscriptSource for MemoryTranscripts {
    fn read_from(
        &self,
        session: &SessionId,
        from_sequence: u64,
        limit: usize,
    ) -> Result<Vec<TranscriptRecord>, HostError> {
        let records = self.lock();
        Ok(records
            .get(session)
            .map(|transcript| {
                transcript
                    .iter()
                    .filter(|record| record.sequence >= from_sequence)
                    .take(limit.min(MAX_PAGE))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    fn head(&self, session: &SessionId) -> Result<u64, HostError> {
        let records = self.lock();
        Ok(records
            .get(session)
            .and_then(|transcript| transcript.last())
            .map_or(0, |last| last.sequence.saturating_add(1)))
    }
}

/// Deterministic turn id for a replayed item: `replay-<sequence>`.
///
/// The runtime persists bare [`TurnItem`]s without their turn id, so replay
/// cannot recover the original one. The fallback is stable across attaches
/// and hosts, so the same record always yields the same frame.
#[must_use]
pub fn replay_turn_id(sequence: u64) -> Option<TurnId> {
    TurnId::try_from_str(format!("replay-{sequence}")).ok()
}

/// Durable frame for one transcript record, or `None` when the record is not
/// part of the client-visible stream (see the module docs for the mapping).
#[must_use]
pub fn record_to_frame(record: &TranscriptRecord) -> Option<SessionFrame> {
    match record.kind {
        RecordKind::Item => {
            let item = match serde_json::from_value::<TurnItem>(record.payload.clone()) {
                Ok(item) => item,
                Err(error) => {
                    tracing::warn!(
                        session = %record.session_id,
                        sequence = record.sequence,
                        %error,
                        "session_host.replay_item_undecodable"
                    );
                    return None;
                }
            };
            let turn_id = replay_turn_id(record.sequence)?;
            Some(SessionFrame::Turn(TurnEvent::ItemAdded { turn_id, item }))
        }
        RecordKind::Lifecycle => {
            let is_marker = record
                .payload
                .get("marker")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|marker| marker == HISTORY_REPLACED_MARKER);
            if !is_marker {
                return None;
            }
            let count = record
                .payload
                .get("item_count")
                .and_then(serde_json::Value::as_u64)?;
            Some(SessionFrame::HistoryReplaced {
                item_count: u32::try_from(count).unwrap_or(u32::MAX),
            })
        }
        RecordKind::Turn | RecordKind::ToolCall => None,
    }
}

/// Envelope for `record`, if it maps to a frame.
fn envelope(
    record: &TranscriptRecord,
    session: &SessionId,
    generation: u32,
) -> Option<FrameEnvelope> {
    record_to_frame(record).map(|frame| FrameEnvelope {
        session_id: session.clone(),
        cursor: Cursor {
            generation,
            durable: record.sequence.saturating_add(1),
            live: 0,
        },
        frame,
    })
}

/// Durable frames from `from` (a transcript sequence), at most `limit`. Each
/// frame's cursor is Cursor { generation, durable: record.sequence + 1, live: 0 }
/// (i.e. "next unseen"). Records that map to None are skipped but still count
/// toward reading progress.
pub fn replay(
    source: &dyn TranscriptSource,
    session: &SessionId,
    generation: u32,
    from: u64,
    limit: usize,
) -> Result<Vec<FrameEnvelope>, HostError> {
    Ok(replay_page(source, session, generation, from, limit)?.frames)
}

pub(crate) struct ReplayPage {
    pub frames: Vec<FrameEnvelope>,
    /// Next record after the last **scanned** record, independent of filtering.
    pub next_sequence: u64,
}

/// Same bounded replay scan as [`replay`], with the underlying transcript progress.
pub(crate) fn replay_page(
    source: &dyn TranscriptSource,
    session: &SessionId,
    generation: u32,
    from: u64,
    limit: usize,
) -> Result<ReplayPage, HostError> {
    let limit = limit.min(MAX_PAGE);
    let mut page = ReplayPage {
        frames: Vec::new(),
        next_sequence: from,
    };
    if limit == 0 {
        return Ok(page);
    }
    for record in source.read_from(session, from, limit)?.iter().take(limit) {
        page.next_sequence = page.next_sequence.max(record.sequence.saturating_add(1));
        if let Some(frame) = envelope(record, session, generation) {
            page.frames.push(frame);
        }
    }
    Ok(page)
}

/// First sequence to send for an attach without cursor: head.saturating_sub(tail_items).
#[must_use]
pub fn tail_start(head: u64, tail_items: u32) -> u64 {
    head.saturating_sub(u64::from(tail_items))
}

/// Older page for session.history: frames with sequence < before.durable, the last `limit` of them, ascending.
///
/// Walks backwards in windows of at least `limit` records until `limit`
/// frames are found or the transcript start is reached, so skipped records
/// never shorten a page that older records could still fill.
pub fn history_before(
    source: &dyn TranscriptSource,
    session: &SessionId,
    generation: u32,
    before: u64,
    limit: usize,
) -> Result<Vec<FrameEnvelope>, HostError> {
    let limit = limit.min(MAX_PAGE);
    if limit == 0 {
        return Ok(Vec::new());
    }
    let window = u64::try_from(limit.max(64)).unwrap_or(u64::MAX);
    // Collected newest window first; each window is ascending.
    let mut windows: Vec<Vec<FrameEnvelope>> = Vec::new();
    let mut found = 0_usize;
    let head = source.head(session)?;
    let floor = source.canonical_floor(session, head)?;
    let mut high = before.min(head);
    while high > floor && found < limit {
        let low = high.saturating_sub(window).max(floor);
        let span = usize::try_from(high - low).unwrap_or(MAX_PAGE);
        let frames: Vec<FrameEnvelope> = source
            .read_from(session, low, span)?
            .iter()
            .filter(|record| record.sequence >= low && record.sequence < high)
            .filter_map(|record| envelope(record, session, generation))
            .collect();
        found = found.saturating_add(frames.len());
        windows.push(frames);
        high = low;
    }
    let mut frames: Vec<FrameEnvelope> = windows.into_iter().rev().flatten().collect();
    let excess = frames.len().saturating_sub(limit);
    frames.drain(..excess);
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use harw_protocol::items::{AssistantMessageItem, ContentPart, UserMessageItem};
    use harw_types::{ItemId, ThreadRef};
    use jiff::Timestamp;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn session() -> SessionId {
        SessionId::from_str("session-a")
    }

    fn record(sequence: u64, kind: RecordKind, payload: serde_json::Value) -> TranscriptRecord {
        TranscriptRecord::new(
            session(),
            ThreadRef::from_str("root"),
            sequence,
            Timestamp::UNIX_EPOCH,
            kind,
            payload,
        )
    }

    /// `RecordKind::Item` exactly as `TranscriptStateStore::save_turn` writes it.
    fn item_record(sequence: u64, text: &str) -> Result<TranscriptRecord, serde_json::Error> {
        let item = TurnItem::UserMessage(UserMessageItem {
            id: ItemId::from_str(format!("item-{sequence}")),
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
        });
        Ok(record(
            sequence,
            RecordKind::Item,
            serde_json::to_value(item)?,
        ))
    }

    /// `history_replaced` marker as `TranscriptStateStore::save_history` writes it.
    fn history_replaced_record(sequence: u64, item_count: u64) -> TranscriptRecord {
        record(
            sequence,
            RecordKind::Lifecycle,
            serde_json::json!({ "marker": "history_replaced", "item_count": item_count }),
        )
    }

    /// Session-state snapshot as `TranscriptStateStore::save_session_state` writes it.
    fn session_state_record(sequence: u64) -> TranscriptRecord {
        record(
            sequence,
            RecordKind::Lifecycle,
            serde_json::json!({
                "kind": "core_session_state",
                "state": {
                    "version": 1,
                    "mode": "default",
                    "total_usage": {},
                    "executable_snapshot_id": null,
                    "base_activation": { "profile": "coding", "enabled_tools": [], "disabled_tools": [] },
                    "activation": { "profile": "coding", "enabled_tools": [], "disabled_tools": [] }
                }
            }),
        )
    }

    /// Transcript with items at 0, 1, 3, 4, a state snapshot at 2 and a marker at 5.
    fn memory_fixture() -> Result<MemoryTranscripts, serde_json::Error> {
        let source = MemoryTranscripts::new();
        source.append(item_record(0, "zero")?);
        source.append(item_record(1, "one")?);
        source.append(session_state_record(2));
        source.append(item_record(3, "three")?);
        source.append(item_record(4, "four")?);
        source.append(history_replaced_record(5, 2));
        Ok(source)
    }

    fn durables(frames: &[FrameEnvelope]) -> Vec<u64> {
        frames.iter().map(|frame| frame.cursor.durable).collect()
    }

    #[test]
    fn item_record_maps_to_item_added_with_replay_turn_id() -> TestResult {
        let Some(frame) = record_to_frame(&item_record(7, "hello")?) else {
            return Err("item record must map to a frame".into());
        };
        let SessionFrame::Turn(TurnEvent::ItemAdded { turn_id, item }) = frame else {
            return Err(format!("expected ItemAdded, got {frame:?}").into());
        };
        assert_eq!(turn_id.as_str(), "replay-7");
        let TurnItem::UserMessage(message) = item else {
            return Err("expected the user message item".into());
        };
        assert_eq!(message.id.as_str(), "item-7");
        assert!(matches!(
            message.content.as_slice(),
            [ContentPart::Text { text }] if text == "hello"
        ));
        Ok(())
    }

    #[test]
    fn assistant_item_record_maps_too() -> TestResult {
        let item = TurnItem::AssistantMessage(AssistantMessageItem {
            id: ItemId::from_str("a-1"),
            content: vec![ContentPart::Text {
                text: "answer".to_owned(),
            }],
            phase: None,
        });
        let frame = record_to_frame(&record(1, RecordKind::Item, serde_json::to_value(item)?));
        assert!(matches!(
            frame,
            Some(SessionFrame::Turn(TurnEvent::ItemAdded {
                item: TurnItem::AssistantMessage(_),
                ..
            }))
        ));
        Ok(())
    }

    #[test]
    fn history_replaced_marker_maps_to_history_replaced() {
        let frame = record_to_frame(&history_replaced_record(3, 12));
        assert!(matches!(
            frame,
            Some(SessionFrame::HistoryReplaced { item_count: 12 })
        ));
        let huge = record_to_frame(&history_replaced_record(3, u64::MAX));
        assert!(matches!(
            huge,
            Some(SessionFrame::HistoryReplaced {
                item_count: u32::MAX
            })
        ));
    }

    #[test]
    fn non_client_records_are_skipped() {
        assert!(record_to_frame(&session_state_record(0)).is_none());
        let drift = record(
            1,
            RecordKind::Lifecycle,
            serde_json::json!({ "marker": "drift", "kind": "x", "session_id": "s",
                "detail": "d", "tool_name": null, "child_role": null }),
        );
        assert!(record_to_frame(&drift).is_none());
        let gateway = record(
            2,
            RecordKind::Lifecycle,
            serde_json::json!({ "event": "opened" }),
        );
        assert!(record_to_frame(&gateway).is_none());
        let usage = record(3, RecordKind::Turn, serde_json::json!({ "round": 1 }));
        assert!(record_to_frame(&usage).is_none());
        let garbage = record(4, RecordKind::Item, serde_json::json!({ "type": "nope" }));
        assert!(record_to_frame(&garbage).is_none());
    }

    // RP-01: replay from a mid cursor returns exactly the suffix.
    #[test]
    fn replay_from_mid_cursor_returns_exact_suffix() -> TestResult {
        let source = memory_fixture()?;
        let frames = replay(&source, &session(), 3, 2, 100)?;
        // Sequence 2 (session state) is skipped; 3, 4, 5 remain.
        assert_eq!(durables(&frames), vec![4, 5, 6]);
        for frame in &frames {
            assert_eq!(frame.session_id, session());
            assert_eq!(frame.cursor.generation, 3);
            assert_eq!(frame.cursor.live, 0);
        }
        assert!(matches!(
            frames.last().map(|frame| &frame.frame),
            Some(SessionFrame::HistoryReplaced { item_count: 2 })
        ));

        // Resuming from the last cursor yields nothing new.
        let Some(last) = frames.last() else {
            return Err("replay must yield frames".into());
        };
        assert!(replay(&source, &session(), 3, last.cursor.durable, 100)?.is_empty());
        Ok(())
    }

    #[test]
    fn replay_limit_counts_skipped_records() -> TestResult {
        let source = memory_fixture()?;
        // Records 1 and 2 are read; 2 is skipped, so one frame comes back and
        // the next page starts at 3 (a cursor that is still correct).
        let frames = replay(&source, &session(), 0, 1, 2)?;
        assert_eq!(durables(&frames), vec![2]);
        assert!(replay(&source, &session(), 0, 0, 0)?.is_empty());
        Ok(())
    }

    #[test]
    fn tail_start_saturates() {
        assert_eq!(tail_start(10, 3), 7);
        assert_eq!(tail_start(2, 5), 0);
        assert_eq!(tail_start(0, 0), 0);
    }

    #[test]
    fn canonical_history_does_not_page_back_into_a_retained_pre_edit_suffix() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        store.append(&item_record(0, "old prefix")?)?;
        store.append(&item_record(1, "old suffix")?)?;
        store.append(&item_record(2, "replacement prefix")?)?;
        store.append(&item_record(3, "replacement prompt")?)?;
        store.append(&history_replaced_record(4, 2))?;
        store.append(&item_record(5, "current answer")?)?;
        let source = StoreTranscripts::new(store);
        assert_eq!(source.canonical_floor(&session(), 6)?, 2);
        assert!(history_before(&source, &session(), 0, 2, 100)?.is_empty());
        assert_eq!(durables(&history_before(&source, &session(), 0, 6, 100)?), [3, 4, 5, 6]);
        Ok(())
    }

    #[test]
    fn history_before_pages_backwards() -> TestResult {
        let source = memory_fixture()?;
        // Frames below durable 5 are sequences 0, 1, 3, 4; the last two.
        let page = history_before(&source, &session(), 1, 5, 2)?;
        assert_eq!(durables(&page), vec![4, 5]);
        // Next older page before the first frame's sequence (3).
        let older = history_before(&source, &session(), 1, 3, 2)?;
        assert_eq!(durables(&older), vec![1, 2]);
        assert!(history_before(&source, &session(), 1, 0, 2)?.is_empty());
        assert!(history_before(&source, &session(), 1, 5, 0)?.is_empty());
        Ok(())
    }

    #[test]
    fn history_before_spans_windows_past_skipped_records() -> TestResult {
        let source = MemoryTranscripts::new();
        source.append(item_record(0, "old")?);
        for sequence in 1..200 {
            source.append(session_state_record(sequence));
        }
        source.append(item_record(200, "new")?);
        let page = history_before(&source, &session(), 0, 201, 2)?;
        assert_eq!(durables(&page), vec![1, 201]);
        Ok(())
    }

    #[test]
    fn memory_head_and_out_of_order_append() -> TestResult {
        let source = MemoryTranscripts::new();
        assert_eq!(source.head(&session())?, 0);
        source.append(item_record(1, "b")?);
        source.append(item_record(0, "a")?);
        assert_eq!(source.head(&session())?, 2);
        let records = source.read_from(&session(), 0, 10)?;
        let sequences: Vec<u64> = records.iter().map(|record| record.sequence).collect();
        assert_eq!(sequences, vec![0, 1]);
        Ok(())
    }

    #[test]
    fn store_transcripts_reads_a_real_transcript() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        store.append(&item_record(0, "zero")?)?;
        store.append(&session_state_record(1))?;
        store.append(&item_record(2, "two")?)?;
        store.append(&history_replaced_record(3, 1))?;
        let source = StoreTranscripts::new(store);

        assert_eq!(source.head(&session())?, 4);
        let records = source.read_from(&session(), 1, 2)?;
        let sequences: Vec<u64> = records.iter().map(|record| record.sequence).collect();
        assert_eq!(sequences, vec![1, 2]);

        let frames = replay(&source, &session(), 0, 0, 100)?;
        assert_eq!(durables(&frames), vec![1, 3, 4]);
        let older = history_before(&source, &session(), 0, 3, 10)?;
        assert_eq!(durables(&older), vec![1, 3]);
        Ok(())
    }

    #[test]
    fn store_transcripts_missing_transcript_is_empty() -> TestResult {
        let temp = tempfile::tempdir()?;
        let source = StoreTranscripts::new(TranscriptStore::new(temp.path()));
        assert_eq!(source.head(&session())?, 0);
        assert!(source.read_from(&session(), 0, 10)?.is_empty());
        assert!(replay(&source, &session(), 0, 0, 10)?.is_empty());
        Ok(())
    }
}
