//! Closes the "spawn authority" gap the existing registry-defaults test
//! suite left open.
//!
//! # The gap
//! `role_rights_matrix.rs` and `tool_admission_coverage.rs` (this crate)
//! prove which *tools* a role is admitted, never whether a
//! `UserInterface`-governed caller may spawn that role as a durable child at
//! all. That second question is a separate axis — the §3 DSL spawn matrix
//! (`harw-agent-dsl/src/roles.rs::can_spawn`) — and nothing in this crate's
//! test suite tied a *registered* role back to it.
//!
//! The gap mattered concretely: `/explore` and `/research-web`
//! (`harw-ops/src/{explore,research}.rs`) used to hand every caller the
//! plain `role_names::EXPLORER` / `role_names::RESEARCHER_WEB` roles, which
//! resolve to organizational role `Worker`. A `UserInterface`-governed root
//! session (the UIA chat session) may never spawn a `Worker`
//! (`can_spawn(UserInterface, Worker) == false`), so both tools failed for
//! every UIA caller — and no test here would have caught it, because both
//! existing files only ever check tool admission, never spawn eligibility.
//!
//! # What this file proves
//! 1. [`test_plain_explore_and_research_roles_are_worker_and_denied_to_uia`]
//!    — regression: the plain roles resolve to `Worker` and are denied to a
//!    `UserInterface` caller. This is the bug as it stood before the fix.
//! 2. [`test_uia_explorer_and_uia_writer_are_uia_worker_and_allowed_for_uia`]
//!    — the fix: `role_names::UIA_EXPLORER` / `role_names::UIA_WRITER`
//!    resolve to organizational role `UiaWorker`, which a `UserInterface`
//!    caller may spawn. These are the exact redirect targets `/explore` and
//!    `/research-web` must use once a UIA caller invokes them.
//! 3. [`test_matrix_roles_are_read_only_worker_roles_fit_for_the_uia_allowlist`]
//!    — die `/matrix`-Rollen (`role_names::MATRIX_ROLES`) sind `Worker`, die
//!    die Spawn-Matrix einer UIA-Wurzel verweigert; sie werden nur über die
//!    enge Freigabeliste `ManagedAgentSpawner::with_uia_spawnable_roles`
//!    admittiert. Der Test belegt die Sicherheitsvoraussetzung dieser Liste:
//!    jede gelistete Rolle ist nur lesend ohne Netz, Schreiben oder Exec
//!    (`RegistryProfile::MatrixReader`: höchstens die lesenden
//!    Datei-Werkzeuge; `AuthorityReducer::ReadOnly`: kein Netz).
//!
//! Both tests resolve roles through the real [`builtin_agent_definitions`]
//! pipeline — the same one `harw-runtime` mounts for `EntryKind::Tui` /
//! `EntryKind::OneShot` — not a hand-rolled fixture, so a drift between the
//! registered TOML and the spawn matrix would show up here.
//!
//! # Determinism
//! No network, filesystem, or process access beyond reading this crate's own
//! embedded definitions (identical precondition to the sibling test files).

use std::collections::HashMap;

use harw_agent_dsl::roles::{AgentRoleId, can_spawn};
use harw_authority::Permission;
use harw_registry_defaults::authority::{AuthorityReducer, authority_reducer_for_role};
use harw_registry_defaults::embedded_agents::builtin_agent_definitions;
use harw_registry_defaults::profile::{RegistryProfile, profile_for_role, role_names};

mod common;
use common::{TestError, TestResult, ctx};

/// Resolves the embedded builtin role definitions with no local overrides —
/// identical helper shape to `tool_admission_coverage.rs::resolved_roles`.
fn resolved_roles() -> TestResult<HashMap<String, harw_agent_dsl::ExecutableAgentIr>> {
    builtin_agent_definitions(&HashMap::new()).map_err(ctx("builtin role definitions must resolve"))
}

/// The builtin roles that `/explore` and `/research-web` hand every caller
/// today, regardless of organizational role — the design conflict this
/// crate's UIA fix closes.
const PLAIN_ROLES_UIA_MUST_NOT_REACH: &[&str] = &[role_names::EXPLORER, role_names::RESEARCHER_WEB];

/// Regression: `explorer` and `researcher-web` resolve to organizational
/// role `Worker`, which `harw-agent-dsl/src/roles.rs::can_spawn` never lets
/// a `UserInterface` caller spawn. Handing a UIA caller these role names
/// (the pre-fix behavior of `harw-ops/src/{explore,research}.rs`) is exactly
/// the design conflict this test suite now guards against regressing.
#[test]
fn test_plain_explore_and_research_roles_are_worker_and_denied_to_uia() -> TestResult {
    let roles = resolved_roles()?;

    for role_name in PLAIN_ROLES_UIA_MUST_NOT_REACH {
        let ir = roles.get(*role_name).ok_or(TestError::Unexpected(format!(
            "builtin role '{role_name}' must be registered"
        )))?;
        assert_eq!(
            ir.role(),
            AgentRoleId::Worker,
            "role '{role_name}' must resolve to organizational role Worker"
        );
        assert!(
            !can_spawn(AgentRoleId::UserInterface, ir.role()),
            "a UserInterface caller must never be able to spawn '{role_name}'"
        );
    }
    Ok(())
}

