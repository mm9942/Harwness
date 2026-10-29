//! `injection` event record: one triggered skill excerpt delivered into a
//! model request (PL-95 S3, measurement §3.5).
//!
//! # Responsibility
//! The turn loop of `harw-core` injects deterministic, budgeted skill
//! excerpts (section `skills.triggered`) into the per-round context. Every
//! delivery is recorded once in the session transcript with the skill, the
//! chosen section, the trigger kind and key, the delivered bytes and the turn.
//! The record carries no skill text; the SHA-256 of the full skill text names
//! the exact version that was delivered.
//!
//! # Wire form
//! The record is stored as a [`crate::RecordKind::Lifecycle`] payload with
//! `"marker": "skill_injection"` — the same pattern as the other core
//! markers (`history_replaced`, `drift`, `agent_orchestration`). A new
//! `RecordKind` variant would make an older reader fail on the whole
//! transcript (`RecordKind` is a closed enum), while an unknown lifecycle
//! marker is skipped by every existing reader. Mixed binary versions on one
//! machine therefore keep loading each other's sessions.
//!
//! # Privacy
//! Nothing leaves the session store; the data stays local (PL-95 §3.5).

use serde::{Deserialize, Serialize};

use crate::error::SessionStoreResult;

/// Value of the `marker` key that identifies an [`InjectionRecord`] payload.
pub const INJECTION_MARKER: &str = "skill_injection";

/// One delivered triggered skill excerpt.
///
/// # Examples
/// ```rust
/// use harw_session_store::InjectionRecord;
///
/// let record = InjectionRecord {
///     skill: "rust-borrow".to_owned(),
///     section: Some("Fix".to_owned()),
///     trigger_kind: "error_code".to_owned(),
///     trigger_key: "E0505".to_owned(),
///     bytes: 812,
///     turn_id: "turn-1".to_owned(),
///     trust: "instruction".to_owned(),
///     sha256: "ab".repeat(32),
/// };
/// let payload = record.to_payload()?;
/// assert_eq!(payload["marker"], "skill_injection");
/// assert_eq!(InjectionRecord::from_payload(&payload), Some(record));
/// # Ok::<(), harw_session_store::SessionStoreError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InjectionRecord {
    /// Skill name from the manifest.
    pub skill: String,
    /// Heading of the delivered `##` section; `None` = the whole skill.
    pub section: Option<String>,
    /// Trigger kind (`keyword`, `error_code`, `tool`, `path`, `command`).
    pub trigger_kind: String,
    /// The manifest entry that matched (e.g. `E0505`).
    pub trigger_key: String,
    /// Bytes of the delivered, framed excerpt.
    pub bytes: u64,
    /// Turn in which the excerpt was delivered first.
    pub turn_id: String,
    /// Trust class of the fragment (`instruction` or `evidence`).
    pub trust: String,
    /// SHA-256 of the full skill text (hex).
    pub sha256: String,
}

impl InjectionRecord {
    /// Encodes the record as a lifecycle payload with
    /// `"marker": "skill_injection"`.
    ///
    /// # Errors
    /// [`crate::SessionStoreError`] when serialization fails (not expected for
    /// this plain data type).
    pub fn to_payload(&self) -> SessionStoreResult<serde_json::Value> {
        let mut payload = serde_json::to_value(self)?;
        if let serde_json::Value::Object(object) = &mut payload {
            object.insert("marker".to_owned(), serde_json::json!(INJECTION_MARKER));
        }
        Ok(payload)
    }

    /// Decodes a lifecycle payload; `None` when it is not an injection
    /// record (another marker, no marker, or a malformed body).
    #[must_use]
    pub fn from_payload(payload: &serde_json::Value) -> Option<Self> {
        if !is_injection_payload(payload) {
            return None;
        }
        let mut body = payload.as_object()?.clone();
        body.remove("marker");
        serde_json::from_value(serde_json::Value::Object(body)).ok()
    }
}

/// Whether `payload` carries the [`INJECTION_MARKER`].
#[must_use]
pub fn is_injection_payload(payload: &serde_json::Value) -> bool {
    payload
        .get("marker")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|marker| marker == INJECTION_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use crate::{RecordKind, TranscriptRecord};
    use harw_types::{SessionId, ThreadRef};

    fn record() -> InjectionRecord {
        InjectionRecord {
            skill: "cargo-tools".to_owned(),
            section: None,
            trigger_kind: "command".to_owned(),
            trigger_key: "cargo test".to_owned(),
            bytes: 1024,
            turn_id: "turn-7".to_owned(),
            trust: "evidence".to_owned(),
            sha256: "0f".repeat(32),
        }
    }

    #[test]
    fn payload_round_trips_through_a_transcript_line() -> TestResult {
        let payload = record().to_payload()?;
        let line = TranscriptRecord::new(
            SessionId::from_str("session-a"),
            ThreadRef::from_str("root"),
            3,
            jiff::Timestamp::now(),
            RecordKind::Lifecycle,
            payload,
        )
        .to_jsonl_line()?;
        let decoded = TranscriptRecord::from_jsonl_line(&line)?;
        assert_eq!(decoded.kind, RecordKind::Lifecycle);
        let Some(back) = InjectionRecord::from_payload(&decoded.payload) else {
            return Err(TestError::Unexpected("injection payload".to_owned()));
        };
        assert_eq!(back, record());
        Ok(())
    }

    #[test]
    fn other_markers_and_malformed_bodies_are_not_injections() {
        let drift = serde_json::json!({ "marker": "drift", "skill": "x" });
        assert!(!is_injection_payload(&drift));
        assert_eq!(InjectionRecord::from_payload(&drift), None);
        let malformed = serde_json::json!({ "marker": INJECTION_MARKER, "skill": 1 });
        assert!(is_injection_payload(&malformed));
        assert_eq!(InjectionRecord::from_payload(&malformed), None);
        assert_eq!(
            InjectionRecord::from_payload(&serde_json::json!(null)),
            None
        );
    }
}
