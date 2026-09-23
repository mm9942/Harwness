use core::fmt;

use serde::{Deserialize, Serialize};

/// Stable identifier supplied by the durable mobile store.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProposalId(String);

impl ProposalId {
    /// Creates an identifier when it is non-empty after trimming whitespace.
    pub fn parse(value: impl Into<String>) -> Result<Self, CleanupError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(CleanupError::EmptyProposalId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Classification produced by deterministic file analysis, not by the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateKind {
    Duplicate,
    OldDownload,
    InstallerPackage,
    ArchiveWithExtractedContents,
    LargeFile,
    Other,
}

/// A file candidate limited to a user-authorized Android storage tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupCandidate {
    /// Content-URI or app-private reference; never a raw path interpreted by a model.
    pub document_ref: String,
    pub display_name: String,
    pub byte_size: u64,
    pub kind: CandidateKind,
    pub explanation: String,
}

impl CleanupCandidate {
    pub fn validate(&self) -> Result<(), CleanupError> {
        if self.document_ref.trim().is_empty() {
            return Err(CleanupError::EmptyDocumentReference);
        }
        if self.display_name.trim().is_empty() {
            return Err(CleanupError::EmptyDisplayName);
        }
        if self.explanation.trim().is_empty() {
            return Err(CleanupError::EmptyExplanation);
        }
        Ok(())
    }
}

/// An immutable request awaiting an explicit human response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupProposal {
    id: ProposalId,
    pub candidate: CleanupCandidate,
}

impl CleanupProposal {
    pub fn new(id: ProposalId, candidate: CleanupCandidate) -> Result<Self, CleanupError> {
        candidate.validate()?;
        Ok(Self { id, candidate })
    }

    pub fn id(&self) -> &ProposalId {
        &self.id
    }

    pub fn status(&self) -> ProposalStatus {
        ProposalStatus::AwaitingDecision
    }

    /// A proposal has no executable disposition until the local owner answers.
    pub fn decide(self, decision: Decision) -> ReviewedProposal {
        let status = match decision {
            Decision::Approve => ProposalStatus::ApprovedForQuarantine,
            Decision::Reject => ProposalStatus::Rejected,
        };
        ReviewedProposal {
            proposal: self,
            decision,
            status,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    Approve,
    Reject,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalStatus {
    AwaitingDecision,
    ApprovedForQuarantine,
    Rejected,
}

/// Only a reviewed proposal can produce a disposition. There is intentionally
/// no `Delete` variant: irreversible deletion belongs to a future separately
/// approved retention workflow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Disposition {
    MoveToQuarantine { document_ref: String },
    Keep { document_ref: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewedProposal {
    proposal: CleanupProposal,
    decision: Decision,
    status: ProposalStatus,
}

impl ReviewedProposal {
    pub fn proposal(&self) -> &CleanupProposal {
        &self.proposal
    }

    pub fn decision(&self) -> Decision {
        self.decision
    }

    pub fn status(&self) -> ProposalStatus {
        self.status
    }

    /// Converts the explicit decision into the sole permitted file operation.
    pub fn disposition(&self) -> Disposition {
        let document_ref = self.proposal.candidate.document_ref.clone();
        match self.decision {
            Decision::Approve => Disposition::MoveToQuarantine { document_ref },
            Decision::Reject => Disposition::Keep { document_ref },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CleanupError {
    EmptyProposalId,
    EmptyDocumentReference,
    EmptyDisplayName,
    EmptyExplanation,
}

impl fmt::Display for CleanupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::EmptyProposalId => "proposal ID must not be empty",
            Self::EmptyDocumentReference => "document reference must not be empty",
            Self::EmptyDisplayName => "display name must not be empty",
            Self::EmptyExplanation => "cleanup explanation must not be empty",
        };
        f.write_str(message)
    }
}

impl std::error::Error for CleanupError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn proposal() -> TestResult<CleanupProposal> {
        let id = ProposalId::parse("proposal-42").map_err(ctx("ProposalId parsen"))?;
        let proposal = CleanupProposal::new(
            id,
            CleanupCandidate {
                document_ref: "content://media/external/downloads/42".into(),
                display_name: "old-installer.apk".into(),
                byte_size: 1_024,
                kind: CandidateKind::InstallerPackage,
                explanation: "APK was downloaded 14 months ago.".into(),
            },
        )
        .map_err(ctx("CleanupProposal erstellen"))?;
        Ok(proposal)
    }

    #[test]
    fn approval_can_only_move_to_quarantine() -> TestResult {
        let reviewed = proposal()?.decide(Decision::Approve);
        assert_eq!(reviewed.status(), ProposalStatus::ApprovedForQuarantine);
        assert_eq!(
            reviewed.disposition(),
            Disposition::MoveToQuarantine {
                document_ref: "content://media/external/downloads/42".into()
            }
        );
        Ok(())
    }

    #[test]
    fn rejection_keeps_the_original_document() -> TestResult {
        let reviewed = proposal()?.decide(Decision::Reject);
        assert_eq!(reviewed.status(), ProposalStatus::Rejected);
        assert!(matches!(reviewed.disposition(), Disposition::Keep { .. }));
        Ok(())
    }
}
