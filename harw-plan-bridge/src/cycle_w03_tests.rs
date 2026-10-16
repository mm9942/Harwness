//! Cross-module integration test for the PL-90 W03 building blocks.
//!
//! Drives a real [`CycleDriver`] with the [`ModelCycleProposer`] over a
//! scripted fake [`ModelProvider`] and the [`ReadOnlyExplorerExecutor`] over a
//! temporary workspace. The explorer only produces `Observed` evidence, so
//! the chain ends in `Escalated` and never fakes `Verified` evidence.

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use harw_authority::{
    AuthorityContext, AuthoritySnapshot, Permission, PermissionSet, SandboxSpec,
    WorkspaceRegistration, WorkspaceRegistry,
};
use harw_core::cancel::CancelToken;
use harw_core::{ModelError, ModelFuture, ModelProvider, ModelRequest, ModelResponse};
use harw_job_core::LeaseToken;
use harw_job_store::RecordStore;
use harw_types::{ContentDigest, ModelId, ReasoningEffort, TenantId, WorkId, WorkspaceId};

use crate::cycle_explorer::{ReadOnlyExplorerExecutor, ReadTargetMap};
use crate::cycle_proposer::{ModelCycleProposer, ProposerRoute, RoutePolicy};
use crate::cycle_runtime::{
    AuthorityReissuer, CycleDriver, CycleRecord, CycleRunOutcome, CycleStore,
};
use crate::intent_cycle::{
    CycleAdmission, CycleCheckpoint, CycleLimits, CycleTargets, CycleTerminal, EvidenceSourceKind,
    EvidenceTrust, IntentBinding,
};
use crate::test_support::{TestError, TestResult, ctx};

const FILE_ONE: &[u8] = b"pub fn first_file_body_marker() {}\n";
const FILE_TWO: &[u8] = b"pub fn second_file_body_marker() {}\n";

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

/// A workspace directory below the crate root (the registry rejects roots
/// outside the harness root) that is removed on drop.
struct Workspace {
    dir: tempfile::TempDir,
}

impl Workspace {
    fn new() -> TestResult<Self> {
        let dir = tempfile::Builder::new()
            .prefix("w03-ws-")
            .tempdir_in(env!("CARGO_MANIFEST_DIR"))
            .map_err(ctx("workspace dir"))?;
        Ok(Self { dir })
    }

    fn write(&self, relative: &str, content: &[u8]) -> TestResult {
        let path = self.dir.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(ctx("mkdir"))?;
        }
        std::fs::write(path, content).map_err(ctx("write file"))
    }

    fn authority(&self) -> TestResult<AuthorityContext> {
        let harness_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or(TestError::Missing("crate has a workspace parent"))?
            .to_path_buf();
        let name = self
            .dir
            .path()
            .file_name()
            .ok_or(TestError::Missing("workspace dir name"))?;
        let tenant = TenantId::from_str("w03-tenant");
        let workspace = WorkspaceId::from_str("w03-integration");
        let binding = WorkspaceRegistry::build(
            &harness_root,
            [WorkspaceRegistration {
                tenant: tenant.clone(),
                workspace: workspace.clone(),
                root: PathBuf::from("harw-plan-bridge").join(name),
            }],
        )
        .map_err(ctx("workspace registers"))?
        .resolve(&tenant, &workspace)
        .map_err(ctx("workspace resolves"))?;
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        )
        .authority()
        .clone())
    }
}

