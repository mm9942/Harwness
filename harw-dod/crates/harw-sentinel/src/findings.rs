//! Regelauswertung über eingefrorene Beweise — der erste geschlossene
//! Produktionspfad zwischen Sensor und Regelwerk (Behebung von Bruch 1, Knoten
//! AW2-19).
//!
//! # Wo im Zyklus die Regeln laufen
//! [`crate::poll_once`] ruft je Zyklus [`RuleCycle::evaluate`] auf, nachdem
//! `poll_all` und die IPC-Inbox den Puffer befüllt haben. `evaluate` friert
//! über [`harw_dod_sentinel::EvidenceBuffer::freeze_since`] nur das **Delta**
//! seit dem vorigen Zyklus ein und übergibt genau dieses
//! [`harw_dod_signals::SecurityEvidence`] an [`report_findings`] — nicht den
//! noch veränderlichen Puffer und nicht den ganzen Ring. `now` ist dabei
//! dasselbe `jiff::Timestamp`, das `poll_once` bereits für `poll_all` gelesen
//! hat (`Timestamp::now()` läuft ausschließlich dort, an der einen
//! Kompositionswurzel dieses Binaries) — [`run_rules_checked`] selbst liest
//! nie die Systemuhr, siehe `harw_dod_rules::rule`-Moduldoku.
//!
//! # Delta-Auswertung statt Befund-Flut (PL-100 WP0.1)
//! Der Ring ([`harw_dod_sentinel::EvidenceBuffer`]) wird nie geleert; er ist
//! der Kontextspeicher, dessen vollen Inhalt `freeze()` weiterhin liefert.
//! Früher lief jeder Zyklus über den ganzen Ring, und jedes Ereignis wurde
//! so lange in jedem Zyklus erneut zu einem Befund mit neuer `FindingId`,
//! wie es im Ring lag. [`RuleCycle`] hält stattdessen die
//! [`harw_dod_sentinel::BufferMark`] des vorigen Zyklus: jedes Ereignis und
//! jedes Sample läuft genau einmal durch die Regeln, egal wie viele Zyklen
//! der Ring es noch hält. Verdrängt der Ring Einträge, bevor sie ausgewertet
//! wurden (mehr Ankünfte in einem Zyklus, als er fasst), meldet `evaluate`
//! deren Anzahl per `tracing::warn!` (nur Zähler, keine Inhalte), statt sie
//! still zu verlieren. Die Attribution (siehe unten) läuft über dasselbe
//! Delta.
//!
//! # Entprellung wiederholter Befunde (PL-100 WP0.4)
//! Auch über das Delta erzeugt ein lauter Dienst viele gleichartige Befunde.
//! [`report_findings`] bestimmt deshalb je Befund seine Herkunft
//! ([`FindingOrigin`]: `uid`, `cgroup_kid`, grobes Subjekt des auslösenden
//! Ereignisses) und daraus den `dedup_key`
//! ([`crate::throttle::DedupKey`]). Mit einem
//! [`crate::throttle::FindingThrottle`] (im Betrieb immer,
//! [`RuleCycle::with_throttle`]) wird eine Wiederholung innerhalb der
//! Abklingzeit nur gezählt (`occurrences`, Metrik
//! [`SECURITY_FINDING_SUPPRESSED_TOTAL`], `debug`) und weder ins Journal
//! (`warn`) noch in den Export geschrieben; siehe `crate::throttle` für
//! Abklingzeit, echte Änderungen und den gespeicherten Zustand. Die
//! Herkunft wird wie die Cgroup-Zuordnung (unten) ohne Eingriff ins
//! Regelwerk bestimmt: die Regeln laufen zusätzlich je **einzelnem**
//! Ereignis des Deltas; ein Befund des vollen Laufs übernimmt die Merkmale,
//! die alle Einzelläufe mit seiner Signatur gemeinsam haben. Stammt nicht
//! jeder Befund dieser Signatur aus einem Einzelereignis, bleiben `uid` und
//! `cgroup_kid` leer; ohne jedes Einzelereignis ist das Subjekt der Digest
//! der Zusammenfassung. Die Kernel-ID einer Cgroup stammt aus der
//! [`crate::cgroup_resolve::CgroupKids`] des Zyklus, in dem die Sonde sie
//! geliefert hat.
//!
//! # Spool-Record und Export v3 (PL-100 WP0.12)
//! Dieselben Einzelläufe liefern je Befund seine **auslösenden Ereignisse**
//! (Indizes im Delta, höchstens [`MAX_TRIGGER_EVENTS`]): die Ereignisse,
//! deren Einzellauf die Signatur des Befundes erzeugt. Mit einem
//! Spool-Schreiber ([`RuleCycle::with_spool`], im Betrieb mit
//! `--findings-export`) wird jeder **gemeldete** Befund über
//! [`harw_dod_sentinel::spool::slice_record`] mit diesen Ereignissen plus
//! höchstens [`harw_dod_sentinel::spool::MAX_CONTEXT_EVENTS`] jüngsten
//! übrigen Ereignissen des Rings eingefroren (≤ 64 KiB) und an
//! [`crate::spool_writer::SpoolWriter`] übergeben; nur ein eingereihter
//! Record bekommt seinen `record_digest` in die Exportzeile. Die Zeile
//! trägt außerdem den Throttle-Stempel, `cgroup_kid`, Art und Härte und
//! eine [`crate::export::EvidenceCard`] der auslösenden Ereignisse (siehe
//! `crate::export`, Schema v3). Unterdrückte Wiederholungen erzeugen weder
//! Record noch Zeile.
//!
//! # Journal-Hygiene (PL-100 WP0.6)
//! Auf `warn` stehen je gemeldetem Befund nur Kennungen und Aufzählungen:
//! `finding_id`, `rule_id`, `kind`, `severity`, `hardness`, `dedup_key`,
//! `occurrences`, `first_seen` und der Meldegrund. Zusammenfassung, Scope
//! (Pfad), `uid` und `cgroup_kid` — also Pfade, IP-Adressen und anderer
//! erzeugerbestimmter Text — stehen nur auf `debug`, die Zusammenfassung
//! dort zusätzlich `Debug`-escapt. Ein Angreifer kann so weder Steuerzeichen
//! noch Ziele ins Journal schreiben, und das Journal verrät keine
//! beobachteten Ziele.
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
//! # Der Netzbereich kommt aus der DoD-Konfiguration (PL-100 WP0.3)
//! [`harw_dod_rules::rule::RuleContext::network_scope`] ist der aus
//! `egress_allow_cidrs` des aktiven Profils der vertrauten
//! Systemkonfiguration (`/etc/harw-dod/config.toml`) gebaute
//! [`harw_authority::NetworkScope`] ([`network_scope_from_profile`]: je CIDR
//! ein [`harw_authority::EgressTarget::Cidr`]). Nur wenn kein Profil geladen
//! ist oder die Liste leer ist, bleibt er [`harw_authority::NetworkScope::empty`]
//! — dann liegt jedes beobachtete Ziel außerhalb des erlaubten Bereichs, und
//! [`harw_dod_rules::EgressFlowRule`] meldet jeden ausgehenden Fluss, den die
//! BPF-Sonde (`EventKind::EgressFlow`) über die IPC-Inbox liefert. `run`
//! warnt beim Start deutlich, wenn die Liste leer ist.
//!
//! # Was dieser Kontext (noch) nicht befüllt
//! [`harw_dod_rules::rule::RuleContext::baselines`] ist ein leerer Schnitt
//! — dieses Binary hält keine Verbindung zu `harw-knowledge`s Baseline-Palast.
//! [`harw_dod_rules::BaselineDeviationRule`] löst deshalb nie aus (keine
//! Baseline, gegen die abgewichen werden könnte), und der Sample-Zweig von
//! [`harw_dod_rules::StructureDriftRule`] („neue Metrik ohne Baseline")
//! schweigt bewusst; ihr Ereignis-Zweig arbeitet mit den registrierten
//! Sensoren (`harw-dod-workspace`, `harw-dod-scanreport`) produktiv. Das ist
//! keine stille Lücke: das Feld ist hier benannt, zusammen mit der
//! Konsequenz.
//!
//! # Zähler → Raten vor den Regeln (PL-100 WP0.14)
//! cpu, netcounters und blockio melden kumulative Zähler. [`RuleCycle`]
//! hält deshalb eine [`crate::rates::RateStage`]: `evaluate` leitet aus den
//! Samples jedes Deltas, bevor die Regeln laufen, je kumulativem Zähler eine
//! Rate je Sekunde ab (`"<metric>.rate"`, erste Beobachtung und Zählerreset
//! ohne Rate), und [`report_findings`] gibt den Regeln die rohen Samples
//! **und** diese Raten als `RuleContext::samples`. Die Attribution (unten)
//! läuft nur über Ereignisse und bleibt davon unberührt. Solange
//! `baselines` leer ist (vorheriger Abschnitt), wertet keine Regel die Raten
//! aus; sie erzeugen also keinen Befund. Siehe `crate::rates` für die
//! kumulativen Metriken, die Reset-Behandlung und die Obergrenze des
//! Zustands.
//!
//! # Cgroup attribution of a finding (PL-97 E4)
//! A `Finding<RuleChecked>` does not name the event it came from. The
//! export's optional `scope` is therefore derived without touching the
//! rule engine: the rules run once over the cycle's delta evidence (these findings
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

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use harw_authority::{EgressTarget, NetworkScope};
use harw_dod_config::ResolvedObservationProfile;
use harw_dod_rules::rule::RuleContext;
use harw_dod_rules::{FindingKind, run_rules, run_rules_checked};
use harw_dod_sentinel::spool::{MAX_CONTEXT_EVENTS, slice_record};
use harw_dod_sentinel::{BufferMark, EvidenceBuffer};
use harw_dod_signals::{
    EventKind, Hardness, HostSample, SecurityEvent, SecurityEvidence, Severity,
};
use harw_observe::{FieldValue, MetricValue, TelemetrySink};
use harw_types::ContentDigest;
use jiff::Timestamp;

