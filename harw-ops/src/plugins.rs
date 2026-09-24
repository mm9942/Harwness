//! `/plugins` — konfigurierte Plugin-Manifeste auflisten und einsehen.
//!
//! # Verantwortungsbereich
//! Implementiert die `plugins`-Operation als `/plugins`-Command (tui_only).
//! `list` und `show <name>` lesen den deklarativen Plugin-Katalog
//! (`ResolvedConfig.plugins`, entdeckt aus `<layer>/plugins/*.toml`) aus dem
//! `Arc<ResolvedConfig>`-Service in [`OpContext`]. Ohne diesen Service bleibt
//! die Operation fail-closed ([`OpError::NotAvailable`]).
//!
//! `enable`/`disable` ändern **keinen** Zustand: Das `enabled`-Flag lebt im
//! jeweiligen Manifest (`plugins/<datei>.toml`, Feld `enabled`), nicht in der
//! Profil-`config.toml`, und es gibt keinen Schreib-Dienst für Manifeste
//! (`harw_config::ConfigWriter` wird von `/model`/`/permissions` nur für die
//! Profil-/Projekt-`config.toml` genutzt). Die Operation weist diese Actions
//! daher statisch ab und nennt die manuelle Änderung im Manifest.
//! `install`/`activate`/`uninstall` (Lebenszyklus über `harw-extension-api`)
//! bleiben ebenfalls abgewiesen.
//!
//! # Schlüsseltypen
//! - [`PluginsArgs`] — typisierte Felder für Action und optionalen Plugin-Namen (`target`).
//! - `PluginsOperation` — generiert vom `#[operation]`-Makro
//!
//! # Nebenläufigkeit
//! `PluginsOperation` ist `Send + Sync` (Unit-Struct ohne inneren Zustand, generiert
//! durch `#[operation]`-Makro). Der Katalog wird nur lesend verwendet.
//!
//! # Fehlertypen
//! - [`OpError::NotAvailable`]: kein `Arc<ResolvedConfig>` im Kontext, oder eine
//!   zustandsändernde Action (`enable`, `disable`, `install`, `activate`,
//!   `uninstall`). Diese Meldungen sind statisch und enthalten keine
//!   übergebenen Action-, Target- oder Value-Werte.
//! - [`OpError::InvalidArguments`]: `show` ohne Namen, unbekanntes Plugin oder
//!   unbekannte Action.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_ops::plugins::PluginsArgs;
//! // Die Operation wird über den harw-operations-Registry-Mechanismus aufgerufen.
//! ```

use harw_config::{PluginToml, ResolvedConfig};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use std::sync::Arc;

/// Statische Meldung, wenn kein `Arc<ResolvedConfig>` im Kontext liegt.
const CATALOG_UNAVAILABLE: &str = "extension registry is not available";

/// Statische Meldung für `enable`/`disable` (kein Manifest-Schreib-Dienst).
const TOGGLE_UNAVAILABLE: &str = "plugin enable/disable is not available from the session; \
     set `enabled = true|false` in the plugin manifest (plugins/<file>.toml in the \
     profile or project layer) and restart the session";

/// Statische Meldung für Lebenszyklus-Actions ohne `harw-extension-api`-Anbindung.
const LIFECYCLE_UNAVAILABLE: &str = "plugin lifecycle mutation is not available";

