//! `harw agent new`: a commented `definition.toml` plus `system.md`.
//!
//! Scaffolding never builds anything; the definition is only a file until
//! someone runs `harw agent build` on it.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::CompileError;

/// Role of a new definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScaffoldRole {
    /// A worker (default).
    Worker,
    /// A child orchestrator.
    ChildOrchestrator,
}

impl ScaffoldRole {
    /// Parses `worker|child-orchestrator`.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "worker" => Some(Self::Worker),
            "child-orchestrator" => Some(Self::ChildOrchestrator),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::ChildOrchestrator => "child-orchestrator",
        }
    }

    fn default_base(self) -> &'static str {
        match self {
            Self::Worker => "harwness.agent.worker-base@1",
            Self::ChildOrchestrator => "harwness.agent.child-orchestrator-base@1",
        }
    }
}

/// What was scaffolded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Scaffolded {
    /// The definition directory.
    pub dir: PathBuf,
    /// Written files.
    pub files: Vec<PathBuf>,
    /// The new definition's ID.
    pub id: String,
}

/// `true` for `[a-z0-9][a-z0-9-]{0,63}`.
#[must_use]
pub fn valid_agent_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The commented `definition.toml` of a new agent.
#[must_use]
pub fn definition_template(name: &str, role: ScaffoldRole, extends: Option<&str>) -> String {
    let base = extends.unwrap_or(role.default_base());
    let body = match role {
        ScaffoldRole::Worker => WORKER_BODY.to_owned(),
        ScaffoldRole::ChildOrchestrator => ORCHESTRATOR_BODY.to_owned(),
    };
    format!(
        "# Agent definition `{name}` (agent-definition-dsl.md).\n\
         #\n\
         # Check it with `harw agent check {name}`, explain a value with\n\
         # `harw agent explain {name} <field>`. Nothing is built until you run\n\
         # `harw agent build {name}`.\n\
         \n\
         schema = \"harwness.agent/v1\"\n\
         id = \"user.agent.{name}@1\"\n\
         version = \"0.1.0\"\n\
         # The built-in definition whose rights bound this one: a definition can\n\
         # only narrow what it extends (`harw agent graph {name} --kind rights`).\n\
         extends = {{ id = \"{base}\" }}\n\
         role = \"{role}\"\n\
         specialization = \"{name}\"\n\
         name = \"{name}\"\n\
         description = \"TODO: one sentence on what {name} does.\"\n\
         # The working instructions, next to this file.\n\
         instructions_file = \"system.md\"\n\
         # Skills embedded at build time (`harw agent skills list`).\n\
         skills = []\n\
         {body}\
         \n\
         # Model preference and the credentials the binary needs at runtime\n\
         # (names only; never put a secret here).\n\
         # [models]\n\
         # provider = \"anthropic\"\n\
         # model = \"claude-sonnet-5\"\n\
         # required_env = [\"ANTHROPIC_API_KEY\"]\n\
         \n\
         # Settings of a compiled binary (`harw agent build {name}`).\n\
         [binary]\n\
         name = \"{name}\"\n\
         interfaces = [\"cli\"]\n\
         default_interface = \"cli\"\n",
        role = role.label(),
    )
}

