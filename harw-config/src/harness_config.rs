use serde::{Deserialize, Serialize};

use crate::auth_toml::SecretRef;
use crate::mode_toml::ModeSection;
use crate::plan_toml::ToolsSection;
use crate::research_toml::ResearchSection;

/// Globale Harness-Konfiguration aus `.harw/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct HarnessConfig {
    #[serde(default)]
    pub config_version: u32,
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub default_provider: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub active_agent_definition: Option<String>,
    #[serde(default)]
    pub policy_profile: Option<String>,
    #[serde(default)]
    pub logging: LoggingSection,
    #[serde(default)]
    pub tui: TuiSection,
    #[serde(default)]
    pub session: SessionSection,
    #[serde(default)]
    pub policy: PolicySection,
    #[serde(default)]
    pub mcp_listener: McpListenerSection,
    #[serde(default)]
    pub onboarding: OnboardingSection,
    /// `[tools]` (aktuell nur `[tools.plan]`) — werkzeugspezifische
    /// Konfiguration. Siehe `plan_toml.rs`.
    #[serde(default)]
    pub tools: ToolsSection,
    /// `[mode]` — Standard-Interaktionsmodus. Siehe `mode_toml.rs`.
    #[serde(default)]
    pub mode: ModeSection,
    /// `[research]` — Netzwerk- und Ressourcen-Policy für Recherche-Tools.
    /// Siehe `research_toml.rs`.
    #[serde(default)]
    pub research: ResearchSection,
    #[serde(skip)]
    pub base_dir: Option<std::path::PathBuf>,
}

/// `[onboarding]` — First-Run-Fortschritt (Hermes-Muster `onboarding.seen.*`).
/// Der Wizard setzt die Flags, sobald der jeweilige Schritt abgeschlossen ist;
/// `harw` überspringt bereits gesehene Schritte bei künftigen Starts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnboardingSection {
    #[serde(default)]
    pub seen: OnboardingSeen,
}

/// Bool-Set der abgeschlossenen Onboarding-Schritte.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnboardingSeen {
    #[serde(default)]
    pub provider: bool,
    #[serde(default)]
    pub model: bool,
    #[serde(default)]
    pub channel: bool,
}

impl OnboardingSeen {
    /// `true`, wenn die pflichtigen Schritte (Provider **und** Modell)
    /// abgeschlossen sind. Der Channel-Schritt ist optional.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.provider && self.model
    }
}

/// `[logging]` — Tracing-Level und Ausgabeform. Alle Felder haben
/// hart-codierte Defaults, sodass eine `config.toml` ohne `[logging]`
/// weiterhin gültig ist.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingSection {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default)]
    pub target_module_paths: bool,
    #[serde(default)]
    pub json: bool,
}

impl Default for LoggingSection {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            target_module_paths: false,
            json: false,
        }
    }
}

/// `[tui]` — Theme und Verweis auf die Keybindings-Datei (relativ zum
/// Layer-Verzeichnis dieser `config.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TuiSection {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_keybindings_file")]
    pub keybindings_file: String,
}

impl Default for TuiSection {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            keybindings_file: default_keybindings_file(),
        }
    }
}

/// `[session]` — Speicherort und Aufbewahrung des Session-Journals.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSection {
    #[serde(default = "default_store_dir")]
    pub store_dir: String,
    #[serde(default = "default_journal_format")]
    pub journal_format: String,
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
}

impl Default for SessionSection {
    fn default() -> Self {
        Self {
            store_dir: default_store_dir(),
            journal_format: default_journal_format(),
            retention_days: default_retention_days(),
        }
    }
}

/// `[policy]` — Standard-Sichtbarkeits-Scope und Approval-Pflichtliste.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySection {
    #[serde(default = "default_visibility_scope")]
    pub default_visibility_scope: String,
    #[serde(default)]
    pub require_approval_for: Vec<String>,
}

