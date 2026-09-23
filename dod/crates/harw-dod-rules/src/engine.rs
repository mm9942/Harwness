//! Regelmaschine: führt alle Regeln parallel über einem gemeinsamen
//! [`RuleContext`] aus (Knoten AW4-04 p2.1-p2.4).
//!
//! # Verantwortungsbereich
//! Dieses Modul bindet [`Rule`]‑Implementierungen aus [`crate::rules`] ein
//! und ruft sie nebenläufig auf. Es entscheidet **nicht**, was ein Befund
//! bedeutet, und zertifiziert ihn nicht — das passiert später in
//! [`crate::certification`].
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

use crate::finding::{Finding, Raw};
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
}
