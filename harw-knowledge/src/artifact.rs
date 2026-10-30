//! Core artifact types: [`ArtifactKind`], [`KnowledgeArtifact`],
//! [`Frontmatter`], [`RecallQuery`] and the [`ArtifactId`] newtype.
//!
//! Spec: `docs/design/knowledge-surfaces.md` §8.1 (type sketches) and §1.2
//! (frontmatter schema). Every surface in the document is backed by one
//! `ArtifactKind` variant so the index can query across them. The frontmatter
//! is the canonical structured data; the markdown body is prose. An `extra`
//! escape hatch keeps the schema forward-compatible.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use harw_types::SessionId;

use crate::caller_identity::{ArtifactOwner, CallerIdentity, is_visible};
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::visibility::{AgentId, VisibilityScope};

id_newtype!(
    /// Stable, human-legible artifact id (e.g. `palace/deploy-pipeline`, §2.2).
    ArtifactId
);

/// Hard cap on `RecallQuery::max_artifacts` (§2.3 bounded-search guarantee).
pub const MAX_RECALL_ARTIFACTS: usize = 256;

/// Hard cap on `RecallQuery::max_hops` for palace backlink traversal (§2.3).
pub const MAX_RECALL_HOPS: u8 = 8;

/// Discriminates what kind of durable material an artifact carries; every
/// surface in the document is backed by one variant so the index can query
/// across them (§8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// The single durable core-memory file (§2.1).
    CoreMemory,
    /// One topic-memory file per coherent subject (§2.1).
    TopicMemory,
    /// A node in the linked palace graph (§2.2).
    PalaceNode,
    /// One dated diary entry (§3).
    DiaryEntry,
    /// A dream job's proposal report (§4.2).
    DreamReport,
    /// A free-form workbench note (§5.1).
    WorkbenchNote,
    /// A structured workbench hypothesis (§5.1).
    WorkbenchHypothesis,
    /// A kanban card (§6.1).
    KanbanCard,
    /// A durable security finding backed by frozen `harw-dod-signals`
    /// evidence (AW4-05). See [`crate::security::SecurityFinding`] — exactly
    /// the kind of artifact `VisibilityScope::OperatorOnly` (§7) exists for.
    SecurityFinding,
    /// A frozen behavioral baseline that AW4-03's rule engine checks new
    /// observations against (AW4-05). See [`crate::security::Baseline`].
    Baseline,
    /// A Layer-4 proposal to change a context program — never applied by
    /// anything in this crate (AW5-09). See
    /// [`crate::context_proposal::ContextProposal`].
    ContextProposal,
    /// A Layer-4 proposal to change a `harw-model-catalog` claim about a
    /// model, derived from observed model behavior — never applied by
    /// anything in this crate (AW6-06). See
    /// [`crate::model_behavior_proposal::ModelBehaviorProposal`].
    ModelBehaviorProposal,
}

/// One stored unit: frontmatter + body, addressable by a stable id and
/// queryable through the index (§8.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeArtifact {
    /// Stable id.
    pub id: ArtifactId,
    /// Which surface this artifact belongs to.
    pub kind: ArtifactKind,
    /// Canonical structured frontmatter.
    pub frontmatter: Frontmatter,
    /// Markdown prose body.
    pub body: String,
}

impl KnowledgeArtifact {
    /// Assemble an artifact from its id, kind, frontmatter and body.
    #[must_use]
    pub fn new(
        id: ArtifactId,
        kind: ArtifactKind,
        frontmatter: Frontmatter,
        body: impl Into<String>,
    ) -> Self {
        Self {
            id,
            kind,
            frontmatter,
            body: body.into(),
        }
    }
}

