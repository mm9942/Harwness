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
/// Ein deutscher Anzeigetext bei Parse-Fehler, Nicht-Befehl, abgelehnter
/// Admission, fehlendem Adapter oder Fehler der Operation.
///
/// # Nebenläufigkeit
/// `async`; awaitet höchstens einen `CommandAdapter::dispatch`-Aufruf.
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
    let invocation =
        crate::classify_input(raw_line).map_err(|error| format!("Eingabe abgelehnt: {error}"))?;
    let typed = match &invocation {
        Invocation::Command { name, .. } => format!("/{name}"),
        _ => {
            return Err(format!(
                "Eingabe abgelehnt: '{}' ist kein /Befehl.",
                raw_line.trim()
            ));
        }
    };

    let registry = CommandRegistry::from_command_adapters(adapters);
    let action = registry
        .dispatch(
            DispatchContext {
                caller_tier,
                surface: InvocationSurface::Tui,
                capabilities: CapabilitySet::default(),
            },
            invocation,
        )
        .map_err(|error| render_admission_error(&typed, &error))?;

    let CommandAction::Command(spec, raw_args) = action else {
        return Err(format!(
            "Eingabe abgelehnt: {typed} ist kein ausführbarer Befehl."
        ));
    };
    let path = format!("/{}", spec.name.as_str());
    let Some(adapter) = adapters.iter().find(|adapter| adapter.path() == path) else {
        return Err(format!("Unbekannter Command: {path}"));
    };
    let ctx = harw_operations::OpContext::new(
        session_id.clone(),
        harw_types::TurnId::new(),
        sandbox.clone(),
        services(),
    );
    adapter
        .dispatch(&ctx, raw_args)
        .await
        .map_err(|error| format!("Fehler: {error}"))
}

/// Setzt einen Admission-Fehler in deutschen Anzeigetext um (gleiche
/// Wortlaute wie `command_exec`).
fn render_admission_error(typed: &str, error: &CommandError) -> String {
    match error {
        CommandError::UnknownCommand {
            suggestion: Some(suggestion),
            ..
        } => format!("Unbekannter Command: {typed} (meinten Sie /{suggestion}?)"),
        CommandError::UnknownCommand {
            suggestion: None, ..
        } => format!("Unbekannter Command: {typed}"),
        CommandError::PermissionDenied {
            required, actual, ..
        } => format!(
            "Berechtigung verweigert: {typed} erfordert {required:?}; aktuelle Stufe ist {actual:?}"
        ),
        other => format!("Eingabe abgelehnt: {other}"),
    }
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
            Some("Berechtigung verweigert: /board erfordert Operator; aktuelle Stufe ist Observer")
        );
        assert_eq!(built.load(Ordering::Relaxed), 0);
        assert_eq!(operation.dispatches.load(Ordering::Relaxed), 0);
        Ok(())
    }

    #[tokio::test]
    async fn unknown_command_is_reported_in_german() -> TestResult {
        let operation = Arc::new(DataOperation::new(PermissionTier::Observer));
        let built = AtomicUsize::new(0);
        let result = run(&operation, PermissionTier::Owner, "/nichtda", &built).await?;
        let error = result.err().ok_or(TestError::Missing("error"))?;
        assert!(
            error.starts_with("Unbekannter Command: /nichtda"),
            "{error}"
        );
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
            assert!(error.starts_with("Eingabe abgelehnt"), "{line}: {error}");
        }
        assert_eq!(built.load(Ordering::Relaxed), 0);
        assert_eq!(operation.dispatches.load(Ordering::Relaxed), 0);
        Ok(())
    }
}
