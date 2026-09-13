#![allow(clippy::vec_init_then_push)]
//! xAI-Modellkatalog — aus verifizierter Vendor-Research 2026-07-16.
//!
//! Quelle: `docs/research/models/xai.json`, verifiziert am 2026-07-16.
//! Endpunkt (OpenAI-kompatibel): `https://api.x.ai/v1`
//!
//! ## Verantwortung
//! Deklariert alle aktuellen und eingestellten xAI-Modelle als
//! [`ModelDescriptor`]-Einträge sowie zugehörige Bootstrap-
//! [`ObservedModelBehavior`]-Einträge (alle Scores auf `Score::HALF`,
//! kein Evidenz-Stand).
//!
//! ## Wichtigste Typen
//! - [`xai_descriptors`] — liefert `ModelDescriptor`-Einträge laut Vendor-JSON.
//! - [`xai_observations`] — liefert je einen Bootstrap-Beobachtungs-Eintrag pro Modell.
//!
//! ## Besonderheiten
//! - `logprobs` und `top_logprobs` werden ab grok-4.20 stillschweigend ignoriert.
//! - Acht Legacy-Modelle wurden am 2026-05-15 eingestellt; sie sind mit
//!   `lifecycle: Retired` aufgenommen.
//! - grok-4.5 ist zum Launch (2026-07-08) nicht im EU-API-Konsol verfügbar.
//!
//! ## Nebenläufigkeit
//! Beide Funktionen sind rein deterministisch und frei von Seiteneffekten.
//! Alle zurückgegebenen Typen sind `Send + Sync`.
//!
//! ## Fehlertypen
//! Dieses Modul ist infallibel; keine `Result`- oder `Option`-Rückgaben.
//!
//! ## Beispiel
//! ```rust,no_run
//! use harw_model_catalog::vendor_xai::xai_descriptors;
//! let catalog = xai_descriptors();
//! assert!(catalog.iter().any(|d| d.model == "grok-4.5"));
//! ```

use crate::descriptor::{
    AgentFeatureSet, Modality, ModalitySet, ModelCapabilities, ModelDescriptor, ModelId,
    ModelLifecycle, PromptCachingSupport, ProviderId, ReasoningSupport, StreamingSupport,
    StructuredOutputSupport, ToolCallingSupport,
};
use crate::observed::ObservedModelBehavior;

