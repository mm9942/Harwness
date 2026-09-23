//! `harw-provider` — Provider-Schicht des Harness.
//!
//! Diese Crate setzt das Typestate-Skelett aus
//! `04_Project/12-provider-typestate-skeleton.md` um und übernimmt die
//! Registry-/Fallback-Doktrin aus `11-provider-registry-and-fallback-policy.md`
//! sowie das OpenAI-/Bearer-Schema aus `08-openai-endpoint-and-bearer-auth.md`.
//!
//! ## Leitentscheidungen (aus den Quell-Noten, wörtlich gegründet)
//! 1. **Der Provider ist das Hauptobjekt.** `Primary`/`Secondary` sind Tags am
//!    Provider selbst, kein separates Fallback-Konzept.
//! 2. **Die Fallback-Kette ist eine abgeleitete Runtime-Sicht** ([`ResolvedProviderChain`]),
//!    ein 1D-`Vec`: `chain[0]` = primary, `chain[1..]` = fallbacks.
//! 3. **Typestate stützt Invarianten** zur Compile-Zeit (Marker-ZSTs), während
//!    die Registrierung selbst eine Runtime-`Result`-Frage bleibt — genau wie
//!    crypt_guards `append_log` ein `Result` gibt statt zur Compile-Zeit zu
//!    beweisen, dass der Logger aktiv ist.
//!
//! Fehler laufen über [`error::ProviderError`] (`#[derive(HarwError)]`,
//! kein `anyhow`/`thiserror`); Tokens tragen `secrecy::SecretString`, nie
//! nacktes `String`.

#![forbid(unsafe_code)]

pub mod auth;
pub mod chain;
pub mod error;
pub mod invocation;
pub mod marker;
pub mod model;
pub mod openai;
pub mod provider;
pub mod registry;
pub mod slots;
#[cfg(test)]
mod test_support;

// ===== Error surface =====
pub use error::{ProviderError, ProviderFailure, ProviderResult};

// ===== Marker / tag surface =====
pub use marker::{
    AnthropicCompat,
    // auth tags
    ApiKeyAuth,
    AudioModel,
    AzureOpenAiProviderMarker,
    // capability tags
    ChatModel,
    CustomProvider,
    EmbeddingModel,
    ExplicitFallbacks,
    // policy tags
    InheritGlobalFallbacks,
    LocalRuntime,
    ModelCapabilityMarker,
    ModelCapabilityTag,
    NoAuth,
    NoFallbacks,
    OllamaProviderMarker,
    // kind tags
    OpenAiCompat,
    OpenAiProviderMarker,
    // role tags
    Primary,
    // provider markers
    ProviderMarker,
    ProviderRoleTag,
    ReasoningModel,
    Registered,
    RoleMarker,
    Secondary,
    StaticBearerAuth,
    ToolCallingModel,
    Unregistered,
    VisionModel,
};

#[cfg(feature = "chatgpt-oauth")]
pub use marker::ChatGptOAuthAuth;

// ===== Auth surface =====
#[cfg(feature = "chatgpt-oauth")]
pub use auth::ChatGptOAuthConfig;
pub use auth::{ApiKeyConfig, AuthProvider, BearerAuth, HeaderMap, SghAuth, StaticBearerConfig};

// ===== Model surface =====
pub use model::{
    ModelBuilder, ModelDefinition, ModelLike, ModelMetadata, ModelRecord, ModelSettings,
};

// ===== Provider surface =====
pub use provider::{
    Provider, ProviderBuilder, ProviderLike, ProviderMetadata, ProviderRecord, ProviderSettings,
};

// ===== Registry surface =====
pub use registry::{
    HashMapProviderRegistry, PROVIDERS, ProviderRegistry, VecProviderRegistry, provider_registry,
    provider_registry_read, provider_registry_write, register_provider, resolve_primary_provider,
    resolve_provider, resolve_provider_chain, resolve_secondary_providers, unregister_provider,
};

// ===== Chain / override surface =====
pub use chain::{
    AgentProviderOverride, ResolvedProviderChain, VecOp, resolve_provider_chain_with_override,
};

// ===== Invocation surface =====
pub use invocation::{
    InvocationRequest, InvocationResponse, ProviderInvoker, invoke_primary, invoke_with_failover,
};

// ===== Macro output targets (Note 12 §18) =====
pub use slots::{
    ModelDefinitionSpec, ProviderDefinition, install_model_definition, install_provider_definition,
};

// Re-export the shared ID newtypes so downstream code can stay on a single path.
pub use harw_types::{AgentName, CustomerId, ModelId, ModelName, ProviderId, ProviderName};
