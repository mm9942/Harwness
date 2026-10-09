#![forbid(unsafe_code)]

//! Konfigurationsdateien und ihre typisierten Repräsentationen.
//!
//! Das Laden und Mergen mehrerer Konfigurationsebenen geschieht über
//! [`discovery::discover_config`].

pub mod agent_limits;
// Ungeparste DSL-Agentendefinitionen der vertrauten Layer; gesenkt wird im
// Konsumenten (`harw_registry_defaults::config_agents`).
pub mod agent_sources;
pub mod agent_toml;
pub mod auth_toml;
pub mod browser_toml;
pub mod channel_toml;
pub mod discovery;
pub mod dod_toml;
pub mod dotenv;
pub mod error;
pub mod harness_config;
// Crypto-Infrastruktur H4: `[infrastructure]` — Daemon-Sockets.
pub mod infrastructure_toml;
pub mod internal_models;
pub mod loader;
pub mod mcp_toml;
pub mod memory_toml;
pub mod merge;
pub mod mode_toml;
pub mod model_toml;
pub mod network_toml;
pub mod permissions_toml;
pub mod plan_toml;
pub mod plugin_toml;
pub mod provider_toml;
pub mod research_toml;
pub mod retention_toml;
mod role_models;
pub mod scope;
mod serde_defaults;
pub mod session_listener_toml;
// Runde 5, Teil N: `[shell] max_timeout_secs`.
pub mod shell_limits;
pub mod skill_toml;
// Runde 5, Teil G: eigene Modellwahl je UIA-Worker-Rolle.
pub mod uia_worker_models;
pub mod web_toml;
pub mod writer;

// Runde 5, Teil K: `[agents]` — Orchestrierungsgrenzen.
pub use agent_limits::{AgentLimitsToml, EffectiveAgentLimits};
pub use agent_sources::{
    AgentDefinitionSource, AgentDefinitionSources, AgentSourceForm, AgentSourceLayer,
    CONTEXT_PROGRAMS_DIR, ContextProgramSource, INSTRUCTIONS_FILE_KEY, discover_run_agent_sources,
};
pub use agent_toml::{AgentSuggestionsToml, AgentToml};
pub use auth_toml::{AuthConfig, CredentialEntry, KekConfig, KekProvenance, SecretRef};
pub use browser_toml::BrowserSection;
pub use channel_toml::{ChannelFileToml, ChannelSectionToml, ChannelToml, TelegramChannelToml};
pub use discovery::{
    HasName, ResolvedConfig, default_config_layers, default_config_layers_named, discover_config,
    discover_config_with_restricted, discover_config_with_restricted_and_project_settings,
};
pub use dod_toml::DodSection;
pub use dotenv::{
    check_dotenv_permissions, load_dotenv, load_env_layer, parse_dotenv_text, resolve_env_ref,
};
pub use error::{ConfigError, ConfigResult};
pub use harness_config::{
    AgentCompilerToml, CargoSandboxModeToml, CargoSandboxToml, DEFAULT_DIARY_RETENTION_DAYS,
    DEFAULT_DREAM_BUDGET_TOKENS, DEFAULT_DREAM_COOLDOWN_MINUTES, DEFAULT_DREAM_ENABLED,
    DEFAULT_DREAM_IDLE_MINUTES, DEFAULT_SUDO_SESSION_MINUTES, DiaryToml, DreamToml, HarnessConfig,
    HostToml, KnowledgeToml, LoggingSection, MAX_SUDO_SESSION_MINUTES, McpJobCapabilityToml,
    McpListenerSection, McpPrincipalToml, PolicySection, SandboxSection, SessionSection,
    TmuxOperationModeToml, TmuxSandboxToml, TuiSection,
};
// Runde 5, Teil N: `[shell] max_timeout_secs`.
pub use shell_limits::ShellToml;
// Runde 5, Teil I: Live-Stream der Kind-Agenten (`[tui] child_stream`).
pub use harness_config::ChildStreamModeToml;
// h7: `[tui] status_expiry` — Verhalten abgelaufener Kind-Statusmeldungen.
pub use harness_config::StatusExpiryMode;
// Crypto-Infrastruktur H4: `[infrastructure]`.
pub use infrastructure_toml::InfrastructureSection;
// Runde 5, Teil E: `ANTHROPIC_FAST_MODEL`/`fast_model_for_active_provider`
// (Vorgabe-Modell des Auto-Modus-Klassifizierers).
pub use internal_models::{
    ANTHROPIC_FAST_MODEL, InternalModelChoice, InternalModelPoint, InternalModelSource,
    InternalModelsToml, OPENROUTER_PROVIDER, ResolvedInternalModel, fast_model_for_active_provider,
    openrouter_available, resolve_internal_model,
};
pub use loader::{
    MAX_AGENT_INSTRUCTIONS_BYTES, load_agent_instructions, load_skill_instructions,
    load_system_prompt, load_uia_personalization, load_uia_user_name,
};
pub use mcp_toml::{McpServerToml, McpTransportToml};
pub use memory_toml::MemorySection;
pub use merge::{LayerRole, ScopeDiagnostic, merge_layer_toml_into};
pub use mode_toml::ModeSection;
pub use model_toml::{ModelCapabilitiesToml, ModelToml, PromptCachingMode};
pub use network_toml::{NetworkSection, ResearchWebMode};
pub use permissions_toml::{PermissionsSection, RuleToml};
pub use plan_toml::{ContainerToolsSection, DocSection, PlanSection, RemoteOcrMode, ToolsSection};
pub use plugin_toml::{PluginCapabilitiesToml, PluginToml};
pub use provider_toml::{
    DEFAULT_LOCAL_REQUEST_TIMEOUT_SECS, DEFAULT_LOCAL_STREAM_IDLE_TIMEOUT_SECS,
    DEFAULT_REQUEST_TIMEOUT_SECS, MaxTokensField, OriginAllowlistToml, ProviderToml, RateLimitMode,
    RateLimitToml, host_is_private_lan,
};
pub use research_toml::ResearchSection;
pub use retention_toml::{RetentionClassToml, RetentionSection};
pub use role_models::*;
pub use scope::{FIELD_TABLE, FieldScope, MergeRule, Scope, SettingScope};
pub use session_listener_toml::SessionListenerSection;
pub use skill_toml::SkillToml;
pub use uia_worker_models::{
    FOLLOW_UIA_VALUE, ResolvedUiaWorkerModel, UIA_WORKER_ROLES, UiaWorkerModelChoice,
    UiaWorkerModelSource, UiaWorkerModelsToml, catalog_provider_of, provider_is_logged_in,
    resolve_uia_worker_model, resolve_uia_worker_models,
};
pub use web_toml::{WebIdentityModeToml, WebIdentityToml, WebSearchToml, WebSection};
pub use writer::{ConfigWriter, RuleKind};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
