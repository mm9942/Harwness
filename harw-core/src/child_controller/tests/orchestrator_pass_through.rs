//! Orchestrator-Durchreichung: Ein lesender Orchestrator gibt seinen Workern
//! nicht seinen eigenen (lesenden) Werkzeugschnitt mit, sondern die
//! Aktivierung seiner Linie (UIA-Wurzel bzw. Delegationsaktivierung). Die
//! Sandbox-Obergrenze bleibt unberührt, der Plan-Modus bleibt die harte
//! Bremse, und der Orchestrator selbst bleibt lesend.

use harw_agent_dsl::roles::AgentRoleId;
use harw_extension_api::DelegationTargetInfo;

use super::*;
use crate::mode::InteractionMode;

const PLAN_REFUSAL: &str = "Plan-Modus: nur lesende Ziele delegierbar (explorer); \
Schreib-/Ausführungsziele erst nach Planfreigabe.";

const RWX: [Permission; 3] = [
    Permission::ReadWorkspace,
    Permission::WriteWorkspace,
    Permission::ExecuteProcess,
];

fn info(name: &str, read_only: bool) -> DelegationTargetInfo {
    DelegationTargetInfo {
        name: name.to_owned(),
        role: "worker".to_owned(),
        description: Some(format!("Testziel {name}")),
        read_only,
        ..DelegationTargetInfo::default()
    }
}

fn ir(sections: &str) -> TestResult<ExecutableAgentIr> {
    test_agent_ir(sections)
}

fn tools_ir(admitted: &str, forbidden: &str) -> String {
    format!("[tools]\nadmitted = [{admitted}]\nforbidden = [{forbidden}]")
}

fn role(name: &str) -> AgentRole {
    AgentRole::Agent {
        name: name.to_owned(),
    }
}

fn ir_registry(sections: &str) -> TestResult<Arc<IrChildRegistry>> {
    Ok(Arc::new(IrChildRegistry { ir: ir(sections)? }))
}

fn new_spawner() -> ManagedAgentSpawner {
    let (events, _receiver) = mpsc::unbounded_channel();
    let manager = Arc::new(Mutex::new(SessionManager::new(events)));
    ManagedAgentSpawner::new(manager, ChildLimits::conservative())
}

/// UIA-Wurzel (extern, Vollaktivierung) mit lesendem `root-orchestrator`,
/// `middle`, Schreib-/Ausführungs-Workern und einem Steward.
fn pass_through_spawner(
    uia_permissions: &[Permission],
    catalog: bool,
) -> TestResult<(Arc<ManagedAgentSpawner>, SessionId, SandboxSpec)> {
    let sandbox = test_sandbox(PermissionSet::from_policy(uia_permissions.iter().copied()))?;
    let forbidden = "\"fs.write\", \"fs.edit\", \"shell.exec\", \"job.start\"";
    let orchestrator = format!(
        "[spawn]\nchild_orchestrators = [\"middle\"]\n\n{}",
        tools_ir("\"fs.read\"", forbidden)
    );
    let mut spawner = new_spawner()
        .with_role(
            "root-orchestrator",
            role("root-orchestrator"),
            AgentRoleId::RootOrchestrator,
            ir_registry(&orchestrator)?,
        )
        .with_role(
            "middle",
            role("middle"),
            AgentRoleId::ChildOrchestrator,
            ir_registry(&tools_ir("\"fs.read\"", forbidden))?,
        )
        .with_role(
            "rust-implementer",
            role("rust-implementer"),
            AgentRoleId::Worker,
            ir_registry(&tools_ir("\"fs.read\", \"fs.write\", \"fs.edit\"", ""))?,
        )
        .with_role(
            "executor",
            role("executor"),
            AgentRoleId::Worker,
            ir_registry(&tools_ir("\"shell.exec\", \"job.start\"", ""))?,
        )
        .with_role(
            "explorer",
            role("explorer"),
            AgentRoleId::Worker,
            Arc::new(EmptyChildRegistry),
        )
        .with_role(
            "steward",
            role("steward"),
            AgentRoleId::AgentSteward,
            ir_registry(&tools_ir("\"fs.read\", \"fs.write\"", ""))?,
        );
    if catalog {
        spawner = spawner.with_delegation_catalog([
            info("root-orchestrator", true),
            info("explorer", true),
            info("rust-implementer", false),
            info("executor", false),
        ]);
    }
    let uia = SessionId::new();
    let spawner = spawner
        .with_external_root_parent(
            uia.clone(),
            external_root_context(sandbox.clone(), AgentRoleId::UserInterface),
            None,
            SessionActivation::default(),
        )
        .map_err(ctx("die UIA-Wurzel registriert sich"))?;
    Ok((Arc::new(spawner), uia, sandbox))
}

