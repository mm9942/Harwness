//! Typed run observations of the intent cycle (PL-90 W03c-1).
//!
//! A [`RunObservationV1`] is a bounded, text-free record of what happened to
//! one cycle transition: which transition, which category of outcome and
//! which cause code. It deliberately carries no reason and no error text, so
//! nothing a model or an executor wrote can flow into later learning or
//! prompting. The classifiers in this module are pure: they map existing
//! typed refusals and failures to `(category, cause)` pairs and never read
//! free text.
//!
//! Design: `docs/planning/90-intent-driven-composed-agents/
//! 05-error-learning-and-run-isolation.md`, sections 3 and 3.1.

use harw_types::ContentDigest;
use serde::{Deserialize, Serialize};

use crate::cycle_runtime::{StepFailure, StepReconciliation};
use crate::intent_cycle::{
    CheckpointFence, CycleCheckpoint, CycleObservations, CycleProposal, CycleRefusal, FenceRefusal,
    is_prompt_safe_char,
};

/// Schema version written into every [`RunObservationV1`].
pub const OBSERVATION_SCHEMA: u32 = 1;

/// Maximum number of evidence ids kept on one observation.
const MAX_EVIDENCE_IDS: usize = 8;

/// Maximum length (in characters) of one kept evidence id.
const MAX_EVIDENCE_ID_CHARS: usize = 128;

/// Coarse category of what happened to a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationCategory {
    /// Admission refused the proposal; nothing ran.
    AdmissionRefused,
    /// A tool or executor failed and had no effect.
    ToolFailedNoEffect,
    /// A required capability is not available.
    CapabilityUnavailable,
    /// The provider was temporarily unavailable.
    ProviderUnavailable,
    /// Verification ran and failed.
    VerificationFailed,
    /// Recorded evidence contradicted an earlier claim.
    Contradicted,
    /// The effect of a step cannot be established either way.
    UncertainEffect,
    /// A failed situation recovered.
    Recovered,
    /// A fix was validated.
    ValidatedFix,
}

/// Bounded cause code; never free text.
///
/// One variant per [`CycleRefusal`] variant (data-carrying variants would map
/// to a unit variant), plus executor, reconciliation and fence causes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CauseCode {
    InvalidIntent,
    InvalidLimits,
    InvalidTarget,
    UnsupportedSchema,
    AlreadyTerminal,
    Exhausted,
    Stalled,
    EmptyOrOversizedGraph,
    DuplicateSegment,
    InvalidJoin,
    ReadNotAllowed,
    WriteNotAllowed,
    ChildNotAllowed,
    RecipeNotAllowed,
    ChainDepthExceeded,
    SpawnDepthExceeded,
    MissingEvidence,
    UnknownClaim,
    IncompleteAcceptance,
    InvalidReason,
    CeilingMismatch,
    /// The executor failed permanently.
    ExecutorFailed,
    /// The executor failed temporarily and may be retried.
    ExecutorRetryable,
    /// Crash reconciliation could not establish the effect.
    ReconcileUncertain,
    /// A verification check was not met (`unmet:` claim).
    VerificationUnmet,
    /// Verification was refused because the file changed since it was read
    /// (`stale:` claim).
    VerificationStale,
    /// A write came from a superseded lease epoch.
    StaleEpoch,
    /// No mapping fits.
    Unclassified,
}

/// How far the observed transition got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationOutcome {
    Pending,
    Attempted,
    Confirmed,
    Refuted,
    Inconclusive,
}

/// Identity of one observation event inside a cycle.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ObservationEventId {
    pub cycle_id: String,
    pub sequence: u64,
    pub index: u32,
}

impl ObservationEventId {
    /// Stable key for de-duplicating repeated deliveries of one event.
    #[must_use]
    pub fn dedupe_key(&self) -> String {
        format!("{}:{}:{}", self.cycle_id, self.sequence, self.index)
    }
}

/// Typed, text-free record of one cycle transition. It has no reason or error
/// field by design.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunObservationV1 {
    pub schema_version: u32,
    pub event_id: ObservationEventId,
    pub intent_id: String,
    pub intent_revision: u64,
    pub intent_digest: ContentDigest,
    /// Proposal kind (see [`proposal_kind`]).
    pub transition: String,
    pub category: ObservationCategory,
    pub cause: CauseCode,
    pub evidence_ids: Vec<String>,
    pub outcome: ObservationOutcome,
    pub epoch: u64,
}

