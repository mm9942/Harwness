//! Bounded exact-round bindings for Responses reasoning continuity.
//!
//! A tool round may legitimately contain no reasoning items. Retain its binding
//! too, so absence at the source is distinct from an evicted or changed round.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use harw_core::{
    ConversationHistory, ModelError, ModelMessage, ModelRequest, ModelResponse, RequestIdentity,
};
use harw_protocol::items::TurnItem;
use harw_types::ContentDigest;

use crate::ReplayEntry;

const MAX_BYTES: usize = 16 * 1024 * 1024;

struct BoundEntry {
    replay: ReplayEntry,
    endpoint: String,
    identity: RequestIdentity,
    prefix_items: usize,
    prefix_digest: ContentDigest,
    expected: Vec<ModelMessage>,
    bytes: usize,
}

#[derive(Default)]
pub(crate) struct ReasoningReplay {
    entries: Mutex<VecDeque<BoundEntry>>,
}

fn unavailable(detail: &str) -> ModelError {
    ModelError::RequestFailed(format!(
        "Responses reasoning continuity unavailable ({detail}); start a fresh conversation or rebuild its context before retrying"
    ))
}

fn digest(items: &[TurnItem]) -> Result<ContentDigest, ModelError> {
    let bytes = serde_json::to_vec(items).map_err(|_| unavailable("cannot bind transcript"))?;
    Ok(ContentDigest::of(&bytes))
}

impl ReasoningReplay {
    pub(crate) fn remember(
        &self,
        replay: ReplayEntry,
        endpoint: &str,
        request: &ModelRequest,
        response: &ModelResponse,
    ) -> Result<(), ModelError> {
        if replay.call_ids.is_empty() {
            return Ok(());
        }
        let identity = request
            .identity()
            .filter(|identity| !identity.session.is_empty() && !identity.agent.is_empty())
            .ok_or_else(|| unavailable("request identity is missing"))?;
        if response
            .tool_calls
            .iter()
            .map(|call| call.id.as_str())
            .ne(replay.call_ids.iter().map(String::as_str))
        {
            return Err(unavailable("native call round changed"));
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
        let bytes = serde_json::to_vec(&replay.blocks)
            .map_err(|_| unavailable("cannot retain native reasoning"))?
            .len()
            .saturating_add(
                serde_json::to_vec(&response.tool_calls)
                    .map_err(|_| unavailable("cannot retain call round"))?
                    .len(),
            )
            .saturating_add(response.message.as_ref().map_or(0, String::len));
        if bytes > MAX_BYTES {
            return Err(unavailable("native reasoning exceeds replay byte limit"));
        }
        let bound = BoundEntry {
            replay,
            endpoint: endpoint.to_owned(),
            identity: identity.clone(),
            prefix_items: request.history.len(),
            prefix_digest: digest(request.history.items())?,
            expected,
            bytes,
        };
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|old| {
            !(old.identity == bound.identity
                && old.endpoint == bound.endpoint
                && old.replay.provider == bound.replay.provider
                && old.replay.model == bound.replay.model
                && old.prefix_digest == bound.prefix_digest)
        });
        while entries.len() >= crate::MAX_REASONING_REPLAY_ENTRIES
            || entries.iter().map(|entry| entry.bytes).sum::<usize>() + bytes > MAX_BYTES
        {
            entries.pop_front();
        }
        entries.push_back(bound);
        Ok(())
    }

