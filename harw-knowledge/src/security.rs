//! Durable security artifacts: [`SecurityFinding`] and [`Baseline`] (AW4-05).
//!
//! Spec: `docs/design/knowledge-surfaces.md` §8.1, extended by the AW4
//! Ausbauprogramm (node AW4-05). Both types wrap frozen evidence from
//! `harw-dod-signals` (`SecurityEvidence`, `Hardness`, `Severity`) so a
//! finding or baseline always points back to exactly what was observed,
//! never to a re-derived summary of it.
//!
//! Neither type builds the rule engine itself: `Finding<S>` and its three
//! typestate transitions live in `harw-dod-rules` (node AW4-03), because
//! their `pub(crate)` constructors must stay reachable from eleven sensor
//! crates that cannot depend on this one. This module owns only the
//! durable, recallable knowledge-surface projection those rules read and
//! write through — the artifact, not the engine.
//!
//! # Why `Baseline` converts into `harw_dod_rules::baseline::Baseline`
//! instead of `harw-dod-rules` reexporting this type
//! `harw-dod-rules` used to `pub use` this module's [`Baseline`] (and
//! `harw_knowledge::memory::palace::PalaceStatus`) directly. That reexport
//! dragged this crate's entire dependency graph — `harw-model-catalog` and,
//! transitively, a full async HTTP stack (`reqwest` + `tokio`) — into every
//! consumer of `harw-dod-rules`, including `harw-sentinel`, an
//! unprivileged security sensor binary that has no business linking an
//! HTTP client. `harw-dod-rules` now defines its own lightweight
//! `harw_dod_rules::baseline::Baseline`/`PalaceStatus` (pure data, no
//! knowledge-surface dependencies), and [`Baseline::to_rule_baseline`]
//! projects this durable artifact onto that lightweight shape. This is the
//! same pattern correction K3 established for `Confidence` (vocabulary
//! moves to the thin side, the heavy side converts), with the roles
//! reversed: here `harw-dod-rules` is the thin, foundational side and this
//! crate is the heavy, aggregating side that converts down to it, rather
//! than the thin side importing up. This crate now depends on
//! `harw-dod-rules` (see `Cargo.toml`) — checked cycle-free: `harw-dod-rules`
//! depends on this crate only as a `[dev-dependencies]` entry (for its own
//! test/doctest convenience), and Cargo explicitly supports dev-dependency
//! cycles since they never enter the production dependency graph.
//!
//! [`PalaceStatus`] itself is unchanged by this: it stays exactly the
//! lifecycle type it already was, and this module still does not map it to
//! any confidence scale — [`Baseline::to_rule_baseline`] only translates it
//! one-for-one into `harw_dod_rules::baseline::PalaceStatus`'s three
//! matching variants, never into a graded confidence value.
//!
//! # Errors
//! Fallible paths return [`crate::error::KnowledgeError`] /
//! [`crate::error::KnowledgeResult`], same as every other surface.
//!
//! # Concurrency
//! Plain `Send + Sync` serde data; no shared state, no threads spawned here.
//!
//! # Examples
//! ```rust
//! use harw_dod_signals::{SecurityEvidence, Severity};
//! use harw_knowledge::ArtifactId;
//! use harw_knowledge::security::SecurityFinding;
//!
//! let evidence = SecurityEvidence::capture(vec![], vec![], jiff::Timestamp::UNIX_EPOCH)
//!     .expect("empty evidence always encodes");
//! let finding = SecurityFinding::new(
//!     ArtifactId::new("security/example"),
//!     "example finding",
//!     Severity::Low,
//!     evidence,
//! );
//! assert_eq!(finding.severity, Severity::Low);
//! ```

use serde::{Deserialize, Serialize};

use harw_dod_signals::{Hardness, SecurityEvidence, Severity};

use crate::artifact::ArtifactId;
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::memory::palace::PalaceStatus;

/// A durable security finding surfaced from frozen sensor evidence (§8.1,
/// AW4-05).
///
/// This is precisely the kind of artifact `VisibilityScope::OperatorOnly`
/// (§7) was designed for: raw evidence and severity are human-operator
/// material by default, not agent-recallable, until an operator scope proves
/// it should be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecurityFinding {
    /// Stable id (`security/<slug>`).
    pub id: ArtifactId,
    /// Human-readable summary of what was found.
    pub title: String,
    /// How heavily this finding weighs (`harw_dod_signals::Severity`).
    pub severity: Severity,
    /// The frozen observations this finding is backed by; never a
    /// re-derived summary, so the finding always points back to exactly
    /// what was observed.
    pub evidence: SecurityEvidence,
    /// Topical tags, independent of the link graph.
    pub tags: Vec<String>,
}

