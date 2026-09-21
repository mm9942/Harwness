//! Der Interaktionsmodus schneidet, er ersetzt nicht (F-153, G-006, G-045).
//!
//! Zwei Eigenschaften werden hier über die öffentliche API von
//! [`harw_core::session::AgentSession`] festgenagelt — beide waren vor dieser
//! Änderung verletzt:
//!
//! 1. **Decke:** Was die Agent-Definition (IR) verbietet, bleibt in *jedem*
//!    Modus verboten. `set_mode(Work)` baute die Aktivierung bisher frisch aus
//!    dem Modus-Profil auf und warf die deny-by-default-Werkzeugfläche der IR
//!    damit weg.
//! 2. **Reversibilität:** `/mode explore` gefolgt von `/mode work` stellt genau
//!    die Basis wieder her — nicht mehr (nie über die Basis hinaus) und nicht
//!    weniger (keine Ratsche, die eine Sitzung dauerhaft entrechtet).
//!
//! Die Sitzungen werden ausschließlich über die öffentlichen Builder gebaut,
//! nicht über interne Felder: genau so entstehen sie auch in TUI, CLI und
//! `ManagedAgentSpawner::admit`.

use harw_agent_dsl::ExecutableAgentIr;
use harw_agent_dsl::authority::AuthorityCeiling;
use harw_agent_dsl::lower;
use harw_agent_dsl::parse::parse_toml;
use harw_agent_dsl::resolved::{ResolutionTrace, ResolvedAgentDefinition};
use harw_agent_dsl::roles::AgentRoleId;
use harw_core::activation::{SessionActivation, ToolProfile};
use harw_core::mode::InteractionMode;
use harw_core::session::{AgentSession, SpawnContext};
use harw_extension_api::ExtensionRegistryBuilder;
use harw_authority::{Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
use harw_tools::ToolName;
use harw_types::{AgentRole, SessionId, TenantId, WorkspaceId};
use std::path::PathBuf;
use tokio::sync::mpsc;

/// Baut eine Session ohne Sandbox und ohne IR — die Basis ist der Default.
fn plain_session() -> AgentSession {
    let (event_tx, _receiver) = mpsc::unbounded_channel();
    AgentSession::new_with_id(
        SessionId::new(),
        AgentRole::Assistant,
        None,
        ExtensionRegistryBuilder::default().build(),
        event_tx,
    )
}

/// Friert eine Agent-Definition mit genau dieser Werkzeugfläche ein.
///
/// Wortgleich zum Helfer der Unit-Tests in `session.rs`: `parse_toml` + `lower`
/// sind der einzige öffentliche Weg zu einer [`ExecutableAgentIr`].
fn executable_agent_ir(admitted: &[&str], forbidden: &[&str]) -> ExecutableAgentIr {
    let admitted = admitted
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let forbidden = forbidden
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let raw = parse_toml(&format!(
        r#"
schema = "harwness.agent/v1"
id = "harwness.agent.session-mode-intersection@1"
version = "1.0.0"
role = "worker"
specialization = "session-mode-intersection"

[tools]
admitted = [{admitted}]
forbidden = [{forbidden}]
"#
    ))
    .expect("Test-Agent-Definition muss parsen");
    let resolved = ResolvedAgentDefinition {
        id: raw.id,
        version: raw.version,
        role: raw.role,
        specialization: raw.specialization,
        name: raw.name,
        description: raw.description,
        authority: AuthorityCeiling::default(),
        trace: ResolutionTrace { steps: Vec::new() },
        config: raw.tables,
    };

    lower(&resolved).expect("Test-Agent-Definition muss lowern")
}

/// Sandbox auf dem echten Harness-Verzeichnis mit genau diesen Permissions.
fn test_sandbox(permissions: &[Permission]) -> SandboxSpec {
    let harness_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("harw-core hat ein Workspace-Elternverzeichnis")
        .to_path_buf();
    let registry = WorkspaceRegistry::build(
        &harness_root,
        [WorkspaceRegistration {
            tenant: TenantId::from_str("test-tenant"),
            workspace: WorkspaceId::from_str("core-mode-intersection-tests"),
            root: PathBuf::from("harw-core"),
        }],
    )
    .expect("Test-Workspace ist registrierbar");
    SandboxSpec::from_resolved(
        registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("core-mode-intersection-tests"),
            )
            .expect("Test-Workspace löst auf"),
        PermissionSet::from_policy(permissions.iter().copied()),
    )
}

fn spawn_context(sandbox: SandboxSpec) -> SpawnContext {
    SpawnContext {
        sandbox,
        suggestions: None,
        capability_snapshot: None,
        approval_actor: None,
        organizational_role: AgentRoleId::RootOrchestrator,
        allowed_child_orchestrators: Vec::new(),
        trace: None,
        ceiling: None,
    }
}

fn sandbox_of(session: &AgentSession) -> &SandboxSpec {
    &session
        .spawn_context()
        .expect("Test-Session hat einen Spawn-Kontext")
        .sandbox
}

