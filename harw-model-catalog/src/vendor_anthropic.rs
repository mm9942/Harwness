//! Vendor-specific model descriptors and observed behaviors for Anthropic Claude models.
//!
//! Implements the Anthropic slice of the Layer 2 / Layer 4 catalog
//! (see `docs/design/model-catalog-v2.md`, §3 and §5).
//!
//! ## Quellen (Stand 2026-09-24, offizielle Anthropic-Doku)
//! - IDs, Kontext, Max-Output, Thinking-Modus, Status:
//!   <https://platform.claude.com/docs/en/about-claude/models/overview> und die
//!   Modellseiten `…/docs/en/models/<modell>/overview`.
//! - Preise (Input/Output/Cache-Lesen pro MTok):
//!   <https://platform.claude.com/docs/en/about-claude/pricing>.
//! - Lebenszyklus (Active/Legacy/Retired):
//!   <https://platform.claude.com/docs/en/about-claude/model-deprecations>.
//! - Tool-Calling- und Agent-Feature-Flags der vor 2026-09 vorhandenen Zeilen
//!   stammen aus `docs/research/models/anthropic.json` (retrieved 2026-07-16).
//!
//! ## Verantwortung
//! - Curates [`ModelDescriptor`] entries for every Anthropic model, current
//!   models first, then legacy records (`lifecycle: Deprecated`) and retired
//!   records (`lifecycle: Retired`).
//! - Derives bootstrap [`ObservedModelBehavior`] values via `ObservedModelBehavior::bootstrap`.
//!
//! ## Wichtigste Typen
//! - [`anthropic_descriptors`] — Vec of all Anthropic `ModelDescriptor`s.
//! - [`anthropic_observations`] — Bootstrap observations derived from descriptors.
//!
//! ## Nebenläufigkeitsmodell
//! All functions are pure, deterministic, and produce `Send + Sync`-compatible values.
//!
//! ## Fehlertypen
//! Infallible — all constructors are deterministic.
//!
//! ## Beispiel
//! ```rust,no_run
//! use harw_model_catalog::vendor_anthropic::anthropic_descriptors;
//! let catalog = anthropic_descriptors();
//! assert!(catalog.iter().any(|d| d.model == "claude-opus-5-5"));
//! ```

use crate::descriptor::{
    AgentFeatureSet, Modality, ModalitySet, ModelCapabilities, ModelDescriptor, ModelId,
    ModelLifecycle, Pricing, PromptCachingSupport, ProviderId, ReasoningSupport, StreamingSupport,
    StructuredOutputSupport, TokenCount, ToolCallingSupport,
};
use crate::observed::ObservedModelBehavior;

/// Kontextfenster 1M Tokens (Modelle ab der 4.6-Generation).
const CTX_1M: TokenCount = 1_000_000;
/// Kontextfenster 200K Tokens (Haiku 4.5, Opus 4.5, Sonnet 4.5, Opus 4.1).
const CTX_200K: TokenCount = 200_000;
/// Max-Output 128K Tokens der synchronen Messages-API (alle 1M-Modelle).
const OUT_128K: Option<TokenCount> = Some(128_000);
/// Max-Output 64K Tokens der synchronen Messages-API.
const OUT_64K: Option<TokenCount> = Some(64_000);

/// Kompakte Beschreibung einer Claude-Zeile; die gemeinsamen Felder
/// (Text+Bild, JSON-Schema, Effort, explizites Caching, SSE) setzt [`claude`].
struct ClaudeSpec {
    /// Claude-API-ID.
    model: &'static str,
    /// Kontextfenster in Tokens.
    context_window: TokenCount,
    /// Max-Output der synchronen Messages-API.
    max_output_tokens: Option<TokenCount>,
    /// Native oder nur parallele Tool-Aufrufe.
    tool_calling: ToolCallingSupport,
    /// Natives Computer-Use-Werkzeug.
    computer_use: bool,
    /// Natives Code-Execution-Werkzeug.
    code_execution: bool,
    /// `(input, output, cache_read)` in USD pro Million Tokens.
    price: (f32, f32, f32),
    /// Lebenszyklus.
    lifecycle: ModelLifecycle,
}