impl SecurityFinding {
    /// Assemble a security finding from its id, title, severity and evidence.
    #[must_use]
    pub fn new(
        id: ArtifactId,
        title: impl Into<String>,
        severity: Severity,
        evidence: SecurityEvidence,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            severity,
            evidence,
            tags: Vec::new(),
        }
    }
}

/// A frozen behavioral baseline that AW4-03's rule engine checks new
/// observations against (§8.1, AW4-05).
///
/// # Why `status` gates what the baseline is allowed to do
/// AW4-03's anomaly/rule engine treats the two non-`Superseded` statuses
/// differently: an `Established` baseline is trusted enough to *trigger a
/// rule* outright, while a `Provisional` one may only *report an anomaly*
/// for a human to weigh. Whether a deviation from this baseline is
/// actionable or merely informative therefore reads directly off `status` —
/// the same field a palace node already carries, not a second parallel
/// one — which is exactly why it lives on this type even though the rule
/// that consumes it lives in `harw-dod-rules`, not here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    /// Stable id (`baseline/<slug>`).
    pub id: ArtifactId,
    /// Human-readable description of what this baseline characterizes.
    pub title: String,
    /// Epistemic distance of the evidence this baseline was derived from
    /// (`harw_dod_signals::Hardness`).
    pub hardness: Hardness,
    /// The frozen observations this baseline was derived from.
    pub evidence: SecurityEvidence,
    /// Lifecycle status: gates whether AW4-03 may trigger a rule
    /// (`Established`) or only report an anomaly (`Provisional`) against
    /// this baseline. See [`PalaceStatus`] for why this is a lifecycle
    /// field, not a confidence score.
    pub status: PalaceStatus,
    /// Topical tags, independent of the link graph.
    pub tags: Vec<String>,
}