/// Separater Spawner mit externer Wurzel `root_role` (nur `fs.read` aktiv,
/// Sandbox RWX) und einem Worker `name`, der `fs.read`/`fs.write` zulässt.
fn external_spawner(
    root_role: AgentRoleId,
    name: &str,
    spawnable: &[&str],
    delegation: Option<SessionActivation>,
) -> TestResult<(Arc<ManagedAgentSpawner>, SessionId, SandboxSpec)> {
    let sandbox = test_sandbox(PermissionSet::from_policy(RWX))?;
    let mut activation = SessionActivation::new(ToolProfile::Minimal);
    activation.enable_tool(ToolName::new("fs.read"));
    let root = SessionId::new();
    let mut spawner = new_spawner()
        .with_role(
            name,
            role(name),
            AgentRoleId::Worker,
            ir_registry(&tools_ir("\"fs.read\", \"fs.write\"", ""))?,
        )
        .with_uia_spawnable_roles(spawnable.iter().map(|entry| (*entry).to_owned()))
        .with_external_root_parent(
            root.clone(),
            external_root_context(sandbox.clone(), root_role),
            None,
            activation,
        )
        .map_err(ctx("die externe Wurzel registriert sich"))?;
    if let Some(delegation) = delegation {
        spawner = spawner
            .with_external_root_delegation_activation(delegation)
            .map_err(ctx("die Delegationsaktivierung wird registriert"))?;
    }
    spawner.live_mode().publish(InteractionMode::Work);
    Ok((Arc::new(spawner), root, sandbox))
}

/// Die aktuelle Sandbox einer Manager-Sitzung.
fn current_sandbox(spawner: &ManagedAgentSpawner, session: &SessionId) -> TestResult<SandboxSpec> {
    let manager = spawner
        .manager
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    manager
        .get(session)
        .map_err(ctx("Sitzung liegt im Manager"))?
        .spawn_context()
        .map(|context| context.sandbox.clone())
        .ok_or(TestError::Missing("Spawn-Kontext"))
}

/// Ob `tool` in Aktivierung und Basis-Aktivierung der Sitzung aktiv ist.
fn enabled(
    spawner: &ManagedAgentSpawner,
    session: &SessionId,
    tool: &str,
) -> TestResult<(bool, bool)> {
    let manager = spawner
        .manager
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let session = manager
        .get(session)
        .map_err(ctx("Sitzung liegt im Manager"))?;
    let name = ToolName::new(tool);
    Ok((
        session.activation().is_tool_enabled(&name),
        session.base_activation().is_tool_enabled(&name),
    ))
}

fn permissions(spawner: &ManagedAgentSpawner, session: &SessionId) -> TestResult<PermissionSet> {
    Ok(current_sandbox(spawner, session)?.permissions().clone())
}

/// Admittiert den Root-Orchestrator unter der UIA im Arbeitsmodus.
fn admit_root(
    spawner: &ManagedAgentSpawner,
    uia: &SessionId,
    sandbox: SandboxSpec,
) -> TestResult<SessionId> {
    spawner
        .admit("root-orchestrator", spawn_input(uia.clone()), sandbox, None)
        .map_err(ctx("Root-Orchestrator wird admittiert"))
}

fn admit_child(
    spawner: &ManagedAgentSpawner,
    role: &str,
    parent: &SessionId,
) -> TestResult<SessionId> {
    let sandbox = current_sandbox(spawner, parent)?;
    spawner
        .admit(role, spawn_input(parent.clone()), sandbox, None)
        .map_err(ctx("Kind wird admittiert"))
}

fn work_setup() -> TestResult<(Arc<ManagedAgentSpawner>, SessionId, SessionId)> {
    let (spawner, uia, sandbox) = pass_through_spawner(&RWX, false)?;
    spawner.live_mode().publish(InteractionMode::Work);
    let root = admit_root(&spawner, &uia, sandbox)?;
    Ok((spawner, uia, root))
}

