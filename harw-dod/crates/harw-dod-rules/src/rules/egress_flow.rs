//! `EgressFlowRule`: ein beobachteter ausgehender Fluss außerhalb des
//! erlaubten Netzbereichs.
//!
//! # Verantwortungsbereich
//! Prüft jedes `EventKind::EgressFlow`-Ereignis in
//! [`crate::rule::RuleContext::events`] gegen
//! [`crate::rule::RuleContext::network_scope`], über
//! `harw_authority::NetworkScope::allows`/`allows_addr` — nie eine eigene
//! Bereichsprüfung, wie im Arbeitsauftrag verlangt.
//!
//! # Warum `allows` **und** `allows_addr`
//! `EventKind::EgressFlow::destination` ist ein freier String: mal ein
//! Hostname (`"docs.rs"`), mal eine literale Adresse
//! (`"203.0.113.7"`). `NetworkScope::allows` prüft Hostnamen/DNS-Suffixe,
//! `NetworkScope::allows_addr` prüft `Cidr`-Bereiche gegen eine geparste
//! `IpAddr` — die beiden Prüfungen sind laut `harw_authority`-Moduldoku
//! disjunkt (`Host`/`DnsSuffix`-Einträge nehmen nie an `allows_addr` teil,
//! `Cidr`-Einträge nie an `allows`). [`destination_allowed`] wählt deshalb
//! anhand von `destination.parse::<IpAddr>()`, welche der beiden Prüfungen
//! zuständig ist.
//!
//! Eine IPv4-gemappte IPv6-Adresse (`::ffff:a.b.c.d`, typisch für Flüsse aus
//! Dual-Stack-Sockets) gilt als erlaubt, wenn die Adresse selbst **oder**
//! ihre kanonische IPv4-Form (`IpAddr::to_canonical`) im Bereich liegt —
//! sonst träfe eine IPv4-`egress_allow_cidrs`-Freigabe (PL-100 WP0.3) solche
//! Flüsse nie.
//!
//! # Zeitstempel: Ereigniszeit, nicht Auswertungszeit (PL-100 WP0.2)
//! Jeder Befund trägt das `observed_at` **des auslösenden Ereignisses**, nie
//! [`crate::rule::RuleContext::now`]. Ein Fluss wurde einmal beobachtet; wird
//! derselbe Ring in mehreren Zyklen ausgewertet, entsteht so derselbe
//! Zeitstempel statt eines neuen je Zyklus — die Voraussetzung dafür, dass
//! nachgelagerte Deduplizierung (Sentinel-Attribution, `FindingThrottle`)
//! denselben Fluss als denselben Befund erkennt.
//!
//! # Loopback ist `Low`, nicht `High` (PL-100 WP0.3)
//! Ein Ziel im Loopback-Bereich (`127.0.0.0/8`, `::1`, auch als
//! IPv4-gemappte IPv6-Adresse `::ffff:127.x.y.z`) verlässt den Host nicht.
//! Liegt es außerhalb des erlaubten Bereichs, wird es weiterhin gemeldet —
//! eine lokale Verbindung kann ein Hinweis sein (lokaler Proxy, Tunnel) —,
//! aber mit [`Severity::Low`] statt [`Severity::High`]: sonst erzeugt jeder
//! lokale Dienst eine High-Flut. Nur **literale** Adressen gelten als
//! Loopback; ein Hostname wie `localhost` wird nicht aufgelöst (Regeln
//! führen kein I/O aus) und bleibt `High`.
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
//! use harw_dod_rules::rules::EgressFlowRule;
//! use harw_dod_signals::{EventKind, SecurityEvent};
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

use harw_authority::NetworkScope;
use harw_dod_signals::{EventKind, Hardness, Severity};

use crate::finding::{Finding, FindingKind, Raw};
use crate::rule::{Rule, RuleContext};

/// Meldet einen ausgehenden Fluss außerhalb des erlaubten Netzbereichs.
///
/// # Description
/// Siehe Moduldoku. Jeder Treffer erzeugt genau einen `Finding<Raw>` mit
/// `FindingKind::RuleTriggered` — eine Scope-Verletzung ist keine bloße
/// Anomalie, sie ist eine harte Regel ohne Baseline-Bezug. Der Befund trägt
/// die Ereigniszeit; ein Loopback-Ziel wird mit [`Severity::Low`] gemeldet,
/// jedes andere mit [`Severity::High`] (siehe Moduldoku).
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
    /// nicht in `ctx.network_scope` erlaubt ist, gestempelt mit dem
    /// `observed_at` des Ereignisses. Leer, wenn jeder beobachtete Fluss
    /// erlaubt war.
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
                    severity_for_destination(destination),
                    Hardness::Observed,
                    format!(
                        "egress flow to {destination}:{port} is outside the allowed network scope"
                    ),
                    // Ereigniszeit, nicht `ctx.now` — siehe Moduldoku.
                    event.observed_at,
                ))
            })
            .collect()
    }
}

// Schwere eines Flusses außerhalb des erlaubten Bereichs: `Low` für ein
// literales Loopback-Ziel, sonst `High` — siehe Moduldoku.
fn severity_for_destination(destination: &str) -> Severity {
    if is_loopback_literal(destination) {
        Severity::Low
    } else {
        Severity::High
    }
}

