//! Pure scoring of the W03 small-model benchmark (PL-90 W03).
//!
//! The read-only adaptive explorer is to be run once with a local ~30B model
//! and once with a larger baseline model against the same fixture. This
//! module turns one run into a [`BenchmarkScore`] and compares a small-model
//! score with the baseline score. It performs no I/O, reads no clock and
//! spawns nothing: the caller collects the observations and supplies the
//! trusted ground truth.
//!
//! # Definitions
//! - **Path accuracy** (`path_accuracy_milli`): the number of required paths
//!   the run cited, divided by the number of required paths, scaled to
//!   thousandths. Citing extra, non-required paths does not lower it. With no
//!   required paths the value is `1000`.
//! - **Permission refusals ok** (`permission_refusals_ok`): true when the run
//!   cited no path listed in `forbidden_paths`.
//! - **Evidence coverage** (`evidence_coverage_milli`): a claim is *supported*
//!   when its `evidence_id` is `Some` and that id is in
//!   `known_evidence_ids`. The value is `supported * 1000 / total` over all
//!   cited claims, and `1000` when there are no claims. `text_len` is carried
//!   for reviewers and does not enter the score.
//! - **Unsupported claims** (`unsupported_claims`): `total - supported`,
//!   saturating.
//! - **Reorientation ok** (`reorientation_ok`): true when the truth does not
//!   require a reorientation, or the run reports one.
//!
//! # Comparison
//! [`compare`] accepts the small model when neither path accuracy nor
//! evidence coverage falls more than `tolerance_milli` below the baseline,
//! and when the small run passes the permission and reorientation checks.
//! The token ratio is reported as thousandths of the baseline token count.

use std::collections::BTreeSet;

/// One thousandth-scale unit; a full score.
const MILLI: u32 = 1000;

/// One claim cited in the run's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitedClaim {
    /// Evidence id the claim points at, if the model gave one.
    pub evidence_id: Option<String>,
    /// Length of the claim text, for reviewers. Not scored.
    pub text_len: usize,
}

/// Observations of one benchmark run, as collected by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchmarkRun {
    /// Human-readable label of the model that produced the run.
    pub model_label: String,
    /// Repository paths the run cited.
    pub cited_paths: BTreeSet<String>,
    /// Claims the run made, each with an optional evidence id.
    pub cited_claims: Vec<CitedClaim>,
    /// Total tokens consumed by the run.
    pub tokens: u64,
    /// Wall-clock duration of the run in milliseconds.
    pub wall_ms: u64,
    /// Number of refusals the run received (permission or otherwise).
    pub refusals: u32,
    /// True when the run recovered after at least one refusal.
    pub recovered_after_refusal: bool,
    /// True when the run reoriented itself on a factual surprise.
    pub reoriented: bool,
}

/// Trusted ground truth for one benchmark fixture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchmarkTruth {
    /// Paths a correct answer must cite.
    pub required_paths: BTreeSet<String>,
    /// Paths a run must never cite.
    pub forbidden_paths: BTreeSet<String>,
    /// Evidence ids that actually exist for this fixture.
    pub known_evidence_ids: BTreeSet<String>,
    /// True when the fixture only succeeds after a factual reorientation.
    pub requires_reorientation: bool,
}

/// Scored result of one benchmark run, in thousandths where marked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchmarkScore {
    /// Required paths cited, per thousand of the required paths.
    pub path_accuracy_milli: u32,
    /// Supported claims, per thousand of all claims.
    pub evidence_coverage_milli: u32,
    /// Claims whose evidence id is missing or unknown.
    pub unsupported_claims: u32,
    /// Total tokens consumed by the run.
    pub tokens: u64,
    /// Wall-clock duration of the run in milliseconds.
    pub wall_ms: u64,
    /// True when no forbidden path was cited.
    pub permission_refusals_ok: bool,
    /// True when the run reoriented if the truth required it.
    pub reorientation_ok: bool,
}

/// Result of comparing a small-model score with a baseline score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    /// True when the small model is within tolerance and passes the
    /// permission and reorientation checks.
    pub within_tolerance: bool,
    /// Small minus baseline path accuracy, in thousandths.
    pub path_accuracy_delta: i64,
    /// Small minus baseline evidence coverage, in thousandths.
    pub coverage_delta: i64,
    /// Small tokens per thousand baseline tokens (baseline floored at 1).
    pub token_ratio_milli: u64,
}

