//! Rights algebra of the compiler: what a manifest claims, what its base
//! role allows, and the author ceiling.
//!
//! This mirrors `claimed_rights_of`/`base_role_rights_of` in
//! `harw_registry_defaults::agent_definition_tools` and the base-role
//! selection of `harw_registry_defaults::roster` (`ceiling_for`), over
//! [`AgentIr`] instead of the legacy view, with one deliberate difference:
//! a base role that makes *no statement* about depth or budget (`None`)
//! bounds nothing there (the runtime default applies), where the steward's
//! proposal check treats it as zero to force a human review.

use std::collections::{BTreeMap, BTreeSet};

use harw_agent_dsl::ir_v2::{AgentIr, Effort};
use harw_agent_dsl::roles::AgentRoleId;
use serde::Serialize;

use crate::builtins::{RoleDefaults, builtin_defaults, require_builtin_defaults};
use crate::error::CompileError;

/// A set of rights in one comparable shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RightsSet {
    /// Admitted tools.
    pub tools: BTreeSet<String>,
    /// Capability classes of the tools (catalog labels).
    pub classes: BTreeSet<String>,
    /// Maximum spawn depth; `None`: no statement.
    pub max_depth: Option<u32>,
    /// Token budget; `None`: no statement.
    pub budget_tokens: Option<u64>,
    /// Effort cap; `None`: no statement.
    pub effort_cap: Option<Effort>,
}

impl RightsSet {
    /// The rights an IR claims: its effective tools (admitted minus
    /// forbidden), their catalog classes, `spawn.max_depth`,
    /// `spawn.budget.max_tokens` and `spawn.budget.effort_cap`.
    #[must_use]
    pub fn claimed_by(ir: &AgentIr) -> Self {
        let tools: BTreeSet<String> = ir
            .tools
            .admitted
            .iter()
            .filter(|tool| !ir.tools.forbidden.contains(tool))
            .cloned()
            .collect();
        let budget = ir.spawn.budget.as_ref();
        Self {
            classes: classes_of(&tools),
            tools,
            max_depth: ir.spawn.max_depth,
            budget_tokens: budget.and_then(|budget| budget.max_tokens),
            effort_cap: budget.and_then(|budget| budget.effort_cap),
        }
    }

    /// An author ceiling from explicit tools (classes derived), depth,
    /// budget and effort.
    #[must_use]
    pub fn ceiling(
        tools: impl IntoIterator<Item = String>,
        max_depth: Option<u32>,
        budget_tokens: Option<u64>,
        effort_cap: Option<Effort>,
    ) -> Self {
        let tools: BTreeSet<String> = tools.into_iter().collect();
        Self {
            classes: classes_of(&tools),
            tools,
            max_depth,
            budget_tokens,
            effort_cap,
        }
    }
}

/// Catalog class labels of `tools` (unknown tools contribute nothing).
#[must_use]
pub fn classes_of(tools: &BTreeSet<String>) -> BTreeSet<String> {
    let defaults = builtin_defaults();
    tools
        .iter()
        .filter_map(|tool| defaults.capability(tool))
        .map(|capability| capability.class.to_owned())
        .collect()
}

/// `true` if any tool of the set writes, runs processes or acts on the host
/// (the catalog's writing classes).
#[must_use]
pub fn is_writing(rights: &RightsSet) -> bool {
    builtin_defaults()
        .writing_classes()
        .iter()
        .any(|class| rights.classes.contains(*class))
}

/// What a claim adds over a limit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RightsDelta {
    /// Tools the limit does not admit.
    pub added_tools: Vec<String>,
    /// Classes the limit does not hold.
    pub added_classes: Vec<String>,
    /// Depth above the limit: `(claimed, limit)`.
    pub depth: Option<(u32, u32)>,
    /// Budget above the limit: `(claimed, limit)`.
    pub budget: Option<(u64, u64)>,
    /// Effort above the limit: `(claimed, limit)`.
    pub effort: Option<(Effort, Effort)>,
}

impl RightsDelta {
    /// `true` without any widening.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added_tools.is_empty()
            && self.added_classes.is_empty()
            && self.depth.is_none()
            && self.budget.is_none()
            && self.effort.is_none()
    }

    /// One line per widening (`tools: shell.exec`, `depth: 3 > 1`, …).
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if !self.added_tools.is_empty() {
            lines.push(format!("tools: {}", self.added_tools.join(", ")));
        }
        if !self.added_classes.is_empty() {
            lines.push(format!("classes: {}", self.added_classes.join(", ")));
        }
        if let Some((claimed, limit)) = self.depth {
            lines.push(format!("max_depth: {claimed} > {limit}"));
        }
        if let Some((claimed, limit)) = self.budget {
            lines.push(format!("max_tokens: {claimed} > {limit}"));
        }
        if let Some((claimed, limit)) = self.effort {
            lines.push(format!(
                "effort_cap: {} > {}",
                claimed.as_str(),
                limit.as_str()
            ));
        }
        lines
    }
}

