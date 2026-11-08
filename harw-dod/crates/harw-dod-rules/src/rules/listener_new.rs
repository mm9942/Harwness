//! `ListenerNewRule`: ein Listener, den dieser Host noch nie gesehen hat
//! (PL-100 WP1.6).
//!
//! # Verantwortungsbereich
//! Prüft jedes `EventKind::ListenerOpened`-Ereignis gegen die
//! [`crate::listener_history::ListenerHistory`] aus
//! [`crate::rule::RuleContext::listener_history`]. Ein Schlüssel
//! ([`crate::listener_history::ListenerKey`]: Besitzer, Protokoll, Port,
//! Bind-Klasse), der dort fehlt, ergibt einen Befund. Der Schlüssel wird mit
//! der [`crate::rule::RuleContext::rule_config`] abgeleitet (flüchtiger
//! Portbereich); der Aufrufer lernt mit derselben Konfiguration.
//!
//! # Lernen liegt beim Aufrufer
//! Die Regel ist rein und verändert die Historie nie. Gelernt wird nach dem
//! Zyklus über `ListenerHistory::observe` (Sentinel). Ohne Historie oder vor
//! dem Lernzyklus (`is_primed() == false`, erster Start) meldet die Regel
//! **nichts**: sonst wäre beim ersten Start jeder längst laufende Dienst ein
//! neuer Listener. Weil der Aufrufer jeden gemeldeten Schlüssel danach lernt,
//! erscheint ein neuer Listener genau einmal.
//!
//! Der Listener-Sensor meldet bei jedem Abruf **alle** offenen Listener; ein
//! Schlüssel, der in einem Zyklus mehrfach vorkommt (etwa `tcp` und `tcp6`
//! auf demselben Port, solange das Protokoll unbekannt ist), ergibt nur
//! einen Befund.
//!
//! # Schwere, Art und Härte
//! - Genauer Port außerhalb des flüchtigen Bereichs, oder ein als TCP
//!   bekannter Listener: wie `egress-flow` (Routing-Tabelle README §3.3) ist
//!   ein nur auf Loopback gebundener Listener [`Severity::Low`], jeder andere
//!   [`Severity::High`] — auch eine unbekannte Bind-Klasse (fail-closed,
//!   solange der Sensor die Adresse nicht liefert); `FindingKind::RuleTriggered`.
//! - Genauer Port **im** flüchtigen Bereich mit nicht als TCP bekanntem
//!   Protokoll (heute: jeder solche Listener), und der grobe UDP-Schlüssel
//!   ([`crate::listener_history::ListenerPort::Ephemeral`]):
//!   [`Severity::Low`] und `FindingKind::Anomaly`. Zufällige Ports nicht
//!   verbundener UDP-Clients und Testserver sind Alltag und dürfen nicht in
//!   jedem Zyklus `High` melden (B2). Gemeldet wird trotzdem **jeder** neue
//!   Port — auch eine TCP-Hintertür auf Port 50050 bleibt sichtbar (B19),
//!   nur eben als `Low`, bis der Sensor das Protokoll liefert und ein
//!   TCP-Listener wieder `RuleTriggered` wird.
//!
//! Härte [`Hardness::Observed`]: der Sensor liest die Listener-Tabellen des
//! Kernels direkt.
//!
//! # Burst-Grenze je Besitzer
//! Höchstens [`ListenerNewRule::MAX_FINDINGS_PER_OWNER`] Einzelbefunde je
//! Besitzer und Auswertung. Jeder weitere neue Listener desselben Besitzers
//! in dieser Auswertung geht in **einen** zusammenfassenden Befund ein
//! (`more than 32 new listeners in one cycle (owner …)`): so flutet ein
//! einzelner unprivilegierter Prozess, der tausende Ports öffnet, weder
//! Menschen noch KI mit tausenden Befunden (B2). Der Sammelbefund ist so
//! schwer wie der schwerste von ihm zusammengefasste Listener (`RuleTriggered`
//! vor `Anomaly`, dann die Schwere) und trägt die Zeit des ersten
//! zusammengefassten Ereignisses. Er steht nach den Einzelbefunden, in der
//! Reihenfolge, in der die Besitzer die Grenze überschritten. Die
//! zusammengefassten Listener lernt der Aufrufer wie alle anderen; einzeln
//! gemeldet werden sie danach nicht mehr.
//!
//! # Zeitstempel und Zusammenfassung
//! Der Befund trägt das `observed_at` des Ereignisses, nie
//! [`crate::rule::RuleContext::now`]. Die Zusammenfassung besteht nur aus
//! dem Schlüssel (Zahlen und feste Kennungen, kein angreiferkontrollierter
//! Text) und ist für denselben Schlüssel immer gleich; der Sammelbefund
//! nennt nur den Besitzer, keine Anzahl.
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
//! use harw_dod_rules::rules::ListenerNewRule;
//! use harw_dod_rules::ListenerHistory;
//! use harw_dod_signals::{EventKind, SecurityEvent};
//! use harw_types::SensorId;
//!
//! let listener = |port| SecurityEvent {
//!     sensor: SensorId::from_str("listener-0"),
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     actor: None,
//!     kind: EventKind::ListenerOpened { port },
//! };
//! let scope = NetworkScope::empty();
//! let mut history = ListenerHistory::new();
//! // Lernzyklus: Port 22 ist bekannt.
//! history.observe(&[listener(22)], jiff::Timestamp::UNIX_EPOCH);
//!
//! let events = vec![listener(22), listener(4444)];
//! let ctx = RuleContext {
//!     now: jiff::Timestamp::UNIX_EPOCH,
//!     samples: &[],
//!     events: &events,
//!     baselines: &[],
//!     network_scope: &scope,
//!     listener_history: Some(&history),
//!     rule_config: None,
//! };
//! let findings = ListenerNewRule.evaluate(&ctx);
//! assert_eq!(findings.len(), 1);
//! assert!(findings.iter().all(|finding| finding.summary().contains("port 4444")));
//! ```

