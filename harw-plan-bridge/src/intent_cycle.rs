//! Pure, fail-closed admission of proposed adaptive intent-cycle transitions
//! (PL-90 W01) plus the durable checkpoint fence and non-widening resume
//! contract (PL-90 W02, pure part).
//!
//! This is a planning mechanism, not a tool executor or a permission issuer:
//!
//! - A model *proposes* a [`CycleProposal`]; [`admit_cycle`] decides whether
//!   the proposal fits the trusted [`CycleAdmission`] and the current
//!   [`CycleCheckpoint`]. An admitted proposal has no side effect.
//! - An admitted [`CycleProposal::ProposePatch`] does NOT write a file. The
//!   writer still has to pass authority, approval, canonical path ownership,
//!   worktree/branch and preimage verification at its own execution boundary.
//! - An admitted [`CycleProposal::Complete`] is a *completion candidate*. Goal
//!   acceptance stays with the existing WorkDriver / human-only contract.
//! - Progress is never a caller- or model-supplied flag. [`apply_observations`]
//!   derives it from new, content-distinct evidence: a renamed copy of an
//!   already known digest is false novelty and counts as a stall.
//! - [`CycleAdmission`] is deliberately not `Deserialize`: serialized content
//!   (checkpoints, model output) can never mint target sets. On recovery the
//!   trusted runtime reissues the persisted [`AuthoritySnapshot`] through
//!   `harw_authority::PolicyBootstrap::reissue` and [`resume_admission`]
//!   intersects the stored ceiling with the current one, so a policy or role
//!   upgrade between crash and resume never widens a running cycle.
//! - Checkpoints carry explicit evidence and decisions, never private model
//!   reasoning traces.

use std::collections::{BTreeMap, BTreeSet};

use harw_authority::{AuthorityContext, AuthoritySnapshot, Permission};
use harw_types::ContentDigest;
use serde::{Deserialize, Serialize};

/// Schema version of [`CycleCheckpoint`]. Bump on any incompatible change.
pub const CYCLE_CHECKPOINT_SCHEMA: u32 = 1;

/// A versioned intent binding (review R02).
///
/// `digest` is the canonical digest of the approved intent payload, computed
/// by the trusted caller — never derived from model output. A revision
/// `n > 1` must name the digest of revision `n - 1` as `predecessor`; see
/// [`IntentBinding::supersedes`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentBinding {
    pub id: String,
    pub revision: u64,
    pub digest: ContentDigest,
    pub predecessor: Option<ContentDigest>,
    pub acceptance: BTreeSet<String>,
}

impl IntentBinding {
    /// Structural validity: non-empty id, revision ≥ 1, a predecessor exactly
    /// when the revision is not the first, non-empty acceptance criteria.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.id.trim().is_empty()
            && self.revision >= 1
            && (self.revision == 1) == self.predecessor.is_none()
            && !self.acceptance.is_empty()
            && self.acceptance.iter().all(|c| !c.trim().is_empty())
    }

    /// Whether `self` is the direct, auditable successor of `previous`.
    ///
    /// # Errors
    /// [`IntentRevisionError`] names why the chain is broken: another intent,
    /// a stale or skipped revision, a fork (same revision, other digest) or a
    /// predecessor digest that does not match.
    pub fn supersedes(&self, previous: &Self) -> Result<(), IntentRevisionError> {
        if !self.is_well_formed() || !previous.is_well_formed() {
            return Err(IntentRevisionError::Malformed);
        }
        if self.id != previous.id {
            return Err(IntentRevisionError::OtherIntent);
        }
        if self.revision == previous.revision {
            return Err(if self.digest == previous.digest {
                IntentRevisionError::Stale
            } else {
                IntentRevisionError::Forked
            });
        }
        if self.revision < previous.revision {
            return Err(IntentRevisionError::Stale);
        }
        if previous.revision.checked_add(1) != Some(self.revision) {
            return Err(IntentRevisionError::SkippedRevision);
        }
        if self.predecessor != Some(previous.digest) {
            return Err(IntentRevisionError::PredecessorMismatch);
        }
        Ok(())
    }
}

/// Why an intent revision does not supersede its predecessor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentRevisionError {
    Malformed,
    OtherIntent,
    Stale,
    Forked,
    SkippedRevision,
    PredecessorMismatch,
}

/// Limits independently bound to an execution by trusted orchestration.
/// Agent spawn depth and chain nesting depth are separate ceilings: a nested
/// recipe never counts as an agent spawn and vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CycleLimits {
    pub max_transitions: u32,
    pub max_chain_depth: u16,
    pub max_spawn_depth: u16,
    pub max_parallel_segments: u16,
    pub max_stall_transitions: u32,
}

impl CycleLimits {
    /// A budget of zero transitions or zero parallel segments cannot run
    /// anything and is rejected instead of silently admitting nothing.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.max_transitions > 0 && self.max_parallel_segments > 0
    }

    /// Component-wise minimum: the narrower of two ceilings.
    #[must_use]
    pub fn narrowed(&self, other: &Self) -> Self {
        Self {
            max_transitions: self.max_transitions.min(other.max_transitions),
            max_chain_depth: self.max_chain_depth.min(other.max_chain_depth),
            max_spawn_depth: self.max_spawn_depth.min(other.max_spawn_depth),
            max_parallel_segments: self.max_parallel_segments.min(other.max_parallel_segments),
            max_stall_transitions: self.max_stall_transitions.min(other.max_stall_transitions),
        }
    }
}

/// The trusted admission ceiling of one cycle: identifiers only.
///
/// Targets are registry identifiers (read sources, admitted child agent
/// names, recipe ids, write-scope entries) that the trusted runtime derived
/// from the effective IR, role matrix and authority context. This type never
/// encodes a permission grant itself, and it is intentionally not
/// `Deserialize`, so neither a checkpoint nor model output can construct one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CycleAdmission {
    intent: IntentBinding,
    limits: CycleLimits,
    allowed_read_targets: BTreeSet<String>,
    allowed_write_targets: BTreeSet<String>,
    allowed_children: BTreeSet<String>,
    allowed_recipes: BTreeSet<String>,
}

/// Trusted inputs for [`CycleAdmission::new`].
#[derive(Debug, Clone, Default)]
pub struct CycleTargets {
    pub read: BTreeSet<String>,
    pub write: BTreeSet<String>,
    pub children: BTreeSet<String>,
    pub recipes: BTreeSet<String>,
}

impl CycleAdmission {
    /// Builds an admission from trusted runtime inputs.
    ///
    /// # Errors
    /// [`CycleRefusal::InvalidIntent`] for a malformed intent,
    /// [`CycleRefusal::InvalidLimits`] for unusable limits and
    /// [`CycleRefusal::InvalidTarget`] for blank target identifiers.
    pub fn new(
        intent: IntentBinding,
        limits: CycleLimits,
        targets: CycleTargets,
    ) -> Result<Self, CycleRefusal> {
        if !intent.is_well_formed() {
            return Err(CycleRefusal::InvalidIntent);
        }
        if !limits.is_valid() {
            return Err(CycleRefusal::InvalidLimits);
        }
        let all = [
            &targets.read,
            &targets.write,
            &targets.children,
            &targets.recipes,
        ];
        if all
            .iter()
            .any(|set| set.iter().any(|t| t.trim().is_empty()))
        {
            return Err(CycleRefusal::InvalidTarget);
        }
        Ok(Self {
            intent,
            limits,
            allowed_read_targets: targets.read,
            allowed_write_targets: targets.write,
            allowed_children: targets.children,
            allowed_recipes: targets.recipes,
        })
    }

    #[must_use]
    pub fn intent(&self) -> &IntentBinding {
        &self.intent
    }

    #[must_use]
    pub fn limits(&self) -> &CycleLimits {
        &self.limits
    }

