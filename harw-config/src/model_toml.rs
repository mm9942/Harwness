use serde::{Deserialize, Serialize};

use harw_types::ReasoningEffort;

use crate::error::ConfigResult;
use crate::provider_toml::RateLimitToml;

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
    /// Token-Streaming (SSE) für genau dieses Modell; übersteuert
    /// `ProviderToml::stream`. `false` erzwingt den Pro-Runde-Fallback
    /// (z. B. für Gateways ohne SSE-Unterstützung).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Client-seitiges Budget nur für dieses Modell (Sektion `[rate_limit]`).
    /// Die Budget-Felder bilden einen eigenen Bucket je (Provider, Modell),
    /// der **zusätzlich** zum Provider-Bucket aus
    /// `ProviderToml::rate_limit` gilt; `enabled`/`safety_margin_pct`
    /// (Header-Pacing) sind hier ohne Wirkung. `None` = kein Modell-Bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitToml>,
}

impl ModelToml {
    /// Prüft Invarianten, die die TOML-Deserialisierung nicht ausdrücken kann.
    ///
    /// # Errors
    /// [`crate::ConfigError::Invalid`], wenn `[rate_limit]` gegen
    /// [`RateLimitToml::validate`] verstößt (z. B. ein Budget-Feld `0`).
    pub fn validate(&self) -> ConfigResult<()> {
        if let Some(rate_limit) = &self.rate_limit {
            rate_limit.validate(&format!(
                "model '{}' (provider '{}')",
                self.id, self.provider
            ))?;
        }
        Ok(())
    }
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
    /// Runde 7, Teil L7: `Some(false)` heißt, das Modell beherrscht keine
    /// Werkzeugaufrufe — der Provider bietet ihm dann keine Werkzeuge an
    /// und hängt stattdessen einen kurzen Hinweis an. `None` = unbekannt
    /// (Werkzeuge werden angeboten). Anders als `tool_use` (Routing-Hinweis,
    /// Vorgabe `false`) wirkt nur ein ausdrückliches `false` hier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calling: Option<bool>,
    /// `Some(false)` heißt, das Modell versteht keine Bilder: der Provider
    /// sendet dann keine Bilder, sondern an ihrer Stelle einen Hinweistext
    /// mit den Maßen. `None` = unbekannt, Bilder werden gesendet (ein Modell
    /// ohne Bildeingabe antwortet dann mit einem Fehler des Servers, der
    /// sichtbar bleibt). Anders als `vision` (Routing-Hinweis, Vorgabe
    /// `false`) wirkt nur ein ausdrückliches `false` hier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_input: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_model_with_aliases_and_capabilities() -> TestResult {
        let src = r#"
            id = "gpt-5"
            provider = "openai"
            aliases = ["gpt5", "openai-flagship"]

            [capabilities]
            tool_use = true
            vision = true
        "#;
        let model: ModelToml = toml::from_str(src).map_err(ctx("model-toml parsen"))?;
        assert_eq!(model.aliases, vec!["gpt5", "openai-flagship"]);
        assert!(model.capabilities.tool_use);
        assert!(!model.capabilities.streaming);
        Ok(())
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
    fn test_model_with_explicit_prompt_caching_parses() -> TestResult {
        let src = r#"
            id = "claude-opus"
            provider = "anthropic"
            prompt_caching = "explicit"
        "#;
        let model: ModelToml = toml::from_str(src).map_err(ctx("model-toml parsen"))?;
        assert_eq!(model.prompt_caching, Some(PromptCachingMode::Explicit));
        Ok(())
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
    fn test_model_without_prompt_caching_is_none() -> TestResult {
        let src = r#"
            id = "claude-opus"
            provider = "anthropic"
        "#;
        let model: ModelToml = toml::from_str(src).map_err(ctx("model-toml parsen"))?;
        assert_eq!(model.prompt_caching, None);
        Ok(())
    }

    #[test]
    fn test_model_without_default_reasoning_effort_is_none() -> TestResult {
        let src = r#"
            id = "claude-opus"
            provider = "anthropic"
        "#;
        let model: ModelToml = toml::from_str(src).map_err(ctx("model-toml parsen"))?;
        assert!(model.default_reasoning_effort.is_none());
        let encoded = toml::to_string(&model).map_err(ctx("model-toml serialisieren"))?;
        assert!(!encoded.contains("default_reasoning_effort"));
        Ok(())
    }

    #[test]
    fn test_model_with_default_reasoning_effort_round_trips() -> TestResult {
        let src = r#"
            id = "claude-opus"
            provider = "anthropic"
            default_reasoning_effort = "xhigh"
        "#;
        let model: ModelToml = toml::from_str(src).map_err(ctx("model-toml parsen"))?;
        assert_eq!(
            model.default_reasoning_effort,
            Some(harw_types::ReasoningEffort::Xhigh)
        );
        Ok(())
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

    #[test]
    fn test_model_with_rate_limit_override_parses_and_validates() -> TestResult {
        let src = r#"
            id = "gpt-5"
            provider = "openai"

            [rate_limit]
            tokens_per_minute = 40000
            requests_per_minute = 100
        "#;
        let model: ModelToml = toml::from_str(src).map_err(ctx("model-toml parsen"))?;
        model
            .validate()
            .map_err(ctx("positive model budget is valid"))?;
        let rate_limit = model
            .rate_limit
            .as_ref()
            .ok_or(crate::test_support::TestError::Missing("model rate_limit"))?;
        assert_eq!(rate_limit.tokens_per_minute, Some(40_000));
        assert!(rate_limit.budget_enabled());
        Ok(())
    }

    #[test]
    fn test_model_rate_limit_zero_budget_is_rejected() -> TestResult {
        let src = r#"
            id = "gpt-5"
            provider = "openai"

            [rate_limit]
            requests_per_minute = 0
        "#;
        let model: ModelToml = toml::from_str(src).map_err(ctx("model-toml parsen"))?;
        let Err(error) = model.validate() else {
            return Err(crate::test_support::TestError::Unexpected(
                "requests_per_minute = 0 must be rejected".into(),
            ));
        };
        assert!(error.to_string().contains("model 'gpt-5'"));
        Ok(())
    }

    #[test]
    fn test_model_without_rate_limit_is_none_and_not_serialized() -> TestResult {
        let src = r#"
            id = "gpt-5"
            provider = "openai"
        "#;
        let model: ModelToml = toml::from_str(src).map_err(ctx("model-toml parsen"))?;
        assert!(model.rate_limit.is_none());
        model.validate().map_err(ctx("no rate_limit is valid"))?;
        let encoded = toml::to_string(&model).map_err(ctx("model-toml serialisieren"))?;
        assert!(!encoded.contains("rate_limit"));
        Ok(())
    }
}
