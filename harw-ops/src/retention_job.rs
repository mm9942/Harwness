//! `retention_sweep` — Aufbewahrungs-Sweep als Job der Job-Infrastruktur.
//!
//! # Verantwortungsbereich
//! Aufräumen ephemerer Daten (Logs, Caches, Spools) läuft nie im
//! Aufrufer-Pfad, sondern als dauerhafter Job der Art
//! [`RETENTION_SWEEP_JOB_KIND`] (`JobKind::Custom`) im [`JobStore`]:
//!
//! - [`RetentionSweepSpec`] — die Job-Payload (aufgelöste absolute Wurzeln, die
//!   wirksame `[retention]`-Konfiguration, Klassenauswahl, Modus, Frist);
//! - [`enqueue_retention_sweep`] / [`admit_retention_sweep`] — legen den
//!   `Ready`-Job an und geben die Id sofort zurück (nicht blockierend);
//! - [`execute_retention_sweep_with_cancel`] — der Handler-Kern, den der
//!   Job-Worker (`harw-cli`) aufruft; [`run_retention_sweep_job`] ist der
//!   gemeinsame asynchrone Treiber (Abbruch per Signal und Store-Poll).
//!
//! # Sicherheit
//! Sicherheitsrelevante Klassen sind Opt-in: ohne `enabled = true` in der
//! Konfiguration wird bei `apply` nichts gelöscht und die Klasse als
//! übersprungen gemeldet; der Probelauf meldet immer nur. Session-Transkripte
//! brauchen zusätzlich eine autoritative Anhänge-Quelle (siehe unten); ohne
//! sie überspringt der Job die Klasse in beiden Modi.
//!
//! # Store-eigene Klassen (PL-100 WP 2.3)
//! Klassen, die der Session-Store besitzt
//! ([`StoreClass::from_id`]: `session_transcripts`,
//! `session_corrupt_backups`, `freeze_resolved`, `dod_verdicts`), laufen nie
//! über den generischen Sweeper (`ResolvedClass::sweep`), sondern über
//! [`retention_sweep_at`] des Stores, mit dessen Sicherheitsprüfungen:
//!
//! - `dod_verdicts`: [`VerdictStore::apply_retention`] mustert nur ganze,
//!   abgelaufene Segmente des geprüften Präfixes aus und verankert die Kette
//!   vor dem ersten Löschen neu. Der generische Sweeper bekommt für diese
//!   Klasse kein Verzeichnis; ohne diese Weiche liefe die 90-Tage-Aufbewahrung
//!   nie. Ein Override wird abgelehnt (Fehlereintrag, nichts gelöscht). Ein
//!   Probelauf prüft nur Kette und Grenzwert-Overrides: der Store hat keine
//!   Plan-API und kann nicht vorhersagen, welche Segmente `apply` ausmustern
//!   würde. Das Ergebnis sagt dann „unbekannt“ statt 0: im Verzeichnis sind
//!   `removed_segments` und `removed_records` `null` mit `planned: false`,
//!   oben sind die beiden Summen `null`.
//! - `session_transcripts` (Review 4505340178): nur mit einer autoritativen,
//!   erst zur Ausführungszeit befragten Anhänge-Quelle. Die Sitzungssperre
//!   `<id>.jsonl.lock` hält `TranscriptStore::append` nur für einen
//!   einzelnen Schreibvorgang; eine angehängte, aber ruhende Sitzung hat also
//!   weder Sperre noch offene Freigabe und sähe inaktiv aus. Eine solche
//!   Quelle gibt es heute nicht: welche Sitzungen ein Session-Host angehängt
//!   hat, weiß nur sein Prozess im Speicher, TUI- und lokale Läufe schreiben
//!   dasselbe Sessions-Verzeichnis aus eigenen Prozessen, und eine
//!   Momentaufnahme beim Einreihen wäre bei der Ausführung veraltet. Der Job
//!   meldet die Klasse deshalb nach der Opt-in-Prüfung als übersprungen
//!   (`skipped`: [`NO_ATTACHMENT_SOURCE`], im Probelauf wie bei `apply`),
//!   ruft den Store dafür nicht auf und setzt die übrigen Klassen fort; auch
//!   der Store selbst löscht ohne `is_attached`-Haken kein Transkript. Mit
//!   einer Quelle (Naht: der private Kern `execute_until`) gelangt sie als
//!   [`SweepOptions::is_attached`] in den Store, der sie je Datei vor der
//!   Sperrprüfung und unter der Sperre unmittelbar vor dem Löschen erneut
//!   fragt; gelöscht wird nur, wenn beide Antworten „nicht angehängt“
//!   lauten. Gegen ein Anhängen nach der zweiten Frage schützt das nur eine
//!   Quelle, die die Registrierungsregel von [`SweepOptions::is_attached`]
//!   erfüllt (siehe „Freischalten“). Zusätzlich bleibt ein Transkript,
//!   solange die Sitzungssperre gehalten wird, eine Freigabe offen ist oder
//!   sich die Datei seit der Planung geändert hat. Wurzel ist je Profil das
//!   Sessions-Verzeichnis, das der Klassen-Resolver liefert.
//!
//!   Freischalten (Folgearbeit): eine Anhänge-Lease, die jeder Prozess, der
//!   eine Sitzung dieses Sessions-Verzeichnisses anhängt, für die ganze Dauer
//!   der Anhängung hält (etwa ein `flock` auf `<sessions>/<id>.attach`,
//!   gehalten von Session-Host und TUI), als Prädikat „Lease belegt =
//!   angehängt“ im Job-Worker bauen und an den Kern übergeben. Damit die
//!   zweite Frage unter der Transkript-Sperre verlässlich ist, gilt eine von
//!   zwei Regeln: (a) der anhängende Prozess nimmt die Lease, während er kurz
//!   `<id>.jsonl.lock` hält, und prüft dabei, ob das Transkript noch
//!   existiert (fehlt es, gilt die Sitzung als verschwunden; der nächste
//!   Schreibvorgang darf sie nicht leer neu anlegen), oder (b) der Sweep hält
//!   die Lease selbst exklusiv über das Löschen hinweg, was eine Store-API
//!   über das Ja/Nein-Prädikat hinaus braucht. Ein bloßes Abfragen der Lease
//!   ohne (a) ließe ein Anhängen zwischen Frage und Löschen zu. Ein nur
//!   prozesslokaler Zustand (etwa die angehängten Sitzungen des
//!   `SessionHost` von `harw serve`) genügt nicht: er kennt die Sitzungen
//!   anderer Prozesse nicht.
//! - `session_corrupt_backups`: nur reguläre, seit der Planung unveränderte
//!   Dateien.
//! - `freeze_resolved`: unter der Freeze-Sperre, nie ein `.active.json` und
//!   kein Record, dessen `.active.json` noch existiert.
//!
//! Vor PL-100 WP 2.3 liefen die drei älteren Klassen über den generischen
//! Sweeper ohne diese Prüfungen; ihre Auswahl (Alter, Anzahl, Bytes,
//! `keep_newest`) und das Opt-in sind unverändert. Je Verzeichnis ergänzt das
//! Ergebnis `protected`, `planned`, `removed_segments`, `removed_records` und
//! `error_messages` (inhaltsfreie Meldungen des Stores, escapt, ohne Pfade
//! einzelner Dateien, höchstens acht je Verzeichnis); oben stehen
//! zusätzlich die Summen `removed_segments` und `removed_records`.
//!
//! # Fehler im Ergebnis
//! Scheitert ein Verzeichnis bzw. eine Store-Wurzel als Ganzes (Sperre
//! belegt, Symlink, I/O, Ablehnung durch den `VerdictStore`), erscheint das
//! als `error` dieses Verzeichnisses, und der Job läuft weiter, wie im
//! generischen Pfad: die Löschungen der übrigen Klassen bleiben so im
//! Ergebnis sichtbar (ein `Failed`-Ausgang trüge nur einen Text). Damit ein
//! solcher Teilausfall nicht hinter einem erfolgreichen Job verschwindet,
//! stehen oben die Summen `failed_dirs` (als Ganzes gescheiterte
//! Verzeichnisse) und `errors` (Einzelfehler aller Verzeichnisse,
//! Kettenfehler und abgelehnte Overrides eingeschlossen); sind sie nicht 0,
//! meldet der Job das zusätzlich auf `warn` (nur Zählwerte). Die Summen
//! `removed*` zählen nur Verzeichnisse ohne `error`.
//!
//! # Frist und Abbruch
//! Die Frist stammt aus der Payload und beginnt mit der Ausführung; eine
//! Frist, die als [`Instant`] nicht darstellbar ist, wird als ungültige
//! Payload abgelehnt (`Failed`). Ablauf und Operator-Abbruch sind
//! unterscheidbar (`timed_out` bzw. `cancelled`); jeder einzelne
//! Löschvorgang ist unabhängig und idempotent, ein Abbruch lässt also keinen
//! halben Zustand zurück. Generische und store-eigene Klassen werden gleich
//! behandelt: der Abbruch wird zwischen Klassen geprüft, die Frist ebenso und
//! zusätzlich vor jedem Löschen (in `harw-retention` bzw. im Session-Store;
//! `dod_verdicts` einmal vor dem Store-Schritt). Meldet ein Verzeichnis
//! `timed_out`, endet der ganze Job als `timed_out`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use harw_job_runtime::{
    Budget, Job, JobKind, JobOutcome, JobScope, RetryPolicy, StoredJob, WorkId,
};
use harw_operations::{OpContext, OpError, OpOutput};
use harw_retention::{
    Report, ResolvedClass, RetentionConfig, RetentionError, Roots, SweepMode, resolve_all,
};
use harw_session_store::retention::{
    NO_ATTACHMENT_SOURCE, StoreClass, StoreSweepReport, SweepOptions, retention_sweep_at,
};
use harw_session_store::{FreezeStore, JobStore, VerdictStore};
use harw_types::{ApprovalActor, Principal};
use harw_untrusted_text::escape_line;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::memory_job::{BLOCKING_GRACE, CANCEL_POLL_INTERVAL, MaintenanceFailure};

