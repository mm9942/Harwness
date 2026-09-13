#![allow(clippy::vec_init_then_push)]
//! OpenAI-Modellkatalog — aus verifizierter Vendor-Research 2026-07-16.
//!
//! Quelle: `docs/research/models/openai.json`, verifiziert am 2026-07-16.
//!
//! ## Verantwortung
//! Deklariert alle aktuellen OpenAI-Modelle als [`ModelDescriptor`]-Einträge sowie
//! zugehörige Bootstrap-[`ObservedModelBehavior`]-Einträge (alle Scores auf
//! `Score::HALF`, kein Evidenz-Stand).
//!
//! ## Wichtigste Typen
//! - [`openai_descriptors`] — liefert 17 `ModelDescriptor`-Einträge laut Vendor-JSON.
//! - [`openai_observations`] — liefert je einen Bootstrap-Beobachtungs-Eintrag pro Modell.
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
//! use harw_model_catalog::vendor_openai::openai_descriptors;
//! let catalog = openai_descriptors();
//! assert_eq!(catalog.len(), 17);
//! ```

use crate::descriptor::{
    AgentFeatureSet, Modality, ModalitySet, ModelCapabilities, ModelDescriptor, ModelId,
    ModelLifecycle, PromptCachingSupport, ProviderId, ReasoningSupport, StreamingSupport,
    StructuredOutputSupport, ToolCallingSupport,
};
use crate::observed::ObservedModelBehavior;

