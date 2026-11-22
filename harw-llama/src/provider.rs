//! [`harw_core::ModelProvider`] over a local GGUF model.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use harw_core::{
    ConversationHistory, ModelError, ModelFuture, ModelMessage, ModelProvider, ModelRequest,
    ModelResponse, ModelStreamEvent, StreamSink,
};
use harw_core::model::StopReason;
use harw_protocol::items::ToolCallResult;
use harw_tools::{ToolCall, ToolName, ToolSpec};
use harw_types::{ReasoningEffort, TokenUsage, ToolCallId};
use serde_json::{Value, json};

use crate::DEFAULT_N_CTX;
use crate::engine::{Engine, EngineConfig, FinishReason, SamplingParams};
use crate::error::LlamaError;
use crate::stream::{OutputSplitter, Piece, SplitOutput};
use crate::template::{
    ChatMessage, ChatToolCall, RenderInput, prompt_opens_think, render_prompt,
};
use crate::toolcall::parse_tool_calls;

/// Provider id reported by [`LocalGgufProvider`].
pub const LOCAL_PROVIDER_ID: &str = "local-gguf";

/// Settings of the local provider.
#[derive(Debug, Clone)]
pub struct LocalGgufConfig {
    /// Context window in tokens.
    pub n_ctx: u32,
    /// Generation threads; `0` picks a phone-friendly count.
    pub n_threads: i32,
    /// Reply token limit when the request sets none.
    pub max_new_tokens: u32,
    /// Whether replies think when the request does not say.
    pub thinking_default: bool,
}

impl Default for LocalGgufConfig {
    fn default() -> Self {
        Self {
            n_ctx: DEFAULT_N_CTX,
            n_threads: 0,
            max_new_tokens: 4096,
            thinking_default: false,
        }
    }
}

/// Runs a GGUF model in-process.
pub struct LocalGgufProvider {
    engine: Arc<Engine>,
    model_id: String,
    config: LocalGgufConfig,
    /// One generation at a time: a second context would double the memory use.
    gate: Arc<Mutex<()>>,
    counter: AtomicU64,
}

impl std::fmt::Debug for LocalGgufProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalGgufProvider")
            .field("model_id", &self.model_id)
            .finish_non_exhaustive()
    }
}

impl LocalGgufProvider {
    /// Loads the GGUF at `path`. Blocking: call it off the async runtime.
    pub fn open(
        path: &Path,
        model_id: impl Into<String>,
        config: LocalGgufConfig,
    ) -> Result<Self, LlamaError> {
        let engine = Engine::load(
            path,
            &EngineConfig {
                n_ctx: config.n_ctx,
                n_threads: config.n_threads,
                ..EngineConfig::default()
            },
        )?;
        Ok(Self {
            engine: Arc::new(engine),
            model_id: model_id.into(),
            config,
            gate: Arc::new(Mutex::new(())),
            counter: AtomicU64::new(0),
        })
    }
}

/// Everything a generation needs, owned so it can move to a blocking thread.
struct Job {
    prompt: String,
    tools: Vec<Value>,
    sampling: SamplingParams,
    start_in_think: bool,
    cancel: Option<harw_types::cancel::CancelToken>,
    stream: Option<StreamSink>,
}

struct Generated {
    prompt_tokens: u64,
    completion_tokens: u64,
    finish: FinishReason,
    output: SplitOutput,
}

