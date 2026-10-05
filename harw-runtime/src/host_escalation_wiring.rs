//! Montage der Host-Mode-Anfrage aus dem Orchestrator-Baum (Runde 5, Teil N).
//!
//! # Beschreibung
//! `shell.exec` mit `request_host: {reason}` fragt die Nutzerin über denselben
//! Host-Permit-Dialog wie der Host-Modus (einmalig oder Host-Arbeitsphase),
//! zeigt dabei aber, **wer** fragt (Rolle, Kind-ID, Pfad im Baum). Dieses
//! Modul bündelt die Runtime-Seite davon:
//!
//! - [`escalation_for_entry`]: nur [`EntryKind::Tui`] bekommt eine
//!   [`HostEscalation`]; jeder andere Einstieg bleibt fail-closed
//!   (`harw_tool_shell::HOST_MODE_REQUIRES_TUI_MSG`).
//! - [`register_root`], [`note_spawn`], [`bind_child`], [`release`]: tragen
//!   Wurzel und Kinder in das gemeinsame
//!   [`harw_tool_shell::HostRequesterBook`] ein bzw. aus. Die Einhängepunkte
//!   liegen in `assembly.rs` (Wurzel) und `children.rs` (Kind-Fabrik), jeweils
//!   mit dem Kommentar „Runde 5, Teil N“.
//!
//! # Sitzungsphase und Kinder
//! Eine Host-Arbeitsphase — über `/sandbox-lease` oder über eine
//! `request_host`-Freigabe — wird prozessweit eingetragen
//! (`HostPermitSessionRegistry::mark_global_approval`) und bleibt ohne
//! Zeitablauf aktiv, bis der Nutzer sie beendet (Strg+H oder
//! `/sandbox-lease revoke`). Weil
//! `profile_tool_providers` Ledger, Registry und Fragekanal an **jeden**
//! gebauten `ShellToolProvider` hängt (auch Strict/Cargo/Tmux) und beide
//! Kind-Fabriken dieselbe `HostPermitWiring` bekommen, laufen alle
//! Shell-fähigen Kinder dieser harw-Sitzung während der Phase auf dem Host.
//! Die Tests unten prüfen das über die echte Kind-Fabrik.

use harw_registry_defaults::profile::HostPermitWiring;
use harw_tool_shell::HostEscalation;
use harw_types::SessionId;

use crate::spec::EntryKind;

/// Beschriftung der Wurzel im Baum-Pfad, wenn die UIA die Wurzel ist.
pub const UIA_ROOT_LABEL: &str = "uia";

/// Die Host-Mode-Verdrahtung für einen Einstieg: nur die interaktive TUI hat
/// einen Menschen, der eine Anfrage beantworten kann.
#[must_use]
pub fn escalation_for_entry(entry: EntryKind) -> Option<HostEscalation> {
    (entry == EntryKind::Tui).then(HostEscalation::new)
}

/// Das Buch der Anfragenden aus einer Verdrahtung, falls Host-Mode-Anfragen
/// überhaupt verdrahtet sind.
fn book(
    wiring: Option<&HostPermitWiring>,
) -> Option<&std::sync::Arc<harw_tool_shell::HostRequesterBook>> {
    wiring
        .and_then(|wiring| wiring.host_escalation.as_ref())
        .map(HostEscalation::requesters)
}

/// Trägt die Wurzelsitzung ein (`uia` mit aktiver UIA, sonst der benannte
/// Wurzel-Agent bzw. `Hauptsitzung`).
pub fn register_root(wiring: Option<&HostPermitWiring>, root: &SessionId, label: Option<&str>) {
    if let Some(book) = book(wiring) {
        book.register_root(
            root.as_str(),
            label.unwrap_or(harw_tool_shell::host_escalation::ROOT_REQUESTER_FALLBACK),
        );
    }
}

/// Merkt vor, dass für `parent` gerade ein Kind der Rolle `role` montiert wird.
pub fn note_spawn(wiring: Option<&HostPermitWiring>, parent: &SessionId, role: &str) {
    if let Some(book) = book(wiring) {
        book.note_spawn(parent.as_str(), role);
    }
}

/// Bindet die soeben erzeugte Kind-Sitzung an den vorgemerkten Eintrag.
pub fn bind_child(wiring: Option<&HostPermitWiring>, role: &str, child: &SessionId) {
    if let Some(book) = book(wiring) {
        book.bind_child(role, child.as_str());
    }
}