impl Baseline {
    /// Create a fresh, `Provisional` baseline (mirrors
    /// [`crate::memory::palace::PalaceNode::new`], which also starts every
    /// new node `Provisional`).
    #[must_use]
    pub fn new(
        id: ArtifactId,
        title: impl Into<String>,
        hardness: Hardness,
        evidence: SecurityEvidence,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            hardness,
            evidence,
            status: PalaceStatus::Provisional,
            tags: Vec::new(),
        }
    }

    /// Promote this baseline to `Established`.
    ///
    /// # Description
    /// Every palace promotion in this crate is gated the same way: refuse to
    /// commit without a recorded review (§2.5). This mirrors that exact
    /// contract — a `reviewed` flag and [`KnowledgeError::PromotionNotReviewed`]
    /// on refusal — rather than building a second, parallel gate.
    /// [`crate::memory::palace::ensure_promotion_reviewed`] is not called
    /// directly here because it hardcodes a `topic/`-prefixed `from` label
    /// for the topic-to-palace case (§2.5); a baseline promotion is not a
    /// promotion *from a topic*, so this constructs the same error variant
    /// with baseline-shaped labels instead.
    ///
    /// # Errors
    /// [`KnowledgeError::PromotionNotReviewed`] unless `reviewed` is `true`
    /// (an explicit operator review, per the same rule as every other
    /// palace promotion).
    pub fn promote_to_established(&mut self, reviewed: bool) -> KnowledgeResult<()> {
        if !reviewed {
            return Err(KnowledgeError::PromotionNotReviewed {
                from: format!("baseline/{}:provisional", self.id),
                to: format!("baseline/{}:established", self.id),
            });
        }
        self.status = PalaceStatus::Established;
        Ok(())
    }

    /// Projects this durable baseline onto the lightweight shape
    /// `harw-dod-rules`'s rule engine reads.
    ///
    /// # Description
    /// See module doc, section "Why `Baseline` converts...". Copies
    /// `title`/`hardness`/`evidence`/`tags` unchanged, formats `id` via its
    /// `Display` impl (matching `harw_dod_rules::baseline::Baseline::new`'s
    /// `impl std::fmt::Display` parameter), and maps `status` one-for-one
    /// onto `harw_dod_rules::baseline::PalaceStatus`'s matching variant —
    /// never onto a confidence scale.
    ///
    /// # Returns
    /// A `harw_dod_rules::baseline::Baseline` carrying the same evidence and
    /// lifecycle status, ready for `harw_dod_rules::rule::RuleContext::baselines`.
    #[must_use]
    pub fn to_rule_baseline(&self) -> harw_dod_rules::baseline::Baseline {
        harw_dod_rules::baseline::Baseline {
            id: self.id.to_string(),
            title: self.title.clone(),
            hardness: self.hardness,
            evidence: self.evidence.clone(),
            status: match self.status {
                PalaceStatus::Established => harw_dod_rules::baseline::PalaceStatus::Established,
                PalaceStatus::Provisional => harw_dod_rules::baseline::PalaceStatus::Provisional,
                PalaceStatus::Superseded => harw_dod_rules::baseline::PalaceStatus::Superseded,
            },
            tags: self.tags.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Baseline, SecurityFinding};
    use crate::artifact::ArtifactId;
    use crate::error::KnowledgeError;
    use crate::memory::palace::PalaceStatus;

    use harw_dod_signals::{Hardness, SecurityEvidence, Severity};
    use jiff::Timestamp;

    fn evidence() -> SecurityEvidence {
        SecurityEvidence::capture(vec![], vec![], Timestamp::UNIX_EPOCH)
            .expect("empty evidence always encodes")
    }

    #[test]
    fn test_security_finding_new_carries_the_given_severity_and_evidence() {
        let finding = SecurityFinding::new(
            ArtifactId::new("security/example"),
            "example finding",
            Severity::Critical,
            evidence(),
        );

        assert_eq!(finding.severity, Severity::Critical);
        assert_eq!(finding.evidence, evidence());
        assert!(finding.tags.is_empty());
    }

    #[test]
    fn test_baseline_new_starts_provisional() {
        let baseline = Baseline::new(
            ArtifactId::new("baseline/example"),
            "example baseline",
            Hardness::Observed,
            evidence(),
        );

        assert_eq!(baseline.status, PalaceStatus::Provisional);
    }

    #[test]
    fn test_baseline_promotion_without_review_is_rejected() {
        let mut baseline = Baseline::new(
            ArtifactId::new("baseline/example"),
            "example baseline",
            Hardness::Observed,
            evidence(),
        );

        let error = baseline
            .promote_to_established(false)
            .expect_err("unreviewed promotion must be refused");

        assert!(matches!(error, KnowledgeError::PromotionNotReviewed { .. }));
        assert_eq!(baseline.status, PalaceStatus::Provisional);
    }

    #[test]
    fn test_baseline_promotion_with_review_becomes_established() {
        let mut baseline = Baseline::new(
            ArtifactId::new("baseline/example"),
            "example baseline",
            Hardness::Observed,
            evidence(),
        );

        baseline
            .promote_to_established(true)
            .expect("reviewed promotion succeeds");

        assert_eq!(baseline.status, PalaceStatus::Established);
    }

    /// The new path a `Baseline` takes to reach `harw-dod-rules`' rule
    /// engine: a conversion, not a reexport (see module doc).
    #[test]
    fn test_to_rule_baseline_carries_evidence_and_status_across() {
        let mut baseline = Baseline::new(
            ArtifactId::new("baseline/example"),
            "example baseline",
            Hardness::Observed,
            evidence(),
        );
        baseline
            .promote_to_established(true)
            .expect("reviewed promotion succeeds");

        let rule_baseline = baseline.to_rule_baseline();

        assert_eq!(rule_baseline.id, "baseline/example");
        assert_eq!(rule_baseline.title, "example baseline");
        assert_eq!(rule_baseline.evidence, evidence());
        assert_eq!(
            rule_baseline.status,
            harw_dod_rules::baseline::PalaceStatus::Established
        );
    }

    /// `PalaceStatus` (the lifecycle) must never collapse into a graded
    /// confidence value across this conversion — each of the three
    /// lifecycle phases maps to its own, distinct
    /// `harw_dod_rules::baseline::PalaceStatus` variant, one-for-one.
    #[test]
    fn test_to_rule_baseline_maps_all_three_statuses_distinctly() {
        let provisional = Baseline::new(
            ArtifactId::new("baseline/a"),
            "a",
            Hardness::Observed,
            evidence(),
        )
        .to_rule_baseline()
        .status;

        let mut established_source = Baseline::new(
            ArtifactId::new("baseline/b"),
            "b",
            Hardness::Observed,
            evidence(),
        );
        established_source
            .promote_to_established(true)
            .expect("reviewed promotion succeeds");
        let established = established_source.to_rule_baseline().status;

        let mut superseded_source = Baseline::new(
            ArtifactId::new("baseline/c"),
            "c",
            Hardness::Observed,
            evidence(),
        );
        superseded_source.status = PalaceStatus::Superseded;
        let superseded = superseded_source.to_rule_baseline().status;

        assert_eq!(provisional, harw_dod_rules::baseline::PalaceStatus::Provisional);
        assert_eq!(established, harw_dod_rules::baseline::PalaceStatus::Established);
        assert_eq!(superseded, harw_dod_rules::baseline::PalaceStatus::Superseded);
    }
}
