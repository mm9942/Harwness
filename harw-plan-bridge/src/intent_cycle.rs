//! Pure, fail-closed admission of proposed adaptive intent-cycle transitions.
//!
//! This is a planning mechanism, not a tool executor or a permission issuer.
//! In particular, a permitted patch proposal does NOT perform a filesystem
//! write. Actual workers must still pass authority, approval, path ownership,
//! branch and preimage verification at their existing execution boundary.
//! Recovery must reissue the trusted authority snapshot; this module cannot
//! mint it from serialized model content.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// A versioned intent binding. The digest is provided by the trusted caller,
/// not derived from arbitrary model output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentBinding {
    pub id: String,
    pub revision: u64,
    pub digest: String,
    pub acceptance: BTreeSet<String>,
}

/// Limits independently bound to an execution by trusted orchestration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CycleLimits {
    pub max_transitions: u32,
    pub max_chain_depth: u16,
    pub max_spawn_depth: u16,
    pub max_parallel_segments: u16,
    pub max_stall_transitions: u32,
}

/// Snapshot of admitted *identifiers* only. It never encodes a permission
/// grant, network authority or a recovered AuthoritySnapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CycleAdmission {
    pub intent: IntentBinding,
    pub limits: CycleLimits,
    pub allowed_read_targets: BTreeSet<String>,
    pub allowed_write_targets: BTreeSet<String>,
    pub allowed_children: BTreeSet<String>,
    pub allowed_recipes: BTreeSet<String>,
}

/// A checkpoint is evidence and state, not an authority source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CycleCheckpoint {
    pub intent_id: String,
    pub intent_revision: u64,
    pub intent_digest: String,
    pub transitions_used: u32,
    pub chain_depth: u16,
    pub spawn_depth: u16,
    pub stall_transitions: u32,
    pub claims: BTreeMap<String, String>,
    pub evidence_ids: BTreeSet<String>,
    pub criteria_met: BTreeSet<String>,
}

/// A requested graph segment; targets are registry identifiers (not tools,
/// filesystem paths, arbitrary model-defined roles or authority grants).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Segment {
    Read { target: String },
    Child { target: String },
    NestedRecipe { recipe: String },
}

/// The LLM may propose a transition, never commit it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CycleProposal {
    Advance { segments: Vec<Segment> },
    Reorient { evidence_id: String, segments: Vec<Segment> },
    ProposePatch { target: String, evidence_id: String },
    Verify { evidence_id: String },
    Complete,
    Wait { reason: String },
    Escalate { reason: String },
}

/// An accepted plan, not an executed action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedCycle {
    pub proposal: CycleProposal,
    pub next_transitions_used: u32,
    pub next_stall_transitions: u32,
}

/// Specific refusals can be surfaced without retrying the same unusable route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleRefusal {
    InvalidIntent,
    Exhausted,
    Stalled,
    EmptyOrOversizedGraph,
    ReadNotAllowed,
    WriteNotAllowed,
    ChildNotAllowed,
    RecipeNotAllowed,
    ChainDepthExceeded,
    SpawnDepthExceeded,
    MissingEvidence,
    IncompleteAcceptance,
    InvalidReason,
}

