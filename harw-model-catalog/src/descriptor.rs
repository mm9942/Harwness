//! Layer 2 — Deklarierte technische Modellfähigkeiten.
//!
//! Dieses Modul implementiert §3 des Design-Dokuments `docs/design/model-catalog-v2.md`.
//!
//! ## Verantwortung
//! - Definition aller Typen, die beschreiben, was ein Provider über sein Modell *behauptet*.
//! - Bereitstellung einer kuratierten Bootstrap-Liste mit mindestens 15 Modellen.
//!
//! ## Wichtigste Typen
//! - [`ModelDescriptor`] — vollständige Beschreibung eines Modells (Layer 2)
//! - [`ModelCapabilities`] — deklarierte Fähigkeiten
//! - [`ModalitySet`] — Menge unterstützter Modalitäten
//! - [`ModelLifecycle`] — Lebenszyklus-Status
//!
//! ## Nebenläufigkeitsmodell
//! Alle Typen sind `Clone + Send + Sync`-kompatibel; keine Locks, keine Threads.
//!
//! ## Fehlertypen
//! Dieses Modul ist infallibel; alle Konstruktoren sind deterministisch.
//!
//! ## Beispiel
//! ```rust,no_run
//! use harw_model_catalog::descriptor::bootstrap_descriptors;
//! let catalog = bootstrap_descriptors();
//! assert!(catalog.len() >= 15);
//! ```

use serde::{Deserialize, Serialize};

/// Opaker Token-Zähler (Einheit: Tokens, 32-Bit-Ganzzahl).
pub type TokenCount = u32;

/// Opake Provider-Kennung, z. B. `"anthropic"` oder `"openai"`.
pub use harw_types::ProviderId;

/// Opake Modell-Kennung, z. B. `"claude-opus-4-8"`.
pub use harw_types::ModelId;

/// Eine einzelne Modalität, die ein Modell als Ein- oder Ausgabe verarbeiten kann.
///
/// # Beschreibung
/// Modalitäten beschreiben, welche Datenformen ein Modell nativ versteht.
/// `Text` ist immer enthalten. Weitere Modalitäten werden vom Provider deklariert.
///
/// # Serialisierung
/// Jeder Variant wird in `snake_case` serialisiert (z. B. `"text"`, `"image"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    /// Verarbeitet reinen Text (immer vorhanden).
    Text,
    /// Verarbeitet Bilddaten (PNG, JPEG, etc.).
    Image,
    /// Verarbeitet Audiodaten (WAV, MP3, etc.).
    Audio,
    /// Verarbeitet Videodaten.
    Video,
    /// Verarbeitet PDF-Dokumente.
    Pdf,
}

/// Geordnete Menge von Modalitäten, die ein Modell unterstützt.
///
/// # Beschreibung
/// Transparent serialisiert als JSON-Array. Jedes Modell muss mindestens
/// [`Modality::Text`] enthalten.
///
/// # Serialisierung
/// Transparent: `["text", "image"]`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModalitySet(pub Vec<Modality>);

impl ModalitySet {
    /// Erstellt ein neues `ModalitySet` mit den angegebenen Modalitäten.
    ///
    /// # Argumente
    /// - `modalities` (`Vec<Modality>`): Liste der unterstützten Modalitäten.
    ///
    /// # Rückgabe
    /// Ein neues `ModalitySet`.
    pub fn new(modalities: Vec<Modality>) -> Self {
        Self(modalities)
    }

    /// Gibt `true` zurück, wenn die angegebene Modalität enthalten ist.
    ///
    /// # Argumente
    /// - `m` (`Modality`): Die zu prüfende Modalität.
    pub fn contains(&self, m: Modality) -> bool {
        self.0.contains(&m)
    }
}

/// Grad der Tool-Calling-Unterstützung laut Provider-Deklaration.
///
/// # Varianten
/// - `None`: Kein Tool-Calling unterstützt.
/// - `Basic`: Einfaches Tool-Calling (ein Tool pro Turn).
/// - `Parallel`: Mehrere Tools werden parallel aufgerufen.
/// - `Native`: Natives Tool-Calling tief im Modell verankert.
///
/// # Serialisierung
/// `snake_case`: `"none"`, `"basic"`, `"parallel"`, `"native"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallingSupport {
    /// Kein Tool-Calling.
    None,
    /// Einfaches Tool-Calling (sequenziell, ein Tool pro Schritt).
    Basic,
    /// Paralleles Tool-Calling (mehrere Tools gleichzeitig).
    Parallel,
    /// Natives Tool-Calling, tief im Modell implementiert.
    Native,
}

