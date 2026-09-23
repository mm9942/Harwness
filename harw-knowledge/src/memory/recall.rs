//! Bounded retrieval: the single read path every surface uses (§2.3).
//!
//! Spec: `docs/design/knowledge-surfaces.md` §2.3. `search` is bounded on three
//! axes (`max_artifacts`, `max_hops`, visibility) so it can never become an
//! unbounded context dump. Matching is keyword + tag + link-graph traversal
//! with BM25-style scoring over frontmatter tags and body text — no embedding
//! dependency in the core crate. The [`RecallRanker`] trait is the explicit
//! extension point for a future embedding-backed ranker; [`KeywordRanker`] is
//! the built-in BM25-lite default. All scoring is implemented fully here.
//!
//! The visibility bound (§7) is enforced twice, not once: once on the direct
//! candidate set in [`search_with`], and again on every hop in
//! [`expand_backlinks`]. `KnowledgeIndex::backlinks` returns linking
//! artifacts regardless of their own visibility, so a hop-level check is
//! required — otherwise a restricted artifact that merely links to (or is
//! linked from) an already-visible hit would leak into the result set
//! through the link graph alone (AW4-05 fixed this; see the `expand_backlinks`
//! doc and the `mod tests` regression test below).

use std::collections::HashMap;

use crate::artifact::{ArtifactId, ArtifactKind, KnowledgeArtifact, RecallQuery};
use crate::error::KnowledgeResult;
use crate::index::{ArtifactRef, KnowledgeIndex};

/// BM25 term-frequency saturation parameter.
const BM25_K1: f64 = 1.2;
/// BM25 length-normalization parameter.
const BM25_B: f64 = 0.75;
/// Per-hop score decay applied to artifacts reached via backlink traversal.
const HOP_DECAY: f64 = 0.5;

/// One scored recall result plus the palace hop-path that surfaced it (§2.3).
#[derive(Debug, Clone)]
pub struct RecallHit {
    /// The matched artifact (lightweight projection).
    pub artifact: ArtifactRef,
    /// BM25 (or decayed) relevance score.
    pub score: f64,
    /// Backlink hop path from a direct keyword hit; empty for a direct hit.
    pub hop_path: Vec<ArtifactId>,
}

/// The bounded result set returned by [`search`] (§2.3).
#[derive(Debug, Clone)]
pub struct RecallResult {
    /// Scored hits, highest score first, capped at `query.max_artifacts`.
    pub hits: Vec<RecallHit>,
    /// `true` when scoring produced more direct hits than the cap allowed.
    pub truncated: bool,
}

/// A candidate's position within the ranker's input slice, plus its score.
#[derive(Debug, Clone, Copy)]
pub struct RankedCandidate {
    /// Index into the `candidates` slice passed to [`RecallRanker::rank`].
    pub index: usize,
    /// Relevance score (higher is more relevant).
    pub score: f64,
}

/// Ranking extension point (§2.3): a future embedding-backed ranker plugs in
/// here without changing the interface every surface depends on.
pub trait RecallRanker {
    /// Score `candidates` against `query`, returning ranked positions (desc).
    fn rank(&self, query: &RecallQuery, candidates: &[&KnowledgeArtifact]) -> Vec<RankedCandidate>;
}

/// The built-in BM25-lite keyword/tag ranker (no embedding dependency).
#[derive(Debug, Clone, Copy, Default)]
pub struct KeywordRanker;

