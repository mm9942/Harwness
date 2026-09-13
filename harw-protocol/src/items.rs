//! Item-/Message-Modelle: ein `TurnItem` ist ein einzelnes inhaltliches
//! Element innerhalb eines Turns (intern getaggtes Enum).

use harw_types::{ItemId, MessagePhase, ToolCallId};
use serde::{Deserialize, Serialize};

/// Ein einzelnes inhaltliches Element innerhalb eines Turns.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "snake_case")]
pub enum TurnItem {
    UserMessage(UserMessageItem),
    AssistantMessage(AssistantMessageItem),
    ToolCall(ToolCallItem),
    ToolResult(ToolResultItem),
    Reasoning(ReasoningItem),
    Error(ErrorItem),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessageItem {
    pub id: ItemId,
    pub content: Vec<ContentPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantMessageItem {
    pub id: ItemId,
    pub content: Vec<ContentPart>,
    /// Optionale Phasen-Metadaten (commentary vs. final_answer).
    pub phase: Option<MessagePhase>,
}

/// Inhalt eines Nachrichten-Items — Text oder base64/URL-Bild.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { url: String, detail: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallItem {
    pub id: ItemId,
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// Explicit, stable outcome of one tool invocation.
///
/// This is deliberately not Rust's `Result` serialization. The tagged wire
/// shape is part of the persisted transcript and cross-process event contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "status", rename_all = "snake_case")]
pub enum ToolCallResult {
    /// The tool completed and returned structured data.
    Success { value: serde_json::Value },
    /// The tool declined or failed with a user-safe diagnostic.
    Error { message: String },
}

impl ToolCallResult {
    /// Constructs a successful tool outcome.
    #[must_use]
    pub fn success(value: serde_json::Value) -> Self {
        Self::Success { value }
    }

    /// Constructs a failed tool outcome.
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }

    /// Whether this outcome contains successful tool data.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultItem {
    pub id: ItemId,
    pub call_id: ToolCallId,
    /// Explicit success/error wire outcome; stable across Rust versions.
    pub result: ToolCallResult,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningItem {
    pub id: ItemId,
    /// Zusammenfassung des Denkprozesses (sichtbar für Nutzer).
    pub summary_text: Vec<String>,
    /// Rohinhalt (nur bei aktiviertem Debug-Modus).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_content: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorItem {
    pub id: ItemId,
    pub message: String,
    pub retryable: bool,
}

#[cfg(test)]
mod tests {
    use super::ToolCallResult;
    use serde_json::json;

    #[test]
    fn tool_call_result_uses_an_explicit_success_wire_shape() {
        let result = ToolCallResult::success(json!({"temperature": 20}));

        assert_eq!(
            serde_json::to_value(result).expect("tool outcome serializes"),
            json!({"status": "success", "value": {"temperature": 20}})
        );
    }

    #[test]
    fn tool_call_result_uses_an_explicit_error_wire_shape() {
        let result = ToolCallResult::error("tool unavailable");

        assert_eq!(
            serde_json::to_value(result).expect("tool outcome serializes"),
            json!({"status": "error", "message": "tool unavailable"})
        );
    }

    #[test]
    fn tool_call_result_rejects_unknown_wire_fields() {
        let malformed = json!({
            "status": "success",
            "value": {"temperature": 20},
            "unexpected": true,
        });

        assert!(serde_json::from_value::<ToolCallResult>(malformed).is_err());
    }
}