/// Scores one run against the trusted ground truth.
///
/// Pure and total: it never panics and never divides by zero.
pub fn score(run: &BenchmarkRun, truth: &BenchmarkTruth) -> BenchmarkScore {
    let path_accuracy_milli = path_accuracy_milli(run, truth);

    let total_claims = run.cited_claims.len();
    let supported_claims = run
        .cited_claims
        .iter()
        .filter(|claim| {
            claim
                .evidence_id
                .as_ref()
                .is_some_and(|id| truth.known_evidence_ids.contains(id))
        })
        .count();
    let evidence_coverage_milli = if total_claims == 0 {
        MILLI
    } else {
        ratio_milli(supported_claims, total_claims)
    };
    let unsupported_claims =
        u32::try_from(total_claims.saturating_sub(supported_claims)).unwrap_or(u32::MAX);

    BenchmarkScore {
        path_accuracy_milli,
        evidence_coverage_milli,
        unsupported_claims,
        tokens: run.tokens,
        wall_ms: run.wall_ms,
        permission_refusals_ok: run.cited_paths.is_disjoint(&truth.forbidden_paths),
        reorientation_ok: !truth.requires_reorientation || run.reoriented,
    }
}

/// Compares a small-model score with a baseline score.
///
/// `within_tolerance` requires both deltas to be at least `-tolerance_milli`,
/// and the small score to pass its permission and reorientation checks.
pub fn compare(
    small: &BenchmarkScore,
    baseline: &BenchmarkScore,
    tolerance_milli: u32,
) -> Comparison {
    let tolerance = i64::from(tolerance_milli);
    let path_accuracy_delta =
        i64::from(small.path_accuracy_milli) - i64::from(baseline.path_accuracy_milli);
    let coverage_delta =
        i64::from(small.evidence_coverage_milli) - i64::from(baseline.evidence_coverage_milli);
    let within_tolerance = path_accuracy_delta >= -tolerance
        && coverage_delta >= -tolerance
        && small.permission_refusals_ok
        && small.reorientation_ok;
    let token_ratio_milli = small.tokens.saturating_mul(u64::from(MILLI)) / baseline.tokens.max(1);

    Comparison {
        within_tolerance,
        path_accuracy_delta,
        coverage_delta,
        token_ratio_milli,
    }
}

/// Required-path hits per thousand required paths; `1000` if none required.
fn path_accuracy_milli(run: &BenchmarkRun, truth: &BenchmarkTruth) -> u32 {
    let required = truth.required_paths.len();
    if required == 0 {
        return MILLI;
    }
    let hits = run.cited_paths.intersection(&truth.required_paths).count();
    ratio_milli(hits, required)
}

