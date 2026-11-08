//! PL-100 WP 3.5 (Owner-Befund „assembled-registry coverage test for
//! `job.suspend`/`job.resume`“): eine tatsächlich montierte Registry mit
//! Job-Verdrahtung führt `job.suspend` und `job.resume`, und ihre
//! Freigabe fragt für beide immer nach (`ALWAYS_ASK_TOOLS`, Autonomie-Stufe
//! A2 „durch den Menschen, immer mit Nachfrage“).
//!
//! # Aufbau
//! - Jedes Registry-Profil, das die sieben `job.*`-Werkzeuge
//!   ([`JOB_TOOLS`]) registriert, wird über dieselbe Montage-Funktion wie
//!   Wurzel und Kinder montiert
//!   (`assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits`),
//!   mit einer [`JobWiring`] an der Host-Permit-Verdrahtung.
//! - Das `executor`-Kind (Profil `ShellExecution`) entsteht über
//!   [`RuntimeChildRegistryFactory`], denselben Fabriktyp, den die Montage
//!   für den Spawner registriert, mit der Wurzelkette
//!   ([`ApprovalChain::for_root`]) samt sitzungsweiter Allow-Regel `job.*`.
//!   `job.stop` läuft unter dieser Regel durch, `job.suspend` und
//!   `job.resume` fragen trotzdem. Weil erst die `SessionActivation` nach
//!   `[tools].admitted` filtert, prüft der Test zusätzlich, dass die
//!   eingebaute `executor`-Definition beide admittiert.
//!
//! Die Freigabe wird wie im Turn-Loop aggregiert
//! (`harw_core::turn_loop::check_approval`: `Deny` vor `AskUser` vor `Allow`
//! über alle Handler der Registry). Kein Modell, kein Netz, keine cgroup:
//! den Freezer selbst und seine Montage prüfen `harw-tool-job` und
//! `harw-runtime/src/assembly.rs` (`job_freezer_tests`). Die Job-Verwaltung
//! entsteht hier ohne den prozessweiten Befehls-Port
//! (`harw_runtime::job_wiring::SessionJobs::open_with_port` installiert ihn
//! global).

#[allow(
    dead_code,
    reason = "shared helper module; this test uses only part of it"
)]
mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use common::{TestError, TestResult, ctx};
use harw_core::{ChildRegistryFactory, EchoModelProvider};
use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
use harw_extension_api::{
    AllowRuleSet, ApprovalDecision, ApprovalRule, ExtensionRegistry, RuleDecision, RuleScope,
    SpawnInput, ToolCall, ToolName,
};
use harw_project_discovery::{DiscoveryConfig, ProjectContext, discover_project};
use harw_registry_defaults::embedded_agents::builtin_agent_definitions;
use harw_registry_defaults::profile::{
    JOB_TOOLS, RegistryProfile,
    assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits,
    role_names,
};
use harw_registry_defaults::{ALWAYS_ASK_TOOLS, HostPermitWiring, IdentityOverrides, JobWiring};
use harw_runtime::approval::ApprovalChain;
use harw_runtime::children::RuntimeChildRegistryFactory;
use harw_runtime::spec::AskResolution;
use harw_sandbox::{HostPermitSessionRegistry, ProcessPermitLedger, SandboxProfile};
use harw_tool_job::{JOB_RESUME_TOOL, JOB_STATUS_TOOL, JOB_STOP_TOOL, JOB_SUSPEND_TOOL};
use harw_tool_shell::HostPermitPromptSender;
use harw_types::{SessionId, ToolCallId};
use tempfile::TempDir;

/// Die beiden Anhalte-Werkzeuge (PL-100 WP 3.5).
const SUSPEND_TOOLS: [&str; 2] = [JOB_SUSPEND_TOOL, JOB_RESUME_TOOL];

/// Die Entscheidung des Turn-Loops über alle Handler einer Registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Jeder Handler gibt frei.
    Allow,
    /// Mindestens ein Handler fragt nach, keiner lehnt ab.
    Ask,
    /// Mindestens ein Handler lehnt ab.
    Deny,
}

/// Ein leeres Projekt (Marker `Cargo.toml`) samt erkanntem Kontext; das
/// Tempverzeichnis lebt so lange wie der Rückgabewert.
fn project() -> TestResult<(TempDir, ProjectContext)> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("project");
    std::fs::create_dir_all(&root).map_err(ctx("project"))?;
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").map_err(ctx("marker"))?;
    let project =
        discover_project(&root, &DiscoveryConfig::default()).map_err(ctx("discover project"))?;
    Ok((dir, project))
}

