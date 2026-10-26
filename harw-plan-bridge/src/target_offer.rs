//! Compact top-k offers of admitted role targets for a reconcile cycle.
//!
//! The planner must not inject every agent definition into context. This
//! module is the pure selection step: from the candidates that the trusted
//! runtime knows about, it keeps only the targets that are admitted, granted
//! and model-ready, ranks them by how many wanted capability tags they carry,
//! and returns at most [`MAX_TARGET_OFFERS`] identifiers.
//!
//! The selection never widens authority. `allowed` is the trusted ceiling
//! that corresponds to
//! [`CycleAdmission::allowed_children`](crate::intent_cycle::CycleAdmission::allowed_children);
//! a candidate outside that set is dropped no matter how well it scores.

use std::collections::BTreeSet;

/// Hard upper bound on the number of offers returned in one selection.
pub const MAX_TARGET_OFFERS: usize = 8;

/// One possible target for a child delegation, as seen by the trusted runtime.
///
/// `granted` and `model_ready` are runtime facts: `granted` means the exact
/// named grant exists for this cycle, and `model_ready` means the capability
/// snapshot reports a usable model for the role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetCandidate {
    /// Registry identifier of the target, matched against the admitted set.
    pub id: String,
    /// Capability tags the target advertises.
    pub capability_tags: BTreeSet<String>,
    /// Whether the capability snapshot reports this target as model-ready.
    pub model_ready: bool,
    /// Whether the exact named grant for this target exists.
    pub granted: bool,
}