/// Baut aus einer [`ClaudeSpec`] einen vollständigen [`ModelDescriptor`].
///
/// # Arguments
/// - `spec` (`ClaudeSpec`): modellspezifische Werte.
///
/// # Returns
/// Deskriptor mit `provider = "anthropic"`.
fn claude(spec: ClaudeSpec) -> ModelDescriptor {
    let (input, output, cache_read) = spec.price;
    ModelDescriptor {
        provider: ProviderId::from("anthropic"),
        model: ModelId::from(spec.model),
        context_window: spec.context_window,
        max_output_tokens: spec.max_output_tokens,
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: spec.tool_calling,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Explicit,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet {
                computer_use: spec.computer_use,
                code_execution: spec.code_execution,
                built_in_search: false,
                file_search: false,
            },
        },
        pricing: Some(Pricing {
            input_per_mtoken_usd: input,
            output_per_mtoken_usd: output,
            cached_input_per_mtoken_usd: Some(cache_read),
        }),
        lifecycle: spec.lifecycle,
    }
}

/// Returns curated [`ModelDescriptor`] entries for all known Anthropic Claude models.
///
/// # Description
/// Reihenfolge: aktuelle Modelle (Fable 5.1, Opus 5.5, Sonnet 5, Haiku 4.5),
/// dann die nur auf Einladung verfügbaren Mythos-Modelle (`Preview`), dann
/// die noch verfügbaren Legacy-Modelle (`Deprecated`), zuletzt auf der
/// Claude-API zurückgezogene Modelle (`Retired`).
///
/// Mapping rules applied (Doku → Rust enum variants):
/// - Status „Active (latest)“ → `ModelLifecycle::Ga`
/// - „Invite only“ / „limited availability“ → `ModelLifecycle::Preview`
/// - „Active (legacy)“ → `ModelLifecycle::Deprecated`
/// - „Retired“ → `ModelLifecycle::Retired`
/// - adaptive oder extended thinking → `ReasoningSupport::Effort`
///   (die Wire-Details regelt `harw-provider-http::anthropic_caps`)
/// - Prompt-Caching → `PromptCachingSupport::Explicit`
/// - `pricing` = Basis-Input, Output und Cache-Lesen laut Pricing-Seite
///   (Cache-Lesen 0,1× Input; 0,025× bei Fable/Mythos 5.1; 0,05× bei Opus 5.5).
///
/// # Returns
/// `Vec<ModelDescriptor>` mit 15 Einträgen (4 aktuell + 3 Mythos + 7 Legacy + 1 Retired).
///
/// # Concurrency
/// Pure, no side effects, no locks.
///
/// # Examples
/// ```rust,no_run
/// use harw_model_catalog::vendor_anthropic::anthropic_descriptors;
/// let catalog = anthropic_descriptors();
/// assert!(catalog.iter().any(|d| d.model == "claude-fable-5-1"));
/// ```
pub fn anthropic_descriptors() -> Vec<ModelDescriptor> {
    use ModelLifecycle::{Deprecated, Ga, Preview, Retired};
    use ToolCallingSupport::{Native, Parallel};
    vec![
        // ── Aktuelle Modelle ──────────────────────────────────────────────────
        // models/fable-5-1/overview: 1M / 128K, adaptive (immer an); Pricing:
        // $10/$50, Cache-Lesen $0.25.
        claude(ClaudeSpec {
            model: "claude-fable-5-1",
            context_window: CTX_1M,
            max_output_tokens: OUT_128K,
            tool_calling: Native,
            computer_use: true,
            code_execution: true,
            price: (10.0, 50.0, 0.25),
            lifecycle: Ga,
        }),
        // models/opus-5-5/overview: 1M / 128K, adaptive (immer an), Default-
        // Effort medium; Pricing: $4/$20, Cache-Lesen $0.20. Computer Use:
        // nur `computer_20251124` wird dort als nicht akzeptiert genannt.
        claude(ClaudeSpec {
            model: "claude-opus-5-5",
            context_window: CTX_1M,
            max_output_tokens: OUT_128K,
            tool_calling: Native,
            computer_use: true,
            code_execution: true,
            price: (4.0, 20.0, 0.20),
            lifecycle: Ga,
        }),
        // models/overview: 1M / 128K, adaptive; Pricing: $2/$10 (seit
        // 2026-09-01 Standardpreis), Cache-Lesen $0.20.
        claude(ClaudeSpec {
            model: "claude-sonnet-5",
            context_window: CTX_1M,
            max_output_tokens: OUT_128K,
            tool_calling: Native,
            computer_use: true,
            code_execution: true,
            price: (2.0, 10.0, 0.20),
            lifecycle: Ga,
        }),
        // models/overview: 200K / 64K, extended thinking, kein Effort;
        // Pricing: $1/$5, Cache-Lesen $0.10.
        claude(ClaudeSpec {
            model: "claude-haiku-4-5-20251001",
            context_window: CTX_200K,
            max_output_tokens: OUT_64K,
            tool_calling: Native,
            computer_use: true,
            code_execution: true,
            price: (1.0, 5.0, 0.10),
            lifecycle: Ga,
        }),
        // ── Nur auf Einladung (Project Glasswing) ─────────────────────────────
        // models/mythos-5-1/overview: „Invite only“, Spezifikation und Preise
        // wie Fable 5.1.
        claude(ClaudeSpec {
            model: "claude-mythos-5-1",
            context_window: CTX_1M,
            max_output_tokens: OUT_128K,
            tool_calling: Native,
            computer_use: true,
            code_execution: true,
            price: (10.0, 50.0, 0.25),
            lifecycle: Preview,
        }),
        // Pricing: „Claude Mythos 5 (limited availability)“, $10/$50, Cache $1;
        // models/fable-5/overview: teilt Spezifikation mit Fable 5.
        claude(ClaudeSpec {
            model: "claude-mythos-5",
            context_window: CTX_1M,
            max_output_tokens: OUT_128K,
            tool_calling: Native,
            computer_use: true,
            code_execution: true,
            price: (10.0, 50.0, 1.0),
            lifecycle: Preview,
        }),
        // ── Legacy (noch verfügbar) ───────────────────────────────────────────
        // models/fable-5/overview: „Legacy“, 1M / 128K, $10/$50, Cache $1.
        claude(ClaudeSpec {
            model: "claude-fable-5",
            context_window: CTX_1M,
            max_output_tokens: OUT_128K,
            tool_calling: Native,
            computer_use: true,
            code_execution: true,
            price: (10.0, 50.0, 1.0),
            lifecycle: Deprecated,
        }),
        // models/opus-5/overview: „Legacy“, 1M / 128K, $5/$25, Cache $0.50.
        claude(ClaudeSpec {
            model: "claude-opus-5",
            context_window: CTX_1M,
            max_output_tokens: OUT_128K,
            tool_calling: Native,
            computer_use: true,
            code_execution: true,
            price: (5.0, 25.0, 0.50),
            lifecycle: Deprecated,
        }),
        // models/opus-4-8/overview: „Legacy“, 1M / 128K, $5/$25, Cache $0.50.
        claude(ClaudeSpec {
            model: "claude-opus-4-8",
            context_window: CTX_1M,
            max_output_tokens: OUT_128K,
            tool_calling: Native,
            computer_use: true,
            code_execution: true,
            price: (5.0, 25.0, 0.50),
            lifecycle: Deprecated,
        }),
        // models/opus-4-7/overview: „Legacy“, 1M / 128K, $5/$25, Cache $0.50.
        claude(ClaudeSpec {
            model: "claude-opus-4-7",
            context_window: CTX_1M,
            max_output_tokens: OUT_128K,
            tool_calling: Parallel,
            computer_use: true,
            code_execution: false,
            price: (5.0, 25.0, 0.50),
            lifecycle: Deprecated,
        }),
        // models/opus-4-6/overview: „Legacy“, 1M / 128K, $5/$25, Cache $0.50.
        claude(ClaudeSpec {
            model: "claude-opus-4-6",
            context_window: CTX_1M,
            max_output_tokens: OUT_128K,
            tool_calling: Parallel,
            computer_use: true,
            code_execution: false,
            price: (5.0, 25.0, 0.50),
            lifecycle: Deprecated,
        }),
        // models/sonnet-4-6/overview: „Legacy“, 1M / 128K, $3/$15, Cache $0.30.
        claude(ClaudeSpec {
            model: "claude-sonnet-4-6",
            context_window: CTX_1M,
            max_output_tokens: OUT_128K,
            tool_calling: Parallel,
            computer_use: false,
            code_execution: false,
            price: (3.0, 15.0, 0.30),
            lifecycle: Deprecated,
        }),
        // models/sonnet-4-5/overview: „Legacy“, 200K / 64K, $3/$15, Cache $0.30.
        claude(ClaudeSpec {
            model: "claude-sonnet-4-5-20250929",
            context_window: CTX_200K,
            max_output_tokens: OUT_64K,
            tool_calling: Parallel,
            computer_use: false,
            code_execution: false,
            price: (3.0, 15.0, 0.30),
            lifecycle: Deprecated,
        }),
        // models/opus-4-5/overview: „Legacy“, 200K / 64K, $5/$25, Cache $0.50.
        claude(ClaudeSpec {
            model: "claude-opus-4-5-20251101",
            context_window: CTX_200K,
            max_output_tokens: OUT_64K,
            tool_calling: Parallel,
            computer_use: true,
            code_execution: false,
            price: (5.0, 25.0, 0.50),
            lifecycle: Deprecated,
        }),
        // ── Zurückgezogen (Claude API) ────────────────────────────────────────
        // model-deprecations: Retired 2026-08-05 („retired, except on Bedrock
        // and Google Cloud“); Pricing: $15/$75, Cache $1.50. Max-Output aus
        // docs/research/models/anthropic.json (Doku führt keinen Wert mehr).
        claude(ClaudeSpec {
            model: "claude-opus-4-1-20250805",
            context_window: CTX_200K,
            max_output_tokens: Some(32_768),
            tool_calling: Parallel,
            computer_use: false,
            code_execution: false,
            price: (15.0, 75.0, 1.50),
            lifecycle: Retired,
        }),
    ]
}

