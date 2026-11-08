//! Optionaler JSON-Lines-Export zertifizierter Befunde für `harw-security-hub`.
//!
//! # Wozu
//! [`crate::findings::report_findings`] meldet jeden `Finding<RuleChecked>`,
//! den der Befund-Throttle (`crate::throttle`, PL-100 WP0.4) nicht als
//! Wiederholung innerhalb seiner Abklingzeit zurückhält, über `tracing` und
//! einen Telemetriezähler; nur diese Befunde erreichen den Export.
//! `harw-security-hub`
//! (`harw-security-hub/src/findings.rs`) korreliert DoD-Befunde aber aus einer
//! append-only JSON-Lines-Datei. Dieses Modul schreibt genau diese Datei —
//! **nur**, wenn `--findings-export <ABSOLUTER-PFAD>` gesetzt ist. Ohne das
//! Flag existiert kein [`FindingsExporter`], und es wird nichts geschrieben.
//!
//! # Zeilenformat (Vertrag mit dem Hub)
//! Version 1 bestand aus genau diesen sechs Feldern:
//!
//! ```json
//! {"finding_id":"…","host":"host-a","rule_id":"structure-drift","severity":"medium","summary":"…","observed_at":"2026-09-27T10:00:00Z"}
//! ```
//!
//! ## Schema version 2 (additive, PL-97 D1)
//! Version 2 adds `"schema_version"` and, when the finding can be
//! attributed to a cgroup, an optional `"scope"` (the absolute cgroup path,
//! see [`sanitize_scope`]). `scope` is set when the finding can be
//! attributed to the resolved cgroup of the observed actor (PL-97 E4: the
//! probe forwards the raw cgroup id, `crate::cgroup_resolve` maps it to the
//! absolute path beneath `--cgroup-root`, `crate::findings` attributes the
//! finding); it is omitted otherwise.
//!
//! ## Schema version 3 (PL-100 WP0.12, written today)
//! Every line now carries `"schema_version": 3`
//! ([`EXPORT_SCHEMA_VERSION`]); v3 is a strict superset of v2. Readers
//! (`harw-dod-client`, harw's `security_signals` through that client,
//! `harw-security-hub`) treat every new field as optional, so v1, v2 and v3
//! lines parse alike (reader-first rollout, PL-100 I11). The fields, in the
//! order they are written after `scope`:
//!
//! - `instance`: UUIDv4 of this sentinel process, new per start;
//! - `seq`: position of the line within `instance`, from 1, monotonic. Every
//!   attempted line takes a number, so a gap marks a line that failed to
//!   write;
//! - `record_digest` (optional): lowercase hex of the BLAKE3 digest of the
//!   `FindingRecord` handed to the finding spool (`crate::spool_writer`);
//!   absent when no record was produced or it could not be queued;
//! - `dedup_key`, `occurrences`, `first_seen`: the [`ThrottleStamp`] of the
//!   finding (`crate::throttle`);
//! - `kind` (`rule_triggered` | `anomaly`, [`kind_label`]) and `hardness`
//!   (`observed` | `correlated` | `inferred`, [`hardness_label`]);
//! - `cgroup_kid` (optional): kernel id of the actor's cgroup. Kernel ids are
//!   reused; readers pair it with `scope` and `first_seen`;
//! - `scope_obfuscated`: `true` when the finding had a scope that could not
//!   be rendered safely (PL-100 WP0.9): the scope is then withheld instead of
//!   silently dropped;
//! - `evidence_card` (optional, [`EvidenceCard`]): a small summary of the
//!   triggering events, at most [`MAX_EVIDENCE_CARD_BYTES`] serialized.
//!
//! Die Feldtypen sind dieselben, die der Hub deserialisiert:
//! [`harw_types::FindingId`], [`harw_types::HostId`],
//! [`harw_types::ImpactSeverity`] (snake_case) und `jiff::Timestamp`.
//! `harw_dod_signals::Severity` wird über [`impact_severity`] erschöpfend auf
//! `ImpactSeverity` abgebildet — keine String-Umwandlung dazwischen.
//!
//! # Keine Geheimnisse, escapter Freitext
//! Exportiert werden ausschließlich die Felder oben — keine Rohereignisse,
//! keine Konfiguration. Jede Zeichenkette, die aus Beobachtungen stammt,
//! läuft durch den gemeinsamen Escaper `harw-untrusted-text` (PL-100
//! WP0.8): `summary` über [`sanitize_summary`] (einzeilig, höchstens
//! [`MAX_SUMMARY_BYTES`] Bytes), `scope` über [`sanitize_scope`] (wird nie
//! umgeschrieben, nur zurückgehalten) und jede Zeichenkette der
//! `evidence_card` über `harw_untrusted_text::escape_line`. Unsichtbare und
//! steuernde Zeichen erscheinen als sichtbares `\u{…}`.
//!
//! # Datei
//! - geöffnet mit `O_APPEND | O_CREAT`, neu angelegt mit Modus
//!   [`EXPORT_FILE_MODE`] (`0640`); ein bestehender, breiter lesbarer
//!   Modus wird auf höchstens `0640` verengt,
//! - kein Symlink, keine Nicht-Datei am Zielpfad (sonst Schreibfehler),
//! - jede Zeile geht in einem einzigen `write_all` hinaus,
//! - Rotation: würde eine Zeile die Datei über [`MAX_EXPORT_BYTES`]
//!   (16 MiB) wachsen lassen, wird sie nach `<pfad>.1` umbenannt (ein
//!   älteres `.1` wird ersetzt) und neu angelegt.
//!
//! # Fehler
//! Ein Schreib-, Öffnungs- oder Rotationsfehler bricht den Sentinel nie ab:
//! er wird gezählt ([`FindingsExporter::failures`] und der Zähler
//! [`FINDINGS_EXPORT_FAILURE_TOTAL`]) und per `tracing::warn!` gemeldet; die
//! nächste Zeile versucht die Datei neu zu öffnen.
//!
//! # Nebenläufigkeit
//! [`FindingsExporter`] lebt ausschließlich im Sammelthread (`&mut self`),
//! wie `crate::poll_once`.
//!
//! # Examples
//! ```rust,ignore
//! let fields = LineFields {
//!     scope: Some("/sys/fs/cgroup/harw.slice"),
//!     ..LineFields::of(&finding, stamp)
//! };
//! assert!(exporter.export(&sink, &fields));
//! ```

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use harw_dod_rules::{Finding, FindingKind, RuleChecked};
use harw_dod_signals::{EventKind, Hardness, SecurityEvent, Severity};
use harw_observe::{MetricValue, TelemetrySink};
use harw_types::{ContentDigest, FindingId, HostId, ImpactSeverity};
use harw_untrusted_text::{escape_line, is_clean};
use jiff::Timestamp;
use serde::Serialize;

use crate::findings::event_kind_label;
use crate::throttle::ThrottleStamp;

/// Größenschwelle, ab der die Exportdatei nach `<pfad>.1` rotiert wird.
pub const MAX_EXPORT_BYTES: u64 = 16 * 1024 * 1024;

/// Längste exportierte `summary` in Bytes (UTF-8, Escapes und `…`
/// eingerechnet).
///
/// Gleich `harw_security_hub::findings::FindingLimits::max_summary_chars`
/// (512) — in Bytes gemessen ist das eine strengere Grenze; der lesende
/// `harw-dod-client` kappt mit derselben Byte-Grenze.
pub const MAX_SUMMARY_BYTES: usize = 512;

/// Modus einer neu angelegten Exportdatei: Eigentümer lesen/schreiben,
/// Gruppe lesen (der Hub liest über Gruppenmitgliedschaft), sonst nichts.
pub const EXPORT_FILE_MODE: u32 = 0o640;

/// Kernel-Hostname, Vorgabe für das `host`-Feld ohne `--findings-host`.
pub const HOSTNAME_PATH: &str = "/proc/sys/kernel/hostname";

/// Schema version written into every export line (`schema_version`).
///
/// Version 1 is the original six-field line without this field; version 2
/// adds `schema_version` itself and the optional `scope`; version 3 adds the
/// PL-100 WP0.12 fields. Every change is additive (see the module
/// documentation).
pub const EXPORT_SCHEMA_VERSION: u32 = 3;

/// Longest exported `scope` (cgroup path) in bytes; a longer value is
/// withheld (`scope_obfuscated`) rather than truncated, because a cut path
/// would name a different cgroup.
pub const MAX_SCOPE_BYTES: usize = 4096;

/// Largest exported `evidence_card`, serialized as compact JSON (4 KiB);
/// the same limit the reading `harw-dod-client` enforces.
pub const MAX_EVIDENCE_CARD_BYTES: usize = 4 * 1024;

/// Longest `evidence_card.path`, in characters (escapes and `…` included).
pub const MAX_EVIDENCE_PATH_CHARS: usize = 256;

/// Longest `evidence_card.process.comm`, in characters (escapes and `…`
/// included).
///
/// The same bound as `harw_dod_signals::ProcessContext::COMM_MAX_CHARS`
/// (PL-100 WP0.15): a `comm` the producing sensor already escaped and
/// bounded fits unchanged; escaping it again here is idempotent and only
/// guards against a producer that did not.
pub const MAX_EVIDENCE_COMM_CHARS: usize = harw_dod_signals::ProcessContext::COMM_MAX_CHARS;

/// Longest event kind key of `evidence_card.event_counts`, in characters.
const MAX_EVENT_KIND_CHARS: usize = 64;

harw_macros::metrics! {
    /// Wie oft eine Befundzeile seit Prozessstart nicht in die
    /// JSON-Lines-Exportdatei geschrieben werden konnte (Öffnen, Rotation,
    /// Kodierung oder Schreiben). Erwartet: `0`.
    FINDINGS_EXPORT_FAILURE_TOTAL: counter, unit = count, labels = [], cardinality = single,
        name = "harw_sentinel_findings_export_failure_total";
}

/// Eine Exportzeile — die ersten sechs Feldnamen und -typen spiegeln exakt
/// `harw_security_hub::findings::DodFinding`; `schema_version` und `scope`
/// kamen mit Version 2, alle Felder ab `instance` mit Version 3 (siehe
/// Moduldoku; die Reihenfolge ist die Schreibreihenfolge).
#[derive(Debug, Serialize)]
struct ExportLine<'a> {
    schema_version: u32,
    finding_id: &'a FindingId,
    host: &'a HostId,
    rule_id: &'a str,
    severity: ImpactSeverity,
    summary: String,
    observed_at: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<&'a str>,
    instance: &'a str,
    seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    record_digest: Option<ContentDigest>,
    dedup_key: String,
    occurrences: u64,
    first_seen: Timestamp,
    kind: &'static str,
    hardness: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cgroup_kid: Option<u64>,
    scope_obfuscated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence_card: Option<&'a EvidenceCard>,
}