/// Host-Permit-Verdrahtung mit einer Job-Verwaltung unter `state`, wie die
/// TUI-Montage sie zu Wurzel und Kindern reicht (ohne Befehls-Port).
fn wiring_with_jobs(state: &Path, sender: HostPermitPromptSender) -> TestResult<HostPermitWiring> {
    let manager = harw_tool_job::JobManager::new(
        harw_tool_job::JobManagerConfig::new(state),
        Arc::new(harw_tool_job::NoopNotifier),
    )
    .map_err(ctx("job manager"))?;
    Ok(HostPermitWiring::new(
        Arc::new(ProcessPermitLedger::default()),
        Arc::new(HostPermitSessionRegistry::default()),
        sender,
    )
    .with_jobs(Some(JobWiring::new(
        manager,
        Arc::new(harw_tool_job::NoLineage),
    ))))
}

/// Ob `profile` alle sieben `job.*`-Werkzeuge registriert.
fn registers_job_tools(profile: RegistryProfile) -> bool {
    let names = profile.registered_tool_names();
    JOB_TOOLS.iter().all(|tool| names.contains(tool))
}

/// Die tatsächlich registrierten Werkzeugnamen von `registry`.
fn registered(registry: &ExtensionRegistry) -> Vec<String> {
    registry
        .tool_providers()
        .iter()
        .flat_map(|provider| provider.tools())
        .map(|spec| spec.name().to_owned())
        .collect()
}

/// Ob ein Provider von `registry` das Werkzeug `tool` ausführt.
fn has_executor(registry: &ExtensionRegistry, tool: &str) -> bool {
    let name = ToolName::new(tool);
    registry
        .tool_providers()
        .iter()
        .any(|provider| provider.executor(&name).is_some())
}

/// Befragt jeden Handler von `registry` genau einmal zu einem Aufruf von
/// `tool` und aggregiert wie der Turn-Loop: `Deny` vor `AskUser` vor
/// `Allow`.
async fn verdict(registry: &ExtensionRegistry, tool: &str) -> Verdict {
    let call = ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new(tool),
        arguments: serde_json::Value::Null,
    };
    let mut outcome = Verdict::Allow;
    for handler in registry.approval_handlers() {
        match handler.review(&call).await {
            ApprovalDecision::Deny(_) => return Verdict::Deny,
            ApprovalDecision::AskUser(_) => outcome = Verdict::Ask,
            ApprovalDecision::Allow => {}
        }
    }
    outcome
}

/// Ein Spawn-Auftrag ohne Modell-Kontext und ohne eigene Decke.
fn spawn_input() -> SpawnInput {
    SpawnInput {
        parent_session_id: SessionId::new(),
        handoff_call_id: ToolCallId::new(),
        instructions: None,
        context: serde_json::Value::Null,
        ceiling: None,
    }
}

/// Jedes Profil, das die sieben `job.*`-Werkzeuge registriert, führt mit
/// Job-Verdrahtung `job.suspend` und `job.resume`: registriert, mit
/// Executor und beworben. Die Freigabe der montierten Registry fragt für
/// beide nach, während das lesende `job.status` ohne Rückfrage durchläuft.
#[tokio::test]
async fn every_job_tools_profile_assembles_suspend_and_resume_behind_always_ask() -> TestResult {
    for tool in SUSPEND_TOOLS {
        assert!(ALWAYS_ASK_TOOLS.contains(&tool), "{tool} must always ask");
        assert!(JOB_TOOLS.contains(&tool), "{tool} is one of the job tools");
    }
    let (dir, project) = project()?;
    let (sender, _receiver) = harw_tool_shell::host_permit_prompt_channel();
    let wiring = wiring_with_jobs(&dir.path().join("state"), sender)?;
    let mut checked: Vec<RegistryProfile> = Vec::new();
    for profile in RegistryProfile::ALL
        .iter()
        .copied()
        .filter(|profile| registers_job_tools(*profile))
    {
        let assembled =
            assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits(
                profile,
                &project,
                IdentityOverrides::default(),
                ApprovalModeCell::new(ApprovalMode::Delegated),
                &profile.required_permissions(),
                None,
                &SandboxProfile::default(),
                Some(wiring.clone()),
            )
            .map_err(ctx("assemble a job tools profile with job wiring"))?;
        let names = registered(&assembled.registry);
        for tool in SUSPEND_TOOLS {
            assert!(
                names.iter().any(|name| name == tool),
                "{profile:?} registers {tool}: {names:?}"
            );
            assert!(
                has_executor(&assembled.registry, tool),
                "{profile:?} executes {tool}"
            );
            assert!(
                assembled
                    .identity
                    .tools_available
                    .iter()
                    .any(|name| name == tool),
                "{profile:?} advertises {tool}"
            );
            assert_eq!(
                verdict(&assembled.registry, tool).await,
                Verdict::Ask,
                "{profile:?}: {tool} always asks"
            );
        }
        assert_eq!(
            verdict(&assembled.registry, JOB_STATUS_TOOL).await,
            Verdict::Allow,
            "{profile:?}: the read-only job.status needs no approval"
        );
        checked.push(profile);
    }
    // Das Profil des `executor` (PL-100 WP 3.5 über `ShellExecution`).
    assert!(
        checked.contains(&RegistryProfile::ShellExecution),
        "{checked:?}"
    );
    Ok(())
}