/// Gibt deklarierte [`ModelDescriptor`]-Einträge aller xAI-Modelle zurück, einschließlich
/// eingestellter Modelle.
///
/// # Beschreibung
/// Alle Einträge werden direkt aus `docs/research/models/xai.json` (Stand 2026-07-16)
/// abgeschrieben. Es wird kein JSON zur Laufzeit geparst; die Werte sind hardcodiert
/// und mit der Quelldatei verifiziert.
///
/// Mapping-Regeln (analog zu OpenAI-Sibling):
/// - `provider` = `"xai"` (fest).
/// - `model` = `api_id` aus dem JSON.
/// - `context_window` / `max_output_tokens` direkt aus JSON; `null` → `None`.
/// - `modalities`: immer `Text`; `Image` wenn `vision = true`; `Video` wenn in `input`-Liste.
/// - `tool_calling`: `true` → `Parallel`, `false` / fehlt → `None`.
/// - `structured_output`: `true` → `JsonSchema`, `null`/`false` → `None`.
/// - `reasoning`: `supported: true` → `Effort`, `supported: false` → `None`.
/// - `prompt_caching`: explizite Cache-Preise vorhanden → `Implicit`, sonst → `None`.
/// - `streaming`: immer `ServerSent` für aktive Modelle.
/// - `native_agent_features`: alle `false` (xAI deklariert keine nativen Agenten-Features).
/// - `lifecycle`: `"GA"` → `Ga`, `"public_beta"` → `Preview`, retired → `Retired`.
/// - `pricing`: `None` (Preise leben in `harw-provider`).
///
/// # Rückgabe
/// `Vec<ModelDescriptor>` mit aktiven und eingestellten Einträgen.
///
/// # Concurrency
/// Rein deterministisch, keine Seiteneffekte.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::vendor_xai::xai_descriptors;
/// let catalog = xai_descriptors();
/// assert!(catalog.iter().any(|d| d.model == "grok-4.5"));
/// assert!(catalog.iter().any(|d| d.model == "grok-build-0.1"));
/// assert!(catalog.iter().any(|d| d.model == "grok-4.3"));
/// ```
pub fn xai_descriptors() -> Vec<ModelDescriptor> {
    let mut out = Vec::with_capacity(12);

    // ── grok-4.5 ──────────────────────────────────────────────────────────────
    // lifecycle: ga | context: 500_000 | max_output: null → None
    // tool_calling: true → Parallel | structured: true → JsonSchema | reasoning: effort
    // prompt_caching: implicit (cached pricing present) | streaming: server_sent
    // image_input: true | vision: true
    // Released: 2026-07-08. Flagship, V9 architecture, 1.5T params MoE.
    // Not yet EU-available at launch.
    out.push(ModelDescriptor {
        provider: ProviderId::from("xai"),
        model: ModelId::from("grok-4.5"),
        context_window: 500_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
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
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── grok-build-0.1 ────────────────────────────────────────────────────────
    // lifecycle: public_beta → Preview | context: 256_000 | max_output: null → None
    // tool_calling: true → Parallel | structured: null → None | reasoning: effort
    // prompt_caching: None | streaming: server_sent | image_input: true
    // Released: 2026-05-29. Purpose-built coding model. Replaced grok-code-fast-1.
    // Integrations: Cursor, Kilo Code, OpenCode, MCP.
    out.push(ModelDescriptor {
        provider: ProviderId::from("xai"),
        model: ModelId::from("grok-build-0.1"),
        context_window: 256_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Preview,
    });

    // ── grok-4.3 ──────────────────────────────────────────────────────────────
    // lifecycle: ga | context: 1_000_000 | max_output: null → None
    // tool_calling: true → Parallel | structured: null → None | reasoning: effort
    // prompt_caching: None | streaming: server_sent
    // image_input: true | video: true (native video input)
    // Released: 2026-04-30. Previous flagship. EU-available. Recommended EU fallback.
    out.push(ModelDescriptor {
        provider: ProviderId::from("xai"),
        model: ModelId::from("grok-4.3"),
        context_window: 1_000_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image, Modality::Video]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── grok-4.20-0309-reasoning ──────────────────────────────────────────────
    // lifecycle: ga | context: 1_000_000 | max_output: null → None
    // tool_calling: true → Parallel | structured: null → None | reasoning: effort
    // prompt_caching: None | streaming: server_sent | image_input: false (null)
    // Released: 2026-03-09. Pinned reasoning-only variant from four-agent architecture launch.
    // logprobs/top_logprobs silently ignored.
    out.push(ModelDescriptor {
        provider: ProviderId::from("xai"),
        model: ModelId::from("grok-4.20-0309-reasoning"),
        context_window: 1_000_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── grok-4.20-0309-non-reasoning ──────────────────────────────────────────
    // lifecycle: ga | context: 1_000_000 | max_output: null → None
    // tool_calling: true → Parallel | structured: null → None | reasoning: none
    // prompt_caching: None | streaming: server_sent | image_input: false (null)
    // Released: 2026-03-09. Fast non-reasoning counterpart. logprobs silently ignored.
    out.push(ModelDescriptor {
        provider: ProviderId::from("xai"),
        model: ModelId::from("grok-4.20-0309-non-reasoning"),
        context_window: 1_000_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::None,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── grok-4.20-multi-agent-0309 ────────────────────────────────────────────
    // lifecycle: ga | context: 1_000_000 | max_output: null → None
    // tool_calling: true → Parallel | structured: null → None | reasoning: effort
    // prompt_caching: None | streaming: server_sent | image_input: false (null)
    // Released: 2026-03-09. Four-agent architecture (Grok, Harper, Benjamin, Lucas).
    // reasoning_effort controls agent count; xhigh = all four sub-agents active.
    out.push(ModelDescriptor {
        provider: ProviderId::from("xai"),
        model: ModelId::from("grok-4.20-multi-agent-0309"),
        context_window: 1_000_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── RETIRED MODELS (2026-05-15) ───────────────────────────────────────────

    // ── grok-3 ────────────────────────────────────────────────────────────────
    // Retired 2026-05-15; redirects to grok-4.3.
    out.push(ModelDescriptor {
        provider: ProviderId::from("xai"),
        model: ModelId::from("grok-3"),
        context_window: 131_072,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::None,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Retired,
    });

    // ── grok-4 ────────────────────────────────────────────────────────────────
    // Retired 2026-05-15; redirects to grok-4.3.
    out.push(ModelDescriptor {
        provider: ProviderId::from("xai"),
        model: ModelId::from("grok-4"),
        context_window: 256_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Retired,
    });

    // ── grok-4-fast ───────────────────────────────────────────────────────────
    // Retired 2026-05-15; redirects to grok-4.3.
    out.push(ModelDescriptor {
        provider: ProviderId::from("xai"),
        model: ModelId::from("grok-4-fast"),
        context_window: 256_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Retired,
    });

    // ── grok-4-0709 ───────────────────────────────────────────────────────────
    // Retired 2026-05-15; redirects to grok-4.3.
    out.push(ModelDescriptor {
        provider: ProviderId::from("xai"),
        model: ModelId::from("grok-4-0709"),
        context_window: 256_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Retired,
    });

    // ── grok-code-fast-1 ──────────────────────────────────────────────────────
    // Retired 2026-05-15; redirects to grok-build-0.1.
    out.push(ModelDescriptor {
        provider: ProviderId::from("xai"),
        model: ModelId::from("grok-code-fast-1"),
        context_window: 131_072,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::None,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Retired,
    });

    // ── grok-imagine-image-pro ────────────────────────────────────────────────
    // Retired 2026-05-15; no redirect (image-generation model).
    out.push(ModelDescriptor {
        provider: ProviderId::from("xai"),
        model: ModelId::from("grok-imagine-image-pro"),
        context_window: 8_192,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::None,
            parallel_tools: false,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::None,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::None,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Retired,
    });

    out
}

/// Gibt Bootstrap-[`ObservedModelBehavior`]-Einträge für alle xAI-Modelle zurück.
///
/// # Beschreibung
/// Für jedes Modell aus [`xai_descriptors`] wird ein Eintrag via
/// [`ObservedModelBehavior::bootstrap`] erzeugt. Alle Scores stehen auf
/// `Score::HALF`, `updated_at` ist `None` und `evidence` ist leer.
/// Die Einträge werden durch echte Harness-Runs befüllt.
///
/// # Rückgabe
/// `Vec<ObservedModelBehavior>` mit derselben Anzahl wie [`xai_descriptors`].
///
/// # Concurrency
/// Rein deterministisch, keine Seiteneffekte.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::vendor_xai::{xai_descriptors, xai_observations};
/// let obs = xai_observations();
/// assert_eq!(obs.len(), xai_descriptors().len());
/// ```
pub fn xai_observations() -> Vec<ObservedModelBehavior> {
    crate::observations_from_descriptors(&xai_descriptors())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observed::Score;

    /// Stellt sicher, dass grok-4.5, grok-build-0.1 und grok-4.3 im Katalog enthalten sind.
    #[test]
    fn catalog_contains_required_models() {
        let catalog = xai_descriptors();
        assert!(
            catalog.iter().any(|d| d.model == "grok-4.5"),
            "grok-4.5 fehlt im xAI-Katalog"
        );
        assert!(
            catalog.iter().any(|d| d.model == "grok-build-0.1"),
            "grok-build-0.1 fehlt im xAI-Katalog"
        );
        assert!(
            catalog.iter().any(|d| d.model == "grok-4.3"),
            "grok-4.3 fehlt im xAI-Katalog"
        );
    }

    /// Jeder Descriptor muss provider == "xai" haben.
    #[test]
    fn descriptors_have_xai_provider() {
        for d in xai_descriptors() {
            assert_eq!(
                d.provider, "xai",
                "Unerwarteter Provider '{}' bei Modell '{}'",
                d.provider, d.model
            );
        }
    }

    /// Jeder Descriptor muss Modality::Text enthalten.
    #[test]
    fn all_have_text_modality() {
        for d in xai_descriptors() {
            assert!(
                d.modalities.contains(Modality::Text),
                "Modality::Text fehlt bei {}",
                d.model
            );
        }
    }

    /// Jeder Descriptor muss ein positives Kontextfenster haben.
    #[test]
    fn descriptors_have_positive_context_window() {
        for d in xai_descriptors() {
            assert!(d.context_window > 0, "context_window == 0 bei {}", d.model);
        }
    }

    /// Modelle mit image_input=true müssen Modality::Image enthalten.
    #[test]
    fn image_models_have_image_modality() {
        for d in xai_descriptors() {
            if d.capabilities.image_input {
                assert!(
                    d.modalities.contains(Modality::Image),
                    "image_input=true, aber Modality::Image fehlt bei {}",
                    d.model
                );
            }
        }
    }

    /// Keine zwei Modelle dürfen dieselbe model-ID haben.
    #[test]
    fn all_ids_unique() {
        let descriptors = xai_descriptors();
        let mut seen = std::collections::HashSet::new();
        for d in &descriptors {
            assert!(
                seen.insert(d.model.as_str()),
                "Duplikate Modell-ID gefunden: '{}'",
                d.model
            );
        }
    }

    /// Anzahl der Observations muss mit der Anzahl der Descriptors übereinstimmen.
    #[test]
    fn observations_match_descriptor_count() {
        assert_eq!(
            xai_observations().len(),
            xai_descriptors().len(),
            "Observations-Anzahl weicht von Descriptors-Anzahl ab"
        );
    }

    /// Alle Observations haben Bootstrap-Scores von Score::HALF ohne Evidenz.
    #[test]
    fn all_observations_have_half_scores() {
        for obs in xai_observations() {
            assert_eq!(
                obs.tool_schema_reliability,
                Score::HALF,
                "tool_schema_reliability != HALF bei {}",
                obs.model
            );
            assert!(
                obs.updated_at.is_none(),
                "updated_at sollte None sein bei {}",
                obs.model
            );
            assert!(
                obs.evidence.is_empty(),
                "evidence sollte leer sein bei {}",
                obs.model
            );
        }
    }

    /// Retired-Modelle sind korrekt mit ModelLifecycle::Retired markiert.
    #[test]
    fn retired_models_have_retired_lifecycle() {
        let retired_ids = [
            "grok-3",
            "grok-4",
            "grok-4-fast",
            "grok-4-0709",
            "grok-code-fast-1",
            "grok-imagine-image-pro",
        ];
        for d in xai_descriptors() {
            if retired_ids.contains(&d.model.as_str()) {
                assert_eq!(
                    d.lifecycle,
                    ModelLifecycle::Retired,
                    "'{}' sollte Retired sein",
                    d.model
                );
            }
        }
    }

    /// grok-build-0.1 ist im Preview-Status (public_beta).
    #[test]
    fn grok_build_is_preview() {
        let d = xai_descriptors()
            .into_iter()
            .find(|d| d.model == "grok-build-0.1")
            .expect("grok-build-0.1 fehlt im Katalog");
        assert_eq!(
            d.lifecycle,
            ModelLifecycle::Preview,
            "grok-build-0.1 sollte Preview-Lifecycle haben"
        );
    }

    /// grok-4.3 unterstützt Video-Eingabe (native video input).
    #[test]
    fn grok_4_3_has_video_modality() {
        let d = xai_descriptors()
            .into_iter()
            .find(|d| d.model == "grok-4.3")
            .expect("grok-4.3 fehlt im Katalog");
        assert!(
            d.modalities.contains(Modality::Video),
            "grok-4.3 sollte Modality::Video enthalten"
        );
    }

    /// grok-4.20-0309-non-reasoning hat kein Reasoning.
    #[test]
    fn non_reasoning_variant_has_no_reasoning() {
        let d = xai_descriptors()
            .into_iter()
            .find(|d| d.model == "grok-4.20-0309-non-reasoning")
            .expect("grok-4.20-0309-non-reasoning fehlt im Katalog");
        assert_eq!(
            d.capabilities.reasoning,
            ReasoningSupport::None,
            "grok-4.20-0309-non-reasoning sollte kein Reasoning haben"
        );
    }

    /// grok-4.5 hat Prompt-Caching (implizit, aufgrund gecachter Preise).
    #[test]
    fn grok_4_5_has_implicit_caching() {
        let d = xai_descriptors()
            .into_iter()
            .find(|d| d.model == "grok-4.5")
            .expect("grok-4.5 fehlt im Katalog");
        assert_eq!(
            d.capabilities.prompt_caching,
            PromptCachingSupport::Implicit,
            "grok-4.5 sollte implizites Prompt-Caching haben"
        );
    }

    /// grok-imagine-image-pro hat kein Streaming (eingestelltes Image-Modell).
    #[test]
    fn imagine_image_pro_has_no_streaming() {
        let d = xai_descriptors()
            .into_iter()
            .find(|d| d.model == "grok-imagine-image-pro")
            .expect("grok-imagine-image-pro fehlt im Katalog");
        assert_eq!(
            d.capabilities.streaming,
            StreamingSupport::None,
            "grok-imagine-image-pro sollte kein Streaming haben"
        );
    }
}
