//! Nativer Anthropic-Messages-Transport (Claude-nativ) — alternativer Weg zum
//! OpenAI-kompatiblen Pfad.
//!
//! ## Verantwortung
//! Diese Datei besitzt die konkrete, netzwerkgestützte Umsetzung des
//! Anthropic-`/v1/messages`-Schemas für Claude-Modelle. Derselbe Transport
//! bedient **drei Ziele**, die sich nur in Endpoint-URL und Auth-Header
//! unterscheiden:
//! 1. Anthropic-direkt mit API-Key (`x-api-key`).
//! 2. Anthropic-direkt mit OAuth-/Setup-Token (`Authorization: Bearer` +
//!    `anthropic-beta: oauth-2025-04-20`).
//! 3. Azure-AI-Foundry-Gateway zu Claude mit `x-api-key`.
//!
//! Es ist der **native** Weg (echtes Messages-Wire-Schema), nicht der
//! OpenAI-Chat-Shim.
//!
//! ## Nebenläufigkeit
//! [`AnthropicMessagesProvider`] ist `Send + Sync` (`reqwest::Client` teilt
//! sich intern und klont günstig). `respond` liefert ein `Box::pin`-Future.
//!
//! ## Sicherheit
//! Das Credential liegt in `secrecy::SecretString` und wird ausschließlich beim
//! Setzen des Auth-Headers via `ExposeSecret` offengelegt — niemals geloggt.

use harw_core::model::StopReason;
use harw_core::{
    ModelError, ModelFuture, ModelProvider, ModelRequest, ModelResponse, ToolCallResult,
};
use harw_protocol::OpaqueReasoning;
use harw_tools::{ToolCall, ToolName, ToolSpec};
use harw_types::{TokenUsage, ToolCallId};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use std::fmt;
use std::time::Duration;

const INVALID_CUSTOM_HEADERS_ERROR: &str = "invalid ANTHROPIC_CUSTOM_HEADERS configuration";

/// Pflicht-Version des Anthropic-Wire-Protokolls.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Beta-Header, der für OAuth-/Setup-Token-Auth gegen `/v1/messages` nötig ist.
pub const ANTHROPIC_OAUTH_BETA: &str = "oauth-2025-04-20";
/// Standard-Basis eines direkten Anthropic-Providers.
pub const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
/// Offizieller Host der Anthropic-API (Host von [`DEFAULT_ANTHROPIC_BASE_URL`]).
/// Nur an diesen Host gehen implizite Umgebungs-Credentials
/// (`CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`).
pub(crate) const ANTHROPIC_API_HOST: &str = "api.anthropic.com";
/// Default-Ausgabe-Token-Obergrenze, falls die Anfrage keine vorgibt.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Art des Anthropic-Credentials und damit des Auth-Header-Schemas.
///
/// # Description
/// - [`AnthropicCredential::ApiKey`] → Header `x-api-key` (Anthropic-API-Key
///   **oder** Foundry-Deployment-Key).
/// - [`AnthropicCredential::OAuth`] → Header `Authorization: Bearer` **plus**
///   `anthropic-beta: oauth-2025-04-20` (Setup-Token / OAuth).
#[derive(Clone)]
pub enum AnthropicCredential {
    /// Statischer API-Key (Anthropic oder Foundry). Header `x-api-key`.
    ApiKey(SecretString),
    /// OAuth-/Setup-Token. Header `Authorization: Bearer` + oauth-beta.
    OAuth(SecretString),
    /// Microsoft Entra bearer token, without Anthropic OAuth beta headers.
    Bearer(SecretString),
}

/// HTTP-Provider gegen die native Anthropic-Messages-API.
///
/// # Description
/// Hält den geteilten `reqwest::Client`, die **vollständige** Endpoint-URL
/// (`…/v1/messages`), den Modellnamen und das geheime Credential.
/// Implementiert [`harw_core::ModelProvider`].
///
/// # Concurrency
/// `Send + Sync`; hinter einem `Arc` von mehreren Threads nutzbar.
pub struct AnthropicMessagesProvider {
    client: reqwest::Client,
    /// Vollständige Endpoint-URL, z. B. `https://api.anthropic.com/v1/messages`
    /// oder `https://<res>.services.ai.azure.com/anthropic/v1/messages`.
    messages_url: String,
    provider_id: String,
    model: String,
    credential: AnthropicCredential,
    max_tokens: u32,
    request_timeout: Duration,
    configured_headers: Option<reqwest::header::HeaderMap>,
}

impl AnthropicMessagesProvider {
    /// Baut einen Provider aus einer bereits zusammengesetzten Endpoint-URL.
    ///
    /// # Arguments
    /// - `messages_url` (`impl Into<String>`): vollständige `/v1/messages`-URL.
    /// - `model` (`impl Into<String>`): Modell-/Deployment-Name für `model`.
    /// - `credential` ([`AnthropicCredential`]): bestimmt das Auth-Header-Schema.
    ///
    /// # Returns
    /// Einen einsatzbereiten [`AnthropicMessagesProvider`] mit
    /// [`DEFAULT_MAX_TOKENS`].
    #[must_use]
    pub fn new(
        messages_url: impl Into<String>,
        model: impl Into<String>,
        credential: AnthropicCredential,
    ) -> Self {
        Self {
            client: super::http_client(),
            messages_url: messages_url.into(),
            provider_id: "anthropic".to_owned(),
            model: model.into(),
            credential,
            max_tokens: DEFAULT_MAX_TOKENS,
            request_timeout: super::DEFAULT_REQUEST_TIMEOUT,
            configured_headers: None,
        }
    }

    /// Baut einen Provider aus Basis-URL + Modell + Credential.
    ///
    /// # Description
    /// Setzt die Endpoint-URL über [`anthropic_messages_url`] zusammen (inkl.
    /// Trailing-Slash- und doppel-`/v1`-Normalisierung).
    #[must_use]
    pub fn from_base(
        base_url: &str,
        model: impl Into<String>,
        credential: AnthropicCredential,
    ) -> Self {
        Self::new(anthropic_messages_url(base_url), model, credential)
    }

    pub(crate) fn configure(&mut self, id: &str, headers: reqwest::header::HeaderMap) {
        self.provider_id = id.to_owned();
        self.configured_headers = Some(headers);
    }

