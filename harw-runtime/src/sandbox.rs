//! Die eine Sandbox-Montage aller `harw`-Einstiege.
//!
//! # Verantwortungsbereich
//! Vor W2b baute jeder Einstieg seine Wurzel-Sandbox selbst: `harw-cli`
//! (`main.rs`, `build_local_spawn_context`), `harw-cli` (`chat.rs`,
//! `build_tui_sandbox` und `build_one_shot_spawn_context`), `harw-tui`
//! (`app.rs`, `sandbox_for_project`) und der Plan-Knoten-Job-Worker
//! (`job_worker.rs`, `derive_plan_node_sandbox`). Vier Stellen, drei
//! Mandanten-Schreibweisen (`cli`, `tui`, `local-tui`) und viermal dieselbe
//! hartkodierte Rechteliste `{Read, Write, Execute}` — auch dort, wo die
//! Reduktionstabelle weniger vorsieht. Dieses Modul ist die eine Stelle:
//! die Rechte kommen ausschließlich aus [`EntryKind::profile`], die
//! Workspace-Bindung entsteht nach genau einem Muster, und Netzrechte
//! vergibt keine der drei Funktionen.
//!
//! `build_local_spawn_context` selbst ist seit W2d-2 entfernt; sein
//! Nachfolger ist [`root_sandbox`] hier, aufgerufen aus der
//! RuntimeAssembly-Montage (siehe [`crate::RuntimeAssembly`]).
//!
//! # Netz
//! Jede hier gebaute [`SandboxSpec`] entsteht über
//! [`SandboxSpec::from_resolved`] und trägt damit den leeren
//! [`harw_authority::NetworkScope`]; [`harw_authority::Permission::NetworkAccess`]
//! ist in keinem Eintrag der Reduktionstabelle enthalten (Vertrag
//! `docs/remediation/CONTRACTS.md` §runtime-spec: „Netz überall leer bis
//! Welle W5 (P1.7)").
//!
//! # Fehler
//! Alle fallierenden Funktionen melden [`RuntimeError::Sandbox`]; die
//! zugrundeliegende [`harw_sandbox::SandboxError`] steht wörtlich im
//! `detail`.

use std::path::{Path, PathBuf};

use harw_authority::{
    Permission, PermissionRequest, PermissionSet, SandboxSpec, WorkspaceRegistration,
    WorkspaceRegistry,
};
use harw_plan::PlanNodeKind;
use harw_types::{PermissionTier, TenantId, WorkspaceId};

use crate::error::{RuntimeError, RuntimeResult};
use crate::spec::EntryKind;

/// Der Workspace-Alias, unter dem jeder Einstieg sein Projekt bindet.
///
/// # Beschreibung
/// Alle vier Montagestellen benutzten bereits denselben Alias `project`;
/// er steht hier einmal statt viermal.
const PROJECT_WORKSPACE: &str = "project";

/// Der Mandant eines Einstiegs.
///
/// # Beschreibung
/// Der Mandantenname trennt Bindungen innerhalb einer
/// [`WorkspaceRegistry`]; er ist kein Recht. Die historischen Schreibweisen
/// (`cli` in `main.rs`/`chat.rs`, `tui` in `build_tui_sandbox`, `local-tui`
/// in `app.rs::sandbox_for_project`) bezeichneten dieselben zwei Oberflächen
/// und fallen hier auf eine Tabelle zusammen: ein Name je Einstiegsfamilie.
///
/// # Argumente
/// - `entry` ([`EntryKind`]): der Einstieg.
///
/// # Rückgabe
/// Der stabile Mandantenname des Einstiegs.
const fn tenant_name(entry: EntryKind) -> &'static str {
    match entry {
        EntryKind::Tui => "tui",
        EntryKind::OneShot | EntryKind::LocalEcho | EntryKind::Analyze | EntryKind::Doctor => "cli",
        EntryKind::Web => "web",
        EntryKind::McpServe => "mcp",
        EntryKind::JobPrompt | EntryKind::JobPlanNode => "job",
        EntryKind::GatewayTelegram => "gateway-telegram",
        EntryKind::GatewayDream => "gateway-dream",
    }
}

