//! Read-only explorer step executor for adaptive intent cycles (PL-90 W03).
//!
//! [`ReadOnlyExplorerExecutor`] is the first trusted [`CycleStepExecutor`]:
//! it turns admitted `Segment::Read` segments into digest-bound
//! [`EvidenceRecord`]s and refuses everything else. It never writes, never
//! spawns a process or child agent, and never follows a symlink out of the
//! workspace.
//!
//! # Trust boundaries
//!
//! - The model only names *target ids* (admission carries ids, nothing
//!   else). Which workspace-relative path an id stands for is decided by the
//!   trusted runtime in a [`ReadTargetMap`]; an id the map does not know is
//!   refused. The map is deliberately not `Deserialize`.
//! - Every read needs [`Permission::ReadWorkspace`] on the executor's
//!   [`AuthorityContext`] and goes through `harw_tool_fsread`'s `Scope`
//!   (RESOLVE_BENEATH-style `open_read`, regular files only, secret paths
//!   such as `.env` or `*.pem` denied).
//! - A file larger than the per-read cap is refused *before* reading; a
//!   truncated prefix is never digested, because a digest of a prefix would
//!   look like evidence about the whole file.
//! - Checkpoints carry only `digest + locator` (the workspace-relative path).
//!   File content never enters the durable record. A proposer that needs the
//!   text can get it from an optional, bounded, in-memory
//!   [`ExplorerReadCache`] that is lost on restart by design.
//!
//! # Partial failure: all or nothing
//!
//! A step that reads several targets either returns the evidence of *all* of
//! them or `Err(StepFailure)`. Reads have no side effects, so `Err` honestly
//! means "no effect happened", which is the contract of
//! [`CycleStepExecutor::execute`]; the driver then commits the step without
//! observations and counts it as a stall, so a permanently unreadable target
//! cannot loop forever. Returning a subset as `Ok` was rejected: the model
//! could take a partial result for the whole branch set (a `Fork` with a
//! `JoinPolicy::All` join), and the evidence set would depend on timing of
//! concurrent file changes. Re-reading on the next proposal is cheap. The
//! failure reason names the failing target so logs stay diagnosable.
//!
//! # Reconciliation
//!
//! [`CycleStepExecutor::reconcile`] always answers
//! [`StepReconciliation::NotStarted`]: a read is idempotent and has no
//! external effect to deduplicate, so dropping the journaled step and letting
//! the proposer ask again is safe and never produces stale evidence (a
//! recovered digest could describe a file that has changed since).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::io::Read;
use std::path::{Component, Path};
use std::sync::{Arc, Mutex};

use harw_authority::{AuthorityContext, Permission};
use harw_core::cancel::CancelToken;
use harw_tool_fsread::budget::MAX_TEXT_BYTES;
use harw_tool_fsread::scope::Scope;
use harw_types::ContentDigest;

use crate::cycle_runtime::{CycleStepExecutor, InFlightStep, StepFailure, StepReconciliation};
use crate::intent_cycle::{
    CycleAdmission, CycleCheckpoint, CycleObservations, CycleProposal, EvidenceRecord,
    EvidenceSourceKind, EvidenceTrust, Segment,
};

/// Default per-file cap (bytes). Files above it are refused, not truncated.
pub const DEFAULT_MAX_READ_BYTES: usize = MAX_TEXT_BYTES;

/// Default cap on the bytes read by one step (all of its reads together).
pub const DEFAULT_MAX_STEP_BYTES: usize = 4 * DEFAULT_MAX_READ_BYTES;

/// Default cap on the content kept by an [`ExplorerReadCache`].
pub const DEFAULT_CACHE_BYTES: usize = 4 * DEFAULT_MAX_READ_BYTES;

const MAX_TARGET_ID_BYTES: usize = 256;
const MAX_PATH_BYTES: usize = 4096;

/// Why a [`ReadTargetMap`] could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadTargetError {
    /// The target id is empty, too long or contains control characters.
    InvalidId(String),
    /// The id is mapped twice.
    DuplicateId(String),
    /// The path is empty, absolute, contains `.`/`..`/NUL, or is too long.
    InvalidPath { id: String },
}

impl fmt::Display for ReadTargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(id) => write!(f, "invalid read target id '{id}'"),
            Self::DuplicateId(id) => write!(f, "read target id '{id}' is mapped twice"),
            Self::InvalidPath { id } => {
                write!(f, "read target '{id}' maps to an invalid workspace path")
            }
        }
    }
}