fn effort_rank(effort: Effort) -> usize {
    Effort::ALL
        .iter()
        .position(|candidate| *candidate == effort)
        .unwrap_or(Effort::ALL.len())
}

/// Compares `claimed` against `limit`.
///
/// # Description
/// Tools: set difference, except handoff tools (`transfer_to_*`) whose
/// target is in `handoff_targets` (delegation grants are checked by the
/// delegation rules, not by the tool ceiling). Classes: set difference.
/// Depth, budget and effort: only where the limit makes a statement.
#[must_use]
pub fn delta(
    claimed: &RightsSet,
    limit: &RightsSet,
    handoff_targets: &BTreeSet<String>,
) -> RightsDelta {
    let added_tools: Vec<String> = claimed
        .tools
        .difference(&limit.tools)
        .filter(|tool| {
            tool.strip_prefix("transfer_to_")
                .is_none_or(|target| !handoff_targets.contains(target))
        })
        .cloned()
        .collect();
    let added_classes: Vec<String> = claimed
        .classes
        .difference(&limit.classes)
        .filter(|class| {
            // A class that only enters through exempted handoff tools is
            // not a widening either.
            claimed.tools.iter().any(|tool| {
                added_tools.contains(tool)
                    && builtin_defaults()
                        .capability(tool)
                        .is_some_and(|capability| capability.class == class.as_str())
            })
        })
        .cloned()
        .collect();
    let depth = match (claimed.max_depth, limit.max_depth) {
        (Some(claimed), Some(limit)) if claimed > limit => Some((claimed, limit)),
        _ => None,
    };
    let budget = match (claimed.budget_tokens, limit.budget_tokens) {
        (Some(claimed), Some(limit)) if claimed > limit => Some((claimed, limit)),
        _ => None,
    };
    let effort = match (claimed.effort_cap, limit.effort_cap) {
        (Some(claimed), Some(limit)) if effort_rank(claimed) > effort_rank(limit) => {
            Some((claimed, limit))
        }
        _ => None,
    };
    RightsDelta {
        added_tools,
        added_classes,
        depth,
        budget,
        effort,
    }
}

/// The built-in role a definition is bounded by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BaseCeiling {
    /// The built-in role name whose rights bound the definition.
    pub base_role: String,
    /// Why this role: `self` (the definition is that role), `extends`
    /// (a built-in role in its `extends` chain) or `generic` (only a base
    /// layer, or none, in the chain).
    pub reason: String,
    /// The ceiling.
    pub rights: RightsSet,
}

/// The lowered built-in roles and bases, loaded once per compiler.
#[derive(Debug, Clone)]
pub struct BuiltinCeilings {
    roles: BTreeMap<String, AgentIr>,
    bases: BTreeMap<String, AgentIr>,
    names: RoleDefaults,
    catalog_tools: Vec<&'static str>,
}

impl BuiltinCeilings {
    /// Lowers the built-in roles and bases.
    ///
    /// # Errors
    /// [`CompileError::Other`] if a built-in definition is broken (a build
    /// defect) or no [`crate::builtins::BuiltinDefaults`] are installed.
    pub fn load(now: time::OffsetDateTime) -> Result<Self, CompileError> {
        let defaults = require_builtin_defaults()?;
        let roles = defaults
            .agent_irs(now)
            .map_err(|error| CompileError::Other(format!("built-in roles: {error}")))?;
        let bases = defaults
            .base_irs(now)
            .map_err(|error| CompileError::Other(format!("built-in bases: {error}")))?;
        Ok(Self {
            roles: roles.into_iter().collect(),
            bases: bases.into_iter().collect(),
            names: defaults.roles(),
            catalog_tools: defaults.catalog_tools(),
        })
    }

    /// The lowered built-in role `name`.
    #[must_use]
    pub fn role(&self, name: &str) -> Option<&AgentIr> {
        self.roles.get(name)
    }

    /// Built-in role names, sorted.
    pub fn role_names(&self) -> impl Iterator<Item = &str> {
        self.roles.keys().map(String::as_str)
    }

    fn role_by_id(&self, id: &str) -> Option<(&str, &AgentIr)> {
        self.roles
            .iter()
            .find(|(_, ir)| ir.id.to_string() == id)
            .map(|(name, ir)| (name.as_str(), ir))
    }

    fn base_by_id(&self, id: &str) -> Option<&str> {
        self.bases
            .iter()
            .find(|(_, ir)| ir.id.to_string() == id)
            .map(|(name, _)| name.as_str())
    }

