//! Regelauswertung über eingefrorene Beweise — der erste geschlossene
//! Produktionspfad zwischen Sensor und Regelwerk (Behebung von Bruch 1, Knoten
//! AW2-19).
//!
//! # Wo im Zyklus die Regeln laufen
//! [`report_findings`] wird von [`crate::poll_once`] aufgerufen, **nach**
//! [`harw_dod_sentinel::Sentinel::freeze`] erfolgreich zurückgekehrt ist —
//! also gegen genau das [`harw_dod_signals::SecurityEvidence`], das dieser
//! Zyklus tatsächlich eingefroren hat, nicht gegen den noch veränderlichen
//! Puffer. `now` ist dabei dasselbe `jiff::Timestamp`, das `poll_once` bereits
//! für `poll_all`/`freeze` gelesen hat (`Timestamp::now()` läuft ausschließlich
//! dort, an der einen Kompositionswurzel dieses Binaries) — [`run_rules`]
//! selbst liest nie die Systemuhr, siehe `harw_dod_rules::rule`-Moduldoku.
//!
//! # Warum der Sentinel nichts durchsetzt
//! Dieses Binary ist die **unprivilegierte** Sammelstelle (siehe
//! `crate`-Moduldoku, Abschnitt „Das erste vollständig unprivilegierte
//! Binary"). [`run_rules`] liefert `Finding<`[`harw_dod_rules::RuleChecked`]`>`
//! — zertifiziert, aber noch nicht triagiert. Der nächste Typestate-Schritt,
//! [`harw_dod_rules::triage`], entscheidet über [`harw_dod_rules::Verdict`]
//! (`Confirmed`/`FalsePositive`/`NeedsReview`) und ist eine Bewertung, die
//! dieses Binary bewusst nicht trifft: eine automatische Triage im
//! unprivilegierten Sammler wäre der erste Schritt Richtung Durchsetzung,
//! und Durchsetzung ist nicht seine Aufgabe. Dieses Modul **meldet** jeden
//! zertifizierten Befund stattdessen über den bereits vorhandenen
//! Ausgabeweg dieses Binaries: denselben `Arc<dyn
//! harw_observe::TelemetrySink>` (im Betrieb ein
//! [`harw_observe_file::FileSink`]), über den [`harw_dod_sentinel::metrics`]
//! bereits Sensor-Zähler schreibt, ergänzt um dieselbe `tracing`-Meldung, mit
//! der `main.rs` bereits jedes beobachtete `SecurityEvent` begleitet. Kein
//! zweiter Ausgabeweg entsteht.
//!
//! # Was zwischen einem Befund und einer durchgesetzten Aktion weiterhin fehlt
//! **Das ist Bruch 2, ausdrücklich benannt, nicht gebaut:** ein
//! `Finding<RuleChecked>`, wie dieses Modul es erzeugt, ist weder triagiert
//! noch eskaliert. Bis heute ruft kein Produktionspfad
//! [`harw_dod_rules::triage`] auf, und `harw-warden` (der fertige Empfänger
//! eines `WardenActionRequest`, mit `Action::authorize`) hat keinen Sender.
//! Der fehlende Knoten wäre `harw-dod-escalate` (bereits als Bezeichner in
//! `harw_dod_rules::engine`-Moduldoku referenziert, Knoten AW5-03): ein
//! **privilegierterer** Prozess, der die von diesem unprivilegierten
//! Sentinel gemeldeten `Finding<RuleChecked>` — z. B. aus derselben
//! Telemetriedatei zurückgelesen, oder über einen eigenen, noch zu
//! entwerfenden Kanal — entgegennimmt, triagiert und bei `Verdict::Confirmed`
//! einen `WardenActionRequest` an `harw-warden` sendet. Dieser Sentinel baut
//! diesen Weg absichtlich nicht: er bliebe sonst nicht mehr die
//! unprivilegierte Sammelstelle, die er sein soll.
//!
//! # Das Privilegienbudget dieser Kante
//! `harw-dod-rules` selbst trägt keine erhöhte Fähigkeit — es ist reine
//! Regellogik. Die Kante zieht aber ein erhebliches **transitives**
//! Abhängigkeitsgewicht in dieses unprivilegierte Binary: nachgezählt (ohne
//! `cargo`, durch manuelles Verfolgen jeder `Cargo.toml` im Pfad) fügt
//! `harw-dod-rules` diesem Binary **21 neue Einträge** im Abhängigkeitsbaum
//! hinzu, die vorher nicht erreichbar waren — `harw-dod-rules` selbst plus
//! zwanzig weitere: `harw-sandbox`, `harw-knowledge`, `harw-research`,
//! `harw-code-graph` (bereits vorher über andere Pfade erreichbar, taucht
//! deshalb *nicht* unter den zwanzig auf), sowie — transitiv über
//! `harw-knowledge` → `harw-model-catalog` — `harw-agent-dsl`,
//! `harw-config`, `harw-context`, `harw-job-runtime`, `harw-lens-types`,
//! `harw-model-catalog`, `harw-plan`, `harw-session-store` und die externen
//! Crates `fs4`, `ipnet`, `reqwest`, `secrecy`, `serde_norway`, `tempfile`,
//! `time`, `tokio`, `tracing`. Bemerkenswert: **`reqwest` und `tokio`** —
//! ein vollständiger asynchroner HTTP-Client-Stack — erreichen dieses
//! Binary damit zum ersten Mal, obwohl keine Regel dieser Crate je ein Netz
//! anspricht. Diese Beobachtung wird hier gemeldet, wie vom Arbeitsauftrag
//! verlangt, und **nicht** dadurch aufgelöst, dass diese Kante wieder
//! entfernt wird — die Regeln sind ohne `harw-dod-rules` nicht erreichbar,
//! und das ist genau der Bruch, den dieser Knoten schließt. Ob das
//! Abhängigkeitsgewicht für ein `CapabilityBoundingSet=`-leeres Binary
//! tragbar ist, entscheidet das Abhängigkeits-Gate, nicht dieses Modul.
//!
//! # Ein leerer Regellauf ist kein Fehler
//! [`report_findings`] liefert `0`, wenn keine Regel etwas ausgelöst hat —
//! das ist der Normalfall in den meisten Zyklen, kein degradierter Zustand.
//! Ein degradierter Sensor liefert weniger Beobachtungen an
//! [`harw_dod_rules::rule::RuleContext`], nie andere; der
//! Degradationsautomat in [`harw_dod_sentinel::Sentinel`] bleibt davon
//! vollständig unberührt.
//!
//! # Was dieser Kontext (noch) nicht befüllt
//! [`harw_dod_rules::rule::RuleContext::network_scope`] ist
//! [`harw_authority::NetworkScope::empty`] — dieses Binary hat heute keine
//! `--flag`, die einen erlaubten Netzbereich konfiguriert, also gibt es
//! keinen ehrlichen Wert außer „leer" zu injizieren.
//! [`harw_dod_rules::rule::RuleContext::baselines`] ist ein leerer Schnitt
//! — dieses Binary hält keine Verbindung zu `harw-knowledge`s Baseline-Palast.
//! Beide Lücken bedeuten praktisch: [`harw_dod_rules::EgressFlowRule`] löst
//! nie aus (jedes Ziel gilt als außerhalb eines leeren Bereichs, aber kein
//! hier registrierter Sensor emittiert derzeit `EventKind::EgressFlow`, siehe
//! `sensors`-Moduldoku), [`harw_dod_rules::BaselineDeviationRule`] löst nie
//! aus (keine Baseline, gegen die abgewichen werden könnte). Nur
//! [`harw_dod_rules::StructureDriftRule`] arbeitet mit den tatsächlich
//! registrierten Sensoren (`harw-dod-workspace`, `harw-dod-scanreport`)
//! produktiv. Das ist keine stille Lücke: beide Felder sind oben benannt,
//! zusammen mit der Konsequenz.
//!
//! # Nebenläufigkeit
//! [`report_findings`] läuft ausschließlich im Sammelthread, aus demselben
//! Aufrufkontext wie `poll_once` (siehe `crate`-Moduldoku, Abschnitt „Warum
//! `Sentinel` nicht von einem `Mutex` umschlossen wird"). `sink.record` ist
//! über `TelemetrySink: Send + Sync` aus jedem Thread aufrufbar, wird hier
//! aber nur aus diesem einen Thread heraus benutzt.
//!
//! # Fehler
//! Keine. [`run_rules`] liefert kein `Result`, und diese Funktion meldet nur
//! — sie bricht bei keiner Befundzahl ab.

