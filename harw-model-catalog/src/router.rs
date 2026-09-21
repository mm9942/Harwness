//! Role-based model selection router (Layer 2 + 3 + 4 integration).
//!
//! # Responsibility
//! Combines [`crate::descriptor::ModelDescriptor`], [`crate::runtime::ModelRuntimeProfile`],
//! and [`crate::observed::ObservedModelBehavior`] into a deterministic, role-aware ranking
//! function. This module owns no state — all inputs are borrowed references.
//!
//! # Key types exported
//! - [`ModelRole`] — the five harness roles a model can be assigned to.
//! - [`Candidate`] — a borrowed triple (descriptor, profile, observed) for one model.
//!
//! # Concurrency model
//! [`rank`] and [`pick`] are pure functions with no shared mutable state.
//! Both are `Send + Sync` and may be called from any thread without synchronisation.
//!
//! # Error types
//! None — the functions are infallible (they return `Vec` / `Option`).
//!
//! # Design reference
//! See `docs/design/model-catalog-v2.md` §6.
//!
//! # Produktionsstatus (Befund K-router-1)
//! [`rank`] und [`pick`] haben derzeit **keinen produktiven Aufrufer** — nur
//! die Tests dieses Moduls rufen sie auf. Die Stelle, an der `harw-runtime`
//! heute tatsächlich das Modell für eine Kind-Rolle wählt, ist
//! `RuntimeChildRegistryFactory::model_for` (Implementierung des
//! `harw_core::ChildRegistryFactory`-Traits, definiert in
//! `harw-core/src/child_controller.rs:1078`) und dessen
//! komplexitätsbewusste Variante `model_for_task`, in
//! `harw-runtime/src/children.rs:476` bzw. `:502` — sie entscheidet über
//! `internal_point_for_role`/`pinned_model_for_point` (Addendum C/D/E)
//! unabhängig von diesem Router, ohne [`crate::descriptor::ModelDescriptor`],
//! [`crate::runtime::ModelRuntimeProfile`] oder
//! [`crate::observed::ObservedModelBehavior`] heranzuziehen.
//!
//! Ein künftiges Verdrahten von [`rank`]/[`pick`] gehört dorthin: als
//! zusätzliche Kandidatenquelle für `model_for`/`model_for_task`, bevor auf
//! den geerbten Eltern-Provider zurückgefallen wird. Diese Datei nimmt diese
//! Verdrahtung bewusst **nicht** vor — `harw-runtime` liegt außerhalb des
//! Bearbeitungsumfangs dieser Änderung.
//!
//! # Examples
//! ```rust,no_run
//! use harw_model_catalog::router::{ModelRole, Candidate, pick};
//!
//! // Build candidates from bootstrap helpers (requires sibling modules).
//! // let candidates = ...;
//! // let best = pick(ModelRole::Orchestrator, &candidates);
//! ```

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ModelRole
// ---------------------------------------------------------------------------

/// The five harness roles for which a model can be selected.
///
/// # Description
/// Each variant maps to a weighted scoring formula defined in §6 of the design
/// document. The formula combines empirically observed behavioural metrics
/// ([`crate::observed::ObservedModelBehavior`]) to produce a comparable `f32`
/// score in the range `0.0 ..= 100.0`.
///
/// # Serde
/// Serialised as `snake_case` strings (e.g. `"focused_coding_worker"`).
///
/// # Design reference
/// `docs/design/model-catalog-v2.md` §6.
///
/// # Examples
/// ```rust
/// use harw_model_catalog::router::ModelRole;
/// let role = ModelRole::Orchestrator;
/// assert_eq!(serde_json::to_string(&role).unwrap(), "\"orchestrator\"");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    /// High-level coordination: delegates tasks, maintains long context.
    Orchestrator,
    /// Repository-scale work: large context, compaction-resilient.
    RepositoryWorker,
    /// Focused single-file coding: tool-schema reliability is paramount.
    FocusedCodingWorker,
    /// Deterministic verification and build checking.
    Verifier,
    /// Lightweight web/doc research: tool-schema + error recovery.
    Scout,
}

// ---------------------------------------------------------------------------
// Candidate
// ---------------------------------------------------------------------------