use crate::cgroup_resolve::CgroupKids;
use crate::export::{EvidenceCard, FindingsExporter, LineFields};
use crate::health_export::RingCounters;
use crate::rates::{RateStage, with_rates};
use crate::spool_writer::SpoolWriter;
use crate::throttle::{
    CoarseSubject, DedupKey, FindingThrottle, ThrottleDecision, ThrottleVerdict,
};

/// Höchstzahl gemerkter auslösender Ereignisse je Befund (Spool-Record und
/// `evidence_card`); weitere gleichartige Ereignisse zählen nur noch in die
/// Herkunft.
pub const MAX_TRIGGER_EVENTS: usize = 64;

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
    /// Wie oft ein Befund seit Prozessstart innerhalb seiner Abklingzeit nur
    /// gezählt statt gemeldet wurde (PL-100 WP0.4, `crate::throttle`).
    /// [`SECURITY_FINDING_TOTAL`] zählt jeden Regeltreffer, auch diese.
    SECURITY_FINDING_SUPPRESSED_TOTAL: counter, unit = count, labels = [], cardinality = single,
        name = "harw_sentinel_security_finding_suppressed_total";
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

/// Stabile Kurzform der Art eines Sicherheitsereignisses — für Zählungen im
/// Journal statt des Ereignisinhalts (PL-100 WP0.6).
///
/// Erschöpfendes `match` ohne Wildcard: eine neue Ereignisart bricht den
/// Build, statt unter falschem Namen gezählt zu werden.
#[must_use]
pub const fn event_kind_label(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::ProcessExec { .. } => "process-exec",
        EventKind::FileWrite { .. } => "file-write",
        EventKind::EgressFlow { .. } => "egress-flow",
        EventKind::ListenerOpened { .. } => "listener-opened",
        EventKind::AuthEvent { .. } => "auth-event",
        EventKind::StructureDrift { .. } => "structure-drift",
        EventKind::SensorDegraded { .. } => "sensor-degraded",
    }
}

/// Zählt `events` je [`event_kind_label`] — der Journal-Ersatz für den
/// vollständigen Ereignisinhalt (`crate::poll_once`, `debug`).
///
/// # Returns
/// Höchstens sieben Einträge (eine je Ereignisart), sortiert nach Label.
#[must_use]
pub fn event_kind_counts(events: &[SecurityEvent]) -> BTreeMap<&'static str, u64> {
    let mut counts = BTreeMap::new();
    for event in events {
        let count: &mut u64 = counts.entry(event_kind_label(&event.kind)).or_insert(0);
        *count = count.saturating_add(1);
    }
    counts
}

/// Herkunft eines Befundes im Delta (PL-100 WP0.4): wer ihn ausgelöst hat
/// und worüber.
///
/// # Description
/// Bestimmt in [`report_findings`] (siehe Moduldoku, Abschnitt
/// „Entprellung wiederholter Befunde"). Jedes Feld ist nur gesetzt, wenn es
/// für **alle** Ereignisse mit der Signatur des Befundes gleich ist — nie
/// geraten.
///
/// # Concurrency
/// Reiner Werttyp, `Send + Sync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingOrigin {
    /// `Actor::uid` des auslösenden Ereignisses.
    pub uid: Option<u32>,
    /// Kernel-ID der Cgroup des auslösenden Ereignisses (`cgroup_kid`).
    pub cgroup_kid: Option<u64>,
    /// Das grobe Subjekt für den `dedup_key`.
    pub subject: CoarseSubject,
}

impl FindingOrigin {
    /// Die Herkunft, die ein einzelnes auslösendes Ereignis beiträgt.
    ///
    /// `cgroup_kid` stammt aus `cgroup_kids` (aufgelöster Pfad des Zyklus)
    /// oder, für einen noch nicht aufgelösten Akteur, aus seiner rohen
    /// Kernel-ID.
    fn of_event(event: &SecurityEvent, cgroup_kids: &CgroupKids) -> Self {
        let actor = event.actor.as_ref();
        let cgroup_kid = actor.and_then(|actor| {
            actor
                .cgroup
                .as_ref()
                .and_then(|cgroup| cgroup_kids.kid_of(cgroup.as_str()))
                .or_else(|| actor.raw_cgroup_id())
        });
        Self {
            uid: actor.map(|actor| actor.uid),
            cgroup_kid,
            subject: CoarseSubject::of_event(event),
        }
    }

    /// Die Herkunft eines Befundes ohne zuordenbares Einzelereignis.
    fn unattributed(summary: &str) -> Self {
        Self {
            uid: None,
            cgroup_kid: None,
            subject: CoarseSubject::of_summary(summary),
        }
    }

    /// Behält nur, was `self` und `other` gemeinsam haben; ein
    /// abweichendes Subjekt fällt auf den Digest von `summary` zurück.
    fn merge(&mut self, other: &Self, summary: &str) {
        if self.uid != other.uid {
            self.uid = None;
        }
        if self.cgroup_kid != other.cgroup_kid {
            self.cgroup_kid = None;
        }
        if self.subject != other.subject {
            self.subject = CoarseSubject::of_summary(summary);
        }
    }

    /// Der `dedup_key` eines Befundes der Regel `rule_id` mit dieser
    /// Herkunft.
    #[must_use]
    pub fn dedup_key(&self, rule_id: &str) -> DedupKey {
        DedupKey::derive(rule_id, self.uid, self.cgroup_kid, &self.subject)
    }
}

