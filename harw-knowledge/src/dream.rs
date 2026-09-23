//! Review-gated output from a governed dream job.
//!
//! Ein [`DreamReport`] wird als gewöhnliches Knowledge-Artefakt
//! (`ArtifactKind::DreamReport`) mit gültigem YAML-Frontmatter unter
//! `dreams/<YYYY-MM-DD>/<work-id>.md` abgelegt — geschrieben ausschließlich über
//! [`crate::store::KnowledgeStore::write_dream_report`], das den gemeinsamen
//! Frontmatter-Writer nutzt. Dadurch kann
//! [`crate::index::KnowledgeIndex::rebuild`] jeden Bericht wieder einlesen.

use harw_job_runtime::WorkId;

use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact};
use crate::error::KnowledgeResult;
use crate::visibility::{AgentId, VisibilityScope};

/// Autor-Id, unter der Traumberichte im Frontmatter geführt werden.
pub const DREAM_AUTHOR_AGENT_ID: &str = "dream";

/// Wert des `extra.kind`-Frontmatter-Felds eines Traumberichts.
pub const DREAM_REPORT_KIND: &str = "dream_report";

/// A proposal emitted by a dream job; it is not a committed knowledge write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamProposal {
    pub artifact_id: ArtifactId,
    pub summary: String,
}

/// The durable report a dream job may write to its own output location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamReport {
    /// Governte Job-Id; zugleich Dateiname (`dreams/<datum>/<work_id>.md`).
    pub work_id: WorkId,
    /// Erstellungszeitpunkt; bestimmt `created_at` und das Datumsverzeichnis (UTC).
    pub created_at: jiff::Timestamp,
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

    /// Datumsverzeichnis des Berichts (`YYYY-MM-DD`, UTC aus `created_at`).
    #[must_use]
    pub fn report_date(&self) -> String {
        self.created_at.strftime("%Y-%m-%d").to_string()
    }

    /// Artefakt-Id, die [`crate::index::KnowledgeIndex::rebuild`] für den
    /// geschriebenen Bericht ableitet (`dream/<YYYY-MM-DD>/<work-id>`).
    #[must_use]
    pub fn artifact_id(&self) -> ArtifactId {
        ArtifactId::new(format!(
            "dream/{}/{}",
            self.report_date(),
            self.work_id.as_str()
        ))
    }

    /// Frontmatter des Berichts: `visibility: operator_only`, `created_at`,
    /// Tags `dream`/`review-gated` und in `extra` die Felder `kind:
    /// dream_report`, `work_id` und `review_gated: true`.
    #[must_use]
    pub fn frontmatter(&self) -> Frontmatter {
        let mut frontmatter = Frontmatter::new(
            AgentId::new(DREAM_AUTHOR_AGENT_ID),
            VisibilityScope::OperatorOnly,
            self.created_at,
        );
        frontmatter.tags = vec!["dream".to_owned(), "review-gated".to_owned()];
        frontmatter.extra.insert(
            "kind".to_owned(),
            serde_json::Value::from(DREAM_REPORT_KIND),
        );
        frontmatter.extra.insert(
            "work_id".to_owned(),
            serde_json::Value::from(self.work_id.as_str()),
        );
        frontmatter
            .extra
            .insert("review_gated".to_owned(), serde_json::Value::Bool(true));
        frontmatter
    }

    /// Markdown-Body des Berichts (ohne Frontmatter).
    #[must_use]
    pub fn render_body(&self) -> String {
        let mut body = format!(
            "# Traumbericht {}\n\n\
             - Zeit: {}\n\
             - Status: **review-gated** (keine automatische Übernahme in Memory/Palace)\n\n\
             ## Zusammenfassung\n\n{}\n",
            self.work_id.as_str(),
            self.created_at.strftime("%Y-%m-%d %H:%M:%S UTC"),
            self.summary.trim_end(),
        );
        push_proposals(
            &mut body,
            "Vorgeschlagene Topic-Updates",
            &self.proposed_topic_updates,
        );
        push_proposals(
            &mut body,
            "Vorgeschlagene Palace-Promotionen",
            &self.proposed_palace_promotions,
        );
        body.push_str("\n## Offene Fäden\n\n");
        if self.follow_ups.is_empty() {
            body.push_str("_Keine._\n");
        } else {
            for follow_up in &self.follow_ups {
                body.push_str("- ");
                body.push_str(follow_up.trim());
                body.push('\n');
            }
        }
        body.push_str(
            "\n_Keine automatisch übernommenen Änderungen. Prüfe den Bericht und \
             promote Inhalte bei Bedarf manuell nach `topics/` oder `palace/`._\n",
        );
        body
    }

    /// Der Bericht als [`KnowledgeArtifact`] (`ArtifactKind::DreamReport`).
    #[must_use]
    pub fn to_artifact(&self) -> KnowledgeArtifact {
        KnowledgeArtifact::new(
            self.artifact_id(),
            ArtifactKind::DreamReport,
            self.frontmatter(),
            self.render_body(),
        )
    }

    /// Vollständiges Markdown-Dokument: `---`-YAML-Frontmatter plus Body,
    /// gerendert über [`crate::store::render_frontmatter`].
    ///
    /// # Errors
    /// [`crate::error::KnowledgeError::Frontmatter`] bei einem YAML-Encode-Fehler.
    pub fn render_markdown(&self) -> KnowledgeResult<String> {
        crate::store::render_frontmatter(&self.frontmatter(), &self.render_body())
    }
}