    #[must_use]
    pub fn allowed_read_targets(&self) -> &BTreeSet<String> {
        &self.allowed_read_targets
    }

    #[must_use]
    pub fn allowed_write_targets(&self) -> &BTreeSet<String> {
        &self.allowed_write_targets
    }

    #[must_use]
    pub fn allowed_children(&self) -> &BTreeSet<String> {
        &self.allowed_children
    }

    #[must_use]
    pub fn allowed_recipes(&self) -> &BTreeSet<String> {
        &self.allowed_recipes
    }

    /// The non-authorizing, persistable view of this ceiling.
    #[must_use]
    pub fn ceiling(&self) -> AdmissionCeiling {
        AdmissionCeiling {
            limits: self.limits,
            read: self.allowed_read_targets.clone(),
            write: self.allowed_write_targets.clone(),
            children: self.allowed_children.clone(),
            recipes: self.allowed_recipes.clone(),
        }
    }
}

/// Persisted record of the identifiers a cycle was admitted with. Like
/// [`AuthoritySnapshot`] it is a *ceiling* for [`resume_admission`], never a
/// source of authority: resume can only keep or drop entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionCeiling {
    pub limits: CycleLimits,
    pub read: BTreeSet<String>,
    pub write: BTreeSet<String>,
    pub children: BTreeSet<String>,
    pub recipes: BTreeSet<String>,
}

/// Where a piece of evidence came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSourceKind {
    /// A repository file at a pinned revision.
    Repository,
    /// A typed tool result.
    Tool,
    /// A child agent's return envelope.
    ChildReturn,
    /// A deterministic verification run (tests, gates).
    Verification,
    /// An explicit human statement or approval.
    Human,
}

/// How far a piece of evidence has been independently confirmed.
/// Ordered: `Reported < Observed < Verified`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceTrust {
    /// Claimed by a model or child; not yet checked.
    Reported,
    /// Observed by the trusted runtime (tool result, file read).
    Observed,
    /// Confirmed by an independent verifier or a deterministic gate.
    Verified,
}

/// One evidence record with provenance. Recorded by the trusted runtime via
/// [`apply_observations`]; a model can only *reference* it by id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRecord {
    pub id: String,
    pub source: EvidenceSourceKind,
    /// Repo path + revision, tool call id, child session id, ...
    pub locator: String,
    pub digest: ContentDigest,
    pub trust: EvidenceTrust,
}

/// Epoch/sequence fence of a durable checkpoint (review I-07).
///
/// `epoch` is the job lease epoch of the executor that wrote the checkpoint;
/// `sequence` increases by exactly one per committed transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointFence {
    pub epoch: u64,
    pub sequence: u64,
}

/// Durable cycle state. A checkpoint is evidence and state, not an authority
/// source: `authority` and `ceiling` are only ever narrowed on resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CycleCheckpoint {
    pub schema_version: u32,
    pub fence: CheckpointFence,
    pub intent_id: String,
    pub intent_revision: u64,
    pub intent_digest: ContentDigest,
    /// Non-authorizing snapshot of the authority the cycle was admitted
    /// under (review R01). Reissued through `PolicyBootstrap::reissue`.
    pub authority: AuthoritySnapshot,
    pub ceiling: AdmissionCeiling,
    pub transitions_used: u32,
    pub chain_depth: u16,
    pub spawn_depth: u16,
    pub stall_transitions: u32,
    /// Explicit claims (claim id -> statement), never hidden reasoning.
    pub claims: BTreeMap<String, String>,
    pub evidence: BTreeMap<String, EvidenceRecord>,
    /// Acceptance criterion -> id of the verified evidence that covers it.
    pub criteria_met: BTreeMap<String, String>,
    /// The last committed (admitted) decision, for status and audit.
    pub last_decision: Option<CycleProposal>,
    pub terminal: Option<CycleTerminal>,
}

impl CycleCheckpoint {
    /// The first checkpoint of a freshly admitted cycle.
    #[must_use]
    pub fn initial(admission: &CycleAdmission, authority: &AuthorityContext, epoch: u64) -> Self {
        Self {
            schema_version: CYCLE_CHECKPOINT_SCHEMA,
            fence: CheckpointFence { epoch, sequence: 0 },
            intent_id: admission.intent.id.clone(),
            intent_revision: admission.intent.revision,
            intent_digest: admission.intent.digest,
            authority: authority.snapshot(),
            ceiling: admission.ceiling(),
            transitions_used: 0,
            chain_depth: 0,
            spawn_depth: 0,
            stall_transitions: 0,
            claims: BTreeMap::new(),
            evidence: BTreeMap::new(),
            criteria_met: BTreeMap::new(),
            last_decision: None,
            terminal: None,
        }
    }

    /// State seed for an admitted nested recipe: one chain level deeper, the
    /// same agent spawn depth, the parent's evidence as read-only context.
    /// The nested cycle starts with a fresh transition budget under the
    /// caller-supplied (narrowed) admission.
    #[must_use]
    pub fn nested(&self) -> Self {
        self.derive(self.chain_depth.saturating_add(1), self.spawn_depth)
    }

    /// State seed for an admitted child agent: one spawn level deeper, chain
    /// depth reset (the child is a new accountable agent with its own chain).
    #[must_use]
    pub fn child(&self) -> Self {
        self.derive(0, self.spawn_depth.saturating_add(1))
    }

    fn derive(&self, chain_depth: u16, spawn_depth: u16) -> Self {
        Self {
            fence: CheckpointFence {
                epoch: self.fence.epoch,
                sequence: 0,
            },
            transitions_used: 0,
            chain_depth,
            spawn_depth,
            stall_transitions: 0,
            claims: BTreeMap::new(),
            criteria_met: BTreeMap::new(),
            last_decision: None,
            terminal: None,
            ..self.clone()
        }
    }

    fn knows(&self, evidence_id: &str) -> bool {
        self.evidence.contains_key(evidence_id)
    }
}

/// A requested graph segment; targets are registry identifiers (not tools,
/// filesystem paths, arbitrary model-defined roles or authority grants).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Segment {
    Read { target: String },
    Child { target: String },
    NestedRecipe { recipe: String },
}

/// How a fork's branches are joined before the next decision point.
/// Struct variants only, for the same `deny_unknown_fields` reason as
/// [`CycleProposal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JoinPolicy {
    /// Every branch must return.
    All {},
    /// The first successful branch suffices; the rest are cancelled.
    Any {},
    /// At least `required` branches must return.
    Quorum { required: u16 },
}

/// The LLM may propose a transition, never commit it. Every variant is a
/// struct variant: serde's `deny_unknown_fields` is not enforced for unit
/// variants of internally tagged enums, so `{"kind":"complete",
/// "permission":"full_access"}` would otherwise deserialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CycleProposal {
    /// Run further segments in parallel (bounded by `max_parallel_segments`).
    Advance {
        segments: Vec<Segment>,
    },
    /// Independent branches plus an explicit join before the next decision.
    Fork {
        branches: Vec<Segment>,
        join: JoinPolicy,
    },
    /// Change course because of a recorded observation.
    Reorient {
        evidence_id: String,
        segments: Vec<Segment>,
    },
    /// Re-examine earlier claims because recorded evidence contradicts them.
    Revisit {
        evidence_id: String,
        invalidated_claims: BTreeSet<String>,
    },
    /// Intention to change exactly these write-scope entries. No write.
    ProposePatch {
        targets: BTreeSet<String>,
        evidence_id: String,
    },
    /// Ask the trusted verifier to check recorded evidence.
    Verify {
        evidence_id: String,
    },
    /// Ask the owner/human for a scope delta. Not an approval.
    RequestApproval {
        reason: String,
        requested_targets: BTreeSet<String>,
    },
    /// Completion candidate; requires verified evidence for every criterion.
    Complete {},
    Wait {
        reason: String,
    },
    Escalate {
        reason: String,
    },
    /// Honest terminal: cannot proceed (missing capability, forbidden egress).
    Blocked {
        reason: String,
    },
    /// Honest terminal: the attempt failed.
    Failed {
        reason: String,
    },
}