/// Ein Worker unter dem lesenden Orchestrator bekommt die Schreibwerkzeuge
/// der UIA-Linie, nicht den lesenden Schnitt des Orchestrators.
#[test]
fn a_worker_under_a_read_only_orchestrator_gets_write_tools() -> TestResult {
    let (spawner, _uia, root) = work_setup()?;
    assert_eq!(enabled(&spawner, &root, "fs.write")?, (false, false));
    let worker = admit_child(&spawner, "rust-implementer", &root)?;
    assert_eq!(enabled(&spawner, &worker, "fs.write")?, (true, true));
    assert_eq!(enabled(&spawner, &worker, "fs.edit")?, (true, true));
    assert!(permissions(&spawner, &worker)?.contains(Permission::WriteWorkspace));
    Ok(())
}

/// Ein Executor unter dem lesenden Orchestrator bekommt die
/// Ausführungswerkzeuge samt Prozess-Recht.
#[test]
fn an_executor_under_a_read_only_orchestrator_gets_exec() -> TestResult {
    let (spawner, _uia, root) = work_setup()?;
    let executor = admit_child(&spawner, "executor", &root)?;
    assert_eq!(enabled(&spawner, &executor, "shell.exec")?, (true, true));
    assert_eq!(enabled(&spawner, &executor, "job.start")?, (true, true));
    assert!(permissions(&spawner, &executor)?.contains(Permission::ExecuteProcess));
    Ok(())
}

/// Im Plan-Modus mit Katalog bleibt der Executor zurückgehalten und wird mit
/// der benannten Plan-Modus-Meldung abgelehnt.
#[test]
fn plan_mode_with_a_catalog_still_refuses_the_executor() -> TestResult {
    let (spawner, uia, sandbox) = pass_through_spawner(&RWX, true)?;
    spawner.live_mode().publish(InteractionMode::Plan);
    let root = admit_root(&spawner, &uia, sandbox)?;
    let targets = AgentSpawner::delegation_targets(spawner.as_ref(), &root, false)
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
    assert!(
        targets
            .withheld_by_plan_mode
            .iter()
            .any(|target| target.name == "executor"),
        "der Executor ist im Plan-Modus zurückgehalten"
    );
    let root_sandbox = current_sandbox(&spawner, &root)?;
    let refused = spawner
        .admit("executor", spawn_input(root), root_sandbox, None)
        .err()
        .ok_or(TestError::Missing(
            "executor muss im Plan-Modus abgelehnt werden",
        ))?;
    assert_eq!(refused.message, PLAN_REFUSAL);
    Ok(())
}

/// Ein im Plan-Modus gespawnter Enkel behält den Orchestrator-Schnitt; auch
/// nach dem Wechsel in den Arbeitsmodus bleibt die harte Bremse erhalten.
#[test]
fn a_grandchild_spawned_in_plan_keeps_the_orchestrator_cut() -> TestResult {
    let (spawner, uia, sandbox) = pass_through_spawner(&RWX, false)?;
    spawner.live_mode().publish(InteractionMode::Plan);
    let root = admit_root(&spawner, &uia, sandbox)?;
    let worker = admit_child(&spawner, "rust-implementer", &root)?;
    assert!(!enabled(&spawner, &worker, "fs.write")?.1);

    spawner.live_mode().publish(InteractionMode::Work);
    {
        let mut manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = manager
            .get_mut(&worker)
            .map_err(ctx("Enkel liegt im Manager"))?;
        assert_eq!(
            session.sync_live_mode(),
            Some((InteractionMode::Plan, InteractionMode::Work))
        );
    }
    assert_eq!(enabled(&spawner, &worker, "fs.write")?, (false, false));
    Ok(())
}

/// Ohne `WriteWorkspace` an der UIA bekommt kein Enkel es, und eine
/// Eskalation der Sandbox wird weiter abgelehnt.
#[test]
fn without_write_workspace_at_the_uia_no_grandchild_gets_it() -> TestResult {
    let (spawner, uia, sandbox) = pass_through_spawner(
        &[Permission::ReadWorkspace, Permission::ExecuteProcess],
        false,
    )?;
    spawner.live_mode().publish(InteractionMode::Work);
    let root = admit_root(&spawner, &uia, sandbox)?;
    let worker = admit_child(&spawner, "rust-implementer", &root)?;
    assert!(!permissions(&spawner, &worker)?.contains(Permission::WriteWorkspace));
    let escalated = test_sandbox(PermissionSet::from_policy(RWX))?;
    let refused = spawner
        .admit("rust-implementer", spawn_input(root), escalated, None)
        .err()
        .ok_or(TestError::Missing("Sandbox-Eskalation muss scheitern"))?;
    assert!(
        refused
            .message
            .starts_with("child sandbox escalation rejected"),
        "unerwartete Meldung: {}",
        refused.message
    );
    Ok(())
}

