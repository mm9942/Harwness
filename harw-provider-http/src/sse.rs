//! Server-Sent-Events: generischer Zeilen-Decoder plus Akkumulatoren, die
//! einen gestreamten Anthropic-Messages- bzw. OpenAI-Chat-Stream in genau
//! den JSON-Body zurückbauen, den die nicht-gestreamte API geliefert hätte.
//!
//! Damit bleiben die bestehenden `extract_*`-Funktionen die einzige
//! Parse-Logik für die finale Antwort; das Streaming fügt nur Live-Deltas
//! über [`harw_core::StreamSink`] hinzu.

use harw_core::{ModelError, ModelStreamEvent, StreamSink};
use harw_types::TokenUsage;
use serde_json::{Map, Value};

/// Obergrenze eines einzelnen SSE-Events (Schutz gegen endlose Zeilen).
const MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;

/// Ein dekodiertes SSE-Event (`event:`-Name optional, `data:` verbunden).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

/// Inkrementeller SSE-Decoder: nimmt beliebig zerschnittene Byte-Chunks an
/// und liefert vollständige Frames.
#[derive(Debug, Default)]
pub(crate) struct SseDecoder {
    line: Vec<u8>,
    event: Option<String>,
    data: String,
    has_data: bool,
}

impl SseDecoder {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseFrame>, ModelError> {
        let mut frames = Vec::new();
        for &byte in bytes {
            if byte != b'\n' {
                if self.line.len() + self.data.len() >= MAX_EVENT_BYTES {
                    return Err(ModelError::RequestFailed(
                        "SSE event exceeds size limit".into(),
                    ));
                }
                self.line.push(byte);
                continue;
            }
            if self.line.last() == Some(&b'\r') {
                self.line.pop();
            }
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.line)).into_owned();
            if line.is_empty() {
                if self.has_data || self.event.is_some() {
                    frames.push(SseFrame {
                        event: self.event.take(),
                        data: std::mem::take(&mut self.data),
                    });
                    self.has_data = false;
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (line.as_str(), ""),
            };
            match field {
                "event" => self.event = Some(value.to_owned()),
                "data" => {
                    if self.has_data {
                        self.data.push('\n');
                    }
                    self.data.push_str(value);
                    self.has_data = true;
                }
                _ => {}
            }
        }
        Ok(frames)
    }
}

/// Liest den Body eines `reqwest::Response` als SSE und reicht jeden Frame an
/// `on_frame` weiter; `on_frame` liefert `true`, sobald der Stream fertig ist.
///
/// # Arguments
/// - `response`: die Antwort mit SSE-Body.
/// - `idle_timeout`: Runde 7, Teil L4 — höchstens so lange darf zwischen
///   zwei empfangenen Chunks vergehen; `None` = unbegrenzt. Es gibt bewusst
///   kein Gesamt-Zeitlimit: ein stetig fließender Stream läuft beliebig lange.
/// - `on_frame`: Verarbeitung je Frame.
///
/// # Errors
/// [`ModelError::Timeout`], wenn `idle_timeout` ohne ein Byte verstreicht;
/// Transport-, Größen- und Frame-Fehler unverändert.
pub(crate) async fn read_sse(
    mut response: reqwest::Response,
    idle_timeout: Option<std::time::Duration>,
    mut on_frame: impl FnMut(SseFrame) -> Result<bool, ModelError>,
) -> Result<(), ModelError> {
    let mut decoder = SseDecoder::default();
    loop {
        let next = match idle_timeout {
            Some(idle) => match tokio::time::timeout(idle, response.chunk()).await {
                Ok(next) => next,
                Err(_elapsed) => {
                    return Err(ModelError::Timeout {
                        message: format!(
                            "provider stream was idle for {} s (stream_idle_timeout_secs)",
                            idle.as_secs()
                        ),
                    });
                }
            },
            None => response.chunk().await,
        };
        let Some(chunk) =
            next.map_err(|error| crate::error::model_error_for_transport(error, true))?
        else {
            break;
        };
        for frame in decoder.push(&chunk)? {
            if on_frame(frame)? {
                return Ok(());
            }
        }
    }
    // Ein letzter, nicht durch Leerzeile abgeschlossener Frame.
    for frame in decoder.push(b"\n\n")? {
        if on_frame(frame)? {
            return Ok(());
        }
    }
    Ok(())
}