/// Snake-case variant name of a proposal.
#[must_use]
pub const fn proposal_kind(proposal: &CycleProposal) -> &'static str {
    match proposal {
        CycleProposal::Advance { .. } => "advance",
        CycleProposal::Fork { .. } => "fork",
        CycleProposal::Reorient { .. } => "reorient",
        CycleProposal::Revisit { .. } => "revisit",
        CycleProposal::ProposePatch { .. } => "propose_patch",
        CycleProposal::Verify { .. } => "verify",
        CycleProposal::RequestApproval { .. } => "request_approval",
        CycleProposal::Complete {} => "complete",
        CycleProposal::Wait { .. } => "wait",
        CycleProposal::Escalate { .. } => "escalate",
        CycleProposal::Blocked { .. } => "blocked",
        CycleProposal::Failed { .. } => "failed",
    }
}

/// Classifies an admission refusal.
///
/// Every refusal is [`ObservationCategory::AdmissionRefused`] with the
/// mirrored cause. This includes [`CycleRefusal::Exhausted`] and
/// [`CycleRefusal::Stalled`]: they are budget refusals at admission, not
/// executor failures.
#[must_use]
pub const fn classify_refusal(refusal: CycleRefusal) -> (ObservationCategory, CauseCode) {
    let cause = match refusal {
        CycleRefusal::InvalidIntent => CauseCode::InvalidIntent,
        CycleRefusal::InvalidLimits => CauseCode::InvalidLimits,
        CycleRefusal::InvalidTarget => CauseCode::InvalidTarget,
        CycleRefusal::UnsupportedSchema => CauseCode::UnsupportedSchema,
        CycleRefusal::AlreadyTerminal => CauseCode::AlreadyTerminal,
        CycleRefusal::Exhausted => CauseCode::Exhausted,
        CycleRefusal::Stalled => CauseCode::Stalled,
        CycleRefusal::EmptyOrOversizedGraph => CauseCode::EmptyOrOversizedGraph,
        CycleRefusal::DuplicateSegment => CauseCode::DuplicateSegment,
        CycleRefusal::InvalidJoin => CauseCode::InvalidJoin,
        CycleRefusal::ReadNotAllowed => CauseCode::ReadNotAllowed,
        CycleRefusal::WriteNotAllowed => CauseCode::WriteNotAllowed,
        CycleRefusal::ChildNotAllowed => CauseCode::ChildNotAllowed,
        CycleRefusal::RecipeNotAllowed => CauseCode::RecipeNotAllowed,
        CycleRefusal::ChainDepthExceeded => CauseCode::ChainDepthExceeded,
        CycleRefusal::SpawnDepthExceeded => CauseCode::SpawnDepthExceeded,
        CycleRefusal::MissingEvidence => CauseCode::MissingEvidence,
        CycleRefusal::UnknownClaim => CauseCode::UnknownClaim,
        CycleRefusal::IncompleteAcceptance => CauseCode::IncompleteAcceptance,
        CycleRefusal::InvalidReason => CauseCode::InvalidReason,
        CycleRefusal::CeilingMismatch => CauseCode::CeilingMismatch,
    };
    (ObservationCategory::AdmissionRefused, cause)
}

/// Classifies a proposer or executor failure from its `retryable` flag only;
/// the reason text is never read.
#[must_use]
pub const fn classify_step_failure(failure: &StepFailure) -> (ObservationCategory, CauseCode) {
    if failure.retryable {
        (
            ObservationCategory::ProviderUnavailable,
            CauseCode::ExecutorRetryable,
        )
    } else {
        (
            ObservationCategory::ToolFailedNoEffect,
            CauseCode::ExecutorFailed,
        )
    }
}

/// Classifies a crash reconciliation: only an uncertain effect is an
/// observation; `NotStarted` and `Completed` yield `None`.
#[must_use]
pub const fn classify_reconciliation(
    reconciliation: &StepReconciliation,
) -> Option<(ObservationCategory, CauseCode)> {
    match reconciliation {
        StepReconciliation::Uncertain => Some((
            ObservationCategory::UncertainEffect,
            CauseCode::ReconcileUncertain,
        )),
        StepReconciliation::NotStarted | StepReconciliation::Completed(_) => None,
    }
}