/// Alles, was eine Exportzeile über einen Befund trägt (Schema v3).
///
/// # Description
/// [`Self::of`] füllt die Felder des Befundes und des Throttle-Stempels;
/// Herkunft (`scope`, `cgroup_kid`), `record_digest` und `evidence_card`
/// setzt der Aufrufer per Struct-Update, sobald er sie kennt
/// (`crate::findings::report_findings`). `host`, `instance` und `seq` kommen
/// vom [`FindingsExporter`].
///
/// # Concurrency
/// Geliehener Werttyp ohne inneren Zustand.
///
/// # Examples
/// ```rust,ignore
/// let fields = LineFields {
///     cgroup_kid: Some(4242),
///     ..LineFields::of(&finding, stamp)
/// };
/// ```
#[derive(Debug, Clone)]
pub struct LineFields<'a> {
    /// Kennung des Befundes (UUIDv4, `FindingId::new`).
    pub finding_id: &'a FindingId,
    /// Auslösende Regel.
    pub rule_id: &'a str,
    /// Art des Befundes (`kind`).
    pub kind: FindingKind,
    /// Schwere.
    pub severity: Severity,
    /// Härte des Nachweises (`hardness`).
    pub hardness: Hardness,
    /// Rohe Zusammenfassung; escapt erst [`encode_line`].
    pub summary: &'a str,
    /// Ereigniszeit des Befundes.
    pub observed_at: Timestamp,
    /// Rohe Cgroup-Zuordnung; geprüft erst [`encode_line`] (`scope`,
    /// `scope_obfuscated`).
    pub scope: Option<&'a str>,
    /// Kernel-ID der Cgroup des Akteurs.
    pub cgroup_kid: Option<u64>,
    /// `dedup_key`, `occurrences`, `first_seen` aus dem Throttle.
    pub stamp: ThrottleStamp,
    /// Digest des an den Spool übergebenen `FindingRecord`.
    pub record_digest: Option<ContentDigest>,
    /// Zusammenfassung der auslösenden Ereignisse.
    pub evidence_card: Option<&'a EvidenceCard>,
}

impl<'a> LineFields<'a> {
    /// Die Felder eines geprüften Befundes mit seinem Throttle-Stempel; ohne
    /// Scope, Cgroup, Record und Karte.
    ///
    /// # Arguments
    /// - `finding` (`&Finding<RuleChecked>`): der gemeldete Befund.
    /// - `stamp` (`ThrottleStamp`): seine Entscheidung im Throttle.
    ///
    /// # Returns
    /// Die Felder; `scope`, `cgroup_kid`, `record_digest` und
    /// `evidence_card` sind `None`.
    #[must_use]
    pub fn of(finding: &'a Finding<RuleChecked>, stamp: ThrottleStamp) -> Self {
        Self {
            finding_id: finding.id(),
            rule_id: finding.rule_id(),
            kind: finding.kind(),
            severity: finding.severity(),
            hardness: finding.hardness(),
            summary: finding.summary(),
            observed_at: finding.observed_at(),
            scope: None,
            cgroup_kid: None,
            stamp,
            record_digest: None,
            evidence_card: None,
        }
    }
}

/// Kleine, escapte Zusammenfassung der auslösenden Ereignisse eines
/// Befundes (`evidence_card`, Export v3).
///
/// # Description
/// Gebaut von [`Self::from_events`]. Ein Feld ist nur gesetzt, wenn **alle**
/// Ereignisse, die es tragen, denselben Wert haben — nie geraten. Jede
/// Zeichenkette ist mit `harw_untrusted_text::escape_line` escapt und
/// begrenzt; serialisiert ist die Karte höchstens
/// [`MAX_EVIDENCE_CARD_BYTES`] groß. Die Feldreihenfolge ist die
/// Schreibreihenfolge.
///
/// # Concurrency
/// Reiner Werttyp, `Send + Sync`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EvidenceCard {
    /// Anzahl der auslösenden Ereignisse je Art (`crate::findings::event_kind_label`).
    pub event_counts: BTreeMap<String, u64>,
    /// uid des Akteurs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    /// Ziel eines ausgehenden Flusses als `ip:port` (nur mit IP-Literal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    /// Escapter Pfad (exec, Dateischreiben), höchstens
    /// [`MAX_EVIDENCE_PATH_CHARS`] Zeichen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Der handelnde Prozess, aus `Actor::process` der auslösenden
    /// Ereignisse (PL-100 WP0.15).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<EvidenceProcess>,
}

/// Der handelnde Prozess einer [`EvidenceCard`].
///
/// # Description
/// Abgeleitet aus `harw_dod_signals::ProcessContext` (`Actor::process`):
/// nur `pid` und `comm`; `ppid` und `euid` bleiben im Spool-Record und
/// erreichen die Exportzeile nicht.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceProcess {
    /// Prozess-ID zum Beobachtungszeitpunkt.
    pub pid: u32,
    /// Escapter Prozessname, höchstens [`MAX_EVIDENCE_COMM_CHARS`] Zeichen;
    /// leer, wenn die Quelle keinen kennt.
    pub comm: String,
}

impl EvidenceCard {
    /// Fasst die auslösenden Ereignisse eines Befundes zusammen.
    ///
    /// # Description
    /// `event_counts` zählt jedes Ereignis unter seiner Art. `uid`,
    /// `destination` (nur `EgressFlow` mit IP-Literal), `path` (`ProcessExec`,
    /// `FileWrite`) und `process` werden gesetzt, wenn sich alle Ereignisse,
    /// die das Merkmal tragen, auf einen Wert einigen. `process` stammt aus
    /// `Actor::process` (PL-100 WP0.15): `pid` und `comm` müssen bei allen
    /// Ereignissen mit Prozesskontext gleich sein, Ereignisse ohne zählen
    /// nicht mit; ein fehlender `comm` wird zur leeren Zeichenkette, jeder
    /// andere wird escapt und auf [`MAX_EVIDENCE_COMM_CHARS`] Zeichen
    /// begrenzt. Wäre die Karte größer
    /// als [`MAX_EVIDENCE_CARD_BYTES`], fällt zuerst `path` weg; reicht das
    /// nicht, gibt es keine Karte.
    ///
    /// # Arguments
    /// - `events` (`&[&SecurityEvent]`): die auslösenden Ereignisse.
    ///
    /// # Returns
    /// `None` ohne Ereignisse oder wenn die Karte die Größengrenze sprengt.
    #[must_use]
    pub fn from_events(events: &[&SecurityEvent]) -> Option<Self> {
        if events.is_empty() {
            return None;
        }
        let mut event_counts: BTreeMap<String, u64> = BTreeMap::new();
        for event in events {
            let kind = escape_line(event_kind_label(&event.kind), MAX_EVENT_KIND_CHARS);
            let count = event_counts.entry(kind).or_insert(0);
            *count = count.saturating_add(1);
        }
        let uid = agreed(
            events
                .iter()
                .filter_map(|event| event.actor.as_ref().map(|actor| actor.uid)),
        );
        let destination = agreed(events.iter().filter_map(|event| match &event.kind {
            EventKind::EgressFlow { destination, port } => Some((destination.as_str(), *port)),
            _ => None,
        }))
        .and_then(|(destination, port)| {
            destination
                .parse::<IpAddr>()
                .ok()
                .map(|ip| SocketAddr::new(ip, port).to_string())
        });
        let path = agreed(events.iter().filter_map(|event| match &event.kind {
            EventKind::ProcessExec { path, .. } | EventKind::FileWrite { path } => {
                Some(path.as_str())
            }
            _ => None,
        }))
        .map(|path| escape_line(path, MAX_EVIDENCE_PATH_CHARS))
        .filter(|path| !path.is_empty());
        let process = agreed(events.iter().filter_map(|event| {
            let process = event.actor.as_ref()?.process.as_ref()?;
            Some((process.pid, process.comm.as_deref()))
        }))
        .map(|(pid, comm)| EvidenceProcess {
            pid,
            comm: escape_line(comm.unwrap_or_default(), MAX_EVIDENCE_COMM_CHARS),
        });
        let mut card = Self {
            event_counts,
            uid,
            destination,
            path,
            process,
        };
        if card.encoded_len() > MAX_EVIDENCE_CARD_BYTES {
            card.path = None;
        }
        (card.encoded_len() <= MAX_EVIDENCE_CARD_BYTES).then_some(card)
    }

    /// Länge der kompakten JSON-Form; `usize::MAX`, falls sie scheitert.
    fn encoded_len(&self) -> usize {
        serde_json::to_vec(self).map_or(usize::MAX, |bytes| bytes.len())
    }
}

/// Der eine Wert, auf den sich alle Elemente einigen; `None` ohne Element
/// oder bei einem Widerspruch.
fn agreed<T: PartialEq>(values: impl IntoIterator<Item = T>) -> Option<T> {
    let mut values = values.into_iter();
    let first = values.next()?;
    values.all(|value| value == first).then_some(first)
}

/// Bildet die DoD-Schwere erschöpfend auf die hub-seitige Skala ab.
///
/// Erschöpfendes `match` ohne Wildcard: eine neue Variante bricht den Build,
/// statt still falsch exportiert zu werden.
#[must_use]
pub const fn impact_severity(severity: Severity) -> ImpactSeverity {
    match severity {
        Severity::Info => ImpactSeverity::Info,
        Severity::Low => ImpactSeverity::Low,
        Severity::Medium => ImpactSeverity::Medium,
        Severity::High => ImpactSeverity::High,
        Severity::Critical => ImpactSeverity::Critical,
    }
}

/// Die Export-Schreibweise von `kind` (snake_case, Export v3).
///
/// Erschöpfendes `match` ohne Wildcard, wie [`impact_severity`].
#[must_use]
pub const fn kind_label(kind: FindingKind) -> &'static str {
    match kind {
        FindingKind::RuleTriggered => "rule_triggered",
        FindingKind::Anomaly => "anomaly",
    }
}

/// Die Export-Schreibweise von `hardness` (Export v3).
///
/// Erschöpfendes `match` ohne Wildcard, wie [`impact_severity`].
#[must_use]
pub const fn hardness_label(hardness: Hardness) -> &'static str {
    match hardness {
        Hardness::Observed => "observed",
        Hardness::Correlated => "correlated",
        Hardness::Inferred => "inferred",
    }
}

/// Escapt eine Befund-Zusammenfassung für den Export.
///
/// # Description
/// Dünne Hülle um `harw_untrusted_text::escape_line`: jedes steuernde oder
/// unsichtbare Zeichen (auch `\n`, `\r`, Bidi, Tag-Block, Variation
/// Selectors) wird zu einem sichtbaren `\u{…}`, und das Ergebnis ist
/// höchstens [`MAX_SUMMARY_BYTES`] Bytes lang. Gekürzt wird an einer
/// Zeichengrenze, nie mitten in einer Ersatzform, mit `…` am Ende.
///
/// # Arguments
/// - `raw` (`&str`): die rohe, erzeugerbeeinflusste Zusammenfassung.
///
/// # Returns
/// Die einzeilige, escapte, begrenzte Zusammenfassung.
#[must_use]
pub fn sanitize_summary(raw: &str) -> String {
    // `escape_line` begrenzt Zeichen, der Vertrag Bytes: so lange weniger
    // Zeichen zulassen, bis die Bytes passen. Jedes Zeichen hat höchstens vier
    // Bytes, also nähert sich das Budget in wenigen Schritten; bei `0` ist die
    // Ausgabe leer und passt immer.
    let mut budget = MAX_SUMMARY_BYTES;
    loop {
        let escaped = escape_line(raw, budget);
        let Some(over) = escaped
            .len()
            .checked_sub(MAX_SUMMARY_BYTES)
            .filter(|over| *over > 0)
        else {
            return escaped;
        };
        budget = budget.saturating_sub(over.div_ceil(4).max(1));
    }
}