impl RecallRanker for KeywordRanker {
    fn rank(&self, query: &RecallQuery, candidates: &[&KnowledgeArtifact]) -> Vec<RankedCandidate> {
        let query_terms = tokenize(&query.text);
        if query_terms.is_empty() || candidates.is_empty() {
            return Vec::new();
        }

        // Build per-document term frequencies over body + tags.
        let docs: Vec<HashMap<String, u32>> = candidates
            .iter()
            .map(|artifact| {
                let mut terms = tokenize(&artifact.body);
                for tag in &artifact.frontmatter.tags {
                    terms.extend(tokenize(tag));
                }
                term_frequencies(&terms)
            })
            .collect();

        let doc_lengths: Vec<f64> = docs
            .iter()
            .map(|tf| tf.values().map(|&count| f64::from(count)).sum())
            .collect();
        let num_docs = docs.len() as f64;
        let avg_dl = {
            let total: f64 = doc_lengths.iter().sum();
            let mean = total / num_docs;
            if mean > 0.0 { mean } else { 1.0 }
        };

        // Document frequency per query term.
        let mut doc_freq: HashMap<&str, f64> = HashMap::new();
        for term in &query_terms {
            let df = docs.iter().filter(|tf| tf.contains_key(term)).count() as f64;
            doc_freq.insert(term.as_str(), df);
        }

        let mut ranked: Vec<RankedCandidate> = docs
            .iter()
            .enumerate()
            .map(|(index, tf)| {
                let dl = doc_lengths[index];
                let score = query_terms
                    .iter()
                    .map(|term| {
                        let f = f64::from(tf.get(term).copied().unwrap_or(0));
                        if f == 0.0 {
                            return 0.0;
                        }
                        let df = doc_freq.get(term.as_str()).copied().unwrap_or(0.0);
                        let idf = (((num_docs - df + 0.5) / (df + 0.5)) + 1.0).ln();
                        let denom = f + BM25_K1 * (1.0 - BM25_B + BM25_B * dl / avg_dl);
                        idf * (f * (BM25_K1 + 1.0)) / denom
                    })
                    .sum::<f64>();
                RankedCandidate { index, score }
            })
            .filter(|candidate| candidate.score > 0.0)
            .collect();

        ranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| candidates[a.index].id.cmp(&candidates[b.index].id))
        });
        ranked
    }
}

/// A ranker that keeps every visibility-filtered candidate, in stable id
/// order, instead of scoring against a keyword query (AW5-09).
///
/// [`KeywordRanker`] returns nothing for an empty query text (§2.3: it is a
/// *keyword* ranker, not a *browse* ranker) — that makes it unusable for an
/// operator surface that needs to list every artifact of a kind (e.g. every
/// `ArtifactKind::ContextProposal`) regardless of content. `ListAllRanker`
/// fills that gap without opening a second, unchecked read path: it is a
/// [`RecallRanker`] like any other, so it only ever sees the candidate slice
/// [`search_with`] has *already* filtered by kind/tags/`VisibilityScope` —
/// the visibility bound (§7) is enforced exactly once, in `search_with`,
/// same as for [`KeywordRanker`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ListAllRanker;

impl RecallRanker for ListAllRanker {
    /// Ranks every candidate with an identical score, in the order the
    /// caller passed them (already sorted by id in [`search_with`]).
    fn rank(
        &self,
        _query: &RecallQuery,
        candidates: &[&KnowledgeArtifact],
    ) -> Vec<RankedCandidate> {
        (0..candidates.len())
            .map(|index| RankedCandidate { index, score: 0.0 })
            .collect()
    }
}

/// Run a bounded recall search using the built-in [`KeywordRanker`] (§2.3).
///
/// # Errors
/// [`crate::error::KnowledgeError::RecallBoundExceeded`] if the query's bounds
/// exceed the hard maxima.
pub fn search(index: &KnowledgeIndex, query: &RecallQuery) -> KnowledgeResult<RecallResult> {
    search_with(index, query, &KeywordRanker)
}

/// Run a bounded recall search with a caller-supplied ranker (§2.3).
///
/// Applies kind/tag/visibility filtering, ranks the candidates, caps to
/// `max_artifacts`, then expands palace backlinks up to `max_hops` with a
/// per-hop decay, never exceeding the artifact cap.
///
/// # Errors
/// [`crate::error::KnowledgeError::RecallBoundExceeded`] if the query's bounds
/// exceed the hard maxima.
pub fn search_with(
    index: &KnowledgeIndex,
    query: &RecallQuery,
    ranker: &dyn RecallRanker,
) -> KnowledgeResult<RecallResult> {
    query.validate()?;

    // Deterministic candidate ordering (HashMap iteration is unordered).
    let mut candidates: Vec<&KnowledgeArtifact> = index
        .iter()
        .filter(|artifact| query.kinds.is_empty() || query.kinds.contains(&artifact.kind))
        .filter(|artifact| {
            query
                .tags
                .iter()
                .all(|tag| artifact.frontmatter.tags.contains(tag))
        })
        .filter(|artifact| {
            artifact
                .frontmatter
                .visibility
                .visible_to_caller(&query.caller_scope)
        })
        .collect();
    candidates.sort_by(|a, b| a.id.cmp(&b.id));

    let ranked = ranker.rank(query, &candidates);
    let truncated = ranked.len() > query.max_artifacts;

    let mut hits: Vec<RecallHit> = Vec::new();
    let mut seen: Vec<ArtifactId> = Vec::new();
    for candidate in ranked.iter().take(query.max_artifacts) {
        let artifact = candidates[candidate.index];
        hits.push(RecallHit {
            artifact: ArtifactRef::from_artifact(artifact),
            score: candidate.score,
            hop_path: Vec::new(),
        });
        seen.push(artifact.id.clone());
    }

    if query.max_hops > 0 {
        expand_backlinks(index, query, &mut hits, &mut seen);
    }

    verify_no_steward_domain_leak(&hits, query);

    Ok(RecallResult { hits, truncated })
}

