//! Immutable capability catalog snapshots and safe skill authoring.
//!
//! Suggestions are discovery context, not capability grants. A runtime must
//! still intersect the selected entry with its frozen sandbox and policy before
//! adding tools, instructions, or an MCP launch plan to an agent.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use harw_config::{
    AgentToml, McpServerToml, McpTransportToml, PluginToml, ResolvedConfig, SkillToml,
};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SuggestionKind {
    Skill,
    Plugin,
    Mcp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySuggestion {
    pub kind: SuggestionKind,
    pub name: String,
    pub description: String,
    /// Declared tool names only; the caller still needs an effective sandbox.
    pub tools: Vec<String>,
    /// Declared MCP references only; this does not start a server.
    pub mcps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmittedSuggestion {
    pub kind: SuggestionKind,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSuggestions {
    pub available: Vec<CapabilitySuggestion>,
    pub omitted: Vec<OmittedSuggestion>,
}

/// A named capability selected for a single agent spawn. Selection is an
/// explicit, server-side step: a model may propose names, but it cannot turn
/// an arbitrary catalog entry into a capability just by mentioning it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CapabilitySelection {
    pub kind: SuggestionKind,
    pub name: String,
}

/// Frozen, non-secret metadata for one capability selected at spawn time.
///
/// This is deliberately *not* an executable grant. The registry builder must
/// still apply sandbox and policy checks before registering a tool or starting
/// an MCP process. `definition_sha256` makes the exact catalog definition
/// attributable even if the live catalog is edited while the child runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivatedCapability {
    pub kind: SuggestionKind,
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub mcps: Vec<String>,
    pub definition_sha256: String,
}

/// Immutable capability contract handed to one spawned agent. It carries both
/// the advisory catalog it was allowed to inspect and the much smaller set
/// explicitly selected by the trusted spawn coordinator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnCapabilitySnapshot {
    pub agent: String,
    pub suggestions: AgentSuggestions,
    pub activated: Vec<ActivatedCapability>,
}

/// Non-secret, frozen MCP launch metadata for the process/runtime boundary.
/// Credential resolution remains outside this catalog and must use a trusted
/// secret resolver immediately before connecting or launching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpRuntimeDescriptor {
    pub name: String,
    pub description: String,
    pub transport: McpTransportToml,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub requires_auth: bool,
    pub tools: Vec<String>,
    pub definition_sha256: String,
}

/// A stable copy used by one spawned agent/run. Catalog mutations take effect
/// only when a later snapshot is built.
#[derive(Debug, Clone)]
pub struct CatalogSnapshot {
    agents: BTreeMap<String, AgentToml>,
    skills: BTreeMap<String, SkillToml>,
    plugins: BTreeMap<String, PluginToml>,
    mcps: BTreeMap<String, McpServerToml>,
}

impl CatalogSnapshot {
    pub fn from_config(config: &ResolvedConfig) -> CatalogResult<Self> {
        config
            .validate()
            .map_err(|error| CatalogError::InvalidConfig(error.to_string()))?;
        Ok(Self {
            agents: config
                .agents
                .iter()
                .map(|(name, agent)| (name.clone(), agent.clone()))
                .collect(),
            skills: config
                .skills
                .iter()
                .map(|(name, skill)| (name.clone(), skill.clone()))
                .collect(),
            plugins: config
                .plugins
                .iter()
                .map(|(name, plugin)| (name.clone(), plugin.clone()))
                .collect(),
            mcps: config
                .mcps
                .iter()
                .map(|(name, mcp)| (name.clone(), mcp.clone()))
                .collect(),
        })
    }

