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
//! dort, an der einen Kompositionswurzel dieses Binaries) —
//! [`run_rules_checked`] selbst liest nie die Systemuhr, siehe
//! `harw_dod_rules::rule`-Moduldoku.
//!
//! # Warum der Sentinel nichts durchsetzt
//! Dieses Binary ist die **unprivilegierte** Sammelstelle (siehe
//! `crate`-Moduldoku, Abschnitt „Das erste vollständig unprivilegierte
//! Binary"). [`run_rules_checked`] liefert
//! `Finding<`[`harw_dod_rules::RuleChecked`]`>` — zertifiziert (je Befund eine
//! frische `FindingId`), aber noch nicht triagiert. Der nächste Typestate-Schritt,
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
//! produktiv — und zwar ausschließlich über deren
//! `EventKind::StructureDrift`-Ereignisse; ihr Sample-Zweig („neue Metrik
//! ohne Baseline") schweigt ohne jede Baseline bewusst. Das ist keine stille Lücke: beide Felder sind oben benannt,
//! zusammen mit der Konsequenz.
//!
//! # Cgroup attribution of a finding (PL-97 E4)
//! A `Finding<RuleChecked>` does not name the event it came from. The
//! export's optional `scope` is therefore derived without touching the
//! rule engine: the rules run once over the whole evidence (these findings
//! are the ones reported, unchanged), and once more per resolved cgroup
//! over only the events whose actor carries that cgroup path (see
//! `crate::cgroup_resolve`). A finding of the full run gets a scope only
//! when its signature (rule, kind, severity, hardness, `observed_at` and
//! summary) is unambiguous: the full run produces it `k` times and the
//! per-cgroup runs produce it exactly `k` times, all under one cgroup path.
//! A signature that two cgroups produce, or that also stems from samples,
//! from events without a resolved cgroup, or from a rule that combines
//! events of several cgroups, gets no scope for any of its findings.
//! Attribution never guesses.
//!
//! # Nebenläufigkeit
//! [`report_findings`] läuft ausschließlich im Sammelthread, aus demselben
//! Aufrufkontext wie `poll_once` (siehe `crate`-Moduldoku, Abschnitt „Warum
//! `Sentinel` nicht von einem `Mutex` umschlossen wird"). `sink.record` ist
//! über `TelemetrySink: Send + Sync` aus jedem Thread aufrufbar, wird hier
//! aber nur aus diesem einen Thread heraus benutzt.
//!
//! # Fehler
//! Keine. [`run_rules_checked`] liefert kein `Result`, und diese Funktion
//! meldet nur
//! — sie bricht bei keiner Befundzahl ab.

use harw_authority::NetworkScope;
use harw_dod_rules::rule::RuleContext;
use harw_dod_rules::{FindingKind, run_rules, run_rules_checked};
use harw_dod_signals::{SecurityEvent, SecurityEvidence, Severity};
use harw_observe::{FieldValue, MetricValue, TelemetrySink};
use jiff::Timestamp;

use crate::export::FindingsExporter;

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
/// [`run_rules_checked`] darauf auf und meldet jeden zurückgelieferten
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
/// - `exporter` (`Option<&mut crate::export::FindingsExporter>`): der
///   optionale JSON-Lines-Export (`--findings-export`); `None` schreibt
///   nichts. Ein Exportfehler wird dort gezählt und geloggt und ändert
///   weder Rückgabewert noch Meldung.
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
/// let reported =
///     harw_sentinel::findings::report_findings(&NullSink, &evidence, Timestamp::UNIX_EPOCH, None);
/// assert_eq!(reported, 0);
/// ```
pub fn report_findings(
    sink: &dyn TelemetrySink,
    evidence: &SecurityEvidence,
    now: Timestamp,
    mut exporter: Option<&mut FindingsExporter>,
) -> usize {
    let scope = NetworkScope::empty();
    let ctx = RuleContext {
        now,
        samples: &evidence.samples,
        events: &evidence.events,
        baselines: &[],
        network_scope: &scope,
    };

    let findings = run_rules_checked(&ctx);
    let scopes = attribute_scopes(&findings, &evidence.events, now, &scope);

    for (finding, finding_scope) in findings.iter().zip(&scopes) {
        tracing::warn!(
            finding_id = finding.id().as_str(),
            rule_id = finding.rule_id(),
            kind = finding_kind_label(finding.kind()),
            severity = ?finding.severity(),
            hardness = ?finding.hardness(),
            summary = %finding.summary(),
            scope = finding_scope.as_deref().unwrap_or(""),
            "security rule finding"
        );
        sink.record(
            &SECURITY_FINDING_TOTAL,
            MetricValue::Count(1),
            &[
                (RULE_LABEL, FieldValue::Str(finding.rule_id())),
                (
                    KIND_LABEL,
                    FieldValue::Str(finding_kind_label(finding.kind())),
                ),
            ],
        );
        if let Some(exporter) = exporter.as_deref_mut() {
            exporter.export(sink, finding, finding_scope.as_deref());
        }
    }

    findings.len()
}