/// Wohin [`report_findings`] gemeldete Befunde außer ins Journal gibt.
///
/// # Description
/// `Default` ist „nirgendwohin": kein Export, kein Spool, kein Kontext.
///
/// # Concurrency
/// Geliehene Ausgabewege des Sammelthreads, nur für einen Aufruf.
///
/// # Examples
/// ```rust,ignore
/// let outputs = ReportOutputs {
///     exporter: Some(&mut exporter),
///     ..ReportOutputs::default()
/// };
/// ```
#[derive(Debug, Default)]
pub struct ReportOutputs<'a> {
    /// Der optionale JSON-Lines-Export (`--findings-export`).
    pub exporter: Option<&'a mut FindingsExporter>,
    /// Der optionale Spool-Schreiber; ohne ihn entsteht kein Record und
    /// keine Zeile trägt `record_digest`.
    pub spool: Option<&'a SpoolWriter>,
    /// Die Ereignisse des Rings (älteste zuerst), aus denen der Kontext
    /// eines Records stammt; leer ohne Spool.
    pub context: &'a [&'a SecurityEvent],
}

/// Baut den [`NetworkScope`] der Regeln aus dem aktiven DoD-Profil.
///
/// # Description
/// Jeder Eintrag von `egress_allow_cidrs` wird ein
/// [`EgressTarget::Cidr`] (PL-100 WP0.3); die Konfiguration hat die CIDRs
/// bereits normalisiert und auf Duplikate geprüft. Ohne Profil (eingebaute
/// Vorgaben, weil keine Systemkonfiguration existiert) oder mit leerer Liste
/// ist das Ergebnis [`NetworkScope::empty`] — jedes beobachtete Ziel liegt
/// dann außerhalb des erlaubten Bereichs (siehe Moduldoku).
///
/// # Arguments
/// - `profile` (`Option<&ResolvedObservationProfile>`): das aufgelöste
///   aktive Profil, falls eine Systemkonfiguration geladen wurde.
///
/// # Returns
/// Den erlaubten Netzbereich für [`RuleContext::network_scope`].
///
/// # Examples
/// ```rust,ignore
/// let scope = network_scope_from_profile(None);
/// assert!(scope.is_empty());
/// ```
#[must_use]
pub fn network_scope_from_profile(profile: Option<&ResolvedObservationProfile>) -> NetworkScope {
    profile.map_or_else(NetworkScope::empty, |profile| {
        NetworkScope::from_targets(
            profile
                .egress_allow_cidrs()
                .iter()
                .copied()
                .map(EgressTarget::Cidr),
        )
    })
}

/// Zustand der Regelauswertung über Poll-Zyklen hinweg.
///
/// # Description
/// Hält die [`BufferMark`] des zuletzt ausgewerteten Zyklus, den
/// konfigurierten [`NetworkScope`], den optionalen
/// [`FindingThrottle`], die kumulierten Zähler der vor der Auswertung
/// verdrängten Einträge ([`Self::ring_counters`], `health.json`) und den
/// vorigen Stand jedes kumulativen Zählers ([`RateStage`], PL-100 WP0.14).
/// [`Self::evaluate`] wertet die Regeln nur über das seit der Marke
/// angekommene Delta aus und rückt die Marke danach vor (siehe Moduldoku,
/// Abschnitt „Delta-Auswertung statt Befund-Flut"). Eine neue Instanz
/// beginnt bei [`BufferMark::START`] und wertet im ersten Zyklus alles aus,
/// was der Ring dann hält (z. B. das vor dem ersten Zyklus nachgereichte
/// Landlock-Ereignis).
///
/// # Concurrency
/// Lebt ausschließlich im Sammelthread neben dem `Sentinel`; `&mut self`
/// für jede Auswertung.
///
/// # Examples
/// ```rust,ignore
/// let mut rules = RuleCycle::new(NetworkScope::empty())
///     .with_throttle(FindingThrottle::open(ThrottleConfig::default(), None));
/// let reported = rules.evaluate(sentinel.buffer(), &NullSink, now, &CgroupKids::default(), None);
/// ```
#[derive(Debug)]
pub struct RuleCycle {
    /// Folgemarke des zuletzt ausgewerteten Deltas.
    since: BufferMark,
    /// Erlaubter Netzbereich für [`harw_dod_rules::EgressFlowRule`].
    network_scope: NetworkScope,
    /// Entprellung wiederholter Befunde; `None` meldet jeden Befund.
    throttle: Option<FindingThrottle>,
    /// Seit Prozessstart vor der Auswertung verdrängte Einträge.
    overflow: RingCounters,
    /// Voriger Stand der kumulativen Zähler für die Raten (WP0.14).
    rates: RateStage,
    /// Spool-Schreiber für die Records gemeldeter Befunde (WP0.12);
    /// `None` friert nichts ein.
    spool: Option<SpoolWriter>,
}

impl RuleCycle {
    /// Beginnt eine Auswertung bei [`BufferMark::START`], ohne Throttle und
    /// ohne bekannten Zählerstand.
    ///
    /// # Arguments
    /// - `network_scope` (`NetworkScope`): der erlaubte Netzbereich, meist
    ///   aus [`network_scope_from_profile`].
    #[must_use]
    pub fn new(network_scope: NetworkScope) -> Self {
        Self {
            since: BufferMark::START,
            network_scope,
            throttle: None,
            overflow: RingCounters::default(),
            rates: RateStage::new(),
            spool: None,
        }
    }

    /// Friert ab jetzt jeden gemeldeten Befund mit seinem Beleg-Ausschnitt
    /// ein und übergibt ihn an `spool` (PL-100 WP0.12); der Schreiber endet
    /// mit dieser Auswertung (sein Drop schreibt Eingereihtes fertig).
    ///
    /// # Arguments
    /// - `spool` (`SpoolWriter`): im Betrieb aus
    ///   [`SpoolWriter::open`] (`crate::run`).
    #[must_use]
    pub fn with_spool(mut self, spool: SpoolWriter) -> Self {
        self.spool = Some(spool);
        self
    }

    /// Entprellt ab jetzt wiederholte Befunde über `throttle` (PL-100
    /// WP0.4).
    ///
    /// # Arguments
    /// - `throttle` (`FindingThrottle`): im Betrieb mit Zustandsdatei unter
    ///   `--home` geöffnet (`crate::run`).
    #[must_use]
    pub fn with_throttle(mut self, throttle: FindingThrottle) -> Self {
        self.throttle = Some(throttle);
        self
    }

    /// Kumulierte Zähler der Einträge, die der Ring verdrängte, bevor die
    /// Regeln sie sahen (für `health.json`, PL-100 WP0.13).
    #[must_use]
    pub const fn ring_counters(&self) -> RingCounters {
        self.overflow
    }

