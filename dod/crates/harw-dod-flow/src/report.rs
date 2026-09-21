//! Die Melderegel: wann ein [`crate::event::FlowEvent`] eine
//! `harw_dod_signals::SecurityEvent` wird — und wann nicht.
//!
//! # Die Entscheidung, die diese Crate trägt
//! `harw_dod_signals::EventKind::EgressFlow { destination, port }` trägt eine
//! Zieladresse — genau die Variante, für die dieser Knoten gebaut wurde, und
//! die ihn vom Listener-Sensor unterscheidet, der ausdrücklich **keine**
//! Ziele melden darf. Eine Zieladresse ist aber ein sensibler Wert: sie
//! sagt, mit wem der Host spricht. Drei Fragen waren zu entscheiden.
//!
//! **Volle Adresse oder Netz?** Diese Crate meldet die volle Adresse
//! (`IpAddr::to_string()`), nicht ein `/24`- oder `/64`-Netz. Begründung:
//! [`harw_authority::NetworkScope::allows_addr`] entscheidet ohnehin exakt,
//! *ob* eine Verbindung überhaupt gemeldet wird (siehe unten) — die
//! Adresse selbst erscheint nur für Verbindungen, die bereits als
//! Politikverstoß eingestuft sind. Ein Netz zu melden verwischte genau in
//! diesem Fall den forensisch wichtigsten Teil des Befundes (welcher Host
//! genau kontaktiert wurde), ohne die Menge der gemeldeten Ereignisse zu
//! verringern — die Melderegel selbst leistet die Sparsamkeit bereits.
//!
//! **Jede Verbindung oder nur Bereichsverlassende?** Nur Bereichsverlassende.
//! Diese Crate hängt dafür an `harw_sandbox` — Kantenrichtung geprüft:
//! `harw-sandbox` hängt selbst an nichts aus dem `harw-dod-*`-Teilbaum (nur
//! `harw-types`, `ipnet`, `serde`), diese Abhängigkeit schließt also keinen
//! Zyklus. [`harw_authority::NetworkScope`] trägt bereits `allows_addr`, exakt
//! für diesen Zweck erweitert. [`to_security_event`] meldet eine
//! [`harw_dod_signals::SecurityEvent`] **nur**, wenn `remote_addr` **nicht**
//! im übergebenen [`harw_authority::NetworkScope`] liegt — das ist zugleich die
//! aussagekräftigste Wahl (jede Meldung ist ein tatsächlicher
//! Politikverstoß) und die sparsamste (Verbindungen innerhalb der erlaubten
//! Richtlinie erzeugen keinen Datenverkehr in der Zeitreihe).
//!
//! **Loopback und private Bereiche: Lärm oder Signal?** Diese Crate trifft
//! dazu **keine** eigene, fest verdrahtete Ausnahme. Ob `127.0.0.1` oder
//! `10.0.0.0/8` Lärm oder Signal sind, hängt davon ab, was der Betreiber im
//! [`harw_authority::NetworkScope`] tatsächlich autorisiert hat — steht ein
//! solcher Bereich dort, ist er erlaubt und wird nicht gemeldet; steht er
//! nicht dort, hat der Betreiber ihn nicht autorisiert, und eine
//! Verbindung dorthin ist derselbe Politikverstoß wie jede andere. Eine
//! eingebaute Ausnahme für „übliche" private Bereiche würde genau die Fälle
//! verstecken, in denen ein kompromittierter Prozess absichtlich über
//! Loopback oder ein vermeintlich vertrauenswürdiges internes Netz
//! kommuniziert — ein Verhalten, für dessen Erkennung dieser Sensor gebaut
//! wurde. Ein leerer (Standard-)Scope erlaubt folgerichtig **kein** Ziel und
//! lässt jede ausgehende Verbindung als Befund erscheinen — für einen
//! Erkennungssensor die sichere Grundeinstellung: zu viel Signal ist ein
//! Tuning-Problem der Politik, zu wenig ist ein blinder Fleck.
//!
//! Nur [`crate::event::Direction::Outbound`]-Ereignisse sind überhaupt
//! Kandidaten: `EventKind::EgressFlow` ist laut Typ-Doku ausdrücklich
//! *ausgehender* Verkehr; eingehende Verbindungen gehören nicht zu dieser
//! Variante und werden von [`to_security_event`] unabhängig vom Scope nie
//! gemeldet.
//!
//! # Der Zeitstempel: `RawBpfEvent::observed_at`, nicht ein Poll-`now`
//! [`observe`] nimmt bewusst **kein** injiziertes `now` entgegen. Der
//! allgemeine Grundsatz „ein Sensor liest nie die Systemuhr selbst" bleibt
//! trotzdem gewahrt: `harw_dod_bpf::RawBpfEvent::observed_at` ist selbst
//! bereits ein injizierter Wert — unter einem echten Lader ein vom Kernel
//! erzeugter, unter [`harw_dod_bpf::fixture::FixtureBpfLoader`] ein in Tests
//! frei gewählter. Ein zusätzliches, grobkörniges Poll-`now` (ein einziger
//! Zeitpunkt für alle in einem Abrufzyklus gelesenen Ereignisse) hätte hier
//! keinen Vorteil: es würde nur die feinere, bereits vorhandene
//! Pro-Ereignis-Präzision verdecken, mit der das erzeugende eBPF-Programm
//! den tatsächlichen Verbindungszeitpunkt festgehalten hat, ohne die
//! Determinismus-Zusage zu verbessern (die schon durch die Injektion von
//! `RawBpfEvent::observed_at` erfüllt ist).
//!
//! # Keine Nutzdaten
//! Eine erzeugte `SecurityEvent` trägt ausschließlich `sensor`,
//! `observed_at`, einen aus `uid` gebauten `Actor` und
//! `EventKind::EgressFlow { destination, port }` — keines dieser Felder kann
//! beliebig lange Verbindungsinhalte tragen. [`tests::test_observe_never_leaks_bytes_beyond_the_flow_layout_into_the_serialized_event`]
//! belegt das für den vollständigen Pfad von [`harw_dod_bpf::RawBpfEvent`]
//! bis zur serialisierten `SecurityEvent`.
//!
//! # Warum `harw-dod-fixtures` hier nicht passt
//! `harw_dod_fixtures::sensor_suite!` setzt einen Sensor voraus, der über
//! `harw_dod_cap::SensorHandle<harw_dod_cap::Bound>::scope()` liest — ein
//! `ReadScope` auf einen echten `fixtures/<fall>/tree`-Verzeichnisbaum, den
//! alle sechs Prüfungen (Determinismus, Inhaltsfreiheit, Scope-Dichtheit,
//! Redaktion, Kardinalität, Fehlerfall) gegen dieses Verzeichnis ausführen.
//! Diese Crate liest nie ein Dateisystem: Ereignisse kommen über einen
//! injizierten `harw_dod_bpf::BpfLoader` herein, und `scope()` würde nie
//! angefasst. Ein Sensor, der `scope()` ignoriert, bestünde jede der sechs
//! Prüfungen **trivial** — grün, weil an genau der Stelle geprüft wird, die
//! diese Crate nicht benutzt, während die eigentliche Payload-Parselogik nie
//! ausgeführt würde (dieselbe Beobachtung, die bei `harw-dod-authlog`
//! auffiel). Die Tests dieses Moduls sind deshalb von Hand geschrieben und
//! laufen ausschließlich über [`harw_dod_bpf::fixture::FixtureBpfLoader`],
//! nie über `sensor_suite!`.
//!
//! # Nebenläufigkeit
//! Zustandslose freie Funktionen: `Send + Sync`, ohne innere Veränderlichkeit.
//!
//! # Fehler
//! [`crate::error::FlowError`], ausschließlich über [`crate::event::parse_flow_payload`]
//! innerhalb von [`observe`].
//!
//! # Examples
//! ```rust
//! use harw_dod_flow::report::to_security_event;
//! use harw_dod_flow::event::{Direction, FlowEvent, Protocol};
//! use harw_authority::NetworkScope;
//! use harw_types::SensorId;
//! use std::net::{IpAddr, Ipv4Addr};
//!
//! let event = FlowEvent {
//!     pid: 1,
//!     uid: 0,
//!     protocol: Protocol::Tcp,
//!     direction: Direction::Outbound,
//!     remote_addr: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9)),
//!     remote_port: 443,
//! };
//! let sensor = SensorId::from_str("flow-0");
//! let scope = NetworkScope::empty(); // erlaubt kein Ziel
//!
//! let reported = to_security_event(&event, &sensor, jiff::Timestamp::UNIX_EPOCH, &scope);
//! assert!(reported.is_some(), "eine Verbindung außerhalb des (leeren) Scopes ist ein Befund");
//! ```

