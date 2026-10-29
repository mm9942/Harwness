//! Restricted source-collection progress. Receipts are provenance, never authority.
use crate::{
    actions::{NodePatch, PlanAction, PlanEvent},
    config::PlanToolConfig,
    error::{PlanError, PlanResult},
    ids::{PathOrSymbol, PlanId, RevisionId, TaskId},
    types::{
        Assignment, EvidenceKind, EvidenceRef, Plan, PlanNodeKind, PlanNodeStatus, VerificationStep,
    },
};
use harw_types::ContentDigest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use time::OffsetDateTime;

/// Immutable parent admission, loaded by the runtime from the stored job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParentAdmission {
    pub parent_work_id: String,
    pub plan_id: PlanId,
    pub revision: RevisionId,
    pub snapshot_digest: ContentDigest,
}
/// Runtime-only request: deliberately not deserializable from model input.
#[derive(Debug, Clone)]
pub struct CollectSourcesProgress {
    pub expected_revision: RevisionId,
    pub expected_chain_head: Option<ContentDigest>,
    pub admission: ParentAdmission,
    pub node_id: TaskId,
    pub attempt: u32,
    pub required_sources: Vec<String>,
    pub contract_digest: ContentDigest,
    pub artifact_digest: ContentDigest,
    pub publishing_epoch: u64,
    pub operation_id: String,
}
/// Checksummed WAL metadata. These data values grant no permission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParentProgressReceipt {
    pub schema_version: u32,
    pub admission: ParentAdmission,
    pub plan_id: PlanId,
    pub node_id: TaskId,
    pub attempt: u32,
    pub required_sources: Vec<String>,
    pub contract_digest: ContentDigest,
    pub artifact_digest: ContentDigest,
    pub publishing_epoch: u64,
    pub operation_id: String,
    pub before_revision: RevisionId,
    pub after_revision: RevisionId,
    pub before_digest: ContentDigest,
    pub after_digest: ContentDigest,
    #[serde(deserialize_with = "required_option")]
    pub previous_chain_head: Option<ContentDigest>,
}
fn required_option<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ContentDigest>, D::Error> {
    Option::<ContentDigest>::deserialize(deserializer)
}
/// Opaque atomic replay result; callers cannot construct an unchecked view.
#[derive(Debug, Clone)]
pub struct ParentProgressView {
    admitted: Plan,
    current: Plan,
    chain_head: Option<ContentDigest>,
}
impl ParentProgressView {
    pub fn admitted_plan(&self) -> &Plan {
        &self.admitted
    }
    pub fn plan(&self) -> &Plan {
        &self.current
    }
    pub fn chain_head(&self) -> Option<&ContentDigest> {
        self.chain_head.as_ref()
    }
}
pub(crate) fn invalid(message: &'static str) -> PlanError {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message).into()
}
/// Exact same domain as WorkDriver admission; serialization failures fail closed.
pub fn snapshot_digest(plan: &Plan) -> PlanResult<ContentDigest> {
    let mut bytes = b"harw/work-driver/admitted-plan/v1\0".to_vec();
    serde_json::to_writer(&mut bytes, plan)?;
    Ok(ContentDigest::of(&bytes))
}
impl ParentProgressReceipt {
    pub fn chain_digest(&self) -> PlanResult<ContentDigest> {
        let mut bytes = b"harw/parent-progress/v1\0".to_vec();
        serde_json::to_writer(&mut bytes, self)?;
        Ok(ContentDigest::of(&bytes))
    }
    fn request(&self) -> CollectSourcesProgress {
        CollectSourcesProgress {
            expected_revision: self.before_revision,
            expected_chain_head: self.previous_chain_head,
            admission: self.admission.clone(),
            node_id: self.node_id.clone(),
            attempt: self.attempt,
            required_sources: self.required_sources.clone(),
            contract_digest: self.contract_digest,
            artifact_digest: self.artifact_digest,
            publishing_epoch: self.publishing_epoch,
            operation_id: self.operation_id.clone(),
        }
    }
}
fn actions(
    plan: &Plan,
    request: &CollectSourcesProgress,
    now: OffsetDateTime,
) -> PlanResult<Vec<PlanAction>> {
    if request.operation_id.is_empty()
        || request.operation_id.len() > 256
        || request.admission.parent_work_id.is_empty()
        || request.attempt != 1
    {
        return Err(invalid("Invalid parent collection identity or attempt"));
    }
    let node = plan
        .nodes
        .iter()
        .find(|node| node.id == request.node_id)
        .ok_or(PlanError::PlanNotFound)?;
    if !matches!(node.kind, PlanNodeKind::Research | PlanNodeKind::Explore)
        || node.status != PlanNodeStatus::Ready
        || node.assignment.is_some()
        || !node.write_scope.is_empty()
        || node.acceptance_criteria.is_empty()
        || !node.evidence.is_empty()
        || !node.output_contracts.is_empty()
    {
        return Err(invalid(
            "Node is not an unassigned ready collection prerequisite",
        ));
    }
    let mut sources = BTreeSet::new();
    for criterion in &node.acceptance_criteria {
        if criterion.verification.is_empty() {
            return Err(invalid("Collection criterion has no verifier"));
        }
        for step in &criterion.verification {
            match step {
                VerificationStep::CollectSources { sources: paths } if !paths.is_empty() => {
                    sources.extend(paths.iter().cloned())
                }
                _ => {
                    return Err(invalid(
                        "Semantic criteria cannot be completed by collection",
                    ));
                }
            }
        }
    }
    let read: Option<BTreeSet<String>> = node
        .read_scope
        .iter()
        .map(|scope| match scope {
            PathOrSymbol::Path(path) => Some(path.clone()),
            _ => None,
        })
        .collect();
    if read.as_ref() != Some(&sources)
        || request.required_sources != sources.into_iter().collect::<Vec<_>>()
    {
        return Err(invalid(
            "Collection sources disagree with the admitted node",
        ));
    }
    let actor = format!("parent:{}", request.admission.parent_work_id);
    let mut actions = vec![
        PlanAction::UpdateNode {
            id: node.id.clone(),
            patch: NodePatch {
                assignment: Some(Some(Assignment {
                    worker: actor.clone(),
                    attempt: request.attempt,
                    job: Some(request.admission.parent_work_id.clone()),
                })),
                ..NodePatch::default()
            },
        },
        PlanAction::SetStatus {
            id: node.id.clone(),
            status: PlanNodeStatus::InProgress,
            reason: None,
        },
        PlanAction::AttachEvidence {
            id: node.id.clone(),
            evidence: EvidenceRef {
                kind: EvidenceKind::Finding,
                locator: format!("parent-collection:{}", request.artifact_digest),
                attached_at: now,
                actor,
                digest: Some(request.artifact_digest),
            },
        },
        PlanAction::SetStatus {
            id: node.id.clone(),
            status: PlanNodeStatus::Completed,
            reason: None,
        },
    ];
    // The admitted graph already contains these scopes and criteria. Only
    // readiness changes: no task synthesis, scope widening or semantic acceptance.
    for dependent in &plan.nodes {
        if dependent.status == PlanNodeStatus::Draft
            && dependent.assignment.is_none()
            && matches!(
                dependent.kind,
                PlanNodeKind::Coding | PlanNodeKind::Integration | PlanNodeKind::Docs
            )
            && dependent.dependencies.contains(&node.id)
            && dependent.dependencies.iter().all(|id| {
                *id == node.id
                    || plan
                        .nodes
                        .iter()
                        .any(|other| other.id == *id && other.status == PlanNodeStatus::Completed)
            })
        {
            actions.push(PlanAction::SetStatus {
                id: dependent.id.clone(),
                status: PlanNodeStatus::Ready,
                reason: None,
            });
        }
    }
    Ok(actions)
}
fn check_revision_capacity(revision: RevisionId, count: usize) -> PlanResult<()> {
    let count = u64::try_from(count).map_err(|_| invalid("Parent progress revision overflow"))?;
    revision
        .value()
        .checked_add(count)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| invalid("Parent progress revision overflow"))?;
    Ok(())
}
pub(crate) fn reconcile(
    bootstrap: Option<&Plan>,
    history: &[PlanEvent],
    receipts: &[ParentProgressReceipt],
    current: &Plan,
    admission: &ParentAdmission,
    config: &PlanToolConfig,
) -> PlanResult<ParentProgressView> {
    let mut replay = bootstrap
        .cloned()
        .ok_or_else(|| invalid("Parent progress requires complete bootstrap history"))?;
    if replay.id != admission.plan_id
        || current.id != admission.plan_id
        || admission.revision < replay.revision
    {
        return Err(invalid("Parent admission identity mismatch"));
    }
    for event in history
        .iter()
        .filter(|event| event.revision > RevisionId::new(1) && event.revision <= admission.revision)
    {
        if Some(event.revision.value()) != replay.revision.value().checked_add(1) {
            return Err(invalid("Missing admitted history"));
        }
        crate::mutation::apply_mutation(&mut replay, &event.action, &event.actor, event.applied_at);
        replay.revision = event.revision;
        replay.updated_at = event.applied_at;
    }
    if replay.revision != admission.revision
        || snapshot_digest(&replay)? != admission.snapshot_digest
    {
        return Err(invalid("Admitted snapshot does not match history"));
    }
    let admitted = replay.clone();
    let mut chain = None;
    let mut seen = BTreeSet::new();
    for receipt in receipts
        .iter()
        .filter(|receipt| receipt.after_revision > admission.revision)
    {
        if receipt.schema_version != 1
            || receipt.admission != *admission
            || receipt.plan_id != admission.plan_id
            || receipt.before_revision != replay.revision
            || receipt.before_digest != snapshot_digest(&replay)?
            || receipt.previous_chain_head != chain
            || !seen.insert(receipt.operation_id.clone())
        {
            return Err(invalid("Invalid parent progress chain"));
        }
        let events: Vec<_> = history
            .iter()
            .filter(|event| {
                event.revision > receipt.before_revision && event.revision <= receipt.after_revision
            })
            .cloned()
            .collect();
        let first = events
            .first()
            .ok_or_else(|| invalid("Receipt has no events"))?;
        let expected = actions(&replay, &receipt.request(), first.applied_at)?;
        check_revision_capacity(replay.revision, expected.len())?;
        let (candidate, derived) = crate::store::stage_actions(
            &replay,
            expected,
            &format!("parent:{}", admission.parent_work_id),
            config,
            replay.revision.next(),
            first.applied_at,
        )
        .map_err(|(_, error)| error)?;
        if serde_json::to_vec(&derived)? != serde_json::to_vec(&events)?
            || candidate.revision != receipt.after_revision
            || snapshot_digest(&candidate)? != receipt.after_digest
        {
            return Err(invalid(
                "Receipt does not describe restricted collection transition",
            ));
        }
        replay = candidate;
        chain = Some(receipt.chain_digest()?);
    }
    if snapshot_digest(&replay)? != snapshot_digest(current)? {
        return Err(invalid(
            "Unreceipted or foreign plan progress after admission",
        ));
    }
    Ok(ParentProgressView {
        admitted,
        current: replay,
        chain_head: chain,
    })
}
pub(crate) enum StagedProgress {
    Existing(ParentProgressReceipt),
    New(Plan, Vec<PlanEvent>, ParentProgressReceipt),
}
pub(crate) fn stage(
    view: &ParentProgressView,
    receipts: &[ParentProgressReceipt],
    request: &CollectSourcesProgress,
    config: &PlanToolConfig,
) -> PlanResult<StagedProgress> {
    if let Some(receipt) = receipts.iter().find(|receipt| {
        receipt.operation_id == request.operation_id && receipt.admission == request.admission
    }) {
        let old = receipt.request();
        if old.node_id != request.node_id
            || old.attempt != request.attempt
            || old.required_sources != request.required_sources
            || old.contract_digest != request.contract_digest
            || old.artifact_digest != request.artifact_digest
        {
            return Err(invalid("Conflicting parent progress operation reuse"));
        }
        return Ok(StagedProgress::Existing(receipt.clone()));
    }
    let now = OffsetDateTime::now_utc();
    let current = view.plan();
    if request.expected_revision != current.revision
        || request.expected_chain_head != view.chain_head
    {
        return Err(invalid("Parent progress chain tip changed"));
    }
    let derived = actions(current, request, now)?;
    check_revision_capacity(current.revision, derived.len())?;
    let (candidate, events) = crate::store::stage_actions(
        current,
        derived,
        &format!("parent:{}", request.admission.parent_work_id),
        config,
        current.revision.next(),
        now,
    )
    .map_err(|(_, error)| error)?;
    let receipt = ParentProgressReceipt {
        schema_version: 1,
        admission: request.admission.clone(),
        plan_id: current.id.clone(),
        node_id: request.node_id.clone(),
        attempt: request.attempt,
        required_sources: request.required_sources.clone(),
        contract_digest: request.contract_digest,
        artifact_digest: request.artifact_digest,
        publishing_epoch: request.publishing_epoch,
        operation_id: request.operation_id.clone(),
        before_revision: current.revision,
        after_revision: candidate.revision,
        before_digest: snapshot_digest(current)?,
        after_digest: snapshot_digest(&candidate)?,
        previous_chain_head: view.chain_head,
    };
    Ok(StagedProgress::New(candidate, events, receipt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;
    use crate::{FilePlanStore, InMemoryPlanStore, PlanStore, types::Criterion};
    fn fixture(store: &dyn PlanStore) -> PlanResult<CollectSourcesProgress> {
        store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("collection"),
                goal: "collect".into(),
            },
            "runtime",
        )?;
        let mut node = crate::testing::base_node("read-doc");
        node.kind = PlanNodeKind::Explore;
        node.write_scope.clear();
        node.read_scope = vec![PathOrSymbol::Path("README.md".into())];
        node.acceptance_criteria = vec![Criterion {
            description: "Collect README".into(),
            verification: vec![VerificationStep::CollectSources {
                sources: vec!["README.md".into()],
            }],
        }];
        store.apply(PlanAction::AddNode { node }, "runtime")?;
        store.apply(
            PlanAction::SetStatus {
                id: TaskId::new("read-doc"),
                status: PlanNodeStatus::Ready,
                reason: None,
            },
            "runtime",
        )?;
        let plan = store.current()?;
        Ok(CollectSourcesProgress {
            expected_revision: plan.revision,
            expected_chain_head: None,
            admission: ParentAdmission {
                parent_work_id: "work-1".into(),
                plan_id: plan.id.clone(),
                revision: plan.revision,
                snapshot_digest: snapshot_digest(&plan)?,
            },
            node_id: TaskId::new("read-doc"),
            attempt: 1,
            required_sources: vec!["README.md".into()],
            contract_digest: ContentDigest::of(b"contract"),
            artifact_digest: ContentDigest::of(b"artifact"),
            publishing_epoch: 1,
            operation_id: "collect-read-doc".into(),
        })
    }
    fn success(store: &dyn PlanStore) -> TestResult {
        let mut request = fixture(store)?;
        let before = store.current()?.revision;
        assert!(
            store
                .commit_parent_progress_checked(&request, &|| Err(invalid("expired")))
                .is_err()
        );
        assert_eq!(store.current()?.revision, before);
        let receipt = store.commit_parent_progress_checked(&request, &|| Ok(()))?;
        assert_eq!(receipt.after_revision.value(), before.value() + 4);
        let view = store.reconcile_parent_progress(&request.admission)?;
        assert_eq!(view.admitted_plan().revision, before);
        assert_eq!(view.plan().nodes[0].status, PlanNodeStatus::Completed);
        request.publishing_epoch = 2;
        assert_eq!(
            store
                .commit_parent_progress_checked(&request, &|| Err(invalid("expired duplicate")))?,
            receipt
        );
        request.artifact_digest = ContentDigest::of(b"other");
        assert!(
            store
                .commit_parent_progress_checked(&request, &|| Ok(()))
                .is_err()
        );
        store.apply(
            PlanAction::BindGoal {
                goal_id: "changed".into(),
            },
            "runtime",
        )?;
        assert!(store.reconcile_parent_progress(&request.admission).is_err());
        Ok(())
    }
    #[test]
    fn memory_progress_is_atomic_owned_and_idempotent() -> TestResult {
        success(&InMemoryPlanStore::new())
    }
    #[test]
    fn file_progress_is_atomic_owned_and_idempotent() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = FilePlanStore::new(dir.path())?;
        success(&store)
    }
    #[test]
    fn restart_replays_receipt_and_rejects_foreign_parent() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = FilePlanStore::new(dir.path())?;
        let request = fixture(&store)?;
        let receipt = store.commit_parent_progress_checked(&request, &|| Ok(()))?;
        drop(store);
        let store = FilePlanStore::new(dir.path())?;
        assert_eq!(
            store
                .reconcile_parent_progress(&request.admission)?
                .plan()
                .revision,
            receipt.after_revision
        );
        let mut foreign = request.admission.clone();
        foreign.parent_work_id = "foreign".into();
        assert!(store.reconcile_parent_progress(&foreign).is_err());
        Ok(())
    }
    #[test]
    fn ordinary_edit_revert_is_not_trusted_progress() -> TestResult {
        let store = InMemoryPlanStore::new();
        let request = fixture(&store)?;
        for objective in ["changed", "test objective"] {
            store.apply(
                PlanAction::UpdateNode {
                    id: request.node_id.clone(),
                    patch: NodePatch {
                        objective: Some(objective.into()),
                        ..NodePatch::default()
                    },
                },
                "parent:work-1",
            )?;
        }
        assert!(store.reconcile_parent_progress(&request.admission).is_err());
        Ok(())
    }
    #[test]
    fn source_mismatch_and_semantic_criteria_never_complete() -> TestResult {
        let store = InMemoryPlanStore::new();
        let mut request = fixture(&store)?;
        request.required_sources = vec!["secret".into()];
        assert!(
            store
                .commit_parent_progress_checked(&request, &|| Ok(()))
                .is_err()
        );
        assert_eq!(store.current()?.revision, request.admission.revision);
        Ok(())
    }
    #[test]
    fn semantic_criterion_stays_pending_even_with_current_admission() -> TestResult {
        let store = InMemoryPlanStore::new();
        let mut request = fixture(&store)?;
        store.apply(
            PlanAction::UpdateNode {
                id: request.node_id.clone(),
                patch: NodePatch {
                    acceptance_criteria: Some(vec![Criterion {
                        description: "Understand implications".into(),
                        verification: vec![VerificationStep::Manual {
                            note: "review".into(),
                        }],
                    }]),
                    ..NodePatch::default()
                },
            },
            "runtime",
        )?;
        let plan = store.current()?;
        request.admission.revision = plan.revision;
        request.admission.snapshot_digest = snapshot_digest(&plan)?;
        request.expected_revision = plan.revision;
        assert!(
            store
                .commit_parent_progress_checked(&request, &|| Ok(()))
                .is_err()
        );
        assert_eq!(store.current()?.nodes[0].status, PlanNodeStatus::Ready);
        Ok(())
    }
    #[test]
    fn stale_expected_tip_and_missing_chain_field_are_rejected() -> TestResult {
        let store = InMemoryPlanStore::new();
        let mut request = fixture(&store)?;
        request.expected_chain_head = Some(ContentDigest::of(b"foreign"));
        assert!(
            store
                .commit_parent_progress_checked(&request, &|| Ok(()))
                .is_err()
        );
        request.expected_chain_head = None;
        let receipt = store.commit_parent_progress_checked(&request, &|| Ok(()))?;
        let mut json = serde_json::to_value(receipt)?;
        json.as_object_mut()
            .ok_or(crate::test_support::TestError::Missing("receipt object"))?
            .remove("previous_chain_head");
        assert!(serde_json::from_value::<ParentProgressReceipt>(json).is_err());
        Ok(())
    }
    #[test]
    fn scoped_progress_refuses_foreign_tenant() -> TestResult {
        use crate::tenant_scope::ScopedPlanStore;
        let store = InMemoryPlanStore::new();
        let a = ScopedPlanStore::new(&store, Some(harw_types::TenantId::from_str("a")));
        let b = ScopedPlanStore::new(&store, Some(harw_types::TenantId::from_str("b")));
        let request = fixture(&a)?;
        assert!(b.reconcile_parent_progress(&request.admission).is_err());
        assert!(
            b.commit_parent_progress_checked(&request, &|| Ok(()))
                .is_err()
        );
        a.commit_parent_progress_checked(&request, &|| Ok(()))?;
        assert!(a.reconcile_parent_progress(&request.admission).is_ok());
        Ok(())
    }
    #[test]
    fn second_store_refreshes_receipts_and_missing_wal_blocks_replay() -> TestResult {
        let dir = tempfile::tempdir()?;
        let first = FilePlanStore::new(dir.path())?;
        let request = fixture(&first)?;
        let second = FilePlanStore::new(dir.path())?;
        let receipt = first.commit_parent_progress_checked(&request, &|| Ok(()))?;
        assert_eq!(
            second
                .reconcile_parent_progress(&request.admission)?
                .plan()
                .revision,
            receipt.after_revision
        );
        drop(first);
        drop(second);
        std::fs::remove_file(dir.path().join("plans/collection/history.jsonl"))?;
        let legacy = FilePlanStore::new(dir.path())?;
        assert!(
            legacy
                .reconcile_parent_progress(&request.admission)
                .is_err()
        );
        Ok(())
    }
    #[test]
    fn collection_completion_promotes_only_ready_admitted_dependents() -> TestResult {
        let store = InMemoryPlanStore::new();
        let mut request = fixture(&store)?;
        let mut other = crate::testing::base_node("other");
        other.kind = PlanNodeKind::Explore;
        other.write_scope.clear();
        store.apply(PlanAction::AddNode { node: other }, "runtime")?;
        for (id, deps) in [
            ("code", vec![request.node_id.clone()]),
            (
                "blocked-code",
                vec![request.node_id.clone(), TaskId::new("other")],
            ),
        ] {
            let mut node = crate::testing::base_node(id);
            node.dependencies = deps;
            store.apply(PlanAction::AddNode { node }, "runtime")?;
        }
        let plan = store.current()?;
        request.admission.revision = plan.revision;
        request.admission.snapshot_digest = snapshot_digest(&plan)?;
        request.expected_revision = plan.revision;
        let receipt = store.commit_parent_progress_checked(&request, &|| Ok(()))?;
        assert_eq!(receipt.after_revision.value(), plan.revision.value() + 5);
        let view = store.reconcile_parent_progress(&request.admission)?;
        let coding = view
            .plan()
            .nodes
            .iter()
            .find(|node| node.id == TaskId::new("code"))
            .ok_or(crate::test_support::TestError::Missing("coding node"))?;
        assert_eq!(coding.status, PlanNodeStatus::Ready);
        assert_eq!(
            coding.write_scope,
            vec![PathOrSymbol::Path("src/code.rs".into())]
        );
        let blocked = view
            .plan()
            .nodes
            .iter()
            .find(|node| node.id == TaskId::new("blocked-code"))
            .ok_or(crate::test_support::TestError::Missing(
                "blocked coding node",
            ))?;
        assert_eq!(blocked.status, PlanNodeStatus::Draft);
        Ok(())
    }
}