/// Defense-in-depth re-check for the Context Steward's `steward_leak`
/// null counter (AW6-08, see [`crate::context_steward`]).
///
/// # Description
/// The direct-candidate filter above and the per-hop check in
/// [`expand_backlinks`] already make it structurally impossible for a hit
/// whose own visibility does not clear `query.caller_scope` to reach `hits`
/// — this is the same guarantee K35 fixed for backlinks. This function does
/// not add a new read path or change that guarantee; it re-verifies it on
/// the assembled result, scoped to the two `ArtifactKind`s the Context
/// Steward curates ([`ArtifactKind::ContextProposal`],
/// [`ArtifactKind::ModelBehaviorProposal`]), both recommended
/// `OperatorOnly` (`context_proposal::RECOMMENDED_VISIBILITY`,
/// `model_behavior_proposal::RECOMMENDED_VISIBILITY`). Under correct
/// filtering this loop runs every call and never finds a hit — exactly the
/// null-counter shape: an invariant made observable, not a second gate.
///
/// Reached in production: every call this function's caller
/// ([`search_with`]) makes runs this loop, including the real
/// `/context-proposal list`/`view` path (`harw-ops/src/context_proposal.rs`
/// via `harw_ops::register_all`, wired into `harw-cli`), whose query always
/// filters `kinds = [ArtifactKind::ContextProposal]` — so the loop body's
/// `ArtifactKind::ContextProposal` arm executes on every such call, even
/// though (correctly) it never finds a violation. The
/// `ArtifactKind::ModelBehaviorProposal` arm of the `matches!` below is not
/// yet reached by any registered operation (no `/model-behavior-proposal`
/// command exists), which is why the module doc calls this counter's
/// coverage partial rather than fully proven.
fn verify_no_steward_domain_leak(hits: &[RecallHit], query: &RecallQuery) {
    for hit in hits {
        let is_steward_domain = matches!(
            hit.artifact.kind,
            ArtifactKind::ContextProposal | ArtifactKind::ModelBehaviorProposal
        );
        if is_steward_domain
            && !hit
                .artifact
                .visibility
                .visible_to_caller(&query.caller_scope)
        {
            crate::context_steward::STEWARD_LEAK_VIOLATION.violated(&harw_observe::NullSink, &[]);
        }
    }
}

