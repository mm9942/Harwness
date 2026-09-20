//! Moonshot (Kimi) vendor module — Layer 2 & 4 bootstrap for provider `"moonshot"`.
//!
//! Dieses Modul ist ein OpenAI-Sibling: es stellt eine kuratierte Liste von
//! [`ModelDescriptor`]-Einträgen und zugehörige [`ObservedModelBehavior`]-Bootstrap-Einträge
//! für den Provider `"moonshot"` bereit.
//!
//! ## Verantwortung
//! - Bereitstellung von `moonshot_descriptors()` (Layer 2, deklarierte Fähigkeiten).
//! - Bereitstellung von `moonshot_observations()` (Layer 4, konservative Bootstrap-Defaults).
//! - Kein Netzwerkzugriff; alle Werte sind statisch aus Provider-Dokumentation entnommen.
//!
//! ## Datenquelle
//! `docs/research/models/moonshot.json`, Stand 2026-07-16.
//! Quellen: platform.kimi.ai/docs/models, GitHub MoonshotAI/Kimi-K2, OpenRouter, Cloudflare.
//!
//! `kimi-k3` (das `default_model` des Providers `"moonshot"` in `providers.toml`)
//! ist in `moonshot.json` noch nicht dokumentiert; die Werte sind konservativ vom
//! nächstälteren GA-Modell `kimi-k2.7-code` übernommen. Siehe TODO-Kommentar bei
//! der Deklaration.
//!
//! ## Wichtigste Typen
//! - [`ModelDescriptor`] — vollständige Modellbeschreibung (Layer 2)
//! - [`ObservedModelBehavior`] — Bootstrap-Messwerte (Layer 4)
//!
//! ## Lebenszyklus-Hinweise
//! Alle Modell-IDs mit `deprecated_on: 2026-05-25` (kimi-k2-turbo-preview,
//! kimi-k2-0905-preview, kimi-k2-0711-preview, kimi-k2-thinking,
//! kimi-k2-thinking-turbo) tragen `lifecycle: ModelLifecycle::Deprecated`.
//!
//! ## Nebenläufigkeitsmodell
//! Alle Typen sind `Clone + Send + Sync`; keine Locks, keine Threads.
//!
//! ## Fehlertypen
//! Dieses Modul ist infallibel; alle Konstruktoren sind deterministisch.
//!
//! ## Beispiel
//! ```rust,no_run
//! use harw_model_catalog::vendor_moonshot::{moonshot_descriptors, moonshot_observations};
//! let descs = moonshot_descriptors();
//! let obs   = moonshot_observations();
//! assert!(descs.iter().any(|d| d.model == "kimi-k2.6"));
//! assert!(descs.iter().any(|d| d.model == "kimi-k2.7-code"));
//! assert_eq!(descs.len(), obs.len());
//! ```

use crate::descriptor::{
    AgentFeatureSet, Modality, ModalitySet, ModelCapabilities, ModelDescriptor, ModelId,
    ModelLifecycle, PromptCachingSupport, ProviderId, ReasoningSupport, StreamingSupport,
    StructuredOutputSupport, ToolCallingSupport,
};
use crate::observed::ObservedModelBehavior;

