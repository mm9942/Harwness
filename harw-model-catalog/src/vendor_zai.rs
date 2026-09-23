//! Vendor-Datei: Z.AI (Zhipu AI) — GLM-Modellfamilie.
//!
//! Spec-Quelle: `docs/research/models/zai.json`, abgerufen 2026-07-16.
//!
//! # Verantwortung
//! Dieses Modul besitzt ausschließlich die kuratierten Layer-2- und Layer-4-
//! Einträge für den Provider `"zai"`. Es enthält keine Provider-Registrierung,
//! keine Netzwerkzugriffe und keine Mutations-Logik.
//!
//! # Exportierte Funktionen
//! - [`zai_descriptors`] — Vec<[`ModelDescriptor`]> für alle aktiven GLM-Modelle.
//! - [`zai_observations`] — Vec<[`ObservedModelBehavior`]> (Bootstrap-Defaults).
//!
//! # Flagship
//! GLM-5.2 ist das aktuelle Flagship-Modell mit 1 000 000 Tokens Context-Window
//! (IndexShare sparse attention, 2.9× FLOP-Reduktion bei 1 M Kontext).
//!
//! # Nebenläufigkeitsmodell
//! Alle zurückgegebenen Typen sind `Clone + Send + Sync`. Keine Locks, keine
//! Threads; beide Funktionen sind rein deterministisch.
//!
//! # Fehlertypen
//! Dieses Modul ist infallibel. Alle Konstruktoren sind deterministisch.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_model_catalog::vendor_zai::{zai_descriptors, zai_observations};
//! let descs = zai_descriptors();
//! assert!(descs.iter().any(|d| d.model == "glm-5.2"));
//! let obs = zai_observations();
//! assert!(obs.iter().any(|o| o.model == "glm-5.2"));
//! ```

use crate::descriptor::{
    AgentFeatureSet, Modality, ModalitySet, ModelCapabilities, ModelDescriptor, ModelId,
    ModelLifecycle, Pricing, PromptCachingSupport, ProviderId, ReasoningSupport, StreamingSupport,
    StructuredOutputSupport, ToolCallingSupport,
};
use crate::observed::ObservedModelBehavior;