/// Validates an optional `scope` (cgroup path) for the export.
///
/// # Description
/// Thin wrapper around `harw_untrusted_text::is_clean`: returns the value
/// unchanged when it is non-empty, absolute, at most [`MAX_SCOPE_BYTES`]
/// bytes long and free of every character the shared escaper would escape.
/// Anything else yields `None` instead of a rewritten or truncated path that
/// would name a different cgroup; [`encode_line`] then sets
/// `scope_obfuscated` for a non-empty scope (PL-100 WP0.9).
#[must_use]
pub fn sanitize_scope(raw: Option<&str>) -> Option<&str> {
    let scope = raw?;
    let acceptable = scope.starts_with('/') && scope.len() <= MAX_SCOPE_BYTES && is_clean(scope);
    acceptable.then_some(scope)
}

/// Kodiert eine Exportzeile (Schema v3) inklusive abschließendem `\n`.
///
/// # Description
/// `summary` geht durch [`sanitize_summary`], `scope` durch
/// [`sanitize_scope`]; ein nicht leerer, aber nicht darstellbarer Scope wird
/// weggelassen und als `scope_obfuscated: true` gemeldet.
///
/// # Arguments
/// - `fields` (`&LineFields`): der Befund samt Stempel, Herkunft, Record und
///   Karte.
/// - `host` (`&HostId`): das `host`-Feld.
/// - `instance` (`&str`): UUIDv4 dieses Prozesses.
/// - `seq` (`u64`): Position der Zeile in `instance`.
///
/// # Errors
/// `serde_json::Error`, falls die Serialisierung scheitert (für diese
/// Feldtypen praktisch unerreichbar, aber nicht als Panik behandelt).
pub fn encode_line(
    fields: &LineFields<'_>,
    host: &HostId,
    instance: &str,
    seq: u64,
) -> Result<String, serde_json::Error> {
    let scope = sanitize_scope(fields.scope);
    let scope_obfuscated = scope.is_none() && fields.scope.is_some_and(|raw| !raw.is_empty());
    let line = ExportLine {
        schema_version: EXPORT_SCHEMA_VERSION,
        finding_id: fields.finding_id,
        host,
        rule_id: fields.rule_id,
        severity: impact_severity(fields.severity),
        summary: sanitize_summary(fields.summary),
        observed_at: fields.observed_at,
        scope,
        instance,
        seq,
        record_digest: fields.record_digest,
        dedup_key: fields.stamp.dedup_key.to_string(),
        occurrences: fields.stamp.occurrences,
        first_seen: fields.stamp.first_seen,
        kind: kind_label(fields.kind),
        hardness: hardness_label(fields.hardness),
        cgroup_kid: fields.cgroup_kid,
        scope_obfuscated,
        evidence_card: fields.evidence_card,
    };
    let mut encoded = serde_json::to_string(&line)?;
    encoded.push('\n');
    Ok(encoded)
}

/// Ermittelt das `host`-Feld: `explicit` (aus `--findings-host`) oder, ohne
/// Angabe, den Kernel-Hostnamen aus `hostname_path`.
///
/// # Returns
/// `None`, wenn weder eine Angabe vorliegt noch der Hostname lesbar, nicht
/// leer und frei von zu escapenden Zeichen ist — der Export bleibt dann aus
/// (siehe `crate::run`).
#[must_use]
pub fn resolve_host(explicit: Option<&HostId>, hostname_path: &Path) -> Option<HostId> {
    if let Some(host) = explicit {
        return Some(host.clone());
    }
    let raw = fs::read_to_string(hostname_path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() || !is_clean(trimmed) {
        return None;
    }
    HostId::try_from_str(trimmed).ok()
}

/// Pfad der rotierten Vorgängerdatei: `<pfad>.1`.
fn rotated_path(path: &Path) -> PathBuf {
    generation_path(path, 1)
}

/// `<pfad>.<generation>`.
fn generation_path(path: &Path, generation: u32) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(format!(".{generation}"));
    PathBuf::from(name)
}

/// Öffnet die Exportdatei append-only; legt sie mit [`EXPORT_FILE_MODE`] an.
fn open_export_file(path: &Path) -> io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "findings export path exists but is not a regular file",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(EXPORT_FILE_MODE);
    }
    let file = options.open(path)?;
    restrict_mode(&file)?;
    Ok(file)
}

/// Verengt den Modus einer bereits bestehenden Datei auf höchstens `0640`.
#[cfg(unix)]
fn restrict_mode(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = file.metadata()?.permissions().mode() & 0o7777;
    if mode & !EXPORT_FILE_MODE != 0 {
        file.set_permissions(fs::Permissions::from_mode(mode & EXPORT_FILE_MODE))?;
    }
    Ok(())
}

/// Siehe die Unix-Fassung; ohne Unix-Modusbits ein No-Op.
#[cfg(not(unix))]
fn restrict_mode(_file: &File) -> io::Result<()> {
    Ok(())
}

/// Why an export destination was refused at startup (PL-97, PR #154 P1).
///
/// # Description
/// Returned by [`validate_export_path`] and
/// [`check_distinct_destinations`]; the sentinel refuses to start on any of
/// these (fail-closed) instead of letting the atomic health rename replace
/// the visible findings file while [`FindingsExporter`] keeps appending to
/// the then unlinked inode.
#[derive(Debug)]
pub enum ExportPathError {
    /// The path is not absolute or carries a non-normal component (`.`,
    /// `..`, an empty component from `//`, or a trailing `/`), or has no
    /// file name.
    NonNormal {
        /// Which export the path belongs to (`findings` or `health`).
        role: &'static str,
        /// The refused path as given.
        path: PathBuf,
    },
    /// The parent directories differ lexically and at least one of them
    /// could not be resolved, so distinct identities cannot be proven.
    Unresolved {
        /// The parent directory that could not be canonicalised.
        parent: PathBuf,
        /// The underlying resolution error.
        source: io::Error,
    },
    /// Both exports resolve to the same file (same canonical parent and
    /// same file name).
    Collision {
        /// The findings export path as given.
        findings: PathBuf,
        /// The health export path as given.
        health: PathBuf,
        /// The common resolved destination.
        resolved: PathBuf,
    },
    /// Both exports live in the same resolved directory and a file the
    /// health writer touches (its destination or its temporary sibling
    /// `.<name>.tmp`) is the findings export or one of its rotated
    /// generations `<findings>.N`.
    SiblingOverlap {
        /// The findings export path as given.
        findings: PathBuf,
        /// The health export path as given.
        health: PathBuf,
        /// The resolved file both writers would touch.
        resolved: PathBuf,
    },
}

impl std::fmt::Display for ExportPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonNormal { role, path } => write!(
                f,
                "{role} export path '{}' must be absolute with only normal components (no '.', '..', '//' or trailing '/')",
                path.display()
            ),
            Self::Unresolved { parent, source } => write!(
                f,
                "cannot resolve export directory '{}' to prove the findings and health exports are distinct: {source}",
                parent.display()
            ),
            Self::Collision {
                findings,
                health,
                resolved,
            } => write!(
                f,
                "findings export '{}' and health export '{}' resolve to the same file '{}'",
                findings.display(),
                health.display(),
                resolved.display()
            ),
            Self::SiblingOverlap {
                findings,
                health,
                resolved,
            } => write!(
                f,
                "health export '{}' (or its temporary sibling) and findings export '{}' (or a rotated generation) would both touch '{}'",
                health.display(),
                findings.display(),
                resolved.display()
            ),
        }
    }
}

impl std::error::Error for ExportPathError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unresolved { source, .. } => Some(source),
            Self::NonNormal { .. } | Self::Collision { .. } | Self::SiblingOverlap { .. } => None,
        }
    }
}

/// Checks that `path` is absolute and spelled with normal components only.
///
/// # Description
/// Works on the raw bytes, not on [`Path::components`], because the latter
/// silently drops interior `.` components, repeated separators and a
/// trailing `/` — exactly the spellings that alias another path lexically.
/// Refused: a relative path, any `.` or `..` component, an empty component
/// (`//` or a trailing `/`), and a path without a file name.
///
/// # Errors
/// [`ExportPathError::NonNormal`] naming `role` and the path.
pub fn validate_export_path(role: &'static str, path: &Path) -> Result<(), ExportPathError> {
    let refuse = || ExportPathError::NonNormal {
        role,
        path: path.to_path_buf(),
    };
    let bytes = path.as_os_str().as_encoded_bytes();
    let Some(rest) = bytes.strip_prefix(b"/") else {
        return Err(refuse());
    };
    let normal = rest
        .split(|byte| *byte == b'/')
        .all(|part| !part.is_empty() && part != b"." && part != b"..");
    if normal && path.file_name().is_some() {
        Ok(())
    } else {
        Err(refuse())
    }
}

/// Refuses a findings and a health export that resolve to the same file.
///
/// # Description
/// Both paths are first checked by [`validate_export_path`]. Then the
/// parent directories are compared by resolved identity: equal lexical
/// parents are the same directory; otherwise both are canonicalised
/// ([`fs::canonicalize`], which resolves symlinked parent aliases). If the
/// parents differ lexically and either cannot be resolved (for example it
/// does not exist yet), distinctness cannot be proven and the check fails
/// closed. Finally, within one resolved directory, the file names are
/// compared: an equal name is a collision; a health name or health
/// temporary sibling (`.<name>.tmp`) that is the findings name or a rotated
/// generation `<findings>.N` is a sibling overlap. The final component
/// itself is never followed: both writers already refuse a symlink there.
///
/// # Errors
/// [`ExportPathError::NonNormal`], [`ExportPathError::Unresolved`],
/// [`ExportPathError::Collision`] or [`ExportPathError::SiblingOverlap`].
pub fn check_distinct_destinations(findings: &Path, health: &Path) -> Result<(), ExportPathError> {
    validate_export_path("findings", findings)?;
    validate_export_path("health", health)?;
    let (Some(findings_parent), Some(health_parent)) = (findings.parent(), health.parent()) else {
        // Nach `validate_export_path` hat jeder Pfad einen Elternteil; ohne
        // ihn lässt sich nichts beweisen — also ablehnen.
        return Err(ExportPathError::NonNormal {
            role: "findings",
            path: findings.to_path_buf(),
        });
    };
    let (findings_dir, health_dir) = if findings_parent == health_parent {
        (findings_parent.to_path_buf(), health_parent.to_path_buf())
    } else {
        let resolve = |parent: &Path| {
            fs::canonicalize(parent).map_err(|source| ExportPathError::Unresolved {
                parent: parent.to_path_buf(),
                source,
            })
        };
        (resolve(findings_parent)?, resolve(health_parent)?)
    };
    if findings_dir != health_dir {
        return Ok(());
    }
    let (Some(findings_name), Some(health_name)) = (findings.file_name(), health.file_name())
    else {
        return Err(ExportPathError::NonNormal {
            role: "findings",
            path: findings.to_path_buf(),
        });
    };
    let resolved = |name: &OsStr| findings_dir.join(name);
    if findings_name == health_name {
        return Err(ExportPathError::Collision {
            findings: findings.to_path_buf(),
            health: health.to_path_buf(),
            resolved: resolved(findings_name),
        });
    }
    // Der Gesundheitsschreiber berührt neben seinem Ziel auch die temporäre
    // Geschwisterdatei `.<name>.tmp` (entfernen, neu anlegen, umbenennen);
    // der Befundschreiber neben seinem Ziel die Generationen `<name>.N`.
    // Keine der Mengen darf die andere schneiden.
    let mut health_temporary = OsString::from(".");
    health_temporary.push(health_name);
    health_temporary.push(".tmp");
    for touched in [health_name, health_temporary.as_os_str()] {
        if is_findings_file(findings_name, touched) {
            return Err(ExportPathError::SiblingOverlap {
                findings: findings.to_path_buf(),
                health: health.to_path_buf(),
                resolved: resolved(touched),
            });
        }
    }
    Ok(())
}

