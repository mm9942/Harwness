//! Der Nullzähler `audit_chain_break`: macht [`ChainAuditMirror`]s internen
//! `chain_break_count` (`crate::audit::mirror`) unter einem benannten
//! Telemetrieschlüssel auffindbar (Knoten AW7-04, Plan `docs/aw-plan.md`).
//!
//! # Warum ein zweiter Zähler zur selben Zahl
//! [`ChainAuditMirror::chain_break_count`](crate::audit::mirror::ChainAuditMirror::chain_break_count)
//! existierte bereits als internes `AtomicU64` — technisch korrekt, aber vom
//! Telemetriesystem getrennt: nichts, was `harw_observe::NullCounterRegistry`
//! aufbaut (die Datenquelle jeder Nullzähler-Leiste, siehe
//! `harw_observe::null_counter`s Moduldoku), konnte ihn finden, weil kein
//! Fundort im Baum den Namen `audit_chain_break` trug. Ein Zähler, den
//! niemand unter einem Namen abfragen kann, ist eine Variable — kein
//! Nullzähler. Diese Datei ist additiv: `chain_break_count` bleibt bestehen
//! und bleibt die Quelle der Wahrheit für [`ChainAuditMirror::verify_and_mirror`];
//! [`AUDIT_CHAIN_BREAK`] spiegelt jede seiner Erhöhungen zusätzlich unter
//! einem Namen, den [`harw_observe::NullCounterRegistry`] registrieren kann.
//!
//! # Was dieser Zähler beobachtet — und was nicht
//! [`AUDIT_CHAIN_BREAK`] steigt in genau dem Moment, in dem
//! [`ChainAuditMirror::verify_and_mirror`](crate::audit::mirror::ChainAuditMirror::verify_and_mirror)
//! einen [`crate::error::AuditError::ChainBroken`] von
//! [`AuditLog::verify`](crate::audit::chain::AuditLog::verify) zurückerhält —
//! der Aufruf selbst ist der Beleg der Verletzung, es gibt keinen
//! zweiten, aufrufer-seitigen Feststellungsschritt (siehe
//! `harw_observe::null_counter`s Moduldoku, Abschnitt „Die Idee des
//! Nullzählers"). Sein erwarteter Wert im Betrieb ist null, weil eine
//! bewiesene, unveränderliche Ereigniskette (§4.2) per Konstruktion keinen
//! Bruch enthalten soll — jede Erhöhung bedeutet, dass entweder das
//! Protokoll selbst manipuliert wurde oder ein Programmfehler eine
//! eigentlich unmögliche Kette erzeugt hat.
//!
//! **Ehrliche Einschränkung, wörtlich aus der Moduldoku von
//! `crate::audit::mirror` übernommen:** dieser Zähler misst, was
//! *gemeldet* wird, nicht was *geschieht*. Er steigt nur, wenn irgendein
//! Aufrufer `verify_and_mirror` tatsächlich aufruft — und Stand dieses
//! Knotens ruft **kein** Scheduler in `harw-cli` oder `harw-job-runtime`
//! ihn periodisch auf; nur die Tests in `crate::audit::mirror` erreichen
//! ihn. Solange das so bleibt, unterscheidet sich „die Kette ist intakt"
//! nicht von „niemand hat je nachgesehen" — beides zeigt sich hier als
//! Stand null (die Falle aus `harw_observe::null_counter`s Moduldoku,
//! Abschnitt „Die Falle"). Und selbst nach Anbindung eines Schedulers bleibt
//! dieser Zähler blind gegenüber einem Host, der vollständig übernommen
//! wurde **einschließlich** seiner Fähigkeit, `verify_and_mirror` gar nicht
//! erst aufzurufen oder dessen Ergebnis zu fälschen — dagegen schützt
//! ausschließlich der hostexterne Meldeweg aus `crate::audit::mirror`
//! selbst, nicht dieser Zähler.
//!
//! # Was niemals in diesen Zähler gerät
//! Kein Label: [`AUDIT_CHAIN_BREAK_KEY`] trägt [`Cardinality::Single`] und
//! keine Labelfelder. Weder ein Schlüssel-, Ereignis- noch Akteursbezug
//! (`actor`, `action`, `subjects` aus [`crate::audit::event::AuditEvent`])
//! erscheint hier — genau wie [`crate::audit::mirror::ChainBreakAlert`],
//! die diese Felder aus demselben Grund ausspart (siehe deren Moduldoku,
//! Abschnitt „Was übertragen wird"), trägt dieser Metrikname und sein
//! Zählstand nur „wie oft", nie „was" oder „wessen".
//!
//! # Nebenläufigkeit
//! [`AUDIT_CHAIN_BREAK`] ist ein `'static` [`harw_observe::NullCounter`] und
//! damit ohne `unsafe impl` `Sync` (siehe dessen Moduldoku); beliebig
//! nebenläufig erhöh- und lesbar.
//!
//! # Examples
//! ```
//! use harw_observe::{NullCounterRegistry, NullSink, assert_all_zero};
//! use harw_secrets::audit::chain::AuditLog;
//! use harw_secrets::audit::event::Actor;
//! use harw_secrets::audit::mirror::{ChainAuditMirror, RecordingMirrorTransport};
//! use harw_secrets::audit::telemetry::AUDIT_CHAIN_BREAK;
//! use jiff::Timestamp;
//!
//! let mut registry = NullCounterRegistry::new();
//! registry.register(&AUDIT_CHAIN_BREAK);
//! assert!(assert_all_zero(&registry).is_ok());
//!
//! let mut log = AuditLog::new();
//! log.append(Actor::System, "secret.access", Vec::new());
//! let mirror = ChainAuditMirror::new(RecordingMirrorTransport::new());
//! mirror
//!     .verify_and_mirror(&log, Timestamp::now(), &NullSink)
//!     .expect("intact chain verifies");
//! assert_eq!(AUDIT_CHAIN_BREAK.count(), 0);
//! ```