    /// Returns exactly the advisory entries configured for this agent. Disabled
    /// entries are retained as diagnostics but are never made available.
    pub fn suggestions_for_agent(&self, agent: &str) -> CatalogResult<AgentSuggestions> {
        let agent = self
            .agents
            .get(agent)
            .ok_or_else(|| CatalogError::AgentNotFound(agent.to_owned()))?;
        let mut available = Vec::new();
        let mut omitted = Vec::new();

        for name in &agent.suggestions.skills {
            let skill = self.skills.get(name).ok_or_else(|| {
                CatalogError::InvalidConfig(format!(
                    "validated skill suggestion '{name}' disappeared from snapshot"
                ))
            })?;
            if skill.enabled {
                available.push(CapabilitySuggestion {
                    kind: SuggestionKind::Skill,
                    name: name.clone(),
                    description: skill.description.clone(),
                    tools: skill.tools.clone(),
                    mcps: skill.mcps.clone(),
                });
            } else {
                omitted.push(omitted_suggestion(SuggestionKind::Skill, name, "disabled"));
            }
        }
        for name in &agent.suggestions.plugins {
            let plugin = self.plugins.get(name).ok_or_else(|| {
                CatalogError::InvalidConfig(format!(
                    "validated plugin suggestion '{name}' disappeared from snapshot"
                ))
            })?;
            if plugin.enabled {
                available.push(CapabilitySuggestion {
                    kind: SuggestionKind::Plugin,
                    name: name.clone(),
                    description: plugin.description.clone(),
                    tools: plugin.capabilities.tools.clone(),
                    mcps: plugin.capabilities.mcps.clone(),
                });
            } else {
                omitted.push(omitted_suggestion(SuggestionKind::Plugin, name, "disabled"));
            }
        }
        for name in &agent.suggestions.mcps {
            let mcp = self.mcps.get(name).ok_or_else(|| {
                CatalogError::InvalidConfig(format!(
                    "validated MCP suggestion '{name}' disappeared from snapshot"
                ))
            })?;
            if mcp.enabled {
                available.push(CapabilitySuggestion {
                    kind: SuggestionKind::Mcp,
                    name: name.clone(),
                    description: mcp.description.clone(),
                    tools: mcp.tools.clone(),
                    mcps: Vec::new(),
                });
            } else {
                omitted.push(omitted_suggestion(SuggestionKind::Mcp, name, "disabled"));
            }
        }

        Ok(AgentSuggestions { available, omitted })
    }

    /// Deterministic low-cost suggestion ranking for a task summary. This is a
    /// pre-model discovery aid, not an autonomous capability selector.
    pub fn rank_for_task(&self, agent: &str, task: &str) -> CatalogResult<AgentSuggestions> {
        let mut suggestions = self.suggestions_for_agent(agent)?;
        let terms = task
            .split(|character: char| !character.is_alphanumeric())
            .filter(|term| !term.is_empty())
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        suggestions.available.sort_by(|left, right| {
            let score = |suggestion: &CapabilitySuggestion| {
                let searchable =
                    format!("{} {}", suggestion.name, suggestion.description).to_lowercase();
                terms
                    .iter()
                    .filter(|term| searchable.contains(term.as_str()))
                    .count()
            };
            score(right)
                .cmp(&score(left))
                .then_with(|| left.name.cmp(&right.name))
        });
        Ok(suggestions)
    }

    /// Freezes an exact, validated subset of an agent's advisory capabilities
    /// for one spawn. Disabled, unknown, cross-agent, and duplicate entries
    /// are rejected rather than silently broadened or ignored.
    pub fn activation_snapshot(
        &self,
        agent: &str,
        selections: &[CapabilitySelection],
    ) -> CatalogResult<SpawnCapabilitySnapshot> {
        let suggestions = self.suggestions_for_agent(agent)?;
        let mut seen = std::collections::BTreeSet::new();
        let mut activated = Vec::with_capacity(selections.len());

        for selection in selections {
            if !seen.insert((selection.kind.clone(), selection.name.clone())) {
                return Err(CatalogError::DuplicateCapabilitySelection {
                    kind: selection.kind.clone(),
                    name: selection.name.clone(),
                });
            }
            let suggestion = suggestions
                .available
                .iter()
                .find(|candidate| {
                    candidate.kind == selection.kind && candidate.name == selection.name
                })
                .ok_or_else(|| CatalogError::CapabilityNotSuggested {
                    agent: agent.to_owned(),
                    kind: selection.kind.clone(),
                    name: selection.name.clone(),
                })?;
            let definition_sha256 = self.definition_hash(&selection.kind, &selection.name)?;
            activated.push(ActivatedCapability {
                kind: selection.kind.clone(),
                name: selection.name.clone(),
                description: suggestion.description.clone(),
                tools: suggestion.tools.clone(),
                mcps: suggestion.mcps.clone(),
                definition_sha256,
            });
        }

        Ok(SpawnCapabilitySnapshot {
            agent: agent.to_owned(),
            suggestions,
            activated,
        })
    }

