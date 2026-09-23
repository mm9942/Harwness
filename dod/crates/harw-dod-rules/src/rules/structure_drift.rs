//! `StructureDriftRule`: Auffällige Veränderungen der beobachteten
//! Struktur.
//!
//! # Verantwortungsbereich
//! Zwei Quellen, ein Befundtyp:
//!
//! 1. **`EventKind::StructureDrift`-Ereignisse** (geliefert von
//!    `harw-dod-workspace`/`harw-dod-scanreport`): jedes Ereignis wird zu
//!    einem `FindingKind::RuleTriggered`-Befund. Die Schwere kommt aus dem
//!    **strukturierten** [`DriftSeverity`]-Feld, nie aus dem Freitext
//!    `detail` — eine Sicherheitsregel, die ihre Einstufung aus Textmustern
//!    rekonstruiert, greift bei einer geänderten Sensor-Formulierung still
//!    daneben (`test_severity_does_not_depend_on_the_free_text` hält das
//!    fest). `DriftSeverity::Unknown` wird zu [`Severity::Medium`], nicht
//!    herabgestuft: eine unbekannte Schwere ist keine geringe.
//! 2. **Neue Sample-Metriken**: [`crate::rule::RuleContext::samples`] mit
//!    einer Metrik, die in **keiner** Baseline vorkommt, ergeben je Metrik
//!    einen `FindingKind::Anomaly`-Befund. Im Unterschied zur
//!    `BaselineDeviationRule` geht es nicht um den Wertebereich, sondern
//!    darum, dass überhaupt neue Signalarten auftauchen. **Nur wenn
//!    mindestens eine Baseline existiert:** ohne jede Baseline gibt es keine
//!    bekannte Struktur, von der abgewichen werden könnte — sonst löste jeder
//!    Host-Sensor (CPU, Speicher, …) in einem Kontext ohne Baselines (z. B.
//!    `harw-sentinel`) in jedem Zyklus einen Befund pro Metrik aus.
//!
//! # Nebenläufigkeit
//! Zustandsloser Unit-Struct: `Send + Sync`.
//!
//! # Fehler
//! Keine.

use std::collections::HashSet;

use harw_dod_signals::{DriftSeverity, EventKind, Severity};

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
        let mut findings: Vec<Finding<Raw>> = ctx
            .events
            .iter()
            .filter_map(|event| {
                let EventKind::StructureDrift { severity, detail } = &event.kind else {
                    return None;
                };
                Some(Finding::raw(
                    self.id(),
                    FindingKind::RuleTriggered,
                    severity_for_drift(*severity),
                    Hardness::Observed,
                    detail.clone(),
                    ctx.now,
                ))
            })
            .collect();

        // Ohne Baseline keine bekannte Struktur — siehe Moduldoku.
        if ctx.baselines.is_empty() {
            return findings;
        }

        let expected: HashSet<&str> = ctx.baselines.iter().map(Baseline::metric).collect();
        let mut seen = HashSet::new();

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

