//! Prompt-Caching-Strategie je Provider/Modell (Token-Effizienz-Kontrakt,
//! Abschnitt `harw-provider-http`).
//!
//! ## Verantwortung
//! Entscheidet, welche Cache-Control-Marker ein ausgehender Request-Body
//! trägt: keine (`None`), ein implizit stabiles Präfix ohne explizite Marker
//! (`ImplicitPrefix`) oder explizite `cache_control: {"type":"ephemeral"}`-
//! Marker (`ExplicitEphemeral`). Das Modul kennt zwei Wire-Schemata:
//! - Chat-Completions-artige Bodies (`system`/`messages`/`tools`), verwendet
//!   von OpenAI-kompatiblen und DashScope-Chat-Endpoints
//!   ([`apply_chat_cache_control`]).
//! - Anthropic-Messages-Bodies (`system`-String, `tools`, `messages` mit
//!   Content-Arrays, [`apply_messages_cache_control`]).
//!
//! ## Nebenläufigkeit
//! Rein funktional, kein geteilter Zustand; alle Funktionen sind `Send + Sync`
//! frei nutzbar.
//!
//! ## Fehler
//! Keine — bei unerwarteter Body-Form (z. B. `messages` fehlt) bleibt der
//! Body unverändert (fail-closed: lieber kein Marker als ein falsch
//! platzierter).

use harw_config::PromptCachingMode;
use serde_json::Value;

/// DashScope-Modelle, die explizites Cache-Control unterstützen
/// (Präfix-Match, case-insensitiv; siehe Kontrakt-Vorgabe der Alibaba-Docs).
const DASHSCOPE_EXPLICIT_MODEL_PREFIXES: &[&str] = &[
    "qwen3.8-max",
    "qwen3.7-max",
    "qwen3.6-max",
    "qwen3-max",
    "qwen3.8-flash",
    "qwen3.7-flash",
    "qwen3.6-flash",
    "qwen3.5-flash",
    "qwen-flash",
    "qwen3.7-plus",
    "qwen3.6-plus",
    "qwen3.5-plus",
    "qwen-plus",
    "qwen3-coder-plus",
    "qwen3-coder-flash",
    "qwen3-vl-plus",
    "qwen3-vl-flash",
    "qwen3.8-2.4t",
    "qwen3.8-27b",
    "deepseek-v3.2",
    "kimi-k2.5",
    "kimi-k2.6",
    "kimi-k2.7-code",
    "glm-5.1",
];

/// Maximale Anzahl gleichzeitiger Cache-Breakpoints (Anthropic und DashScope
/// erlauben beide höchstens 4 Marker pro Request).
const MAX_BREAKPOINTS: u8 = 4;

/// Anzahl der Nachrichten, nach denen — von hinten gezählt — ein weiterer
/// Zwischen-Marker gesetzt wird (Kontrakt-Vorgabe: alle 18 Nachrichten).
const INTERMEDIATE_MARKER_STRIDE: usize = 18;

/// Gewählte Prompt-Caching-Strategie für einen Request.
///
/// # Description
/// [`CacheStrategy::None`] setzt keine Marker. [`CacheStrategy::ImplicitPrefix`]
/// verlässt sich auf ein stabiles Verlaufs-Präfix, das der Provider automatisch
/// cached — der Body bleibt unverändert. [`CacheStrategy::ExplicitEphemeral`]
/// setzt aktiv `cache_control`-Marker, begrenzt auf `max_breakpoints`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStrategy {
    /// Kein Caching-Verhalten wird aktiv gesteuert.
    None,
    /// Stabiles Präfix ohne explizite Marker; Provider cached automatisch.
    ImplicitPrefix,
    /// Explizite `cache_control: {"type":"ephemeral"}`-Marker.
    ExplicitEphemeral {
        /// Obergrenze gleichzeitig gesetzter Marker in diesem Request.
        max_breakpoints: u8,
    },
}

