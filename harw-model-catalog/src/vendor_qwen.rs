#![allow(clippy::vec_init_then_push)]
//! Alibaba Qwen-Modellkatalog — aus verifizierter Vendor-Research 2026-07-16.
//!
//! Quelle: `docs/research/models/qwen.json`, verifiziert am 2026-07-16.
//!
//! ## Verantwortung
//! Deklariert alle 32 Qwen/Alibaba-Modelle (inkl. Legacy) als
//! [`ModelDescriptor`]-Einträge sowie zugehörige Bootstrap-
//! [`ObservedModelBehavior`]-Einträge (alle Scores auf `Score::HALF`,
//! kein Evidenz-Stand).
//!
//! ## Wichtigste Typen
//! - [`qwen_descriptors`] — liefert 32 `ModelDescriptor`-Einträge laut Vendor-JSON.
//! - [`qwen_observations`] — liefert je einen Bootstrap-Beobachtungs-Eintrag pro Modell.
//!
//! ## Mapping-Regeln
//! - `provider` = `"alibaba"` (Vendor ist Alibaba Cloud, nicht DashScope).
//! - `model` = JSON `id`.
//! - `context_window_tokens: null` → konservativer Fallback `32_768`.
//! - `reasoning_thinking: true` → `ReasoningSupport::Effort`.
//! - `vision/image_input: true` → `Modality::Image` + `image_input: true`.
//! - `audio: true` → `Modality::Audio`.
//! - `video: true` → `Modality::Video`.
//! - `tool_calling: true` → `ToolCallingSupport::Parallel`.
//! - `streaming: true` → `StreamingSupport::ServerSent`.
//! - `status: "active"` / `"active_preview"` → `ModelLifecycle::Ga` / `::Preview`.
//! - `status: "legacy"` / `"being_replaced"` → `ModelLifecycle::Deprecated`.
//! - `mcp_native: true` → `built_in_search: true` (natives Agentic-Feature).
//! - `built_in_tools: ["web_search", ...]` → `built_in_search: true`.
//! - `pricing` = `None` (autoritative Preise leben in `harw-provider`).
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
//! use harw_model_catalog::vendor_qwen::qwen_descriptors;
//! let catalog = qwen_descriptors();
//! assert_eq!(catalog.len(), 32);
//! assert!(catalog.iter().any(|d| d.model == "qwen3.7-max"));
//! ```

use crate::descriptor::{
    AgentFeatureSet, Modality, ModalitySet, ModelCapabilities, ModelDescriptor, ModelId,
    ModelLifecycle, PromptCachingSupport, ProviderId, ReasoningSupport, StreamingSupport,
    StructuredOutputSupport, ToolCallingSupport,
};
use crate::observed::ObservedModelBehavior;

