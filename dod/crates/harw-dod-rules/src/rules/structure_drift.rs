//! `StructureDriftRule`: meldet, was `harw-dod-workspace` als
//! `EventKind::StructureDrift` liefert, als Befund mit passender Schwere.
//!
//! # Die Kopplung, die hier aufgelöst wurde
//! In der ersten Fassung trug `EventKind::StructureDrift` **nur** einen
//! Freitext. Der Sensor hatte die Schwere strukturiert
//! ([`harw_dod_workspace::VersionSeverity`] mit ihren SemVer-Feinheiten),
//! flachte sie in seiner privaten `describe`-Funktion zu Prosa ab, und diese
//! Regel gewann sie über feste Textpräfixe (`"Versionssprung ("`,
//! `"neue Abhängigkeit:"`, …) wieder heraus.
//!
//! **Das war eine Sicherheitsregel, die ihre Einstufung aus Textmustern
//! rekonstruierte.** Eine geänderte Formulierung im Sensor hätte sie still
//! danebengreifen lassen: kein Compilefehler, kein fehlschlagender Test, nur
//! ein Befund, der ab dann falsch eingestuft ist. Der erbauende Knoten hat
//! die Kopplung offengelegt statt sie hinzunehmen — und sie wurde daraufhin
//! an der Ursache behoben.
//!
//! **Heute trägt `EventKind::StructureDrift` ein
//! [`harw_dod_signals::DriftSeverity`]-Feld.** Der Sensor bildet seine feinere
//! Einstufung an genau einer Stelle darauf ab; diese Regel liest das Feld.
//! Der Freitext ist Beschreibung für Menschen, **keine Datenquelle** — und
//! [`test_severity_does_not_depend_on_the_free_text`] hält genau das fest.
//!
//! # Nebenläufigkeit
//! Zustandsloser Unit-Struct: `Send + Sync`.
//!
//! # Fehler
//! Keine.
//!
//! # Examples
//! ```rust
//! use harw_dod_rules::rule::{Rule, RuleContext};
//! use harw_dod_rules::rules::StructureDriftRule;
//! use harw_dod_signals::{DriftSeverity, EventKind, SecurityEvent};
//! use harw_sandbox::NetworkScope;
//! use harw_types::SensorId;
//!
//! let scope = NetworkScope::empty();
//! let events = vec![SecurityEvent {
//!     sensor: SensorId::from_str("workspace-drift"),
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     actor: None,
//!     kind: EventKind::StructureDrift {
//!         severity: DriftSeverity::Medium,
//!         detail: "neues Workspace-Member: harw-new-crate".to_owned(),
//!     },
//! }];
//! let ctx = RuleContext {
//!     now: jiff::Timestamp::UNIX_EPOCH,
//!     samples: &[],
//!     events: &events,
//!     baselines: &[],
//!     network_scope: &scope,
//! };
//! assert_eq!(StructureDriftRule.evaluate(&ctx).len(), 1);
//! ```

use harw_dod_signals::{DriftSeverity, EventKind, Hardness, Severity};

use crate::finding::{Finding, FindingKind, Raw};
use crate::rule::{Rule, RuleContext};

/// Meldet jede beobachtete Strukturabweichung als Befund mit passender
/// Schwere.
///
/// # Description
/// Siehe Moduldoku, insbesondere den Abschnitt zur Freitext-Kopplung.
///
/// # Errors
/// Keine eigenen Fehler; siehe [`crate::rule`]-Moduldoku.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructureDriftRule;

impl Rule for StructureDriftRule {
    /// # Returns
    /// `"structure-drift"`.
    fn id(&self) -> &'static str {
        "structure-drift"
    }

    /// # Arguments
    /// - `ctx` (`&RuleContext<'_>`): siehe [`crate::rule::RuleContext`].
    ///
    /// # Returns
    /// Einen `Finding<Raw>` je `StructureDrift`-Ereignis, dessen Schwere über
    /// [`severity_for_drift`] aus dem strukturierten Schweregrad abgebildet wird.
    fn evaluate(&self, ctx: &RuleContext<'_>) -> Vec<Finding<Raw>> {
        ctx.events
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
            .collect()
    }
}

