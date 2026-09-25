//! [`CompileUnit`]: one agent on its way through the passes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use harw_agent_dsl::diagnostics::{
    Diagnostic, DiagnosticCode, SourceFile, SourceIndex, SourceSpan,
};
use harw_agent_dsl::ir_v2::AgentIr;
use harw_agent_dsl::roles::AgentRoleId;
use serde::Serialize;

use crate::rights::{BaseCeiling, RightsDelta, RightsSet};

/// A file of this agent: stored once in the artifact's pool, referenced
/// from the agent's entry by `(kind, logical path, blake3)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitFile {
    /// What the file is to the agent (`skill`, `knowledge`, …).
    pub kind: String,
    /// Where the agent sees it (`skills/<name>/instructions.md`).
    pub path: String,
    /// Content.
    pub bytes: Vec<u8>,
}

/// A tool provider the agent needs (`ReachableTools`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderUse {
    /// Provider id.
    pub id: String,
    /// Implementing crate.
    pub crate_name: String,
    /// Runner feature.
    pub feature: String,
    /// Tools of the manifest it serves, sorted.
    pub tools: Vec<String>,
}

/// The rights flow of one agent: manifest ≤ base role ≤ author ceiling.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RightsFlow {
    /// What the manifest claims.
    pub manifest: RightsSet,
    /// The base-role ceiling, if one applies.
    pub base: Option<BaseCeiling>,
    /// The author ceiling, if the build has one.
    pub ceiling: Option<RightsSet>,
    /// Manifest over base role (a non-empty delta is an error).
    pub over_base: RightsDelta,
    /// Manifest over author ceiling (a non-empty delta is an error).
    pub over_ceiling: RightsDelta,
    /// Base role over author ceiling (narrowed away, a note).
    pub base_over_ceiling: RightsDelta,
    /// What the manifest leaves out of the base role (tools), for display.
    pub narrowed_tools: Vec<String>,
}

/// An embedded child agent (`ChildClosure`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompiledChild {
    /// Lookup name (delegation target name).
    pub name: String,
    /// Definition ID.
    pub id: String,
    /// Organizational role.
    pub role: AgentRoleId,
    /// Who references it (parent name).
    pub parent: String,
    /// `delegation` or `child-orchestrator`.
    pub via: String,
    /// Depth below the compiled root (1 = direct child).
    pub depth: u32,
    /// Whether the child only reads.
    pub read_only: bool,
    /// The child's v7 snapshot (hex).
    pub snapshot: String,
}

/// One agent on its way through the passes.
#[derive(Debug, Clone)]
pub struct CompileUnit {
    /// Lookup name.
    pub name: String,
    /// The IR (changed by the passes; the snapshot is recomputed at the end).
    pub ir: AgentIr,
    /// The definition directory on disk, if any.
    pub dir: Option<PathBuf>,
    /// All source files (for spans and excerpts).
    pub sources: Arc<Vec<SourceFile>>,
    /// This agent's files (skills, bundle files); the instruction text is
    /// added when the artifact is built.
    pub files: Vec<UnitFile>,
    /// The entries of every agent below this one (`ChildClosure`), each
    /// once; the artifact holds them next to this agent's own entry.
    pub agents: Vec<harw_agent_artifact::AgentInput>,
    /// Providers needed by the manifest.
    pub providers: BTreeMap<String, ProviderUse>,
    /// Runner features: providers plus interfaces.
    pub features: BTreeSet<String>,
    /// The rights flow (`RightsCheck`).
    pub rights: Option<RightsFlow>,
    /// Embedded children (`ChildClosure`), breadth first.
    pub children: Vec<CompiledChild>,
    /// Pruned tools with the reason.
    pub pruned: Vec<(String, String)>,
    /// Depth of this unit below the compiled root (0 = root).
    pub depth: u32,
    /// The v7 snapshot of the lowered IR before any pass (identifies the
    /// definition sources; `harw agent list` compares it with builds).
    pub source_snapshot: String,
}

impl CompileUnit {
    /// A unit for a freshly lowered IR.
    #[must_use]
    pub fn new(name: String, ir: AgentIr, dir: Option<PathBuf>, sources: Arc<Vec<SourceFile>>) -> Self {
        let source_snapshot = ir.compute_snapshot().digest;
        Self {
            name,
            ir,
            dir,
            sources,
            files: Vec::new(),
            agents: Vec::new(),
            providers: BTreeMap::new(),
            features: BTreeSet::new(),
            rights: None,
            children: Vec::new(),
            pruned: Vec::new(),
            depth: 0,
            source_snapshot,
        }
    }

    /// The span of `path` in the target's highest-priority file.
    #[must_use]
    pub fn span(&self, path: &str) -> Option<SourceSpan> {
        let target = self.ir.id.to_string();
        SourceIndex::new(&self.sources, Some(target.as_str())).span_for(path)
    }

    /// The span of `value` inside the array at `path` (`tools.admitted`),
    /// else the span of the array.
    #[must_use]
    pub fn span_of_item(&self, path: &str, value: &str) -> Option<SourceSpan> {
        let target = self.ir.id.to_string();
        let index = SourceIndex::new(&self.sources, Some(target.as_str()));
        for file in index.target_files().into_iter().rev() {
            let Ok(table) = toml::from_str::<toml::Table>(&file.text) else {
                continue;
            };
            let mut current: Option<&toml::Value> = None;
            let mut table_ref = &table;
            let mut found = true;
            for (position, segment) in path.split('.').enumerate() {
                match table_ref.get(segment) {
                    Some(value) if position + 1 == path.split('.').count() => {
                        current = Some(value);
                    }
                    Some(toml::Value::Table(inner)) => table_ref = inner,
                    _ => {
                        found = false;
                        break;
                    }
                }
            }
            if !found {
                continue;
            }
            let span = match current {
                Some(toml::Value::Array(items)) => items
                    .iter()
                    .position(|item| item.as_str() == Some(value))
                    .and_then(|position| file.span_of(&format!("{path}[{position}]"))),
                _ => None,
            };
            if span.is_some() {
                return span;
            }
        }
        self.span(path)
    }

    /// A diagnostic for `code` at `path` (span looked up).
    #[must_use]
    pub fn diagnostic(&self, code: &DiagnosticCode, path: &str, message: String) -> Diagnostic {
        Diagnostic::new(code, message)
            .with_path(path)
            .with_span(self.span(path))
    }

    /// Adds or replaces a file of this agent.
    pub fn put_file(&mut self, kind: &str, path: String, bytes: Vec<u8>) {
        self.files.retain(|file| !(file.kind == kind && file.path == path));
        self.files.push(UnitFile {
            kind: kind.to_owned(),
            path,
            bytes,
        });
    }

    /// The effective tools (admitted minus forbidden), sorted.
    #[must_use]
    pub fn effective_tools(&self) -> Vec<String> {
        self.ir
            .tools
            .admitted
            .iter()
            .filter(|tool| !self.ir.tools.forbidden.contains(tool))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Delegation targets and child orchestrators (declaration order,
    /// deduplicated) with how they are reached.
    #[must_use]
    pub fn child_names(&self) -> Vec<(String, &'static str)> {
        let mut out: Vec<(String, &'static str)> = Vec::new();
        for target in self.ir.spawn.delegation_targets.iter().flatten() {
            if !out.iter().any(|(name, _)| name == target) {
                out.push((target.clone(), "delegation"));
            }
        }
        for target in &self.ir.spawn.child_orchestrators {
            if !out.iter().any(|(name, _)| name == target) {
                out.push((target.clone(), "child-orchestrator"));
            }
        }
        out
    }
}

