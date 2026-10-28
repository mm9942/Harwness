//! Deterministic acceptance verifier and the production step executor of
//! read-only intent cycles (PL-90 W03 wiring).
//!
//! Before this module an explorer cycle could never reach
//! [`CycleTerminal::CompletionProposed`](crate::intent_cycle::CycleTerminal):
//! `Complete` needs every acceptance criterion covered by `Verified`
//! evidence, and nothing produced `Verified` evidence. The verifier closes
//! that gap without trusting the model:
//!
//! - Each acceptance criterion carries a trusted [`ContentCheck`] (set by the
//!   human-approved intent, never by model output): a read target id plus
//!   substrings that must / must not occur in that file.
//! - An admitted `Verify { evidence_id }` names `Observed` repository
//!   evidence. The verifier re-reads the file behind it through the same
//!   workspace-bound `Scope` as the explorer, requires the content digest to
//!   be **unchanged** since the observation (a changed file yields a
//!   `stale:` claim, never a verdict), and evaluates every check bound to
//!   that target.
//! - A passing check becomes a new `Verified` evidence record of source
//!   [`EvidenceSourceKind::Verification`] that covers its criterion. A failing
//!   check becomes an `unmet:` claim the model can see by id. Neither path
//!   ever writes, spawns or widens anything.
//!
//! [`ExplorerCycleExecutor`] combines the read-only explorer (reads, waits)
//! with this verifier (verify) into the one executor the job worker runs.
//! Every other proposal kind stays refused, exactly as in the explorer.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::sync::Arc;

use harw_authority::{AuthorityContext, Permission};
use harw_core::cancel::CancelToken;
use harw_tool_fsread::scope::Scope;
use harw_types::ContentDigest;
use serde::{Deserialize, Serialize};

use crate::cycle_explorer::{
    DEFAULT_MAX_READ_BYTES, ExplorerReadCache, ReadOnlyExplorerExecutor, ReadTargetMap, normalized,
    read_beneath,
};
use crate::cycle_runtime::{CycleStepExecutor, InFlightStep, StepFailure, StepReconciliation};
use crate::intent_cycle::{
    CycleAdmission, CycleCheckpoint, CycleObservations, CycleProposal, EvidenceRecord,
    EvidenceSourceKind, EvidenceTrust,
};

/// Upper bound of acceptance checks per intent.
pub const MAX_ACCEPTANCE_CHECKS: usize = 32;
/// Upper bound of needles (`must_contain` + `must_not_contain`) per check.
pub const MAX_CHECK_NEEDLES: usize = 16;
/// Upper bound (bytes) of one needle.
pub const MAX_NEEDLE_BYTES: usize = 256;

/// A trusted, deterministic content check for one acceptance criterion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentCheck {
    /// Read target id (key of the trusted [`ReadTargetMap`]).
    pub target: String,
    /// Every one of these substrings must occur in the file.
    #[serde(default)]
    pub must_contain: Vec<String>,
    /// None of these substrings may occur in the file.
    #[serde(default)]
    pub must_not_contain: Vec<String>,
}

impl ContentCheck {
    /// `Ok` when the file content satisfies the check, else the first
    /// violated needle (bounded, for an `unmet:` claim).
    fn evaluate(&self, text: &str) -> Result<(), String> {
        if let Some(missing) = self
            .must_contain
            .iter()
            .find(|n| !text.contains(n.as_str()))
        {
            return Err(format!("required text missing: {}", clip(missing, 80)));
        }
        if let Some(present) = self
            .must_not_contain
            .iter()
            .find(|n| text.contains(n.as_str()))
        {
            return Err(format!("forbidden text present: {}", clip(present, 80)));
        }
        Ok(())
    }
}