// Ob `destination` eine literale Loopback-Adresse ist (`127.0.0.0/8`, `::1`
// oder `::ffff:127.x.y.z`). Hostnamen werden nie aufgelöst.
fn is_loopback_literal(destination: &str) -> bool {
    match destination.parse::<IpAddr>() {
        Ok(IpAddr::V4(addr)) => addr.is_loopback(),
        Ok(IpAddr::V6(addr)) => {
            addr.is_loopback() || addr.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
        Err(_) => false,
    }
}

// Wählt die zu `destination` passende `NetworkScope`-Prüfung — siehe
// Moduldoku, warum beide Methoden gebraucht werden.
fn destination_allowed(scope: &NetworkScope, destination: &str) -> bool {
    match destination.parse::<IpAddr>() {
        Ok(addr) => scope.allows_addr(addr) || scope.allows_addr(addr.to_canonical()),
        Err(_) => scope.allows(destination),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;
    use harw_dod_signals::SecurityEvent;
    use harw_types::SensorId;
    use jiff::Timestamp;

    fn event(destination: &str, port: u16) -> SecurityEvent {
        event_at(destination, port, Timestamp::UNIX_EPOCH)
    }

    fn event_at(destination: &str, port: u16, observed_at: Timestamp) -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("net-0"),
            observed_at,
            actor: None,
            kind: EventKind::EgressFlow {
                destination: destination.to_owned(),
                port,
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
    fn test_address_inside_cidr_scope_does_not_trigger() -> TestResult {
        // `NetworkScope` bietet keinen anderen öffentlichen Konstruktor für
        // ein `Cidr`-Ziel als seine `Deserialize`-Implementierung (siehe
        // `harw_authority`-Moduldoku zu `EgressTarget`).
        let scope: NetworkScope = serde_json::from_str(r#"{"allow_hosts":["203.0.113.0/24"]}"#)
            .map_err(crate::test_support::ctx("NetworkScope liest ein Cidr-Ziel"))?;
        let events = vec![event("203.0.113.7", 443)];
        let ctx = ctx_with(&events, &scope);

        assert!(EgressFlowRule.evaluate(&ctx).is_empty());
        Ok(())
    }

    #[test]
    fn test_ipv4_mapped_address_inside_a_v4_cidr_does_not_trigger() -> TestResult {
        let scope: NetworkScope = serde_json::from_str(r#"{"allow_hosts":["203.0.113.0/24"]}"#)
            .map_err(crate::test_support::ctx("NetworkScope liest ein Cidr-Ziel"))?;
        let inside = vec![event("::ffff:203.0.113.7", 443)];
        assert!(EgressFlowRule.evaluate(&ctx_with(&inside, &scope)).is_empty());
        let outside = vec![event("::ffff:198.51.100.5", 443)];
        assert_eq!(EgressFlowRule.evaluate(&ctx_with(&outside, &scope)).len(), 1);
        Ok(())
    }

    #[test]
    fn test_address_outside_cidr_scope_triggers_a_finding() -> TestResult {
        let scope: NetworkScope = serde_json::from_str(r#"{"allow_hosts":["203.0.113.0/24"]}"#)
            .map_err(crate::test_support::ctx("NetworkScope liest ein Cidr-Ziel"))?;
        let events = vec![event("198.51.100.5", 8080)];
        let ctx = ctx_with(&events, &scope);

        let findings = EgressFlowRule.evaluate(&ctx);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].summary.contains("198.51.100.5:8080"));
        Ok(())
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

    /// PL-100 WP0.2: der Befund trägt die Zeit des auslösenden Ereignisses,
    /// nicht die Auswertungszeit — zwei Auswertungen zu verschiedenen
    /// Zeitpunkten liefern denselben Stempel.
    #[test]
    fn test_finding_is_stamped_with_the_event_time_not_ctx_now() -> TestResult {
        let event_time: Timestamp = "2026-11-08T10:00:00Z"
            .parse()
            .map_err(crate::test_support::ctx("Ereigniszeit"))?;
        let later: Timestamp = "2026-11-08T10:05:00Z"
            .parse()
            .map_err(crate::test_support::ctx("Auswertungszeit"))?;
        let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
        let events = vec![event_at("evil.example.com", 443, event_time)];
        let mut ctx = ctx_with(&events, &scope);
        ctx.now = later;

        let first = EgressFlowRule
            .evaluate(&ctx)
            .into_iter()
            .next()
            .ok_or_else(crate::test_support::missing("Befund"))?;
        assert_eq!(first.observed_at(), event_time);
        assert_ne!(first.observed_at(), ctx.now);

        // Eine spätere Auswertung desselben Ereignisses: derselbe Stempel.
        ctx.now = Timestamp::UNIX_EPOCH;
        let second = EgressFlowRule
            .evaluate(&ctx)
            .into_iter()
            .next()
            .ok_or_else(crate::test_support::missing("Befund"))?;
        assert_eq!(second.observed_at(), event_time);
        Ok(())
    }

    /// PL-100 WP0.3: Loopback-Ziele außerhalb des Bereichs werden mit `Low`
    /// gemeldet, alle anderen mit `High`.
    #[test]
    fn test_loopback_destinations_are_reported_low() {
        let scope = NetworkScope::empty();
        for destination in ["127.0.0.1", "127.12.0.9", "::1", "::ffff:127.0.0.1"] {
            let events = vec![event(destination, 8080)];
            let ctx = ctx_with(&events, &scope);
            let severities: Vec<Severity> = EgressFlowRule
                .evaluate(&ctx)
                .iter()
                .map(Finding::severity)
                .collect();
            assert_eq!(severities, vec![Severity::Low], "{destination}");
        }
    }

    #[test]
    fn test_non_loopback_destinations_stay_high() {
        let scope = NetworkScope::empty();
        for destination in ["198.51.100.5", "2001:db8::1", "128.0.0.1", "localhost"] {
            let events = vec![event(destination, 443)];
            let ctx = ctx_with(&events, &scope);
            let severities: Vec<Severity> = EgressFlowRule
                .evaluate(&ctx)
                .iter()
                .map(Finding::severity)
                .collect();
            assert_eq!(severities, vec![Severity::High], "{destination}");
        }
    }
}