    /// Wertet die Regeln über das Delta seit dem vorigen Aufruf aus und
    /// meldet jeden nicht unterdrückten Befund.
    ///
    /// # Description
    /// Friert über [`EvidenceBuffer::freeze_since`] nur die seit der
    /// gehaltenen Marke angekommenen Einträge ein, leitet über
    /// [`RateStage::derive`] die Raten der kumulativen Zähler dieses Deltas
    /// ab (PL-100 WP0.14), ruft [`report_findings`] mit Delta und Raten auf
    /// und rückt die Marke auf den Zählerstand des Puffers vor.
    /// Verdrängte, nie ausgewertete Einträge werden kumuliert
    /// ([`Self::ring_counters`]) und per `tracing::warn!` gemeldet (nur
    /// Zähler). Scheitert das Einfrieren (praktisch unerreichbar, siehe
    /// `harw_dod_sentinel::error`), wird das Delta mit einer Warnung
    /// übersprungen und die Marke trotzdem vorgerückt: ein dauerhaft nicht
    /// kodierbarer Eintrag soll nicht jede spätere Auswertung blockieren. Am
    /// Ende schreibt ein vorhandener Throttle seinen Zustand, wenn fällig.
    ///
    /// # Arguments
    /// - `buffer` (`&EvidenceBuffer`): der Puffer des `Sentinel`
    ///   (`Sentinel::buffer`).
    /// - `sink` (`&dyn TelemetrySink`): siehe [`report_findings`].
    /// - `now` (`jiff::Timestamp`): die injizierte Zeit dieses Zyklus.
    /// - `cgroup_kids` (`&CgroupKids`): Kernel-IDs der in diesem Zyklus
    ///   aufgelösten Cgroups (`crate::drain_external_events`).
    /// - `exporter` (`Option<&mut FindingsExporter>`): siehe
    ///   [`report_findings`].
    ///
    /// # Returns
    /// Die Anzahl in diesem Zyklus gemeldeter (nicht unterdrückter) Befunde.
    ///
    /// # Concurrency
    /// Siehe Typ-Doku.
    pub fn evaluate(
        &mut self,
        buffer: &EvidenceBuffer,
        sink: &dyn TelemetrySink,
        now: Timestamp,
        cgroup_kids: &CgroupKids,
        exporter: Option<&mut FindingsExporter>,
    ) -> usize {
        let since = self.since;
        self.since = buffer.mark();
        let reported = match buffer.freeze_since(since, now) {
            Ok(delta) => {
                self.overflow.overflow_samples_total = self
                    .overflow
                    .overflow_samples_total
                    .saturating_add(delta.samples_lost);
                self.overflow.overflow_events_total = self
                    .overflow
                    .overflow_events_total
                    .saturating_add(delta.events_lost);
                if delta.samples_lost > 0 || delta.events_lost > 0 {
                    tracing::warn!(
                        samples_lost = delta.samples_lost,
                        events_lost = delta.events_lost,
                        "evidence ring evicted entries before the rules evaluated them"
                    );
                }
                tracing::debug!(
                    samples = delta.evidence.samples.len(),
                    events = delta.evidence.events.len(),
                    "buffer delta frozen into evidence"
                );
                let rate_samples = self.rates.derive(&delta.evidence.samples);
                // Kontext der Spool-Records: der ganze Ring (Zeiger, keine
                // Kopien); nur mit verfügbarem Spool.
                let ring: Vec<&SecurityEvent> =
                    if self.spool.as_ref().is_some_and(SpoolWriter::is_available) {
                        buffer.events().collect()
                    } else {
                        Vec::new()
                    };
                report_findings(
                    sink,
                    &delta.evidence,
                    &rate_samples,
                    now,
                    &self.network_scope,
                    cgroup_kids,
                    self.throttle.as_mut(),
                    ReportOutputs {
                        exporter,
                        spool: self.spool.as_ref(),
                        context: &ring,
                    },
                )
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    samples_skipped = buffer.samples_since(since).count(),
                    events_skipped = buffer.events_since(since).count(),
                    "failed to freeze this cycle's buffer delta; skipping it"
                );
                0
            }
        };
        if let Some(throttle) = self.throttle.as_mut() {
            throttle.persist_if_due(now);
        }
        if let Some(spool) = self.spool.as_ref() {
            // Aufbewahrung des Spools (14 Tage), höchstens jede Minute wirksam.
            spool.tick(now);
        }
        reported
    }
}

/// Wertet die drei Regeln dieses Knotens gegen ein eingefrorenes
/// [`SecurityEvidence`] aus und meldet jeden zertifizierten, nicht
/// unterdrückten Befund.
///
/// # Description
/// Baut einen [`RuleContext`] aus `evidence.samples` samt den daraus
/// abgeleiteten `rate_samples` ([`with_rates`], PL-100 WP0.14),
/// `evidence.events`, dem übergebenen `network_scope` und einer leeren
/// Baseline-Liste (siehe Moduldoku, Abschnitte „Der Netzbereich kommt aus
/// der DoD-Konfiguration" und „Was dieser Kontext (noch) nicht befüllt"), ruft
/// [`run_rules_checked`] darauf auf und zählt jeden zurückgelieferten
/// `Finding<RuleChecked>` in [`SECURITY_FINDING_TOTAL`] auf `sink`
/// (maschinell auswertbar, ohne Freitext im Label — siehe
/// `harw_dod_sentinel::metrics`-Moduldoku, Abschnitt „Keine Inhalte in
/// Labels", dessen Auflage dieses Modul für dieselbe Sink-Instanz
/// übernimmt). Je Befund werden Herkunft und `dedup_key` bestimmt und, mit
/// `throttle`, über [`FindingThrottle::observe`] entschieden, ob er
/// gemeldet oder nur gezählt wird (siehe Moduldoku, „Entprellung
/// wiederholter Befunde"). Ein gemeldeter Befund erscheint auf
/// `tracing::warn!` nur mit Kennungen (siehe Moduldoku,
/// „Journal-Hygiene"), Details auf `debug`; mit Spool wird er samt
/// Beleg-Ausschnitt eingefroren und eingereiht, und er geht als Zeile
/// (Schema v3) an den Export (siehe Moduldoku, „Spool-Record und Export
/// v3").
///
/// # Arguments
/// - `sink` (`&dyn TelemetrySink`): dasselbe Sink-Objekt, über das dieses
///   Binary bereits Sensor-Metriken schreibt (im Betrieb ein
///   [`harw_observe_file::FileSink`]).
/// - `evidence` (`&SecurityEvidence`): der in diesem Zyklus eingefrorene
///   Beleg (im Betrieb das Delta aus [`RuleCycle::evaluate`]), dessen
///   `samples`/`events` gegen die Regeln laufen.
/// - `rate_samples` (`&[HostSample]`): die Raten, die
///   [`crate::rates::RateStage::derive`] aus `evidence.samples` abgeleitet
///   hat; sie laufen hinter den rohen Samples durch die Regeln. `&[]`, wenn
///   es keine gibt.
/// - `now` (`jiff::Timestamp`): dieselbe injizierte Zeit, mit der `evidence`
///   eingefroren wurde — nie die Systemuhr dieser Funktion selbst.
/// - `network_scope` (`&NetworkScope`): der erlaubte Netzbereich
///   ([`network_scope_from_profile`]).
/// - `cgroup_kids` (`&CgroupKids`): Kernel-IDs der in diesem Zyklus
///   aufgelösten Cgroup-Pfade, für `cgroup_kid` der Herkunft.
/// - `throttle` (`Option<&mut FindingThrottle>`): die Entprellung; `None`
///   meldet jeden Befund (mit `occurrences = 1`).
/// - `outputs` (`ReportOutputs`): Export, Spool-Schreiber und Ring-Kontext;
///   [`ReportOutputs::default`] schreibt nirgendwohin. Ein Export- oder
///   Spoolfehler wird dort gezählt und geloggt und ändert weder
///   Rückgabewert noch Meldung.
///
/// # Returns
/// Die Anzahl gemeldeter (nicht unterdrückter) Befunde. `0` ist der
/// häufigste, unauffällige Fall (siehe Moduldoku, Abschnitt „Ein leerer
/// Regellauf ist kein Fehler") — kein Fehler.
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
/// let reported = harw_sentinel::findings::report_findings(
///     &NullSink,
///     &evidence,
///     &[],
///     Timestamp::UNIX_EPOCH,
///     &harw_authority::NetworkScope::empty(),
///     &harw_sentinel::cgroup_resolve::CgroupKids::default(),
///     None,
///     harw_sentinel::findings::ReportOutputs::default(),
/// );
/// assert_eq!(reported, 0);
/// ```
// Acht Eingänge eines Zyklus, keiner davon ableitbar; die Ausgabewege sind
// bereits in `ReportOutputs` gebündelt.
#[allow(clippy::too_many_arguments)]
pub fn report_findings(
    sink: &dyn TelemetrySink,
    evidence: &SecurityEvidence,
    rate_samples: &[HostSample],
    now: Timestamp,
    network_scope: &NetworkScope,
    cgroup_kids: &CgroupKids,
    mut throttle: Option<&mut FindingThrottle>,
    mut outputs: ReportOutputs<'_>,
) -> usize {
    let samples = with_rates(&evidence.samples, rate_samples);
    let ctx = RuleContext {
        now,
        samples: &samples,
        events: &evidence.events,
        baselines: &[],
        network_scope,
    };

    let findings = run_rules_checked(&ctx);
    if findings.is_empty() {
        return 0;
    }
    let scopes = attribute_scopes(&findings, &evidence.events, now, network_scope);
    let provenance =
        attribute_provenance(&findings, &evidence.events, now, network_scope, cgroup_kids);

    let mut reported = 0_usize;
    for ((finding, finding_scope), Provenance { origin, triggers }) in
        findings.iter().zip(&scopes).zip(&provenance)
    {
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
        let dedup_key = origin.dedup_key(finding.rule_id());
        let decision = match throttle.as_deref_mut() {
            Some(throttle) => throttle.observe(
                dedup_key,
                finding.hardness(),
                finding.severity(),
                finding.observed_at(),
                now,
            ),
            None => ThrottleDecision::unthrottled(dedup_key, finding.observed_at()),
        };
        let stamp = decision.stamp;
        let ThrottleVerdict::Emit(reason) = decision.verdict else {
            sink.record(
                &SECURITY_FINDING_SUPPRESSED_TOTAL,
                MetricValue::Count(1),
                &[],
            );
            tracing::debug!(
                finding_id = finding.id().as_str(),
                rule_id = finding.rule_id(),
                dedup_key = %stamp.dedup_key,
                occurrences = stamp.occurrences,
                "repeated security finding counted within its cooldown, not reported"
            );
            continue;
        };
        // Journal-Hygiene (WP0.6): auf `warn` nur Kennungen und Aufzählungen.
        tracing::warn!(
            finding_id = finding.id().as_str(),
            rule_id = finding.rule_id(),
            kind = finding_kind_label(finding.kind()),
            severity = ?finding.severity(),
            hardness = ?finding.hardness(),
            dedup_key = %stamp.dedup_key,
            occurrences = stamp.occurrences,
            first_seen = %stamp.first_seen,
            reason = reason.label(),
            "security rule finding"
        );
        tracing::debug!(
            finding_id = finding.id().as_str(),
            summary = ?finding.summary(),
            scope = ?finding_scope.as_deref(),
            uid = ?origin.uid,
            cgroup_kid = ?origin.cgroup_kid,
            "security rule finding detail"
        );
        let trigger_events: Vec<&SecurityEvent> = triggers
            .iter()
            .filter_map(|index| evidence.events.get(*index))
            .collect();
        let record_digest = outputs
            .spool
            .and_then(|spool| spool_record(spool, finding, &trigger_events, outputs.context, now));
        // Export v3 (PL-100 WP0.12): Stempel, Herkunft, Record und Karte.
        if let Some(exporter) = outputs.exporter.as_deref_mut() {
            let card = EvidenceCard::from_events(&trigger_events);
            let fields = LineFields {
                scope: finding_scope.as_deref(),
                cgroup_kid: origin.cgroup_kid,
                record_digest,
                evidence_card: card.as_ref(),
                ..LineFields::of(finding, stamp)
            };
            exporter.export(sink, &fields);
        }
        reported = reported.saturating_add(1);
    }

    reported
}

