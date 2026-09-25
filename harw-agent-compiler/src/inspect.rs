//! `harw agent inspect`: what is inside a built binary or artifact file.

use std::path::{Path, PathBuf};

use harw_agent_artifact::{ARTIFACT_MAGIC, Artifact, ArtifactDigest, EmbeddedArtifact, PoolStats};
use harw_agent_dsl::ir_v2::AgentIr;
use serde::Serialize;

use crate::artifact_out::{INSTRUCTIONS_PAYLOAD_PATH, bundle_of, entry_ir, ir_from_artifact};
use crate::bin_dir::{BinDir, BuildRecord};
use crate::env::CompilerEnv;
use crate::error::CompileError;
use crate::passes::skill_payload_path;

/// A skill of the root agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectedSkill {
    /// Name.
    pub name: String,
    /// Hash recorded in the IR.
    pub hash: Option<String>,
    /// Whether the root's entry references it with that hash and the blob
    /// is in the pool.
    pub verified: bool,
}

/// One agent of the bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectedAgent {
    /// Agent ID.
    pub id: String,
    /// Lookup name.
    pub name: String,
    /// Role.
    pub role: String,
    /// Snapshot recorded in the index.
    pub snapshot: String,
    /// References: `kind logical_path blake3-prefix`.
    pub refs: Vec<String>,
    /// Direct children (names).
    pub children: Vec<String>,
}

/// The report of `harw agent inspect`.
#[derive(Debug, Clone, Serialize)]
pub struct InspectReport {
    /// The inspected file.
    pub path: PathBuf,
    /// `artifact` (a `.harwa` file) or `binary` (runner plus footer).
    pub container: String,
    /// Artifact digest.
    pub artifact_digest: String,
    /// v7 snapshot as recorded.
    pub snapshot: Option<String>,
    /// Whether the recorded snapshot matches a fresh computation.
    pub snapshot_verified: bool,
    /// Size of the runner part (binaries only).
    pub runner_bytes: Option<u64>,
    /// The root IR (header).
    pub ir: AgentIr,
    /// Skills of the root.
    pub skills: Vec<InspectedSkill>,
    /// Every agent of the bundle, root first.
    pub agents: Vec<InspectedAgent>,
    /// Pool statistics (dedup).
    pub pool: PoolStats,
    /// Whether the root's instruction text matches `instructions.blake3`.
    pub instructions_verified: bool,
    /// The build record, for an installed binary.
    pub build: Option<BuildRecord>,
}

/// Reads an artifact from a `.harwa` file or a built binary.
///
/// # Errors
/// [`CompileError::Io`] or the artifact's verification error (tampering is
/// [`harw_agent_artifact::ArtifactError::Tampered`]).
pub fn read_artifact(path: &Path) -> Result<(Artifact, String, Option<u64>), CompileError> {
    let bytes =
        std::fs::read(path).map_err(CompileError::io(format!("read {}", path.display())))?;
    if bytes.starts_with(ARTIFACT_MAGIC) {
        return Ok((Artifact::from_bytes(&bytes)?, "artifact".to_owned(), None));
    }
    let embedded = EmbeddedArtifact::from_executable_bytes(&bytes)?;
    let runner = embedded.runner_len();
    Ok((embedded.into_artifact(), "binary".to_owned(), Some(runner)))
}

/// Resolves the `inspect` argument: an existing file, else an installed
/// agent name in `~/.harw/bin`.
///
/// # Errors
/// [`CompileError::Other`] if it is neither.
pub fn resolve_target(
    env: &CompilerEnv,
    raw: &str,
) -> Result<(PathBuf, Option<BuildRecord>), CompileError> {
    let path = env.resolve(Path::new(raw));
    if path.is_file() {
        return Ok((path, None));
    }
    let bin = BinDir::new(env.bin_dir());
    if let Some(current) = bin.current(raw) {
        return Ok((current.file, Some(current.record)));
    }
    Err(CompileError::Other(format!(
        "`{raw}` is neither a file nor an agent installed in {}",
        env.bin_dir().display()
    )))
}

/// Inspects a file.
///
/// # Errors
/// See [`read_artifact`] and [`ir_from_artifact`]; an invalid bundle
/// layout is [`CompileError::Bundle`].
pub fn inspect_path(
    path: &Path,
    build: Option<BuildRecord>,
) -> Result<InspectReport, CompileError> {
    let (artifact, container, runner_bytes) = read_artifact(path)?;
    let bundle = bundle_of(&artifact)?;
    let ir = ir_from_artifact(&artifact)?;
    let root = bundle.header.root.clone();
    let root_refs = bundle
        .agents
        .get(&root)
        .map(|entry| entry.payload_refs.clone())
        .unwrap_or_default();
    let skills = ir
        .skills
        .entries
        .iter()
        .map(|entry| {
            let logical = skill_payload_path(&entry.name);
            let reference = root_refs
                .iter()
                .find(|reference| reference.logical_path == logical);
            InspectedSkill {
                name: entry.name.clone(),
                hash: entry.hash.clone(),
                verified: reference.is_some_and(|reference| {
                    Some(reference.blake3.to_hex()) == entry.hash
                        && bundle.resolve(&artifact, reference).is_some()
                }),
            }
        })
        .collect();
    let instructions_verified = if ir.instructions.text.is_empty() {
        true
    } else {
        bundle
            .file(&artifact, &root, INSTRUCTIONS_PAYLOAD_PATH)
            .is_some_and(|bytes| ArtifactDigest::of(bytes).to_hex() == ir.instructions.blake3)
    };
    let mut ids: Vec<&String> = bundle.agents.keys().collect();
    ids.sort_by_key(|id| **id != root);
    let agents = ids
        .into_iter()
        .filter_map(|id| {
            let entry = bundle.agents.get(id)?;
            let role = entry_ir(&bundle, id)
                .map(|agent| crate::passes::role_label(agent.role).to_owned())
                .unwrap_or_default();
            Some(InspectedAgent {
                id: id.clone(),
                name: entry.name.clone(),
                role,
                snapshot: bundle
                    .header
                    .agents
                    .get(id)
                    .map(|index| index.snapshot.clone())
                    .unwrap_or_default(),
                refs: entry
                    .payload_refs
                    .iter()
                    .map(|reference| {
                        format!(
                            "{} {} {}",
                            reference.kind,
                            reference.logical_path,
                            reference
                                .blake3
                                .to_hex()
                                .chars()
                                .take(12)
                                .collect::<String>()
                        )
                    })
                    .collect(),
                children: entry
                    .children
                    .iter()
                    .map(|child| child.name.clone())
                    .collect(),
            })
        })
        .collect();
    Ok(InspectReport {
        path: path.to_path_buf(),
        container,
        artifact_digest: artifact.digest().to_hex(),
        snapshot: ir.snapshot.as_ref().map(|snapshot| snapshot.digest.clone()),
        snapshot_verified: ir.verify_snapshot(),
        runner_bytes,
        skills,
        agents,
        pool: bundle.pool_stats(),
        instructions_verified,
        build,
        ir,
    })
}