use harw_dod_rules::rule::RuleContext;
use harw_dod_rules::{FindingKind, run_rules};
use harw_dod_signals::SecurityEvidence;
use harw_observe::{FieldValue, MetricValue, TelemetrySink};
use harw_authority::NetworkScope;
use jiff::Timestamp;

/// Label-Feldname für die auslösende Regel ([`harw_dod_rules::Rule::id`]).
const RULE_LABEL: harw_observe::FieldName = harw_macros::field!("rule_id");
/// Label-Feldname für [`FindingKind`], über [`finding_kind_label`].
const KIND_LABEL: harw_observe::FieldName = harw_macros::field!("kind");

harw_macros::metrics! {
    /// Wie oft eine Regel dieser Crate seit Prozessstart einen Befund
    /// ausgelöst hat, aufgeschlüsselt nach der auslösenden Regel (`rule_id`
    /// — drei mögliche Werte: `"egress-flow"`, `"structure-drift"`,
    /// `"baseline-deviation"`, siehe [`harw_dod_rules::rule::Rule::id`]) und
    /// nach [`FindingKind`] (`kind` — zwei mögliche Werte: `"rule-triggered"`,
    /// `"anomaly"`). Kardinalität deshalb `bounded(6)`: drei Regeln mal zwei
    /// Befundarten, keine weitere Regel dieses Knotens.
    SECURITY_FINDING_TOTAL: counter, unit = count, labels = ["rule_id", "kind"], cardinality = bounded(6),
        name = "harw_sentinel_security_finding_total";
}