impl CacheStrategy {
    /// Menschenlesbares, stabiles Label für strukturierte Logs
    /// (`tracing::debug!(strategy = strategy.label(), ..)`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ImplicitPrefix => "implicit_prefix",
            Self::ExplicitEphemeral { .. } => "explicit_ephemeral",
        }
    }
}

/// `true`, wenn `model` (case-insensitiv) mit einem der explizit
/// cache-fähigen DashScope-Modellpräfixe beginnt.
fn dashscope_model_is_explicit_capable(model: &str) -> bool {
    let model_lower = model.to_ascii_lowercase();
    DASHSCOPE_EXPLICIT_MODEL_PREFIXES
        .iter()
        .any(|prefix| model_lower.starts_with(prefix))
}

/// Bestimmt die anzuwendende [`CacheStrategy`] für `provider_name`/`model`.
///
/// # Description
/// Ein `override_mode` gewinnt immer: `Explicit` → `ExplicitEphemeral{4}`,
/// `Implicit` → `ImplicitPrefix`, `None` → [`CacheStrategy::None`]. Fehlt der
/// Override, entscheidet der Provider:
/// - Provider-Name enthält `dashscope` (case-insensitiv) oder `model` matcht
///   die DashScope-Explicit-Liste: explizit cache-fähige Modelle bekommen
///   `ExplicitEphemeral{4}`, alle anderen DashScope-Modelle `ImplicitPrefix`.
/// - Provider-Name ist `anthropic` (case-insensitiv) oder `model` beginnt mit
///   `claude`: `ExplicitEphemeral{4}`.
/// - Alles andere: `ImplicitPrefix` (ein stabiles Präfix ist immer
///   vorteilhaft; implizit bedeutet keine Marker).
///
/// # Arguments
/// - `provider_name` (`&str`): konfigurierter Provider-Bezeichner.
/// - `model` (`&str`): effektive Modell-ID des Requests.
/// - `override_mode` (`Option<PromptCachingMode>`): Modell-Override aus
///   `ModelToml::prompt_caching`, falls gesetzt.
///
/// # Returns
/// Die zu verwendende [`CacheStrategy`].
#[must_use]
pub fn resolve_cache_strategy(
    provider_name: &str,
    model: &str,
    override_mode: Option<PromptCachingMode>,
) -> CacheStrategy {
    if let Some(mode) = override_mode {
        return match mode {
            PromptCachingMode::Explicit => CacheStrategy::ExplicitEphemeral {
                max_breakpoints: MAX_BREAKPOINTS,
            },
            PromptCachingMode::Implicit => CacheStrategy::ImplicitPrefix,
            PromptCachingMode::None => CacheStrategy::None,
        };
    }

    let provider_lower = provider_name.to_ascii_lowercase();
    let is_dashscope = provider_lower.contains("dashscope");
    let model_is_dashscope_explicit = dashscope_model_is_explicit_capable(model);

    if is_dashscope || model_is_dashscope_explicit {
        if model_is_dashscope_explicit {
            return CacheStrategy::ExplicitEphemeral {
                max_breakpoints: MAX_BREAKPOINTS,
            };
        }
        if is_dashscope {
            return CacheStrategy::ImplicitPrefix;
        }
    }

    let model_lower = model.to_ascii_lowercase();
    if provider_lower == "anthropic" || model_lower.starts_with("claude") {
        return CacheStrategy::ExplicitEphemeral {
            max_breakpoints: MAX_BREAKPOINTS,
        };
    }

    CacheStrategy::ImplicitPrefix
}

