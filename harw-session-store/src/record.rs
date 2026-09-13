//! Transcript record type and JSONL line codec.
//!
//! Spec: `docs/design/knowledge-surfaces.md` §1.4 — the store owns the
//! sequential, replayable record of turns, tool calls and model outputs for a
//! `SessionId`/`ThreadId`. Records are append-only and carry provenance
//! (`SessionId` + `ThreadRef`) and a `jiff::Timestamp`. The turn/tool/item
//! payload is kept as opaque `serde_json::Value` so the store stays decoupled
//! from `harw-core`'s turn types.

use harw_types::{SessionId, ThreadRef};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::{SessionStoreError, SessionStoreResult};

/// What a transcript record body represents (§1.4 unit: Turn / ToolCall / Item).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    /// A user/model/system turn message.
    Turn,
    /// A tool invocation request or its result.
    ToolCall,
    /// An individual turn item (reasoning, message fragment, …).
    Item,
    /// A session lifecycle marker (opened / closed).
    Lifecycle,
}

/// One append-only transcript entry for a session's replay log.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TranscriptRecord {
    /// Owning session (primary provenance and file-partition key).
    pub session_id: SessionId,
    /// External thread reference this record belongs to (§1.4 provenance).
    pub thread: ThreadRef,
    /// Monotonic per-session ordering index (0-based append position).
    pub sequence: u64,
    /// Wall-clock time the record was captured.
    pub recorded_at: Timestamp,
    /// Discriminant describing the payload shape.
    pub kind: RecordKind,
    /// Opaque serialized body (turn / tool-call / item), decoded by the reader.
    pub payload: serde_json::Value,
}

impl<'de> Deserialize<'de> for TranscriptRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireTranscriptRecord {
            session_id: SessionId,
            thread: ThreadRef,
            sequence: u64,
            recorded_at: Timestamp,
            kind: RecordKind,
            payload: serde_json::Value,
        }

        let record = WireTranscriptRecord::deserialize(deserializer)?;
        if record.session_id.as_str().is_empty() {
            return Err(serde::de::Error::custom("session_id must not be empty"));
        }
        if record.thread.as_str().is_empty() {
            return Err(serde::de::Error::custom("thread must not be empty"));
        }
        if record.payload.is_null() {
            return Err(serde::de::Error::custom("payload must not be null"));
        }

        Ok(Self {
            session_id: record.session_id,
            thread: record.thread,
            sequence: record.sequence,
            recorded_at: record.recorded_at,
            kind: record.kind,
            payload: record.payload,
        })
    }
}

impl TranscriptRecord {
    /// Builds a record from its provenance, ordering, kind and payload.
    #[must_use]
    pub fn new(
        session_id: SessionId,
        thread: ThreadRef,
        sequence: u64,
        recorded_at: Timestamp,
        kind: RecordKind,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            session_id,
            thread,
            sequence,
            recorded_at,
            kind,
            payload,
        }
    }

    /// Encodes the record as a single newline-terminated JSONL line.
    pub fn to_jsonl_line(&self) -> SessionStoreResult<String> {
        let mut line = serde_json::to_string(self)?;
        line.push('\n');
        Ok(line)
    }

    /// Decodes one JSONL line into a record, mapping failures to `CorruptRecord`.
    pub fn from_jsonl_line(line: &str) -> SessionStoreResult<Self> {
        serde_json::from_str(line).map_err(|e| SessionStoreError::CorruptRecord {
            detail: e.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_types::ThreadRef;

    fn valid_json() -> serde_json::Value {
        serde_json::json!({
            "session_id": "session-a",
            "thread": "root",
            "sequence": 0,
            "recorded_at": "2026-07-18T07:59:00Z",
            "kind": "turn",
            "payload": { "text": "hello" }
        })
    }

    #[test]
    fn deserialization_rejects_null_payload() {
        let mut value = valid_json();
        value["payload"] = serde_json::Value::Null;

        let error = TranscriptRecord::from_jsonl_line(&value.to_string()).unwrap_err();

        assert!(matches!(
            error,
            SessionStoreError::CorruptRecord { detail } if detail.contains("payload must not be null")
        ));
    }

    #[test]
    fn deserialization_preserves_session_id_validation() {
        let mut value = valid_json();
        value["session_id"] = serde_json::Value::String(String::new());

        let error = TranscriptRecord::from_jsonl_line(&value.to_string()).unwrap_err();

        assert!(matches!(
            error,
            SessionStoreError::CorruptRecord { detail }
                if detail.contains("SessionId must not be empty or whitespace-only")
        ));
    }

    #[test]
    fn valid_record_still_round_trips() {
        let record = TranscriptRecord::new(
            SessionId::from_str("session-a"),
            ThreadRef::from_str("root"),
            0,
            Timestamp::now(),
            RecordKind::Turn,
            serde_json::json!({ "text": "hello" }),
        );

        let decoded = TranscriptRecord::from_jsonl_line(&record.to_jsonl_line().unwrap()).unwrap();

        assert_eq!(decoded, record);
    }
}