/// Replays scripted model replies and records every request.
struct Scripted {
    replies: Mutex<VecDeque<String>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl Scripted {
    fn new(replies: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.iter().map(|r| (*r).to_owned()).collect()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> usize {
        self.requests.lock().map_or(0, |r| r.len())
    }

    /// The debug rendering of the conversation history of request `index`.
    fn prompt(&self, index: usize) -> Option<String> {
        let requests = self.requests.lock().ok()?;
        Some(format!("{:?}", requests.get(index)?.history))
    }
}

impl ModelProvider for Scripted {
    fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
        if let Ok(mut seen) = self.requests.lock() {
            seen.push(request);
        }
        let next = self
            .replies
            .lock()
            .ok()
            .and_then(|mut queue| queue.pop_front())
            .ok_or_else(|| ModelError::RequestFailed("script exhausted".to_owned()));
        Box::pin(async move { next.map(ModelResponse::text) })
    }
}

struct FixedReissuer(AuthorityContext);

impl AuthorityReissuer for FixedReissuer {
    fn reissue(&self, _snapshot: &AuthoritySnapshot) -> Result<AuthorityContext, String> {
        Ok(self.0.clone())
    }
}

fn admission() -> TestResult<CycleAdmission> {
    CycleAdmission::new(
        IntentBinding {
            id: "intent-w03".to_owned(),
            revision: 1,
            digest: ContentDigest::of(b"intent-w03@1"),
            predecessor: None,
            acceptance: set(&["tested"]),
        },
        CycleLimits {
            max_transitions: 10,
            max_chain_depth: 1,
            max_spawn_depth: 1,
            max_parallel_segments: 2,
            max_stall_transitions: 3,
        },
        CycleTargets {
            read: set(&["source", "other"]),
            write: set(&[]),
            children: set(&[]),
            recipes: set(&[]),
        },
    )
    .map_err(|_| TestError::Missing("admission builds"))
}

#[tokio::test]
async fn advance_read_then_reorient_on_evidence_then_escalate() -> TestResult {
    let ws = Workspace::new()?;
    ws.write("src/one.rs", FILE_ONE)?;
    ws.write("src/two.rs", FILE_TWO)?;
    let authority = ws.authority()?;
    let adm = admission()?;

    let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let records =
        RecordStore::create_ambient(&temp.path().join("cycles")).map_err(ctx("record store"))?;
    let store = Arc::new(CycleStore::new(records));
    let work_id = WorkId::from_str("job-w03-it");
    let record = CycleRecord::new(
        "cycle-w03",
        work_id.clone(),
        CycleCheckpoint::initial(&adm, &authority, 0),
    )
    .map_err(ctx("record builds"))?;
    store.create(&record).map_err(ctx("record persists"))?;

    // Step 0 reads `source`; step 1 reorients on that evidence id and reads
    // `other`; step 2 escalates. The explorer cannot verify, so there is no
    // completion path in this cycle.
    let provider = Scripted::new(&[
        r#"{"kind":"advance","segments":[{"kind":"read","target":"source"}]}"#,
        r#"{"kind":"reorient","evidence_id":"cycle-w03-0:source","segments":[{"kind":"read","target":"other"}]}"#,
        r#"{"kind":"escalate","reason":"read both files, cannot verify"}"#,
    ]);
    let routes = RoutePolicy::new(ProposerRoute {
        model: ModelId::from("m-default"),
        effort: Some(ReasoningEffort::Low),
        max_output_tokens: 256,
    });
    let proposer = ModelCycleProposer::new(Arc::clone(&provider) as Arc<dyn ModelProvider>, routes);
    let targets = ReadTargetMap::new([("source", "src/one.rs"), ("other", "src/two.rs")])
        .map_err(ctx("target map"))?;
    let explorer = ReadOnlyExplorerExecutor::new(authority.clone(), targets);
    let mut driver = CycleDriver::new(
        Arc::clone(&store),
        proposer,
        explorer,
        Arc::new(FixedReissuer(authority)),
    );
    let lease = LeaseToken {
        work_id,
        epoch: 1,
        nonce: "nonce-1".to_owned(),
    };
    let outcome = driver
        .run("cycle-w03", &lease, &adm, &CancelToken::new())
        .await
        .map_err(ctx("driver runs"))?;
    let CycleRunOutcome::Terminal {
        terminal,
        checkpoint,
    } = outcome
    else {
        return Err(TestError::Missing("terminal outcome"));
    };

    assert_eq!(terminal, CycleTerminal::Escalated);
    assert_eq!(provider.calls(), 3, "no repair re-prompts were needed");

    // Evidence of both reads is checkpointed: Observed, digest of the bytes,
    // workspace-relative locator. Nothing is Verified.
    let first = checkpoint
        .evidence
        .get("cycle-w03-0:source")
        .ok_or(TestError::Missing("evidence of step 0"))?;
    assert_eq!(first.source, EvidenceSourceKind::Repository);
    assert_eq!(first.trust, EvidenceTrust::Observed);
    assert_eq!(first.locator, "src/one.rs");
    assert_eq!(first.digest, ContentDigest::of(FILE_ONE));
    let second = checkpoint
        .evidence
        .get("cycle-w03-1:other")
        .ok_or(TestError::Missing("evidence of step 1"))?;
    assert_eq!(second.trust, EvidenceTrust::Observed);
    assert_eq!(second.locator, "src/two.rs");
    assert_eq!(second.digest, ContentDigest::of(FILE_TWO));
    assert!(
        checkpoint
            .evidence
            .values()
            .all(|e| e.trust != EvidenceTrust::Verified),
        "a pure explorer cycle has no verified evidence"
    );
    assert!(checkpoint.criteria_met.is_empty());

    // The prompt of turn 2 names the evidence by id and locator, and carries
    // neither file content nor any digest.
    let turn2 = provider
        .prompt(1)
        .ok_or(TestError::Missing("request of turn 2"))?;
    assert!(turn2.contains("cycle-w03-0:source"), "{turn2}");
    assert!(turn2.contains("locator=src/one.rs"), "{turn2}");
    assert!(!turn2.contains("first_file_body_marker"), "{turn2}");
    assert!(!turn2.contains("second_file_body_marker"), "{turn2}");
    for digest in [first.digest, second.digest] {
        let hex = digest.to_string();
        assert!(!turn2.contains(&hex), "digest leaked into the prompt");
        assert!(
            !turn2.contains(&format!("{digest:?}")),
            "debug digest leaked into the prompt"
        );
    }
    Ok(())
}
