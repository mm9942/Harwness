//! Exact, bounded chat reasoning continuity for explicitly supported protocols.
//!
//! The cache is private to a provider instance and bound to the authenticated
//! request identity, model, exact transcript prefix and complete response round.
//! It never derives calls from reasoning or replays rendered transcript summaries.
//! Restart, compaction or eviction that loses required continuity fails explicitly.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use harw_config::ChatReasoningReplay;
use harw_core::{
    ConversationHistory, ModelError, ModelMessage, ModelRequest, ModelResponse, RequestIdentity,
};
use harw_protocol::items::TurnItem;
use harw_types::ContentDigest;
use serde_json::Value;

const MAX_ENTRIES: usize = 64;
const MAX_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn resolve_mode(
    base_url: &str,
    model: &str,
    configured: Option<ChatReasoningReplay>,
) -> ChatReasoningReplay {
    configured.unwrap_or_else(|| {
        if crate::thinking_profiles::supports_reasoning_content(base_url, model) {
            ChatReasoningReplay::ReasoningContent
        } else {
            ChatReasoningReplay::Disabled
        }
    })
}

struct Round {
    identity: RequestIdentity,
    provider: String,
    model: String,
    prefix_items: usize,
    prefix_digest: ContentDigest,
    expected: Vec<ModelMessage>,
    // Original assistant content and reasoning are retained without trimming,
    // concatenation with display summaries, or interpretation as instructions.
    content: Value,
    reasoning: Option<String>,
    bytes: usize,
}

#[derive(Default)]
pub(crate) struct ChatReasoningCache {
    rounds: Mutex<VecDeque<Round>>,
}

fn unavailable(detail: &str) -> ModelError {
    ModelError::RequestFailed(format!(
        "chat reasoning continuity unavailable ({detail}); start a fresh conversation or explicitly rebuild its context before retrying"
    ))
}

fn digest(items: &[TurnItem]) -> Result<ContentDigest, ModelError> {
    let bytes =
        serde_json::to_vec(items).map_err(|_| unavailable("cannot bind transcript prefix"))?;
    Ok(ContentDigest::of(&bytes))
}

fn projection(items: &[TurnItem]) -> Vec<ModelMessage> {
    ConversationHistory::from_items(items.to_vec()).to_model_messages()
}

fn wire_count(messages: &[ModelMessage]) -> usize {
    let mut count = 0;
    let mut previous_call = false;
    for message in messages {
        let call = matches!(message, ModelMessage::ToolCall { .. });
        if !call || !previous_call {
            count += 1;
        }
        previous_call = call;
    }
    count
}