/// Returns bootstrap [`ObservedModelBehavior`] entries for all Anthropic descriptors.
///
/// # Description
/// Derives one bootstrap `ObservedModelBehavior` per descriptor from
/// [`anthropic_descriptors`] using `ObservedModelBehavior::bootstrap`.
/// All scores are `Score::HALF`, `updated_at` is `None`, and `evidence` is empty
/// (§5, model-catalog-v2.md).
///
/// # Returns
/// `Vec<ObservedModelBehavior>` with one entry per descriptor.
///
/// # Concurrency
/// Pure, no side effects, no locks.
///
/// # Examples
/// ```rust,no_run
/// use harw_model_catalog::vendor_anthropic::anthropic_observations;
/// use harw_model_catalog::observed::Score;
/// let obs = anthropic_observations();
/// assert!(obs.iter().all(|o| o.tool_schema_reliability == Score::HALF));
/// ```
pub fn anthropic_observations() -> Vec<ObservedModelBehavior> {
    crate::observations_from_descriptors(&anthropic_descriptors())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::{
        Modality, ModelLifecycle, Pricing, ReasoningSupport, ToolCallingSupport,
    };
    use crate::observed::Score;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn test_anthropic_descriptors_required_models_present() {
        // Alle aktuellen, Einladungs- und Legacy-Modelle der Anthropic-Doku
        // (Stand 2026-09-24) müssen im Katalog stehen.
        let descriptors = anthropic_descriptors();
        let ids: Vec<&str> = descriptors.iter().map(|d| d.model.as_str()).collect();
        for required in &[
            "claude-fable-5-1",
            "claude-opus-5-5",
            "claude-sonnet-5",
            "claude-haiku-4-5-20251001",
            "claude-mythos-5-1",
            "claude-fable-5",
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-opus-4-5-20251101",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5-20250929",
        ] {
            assert!(
                ids.contains(required),
                "Required model '{}' not found in anthropic_descriptors()",
                required
            );
        }
    }

    #[test]
    fn test_anthropic_descriptors_provider_is_anthropic() {
        // Every descriptor must have provider == "anthropic".
        for d in anthropic_descriptors() {
            assert_eq!(
                d.provider, "anthropic",
                "Unexpected provider '{}' for model '{}'",
                d.provider, d.model
            );
        }
    }

    #[test]
    fn test_anthropic_descriptors_all_have_text_modality() {
        // Every descriptor must include Modality::Text.
        for d in anthropic_descriptors() {
            assert!(
                d.modalities.contains(Modality::Text),
                "Modality::Text missing for {}",
                d.model
            );
        }
    }

    #[test]
    fn test_anthropic_descriptors_image_models_have_image_modality() {
        // image_input=true implies Modality::Image in modalities.
        for d in anthropic_descriptors() {
            if d.capabilities.image_input {
                assert!(
                    d.modalities.contains(Modality::Image),
                    "image_input=true but Modality::Image missing for {}",
                    d.model
                );
            }
        }
    }

    #[test]
    fn test_anthropic_descriptors_all_context_windows_positive() {
        // Every context_window must be greater than zero.
        for d in anthropic_descriptors() {
            assert!(d.context_window > 0, "context_window == 0 for {}", d.model);
        }
    }

    #[test]
    fn test_anthropic_descriptors_no_empty_model_ids() {
        // Neither provider nor model may be empty.
        for d in anthropic_descriptors() {
            assert!(!d.model.is_empty(), "Empty model ID found: {:?}", d);
        }
    }

    #[test]
    fn test_anthropic_descriptors_legacy_models_are_deprecated() -> TestResult {
        // Alle „Legacy (still available)“-Modelle der Doku tragen Deprecated.
        let deprecated_ids = [
            "claude-fable-5",
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5-20250929",
            "claude-opus-4-5-20251101",
        ];
        let map: std::collections::HashMap<ModelId, ModelLifecycle> = anthropic_descriptors()
            .into_iter()
            .map(|d| (d.model, d.lifecycle))
            .collect();
        for id in &deprecated_ids {
            let lifecycle = map
                .get(*id)
                .ok_or_else(|| TestError::Unexpected(format!("Legacy model '{}' not found", id)))?;
            assert_eq!(
                *lifecycle,
                ModelLifecycle::Deprecated,
                "Legacy model '{}' should be Deprecated, got {:?}",
                id,
                lifecycle
            );
        }
        Ok(())
    }

    #[test]
    fn test_anthropic_descriptors_opus_series_computer_use() {
        // Opus-series GA models must have computer_use = true.
        let ga_opus: Vec<_> = anthropic_descriptors()
            .into_iter()
            .filter(|d| d.lifecycle == ModelLifecycle::Ga && d.model.contains("opus"))
            .collect();
        assert!(
            !ga_opus.is_empty(),
            "No GA Opus models found — test precondition failed"
        );
        for d in ga_opus {
            assert!(
                d.capabilities.native_agent_features.computer_use,
                "GA Opus model '{}' must have computer_use=true",
                d.model
            );
        }
    }

    #[test]
    fn test_anthropic_descriptors_current_models_native_tool_calling() -> TestResult {
        // GA Fable/Opus/Sonnet/Haiku models must use Native tool calling.
        let native_models = [
            "claude-fable-5-1",
            "claude-opus-5-5",
            "claude-sonnet-5",
            "claude-haiku-4-5-20251001",
        ];
        let map: std::collections::HashMap<ModelId, ToolCallingSupport> = anthropic_descriptors()
            .into_iter()
            .map(|d| (d.model, d.capabilities.tool_calling))
            .collect();
        for id in &native_models {
            let tc = map
                .get(*id)
                .ok_or_else(|| TestError::Unexpected(format!("Model '{}' not found", id)))?;
            assert_eq!(
                *tc,
                ToolCallingSupport::Native,
                "Model '{}' should have Native tool calling",
                id
            );
        }
        Ok(())
    }

    /// Sucht einen Deskriptor per ID oder liefert einen Testfehler.
    fn descriptor(id: &str) -> Result<ModelDescriptor, TestError> {
        anthropic_descriptors()
            .into_iter()
            .find(|d| d.model == id)
            .ok_or_else(|| TestError::Unexpected(format!("Model '{id}' not found")))
    }

    #[test]
    fn test_anthropic_descriptors_ga_set_is_current_lineup() {
        // Genau die vier Modelle der Vergleichstabelle sind GA.
        let mut ga: Vec<String> = anthropic_descriptors()
            .into_iter()
            .filter(|d| d.lifecycle == ModelLifecycle::Ga)
            .map(|d| d.model.as_str().to_owned())
            .collect();
        ga.sort();
        assert_eq!(
            ga,
            vec![
                "claude-fable-5-1",
                "claude-haiku-4-5-20251001",
                "claude-opus-5-5",
                "claude-sonnet-5",
            ]
        );
    }

    #[test]
    fn test_anthropic_descriptors_current_models_come_first() {
        // Reihenfolge aktuell → Einladung → Legacy → Retired.
        let rank = |l: ModelLifecycle| match l {
            ModelLifecycle::Ga => 0,
            ModelLifecycle::Preview => 1,
            ModelLifecycle::Deprecated => 2,
            ModelLifecycle::Retired => 3,
        };
        let ranks: Vec<u8> = anthropic_descriptors()
            .into_iter()
            .map(|d| rank(d.lifecycle))
            .collect();
        assert!(ranks.windows(2).all(|w| w[0] <= w[1]), "{ranks:?}");
    }

    #[test]
    fn test_opus_5_5_limits_and_pricing() -> TestResult {
        let d = descriptor("claude-opus-5-5")?;
        assert_eq!(d.context_window, 1_000_000);
        assert_eq!(d.max_output_tokens, Some(128_000));
        assert_eq!(d.capabilities.reasoning, ReasoningSupport::Effort);
        assert_eq!(d.lifecycle, ModelLifecycle::Ga);
        assert_eq!(
            d.pricing,
            Some(Pricing {
                input_per_mtoken_usd: 4.0,
                output_per_mtoken_usd: 20.0,
                cached_input_per_mtoken_usd: Some(0.20),
            })
        );
        Ok(())
    }

    #[test]
    fn test_fable_5_1_limits_and_pricing() -> TestResult {
        let d = descriptor("claude-fable-5-1")?;
        assert_eq!(d.context_window, 1_000_000);
        assert_eq!(d.max_output_tokens, Some(128_000));
        assert_eq!(d.capabilities.reasoning, ReasoningSupport::Effort);
        assert_eq!(d.lifecycle, ModelLifecycle::Ga);
        assert_eq!(
            d.pricing,
            Some(Pricing {
                input_per_mtoken_usd: 10.0,
                output_per_mtoken_usd: 50.0,
                cached_input_per_mtoken_usd: Some(0.25),
            })
        );
        Ok(())
    }

    #[test]
    fn test_mythos_5_1_is_invite_only_with_fable_5_1_specs() -> TestResult {
        let mythos = descriptor("claude-mythos-5-1")?;
        let fable = descriptor("claude-fable-5-1")?;
        assert_eq!(mythos.lifecycle, ModelLifecycle::Preview);
        assert_eq!(mythos.context_window, fable.context_window);
        assert_eq!(mythos.max_output_tokens, fable.max_output_tokens);
        assert_eq!(mythos.pricing, fable.pricing);
        Ok(())
    }

    #[test]
    fn test_opus_4_1_is_retired() -> TestResult {
        assert_eq!(
            descriptor("claude-opus-4-1-20250805")?.lifecycle,
            ModelLifecycle::Retired
        );
        Ok(())
    }

    #[test]
    fn test_all_anthropic_descriptors_have_pricing_with_cache_read_below_input() -> TestResult {
        for d in anthropic_descriptors() {
            let p = d
                .pricing
                .ok_or_else(|| TestError::Unexpected(format!("pricing missing for {}", d.model)))?;
            let cache = p.cached_input_per_mtoken_usd.ok_or_else(|| {
                TestError::Unexpected(format!("cache price missing for {}", d.model))
            })?;
            assert!(cache < p.input_per_mtoken_usd, "{}", d.model);
            assert!(
                p.input_per_mtoken_usd < p.output_per_mtoken_usd,
                "{}",
                d.model
            );
        }
        Ok(())
    }

    #[test]
    fn test_anthropic_observations_count_matches_descriptors() {
        // Observation count must equal descriptor count.
        assert_eq!(
            anthropic_observations().len(),
            anthropic_descriptors().len(),
            "anthropic_observations() count must equal anthropic_descriptors() count"
        );
    }

    #[test]
    fn test_anthropic_observations_all_bootstrap_defaults() {
        // All observations must have Score::HALF, no updated_at, no evidence.
        for obs in anthropic_observations() {
            assert_eq!(
                obs.tool_schema_reliability,
                Score::HALF,
                "model: {}",
                obs.model
            );
            assert_eq!(
                obs.long_context_retention,
                Score::HALF,
                "model: {}",
                obs.model
            );
            assert_eq!(
                obs.delegation_discipline,
                Score::HALF,
                "model: {}",
                obs.model
            );
            assert_eq!(
                obs.recovery_after_tool_error,
                Score::HALF,
                "model: {}",
                obs.model
            );
            assert_eq!(
                obs.completion_calibration,
                Score::HALF,
                "model: {}",
                obs.model
            );
            assert_eq!(
                obs.compaction_resilience,
                Score::HALF,
                "model: {}",
                obs.model
            );
            assert!(obs.updated_at.is_none(), "model: {}", obs.model);
            assert!(obs.evidence.is_empty(), "model: {}", obs.model);
        }
    }

    #[test]
    fn test_anthropic_observations_no_duplicate_model_ids() {
        // No two observations may have the same model ID.
        let obs = anthropic_observations();
        let mut seen = std::collections::HashSet::new();
        for o in &obs {
            assert!(
                seen.insert(o.model.clone()),
                "Duplicate model ID in observations: {}",
                o.model
            );
        }
    }

    #[test]
    fn test_anthropic_observations_provider_is_anthropic() {
        // Every observation must have provider == "anthropic".
        for obs in anthropic_observations() {
            assert_eq!(
                obs.provider, "anthropic",
                "Expected provider 'anthropic', got '{}' for model '{}'",
                obs.provider, obs.model
            );
        }
    }
}