/// Entfernt ein freigegebenes Kind aus dem Buch.
pub fn release(wiring: Option<&HostPermitWiring>, child: &SessionId) {
    if let Some(book) = book(wiring) {
        book.release(child.as_str());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::children::RuntimeChildRegistryFactory;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_core::ChildRegistryFactory;
    use harw_extension_api::{
        SpawnInput, ToolCall, ToolExecutionContext, ToolExecutor, ToolName, ToolOutput,
    };
    use harw_project_discovery::ProjectContext;
    use harw_registry_defaults::profile::role_names;
    use harw_sandbox::{HostPermitSessionRegistry, ProcessPermitLedger, SandboxProfile};
    use harw_types::{TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::sync::Arc;

    fn project() -> ProjectContext {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        ProjectContext {
            cwd: root.clone(),
            project_root: root,
            docs: Vec::new(),
        }
    }

    fn factory(
        profile: SandboxProfile,
        wiring: HostPermitWiring,
    ) -> TestResult<RuntimeChildRegistryFactory> {
        let config = harw_config::ResolvedConfig::default();
        let chain = crate::approval::ApprovalChain::for_root(
            &config,
            crate::spec::AskResolution::Interactive,
            harw_extension_api::approval_mode::ApprovalModeCell::default(),
            None,
            harw_extension_api::allow_rules::AllowRuleSet::new(),
        );
        Ok(RuntimeChildRegistryFactory::new(
            project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            chain,
        )
        .map_err(ctx("factory builds"))?
        .with_host_permits(profile, Some(wiring)))
    }

    fn spawn_input(parent: &SessionId) -> SpawnInput {
        SpawnInput {
            parent_session_id: parent.clone(),
            handoff_call_id: ToolCallId::new(),
            instructions: None,
            context: serde_json::Value::Null,
            ceiling: None,
        }
    }

    fn shell_executor(
        factory: &RuntimeChildRegistryFactory,
        role: &str,
        parent: &SessionId,
    ) -> TestResult<Option<Arc<dyn ToolExecutor>>> {
        let registry = factory
            .build_registry(role, &spawn_input(parent), None)
            .map_err(|error| TestError::Unexpected(error.message))?;
        Ok(registry
            .tool_providers()
            .iter()
            .find_map(|provider| provider.executor(&ToolName::new("shell.exec"))))
    }

    fn child_context(
        dir: &tempfile::TempDir,
        child: SessionId,
    ) -> TestResult<ToolExecutionContext> {
        let ws = dir.path().join("project");
        std::fs::create_dir_all(&ws).map_err(ctx("project dir"))?;
        let registry = WorkspaceRegistry::build(
            dir.path(),
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("project"),
                root: ws,
            }],
        )
        .map_err(ctx("workspace registry"))?;
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("project"))
            .map_err(ctx("resolve"))?;
        Ok(ToolExecutionContext::new(
            child,
            TurnId::new(),
            SandboxSpec::from_resolved(
                binding,
                PermissionSet::from_policy([Permission::ReadWorkspace, Permission::ExecuteProcess]),
            ),
        ))
    }

    /// Ein Cargo-Profil aus Temp-Pfaden (echte Verzeichnisse und eine
    /// ausführbare Datei, wie `CargoSandboxProfile::new` es verlangt).
    fn cargo_profile(dir: &tempfile::TempDir) -> TestResult<SandboxProfile> {
        use std::os::unix::fs::PermissionsExt;
        let rustup = dir.path().join("rustup");
        let cargo_home = dir.path().join("cargo-home");
        let bin_dir = dir.path().join("bin");
        for path in [&rustup, &cargo_home, &bin_dir] {
            std::fs::create_dir_all(path).map_err(ctx("cargo profile dir"))?;
        }
        let cargo_bin = bin_dir.join("cargo");
        std::fs::write(&cargo_bin, "#!/bin/sh\n").map_err(ctx("cargo bin"))?;
        std::fs::set_permissions(&cargo_bin, std::fs::Permissions::from_mode(0o755))
            .map_err(ctx("chmod cargo bin"))?;
        let profile = harw_sandbox::CargoSandboxProfile::new(
            harw_sandbox::CargoExecutionMode::BuildOffline,
            &cargo_bin,
            &rustup,
            &cargo_home,
        )
        .map_err(ctx("cargo profile"))?;
        Ok(SandboxProfile::Cargo(profile))
    }

    #[test]
    fn test_only_the_tui_entry_gets_host_escalation() {
        assert!(escalation_for_entry(EntryKind::Tui).is_some());
        for entry in [
            EntryKind::OneShot,
            EntryKind::Analyze,
            EntryKind::Web,
            EntryKind::JobPrompt,
            EntryKind::JobPlanNode,
            EntryKind::GatewayTelegram,
        ] {
            assert!(
                escalation_for_entry(entry).is_none(),
                "{entry:?} has no human to answer a host request"
            );
        }
    }

    /// Nutzerinnen-Fall: eine aktive `/sandbox-lease` (prozessweit, bis Strg+H)
    /// muss auch für Kinder mit Strict- **und** Cargo-Profil gelten — der
    /// Aufruf läuft ohne erneute Frage auf dem Host.
    #[tokio::test]
    async fn test_active_sandbox_lease_applies_to_strict_and_cargo_children() -> TestResult {
        harw_command::install_host_default(&std::env::temp_dir().join("harw-runtime-command-jobs"));
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let profiles = [SandboxProfile::Strict, cargo_profile(&tmp)?];
        for profile in profiles {
            let registry = Arc::new(HostPermitSessionRegistry::default());
            let (sender, mut receiver) = harw_tool_shell::host_permit_prompt_channel();
            let wiring = HostPermitWiring::new(
                Arc::new(ProcessPermitLedger::default()),
                Arc::clone(&registry),
                sender,
            )
            .with_host_escalation(escalation_for_entry(EntryKind::Tui));
            // Genau das, was `/sandbox-lease` bei „Host-Arbeitsphase“ tut.
            registry.mark_global_approval();

            let factory = factory(profile.clone(), wiring)?;
            let root = SessionId::new();
            // `executor` ist die Shell-fähige Rolle im Orchestrator-Baum
            // (`cargo-worker`/`sandbox-shell-worker` sind noch keine
            // spawnbaren Rollen); das Cargo-Profil kommt aus der Montage.
            for role in [role_names::EXECUTOR] {
                let executor = shell_executor(&factory, role, &root)?
                    .ok_or(TestError::Missing("shell.exec for a shell-capable child"))?;
                let context = child_context(&tmp, SessionId::new())?;
                let output = executor
                    .execute(
                        &context,
                        &ToolCall {
                            id: ToolCallId::new(),
                            name: ToolName::new("shell.exec"),
                            arguments: serde_json::json!({ "command": "echo lease_child" }),
                        },
                    )
                    .await
                    .map_err(ctx("execute"))?;
                match output {
                    ToolOutput::Json { content } => assert_eq!(
                        content["executed_on"], "host",
                        "{role} with {profile:?} must run on the host during a lease: {content}"
                    ),
                    other => {
                        return Err(TestError::Unexpected(format!(
                            "{role} with {profile:?}: expected host JSON, got {other:?}"
                        )));
                    }
                }
            }
            assert!(
                receiver.try_recv().is_err(),
                "an active lease must never prompt again"
            );
        }
        Ok(())
    }

    /// Read-only-Rollen bekommen weiterhin nie `shell.exec` — sie können also
    /// auch keinen Host-Mode anfragen, selbst mit TUI-Verdrahtung.
    #[test]
    fn test_read_only_roles_cannot_request_host_mode() -> TestResult {
        let (sender, _receiver) = harw_tool_shell::host_permit_prompt_channel();
        let wiring = HostPermitWiring::new(
            Arc::new(ProcessPermitLedger::default()),
            Arc::new(HostPermitSessionRegistry::default()),
            sender,
        )
        .with_host_escalation(escalation_for_entry(EntryKind::Tui));
        let factory = factory(SandboxProfile::Strict, wiring)?;
        let root = SessionId::new();
        for role in [
            role_names::EXPLORER,
            role_names::PLANNER,
            role_names::ANALYST,
            role_names::RESEARCHER_WEB,
        ] {
            assert!(
                shell_executor(&factory, role, &root)?.is_none(),
                "{role} is read-only and must never get shell.exec"
            );
        }
        Ok(())
    }

    /// Der Baum-Pfad entsteht über die echten Fabrik-Haken
    /// (`build_registry` → `child_session_observers` → `child_session_released`).
    #[test]
    fn test_factory_hooks_record_role_and_tree_path() -> TestResult {
        let (sender, _receiver) = harw_tool_shell::host_permit_prompt_channel();
        let escalation = escalation_for_entry(EntryKind::Tui);
        let book = escalation
            .as_ref()
            .map(|escalation| Arc::clone(escalation.requesters()))
            .ok_or(TestError::Missing("tui escalation"))?;
        let wiring = HostPermitWiring::new(
            Arc::new(ProcessPermitLedger::default()),
            Arc::new(HostPermitSessionRegistry::default()),
            sender,
        )
        .with_host_escalation(escalation);
        let root = SessionId::new();
        register_root(Some(&wiring), &root, Some(UIA_ROOT_LABEL));
        let factory = factory(SandboxProfile::Strict, wiring)?;

        let orchestrator = SessionId::new();
        factory
            .build_registry(role_names::ROOT_ORCHESTRATOR, &spawn_input(&root), None)
            .map_err(|error| TestError::Unexpected(error.message))?;
        let _ = factory.child_session_observers(role_names::ROOT_ORCHESTRATOR, &orchestrator);
        let executor_child = SessionId::new();
        factory
            .build_registry(role_names::EXECUTOR, &spawn_input(&orchestrator), None)
            .map_err(|error| TestError::Unexpected(error.message))?;
        let _ = factory.child_session_observers(role_names::EXECUTOR, &executor_child);

        let requester = book.requester_for(executor_child.as_str());
        assert_eq!(requester.role, role_names::EXECUTOR);
        assert_eq!(
            requester.path,
            format!(
                "uia › {} › {}",
                role_names::ROOT_ORCHESTRATOR,
                role_names::EXECUTOR
            )
        );

        factory.child_session_released(role_names::EXECUTOR, &executor_child);
        assert_eq!(
            book.requester_for(executor_child.as_str()).role,
            "unbekannt"
        );
        Ok(())
    }
}