/// Prefix of a verifier claim id for a check that was not met.
const UNMET_CLAIM_PREFIX: &str = "unmet:";

/// Prefix of a verifier claim id for evidence that went stale before it could
/// be verified.
const STALE_CLAIM_PREFIX: &str = "stale:";

/// Classifies the verifier claims of a successful step.
///
/// A claim id starting with `unmet:` is
/// `(VerificationFailed, VerificationUnmet)`; one starting with `stale:` is
/// `(VerificationFailed, VerificationStale)`; any other set of claims yields
/// `None`. Only the claim id prefix is read, never the claim body, so text a
/// model or an executor wrote cannot influence the result. The claims are
/// walked in id order and the first match wins, which makes the result
/// deterministic should both kinds ever be present (`stale:` sorts first).
#[must_use]
pub fn classify_verification(
    observations: &CycleObservations,
) -> Option<(ObservationCategory, CauseCode)> {
    observations.claims.keys().find_map(|id| {
        if id.starts_with(STALE_CLAIM_PREFIX) {
            Some((
                ObservationCategory::VerificationFailed,
                CauseCode::VerificationStale,
            ))
        } else if id.starts_with(UNMET_CLAIM_PREFIX) {
            Some((
                ObservationCategory::VerificationFailed,
                CauseCode::VerificationUnmet,
            ))
        } else {
            None
        }
    })
}

/// Classifies a checkpoint fence refusal. A widened ceiling maps to
/// [`CauseCode::CeilingMismatch`]; refusals without a fitting cause are
/// [`CauseCode::Unclassified`].
#[must_use]
pub const fn classify_fence(refusal: FenceRefusal) -> (ObservationCategory, CauseCode) {
    let cause = match refusal {
        FenceRefusal::StaleEpoch => CauseCode::StaleEpoch,
        FenceRefusal::CeilingWidened => CauseCode::CeilingMismatch,
        FenceRefusal::OutOfOrder | FenceRefusal::IntentMismatch | FenceRefusal::StateTampered => {
            CauseCode::Unclassified
        }
    };
    (ObservationCategory::AdmissionRefused, cause)
}

fn is_valid_evidence_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= MAX_EVIDENCE_ID_CHARS
        && id.chars().all(is_prompt_safe_char)
}

/// Builds an observation for the transition `proposal` of cycle `cycle_id`.
///
/// Intent identity comes from `checkpoint`, sequence and epoch from `fence`.
/// The outcome is `Pending`, or `Inconclusive` for an uncertain effect.
/// Evidence ids that are empty, longer than 128 characters or contain a
/// character outside the prompt-safe set are dropped (never truncated); at
/// most 8 valid ids are kept, in order.
#[must_use]
#[allow(
    clippy::too_many_arguments,
    reason = "trusted builder: every field is a separate identity input; a parameter struct would only hide the same arity"
)]
pub fn build_observation(
    cycle_id: &str,
    fence: CheckpointFence,
    index: u32,
    checkpoint: &CycleCheckpoint,
    proposal: &CycleProposal,
    category: ObservationCategory,
    cause: CauseCode,
    evidence_ids: &[String],
) -> RunObservationV1 {
    let outcome = if category == ObservationCategory::UncertainEffect {
        ObservationOutcome::Inconclusive
    } else {
        ObservationOutcome::Pending
    };
    RunObservationV1 {
        schema_version: OBSERVATION_SCHEMA,
        event_id: ObservationEventId {
            cycle_id: cycle_id.to_owned(),
            sequence: fence.sequence,
            index,
        },
        intent_id: checkpoint.intent_id.clone(),
        intent_revision: checkpoint.intent_revision,
        intent_digest: checkpoint.intent_digest,
        transition: proposal_kind(proposal).to_owned(),
        category,
        cause,
        evidence_ids: evidence_ids
            .iter()
            .filter(|id| is_valid_evidence_id(id))
            .take(MAX_EVIDENCE_IDS)
            .cloned()
            .collect(),
        outcome,
        epoch: fence.epoch,
    }
}

