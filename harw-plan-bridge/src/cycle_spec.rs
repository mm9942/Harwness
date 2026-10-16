//! The approved, persistable specification of a read-only intent cycle and
//! its translation into the trusted runtime inputs (PL-90 W03 wiring).
//!
//! `intent_cycle.start` (harw-ops) validates a [`ReadOnlyCycleSpec`], stores
//! it in the job input and asks a human to approve it; the job worker
//! (harw-cli) rebuilds the *same* [`ReadOnlyCyclePlan`] from the stored spec
//! on every claim through [`ReadOnlyCycleSpec::plan`]. Both sides therefore
//! share one validation and one digest:
//!
//! - The intent digest is the canonical JSON of objective, read targets,
//!   acceptance checks and limits. Changing any of it is a different intent.
//! - Limits may only be *lowered* below [`MAX_CYCLE_LIMITS`].
//! - Every acceptance criterion needs a deterministic [`ContentCheck`], so a
//!   cycle that succeeds can only do so through `Verified` evidence.
//! - Write targets, children and recipes stay empty: this is the read-only
//!   explorer recipe. Corrective writes are W05 and need their own adapter.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use harw_types::ContentDigest;
use serde::{Deserialize, Serialize};

use crate::cycle_explorer::{ReadTargetError, ReadTargetMap};
use crate::cycle_verifier::{AcceptanceChecks, CheckError, ContentCheck};
use crate::intent_cycle::{CycleAdmission, CycleLimits, CycleRefusal, CycleTargets, IntentBinding};

/// Schema version of [`ReadOnlyCycleSpec`].
pub const READ_ONLY_CYCLE_SPEC_SCHEMA: u32 = 1;

/// Upper bound (characters) of the objective text.
pub const MAX_OBJECTIVE_CHARS: usize = 2_000;

/// Upper bound of read targets per cycle.
pub const MAX_READ_TARGETS: usize = 64;

/// Limits used when the caller sets none.
pub const DEFAULT_CYCLE_LIMITS: CycleLimits = CycleLimits {
    max_transitions: 24,
    max_chain_depth: 0,
    max_spawn_depth: 0,
    max_parallel_segments: 4,
    max_stall_transitions: 4,
};

/// Host ceiling: no spec may exceed these.
pub const MAX_CYCLE_LIMITS: CycleLimits = CycleLimits {
    max_transitions: 64,
    max_chain_depth: 0,
    max_spawn_depth: 0,
    max_parallel_segments: 8,
    max_stall_transitions: 8,
};

/// The approved description of one read-only intent cycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadOnlyCycleSpec {
    /// [`READ_ONLY_CYCLE_SPEC_SCHEMA`].
    pub schema_version: u32,
    /// What the cycle should establish, in the owner's words.
    pub objective: String,
    /// Read target id -> workspace-relative path.
    pub read_targets: BTreeMap<String, String>,
    /// Acceptance criterion -> deterministic check.
    pub acceptance: BTreeMap<String, ContentCheck>,
    /// Effective limits (already narrowed below [`MAX_CYCLE_LIMITS`]).
    pub limits: CycleLimits,
}

/// Why a spec is not admissible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CycleSpecError {
    UnsupportedSchema,
    BlankObjective,
    ObjectiveTooLong,
    NoReadTargets,
    TooManyReadTargets,
    NoAcceptance,
    /// A limit is above the host ceiling or unusable.
    LimitsWidened,
    Targets(ReadTargetError),
    Checks(CheckError),
    Admission(CycleRefusal),
}

impl fmt::Display for CycleSpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchema => f.write_str("unsupported intent cycle spec schema"),
            Self::BlankObjective => f.write_str("objective is required"),
            Self::ObjectiveTooLong => {
                write!(
                    f,
                    "objective is longer than {MAX_OBJECTIVE_CHARS} characters"
                )
            }
            Self::NoReadTargets => f.write_str("at least one read target is required"),
            Self::TooManyReadTargets => {
                write!(f, "more than {MAX_READ_TARGETS} read targets")
            }
            Self::NoAcceptance => f.write_str("at least one acceptance criterion is required"),
            Self::LimitsWidened => f.write_str(
                "limits must allow at least 1 transition, 1 parallel segment and 1 stall \
                 transition and may not exceed the host ceiling",
            ),
            Self::Targets(error) => write!(f, "{error}"),
            Self::Checks(error) => write!(f, "{error}"),
            Self::Admission(refusal) => write!(f, "cycle admission refused: {refusal:?}"),
        }
    }
}

impl std::error::Error for CycleSpecError {}

/// Trusted runtime inputs rebuilt from a validated spec.
#[derive(Debug, Clone)]
pub struct ReadOnlyCyclePlan {
    pub admission: CycleAdmission,
    pub targets: ReadTargetMap,
    pub checks: AcceptanceChecks,
}

impl ReadOnlyCycleSpec {
    /// A spec with [`DEFAULT_CYCLE_LIMITS`] narrowed by `limits`.
    #[must_use]
    pub fn new(
        objective: String,
        read_targets: BTreeMap<String, String>,
        acceptance: BTreeMap<String, ContentCheck>,
        limits: Option<CycleLimits>,
    ) -> Self {
        Self {
            schema_version: READ_ONLY_CYCLE_SPEC_SCHEMA,
            objective,
            read_targets,
            acceptance,
            limits: limits.unwrap_or(DEFAULT_CYCLE_LIMITS),
        }
    }

    /// Canonical digest of the approved intent payload.
    #[must_use]
    pub fn digest(&self) -> ContentDigest {
        // `BTreeMap` keys serialize sorted; the struct field order is fixed.
        // Serializing plain strings, integers and maps cannot fail; should it
        // ever, the `Debug` rendering still covers every field, so distinct
        // specs never collapse onto the digest of empty input.
        let canonical = serde_json::to_vec(self)
            .unwrap_or_else(|_| format!("harw-cycle-spec\0{self:?}").into_bytes());
        ContentDigest::of(&canonical)
    }

