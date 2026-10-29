//! Runtime-bound source collection. Serialized contracts are data, never authority.
//!
//! Admission captures only explicitly typed collection nodes. It does not infer
//! semantic acceptance from a model's prose. Completing this contract may make
//! an already-admitted direct Coding/Integration/Docs successor ready, but only
//! through the restricted parent-progress transaction and immutable full-plan
//! replay: its criteria and file scopes remain exactly those admitted. This
//! module itself neither changes node status nor grants write permissions.
//!
//! A required source must be a regular UTF-8 file. Exact paths are bound here;
//! file type, symlinks and secret policy are enforced again by the retained
//! descriptor-relative reader. No directory discovery or immutable-source
//! promise is implied: digests describe the bytes actually observed.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn paths_reject_ambiguous_or_expanding_spellings() {
        for path in [
            "",
            "/etc/passwd",
            "a/../b",
            "a/./b",
            "a//b",
            "src/**",
            "x::symbol",
            "a/",
            ".env",
        ] {
            assert!(validate_source_path(path).is_err(), "{path}");
        }
        assert!(validate_source_path("docs/intro.md").is_ok());
    }

    #[test]
    fn unknown_contract_versions_and_fields_fail_closed() -> TestResult {
        let contract = fixture();
        let mut json = serde_json::to_value(&contract).map_err(ctx("encode"))?;
        json["schema_version"] = serde_json::json!(2);
        assert!(serde_json::from_value::<SourceCollectionContract>(json).is_err());
        let mut json = serde_json::to_value(&contract).map_err(ctx("encode"))?;
        json["authority"] = serde_json::json!("all");
        assert!(serde_json::from_value::<SourceCollectionContract>(json).is_err());
        Ok(())
    }

    #[test]
    fn targets_and_caps_are_validated_at_decode() -> TestResult {
        let mut contract = fixture();
        contract
            .sources
            .insert("source-0000".into(), "../outside".into());
        assert!(contract.validate().is_err());
        let mut contract = fixture();
        contract.max_total_bytes = usize::MAX;
        assert!(contract.validate().is_err());
        let mut contract = fixture();
        contract.criterion_indexes.clear();
        assert!(contract.validate().is_err());
        Ok(())
    }

    #[test]
    fn reader_is_id_only_bounded_and_requires_complete_coverage() -> TestResult {
        let (dir, authority) = workspace()?;
        std::fs::create_dir(dir.path().join("docs")).map_err(ctx("mkdir"))?;
        std::fs::write(dir.path().join("docs/a.md"), b"source text").map_err(ctx("write"))?;
        let mut reader = CollectionReader::new(&authority, &fixture()).map_err(ctx("reader"))?;
        assert!(reader.validate_complete().is_err());
        assert!(reader.read("docs/a.md").is_err());
        assert!(reader.read("../outside").is_err());
        assert!(reader.observed().is_empty());
        let read = reader.read("source-0000").map_err(ctx("read"))?;
        assert_eq!(read.digest, ContentDigest::of(b"source text"));
        assert_eq!(reader.read("source-0000").map_err(ctx("reread"))?, read);
        reader.validate_complete().map_err(ctx("complete"))?;
        assert_eq!(reader.observed().len(), 1);
        let mut small = fixture();
        small.max_file_bytes = 3;
        let mut limited = CollectionReader::new(&authority, &small).map_err(ctx("small reader"))?;
        assert!(limited.read("source-0000").is_err());
        assert!(limited.observed().is_empty());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn reader_refuses_replaced_symlinks_and_empty_sources() -> TestResult {
        let (dir, authority) = workspace()?;
        std::fs::create_dir(dir.path().join("docs")).map_err(ctx("mkdir"))?;
        std::fs::write(dir.path().join("outside.md"), b"outside").map_err(ctx("write"))?;
        let mut reader = CollectionReader::new(&authority, &fixture()).map_err(ctx("reader"))?;
        // Replacement happens after root binding but before opening the file.
        std::os::unix::fs::symlink("../outside.md", dir.path().join("docs/a.md"))
            .map_err(ctx("symlink"))?;
        assert!(reader.read("source-0000").is_err());
        std::fs::remove_file(dir.path().join("docs/a.md")).map_err(ctx("remove"))?;
        std::fs::write(dir.path().join("docs/a.md"), []).map_err(ctx("empty"))?;
        assert!(reader.read("source-0000").is_err());
        assert!(reader.observed().is_empty());
        Ok(())
    }

    fn workspace() -> TestResult<(tempfile::TempDir, AuthorityContext)> {
        use harw_authority::{
            PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
        };
        use harw_types::{TenantId, WorkspaceId};
        let dir = tempfile::Builder::new()
            .prefix("collection-")
            .tempdir_in(env!("CARGO_MANIFEST_DIR"))
            .map_err(ctx("temp"))?;
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or(crate::test_support::TestError::Missing("parent"))?;
        let relative = dir
            .path()
            .strip_prefix(root)
            .map_err(ctx("relative"))?
            .to_path_buf();
        let tenant = TenantId::from_str("collection-tenant");
        let workspace = WorkspaceId::from_str("collection-workspace");
        let registry = WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: tenant.clone(),
                workspace: workspace.clone(),
                root: relative,
            }],
        )
        .map_err(ctx("registry"))?;
        let binding = registry
            .resolve(&tenant, &workspace)
            .map_err(ctx("binding"))?;
        let authority = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        )
        .authority()
        .clone();
        Ok((dir, authority))
    }

    #[test]
    fn capture_requires_explicit_collection_and_exact_file_scope() -> TestResult {
        let mut plan = harw_plan::testing::base_plan();
        let mut node = harw_plan::testing::base_node("collect");
        node.kind = PlanNodeKind::Research;
        node.write_scope.clear();
        node.read_scope = vec![PathOrSymbol::Path("docs/a.md".into())];
        node.acceptance_criteria = vec![harw_plan::Criterion {
            description: "Collect source bytes, not semantic correctness".into(),
            verification: vec![VerificationStep::CollectSources {
                sources: vec!["docs/a.md".into()],
            }],
        }];
        plan.nodes = vec![node.clone()];
        let digest = ContentDigest::of(b"admission");
        let contracts =
            SourceCollectionContract::capture_all(&plan, digest).map_err(ctx("capture"))?;
        assert_eq!(contracts.len(), 1);
        assert_eq!(contracts[0].sources, fixture().sources);
        assert_eq!(contracts[0].admission_digest, digest);
        for bad_scope in [
            PathOrSymbol::Path("docs/**".into()),
            PathOrSymbol::symbol("docs/a.md", "Topic"),
            PathOrSymbol::Path("docs/b.md".into()),
        ] {
            plan.nodes[0].read_scope = vec![bad_scope];
            assert!(SourceCollectionContract::capture_all(&plan, digest).is_err());
        }
        plan.nodes[0] = node.clone();
        plan.nodes[0].forbidden_scope = vec![PathOrSymbol::Path("docs".into())];
        assert!(SourceCollectionContract::capture_all(&plan, digest).is_err());
        plan.nodes[0] = node.clone();
        plan.nodes[0].acceptance_criteria[0]
            .verification
            .push(VerificationStep::Manual {
                note: "correct analysis".into(),
            });
        assert!(SourceCollectionContract::capture_all(&plan, digest).is_err());
        plan.nodes[0] = node;
        plan.nodes[0].acceptance_criteria[0].verification = vec![VerificationStep::Manual {
            note: "Read docs and understand".into(),
        }];
        assert!(
            SourceCollectionContract::capture_all(&plan, digest)
                .map_err(ctx("legacy"))?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn reenqueued_plan_does_not_readmit_terminal_collection_nodes() -> TestResult {
        let mut plan = harw_plan::testing::base_plan();
        let mut node = harw_plan::testing::base_node("collected");
        node.kind = PlanNodeKind::Research;
        node.write_scope.clear();
        node.read_scope = vec![PathOrSymbol::Path("docs/a.md".into())];
        node.acceptance_criteria = vec![harw_plan::Criterion {
            description: "Collect exact source".into(),
            verification: vec![VerificationStep::CollectSources {
                sources: vec!["docs/a.md".into()],
            }],
        }];
        node.assignment = Some(harw_plan::Assignment {
            worker: "previous-parent".into(),
            attempt: 1,
            job: Some("old-job".into()),
        });
        for status in [
            harw_plan::PlanNodeStatus::Completed,
            harw_plan::PlanNodeStatus::Superseded,
            harw_plan::PlanNodeStatus::Invalidated,
        ] {
            node.status = status;
            plan.nodes = vec![node.clone()];
            assert!(
                SourceCollectionContract::capture_all(&plan, ContentDigest::of(b"new admission"))
                    .map_err(ctx("readmit completed plan"))?
                    .is_empty()
            );
        }
        node.assignment = None;
        for status in [
            harw_plan::PlanNodeStatus::InProgress,
            harw_plan::PlanNodeStatus::Blocked,
        ] {
            node.status = status;
            plan.nodes = vec![node.clone()];
            assert!(
                SourceCollectionContract::capture_all(&plan, ContentDigest::of(b"new admission"))
                    .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn non_utf8_source_cannot_become_a_text_artifact() -> TestResult {
        let (dir, authority) = workspace()?;
        std::fs::create_dir(dir.path().join("docs")).map_err(ctx("mkdir"))?;
        std::fs::write(dir.path().join("docs/a.md"), [0xff, 0xfe]).map_err(ctx("binary"))?;
        let mut reader = CollectionReader::new(&authority, &fixture()).map_err(ctx("reader"))?;
        assert!(reader.read("source-0000").is_err());
        assert!(reader.observed().is_empty());
        Ok(())
    }

    fn fixture() -> SourceCollectionContract {
        SourceCollectionContract {
            schema_version: 1,
            plan_id: "plan".into(),
            plan_revision: 1,
            admission_digest: ContentDigest::of(b"baseline"),
            node_id: "collect".into(),
            attempt: 1,
            sources: BTreeMap::from([("source-0000".into(), "docs/a.md".into())]),
            criterion_indexes: vec![0],
            max_file_bytes: MAX_FILE_BYTES,
            max_total_bytes: MAX_TOTAL_BYTES,
            max_draft_bytes: MAX_DRAFT_BYTES,
        }
    }
}

use crate::cycle_explorer::{ReadTargetMap, read_beneath};
use harw_authority::{AuthorityContext, Permission};
use harw_plan::ids::PathOrSymbol;
use harw_plan::{Plan, PlanNodeKind, VerificationStep};
use harw_tool_fsread::scope::{Scope, is_secret_path};
use harw_types::ContentDigest;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Fixed first-slice caps; stored values may narrow, never widen, these bounds.
pub const MAX_FILE_BYTES: usize = 64 * 1024;
/// Aggregate source content bound.
pub const MAX_TOTAL_BYTES: usize = 256 * 1024;
/// Model draft output byte bound.
pub const MAX_DRAFT_BYTES: usize = 16 * 1024;
const MAX_SOURCES: usize = 32;

/// Immutable runtime-captured collection descriptor. Deserialization validates
/// shape only; execution still needs the admitted job and actual parent claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ContractWire")]
pub struct SourceCollectionContract {
    pub schema_version: u32,
    pub plan_id: String,
    pub plan_revision: u64,
    pub admission_digest: ContentDigest,
    pub node_id: String,
    pub attempt: u32,
    pub sources: BTreeMap<String, String>,
    pub criterion_indexes: Vec<usize>,
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
    pub max_draft_bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractWire {
    schema_version: u32,
    plan_id: String,
    plan_revision: u64,
    admission_digest: ContentDigest,
    node_id: String,
    attempt: u32,
    sources: BTreeMap<String, String>,
    criterion_indexes: Vec<usize>,
    max_file_bytes: usize,
    max_total_bytes: usize,
    max_draft_bytes: usize,
}
impl TryFrom<ContractWire> for SourceCollectionContract {
    type Error = String;
    fn try_from(w: ContractWire) -> Result<Self, String> {
        let value = Self {
            schema_version: w.schema_version,
            plan_id: w.plan_id,
            plan_revision: w.plan_revision,
            admission_digest: w.admission_digest,
            node_id: w.node_id,
            attempt: w.attempt,
            sources: w.sources,
            criterion_indexes: w.criterion_indexes,
            max_file_bytes: w.max_file_bytes,
            max_total_bytes: w.max_total_bytes,
            max_draft_bytes: w.max_draft_bytes,
        };
        value.validate()?;
        Ok(value)
    }
}

/// Single canonical spelling. No symbol inference or wildcard expansion.
pub fn validate_source_path(path: &str) -> Result<(), String> {
    if path.contains(['*', '?', '[', ']', '\\'])
        || path.contains("::")
        || path.chars().any(char::is_control)
        || path.ends_with('/')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || is_secret_path(Path::new(path))
    {
        return Err("source must be an exact non-secret relative file path".into());
    }
    ReadTargetMap::new([("source", path)]).map_err(|error| error.to_string())?;
    Ok(())
}

impl SourceCollectionContract {
    /// Validates data without granting access or accepting model completion.
    pub fn validate(&self) -> Result<(), String> {
        harw_plan::ids::PlanId::parse(&self.plan_id).map_err(|e| e.to_string())?;
        if self.schema_version != 1
            || self.node_id.is_empty()
            || self.node_id.len() > 256
            || !self
                .node_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
            || self.attempt != 1
            || self.sources.is_empty()
            || self.sources.len() > MAX_SOURCES
            || self.criterion_indexes.is_empty()
            || self.criterion_indexes != (0..self.criterion_indexes.len()).collect::<Vec<_>>()
            || self.max_file_bytes == 0
            || self.max_file_bytes > MAX_FILE_BYTES
            || self.max_total_bytes < self.max_file_bytes
            || self.max_total_bytes > MAX_TOTAL_BYTES
            || self.max_draft_bytes == 0
            || self.max_draft_bytes > MAX_DRAFT_BYTES
        {
            return Err("unsupported source-collection contract shape/version/caps".into());
        }
        let mut seen = BTreeSet::new();
        for (index, (id, path)) in self.sources.iter().enumerate() {
            if id != &format!("source-{index:04}") || !seen.insert(path) {
                return Err("source IDs must be canonical, unique and ordered".into());
            }
            validate_source_path(path)?;
        }
        if !self.sources.values().is_sorted() {
            return Err("source paths must be ordered".into());
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<ContentDigest, String> {
        self.validate()?;
        let mut bytes = b"harw/source-collection/contract/v1\0".to_vec();
        serde_json::to_writer(&mut bytes, self).map_err(|e| e.to_string())?;
        Ok(ContentDigest::of(&bytes))
    }

    /// Ordered workspace-relative paths for trusted store receipt validation.
    /// These are NOT the opaque IDs accepted by the reader/model tool.
    pub fn required_sources(&self) -> Vec<String> {
        self.sources.values().cloned().collect()
    }

    /// Only explicit typed collection criteria opt in. Other research nodes
    /// remain unsupported, never translated from natural-language assertions.
    pub fn capture_all(plan: &Plan, admission_digest: ContentDigest) -> Result<Vec<Self>, String> {
        let mut result = Vec::new();
        for node in &plan.nodes {
            if matches!(
                node.status,
                harw_plan::PlanNodeStatus::Completed
                    | harw_plan::PlanNodeStatus::Superseded
                    | harw_plan::PlanNodeStatus::Invalidated
            ) {
                continue;
            }
            let opted_in = node
                .acceptance_criteria
                .iter()
                .flat_map(|c| &c.verification)
                .any(|step| matches!(step, VerificationStep::CollectSources { .. }));
            if !opted_in {
                continue;
            }
            if !matches!(node.kind, PlanNodeKind::Research | PlanNodeKind::Explore)
                || !matches!(
                    node.status,
                    harw_plan::PlanNodeStatus::Draft | harw_plan::PlanNodeStatus::Ready
                )
                || !node.evidence.is_empty()
                || !node.write_scope.is_empty()
                || node.assignment.is_some()
                || !node.output_contracts.is_empty()
            {
                return Err(format!(
                    "node {} is not an unassigned read-only collection",
                    node.id
                ));
            }
            let mut paths = BTreeSet::new();
            for criterion in &node.acceptance_criteria {
                if criterion.verification.is_empty() {
                    return Err("empty collection criterion".into());
                }
                for step in &criterion.verification {
                    let VerificationStep::CollectSources { sources } = step else {
                        return Err("collection cannot satisfy semantic or other criteria".into());
                    };
                    if sources.is_empty() {
                        return Err("empty required sources".into());
                    }
                    for path in sources {
                        validate_source_path(path)?;
                        paths.insert(path.clone());
                    }
                }
            }
            let scope = node
                .read_scope
                .iter()
                .map(|entry| match entry {
                    PathOrSymbol::Path(path) => {
                        validate_source_path(path)?;
                        Ok(path.clone())
                    }
                    PathOrSymbol::Symbol { .. } => Err("symbol read scopes are unsupported".into()),
                })
                .collect::<Result<BTreeSet<_>, String>>()?;
            if scope != paths {
                return Err("collection sources must equal exact admitted read scope".into());
            }
            for forbidden in &node.forbidden_scope {
                let PathOrSymbol::Path(forbidden) = forbidden else {
                    return Err("symbol forbidden scopes cannot be proven disjoint".into());
                };
                // Conservative: malformed/wildcard deny scopes block admission.
                validate_source_path(forbidden)?;
                if paths
                    .iter()
                    .any(|path| Path::new(path).starts_with(forbidden))
                {
                    return Err("collection intersects forbidden scope".into());
                }
            }
            let contract = Self {
                schema_version: 1,
                plan_id: plan.id.as_str().into(),
                plan_revision: plan.revision.value(),
                admission_digest,
                node_id: node.id.as_str().into(),
                attempt: 1,
                sources: paths
                    .into_iter()
                    .enumerate()
                    .map(|(i, p)| (format!("source-{i:04}"), p))
                    .collect(),
                criterion_indexes: (0..node.acceptance_criteria.len()).collect(),
                max_file_bytes: MAX_FILE_BYTES,
                max_total_bytes: MAX_TOTAL_BYTES,
                max_draft_bytes: MAX_DRAFT_BYTES,
            };
            contract.validate()?;
            result.push(contract);
            if result.len() > 64 {
                return Err("too many admitted collection prerequisites".into());
            }
        }
        Ok(result)
    }
}

/// Observed byte digest and exact UTF-8 text. Text is untrusted data;
/// invalid UTF-8 is refused so the artifact preserves the original bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectedSource {
    pub source_id: String,
    pub path: String,
    pub digest: ContentDigest,
    pub text: String,
    pub bytes: usize,
}

/// ID-only reader retaining the root descriptor for its whole lifetime.
/// Call blocking reads with the parent effect guard retained by the caller.
pub struct CollectionReader {
    contract: SourceCollectionContract,
    targets: ReadTargetMap,
    scope: Scope,
    observed: BTreeMap<String, CollectedSource>,
    used: usize,
}
impl CollectionReader {
    pub fn new(
        authority: &AuthorityContext,
        contract: &SourceCollectionContract,
    ) -> Result<Self, String> {
        contract.validate()?;
        if !authority.permissions().contains(Permission::ReadWorkspace) {
            return Err("ReadWorkspace permission missing".into());
        }
        let targets = ReadTargetMap::new(contract.sources.clone()).map_err(|e| e.to_string())?;
        let scope =
            Scope::new(authority.workspace().canonical_root()).map_err(|e| e.to_string())?;
        Ok(Self {
            contract: contract.clone(),
            targets,
            scope,
            observed: BTreeMap::new(),
            used: 0,
        })
    }
    pub fn read(&mut self, source_id: &str) -> Result<CollectedSource, String> {
        // Unknown IDs rejected before any filesystem access.
        let path = self
            .targets
            .path(source_id)
            .ok_or("unknown admitted source ID")?;
        if let Some(source) = self.observed.get(source_id) {
            return Ok(source.clone());
        }
        let read = read_beneath(
            &self.scope,
            path,
            self.contract.max_file_bytes,
            self.contract.max_total_bytes.saturating_sub(self.used),
        )?;
        if read.bytes == 0 {
            return Err("empty required source refused".into());
        }
        if read.bytes != read.text.len() || ContentDigest::of(read.text.as_bytes()) != read.digest {
            return Err("required sources must be valid UTF-8 text".into());
        }
        let source = CollectedSource {
            source_id: source_id.into(),
            path: read.locator,
            digest: read.digest,
            text: read.text,
            bytes: read.bytes,
        };
        self.used += source.bytes;
        self.observed.insert(source_id.into(), source.clone());
        Ok(source)
    }
    pub fn observed(&self) -> Vec<CollectedSource> {
        self.observed.values().cloned().collect()
    }
    pub fn validate_complete(&self) -> Result<(), String> {
        if self.observed.len() != self.targets.len() {
            return Err("required source coverage incomplete".into());
        }
        Ok(())
    }
}
