use serde::{Deserialize, Serialize};

use crate::auth_toml::SecretRef;
use crate::internal_models::InternalModelsToml;
use crate::mode_toml::ModeSection;
use crate::permissions_toml::PermissionsSection;
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
    /// Pflichtauswahl des Benutzeroberflächen-Agenten. Der Name verweist auf
    /// eine gesenkte Agent-Definition mit der Rolle `user-interface`.
    /// Ohne gültige Auswahl wird keine interaktive Runtime gestartet.
    #[serde(default)]
    pub active_uia_definition: Option<String>,
    /// Pinnt das Modell (Anbieter) der interaktiven UIA-Sitzung unabhängig von
    /// `default_provider` — im Gegensatz zu `active_uia_definition` (welche
    /// UIA-Agent-*Definition* aktiv ist), wählt dieses Feld nur, welcher
    /// bereits konfigurierte Provider-Backend die UIA-Sitzung bedient.
    /// `None` → die UIA nutzt `default_provider` wie bisher.
    #[serde(default)]
    pub uia_provider: Option<String>,
    /// Pinnt das Modell der interaktiven UIA-Sitzung unabhängig von
    /// `default_model`. `None` → die UIA nutzt `default_model` wie bisher.
    #[serde(default)]
    pub uia_model: Option<String>,
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
    /// `[permissions]` — persistenter Freigabemodus, Timeout sowie
    /// Allow/Deny-Regeln und zusätzliche Arbeitswurzeln (Contract
    /// `harw-scopes-contract.md` §2/§5 Zeile A2). Siehe `permissions_toml.rs`.
    #[serde(default)]
    pub permissions: PermissionsSection,
    /// `[sandbox]` — hostseitig vertrauenswürdige Laufzeitvorgaben für
    /// Prozess-Sandboxes. Eine konfigurierte Cargo-Toolchain oder ein
    /// tmux-Socket wird ausschließlich beim Aufbau der Runtime gelesen, nie
    /// aus einem Tool-Aufruf übernommen.
    #[serde(default)]
    pub sandbox: SandboxSection,
    /// Marker-Dateinamen für die Projekt-Root-Erkennung (Contract §3),
    /// Default `[".git"]` beim Consumer (`harw-home::project`), sofern hier
    /// nicht gesetzt.
    #[serde(default)]
    pub project_root_markers: Option<Vec<String>>,
    /// `[internal_models]` — pro-Stelle wählbares Modell für interne
    /// Hilfsaufgaben (Sitzungstitel, Verdichtung, Gedächtnis-Konsolidierung,
    /// Traum-Reflexion, Explorer, Recherche), siehe `internal_models.rs`
    /// (Addendum C). Legacy `session.title_model` bleibt unverändert
    /// bestehen und wird vom Resolver als Fallback für `SessionTitle`
    /// gelesen.
    #[serde(default)]
    pub internal_models: InternalModelsToml,
    /// `[compaction]` — Verdichtungs-Obergrenzen (Addendum D+E). Siehe
    /// [`CompactionToml`].
    #[serde(default)]
    pub compaction: CompactionToml,
    /// `[reasoning]` — Rollen-Reasoning-Effort-Gewichtung (Addendum F+G).
    /// Siehe [`ReasoningWeightsToml`].
    #[serde(default)]
    pub reasoning: ReasoningWeightsToml,
    /// `[guards]` — Wächter-Schwellen für Drift/Zombies/Schleifen
    /// (Addendum F+G). Siehe [`GuardsToml`].
    #[serde(default)]
    pub guards: GuardsToml,
    #[serde(skip)]
    pub base_dir: Option<std::path::PathBuf>,
}