/// Friert `finding` mit seinem Beleg-Ausschnitt ein und reiht den Record
/// beim Spool-Schreiber ein (PL-100 WP0.12).
///
/// # Description
/// Ausschnitt: `triggers` plus die jüngsten höchstens
/// [`MAX_CONTEXT_EVENTS`] Ereignisse aus `ring`, die kein auslösendes sind
/// ([`slice_record`], Grenze [`SpoolWriter::max_record_bytes`]). Ein
/// Record, der schon beim Zuschneiden scheitert, wird beim Schreiber
/// gezählt (`RecordTooLarge` und andere; `health.json`).
///
/// # Returns
/// Den Record-Digest, wenn der Record eingereiht wurde; sonst `None` (kein
/// Spool, zu groß, Warteschlange voll).
fn spool_record(
    spool: &SpoolWriter,
    finding: &harw_dod_rules::Finding<harw_dod_rules::RuleChecked>,
    triggers: &[&SecurityEvent],
    ring: &[&SecurityEvent],
    now: Timestamp,
) -> Option<ContentDigest> {
    if !spool.is_available() {
        return None;
    }
    let trigger_events: Vec<SecurityEvent> = triggers.iter().copied().cloned().collect();
    let mut context: Vec<SecurityEvent> = ring
        .iter()
        .rev()
        .copied()
        .filter(|event| !triggers.contains(event))
        .take(MAX_CONTEXT_EVENTS)
        .cloned()
        .collect();
    // Jüngste zuerst gesammelt; der Record trägt sie in Ring-Reihenfolge.
    context.reverse();
    match slice_record(
        finding,
        &trigger_events,
        &context,
        now,
        spool.max_record_bytes(),
    ) {
        Ok(record) => {
            let digest = record.digest();
            spool.submit(record).then_some(digest)
        }
        Err(error) => {
            spool.note_rejected(&error);
            tracing::debug!(
                finding_id = finding.id().as_str(),
                error = %error,
                "security finding not spooled; exported without record_digest"
            );
            None
        }
    }
}

/// What makes two findings of different rule runs the same finding.
///
/// The kind enters as its [`finding_kind_label`], so the tuple is `Ord` and
/// can key a `BTreeMap` ([`attribute_origins`]).
type FindingSignature = (
    &'static str,
    &'static str,
    Severity,
    Hardness,
    Timestamp,
    String,
);

/// The [`FindingSignature`] of one finding of either rule run.
macro_rules! signature_of {
    ($finding:expr) => {{
        let finding = $finding;
        (
            finding.rule_id(),
            finding_kind_label(finding.kind()),
            finding.severity(),
            finding.hardness(),
            finding.observed_at(),
            finding.summary().to_owned(),
        )
    }};
}

