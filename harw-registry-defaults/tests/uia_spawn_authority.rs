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
use harw_registry_defaults::embedded_agents::builtin_agent_definitions;
use harw_registry_defaults::profile::role_names;

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

    for role_name in [role_names::UIA_EXPLORER, role_names::UIA_WRITER] {
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
