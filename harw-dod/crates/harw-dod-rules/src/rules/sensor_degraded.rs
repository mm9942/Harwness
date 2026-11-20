//! `SensorDegradedRule`: ein Sensor hat sich als beeinträchtigt gemeldet
//! (PL-100 WP1.6, Bedrohung B19 „blinde Sensoren ohne Hinweis").
//!
//! # Verantwortungsbereich
//! Jedes `EventKind::SensorDegraded`-Ereignis wird zu genau einem Befund.
//! Ohne diese Regel bliebe ein ausgefallener Sensor nur ein Zählerstand —
//! der Mensch erführe nicht, dass die Erkennung gerade blind ist.
//!
//! # Einstufung
//! [`Severity::Medium`], [`Hardness::Observed`] (der Sentinel beobachtet den
//! Ausfall selbst) und `FindingKind::Anomaly`: ein Sensorausfall ist ein
//! Zustand zur Kenntnisnahme, kein Angriff, und darf nie eine Stufe der
//! Eskalationsleiter erreichen (die verlangt `RuleTriggered`). Der Dispatcher
//! leitet `sensor-degraded` nie an ein Modell weiter, sondern erzeugt einen
//! deterministischen Hinweis (README §3.3).
//!
//! # Zeitstempel und Zusammenfassung
//! Ereigniszeit, nie [`crate::rule::RuleContext::now`]. Die Zusammenfassung
//! nennt nur die Kennung des betroffenen Sensors — escapt und gekürzt, weil
//! eine Sensor-Kennung über die Sonden-IPC auch von außerhalb dieser Crate
//! kommen kann — und ist für denselben Sensor immer gleich.
//!
//! # Nebenläufigkeit
//! Zustandsloser Unit-Struct: `Send + Sync`.
//!
//! # Fehler
//! Keine.
//!
//! # Examples
//! ```rust
//! use harw_authority::NetworkScope;
//! use harw_dod_rules::rule::{Rule, RuleContext};
//! use harw_dod_rules::rules::SensorDegradedRule;
//! use harw_dod_rules::FindingKind;
//! use harw_dod_signals::{EventKind, SecurityEvent, Severity};
//! use harw_types::SensorId;
//!
//! let events = vec![SecurityEvent {
//!     sensor: SensorId::from_str("sentinel"),
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     actor: None,
//!     kind: EventKind::SensorDegraded { sensor: SensorId::from_str("listener-0") },
//! }];
//! let scope = NetworkScope::empty();
//! let ctx = RuleContext {
//!     now: jiff::Timestamp::UNIX_EPOCH,
//!     samples: &[],
//!     events: &events,
//!     baselines: &[],
//!     network_scope: &scope,
//!     listener_history: None,
//!     rule_config: None,
//! };
//! let findings = SensorDegradedRule.evaluate(&ctx);
//! assert!(findings
//!     .iter()
//!     .all(|f| f.severity() == Severity::Medium && f.kind() == FindingKind::Anomaly));
//! assert_eq!(findings.len(), 1);
//! ```

use harw_dod_signals::{EventKind, Hardness, Severity};
use harw_untrusted_text::escape_line;

use crate::finding::{Finding, FindingKind, Raw};
use crate::rule::{Rule, RuleContext};

/// Höchstzahl Zeichen einer Sensor-Kennung in der Zusammenfassung (nach dem
/// Escapen, Kürzungsmarke eingerechnet).
const SENSOR_ID_MAX_CHARS: usize = 64;

/// Meldet jeden beeinträchtigten Sensor.
///
/// # Description
/// Siehe Moduldoku: `Anomaly`, `Medium`, `Observed`, Ereigniszeit.
///
/// # Errors
/// Keine eigenen Fehler; siehe [`crate::rule`]-Moduldoku.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensorDegradedRule;

