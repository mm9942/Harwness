//! `ReachableTools`.

use harw_agent_dsl::{Diagnostic, Diagnostics};

use super::Pass;
use crate::builtins::builtin_defaults;
use crate::codes;
use crate::discovery::edit_distance;
use crate::unit::{CompileUnit, ProviderUse};

/// Collects the tool providers the manifest needs and reports tools no
/// provider serves.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReachableTools;

/// Recomputes `unit.providers` and `unit.features` from the effective
/// tools; returns the tools without a catalog row.
pub fn collect_providers(unit: &mut CompileUnit) -> Vec<String> {
    unit.providers.clear();
    unit.features.clear();
    let defaults = builtin_defaults();
    unit.features.insert(defaults.core_feature().to_owned());
    let mut unknown = Vec::new();
    for tool in unit.effective_tools() {
        match defaults.capability(&tool) {
            Some(entry) => {
                let provider = unit
                    .providers
                    .entry(entry.provider_id.to_owned())
                    .or_insert_with(|| ProviderUse {
                        id: entry.provider_id.to_owned(),
                        crate_name: entry.crate_name.to_owned(),
                        feature: entry.feature.to_owned(),
                        tools: Vec::new(),
                    });
                provider.tools.push(tool);
                unit.features.insert(entry.feature.to_owned());
            }
            None => unknown.push(tool),
        }
    }
    unknown
}

impl Pass for ReachableTools {
    fn name(&self) -> &'static str {
        "reachable-tools"
    }

    fn run(&self, unit: &mut CompileUnit) -> Diagnostics {
        let mut diagnostics = Diagnostics::new();
        for tool in collect_providers(unit) {
            let mut close: Vec<(usize, String)> = builtin_defaults()
                .catalog_labels()
                .into_iter()
                .map(|label| (edit_distance(&label, &tool), label))
                .filter(|(distance, _)| *distance <= 3)
                .collect();
            close.sort();
            let mut diagnostic = Diagnostic::new(
                &codes::UNKNOWN_TOOL,
                format!("no tool provider serves `{tool}`"),
            )
            .with_path("tools.admitted")
            .with_span(unit.span_of_item("tools.admitted", &tool));
            if !close.is_empty() {
                let names: Vec<String> = close.into_iter().take(3).map(|(_, name)| name).collect();
                diagnostic = diagnostic.with_help(format!(
                    "{} Did you mean: {}",
                    codes::UNKNOWN_TOOL.help,
                    names.join(", ")
                ));
            }
            diagnostics.push(diagnostic);
        }
        diagnostics
    }
}
