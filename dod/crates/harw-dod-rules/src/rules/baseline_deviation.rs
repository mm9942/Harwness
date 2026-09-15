//! `BaselineDeviationRule`: eine beobachtete Abweichung von einer Baseline —
//! die Regel, die die Baseline-Gate-Regel in einer vollständigen
//! `Rule::evaluate`-Auswertung demonstriert.
//!
//! # Warum diese Regel existiert
//! Der Arbeitsauftrag verlangt als „wichtigsten Regeltest“: eine
//! `Established`-Baseline darf `RuleTriggered` auslösen, eine `Provisional`
//! nur `Anomaly`. [`crate::baseline::finding_kind_for_status`] kodiert diese
//! Regel bereits als eigenständig geprüfte, reine Funktion — diese Regel
//! verdrahtet sie zusätzlich in einen vollständigen, realistischen Kontext:
//! eine [`crate::baseline::Baseline`] friert erwartete Messwerte in ihrer
//! `evidence.samples` ein (siehe `harw_dod_signals::SecurityEvidence`); diese
//! Regel vergleicht sie mit den aktuell beobachteten
//! [`crate::rule::RuleContext::samples`] mit demselben `metric`-Namen und
//! meldet jede Abweichung, mit der von der Baseline zugelassenen Befund-Art.
//!
//! # Warum `Superseded`-Baselines übersprungen werden
//! Eine zurückgezogene Baseline ist Historie, kein aktiver Bezugspunkt
//! (siehe `PalaceNode`-Moduldoku in `harw-knowledge`: „nodes are never
//! silently deleted“ — zurückgezogen heißt bewahrt, nicht mehr aktiv). Ein
//! Vergleich gegen sie wäre ein Vergleich gegen einen Wert, von dem die
//! Wissensbasis selbst sagt, dass er nicht mehr gilt.
//!
//! # Nebenläufigkeit
//! Zustandsloser Unit-Struct: `Send + Sync`.
//!
//! # Fehler
//! Keine.
//!
//! # Examples
//! ```rust
//! use harw_dod_rules::baseline::{Baseline, PalaceStatus};
//! use harw_dod_rules::rule::{Rule, RuleContext};
//! use harw_dod_rules::rules::BaselineDeviationRule;
//! use harw_dod_rules::FindingKind;
//! use harw_dod_signals::{HostSample, SecurityEvidence, Hardness};
//! use harw_knowledge::artifact::ArtifactId;
//! use harw_sandbox::NetworkScope;
//! use harw_types::SensorId;
//! use std::borrow::Cow;
//!
//! fn sample(value: f64) -> HostSample {
//!     HostSample {
//!         sensor: SensorId::from_str("cpu-0"),
//!         observed_at: jiff::Timestamp::UNIX_EPOCH,
//!         metric: Cow::Borrowed("cpu_util_percent"),
//!         value,
//!     }
//! }
//!
//! let evidence = SecurityEvidence::capture(vec![sample(10.0)], vec![], jiff::Timestamp::UNIX_EPOCH)
//!     .expect("empty events always encode");
//! let baseline = Baseline::new(
//!     ArtifactId::new("baseline/cpu"),
//!     "erwartete CPU-Auslastung",
//!     Hardness::Observed,
//!     evidence,
//! );
//! // Frisch gebaut: `Provisional` (siehe `Baseline::new`).
//! assert_eq!(baseline.status, PalaceStatus::Provisional);
//!
//! let baselines = vec![baseline];
//! let current = vec![sample(95.0)];
//! let scope = NetworkScope::empty();
//! let ctx = RuleContext {
//!     now: jiff::Timestamp::UNIX_EPOCH,
//!     samples: &current,
//!     events: &[],
//!     baselines: &baselines,
//!     network_scope: &scope,
//! };
//!
//! let findings = BaselineDeviationRule.evaluate(&ctx);
//! assert_eq!(findings.len(), 1);
//! assert_eq!(findings[0].kind(), FindingKind::Anomaly);
//! ```

use harw_dod_signals::Hardness;

use crate::baseline::finding_kind_for_status;
use crate::finding::{Finding, FindingKind, Raw};
use crate::rule::{Rule, RuleContext};

/// Meldet eine beobachtete Abweichung von einer Baseline, gegate't durch
/// deren Lebenszyklus-Status.
///
/// # Description
/// Siehe Moduldoku.
///
/// # Errors
/// Keine eigenen Fehler; siehe [`crate::rule`]-Moduldoku.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaselineDeviationRule;

impl Rule for BaselineDeviationRule {
    /// # Returns
    /// `"baseline-deviation"`.
    fn id(&self) -> &'static str {
        "baseline-deviation"
    }

    /// # Arguments
    /// - `ctx` (`&RuleContext<'_>`): siehe [`crate::rule::RuleContext`].
    ///
    /// # Returns
    /// Einen `Finding<Raw>` je Metrik, deren aktueller Wert vom
    /// eingefrorenen Baseline-Wert derselben Metrik abweicht — mit der von
    /// [`crate::baseline::finding_kind_for_status`] zugelassenen Befund-Art.
    /// `Superseded`-Baselines werden übersprungen (siehe Moduldoku). Metriken
    /// ohne aktuellen Messwert derselben Bezeichnung werden ebenfalls
    /// übersprungen — es gibt nichts zu vergleichen.
    fn evaluate(&self, ctx: &RuleContext<'_>) -> Vec<Finding<Raw>> {
        let mut findings = Vec::new();

        for baseline in ctx.baselines {
            let Some(kind) = finding_kind_for_status(baseline.status) else {
                continue;
            };

            for expected in &baseline.evidence.samples {
                let Some(actual) = ctx
                    .samples
                    .iter()
                    .find(|sample| sample.metric == expected.metric)
                else {
                    continue;
                };
                if (actual.value - expected.value).abs() <= f64::EPSILON {
                    continue;
                }

                findings.push(Finding::raw(
                    self.id(),
                    kind,
                    severity_for_kind(kind),
                    Hardness::Correlated,
                    format!(
                        "{metric} deviates from baseline {baseline_id}: expected {expected_value}, observed {observed_value}",
                        metric = expected.metric,
                        baseline_id = baseline.id,
                        expected_value = expected.value,
                        observed_value = actual.value,
                    ),
                    ctx.now,
                ));
            }
        }

        findings
    }
}