/// Canonical structured frontmatter attached to every stored artifact (§1.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frontmatter {
    /// Creation instant.
    pub created_at: jiff::Timestamp,
    /// Most-recent update instant.
    pub updated_at: jiff::Timestamp,
    /// Visibility scope governing read/write (§7).
    pub visibility: VisibilityScope,
    /// Topical tags, independent of the link graph.
    pub tags: Vec<String>,
    /// Outgoing artifact links (powers palace backlinks, §2.2).
    pub links: Vec<ArtifactId>,
    /// Authoring agent identity (audit, §2.4).
    pub author_agent_id: AgentId,
    /// Transcript provenance back-reference (§1.4 promotion bridge).
    pub source_session_id: Option<SessionId>,
    /// Forward-compatibility escape hatch for kind-specific fields.
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Frontmatter {
    /// Build fresh frontmatter authored now with empty tags/links/extra.
    #[must_use]
    pub fn new(
        author_agent_id: AgentId,
        visibility: VisibilityScope,
        now: jiff::Timestamp,
    ) -> Self {
        Self {
            created_at: now,
            updated_at: now,
            visibility,
            tags: Vec::new(),
            links: Vec::new(),
            author_agent_id,
            source_session_id: None,
            extra: BTreeMap::new(),
        }
    }

    /// Stamp `updated_at` to `now` (call on every edit).
    pub fn touch(&mut self, now: jiff::Timestamp) {
        self.updated_at = now;
    }
}

/// Bounded read query — the only retrieval path every surface uses, so the
/// §2.3 bounded-search guarantee lives in one place (§8.1).
#[derive(Debug, Clone)]
pub struct RecallQuery {
    /// Free-text keyword query.
    pub text: String,
    /// Required tags (AND semantics against artifact tags).
    pub tags: Vec<String>,
    /// Restrict to these kinds (empty = all kinds).
    pub kinds: Vec<ArtifactKind>,
    /// Hard cap on results returned.
    pub max_artifacts: usize,
    /// How far to traverse palace backlinks from a hit.
    pub max_hops: u8,
    /// Caller's visibility scope; out-of-scope artifacts are invisible.
    ///
    /// Only consulted when [`Self::caller_identity`] is `None`: it feeds the
    /// conservative scope heuristic ([`VisibilityScope::visible_to_caller`]).
    pub caller_scope: VisibilityScope,
    /// Trusted identity of the caller, as established by admission (PL-90
    /// W03d). When set, [`artifact_visible`] decides visibility by identity
    /// ([`is_visible`]: author, spawn ancestors, role, operator flag) and
    /// `caller_scope` is ignored; when `None` the scope heuristic applies and
    /// `SelfOnly`/`DescendantTree`/`ExplicitlyGranted` fail closed. Must
    /// never be built from agent-controlled input.
    pub caller_identity: Option<CallerIdentity>,
}

impl RecallQuery {
    /// Construct a query with the given text and caller scope, other bounds
    /// defaulted to the hard maxima.
    #[must_use]
    pub fn new(text: impl Into<String>, caller_scope: VisibilityScope) -> Self {
        Self {
            text: text.into(),
            tags: Vec::new(),
            kinds: Vec::new(),
            max_artifacts: MAX_RECALL_ARTIFACTS,
            max_hops: MAX_RECALL_HOPS,
            caller_scope,
            caller_identity: None,
        }
    }

    /// Bind a trusted [`CallerIdentity`] to the query, switching visibility
    /// from the scope heuristic to the identity-based decision (see
    /// [`artifact_visible`]).
    #[must_use]
    pub fn with_caller_identity(mut self, identity: CallerIdentity) -> Self {
        self.caller_identity = Some(identity);
        self
    }