/// A complete scoring context for one model candidate.
///
/// # Description
/// Holds borrowed references to all three layers of model metadata required by
/// the router's scoring formula. The lifetime `'a` ties all three references
/// to the same backing storage (e.g. `Vec`s kept alive by the caller).
///
/// # Arguments
/// - `descriptor` (`&'a ModelDescriptor`): declared technical capabilities (Layer 2).
/// - `profile` (`&'a ModelRuntimeProfile`): harness-decided runtime policies (Layer 3).
/// - `observed` (`&'a ObservedModelBehavior`): empirically measured scores (Layer 4).
///
/// # Concurrency
/// `Candidate` is `Send + Sync` because all fields are shared references to
/// `Send + Sync` types. No mutation occurs.
///
/// # Design reference
/// `docs/design/model-catalog-v2.md` §6.
#[derive(Debug, Clone)]
pub struct Candidate<'a> {
    /// Declared technical capabilities (Layer 2).
    pub descriptor: &'a crate::descriptor::ModelDescriptor,
    /// Harness-decided runtime policies (Layer 3).
    pub profile: &'a crate::runtime::ModelRuntimeProfile,
    /// Empirically measured behavioural scores (Layer 4).
    pub observed: &'a crate::observed::ObservedModelBehavior,
}

impl<'a> Candidate<'a> {
    /// Returns whether this candidate can safely be returned as a route.
    ///
    /// A route retains the exact runtime profile selected for the model.  The
    /// router therefore rejects profiles that violate the runtime invariants
    /// before they can reach context construction, retry scheduling, tool
    /// dispatch, or child-agent fanout.
    #[must_use]
    pub const fn has_valid_runtime_profile(&self) -> bool {
        self.profile.validate().is_ok()
    }
}

// ---------------------------------------------------------------------------
// Internal scoring
// ---------------------------------------------------------------------------

