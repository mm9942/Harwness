//! `ResolveSkills`.

use harw_agent_artifact::ArtifactDigest;
use harw_agent_dsl::{Diagnostic, Diagnostics};
use harw_catalog::{SkillIndex, SkillRuntimeSnapshot};

use super::Pass;
use crate::codes;
use crate::unit::CompileUnit;

/// Embeds every skill of the definition from the skill index: the
/// instructions body at `skills/<name>/instructions.md` (see
/// [`skill_payload_path`]) and its manifest at `skills/<name>/skill.toml`
/// (see [`skill_manifest_path`]) so `harw_catalog::SkillIndex::build_with_bundle`
/// recognizes the skill at runtime, and fills `skills.entries[].hash` with
/// the BLAKE3 (hex) of the embedded instructions bytes.
#[derive(Debug, Clone, Copy)]
pub struct ResolveSkills<'a> {
    /// The skill index (layers plus bundled skills).
    pub index: &'a SkillIndex,
}

/// Logical path of a skill's instructions body: `skills/<name>/instructions.md`.
#[must_use]
pub fn skill_payload_path(name: &str) -> String {
    format!("skills/{name}/instructions.md")
}

/// Logical path of a skill's manifest: `skills/<name>/skill.toml`.
///
/// The embedded runtime only recognizes a skill through this manifest file
/// next to its instructions body; without it a compiled agent with skills
/// fails at startup with "not configured" even though the instructions text
/// was embedded.
#[must_use]
pub fn skill_manifest_path(name: &str) -> String {
    format!("skills/{name}/skill.toml")
}

/// Builds the TOML text of a skill's runtime manifest from its snapshot.
///
/// Writes exactly `name` (the requested name, not `snapshot.name`),
/// `description`, `tools` and `mcps`. `enabled` and `instructions_file` are
/// left out so the parsed `SkillToml` defaults apply (`enabled = true`,
/// `instructions_file` -> `instructions.md`), and `source_path`/`sha256` are
/// never embedded so the artifact stays reproducible and host-independent.
fn skill_manifest(name: &str, snapshot: &SkillRuntimeSnapshot) -> Result<String, toml::ser::Error> {
    let mut table = toml::Table::new();
    table.insert("name".to_owned(), toml::Value::String(name.to_owned()));
    table.insert(
        "description".to_owned(),
        toml::Value::String(snapshot.description.to_owned()),
    );
    table.insert(
        "tools".to_owned(),
        toml::Value::Array(
            snapshot
                .tools
                .iter()
                .map(|tool| toml::Value::String(tool.to_owned()))
                .collect(),
        ),
    );
    table.insert(
        "mcps".to_owned(),
        toml::Value::Array(
            snapshot
                .mcps
                .iter()
                .map(|mcp| toml::Value::String(mcp.to_owned()))
                .collect(),
        ),
    );
    toml::to_string(&table)
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
                    let snapshot = entry.snapshot();
                    let bytes = snapshot.instructions.as_bytes().to_vec();
                    let hash = ArtifactDigest::of(&bytes).to_hex();
                    unit.put_file("skill", skill_payload_path(name), bytes);
                    match skill_manifest(name, snapshot) {
                        Ok(manifest) => {
                            unit.put_file(
                                "skill",
                                skill_manifest_path(name),
                                manifest.into_bytes(),
                            );
                        }
                        Err(error) => {
                            // No BUILD code models "the skill was found but its
                            // manifest could not be serialized": `SKILL_NOT_FOUND`
                            // would be wrong (the skill *was* found in a layer),
                            // and no other code is skill-shaped. `SKILL_DISABLED`
                            // is the closest fit among the existing codes: like a
                            // disabled skill, this skill is recognized but ends up
                            // never embedded. This branch is not expected to be
                            // reachable (`name`, `description`, `tools` and `mcps`
                            // are always plain strings), it only guards against a
                            // panic instead of unwrapping.
                            diagnostics.push(
                                Diagnostic::new(
                                    &codes::SKILL_DISABLED,
                                    format!(
                                        "skill `{name}` manifest could not be serialized: {error}"
                                    ),
                                )
                                .with_path("skills")
                                .with_span(unit.span_of_item("skills", name)),
                            );
                        }
                    }
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn sample_snapshot() -> SkillRuntimeSnapshot {
        SkillRuntimeSnapshot {
            name: "evidence-quality-review".to_owned(),
            description: "Reviews evidence quality.".to_owned(),
            instructions: "# Evidence quality review\n".to_owned(),
            tools: vec!["fs.read".to_owned(), "shell.exec".to_owned()],
            mcps: vec!["catalog".to_owned()],
            source_path: PathBuf::from("/skills/evidence-quality-review"),
            sha256: "deadbeef".to_owned(),
        }
    }

    #[test]
    fn test_skill_manifest_path_is_skill_toml_under_the_skill_directory() {
        assert_eq!(skill_manifest_path("x"), "skills/x/skill.toml");
    }

    #[test]
    fn test_skill_manifest_round_trips_expected_keys_only() {
        let snapshot = sample_snapshot();
        let text = skill_manifest("evidence-quality-review", &snapshot)
            .expect("a plain string/vec snapshot always serializes");
        let table: toml::Table = toml::from_str(&text).expect("manifest text must be valid TOML");

        assert_eq!(
            table.get("name").and_then(toml::Value::as_str),
            Some("evidence-quality-review")
        );
        assert_eq!(
            table.get("description").and_then(toml::Value::as_str),
            Some("Reviews evidence quality.")
        );
        let tools: Vec<&str> = table
            .get("tools")
            .and_then(toml::Value::as_array)
            .expect("tools must be an array")
            .iter()
            .filter_map(toml::Value::as_str)
            .collect();
        assert_eq!(tools, vec!["fs.read", "shell.exec"]);
        let mcps: Vec<&str> = table
            .get("mcps")
            .and_then(toml::Value::as_array)
            .expect("mcps must be an array")
            .iter()
            .filter_map(toml::Value::as_str)
            .collect();
        assert_eq!(mcps, vec!["catalog"]);

        assert!(
            table.get("enabled").is_none(),
            "enabled must default, not be embedded"
        );
        assert!(
            table.get("instructions_file").is_none(),
            "instructions_file must default, not be embedded"
        );
        assert!(
            table.get("sha256").is_none(),
            "sha256 must never be embedded"
        );
        assert!(
            table.get("source_path").is_none(),
            "source_path must never be embedded"
        );
    }
}