/// Nutzerentscheidung vom 2026-09-24 (THREAT-MODEL B22 und §4): unter
/// `ApprovalMode::FullAccess` fragt die Freigabe nie nach, auch nicht für
/// `ALWAYS_ASK_TOOLS`. `job.suspend` und `job.resume` laufen dann ohne
/// Rückfrage. Der Test hält diesen Stand fest, damit eine Änderung an der
/// Stufe A2 „immer mit Nachfrage“ sichtbar wird; unter `Delegated` fragen
/// beide (oben).
#[tokio::test]
async fn full_access_lets_suspend_and_resume_through_without_a_question() -> TestResult {
    let (dir, project) = project()?;
    let (sender, _receiver) = harw_tool_shell::host_permit_prompt_channel();
    let wiring = wiring_with_jobs(&dir.path().join("state"), sender)?;
    let profile = RegistryProfile::ShellExecution;
    let assembled =
        assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits(
            profile,
            &project,
            IdentityOverrides::default(),
            ApprovalModeCell::new(ApprovalMode::FullAccess),
            &profile.required_permissions(),
            None,
            &SandboxProfile::default(),
            Some(wiring),
        )
        .map_err(ctx("assemble the executor profile under full access"))?;
    for tool in SUSPEND_TOOLS {
        assert!(
            has_executor(&assembled.registry, tool),
            "{profile:?} executes {tool}"
        );
        assert_eq!(
            verdict(&assembled.registry, tool).await,
            Verdict::Allow,
            "{tool}: full access asks nothing, ALWAYS_ASK_TOOLS included"
        );
    }
    Ok(())
}

/// Das `executor`-Kind, gebaut wie im echten Lauf über
/// [`RuntimeChildRegistryFactory`] mit Job-Verdrahtung und der Wurzelkette:
/// es führt `job.suspend` und `job.resume`, und beide fragen auch dann
/// nach, wenn eine sitzungsweite Allow-Regel `job.*` freigibt. `job.stop`
/// läuft unter derselben Regel ohne Rückfrage (die Regel wirkt also).
///
/// Die Kind-Registry filtert nicht nach `[tools].admitted`; das tut erst die
/// `SessionActivation` aus der Definition der Rolle. Aufrufen kann das Kind
/// die Werkzeuge also nur, wenn die eingebaute Definition des `executor`
/// (`agents/executor.toml`) sie admittiert; auch das prüft dieser Test.
#[tokio::test]
async fn the_executor_child_asks_for_suspend_and_resume_despite_an_allow_rule() -> TestResult {
    let definitions =
        builtin_agent_definitions(&HashMap::new()).map_err(ctx("builtin agent definitions"))?;
    let executor = definitions
        .get(role_names::EXECUTOR)
        .ok_or(TestError::Missing("executor definition"))?;
    let admitted = executor.tool_surface().admitted();
    for tool in SUSPEND_TOOLS {
        assert!(
            admitted.iter().any(|name| name == tool),
            "the executor definition admits {tool}: {admitted:?}"
        );
    }

    let (dir, project) = project()?;
    let (sender, _receiver) = harw_tool_shell::host_permit_prompt_channel();
    let wiring = wiring_with_jobs(&dir.path().join("state"), sender)?;
    let rules = AllowRuleSet::from_rules(vec![ApprovalRule {
        tool: "job.*".to_owned(),
        pattern: None,
        decision: RuleDecision::Allow,
        scope: RuleScope::Session,
    }]);
    let chain = ApprovalChain::for_root(
        &harw_config::ResolvedConfig::default(),
        AskResolution::Interactive,
        ApprovalModeCell::new(ApprovalMode::Delegated),
        None,
        rules,
    );
    let factory =
        RuntimeChildRegistryFactory::new(project, Arc::new(EchoModelProvider::new("echo")), chain)?
            .with_host_permits(SandboxProfile::default(), Some(wiring));
    let registry = factory
        .build_registry(role_names::EXECUTOR, &spawn_input(), None)
        .map_err(ctx("executor child registry"))?;
    let names = registered(&registry);
    for tool in SUSPEND_TOOLS {
        assert!(
            names.iter().any(|name| name == tool),
            "the executor registers {tool}: {names:?}"
        );
        assert!(
            has_executor(&registry, tool),
            "the executor executes {tool}"
        );
        assert_eq!(
            verdict(&registry, tool).await,
            Verdict::Ask,
            "{tool} asks despite the allow rule"
        );
    }
    assert_eq!(
        verdict(&registry, JOB_STOP_TOOL).await,
        Verdict::Allow,
        "the allow rule is live: job.stop passes without a question"
    );
    Ok(())
}
