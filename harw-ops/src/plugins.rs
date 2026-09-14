//! `/plugins` — installierte Plugins auflisten und verwalten.
//!
//! # Verantwortungsbereich
//! Implementiert die `plugins`-Operation als `/plugins`-Command (tui_only).
//! `list` liest den konfigurierten Plugin-Katalog aus dem
//! `Arc<ResolvedConfig>`-Service in [`OpContext`]. Ohne diesen Service bleibt
//! die Operation fail-closed ([`OpError::NotAvailable`]).
//!
//! # Schlüsseltypen
//! - [`PluginsArgs`] — typisierte Felder für Action (`list`, `install`, `activate`,
//!   `uninstall`) und optionalen Plugin-Namen (`target`).
//! - `PluginsOperation` — generiert vom `#[operation]`-Makro
//!
//! # Nebenläufigkeit
//! `PluginsOperation` ist `Send + Sync` (Unit-Struct ohne inneren Zustand, generiert
//! durch `#[operation]`-Makro).
//!
//! # Fehlertypen
//! Diese Operation erzeugt stets [`OpError::NotAvailable`]. Die Fehlermeldung ist
//! statisch und enthält keine übergebenen Action-, Target- oder Value-Werte.
//!
//! # Ausstehend
//! Die Verdrahtung mit `harw-extension-api` ist noch nicht implementiert. Der echte
//! Body wird ergänzt, sobald `harw-extension-api` eine stabile Registry-API bereitstellt.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_ops::plugins::PluginsArgs;
//! // Die Operation wird über den harw-operations-Registry-Mechanismus aufgerufen.
//! ```

use harw_config::ResolvedConfig;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use std::sync::Arc;

/// Argumente für die `/plugins`-Operation.
///
/// # Beschreibung
/// Trägt das Sub-Kommando (`action`) und den optionalen Plugin-Namen (`target`).
/// Die Felder werden durch das [`harw_macros::FromRawArgs`]-Derive direkt aus
/// den tokenisierten TUI-Rohargumenten befüllt — jedes Feld erhält genau das
/// Token an der entsprechenden Position (0-basiert).
///
/// # Felder
/// - `action` (`Option<String>`): Token 0 — Sub-Kommando; gültige Werte:
///   `"list"` (Standard), `"install"`, `"activate"`, `"uninstall"`.
///   `None` bezeichnet das Standard-Listing. Alle Actions werden derzeit
///   fail-closed abgewiesen.
/// - `target` (`Option<String>`): Token 1 — Plugin-Name (z. B. `"my-plugin"`).
///   Für künftige `install`, `activate` und `uninstall`-Integration vorgesehen;
///   derzeit wird der Wert nicht verarbeitet.
/// - `value` (`Option<String>`): Token 2 — dritter Parameter (reserviert für
///   zukünftige Erweiterungen; aktuell ungenutzt).
///
/// # Hinweis
/// Install, Activate und Uninstall bleiben Command-only. Die Operation hat kein
/// Model-Tool-Surface, solange die Registry-Integration fehlt.
///
/// # Spec-Referenz
/// Plan v2 — `/plugins` Meta-Definition.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct PluginsArgs {
    /// Sub-Kommando: `"list"` (Standard), `"install"`, `"activate"`, `"uninstall"`. Token 0.
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