/// Sets the flag when the response future is dropped, so a cancelled turn stops decoding.
struct AbortOnDrop(Arc<AtomicBool>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn emit(stream: Option<&StreamSink>, pieces: &mut Vec<Piece>) {
    for piece in pieces.drain(..) {
        if let Some(sink) = stream {
            sink.emit(match piece {
                Piece::Reasoning(text) => ModelStreamEvent::ReasoningDelta(text),
                Piece::Text(text) => ModelStreamEvent::TextDelta(text),
            });
        }
    }
}

fn run_job(
    engine: &Engine,
    gate: &Mutex<()>,
    job: &Job,
    abort: &AtomicBool,
) -> Result<Generated, LlamaError> {
    let _guard = gate.lock().unwrap_or_else(PoisonError::into_inner);
    let should_stop = || {
        abort.load(Ordering::Relaxed)
            || job.cancel.as_ref().is_some_and(harw_types::cancel::CancelToken::is_cancelled)
    };
    let mut splitter = OutputSplitter::new(job.start_in_think);
    let mut pieces = Vec::new();
    let stats = engine.generate(&job.prompt, &job.sampling, &should_stop, &mut |chunk| {
        splitter.push(chunk, &mut pieces);
        emit(job.stream.as_ref(), &mut pieces);
    })?;
    let output = splitter.finish(&mut pieces);
    emit(job.stream.as_ref(), &mut pieces);
    Ok(Generated {
        prompt_tokens: stats.prompt_tokens,
        completion_tokens: stats.completion_tokens,
        finish: stats.finish,
        output,
    })
}

/// Removes a standalone `/think` or `/no_think` switch from `text`.
fn take_switch(text: &mut String) -> Option<bool> {
    for (marker, value) in [("/no_think", false), ("/think", true)] {
        let mut from = 0;
        while let Some(found) = text[from..].find(marker) {
            let start = from + found;
            let end = start + marker.len();
            let boundary = text[end..]
                .chars()
                .next()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
            if boundary {
                text.replace_range(start..end, "");
                *text = text.trim().to_owned();
                return Some(value);
            }
            from = end;
        }
    }
    None
}

fn tool_result_text(result: &ToolCallResult) -> String {
    match result {
        ToolCallResult::Success { value } => match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        },
        ToolCallResult::Error { message } => format!("Error: {message}"),
    }
}

fn tools_json(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|spec| match spec {
            ToolSpec::Function(f) => json!({
                "type": "function",
                "function": {
                    "name": f.name.as_str(),
                    "description": f.description,
                    "parameters": serde_json::to_value(&f.parameters)
                        .unwrap_or_else(|_| json!({"type": "object"})),
                },
            }),
        })
        .collect()
}

/// Chat messages for the template; the second value is a `/think` override.
fn chat_messages(request: &ModelRequest) -> (Vec<ChatMessage>, Option<bool>) {
    let mut system = request.system_prompt.clone();
    for fragment in &request.instruction_fragments {
        if !fragment.trim().is_empty() {
            if !system.is_empty() {
                system.push_str("\n\n");
            }
            system.push_str(fragment);
        }
    }
    let mut messages = Vec::new();
    if !system.trim().is_empty() {
        messages.push(ChatMessage::new("system", system));
    }
    let history: &ConversationHistory = &request.history;
    for item in history.to_model_messages() {
        match item {
            ModelMessage::User { mut text, images } => {
                if !images.is_empty() {
                    text.push_str(&format!(
                        "\n[{} image(s) omitted: this on-device model reads text only]",
                        images.len()
                    ));
                }
                messages.push(ChatMessage::new("user", text));
            }
            ModelMessage::Assistant { text } => messages.push(ChatMessage::new("assistant", text)),
            ModelMessage::ToolCall { name, arguments, .. } => {
                let call = ChatToolCall { name, arguments };
                match messages.last_mut() {
                    Some(last) if last.role == "assistant" => last.tool_calls.push(call),
                    _ => {
                        let mut message = ChatMessage::new("assistant", "");
                        message.tool_calls.push(call);
                        messages.push(message);
                    }
                }
            }
            ModelMessage::ToolResult { result, .. } => {
                messages.push(ChatMessage::new("tool", tool_result_text(&result)));
            }
        }
    }
    if let Some(block) = request.data_block.as_deref().filter(|b| !b.trim().is_empty()) {
        messages.push(ChatMessage::new("user", block));
    }
    let switch = messages
        .iter_mut()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| take_switch(&mut m.content));
    (messages, switch)
}

fn thinking_for(request: &ModelRequest, switch: Option<bool>, default: bool) -> bool {
    switch.unwrap_or(match request.reasoning_effort {
        Some(ReasoningEffort::Minimal) => false,
        Some(_) => true,
        None => default,
    })
}

fn map_error(error: LlamaError) -> ModelError {
    match error {
        LlamaError::ContextLength { .. } => ModelError::ContextLength {
            message: error.to_string(),
        },
        other => ModelError::RequestFailed(other.to_string()),
    }
}