/// [`JobKind::Custom`]-Name der Aufbewahrungs-Sweeps.
pub const RETENTION_SWEEP_JOB_KIND: &str = "retention_sweep";
/// Schema-Version der Payload [`RetentionSweepSpec`].
pub const RETENTION_SWEEP_SCHEMA_VERSION: u32 = 1;
/// Standardfrist eines Sweeps in Sekunden.
pub const DEFAULT_SWEEP_DEADLINE_SECS: u64 = 60;
/// Ein Sweep wird nie automatisch wiederholt.
const SWEEP_MAX_ATTEMPTS: u32 = 1;
/// Höchstzahl Fehlermeldungen (`error_messages`) je Verzeichnis im Ergebnis
/// einer store-eigenen Klasse; der Zähler `errors` bleibt vollständig.
const MAX_ERROR_MESSAGES_PER_DIR: usize = 8;
/// Obergrenze in Zeichen für jede Meldung und Verzeichnisangabe im Ergebnis
/// (nach dem Escapen, siehe [`harw_untrusted_text::escape_line`]).
const MAX_RESULT_TEXT_CHARS: usize = 1_024;

/// Payload eines `retention_sweep`-Jobs (alle Pfade absolut und beim
/// Einreihen aufgelöst; der Worker kennt weder Home-Auflösung noch Projekt).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionSweepSpec {
    /// Siehe [`RETENTION_SWEEP_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Harness-Home (`~/.harw` oder Override).
    pub home: PathBuf,
    /// Projektwurzel, falls es eine gibt.
    pub project: Option<PathBuf>,
    /// Klassen-Ids; leer = alle Klassen.
    pub classes: Vec<String>,
    /// `true` löscht (nur erlaubte Klassen), `false` ist der Probelauf.
    pub apply: bool,
    /// Wirksame `[retention]`-Konfiguration.
    pub config: RetentionConfig,
    /// Frist der Ausführung in Sekunden.
    pub deadline_secs: u64,
}

impl RetentionSweepSpec {
    /// Probelauf über alle Klassen mit der Standardfrist.
    #[must_use]
    pub fn new(home: PathBuf, project: Option<PathBuf>, config: RetentionConfig) -> Self {
        Self {
            schema_version: RETENTION_SWEEP_SCHEMA_VERSION,
            home,
            project,
            classes: Vec::new(),
            apply: false,
            config,
            deadline_secs: DEFAULT_SWEEP_DEADLINE_SECS,
        }
    }
}

/// Gemeinsames Abbruchsignal des Handler-Kerns.
#[derive(Clone, Debug, Default)]
pub struct SweepCancel(Arc<AtomicBool>);

impl SweepCancel {
    /// Neues, nicht gesetztes Signal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Setzt den Abbruch.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Ob abgebrochen wurde.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// `true` für `JobKind::Custom("retention_sweep")`.
#[must_use]
pub fn is_retention_sweep_kind(kind: &JobKind) -> bool {
    matches!(kind, JobKind::Custom(name) if name == RETENTION_SWEEP_JOB_KIND)
}

/// Legt einen `Ready`-Job für `spec` im `jobs`-Store an und gibt seine Id
/// sofort zurück.
///
/// # Errors
/// Text bei Frist `0` oder wenn der Store den Job ablehnt.
pub fn admit_retention_sweep(
    jobs: &JobStore,
    scope: JobScope,
    spec: &RetentionSweepSpec,
) -> Result<WorkId, String> {
    if spec.deadline_secs == 0 {
        return Err("deadline_secs muss größer als 0 sein".to_owned());
    }
    let now = Timestamp::now();
    let wall = SignedDuration::from_secs(i64::try_from(spec.deadline_secs).unwrap_or(i64::MAX));
    let mut job = Job::new(
        WorkId::new(),
        JobKind::Custom(RETENTION_SWEEP_JOB_KIND.to_owned()),
        Budget {
            max_tokens: None,
            max_wall: Some(wall),
            max_tool_calls: None,
        },
        RetryPolicy {
            max_attempts: SWEEP_MAX_ATTEMPTS,
            base_delay: SignedDuration::from_secs(30),
            factor: 2.0,
            max_delay: SignedDuration::from_secs(300),
            jitter: 0.0,
        },
        now,
    );
    job.mark_ready(now).map_err(|error| error.to_string())?;
    let input = serde_json::to_value(spec).map_err(|error| error.to_string())?;
    let work_id = job.id.clone();
    let record = StoredJob {
        job,
        scope,
        input,
        submitted_at: now,
        not_before: now,
        lease: None,
        lease_epoch: 0,
        completion: None,
        cancellation: None,
        revision: 0,
        trace: None,
    };
    jobs.admit(&record).map_err(|error| error.to_string())?;
    Ok(work_id)
}

/// Reiht den Sweep über den Job-Store des Kontexts ein (Standardweg von
/// `harw cleanup`).
///
/// # Errors
/// [`OpError::NotAvailable`] ohne Job-Store, [`OpError::Execution`] wenn der
/// Store den Job ablehnt.
pub fn enqueue_retention_sweep(
    ctx: &OpContext,
    spec: &RetentionSweepSpec,
) -> Result<OpOutput, OpError> {
    let jobs = ctx.service::<Arc<JobStore>>().ok_or_else(|| {
        OpError::NotAvailable("kein dauerhafter Job-Store konfiguriert".to_owned())
    })?;
    let submitter = ctx.service::<Principal>().map_or_else(
        || "cleanup".to_owned(),
        |principal| principal.id().to_owned(),
    );
    let binding = ctx.sandbox().workspace();
    let scope = JobScope::new(
        binding.tenant().clone(),
        binding.workspace().clone(),
        ApprovalActor::Operator { id: submitter },
    );
    let work_id = admit_retention_sweep(jobs, scope, spec).map_err(|error| {
        OpError::Execution(format!(
            "Aufbewahrungs-Job konnte nicht eingereiht werden: {error}"
        ))
    })?;
    let mode = if spec.apply { "apply" } else { "dry_run" };
    Ok(OpOutput {
        text: format!(
            "Aufbewahrungs-Job {work_id} ({mode}) eingereiht, Frist {} s. Status und Abbruch \
             über die Job-Werkzeuge (harw jobs show / cancel).",
            spec.deadline_secs
        ),
        data: Some(serde_json::json!({
            "job_id": work_id.as_str(),
            "kind": RETENTION_SWEEP_JOB_KIND,
            "mode": mode,
            "deadline_secs": spec.deadline_secs,
        })),
    })
}

/// Handler-Kern: führt den Sweep aus und liefert das Ergebnis als JSON.
///
/// # Description
/// Store-eigene Klassen ([`StoreClass::from_id`]) laufen über
/// [`retention_sweep_at`] des Session-Stores, alle anderen über den
/// generischen Sweeper von `harw-retention` (siehe Moduldoku). Ergebnis:
/// `{mode, removed, removed_bytes, removed_segments, removed_records,
/// errors, failed_dirs, classes: [{id, kind, dirs: [..]} | {id, kind,
/// skipped}]}`; `removed_segments`/`removed_records` sind `null`, wenn ein
/// `dod_verdicts`-Probelauf sie nicht vorhersagen kann.
///
/// Dieser Einstieg hat keine autoritative Anhänge-Quelle: `session_transcripts`
/// erscheint deshalb (nach der Opt-in-Prüfung) immer als `{id, kind,
/// skipped: "no authoritative attachment source"}` ([`NO_ATTACHMENT_SOURCE`]),
/// ohne dass ein Transkript gelesen oder gelöscht wird; die übrigen Klassen
/// laufen unverändert.
///
/// # Errors
/// [`MaintenanceFailure`]: ungültige Payload (`Failed`, auch eine Frist
/// außerhalb des darstellbaren Bereichs), Abbruch (`Cancelled`) oder
/// Fristablauf (`TimedOut`). Bereits erfolgte Löschungen bleiben gültig
/// (jede ist unabhängig und idempotent). Der Fehler eines einzelnen
/// Verzeichnisses bzw. einer Store-Wurzel ist kein Job-Fehler, sondern ein
/// `error`-Eintrag dieses Verzeichnisses, gezählt in `failed_dirs`.
///
/// # Concurrency
/// Blockierende Datei-I/O; der Aufrufer führt den Kern in `spawn_blocking`
/// aus ([`run_retention_sweep_job`]).
pub fn execute_retention_sweep_with_cancel(
    spec: &RetentionSweepSpec,
    cancel: &SweepCancel,
) -> Result<serde_json::Value, MaintenanceFailure> {
    // Geprüft statt `+`: `deadline_secs` stammt aus der Payload (jeder `u64`),
    // eine Überschreitung wäre sonst ein Panic im Blocking-Thread.
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(spec.deadline_secs))
        .ok_or_else(|| {
            MaintenanceFailure::Failed(
                "deadline_secs liegt außerhalb des darstellbaren Bereichs".to_owned(),
            )
        })?;
    // Keine autoritative Anhänge-Quelle (Review 4505340178): Transkripte
    // werden übersprungen, nie ohne Anhänge-Schutz gelöscht.
    execute_until(spec, cancel, deadline, None)
}

