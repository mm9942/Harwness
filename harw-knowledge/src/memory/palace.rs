//! The palace: a linked long-term memory graph of `[[wikilink]]` nodes (§2.2).
//!
//! Spec: `docs/design/knowledge-surfaces.md` §2.2, §2.5. Each node is one
//! `PalaceNode` artifact with a slug-derived id, a `[[other-node]]`-linked
//! body, tags, and a [`PalaceStatus`]. Nodes are never silently deleted; a
//! superseded node keeps a `superseded_by` link so history stays traceable.
//! The `[[wikilink]]` scanner is implemented fully; topic->palace promotion is
//! the strictest gate (§2.5) and refuses to commit without a recorded review.

use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact};
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::visibility::VisibilityScope;

/// Frontmatter-`extra`-Schlüssel für den Titel eines Knotens bzw. Themas.
pub const TITLE_KEY: &str = "title";

/// Frontmatter-`extra`-Schlüssel des [`PalaceStatus`] (historischer Name
/// `confidence`, siehe [`PalaceStatus`]).
pub const STATUS_KEY: &str = "confidence";

/// Frontmatter-`extra`-Schlüssel des Nachfolgers eines ersetzten Knotens.
pub const SUPERSEDED_BY_KEY: &str = "superseded_by";

/// Frontmatter-`extra`-Schlüssel der Revisionsnummer eines Knotens.
pub const REVISION_KEY: &str = "revision";

/// Lifecycle status of a palace node; nodes are superseded, never deleted
/// (§2.2).
///
/// # Why this is not called `Confidence` (AW4-05)
/// This type was named `Confidence` until node AW4-05 renamed it. That name
/// was wrong: its three values are not a graded confidence *scale* — no
/// value is "more confident" than another the way, say, "high" is more
/// confident than "low" — they are three points in a **promotion
/// lifecycle**. A node starts `Provisional`, is promoted to `Established`
/// only through the reviewed palace-promotion gate (§2.5,
/// [`ensure_promotion_reviewed`]), and a node later corrected becomes
/// `Superseded` (with a `superseded_by` pointer) rather than being deleted.
///
/// Three other, unrelated types named `Confidence` already exist in this
/// workspace, and they really are graded confidence scales:
/// - `harw_research::Confidence` — `Low` / `Medium` / `High` / `Verified`
///   (four levels): trust in a research finding.
/// - `harw_memory::epistemic::Confidence` — `VeryLow` .. `VeryHigh` (five
///   levels): epistemic confidence in a memory claim.
/// - `harw_model_catalog::provenance::Confidence` — `VeryLow` .. `VeryHigh`
///   (five levels): confidence in a catalog provenance estimate.
///
/// A fourth `Confidence` type with a completely different (lifecycle, not
/// graded) meaning is exactly the kind of name collision that produces a
/// category-confusion bug: this workspace's AW4 Ausbauprogramm already once
/// planned an invented "conversion" between two of the above types that
/// would itself have been such a category error. Renaming this type to
/// `PalaceStatus` removes the collision instead of adding a fourth
/// lookalike. **Do not rename it back to `Confidence`.**
///
/// The serialized form is unchanged by this rename: `#[serde(rename_all =
/// "snake_case")]` derives its wire strings from the variant names
/// (`Established` / `Provisional` / `Superseded`), which are untouched, not
/// from the enum's own type name — so a file written before this rename
/// still deserializes into [`PalaceStatus`] unmodified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PalaceStatus {
    /// Durable truth other agents traverse and trust.
    Established,
    /// Believed but not yet promotion-hardened.
    Provisional,
    /// Retired in favour of a `superseded_by` node.
    Superseded,
}

/// One node in the palace graph (§2.2).
#[derive(Debug, Clone, PartialEq, ::serde::Serialize, ::serde::Deserialize)]
pub struct PalaceNode {
    /// Stable, human-legible node id (`palace/<slug>`).
    pub id: ArtifactId,
    /// Human-readable title.
    pub title: String,
    /// Prose body containing `[[wikilink]]` references.
    pub body: String,
    /// Topical tags, independent of the link graph.
    pub tags: Vec<String>,
    /// Lifecycle status; see [`PalaceStatus`] for why this is not a
    /// confidence score.
    pub confidence: PalaceStatus,
    /// If superseded, the node that replaces this one.
    pub superseded_by: Option<ArtifactId>,
}

impl PalaceNode {
    /// Create a fresh, `Provisional` node with the given id/title/body/tags.
    #[must_use]
    pub fn new(
        id: ArtifactId,
        title: impl Into<String>,
        body: impl Into<String>,
        tags: Vec<String>,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            body: body.into(),
            tags,
            confidence: PalaceStatus::Provisional,
            superseded_by: None,
        }
    }

    /// Compute this node's outgoing links by scanning its body for `[[links]]`.
    #[must_use]
    pub fn outgoing_links(&self) -> Vec<ArtifactId> {
        scan_wikilinks(&self.body)
            .into_iter()
            .map(ArtifactId::new)
            .collect()
    }
}

