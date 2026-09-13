//! `/skills` — Skill-Kataloggrenze.
//!
//! `list` und `show` lesen den konfigurierten Skill-Katalog aus dem
//! `Arc<ResolvedConfig>`-Service in [`OpContext`]. Ohne diesen Service bleibt
//! die Operation fail-closed ([`OpError::NotAvailable`]).
//!
//! # Verantwortungsbereich
//! Implementiert die `skills`-Operation ausschließlich als `/skills`-Command
//! (`channel_reduced`). Bis zur harw-catalog-Anbindung ist sie fail-closed und
//! wird nicht als Model-Tool exponiert.
//!
//! # Schlüsseltypen
//! - [`SkillsArgs`] — typisierte Felder für Action (`list` | `activate` |
//!   `deactivate` | `show`) und optionalen Skill-Namen (`target`).
//! - [`SkillsOperation`] — generiertes Unit-Struct (via `#[operation]`-Makro)
//!
//! # Surface-Matrix
//! | Surface | Sichtbarkeit      |
//! |---------|-------------------|
//! | Command | `channel_reduced` |
//!
//! # Ausstehende Anbindung
//! Der Skill-Katalog ist noch nicht verfügbar. Die Operation gibt für jede
//! Aktion denselben statischen [`OpError::NotAvailable`] zurück und echo't
//! keine vom Aufrufer gelieferten Werte.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_ops::skills::SkillsOperation;
//! use harw_operations::Operation;
//!
//! let op = SkillsOperation;
//! assert_eq!(op.meta().name, "skills");
//! ```

use harw_config::ResolvedConfig;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use std::sync::Arc;

// ── Args ──────────────────────────────────────────────────────────────────────

/// Argumente für die `/skills`-Operation.
///
/// # Beschreibung
/// Trägt das Sub-Kommando (`action`) und einen optionalen Skill-Namen (`target`).
/// Die Felder werden durch das [`harw_macros::FromRawArgs`]-Derive direkt aus
/// den tokenisierten TUI-Rohargumenten befüllt — jedes Feld erhält genau das
/// Token an der entsprechenden Position (0-basiert).
///
/// - `None` oder `action = Some("list")` → Skills auflisten (Standard-Verhalten;
///   derzeit wegen der fehlenden Katalog-Anbindung nicht verfügbar).
/// - `action = Some("activate")` mit `target = Some("<name>")` → benannten Skill
///   aktivieren (derzeit nicht verfügbar).
///
/// # Felder
/// - `action` (`Option<String>`): Token 0 — Sub-Kommando: `"list"` (Standard),
///   `"activate"`, `"deactivate"`, `"show"`. `None` wird wie `"list"` behandelt.
/// - `target` (`Option<String>`): Token 1 — Skill-Name. Relevant für `activate`,
///   `deactivate` und `show`; bei `list` ignoriert.
/// - `value` (`Option<String>`): Token 2 — dritter Parameter (reserviert für
///   zukünftige Erweiterungen; aktuell ungenutzt).
///
/// # Deserialisierung
/// Das `#[operation]`-Makro deserialisiert `input.json_args` via `serde_json`
/// in diesen Typ. Bei Command-Aufrufen ist `json_args == Null`, weshalb
/// [`Default::default`] greift und `action = None` ergibt. Die Operation ist
/// ausschließlich über den `/skills`-Command erreichbar; jede Ausführung wird
/// mit [`OpError::NotAvailable`] abgewiesen.
///
/// # Spec-Referenz
/// Spec-Abschnitt: `/skills` — Args.
///
/// # Beispiel
/// ```rust
/// use harw_ops::skills::SkillsArgs;
/// use harw_operations::FromRawArgs;
///
/// let list_args = SkillsArgs::default();
/// assert!(list_args.action.is_none());
///
/// // "/skills activate my-skill" → tokens = ["activate", "my-skill"]
/// let args = SkillsArgs::from_raw_args(&["activate".to_owned(), "my-skill".to_owned()]).unwrap();
/// assert_eq!(args.action.as_deref(), Some("activate"));
/// assert_eq!(args.target.as_deref(), Some("my-skill"));
/// ```
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct SkillsArgs {
    /// Sub-Kommando: `"list"` (Standard), `"activate"`, `"deactivate"`, `"show"`. Token 0.
    ///
    /// `None` wird wie `"list"` behandelt. Alle Werte werden derzeit nur für
    /// das Parsen erhalten; die Ausführung bleibt fail-closed.
    #[serde(default)]
    #[raw(first)]
    pub action: Option<String>,
    /// Skill-Name (z. B. `"my-skill"`). Token 1. `None` wenn nicht angegeben.
    #[serde(default)]
    #[raw(nth = 1)]
    pub target: Option<String>,
    /// Dritter Parameter (reserviert). Token 2. `None` wenn nicht angegeben.
    #[serde(default)]
    #[raw(nth = 2)]
    pub value: Option<String>,
}