/// Erstellt die kuratierten Layer-2-Deskriptoren für alle aktiven Z.AI-Modelle.
///
/// # Beschreibung
/// Alle Werte basieren auf der Provider-Dokumentation (docs.z.ai, llmgateway.io),
/// Stand 2026-07-16. GLM-5.2 ist das Flagship mit 1 000 000 Token Context-Window.
/// Deaktivierte Modelle (`glm-4.7-flash-free`, `glm-4.6v-flash`, `glm-4.5-flash`)
/// sind nicht enthalten.
///
/// # Rückgabe
/// `Vec<ModelDescriptor>` mit 14 aktiven GLM-Einträgen.
///
/// # Concurrency
/// Rein deterministisch, keine Seiteneffekte. Sicher aus mehreren Threads aufrufbar.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::vendor_zai::zai_descriptors;
/// let descs = zai_descriptors();
/// assert!(descs.iter().any(|d| d.model == "glm-5.2"));
/// assert!(descs.iter().all(|d| d.provider == "zai"));
/// ```
pub fn zai_descriptors() -> Vec<ModelDescriptor> {
    vec![
        // ── GLM-5.2 ── Flagship; 1M-Token Context via IndexShare sparse attention.
        // Quelle: docs.z.ai/guides/llm/glm-5.2, llmgateway.io 2026-07-16.
        ModelDescriptor {
            provider: ProviderId::from("zai"),
            model: ModelId::from("glm-5.2"),
            context_window: 1_000_000,
            max_output_tokens: Some(131_072),
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet {
                    computer_use: false,
                    code_execution: false,
                    built_in_search: true,
                    file_search: false,
                },
            },
            pricing: Some(Pricing {
                input_per_mtoken_usd: 1.40,
                output_per_mtoken_usd: 4.40,
                cached_input_per_mtoken_usd: Some(0.26),
            }),
            lifecycle: ModelLifecycle::Ga,
        },
        // ── GLM-5.1 ── Long-horizon agentic flagship (prev); 200K context.
        // Quelle: docs.z.ai, llmgateway.io 2026-07-16.
        ModelDescriptor {
            provider: ProviderId::from("zai"),
            model: ModelId::from("glm-5.1"),
            context_window: 200_000,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet {
                    computer_use: false,
                    code_execution: false,
                    built_in_search: true,
                    file_search: false,
                },
            },
            pricing: Some(Pricing {
                input_per_mtoken_usd: 1.40,
                output_per_mtoken_usd: 4.40,
                cached_input_per_mtoken_usd: Some(0.26),
            }),
            lifecycle: ModelLifecycle::Ga,
        },
        // ── GLM-5 ── Deep reasoning, DeepSeek Sparse Attention; 202752-token context.
        // Quelle: docs.z.ai, llmgateway.io 2026-07-16.
        ModelDescriptor {
            provider: ProviderId::from("zai"),
            model: ModelId::from("glm-5"),
            context_window: 202_752,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet {
                    computer_use: false,
                    code_execution: false,
                    built_in_search: true,
                    file_search: false,
                },
            },
            pricing: Some(Pricing {
                input_per_mtoken_usd: 1.00,
                output_per_mtoken_usd: 3.20,
                cached_input_per_mtoken_usd: Some(0.20),
            }),
            lifecycle: ModelLifecycle::Ga,
        },
        // ── GLM-5-Turbo ── Fast inference for long agent chains; 203K context.
        // Quelle: docs.z.ai, llmgateway.io 2026-07-16.
        ModelDescriptor {
            provider: ProviderId::from("zai"),
            model: ModelId::from("glm-5-turbo"),
            context_window: 203_000,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::None,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: Some(Pricing {
                input_per_mtoken_usd: 1.20,
                output_per_mtoken_usd: 4.00,
                cached_input_per_mtoken_usd: None,
            }),
            lifecycle: ModelLifecycle::Ga,
        },
        // ── GLM-5V-Turbo ── First native multimodal agent model; image + video input.
        // Quelle: docs.z.ai, llmgateway.io 2026-07-16.
        ModelDescriptor {
            provider: ProviderId::from("zai"),
            model: ModelId::from("glm-5v-turbo"),
            context_window: 203_000,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image, Modality::Video]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::None,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: Some(Pricing {
                input_per_mtoken_usd: 1.20,
                output_per_mtoken_usd: 4.00,
                cached_input_per_mtoken_usd: None,
            }),
            lifecycle: ModelLifecycle::Ga,
        },
        // ── GLM-4.7 ── Enhanced coding + reasoning, agentic coding; 200K context.
        // Quelle: docs.z.ai, llmgateway.io 2026-07-16.
        ModelDescriptor {
            provider: ProviderId::from("zai"),
            model: ModelId::from("glm-4.7"),
            context_window: 200_000,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet {
                    computer_use: false,
                    code_execution: false,
                    built_in_search: true,
                    file_search: false,
                },
            },
            pricing: Some(Pricing {
                input_per_mtoken_usd: 0.60,
                output_per_mtoken_usd: 2.20,
                cached_input_per_mtoken_usd: Some(0.11),
            }),
            lifecycle: ModelLifecycle::Ga,
        },
        // ── GLM-4.7-Flash ── 30B-class free model; 203K context.
        // Quelle: docs.z.ai, llmgateway.io 2026-07-16.
        ModelDescriptor {
            provider: ProviderId::from("zai"),
            model: ModelId::from("glm-4.7-flash"),
            context_window: 203_000,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Basic,
                parallel_tools: false,
                structured_output: StructuredOutputSupport::JsonMode,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::None,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: Some(Pricing {
                input_per_mtoken_usd: 0.06,
                output_per_mtoken_usd: 0.40,
                cached_input_per_mtoken_usd: None,
            }),
            lifecycle: ModelLifecycle::Ga,
        },
        // ── GLM-4.7-FlashX ── Fast-accelerated Flash variant with reasoning support.
        // Quelle: docs.z.ai, llmgateway.io 2026-07-16.
        ModelDescriptor {
            provider: ProviderId::from("zai"),
            model: ModelId::from("glm-4.7-flashx"),
            context_window: 200_000,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: Some(Pricing {
                input_per_mtoken_usd: 0.07,
                output_per_mtoken_usd: 0.40,
                cached_input_per_mtoken_usd: Some(0.01),
            }),
            lifecycle: ModelLifecycle::Ga,
        },
        // ── GLM-4.6 ── Flagship coding model (China domestic release Sept 2025); 200K.
        // Quelle: docs.z.ai, bigmodel.cn, llmgateway.io 2026-07-16.
        ModelDescriptor {
            provider: ProviderId::from("zai"),
            model: ModelId::from("glm-4.6"),
            context_window: 200_000,
            max_output_tokens: Some(131_072),
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet {
                    computer_use: false,
                    code_execution: false,
                    built_in_search: true,
                    file_search: false,
                },
            },
            pricing: Some(Pricing {
                input_per_mtoken_usd: 0.60,
                output_per_mtoken_usd: 2.20,
                cached_input_per_mtoken_usd: Some(0.11),
            }),
            lifecycle: ModelLifecycle::Ga,
        },
        // ── GLM-4.6V ── Multimodal; native function calling + vision reasoning.
        // Quelle: docs.z.ai, bigmodel.cn, llmgateway.io 2026-07-16.
        ModelDescriptor {
            provider: ProviderId::from("zai"),
            model: ModelId::from("glm-4.6v"),
            context_window: 128_000,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Native,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: Some(Pricing {
                input_per_mtoken_usd: 0.30,
                output_per_mtoken_usd: 0.90,
                cached_input_per_mtoken_usd: Some(0.05),
            }),
            lifecycle: ModelLifecycle::Ga,
        },
        // ── GLM-4.5 ── Native agentic LLM; hybrid thinking/non-thinking; 128K.
        // Quelle: docs.z.ai, bigmodel.cn, llmgateway.io 2026-07-16.
        ModelDescriptor {
            provider: ProviderId::from("zai"),
            model: ModelId::from("glm-4.5"),
            context_window: 128_000,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: Some(Pricing {
                input_per_mtoken_usd: 0.60,
                output_per_mtoken_usd: 2.20,
                cached_input_per_mtoken_usd: Some(0.11),
            }),
            lifecycle: ModelLifecycle::Ga,
        },
        // ── GLM-4.5-Air ── Compact MoE (106B/12B active); agent-centric; 128K.
        // Quelle: docs.z.ai, bigmodel.cn, llmgateway.io 2026-07-16.
        ModelDescriptor {
            provider: ProviderId::from("zai"),
            model: ModelId::from("glm-4.5-air"),
            context_window: 128_000,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: Some(Pricing {
                input_per_mtoken_usd: 0.20,
                output_per_mtoken_usd: 1.10,
                cached_input_per_mtoken_usd: Some(0.03),
            }),
            lifecycle: ModelLifecycle::Ga,
        },
        // ── GLM-4.5V ── Vision reasoning; video understanding + GUI agent; 128K.
        // Quelle: docs.z.ai, openrouter.ai/z-ai, llmgateway.io 2026-07-16.
        ModelDescriptor {
            provider: ProviderId::from("zai"),
            model: ModelId::from("glm-4.5v"),
            context_window: 128_000,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image, Modality::Video]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: Some(Pricing {
                input_per_mtoken_usd: 0.60,
                output_per_mtoken_usd: 1.80,
                cached_input_per_mtoken_usd: Some(0.11),
            }),
            lifecycle: ModelLifecycle::Ga,
        },
        // ── GLM-4-32B ── 32B dense; cost-effective tool use + code; 128K.
        // Quelle: docs.z.ai, bigmodel.cn, llmgateway.io 2026-07-16.
        ModelDescriptor {
            provider: ProviderId::from("zai"),
            model: ModelId::from("glm-4-32b-0414-128k"),
            context_window: 128_000,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Basic,
                parallel_tools: false,
                structured_output: StructuredOutputSupport::JsonMode,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: Some(Pricing {
                input_per_mtoken_usd: 0.10,
                output_per_mtoken_usd: 0.10,
                cached_input_per_mtoken_usd: Some(0.00),
            }),
            lifecycle: ModelLifecycle::Ga,
        },
    ]
}

