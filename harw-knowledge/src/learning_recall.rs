//! Learning recall: scope-before-rank selection with hard budgets.
//!
//! Spec: `docs/planning/90-intent-driven-composed-agents/05-error-learning-and-run-isolation.md`
//! (PL-90 W09b-1, acceptance test 11).
//!
//! [`select_lessons`] is pure and std-only. It first FILTERS the candidate
//! set (revoked, superseded, expired, out-of-scope, invisible, and, if
//! requested, unverified lessons are dropped) and only THEN ranks the
//! survivors by relevance. Ranking therefore never sees an out-of-scope
//! item, so a highly relevant lesson from a foreign scope can neither be
//! returned nor displace an in-scope lesson from the budget.
//!
//! The output is bounded by [`MAX_RECALL_TOP_K`] items and
//! [`MAX_RECALL_TOKENS`] estimated tokens regardless of how many candidates
//! are supplied or what the caller requests.

/// Hard upper bound on the number of lessons a single recall may return.
pub const MAX_RECALL_TOP_K: usize = 8;

/// Hard upper bound on the summed estimated tokens of a single recall.
pub const MAX_RECALL_TOKENS: usize = 1024;

/// A lesson that may be recalled into an agent's context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LessonCandidate {
    /// Stable lesson id; also the deterministic tie-breaker.
    pub id: String,
    /// Scope label the lesson belongs to.
    pub scope_label: String,
    /// Relevance score in thousandths (higher is more relevant).
    pub relevance_milli: u32,
    /// Estimated token cost of injecting the lesson.
    pub est_tokens: usize,
    /// Whether the lesson's provenance is trust-verified.
    pub trust_verified: bool,
    /// Unix expiry time; the lesson is expired when `expires_at <= now`.
    pub expires_at: Option<i64>,
    /// Whether a newer lesson has superseded this one.
    pub superseded: bool,
    /// Whether the lesson has been revoked.
    pub revoked: bool,
}

/// Parameters of one recall request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearningRecallRequest {
    /// Requested maximum number of lessons (clamped to [`MAX_RECALL_TOP_K`]).
    pub top_k: usize,
    /// Requested token ceiling (clamped to [`MAX_RECALL_TOKENS`]).
    pub token_ceiling: usize,
    /// Current time as Unix seconds, used for expiry checks.
    pub now_unix: i64,
    /// Scope labels the caller may recall from.
    pub allowed_scopes: Vec<String>,
    /// When set, unverified lessons are dropped.
    pub require_verified: bool,
}

/// Select lesson ids for recall: filter first, then rank, then budget.
///
/// Returns an empty list when `top_k == 0` or `allowed_scopes` is empty.
/// Ranking is by `relevance_milli` descending, ties by `id` ascending, so
/// the result is deterministic. An item that does not fit the remaining
/// token budget is skipped; later, smaller items may still be taken.
pub fn select_lessons(
    req: &LearningRecallRequest,
    candidates: &[LessonCandidate],
    visible: &dyn Fn(&LessonCandidate) -> bool,
) -> Vec<String> {
    if req.top_k == 0 || req.allowed_scopes.is_empty() {
        return Vec::new();
    }

    // Step 1: filter. Nothing filtered out here ever reaches the ranker.
    let mut eligible: Vec<&LessonCandidate> = candidates
        .iter()
        .filter(|c| is_eligible(req, c, visible))
        .collect();

    // Step 2: rank (relevance desc, id asc).
    eligible.sort_by(|a, b| {
        b.relevance_milli
            .cmp(&a.relevance_milli)
            .then_with(|| a.id.cmp(&b.id))
    });

    // Step 3: apply the hard budgets.
    let k = req.top_k.min(MAX_RECALL_TOP_K);
    let ceiling = req.token_ceiling.min(MAX_RECALL_TOKENS);
    let mut used: usize = 0;
    let mut out: Vec<String> = Vec::new();
    for c in eligible {
        if out.len() >= k {
            break;
        }
        let next = used.saturating_add(c.est_tokens);
        if next > ceiling {
            continue;
        }
        used = next;
        out.push(c.id.clone());
    }
    out
}

