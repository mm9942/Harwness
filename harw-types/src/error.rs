//! Typed errors shared by the `harw-types` vocabulary.

use crate::impact::ImpactSeverity;
use std::fmt;

/// Error returned when an ID is constructed from an empty value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidId {
    id_type: &'static str,
}

impl InvalidId {
    /// Creates an invalid-ID error for the named ID type.
    #[must_use]
    pub(crate) const fn new(id_type: &'static str) -> Self {
        Self { id_type }
    }
}

impl fmt::Display for InvalidId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} must not be empty or whitespace-only", self.id_type)
    }
}

impl std::error::Error for InvalidId {}

/// Error returned when an impact assessment violates its safety invariants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImpactAssessmentError {
    /// The human-readable reason must not be blank.
    EmptyReason,
    /// The provenance identifier must not be blank.
    EmptyReporter,
    /// High and critical assessments must cite at least one piece of evidence.
    MissingEvidence { level: ImpactSeverity },
    /// An evidence reference must not be blank.
    EmptyEvidence { index: usize },
    /// An optional normalisation policy, when present, must not be blank.
    EmptyNormalizer,
}

impl fmt::Display for ImpactAssessmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyReason => f.write_str("impact assessment reason must not be blank"),
            Self::EmptyReporter => f.write_str("impact assessment reporter must not be blank"),
            Self::MissingEvidence { level } => {
                write!(f, "{level} impact assessments require evidence")
            }
            Self::EmptyEvidence { index } => {
                write!(
                    f,
                    "impact assessment evidence at index {index} must not be blank"
                )
            }
            Self::EmptyNormalizer => {
                f.write_str("impact assessment normalizer must not be blank when present")
            }
        }
    }
}

impl std::error::Error for ImpactAssessmentError {}

/// Error returned when a [`crate::digest::ContentDigest`] cannot be parsed
/// from a string. Defined in `harw-digest` next to the digest itself and
/// re-exported here so `harw_types::error::InvalidDigest` keeps working.
pub use harw_digest::InvalidDigest;