/// Entscheidet pro Modell, ob Token-Streaming (SSE) genutzt wird.
///
/// Rangfolge: `ModelToml::stream` (per ID **und** Alias) → `ProviderToml::stream`
/// → `true` (alle nativen Anthropic-/OpenAI-Transporte unterstützen SSE).
/// `false` erzwingt den Pro-Runde-Fallback.
#[derive(Debug, Clone)]
pub(crate) struct StreamPolicy {
    default: bool,
    overrides: std::collections::HashMap<String, bool>,
}

impl Default for StreamPolicy {
    fn default() -> Self {
        Self {
            default: true,
            overrides: std::collections::HashMap::new(),
        }
    }
}

impl StreamPolicy {
    pub(crate) fn from_config(
        provider_name: &str,
        provider: &harw_config::ProviderToml,
        config: &harw_config::ResolvedConfig,
    ) -> Self {
        let mut overrides = std::collections::HashMap::new();
        for model in config
            .models
            .values()
            .filter(|m| m.provider == provider_name)
        {
            if let Some(stream) = model.stream {
                overrides.insert(model.id.clone(), stream);
                for alias in &model.aliases {
                    overrides.insert(alias.clone(), stream);
                }
            }
        }
        Self {
            default: provider.stream.unwrap_or(true),
            overrides,
        }
    }

    /// `true`, wenn für `model` gestreamt werden soll.
    pub(crate) fn enabled(&self, model: &str) -> bool {
        self.overrides.get(model).copied().unwrap_or(self.default)
    }
}

fn emit(sink: Option<&StreamSink>, event: ModelStreamEvent) {
    if let Some(sink) = sink {
        sink.emit(event);
    }
}

// ---------------------------------------------------------------------------
// Anthropic Messages
// ---------------------------------------------------------------------------

/// Baut aus Anthropic-Messages-Stream-Events den nicht-gestreamten Body.
#[derive(Debug, Default)]
pub(crate) struct AnthropicStreamAccumulator {
    message: Map<String, Value>,
    blocks: Vec<Value>,
    partial_json: Vec<String>,
    usage: Map<String, Value>,
    done: bool,
}

