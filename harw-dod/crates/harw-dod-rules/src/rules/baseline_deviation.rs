//! `BaselineDeviationRule`: Abweichung von einer bekannten Baseline.
//!
//! # Verantwortungsbereich
//! Prüft jede [`HostSample`] im [`crate::rule::RuleContext::samples`], ob sie
//! eine zugehörige [`crate::baseline::Baseline`] verletzt. Im Unterschied zur
//! `StructureDriftRule` geht es hier **einzelne Werte außerhalb** eines
//! erwarteten Bereichs.
//!
//! # Status-Gate (PL-100 WP1.7)
//! Nur eine `Established`-Baseline eskaliert: ihre Verletzung wird
//! `FindingKind::RuleTriggered`. Eine `Provisional`-Baseline ergibt nur
//! `FindingKind::Anomaly` (zur Kenntnisnahme, die Eskalationsleiter stuft sie
//! nie ein), eine `Superseded`-Baseline nichts
//! ([`crate::baseline::finding_kind_for_status`]).
//!
//! # Mehrdeutiger oder ungültiger Satz: fail-closed (PL-100 WP1.7)
//! - **Doppelte Metrik:** Haben im Satz zwei aktive Baselines dieselbe
//!   Metrik, ist das ein Fehler
//!   ([`crate::baseline::BaselineError::DuplicateMetric`]), kein „die letzte
//!   gewinnt": welcher Bereich gilt, hinge sonst von der Reihenfolge ab.
//! - **Ungültiger Bereich:** eine NaN-Grenze oder `min > max` (z. B. aus
//!   `Baseline::with_details` oder dem ungeprüften Struct-Literal in
//!   `harw-knowledge`).
//!
//! Die Regel bewertet eine solche Metrik nicht gegen ihre Baselines, schweigt
//! aber auch nicht: hat die Metrik in dieser Auswertung mindestens ein
//! endliches Sample, entsteht **genau ein** Befund über den Satz selbst —
//! `FindingKind::Anomaly`, [`Severity::Medium`], `Hardness::Observed`,
//! gestempelt mit dem ersten endlichen Sample der Metrik. Sonst könnte eine
//! einzige zusätzliche Baseline (etwa eine `Provisional`-Dublette oder eine
//! NaN-Grenze) eine `Established`-Baseline unbemerkt abschalten
//! (Bedrohung B9, Schutzgut „keine stille Blindheit"). Andere Metriken
//! bleiben bewertbar. Den Fehler selbst liefert
//! [`crate::baseline::check_baselines`], das der Aufrufer beim Laden des
//! Satzes ruft ([`Rule::evaluate`] liefert keine Fehler).
//!
//! # Was nie einen Befund ergibt (PL-100 WP1.7)
//! - **Nicht endlicher Wert:** Ein Sample mit NaN oder ±∞ wird übergangen,
//!   bevor irgendetwas anderes geprüft wird — es ergibt weder eine
//!   Abweichung noch den Befund über einen mehrdeutigen oder ungültigen Satz
//!   und stempelt auch keinen Befund. Ein späteres, endliches Sample
//!   derselben Metrik wird normal bewertet.
//! - **Keine Baseline** für die Metrik, oder nur `Superseded`-Baselines.
//!
//! # Zeitstempel (PL-100 WP0.2)
//! Ein Befund trägt das `observed_at` des ersten abweichenden Samples seiner
//! Metrik, nie [`crate::rule::RuleContext::now`]: eine wiederholte
//! Auswertung derselben Beobachtung erzeugt denselben Stempel.
//!
//! # Zusammenfassung
//! Die Zusammenfassung nennt nur die Metrik, escapt über
//! `harw_untrusted_text::escape_line` (der Name kann aus einer
//! modellgeschriebenen Wissensnotiz stammen), und keinen Messwert: dieselbe
//! Metrik ergibt immer dieselbe Zusammenfassung. Ein Befund über einen
//! mehrdeutigen oder ungültigen Satz sagt das ausdrücklich
//! (`… is ambiguous; metric not evaluated` bzw.
//! `… is invalid; metric not evaluated`).

