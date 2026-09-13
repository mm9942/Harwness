//! `EgressFlowRule`: ein beobachteter ausgehender Fluss außerhalb des
//! erlaubten Netzbereichs.
//!
//! # Verantwortungsbereich
//! Prüft jedes `EventKind::EgressFlow`-Ereignis in
//! [`crate::rule::RuleContext::events`] gegen
//! [`crate::rule::RuleContext::network_scope`], über
//! `harw_sandbox::NetworkScope::allows`/`allows_addr` — nie eine eigene
//! Bereichsprüfung, wie im Arbeitsauftrag verlangt.
//!
//! # Warum `allows` **und** `allows_addr`
//! `EventKind::EgressFlow::destination` ist ein freier String: mal ein
//! Hostname (`"docs.rs"`), mal eine literale Adresse
//! (`"203.0.113.7"`). `NetworkScope::allows` prüft Hostnamen/DNS-Suffixe,
//! `NetworkScope::allows_addr` prüft `Cidr`-Bereiche gegen eine geparste
//! `IpAddr` — die beiden Prüfungen sind laut `harw_sandbox`-Moduldoku
//! disjunkt (`Host`/`DnsSuffix`-Einträge nehmen nie an `allows_addr` teil,
//! `Cidr`-Einträge nie an `allows`). [`destination_allowed`] wählt deshalb
//! anhand von `destination.parse::<IpAddr>()`, welche der beiden Prüfungen
//! zuständig ist.
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
//! use harw_dod_rules::rules::EgressFlowRule;
//! use harw_dod_signals::{EventKind, SecurityEvent};
//! use harw_sandbox::NetworkScope;
//! use harw_types::SensorId;
//!
//! let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
//! let events = vec![SecurityEvent {
//!     sensor: SensorId::from_str("net-0"),
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     actor: None,
//!     kind: EventKind::EgressFlow {
//!         destination: "static.docs.rs".to_owned(),
//!         port: 443,
//!     },
//! }];
//! let ctx = RuleContext {
//!     now: jiff::Timestamp::UNIX_EPOCH,
//!     samples: &[],
//!     events: &events,
//!     baselines: &[],
//!     network_scope: &scope,
//! };
//! assert!(EgressFlowRule.evaluate(&ctx).is_empty(), "docs.rs ist erlaubt");
//! ```

use std::net::IpAddr;

use harw_dod_signals::{EventKind, Hardness, Severity};
use harw_sandbox::NetworkScope;

use crate::finding::{Finding, FindingKind, Raw};
use crate::rule::{Rule, RuleContext};

/// Meldet einen ausgehenden Fluss außerhalb des erlaubten Netzbereichs.
///
/// # Description
/// Siehe Moduldoku. Jeder Treffer erzeugt genau einen `Finding<Raw>` mit
/// `FindingKind::RuleTriggered` — eine Scope-Verletzung ist keine bloße
/// Anomalie, sie ist eine harte Regel ohne Baseline-Bezug.
///
/// # Errors
/// Keine eigenen Fehler; siehe [`crate::rule`]-Moduldoku.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressFlowRule;

impl Rule for EgressFlowRule {
    /// # Returns
    /// `"egress-flow"`.
    fn id(&self) -> &'static str {
        "egress-flow"
    }

    /// # Arguments
    /// - `ctx` (`&RuleContext<'_>`): siehe [`crate::rule::RuleContext`].
    ///
    /// # Returns
    /// Einen `Finding<Raw>` je `EgressFlow`-Ereignis, dessen `destination`
    /// nicht in `ctx.network_scope` erlaubt ist. Leer, wenn jeder
    /// beobachtete Fluss erlaubt war.
    fn evaluate(&self, ctx: &RuleContext<'_>) -> Vec<Finding<Raw>> {
        ctx.events
            .iter()
            .filter_map(|event| {
                let EventKind::EgressFlow { destination, port } = &event.kind else {
                    return None;
                };
                if destination_allowed(ctx.network_scope, destination) {
                    return None;
                }
                Some(Finding::raw(
                    self.id(),
                    FindingKind::RuleTriggered,
                    Severity::High,
                    Hardness::Observed,
                    format!(
                        "egress flow to {destination}:{port} is outside the allowed network scope"
                    ),
                    ctx.now,
                ))
            })
            .collect()
    }
}