impl CycleProposal {
    /// Honest exits stay admissible even when the cycle is exhausted or
    /// stalled: running out of budget must never force a fake success.
    #[must_use]
    pub fn is_exit(&self) -> bool {
        matches!(
            self,
            Self::Escalate { .. } | Self::Blocked { .. } | Self::Failed { .. }
        )
    }
}

/// Terminal state recorded in a checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CycleTerminal {
    /// All criteria covered by verified evidence. Goal acceptance still
    /// belongs to the owner (WorkDriver `ProposeAchieved`, human-only).
    CompletionProposed,
    Escalated,
    Blocked,
    Failed,
}

/// An accepted plan, not an executed action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedCycle {
    proposal: CycleProposal,
    expected_sequence: u64,
}

impl AdmittedCycle {
    /// Re-creates the admission of a step that was journaled as in flight
    /// before a crash. Its effect may already have happened, so its outcome
    /// must be recorded truthfully even if a narrowed admission would refuse
    /// the same proposal today. Crate-internal: only the durable cycle
    /// runtime may use it, and only for a journaled proposal.
    pub(crate) fn recovered(proposal: CycleProposal, expected_sequence: u64) -> Self {
        Self {
            proposal,
            expected_sequence,
        }
    }

    #[must_use]
    pub fn proposal(&self) -> &CycleProposal {
        &self.proposal
    }

    /// Checkpoint sequence this admission was decided against.
    #[must_use]
    pub fn expected_sequence(&self) -> u64 {
        self.expected_sequence
    }

    /// The terminal state this transition leads to, if any.
    #[must_use]
    pub fn terminal(&self) -> Option<CycleTerminal> {
        match self.proposal {
            CycleProposal::Complete {} => Some(CycleTerminal::CompletionProposed),
            CycleProposal::Escalate { .. } => Some(CycleTerminal::Escalated),
            CycleProposal::Blocked { .. } => Some(CycleTerminal::Blocked),
            CycleProposal::Failed { .. } => Some(CycleTerminal::Failed),
            _ => None,
        }
    }
}

/// Specific refusals can be surfaced without retrying the same unusable route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleRefusal {
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
    /// The checkpoint was not admitted under (a subset of) this admission.
    CeilingMismatch,
}

/// Pure admission decision.
///
/// # Errors
/// A [`CycleRefusal`] naming the first violated rule. Nothing is mutated.
pub fn admit_cycle(
    policy: &CycleAdmission,
    state: &CycleCheckpoint,
    proposal: CycleProposal,
) -> Result<AdmittedCycle, CycleRefusal> {
    check_state(policy, state)?;
    let limits = &policy.limits;
    if !proposal.is_exit() {
        if state.transitions_used >= limits.max_transitions {
            return Err(CycleRefusal::Exhausted);
        }
        if state.stall_transitions >= limits.max_stall_transitions {
            return Err(CycleRefusal::Stalled);
        }
    }
    match &proposal {
        CycleProposal::Advance { segments } => check_segments(policy, state, segments)?,
        CycleProposal::Fork { branches, join } => {
            if branches.len() < 2 {
                return Err(CycleRefusal::InvalidJoin);
            }
            check_segments(policy, state, branches)?;
            if let JoinPolicy::Quorum { required } = join {
                if *required == 0 || usize::from(*required) > branches.len() {
                    return Err(CycleRefusal::InvalidJoin);
                }
            }
        }
        CycleProposal::Reorient {
            evidence_id,
            segments,
        } => {
            require_evidence(state, evidence_id)?;
            check_segments(policy, state, segments)?;
        }
        CycleProposal::Revisit {
            evidence_id,
            invalidated_claims,
        } => {
            require_evidence(state, evidence_id)?;
            if invalidated_claims.is_empty()
                || !invalidated_claims
                    .iter()
                    .all(|claim| state.claims.contains_key(claim))
            {
                return Err(CycleRefusal::UnknownClaim);
            }
        }
        CycleProposal::ProposePatch {
            targets,
            evidence_id,
        } => {
            if targets.is_empty()
                || !targets
                    .iter()
                    .all(|target| policy.allowed_write_targets.contains(target))
            {
                return Err(CycleRefusal::WriteNotAllowed);
            }
            require_evidence(state, evidence_id)?;
        }
        CycleProposal::Verify { evidence_id } => require_evidence(state, evidence_id)?,
        CycleProposal::RequestApproval {
            reason,
            requested_targets,
        } => {
            check_reason(reason)?;
            if requested_targets.is_empty() || requested_targets.iter().any(|t| t.trim().is_empty())
            {
                return Err(CycleRefusal::InvalidTarget);
            }
        }
        CycleProposal::Complete {} => {
            let covered = policy.intent.acceptance.iter().all(|criterion| {
                state
                    .criteria_met
                    .get(criterion)
                    .and_then(|evidence_id| state.evidence.get(evidence_id))
                    .is_some_and(|record| record.trust == EvidenceTrust::Verified)
            });
            if !covered {
                return Err(CycleRefusal::IncompleteAcceptance);
            }
        }
        CycleProposal::Wait { reason }
        | CycleProposal::Escalate { reason }
        | CycleProposal::Blocked { reason }
        | CycleProposal::Failed { reason } => check_reason(reason)?,
    }
    Ok(AdmittedCycle {
        proposal,
        expected_sequence: state.fence.sequence,
    })
}

fn check_state(policy: &CycleAdmission, state: &CycleCheckpoint) -> Result<(), CycleRefusal> {
    if state.schema_version != CYCLE_CHECKPOINT_SCHEMA {
        return Err(CycleRefusal::UnsupportedSchema);
    }
    if !policy.intent.is_well_formed()
        || state.intent_id != policy.intent.id
        || state.intent_revision != policy.intent.revision
        || state.intent_digest != policy.intent.digest
    {
        return Err(CycleRefusal::InvalidIntent);
    }
    if !policy.limits.is_valid() {
        return Err(CycleRefusal::InvalidLimits);
    }
    if !ceiling_covers(&state.ceiling, policy) {
        return Err(CycleRefusal::CeilingMismatch);
    }
    if state.terminal.is_some() {
        return Err(CycleRefusal::AlreadyTerminal);
    }
    if state.chain_depth > policy.limits.max_chain_depth {
        return Err(CycleRefusal::ChainDepthExceeded);
    }
    if state.spawn_depth > policy.limits.max_spawn_depth {
        return Err(CycleRefusal::SpawnDepthExceeded);
    }
    Ok(())
}

/// The admission in force must stay within the ceiling recorded in the
/// checkpoint; a broader admission indicates a widened, unreissued policy.
fn ceiling_covers(ceiling: &AdmissionCeiling, policy: &CycleAdmission) -> bool {
    policy.allowed_read_targets.is_subset(&ceiling.read)
        && policy.allowed_write_targets.is_subset(&ceiling.write)
        && policy.allowed_children.is_subset(&ceiling.children)
        && policy.allowed_recipes.is_subset(&ceiling.recipes)
        && policy.limits.narrowed(&ceiling.limits) == policy.limits
}