/// Argumente für die `/plugins`-Operation.
///
/// # Beschreibung
/// Trägt das Sub-Kommando (`action`) und den optionalen Plugin-Namen (`target`).
/// Die Felder werden durch das [`harw_macros::FromRawArgs`]-Derive direkt aus
/// den tokenisierten TUI-Rohargumenten befüllt — jedes Feld erhält genau das
/// Token an der entsprechenden Position (0-basiert).
///
/// # Felder
/// - `action` (`Option<String>`): Token 0 — Sub-Kommando; `"list"` (Standard)
///   und `"show"` werden ausgeführt; `"enable"`, `"disable"`, `"install"`,
///   `"activate"` und `"uninstall"` werden statisch abgewiesen.
///   `None` bezeichnet das Standard-Listing.
/// - `target` (`Option<String>`): Token 1 — Plugin-Name (z. B. `"my-plugin"`),
///   Pflicht für `show`.
/// - `value` (`Option<String>`): Token 2 — dritter Parameter (reserviert für
///   zukünftige Erweiterungen; aktuell ungenutzt).
///
/// # Hinweis
/// Die Operation hat kein Model-Tool-Surface (reine Operator-Fläche).
///
/// # Spec-Referenz
/// Plan v2 — `/plugins` Meta-Definition.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct PluginsArgs {
    /// Sub-Kommando: `"list"` (Standard), `"show"`, `"enable"`, `"disable"`, … Token 0.
    /// `None` wird als `"list"` interpretiert.
    #[serde(default)]
    #[raw(first)]
    pub action: Option<String>,
    /// Plugin-Name (z. B. `"my-plugin"`). Token 1. `None` wenn nicht angegeben.
    #[serde(default)]
    #[raw(nth = 1)]
    pub target: Option<String>,
    /// Dritter Parameter (reserviert). Token 2. `None` wenn nicht angegeben.
    #[serde(default)]
    #[raw(nth = 2)]
    pub value: Option<String>,
}

/// Listet konfigurierte Plugins auf oder zeigt ein einzelnes Manifest an.
///
/// # Beschreibung
/// Liest `ResolvedConfig.plugins` aus dem `Arc<ResolvedConfig>`-Service:
/// - `list` (Standard): alphabetisch sortierte Übersicht mit Version, Status,
///   Beschreibung und Netzwerk-Egress-Zusammenfassung.
/// - `show <name>`: alle Manifest-Felder (Version, Quelle, `enabled`,
///   Beschreibung, Tools, Skills, MCPs, Channels, `network_egress`); zusätzlich
///   das Manifest als strukturierte Nutzlast in [`OpOutput::data`].
/// - `enable`/`disable`: statisch abgewiesen, mit Hinweis auf das
///   `enabled`-Feld im Manifest.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Session-Kontext mit `Arc<ResolvedConfig>`-Service.
/// - `args` (`PluginsArgs`): Typisierte Sub-Kommando-Argumente.
///
/// # Rückgabe
/// Menschenlesbarer Text (bei `show` plus JSON-Nutzlast).
///
/// # Fehler
/// - [`OpError::NotAvailable`]: kein Katalog-Service, oder zustandsändernde Action.
/// - [`OpError::InvalidArguments`]: `show` ohne Namen, unbekanntes Plugin,
///   unbekannte Action (oder Deserialisierungsfehler, vom Makro gehandhabt).
///
/// # Nebenläufigkeit
/// Zustandslos; nur lesender Zugriff auf den geteilten Katalog.
///
/// # Beispiel
/// ```rust,no_run
/// // Wird indirekt über Operation::run aufgerufen.
/// ```
#[operation(
    name = "plugins",
    summary = "Plugin-Katalog: list/show gegen die geladene ResolvedConfig.plugins.",
    domain = "catalog_config",
    permission = "maintainer",
    command(
        path = "/plugins",
        visibility = "tui_only",
        busy = "immediate",
        busy_subcommands = "install=deferred, activate=deferred, uninstall=deferred"
    )
)]
async fn plugins(ctx: &OpContext, args: PluginsArgs) -> Result<OpOutput, OpError> {
    let Some(config) = ctx.service::<Arc<ResolvedConfig>>() else {
        return Err(OpError::NotAvailable(CATALOG_UNAVAILABLE.to_owned()));
    };

    match args.action.as_deref().unwrap_or("list") {
        "list" => Ok(OpOutput::from(render_list(config))),
        "show" => {
            let Some(target) = args.target.as_deref() else {
                return Err(OpError::InvalidArguments(
                    "action 'show' requires a plugin name".to_owned(),
                ));
            };
            let Some(plugin) = config.plugins.get(target) else {
                return Err(OpError::InvalidArguments(format!(
                    "unknown plugin '{target}'"
                )));
            };
            Ok(OpOutput {
                text: render_show(plugin),
                data: serde_json::to_value(plugin).ok(),
            })
        }
        "enable" | "disable" => Err(OpError::NotAvailable(TOGGLE_UNAVAILABLE.to_owned())),
        "install" | "activate" | "uninstall" => {
            Err(OpError::NotAvailable(LIFECYCLE_UNAVAILABLE.to_owned()))
        }
        unknown => Err(OpError::InvalidArguments(format!(
            "unknown /plugins action '{unknown}'"
        ))),
    }
}

