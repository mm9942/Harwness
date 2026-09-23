use serde::{Deserialize, Serialize};

use crate::{CleanupProposal, ProposalId};

/// Actions that an Android notification adapter may expose. `Details` must
/// reopen the app; it must not alter the proposal state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotificationAction {
    Approve,
    Reject,
    Details,
}

/// Platform-neutral payload for an Android notification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalNotification {
    pub proposal_id: ProposalId,
    pub title: String,
    pub body: String,
    pub actions: [NotificationAction; 3],
}

impl From<&CleanupProposal> for ApprovalNotification {
    fn from(proposal: &CleanupProposal) -> Self {
        let candidate = &proposal.candidate;
        Self {
            proposal_id: proposal.id().clone(),
            title: "Aufräumvorschlag".into(),
            body: format!(
                "{} ({}) — {}",
                candidate.display_name,
                human_bytes(candidate.byte_size),
                candidate.explanation
            ),
            actions: [
                NotificationAction::Approve,
                NotificationAction::Reject,
                NotificationAction::Details,
            ],
        }
    }
}

fn human_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * KIB;
    const GIB: u64 = MIB * KIB;
    if bytes >= GIB {
        format!("{:.1} GB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TestResult, ctx};
    use crate::{CandidateKind, CleanupCandidate, CleanupProposal};

    use super::*;

    #[test]
    fn notification_is_an_approval_prompt_not_a_delete_prompt() -> TestResult {
        let proposal = CleanupProposal::new(
            ProposalId::parse("p1").map_err(ctx("Proposal-Id parsen"))?,
            CleanupCandidate {
                document_ref: "content://test/1".into(),
                display_name: "backup.zip".into(),
                byte_size: 2 * 1024 * 1024,
                kind: CandidateKind::OldDownload,
                explanation: "Seit einem Jahr nicht verwendet.".into(),
            },
        )
        .map_err(ctx("Cleanup-Proposal erstellen"))?;
        let notification = ApprovalNotification::from(&proposal);

        assert_eq!(notification.title, "Aufräumvorschlag");
        assert_eq!(
            notification.actions,
            [
                NotificationAction::Approve,
                NotificationAction::Reject,
                NotificationAction::Details
            ]
        );
        assert!(notification.body.contains("2.0 MB"));
        Ok(())
    }
}
