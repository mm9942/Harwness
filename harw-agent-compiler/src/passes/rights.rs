//! `RightsCheck`: manifest ≤ base role ≤ author ceiling.

use std::collections::BTreeSet;

use harw_agent_dsl::Diagnostics;
use harw_agent_dsl::classify::reclassify_permissions;
use harw_agent_dsl::diagnostics::DiagnosticCode;

use super::Pass;
use crate::builtins::builtin_defaults;
use crate::codes;
use crate::rights::{BuiltinCeilings, RightsDelta, RightsSet, delta};
use crate::unit::{CompileUnit, RightsFlow};

/// Re-classifies the permission manifest from the capability catalog and
/// checks it against the base role and the optional author ceiling.
#[derive(Debug, Clone, Copy)]
pub struct RightsCheck<'a> {
    /// The built-in roles and bases.
    pub ceilings: &'a BuiltinCeilings,
    /// The author ceiling of this build, if any.
    pub author: Option<&'a RightsSet>,
}

impl Pass for RightsCheck<'_> {
    fn name(&self) -> &'static str {
        "rights-check"
    }

    fn run(&self, unit: &mut CompileUnit) -> Diagnostics {
        let mut diagnostics = Diagnostics::new();
        // Unknown tools keep their label classification here;
        // `ReachableTools` reports them (HARW-BUILD-003, with suggestions).
        // They must also be left out of the widening checks below: a base
        // role's ceiling never lists a nonexistent tool either, so an
        // unrecognized (e.g. mistyped) tool name would otherwise look like a
        // widening and fail with HARW-BUILD-004 here — which stops the pass
        // pipeline before `ReachableTools` ever runs, hiding the real,
        // suggestion-bearing diagnostic behind a misleading one.
        let unknown: BTreeSet<String> =
            reclassify_permissions(&mut unit.ir, builtin_defaults().tool_classifier())
                .into_iter()
                .collect();
        let manifest = RightsSet::claimed_by(&unit.ir);
        let known = RightsSet {
            tools: manifest.tools.difference(&unknown).cloned().collect(),
            ..manifest.clone()
        };
        let handoffs: BTreeSet<String> = unit
            .child_names()
            .into_iter()
            .map(|(name, _)| name)
            .collect();

        let base = self.ceilings.ceiling_for(&unit.ir);
        let mut flow = RightsFlow {
            manifest: manifest.clone(),
            base: base.clone(),
            ceiling: self.author.cloned(),
            ..RightsFlow::default()
        };
        match &base {
            None => diagnostics.push(unit.diagnostic(
                &codes::NO_BASE_ROLE,
                "role",
                format!(
                    "`{}` extends no built-in role that bounds a {:?} definition",
                    unit.name, unit.ir.role
                ),
            )),
            Some(base) => {
                flow.over_base = delta(&known, &base.rights, &handoffs);
                flow.narrowed_tools = base
                    .rights
                    .tools
                    .difference(&manifest.tools)
                    .cloned()
                    .collect();
                let limit = format!("its base role `{}` ({})", base.base_role, base.reason);
                report(
                    &mut diagnostics,
                    unit,
                    &codes::WIDENS_BASE_ROLE,
                    &flow.over_base,
                    &limit,
                );
            }
        }
        if let Some(author) = self.author {
            flow.over_ceiling = delta(&known, author, &handoffs);
            report(
                &mut diagnostics,
                unit,
                &codes::EXCEEDS_AUTHOR_CEILING,
                &flow.over_ceiling,
                "the author ceiling",
            );
            if let Some(base) = &base {
                flow.base_over_ceiling = delta(&base.rights, author, &BTreeSet::new());
                if !flow.base_over_ceiling.is_empty() {
                    diagnostics.push(unit.diagnostic(
                        &codes::BASE_ABOVE_CEILING,
                        "extends",
                        format!(
                            "base role `{}` holds more than the author ceiling ({}); the difference is narrowed away",
                            base.base_role,
                            flow.base_over_ceiling.lines().join("; ")
                        ),
                    ));
                }
            }
        }
        unit.rights = Some(flow);
        diagnostics
    }
}

/// One diagnostic per widening, located at the offending value.
fn report(
    diagnostics: &mut Diagnostics,
    unit: &CompileUnit,
    code: &DiagnosticCode,
    delta: &RightsDelta,
    limit: &str,
) {
    for tool in &delta.added_tools {
        let span = unit.span_of_item("tools.admitted", tool);
        diagnostics.push(
            harw_agent_dsl::Diagnostic::new(
                code,
                format!("`{}` admits `{tool}`, which {limit} does not", unit.name),
            )
            .with_path("tools.admitted")
            .with_span(span),
        );
    }
    if let Some((claimed, bound)) = delta.depth {
        diagnostics.push(unit.diagnostic(
            code,
            "spawn.max_depth",
            format!(
                "`{}` asks for max_depth {claimed}; {limit} allows {bound}",
                unit.name
            ),
        ));
    }
    if let Some((claimed, bound)) = delta.budget {
        diagnostics.push(unit.diagnostic(
            code,
            "spawn.budget.max_tokens",
            format!(
                "`{}` asks for max_tokens {claimed}; {limit} allows {bound}",
                unit.name
            ),
        ));
    }
    if let Some((claimed, bound)) = delta.effort {
        diagnostics.push(unit.diagnostic(
            code,
            "spawn.budget.effort_cap",
            format!(
                "`{}` asks for effort_cap {}; {limit} allows {}",
                unit.name,
                claimed.as_str(),
                bound.as_str()
            ),
        ));
    }
}