/// Computes a role-specific composite score for `candidate` in the range `0.0 ..= 100.0`.
///
/// # Description
/// Applies the weighted formula from §6 of the design document.  Each weight
/// is applied to the raw `u8` value obtained from [`crate::observed::Score::get`]
/// and cast to `f32` before multiplication, so the result is already on the
/// 0–100 scale.
///
/// # Arguments
/// - `role` (`ModelRole`): determines which formula to apply.
/// - `candidate` (`&Candidate<'_>`): the model whose observed metrics are read.
///
/// # Returns
/// A `f32` score in `0.0 ..= 100.0`. Higher is better.
///
/// # Concurrency
/// Pure function; no I/O, no shared state.
///
/// # Design reference
/// `docs/design/model-catalog-v2.md` §6.
fn score(role: ModelRole, candidate: &Candidate<'_>) -> f32 {
    let obs = candidate.observed;
    let tool = obs.tool_schema_reliability.get() as f32;
    let ctx = obs.long_context_retention.get() as f32;
    let deleg = obs.delegation_discipline.get() as f32;
    let recov = obs.recovery_after_tool_error.get() as f32;
    let calib = obs.completion_calibration.get() as f32;
    let compac = obs.compaction_resilience.get() as f32;

    match role {
        ModelRole::Orchestrator => 0.4 * deleg + 0.3 * ctx + 0.3 * tool,
        ModelRole::RepositoryWorker => 0.5 * ctx + 0.3 * compac + 0.2 * tool,
        ModelRole::FocusedCodingWorker => 0.5 * tool + 0.3 * calib + 0.2 * recov,
        ModelRole::Verifier => 0.5 * calib + 0.5 * tool,
        ModelRole::Scout => 0.5 * tool + 0.5 * recov,
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Ranks `candidates` for `role` in descending score order.
///
/// # Description
/// Computes a weighted composite score for every candidate using the formula
/// defined in `docs/design/model-catalog-v2.md` §6, then sorts them:
///
/// 1. Primary key: score **descending** (highest score first).
/// 2. Tie-breaker: `descriptor.model` **ascending** (alphabetical), ensuring
///    the ordering is deterministic even when two candidates have equal scores.
///
/// Candidates with invalid runtime profiles are excluded. This ensures every
/// returned route carries a profile that can be enforced by downstream context,
/// retry, tool-dispatch, and delegation/fanout consumers. All valid candidates
/// are returned — not just the best — so callers can inspect the full ranking
/// or apply additional filters.
///
/// # Arguments
/// - `role` (`ModelRole`): the harness role that drives the scoring formula.
/// - `candidates` (`&'a [Candidate<'a>]`): the pool of models to rank.
///
/// # Returns
/// A `Vec<&'a Candidate<'a>>` containing every candidate with a valid runtime
/// profile, sorted as described above. Returns an empty `Vec` when `candidates`
/// is empty or none has a valid profile.
///
/// # Concurrency
/// Pure function; safe to call from multiple threads simultaneously.
///
/// # Design reference
/// `docs/design/model-catalog-v2.md` §6.
///
/// # Examples
/// ```rust,no_run
/// use harw_model_catalog::router::{ModelRole, rank};
/// // let ranked = rank(ModelRole::Scout, &candidates);
/// ```
pub fn rank<'a>(role: ModelRole, candidates: &'a [Candidate<'a>]) -> Vec<&'a Candidate<'a>> {
    let mut scored: Vec<(&'a Candidate<'a>, f32)> = candidates
        .iter()
        .filter(|candidate| candidate.has_valid_runtime_profile())
        .map(|candidate| (candidate, score(role, candidate)))
        .collect();

    scored.sort_by(|(ca, sa), (cb, sb)| {
        // Primary: descending score.
        sb.partial_cmp(sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Tie-breaker: ascending model id (alphabetical).
            .then_with(|| ca.descriptor.model.cmp(&cb.descriptor.model))
    });

    scored.into_iter().map(|(c, _)| c).collect()
}

/// Returns the highest-scoring candidate for `role`, or `None` if the pool is empty.
///
/// # Description
/// Delegates to [`rank`] and returns the first element, which is the candidate
/// with the highest composite score.  When two candidates are tied on score the
/// one with the lexicographically smaller `descriptor.model` is returned.
///
/// # Arguments
/// - `role` (`ModelRole`): the harness role driving the scoring formula.
/// - `candidates` (`&'a [Candidate<'a>]`): the pool of models to choose from.
///
/// # Returns
/// `Some(&'a Candidate<'a>)` — the best-scoring candidate, retaining its
/// validated runtime profile for downstream enforcement, or `None` if
/// `candidates` is empty or all profiles are invalid.
///
/// # Concurrency
/// Pure function; safe to call from multiple threads simultaneously.
///
/// # Design reference
/// `docs/design/model-catalog-v2.md` §6.
///
/// # Examples
/// ```rust,no_run
/// use harw_model_catalog::router::{ModelRole, pick};
/// // let best = pick(ModelRole::Verifier, &candidates);
/// ```
pub fn pick<'a>(role: ModelRole, candidates: &'a [Candidate<'a>]) -> Option<&'a Candidate<'a>> {
    rank(role, candidates).into_iter().next()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::{
        AgentFeatureSet, Modality, ModalitySet, ModelCapabilities, ModelDescriptor, ModelId,
        ModelLifecycle, PromptCachingSupport, ProviderId, ReasoningSupport, StreamingSupport,
        StructuredOutputSupport, ToolCallingSupport,
    };
    use crate::observed::{ObservedModelBehavior, Score};
    use crate::runtime::{
        CompactionPolicy, ContextPolicy, DelegationPolicy, ModelRuntimeProfile, RetryPolicy,
        TaskShape,
    };

    // -----------------------------------------------------------------------
    // Fixture helpers
    // -----------------------------------------------------------------------

    /// Builds a minimal [`ModelDescriptor`] with the given `model` id.
    fn make_descriptor(provider: &str, model: &str) -> ModelDescriptor {
        ModelDescriptor {
            provider: ProviderId::from(provider),
            model: ModelId::from(model),
            context_window: 8192,
            max_output_tokens: None,
            modalities: ModalitySet(vec![Modality::Text]),
            capabilities: ModelCapabilities {
                tool_calling: ToolCallingSupport::Basic,
                parallel_tools: false,
                structured_output: StructuredOutputSupport::None,
                reasoning: ReasoningSupport::None,
                prompt_caching: PromptCachingSupport::None,
                streaming: StreamingSupport::ServerSent,
                image_input: false,
                native_agent_features: AgentFeatureSet::default(),
            },
            pricing: None,
            lifecycle: ModelLifecycle::Ga,
        }
    }

    /// Builds a conservative default [`ModelRuntimeProfile`].
    fn make_profile() -> ModelRuntimeProfile {
        ModelRuntimeProfile {
            context_policy: ContextPolicy::TightSelect,
            compaction_policy: CompactionPolicy::OnPressure,
            delegation_policy: DelegationPolicy::Cautious,
            retry_policy: RetryPolicy {
                max_retries: 2,
                backoff_ms: 500,
                retry_on_tool_error: false,
            },
            preferred_task_shape: TaskShape::Small,
            max_parallel_tools: 2,
            max_child_fanout: 2,
        }
    }

    /// Builds an [`ObservedModelBehavior`] with all scores set to `value`.
    fn make_observed(provider: &str, model: &str, value: u8) -> ObservedModelBehavior {
        let s = Score::clamp(value);
        ObservedModelBehavior {
            provider: provider.to_owned(),
            model: model.to_owned(),
            tool_schema_reliability: s,
            long_context_retention: s,
            delegation_discipline: s,
            recovery_after_tool_error: s,
            completion_calibration: s,
            compaction_resilience: s,
            updated_at: None,
            evidence: vec![],
        }
    }

    /// Builds an [`ObservedModelBehavior`] with each score set individually.
    #[allow(clippy::too_many_arguments)]
    fn make_observed_detailed(
        provider: &str,
        model: &str,
        tool: u8,
        ctx: u8,
        deleg: u8,
        recov: u8,
        calib: u8,
        compac: u8,
    ) -> ObservedModelBehavior {
        ObservedModelBehavior {
            provider: provider.to_owned(),
            model: model.to_owned(),
            tool_schema_reliability: Score::clamp(tool),
            long_context_retention: Score::clamp(ctx),
            delegation_discipline: Score::clamp(deleg),
            recovery_after_tool_error: Score::clamp(recov),
            completion_calibration: Score::clamp(calib),
            compaction_resilience: Score::clamp(compac),
            updated_at: None,
            evidence: vec![],
        }
    }

    // -----------------------------------------------------------------------
    // Test 1: empty candidates → pick returns None
    // -----------------------------------------------------------------------

    /// Verifies that `pick` returns `None` for an empty candidate pool.
    ///
    /// Design ref: §6 — `pick` delegates to `rank`, which returns an empty
    /// `Vec` for empty input; `into_iter().next()` therefore yields `None`.
    #[test]
    fn test_empty_candidates_returns_none() {
        let result = pick(ModelRole::Orchestrator, &[]);
        assert!(result.is_none(), "pick on empty slice must return None");
    }

    // -----------------------------------------------------------------------
    // Test 2: tie-breaking is alphabetical by model id
    // -----------------------------------------------------------------------

    /// Verifies that candidates with identical scores are ordered alphabetically
    /// by `descriptor.model` (ascending).
    ///
    /// Design ref: §6 — "Bei Gleichstand: alphabetisch nach `descriptor.model`".
    #[test]
    fn test_rank_is_deterministic_on_tie() {
        let desc_b = make_descriptor("prov", "beta-model");
        let desc_a = make_descriptor("prov", "alpha-model");
        let profile = make_profile();
        // Both get score = 50 (all observed metrics = 50).
        let obs_b = make_observed("prov", "beta-model", 50);
        let obs_a = make_observed("prov", "alpha-model", 50);

        let candidates = vec![
            Candidate {
                descriptor: &desc_b,
                profile: &profile,
                observed: &obs_b,
            },
            Candidate {
                descriptor: &desc_a,
                profile: &profile,
                observed: &obs_a,
            },
        ];

        let ranked = rank(ModelRole::Orchestrator, &candidates);
        assert_eq!(ranked.len(), 2, "must return both candidates");
        assert_eq!(
            ranked[0].descriptor.model, "alpha-model",
            "tie-breaker: alpha-model < beta-model alphabetically"
        );
        assert_eq!(ranked[1].descriptor.model, "beta-model");
    }

    // -----------------------------------------------------------------------
    // Test 3: rank returns ALL candidates
    // -----------------------------------------------------------------------

    /// Verifies that `rank` returns exactly as many items as were in the input.
    ///
    /// Design ref: §6 — "Rückgabe: alle Kandidaten in dieser Reihenfolge".
    #[test]
    fn test_rank_returns_all_candidates() {
        let descs: Vec<ModelDescriptor> = (0..7)
            .map(|i| make_descriptor("prov", &format!("model-{i}")))
            .collect();
        let profile = make_profile();
        let obs: Vec<ObservedModelBehavior> = (0..7)
            .map(|i| make_observed("prov", &format!("model-{i}"), (i * 10) as u8))
            .collect();

        let candidates: Vec<Candidate<'_>> = descs
            .iter()
            .zip(obs.iter())
            .map(|(d, o)| Candidate {
                descriptor: d,
                profile: &profile,
                observed: o,
            })
            .collect();

        let ranked = rank(ModelRole::Scout, &candidates);
        assert_eq!(ranked.len(), 7, "rank must return all 7 candidates");
    }

    // -----------------------------------------------------------------------
    // Test 4: pick returns the highest-scoring candidate
    // -----------------------------------------------------------------------

    /// Verifies that `pick` selects the candidate with the higher composite score.
    ///
    /// Uses the `FocusedCodingWorker` formula:
    ///   score = 0.5·tool + 0.3·calib + 0.2·recov
    ///
    /// Candidate A: tool=10, calib=10, recov=10  → score = 10.0
    /// Candidate B: tool=90, calib=80, recov=70  → score = 0.5*90+0.3*80+0.2*70 = 45+24+14 = 83.0
    ///
    /// Design ref: §6 — FocusedCodingWorker formula.
    #[test]
    fn test_pick_returns_best_scoring() {
        let desc_low = make_descriptor("prov", "low-model");
        let desc_high = make_descriptor("prov", "high-model");
        let low_profile = make_profile();
        let high_profile = ModelRuntimeProfile {
            context_policy: ContextPolicy::BroadContext,
            compaction_policy: CompactionPolicy::Periodic,
            delegation_policy: DelegationPolicy::Bold,
            retry_policy: RetryPolicy {
                max_retries: 4,
                backoff_ms: 1_200,
                retry_on_tool_error: true,
            },
            preferred_task_shape: TaskShape::Large,
            max_parallel_tools: 6,
            max_child_fanout: 5,
        };

        // low: all metrics = 10 → FocusedCodingWorker score = 10.0
        let obs_low = make_observed_detailed("prov", "low-model", 10, 10, 10, 10, 10, 10);
        // high: tool=90, calib=80, recov=70 → score = 83.0
        let obs_high = make_observed_detailed("prov", "high-model", 90, 60, 60, 70, 80, 60);

        let candidates = vec![
            Candidate {
                descriptor: &desc_low,
                profile: &low_profile,
                observed: &obs_low,
            },
            Candidate {
                descriptor: &desc_high,
                profile: &high_profile,
                observed: &obs_high,
            },
        ];

        let best = pick(ModelRole::FocusedCodingWorker, &candidates)
            .expect("non-empty pool must yield Some");
        assert_eq!(
            best.descriptor.model, "high-model",
            "pick must return the candidate with the higher score"
        );
        assert_eq!(
            best.profile, &high_profile,
            "the selected route must retain the winning runtime profile"
        );
        assert_eq!(best.profile.context_policy, ContextPolicy::BroadContext);
        assert_eq!(best.profile.retry_policy.max_retries, 4);
        assert!(best.profile.retry_policy.retry_on_tool_error);
        assert_eq!(best.profile.max_parallel_tools, 6);
        assert_eq!(best.profile.delegation_policy, DelegationPolicy::Bold);
        assert_eq!(best.profile.max_child_fanout, 5);
    }

    // -----------------------------------------------------------------------
    // Test 5: invalid profiles cannot be returned as routes
    // -----------------------------------------------------------------------

    /// Verifies that routing rejects an otherwise higher-scoring candidate
    /// whose profile violates the runtime limits.
    #[test]
    fn test_pick_skips_candidate_with_invalid_runtime_profile() {
        let valid_descriptor = make_descriptor("prov", "valid-model");
        let invalid_descriptor = make_descriptor("prov", "invalid-model");
        let valid_profile = make_profile();
        let invalid_profile = ModelRuntimeProfile {
            max_parallel_tools: 0,
            ..make_profile()
        };
        let valid_observed = make_observed("prov", "valid-model", 20);
        let invalid_observed = make_observed("prov", "invalid-model", 95);
        let candidates = vec![
            Candidate {
                descriptor: &valid_descriptor,
                profile: &valid_profile,
                observed: &valid_observed,
            },
            Candidate {
                descriptor: &invalid_descriptor,
                profile: &invalid_profile,
                observed: &invalid_observed,
            },
        ];

        let ranked = rank(ModelRole::Scout, &candidates);
        assert_eq!(ranked.len(), 1, "invalid profiles must not produce routes");
        assert_eq!(ranked[0].descriptor.model, "valid-model");

        let selected = pick(ModelRole::Scout, &candidates).expect("valid candidate must remain");
        assert_eq!(selected.descriptor.model, "valid-model");
        assert!(selected.has_valid_runtime_profile());
        assert!(!candidates[1].has_valid_runtime_profile());
    }

    // -----------------------------------------------------------------------
    // Test 6: ModelRole serialises as snake_case
    // -----------------------------------------------------------------------

    /// Verifies that `ModelRole` serialises to `snake_case` JSON strings.
    ///
    /// Design ref: §6 — `#[serde(rename_all = "snake_case")]` on `ModelRole`.
    #[test]
    fn test_role_enum_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&ModelRole::FocusedCodingWorker).unwrap(),
            "\"focused_coding_worker\""
        );
        assert_eq!(
            serde_json::to_string(&ModelRole::Orchestrator).unwrap(),
            "\"orchestrator\""
        );
        assert_eq!(
            serde_json::to_string(&ModelRole::RepositoryWorker).unwrap(),
            "\"repository_worker\""
        );
        assert_eq!(
            serde_json::to_string(&ModelRole::Verifier).unwrap(),
            "\"verifier\""
        );
        assert_eq!(
            serde_json::to_string(&ModelRole::Scout).unwrap(),
            "\"scout\""
        );
    }

    // -----------------------------------------------------------------------
    // Bonus test: ordering is monotonically non-increasing by score
    // -----------------------------------------------------------------------

    /// Verifies that the ranked list is monotonically non-increasing by score.
    ///
    /// Constructs 5 candidates with distinct observed metrics, ranks them for
    /// every role, and asserts `score[i] >= score[i+1]` throughout.
    ///
    /// Design ref: §6 — "Sortiere absteigend nach Score".
    #[test]
    fn test_ordering_is_monotone_non_increasing() {
        let role = ModelRole::Verifier;

        // score(Verifier) = 0.5*calib + 0.5*tool
        // Assign distinct values so scores are strictly decreasing.
        let configs = [
            ("prov", "model-a", 90u8, 80u8), // score = 85.0
            ("prov", "model-b", 70u8, 60u8), // score = 65.0
            ("prov", "model-c", 50u8, 40u8), // score = 45.0
            ("prov", "model-d", 30u8, 20u8), // score = 25.0
            ("prov", "model-e", 10u8, 0u8),  // score = 5.0
        ];

        let descs: Vec<ModelDescriptor> = configs
            .iter()
            .map(|(p, m, _, _)| make_descriptor(p, m))
            .collect();
        let profile = make_profile();
        let obs: Vec<ObservedModelBehavior> = configs
            .iter()
            .map(|(p, m, calib, tool)| make_observed_detailed(p, m, *tool, 50, 50, 50, *calib, 50))
            .collect();

        // Shuffle input to ensure sorting is not order-dependent.
        let mut candidates: Vec<Candidate<'_>> = descs
            .iter()
            .zip(obs.iter())
            .map(|(d, o)| Candidate {
                descriptor: d,
                profile: &profile,
                observed: o,
            })
            .collect();
        // Reverse before ranking to prove the sort does work.
        candidates.reverse();

        let ranked = rank(role, &candidates);
        assert_eq!(ranked.len(), configs.len());

        for window in ranked.windows(2) {
            let s0 = score(role, window[0]);
            let s1 = score(role, window[1]);
            assert!(
                s0 >= s1,
                "rank is not monotone: {s0} < {s1} for models '{}' and '{}'",
                window[0].descriptor.model,
                window[1].descriptor.model
            );
        }
    }
}
