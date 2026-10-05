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
    Text {
        text: String,
    },
    /// An image by URL. Providers do **not** forward it: the harness sends
    /// only bytes it holds itself (see [`ContentPart::Media`]), never a URL a
    /// third party would have to fetch.
    ImageUrl {
        url: String,
        detail: Option<String>,
    },
    /// An image the media store holds, by reference. This is the form a user
    /// attachment or a tool-produced image takes in a transcript; provider
    /// adapters load the bytes through the media source when they build a
    /// request. Older readers without this variant fail to parse an item that
    /// carries it; items without images are unchanged.
    Media {
        /// The stored image.
        media: crate::media::MediaRef,
        /// How much detail a provider should spend, if the sender cares.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<crate::media::ImageDetail>,
    },
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

/// Provenance class of a tool result (W3 C-PROTO, findings F-170/F-111).
///
/// # Description
/// Decides how a tool result is presented to the model: `Untrusted` results
/// are wrapped in the untrusted envelope rendered by
/// `harw_core::envelope::render_tool_result` (unique header/footer, fenced
/// content lines, notice); `Runtime` results were produced by the harness
/// itself (synthetic cancellations, admission denials) and are rendered
/// without the envelope but still byte-capped.
///
/// The default is `Untrusted`, so every transcript persisted before this
/// field existed deserializes fail-closed.
///
/// # Concurrency
/// Plain `Copy` value, `Send + Sync`.
///
/// # Examples
/// ```rust
/// use harw_protocol::items::ResultTrust;
///
/// assert_eq!(ResultTrust::default(), ResultTrust::Untrusted);
/// assert_eq!(serde_json::to_string(&ResultTrust::Runtime)?, "\"runtime\"");
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultTrust {
    /// Content originates outside the harness (web, filesystem, MCP, child
    /// agents, shell output) and may carry prompt-injection attempts.
    #[default]
    Untrusted,
    /// Content was synthesized by the harness runtime itself.
    Runtime,
}

/// Where a tool call was actually executed (R18 D-D, contract
/// `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md` §5).
///
/// # Description
/// Surfaces show the placement instead of a shell command line. The value is
/// always set by the executing side (the runtime or the gateway tool host),
/// never by the model or a request payload.
///
/// - `Host`: ran on the local host outside a sandbox (approved host
///   escalation).
/// - `Sandbox`: ran in the local sandbox of the agent process itself.
/// - `Gateway`: ran in the gateway tool host, inside a gateway-side sandbox;
///   `node` names the gateway node when known.
/// - `Unknown`: a placement kind this version does not know (additive
///   evolution); render it neutrally, never as `Host`.
///
/// # Concurrency
/// Plain owned value, `Send + Sync`.
///
/// # Examples
/// ```rust
/// use harw_protocol::items::ToolPlacement;
///
/// let placement = ToolPlacement::Gateway { node: Some("gw-1".to_owned()) };
/// let json = serde_json::to_value(&placement)?;
/// assert_eq!(json, serde_json::json!({"kind": "gateway", "node": "gw-1"}));
/// let future: ToolPlacement = serde_json::from_value(serde_json::json!({"kind": "zellhost", "pod": "p"}))?;
/// assert_eq!(future, ToolPlacement::Unknown);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolPlacement {
    /// Local host, outside any sandbox.
    Host,
    /// Local sandbox of the agent process.
    Sandbox,
    /// Gateway tool host (always sandboxed on the gateway side).
    Gateway {
        /// Gateway node label, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<String>,
    },
    /// A placement kind this version does not know.
    #[serde(other)]
    Unknown,
}

impl ToolPlacement {
    /// Short, stable label for surfaces (`host`, `sandbox`, `gateway`,
    /// `unknown`).
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Sandbox => "sandbox",
            Self::Gateway { .. } => "gateway",
            Self::Unknown => "unknown",
        }
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
    /// Provenance of `result`. Missing in transcripts written before W3;
    /// `#[serde(default)]` reads those as [`ResultTrust::Untrusted`].
    #[serde(default)]
    pub trust: ResultTrust,
}