impl Rule for SensorDegradedRule {
    /// # Returns
    /// `"sensor-degraded"`.
    fn id(&self) -> &'static str {
        "sensor-degraded"
    }

    /// # Arguments
    /// - `ctx` (`&RuleContext<'_>`): siehe [`crate::rule::RuleContext`].
    ///
    /// # Returns
    /// Je `SensorDegraded`-Ereignis einen Befund, gestempelt mit dessen
    /// `observed_at`.
    fn evaluate(&self, ctx: &RuleContext<'_>) -> Vec<Finding<Raw>> {
        ctx.events
            .iter()
            .filter_map(|event| {
                let EventKind::SensorDegraded { sensor } = &event.kind else {
                    return None;
                };
                Some(Finding::raw(
                    self.id(),
                    FindingKind::Anomaly,
                    Severity::Medium,
                    Hardness::Observed,
                    format!(
                        "sensor {} reports itself degraded",
                        escape_line(sensor.as_str(), SENSOR_ID_MAX_CHARS)
                    ),
                    // Ereigniszeit, nicht `ctx.now`.
                    event.observed_at,
                ))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx, missing};
    use harw_authority::NetworkScope;
    use harw_dod_signals::SecurityEvent;
    use harw_types::SensorId;
    use jiff::Timestamp;

    fn degraded(affected: &str, observed_at: Timestamp) -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("sentinel"),
            observed_at,
            actor: None,
            kind: EventKind::SensorDegraded {
                sensor: SensorId::from_str(affected),
            },
        }
    }

    fn ctx_with<'a>(events: &'a [SecurityEvent], scope: &'a NetworkScope) -> RuleContext<'a> {
        RuleContext {
            now: Timestamp::UNIX_EPOCH,
            samples: &[],
            events,
            baselines: &[],
            network_scope: scope,
            listener_history: None,
            rule_config: None,
        }
    }

    #[test]
    fn test_degraded_sensor_yields_medium_observed_anomaly() -> TestResult {
        let scope = NetworkScope::empty();
        let events = vec![degraded("listener-0", Timestamp::UNIX_EPOCH)];
        let findings = SensorDegradedRule.evaluate(&ctx_with(&events, &scope));

        assert_eq!(findings.len(), 1);
        let finding = findings.first().ok_or_else(missing("Befund"))?;
        assert_eq!(finding.rule_id(), "sensor-degraded");
        assert_eq!(finding.kind(), FindingKind::Anomaly);
        assert_eq!(finding.severity(), Severity::Medium);
        assert_eq!(finding.hardness(), Hardness::Observed);
        assert_eq!(
            finding.summary(),
            "sensor listener-0 reports itself degraded"
        );
        Ok(())
    }

    #[test]
    fn test_each_degraded_event_yields_one_finding() {
        let scope = NetworkScope::empty();
        let events = vec![
            degraded("listener-0", Timestamp::UNIX_EPOCH),
            degraded("probe-fs-0", Timestamp::UNIX_EPOCH),
        ];
        assert_eq!(
            SensorDegradedRule
                .evaluate(&ctx_with(&events, &scope))
                .len(),
            2
        );
    }

    #[test]
    fn test_finding_is_stamped_with_the_event_time_not_ctx_now() -> TestResult {
        let event_time: Timestamp = "2026-11-08T10:00:00Z"
            .parse()
            .map_err(ctx("Ereigniszeit"))?;
        let scope = NetworkScope::empty();
        let events = vec![degraded("cpu-0", event_time)];
        let mut rule_ctx = ctx_with(&events, &scope);
        rule_ctx.now = "2026-11-08T10:05:00Z"
            .parse()
            .map_err(ctx("Auswertungszeit"))?;

        let stamps: Vec<Timestamp> = SensorDegradedRule
            .evaluate(&rule_ctx)
            .iter()
            .map(Finding::observed_at)
            .collect();
        assert_eq!(stamps, vec![event_time]);
        Ok(())
    }

    #[test]
    fn test_sensor_id_is_escaped_in_the_summary() -> TestResult {
        let scope = NetworkScope::empty();
        let events = vec![degraded("probe\u{202e}\n0", Timestamp::UNIX_EPOCH)];
        let findings = SensorDegradedRule.evaluate(&ctx_with(&events, &scope));
        let finding = findings.first().ok_or_else(missing("Befund"))?;
        assert_eq!(
            finding.summary(),
            "sensor probe\\u{202e}\\u{000a}0 reports itself degraded"
        );
        Ok(())
    }

    #[test]
    fn test_other_event_kinds_are_ignored() {
        let scope = NetworkScope::empty();
        let events = vec![SecurityEvent {
            sensor: SensorId::from_str("listener-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::ListenerOpened { port: 22 },
        }];
        assert!(
            SensorDegradedRule
                .evaluate(&ctx_with(&events, &scope))
                .is_empty()
        );
    }

    #[test]
    fn test_evaluate_is_deterministic_for_identical_context() {
        let scope = NetworkScope::empty();
        let events = vec![degraded("listener-0", Timestamp::UNIX_EPOCH)];
        let ctx = ctx_with(&events, &scope);
        assert_eq!(
            SensorDegradedRule.evaluate(&ctx),
            SensorDegradedRule.evaluate(&ctx)
        );
    }
}
