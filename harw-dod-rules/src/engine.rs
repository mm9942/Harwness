//! Ausführungsmotor: führt Regeln aus und zertifiziert ihre rohen Befunde.
//!
//! # Verantwortungsbereich
//! [`run_rules`] ist der einzige Ort außerhalb von [`crate::finding`], der
//! den `pub(crate)`-Übergang `Finding::check` aufruft. Genau darüber bekommt
//! ein Aufrufer außerhalb dieser Crate — namentlich `harw-dod-escalate`
//! (Knoten AW5-03) — überhaupt einen `Finding<RuleChecked>` in die Hand:
//! nicht durch eigene Konstruktion (unmöglich, siehe
//! [`crate::finding`]-Moduldoku), sondern indem es das Ergebnis dieser
//! Funktion entgegennimmt.
//!
//! # Warum die Identitätsvergabe hier liegt und nicht in `Rule::evaluate`
//! [`harw_types::FindingId::new`] erzeugt eine zufällige UUID. Läge diese
//! Vergabe in einer [`crate::rule::Rule::evaluate`]-Implementierung, wäre die
//! Regel nicht mehr deterministisch prüfbar: zwei Aufrufe mit identischem
//! [`crate::rule::RuleContext`] lieferten unterschiedliche Ergebnisse, obwohl
//! sich an der Beobachtung nichts geändert hat — genau der Fall, den die
//! Reinheitsauflage in [`crate::rule`]-Moduldoku ausschließt. Diese Funktion
//! liegt deshalb bewusst *hinter* jeder Regelauswertung, in der reinen
//! Verdrahtung: `Finding<Raw>` trägt keine Identität (`S::Identity = ()`,
//! siehe [`crate::finding`]-Moduldoku), also bleibt die Regel selbst
//! vollständig deterministisch, während der Motor jedem zertifizierten
//! Befund erst danach eine stabile, einmalige Identität zuweist.
//!
//! # Nebenläufigkeit
//! [`run_rules`] liest `rules` und `ctx` nur (`&`), schreibt nirgends
//! geteilten Zustand und ist damit sicher aus mehreren Threads mit
//! unterschiedlichen Argumenten aufrufbar. Jede einzelne
//! [`crate::rule::Rule::evaluate`]-Implementierung muss selbst
//! `Send + Sync` sein (Supertrait-Bound auf [`crate::rule::Rule`]).
//!
//! # Fehler
//! Keine. Regeln liefern kein `Result`; die Identitätsvergabe ist total.
//!
//! # Examples
//! ```rust
//! use harw_dod_rules::rule::{Rule, RuleContext};
//! use harw_dod_rules::rules::StructureDriftRule;
//! use harw_dod_rules::run_rules;
//! use harw_sandbox::NetworkScope;
//!
//! let scope = NetworkScope::empty();
//! let ctx = RuleContext {
//!     now: jiff::Timestamp::UNIX_EPOCH,
//!     samples: &[],
//!     events: &[],
//!     baselines: &[],
//!     network_scope: &scope,
//! };
//! let rule: &dyn Rule = &StructureDriftRule;
//! assert!(run_rules(&[rule], &ctx).is_empty());
//! ```

use harw_types::FindingId;

use crate::finding::{Finding, RuleChecked};
use crate::rule::{Rule, RuleContext};

/// Führt alle übergebenen Regeln gegen einen Kontext aus und zertifiziert die
/// Ergebnisse.
///
/// # Description
/// Ruft [`crate::rule::Rule::evaluate`] auf jeder Regel in `rules` auf und
/// überführt jeden zurückgelieferten `Finding<Raw>` per
/// `Finding::check` (dieser Crate vorbehalten) in einen `Finding<RuleChecked>`
/// mit frischer, zufälliger [`FindingId`]. Siehe Moduldoku für die
/// Begründung, warum die Identitätsvergabe hier und nicht in einer Regel
/// selbst liegt.
///
/// # Arguments
/// - `rules` (`&[&dyn Rule]`): die auszuführenden Regeln, in Aufrufreihenfolge.
/// - `ctx` (`&RuleContext<'_>`): der gemeinsame Eingabekontext für alle Regeln.
///
/// # Returns
/// Alle ausgelösten Befunde, zertifiziert, in der Reihenfolge: erst alle
/// Befunde der ersten Regel, dann alle der zweiten, und so weiter. Leer, wenn
/// keine Regel etwas ausgelöst hat.
///
/// # Errors
/// Keine.
///
/// # Concurrency
/// Siehe Moduldoku.
///
/// # Examples
/// Siehe Moduldoku.
#[must_use]
pub fn run_rules(rules: &[&dyn Rule], ctx: &RuleContext<'_>) -> Vec<Finding<RuleChecked>> {
    rules
        .iter()
        .flat_map(|rule| rule.evaluate(ctx))
        .map(|raw| raw.check(FindingId::new()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::EgressFlowRule;
    use harw_dod_signals::{EventKind, SecurityEvent};
    use harw_sandbox::NetworkScope;
    use harw_types::SensorId;
    use jiff::Timestamp;

    fn triggering_event() -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("net-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::EgressFlow {
                destination: "evil.example.com".to_owned(),
                port: 443,
            },
        }
    }

    #[test]
    fn test_run_rules_certifies_every_raw_finding_with_an_id() {
        let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
        let events = vec![triggering_event()];
        let ctx = RuleContext {
            now: Timestamp::UNIX_EPOCH,
            samples: &[],
            events: &events,
            baselines: &[],
            network_scope: &scope,
        };
        let rule: &dyn Rule = &EgressFlowRule;
        let checked = run_rules(&[rule], &ctx);

        assert_eq!(checked.len(), 1);
        assert!(!checked[0].id().as_str().is_empty());
    }

    #[test]
    fn test_run_rules_with_no_rules_yields_nothing() {
        let scope = NetworkScope::empty();
        let ctx = RuleContext {
            now: Timestamp::UNIX_EPOCH,
            samples: &[],
            events: &[],
            baselines: &[],
            network_scope: &scope,
        };
        assert!(run_rules(&[], &ctx).is_empty());
    }
}
