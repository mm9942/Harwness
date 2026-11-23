//! [`harw_core::ModelProvider`] over a local GGUF model.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use harw_core::model::StopReason;
use harw_core::{
    ConversationHistory, ModelError, ModelFuture, ModelMessage, ModelProvider, ModelRequest,
    ModelResponse, ModelStreamEvent, StreamSink,
};
use harw_protocol::items::{OpaqueReasoning, ToolCallResult};
use harw_tools::{ToolCall, ToolName, ToolSpec};
use harw_types::{ReasoningEffort, TokenUsage, ToolCallId};
use serde_json::{Value, json};

use crate::DEFAULT_N_CTX;
use crate::engine::{
    DEFAULT_IMAGE_MAX_TOKENS, DEFAULT_IMAGE_MIN_TOKENS, Engine, EngineConfig, FinishReason,
    MEDIA_MARKER, SamplingParams,
};
use crate::error::LlamaError;
use crate::inference::{InferencePermit, LocalInferenceControl};
use crate::stream::{OutputSplitter, Piece, SplitOutput};
use crate::template::{ChatMessage, ChatToolCall, RenderInput, prompt_opens_think, render_prompt};
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
    /// Text-only chat: a short system prompt and no tool specs. The host's
    /// full prompt and tool catalogue run to tens of thousands of tokens,
    /// which a phone-sized context cannot hold.
    pub lean: bool,
    /// Vision projector (`mmproj` GGUF) of the model. When it loads, images of
    /// the last user message are shown to the model.
    pub mmproj: Option<PathBuf>,
    /// Tokens one image may cost at most (smaller is faster on a phone).
    pub image_max_tokens: i32,
    /// Shared settings and admission gate for local inference.
    pub inference: LocalInferenceControl,
}

impl Default for LocalGgufConfig {
    fn default() -> Self {
        Self {
            n_ctx: DEFAULT_N_CTX,
            n_threads: 0,
            max_new_tokens: 4096,
            thinking_default: false,
            lean: true,
            mmproj: None,
            image_max_tokens: DEFAULT_IMAGE_MAX_TOKENS,
            inference: LocalInferenceControl::default(),
        }
    }
}

/// Runs a GGUF model in-process.
pub struct LocalGgufProvider {
    engine: Arc<Engine>,
    model_id: String,
    config: LocalGgufConfig,
    counter: AtomicU64,
    /// Image descriptions by (image bytes, question) hash.
    descriptions: Mutex<HashMap<u64, String>>,
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
                mmproj: config.mmproj.clone(),
                image_min_tokens: DEFAULT_IMAGE_MIN_TOKENS.min(config.image_max_tokens),
                image_max_tokens: config.image_max_tokens,
                ..EngineConfig::default()
            },
        )?;
        Ok(Self {
            engine: Arc::new(engine),
            model_id: model_id.into(),
            config,
            counter: AtomicU64::new(0),
            descriptions: Mutex::new(HashMap::new()),
        })
    }
}

impl LocalGgufProvider {
    /// Whether the loaded model can read images (its projector is loaded).
    #[must_use]
    pub fn supports_vision(&self) -> bool {
        self.engine.supports_vision()
    }

    /// Why the configured projector could not be loaded, if it could not.
    #[must_use]
    pub fn vision_error(&self) -> Option<&str> {
        self.engine.vision_error()
    }
}