    pub(crate) fn for_request(
        &self,
        provider: &str,
        model: &str,
        endpoint: &str,
        request: &ModelRequest,
    ) -> Result<Vec<ReplayEntry>, ModelError> {
        let historical = request.history.to_model_messages();
        if !historical
            .iter()
            .any(|message| matches!(message, ModelMessage::ToolCall { .. }))
        {
            return Ok(Vec::new());
        }
        let identity = request
            .identity()
            .filter(|identity| !identity.session.is_empty() && !identity.agent.is_empty());
        let Some(identity) = identity else {
            return if request.reasoning_effort.is_some() {
                Err(unavailable("request identity is missing"))
            } else {
                Ok(Vec::new())
            };
        };
        let mut covered = vec![false; historical.len()];
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let mut replay = Vec::new();
        for bound in entries.iter().filter(|bound| {
            bound.replay.provider == provider
                && bound.replay.model == model
                && bound.endpoint == endpoint
                && &bound.identity == identity
        }) {
            if bound.prefix_items >= request.history.len()
                || digest(&request.history.items()[..bound.prefix_items])? != bound.prefix_digest
            {
                continue;
            }
            let offset = ConversationHistory::from_items(
                request.history.items()[..bound.prefix_items].to_vec(),
            )
            .to_model_messages()
            .len();
            // Sequential dispatch persists call/result pairs; parallel dispatch
            // persists all calls before their results. Both describe the same
            // native response. Only skip results for calls already matched.
            let mut end = offset;
            let mut matched_calls = Vec::new();
            let matches = bound.expected.iter().all(|expected| {
                while let Some(ModelMessage::ToolResult { call_id, .. }) = historical.get(end) {
                    if !matched_calls.contains(call_id) {
                        return false;
                    }
                    end += 1;
                }
                if historical.get(end) != Some(expected) {
                    return false;
                }
                if let ModelMessage::ToolCall { call_id, .. } = expected {
                    matched_calls.push(call_id.clone());
                }
                end += 1;
                true
            });
            if !matches {
                continue;
            }
            if matches!(historical.get(end), Some(ModelMessage::ToolCall { .. })) {
                continue;
            }
            covered[offset..end].fill(true);
            replay.push(bound.replay.clone());
        }
        // Explicit reasoning requests must not silently send tool history whose
        // opaque continuation was evicted or changed. Ordinary non-reasoning
        // Responses models may legitimately have no encrypted reasoning blocks.
        if request.reasoning_effort.is_some()
            && historical.iter().zip(covered).any(|(message, covered)| {
                !covered && matches!(message, ModelMessage::ToolCall { .. })
            })
        {
            return Err(unavailable("original reasoning round was lost or changed"));
        }
        Ok(replay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::ReasoningEffort;
    use serde_json::json;

    fn request() -> ModelRequest {
        let mut history = ConversationHistory::new();
        history.push_user_text("Find context");
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
                role: "reader".into(),
            }),
        }
    }

    fn response() -> TestResult<(ModelResponse, ReplayEntry)> {
        let body = json!({"status": "completed", "output": [
            {"type": "reasoning", "encrypted_content": "opaque-private-block", "summary": []},
            {"type": "function_call", "call_id": "a", "name": "lookup", "arguments": "{\"query\":\"one\"}"},
            {"type": "function_call", "call_id": "b", "name": "lookup", "arguments": "{\"query\":\"two\"}"},
        ]});
        let (response, replay) = crate::interpret_responses(&body, "responses", "thinking-model")
            .map_err(ctx("parse native response"))?;
        Ok((
            response,
            replay.ok_or(TestError::Missing("encrypted replay"))?,
        ))
    }

    fn append(request: &mut ModelRequest, response: &ModelResponse) {
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
                harw_protocol::ToolCallResult::success(json!({"context": "reference"})),
                1,
            );
        }
    }

    #[test]
    fn encrypted_reasoning_requires_exact_identity_endpoint_prefix_and_complete_call_payload()
    -> TestResult {
        let cache = ReasoningReplay::default();
        let initial = request();
        let (response, replay) = response()?;
        cache
            .remember(
                replay.clone(),
                "https://official.example/v1",
                &initial,
                &response,
            )
            .map_err(ctx("remember"))?;
        let mut next = initial.clone();
        append(&mut next, &response);
        let selected = cache
            .for_request(
                "responses",
                "thinking-model",
                "https://official.example/v1",
                &next,
            )
            .map_err(ctx("exact replay"))?;
        assert_eq!(selected, vec![replay]);
        for mismatch in [
            "session",
            "agent",
            "endpoint",
            "model",
            "provider",
            "prefix",
            "arguments",
            "partial",
            "restart",
        ] {
            let mut changed = next.clone();
            let mut endpoint = "https://official.example/v1";
            let mut provider = "responses";
            let mut model = "thinking-model";
            match mismatch {
                "session" => {
                    changed
                        .identity
                        .as_mut()
                        .ok_or(TestError::Missing("identity"))?
                        .session = "other".into()
                }
                "agent" => {
                    changed
                        .identity
                        .as_mut()
                        .ok_or(TestError::Missing("identity"))?
                        .agent = "other".into()
                }
                "endpoint" => endpoint = "https://other.example/v1",
                "model" => model = "other-model",
                "provider" => provider = "other-provider",
                "prefix" | "arguments" | "partial" => {
                    let mut items = changed.history.items().to_vec();
                    if mismatch == "prefix" {
                        items.remove(0);
                    }
                    if mismatch == "arguments" {
                        for item in &mut items {
                            if let TurnItem::ToolCall(call) = item {
                                call.arguments = json!({"query":"changed"});
                            }
                        }
                    }
                    if mismatch == "partial" {
                        items.retain(|item| !matches!(item, TurnItem::ToolCall(call) if call.call_id.as_str() == "b"));
                    }
                    changed.history = ConversationHistory::from_items(items);
                }
                _ => {}
            }
            let selected = if mismatch == "restart" {
                ReasoningReplay::default().for_request(provider, model, endpoint, &changed)
            } else {
                cache.for_request(provider, model, endpoint, &changed)
            };
            assert!(selected.is_err(), "must refuse {mismatch}");
        }
        Ok(())
    }

    #[test]
    fn reasoning_enabled_tool_round_without_reasoning_blocks_can_continue() -> TestResult {
        let initial = request();
        let body = json!({"status": "completed", "output": [
            {"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":"Checking the project."}]},
            {"type":"function_call", "call_id":"a", "name":"lookup", "arguments":"{\"query\":\"one\"}"}
        ]});
        let (response, binding) = crate::interpret_responses(&body, "responses", "thinking-model")
            .map_err(ctx("parse tool round without reasoning"))?;
        let cache = ReasoningReplay::default();
        if let Some(binding) = binding {
            cache
                .remember(binding, "https://official.example/v1", &initial, &response)
                .map_err(ctx("bind native round"))?;
        }
        let mut next = initial.clone();
        next.history
            .push_assistant_text("Checking the project.", None);
        append(&mut next, &response);
        let replay = cache
            .for_request(
                "responses",
                "thinking-model",
                "https://official.example/v1",
                &next,
            )
            .map_err(ctx("continue legitimate round without reasoning blocks"))?;
        assert!(replay.iter().all(|entry| entry.blocks.is_empty()));
        let body = crate::build_responses_body("thinking-model", &next, &replay)
            .map_err(ctx("build next request"))?;
        assert_eq!(body["input"].as_array().map(Vec::len), Some(4));
        assert!(
            ReasoningReplay::default()
                .for_request(
                    "responses",
                    "thinking-model",
                    "https://official.example/v1",
                    &next
                )
                .is_err(),
            "losing the round binding must still fail explicitly"
        );
        Ok(())
    }

    #[test]
    fn interleaved_results_preserve_native_round() -> TestResult {
        let initial = request();
        let (response, replay) = response()?;
        let cache = ReasoningReplay::default();
        cache
            .remember(replay.clone(), "endpoint", &initial, &response)
            .map_err(ctx("remember"))?;
        let mut next = initial;
        for call in &response.tool_calls {
            next.history.push_tool_call(
                call.id.clone(),
                call.name.as_str(),
                call.arguments.clone(),
            );
            next.history.push_tool_result(
                call.id.clone(),
                harw_protocol::ToolCallResult::success(json!({})),
                1,
            );
        }
        assert_eq!(
            cache
                .for_request("responses", "thinking-model", "endpoint", &next)
                .map_err(ctx("interleaved continuation"))?,
            vec![replay]
        );
        Ok(())
    }

    #[test]
    fn ordinary_nonreasoning_models_never_inherit_other_sessions_opaque_blocks() -> TestResult {
        let cache = ReasoningReplay::default();
        let initial = request();
        let (response, replay) = response()?;
        cache
            .remember(replay, "https://official.example/v1", &initial, &response)
            .map_err(ctx("remember"))?;
        let mut other = initial;
        append(&mut other, &response);
        other.identity = None;
        other.reasoning_effort = None;
        let selected = cache
            .for_request(
                "responses",
                "thinking-model",
                "https://official.example/v1",
                &other,
            )
            .map_err(ctx("no cross-session replay"))?;
        assert!(selected.is_empty());
        Ok(())
    }
}