/// Lehnt Plugin-Listing und -Verwaltung ab, solange die Registry-Boundary fehlt.
///
/// # Beschreibung
/// Die Anbindung an `harw-extension-api` steht aus. Die Operation verarbeitet
/// die bereits geparsten Argumente nicht weiter und schlägt für alle Actions,
/// einschließlich des Standard-Listings, fail-closed fehl.
///
/// # Argumente
/// - `_ctx` (`&OpContext`): Session-Kontext (aktuell ungenutzt).
/// - `_args` (`PluginsArgs`): Typisierte Sub-Kommando-Argumente, die bis zur
///   Registry-Integration bewusst nicht verarbeitet werden.
///
/// # Rückgabe
/// Immer [`OpError::NotAvailable`], bis eine Extension-Registry-Boundary im
/// [`OpContext`] verfügbar ist.
///
/// # Fehler
/// Gibt [`OpError::InvalidArguments`] zurück, wenn `json_args` nicht in
/// [`PluginsArgs`] deserialisiert werden kann (wird vom Makro gehandhabt), oder
/// [`OpError::NotAvailable`] für jede erfolgreich geparste Invocation.
///
/// # Nebenläufigkeit
/// Zustandslos; keine Locks, keine Threads, kein geteilter Zustand.
///
/// # Ausstehend
/// Ein echter Body darf erst mit einer expliziten `harw-extension-api`-
/// Registry-Boundary ergänzt werden.
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
    command(path = "/plugins", visibility = "tui_only")
)]
async fn plugins(ctx: &OpContext, args: PluginsArgs) -> Result<OpOutput, OpError> {
    let Some(config) = ctx.service::<Arc<ResolvedConfig>>() else {
        return Err(OpError::NotAvailable(
            "extension registry is not available".to_owned(),
        ));
    };

    match args.action.as_deref().unwrap_or("list") {
        "list" => {
            if config.plugins.is_empty() {
                return Ok(OpOutput::from("Keine Plugins konfiguriert.".to_owned()));
            }
            let mut names: Vec<&String> = config.plugins.keys().collect();
            names.sort();
            let mut lines = vec![format!("{} konfigurierte(s) Plugin(s):", names.len())];
            for name in names {
                let plugin = &config.plugins[name];
                let status = if plugin.enabled { "enabled" } else { "disabled" };
                lines.push(format!(
                    "- {name}@{} ({status}): {}",
                    plugin.version, plugin.description
                ));
            }
            Ok(OpOutput::from(lines.join("\n")))
        }
        "install" | "activate" | "uninstall" => Err(OpError::NotAvailable(
            "plugin lifecycle mutation is not available".to_owned(),
        )),
        unknown => Err(OpError::InvalidArguments(format!(
            "unknown /plugins action '{unknown}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::PluginsArgs;
    use crate::testutil::toks;
    use harw_operations::operation::{CommandVisibility, Surface};
    use harw_operations::{FromRawArgs, OpContext, OpError, Operation, context::ServiceMap};
    use harw_sandbox::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_context() -> (OpContext, std::path::PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-plugins-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("ws")).expect("create test workspace");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .expect("build workspace registry");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .expect("resolve workspace binding");
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        (
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new()),
            root,
        )
    }

    #[test]
    fn test_plugins_args_from_raw_args_sets_action() {
        let args = PluginsArgs::from_raw_args(&toks(&["install"]));
        match args {
            Ok(a) => {
                assert_eq!(a.action.as_deref(), Some("install"));
                assert!(a.target.is_none());
            }
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    #[test]
    fn test_plugins_args_from_raw_args_install_preserves_target() {
        let args = PluginsArgs::from_raw_args(&toks(&["install", "my-plugin"]));
        match args {
            Ok(a) => {
                assert_eq!(a.action.as_deref(), Some("install"));
                assert_eq!(a.target.as_deref(), Some("my-plugin"));
                assert!(a.value.is_none());
            }
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    #[test]
    fn test_plugins_args_from_raw_args_empty_tokens_sets_action_none() {
        let args = PluginsArgs::from_raw_args(&toks(&[]));
        match args {
            Ok(a) => {
                assert!(a.action.is_none());
                assert!(a.target.is_none());
            }
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
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
    async fn plugins_default_list_returns_static_not_available() {
        let (ctx, root) = test_context();
        let result = super::plugins(&ctx, PluginsArgs::default()).await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        assert!(
            matches!(result, Err(OpError::NotAvailable(message)) if message == "extension registry is not available")
        );
    }

    #[tokio::test]
    async fn plugins_list_with_config_service_reports_configured_plugins() {
        use harw_config::PluginToml;
        use harw_operations::context::ServiceMap;
        use std::sync::Arc;

        let (ctx, root) = test_context();
        let mut config = harw_config::ResolvedConfig::default();
        config.plugins.insert(
            "review".to_owned(),
            PluginToml {
                name: "review".to_owned(),
                version: "1.0.0".to_owned(),
                source: String::new(),
                enabled: true,
                description: "review helper".to_owned(),
                capabilities: Default::default(),
            },
        );
        let mut services = ServiceMap::new();
        services.insert(Arc::new(config));
        let ctx = OpContext::new(
            ctx.session_id().clone(),
            ctx.turn_id().clone(),
            ctx.sandbox().clone(),
            services,
        );

        let result = super::plugins(&ctx, PluginsArgs::default()).await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        match result {
            Ok(output) => assert!(output.text.contains("review")),
            other => panic!("expected Ok listing, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn plugins_target_bearing_action_does_not_leak_input() {
        let (ctx, root) = test_context();
        let action = "install-sensitive-action";
        let target = "private-plugin-name";
        let value = "secret-registry-value";
        let result = super::plugins(
            &ctx,
            PluginsArgs {
                action: Some(action.to_owned()),
                target: Some(target.to_owned()),
                value: Some(value.to_owned()),
            },
        )
        .await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert_eq!(message, "extension registry is not available");
                assert!(!message.contains(action));
                assert!(!message.contains(target));
                assert!(!message.contains(value));
            }
            other => panic!("expected NotAvailable, got: {other:?}"),
        }
    }
}