/// `[compaction]` — Verdichtungs-Konfiguration (Addendum D+E,
/// `CONTRACT.md`). Überschreibt die absolute Obergrenze der
/// UIA/Root-Sitzung, die sonst `harw_core::DEFAULT_ABSOLUTE_CEILING_TOKENS`
/// verwendet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionToml {
    /// Feste Obergrenze in Tokens für `AutoCompactPolicy::with_absolute_ceiling`
    /// der Root-/UIA-Sitzung. `None` → Standard
    /// (`harw_core::DEFAULT_ABSOLUTE_CEILING_TOKENS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absolute_ceiling_tokens: Option<u64>,
}

/// `[reasoning]` — Rollen-Reasoning-Effort-Gewichtung (Addendum F+G,
/// `CONTRACT.md`). Jedes Feld überschreibt, sofern gesetzt und gültig
/// (`"minimal"|"low"|"medium"|"high"|"xhigh"|"max"`), das entsprechende Feld
/// aus `harw_core::RoleEffortWeights::default()`; ein ungültiges Label wird
/// von `harw-runtime::guard_wiring::role_effort_weights_from_config` nur
/// `tracing::warn!`-gemeldet und fällt auf den Vorgabewert zurück.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningWeightsToml {
    /// Effort des Benutzeroberflächen-Agenten. Vorgabe `high`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uia: Option<String>,
    /// Effort eines Wurzel-Orchestrators ohne Sub-Orchestrator-Freigaben.
    /// Vorgabe `high`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_orchestrator: Option<String>,
    /// Effort eines Wurzel-Orchestrators mit Sub-Orchestrator-Freigaben.
    /// Vorgabe `medium`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_orchestrator_with_subs: Option<String>,
    /// Effort eines Sub-Orchestrators. Vorgabe `medium`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_orchestrator: Option<String>,
    /// Effort eines komplexen Workers. Vorgabe `medium`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_complex: Option<String>,
    /// Effort eines einfachen Workers. Vorgabe `low`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_simple: Option<String>,
}

/// `[guards]` — Wächter-Schwellen für Drift/Zombies/Schleifen (Addendum F+G,
/// `CONTRACT.md`). Jedes `None`-Feld fällt auf
/// `harw_core::GuardPolicy::default()` zurück (siehe
/// `harw-runtime::guard_wiring::guard_policy_from_config`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardsToml {
    /// Wächter global ein-/ausschalten. Vorgabe `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Fehlerwiederholungen derselben Signatur bis zur Warnung. Vorgabe `2`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeated_failure_warn: Option<u32>,
    /// Fehlerwiederholungen derselben Signatur bis zum Abbruch. Vorgabe `3`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeated_failure_abort: Option<u32>,
    /// Runden ohne Fortschritt bis zur Warnung. Vorgabe `4`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_progress_rounds_warn: Option<u32>,
    /// Runden ohne Fortschritt bis zum Abbruch. Vorgabe `8`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_progress_rounds_abort: Option<u32>,
    /// Runden ohne `plan.*`-Aufruf bis zur Stale-Warnung. Vorgabe `6`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_stale_rounds: Option<u32>,
}

/// `[sandbox]` — Prozess-Sandbox-Konfiguration. Das Fehlen eines Moduls
/// lässt die Sandbox unverändert hermetisch, stellt aber bewusst keine
/// Toolchain und keinen tmux-Socket bereit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo: Option<CargoSandboxToml>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux: Option<TmuxSandboxToml>,
}

/// Vertrauenswürdige Host-Wurzeln einer Cargo-Toolchain.
///
/// Alle drei Werte müssen absolute Pfade sein. Existenz, Kanonisierung und
/// Eigentums-/Ausführbarkeitseigenschaften werden erst von
/// `harw-sandbox::CargoSandboxProfile` beim Runtime-Aufbau geprüft, damit die
/// Konfigurations-Crate keine Sicherheitsgrenze dupliziert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CargoSandboxToml {
    pub mode: CargoSandboxModeToml,
    pub cargo_bin: String,
    pub rustup_home: String,
    pub cargo_home: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoSandboxModeToml {
    Inspect,
    BuildOffline,
    Fetch,
}