/// Gibt deklarierte [`ModelDescriptor`]-Einträge aller 32 Qwen/Alibaba-Modelle zurück.
///
/// # Beschreibung
/// Alle 32 Einträge werden direkt aus `docs/research/models/qwen.json` (Stand
/// 2026-07-16) abgeschrieben. Es wird kein JSON zur Laufzeit geparst; die Werte
/// sind hardcodiert und mit der Quelldatei verifiziert.
///
/// Modelle mit `context_window_tokens: null` erhalten den konservativen Fallback
/// von `32_768` Tokens.
///
/// # Rückgabe
/// `Vec<ModelDescriptor>` mit genau 32 Einträgen.
///
/// # Concurrency
/// Rein deterministisch, keine Seiteneffekte.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::vendor_qwen::qwen_descriptors;
/// let catalog = qwen_descriptors();
/// assert_eq!(catalog.len(), 32);
/// assert!(catalog.iter().any(|d| d.model == "qwen3.7-max"));
/// assert!(catalog.iter().any(|d| d.model == "qwen3.6-plus"));
/// assert!(catalog.iter().any(|d| d.model == "qwen3-coder-next"));
/// ```
pub fn qwen_descriptors() -> Vec<ModelDescriptor> {
    let mut out = Vec::with_capacity(32);

    // ── qwen3.7-max ──────────────────────────────────────────────────────────
    // status: active | context: 1_000_000 | reasoning: yes | vision: no
    // tool_calling: true | streaming: true | mcp_native: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3.7-max"),
        context_window: 1_000_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Explicit,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet {
                computer_use: false,
                code_execution: false,
                built_in_search: true,
                file_search: false,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3.7-plus ─────────────────────────────────────────────────────────
    // status: active | context: 1_000_000 | reasoning: yes | vision: yes
    // tool_calling: true | streaming: true | mcp_native: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3.7-plus"),
        context_window: 1_000_000,
        max_output_tokens: None,
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
                built_in_search: true,
                file_search: false,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3.6-max-preview ───────────────────────────────────────────────────
    // status: active_preview | context: 1_000_000 | reasoning: yes | vision: no
    // tool_calling: true | streaming: true | mcp_native: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3.6-max-preview"),
        context_window: 1_000_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Explicit,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet {
                computer_use: false,
                code_execution: false,
                built_in_search: true,
                file_search: false,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Preview,
    });

    // ── qwen3.6-plus ─────────────────────────────────────────────────────────
    // status: active | context: 1_000_000 | reasoning: yes | vision: yes
    // tool_calling: true | streaming: true | mcp_native: true
    // built_in_tools: web_search, code_interpreter, web_scraping
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3.6-plus"),
        context_window: 1_000_000,
        max_output_tokens: None,
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
                code_execution: true,
                built_in_search: true,
                file_search: false,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3.6-flash ─────────────────────────────────────────────────────────
    // status: active | context: 1_000_000 | reasoning: yes | vision: no
    // tool_calling: true | streaming: true | mcp_native: true
    // built_in_tools: web_search, code_interpreter, web_scraping
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3.6-flash"),
        context_window: 1_000_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Explicit,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet {
                computer_use: false,
                code_execution: true,
                built_in_search: true,
                file_search: false,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3.6-35b-a3b ──────────────────────────────────────────────────────
    // status: active | context: null → 32_768 | reasoning: yes | vision: no
    // open-weight MoE | tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3.6-35b-a3b"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3.5-plus ─────────────────────────────────────────────────────────
    // status: active | context: 1_000_000 | reasoning: yes | vision: no
    // tool_calling: true | streaming: true | mcp_native: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3.5-plus"),
        context_window: 1_000_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Explicit,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet {
                computer_use: false,
                code_execution: false,
                built_in_search: true,
                file_search: false,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3.5-flash ─────────────────────────────────────────────────────────
    // status: active | context: 1_000_000 | reasoning: yes | vision: no
    // tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3.5-flash"),
        context_window: 1_000_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Explicit,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3.5-omni-plus ─────────────────────────────────────────────────────
    // status: active | context: null → 32_768 | reasoning: no
    // vision: true | audio: true | video: true
    // tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3.5-omni-plus"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![
            Modality::Text,
            Modality::Image,
            Modality::Audio,
            Modality::Video,
        ]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::None,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3.5-omni-flash ────────────────────────────────────────────────────
    // status: active | context: null → 32_768 | reasoning: no
    // vision: true | audio: true | video: true
    // tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3.5-omni-flash"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![
            Modality::Text,
            Modality::Image,
            Modality::Audio,
            Modality::Video,
        ]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::None,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3-omni-flash ──────────────────────────────────────────────────────
    // status: active | context: null → 32_768 | reasoning: yes (thinking_only)
    // vision: true | audio: true | video: true
    // tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-omni-flash"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![
            Modality::Text,
            Modality::Image,
            Modality::Audio,
            Modality::Video,
        ]),
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

    // ── qwen3-max ─────────────────────────────────────────────────────────────
    // status: legacy | context: null → 32_768 | reasoning: yes
    // vision: no | tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-max"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Explicit,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Deprecated,
    });

    // ── qwen-plus ─────────────────────────────────────────────────────────────
    // status: active | context: null → 32_768 | reasoning: yes
    // vision: no | tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen-plus"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Explicit,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen-flash ────────────────────────────────────────────────────────────
    // status: active | context: null → 32_768 | reasoning: yes
    // vision: no | tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen-flash"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Explicit,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen-turbo ────────────────────────────────────────────────────────────
    // status: being_replaced → Deprecated | context: null → 32_768
    // reasoning: no | vision: no | tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen-turbo"),
        context_window: 32_768,
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
        lifecycle: ModelLifecycle::Deprecated,
    });

    // ── qwen3-235b-a22b ───────────────────────────────────────────────────────
    // status: active | context: null → 32_768 | reasoning: yes
    // open-weight MoE | tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-235b-a22b"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3-32b ─────────────────────────────────────────────────────────────
    // status: active | context: null → 32_768 | reasoning: yes
    // open-weight dense | tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-32b"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3-30b-a3b ─────────────────────────────────────────────────────────
    // status: active | context: null → 32_768 | reasoning: yes
    // open-weight MoE | tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-30b-a3b"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3-14b ─────────────────────────────────────────────────────────────
    // status: active | context: not specified → 32_768 | reasoning: yes
    // open-weight dense | tool_calling: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-14b"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::None,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3-8b ──────────────────────────────────────────────────────────────
    // status: active | context: not specified → 32_768 | reasoning: yes
    // open-weight dense | tool_calling: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-8b"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::None,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3-coder-plus ──────────────────────────────────────────────────────
    // status: active | context: 1_000_000 | reasoning: yes
    // tool_calling: true | streaming: true | mcp_native: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-coder-plus"),
        context_window: 1_000_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Explicit,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet {
                computer_use: false,
                code_execution: true,
                built_in_search: true,
                file_search: false,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3-coder-next ──────────────────────────────────────────────────────
    // status: active | context: 256_000 | reasoning: yes
    // open-weight MoE | tool_calling: true | streaming: true | mcp_native: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-coder-next"),
        context_window: 256_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet {
                computer_use: false,
                code_execution: true,
                built_in_search: true,
                file_search: false,
            },
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3-vl-plus ─────────────────────────────────────────────────────────
    // status: active | context: 1_000_000 | reasoning: yes
    // vision: true | video_input: true | gui_agent: true
    // tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-vl-plus"),
        context_window: 1_000_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image, Modality::Video]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Explicit,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3-vl-flash ────────────────────────────────────────────────────────
    // status: active | context: 1_000_000 | reasoning: yes
    // vision: true | video_input: true | gui_agent: true
    // tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-vl-flash"),
        context_window: 1_000_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image, Modality::Video]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::Effort,
            prompt_caching: PromptCachingSupport::Explicit,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3-vl-235b-a22b-instruct ──────────────────────────────────────────
    // status: active | context: 1_000_000 | reasoning: no (non_thinking)
    // open-weight MoE | vision: true | video: true | gui_agent: true
    // tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-vl-235b-a22b-instruct"),
        context_window: 1_000_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image, Modality::Video]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::JsonSchema,
            reasoning: ReasoningSupport::None,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3-vl-235b-a22b-thinking ───────────────────────────────────────────
    // status: active | context: 1_000_000 | reasoning: yes (thinking_only)
    // open-weight MoE | vision: true | video: true | gui_agent: true
    // tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-vl-235b-a22b-thinking"),
        context_window: 1_000_000,
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
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3-vl-32b-instruct ─────────────────────────────────────────────────
    // status: active | context: null → 32_768 | reasoning: no
    // open-weight dense | vision: true | video: true
    // tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-vl-32b-instruct"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image, Modality::Video]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::None,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3-vl-30b-a3b-instruct ────────────────────────────────────────────
    // status: active | context: not specified → 32_768 | reasoning: no
    // open-weight MoE | vision: true | video: true
    // tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-vl-30b-a3b-instruct"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image, Modality::Video]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::None,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3-vl-30b-a3b-thinking ─────────────────────────────────────────────
    // status: active | context: not specified → 32_768 | reasoning: yes (thinking_only)
    // open-weight MoE | vision: true | video: true
    // tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-vl-30b-a3b-thinking"),
        context_window: 32_768,
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

    // ── qwen3-vl-8b-instruct ──────────────────────────────────────────────────
    // status: active | context: not specified → 32_768 | reasoning: no
    // open-weight dense | vision: true | video_input: false | tool_calling: false
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-vl-8b-instruct"),
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
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen3-vl-8b-thinking ──────────────────────────────────────────────────
    // status: active | context: not specified → 32_768 | reasoning: yes (thinking_only)
    // open-weight dense | vision: true | video_input: false | tool_calling: false
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen3-vl-8b-thinking"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::None,
            parallel_tools: false,
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

    // ── qwq-plus ──────────────────────────────────────────────────────────────
    // status: active | context: null → 32_768 | reasoning: yes (thinking_only)
    // vision: no | tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwq-plus"),
        context_window: 32_768,
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
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen-long-latest ──────────────────────────────────────────────────────
    // status: active | context: 10_000_000 | reasoning: no
    // vision: no | tool_calling: false | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen-long-latest"),
        context_window: 10_000_000,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::None,
            parallel_tools: false,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::None,
            prompt_caching: PromptCachingSupport::Implicit,
            streaming: StreamingSupport::ServerSent,
            image_input: false,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Ga,
    });

    // ── text-embedding-v4 ─────────────────────────────────────────────────────
    // status: active | context: not specified → 32_768 | reasoning: no
    // embedding model; no tool_calling, no streaming in generative sense
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("text-embedding-v4"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text]),
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
        lifecycle: ModelLifecycle::Ga,
    });

    // ── qwen-max ──────────────────────────────────────────────────────────────
    // status: legacy → Deprecated | context: 32_000 | reasoning: no
    // vision: no | tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen-max"),
        context_window: 32_000,
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
        lifecycle: ModelLifecycle::Deprecated,
    });

    // ── qwen2.5-coder ─────────────────────────────────────────────────────────
    // status: legacy → Deprecated | context: not specified → 32_768
    // reasoning: no | vision: no | tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen2.5-coder"),
        context_window: 32_768,
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
        lifecycle: ModelLifecycle::Deprecated,
    });

    // ── qwen2.5-vl-72b-instruct ───────────────────────────────────────────────
    // status: legacy → Deprecated | context: not specified → 32_768
    // reasoning: no | vision: true | video: true
    // tool_calling: true | streaming: true
    out.push(ModelDescriptor {
        provider: ProviderId::from("alibaba"),
        model: ModelId::from("qwen2.5-vl-72b-instruct"),
        context_window: 32_768,
        max_output_tokens: None,
        modalities: ModalitySet::new(vec![Modality::Text, Modality::Image, Modality::Video]),
        capabilities: ModelCapabilities {
            tool_calling: ToolCallingSupport::Parallel,
            parallel_tools: true,
            structured_output: StructuredOutputSupport::None,
            reasoning: ReasoningSupport::None,
            prompt_caching: PromptCachingSupport::None,
            streaming: StreamingSupport::ServerSent,
            image_input: true,
            native_agent_features: AgentFeatureSet::default(),
        },
        pricing: None,
        lifecycle: ModelLifecycle::Deprecated,
    });

    out
}