    /// The first-revision intent binding of this spec under `intent_id`.
    #[must_use]
    pub fn intent(&self, intent_id: &str) -> IntentBinding {
        IntentBinding {
            id: intent_id.to_owned(),
            revision: 1,
            digest: self.digest(),
            predecessor: None,
            acceptance: self.acceptance.keys().cloned().collect(),
        }
    }

    /// Validates the spec and builds admission, target map and checks.
    ///
    /// # Errors
    /// [`CycleSpecError`] naming the first violated rule.
    pub fn plan(&self, intent_id: &str) -> Result<ReadOnlyCyclePlan, CycleSpecError> {
        if self.schema_version != READ_ONLY_CYCLE_SPEC_SCHEMA {
            return Err(CycleSpecError::UnsupportedSchema);
        }
        if self.objective.trim().is_empty() {
            return Err(CycleSpecError::BlankObjective);
        }
        if self.objective.chars().count() > MAX_OBJECTIVE_CHARS {
            return Err(CycleSpecError::ObjectiveTooLong);
        }
        if self.read_targets.is_empty() {
            return Err(CycleSpecError::NoReadTargets);
        }
        if self.read_targets.len() > MAX_READ_TARGETS {
            return Err(CycleSpecError::TooManyReadTargets);
        }
        if self.acceptance.is_empty() {
            return Err(CycleSpecError::NoAcceptance);
        }
        // `max_stall_transitions == 0` would refuse every non-exit proposal.
        if !self.limits.is_valid()
            || self.limits.max_stall_transitions == 0
            || self.limits.narrowed(&MAX_CYCLE_LIMITS) != self.limits
        {
            return Err(CycleSpecError::LimitsWidened);
        }
        let targets = ReadTargetMap::new(
            self.read_targets
                .iter()
                .map(|(id, path)| (id.clone(), path.clone())),
        )
        .map_err(CycleSpecError::Targets)?;
        let checks = AcceptanceChecks::new(self.acceptance.clone(), &targets)
            .map_err(CycleSpecError::Checks)?;
        let intent = self.intent(intent_id);
        checks
            .ensure_covers(&intent.acceptance)
            .map_err(CycleSpecError::Checks)?;
        let admission = CycleAdmission::new(
            intent,
            self.limits,
            CycleTargets {
                read: targets.ids().map(str::to_owned).collect::<BTreeSet<_>>(),
                ..CycleTargets::default()
            },
        )
        .map_err(CycleSpecError::Admission)?;
        Ok(ReadOnlyCyclePlan {
            admission,
            targets,
            checks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn spec() -> ReadOnlyCycleSpec {
        ReadOnlyCycleSpec::new(
            "find out whether the marker exists".to_owned(),
            BTreeMap::from([("lib".to_owned(), "src/lib.rs".to_owned())]),
            BTreeMap::from([(
                "has-marker".to_owned(),
                ContentCheck {
                    target: "lib".to_owned(),
                    must_contain: vec!["marker".to_owned()],
                    must_not_contain: Vec::new(),
                },
            )]),
            None,
        )
    }

    #[test]
    fn valid_spec_builds_a_read_only_admission() -> TestResult {
        let plan = spec().plan("intent-1").map_err(ctx("plan"))?;
        assert_eq!(plan.admission.allowed_read_targets().len(), 1);
        assert!(plan.admission.allowed_write_targets().is_empty());
        assert!(plan.admission.allowed_children().is_empty());
        assert_eq!(plan.admission.intent().digest, spec().digest());
        Ok(())
    }

    #[test]
    fn digest_changes_with_any_approved_field() {
        let base = spec();
        let mut other = spec();
        other.objective.push('!');
        assert_ne!(base.digest(), other.digest());
        let mut other = spec();
        other.limits.max_transitions -= 1;
        assert_ne!(base.digest(), other.digest());
    }

    #[test]
    fn invalid_specs_are_refused() {
        let mut widened = spec();
        widened.limits.max_transitions = MAX_CYCLE_LIMITS.max_transitions + 1;
        assert_eq!(widened.plan("i").err(), Some(CycleSpecError::LimitsWidened));
        let mut chained = spec();
        chained.limits.max_chain_depth = 1;
        assert_eq!(chained.plan("i").err(), Some(CycleSpecError::LimitsWidened));
        let mut spawning = spec();
        spawning.limits.max_spawn_depth = 1;
        assert_eq!(spawning.plan("i").err(), Some(CycleSpecError::LimitsWidened));
        let mut no_stall = spec();
        no_stall.limits.max_stall_transitions = 0;
        assert_eq!(no_stall.plan("i").err(), Some(CycleSpecError::LimitsWidened));
        let mut blank = spec();
        blank.objective = " ".to_owned();
        assert_eq!(blank.plan("i").err(), Some(CycleSpecError::BlankObjective));
        let mut escape = spec();
        escape
            .read_targets
            .insert("up".to_owned(), "../secret".to_owned());
        assert!(matches!(
            escape.plan("i").err(),
            Some(CycleSpecError::Targets(_))
        ));
        let mut unchecked = spec();
        unchecked.acceptance.clear();
        assert_eq!(
            unchecked.plan("i").err(),
            Some(CycleSpecError::NoAcceptance)
        );
        let mut bad_id = spec();
        bad_id
            .read_targets
            .insert("has space".to_owned(), "src/x.rs".to_owned());
        assert!(matches!(
            bad_id.plan("i").err(),
            Some(CycleSpecError::Admission(CycleRefusal::InvalidTarget))
        ));
    }
}
