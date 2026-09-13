//! Review-gated output from a governed dream job.

use harw_job_runtime::WorkId;

use crate::artifact::ArtifactId;

/// A proposal emitted by a dream job; it is not a committed knowledge write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamProposal {
    pub artifact_id: ArtifactId,
    pub summary: String,
}

/// The durable report a dream job may write to its own output location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamReport {
    pub work_id: WorkId,
    pub summary: String,
    pub proposed_topic_updates: Vec<DreamProposal>,
    pub proposed_palace_promotions: Vec<DreamProposal>,
    pub follow_ups: Vec<String>,
}

impl DreamReport {
    #[must_use]
    pub fn has_proposals(&self) -> bool {
        !self.proposed_topic_updates.is_empty() || !self.proposed_palace_promotions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_with_no_proposals_is_not_committable_work() {
        let report = DreamReport {
            work_id: WorkId::from_str("work-1"),
            summary: "none".to_owned(),
            proposed_topic_updates: Vec::new(),
            proposed_palace_promotions: Vec::new(),
            follow_ups: Vec::new(),
        };
        assert!(!report.has_proposals());
    }
}