    /// Resolves the model for one request after checking its provider affinity.
    ///
    /// A request without a provider ID (or with an empty one) remains compatible
    /// with the configured provider. A non-empty provider ID must match exactly;
    /// otherwise this provider must not route the request. Likewise, an absent or
    /// empty model ID retains the configured model default.
    fn selected_model<'a>(&'a self, request: &'a ModelRequest) -> Result<&'a str, ModelError> {
        if let Some(provider_id) = request.provider_id.as_ref()
            && !provider_id.as_str().is_empty()
            && provider_id.as_str() != self.provider_id.as_str()
        {
            return Err(ModelError::RequestFailed(format!(
                "request targets provider '{}' but this HTTP provider is configured for '{}'",
                provider_id, self.provider_id
            )));
        }

        Ok(request
            .model_id
            .as_ref()
            .filter(|model_id| !model_id.as_str().is_empty())
            .map_or(self.model.as_str(), |model_id| model_id.as_str()))
    }
}

/// Setzt aus einer Basis-URL die vollständige `/v1/messages`-Endpoint-URL
/// zusammen.
///
/// # Description
/// Normalisiert die Eingabe robust gegen die zwei realen Formen:
/// - Foundry: `https://<res>.services.ai.azure.com/anthropic/` (Trailing-Slash)
/// - Anthropic-Katalog: `https://api.anthropic.com/v1` (bereits `/v1`)
///
/// Trailing-`/` werden entfernt; ein bereits vorhandenes `/v1`-Suffix wird
/// abgeschnitten, bevor `/v1/messages` angehängt wird — so entsteht nie
/// `//v1/messages` oder `/v1/v1/messages`.
///
/// # Examples
/// ```
/// use harw_provider_http::anthropic_messages_url;
/// assert_eq!(
///     anthropic_messages_url("https://x.services.ai.azure.com/anthropic/"),
///     "https://x.services.ai.azure.com/anthropic/v1/messages"
/// );
/// assert_eq!(
///     anthropic_messages_url("https://api.anthropic.com/v1"),
///     "https://api.anthropic.com/v1/messages"
/// );
/// ```
#[must_use]
pub fn anthropic_messages_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let root = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    let root = root.trim_end_matches('/');
    format!("{root}/v1/messages")
}

/// Parses the optional custom-header environment variable without accepting
/// malformed or control-character-containing header data.
fn parse_anthropic_custom_headers(
    raw: &str,
) -> Result<Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)>, ModelError> {
    let mut headers = Vec::new();
    for header_spec in raw.split(',') {
        let header_spec = header_spec.trim();
        let Some((name, value)) = header_spec.split_once(':') else {
            return Err(ModelError::RequestFailed(
                INVALID_CUSTOM_HEADERS_ERROR.to_owned(),
            ));
        };
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.contains('\r') || value.contains('\n') {
            return Err(ModelError::RequestFailed(
                INVALID_CUSTOM_HEADERS_ERROR.to_owned(),
            ));
        }
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| ModelError::RequestFailed(INVALID_CUSTOM_HEADERS_ERROR.to_owned()))?;
        let value = reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| ModelError::RequestFailed(INVALID_CUSTOM_HEADERS_ERROR.to_owned()))?;
        headers.push((name, value));
    }
    Ok(headers)
}

fn anthropic_custom_headers()
-> Result<Option<Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)>>, ModelError> {
    match std::env::var("ANTHROPIC_CUSTOM_HEADERS") {
        Ok(raw) if raw.trim().is_empty() => Ok(None),
        Ok(raw) => parse_anthropic_custom_headers(&raw).map(Some),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(ModelError::RequestFailed(
            INVALID_CUSTOM_HEADERS_ERROR.to_owned(),
        )),
    }
}

/// Übersetzt die dem Modell angebotenen [`ToolSpec`]s in das Anthropic-
/// Wire-Format des `tools`-Arrays: `{name, description, input_schema}`.
///
/// # Description
/// Aktuell existiert nur `ToolSpec::Function`; die `input_schema` ist die
/// JSON-Schema-Repräsentation der Tool-Parameter ([`harw_tools::JsonSchema`]
/// via `serde_json::to_value`). Ein Serialisierungsfehler ist praktisch
/// ausgeschlossen (reine Daten-Struktur ohne fehlbare Typen) — im
/// unwahrscheinlichen Fehlerfall wird ein leeres Objekt-Schema verwendet,
/// statt zu paniken.
fn build_anthropic_tools(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|spec| match spec {
            ToolSpec::Function(f) => {
                let input_schema =
                    serde_json::to_value(&f.parameters).unwrap_or_else(|_| serde_json::json!({}));
                serde_json::json!({
                    "name": f.name.as_str(),
                    "description": f.description,
                    "input_schema": input_schema,
                })
            }
        })
        .collect()
}

/// Hängt einen Content-Block an die letzte Nachricht derselben Rolle an oder
/// beginnt eine neue Nachricht.
///
/// # Description
/// Die Messages-API verlangt abwechselnde Rollen: mehrere `tool_use`-Blöcke
/// eines Modell-Turns gehören in **eine** Assistant-Nachricht, die zugehörigen
/// `tool_result`-Blöcke in **eine** darauffolgende User-Nachricht. Im Verlauf
/// stehen sie dagegen als einzelne Einträge (erst alle Calls, dann alle
/// Ergebnisse), weil der Turn-Loop parallele Calls so protokolliert. Diese
/// Funktion führt beides wieder zusammen. Nachrichten mit Text-Content
/// (`content` als String) werden nie erweitert, sodass ein Textbeitrag eine
/// Gruppe zuverlässig beendet.
///
/// # Arguments
/// - `messages` (`&mut Vec<Value>`): der bisher aufgebaute Nachrichtenverlauf.
/// - `role` (`&str`): `"assistant"` für `tool_use`, `"user"` für `tool_result`.
/// - `block` (`Value`): der anzuhängende Content-Block.
fn push_content_block(messages: &mut Vec<Value>, role: &str, block: Value) {
    let open_group = messages
        .last_mut()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some(role))
        .and_then(|message| message.get_mut("content"))
        .and_then(Value::as_array_mut);
    match open_group {
        Some(content) => content.push(block),
        None => messages.push(serde_json::json!({ "role": role, "content": [block] })),
    }
}

