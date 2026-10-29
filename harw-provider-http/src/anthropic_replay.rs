//! Provider-private, bounded replay of complete native Anthropic assistant rounds.
//!
//! Signatures and redacted thinking are opaque. Only native tool_use blocks are
//! executable calls; neither thinking text nor transcript summaries are parsed.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use harw_core::{
    ConversationHistory, ModelError, ModelMessage, ModelRequest, ModelResponse, RequestIdentity,
};
use harw_protocol::items::TurnItem;
use harw_types::ContentDigest;
use serde_json::Value;

const MAX_ROUNDS: usize = 64;
const MAX_BYTES: usize = 16 * 1024 * 1024;

struct Round {
    identity: RequestIdentity,
    provider: String,
    endpoint: String,
    model: String,
    prefix_len: usize,
    prefix_digest: ContentDigest,
    expected: Vec<ModelMessage>,
    content: Value,
    bytes: usize,
}

#[derive(Default)]
pub(crate) struct AnthropicReplayCache {
    rounds: Mutex<VecDeque<Round>>,
}

fn unavailable(detail: &str) -> ModelError {
    ModelError::RequestFailed(format!(
        "Anthropic thinking continuity unavailable ({detail}); start a fresh conversation or explicitly rebuild its context before retrying"
    ))
}

fn digest(items: &[TurnItem]) -> Result<ContentDigest, ModelError> {
    serde_json::to_vec(items)
        .map(|bytes| ContentDigest::of(&bytes))
        .map_err(|_| unavailable("cannot bind transcript prefix"))
}

fn identity(request: &ModelRequest) -> Option<&RequestIdentity> {
    request
        .identity()
        .filter(|identity| !identity.session.is_empty() && !identity.agent.is_empty())
}

fn projected(items: &[TurnItem]) -> Vec<ModelMessage> {
    ConversationHistory::from_items(items.to_vec()).to_model_messages()
}

// Mirrors build_messages_body's grouping before native rounds are restored.
fn wire_count(messages: &[ModelMessage]) -> usize {
    let mut count = 0;
    let mut previous = None;
    for message in messages {
        let (role, array, merge) = match message {
            ModelMessage::User { images, .. } => ("user", !images.is_empty(), false),
            ModelMessage::Assistant { .. } => ("assistant", false, false),
            ModelMessage::ToolCall { .. } => ("assistant", true, true),
            ModelMessage::ToolResult { .. } => ("user", true, true),
        };
        if !merge || previous != Some((role, true)) {
            count += 1;
        }
        previous = Some((role, array));
    }
    count
}

impl AnthropicReplayCache {
    pub(crate) fn remember(
        &self,
        provider: &str,
        endpoint: &str,
        model: &str,
        request: &ModelRequest,
        response: &ModelResponse,
        body: &Value,
    ) -> Result<(), ModelError> {
        let Some(content) = body.get("content").and_then(Value::as_array) else {
            return Err(unavailable("native assistant content is missing"));
        };
        let has_thinking = content.iter().any(|block| {
            matches!(
                block.get("type").and_then(Value::as_str),
                Some("thinking" | "redacted_thinking")
            )
        });
        let Some(identity) = identity(request) else {
            return if has_thinking {
                Err(unavailable("request identity is missing or empty"))
            } else {
                Ok(())
            };
        };
        let native_calls = content
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
            .count();
        if native_calls != response.tool_calls.len() {
            return Err(unavailable("native assistant tool round is incomplete"));
        }
        let mut expected = Vec::new();
        if let Some(text) = &response.message {
            expected.push(ModelMessage::Assistant { text: text.clone() });
        }
        expected.extend(
            response
                .tool_calls
                .iter()
                .map(|call| ModelMessage::ToolCall {
                    call_id: call.id.clone(),
                    name: call.name.as_str().to_owned(),
                    arguments: call.arguments.clone(),
                }),
        );
        if expected.is_empty() {
            return Err(unavailable("assistant round has no complete output"));
        }
        let bytes = serde_json::to_vec(content)
            .map_err(|_| unavailable("cannot retain native content"))?
            .len();
        if bytes > MAX_BYTES {
            return Err(unavailable("native round exceeds the replay byte limit"));
        }
        let round = Round {
            identity: identity.clone(),
            provider: provider.to_owned(),
            endpoint: endpoint.to_owned(),
            model: model.to_owned(),
            prefix_len: request.history.len(),
            prefix_digest: digest(request.history.items())?,
            expected,
            content: Value::Array(content.clone()),
            bytes,
        };
        let mut rounds = self.rounds.lock().unwrap_or_else(PoisonError::into_inner);
        rounds.retain(|old| {
            !(old.identity == round.identity
                && old.provider == round.provider
                && old.endpoint == round.endpoint
                && old.model == round.model
                && old.prefix_digest == round.prefix_digest)
        });
        while rounds.len() >= MAX_ROUNDS
            || rounds.iter().map(|round| round.bytes).sum::<usize>() + bytes > MAX_BYTES
        {
            rounds.pop_front();
        }
        rounds.push_back(round);
        Ok(())
    }