use std::collections::{BTreeMap, HashSet};

use harw_dod_signals::{Hardness, Severity};
use jiff::Timestamp;

use crate::finding::{Finding, FindingKind, Raw};
use crate::listener_history::{
    BindClass, ListenerKey, ListenerOwner, ListenerPort, ListenerProtocol, is_ephemeral_port,
};
use crate::rule::{Rule, RuleConfig, RuleContext};

/// Meldet einen Listener, dessen Schlüssel die Historie noch nicht kennt.
///
/// # Description
/// Siehe Moduldoku. Genauer Port außerhalb des flüchtigen Bereichs (oder
/// TCP): `FindingKind::RuleTriggered`, Schwere nach Bind-Klasse; flüchtiger
/// Port ohne bekanntes TCP: `Anomaly`/`Low`. Härte `Observed`, Ereigniszeit.
/// Höchstens [`Self::MAX_FINDINGS_PER_OWNER`] Einzelbefunde je Besitzer und
/// Auswertung, darüber ein Sammelbefund.
///
/// # Errors
/// Keine eigenen Fehler; siehe [`crate::rule`]-Moduldoku.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenerNewRule;

impl ListenerNewRule {
    /// Höchstzahl Einzelbefunde je Besitzer und Auswertung; jeder weitere
    /// neue Listener desselben Besitzers geht in einen Sammelbefund ein
    /// (siehe Moduldoku, „Burst-Grenze je Besitzer").
    pub const MAX_FINDINGS_PER_OWNER: usize = 32;
}

impl Rule for ListenerNewRule {
    /// # Returns
    /// `"listener-new"`.
    fn id(&self) -> &'static str {
        "listener-new"
    }

    /// # Arguments
    /// - `ctx` (`&RuleContext<'_>`): siehe [`crate::rule::RuleContext`].
    ///
    /// # Returns
    /// Je unbekanntem Listener-Schlüssel einen Befund, gestempelt mit dem
    /// ersten Ereignis dieses Schlüssels — je Besitzer höchstens
    /// [`Self::MAX_FINDINGS_PER_OWNER`], dahinter je überlaufendem Besitzer
    /// ein Sammelbefund. Leer ohne Historie und vor dem Lernzyklus.
    fn evaluate(&self, ctx: &RuleContext<'_>) -> Vec<Finding<Raw>> {
        let Some(history) = ctx.listener_history else {
            return Vec::new();
        };
        if !history.is_primed() {
            return Vec::new();
        }
        let mut reported: HashSet<ListenerKey> = HashSet::new();
        let mut bursts: BTreeMap<ListenerOwner, Burst> = BTreeMap::new();
        // Besitzer in der Reihenfolge, in der sie die Grenze überschritten.
        let mut overflowed: Vec<ListenerOwner> = Vec::new();
        let mut findings = Vec::new();

        for event in ctx.events {
            let Some(key) = ListenerKey::from_event(event, ctx.rule_config) else {
                continue;
            };
            // Zuerst die billige Dublettenprüfung dieses Zyklus.
            if !reported.insert(key) || history.contains(&key) {
                continue;
            }
            let classification = classification_for(&key, ctx.rule_config);
            let burst = bursts.entry(key.owner).or_default();
            burst.new_keys = burst.new_keys.saturating_add(1);
            if burst.new_keys <= Self::MAX_FINDINGS_PER_OWNER {
                findings.push(Finding::raw(
                    self.id(),
                    classification.kind,
                    classification.severity,
                    Hardness::Observed,
                    summary_for(&key),
                    // Ereigniszeit, nicht `ctx.now` — siehe Moduldoku.
                    event.observed_at,
                ));
                continue;
            }
            match burst.overflow.as_mut() {
                Some(overflow) => {
                    overflow.classification = overflow.classification.heavier(classification);
                }
                None => {
                    burst.overflow = Some(Overflow {
                        first_at: event.observed_at,
                        classification,
                    });
                    overflowed.push(key.owner);
                }
            }
        }

        for owner in overflowed {
            let Some(overflow) = bursts.get(&owner).and_then(|burst| burst.overflow) else {
                continue;
            };
            findings.push(Finding::raw(
                self.id(),
                overflow.classification.kind,
                overflow.classification.severity,
                Hardness::Observed,
                burst_summary_for(owner),
                // Zeit des ersten zusammengefassten Ereignisses.
                overflow.first_at,
            ));
        }
        findings
    }
}