impl AnthropicStreamAccumulator {
    /// Verarbeitet einen Frame; `Ok(true)` nach `message_stop`.
    pub(crate) fn push(
        &mut self,
        frame: &SseFrame,
        sink: Option<&StreamSink>,
    ) -> Result<bool, ModelError> {
        if frame.data.is_empty() {
            return Ok(false);
        }
        let event: Value = serde_json::from_str(&frame.data)?;
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .or(frame.event.as_deref())
            .unwrap_or_default();
        match kind {
            "message_start" => {
                if let Some(Value::Object(message)) = event.get("message") {
                    self.message = message.clone();
                    if let Some(Value::Object(usage)) = message.get("usage") {
                        self.usage = usage.clone();
                    }
                }
                emit(sink, ModelStreamEvent::Usage(self.usage()));
            }
            "content_block_start" => {
                let index = block_index(&event);
                let block = event.get("content_block").cloned().unwrap_or(Value::Null);
                self.ensure(index);
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    emit(
                        sink,
                        ModelStreamEvent::ToolCallDelta {
                            index,
                            id: block.get("id").and_then(Value::as_str).map(str::to_owned),
                            name: block.get("name").and_then(Value::as_str).map(str::to_owned),
                            arguments_fragment: String::new(),
                        },
                    );
                }
                self.blocks[index] = block;
            }
            "content_block_delta" => {
                let index = block_index(&event);
                self.ensure(index);
                let delta = event.get("delta").cloned().unwrap_or(Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
                        append_str(&mut self.blocks[index], "text", text);
                        emit(sink, ModelStreamEvent::TextDelta(text.to_owned()));
                    }
                    Some("thinking_delta") => {
                        let text = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
                        append_str(&mut self.blocks[index], "thinking", text);
                        emit(sink, ModelStreamEvent::ReasoningDelta(text.to_owned()));
                    }
                    Some("signature_delta") => {
                        let sig = delta.get("signature").and_then(Value::as_str).unwrap_or("");
                        append_str(&mut self.blocks[index], "signature", sig);
                    }
                    Some("input_json_delta") => {
                        let part = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        self.partial_json[index].push_str(part);
                        emit(
                            sink,
                            ModelStreamEvent::ToolCallDelta {
                                index,
                                id: None,
                                name: None,
                                arguments_fragment: part.to_owned(),
                            },
                        );
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = block_index(&event);
                self.ensure(index);
                self.finish_block(index)?;
            }
            "message_delta" => {
                if let Some(Value::Object(delta)) = event.get("delta") {
                    for (key, value) in delta {
                        self.message.insert(key.clone(), value.clone());
                    }
                }
                if let Some(Value::Object(usage)) = event.get("usage") {
                    for (key, value) in usage {
                        if !value.is_null() {
                            self.usage.insert(key.clone(), value.clone());
                        }
                    }
                }
                emit(sink, ModelStreamEvent::Usage(self.usage()));
            }
            "message_stop" => {
                self.done = true;
                return Ok(true);
            }
            "error" => {
                let message = event
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown stream error");
                let error_type = event
                    .get("error")
                    .and_then(|error| error.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                return Err(if error_type == "overloaded_error" {
                    ModelError::Transient {
                        status: None,
                        retry_after_secs: None,
                        message: format!("Anthropic stream error: {message}"),
                    }
                } else {
                    ModelError::RequestFailed(format!("Anthropic stream error: {message}"))
                });
            }
            _ => {}
        }
        Ok(false)
    }

    fn ensure(&mut self, index: usize) {
        while self.blocks.len() <= index {
            self.blocks.push(Value::Null);
            self.partial_json.push(String::new());
        }
    }

    fn finish_block(&mut self, index: usize) -> Result<(), ModelError> {
        let block = &mut self.blocks[index];
        if block.get("type").and_then(Value::as_str) == Some("tool_use") {
            let raw = std::mem::take(&mut self.partial_json[index]);
            let input = if raw.trim().is_empty() {
                block
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Map::new()))
            } else {
                serde_json::from_str(&raw)?
            };
            if let Some(object) = block.as_object_mut() {
                object.insert("input".into(), input);
            }
        }
        Ok(())
    }

    fn usage(&self) -> TokenUsage {
        crate::anthropic::extract_anthropic_usage(
            &serde_json::json!({ "usage": Value::Object(self.usage.clone()) }),
        )
    }

    /// Der rekonstruierte, nicht-gestreamte Body.
    pub(crate) fn finish(mut self) -> Result<Value, ModelError> {
        if !self.done && self.message.is_empty() {
            return Err(ModelError::Truncated {
                message: "Anthropic stream ended before message_start".into(),
            });
        }
        for index in 0..self.blocks.len() {
            if !self.partial_json[index].is_empty() {
                self.finish_block(index)?;
            }
        }
        let blocks = self.blocks.into_iter().filter(|b| !b.is_null()).collect();
        self.message.insert("content".into(), Value::Array(blocks));
        self.message
            .insert("usage".into(), Value::Object(self.usage));
        if !self.done && !self.message.contains_key("stop_reason") {
            self.message
                .insert("stop_reason".into(), Value::String("max_tokens".into()));
        }
        Ok(Value::Object(self.message))
    }
}

fn block_index(event: &Value) -> usize {
    event
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|i| usize::try_from(i).ok())
        .unwrap_or(0)
        .min(1024)
}

fn append_str(block: &mut Value, key: &str, text: &str) {
    if let Some(object) = block.as_object_mut() {
        let entry = object
            .entry(key.to_owned())
            .or_insert_with(|| Value::String(String::new()));
        if let Value::String(existing) = entry {
            existing.push_str(text);
        } else {
            *entry = Value::String(text.to_owned());
        }
    }
}

// ---------------------------------------------------------------------------
// OpenAI Chat Completions
// ---------------------------------------------------------------------------

/// Öffnender Denk-Marker lokaler Reasoning-Modelle (Qwen3, DeepSeek-R1, …).
const THINK_OPEN: &str = "<think>";
/// Schließender Denk-Marker.
const THINK_CLOSE: &str = "</think>";
/// Marker, ab denen der restliche Text Tool-Call-Syntax ist und nicht mehr
/// live angezeigt wird (Hermes/Qwen, Mistral, Llama).
const TOOL_CALL_MARKERS: [&str; 3] = ["<tool_call>", "[TOOL_CALLS]", "<|python_tag|>"];