    /// The base-role ceiling of `ir` (see `roster::ceiling_for`).
    ///
    /// # Description
    /// 1. `ir` *is* a built-in role (same ID): its own lowered rights.
    /// 2. The nearest `extends` step that names a built-in role of the same
    ///    organizational role: that role's rights.
    /// 3. Otherwise the generic ceiling of the role: a worker is bounded by
    ///    `analyst` (plus `fs.write`/`fs.edit` if it admits one of them,
    ///    depth at most 1), a UIA worker by `uia-explorer`, a child
    ///    orchestrator by `child-orchestrator-base`.
    ///
    /// `None` for roles without a generic ceiling (root orchestrator
    /// without a built-in ancestor, UI agent, steward).
    #[must_use]
    pub fn ceiling_for(&self, ir: &AgentIr) -> Option<BaseCeiling> {
        let id = ir.id.to_string();
        if let Some((name, own)) = self.role_by_id(&id) {
            return Some(BaseCeiling {
                base_role: name.to_owned(),
                reason: "self".to_owned(),
                rights: RightsSet::claimed_by(own),
            });
        }
        let ancestor = ir
            .trace
            .steps
            .iter()
            .rev()
            .filter(|step| step.kind == "base" && step.source != id)
            .find_map(|step| {
                self.role_by_id(&step.source)
                    .map(|(name, role_ir)| Ancestor::Role(name, role_ir))
                    .or_else(|| self.base_by_id(&step.source).map(Ancestor::Base))
            });
        match ancestor {
            Some(Ancestor::Role(name, role_ir)) if role_ir.role == ir.role => Some(BaseCeiling {
                base_role: name.to_owned(),
                reason: "extends".to_owned(),
                rights: RightsSet::claimed_by(role_ir),
            }),
            Some(Ancestor::Role(..)) => None,
            Some(Ancestor::Base(base)) => {
                let child_orchestrator_base = base == self.names.child_orchestrator_base_name
                    && ir.role == AgentRoleId::ChildOrchestrator;
                if child_orchestrator_base || base == self.names.worker_base_name {
                    self.generic(ir)
                } else {
                    None
                }
            }
            None => self.generic(ir),
        }
    }

    fn generic(&self, ir: &AgentIr) -> Option<BaseCeiling> {
        let RoleDefaults {
            child_orchestrator_base_name,
            generic_worker_base,
            generic_worker_max_depth,
            generic_child_orchestrator_base,
            generic_uia_worker_base,
            generic_worker_write_tools,
            ..
        } = self.names;
        match ir.role {
            AgentRoleId::Worker => {
                let base = self.roles.get(generic_worker_base)?;
                let mut rights = RightsSet::claimed_by(base);
                let writes = ir
                    .tools
                    .admitted
                    .iter()
                    .any(|tool| generic_worker_write_tools.contains(&tool.as_str()));
                if writes {
                    rights.tools.extend(
                        generic_worker_write_tools
                            .iter()
                            .map(|tool| (*tool).to_owned()),
                    );
                    rights.classes = classes_of(&rights.tools);
                }
                rights.max_depth =
                    Some(rights.max_depth.map_or(generic_worker_max_depth, |depth| {
                        depth.min(generic_worker_max_depth)
                    }));
                Some(BaseCeiling {
                    base_role: generic_worker_base.to_owned(),
                    reason: if writes {
                        "generic writing worker".to_owned()
                    } else {
                        "generic worker".to_owned()
                    },
                    rights,
                })
            }
            AgentRoleId::UiaWorker => {
                let base = self.roles.get(generic_uia_worker_base)?;
                Some(BaseCeiling {
                    base_role: generic_uia_worker_base.to_owned(),
                    reason: "generic UIA worker".to_owned(),
                    rights: RightsSet::claimed_by(base),
                })
            }
            AgentRoleId::ChildOrchestrator => {
                let base = self.bases.get(child_orchestrator_base_name)?;
                Some(BaseCeiling {
                    base_role: generic_child_orchestrator_base.to_owned(),
                    reason: "generic child orchestrator".to_owned(),
                    rights: RightsSet::claimed_by(base),
                })
            }
            AgentRoleId::UserInterface => Some(BaseCeiling {
                base_role: USER_INTERFACE_BASE.to_owned(),
                reason: "the user's own agent: every catalog tool, runtime flags narrow".to_owned(),
                rights: RightsSet::ceiling(
                    self.catalog_tools.iter().map(|tool| (*tool).to_owned()),
                    None,
                    None,
                    None,
                ),
            }),
            _ => None,
        }
    }
}

/// Base-role label of a user-interface agent (the UIA has no built-in role
/// row; it runs with the user's rights, narrowed at runtime).
pub const USER_INTERFACE_BASE: &str = "user-interface";

enum Ancestor<'a> {
    Role(&'a str, &'a AgentIr),
    Base(&'a str),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(tools: &[&str], depth: Option<u32>, budget: Option<u64>) -> RightsSet {
        RightsSet::ceiling(
            tools.iter().map(|tool| (*tool).to_owned()),
            depth,
            budget,
            None,
        )
    }

    #[test]
    fn test_no_statement_in_the_limit_bounds_nothing() {
        let claimed = set(&["fs.read"], Some(5), Some(1_000_000));
        let limit = set(&["fs.read"], None, None);
        assert!(delta(&claimed, &limit, &BTreeSet::new()).is_empty());
    }
}