    /// Verify the query's bounds sit within the hard maxima (§2.3).
    ///
    /// # Description
    /// Runs at the top of every [`crate::memory::recall::search_with`] call
    /// (`search`/`search_with` both call this first), so this is the one
    /// place that can prove the Context Steward's `steward_ceiling_violation`
    /// null counter (AW6-08, see [`crate::context_steward`]) — a bounded
    /// recall never asks for more than the hard ceiling — is actually
    /// checked on every real call, not just in a test. Reached in production
    /// via `harw-cli`'s `harw_ops::register_all` → `/context-proposal`
    /// (`harw-ops/src/context_proposal.rs`), which calls `search_with` on
    /// every `list`/`view` invocation; today every registered caller builds
    /// its query through [`RecallQuery::new`], whose bounds sit exactly at
    /// the ceiling, so this branch runs on every call and — correctly —
    /// never fires.
    ///
    /// # Errors
    /// [`KnowledgeError::RecallBoundExceeded`] when `max_artifacts` or
    /// `max_hops` exceeds its cap.
    pub fn validate(&self) -> KnowledgeResult<()> {
        if self.max_artifacts > MAX_RECALL_ARTIFACTS {
            crate::context_steward::STEWARD_CEILING_VIOLATION
                .violated(&harw_observe::NullSink, &[]);
            return Err(KnowledgeError::RecallBoundExceeded {
                field: "max_artifacts".to_owned(),
                value: self.max_artifacts,
                max: MAX_RECALL_ARTIFACTS,
            });
        }
        if self.max_hops > MAX_RECALL_HOPS {
            crate::context_steward::STEWARD_CEILING_VIOLATION
                .violated(&harw_observe::NullSink, &[]);
            return Err(KnowledgeError::RecallBoundExceeded {
                field: "max_hops".to_owned(),
                value: usize::from(self.max_hops),
                max: usize::from(MAX_RECALL_HOPS),
            });
        }
        Ok(())
    }
}

/// Visibility decision for one artifact: identity-based when the query
/// carries a trusted [`CallerIdentity`], else the scope heuristic.
///
/// # Description
/// With `query.caller_identity == Some(..)` the artifact is judged by
/// [`is_visible`] over an [`ArtifactOwner`] built from `author` and
/// `visibility`; `query.caller_scope` is not consulted. Without an identity
/// this is exactly [`VisibilityScope::visible_to_caller`] against
/// `query.caller_scope` (fail-closed for `SelfOnly`, `DescendantTree` and
/// `ExplicitlyGranted`), and `author` is unused.
///
/// `visibility` and `author` must come from the index entry itself (the
/// stored [`Frontmatter::visibility`] / [`Frontmatter::author_agent_id`]),
/// never from `Frontmatter::extra` or the artifact body, which an author can
/// fill with arbitrary claims.
///
/// # Arguments
/// - `visibility` (`&VisibilityScope`): the scope the author assigned.
/// - `author` (`&AgentId`): the authoring agent of the artifact.
/// - `query` (`&RecallQuery`): the query carrying the caller context.
///
/// # Returns
/// `true` when the caller may read the artifact.
#[must_use]
pub fn artifact_visible(
    visibility: &VisibilityScope,
    author: &AgentId,
    query: &RecallQuery,
) -> bool {
    match &query.caller_identity {
        Some(identity) => {
            let owner = ArtifactOwner {
                author: author.clone(),
                scope: visibility.clone(),
            };
            is_visible(&owner, identity)
        }
        None => visibility.visible_to_caller(&query.caller_scope),
    }
}

#[cfg(test)]
mod tests {
    use super::ArtifactKind;
    use crate::test_support::{TestError, TestResult};

    /// Every existing variant's snake_case wire form, pinned so the AW4-05
    /// additive change (`SecurityFinding`, `Baseline`) cannot silently shift
    /// an already-stored variant's serialized form.
    #[test]
    fn test_existing_artifact_kind_variants_serialize_unchanged() -> TestResult {
        let cases = [
            (ArtifactKind::CoreMemory, "\"core_memory\""),
            (ArtifactKind::TopicMemory, "\"topic_memory\""),
            (ArtifactKind::PalaceNode, "\"palace_node\""),
            (ArtifactKind::DiaryEntry, "\"diary_entry\""),
            (ArtifactKind::DreamReport, "\"dream_report\""),
            (ArtifactKind::WorkbenchNote, "\"workbench_note\""),
            (
                ArtifactKind::WorkbenchHypothesis,
                "\"workbench_hypothesis\"",
            ),
            (ArtifactKind::KanbanCard, "\"kanban_card\""),
        ];
        for (kind, expected) in cases {
            assert_eq!(serde_json::to_string(&kind)?, expected);
        }
        Ok(())
    }