/// Everything a generation needs, owned so it can move to a blocking thread.
struct Job {
    prompt: String,
    images: Vec<Arc<[u8]>>,
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

fn run_job(engine: &Engine, job: &Job, abort: &AtomicBool) -> Result<Generated, LlamaError> {
    let should_stop = || {
        abort.load(Ordering::Relaxed)
            || job
                .cancel
                .as_ref()
                .is_some_and(harw_types::cancel::CancelToken::is_cancelled)
    };
    let mut splitter = OutputSplitter::new(job.start_in_think);
    let mut pieces = Vec::new();
    let images: Vec<&[u8]> = job.images.iter().map(|image| &image[..]).collect();
    let stats = engine.generate(
        &job.prompt,
        &images,
        &job.sampling,
        &should_stop,
        &mut |chunk| {
            splitter.push(chunk, &mut pieces);
            emit(job.stream.as_ref(), &mut pieces);
        },
    )?;
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

/// System prompt of lean (text-only) runs.
const LEAN_SYSTEM_PROMPT: &str = "You are a helpful assistant running on the user's phone. \
Answer concisely and in the language of the user. You have no tools: answer from your own knowledge.";

/// System prompt of lean runs whose model reads images: one tool, `view_image`.
const LEAN_VISION_SYSTEM_PROMPT: &str = "You are a helpful assistant running on the user's phone. \
Answer concisely and in the language of the user. Images in the chat are numbered [Bild N]; \
only the newest ones are visible. To look at an earlier image again, call the tool view_image \
with its number. You have no other tools: answer from your own knowledge.";

const VIEW_IMAGE: &str = "view_image";

/// The only tool of lean vision runs; the provider answers it itself.
fn view_image_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": VIEW_IMAGE,
            "description": "Look at an earlier image of this chat again.",
            "parameters": {
                "type": "object",
                "properties": {
                    "image": {"type": "integer", "description": "Number N of [Bild N]"}
                },
                "required": ["image"],
            },
        },
    })
}

/// Image numbers asked for by `view_image` calls.
fn view_image_numbers(calls: &[ToolCall]) -> Vec<usize> {
    calls
        .iter()
        .filter(|call| call.name.as_str() == VIEW_IMAGE)
        .filter_map(|call| {
            let value = call.arguments.get("image")?;
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|t| t.trim().parse().ok()))
                .and_then(|n| usize::try_from(n).ok())
        })
        .collect()
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

/// Prompt of the per-image vision pass.
const DESCRIBE_PROMPT: &str =
    "Beschreibe das Bild genau und sachlich, mit allem, was f\u{fc}r die Frage wichtig ist.";

/// Tokens one image description may take.
const DESCRIBE_MAX_TOKENS: u32 = 220;

/// Newest images of a chat that are described (older ones stay a bare marker).
const MAX_DESCRIBED_IMAGES: usize = usize::MAX;

fn description_key(data: &[u8], question: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    question.hash(&mut hasher);
    hasher.finish()
}

/// `(image number, question of its message, bytes)` for the images with data,
/// numbered like `chat_messages` does.
fn images_to_describe(request: &ModelRequest) -> Vec<(usize, String, Arc<[u8]>)> {
    let mut found = Vec::new();
    let mut number = 0_usize;
    for item in request.history.to_model_messages() {
        if let ModelMessage::User { text, images } = item {
            for image in images {
                number += 1;
                if let Some(data) = image.data {
                    found.push((number, text.clone(), data));
                }
            }
        }
    }
    let skip = found.len().saturating_sub(MAX_DESCRIBED_IMAGES);
    found.split_off(skip)
}

/// Most images shown to the model at once (each costs up to `image_max_tokens`).
const MAX_SHOWN_IMAGES: usize = 3;

/// Most images a message may name for a re-view.
const MAX_REFERENCED_IMAGES: usize = 4;