/// Gibt Bootstrap-[`ObservedModelBehavior`]-Einträge für alle Qwen/Alibaba-Modelle zurück.
///
/// # Beschreibung
/// Für jedes Modell aus [`qwen_descriptors`] wird ein Eintrag via
/// [`ObservedModelBehavior::bootstrap`] erzeugt. Alle Scores stehen auf
/// `Score::HALF`, `updated_at` ist `None` und `evidence` ist leer.
/// Die Einträge werden durch echte Harness-Runs befüllt.
///
/// # Rückgabe
/// `Vec<ObservedModelBehavior>` mit genau 32 Einträgen.
///
/// # Concurrency
/// Rein deterministisch, keine Seiteneffekte.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::vendor_qwen::{qwen_descriptors, qwen_observations};
/// let obs = qwen_observations();
/// assert_eq!(obs.len(), qwen_descriptors().len());
/// ```
pub fn qwen_observations() -> Vec<ObservedModelBehavior> {
    crate::observations_from_descriptors(&qwen_descriptors())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observed::Score;

    /// Substantielle Qwen-Modell-Menge muss vorhanden sein.
    #[test]
    fn descriptor_count_is_substantial() {
        let n = qwen_descriptors().len();
        assert!(
            n >= 20,
            "qwen_descriptors muss mindestens 20 Einträge liefern, aktuell {n}"
        );
    }

    /// Jeder Descriptor muss provider == "alibaba" haben.
    #[test]
    fn all_have_alibaba_provider() {
        for d in qwen_descriptors() {
            assert_eq!(
                d.provider, "alibaba",
                "Unerwarteter Provider '{}' bei Modell '{}'",
                d.provider, d.model
            );
        }
    }

    /// Jeder Descriptor muss Modality::Text enthalten.
    #[test]
    fn all_have_text_modality() {
        for d in qwen_descriptors() {
            assert!(
                d.modalities.contains(Modality::Text),
                "Modality::Text fehlt bei {}",
                d.model
            );
        }
    }

    /// Jedes Kontextfenster muss > 0 sein.
    #[test]
    fn all_have_positive_context_window() {
        for d in qwen_descriptors() {
            assert!(d.context_window > 0, "context_window == 0 bei {}", d.model);
        }
    }

    /// Keine zwei Modelle dürfen dieselbe model-ID haben.
    #[test]
    fn all_ids_unique() {
        let descriptors = qwen_descriptors();
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
            qwen_observations().len(),
            qwen_descriptors().len(),
            "Observations-Anzahl weicht von Descriptors-Anzahl ab"
        );
    }

    /// Alle Observations haben Bootstrap-Scores von Score::HALF.
    #[test]
    fn all_observations_have_half_scores() {
        for obs in qwen_observations() {
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

    /// Spezifische Assertion: qwen3.7-max ist enthalten (§ Brief).
    #[test]
    fn catalog_contains_qwen3_7_max() {
        assert!(
            qwen_descriptors().iter().any(|d| d.model == "qwen3.7-max"),
            "qwen3.7-max fehlt im Qwen-Katalog"
        );
    }

    /// Spezifische Assertion: qwen3.6-plus ist enthalten (§ Brief).
    #[test]
    fn catalog_contains_qwen3_6_plus() {
        assert!(
            qwen_descriptors().iter().any(|d| d.model == "qwen3.6-plus"),
            "qwen3.6-plus fehlt im Qwen-Katalog"
        );
    }

    /// Spezifische Assertion: qwen3-coder-next ist enthalten (§ Brief).
    #[test]
    fn catalog_contains_qwen3_coder_next() {
        assert!(
            qwen_descriptors()
                .iter()
                .any(|d| d.model == "qwen3-coder-next"),
            "qwen3-coder-next fehlt im Qwen-Katalog"
        );
    }

    /// Legacy-Modelle müssen als Deprecated markiert sein.
    #[test]
    fn legacy_models_are_deprecated() {
        let deprecated_ids = [
            "qwen3-max",
            "qwen-turbo",
            "qwen-max",
            "qwen2.5-coder",
            "qwen2.5-vl-72b-instruct",
        ];
        for d in qwen_descriptors() {
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

    /// qwen3.6-max-preview muss als Preview markiert sein.
    #[test]
    fn preview_model_has_preview_lifecycle() {
        let d = qwen_descriptors()
            .into_iter()
            .find(|d| d.model == "qwen3.6-max-preview")
            .expect("qwen3.6-max-preview fehlt im Katalog");
        assert_eq!(
            d.lifecycle,
            ModelLifecycle::Preview,
            "qwen3.6-max-preview sollte Preview-Lifecycle haben"
        );
    }

    /// qwen-long-latest hat das größte Kontextfenster (10M).
    #[test]
    fn qwen_long_has_10m_context() {
        let d = qwen_descriptors()
            .into_iter()
            .find(|d| d.model == "qwen-long-latest")
            .expect("qwen-long-latest fehlt im Katalog");
        assert_eq!(
            d.context_window, 10_000_000,
            "qwen-long-latest sollte 10M Kontext haben"
        );
    }

    /// Modelle mit image_input=true müssen Modality::Image enthalten.
    #[test]
    fn image_models_have_image_modality() {
        for d in qwen_descriptors() {
            if d.capabilities.image_input {
                assert!(
                    d.modalities.contains(Modality::Image),
                    "image_input=true, aber Modality::Image fehlt bei {}",
                    d.model
                );
            }
        }
    }

    /// Omni-Modelle müssen Audio- und Video-Modalität haben.
    #[test]
    fn omni_models_have_audio_and_video() {
        let omni_ids = [
            "qwen3.5-omni-plus",
            "qwen3.5-omni-flash",
            "qwen3-omni-flash",
        ];
        for d in qwen_descriptors() {
            if omni_ids.contains(&d.model.as_str()) {
                assert!(
                    d.modalities.contains(Modality::Audio),
                    "Modality::Audio fehlt bei {}",
                    d.model
                );
                assert!(
                    d.modalities.contains(Modality::Video),
                    "Modality::Video fehlt bei {}",
                    d.model
                );
            }
        }
    }

    /// text-embedding-v4 hat kein Tool-Calling und kein Streaming.
    #[test]
    fn embedding_model_has_no_tool_calling() {
        let d = qwen_descriptors()
            .into_iter()
            .find(|d| d.model == "text-embedding-v4")
            .expect("text-embedding-v4 fehlt im Katalog");
        assert_eq!(
            d.capabilities.tool_calling,
            ToolCallingSupport::None,
            "text-embedding-v4 sollte kein Tool-Calling haben"
        );
        assert_eq!(
            d.capabilities.streaming,
            StreamingSupport::None,
            "text-embedding-v4 sollte kein Streaming haben"
        );
    }
}
