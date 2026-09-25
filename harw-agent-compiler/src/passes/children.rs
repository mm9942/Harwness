//! `ChildClosure`.

use std::collections::BTreeSet;

use harw_agent_artifact::AgentInput;
use harw_agent_dsl::ir_v2::AgentIr;
use harw_agent_dsl::roles::can_spawn;
use harw_agent_dsl::{Diagnostic, Diagnostics};

use super::Pass;
use crate::codes;
use crate::rights::{RightsSet, is_writing};
use crate::unit::{CompileUnit, CompiledChild};

/// A compiled child as the resolver returns it.
#[derive(Debug, Clone)]
pub struct ResolvedChild {
    /// The child's final IR.
    pub ir: AgentIr,
    /// The child's own agent entry first, then every entry below it.
    pub entries: Vec<AgentInput>,
    /// The child's v7 snapshot (hex).
    pub snapshot: String,
    /// The child's own (transitive) children.
    pub children: Vec<CompiledChild>,
}

/// Compiles a child agent by name (implemented by the driver).
pub trait ChildResolver {
    /// Compiles `name` as a child at `depth` below the compiled root.
    /// `stack` holds the names from the root to the parent (cycle guard).
    ///
    /// # Errors
    /// `Err(message)` if the name resolves to nothing, or the diagnostics of
    /// a child that does not compile, rendered as text.
    fn compile_child(&self, name: &str, depth: u32, stack: &[String]) -> Result<ResolvedChild, String>;
}

/// Resolves every delegation target and child orchestrator, transitively,
/// adds each one's agent entry to the unit (the artifact stores every agent
/// as an entry in one bundle, with a shared payload pool) and checks
/// child ≤ parent.
///
/// # Description
/// "Child ≤ parent" covers what a parent passes down (DSL delegation
/// capabilities): the role matrix (`can_spawn`), the remaining depth (a
/// child's `max_depth` stays below the parent's), the token budget, the
/// effort cap and the authority capabilities. Tool rights are *not*
/// compared with the parent's own tools: an orchestrator deliberately holds
/// fewer tools than the workers it starts. Each child's tools are bounded by
/// its own base role (its own `RightsCheck`), and at runtime the runner
/// grants min(child manifest, parent's live rights).
pub struct ChildClosure<'a> {
    /// The resolver (the driver).
    pub resolver: &'a dyn ChildResolver,
    /// Names from the compiled root down to this unit's parent.
    pub stack: Vec<String>,
}

impl Pass for ChildClosure<'_> {
    fn name(&self) -> &'static str {
        "child-closure"
    }

    fn run(&self, unit: &mut CompileUnit) -> Diagnostics {
        let mut diagnostics = Diagnostics::new();
        let mut stack = self.stack.clone();
        stack.push(unit.name.clone());
        let parent_rights = RightsSet::claimed_by(&unit.ir);
        let mut embedded: BTreeSet<String> = BTreeSet::new();
        for (name, via) in unit.child_names() {
            let path = if via == "delegation" {
                "delegation.targets"
            } else {
                "spawn.child_orchestrators"
            };
            if stack.contains(&name) || !embedded.insert(name.clone()) {
                diagnostics.push(unit.diagnostic(
                    &codes::CHILD_REPEATED,
                    path,
                    format!("`{name}` is already embedded higher up in the delegation graph"),
                ));
                continue;
            }
            let child = match self.resolver.compile_child(&name, unit.depth + 1, &stack) {
                Ok(child) => child,
                Err(reason) => {
                    diagnostics.push(
                        Diagnostic::new(
                            &codes::UNKNOWN_CHILD,
                            format!("`{}` cannot embed `{name}`: {reason}", unit.name),
                        )
                        .with_path(path)
                        .with_span(unit.span_of_item(path, &name)),
                    );
                    continue;
                }
            };
            for problem in child_problems(&unit.ir, &parent_rights, &child.ir) {
                diagnostics.push(
                    Diagnostic::new(
                        &codes::CHILD_EXCEEDS_PARENT,
                        format!("child `{name}`: {problem}"),
                    )
                    .with_path(path)
                    .with_span(unit.span_of_item(path, &name)),
                );
            }
            let child_rights = RightsSet::claimed_by(&child.ir);
            for entry in child.entries {
                if !unit.agents.iter().any(|known| known.id == entry.id) {
                    unit.agents.push(entry);
                }
            }
            unit.children.push(CompiledChild {
                name: name.clone(),
                id: child.ir.id.to_string(),
                role: child.ir.role,
                parent: unit.name.clone(),
                via: via.to_owned(),
                depth: unit.depth + 1,
                read_only: !is_writing(&child_rights),
                snapshot: child.snapshot,
            });
            unit.children.extend(child.children);
        }
        diagnostics
    }
}

/// What a child claims beyond what the parent can pass down.
#[must_use]
pub fn child_problems(parent: &AgentIr, parent_rights: &RightsSet, child: &AgentIr) -> Vec<String> {
    let mut problems = Vec::new();
    if !can_spawn(parent.role, child.role) {
        problems.push(format!(
            "role {:?} may not start role {:?}",
            parent.role, child.role
        ));
    }
    let child_rights = RightsSet::claimed_by(child);
    let depth = parent_rights
        .max_depth
        .zip(child_rights.max_depth)
        .filter(|(parent_depth, child_depth)| child_depth >= parent_depth);
    if let Some((parent_depth, child_depth)) = depth {
        problems.push(format!(
            "max_depth {child_depth} does not fit below the parent's remaining depth {parent_depth}"
        ));
    }
    let tokens = parent_rights
        .budget_tokens
        .zip(child_rights.budget_tokens)
        .filter(|(parent_tokens, child_tokens)| child_tokens > parent_tokens);
    if let Some((parent_tokens, child_tokens)) = tokens {
        problems.push(format!(
            "max_tokens {child_tokens} exceeds the parent's {parent_tokens}"
        ));
    }
    let rank = |effort: harw_agent_dsl::ir_v2::Effort| {
        harw_agent_dsl::ir_v2::Effort::ALL
            .iter()
            .position(|candidate| *candidate == effort)
    };
    let effort = parent_rights
        .effort_cap
        .zip(child_rights.effort_cap)
        .filter(|(parent_effort, child_effort)| rank(*child_effort) > rank(*parent_effort));
    if let Some((parent_effort, child_effort)) = effort {
        problems.push(format!(
            "effort_cap {} exceeds the parent's {}",
            child_effort.as_str(),
            parent_effort.as_str()
        ));
    }
    let parent_caps: BTreeSet<&String> = parent.authority.capabilities.iter().collect();
    if !parent_caps.is_empty() {
        let extra: Vec<&str> = child
            .authority
            .capabilities
            .iter()
            .filter(|capability| !parent_caps.contains(capability))
            .map(String::as_str)
            .collect();
        if !extra.is_empty() {
            problems.push(format!(
                "authority capabilities the parent does not hold: {}",
                extra.join(", ")
            ));
        }
    }
    problems
}