use harw_observe::{Cardinality, MetricKey, MetricKind, NullCounter, Unit};

/// Metrikschlüssel des Nullzählers [`AUDIT_CHAIN_BREAK`].
///
/// Trägt [`Cardinality::Single`] und keine Labelfelder (§ Moduldoku „Was
/// niemals in diesen Zähler gerät") — eine Kennung mit unbegrenztem
/// Wertebereich (z. B. ein `SecretId` oder `AuditEventId`) gehört nicht in
/// ein Metriklabel mit deklarierter Kardinalität.
const AUDIT_CHAIN_BREAK_KEY: MetricKey = MetricKey {
    name: "audit_chain_break",
    kind: MetricKind::Counter,
    unit: Unit::Count,
    labels: &[],
    cardinality: Cardinality::Single,
};

/// Der Nullzähler dieses Knotens (AW7-04): wie oft
/// [`crate::audit::mirror::ChainAuditMirror::verify_and_mirror`] seit
/// Prozessstart einen Kettenbruch der Audit-Kette festgestellt hat.
/// Erwarteter Wert im Betrieb: **null**. Siehe die Moduldoku für den Pfad,
/// den dieser Zähler beobachtet, und die ehrliche Einschränkung, was er
/// nicht beweisen kann.
pub static AUDIT_CHAIN_BREAK: NullCounter = NullCounter::new(
    &AUDIT_CHAIN_BREAK_KEY,
    "keine bereits angehängte Audit-Ereigniskette wird nachträglich manipuliert (Kettenbruch, den crate::audit::chain::AuditLog::verify feststellt)",
);