impl LocalGgufProvider {
    fn next_call_id(&self) -> ToolCallId {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        ToolCallId::from_str(format!("call_local_{millis:x}_{n}"))
    }

    fn prepare(&self, request: &ModelRequest) -> Result<Job, ModelError> {
        let (messages, switch) = chat_messages(request);
        let thinking = thinking_for(request, switch, self.config.thinking_default);
        let tools = tools_json(&request.tools);
        let prompt = render_prompt(
            self.engine.chat_template(),
            &RenderInput {
                messages: &messages,
                tools: &tools,
                enable_thinking: thinking,
                bos_token: self.engine.bos_token(),
                eos_token: self.engine.eos_token(),
            },
        )
        .map_err(map_error)?;
        let max_new = request
            .max_output_tokens
            .unwrap_or(self.config.max_new_tokens)
            .clamp(16, self.engine.n_ctx().max(16));
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        Ok(Job {
            start_in_think: prompt_opens_think(&prompt),
            prompt,
            tools,
            sampling: SamplingParams::for_mode(thinking, max_new, seed),
            cancel: request.cancel.clone(),
            stream: request.stream.clone(),
        })
    }

    fn into_response(&self, generated: Generated, tools: &[Value]) -> Result<ModelResponse, ModelError> {
        if generated.finish == FinishReason::Cancelled {
            return Err(ModelError::Cancelled);
        }
        let calls = parse_tool_calls(&generated.output.tool_raw, tools);
        let tool_calls: Vec<ToolCall> = calls
            .into_iter()
            .map(|call| ToolCall {
                id: self.next_call_id(),
                name: ToolName::new(call.name),
                arguments: call.arguments,
            })
            .collect();
        let text = generated.output.text;
        if tool_calls.is_empty() && text.is_empty() {
            return Err(ModelError::EmptyResponse);
        }
        let stop = if !tool_calls.is_empty() {
            StopReason::ToolUse
        } else if matches!(generated.finish, FinishReason::MaxTokens | FinishReason::ContextFull) {
            StopReason::MaxTokens
        } else {
            StopReason::EndTurn
        };
        let usage = TokenUsage {
            input_tokens: generated.prompt_tokens,
            output_tokens: generated.completion_tokens,
            ..TokenUsage::default()
        };
        Ok(ModelResponse {
            message: (!text.is_empty()).then_some(text),
            tool_calls,
            usage,
            stop,
            reasoning: None,
        })
    }
}

impl ModelProvider for LocalGgufProvider {
    fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
        Box::pin(async move {
            let job = self.prepare(&request)?;
            let tools = job.tools.clone();
            let stream = job.stream.clone();
            let abort = Arc::new(AtomicBool::new(false));
            let _abort_on_drop = AbortOnDrop(Arc::clone(&abort));
            let engine = Arc::clone(&self.engine);
            let gate = Arc::clone(&self.gate);
            let worker_abort = Arc::clone(&abort);
            let generated = tokio::task::spawn_blocking(move || {
                run_job(&engine, &gate, &job, &worker_abort)
            })
            .await
            .map_err(|e| ModelError::RequestFailed(format!("generation task failed: {e}")))?
            .map_err(map_error)?;
            let response = self.into_response(generated, &tools)?;
            if let Some(sink) = stream {
                sink.emit(ModelStreamEvent::Usage(response.usage.clone()));
            }
            Ok(response)
        })
    }

    fn pinned_model_id(&self) -> Option<String> {
        Some(self.model_id.clone())
    }

    fn pinned_provider_id(&self) -> Option<String> {
        Some(LOCAL_PROVIDER_ID.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_is_removed_and_reported() {
        let mut text = "hello /no_think".to_owned();
        assert_eq!(take_switch(&mut text), Some(false));
        assert_eq!(text, "hello");
        let mut text = "use /thinking please".to_owned();
        assert_eq!(take_switch(&mut text), None);
        let mut text = "/think why".to_owned();
        assert_eq!(take_switch(&mut text), Some(true));
        assert_eq!(text, "why");
    }
}