use std::collections::BTreeSet;

use harw_dod_signals::{HostSample, Severity};
use harw_untrusted_text::escape_line;

use crate::baseline::{
    Baseline, BaselineError, active_baselines_by_metric, finding_kind_for_status,
};
use crate::finding::{Finding, FindingKind, Hardness, Raw};
use crate::rule::{Rule, RuleContext};

// Obergrenze (in Zeichen) für den Metriknamen in der Zusammenfassung.
const SUMMARY_METRIC_MAX_CHARS: usize = 64;

/// Meldet einzelne Sample-Werte außerhalb ihrer Baseline.
///
/// # Description
/// Je Metrik höchstens ein Befund pro Auswertung, gestempelt mit dem ersten
/// abweichenden Sample. Art nach Status der Baseline (siehe Moduldoku):
/// `Established` → `RuleTriggered`, `Provisional` → `Anomaly`,
/// `Superseded` → nichts. Severity `Medium`, Hardness `Observed`. Eine
/// doppelte Metrik oder ein ungültiger Bereich ergibt statt einer Bewertung
/// genau einen `Anomaly`-Befund über den Satz (fail-closed, siehe
/// Moduldoku).
///
/// # Errors
/// Keine: ein nicht endlicher Wert ergibt keinen Befund, ein mehrdeutiger
/// oder ungültiger Satz einen Befund statt eines Fehlers (siehe Moduldoku).
///
/// # Concurrency
/// Zustandsloser Unit-Struct, `Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaselineDeviationRule;

impl Rule for BaselineDeviationRule {
    fn id(&self) -> &'static str {
        "baseline-deviation"
    }

    fn evaluate(&self, ctx: &RuleContext<'_>) -> Vec<Finding<Raw>> {
        // Kein „die letzte gewinnt": eine doppelte Metrik steht im Index als
        // Fehler (siehe Moduldoku).
        let by_metric = active_baselines_by_metric(ctx.baselines);

        let mut seen_metrics_with_finding: BTreeSet<&str> = BTreeSet::new();
        let mut findings = Vec::new();

        for sample in ctx.samples {
            let metric = sample.metric.as_ref();
            if seen_metrics_with_finding.contains(metric) {
                continue;
            }
            // NaN und ±∞ sind keine Messung: sie ergeben nie einen Befund —
            // auch nicht den Satz-Befund — und stempeln keinen (siehe
            // Moduldoku).
            if !sample.value.is_finite() {
                continue;
            }
            let entry = by_metric.get(metric);
            // Fail-closed: ein mehrdeutiger oder ungültiger Satz ergibt beim
            // ersten Sample der Metrik genau einen Befund (siehe Moduldoku).
            if let Some(problem) = set_problem(entry) {
                seen_metrics_with_finding.insert(metric);
                findings.push(Finding::raw(
                    self.id(),
                    FindingKind::Anomaly,
                    Severity::Medium,
                    Hardness::Observed,
                    format!(
                        "baseline set for metric '{}' is {problem}; metric not evaluated",
                        escape_line(metric, SUMMARY_METRIC_MAX_CHARS)
                    ),
                    // Samplezeit, nicht `ctx.now` — siehe Moduldoku.
                    sample.observed_at,
                ));
                continue;
            }
            match deviation_kind(entry, sample) {
                Ok(Some(kind)) => {
                    seen_metrics_with_finding.insert(metric);
                    findings.push(Finding::raw(
                        self.id(),
                        kind,
                        Severity::Medium,
                        Hardness::Observed,
                        format!(
                            "sample metric '{}' is outside expected baseline",
                            escape_line(metric, SUMMARY_METRIC_MAX_CHARS)
                        ),
                        // Samplezeit, nicht `ctx.now` — siehe Moduldoku.
                        sample.observed_at,
                    ));
                }
                // Im Bereich, oder ihr Status erlaubt keine Befund-Art
                // (`Superseded` fehlt schon im Index).
                Ok(None) => continue,
                // Keine Baseline: nicht bewertbar, kein Befund.
                Err(BaselineError::MissingBaseline) => continue,
                // Oben bereits als Satz-Befund gemeldet (`set_problem`).
                Err(BaselineError::DuplicateMetric { .. })
                | Err(BaselineError::InvalidRange { .. }) => continue,
                // Entstehen beim Bewerten nie (nicht endliche Werte sind oben
                // schon übergangen); der Vollständigkeit halber.
                Err(BaselineError::UnsupportedValue)
                | Err(BaselineError::PromotionNotReviewed { .. })
                | Err(BaselineError::EvidenceEncoding { .. }) => continue,
            }
        }

        findings
    }
}