/// Bindet `project_root` als Workspace `project` des Mandanten `tenant`.
///
/// # Beschreibung
/// Folgt dem Muster aller bisherigen Montagestellen: eine Registry mit genau
/// einer Registrierung, deren Wurzel relativ (`.`) unter dem
/// Harness-Root liegt. [`WorkspaceRegistry::build`] kanonisiert dabei einmal
/// und lehnt fehlende, nicht-verzeichnisartige und ausbrechende Wurzeln ab —
/// diese Prüfung ist der Grund, warum die Bindung überhaupt über die
/// Registry und nicht über einen direkten Konstruktor läuft.
///
/// # Argumente
/// - `tenant` (`&str`): Mandantenname aus [`tenant_name`].
/// - `project_root` (`&Path`): Wurzel des Projekts.
///
/// # Errors
/// [`RuntimeError::Sandbox`], wenn `project_root` nicht kanonisierbar ist,
/// kein Verzeichnis ist oder die Registrierung scheitert.
fn bind_project(
    tenant: &str,
    project_root: &Path,
) -> RuntimeResult<harw_authority::WorkspaceBinding> {
    let tenant = TenantId::from_str(tenant);
    let workspace = WorkspaceId::from_str(PROJECT_WORKSPACE);
    let registry = WorkspaceRegistry::build(
        project_root,
        [WorkspaceRegistration {
            tenant: tenant.clone(),
            workspace: workspace.clone(),
            root: PathBuf::from("."),
        }],
    )
    .map_err(|error| RuntimeError::Sandbox {
        detail: format!(
            "could not bind {} as workspace root: {error}",
            project_root.display()
        ),
    })?;
    registry
        .resolve(&tenant, &workspace)
        .map_err(|error| RuntimeError::Sandbox {
            detail: format!("could not resolve the bound project workspace: {error}"),
        })
}

/// Baut die Wurzel-Sandbox eines Einstiegs.
///
/// # Beschreibung
/// Die Rechte stammen ausschließlich aus [`EntryKind::profile`] — diese
/// Funktion trifft keine eigene Rechteentscheidung und kennt insbesondere
/// keine hartkodierte `{Read, Write, Execute}`-Liste mehr. Für
/// [`EntryKind::Web`] liefert das Profil die Spec-Obergrenze `{Read}`; die
/// zusätzliche Verengung nach Aufrufer-Tier leistet
/// [`permissions_for_tier`], die der Web-Einstieg per
/// [`SandboxSpec::restrict`] auf das Ergebnis anwendet (Autorität wird nur
/// geschnitten, nie neu vergeben).
///
/// # Argumente
/// - `entry` ([`EntryKind`]): der Einstieg, dessen Profil die Rechte nennt.
/// - `project_root` (`&Path`): Wurzel des Projekts, an die die Sandbox
///   gebunden wird.
///
/// # Rückgabe
/// Eine [`SandboxSpec`] mit den Profilrechten, gebunden an `project_root`
/// und mit leerem Netz-Scope.
///
/// # Errors
/// [`RuntimeError::Sandbox`], wenn `project_root` nicht kanonisierbar ist,
/// kein Verzeichnis ist oder die Workspace-Registrierung scheitert.
pub fn root_sandbox(entry: EntryKind, project_root: &Path) -> RuntimeResult<SandboxSpec> {
    let binding = bind_project(tenant_name(entry), project_root)?;
    Ok(SandboxSpec::from_resolved(
        binding,
        entry.profile().permissions,
    ))
}