// `RuleTriggered` wiegt schwerer als `Anomaly` — konsistent mit der
// Eskalationsleiter, die eine `RuleTriggered`-Meldung ernster nimmt.
fn severity_for_kind(kind: FindingKind) -> harw_dod_signals::Severity {
    match kind {
        FindingKind::RuleTriggered => harw_dod_signals::Severity::High,
        FindingKind::Anomaly => harw_dod_signals::Severity::Low,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline::{Baseline, PalaceStatus};
    use harw_dod_signals::{HostSample, SecurityEvidence};
    use harw_knowledge::artifact::ArtifactId;
    use harw_sandbox::NetworkScope;
    use harw_types::SensorId;
    use jiff::Timestamp;
    use std::borrow::Cow;

    fn sample(metric: &'static str, value: f64) -> HostSample {
        HostSample {
            sensor: SensorId::from_str("cpu-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            metric: Cow::Borrowed(metric),
            value,
        }
    }

    fn baseline_with(status: PalaceStatus, expected_value: f64) -> Baseline {
        let evidence = SecurityEvidence::capture(
            vec![sample("cpu_util_percent", expected_value)],
            vec![],
            Timestamp::UNIX_EPOCH,
        )
        .expect("empty events always encode");
        let mut baseline = Baseline::new(
            ArtifactId::new("baseline/cpu"),
            "erwartete CPU-Auslastung",
            Hardness::Observed,
            evidence,
        );
        match status {
            PalaceStatus::Established => {
                baseline
                    .promote_to_established(true)
                    .expect("reviewed promotion succeeds");
            }
            PalaceStatus::Provisional => {}
            PalaceStatus::Superseded => baseline.status = PalaceStatus::Superseded,
        }
        baseline
    }

    fn ctx_with<'a>(
        samples: &'a [HostSample],
        baselines: &'a [Baseline],
        scope: &'a NetworkScope,
    ) -> RuleContext<'a> {
        RuleContext {
            now: Timestamp::UNIX_EPOCH,
            samples,
            events: &[],
            baselines,
            network_scope: scope,
        }
    }

    #[test]
    fn test_established_baseline_deviation_triggers_rule_triggered() {
        let baselines = vec![baseline_with(PalaceStatus::Established, 10.0)];
        let current = vec![sample("cpu_util_percent", 95.0)];
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&current, &baselines, &scope);

        let findings = BaselineDeviationRule.evaluate(&ctx);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::RuleTriggered);
        assert_eq!(findings[0].severity, harw_dod_signals::Severity::High);
    }

    #[test]
    fn test_provisional_baseline_deviation_yields_anomaly_only() {
        // Der wichtigste Regeltest: dieselbe Abweichung, nur eine
        // `Provisional`-Baseline, darf `RuleTriggered` nicht auslösen.
        let baselines = vec![baseline_with(PalaceStatus::Provisional, 10.0)];
        let current = vec![sample("cpu_util_percent", 95.0)];
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&current, &baselines, &scope);

        let findings = BaselineDeviationRule.evaluate(&ctx);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::Anomaly);
        assert_ne!(findings[0].kind, FindingKind::RuleTriggered);
    }

    #[test]
    fn test_superseded_baseline_is_skipped_entirely() {
        let baselines = vec![baseline_with(PalaceStatus::Superseded, 10.0)];
        let current = vec![sample("cpu_util_percent", 95.0)];
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&current, &baselines, &scope);

        assert!(BaselineDeviationRule.evaluate(&ctx).is_empty());
    }

    #[test]
    fn test_matching_value_does_not_trigger() {
        let baselines = vec![baseline_with(PalaceStatus::Established, 10.0)];
        let current = vec![sample("cpu_util_percent", 10.0)];
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&current, &baselines, &scope);

        assert!(BaselineDeviationRule.evaluate(&ctx).is_empty());
    }

    #[test]
    fn test_metric_absent_from_current_samples_does_not_trigger() {
        let baselines = vec![baseline_with(PalaceStatus::Established, 10.0)];
        let current = vec![sample("mem_used_bytes", 95.0)];
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&current, &baselines, &scope);

        assert!(BaselineDeviationRule.evaluate(&ctx).is_empty());
    }

    #[test]
    fn test_evaluate_is_deterministic_for_identical_context() {
        let baselines = vec![baseline_with(PalaceStatus::Established, 10.0)];
        let current = vec![sample("cpu_util_percent", 95.0)];
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&current, &baselines, &scope);

        assert_eq!(
            BaselineDeviationRule.evaluate(&ctx),
            BaselineDeviationRule.evaluate(&ctx)
        );
    }
}
