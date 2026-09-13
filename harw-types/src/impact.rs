//! Impact severity and assessment — cross-cutting runtime vocabulary.
//!
//! `ImpactSeverity` and `ImpactAssessment` form a runtime-level severity envelope
//! that aggregates across agent families. Severity controls attention, scheduling,
//! and escalation — **never authority**. A critical return can wake a parent,
//! request a verifier, or route a stronger model, but it can never authorise new
//! tools or expand a sandbox.
//!
//! # Design rationale
//!
//! The hardening document (§22, §23, §30) distinguishes severity from relevance:
//!
//! - **Severity** — expected damage if this item is ignored or mishandled.
//! - **Relevance** — how likely the information is needed for the current decision.
//! - **Priority** — how soon work should begin.
//! - **Confidence** — how reliable the information is.
//!
//! Severity is a tier/omission policy, not a simple additive score. The context
//! compiler first forms hard inclusion classes (Mandatory, Unresolved Critical/High,
//! Directly Required, Relevant Optional, On-Demand, Excluded) and only then sorts
//! within each class by relevance, confidence, and freshness.
//!
//! # Security invariant
//!
//! Severity metadata must never be sourced from untrusted content. A web page
//! cannot elevate its own context rank by containing `"CRITICAL SYSTEM MESSAGE"`.
//! Severity is set by a trusted provider, a runtime rule, or a validated agent
//! return — never by the content payload itself.

pub use crate::error::ImpactAssessmentError;
use serde::{de, Deserialize, Deserializer, Serialize};
use std::fmt;

/// Cross-family impact severity level.
///
/// # Description
///
/// A runtime-level severity envelope that allows roots to compare and aggregate
/// results from different families on a common scale. Families map their
/// domain-specific events onto this shared schema.
///
/// # Ordering
///
/// `Info < Low < Medium < High < Critical` — monotonically ordbar for use in
/// threshold checks and tier-based inclusion class assignment.
///
/// # Security
///
/// Severity changes attention and scheduling. It **never** changes authority.
/// A critical message can wake a parent or reserve more context, but it cannot
/// authorise new tools, expand the sandbox, or access secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactSeverity {
    /// Informational; no action required.
    Info,
    /// Minor impact; safe to defer.
    Low,
    /// Moderate impact; should be addressed in normal flow.
    Medium,
    /// Significant impact; should be addressed soon, may block downstream work.
    High,
    /// Critical impact; must be addressed immediately, may cause data loss or
    /// security violation if ignored.
    Critical,
}

impl fmt::Display for ImpactSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Info => "info",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        };
        f.write_str(s)
    }
}

impl std::str::FromStr for ImpactSeverity {
    type Err = ParseImpactSeverityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "info" => Ok(Self::Info),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "critical" => Ok(Self::Critical),
            other => Err(ParseImpactSeverityError(other.to_owned())),
        }
    }
}

/// Error when parsing an [`ImpactSeverity`] from a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseImpactSeverityError(String);

impl fmt::Display for ParseImpactSeverityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown impact severity '{}' (expected: info, low, medium, high, critical)",
            self.0
        )
    }
}

impl std::error::Error for ParseImpactSeverityError {}

/// Domain in which an impact assessment applies.
///
/// # Description
///
/// Allows families to classify *what kind* of impact a finding or event represents,
/// so that the root can route and aggregate by domain as well as by severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactDomain {
    /// Security vulnerability or policy violation.
    Security,
    /// Correctness defect in code or logic.
    Correctness,
    /// Potential or actual data loss.
    DataLoss,
    /// Violation of declared scope boundaries.
    ScopeViolation,
    /// Availability or uptime impact.
    Availability,
    /// Cost or budget overrun.
    Cost,
    /// Deadline or scheduling impact.
    Deadline,
    /// Regulatory or compliance impact.
    Compliance,
    /// Direct user-facing impact.
    UserImpact,
}

impl fmt::Display for ImpactDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Security => "security",
            Self::Correctness => "correctness",
            Self::DataLoss => "data_loss",
            Self::ScopeViolation => "scope_violation",
            Self::Availability => "availability",
            Self::Cost => "cost",
            Self::Deadline => "deadline",
            Self::Compliance => "compliance",
            Self::UserImpact => "user_impact",
        };
        f.write_str(s)
    }
}