// Wählt die zu `destination` passende `NetworkScope`-Prüfung — siehe
// Moduldoku, warum beide Methoden gebraucht werden.
fn destination_allowed(scope: &NetworkScope, destination: &str) -> bool {
    match destination.parse::<IpAddr>() {
        Ok(addr) => scope.allows_addr(addr),
        Err(_) => scope.allows(destination),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_dod_signals::SecurityEvent;
    use harw_types::SensorId;
    use jiff::Timestamp;

    fn event(destination: &str, port: u16) -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("net-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::EgressFlow {
                destination: destination.to_owned(),
                port,
            },
        }
    }

    fn ctx_with<'a>(
        events: &'a [SecurityEvent],
        scope: &'a NetworkScope,
    ) -> RuleContext<'a> {
        RuleContext {
            now: Timestamp::UNIX_EPOCH,
            samples: &[],
            events,
            baselines: &[],
            network_scope: scope,
        }
    }

    #[test]
    fn test_destination_outside_host_scope_triggers_a_finding() {
        let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
        let events = vec![event("evil.example.com", 443)];
        let ctx = ctx_with(&events, &scope);

        let findings = EgressFlowRule.evaluate(&ctx);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::RuleTriggered);
        assert_eq!(findings[0].severity, Severity::High);
        assert!(findings[0].summary.contains("evil.example.com:443"));
    }

    #[test]
    fn test_destination_inside_host_scope_does_not_trigger() {
        let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
        let events = vec![event("static.docs.rs", 443)];
        let ctx = ctx_with(&events, &scope);

        assert!(EgressFlowRule.evaluate(&ctx).is_empty());
    }

    #[test]
    fn test_address_inside_cidr_scope_does_not_trigger() {
        // `NetworkScope` bietet keinen anderen öffentlichen Konstruktor für
        // ein `Cidr`-Ziel als seine `Deserialize`-Implementierung (siehe
        // `harw_sandbox`-Moduldoku zu `EgressTarget`).
        let scope: NetworkScope =
            serde_json::from_str(r#"{"allow_hosts":["203.0.113.0/24"]}"#)
                .expect("NetworkScope liest ein Cidr-Ziel");
        let events = vec![event("203.0.113.7", 443)];
        let ctx = ctx_with(&events, &scope);

        assert!(EgressFlowRule.evaluate(&ctx).is_empty());
    }

    #[test]
    fn test_address_outside_cidr_scope_triggers_a_finding() {
        let scope: NetworkScope =
            serde_json::from_str(r#"{"allow_hosts":["203.0.113.0/24"]}"#)
                .expect("NetworkScope liest ein Cidr-Ziel");
        let events = vec![event("198.51.100.5", 8080)];
        let ctx = ctx_with(&events, &scope);

        let findings = EgressFlowRule.evaluate(&ctx);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].summary.contains("198.51.100.5:8080"));
    }

    #[test]
    fn test_non_egress_events_are_ignored() {
        let scope = NetworkScope::empty();
        let events = vec![SecurityEvent {
            sensor: SensorId::from_str("fsmon-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::ListenerOpened { port: 8080 },
        }];
        let ctx = ctx_with(&events, &scope);

        assert!(EgressFlowRule.evaluate(&ctx).is_empty());
    }

    #[test]
    fn test_evaluate_is_deterministic_for_identical_context() {
        let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
        let events = vec![event("evil.example.com", 443)];
        let ctx = ctx_with(&events, &scope);

        assert_eq!(EgressFlowRule.evaluate(&ctx), EgressFlowRule.evaluate(&ctx));
    }
}