/// Gibt deklarierte [`ModelDescriptor`]-Einträge aller aktuellen OpenAI-Modelle zurück.
///
/// # Beschreibung
/// Alle 18 Einträge werden direkt aus `docs/research/models/openai.json` (Stand
/// 2026-07-16) abgeschrieben. Es wird kein JSON zur Laufzeit geparst; die Werte
/// sind hardcodiert und mit der Quelldatei verifiziert.
///
/// Mapping-Regeln (gemäß Orchestrator-Brief):
/// - `provider` = `"openai"` (fest).
/// - `model` = `canonical_id`.
/// - `context_window` / `max_output_tokens` direkt aus JSON.
/// - `modalities`: immer `Text`; `Image` wenn `image_input = true`.
/// - `tool_calling`: `"none"` → `None`, `"parallel"` → `Parallel`.
/// - `structured_output`: `null`/`"none"` → `None`, `"json_schema"` → `JsonSchema`.
/// - `reasoning`: `"none"` → `None`, `"effort"` → `Effort`.
/// - `prompt_caching`: `null`/`"none"` → `None`, `"implicit"` → `Implicit`, `"explicit"` → `Explicit`.
/// - `streaming`: `"none"` → `None`, `"server_sent"` → `ServerSent`.
/// - `native_agent_features`: boolsche Flags direkt; `null` → `false`.
/// - `lifecycle`: `"ga"` → `Ga`, `"deprecated"` → `Deprecated`.
/// - `pricing`: `None` (Preise leben in `harw-provider`).
///
/// # Rückgabe
/// `Vec<ModelDescriptor>` mit genau 17 Einträgen.
///
/// # Concurrency
/// Rein deterministisch, keine Seiteneffekte.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::vendor_openai::openai_descriptors;
/// let catalog = openai_descriptors();
/// assert_eq!(catalog.len(), 17);
/// assert!(catalog.iter().any(|d| d.model == "gpt-5.6-sol"));
/// ```
pub fn openai_descriptors() -> Vec<ModelDescriptor> {
    let mut out = Vec::with_capacity(17);

    // ── GPT-5.6-sol ──────────────────────────────────────────────────────────
    // lifecycle: ga | context: 1_050_000 | max_output: 128_000
    // tool_calling: parallel | structured: json_schema | reasoning: effort
    // prompt_caching: explicit | streaming: server_sent | image_input: true
    // computer_use: true | code_execution: true | built_in_search: true | file_search: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("gpt-5.6-sol"),
        context_window: 1_050_000,
        max_output_tokens: Some(128_000),
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
                code_execution: true,
                built_in_search: true,
                file_search: true,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── GPT-5.6-terra ─────────────────────────────────────────────────────────
    // lifecycle: ga | context: 1_050_000 | max_output: 128_000
    // tool_calling: parallel | structured: json_schema | reasoning: effort
    // prompt_caching: explicit | streaming: server_sent | image_input: true
    // computer_use: true | code_execution: true | built_in_search: true | file_search: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("gpt-5.6-terra"),
        context_window: 1_050_000,
        max_output_tokens: Some(128_000),
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
                code_execution: true,
                built_in_search: true,
                file_search: true,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── GPT-5.6-luna ──────────────────────────────────────────────────────────
    // lifecycle: ga | context: 1_050_000 | max_output: 128_000
    // tool_calling: parallel | structured: json_schema | reasoning: effort
    // prompt_caching: explicit | streaming: server_sent | image_input: true
    // computer_use: true | code_execution: true | built_in_search: true | file_search: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("gpt-5.6-luna"),
        context_window: 1_050_000,
        max_output_tokens: Some(128_000),
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
                code_execution: true,
                built_in_search: true,
                file_search: true,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── GPT-5.4-pro ───────────────────────────────────────────────────────────
    // lifecycle: ga | context: 1_050_000 | max_output: 128_000
    // tool_calling: parallel | structured: null → None | reasoning: effort
    // prompt_caching: implicit | streaming: server_sent | image_input: true
    // computer_use: true | code_execution: false | built_in_search: true | file_search: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("gpt-5.4-pro"),
        context_window: 1_050_000,
        max_output_tokens: Some(128_000),
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Implicit,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet {
                computer_use: true,
                code_execution: false,
                built_in_search: true,
                file_search: true,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── GPT-5.4 ───────────────────────────────────────────────────────────────
    // lifecycle: ga | context: 1_050_000 | max_output: 128_000
    // tool_calling: parallel | structured: json_schema | reasoning: effort
    // prompt_caching: implicit | streaming: server_sent | image_input: true
    // computer_use: true | code_execution: true | built_in_search: true | file_search: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("gpt-5.4"),
        context_window: 1_050_000,
        max_output_tokens: Some(128_000),
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Implicit,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet {
                computer_use: true,
                code_execution: true,
                built_in_search: true,
                file_search: true,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── GPT-5.4-mini ──────────────────────────────────────────────────────────
    // lifecycle: ga | context: 400_000 | max_output: 128_000
    // tool_calling: parallel | structured: json_schema | reasoning: effort
    // prompt_caching: implicit | streaming: server_sent | image_input: true
    // computer_use: true | code_execution: true | built_in_search: true | file_search: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("gpt-5.4-mini"),
        context_window: 400_000,
        max_output_tokens: Some(128_000),
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Implicit,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet {
                computer_use: true,
                code_execution: true,
                built_in_search: true,
                file_search: true,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── GPT-5.4-nano ──────────────────────────────────────────────────────────
    // lifecycle: ga | context: 400_000 | max_output: 128_000
    // tool_calling: parallel | structured: json_schema | reasoning: effort
    // prompt_caching: implicit | streaming: server_sent | image_input: true
    // computer_use: false | code_execution: true | built_in_search: true | file_search: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("gpt-5.4-nano"),
        context_window: 400_000,
        max_output_tokens: Some(128_000),
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Implicit,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet {
                computer_use: false,
                code_execution: true,
                built_in_search: true,
                file_search: true,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── GPT-5.2 ───────────────────────────────────────────────────────────────
    // lifecycle: ga | context: 400_000 | max_output: 128_000
    // tool_calling: parallel | structured: json_schema | reasoning: effort
    // prompt_caching: implicit | streaming: server_sent | image_input: true
    // native_agent_features: all null → all false
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("gpt-5.2"),
        context_window: 400_000,
        max_output_tokens: Some(128_000),
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

    // ── GPT-5 ─────────────────────────────────────────────────────────────────
    // lifecycle: ga | context: 400_000 | max_output: 128_000
    // tool_calling: parallel | structured: json_schema | reasoning: effort
    // prompt_caching: implicit | streaming: server_sent | image_input: true
    // native_agent_features: all null → all false
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("gpt-5"),
        context_window: 400_000,
        max_output_tokens: Some(128_000),
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

    // ── GPT-4.1 ───────────────────────────────────────────────────────────────
    // lifecycle: ga | context: 1_047_576 | max_output: 32_768
    // tool_calling: parallel | structured: json_schema | reasoning: none
    // prompt_caching: implicit | streaming: server_sent | image_input: true
    // native_agent_features: all null → all false
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("gpt-4.1"),
        context_window: 1_047_576,
        max_output_tokens: Some(32_768),
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::None,
            prompt_caching: PromptCachingSupport::Implicit,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── GPT-4.1-mini ──────────────────────────────────────────────────────────
    // lifecycle: ga | context: 1_047_576 | max_output: 32_768
    // tool_calling: parallel | structured: json_schema | reasoning: none
    // prompt_caching: implicit | streaming: server_sent | image_input: true
    // native_agent_features: all null → all false
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("gpt-4.1-mini"),
        context_window: 1_047_576,
        max_output_tokens: Some(32_768),
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::None,
            prompt_caching: PromptCachingSupport::Implicit,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── GPT-4.1-nano ──────────────────────────────────────────────────────────
    // lifecycle: ga | context: 1_047_576 | max_output: 32_768
    // tool_calling: parallel | structured: json_schema | reasoning: none
    // prompt_caching: implicit | streaming: server_sent | image_input: true
    // native_agent_features: all null → all false
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("gpt-4.1-nano"),
        context_window: 1_047_576,
        max_output_tokens: Some(32_768),
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::None,
            prompt_caching: PromptCachingSupport::Implicit,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── o3-pro ────────────────────────────────────────────────────────────────
    // lifecycle: ga | context: 200_000 | max_output: 100_000
    // tool_calling: parallel | structured: json_schema | reasoning: effort
    // prompt_caching: null → None | streaming: none → None | image_input: true
    // native_agent_features: all null → all false
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("o3-pro"),
        context_window: 200_000,
        max_output_tokens: Some(100_000),
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::None,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── o3 ────────────────────────────────────────────────────────────────────
    // lifecycle: ga | context: 200_000 | max_output: 100_000
    // tool_calling: parallel | structured: json_schema | reasoning: effort
    // prompt_caching: implicit | streaming: server_sent | image_input: true
    // native_agent_features: all null → all false
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("o3"),
        context_window: 200_000,
        max_output_tokens: Some(100_000),
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

    // ── o4-mini ───────────────────────────────────────────────────────────────
    // lifecycle: deprecated | context: 200_000 | max_output: 100_000
    // tool_calling: parallel | structured: json_schema | reasoning: effort
    // prompt_caching: implicit | streaming: server_sent | image_input: true
    // native_agent_features: all null → all false
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("o4-mini"),
        context_window: 200_000,
        max_output_tokens: Some(100_000),
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
        lifecycle: ModelLifecycle::Deprecated,
    });

    // ── o3-deep-research ──────────────────────────────────────────────────────
    // lifecycle: deprecated | context: 200_000 | max_output: 100_000
    // tool_calling: none → None | structured: null → None | reasoning: effort
    // prompt_caching: implicit | streaming: server_sent | image_input: true
    // computer_use: false | code_execution: false | built_in_search: true | file_search: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("o3-deep-research"),
        context_window: 200_000,
        max_output_tokens: Some(100_000),
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::None,
            parallel_tools: false,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Implicit,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet {
                computer_use: false,
                code_execution: false,
                built_in_search: true,
                file_search: true,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Deprecated,
    });

    // ── o4-mini-deep-research ─────────────────────────────────────────────────
    // lifecycle: deprecated | context: 200_000 | max_output: 100_000
    // tool_calling: none → None | structured: null → None | reasoning: effort
    // prompt_caching: implicit | streaming: server_sent | image_input: true
    // computer_use: false | code_execution: false | built_in_search: true | file_search: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("openai"),
        model: ModelId::from("o4-mini-deep-research"),
        context_window: 200_000,
        max_output_tokens: Some(100_000),
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::None,
            parallel_tools: false,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Implicit,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet {
                computer_use: false,
                code_execution: false,
                built_in_search: true,
                file_search: true,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Deprecated,
    });

    out
}

