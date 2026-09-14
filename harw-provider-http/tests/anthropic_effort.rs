//! Integration tests for `build_messages_body` reasoning-effort wire behaviour.
//!
//! Spec source: harw-provider-http task brief (Task A + Task C).
//!
//! These tests verify two wire-format invariants:
//! 1. `ReasoningEffort::Minimal` → `thinking.type == "adaptive"` is set,
//!    `output_config` object is absent (or has no `effort` key) so the model
//!    auto-selects its thinking budget.
//! 2. `ReasoningEffort::High` → both `thinking.type == "adaptive"` and
//!    `output_config.effort == "high"` are emitted.

use harw_core::{ConversationHistory, ModelRequest};
use harw_provider_http::build_messages_body;
use harw_types::ReasoningEffort;
use serde_json::Value;

/// Builds a minimal [`ModelRequest`] with the given `reasoning_effort`.
fn make_request(effort: Option<ReasoningEffort>) -> ModelRequest {
    let mut history = ConversationHistory::new();
    history.push_user_text("hello");
    ModelRequest {
        system_prompt: String::new(),
        instruction_fragments: Vec::new(),
        context: Vec::new(),
        history,
        tools: Vec::new(),
        context_assembly: Default::default(),
        reasoning_effort: effort,
        model_id: None,
        provider_id: None,
        data_block: None,
        max_output_tokens: None,
        tool_result_max_bytes: None,
    }
}

/// Asserts that `output_config.effort` wire field is absent when effort is `Minimal`.
///
/// When `ReasoningEffort::Minimal` is requested, the Anthropic provider must:
/// - emit `thinking: {"type": "adaptive"}` (reasoning IS requested)
/// - omit `output_config.effort` entirely (no pinned level; model auto-selects)
#[test]
fn minimal_effort_omits_output_config_effort_field() {
    let request = make_request(Some(ReasoningEffort::Minimal));
    let body = build_messages_body("claude-sonnet-5", 1024, &request);

    // Extended Thinking must still be active.
    assert_eq!(
        body.get("thinking")
            .and_then(|t| t.get("type"))
            .and_then(Value::as_str),
        Some("adaptive"),
        "thinking.type should be 'adaptive' even for Minimal effort"
    );

    // output_config must not exist OR must not contain an `effort` key.
    let effort_field = body.get("output_config").and_then(|oc| oc.get("effort"));
    assert!(
        effort_field.is_none(),
        "output_config.effort must be absent for Minimal effort, got: {effort_field:?}"
    );
}

/// Asserts that `output_config.effort == "high"` is emitted for `High`.
///
/// When `ReasoningEffort::High` is requested the provider must emit both:
/// - `thinking: {"type": "adaptive"}`
/// - `output_config: {"effort": "high"}`
#[test]
fn high_effort_emits_high_string() {
    let request = make_request(Some(ReasoningEffort::High));
    let body = build_messages_body("claude-opus-4-8", 1024, &request);

    assert_eq!(
        body.get("thinking")
            .and_then(|t| t.get("type"))
            .and_then(Value::as_str),
        Some("adaptive"),
        "thinking.type should be 'adaptive' for High effort"
    );

    assert_eq!(
        body.get("output_config")
            .and_then(|oc| oc.get("effort"))
            .and_then(Value::as_str),
        Some("high"),
        "output_config.effort should be 'high' for High effort"
    );
}