/// Ein Stück live weiterzugebender Ausgabe (siehe [`LiveTextFilter`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum LiveOut {
    /// Sichtbarer Antworttext.
    Text(String),
    /// Denktext aus einem `<think>`-Block.
    Reasoning(String),
}

/// Runde 7, Teil L7: Filtert den **Live**-Textstrom eines Chat-Streams.
///
/// # Description
/// - `<think>…</think>` wird als Reasoning statt als Antworttext gemeldet;
///   ein verwaistes `</think>` wird verschluckt.
/// - Ab `<tool_call>`, `[TOOL_CALLS]` oder `<|python_tag|>` wird nichts mehr
///   live angezeigt — das ist Tool-Call-Syntax, die erst am Ende
///   (`interpret_chat`) geparst wird.
/// - Ein Textende, das der Anfang eines Markers sein könnte (z. B. `<th`),
///   wird bis zum nächsten Chunk zurückgehalten, damit über Chunk-Grenzen
///   zerschnittene Marker nie sichtbar werden.
///
/// Der akkumulierte Rohtext (für die finale Antwort) bleibt unberührt.
#[derive(Debug, Default)]
struct LiveTextFilter {
    pending: String,
    in_think: bool,
    suppressed: bool,
}

impl LiveTextFilter {
    /// Nimmt ein Text-Delta auf und liefert das, was jetzt sicher live
    /// weitergegeben werden kann.
    fn push(&mut self, text: &str) -> Vec<LiveOut> {
        let mut out = Vec::new();
        if self.suppressed {
            return out;
        }
        self.pending.push_str(text);
        loop {
            if self.suppressed {
                self.pending.clear();
                break;
            }
            if self.in_think {
                if let Some(index) = self.pending.find(THINK_CLOSE) {
                    push_out(
                        &mut out,
                        LiveOut::Reasoning(self.pending[..index].to_owned()),
                    );
                    self.pending.drain(..index + THINK_CLOSE.len());
                    self.in_think = false;
                    continue;
                }
                let keep = held_back_len(&self.pending, &[THINK_CLOSE]);
                let emit_len = self.pending.len() - keep;
                push_out(
                    &mut out,
                    LiveOut::Reasoning(self.pending[..emit_len].to_owned()),
                );
                self.pending.drain(..emit_len);
                break;
            }
            let earliest = [THINK_OPEN, THINK_CLOSE]
                .iter()
                .chain(TOOL_CALL_MARKERS.iter())
                .filter_map(|marker| self.pending.find(marker).map(|index| (index, *marker)))
                .min_by_key(|(index, _)| *index);
            if let Some((index, marker)) = earliest {
                push_out(&mut out, LiveOut::Text(self.pending[..index].to_owned()));
                self.pending.drain(..index + marker.len());
                if marker == THINK_OPEN {
                    self.in_think = true;
                } else if marker != THINK_CLOSE {
                    self.suppressed = true;
                }
                continue;
            }
            let mut markers = vec![THINK_OPEN, THINK_CLOSE];
            markers.extend(TOOL_CALL_MARKERS);
            let keep = held_back_len(&self.pending, &markers);
            let emit_len = self.pending.len() - keep;
            push_out(&mut out, LiveOut::Text(self.pending[..emit_len].to_owned()));
            self.pending.drain(..emit_len);
            break;
        }
        out
    }

    /// Gibt am Stream-Ende zurückgehaltenen Text frei.
    fn flush(&mut self) -> Vec<LiveOut> {
        let pending = std::mem::take(&mut self.pending);
        let mut out = Vec::new();
        if self.suppressed {
            return out;
        }
        if self.in_think {
            push_out(&mut out, LiveOut::Reasoning(pending));
        } else {
            push_out(&mut out, LiveOut::Text(pending));
        }
        out
    }
}

/// Hängt `item` an, sofern es nicht leer ist.
fn push_out(out: &mut Vec<LiveOut>, item: LiveOut) {
    let empty = match &item {
        LiveOut::Text(text) | LiveOut::Reasoning(text) => text.is_empty(),
    };
    if !empty {
        out.push(item);
    }
}