/// Ordnet einen [`FindingKind`] seinem Label-Wert für
/// [`SECURITY_FINDING_TOTAL`] zu.
///
/// Erschöpfendes `match` ohne Wildcard: eine dritte Befundart bricht diese
/// Funktion beim Kompilieren, statt die deklarierte Kardinalitätsgrenze
/// still zu verletzen — dasselbe Muster wie
/// `harw_dod_sentinel::metrics::BufferKind::label`.
const fn finding_kind_label(kind: FindingKind) -> &'static str {
    match kind {
        FindingKind::RuleTriggered => "rule-triggered",
        FindingKind::Anomaly => "anomaly",
    }
}

/// Wertet die drei Regeln dieses Knotens gegen ein eingefrorenes
/// [`SecurityEvidence`] aus und meldet jeden zertifizierten Befund.
///
/// # Description
/// Baut einen [`RuleContext`] aus `evidence.samples`/`evidence.events`, einem
/// leeren [`NetworkScope`] und einer leeren Baseline-Liste (siehe Moduldoku,
/// Abschnitt „Was dieser Kontext (noch) nicht befüllt"), ruft
/// [`run_rules`] darauf auf und meldet jeden zurückgelieferten
/// `Finding<RuleChecked>` über `tracing::warn!` (menschenlesbar, mit
/// Zusammenfassung) und [`SECURITY_FINDING_TOTAL`] auf `sink` (maschinell
/// auswertbar, ohne Freitext im Label — siehe
/// `harw_dod_sentinel::metrics`-Moduldoku, Abschnitt „Keine Inhalte in
/// Labels", dessen Auflage dieses Modul für dieselbe Sink-Instanz
/// übernimmt).
///
/// # Arguments
/// - `sink` (`&dyn TelemetrySink`): dasselbe Sink-Objekt, über das dieses
///   Binary bereits Sensor-Metriken schreibt (im Betrieb ein
///   [`harw_observe_file::FileSink`]).
/// - `evidence` (`&SecurityEvidence`): der in diesem Zyklus eingefrorene
///   Beleg, dessen `samples`/`events` gegen die Regeln laufen.
/// - `now` (`jiff::Timestamp`): dieselbe injizierte Zeit, mit der `evidence`
///   eingefroren wurde — nie die Systemuhr dieser Funktion selbst.
///
/// # Returns
/// Die Anzahl gemeldeter Befunde. `0` ist der häufigste, unauffällige Fall
/// (siehe Moduldoku, Abschnitt „Ein leerer Regellauf ist kein Fehler") —
/// kein Fehler.
///
/// # Errors
/// Keine — siehe Moduldoku.
///
/// # Concurrency
/// Siehe Moduldoku.
///
/// # Examples
/// ```rust
/// use harw_dod_signals::SecurityEvidence;
/// use harw_observe::NullSink;
/// use jiff::Timestamp;
///
/// let evidence = SecurityEvidence::capture(vec![], vec![], Timestamp::UNIX_EPOCH)
///     .expect("empty events always encode");
/// let reported = harw_sentinel::findings::report_findings(&NullSink, &evidence, Timestamp::UNIX_EPOCH);
/// assert_eq!(reported, 0);
/// ```
pub fn report_findings(sink: &dyn TelemetrySink, evidence: &SecurityEvidence, now: Timestamp) -> usize {
    let scope = NetworkScope::empty();
    let ctx = RuleContext {
        now,
        samples: &evidence.samples,
        events: &evidence.events,
        baselines: &[],
        network_scope: &scope,
    };

    let findings = run_rules(&ctx);

    for finding in &findings {
        tracing::warn!(
            rule_id = finding.rule_id(),
            kind = finding_kind_label(finding.kind()),
            severity = ?finding.severity(),
            hardness = ?finding.hardness(),
            summary = %finding.summary(),
            "security rule finding"
        );
        sink.record(
            &SECURITY_FINDING_TOTAL,
            MetricValue::Count(1),
            &[
                (RULE_LABEL, FieldValue::Str(finding.rule_id())),
                (KIND_LABEL, FieldValue::Str(finding_kind_label(finding.kind()))),
            ],
        );
    }

    findings.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_dod_signals::{DriftSeverity, EventKind, SecurityEvent};
    use harw_types::SensorId;

    fn structure_drift_event() -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("workspace-drift-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::StructureDrift {
                severity: DriftSeverity::Medium,
                detail: "neues Workspace-Member: harw-example".to_owned(),
            },
        }
    }

    /// Der wichtigste Test dieses Knotens: `run_rules` wird tatsächlich von
    /// einem Produktionspfad aufgerufen (`report_findings`, das
    /// `crate::poll_once` jeden Zyklus aufruft) — nicht nur aus einem
    /// Doctest oder einem Test innerhalb von `harw-dod-rules` selbst.
    #[test]
    fn test_report_findings_calls_run_rules_from_a_production_path() {
        let evidence =
            SecurityEvidence::capture(vec![], vec![structure_drift_event()], Timestamp::UNIX_EPOCH)
                .expect("well-formed content always encodes");

        let reported = report_findings(&harw_observe::NullSink, &evidence, Timestamp::UNIX_EPOCH);

        assert_eq!(reported, 1, "the structure-drift event must trigger exactly one finding");
    }

    #[test]
    fn test_report_findings_with_an_empty_buffer_yields_neither_finding_nor_panic() {
        let evidence = SecurityEvidence::capture(vec![], vec![], Timestamp::UNIX_EPOCH)
            .expect("empty content always encodes");

        let reported = report_findings(&harw_observe::NullSink, &evidence, Timestamp::UNIX_EPOCH);

        assert_eq!(reported, 0, "an empty evidence buffer must report zero findings, not an error");
    }

    #[test]
    fn test_finding_kind_label_is_exhaustive_and_stable() {
        assert_eq!(finding_kind_label(FindingKind::RuleTriggered), "rule-triggered");
        assert_eq!(finding_kind_label(FindingKind::Anomaly), "anomaly");
    }
}