impl PalaceStatus {
    /// Wire-Form (`established`/`provisional`/`superseded`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Established => "established",
            Self::Provisional => "provisional",
            Self::Superseded => "superseded",
        }
    }

    /// Umkehrung von [`Self::label`]; unbekannte Werte ergeben `None`.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "established" => Some(Self::Established),
            "provisional" => Some(Self::Provisional),
            "superseded" => Some(Self::Superseded),
            _ => None,
        }
    }
}

/// Liest den [`PalaceStatus`] aus dem Frontmatter eines Knotens oder Themas.
///
/// # Beschreibung
/// Fehlt `extra.confidence` oder ist der Wert unbekannt, gilt der Eintrag
/// als [`PalaceStatus::Provisional`] — nie als `established` (fail-closed:
/// nur ein ausdrücklich promoteter Eintrag ist geteiltes Wissen).
#[must_use]
pub fn artifact_status(artifact: &KnowledgeArtifact) -> PalaceStatus {
    artifact
        .frontmatter
        .extra
        .get(STATUS_KEY)
        .and_then(serde_json::Value::as_str)
        .and_then(PalaceStatus::from_label)
        .unwrap_or(PalaceStatus::Provisional)
}

/// Setzt den [`PalaceStatus`] im Frontmatter (`extra.confidence`).
pub fn set_status(frontmatter: &mut Frontmatter, status: PalaceStatus) {
    frontmatter.extra.insert(
        STATUS_KEY.to_owned(),
        serde_json::Value::String(status.label().to_owned()),
    );
}

/// Ob ein Artefakt über die Palace-Lesewerkzeuge für Agenten sichtbar ist.
///
/// # Beschreibung
/// Agenten sehen ausschließlich Palace-Knoten mit Status
/// [`PalaceStatus::Established`] (§2.2/§2.5: der Review-Gate von
/// `/palace promote` ist die Freigabe als geteiltes Wissen). Von diesen nur
/// solche mit [`VisibilityScope::OperatorOnly`] — das ist die Sichtbarkeit,
/// die der Operator-Promote-Pfad vergibt. Knoten mit `SelfOnly`,
/// `DescendantTree` oder `ExplicitlyGranted` bleiben verborgen, weil der
/// Werkzeug-Kontext keine Identität trägt, die diese Scopes belegen könnte
/// (fail-closed wie [`VisibilityScope::visible_to_caller`]). `provisional`
/// und `superseded` sind nie sichtbar.
#[must_use]
pub fn is_agent_readable(artifact: &KnowledgeArtifact) -> bool {
    artifact.kind == ArtifactKind::PalaceNode
        && artifact_status(artifact) == PalaceStatus::Established
        && matches!(
            artifact.frontmatter.visibility,
            VisibilityScope::OperatorOnly
        )
}

/// Extract `[[target]]` wikilink targets from body text (§2.2).
///
/// Supports Obsidian-style `[[target|alias]]` (the target before `|` is used).
/// Targets are trimmed; empty targets are skipped. Returned in document order
/// with duplicates preserved (callers dedupe if needed).
#[must_use]
pub fn scan_wikilinks(body: &str) -> Vec<String> {
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            if let Some(close) = find_closing(body, i + 2) {
                let inner = &body[i + 2..close];
                let target = inner.split('|').next().unwrap_or(inner).trim();
                if !target.is_empty() {
                    out.push(target.to_owned());
                }
                i = close + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Find the byte index of the `]]` that closes a wikilink opened at `start`.
fn find_closing(body: &str, start: usize) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut i = start;
    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b']' {
            return Some(i);
        }
        // A newline inside `[[...]]` is not a valid wikilink; bail out.
        if bytes[i] == b'\n' {
            return None;
        }
        i += 1;
    }
    None
}

