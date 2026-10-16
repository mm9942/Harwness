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
//! übersprungen gemeldet; der Probelauf meldet immer nur.
//!
//! # Frist und Abbruch
//! Die Frist stammt aus der Payload und beginnt mit der Ausführung. Ablauf und
//! Operator-Abbruch sind unterscheidbar (`timed_out` bzw. `cancelled`); jeder
//! einzelne Löschvorgang ist unabhängig und idempotent, ein Abbruch lässt also
//! keinen halben Zustand zurück. Der Abbruch wird zwischen Klassen geprüft; die
//! Frist zusätzlich vor jedem Löschen (in `harw-retention`).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use harw_job_runtime::{
    Budget, Job, JobKind, JobOutcome, JobScope, RetryPolicy, StoredJob, WorkId,
};
use harw_operations::{OpContext, OpError, OpOutput};
use harw_retention::{RetentionConfig, RetentionError, Roots, SweepMode, resolve_all};
use harw_session_store::JobStore;
use harw_types::{ApprovalActor, Principal};
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
/// # Errors
/// [`MaintenanceFailure`]: ungültige Payload (`Failed`), Abbruch
/// (`Cancelled`) oder Fristablauf (`TimedOut`). Bereits erfolgte Löschungen
/// bleiben gültig (jede ist unabhängig und idempotent).
pub fn execute_retention_sweep_with_cancel(
    spec: &RetentionSweepSpec,
    cancel: &SweepCancel,
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

    let deadline = Instant::now() + Duration::from_secs(spec.deadline_secs);
    let mode = if spec.apply {
        SweepMode::Apply
    } else {
        SweepMode::DryRun
    };
    let roots = Roots {
        home: spec.home.clone(),
        project: spec.project.clone(),
    };
    let mut classes = Vec::new();
    let mut removed = 0_usize;
    let mut removed_bytes = 0_u64;
    let mut timed_out = false;
    for class in &selected {
        if cancel.is_cancelled() {
            return Err(MaintenanceFailure::Cancelled(
                "retention sweep cancelled between classes".to_owned(),
            ));
        }
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        let id = class.class.id;
        let kind = class.class.kind.as_str();
        match class.sweep(&roots, mode, Some(deadline), SystemTime::now()) {
            Ok(outcomes) => {
                let mut dirs = Vec::new();
                for outcome in outcomes {
                    match outcome.result {
                        Ok(report) => {
                            removed += report.removed.len();
                            removed_bytes += report.removed_bytes();
                            timed_out |= report.timed_out;
                            dirs.push(serde_json::json!({
                                "dir": outcome.dir,
                                "removed": report.removed.len(),
                                "removed_bytes": report.removed_bytes(),
                                "kept": report.kept,
                                "skipped": report.skipped,
                                "errors": report.errors.len(),
                                "timed_out": report.timed_out,
                            }));
                        }
                        Err(error) => dirs.push(serde_json::json!({
                            "dir": outcome.dir,
                            "error": error.to_string(),
                        })),
                    }
                }
                classes.push(serde_json::json!({"id": id, "kind": kind, "dirs": dirs}));
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
    if timed_out {
        return Err(MaintenanceFailure::TimedOut(
            "the retention sweep did not finish in time; the rest stays untouched".to_owned(),
        ));
    }
    Ok(serde_json::json!({
        "mode": if spec.apply { "apply" } else { "dry_run" },
        "removed": removed,
        "removed_bytes": removed_bytes,
        "classes": classes,
    }))
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
    use harw_retention::ClassConfig;

    type TestError = Box<dyn std::error::Error>;
    type TestResult<T = ()> = Result<T, TestError>;

    fn ensure(condition: bool, what: &str) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(what.to_owned().into())
        }
    }

    /// `<home>/logs` mit vier `tui.log.N`-Dateien; Klasse `tui_log` auf
    /// `max_files = 1` begrenzt, damit drei fallen würden.
    fn setup() -> TestResult<(tempfile::TempDir, RetentionSweepSpec)> {
        let home = tempfile::tempdir()?;
        let logs = home.path().join("logs");
        std::fs::create_dir_all(&logs)?;
        for n in 1..=4 {
            std::fs::write(logs.join(format!("tui.log.{n}")), b"x")?;
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
        Ok(std::fs::read_dir(home.path().join("logs"))?.count())
    }

    #[test]
    fn dry_run_reports_but_deletes_nothing() -> TestResult {
        let (home, spec) = setup()?;
        let value = execute_retention_sweep_with_cancel(&spec, &SweepCancel::new())
            .map_err(|failure| failure.reason())?;
        ensure(value["mode"] == "dry_run", "dry run mode")?;
        ensure(value["removed"] == 3, "three files would fall")?;
        ensure(count_logs(&home)? == 4, "nothing deleted in a dry run")
    }

    #[test]
    fn apply_deletes_only_what_the_policy_selects() -> TestResult {
        let (home, mut spec) = setup()?;
        spec.apply = true;
        let value = execute_retention_sweep_with_cancel(&spec, &SweepCancel::new())
            .map_err(|failure| failure.reason())?;
        ensure(value["mode"] == "apply", "apply mode")?;
        ensure(count_logs(&home)? == 1, "one file kept")
    }

    #[test]
    fn security_classes_are_skipped_without_opt_in() -> TestResult {
        let home = tempfile::tempdir()?;
        let mut spec =
            RetentionSweepSpec::new(home.path().to_path_buf(), None, RetentionConfig::default());
        spec.apply = true;
        spec.classes = vec!["session_transcripts".to_owned()];
        let value = execute_retention_sweep_with_cancel(&spec, &SweepCancel::new())
            .map_err(|failure| failure.reason())?;
        ensure(
            value["classes"][0]["skipped"].is_string(),
            "security class is reported as skipped",
        )?;
        ensure(value["removed"] == 0, "nothing removed")
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
        let value = serde_json::to_value(&spec)?;
        let back: RetentionSweepSpec = serde_json::from_value(value.clone())?;
        ensure(back == spec, "round trip")?;
        let mut tampered = value;
        tampered["extra"] = serde_json::json!(1);
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
}
