//! Shared payload pool and agent entries (`agent-artifact-v1.md` §10).
//!
//! A compiled orchestrator carries its whole delegation closure. Instead of
//! nesting a complete artifact per child (each with its own copy of shared
//! skills), one artifact holds:
//!
//! - a **pool** of blobs, each payload exactly once:
//!   [`PayloadKind::Other`]`("blob")` at `pool/<blake3-hex>`; the dedup key
//!   is the BLAKE3 of the bytes, and the path must equal it;
//! - one **agent entry** per agent, the root included:
//!   [`PayloadKind::Other`]`("agent")` at `agents/<id>.json`, the canonical
//!   JSON of [`AgentEntry`]: the agent's IR, its `payload_refs`
//!   (`{kind, logical_path, blake3}`, no bytes) and its children;
//! - a **header** ([`BundleHeader`]) with the root's ID and IR and an
//!   `agents` index (ID → entry path and IR snapshot).
//!
//! # Normative checks ([`Bundle::from_artifact`], fail closed)
//! - the header has schema [`BUNDLE_SCHEMA`], its root is in the index;
//! - every index entry exists, parses (schema [`ENTRY_SCHEMA`]) and carries
//!   the indexed ID; no agent payload is missing from the index;
//! - every blob's path is `pool/<hex>` of its own BLAKE3;
//! - every reference resolves to a blob with the same hash;
//! - no blob is unreferenced, and no payload of another kind is present.
//!
//! # Determinism
//! Entries are canonical JSON with refs sorted by `(kind, logical_path)` and
//! children in declaration order; the artifact table is sorted by `(kind,
//! path)`, so pool blobs come out ordered by hash and entries by ID.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::artifact::{Artifact, ArtifactBuilder};
use crate::canonical::canonical_json;
use crate::digest::ArtifactDigest;
use crate::error::ArtifactError;
use crate::kind::PayloadKind;

/// Schema label of the bundle header.
pub const BUNDLE_SCHEMA: &str = "harwness.agent-bundle/v1";

/// Schema label of an agent entry.
pub const ENTRY_SCHEMA: &str = "harwness.agent-entry/v1";

/// Payload kind name of a pool blob.
pub const BLOB_KIND: &str = "blob";

/// Payload kind name of an agent entry.
pub const AGENT_KIND: &str = "agent";

/// Path prefix of pool blobs.
pub const POOL_PREFIX: &str = "pool/";

/// Path prefix of agent entries.
pub const AGENTS_PREFIX: &str = "agents/";

/// Path of the entry of agent `id`: `agents/<id>.json`.
#[must_use]
pub fn entry_path(id: &str) -> String {
    format!("{AGENTS_PREFIX}{id}.json")
}

/// Path of the blob with `hash`: `pool/<hex>`.
#[must_use]
pub fn blob_path(hash: &ArtifactDigest) -> String {
    format!("{POOL_PREFIX}{hash}")
}

/// A reference from an agent to a pool blob.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadRef {
    /// What the file is to the agent (`instructions`, `skill`, `knowledge`, …).
    pub kind: String,
    /// Where the agent sees it (`skills/<name>/instructions.md`).
    pub logical_path: String,
    /// BLAKE3 of the bytes; the blob is `pool/<blake3>`.
    pub blake3: ArtifactDigest,
}

/// How a child is reached from its parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildLink {
    /// Name the parent uses (delegation target name).
    pub name: String,
    /// The child's agent ID (its entry is `agents/<id>.json`).
    pub id: String,
    /// `delegation` or `child-orchestrator`.
    pub via: String,
}

/// One agent entry (`agents/<id>.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentEntry {
    /// Always [`ENTRY_SCHEMA`].
    pub schema: String,
    /// Agent ID.
    pub id: String,
    /// Lookup name.
    pub name: String,
    /// The agent's IR (canonical JSON value).
    pub ir: Value,
    /// References into the pool, sorted by `(kind, logical_path)`.
    pub payload_refs: Vec<PayloadRef>,
    /// Direct children, declaration order.
    pub children: Vec<ChildLink>,
}

/// One row of the header's agent index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexEntry {
    /// Path of the agent entry.
    pub entry: String,
    /// The IR's snapshot digest (empty if the IR has none).
    pub snapshot: String,
}

/// The header of a bundle artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleHeader {
    /// Always [`BUNDLE_SCHEMA`].
    pub schema: String,
    /// The root agent's ID.
    pub root: String,
    /// The root agent's IR.
    pub ir: Value,
    /// Every agent: ID → entry.
    pub agents: BTreeMap<String, IndexEntry>,
}

