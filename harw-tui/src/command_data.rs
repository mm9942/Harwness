//! Befehlsausführung mit strukturierter Ausgabe (`OpOutput.data`).
//!
//! [`crate::command_exec::execute_command_as`] liefert aus
//! Kompatibilitätsgründen nur den Anzeigetext. TUI-Ansichten (Workbench,
//! Kanban, Modelle je Rolle, Wissensbrowser, …) brauchen jedoch die
//! strukturierten Daten, die eine Operation in [`OpOutput::data`] ablegt.
//! [`execute_command_with_data`] führt deshalb eine beliebige `/command`-Zeile
//! über dieselbe Registry-/Admission-/Adapter-Pipeline aus und gibt das
//! vollständige [`OpOutput`] zurück. Die TUI öffnet dafür keine eigenen
//! Speicher (etwa einen `KnowledgeStore`); alle Daten kommen aus den Ops.
//!
//! Verallgemeinert den früheren `/export`-Sonderpfad in `app.rs`
//! (`execute_export_command_with_data`) ohne dessen Namensfilter.

use harw_authority::SandboxSpec;
use harw_operations::adapter::CommandAdapter;
use harw_operations::{OpOutput, ServiceMap};
use harw_types::SessionId;

use crate::{
    CapabilitySet, CommandAction, CommandError, CommandRegistry, DispatchContext, Invocation,
    InvocationSurface, PermissionTier,
};

/// Führt eine `/command`-Zeile aus und liefert das vollständige [`OpOutput`].
///
/// # Beschreibung
/// 1. `raw_line` wird über [`crate::classify_input`] geparst; nur
///    `/befehl …`-Zeilen sind zulässig (Shell, Notizen, Erwähnungen und Chat
///    werden abgelehnt).
/// 2. Die Admission (unbekannter Befehl mit Vorschlag, Berechtigungsstufe
///    gegen `caller_tier`, Scope) läuft über [`CommandRegistry::dispatch`]
///    mit TUI-Oberfläche.
/// 3. Der Adapter mit dem kanonischen Pfad `/<name>` wird gesucht, ein
///    [`harw_operations::OpContext`] mit frischer Turn-ID und `services()`
///    gebaut und der Adapter dispatcht.
///
/// `services` wird **nur** nach erfolgreicher Admission aufgerufen.
///
/// # Argumente
/// - `adapters`: alle registrierten Command-Adapter.
/// - `sandbox`: Sandbox der Sitzung (wird für den Kontext geklont).
/// - `session_id`: aktuelle Sitzung.
/// - `caller_tier`: Berechtigungsstufe des Aufrufers.
/// - `raw_line`: die unveränderte Befehlszeile, z. B. `/workbench show`.
/// - `services`: liefert die [`ServiceMap`] für genau diesen Dispatch.
///
/// # Fehler
/// Ein lokalisierter Anzeigetext bei Parse-Fehler, Nicht-Befehl, abgelehnter
/// Admission, fehlendem Adapter oder Fehler der Operation.
///
/// # Nebenläufigkeit
/// `async`; awaitet höchstens einen `CommandAdapter::dispatch`-Aufruf.
#[cfg(test)]
pub(crate) async fn execute_command_with_data<F>(
    adapters: &[CommandAdapter],
    sandbox: &SandboxSpec,
    session_id: &SessionId,
    caller_tier: PermissionTier,
    raw_line: &str,
    services: F,
) -> Result<OpOutput, String>
where
    F: FnOnce() -> ServiceMap,
{
    execute_command_with_data_for(
        harw_config::UiLanguage::English,
        adapters,
        sandbox,
        session_id,
        caller_tier,
        raw_line,
        services,
    )
    .await
}

