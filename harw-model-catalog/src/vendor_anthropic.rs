//! Vendor-specific model descriptors and observed behaviors for Anthropic Claude models.
//!
//! Implements the Anthropic slice of the Layer 2 / Layer 4 catalog
//! (see `docs/design/model-catalog-v2.md`, §3 and §5). All values are
//! sourced from `docs/research/models/anthropic.json` (retrieved 2026-07-16).
//!
//! ## Verantwortung
//! - Curates [`ModelDescriptor`] entries for every Anthropic model, including
//!   legacy records with `lifecycle: Deprecated`.
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
//! assert!(catalog.iter().any(|d| d.model == "claude-opus-4-8"));
//! ```

use crate::descriptor::{
    AgentFeatureSet, Modality, ModalitySet, ModelCapabilities, ModelDescriptor, ModelId,
    ModelLifecycle, PromptCachingSupport, ProviderId, ReasoningSupport, StreamingSupport,
    StructuredOutputSupport, ToolCallingSupport,
};
use crate::observed::ObservedModelBehavior;

/// Returns curated [`ModelDescriptor`] entries for all known Anthropic Claude models.
///
/// # Description
/// Covers generally-available, preview, and legacy (deprecated) models sourced
/// from `docs/research/models/anthropic.json`. Legacy records are included with
/// `lifecycle: Deprecated` as required by the spec.
///
/// Mapping rules applied (from JSON → Rust enum variants):
/// - `status: "ga"` → `ModelLifecycle::Ga`
/// - `status: "limited_availability"` → `ModelLifecycle::Preview`
/// - `status: "legacy" | "deprecated"` → `ModelLifecycle::Deprecated`
/// - `tool_use_native: true` → `ToolCallingSupport::Native`, `parallel_tools: true`
/// - `tool_use_parallel: true` (no native) → `ToolCallingSupport::Parallel`, `parallel_tools: true`
/// - `extended_thinking | adaptive_thinking | effort_parameter: true` → `ReasoningSupport::Effort`
/// - `prompt_caching: true` → `PromptCachingSupport::Explicit` (Anthropic requires explicit breakpoints)
/// - `streaming: true` → `StreamingSupport::ServerSent`
/// - `vision: true` → `image_input: true` + `Modality::Image` in `modalities`
/// - `computer_use: true` → `native_agent_features.computer_use = true`
/// - `code_execution: true` → `native_agent_features.code_execution = true`
///
/// Opus-series models have `computer_use: true` per native feature documentation.
///
/// # Returns
/// `Vec<ModelDescriptor>` with 11 entries (5 current + 6 legacy).
///
/// # Concurrency
/// Pure, no side effects, no locks.
///
/// # Examples
/// ```rust,no_run
/// use harw_model_catalog::vendor_anthropic::anthropic_descriptors;
/// let catalog = anthropic_descriptors();
/// assert!(catalog.iter().any(|d| d.model == "claude-fable-5"));
/// ```
pub fn anthropic_descriptors() -> Vec<ModelDescriptor> {
    vec![
        // ── Current GA / Preview models ───────────────────────────────────────
        // Source: docs/research/models/anthropic.json, retrieved 2026-07-16
        ModelDescriptor {
            provider: ProviderId::from("anthropic"),
            model: ModelId::from("claude-fable-5"),
            context_window: 1_000_000,
            max_output_tokens: Some(131_072),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Native,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Explicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet {
                    computer_use: true,
                    code_execution: true,
                    built_in_search: false,
                    file_search: false,
                },
            },
            pricing: None,
            lifecycle: ModelLifecycle::Ga,
        },
        // Source: docs/research/models/anthropic.json — status: limited_availability
        ModelDescriptor {
            provider: ProviderId::from("anthropic"),
            model: ModelId::from("claude-mythos-5"),
            context_window: 1_000_000,
            max_output_tokens: Some(131_072),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Native,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Explicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet {
                    computer_use: true,
                    code_execution: true,
                    built_in_search: false,
                    file_search: false,
                },
            },
            pricing: None,
            lifecycle: ModelLifecycle::Preview,
        },
        // Source: docs/research/models/anthropic.json — status: ga, ga_date: 2026-05-28
        ModelDescriptor {
            provider: ProviderId::from("anthropic"),
            model: ModelId::from("claude-opus-4-8"),
            context_window: 1_000_000,
            max_output_tokens: Some(131_072),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Native,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Explicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet {
                    computer_use: true,
                    code_execution: true,
                    built_in_search: false,
                    file_search: false,
                },
            },
            pricing: None,
            lifecycle: ModelLifecycle::Ga,
        },
        // Source: docs/research/models/anthropic.json — status: ga, ga_date: 2026-06-30
        ModelDescriptor {
            provider: ProviderId::from("anthropic"),
            model: ModelId::from("claude-sonnet-5"),
            context_window: 1_000_000,
            max_output_tokens: Some(131_072),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Native,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Explicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet {
                    computer_use: true,
                    code_execution: true,
                    built_in_search: false,
                    file_search: false,
                },
            },
            pricing: None,
            lifecycle: ModelLifecycle::Ga,
        },
        // Source: docs/research/models/anthropic.json — status: ga, ga_date: 2025-10-15
        ModelDescriptor {
            provider: ProviderId::from("anthropic"),
            model: ModelId::from("claude-haiku-4-5-20251001"),
            context_window: 200_000,
            max_output_tokens: Some(65_536),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Native,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Explicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet {
                    computer_use: true,
                    code_execution: true,
                    built_in_search: false,
                    file_search: false,
                },
            },
            pricing: None,
            lifecycle: ModelLifecycle::Ga,
        },
        // ── Legacy / Deprecated models ────────────────────────────────────────
        // Source: docs/research/models/anthropic.json — status: legacy
        ModelDescriptor {
            provider: ProviderId::from("anthropic"),
            model: ModelId::from("claude-opus-4-7"),
            context_window: 1_000_000,
            max_output_tokens: Some(131_072),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Explicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet {
                    computer_use: true,
                    code_execution: false,
                    built_in_search: false,
                    file_search: false,
                },
            },
            pricing: None,
            lifecycle: ModelLifecycle::Deprecated,
        },
        // Source: docs/research/models/anthropic.json — status: legacy
        ModelDescriptor {
            provider: ProviderId::from("anthropic"),
            model: ModelId::from("claude-opus-4-6"),
            context_window: 1_000_000,
            max_output_tokens: Some(131_072),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Explicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet {
                    computer_use: true,
                    code_execution: false,
                    built_in_search: false,
                    file_search: false,
                },
            },
            pricing: None,
            lifecycle: ModelLifecycle::Deprecated,
        },
        // Source: docs/research/models/anthropic.json — status: legacy
        ModelDescriptor {
            provider: ProviderId::from("anthropic"),
            model: ModelId::from("claude-sonnet-4-6"),
            context_window: 1_000_000,
            max_output_tokens: Some(131_072),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Explicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet {
                    computer_use: false,
                    code_execution: false,
                    built_in_search: false,
                    file_search: false,
                },
            },
            pricing: None,
            lifecycle: ModelLifecycle::Deprecated,
        },
        // Source: docs/research/models/anthropic.json — status: legacy
        ModelDescriptor {
            provider: ProviderId::from("anthropic"),
            model: ModelId::from("claude-sonnet-4-5-20250929"),
            context_window: 200_000,
            max_output_tokens: Some(65_536),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Explicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet {
                    computer_use: false,
                    code_execution: false,
                    built_in_search: false,
                    file_search: false,
                },
            },
            pricing: None,
            lifecycle: ModelLifecycle::Deprecated,
        },
        // Source: docs/research/models/anthropic.json — status: legacy
        ModelDescriptor {
            provider: ProviderId::from("anthropic"),
            model: ModelId::from("claude-opus-4-5-20251101"),
            context_window: 200_000,
            max_output_tokens: Some(65_536),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Explicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet {
                    computer_use: true,
                    code_execution: false,
                    built_in_search: false,
                    file_search: false,
                },
            },
            pricing: None,
            lifecycle: ModelLifecycle::Deprecated,
        },
        // Source: docs/research/models/anthropic.json — status: deprecated, retirement_date: 2026-08-05
        ModelDescriptor {
            provider: ProviderId::from("anthropic"),
            model: ModelId::from("claude-opus-4-1-20250805"),
            context_window: 200_000,
            max_output_tokens: Some(32_768),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Explicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet {
                    computer_use: false,
                    code_execution: false,
                    built_in_search: false,
                    file_search: false,
                },
            },
            pricing: None,
            lifecycle: ModelLifecycle::Deprecated,
        },
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
    use crate::descriptor::{Modality, ModelLifecycle, ToolCallingSupport};
    use crate::observed::Score;

    #[test]
    fn test_anthropic_descriptors_required_models_present() {
        // Asserts the four mandated model IDs are present (spec requirement).
        let descriptors = anthropic_descriptors();
        let ids: Vec<&str> = descriptors.iter().map(|d| d.model.as_str()).collect();
        for required in &[
            "claude-opus-4-8",
            "claude-sonnet-5",
            "claude-haiku-4-5-20251001",
            "claude-fable-5",
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
    fn test_anthropic_descriptors_legacy_models_are_deprecated() {
        // All legacy/deprecated records from the JSON must carry Deprecated lifecycle.
        let deprecated_ids = [
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5-20250929",
            "claude-opus-4-5-20251101",
            "claude-opus-4-1-20250805",
        ];
        let map: std::collections::HashMap<ModelId, ModelLifecycle> = anthropic_descriptors()
            .into_iter()
            .map(|d| (d.model, d.lifecycle))
            .collect();
        for id in &deprecated_ids {
            let lifecycle = map
                .get(*id)
                .unwrap_or_else(|| panic!("Legacy model '{}' not found", id));
            assert_eq!(
                *lifecycle,
                ModelLifecycle::Deprecated,
                "Legacy model '{}' should be Deprecated, got {:?}",
                id,
                lifecycle
            );
        }
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
    fn test_anthropic_descriptors_current_models_native_tool_calling() {
        // GA Fable/Opus/Sonnet/Haiku models must use Native tool calling.
        let native_models = [
            "claude-fable-5",
            "claude-opus-4-8",
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
                .unwrap_or_else(|| panic!("Model '{}' not found", id));
            assert_eq!(
                *tc,
                ToolCallingSupport::Native,
                "Model '{}' should have Native tool calling",
                id
            );
        }
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