/// Bildet die grobe Schwere des Signalstroms auf die Befundschwere ab.
///
/// # Description
/// Vorher stand hier eine Rekonstruktion aus den festen Textpräfixen von
/// `harw_dod_workspace::sensor::describe` — eine geänderte Formulierung dort
/// hätte diese Regel still falsch einstufen lassen, ohne dass ein Test
/// angeschlagen hätte. `EventKind::StructureDrift` trägt die Schwere seit
/// dieser Korrektur **strukturiert**; der Freitext ist Beschreibung, nicht
/// Datenquelle.
///
/// # Arguments
/// - `severity` (`DriftSeverity`): die vom Sensor vergebene Einstufung.
///
/// # Returns
/// Die entsprechende [`Severity`] des Befunds.
///
/// `DriftSeverity::Unknown` wird zu [`Severity::Medium`], **nicht** zu
/// `Info`: eine unbekannte Schwere ist keine geringe. Ein Datensatz aus der
/// Zeit vor dem strukturierten Feld verdient einen Blick, keinen Freispruch.
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
    use harw_dod_signals::SecurityEvent;
    use harw_sandbox::NetworkScope;
    use harw_types::SensorId;
    use jiff::Timestamp;

    /// Baut ein Drift-Ereignis mit **strukturierter** Schwere.
    ///
    /// Der Freitext ist hier bewusst nichtssagend: die Regel darf ihn nicht
    /// mehr auswerten, und ein Test, der plausiblen Text mitgibt, würde eine
    /// zurückkehrende Textabhängigkeit nicht bemerken.
    fn event(severity: DriftSeverity) -> SecurityEvent {
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

    fn ctx_with<'a>(events: &'a [SecurityEvent], scope: &'a NetworkScope) -> RuleContext<'a> {
        RuleContext {
            now: Timestamp::UNIX_EPOCH,
            samples: &[],
            events,
            baselines: &[],
            network_scope: scope,
        }
    }

    #[test]
    fn test_high_drift_severity_becomes_high_finding_severity() {
        let scope = NetworkScope::empty();
        let events = vec![event(DriftSeverity::High)];
        let ctx = ctx_with(&events, &scope);

        let findings = StructureDriftRule.evaluate(&ctx);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::High);
        assert_eq!(findings[0].kind, FindingKind::RuleTriggered);
    }

    #[test]
    fn test_medium_and_low_drift_severities_map_through() {
        let scope = NetworkScope::empty();
        let events = vec![event(DriftSeverity::Medium), event(DriftSeverity::Low)];
        let ctx = ctx_with(&events, &scope);

        let findings = StructureDriftRule.evaluate(&ctx);

        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].severity, Severity::Medium);
        assert_eq!(findings[1].severity, Severity::Low);
    }

    #[test]
    fn test_unknown_drift_severity_is_not_downgraded_to_info() {
        // Ein Datensatz aus der Zeit vor dem strukturierten Feld. Wer eine
        // unbekannte Schwere wie eine geringe behandelt, spricht einen Befund
        // frei, über den nie jemand entschieden hat.
        let scope = NetworkScope::empty();
        let events = vec![event(DriftSeverity::Unknown)];
        let ctx = ctx_with(&events, &scope);

        let findings = StructureDriftRule.evaluate(&ctx);

        assert_eq!(findings[0].severity, Severity::Medium);
    }

    #[test]
    fn test_severity_does_not_depend_on_the_free_text() {
        // Der eigentliche Regressionstest dieser Korrektur: derselbe
        // Schweregrad, völlig verschiedener Text — dieselbe Einstufung.
        // Vorher hing die Schwere an festen Textpräfixen, und eine geänderte
        // Formulierung im Sensor hätte hier still danebengegriffen.
        let scope = NetworkScope::empty();
        let mut a = event(DriftSeverity::High);
        let mut b = event(DriftSeverity::High);
        if let EventKind::StructureDrift { detail, .. } = &mut a.kind {
            *detail = "Versionssprung (Patch) bei serde".to_owned();
        }
        if let EventKind::StructureDrift { detail, .. } = &mut b.kind {
            *detail = "Abhängigkeit entfernt: old-crate".to_owned();
        }
        let events = vec![a, b];
        let ctx = ctx_with(&events, &scope);

        let findings = StructureDriftRule.evaluate(&ctx);

        assert_eq!(findings[0].severity, Severity::High);
        assert_eq!(
            findings[1].severity,
            Severity::High,
            "der Freitext darf die Einstufung nicht beeinflussen"
        );
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
        let ctx = ctx_with(&events, &scope);

        assert!(StructureDriftRule.evaluate(&ctx).is_empty());
    }

    #[test]
    fn test_evaluate_is_deterministic_for_identical_context() {
        let scope = NetworkScope::empty();
        let events = vec![event(DriftSeverity::Medium)];
        let ctx = ctx_with(&events, &scope);

        assert_eq!(
            StructureDriftRule.evaluate(&ctx),
            StructureDriftRule.evaluate(&ctx)
        );
    }
}