fn check_segments(
    policy: &CycleAdmission,
    state: &CycleCheckpoint,
    segments: &[Segment],
) -> Result<(), CycleRefusal> {
    if segments.is_empty() || segments.len() > usize::from(policy.limits.max_parallel_segments) {
        return Err(CycleRefusal::EmptyOrOversizedGraph);
    }
    let mut seen = BTreeSet::new();
    for segment in segments {
        if !seen.insert(segment) {
            return Err(CycleRefusal::DuplicateSegment);
        }
        match segment {
            Segment::Read { target } => {
                if !policy.allowed_read_targets.contains(target) {
                    return Err(CycleRefusal::ReadNotAllowed);
                }
            }
            Segment::Child { target } => {
                if !policy.allowed_children.contains(target) {
                    return Err(CycleRefusal::ChildNotAllowed);
                }
                if state.spawn_depth >= policy.limits.max_spawn_depth {
                    return Err(CycleRefusal::SpawnDepthExceeded);
                }
            }
            Segment::NestedRecipe { recipe } => {
                if !policy.allowed_recipes.contains(recipe) {
                    return Err(CycleRefusal::RecipeNotAllowed);
                }
                if state.chain_depth >= policy.limits.max_chain_depth {
                    return Err(CycleRefusal::ChainDepthExceeded);
                }
            }
        }
    }
    Ok(())
}

fn require_evidence(state: &CycleCheckpoint, evidence_id: &str) -> Result<(), CycleRefusal> {
    if state.knows(evidence_id) {
        Ok(())
    } else {
        Err(CycleRefusal::MissingEvidence)
    }
}

fn check_reason(reason: &str) -> Result<(), CycleRefusal> {
    if reason.trim().is_empty() {
        Err(CycleRefusal::InvalidReason)
    } else {
        Ok(())
    }
}

/// Runtime-observed outcome of an admitted transition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CycleObservations {
    pub evidence: Vec<EvidenceRecord>,
    pub claims: BTreeMap<String, String>,
    /// Criterion -> evidence id the runtime attributes to it.
    pub criteria: BTreeMap<String, String>,
}

/// Why an outcome could not be folded into the checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationError {
    /// The checkpoint moved on since the transition was admitted.
    StaleAdmission,
    /// An evidence id was reused for different content.
    ConflictingEvidence,
    /// A criterion is not part of the intent or names unknown evidence.
    UnknownCriterion,
    Overflow,
}

/// Folds the runtime-observed outcome of `admitted` into the next checkpoint.
///
/// Progress is derived here, not supplied: it requires at least one evidence
/// record with a content digest the cycle has not seen before, a newly
/// covered criterion, or a `Revisit` that invalidated claims. Re-reporting a
/// known digest under a fresh id is false novelty. A `Revisit` removes the
/// invalidated claims and any criterion whose evidence it names.
///
/// # Errors
/// [`ObservationError`] if the admission is stale, evidence ids collide,
/// a criterion is unknown or a counter would overflow.
pub fn apply_observations(
    intent: &IntentBinding,
    state: &CycleCheckpoint,
    admitted: &AdmittedCycle,
    observations: CycleObservations,
) -> Result<CycleCheckpoint, ObservationError> {
    if admitted.expected_sequence != state.fence.sequence {
        return Err(ObservationError::StaleAdmission);
    }
    let mut next = state.clone();
    let known_digests: BTreeSet<ContentDigest> = state
        .evidence
        .values()
        .map(|record| record.digest)
        .collect();
    let mut progress = false;
    for record in observations.evidence {
        match next.evidence.get(&record.id) {
            Some(existing) if existing.digest != record.digest => {
                return Err(ObservationError::ConflictingEvidence);
            }
            Some(existing) => {
                // Same content re-observed: only a trust upgrade counts.
                if record.trust > existing.trust {
                    progress = true;
                    next.evidence.insert(record.id.clone(), record);
                }
            }
            None => {
                if !known_digests.contains(&record.digest) {
                    progress = true;
                }
                next.evidence.insert(record.id.clone(), record);
            }
        }
    }
    if let CycleProposal::Revisit {
        evidence_id,
        invalidated_claims,
    } = &admitted.proposal
    {
        for claim in invalidated_claims {
            progress |= next.claims.remove(claim).is_some();
        }
        next.criteria_met
            .retain(|_, covered_by| covered_by != evidence_id);
    }
    next.claims.extend(observations.claims);
    for (criterion, evidence_id) in observations.criteria {
        if !intent.acceptance.contains(&criterion) || !next.evidence.contains_key(&evidence_id) {
            return Err(ObservationError::UnknownCriterion);
        }
        if next.criteria_met.get(&criterion) != Some(&evidence_id) {
            progress = true;
            next.criteria_met.insert(criterion, evidence_id);
        }
    }
    next.transitions_used = next
        .transitions_used
        .checked_add(1)
        .ok_or(ObservationError::Overflow)?;
    next.fence.sequence = next
        .fence
        .sequence
        .checked_add(1)
        .ok_or(ObservationError::Overflow)?;
    next.stall_transitions = if progress {
        0
    } else {
        next.stall_transitions
            .checked_add(1)
            .ok_or(ObservationError::Overflow)?
    };
    next.terminal = admitted.terminal();
    next.last_decision = Some(admitted.proposal.clone());
    Ok(next)
}

/// Why a checkpoint must not replace the stored one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceRefusal {
    /// Written by an executor whose lease epoch was superseded.
    StaleEpoch,
    /// Not the direct successor of the stored checkpoint.
    OutOfOrder,
    /// Belongs to another intent or revision.
    IntentMismatch,
    /// Tries to widen the recorded ceiling or swap the authority snapshot.
    CeilingWidened,
    /// A rebind changed cycle state other than epoch, ceiling and authority.
    StateTampered,
}

/// Compare-and-swap rule for persisting `candidate` over `stored`.
///
/// Within one epoch the sequence must advance by exactly one. A new lease
/// epoch (resumed executor) may continue from any later sequence but never
/// rewind. Intent identity, authority snapshot and the recorded ceiling are
/// immutable across commits — resume narrows them through
/// [`resume_admission`] before the first new commit.
///
/// # Errors
/// [`FenceRefusal`] naming the violated rule.
pub fn check_commit(
    stored: &CycleCheckpoint,
    candidate: &CycleCheckpoint,
) -> Result<(), FenceRefusal> {
    if candidate.intent_id != stored.intent_id
        || candidate.intent_revision != stored.intent_revision
        || candidate.intent_digest != stored.intent_digest
    {
        return Err(FenceRefusal::IntentMismatch);
    }
    if candidate.authority != stored.authority || candidate.ceiling != stored.ceiling {
        return Err(FenceRefusal::CeilingWidened);
    }
    if candidate.fence.epoch < stored.fence.epoch {
        return Err(FenceRefusal::StaleEpoch);
    }
    let in_order = if candidate.fence.epoch == stored.fence.epoch {
        stored.fence.sequence.checked_add(1) == Some(candidate.fence.sequence)
    } else {
        candidate.fence.sequence > stored.fence.sequence
    };
    if in_order {
        Ok(())
    } else {
        Err(FenceRefusal::OutOfOrder)
    }
}