use harw_dod_bpf::RawBpfEvent;
use harw_dod_signals::{Actor, EventKind, SecurityEvent};
use harw_authority::NetworkScope;
use harw_types::SensorId;
use jiff::Timestamp;

use crate::error::FlowError;
use crate::event::{parse_flow_payload, Direction, FlowEvent};

/// Übersetzt ein geparstes [`FlowEvent`] in eine `SecurityEvent` — oder in
/// gar keine, wenn die Melderegel greift.
///
/// # Description
/// Siehe Moduldoku für die vollständige Begründung. Kurz: gemeldet wird nur
/// [`Direction::Outbound`]-Verkehr, dessen `remote_addr` **nicht** im
/// übergebenen `scope` erlaubt ist.
///
/// # Arguments
/// - `event` (`&FlowEvent`): das bereits geparste Verbindungsereignis.
/// - `sensor` (`&harw_types::SensorId`): die Kennung des meldenden Sensors.
/// - `observed_at` (`jiff::Timestamp`): der Beobachtungszeitpunkt — siehe
///   Moduldoku, Abschnitt „Der Zeitstempel", für die empfohlene Quelle
///   ([`RawBpfEvent::observed_at`]).
/// - `scope` (`&harw_authority::NetworkScope`): der für diesen Host bzw. diese
///   Sandbox autorisierte Zielbereich.
///
/// # Returns
/// `Some(SecurityEvent)` mit `EventKind::EgressFlow`, wenn `event` die
/// Melderegel auslöst; `None`, wenn `event` eingehend ist oder sein Ziel
/// innerhalb von `scope` liegt.
///
/// # Examples
/// Siehe Moduldoku.
#[must_use]
pub fn to_security_event(
    event: &FlowEvent,
    sensor: &SensorId,
    observed_at: Timestamp,
    scope: &NetworkScope,
) -> Option<SecurityEvent> {
    if event.direction != Direction::Outbound {
        return None;
    }
    if scope.allows_addr(event.remote_addr) {
        return None;
    }

    Some(SecurityEvent {
        sensor: sensor.clone(),
        observed_at,
        actor: Some(Actor {
            uid: event.uid,
            auid: None,
            cgroup: None,
        }),
        kind: EventKind::EgressFlow {
            destination: event.remote_addr.to_string(),
            port: event.remote_port,
        },
    })
}