/// Why a set of acceptance checks is unusable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckError {
    /// More than [`MAX_ACCEPTANCE_CHECKS`] checks.
    TooManyChecks,
    /// A criterion name is blank.
    BlankCriterion,
    /// The check names a target the trusted map does not know.
    UnknownTarget { criterion: String },
    /// No needle at all, too many, or a blank/oversized one.
    InvalidNeedles { criterion: String },
    /// An acceptance criterion of the intent has no check.
    UncheckedCriterion { criterion: String },
    /// A check names a criterion the intent does not have.
    UnknownCriterion { criterion: String },
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyChecks => write!(f, "more than {MAX_ACCEPTANCE_CHECKS} acceptance checks"),
            Self::BlankCriterion => f.write_str("an acceptance criterion is blank"),
            Self::UnknownTarget { criterion } => {
                write!(f, "check of '{criterion}' names an unknown read target")
            }
            Self::InvalidNeedles { criterion } => write!(
                f,
                "check of '{criterion}' needs 1..={MAX_CHECK_NEEDLES} non-blank needles of at most {MAX_NEEDLE_BYTES} bytes"
            ),
            Self::UncheckedCriterion { criterion } => {
                write!(f, "acceptance criterion '{criterion}' has no check")
            }
            Self::UnknownCriterion { criterion } => {
                write!(
                    f,
                    "check names '{criterion}', which is not an acceptance criterion"
                )
            }
        }
    }
}

impl std::error::Error for CheckError {}

/// Validated `criterion -> check` table. Not `Deserialize`: built by the
/// trusted runtime from the approved job input via [`AcceptanceChecks::new`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcceptanceChecks {
    checks: BTreeMap<String, ContentCheck>,
}

impl AcceptanceChecks {
    /// Validates `checks` against the trusted read target map.
    ///
    /// # Errors
    /// [`CheckError`] naming the first unusable entry.
    pub fn new(
        checks: BTreeMap<String, ContentCheck>,
        targets: &ReadTargetMap,
    ) -> Result<Self, CheckError> {
        if checks.len() > MAX_ACCEPTANCE_CHECKS {
            return Err(CheckError::TooManyChecks);
        }
        for (criterion, check) in &checks {
            if criterion.trim().is_empty() {
                return Err(CheckError::BlankCriterion);
            }
            if targets.path(&check.target).is_none() {
                return Err(CheckError::UnknownTarget {
                    criterion: criterion.clone(),
                });
            }
            let needles = check.must_contain.len() + check.must_not_contain.len();
            let well_formed = check
                .must_contain
                .iter()
                .chain(&check.must_not_contain)
                .all(|needle| !needle.trim().is_empty() && needle.len() <= MAX_NEEDLE_BYTES);
            if needles == 0 || needles > MAX_CHECK_NEEDLES || !well_formed {
                return Err(CheckError::InvalidNeedles {
                    criterion: criterion.clone(),
                });
            }
        }
        Ok(Self { checks })
    }

    /// Requires exactly one check per acceptance criterion of the intent.
    ///
    /// # Errors
    /// [`CheckError::UncheckedCriterion`] or [`CheckError::UnknownCriterion`].
    pub fn ensure_covers(&self, acceptance: &BTreeSet<String>) -> Result<(), CheckError> {
        if let Some(criterion) = acceptance.iter().find(|c| !self.checks.contains_key(*c)) {
            return Err(CheckError::UncheckedCriterion {
                criterion: criterion.clone(),
            });
        }
        if let Some(criterion) = self.checks.keys().find(|c| !acceptance.contains(*c)) {
            return Err(CheckError::UnknownCriterion {
                criterion: criterion.clone(),
            });
        }
        Ok(())
    }

    /// Checks (with their stable index) bound to `target`.
    fn for_target<'a>(
        &'a self,
        target: &'a str,
    ) -> impl Iterator<Item = (usize, &'a String, &'a ContentCheck)> + 'a {
        self.checks
            .iter()
            .enumerate()
            .filter(move |(_, (_, check))| check.target == target)
            .map(|(index, (criterion, check))| (index, criterion, check))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.checks.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.checks.is_empty()
    }
}

/// Read-only executor of production intent cycles: `Segment::Read` and
/// `Wait` through [`ReadOnlyExplorerExecutor`], `Verify` through the
/// deterministic [`AcceptanceChecks`]. Everything else is refused.
#[derive(Debug)]
pub struct ExplorerCycleExecutor {
    explorer: ReadOnlyExplorerExecutor,
    authority: AuthorityContext,
    targets: ReadTargetMap,
    checks: AcceptanceChecks,
    max_read_bytes: usize,
}