/// Gibt Bootstrap-[`ObservedModelBehavior`]-Einträge für alle OpenAI-Modelle zurück.
///
/// # Beschreibung
/// Für jedes Modell aus [`openai_descriptors`] wird ein Eintrag via
/// [`ObservedModelBehavior::bootstrap`] erzeugt. Alle Scores stehen auf
/// `Score::HALF`, `updated_at` ist `None` und `evidence` ist leer.
/// Die Einträge werden durch echte Harness-Runs befüllt.
///
/// # Rückgabe
/// `Vec<ObservedModelBehavior>` mit genau 17 Einträgen.
///
/// # Concurrency
/// Rein deterministisch, keine Seiteneffekte.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::vendor_openai::{openai_descriptors, openai_observations};
/// let obs = openai_observations();
/// assert_eq!(obs.len(), openai_descriptors().len());
/// ```
pub fn openai_observations() -> Vec<ObservedModelBehavior> {
    crate::observations_from_descriptors(&openai_descriptors())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Jeder Descriptor muss ein positives Kontextfenster haben.
    #[test]
    fn descriptors_have_positive_context_window() {
        for d in openai_descriptors() {
            assert!(d.context_window > 0, "context_window == 0 bei {}", d.model);
        }
    }

    /// Jeder Descriptor muss provider == "openai" haben.
    #[test]
    fn descriptors_have_openai_provider() {
        for d in openai_descriptors() {
            assert_eq!(
                d.provider, "openai",
                "Unerwarteter Provider '{}' bei Modell '{}'",
                d.provider, d.model
            );
        }
    }

    /// Jeder Descriptor muss Modality::Text in seinem ModalitySet enthalten.
    #[test]
    fn all_have_text_modality() {
        for d in openai_descriptors() {
            assert!(
                d.modalities.contains(Modality::Text),
                "Modality::Text fehlt bei {}",
                d.model
            );
        }
    }

    /// Anzahl der Observations muss mit der Anzahl der Descriptors übereinstimmen.
    #[test]
    fn observations_match_descriptor_count() {
        assert_eq!(
            openai_observations().len(),
            openai_descriptors().len(),
            "Observations-Anzahl weicht von Descriptors-Anzahl ab"
        );
    }

    /// Keine zwei Modelle dürfen dieselbe model-ID haben.
    #[test]
    fn all_ids_unique() {
        let descriptors = openai_descriptors();
        let mut seen = std::collections::HashSet::new();
        for d in &descriptors {
            assert!(
                seen.insert(d.model.as_str()),
                "Duplikate Modell-ID gefunden: '{}'",
                d.model
            );
        }
    }

    /// gpt-5.6-sol muss in den Descriptors vorhanden sein (spezifische Assertion).
    #[test]
    fn catalog_contains_gpt_5_6_sol() {
        assert!(
            openai_descriptors()
                .iter()
                .any(|d| d.model == "gpt-5.6-sol"),
            "gpt-5.6-sol fehlt im OpenAI-Katalog"
        );
    }

    /// o3-pro hat kein Streaming (laut JSON streaming: "none").
    #[test]
    fn o3_pro_has_no_streaming() {
        let d = openai_descriptors()
            .into_iter()
            .find(|d| d.model == "o3-pro")
            .expect("o3-pro fehlt im Katalog");
        assert_eq!(
            d.capabilities.streaming,
            StreamingSupport::None,
            "o3-pro sollte kein Streaming haben"
        );
    }

    /// o3-deep-research hat kein Tool-Calling (laut JSON tool_calling: "none").
    #[test]
    fn o3_deep_research_has_no_tool_calling() {
        let d = openai_descriptors()
            .into_iter()
            .find(|d| d.model == "o3-deep-research")
            .expect("o3-deep-research fehlt im Katalog");
        assert_eq!(
            d.capabilities.tool_calling,
            ToolCallingSupport::None,
            "o3-deep-research sollte kein Tool-Calling haben"
        );
    }

    /// Deprecated-Modelle sind korrekt markiert.
    #[test]
    fn deprecated_models_have_correct_lifecycle() {
        let deprecated_ids = ["o4-mini", "o3-deep-research", "o4-mini-deep-research"];
        for d in openai_descriptors() {
            if deprecated_ids.contains(&d.model.as_str()) {
                assert_eq!(
                    d.lifecycle,
                    ModelLifecycle::Deprecated,
                    "'{}' sollte Deprecated sein",
                    d.model
                );
            }
        }
    }

    /// Alle Observations haben Bootstrap-Scores von Score::HALF.
    #[test]
    fn all_observations_have_half_scores() {
        use crate::observed::Score;
        for obs in openai_observations() {
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
}