/// Unterstützung für strukturierte Ausgaben laut Provider-Deklaration.
///
/// # Serialisierung
/// `snake_case`: `"none"`, `"json_mode"`, `"json_schema"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredOutputSupport {
    /// Keine strukturierte Ausgabe.
    None,
    /// JSON-Modus (kein Schema-Enforcement).
    JsonMode,
    /// Vollständige JSON-Schema-Validierung.
    JsonSchema,
}

/// Unterstützung für explizite Reasoning-/Denk-Stufen.
///
/// # Varianten
/// - `None`: Kein Reasoning-Modus.
/// - `Effort`: Effort-basiertes Reasoning (z. B. OpenAI `reasoning_effort`).
/// - `Trace`: Sichtbarer Reasoning-Trace in der Antwort.
///
/// # Serialisierung
/// `snake_case`: `"none"`, `"effort"`, `"trace"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningSupport {
    /// Kein Reasoning-Modus.
    None,
    /// Effort-basiertes Reasoning (konfigurierbar per `reasoning_effort`).
    Effort,
    /// Sichtbarer Reasoning-Trace (z. B. DeepSeek-Reasoner).
    Trace,
}

/// Unterstützung für Prompt-Caching laut Provider-Deklaration.
///
/// # Varianten
/// - `None`: Kein Prompt-Caching.
/// - `Implicit`: Cache wird automatisch vom Provider verwaltet.
/// - `Explicit`: Cache-Steuerung durch explizite Breakpoints im Request.
///
/// # Serialisierung
/// `snake_case`: `"none"`, `"implicit"`, `"explicit"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptCachingSupport {
    /// Kein Prompt-Caching.
    None,
    /// Implizites Caching (automatisch durch Provider-Infrastruktur).
    Implicit,
    /// Explizites Caching (Breakpoints im API-Request erforderlich).
    Explicit,
}

/// Unterstützung für Token-Streaming laut Provider-Deklaration.
///
/// # Varianten
/// - `None`: Kein Streaming.
/// - `ServerSent`: Server-Sent Events (SSE) Streaming.
///
/// # Serialisierung
/// `snake_case`: `"none"`, `"server_sent"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamingSupport {
    /// Kein Streaming; nur vollständige Antworten.
    None,
    /// Server-Sent Events (SSE) Streaming.
    ServerSent,
}

/// Nativer Agent-Feature-Satz, den der Provider für dieses Modell bereitstellt.
///
/// # Beschreibung
/// Enthält boolesche Flags für native Agenten-Fähigkeiten, die direkt in der
/// Provider-API verfügbar sind — ohne zusätzliche Tool-Konfiguration.
///
/// # Felder
/// - `computer_use`: Modell kann Desktops/Browser steuern (z. B. Anthropic Computer Use).
/// - `code_execution`: Modell kann Code direkt ausführen.
/// - `built_in_search`: Modell hat integrierten Websearch-Zugriff.
/// - `file_search`: Modell kann in hochgeladenen Dokumenten suchen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentFeatureSet {
    /// Natives Computer-Use-Feature (Desktop/Browser-Steuerung).
    pub computer_use: bool,
    /// Natives Code-Execution-Feature.
    pub code_execution: bool,
    /// Eingebauter Websearch-Zugriff.
    pub built_in_search: bool,
    /// Eingebaute Dokumentensuche.
    pub file_search: bool,
}