    fn definition_hash(&self, kind: &SuggestionKind, name: &str) -> CatalogResult<String> {
        let encoded = match kind {
            SuggestionKind::Skill => {
                serde_json::to_vec(self.skills.get(name).ok_or_else(|| {
                    CatalogError::InvalidConfig(format!("skill '{name}' disappeared from snapshot"))
                })?)
            }
            SuggestionKind::Plugin => {
                serde_json::to_vec(self.plugins.get(name).ok_or_else(|| {
                    CatalogError::InvalidConfig(format!(
                        "plugin '{name}' disappeared from snapshot"
                    ))
                })?)
            }
            SuggestionKind::Mcp => serde_json::to_vec(self.mcps.get(name).ok_or_else(|| {
                CatalogError::InvalidConfig(format!("MCP '{name}' disappeared from snapshot"))
            })?),
        }
        .map_err(|error| {
            CatalogError::InvalidConfig(format!("cannot serialize {name} for provenance: {error}"))
        })?;
        Ok(format!("{:x}", Sha256::digest(encoded)))
    }

    /// Resolves only directly activated MCP capabilities into process/HTTP
    /// descriptors. The supplied spawn snapshot is revalidated against this
    /// immutable catalog snapshot, so a stale or fabricated activation cannot
    /// start an arbitrary configured server.
    pub fn mcp_runtime_descriptors(
        &self,
        spawn: &SpawnCapabilitySnapshot,
    ) -> CatalogResult<Vec<McpRuntimeDescriptor>> {
        let mut descriptors = Vec::new();
        for activated in &spawn.activated {
            if activated.kind != SuggestionKind::Mcp {
                continue;
            }
            let expected = self.activation_snapshot(
                &spawn.agent,
                &[CapabilitySelection {
                    kind: SuggestionKind::Mcp,
                    name: activated.name.clone(),
                }],
            )?;
            let expected = expected.activated.into_iter().next().ok_or_else(|| {
                CatalogError::InvalidConfig("MCP activation unexpectedly empty".to_owned())
            })?;
            if &expected != activated {
                return Err(CatalogError::StaleCapabilitySnapshot {
                    kind: SuggestionKind::Mcp,
                    name: activated.name.clone(),
                });
            }
            let mcp = self.mcps.get(&activated.name).ok_or_else(|| {
                CatalogError::InvalidConfig(format!(
                    "activated MCP '{}' disappeared from catalog snapshot",
                    activated.name
                ))
            })?;
            match mcp.transport {
                McpTransportToml::Stdio if mcp.command.as_deref().is_none_or(str::is_empty) => {
                    return Err(CatalogError::McpMissingCommand(mcp.name.clone()));
                }
                McpTransportToml::StreamableHttp
                    if mcp.url.as_deref().is_none_or(str::is_empty) =>
                {
                    return Err(CatalogError::McpMissingUrl(mcp.name.clone()));
                }
                _ => {}
            }
            descriptors.push(McpRuntimeDescriptor {
                name: mcp.name.clone(),
                description: mcp.description.clone(),
                transport: mcp.transport,
                command: mcp.command.clone(),
                args: mcp.args.clone(),
                url: mcp.url.clone(),
                requires_auth: mcp.auth.is_some(),
                tools: mcp.tools.clone(),
                definition_sha256: expected.definition_sha256,
            });
        }
        descriptors.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(descriptors)
    }
}

fn omitted_suggestion(kind: SuggestionKind, name: &str, reason: &str) -> OmittedSuggestion {
    OmittedSuggestion {
        kind,
        name: name.to_owned(),
        reason: reason.to_owned(),
    }
}