// Kern von `execute_retention_sweep_with_cancel` mit fester Frist `deadline`
// (getrennt, damit Tests eine bereits abgelaufene Frist einsetzen können).
//
// `attached` ist die autoritative Anhänge-Quelle für `session_transcripts`
// (Vertrag: [`SweepOptions::is_attached`]; zur Ausführungszeit je Datei
// befragt, nie eine Momentaufnahme vom Einreihen). `None` überspringt die
// Klasse mit [`NO_ATTACHMENT_SOURCE`]; das ist heute der einzige
// Produktionsfall (siehe Moduldoku, „Freischalten“).
fn execute_until(
    spec: &RetentionSweepSpec,
    cancel: &SweepCancel,
    deadline: Instant,
    attached: Option<&dyn Fn(&str) -> bool>,
) -> Result<serde_json::Value, MaintenanceFailure> {
    if spec.schema_version != RETENTION_SWEEP_SCHEMA_VERSION {
        return Err(MaintenanceFailure::Failed(format!(
            "unbekannte Payload-Version {}",
            spec.schema_version
        )));
    }
    if spec.deadline_secs == 0 {
        return Err(MaintenanceFailure::Failed(
            "deadline_secs muss größer als 0 sein".to_owned(),
        ));
    }
    if !spec.home.is_absolute() || spec.project.as_ref().is_some_and(|p| !p.is_absolute()) {
        return Err(MaintenanceFailure::Failed(
            "Wurzeln müssen absolute Pfade sein".to_owned(),
        ));
    }
    spec.config.validate().map_err(MaintenanceFailure::Failed)?;

    let resolved = resolve_all(&spec.config);
    for id in &spec.classes {
        if !resolved.iter().any(|class| class.class.id == id) {
            return Err(MaintenanceFailure::Failed(format!(
                "unbekannte Klasse `{id}`"
            )));
        }
    }
    let selected: Vec<_> = resolved
        .into_iter()
        .filter(|class| {
            spec.classes.is_empty() || spec.classes.iter().any(|id| id == class.class.id)
        })
        .collect();

    let mode = if spec.apply {
        SweepMode::Apply
    } else {
        SweepMode::DryRun
    };
    let roots = Roots {
        home: spec.home.clone(),
        project: spec.project.clone(),
    };
    let run = StoreRun {
        mode,
        deadline,
        attached,
    };
    let mut classes = Vec::new();
    let mut totals = SweepCounts::default();
    for class in &selected {
        if cancel.is_cancelled() {
            return Err(MaintenanceFailure::Cancelled(
                "retention sweep cancelled between classes".to_owned(),
            ));
        }
        if Instant::now() >= deadline {
            totals.timed_out = true;
            break;
        }
        let id = class.class.id;
        let kind = class.class.kind.as_str();
        // Store-eigene Klassen nie über den generischen Sweeper: ihre
        // Sicherheitsprüfungen (für `dod_verdicts` die Kette) gehören dem
        // Session-Store (PL-100 WP 2.3).
        let swept = match StoreClass::from_id(id) {
            Some(store_class) => {
                sweep_store_class(&spec.config, class, store_class, &roots, run, &mut totals)
            }
            None => sweep_generic_class(class, &roots, mode, deadline, &mut totals)
                .map(ClassSweep::Dirs),
        };
        match swept {
            Ok(ClassSweep::Dirs(dirs)) => {
                classes.push(serde_json::json!({"id": id, "kind": kind, "dirs": dirs}));
            }
            // Ohne Store-Aufruf übersprungen (inhaltsfreier, stabiler Grund),
            // kein Fehler des Jobs; die übrigen Klassen laufen weiter.
            Ok(ClassSweep::Skipped(reason)) => {
                classes.push(serde_json::json!({
                    "id": id,
                    "kind": kind,
                    "skipped": reason,
                }));
            }
            // Opt-in nicht erteilt oder Klasse abgeschaltet: überspringen,
            // nichts löschen — kein Fehler des Jobs.
            Err(error @ (RetentionError::OptInRequired(_) | RetentionError::Disabled(_))) => {
                classes.push(serde_json::json!({
                    "id": id,
                    "kind": kind,
                    "skipped": error.to_string(),
                }));
            }
            Err(error) => {
                return Err(MaintenanceFailure::Failed(format!("{id}: {error}")));
            }
        }
    }
    if cancel.is_cancelled() {
        return Err(MaintenanceFailure::Cancelled(
            "retention sweep cancelled".to_owned(),
        ));
    }
    if totals.timed_out {
        return Err(MaintenanceFailure::TimedOut(
            "the retention sweep did not finish in time; the rest stays untouched".to_owned(),
        ));
    }
    if totals.failed_dirs > 0 || totals.errors > 0 {
        // Nur Zählwerte: Pfade und Meldungen stehen im Job-Ergebnis.
        tracing::warn!(
            failed_dirs = totals.failed_dirs,
            errors = totals.errors,
            "retention sweep finished with failures; see the job result"
        );
    }
    let (removed_segments, removed_records) = totals.segment_fields();
    Ok(serde_json::json!({
        "mode": if spec.apply { "apply" } else { "dry_run" },
        "removed": totals.removed,
        "removed_bytes": totals.removed_bytes,
        "removed_segments": removed_segments,
        "removed_records": removed_records,
        "errors": totals.errors,
        "failed_dirs": totals.failed_dirs,
        "classes": classes,
    }))
}

// Zählwerte eines Verzeichnisses bzw. die Summen über alle Klassen.
#[derive(Clone, Copy, Debug, Default)]
struct SweepCounts {
    // Entfernte (Probelauf: entfernbare) Dateien.
    removed: usize,
    // Deren Bytes zum Planungszeitpunkt.
    removed_bytes: u64,
    // `dod_verdicts`: vom `VerdictStore` entfernte Segmente.
    removed_segments: usize,
    // `dod_verdicts`: Datensätze in den entfernten, abgelaufenen Segmenten.
    removed_records: u64,
    // `removed_segments`/`removed_records` sind unbekannt (ein
    // `dod_verdicts`-Probelauf: der `VerdictStore` hat keine Plan-API).
    segments_unplanned: bool,
    // Einzelfehler (`ItemError`s) der Verzeichnisse, Kettenfehler und
    // abgelehnte Overrides eingeschlossen.
    errors: usize,
    // Verzeichnisse bzw. Store-Wurzeln, deren Sweep als Ganzes scheiterte.
    failed_dirs: usize,
    // Die Frist lief ab; der Rest blieb unberührt.
    timed_out: bool,
}

impl SweepCounts {
    // Die Zählwerte eines generischen Verzeichnis-Sweeps.
    fn of_report(report: &Report) -> Self {
        Self {
            removed: report.removed.len(),
            removed_bytes: report
                .removed
                .iter()
                .fold(0_u64, |sum, removal| sum.saturating_add(removal.bytes)),
            errors: report.errors.len(),
            timed_out: report.timed_out,
            ..Self::default()
        }
    }

    // Die Zählwerte eines Store-Sweeps (inklusive Verdict-Segmente);
    // `segments_planned = false` markiert die Segmentzahlen als unbekannt.
    fn of_store(report: &StoreSweepReport, segments_planned: bool) -> Self {
        Self {
            removed: report.removed.len(),
            removed_bytes: report
                .removed
                .iter()
                .fold(0_u64, |sum, removal| sum.saturating_add(removal.bytes)),
            removed_segments: report.removed_segments,
            removed_records: report.removed_records,
            segments_unplanned: !segments_planned,
            errors: report.errors.len(),
            failed_dirs: 0,
            timed_out: report.timed_out,
        }
    }

    // Ein Verzeichnis, dessen Sweep als Ganzes scheiterte.
    fn failed_dir(segments_planned: bool) -> Self {
        Self {
            segments_unplanned: !segments_planned,
            failed_dirs: 1,
            ..Self::default()
        }
    }

    // Addiert `other` sättigend auf.
    fn absorb(&mut self, other: Self) {
        self.removed = self.removed.saturating_add(other.removed);
        self.removed_bytes = self.removed_bytes.saturating_add(other.removed_bytes);
        self.removed_segments = self.removed_segments.saturating_add(other.removed_segments);
        self.removed_records = self.removed_records.saturating_add(other.removed_records);
        self.segments_unplanned |= other.segments_unplanned;
        self.errors = self.errors.saturating_add(other.errors);
        self.failed_dirs = self.failed_dirs.saturating_add(other.failed_dirs);
        self.timed_out |= other.timed_out;
    }

    // `removed_segments` und `removed_records` als JSON: `null`, solange sie
    // unbekannt sind (nie eine 0, die `apply` widerlegen würde).
    fn segment_fields(self) -> (serde_json::Value, serde_json::Value) {
        if self.segments_unplanned {
            (serde_json::Value::Null, serde_json::Value::Null)
        } else {
            (
                serde_json::Value::from(self.removed_segments),
                serde_json::Value::from(self.removed_records),
            )
        }
    }
}

// Ergebnis einer Klasse im Job: ihre Verzeichnis-Einträge oder ein
// inhaltsfreier Grund, warum sie ohne Store-Aufruf übersprungen wurde.
enum ClassSweep {
    // Je Verzeichnis bzw. Store-Wurzel ein Ergebnis-Eintrag.
    Dirs(Vec<serde_json::Value>),
    // Übersprungen mit stabilem Kurztext (z. B. [`NO_ATTACHMENT_SOURCE`]).
    Skipped(&'static str),
}

// Was jede store-eigene Klasse eines Laufs teilt: Modus, Frist und die
// Anhänge-Quelle für `session_transcripts`.
#[derive(Clone, Copy)]
struct StoreRun<'a> {
    // Probelauf oder Löschen.
    mode: SweepMode,
    // Frist des ganzen Jobs.
    deadline: Instant,
    // Autoritative Anhänge-Quelle (Vertrag: `SweepOptions::is_attached`);
    // `None` überspringt `session_transcripts`.
    attached: Option<&'a dyn Fn(&str) -> bool>,
}

// Eine Klasse über den generischen Sweeper von `harw-retention` (Klassen, die
// kein Store besitzt). `Err` nur für Opt-in bzw. Abschaltung (aus
// `ResolvedClass::policy`); Verzeichnisfehler werden zu `error`-Einträgen.
fn sweep_generic_class(
    class: &ResolvedClass,
    roots: &Roots,
    mode: SweepMode,
    deadline: Instant,
    totals: &mut SweepCounts,
) -> Result<Vec<serde_json::Value>, RetentionError> {
    let outcomes = class.sweep(roots, mode, Some(deadline), SystemTime::now())?;
    let mut dirs = Vec::with_capacity(outcomes.len());
    for outcome in outcomes {
        match outcome.result {
            Ok(report) => {
                let counts = SweepCounts::of_report(&report);
                totals.absorb(counts);
                dirs.push(serde_json::json!({
                    "dir": display_dir(&outcome.dir),
                    "removed": counts.removed,
                    "removed_bytes": counts.removed_bytes,
                    "kept": report.kept,
                    "skipped": report.skipped,
                    "errors": counts.errors,
                    "timed_out": report.timed_out,
                }));
            }
            Err(error) => {
                totals.absorb(SweepCounts::failed_dir(true));
                dirs.push(dir_error_json(&outcome.dir, &error));
            }
        }
    }
    Ok(dirs)
}