/// Vollständige Fähigkeiten eines Modells laut Provider-Deklaration (Layer 2).
///
/// # Beschreibung
/// Aggregiert alle technischen Fähigkeiten in einem kompakten, kopierbaren Struct.
/// Werte stammen aus Provider-Dokumentation und werden nicht gemessen.
///
/// # Felder
/// - `tool_calling` — Grad der Tool-Calling-Unterstützung.
/// - `parallel_tools` — Ob mehrere Tools gleichzeitig gerufen werden können.
/// - `structured_output` — Modus für strukturierte Ausgaben.
/// - `reasoning` — Reasoning-Modus.
/// - `prompt_caching` — Caching-Modus.
/// - `streaming` — Streaming-Modus.
/// - `image_input` — Ob das Modell Bilder als Eingabe akzeptiert.
/// - `native_agent_features` — Nativer Agent-Feature-Satz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Grad der Tool-Calling-Unterstützung.
    pub tool_calling: ToolCallingSupport,
    /// Ob parallele Tool-Calls unterstützt werden.
    pub parallel_tools: bool,
    /// Modus für strukturierte Ausgaben.
    pub structured_output: StructuredOutputSupport,
    /// Reasoning-Modus.
    pub reasoning: ReasoningSupport,
    /// Prompt-Caching-Modus.
    pub prompt_caching: PromptCachingSupport,
    /// Streaming-Modus.
    pub streaming: StreamingSupport,
    /// Ob das Modell Bildeingaben unterstützt.
    pub image_input: bool,
    /// Nativer Agent-Feature-Satz.
    pub native_agent_features: AgentFeatureSet,
}

/// Lebenszyklus-Status eines Modells.
///
/// # Varianten
/// - `Preview`: Vorschau, kann sich ändern.
/// - `Ga`: Generally Available, stabil.
/// - `Deprecated`: Veraltet, wird entfernt.
/// - `Retired`: Nicht mehr verfügbar.
///
/// # Serialisierung
/// `snake_case`: `"preview"`, `"ga"`, `"deprecated"`, `"retired"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelLifecycle {
    /// Vorschau-Status; API kann sich ohne Vorankündigung ändern.
    Preview,
    /// Generally Available; stabiler Produktionsstatus.
    Ga,
    /// Veraltet; wird in einer zukünftigen Version entfernt.
    Deprecated,
    /// Eingestellt; nicht mehr erreichbar.
    Retired,
}

/// Preisinformation eines Modells in USD pro Million Tokens.
///
/// # Beschreibung
/// Preise werden in `harw-provider` verwaltet; dieser Typ dient nur als
/// optionaler Hinweis in Layer 2. Preise können sich jederzeit ändern.
///
/// # Felder
/// - `input_per_mtoken_usd` — Preis pro Million Input-Tokens in USD.
/// - `output_per_mtoken_usd` — Preis pro Million Output-Tokens in USD.
/// - `cached_input_per_mtoken_usd` — Preis für gecachte Input-Tokens (optional).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Pricing {
    /// Preis pro Million Input-Tokens in USD.
    pub input_per_mtoken_usd: f32,
    /// Preis pro Million Output-Tokens in USD.
    pub output_per_mtoken_usd: f32,
    /// Preis für gecachte Input-Tokens in USD pro Million (falls zutreffend).
    pub cached_input_per_mtoken_usd: Option<f32>,
}

