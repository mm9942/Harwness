//! `StructureDriftRule`: Auffällige Veränderungen der beobachteten
//! Datenstruktur.
//!
//! # Verantwortungsbereich
//! Prüft [`crate::rule::RuleContext::samples`] auf bislang in der zugehörigen
//! Baseline **nicht vorkommende Metriken**. Im Unterschied zur
//! `BaselineDeviationRule` geht es hier nicht um den Wertebereich, sondern
//! darum, dass überhaupt neue Signalarten auftauchen.

use std::collections::HashSet;

use harw_dod_signals::Severity;

use crate::baseline::Baseline;
use crate::finding::{Finding, FindingKind, Hardness, Raw};
use crate::rule::{Rule, RuleContext};

/// Meldet neue Sample-Metriken, die in keiner bekannten Baseline definiert sind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructureDriftRule;

impl Rule for StructureDriftRule {
    fn id(&self) -> &'static str {
        "structure-drift"
    }

    fn evaluate(&self, ctx: &RuleContext<'_>) -> Vec<Finding<Raw>> {
        let expected: HashSet<&str> = ctx.baselines.iter().map(Baseline::metric).collect();
        let mut seen = HashSet::new();
        let mut findings = Vec::new();

        for sample in ctx.samples {
            if expected.contains(sample.metric.as_ref()) {
                continue;
            }
            if !seen.insert(sample.metric.clone()) {
                continue;
            }
            findings.push(Finding::raw(
                self.id(),
                FindingKind::Anomaly,
                Severity::Low,
                Hardness::Observed,
                format!(
                    "sample metric '{}' is not defined by any baseline (structure drift)",
                    sample.metric
                ),
                ctx.now,
            ));
        }

        findings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_dod_signals::{HostSample, SensorId};
    use jiff::Timestamp;
    use std::borrow::Cow;

    fn sample(metric: &str) -> HostSample {
        HostSample {
            sensor: SensorId::from_str("test"),
            observed_at: Timestamp::UNIX_EPOCH,
            metric: Cow::Borrowed(metric),
            value: 1.0,
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
    fn test_known_metric_does_not_trigger() {
        let baseline = Baseline::new("cpu-load", "cpu-load", 0.0, 100.0).unwrap();
        let samples = vec![sample("cpu-load")];
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&samples, &[baseline], &scope);
        assert!(StructureDriftRule.evaluate(&ctx).is_empty());
    }

    #[test]
    fn test_unknown_metric_triggers_structure_drift() {
        let baseline = Baseline::new("cpu-load", "cpu-load", 0.0, 100.0).unwrap();
        let samples = vec![sample("disk-queue")];
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&samples, &[baseline], &scope);

        let findings = StructureDriftRule.evaluate(&ctx);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::Anomaly);
        assert_eq!(findings[0].severity, Severity::Low);
        assert!(findings[0].summary.contains("disk-queue"));
    }

    #[test]
    fn test_only_one_finding_per_unknown_metric() {
        let baseline = Baseline::new("cpu-load", "cpu-load", 0.0, 100.0).unwrap();
        let samples = vec![sample("disk-queue"), sample("disk-queue")];
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&samples, &[baseline], &scope);

        assert_eq!(StructureDriftRule.evaluate(&ctx).len(), 1);
    }

    #[test]
    fn test_known_and_unknown_metrics_together() {
        let baseline = Baseline::new("cpu-load", "cpu-load", 0.0, 100.0).unwrap();
        let samples = vec![sample("cpu-load"), sample("disk-queue")];
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&samples, &[baseline], &scope);

        let findings = StructureDriftRule.evaluate(&ctx);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].summary.contains("disk-queue"));
    }
}
