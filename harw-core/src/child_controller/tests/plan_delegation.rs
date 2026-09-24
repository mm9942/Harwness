//! Plan R9, Teil C/E1: Delegationsziele im Plan-Modus.
//!
//! Beleg aus dem Live-Lauf: die Sitzung stand im Plan-Modus; die Übergaben
//! fielen dort ganz weg, `delegate_wave` fand kein Ziel („no delegation
//! capability“) — der Root-Orchestrator war blind. Jetzt bleiben lesende
//! Ziele delegierbar, schreibende/ausführende werden mit einer benannten
//! Meldung abgelehnt, und jede Ablehnung ohne Ziel nennt ihren Grund.

use harw_agent_dsl::roles::AgentRoleId;
use harw_extension_api::{DelegationTargetInfo, DelegationUnavailable};

use super::*;
use crate::mode::InteractionMode;

fn info(name: &str, read_only: bool) -> DelegationTargetInfo {
    DelegationTargetInfo {
        name: name.to_owned(),
        role: "worker".to_owned(),
        description: Some(format!("Testziel {name}")),
        read_only,
        ..DelegationTargetInfo::default()
    }
}

/// UIA-Wurzel (extern) mit `root-orchestrator`, lesendem `explorer` und
/// ausführendem `executor` samt Katalog.
fn plan_spawner(
    limits: ChildLimits,
    catalog: bool,
) -> TestResult<(Arc<ManagedAgentSpawner>, SessionId, SandboxSpec)> {
    let (events, _receiver) = mpsc::unbounded_channel();
    let manager = Arc::new(Mutex::new(SessionManager::new(events)));
    let sandbox = test_sandbox(PermissionSet::from_policy([
        Permission::ReadWorkspace,
        Permission::WriteWorkspace,
        Permission::ExecuteProcess,
    ]))?;
    let uia = SessionId::new();
    let role = |name: &str| AgentRole::Agent {
        name: name.to_owned(),
    };
    let mut spawner = ManagedAgentSpawner::new(manager, limits)
        .with_role(
            "root-orchestrator",
            role("root-orchestrator"),
            AgentRoleId::RootOrchestrator,
            Arc::new(EmptyChildRegistry),
        )
        .with_role(
            "explorer",
            role("explorer"),
            AgentRoleId::Worker,
            Arc::new(EmptyChildRegistry),
        )
        .with_role(
            "executor",
            role("executor"),
            AgentRoleId::Worker,
            Arc::new(EmptyChildRegistry),
        );
    if catalog {
        spawner = spawner.with_delegation_catalog([
            info("root-orchestrator", true),
            info("explorer", true),
            info("executor", false),
        ]);
    }
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

fn names(targets: &[DelegationTargetInfo]) -> Vec<&str> {
    targets.iter().map(|target| target.name.as_str()).collect()
}

/// Die aktuelle Sandbox einer Manager-Sitzung (wie `governed_spawn_context`).
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

/// Die Wurzel meldet den Plan-Modus (`note_caller_mode`); ihr Kind erbt ihn:
/// lesende Ziele bleiben sichtbar und admittierbar, `executor` ist
/// zurückgehalten und wird mit der Plan-Modus-Meldung abgelehnt. Nach dem
/// Wechsel zurück sieht der Orchestrator wieder alle Ziele.
#[test]
fn plan_mode_is_inherited_and_keeps_only_read_only_targets() -> TestResult {
    let (spawner, uia, sandbox) = plan_spawner(ChildLimits::conservative(), true)?;
    AgentSpawner::note_caller_mode(spawner.as_ref(), &uia, true);

    // Die UIA selbst: der Root-Orchestrator ist lesend und bleibt sichtbar.
    let uia_targets = AgentSpawner::delegation_targets(spawner.as_ref(), &uia, false)
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
    assert!(uia_targets.plan_mode);
    assert_eq!(names(&uia_targets.targets), vec!["root-orchestrator"]);

    let root = spawner
        .admit("root-orchestrator", spawn_input(uia.clone()), sandbox, None)
        .map_err(ctx("Root-Orchestrator im Plan-Modus"))?;
    let targets = AgentSpawner::delegation_targets(spawner.as_ref(), &root, false)
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
    assert!(targets.plan_mode, "der Plan-Modus der Wurzel wird geerbt");
    assert_eq!(names(&targets.targets), vec!["explorer"]);
    assert_eq!(names(&targets.withheld_by_plan_mode), vec!["executor"]);
    assert_eq!(
        AgentSpawner::delegation_target_names(spawner.as_ref(), &root),
        vec!["explorer".to_owned()]
    );

    let root_sandbox = current_sandbox(&spawner, &root)?;
    spawner
        .admit(
            "explorer",
            spawn_input(root.clone()),
            root_sandbox.clone(),
            None,
        )
        .map_err(ctx("ein lesendes Ziel ist im Plan-Modus admittierbar"))?;
    let refused = spawner
        .admit("executor", spawn_input(root.clone()), root_sandbox, None)
        .err()
        .ok_or(TestError::Missing(
            "executor muss im Plan-Modus abgelehnt werden",
        ))?;
    assert_eq!(
        refused.message,
        "Plan-Modus: nur lesende Ziele delegierbar (explorer); Schreib-/Ausführungsziele erst \
         nach Planfreigabe."
    );
    assert!(crate::delegation_visibility::is_plan_mode_refusal(
        &refused.message
    ));

    // Außerhalb des Plan-Modus: alle sichtbaren Ziele.
    AgentSpawner::note_caller_mode(spawner.as_ref(), &uia, false);
    let targets = AgentSpawner::delegation_targets(spawner.as_ref(), &root, false)
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
    assert!(!targets.plan_mode);
    assert_eq!(names(&targets.targets), vec!["executor", "explorer"]);
    assert!(targets.withheld_by_plan_mode.is_empty());
    Ok(())
}

/// Der veröffentlichte Live-Modus (Runde 9, E6) ist die Antwort für den
/// ganzen Baum — auch wenn eine Sitzung selbst (noch) einen anderen Modus
/// trägt; ein angefragter Plan-Modus (die Wurzel kennt ihren eigenen) gilt
/// zusätzlich.
#[test]
fn the_published_live_mode_decides_and_a_requested_plan_mode_narrows() -> TestResult {
    let (spawner, uia, sandbox) = plan_spawner(ChildLimits::conservative(), true)?;
    let root = spawner
        .admit("root-orchestrator", spawn_input(uia.clone()), sandbox, None)
        .map_err(ctx("Root-Orchestrator"))?;
    spawner.live_mode().publish(InteractionMode::Plan);
    let targets = AgentSpawner::delegation_targets(spawner.as_ref(), &root, false)
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
    assert_eq!(names(&targets.targets), vec!["explorer"]);

    spawner.live_mode().publish(InteractionMode::Work);
    let targets = AgentSpawner::delegation_targets(spawner.as_ref(), &root, false)
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
    assert_eq!(names(&targets.targets), vec!["executor", "explorer"]);
    let requested = AgentSpawner::delegation_targets(spawner.as_ref(), &root, true)
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
    assert_eq!(names(&requested.targets), vec!["explorer"]);
    Ok(())
}

/// Ohne Katalog kennt der Spawner keine Lese-Eigenschaft: im Plan-Modus ist
/// dann kein Ziel sichtbar (Verhalten vor Plan R9), die Admission prüft den
/// Plan-Modus nicht zusätzlich.
#[test]
fn without_a_catalog_plan_mode_shows_no_target() -> TestResult {
    let (spawner, uia, sandbox) = plan_spawner(ChildLimits::conservative(), false)?;
    AgentSpawner::note_caller_mode(spawner.as_ref(), &uia, true);
    let root = spawner
        .admit("root-orchestrator", spawn_input(uia), sandbox, None)
        .map_err(ctx("Root-Orchestrator"))?;
    let targets = AgentSpawner::delegation_targets(spawner.as_ref(), &root, false)
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
    assert!(targets.targets.is_empty());
    assert_eq!(
        names(&targets.withheld_by_plan_mode),
        vec!["executor", "explorer"]
    );
    Ok(())
}

/// Plan R9, Teil C: ein Aufrufer ohne Ziele bekommt einen benannten Grund —
/// „Kein Spawn-Kontext (interner Fehler)“ für eine unbekannte Sitzung
/// (früher verschluckt `unwrap_or_default`), „Restliche Spawn-Tiefe 0“ an
/// der Tiefengrenze.
#[test]
fn a_caller_without_targets_gets_a_named_reason() -> TestResult {
    let (spawner, uia, sandbox) = plan_spawner(
        ChildLimits {
            max_depth: 1,
            ..ChildLimits::conservative()
        },
        true,
    )?;
    let unknown = AgentSpawner::delegation_targets(spawner.as_ref(), &SessionId::new(), false);
    let Err(DelegationUnavailable::NoSpawnContext { detail }) = &unknown else {
        return Err(TestError::Unexpected(format!("{unknown:?}")));
    };
    assert!(detail.contains("unknown delegation caller"), "{detail}");
    let rendered = unknown
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(
        rendered.starts_with("Kein Spawn-Kontext (interner Fehler)"),
        "{rendered}"
    );
    assert!(AgentSpawner::delegation_target_names(spawner.as_ref(), &SessionId::new()).is_empty());

    let root = spawner
        .admit("root-orchestrator", spawn_input(uia), sandbox, None)
        .map_err(ctx("Root-Orchestrator auf Tiefe 1"))?;
    let exhausted = AgentSpawner::delegation_targets(spawner.as_ref(), &root, false);
    assert_eq!(exhausted, Err(DelegationUnavailable::DepthExhausted));
    assert!(
        DelegationUnavailable::DepthExhausted
            .to_string()
            .starts_with("Restliche Spawn-Tiefe 0")
    );
    Ok(())
}

/// Runde 9 E6 / Plan R9 E1: ein Enkel, der im Plan-Modus startet, wird gegen
/// die **Basis** seines Elternteils geschnitten. Nach dem Wechsel zu `work`
/// hat er seine Schreibwerkzeuge und sein Schreibrecht wieder — vorher trug
/// er die Plan-Decke des Elternteils für immer.
#[test]
fn a_grandchild_spawned_in_plan_regains_write_tools_after_work() -> TestResult {
    // Ohne Katalog: die Admission prüft den Plan-Modus nicht, damit der
    // schreibende Enkel überhaupt im Plan-Modus entsteht (etwa unter einem
    // lesenden Orchestrator, der ihn erst nach der Freigabe beauftragt).
    let (spawner, uia, sandbox) = plan_spawner(ChildLimits::conservative(), false)?;
    spawner.live_mode().publish(InteractionMode::Plan);
    let root = spawner
        .admit("root-orchestrator", spawn_input(uia), sandbox, None)
        .map_err(ctx("Root-Orchestrator im Plan-Modus"))?;
    let root_sandbox = current_sandbox(&spawner, &root)?;
    assert!(
        !root_sandbox
            .permissions()
            .contains(Permission::WriteWorkspace),
        "der Plan-Modus schneidet die aktuelle Sandbox des Elternteils"
    );
    let grandchild = spawner
        .admit("executor", spawn_input(root.clone()), root_sandbox, None)
        .map_err(ctx("Enkel im Plan-Modus"))?;
    let fs_write = ToolName::new("fs.write");
    {
        let manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = manager.get(&grandchild).map_err(ctx("Enkel"))?;
        assert_eq!(session.mode(), InteractionMode::Plan);
        assert!(!session.activation().is_tool_enabled(&fs_write));
        assert!(
            session.base_activation().is_tool_enabled(&fs_write),
            "die Basis des Enkels trägt nicht die Plan-Decke des Elternteils"
        );
    }

    spawner.live_mode().publish(InteractionMode::Work);
    let mut manager = spawner
        .manager
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let session = manager.get_mut(&grandchild).map_err(ctx("Enkel"))?;
    assert_eq!(
        session.sync_live_mode(),
        Some((InteractionMode::Plan, InteractionMode::Work))
    );
    assert!(session.activation().is_tool_enabled(&fs_write));
    let permissions = session
        .spawn_context()
        .map(|context| context.sandbox.permissions().clone())
        .ok_or(TestError::Missing("Spawn-Kontext"))?;
    assert!(permissions.contains(Permission::WriteWorkspace));
    assert!(permissions.contains(Permission::ExecuteProcess));
    Ok(())
}