/// What goes into the bundle for one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInput {
    /// Agent ID.
    pub id: String,
    /// Lookup name.
    pub name: String,
    /// The IR as JSON (without trace).
    pub ir: Value,
    /// Files: `(kind, logical_path, bytes)`.
    pub files: Vec<(String, String, Vec<u8>)>,
    /// Direct children.
    pub children: Vec<ChildLink>,
}

/// Builds a bundle artifact.
#[derive(Debug, Clone)]
pub struct BundleBuilder {
    root: AgentInput,
    others: BTreeMap<String, AgentInput>,
}

fn snapshot_of(ir: &Value) -> String {
    ir.get("snapshot")
        .and_then(|snapshot| snapshot.get("digest"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn to_value<T: Serialize>(value: &T) -> Result<Value, ArtifactError> {
    serde_json::to_value(value).map_err(|error| ArtifactError::InvalidHeader {
        reason: error.to_string(),
    })
}

impl BundleBuilder {
    /// Starts a bundle with its root agent.
    #[must_use]
    pub fn new(root: AgentInput) -> Self {
        Self {
            root,
            others: BTreeMap::new(),
        }
    }

    /// Adds another agent of the closure (the first input per ID wins; the
    /// root's ID is ignored).
    #[must_use]
    pub fn add_agent(mut self, agent: AgentInput) -> Self {
        if agent.id != self.root.id {
            self.others.entry(agent.id.clone()).or_insert(agent);
        }
        self
    }

    /// Deduplicates the files into the pool and encodes the artifact.
    ///
    /// # Errors
    /// Every [`ArtifactBuilder::build`] error (limits, paths).
    pub fn build(self) -> Result<Artifact, ArtifactError> {
        let mut pool: BTreeMap<ArtifactDigest, Vec<u8>> = BTreeMap::new();
        let mut index = BTreeMap::new();
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        let agents = std::iter::once(&self.root).chain(self.others.values());
        for agent in agents {
            let mut refs: Vec<PayloadRef> = Vec::with_capacity(agent.files.len());
            for (kind, logical_path, bytes) in &agent.files {
                let hash = ArtifactDigest::of(bytes);
                pool.entry(hash).or_insert_with(|| bytes.clone());
                refs.push(PayloadRef {
                    kind: kind.clone(),
                    logical_path: logical_path.clone(),
                    blake3: hash,
                });
            }
            refs.sort();
            refs.dedup();
            let entry = AgentEntry {
                schema: ENTRY_SCHEMA.to_owned(),
                id: agent.id.clone(),
                name: agent.name.clone(),
                ir: agent.ir.clone(),
                payload_refs: refs,
                children: agent.children.clone(),
            };
            let path = entry_path(&agent.id);
            index.insert(
                agent.id.clone(),
                IndexEntry {
                    entry: path.clone(),
                    snapshot: snapshot_of(&agent.ir),
                },
            );
            entries.push((path, canonical_json(&to_value(&entry)?)));
        }
        let header = BundleHeader {
            schema: BUNDLE_SCHEMA.to_owned(),
            root: self.root.id.clone(),
            ir: self.root.ir.clone(),
            agents: index,
        };
        let mut builder = ArtifactBuilder::new(&to_value(&header)?);
        for (path, bytes) in entries {
            builder = builder.add_payload(PayloadKind::Other(AGENT_KIND.to_owned()), path, bytes);
        }
        for (hash, bytes) in pool {
            builder = builder.add_payload(
                PayloadKind::Other(BLOB_KIND.to_owned()),
                blob_path(&hash),
                bytes,
            );
        }
        builder.build()
    }
}

/// Why a bundle is rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleError {
    /// The header is no bundle header.
    Header(String),
    /// An indexed entry is missing or does not parse.
    Entry {
        /// Agent ID.
        id: String,
        /// Why.
        reason: String,
    },
    /// An agent payload is not in the index.
    UnindexedAgent(String),
    /// A blob's path is not the hash of its bytes.
    BlobHashMismatch(String),
    /// A reference names no blob.
    MissingBlob {
        /// Agent ID.
        id: String,
        /// Logical path of the reference.
        logical_path: String,
        /// The referenced hash.
        hash: String,
    },
    /// A blob nobody references.
    UnreferencedBlob(String),
    /// A payload that is neither an agent entry nor a blob.
    UnexpectedPayload(String),
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Header(reason) => write!(f, "bundle header: {reason}"),
            Self::Entry { id, reason } => write!(f, "agent entry {id}: {reason}"),
            Self::UnindexedAgent(path) => write!(f, "agent entry {path} is not in the index"),
            Self::BlobHashMismatch(path) => {
                write!(f, "blob {path} does not carry the hash of its bytes")
            }
            Self::MissingBlob {
                id,
                logical_path,
                hash,
            } => write!(
                f,
                "agent {id}: {logical_path} references the missing blob {hash}"
            ),
            Self::UnreferencedBlob(path) => write!(f, "blob {path} is referenced by no agent"),
            Self::UnexpectedPayload(path) => write!(f, "unexpected payload {path}"),
        }
    }
}