/// Baut den Anthropic-Messages-Request-Body aus einem [`ModelRequest`].
///
/// # Description
/// System-Prompt + Instruction-Fragmente werden zum `system`-Feld
/// zusammengefasst; der Verlauf wird auf `messages[]` projiziert:
/// - `User` → `user` mit reinem Text-Content.
/// - `Assistant` → `assistant` mit reinem Text-Content.
/// - `ToolCall` → `assistant`-Message mit einem `tool_use`-Content-Block
///   (`{"type":"tool_use","id":<call_id>,"name":<tool_name>,"input":<args>}`).
/// - `ToolResult` → `user`-Message mit einem `tool_result`-Content-Block
///   (`{"type":"tool_result","tool_use_id":<call_id>,"content":<text>}`),
///   plus `"is_error":true` bei `Err`.
///
/// `request.tools` wird — sofern nicht leer — via [`build_anthropic_tools`]
/// auf `body["tools"]` abgebildet; bei leerem Tool-Set bleibt das Feld unset
/// (bestehendes Verhalten für Requests ohne Tools). Rein und I/O-frei, daher
/// direkt testbar.
///
/// Ist `request.reasoning_effort` `Some(effort)`, schlägt die Funktion das
/// Modell in der Capability-Tabelle [`crate::anthropic_caps`] nach
/// ([`anthropic_caps::lookup`]):
/// - Akzeptiert das Modell `thinking: {"type": "adaptive"}`
///   ([`anthropic_caps::ThinkingSupport::Adaptive`]), wird dieses Top-Level-
///   Feld gesetzt (aktuelles Anthropic-Wire-Schema — NICHT das veraltete
///   `{"type": "enabled", "budget_tokens": N}`-Schema, das der Harness mangels
///   ableitbarem Token-Budget nicht sendet).
/// - Liefert [`anthropic_caps::effort_wire_value`] für dieses Modell einen
///   Wert, wird zusätzlich `output_config: {"effort": <Wert>}` gesetzt — das
///   ist unabhängig vom `thinking`-Feld möglich (manche Modelle akzeptieren
///   `effort` ohne `adaptive`-Thinking).
/// - Unbekannte Modelle (`lookup` liefert `None`) und Modelle ohne die
///   jeweilige Fähigkeit bekommen das betreffende Feld gar nicht (fail
///   closed — ein falsches `thinking`- oder `effort`-Feld erzeugt HTTP 400).
///
/// Ist `request.reasoning_effort` `None`, bleiben beide Felder unset.
///
/// `max_tokens` wird über [`anthropic_caps::clamp_max_tokens`] auf das
/// Ausgabe-Limit des Modells geklemmt, bevor es auf `body["max_tokens"]`
/// landet; für unbekannte Modelle bleibt der angeforderte Wert unverändert.
///
/// # Arguments
/// - `model` (`&str`): Modell-/Deployment-Name für das `model`-Feld.
/// - `max_tokens` (`u32`): gewünschte Ausgabe-Token-Obergrenze (vor Clamping).
/// - `request` (`&ModelRequest`): Quelle für System-Prompt, Fragmente,
///   Verlauf, Tools und optionales `reasoning_effort`.
///
/// # Returns
/// Ein [`serde_json::Value`]-Objekt, direkt als Request-Body serialisierbar.
#[must_use]
pub fn build_messages_body(model: &str, max_tokens: u32, request: &ModelRequest) -> Value {
    let mut system = request.system_prompt.clone();
    for fragment in &request.instruction_fragments {
        system.push('\n');
        system.push_str(fragment);
    }

    let mut messages: Vec<Value> = Vec::new();
    for message in request.history.to_model_messages() {
        match message {
            harw_core::ModelMessage::User { text } => {
                messages.push(serde_json::json!({ "role": "user", "content": text }));
            }
            harw_core::ModelMessage::Assistant { text } => {
                messages.push(serde_json::json!({ "role": "assistant", "content": text }));
            }
            harw_core::ModelMessage::ToolCall {
                call_id,
                name,
                arguments,
            } => {
                push_content_block(
                    &mut messages,
                    "assistant",
                    serde_json::json!({
                        "type": "tool_use",
                        "id": call_id.as_str(),
                        "name": name,
                        "input": arguments,
                    }),
                );
            }
            harw_core::ModelMessage::ToolResult { call_id, result } => {
                let (content, is_error) = match result {
                    ToolCallResult::Success { value } => (value.to_string(), false),
                    ToolCallResult::Error { message } => (message, true),
                };
                let mut block = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": call_id.as_str(),
                    "content": content,
                });
                if is_error {
                    block["is_error"] = Value::Bool(true);
                }
                push_content_block(&mut messages, "user", block);
            }
        }
    }

    let max_tokens = crate::anthropic_caps::clamp_max_tokens(model, max_tokens);
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": messages,
    });
    if !system.is_empty() {
        body["system"] = Value::String(system);
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(build_anthropic_tools(&request.tools));
    }
    if let Some(effort) = request.reasoning_effort
        && let Some(caps) = crate::anthropic_caps::lookup(model)
    {
        if caps.thinking == crate::anthropic_caps::ThinkingSupport::Adaptive {
            body["thinking"] = serde_json::json!({"type": "adaptive"});
        }
        // Minimal → thinking (falls gesetzt) bleibt aktiv, aber
        // output_config.effort wird weggelassen; das Modell wählt den
        // Denkaufwand selbst. effort_wire_value liefert dafür bereits `None`.
        if let Some(level) = crate::anthropic_caps::effort_wire_value(caps, effort) {
            body["output_config"] = serde_json::json!({"effort": level});
        }
        // Unbekannte Modelle (lookup == None) bekommen weder thinking noch
        // output_config — fail closed, siehe Modul-Doku von anthropic_caps.
    }
    body
}

/// Extrahiert den Assistant-Text aus einer nicht-gestreamten Messages-Antwort.
///
/// # Description
/// Sammelt alle `content[]`-Blöcke vom Typ `text` und verbindet ihren `text`.
///
/// # Returns
/// `Some(text)` oder `None`, wenn kein nicht-leerer Text vorliegt.
#[must_use]
pub fn extract_anthropic_text(body: &Value) -> Option<String> {
    let content = body.get("content")?.as_array()?;
    let mut text = String::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(chunk) = block.get("text").and_then(Value::as_str) {
                text.push_str(chunk);
            }
        }
    }
    if text.is_empty() { None } else { Some(text) }
}