fn is_eligible(
    req: &LearningRecallRequest,
    c: &LessonCandidate,
    visible: &dyn Fn(&LessonCandidate) -> bool,
) -> bool {
    if c.revoked || c.superseded {
        return false;
    }
    if let Some(exp) = c.expires_at
        && exp <= req.now_unix
    {
        return false;
    }
    if !req.allowed_scopes.contains(&c.scope_label) {
        return false;
    }
    if req.require_verified && !c.trust_verified {
        return false;
    }
    visible(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, scope: &str, rel: u32, tokens: usize) -> LessonCandidate {
        LessonCandidate {
            id: id.to_string(),
            scope_label: scope.to_string(),
            relevance_milli: rel,
            est_tokens: tokens,
            trust_verified: true,
            expires_at: None,
            superseded: false,
            revoked: false,
        }
    }

    fn req(top_k: usize, ceiling: usize) -> LearningRecallRequest {
        LearningRecallRequest {
            top_k,
            token_ceiling: ceiling,
            now_unix: 1_000,
            allowed_scopes: vec!["a".to_string()],
            require_verified: false,
        }
    }

    fn all(_: &LessonCandidate) -> bool {
        true
    }

    #[test]
    fn out_of_scope_high_relevance_never_returned() {
        let cs = [cand("x", "other", 999, 1), cand("y", "a", 1, 1)];
        let out = select_lessons(&req(8, 1024), &cs, &all);
        assert_eq!(out, vec!["y".to_string()]);
    }

    #[test]
    fn invisible_high_relevance_never_returned() {
        let cs = [cand("x", "a", 999, 1), cand("y", "a", 1, 1)];
        let hide = |c: &LessonCandidate| c.id != "x";
        let out = select_lessons(&req(8, 1024), &cs, &hide);
        assert_eq!(out, vec!["y".to_string()]);
    }

    #[test]
    fn revoked_superseded_expired_excluded() {
        let mut revoked = cand("r", "a", 900, 1);
        revoked.revoked = true;
        let mut superseded = cand("s", "a", 800, 1);
        superseded.superseded = true;
        let mut expired = cand("e", "a", 700, 1);
        expired.expires_at = Some(1_000);
        let mut fresh = cand("f", "a", 100, 1);
        fresh.expires_at = Some(1_001);
        let cs = [revoked, superseded, expired, fresh];
        let out = select_lessons(&req(8, 1024), &cs, &all);
        assert_eq!(out, vec!["f".to_string()]);
    }

    #[test]
    fn unverified_dropped_only_when_required() {
        let mut u = cand("u", "a", 500, 1);
        u.trust_verified = false;
        let cs = [u, cand("v", "a", 100, 1)];
        let lax = select_lessons(&req(8, 1024), &cs, &all);
        assert_eq!(lax, vec!["u".to_string(), "v".to_string()]);
        let mut strict = req(8, 1024);
        strict.require_verified = true;
        let out = select_lessons(&strict, &cs, &all);
        assert_eq!(out, vec!["v".to_string()]);
    }

    #[test]
    fn ties_are_deterministic_by_id() {
        let cs = [
            cand("c", "a", 10, 1),
            cand("a", "a", 10, 1),
            cand("b", "a", 10, 1),
        ];
        let out = select_lessons(&req(8, 1024), &cs, &all);
        assert_eq!(out, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    #[test]
    fn top_k_is_clamped() {
        let cs: Vec<LessonCandidate> = (0..20)
            .map(|i| cand(&format!("id{i:02}"), "a", 100, 1))
            .collect();
        assert_eq!(
            select_lessons(&req(100, 1024), &cs, &all).len(),
            MAX_RECALL_TOP_K
        );
        assert_eq!(select_lessons(&req(3, 1024), &cs, &all).len(), 3);
    }

    #[test]
    fn token_ceiling_is_clamped() {
        let cs: Vec<LessonCandidate> = (0..4)
            .map(|i| cand(&format!("id{i}"), "a", 100, 400))
            .collect();
        // Requested ceiling far above the hard cap: only 2 * 400 <= 1024 fit.
        let out = select_lessons(&req(8, usize::MAX), &cs, &all);
        assert_eq!(out.len(), 2);
        // Lower requested ceiling wins.
        let out = select_lessons(&req(8, 400), &cs, &all);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn oversize_item_is_skipped_not_stop() {
        let cs = [
            cand("big", "a", 900, 500),
            cand("huge", "a", 800, 600),
            cand("small", "a", 100, 100),
        ];
        let out = select_lessons(&req(8, 600), &cs, &all);
        assert_eq!(out, vec!["big".to_string(), "small".to_string()]);
    }

    #[test]
    fn saturating_tokens_do_not_overflow() {
        let cs = [cand("a1", "a", 10, usize::MAX), cand("a2", "a", 5, 1)];
        let out = select_lessons(&req(8, 1024), &cs, &all);
        assert_eq!(out, vec!["a2".to_string()]);
    }

    #[test]
    fn candidate_count_does_not_change_bound() {
        let make = |n: usize| -> Vec<LessonCandidate> {
            (0..n)
                .map(|i| cand(&format!("id{i:05}"), "a", 100, 1))
                .collect()
        };
        let small = select_lessons(&req(100, 1024), &make(10), &all);
        let large = select_lessons(&req(100, 1024), &make(10_000), &all);
        assert_eq!(small.len(), MAX_RECALL_TOP_K);
        assert_eq!(large.len(), MAX_RECALL_TOP_K);
    }

    #[test]
    fn empty_scopes_or_zero_top_k_yield_empty() {
        let cs = [cand("x", "a", 100, 1)];
        let mut r = req(8, 1024);
        r.allowed_scopes.clear();
        assert!(select_lessons(&r, &cs, &all).is_empty());
        assert!(select_lessons(&req(0, 1024), &cs, &all).is_empty());
    }
}
