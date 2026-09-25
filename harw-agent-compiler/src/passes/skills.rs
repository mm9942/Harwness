//! `ResolveSkills`.

use harw_agent_artifact::ArtifactDigest;
use harw_agent_dsl::{Diagnostic, Diagnostics};
use harw_catalog::SkillIndex;

use super::Pass;
use crate::codes;
use crate::unit::CompileUnit;

/// Embeds every skill of the definition from the skill index and fills
/// `skills.entries[].hash` with the BLAKE3 (hex) of the embedded text.
#[derive(Debug, Clone, Copy)]
pub struct ResolveSkills<'a> {
    /// The skill index (layers plus bundled skills).
    pub index: &'a SkillIndex,
}

/// Logical path of a skill: `skills/<name>/instructions.md`.
#[must_use]
pub fn skill_payload_path(name: &str) -> String {
    format!("skills/{name}/instructions.md")
}

impl Pass for ResolveSkills<'_> {
    fn name(&self) -> &'static str {
        "resolve-skills"
    }

    fn run(&self, unit: &mut CompileUnit) -> Diagnostics {
        let mut diagnostics = Diagnostics::new();
        let names = unit.ir.skill_names();
        for (position, name) in names.iter().enumerate() {
            match self.index.get(name) {
                Some(entry) => {
                    let bytes = entry.snapshot().instructions.as_bytes().to_vec();
                    let hash = ArtifactDigest::of(&bytes).to_hex();
                    unit.put_file("skill", skill_payload_path(name), bytes);
                    if let Some(slot) = unit.ir.skills.entries.get_mut(position) {
                        slot.hash = Some(hash);
                    }
                }
                None => {
                    let (code, message) = if self.index.is_disabled(name) {
                        (
                            &codes::SKILL_DISABLED,
                            format!("skill `{name}` is disabled in a layer"),
                        )
                    } else {
                        (
                            &codes::SKILL_NOT_FOUND,
                            format!("skill `{name}` is in no skill layer and not bundled"),
                        )
                    };
                    let mut diagnostic = Diagnostic::new(code, message)
                        .with_path("skills")
                        .with_span(unit.span_of_item("skills", name));
                    let close = self.index.closest_names(name, 3);
                    if !close.is_empty() {
                        diagnostic = diagnostic.with_help(format!(
                            "{} Similar skills: {}",
                            code.help,
                            close.join(", ")
                        ));
                    }
                    diagnostics.push(diagnostic);
                }
            }
        }
        diagnostics
    }
}