/// `12 Referenzen, 5 einzigartig, gespeichert …, gespart …`.
#[must_use]
pub fn pool_line(pool: &PoolStats) -> String {
    format!(
        "{} Referenzen, {} einzigartig, gespeichert {}, gespart {}",
        pool.references,
        pool.blobs,
        crate::doctor::human_bytes(pool.stored_bytes),
        crate::doctor::human_bytes(pool.saved_bytes())
    )
}

impl InspectReport {
    /// Text form.
    #[must_use]
    pub fn text(&self) -> String {
        let ir = &self.ir;
        let permissions = &ir.permissions;
        let mut out = format!(
            "{} ({})\n  agent:      {} {} ({})\n  artifact:   {}\n  snapshot:   {} ({})\n",
            self.path.display(),
            self.container,
            ir.id,
            ir.version.0,
            crate::passes::role_label(ir.role),
            self.artifact_digest,
            self.snapshot.as_deref().unwrap_or("-"),
            if self.snapshot_verified {
                "verified"
            } else {
                "MISMATCH"
            },
        );
        let interfaces: Vec<&str> = ir.binary.interfaces.iter().map(|i| i.as_str()).collect();
        out.push_str(&format!(
            "  binary:     {} [{}], default {}\n",
            ir.binary.name,
            interfaces.join(", "),
            ir.binary.default_interface.as_str()
        ));
        out.push_str(&format!("  tools:      {}\n", list(&permissions.tools)));
        out.push_str(&format!(
            "  files:      read={} write={} paths={}\n",
            permissions.filesystem.read,
            permissions.filesystem.write,
            list(&permissions.filesystem.write_paths)
        ));
        out.push_str(&format!(
            "  network:    {:?} hosts={}\n  shell:      {}\n  host:       {}\n",
            permissions.network.mode,
            list(&permissions.network.hosts),
            permissions.shell,
            permissions.host
        ));
        out.push_str(&format!(
            "  spawn:      max_depth={} targets={}\n",
            permissions.spawn.max_depth,
            list(&permissions.spawn.delegation_targets)
        ));
        if let Some(budget) = &permissions.budget {
            out.push_str(&format!(
                "  budget:     tokens={:?} tool_calls={:?} wall_secs={:?}\n",
                budget.max_tokens, budget.max_tool_calls, budget.max_wall_secs
            ));
        }
        out.push_str(&format!(
            "  env:        {}\n",
            list(&permissions.required_env)
        ));
        if let Some(models) = &ir.models {
            out.push_str(&format!(
                "  models:     {}/{} effort={}\n",
                models.provider.as_deref().unwrap_or("-"),
                models.model.as_deref().unwrap_or("-"),
                models.effort.map_or("-", |effort| effort.as_str())
            ));
        }
        for skill in &self.skills {
            out.push_str(&format!(
                "  skill:      {} {} ({})\n",
                skill.name,
                skill.hash.as_deref().unwrap_or("-"),
                if skill.verified {
                    "verified"
                } else {
                    "NOT EMBEDDED"
                }
            ));
        }
        for agent in self.agents.iter().skip(1) {
            out.push_str(&format!(
                "  child:      {} {} ({})\n",
                agent.name, agent.id, agent.role
            ));
        }
        for agent in &self.agents {
            let children = if agent.children.is_empty() {
                String::new()
            } else {
                format!(", children {}", agent.children.join(", "))
            };
            out.push_str(&format!(
                "  agent {} ({} refs{children}):\n",
                agent.name,
                agent.refs.len()
            ));
            for reference in &agent.refs {
                out.push_str(&format!("    {reference}\n"));
            }
        }
        out.push_str(&format!("  pool:       {}\n", pool_line(&self.pool)));
        if let Some(build) = &self.build {
            out.push_str(&format!(
                "  build:      {} via {} on {} (harw {})\n",
                build.built_at, build.backend, build.target, build.harw_version
            ));
        }
        out
    }
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        "-".to_owned()
    } else {
        items.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_line_reports_savings() {
        let pool = PoolStats {
            references: 12,
            blobs: 5,
            referenced_bytes: 4 * 1024 * 1024,
            stored_bytes: 1024 * 1024,
        };
        let line = pool_line(&pool);
        assert!(line.starts_with("12 Referenzen, 5 einzigartig"), "{line}");
        assert!(line.contains("gespart 3072 KiB"), "{line}");
    }
}