#[derive(Debug, Clone)]
pub struct SkillDraft {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub tools: Vec<String>,
    pub mcps: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SkillUpdate {
    pub description: Option<String>,
    pub instructions: Option<String>,
    pub tools: Option<Vec<String>>,
    pub mcps: Option<Vec<String>>,
    pub enabled: Option<bool>,
}

/// Frozen skill body that a runtime can attach to one child/run. The hash is
/// recorded with the run so later edits cannot silently change its behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRuntimeSnapshot {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub tools: Vec<String>,
    pub mcps: Vec<String>,
    pub source_path: PathBuf,
    pub sha256: String,
}

/// Filesystem authoring surface. It writes instructions before the manifest so
/// discovery never sees an enabled manifest whose referenced body is absent.
#[derive(Debug, Clone)]
pub struct SkillWorkspace {
    config_root: PathBuf,
}

impl SkillWorkspace {
    #[must_use]
    pub fn new(config_root: PathBuf) -> Self {
        Self { config_root }
    }

    pub fn create(&self, draft: SkillDraft) -> CatalogResult<SkillToml> {
        validate_skill_name(&draft.name)?;
        let directory = self.skill_directory(&draft.name);
        if directory.exists() {
            return Err(CatalogError::SkillAlreadyExists(draft.name));
        }
        std::fs::create_dir_all(&directory).map_err(|error| CatalogError::Io {
            path: directory.clone(),
            reason: error.to_string(),
        })?;
        let skill = SkillToml {
            name: draft.name,
            enabled: true,
            description: draft.description,
            instructions_file: Some("instructions.md".to_owned()),
            tools: draft.tools,
            mcps: draft.mcps,
        };
        atomic_write(
            &directory.join("instructions.md"),
            draft.instructions.as_bytes(),
        )?;
        atomic_toml(&directory.join("skill.toml"), &skill)?;
        Ok(skill)
    }

    pub fn update(&self, name: &str, update: SkillUpdate) -> CatalogResult<SkillToml> {
        validate_skill_name(name)?;
        let directory = self.skill_directory(name);
        let manifest = directory.join("skill.toml");
        let source = std::fs::read_to_string(&manifest).map_err(|error| CatalogError::Io {
            path: manifest.clone(),
            reason: error.to_string(),
        })?;
        let mut skill: SkillToml =
            toml::from_str(&source).map_err(|error| CatalogError::Toml(error.to_string()))?;
        if skill.name != name {
            return Err(CatalogError::InvalidConfig(format!(
                "skill manifest name '{}' does not match directory '{name}'",
                skill.name
            )));
        }
        if let Some(description) = update.description {
            skill.description = description;
        }
        if let Some(tools) = update.tools {
            skill.tools = tools;
        }
        if let Some(mcps) = update.mcps {
            skill.mcps = mcps;
        }
        if let Some(enabled) = update.enabled {
            skill.enabled = enabled;
        }
        if let Some(instructions) = update.instructions {
            let filename = skill
                .instructions_file
                .as_deref()
                .unwrap_or("instructions.md");
            atomic_write(
                &instruction_path(&directory, filename)?,
                instructions.as_bytes(),
            )?;
        }
        atomic_toml(&manifest, &skill)?;
        Ok(skill)
    }