/// Vollständige Modellbeschreibung (Layer 2) — was der Provider über das Modell behauptet.
///
/// # Beschreibung
/// `ModelDescriptor` ist der zentrale Layer-2-Typ. Er enthält ausschließlich
/// Provider-deklarierte Eigenschaften — keine gemessenen Werte (→ Layer 4) und
/// keine Runtime-Policies (→ Layer 3).
///
/// Preise (`pricing`) sind optional; autoritative Preise leben in `harw-provider`.
///
/// # Felder
/// - `provider` — Provider-Kennung (z. B. `"anthropic"`).
/// - `model` — Modell-Kennung (z. B. `"claude-opus-4-8"`).
/// - `context_window` — Maximales Kontextfenster in Tokens.
/// - `max_output_tokens` — Maximale Output-Tokens pro Request (falls deklariert).
/// - `modalities` — Unterstützte Modalitäten.
/// - `capabilities` — Deklarierte technische Fähigkeiten.
/// - `pricing` — Optionale Preisinformation (nicht autoritativ).
/// - `lifecycle` — Lebenszyklus-Status.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::descriptor::{
///     ModelDescriptor, ModelCapabilities, ModalitySet, Modality,
///     ToolCallingSupport, StructuredOutputSupport, ReasoningSupport,
///     PromptCachingSupport, StreamingSupport, AgentFeatureSet, ModelLifecycle,
/// };
/// use harw_types::{ProviderId, ModelId};
/// let d = ModelDescriptor {
///     provider: ProviderId::from("openai"),
///     model: ModelId::from("gpt-4o"),
///     context_window: 128_000,
///     max_output_tokens: Some(8192),
///     modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
///     capabilities: ModelCapabilities {
///         tool_calling: ToolCallingSupport::Parallel,
///         parallel_tools: true,
///         structured_output: StructuredOutputSupport::JsonSchema,
///         reasoning: ReasoningSupport::None,
///         prompt_caching: PromptCachingSupport::Implicit,
///         streaming: StreamingSupport::ServerSent,
///         image_input: true,
///         native_agent_features: AgentFeatureSet::default(),
///     },
///     pricing: None,
///     lifecycle: ModelLifecycle::Ga,
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelDescriptor {
    /// Provider-Kennung (z. B. `"anthropic"`, `"openai"`).
    pub provider: ProviderId,
    /// Modell-Kennung (z. B. `"claude-opus-4-8"`).
    pub model: ModelId,
    /// Maximale Kontextgröße in Tokens (laut Provider-Dokumentation).
    pub context_window: TokenCount,
    /// Maximale Output-Tokens pro Request (laut Provider-Dokumentation, falls deklariert).
    pub max_output_tokens: Option<TokenCount>,
    /// Unterstützte Modalitäten (mindestens `Modality::Text`).
    pub modalities: ModalitySet,
    /// Deklarierte technische Fähigkeiten.
    pub capabilities: ModelCapabilities,
    /// Optionale Preisinformation. Preise sind nicht autoritativ — autoritative
    /// Preise werden in `harw-provider` verwaltet (Quelle: Provider-Pricing-Seiten).
    pub pricing: Option<Pricing>,
    /// Lebenszyklus-Status des Modells.
    pub lifecycle: ModelLifecycle,
}

/// Erstellt eine kuratierte Bootstrap-Liste mit mindestens 15 `ModelDescriptor`-Einträgen.
///
/// # Beschreibung
/// Alle Werte basieren auf Provider-Dokumentation, Stand 2026-07. Sie sind konservativ
/// gewählt und spiegeln deklarierte (nicht gemessene) Fähigkeiten wider.
///
/// Autoritative Preise leben in `harw-provider`; `pricing` ist hier `None`.
///
/// # Rückgabe
/// `Vec<ModelDescriptor>` mit mindestens 15 kuratierten Einträgen.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::descriptor::bootstrap_descriptors;
/// let catalog = bootstrap_descriptors();
/// assert!(catalog.len() >= 15);
/// ```
pub fn bootstrap_descriptors() -> Vec<ModelDescriptor> {
    let mut all = Vec::new();
    all.extend(crate::vendor_openai::openai_descriptors());
    all.extend(crate::vendor_anthropic::anthropic_descriptors());
    all.extend(crate::vendor_mistral::mistral_descriptors());
    all.extend(crate::vendor_zai::zai_descriptors());
    all.extend(crate::vendor_moonshot::moonshot_descriptors());
    all.extend(crate::vendor_qwen::qwen_descriptors());
    all.extend(crate::vendor_meta::meta_descriptors());
    all.extend(crate::vendor_xai::xai_descriptors());
    all
}