/// Länge des längsten Suffixes von `text`, das ein echter Präfix eines der
/// `markers` ist (muss bis zum nächsten Chunk zurückgehalten werden).
fn held_back_len(text: &str, markers: &[&str]) -> usize {
    let longest = markers.iter().map(|marker| marker.len()).max().unwrap_or(0);
    let upper = longest.saturating_sub(1).min(text.len());
    (1..=upper)
        .rev()
        .find(|&len| {
            let start = text.len() - len;
            text.is_char_boundary(start)
                && markers
                    .iter()
                    .any(|marker| marker.len() > len && marker.starts_with(&text[start..]))
        })
        .unwrap_or(0)
}

/// Gibt die Ausgaben des [`LiveTextFilter`] an den Sink weiter.
fn emit_live(sink: Option<&StreamSink>, outputs: Vec<LiveOut>) {
    for output in outputs {
        match output {
            LiveOut::Text(text) => emit(sink, ModelStreamEvent::TextDelta(text)),
            LiveOut::Reasoning(text) => emit(sink, ModelStreamEvent::ReasoningDelta(text)),
        }
    }
}

/// Baut aus `chat.completion.chunk`-Events einen `chat.completion`-Body.
#[derive(Debug, Default)]
pub(crate) struct ChatStreamAccumulator {
    id: Option<Value>,
    model: Option<Value>,
    content: String,
    reasoning: String,
    has_content: bool,
    tool_calls: Vec<Map<String, Value>>,
    finish_reason: Option<Value>,
    usage: Option<Value>,
    done: bool,
    /// Runde 7, Teil L7: Live-Filter für `<think>`/Tool-Call-Marker.
    live: LiveTextFilter,
}

impl ChatStreamAccumulator {
    /// Verarbeitet einen Frame; `Ok(true)` bei `[DONE]`.
    pub(crate) fn push(
        &mut self,
        frame: &SseFrame,
        sink: Option<&StreamSink>,
    ) -> Result<bool, ModelError> {
        let data = frame.data.trim();
        if data.is_empty() {
            return Ok(false);
        }
        if data == "[DONE]" {
            self.done = true;
            emit_live(sink, self.live.flush());
            return Ok(true);
        }
        let chunk: Value = serde_json::from_str(data)?;
        if let Some(error) = chunk.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown stream error");
            return Err(ModelError::RequestFailed(format!(
                "stream error: {message}"
            )));
        }
        if self.id.is_none() {
            self.id = chunk.get("id").cloned();
            self.model = chunk.get("model").cloned();
        }
        if let Some(usage) = chunk.get("usage").filter(|u| !u.is_null()) {
            self.usage = Some(usage.clone());
            emit(
                sink,
                ModelStreamEvent::Usage(crate::extract_openai_usage(
                    &serde_json::json!({ "usage": usage }),
                    crate::Transport::Chat,
                )),
            );
        }
        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        else {
            return Ok(false);
        };
        let finished = choice.get("finish_reason").filter(|r| !r.is_null());
        if let Some(reason) = finished {
            self.finish_reason = Some(reason.clone());
        }
        let Some(delta) = choice.get("delta") else {
            if finished.is_some() {
                emit_live(sink, self.live.flush());
            }
            return Ok(false);
        };
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            self.has_content = true;
            self.content.push_str(text);
            emit_live(sink, self.live.push(text));
        }
        if finished.is_some() {
            emit_live(sink, self.live.flush());
        }
        for key in ["reasoning_content", "reasoning"] {
            if let Some(text) = delta.get(key).and_then(Value::as_str) {
                self.reasoning.push_str(text);
                emit(sink, ModelStreamEvent::ReasoningDelta(text.to_owned()));
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index = call
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|i| usize::try_from(i).ok())
                    .unwrap_or(0)
                    .min(1024);
                while self.tool_calls.len() <= index {
                    let mut entry = Map::new();
                    entry.insert("type".into(), Value::String("function".into()));
                    let mut function = Map::new();
                    function.insert("name".into(), Value::String(String::new()));
                    function.insert("arguments".into(), Value::String(String::new()));
                    entry.insert("function".into(), Value::Object(function));
                    self.tool_calls.push(entry);
                }
                let entry = &mut self.tool_calls[index];
                let id = call.get("id").and_then(Value::as_str);
                if let Some(id) = id {
                    entry.insert("id".into(), Value::String(id.to_owned()));
                }
                let function = call.get("function");
                let name = function.and_then(|f| f.get("name")).and_then(Value::as_str);
                let args = function
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if let Some(Value::Object(target)) = entry.get_mut("function") {
                    if let Some(name) = name {
                        append_str_map(target, "name", name);
                    }
                    append_str_map(target, "arguments", args);
                }
                emit(
                    sink,
                    ModelStreamEvent::ToolCallDelta {
                        index,
                        id: id.map(str::to_owned),
                        name: name.map(str::to_owned),
                        arguments_fragment: args.to_owned(),
                    },
                );
            }
        }
        Ok(false)
    }

    /// Der rekonstruierte `chat.completion`-Body.
    pub(crate) fn finish(self) -> Result<Value, ModelError> {
        if !self.done && self.finish_reason.is_none() && self.id.is_none() {
            return Err(ModelError::Truncated {
                message: "chat stream ended without any chunk".into(),
            });
        }
        let mut message = Map::new();
        message.insert("role".into(), Value::String("assistant".into()));
        message.insert(
            "content".into(),
            if self.has_content {
                Value::String(self.content)
            } else {
                Value::Null
            },
        );
        if !self.reasoning.is_empty() {
            message.insert("reasoning_content".into(), Value::String(self.reasoning));
        }
        if !self.tool_calls.is_empty() {
            message.insert(
                "tool_calls".into(),
                Value::Array(self.tool_calls.into_iter().map(Value::Object).collect()),
            );
        }
        let mut choice = Map::new();
        choice.insert("index".into(), Value::from(0));
        choice.insert("message".into(), Value::Object(message));
        choice.insert(
            "finish_reason".into(),
            self.finish_reason
                .unwrap_or_else(|| Value::String(if self.done { "stop" } else { "length" }.into())),
        );
        let mut body = Map::new();
        body.insert("object".into(), Value::String("chat.completion".into()));
        if let Some(id) = self.id {
            body.insert("id".into(), id);
        }
        if let Some(model) = self.model {
            body.insert("model".into(), model);
        }
        body.insert("choices".into(), Value::Array(vec![Value::Object(choice)]));
        if let Some(usage) = self.usage {
            body.insert("usage".into(), usage);
        }
        Ok(Value::Object(body))
    }
}