    /// Loads one enabled skill into an immutable instruction/provenance
    /// snapshot. The instruction path is canonicalized and must stay inside
    /// its skill directory; symlink and traversal escapes are rejected.
    pub fn runtime_snapshot(&self, name: &str) -> CatalogResult<SkillRuntimeSnapshot> {
        validate_skill_name(name)?;
        let directory = self.skill_directory(name);
        let manifest = directory.join("skill.toml");
        let source = std::fs::read_to_string(&manifest).map_err(|error| CatalogError::Io {
            path: manifest.clone(),
            reason: error.to_string(),
        })?;
        let skill: SkillToml =
            toml::from_str(&source).map_err(|error| CatalogError::Toml(error.to_string()))?;
        if skill.name != name {
            return Err(CatalogError::InvalidConfig(format!(
                "skill manifest name '{}' does not match directory '{name}'",
                skill.name
            )));
        }
        if !skill.enabled {
            return Err(CatalogError::SkillDisabled(name.to_owned()));
        }
        let filename = skill
            .instructions_file
            .as_deref()
            .unwrap_or("instructions.md");
        let path = instruction_path(&directory, filename)?;
        let metadata = std::fs::metadata(&path).map_err(|error| CatalogError::Io {
            path: path.clone(),
            reason: error.to_string(),
        })?;
        const MAX_INSTRUCTIONS_BYTES: u64 = 512 * 1024;
        if metadata.len() > MAX_INSTRUCTIONS_BYTES {
            return Err(CatalogError::InstructionsTooLarge {
                path,
                size: metadata.len(),
                limit: MAX_INSTRUCTIONS_BYTES,
            });
        }
        let bytes = std::fs::read(&path).map_err(|error| CatalogError::Io {
            path: path.clone(),
            reason: error.to_string(),
        })?;
        let instructions = String::from_utf8(bytes.clone()).map_err(|error| {
            CatalogError::InvalidConfig(format!(
                "skill '{name}' instructions are not UTF-8: {error}"
            ))
        })?;
        let sha256 = format!("{:x}", Sha256::digest(bytes));
        Ok(SkillRuntimeSnapshot {
            name: skill.name,
            description: skill.description,
            instructions,
            tools: skill.tools,
            mcps: skill.mcps,
            source_path: path,
            sha256,
        })
    }

    fn skill_directory(&self, name: &str) -> PathBuf {
        self.config_root.join("skills").join(name)
    }
}

fn instruction_path(directory: &Path, filename: &str) -> CatalogResult<PathBuf> {
    let relative = Path::new(filename);
    if relative.as_os_str().is_empty()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::ParentDir
            )
        })
    {
        return Err(CatalogError::InvalidInstructionPath(filename.to_owned()));
    }
    let canonical_directory = directory.canonicalize().map_err(|error| CatalogError::Io {
        path: directory.to_path_buf(),
        reason: error.to_string(),
    })?;
    let candidate = directory.join(relative);
    let canonical = candidate.canonicalize().map_err(|error| CatalogError::Io {
        path: candidate,
        reason: error.to_string(),
    })?;
    if !canonical.starts_with(&canonical_directory) || !canonical.is_file() {
        return Err(CatalogError::InvalidInstructionPath(filename.to_owned()));
    }
    Ok(canonical)
}

fn validate_skill_name(name: &str) -> CatalogResult<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(CatalogError::InvalidSkillName(name.to_owned()));
    }
    Ok(())
}

fn atomic_toml(path: &Path, value: &SkillToml) -> CatalogResult<()> {
    let encoded =
        toml::to_string_pretty(value).map_err(|error| CatalogError::Toml(error.to_string()))?;
    atomic_write(path, encoded.as_bytes())
}