/// Wandelt einen String- oder bereits-Array-`content`-Wert in ein
/// `[{"type":"text","text":..}]`-Array und hängt `cache_control` an den
/// letzten Block. Bereits vorhandene Nicht-String-Inhalte werden nicht
/// verändert, außer dass der letzte Block einen Marker bekommt.
fn mark_last_text_block(content: &mut Value, applied: &mut u8, max_breakpoints: u8) -> bool {
    if *applied >= max_breakpoints {
        return false;
    }
    if let Some(text) = content.as_str() {
        let text = text.to_owned();
        *content = Value::Array(vec![serde_json::json!({
            "type": "text",
            "text": text,
            "cache_control": { "type": "ephemeral" },
        })]);
        *applied += 1;
        return true;
    }
    if let Some(array) = content.as_array_mut()
        && let Some(last) = array.last_mut()
    {
        last["cache_control"] = serde_json::json!({ "type": "ephemeral" });
        *applied += 1;
        return true;
    }
    false
}

/// Setzt Cache-Control-Marker auf einen Chat-Completions-artigen Request-Body
/// (`system`/`messages`/`tools`-Schema).
///
/// # Description
/// [`CacheStrategy::None`] und [`CacheStrategy::ImplicitPrefix`] lassen den
/// Body unverändert (implizit heißt: kein Marker, stabiles Präfix genügt).
/// Für [`CacheStrategy::ExplicitEphemeral`]:
/// - `messages[0]` mit `role == "system"`: `content` (String) wird zu einem
///   Text-Block-Array mit `cache_control` konvertiert.
/// - Die letzte Nachricht in `messages` bekommt ebenfalls einen Marker auf
///   ihrem letzten Content-Block (String-`content` wird dafür in ein
///   Text-Block-Array konvertiert).
/// - Liegen mehr als 18 Nachrichten zwischen System- und letztem Marker,
///   werden zusätzliche Zwischen-Marker alle 18 Nachrichten (von hinten
///   gezählt) gesetzt, bis `max_breakpoints` erreicht ist.
/// - `tools` wird nie verändert (DashScope: Marker dort nicht erlaubt).
///
/// # Arguments
/// - `body` (`&mut serde_json::Value`): der Chat-Completions-Request-Body.
/// - `strategy` ([`CacheStrategy`]): die anzuwendende Strategie.
pub fn apply_chat_cache_control(body: &mut Value, strategy: CacheStrategy) {
    let CacheStrategy::ExplicitEphemeral { max_breakpoints } = strategy else {
        return;
    };
    let mut applied: u8 = 0;

    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if messages.is_empty() {
        return;
    }

    // (a) System-Block (nur wenn messages[0] role == "system").
    if messages[0].get("role").and_then(Value::as_str) == Some("system")
        && let Some(content) = messages[0].get_mut("content")
    {
        mark_last_text_block(content, &mut applied, max_breakpoints);
    }

    // (b) Letzte Nachricht des Verlaufs.
    let len = messages.len();
    if let Some(last) = messages.last_mut()
        && let Some(content) = last.get_mut("content")
        && !content.is_null()
    {
        mark_last_text_block(content, &mut applied, max_breakpoints);
    }

    // (c) Zwischen-Marker alle INTERMEDIATE_MARKER_STRIDE Nachrichten,
    // von hinten gezählt, solange das Breakpoint-Budget reicht.
    if len > INTERMEDIATE_MARKER_STRIDE {
        let mut index = len.saturating_sub(1 + INTERMEDIATE_MARKER_STRIDE);
        loop {
            if applied >= max_breakpoints {
                break;
            }
            if let Some(content) = messages[index].get_mut("content")
                && !content.is_null()
            {
                mark_last_text_block(content, &mut applied, max_breakpoints);
            }
            if index < INTERMEDIATE_MARKER_STRIDE {
                break;
            }
            index -= INTERMEDIATE_MARKER_STRIDE;
        }
    }
}