    /// The two AW4-05 variants serialize/deserialize in the same snake_case
    /// style as every existing variant (round-trip plus a literal check).
    #[test]
    fn test_new_artifact_kind_variants_round_trip_snake_case() -> TestResult {
        let cases = [
            (ArtifactKind::SecurityFinding, "\"security_finding\""),
            (ArtifactKind::Baseline, "\"baseline\""),
        ];
        for (kind, expected) in cases {
            let encoded = serde_json::to_string(&kind)?;
            assert_eq!(encoded, expected);
            let decoded: ArtifactKind = serde_json::from_str(&encoded)?;
            assert_eq!(decoded, kind);
        }
        Ok(())
    }

    /// The AW5-09 addition round-trips in the same snake_case style as every
    /// other variant (literal check plus round-trip, mirroring the AW4-05 test
    /// above without touching it).
    #[test]
    fn test_context_proposal_artifact_kind_round_trips_snake_case() -> TestResult {
        let encoded = serde_json::to_string(&ArtifactKind::ContextProposal)?;
        assert_eq!(encoded, "\"context_proposal\"");
        let decoded: ArtifactKind = serde_json::from_str(&encoded)?;
        assert_eq!(decoded, ArtifactKind::ContextProposal);
        Ok(())
    }

    /// The AW6-06 addition round-trips in the same snake_case style as every
    /// other variant (literal check plus round-trip, mirroring the AW5-09 test
    /// above without touching it).
    #[test]
    fn test_model_behavior_proposal_artifact_kind_round_trips_snake_case() -> TestResult {
        let encoded = serde_json::to_string(&ArtifactKind::ModelBehaviorProposal)?;
        assert_eq!(encoded, "\"model_behavior_proposal\"");
        let decoded: ArtifactKind = serde_json::from_str(&encoded)?;
        assert_eq!(decoded, ArtifactKind::ModelBehaviorProposal);
        Ok(())
    }

    /// A query within the hard bounds validates and never trips
    /// `steward_ceiling_violation_total` (AW6-08).
    #[test]
    fn test_validate_accepts_bounds_at_the_ceiling_without_tripping_the_counter() {
        use crate::context_steward::{STEWARD_CEILING_VIOLATION, STEWARD_COUNTER_LOCK};
        use crate::visibility::VisibilityScope;

        let _guard = STEWARD_COUNTER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = STEWARD_CEILING_VIOLATION.count();
        let query = super::RecallQuery::new("", VisibilityScope::OperatorOnly);
        assert!(query.validate().is_ok());
        assert_eq!(
            STEWARD_CEILING_VIOLATION.count(),
            before,
            "a query built via RecallQuery::new sits exactly at the ceiling and must not trip the counter"
        );
    }

    /// `max_artifacts` above the hard ceiling is rejected and drives
    /// `steward_ceiling_violation_total` over its previous count — this is
    /// the real production call site (`RecallQuery::validate`, called first
    /// by `search_with` on every `/context-proposal` invocation), not a
    /// synthetic mirror of the counter mechanism.
    #[test]
    fn test_validate_rejects_max_artifacts_above_ceiling_and_trips_the_counter() -> TestResult {
        use crate::context_steward::{STEWARD_CEILING_VIOLATION, STEWARD_COUNTER_LOCK};
        use crate::error::KnowledgeError;
        use crate::visibility::VisibilityScope;

        let _guard = STEWARD_COUNTER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = STEWARD_CEILING_VIOLATION.count();
        let mut query = super::RecallQuery::new("", VisibilityScope::OperatorOnly);
        query.max_artifacts = super::MAX_RECALL_ARTIFACTS + 1;

        let Err(error) = query.validate() else {
            return Err(TestError::Unexpected(
                "bound above the ceiling must be rejected".to_owned(),
            ));
        };
        assert!(matches!(error, KnowledgeError::RecallBoundExceeded { .. }));
        assert!(
            STEWARD_CEILING_VIOLATION.count() > before,
            "exceeding max_artifacts must trip the ceiling counter"
        );
        Ok(())
    }