fn atomic_write(path: &Path, contents: &[u8]) -> CatalogResult<()> {
    let parent = path.parent().ok_or_else(|| {
        CatalogError::InvalidConfig(format!("path '{}' has no parent", path.display()))
    })?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| CatalogError::Io {
            path: parent.to_path_buf(),
            reason: error.to_string(),
        })?;
    temporary
        .write_all(contents)
        .map_err(|error| CatalogError::Io {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| CatalogError::Io {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    temporary.persist(path).map_err(|error| CatalogError::Io {
        path: path.to_path_buf(),
        reason: error.error.to_string(),
    })?;
    Ok(())
}

#[derive(Debug)]
pub enum CatalogError {
    InvalidConfig(String),
    AgentNotFound(String),
    CapabilityNotSuggested {
        agent: String,
        kind: SuggestionKind,
        name: String,
    },
    DuplicateCapabilitySelection {
        kind: SuggestionKind,
        name: String,
    },
    StaleCapabilitySnapshot {
        kind: SuggestionKind,
        name: String,
    },
    McpMissingCommand(String),
    McpMissingUrl(String),
    InvalidSkillName(String),
    SkillAlreadyExists(String),
    SkillDisabled(String),
    InvalidInstructionPath(String),
    InstructionsTooLarge {
        path: PathBuf,
        size: u64,
        limit: u64,
    },
    Io {
        path: PathBuf,
        reason: String,
    },
    Toml(String),
}

pub type CatalogResult<T> = Result<T, CatalogError>;

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(reason) => write!(f, "invalid catalog configuration: {reason}"),
            Self::AgentNotFound(name) => write!(f, "agent '{name}' not found in catalog"),
            Self::CapabilityNotSuggested { agent, kind, name } => write!(
                f,
                "{kind:?} capability '{name}' is not an enabled suggestion for agent '{agent}'"
            ),
            Self::DuplicateCapabilitySelection { kind, name } => {
                write!(f, "duplicate {kind:?} capability selection '{name}'")
            }
            Self::StaleCapabilitySnapshot { kind, name } => {
                write!(
                    f,
                    "stale or forged {kind:?} capability snapshot for '{name}'"
                )
            }
            Self::McpMissingCommand(name) => {
                write!(f, "stdio MCP '{name}' has no executable command")
            }
            Self::McpMissingUrl(name) => {
                write!(f, "Streamable HTTP MCP '{name}' has no endpoint URL")
            }
            Self::InvalidSkillName(name) => write!(f, "invalid skill name '{name}'"),
            Self::SkillAlreadyExists(name) => write!(f, "skill '{name}' already exists"),
            Self::SkillDisabled(name) => write!(f, "skill '{name}' is disabled"),
            Self::InvalidInstructionPath(path) => {
                write!(f, "invalid skill instruction path: '{path}'")
            }
            Self::InstructionsTooLarge { path, size, limit } => write!(
                f,
                "skill instructions '{}' are {size} bytes, exceeding the {limit} byte limit",
                path.display()
            ),
            Self::Io { path, reason } => write!(f, "catalog I/O at '{}': {reason}", path.display()),
            Self::Toml(reason) => write!(f, "catalog TOML error: {reason}"),
        }
    }
}