/// Hängt einen Vorschlagsabschnitt an den Body an.
fn push_proposals(body: &mut String, heading: &str, proposals: &[DreamProposal]) {
    body.push_str("\n## ");
    body.push_str(heading);
    body.push_str("\n\n");
    if proposals.is_empty() {
        body.push_str("_Keine._\n");
        return;
    }
    for proposal in proposals {
        body.push_str("- `");
        body.push_str(proposal.artifact_id.as_str());
        body.push_str("`: ");
        body.push_str(proposal.summary.trim());
        body.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    fn report() -> DreamReport {
        DreamReport {
            work_id: WorkId::from_str("work-1"),
            created_at: jiff::Timestamp::UNIX_EPOCH,
            summary: "none".to_owned(),
            proposed_topic_updates: Vec::new(),
            proposed_palace_promotions: Vec::new(),
            follow_ups: Vec::new(),
        }
    }

    #[test]
    fn report_with_no_proposals_is_not_committable_work() {
        assert!(!report().has_proposals());
    }

    #[test]
    fn render_markdown_produces_parseable_operator_only_frontmatter() -> TestResult {
        let mut report = report();
        report.proposed_topic_updates.push(DreamProposal {
            artifact_id: ArtifactId::new("topic/runtime"),
            summary: "Laufzeitnotiz ergänzen".to_owned(),
        });
        report.follow_ups.push("Deploy prüfen".to_owned());

        let rendered = report.render_markdown()?;
        let (frontmatter, body) = crate::store::parse_frontmatter(&rendered)?;

        assert_eq!(frontmatter.visibility, VisibilityScope::OperatorOnly);
        assert_eq!(frontmatter.created_at, jiff::Timestamp::UNIX_EPOCH);
        assert_eq!(
            frontmatter.extra.get("kind"),
            Some(&serde_json::Value::from(DREAM_REPORT_KIND))
        );
        assert_eq!(
            frontmatter.extra.get("work_id"),
            Some(&serde_json::Value::from("work-1"))
        );
        assert!(body.contains("# Traumbericht work-1"));
        assert!(body.contains("`topic/runtime`: Laufzeitnotiz ergänzen"));
        assert!(body.contains("- Deploy prüfen"));
        assert_eq!(
            report.artifact_id(),
            ArtifactId::new("dream/1970-01-01/work-1")
        );
        Ok(())
    }
}