impl std::error::Error for BundleError {}

/// Pool statistics (for `harw agent inspect`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PoolStats {
    /// References over all agents.
    pub references: usize,
    /// Distinct blobs.
    pub blobs: usize,
    /// Bytes all references would take without dedup.
    pub referenced_bytes: u64,
    /// Bytes actually stored.
    pub stored_bytes: u64,
}

impl PoolStats {
    /// Bytes saved by the dedup.
    #[must_use]
    pub fn saved_bytes(&self) -> u64 {
        self.referenced_bytes.saturating_sub(self.stored_bytes)
    }
}

/// A verified bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    /// The header.
    pub header: BundleHeader,
    /// Every agent entry by ID.
    pub agents: BTreeMap<String, AgentEntry>,
    /// Blob sizes by hash.
    blob_sizes: BTreeMap<ArtifactDigest, u64>,
}

impl Bundle {
    /// Reads and verifies the bundle layout of an (already hash-verified)
    /// artifact (see module docs).
    ///
    /// # Errors
    /// The first [`BundleError`].
    pub fn from_artifact(artifact: &Artifact) -> Result<Self, BundleError> {
        let header: BundleHeader = serde_json::from_slice(artifact.header_json())
            .map_err(|error| BundleError::Header(error.to_string()))?;
        if header.schema != BUNDLE_SCHEMA {
            return Err(BundleError::Header(format!(
                "schema `{}`, expected `{BUNDLE_SCHEMA}`",
                header.schema
            )));
        }
        if !header.agents.contains_key(&header.root) {
            return Err(BundleError::Header(format!(
                "the root {} is not in the agent index",
                header.root
            )));
        }
        let agent_kind = PayloadKind::Other(AGENT_KIND.to_owned());
        let blob_kind = PayloadKind::Other(BLOB_KIND.to_owned());
        let mut blob_sizes = BTreeMap::new();
        let mut indexed_paths = BTreeSet::new();
        for payload in artifact.payloads() {
            if *payload.kind() == blob_kind {
                let expected = ArtifactDigest::from_bytes(*payload.blake3());
                if payload.path() != blob_path(&expected) {
                    return Err(BundleError::BlobHashMismatch(payload.path().to_owned()));
                }
                blob_sizes.insert(expected, payload.len() as u64);
            } else if *payload.kind() != agent_kind {
                return Err(BundleError::UnexpectedPayload(format!(
                    "{}:{}",
                    payload.kind(),
                    payload.path()
                )));
            }
        }
        let mut agents = BTreeMap::new();
        let mut referenced = BTreeSet::new();
        for (id, index) in &header.agents {
            indexed_paths.insert(index.entry.clone());
            let payload =
                artifact
                    .payload(&agent_kind, &index.entry)
                    .ok_or_else(|| BundleError::Entry {
                        id: id.clone(),
                        reason: format!("{} is missing", index.entry),
                    })?;
            let entry: AgentEntry =
                serde_json::from_slice(payload.bytes()).map_err(|error| BundleError::Entry {
                    id: id.clone(),
                    reason: error.to_string(),
                })?;
            if entry.schema != ENTRY_SCHEMA || entry.id != *id {
                return Err(BundleError::Entry {
                    id: id.clone(),
                    reason: format!("schema `{}` / id `{}`", entry.schema, entry.id),
                });
            }
            for reference in &entry.payload_refs {
                if !blob_sizes.contains_key(&reference.blake3) {
                    return Err(BundleError::MissingBlob {
                        id: id.clone(),
                        logical_path: reference.logical_path.clone(),
                        hash: reference.blake3.to_hex(),
                    });
                }
                referenced.insert(reference.blake3);
            }
            agents.insert(id.clone(), entry);
        }
        for payload in artifact.payloads() {
            if *payload.kind() == agent_kind && !indexed_paths.contains(payload.path()) {
                return Err(BundleError::UnindexedAgent(payload.path().to_owned()));
            }
        }
        if let Some(unreferenced) = blob_sizes.keys().find(|hash| !referenced.contains(*hash)) {
            return Err(BundleError::UnreferencedBlob(blob_path(unreferenced)));
        }
        Ok(Self {
            header,
            agents,
            blob_sizes,
        })
    }