/// Image numbers a user text refers to: "Bild 2", "image 3", "das erste Bild".
fn referenced_images(text: &str) -> Vec<usize> {
    const ORDINALS: [(&str, usize); 5] = [
        ("erste", 1),
        ("zweite", 2),
        ("dritte", 3),
        ("vierte", 4),
        ("fuenfte", 5),
    ];
    let lower = text
        .to_lowercase()
        .replace('\u{e4}', "ae")
        .replace('\u{fc}', "ue");
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let mut found = Vec::new();
    for (i, word) in words.iter().enumerate() {
        let is_image = word.starts_with("bild") || word.starts_with("image");
        if !is_image {
            continue;
        }
        if let Some(number) = words.get(i + 1).and_then(|w| w.parse::<usize>().ok()) {
            found.push(number);
        }
        if let Some(prev) = i.checked_sub(1).and_then(|p| words.get(p)) {
            if let Some((_, number)) = ORDINALS.iter().find(|(name, _)| prev.starts_with(name)) {
                found.push(*number);
            }
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

/// What `chat_messages` prepared.
struct Chat {
    messages: Vec<ChatMessage>,
    /// A `/think` override typed by the user.
    switch: Option<bool>,
    /// Images of the last user message, in marker order (vision models only).
    images: Vec<Arc<[u8]>>,
}

/// Chat messages for the template. With `vision`, the newest images of the
/// history are shown to the model (one media marker each); every other image is
/// replaced by a short note.
fn chat_messages(
    request: &ModelRequest,
    lean: bool,
    vision: bool,
    projector_failure: Option<&str>,
    extra_refs: &[usize],
    described: &HashMap<usize, String>,
) -> Chat {
    let mut system = if lean && vision {
        LEAN_VISION_SYSTEM_PROMPT.to_owned()
    } else if lean {
        LEAN_SYSTEM_PROMPT.to_owned()
    } else {
        request.system_prompt.clone()
    };
    if !described.is_empty() {
        system.push_str(
            "\nImages arrive as text descriptions marked [Bild N]; treat them as what you see.",
        );
    }
    for fragment in request.instruction_fragments.iter().filter(|_| !lean) {
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
    let items = history.to_model_messages();
    // Every attached image gets a running number ("[Bild N]") in the prompt. The
    // lean local chat has no tools, so earlier images are re-shown by a plain
    // rule: when the newest user text names images ("Bild 2", "das erste
    // Bild"), exactly those are sent again; otherwise the newest few are.
    let mut numbered: Vec<(usize, usize, bool)> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if let ModelMessage::User { images, .. } = item {
            for (n, image) in images.iter().enumerate() {
                numbered.push((index, n, image.data.is_some()));
            }
        }
    }
    let newest_text = items.iter().rev().find_map(|item| match item {
        ModelMessage::User { text, .. } => Some(text.as_str()),
        _ => None,
    });
    let mut referenced = newest_text.map(referenced_images).unwrap_or_default();
    referenced.extend_from_slice(extra_refs);
    referenced.sort_unstable();
    referenced.dedup();
    let mut visible: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let by_reference: Vec<_> = referenced
        .iter()
        .filter_map(|number| numbered.get(number.wrapping_sub(1)))
        .filter(|(_, _, has_data)| *has_data)
        .take(MAX_REFERENCED_IMAGES)
        .collect();
    if by_reference.is_empty() {
        for (index, n, has_data) in numbered.iter().rev().take(MAX_SHOWN_IMAGES) {
            if *has_data {
                visible.insert((*index, *n));
            }
        }
    } else {
        for (index, n, _) in by_reference {
            visible.insert((*index, *n));
        }
    }
    let number_of = |index: usize, n: usize| {
        numbered
            .iter()
            .position(|(i, k, _)| *i == index && *k == n)
            .map_or(0, |p| p + 1)
    };
    let mut shown: Vec<Arc<[u8]>> = Vec::new();
    for (index, item) in items.into_iter().enumerate() {
        match item {
            ModelMessage::User { mut text, images } => {
                let mut omitted = 0_usize;
                let mut markers = String::new();
                for (n, image) in images.iter().enumerate() {
                    match image.data.as_ref() {
                        Some(data) if vision && visible.contains(&(index, n)) => {
                            markers.push_str(&format!("[Bild {}]\n", number_of(index, n)));
                            markers.push_str(MEDIA_MARKER);
                            markers.push('\n');
                            shown.push(Arc::clone(data));
                        }
                        Some(_) if described.contains_key(&number_of(index, n)) => {
                            let number = number_of(index, n);
                            let description = described.get(&number).map_or("", String::as_str);
                            text.push_str(&format!("\n[Bild {number}] {description}"));
                        }
                        Some(_) => {
                            omitted += 1;
                            text.push_str(&format!("\n[Bild {}]", number_of(index, n)));
                        }
                        None => {
                            text.push('\n');
                            text.push_str(&image.placeholder("the image data is not available"));
                        }
                    }
                }
                if omitted > 0 {
                    let why = if vision {
                        "only the newest images are shown".to_owned()
                    } else if let Some(reason) = projector_failure {
                        format!("the vision projector failed to load ({reason})")
                    } else {
                        "this on-device model reads text only".to_owned()
                    };
                    text.push_str(&format!("\n[{omitted} image(s) omitted: {why}]"));
                }
                if markers.is_empty() {
                    messages.push(ChatMessage::new("user", text));
                } else {
                    messages.push(ChatMessage::new("user", format!("{markers}{text}")));
                }
            }
            ModelMessage::Assistant { text } => messages.push(ChatMessage::new("assistant", text)),
            ModelMessage::ToolCall {
                name, arguments, ..
            } => {
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
    if let Some(block) = request
        .data_block
        .as_deref()
        .filter(|b| !b.trim().is_empty())
    {
        messages.push(ChatMessage::new("user", block));
    }
    let switch = messages
        .iter_mut()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| take_switch(&mut m.content));
    Chat {
        messages,
        switch,
        images: shown,
    }
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

    fn prepare(
        &self,
        request: &ModelRequest,
        forced: &[usize],
        described: &HashMap<usize, String>,
    ) -> Result<Job, ModelError> {
        // With a projector every image is described in its own short pass first;
        // the answering run only sees the descriptions as text.
        let describe_mode = self.config.lean && self.engine.supports_vision();
        let Chat {
            messages,
            switch,
            images,
        } = chat_messages(
            request,
            self.config.lean,
            self.engine.supports_vision() && !describe_mode,
            self.engine.vision_error(),
            forced,
            described,
        );
        let thinking = thinking_for(request, switch, self.config.thinking_default);
        let tools = if self.config.lean {
            if self.engine.supports_vision() && !describe_mode {
                vec![view_image_tool()]
            } else {
                Vec::new()
            }
        } else {
            tools_json(&request.tools)
        };
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
            images,
            tools,
            sampling: SamplingParams::for_mode(thinking, max_new, seed),
            cancel: request.cancel.clone(),
            stream: request.stream.clone(),
        })
    }

    fn describe_job(
        &self,
        request: &ModelRequest,
        question: &str,
        data: Arc<[u8]>,
    ) -> Result<Job, ModelError> {
        let question = question.trim();
        let question = if question.is_empty() {
            "(keine Frage)"
        } else {
            question
        };
        let messages = vec![ChatMessage::new(
            "user",
            format!("{MEDIA_MARKER}\nFrage des Nutzers: {question}\n\n{DESCRIBE_PROMPT}"),
        )];
        let no_tools: Vec<Value> = Vec::new();
        let prompt = render_prompt(
            self.engine.chat_template(),
            &RenderInput {
                messages: &messages,
                tools: &no_tools,
                enable_thinking: false,
                bos_token: self.engine.bos_token(),
                eos_token: self.engine.eos_token(),
            },
        )
        .map_err(map_error)?;
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        Ok(Job {
            start_in_think: prompt_opens_think(&prompt),
            prompt,
            images: vec![data],
            tools: no_tools,
            sampling: SamplingParams::for_mode(false, DESCRIBE_MAX_TOKENS, seed),
            cancel: request.cancel.clone(),
            stream: None,
        })
    }

    /// Describes each uncached image in its own context, up to the admitted limit.
    async fn describe_images(
        &self,
        request: &ModelRequest,
        turn: &InferencePermit,
    ) -> Result<HashMap<usize, String>, ModelError> {
        let mut result = HashMap::new();
        let todo = images_to_describe(request);
        let total = todo.len();
        let parallel = turn.image_concurrency();
        let mut pending = Vec::new();
        for (number, question, data) in todo {
            let key = description_key(&data, &question);
            let cached = self
                .descriptions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&key)
                .cloned();
            if let Some(text) = cached {
                result.insert(number, text);
                continue;
            }
            let job = self.describe_job(request, &question, data)?;
            pending.push((number, key, job));
        }
        let progress = |finished: usize, running: usize| {
            if let Some(sink) = request.stream.as_ref() {
                sink.emit(ModelStreamEvent::ReasoningDelta(format!(
                    "Bildanalyse: {finished}/{total} fertig · {running} laufen\n"
                )));
            }
        };
        let mut pending = pending.into_iter();
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            while tasks.len() < parallel {
                let Some((number, key, job)) = pending.next() else {
                    break;
                };
                let (handle, guard) = self.spawn_job(job, turn);
                tasks.spawn(async move {
                    let _guard = guard;
                    (number, key, handle.await)
                });
            }
            if tasks.is_empty() {
                break;
            }
            progress(result.len(), tasks.len());
            let Some(done) = tasks.join_next().await else {
                break;
            };
            let (number, key, generated) =
                done.map_err(|e| ModelError::RequestFailed(format!("image task failed: {e}")))?;
            let generated = generated
                .map_err(|e| ModelError::RequestFailed(format!("generation task failed: {e}")))?
                .map_err(map_error)?;
            if generated.finish == FinishReason::Cancelled {
                return Err(ModelError::Cancelled);
            }
            let text = generated.output.text.trim().to_owned();
            self.descriptions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, text.clone());
            result.insert(number, text);
        }
        if total != 0 {
            progress(result.len(), 0);
        }
        Ok(result)
    }

    fn spawn_job(
        &self,
        job: Job,
        turn: &InferencePermit,
    ) -> (
        tokio::task::JoinHandle<Result<Generated, LlamaError>>,
        AbortOnDrop,
    ) {
        let engine = Arc::clone(&self.engine);
        let turn = turn.clone();
        let abort = Arc::new(AtomicBool::new(false));
        let abort_on_drop = AbortOnDrop(Arc::clone(&abort));
        let handle = tokio::task::spawn_blocking(move || {
            let _permit = turn;
            run_job(&engine, &job, &abort)
        });
        (handle, abort_on_drop)
    }

    fn into_response(
        &self,
        generated: Generated,
        tools: &[Value],
    ) -> Result<ModelResponse, ModelError> {
        if generated.finish == FinishReason::Cancelled {
            return Err(ModelError::Cancelled);
        }
        let calls = parse_tool_calls(&generated.output.tool_raw, tools);
        let tool_calls: Vec<ToolCall> = calls
            .into_iter()
            .filter(|call| !(self.config.lean && call.name == VIEW_IMAGE))
            .map(|call| ToolCall {
                id: self.next_call_id(),
                name: ToolName::new(call.name),
                arguments: call.arguments,
            })
            .collect();
        let text = generated.output.text;
        // This is the same display text already emitted as a live delta by
        // the local model, not an opaque replay block. Keep the useful tail.
        let reasoning = generated.output.reasoning;
        let reasoning = if reasoning.is_empty() {
            None
        } else {
            let start = reasoning.ceil_char_boundary(reasoning.len().saturating_sub(16 * 1024));
            Some(OpaqueReasoning {
                provider: LOCAL_PROVIDER_ID.to_owned(),
                model: LOCAL_PROVIDER_ID.to_owned(),
                blocks: vec![json!({"type": "reasoning_content", "text": &reasoning[start..]})],
            })
        };
        if tool_calls.is_empty() && text.is_empty() {
            return Err(ModelError::EmptyResponse);
        }
        let stop = if !tool_calls.is_empty() {
            StopReason::ToolUse
        } else if matches!(
            generated.finish,
            FinishReason::MaxTokens | FinishReason::ContextFull
        ) {
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
            reasoning,
        })
    }
}