impl std::error::Error for ReadTargetError {}

/// Trusted mapping `target id -> workspace-relative path`.
///
/// Built by the trusted runtime (not by model output); intentionally neither
/// `Deserialize` nor constructible from admission data. Secret-path policy is
/// not duplicated here: it is applied at read time by `Scope::rel_readable`,
/// the single source of that rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadTargetMap {
    paths: BTreeMap<String, String>,
}

impl ReadTargetMap {
    /// Builds the map from `(id, relative path)` pairs.
    ///
    /// # Errors
    /// [`ReadTargetError`] for an empty/oversized/control-character id, a
    /// duplicate id, or a path that is empty, absolute, longer than 4096
    /// bytes, or contains NUL, `.` or `..` components.
    pub fn new<I, K, V>(entries: I) -> Result<Self, ReadTargetError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut paths = BTreeMap::new();
        for (id, path) in entries {
            let (id, path) = (id.into(), path.into());
            if id.trim().is_empty()
                || id.len() > MAX_TARGET_ID_BYTES
                || id.chars().any(char::is_control)
            {
                return Err(ReadTargetError::InvalidId(id));
            }
            if !is_plain_relative(&path) {
                return Err(ReadTargetError::InvalidPath { id });
            }
            if paths.insert(id.clone(), path).is_some() {
                return Err(ReadTargetError::DuplicateId(id));
            }
        }
        Ok(Self { paths })
    }

    /// The workspace-relative path for `id`, if the runtime mapped it.
    #[must_use]
    pub fn path(&self, id: &str) -> Option<&str> {
        self.paths.get(id).map(String::as_str)
    }

    /// Mapped target ids, e.g. to derive `CycleTargets::read`.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.paths.keys().map(String::as_str)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// Only plain, normal path components: no root, prefix, `.` or `..`.
fn is_plain_relative(path: &str) -> bool {
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.contains('\0') {
        return false;
    }
    let mut components = Path::new(path).components().peekable();
    components.peek().is_some() && components.all(|c| matches!(c, Component::Normal(_)))
}

/// Bounded, in-memory store of the text the explorer read, keyed by evidence
/// id. Optional and lossy by design: it is a convenience for the proposer's
/// next prompt, never part of a checkpoint, and does not survive a restart.
///
/// Content of a step is inserted only when the whole step succeeded. When the
/// byte cap would be exceeded, older entries are evicted first (by insertion
/// order); content larger than the whole cap is not stored.
#[derive(Debug, Clone)]
pub struct ExplorerReadCache {
    inner: Arc<Mutex<CacheState>>,
}

#[derive(Debug, Default)]
struct CacheState {
    max_bytes: usize,
    used: usize,
    order: Vec<String>,
    entries: BTreeMap<String, String>,
}

impl ExplorerReadCache {
    #[must_use]
    pub fn new(max_bytes: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CacheState {
                max_bytes,
                ..CacheState::default()
            })),
        }
    }

    /// The cached text for `evidence_id`, if still held.
    #[must_use]
    pub fn get(&self, evidence_id: &str) -> Option<String> {
        self.inner
            .lock()
            .ok()
            .and_then(|state| state.entries.get(evidence_id).cloned())
    }

    /// Bytes currently held.
    #[must_use]
    pub fn used_bytes(&self) -> usize {
        self.inner.lock().map_or(0, |state| state.used)
    }

    fn insert(&self, evidence_id: &str, text: String) {
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        if text.len() > state.max_bytes {
            return;
        }
        if let Some(old) = state.entries.remove(evidence_id) {
            state.used = state.used.saturating_sub(old.len());
            state.order.retain(|id| id != evidence_id);
        }
        while state.used + text.len() > state.max_bytes && !state.order.is_empty() {
            let oldest = state.order.remove(0);
            if let Some(old) = state.entries.remove(&oldest) {
                state.used = state.used.saturating_sub(old.len());
            }
        }
        state.used += text.len();
        state.order.push(evidence_id.to_owned());
        state.entries.insert(evidence_id.to_owned(), text);
    }
}

/// Read-only [`CycleStepExecutor`]: `Segment::Read` only.
#[derive(Debug)]
pub struct ReadOnlyExplorerExecutor {
    authority: AuthorityContext,
    targets: ReadTargetMap,
    max_read_bytes: usize,
    max_step_bytes: usize,
    cache: Option<ExplorerReadCache>,
}