/// Die Rechte, die ein Aufrufer-Tier höchstens tragen darf.
///
/// # Beschreibung
/// Schließt Befund F-045: Der Web-Einstieg nahm bisher jedes
/// [`PermissionTier`] entgegen, ohne die Sandbox danach zu verengen
/// (`harw-cli/src/web.rs`: die `WebContextFactory` band den Tier-Parameter
/// an `_tier` und baute für jeden Aufrufer denselben Kontext). Ein
/// `Observer` erhielt damit dieselbe Autorität wie ein `Owner`.
///
/// Die Tabelle ist monoton in der Tier-Ordnung
/// (`Observer < Operator < Maintainer < Owner`): jede Stufe enthält die
/// Rechte der darunterliegenden.
///
/// | Tier | Rechte |
/// |---|---|
/// | `Observer` | `{ReadWorkspace}` |
/// | `Operator` | `{ReadWorkspace, WriteWorkspace}` |
/// | `Maintainer` | `{ReadWorkspace, WriteWorkspace, ExecuteProcess}` |
/// | `Owner` | `{ReadWorkspace, WriteWorkspace, ExecuteProcess}` |
///
/// Kein Tier trägt [`Permission::NetworkAccess`], [`Permission::ReadSecrets`],
/// [`Permission::ManagePlugins`] oder [`Permission::ReadCargoRegistry`]: ein
/// Tier beschreibt den Rang des Aufrufers, nicht eine zusätzliche
/// Operationsklasse.
///
/// # Argumente
/// - `tier` ([`PermissionTier`]): der Rang des vertrauenswürdig ermittelten
///   Aufrufers ([`harw_types::Principal::tier`]).
///
/// # Rückgabe
/// Die Obergrenze, mit der der Einstieg seine Wurzel-Sandbox schneidet.
///
/// # Examples
/// ```rust
/// use harw_runtime::sandbox::permissions_for_tier;
/// use harw_authority::Permission;
/// use harw_types::PermissionTier;
///
/// let observer = permissions_for_tier(PermissionTier::Observer);
/// assert!(observer.contains(Permission::ReadWorkspace));
/// assert!(!observer.contains(Permission::WriteWorkspace));
/// ```
#[must_use]
pub fn permissions_for_tier(tier: PermissionTier) -> PermissionSet {
    use Permission::{ExecuteProcess, ReadWorkspace, WriteWorkspace};

    match tier {
        PermissionTier::Observer => PermissionSet::from_policy([ReadWorkspace]),
        PermissionTier::Operator => PermissionSet::from_policy([ReadWorkspace, WriteWorkspace]),
        PermissionTier::Maintainer | PermissionTier::Owner => {
            PermissionSet::from_policy([ReadWorkspace, WriteWorkspace, ExecuteProcess])
        }
    }
}

/// Ob eine Knotenart überhaupt schreibend arbeiten kann.
///
/// # Beschreibung
/// Übernommen aus `harw-cli/src/job_worker.rs::profile_for_node_kind`: dort
/// entschied dieselbe Fallunterscheidung, ob ein Knoten das schreibende
/// Registry-Profil sieht. Eine Recherche schreibt nicht, eine
/// Implementierung schon. Die Funktion vergibt nichts — sie kann das
/// Schreibrecht nur wegnehmen.
const fn kind_may_write(kind: PlanNodeKind) -> bool {
    match kind {
        PlanNodeKind::Coding
        | PlanNodeKind::Integration
        | PlanNodeKind::Verification
        | PlanNodeKind::Docs
        | PlanNodeKind::Contract => true,
        PlanNodeKind::Research
        | PlanNodeKind::Explore
        | PlanNodeKind::Analysis
        | PlanNodeKind::Synthesis
        | PlanNodeKind::Composite => false,
    }
}