/// Derives the [`FindingOrigin`] of each finding (see the module
/// documentation, "Entprellung wiederholter Befunde").
///
/// # Description
/// Runs the rules once per single event of the delta and pools the origin
/// of every event under each signature it produces, keeping only what all
/// those events share ([`FindingOrigin::merge`]). A finding of the full run
/// takes the pooled origin of its signature; when the full run produces
/// that signature more often or less often than the single-event runs do
/// (a sample finding, or a rule combining several events), its `uid` and
/// `cgroup_kid` stay empty. Without any single-event match the subject is
/// the digest of the summary.
///
/// # Returns
/// One entry per element of `findings`, in the same order.
fn attribute_origins(
    findings: &[harw_dod_rules::Finding<harw_dod_rules::RuleChecked>],
    events: &[SecurityEvent],
    now: Timestamp,
    network_scope: &NetworkScope,
    cgroup_kids: &CgroupKids,
) -> Vec<FindingOrigin> {
    let mut pool: BTreeMap<FindingSignature, (usize, FindingOrigin)> = BTreeMap::new();
    for event in events {
        let ctx = RuleContext {
            now,
            samples: &[],
            events: std::slice::from_ref(event),
            baselines: &[],
            network_scope,
        };
        let raws = run_rules(&ctx);
        if raws.is_empty() {
            continue;
        }
        let origin = FindingOrigin::of_event(event, cgroup_kids);
        for raw in &raws {
            match pool.entry(signature_of!(raw)) {
                Entry::Occupied(mut slot) => {
                    let (count, pooled) = slot.get_mut();
                    *count = count.saturating_add(1);
                    pooled.merge(&origin, raw.summary());
                }
                Entry::Vacant(slot) => {
                    slot.insert((1, origin.clone()));
                }
            }
        }
    }

    let signatures: Vec<FindingSignature> = findings
        .iter()
        .map(|finding| signature_of!(finding))
        .collect();
    let mut in_full_run: BTreeMap<&FindingSignature, usize> = BTreeMap::new();
    for signature in &signatures {
        let count = in_full_run.entry(signature).or_insert(0);
        *count = count.saturating_add(1);
    }
    signatures
        .iter()
        .zip(findings)
        .map(|(signature, finding)| match pool.get(signature) {
            Some((count, pooled)) => {
                let mut origin = pooled.clone();
                if in_full_run.get(signature) != Some(count) {
                    origin.uid = None;
                    origin.cgroup_kid = None;
                }
                origin
            }
            None => FindingOrigin::unattributed(finding.summary()),
        })
        .collect()
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
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::throttle::ThrottleConfig;
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
            &[],
            Timestamp::UNIX_EPOCH,
            &NetworkScope::empty(),
            &CgroupKids::default(),
            None,
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
            &[],
            Timestamp::UNIX_EPOCH,
            &NetworkScope::empty(),
            &CgroupKids::default(),
            None,
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
            &[],
            Timestamp::UNIX_EPOCH,
            &NetworkScope::empty(),
            &CgroupKids::default(),
            None,
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
                process: None,
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
            &[],
            Timestamp::UNIX_EPOCH,
            &NetworkScope::empty(),
            &CgroupKids::default(),
            None,
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

    fn egress_event(destination: &str, port: u16) -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("probe-bpf-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::EgressFlow {
                destination: destination.to_owned(),
                port,
            },
        }
    }

    fn profile_with_cidrs(cidrs: &str) -> TestResult<ResolvedObservationProfile> {
        let config = harw_dod_config::Config::from_toml(&format!(
            "schema_version = 1\nmode = \"observe\"\nactive_profile = \"host\"\n\n\
             [profiles.host]\nscope = \"host\"\nsensors = [\"exec\", \"tcp-connect\"]\n\
             egress_allow_cidrs = {cidrs}\n"
        ))
        .map_err(ctx("valid DoD config"))?;
        config.resolve_active().map_err(ctx("active profile"))
    }

    fn checked_sum(total: usize, more: usize) -> TestResult<usize> {
        total
            .checked_add(more)
            .ok_or(crate::test_support::TestError::Missing(
                "finding count overflow",
            ))
    }

    /// Flut-Regressionstest (PL-100 WP0.1, Tor I9): N Zyklen über einen
    /// unveränderten Ring melden jedes auslösende Ereignis genau einmal.
    #[test]
    fn test_n_cycles_over_a_constant_ring_report_each_triggering_event_once() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        let host = harw_types::HostId::try_from_str("host-a").map_err(ctx("host id"))?;
        let mut exporter = FindingsExporter::new(path.clone(), host);
        let mut buffer = EvidenceBuffer::new(16, 16);
        buffer.push_event(structure_drift_event());
        buffer.push_event(egress_event("198.51.100.5", 443));
        let mut rules = RuleCycle::new(NetworkScope::empty());

        let mut reported = 0_usize;
        for _ in 0..25 {
            let cycle = rules.evaluate(
                &buffer,
                &harw_observe::NullSink,
                Timestamp::UNIX_EPOCH,
                &CgroupKids::default(),
                Some(&mut exporter),
            );
            reported = checked_sum(reported, cycle)?;
        }
        assert_eq!(
            reported, 2,
            "one finding per triggering event, not per cycle"
        );
        assert_eq!(exporter.written(), 2);
        assert_eq!(buffer.event_len(), 2, "the ring stays the context store");

        // Ein neues Ereignis löst genau einen weiteren Befund aus.
        buffer.push_event(egress_event("198.51.100.6", 8443));
        for _ in 0..25 {
            let cycle = rules.evaluate(
                &buffer,
                &harw_observe::NullSink,
                Timestamp::UNIX_EPOCH,
                &CgroupKids::default(),
                Some(&mut exporter),
            );
            reported = checked_sum(reported, cycle)?;
        }
        assert_eq!(reported, 3);
        let text = std::fs::read_to_string(&path).map_err(ctx("read export"))?;
        assert_eq!(text.lines().count(), 3);
        Ok(())
    }

    #[test]
    fn test_rule_cycle_evaluates_events_evicted_from_view_only_once() {
        // Mehr Ankünfte als Kapazität: nur die gehaltenen werden ausgewertet,
        // und auch die nur einmal.
        let mut buffer = EvidenceBuffer::new(2, 2);
        let mut rules = RuleCycle::new(NetworkScope::empty());
        for port in 1..=5 {
            buffer.push_event(egress_event("198.51.100.5", port));
        }
        let first = rules.evaluate(
            &buffer,
            &harw_observe::NullSink,
            Timestamp::UNIX_EPOCH,
            &CgroupKids::default(),
            None,
        );
        let second = rules.evaluate(
            &buffer,
            &harw_observe::NullSink,
            Timestamp::UNIX_EPOCH,
            &CgroupKids::default(),
            None,
        );
        assert_eq!(first, 2);
        assert_eq!(second, 0);
    }

    #[test]
    fn test_attribution_runs_over_the_delta_only() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        let host = harw_types::HostId::try_from_str("host-a").map_err(ctx("host id"))?;
        let mut exporter = FindingsExporter::new(path.clone(), host);
        let job = "/sys/fs/cgroup/harw-job-1";
        let mut buffer = EvidenceBuffer::new(16, 16);
        buffer.push_event(drift_in(Some(job), "same"));
        let mut rules = RuleCycle::new(NetworkScope::empty());
        rules.evaluate(
            &buffer,
            &harw_observe::NullSink,
            Timestamp::UNIX_EPOCH,
            &CgroupKids::default(),
            Some(&mut exporter),
        );
        // Dieselbe Signatur aus einer anderen cgroup im nächsten Zyklus: über
        // den ganzen Ring wäre sie mehrdeutig, über das Delta ist sie es nicht.
        buffer.push_event(drift_in(Some("/sys/fs/cgroup/harw-job-2"), "same"));
        rules.evaluate(
            &buffer,
            &harw_observe::NullSink,
            Timestamp::UNIX_EPOCH,
            &CgroupKids::default(),
            Some(&mut exporter),
        );

        let text = std::fs::read_to_string(&path).map_err(ctx("read export"))?;
        let mut scopes = Vec::new();
        for line in text.lines() {
            let value: serde_json::Value = serde_json::from_str(line).map_err(ctx("json line"))?;
            scopes.push(
                value
                    .get("scope")
                    .and_then(|s| s.as_str())
                    .map(str::to_owned),
            );
        }
        assert_eq!(
            scopes,
            vec![
                Some(job.to_owned()),
                Some("/sys/fs/cgroup/harw-job-2".to_owned())
            ]
        );
        Ok(())
    }

    #[test]
    fn test_network_scope_without_profile_is_empty() {
        assert!(network_scope_from_profile(None).is_empty());
    }

    #[test]
    fn test_network_scope_with_an_empty_allowlist_stays_empty() -> TestResult {
        let profile = profile_with_cidrs("[]")?;
        assert!(network_scope_from_profile(Some(&profile)).is_empty());
        Ok(())
    }

    #[test]
    fn test_network_scope_is_built_from_the_configured_cidrs() -> TestResult {
        let profile = profile_with_cidrs(r#"["203.0.113.0/24", "2001:db8::/32"]"#)?;
        let scope = network_scope_from_profile(Some(&profile));
        assert_eq!(scope.targets().count(), 2);
        let inside = "203.0.113.7"
            .parse::<std::net::IpAddr>()
            .map_err(ctx("ipv4 literal"))?;
        let inside_v6 = "2001:db8::1"
            .parse::<std::net::IpAddr>()
            .map_err(ctx("ipv6 literal"))?;
        let outside = "198.51.100.5"
            .parse::<std::net::IpAddr>()
            .map_err(ctx("ipv4 literal"))?;
        assert!(scope.allows_addr(inside));
        assert!(scope.allows_addr(inside_v6));
        assert!(!scope.allows_addr(outside));
        Ok(())
    }

    /// PL-100 WP0.3: ein Fluss in eine erlaubte CIDR erzeugt keinen Befund,
    /// einer außerhalb genau einen.
    #[test]
    fn test_egress_into_an_allowed_cidr_yields_no_finding() -> TestResult {
        let profile = profile_with_cidrs(r#"["203.0.113.0/24"]"#)?;
        let mut buffer = EvidenceBuffer::new(8, 8);
        buffer.push_event(egress_event("203.0.113.7", 443));
        buffer.push_event(egress_event("203.0.113.200", 22));
        let mut rules = RuleCycle::new(network_scope_from_profile(Some(&profile)));
        let allowed = rules.evaluate(
            &buffer,
            &harw_observe::NullSink,
            Timestamp::UNIX_EPOCH,
            &CgroupKids::default(),
            None,
        );
        assert_eq!(
            allowed, 0,
            "flows into the configured allowlist are not findings"
        );

        buffer.push_event(egress_event("198.51.100.5", 443));
        let outside = rules.evaluate(
            &buffer,
            &harw_observe::NullSink,
            Timestamp::UNIX_EPOCH,
            &CgroupKids::default(),
            None,
        );
        assert_eq!(outside, 1);
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

    fn at(secs: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(secs).map_err(ctx("timestamp"))
    }

    fn egress_from(uid: Option<u32>, destination: &str, port: u16) -> SecurityEvent {
        SecurityEvent {
            actor: uid.map(|uid| harw_dod_signals::Actor {
                uid,
                auid: None,
                cgroup: None,
                process: None,
            }),
            ..egress_event(destination, port)
        }
    }

    fn throttled_cycle(cooldown_secs: u64) -> RuleCycle {
        RuleCycle::new(NetworkScope::empty()).with_throttle(FindingThrottle::open(
            ThrottleConfig::with_cooldown_secs(cooldown_secs),
            None,
        ))
    }

    /// PL-100 WP0.4: Wiederholungen aus demselben /24 an denselben Port
    /// werden innerhalb der Abklingzeit nur gezählt, danach erneut gemeldet.
    #[test]
    fn test_throttle_reports_a_repeated_egress_once_per_cooldown() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        let host = harw_types::HostId::try_from_str("host-a").map_err(ctx("host id"))?;
        let mut exporter = FindingsExporter::new(path, host);
        let mut buffer = EvidenceBuffer::new(64, 64);
        let mut rules = throttled_cycle(600);

        let mut reported = 0_usize;
        for (second, destination) in [
            (0, "198.51.100.5"),
            (5, "198.51.100.77"),
            (10, "198.51.100.5"),
        ] {
            buffer.push_event(egress_from(Some(1000), destination, 443));
            let cycle = rules.evaluate(
                &buffer,
                &harw_observe::NullSink,
                at(second)?,
                &CgroupKids::default(),
                Some(&mut exporter),
            );
            reported = checked_sum(reported, cycle)?;
        }
        assert_eq!(reported, 1, "same uid, same /24, same port: one report");
        assert_eq!(exporter.written(), 1);

        buffer.push_event(egress_from(Some(1000), "198.51.100.9", 443));
        let late = rules.evaluate(
            &buffer,
            &harw_observe::NullSink,
            at(700)?,
            &CgroupKids::default(),
            Some(&mut exporter),
        );
        assert_eq!(late, 1, "the cooldown elapsed");
        assert_eq!(exporter.written(), 2);
        Ok(())
    }

    #[test]
    fn test_throttle_keeps_new_ports_and_other_uids_apart() -> TestResult {
        let mut buffer = EvidenceBuffer::new(64, 64);
        let mut rules = throttled_cycle(600);
        buffer.push_event(egress_from(Some(1000), "198.51.100.5", 443));
        buffer.push_event(egress_from(Some(1000), "198.51.100.5", 8443));
        buffer.push_event(egress_from(Some(1001), "198.51.100.5", 443));
        buffer.push_event(egress_from(Some(1000), "198.51.100.6", 443));
        let reported = rules.evaluate(
            &buffer,
            &harw_observe::NullSink,
            at(0)?,
            &CgroupKids::default(),
            None,
        );
        assert_eq!(
            reported, 3,
            "a new port and another uid are new keys; the same /24 is not"
        );
        Ok(())
    }

    /// Zählt nur die Metrik unterdrückter Befunde.
    #[derive(Debug, Default)]
    struct SuppressedCounter(std::sync::atomic::AtomicU64);

    impl TelemetrySink for SuppressedCounter {
        fn record(
            &self,
            key: &harw_observe::MetricKey,
            _value: MetricValue,
            _labels: &[(harw_observe::FieldName, FieldValue)],
        ) {
            if key.name == SECURITY_FINDING_SUPPRESSED_TOTAL.name {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }

        fn flush(&self) {}

        fn name(&self) -> &'static str {
            "suppressed-counter"
        }
    }

    #[test]
    fn test_suppressed_repeats_are_counted_in_their_metric() -> TestResult {
        let sink = SuppressedCounter::default();
        let mut buffer = EvidenceBuffer::new(64, 64);
        let mut rules = throttled_cycle(600);
        for _ in 0..3 {
            buffer.push_event(egress_from(None, "198.51.100.5", 443));
        }
        let reported = rules.evaluate(&buffer, &sink, at(0)?, &CgroupKids::default(), None);
        assert_eq!(reported, 1);
        assert_eq!(sink.0.load(std::sync::atomic::Ordering::Relaxed), 2);
        Ok(())
    }

    fn checked_findings(
        events: &[SecurityEvent],
    ) -> TestResult<Vec<harw_dod_rules::Finding<harw_dod_rules::RuleChecked>>> {
        let evidence = SecurityEvidence::capture(vec![], events.to_vec(), Timestamp::UNIX_EPOCH)
            .map_err(ctx("well-formed content always encodes"))?;
        let scope = NetworkScope::empty();
        let ctx_full = RuleContext {
            now: Timestamp::UNIX_EPOCH,
            samples: &evidence.samples,
            events: &evidence.events,
            baselines: &[],
            network_scope: &scope,
        };
        Ok(run_rules_checked(&ctx_full))
    }

    #[test]
    fn test_origin_carries_uid_and_the_cgroup_kid_of_the_cycle() -> TestResult {
        let job = "/sys/fs/cgroup/harw.slice/harw-job-1";
        let mut kids = CgroupKids::default();
        kids.record(job, 4_242);
        let mut raw = drift_in(None, "raw id");
        if let Some(actor) = raw.actor.as_mut() {
            actor.cgroup = harw_dod_signals::Actor::raw_cgroup(77);
        }
        let events = vec![
            drift_in(Some(job), "job one"),
            drift_in(None, "no cgroup"),
            raw,
        ];
        let findings = checked_findings(&events)?;
        let origins = attribute_origins(
            &findings,
            &events,
            Timestamp::UNIX_EPOCH,
            &NetworkScope::empty(),
            &kids,
        );
        let ids: Vec<(Option<u32>, Option<u64>)> = origins
            .iter()
            .map(|origin| (origin.uid, origin.cgroup_kid))
            .collect();
        assert_eq!(
            ids,
            vec![
                (Some(1000), Some(4_242)),
                (Some(1000), None),
                (Some(1000), Some(77))
            ]
        );
        let first_event = events.first().ok_or(TestError::Missing("first event"))?;
        let first_origin = origins.first().ok_or(TestError::Missing("first origin"))?;
        assert_eq!(first_origin.subject, CoarseSubject::of_event(first_event));
        Ok(())
    }

    #[test]
    fn test_identical_findings_from_two_uids_keep_only_the_shared_subject() -> TestResult {
        let mut other = drift_in(None, "same");
        if let Some(actor) = other.actor.as_mut() {
            actor.uid = 1001;
        }
        let events = vec![drift_in(None, "same"), other];
        let findings = checked_findings(&events)?;
        let origins = attribute_origins(
            &findings,
            &events,
            Timestamp::UNIX_EPOCH,
            &NetworkScope::empty(),
            &CgroupKids::default(),
        );
        let first_event = events.first().ok_or(TestError::Missing("first event"))?;
        let expected = FindingOrigin {
            uid: None,
            cgroup_kid: None,
            subject: CoarseSubject::of_event(first_event),
        };
        assert_eq!(origins, vec![expected.clone(), expected]);
        Ok(())
    }

    #[test]
    fn test_a_finding_without_a_single_event_match_keys_on_its_summary() -> TestResult {
        let findings = checked_findings(&[structure_drift_event()])?;
        let origins = attribute_origins(
            &findings,
            &[],
            Timestamp::UNIX_EPOCH,
            &NetworkScope::empty(),
            &CgroupKids::default(),
        );
        let finding = findings.first().ok_or(TestError::Missing("finding"))?;
        assert_eq!(
            origins,
            vec![FindingOrigin {
                uid: None,
                cgroup_kid: None,
                subject: CoarseSubject::of_summary(finding.summary()),
            }]
        );
        Ok(())
    }

    #[test]
    fn test_ring_counters_accumulate_entries_lost_before_evaluation() {
        let mut buffer = EvidenceBuffer::new(2, 2);
        let mut rules = RuleCycle::new(NetworkScope::empty());
        for port in 1..=5 {
            buffer.push_event(egress_event("198.51.100.5", port));
        }
        rules.evaluate(
            &buffer,
            &harw_observe::NullSink,
            Timestamp::UNIX_EPOCH,
            &CgroupKids::default(),
            None,
        );
        assert_eq!(
            rules.ring_counters(),
            RingCounters {
                overflow_samples_total: 0,
                overflow_events_total: 3,
            }
        );
        for port in 6..=8 {
            buffer.push_event(egress_event("198.51.100.5", port));
        }
        rules.evaluate(
            &buffer,
            &harw_observe::NullSink,
            Timestamp::UNIX_EPOCH,
            &CgroupKids::default(),
            None,
        );
        assert_eq!(rules.ring_counters().overflow_events_total, 4);
    }

    fn user_jiffies(secs: i64, value: f64) -> TestResult<HostSample> {
        Ok(HostSample {
            sensor: SensorId::from_str("cpu-0"),
            observed_at: at(secs)?,
            metric: std::borrow::Cow::Borrowed("user_jiffies"),
            value,
        })
    }

    /// PL-100 WP0.14: `evaluate` gibt jedes Sample des Deltas genau einmal an
    /// die Ratenstufe; ihr Zustand trägt danach den neuesten Zählerstand.
    #[test]
    fn test_evaluate_feeds_each_delta_sample_into_the_rate_stage_once() -> TestResult {
        let mut buffer = EvidenceBuffer::new(16, 16);
        let mut rules = RuleCycle::new(NetworkScope::empty());
        buffer.push_sample(user_jiffies(0, 100.0)?);
        let first = rules.evaluate(
            &buffer,
            &harw_observe::NullSink,
            at(0)?,
            &CgroupKids::default(),
            None,
        );
        buffer.push_sample(user_jiffies(5, 600.0)?);
        // Zwei Zyklen über denselben Ring: das Delta des zweiten ist leer.
        for now in [5, 6] {
            let reported = rules.evaluate(
                &buffer,
                &harw_observe::NullSink,
                at(now)?,
                &CgroupKids::default(),
                None,
            );
            assert_eq!(reported, 0, "rates alone trigger no finding");
        }
        assert_eq!(first, 0);
        assert_eq!(rules.rates.tracked(), 1);

        // Bezugspunkt ist das Sample bei t = 5 s (600): (1600 − 600) / 5 s.
        // Vom Sample bei 0 s aus wären es (1600 − 100) / 10 s = 150.
        let rates = rules.rates.derive(&[user_jiffies(10, 1_600.0)?]);
        let rate = rates
            .first()
            .ok_or(TestError::Missing("rate from the stage state"))?;
        assert_eq!(rate.metric, "user_jiffies.rate");
        assert!((rate.value - 200.0).abs() < 1e-9, "rate {}", rate.value);
        Ok(())
    }

    /// Ohne Baselines wertet keine Regel Samples aus: Raten im Kontext
    /// erzeugen keinen Befund (kein neuer Flutweg, Tor I9).
    #[test]
    fn test_rate_samples_in_the_rule_context_trigger_no_finding_without_baselines() -> TestResult {
        let evidence = SecurityEvidence::capture(
            vec![user_jiffies(5, 600.0)?],
            vec![structure_drift_event()],
            Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("well-formed content always encodes"))?;
        let rate = HostSample {
            metric: std::borrow::Cow::Borrowed("user_jiffies.rate"),
            value: 100.0,
            ..user_jiffies(5, 0.0)?
        };

        let reported = report_findings(
            &harw_observe::NullSink,
            &evidence,
            &[rate],
            Timestamp::UNIX_EPOCH,
            &NetworkScope::empty(),
            &CgroupKids::default(),
            None,
            None,
        );
        assert_eq!(reported, 1, "only the drift event reports");
        Ok(())
    }

    #[test]
    fn test_event_kind_counts_name_kinds_only() {
        let counts = event_kind_counts(&[
            egress_event("198.51.100.5", 443),
            egress_event("198.51.100.6", 80),
            structure_drift_event(),
        ]);
        assert_eq!(counts.get("egress-flow"), Some(&2));
        assert_eq!(counts.get("structure-drift"), Some(&1));
        assert_eq!(counts.len(), 2);
    }

    /// Sammelt die formatierte `tracing`-Ausgabe eines Tests.
    #[derive(Debug, Clone, Default)]
    struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| std::io::Error::other("capture lock poisoned"))?
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Captured {
        fn text(&self) -> TestResult<String> {
            let bytes = self
                .0
                .lock()
                .map_err(|_| TestError::Missing("capture lock"))?
                .clone();
            String::from_utf8(bytes).map_err(ctx("utf-8 log output"))
        }
    }

    /// PL-100 WP0.6: auf `warn` stehen keine Ziele, Pfade oder
    /// Zusammenfassungen, nur Kennungen.
    #[test]
    fn test_warn_level_names_no_destination_path_or_summary() -> TestResult {
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let evidence = SecurityEvidence::capture(
            vec![],
            vec![
                egress_event("198.51.100.5", 443),
                drift_in(
                    Some("/sys/fs/cgroup/harw-job-1"),
                    "drift at /etc/secret-file",
                ),
            ],
            Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("well-formed content always encodes"))?;

        let reported = tracing::subscriber::with_default(subscriber, || {
            report_findings(
                &harw_observe::NullSink,
                &evidence,
                &[],
                Timestamp::UNIX_EPOCH,
                &NetworkScope::empty(),
                &CgroupKids::default(),
                None,
                None,
            )
        });

        assert_eq!(reported, 2);
        let text = captured.text()?;
        assert!(text.contains("security rule finding"), "{text}");
        assert!(text.contains("egress-flow"), "{text}");
        for leaked in [
            "198.51.100.5",
            "/etc/secret-file",
            "harw-job-1",
            "outside the allowed",
        ] {
            assert!(!text.contains(leaked), "{leaked} reached warn: {text}");
        }
        Ok(())
    }
}