/// Pure decision function. `has_progress` is a trusted observation by the
/// caller, never a model-controlled flag. It updates the stall counter but
/// does not waive any of the policy checks.
pub fn admit_cycle(
    policy: &CycleAdmission,
    state: &CycleCheckpoint,
    proposal: CycleProposal,
    has_progress: bool,
) -> Result<AdmittedCycle, CycleRefusal> {
    if policy.intent.id.is_empty()
        || policy.intent.digest.is_empty()
        || state.intent_id != policy.intent.id
        || state.intent_revision != policy.intent.revision
        || state.intent_digest != policy.intent.digest
    {
        return Err(CycleRefusal::InvalidIntent);
    }
    if policy.limits.max_transitions == 0
        || state.transitions_used >= policy.limits.max_transitions
    {
        return Err(CycleRefusal::Exhausted);
    }
    if state.chain_depth > policy.limits.max_chain_depth
        || state.spawn_depth > policy.limits.max_spawn_depth
    {
        return Err(CycleRefusal::ChainDepthExceeded);
    }

    let stall = if has_progress {
        0
    } else {
        state.stall_transitions.saturating_add(1)
    };
    if stall > policy.limits.max_stall_transitions {
        return Err(CycleRefusal::Stalled);
    }
    match &proposal {
        CycleProposal::Advance { segments }
        | CycleProposal::Reorient { segments, .. } => {
            if segments.is_empty()
                || segments.len() > usize::from(policy.limits.max_parallel_segments)
            {
                return Err(CycleRefusal::EmptyOrOversizedGraph);
            }
            for segment in segments {
                match segment {
                    Segment::Read { target } => {
                        if target.is_empty() || !policy.allowed_read_targets.contains(target) {
                            return Err(CycleRefusal::ReadNotAllowed);
                        }
                    }
                    Segment::Child { target } => {
                        if target.is_empty() || !policy.allowed_children.contains(target) {
                            return Err(CycleRefusal::ChildNotAllowed);
                        }
                        if state.spawn_depth >= policy.limits.max_spawn_depth {
                            return Err(CycleRefusal::SpawnDepthExceeded);
                        }
                    }
                    Segment::NestedRecipe { recipe } => {
                        if recipe.is_empty() || !policy.allowed_recipes.contains(recipe) {
                            return Err(CycleRefusal::RecipeNotAllowed);
                        }
                        if state.chain_depth >= policy.limits.max_chain_depth {
                            return Err(CycleRefusal::ChainDepthExceeded);
                        }
                    }
                }
            }
            if let CycleProposal::Reorient { evidence_id, .. } = &proposal {
                if !state.evidence_ids.contains(evidence_id) {
                    return Err(CycleRefusal::MissingEvidence);
                }
            }
        }
        CycleProposal::ProposePatch {
            target,
            evidence_id,
        } => {
            if target.is_empty() || !policy.allowed_write_targets.contains(target) {
                return Err(CycleRefusal::WriteNotAllowed);
            }
            if !state.evidence_ids.contains(evidence_id) {
                return Err(CycleRefusal::MissingEvidence);
            }
        }
        CycleProposal::Verify { evidence_id } => {
            if !state.evidence_ids.contains(evidence_id) {
                return Err(CycleRefusal::MissingEvidence);
            }
        }
        CycleProposal::Complete => {
            if !policy.intent.acceptance.is_subset(&state.criteria_met) {
                return Err(CycleRefusal::IncompleteAcceptance);
            }
        }
        CycleProposal::Wait { reason } | CycleProposal::Escalate { reason } => {
            if reason.trim().is_empty() {
                return Err(CycleRefusal::InvalidReason);
            }
        }
    }
    Ok(AdmittedCycle {
        proposal,
        next_transitions_used: state.transitions_used + 1,
        next_stall_transitions: stall,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    fn fixture() -> (CycleAdmission, CycleCheckpoint) {
        let policy = CycleAdmission {
            intent: IntentBinding {
                id: "intent-a".to_owned(),
                revision: 2,
                digest: "trusted-digest".to_owned(),
                acceptance: BTreeSet::from(["tested".to_owned()]),
            },
            limits: CycleLimits {
                max_transitions: 8,
                max_chain_depth: 3,
                max_spawn_depth: 1,
                max_parallel_segments: 2,
                max_stall_transitions: 2,
            },
            allowed_read_targets: BTreeSet::from(["source".to_owned()]),
            allowed_write_targets: BTreeSet::from(["code.rs".to_owned()]),
            allowed_children: BTreeSet::from(["explorer".to_owned()]),
            allowed_recipes: BTreeSet::from(["hypothesis".to_owned()]),
        };
        let state = CycleCheckpoint {
            intent_id: "intent-a".to_owned(),
            intent_revision: 2,
            intent_digest: "trusted-digest".to_owned(),
            transitions_used: 0,
            chain_depth: 0,
            spawn_depth: 0,
            stall_transitions: 0,
            claims: BTreeMap::new(),
            evidence_ids: BTreeSet::from(["evidence-1".to_owned()]),
            criteria_met: BTreeSet::new(),
        };
        (policy, state)
    }

    #[test]
    fn reorientation_requires_real_observation() -> TestResult {
        let (policy, state) = fixture();
        let proposal = CycleProposal::Reorient {
            evidence_id: "invented".to_owned(),
            segments: vec![Segment::Read {
                target: "source".to_owned(),
            }],
        };
        assert_eq!(
            admit_cycle(&policy, &state, proposal, true),
            Err(CycleRefusal::MissingEvidence)
        );
        Ok(())
    }

    #[test]
    fn forbidden_nested_agent_is_rejected() -> TestResult {
        let (policy, state) = fixture();
        let proposal = CycleProposal::Advance {
            segments: vec![Segment::Child {
                target: "matrix-game-master".to_owned(),
            }],
        };
        assert_eq!(
            admit_cycle(&policy, &state, proposal, true),
            Err(CycleRefusal::ChildNotAllowed)
        );
        Ok(())
    }

    #[test]
    fn nested_chains_have_separate_depth() -> TestResult {
        let (policy, mut state) = fixture();
        state.chain_depth = 3;
        let nested = CycleProposal::Advance {
            segments: vec![Segment::NestedRecipe {
                recipe: "hypothesis".to_owned(),
            }],
        };
        assert_eq!(
            admit_cycle(&policy, &state, nested, true),
            Err(CycleRefusal::ChainDepthExceeded)
        );
        let read = CycleProposal::Advance {
            segments: vec![Segment::Read {
                target: "source".to_owned(),
            }],
        };
        assert!(admit_cycle(&policy, &state, read, true).is_ok());
        Ok(())
    }

    #[test]
    fn patch_proposal_needs_both_scope_and_evidence() -> TestResult {
        let (policy, state) = fixture();
        let patch = |path: &str, evidence: &str| CycleProposal::ProposePatch {
            target: path.to_owned(),
            evidence_id: evidence.to_owned(),
        };
        assert_eq!(
            admit_cycle(&policy, &state, patch("other.rs", "evidence-1"), true),
            Err(CycleRefusal::WriteNotAllowed)
        );
        assert_eq!(
            admit_cycle(&policy, &state, patch("code.rs", "invented"), true),
            Err(CycleRefusal::MissingEvidence)
        );
        assert!(admit_cycle(&policy, &state, patch("code.rs", "evidence-1"), true).is_ok());
        Ok(())
    }

    #[test]
    fn stale_intent_and_unproven_completion_are_refused() -> TestResult {
        let (policy, mut state) = fixture();
        state.intent_revision = 1;
        assert_eq!(
            admit_cycle(&policy, &state, CycleProposal::Complete, true),
            Err(CycleRefusal::InvalidIntent)
        );
        state.intent_revision = 2;
        assert_eq!(
            admit_cycle(&policy, &state, CycleProposal::Complete, true),
            Err(CycleRefusal::IncompleteAcceptance)
        );
        state.criteria_met.insert("tested".to_owned());
        assert!(admit_cycle(&policy, &state, CycleProposal::Complete, true).is_ok());
        Ok(())
    }

    #[test]
    fn progress_and_stall_are_runtime_observations() -> TestResult {
        let (policy, mut state) = fixture();
        state.stall_transitions = 2;
        assert_eq!(
            admit_cycle(&policy, &state, CycleProposal::Wait {
                reason: "event".to_owned(),
            }, false),
            Err(CycleRefusal::Stalled)
        );
        assert!(admit_cycle(&policy, &state, CycleProposal::Wait {
            reason: "event".to_owned(),
        }, true).is_ok());
        state.transitions_used = policy.limits.max_transitions;
        assert_eq!(
            admit_cycle(&policy, &state, CycleProposal::Complete, true),
            Err(CycleRefusal::Exhausted)
        );
        Ok(())
    }

    #[test]
    fn deserialize_rejects_injected_authority_fields() -> TestResult {
        let attack = r#"{"kind":"complete","permission":"full_access"}"#;
        assert!(serde_json::from_str::<CycleProposal>(attack).is_err());
        Ok(())
    }
}