/// Explicit session locale; no synchronous language scope crosses dispatch await.
pub(crate) async fn execute_command_with_data_for<F>(
    language: harw_config::UiLanguage,
    adapters: &[CommandAdapter],
    sandbox: &SandboxSpec,
    session_id: &SessionId,
    caller_tier: PermissionTier,
    raw_line: &str,
    services: F,
) -> Result<OpOutput, String>
where
    F: FnOnce() -> ServiceMap,
{
    use crate::i18n::{tr_args_for, tr_for};
    let invocation = crate::classify_input(raw_line).map_err(|error| {
        tr_args_for(
            language,
            "dispatch.rejected",
            "Input rejected: {error}",
            &[("error", &error.present_for(language, raw_line))],
        )
    })?;
    let typed = match &invocation {
        Invocation::Command { name, .. } => format!("/{name}"),
        _ => {
            return Err(
                tr_for(language, "dispatch.not_command", "Expected a slash command").to_owned(),
            );
        }
    };
    let registry = crate::i18n::with_language(language, || {
        CommandRegistry::from_command_adapters(adapters)
    });
    let action = registry
        .dispatch(
            DispatchContext {
                caller_tier,
                surface: InvocationSurface::Tui,
                capabilities: CapabilitySet::default(),
            },
            invocation,
        )
        .map_err(|error| render_admission_error(language, &typed, &error))?;
    let CommandAction::Command(spec, raw_args) = action else {
        return Err(
            tr_for(language, "dispatch.not_command", "Expected a slash command").to_owned(),
        );
    };
    let path = format!("/{}", spec.name.as_str());
    let Some(adapter) = adapters.iter().find(|adapter| adapter.path() == path) else {
        return Err(tr_args_for(
            language,
            "dispatch.unknown",
            "Unknown command: {command}",
            &[("command", &path)],
        ));
    };
    let ctx = harw_operations::OpContext::new(
        session_id.clone(),
        harw_types::TurnId::new(),
        sandbox.clone(),
        services(),
    );
    let mut output = adapter.dispatch(&ctx, raw_args).await.map_err(|error| {
        tr_args_for(
            language,
            "dispatch.error",
            "Error: {error}",
            &[("error", &error.to_string())],
        )
    })?;
    if let Some(presentation) =
        crate::operation_feedback::project(adapter.operation().meta(), language, &output)
    {
        let _timing = presentation.timing;
        output.text = presentation.text;
    }
    if path == "/settings" {
        output.text = crate::settings_view::present_output(language, &output);
    }
    Ok(output)
}