/// Erstellt Bootstrap-Observations für alle aktiven Z.AI-Modelle (Layer 4).
///
/// # Beschreibung
/// Gibt einen `Vec` mit je einem `ObservedModelBehavior::bootstrap`-Eintrag
/// pro Modell zurück. Alle Scores sind `Score::HALF`, `updated_at` ist `None`
/// und `evidence` ist leer — konservative Defaults ohne Harness-Evidenz.
/// Die Reihenfolge entspricht exakt der von [`zai_descriptors`].
///
/// # Rückgabe
/// `Vec<ObservedModelBehavior>` mit 14 Einträgen.
///
/// # Concurrency
/// Rein deterministisch, keine Seiteneffekte.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::vendor_zai::zai_observations;
/// use harw_model_catalog::observed::Score;
/// let obs = zai_observations();
/// assert_eq!(obs.len(), 14);
/// assert!(obs.iter().all(|o| o.tool_schema_reliability == Score::HALF));
/// ```
pub fn zai_observations() -> Vec<ObservedModelBehavior> {
    vec![
        ObservedModelBehavior::bootstrap("zai", "glm-5.2"),
        ObservedModelBehavior::bootstrap("zai", "glm-5.1"),
        ObservedModelBehavior::bootstrap("zai", "glm-5"),
        ObservedModelBehavior::bootstrap("zai", "glm-5-turbo"),
        ObservedModelBehavior::bootstrap("zai", "glm-5v-turbo"),
        ObservedModelBehavior::bootstrap("zai", "glm-4.7"),
        ObservedModelBehavior::bootstrap("zai", "glm-4.7-flash"),
        ObservedModelBehavior::bootstrap("zai", "glm-4.7-flashx"),
        ObservedModelBehavior::bootstrap("zai", "glm-4.6"),
        ObservedModelBehavior::bootstrap("zai", "glm-4.6v"),
        ObservedModelBehavior::bootstrap("zai", "glm-4.5"),
        ObservedModelBehavior::bootstrap("zai", "glm-4.5-air"),
        ObservedModelBehavior::bootstrap("zai", "glm-4.5v"),
        ObservedModelBehavior::bootstrap("zai", "glm-4-32b-0414-128k"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::{Modality, ModelLifecycle};
    use crate::observed::Score;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn test_zai_descriptors_required_models_present() {
        // §spec: glm-5.2, glm-4.6, glm-4.5-air müssen enthalten sein.
        let descs = zai_descriptors();
        for required in &["glm-5.2", "glm-4.6", "glm-4.5-air"] {
            assert!(
                descs.iter().any(|d| d.model == *required),
                "Pflichtmodell fehlt: {required}"
            );
        }
    }

    #[test]
    fn test_zai_descriptors_all_provider_zai() {
        // Alle Deskriptoren müssen provider == "zai" haben.
        for d in zai_descriptors() {
            assert_eq!(
                d.provider, "zai",
                "Falscher Provider bei Modell {}",
                d.model
            );
        }
    }

    #[test]
    fn test_zai_descriptors_all_have_text_modality() {
        // Jedes Modell muss Modality::Text enthalten.
        for d in zai_descriptors() {
            assert!(
                d.modalities.contains(Modality::Text),
                "Modality::Text fehlt bei {}",
                d.model
            );
        }
    }

    #[test]
    fn test_zai_descriptors_all_ga_lifecycle() {
        // Alle aktiven Z.AI-Modelle sind Generally Available.
        for d in zai_descriptors() {
            assert_eq!(
                d.lifecycle,
                ModelLifecycle::Ga,
                "Modell {} ist nicht Ga",
                d.model
            );
        }
    }

    #[test]
    fn test_glm52_flagship_context_window() -> TestResult {
        // GLM-5.2 muss das 1M-Kontext-Fenster haben (Pflicht-Assertion).
        let descs = zai_descriptors();
        let glm52 = descs
            .iter()
            .find(|d| d.model == "glm-5.2")
            .ok_or(TestError::Missing("glm-5.2 descriptor"))?;
        assert_eq!(
            glm52.context_window, 1_000_000,
            "GLM-5.2 context_window muss 1_000_000 sein"
        );
        Ok(())
    }

    #[test]
    fn test_glm52_reasoning_effort() -> TestResult {
        // GLM-5.2 unterstützt Effort-basiertes Reasoning (high/max).
        let descs = zai_descriptors();
        let glm52 = descs
            .iter()
            .find(|d| d.model == "glm-5.2")
            .ok_or(TestError::Missing("glm-5.2 descriptor"))?;
        assert_eq!(glm52.capabilities.reasoning, ReasoningSupport::Effort);
        Ok(())
    }

    #[test]
    fn test_multimodal_models_have_image_modality() {
        // Modelle mit image_input=true müssen Modality::Image im ModalitySet haben.
        for d in zai_descriptors() {
            if d.capabilities.image_input {
                assert!(
                    d.modalities.contains(Modality::Image),
                    "image_input=true aber Modality::Image fehlt bei {}",
                    d.model
                );
            }
        }
    }

    #[test]
    fn test_zai_observations_required_models_present() {
        // §spec: glm-5.2, glm-4.6, glm-4.5-air müssen in Observations enthalten sein.
        let obs = zai_observations();
        for required in &["glm-5.2", "glm-4.6", "glm-4.5-air"] {
            assert!(
                obs.iter().any(|o| o.model == *required),
                "Pflicht-Observation fehlt: {required}"
            );
        }
    }

    #[test]
    fn test_zai_observations_count_matches_descriptors() {
        // Observations und Descriptors müssen dieselbe Anzahl Einträge haben.
        assert_eq!(
            zai_observations().len(),
            zai_descriptors().len(),
            "Anzahl Observations stimmt nicht mit Descriptors überein"
        );
    }

    #[test]
    fn test_zai_observations_all_scores_half() {
        // Bootstrap-Defaults: alle Scores müssen Score::HALF sein.
        for obs in zai_observations() {
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
    fn test_zai_observations_no_duplicates() {
        // Keine Duplikate in der Observation-Liste.
        let obs = zai_observations();
        let mut seen = std::collections::HashSet::new();
        for o in &obs {
            assert!(
                seen.insert(o.model.as_str()),
                "Duplikat gefunden: {}",
                o.model
            );
        }
    }

    #[test]
    fn test_zai_observations_all_provider_zai() {
        // Alle Observations müssen provider == "zai" haben.
        for o in zai_observations() {
            assert_eq!(
                o.provider, "zai",
                "Falscher Provider bei Observation {}",
                o.model
            );
        }
    }

    #[test]
    fn test_descriptor_model_ids_match_observations() {
        // Die Modell-IDs in Descriptors und Observations müssen exakt übereinstimmen.
        let desc_strings: Vec<ModelId> = zai_descriptors().into_iter().map(|d| d.model).collect();
        let obs_strings: Vec<String> = zai_observations().into_iter().map(|o| o.model).collect();
        let desc_set: std::collections::HashSet<&str> =
            desc_strings.iter().map(|s| s.as_str()).collect();
        let obs_set: std::collections::HashSet<&str> =
            obs_strings.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            desc_set, obs_set,
            "Descriptor- und Observation-Modell-IDs stimmen nicht überein"
        );
    }
}