impl ReadOnlyExplorerExecutor {
    /// Explorer over `authority`'s workspace with the default byte caps.
    #[must_use]
    pub fn new(authority: AuthorityContext, targets: ReadTargetMap) -> Self {
        Self {
            authority,
            targets,
            max_read_bytes: DEFAULT_MAX_READ_BYTES,
            max_step_bytes: DEFAULT_MAX_STEP_BYTES,
            cache: None,
        }
    }

    /// Overrides the per-file and per-step byte caps.
    #[must_use]
    pub fn with_limits(mut self, max_read_bytes: usize, max_step_bytes: usize) -> Self {
        self.max_read_bytes = max_read_bytes;
        self.max_step_bytes = max_step_bytes;
        self
    }

    /// Also hands the text of successful reads to `cache`.
    #[must_use]
    pub fn with_cache(mut self, cache: ExplorerReadCache) -> Self {
        self.cache = Some(cache);
        self
    }
}

/// The read targets of an executable proposal, or why it is not executable.
fn read_targets(proposal: &CycleProposal) -> Result<Vec<&str>, StepFailure> {
    let segments = match proposal {
        CycleProposal::Advance { segments } | CycleProposal::Reorient { segments, .. } => segments,
        CycleProposal::Fork { branches, .. } => branches,
        CycleProposal::Wait { .. } => return Ok(Vec::new()),
        _ => {
            return Err(StepFailure::new(
                "not supported by the read-only explorer: only read segments run",
            ));
        }
    };
    let mut targets = Vec::new();
    for segment in segments {
        match segment {
            Segment::Read { target } => targets.push(target.as_str()),
            Segment::Child { .. } | Segment::NestedRecipe { .. } => {
                return Err(StepFailure::new(
                    "not supported by the read-only explorer: child and nested recipe segments",
                ));
            }
        }
    }
    Ok(targets)
}

/// One completed read.
struct ReadOutcome {
    locator: String,
    digest: ContentDigest,
    text: String,
    bytes: usize,
}