/// Der Orchestrator selbst bleibt lesend, auch nachdem seine Worker
/// schreibende und ausführende Werkzeuge bekamen.
#[test]
fn the_orchestrator_itself_stays_read_only() -> TestResult {
    let (spawner, _uia, root) = work_setup()?;
    admit_child(&spawner, "rust-implementer", &root)?;
    admit_child(&spawner, "executor", &root)?;
    for tool in ["fs.write", "fs.edit", "shell.exec", "job.start"] {
        assert_eq!(enabled(&spawner, &root, tool)?, (false, false), "{tool}");
    }
    Ok(())
}

/// Die Durchreichung läuft über einen Kind-Orchestrator hinweg bis zur
/// UIA-Linie; der Kind-Orchestrator selbst bleibt lesend.
#[test]
fn the_pass_through_is_transitive_over_a_child_orchestrator() -> TestResult {
    let (spawner, _uia, root) = work_setup()?;
    let middle = admit_child(&spawner, "middle", &root)?;
    let worker = admit_child(&spawner, "rust-implementer", &middle)?;
    assert_eq!(enabled(&spawner, &worker, "fs.write")?, (true, true));
    assert_eq!(enabled(&spawner, &middle, "fs.write")?, (false, false));
    Ok(())
}

/// Ein Agent-Steward wird weiter gegen den Orchestrator geschnitten.
#[test]
fn an_agent_steward_is_still_cut_against_the_orchestrator() -> TestResult {
    let (spawner, _uia, root) = work_setup()?;
    let steward = admit_child(&spawner, "steward", &root)?;
    assert_eq!(enabled(&spawner, &steward, "fs.write")?, (false, false));
    assert!(enabled(&spawner, &steward, "fs.read")?.0);
    Ok(())
}

/// Ein Nicht-Orchestrator-Elternteil schneidet wie bisher.
#[test]
fn a_non_orchestrator_parent_still_cuts() -> TestResult {
    let (spawner, uia, sandbox) =
        external_spawner(AgentRoleId::UserInterface, "reader", &["reader"], None)?;
    let reader = spawner
        .admit("reader", spawn_input(uia), sandbox, None)
        .map_err(ctx("Reader wird admittiert"))?;
    assert_eq!(enabled(&spawner, &reader, "fs.write")?, (false, false));
    assert!(enabled(&spawner, &reader, "fs.read")?.0);
    Ok(())
}

/// Eine externe Orchestrator-Wurzel reicht ihre Delegationsaktivierung an die
/// Worker weiter; ohne sie gilt der Schnitt der Wurzel. Ohne externe Wurzel
/// ist die Registrierung ein Fehler.
#[test]
fn an_external_orchestrator_root_passes_on_its_delegation_activation() -> TestResult {
    let (spawner, root, sandbox) = external_spawner(
        AgentRoleId::RootOrchestrator,
        "rust-implementer",
        &[],
        Some(SessionActivation::default()),
    )?;
    let worker = spawner
        .admit("rust-implementer", spawn_input(root), sandbox, None)
        .map_err(ctx("Worker mit Delegationsaktivierung"))?;
    assert!(enabled(&spawner, &worker, "fs.write")?.0);

    let (spawner, root, sandbox) =
        external_spawner(AgentRoleId::RootOrchestrator, "rust-implementer", &[], None)?;
    let worker = spawner
        .admit("rust-implementer", spawn_input(root), sandbox, None)
        .map_err(ctx("Worker ohne Delegationsaktivierung"))?;
    assert!(!enabled(&spawner, &worker, "fs.write")?.0);

    let error = new_spawner()
        .with_external_root_delegation_activation(SessionActivation::default())
        .err()
        .ok_or(TestError::Missing("ohne externe Wurzel muss es scheitern"))?;
    assert_eq!(error.message, "no external root parent is registered");
    Ok(())
}
