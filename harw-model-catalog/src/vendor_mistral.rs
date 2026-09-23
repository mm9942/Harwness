//! Mistral AI vendor module — Layer-2 descriptors and Layer-4 observations.
//!
//! Spec source: `docs/research/models/mistral.json` (retrieved 2026-07-16).
//! Primary source: <https://docs.mistral.ai/getting-started/models/models_overview/>
//!
//! ## Verantwortung
//! - Liefert [`mistral_descriptors`] (Layer 2): deklarierte Fähigkeiten für alle
//!   aktiven und deprecated Mistral-Modelle per research-date.
//! - Liefert [`mistral_observations`] (Layer 4): konservative Bootstrap-Einträge
//!   (alle `Score::HALF`, `updated_at = None`) für denselben Modellsatz.
//! - `provider`-Feld ist immer `"mistral"` in beiden Listen.
//!
//! ## Wichtigste Typen
//! - [`ModelDescriptor`] (re-export aus `descriptor`)
//! - [`ObservedModelBehavior`] (re-export aus `observed`)
//!
//! ## Nebenläufigkeit
//! Alle Rückgabetypen sind `Clone + Send + Sync`; keine Locks, keine Threads.
//!
//! ## Fehlertypen
//! Infallibel; alle Konstruktoren sind deterministisch.
//!
//! ## Mapping-Regeln
//! | JSON-Feld                         | Descriptor-Feld / Enum-Variant                                     |
//! |-----------------------------------|--------------------------------------------------------------------|
//! | `capabilities.tool_calling=true, parallel=true`  | `ToolCallingSupport::Parallel`          |
//! | `capabilities.tool_calling=true, parallel=false`  | `ToolCallingSupport::Basic`             |
//! | `capabilities.structured_output="json_schema"` | `StructuredOutputSupport::JsonSchema` |
//! | `capabilities.structured_output="json_mode"`   | `StructuredOutputSupport::JsonMode`   |
//! | `capabilities.reasoning=true, effort_param=true` | `ReasoningSupport::Effort`            |
//! | `capabilities.reasoning=true, effort_param=false`| `ReasoningSupport::Trace`             |
//! | `capabilities.reasoning=false`                  | `ReasoningSupport::None`              |
//! | `capabilities.prompt_caching=true`              | `PromptCachingSupport::Implicit`      |
//! | `capabilities.prompt_caching=false`             | `PromptCachingSupport::None`          |
//! | `capabilities.streaming=true`                   | `StreamingSupport::ServerSent`        |
//! | `lifecycle="ga"`                                | `ModelLifecycle::Ga`                  |
//! | `lifecycle="deprecated"`                        | `ModelLifecycle::Deprecated`          |
//! | `pricing_usd_per_1m.*`                          | `Pricing` struct                      |
//!
//! ## Beispiel
//! ```rust,no_run
//! use harw_model_catalog::vendor_mistral::{mistral_descriptors, mistral_observations};
//! let descs = mistral_descriptors();
//! let obs   = mistral_observations();
//! assert!(descs.len() >= 10);
//! assert_eq!(descs.len(), obs.len());
//! ```

use crate::descriptor::{
    AgentFeatureSet, Modality, ModalitySet, ModelCapabilities, ModelDescriptor, ModelId,
    ModelLifecycle, Pricing, PromptCachingSupport, ProviderId, ReasoningSupport, StreamingSupport,
    StructuredOutputSupport, ToolCallingSupport,
};
use crate::observed::ObservedModelBehavior;

// ─────────────────────────────────────────────────────────────────────────────
// Internal helper
// ─────────────────────────────────────────────────────────────────────────────

