//! `ValidateRoles`.

use harw_agent_dsl::Diagnostics;
use harw_agent_dsl::roles::AgentRoleId;

use super::Pass;
use crate::codes;
use crate::unit::CompileUnit;

/// Checks that the role can be compiled and that the spawn declarations fit
/// the role.
#[derive(Debug, Clone, Copy, Default)]
pub struct ValidateRoles;

/// Roles a standalone binary can run as.
pub const COMPILABLE_ROLES: &[AgentRoleId] = &[
    AgentRoleId::Worker,
    AgentRoleId::ChildOrchestrator,
    AgentRoleId::RootOrchestrator,
    AgentRoleId::UiaWorker,
    AgentRoleId::UserInterface,
];

impl Pass for ValidateRoles {
    fn name(&self) -> &'static str {
        "validate-roles"
    }

    fn run(&self, unit: &mut CompileUnit) -> Diagnostics {
        let mut diagnostics = Diagnostics::new();
        let role = unit.ir.role;
        if !COMPILABLE_ROLES.contains(&role) {
            diagnostics.push(unit.diagnostic(
                &codes::ROLE_NOT_COMPILABLE,
                "role",
                format!(
                    "`{}` has role `{}`, which cannot run as a standalone agent",
                    unit.name,
                    role_label(role)
                ),
            ));
        }
        let has_targets = unit
            .ir
            .spawn
            .delegation_targets
            .as_ref()
            .is_some_and(|targets| !targets.is_empty());
        if role == AgentRoleId::Worker && has_targets {
            diagnostics.push(unit.diagnostic(
                &codes::WORKER_DELEGATES,
                "delegation.targets",
                format!(
                    "`{}` is a worker; its `[delegation] targets` are ignored",
                    unit.name
                ),
            ));
        }
        let orchestrator = matches!(
            role,
            AgentRoleId::ChildOrchestrator | AgentRoleId::RootOrchestrator
        );
        if orchestrator && unit.child_names().is_empty() {
            diagnostics.push(unit.diagnostic(
                &codes::DEPTH_WITHOUT_CHILDREN,
                "role",
                format!(
                    "`{}` is an orchestrator without delegation targets; the compiled agent can start no child",
                    unit.name
                ),
            ));
        }
        diagnostics
    }
}

/// The kebab-case label of a role (`child-orchestrator`).
#[must_use]
pub fn role_label(role: AgentRoleId) -> &'static str {
    match role {
        AgentRoleId::UserInterface => "user-interface",
        AgentRoleId::RootOrchestrator => "root-orchestrator",
        AgentRoleId::ChildOrchestrator => "child-orchestrator",
        AgentRoleId::Worker => "worker",
        AgentRoleId::UiaWorker => "uia-worker",
        AgentRoleId::AgentSteward => "agent-steward",
    }
}
