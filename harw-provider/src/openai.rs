//! OpenAI-kompatibles Wire-Schema (Note 08 §2, §6).
//!
//! Ein Endpoint (`/responses`), ein Header (`Authorization: Bearer …`), ein
//! Body-Schema, ein Stream-Format. Codex hat Chat-Completions entfernt — wir
//! beherrschen nur noch die Responses-API.
//!
//! Reine `serde`-Daten: kein `reqwest`, kein `schemars` (Compile-Zeit-Typsicherheit
//! über die SSE-Verarbeitung, ohne zusätzliche Dependencies).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ===== Endpoint-Konstanten (Note 08 §1.6) =====

/// Voller URL = `{base_url}{RESPONSES_ENDPOINT}`.
pub const RESPONSES_ENDPOINT: &str = "/responses";
pub const RESPONSES_COMPACT_ENDPOINT: &str = "/responses/compact";
pub const MEMORIES_SUMMARIZE_ENDPOINT: &str = "/memories/trace_summarize";
/// Default-Base-URL eines OpenAI-Providers (Note 08 §1.5).
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Wire-API-Form (Note 08 §1.7). Codex hat alles außer `Responses` entfernt;
/// wir lassen die Variante erweiterbar für spätere Anthropic-Waves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireApi {
    #[default]
    Responses,
}

// ===== Request-Schema (Note 08 §6) =====

/// Body eines `POST {base_url}/responses`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResponsesRequest {
    pub model: String,
    pub input: Vec<InputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    pub stream: bool,
    pub store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<TextFormatConfig>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
    pub parallel_tool_calls: bool,
}

impl ResponsesRequest {
    /// Minimaler Konstruktor: nur Modell + Input, sinnvolle Defaults sonst.
    #[must_use]
    pub fn new(model: impl Into<String>, input: Vec<InputItem>) -> Self {
        Self {
            model: model.into(),
            input,
            instructions: None,
            tools: Vec::new(),
            tool_choice: None,
            stream: true,
            store: false,
            reasoning: None,
            text: None,
            metadata: HashMap::new(),
            parallel_tool_calls: true,
        }
    }
}

/// Ein Input-Item (Note 08 §2): Message, Tool-Call oder Tool-Output.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputItem {
    Message {
        role: String,
        content: Vec<ContentPart>,
    },
    /// Ein vom Modell in einem früheren Turn angeforderter Tool-Call, zur
    /// Verlaufs-Replay im (`store: false`) stateless Responses-Modus.
    /// `arguments` ist der JSON-serialisierte Argument-String (Wire-Format
    /// der Responses-API — nicht das geparste `serde_json::Value`).
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
}

/// Inhalts-Teil einer Nachricht.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    InputText {
        text: String,
    },
    OutputText {
        text: String,
    },
    InputImage {
        /// A `data:` URL: the harness only sends bytes it holds itself.
        image_url: String,
        /// `low`, `high` or `auto`; omitted when the sender has no preference.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

/// Tool-Definition (Function-Calling).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: serde_json::Value,
    #[serde(default)]
    pub strict: bool,
}

impl ToolDef {
    #[must_use]
    pub fn function(
        name: impl Into<String>,
        description: Option<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            kind: "function".to_owned(),
            name: name.into(),
            description,
            parameters,
            strict: true,
        }
    }
}

/// Tool-Choice-Steuerung.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Function { name: String },
}

/// Reasoning-Konfiguration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReasoningConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Text-Format-Konfiguration (z.B. JSON-Schema-Output).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextFormatConfig {
    pub format: serde_json::Value,
}

// ===== Stream-Schema (Note 08 §2, §6) =====

/// SSE-Event der Responses-API. Tagged über das `type`-Feld; jeder unbekannte
/// Frame ist beim Deserialisieren ein harter `match`-Arm statt stiller Verlust.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SseEvent {
    #[serde(rename = "response.created")]
    Created { response: ResponseHeader },
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded { item: OutputItem },
    #[serde(rename = "response.content_part.added")]
    ContentPartAdded { delta: Option<String> },
    #[serde(rename = "response.content_part.delta")]
    ContentPartDelta { delta: String },
    #[serde(rename = "response.content_part.done")]
    ContentPartDone {},
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgumentsDelta { delta: String },
    #[serde(rename = "response.function_call_arguments.done")]
    FunctionCallArgumentsDone {},
    #[serde(rename = "response.output_item.done")]
    OutputItemDone { item: OutputItem },
    #[serde(rename = "response.completed")]
    Completed { response: ResponseFooter },
    #[serde(rename = "error")]
    Error(ErrorPayload),
}

/// Terminaler SSE-Sentinel.
pub const SSE_DONE_SENTINEL: &str = "[DONE]";

/// Kopf des Streams (`response.created`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResponseHeader {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Fuß des Streams (`response.completed`) mit Usage.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResponseFooter {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// Token-Usage des Aufrufs.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

/// Ein Output-Item im Stream (Message, Function-Call, Reasoning).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputItem {
    Message {
        #[serde(default)]
        role: Option<String>,
        #[serde(default)]
        content: Vec<ContentPart>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        #[serde(default)]
        arguments: String,
    },
    Reasoning {
        #[serde(default)]
        summary: Option<String>,
    },
}

/// Fehlerframe des Streams.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
}