/// Baut die Sandbox eines Plan-Knoten-Jobs.
///
/// # Beschreibung
/// Die Obergrenze ist das Profil von [`EntryKind::JobPlanNode`] (`{Read,
/// Write}`); darauf wird ausschließlich **geschnitten**
/// ([`SandboxSpec::restrict`]). `WriteWorkspace` überlebt nur, wenn beides
/// gilt: der Knoten ist eine schreibende Art (`kind_may_write`) **und** der
/// Aufrufer hat für diesen Knoten Schreibrecht ermittelt (`may_write`, im
/// Job-Worker das Ergebnis der Vertragsableitung
/// `derive_plan_node_sandbox`). [`Permission::ExecuteProcess`] und
/// [`Permission::NetworkAccess`] sind bereits in der Obergrenze nicht
/// enthalten und können deshalb auf keinem Weg entstehen.
///
/// # Argumente
/// - `kind` ([`PlanNodeKind`]): Art des Plan-Knotens.
/// - `may_write` (`bool`): ob der Knotenvertrag Schreibrecht trägt.
/// - `project_root` (`&Path`): Wurzel des Projekts.
///
/// # Rückgabe
/// Eine [`SandboxSpec`] mit `{Read}` oder `{Read, Write}` und leerem Netz.
///
/// # Errors
/// [`RuntimeError::Sandbox`], wenn `project_root` nicht kanonisierbar ist,
/// kein Verzeichnis ist oder die Workspace-Registrierung scheitert.
pub fn plan_node_sandbox(
    kind: PlanNodeKind,
    may_write: bool,
    project_root: &Path,
) -> RuntimeResult<SandboxSpec> {
    let root = root_sandbox(EntryKind::JobPlanNode, project_root)?;
    let ceiling = if may_write && kind_may_write(kind) {
        PermissionSet::from_policy([Permission::ReadWorkspace, Permission::WriteWorkspace])
    } else {
        PermissionSet::from_policy([Permission::ReadWorkspace])
    };
    Ok(root.restrict(&PermissionRequest::from_permissions(ceiling.iter())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Alle Einstiege, in der Reihenfolge der Vertragstabelle.
    const ALL_ENTRIES: [EntryKind; 11] = [
        EntryKind::Tui,
        EntryKind::OneShot,
        EntryKind::LocalEcho,
        EntryKind::Analyze,
        EntryKind::Doctor,
        EntryKind::Web,
        EntryKind::McpServe,
        EntryKind::JobPrompt,
        EntryKind::JobPlanNode,
        EntryKind::GatewayTelegram,
        EntryKind::GatewayDream,
    ];

    /// Alle Knotenarten; das `match` in [`kind_may_write`] erzwingt beim
    /// Kompilieren, dass keine Variante fehlt, diese Liste hält den Test
    /// vollständig.
    const ALL_KINDS: [PlanNodeKind; 10] = [
        PlanNodeKind::Research,
        PlanNodeKind::Explore,
        PlanNodeKind::Analysis,
        PlanNodeKind::Synthesis,
        PlanNodeKind::Contract,
        PlanNodeKind::Coding,
        PlanNodeKind::Integration,
        PlanNodeKind::Verification,
        PlanNodeKind::Docs,
        PlanNodeKind::Composite,
    ];

    const ALL_TIERS: [PermissionTier; 4] = [
        PermissionTier::Observer,
        PermissionTier::Operator,
        PermissionTier::Maintainer,
        PermissionTier::Owner,
    ];

    /// Ein isoliertes, kanonisierbares Verzeichnis für die Workspace-Bindung,
    /// das beim Fallenlassen automatisch entfernt wird. Die Tests schreiben
    /// nichts hinein; sie brauchen nur einen Pfad, den
    /// [`WorkspaceRegistry::build`] akzeptiert.
    fn existing_root() -> tempfile::TempDir {
        tempfile::TempDir::new().expect("temp dir")
    }

    #[test]
    fn root_sandbox_takes_its_permissions_from_the_entry_profile() {
        let dir = existing_root();
        let root = dir.path();
        for entry in ALL_ENTRIES {
            let sandbox = root_sandbox(entry, root).expect("temp dir binds as workspace root");
            assert_eq!(
                sandbox.permissions(),
                &entry.profile().permissions,
                "{entry:?}"
            );
        }
    }

    #[test]
    fn no_entry_sandbox_carries_network() {
        let dir = existing_root();
        let root = dir.path();
        for entry in ALL_ENTRIES {
            let sandbox = root_sandbox(entry, root).expect("temp dir binds as workspace root");
            assert!(
                !sandbox.permissions().contains(Permission::NetworkAccess),
                "{entry:?} holds NetworkAccess"
            );
            assert!(
                sandbox.network_scope().is_empty(),
                "{entry:?} has a non-empty egress scope"
            );
        }
    }

    #[test]
    fn root_sandbox_binds_the_given_project_root() {
        let dir = existing_root();
        let root = dir.path();
        let canonical = root.canonicalize().expect("temp dir is canonicalizable");
        let sandbox = root_sandbox(EntryKind::Tui, root).expect("temp dir binds as workspace root");
        assert_eq!(sandbox.workspace().canonical_root(), canonical.as_path());
        assert_eq!(sandbox.workspace().workspace().as_str(), PROJECT_WORKSPACE);
        assert_eq!(sandbox.workspace().tenant().as_str(), "tui");
    }

    #[test]
    fn root_sandbox_rejects_a_root_that_is_not_a_directory() {
        let dir = existing_root();
        let missing = dir.path().join("harw-runtime-no-such-directory-w2b02");
        let error = root_sandbox(EntryKind::Tui, &missing).expect_err("missing root must fail");
        assert!(matches!(error, RuntimeError::Sandbox { .. }));
    }

    #[test]
    fn tier_table_matches_the_contract() {
        use Permission::{ExecuteProcess as X, ReadWorkspace as R, WriteWorkspace as W};

        let expected: [(PermissionTier, &[Permission]); 4] = [
            (PermissionTier::Observer, &[R]),
            (PermissionTier::Operator, &[R, W]),
            (PermissionTier::Maintainer, &[R, W, X]),
            (PermissionTier::Owner, &[R, W, X]),
        ];
        for (tier, permissions) in expected {
            assert_eq!(
                permissions_for_tier(tier),
                PermissionSet::from_policy(permissions.iter().copied()),
                "{tier:?}"
            );
        }
    }

    #[test]
    fn tiers_are_monotone_and_never_grant_network_or_secrets() {
        let forbidden = [
            Permission::NetworkAccess,
            Permission::ReadSecrets,
            Permission::ManagePlugins,
            Permission::ReadCargoRegistry,
        ];
        for tier in ALL_TIERS {
            let permissions = permissions_for_tier(tier);
            for permission in forbidden {
                assert!(!permissions.contains(permission), "{tier:?} {permission:?}");
            }
        }
        for pair in ALL_TIERS.windows(2) {
            let lower = permissions_for_tier(pair[0]);
            let higher = permissions_for_tier(pair[1]);
            assert!(
                lower.is_subset_of(&higher),
                "{:?} is not contained in {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn web_entry_narrowed_by_tier_never_exceeds_its_profile() {
        let dir = existing_root();
        let root = dir.path();
        let sandbox = root_sandbox(EntryKind::Web, root).expect("temp dir binds");
        for tier in ALL_TIERS {
            let narrowed = sandbox.restrict(&PermissionRequest::from_permissions(
                permissions_for_tier(tier).iter(),
            ));
            narrowed
                .ensure_child_of(&sandbox)
                .expect("a tier narrowing is always a reduction");
            assert!(!narrowed.permissions().contains(Permission::WriteWorkspace));
            assert!(!narrowed.permissions().contains(Permission::ExecuteProcess));
        }
    }

    #[test]
    fn plan_node_sandbox_is_monotone_and_never_executes_or_networks() {
        let dir = existing_root();
        let root = dir.path();
        let parent = root_sandbox(EntryKind::JobPlanNode, root).expect("temp dir binds");
        for kind in ALL_KINDS {
            for may_write in [false, true] {
                let sandbox = plan_node_sandbox(kind, may_write, root).expect("temp dir binds");
                sandbox
                    .ensure_child_of(&parent)
                    .expect("plan-node sandbox must be a reduction of the job sandbox");
                assert!(sandbox.permissions().contains(Permission::ReadWorkspace));
                assert!(
                    !sandbox.permissions().contains(Permission::ExecuteProcess),
                    "{kind:?}/{may_write}"
                );
                assert!(
                    !sandbox.permissions().contains(Permission::NetworkAccess),
                    "{kind:?}/{may_write}"
                );
                assert!(sandbox.network_scope().is_empty(), "{kind:?}/{may_write}");
                assert_eq!(
                    sandbox.permissions().contains(Permission::WriteWorkspace),
                    may_write && kind_may_write(kind),
                    "{kind:?}/{may_write}"
                );
            }
        }
    }

    #[test]
    fn read_only_node_kinds_never_write_even_with_a_writing_contract() {
        let dir = existing_root();
        let root = dir.path();
        for kind in [
            PlanNodeKind::Research,
            PlanNodeKind::Explore,
            PlanNodeKind::Analysis,
            PlanNodeKind::Synthesis,
            PlanNodeKind::Composite,
        ] {
            let sandbox = plan_node_sandbox(kind, true, root).expect("temp dir binds");
            assert!(
                !sandbox.permissions().contains(Permission::WriteWorkspace),
                "{kind:?}"
            );
        }
    }
}