/// Constructs a `ModelDescriptor` for the Mistral provider.
///
/// # Description
/// Centralises field assembly so individual model entries stay brief.
/// All `provider` strings are hardcoded to `"mistral"` (§ spec).
///
/// # Arguments
/// - `model` (`&str`): canonical API snapshot ID (e.g. `"mistral-large-2512"`).
/// - `context_window` (`u32`): declared max context in tokens.
/// - `max_output_tokens` (`Option<u32>`): declared max output, if published.
/// - `modalities` (`ModalitySet`): supported input modalities.
/// - `capabilities` (`ModelCapabilities`): declared technical capabilities.
/// - `pricing` (`Option<Pricing>`): indicative pricing hint (not authoritative).
/// - `lifecycle` (`ModelLifecycle`): current lifecycle status.
///
/// # Returns
/// A fully populated `ModelDescriptor` with `provider = "mistral"`.
///
/// # Concurrency
/// Pure function; no side effects.
fn md(
    model: &str,
    context_window: u32,
    max_output_tokens: Option<u32>,
    modalities: ModalitySet,
    capabilities: ModelCapabilities,
    pricing: Option<Pricing>,
    lifecycle: ModelLifecycle,
) -> ModelDescriptor {
    ModelDescriptor {
        provider: ProviderId::from("mistral"),
        model: ModelId::from(model),
        context_window,
        max_output_tokens,
        modalities,
        capabilities,
        pricing,
        lifecycle,
    }
}

/// Shorthand `ModalitySet` for text-only models.
///
/// # Returns
/// `ModalitySet` containing only `Modality::Text`.
fn text_only() -> ModalitySet {
    ModalitySet::new(vec![Modality::Text])
}