/// What makes two findings of different rule runs the same finding.
type FindingSignature = (
    &'static str,
    FindingKind,
    Severity,
    harw_dod_signals::Hardness,
    Timestamp,
    String,
);

/// The [`FindingSignature`] of one finding of either rule run.
macro_rules! signature_of {
    ($finding:expr) => {{
        let finding = $finding;
        (
            finding.rule_id(),
            finding.kind(),
            finding.severity(),
            finding.hardness(),
            finding.observed_at(),
            finding.summary().to_owned(),
        )
    }};
}

/// Derives the optional cgroup `scope` of each finding (see the module
/// documentation, "Cgroup attribution of a finding").
///
/// # Description
/// A signature that occurs `k` times in the full run is attributed only
/// when the per-cgroup runs produce it exactly `k` times, all under one
/// cgroup path; then every one of those `k` findings gets that path.
/// Any other count or more than one path makes the signature ambiguous,
/// and every finding with it gets `None`.
///
/// # Returns
/// One entry per element of `findings`, in the same order.
fn attribute_scopes(
    findings: &[harw_dod_rules::Finding<harw_dod_rules::RuleChecked>],
    events: &[SecurityEvent],
    now: Timestamp,
    network_scope: &NetworkScope,
) -> Vec<Option<String>> {
    let mut groups: std::collections::BTreeMap<&str, Vec<SecurityEvent>> =
        std::collections::BTreeMap::new();
    for event in events {
        let cgroup = event
            .actor
            .as_ref()
            .and_then(|actor| actor.cgroup.as_ref())
            .map(harw_types::CgroupId::as_str)
            .filter(|path| path.starts_with('/'));
        if let Some(path) = cgroup {
            groups.entry(path).or_default().push(event.clone());
        }
    }
    if groups.is_empty() {
        return vec![None; findings.len()];
    }

    let mut pool: Vec<(FindingSignature, &str)> = Vec::new();
    for (path, group_events) in &groups {
        let ctx = RuleContext {
            now,
            samples: &[],
            events: group_events,
            baselines: &[],
            network_scope,
        };
        for raw in run_rules(&ctx) {
            pool.push((signature_of!(&raw), path));
        }
    }

    let signatures: Vec<FindingSignature> = findings
        .iter()
        .map(|finding| signature_of!(finding))
        .collect();
    signatures
        .iter()
        .map(|signature| {
            let in_full_run = signatures
                .iter()
                .filter(|other| *other == signature)
                .count();
            let mut matches = pool
                .iter()
                .filter(|(candidate, _)| candidate == signature)
                .map(|(_, path)| *path);
            let first = matches.next()?;
            let mut in_pool = 1_usize;
            for path in matches {
                if path != first {
                    return None;
                }
                in_pool += 1;
            }
            (in_pool == in_full_run).then(|| first.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
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

    /// Der wichtigste Test dieses Knotens: die Regeln (`run_rules_checked`)
    /// werden tatsächlich von einem Produktionspfad aufgerufen (`report_findings`, das
    /// `crate::poll_once` jeden Zyklus aufruft) — nicht nur aus einem
    /// Doctest oder einem Test innerhalb von `harw-dod-rules` selbst.
    #[test]
    fn test_report_findings_calls_run_rules_from_a_production_path() -> TestResult {
        let evidence =
            SecurityEvidence::capture(vec![], vec![structure_drift_event()], Timestamp::UNIX_EPOCH)
                .map_err(ctx("well-formed content always encodes"))?;

        let reported = report_findings(
            &harw_observe::NullSink,
            &evidence,
            Timestamp::UNIX_EPOCH,
            None,
        );

        assert_eq!(
            reported, 1,
            "the structure-drift event must trigger exactly one finding"
        );
        Ok(())
    }

    #[test]
    fn test_report_findings_with_an_empty_buffer_yields_neither_finding_nor_panic() -> TestResult {
        let evidence = SecurityEvidence::capture(vec![], vec![], Timestamp::UNIX_EPOCH)
            .map_err(ctx("empty content always encodes"))?;

        let reported = report_findings(
            &harw_observe::NullSink,
            &evidence,
            Timestamp::UNIX_EPOCH,
            None,
        );

        assert_eq!(
            reported, 0,
            "an empty evidence buffer must report zero findings, not an error"
        );
        Ok(())
    }

    #[test]
    fn test_report_findings_writes_one_export_line_per_finding() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        let host = harw_types::HostId::try_from_str("host-a").map_err(ctx("host id"))?;
        let mut exporter = FindingsExporter::new(path.clone(), host);
        let evidence =
            SecurityEvidence::capture(vec![], vec![structure_drift_event()], Timestamp::UNIX_EPOCH)
                .map_err(ctx("well-formed content always encodes"))?;

        let reported = report_findings(
            &harw_observe::NullSink,
            &evidence,
            Timestamp::UNIX_EPOCH,
            Some(&mut exporter),
        );

        assert_eq!(reported, 1);
        assert_eq!(exporter.written(), 1);
        let text = std::fs::read_to_string(&path).map_err(ctx("read export"))?;
        let value: serde_json::Value =
            serde_json::from_str(text.trim_end()).map_err(ctx("json line"))?;
        assert_eq!(value["rule_id"], "structure-drift");
        assert_eq!(value["host"], "host-a");
        Ok(())
    }

    fn drift_in(cgroup: Option<&str>, detail: &str) -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("probe-bpf-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: Some(harw_dod_signals::Actor {
                uid: 1000,
                auid: None,
                cgroup: cgroup.map(harw_types::CgroupId::from_str),
            }),
            kind: EventKind::StructureDrift {
                severity: DriftSeverity::Medium,
                detail: detail.to_owned(),
            },
        }
    }

    #[test]
    fn test_findings_carry_the_scope_of_their_resolved_cgroup_only() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        let host = harw_types::HostId::try_from_str("host-a").map_err(ctx("host id"))?;
        let mut exporter = FindingsExporter::new(path.clone(), host);
        let events = vec![
            drift_in(Some("/sys/fs/cgroup/harw.slice/harw-job-1"), "job one"),
            drift_in(None, "unattributed"),
            // Ein nicht aufgelöster Rohwert ist kein Pfad und bekommt keinen Scope.
            drift_in(Some("cgroup-id:42"), "raw id"),
            drift_in(Some("/sys/fs/cgroup/harw.slice/harw-job-2"), "job two"),
        ];
        let evidence = SecurityEvidence::capture(vec![], events, Timestamp::UNIX_EPOCH)
            .map_err(ctx("well-formed content always encodes"))?;

        let reported = report_findings(
            &harw_observe::NullSink,
            &evidence,
            Timestamp::UNIX_EPOCH,
            Some(&mut exporter),
        );
        assert_eq!(reported, 4);

        let text = std::fs::read_to_string(&path).map_err(ctx("read export"))?;
        let mut scopes = Vec::new();
        for line in text.lines() {
            let value: serde_json::Value = serde_json::from_str(line).map_err(ctx("json line"))?;
            assert_eq!(value["schema_version"], 2);
            scopes.push((
                value["summary"].as_str().map(str::to_owned),
                value
                    .get("scope")
                    .and_then(|s| s.as_str())
                    .map(str::to_owned),
            ));
        }
        let expect = |summary: &str, scope: Option<&str>| {
            (Some(summary.to_owned()), scope.map(str::to_owned))
        };
        assert_eq!(
            scopes,
            vec![
                expect("job one", Some("/sys/fs/cgroup/harw.slice/harw-job-1")),
                expect("unattributed", None),
                expect("raw id", None),
                expect("job two", Some("/sys/fs/cgroup/harw.slice/harw-job-2")),
            ]
        );
        Ok(())
    }

    fn scopes_of(events: Vec<SecurityEvent>) -> TestResult<Vec<Option<String>>> {
        let evidence = SecurityEvidence::capture(vec![], events.clone(), Timestamp::UNIX_EPOCH)
            .map_err(ctx("well-formed content always encodes"))?;
        let scope = NetworkScope::empty();
        let ctx_full = RuleContext {
            now: Timestamp::UNIX_EPOCH,
            samples: &evidence.samples,
            events: &evidence.events,
            baselines: &[],
            network_scope: &scope,
        };
        let findings = run_rules_checked(&ctx_full);
        Ok(attribute_scopes(
            &findings,
            &events,
            Timestamp::UNIX_EPOCH,
            &scope,
        ))
    }

    #[test]
    fn test_identical_findings_with_an_unattributed_source_get_no_scope() -> TestResult {
        let scopes = scopes_of(vec![
            drift_in(None, "same"),
            drift_in(Some("/sys/fs/cgroup/harw-job-1"), "same"),
        ])?;
        assert_eq!(
            scopes,
            vec![None, None],
            "which of the identical findings stems from the cgroup is unknown"
        );
        Ok(())
    }

    #[test]
    fn test_identical_findings_from_two_cgroups_get_no_scope() -> TestResult {
        let scopes = scopes_of(vec![
            drift_in(Some("/sys/fs/cgroup/harw-job-2"), "same"),
            drift_in(Some("/sys/fs/cgroup/harw-job-1"), "same"),
            drift_in(Some("/sys/fs/cgroup/harw-job-1"), "distinct"),
        ])?;
        assert_eq!(
            scopes,
            vec![None, None, Some("/sys/fs/cgroup/harw-job-1".to_owned())],
            "a signature two cgroups produce is ambiguous; a unique one is not"
        );
        Ok(())
    }

    #[test]
    fn test_identical_findings_from_one_cgroup_all_get_its_scope() -> TestResult {
        let job = "/sys/fs/cgroup/harw-job-1";
        let scopes = scopes_of(vec![
            drift_in(Some(job), "same"),
            drift_in(Some(job), "same"),
        ])?;
        assert_eq!(scopes, vec![Some(job.to_owned()), Some(job.to_owned())]);
        Ok(())
    }

    #[test]
    fn test_finding_kind_label_is_exhaustive_and_stable() {
        assert_eq!(
            finding_kind_label(FindingKind::RuleTriggered),
            "rule-triggered"
        );
        assert_eq!(finding_kind_label(FindingKind::Anomaly), "anomaly");
    }
}
