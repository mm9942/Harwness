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
    let limit = limit.min(MAX_PAGE);
    if limit == 0 {
        return Ok(Vec::new());
    }
    Ok(source
        .read_from(session, from, limit)?
        .iter()
        .take(limit)
        .filter_map(|record| envelope(record, session, generation))
        .collect())
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
    let mut high = before;
    while high > 0 && found < limit {
        let low = high.saturating_sub(window);
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