/// Shorthand `ModalitySet` for multimodal (text + image) models.
///
/// # Returns
/// `ModalitySet` containing `Modality::Text` and `Modality::Image`.
fn text_image() -> ModalitySet {
    ModalitySet::new(vec![Modality::Text, Modality::Image])
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Returns Layer-2 `ModelDescriptor` entries for all Mistral AI models.
///
/// # Description
/// Covers the full set of active (`Ga`) and deprecated (`Deprecated`) Mistral
/// snapshot IDs listed in `docs/research/models/mistral.json` as of 2026-07-16.
/// Legacy entries (fully `Retired` prior to the research date) are excluded.
///
/// Capabilities are mapped from the JSON per the module-level mapping table.
/// `pricing` is populated as a non-authoritative hint; authoritative prices
/// live in `harw-provider`.
///
/// # Returns
/// `Vec<ModelDescriptor>` with one entry per Mistral snapshot ID. Length >= 10.
///
/// # Concurrency
/// Pure function; no side effects.
///
/// # Examples
/// ```rust,no_run
/// use harw_model_catalog::vendor_mistral::mistral_descriptors;
/// let descs = mistral_descriptors();
/// assert!(descs.iter().any(|d| d.model == "mistral-large-2512"));
/// assert!(descs.iter().any(|d| d.model == "mistral-medium-2604"));
/// assert!(descs.iter().any(|d| d.model == "codestral-2508"));
/// ```
pub fn mistral_descriptors() -> Vec<ModelDescriptor> {
    vec![
        // ── mistral-medium-2604 ── (Mistral Medium 3.5, GA, Open, 2026-04-28)
        // Source: docs.mistral.ai/models/model-cards/mistral-medium-3-5-26-04
        md(
            "mistral-medium-2604",
            262_144,
            None,
            text_image(),
            ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet::default(),
            },
            Some(Pricing {
                input_per_mtoken_usd: 1.50,
                output_per_mtoken_usd: 7.50,
                cached_input_per_mtoken_usd: Some(0.15),
            }),
            ModelLifecycle::Ga,
        ),
        // ── mistral-small-2603 ── (Mistral Small 4, GA, Open, 2026-03-16)
        // Source: docs.mistral.ai/models/model-cards/mistral-small-4-0-26-03
        md(
            "mistral-small-2603",
            262_144,
            Some(262_144),
            text_image(),
            ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet::default(),
            },
            Some(Pricing {
                input_per_mtoken_usd: 0.15,
                output_per_mtoken_usd: 0.60,
                cached_input_per_mtoken_usd: Some(0.015),
            }),
            ModelLifecycle::Ga,
        ),
        // ── mistral-large-2512 ── (Mistral Large 3, GA, Open, 2025-12-02)
        // Source: docs.mistral.ai/models/model-cards/mistral-large-3-25-12
        md(
            "mistral-large-2512",
            262_144,
            Some(4_096),
            text_image(),
            ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet::default(),
            },
            Some(Pricing {
                input_per_mtoken_usd: 0.50,
                output_per_mtoken_usd: 1.50,
                cached_input_per_mtoken_usd: Some(0.05),
            }),
            ModelLifecycle::Ga,
        ),
        // ── magistral-medium-2509 ── (Magistral Medium 1.2, Deprecated, Premier)
        // deprecated_at: 2026-05-22; retired_at: 2026-07-31
        // Source: docs.mistral.ai (changelog + model overview)
        md(
            "magistral-medium-2509",
            131_072,
            Some(40_000),
            text_only(),
            ModelCapabilities {
                tool_calling: ToolCallingSupport::Basic,
                parallel_tools: false,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Trace,
                prompt_caching: PromptCachingSupport::None,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            Some(Pricing {
                input_per_mtoken_usd: 2.00,
                output_per_mtoken_usd: 5.00,
                cached_input_per_mtoken_usd: None,
            }),
            ModelLifecycle::Deprecated,
        ),
        // ── codestral-2508 ── (Codestral, GA, Premier, 2025-07-30)
        // Source: docs.mistral.ai/models/model-cards/codestral-25-08
        md(
            "codestral-2508",
            131_072,
            None,
            text_only(),
            ModelCapabilities {
                tool_calling: ToolCallingSupport::Basic,
                parallel_tools: false,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            Some(Pricing {
                input_per_mtoken_usd: 0.30,
                output_per_mtoken_usd: 0.90,
                cached_input_per_mtoken_usd: Some(0.03),
            }),
            ModelLifecycle::Ga,
        ),
        // ── devstral-2512 ── (Devstral 2, Deprecated, Open)
        // deprecated_at: 2026-05-22; retired_at: 2026-07-31
        md(
            "devstral-2512",
            262_144,
            None,
            text_only(),
            ModelCapabilities {
                tool_calling: ToolCallingSupport::Basic,
                parallel_tools: false,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::None,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            Some(Pricing {
                input_per_mtoken_usd: 0.40,
                output_per_mtoken_usd: 2.00,
                cached_input_per_mtoken_usd: None,
            }),
            ModelLifecycle::Deprecated,
        ),
        // ── ministral-14b-2512 ── (Ministral 3 14B, GA, Open, 2025-12-02)
        // Source: docs.mistral.ai/models/model-cards/ministral-3-14b-25-12
        md(
            "ministral-14b-2512",
            262_144,
            None,
            text_image(),
            ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet::default(),
            },
            Some(Pricing {
                input_per_mtoken_usd: 0.20,
                output_per_mtoken_usd: 0.20,
                cached_input_per_mtoken_usd: Some(0.02),
            }),
            ModelLifecycle::Ga,
        ),
        // ── ministral-8b-2512 ── (Ministral 3 8B, GA, Open, 2025-12-02)
        // Source: docs.mistral.ai/models/model-cards/ministral-3-8b-25-12
        md(
            "ministral-8b-2512",
            262_144,
            None,
            text_image(),
            ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet::default(),
            },
            Some(Pricing {
                input_per_mtoken_usd: 0.15,
                output_per_mtoken_usd: 0.15,
                cached_input_per_mtoken_usd: Some(0.015),
            }),
            ModelLifecycle::Ga,
        ),
        // ── ministral-3b-2512 ── (Ministral 3 3B, GA, Open, 2025-12-02)
        // Source: docs.mistral.ai/models/model-cards/ministral-3-3b-25-12
        md(
            "ministral-3b-2512",
            262_144,
            None,
            text_image(),
            ModelCapabilities {
                tool_calling: ToolCallingSupport::Basic,
                parallel_tools: false,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet::default(),
            },
            Some(Pricing {
                input_per_mtoken_usd: 0.10,
                output_per_mtoken_usd: 0.10,
                cached_input_per_mtoken_usd: Some(0.01),
            }),
            ModelLifecycle::Ga,
        ),
        // ── open-mistral-nemo-2407 ── (Mistral NeMo 12B, Deprecated, Open)
        // deprecated_at: 2026-05-22; retired_at: 2026-07-31
        md(
            "open-mistral-nemo-2407",
            131_072,
            Some(131_072),
            text_only(),
            ModelCapabilities {
                tool_calling: ToolCallingSupport::Basic,
                parallel_tools: false,
                structured_output: StructuredOutputSupport::JsonMode,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::None,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            Some(Pricing {
                input_per_mtoken_usd: 0.15,
                output_per_mtoken_usd: 0.15,
                cached_input_per_mtoken_usd: None,
            }),
            ModelLifecycle::Deprecated,
        ),
    ]
}