/// `true` only for a confirmed, validated fix; an uncertain or pending
/// observation never counts.
#[must_use]
pub const fn is_countable_as_fix(observation: &RunObservationV1) -> bool {
    matches!(observation.category, ObservationCategory::ValidatedFix)
        && matches!(observation.outcome, ObservationOutcome::Confirmed)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use harw_authority::{
        AuthorityContext, Permission, PermissionSet, SandboxSpec, WorkspaceRegistration,
        WorkspaceRegistry,
    };
    use harw_types::{TenantId, WorkspaceId};

    use super::*;
    use crate::intent_cycle::{CycleAdmission, CycleLimits, CycleTargets, IntentBinding};
    use crate::test_support::{TestError, TestResult, ctx};

    fn authority() -> TestResult<AuthorityContext> {
        let harness_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or(TestError::Missing("workspace parent"))?
            .to_path_buf();
        let tenant = TenantId::from_str("observation-tenant");
        let workspace = WorkspaceId::from_str("cycle-observation-tests");
        let binding = WorkspaceRegistry::build(
            &harness_root,
            [WorkspaceRegistration {
                tenant: tenant.clone(),
                workspace: workspace.clone(),
                root: PathBuf::from("harw-plan-bridge"),
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

    fn checkpoint() -> TestResult<CycleCheckpoint> {
        let admission = CycleAdmission::new(
            IntentBinding {
                id: "intent-obs".to_owned(),
                revision: 3,
                digest: ContentDigest::of(b"intent-obs@3"),
                predecessor: Some(ContentDigest::of(b"intent-obs@2")),
                acceptance: BTreeSet::from(["done".to_owned()]),
            },
            CycleLimits {
                max_transitions: 8,
                max_chain_depth: 1,
                max_spawn_depth: 1,
                max_parallel_segments: 2,
                max_stall_transitions: 3,
            },
            CycleTargets::default(),
        )
        .map_err(|_| TestError::Missing("admission builds"))?;
        Ok(CycleCheckpoint::initial(&admission, &authority()?, 5))
    }

    fn verify() -> CycleProposal {
        CycleProposal::Verify {
            evidence_id: "ev-1".to_owned(),
        }
    }

    fn sample() -> TestResult<RunObservationV1> {
        let state = checkpoint()?;
        Ok(build_observation(
            "cycle-1",
            CheckpointFence {
                epoch: 7,
                sequence: 4,
            },
            2,
            &state,
            &verify(),
            ObservationCategory::VerificationFailed,
            CauseCode::Unclassified,
            &["ev-1".to_owned()],
        ))
    }

    #[test]
    fn refusals_map_to_admission_refused_with_mirrored_cause() {
        assert_eq!(
            classify_refusal(CycleRefusal::WriteNotAllowed),
            (
                ObservationCategory::AdmissionRefused,
                CauseCode::WriteNotAllowed
            )
        );
        assert_eq!(
            classify_refusal(CycleRefusal::Exhausted),
            (ObservationCategory::AdmissionRefused, CauseCode::Exhausted)
        );
        assert_eq!(
            classify_refusal(CycleRefusal::Stalled),
            (ObservationCategory::AdmissionRefused, CauseCode::Stalled)
        );
        assert_eq!(
            classify_refusal(CycleRefusal::CeilingMismatch),
            (
                ObservationCategory::AdmissionRefused,
                CauseCode::CeilingMismatch
            )
        );
    }

    #[test]
    fn step_failure_maps_by_retryable_and_ignores_reason() {
        let permanent_a = StepFailure::new("alpha");
        let permanent_b = StepFailure::new("completely different text");
        let temporary_a = StepFailure::retryable("overloaded");
        let temporary_b = StepFailure::retryable("");
        let permanent = (
            ObservationCategory::ToolFailedNoEffect,
            CauseCode::ExecutorFailed,
        );
        let temporary = (
            ObservationCategory::ProviderUnavailable,
            CauseCode::ExecutorRetryable,
        );
        assert_eq!(classify_step_failure(&permanent_a), permanent);
        assert_eq!(classify_step_failure(&permanent_b), permanent);
        assert_eq!(classify_step_failure(&temporary_a), temporary);
        assert_eq!(classify_step_failure(&temporary_b), temporary);
    }

    #[test]
    fn reconciliation_only_uncertain_is_an_observation() {
        assert_eq!(
            classify_reconciliation(&StepReconciliation::Uncertain),
            Some((
                ObservationCategory::UncertainEffect,
                CauseCode::ReconcileUncertain
            ))
        );
        assert_eq!(
            classify_reconciliation(&StepReconciliation::NotStarted),
            None
        );
    }

    fn claims(entries: &[(&str, &str)]) -> CycleObservations {
        CycleObservations {
            claims: entries
                .iter()
                .map(|(id, body)| ((*id).to_owned(), (*body).to_owned()))
                .collect(),
            ..CycleObservations::default()
        }
    }

    #[test]
    fn verification_claims_are_classified_by_prefix_only() {
        assert_eq!(
            classify_verification(&claims(&[("unmet:0", "needle missing")])),
            Some((
                ObservationCategory::VerificationFailed,
                CauseCode::VerificationUnmet
            ))
        );
        assert_eq!(
            classify_verification(&claims(&[("stale:cycle-v-0:lib", "file changed")])),
            Some((
                ObservationCategory::VerificationFailed,
                CauseCode::VerificationStale
            ))
        );
        assert_eq!(classify_verification(&CycleObservations::default()), None);
        assert_eq!(
            classify_verification(&claims(&[("note:1", "ok"), ("check:0:abc", "fine")])),
            None
        );
        // A prefix inside the claim body, or in the middle of an id, never counts.
        assert_eq!(
            classify_verification(&claims(&[
                ("note:1", "unmet:0 stale:x"),
                ("x-unmet:2", "stale:y"),
                (" stale:3", "unmet:4"),
            ])),
            None
        );
        // Mixed claims are resolved deterministically in id order.
        assert_eq!(
            classify_verification(&claims(&[("unmet:0", "a"), ("stale:z", "b")])),
            Some((
                ObservationCategory::VerificationFailed,
                CauseCode::VerificationStale
            ))
        );
    }

    #[test]
    fn fence_refusals_are_classified() {
        assert_eq!(
            classify_fence(FenceRefusal::StaleEpoch),
            (ObservationCategory::AdmissionRefused, CauseCode::StaleEpoch)
        );
        assert_eq!(
            classify_fence(FenceRefusal::CeilingWidened),
            (
                ObservationCategory::AdmissionRefused,
                CauseCode::CeilingMismatch
            )
        );
        assert_eq!(
            classify_fence(FenceRefusal::OutOfOrder),
            (
                ObservationCategory::AdmissionRefused,
                CauseCode::Unclassified
            )
        );
    }

    #[test]
    fn proposal_kind_is_snake_case_variant_name() {
        assert_eq!(proposal_kind(&verify()), "verify");
        assert_eq!(proposal_kind(&CycleProposal::Complete {}), "complete");
        assert_eq!(
            proposal_kind(&CycleProposal::ProposePatch {
                targets: BTreeSet::new(),
                evidence_id: String::new(),
            }),
            "propose_patch"
        );
        assert_eq!(
            proposal_kind(&CycleProposal::Wait {
                reason: "x".to_owned()
            }),
            "wait"
        );
    }

    #[test]
    fn dedupe_key_is_stable_and_unique() {
        let a = ObservationEventId {
            cycle_id: "c".to_owned(),
            sequence: 1,
            index: 2,
        };
        let same = a.clone();
        let other_index = ObservationEventId {
            index: 3,
            ..a.clone()
        };
        let other_sequence = ObservationEventId {
            sequence: 2,
            ..a.clone()
        };
        assert_eq!(a.dedupe_key(), "c:1:2");
        assert_eq!(a.dedupe_key(), same.dedupe_key());
        assert_ne!(a.dedupe_key(), other_index.dedupe_key());
        assert_ne!(a.dedupe_key(), other_sequence.dedupe_key());
    }

    #[test]
    fn build_observation_takes_identity_from_checkpoint_and_fence() -> TestResult {
        let observation = sample()?;
        assert_eq!(observation.schema_version, OBSERVATION_SCHEMA);
        assert_eq!(observation.intent_id, "intent-obs");
        assert_eq!(observation.intent_revision, 3);
        assert_eq!(
            observation.intent_digest,
            ContentDigest::of(b"intent-obs@3")
        );
        assert_eq!(observation.transition, "verify");
        assert_eq!(observation.epoch, 7);
        assert_eq!(observation.event_id.sequence, 4);
        assert_eq!(observation.event_id.index, 2);
        assert_eq!(observation.event_id.cycle_id, "cycle-1");
        assert_eq!(observation.outcome, ObservationOutcome::Pending);
        Ok(())
    }

    #[test]
    fn build_observation_caps_and_filters_evidence_ids() -> TestResult {
        let state = checkpoint()?;
        let long_ok = "a".repeat(128);
        let too_long = "b".repeat(129);
        let mut ids = vec![
            "bad id".to_owned(),
            too_long,
            String::new(),
            "ev:ü".to_owned(),
            long_ok.clone(),
        ];
        for n in 0..10 {
            ids.push(format!("ev-{n}"));
        }
        let observation = build_observation(
            "cycle-1",
            CheckpointFence {
                epoch: 1,
                sequence: 1,
            },
            0,
            &state,
            &verify(),
            ObservationCategory::VerificationFailed,
            CauseCode::Unclassified,
            &ids,
        );
        assert_eq!(observation.evidence_ids.len(), 8);
        assert_eq!(observation.evidence_ids.first(), Some(&long_ok));
        assert_eq!(
            observation.evidence_ids.get(1).map(String::as_str),
            Some("ev-0")
        );
        assert!(
            observation
                .evidence_ids
                .iter()
                .all(|id| is_valid_evidence_id(id))
        );
        Ok(())
    }

    #[test]
    fn uncertain_effect_is_inconclusive_and_never_a_fix() -> TestResult {
        let state = checkpoint()?;
        let observation = build_observation(
            "cycle-1",
            CheckpointFence {
                epoch: 1,
                sequence: 1,
            },
            0,
            &state,
            &verify(),
            ObservationCategory::UncertainEffect,
            CauseCode::ReconcileUncertain,
            &[],
        );
        assert_eq!(observation.outcome, ObservationOutcome::Inconclusive);
        assert!(!is_countable_as_fix(&observation));
        Ok(())
    }

    #[test]
    fn only_confirmed_validated_fix_is_countable() -> TestResult {
        let mut observation = sample()?;
        assert!(!is_countable_as_fix(&observation));
        observation.category = ObservationCategory::ValidatedFix;
        assert!(!is_countable_as_fix(&observation));
        observation.outcome = ObservationOutcome::Confirmed;
        assert!(is_countable_as_fix(&observation));
        observation.category = ObservationCategory::Recovered;
        assert!(!is_countable_as_fix(&observation));
        Ok(())
    }

    #[test]
    fn serde_round_trip_preserves_the_observation() -> TestResult {
        let observation = sample()?;
        let json = serde_json::to_string(&observation).map_err(ctx("serialize"))?;
        let back: RunObservationV1 = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, observation);
        Ok(())
    }

    #[test]
    fn unknown_field_is_rejected() -> TestResult {
        let observation = sample()?;
        let mut value = serde_json::to_value(&observation).map_err(ctx("to value"))?;
        let object = value
            .as_object_mut()
            .ok_or(TestError::Missing("observation is an object"))?;
        object.insert(
            "permission".to_owned(),
            serde_json::Value::String("full_access".to_owned()),
        );
        let parsed = serde_json::from_value::<RunObservationV1>(value);
        assert!(parsed.is_err());
        Ok(())
    }

    #[test]
    fn serialized_observation_has_no_reason_key() -> TestResult {
        let observation = sample()?;
        let value = serde_json::to_value(&observation).map_err(ctx("to value"))?;
        let object = value
            .as_object()
            .ok_or(TestError::Missing("observation is an object"))?;
        assert!(!object.contains_key("reason"));
        assert!(!object.contains_key("error"));
        assert_eq!(
            object.get("category"),
            Some(&serde_json::Value::String("verification_failed".to_owned()))
        );
        Ok(())
    }
}