pub(crate) fn render_admission_error(
    language: harw_config::UiLanguage,
    typed: &str,
    error: &CommandError,
) -> String {
    error.present_for(language, typed)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_operations::adapter::CommandAdapter;
    use harw_operations::operation::BusyAvailability;
    use harw_operations::{
        CommandVisibility, OpContext, OpFuture, OpInput, OpOutput, Operation, OperationCategory,
        OperationDomain, OperationMeta, PermissionTier, ServiceMap, Surface,
    };
    use harw_types::{SessionId, TenantId, WorkspaceId};

    use super::execute_command_with_data;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Fake-Operation mit strukturierter Ausgabe.
    struct DataOperation {
        meta: OperationMeta,
        dispatches: AtomicUsize,
    }

    impl DataOperation {
        fn new(permission: PermissionTier) -> Self {
            Self {
                meta: OperationMeta {
                    name: "test.board",
                    summary: "Test-Board mit Daten.",
                    domain: OperationDomain::Misc,
                    permission,
                    surfaces: vec![Surface::Command {
                        path: "/board",
                        visibility: CommandVisibility::TuiOnly,
                    }],
                    aliases: &["brett"],
                    category: OperationCategory::Misc,
                    args_schema: None,
                    output_schema: None,
                    busy: BusyAvailability::Immediate,
                },
                dispatches: AtomicUsize::new(0),
            }
        }
    }

    impl Operation for DataOperation {
        fn meta(&self) -> &OperationMeta {
            &self.meta
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            self.dispatches.fetch_add(1, Ordering::Relaxed);
            Box::pin(async {
                Ok(OpOutput {
                    text: "Board".to_owned(),
                    data: Some(serde_json::json!({ "lanes": [{ "id": "todo" }] })),
                })
            })
        }
    }

    fn test_sandbox() -> TestResult<(SandboxSpec, PathBuf)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-tui-command-data-test-{}-{}",
            std::process::id(),
            id
        ));
        std::fs::create_dir_all(root.join("workspace")).map_err(ctx("temp workspace dir"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tui-test"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .map_err(ctx("workspace registry build"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("tui-test"),
                &WorkspaceId::from_str("workspace"),
            )
            .map_err(ctx("resolve workspace binding"))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace, Permission::WriteWorkspace]),
        );
        Ok((sandbox, root))
    }

    async fn run(
        operation: &Arc<DataOperation>,
        tier: PermissionTier,
        line: &str,
        built: &AtomicUsize,
    ) -> TestResult<Result<OpOutput, String>> {
        let adapters = CommandAdapter::from_operation(operation.clone());
        let (sandbox, root) = test_sandbox()?;
        let result =
            execute_command_with_data(&adapters, &sandbox, &SessionId::new(), tier, line, || {
                built.fetch_add(1, Ordering::Relaxed);
                ServiceMap::new()
            })
            .await;
        std::fs::remove_dir_all(root).ok();
        Ok(result)
    }

    #[tokio::test]
    async fn returns_structured_data_for_any_command() -> TestResult {
        let operation = Arc::new(DataOperation::new(PermissionTier::Observer));
        let built = AtomicUsize::new(0);
        let output = run(&operation, PermissionTier::Owner, "/board show", &built)
            .await?
            .map_err(TestError::Unexpected)?;
        assert_eq!(output.text, "Board");
        let data = output.data.ok_or(TestError::Missing("data"))?;
        assert_eq!(data["lanes"][0]["id"], "todo");
        assert_eq!(built.load(Ordering::Relaxed), 1);
        Ok(())
    }

    #[tokio::test]
    async fn alias_resolves_to_canonical_adapter() -> TestResult {
        let operation = Arc::new(DataOperation::new(PermissionTier::Observer));
        let built = AtomicUsize::new(0);
        let output = run(&operation, PermissionTier::Owner, "/brett", &built)
            .await?
            .map_err(TestError::Unexpected)?;
        assert_eq!(output.text, "Board");
        Ok(())
    }

    #[tokio::test]
    async fn denied_tier_neither_builds_services_nor_dispatches() -> TestResult {
        let operation = Arc::new(DataOperation::new(PermissionTier::Operator));
        let built = AtomicUsize::new(0);
        let result = run(&operation, PermissionTier::Observer, "/board show", &built).await?;
        assert_eq!(
            result.err().as_deref(),
            Some("Permission denied: /board requires Operator; current tier is Observer")
        );
        assert_eq!(built.load(Ordering::Relaxed), 0);
        assert_eq!(operation.dispatches.load(Ordering::Relaxed), 0);
        Ok(())
    }

    #[tokio::test]
    async fn unknown_command_defaults_to_english() -> TestResult {
        let operation = Arc::new(DataOperation::new(PermissionTier::Observer));
        let built = AtomicUsize::new(0);
        let result = run(&operation, PermissionTier::Owner, "/nichtda", &built).await?;
        let error = result.err().ok_or(TestError::Missing("error"))?;
        assert!(error.starts_with("Unknown command: /nichtda"), "{error}");
        assert_eq!(built.load(Ordering::Relaxed), 0);
        Ok(())
    }

    #[tokio::test]
    async fn non_command_lines_are_rejected() -> TestResult {
        let operation = Arc::new(DataOperation::new(PermissionTier::Observer));
        let built = AtomicUsize::new(0);
        for line in ["hallo welt", "!ls"] {
            let result = run(&operation, PermissionTier::Owner, line, &built).await?;
            let error = result.err().ok_or(TestError::Missing("error"))?;
            assert!(error.contains("slash command"), "{line}: {error}");
        }
        assert_eq!(built.load(Ordering::Relaxed), 0);
        assert_eq!(operation.dispatches.load(Ordering::Relaxed), 0);
        Ok(())
    }
    struct SkillPersistence {
        changed: bool,
        fail: bool,
    }
    impl harw_ops::skills::SkillStatePersistence for SkillPersistence {
        fn set_skill_enabled(
            &self,
            _name: &str,
            _enabled: bool,
        ) -> Result<harw_ops::skills::SkillStateOutcome, harw_operations::OpError> {
            if self.fail {
                return Err(harw_operations::OpError::Execution(
                    "save refused detail".into(),
                ));
            }
            Ok(harw_ops::skills::SkillStateOutcome {
                manifest: PathBuf::from("/active-profile/skills/review/skill.toml"),
                changed: self.changed,
            })
        }
    }
    fn skill_services(changed: bool, fail: bool) -> ServiceMap {
        let mut config = harw_config::ResolvedConfig::default();
        config.skills.insert(
            "review".into(),
            harw_config::SkillToml {
                name: "review".into(),
                enabled: false,
                description: "user description aktiviert".into(),
                instructions_file: None,
                tools: vec![],
                mcps: vec![],
            },
        );
        let mut services = ServiceMap::new();
        services.insert(Arc::new(config));
        let persistence: Arc<dyn harw_ops::skills::SkillStatePersistence> =
            Arc::new(SkillPersistence { changed, fail });
        services.insert(persistence);
        services
    }
    #[tokio::test]
    async fn operation_feedback_real_skill_dispatch_retains_explicit_locale_and_typed_errors()
    -> TestResult {
        use harw_config::UiLanguage::{English, German};
        let adapters = CommandAdapter::from_operation(Arc::new(harw_ops::skills::SkillsOperation));
        let (sandbox, root) = test_sandbox()?;
        let session = SessionId::new();
        // Futures are created before poll; unrelated synchronous locale scopes
        // cannot affect the captured dispatch locale after suspension.
        let en = super::execute_command_with_data_for(
            English,
            &adapters,
            &sandbox,
            &session,
            PermissionTier::Owner,
            "/skills activate review",
            || skill_services(true, false),
        );
        let de = super::execute_command_with_data_for(
            German,
            &adapters,
            &sandbox,
            &session,
            PermissionTier::Owner,
            "/skills activate review",
            || skill_services(false, false),
        );
        crate::i18n::with_language(German, || {
            assert_eq!(
                crate::i18n::tr("operation_feedback.activated", "Activated"),
                "Aktiviert"
            )
        });
        tokio::task::yield_now().await;
        let (en, de) = tokio::join!(en, de);
        let en = en.map_err(TestError::Unexpected)?;
        let de = de.map_err(TestError::Unexpected)?;
        assert!(en.text.contains(": Activated ("), "{}", en.text);
        assert!(de.text.contains(": Aktiviert ("), "{}", de.text);
        assert!(de.text.contains("Bereits gesetzt."));
        assert!(en.text.contains("next config load (next session)"));
        assert_eq!(en.data.as_ref().unwrap()["ui_feedback"]["changed"], true);
        assert_eq!(de.data.as_ref().unwrap()["ui_feedback"]["changed"], false);
        let disabled = super::execute_command_with_data_for(
            English,
            &adapters,
            &sandbox,
            &session,
            PermissionTier::Owner,
            "/skills deactivate review",
            || skill_services(true, false),
        )
        .await
        .map_err(TestError::Unexpected)?;
        assert!(disabled.text.contains(": Deactivated ("));
        let failed = super::execute_command_with_data_for(
            German,
            &adapters,
            &sandbox,
            &session,
            PermissionTier::Owner,
            "/skills activate review",
            || skill_services(true, true),
        )
        .await;
        assert!(failed.is_err());
        assert!(failed.unwrap_err().contains("save refused detail"));
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;
        Ok(())
    }

    struct TestLiveModels;
    impl harw_ops::live_model::LiveModelControl for TestLiveModels {
        fn ensure_provider_ready(&self, _provider: &str) -> Result<(), String> {
            Ok(())
        }
        fn set_internal_model(
            &self,
            point: harw_config::InternalModelPoint,
            _choice: Option<harw_config::InternalModelChoice>,
        ) -> bool {
            point != harw_config::InternalModelPoint::SessionTitle
        }
    }
    #[tokio::test]
    async fn operation_feedback_actual_control_commands_keep_timing_and_root_data() -> TestResult {
        use harw_config::UiLanguage::{English, German};
        let mut registry = harw_operations::registry::OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        let adapters: Vec<_> = registry
            .iter()
            .flat_map(|op| CommandAdapter::from_operation(Arc::clone(op)))
            .collect();
        let (sandbox, root) = test_sandbox()?;
        let session = SessionId::new();
        let mut config = harw_config::ResolvedConfig::default();
        config.providers.insert("test-provider".into(),serde_json::from_value(serde_json::json!({"name":"test-provider","api":"openai-chat","base_url":"https://example.test","auth":"env:NEVER_READ_TEST_KEY","enabled":true})).map_err(ctx("provider"))?);
        config.models.insert(
            "test-model".into(),
            serde_json::from_value(
                serde_json::json!({"id":"test-model","provider":"test-provider"}),
            )
            .map_err(ctx("model"))?,
        );
        let controller: harw_operations::SharedSessionController =
            Arc::new(harw_operations::NullSessionController::new());
        let live: harw_ops::live_model::SharedLiveModelControl = Arc::new(TestLiveModels);
        let cell = harw_extension_api::approval_mode::ApprovalModeCell::default();
        let services = || {
            let mut services = ServiceMap::new();
            services.insert(Arc::new(config.clone()));
            services.insert(Arc::clone(&controller));
            services.insert(Arc::clone(&live));
            services.insert(cell.clone());
            services
        };
        for (command, timing, needle) in [
            (
                "/model switch test-model",
                "next_turn",
                "Model selection (session): test-provider/test-model.",
            ),
            (
                "/uia-model switch test-model",
                "next_uia_turn",
                "next UIA turn",
            ),
            (
                "/uia-worker-model switch test-model",
                "next_session",
                "next session",
            ),
            (
                "/models set explorer test-provider/test-model",
                "new_agents",
                "running agents keep their model",
            ),
            (
                "/models reset explorer",
                "new_agents",
                "role default applies",
            ),
            (
                "/models set session-title test-provider/test-model",
                "next_session",
                "next session",
            ),
            (
                "/models worker uia-writer test-provider/test-model",
                "routing_unconfirmed",
                "current-session routing is not confirmed",
            ),
            (
                "/models worker uia-writer uia",
                "routing_unconfirmed",
                "Worker roles uia-writer: uia.",
            ),
            (
                "/models worker all uia",
                "routing_unconfirmed",
                "Worker roles",
            ),
            (
                "/effort high",
                "next_turn",
                "Reasoning effort (session): high.",
            ),
            ("/effort clear", "next_turn", "Provider default"),
            (
                "/uia-effort high",
                "next_session",
                "Reasoning effort (uia): high.",
            ),
            ("/uia-effort clear", "next_session", "Role default"),
            ("/mode plan", "next_turn", "Mode requested: plan."),
            (
                "/mode default work",
                "next_session",
                "Default mode selected: work.",
            ),
            ("/permissions mode ask", "next_tool_call", "next tool call"),
        ] {
            let (shown, result) = crate::command_exec::dispatch_command_with_shell_result_for(
                English,
                &adapters,
                &sandbox,
                &session,
                PermissionTier::Owner,
                command,
                None,
                services,
            )
            .await;
            let output = result
                .ok_or(TestError::Missing("structured result"))?
                .map_err(TestError::Unexpected)?;
            assert_eq!(shown.text, output.text);
            assert!(output.text.contains(needle), "{command}: {}", output.text);
            assert_eq!(
                output.data.as_ref().unwrap()["ui_feedback"]["effective"],
                timing,
                "{command}"
            );
            if let Some(note) = output.data.as_ref().unwrap()["ui_feedback"]["note"].as_str() {
                assert!(
                    output.text.ends_with(note),
                    "save detail must survive: {command}"
                );
            }
            if command.starts_with("/models ") {
                assert!(output.data.as_ref().unwrap().get("action").is_some());
                assert!(output.data.as_ref().unwrap().get("persisted").is_some());
            }
        }
        let already = super::execute_command_with_data_for(
            German,
            &adapters,
            &sandbox,
            &session,
            PermissionTier::Owner,
            "/model switch test-model",
            services,
        )
        .await
        .map_err(TestError::Unexpected)?;
        assert!(already.text.contains("Bereits gesetzt."));
        let before = controller.snapshot();
        let invalid = super::execute_command_with_data_for(
            German,
            &adapters,
            &sandbox,
            &session,
            PermissionTier::Owner,
            "/model switch forbidden",
            services,
        )
        .await;
        assert!(invalid.is_err());
        assert_eq!(controller.snapshot().active_model, before.active_model);
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;
        Ok(())
    }
    struct DelayedFailure {
        meta: OperationMeta,
    }
    impl Operation for DelayedFailure {
        fn meta(&self) -> &OperationMeta {
            &self.meta
        }
        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async {
                tokio::task::yield_now().await;
                Err(harw_operations::OpError::Execution(
                    "backend detail aktiviert".into(),
                ))
            })
        }
    }
    #[tokio::test]
    async fn operation_feedback_delayed_errors_keep_captured_locale_and_backend_detail()
    -> TestResult {
        let base = DataOperation::new(PermissionTier::Observer);
        let adapters = CommandAdapter::from_operation(Arc::new(DelayedFailure { meta: base.meta }));
        let (sandbox, root) = test_sandbox()?;
        let session = SessionId::new();
        let en = super::execute_command_with_data_for(
            harw_config::UiLanguage::English,
            &adapters,
            &sandbox,
            &session,
            PermissionTier::Owner,
            "/board",
            ServiceMap::new,
        );
        let de = super::execute_command_with_data_for(
            harw_config::UiLanguage::German,
            &adapters,
            &sandbox,
            &session,
            PermissionTier::Owner,
            "/board",
            ServiceMap::new,
        );
        let (en, de) = tokio::join!(en, de);
        let en = en.expect_err("backend failure");
        let de = de.expect_err("backend failure");
        assert!(en.starts_with("Error: "), "{en}");
        assert!(de.starts_with("Fehler: "), "{de}");
        assert!(en.ends_with("backend detail aktiviert"));
        assert!(de.ends_with("backend detail aktiviert"));
        std::fs::remove_dir_all(root).map_err(ctx("remove workspace"))?;
        Ok(())
    }
}