/// Provider-opaque reasoning blocks that must be replayed verbatim (W3 C-PROTO).
///
/// # Description
/// Some providers (e.g. Anthropic `thinking`/`redacted_thinking`, OpenAI
/// reasoning items) require the exact blocks of a previous response to be sent
/// back unchanged on the next request. The harness never interprets `blocks`;
/// it only stores them together with the `provider` and `model` that produced
/// them, so that a replay to a different provider/model can be skipped.
///
/// # Concurrency
/// Plain owned value, `Send + Sync`.
///
/// # Examples
/// ```rust
/// use harw_protocol::items::OpaqueReasoning;
///
/// let reasoning = OpaqueReasoning {
///     provider: "anthropic".to_owned(),
///     model: "claude-opus-5".to_owned(),
///     blocks: vec![serde_json::json!({"type": "redacted_thinking", "data": "..."})],
/// };
/// let json = serde_json::to_value(&reasoning)?;
/// let back: OpaqueReasoning = serde_json::from_value(json)?;
/// assert_eq!(back, reasoning);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpaqueReasoning {
    /// Provider id that produced `blocks` (e.g. `"anthropic"`).
    pub provider: String,
    /// Model id that produced `blocks`.
    pub model: String,
    /// Raw provider blocks, replayed byte-for-byte in JSON value form.
    pub blocks: Vec<serde_json::Value>,
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
    use super::{
        OpaqueReasoning, ResultTrust, ToolCallResult, ToolPlacement, ToolResultItem, TurnItem,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use serde_json::json;

    #[test]
    fn test_result_trust_default_is_untrusted() {
        assert_eq!(ResultTrust::default(), ResultTrust::Untrusted);
    }

    #[test]
    fn test_result_trust_wire_names_are_snake_case() -> TestResult {
        assert_eq!(
            serde_json::to_value(ResultTrust::Untrusted).map_err(ctx("serializes"))?,
            json!("untrusted")
        );
        assert_eq!(
            serde_json::to_value(ResultTrust::Runtime).map_err(ctx("serializes"))?,
            json!("runtime")
        );
        assert!(serde_json::from_value::<ResultTrust>(json!("trusted")).is_err());
        Ok(())
    }

    /// A transcript line written before W3 has no `trust` field and must stay
    /// readable; it is read fail-closed as `Untrusted`.
    #[test]
    fn test_tool_result_item_deserializes_legacy_item_without_trust_as_untrusted() -> TestResult {
        let legacy = json!({
            "type": "tool_result",
            "id": "item-1",
            "call_id": "call-1",
            "result": {"status": "success", "value": {"ok": true}},
            "duration_ms": 7,
        });

        let item: TurnItem =
            serde_json::from_value(legacy).map_err(ctx("legacy item stays readable"))?;

        match item {
            TurnItem::ToolResult(result) => {
                assert_eq!(result.trust, ResultTrust::Untrusted);
                assert_eq!(result.duration_ms, 7);
                assert_eq!(result.result, ToolCallResult::success(json!({"ok": true})));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected a tool result, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_tool_result_item_roundtrip_legacy_to_new_keeps_trust_explicit() -> TestResult {
        let legacy = json!({
            "id": "item-2",
            "call_id": "call-2",
            "result": {"status": "error", "message": "denied"},
            "duration_ms": 0,
        });
        let item: ToolResultItem =
            serde_json::from_value(legacy).map_err(ctx("legacy item stays readable"))?;

        let rewritten = serde_json::to_value(&item).map_err(ctx("item serializes"))?;
        assert_eq!(rewritten["trust"], json!("untrusted"));

        let reread: ToolResultItem =
            serde_json::from_value(rewritten).map_err(ctx("new item reads back"))?;
        assert_eq!(reread.trust, ResultTrust::Untrusted);
        assert_eq!(reread.result, ToolCallResult::error("denied"));
        Ok(())
    }

    #[test]
    fn test_tool_result_item_roundtrip_preserves_runtime_trust() -> TestResult {
        let value = json!({
            "id": "item-3",
            "call_id": "call-3",
            "result": {"status": "error", "message": "cancelled"},
            "duration_ms": 1,
            "trust": "runtime",
        });
        let item: ToolResultItem = serde_json::from_value(value).map_err(ctx("new item reads"))?;
        assert_eq!(item.trust, ResultTrust::Runtime);
        let back = serde_json::to_value(&item).map_err(ctx("item serializes"))?;
        assert_eq!(back["trust"], json!("runtime"));
        Ok(())
    }

    #[test]
    fn test_tool_result_item_still_rejects_unknown_fields() {
        let value = json!({
            "id": "item-4",
            "call_id": "call-4",
            "result": {"status": "success", "value": null},
            "duration_ms": 1,
            "trust": "untrusted",
            "unexpected": true,
        });
        assert!(serde_json::from_value::<ToolResultItem>(value).is_err());
    }

    #[test]
    fn test_opaque_reasoning_serde_roundtrip_preserves_blocks_verbatim() -> TestResult {
        let reasoning = OpaqueReasoning {
            provider: "anthropic".to_owned(),
            model: "claude-opus-5".to_owned(),
            blocks: vec![
                json!({"type": "thinking", "thinking": "step", "signature": "sig=="}),
                json!({"type": "redacted_thinking", "data": "opaque"}),
            ],
        };
        let json = serde_json::to_value(&reasoning).map_err(ctx("reasoning serializes"))?;
        assert_eq!(json["blocks"][1]["data"], json!("opaque"));
        let back: OpaqueReasoning =
            serde_json::from_value(json).map_err(ctx("reasoning deserializes"))?;
        assert_eq!(back, reasoning);
        Ok(())
    }

    #[test]
    fn test_opaque_reasoning_rejects_unknown_fields() {
        let value = json!({"provider": "p", "model": "m", "blocks": [], "extra": 1});
        assert!(serde_json::from_value::<OpaqueReasoning>(value).is_err());
    }

    #[test]
    fn tool_call_result_uses_an_explicit_success_wire_shape() -> TestResult {
        let result = ToolCallResult::success(json!({"temperature": 20}));

        assert_eq!(
            serde_json::to_value(result).map_err(ctx("tool outcome serializes"))?,
            json!({"status": "success", "value": {"temperature": 20}})
        );
        Ok(())
    }

    #[test]
    fn tool_call_result_uses_an_explicit_error_wire_shape() -> TestResult {
        let result = ToolCallResult::error("tool unavailable");

        assert_eq!(
            serde_json::to_value(result).map_err(ctx("tool outcome serializes"))?,
            json!({"status": "error", "message": "tool unavailable"})
        );
        Ok(())
    }

    #[test]
    fn tool_placement_wire_shapes_are_tagged_by_kind() -> TestResult {
        assert_eq!(
            serde_json::to_value(ToolPlacement::Host).map_err(ctx("host"))?,
            json!({"kind": "host"})
        );
        assert_eq!(
            serde_json::to_value(ToolPlacement::Sandbox).map_err(ctx("sandbox"))?,
            json!({"kind": "sandbox"})
        );
        assert_eq!(
            serde_json::to_value(ToolPlacement::Gateway { node: None }).map_err(ctx("gateway"))?,
            json!({"kind": "gateway"})
        );
        let back: ToolPlacement = serde_json::from_value(json!({"kind": "gateway", "node": "gw"}))
            .map_err(ctx("gateway decodes"))?;
        assert_eq!(
            back,
            ToolPlacement::Gateway {
                node: Some("gw".to_owned())
            }
        );
        assert_eq!(back.label(), "gateway");
        Ok(())
    }

    /// Additive evolution: an unknown placement kind (even with extra
    /// fields) decodes to `Unknown`, never to `Host`.
    #[test]
    fn tool_placement_unknown_kind_decodes_to_unknown() -> TestResult {
        let future: ToolPlacement =
            serde_json::from_value(json!({"kind": "zellhost", "pod": "p-1"}))
                .map_err(ctx("unknown placement decodes"))?;
        assert_eq!(future, ToolPlacement::Unknown);
        assert_eq!(future.label(), "unknown");
        assert!(serde_json::from_value::<ToolPlacement>(json!({"node": "x"})).is_err());
        Ok(())
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