/// Commit a topic->palace promotion through the strictest gate (§2.5).
///
/// # Errors
/// [`KnowledgeError::PromotionNotReviewed`] unless `reviewed` is `true` (an
/// explicit `/palace promote` authorization or a reviewed dream proposal).
pub fn ensure_promotion_reviewed(
    topic_id: &str,
    node_id: &ArtifactId,
    reviewed: bool,
) -> KnowledgeResult<()> {
    if reviewed {
        return Ok(());
    }
    Err(KnowledgeError::PromotionNotReviewed {
        from: format!("topic/{topic_id}"),
        to: node_id.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        PalaceNode, PalaceStatus, artifact_status, ensure_promotion_reviewed, is_agent_readable,
        scan_wikilinks, set_status,
    };
    use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact};
    use crate::error::KnowledgeError;
    use crate::test_support::{TestError, TestResult};
    use crate::visibility::{AgentId, VisibilityScope};

    #[test]
    fn test_scan_wikilinks_extracts_targets_and_strips_aliases() {
        let body = "see [[palace/deploy-pipeline]] and [[palace/rollback|the rollback plan]]";
        assert_eq!(
            scan_wikilinks(body),
            vec![
                "palace/deploy-pipeline".to_owned(),
                "palace/rollback".to_owned(),
            ]
        );
    }

    #[test]
    fn test_ensure_promotion_reviewed_rejects_unreviewed_promotion() -> TestResult {
        let Err(error) =
            ensure_promotion_reviewed("runtime", &ArtifactId::new("palace/runtime"), false)
        else {
            return Err(TestError::Unexpected(
                "unreviewed promotion must be refused".to_owned(),
            ));
        };
        assert!(matches!(error, KnowledgeError::PromotionNotReviewed { .. }));
        Ok(())
    }

    fn node(status: Option<PalaceStatus>, visibility: VisibilityScope) -> KnowledgeArtifact {
        let mut frontmatter = Frontmatter::new(
            AgentId::new("operator"),
            visibility,
            jiff::Timestamp::UNIX_EPOCH,
        );
        if let Some(status) = status {
            set_status(&mut frontmatter, status);
        }
        KnowledgeArtifact::new(
            ArtifactId::new("palace/n"),
            ArtifactKind::PalaceNode,
            frontmatter,
            "body",
        )
    }

    #[test]
    fn missing_or_unknown_status_reads_as_provisional() {
        assert_eq!(
            artifact_status(&node(None, VisibilityScope::OperatorOnly)),
            PalaceStatus::Provisional
        );
        let mut unknown = node(None, VisibilityScope::OperatorOnly);
        unknown
            .frontmatter
            .extra
            .insert("confidence".to_owned(), serde_json::json!("certain"));
        assert_eq!(artifact_status(&unknown), PalaceStatus::Provisional);
        for status in [
            PalaceStatus::Established,
            PalaceStatus::Provisional,
            PalaceStatus::Superseded,
        ] {
            assert_eq!(
                artifact_status(&node(Some(status), VisibilityScope::OperatorOnly)),
                status
            );
            assert_eq!(PalaceStatus::from_label(status.label()), Some(status));
        }
    }

    #[test]
    fn only_established_operator_nodes_are_agent_readable() {
        assert!(is_agent_readable(&node(
            Some(PalaceStatus::Established),
            VisibilityScope::OperatorOnly
        )));
        for hidden in [
            node(
                Some(PalaceStatus::Provisional),
                VisibilityScope::OperatorOnly,
            ),
            node(
                Some(PalaceStatus::Superseded),
                VisibilityScope::OperatorOnly,
            ),
            node(None, VisibilityScope::OperatorOnly),
            node(Some(PalaceStatus::Established), VisibilityScope::SelfOnly),
        ] {
            assert!(!is_agent_readable(&hidden), "{hidden:?}");
        }
        let mut topic = node(
            Some(PalaceStatus::Established),
            VisibilityScope::OperatorOnly,
        );
        topic.kind = ArtifactKind::TopicMemory;
        assert!(!is_agent_readable(&topic), "topics are never palace reads");
    }

    #[test]
    fn test_ensure_promotion_reviewed_accepts_reviewed_promotion() {
        assert!(
            ensure_promotion_reviewed("runtime", &ArtifactId::new("palace/runtime"), true).is_ok()
        );
    }

    /// AW4-05: renaming `Confidence` to `PalaceStatus` must not change the
    /// wire form. Pinned against a literal, not just a round-trip, per the
    /// node's explicit instruction.
    #[test]
    fn test_palace_status_serializes_identically_to_the_former_confidence_literal() -> TestResult {
        assert_eq!(
            serde_json::to_string(&PalaceStatus::Established)
                .map_err(crate::test_support::ctx("serializes"))?,
            "\"established\""
        );
        assert_eq!(
            serde_json::to_string(&PalaceStatus::Provisional)
                .map_err(crate::test_support::ctx("serializes"))?,
            "\"provisional\""
        );
        assert_eq!(
            serde_json::to_string(&PalaceStatus::Superseded)
                .map_err(crate::test_support::ctx("serializes"))?,
            "\"superseded\""
        );
        Ok(())
    }

    /// A `PalaceNode` written to disk before the AW4-05 rename still used the
    /// field name `confidence` (kept) with these exact snake_case values
    /// (also kept). This fixture is byte-for-byte what such a pre-rename
    /// file's JSON projection looked like, and it must keep loading.
    #[test]
    fn test_palace_node_loads_the_pre_rename_confidence_field_form() -> TestResult {
        let legacy_fixture = r#"{
            "id": "palace/deploy-pipeline",
            "title": "Deploy pipeline",
            "body": "see [[palace/rollback]]",
            "tags": ["deploy"],
            "confidence": "established",
            "superseded_by": null
        }"#;

        let node: PalaceNode = serde_json::from_str(legacy_fixture).map_err(
            crate::test_support::ctx("pre-rename fixture still deserializes"),
        )?;

        assert_eq!(node.confidence, PalaceStatus::Established);
        assert_eq!(node.id, ArtifactId::new("palace/deploy-pipeline"));
        Ok(())
    }
}