// Eine store-eigene Klasse über
// `harw_session_store::retention::retention_sweep_at` je Store-Wurzel
// ([`store_targets`]).
//
// Opt-in und Abschaltung werden vorab einmal geprüft, wie in
// `ResolvedClass::sweep`: auch eine Klasse ohne Wurzel (kein Profil) erscheint
// dann als übersprungen. Danach wird `session_transcripts` ohne
// Anhänge-Quelle (`run.attached` ist `None`) mit [`NO_ATTACHMENT_SOURCE`]
// übersprungen, ohne den Store aufzurufen (Review 4505340178). Der Fehler
// einer einzelnen Wurzel (Sperre belegt, Symlink, I/O, Ablehnung durch den
// `VerdictStore`) wird zu deren `error`-Eintrag und zählt in `failed_dirs`;
// die übrigen Wurzeln laufen weiter.
fn sweep_store_class(
    config: &RetentionConfig,
    class: &ResolvedClass,
    store_class: StoreClass,
    roots: &Roots,
    run: StoreRun<'_>,
    totals: &mut SweepCounts,
) -> Result<ClassSweep, RetentionError> {
    class.policy(run.mode, Some(run.deadline))?;
    let id = class.class.id;
    if store_class == StoreClass::SessionTranscripts && run.attached.is_none() {
        tracing::debug!(
            class = id,
            reason = NO_ATTACHMENT_SOURCE,
            "store retention class skipped"
        );
        return Ok(ClassSweep::Skipped(NO_ATTACHMENT_SOURCE));
    }
    // Nur `transcript_item` des Stores fragt den Haken; die übrigen Klassen
    // ignorieren ihn.
    let options = SweepOptions {
        is_attached: run.attached,
    };
    // `resolve_all` hat die Klasse aus genau dieser Tabelle aufgelöst; fehlt
    // sie, wird nichts angefasst.
    let cfg = config
        .class_config(id)
        .ok_or_else(|| RetentionError::UnknownClass(id.to_owned()))?;
    // Jede Store-Klasse plant echt, auch der Probelauf von `dod_verdicts`:
    // er fragt `VerdictStore::plan_retention` (nur lesend) und meldet genau
    // die Segment- und Datensatzzahlen, die `apply` zur selben Zeit melden
    // würde. `planned` bleibt im Ergebnis (immer `true`), damit dessen Form
    // stabil bleibt.
    let segments_planned = true;
    let targets = store_targets(store_class, class, roots);
    let mut dirs = Vec::with_capacity(targets.len());
    for target in targets {
        match retention_sweep_at(
            &target.root,
            store_class,
            cfg,
            run.mode,
            Some(run.deadline),
            SystemTime::now(),
            &options,
        ) {
            Ok(report) => {
                let counts = SweepCounts::of_store(&report, segments_planned);
                totals.absorb(counts);
                tracing::debug!(
                    class = id,
                    removed = counts.removed,
                    protected = report.protected.len(),
                    errors = counts.errors,
                    removed_segments = counts.removed_segments,
                    removed_records = counts.removed_records,
                    planned = segments_planned,
                    timed_out = counts.timed_out,
                    "store retention class swept"
                );
                dirs.push(store_dir_json(&target.dir, &report, counts));
            }
            Err(error) => {
                tracing::debug!(class = id, error = %error, "store retention root refused");
                totals.absorb(SweepCounts::failed_dir(segments_planned));
                dirs.push(dir_error_json(&target.dir, &error));
            }
        }
    }
    Ok(ClassSweep::Dirs(dirs))
}

// Eine Wurzel für `retention_sweep_at` und das Verzeichnis, das das Ergebnis
// dafür nennt.
struct StoreTarget {
    // Wurzel, mit der der besitzende Store gebaut wird.
    root: PathBuf,
    // Verzeichnis, das der Store darunter tatsächlich durchsucht.
    dir: PathBuf,
}

// Die Store-Wurzeln einer Klasse (siehe `retention_sweep_at`): die
// Sessions-Verzeichnisse aus dem Klassen-Resolver (je Profil) für Transkripte
// und Backups, die Home für Freeze-Records und den `VerdictStore`.
fn store_targets(
    store_class: StoreClass,
    class: &ResolvedClass,
    roots: &Roots,
) -> Vec<StoreTarget> {
    match store_class {
        StoreClass::SessionTranscripts | StoreClass::SessionCorruptBackups => {
            (class.class.resolve_dirs)(roots)
                .into_iter()
                .map(|dir| StoreTarget {
                    root: dir.clone(),
                    dir,
                })
                .collect()
        }
        StoreClass::FreezeResolved => vec![StoreTarget {
            root: roots.home.clone(),
            dir: FreezeStore::new(&roots.home).root().to_path_buf(),
        }],
        StoreClass::DodVerdicts => vec![StoreTarget {
            root: roots.home.clone(),
            dir: VerdictStore::new(&roots.home).root().to_path_buf(),
        }],
    }
}

// Ergebnis-Eintrag eines Store-Verzeichnisses: die generischen Felder plus
// `protected`, `planned` (ob `removed_segments`/`removed_records` bekannt
// sind; sonst `null`), `removed_segments`, `removed_records` und die
// inhaltsfreien Meldungen (`error_messages`, ohne Pfade einzelner Dateien:
// Transkriptpfade tragen Session-Ids).
fn store_dir_json(dir: &Path, report: &StoreSweepReport, counts: SweepCounts) -> serde_json::Value {
    let error_messages: Vec<String> = report
        .errors
        .iter()
        .take(MAX_ERROR_MESSAGES_PER_DIR)
        .map(|error| escape_line(&error.message, MAX_RESULT_TEXT_CHARS))
        .collect();
    let (removed_segments, removed_records) = counts.segment_fields();
    serde_json::json!({
        "dir": display_dir(dir),
        "removed": counts.removed,
        "removed_bytes": counts.removed_bytes,
        "kept": report.kept,
        "skipped": report.skipped,
        "protected": report.protected.len(),
        "errors": counts.errors,
        "error_messages": error_messages,
        "timed_out": report.timed_out,
        "planned": !counts.segments_unplanned,
        "removed_segments": removed_segments,
        "removed_records": removed_records,
    })
}

// Ergebnis-Eintrag eines Verzeichnisses, dessen Sweep als Ganzes scheiterte.
fn dir_error_json(dir: &Path, error: &RetentionError) -> serde_json::Value {
    serde_json::json!({
        "dir": display_dir(dir),
        "error": escape_line(&error.to_string(), MAX_RESULT_TEXT_CHARS),
    })
}

// Verzeichnisangabe für das Ergebnis: als Text (ein Nicht-UTF-8-Pfad wird
// verlustbehaftet statt zum Serialisierungsfehler) und escapt, weil
// Job-Ergebnisse im Terminal landen.
fn display_dir(dir: &Path) -> String {
    escape_line(&dir.to_string_lossy(), MAX_RESULT_TEXT_CHARS)
}

