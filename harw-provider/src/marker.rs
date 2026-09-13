//! Marker-Typen (ZSTs) und Marker-Traits — die Compile-Zeit-Schicht.
//!
//! Note 12 §1, §9, §12. Diese Typen tragen **die Form** (welcher Provider,
//! welche Rolle, welche Auth, welcher Zustand) zur Compile-Zeit. Der Wert
//! (Key, base_url) lebt zur Runtime in der Registry. Der Marker beweist die
//! Registrierung *nicht* — das bleibt eine `Result`-Frage.

use serde::{Deserialize, Serialize};

macro_rules! marker_zst {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
        pub struct $name;
    };
}

// ===== Role / Priority tags =====
marker_zst!(
    /// Rollen-Tag: genau ein Primary pro Registry (Invariante).
    Primary
);
marker_zst!(
    /// Rollen-Tag: alle übrigen Provider sind Secondary.
    Secondary
);

// ===== Provider kind tags =====
marker_zst!(
    /// OpenAI-kompatibles Wire-Format (`/responses`).
    OpenAiCompat
);
marker_zst!(
    /// Anthropic-kompatibles Wire-Format (späterer Wave).
    AnthropicCompat
);
marker_zst!(
    /// Lokale Runtime (z.B. ollama), häufig `NoAuth`.
    LocalRuntime
);
marker_zst!(
    /// Frei definierter Provider-Typ.
    CustomProvider
);

// ===== Auth tags =====
marker_zst!(
    /// API-Key aus ENV oder Config.
    ApiKeyAuth
);
marker_zst!(
    /// Statisch gesetztes Bearer-Token.
    StaticBearerAuth
);
#[cfg(feature = "chatgpt-oauth")]
marker_zst!(
    /// ChatGPT-OAuth (nur unter Feature `chatgpt-oauth`).
    ChatGptOAuthAuth
);
marker_zst!(
    /// Kein Auth-Header (lokale OSS-Provider).
    NoAuth
);

// ===== Model capability tags =====
marker_zst!(
    /// Standard-Chat-/Completion-Modell.
    ChatModel
);
marker_zst!(
    /// Embedding-Modell.
    EmbeddingModel
);
marker_zst!(
    /// Vision-fähiges Modell.
    VisionModel
);
marker_zst!(
    /// Audio-Modell.
    AudioModel
);
marker_zst!(
    /// Reasoning-Modell.
    ReasoningModel
);
marker_zst!(
    /// Tool-Calling-fähiges Modell.
    ToolCallingModel
);

// ===== Runtime state tags =====
marker_zst!(
    /// Provider wurde gebaut und registriert.
    Registered
);
marker_zst!(
    /// Provider ist nur konfiguriert, noch nicht registriert.
    Unregistered
);

// ===== Policy tags =====
marker_zst!(
    /// Erbt die globale Fallback-Kette.
    InheritGlobalFallbacks
);
marker_zst!(
    /// Ersetzt die Fallback-Kette explizit.
    ExplicitFallbacks
);
marker_zst!(
    /// Keine Fallbacks.
    NoFallbacks
);

// ===== Provider marker structs (Note 12 §12) =====
marker_zst!(
    /// Marker für den OpenAI-Provider.
    OpenAiProviderMarker
);
marker_zst!(
    /// Marker für den Azure-OpenAI-Provider.
    AzureOpenAiProviderMarker
);
marker_zst!(
    /// Marker für den ollama-Provider.
    OllamaProviderMarker
);

// ===== Runtime tag enums (erased forms; Note 12 §8) =====

/// Runtime-Sicht der Rolle. `Primary`/`Secondary` als erasable Daten-Enum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRoleTag {
    Primary,
    #[default]
    Secondary,
}

/// Runtime-Sicht der Modell-Fähigkeit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapabilityTag {
    #[default]
    Chat,
    Embedding,
    Vision,
    Audio,
    Reasoning,
    ToolCalling,
}

// ===== Marker traits (Note 12 §9) =====

/// Brücke Marker -> Lookup-Name (Note 11 §3: `trait ProviderMarker`).
pub trait ProviderMarker {
    const NAME: &'static str;
}

/// Brücke Rollen-Marker -> Runtime-Tag.
pub trait RoleMarker {
    const ROLE: ProviderRoleTag;
}

/// Brücke Capability-Marker -> Runtime-Tag.
pub trait ModelCapabilityMarker {
    const CAPABILITY: ModelCapabilityTag;
}

// ===== Role bridge impls (Note 12 §12) =====
impl RoleMarker for Primary {
    const ROLE: ProviderRoleTag = ProviderRoleTag::Primary;
}

impl RoleMarker for Secondary {
    const ROLE: ProviderRoleTag = ProviderRoleTag::Secondary;
}

// ===== Provider marker impls =====
impl ProviderMarker for OpenAiProviderMarker {
    const NAME: &'static str = "openai";
}

impl ProviderMarker for AzureOpenAiProviderMarker {
    const NAME: &'static str = "azure-openai";
}

impl ProviderMarker for OllamaProviderMarker {
    const NAME: &'static str = "ollama";
}

// ===== Capability bridge impls =====
impl ModelCapabilityMarker for ChatModel {
    const CAPABILITY: ModelCapabilityTag = ModelCapabilityTag::Chat;
}

impl ModelCapabilityMarker for EmbeddingModel {
    const CAPABILITY: ModelCapabilityTag = ModelCapabilityTag::Embedding;
}

impl ModelCapabilityMarker for VisionModel {
    const CAPABILITY: ModelCapabilityTag = ModelCapabilityTag::Vision;
}

impl ModelCapabilityMarker for AudioModel {
    const CAPABILITY: ModelCapabilityTag = ModelCapabilityTag::Audio;
}

impl ModelCapabilityMarker for ReasoningModel {
    const CAPABILITY: ModelCapabilityTag = ModelCapabilityTag::Reasoning;
}

impl ModelCapabilityMarker for ToolCallingModel {
    const CAPABILITY: ModelCapabilityTag = ModelCapabilityTag::ToolCalling;
}
