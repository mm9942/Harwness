//! `BaselineDeviationRule`: Abweichung von einer bekannten Baseline.
//!
//! # Verantwortungsbereich
//! Prüft jede [`HostSample`] im [`crate::rule::RuleContext::samples`], ob sie
//! eine zugehörige [`crate::baseline::Baseline`] verletzt. Im Unterschied zur
//! `StructureDriftRule` geht es hier **einzelne Werte außerhalb** eines
//! erwarteten Bereichs.

use std::collections::BTreeSet;

use harw_dod_signals::{HostSample, Severity};

use crate::baseline::{Baseline, BaselineError};
use crate::finding::{Finding, FindingKind, Hardness, Raw};
use crate::rule::{Rule, RuleContext};

/// Meldet einzelne Sample-Werte außerhalb ihrer Baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaselineDeviationRule;

impl Rule for BaselineDeviationRule {
    fn id(&self) -> &'static str {
        "baseline-deviation"
    }

    fn evaluate(&self, ctx: &RuleContext<'_>) -> Vec<Finding<Raw>> {
        let by_metric: std::collections::BTreeMap<_, _> =
            ctx.baselines.iter().map(|b| (b.metric(), b)).collect();

        let mut seen_metrics_with_finding: BTreeSet<&str> = BTreeSet::new();
        let mut findings = Vec::new();

        for sample in ctx.samples {
            let Some(baseline) = by_metric.get(sample.metric.as_ref()) else {
                continue;
            };
            if seen_metrics_with_finding.contains(sample.metric.as_ref()) {
                continue;
            }
            match is_within_baseline(baseline, sample) {
                Ok(true) | Err(BaselineError::UnsupportedValue) => continue,
                Ok(false) => {
                    seen_metrics_with_finding.insert(sample.metric.as_ref());
                    findings.push(Finding::raw(
                        self.id(),
                        FindingKind::Anomaly,
                        Severity::Medium,
                        Hardness::Observed,
                        format!(
                            "sample metric '{}' is outside expected baseline",
                            sample.metric
                        ),
                        ctx.now,
                    ));
                }
                Err(BaselineError::MissingBaseline) => continue,
                Err(BaselineError::PromotionNotReviewed { .. })
                | Err(BaselineError::InvalidRange { .. })
                | Err(BaselineError::EvidenceEncoding { .. }) => continue,
            }
        }

        findings
    }
}

fn is_within_baseline(baseline: &Baseline, sample: &HostSample) -> Result<bool, BaselineError> {
    Ok(baseline.contains(sample.value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use harw_dod_signals::SensorId;
    use jiff::Timestamp;

    fn sample(metric: &str, value: f64) -> HostSample {
        HostSample {
            sensor: SensorId::from_str("test"),
            observed_at: Timestamp::UNIX_EPOCH,
            metric: std::borrow::Cow::Borrowed(metric),
            value,
        }
    }

    fn ctx_with(
        samples: &[HostSample],
        baselines: &[Baseline],
        scope: &NetworkScope,
    ) -> RuleContext<'_> {
        RuleContext {
            now: Timestamp::UNIX_EPOCH,
            samples,
            events: &[],
            baselines,
            network_scope: scope,
        }
    }

    #[test]
    fn test_value_within_baseline_does_not_trigger() -> TestResult {
        let baseline =
            Baseline::new("cpu-load", "cpu-load", 0.0, 80.0).map_err(ctx("gültige Baseline"))?;
        let samples = vec![sample("cpu-load", 42.0)];
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&samples, &[baseline], &scope);

        assert!(BaselineDeviationRule.evaluate(&ctx).is_empty());
        Ok(())
    }

    #[test]
    fn test_value_above_baseline_triggers_anomaly() -> TestResult {
        let baseline =
            Baseline::new("cpu-load", "cpu-load", 0.0, 80.0).map_err(ctx("gültige Baseline"))?;
        let samples = vec![sample("cpu-load", 99.9)];
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&samples, &[baseline], &scope);

        let findings = BaselineDeviationRule.evaluate(&ctx);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::Anomaly);
        assert_eq!(findings[0].severity, Severity::Medium);
        assert!(findings[0].summary.contains("cpu-load"));
        Ok(())
    }

    #[test]
    fn test_missing_baseline_does_not_trigger() {
        let samples = vec![sample("cpu-load", 99.9)];
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&samples, &[], &scope);
        assert!(BaselineDeviationRule.evaluate(&ctx).is_empty());
    }

    #[test]
    fn test_only_one_finding_per_metric() -> TestResult {
        let baseline =
            Baseline::new("cpu-load", "cpu-load", 0.0, 80.0).map_err(ctx("gültige Baseline"))?;
        let samples = vec![sample("cpu-load", 90.0), sample("cpu-load", 95.0)];
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&samples, &[baseline], &scope);

        let findings = BaselineDeviationRule.evaluate(&ctx);
        assert_eq!(findings.len(), 1);
        Ok(())
    }

    #[test]
    fn test_evaluate_is_deterministic_for_identical_context() -> TestResult {
        let baseline =
            Baseline::new("cpu-load", "cpu-load", 0.0, 80.0).map_err(ctx("gültige Baseline"))?;
        let samples = vec![sample("cpu-load", 99.9)];
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&samples, &[baseline], &scope);

        assert_eq!(
            BaselineDeviationRule.evaluate(&ctx),
            BaselineDeviationRule.evaluate(&ctx)
        );
        Ok(())
    }
}