// ── Operation ─────────────────────────────────────────────────────────────────

/// Greift auf den Skill-Katalog zu (derzeit nicht verfügbar).
///
/// # Beschreibung
/// Die harw-catalog-Anbindung fehlt noch. Deshalb werden `list`, `show`,
/// `activate`, `deactivate` und unbekannte Sub-Befehle gleichermaßen statisch
/// mit [`OpError::NotAvailable`] abgewiesen. Weder `action` noch `target` oder
/// `value` werden in die Fehlermeldung interpoliert.
///
/// # Argumente
/// - `_ctx` (`&OpContext`): Ausführungskontext — wird bis zur Katalog-Anbindung
///   nicht benötigt.
/// - `args` (`SkillsArgs`): Typisierte Sub-Kommando-Argumente; sie werden
///   erhalten, aber bis zur Katalog-Anbindung nicht ausgewertet.
///
/// # Rückgabe
/// - `Err(OpError::NotAvailable)`: Die Katalog-Anbindung ist nicht verfügbar.
///
/// # Fehler
/// - [`OpError::NotAvailable`]: Jede Ausführung, bis harw-catalog angebunden ist.
///
/// # Nebenläufigkeit
/// Zustandslos; sicher aus mehreren Threads aufrufbar.
///
/// # Spec-Referenz
/// Spec-Abschnitt: `/skills` — Body, Command-Surface, Soft-Refuse.
///
/// # Beispiel
/// ```rust,no_run
/// // Aufruf erfolgt über Operation::run() — direkte Verwendung nur im Test-Kontext.
/// ```
#[operation(
    name = "skills",
    summary = "Skill-Katalog: list/show gegen die geladene ResolvedConfig.skills.",
    domain = "catalog_config",
    permission = "operator",
    command(path = "/skills", visibility = "channel_reduced")
)]
async fn skills(ctx: &OpContext, args: SkillsArgs) -> Result<OpOutput, OpError> {
    let Some(config) = ctx.service::<Arc<ResolvedConfig>>() else {
        return Err(OpError::NotAvailable(
            "skill catalog integration is not available".to_owned(),
        ));
    };

    match args.action.as_deref().unwrap_or("list") {
        "list" => {
            if config.skills.is_empty() {
                return Ok(OpOutput {
                    text: "Keine Skills konfiguriert.".to_owned(),
                });
            }
            let mut names: Vec<&String> = config.skills.keys().collect();
            names.sort();
            let mut lines = vec![format!("{} konfigurierte(r) Skill(s):", names.len())];
            for name in names {
                let skill = &config.skills[name];
                let status = if skill.enabled { "enabled" } else { "disabled" };
                lines.push(format!("- {name} ({status}): {}", skill.description));
            }
            Ok(OpOutput {
                text: lines.join("\n"),
            })
        }
        "show" => {
            let Some(target) = args.target.as_deref() else {
                return Err(OpError::InvalidArguments(
                    "action 'show' requires a skill name".to_owned(),
                ));
            };
            let Some(skill) = config.skills.get(target) else {
                return Err(OpError::InvalidArguments(format!(
                    "unknown skill '{target}'"
                )));
            };
            Ok(OpOutput {
                text: format!(
                    "{} (enabled={}): {}\ntools: {}\nmcps: {}",
                    skill.name,
                    skill.enabled,
                    skill.description,
                    skill.tools.join(", "),
                    skill.mcps.join(", ")
                ),
            })
        }
        "activate" | "deactivate" => Err(OpError::NotAvailable(
            "skill activation state changes are not available".to_owned(),
        )),
        unknown => Err(OpError::InvalidArguments(format!(
            "unknown /skills action '{unknown}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{SkillsArgs, SkillsOperation};
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
            std::env::temp_dir().join(format!("harw-skills-test-{}-{id}", std::process::id()));
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
    fn test_skills_args_from_raw_args_sets_action() {
        let args = SkillsArgs::from_raw_args(&toks(&["list"]));
        match args {
            Ok(a) => {
                assert_eq!(a.action.as_deref(), Some("list"));
                assert!(a.target.is_none());
            }
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    #[test]
    fn test_skills_args_from_raw_args_activate_preserves_target() {
        let args = SkillsArgs::from_raw_args(&toks(&["activate", "my-skill"]));
        match args {
            Ok(a) => {
                assert_eq!(a.action.as_deref(), Some("activate"));
                assert_eq!(a.target.as_deref(), Some("my-skill"));
                assert!(a.value.is_none());
            }
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    #[test]
    fn test_skills_args_from_raw_args_empty_tokens_sets_action_none() {
        let args = SkillsArgs::from_raw_args(&toks(&[]));
        match args {
            Ok(a) => {
                assert!(a.action.is_none());
                assert!(a.target.is_none());
            }
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    #[test]
    fn skills_operation_is_command_only() {
        let surfaces = &SkillsOperation.meta().surfaces;

        assert!(
            !surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. }))
        );
        assert!(surfaces.iter().any(|surface| {
            matches!(
                surface,
                Surface::Command {
                    path: "/skills",
                    visibility: CommandVisibility::ChannelReduced,
                }
            )
        }));
    }

    #[tokio::test]
    async fn skills_default_and_list_return_not_available() {
        let (ctx, root) = test_context();
        let expected = "skill catalog integration is not available";
        assert!(matches!(
            super::skills(&ctx, SkillsArgs::default()).await,
            Err(OpError::NotAvailable(message)) if message == expected
        ));
        assert!(matches!(
            super::skills(&ctx, SkillsArgs { action: Some("list".to_owned()), target: None, value: None }).await,
            Err(OpError::NotAvailable(message)) if message == expected
        ));
        std::fs::remove_dir_all(root).expect("remove test workspace");
    }

    #[tokio::test]
    async fn skills_list_with_config_service_reports_configured_skills() {
        use harw_config::SkillToml;
        use harw_operations::context::ServiceMap;
        use std::sync::Arc;

        let (mut ctx, root) = test_context();
        let mut config = harw_config::ResolvedConfig::default();
        config.skills.insert(
            "review".to_owned(),
            SkillToml {
                name: "review".to_owned(),
                enabled: true,
                description: "code review".to_owned(),
                instructions_file: None,
                tools: vec![],
                mcps: vec![],
            },
        );
        let mut services = ServiceMap::new();
        services.insert(Arc::new(config));
        ctx = OpContext::new(
            ctx.session_id().clone(),
            ctx.turn_id().clone(),
            ctx.sandbox().clone(),
            services,
        );

        let result = super::skills(&ctx, SkillsArgs::default()).await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        match result {
            Ok(output) => assert!(output.text.contains("review")),
            other => panic!("expected Ok listing, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn skills_target_bearing_action_returns_not_available_without_input_leakage() {
        let (ctx, root) = test_context();
        let action = "activate";
        let target = "secret-skill-target";
        let value = "secret-value";
        let result = super::skills(
            &ctx,
            SkillsArgs {
                action: Some(action.to_owned()),
                target: Some(target.to_owned()),
                value: Some(value.to_owned()),
            },
        )
        .await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert_eq!(message, "skill catalog integration is not available");
                assert!(!message.contains(action));
                assert!(!message.contains(target));
                assert!(!message.contains(value));
            }
            other => panic!("expected NotAvailable, got: {other:?}"),
        }
    }
}