/// Rendert die `list`-Übersicht (alphabetisch nach Katalog-Schlüssel).
fn render_list(config: &ResolvedConfig) -> String {
    if config.plugins.is_empty() {
        return "Keine Plugins konfiguriert.".to_owned();
    }
    let mut entries: Vec<(&String, &PluginToml)> = config.plugins.iter().collect();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    let mut lines = vec![format!("{} konfigurierte(s) Plugin(s):", entries.len())];
    for (name, plugin) in entries {
        let egress = if plugin.capabilities.network_egress.is_empty() {
            "kein Netzwerk-Egress".to_owned()
        } else {
            format!("egress: {}", plugin.capabilities.network_egress.join(", "))
        };
        lines.push(format!(
            "- {name}@{} ({}; {egress}): {}",
            plugin.version,
            status_label(plugin.enabled),
            plugin.description
        ));
    }
    lines.join("\n")
}

/// Rendert die Detailansicht eines Manifests für `show`.
fn render_show(plugin: &PluginToml) -> String {
    let caps = &plugin.capabilities;
    [
        format!(
            "{}@{} ({})",
            plugin.name,
            plugin.version,
            status_label(plugin.enabled)
        ),
        format!("enabled: {}", plugin.enabled),
        format!("description: {}", or_dash(&plugin.description)),
        format!("source: {}", or_dash(&plugin.source)),
        format!("tools: {}", join_or_dash(&caps.tools)),
        format!("skills: {}", join_or_dash(&caps.skills)),
        format!("mcps: {}", join_or_dash(&caps.mcps)),
        format!("channels: {}", join_or_dash(&caps.channels)),
        format!("network_egress: {}", join_or_dash(&caps.network_egress)),
    ]
    .join("\n")
}

fn status_label(enabled: bool) -> &'static str {
    if enabled { "enabled" } else { "disabled" }
}

fn or_dash(value: &str) -> &str {
    if value.is_empty() { "-" } else { value }
}

