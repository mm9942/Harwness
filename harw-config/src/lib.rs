#![forbid(unsafe_code)]

pub mod agent_toml;
pub mod auth_toml;
pub mod channel_toml;
pub mod discovery;
pub mod dotenv;
pub mod error;
pub mod harness_config;
pub mod loader;
pub mod mcp_toml;
pub mod mode_toml;
pub mod model_toml;
pub mod plan_toml;
pub mod plugin_toml;
pub mod provider_toml;
pub mod research_toml;
pub mod skill_toml;

pub use agent_toml::{AgentSuggestionsToml, AgentToml};
pub use auth_toml::{AuthConfig, CredentialEntry, KekConfig, KekProvenance, SecretRef};
pub use channel_toml::{ChannelFileToml, ChannelSectionToml, ChannelToml, TelegramChannelToml};
pub use discovery::{
    HasName, ResolvedConfig, default_config_layers, discover_config,
    discover_config_with_restricted,
};
pub use dotenv::{
    check_dotenv_permissions, load_dotenv, load_env_layer, parse_dotenv_text, resolve_env_ref,
};
pub use error::{ConfigError, ConfigResult};
pub use harness_config::{
    HarnessConfig, LoggingSection, McpJobCapabilityToml, McpListenerSection, McpPrincipalToml,
    PolicySection, SessionSection, TuiSection,
};
pub use loader::{load_skill_instructions, load_system_prompt};
pub use mcp_toml::{McpServerToml, McpTransportToml};
pub use mode_toml::ModeSection;
pub use model_toml::{ModelCapabilitiesToml, ModelToml};
pub use plan_toml::{PlanSection, ToolsSection};
pub use plugin_toml::{PluginCapabilitiesToml, PluginToml};
pub use provider_toml::{OriginAllowlistToml, ProviderToml};
pub use research_toml::ResearchSection;
pub use skill_toml::SkillToml;