// Alt-Konservendaten, ausschließlich für interne Konsistenz-Tests bewahrt.
// Wird nicht öffentlich exportiert; die Produktions-Bootstrap-Funktion oben
// nutzt die verifizierten Vendor-Files.
#[cfg(test)]
#[allow(dead_code)]
fn legacy_hardcoded_descriptors() -> Vec<ModelDescriptor> {
    vec![
        // Quelle: Provider-Doku, Stand 2026-07 — openai.com/api/pricing
        ModelDescriptor {
            provider: ProviderId::from("openai"),
            model: ModelId::from("gpt-5"),
            context_window: 400_000,
            max_output_tokens: Some(16_384),
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
                    code_execution: false,
                    built_in_search: true,
                    file_search: false,
                },
            },
            pricing: None, // autoritative Preise in harw-provider
            lifecycle: ModelLifecycle::Ga,
        },
        // Quelle: Provider-Doku, Stand 2026-07 — openai.com/api/pricing
        ModelDescriptor {
            provider: ProviderId::from("openai"),
            model: ModelId::from("gpt-4o"),
            context_window: 128_000,
            max_output_tokens: Some(8_192),
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
        },
        // Quelle: Provider-Doku, Stand 2026-07 — openai.com/api/pricing
        ModelDescriptor {
            provider: ProviderId::from("openai"),
            model: ModelId::from("o4-mini"),
            context_window: 200_000,
            max_output_tokens: Some(8_192),
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
                    code_execution: false,
                    built_in_search: true,
                    file_search: false,
                },
            },
            pricing: None,
            lifecycle: ModelLifecycle::Ga,
        },
        // Quelle: Provider-Doku, Stand 2026-07 — anthropic.com/api/pricing
        ModelDescriptor {
            provider: ProviderId::from("anthropic"),
            model: ModelId::from("claude-opus-4-8"),
            context_window: 200_000,
            max_output_tokens: Some(64_000),
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
            lifecycle: ModelLifecycle::Ga,
        },
        // Quelle: Provider-Doku, Stand 2026-07 — anthropic.com/api/pricing
        ModelDescriptor {
            provider: ProviderId::from("anthropic"),
            model: ModelId::from("claude-sonnet-5"),
            context_window: 1_000_000,
            max_output_tokens: Some(64_000),
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
            lifecycle: ModelLifecycle::Ga,
        },
        // Quelle: Provider-Doku, Stand 2026-07 — anthropic.com/api/pricing
        ModelDescriptor {
            provider: ProviderId::from("anthropic"),
            model: ModelId::from("claude-haiku-4-5-20251001"),
            context_window: 200_000,
            max_output_tokens: Some(64_000),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonSchema,
                reasoning: ReasoningSupport::None,
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
            lifecycle: ModelLifecycle::Ga,
        },
        // Quelle: Provider-Doku, Stand 2026-07 — api.z.ai (Zhipu AI)
        ModelDescriptor {
            provider: ProviderId::from("zai"),
            model: ModelId::from("glm-4.6"),
            context_window: 1_000_000,
            max_output_tokens: Some(8_192),
            modalities: ModalitySet::new(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Parallel,
                parallel_tools: true,
                structured_output: StructuredOutputSupport::JsonMode,
                reasoning: ReasoningSupport::Effort,
                prompt_caching: PromptCachingSupport::Implicit,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: None,
            lifecycle: ModelLifecycle::Ga,
        },
        // Quelle: Provider-Doku, Stand 2026-07 — platform.moonshot.ai (Kimi)
        ModelDescriptor {
            provider: ProviderId::from("moonshot"),
            model: ModelId::from("kimi-k2-turbo-preview"),
            context_window: 262_144,
            max_output_tokens: Some(8_192),
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
            lifecycle: ModelLifecycle::Preview,
        },
        // Quelle: Provider-Doku, Stand 2026-07 — dashscope.aliyuncs.com (Qwen)
        ModelDescriptor {
            provider: ProviderId::from("dashscope"),
            model: ModelId::from("qwen3-max"),
            context_window: 262_144,
            max_output_tokens: Some(8_192),
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
        },
        // Quelle: Provider-Doku, Stand 2026-07 — dashscope.aliyuncs.com (Qwen)
        ModelDescriptor {
            provider: ProviderId::from("dashscope"),
            model: ModelId::from("qwen3-coder-plus"),
            context_window: 262_144,
            max_output_tokens: Some(8_192),
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
            pricing: None,
            lifecycle: ModelLifecycle::Ga,
        },
        // Quelle: Provider-Doku, Stand 2026-07 — mistral.ai/technology/api
        ModelDescriptor {
            provider: ProviderId::from("mistral"),
            model: ModelId::from("mistral-large-2411"),
            context_window: 131_072,
            max_output_tokens: Some(8_192),
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
            pricing: None,
            lifecycle: ModelLifecycle::Ga,
        },
        // Quelle: Provider-Doku, Stand 2026-07 — x.ai/api (xAI Grok)
        ModelDescriptor {
            provider: ProviderId::from("xai"),
            model: ModelId::from("grok-4.5"),
            context_window: 500_000,
            max_output_tokens: Some(8_192),
            modalities: ModalitySet::new(vec![Modality::Text, Modality::Image]),
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
        },
        // Quelle: Provider-Doku, Stand 2026-07 — platform.deepseek.com
        ModelDescriptor {
            provider: ProviderId::from("deepseek"),
            model: ModelId::from("deepseek-chat"),
            context_window: 131_072,
            max_output_tokens: Some(8_192),
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
            lifecycle: ModelLifecycle::Ga,
        },
        // Quelle: Provider-Doku, Stand 2026-07 — platform.deepseek.com
        ModelDescriptor {
            provider: ProviderId::from("deepseek"),
            model: ModelId::from("deepseek-reasoner"),
            context_window: 131_072,
            max_output_tokens: Some(8_192),
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
            lifecycle: ModelLifecycle::Ga,
        },
        // Quelle: Provider-Doku, Stand 2026-07 — console.groq.com
        ModelDescriptor {
            provider: ProviderId::from("groq"),
            model: ModelId::from("llama-3.3-70b-versatile"),
            context_window: 131_072,
            max_output_tokens: Some(8_192),
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
            pricing: None,
            lifecycle: ModelLifecycle::Ga,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_has_at_least_15() {
        // Prüft, dass die Bootstrap-Liste mindestens 15 Einträge enthält (§3 Design-Doc).
        assert!(
            bootstrap_descriptors().len() >= 15,
            "bootstrap_descriptors muss mindestens 15 Einträge liefern"
        );
    }

    #[test]
    fn bootstrap_all_have_nonempty_ids() {
        // Kein Descriptor darf leere provider- oder model-Felder haben.
        for d in bootstrap_descriptors() {
            assert!(!d.provider.is_empty(), "Leerer Provider gefunden: {:?}", d);
            assert!(!d.model.is_empty(), "Leeres Model gefunden: {:?}", d);
        }
    }

    #[test]
    fn bootstrap_all_have_positive_context_window() {
        // Jedes Kontextfenster muss > 0 sein.
        for d in bootstrap_descriptors() {
            assert!(
                d.context_window > 0,
                "context_window == 0 für {}:{}",
                d.provider,
                d.model
            );
        }
    }

    #[test]
    fn bootstrap_all_have_text_modality() {
        // Jeder Descriptor muss Modality::Text in seinen Modalitäten enthalten.
        for d in bootstrap_descriptors() {
            assert!(
                d.modalities.contains(Modality::Text),
                "Modality::Text fehlt bei {}:{}",
                d.provider,
                d.model
            );
        }
    }

    #[test]
    fn enums_serde_snake_case() {
        // ToolCallingSupport::Parallel wird als "parallel" serialisiert.
        let json = serde_json::to_string(&ToolCallingSupport::Parallel)
            .expect("Serialisierung fehlgeschlagen");
        assert_eq!(json, "\"parallel\"");

        let json = serde_json::to_string(&ReasoningSupport::Effort)
            .expect("Serialisierung fehlgeschlagen");
        assert_eq!(json, "\"effort\"");

        let json = serde_json::to_string(&PromptCachingSupport::Explicit)
            .expect("Serialisierung fehlgeschlagen");
        assert_eq!(json, "\"explicit\"");

        let json = serde_json::to_string(&StreamingSupport::ServerSent)
            .expect("Serialisierung fehlgeschlagen");
        assert_eq!(json, "\"server_sent\"");

        let json =
            serde_json::to_string(&ModelLifecycle::Ga).expect("Serialisierung fehlgeschlagen");
        assert_eq!(json, "\"ga\"");
    }

    #[test]
    fn descriptor_roundtrip() {
        // Serde-JSON Roundtrip eines vollständigen ModelDescriptor.
        let original = bootstrap_descriptors()
            .into_iter()
            .next()
            .expect("Mindestens ein Descriptor erwartet");
        let json = serde_json::to_string(&original).expect("Serialisierung fehlgeschlagen");
        let restored: ModelDescriptor =
            serde_json::from_str(&json).expect("Deserialisierung fehlgeschlagen");
        assert_eq!(original, restored);
    }

    #[test]
    fn image_models_have_image_modality() {
        // Jeder Descriptor mit image_input=true muss Modality::Image in modalities haben.
        for d in bootstrap_descriptors() {
            if d.capabilities.image_input {
                assert!(
                    d.modalities.contains(Modality::Image),
                    "image_input=true, aber Modality::Image fehlt bei {}:{}",
                    d.provider,
                    d.model
                );
            }
        }
    }
}