impl ExplorerCycleExecutor {
    #[must_use]
    pub fn new(
        authority: AuthorityContext,
        targets: ReadTargetMap,
        checks: AcceptanceChecks,
    ) -> Self {
        Self {
            explorer: ReadOnlyExplorerExecutor::new(authority.clone(), targets.clone()),
            authority,
            targets,
            checks,
            max_read_bytes: DEFAULT_MAX_READ_BYTES,
        }
    }

    /// Hands successful read texts to `cache` (see
    /// [`ReadOnlyExplorerExecutor::with_cache`]).
    #[must_use]
    pub fn with_cache(mut self, cache: ExplorerReadCache) -> Self {
        // `ReadOnlyExplorerExecutor::with_cache` consumes the executor, so the
        // field is swapped for an equivalent fresh one and the original is
        // rebuilt with the cache. The fresh stand-in is dropped immediately.
        let explorer = std::mem::replace(
            &mut self.explorer,
            ReadOnlyExplorerExecutor::new(self.authority.clone(), self.targets.clone()),
        );
        self.explorer = explorer.with_cache(cache);
        self
    }

    /// Plans a verification: the target ids bound to the evidence's locator
    /// that both the step admission and at least one check name.
    fn verify_plan(
        &self,
        evidence_id: &str,
        admission: &CycleAdmission,
        state: &CycleCheckpoint,
    ) -> Result<VerifyPlan, StepFailure> {
        if !self
            .authority
            .permissions()
            .contains(Permission::ReadWorkspace)
        {
            return Err(StepFailure::new("ReadWorkspace permission missing"));
        }
        let record = state
            .evidence
            .get(evidence_id)
            .ok_or_else(|| StepFailure::new("verify names unknown evidence"))?;
        if record.source != EvidenceSourceKind::Repository {
            return Err(StepFailure::new(
                "only repository evidence can be verified by the content verifier",
            ));
        }
        // Only checks whose target resolves to exactly the evidence's file
        // are evaluated: a verdict about another file can never cover a
        // criterion through this evidence. The explorer records the
        // normalised path as locator, so compare normalised paths.
        let mut checks = Vec::new();
        let mut bound = false;
        for target in admission.allowed_read_targets() {
            let same_file = self
                .targets
                .path(target)
                .is_some_and(|path| normalized(path) == record.locator);
            if !same_file {
                continue;
            }
            bound = true;
            for (index, criterion, check) in self.checks.for_target(target) {
                checks.push((index, criterion.clone(), check.clone()));
            }
        }
        if !bound {
            return Err(StepFailure::new(
                "evidence locator is not an admitted read target",
            ));
        }
        if checks.is_empty() {
            return Err(StepFailure::new(
                "no acceptance check is bound to this evidence",
            ));
        }
        Ok(VerifyPlan {
            evidence_id: evidence_id.to_owned(),
            locator: record.locator.clone(),
            observed: record.digest,
            checks,
        })
    }
}

struct VerifyPlan {
    evidence_id: String,
    locator: String,
    observed: ContentDigest,
    checks: Vec<(usize, String, ContentCheck)>,
}

impl CycleStepExecutor for ExplorerCycleExecutor {
    fn execute(
        &mut self,
        step: &InFlightStep,
        admission: &CycleAdmission,
        state: &CycleCheckpoint,
        cancel: &CancelToken,
    ) -> impl Future<Output = Result<CycleObservations, StepFailure>> + Send {
        let verify = match &step.proposal {
            CycleProposal::Verify { evidence_id } => {
                Some(self.verify_plan(evidence_id, admission, state))
            }
            _ => None,
        };
        let root = self.authority.workspace().canonical_root().to_path_buf();
        let max_read = self.max_read_bytes;
        let read = verify
            .is_none()
            .then(|| self.explorer.execute(step, admission, state, cancel));
        let cancel = cancel.clone();
        async move {
            let Some(plan) = verify else {
                return match read {
                    Some(read) => read.await,
                    None => Err(StepFailure::new("explorer step unavailable")),
                };
            };
            let plan = plan?;
            if cancel.is_cancelled() {
                return Err(StepFailure::new("verify step cancelled"));
            }
            let locator = plan.locator.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                let scope = Arc::new(
                    Scope::new(&root)
                        .map_err(|error| format!("workspace root not readable: {error}"))?,
                );
                read_beneath(&scope, &locator, max_read, max_read)
            })
            .await
            .map_err(|error| StepFailure::new(format!("verify task failed: {error}")))?
            .map_err(|reason| StepFailure::new(format!("verify read failed: {reason}")))?;
            Ok(judge(&plan, outcome.digest, &outcome.text))
        }
    }

    async fn reconcile(&mut self, _step: &InFlightStep) -> StepReconciliation {
        // Reads and verifications have no external effect; re-asking is safe
        // and never yields a verdict over content that may have changed.
        StepReconciliation::NotStarted
    }

    fn rebind_authority(&mut self, authority: &AuthorityContext) {
        self.explorer.rebind_authority(authority);
        self.authority = authority.clone();
    }
}

