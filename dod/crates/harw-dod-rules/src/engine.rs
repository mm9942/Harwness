//! Regelmaschine: führt alle Regeln parallel über einem gemeinsamen
//! [`RuleContext`] aus (Knoten AW4-04 p2.1-p2.4).
//!
//! # Verantwortungsbereich
//! Dieses Modul bindet [`crate::rule::Rule`]‑Implementierungen aus
//! [`crate::rules`] ein und ruft sie nebenläufig auf. Es entscheidet
//! **nicht**, was ein Befund bedeutet.
//!
//! # Zwei Einstiege: roh und zertifiziert
//! - [`run_rules`] liefert die rohen `Finding<Raw>` aller Regeln —
//!   deterministisch, ohne Identität. Geeignet zum Zählen, Filtern und für
//!   Tests, die zwei Läufe vergleichen.
//! - [`run_rules_checked`] ist die **öffentliche Prägestelle** für
//!   `Finding<RuleChecked>` aus Regelauswertungen: sie ruft [`run_rules`]
//!   auf und überführt jeden Befund per `Finding::check` (dieser Crate
//!   vorbehalten, `pub(crate)`) mit frischer [`FindingId`] in einen
//!   zertifizierten Befund. Darüber — und über
//!   [`crate::advisory::correlate_advisories`] als zweite Prägestelle —
//!   bekommt ein Aufrufer außerhalb dieser Crate (z. B. `harw-sentinel`,
//!   `harw-dod-escalate`) überhaupt einen `Finding<RuleChecked>` in die Hand,
//!   den er per [`crate::finding::Finding::record`] einfrieren und später per
//!   [`crate::finding::triage_record`] triagieren lassen kann.
//!
//! # Warum die Identitätsvergabe hier liegt und nicht in `Rule::evaluate`
//! [`FindingId::new`] erzeugt eine zufällige UUID. Läge diese Vergabe in
//! einer Regel, wäre die Regel nicht mehr deterministisch prüfbar (siehe
//! Reinheitsauflage in der [`crate::rule`]-Moduldoku). Sie liegt deshalb
//! bewusst *hinter* jeder Regelauswertung, in [`run_rules_checked`] — der
//! einzigen Stelle dieser Crate, die für Regelbefunde einen Zufallswert
//! erzeugt.
//!
//! # Parallelität
//! Die Implementierung benutzt `rayon::join_all`, damit jede Regel als
//! unabhängige Berechnung läuft. Alle Regeln sind reine Funktionen auf dem
//! unveränderlichen [`RuleContext`]; es gibt keinen gemeinsamen veränderlichen
//! Zustand und keine Sperren.
//!
//! # Fehler
//! Keine. Regeln liefern entweder Befunde oder nicht; ein Lauf kann nicht
//! fehlschlagen.
//!
//! # Examples
//! ```rust
//! use harw_authority::NetworkScope;
//! use harw_dod_rules::engine::run_rules;
//! use harw_dod_rules::rule::RuleContext;
//!
//! let scope = NetworkScope::empty();
//! let ctx = RuleContext {
//!     now: jiff::Timestamp::UNIX_EPOCH,
//!     samples: &[],
//!     events: &[],
//!     baselines: &[],
//!     network_scope: &scope,
//! };
//! assert!(run_rules(&ctx).is_empty());
//! ```

use rayon::prelude::*;

use harw_types::FindingId;

use crate::finding::{Finding, Raw, RuleChecked};
use crate::rule::RuleContext;
use crate::rules::ALL_RULES;

/// Führt alle bekannten Regeln parallel aus und liefert deren Befunde.
///
/// # Arguments
/// - `ctx` (`&RuleContext<'_>`): der gemeinsame Eingabekontext für alle Regeln.
///
/// # Returns
/// Eine flache Liste aller ausgelösten Befunde.
///
/// # Panics
/// Wenn eine Regel panikt, wird der Panic-Thread geworfen; `run_rules`
/// versteckt keinen Fehler.
pub fn run_rules(ctx: &RuleContext<'_>) -> Vec<Finding<Raw>> {
    ALL_RULES
        .par_iter()
        .flat_map(|rule| rule.evaluate(ctx))
        .collect()
}