impl ModelProvider for LocalGgufProvider {
    fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
        Box::pin(async move {
            let turn = self
                .config
                .inference
                .begin(request.cancel.as_ref())
                .await
                .ok_or(ModelError::Cancelled)?;
            let mut forced: Vec<usize> = Vec::new();
            let mut round = 0;
            let described = if self.config.lean && self.engine.supports_vision() {
                self.describe_images(&request, &turn).await?
            } else {
                HashMap::new()
            };
            let (generated, tools, stream) = loop {
                let job = self.prepare(&request, &forced, &described)?;
                let tools = job.tools.clone();
                let stream = job.stream.clone();
                let (handle, _abort_on_drop) = self.spawn_job(job, &turn);
                let generated = handle
                    .await
                    .map_err(|e| ModelError::RequestFailed(format!("generation task failed: {e}")))?
                    .map_err(map_error)?;
                // `view_image` is answered here: the asked-for images are put into
                // the prompt and the model answers again (at most twice).
                if self.config.lean && round < 2 && generated.finish != FinishReason::Cancelled {
                    let calls: Vec<ToolCall> = parse_tool_calls(&generated.output.tool_raw, &tools)
                        .into_iter()
                        .map(|call| ToolCall {
                            id: self.next_call_id(),
                            name: ToolName::new(call.name),
                            arguments: call.arguments,
                        })
                        .collect();
                    let wanted = view_image_numbers(&calls);
                    if !wanted.is_empty() && wanted.iter().any(|n| !forced.contains(n)) {
                        forced.extend(wanted);
                        round += 1;
                        continue;
                    }
                }
                break (generated, tools, stream);
            };
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