/// Breadth-first palace-backlink expansion from the direct hits (§2.2, §2.3).
///
/// Each traversed backlink is re-checked against `query.caller_scope` (§7)
/// before it is added or traversed further: `KnowledgeIndex::backlinks`
/// returns every linking artifact regardless of its own visibility, so
/// without this check a restricted artifact that merely links to (or is
/// linked from) an already-visible hit would leak into the result set even
/// though it would never pass the direct-candidate filter in [`search_with`]
/// on its own (AW4-05 finding; see the module-level doc and the
/// `test_backlink_expansion_does_not_leak_a_self_only_note` regression test).
fn expand_backlinks(
    index: &KnowledgeIndex,
    query: &RecallQuery,
    hits: &mut Vec<RecallHit>,
    seen: &mut Vec<ArtifactId>,
) {
    // Frontier entries carry (id, base_score, hop_depth, path-to-here).
    let mut frontier: Vec<(ArtifactId, f64, u8, Vec<ArtifactId>)> = hits
        .iter()
        .map(|hit| {
            (
                hit.artifact.id.clone(),
                hit.score,
                0u8,
                vec![hit.artifact.id.clone()],
            )
        })
        .collect();

    while let Some((node_id, base_score, depth, path)) = pop_front(&mut frontier) {
        if depth >= query.max_hops || hits.len() >= query.max_artifacts {
            continue;
        }
        for backref in index.backlinks(&node_id) {
            if hits.len() >= query.max_artifacts {
                break;
            }
            if seen.contains(&backref.id) {
                continue;
            }
            seen.push(backref.id.clone());
            if !backref.visibility.visible_to_caller(&query.caller_scope) {
                // Restricted artifact reached only via the link graph: it
                // would not have passed the direct-candidate filter above,
                // so it must not be added, and must not be traversed
                // further either (its own backlinks reveal nothing about
                // what this caller may see).
                continue;
            }
            let next_depth = depth + 1;
            let score = base_score * HOP_DECAY.powi(i32::from(next_depth));
            let mut next_path = path.clone();
            next_path.push(backref.id.clone());
            hits.push(RecallHit {
                artifact: backref.clone(),
                score,
                hop_path: next_path.clone(),
            });
            frontier.push((backref.id.clone(), base_score, next_depth, next_path));
        }
    }
}

/// Remove and return the first frontier entry (FIFO for breadth-first order).
fn pop_front<T>(queue: &mut Vec<T>) -> Option<T> {
    if queue.is_empty() {
        None
    } else {
        Some(queue.remove(0))
    }
}

/// Tokenize text into lowercase alphanumeric terms (ASCII word split).
#[must_use]
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .collect()
}

/// Count occurrences of each term.
fn term_frequencies(terms: &[String]) -> HashMap<String, u32> {
    let mut map: HashMap<String, u32> = HashMap::new();
    for term in terms {
        *map.entry(term.clone()).or_insert(0) += 1;
    }
    map
}

