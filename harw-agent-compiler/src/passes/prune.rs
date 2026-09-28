//! `PruneUnusedTools`.

use std::collections::BTreeSet;

use harw_agent_dsl::Diagnostics;
use harw_agent_dsl::classify::reclassify_permissions;
use harw_agent_dsl::roles::AgentRoleId;

use super::Pass;
use super::reachable::collect_providers;
use crate::builtins::builtin_defaults;
use crate::codes;
use crate::unit::CompileUnit;

/// Drops tools the compiled agent can never use and recomputes manifest and
/// providers.
///
/// # Description
/// - Duplicates in `tools.admitted` are merged.
/// - Spawn tools (`delegate_wave`, `agents.delegate`, `agent.*`) are dropped
///   from agents that cannot start anything: workers and orchestrators
///   without delegation targets or child orchestrators.
/// - A handoff `transfer_to_<x>` is dropped unless `<x>` is a delegation
///   target or child orchestrator (the root orchestrator keeps its handoffs:
///   its targets come from the built-in organization).
#[derive(Debug, Clone, Copy, Default)]
pub struct PruneUnusedTools;

impl Pass for PruneUnusedTools {
    fn name(&self) -> &'static str {
        "prune-unused-tools"
    }

    fn run(&self, unit: &mut CompileUnit) -> Diagnostics {
        let mut diagnostics = Diagnostics::new();
        let defaults = builtin_defaults();
        let children: BTreeSet<String> = unit
            .child_names()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let role = unit.ir.role;
        let is_root = role == AgentRoleId::RootOrchestrator;
        let can_spawn = is_root || (role == AgentRoleId::ChildOrchestrator && !children.is_empty());
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut kept: Vec<String> = Vec::new();
        let mut pruned: Vec<(String, String)> = Vec::new();
        for tool in &unit.ir.tools.admitted {
            if !seen.insert(tool.clone()) {
                continue;
            }
            let reason = if defaults.spawn_tools().contains(&tool.as_str()) && !can_spawn {
                Some("the agent can start no child agent".to_owned())
            } else if !is_root
                && defaults.is_handoff_tool(tool)
                && tool
                    .strip_prefix("transfer_to_")
                    .is_some_and(|target| !children.contains(target))
            {
                Some("its target is no delegation target of this agent".to_owned())
            } else {
                None
            };
            match reason {
                Some(reason) => pruned.push((tool.clone(), reason)),
                None => kept.push(tool.clone()),
            }
        }
        for (tool, reason) in &pruned {
            let span = unit.span_of_item("tools.admitted", tool);
            diagnostics.push(
                harw_agent_dsl::Diagnostic::new(
                    &codes::TOOL_PRUNED,
                    format!("`{tool}` is pruned: {reason}"),
                )
                .with_path("tools.admitted")
                .with_span(span),
            );
        }
        unit.ir.tools.admitted = kept;
        unit.pruned.extend(pruned);
        let _unknown = reclassify_permissions(&mut unit.ir, defaults.tool_classifier());
        let _unknown = collect_providers(unit);
        diagnostics
    }
}