/// Rule for replacing `stored` with the output of [`resume_admission`] when
/// a new lease epoch takes over a cycle.
///
/// The candidate must be fenced under a strictly newer epoch at the *same*
/// sequence, may only narrow the ceiling and the authority snapshot (same
/// workspace, subset of permissions), and must leave every other field
/// untouched.
///
/// # Errors
/// [`FenceRefusal`] naming the violated rule.
pub fn check_rebind(
    stored: &CycleCheckpoint,
    candidate: &CycleCheckpoint,
) -> Result<(), FenceRefusal> {
    if candidate.intent_id != stored.intent_id
        || candidate.intent_revision != stored.intent_revision
        || candidate.intent_digest != stored.intent_digest
    {
        return Err(FenceRefusal::IntentMismatch);
    }
    if candidate.fence.epoch <= stored.fence.epoch {
        return Err(FenceRefusal::StaleEpoch);
    }
    if candidate.fence.sequence != stored.fence.sequence {
        return Err(FenceRefusal::OutOfOrder);
    }
    let narrowed = candidate.ceiling.read.is_subset(&stored.ceiling.read)
        && candidate.ceiling.write.is_subset(&stored.ceiling.write)
        && candidate
            .ceiling
            .children
            .is_subset(&stored.ceiling.children)
        && candidate.ceiling.recipes.is_subset(&stored.ceiling.recipes)
        && candidate.ceiling.limits.narrowed(&stored.ceiling.limits) == candidate.ceiling.limits
        && candidate.authority.workspace() == stored.authority.workspace()
        && candidate
            .authority
            .request()
            .iter()
            .all(|permission| stored.authority.request().contains(permission))
        && candidate
            .authority
            .request()
            .network_scope()
            .is_subset_of(stored.authority.request().network_scope());
    if !narrowed {
        return Err(FenceRefusal::CeilingWidened);
    }
    let mut rest = candidate.clone();
    rest.fence = stored.fence;
    rest.ceiling = stored.ceiling.clone();
    rest.authority = stored.authority.clone();
    if &rest != stored {
        return Err(FenceRefusal::StateTampered);
    }
    Ok(())
}

/// Why a cycle cannot be resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeRefusal {
    /// The current admission is for another intent/revision/digest.
    IntentChanged,
    /// The reissued authority belongs to another workspace than recorded.
    WorkspaceMismatch,
    /// The reissued authority holds a permission the snapshot never had.
    AuthorityWidened,
    UnsupportedSchema,
    AlreadyTerminal,
    InvalidLimits,
}