/// Selects up to `k` (and at most [`MAX_TARGET_OFFERS`]) target identifiers.
///
/// Rules, applied in this order:
///
/// 1. A candidate is dropped if its `id` is not in `allowed`, if it is not
///    `granted`, or if it is not `model_ready`.
/// 2. Each remaining candidate scores the number of `wanted_tags` that are
///    present in its `capability_tags`.
/// 3. Candidates are sorted by score descending, then by `id` ascending.
/// 4. Duplicate identifiers are removed, keeping the first occurrence in that
///    sorted order. Duplicates do not consume slots.
/// 5. At most `k.min(MAX_TARGET_OFFERS)` identifiers are returned.
///
/// The result is deterministic for a given input, independent of the order
/// of `candidates`, and every returned identifier is contained in `allowed`.
#[must_use]
pub fn offer_targets(
    allowed: &BTreeSet<String>,
    candidates: &[TargetCandidate],
    wanted_tags: &BTreeSet<String>,
    k: usize,
) -> Vec<String> {
    let limit = k.min(MAX_TARGET_OFFERS);

    let mut ranked: Vec<(usize, &TargetCandidate)> = candidates
        .iter()
        .filter(|candidate| {
            candidate.granted && candidate.model_ready && allowed.contains(candidate.id.as_str())
        })
        .map(|candidate| {
            let score = wanted_tags
                .iter()
                .filter(|tag| candidate.capability_tags.contains(*tag))
                .count();
            (score, candidate)
        })
        .collect();

    ranked.sort_by(|x, y| y.0.cmp(&x.0).then_with(|| x.1.id.cmp(&y.1.id)));

    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut offers: Vec<String> = Vec::new();
    for (_, candidate) in ranked {
        if offers.len() >= limit {
            break;
        }
        if seen.insert(candidate.id.as_str()) {
            offers.push(candidate.id.clone());
        }
    }
    offers
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{MAX_TARGET_OFFERS, TargetCandidate, offer_targets};

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    fn ids(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    fn cand(id: &str, tags: &[&str], model_ready: bool, granted: bool) -> TargetCandidate {
        TargetCandidate {
            id: id.to_string(),
            capability_tags: set(tags),
            model_ready,
            granted,
        }
    }

    fn ready(id: &str, tags: &[&str]) -> TargetCandidate {
        cand(id, tags, true, true)
    }

    fn numbered(count: usize) -> Vec<TargetCandidate> {
        (0..count)
            .map(|index| ready(&format!("t{index:02}"), &[]))
            .collect()
    }

    fn allowed_of(candidates: &[TargetCandidate]) -> BTreeSet<String> {
        candidates.iter().map(|c| c.id.clone()).collect()
    }

    #[test]
    fn ungranted_and_not_ready_candidates_are_dropped() {
        let candidates = [
            ready("a", &[]),
            cand("b", &[], true, false),
            cand("c", &[], false, true),
            cand("d", &[], false, false),
        ];
        let allowed = allowed_of(&candidates);

        assert_eq!(
            offer_targets(&allowed, &candidates, &set(&[]), MAX_TARGET_OFFERS),
            ids(&["a"])
        );
    }

    #[test]
    fn candidates_outside_allowed_are_never_returned() {
        let candidates = [ready("outside", &["x", "y"]), ready("inside", &[])];
        let allowed = set(&["inside"]);

        assert_eq!(
            offer_targets(&allowed, &candidates, &set(&["x", "y"]), MAX_TARGET_OFFERS),
            ids(&["inside"])
        );
    }

    #[test]
    fn empty_allowed_returns_empty() {
        let candidates = [ready("a", &["x"]), ready("b", &["x"])];

        assert!(offer_targets(&set(&[]), &candidates, &set(&["x"]), MAX_TARGET_OFFERS).is_empty());
    }

    #[test]
    fn tag_matches_order_the_result() {
        let candidates = [
            ready("a", &["x"]),
            ready("b", &["x", "y"]),
            ready("c", &[]),
            ready("d", &["z"]),
        ];
        let allowed = allowed_of(&candidates);

        assert_eq!(
            offer_targets(&allowed, &candidates, &set(&["x", "y"]), MAX_TARGET_OFFERS),
            ids(&["b", "a", "c", "d"])
        );
    }

    #[test]
    fn zero_one_eight_and_nine_eligible_targets() {
        let none: Vec<TargetCandidate> = Vec::new();
        assert!(offer_targets(&set(&[]), &none, &set(&[]), MAX_TARGET_OFFERS).is_empty());

        let one = numbered(1);
        assert_eq!(
            offer_targets(&allowed_of(&one), &one, &set(&[]), MAX_TARGET_OFFERS),
            ids(&["t00"])
        );

        let eight = numbered(8);
        let eight_allowed = allowed_of(&eight);
        assert_eq!(
            offer_targets(&eight_allowed, &eight, &set(&[]), MAX_TARGET_OFFERS),
            ids(&["t00", "t01", "t02", "t03", "t04", "t05", "t06", "t07"])
        );

        let nine = numbered(9);
        let nine_allowed = allowed_of(&nine);
        let capped = offer_targets(&nine_allowed, &nine, &set(&[]), 9);
        assert_eq!(capped.len(), MAX_TARGET_OFFERS);
        assert_eq!(
            capped,
            ids(&["t00", "t01", "t02", "t03", "t04", "t05", "t06", "t07"])
        );
        assert_eq!(
            offer_targets(&nine_allowed, &nine, &set(&[]), usize::MAX).len(),
            MAX_TARGET_OFFERS
        );
    }

    #[test]
    fn k_limits_the_result_below_the_cap() {
        let nine = numbered(9);
        let allowed = allowed_of(&nine);

        assert_eq!(
            offer_targets(&allowed, &nine, &set(&[]), 3),
            ids(&["t00", "t01", "t02"])
        );
    }

    #[test]
    fn k_zero_returns_empty() {
        let candidates = [ready("a", &["x"]), ready("b", &["x"])];
        let allowed = allowed_of(&candidates);

        assert!(offer_targets(&allowed, &candidates, &set(&["x"]), 0).is_empty());
    }

    #[test]
    fn deterministic_ties_are_ordered_by_id() {
        let forward = [ready("a", &["x"]), ready("b", &["x"]), ready("c", &["x"])];
        let mut reversed = forward.clone();
        reversed.reverse();
        let allowed = allowed_of(&forward);

        let first = offer_targets(&allowed, &forward, &set(&["x"]), 2);
        let second = offer_targets(&allowed, &reversed, &set(&["x"]), 2);

        assert_eq!(first, ids(&["a", "b"]));
        assert_eq!(first, second);
    }

    #[test]
    fn duplicate_ids_are_returned_once_at_their_best_position() {
        let candidates = [
            ready("a", &[]),
            ready("b", &["x"]),
            ready("a", &["x", "y"]),
            ready("c", &[]),
        ];
        let allowed = allowed_of(&candidates);

        // Sorted: a(score 2), b(1), a(0), c(0). The second "a" is skipped and
        // does not consume a slot, so k = 2 yields a and b.
        assert_eq!(
            offer_targets(&allowed, &candidates, &set(&["x", "y"]), 2),
            ids(&["a", "b"])
        );
        assert_eq!(
            offer_targets(&allowed, &candidates, &set(&["x", "y"]), MAX_TARGET_OFFERS),
            ids(&["a", "b", "c"])
        );
    }
}