/// The worker-specific tables (the worker base defines none of them).
const WORKER_BODY: &str = "
# The tools the agent may use. Everything else is unavailable.
[tools]
admitted = [
  \"fs.read\",
  \"fs.list\",
  \"fs.search\",
  \"fs.glob\",
  \"fs.grep\",
  \"parent.message\",
  \"skills.search\",
  \"skills.load\",
]
forbidden = [\"fs.write\", \"fs.edit\", \"shell.exec\"]

[spawn]
max_depth = 0

[spawn.budget]
max_tokens = 60000
max_tool_calls = 40
max_wall_secs = 600

[return]
contract = \"harwness.return.execution-summary@1\"
";

/// The orchestrator-specific tables. The orchestrator base already owns
/// `[tools]`, `[spawn]` and `[return]`; change them with `[patch.*]`, not by
/// redefining them (a redefined table is shadowed, HARW-RESOLVE-004).
const ORCHESTRATOR_BODY: &str = "
# Agents this orchestrator may start (by name). A compiled orchestrator
# embeds each of them.
[delegation]
targets = [\"explorer\"]

# The base owns [tools], [spawn] and [return]; narrow them with patches:
# [patch.tools.admitted]
# remove = [\"lens.ask\"]
";

/// The `system.md` of a new agent.
#[must_use]
pub fn instructions_template(name: &str, role: ScaffoldRole) -> String {
    let work = match role {
        ScaffoldRole::Worker => {
            "Du bearbeitest genau den Auftrag, den du bekommst, und lieferst eine knappe Zusammenfassung mit Fundstellen."
        }
        ScaffoldRole::ChildOrchestrator => {
            "Du zerlegst den Auftrag in unabhängige Teilaufträge, delegierst sie an deine Zielagenten und fasst ihre Ergebnisse zusammen."
        }
    };
    format!(
        "# {name}\n\n{work}\n\n## Vorgehen\n\n1. Auftrag klären.\n2. Arbeiten.\n3. Ergebnis mit Belegen zurückgeben.\n"
    )
}

/// Writes `definition.toml` and `system.md` into `dir`.
///
/// # Errors
/// [`CompileError::Other`] for an invalid name or an existing
/// `definition.toml`, [`CompileError::Io`] on a write failure.
pub fn scaffold(
    dir: &Path,
    name: &str,
    role: ScaffoldRole,
    extends: Option<&str>,
) -> Result<Scaffolded, CompileError> {
    if !valid_agent_name(name) {
        return Err(CompileError::Other(format!(
            "`{name}` is no valid agent name ([a-z0-9][a-z0-9-]{{0,63}})"
        )));
    }
    let definition = dir.join("definition.toml");
    if definition.exists() {
        return Err(CompileError::Other(format!(
            "{} already exists; nothing was written",
            definition.display()
        )));
    }
    std::fs::create_dir_all(dir).map_err(CompileError::io(format!("create {}", dir.display())))?;
    std::fs::write(&definition, definition_template(name, role, extends))
        .map_err(CompileError::io(format!("write {}", definition.display())))?;
    let system = dir.join("system.md");
    let mut files = vec![definition];
    if !system.exists() {
        std::fs::write(&system, instructions_template(name, role))
            .map_err(CompileError::io(format!("write {}", system.display())))?;
        files.push(system);
    }
    Ok(Scaffolded {
        dir: dir.to_path_buf(),
        files,
        id: format!("user.agent.{name}@1"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_names_and_roles() {
        assert!(valid_agent_name("critic-2"));
        assert!(!valid_agent_name("Critic"));
        assert!(!valid_agent_name("-x"));
        assert!(!valid_agent_name(""));
        assert_eq!(ScaffoldRole::parse("worker"), Some(ScaffoldRole::Worker));
        assert_eq!(ScaffoldRole::parse("root"), None);
    }

    #[test]
    fn test_templates_parse_as_definitions() -> Result<(), String> {
        for role in [ScaffoldRole::Worker, ScaffoldRole::ChildOrchestrator] {
            let text = definition_template("demo", role, None);
            let raw =
                harw_agent_dsl::parse::parse_toml(&text).map_err(|error| error.to_string())?;
            assert_eq!(raw.id.to_string(), "user.agent.demo@1");
        }
        Ok(())
    }

    #[test]
    fn test_scaffold_refuses_to_overwrite() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let target = dir.path().join("demo");
        let first = scaffold(&target, "demo", ScaffoldRole::Worker, None)?;
        assert_eq!(first.files.len(), 2);
        assert!(scaffold(&target, "demo", ScaffoldRole::Worker, None).is_err());
        Ok(())
    }
}