fn append_str_map(object: &mut Map<String, Value>, key: &str, text: &str) {
    match object.get_mut(key) {
        Some(Value::String(existing)) => existing.push_str(text),
        _ => {
            object.insert(key.to_owned(), Value::String(text.to_owned()));
        }
    }
}

// ---------------------------------------------------------------------------
// OpenAI Responses (Deltas; das Terminal-Event trägt den vollständigen Body)
// ---------------------------------------------------------------------------

/// Leitet Deltas eines Responses-API-Events an den Sink weiter und liefert
/// den finalen `response`-Body, sobald `response.completed`/`.incomplete`
/// eintrifft.
pub(crate) fn responses_event(
    event: &Value,
    sink: Option<&StreamSink>,
) -> Result<Option<Value>, ModelError> {
    match event.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta") => {
            if let Some(text) = event.get("delta").and_then(Value::as_str) {
                emit(sink, ModelStreamEvent::TextDelta(text.to_owned()));
            }
        }
        Some("response.reasoning_summary_text.delta" | "response.reasoning_text.delta") => {
            if let Some(text) = event.get("delta").and_then(Value::as_str) {
                emit(sink, ModelStreamEvent::ReasoningDelta(text.to_owned()));
            }
        }
        Some("response.function_call_arguments.delta") => {
            let index = event
                .get("output_index")
                .and_then(Value::as_u64)
                .and_then(|i| usize::try_from(i).ok())
                .unwrap_or(0);
            emit(
                sink,
                ModelStreamEvent::ToolCallDelta {
                    index,
                    id: event
                        .get("item_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    name: None,
                    arguments_fragment: event
                        .get("delta")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                },
            );
        }
        Some("response.completed" | "response.incomplete") => {
            let mut response = event
                .get("response")
                .filter(|value| value.is_object())
                .cloned()
                .ok_or_else(|| ModelError::RequestFailed("missing terminal response".into()))?;
            if event["type"] == "response.incomplete" {
                response["status"] = Value::String("incomplete".into());
            }
            emit(
                sink,
                ModelStreamEvent::Usage(crate::extract_openai_usage(
                    &response,
                    crate::Transport::Responses,
                )),
            );
            return Ok(Some(response));
        }
        Some("response.failed" | "error") => {
            let message = event
                .pointer("/response/error/message")
                .or_else(|| event.pointer("/error/message"))
                .or_else(|| event.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("response failed");
            return Err(ModelError::RequestFailed(format!(
                "stream error: {message}"
            )));
        }
        _ => {}
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn recording_sink() -> (StreamSink, Arc<Mutex<Vec<ModelStreamEvent>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let inner = Arc::clone(&seen);
        let sink = StreamSink::new(move |event| {
            if let Ok(mut guard) = inner.lock() {
                guard.push(event);
            }
        });
        (sink, seen)
    }

    fn decode_all(raw: &str) -> Result<Vec<SseFrame>, ModelError> {
        let mut decoder = SseDecoder::default();
        let mut frames = Vec::new();
        // In kleine Stücke zerschneiden, um Chunk-Grenzen zu testen.
        for chunk in raw.as_bytes().chunks(7) {
            frames.extend(decoder.push(chunk)?);
        }
        frames.extend(decoder.push(b"\n\n")?);
        Ok(frames)
    }

    #[test]
    fn stream_policy_defaults_on_and_honours_overrides() {
        let mut policy = StreamPolicy::default();
        assert!(policy.enabled("any"));
        policy.overrides.insert("slow".into(), false);
        assert!(!policy.enabled("slow"));
        policy.default = false;
        assert!(!policy.enabled("any"));
    }

    #[test]
    fn decoder_joins_multiline_data_and_ignores_comments() -> TestResult {
        let frames = decode_all(": ping\nevent: a\ndata: x\ndata: y\n\ndata: z\r\n\r\n")?;
        assert_eq!(
            frames,
            vec![
                SseFrame {
                    event: Some("a".into()),
                    data: "x\ny".into()
                },
                SseFrame {
                    event: None,
                    data: "z".into()
                }
            ]
        );
        Ok(())
    }

    const ANTHROPIC_STREAM: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":90,\"cache_creation_input_tokens\":5,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hal\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"fs_read\",\"input\":{}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"a.rs\\\"}\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":42}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    #[test]
    fn anthropic_stream_rebuilds_non_streamed_body() -> TestResult {
        let (sink, seen) = recording_sink();
        let mut acc = AnthropicStreamAccumulator::default();
        let mut stopped = false;
        for frame in decode_all(ANTHROPIC_STREAM)? {
            stopped |= acc.push(&frame, Some(&sink))?;
        }
        assert!(stopped);
        let body = acc.finish()?;
        assert_eq!(
            crate::anthropic::extract_anthropic_text(&body).as_deref(),
            Some("Hallo")
        );
        let calls =
            crate::anthropic::extract_anthropic_tool_calls(&body).map_err(|_| "tool calls")?;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "a.rs"}));
        let usage = crate::anthropic::extract_anthropic_usage(&body);
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 42);
        assert_eq!(usage.cached_tokens, Some(90));
        assert_eq!(usage.cache_write_tokens, Some(5));
        assert_eq!(body["stop_reason"], "tool_use");

        let events = seen.lock().map(|g| g.clone()).unwrap_or_default();
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                ModelStreamEvent::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hallo");
        assert!(events.iter().any(|e| matches!(
            e,
            ModelStreamEvent::Usage(u) if u.output_tokens == 42
        )));
        Ok(())
    }

    #[test]
    fn anthropic_stream_error_event_maps_overloaded_to_transient() -> TestResult {
        let mut acc = AnthropicStreamAccumulator::default();
        let frames = decode_all(
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"busy\"}}\n\n",
        )?;
        let result = acc.push(&frames[0], None);
        assert!(matches!(result, Err(ModelError::Transient { .. })));
        Ok(())
    }

    #[test]
    fn chat_stream_rebuilds_completion_with_tool_calls_and_usage() -> TestResult {
        let raw = concat!(
            "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hi\"}}]}\n\n",
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"fs_read\",\"arguments\":\"{\\\"pa\"}}]}}]}\n\n",
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":1}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"id\":\"c1\",\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3,\"total_tokens\":10}}\n\n",
            "data: [DONE]\n\n",
        );
        let (sink, seen) = recording_sink();
        let mut acc = ChatStreamAccumulator::default();
        let mut stopped = false;
        for frame in decode_all(raw)? {
            stopped |= acc.push(&frame, Some(&sink))?;
        }
        assert!(stopped);
        let body = acc.finish()?;
        let message = &body["choices"][0]["message"];
        assert_eq!(message["content"], "Hi");
        assert_eq!(message["tool_calls"][0]["id"], "call_1");
        assert_eq!(message["tool_calls"][0]["function"]["name"], "fs_read");
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            "{\"path\":1}"
        );
        assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(body["usage"]["prompt_tokens"], 7);
        let events = seen.lock().map(|g| g.clone()).unwrap_or_default();
        assert!(events.contains(&ModelStreamEvent::TextDelta("Hi".into())));
        Ok(())
    }

    /// Runde 7, Teil L7: `<think>` und Tool-Call-Syntax erscheinen nicht im
    /// Live-Stream, auch wenn die Marker über Chunk-Grenzen zerschnitten sind;
    /// der finale Body behält den Rohtext.
    #[test]
    fn chat_stream_filters_think_and_tool_call_markers_live() -> TestResult {
        let deltas = [
            "<thi",
            "nk>überlege",
            " kurz</th",
            "ink>Hallo ",
            "Welt<to",
            "ol_call>",
            "fs.read{\"path\":\"a\"}",
        ];
        let mut raw = String::from(
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n",
        );
        for delta in deltas {
            let chunk = serde_json::json!({
                "id": "c1",
                "choices": [{"index": 0, "delta": {"content": delta}}]
            });
            raw.push_str(&format!("data: {chunk}\n\n"));
        }
        raw.push_str("data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
        raw.push_str("data: [DONE]\n\n");
        let (sink, seen) = recording_sink();
        let mut acc = ChatStreamAccumulator::default();
        for frame in decode_all(&raw)? {
            acc.push(&frame, Some(&sink))?;
        }
        let body = acc.finish()?;
        let events = seen.lock().map(|g| g.clone()).unwrap_or_default();
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                ModelStreamEvent::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        let reasoning: String = events
            .iter()
            .filter_map(|e| match e {
                ModelStreamEvent::ReasoningDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hallo Welt");
        assert_eq!(reasoning, "überlege kurz");
        assert!(!text.contains("<"));
        let content = body["choices"][0]["message"]["content"]
            .as_str()
            .ok_or("content")?;
        assert!(content.contains("<tool_call>fs.read"));
        Ok(())
    }

    #[test]
    fn live_filter_releases_held_back_text_and_drops_orphan_close() {
        let mut filter = LiveTextFilter::default();
        assert_eq!(filter.push("a < b"), vec![LiveOut::Text("a < b".into())]);
        assert_eq!(filter.push(" und <"), vec![LiveOut::Text(" und ".into())]);
        assert_eq!(filter.flush(), vec![LiveOut::Text("<".into())]);
        let mut filter = LiveTextFilter::default();
        assert_eq!(
            filter.push("denken</think>Antwort"),
            vec![
                LiveOut::Text("denken".into()),
                LiveOut::Text("Antwort".into())
            ]
        );
    }

    #[test]
    fn responses_event_forwards_deltas_and_returns_terminal_body() -> TestResult {
        let (sink, seen) = recording_sink();
        let delta = serde_json::json!({"type":"response.output_text.delta","delta":"x"});
        assert!(responses_event(&delta, Some(&sink))?.is_none());
        let done = serde_json::json!({
            "type":"response.completed",
            "response":{"id":"r","output":[],"usage":{"input_tokens":3,"output_tokens":4}}
        });
        let body = responses_event(&done, Some(&sink))?.ok_or("terminal")?;
        assert_eq!(body["id"], "r");
        let events = seen.lock().map(|g| g.clone()).unwrap_or_default();
        assert_eq!(events[0], ModelStreamEvent::TextDelta("x".into()));
        Ok(())
    }
}