/// Führt alle bekannten Regeln aus und zertifiziert jeden Befund.
///
/// # Description
/// Ruft [`run_rules`] auf und überführt jeden `Finding<Raw>` per
/// `Finding::check` mit einer frischen, zufälligen [`FindingId`] in einen
/// `Finding<RuleChecked>`. Siehe Moduldoku, warum die Identitätsvergabe hier
/// und nicht in einer Regel liegt.
///
/// # Arguments
/// - `ctx` (`&RuleContext<'_>`): der gemeinsame Eingabekontext für alle Regeln.
///
/// # Returns
/// Alle ausgelösten Befunde, zertifiziert. Leer, wenn keine Regel etwas
/// ausgelöst hat.
///
/// # Errors
/// Keine — die Identitätsvergabe ist total.
///
/// # Panics
/// Wie [`run_rules`].
///
/// # Examples
/// ```rust
/// use harw_authority::NetworkScope;
/// use harw_dod_rules::rule::RuleContext;
/// use harw_dod_rules::run_rules_checked;
/// use harw_dod_signals::{DriftSeverity, EventKind, SecurityEvent};
/// use harw_types::SensorId;
///
/// let scope = NetworkScope::empty();
/// let events = vec![SecurityEvent {
///     sensor: SensorId::from_str("workspace-drift"),
///     observed_at: jiff::Timestamp::UNIX_EPOCH,
///     actor: None,
///     kind: EventKind::StructureDrift {
///         severity: DriftSeverity::Medium,
///         detail: "neues Workspace-Member: harw-new-crate".to_owned(),
///     },
/// }];
/// let ctx = RuleContext {
///     now: jiff::Timestamp::UNIX_EPOCH,
///     samples: &[],
///     events: &events,
///     baselines: &[],
///     network_scope: &scope,
/// };
/// let checked = run_rules_checked(&ctx);
/// assert_eq!(checked.len(), 1);
/// assert!(!checked[0].id().as_str().is_empty());
/// ```
#[must_use]
pub fn run_rules_checked(ctx: &RuleContext<'_>) -> Vec<Finding<RuleChecked>> {
    run_rules(ctx)
        .into_iter()
        .map(|raw| raw.check(FindingId::new()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::Rule;
    use crate::rules::EgressFlowRule;
    use harw_authority::NetworkScope;
    use harw_dod_signals::{EventKind, SecurityEvent};
    use harw_types::SensorId;
    use jiff::Timestamp;

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
    fn test_run_rules_on_empty_context_is_empty() {
        let scope = NetworkScope::empty();
        let ctx = ctx_with(&[], &scope);
        assert!(run_rules(&ctx).is_empty());
    }

    #[test]
    fn test_egress_flow_rule_is_included() {
        // Keine Exhaustivität, nur ein Rauchtest, dass die Regelliste nicht
        // leer ist und die Egress-Regel enthält.
        assert_eq!(ALL_RULES.len(), 3);
        assert!(
            ALL_RULES
                .iter()
                .any(|rule| rule.id() == EgressFlowRule.id())
        );
    }

    #[test]
    fn test_run_rules_collects_findings() {
        let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
        let events = vec![SecurityEvent {
            sensor: SensorId::from_str("net"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::EgressFlow {
                destination: "evil.example".to_owned(),
                port: 443,
            },
        }];
        let ctx = ctx_with(&events, &scope);
        let findings = run_rules(&ctx);
        assert!(!findings.is_empty());
        assert!(findings.iter().any(|f| f.summary.contains("egress")));
    }

    #[test]
    fn test_run_rules_checked_certifies_every_raw_finding_with_a_distinct_id() {
        let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
        let events = vec![
            SecurityEvent {
                sensor: SensorId::from_str("net"),
                observed_at: Timestamp::UNIX_EPOCH,
                actor: None,
                kind: EventKind::EgressFlow {
                    destination: "evil.example".to_owned(),
                    port: 443,
                },
            },
            SecurityEvent {
                sensor: SensorId::from_str("net"),
                observed_at: Timestamp::UNIX_EPOCH,
                actor: None,
                kind: EventKind::EgressFlow {
                    destination: "other.example".to_owned(),
                    port: 443,
                },
            },
        ];
        let ctx = ctx_with(&events, &scope);

        let raw_count = run_rules(&ctx).len();
        let checked = run_rules_checked(&ctx);

        assert_eq!(checked.len(), raw_count);
        assert!(checked.len() >= 2);
        let ids: std::collections::BTreeSet<_> =
            checked.iter().map(|f| f.id().as_str().to_owned()).collect();
        assert_eq!(ids.len(), checked.len(), "jede Identität ist einmalig");
    }
}