/// Whether `name` is the findings export `findings_name` itself or one of
/// its rotated generations `<findings_name>.<digits>`.
fn is_findings_file(findings_name: &OsStr, name: &OsStr) -> bool {
    let findings = findings_name.as_encoded_bytes();
    let name = name.as_encoded_bytes();
    if name == findings {
        return true;
    }
    name.strip_prefix(findings)
        .and_then(|rest| rest.strip_prefix(b"."))
        .is_some_and(|digits| !digits.is_empty() && digits.iter().all(u8::is_ascii_digit))
}

/// Append-only JSON-Lines-Schreiber für zertifizierte Befunde.
///
/// # Description
/// Hält die offene Datei, den `host`-Wert jeder Zeile, die `instance` dieses
/// Prozesses (UUIDv4, neu je Start), die nächste Folgenummer `seq` und die
/// Zähler für geschriebene und gescheiterte Zeilen. Siehe Moduldoku für
/// Format, Rotation und Fehlerverhalten.
#[derive(Debug)]
pub struct FindingsExporter {
    path: PathBuf,
    rotated: PathBuf,
    /// Anzahl aufbewahrter rotierter Generationen (`<pfad>.1` … `<pfad>.N`).
    keep: u32,
    host: HostId,
    /// UUIDv4 dieses Prozesses (`instance`, Export v3).
    instance: String,
    /// Folgenummer der nächsten Zeile (`seq`, ab 1).
    next_seq: u64,
    max_bytes: u64,
    file: Option<File>,
    written: u64,
    failures: u64,
}

impl FindingsExporter {
    /// Baut einen Exporter für `path` mit der Rotationsschwelle
    /// [`MAX_EXPORT_BYTES`] und einer frischen `instance`. Öffnet noch
    /// nichts — siehe [`Self::prepare`].
    #[must_use]
    pub fn new(path: PathBuf, host: HostId) -> Self {
        Self::with_max_bytes(path, host, MAX_EXPORT_BYTES)
    }

    /// Wie [`Self::new`], mit frei wählbarer Rotationsschwelle (Tests).
    #[must_use]
    pub fn with_max_bytes(path: PathBuf, host: HostId, max_bytes: u64) -> Self {
        let rotated = rotated_path(&path);
        Self {
            path,
            rotated,
            keep: 1,
            host,
            instance: uuid::Uuid::new_v4().to_string(),
            next_seq: 1,
            max_bytes,
            file: None,
            written: 0,
            failures: 0,
        }
    }

    /// Bewahrt `keep` rotierte Generationen auf (`<pfad>.1` neueste …
    /// `<pfad>.<keep>` älteste; Standard 1, mindestens 1). Die älteste fällt
    /// bei der nächsten Rotation heraus — der Platzbedarf ist damit durch
    /// `(keep + 1) × max_bytes` begrenzt.
    #[must_use]
    pub fn with_keep(mut self, keep: u32) -> Self {
        self.keep = keep.max(1);
        self
    }

    /// Zielpfad der Exportdatei.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Die `instance` dieses Exporters (UUIDv4).
    #[cfg(test)]
    #[must_use]
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// Verzeichnis der Exportdatei — muss für Rotation und Neuanlage
    /// beschreibbar bleiben (Landlock-Regel in `crate::run`).
    #[must_use]
    pub fn directory(&self) -> Option<&Path> {
        self.path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
    }

    /// Anzahl erfolgreich geschriebener Zeilen seit Prozessstart.
    #[must_use]
    pub const fn written(&self) -> u64 {
        self.written
    }

    /// Anzahl gescheiterter Zeilen (bzw. Öffnungsversuche) seit Prozessstart.
    #[must_use]
    pub const fn failures(&self) -> u64 {
        self.failures
    }

    /// Öffnet die Datei vorab (vor der Landlock-Selbstbeschränkung), damit
    /// ein Konfigurationsfehler schon beim Start im Log steht.
    ///
    /// Ein Fehlschlag wird gezählt und geloggt, nie zurückgegeben.
    pub fn prepare(&mut self, sink: &dyn TelemetrySink) {
        let opened = self.ensure_open().map(|_| ());
        if let Err(error) = opened {
            self.record_failure(sink, &error, "open");
        }
    }

    /// Schreibt eine Zeile (Schema v3) für `fields`.
    ///
    /// # Description
    /// Vergibt die nächste Folgenummer `seq` — auch dann, wenn die Zeile
    /// danach scheitert: Nummern bleiben eindeutig, eine Lücke zeigt eine
    /// verlorene Zeile —, kodiert über [`encode_line`] und hängt die Zeile
    /// an (mit Rotation, siehe Moduldoku).
    ///
    /// # Arguments
    /// - `sink` (`&dyn TelemetrySink`): erhält
    ///   [`FINDINGS_EXPORT_FAILURE_TOTAL`] bei einem Fehler.
    /// - `fields` (`&LineFields`): der Befund samt Stempel, Herkunft, Record
    ///   und Karte.
    ///
    /// # Returns
    /// `true`, wenn die Zeile geschrieben wurde; `false` nach einem
    /// gezählten und geloggten Fehler.
    pub fn export(&mut self, sink: &dyn TelemetrySink, fields: &LineFields<'_>) -> bool {
        let seq = self.next_seq;
        // 2^64 Zeilen je Prozess sind unerreichbar; die Sättigung verhindert
        // nur einen Überlauf, keine realistische Wiederholung.
        self.next_seq = self.next_seq.saturating_add(1);
        let line = match encode_line(fields, &self.host, &self.instance, seq) {
            Ok(line) => line,
            Err(error) => {
                self.record_failure(sink, &error, "encode");
                return false;
            }
        };
        match self.append(line.as_bytes()) {
            Ok(()) => {
                self.written = self.written.saturating_add(1);
                true
            }
            Err(error) => {
                self.record_failure(sink, &error, "write");
                false
            }
        }
    }

    fn ensure_open(&mut self) -> io::Result<&mut File> {
        let file = match self.file.take() {
            Some(file) => file,
            None => open_export_file(&self.path)?,
        };
        Ok(self.file.insert(file))
    }