impl ChatReasoningCache {
    /// Remember one native assistant round after names were decoded by the
    /// adapter. A tool call embedded in plain text is not a replayable round.
    pub(crate) fn remember(
        &self,
        provider: &str,
        model: &str,
        request: &ModelRequest,
        response: &ModelResponse,
        body: &Value,
    ) -> Result<(), ModelError> {
        let identity = request
            .identity()
            .filter(|identity| !identity.session.is_empty() && !identity.agent.is_empty())
            .ok_or_else(|| unavailable("request identity is missing"))?;
        let message = body
            .pointer("/choices/0/message")
            .ok_or_else(|| unavailable("native assistant message is missing"))?;
        let native_calls = message.get("tool_calls").and_then(Value::as_array);
        if native_calls.map_or(0, Vec::len) != response.tool_calls.len() {
            return Err(unavailable(
                "tool calls are not a complete native response round",
            ));
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
        let reasoning = match message.get("reasoning_content") {
            Some(Value::String(text)) => Some(text.clone()),
            Some(Value::Null) | None => None,
            _ => return Err(unavailable("reasoning_content has an unsupported type")),
        };
        let bytes = serde_json::to_vec(message)
            .map_err(|_| unavailable("cannot retain native assistant message"))?
            .len()
            .saturating_add(response.message.as_ref().map_or(0, String::len))
            .saturating_add(
                serde_json::to_vec(&response.tool_calls)
                    .map_err(|_| unavailable("cannot retain native call payloads"))?
                    .len(),
            );
        if bytes > MAX_BYTES {
            return Err(unavailable(
                "assistant round exceeds the reasoning continuity byte limit",
            ));
        }
        let round = Round {
            identity: identity.clone(),
            provider: provider.to_owned(),
            model: model.to_owned(),
            prefix_items: request.history.len(),
            prefix_digest: digest(request.history.items())?,
            expected,
            content: message.get("content").cloned().unwrap_or(Value::Null),
            reasoning,
            bytes,
        };
        let mut rounds = self.rounds.lock().unwrap_or_else(PoisonError::into_inner);
        // Retries of the exact same input replace the discarded output rather
        // than making selection depend on a reused provider call id.
        rounds.retain(|old| {
            !(old.identity == round.identity
                && old.provider == round.provider
                && old.model == round.model
                && old.prefix_digest == round.prefix_digest)
        });
        while rounds.len() >= MAX_ENTRIES
            || rounds.iter().map(|round| round.bytes).sum::<usize>() + bytes > MAX_BYTES
        {
            rounds.pop_front();
        }
        rounds.push_back(round);
        Ok(())
    }

    /// Restore every historical assistant round, including final answers.
    /// This also reunites assistant content and calls split by generic history
    /// projection. Missing or mutated rounds fail before the HTTP request.
    pub(crate) fn apply(
        &self,
        provider: &str,
        model: &str,
        request: &ModelRequest,
        body: &mut Value,
    ) -> Result<(), ModelError> {
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
        let identity = request
            .identity()
            .filter(|identity| !identity.session.is_empty() && !identity.agent.is_empty())
            .ok_or_else(|| unavailable("request identity is missing"))?;
        let messages = body
            .get_mut("messages")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| unavailable("chat messages are missing"))?;
        let system_offset = usize::from(
            messages
                .first()
                .and_then(|message| message.get("role"))
                .and_then(Value::as_str)
                == Some("system"),
        );
        let mut replacements = Vec::new();
        let mut covered = vec![false; historical.len()];
        let rounds = self.rounds.lock().unwrap_or_else(PoisonError::into_inner);
        for round in rounds.iter().filter(|round| {
            round.provider == provider && round.model == model && &round.identity == identity
        }) {
            if round.prefix_items >= history.len()
                || digest(&history[..round.prefix_items])? != round.prefix_digest
            {
                continue;
            }
            let prefix = projection(&history[..round.prefix_items]);
            let offset = prefix.len();
            let end = offset + round.expected.len();
            if historical.get(offset..end) != Some(round.expected.as_slice()) {
                continue;
            }
            // A partial call group must not inherit another round's reasoning.
            if matches!(round.expected.last(), Some(ModelMessage::ToolCall { .. }))
                && matches!(historical.get(end), Some(ModelMessage::ToolCall { .. }))
            {
                continue;
            }
            let start_wire = system_offset + wire_count(&prefix);
            let end_wire = start_wire + wire_count(&round.expected);
            let existing = messages
                .get(start_wire..end_wire)
                .ok_or_else(|| unavailable("assistant round projection changed"))?;
            let mut restored = serde_json::json!({"role": "assistant", "content": round.content});
            if let Some(reasoning) = &round.reasoning {
                restored["reasoning_content"] = Value::String(reasoning.clone());
            }
            if let Some(calls) = existing
                .iter()
                .find_map(|message| message.get("tool_calls"))
            {
                restored["tool_calls"] = calls.clone();
            }
            replacements.push((start_wire, end_wire, restored));
            covered[offset..end].fill(true);
        }
        if historical.iter().zip(covered).any(|(message, covered)| {
            !covered
                && matches!(
                    message,
                    ModelMessage::Assistant { .. } | ModelMessage::ToolCall { .. }
                )
        }) {
            return Err(unavailable(
                "original assistant round was lost, changed, compacted or belongs to another identity/model",
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
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_protocol::items::{ReasoningItem, ToolCallResult};
    use harw_types::ItemId;
    use serde_json::json;

    fn request(history: ConversationHistory) -> ModelRequest {
        ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: Some(RequestIdentity {
                session: "session-a".into(),
                agent: "agent-a".into(),
                role: "reader".into(),
            }),
        }
    }

    fn native(content: Value, reasoning: &str, calls: Value) -> Value {
        json!({"choices": [{"finish_reason": "tool_calls", "message": {
            "role": "assistant", "content": content, "reasoning_content": reasoning,
            "tool_calls": calls,
        }}]})
    }

    fn call(id: &str, query: &str) -> Value {
        json!({"id": id, "type": "function", "function": {
            "name": "lookup", "arguments": json!({"query": query}).to_string(),
        }})
    }

    fn append(history: &mut ConversationHistory, response: &ModelResponse) {
        history.push(TurnItem::Reasoning(ReasoningItem {
            id: ItemId::new(),
            summary_text: vec!["display summary must not be replayed".into()],
            raw_content: Vec::new(),
        }));
        if let Some(text) = &response.message {
            history.push_assistant_text(text.clone(), None);
        }
        for call in &response.tool_calls {
            history.push_tool_call(call.id.clone(), call.name.as_str(), call.arguments.clone());
        }
        for call in &response.tool_calls {
            history.push_tool_result(
                call.id.clone(),
                ToolCallResult::success(json!({"fact": "scoped context"})),
                1,
            );
        }
    }

    #[test]
    fn exact_parallel_round_and_final_answer_reasoning_survive_followups() -> TestResult {
        let cache = ChatReasoningCache::default();
        let mut history = ConversationHistory::new();
        history.push_user_text("Find the relevant context");
        let first = request(history);
        let raw = native(
            json!("Checking context"),
            "  original\nreasoning  ",
            json!([call("a", "one"), call("b", "two")]),
        );
        let response = crate::interpret_chat(&raw, "supported", "thinking-model", &[])
            .map_err(ctx("native round"))?;
        cache
            .remember("supported", "thinking-model", &first, &response, &raw)
            .map_err(ctx("remember"))?;
        let mut second = first.clone();
        append(&mut second.history, &response);
        let mut wire = crate::build_chat_body(&second, "thinking-model");
        cache
            .apply("supported", "thinking-model", &second, &mut wire)
            .map_err(ctx("restore round"))?;
        assert_eq!(wire["messages"].as_array().map(Vec::len), Some(4));
        assert_eq!(
            wire["messages"][1]["content"],
            raw["choices"][0]["message"]["content"]
        );
        assert_eq!(
            wire["messages"][1]["reasoning_content"],
            "  original\nreasoning  "
        );
        assert_eq!(
            wire["messages"][1]["tool_calls"].as_array().map(Vec::len),
            Some(2)
        );
        assert_eq!(wire["messages"][2]["tool_call_id"], "a");
        assert_eq!(wire["messages"][3]["tool_call_id"], "b");

        let final_raw = native(
            json!("The context says X."),
            "exact final reasoning",
            json!([]),
        );
        let final_response = crate::interpret_chat(&final_raw, "supported", "thinking-model", &[])
            .map_err(ctx("final response"))?;
        cache
            .remember(
                "supported",
                "thinking-model",
                &second,
                &final_response,
                &final_raw,
            )
            .map_err(ctx("remember final"))?;
        append(&mut second.history, &final_response);
        second.history.push_user_text("Explain X");
        let mut wire = crate::build_chat_body(&second, "thinking-model");
        cache
            .apply("supported", "thinking-model", &second, &mut wire)
            .map_err(ctx("restore final"))?;
        assert_eq!(wire["messages"].as_array().map(Vec::len), Some(6));
        assert_eq!(
            wire["messages"][1]["reasoning_content"],
            "  original\nreasoning  "
        );
        assert_eq!(
            wire["messages"][4]["reasoning_content"],
            "exact final reasoning"
        );
        assert_eq!(wire["messages"][4]["content"], "The context says X.");
        assert_eq!(wire["messages"][5]["role"], "user");
        assert!(
            !wire
                .to_string()
                .contains("display summary must not be replayed")
        );
        Ok(())
    }

    #[test]
    fn reasoning_and_native_retrieval_need_no_visible_assistant_text() -> TestResult {
        let cache = ChatReasoningCache::default();
        let mut history = ConversationHistory::new();
        history.push_user_text("Find context");
        let initial = request(history);
        let raw = native(
            Value::Null,
            "Need context before answering",
            json!([call("read", "reference")]),
        );
        let response = crate::interpret_chat(&raw, "supported", "thinking-model", &[])
            .map_err(ctx("native call"))?;
        assert!(response.message.is_none());
        cache
            .remember("supported", "thinking-model", &initial, &response, &raw)
            .map_err(ctx("remember"))?;
        let mut next = initial.clone();
        append(&mut next.history, &response);
        let mut wire = crate::build_chat_body(&next, "thinking-model");
        cache
            .apply("supported", "thinking-model", &next, &mut wire)
            .map_err(ctx("replay"))?;
        assert!(wire["messages"][1]["content"].is_null());
        assert_eq!(
            wire["messages"][1]["reasoning_content"],
            "Need context before answering"
        );
        assert_eq!(wire["messages"][2]["role"], "tool");
        Ok(())
    }

    #[test]
    fn replay_refuses_other_identity_model_payload_prefix_and_missing_cache() -> TestResult {
        let cache = ChatReasoningCache::default();
        let mut history = ConversationHistory::new();
        history.push_user_text("Find context");
        let initial = request(history);
        let raw = native(
            Value::Null,
            "private reasoning",
            json!([call("same-id", "one")]),
        );
        let response = crate::interpret_chat(&raw, "supported", "thinking-model", &[])
            .map_err(ctx("parse"))?;
        cache
            .remember("supported", "thinking-model", &initial, &response, &raw)
            .map_err(ctx("remember"))?;
        let mut next = initial.clone();
        append(&mut next.history, &response);
        for mismatch in [
            "session", "agent", "provider", "model", "payload", "prefix", "restart", "identity",
        ] {
            let mut changed = next.clone();
            let mut provider = "supported";
            let mut model = "thinking-model";
            match mismatch {
                "session" => {
                    changed
                        .identity
                        .as_mut()
                        .ok_or(TestError::Missing("test identity"))?
                        .session = "other".into()
                }
                "agent" => {
                    changed
                        .identity
                        .as_mut()
                        .ok_or(TestError::Missing("test identity"))?
                        .agent = "other".into()
                }
                "provider" => provider = "other",
                "model" => model = "other",
                "identity" => changed.identity = None,
                "payload" => {
                    let mut items = changed.history.items().to_vec();
                    for item in &mut items {
                        if let TurnItem::ToolCall(call) = item {
                            call.arguments = json!({"query": "substituted"});
                        }
                    }
                    changed.history = ConversationHistory::from_items(items);
                }
                "prefix" => {
                    let mut items = changed.history.items().to_vec();
                    items.remove(0);
                    changed.history = ConversationHistory::from_items(items);
                }
                _ => {}
            }
            let mut wire = crate::build_chat_body(&changed, model);
            let result = if mismatch == "restart" {
                ChatReasoningCache::default().apply(provider, model, &changed, &mut wire)
            } else {
                cache.apply(provider, model, &changed, &mut wire)
            };
            let error = result
                .err()
                .ok_or(TestError::Missing("test expected a refusal"))?;
            assert!(!error.to_string().contains("private reasoning"));
            assert!(!wire.to_string().contains("private reasoning"));
        }
        Ok(())
    }

    #[test]
    fn verified_routes_and_explicit_capabilities_do_not_change_unknown_providers() {
        assert_eq!(
            resolve_mode("https://api.z.ai/api/coding/paas/v4", "glm-5.3", None),
            ChatReasoningReplay::ReasoningContent
        );
        assert_eq!(
            resolve_mode("https://api.deepseek.com/v1", "deepseek-flash", None),
            ChatReasoningReplay::ReasoningContent
        );
        assert_eq!(
            resolve_mode("https://proxy.example/v1", "glm-5.3", None),
            ChatReasoningReplay::Disabled
        );
        assert_eq!(
            resolve_mode(
                "https://api.z.ai/api/paas/v4",
                "glm-5.3",
                Some(ChatReasoningReplay::Disabled)
            ),
            ChatReasoningReplay::Disabled
        );
        assert_eq!(
            resolve_mode(
                "https://proxy.example/v1",
                "custom-thinker",
                Some(ChatReasoningReplay::ReasoningContent)
            ),
            ChatReasoningReplay::ReasoningContent
        );
    }
    #[test]
    fn cache_eviction_and_oversize_rounds_are_explicit_not_partial_replay() -> TestResult {
        let cache = ChatReasoningCache::default();
        let raw = native(json!("answer"), "exact reasoning", json!([]));
        let response = crate::interpret_chat(&raw, "supported", "thinking-model", &[])
            .map_err(ctx("parse"))?;
        let mut first = None;
        for index in 0..=MAX_ENTRIES {
            let mut history = ConversationHistory::new();
            history.push_user_text(format!("question-{index}"));
            let initial = request(history);
            cache
                .remember("supported", "thinking-model", &initial, &response, &raw)
                .map_err(ctx("remember bounded round"))?;
            if first.is_none() {
                first = Some(initial);
            }
        }
        let mut first = first.ok_or(TestError::Missing("first round"))?;
        append(&mut first.history, &response);
        let mut wire = crate::build_chat_body(&first, "thinking-model");
        assert!(
            cache
                .apply("supported", "thinking-model", &first, &mut wire)
                .is_err()
        );
        let oversized = native(json!("answer"), &"x".repeat(MAX_BYTES + 1), json!([]));
        let parsed = crate::interpret_chat(&oversized, "supported", "thinking-model", &[])
            .map_err(ctx("parse oversized"))?;
        assert!(
            cache
                .remember("supported", "thinking-model", &first, &parsed, &oversized)
                .is_err()
        );
        assert!(
            cache
                .rounds
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len()
                <= MAX_ENTRIES
        );
        Ok(())
    }
}