/// Vertrauenswürdiger Host-Pfad eines lokalen tmux-Sockets.
///
/// Der Pfad muss absolut und normal sein. Existenz und Socket-Eigenschaft
/// werden erst von `harw-sandbox::TmuxSandboxProfile` beim Runtime-Aufbau
/// geprüft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TmuxSandboxToml {
    pub mode: TmuxOperationModeToml,
    pub socket_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TmuxOperationModeToml {
    Inspect,
    Write,
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
    /// Ob nach dem ersten abgeschlossenen Turn ein Session-Titel per Modell
    /// erzeugt wird (Memory-/Session-Titel, Schritt 7). Default `true`; ohne
    /// erreichbaren Provider fällt die Erzeugung auf den Anfang der ersten
    /// Nutzernachricht zurück.
    #[serde(default = "default_title_generation")]
    pub title_generation: bool,
    /// Modell für die Titelerzeugung (`provider/modell`). `None` bedeutet:
    /// dasselbe Modell wie die Session, mit niedrigem Aufwand.
    #[serde(default)]
    pub title_model: Option<String>,
}

/// Default für [`SessionSection::title_generation`].
fn default_title_generation() -> bool {
    true
}

impl Default for SessionSection {
    fn default() -> Self {
        Self {
            store_dir: default_store_dir(),
            journal_format: default_journal_format(),
            retention_days: default_retention_days(),
            title_generation: default_title_generation(),
            title_model: None,
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
    fn test_active_uia_definition_defaults_to_none() {
        let cfg: HarnessConfig = toml::from_str("default_provider = \"anthropic\"").unwrap();
        assert_eq!(cfg.active_uia_definition, None);
    }

    #[test]
    fn test_active_uia_definition_reads_exact_definition_id() {
        let cfg: HarnessConfig = toml::from_str(
            r#"
                active_uia_definition = "harwness.agent.terminal-ui@1"
            "#,
        )
        .unwrap();
        assert_eq!(
            cfg.active_uia_definition.as_deref(),
            Some("harwness.agent.terminal-ui@1")
        );
    }

    #[test]
    fn test_uia_provider_and_model_default_to_none() {
        let cfg: HarnessConfig = toml::from_str("default_provider = \"anthropic\"").unwrap();
        assert_eq!(cfg.uia_provider, None);
        assert_eq!(cfg.uia_model, None);
    }

    #[test]
    fn test_uia_provider_and_model_read_exact_values() {
        let cfg: HarnessConfig = toml::from_str(
            r#"
                uia_provider = "anthropic"
                uia_model = "claude-x"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.uia_provider.as_deref(), Some("anthropic"));
        assert_eq!(cfg.uia_model.as_deref(), Some("claude-x"));
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

        assert!(cfg.tools.plan.enabled);
        assert!(cfg.tools.plan.persist);
        assert_eq!(cfg.mode.default, "plan");
        assert_eq!(cfg.research.max_fetch_bytes, 1_048_576);
        assert_eq!(cfg.permissions, crate::permissions_toml::PermissionsSection::default());
        assert!(cfg.project_root_markers.is_none());
        assert!(cfg.tools.plan.validate().is_ok());
        assert!(cfg.mode.validate().is_ok());
        assert!(cfg.research.validate().is_ok());
        assert!(cfg.permissions.validate().is_ok());
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
        assert!(cfg.permissions.validate().is_ok());
    }

    #[test]
    fn test_permissions_section_and_project_root_markers_parse_together() {
        let src = r#"
            default_provider = "anthropic"
            project_root_markers = [".git", ".hg"]

            [permissions]
            default_mode = "auto"
            approval_timeout_secs = 120

            [[permissions.allow]]
            tool = "shell.exec"
            pattern = "cargo check"

            [[permissions.deny]]
            tool = "fs.write"
        "#;
        let cfg: HarnessConfig = toml::from_str(src).unwrap();

        assert_eq!(
            cfg.project_root_markers,
            Some(vec![".git".to_owned(), ".hg".to_owned()])
        );
        assert_eq!(cfg.permissions.default_mode.as_deref(), Some("auto"));
        assert_eq!(cfg.permissions.approval_timeout_secs, Some(120));
        assert_eq!(cfg.permissions.allow.len(), 1);
        assert_eq!(cfg.permissions.allow[0].tool, "shell.exec");
        assert_eq!(cfg.permissions.deny.len(), 1);
        assert_eq!(cfg.permissions.deny[0].tool, "fs.write");
        assert!(cfg.permissions.validate().is_ok());
    }

    #[test]
    fn test_permissions_section_rejects_unknown_nested_field() {
        let src = r#"
            [permissions]
            defualt_mode = "auto"
        "#;
        assert!(toml::from_str::<HarnessConfig>(src).is_err());
    }

    #[test]
    fn test_tools_section_rejects_unknown_nested_field() {
        let src = r#"
            [tools.plan]
            enabeld = true
        "#;
        assert!(toml::from_str::<HarnessConfig>(src).is_err());
    }

    #[test]
    fn test_sandbox_section_defaults_when_absent() {
        let cfg: HarnessConfig = toml::from_str(
            r#"
                default_provider = "anthropic"
            "#,
        )
        .unwrap();
        assert!(cfg.sandbox.cargo.is_none());
        assert!(cfg.sandbox.tmux.is_none());
    }

    #[test]
    fn test_sandbox_cargo_section_parses() {
        let cfg: HarnessConfig = toml::from_str(
            r#"
                default_provider = "anthropic"

                [sandbox.cargo]
                mode = "build_offline"
                cargo_bin = "/opt/harw/toolchain/bin/cargo"
                rustup_home = "/opt/harw/rustup"
                cargo_home = "/var/cache/harw/cargo"
            "#,
        )
        .unwrap();
        let cargo = cfg.sandbox.cargo.unwrap();
        assert_eq!(cargo.mode, CargoSandboxModeToml::BuildOffline);
        assert_eq!(cargo.cargo_bin, "/opt/harw/toolchain/bin/cargo");
        assert_eq!(cargo.rustup_home, "/opt/harw/rustup");
        assert_eq!(cargo.cargo_home, "/var/cache/harw/cargo");
    }

    #[test]
    fn test_sandbox_tmux_section_parses() {
        let cfg: HarnessConfig = toml::from_str(
            r#"
                default_provider = "anthropic"

                [sandbox.tmux]
                mode = "inspect"
                socket_path = "/tmp/tmux-1000/default"
            "#,
        )
        .unwrap();
        let tmux = cfg.sandbox.tmux.unwrap();
        assert_eq!(tmux.mode, TmuxOperationModeToml::Inspect);
        assert_eq!(tmux.socket_path, "/tmp/tmux-1000/default");
    }

    #[test]
    fn test_sandbox_section_rejects_unknown_field() {
        let src = r#"
            [sandbox]
            no_such_field = true
        "#;
        assert!(toml::from_str::<HarnessConfig>(src).is_err());
    }

    #[test]
    fn test_sandbox_cargo_rejects_unknown_field() {
        let src = r#"
            [sandbox.cargo]
            mode = "fetch"
            cargo_bin = "/cargo"
            rustup_home = "/rustup"
            cargo_home = "/cache"
            extra_field = true
        "#;
        assert!(toml::from_str::<HarnessConfig>(src).is_err());
    }

    #[test]
    fn test_sandbox_cargo_rejects_invalid_mode() {
        let src = r#"
            [sandbox.cargo]
            mode = "unrestricted"
            cargo_bin = "/cargo"
            rustup_home = "/rustup"
            cargo_home = "/cache"
        "#;
        assert!(toml::from_str::<HarnessConfig>(src).is_err());
    }
}