/// Turns one fresh read into observations (pure).
///
/// The evidence id is a function of the check and the verified content, not
/// of the step: re-verifying unchanged content re-reports the same record,
/// which neither conflicts nor counts as progress, so the model cannot reset
/// the stall counter by verifying in a loop. Changed content yields a new id
/// (and never a `ConflictingEvidence`).
fn judge(plan: &VerifyPlan, current: ContentDigest, text: &str) -> CycleObservations {
    let mut observations = CycleObservations::default();
    if current != plan.observed {
        observations.claims.insert(
            format!("stale:{}", plan.evidence_id),
            "file changed since it was observed; read it again before verifying".to_owned(),
        );
        return observations;
    }
    for (index, criterion, check) in &plan.checks {
        match check.evaluate(text) {
            Ok(()) => {
                let id = format!("check:{index}:{current}");
                let digest = ContentDigest::of(
                    format!("harw-cycle-check\0{criterion}\0{current}").as_bytes(),
                );
                observations.evidence.push(EvidenceRecord {
                    id: id.clone(),
                    source: EvidenceSourceKind::Verification,
                    locator: format!("check:{index}@{}", plan.locator),
                    digest,
                    trust: EvidenceTrust::Verified,
                });
                observations.criteria.insert(criterion.clone(), id);
            }
            Err(reason) => {
                observations.claims.insert(format!("unmet:{index}"), reason);
            }
        }
    }
    observations
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{TenantId, WorkspaceId};

    use super::*;
    use crate::intent_cycle::{CycleLimits, CycleTargets, IntentBinding};
    use crate::test_support::{TestError, TestResult, ctx};

    struct Workspace {
        dir: tempfile::TempDir,
    }

    impl Workspace {
        fn new() -> TestResult<Self> {
            let dir = tempfile::Builder::new()
                .prefix("verifier-ws-")
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
            let tenant = TenantId::from_str("verifier-tenant");
            let workspace = WorkspaceId::from_str("verifier-tests");
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

    fn check(target: &str, contains: &[&str], forbids: &[&str]) -> ContentCheck {
        ContentCheck {
            target: target.to_owned(),
            must_contain: contains.iter().map(|s| (*s).to_owned()).collect(),
            must_not_contain: forbids.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn admission() -> TestResult<CycleAdmission> {
        CycleAdmission::new(
            IntentBinding {
                id: "intent-verify".to_owned(),
                revision: 1,
                digest: ContentDigest::of(b"intent-verify@1"),
                predecessor: None,
                acceptance: set(&["has-marker"]),
            },
            CycleLimits {
                max_transitions: 8,
                max_chain_depth: 1,
                max_spawn_depth: 1,
                max_parallel_segments: 2,
                max_stall_transitions: 3,
            },
            CycleTargets {
                read: set(&["lib"]),
                ..CycleTargets::default()
            },
        )
        .map_err(|_| TestError::Missing("admission builds"))
    }

    fn step(proposal: CycleProposal) -> InFlightStep {
        InFlightStep {
            epoch: 1,
            sequence: 1,
            idempotency_key: "cycle-v-1".to_owned(),
            proposal,
        }
    }

    fn observed(state: &mut CycleCheckpoint, content: &[u8]) {
        state.evidence.insert(
            "cycle-v-0:lib".to_owned(),
            EvidenceRecord {
                id: "cycle-v-0:lib".to_owned(),
                source: EvidenceSourceKind::Repository,
                locator: "src/lib.rs".to_owned(),
                digest: ContentDigest::of(content),
                trust: EvidenceTrust::Observed,
            },
        );
    }

    fn verify() -> CycleProposal {
        CycleProposal::Verify {
            evidence_id: "cycle-v-0:lib".to_owned(),
        }
    }

    fn fixture(
        content: &[u8],
        checks: BTreeMap<String, ContentCheck>,
    ) -> TestResult<(
        Workspace,
        ExplorerCycleExecutor,
        CycleAdmission,
        CycleCheckpoint,
    )> {
        let ws = Workspace::new()?;
        ws.write("src/lib.rs", content)?;
        let authority = ws.authority(&[Permission::ReadWorkspace])?;
        let targets = ReadTargetMap::new([("lib", "src/lib.rs")]).map_err(ctx("map"))?;
        let checks = AcceptanceChecks::new(checks, &targets).map_err(ctx("checks"))?;
        let adm = admission()?;
        let state = CycleCheckpoint::initial(&adm, &authority, 1);
        let executor = ExplorerCycleExecutor::new(authority, targets, checks);
        Ok((ws, executor, adm, state))
    }

    #[test]
    fn checks_are_validated_against_targets_and_intent() -> TestResult {
        let targets = ReadTargetMap::new([("lib", "src/lib.rs")]).map_err(ctx("map"))?;
        let unknown = BTreeMap::from([("c".to_owned(), check("nope", &["x"], &[]))]);
        assert!(matches!(
            AcceptanceChecks::new(unknown, &targets),
            Err(CheckError::UnknownTarget { .. })
        ));
        let empty = BTreeMap::from([("c".to_owned(), check("lib", &[], &[]))]);
        assert!(matches!(
            AcceptanceChecks::new(empty, &targets),
            Err(CheckError::InvalidNeedles { .. })
        ));
        let blank = BTreeMap::from([("c".to_owned(), check("lib", &["  "], &[]))]);
        assert!(matches!(
            AcceptanceChecks::new(blank, &targets),
            Err(CheckError::InvalidNeedles { .. })
        ));
        let ok = AcceptanceChecks::new(
            BTreeMap::from([("c".to_owned(), check("lib", &["x"], &[]))]),
            &targets,
        )
        .map_err(ctx("valid"))?;
        assert!(ok.ensure_covers(&set(&["c"])).is_ok());
        assert!(matches!(
            ok.ensure_covers(&set(&["c", "d"])),
            Err(CheckError::UncheckedCriterion { .. })
        ));
        assert!(matches!(
            ok.ensure_covers(&set(&["d"])),
            Err(CheckError::UncheckedCriterion { .. })
        ));
        assert!(matches!(
            ok.ensure_covers(&BTreeSet::new()),
            Err(CheckError::UnknownCriterion { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn repeated_verification_is_idempotent_and_not_false_progress() -> TestResult {
        let content = b"pub fn marker() {}\n";
        let checks = BTreeMap::from([("has-marker".to_owned(), check("lib", &["fn marker"], &[]))]);
        let (_ws, mut executor, adm, mut state) = fixture(content, checks)?;
        observed(&mut state, content);
        let first = executor
            .execute(&step(verify()), &adm, &state, &CancelToken::new())
            .await
            .map_err(|failure| TestError::Unexpected(failure.reason))?;
        let mut later = step(verify());
        later.sequence = 2;
        later.idempotency_key = "cycle-v-2".to_owned();
        let second = executor
            .execute(&later, &adm, &state, &CancelToken::new())
            .await
            .map_err(|failure| TestError::Unexpected(failure.reason))?;
        assert_eq!(first, second);
        Ok(())
    }

    #[test]
    fn locator_comparison_ignores_path_spelling() {
        assert_eq!(normalized("src//lib.rs"), "src/lib.rs");
        assert_eq!(normalized("src/lib.rs/"), "src/lib.rs");
        assert_eq!(normalized("src/lib.rs"), "src/lib.rs");
    }

    #[tokio::test]
    async fn passing_check_yields_verified_evidence_covering_the_criterion() -> TestResult {
        let content = b"pub fn marker() {}\n";
        let checks = BTreeMap::from([(
            "has-marker".to_owned(),
            check("lib", &["fn marker"], &["todo!()"]),
        )]);
        let (_ws, mut executor, adm, mut state) = fixture(content, checks)?;
        observed(&mut state, content);
        let observations = executor
            .execute(&step(verify()), &adm, &state, &CancelToken::new())
            .await
            .map_err(|failure| TestError::Unexpected(failure.reason))?;
        let id = observations
            .criteria
            .get("has-marker")
            .ok_or(TestError::Missing("criterion covered"))?;
        let record = observations
            .evidence
            .iter()
            .find(|record| &record.id == id)
            .ok_or(TestError::Missing("verification evidence"))?;
        assert_eq!(record.trust, EvidenceTrust::Verified);
        assert_eq!(record.source, EvidenceSourceKind::Verification);
        assert_ne!(record.digest, ContentDigest::of(content));
        Ok(())
    }

    #[tokio::test]
    async fn failing_check_is_an_unmet_claim_not_evidence() -> TestResult {
        let content = b"pub fn marker() { todo!() }\n";
        let checks = BTreeMap::from([(
            "has-marker".to_owned(),
            check("lib", &["fn marker"], &["todo!()"]),
        )]);
        let (_ws, mut executor, adm, mut state) = fixture(content, checks)?;
        observed(&mut state, content);
        let observations = executor
            .execute(&step(verify()), &adm, &state, &CancelToken::new())
            .await
            .map_err(|failure| TestError::Unexpected(failure.reason))?;
        assert!(observations.evidence.is_empty());
        assert!(observations.criteria.is_empty());
        assert!(observations.claims.contains_key("unmet:0"));
        Ok(())
    }

    #[tokio::test]
    async fn changed_file_is_stale_and_never_verified() -> TestResult {
        let checks = BTreeMap::from([("has-marker".to_owned(), check("lib", &["fn marker"], &[]))]);
        let (_ws, mut executor, adm, mut state) = fixture(b"pub fn marker() {}\n", checks)?;
        observed(&mut state, b"an older version");
        let observations = executor
            .execute(&step(verify()), &adm, &state, &CancelToken::new())
            .await
            .map_err(|failure| TestError::Unexpected(failure.reason))?;
        assert!(observations.criteria.is_empty());
        assert!(observations.claims.contains_key("stale:cycle-v-0:lib"));
        Ok(())
    }

    #[tokio::test]
    async fn verify_needs_read_permission_and_repository_evidence() -> TestResult {
        let content = b"pub fn marker() {}\n";
        let checks = BTreeMap::from([("has-marker".to_owned(), check("lib", &["marker"], &[]))]);
        let (ws, _executor, adm, mut state) = fixture(content, checks.clone())?;
        observed(&mut state, content);
        let targets = ReadTargetMap::new([("lib", "src/lib.rs")]).map_err(ctx("map"))?;
        let no_read = ws.authority(&[])?;
        let mut denied = ExplorerCycleExecutor::new(
            no_read,
            targets.clone(),
            AcceptanceChecks::new(checks, &targets).map_err(ctx("checks"))?,
        );
        assert!(
            denied
                .execute(&step(verify()), &adm, &state, &CancelToken::new())
                .await
                .is_err()
        );
        if let Some(record) = state.evidence.get_mut("cycle-v-0:lib") {
            record.source = EvidenceSourceKind::Tool;
        }
        let (_ws2, mut executor, _, _) = fixture(
            content,
            BTreeMap::from([("has-marker".to_owned(), check("lib", &["marker"], &[]))]),
        )?;
        assert!(
            executor
                .execute(&step(verify()), &adm, &state, &CancelToken::new())
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn other_proposals_keep_the_explorer_contract() -> TestResult {
        let checks = BTreeMap::from([("has-marker".to_owned(), check("lib", &["marker"], &[]))]);
        let (_ws, mut executor, adm, state) = fixture(b"pub fn marker() {}\n", checks)?;
        let patch = CycleProposal::ProposePatch {
            targets: set(&["lib"]),
            evidence_id: "x".to_owned(),
        };
        assert!(
            executor
                .execute(&step(patch), &adm, &state, &CancelToken::new())
                .await
                .is_err()
        );
        let read = CycleProposal::Advance {
            segments: vec![crate::intent_cycle::Segment::Read {
                target: "lib".to_owned(),
            }],
        };
        let observations = executor
            .execute(&step(read), &adm, &state, &CancelToken::new())
            .await
            .map_err(|failure| TestError::Unexpected(failure.reason))?;
        assert_eq!(observations.evidence.len(), 1);
        Ok(())
    }
}