// Warum der Satz einer Metrik nicht bewertbar ist: `Some("ambiguous")` bei
// mehr als einer aktiven Baseline, `Some("invalid")` bei einem ungültigen
// Bereich, sonst `None`.
fn set_problem(entry: Option<&Result<&Baseline, BaselineError>>) -> Option<&'static str> {
    match entry {
        Some(Err(BaselineError::DuplicateMetric { .. })) => Some("ambiguous"),
        Some(Err(BaselineError::InvalidRange { .. })) => Some("invalid"),
        _ => None,
    }
}

// Bewertet ein endliches Sample gegen den Index-Eintrag seiner Metrik:
// `Ok(Some(kind))` bei einer meldepflichtigen Abweichung, `Ok(None)` im
// Bereich oder bei einer Baseline ohne Befund-Art, `Err` wenn die Metrik
// nicht bewertbar ist. Nicht endliche Werte filtert der Aufrufer vorher.
fn deviation_kind(
    entry: Option<&Result<&Baseline, BaselineError>>,
    sample: &HostSample,
) -> Result<Option<FindingKind>, BaselineError> {
    let baseline = match entry {
        None => return Err(BaselineError::MissingBaseline),
        Some(Err(error)) => return Err(error.clone()),
        Some(Ok(baseline)) => *baseline,
    };
    if baseline.contains(sample.value) {
        return Ok(None);
    }
    Ok(finding_kind_for_status(baseline.status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline::PalaceStatus;
    use crate::test_support::{TestResult, ctx, missing};
    use harw_authority::NetworkScope;
    use harw_types::SensorId;
    use jiff::Timestamp;

    fn sample(metric: &'static str, value: f64) -> HostSample {
        HostSample {
            sensor: SensorId::from_str("test"),
            observed_at: Timestamp::UNIX_EPOCH,
            metric: std::borrow::Cow::Borrowed(metric),
            value,
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
            listener_history: None,
            rule_config: None,
        }
    }

    #[test]
    fn test_value_within_baseline_does_not_trigger() -> TestResult {
        let baseline =
            Baseline::new("cpu-load", "cpu-load", 0.0, 80.0).map_err(ctx("gültige Baseline"))?;
        let samples = vec![sample("cpu-load", 42.0)];
        let scope = NetworkScope::empty();
        let baselines = [baseline];
        let ctx = ctx_with(&samples, &baselines, &scope);

        assert!(BaselineDeviationRule.evaluate(&ctx).is_empty());
        Ok(())
    }

    #[test]
    fn test_value_above_baseline_triggers_anomaly() -> TestResult {
        let baseline =
            Baseline::new("cpu-load", "cpu-load", 0.0, 80.0).map_err(ctx("gültige Baseline"))?;
        let samples = vec![sample("cpu-load", 99.9)];
        let scope = NetworkScope::empty();
        let baselines = [baseline];
        let ctx = ctx_with(&samples, &baselines, &scope);

        let findings = BaselineDeviationRule.evaluate(&ctx);
        assert_eq!(findings.len(), 1);
        let finding = findings.first().ok_or_else(missing("Befund"))?;
        assert_eq!(finding.kind(), FindingKind::Anomaly);
        assert_eq!(finding.severity(), Severity::Medium);
        assert!(finding.summary().contains("cpu-load"));
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
        let baselines = [baseline];
        let ctx = ctx_with(&samples, &baselines, &scope);

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
        let baselines = [baseline];
        let ctx = ctx_with(&samples, &baselines, &scope);

        assert_eq!(
            BaselineDeviationRule.evaluate(&ctx),
            BaselineDeviationRule.evaluate(&ctx)
        );
        Ok(())
    }

    /// PL-100 WP0.2: der Befund trägt die Zeit des abweichenden Samples,
    /// nicht die Auswertungszeit.
    #[test]
    fn test_finding_is_stamped_with_the_sample_time_not_ctx_now() -> TestResult {
        let sample_time: Timestamp = "2026-11-08T08:00:00Z".parse().map_err(ctx("Samplezeit"))?;
        let now: Timestamp = "2026-11-08T08:10:00Z"
            .parse()
            .map_err(ctx("Auswertungszeit"))?;
        let baseline =
            Baseline::new("cpu-load", "cpu-load", 0.0, 80.0).map_err(ctx("gültige Baseline"))?;
        let mut deviating = sample("cpu-load", 99.9);
        deviating.observed_at = sample_time;
        let samples = vec![deviating];
        let scope = NetworkScope::empty();
        let baselines = [baseline];
        let mut rule_ctx = ctx_with(&samples, &baselines, &scope);
        rule_ctx.now = now;

        let stamps: Vec<Timestamp> = BaselineDeviationRule
            .evaluate(&rule_ctx)
            .iter()
            .map(Finding::observed_at)
            .collect();
        assert_eq!(stamps, vec![sample_time]);
        Ok(())
    }

    // Baseline mit Status; `Established` nur über die reviewte Beförderung.
    fn baseline_with(
        id: &str,
        metric: &'static str,
        min: f64,
        max: f64,
        status: PalaceStatus,
    ) -> TestResult<Baseline> {
        let mut baseline = Baseline::new(id, metric, min, max).map_err(ctx("gültige Baseline"))?;
        match status {
            PalaceStatus::Established => baseline
                .promote_to_established(true)
                .map_err(ctx("reviewte Beförderung"))?,
            PalaceStatus::Provisional => {}
            PalaceStatus::Superseded => baseline.status = PalaceStatus::Superseded,
        }
        Ok(baseline)
    }

    // (Metrik-Zusammenfassung, Art) aller Befunde, in Ausgabereihenfolge.
    fn kinds(rule_ctx: &RuleContext<'_>) -> Vec<(String, FindingKind)> {
        BaselineDeviationRule
            .evaluate(rule_ctx)
            .iter()
            .map(|finding| (finding.summary().to_owned(), finding.kind()))
            .collect()
    }

    fn summary_for(metric: &str) -> String {
        format!("sample metric '{metric}' is outside expected baseline")
    }

    fn ambiguous_for(metric: &str) -> String {
        format!("baseline set for metric '{metric}' is ambiguous; metric not evaluated")
    }

    fn invalid_for(metric: &str) -> String {
        format!("baseline set for metric '{metric}' is invalid; metric not evaluated")
    }

    /// PL-100 WP1.7: nur eine `Established`-Baseline eskaliert
    /// (`RuleTriggered`); `Provisional` bleibt `Anomaly`.
    #[test]
    fn test_only_established_baseline_yields_rule_triggered() -> TestResult {
        let baselines = [
            baseline_with("cpu", "cpu-load", 0.0, 80.0, PalaceStatus::Established)?,
            baseline_with("mem", "mem-used", 0.0, 80.0, PalaceStatus::Provisional)?,
        ];
        let samples = vec![sample("cpu-load", 99.9), sample("mem-used", 99.9)];
        let scope = NetworkScope::empty();
        let rule_ctx = ctx_with(&samples, &baselines, &scope);

        assert_eq!(
            kinds(&rule_ctx),
            vec![
                (summary_for("cpu-load"), FindingKind::RuleTriggered),
                (summary_for("mem-used"), FindingKind::Anomaly),
            ]
        );
        let established = BaselineDeviationRule.evaluate(&rule_ctx);
        let first = established.first().ok_or_else(missing("Befund"))?;
        assert_eq!(first.severity(), Severity::Medium);
        assert_eq!(first.hardness(), Hardness::Observed);
        Ok(())
    }

    /// Eine `Established`-Baseline meldet nichts, solange der Wert im Bereich
    /// liegt.
    #[test]
    fn test_established_baseline_within_range_does_not_trigger() -> TestResult {
        let baselines = [baseline_with(
            "cpu",
            "cpu-load",
            0.0,
            80.0,
            PalaceStatus::Established,
        )?];
        let samples = vec![sample("cpu-load", 80.0), sample("cpu-load", 0.0)];
        let scope = NetworkScope::empty();

        assert!(kinds(&ctx_with(&samples, &baselines, &scope)).is_empty());
        Ok(())
    }

    /// Eine `Superseded`-Baseline gate't nichts.
    #[test]
    fn test_superseded_baseline_does_not_trigger() -> TestResult {
        let baselines = [baseline_with(
            "cpu",
            "cpu-load",
            0.0,
            80.0,
            PalaceStatus::Superseded,
        )?];
        let samples = vec![sample("cpu-load", 99.9)];
        let scope = NetworkScope::empty();

        assert!(kinds(&ctx_with(&samples, &baselines, &scope)).is_empty());
        Ok(())
    }

    /// Eine `Superseded`-Baseline neben ihrer Nachfolgerin ist kein Duplikat:
    /// es gilt der Bereich der aktiven Baseline.
    #[test]
    fn test_superseded_baseline_does_not_shadow_its_successor() -> TestResult {
        let baselines = [
            baseline_with("old", "cpu-load", 0.0, 10.0, PalaceStatus::Superseded)?,
            baseline_with("new", "cpu-load", 0.0, 80.0, PalaceStatus::Established)?,
        ];
        let scope = NetworkScope::empty();

        let within = vec![sample("cpu-load", 50.0)];
        assert!(kinds(&ctx_with(&within, &baselines, &scope)).is_empty());

        let outside = vec![sample("cpu-load", 90.0)];
        assert_eq!(
            kinds(&ctx_with(&outside, &baselines, &scope)),
            vec![(summary_for("cpu-load"), FindingKind::RuleTriggered)]
        );
        Ok(())
    }

    /// PL-100 WP1.7: zwei aktive Baselines derselben Metrik sind ein Fehler
    /// statt „die letzte gewinnt" — die Metrik wird nicht bewertet, sondern
    /// ergibt in jeder Reihenfolge denselben Satz-Befund; andere Metriken
    /// bleiben bewertbar.
    #[test]
    fn test_duplicate_metric_is_not_last_wins() -> TestResult {
        // Unter „die letzte gewinnt" hätte die zweite (enge) Baseline bei
        // 90.0 ausgelöst, in umgekehrter Reihenfolge nicht.
        let wide = baseline_with("wide", "cpu-load", 0.0, 100.0, PalaceStatus::Established)?;
        let narrow = baseline_with("narrow", "cpu-load", 0.0, 80.0, PalaceStatus::Established)?;
        let mem = baseline_with("mem", "mem-used", 0.0, 80.0, PalaceStatus::Established)?;
        let samples = vec![sample("cpu-load", 90.0), sample("mem-used", 99.9)];
        let scope = NetworkScope::empty();

        for baselines in [
            [wide.clone(), narrow.clone(), mem.clone()],
            [narrow.clone(), mem.clone(), wide.clone()],
        ] {
            assert_eq!(
                kinds(&ctx_with(&samples, &baselines, &scope)),
                vec![
                    (ambiguous_for("cpu-load"), FindingKind::Anomaly),
                    (summary_for("mem-used"), FindingKind::RuleTriggered),
                ]
            );
            assert_eq!(
                crate::baseline::check_baselines(&baselines),
                Err(BaselineError::DuplicateMetric {
                    metric: "cpu-load".to_owned(),
                })
            );
        }
        Ok(())
    }

    /// Auch identische Baselines derselben Metrik sind mehrdeutig.
    #[test]
    fn test_identical_duplicate_baselines_report_the_ambiguous_set() -> TestResult {
        let baseline = baseline_with("cpu", "cpu-load", 0.0, 80.0, PalaceStatus::Provisional)?;
        let baselines = [baseline.clone(), baseline];
        let samples = vec![sample("cpu-load", 99.9)];
        let scope = NetworkScope::empty();

        assert_eq!(
            kinds(&ctx_with(&samples, &baselines, &scope)),
            vec![(ambiguous_for("cpu-load"), FindingKind::Anomaly)]
        );
        Ok(())
    }

    /// Review-Befund (B9): eine einzige zusätzliche `Provisional`-Dublette
    /// schaltete eine `Established`-Baseline still ab. Jetzt meldet die
    /// Regel den mehrdeutigen Satz — genau einmal, in jeder Reihenfolge,
    /// gestempelt mit dem ersten Sample der Metrik, auch wenn der Wert im
    /// Bereich liegt.
    #[test]
    fn test_provisional_duplicate_cannot_silence_an_established_baseline() -> TestResult {
        let first_time: Timestamp = "2026-11-08T08:00:00Z".parse().map_err(ctx("Samplezeit"))?;
        let later_time: Timestamp = "2026-11-08T08:00:05Z".parse().map_err(ctx("Samplezeit"))?;
        let established = baseline_with("cpu", "cpu-load", 0.0, 80.0, PalaceStatus::Established)?;
        let decoy = baseline_with("decoy", "cpu-load", 0.0, 1.0e9, PalaceStatus::Provisional)?;
        let mut first = sample("cpu-load", 42.0);
        first.observed_at = first_time;
        let mut deviating = sample("cpu-load", 99.9);
        deviating.observed_at = later_time;
        let samples = vec![first, deviating];
        let scope = NetworkScope::empty();

        for baselines in [
            [established.clone(), decoy.clone()],
            [decoy.clone(), established.clone()],
        ] {
            let findings = BaselineDeviationRule.evaluate(&ctx_with(&samples, &baselines, &scope));
            assert_eq!(findings.len(), 1);
            let finding = findings.first().ok_or_else(missing("Satz-Befund"))?;
            assert_eq!(finding.summary(), ambiguous_for("cpu-load"));
            assert_eq!(finding.kind(), FindingKind::Anomaly);
            assert_eq!(finding.severity(), Severity::Medium);
            assert_eq!(finding.hardness(), Hardness::Observed);
            assert_eq!(finding.observed_at(), first_time);
        }
        Ok(())
    }

    /// Ein mehrdeutiger Satz ohne Sample der Metrik ergibt nichts: ohne
    /// Beobachtung gibt es nichts, das unbewertet bliebe.
    #[test]
    fn test_ambiguous_set_without_samples_reports_nothing() -> TestResult {
        let baselines = [
            baseline_with("a", "cpu-load", 0.0, 80.0, PalaceStatus::Established)?,
            baseline_with("b", "cpu-load", 0.0, 90.0, PalaceStatus::Provisional)?,
        ];
        let samples = vec![sample("mem-used", 1.0)];
        let scope = NetworkScope::empty();

        assert!(kinds(&ctx_with(&samples, &baselines, &scope)).is_empty());
        Ok(())
    }

    /// PL-100 WP1.7: NaN und ±∞ erzeugen nie einen Befund, gegen keine
    /// Baseline-Art.
    #[test]
    fn test_non_finite_samples_never_trigger() -> TestResult {
        let scope = NetworkScope::empty();
        for status in [PalaceStatus::Established, PalaceStatus::Provisional] {
            let baselines = [baseline_with("cpu", "cpu-load", 0.0, 80.0, status)?];
            for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                let samples = vec![sample("cpu-load", value)];
                assert!(
                    kinds(&ctx_with(&samples, &baselines, &scope)).is_empty(),
                    "{value} gegen {status:?} darf keinen Befund ergeben"
                );
            }
        }
        Ok(())
    }

    /// Auch ein einseitiger Bereich (`-∞..=max`) macht ±∞ nicht zum Befund.
    #[test]
    fn test_infinite_sample_against_one_sided_baseline_does_not_trigger() -> TestResult {
        let baselines = [baseline_with(
            "cpu",
            "cpu-load",
            f64::NEG_INFINITY,
            80.0,
            PalaceStatus::Established,
        )?];
        let samples = vec![sample("cpu-load", f64::INFINITY)];
        let scope = NetworkScope::empty();

        assert!(kinds(&ctx_with(&samples, &baselines, &scope)).is_empty());
        Ok(())
    }

    /// Ein nicht endliches Sample verdeckt kein späteres, endliches
    /// abweichendes Sample derselben Metrik; der Stempel ist dessen Zeit.
    #[test]
    fn test_non_finite_sample_does_not_hide_a_later_deviation() -> TestResult {
        let deviation_time: Timestamp =
            "2026-11-08T08:00:05Z".parse().map_err(ctx("Samplezeit"))?;
        let baselines = [baseline_with(
            "cpu",
            "cpu-load",
            0.0,
            80.0,
            PalaceStatus::Established,
        )?];
        let mut deviating = sample("cpu-load", 99.9);
        deviating.observed_at = deviation_time;
        let samples = vec![sample("cpu-load", f64::NAN), deviating];
        let scope = NetworkScope::empty();

        let findings = BaselineDeviationRule.evaluate(&ctx_with(&samples, &baselines, &scope));
        let stamps: Vec<(FindingKind, Timestamp)> = findings
            .iter()
            .map(|finding| (finding.kind(), finding.observed_at()))
            .collect();
        assert_eq!(stamps, vec![(FindingKind::RuleTriggered, deviation_time)]);
        Ok(())
    }

    /// Eine Baseline mit NaN-Grenze oder `min > max` (ohne `Baseline::new`
    /// gebaut) wird nicht bewertet — sie löst weder bei jedem Wert aus noch
    /// schweigt sie: je Metrik genau ein Satz-Befund.
    #[test]
    fn test_invalid_baseline_range_reports_the_invalid_set_once() -> TestResult {
        let mut nan_bound = baseline_with("cpu", "cpu-load", 0.0, 80.0, PalaceStatus::Established)?;
        nan_bound.min = f64::NAN;
        let mut reversed = baseline_with("mem", "mem-used", 0.0, 80.0, PalaceStatus::Established)?;
        reversed.min = 90.0;
        let baselines = [nan_bound, reversed];
        let samples = vec![
            sample("cpu-load", 42.0),
            sample("cpu-load", 99.9),
            sample("mem-used", 85.0),
        ];
        let scope = NetworkScope::empty();

        assert_eq!(
            kinds(&ctx_with(&samples, &baselines, &scope)),
            vec![
                (invalid_for("cpu-load"), FindingKind::Anomaly),
                (invalid_for("mem-used"), FindingKind::Anomaly),
            ]
        );
        Ok(())
    }

    /// Review-Befund (Runde 2): bei einem mehrdeutigen oder ungültigen Satz
    /// ergab schon ein NaN-/±∞-Sample den Satz-Befund, gestempelt mit der
    /// Zeit des Müllwerts. Jetzt ergeben nur nicht endliche Samples nichts;
    /// ein späteres endliches Sample ergibt den Satz-Befund mit **seiner**
    /// Zeit.
    #[test]
    fn test_non_finite_samples_never_produce_the_set_finding() -> TestResult {
        let garbage_time: Timestamp = "2026-11-08T08:00:00Z".parse().map_err(ctx("Samplezeit"))?;
        let finite_time: Timestamp = "2026-11-08T08:00:05Z".parse().map_err(ctx("Samplezeit"))?;
        let ambiguous = [
            baseline_with("a", "cpu-load", 0.0, 80.0, PalaceStatus::Established)?,
            baseline_with("b", "cpu-load", 0.0, 90.0, PalaceStatus::Provisional)?,
        ];
        let mut reversed = baseline_with("c", "cpu-load", 0.0, 80.0, PalaceStatus::Established)?;
        reversed.min = 90.0;
        let invalid = [reversed];
        let scope = NetworkScope::empty();

        for baselines in [ambiguous.as_slice(), invalid.as_slice()] {
            let mut garbage: Vec<HostSample> = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY]
                .into_iter()
                .map(|value| sample("cpu-load", value))
                .collect();
            for item in &mut garbage {
                item.observed_at = garbage_time;
            }
            assert!(
                kinds(&ctx_with(&garbage, baselines, &scope)).is_empty(),
                "nur nicht endliche Samples ergeben keinen Befund"
            );

            let mut finite = sample("cpu-load", 42.0);
            finite.observed_at = finite_time;
            garbage.push(finite);
            let findings = BaselineDeviationRule.evaluate(&ctx_with(&garbage, baselines, &scope));
            let stamps: Vec<(FindingKind, Timestamp)> = findings
                .iter()
                .map(|finding| (finding.kind(), finding.observed_at()))
                .collect();
            assert_eq!(stamps, vec![(FindingKind::Anomaly, finite_time)]);
            let finding = findings.first().ok_or_else(missing("Satz-Befund"))?;
            assert!(
                finding
                    .summary()
                    .starts_with("baseline set for metric 'cpu-load' is ")
            );
        }
        Ok(())
    }

    /// Eine NaN-Grenze neben einer gültigen `Established`-Baseline: zwei
    /// aktive Baselines, also mehrdeutig — in jeder Reihenfolge derselbe
    /// Satz-Befund, nie eine stille Lücke.
    #[test]
    fn test_nan_bound_duplicate_reports_the_ambiguous_set_in_both_orders() -> TestResult {
        let valid = baseline_with("cpu", "cpu-load", 0.0, 80.0, PalaceStatus::Established)?;
        let mut nan_bound = baseline_with("nan", "cpu-load", 0.0, 80.0, PalaceStatus::Provisional)?;
        nan_bound.max = f64::NAN;
        let samples = vec![sample("cpu-load", 99.9)];
        let scope = NetworkScope::empty();

        for baselines in [
            [valid.clone(), nan_bound.clone()],
            [nan_bound.clone(), valid.clone()],
        ] {
            assert_eq!(
                kinds(&ctx_with(&samples, &baselines, &scope)),
                vec![(ambiguous_for("cpu-load"), FindingKind::Anomaly)]
            );
        }
        Ok(())
    }

    /// Der Metrikname kann aus einer modellgeschriebenen Notiz stammen: die
    /// Zusammenfassung escapt ihn.
    #[test]
    fn test_summary_escapes_the_metric_name() -> TestResult {
        let metric: &'static str = "cpu\u{202E}load\nFAKE: approved";
        let baselines = [baseline_with(
            "cpu",
            metric,
            0.0,
            80.0,
            PalaceStatus::Provisional,
        )?];
        let samples = vec![sample(metric, 99.9)];
        let scope = NetworkScope::empty();

        let findings = BaselineDeviationRule.evaluate(&ctx_with(&samples, &baselines, &scope));
        let finding = findings.first().ok_or_else(missing("Befund"))?;
        assert!(!finding.summary().contains('\u{202E}'));
        assert!(!finding.summary().contains('\n'));
        assert!(
            finding
                .summary()
                .contains("cpu\\u{202e}load\\u{000a}FAKE: approved")
        );
        Ok(())
    }
}