fn join_or_dash(values: &[String]) -> String {
    if values.is_empty() {
        "-".to_owned()
    } else {
        values.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::PluginsArgs;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_operations::operation::{CommandVisibility, Surface};
    use harw_operations::{FromRawArgs, OpContext, OpError, Operation, context::ServiceMap};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_context() -> TestResult<(OpContext, std::path::PathBuf)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-plugins-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("ws")).map_err(ctx("create test workspace"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("build workspace registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("resolve workspace binding"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        Ok((
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new()),
            root,
        ))
    }

    #[test]
    fn test_plugins_args_from_raw_args_sets_action() -> TestResult {
        let args = PluginsArgs::from_raw_args(&toks(&["install"]))
            .map_err(ctx("PluginsArgs::from_raw_args"))?;
        assert_eq!(args.action.as_deref(), Some("install"));
        assert!(args.target.is_none());
        Ok(())
    }

    #[test]
    fn test_plugins_args_from_raw_args_install_preserves_target() -> TestResult {
        let args = PluginsArgs::from_raw_args(&toks(&["install", "my-plugin"]))
            .map_err(ctx("PluginsArgs::from_raw_args"))?;
        assert_eq!(args.action.as_deref(), Some("install"));
        assert_eq!(args.target.as_deref(), Some("my-plugin"));
        assert!(args.value.is_none());
        Ok(())
    }

    #[test]
    fn test_plugins_args_from_raw_args_empty_tokens_sets_action_none() -> TestResult {
        let args =
            PluginsArgs::from_raw_args(&toks(&[])).map_err(ctx("PluginsArgs::from_raw_args"))?;
        assert!(args.action.is_none());
        assert!(args.target.is_none());
        Ok(())
    }

    #[test]
    fn plugins_operation_is_command_only() {
        let surfaces = &super::PluginsOperation.meta().surfaces;

        assert!(
            !surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. }))
        );
        assert!(surfaces.iter().any(|surface| {
            matches!(
                surface,
                Surface::Command {
                    path: "/plugins",
                    visibility: CommandVisibility::TuiOnly,
                }
            )
        }));
    }

    #[tokio::test]
    async fn plugins_default_list_without_config_service_is_not_available() -> TestResult {
        let (op_ctx, root) = test_context()?;
        let result = super::plugins(&op_ctx, PluginsArgs::default()).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        assert!(
            matches!(result, Err(OpError::NotAvailable(message)) if message == "extension registry is not available")
        );
        Ok(())
    }

    fn sample_plugin() -> harw_config::PluginToml {
        harw_config::PluginToml {
            name: "review".to_owned(),
            version: "1.0.0".to_owned(),
            source: "git+https://example.invalid/review".to_owned(),
            enabled: true,
            description: "review helper".to_owned(),
            capabilities: harw_config::PluginCapabilitiesToml {
                tools: vec!["read_file".to_owned()],
                skills: vec!["code-review".to_owned()],
                mcps: Vec::new(),
                channels: Vec::new(),
                network_egress: vec!["api.example.invalid".to_owned()],
            },
        }
    }

    fn context_with_plugins(
        plugins: Vec<harw_config::PluginToml>,
    ) -> TestResult<(OpContext, std::path::PathBuf)> {
        let (base, root) = test_context()?;
        let mut config = harw_config::ResolvedConfig::default();
        for plugin in plugins {
            config.plugins.insert(plugin.name.clone(), plugin);
        }
        let mut services = ServiceMap::new();
        services.insert(std::sync::Arc::new(config));
        Ok((
            OpContext::new(
                base.session_id().clone(),
                base.turn_id().clone(),
                base.sandbox().clone(),
                services,
            ),
            root,
        ))
    }

    fn args(action: &str, target: Option<&str>) -> PluginsArgs {
        PluginsArgs {
            action: Some(action.to_owned()),
            target: target.map(str::to_owned),
            value: None,
        }
    }

    #[tokio::test]
    async fn plugins_list_with_config_service_reports_configured_plugins() -> TestResult {
        let mut disabled = sample_plugin();
        disabled.name = "archive".to_owned();
        disabled.enabled = false;
        disabled.capabilities.network_egress.clear();
        let (op_ctx, root) = context_with_plugins(vec![sample_plugin(), disabled])?;

        let result = super::plugins(&op_ctx, PluginsArgs::default()).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        let output = result.map_err(ctx("plugins list"))?;
        assert!(output.text.starts_with("2 konfigurierte(s) Plugin(s):"));
        let archive = output
            .text
            .find("- archive@")
            .ok_or(TestError::Missing("archive"))?;
        let review = output
            .text
            .find("- review@")
            .ok_or(TestError::Missing("review"))?;
        assert!(archive < review, "list must be sorted: {}", output.text);
        assert!(
            output
                .text
                .contains("- review@1.0.0 (enabled; egress: api.example.invalid): review helper")
        );
        assert!(output.text.contains("(disabled; kein Netzwerk-Egress)"));
        Ok(())
    }

    #[tokio::test]
    async fn plugins_list_without_plugins_reports_empty_catalog() -> TestResult {
        let (op_ctx, root) = context_with_plugins(Vec::new())?;
        let result = super::plugins(&op_ctx, args("list", None)).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        let output = result.map_err(ctx("plugins list"))?;
        assert_eq!(output.text, "Keine Plugins konfiguriert.");
        Ok(())
    }

    #[tokio::test]
    async fn plugins_show_reports_all_manifest_fields() -> TestResult {
        let (op_ctx, root) = context_with_plugins(vec![sample_plugin()])?;
        let result = super::plugins(&op_ctx, args("show", Some("review"))).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        let output = result.map_err(ctx("plugins show"))?;
        for expected in [
            "review@1.0.0 (enabled)",
            "enabled: true",
            "description: review helper",
            "source: git+https://example.invalid/review",
            "tools: read_file",
            "skills: code-review",
            "mcps: -",
            "channels: -",
            "network_egress: api.example.invalid",
        ] {
            assert!(
                output.text.contains(expected),
                "missing '{expected}' in {}",
                output.text
            );
        }
        let data = output.data.ok_or(TestError::Missing("show data"))?;
        assert_eq!(data["name"], "review");
        assert_eq!(data["enabled"], true);
        assert_eq!(
            data["capabilities"]["network_egress"][0],
            "api.example.invalid"
        );
        Ok(())
    }

    #[tokio::test]
    async fn plugins_show_requires_known_target() -> TestResult {
        let (op_ctx, root) = context_with_plugins(vec![sample_plugin()])?;
        let missing = super::plugins(&op_ctx, args("show", None)).await;
        let unknown = super::plugins(&op_ctx, args("show", Some("nope"))).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        assert!(
            matches!(missing, Err(OpError::InvalidArguments(message)) if message == "action 'show' requires a plugin name")
        );
        assert!(
            matches!(unknown, Err(OpError::InvalidArguments(message)) if message == "unknown plugin 'nope'")
        );
        Ok(())
    }

    #[tokio::test]
    async fn plugins_enable_disable_are_static_not_available() -> TestResult {
        let (op_ctx, root) = context_with_plugins(vec![sample_plugin()])?;
        let target = "private-plugin-name";
        let enable = super::plugins(&op_ctx, args("enable", Some(target))).await;
        let disable = super::plugins(&op_ctx, args("disable", Some(target))).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        for result in [enable, disable] {
            match result {
                Err(OpError::NotAvailable(message)) => {
                    assert_eq!(message, super::TOGGLE_UNAVAILABLE);
                    assert!(message.contains("enabled = true|false"));
                    assert!(!message.contains(target));
                }
                other => {
                    return Err(TestError::Unexpected(format!(
                        "expected NotAvailable, got: {other:?}"
                    )));
                }
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn plugins_lifecycle_and_unknown_actions_are_rejected() -> TestResult {
        let (op_ctx, root) = context_with_plugins(vec![sample_plugin()])?;
        let install = super::plugins(&op_ctx, args("install", Some("review"))).await;
        let unknown = super::plugins(&op_ctx, args("frobnicate", None)).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        assert!(
            matches!(install, Err(OpError::NotAvailable(message)) if message == super::LIFECYCLE_UNAVAILABLE)
        );
        assert!(matches!(unknown, Err(OpError::InvalidArguments(_))));
        Ok(())
    }

    #[tokio::test]
    async fn plugins_target_bearing_action_does_not_leak_input() -> TestResult {
        let (op_ctx, root) = test_context()?;
        let action = "install-sensitive-action";
        let target = "private-plugin-name";
        let value = "secret-registry-value";
        let result = super::plugins(
            &op_ctx,
            PluginsArgs {
                action: Some(action.to_owned()),
                target: Some(target.to_owned()),
                value: Some(value.to_owned()),
            },
        )
        .await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert_eq!(message, "extension registry is not available");
                assert!(!message.contains(action));
                assert!(!message.contains(target));
                assert!(!message.contains(value));
                Ok(())
            }
            other => Err(TestError::Unexpected(format!(
                "expected NotAvailable, got: {other:?}"
            ))),
        }
    }
}