/// Bildet die grobe Schwere des Signalstroms auf die Befundschwere ab.
///
/// `DriftSeverity::Unknown` wird zu [`Severity::Medium`], **nicht** zu einer
/// geringeren Stufe: eine unbekannte Schwere ist keine geringe.
fn severity_for_drift(severity: DriftSeverity) -> Severity {
    match severity {
        DriftSeverity::High => Severity::High,
        DriftSeverity::Medium | DriftSeverity::Unknown => Severity::Medium,
        DriftSeverity::Low => Severity::Low,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use harw_authority::NetworkScope;
    use harw_dod_signals::{HostSample, SecurityEvent};
    use harw_types::SensorId;
    use jiff::Timestamp;
    use std::borrow::Cow;

    fn sample(metric: &'static str) -> HostSample {
        HostSample {
            sensor: SensorId::from_str("test"),
            observed_at: Timestamp::UNIX_EPOCH,
            metric: Cow::Borrowed(metric),
            value: 1.0,
        }
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
    fn test_known_metric_does_not_trigger() -> TestResult {
        let baseline =
            Baseline::new("cpu-load", "cpu-load", 0.0, 100.0).map_err(ctx("gültige Baseline"))?;
        let samples = vec![sample("cpu-load")];
        let scope = NetworkScope::empty();
        let baselines = [baseline];
        let ctx = ctx_with(&samples, &baselines, &scope);
        assert!(StructureDriftRule.evaluate(&ctx).is_empty());
        Ok(())
    }

    #[test]
    fn test_unknown_metric_triggers_structure_drift() -> TestResult {
        let baseline =
            Baseline::new("cpu-load", "cpu-load", 0.0, 100.0).map_err(ctx("gültige Baseline"))?;
        let samples = vec![sample("disk-queue")];
        let scope = NetworkScope::empty();
        let baselines = [baseline];
        let ctx = ctx_with(&samples, &baselines, &scope);

        let findings = StructureDriftRule.evaluate(&ctx);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::Anomaly);
        assert_eq!(findings[0].severity, Severity::Low);
        assert!(findings[0].summary.contains("disk-queue"));
        Ok(())
    }

    #[test]
    fn test_only_one_finding_per_unknown_metric() -> TestResult {
        let baseline =
            Baseline::new("cpu-load", "cpu-load", 0.0, 100.0).map_err(ctx("gültige Baseline"))?;
        let samples = vec![sample("disk-queue"), sample("disk-queue")];
        let scope = NetworkScope::empty();
        let baselines = [baseline];
        let ctx = ctx_with(&samples, &baselines, &scope);

        assert_eq!(StructureDriftRule.evaluate(&ctx).len(), 1);
        Ok(())
    }

    #[test]
    fn test_known_and_unknown_metrics_together() -> TestResult {
        let baseline =
            Baseline::new("cpu-load", "cpu-load", 0.0, 100.0).map_err(ctx("gültige Baseline"))?;
        let samples = vec![sample("cpu-load"), sample("disk-queue")];
        let scope = NetworkScope::empty();
        let baselines = [baseline];
        let ctx = ctx_with(&samples, &baselines, &scope);

        let findings = StructureDriftRule.evaluate(&ctx);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].summary.contains("disk-queue"));
        Ok(())
    }

    /// Drift-Ereignis mit **strukturierter** Schwere und bewusst
    /// nichtssagendem Freitext (die Regel darf ihn nicht auswerten).
    fn drift_event(severity: DriftSeverity) -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("workspace-drift"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::StructureDrift {
                severity,
                detail: "irgendeine Beschreibung".to_owned(),
            },
        }
    }

    fn event_ctx<'a>(events: &'a [SecurityEvent], scope: &'a NetworkScope) -> RuleContext<'a> {
        RuleContext {
            now: Timestamp::UNIX_EPOCH,
            samples: &[],
            events,
            baselines: &[],
            network_scope: scope,
        }
    }

    #[test]
    fn test_structure_drift_event_triggers_rule_finding_without_baselines() {
        let scope = NetworkScope::empty();
        let events = vec![drift_event(DriftSeverity::High)];
        let ctx = event_ctx(&events, &scope);

        let findings = StructureDriftRule.evaluate(&ctx);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::High);
        assert_eq!(findings[0].kind, FindingKind::RuleTriggered);
    }

    #[test]
    fn test_medium_low_and_unknown_drift_severities_map_through() {
        let scope = NetworkScope::empty();
        let events = vec![
            drift_event(DriftSeverity::Medium),
            drift_event(DriftSeverity::Low),
            drift_event(DriftSeverity::Unknown),
        ];
        let ctx = event_ctx(&events, &scope);

        let severities: Vec<_> = StructureDriftRule
            .evaluate(&ctx)
            .iter()
            .map(|f| f.severity)
            .collect();

        assert_eq!(
            severities,
            vec![Severity::Medium, Severity::Low, Severity::Medium]
        );
    }

    #[test]
    fn test_severity_does_not_depend_on_the_free_text() {
        let scope = NetworkScope::empty();
        let mut a = drift_event(DriftSeverity::High);
        let mut b = drift_event(DriftSeverity::High);
        if let EventKind::StructureDrift { detail, .. } = &mut a.kind {
            *detail = "Versionssprung (Patch) bei serde".to_owned();
        }
        if let EventKind::StructureDrift { detail, .. } = &mut b.kind {
            *detail = "Abhängigkeit entfernt: old-crate".to_owned();
        }
        let events = vec![a, b];
        let ctx = event_ctx(&events, &scope);

        let findings = StructureDriftRule.evaluate(&ctx);

        assert_eq!(findings.len(), 2);
        assert!(findings.iter().all(|f| f.severity == Severity::High));
    }

    #[test]
    fn test_non_drift_events_are_ignored() {
        let scope = NetworkScope::empty();
        let events = vec![SecurityEvent {
            sensor: SensorId::from_str("net-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::ListenerOpened { port: 22 },
        }];
        let ctx = event_ctx(&events, &scope);

        assert!(StructureDriftRule.evaluate(&ctx).is_empty());
    }

    #[test]
    fn test_samples_without_any_baseline_do_not_trigger() {
        let samples = vec![sample("cpu-load"), sample("disk-queue")];
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&samples, &[], &scope);

        assert!(StructureDriftRule.evaluate(&ctx).is_empty());
    }

    #[test]
    fn test_evaluate_is_deterministic_for_identical_context() {
        let scope = NetworkScope::empty();
        let events = vec![drift_event(DriftSeverity::Medium)];
        let ctx = event_ctx(&events, &scope);

        assert_eq!(
            StructureDriftRule.evaluate(&ctx),
            StructureDriftRule.evaluate(&ctx)
        );
    }
}