/// Extrahiert die vom Modell angeforderten Tool-Calls aus einer
/// nicht-gestreamten Messages-Antwort.
///
/// # Description
/// Durchsucht `content[]` nach Blöcken vom Typ `tool_use` und mappt
/// `id → call_id`, `name → name`, `input → arguments`. Blöcke mit fehlendem
/// `id` oder `name` müssen als Strings vorhanden sein; unvollständige
/// `tool_use`-Blöcke werden fail-closed abgelehnt.
///
/// # Arguments
/// - `body` (`&Value`): der bereits geparste JSON-Response-Body.
///
/// # Returns
/// Einen `Vec<ToolCall>`, leer wenn kein `content`-Array oder keine
/// `tool_use`-Blöcke vorhanden sind. Eine fehlerhafte `tool_use`-Antwort mit
/// fehlendem oder nicht-stringförmigem `id`/`name` oder fehlendem bzw.
/// nicht-objektförmigem `input` wird als ungültige Provider-Antwort
/// fail-closed abgelehnt.
/// Fehler beim fail-closed Parsen eines Anthropic-`tool_use`-Blocks.
#[derive(Debug, PartialEq, Eq)]
pub enum AnthropicToolCallParseError {
    MissingId,
    MissingName,
    InvalidInput,
}

impl fmt::Display for AnthropicToolCallParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingId => "Anthropic tool-use id must be a string",
            Self::MissingName => "Anthropic tool-use name must be a string",
            Self::InvalidInput => "Anthropic tool-use input must be an object",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for AnthropicToolCallParseError {}

pub fn extract_anthropic_tool_calls(
    body: &Value,
) -> Result<Vec<ToolCall>, AnthropicToolCallParseError> {
    let Some(content) = body.get("content").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut calls = Vec::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }

        let Some(arguments) = block
            .get("input")
            .filter(|value| value.is_object())
            .cloned()
        else {
            return Err(AnthropicToolCallParseError::InvalidInput);
        };
        let Some(id) = block.get("id").and_then(Value::as_str) else {
            return Err(AnthropicToolCallParseError::MissingId);
        };
        let Some(name) = block.get("name").and_then(Value::as_str) else {
            return Err(AnthropicToolCallParseError::MissingName);
        };
        calls.push(ToolCall {
            id: ToolCallId::from_str(id.to_owned()),
            name: ToolName::new(name.to_owned()),
            arguments,
        });
    }
    Ok(calls)
}

/// Extrahiert die Token-Nutzung aus einer nicht-gestreamten Messages-Antwort.
///
/// # Description
/// Liest das Top-Level-`usage`-Objekt der Anthropic-Messages-Antwort:
/// `input_tokens` und `output_tokens` direkt, `cached_tokens` aus
/// `cache_read_input_tokens`. Anthropic liefert keine separaten
/// Reasoning-Token-Zähler — Extended-Thinking-Tokens sind bereits in
/// `output_tokens` enthalten, daher bleibt `reasoning_tokens` immer `None`.
///
/// Ein fehlendes `usage`-Objekt oder fehlende Einzelfelder führen zu `0`
/// (bzw. `None` für `cached_tokens`), nie zu einem Panic — passend zum
/// restlichen Parsing-Stil dieser Datei (`Option`-Kombinatoren statt
/// `unwrap`/`expect`).
///
/// # Arguments
/// - `body` (`&Value`): der bereits geparste JSON-Response-Body.
///
/// # Returns
/// Ein [`TokenUsage`] mit den extrahierten (oder default-mäßig genullten)
/// Werten. `cached_tokens` ist `None` genau dann, wenn
/// `cache_read_input_tokens` im Body fehlt — ein explizit übermittelter
/// Wert von `0` ist ein gültiger, gemessener Wert und bleibt `Some(0)`.
#[must_use]
pub fn extract_anthropic_usage(body: &Value) -> TokenUsage {
    let input_tokens = body
        .get("usage")
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = body
        .get("usage")
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached_tokens = body
        .get("usage")
        .and_then(|usage| usage.get("cache_read_input_tokens"))
        .and_then(Value::as_u64);

    TokenUsage {
        input_tokens,
        output_tokens,
        reasoning_tokens: None,
        cached_tokens,
    }
}

/// Übersetzt Anthropics Top-Level-`stop_reason` in [`StopReason`].
///
/// # Description
/// Liest `body.stop_reason` (String) einer nicht-gestreamten
/// Messages-Antwort und mappt sie auf die provider-neutrale Variante.
/// Ein fehlender oder unbekannter Wert wird nicht verworfen: bekannte
/// Anthropic-Werte werden 1:1 gemappt, ein unbekannter String landet in
/// [`StopReason::Other`], und ein komplett fehlendes Feld fällt auf
/// [`StopReason::EndTurn`] zurück (Default-Verhalten bei regulärer Antwort).
///
/// # Arguments
/// - `body` (`&Value`): der bereits geparste JSON-Response-Body.
///
/// # Returns
/// Die gemappte [`StopReason`].
#[must_use]
pub fn extract_anthropic_stop_reason(body: &Value) -> StopReason {
    match body.get("stop_reason").and_then(Value::as_str) {
        Some("end_turn") => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("stop_sequence") => StopReason::StopSequence,
        Some("pause_turn") => StopReason::PauseTurn,
        Some("refusal") => StopReason::Refusal { detail: None },
        Some(other) => StopReason::Other(other.to_owned()),
        None => StopReason::EndTurn,
    }
}

/// Extrahiert opake Denkblöcke (`thinking`/`redacted_thinking`) aus einer
/// nicht-gestreamten Messages-Antwort (G-015).
///
/// # Description
/// Sammelt alle `content[]`-Blöcke vom Typ `thinking` oder
/// `redacted_thinking` unverändert (als rohe `serde_json::Value`) in
/// Aufrufreihenfolge, damit sie im nächsten Tool-Loop-Turn byte-identisch
/// zurückgespielt werden können.
///
/// # Arguments
/// - `body` (`&Value`): der bereits geparste JSON-Response-Body.
/// - `model` (`&str`): die für den Request verwendete Modell-ID, zur
///   Provenance-Markierung im zurückgegebenen [`OpaqueReasoning`].
///
/// # Returns
/// `Some(OpaqueReasoning)` mit `provider: "anthropic"`, wenn mindestens ein
/// Denkblock vorhanden ist; sonst `None` (kein Extended Thinking in der
/// Antwort).
#[must_use]
pub fn extract_anthropic_reasoning(body: &Value, model: &str) -> Option<OpaqueReasoning> {
    let content = body.get("content")?.as_array()?;
    let blocks: Vec<Value> = content
        .iter()
        .filter(|block| {
            matches!(
                block.get("type").and_then(Value::as_str),
                Some("thinking") | Some("redacted_thinking")
            )
        })
        .cloned()
        .collect();
    if blocks.is_empty() {
        None
    } else {
        Some(OpaqueReasoning {
            provider: "anthropic".to_owned(),
            model: model.to_owned(),
            blocks,
        })
    }
}