/// Resumes a persisted cycle under the *current* trusted admission.
///
/// `reissued` must come from `PolicyBootstrap::reissue(binding,
/// &checkpoint.authority)`; it is checked against the snapshot again so a
/// caller passing a broader context fails closed. The result is the
/// intersection of stored ceiling, current admission and reissued authority:
/// write targets survive only with `WriteWorkspace`, read targets only with
/// `ReadWorkspace`. The returned checkpoint carries the narrowed ceiling and
/// the reissued snapshot under the new lease `epoch`.
///
/// # Errors
/// [`ResumeRefusal`] naming the violated rule.
pub fn resume_admission(
    checkpoint: &CycleCheckpoint,
    current: &CycleAdmission,
    reissued: &AuthorityContext,
    epoch: u64,
) -> Result<(CycleAdmission, CycleCheckpoint), ResumeRefusal> {
    if checkpoint.schema_version != CYCLE_CHECKPOINT_SCHEMA {
        return Err(ResumeRefusal::UnsupportedSchema);
    }
    if checkpoint.terminal.is_some() {
        return Err(ResumeRefusal::AlreadyTerminal);
    }
    if checkpoint.intent_id != current.intent.id
        || checkpoint.intent_revision != current.intent.revision
        || checkpoint.intent_digest != current.intent.digest
    {
        return Err(ResumeRefusal::IntentChanged);
    }
    if !checkpoint
        .authority
        .workspace()
        .matches(reissued.workspace())
    {
        return Err(ResumeRefusal::WorkspaceMismatch);
    }
    let recorded = checkpoint.authority.request();
    if reissued
        .permissions()
        .iter()
        .any(|permission| !recorded.contains(permission))
        || !reissued
            .network_scope()
            .is_subset_of(recorded.network_scope())
    {
        return Err(ResumeRefusal::AuthorityWidened);
    }
    let permissions = reissued.permissions();
    let keep = |stored: &BTreeSet<String>, now: &BTreeSet<String>, needed: Option<Permission>| {
        if needed.is_some_and(|permission| !permissions.contains(permission)) {
            BTreeSet::new()
        } else {
            stored.intersection(now).cloned().collect()
        }
    };
    let ceiling = &checkpoint.ceiling;
    let limits = ceiling.limits.narrowed(&current.limits);
    if !limits.is_valid() {
        return Err(ResumeRefusal::InvalidLimits);
    }
    let admission = CycleAdmission {
        intent: current.intent.clone(),
        limits,
        allowed_read_targets: keep(
            &ceiling.read,
            &current.allowed_read_targets,
            Some(Permission::ReadWorkspace),
        ),
        allowed_write_targets: keep(
            &ceiling.write,
            &current.allowed_write_targets,
            Some(Permission::WriteWorkspace),
        ),
        allowed_children: keep(&ceiling.children, &current.allowed_children, None),
        allowed_recipes: keep(&ceiling.recipes, &current.allowed_recipes, None),
    };
    let mut resumed = checkpoint.clone();
    resumed.authority = reissued.snapshot();
    resumed.ceiling = admission.ceiling();
    resumed.fence.epoch = epoch;
    Ok((admission, resumed))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use harw_authority::{
        PermissionSet, SandboxSpec, WorkspaceBinding, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{TenantId, WorkspaceId};

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn binding(workspace: &str) -> TestResult<WorkspaceBinding> {
        let harness_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or(TestError::Missing(
                "harw-plan-bridge has a workspace parent",
            ))?
            .to_path_buf();
        let tenant = TenantId::from_str("cycle-tenant");
        let id = WorkspaceId::from_str(workspace);
        WorkspaceRegistry::build(
            &harness_root,
            [WorkspaceRegistration {
                tenant: tenant.clone(),
                workspace: id.clone(),
                root: PathBuf::from("harw-plan-bridge"),
            }],
        )
        .map_err(ctx("test workspace registers"))?
        .resolve(&tenant, &id)
        .map_err(ctx("test workspace resolves"))
    }

    fn authority(permissions: &[Permission]) -> TestResult<AuthorityContext> {
        Ok(SandboxSpec::from_resolved(
            binding("cycle-tests")?,
            PermissionSet::from_policy(permissions.iter().copied()),
        )
        .authority()
        .clone())
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn intent() -> IntentBinding {
        IntentBinding {
            id: "intent-a".to_owned(),
            revision: 2,
            digest: ContentDigest::of(b"intent-a@2"),
            predecessor: Some(ContentDigest::of(b"intent-a@1")),
            acceptance: set(&["tested"]),
        }
    }

    fn limits() -> CycleLimits {
        CycleLimits {
            max_transitions: 8,
            max_chain_depth: 3,
            max_spawn_depth: 1,
            max_parallel_segments: 3,
            max_stall_transitions: 2,
        }
    }

    fn targets() -> CycleTargets {
        CycleTargets {
            read: set(&["source", "tests"]),
            write: set(&["code.rs", "lib.rs"]),
            children: set(&["explorer"]),
            recipes: set(&["hypothesis"]),
        }
    }

    fn evidence(id: &str, content: &str, trust: EvidenceTrust) -> EvidenceRecord {
        EvidenceRecord {
            id: id.to_owned(),
            source: EvidenceSourceKind::Repository,
            locator: format!("repo:{content}"),
            digest: ContentDigest::of(content.as_bytes()),
            trust,
        }
    }

    fn fixture() -> TestResult<(CycleAdmission, CycleCheckpoint)> {
        let policy = CycleAdmission::new(intent(), limits(), targets())
            .map_err(|_| TestError::Missing("fixture admission is valid"))?;
        let rights = authority(&[Permission::ReadWorkspace, Permission::WriteWorkspace])?;
        let mut state = CycleCheckpoint::initial(&policy, &rights, 1);
        let record = evidence("evidence-1", "observation", EvidenceTrust::Observed);
        state.evidence.insert(record.id.clone(), record);
        state
            .claims
            .insert("claim-1".to_owned(), "the bug is in parse()".to_owned());
        Ok((policy, state))
    }

    fn read(target: &str) -> Segment {
        Segment::Read {
            target: target.to_owned(),
        }
    }

    fn reason(text: &str) -> String {
        text.to_owned()
    }

    #[test]
    fn reorientation_requires_real_observation() -> TestResult {
        let (policy, state) = fixture()?;
        let proposal = CycleProposal::Reorient {
            evidence_id: "invented".to_owned(),
            segments: vec![read("source")],
        };
        assert_eq!(
            admit_cycle(&policy, &state, proposal),
            Err(CycleRefusal::MissingEvidence)
        );
        Ok(())
    }

    #[test]
    fn forbidden_nested_agent_is_rejected() -> TestResult {
        let (policy, state) = fixture()?;
        let proposal = CycleProposal::Advance {
            segments: vec![Segment::Child {
                target: "matrix-game-master".to_owned(),
            }],
        };
        assert_eq!(
            admit_cycle(&policy, &state, proposal),
            Err(CycleRefusal::ChildNotAllowed)
        );
        Ok(())
    }

    #[test]
    fn nested_chains_and_spawns_have_separate_depths() -> TestResult {
        let (policy, state) = fixture()?;
        let nested = |recipe: &str| CycleProposal::Advance {
            segments: vec![Segment::NestedRecipe {
                recipe: recipe.to_owned(),
            }],
        };
        let spawn = CycleProposal::Advance {
            segments: vec![Segment::Child {
                target: "explorer".to_owned(),
            }],
        };
        let mut deep = state.nested().nested().nested();
        assert_eq!(deep.chain_depth, 3);
        assert_eq!(deep.spawn_depth, 0);
        assert_eq!(
            admit_cycle(&policy, &deep, nested("hypothesis")),
            Err(CycleRefusal::ChainDepthExceeded)
        );
        // Chain depth does not consume the agent spawn budget.
        assert!(admit_cycle(&policy, &deep, spawn.clone()).is_ok());
        deep = state.child();
        assert_eq!((deep.chain_depth, deep.spawn_depth), (0, 1));
        assert_eq!(
            admit_cycle(&policy, &deep, spawn),
            Err(CycleRefusal::SpawnDepthExceeded)
        );
        // ... and spawn depth does not consume chain nesting.
        assert!(admit_cycle(&policy, &deep, nested("hypothesis")).is_ok());
        // An over-deep state reports the right ceiling.
        deep.spawn_depth = 2;
        assert_eq!(
            admit_cycle(
                &policy,
                &deep,
                CycleProposal::Advance {
                    segments: vec![read("source")]
                }
            ),
            Err(CycleRefusal::SpawnDepthExceeded)
        );
        Ok(())
    }

    #[test]
    fn graph_shape_is_bounded_and_deduplicated() -> TestResult {
        let (policy, state) = fixture()?;
        let advance = |segments: Vec<Segment>| CycleProposal::Advance { segments };
        assert_eq!(
            admit_cycle(&policy, &state, advance(vec![])),
            Err(CycleRefusal::EmptyOrOversizedGraph)
        );
        assert_eq!(
            admit_cycle(
                &policy,
                &state,
                advance(vec![read("source"), read("source")])
            ),
            Err(CycleRefusal::DuplicateSegment)
        );
        let too_many = vec![
            read("source"),
            read("tests"),
            Segment::Child {
                target: "explorer".to_owned(),
            },
            Segment::NestedRecipe {
                recipe: "hypothesis".to_owned(),
            },
        ];
        assert_eq!(
            admit_cycle(&policy, &state, advance(too_many)),
            Err(CycleRefusal::EmptyOrOversizedGraph)
        );
        assert_eq!(
            admit_cycle(&policy, &state, advance(vec![read("")])),
            Err(CycleRefusal::ReadNotAllowed)
        );
        Ok(())
    }

    #[test]
    fn fork_requires_branches_and_a_satisfiable_join() -> TestResult {
        let (policy, state) = fixture()?;
        let fork = |branches: Vec<Segment>, join| CycleProposal::Fork { branches, join };
        assert_eq!(
            admit_cycle(
                &policy,
                &state,
                fork(vec![read("source")], JoinPolicy::All {})
            ),
            Err(CycleRefusal::InvalidJoin)
        );
        let two = vec![read("source"), read("tests")];
        assert_eq!(
            admit_cycle(
                &policy,
                &state,
                fork(two.clone(), JoinPolicy::Quorum { required: 3 })
            ),
            Err(CycleRefusal::InvalidJoin)
        );
        assert_eq!(
            admit_cycle(
                &policy,
                &state,
                fork(two.clone(), JoinPolicy::Quorum { required: 0 })
            ),
            Err(CycleRefusal::InvalidJoin)
        );
        assert!(
            admit_cycle(
                &policy,
                &state,
                fork(two, JoinPolicy::Quorum { required: 2 })
            )
            .is_ok()
        );
        Ok(())
    }

    #[test]
    fn revisit_needs_cause_and_known_claims_and_drops_them() -> TestResult {
        let (policy, mut state) = fixture()?;
        state
            .criteria_met
            .insert("tested".to_owned(), "evidence-1".to_owned());
        let revisit = |claims: &[&str]| CycleProposal::Revisit {
            evidence_id: "evidence-1".to_owned(),
            invalidated_claims: set(claims),
        };
        assert_eq!(
            admit_cycle(&policy, &state, revisit(&[])),
            Err(CycleRefusal::UnknownClaim)
        );
        assert_eq!(
            admit_cycle(&policy, &state, revisit(&["claim-x"])),
            Err(CycleRefusal::UnknownClaim)
        );
        let admitted = admit_cycle(&policy, &state, revisit(&["claim-1"]))
            .map_err(|_| TestError::Missing("revisit is admitted"))?;
        let next = apply_observations(
            policy.intent(),
            &state,
            &admitted,
            CycleObservations::default(),
        )
        .map_err(|_| TestError::Missing("revisit applies"))?;
        assert!(next.claims.is_empty());
        assert!(
            next.criteria_met.is_empty(),
            "criteria built on revisited evidence reopen"
        );
        assert_eq!(
            next.stall_transitions, 0,
            "invalidating a claim is progress"
        );
        Ok(())
    }

    #[test]
    fn patch_proposal_needs_scope_for_every_target_and_evidence() -> TestResult {
        let (policy, state) = fixture()?;
        let patch = |paths: &[&str], evidence: &str| CycleProposal::ProposePatch {
            targets: set(paths),
            evidence_id: evidence.to_owned(),
        };
        assert_eq!(
            admit_cycle(
                &policy,
                &state,
                patch(&["code.rs", "other.rs"], "evidence-1")
            ),
            Err(CycleRefusal::WriteNotAllowed)
        );
        assert_eq!(
            admit_cycle(&policy, &state, patch(&[], "evidence-1")),
            Err(CycleRefusal::WriteNotAllowed)
        );
        assert_eq!(
            admit_cycle(&policy, &state, patch(&["code.rs"], "invented")),
            Err(CycleRefusal::MissingEvidence)
        );
        let admitted = admit_cycle(&policy, &state, patch(&["code.rs", "lib.rs"], "evidence-1"))
            .map_err(|_| TestError::Missing("contract patch is admitted"))?;
        assert_eq!(
            admitted.terminal(),
            None,
            "a patch proposal is not an effect"
        );
        Ok(())
    }

    #[test]
    fn stale_intent_and_unproven_completion_are_refused() -> TestResult {
        let (policy, mut state) = fixture()?;
        let complete = || CycleProposal::Complete {};
        state.intent_revision = 1;
        assert_eq!(
            admit_cycle(&policy, &state, complete()),
            Err(CycleRefusal::InvalidIntent)
        );
        state.intent_revision = 2;
        state.intent_digest = ContentDigest::of(b"forked payload");
        assert_eq!(
            admit_cycle(&policy, &state, complete()),
            Err(CycleRefusal::InvalidIntent)
        );
        state.intent_digest = policy.intent().digest;
        assert_eq!(
            admit_cycle(&policy, &state, complete()),
            Err(CycleRefusal::IncompleteAcceptance)
        );
        // Merely observed (unverified) evidence does not satisfy acceptance.
        state
            .criteria_met
            .insert("tested".to_owned(), "evidence-1".to_owned());
        assert_eq!(
            admit_cycle(&policy, &state, complete()),
            Err(CycleRefusal::IncompleteAcceptance)
        );
        let verified = evidence("gate-1", "cargo test green", EvidenceTrust::Verified);
        state.evidence.insert(verified.id.clone(), verified);
        state
            .criteria_met
            .insert("tested".to_owned(), "gate-1".to_owned());
        let admitted = admit_cycle(&policy, &state, complete())
            .map_err(|_| TestError::Missing("verified completion is admitted"))?;
        assert_eq!(admitted.terminal(), Some(CycleTerminal::CompletionProposed));
        Ok(())
    }

    #[test]
    fn progress_is_derived_and_false_novelty_stalls() -> TestResult {
        let (policy, state) = fixture()?;
        let wait = || CycleProposal::Wait {
            reason: reason("child running"),
        };
        let step = |state: &CycleCheckpoint, observations| -> TestResult<CycleCheckpoint> {
            let admitted = admit_cycle(&policy, state, wait())
                .map_err(|_| TestError::Missing("wait is admitted"))?;
            apply_observations(policy.intent(), state, &admitted, observations)
                .map_err(|_| TestError::Missing("observations apply"))
        };
        // Same content under a new id is not progress.
        let renamed = CycleObservations {
            evidence: vec![evidence(
                "evidence-2",
                "observation",
                EvidenceTrust::Observed,
            )],
            ..CycleObservations::default()
        };
        let s1 = step(&state, renamed)?;
        assert_eq!(s1.stall_transitions, 1);
        let s2 = step(&s1, CycleObservations::default())?;
        assert_eq!(s2.stall_transitions, 2);
        assert_eq!(
            admit_cycle(&policy, &s2, wait()),
            Err(CycleRefusal::Stalled)
        );
        // Honest exits remain possible when stalled.
        assert!(
            admit_cycle(
                &policy,
                &s2,
                CycleProposal::Blocked {
                    reason: reason("no route")
                }
            )
            .is_ok()
        );
        // New content resets the stall counter.
        let fresh = CycleObservations {
            evidence: vec![evidence("evidence-3", "new fact", EvidenceTrust::Observed)],
            ..CycleObservations::default()
        };
        let s3 = step(&s1, fresh)?;
        assert_eq!(s3.stall_transitions, 0);
        assert_eq!(s3.transitions_used, 2);
        assert_eq!(s3.fence.sequence, 2);
        Ok(())
    }

    #[test]
    fn transition_budget_is_enforced_but_exits_remain() -> TestResult {
        let (policy, mut state) = fixture()?;
        state.transitions_used = policy.limits().max_transitions;
        assert_eq!(
            admit_cycle(
                &policy,
                &state,
                CycleProposal::Verify {
                    evidence_id: "evidence-1".to_owned()
                }
            ),
            Err(CycleRefusal::Exhausted)
        );
        assert!(
            admit_cycle(
                &policy,
                &state,
                CycleProposal::Escalate {
                    reason: reason("budget")
                }
            )
            .is_ok()
        );
        assert_eq!(
            admit_cycle(
                &policy,
                &state,
                CycleProposal::Failed {
                    reason: reason("  ")
                }
            ),
            Err(CycleRefusal::InvalidReason)
        );
        Ok(())
    }

    #[test]
    fn terminal_cycles_admit_nothing_more() -> TestResult {
        let (policy, state) = fixture()?;
        let admitted = admit_cycle(
            &policy,
            &state,
            CycleProposal::Blocked {
                reason: reason("egress denied"),
            },
        )
        .map_err(|_| TestError::Missing("blocked is admitted"))?;
        let done = apply_observations(
            policy.intent(),
            &state,
            &admitted,
            CycleObservations::default(),
        )
        .map_err(|_| TestError::Missing("blocked applies"))?;
        assert_eq!(done.terminal, Some(CycleTerminal::Blocked));
        assert_eq!(
            admit_cycle(
                &policy,
                &done,
                CycleProposal::Wait {
                    reason: reason("again")
                }
            ),
            Err(CycleRefusal::AlreadyTerminal)
        );
        Ok(())
    }

    #[test]
    fn observations_cannot_rewrite_evidence_or_invent_criteria() -> TestResult {
        let (policy, state) = fixture()?;
        let admitted = admit_cycle(
            &policy,
            &state,
            CycleProposal::Wait {
                reason: reason("x"),
            },
        )
        .map_err(|_| TestError::Missing("wait is admitted"))?;
        let rewrite = CycleObservations {
            evidence: vec![evidence(
                "evidence-1",
                "different content",
                EvidenceTrust::Verified,
            )],
            ..CycleObservations::default()
        };
        assert_eq!(
            apply_observations(policy.intent(), &state, &admitted, rewrite),
            Err(ObservationError::ConflictingEvidence)
        );
        let foreign = CycleObservations {
            criteria: BTreeMap::from([("not-a-criterion".to_owned(), "evidence-1".to_owned())]),
            ..CycleObservations::default()
        };
        assert_eq!(
            apply_observations(policy.intent(), &state, &admitted, foreign),
            Err(ObservationError::UnknownCriterion)
        );
        // An admission from an older sequence cannot be applied to a newer state.
        let next = apply_observations(
            policy.intent(),
            &state,
            &admitted,
            CycleObservations::default(),
        )
        .map_err(|_| TestError::Missing("first apply works"))?;
        assert_eq!(
            apply_observations(
                policy.intent(),
                &next,
                &admitted,
                CycleObservations::default()
            ),
            Err(ObservationError::StaleAdmission)
        );
        Ok(())
    }

    #[test]
    fn deserialize_rejects_injected_authority_fields() -> TestResult {
        for attack in [
            r#"{"kind":"complete","permission":"full_access"}"#,
            r#"{"kind":"verify","evidence_id":"e","network":"*"}"#,
            r#"{"kind":"advance","segments":[{"kind":"child","target":"explorer","agent_role":"root_orchestrator"}]}"#,
            r#"{"kind":"fork","branches":[],"join":{"kind":"all","write":true}}"#,
        ] {
            assert!(
                serde_json::from_str::<CycleProposal>(attack).is_err(),
                "accepted injected field: {attack}"
            );
        }
        assert!(serde_json::from_str::<CycleProposal>(r#"{"kind":"complete"}"#).is_ok());
        Ok(())
    }

    #[test]
    fn checkpoint_serde_round_trips_and_rejects_unknown_fields() -> TestResult {
        let (_, state) = fixture()?;
        let json = serde_json::to_value(&state).map_err(ctx("checkpoint serializes"))?;
        let back: CycleCheckpoint =
            serde_json::from_value(json.clone()).map_err(ctx("checkpoint deserializes"))?;
        assert_eq!(back, state);
        let mut tampered = json;
        if let Some(object) = tampered.as_object_mut() {
            object.insert("permission".to_owned(), serde_json::json!("full_access"));
        }
        assert!(serde_json::from_value::<CycleCheckpoint>(tampered).is_err());
        Ok(())
    }

    #[test]
    fn admission_rejects_unusable_construction() -> TestResult {
        let zero = CycleLimits {
            max_transitions: 0,
            ..limits()
        };
        assert_eq!(
            CycleAdmission::new(intent(), zero, targets()),
            Err(CycleRefusal::InvalidLimits)
        );
        let no_parallel = CycleLimits {
            max_parallel_segments: 0,
            ..limits()
        };
        assert_eq!(
            CycleAdmission::new(intent(), no_parallel, targets()),
            Err(CycleRefusal::InvalidLimits)
        );
        let mut blank = targets();
        blank.write.insert(" ".to_owned());
        assert_eq!(
            CycleAdmission::new(intent(), limits(), blank),
            Err(CycleRefusal::InvalidTarget)
        );
        let mut first_with_predecessor = intent();
        first_with_predecessor.revision = 1;
        assert_eq!(
            CycleAdmission::new(first_with_predecessor, limits(), targets()),
            Err(CycleRefusal::InvalidIntent)
        );
        Ok(())
    }

    #[test]
    fn widened_admission_against_recorded_ceiling_is_refused() -> TestResult {
        let (_, state) = fixture()?;
        let mut wider = targets();
        wider.children.insert("security-reviewer".to_owned());
        let widened = CycleAdmission::new(intent(), limits(), wider)
            .map_err(|_| TestError::Missing("wider admission builds"))?;
        assert_eq!(
            admit_cycle(
                &widened,
                &state,
                CycleProposal::Wait {
                    reason: reason("x")
                }
            ),
            Err(CycleRefusal::CeilingMismatch)
        );
        Ok(())
    }

    #[test]
    fn intent_revisions_form_an_auditable_chain() -> TestResult {
        let first = IntentBinding {
            revision: 1,
            digest: ContentDigest::of(b"intent-a@1"),
            predecessor: None,
            ..intent()
        };
        let second = intent();
        assert_eq!(second.supersedes(&first), Ok(()));
        assert_eq!(first.supersedes(&second), Err(IntentRevisionError::Stale));
        let fork = IntentBinding {
            digest: ContentDigest::of(b"other payload"),
            ..second.clone()
        };
        assert_eq!(fork.supersedes(&second), Err(IntentRevisionError::Forked));
        let skipped = IntentBinding {
            revision: 3,
            predecessor: Some(first.digest),
            ..second.clone()
        };
        assert_eq!(
            skipped.supersedes(&first),
            Err(IntentRevisionError::SkippedRevision)
        );
        let wrong_parent = IntentBinding {
            predecessor: Some(ContentDigest::of(b"unrelated")),
            ..second.clone()
        };
        assert_eq!(
            wrong_parent.supersedes(&first),
            Err(IntentRevisionError::PredecessorMismatch)
        );
        let other = IntentBinding {
            id: "intent-b".to_owned(),
            ..second
        };
        assert_eq!(
            other.supersedes(&first),
            Err(IntentRevisionError::OtherIntent)
        );
        Ok(())
    }

    #[test]
    fn commit_fence_rejects_stale_epochs_and_reordering() -> TestResult {
        let (policy, stored) = fixture()?;
        let admitted = admit_cycle(
            &policy,
            &stored,
            CycleProposal::Wait {
                reason: reason("x"),
            },
        )
        .map_err(|_| TestError::Missing("wait is admitted"))?;
        let next = apply_observations(
            policy.intent(),
            &stored,
            &admitted,
            CycleObservations::default(),
        )
        .map_err(|_| TestError::Missing("apply works"))?;
        assert_eq!(check_commit(&stored, &next), Ok(()));
        assert_eq!(check_commit(&next, &stored), Err(FenceRefusal::OutOfOrder));
        assert_eq!(check_commit(&next, &next), Err(FenceRefusal::OutOfOrder));
        let mut newer_lease = next.clone();
        newer_lease.fence.epoch = 2;
        newer_lease.fence.sequence = 2;
        assert_eq!(check_commit(&next, &newer_lease), Ok(()));
        // The old executor (epoch 1) can no longer overwrite epoch 2.
        let mut zombie = next.clone();
        zombie.fence.sequence = 3;
        assert_eq!(
            check_commit(&newer_lease, &zombie),
            Err(FenceRefusal::StaleEpoch)
        );
        let mut swapped = next.clone();
        swapped.ceiling.write.insert("secrets.toml".to_owned());
        assert_eq!(
            check_commit(&stored, &swapped),
            Err(FenceRefusal::CeilingWidened)
        );
        let mut other_intent = next;
        other_intent.intent_revision = 3;
        assert_eq!(
            check_commit(&stored, &other_intent),
            Err(FenceRefusal::IntentMismatch)
        );
        Ok(())
    }

    #[test]
    fn resume_never_widens_after_policy_or_role_upgrade() -> TestResult {
        // Admitted under read-only authority with a narrow child list.
        let narrow = CycleTargets {
            write: BTreeSet::new(),
            ..targets()
        };
        let original = CycleAdmission::new(intent(), limits(), narrow)
            .map_err(|_| TestError::Missing("original admission builds"))?;
        let read_only = authority(&[Permission::ReadWorkspace])?;
        let checkpoint = CycleCheckpoint::initial(&original, &read_only, 1);

        // After the crash the operator upgraded policy and role definition.
        let mut upgraded_targets = targets();
        upgraded_targets
            .children
            .insert("security-reviewer".to_owned());
        let upgraded_limits = CycleLimits {
            max_transitions: 100,
            max_spawn_depth: 4,
            ..limits()
        };
        let current = CycleAdmission::new(intent(), upgraded_limits, upgraded_targets)
            .map_err(|_| TestError::Missing("upgraded admission builds"))?;

        // A caller passing a broader context than the snapshot fails closed.
        let broader = authority(&[Permission::ReadWorkspace, Permission::WriteWorkspace])?;
        assert_eq!(
            resume_admission(&checkpoint, &current, &broader, 2).map(|_| ()),
            Err(ResumeRefusal::AuthorityWidened)
        );

        let (resumed, state) = resume_admission(&checkpoint, &current, &read_only, 2)
            .map_err(|_| TestError::Missing("resume under reissued authority works"))?;
        assert!(resumed.allowed_write_targets().is_empty());
        assert_eq!(resumed.allowed_children(), &set(&["explorer"]));
        assert_eq!(resumed.limits(), &limits());
        assert_eq!(state.fence.epoch, 2);
        assert!(
            admit_cycle(
                &resumed,
                &state,
                CycleProposal::Wait {
                    reason: reason("x")
                }
            )
            .is_ok()
        );
        assert_eq!(
            admit_cycle(
                &resumed,
                &state,
                CycleProposal::Advance {
                    segments: vec![Segment::Child {
                        target: "security-reviewer".to_owned()
                    }],
                }
            ),
            Err(CycleRefusal::ChildNotAllowed)
        );

        // A narrower reissue (policy downgrade) drops reads as well.
        let none = authority(&[])?;
        let (downgraded, _) = resume_admission(&checkpoint, &current, &none, 3)
            .map_err(|_| TestError::Missing("downgraded resume works"))?;
        assert!(downgraded.allowed_read_targets().is_empty());
        Ok(())
    }

    #[test]
    fn resume_refuses_changed_intent_and_other_workspace() -> TestResult {
        let (policy, checkpoint) = fixture()?;
        let next_revision = IntentBinding {
            revision: 3,
            digest: ContentDigest::of(b"intent-a@3"),
            predecessor: Some(intent().digest),
            ..intent()
        };
        let current = CycleAdmission::new(next_revision, limits(), targets())
            .map_err(|_| TestError::Missing("revision 3 builds"))?;
        let rights = authority(&[Permission::ReadWorkspace])?;
        assert_eq!(
            resume_admission(&checkpoint, &current, &rights, 2).map(|_| ()),
            Err(ResumeRefusal::IntentChanged)
        );
        let elsewhere = SandboxSpec::from_resolved(
            binding("other-workspace")?,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        assert_eq!(
            resume_admission(&checkpoint, &policy, elsewhere.authority(), 2).map(|_| ()),
            Err(ResumeRefusal::WorkspaceMismatch)
        );
        Ok(())
    }
}