/// Deutet ein rohes eBPF-Ereignis und wendet die Melderegel darauf an.
///
/// # Description
/// Kombiniert [`crate::event::parse_flow_payload`] auf
/// `raw.payload` mit [`to_security_event`], unter Verwendung von
/// `raw.observed_at` als Beobachtungszeitpunkt (siehe Moduldoku, Abschnitt
/// „Der Zeitstempel").
///
/// # Arguments
/// - `raw` (`&harw_dod_bpf::RawBpfEvent`): ein von einem
///   `harw_dod_bpf::BpfLoader` gelesenes Rohereignis.
/// - `sensor` (`&harw_types::SensorId`): die Kennung des meldenden Sensors.
/// - `scope` (`&harw_authority::NetworkScope`): siehe [`to_security_event`].
///
/// # Returns
/// `Ok(Some(SecurityEvent))`, wenn die Melderegel greift; `Ok(None)`, wenn
/// `raw` erfolgreich geparst wurde, aber kein Befund ist.
///
/// # Errors
/// - [`FlowError::MalformedEvent`], wenn `raw.payload` nicht dem
///   Flow-Payload-Layout entspricht.
///
/// # Examples
/// Siehe Moduldoku sowie die Tests dieses Moduls.
pub fn observe(
    raw: &RawBpfEvent,
    sensor: &SensorId,
    scope: &NetworkScope,
) -> Result<Option<SecurityEvent>, FlowError> {
    let event = parse_flow_payload(&raw.payload)?;
    Ok(to_security_event(&event, sensor, raw.observed_at, scope))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use harw_dod_bpf::RawBpfEvent;
    use harw_authority::{EgressTarget, NetworkScope};
    use harw_types::SensorId;
    use jiff::Timestamp;

    use super::{observe, to_security_event};
    use crate::event::{Direction, FlowEvent, Protocol};

    fn sensor() -> SensorId {
        SensorId::from_str("flow-0")
    }

    fn scope_allowing_10_0_0_0_24() -> NetworkScope {
        let cidr: ipnet::IpNet = "10.0.0.0/24".parse().expect("valid test CIDR literal");
        NetworkScope::from_targets([EgressTarget::Cidr(cidr)])
    }

    fn outbound_event(addr: IpAddr) -> FlowEvent {
        FlowEvent {
            pid: 100,
            uid: 1_000,
            protocol: Protocol::Tcp,
            direction: Direction::Outbound,
            remote_addr: addr,
            remote_port: 443,
        }
    }

    #[test]
    fn test_to_security_event_reports_a_flow_that_leaves_the_allowed_scope() {
        let event = outbound_event(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9)));
        let scope = scope_allowing_10_0_0_0_24();

        let reported = to_security_event(&event, &sensor(), Timestamp::UNIX_EPOCH, &scope)
            .expect("a destination outside the allowed scope must be reported");

        assert_eq!(reported.sensor, sensor());
        assert_eq!(reported.actor.as_ref().map(|a| a.uid), Some(1_000));
        assert!(matches!(
            reported.kind,
            harw_dod_signals::EventKind::EgressFlow { ref destination, port: 443 }
                if destination == "203.0.113.9"
        ));
    }

    #[test]
    fn test_to_security_event_does_not_report_a_flow_inside_the_allowed_scope() {
        let event = outbound_event(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)));
        let scope = scope_allowing_10_0_0_0_24();

        assert!(
            to_security_event(&event, &sensor(), Timestamp::UNIX_EPOCH, &scope).is_none(),
            "eine Verbindung innerhalb des erlaubten Bereichs darf nicht gemeldet werden"
        );
    }

    #[test]
    fn test_to_security_event_never_reports_inbound_flows_regardless_of_scope() {
        let mut event = outbound_event(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9)));
        event.direction = Direction::Inbound;
        let scope = NetworkScope::empty();

        assert!(
            to_security_event(&event, &sensor(), Timestamp::UNIX_EPOCH, &scope).is_none(),
            "EgressFlow ist ausdrücklich ausgehender Verkehr — eingehend wird nie gemeldet"
        );
    }

    #[test]
    fn test_to_security_event_empty_scope_reports_every_outbound_destination() {
        let event = outbound_event(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
        let scope = NetworkScope::empty();

        assert!(
            to_security_event(&event, &sensor(), Timestamp::UNIX_EPOCH, &scope).is_some(),
            "ein leerer Scope autorisiert kein Ziel — auch Loopback nicht, siehe Moduldoku"
        );
    }

    #[test]
    fn test_to_security_event_is_deterministic_for_the_same_inputs() {
        let event = outbound_event(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9)));
        let scope = scope_allowing_10_0_0_0_24();

        let first = to_security_event(&event, &sensor(), Timestamp::UNIX_EPOCH, &scope);
        let second = to_security_event(&event, &sensor(), Timestamp::UNIX_EPOCH, &scope);
        assert_eq!(first, second);
    }

    fn well_formed_flow_payload(port: u16, addr: [u8; 4], trailer: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8; 32];
        bytes[0..4].copy_from_slice(&100u32.to_le_bytes()); // pid
        bytes[4..8].copy_from_slice(&1_000u32.to_le_bytes()); // uid
        bytes[8] = 0; // TCP
        bytes[9] = 1; // outbound
        bytes[10] = 0; // IPv4
        bytes[12..14].copy_from_slice(&port.to_be_bytes());
        bytes[16..20].copy_from_slice(&addr);
        bytes.extend_from_slice(trailer);
        bytes
    }

    #[test]
    fn test_observe_reports_a_flow_that_leaves_the_allowed_scope() {
        let payload = well_formed_flow_payload(443, [203, 0, 113, 9], &[]);
        let raw = RawBpfEvent {
            pid: 100,
            comm: "curl".to_owned(),
            observed_at: Timestamp::UNIX_EPOCH,
            payload,
        };
        let scope = scope_allowing_10_0_0_0_24();

        let reported = observe(&raw, &sensor(), &scope)
            .expect("well-formed payload must parse")
            .expect("destination outside scope must be reported");
        assert_eq!(reported.observed_at, Timestamp::UNIX_EPOCH);
    }

    #[test]
    fn test_observe_is_deterministic_for_the_same_raw_event() {
        let payload = well_formed_flow_payload(443, [203, 0, 113, 9], &[]);
        let raw = RawBpfEvent {
            pid: 100,
            comm: "curl".to_owned(),
            observed_at: Timestamp::UNIX_EPOCH,
            payload,
        };
        let scope = scope_allowing_10_0_0_0_24();

        let first = observe(&raw, &sensor(), &scope).expect("parses");
        let second = observe(&raw, &sensor(), &scope).expect("parses");
        assert_eq!(first, second);
    }

    #[test]
    fn test_observe_never_leaks_bytes_beyond_the_flow_layout_into_the_serialized_event() {
        const CANARY: &str = "HARW-FLOW-PAYLOAD-CANARY-CONTENT";
        let payload = well_formed_flow_payload(443, [203, 0, 113, 9], CANARY.as_bytes());
        let raw = RawBpfEvent {
            pid: 100,
            comm: "curl".to_owned(),
            observed_at: Timestamp::UNIX_EPOCH,
            payload,
        };
        let scope = scope_allowing_10_0_0_0_24();

        let reported = observe(&raw, &sensor(), &scope)
            .expect("well-formed payload must parse")
            .expect("destination outside scope must be reported");

        let serialized = serde_json::to_string(&reported).expect("SecurityEvent serializes");
        assert!(!serialized.contains(CANARY));
    }
}