/// Setzt die Auth-Header des Credentials; alle Credential-Werte sind als
/// sensitiv markiert (`x-api-key` und OAuth-`authorization` über
/// [`super::sensitive_header_value`], `bearer_auth` in reqwest selbst).
fn apply_anthropic_credential(
    builder: reqwest::RequestBuilder,
    credential: &AnthropicCredential,
) -> Result<reqwest::RequestBuilder, ModelError> {
    Ok(match credential {
        AnthropicCredential::ApiKey(secret) => builder.header(
            "x-api-key",
            super::sensitive_header_value(secret.expose_secret())?,
        ),
        AnthropicCredential::Bearer(secret) => builder.bearer_auth(secret.expose_secret()),
        AnthropicCredential::OAuth(secret) => builder
            .header(
                "authorization",
                super::sensitive_header_value(&format!("Bearer {}", secret.expose_secret()))?,
            )
            .header("anthropic-beta", ANTHROPIC_OAUTH_BETA),
    })
}

impl ModelProvider for AnthropicMessagesProvider {
    fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
        Box::pin(async move {
            let model = self.selected_model(&request)?;
            tracing::debug!(model, "sending anthropic messages request");
            let wire = build_messages_body(model, self.max_tokens, &request);

            let mut builder = self
                .client
                .post(&self.messages_url)
                .header("content-type", "application/json")
                .header("anthropic-version", ANTHROPIC_VERSION);

            // Optionaler Passthrough zusätzlicher Header (z. B. für Cloudflare AI
            // Gateway: `cf-aig-authorization: Bearer …`). Format aus der Env-Var
            // `ANTHROPIC_CUSTOM_HEADERS` — kompatibel mit Anthropic's offiziellem
            // Claude-Code-Client. Mehrere Header via `,` getrennt.
            let custom_headers = if let Some(headers) = &self.configured_headers {
                Some(headers.iter().map(|(name, value)| (name.clone(), value.clone())).collect::<Vec<_>>())
            } else {
                anthropic_custom_headers()?
            };

            builder = apply_anthropic_credential(builder, &self.credential)?;
            if let Some(custom_headers) = custom_headers {
                for (name, value) in custom_headers {
                    builder = builder.header(name, value);
                }
            }

            let response = builder
                .json(&wire)
                .timeout(self.request_timeout)
                .send()
                .await
                .map_err(|error| {
                    ModelError::RequestFailed(super::HttpProviderError::from(error).to_string())
                })?;

            let status = response.status();
            let retry_after_header = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let request_id = super::provider_request_id(response.headers());
            let body = response.text().await.map_err(|error| {
                ModelError::RequestFailed(super::HttpProviderError::from(error).to_string())
            })?;

            if status.as_u16() == 429 {
                let retry_after = super::parse_retry_after(retry_after_header.as_deref(), &body);
                return Err(ModelError::RateLimited {
                    retry_after_secs: retry_after.as_secs(),
                    message: super::sanitized_provider_error(
                        status.as_u16(),
                        request_id.as_deref(),
                        &body,
                    ),
                });
            }

            if !status.is_success() {
                return Err(ModelError::RequestFailed(super::sanitized_provider_error(
                    status.as_u16(),
                    request_id.as_deref(),
                    &body,
                )));
            }

            let value: Value = serde_json::from_str(&body)?;
            let text = extract_anthropic_text(&value);
            let tool_calls = extract_anthropic_tool_calls(&value).map_err(|_| {
                ModelError::RequestFailed(
                    "Anthropic response contained a tool-use block with invalid input".to_owned(),
                )
            })?;
            if text.is_none() && tool_calls.is_empty() {
                return Err(ModelError::EmptyResponse);
            }

            Ok(ModelResponse {
                message: text,
                tool_calls,
                usage: extract_anthropic_usage(&value),
                stop: extract_anthropic_stop_reason(&value),
                reasoning: extract_anthropic_reasoning(&value, model),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with_ids(model_id: Option<&str>, provider_id: Option<&str>) -> ModelRequest {
        ModelRequest {
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history: harw_core::ConversationHistory::new(),
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: model_id.map(Into::into),
            provider_id: provider_id.map(Into::into),
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
        }
    }

    fn test_provider() -> AnthropicMessagesProvider {
        AnthropicMessagesProvider::new(
            "https://example.test/v1/messages",
            "configured-model",
            AnthropicCredential::ApiKey(SecretString::new("sk-secret".into())),
        )
    }

    #[test]
    fn test_parse_anthropic_custom_headers_accepts_gateway_headers() {
        let headers = parse_anthropic_custom_headers(
            "cf-aig-authorization: Bearer gateway-token, cf-aig-metadata: tenant=test",
        )
        .expect("valid gateway headers");

        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].0.as_str(), "cf-aig-authorization");
        assert_eq!(headers[0].1.to_str().unwrap(), "Bearer gateway-token");
        assert_eq!(headers[1].0.as_str(), "cf-aig-metadata");
        assert_eq!(headers[1].1.to_str().unwrap(), "tenant=test");
    }

    #[test]
    fn test_parse_anthropic_custom_headers_rejects_malformed_entry_redacted() {
        let error = parse_anthropic_custom_headers("cf-aig-authorization").unwrap_err();

        assert!(matches!(
            error,
            ModelError::RequestFailed(message)
                if message == INVALID_CUSTOM_HEADERS_ERROR
        ));
    }

    #[test]
    fn test_parse_anthropic_custom_headers_rejects_crlf_injection_without_echoing_value() {
        let malicious_value = "Bearer gateway-token\r\nX-Injected: true";
        let error =
            parse_anthropic_custom_headers(&format!("cf-aig-authorization: {malicious_value}"))
                .unwrap_err();

        assert!(matches!(
            error,
            ModelError::RequestFailed(message)
                if message == INVALID_CUSTOM_HEADERS_ERROR
                    && !message.contains("gateway-token")
                    && !message.contains("X-Injected")
        ));
    }

    #[test]
    fn test_selected_model_uses_requested_model_for_compatible_provider() {
        let provider = test_provider();
        let request = request_with_ids(Some("requested-model"), Some("anthropic"));

        assert_eq!(
            provider.selected_model(&request).unwrap(),
            "requested-model"
        );
    }

    #[test]
    fn test_provider_uses_default_bounded_request_timeout() {
        let provider = test_provider();

        assert_eq!(
            provider.request_timeout,
            super::super::DEFAULT_REQUEST_TIMEOUT
        );
    }

    #[test]
    fn test_selected_model_preserves_configured_default_for_empty_identifiers() {
        let provider = test_provider();
        let request = request_with_ids(Some(""), Some(""));

        assert_eq!(
            provider.selected_model(&request).unwrap(),
            "configured-model"
        );
    }

    #[test]
    fn test_selected_model_rejects_provider_mismatch() {
        let provider = test_provider();
        let request = request_with_ids(Some("requested-model"), Some("openai"));

        assert!(matches!(
            provider.selected_model(&request),
            Err(ModelError::RequestFailed(message))
                if message.contains("openai") && message.contains("anthropic")
        ));
    }

    #[test]
    fn test_anthropic_messages_url_foundry_trailing_slash() {
        assert_eq!(
            anthropic_messages_url("https://x.services.ai.azure.com/anthropic/"),
            "https://x.services.ai.azure.com/anthropic/v1/messages"
        );
    }

    #[test]
    fn test_anthropic_messages_url_strips_existing_v1() {
        assert_eq!(
            anthropic_messages_url("https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn test_anthropic_messages_url_plain_root() {
        assert_eq!(
            anthropic_messages_url("https://api.anthropic.com"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn test_anthropic_messages_url_multiple_trailing_slashes() {
        assert_eq!(
            anthropic_messages_url("https://api.anthropic.com/v1///"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn test_build_messages_body_shape() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("Hi there");
        let request = ModelRequest {
            system_prompt: "You are terse.".to_owned(),
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
        };

        let body = build_messages_body("claude-sonnet-5", 1024, &request);
        assert_eq!(
            body.get("model").and_then(Value::as_str),
            Some("claude-sonnet-5")
        );
        assert_eq!(body.get("max_tokens").and_then(Value::as_u64), Some(1024));
        assert_eq!(
            body.get("system").and_then(Value::as_str),
            Some("You are terse.")
        );
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages array");
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("user")
        );
        assert_eq!(
            messages[0].get("content").and_then(Value::as_str),
            Some("Hi there")
        );
    }

    #[test]
    fn test_build_messages_body_omits_empty_system() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("Only user");
        let request = ModelRequest {
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
        };
        let body = build_messages_body("m", 256, &request);
        assert!(body.get("system").is_none());
    }

    #[test]
    fn test_build_messages_body_with_reasoning_effort_sets_thinking_and_output_config() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("Hi there");
        let request = ModelRequest {
            system_prompt: "You are terse.".to_owned(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: Some(harw_types::ReasoningEffort::High),
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
        };

        let body = build_messages_body("claude-opus-4-8", 1024, &request);
        assert_eq!(body["thinking"]["type"].as_str(), Some("adaptive"));
        assert_eq!(body["output_config"]["effort"].as_str(), Some("high"));
    }

    #[test]
    fn test_build_messages_body_omits_reasoning_for_unsupported_model() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("Hi there");
        let request = ModelRequest {
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: Some(harw_types::ReasoningEffort::High),
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
        };

        let body = build_messages_body("anthropic-deployment-alias", 1024, &request);

        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn test_build_messages_body_without_reasoning_effort_omits_thinking_and_output_config() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("Hi there");
        let request = ModelRequest {
            system_prompt: "You are terse.".to_owned(),
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
        };

        let body = build_messages_body("claude-opus-4-8", 1024, &request);
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    /// Baut eine [`ModelRequest`] mit gegebenem `reasoning_effort`, sonst
    /// minimalem Inhalt — Hilfsfunktion für die Capability-Wiring-Tests unten.
    fn request_with_effort(effort: Option<harw_types::ReasoningEffort>) -> ModelRequest {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("hi");
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

    #[test]
    fn test_build_messages_body_known_model_with_thinking_gets_field_and_clamped_max_tokens() {
        let request = request_with_effort(Some(harw_types::ReasoningEffort::High));

        // claude-opus-5 unterstützt adaptive thinking + effort und erlaubt
        // höchstens 128k Ausgabe-Tokens (siehe anthropic_caps::MODELS).
        let body = build_messages_body("claude-opus-5", 500_000, &request);

        assert_eq!(body["thinking"]["type"].as_str(), Some("adaptive"));
        assert_eq!(body["output_config"]["effort"].as_str(), Some("high"));
        assert_eq!(body.get("max_tokens").and_then(Value::as_u64), Some(128_000));
    }

    #[test]
    fn test_build_messages_body_model_without_thinking_support_omits_thinking_field() {
        let request = request_with_effort(Some(harw_types::ReasoningEffort::High));

        // claude-sonnet-4-5-20250929 kennt nur den Legacy-Modus (ExtendedOnly)
        // und akzeptiert `output_config.effort` gar nicht (caps.effort == false).
        let body = build_messages_body("claude-sonnet-4-5-20250929", 1024, &request);

        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn test_build_messages_body_extended_only_model_can_still_get_effort_without_thinking() {
        let request = request_with_effort(Some(harw_types::ReasoningEffort::High));

        // claude-opus-4-5-20251101 ist ExtendedOnly (kein adaptive-thinking),
        // akzeptiert laut Tabelle aber `output_config.effort` unabhängig davon.
        let body = build_messages_body("claude-opus-4-5-20251101", 1024, &request);

        assert!(body.get("thinking").is_none());
        assert_eq!(body["output_config"]["effort"].as_str(), Some("high"));
    }

    #[test]
    fn test_build_messages_body_unknown_model_uses_conservative_fallback() {
        let request = request_with_effort(Some(harw_types::ReasoningEffort::High));

        let body = build_messages_body("totally-unknown-deployment", 900_000, &request);

        // Fail closed: keine Reasoning-Felder für unbekannte Modelle.
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
        // Kein Modell-Limit bekannt → max_tokens bleibt unangetastet.
        assert_eq!(body.get("max_tokens").and_then(Value::as_u64), Some(900_000));
    }

    #[test]
    fn test_extract_anthropic_text_joins_blocks() {
        let body = serde_json::json!({
            "content": [
                { "type": "thinking", "text": "ignore" },
                { "type": "text", "text": "Hello, " },
                { "type": "text", "text": "world" }
            ]
        });
        assert_eq!(
            extract_anthropic_text(&body).as_deref(),
            Some("Hello, world")
        );
    }

    #[test]
    fn test_extract_anthropic_text_none_when_empty() {
        let body = serde_json::json!({ "content": [] });
        assert_eq!(extract_anthropic_text(&body), None);
    }

    #[test]
    fn test_extract_anthropic_usage_full_object() {
        let body = serde_json::json!({
            "usage": {
                "input_tokens": 123,
                "output_tokens": 456,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 78
            }
        });
        let usage = extract_anthropic_usage(&body);
        assert_eq!(usage.input_tokens, 123);
        assert_eq!(usage.output_tokens, 456);
        assert_eq!(usage.cached_tokens, Some(78));
        assert_eq!(usage.reasoning_tokens, None);
    }

    #[test]
    fn test_extract_anthropic_usage_missing_object_defaults() {
        let body = serde_json::json!({ "content": [] });
        assert_eq!(extract_anthropic_usage(&body), TokenUsage::default());
    }

    fn sample_tool_spec() -> ToolSpec {
        ToolSpec::Function(harw_tools::FunctionToolSpec {
            name: ToolName::new("get_weather"),
            description: "Get the current weather".to_owned(),
            parameters: harw_tools::JsonSchema {
                schema_type: Some(harw_tools::JsonSchemaType::Object),
                ..Default::default()
            },
            strict: false,
        })
    }

    #[test]
    fn test_build_messages_body_includes_tools_when_present() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("weather?");
        let request = ModelRequest {
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: vec![sample_tool_spec()],
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
        };

        let body = build_messages_body("claude-sonnet-5", 256, &request);
        let tools = body
            .get("tools")
            .and_then(Value::as_array)
            .expect("tools array");
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0].get("name").and_then(Value::as_str),
            Some("get_weather")
        );
        assert_eq!(
            tools[0].get("description").and_then(Value::as_str),
            Some("Get the current weather")
        );
        assert_eq!(
            tools[0]
                .get("input_schema")
                .and_then(|s| s.get("type"))
                .and_then(Value::as_str),
            Some("object")
        );
    }

    #[test]
    fn test_build_messages_body_omits_tools_when_empty() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("hi");
        let request = ModelRequest {
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
        };
        let body = build_messages_body("m", 256, &request);
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn test_build_messages_body_maps_tool_call_to_tool_use_block() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_1"),
            "get_weather",
            serde_json::json!({"location": "Paris"}),
        );
        let request = ModelRequest {
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
        };
        let body = build_messages_body("m", 256, &request);
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("assistant")
        );
        let content = messages[0]
            .get("content")
            .and_then(Value::as_array)
            .expect("content array");
        assert_eq!(
            content[0].get("type").and_then(Value::as_str),
            Some("tool_use")
        );
        assert_eq!(content[0].get("id").and_then(Value::as_str), Some("call_1"));
        assert_eq!(
            content[0].get("name").and_then(Value::as_str),
            Some("get_weather")
        );
        assert_eq!(
            content[0]
                .get("input")
                .and_then(|i| i.get("location"))
                .and_then(Value::as_str),
            Some("Paris")
        );
    }

    #[test]
    fn test_build_messages_body_maps_tool_result_ok_to_tool_result_block() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_result(
            harw_types::ToolCallId::from_str("call_1"),
            ToolCallResult::success(serde_json::json!({"temp": 20})),
            5,
        );
        let request = ModelRequest {
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
        };
        let body = build_messages_body("m", 256, &request);
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages");
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("user")
        );
        let content = messages[0]
            .get("content")
            .and_then(Value::as_array)
            .expect("content array");
        assert_eq!(
            content[0].get("type").and_then(Value::as_str),
            Some("tool_result")
        );
        assert_eq!(
            content[0].get("tool_use_id").and_then(Value::as_str),
            Some("call_1")
        );
        assert!(content[0].get("is_error").is_none());
    }

    #[test]
    fn test_build_messages_body_maps_tool_result_err_sets_is_error() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_result(
            harw_types::ToolCallId::from_str("call_1"),
            ToolCallResult::error("boom"),
            5,
        );
        let request = ModelRequest {
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
        };
        let body = build_messages_body("m", 256, &request);
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages");
        let content = messages[0]
            .get("content")
            .and_then(Value::as_array)
            .expect("content array");
        assert_eq!(
            content[0].get("content").and_then(Value::as_str),
            Some("boom")
        );
        assert_eq!(
            content[0].get("is_error").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn test_build_messages_body_groups_parallel_tool_calls_and_results_alternate_roles() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_a"),
            "get_weather",
            serde_json::json!({"location": "Paris"}),
        );
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_b"),
            "get_weather",
            serde_json::json!({"location": "Berlin"}),
        );
        history.push_tool_result(
            harw_types::ToolCallId::from_str("call_a"),
            ToolCallResult::success(serde_json::json!({"temp": 20})),
            5,
        );
        history.push_tool_result(
            harw_types::ToolCallId::from_str("call_b"),
            ToolCallResult::success(serde_json::json!({"temp": 15})),
            5,
        );
        let request = ModelRequest {
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
        };
        let body = build_messages_body("m", 256, &request);
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages");