/// `[mcp_listener]` — local Streamable HTTP ingress for the standalone
/// harness. The listener is loopback-only by default; a future remote ingress
/// must terminate TLS and authenticate before it can hand a request here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpListenerSection {
    /// Opt-in until the standalone runtime composes the listener and durable
    /// job supervisor together.
    #[serde(default)]
    pub enabled: bool,
    /// Socket address, deliberately including the port to prevent host/port
    /// configuration from drifting between channels and MCP clients.
    #[serde(default = "default_mcp_listener_addr")]
    pub listen_addr: String,
    /// Streamable HTTP path. This stays fixed to an absolute path rather than
    /// accepting a full URL so authority remains server-side.
    #[serde(default = "default_mcp_listener_path")]
    pub path: String,
    /// Explicit authenticated identities for the local HTTP endpoint. Empty
    /// grants no access; an enabled listener requires at least one principal.
    #[serde(default)]
    pub principals: Vec<McpPrincipalToml>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpPrincipalToml {
    pub id: String,
    pub credential_ref: SecretRef,
    pub tenant: String,
    pub workspace: String,
    #[serde(default)]
    pub job_capabilities: Vec<McpJobCapabilityToml>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum McpJobCapabilityToml {
    ReadOwn,
    ReadWorkspace,
    SubmitOwn,
    CancelOwn,
    CancelWorkspace,
}

impl Default for McpListenerSection {
    fn default() -> Self {
        Self {
            enabled: false,
            listen_addr: default_mcp_listener_addr(),
            path: default_mcp_listener_path(),
            principals: Vec::new(),
        }
    }
}

impl Default for PolicySection {
    fn default() -> Self {
        Self {
            default_visibility_scope: default_visibility_scope(),
            require_approval_for: Vec::new(),
        }
    }
}

fn default_log_level() -> String {
    "info".to_owned()
}
fn default_theme() -> String {
    "default-dark".to_owned()
}
fn default_keybindings_file() -> String {
    "keybindings.toml".to_owned()
}
fn default_store_dir() -> String {
    "sessions".to_owned()
}
fn default_journal_format() -> String {
    "jsonl".to_owned()
}
fn default_retention_days() -> u32 {
    90
}
fn default_visibility_scope() -> String {
    "self".to_owned()
}
fn default_mcp_listener_addr() -> String {
    "127.0.0.1:1337".to_owned()
}
fn default_mcp_listener_path() -> String {
    "/mcp".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_minimal_config_still_parses() {
        let src = r#"
            default_provider = "anthropic"
            default_model = "claude-sonnet"
        "#;
        let cfg: HarnessConfig = toml::from_str(src).unwrap();
        assert_eq!(cfg.logging.level, "info");
        assert_eq!(cfg.tui.theme, "default-dark");
        assert_eq!(cfg.session.retention_days, 90);
        assert_eq!(cfg.mcp_listener.listen_addr, "127.0.0.1:1337");
        assert_eq!(cfg.mcp_listener.path, "/mcp");
    }

    #[test]
    fn test_config_version_defaults_and_reads() {
        let without = r#"
            default_provider = "anthropic"
        "#;
        let cfg: HarnessConfig = toml::from_str(without).unwrap();
        assert_eq!(cfg.config_version, 0);

        let with = r#"
            config_version = 3
            default_provider = "anthropic"
        "#;
        let cfg: HarnessConfig = toml::from_str(with).unwrap();
        assert_eq!(cfg.config_version, 3);
    }

    #[test]
    fn test_active_agent_definition_defaults_to_none() {
        let cfg: HarnessConfig = toml::from_str("default_provider = \"anthropic\"").unwrap();

        assert_eq!(cfg.active_agent_definition, None);
    }

    #[test]
    fn test_active_agent_definition_reads_exact_definition_id() {
        let cfg: HarnessConfig = toml::from_str(
            r#"
                active_agent_definition = "definition://coding/rust/strict-v1"
            "#,
        )
        .unwrap();

        assert_eq!(
            cfg.active_agent_definition.as_deref(),
            Some("definition://coding/rust/strict-v1")
        );
    }

    #[test]
    fn test_unknown_fields_are_rejected_at_each_runtime_config_boundary() {
        let top_level = r#"
            default_provider = "anthropic"
            defualt_model = "claude-sonnet"
        "#;
        assert!(toml::from_str::<HarnessConfig>(top_level).is_err());

        let nested = r#"
            [logging]
            levle = "debug"
        "#;
        assert!(toml::from_str::<HarnessConfig>(nested).is_err());

        let enum_variant = r#"
            capability = "submit_won"
        "#;
        #[derive(Deserialize)]
        // Struct dient nur der Deserialisierungs-Prüfung (ungültige Enum-Variante
        // muss fehlschlagen); das Feld wird nie gelesen, da from_str immer Err liefert.
        #[allow(dead_code)]
        struct CapabilityConfig {
            capability: McpJobCapabilityToml,
        }
        assert!(toml::from_str::<CapabilityConfig>(enum_variant).is_err());
    }

    #[test]
    fn test_full_config_overrides_defaults() {
        let src = r#"
            default_provider = "anthropic"
            default_model = "claude-sonnet"

            [logging]
            level = "debug"

            [tui]
            theme = "light"

            [mcp_listener]
            enabled = true
            listen_addr = "127.0.0.1:1337"
            path = "/mcp"

            [[mcp_listener.principals]]
            id = "mia-local"
            credential_ref = "env:HARW_MCP_TOKEN"
            tenant = "mia"
            workspace = "harwness"
            job_capabilities = ["read_own", "cancel_own"]
        "#;
        let cfg: HarnessConfig = toml::from_str(src).unwrap();
        assert_eq!(cfg.logging.level, "debug");
        assert_eq!(cfg.tui.theme, "light");
        assert!(cfg.mcp_listener.enabled);
        assert_eq!(cfg.mcp_listener.principals.len(), 1);
    }

    #[test]
    fn test_submit_own_capability_parses_and_round_trips() {
        #[derive(Debug, Deserialize, PartialEq, Serialize)]
        struct CapabilityConfig {
            capability: McpJobCapabilityToml,
        }

        let config: CapabilityConfig = toml::from_str("capability = \"submit_own\"").unwrap();
        assert_eq!(config.capability, McpJobCapabilityToml::SubmitOwn);

        let encoded = toml::to_string(&config).unwrap();
        assert_eq!(encoded, "capability = \"submit_own\"\n");
        let decoded: CapabilityConfig = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn test_mcp_principal_has_no_default_job_capabilities() {
        let principal: McpPrincipalToml = toml::from_str(
            r#"
                id = "mia-local"
                credential_ref = "env:HARW_MCP_TOKEN"
                tenant = "mia"
                workspace = "harwness"
            "#,
        )
        .unwrap();

        assert!(principal.job_capabilities.is_empty());
    }

    #[test]
    fn test_tools_mode_research_sections_default_when_absent() {
        let cfg: HarnessConfig = toml::from_str(
            r#"
                default_provider = "anthropic"
            "#,
        )
        .unwrap();

        assert!(!cfg.tools.plan.enabled);
        assert_eq!(cfg.mode.default, "chat");
        assert_eq!(cfg.research.max_fetch_bytes, 1_048_576);
        assert!(cfg.tools.plan.validate().is_ok());
        assert!(cfg.mode.validate().is_ok());
        assert!(cfg.research.validate().is_ok());
    }

    #[test]
    fn test_tools_mode_research_sections_load_together() {
        let src = r#"
            default_provider = "anthropic"

            [tools.plan]
            enabled = true
            persist = true
            max_nodes = 128
            require_exploration_for = ["coding"]

            [mode]
            default = "plan"

            [research]
            network_allow_hosts = ["docs.rs", "crates.io"]
            max_fetch_bytes = 2048
        "#;
        let cfg: HarnessConfig = toml::from_str(src).unwrap();

        assert!(cfg.tools.plan.enabled);
        assert!(cfg.tools.plan.persist);
        assert_eq!(cfg.tools.plan.max_nodes, 128);
        assert_eq!(cfg.mode.default, "plan");
        assert_eq!(
            cfg.research.network_allow_hosts,
            vec!["docs.rs".to_owned(), "crates.io".to_owned()]
        );
        assert_eq!(cfg.research.max_fetch_bytes, 2048);

        assert!(cfg.tools.plan.validate().is_ok());
        assert!(cfg.mode.validate().is_ok());
        assert!(cfg.research.validate().is_ok());
    }

    #[test]
    fn test_tools_section_rejects_unknown_nested_field() {
        let src = r#"
            [tools.plan]
            enabeld = true
        "#;
        assert!(toml::from_str::<HarnessConfig>(src).is_err());
    }
}