/// The fix: `uia-explorer` and `uia-writer` resolve to organizational role
/// `UiaWorker` (Addendum J), which `can_spawn(UserInterface, UiaWorker)`
/// already permits at the matrix level
/// (`harw-agent-dsl/src/roles.rs::test_can_spawn_uia_to_uia_worker_ok`).
/// These are the exact redirect targets `/explore` and `/research-web` must
/// use for a UIA caller instead of `role_names::EXPLORER` /
/// `role_names::RESEARCHER_WEB`.
///
/// This test needs three things from the sibling fix to compile and pass:
/// `role_names::UIA_EXPLORER` / `role_names::UIA_WRITER` declared, both
/// names included in `role_names::ALL` (`builtin_agent_definitions` only
/// resolves names present there — see
/// `harw-registry-defaults/src/embedded_agents.rs`, the `targets.push`
/// filter), and an embedded TOML definition backing each name with
/// `role = "uia-worker"`. Until all three land, this test does not compile
/// — intentionally, per the task's instruction to write it against the
/// contractual names rather than skip it.
#[test]
fn test_uia_explorer_and_uia_writer_are_uia_worker_and_allowed_for_uia() -> TestResult {
    let roles = resolved_roles()?;

    // Runde 4, Teil E: `uia-latex-writer` ist dieselbe Organisationsrolle
    // `UiaWorker` und damit ohne Freigabeliste für die UIA spawnbar.
    for role_name in [
        role_names::UIA_EXPLORER,
        role_names::UIA_WRITER,
        role_names::UIA_LATEX_WRITER,
    ] {
        let ir = roles.get(role_name).ok_or(TestError::Unexpected(format!(
            "builtin role '{role_name}' must be registered"
        )))?;
        assert_eq!(
            ir.role(),
            AgentRoleId::UiaWorker,
            "role '{role_name}' must resolve to organizational role UiaWorker"
        );
        assert!(
            can_spawn(AgentRoleId::UserInterface, ir.role()),
            "a UserInterface caller must be able to spawn '{role_name}'"
        );
    }
    Ok(())
}

/// Die lesenden Datei-Werkzeuge, die eine UIA-freigegebene Matrix-Rolle
/// höchstens registrieren darf: die fünf lesenden `fs.*` plus
/// `doc.read_pdf` — kein Netz, kein Schreiben, keine Ausführung.
const READ_ONLY_FILE_TOOLS: &[&str] = &[
    "fs.read",
    "fs.list",
    "fs.search",
    "fs.glob",
    "fs.grep",
    "doc.read_pdf",
];

/// Sicherheitsvoraussetzung der UIA-Freigabeliste
/// (`harw-core::child_controller::ManagedAgentSpawner::with_uia_spawnable_roles`):
/// Die Runtime-Montage übergibt dort genau `role_names::MATRIX_ROLES`. Die
/// Liste hebelt `can_spawn(UserInterface, Worker) == false` für diese Namen
/// aus — vertretbar nur, weil jede dieser Rollen **nur lesend** ist, ohne
/// **Netz**, **Schreiben** oder **Exec**. Dieser Test hält genau das fest:
/// jede Matrix-Rolle bildet auf `RegistryProfile::MatrixReader` und
/// `AuthorityReducer::ReadOnly` ab, die registrierten Werkzeuge sind eine
/// Teilmenge der lesenden Datei-Werkzeuge (kein `web.*`, `fs.write`,
/// `shell.*`, `process.*`, `browser.*`), weder Profil noch Reducer-Decke
/// enthalten `NetworkAccess`, `WriteWorkspace` oder `ExecuteProcess`, und die
/// Rolle ist ein `Worker`, den die Spawn-Matrix allein einer UIA-Wurzel
/// weiterhin verweigert (die Ausnahme also wirklich nur über die explizite
/// Liste entsteht).
#[test]
fn test_matrix_roles_are_read_only_worker_roles_fit_for_the_uia_allowlist() -> TestResult {
    let roles = resolved_roles()?;

    for role_name in role_names::MATRIX_ROLES {
        assert_eq!(
            profile_for_role(role_name),
            Some(RegistryProfile::MatrixReader),
            "UIA-spawnable role '{role_name}' must map to RegistryProfile::MatrixReader"
        );
        for tool in RegistryProfile::MatrixReader.registered_tool_names() {
            assert!(
                READ_ONLY_FILE_TOOLS.contains(&tool),
                "RegistryProfile::MatrixReader registers '{tool}', which is not a read-only file tool"
            );
            assert!(
                tool != "fs.write"
                    && !tool.starts_with("web.")
                    && !tool.starts_with("shell.")
                    && !tool.starts_with("process.")
                    && !tool.starts_with("browser."),
                "RegistryProfile::MatrixReader must never register '{tool}'"
            );
        }
        let required = RegistryProfile::MatrixReader.required_permissions();
        assert_eq!(
            authority_reducer_for_role(role_name),
            Some(AuthorityReducer::ReadOnly),
            "UIA-spawnable role '{role_name}' must map to AuthorityReducer::ReadOnly"
        );
        let ceiling = AuthorityReducer::ReadOnly.ceiling();
        for forbidden in [
            Permission::NetworkAccess,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
        ] {
            assert!(
                !required.contains(forbidden),
                "RegistryProfile::MatrixReader must never need {forbidden:?}"
            );
            assert!(
                !ceiling.contains(forbidden),
                "AuthorityReducer::ReadOnly must never grant {forbidden:?}"
            );
        }

        let ir = roles.get(role_name).ok_or(TestError::Unexpected(format!(
            "builtin role '{role_name}' must be registered"
        )))?;
        assert_eq!(
            ir.role(),
            AgentRoleId::Worker,
            "role '{role_name}' must resolve to organizational role Worker"
        );
        assert!(
            !can_spawn(AgentRoleId::UserInterface, ir.role()),
            "only the explicit UIA allowlist may admit '{role_name}', never the spawn matrix"
        );
    }
    Ok(())
}