    /// The root agent's entry.
    #[must_use]
    pub fn root(&self) -> Option<&AgentEntry> {
        self.agents.get(&self.header.root)
    }

    /// The bytes a reference points at.
    #[must_use]
    pub fn resolve<'a>(&self, artifact: &'a Artifact, reference: &PayloadRef) -> Option<&'a [u8]> {
        artifact
            .payload(
                &PayloadKind::Other(BLOB_KIND.to_owned()),
                &blob_path(&reference.blake3),
            )
            .map(crate::artifact::Payload::bytes)
    }

    /// The bytes of an agent's file by logical path.
    #[must_use]
    pub fn file<'a>(
        &self,
        artifact: &'a Artifact,
        id: &str,
        logical_path: &str,
    ) -> Option<&'a [u8]> {
        let entry = self.agents.get(id)?;
        let reference = entry
            .payload_refs
            .iter()
            .find(|reference| reference.logical_path == logical_path)?;
        self.resolve(artifact, reference)
    }

    /// Pool statistics.
    #[must_use]
    pub fn pool_stats(&self) -> PoolStats {
        let mut stats = PoolStats {
            blobs: self.blob_sizes.len(),
            stored_bytes: self.blob_sizes.values().sum(),
            ..PoolStats::default()
        };
        for entry in self.agents.values() {
            for reference in &entry.payload_refs {
                stats.references += 1;
                stats.referenced_bytes +=
                    self.blob_sizes.get(&reference.blake3).copied().unwrap_or(0);
            }
        }
        stats
    }

    /// Every blob hash, sorted.
    #[must_use]
    pub fn blob_hashes(&self) -> Vec<ArtifactDigest> {
        self.blob_sizes.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn agent(id: &str, files: &[(&str, &str, &[u8])], children: &[&str]) -> AgentInput {
        AgentInput {
            id: id.to_owned(),
            name: id.to_owned(),
            ir: serde_json::json!({"id": id, "snapshot": {"digest": format!("s-{id}")}}),
            files: files
                .iter()
                .map(|(kind, path, bytes)| ((*kind).to_owned(), (*path).to_owned(), bytes.to_vec()))
                .collect(),
            children: children
                .iter()
                .map(|child| ChildLink {
                    name: (*child).to_owned(),
                    id: (*child).to_owned(),
                    via: "delegation".to_owned(),
                })
                .collect(),
        }
    }

    fn family() -> BundleBuilder {
        let shared: &[u8] = b"# shared skill";
        BundleBuilder::new(agent(
            "lead",
            &[("instructions", "instructions/system.md", b"lead".as_slice())],
            &["a", "b"],
        ))
        .add_agent(agent(
            "a",
            &[("skill", "skills/review/instructions.md", shared)],
            &[],
        ))
        .add_agent(agent(
            "b",
            &[
                ("skill", "skills/review/instructions.md", shared),
                ("knowledge", "knowledge/x.md", b"x".as_slice()),
            ],
            &[],
        ))
    }

    #[test]
    fn test_shared_skill_is_stored_once_and_refs_resolve() -> TestResult {
        let artifact = family().build().map_err(ctx("build"))?;
        let bundle = Bundle::from_artifact(&artifact).map_err(ctx("verify"))?;
        assert_eq!(bundle.agents.len(), 3);
        let stats = bundle.pool_stats();
        assert_eq!(stats.references, 4);
        assert_eq!(stats.blobs, 3, "the shared skill is stored once");
        assert_eq!(stats.saved_bytes(), b"# shared skill".len() as u64);
        let skill = bundle.file(&artifact, "b", "skills/review/instructions.md");
        assert_eq!(skill, Some(&b"# shared skill"[..]));
        assert_eq!(bundle.root().map(|entry| entry.children.len()), Some(2));
        assert_eq!(bundle.header.agents["a"].snapshot, "s-a");
        Ok(())
    }

    #[test]
    fn test_bundle_is_deterministic() -> TestResult {
        let first = family().build().map_err(ctx("first"))?;
        // Other insertion order, same content.
        let second = BundleBuilder::new(agent(
            "lead",
            &[("instructions", "instructions/system.md", b"lead".as_slice())],
            &["a", "b"],
        ))
        .add_agent(agent(
            "b",
            &[
                ("knowledge", "knowledge/x.md", b"x".as_slice()),
                (
                    "skill",
                    "skills/review/instructions.md",
                    b"# shared skill".as_slice(),
                ),
            ],
            &[],
        ))
        .add_agent(agent(
            "a",
            &[(
                "skill",
                "skills/review/instructions.md",
                b"# shared skill".as_slice(),
            )],
            &[],
        ))
        .build()
        .map_err(ctx("second"))?;
        assert_eq!(first.to_bytes(), second.to_bytes());
        Ok(())
    }

    /// Builds an artifact with a hand-made header and payloads.
    fn raw(
        header: &BundleHeader,
        payloads: Vec<(PayloadKind, String, Vec<u8>)>,
    ) -> TestResult<Artifact> {
        let mut builder =
            ArtifactBuilder::new(&serde_json::to_value(header).map_err(ctx("header"))?);
        for (kind, path, bytes) in payloads {
            builder = builder.add_payload(kind, path, bytes);
        }
        builder.build().map_err(ctx("build raw"))
    }

    fn one_agent_header() -> BundleHeader {
        BundleHeader {
            schema: BUNDLE_SCHEMA.to_owned(),
            root: "r".to_owned(),
            ir: serde_json::json!({}),
            agents: [(
                "r".to_owned(),
                IndexEntry {
                    entry: entry_path("r"),
                    snapshot: String::new(),
                },
            )]
            .into(),
        }
    }

    fn entry_bytes(refs: Vec<PayloadRef>) -> TestResult<Vec<u8>> {
        let entry = AgentEntry {
            schema: ENTRY_SCHEMA.to_owned(),
            id: "r".to_owned(),
            name: "r".to_owned(),
            ir: serde_json::json!({}),
            payload_refs: refs,
            children: Vec::new(),
        };
        Ok(canonical_json(
            &serde_json::to_value(&entry).map_err(ctx("entry"))?,
        ))
    }

    #[test]
    fn test_tampered_missing_and_unreferenced_blobs_are_rejected() -> TestResult {
        let agent_kind = PayloadKind::Other(AGENT_KIND.to_owned());
        let blob_kind = PayloadKind::Other(BLOB_KIND.to_owned());
        let good = ArtifactDigest::of(b"skill");
        let reference = PayloadRef {
            kind: "skill".to_owned(),
            logical_path: "skills/s/instructions.md".to_owned(),
            blake3: good,
        };

        // A blob whose bytes do not match its pool path (tampered content).
        let tampered = raw(
            &one_agent_header(),
            vec![
                (
                    agent_kind.clone(),
                    entry_path("r"),
                    entry_bytes(vec![reference.clone()])?,
                ),
                (
                    blob_kind.clone(),
                    blob_path(&good),
                    b"skill, changed".to_vec(),
                ),
            ],
        )?;
        assert!(matches!(
            Bundle::from_artifact(&tampered),
            Err(BundleError::BlobHashMismatch(_))
        ));

        // A reference to a blob that is not there.
        let missing = raw(
            &one_agent_header(),
            vec![(
                agent_kind.clone(),
                entry_path("r"),
                entry_bytes(vec![reference.clone()])?,
            )],
        )?;
        assert!(matches!(
            Bundle::from_artifact(&missing),
            Err(BundleError::MissingBlob { .. })
        ));

        // A blob nobody references.
        let orphan = ArtifactDigest::of(b"orphan");
        let unreferenced = raw(
            &one_agent_header(),
            vec![
                (
                    agent_kind.clone(),
                    entry_path("r"),
                    entry_bytes(vec![reference])?,
                ),
                (blob_kind.clone(), blob_path(&good), b"skill".to_vec()),
                (blob_kind, blob_path(&orphan), b"orphan".to_vec()),
            ],
        )?;
        assert!(matches!(
            Bundle::from_artifact(&unreferenced),
            Err(BundleError::UnreferencedBlob(_))
        ));

        // Another payload kind is not allowed in a bundle.
        let foreign = raw(
            &one_agent_header(),
            vec![
                (agent_kind, entry_path("r"), entry_bytes(Vec::new())?),
                (PayloadKind::Skill, "skills/x.md".to_owned(), b"x".to_vec()),
            ],
        )?;
        assert!(matches!(
            Bundle::from_artifact(&foreign),
            Err(BundleError::UnexpectedPayload(_))
        ));
        Ok(())
    }
}