impl std::error::Error for CatalogError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use harw_config::{AgentSuggestionsToml, PluginCapabilitiesToml};
    #[test]
    fn snapshot_keeps_disabled_entries_out_of_agent_context() -> TestResult {
        let mut config = ResolvedConfig::default();
        config.agents.insert(
            "worker".to_owned(),
            AgentToml {
                name: "worker".to_owned(),
                role: "worker".to_owned(),
                description: String::new(),
                system_file: None,
                providers: Vec::new(),
                models: Vec::new(),
                skills: Vec::new(),
                suggestions: AgentSuggestionsToml {
                    skills: vec!["review".to_owned()],
                    plugins: vec!["git".to_owned()],
                    mcps: vec!["docs".to_owned()],
                },
                primary_provider: None,
                secondary_providers: Vec::new(),
                timeout_seconds: 120,
                max_retries: 2,
            },
        );
        config.skills.insert(
            "review".to_owned(),
            SkillToml {
                name: "review".to_owned(),
                enabled: true,
                description: "Review code".to_owned(),
                instructions_file: None,
                tools: vec!["read".to_owned()],
                mcps: Vec::new(),
            },
        );
        config.plugins.insert(
            "git".to_owned(),
            PluginToml {
                name: "git".to_owned(),
                version: "1".to_owned(),
                source: String::new(),
                enabled: false,
                description: String::new(),
                capabilities: PluginCapabilitiesToml::default(),
            },
        );
        config.mcps.insert(
            "docs".to_owned(),
            McpServerToml {
                name: "docs".to_owned(),
                description: "Docs".to_owned(),
                transport: Default::default(),
                command: None,
                args: Vec::new(),
                url: None,
                auth: None,
                tools: Vec::new(),
                enabled: false,
            },
        );

        let suggestions = CatalogSnapshot::from_config(&config)
            .map_err(ctx("from_config"))?
            .suggestions_for_agent("worker")
            .map_err(ctx("suggestions_for_agent"))?;
        assert_eq!(suggestions.available.len(), 1);
        assert_eq!(suggestions.available[0].name, "review");
        assert_eq!(suggestions.omitted.len(), 2);
        Ok(())
    }

    #[test]
    fn activation_snapshot_freezes_only_explicit_enabled_agent_suggestions() -> TestResult {
        let mut config = ResolvedConfig::default();
        config.agents.insert(
            "worker".to_owned(),
            AgentToml {
                name: "worker".to_owned(),
                role: "worker".to_owned(),
                description: String::new(),
                system_file: None,
                providers: Vec::new(),
                models: Vec::new(),
                skills: Vec::new(),
                suggestions: AgentSuggestionsToml {
                    skills: vec!["review".to_owned()],
                    plugins: vec!["git".to_owned()],
                    mcps: Vec::new(),
                },
                primary_provider: None,
                secondary_providers: Vec::new(),
                timeout_seconds: 120,
                max_retries: 2,
            },
        );
        config.skills.insert(
            "review".to_owned(),
            SkillToml {
                name: "review".to_owned(),
                enabled: true,
                description: "Review code".to_owned(),
                instructions_file: None,
                tools: vec!["read".to_owned()],
                mcps: Vec::new(),
            },
        );
        config.plugins.insert(
            "git".to_owned(),
            PluginToml {
                name: "git".to_owned(),
                version: "1".to_owned(),
                source: "registry:git".to_owned(),
                enabled: true,
                description: "Git helpers".to_owned(),
                capabilities: PluginCapabilitiesToml {
                    tools: vec!["git.status".to_owned()],
                    ..PluginCapabilitiesToml::default()
                },
            },
        );

        let snapshot = CatalogSnapshot::from_config(&config)
            .map_err(ctx("from_config"))?
            .activation_snapshot(
                "worker",
                &[CapabilitySelection {
                    kind: SuggestionKind::Skill,
                    name: "review".to_owned(),
                }],
            )
            .map_err(ctx("activation_snapshot"))?;
        assert_eq!(snapshot.agent, "worker");
        assert_eq!(snapshot.suggestions.available.len(), 2);
        assert_eq!(snapshot.activated.len(), 1);
        assert_eq!(snapshot.activated[0].name, "review");
        assert_eq!(snapshot.activated[0].tools, ["read"]);
        assert_eq!(snapshot.activated[0].definition_sha256.len(), 64);

        let snapshot = CatalogSnapshot::from_config(&config).map_err(ctx("from_config"))?;
        assert!(matches!(
            snapshot.activation_snapshot(
                "worker",
                &[CapabilitySelection {
                    kind: SuggestionKind::Mcp,
                    name: "not-configured".to_owned(),
                }],
            ),
            Err(CatalogError::CapabilityNotSuggested { .. })
        ));
        assert!(matches!(
            snapshot.activation_snapshot(
                "worker",
                &[
                    CapabilitySelection {
                        kind: SuggestionKind::Skill,
                        name: "review".to_owned(),
                    },
                    CapabilitySelection {
                        kind: SuggestionKind::Skill,
                        name: "review".to_owned(),
                    },
                ],
            ),
            Err(CatalogError::DuplicateCapabilitySelection { .. })
        ));
        Ok(())
    }

    #[test]
    fn mcp_runtime_descriptor_requires_a_verified_direct_activation() -> TestResult {
        let mut config = ResolvedConfig::default();
        config.agents.insert(
            "worker".to_owned(),
            AgentToml {
                name: "worker".to_owned(),
                role: "worker".to_owned(),
                description: String::new(),
                system_file: None,
                providers: Vec::new(),
                models: Vec::new(),
                skills: Vec::new(),
                suggestions: AgentSuggestionsToml {
                    skills: Vec::new(),
                    plugins: Vec::new(),
                    mcps: vec!["code-search".to_owned()],
                },
                primary_provider: None,
                secondary_providers: Vec::new(),
                timeout_seconds: 120,
                max_retries: 2,
            },
        );
        config.mcps.insert(
            "code-search".to_owned(),
            McpServerToml {
                name: "code-search".to_owned(),
                description: "Local source search".to_owned(),
                transport: McpTransportToml::Stdio,
                command: Some("/usr/local/bin/code-search-mcp".to_owned()),
                args: vec!["--stdio".to_owned()],
                url: None,
                auth: None,
                tools: vec!["search".to_owned()],
                enabled: true,
            },
        );
        let catalog = CatalogSnapshot::from_config(&config).map_err(ctx("from_config"))?;
        let mut spawn = catalog
            .activation_snapshot(
                "worker",
                &[CapabilitySelection {
                    kind: SuggestionKind::Mcp,
                    name: "code-search".to_owned(),
                }],
            )
            .map_err(ctx("activation_snapshot"))?;
        let descriptors = catalog
            .mcp_runtime_descriptors(&spawn)
            .map_err(ctx("mcp_runtime_descriptors"))?;
        assert_eq!(descriptors.len(), 1);
        assert_eq!(
            descriptors[0].command.as_deref(),
            Some("/usr/local/bin/code-search-mcp")
        );
        assert!(!descriptors[0].requires_auth);

        spawn.activated[0].definition_sha256 = "forged".to_owned();
        assert!(matches!(
            catalog.mcp_runtime_descriptors(&spawn),
            Err(CatalogError::StaleCapabilitySnapshot { .. })
        ));
        Ok(())
    }

    #[test]
    fn authoring_creates_then_updates_a_skill_atomically() -> TestResult {
        let temporary = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let workspace = SkillWorkspace::new(temporary.path().to_path_buf());
        let created = workspace
            .create(SkillDraft {
                name: "rust-review".to_owned(),
                description: "Review Rust".to_owned(),
                instructions: "Read the diff.".to_owned(),
                tools: vec!["read".to_owned()],
                mcps: Vec::new(),
            })
            .map_err(ctx("create"))?;
        assert!(created.enabled);
        let updated = workspace
            .update(
                "rust-review",
                SkillUpdate {
                    description: Some("Review Rust carefully".to_owned()),
                    instructions: Some("Read tests first.".to_owned()),
                    enabled: Some(false),
                    ..SkillUpdate::default()
                },
            )
            .map_err(ctx("update"))?;
        assert!(!updated.enabled);
        assert_eq!(
            std::fs::read_to_string(temporary.path().join("skills/rust-review/instructions.md"))
                .map_err(ctx("read_to_string"))?,
            "Read tests first."
        );
        Ok(())
    }

    #[test]
    fn skill_names_cannot_escape_the_catalog_directory() {
        assert!(matches!(
            validate_skill_name("../escape"),
            Err(CatalogError::InvalidSkillName(_))
        ));
    }

    #[test]
    fn runtime_snapshot_freezes_verified_skill_body_and_provenance() -> TestResult {
        let temporary = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let workspace = SkillWorkspace::new(temporary.path().to_path_buf());
        workspace
            .create(SkillDraft {
                name: "review".to_owned(),
                description: "Review code".to_owned(),
                instructions: "Read tests first.".to_owned(),
                tools: vec!["read".to_owned()],
                mcps: vec!["docs".to_owned()],
            })
            .map_err(ctx("create"))?;

        let snapshot = workspace
            .runtime_snapshot("review")
            .map_err(ctx("runtime_snapshot"))?;
        assert_eq!(snapshot.instructions, "Read tests first.");
        assert_eq!(snapshot.tools, ["read"]);
        assert_eq!(snapshot.sha256.len(), 64);
        assert!(snapshot.source_path.ends_with("instructions.md"));
        Ok(())
    }

    #[test]
    fn runtime_snapshot_rejects_instruction_path_escape() -> TestResult {
        let temporary = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let workspace = SkillWorkspace::new(temporary.path().to_path_buf());
        workspace
            .create(SkillDraft {
                name: "review".to_owned(),
                description: String::new(),
                instructions: "safe".to_owned(),
                tools: Vec::new(),
                mcps: Vec::new(),
            })
            .map_err(ctx("create"))?;
        std::fs::write(
            temporary.path().join("skills/review/skill.toml"),
            "name = \"review\"\ninstructions_file = \"../escape.md\"\n",
        )
        .map_err(ctx("write skill.toml"))?;

        assert!(matches!(
            workspace.runtime_snapshot("review"),
            Err(CatalogError::InvalidInstructionPath(_))
        ));
        Ok(())
    }
}

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