    fn append(&mut self, line: &[u8]) -> io::Result<()> {
        let len = u64::try_from(line.len()).unwrap_or(u64::MAX);
        let size = self.ensure_open()?.metadata()?.len();
        if size > 0 && size.saturating_add(len) > self.max_bytes {
            self.rotate()?;
        }
        let result = self.ensure_open()?.write_all(line);
        if result.is_err() {
            // Beim nächsten Mal frisch öffnen statt auf einem kaputten
            // Deskriptor weiterzuschreiben.
            self.file = None;
        }
        result
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        // Ältere Generationen nach hinten schieben (`.N-1` → `.N`, …,
        // `.1` → `.2`); die älteste wird dabei ersetzt. Fehlende Stufen sind
        // kein Fehler.
        for generation in (1..self.keep).rev() {
            let from = generation_path(&self.path, generation);
            let to = generation_path(&self.path, generation + 1);
            match fs::rename(&from, &to) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        match fs::rename(&self.path, &self.rotated) {
            Ok(()) => {
                tracing::info!(
                    path = %self.path.display(),
                    rotated = %self.rotated.display(),
                    keep = self.keep,
                    written = self.written(),
                    "findings export rotated"
                );
                Ok(())
            }
            // Extern entfernt: nichts zu rotieren, einfach neu anlegen.
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn record_failure(
        &mut self,
        sink: &dyn TelemetrySink,
        error: &dyn std::fmt::Display,
        stage: &'static str,
    ) {
        self.failures = self.failures.saturating_add(1);
        sink.record(&FINDINGS_EXPORT_FAILURE_TOTAL, MetricValue::Count(1), &[]);
        tracing::warn!(
            error = %error,
            stage,
            path = %self.path.display(),
            failures = self.failures(),
            "findings export failed; the finding is still logged and counted"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::throttle::{CoarseSubject, DedupKey};
    use harw_observe::NullSink;
    use serde::Deserialize;

    /// Spiegel von `harw_security_hub::findings::DodFinding` — derselbe
    /// Feldsatz, dieselben Typen (inklusive der v3-Felder, die der Hub liest:
    /// `schema_version`, `dedup_key`, `occurrences`), und wie dort **ohne**
    /// `deny_unknown_fields`: der Hub ignoriert die übrigen Felder von
    /// Version 2 und 3. Nur lokaler Rauchtest; verbindlich sind die Tests der
    /// echten Leser gegen [`V3_FIXTURE_LINE`].
    #[derive(Debug, Deserialize)]
    struct HubDodFinding {
        finding_id: FindingId,
        host: HostId,
        rule_id: String,
        severity: ImpactSeverity,
        summary: String,
        observed_at: Timestamp,
        #[serde(default)]
        schema_version: Option<u32>,
        #[serde(default)]
        dedup_key: Option<String>,
        #[serde(default)]
        occurrences: Option<u64>,
    }

    /// Spiegel der Wire-Form, über die harw's `security_signals` liest: seit
    /// PL-100 WP0.11 parst es mit `harw_dod_client::parse_finding_line`
    /// (`RawFindingLine`) — jedes v3-Feld optional, unbekannte Felder
    /// ignoriert. Die Regeln, die der Client zusätzlich prüft, prüft
    /// [`assert_client_contract`].
    #[derive(Debug, Deserialize)]
    struct SecuritySignalsLine {
        #[serde(default)]
        schema_version: Option<u32>,
        finding_id: String,
        rule_id: String,
        severity: String,
        observed_at: String,
        #[serde(default)]
        instance: Option<String>,
        #[serde(default)]
        seq: Option<u64>,
        #[serde(default)]
        record_digest: Option<String>,
        #[serde(default)]
        dedup_key: Option<String>,
        #[serde(default)]
        occurrences: Option<u64>,
        #[serde(default)]
        first_seen: Option<String>,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        hardness: Option<String>,
        #[serde(default)]
        cgroup_kid: Option<u64>,
        #[serde(default)]
        scope_obfuscated: bool,
        #[serde(default)]
        evidence_card: Option<serde_json::Value>,
    }

    /// Strenger Spiegel einer Zeile bis Version 3: `deny_unknown_fields`,
    /// damit der Export **genau** diese Felder trägt und keines mehr; die
    /// v2/v3-Felder sind optional, damit auch v1/v2-Zeilen passen.
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ExportLineV3 {
        #[serde(default = "schema_v1")]
        schema_version: u32,
        finding_id: FindingId,
        host: HostId,
        rule_id: String,
        severity: ImpactSeverity,
        summary: String,
        observed_at: Timestamp,
        #[serde(default)]
        scope: Option<String>,
        #[serde(default)]
        instance: Option<String>,
        #[serde(default)]
        seq: Option<u64>,
        #[serde(default)]
        record_digest: Option<String>,
        #[serde(default)]
        dedup_key: Option<String>,
        #[serde(default)]
        occurrences: Option<u64>,
        #[serde(default)]
        first_seen: Option<Timestamp>,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        hardness: Option<String>,
        #[serde(default)]
        cgroup_kid: Option<u64>,
        #[serde(default)]
        scope_obfuscated: bool,
        #[serde(default)]
        evidence_card: Option<EvidenceCardMirror>,
    }

    /// Strenger Spiegel der `evidence_card` (die Schlüssel, die der Client
    /// bis Version 3 annimmt).
    #[derive(Debug, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct EvidenceCardMirror {
        #[serde(default)]
        event_counts: BTreeMap<String, u64>,
        #[serde(default)]
        uid: Option<u32>,
        #[serde(default)]
        destination: Option<String>,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        process: Option<ProcessMirror>,
    }

    /// Strenger Spiegel von `evidence_card.process`.
    #[derive(Debug, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct ProcessMirror {
        pid: u32,
        comm: String,
    }

    /// A line without `schema_version` is a version-1 line.
    const fn schema_v1() -> u32 {
        1
    }

    /// `true` for a canonical lowercase UUIDv4 (the rule of
    /// `harw_dod_client::findings::is_uuid_v4`).
    fn is_uuid_v4(value: &str) -> bool {
        let bytes = value.as_bytes();
        bytes.len() == 36
            && bytes.iter().enumerate().all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => *byte == b'-',
                14 => *byte == b'4',
                19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
                _ => matches!(byte, b'0'..=b'9' | b'a'..=b'f'),
            })
    }

    /// `true` for exactly `len` lowercase hex characters.
    fn is_lower_hex(value: &str, len: usize) -> bool {
        value.len() == len
            && value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }

    /// Die Regeln, mit denen `harw_dod_client` eine Zeile bis Version 3
    /// annimmt oder verwirft (Kennungen, die der Sentinel selbst erzeugt).
    fn assert_client_contract(line: &SecuritySignalsLine) {
        let v3 = line.schema_version.is_some_and(|version| version >= 3);
        assert!(
            !v3 || is_uuid_v4(&line.finding_id),
            "v3 finding_id is UUIDv4"
        );
        assert!(line.instance.as_deref().is_none_or(is_uuid_v4), "instance");
        assert_ne!(line.seq, Some(0), "seq starts at 1");
        assert!(
            line.record_digest
                .as_deref()
                .is_none_or(|digest| is_lower_hex(digest, 64)),
            "record_digest"
        );
        assert!(
            line.dedup_key
                .as_deref()
                .is_none_or(|key| is_lower_hex(key, 32)),
            "dedup_key"
        );
        assert_ne!(line.occurrences, Some(0), "occurrences >= 1");
        assert!(
            line.first_seen
                .as_deref()
                .is_none_or(|text| text.parse::<Timestamp>().is_ok()),
            "first_seen is RFC 3339"
        );
        assert!(
            line.kind
                .as_deref()
                .is_none_or(|kind| matches!(kind, "rule_triggered" | "anomaly")),
            "kind"
        );
        assert!(
            line.hardness
                .as_deref()
                .is_none_or(|hardness| matches!(hardness, "observed" | "correlated" | "inferred")),
            "hardness"
        );
        assert!(
            line.evidence_card.as_ref().is_none_or(|card| {
                serde_json::to_vec(card).is_ok_and(|bytes| bytes.len() <= MAX_EVIDENCE_CARD_BYTES)
            }),
            "evidence_card <= 4 KiB"
        );
    }

    /// Eine Version-1-Zeile, wie sie vor PL-97 D1 geschrieben wurde.
    const V1_LINE: &str = r#"{"finding_id":"f-1","host":"host-a","rule_id":"structure-drift","severity":"high","summary":"s","observed_at":"2026-09-27T10:00:00Z"}"#;

    /// Eine Version-2-Zeile, wie sie bis PL-100 WP0.12 geschrieben wurde
    /// (dieselbe Zeile steht in den Lesertests von `harw-security-hub` und
    /// `harw-runtime`).
    const V2_FIXTURE_LINE: &str = r#"{"schema_version":2,"finding_id":"f-1","host":"host-a","rule_id":"structure-drift","severity":"high","summary":"s","observed_at":"2026-09-27T10:00:00Z","scope":"/sys/fs/cgroup/harw.slice"}"#;

    /// Canonical version-3 fixture line: byte-for-byte what [`encode_line`]
    /// writes for [`v3_fixture_fields`] (pinned by
    /// `test_v3_fixture_line_is_byte_identical_to_encode_line`) and the same
    /// line as `V3_FIXTURE_LINE` in `harw-security-hub/src/findings.rs`, so a
    /// change to the encoder or to that reader breaks a test on the side that
    /// changed.
    const V3_FIXTURE_LINE: &str = r#"{"schema_version":3,"finding_id":"3f2b8c1e-9d4a-4b7e-8a6f-1c2d3e4f5a6b","host":"host-a","rule_id":"egress-flow","severity":"critical","summary":"tcp connect","observed_at":"2026-09-27T11:00:00Z","scope":"/sys/fs/cgroup/harw.slice","instance":"a1b2c3d4-e5f6-4a7b-9c8d-0e1f2a3b4c5d","seq":7,"record_digest":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","dedup_key":"0123456789abcdef0123456789abcdef","occurrences":3,"first_seen":"2026-09-27T10:00:00Z","kind":"rule_triggered","hardness":"observed","cgroup_kid":4242,"scope_obfuscated":false,"evidence_card":{"event_counts":{"tcp_connect":3},"uid":1000,"destination":"203.0.113.9:443","path":"/usr/bin/curl","process":{"pid":4711,"comm":"curl"}}}"#;

    /// `instance` of every test line encoded directly.
    const INSTANCE: &str = "a1b2c3d4-e5f6-4a7b-9c8d-0e1f2a3b4c5d";

    fn host() -> TestResult<HostId> {
        HostId::try_from_str("host-a").map_err(ctx("host id"))
    }

    fn finding_id() -> TestResult<FindingId> {
        FindingId::try_from_str("3f2b8c1e-9d4a-4b7e-8a6f-1c2d3e4f5a6b").map_err(ctx("finding id"))
    }

    fn read_lines(path: &Path) -> TestResult<Vec<String>> {
        let text = fs::read_to_string(path).map_err(ctx("read export"))?;
        Ok(text.lines().map(str::to_owned).collect())
    }

    /// Ein Throttle-Stempel ohne Wiederholung.
    fn stamp_at(first_seen: Timestamp) -> ThrottleStamp {
        ThrottleStamp {
            dedup_key: DedupKey::derive(
                "structure-drift",
                None,
                None,
                &CoarseSubject::of_summary("s"),
            ),
            occurrences: 1,
            first_seen,
        }
    }

    /// Felder eines Befundes ohne Herkunft, Record und Karte.
    fn fields<'a>(
        finding_id: &'a FindingId,
        rule_id: &'a str,
        severity: Severity,
        summary: &'a str,
        observed_at: Timestamp,
        scope: Option<&'a str>,
    ) -> LineFields<'a> {
        LineFields {
            finding_id,
            rule_id,
            kind: FindingKind::RuleTriggered,
            severity,
            hardness: Hardness::Observed,
            summary,
            observed_at,
            scope,
            cgroup_kid: None,
            stamp: stamp_at(observed_at),
            record_digest: None,
            evidence_card: None,
        }
    }

    fn encode(fields: &LineFields<'_>) -> TestResult<String> {
        encode_line(fields, &host()?, INSTANCE, 1).map_err(ctx("encode"))
    }

    fn at(text: &str) -> TestResult<Timestamp> {
        text.parse().map_err(ctx("timestamp"))
    }

    /// Die Karte der Fixture-Zeile.
    fn fixture_card() -> EvidenceCard {
        EvidenceCard {
            event_counts: BTreeMap::from([("tcp_connect".to_owned(), 3)]),
            uid: Some(1000),
            destination: Some("203.0.113.9:443".to_owned()),
            path: Some("/usr/bin/curl".to_owned()),
            process: Some(EvidenceProcess {
                pid: 4711,
                comm: "curl".to_owned(),
            }),
        }
    }

    /// Kodiert die Fixture-Zeile mit `seq = 7`.
    fn encode_v3_fixture(id: &FindingId, card: &EvidenceCard) -> TestResult<String> {
        let dedup_key = DedupKey::parse_hex("0123456789abcdef0123456789abcdef")
            .ok_or(TestError::Missing("fixture dedup key"))?;
        let record_digest: ContentDigest =
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .parse()
                .map_err(ctx("fixture record digest"))?;
        let fields = LineFields {
            finding_id: id,
            rule_id: "egress-flow",
            kind: FindingKind::RuleTriggered,
            severity: Severity::Critical,
            hardness: Hardness::Observed,
            summary: "tcp connect",
            observed_at: at("2026-09-27T11:00:00Z")?,
            scope: Some("/sys/fs/cgroup/harw.slice"),
            cgroup_kid: Some(4242),
            stamp: ThrottleStamp {
                dedup_key,
                occurrences: 3,
                first_seen: at("2026-09-27T10:00:00Z")?,
            },
            record_digest: Some(record_digest),
            evidence_card: Some(card),
        };
        encode_line(&fields, &host()?, INSTANCE, 7).map_err(ctx("encode"))
    }

    #[test]
    fn test_encoded_line_parses_as_the_hub_record() -> TestResult {
        let observed_at = at("2026-09-27T10:00:00Z")?;
        let id = finding_id()?;
        let line = encode(&fields(
            &id,
            "structure-drift",
            Severity::High,
            "new workspace member",
            observed_at,
            None,
        ))?;

        assert!(line.ends_with('\n'), "one JSON object per line");
        assert_eq!(line.matches('\n').count(), 1, "exactly one newline");

        let parsed: HubDodFinding =
            serde_json::from_str(line.trim_end()).map_err(ctx("hub-shaped parse"))?;
        assert_eq!(parsed.finding_id, id);
        assert_eq!(parsed.host.as_str(), "host-a");
        assert_eq!(parsed.rule_id, "structure-drift");
        assert_eq!(parsed.severity, ImpactSeverity::High);
        assert_eq!(parsed.summary, "new workspace member");
        assert_eq!(parsed.observed_at, observed_at);
        assert_eq!(parsed.schema_version, Some(EXPORT_SCHEMA_VERSION));
        assert_eq!(parsed.occurrences, Some(1));

        let value: serde_json::Value =
            serde_json::from_str(line.trim_end()).map_err(ctx("raw parse"))?;
        assert_eq!(value["severity"], "high", "snake_case severity names");
        assert_eq!(value["observed_at"], "2026-09-27T10:00:00Z");
        Ok(())
    }

    #[test]
    fn test_encoded_line_is_schema_version_3_without_optional_fields_by_default() -> TestResult {
        let id = finding_id()?;
        let line = encode(&fields(
            &id,
            "structure-drift",
            Severity::High,
            "s",
            Timestamp::UNIX_EPOCH,
            None,
        ))?;
        let strict: ExportLineV3 =
            serde_json::from_str(line.trim_end()).map_err(ctx("strict v3 parse"))?;
        assert_eq!(strict.schema_version, EXPORT_SCHEMA_VERSION);
        assert_eq!(strict.schema_version, 3);
        assert_eq!(strict.scope, None);
        assert!(!strict.scope_obfuscated);
        assert_eq!(strict.instance.as_deref(), Some(INSTANCE));
        assert_eq!(strict.seq, Some(1));
        assert_eq!(strict.kind.as_deref(), Some("rule_triggered"));
        assert_eq!(strict.hardness.as_deref(), Some("observed"));
        assert_eq!(strict.occurrences, Some(1));
        assert_eq!(strict.first_seen, Some(Timestamp::UNIX_EPOCH));
        let value: serde_json::Value =
            serde_json::from_str(line.trim_end()).map_err(ctx("raw parse"))?;
        for absent in ["scope", "record_digest", "cgroup_kid", "evidence_card"] {
            assert!(
                value.get(absent).is_none(),
                "{absent} is omitted when unset"
            );
        }
        assert_eq!(value["scope_obfuscated"], false, "always written");
        Ok(())
    }

    #[test]
    fn test_encoded_line_carries_an_acceptable_scope() -> TestResult {
        let id = finding_id()?;
        let line = encode(&fields(
            &id,
            "egress-flow",
            Severity::Critical,
            "s",
            Timestamp::UNIX_EPOCH,
            Some("/sys/fs/cgroup/harw.slice/harw-job-1"),
        ))?;
        let strict: ExportLineV3 =
            serde_json::from_str(line.trim_end()).map_err(ctx("strict v3 parse"))?;
        assert_eq!(
            strict.scope.as_deref(),
            Some("/sys/fs/cgroup/harw.slice/harw-job-1")
        );
        assert!(!strict.scope_obfuscated);
        Ok(())
    }

    /// PL-100 WP0.9: ein nicht darstellbarer Scope wird zurückgehalten und
    /// als `scope_obfuscated` gemeldet, statt still zu verschwinden.
    #[test]
    fn test_unrenderable_scope_is_withheld_and_marked_obfuscated() -> TestResult {
        let id = finding_id()?;
        for raw in [
            "/sys/fs/cgroup/harw\u{200B}.slice",
            "/sys/fs/cgroup/a\nb",
            "/sys/fs/cgroup/\u{E0041}x",
            "relative/cgroup",
        ] {
            let line = encode(&fields(
                &id,
                "egress-flow",
                Severity::High,
                "s",
                Timestamp::UNIX_EPOCH,
                Some(raw),
            ))?;
            let strict: ExportLineV3 =
                serde_json::from_str(line.trim_end()).map_err(ctx("strict v3 parse"))?;
            assert_eq!(strict.scope, None, "{raw:?} is not exported");
            assert!(strict.scope_obfuscated, "{raw:?} is marked obfuscated");
            assert!(!line.contains('\u{200B}') && !line.contains('\u{E0041}'));
        }
        let empty = encode(&fields(
            &id,
            "egress-flow",
            Severity::High,
            "s",
            Timestamp::UNIX_EPOCH,
            Some(""),
        ))?;
        let strict: ExportLineV3 =
            serde_json::from_str(empty.trim_end()).map_err(ctx("strict v3 parse"))?;
        assert!(!strict.scope_obfuscated, "an empty scope is no scope");
        Ok(())
    }

    #[test]
    fn test_sanitize_scope_omits_instead_of_rewriting() {
        assert_eq!(sanitize_scope(None), None);
        assert_eq!(sanitize_scope(Some("")), None);
        assert_eq!(sanitize_scope(Some("relative/cgroup")), None);
        assert_eq!(sanitize_scope(Some("/a\nb")), None);
        assert_eq!(sanitize_scope(Some("/a\u{202E}b")), None);
        assert_eq!(sanitize_scope(Some("/a\u{E0041}b")), None, "tag block");
        assert_eq!(
            sanitize_scope(Some("/a\u{FE0F}b")),
            None,
            "variation selector"
        );
        let long = format!("/{}", "x".repeat(MAX_SCOPE_BYTES));
        assert_eq!(sanitize_scope(Some(&long)), None);
        assert_eq!(sanitize_scope(Some("/harw.slice")), Some("/harw.slice"));
    }

    #[test]
    fn test_v1_v2_and_v3_lines_parse_for_every_known_reader() -> TestResult {
        let id = finding_id()?;
        let v3 = encode(&fields(
            &id,
            "structure-drift",
            Severity::High,
            "s",
            at("2026-09-27T10:00:00Z")?,
            Some("/sys/fs/cgroup/harw.slice"),
        ))?;
        for raw in [V1_LINE, V2_FIXTURE_LINE, v3.trim_end(), V3_FIXTURE_LINE] {
            let hub: HubDodFinding = serde_json::from_str(raw).map_err(ctx("hub reader"))?;
            assert!(!hub.rule_id.is_empty());
            assert!(!hub.summary.is_empty());
            let signals: SecuritySignalsLine =
                serde_json::from_str(raw).map_err(ctx("security_signals reader"))?;
            assert_client_contract(&signals);
            let strict: ExportLineV3 = serde_json::from_str(raw).map_err(ctx("strict v3 shape"))?;
            assert!((1..=EXPORT_SCHEMA_VERSION).contains(&strict.schema_version));
        }
        let v1: ExportLineV3 = serde_json::from_str(V1_LINE).map_err(ctx("v1 via v3 shape"))?;
        assert_eq!(v1.schema_version, 1, "missing schema_version means v1");
        assert_eq!(v1.scope, None);
        assert_eq!(v1.instance, None);
        assert_eq!(v1.finding_id.as_str(), "f-1");
        assert_eq!(v1.host.as_str(), "host-a");
        assert_eq!(v1.rule_id, "structure-drift");
        assert_eq!(v1.severity, ImpactSeverity::High);
        assert_eq!(v1.summary, "s");
        assert_eq!(v1.observed_at, at("2026-09-27T10:00:00Z")?);
        let v2: ExportLineV3 =
            serde_json::from_str(V2_FIXTURE_LINE).map_err(ctx("v2 via v3 shape"))?;
        assert_eq!(v2.schema_version, 2);
        assert_eq!(v2.scope.as_deref(), Some("/sys/fs/cgroup/harw.slice"));
        assert_eq!((v2.seq, v2.dedup_key), (None, None));
        Ok(())
    }

    #[test]
    fn test_v3_fixture_line_is_byte_identical_to_encode_line() -> TestResult {
        let id = finding_id()?;
        let card = fixture_card();
        let encoded = encode_v3_fixture(&id, &card)?;
        assert_eq!(encoded, format!("{V3_FIXTURE_LINE}\n"));
        Ok(())
    }

    /// Rundreise v3: jedes geschriebene Feld kommt bei jedem Leser-Spiegel
    /// unverändert an.
    #[test]
    fn test_v3_line_round_trips_through_every_reader_mirror() -> TestResult {
        let id = finding_id()?;
        let card = fixture_card();
        let encoded = encode_v3_fixture(&id, &card)?;
        let raw = encoded.trim_end();

        let strict: ExportLineV3 = serde_json::from_str(raw).map_err(ctx("strict v3 parse"))?;
        assert_eq!(strict.schema_version, 3);
        assert_eq!(strict.finding_id, id);
        assert_eq!(strict.rule_id, "egress-flow");
        assert_eq!(strict.severity, ImpactSeverity::Critical);
        assert_eq!(strict.summary, "tcp connect");
        assert_eq!(strict.observed_at, at("2026-09-27T11:00:00Z")?);
        assert_eq!(strict.scope.as_deref(), Some("/sys/fs/cgroup/harw.slice"));
        assert_eq!(strict.instance.as_deref(), Some(INSTANCE));
        assert_eq!(strict.seq, Some(7));
        assert_eq!(
            strict.record_digest.as_deref(),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        assert_eq!(
            strict.dedup_key.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(strict.occurrences, Some(3));
        assert_eq!(strict.first_seen, Some(at("2026-09-27T10:00:00Z")?));
        assert_eq!(strict.kind.as_deref(), Some("rule_triggered"));
        assert_eq!(strict.hardness.as_deref(), Some("observed"));
        assert_eq!(strict.cgroup_kid, Some(4242));
        assert!(!strict.scope_obfuscated);
        assert_eq!(
            strict.evidence_card,
            Some(EvidenceCardMirror {
                event_counts: BTreeMap::from([("tcp_connect".to_owned(), 3)]),
                uid: Some(1000),
                destination: Some("203.0.113.9:443".to_owned()),
                path: Some("/usr/bin/curl".to_owned()),
                process: Some(ProcessMirror {
                    pid: 4711,
                    comm: "curl".to_owned(),
                }),
            })
        );

        let hub: HubDodFinding = serde_json::from_str(raw).map_err(ctx("hub reader"))?;
        assert_eq!(hub.schema_version, Some(3));
        assert_eq!(
            hub.dedup_key.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(hub.occurrences, Some(3));

        let signals: SecuritySignalsLine =
            serde_json::from_str(raw).map_err(ctx("security_signals reader"))?;
        assert_client_contract(&signals);
        assert_eq!(signals.rule_id, "egress-flow");
        assert_eq!(signals.severity, "critical");
        assert_eq!(signals.observed_at, "2026-09-27T11:00:00Z");
        assert_eq!(signals.cgroup_kid, Some(4242));
        assert!(!signals.scope_obfuscated);
        Ok(())
    }

    #[test]
    fn test_every_severity_maps_to_the_same_named_impact_severity() -> TestResult {
        for (severity, name) in [
            (Severity::Info, "info"),
            (Severity::Low, "low"),
            (Severity::Medium, "medium"),
            (Severity::High, "high"),
            (Severity::Critical, "critical"),
        ] {
            let encoded =
                serde_json::to_string(&impact_severity(severity)).map_err(ctx("encode"))?;
            assert_eq!(encoded, format!("\"{name}\""));
        }
        Ok(())
    }

    #[test]
    fn test_kind_and_hardness_labels_are_the_reader_spellings() {
        assert_eq!(kind_label(FindingKind::RuleTriggered), "rule_triggered");
        assert_eq!(kind_label(FindingKind::Anomaly), "anomaly");
        assert_eq!(hardness_label(Hardness::Observed), "observed");
        assert_eq!(hardness_label(Hardness::Correlated), "correlated");
        assert_eq!(hardness_label(Hardness::Inferred), "inferred");
    }

    #[test]
    fn test_sanitize_summary_escapes_control_and_bidi_characters() {
        let cleaned = sanitize_summary("a\nb\r\tc\u{1b}[31md\u{202E}e\u{0}f\u{E0041}");
        assert_eq!(
            cleaned,
            "a\\u{000a}b\\u{000d}\\u{0009}c\\u{001b}[31md\\u{202e}e\\u{0000}f\\u{e0041}"
        );
        assert!(is_clean(&cleaned));
    }

    #[test]
    fn test_sanitize_summary_caps_at_512_bytes_on_a_char_boundary() {
        let ascii = sanitize_summary(&"x".repeat(2 * MAX_SUMMARY_BYTES));
        assert!(ascii.len() <= MAX_SUMMARY_BYTES);
        assert!(ascii.ends_with('…'), "a cut summary is marked");
        assert!(ascii.len() >= MAX_SUMMARY_BYTES - 4, "the budget is used");

        // 3-Byte-Zeichen: die Grenze gilt in Bytes, nicht in Zeichen.
        let wide = sanitize_summary(&"€".repeat(MAX_SUMMARY_BYTES));
        assert!(wide.len() <= MAX_SUMMARY_BYTES);
        assert!(wide.ends_with('…'));
        assert!(wide.trim_end_matches('…').chars().all(|ch| ch == '€'));

        let short = sanitize_summary("unchanged");
        assert_eq!(short, "unchanged");
    }

    #[test]
    fn test_sanitize_summary_never_cuts_an_escape_sequence() {
        let escapes = sanitize_summary(&"\u{202E}".repeat(MAX_SUMMARY_BYTES));
        assert!(escapes.len() <= MAX_SUMMARY_BYTES);
        let body = escapes.trim_end_matches('…');
        assert_eq!(body.len() % "\\u{202e}".len(), 0, "{escapes}");
        assert!(body.split("\\u{202e}").all(str::is_empty));
    }

    #[test]
    fn test_encoded_line_carries_the_sanitized_summary() -> TestResult {
        let id = finding_id()?;
        let summary = format!("evil\n{{\"injected\":1}}{}", "y".repeat(4096));
        let line = encode(&fields(
            &id,
            "structure-drift",
            Severity::Low,
            &summary,
            Timestamp::UNIX_EPOCH,
            None,
        ))?;
        assert_eq!(line.matches('\n').count(), 1, "no line injection");
        let parsed: HubDodFinding =
            serde_json::from_str(line.trim_end()).map_err(ctx("hub-shaped parse"))?;
        assert!(parsed.summary.len() <= MAX_SUMMARY_BYTES);
        assert!(parsed.summary.starts_with("evil\\u{000a}{"));
        Ok(())
    }

    fn egress_event(uid: Option<u32>, destination: &str, port: u16) -> SecurityEvent {
        SecurityEvent {
            sensor: harw_types::SensorId::from_str("probe-bpf-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: uid.map(|uid| harw_dod_signals::Actor {
                uid,
                auid: None,
                cgroup: None,
                process: Some(harw_dod_signals::ProcessContext {
                    pid: 4711,
                    ppid: None,
                    comm: Some("curl\u{202E}".to_owned()),
                    euid: None,
                }),
            }),
            kind: EventKind::EgressFlow {
                destination: destination.to_owned(),
                port,
            },
        }
    }

    #[test]
    fn test_evidence_card_summarizes_agreeing_events_only() -> TestResult {
        let first = egress_event(Some(1000), "203.0.113.9", 443);
        let second = egress_event(Some(1000), "203.0.113.9", 443);
        let card = EvidenceCard::from_events(&[&first, &second])
            .ok_or(TestError::Missing("card of two events"))?;
        assert_eq!(card.event_counts.get("egress-flow"), Some(&2));
        assert_eq!(card.uid, Some(1000));
        assert_eq!(card.destination.as_deref(), Some("203.0.113.9:443"));
        assert_eq!(card.path, None);
        let process = card.process.as_ref().ok_or(TestError::Missing("process"))?;
        assert_eq!(process.pid, 4711);
        assert_eq!(process.comm, "curl\\u{202e}", "comm is escaped");

        let other_uid = egress_event(Some(1001), "203.0.113.10", 443);
        let mixed = EvidenceCard::from_events(&[&first, &other_uid])
            .ok_or(TestError::Missing("card of mixed events"))?;
        assert_eq!(mixed.uid, None, "disagreeing uids are not guessed");
        assert_eq!(mixed.destination, None, "disagreeing destinations neither");

        let v6 = egress_event(None, "2001:db8::1", 8443);
        let v6_card =
            EvidenceCard::from_events(&[&v6]).ok_or(TestError::Missing("card of v6 flow"))?;
        assert_eq!(v6_card.destination.as_deref(), Some("[2001:db8::1]:8443"));
        assert_eq!(v6_card.uid, None);

        let hostname = egress_event(None, "example.invalid", 443);
        let named = EvidenceCard::from_events(&[&hostname])
            .ok_or(TestError::Missing("card of named flow"))?;
        assert_eq!(named.destination, None, "only IP literals are ip:port");

        assert_eq!(EvidenceCard::from_events(&[]), None);
        Ok(())
    }

    /// Ein Egress-Ereignis von uid 1000 mit dem Prozesskontext `process`.
    fn egress_with_process(
        process: Option<harw_dod_signals::ProcessContext>,
    ) -> SecurityEvent {
        SecurityEvent {
            actor: Some(harw_dod_signals::Actor {
                uid: 1000,
                auid: None,
                cgroup: None,
                process,
            }),
            ..egress_event(None, "203.0.113.9", 443)
        }
    }

    /// Ein Prozesskontext mit Elternprozess und effektiver uid.
    fn process_context(pid: u32, comm: Option<&str>) -> harw_dod_signals::ProcessContext {
        harw_dod_signals::ProcessContext {
            pid,
            ppid: Some(1),
            comm: comm.map(str::to_owned),
            euid: Some(0),
        }
    }

    /// PL-100 WP0.15: `evidence_card.process` kommt aus `Actor::process` —
    /// nur `pid` und `comm`, ein fehlender `comm` wird leer, Ereignisse ohne
    /// Prozesskontext zählen nicht mit, widersprüchliche Prozesse ergeben
    /// keinen.
    #[test]
    fn test_evidence_card_process_comes_from_the_actor_process_context() -> TestResult {
        let unnamed = egress_with_process(Some(process_context(77, None)));
        let card = EvidenceCard::from_events(&[&unnamed])
            .ok_or(TestError::Missing("card of an unnamed process"))?;
        assert_eq!(
            card.process,
            Some(EvidenceProcess {
                pid: 77,
                comm: String::new(),
            }),
            "a missing comm is exported empty"
        );

        let named = egress_with_process(Some(process_context(77, Some("sshd"))));
        let bare = egress_with_process(None);
        let card = EvidenceCard::from_events(&[&named, &bare])
            .ok_or(TestError::Missing("card of named and bare events"))?;
        assert_eq!(
            card.process,
            Some(EvidenceProcess {
                pid: 77,
                comm: "sshd".to_owned(),
            }),
            "an event without process context does not veto"
        );
        assert_eq!(card.uid, Some(1000));

        let only_bare = EvidenceCard::from_events(&[&bare])
            .ok_or(TestError::Missing("card of a bare event"))?;
        assert_eq!(only_bare.process, None, "no process context, no process");

        let other_pid = egress_with_process(Some(process_context(78, Some("sshd"))));
        let card = EvidenceCard::from_events(&[&named, &other_pid])
            .ok_or(TestError::Missing("card of two pids"))?;
        assert_eq!(card.process, None, "disagreeing pids are not guessed");

        let other_comm = egress_with_process(Some(process_context(77, Some("bash"))));
        let card = EvidenceCard::from_events(&[&named, &other_comm])
            .ok_or(TestError::Missing("card of two names"))?;
        assert_eq!(card.process, None, "disagreeing names are not guessed");

        let encoded = serde_json::to_value(
            EvidenceCard::from_events(&[&named])
                .ok_or(TestError::Missing("card of one process"))?,
        )
        .map_err(ctx("card encodes"))?;
        assert_eq!(
            encoded["process"],
            serde_json::json!({ "pid": 77, "comm": "sshd" }),
            "ppid and euid stay out of the export line"
        );
        Ok(())
    }

    /// PL-100 WP0.8/WP0.15: ein feindlicher `comm`, den die Sonde nicht
    /// escapt hat, wird hier escapt und begrenzt.
    #[test]
    fn test_evidence_card_escapes_and_bounds_a_hostile_comm() -> TestResult {
        let hostile = format!("{}\u{202E}gpj", "\u{1}".repeat(40));
        let event = egress_with_process(Some(process_context(9, Some(hostile.as_str()))));
        let card =
            EvidenceCard::from_events(&[&event]).ok_or(TestError::Missing("card of a process"))?;
        let process = card.process.ok_or(TestError::Missing("process"))?;
        assert_eq!(process.pid, 9);
        assert!(process.comm.chars().count() <= MAX_EVIDENCE_COMM_CHARS);
        assert!(process.comm.starts_with("\\u{0001}"), "{}", process.comm);
        assert!(is_clean(&process.comm), "{}", process.comm);

        let escaped = egress_with_process(Some(process_context(9, Some("curl\\u{202e}"))));
        let card =
            EvidenceCard::from_events(&[&escaped]).ok_or(TestError::Missing("card of a process"))?;
        assert_eq!(
            card.process.map(|process| process.comm).as_deref(),
            Some("curl\\u{202e}"),
            "an already escaped comm passes unchanged"
        );
        Ok(())
    }

    #[test]
    fn test_evidence_card_path_is_escaped_bounded_and_the_card_fits() -> TestResult {
        let event = SecurityEvent {
            sensor: harw_types::SensorId::from_str("probe-fs-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::FileWrite {
                path: format!("/tmp/{}\u{202E}gpj.exe", "\u{200B}".repeat(4096)),
            },
        };
        let card =
            EvidenceCard::from_events(&[&event]).ok_or(TestError::Missing("card of a write"))?;
        let path = card
            .path
            .as_deref()
            .ok_or(TestError::Missing("path kept"))?;
        assert!(path.chars().count() <= MAX_EVIDENCE_PATH_CHARS);
        assert!(path.ends_with('…'));
        assert!(is_clean(path));
        let encoded = serde_json::to_vec(&card).map_err(ctx("card encodes"))?;
        assert!(encoded.len() <= MAX_EVIDENCE_CARD_BYTES);
        Ok(())
    }

    #[test]
    fn test_exporter_appends_one_line_per_finding_with_increasing_seq() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        let mut exporter = FindingsExporter::new(path.clone(), host()?);
        assert!(is_uuid_v4(exporter.instance()), "instance is a UUIDv4");

        let id = finding_id()?;
        for rule in ["structure-drift", "egress-flow"] {
            let ok = exporter.export(
                &NullSink,
                &fields(
                    &id,
                    rule,
                    Severity::Medium,
                    "summary",
                    Timestamp::UNIX_EPOCH,
                    None,
                ),
            );
            assert!(ok);
        }
        assert_eq!(exporter.written(), 2);
        assert_eq!(exporter.failures(), 0);

        let lines = read_lines(&path)?;
        assert_eq!(lines.len(), 2);
        let mut seqs = Vec::new();
        for line in &lines {
            let strict: ExportLineV3 = serde_json::from_str(line).map_err(ctx("line"))?;
            assert_eq!(strict.instance.as_deref(), Some(exporter.instance()));
            seqs.push(strict.seq);
        }
        assert_eq!(seqs, vec![Some(1), Some(2)]);
        let second = lines.get(1).ok_or(TestError::Missing("second line"))?;
        let second: HubDodFinding = serde_json::from_str(second).map_err(ctx("second line"))?;
        assert_eq!(second.rule_id, "egress-flow");

        let other = FindingsExporter::new(path, host()?);
        assert_ne!(other.instance(), exporter.instance(), "new per start");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_exporter_creates_the_file_with_mode_0640() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        let mut exporter = FindingsExporter::new(path.clone(), host()?);
        exporter.prepare(&NullSink);
        assert_eq!(exporter.failures(), 0);
        let mode = fs::metadata(&path)
            .map_err(ctx("metadata"))?
            .permissions()
            .mode()
            & 0o777;
        // Die Prozess-umask kann höchstens Bits entfernen, nie hinzufügen.
        assert_eq!(mode & !EXPORT_FILE_MODE, 0, "mode {mode:o} exceeds 0640");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_exporter_narrows_an_existing_world_readable_file() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        fs::write(&path, b"").map_err(ctx("seed"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).map_err(ctx("chmod"))?;
        let mut exporter = FindingsExporter::new(path.clone(), host()?);
        exporter.prepare(&NullSink);
        let mode = fs::metadata(&path)
            .map_err(ctx("metadata"))?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o640);
        Ok(())
    }

    /// Länge einer Zeile mit `seq` aus genau einer Ziffer.
    fn one_digit_line_len() -> TestResult<u64> {
        let id = finding_id()?;
        let line = encode(&fields(
            &id,
            "structure-drift",
            Severity::Info,
            "s",
            Timestamp::UNIX_EPOCH,
            None,
        ))?;
        u64::try_from(line.len()).map_err(ctx("line length"))
    }

    #[test]
    fn test_exporter_rotates_to_dot_one_when_the_cap_would_be_exceeded() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        // Platz für genau zwei Zeilen (`seq` 1 bis 3 haben je eine Ziffer).
        let cap = one_digit_line_len()?
            .checked_mul(2)
            .ok_or(TestError::Missing("cap"))?;
        let mut exporter = FindingsExporter::with_max_bytes(path.clone(), host()?, cap);

        let id = finding_id()?;
        for _ in 0..3 {
            let ok = exporter.export(
                &NullSink,
                &fields(
                    &id,
                    "structure-drift",
                    Severity::Info,
                    "s",
                    Timestamp::UNIX_EPOCH,
                    None,
                ),
            );
            assert!(ok);
        }

        let rotated = dir.path().join("findings.jsonl.1");
        assert_eq!(read_lines(&rotated)?.len(), 2, "first two lines rotated");
        assert_eq!(read_lines(&path)?.len(), 1, "third line in a fresh file");
        let size = fs::metadata(&path).map_err(ctx("metadata"))?.len();
        assert!(size <= cap);
        assert_eq!(exporter.failures(), 0);
        Ok(())
    }

    #[test]
    fn test_keep_three_generations_shifts_older_files_and_drops_the_oldest() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        // Genau eine Zeile je Datei: jede weitere Zeile rotiert (`seq` 1
        // bis 6 haben je eine Ziffer).
        let cap = one_digit_line_len()?;
        let mut exporter =
            FindingsExporter::with_max_bytes(path.clone(), host()?, cap).with_keep(3);
        let id = finding_id()?;
        for _ in 0..6 {
            assert!(exporter.export(
                &NullSink,
                &fields(
                    &id,
                    "structure-drift",
                    Severity::Info,
                    "s",
                    Timestamp::UNIX_EPOCH,
                    None,
                ),
            ));
        }
        for generation in 1..=3 {
            let name = format!("findings.jsonl.{generation}");
            assert_eq!(read_lines(&dir.path().join(&name))?.len(), 1, "{name}");
        }
        assert!(
            !dir.path().join("findings.jsonl.4").exists(),
            "older generations fall out"
        );
        assert_eq!(read_lines(&path)?.len(), 1);
        assert_eq!(exporter.failures(), 0);
        Ok(())
    }

    #[test]
    fn test_keep_is_at_least_one() -> TestResult {
        let exporter = FindingsExporter::new(PathBuf::from("/tmp/x.jsonl"), host()?).with_keep(0);
        assert_eq!(exporter.keep, 1);
        Ok(())
    }

    #[test]
    fn test_default_cap_is_16_mib() {
        assert_eq!(MAX_EXPORT_BYTES, 16 * 1024 * 1024);
    }

    #[test]
    fn test_write_failure_is_counted_not_fatal_and_still_takes_a_seq() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        // Ein Verzeichnis am Zielpfad: jeder Versuch scheitert.
        let path = dir.path().join("is-a-dir");
        fs::create_dir(&path).map_err(ctx("mkdir"))?;
        let mut exporter = FindingsExporter::new(path, host()?);
        exporter.prepare(&NullSink);
        let id = finding_id()?;
        let ok = exporter.export(
            &NullSink,
            &fields(
                &id,
                "structure-drift",
                Severity::High,
                "s",
                Timestamp::UNIX_EPOCH,
                None,
            ),
        );
        assert!(!ok);
        assert_eq!(exporter.failures(), 2, "prepare + export both counted");
        assert_eq!(exporter.written(), 0);
        assert_eq!(exporter.next_seq, 2, "the failed line used seq 1");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_symlink_at_export_path_is_refused() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let target = dir.path().join("target");
        fs::write(&target, b"").map_err(ctx("seed target"))?;
        let path = dir.path().join("findings.jsonl");
        std::os::unix::fs::symlink(&target, &path).map_err(ctx("symlink"))?;
        let mut exporter = FindingsExporter::new(path, host()?);
        exporter.prepare(&NullSink);
        assert_eq!(exporter.failures(), 1);
        let untouched = fs::read(&target).map_err(ctx("read target"))?;
        assert!(untouched.is_empty());
        Ok(())
    }

    #[test]
    fn test_resolve_host_prefers_explicit_then_hostname_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let hostname = dir.path().join("hostname");
        fs::write(&hostname, "node-7\n").map_err(ctx("hostname"))?;

        let explicit = host()?;
        let chosen =
            resolve_host(Some(&explicit), &hostname).ok_or(TestError::Missing("explicit host"))?;
        assert_eq!(chosen.as_str(), "host-a");

        let fallback = resolve_host(None, &hostname).ok_or(TestError::Missing("hostname file"))?;
        assert_eq!(fallback.as_str(), "node-7");

        assert!(resolve_host(None, &dir.path().join("missing")).is_none());
        fs::write(&hostname, "  \n").map_err(ctx("blank hostname"))?;
        assert!(resolve_host(None, &hostname).is_none());
        fs::write(&hostname, "node\u{200B}-7\n").map_err(ctx("zero-width hostname"))?;
        assert!(resolve_host(None, &hostname).is_none());
        Ok(())
    }

    #[test]
    fn test_rotated_path_appends_dot_one() {
        assert_eq!(
            rotated_path(Path::new("/var/lib/harw-sentinel/findings.jsonl")),
            PathBuf::from("/var/lib/harw-sentinel/findings.jsonl.1")
        );
    }

    #[test]
    fn test_validate_export_path_rejects_non_normal_components() {
        for raw in [
            "relative/findings.jsonl",
            "/var/lib/./export/findings.jsonl",
            "/var/lib/../lib/export/findings.jsonl",
            "/var/lib//export/findings.jsonl",
            "/var/lib/export/findings.jsonl/",
            "/var/lib/export/.",
            "/",
        ] {
            assert!(
                matches!(
                    validate_export_path("findings", Path::new(raw)),
                    Err(ExportPathError::NonNormal { .. })
                ),
                "{raw} must be refused"
            );
        }
        assert!(
            validate_export_path("findings", Path::new("/var/lib/export/findings.jsonl")).is_ok()
        );
    }

    #[test]
    fn test_distinct_destinations_in_one_directory_pass() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        check_distinct_destinations(
            &dir.path().join("findings.jsonl"),
            &dir.path().join("health.json"),
        )
        .map_err(ctx("distinct names"))?;
        Ok(())
    }

    #[test]
    fn test_identical_destinations_are_a_collision() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        assert!(matches!(
            check_distinct_destinations(&path, &path),
            Err(ExportPathError::Collision { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_dot_component_alias_of_the_findings_path_is_refused() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let findings = dir.path().join("findings.jsonl");
        let mut alias = dir.path().as_os_str().to_owned();
        alias.push("/./findings.jsonl");
        let error = check_distinct_destinations(&findings, Path::new(&alias))
            .err()
            .ok_or(TestError::Missing("refusal of the dot alias"))?;
        assert!(matches!(
            error,
            ExportPathError::NonNormal { role: "health", .. }
        ));
        let mut parent_alias = dir.path().as_os_str().to_owned();
        parent_alias.push("/sub/../findings.jsonl");
        assert!(check_distinct_destinations(&findings, Path::new(&parent_alias)).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_symlinked_parent_alias_of_the_findings_path_is_refused() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let real = dir.path().join("real");
        fs::create_dir(&real).map_err(ctx("real dir"))?;
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).map_err(ctx("symlink"))?;
        let error =
            check_distinct_destinations(&real.join("findings.jsonl"), &link.join("findings.jsonl"))
                .err()
                .ok_or(TestError::Missing("refusal of the symlinked parent"))?;
        assert!(matches!(error, ExportPathError::Collision { .. }));
        assert!(error.to_string().contains("resolve to the same file"));
        Ok(())
    }

    #[test]
    fn test_findings_on_the_health_temporary_sibling_is_refused() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let error = check_distinct_destinations(
            &dir.path().join(".health.json.tmp"),
            &dir.path().join("health.json"),
        )
        .err()
        .ok_or(TestError::Missing("refusal of the temporary sibling"))?;
        assert!(matches!(error, ExportPathError::SiblingOverlap { .. }));
        assert!(error.to_string().contains(".health.json.tmp"));
        Ok(())
    }

    #[test]
    fn test_health_on_a_rotated_findings_generation_is_refused() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let findings = dir.path().join("findings.jsonl");
        for name in ["findings.jsonl.1", "findings.jsonl.12"] {
            assert!(
                matches!(
                    check_distinct_destinations(&findings, &dir.path().join(name)),
                    Err(ExportPathError::SiblingOverlap { .. })
                ),
                "{name} must be refused"
            );
        }
        // Kein Generationsname: Ziffern fehlen oder sind keine Ziffern.
        for name in ["findings.jsonl.json", "findings.jsonl.", "findings.jsonl1"] {
            check_distinct_destinations(&findings, &dir.path().join(name))
                .map_err(ctx("not a rotated generation"))?;
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_symlinked_parent_alias_of_a_rotated_generation_is_refused() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let real = dir.path().join("real");
        fs::create_dir(&real).map_err(ctx("real dir"))?;
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).map_err(ctx("symlink"))?;
        assert!(matches!(
            check_distinct_destinations(
                &real.join("findings.jsonl"),
                &link.join("findings.jsonl.2")
            ),
            Err(ExportPathError::SiblingOverlap { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_unresolvable_distinct_parent_fails_closed() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let error = check_distinct_destinations(
            &dir.path().join("findings.jsonl"),
            &dir.path().join("missing").join("health.json"),
        )
        .err()
        .ok_or(TestError::Missing("fail-closed refusal"))?;
        assert!(matches!(error, ExportPathError::Unresolved { .. }));
        Ok(())
    }
}