    /// Restore whole content arrays, never independently matched thinking/calls.
    /// Missing continuity during thinking is an explicit error, including after
    /// restart, eviction, model/provider switches or transcript compaction.
    pub(crate) fn apply(
        &self,
        provider: &str,
        endpoint: &str,
        model: &str,
        request: &ModelRequest,
        body: &mut Value,
    ) -> Result<(), ModelError> {
        if body.get("thinking").is_some() && identity(request).is_none() {
            return Err(unavailable("request identity is missing or empty"));
        }
        let history = request.history.items();
        let historical = request.history.to_model_messages();
        if !historical.iter().any(|message| {
            matches!(
                message,
                ModelMessage::Assistant { .. } | ModelMessage::ToolCall { .. }
            )
        }) {
            return Ok(());
        }
        // Redacted-only thinking has no visible ReasoningItem. Historical calls
        // therefore also require original rounds, even if thinking was disabled
        // after that response. A restart must never silently omit its signature.
        let required = body.get("thinking").is_some()
            || history
                .iter()
                .any(|item| matches!(item, TurnItem::Reasoning(_)))
            || historical
                .iter()
                .any(|message| matches!(message, ModelMessage::ToolCall { .. }));
        let Some(identity) = identity(request) else {
            return if required {
                Err(unavailable("request identity is missing or empty"))
            } else {
                Ok(())
            };
        };
        let messages = body
            .get_mut("messages")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| unavailable("native messages are missing"))?;
        let mut covered = vec![false; historical.len()];
        let mut replacements = Vec::new();
        let rounds = self.rounds.lock().unwrap_or_else(PoisonError::into_inner);
        for round in rounds.iter().filter(|round| {
            round.identity == *identity
                && round.provider == provider
                && round.endpoint == endpoint
                && round.model == model
        }) {
            if round.prefix_len >= history.len()
                || digest(&history[..round.prefix_len])? != round.prefix_digest
            {
                continue;
            }
            let prefix = projected(&history[..round.prefix_len]);
            let start = prefix.len();
            let end = start + round.expected.len();
            if historical.get(start..end) != Some(round.expected.as_slice())
                || (matches!(round.expected.last(), Some(ModelMessage::ToolCall { .. }))
                    && matches!(historical.get(end), Some(ModelMessage::ToolCall { .. })))
            {
                continue;
            }
            let wire_start = wire_count(&prefix);
            let wire_end = wire_start + wire_count(&round.expected);
            if messages.get(wire_start..wire_end).is_none() {
                return Err(unavailable("assistant projection changed"));
            }
            replacements.push((
                wire_start,
                wire_end,
                serde_json::json!({
                    "role": "assistant", "content": round.content,
                }),
            ));
            covered[start..end].fill(true);
        }
        if required
            && historical.iter().zip(covered).any(|(message, covered)| {
                !covered
                    && matches!(
                        message,
                        ModelMessage::Assistant { .. } | ModelMessage::ToolCall { .. }
                    )
            })
        {
            return Err(unavailable(
                "original full round was lost, changed, compacted or belongs to another provider/model/identity",
            ));
        }
        replacements.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
        for (start, end, restored) in replacements {
            messages.splice(start..end, [restored]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::{
        build_messages_body, extract_anthropic_stop_reason, extract_anthropic_text,
        extract_anthropic_tool_calls, extract_anthropic_usage,
    };
    use crate::test_support::{TestResult, ctx};
    use harw_protocol::items::{ReasoningItem, ToolCallResult};
    use harw_types::{ItemId, ReasoningEffort};
    use serde_json::json;

    const PROVIDER: &str = "anthropic";
    const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
    const MODEL: &str = "claude-opus-5";

    fn request() -> ModelRequest {
        let mut history = ConversationHistory::new();
        history.push_user_text("Look up context");
        ModelRequest {
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: Some(ReasoningEffort::High),
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            stream: None,
            identity: Some(RequestIdentity {
                session: "session-a".into(),
                agent: "agent-a".into(),
                role: "worker".into(),
            }),
        }
    }

    fn native() -> Value {
        json!({"stop_reason":"tool_use", "content":[
            {"type":"thinking", "thinking":"  provider internal\ntext  ", "signature":"opaque-signature=="},
            {"type":"tool_use", "id":"one", "name":"lookup", "input":{"q":"first"}},
            {"type":"redacted_thinking", "data":"opaque-redacted=="},
            {"type":"text", "text":"Checking context"},
            {"type":"thinking", "thinking":"second opaque block", "signature":"signature-two=="},
            {"type":"tool_use", "id":"two", "name":"lookup", "input":{"q":"second"}}
        ]})
    }

    fn response(body: &Value) -> TestResult<ModelResponse> {
        Ok(ModelResponse {
            message: extract_anthropic_text(body),
            tool_calls: extract_anthropic_tool_calls(body).map_err(ctx("parse native calls"))?,
            usage: extract_anthropic_usage(body),
            stop: extract_anthropic_stop_reason(body),
            reasoning: None,
        })
    }

    fn append(request: &mut ModelRequest, response: &ModelResponse) {
        request.history.push(TurnItem::Reasoning(ReasoningItem {
            id: ItemId::new(),
            summary_text: vec!["surface summary must never be replayed".into()],
            raw_content: Vec::new(),
        }));
        if let Some(text) = &response.message {
            request.history.push_assistant_text(text.clone(), None);
        }
        for call in &response.tool_calls {
            request.history.push_tool_call(
                call.id.clone(),
                call.name.as_str(),
                call.arguments.clone(),
            );
        }
        for call in &response.tool_calls {
            request.history.push_tool_result(
                call.id.clone(),
                ToolCallResult::success(json!({"fact":"context"})),
                1,
            );
        }
    }

    #[test]
    fn signed_thinking_tool_use_and_results_replay_complete_native_rounds() -> TestResult {
        let cache = AnthropicReplayCache::default();
        let first = request();
        let raw = native();
        let parsed = response(&raw)?;
        cache
            .remember(PROVIDER, ENDPOINT, MODEL, &first, &parsed, &raw)
            .map_err(ctx("remember first"))?;
        let mut second = first.clone();
        append(&mut second, &parsed);
        second.data_block = Some("new context".into());
        let mut wire = build_messages_body(MODEL, 4096, &second);
        cache
            .apply(PROVIDER, ENDPOINT, MODEL, &second, &mut wire)
            .map_err(ctx("replay first"))?;
        assert_eq!(wire["messages"][1]["content"], raw["content"]);
        assert_eq!(wire["messages"].as_array().map(Vec::len), Some(3));
        assert_eq!(wire["messages"][2]["content"][0]["tool_use_id"], "one");
        assert_eq!(wire["messages"][2]["content"][1]["tool_use_id"], "two");
        assert_eq!(wire["messages"][2]["content"][2]["text"], "new context");

        // A second tool round shares the same assistant turn. Every earlier
        // signed block and call ordering remains intact when its results return.
        let raw_second = json!({"stop_reason":"tool_use", "content":[
            {"type":"thinking", "thinking":"more context", "signature":"third-signature"},
            {"type":"tool_use", "id":"three", "name":"lookup", "input":{}}
        ]});
        let parsed_second = response(&raw_second)?;
        cache
            .remember(
                PROVIDER,
                ENDPOINT,
                MODEL,
                &second,
                &parsed_second,
                &raw_second,
            )
            .map_err(ctx("remember second"))?;
        let mut third = second.clone();
        third.data_block = None;
        append(&mut third, &parsed_second);
        let mut third_wire = build_messages_body(MODEL, 4096, &third);
        cache
            .apply(PROVIDER, ENDPOINT, MODEL, &third, &mut third_wire)
            .map_err(ctx("replay both"))?;
        assert_eq!(third_wire["messages"][1]["content"], raw["content"]);
        assert_eq!(third_wire["messages"][3]["content"], raw_second["content"]);
        assert_eq!(
            third_wire["messages"][4]["content"][0]["tool_use_id"],
            "three"
        );
        Ok(())
    }

    #[test]
    fn identity_provider_endpoint_model_and_mutated_rounds_are_refused() -> TestResult {
        let cache = AnthropicReplayCache::default();
        let first = request();
        let raw = native();
        let parsed = response(&raw)?;
        cache
            .remember(PROVIDER, ENDPOINT, MODEL, &first, &parsed, &raw)
            .map_err(ctx("remember"))?;
        let mut followup = first.clone();
        append(&mut followup, &parsed);
        for (provider, endpoint, model) in [
            ("other-provider", ENDPOINT, MODEL),
            (PROVIDER, "https://other.example/v1/messages", MODEL),
            (PROVIDER, ENDPOINT, "claude-sonnet-5"),
        ] {
            let mut wire = build_messages_body(model, 4096, &followup);
            assert!(
                cache
                    .apply(provider, endpoint, model, &followup, &mut wire)
                    .is_err()
            );
        }
        for identity in [
            None,
            Some(RequestIdentity {
                session: "session-b".into(),
                agent: "agent-a".into(),
                role: "worker".into(),
            }),
            Some(RequestIdentity {
                session: "session-a".into(),
                agent: "agent-b".into(),
                role: "worker".into(),
            }),
        ] {
            let mut wrong = followup.clone();
            wrong.identity = identity;
            let mut wire = build_messages_body(MODEL, 4096, &wrong);
            assert!(
                cache
                    .apply(PROVIDER, ENDPOINT, MODEL, &wrong, &mut wire)
                    .is_err()
            );
        }
        let mut changed = first.clone();
        let mut changed_response = parsed.clone();
        changed_response.tool_calls[1].arguments = json!({"q":"changed"});
        append(&mut changed, &changed_response);
        let mut wire = build_messages_body(MODEL, 4096, &changed);
        assert!(
            cache
                .apply(PROVIDER, ENDPOINT, MODEL, &changed, &mut wire)
                .is_err()
        );
        let empty_cache = AnthropicReplayCache::default();
        let mut wire = build_messages_body(MODEL, 4096, &followup);
        assert!(
            empty_cache
                .apply(PROVIDER, ENDPOINT, MODEL, &followup, &mut wire)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn initial_thinking_requires_nonempty_scoped_identity() -> TestResult {
        let cache = AnthropicReplayCache::default();
        let raw = native();
        let parsed = response(&raw)?;
        for identity in [
            None,
            Some(RequestIdentity {
                session: String::new(),
                agent: "agent-a".into(),
                role: "worker".into(),
            }),
            Some(RequestIdentity {
                session: "session-a".into(),
                agent: String::new(),
                role: "worker".into(),
            }),
        ] {
            let mut first = request();
            first.identity = identity;
            let mut wire = build_messages_body(MODEL, 4096, &first);
            assert!(
                cache
                    .apply(PROVIDER, ENDPOINT, MODEL, &first, &mut wire)
                    .is_err()
            );
            assert!(
                cache
                    .remember(PROVIDER, ENDPOINT, MODEL, &first, &parsed, &raw)
                    .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn same_calls_with_changed_prefix_and_partial_parallel_round_are_refused() -> TestResult {
        let cache = AnthropicReplayCache::default();
        let first = request();
        let raw = native();
        let parsed = response(&raw)?;
        cache
            .remember(PROVIDER, ENDPOINT, MODEL, &first, &parsed, &raw)
            .map_err(ctx("remember"))?;
        let mut changed_prefix = request(); // Even the same text has a new item ID.
        append(&mut changed_prefix, &parsed);
        let mut wire = build_messages_body(MODEL, 4096, &changed_prefix);
        assert!(
            cache
                .apply(PROVIDER, ENDPOINT, MODEL, &changed_prefix, &mut wire)
                .is_err()
        );
        let mut partial = first.clone();
        let mut partial_response = parsed.clone();
        partial_response.tool_calls.pop();
        append(&mut partial, &partial_response);
        let mut wire = build_messages_body(MODEL, 4096, &partial);
        assert!(
            cache
                .apply(PROVIDER, ENDPOINT, MODEL, &partial, &mut wire)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn redacted_tool_only_round_needs_continuity_even_after_thinking_is_disabled() -> TestResult {
        let cache = AnthropicReplayCache::default();
        let first = request();
        let raw = json!({"stop_reason":"tool_use", "content":[
            {"type":"redacted_thinking", "data":"opaque-redacted=="},
            {"type":"tool_use", "id":"opaque-call", "name":"lookup", "input":{}}
        ]});
        let parsed = response(&raw)?;
        assert!(parsed.message.is_none());
        assert_eq!(parsed.tool_calls.len(), 1);
        cache
            .remember(PROVIDER, ENDPOINT, MODEL, &first, &parsed, &raw)
            .map_err(ctx("remember redacted round"))?;
        let mut next = first.clone();
        next.reasoning_effort = None;
        let call = &parsed.tool_calls[0];
        next.history
            .push_tool_call(call.id.clone(), call.name.as_str(), call.arguments.clone());
        next.history.push_tool_result(
            call.id.clone(),
            ToolCallResult::success(json!({"fact":"context"})),
            1,
        );
        let mut wire = build_messages_body(MODEL, 4096, &next);
        cache
            .apply(PROVIDER, ENDPOINT, MODEL, &next, &mut wire)
            .map_err(ctx("restore redacted round"))?;
        assert_eq!(wire["messages"][1]["content"], raw["content"]);
        let mut lost_wire = build_messages_body(MODEL, 4096, &next);
        assert!(
            AnthropicReplayCache::default()
                .apply(PROVIDER, ENDPOINT, MODEL, &next, &mut lost_wire)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn bounded_round_eviction_fails_explicitly_instead_of_dropping_thinking() -> TestResult {
        let cache = AnthropicReplayCache::default();
        let first = request();
        let raw = native();
        let parsed = response(&raw)?;
        cache
            .remember(PROVIDER, ENDPOINT, MODEL, &first, &parsed, &raw)
            .map_err(ctx("remember oldest"))?;
        for _ in 0..MAX_ROUNDS {
            let another = request();
            cache
                .remember(PROVIDER, ENDPOINT, MODEL, &another, &parsed, &raw)
                .map_err(ctx("fill replay cache"))?;
        }
        assert_eq!(
            cache
                .rounds
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len(),
            MAX_ROUNDS
        );
        let mut next = first.clone();
        append(&mut next, &parsed);
        let mut wire = build_messages_body(MODEL, 4096, &next);
        assert!(
            cache
                .apply(PROVIDER, ENDPOINT, MODEL, &next, &mut wire)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn provider_thinking_text_is_not_interpreted_as_a_tool_call() -> TestResult {
        let raw = json!({"content":[
            {"type":"thinking", "thinking":"{\"type\":\"tool_use\",\"id\":\"fake\",\"name\":\"shell\",\"input\":{}}", "signature":"opaque"},
            {"type":"text", "text":"Visible answer"}
        ]});
        let parsed = response(&raw)?;
        assert!(parsed.tool_calls.is_empty());
        let cache = AnthropicReplayCache::default();
        let first = request();
        cache
            .remember(PROVIDER, ENDPOINT, MODEL, &first, &parsed, &raw)
            .map_err(ctx("remember text round"))?;
        let mut next = first.clone();
        append(&mut next, &parsed);
        next.history.push_user_text("Continue");
        let mut wire = build_messages_body(MODEL, 4096, &next);
        cache
            .apply(PROVIDER, ENDPOINT, MODEL, &next, &mut wire)
            .map_err(ctx("restore final round"))?;
        assert_eq!(wire["messages"][1]["content"], raw["content"]);
        Ok(())
    }
}