// Art und Schwere eines Befunds. Geordnet nach Gewicht: `RuleTriggered` vor
// `Anomaly`, dann die Schwere (siehe `Classification::rank`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Classification {
    // Befund-Art.
    kind: FindingKind,
    // Schwere.
    severity: Severity,
}

impl Classification {
    // Gewicht für den Vergleich: eskalierende Art zuerst, dann Schwere.
    fn rank(self) -> (bool, Severity) {
        (self.kind == FindingKind::RuleTriggered, self.severity)
    }

    // Die schwerere zweier Klassifikationen; bei Gleichstand `self`.
    fn heavier(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

// Zähler eines Besitzers innerhalb einer Auswertung.
#[derive(Debug, Default)]
struct Burst {
    // Neue Schlüssel dieses Besitzers bisher.
    new_keys: usize,
    // Gesetzt, sobald die Grenze überschritten ist.
    overflow: Option<Overflow>,
}

// Der Sammelbefund eines Besitzers, solange er entsteht.
#[derive(Debug, Clone, Copy)]
struct Overflow {
    // Zeit des ersten zusammengefassten Ereignisses.
    first_at: Timestamp,
    // Schwerste zusammengefasste Klassifikation.
    classification: Classification,
}

// Art und Schwere eines neuen Schlüssels — siehe Moduldoku.
fn classification_for(key: &ListenerKey, config: Option<&RuleConfig>) -> Classification {
    let low_anomaly = Classification {
        kind: FindingKind::Anomaly,
        severity: Severity::Low,
    };
    match key.port {
        ListenerPort::Ephemeral => low_anomaly,
        ListenerPort::Exact(port)
            if key.protocol != ListenerProtocol::Tcp && is_ephemeral_port(port, config) =>
        {
            low_anomaly
        }
        ListenerPort::Exact(_) => Classification {
            kind: FindingKind::RuleTriggered,
            severity: severity_for_bind(key.bind),
        },
    }
}

// `Low` nur für Loopback, sonst `High` (fail-closed) — siehe Moduldoku.
fn severity_for_bind(bind: BindClass) -> Severity {
    match bind {
        BindClass::Loopback => Severity::Low,
        BindClass::Any | BindClass::Specific | BindClass::Unknown => Severity::High,
    }
}

// Feste Kennung eines Besitzers für Zusammenfassungen.
fn owner_label(owner: ListenerOwner) -> String {
    match owner {
        ListenerOwner::CgroupKid(kid) => format!("cgroup-kid {kid}"),
        ListenerOwner::Uid(uid) => format!("uid {uid}"),
        ListenerOwner::Unknown => "unknown".to_owned(),
    }
}

// Stabile Zusammenfassung allein aus dem Schlüssel.
fn summary_for(key: &ListenerKey) -> String {
    let port = match key.port {
        ListenerPort::Exact(port) => format!("port {port}"),
        ListenerPort::Ephemeral => "an ephemeral port".to_owned(),
    };
    format!(
        "new listener on {port} (protocol {}, bind {}, owner {})",
        key.protocol.as_str(),
        key.bind.as_str(),
        owner_label(key.owner),
    )
}

// Stabile Zusammenfassung des Sammelbefunds: nur der Besitzer, keine Anzahl.
fn burst_summary_for(owner: ListenerOwner) -> String {
    format!(
        "more than {} new listeners in one cycle (owner {})",
        ListenerNewRule::MAX_FINDINGS_PER_OWNER,
        owner_label(owner),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listener_history::ListenerHistory;
    use crate::test_support::{TestResult, ctx, missing};
    use harw_authority::NetworkScope;
    use harw_dod_signals::{Actor, EventKind, SecurityEvent};
    use harw_types::SensorId;

    fn at(second: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(second).map_err(ctx("Testzeitpunkt"))
    }

    fn listener_at(port: u16, actor: Option<Actor>, observed_at: Timestamp) -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("listener-0"),
            observed_at,
            actor,
            kind: EventKind::ListenerOpened { port },
        }
    }

    fn listener(port: u16) -> SecurityEvent {
        listener_at(port, None, Timestamp::UNIX_EPOCH)
    }

    fn uid_actor(uid: u32) -> Option<Actor> {
        Some(Actor {
            uid,
            auid: None,
            cgroup: None,
            process: None,
        })
    }

    fn ctx_with<'a>(
        events: &'a [SecurityEvent],
        scope: &'a NetworkScope,
        history: Option<&'a ListenerHistory>,
    ) -> RuleContext<'a> {
        RuleContext {
            now: Timestamp::UNIX_EPOCH,
            samples: &[],
            events,
            baselines: &[],
            network_scope: scope,
            listener_history: history,
            rule_config: None,
        }
    }

    // Eine vorbereitete Historie, die genau `known` gelernt hat.
    fn primed(known: &[SecurityEvent]) -> ListenerHistory {
        let mut history = ListenerHistory::new();
        history.observe(known, Timestamp::UNIX_EPOCH);
        history
    }

    #[test]
    fn test_new_listener_on_primed_history_triggers_once() -> TestResult {
        let scope = NetworkScope::empty();
        let history = primed(&[listener(22)]);
        let events = vec![listener(4444)];
        let ctx = ctx_with(&events, &scope, Some(&history));

        let findings = ListenerNewRule.evaluate(&ctx);

        assert_eq!(findings.len(), 1);
        let finding = findings.first().ok_or_else(missing("Befund"))?;
        assert_eq!(finding.rule_id(), "listener-new");
        assert_eq!(finding.kind(), FindingKind::RuleTriggered);
        assert_eq!(finding.severity(), Severity::High);
        assert_eq!(finding.hardness(), Hardness::Observed);
        assert_eq!(
            finding.summary(),
            "new listener on port 4444 (protocol unknown, bind unknown, owner unknown)"
        );
        Ok(())
    }

    #[test]
    fn test_known_listener_does_not_trigger() {
        let scope = NetworkScope::empty();
        let history = primed(&[
            listener(22),
            listener_at(8080, uid_actor(1000), Timestamp::UNIX_EPOCH),
        ]);
        let events = vec![
            listener(22),
            listener_at(8080, uid_actor(1000), Timestamp::UNIX_EPOCH),
        ];
        let ctx = ctx_with(&events, &scope, Some(&history));

        assert!(ListenerNewRule.evaluate(&ctx).is_empty());
    }

    /// Derselbe Port unter einem anderen Besitzer ist ein neuer Listener.
    #[test]
    fn test_same_port_with_another_owner_triggers() -> TestResult {
        let scope = NetworkScope::empty();
        let history = primed(&[listener_at(8080, uid_actor(1000), Timestamp::UNIX_EPOCH)]);
        let events = vec![listener_at(8080, uid_actor(1001), Timestamp::UNIX_EPOCH)];
        let ctx = ctx_with(&events, &scope, Some(&history));

        let findings = ListenerNewRule.evaluate(&ctx);
        let finding = findings.first().ok_or_else(missing("Befund"))?;
        assert!(finding.summary().ends_with("owner uid 1001)"));
        Ok(())
    }

    /// Erster Start: vor dem Lernzyklus meldet die Regel nichts.
    #[test]
    fn test_unprimed_history_only_learns_and_reports_nothing() {
        let scope = NetworkScope::empty();
        let history = ListenerHistory::new();
        let events = vec![listener(22), listener(4444)];
        let ctx = ctx_with(&events, &scope, Some(&history));

        assert!(ListenerNewRule.evaluate(&ctx).is_empty());
    }

    #[test]
    fn test_missing_history_reports_nothing() {
        let scope = NetworkScope::empty();
        let events = vec![listener(4444)];
        let ctx = ctx_with(&events, &scope, None);

        assert!(ListenerNewRule.evaluate(&ctx).is_empty());
    }

    /// Der vollständige Ablauf über drei Zyklen: Lernzyklus still, ein neuer
    /// Listener genau einmal, danach bekannt.
    #[test]
    fn test_first_start_learning_then_report_once_then_quiet() {
        let scope = NetworkScope::empty();
        let mut history = ListenerHistory::new();

        let first = vec![listener(22)];
        assert!(
            ListenerNewRule
                .evaluate(&ctx_with(&first, &scope, Some(&history)))
                .is_empty()
        );
        history.observe(&first, Timestamp::UNIX_EPOCH);

        let second = vec![listener(22), listener(4444)];
        assert_eq!(
            ListenerNewRule
                .evaluate(&ctx_with(&second, &scope, Some(&history)))
                .len(),
            1
        );
        history.observe(&second, Timestamp::UNIX_EPOCH);

        assert!(
            ListenerNewRule
                .evaluate(&ctx_with(&second, &scope, Some(&history)))
                .is_empty()
        );
    }

    /// Ein Ereignis ohne Akteur (fremder Benutzer, Besitzer nicht lesbar)
    /// wird nicht verworfen, sondern mit unbekanntem Besitzer gemeldet.
    #[test]
    fn test_listener_without_actor_is_reported_with_unknown_owner() -> TestResult {
        let scope = NetworkScope::empty();
        let history = primed(&[]);
        let events = vec![listener(31337)];
        let ctx = ctx_with(&events, &scope, Some(&history));

        let findings = ListenerNewRule.evaluate(&ctx);
        let finding = findings.first().ok_or_else(missing("Befund"))?;
        assert!(finding.summary().contains("owner unknown"));
        Ok(())
    }

    #[test]
    fn test_duplicate_key_in_one_cycle_yields_one_finding() {
        let scope = NetworkScope::empty();
        let history = primed(&[]);
        let events = vec![listener(4444), listener(4444)];
        let ctx = ctx_with(&events, &scope, Some(&history));

        assert_eq!(ListenerNewRule.evaluate(&ctx).len(), 1);
    }

    #[test]
    fn test_finding_is_stamped_with_the_event_time_not_ctx_now() -> TestResult {
        let event_time: Timestamp = "2026-11-08T10:00:00Z"
            .parse()
            .map_err(ctx("Ereigniszeit"))?;
        let later: Timestamp = "2026-11-08T10:05:00Z"
            .parse()
            .map_err(ctx("Auswertungszeit"))?;
        let scope = NetworkScope::empty();
        let history = primed(&[]);
        let events = vec![listener_at(4444, None, event_time)];
        let mut rule_ctx = ctx_with(&events, &scope, Some(&history));
        rule_ctx.now = later;

        let stamps: Vec<Timestamp> = ListenerNewRule
            .evaluate(&rule_ctx)
            .iter()
            .map(Finding::observed_at)
            .collect();
        assert_eq!(stamps, vec![event_time]);
        Ok(())
    }

    #[test]
    fn test_loopback_bind_is_low_everything_else_high() {
        assert_eq!(severity_for_bind(BindClass::Loopback), Severity::Low);
        assert_eq!(severity_for_bind(BindClass::Any), Severity::High);
        assert_eq!(severity_for_bind(BindClass::Specific), Severity::High);
        assert_eq!(severity_for_bind(BindClass::Unknown), Severity::High);
    }

    #[test]
    fn test_summary_names_every_key_part() {
        let summary = summary_for(&ListenerKey {
            owner: ListenerOwner::CgroupKid(42),
            protocol: ListenerProtocol::Udp,
            port: ListenerPort::Exact(53),
            bind: BindClass::Loopback,
        });
        assert_eq!(
            summary,
            "new listener on port 53 (protocol udp, bind loopback, owner cgroup-kid 42)"
        );
        let ephemeral = summary_for(&ListenerKey {
            owner: ListenerOwner::Uid(1000),
            protocol: ListenerProtocol::Unknown,
            port: ListenerPort::Ephemeral,
            bind: BindClass::Unknown,
        });
        assert_eq!(
            ephemeral,
            "new listener on an ephemeral port (protocol unknown, bind unknown, owner uid 1000)"
        );
    }

    /// Review-Befund (Runde 2, hoch): solange das Protokoll unbekannt ist,
    /// bleibt ein flüchtiger Port genau geschlüsselt. Jeder neue zufällige
    /// Port ergibt genau einmal einen `Low`/`Anomaly`-Befund (kein `High` je
    /// Zyklus, B2), ein bereits gelernter Port nichts.
    #[test]
    fn test_random_ephemeral_ports_report_each_new_port_once_and_low() {
        let scope = NetworkScope::empty();
        let mut history = primed(&[listener(22)]);
        let mut all = Vec::new();
        for port in [41_234_u16, 50_001, 33_333, 60_999, 41_234, 32_768] {
            let events = vec![listener_at(port, uid_actor(1000), Timestamp::UNIX_EPOCH)];
            all.extend(ListenerNewRule.evaluate(&ctx_with(&events, &scope, Some(&history))));
            history.observe(&events, Timestamp::UNIX_EPOCH);
        }

        let found: Vec<(FindingKind, Severity, String)> = all
            .iter()
            .map(|finding| {
                (
                    finding.kind(),
                    finding.severity(),
                    finding.summary().to_owned(),
                )
            })
            .collect();
        let expected: Vec<(FindingKind, Severity, String)> = [
            41_234_u16, 50_001, 33_333, 60_999, 32_768,
        ]
        .into_iter()
        .map(|port| {
            (
                FindingKind::Anomaly,
                Severity::Low,
                format!(
                    "new listener on port {port} (protocol unknown, bind unknown, owner uid 1000)"
                ),
            )
        })
        .collect();
        assert_eq!(found, expected);
    }

    /// Bekannte und registrierte Ports außerhalb des flüchtigen Bereichs
    /// bleiben genau geschlüsselt und `High`.
    #[test]
    fn test_well_known_and_registered_ports_stay_high() {
        let scope = NetworkScope::empty();
        let history = primed(&[]);
        let events = vec![
            listener(22),
            listener(4444),
            listener(31_337),
            listener(32_767),
            listener(61_000),
        ];
        let found: Vec<(FindingKind, Severity)> = ListenerNewRule
            .evaluate(&ctx_with(&events, &scope, Some(&history)))
            .iter()
            .map(|finding| (finding.kind(), finding.severity()))
            .collect();
        assert_eq!(found, vec![(FindingKind::RuleTriggered, Severity::High); 5]);
    }

    /// Regressionstest zum Befund (Runde 2, hoch): fremde Listener kommen
    /// ohne Akteur (Besitzer unbekannt). Nachdem ein flüchtiger Port dieses
    /// Besitzers gelernt ist — oder sogar ein grober UDP-Schlüssel —, wird
    /// ein neuer Listener auf 50050 (etwa eine TCP-Hintertür) trotzdem
    /// gemeldet.
    #[test]
    fn test_new_ephemeral_range_listener_is_reported_after_another_was_learned() -> TestResult {
        let scope = NetworkScope::empty();
        let expected = vec![(
            FindingKind::Anomaly,
            Severity::Low,
            "new listener on port 50050 (protocol unknown, bind unknown, owner unknown)".to_owned(),
        )];
        let shapes = |history: &ListenerHistory| -> Vec<(FindingKind, Severity, String)> {
            let events = vec![listener(41_234), listener(50_050)];
            ListenerNewRule
                .evaluate(&ctx_with(&events, &scope, Some(history)))
                .iter()
                .map(|finding| {
                    (
                        finding.kind(),
                        finding.severity(),
                        finding.summary().to_owned(),
                    )
                })
                .collect()
        };

        let learned_port = primed(&[listener(41_234)]);
        assert_eq!(shapes(&learned_port), expected);

        // Eine Historie mit gelerntem grobem UDP-Schlüssel und 41234.
        let with_udp = ListenerHistory::from_json(
            br#"{"version":1,"primed_at":"1970-01-01T00:00:00Z","entries":[
                {"key":{"owner":"unknown","protocol":"udp","port":"ephemeral","bind":"unknown"},
                 "last_seen":"1970-01-01T00:00:00Z"},
                {"key":{"owner":"unknown","protocol":"unknown","port":{"exact":41234},"bind":"unknown"},
                 "last_seen":"1970-01-01T00:00:00Z"}]}"#,
        )
        .map_err(ctx("Historie mit UDP-Schlüssel"))?;
        assert_eq!(shapes(&with_udp), expected);
        Ok(())
    }

    #[test]
    fn test_classification_by_port_range_protocol_and_bind() {
        let key = |protocol, port, bind| ListenerKey {
            owner: ListenerOwner::Unknown,
            protocol,
            port,
            bind,
        };
        let low_anomaly = Classification {
            kind: FindingKind::Anomaly,
            severity: Severity::Low,
        };
        let triggered = |severity| Classification {
            kind: FindingKind::RuleTriggered,
            severity,
        };
        let cases = [
            // Unbekanntes Protokoll im flüchtigen Bereich: leise.
            (
                key(
                    ListenerProtocol::Unknown,
                    ListenerPort::Exact(50_050),
                    BindClass::Any,
                ),
                low_anomaly,
            ),
            // TCP im flüchtigen Bereich: bewusstes `listen()`.
            (
                key(
                    ListenerProtocol::Tcp,
                    ListenerPort::Exact(50_050),
                    BindClass::Any,
                ),
                triggered(Severity::High),
            ),
            (
                key(
                    ListenerProtocol::Udp,
                    ListenerPort::Ephemeral,
                    BindClass::Any,
                ),
                low_anomaly,
            ),
            (
                key(
                    ListenerProtocol::Unknown,
                    ListenerPort::Exact(8080),
                    BindClass::Unknown,
                ),
                triggered(Severity::High),
            ),
            (
                key(
                    ListenerProtocol::Tcp,
                    ListenerPort::Exact(8080),
                    BindClass::Loopback,
                ),
                triggered(Severity::Low),
            ),
        ];
        for (key, expected) in cases {
            assert_eq!(classification_for(&key, None), expected, "{key:?}");
        }
    }

    #[test]
    fn test_heavier_classification_prefers_rule_triggered_then_severity() {
        let low_anomaly = Classification {
            kind: FindingKind::Anomaly,
            severity: Severity::Low,
        };
        let high = Classification {
            kind: FindingKind::RuleTriggered,
            severity: Severity::High,
        };
        let low_triggered = Classification {
            kind: FindingKind::RuleTriggered,
            severity: Severity::Low,
        };
        assert_eq!(low_anomaly.heavier(high), high);
        assert_eq!(high.heavier(low_anomaly), high);
        assert_eq!(low_anomaly.heavier(low_triggered), low_triggered);
        assert_eq!(low_triggered.heavier(high), high);
    }

    /// Review-Befund (Runde 2): ein Prozess mit tausend neuen Ports ergab
    /// tausend `High`-Befunde in einem Zyklus. Jetzt: 32 Einzelbefunde und
    /// ein Sammelbefund, gestempelt mit dem ersten zusammengefassten
    /// Ereignis.
    #[test]
    fn test_burst_of_new_listeners_is_capped_per_owner() -> TestResult {
        let scope = NetworkScope::empty();
        let history = primed(&[]);
        let mut events = Vec::with_capacity(1_000);
        for port in 2_000_u16..3_000 {
            events.push(listener_at(port, None, at(i64::from(port))?));
        }

        let findings = ListenerNewRule.evaluate(&ctx_with(&events, &scope, Some(&history)));

        assert_eq!(
            findings.len(),
            ListenerNewRule::MAX_FINDINGS_PER_OWNER.saturating_add(1)
        );
        let (aggregate, individual) = findings.split_last().ok_or_else(missing("Sammelbefund"))?;
        assert!(
            individual
                .iter()
                .all(|finding| finding.summary().starts_with("new listener on port"))
        );
        let last_individual = individual.last().ok_or_else(missing("Einzelbefund"))?;
        assert_eq!(
            last_individual.summary(),
            "new listener on port 2031 (protocol unknown, bind unknown, owner unknown)"
        );
        assert_eq!(
            aggregate.summary(),
            "more than 32 new listeners in one cycle (owner unknown)"
        );
        assert_eq!(aggregate.kind(), FindingKind::RuleTriggered);
        assert_eq!(aggregate.severity(), Severity::High);
        assert_eq!(aggregate.hardness(), Hardness::Observed);
        assert_eq!(aggregate.observed_at(), at(2_032)?);
        Ok(())
    }

    /// Die Grenze gilt je Besitzer; der Sammelbefund ist so schwer wie der
    /// schwerste zusammengefasste Listener.
    #[test]
    fn test_burst_cap_is_per_owner_and_takes_the_heaviest_classification() -> TestResult {
        let scope = NetworkScope::empty();
        let history = primed(&[]);
        let mut events = Vec::new();
        // uid 1000: 40 zufällige flüchtige Ports, alle `Low`/`Anomaly`.
        for port in 40_000_u16..40_040 {
            events.push(listener_at(port, uid_actor(1000), at(1)?));
        }
        // uid 1001: 32 flüchtige Ports, dann ein `High`-Port.
        for port in 50_000_u16..50_032 {
            events.push(listener_at(port, uid_actor(1001), at(2)?));
        }
        events.push(listener_at(4444, uid_actor(1001), at(3)?));
        // uid 1002: genau an der Grenze, kein Sammelbefund.
        for port in 5_000_u16..5_032 {
            events.push(listener_at(port, uid_actor(1002), at(4)?));
        }

        let findings = ListenerNewRule.evaluate(&ctx_with(&events, &scope, Some(&history)));
        // Je 32 Einzelbefunde für drei Besitzer plus zwei Sammelbefunde.
        assert_eq!(findings.len(), 98);
        let aggregates: Vec<(String, FindingKind, Severity, Timestamp)> = findings
            .iter()
            .filter(|finding| finding.summary().starts_with("more than"))
            .map(|finding| {
                (
                    finding.summary().to_owned(),
                    finding.kind(),
                    finding.severity(),
                    finding.observed_at(),
                )
            })
            .collect();
        assert_eq!(
            aggregates,
            vec![
                (
                    "more than 32 new listeners in one cycle (owner uid 1000)".to_owned(),
                    FindingKind::Anomaly,
                    Severity::Low,
                    at(1)?,
                ),
                (
                    "more than 32 new listeners in one cycle (owner uid 1001)".to_owned(),
                    FindingKind::RuleTriggered,
                    Severity::High,
                    at(3)?,
                ),
            ]
        );
        Ok(())
    }

    /// Der flüchtige Bereich kommt aus der `rule_config`; Lernen mit
    /// derselben Konfiguration hält Auswertung und Historie deckungsgleich.
    #[test]
    fn test_configured_ephemeral_range_is_used_for_rule_and_history() -> TestResult {
        let scope = NetworkScope::empty();
        let config = RuleConfig::new()
            .with_ephemeral_ports(10_000, 20_000)
            .map_err(ctx("Portbereich"))?;
        let mut history = ListenerHistory::new();
        history.observe_with_config(&[], Timestamp::UNIX_EPOCH, Some(&config));

        let first = vec![listener(15_000)];
        let mut rule_ctx = ctx_with(&first, &scope, Some(&history));
        rule_ctx.rule_config = Some(&config);
        let findings = ListenerNewRule.evaluate(&rule_ctx);
        let finding = findings.first().ok_or_else(missing("Befund"))?;
        assert_eq!(finding.severity(), Severity::Low);
        history.observe_with_config(&first, Timestamp::UNIX_EPOCH, Some(&config));

        // Der gelernte Port ist bekannt; ein anderer Port im konfigurierten
        // Bereich ist neu und leise, ein Port im Vorgabe-Bereich, aber
        // außerhalb des konfigurierten, neu und `High`.
        let second = vec![listener(15_000), listener(19_999), listener(41_234)];
        let mut rule_ctx = ctx_with(&second, &scope, Some(&history));
        rule_ctx.rule_config = Some(&config);
        let found: Vec<(Severity, String)> = ListenerNewRule
            .evaluate(&rule_ctx)
            .iter()
            .map(|finding| (finding.severity(), finding.summary().to_owned()))
            .collect();
        assert_eq!(
            found,
            vec![
                (
                    Severity::Low,
                    "new listener on port 19999 (protocol unknown, bind unknown, owner unknown)"
                        .to_owned()
                ),
                (
                    Severity::High,
                    "new listener on port 41234 (protocol unknown, bind unknown, owner unknown)"
                        .to_owned()
                ),
            ]
        );
        Ok(())
    }

    #[test]
    fn test_other_event_kinds_are_ignored() {
        let scope = NetworkScope::empty();
        let history = primed(&[]);
        let events = vec![SecurityEvent {
            sensor: SensorId::from_str("net-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::EgressFlow {
                destination: "198.51.100.5".to_owned(),
                port: 443,
            },
        }];
        let ctx = ctx_with(&events, &scope, Some(&history));

        assert!(ListenerNewRule.evaluate(&ctx).is_empty());
    }

    #[test]
    fn test_evaluate_is_deterministic_for_identical_context() {
        let scope = NetworkScope::empty();
        let history = primed(&[listener(22)]);
        let events = vec![listener(4444), listener(8080), listener(22)];
        let ctx = ctx_with(&events, &scope, Some(&history));

        assert_eq!(
            ListenerNewRule.evaluate(&ctx),
            ListenerNewRule.evaluate(&ctx)
        );
    }
}