/// Returns Layer-4 `ObservedModelBehavior` bootstrap entries for all Mistral models.
///
/// # Description
/// One `ObservedModelBehavior::bootstrap("mistral", <model_id>)` entry per model
/// returned by [`mistral_descriptors`], in the same order. All scores are
/// `Score::HALF`, `updated_at = None`, `evidence = []` — no real Harness run
/// has been evaluated yet (§5 model-catalog-v2.md).
///
/// # Returns
/// `Vec<ObservedModelBehavior>` with `len() == mistral_descriptors().len()`.
///
/// # Concurrency
/// Pure function; no side effects.
///
/// # Examples
/// ```rust,no_run
/// use harw_model_catalog::vendor_mistral::{mistral_descriptors, mistral_observations};
/// let obs = mistral_observations();
/// assert!(obs.iter().any(|o| o.model == "mistral-large-2512"));
/// assert_eq!(obs.len(), mistral_descriptors().len());
/// ```
pub fn mistral_observations() -> Vec<ObservedModelBehavior> {
    mistral_descriptors()
        .iter()
        .map(|d| ObservedModelBehavior::bootstrap("mistral", &d.model))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::ModelLifecycle;
    use crate::observed::Score;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn test_mistral_descriptors_required_models_present() {
        // Asserts that the three models required by the task brief are included.
        let descs = mistral_descriptors();
        let ids: Vec<&str> = descs.iter().map(|d| d.model.as_str()).collect();
        assert!(
            ids.contains(&"mistral-medium-2604"),
            "mistral-medium-2604 missing from descriptors"
        );
        assert!(
            ids.contains(&"mistral-large-2512"),
            "mistral-large-2512 missing from descriptors"
        );
        assert!(
            ids.contains(&"codestral-2508"),
            "codestral-2508 missing from descriptors"
        );
    }

    #[test]
    fn test_mistral_descriptors_all_provider_mistral() {
        // Every descriptor must carry provider = "mistral".
        for d in mistral_descriptors() {
            assert_eq!(d.provider, "mistral", "Wrong provider for {}", d.model);
        }
    }

    #[test]
    fn test_mistral_descriptors_all_have_text_modality() {
        // Every model must support text input.
        for d in mistral_descriptors() {
            assert!(
                d.modalities.contains(Modality::Text),
                "Modality::Text missing for {}",
                d.model
            );
        }
    }

    #[test]
    fn test_mistral_descriptors_positive_context_window() {
        for d in mistral_descriptors() {
            assert!(d.context_window > 0, "context_window == 0 for {}", d.model);
        }
    }

    #[test]
    fn test_mistral_descriptors_image_models_have_image_modality() {
        // image_input=true must imply Modality::Image in the modality set.
        for d in mistral_descriptors() {
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
    fn test_mistral_descriptors_deprecated_models_have_correct_lifecycle() -> TestResult {
        // magistral-medium-2509, devstral-2512, open-mistral-nemo-2407 must be Deprecated.
        let deprecated_ids = [
            "magistral-medium-2509",
            "devstral-2512",
            "open-mistral-nemo-2407",
        ];
        let descs = mistral_descriptors();
        for id in &deprecated_ids {
            let entry = descs.iter().find(|d| d.model == *id).ok_or_else(|| {
                TestError::Unexpected(format!("Expected deprecated model {} not found", id))
            })?;
            assert_eq!(
                entry.lifecycle,
                ModelLifecycle::Deprecated,
                "Model {} should be Deprecated",
                id
            );
        }
        Ok(())
    }

    #[test]
    fn test_mistral_descriptors_ga_models_have_correct_lifecycle() -> TestResult {
        let ga_ids = [
            "mistral-medium-2604",
            "mistral-small-2603",
            "mistral-large-2512",
            "codestral-2508",
            "ministral-14b-2512",
            "ministral-8b-2512",
            "ministral-3b-2512",
        ];
        let descs = mistral_descriptors();
        for id in &ga_ids {
            let entry = descs.iter().find(|d| d.model == *id).ok_or_else(|| {
                TestError::Unexpected(format!("Expected GA model {} not found", id))
            })?;
            assert_eq!(
                entry.lifecycle,
                ModelLifecycle::Ga,
                "Model {} should be Ga",
                id
            );
        }
        Ok(())
    }

    #[test]
    fn test_mistral_descriptors_no_duplicate_ids() {
        let descs = mistral_descriptors();
        let mut seen = std::collections::HashSet::new();
        for d in &descs {
            assert!(
                seen.insert(d.model.as_str()),
                "Duplicate model id: {}",
                d.model
            );
        }
    }

    #[test]
    fn test_mistral_observations_required_models_present() {
        // Same three required models must appear in observations.
        let obs = mistral_observations();
        let ids: Vec<&str> = obs.iter().map(|o| o.model.as_str()).collect();
        assert!(ids.contains(&"mistral-medium-2604"));
        assert!(ids.contains(&"mistral-large-2512"));
        assert!(ids.contains(&"codestral-2508"));
    }

    #[test]
    fn test_mistral_observations_all_provider_mistral() {
        for o in mistral_observations() {
            assert_eq!(o.provider, "mistral", "Wrong provider for {}", o.model);
        }
    }

    #[test]
    fn test_mistral_observations_bootstrap_defaults() {
        // All observations must have HALF scores, no evidence, no timestamp.
        for o in mistral_observations() {
            assert_eq!(o.tool_schema_reliability, Score::HALF, "{}", o.model);
            assert_eq!(o.long_context_retention, Score::HALF, "{}", o.model);
            assert_eq!(o.delegation_discipline, Score::HALF, "{}", o.model);
            assert_eq!(o.recovery_after_tool_error, Score::HALF, "{}", o.model);
            assert_eq!(o.completion_calibration, Score::HALF, "{}", o.model);
            assert_eq!(o.compaction_resilience, Score::HALF, "{}", o.model);
            assert!(o.updated_at.is_none(), "{}", o.model);
            assert!(o.evidence.is_empty(), "{}", o.model);
        }
    }

    #[test]
    fn test_mistral_descriptors_and_observations_same_length() {
        assert_eq!(
            mistral_descriptors().len(),
            mistral_observations().len(),
            "descriptor and observation counts must match"
        );
    }

    #[test]
    fn test_mistral_observations_model_ids_match_descriptors() {
        let descs = mistral_descriptors();
        let obs = mistral_observations();
        for (d, o) in descs.iter().zip(obs.iter()) {
            assert_eq!(d.model, o.model, "Model ID mismatch at same position");
        }
    }

    #[test]
    fn test_mistral_large_2512_max_output_tokens() -> TestResult {
        // mistral-large-2512 declares 4096 max output per research data.
        let descs = mistral_descriptors();
        let large = descs
            .iter()
            .find(|d| d.model == "mistral-large-2512")
            .ok_or(TestError::Missing("mistral-large-2512 descriptor"))?;
        assert_eq!(large.max_output_tokens, Some(4_096));
        Ok(())
    }

    #[test]
    fn test_codestral_2508_no_vision() -> TestResult {
        // codestral-2508 is text-only.
        let descs = mistral_descriptors();
        let cs = descs
            .iter()
            .find(|d| d.model == "codestral-2508")
            .ok_or(TestError::Missing("codestral-2508 descriptor"))?;
        assert!(!cs.capabilities.image_input);
        assert!(!cs.modalities.contains(Modality::Image));
        Ok(())
    }

    #[test]
    fn test_mistral_medium_2604_reasoning_effort() -> TestResult {
        // mistral-medium-2604 supports reasoning with effort parameter.
        let descs = mistral_descriptors();
        let mm = descs
            .iter()
            .find(|d| d.model == "mistral-medium-2604")
            .ok_or(TestError::Missing("mistral-medium-2604 descriptor"))?;
        assert_eq!(mm.capabilities.reasoning, ReasoningSupport::Effort);
        Ok(())
    }
}