/// Setzt Cache-Control-Marker auf einen Anthropic-Messages-Request-Body
/// (`system`-String, `tools`-Array, `messages` mit Content-Arrays).
///
/// # Description
/// [`CacheStrategy::None`] und [`CacheStrategy::ImplicitPrefix`] lassen den
/// Body unverändert. Für [`CacheStrategy::ExplicitEphemeral`]:
/// - `body.system` (falls String) wird zu
///   `[{"type":"text","text":..,"cache_control":..}]`.
/// - Der letzte Eintrag von `body.tools` (falls vorhanden) bekommt
///   `cache_control`.
/// - Der letzte Content-Block der letzten `messages`-Nachricht bekommt
///   `cache_control`.
///
/// Insgesamt werden höchstens `max_breakpoints` Marker gesetzt.
///
/// # Arguments
/// - `body` (`&mut serde_json::Value`): der Anthropic-Messages-Request-Body.
/// - `strategy` ([`CacheStrategy`]): die anzuwendende Strategie.
pub fn apply_messages_cache_control(body: &mut Value, strategy: CacheStrategy) {
    let CacheStrategy::ExplicitEphemeral { max_breakpoints } = strategy else {
        return;
    };
    let mut applied: u8 = 0;

    if applied < max_breakpoints
        && let Some(system) = body.get("system").and_then(Value::as_str)
    {
        let system = system.to_owned();
        body["system"] = Value::Array(vec![serde_json::json!({
            "type": "text",
            "text": system,
            "cache_control": { "type": "ephemeral" },
        })]);
        applied += 1;
    }

    if applied < max_breakpoints
        && let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut)
        && let Some(last_tool) = tools.last_mut()
    {
        last_tool["cache_control"] = serde_json::json!({ "type": "ephemeral" });
        applied += 1;
    }

    if applied < max_breakpoints
        && let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut)
        && let Some(last_message) = messages.last_mut()
        && let Some(content) = last_message.get_mut("content")
        && !content.is_null()
    {
        mark_last_text_block(content, &mut applied, max_breakpoints);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_dashscope_kimi_k3_is_implicit() {
        let strategy = resolve_cache_strategy("dashscope", "kimi-k3", None);
        assert_eq!(strategy, CacheStrategy::ImplicitPrefix);
    }

    #[test]
    fn resolve_dashscope_qwen3_coder_plus_is_explicit() {
        let strategy = resolve_cache_strategy("dashscope", "qwen3-coder-plus", None);
        assert_eq!(
            strategy,
            CacheStrategy::ExplicitEphemeral { max_breakpoints: 4 }
        );
    }

    #[test]
    fn resolve_anthropic_provider_is_explicit() {
        let strategy = resolve_cache_strategy("anthropic", "claude-opus-5", None);
        assert_eq!(
            strategy,
            CacheStrategy::ExplicitEphemeral { max_breakpoints: 4 }
        );
    }

    #[test]
    fn resolve_claude_model_prefix_is_explicit_even_off_anthropic_provider() {
        let strategy = resolve_cache_strategy("foundry", "claude-sonnet-5", None);
        assert_eq!(
            strategy,
            CacheStrategy::ExplicitEphemeral { max_breakpoints: 4 }
        );
    }

    #[test]
    fn resolve_unknown_provider_defaults_to_implicit() {
        let strategy = resolve_cache_strategy("openai", "gpt-5", None);
        assert_eq!(strategy, CacheStrategy::ImplicitPrefix);
    }

    #[test]
    fn resolve_override_wins_over_provider_default() {
        let strategy = resolve_cache_strategy("openai", "gpt-5", Some(PromptCachingMode::Explicit));
        assert_eq!(
            strategy,
            CacheStrategy::ExplicitEphemeral { max_breakpoints: 4 }
        );

        let strategy =
            resolve_cache_strategy("anthropic", "claude-opus-5", Some(PromptCachingMode::None));
        assert_eq!(strategy, CacheStrategy::None);

        let strategy = resolve_cache_strategy(
            "dashscope",
            "qwen3-coder-plus",
            Some(PromptCachingMode::Implicit),
        );
        assert_eq!(strategy, CacheStrategy::ImplicitPrefix);
    }

    #[test]
    fn apply_chat_cache_control_implicit_leaves_body_unchanged() {
        let mut body = serde_json::json!({
            "model": "m",
            "messages": [
                { "role": "system", "content": "sys" },
                { "role": "user", "content": "hi" },
            ],
        });
        let before = body.clone();
        apply_chat_cache_control(&mut body, CacheStrategy::ImplicitPrefix);
        assert_eq!(body, before);
        apply_chat_cache_control(&mut body, CacheStrategy::None);
        assert_eq!(body, before);
    }

    #[test]
    fn apply_chat_cache_control_explicit_marks_system_and_last_message() {
        let mut body = serde_json::json!({
            "model": "m",
            "messages": [
                { "role": "system", "content": "sys" },
                { "role": "user", "content": "hi" },
            ],
            "tools": [ { "type": "function", "function": { "name": "f" } } ],
        });
        let before_tools = body["tools"].clone();
        apply_chat_cache_control(
            &mut body,
            CacheStrategy::ExplicitEphemeral { max_breakpoints: 4 },
        );

        let system_content = &body["messages"][0]["content"];
        assert!(system_content.is_array());
        assert_eq!(
            system_content[0]["cache_control"]["type"].as_str(),
            Some("ephemeral")
        );

        let last_content = &body["messages"][1]["content"];
        assert!(last_content.is_array());
        assert_eq!(
            last_content[0]["cache_control"]["type"].as_str(),
            Some("ephemeral")
        );

        // tools bleibt vollständig unverändert.
        assert_eq!(body["tools"], before_tools);
    }

    #[test]
    fn apply_chat_cache_control_never_exceeds_max_breakpoints() {
        let mut messages = vec![serde_json::json!({ "role": "system", "content": "sys" })];
        for i in 0..60 {
            messages.push(serde_json::json!({ "role": "user", "content": format!("m{i}") }));
        }
        let mut body = serde_json::json!({ "model": "m", "messages": messages });
        apply_chat_cache_control(
            &mut body,
            CacheStrategy::ExplicitEphemeral { max_breakpoints: 4 },
        );

        let marker_count = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| {
                message["content"].as_array().is_some_and(|content| {
                    content
                        .iter()
                        .any(|block| block.get("cache_control").is_some())
                })
            })
            .count();
        assert!(marker_count <= 4);
    }

    #[test]
    fn apply_messages_cache_control_implicit_leaves_body_unchanged() {
        let mut body = serde_json::json!({
            "system": "sys",
            "messages": [ { "role": "user", "content": [ { "type": "text", "text": "hi" } ] } ],
        });
        let before = body.clone();
        apply_messages_cache_control(&mut body, CacheStrategy::ImplicitPrefix);
        assert_eq!(body, before);
        apply_messages_cache_control(&mut body, CacheStrategy::None);
        assert_eq!(body, before);
    }

    #[test]
    fn apply_messages_cache_control_explicit_marks_system_tools_and_last_message() {
        let mut body = serde_json::json!({
            "system": "sys",
            "tools": [ { "name": "a" }, { "name": "b" } ],
            "messages": [ { "role": "user", "content": [ { "type": "text", "text": "hi" } ] } ],
        });
        apply_messages_cache_control(
            &mut body,
            CacheStrategy::ExplicitEphemeral { max_breakpoints: 4 },
        );

        assert!(body["system"].is_array());
        assert_eq!(
            body["system"][0]["cache_control"]["type"].as_str(),
            Some("ephemeral")
        );
        assert_eq!(
            body["tools"][1]["cache_control"]["type"].as_str(),
            Some("ephemeral")
        );
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["type"].as_str(),
            Some("ephemeral")
        );
        let marker_count = [
            body["system"][0].get("cache_control").is_some(),
            body["tools"][1].get("cache_control").is_some(),
            body["messages"][0]["content"][0]
                .get("cache_control")
                .is_some(),
        ]
        .into_iter()
        .filter(|marked| *marked)
        .count();
        assert!(marker_count <= 4);
    }
}