/// Erstellt die kuratierte Descriptor-Liste für den Provider `"moonshot"` (Layer 2).
///
/// # Beschreibung
/// Alle Werte basieren auf Provider-Dokumentation (platform.kimi.ai, GitHub MoonshotAI/Kimi-K2,
/// OpenRouter, Cloudflare Workers AI), Stand 2026-07-16. Werte sind konservativ
/// gewählt und spiegeln deklarierte (nicht gemessene) Fähigkeiten wider.
///
/// Deprecated-IDs (per 2026-05-25):
/// - `kimi-k2-turbo-preview` — migrate_to: kimi-k2.6
/// - `kimi-k2-0905-preview` — migrate_to: kimi-k2.6
/// - `kimi-k2-0711-preview` — migrate_to: kimi-k2.6
/// - `kimi-k2-thinking`     — migrate_to: kimi-k2.6
/// - `kimi-k2-thinking-turbo` — migrate_to: kimi-k2.6
/// - `kimi-latest`          — deprecated 2026-01-28
/// - `kimi-thinking-preview` — deprecated 2025-11-11 (K1.5 era)
///
/// # Rückgabe
/// `Vec<ModelDescriptor>` mit allen aktiven und deprecated Moonshot-Modellen.
///
/// # Concurrency
/// Rein deterministisch, keine Seiteneffekte.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::vendor_moonshot::moonshot_descriptors;
/// let descs = moonshot_descriptors();
/// assert!(descs.iter().any(|d| d.model == "kimi-k2.6"));
/// assert!(descs.iter().any(|d| d.model == "kimi-k2.7-code"));
/// ```
pub fn moonshot_descriptors() -> Vec<ModelDescriptor> {
    vec![
        // ── Aktive Kimi-K3-Familie ───────────────────────────────────────────
        // Quelle: providers.toml führt "kimi-k3" als `default_model` des Providers
        // "moonshot", aber es existiert noch kein Eintrag in
        // docs/research/models/moonshot.json für diese Version. Werte sind daher
        // konservativ vom nächstälteren GA-Modell (kimi-k2.7-code) übernommen, um
        // Katalog-Drift zwischen providers.toml und bootstrap_descriptors() zu
        // vermeiden (siehe resolved.rs::every_provider_default_model_resolves_or_is_allowlisted).
        // TODO: durch verifizierte Werte ersetzen, sobald moonshot.json aktualisiert ist.
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("kimi-k3"),
            context_window: 262_144,
            max_output_tokens: Some(131_072),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image, Modality::Video]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonMode,
                reasoning: ReasoningSupport::Trace,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet {
                    computer_use: false,
                    code_execution: false,
                    built_in_search: false,
                    file_search: false,
                },
            },
            pricing: None, // autoritative Preise in harw-provider
            lifecycle: ModelLifecycle::Ga,
        },
        // ── Aktive Kimi-K2-Familie ───────────────────────────────────────────
        // Quelle: platform.kimi.ai/docs/guide/kimi-k2-7-code-quickstart
        // release_date: 2026-06-12; thinking_mode: always_on; MoE 1T/32B active
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("kimi-k2.7-code"),
            context_window: 262_144,
            max_output_tokens: Some(131_072),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image, Modality::Video]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonMode,
                // Thinking mode is always on; maps to Trace (visible reasoning tokens)
                reasoning: ReasoningSupport::Trace,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet {
                    computer_use: false,
                    code_execution: false,
                    built_in_search: false,
                    file_search: false,
                },
            },
            pricing: None, // autoritative Preise in harw-provider
            lifecycle: ModelLifecycle::Ga,
        },
        // Quelle: platform.kimi.ai/docs/guide/kimi-k2-7-code-quickstart (high-speed variant)
        // Same model, high-speed throughput variant (~180–260 tokens/s)
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("kimi-k2.7-code-highspeed"),
            context_window: 262_144,
            max_output_tokens: Some(131_072),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image, Modality::Video]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonMode,
                reasoning: ReasoningSupport::Trace,
                prompt_caching: PromptCachingSupport::Implicit,
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
            lifecycle: ModelLifecycle::Ga,
        },
        // Quelle: platform.kimi.ai/docs/guide/kimi-k2-6-quickstart
        // release_date: 2026-04-20; thinking_mode: optional; SWE-Bench Verified 80.2%
        // max_output_tokens: conservative 32768 (official quickstart); hard cap unpublished.
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("kimi-k2.6"),
            context_window: 262_144,
            max_output_tokens: Some(32_768),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image, Modality::Video]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonMode,
                // Thinking mode optional; Trace captures when reasoning tokens are visible
                reasoning: ReasoningSupport::Trace,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet {
                    computer_use: false,
                    code_execution: false,
                    // built-in $web_search exists but incompatible with thinking mode
                    built_in_search: true,
                    file_search: false,
                },
            },
            pricing: None,
            lifecycle: ModelLifecycle::Ga,
        },
        // Quelle: docs.aws.amazon.com/bedrock/.../model-card-moonshot-ai-kimi-k2-5.html
        // release_date: 2026-01; AWS Bedrock max_output_tokens=16384 (platform-specific)
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("kimi-k2.5"),
            context_window: 262_144,
            max_output_tokens: Some(16_384),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonMode,
                reasoning: ReasoningSupport::Trace,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: None,
            lifecycle: ModelLifecycle::Ga,
        },
        // ── Aktive moonshot-v1-Familie (legacy text-only) ───────────────────
        // Quelle: platform.kimi.ai/docs/models — differ only in context length
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("moonshot-v1-8k"),
            context_window: 8_192,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::None,
                parallel_tools: false,
                structured_output: StructuredOutputSupport::None,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::None,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: None,
            lifecycle: ModelLifecycle::Ga,
        },
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("moonshot-v1-32k"),
            context_window: 32_768,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::None,
                parallel_tools: false,
                structured_output: StructuredOutputSupport::None,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::None,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: None,
            lifecycle: ModelLifecycle::Ga,
        },
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("moonshot-v1-128k"),
            context_window: 131_072,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::None,
                parallel_tools: false,
                structured_output: StructuredOutputSupport::None,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::None,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: None,
            lifecycle: ModelLifecycle::Ga,
        },
        // ── Aktive moonshot-v1 vision-preview (legacy) ──────────────────────
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("moonshot-v1-8k-vision-preview"),
            context_window: 8_192,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::None,
                parallel_tools: false,
                structured_output: StructuredOutputSupport::None,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::None,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: None,
            lifecycle: ModelLifecycle::Preview,
        },
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("moonshot-v1-32k-vision-preview"),
            context_window: 32_768,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::None,
                parallel_tools: false,
                structured_output: StructuredOutputSupport::None,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::None,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: None,
            lifecycle: ModelLifecycle::Preview,
        },
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("moonshot-v1-128k-vision-preview"),
            context_window: 131_072,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::None,
                parallel_tools: false,
                structured_output: StructuredOutputSupport::None,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::None,
                streaming: StreamingSupport::ServerSent,
                image_input: true,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: None,
            lifecycle: ModelLifecycle::Preview,
        },
        // ── Deprecated per 2026-05-25 ────────────────────────────────────────
        // Quelle: moonshot.json §models.deprecated — all migrate_to: kimi-k2.6
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("kimi-k2-turbo-preview"),
            context_window: 262_144,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonMode,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: None,
            lifecycle: ModelLifecycle::Deprecated,
        },
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("kimi-k2-0905-preview"),
            context_window: 262_144,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonMode,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: None,
            lifecycle: ModelLifecycle::Deprecated,
        },
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("kimi-k2-0711-preview"),
            context_window: 262_144,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonMode,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: None,
            lifecycle: ModelLifecycle::Deprecated,
        },
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("kimi-k2-thinking"),
            context_window: 262_144,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonMode,
                reasoning: ReasoningSupport::Trace,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: None,
            lifecycle: ModelLifecycle::Deprecated,
        },
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("kimi-k2-thinking-turbo"),
            context_window: 262_144,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonMode,
                reasoning: ReasoningSupport::Trace,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: None,
            lifecycle: ModelLifecycle::Deprecated,
        },
        // kimi-latest deprecated 2026-01-28
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("kimi-latest"),
            context_window: 262_144,
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
            pricing: None,
            lifecycle: ModelLifecycle::Deprecated,
        },
        // kimi-thinking-preview deprecated 2025-11-11 (K1.5 era)
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("kimi-thinking-preview"),
            context_window: 131_072,
            max_output_tokens: None,
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::None,
                parallel_tools: false,
                structured_output: StructuredOutputSupport::None,
                reasoning: ReasoningSupport::Trace,
                prompt_caching: PromptCachingSupport::None,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: None,
            lifecycle: ModelLifecycle::Deprecated,
        },
    ]
}

