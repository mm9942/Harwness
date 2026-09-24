#![forbid(unsafe_code)]

//! Konfigurationsdateien und ihre typisierten Repräsentationen.
//!
//! Das Laden und Mergen mehrerer Konfigurationsebenen geschieht über
//! [`discovery::discover_config`].

pub mod agent_toml;
pub mod auth_toml;
pub mod browser_toml;
pub mod channel_toml;
pub mod discovery;
pub mod dod_toml;
pub mod dotenv;
pub mod error;
pub mod harness_config;
pub mod internal_models;
pub mod loader;
pub mod mcp_toml;
pub mod merge;
pub mod mode_toml;
pub mod model_toml;
pub mod network_toml;
pub mod permissions_toml;
pub mod plan_toml;
pub mod plugin_toml;
pub mod provider_toml;
pub mod research_toml;
mod role_models;
pub mod scope;
pub mod skill_toml;
pub mod web_toml;
pub mod writer;

pub use agent_toml::{AgentSuggestionsToml, AgentToml};
pub use auth_toml::{AuthConfig, CredentialEntry, KekConfig, KekProvenance, SecretRef};
pub use browser_toml::BrowserSection;
pub use channel_toml::{ChannelFileToml, ChannelSectionToml, ChannelToml, TelegramChannelToml};
pub use discovery::{
    HasName, ResolvedConfig, default_config_layers, discover_config,
    discover_config_with_restricted, discover_config_with_restricted_and_project_settings,
};
pub use dod_toml::DodSection;
pub use dotenv::{
    check_dotenv_permissions, load_dotenv, load_env_layer, parse_dotenv_text, resolve_env_ref,
};
pub use error::{ConfigError, ConfigResult};
pub use harness_config::{
    CargoSandboxModeToml, CargoSandboxToml, HarnessConfig, LoggingSection, McpJobCapabilityToml,
    McpListenerSection, McpPrincipalToml, PolicySection, SandboxSection, SessionSection,
    TmuxOperationModeToml, TmuxSandboxToml, TuiSection,
};
pub use internal_models::{
    InternalModelChoice, InternalModelPoint, InternalModelSource, InternalModelsToml,
    OPENROUTER_PROVIDER, ResolvedInternalModel, openrouter_available, resolve_internal_model,
};
pub use loader::{
    load_skill_instructions, load_system_prompt, load_uia_personalization, load_uia_user_name,
};
pub use mcp_toml::{McpServerToml, McpTransportToml};
pub use merge::{LayerRole, ScopeDiagnostic, merge_layer_into};
pub use mode_toml::ModeSection;
pub use model_toml::{ModelCapabilitiesToml, ModelToml, PromptCachingMode};
pub use network_toml::NetworkSection;
pub use permissions_toml::{PermissionsSection, RuleToml};
pub use plan_toml::{PlanSection, ToolsSection};
pub use plugin_toml::{PluginCapabilitiesToml, PluginToml};
pub use provider_toml::{OriginAllowlistToml, ProviderToml, RateLimitMode, RateLimitToml};
pub use research_toml::ResearchSection;
pub use role_models::*;
pub use scope::{FIELD_TABLE, FieldScope, MergeRule, Scope, SettingScope};
pub use skill_toml::SkillToml;
pub use web_toml::{WebSearchToml, WebSection};
pub use writer::{ConfigWriter, RuleKind};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