    /// The `max_hops` branch of the same check, independently.
    #[test]
    fn test_validate_rejects_max_hops_above_ceiling_and_trips_the_counter() -> TestResult {
        use crate::context_steward::{STEWARD_CEILING_VIOLATION, STEWARD_COUNTER_LOCK};
        use crate::error::KnowledgeError;
        use crate::visibility::VisibilityScope;

        let _guard = STEWARD_COUNTER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = STEWARD_CEILING_VIOLATION.count();
        let mut query = super::RecallQuery::new("", VisibilityScope::OperatorOnly);
        query.max_hops = super::MAX_RECALL_HOPS + 1;

        let Err(error) = query.validate() else {
            return Err(TestError::Unexpected(
                "bound above the ceiling must be rejected".to_owned(),
            ));
        };
        assert!(matches!(error, KnowledgeError::RecallBoundExceeded { .. }));
        assert!(
            STEWARD_CEILING_VIOLATION.count() > before,
            "exceeding max_hops must trip the ceiling counter"
        );
        Ok(())
    }
}

/// PL-90 W03d: identity-bound visibility via [`artifact_visible`].
#[cfg(test)]
mod identity_visibility_tests {
    use super::{RecallQuery, artifact_visible};
    use crate::caller_identity::CallerIdentity;
    use crate::visibility::{AgentId, AgentRoleRef, VisibilityScope};

    /// A caller spawned by `parent`, as admission would describe it.
    fn child_identity(agent: &str) -> CallerIdentity {
        CallerIdentity::new(AgentId::new(agent)).with_ancestors(vec![AgentId::new("parent")])
    }

    /// A query whose scope heuristic would deny every restricted scope, so a
    /// positive answer can only come from the bound identity.
    fn query_with(identity: Option<CallerIdentity>) -> RecallQuery {
        let query = RecallQuery::new("", VisibilityScope::SelfOnly);
        match identity {
            Some(identity) => query.with_caller_identity(identity),
            None => query,
        }
    }

    #[test]
    fn identity_recall_sees_self_only_artifacts_of_the_author() {
        let query = query_with(Some(child_identity("a")));
        let author = AgentId::new("a");

        assert!(artifact_visible(
            &VisibilityScope::SelfOnly,
            &author,
            &query
        ));
        assert!(artifact_visible(
            &VisibilityScope::DescendantTree,
            &author,
            &query
        ));
        // OperatorOnly stays closed: the identity is an agent, not an operator.
        assert!(!artifact_visible(
            &VisibilityScope::OperatorOnly,
            &author,
            &query
        ));
    }

    #[test]
    fn sibling_identity_never_sees_sibling_self_only() {
        let query = query_with(Some(child_identity("b")));
        let author = AgentId::new("a");

        assert!(!artifact_visible(
            &VisibilityScope::SelfOnly,
            &author,
            &query
        ));
        // The shared parent is not the author, so the tree stays closed too.
        assert!(!artifact_visible(
            &VisibilityScope::DescendantTree,
            &author,
            &query
        ));
        // A child still sees the tree of its real ancestor.
        assert!(artifact_visible(
            &VisibilityScope::DescendantTree,
            &AgentId::new("parent"),
            &query
        ));
        // A granted role is honoured only for the identity's own role.
        let granted = VisibilityScope::ExplicitlyGranted(vec![AgentRoleRef::new("reader")]);
        assert!(!artifact_visible(&granted, &author, &query));
        let reader = query_with(Some(
            child_identity("b").with_role(AgentRoleRef::new("reader")),
        ));
        assert!(artifact_visible(&granted, &author, &reader));
    }

    #[test]
    fn query_without_identity_keeps_scope_heuristic() {
        let author = AgentId::new("a");
        let operator = RecallQuery::new("", VisibilityScope::OperatorOnly);
        assert!(operator.caller_identity.is_none());

        // The scope heuristic: OperatorOnly opens for an operator scope ...
        assert!(artifact_visible(
            &VisibilityScope::OperatorOnly,
            &author,
            &operator
        ));
        // ... and everything else fails closed, even for the author itself.
        for scope in [
            VisibilityScope::SelfOnly,
            VisibilityScope::DescendantTree,
            VisibilityScope::ExplicitlyGranted(vec![AgentRoleRef::new("reader")]),
        ] {
            assert!(
                !artifact_visible(&scope, &author, &operator),
                "{scope:?} must fail closed without an identity"
            );
        }
        let agent = query_with(None);
        assert!(!artifact_visible(
            &VisibilityScope::OperatorOnly,
            &author,
            &agent
        ));
    }
}