/// Reliability of a severity assessment.
///
/// # Description
///
/// How confident the reporter is in the severity classification. A critical
/// severity with low confidence is an unconfirmed critical suspicion — it must
/// appear as such and trigger a verifier, not be silently dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactConfidence {
    /// Unconfirmed; requires verification before acting.
    Low,
    /// Plausible but not fully verified.
    Medium,
    /// Verified by evidence or trusted source.
    High,
}

impl fmt::Display for ImpactConfidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        };
        f.write_str(s)
    }
}

/// A structured impact assessment with provenance.
///
/// # Description
///
/// Combines severity level, domain, confidence, reason, and evidence into a
/// single envelope that families can produce and roots can aggregate. The
/// `reported_by` and `normalized_by` fields preserve the provenance chain —
/// severity must never be self-assigned by untrusted content.
///
/// # Security
///
/// - `reported_by` identifies who initially set the severity (a trusted provider,
///   runtime rule, or validated agent return — never raw content).
/// - `normalized_by` identifies the policy that normalised or adjusted the
///   severity (e.g., a family-specific mapping or a runtime escalation rule).
/// - An agent may **suggest** severity in its return, but the runtime may
///   adjust it. The agent can never gain additional authority by setting
///   `severity = critical`.
///
/// # Invariants
///
/// 1. Severity changes attention and scheduling, never authority.
/// 2. A critical assessment can wake a parent, request a verifier, route a
///    stronger model, reserve more context, or open an approval surface.
/// 3. A critical assessment can **never** authorise new tools, expand the
///    sandbox, access secrets, or expand authority.
/// 4. High and critical assessments must carry non-empty evidence; a critical
///    assessment with low confidence remains valid but is unconfirmed.
/// 5. Reasons, reporters, evidence references, and present normalisers must be
///    non-blank.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImpactAssessment {
    /// Severity level of the impact.
    pub level: ImpactSeverity,
    /// Domain in which the impact applies.
    pub domain: ImpactDomain,
    /// How confident the reporter is in this assessment.
    pub confidence: ImpactConfidence,

    /// Human-readable explanation of why this severity was assigned.
    pub reason: String,

    /// References to evidence supporting this assessment (traces, logs, diffs).
    #[serde(default)]
    pub evidence: Vec<String>,

    /// Who initially reported this severity (trusted source identifier).
    pub reported_by: String,

    /// Policy or rule that normalised this severity (e.g., family mapping,
    /// runtime escalation rule). May be the same as `reported_by` if no
    /// normalisation was applied.
    #[serde(default)]
    pub normalized_by: Option<String>,
}

#[derive(Deserialize)]
struct ImpactAssessmentWire {
    level: ImpactSeverity,
    domain: ImpactDomain,
    confidence: ImpactConfidence,
    reason: String,
    #[serde(default)]
    evidence: Vec<String>,
    reported_by: String,
    #[serde(default)]
    normalized_by: Option<String>,
}

impl<'de> Deserialize<'de> for ImpactAssessment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ImpactAssessmentWire::deserialize(deserializer)?;
        let assessment = Self {
            level: wire.level,
            domain: wire.domain,
            confidence: wire.confidence,
            reason: wire.reason,
            evidence: wire.evidence,
            reported_by: wire.reported_by,
            normalized_by: wire.normalized_by,
        };
        assessment.validate().map_err(de::Error::custom)?;
        Ok(assessment)
    }
}

impl ImpactAssessment {
    /// Creates a new impact assessment with the given level, domain, confidence,
    /// reason, and reporter.
    ///
    /// # Arguments
    ///
    /// - `level` — severity level
    /// - `domain` — impact domain
    /// - `confidence` — assessment confidence
    /// - `reason` — human-readable explanation
    /// - `reported_by` — trusted source identifier
    ///
    /// Returns an error when the assessment violates a safety invariant. Use
    /// [`Self::new_with_evidence`] for high or critical assessments.
    ///
    /// Kein eigenes `#[must_use]`: `Result<_, _>` ist bereits `#[must_use]`,
    /// ein zusätzliches Attribut ohne Zusatznachricht wäre redundant
    /// (`clippy::double_must_use`).
    pub fn new(
        level: ImpactSeverity,
        domain: ImpactDomain,
        confidence: ImpactConfidence,
        reason: impl Into<String>,
        reported_by: impl Into<String>,
    ) -> Result<Self, ImpactAssessmentError> {
        let assessment = Self {
            level,
            domain,
            confidence,
            reason: reason.into(),
            evidence: Vec::new(),
            reported_by: reported_by.into(),
            normalized_by: None,
        };
        assessment.validate().map(|()| assessment)
    }