        // one grouped assistant message, one grouped user (tool_result) message.
        assert_eq!(messages.len(), 2);

        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("assistant")
        );
        let assistant_content = messages[0]
            .get("content")
            .and_then(Value::as_array)
            .expect("assistant content array");
        assert_eq!(assistant_content.len(), 2);
        assert_eq!(
            assistant_content[0].get("type").and_then(Value::as_str),
            Some("tool_use")
        );
        assert_eq!(
            assistant_content[0].get("id").and_then(Value::as_str),
            Some("call_a")
        );
        assert_eq!(
            assistant_content[1].get("id").and_then(Value::as_str),
            Some("call_b")
        );

        assert_eq!(
            messages[1].get("role").and_then(Value::as_str),
            Some("user")
        );
        let user_content = messages[1]
            .get("content")
            .and_then(Value::as_array)
            .expect("user content array");
        assert_eq!(user_content.len(), 2);
        assert_eq!(
            user_content[0].get("type").and_then(Value::as_str),
            Some("tool_result")
        );
        assert_eq!(
            user_content[0].get("tool_use_id").and_then(Value::as_str),
            Some("call_a")
        );
        assert_eq!(
            user_content[1].get("tool_use_id").and_then(Value::as_str),
            Some("call_b")
        );
    }

    #[test]
    fn test_build_messages_body_grouped_tool_result_keeps_is_error_flag() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_a"),
            "get_weather",
            serde_json::json!({"location": "Paris"}),
        );
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_b"),
            "get_weather",
            serde_json::json!({"location": "Berlin"}),
        );
        history.push_tool_result(
            harw_types::ToolCallId::from_str("call_a"),
            ToolCallResult::success(serde_json::json!({"temp": 20})),
            5,
        );
        history.push_tool_result(
            harw_types::ToolCallId::from_str("call_b"),
            ToolCallResult::error("boom"),
            5,
        );
        let request = ModelRequest {
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
        };
        let body = build_messages_body("m", 256, &request);
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages");
        let user_content = messages[1]
            .get("content")
            .and_then(Value::as_array)
            .expect("user content array");
        assert!(user_content[0].get("is_error").is_none());
        assert_eq!(
            user_content[1].get("is_error").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn test_build_messages_body_assistant_text_message_does_not_absorb_tool_use() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_assistant_text("here you go", None);
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_a"),
            "get_weather",
            serde_json::json!({"location": "Paris"}),
        );
        let request = ModelRequest {
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
        };
        let body = build_messages_body("m", 256, &request);
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages");

        // Text message keeps a string `content`; the tool_use block must land in
        // a separate, new assistant message rather than being appended to it.
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("assistant")
        );
        assert!(messages[0].get("content").expect("content").is_string());

        assert_eq!(
            messages[1].get("role").and_then(Value::as_str),
            Some("assistant")
        );
        let content = messages[1]
            .get("content")
            .and_then(Value::as_array)
            .expect("content array");
        assert_eq!(
            content[0].get("type").and_then(Value::as_str),
            Some("tool_use")
        );
    }

    #[test]
    fn test_extract_anthropic_tool_calls_parses_tool_use_blocks() {
        let body = serde_json::json!({
            "content": [
                { "type": "text", "text": "Let me check." },
                {
                    "type": "tool_use",
                    "id": "toolu_01",
                    "name": "get_weather",
                    "input": {"location": "Paris"}
                }
            ]
        });
        let calls = extract_anthropic_tool_calls(&body).expect("valid tool-use input");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id.as_str(), "toolu_01");
        assert_eq!(calls[0].name.as_str(), "get_weather");
        assert_eq!(
            calls[0].arguments.get("location").and_then(Value::as_str),
            Some("Paris")
        );
    }

    #[test]
    fn test_extract_anthropic_tool_calls_empty_when_no_tool_use() {
        let body = serde_json::json!({ "content": [{ "type": "text", "text": "hi" }] });
        assert!(
            extract_anthropic_tool_calls(&body)
                .expect("no tool-use blocks")
                .is_empty()
        );
    }

    #[test]
    fn test_extract_anthropic_tool_calls_rejects_missing_id_without_partial_calls() {
        let body = serde_json::json!({
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_valid",
                    "name": "valid_tool",
                    "input": {}
                },
                { "type": "tool_use", "name": "missing_id", "input": {} }
            ]
        });

        assert!(matches!(
            extract_anthropic_tool_calls(&body),
            Err(AnthropicToolCallParseError::MissingId)
        ));
    }

    #[test]
    fn test_extract_anthropic_tool_calls_rejects_missing_name_without_partial_calls() {
        let body = serde_json::json!({
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_valid",
                    "name": "valid_tool",
                    "input": {}
                },
                { "type": "tool_use", "id": "missing_name", "input": {} }
            ]
        });

        assert!(matches!(
            extract_anthropic_tool_calls(&body),
            Err(AnthropicToolCallParseError::MissingName)
        ));
    }

    #[test]
    fn test_extract_anthropic_tool_calls_rejects_non_string_id_and_name() {
        for (field, expected_error) in [
            ("id", AnthropicToolCallParseError::MissingId),
            ("name", AnthropicToolCallParseError::MissingName),
        ] {
            let body = serde_json::json!({
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_valid",
                        "name": "valid_tool",
                        "input": {}
                    },
                    {
                        "type": "tool_use",
                        "id": "toolu_invalid",
                        "name": "invalid_tool",
                        "input": {}
                    }
                ]
            });
            let mut body = body;
            body["content"][1][field] = serde_json::json!(42);

            assert!(matches!(
                extract_anthropic_tool_calls(&body),
                Err(error) if error == expected_error
            ));
        }
    }

    #[test]
    fn test_extract_anthropic_tool_calls_rejects_missing_input() {
        let body = serde_json::json!({
            "content": [{"type": "tool_use", "id": "toolu_01", "name": "get_weather"}]
        });

        assert!(extract_anthropic_tool_calls(&body).is_err());
    }

    #[test]
    fn test_extract_anthropic_tool_calls_rejects_null_input() {
        let body = serde_json::json!({
            "content": [{
                "type": "tool_use",
                "id": "toolu_01",
                "name": "get_weather",
                "input": null
            }]
        });

        assert!(extract_anthropic_tool_calls(&body).is_err());
    }

    #[test]
    fn test_extract_anthropic_tool_calls_rejects_scalar_input() {
        let body = serde_json::json!({
            "content": [{
                "type": "tool_use",
                "id": "toolu_01",
                "name": "get_weather",
                "input": "Paris"
            }]
        });

        assert!(extract_anthropic_tool_calls(&body).is_err());
    }

    #[test]
    fn test_extract_anthropic_tool_calls_accepts_empty_object_input() {
        let body = serde_json::json!({
            "content": [{
                "type": "tool_use",
                "id": "toolu_01",
                "name": "get_weather",
                "input": {}
            }]
        });

        let calls = extract_anthropic_tool_calls(&body).expect("empty object is valid input");
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0]
                .arguments
                .as_object()
                .is_some_and(|object| object.is_empty())
        );
    }

    #[test]
    fn anthropic_credential_headers_are_marked_sensitive() {
        let secret = "sk-ant-sensitive-header-value";
        for (credential, header_name) in [
            (
                AnthropicCredential::ApiKey(SecretString::new(secret.into())),
                "x-api-key",
            ),
            (
                AnthropicCredential::OAuth(SecretString::new(secret.into())),
                "authorization",
            ),
            (
                AnthropicCredential::Bearer(SecretString::new(secret.into())),
                "authorization",
            ),
        ] {
            let builder =
                super::super::http_client().post("https://api.anthropic.com/v1/messages");
            let request = apply_anthropic_credential(builder, &credential)
                .expect("credential header")
                .build()
                .expect("request builds");
            let value = request.headers().get(header_name).expect(header_name);
            assert!(value.is_sensitive(), "{header_name}");
            assert!(!format!("{:?}", request.headers()).contains(secret));
        }
    }

    #[test]
    fn official_anthropic_host_matches_default_base_url() {
        assert_eq!(
            reqwest::Url::parse(DEFAULT_ANTHROPIC_BASE_URL)
                .expect("default base URL")
                .host_str(),
            Some(ANTHROPIC_API_HOST)
        );
    }
}