/// Erstellt Bootstrap-Beobachtungen (Layer 4) für alle Moonshot-Modelle.
///
/// # Beschreibung
/// Gibt einen `Vec` mit je einem [`ObservedModelBehavior::bootstrap`]-Eintrag
/// für jedes in [`moonshot_descriptors`] enthaltene Modell zurück.
/// Reihenfolge und Anzahl sind identisch mit `moonshot_descriptors()`.
/// Alle Werte sind konservative Defaults ohne Evidenz (§5 model-catalog-v2.md).
///
/// # Rückgabe
/// `Vec<ObservedModelBehavior>` — gleiche Länge wie `moonshot_descriptors()`.
///
/// # Concurrency
/// Rein deterministisch, keine Seiteneffekte.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::vendor_moonshot::{moonshot_descriptors, moonshot_observations};
/// let obs = moonshot_observations();
/// assert_eq!(obs.len(), moonshot_descriptors().len());
/// assert!(obs.iter().any(|o| o.model == "kimi-k2.6"));
/// ```
pub fn moonshot_observations() -> Vec<ObservedModelBehavior> {
    crate::observations_from_descriptors(&moonshot_descriptors())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::{Modality, ModelLifecycle};

    #[test]
    fn kimi_k2_6_is_present() {
        // §spec: kimi-k2.6 must be in descriptors (required assertion)
        let descs = moonshot_descriptors();
        assert!(
            descs.iter().any(|d| d.model == "kimi-k2.6"),
            "kimi-k2.6 fehlt in moonshot_descriptors()"
        );
    }

    #[test]
    fn kimi_k2_7_code_is_present() {
        // §spec: kimi-k2.7-code must be in descriptors (required assertion)
        let descs = moonshot_descriptors();
        assert!(
            descs.iter().any(|d| d.model == "kimi-k2.7-code"),
            "kimi-k2.7-code fehlt in moonshot_descriptors()"
        );
    }

    #[test]
    fn kimi_k3_is_present() {
        // kimi-k3 ist das `default_model` des Providers "moonshot" in
        // providers.toml und muss deshalb über resolve() auflösbar sein.
        let descs = moonshot_descriptors();
        assert!(
            descs.iter().any(|d| d.model == "kimi-k3"),
            "kimi-k3 fehlt in moonshot_descriptors() (providers.toml default_model)"
        );
    }

    #[test]
    fn deprecated_preview_ids_have_deprecated_lifecycle() {
        // kimi-k2-turbo-preview and kimi-k2-0905-preview deprecated per 2026-05-25
        let descs = moonshot_descriptors();
        for id in &["kimi-k2-turbo-preview", "kimi-k2-0905-preview"] {
            let entry = descs
                .iter()
                .find(|d| d.model.as_str() == *id)
                .unwrap_or_else(|| panic!("Modell '{}' fehlt in moonshot_descriptors()", id));
            assert_eq!(
                entry.lifecycle,
                ModelLifecycle::Deprecated,
                "Modell '{}' muss lifecycle=Deprecated haben",
                id
            );
        }
    }

    #[test]
    fn all_deprecated_k2_ids_are_deprecated() {
        // All five K2-era deprecated IDs must carry Deprecated lifecycle
        let deprecated_ids = [
            "kimi-k2-turbo-preview",
            "kimi-k2-0905-preview",
            "kimi-k2-0711-preview",
            "kimi-k2-thinking",
            "kimi-k2-thinking-turbo",
        ];
        let descs = moonshot_descriptors();
        for id in &deprecated_ids {
            let entry = descs
                .iter()
                .find(|d| d.model.as_str() == *id)
                .unwrap_or_else(|| panic!("Modell '{}' fehlt", id));
            assert_eq!(
                entry.lifecycle,
                ModelLifecycle::Deprecated,
                "'{}' muss Deprecated sein",
                id
            );
        }
    }

    #[test]
    fn all_have_provider_moonshot() {
        // Every descriptor must carry provider="moonshot"
        for d in moonshot_descriptors() {
            assert_eq!(
                d.provider, "moonshot",
                "Falscher Provider bei Modell '{}'",
                d.model
            );
        }
    }

    #[test]
    fn all_have_text_modality() {
        // Every descriptor must include Modality::Text
        for d in moonshot_descriptors() {
            assert!(
                d.modalities.contains(Modality::Text),
                "Modality::Text fehlt bei '{}'",
                d.model
            );
        }
    }

    #[test]
    fn image_models_have_image_modality_and_flag() {
        // image_input=true ↔ Modality::Image present
        for d in moonshot_descriptors() {
            if d.capabilities.image_input {
                assert!(
                    d.modalities.contains(Modality::Image),
                    "image_input=true aber Modality::Image fehlt bei '{}'",
                    d.model
                );
            }
        }
    }

    #[test]
    fn observations_len_matches_descriptors() {
        // moonshot_observations() must be same length as moonshot_descriptors()
        assert_eq!(
            moonshot_observations().len(),
            moonshot_descriptors().len(),
            "Länge von observations und descriptors stimmt nicht überein"
        );
    }

    #[test]
    fn observations_all_half_scores() {
        use crate::observed::Score;
        for obs in moonshot_observations() {
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
    fn no_duplicate_model_ids() {
        let descs = moonshot_descriptors();
        let mut seen = std::collections::HashSet::new();
        for d in &descs {
            assert!(
                seen.insert(d.model.clone()),
                "Duplizierte Modell-ID: '{}'",
                d.model
            );
        }
    }

    #[test]
    fn all_have_positive_context_window() {
        for d in moonshot_descriptors() {
            assert!(
                d.context_window > 0,
                "context_window == 0 bei '{}'",
                d.model
            );
        }
    }

    #[test]
    fn kimi_k2_6_is_ga_with_tool_calling() {
        let descs = moonshot_descriptors();
        let k26 = descs.iter().find(|d| d.model == "kimi-k2.6").unwrap();
        assert_eq!(k26.lifecycle, ModelLifecycle::Ga);
        assert_eq!(k26.capabilities.tool_calling, ToolCallingSupport::Parallel);
        assert!(k26.capabilities.parallel_tools);
    }

    #[test]
    fn kimi_k2_7_code_always_on_reasoning() {
        // Thinking mode is always on for kimi-k2.7-code → ReasoningSupport::Trace
        let descs = moonshot_descriptors();
        let k27 = descs.iter().find(|d| d.model == "kimi-k2.7-code").unwrap();
        assert_eq!(k27.capabilities.reasoning, ReasoningSupport::Trace);
        assert_eq!(k27.lifecycle, ModelLifecycle::Ga);
    }
}