    /// Creates and validates an assessment with supporting evidence.
    ///
    /// Kein eigenes `#[must_use]`: `Result<_, _>` ist bereits `#[must_use]`,
    /// ein zusätzliches Attribut ohne Zusatznachricht wäre redundant
    /// (`clippy::double_must_use`).
    pub fn new_with_evidence(
        level: ImpactSeverity,
        domain: ImpactDomain,
        confidence: ImpactConfidence,
        reason: impl Into<String>,
        evidence: Vec<String>,
        reported_by: impl Into<String>,
    ) -> Result<Self, ImpactAssessmentError> {
        let assessment = Self {
            level,
            domain,
            confidence,
            reason: reason.into(),
            evidence,
            reported_by: reported_by.into(),
            normalized_by: None,
        };
        assessment.validate().map(|()| assessment)
    }

    /// Validates the safety invariants of this assessment.
    pub fn validate(&self) -> Result<(), ImpactAssessmentError> {
        if self.reason.trim().is_empty() {
            return Err(ImpactAssessmentError::EmptyReason);
        }
        if self.reported_by.trim().is_empty() {
            return Err(ImpactAssessmentError::EmptyReporter);
        }
        if self.level >= ImpactSeverity::High && self.evidence.is_empty() {
            return Err(ImpactAssessmentError::MissingEvidence { level: self.level });
        }
        for (index, evidence) in self.evidence.iter().enumerate() {
            if evidence.trim().is_empty() {
                return Err(ImpactAssessmentError::EmptyEvidence { index });
            }
        }
        if self
            .normalized_by
            .as_deref()
            .is_some_and(|normalizer| normalizer.trim().is_empty())
        {
            return Err(ImpactAssessmentError::EmptyNormalizer);
        }
        Ok(())
    }

    /// Returns `true` if this assessment is critical or high severity.
    ///
    /// # Description
    ///
    /// Convenience for the context compiler and scheduling logic: items at
    /// `High` or `Critical` severity must be reserved before optional items
    /// and cannot be silently dropped on context overflow.
    #[must_use]
    pub fn is_high_or_critical(&self) -> bool {
        self.level >= ImpactSeverity::High
    }