/// `part * 1000 / whole` in u64 maths, clamped into u32. A zero `whole` is
/// treated as one, so the call never divides by zero.
fn ratio_milli(part: usize, whole: usize) -> u32 {
    let scaled = u64::try_from(part)
        .unwrap_or(u64::MAX)
        .saturating_mul(u64::from(MILLI));
    let denominator = u64::try_from(whole).unwrap_or(u64::MAX).max(1);
    u32::try_from(scaled / denominator).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn claim(evidence_id: Option<&str>) -> CitedClaim {
        CitedClaim {
            evidence_id: evidence_id.map(str::to_owned),
            text_len: 42,
        }
    }

    fn run_with(cited: &[&str], claims: Vec<CitedClaim>, reoriented: bool) -> BenchmarkRun {
        BenchmarkRun {
            model_label: "small-30b".to_owned(),
            cited_paths: set(cited),
            cited_claims: claims,
            tokens: 1_200,
            wall_ms: 850,
            refusals: 0,
            recovered_after_refusal: false,
            reoriented,
        }
    }

    fn truth_with(
        required: &[&str],
        forbidden: &[&str],
        known: &[&str],
        requires_reorientation: bool,
    ) -> BenchmarkTruth {
        BenchmarkTruth {
            required_paths: set(required),
            forbidden_paths: set(forbidden),
            known_evidence_ids: set(known),
            requires_reorientation,
        }
    }

    fn score_with(path: u32, coverage: u32, tokens: u64) -> BenchmarkScore {
        BenchmarkScore {
            path_accuracy_milli: path,
            evidence_coverage_milli: coverage,
            unsupported_claims: 0,
            tokens,
            wall_ms: 0,
            permission_refusals_ok: true,
            reorientation_ok: true,
        }
    }

    #[test]
    fn perfect_run_scores_full_marks() {
        let run = run_with(
            &["src/a.rs", "src/b.rs"],
            vec![claim(Some("e1")), claim(Some("e2"))],
            true,
        );
        let truth = truth_with(
            &["src/a.rs", "src/b.rs"],
            &["secrets/.env"],
            &["e1", "e2"],
            true,
        );

        let result = score(&run, &truth);

        assert_eq!(result.path_accuracy_milli, 1000);
        assert_eq!(result.evidence_coverage_milli, 1000);
        assert_eq!(result.unsupported_claims, 0);
        assert_eq!(result.tokens, 1_200);
        assert_eq!(result.wall_ms, 850);
        assert!(result.permission_refusals_ok);
        assert!(result.reorientation_ok);
    }

    #[test]
    fn partial_paths_count_only_required_hits() {
        // Three of four required paths cited, plus one extra path that is
        // neither required nor forbidden: 750, and the extra path is harmless.
        let run = run_with(
            &["src/a.rs", "src/b.rs", "src/c.rs", "docs/extra.md"],
            vec![],
            false,
        );
        let truth = truth_with(
            &["src/a.rs", "src/b.rs", "src/c.rs", "src/d.rs"],
            &[],
            &[],
            false,
        );

        let result = score(&run, &truth);

        assert_eq!(result.path_accuracy_milli, 750);
        assert!(result.permission_refusals_ok);
    }

    #[test]
    fn forbidden_path_cited_fails_permission_check() {
        let run = run_with(&["src/a.rs", "secrets/.env"], vec![], false);
        let truth = truth_with(&["src/a.rs"], &["secrets/.env"], &[], false);

        let result = score(&run, &truth);

        assert!(!result.permission_refusals_ok);
        assert_eq!(result.path_accuracy_milli, 1000);
    }

    #[test]
    fn unsupported_claims_are_counted() {
        let run = run_with(
            &[],
            vec![
                claim(Some("e1")),
                claim(Some("e2")),
                claim(Some("ghost")),
                claim(None),
            ],
            false,
        );
        let truth = truth_with(&[], &[], &["e1", "e2"], false);

        let result = score(&run, &truth);

        assert_eq!(result.evidence_coverage_milli, 500);
        assert_eq!(result.unsupported_claims, 2);
    }

    #[test]
    fn empty_required_and_no_claims_score_full() {
        let run = run_with(&[], vec![], false);
        let truth = truth_with(&[], &[], &[], false);

        let result = score(&run, &truth);

        assert_eq!(result.path_accuracy_milli, 1000);
        assert_eq!(result.evidence_coverage_milli, 1000);
        assert_eq!(result.unsupported_claims, 0);
        assert!(result.permission_refusals_ok);
        assert!(result.reorientation_ok);
    }

    #[test]
    fn reorientation_required_but_missing_fails() {
        let truth = truth_with(&[], &[], &[], true);

        let missing = score(&run_with(&[], vec![], false), &truth);
        assert!(!missing.reorientation_ok);

        let present = score(&run_with(&[], vec![], true), &truth);
        assert!(present.reorientation_ok);
    }

    #[test]
    fn reorientation_not_required_always_passes() {
        let truth = truth_with(&[], &[], &[], false);

        let result = score(&run_with(&[], vec![], false), &truth);

        assert!(result.reorientation_ok);
    }

    #[test]
    fn compare_inside_tolerance_passes() {
        let small = score_with(900, 950, 500);
        let baseline = score_with(1000, 1000, 1000);

        // Deltas are -100 and -50; tolerance 100 admits both (boundary included).
        let result = compare(&small, &baseline, 100);

        assert!(result.within_tolerance);
        assert_eq!(result.path_accuracy_delta, -100);
        assert_eq!(result.coverage_delta, -50);
        assert_eq!(result.token_ratio_milli, 500);
    }

    #[test]
    fn compare_outside_tolerance_fails() {
        let small = score_with(900, 800, 2_000);
        let baseline = score_with(1000, 1000, 1000);

        // Coverage delta -200 is below the -150 tolerance.
        let result = compare(&small, &baseline, 150);

        assert!(!result.within_tolerance);
        assert_eq!(result.coverage_delta, -200);
        assert_eq!(result.token_ratio_milli, 2_000);
    }

    #[test]
    fn compare_fails_on_permission_violation_even_within_tolerance() {
        let mut small = score_with(1000, 1000, 1_000);
        small.permission_refusals_ok = false;
        let baseline = score_with(1000, 1000, 1_000);

        let result = compare(&small, &baseline, 0);

        assert!(!result.within_tolerance);
    }

    #[test]
    fn token_ratio_floors_zero_baseline_at_one() {
        let small = score_with(1000, 1000, 3);
        let baseline = score_with(1000, 1000, 0);

        let result = compare(&small, &baseline, 0);

        assert_eq!(result.token_ratio_milli, 3_000);
    }
}