/// Gemeinsamer asynchroner Treiber: Handler-Kern in `spawn_blocking`, Abbruch
/// per Signal und per Store-Poll (`Cancelled` im Job-Store).
pub async fn run_retention_sweep_job<F>(
    spec: RetentionSweepSpec,
    work_id: WorkId,
    store: Arc<JobStore>,
    cancel_signal: F,
) -> JobOutcome
where
    F: std::future::Future<Output = ()> + Send,
{
    if crate::memory_job::store_says_cancelled(&store, &work_id) {
        return JobOutcome::Cancelled {
            reason: "cancelled before the retention sweep started".to_owned(),
        };
    }
    let flag = SweepCancel::new();
    let wait = Duration::from_secs(spec.deadline_secs).saturating_add(BLOCKING_GRACE);
    let thread_flag = flag.clone();
    let blocking = tokio::task::spawn_blocking(move || {
        execute_retention_sweep_with_cancel(&spec, &thread_flag)
    });
    let hard_stop = tokio::time::sleep(wait);
    let mut ticker = tokio::time::interval(CANCEL_POLL_INTERVAL);
    tokio::pin!(blocking, hard_stop, cancel_signal);
    let mut signal_seen = false;
    let joined = loop {
        tokio::select! {
            joined = &mut blocking => break Some(joined),
            () = &mut cancel_signal, if !signal_seen => {
                signal_seen = true;
                flag.cancel();
            }
            _ = ticker.tick() => {
                if crate::memory_job::store_says_cancelled(&store, &work_id) {
                    flag.cancel();
                }
            }
            () = &mut hard_stop => break None,
        }
    };
    match joined {
        Some(Ok(Ok(result))) => JobOutcome::Succeeded { result },
        Some(Ok(Err(failure))) => {
            tracing::warn!(work_id = %work_id.as_str(), reason = %failure.reason(), "retention sweep ended without success");
            failure.into_outcome()
        }
        Some(Err(join_error)) => {
            tracing::warn!(work_id = %work_id.as_str(), error = %join_error, "retention sweep task failed");
            JobOutcome::Failed {
                reason: "retention sweep task failed".to_owned(),
            }
        }
        None => {
            flag.cancel();
            MaintenanceFailure::TimedOut("the retention sweep did not finish in time".to_owned())
                .into_outcome()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_retention::ClassConfig;
    use harw_session_store::{Freeze, NewVerdict, StoredClassification, StoredSeverity};
    use harw_types::{CgroupId, ContentDigest, FindingId};

    // Schlägt mit `what` fehl, wenn `condition` nicht gilt.
    fn ensure(condition: bool, what: &str) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(TestError::Unexpected(what.to_owned()))
        }
    }

    // Führt den Handler-Kern ohne Abbruch aus; ein Misserfolg wird zum
    // Testfehler.
    fn run(spec: &RetentionSweepSpec) -> TestResult<serde_json::Value> {
        execute_retention_sweep_with_cancel(spec, &SweepCancel::new())
            .map_err(|failure| TestError::Unexpected(failure.reason()))
    }

    // Wie `run`, aber über die Naht des Kerns mit der autoritativen
    // Anhänge-Quelle `attached` (die Produktion hat heute keine).
    fn run_with_attachments(
        spec: &RetentionSweepSpec,
        attached: &dyn Fn(&str) -> bool,
    ) -> TestResult<serde_json::Value> {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(spec.deadline_secs))
            .ok_or(TestError::Missing("a representable deadline"))?;
        execute_until(spec, &SweepCancel::new(), deadline, Some(attached))
            .map_err(|failure| TestError::Unexpected(failure.reason()))
    }

    // `<home>/profiles/default/sessions` mit je einem Transkript `<id>.jsonl`
    // für jede Id aus `ids`; die aus `old` sind 400 Tage alt.
    fn sessions_with(home: &tempfile::TempDir, ids: &[&str], old: &[&str]) -> TestResult<PathBuf> {
        let sessions = home
            .path()
            .join("profiles")
            .join("default")
            .join("sessions");
        std::fs::create_dir_all(&sessions).map_err(ctx("create sessions dir"))?;
        for id in ids {
            std::fs::write(sessions.join(format!("{id}.jsonl")), b"{}\n")
                .map_err(ctx("write transcript"))?;
        }
        for id in old {
            age(&sessions.join(format!("{id}.jsonl")), 400)?;
        }
        Ok(sessions)
    }

    // `session_transcripts` mit Opt-in; die jüngste Datei bleibt immer.
    fn transcripts_enabled() -> ClassConfig {
        ClassConfig {
            enabled: Some(true),
            keep_newest: Some(1),
            ..ClassConfig::default()
        }
    }

    // Der Wert unter dem JSON-Pointer `pointer`.
    fn field<'a>(
        value: &'a serde_json::Value,
        pointer: &'static str,
    ) -> TestResult<&'a serde_json::Value> {
        value.pointer(pointer).ok_or(TestError::Missing(pointer))
    }

    // Die Zahl unter dem JSON-Pointer `pointer`.
    fn number(value: &serde_json::Value, pointer: &'static str) -> TestResult<u64> {
        field(value, pointer)?
            .as_u64()
            .ok_or(TestError::Missing(pointer))
    }

    // Der Text unter dem JSON-Pointer `pointer`.
    fn text<'a>(value: &'a serde_json::Value, pointer: &'static str) -> TestResult<&'a str> {
        field(value, pointer)?
            .as_str()
            .ok_or(TestError::Missing(pointer))
    }

    // Der Ergebnis-Eintrag der Klasse `id`.
    fn class_entry<'a>(
        value: &'a serde_json::Value,
        id: &'static str,
    ) -> TestResult<&'a serde_json::Value> {
        field(value, "/classes")?
            .as_array()
            .and_then(|classes| {
                classes
                    .iter()
                    .find(|class| class.get("id").and_then(serde_json::Value::as_str) == Some(id))
            })
            .ok_or(TestError::Missing(id))
    }

    /// `<home>/logs` mit vier `tui.log.N`-Dateien; Klasse `tui_log` auf
    /// `max_files = 1` begrenzt, damit drei fallen würden.
    fn setup() -> TestResult<(tempfile::TempDir, RetentionSweepSpec)> {
        let home = tempfile::tempdir().map_err(ctx("create temp home"))?;
        let logs = home.path().join("logs");
        std::fs::create_dir_all(&logs).map_err(ctx("create logs dir"))?;
        for n in 1..=4 {
            std::fs::write(logs.join(format!("tui.log.{n}")), b"x")
                .map_err(ctx("write log file"))?;
        }
        let config = RetentionConfig {
            tui_log: ClassConfig {
                max_files: Some(1),
                keep_newest: Some(1),
                ..ClassConfig::default()
            },
            ..RetentionConfig::default()
        };
        let mut spec = RetentionSweepSpec::new(home.path().to_path_buf(), None, config);
        spec.classes = vec!["tui_log".to_owned()];
        Ok((home, spec))
    }

    fn count_logs(home: &tempfile::TempDir) -> TestResult<usize> {
        Ok(std::fs::read_dir(home.path().join("logs"))
            .map_err(ctx("list logs dir"))?
            .count())
    }

    #[test]
    fn dry_run_reports_but_deletes_nothing() -> TestResult {
        let (home, spec) = setup()?;
        let value = run(&spec)?;
        ensure(text(&value, "/mode")? == "dry_run", "dry run mode")?;
        ensure(number(&value, "/removed")? == 3, "three files would fall")?;
        ensure(
            number(&value, "/removed_segments")? == 0,
            "a generic class retires no verdict segment",
        )?;
        ensure(number(&value, "/errors")? == 0, "no item errors")?;
        ensure(number(&value, "/failed_dirs")? == 0, "no failed directory")?;
        ensure(count_logs(&home)? == 4, "nothing deleted in a dry run")
    }

    #[test]
    fn apply_deletes_only_what_the_policy_selects() -> TestResult {
        let (home, mut spec) = setup()?;
        spec.apply = true;
        let value = run(&spec)?;
        ensure(text(&value, "/mode")? == "apply", "apply mode")?;
        // The generic directory entry exists with its own fields ...
        ensure(
            number(&value, "/classes/0/dirs/0/removed")? == 3,
            "the generic directory entry reports its removals",
        )?;
        ensure(
            number(&value, "/classes/0/dirs/0/errors")? == 0,
            "the generic directory entry reports its errors",
        )?;
        // ... and none of the store-only fields.
        for store_only in [
            "/classes/0/dirs/0/protected",
            "/classes/0/dirs/0/planned",
            "/classes/0/dirs/0/removed_segments",
            "/classes/0/dirs/0/removed_records",
            "/classes/0/dirs/0/error_messages",
        ] {
            ensure(
                field(&value, store_only).is_err(),
                "generic directories keep their shape",
            )?;
        }
        ensure(number(&value, "/failed_dirs")? == 0, "no failed directory")?;
        ensure(count_logs(&home)? == 1, "one file kept")
    }

    /// Eine Frist, die als `Instant` nicht darstellbar ist (`harw cleanup
    /// --deadline-secs` nimmt jeden `u64`), ist eine typisierte Ablehnung,
    /// kein Panic im Blocking-Thread.
    #[test]
    fn an_out_of_range_deadline_is_a_typed_failure_and_touches_nothing() -> TestResult {
        let (home, mut spec) = setup()?;
        spec.apply = true;
        spec.deadline_secs = u64::MAX;
        ensure(
            matches!(
                execute_retention_sweep_with_cancel(&spec, &SweepCancel::new()),
                Err(MaintenanceFailure::Failed(_))
            ),
            "an unrepresentable deadline is rejected",
        )?;
        ensure(count_logs(&home)? == 4, "nothing touched")
    }

    /// Ohne Opt-in meldet `apply` zuerst das fehlende Opt-in; der Probelauf
    /// (kein Opt-in nötig) überspringt Transkripte mangels Anhänge-Quelle.
    #[test]
    fn security_classes_are_skipped_without_opt_in() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temp home"))?;
        let sessions = sessions_with(&home, &["old-a", "fresh"], &["old-a"])?;
        let mut spec =
            RetentionSweepSpec::new(home.path().to_path_buf(), None, RetentionConfig::default());
        spec.apply = true;
        spec.classes = vec!["session_transcripts".to_owned()];
        let value = run(&spec)?;
        let skipped = text(&value, "/classes/0/skipped")?;
        ensure(
            skipped.contains("opt-in"),
            "the missing opt-in is reported first",
        )?;
        ensure(number(&value, "/removed")? == 0, "nothing removed")?;

        spec.apply = false;
        let dry = run(&spec)?;
        ensure(
            text(&dry, "/classes/0/skipped")? == NO_ATTACHMENT_SOURCE,
            "a dry run never plans transcripts without an attachment source",
        )?;
        ensure(number(&dry, "/removed")? == 0, "nothing would be removed")?;
        ensure(
            sessions.join("old-a.jsonl").exists(),
            "the old transcript stays",
        )
    }

    #[test]
    fn unknown_class_and_bad_payload_fail_without_touching_anything() -> TestResult {
        let (home, mut spec) = setup()?;
        spec.classes = vec!["nope".to_owned()];
        ensure(
            matches!(
                execute_retention_sweep_with_cancel(&spec, &SweepCancel::new()),
                Err(MaintenanceFailure::Failed(_))
            ),
            "unknown class fails",
        )?;
        spec.classes.clear();
        spec.deadline_secs = 0;
        ensure(
            matches!(
                execute_retention_sweep_with_cancel(&spec, &SweepCancel::new()),
                Err(MaintenanceFailure::Failed(_))
            ),
            "zero deadline fails",
        )?;
        ensure(count_logs(&home)? == 4, "nothing touched")
    }

    #[test]
    fn a_cancelled_sweep_ends_as_cancelled_and_leaves_the_files() -> TestResult {
        let (home, mut spec) = setup()?;
        spec.apply = true;
        let cancel = SweepCancel::new();
        cancel.cancel();
        ensure(
            matches!(
                execute_retention_sweep_with_cancel(&spec, &cancel),
                Err(MaintenanceFailure::Cancelled(_))
            ),
            "cancelled",
        )?;
        ensure(count_logs(&home)? == 4, "nothing deleted after cancel")
    }

    #[test]
    fn payload_round_trips_and_rejects_unknown_fields() -> TestResult {
        let (_home, spec) = setup()?;
        let value = serde_json::to_value(&spec).map_err(ctx("serialize payload"))?;
        let back: RetentionSweepSpec =
            serde_json::from_value(value.clone()).map_err(ctx("deserialize payload"))?;
        ensure(back == spec, "round trip")?;
        let mut tampered = value;
        let previous = tampered
            .as_object_mut()
            .ok_or(TestError::Missing("payload object"))?
            .insert("extra".to_owned(), serde_json::json!(1));
        ensure(previous.is_none(), "no `extra` field before tampering")?;
        ensure(
            serde_json::from_value::<RetentionSweepSpec>(tampered).is_err(),
            "unknown field rejected",
        )
    }

    #[test]
    fn kind_detection_matches_only_the_sweep_kind() {
        assert!(is_retention_sweep_kind(&JobKind::Custom(
            RETENTION_SWEEP_JOB_KIND.to_owned()
        )));
        assert!(!is_retention_sweep_kind(&JobKind::Custom(
            "memory_maintenance".to_owned()
        )));
    }

    // ── store-eigene Klassen (PL-100 WP 2.3, Review 4505256692) ─────────────

    // Ein Modell-Verdict zu einem exportierten Befund, verfasst um
    // `created_at`.
    fn new_verdict(rationale: &str, created_at: Timestamp) -> TestResult<NewVerdict> {
        Ok(NewVerdict {
            finding_id: FindingId::try_from_str("7f1c2a4e-0b8d-4c55-9a1e-3d2f6b7c8e90")
                .map_err(ctx("finding id"))?,
            record_digest: ContentDigest::of(b"exported finding record"),
            classification: StoredClassification::Suspicious,
            severity: StoredSeverity::High,
            suggested_response: None,
            confidence: None,
            rationale: rationale.to_owned(),
            model: None,
            created_at,
        })
    }

    // `days` Tage vor `now`.
    fn days_ago(now: Timestamp, days: i64) -> TestResult<Timestamp> {
        now.checked_sub(SignedDuration::from_secs(days.saturating_mul(86_400)))
            .map_err(ctx("timestamp arithmetic"))
    }

    // Drei Ein-Datensatz-Segmente, 93 und 91 Tage alt (abgelaufen) und 4
    // Tage alt (frisch), ohne Anker: ein Segment endet, sobald der nächste
    // Datensatz einen Tag jünger ist als sein erster. Die Daten halten die
    // Retention, die `append` beim Segmentwechsel selbst ausführt, aus der
    // Vorlage heraus: beginnt der frische Datensatz Segment 3, sieht sie (mit
    // seinem `created_at` als Jetzt) nur Segment 1, das dann erst 89 Tage alt
    // ist. Mit 200/198/0 Tagen hätte sie Segment 1 schon dort entfernt und
    // einen Anker geschrieben.
    fn verdict_store(home: &Path) -> TestResult<VerdictStore> {
        let store = VerdictStore::new(home);
        let now = Timestamp::now();
        for (rationale, days) in [("reason-0", 93), ("reason-1", 91), ("reason-2", 4)] {
            store
                .append(&new_verdict(rationale, days_ago(now, days)?)?)
                .map_err(ctx("append verdict"))?;
        }
        Ok(store)
    }

    // Die Kettensegmente (`*.jsonl`) des Stores, nach Name (= seq) sortiert.
    fn verdict_segments(store: &VerdictStore) -> TestResult<Vec<PathBuf>> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(store.root())
            .map_err(ctx("list verdict store"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .collect();
        out.sort();
        Ok(out)
    }

    // `dod_verdicts` mit Opt-in und genau den Grenzen des Stores.
    fn verdicts_enabled() -> ClassConfig {
        ClassConfig {
            enabled: Some(true),
            ..ClassConfig::default()
        }
    }

    // Ein Sweep nur über `dod_verdicts` mit `cfg`.
    fn verdict_spec(home: &Path, cfg: ClassConfig, apply: bool) -> RetentionSweepSpec {
        let config = RetentionConfig {
            dod_verdicts: cfg,
            ..RetentionConfig::default()
        };
        let mut spec = RetentionSweepSpec::new(home.to_path_buf(), None, config);
        spec.classes = vec!["dod_verdicts".to_owned()];
        spec.apply = apply;
        spec
    }

    // Setzt die mtime von `path` auf `days` Tage vor jetzt.
    fn age(path: &Path, days: u64) -> TestResult {
        let then = SystemTime::now()
            .checked_sub(Duration::from_secs(days.saturating_mul(86_400)))
            .ok_or(TestError::Missing("a time in the past"))?;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(ctx("open for mtime"))?;
        file.set_modified(then).map_err(ctx("set mtime"))
    }

    /// Review 4505256692: `harw cleanup --class dod_verdicts --apply` mustert
    /// die abgelaufenen Segmente über den `VerdictStore` aus (neu verankert,
    /// nie Datei für Datei), meldet die Zählwerte und der Rest der Kette
    /// bleibt prüfbar.
    #[test]
    fn dod_verdicts_apply_retires_expired_segments_and_the_rest_still_verifies() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temp home"))?;
        let store = verdict_store(home.path())?;
        let before = verdict_segments(&store)?;
        assert_eq!(before.len(), 3, "one segment per day-separated record");

        let value = run(&verdict_spec(home.path(), verdicts_enabled(), true))?;
        assert_eq!(text(&value, "/mode")?, "apply");
        assert_eq!(text(&value, "/classes/0/id")?, "dod_verdicts");
        assert_eq!(number(&value, "/removed_segments")?, 2);
        assert_eq!(number(&value, "/removed_records")?, 2);
        assert_eq!(number(&value, "/removed")?, 0, "no file-by-file removal");
        assert_eq!(number(&value, "/errors")?, 0);
        assert_eq!(number(&value, "/failed_dirs")?, 0);
        assert_eq!(
            field(&value, "/classes/0/dirs/0/planned")?.as_bool(),
            Some(true),
            "apply reports real counts"
        );
        let dirs = field(&value, "/classes/0/dirs")?
            .as_array()
            .ok_or(TestError::Missing("dirs"))?;
        assert_eq!(dirs.len(), 1, "one verdict store below the home");
        assert_eq!(
            Some(text(&value, "/classes/0/dirs/0/dir")?),
            store.root().to_str()
        );
        assert_eq!(number(&value, "/classes/0/dirs/0/removed_segments")?, 2);
        assert_eq!(number(&value, "/classes/0/dirs/0/removed_records")?, 2);
        assert_eq!(number(&value, "/classes/0/dirs/0/errors")?, 0);
        assert!(
            field(&value, "/classes/0/dirs/0/error_messages")?
                .as_array()
                .is_some_and(Vec::is_empty),
            "no error messages"
        );

        let after = verdict_segments(&store)?;
        assert_eq!(after.len(), 1);
        assert_eq!(after.first(), before.last(), "the fresh segment stays");
        assert!(
            store.root().join("anchor.json").exists(),
            "the chain is re-anchored, not cut"
        );
        let summary = store.verify_chain().map_err(ctx("verify chain"))?;
        assert_eq!(summary.records, 1);
        assert_eq!(summary.first_seq, 3);

        // Idempotent: a second run finds nothing more to retire.
        let again = run(&verdict_spec(home.path(), verdicts_enabled(), true))?;
        assert_eq!(number(&again, "/removed_segments")?, 0);
        assert_eq!(verdict_segments(&store)?, after);
        Ok(())
    }

    /// Review 4505256692: ein Probelauf über alle Klassen (`harw cleanup` ohne
    /// `--class`) erreicht den `VerdictStore` und meldet über dessen
    /// Plan-API (`plan_retention`, nur lesend) genau, was `apply` ausmustern
    /// würde: die zwei fälligen Segmente mit ihren zwei Datensätzen
    /// (`planned: true`). Er löscht nichts und schreibt keinen Anker, auch
    /// mit Opt-in; danach mustert `apply` genau diese Zahl aus.
    #[test]
    fn dod_verdicts_dry_run_reaches_the_store_and_deletes_nothing() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temp home"))?;
        let store = verdict_store(home.path())?;
        let before = verdict_segments(&store)?;
        // Voraussetzung, damit eine geänderte Vorlage (oder Retention beim
        // Schreiben, `append`) nicht unbemerkt die Erwartung 2/2 verschiebt: drei
        // Segmente, noch kein Anker.
        ensure(
            before.len() == 3 && !store.root().join("anchor.json").exists(),
            "verdict fixture: three segments and no anchor",
        )?;
        let config = RetentionConfig {
            dod_verdicts: verdicts_enabled(),
            ..RetentionConfig::default()
        };
        let spec = RetentionSweepSpec::new(home.path().to_path_buf(), None, config);

        let value = run(&spec)?;
        assert_eq!(text(&value, "/mode")?, "dry_run");
        assert_eq!(
            number(&value, "/removed_segments")?,
            2,
            "planned, not unknown"
        );
        assert_eq!(number(&value, "/removed_records")?, 2);
        assert_eq!(number(&value, "/errors")?, 0);
        assert_eq!(number(&value, "/failed_dirs")?, 0);
        let verdicts = class_entry(&value, "dod_verdicts")?;
        assert_eq!(number(verdicts, "/dirs/0/errors")?, 0, "the chain verifies");
        assert_eq!(field(verdicts, "/dirs/0/planned")?.as_bool(), Some(true));
        assert_eq!(number(verdicts, "/dirs/0/removed_segments")?, 2);
        assert_eq!(number(verdicts, "/dirs/0/removed_records")?, 2);
        let freeze = class_entry(&value, "freeze_resolved")?;
        assert_eq!(field(freeze, "/dirs/0/planned")?.as_bool(), Some(true));
        assert_eq!(number(freeze, "/dirs/0/removed_segments")?, 0);
        assert_eq!(verdict_segments(&store)?, before, "nothing was deleted");
        assert!(
            !store.root().join("anchor.json").exists(),
            "no anchor written"
        );
        let summary = store.verify_chain().map_err(ctx("verify chain"))?;
        assert_eq!(summary.records, 3);

        // The plan holds: `apply` retires exactly what the dry run reported.
        let applied = run(&verdict_spec(home.path(), verdicts_enabled(), true))?;
        assert_eq!(number(&applied, "/removed_segments")?, 2);
        assert_eq!(number(&applied, "/removed_records")?, 2);
        assert_eq!(verdict_segments(&store)?.len(), 1);
        Ok(())
    }

    /// Ohne Opt-in bzw. abgeschaltet wird die Klasse bei `apply`
    /// übersprungen; der Store wird nicht einmal geöffnet.
    #[test]
    fn dod_verdicts_without_opt_in_or_disabled_do_nothing_on_apply() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temp home"))?;
        let store = verdict_store(home.path())?;
        let before = verdict_segments(&store)?;
        let disabled = ClassConfig {
            enabled: Some(false),
            ..ClassConfig::default()
        };
        for cfg in [ClassConfig::default(), disabled] {
            let value = run(&verdict_spec(home.path(), cfg, true))?;
            let skipped = text(&value, "/classes/0/skipped")?;
            assert!(skipped.contains("dod_verdicts"), "{skipped}");
            assert!(field(&value, "/classes/0/dirs").is_err(), "no store swept");
            assert_eq!(number(&value, "/removed_segments")?, 0);
        }
        assert_eq!(verdict_segments(&store)?, before, "nothing was deleted");
        assert!(!store.root().join("anchor.json").exists());
        Ok(())
    }

    /// Ein Kettenbruch erscheint als inhaltsfreier Fehler des Verzeichnisses
    /// (Probelauf und `apply`); `apply` mustert nur das geprüfte Präfix davor
    /// aus, das verletzte Segment bleibt sichtbar, der Job gelingt.
    #[test]
    fn dod_verdicts_report_a_broken_chain_content_free() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temp home"))?;
        let store = verdict_store(home.path())?;
        let second = verdict_segments(&store)?
            .get(1)
            .cloned()
            .ok_or(TestError::Missing("second segment"))?;
        let original = std::fs::read_to_string(&second).map_err(ctx("read segment"))?;
        let tampered = original.replacen("reason-1", "reason-X", 1);
        ensure(tampered != original, "the rationale must be found")?;
        std::fs::write(&second, tampered).map_err(ctx("tamper segment"))?;
        let home_text = home.path().to_string_lossy().into_owned();

        let dry = run(&verdict_spec(home.path(), verdicts_enabled(), false))?;
        assert_eq!(number(&dry, "/errors")?, 1, "visible at the top level");
        assert_eq!(number(&dry, "/failed_dirs")?, 0);
        assert_eq!(number(&dry, "/classes/0/dirs/0/errors")?, 1);
        let dry_message = text(&dry, "/classes/0/dirs/0/error_messages/0")?;
        assert!(
            !dry_message.contains("reason"),
            "content-free: {dry_message}"
        );
        assert_eq!(
            verdict_segments(&store)?.len(),
            3,
            "a dry run deletes nothing"
        );

        let value = run(&verdict_spec(home.path(), verdicts_enabled(), true))?;
        assert_eq!(
            number(&value, "/removed_segments")?,
            1,
            "verified prefix only"
        );
        assert_eq!(number(&value, "/removed_records")?, 1);
        assert_eq!(number(&value, "/errors")?, 1, "visible at the top level");
        assert_eq!(number(&value, "/classes/0/dirs/0/errors")?, 1);
        let message = text(&value, "/classes/0/dirs/0/error_messages/0")?;
        assert!(message.contains("segment 2"), "{message}");
        assert!(!message.contains("reason"), "content-free: {message}");
        assert!(!message.contains(home_text.as_str()), "no path: {message}");
        assert!(second.exists(), "the broken segment stays visible");
        Ok(())
    }

    /// Ein Grenzwert-Override für `dod_verdicts` wird über den Job
    /// abgelehnt statt ignoriert: Fehlereintrag mit dem Schlüssel, nichts
    /// gelöscht.
    #[test]
    fn dod_verdicts_limit_override_is_refused_through_the_job() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temp home"))?;
        let store = verdict_store(home.path())?;
        let before = verdict_segments(&store)?;
        let longer = ClassConfig {
            // 365 days instead of the store's fixed 90.
            max_age_secs: Some(31_536_000),
            ..verdicts_enabled()
        };
        let value = run(&verdict_spec(home.path(), longer, true))?;
        assert_eq!(number(&value, "/removed_segments")?, 0);
        assert_eq!(number(&value, "/errors")?, 1, "visible at the top level");
        assert_eq!(number(&value, "/classes/0/dirs/0/errors")?, 1);
        let message = text(&value, "/classes/0/dirs/0/error_messages/0")?;
        assert!(message.contains("max_age_secs"), "{message}");
        assert_eq!(verdict_segments(&store)?, before, "nothing was deleted");
        Ok(())
    }

    /// Transkripte laufen über den Store: Wurzel ist das Sessions-Verzeichnis
    /// des Profils (nicht die Home), und eine gesperrte Sitzung bleibt, wo der
    /// generische Sweeper sie gelöscht hätte. Die Anhänge-Quelle kennt hier
    /// keine angehängte Sitzung, damit allein die Sperre schützt.
    #[test]
    fn session_transcripts_route_through_the_store_and_spare_locked_sessions() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temp home"))?;
        let sessions = sessions_with(
            &home,
            &["old-gone", "old-locked", "fresh"],
            &["old-gone", "old-locked"],
        )?;
        let held = std::fs::File::create(sessions.join("old-locked.jsonl.lock"))
            .map_err(ctx("create transcript lock"))?;
        held.try_lock().map_err(ctx("hold transcript lock"))?;

        let config = RetentionConfig {
            session_transcripts: transcripts_enabled(),
            ..RetentionConfig::default()
        };
        let mut spec = RetentionSweepSpec::new(home.path().to_path_buf(), None, config);
        spec.classes = vec!["session_transcripts".to_owned()];
        spec.apply = true;
        let result = run_with_attachments(&spec, &|_session: &str| false);
        held.unlock().map_err(ctx("release transcript lock"))?;
        let value = result?;

        assert_eq!(
            Some(text(&value, "/classes/0/dirs/0/dir")?),
            sessions.to_str()
        );
        assert_eq!(number(&value, "/classes/0/dirs/0/removed")?, 1);
        assert_eq!(
            number(&value, "/classes/0/dirs/0/protected")?,
            1,
            "the locked session"
        );
        assert_eq!(number(&value, "/removed")?, 1);
        assert!(!sessions.join("old-gone.jsonl").exists());
        assert!(
            sessions.join("old-locked.jsonl").exists(),
            "a locked session is never deleted"
        );
        assert!(sessions.join("fresh.jsonl").exists());
        Ok(())
    }

    /// Review 4505340178: eine angehängte, aber ruhende Sitzung (keine
    /// gehaltene Sperre, keine offene Freigabe) mit altem Transkript bleibt
    /// bei `apply`, sobald die Anhänge-Quelle sie als angehängt meldet; ein
    /// ebenso altes, nicht angehängtes Transkript fällt. Der Probelauf meldet
    /// dasselbe und löscht nichts.
    #[test]
    fn an_attached_unlocked_old_transcript_survives_apply() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temp home"))?;
        let sessions = sessions_with(
            &home,
            &["old-gone", "old-attached", "fresh"],
            &["old-gone", "old-attached"],
        )?;
        // Keine Sperre gehalten: der Schreiber hat sie nach seinem letzten
        // Schreibvorgang (`TranscriptStore::append`) wieder freigegeben.
        let lock = sessions.join("old-attached.jsonl.lock");
        std::fs::write(&lock, b"").map_err(ctx("create released transcript lock"))?;
        let config = RetentionConfig {
            session_transcripts: transcripts_enabled(),
            ..RetentionConfig::default()
        };
        let mut spec = RetentionSweepSpec::new(home.path().to_path_buf(), None, config);
        spec.classes = vec!["session_transcripts".to_owned()];
        let attached = |session: &str| session == "old-attached";

        let dry = run_with_attachments(&spec, &attached)?;
        assert_eq!(text(&dry, "/mode")?, "dry_run");
        assert_eq!(number(&dry, "/classes/0/dirs/0/removed")?, 1);
        assert_eq!(number(&dry, "/classes/0/dirs/0/protected")?, 1);
        assert!(
            sessions.join("old-gone.jsonl").exists(),
            "a dry run deletes nothing"
        );

        spec.apply = true;
        let value = run_with_attachments(&spec, &attached)?;
        assert_eq!(text(&value, "/mode")?, "apply");
        assert!(
            field(&value, "/classes/0/skipped").is_err(),
            "with an attachment source the class runs"
        );
        assert_eq!(number(&value, "/classes/0/dirs/0/removed")?, 1);
        assert_eq!(
            number(&value, "/classes/0/dirs/0/protected")?,
            1,
            "the attached session"
        );
        assert_eq!(number(&value, "/errors")?, 0);
        assert_eq!(number(&value, "/failed_dirs")?, 0);
        assert!(
            sessions.join("old-attached.jsonl").exists(),
            "an attached session keeps its transcript"
        );
        assert!(!sessions.join("old-gone.jsonl").exists());
        assert!(sessions.join("fresh.jsonl").exists());
        Ok(())
    }

    /// Review 4505340178: ohne autoritative Anhänge-Quelle (der öffentliche
    /// Einstieg, also jeder `harw cleanup`-Job heute) wird
    /// `session_transcripts` trotz Opt-in in beiden Modi mit dem
    /// inhaltsfreien Grund übersprungen, ohne den Store aufzurufen; kein
    /// Transkript fällt, und die übrigen Klassen laufen weiter.
    #[test]
    fn session_transcripts_without_an_attachment_source_are_skipped_in_both_modes() -> TestResult {
        let (home, mut spec) = setup()?;
        let sessions = sessions_with(&home, &["old-a", "old-b", "fresh"], &["old-a", "old-b"])?;
        spec.config.session_transcripts = transcripts_enabled();
        spec.classes = vec!["tui_log".to_owned(), "session_transcripts".to_owned()];
        for apply in [false, true] {
            spec.apply = apply;
            let value = run(&spec)?;
            let transcripts = class_entry(&value, "session_transcripts")?;
            assert_eq!(text(transcripts, "/skipped")?, NO_ATTACHMENT_SOURCE);
            assert!(
                field(transcripts, "/dirs").is_err(),
                "the store is not called for transcripts"
            );
            let logs = class_entry(&value, "tui_log")?;
            assert_eq!(number(logs, "/dirs/0/removed")?, 3, "tui_log still runs");
            assert_eq!(number(&value, "/removed")?, 3, "only the logs count");
            assert_eq!(number(&value, "/errors")?, 0, "a skip is no error");
            assert_eq!(number(&value, "/failed_dirs")?, 0);
            assert_eq!(count_logs(&home)?, if apply { 1 } else { 4 });
            for id in ["old-a", "old-b", "fresh"] {
                assert!(sessions.join(format!("{id}.jsonl")).exists(), "{id}");
            }
        }
        Ok(())
    }

    /// Review 4505256692: eine abgelehnte Verdict-Retention (ein zweiter
    /// Schreiber hält die Sperre länger als die Wartezeit des Stores)
    /// verschwindet nicht hinter einem erfolgreichen Job: `failed_dirs` oben
    /// zählt sie; nichts wurde gelöscht oder neu verankert.
    #[test]
    fn a_refused_dod_verdicts_apply_is_counted_at_the_top_level() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temp home"))?;
        let store = verdict_store(home.path())?;
        let before = verdict_segments(&store)?;
        // flock gilt je offener Dateibeschreibung: ein eigener Handle sperrt
        // auch im selben Prozess gegen den Store.
        let holder = std::fs::File::options()
            .write(true)
            .open(store.root().join(".lock"))
            .map_err(ctx("open verdict lock"))?;
        holder.try_lock().map_err(ctx("hold verdict lock"))?;
        let result = run(&verdict_spec(home.path(), verdicts_enabled(), true));
        holder.unlock().map_err(ctx("release verdict lock"))?;
        let value = result?;

        assert_eq!(number(&value, "/failed_dirs")?, 1, "the refused store");
        assert_eq!(number(&value, "/removed_segments")?, 0);
        assert_eq!(number(&value, "/removed_records")?, 0);
        let error = text(&value, "/classes/0/dirs/0/error")?;
        assert!(error.contains("lock is contended"), "{error}");
        assert!(!error.contains("reason"), "content-free: {error}");
        assert_eq!(verdict_segments(&store)?, before, "nothing was deleted");
        assert!(!store.root().join("anchor.json").exists());
        Ok(())
    }

    /// Ein abgebrochener Job mit `dod_verdicts` endet als `Cancelled`, ohne
    /// den Store anzufassen: alle Segmente bleiben, kein Anker entsteht.
    #[test]
    fn a_cancelled_dod_verdicts_apply_leaves_the_chain_untouched() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temp home"))?;
        let store = verdict_store(home.path())?;
        let before = verdict_segments(&store)?;
        let cancel = SweepCancel::new();
        cancel.cancel();

        let outcome = execute_retention_sweep_with_cancel(
            &verdict_spec(home.path(), verdicts_enabled(), true),
            &cancel,
        );
        assert!(
            matches!(outcome, Err(MaintenanceFailure::Cancelled(_))),
            "cancelled"
        );
        assert_eq!(verdict_segments(&store)?, before, "nothing was deleted");
        assert!(!store.root().join("anchor.json").exists());
        assert_eq!(
            store.verify_chain().map_err(ctx("verify chain"))?.records,
            3
        );
        Ok(())
    }

    /// Die Frist gilt für `dod_verdicts` wie für generische Klassen: der Job
    /// prüft sie vor der Klasse, der Store vor seinem gesperrten Schritt;
    /// beides endet als `timed_out`, ohne Segment oder Anker anzufassen.
    #[test]
    fn an_expired_deadline_leaves_the_verdict_chain_untouched() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temp home"))?;
        let store = verdict_store(home.path())?;
        let before = verdict_segments(&store)?;
        let spec = verdict_spec(home.path(), verdicts_enabled(), true);

        // Job level: checked before the class starts.
        let outcome = execute_until(&spec, &SweepCancel::new(), Instant::now(), None);
        assert!(
            matches!(outcome, Err(MaintenanceFailure::TimedOut(_))),
            "timed out"
        );

        // Store level: the class runs, the store sees the expired deadline.
        let class = resolve_all(&spec.config)
            .into_iter()
            .find(|class| class.class.id == "dod_verdicts")
            .ok_or(TestError::Missing("dod_verdicts class"))?;
        let roots = Roots {
            home: spec.home.clone(),
            project: None,
        };
        let mut totals = SweepCounts::default();
        let run = StoreRun {
            mode: SweepMode::Apply,
            deadline: Instant::now(),
            attached: None,
        };
        let swept = sweep_store_class(
            &spec.config,
            &class,
            StoreClass::DodVerdicts,
            &roots,
            run,
            &mut totals,
        )
        .map_err(ctx("sweep the verdict class"))?;
        let ClassSweep::Dirs(dirs) = swept else {
            return Err(TestError::Unexpected(
                "the verdict class is never skipped for a missing attachment source".to_owned(),
            ));
        };
        assert!(totals.timed_out, "the job would end as timed_out");
        assert_eq!(totals.removed_segments, 0);
        assert_eq!(totals.failed_dirs, 0);
        let entry = dirs
            .first()
            .ok_or(TestError::Missing("verdict store entry"))?;
        assert_eq!(
            entry.get("timed_out").and_then(serde_json::Value::as_bool),
            Some(true)
        );

        assert_eq!(verdict_segments(&store)?, before, "nothing was deleted");
        assert!(!store.root().join("anchor.json").exists());
        Ok(())
    }

    // Der `.resolved.json`-Record des Befunds `finding` unter dem Schlüssel
    // `cg-1.<finding>.<frozen_at>`.
    fn resolved_freeze_record(store: &FreezeStore, finding: &str) -> TestResult<PathBuf> {
        let prefix = format!("cg-1.{finding}.");
        std::fs::read_dir(store.root())
            .map_err(ctx("list freeze store"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with(prefix.as_str()) && name.ends_with(".resolved.json")
                    })
            })
            .ok_or(TestError::Missing("resolved freeze record"))
    }

    /// `freeze_resolved` läuft über den Freeze-Store mit der Home als Wurzel
    /// (Verzeichnis `<home>/freeze`, nicht `<home>/freeze/freeze`): ein alter
    /// Record fällt, einer mit wieder vorhandenem `.active.json` bleibt
    /// geschützt (sein Löschen ließe den Freeze wieder aufleben).
    #[test]
    fn freeze_resolved_routes_through_the_store_with_the_home_as_root() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temp home"))?;
        let store = FreezeStore::new(home.path());
        let now = Timestamp::now();
        let cgroup = CgroupId::try_from_str("cg-1").map_err(ctx("cgroup id"))?;
        let mut records = Vec::new();
        for (finding, second) in [("f-old", 1_000), ("f-reopened", 2_000), ("f-fresh", 3_000)] {
            let record = Freeze {
                cgroup: cgroup.clone(),
                finding: FindingId::try_from_str(finding).map_err(ctx("finding id"))?,
                frozen_at: Timestamp::from_second(second).map_err(ctx("frozen_at"))?,
                expires_at: None,
            };
            store.freeze(&record).map_err(ctx("freeze"))?;
            store
                .resolve(&record.cgroup, &record.finding, record.frozen_at, now)
                .map_err(ctx("resolve"))?;
            records.push(record);
        }
        let old = resolved_freeze_record(&store, "f-old")?;
        let reopened = resolved_freeze_record(&store, "f-reopened")?;
        let fresh = resolved_freeze_record(&store, "f-fresh")?;
        age(&old, 400)?;
        age(&reopened, 400)?;
        // Crash simulation: the reopened key has its `.active.json` again.
        let reopened_name = reopened
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(TestError::Missing("resolved record name"))?;
        let active =
            reopened.with_file_name(reopened_name.replace(".resolved.json", ".active.json"));
        let active_record = records
            .get(1)
            .ok_or(TestError::Missing("reopened freeze"))?;
        let active_bytes = serde_json::to_vec(active_record).map_err(ctx("encode freeze"))?;
        std::fs::write(&active, active_bytes).map_err(ctx("write active record"))?;

        let config = RetentionConfig {
            freeze_resolved: ClassConfig {
                enabled: Some(true),
                ..ClassConfig::default()
            },
            ..RetentionConfig::default()
        };
        let mut spec = RetentionSweepSpec::new(home.path().to_path_buf(), None, config);
        spec.classes = vec!["freeze_resolved".to_owned()];
        spec.apply = true;
        let value = run(&spec)?;

        assert_eq!(
            Some(text(&value, "/classes/0/dirs/0/dir")?),
            store.root().to_str(),
            "the store below the home, not <home>/freeze/freeze"
        );
        assert_eq!(number(&value, "/classes/0/dirs/0/removed")?, 1);
        assert_eq!(
            number(&value, "/classes/0/dirs/0/protected")?,
            1,
            "the record whose active freeze is back"
        );
        assert_eq!(number(&value, "/errors")?, 0);
        assert_eq!(number(&value, "/failed_dirs")?, 0);
        assert!(!old.exists(), "the old resolved record is gone");
        assert!(reopened.exists(), "never resurrect a freeze");
        assert!(active.exists(), "an active record is never touched");
        assert!(fresh.exists(), "the fresh record stays");
        Ok(())
    }

    /// `session_corrupt_backups` läuft über den Store im Sessions-Verzeichnis
    /// des Profils: eine alte Quarantäne-Datei fällt, das Transkript daneben
    /// bleibt (eine andere Klasse), die jüngste Kopie bleibt.
    #[test]
    fn session_corrupt_backups_route_through_the_store_and_spare_transcripts() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temp home"))?;
        let sessions = home
            .path()
            .join("profiles")
            .join("default")
            .join("sessions");
        std::fs::create_dir_all(&sessions).map_err(ctx("create sessions dir"))?;
        let transcript = sessions.join("s1.jsonl");
        let old = sessions.join("s1.jsonl.corrupt-100");
        let recent = sessions.join("s1.jsonl.corrupt-200");
        for path in [&transcript, &old, &recent] {
            std::fs::write(path, b"{}\n").map_err(ctx("write session file"))?;
        }
        age(&transcript, 400)?;
        age(&old, 100)?;

        let config = RetentionConfig {
            session_corrupt_backups: ClassConfig {
                enabled: Some(true),
                ..ClassConfig::default()
            },
            ..RetentionConfig::default()
        };
        let mut spec = RetentionSweepSpec::new(home.path().to_path_buf(), None, config);
        spec.classes = vec!["session_corrupt_backups".to_owned()];
        spec.apply = true;
        let value = run(&spec)?;

        assert_eq!(
            Some(text(&value, "/classes/0/dirs/0/dir")?),
            sessions.to_str()
        );
        assert_eq!(number(&value, "/classes/0/dirs/0/removed")?, 1);
        assert_eq!(number(&value, "/classes/0/dirs/0/protected")?, 0);
        assert_eq!(number(&value, "/failed_dirs")?, 0);
        assert!(!old.exists(), "the aged backup is gone");
        assert!(recent.exists(), "the newest backup stays");
        assert!(transcript.exists(), "a transcript is never a backup");
        Ok(())
    }
}