/// Serialisiert alle Tests (in diesem Modul und in `crate::audit::mirror`),
/// die den prozessweiten [`AUDIT_CHAIN_BREAK`] lesen oder erhöhen — derselbe
/// Grund, aus dem `harw-core/src/context_budget.rs` seinen
/// `TRUST_BLOCK_VIOLATION`-Test hinter einem eigenen Lock serialisiert: ein
/// `static NullCounter` ist prozessweit geteilt, und `cargo test` läuft
/// standardmäßig mit mehreren Threads im selben Prozess. `pub(crate)`, damit
/// `crate::audit::mirror`s Tests, die denselben Zähler über
/// [`crate::audit::mirror::ChainAuditMirror::verify_and_mirror`] erhöhen,
/// denselben Lock nehmen können.
#[cfg(test)]
pub(crate) static AUDIT_COUNTER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::chain::AuditLog;
    use crate::audit::event::{Actor, SubjectRef};
    use crate::audit::mirror::{ChainAuditMirror, RecordingMirrorTransport};
    use crate::test_support::{TestResult, ctx};
    use harw_observe::{NullCounterRegistry, NullSink, TelemetrySink};
    use jiff::Timestamp;
    use std::sync::Mutex;

    fn ts(seconds: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(seconds).map_err(ctx("valid test timestamp"))
    }

    #[derive(Debug, Default)]
    struct RecordingSink {
        calls: Mutex<usize>,
    }

    impl TelemetrySink for RecordingSink {
        fn record(
            &self,
            key: &MetricKey,
            _value: harw_observe::MetricValue,
            _labels: &[(harw_observe::FieldName, harw_observe::FieldValue)],
        ) {
            assert_eq!(key.name, "audit_chain_break");
            *self.calls.lock().unwrap_or_else(|p| p.into_inner()) += 1;
        }

        fn flush(&self) {}

        fn name(&self) -> &'static str {
            "recording"
        }
    }

    fn broken_log() -> AuditLog {
        let mut log = AuditLog::new();
        log.append(Actor::System, "secret.access", Vec::new());
        log.append(
            Actor::Operator("operator-jane".to_owned()),
            "secret.rotate",
            vec![SubjectRef::new("secret", "prod-token")],
        );

        let good_first = log.events()[0].clone();
        let mut tampered_second = log.events()[1].clone();
        tampered_second.prev_hash[0] ^= 0xFF;

        AuditLog::from_raw_events_for_test(vec![good_first, tampered_second])
    }

    /// Der wichtigste Test dieses Knotens: der Zähler ist unter dem Namen
    /// `audit_chain_break` auffindbar — über [`NullCounterRegistry`], die
    /// Datenquelle jeder Nullzähler-Leiste.
    #[test]
    fn test_audit_chain_break_is_findable_by_name_in_registry() {
        let mut registry = NullCounterRegistry::new();
        registry.register(&AUDIT_CHAIN_BREAK);

        assert_eq!(registry.snapshot()[0].0, "audit_chain_break");
        assert!(!AUDIT_CHAIN_BREAK.invariant().is_empty());
    }

    #[test]
    fn test_intact_chain_section_leaves_audit_chain_break_at_zero() -> TestResult {
        let _guard = AUDIT_COUNTER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = AUDIT_CHAIN_BREAK.count();

        let mut log = AuditLog::new();
        log.append(Actor::System, "secret.access", Vec::new());
        let mirror = ChainAuditMirror::new(RecordingMirrorTransport::new());

        let outcome = mirror.verify_and_mirror(&log, ts(700)?, &NullSink);

        assert!(outcome.is_ok());
        assert_eq!(AUDIT_CHAIN_BREAK.count(), before);
        Ok(())
    }

    #[test]
    fn test_tampered_chain_section_increments_audit_chain_break_and_records() -> TestResult {
        let _guard = AUDIT_COUNTER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = AUDIT_CHAIN_BREAK.count();

        let log = broken_log();
        let mirror = ChainAuditMirror::new(RecordingMirrorTransport::new());
        let sink = RecordingSink::default();

        let outcome = mirror.verify_and_mirror(&log, ts(800)?, &sink);

        assert!(outcome.is_err());
        assert_eq!(AUDIT_CHAIN_BREAK.count(), before + 1);
        assert_eq!(*sink.calls.lock().unwrap_or_else(|p| p.into_inner()), 1);
        Ok(())
    }

    /// Kein Schlüsselmaterial und kein Protokollinhalt darf in Name, Label
    /// oder Invariantentext dieses Zählers erscheinen.
    #[test]
    fn test_no_secret_material_or_event_content_in_name_label_or_invariant() {
        assert_eq!(AUDIT_CHAIN_BREAK.name(), "audit_chain_break");
        assert!(AUDIT_CHAIN_BREAK_KEY.labels.is_empty());
        let invariant = AUDIT_CHAIN_BREAK.invariant();
        assert!(!invariant.contains("operator-jane"));
        assert!(!invariant.contains("secret.rotate"));
        assert!(!invariant.contains("prod-token"));
    }
}