/// Reads one file beneath `root`. Blocking; call from `spawn_blocking`.
fn read_beneath(
    root: &Path,
    relative: &str,
    max_read: usize,
    remaining_step: usize,
) -> Result<ReadOutcome, String> {
    let scope =
        Scope::new(root).map_err(|error| format!("workspace root not readable: {error}"))?;
    let rel = scope
        .rel_readable(relative)
        .map_err(|error| error.to_string())?;
    let mut file = scope.open_read(&rel).map_err(|error| error.to_string())?;
    let length = file
        .metadata()
        .map_err(|error| format!("metadata unreadable: {error}"))?
        .len();
    let limit = max_read.min(remaining_step);
    if length > limit as u64 {
        return Err(format!(
            "'{relative}' is {length} bytes, over the {limit} byte read budget"
        ));
    }
    let mut bytes = Vec::new();
    // One byte beyond the limit detects a file that grew since `metadata`.
    (&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read failed: {error}"))?;
    if bytes.len() > limit {
        return Err(format!(
            "'{relative}' grew past the {limit} byte read budget while reading"
        ));
    }
    Ok(ReadOutcome {
        locator: rel.display(),
        digest: ContentDigest::of(&bytes),
        text: String::from_utf8_lossy(&bytes).into_owned(),
        bytes: bytes.len(),
    })
}

impl CycleStepExecutor for ReadOnlyExplorerExecutor {
    fn execute(
        &mut self,
        step: &InFlightStep,
        _admission: &CycleAdmission,
        _state: &CycleCheckpoint,
        cancel: &CancelToken,
    ) -> impl Future<Output = Result<CycleObservations, StepFailure>> + Send {
        let plan = read_targets(&step.proposal).and_then(|targets| {
            if targets.is_empty() {
                return Ok(Vec::new());
            }
            if !self
                .authority
                .permissions()
                .contains(Permission::ReadWorkspace)
            {
                return Err(StepFailure::new("ReadWorkspace permission missing"));
            }
            // Deduplicate: one evidence record per target and step.
            let unique: BTreeSet<&str> = targets.into_iter().collect();
            unique
                .into_iter()
                .map(|target| {
                    self.targets
                        .path(target)
                        .map(|path| (target.to_owned(), path.to_owned()))
                        .ok_or_else(|| {
                            StepFailure::new(format!("read target '{target}' is not mapped"))
                        })
                })
                .collect::<Result<Vec<_>, _>>()
        });
        let root = self.authority.workspace().canonical_root().to_path_buf();
        let key = step.idempotency_key.clone();
        let (max_read, max_step) = (self.max_read_bytes, self.max_step_bytes);
        let cache = self.cache.clone();
        let cancel = cancel.clone();
        async move {
            let plan = plan?;
            let mut evidence = Vec::with_capacity(plan.len());
            let mut texts = Vec::with_capacity(plan.len());
            let mut used = 0usize;
            for (target, relative) in plan {
                if cancel.is_cancelled() {
                    return Err(StepFailure::new("explorer step cancelled"));
                }
                let (root, remaining) = (root.clone(), max_step.saturating_sub(used));
                let outcome = tokio::task::spawn_blocking(move || {
                    read_beneath(&root, &relative, max_read, remaining)
                })
                .await
                .map_err(|error| StepFailure::new(format!("read task failed: {error}")))?
                .map_err(|reason| StepFailure::new(format!("read '{target}' failed: {reason}")))?;
                used += outcome.bytes;
                let id = format!("{key}:{target}");
                texts.push((id.clone(), outcome.text));
                evidence.push(EvidenceRecord {
                    id,
                    source: EvidenceSourceKind::Repository,
                    locator: outcome.locator,
                    digest: outcome.digest,
                    trust: EvidenceTrust::Observed,
                });
            }
            // Content leaves the step only when the whole step succeeded.
            if let Some(cache) = cache {
                for (id, text) in texts {
                    cache.insert(&id, text);
                }
            }
            Ok(CycleObservations {
                evidence,
                ..CycleObservations::default()
            })
        }
    }

    async fn reconcile(&mut self, _step: &InFlightStep) -> StepReconciliation {
        StepReconciliation::NotStarted
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;

    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_core::cancel::CancelReason;
    use harw_job_core::LeaseToken;
    use harw_job_store::RecordStore;
    use harw_types::{TenantId, WorkId, WorkspaceId};

    use super::*;
    use crate::cycle_runtime::{
        AuthorityReissuer, CycleDriver, CycleProposer, CycleRecord, CycleRunOutcome, CycleStore,
    };
    use crate::intent_cycle::{
        CycleLimits, CycleRefusal, CycleTargets, CycleTerminal, IntentBinding, JoinPolicy,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::AuthoritySnapshot;

    /// A real workspace directory below the harness root (the registry
    /// rejects roots outside it) that is removed on drop.
    struct Workspace {
        dir: tempfile::TempDir,
    }

    impl Workspace {
        fn new() -> TestResult<Self> {
            let dir = tempfile::Builder::new()
                .prefix("explorer-ws-")
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

        fn authority(&self, permissions: &[Permission]) -> TestResult<AuthorityContext> {
            let harness_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .ok_or(TestError::Missing("crate has a workspace parent"))?
                .to_path_buf();
            let name = self
                .dir
                .path()
                .file_name()
                .ok_or(TestError::Missing("workspace dir name"))?;
            let tenant = TenantId::from_str("explorer-tenant");
            let workspace = WorkspaceId::from_str("explorer-tests");
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
                PermissionSet::from_policy(permissions.iter().copied()),
            )
            .authority()
            .clone())
        }
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
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
                max_parallel_segments: 4,
                max_stall_transitions: 3,
            },
            CycleTargets {
                read: set(&["source", "other"]),
                write: set(&[]),
                children: set(&["explorer"]),
                recipes: set(&["hypothesis"]),
            },
        )
        .map_err(|_| TestError::Missing("admission builds"))
    }

    fn step(proposal: CycleProposal) -> InFlightStep {
        InFlightStep {
            epoch: 1,
            sequence: 0,
            idempotency_key: "cyc-0".to_owned(),
            proposal,
        }
    }

    fn read(target: &str) -> Segment {
        Segment::Read {
            target: target.to_owned(),
        }
    }

    fn advance(targets: &[&str]) -> CycleProposal {
        CycleProposal::Advance {
            segments: targets.iter().map(|t| read(t)).collect(),
        }
    }

    fn map(entries: &[(&str, &str)]) -> TestResult<ReadTargetMap> {
        ReadTargetMap::new(entries.iter().copied()).map_err(ctx("map builds"))
    }

    async fn run(
        executor: &mut ReadOnlyExplorerExecutor,
        proposal: CycleProposal,
        cancel: &CancelToken,
    ) -> Result<CycleObservations, StepFailure> {
        let admission = admission().map_err(|_| StepFailure::new("admission"))?;
        let state = CycleCheckpoint::initial(&admission, &executor.authority, 0);
        executor
            .execute(&step(proposal), &admission, &state, cancel)
            .await
    }

    fn step_err(_failure: StepFailure) -> TestError {
        TestError::Missing("executor step failed")
    }

    fn read_rights() -> [Permission; 1] {
        [Permission::ReadWorkspace]
    }

    fn failure_reason(result: Result<CycleObservations, StepFailure>) -> TestResult<String> {
        match result {
            Err(failure) => Ok(failure.reason),
            Ok(_) => Err(TestError::Missing("expected a step failure")),
        }
    }

    #[test]
    fn target_map_validates_ids_and_paths() -> TestResult {
        assert!(ReadTargetMap::new([("a", "src/lib.rs")]).is_ok());
        for bad in ["", "/etc/passwd", "../x", "a/../b", "./a", "a\0b"] {
            assert_eq!(
                ReadTargetMap::new([("a", bad)]),
                Err(ReadTargetError::InvalidPath { id: "a".to_owned() }),
                "path {bad:?}"
            );
        }
        assert_eq!(
            ReadTargetMap::new([(" ", "a")]),
            Err(ReadTargetError::InvalidId(" ".to_owned()))
        );
        assert_eq!(
            ReadTargetMap::new([("a", "x"), ("a", "y")]),
            Err(ReadTargetError::DuplicateId("a".to_owned()))
        );
        let targets = map(&[("a", "x"), ("b", "y")])?;
        assert_eq!(targets.ids().collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(targets.path("a"), Some("x"));
        assert_eq!(targets.path("c"), None);
        assert_eq!(targets.len(), 2);
        assert!(!targets.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn read_yields_observed_repository_evidence_with_digest() -> TestResult {
        let ws = Workspace::new()?;
        ws.write("src/lib.rs", b"pub fn hello() {}\n")?;
        let cache = ExplorerReadCache::new(DEFAULT_CACHE_BYTES);
        let mut executor = ReadOnlyExplorerExecutor::new(
            ws.authority(&read_rights())?,
            map(&[("source", "src/lib.rs")])?,
        )
        .with_cache(cache.clone());
        let observed = run(&mut executor, advance(&["source"]), &CancelToken::new())
            .await
            .map_err(step_err)?;
        assert_eq!(
            observed.evidence,
            vec![EvidenceRecord {
                id: "cyc-0:source".to_owned(),
                source: EvidenceSourceKind::Repository,
                locator: "src/lib.rs".to_owned(),
                digest: ContentDigest::of(b"pub fn hello() {}\n"),
                trust: EvidenceTrust::Observed,
            }]
        );
        assert!(observed.claims.is_empty() && observed.criteria.is_empty());
        assert_eq!(
            cache.get("cyc-0:source").as_deref(),
            Some("pub fn hello() {}\n")
        );
        Ok(())
    }

    #[tokio::test]
    async fn fork_and_reorient_read_and_duplicates_collapse() -> TestResult {
        let ws = Workspace::new()?;
        ws.write("a.txt", b"a")?;
        ws.write("b.txt", b"b")?;
        let mut executor = ReadOnlyExplorerExecutor::new(
            ws.authority(&read_rights())?,
            map(&[("source", "a.txt"), ("other", "b.txt")])?,
        );
        let fork = CycleProposal::Fork {
            branches: vec![read("source"), read("other"), read("source")],
            join: JoinPolicy::All {},
        };
        let observed = run(&mut executor, fork, &CancelToken::new())
            .await
            .map_err(step_err)?;
        assert_eq!(observed.evidence.len(), 2);
        let reorient = CycleProposal::Reorient {
            evidence_id: "x".to_owned(),
            segments: vec![read("other")],
        };
        let observed = run(&mut executor, reorient, &CancelToken::new())
            .await
            .map_err(step_err)?;
        assert_eq!(observed.evidence.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn unmapped_target_is_refused() -> TestResult {
        let ws = Workspace::new()?;
        ws.write("a.txt", b"a")?;
        let mut executor = ReadOnlyExplorerExecutor::new(
            ws.authority(&read_rights())?,
            map(&[("source", "a.txt")])?,
        );
        let reason =
            failure_reason(run(&mut executor, advance(&["other"]), &CancelToken::new()).await)?;
        assert!(reason.contains("'other' is not mapped"), "{reason}");
        Ok(())
    }

    #[tokio::test]
    async fn missing_read_permission_is_refused() -> TestResult {
        let ws = Workspace::new()?;
        ws.write("a.txt", b"a")?;
        let mut executor = ReadOnlyExplorerExecutor::new(
            ws.authority(&[Permission::WriteWorkspace])?,
            map(&[("source", "a.txt")])?,
        );
        let reason =
            failure_reason(run(&mut executor, advance(&["source"]), &CancelToken::new()).await)?;
        assert!(reason.contains("ReadWorkspace"), "{reason}");
        Ok(())
    }

    #[tokio::test]
    async fn symlink_escape_is_refused() -> TestResult {
        let ws = Workspace::new()?;
        let outside = tempfile::tempdir().map_err(ctx("outside dir"))?;
        std::fs::write(outside.path().join("loot.txt"), b"outside").map_err(ctx("outside file"))?;
        std::os::unix::fs::symlink(
            outside.path().join("loot.txt"),
            ws.dir.path().join("link.txt"),
        )
        .map_err(ctx("file symlink"))?;
        std::os::unix::fs::symlink(outside.path(), ws.dir.path().join("dirlink"))
            .map_err(ctx("dir symlink"))?;
        let cache = ExplorerReadCache::new(DEFAULT_CACHE_BYTES);
        let mut executor = ReadOnlyExplorerExecutor::new(
            ws.authority(&read_rights())?,
            map(&[("source", "link.txt"), ("other", "dirlink/loot.txt")])?,
        )
        .with_cache(cache.clone());
        for target in ["source", "other"] {
            let result = run(&mut executor, advance(&[target]), &CancelToken::new()).await;
            let reason = failure_reason(result)?;
            assert!(reason.contains("failed"), "{target}: {reason}");
        }
        assert_eq!(cache.used_bytes(), 0, "no content leaked");
        Ok(())
    }

    #[tokio::test]
    async fn secret_path_is_refused() -> TestResult {
        let ws = Workspace::new()?;
        ws.write(".env", b"TOKEN=1")?;
        ws.write("keys/server.pem", b"pem")?;
        let mut executor = ReadOnlyExplorerExecutor::new(
            ws.authority(&read_rights())?,
            map(&[("source", ".env"), ("other", "keys/server.pem")])?,
        );
        for target in ["source", "other"] {
            let result = run(&mut executor, advance(&[target]), &CancelToken::new()).await;
            let reason = failure_reason(result)?;
            assert!(reason.contains("protected"), "{target}: {reason}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn oversized_file_is_refused_not_truncated() -> TestResult {
        let ws = Workspace::new()?;
        ws.write("big.txt", &[b'x'; 33])?;
        ws.write("ok.txt", &[b'y'; 32])?;
        let mut executor = ReadOnlyExplorerExecutor::new(
            ws.authority(&read_rights())?,
            map(&[("source", "big.txt"), ("other", "ok.txt")])?,
        )
        .with_limits(32, 1024);
        let reason =
            failure_reason(run(&mut executor, advance(&["source"]), &CancelToken::new()).await)?;
        assert!(reason.contains("over the 32 byte"), "{reason}");
        let observed = run(&mut executor, advance(&["other"]), &CancelToken::new())
            .await
            .map_err(step_err)?;
        assert_eq!(observed.evidence[0].digest, ContentDigest::of(&[b'y'; 32]));
        Ok(())
    }

    #[tokio::test]
    async fn step_budget_and_partial_failure_are_all_or_nothing() -> TestResult {
        let ws = Workspace::new()?;
        ws.write("a.txt", &[b'a'; 20])?;
        ws.write("b.txt", &[b'b'; 20])?;
        let cache = ExplorerReadCache::new(DEFAULT_CACHE_BYTES);
        let mut executor = ReadOnlyExplorerExecutor::new(
            ws.authority(&read_rights())?,
            map(&[("source", "a.txt"), ("other", "b.txt")])?,
        )
        .with_limits(32, 30)
        .with_cache(cache.clone());
        // `other` (b.txt) sorts first; the second read exceeds the step cap.
        let reason = failure_reason(
            run(
                &mut executor,
                advance(&["source", "other"]),
                &CancelToken::new(),
            )
            .await,
        )?;
        assert!(reason.contains("read budget"), "{reason}");
        assert_eq!(cache.used_bytes(), 0, "failed step publishes no content");
        // A missing file after a good one: whole step fails, no evidence.
        ws.write("c.txt", b"c")?;
        let mut executor = ReadOnlyExplorerExecutor::new(
            ws.authority(&read_rights())?,
            map(&[("source", "c.txt"), ("other", "gone.txt")])?,
        )
        .with_cache(cache.clone());
        let result = run(
            &mut executor,
            advance(&["source", "other"]),
            &CancelToken::new(),
        )
        .await;
        assert!(failure_reason(result)?.contains("'other' failed"));
        assert_eq!(cache.used_bytes(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn unsupported_proposals_and_segments_are_refused() -> TestResult {
        let ws = Workspace::new()?;
        let mut executor = ReadOnlyExplorerExecutor::new(
            ws.authority(&read_rights())?,
            map(&[("source", "a.txt")])?,
        );
        let cancel = CancelToken::new();
        let child = CycleProposal::Advance {
            segments: vec![Segment::Child {
                target: "explorer".to_owned(),
            }],
        };
        let nested = CycleProposal::Advance {
            segments: vec![Segment::NestedRecipe {
                recipe: "hypothesis".to_owned(),
            }],
        };
        let mixed = CycleProposal::Advance {
            segments: vec![
                read("source"),
                Segment::Child {
                    target: "explorer".to_owned(),
                },
            ],
        };
        let others = vec![
            CycleProposal::ProposePatch {
                targets: set(&["a.rs"]),
                evidence_id: "e".to_owned(),
            },
            CycleProposal::Verify {
                evidence_id: "e".to_owned(),
            },
            CycleProposal::RequestApproval {
                reason: "r".to_owned(),
                requested_targets: set(&["a.rs"]),
            },
            CycleProposal::Revisit {
                evidence_id: "e".to_owned(),
                invalidated_claims: set(&["c"]),
            },
        ];
        for proposal in [child, nested, mixed].into_iter().chain(others) {
            let reason = failure_reason(run(&mut executor, proposal, &cancel).await)?;
            assert!(reason.contains("not supported"), "{reason}");
        }
        let waited = run(
            &mut executor,
            CycleProposal::Wait {
                reason: "idle".to_owned(),
            },
            &cancel,
        )
        .await
        .map_err(step_err)?;
        assert_eq!(waited, CycleObservations::default());
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_stops_before_reading() -> TestResult {
        let ws = Workspace::new()?;
        ws.write("a.txt", b"a")?;
        let cache = ExplorerReadCache::new(DEFAULT_CACHE_BYTES);
        let mut executor = ReadOnlyExplorerExecutor::new(
            ws.authority(&read_rights())?,
            map(&[("source", "a.txt")])?,
        )
        .with_cache(cache.clone());
        let cancel = CancelToken::new();
        cancel.cancel(CancelReason::User);
        let reason = failure_reason(run(&mut executor, advance(&["source"]), &cancel).await)?;
        assert!(reason.contains("cancelled"), "{reason}");
        assert_eq!(cache.used_bytes(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn reconcile_reports_not_started() -> TestResult {
        let ws = Workspace::new()?;
        let mut executor =
            ReadOnlyExplorerExecutor::new(ws.authority(&read_rights())?, map(&[("source", "a")])?);
        let outcome = executor.reconcile(&step(advance(&["source"]))).await;
        assert_eq!(outcome, StepReconciliation::NotStarted);
        Ok(())
    }

    #[test]
    fn cache_is_bounded_and_evicts_oldest() {
        let cache = ExplorerReadCache::new(10);
        cache.insert("a", "123456".to_owned());
        cache.insert("b", "1234".to_owned());
        assert_eq!(cache.used_bytes(), 10);
        cache.insert("c", "12345".to_owned());
        assert_eq!(cache.get("a"), None);
        assert_eq!(cache.get("b").as_deref(), Some("1234"));
        assert_eq!(cache.used_bytes(), 9);
        cache.insert("huge", "x".repeat(11));
        assert_eq!(cache.get("huge"), None);
        assert_eq!(cache.used_bytes(), 9);
    }

    // ----- integration: CycleDriver + scripted proposer + explorer -----

    struct FixedReissuer(AuthorityContext);

    impl AuthorityReissuer for FixedReissuer {
        fn reissue(&self, _snapshot: &AuthoritySnapshot) -> Result<AuthorityContext, String> {
            Ok(self.0.clone())
        }
    }

    struct Script(VecDeque<CycleProposal>);

    impl CycleProposer for Script {
        fn propose(
            &mut self,
            _admission: &CycleAdmission,
            _state: &CycleCheckpoint,
            _last_refusal: Option<CycleRefusal>,
        ) -> impl Future<Output = Result<CycleProposal, StepFailure>> + Send {
            let next = self
                .0
                .pop_front()
                .ok_or_else(|| StepFailure::new("script exhausted"));
            async move { next }
        }
    }

    /// Reads go to the real explorer; `Verify` stands in for the trusted
    /// verifier (W03 part B) and upgrades the observed evidence.
    struct ExplorerWithVerifier(ReadOnlyExplorerExecutor);

    impl CycleStepExecutor for ExplorerWithVerifier {
        fn execute(
            &mut self,
            step: &InFlightStep,
            admission: &CycleAdmission,
            state: &CycleCheckpoint,
            cancel: &CancelToken,
        ) -> impl Future<Output = Result<CycleObservations, StepFailure>> + Send {
            let verified = match &step.proposal {
                CycleProposal::Verify { evidence_id } => {
                    let record = EvidenceRecord {
                        id: format!("{}:verified", step.idempotency_key),
                        source: EvidenceSourceKind::Verification,
                        locator: format!("gate:{evidence_id}"),
                        digest: ContentDigest::of(evidence_id.as_bytes()),
                        trust: EvidenceTrust::Verified,
                    };
                    Some(CycleObservations {
                        criteria: BTreeMap::from([("tested".to_owned(), record.id.clone())]),
                        evidence: vec![record],
                        claims: BTreeMap::new(),
                    })
                }
                _ => None,
            };
            let read = self.0.execute(step, admission, state, cancel);
            async move {
                match verified {
                    Some(observations) => Ok(observations),
                    None => read.await,
                }
            }
        }

        fn reconcile(
            &mut self,
            step: &InFlightStep,
        ) -> impl Future<Output = StepReconciliation> + Send {
            self.0.reconcile(step)
        }
    }

    #[tokio::test]
    async fn driver_with_explorer_reaches_completion_proposed() -> TestResult {
        let ws = Workspace::new()?;
        ws.write("src/lib.rs", b"pub fn hello() {}\n")?;
        let authority = ws.authority(&read_rights())?;
        let admission = admission()?;

        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let records = RecordStore::create_ambient(&temp.path().join("cycles"))
            .map_err(ctx("cycle record store"))?;
        let store = Arc::new(CycleStore::new(records));
        let work_id = WorkId::from_str("job-w03");
        let record = CycleRecord::new(
            "cycle-w03",
            work_id.clone(),
            CycleCheckpoint::initial(&admission, &authority, 0),
        )
        .map_err(ctx("record builds"))?;
        store.create(&record).map_err(ctx("record persists"))?;

        let proposer = Script(VecDeque::from([
            advance(&["source"]),
            CycleProposal::Verify {
                evidence_id: "cycle-w03-0:source".to_owned(),
            },
            CycleProposal::Complete {},
        ]));
        let explorer =
            ReadOnlyExplorerExecutor::new(authority.clone(), map(&[("source", "src/lib.rs")])?);
        let mut driver = CycleDriver::new(
            Arc::clone(&store),
            proposer,
            ExplorerWithVerifier(explorer),
            Arc::new(FixedReissuer(authority)),
        );
        let lease = LeaseToken {
            work_id,
            epoch: 1,
            nonce: "nonce-1".to_owned(),
        };
        let outcome = driver
            .run("cycle-w03", &lease, &admission, &CancelToken::new())
            .await
            .map_err(ctx("driver runs"))?;
        let CycleRunOutcome::Terminal {
            terminal,
            checkpoint,
        } = outcome
        else {
            return Err(TestError::Missing("terminal outcome"));
        };
        assert_eq!(terminal, CycleTerminal::CompletionProposed);
        let evidence = checkpoint
            .evidence
            .get("cycle-w03-0:source")
            .ok_or(TestError::Missing("explorer evidence is checkpointed"))?;
        assert_eq!(evidence.trust, EvidenceTrust::Observed);
        assert_eq!(evidence.source, EvidenceSourceKind::Repository);
        assert_eq!(evidence.locator, "src/lib.rs");
        assert_eq!(evidence.digest, ContentDigest::of(b"pub fn hello() {}\n"));
        Ok(())
    }
}