#[cfg(test)]
mod tests {
    use super::{ListAllRanker, search, search_with};
    use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact, RecallQuery};
    use crate::index::KnowledgeIndex;
    use crate::test_support::TestResult;
    use crate::visibility::{AgentId, VisibilityScope};

    /// Build a test artifact with a fixed author/timestamp and the given
    /// kind/visibility/body/outgoing-links, matching this crate's other
    /// test-module fixture style (see `index.rs`).
    fn artifact(
        id: &str,
        kind: ArtifactKind,
        visibility: VisibilityScope,
        body: &str,
        links: Vec<ArtifactId>,
    ) -> KnowledgeArtifact {
        let mut frontmatter = Frontmatter::new(
            AgentId::new("agent"),
            visibility,
            jiff::Timestamp::UNIX_EPOCH,
        );
        frontmatter.links = links;
        KnowledgeArtifact::new(ArtifactId::new(id), kind, frontmatter, body)
    }

    /// The literal AW4-05 ask: a `SecurityFinding` marked `OperatorOnly`
    /// must not come back for a caller without operator permission.
    #[test]
    fn test_operator_only_security_finding_absent_from_recall_without_operator_permission()
    -> TestResult {
        let mut index = KnowledgeIndex::new();
        index.insert(artifact(
            "security/critical-vuln",
            ArtifactKind::SecurityFinding,
            VisibilityScope::OperatorOnly,
            "critical vulnerability found in the auth module",
            Vec::new(),
        ));

        let query = RecallQuery::new("vulnerability", VisibilityScope::SelfOnly);
        let result = search(&index, &query)
            .map_err(crate::test_support::ctx("bounded recall query succeeds"))?;

        assert!(result.hits.is_empty());
        Ok(())
    }

    /// The counterpart: the same `OperatorOnly` finding must come back for a
    /// caller that does hold operator scope.
    #[test]
    fn test_operator_only_security_finding_present_with_operator_permission() -> TestResult {
        let mut index = KnowledgeIndex::new();
        index.insert(artifact(
            "security/critical-vuln",
            ArtifactKind::SecurityFinding,
            VisibilityScope::OperatorOnly,
            "critical vulnerability found in the auth module",
            Vec::new(),
        ));

        let query = RecallQuery::new("vulnerability", VisibilityScope::OperatorOnly);
        let result = search(&index, &query)
            .map_err(crate::test_support::ctx("bounded recall query succeeds"))?;

        assert_eq!(result.hits.len(), 1);
        assert_eq!(
            result.hits[0].artifact.id,
            ArtifactId::new("security/critical-vuln")
        );
        Ok(())
    }

    /// AW4-05's actual finding: before the `expand_backlinks` fix, a `SelfOnly`
    /// diary note that merely *linked to* an `OperatorOnly` security finding
    /// leaked into an operator's recall results through backlink expansion,
    /// even though `VisibilityScope::visible_to_caller` (`visibility.rs`)
    /// documents that `SelfOnly` must fail closed for *every* caller,
    /// operator included. The direct-candidate filter alone cannot catch
    /// this because the note only ever surfaces via the link graph, not via
    /// a keyword match on its own body.
    #[test]
    fn test_backlink_expansion_does_not_leak_a_self_only_note() -> TestResult {
        let mut index = KnowledgeIndex::new();
        index.insert(artifact(
            "security/critical-vuln",
            ArtifactKind::SecurityFinding,
            VisibilityScope::OperatorOnly,
            "critical vulnerability found in the auth module",
            Vec::new(),
        ));
        index.insert(artifact(
            "diary/private-note",
            ArtifactKind::DiaryEntry,
            VisibilityScope::SelfOnly,
            "private note referencing the vulnerability",
            vec![ArtifactId::new("security/critical-vuln")],
        ));

        let query = RecallQuery::new("vulnerability", VisibilityScope::OperatorOnly);
        let result = search(&index, &query)
            .map_err(crate::test_support::ctx("bounded recall query succeeds"))?;

        let ids: Vec<ArtifactId> = result
            .hits
            .iter()
            .map(|hit| hit.artifact.id.clone())
            .collect();
        assert_eq!(ids, vec![ArtifactId::new("security/critical-vuln")]);
        Ok(())
    }

    /// AW5-09: a `ContextProposal` artifact stored with
    /// `context_proposal::RECOMMENDED_VISIBILITY` (`OperatorOnly`) is absent
    /// from recall for a caller without operator permission, and present for
    /// one that has it — the same rule that governs `SecurityFinding`, now
    /// exercised through a real `ContextProposal::to_artifact` embedding
    /// rather than the generic `artifact()` test fixture above.
    #[test]
    fn test_context_proposal_visibility_follows_recommended_visibility() -> TestResult {
        use crate::context_proposal::{ContextProposal, RECOMMENDED_VISIBILITY};
        use harw_agent_dsl::ids::DefinitionId;

        let proposal = ContextProposal::new(
            ArtifactId::new("context-proposal/promote-history-tail"),
            "history.tail wiederholt über Budget ausgelassen",
            DefinitionId::parse("harwness.context.base@1")
                .map_err(crate::test_support::ctx("valid definition id"))?,
            "deadbeef".repeat(8),
            Vec::new(),
            Vec::new(),
            jiff::Timestamp::UNIX_EPOCH,
            "heuristic:must-include-promotion@1",
        );
        let frontmatter = Frontmatter::new(
            AgentId::new("system"),
            RECOMMENDED_VISIBILITY,
            jiff::Timestamp::UNIX_EPOCH,
        );
        let artifact = proposal
            .to_artifact(frontmatter)
            .map_err(crate::test_support::ctx("proposal embeds into an artifact"))?;

        let mut index = KnowledgeIndex::new();
        index.insert(artifact);

        let unauthorized = RecallQuery::new("", VisibilityScope::SelfOnly);
        let denied = search_with(&index, &unauthorized, &ListAllRanker)
            .map_err(crate::test_support::ctx("bounded recall query succeeds"))?;
        assert!(
            denied.hits.is_empty(),
            "a ContextProposal must not be visible without operator permission"
        );

        let authorized = RecallQuery::new("", VisibilityScope::OperatorOnly);
        let granted = search_with(&index, &authorized, &ListAllRanker)
            .map_err(crate::test_support::ctx("bounded recall query succeeds"))?;
        assert_eq!(
            granted.hits.len(),
            1,
            "the same ContextProposal must be visible to an operator caller"
        );
        assert_eq!(
            granted.hits[0].artifact.id,
            ArtifactId::new("context-proposal/promote-history-tail")
        );
        Ok(())
    }

    /// `ListAllRanker` returns every visibility-filtered candidate — unlike
    /// `KeywordRanker`, which returns nothing for an empty query text.
    #[test]
    fn test_list_all_ranker_returns_every_candidate_for_an_empty_query() -> TestResult {
        let mut index = KnowledgeIndex::new();
        index.insert(artifact(
            "topic/a",
            ArtifactKind::TopicMemory,
            VisibilityScope::OperatorOnly,
            "first",
            Vec::new(),
        ));
        index.insert(artifact(
            "topic/b",
            ArtifactKind::TopicMemory,
            VisibilityScope::OperatorOnly,
            "second",
            Vec::new(),
        ));

        let query = RecallQuery::new("", VisibilityScope::OperatorOnly);

        let keyword_result = search_with(&index, &query, &super::KeywordRanker)
            .map_err(crate::test_support::ctx("query succeeds"))?;
        assert!(
            keyword_result.hits.is_empty(),
            "KeywordRanker yields nothing for an empty query text"
        );

        let list_all_result = search_with(&index, &query, &ListAllRanker)
            .map_err(crate::test_support::ctx("query succeeds"))?;
        assert_eq!(list_all_result.hits.len(), 2);
        Ok(())
    }

    /// White-box drive of `steward_leak_violation_total` (AW6-08): the
    /// direct/backlink filters make an actual leak structurally impossible
    /// through the public `search`/`search_with` surface (that guarantee is
    /// covered by the tests above and by K35's own regression test), so the
    /// only way to prove the counter itself fires is to call the private
    /// re-check directly with a hand-built, already-filtered-wrong `RecallHit`
    /// — exactly the "should never happen given correct callers" shape a null
    /// counter exists to catch.
    #[test]
    fn test_verify_no_steward_domain_leak_fires_on_a_hand_built_violation() {
        use super::RecallHit;
        use crate::context_steward::{STEWARD_COUNTER_LOCK, STEWARD_LEAK_VIOLATION};
        use crate::index::ArtifactRef;

        let _guard = STEWARD_COUNTER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = STEWARD_LEAK_VIOLATION.count();

        let leaking_hit = RecallHit {
            artifact: ArtifactRef {
                id: ArtifactId::new("context-proposal/should-not-leak"),
                kind: ArtifactKind::ContextProposal,
                tags: Vec::new(),
                visibility: VisibilityScope::SelfOnly,
            },
            score: 0.0,
            hop_path: Vec::new(),
        };
        let query = RecallQuery::new("", VisibilityScope::OperatorOnly);

        super::verify_no_steward_domain_leak(&[leaking_hit], &query);

        assert!(
            STEWARD_LEAK_VIOLATION.count() > before,
            "a SelfOnly ContextProposal hit invisible to an OperatorOnly caller must trip the counter"
        );
    }

    /// The counterpart: a hit whose kind is outside the Context Steward's
    /// domain never trips `steward_leak_violation_total`, even if it would
    /// itself fail `visible_to_caller` — the counter watches exactly the two
    /// Steward-curated kinds, not visibility in general (K35's own
    /// regression test already covers the general case).
    #[test]
    fn test_verify_no_steward_domain_leak_ignores_non_steward_kinds() {
        use super::RecallHit;
        use crate::context_steward::{STEWARD_COUNTER_LOCK, STEWARD_LEAK_VIOLATION};
        use crate::index::ArtifactRef;

        let _guard = STEWARD_COUNTER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = STEWARD_LEAK_VIOLATION.count();

        let unrelated_hit = RecallHit {
            artifact: ArtifactRef {
                id: ArtifactId::new("diary/unrelated"),
                kind: ArtifactKind::DiaryEntry,
                tags: Vec::new(),
                visibility: VisibilityScope::SelfOnly,
            },
            score: 0.0,
            hop_path: Vec::new(),
        };
        let query = RecallQuery::new("", VisibilityScope::OperatorOnly);

        super::verify_no_steward_domain_leak(&[unrelated_hit], &query);

        assert_eq!(
            STEWARD_LEAK_VIOLATION.count(),
            before,
            "a non-Steward-domain kind must never trip this counter"
        );
    }
}
