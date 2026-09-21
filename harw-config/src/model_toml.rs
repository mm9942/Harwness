use serde::{Deserialize, Serialize};

use harw_types::ReasoningEffort;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelToml {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub provider: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// Prompt-Caching-Modus für dieses Modell; `None` = kein Override, der
    /// Provider-Default (siehe `harw-provider-http::cache_strategy`) greift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_caching: Option<PromptCachingMode>,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub input_types: Vec<String>,
    #[serde(default)]
    pub capabilities: ModelCapabilitiesToml,
    /// Standard-Reasoning-Effort für Sessions/Kinder, die dieses Modell
    /// verwenden, sofern nicht durch eine spezifischere Ebene überschrieben
    /// (Agenten-Definition). `None` = keine Modell-seitige Vorgabe.
    ///
    /// Gleicher Typ-Stil wie `ProviderToml::default_reasoning_effort`
    /// (`Option<ReasoningEffort>` statt `Option<String>`), weil
    /// `harw_types::ReasoningEffort` bereits serde-fähig ist und
    /// `harw-config` bereits von `harw-types` abhängt. Die **Rangfolge**
    /// gegenüber Provider-/Agenten-Ebene ist NICHT Teil dieser Änderung.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_effort: Option<ReasoningEffort>,
}

/// Steuert, wie Prompt-Caching für ein Modell angewendet wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptCachingMode {
    /// Explizit: `cache_control`-Marker werden aktiv gesetzt.
    Explicit,
    /// Implizit: es wird nur ein stabiles Präfix gepflegt, ohne explizite
    /// Marker (Provider cached automatisch).
    Implicit,
    /// Kein Caching.
    None,
}

/// Zusätzliche Fähigkeits-Metadaten für Routing-Entscheidungen.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCapabilitiesToml {
    #[serde(default)]
    pub tool_use: bool,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub json_mode: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_with_aliases_and_capabilities() {
        let src = r#"
            id = "gpt-5"
            provider = "openai"
            aliases = ["gpt5", "openai-flagship"]

            [capabilities]
            tool_use = true
            vision = true
        "#;
        let model: ModelToml = toml::from_str(src).unwrap();
        assert_eq!(model.aliases, vec!["gpt5", "openai-flagship"]);
        assert!(model.capabilities.tool_use);
        assert!(!model.capabilities.streaming);
    }

    #[test]
    fn test_model_rejects_misspelled_context_field() {
        let src = r#"
            id = "gpt-5"
            provider = "openai"
            context_windw = 128000
        "#;

        assert!(toml::from_str::<ModelToml>(src).is_err());
    }

    #[test]
    fn test_model_rejects_misspelled_capability_field() {
        let src = r#"
            id = "gpt-5"
            provider = "openai"

            [capabilities]
            tool_usee = true
        "#;

        assert!(toml::from_str::<ModelToml>(src).is_err());
    }

    #[test]
    fn test_model_with_explicit_prompt_caching_parses() {
        let src = r#"
            id = "claude-opus"
            provider = "anthropic"
            prompt_caching = "explicit"
        "#;
        let model: ModelToml = toml::from_str(src).unwrap();
        assert_eq!(model.prompt_caching, Some(PromptCachingMode::Explicit));
    }

    #[test]
    fn test_model_rejects_unknown_prompt_caching_value() {
        let src = r#"
            id = "claude-opus"
            provider = "anthropic"
            prompt_caching = "sometimes"
        "#;
        assert!(toml::from_str::<ModelToml>(src).is_err());
    }

    #[test]
    fn test_model_without_prompt_caching_is_none() {
        let src = r#"
            id = "claude-opus"
            provider = "anthropic"
        "#;
        let model: ModelToml = toml::from_str(src).unwrap();
        assert_eq!(model.prompt_caching, None);
    }

    #[test]
    fn test_model_without_default_reasoning_effort_is_none() {
        let src = r#"
            id = "claude-opus"
            provider = "anthropic"
        "#;
        let model: ModelToml = toml::from_str(src).unwrap();
        assert!(model.default_reasoning_effort.is_none());
        assert!(!toml::to_string(&model).unwrap().contains("default_reasoning_effort"));
    }

    #[test]
    fn test_model_with_default_reasoning_effort_round_trips() {
        let src = r#"
            id = "claude-opus"
            provider = "anthropic"
            default_reasoning_effort = "xhigh"
        "#;
        let model: ModelToml = toml::from_str(src).unwrap();
        assert_eq!(
            model.default_reasoning_effort,
            Some(harw_types::ReasoningEffort::Xhigh)
        );
    }

    #[test]
    fn test_model_rejects_unknown_default_reasoning_effort_value() {
        let src = r#"
            id = "claude-opus"
            provider = "anthropic"
            default_reasoning_effort = "ultra"
        "#;
        assert!(toml::from_str::<ModelToml>(src).is_err());
    }
}