fn enabled(session: &AgentSession, name: &str) -> bool {
    session.activation().is_tool_enabled(&ToolName::new(name))
}

// ---------------------------------------------------------------------------
// 1. Decke: das IR-Verbot überlebt jeden Modus
// ---------------------------------------------------------------------------

#[test]
fn ir_forbidden_tool_stays_forbidden_after_set_mode_work() {
    let executable = executable_agent_ir(&["fs.read", "shell.exec"], &["shell.exec"]);
    let mut session = plain_session().with_executable_agent_ir(&executable);

    assert!(!enabled(&session, "shell.exec"));

    session.set_mode(InteractionMode::Work);

    assert!(
        !enabled(&session, "shell.exec"),
        "ein `forbidden` der Agent-Definition darf durch keinen Modus zurückkehren"
    );
    assert!(
        enabled(&session, "fs.read"),
        "ein zugelassenes Werkzeug bleibt in Work sichtbar"
    );
}

#[test]
fn set_mode_work_admits_nothing_the_ir_never_admitted() {
    let executable = executable_agent_ir(&["fs.read"], &[]);
    let mut session = plain_session().with_executable_agent_ir(&executable);

    session.set_mode(InteractionMode::Work);

    assert_eq!(session.mode(), InteractionMode::Work);
    for never_admitted in ["fs.write", "shell.exec", "web.fetch", "custom.tool"] {
        assert!(
            !enabled(&session, never_admitted),
            "Work darf {never_admitted} nicht freischalten — die IR hat es nie zugelassen"
        );
    }
    assert_ne!(
        session.activation().profile(),
        ToolProfile::Full,
        "das Full-Profil des Modus darf die deny-by-default-Fläche der IR nicht ersetzen"
    );
}

// ---------------------------------------------------------------------------
// 2. Reversibilität: Explore → Work stellt die Basis wieder her
// ---------------------------------------------------------------------------

#[test]
fn explore_then_work_restores_exactly_the_ir_tool_surface() {
    let executable = executable_agent_ir(&["fs.read", "fs.write"], &["shell.exec"]);
    let mut session = plain_session().with_executable_agent_ir(&executable);

    session.set_mode(InteractionMode::Explore);
    assert!(enabled(&session, "fs.read"), "Explore liest");
    assert!(!enabled(&session, "fs.write"), "Explore schreibt nicht");

    session.set_mode(InteractionMode::Work);

    assert!(
        enabled(&session, "fs.write"),
        "der Rückweg nach Work muss die Basis-Freigabe zurückgeben — sonst ist /mode eine Ratsche"
    );
    assert!(enabled(&session, "fs.read"));
    assert!(
        !enabled(&session, "shell.exec"),
        "der Rückweg gibt nur die Basis zurück, nie mehr"
    );
}

#[test]
fn base_activation_is_the_unmodified_ir_surface_while_a_mode_narrows_it() {
    let executable = executable_agent_ir(&["fs.read", "fs.write"], &[]);
    let mut session = plain_session().with_executable_agent_ir(&executable);
    session.set_mode(InteractionMode::Explore);

    let base = session.base_activation();
    assert!(
        base.is_tool_enabled(&ToolName::new("fs.write")),
        "die Basis kennt fs.write weiterhin"
    );
    assert!(
        !enabled(&session, "fs.write"),
        "die wirksame Aktivierung ist die engere von beiden"
    );
}

#[test]
fn sandbox_after_explore_then_work_equals_the_base_sandbox() {
    let base_sandbox = test_sandbox(&[
        Permission::ReadWorkspace,
        Permission::WriteWorkspace,
        Permission::ExecuteProcess,
    ]);
    let mut session = plain_session().with_spawn_context(spawn_context(base_sandbox.clone()));
    assert_eq!(sandbox_of(&session), &base_sandbox);

    session.set_mode(InteractionMode::Explore);
    assert!(
        !sandbox_of(&session)
            .permissions()
            .contains(Permission::WriteWorkspace),
        "Explore entzieht das Schreibrecht"
    );

    session.set_mode(InteractionMode::Work);

    assert_eq!(
        sandbox_of(&session),
        &base_sandbox,
        "Work stellt die Basis-Sandbox vollständig wieder her: Workspace, Permissions, NetworkScope"
    );
}

#[test]
fn no_mode_ever_exceeds_the_base_sandbox() {
    let base_sandbox = test_sandbox(&[Permission::ReadWorkspace]);
    let mut session = plain_session().with_spawn_context(spawn_context(base_sandbox.clone()));

    for mode in [
        InteractionMode::Explore,
        InteractionMode::Plan,
        InteractionMode::Work,
        InteractionMode::Chat,
    ] {
        session.set_mode(mode);
        let permissions = sandbox_of(&session).permissions().clone();
        assert!(
            permissions.is_subset_of(base_sandbox.permissions()),
            "{mode:?} darf die Basis-Autorität nicht überschreiten"
        );
        assert!(
            sandbox_of(&session).ensure_child_of(&base_sandbox).is_ok(),
            "{mode:?} muss eine zulässige Verengung der Basis bleiben"
        );
    }
}

