//! From a finished [`CompileUnit`] to an [`Artifact`] and back.
//!
//! The artifact is a bundle (`agent-artifact-v1.md` §10,
//! [`harw_agent_artifact::bundle`]): a header with the root's IR and the
//! agent index, one entry per agent (the root and its whole delegation
//! closure) and a content-addressed pool that stores every file once.
//!
//! # Files and references (for the runner, wave 3)
//! | IR field | Reference in the agent entry |
//! |---|---|
//! | `instructions.blake3` (non-empty text) | `instructions` · `instructions/system.md` |
//! | `skills.entries[i].hash` | `skill` · `skills/<name>/instructions.md` |
//! | UIA bundle files | `knowledge` · `knowledge/<file>` |
//! | `spawn.delegation_targets`, `spawn.child_orchestrators` | `children[]` → `agents/<id>.json` |
//!
//! Every hash is lowercase hex BLAKE3 of the bytes. Entries carry the IR
//! without its trace (timestamps and file labels), which keeps the artifact
//! reproducible.

use harw_agent_artifact::{AgentInput, Artifact, Bundle, BundleBuilder, ChildLink};
use harw_agent_dsl::ir_v2::{AgentIr, Trace};

use crate::error::CompileError;
use crate::unit::CompileUnit;

/// Logical path of the instruction text.
pub const INSTRUCTIONS_PAYLOAD_PATH: &str = "instructions/system.md";

/// The IR as stored in an artifact: its JSON with the trace removed.
///
/// # Errors
/// [`CompileError::Other`] if the IR cannot be serialized (not expected).
pub fn header_value(ir: &AgentIr) -> Result<serde_json::Value, CompileError> {
    let mut header = ir.clone();
    header.trace = Trace::default();
    serde_json::to_value(&header)
        .map_err(|error| CompileError::Other(format!("serialize the IR: {error}")))
}

/// The bundle entry of one finished unit: its IR, its files (instruction
/// text first) and its direct children.
///
/// # Errors
/// As [`header_value`].
pub fn agent_input(unit: &CompileUnit) -> Result<AgentInput, CompileError> {
    let mut files: Vec<(String, String, Vec<u8>)> = Vec::new();
    if !unit.ir.instructions.text.is_empty() {
        files.push((
            "instructions".to_owned(),
            INSTRUCTIONS_PAYLOAD_PATH.to_owned(),
            unit.ir.instructions.text.as_bytes().to_vec(),
        ));
    }
    files.extend(
        unit.files
            .iter()
            .map(|file| (file.kind.clone(), file.path.clone(), file.bytes.clone())),
    );
    let children = unit
        .children
        .iter()
        .filter(|child| child.parent == unit.name && child.depth == unit.depth + 1)
        .map(|child| ChildLink {
            name: child.name.clone(),
            id: child.id.clone(),
            via: child.via.clone(),
        })
        .collect();
    Ok(AgentInput {
        id: unit.ir.id.to_string(),
        name: unit.name.clone(),
        ir: header_value(&unit.ir)?,
        files,
        children,
    })
}

/// Builds the artifact of a finished unit (snapshot already recomputed):
/// its own entry plus every agent entry below it, files pooled.
///
/// # Errors
/// [`CompileError::Artifact`] for a limit or path violation.
pub fn build_artifact(unit: &CompileUnit) -> Result<Artifact, CompileError> {
    let mut builder = BundleBuilder::new(agent_input(unit)?);
    for agent in &unit.agents {
        builder = builder.add_agent(agent.clone());
    }
    Ok(builder.build()?)
}

/// Verifies the bundle layout of an artifact.
///
/// # Errors
/// [`CompileError::Bundle`] for the first layout violation.
pub fn bundle_of(artifact: &Artifact) -> Result<Bundle, CompileError> {
    Bundle::from_artifact(artifact).map_err(CompileError::Bundle)
}

/// Reads the root IR back from an artifact.
///
/// # Errors
/// [`CompileError::Bundle`] or [`CompileError::Other`] if the header holds
/// no `AgentIr` v2.
pub fn ir_from_artifact(artifact: &Artifact) -> Result<AgentIr, CompileError> {
    let bundle = bundle_of(artifact)?;
    serde_json::from_value(bundle.header.ir).map_err(|error| {
        CompileError::Other(format!("artifact header holds no AgentIr v2: {error}"))
    })
}

/// The IR of one agent entry of a bundle.
///
/// # Errors
/// [`CompileError::Other`] if the entry's IR is no `AgentIr` v2.
pub fn entry_ir(bundle: &Bundle, id: &str) -> Result<AgentIr, CompileError> {
    let entry = bundle
        .agents
        .get(id)
        .ok_or_else(|| CompileError::Other(format!("no agent entry {id}")))?;
    serde_json::from_value(entry.ir.clone())
        .map_err(|error| CompileError::Other(format!("agent entry {id}: {error}")))
}