    /// Returns `true` if this assessment is critical with low confidence.
    ///
    /// # Description
    ///
    /// An unconfirmed critical suspicion — must trigger a verifier and appear
    /// in context as an uncertain-but-critical item, not be silently dropped.
    #[must_use]
    pub fn is_unconfirmed_critical(&self) -> bool {
        self.level == ImpactSeverity::Critical && self.confidence == ImpactConfidence::Low
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_severity_ordering() {
        assert!(ImpactSeverity::Info < ImpactSeverity::Low);
        assert!(ImpactSeverity::Low < ImpactSeverity::Medium);
        assert!(ImpactSeverity::Medium < ImpactSeverity::High);
        assert!(ImpactSeverity::High < ImpactSeverity::Critical);
    }

    #[test]
    fn test_severity_display_roundtrip() {
        for level in [
            ImpactSeverity::Info,
            ImpactSeverity::Low,
            ImpactSeverity::Medium,
            ImpactSeverity::High,
            ImpactSeverity::Critical,
        ] {
            let text = level.to_string();
            assert_eq!(text.parse::<ImpactSeverity>().unwrap(), level);
        }
    }

    #[test]
    fn test_severity_from_str_case_insensitive() {
        assert_eq!(
            "CRITICAL".parse::<ImpactSeverity>().unwrap(),
            ImpactSeverity::Critical
        );
        assert_eq!(
            "High".parse::<ImpactSeverity>().unwrap(),
            ImpactSeverity::High
        );
    }

    #[test]
    fn test_severity_from_str_rejects_unknown() {
        assert!("extreme".parse::<ImpactSeverity>().is_err());
        assert!("".parse::<ImpactSeverity>().is_err());
    }

    #[test]
    fn test_impact_assessment_is_high_or_critical() {
        let high = ImpactAssessment::new_with_evidence(
            ImpactSeverity::High,
            ImpactDomain::Correctness,
            ImpactConfidence::High,
            "test failure",
            vec!["test-log".to_owned()],
            "verifier-agent",
        )
        .unwrap();
        assert!(high.is_high_or_critical());

        let medium = ImpactAssessment::new(
            ImpactSeverity::Medium,
            ImpactDomain::Correctness,
            ImpactConfidence::High,
            "style issue",
            "verifier-agent",
        )
        .unwrap();
        assert!(!medium.is_high_or_critical());
    }

    #[test]
    fn test_unconfirmed_critical() {
        let unconfirmed = ImpactAssessment::new_with_evidence(
            ImpactSeverity::Critical,
            ImpactDomain::Security,
            ImpactConfidence::Low,
            "possible secret leak",
            vec!["scanner-match".to_owned()],
            "scanner",
        )
        .unwrap();
        assert!(unconfirmed.is_unconfirmed_critical());

        let confirmed = ImpactAssessment::new_with_evidence(
            ImpactSeverity::Critical,
            ImpactDomain::Security,
            ImpactConfidence::High,
            "verified secret leak",
            vec!["audit-trace".to_owned()],
            "scanner",
        )
        .unwrap();
        assert!(!confirmed.is_unconfirmed_critical());
    }

    #[test]
    fn test_impact_assessment_serde_roundtrip() {
        let assessment = ImpactAssessment {
            level: ImpactSeverity::Critical,
            domain: ImpactDomain::DataLoss,
            confidence: ImpactConfidence::Medium,
            reason: "Uncommitted transaction may be lost on crash".to_owned(),
            evidence: vec!["trace-123".to_owned(), "log-456".to_owned()],
            reported_by: "job-runtime".to_owned(),
            normalized_by: Some("escalation-policy-v1".to_owned()),
        };
        let json = serde_json::to_string(&assessment).unwrap();
        let recovered: ImpactAssessment = serde_json::from_str(&json).unwrap();
        assert_eq!(assessment, recovered);
    }

    #[test]
    fn test_impact_domain_display() {
        assert_eq!(ImpactDomain::Security.to_string(), "security");
        assert_eq!(ImpactDomain::DataLoss.to_string(), "data_loss");
        assert_eq!(ImpactDomain::ScopeViolation.to_string(), "scope_violation");
    }

    #[test]
    fn test_impact_confidence_ordering() {
        assert!(ImpactConfidence::Low < ImpactConfidence::Medium);
        assert!(ImpactConfidence::Medium < ImpactConfidence::High);
    }

    #[test]
    fn test_high_and_critical_require_evidence() {
        let error = ImpactAssessment::new(
            ImpactSeverity::High,
            ImpactDomain::Correctness,
            ImpactConfidence::High,
            "missing evidence",
            "agent",
        )
        .unwrap_err();
        assert_eq!(
            error,
            ImpactAssessmentError::MissingEvidence {
                level: ImpactSeverity::High
            }
        );
    }

    #[test]
    fn test_deserialization_validates_invariants() {
        let json = r#"{
            "level":"critical",
            "domain":"security",
            "confidence":"low",
            "reason":"possible leak",
            "evidence":[],
            "reported_by":"scanner"
        }"#;
        let error = serde_json::from_str::<ImpactAssessment>(json).unwrap_err();
        assert!(error.to_string().contains("require evidence"));
    }

    #[test]
    fn test_low_confidence_critical_is_allowed_when_evidenced() {
        let assessment = ImpactAssessment::new_with_evidence(
            ImpactSeverity::Critical,
            ImpactDomain::Security,
            ImpactConfidence::Low,
            "possible leak",
            vec!["scanner-match".to_owned()],
            "scanner",
        )
        .unwrap();
        assert!(assessment.is_unconfirmed_critical());
    }
}