#[test]
fn mode_intersection_is_independent_of_builder_order() {
    let executable = executable_agent_ir(&["fs.read", "fs.write"], &[]);
    let permissions = [Permission::ReadWorkspace, Permission::WriteWorkspace];

    let mode_first = plain_session()
        .with_mode(InteractionMode::Explore)
        .with_executable_agent_ir(&executable)
        .with_spawn_context(spawn_context(test_sandbox(&permissions)));
    let mode_last = plain_session()
        .with_executable_agent_ir(&executable)
        .with_spawn_context(spawn_context(test_sandbox(&permissions)))
        .with_mode(InteractionMode::Explore);

    assert_eq!(sandbox_of(&mode_first), sandbox_of(&mode_last));
    for name in ["fs.read", "fs.write", "shell.exec"] {
        assert_eq!(
            enabled(&mode_first, name),
            enabled(&mode_last, name),
            "{name} darf nicht von der Reihenfolge der Builder-Aufrufe abhängen"
        );
    }
    assert!(enabled(&mode_first, "fs.read"));
    assert!(!enabled(&mode_first, "fs.write"));
}

#[test]
fn with_activation_is_independent_of_builder_order() {
    // A4/A2: `with_activation` setzt die Basis und wendet den Modus an. Ohne
    // den `apply_mode`-Aufruf bliebe `mode_first` ungeschnitten und `fs.write`
    // in `Explore` offen.
    let mut base = SessionActivation::new(ToolProfile::Full);
    base.disable_tool(ToolName::new("shell.exec"));

    let mode_first = plain_session()
        .with_mode(InteractionMode::Explore)
        .with_activation(base.clone());
    let mode_last = plain_session()
        .with_activation(base)
        .with_mode(InteractionMode::Explore);

    for name in ["fs.read", "fs.write", "shell.exec"] {
        assert_eq!(
            enabled(&mode_first, name),
            enabled(&mode_last, name),
            "{name} darf nicht von der Reihenfolge der Builder-Aufrufe abhängen"
        );
    }
    assert!(enabled(&mode_first, "fs.read"), "Explore lässt fs.read zu");
    assert!(
        !enabled(&mode_first, "fs.write"),
        "die Explore-Decke greift auch, wenn die Basis danach gesetzt wird"
    );
    assert!(
        !enabled(&mode_first, "shell.exec"),
        "das Verbot der Basis bleibt in jedem Fall bestehen"
    );
}

#[test]
fn a_base_set_through_with_activation_survives_a_mode_switch() {
    // A4(a): dieselbe Zusage wie für `with_executable_agent_ir`, nur über den
    // zweiten Basis-Setzer — `Work` ist die weiteste Modus-Decke.
    let mut base = SessionActivation::new(ToolProfile::Full);
    base.disable_tool(ToolName::new("shell.exec"));
    let mut session = plain_session().with_activation(base);

    for mode in [
        InteractionMode::Explore,
        InteractionMode::Work,
        InteractionMode::Plan,
        InteractionMode::Chat,
    ] {
        session.set_mode(mode);
        assert!(
            !enabled(&session, "shell.exec"),
            "{mode:?} darf ein Verbot der Basis nicht zurücknehmen"
        );
    }
    assert!(
        enabled(&session, "fs.write"),
        "Chat stellt die Basis vollständig wieder her — keine Ratsche"
    );
}

#[test]
fn narrow_base_activation_is_monotone_and_outlives_every_mode() {
    // A1: die Decke landet in der Basis, nicht im abgeleiteten Wert.
    let executable = executable_agent_ir(&["fs.read", "fs.write", "shell.exec"], &[]);
    let mut session = plain_session().with_executable_agent_ir(&executable);
    assert!(enabled(&session, "shell.exec"));

    let mut ceiling = SessionActivation::new(ToolProfile::Full);
    ceiling.disable_tool(ToolName::new("shell.exec"));
    session.narrow_base_activation(&ceiling);

    session.set_mode(InteractionMode::Work);
    assert!(
        !enabled(&session, "shell.exec"),
        "die Verengung überlebt den Moduswechsel"
    );
    assert!(enabled(&session, "fs.write"), "Work gibt die Basis frei");

    // Monoton: eine zweite, disjunkte Verengung nimmt weiter weg und gibt
    // nichts zurück.
    let mut second = SessionActivation::new(ToolProfile::Full);
    second.disable_tool(ToolName::new("fs.write"));
    session.narrow_base_activation(&second);
    assert!(!enabled(&session, "shell.exec"));
    assert!(!enabled(&session, "fs.write"));
    assert!(enabled(&session, "fs.read"));
}
