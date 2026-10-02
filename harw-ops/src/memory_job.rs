//! `memory_maintenance` — Gedächtnis-Wartung als Job der Job-Infrastruktur.
//!
//! # Verantwortungsbereich
//! Konsolidieren, Vergessen, Promotion nach Global und der Startup-Sweep laufen
//! nie im Aufrufer-Pfad, sondern als dauerhafter Job der Art
//! [`MEMORY_MAINTENANCE_JOB_KIND`] (`JobKind::Custom`) im [`JobStore`]. Dieses
//! Modul enthält die drei Teile, die alle Verbraucher teilen:
//!
//! - [`MemoryMaintenanceSpec`] — die Job-Payload (Operation, aufgelöste
//!   absolute Wurzeln, Frist in Sekunden);
//! - [`admit_memory_maintenance`] — legt den `Ready`-Job an und gibt dessen
//!   Id sofort zurück (der `/memory`-Befehl und der Startup-Sweep der
//!   Runtime nutzen genau diese Funktion);
//! - [`execute_memory_maintenance`] — der Handler-Kern, den der Job-Worker
//!   (`harw-cli`) und der In-Prozess-Sweep der Runtime aufrufen. Er ruft die
//!   fristgebundenen Operationen von `harw-memory` auf (die ihren
//!   `ConsolidationLock` selbst halten) und meldet Fristablauf als
//!   [`MaintenanceFailure::TimedOut`].
//!
//! # Frist
//! Die Frist stammt aus der Job-Payload (`deadline_secs`, Vorgaben aus
//! `[memory]`: 30 s Konsolidieren/Vergessen/Promotion, 120 s Sweep) und
//! beginnt mit der Ausführung, nicht mit dem Einreihen. Ablauf heißt Abbruch
//! ohne Teilzustand (die Operationen prüfen vor ihrem Commit-Punkt); der Job
//! wird als `Failed` mit Grund `timed_out: …` abgeschlossen.
//!
//! # Fristablauf und Abbruch
//! Fristablauf ist ein typisierter Grund auf `Failed`
//! ([`harw_job_runtime::JobOutcome::timed_out`], Disposition `timed_out`;
//! die Drahtform bleibt `failed` + `timed_out: …`, siehe
//! [`harw_job_runtime::TIMED_OUT_REASON_PREFIX`]). Ein Operator-Abbruch ist
//! davon unterscheidbar: [`MaintenanceFailure::Cancelled`] →
//! `JobOutcome::Cancelled`. Der Abbruch ist kooperativ: ein
//! [`harw_memory::consolidation::CancelFlag`] hängt an der `Deadline` und wird
//! an jeder Prüfstelle zwischen den Schritten gelesen
//! ([`execute_memory_maintenance_with_cancel`]); [`run_memory_maintenance_job`]
//! ist der gemeinsame asynchrone Treiber (Worker und In-Prozess-Ausführer).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use harw_job_runtime::{
    Budget, Job, JobKind, JobOutcome, JobScope, JobState, RetryPolicy, StoredJob, WorkId,
};
use harw_memory::capture::consolidate_memories_with_options;
use harw_memory::consolidation::{CancelFlag, ConsolidationError, Deadline};
use harw_memory::promote::{GlobalPromotionError, promote_fact_to_global};
use harw_memory::{FactScope, FactStore};
use harw_operations::{OpContext, OpError, OpOutput};
use harw_session_store::JobStore;
use harw_types::{ApprovalActor, Principal};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

/// [`JobKind::Custom`]-Name der Gedächtnis-Wartungsjobs.
pub const MEMORY_MAINTENANCE_JOB_KIND: &str = "memory_maintenance";
/// Schema-Version der Payload [`MemoryMaintenanceSpec`].
pub const MEMORY_MAINTENANCE_SCHEMA_VERSION: u32 = 1;
/// Präfix des Fehlergrunds bei Fristablauf (siehe Moduldoku).
pub const TIMED_OUT_REASON_PREFIX: &str = harw_job_runtime::TIMED_OUT_REASON_PREFIX;
/// Höchstzahl Ausführungsversuche: eine Wartung wird nie automatisch
/// wiederholt (der Aufrufer reiht sie bei Bedarf neu ein).
const MAINTENANCE_MAX_ATTEMPTS: u32 = 1;

/// Betroffene Fakt-Wurzel einer Konsolidierung.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceScope {
    /// `<projekt>/.harw/memories`.
    Project,
    /// `<profil>/memories`.
    Global,
}

impl MaintenanceScope {
    fn fact_scope(self) -> FactScope {
        match self {
            Self::Project => FactScope::Project,
            Self::Global => FactScope::Global,
        }
    }
}

/// Die Wartungsoperation eines Jobs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryMaintenanceOp {
    /// Konsolidierung (`consolidate_memories_with_options`) einer Wurzel.
    Consolidate {
        /// Zu konsolidierende Wurzel.
        scope: MaintenanceScope,
    },
    /// Startup-Sweep: Konsolidierung der Projekt-Wurzel, liegengebliebene
    /// `_incoming`-Kandidaten nach einem Absturz nachholen.
    Sweep,
    /// Fakt `name` aus Projekt und Global löschen, wo er existiert.
    Forget {
        /// Fakt-Name.
        name: String,
    },
    /// Projekt-Fakt `name` in den globalen Scope kopieren
    /// (`promote_fact_to_global`).
    PromoteToGlobal {
        /// Fakt-Name.
        name: String,
    },
}

impl MemoryMaintenanceOp {
    /// Kurzname für Berichte und Logs.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Consolidate { .. } => "consolidate",
            Self::Sweep => "sweep",
            Self::Forget { .. } => "forget",
            Self::PromoteToGlobal { .. } => "promote_to_global",
        }
    }
}

/// Payload eines `memory_maintenance`-Jobs.
///
/// # Beschreibung
/// Alle Pfade sind beim Einreihen serverseitig aufgelöste, absolute
/// Fakt-Wurzeln; der Job-Worker kennt weder Projekt-Erkennung noch Sitzung.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryMaintenanceSpec {
    /// Siehe [`MEMORY_MAINTENANCE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Die Operation.
    pub operation: MemoryMaintenanceOp,
    /// Projekt-Fakt-Wurzel (`…/.harw/memories`), falls aufgelöst.
    pub project_root: Option<PathBuf>,
    /// Globale Fakt-Wurzel (`<profil>/memories`), falls aufgelöst.
    pub global_root: Option<PathBuf>,
    /// Herkunftsbezeichnung für `promoted_from:` (Projektname).
    pub project_label: String,
    /// Frist der Ausführung in Sekunden (aus `[memory]`).
    pub deadline_secs: u64,
    /// Verfallsfenster der Konsolidierung (`[memory] max_unused_days`).
    pub max_unused_days: i64,
}

impl MemoryMaintenanceSpec {
    /// Spezifikation mit den Vorgaben für Label und Verfallsfenster.
    #[must_use]
    pub fn new(op: MemoryMaintenanceOp, deadline_secs: u64) -> Self {
        Self {
            schema_version: MEMORY_MAINTENANCE_SCHEMA_VERSION,
            operation: op,
            project_root: None,
            global_root: None,
            project_label: "project".to_owned(),
            deadline_secs,
            max_unused_days: harw_memory::capture::DECAY_MAX_UNUSED_DAYS,
        }
    }
}

/// Grund, aus dem [`execute_memory_maintenance`] keinen Erfolg meldet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaintenanceFailure {
    /// Die Frist ist abgelaufen; der Bestand ist unverändert.
    TimedOut(String),
    /// Ein Operator hat abgebrochen; der Bestand ist unverändert.
    Cancelled(String),
    /// Jeder andere Fehlschlag (ungültige Payload, Lock, I/O, Ablehnung).
    Failed(String),
}

impl MaintenanceFailure {
    /// Der Text für den Job-Ausgang; Fristablauf beginnt mit
    /// [`TIMED_OUT_REASON_PREFIX`].
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::TimedOut(detail) => format!("{TIMED_OUT_REASON_PREFIX}: {detail}"),
            Self::Cancelled(detail) | Self::Failed(detail) => detail.clone(),
        }
    }

    /// `true` bei Fristablauf.
    #[must_use]
    pub fn is_timed_out(&self) -> bool {
        matches!(self, Self::TimedOut(_))
    }

    /// `true` bei Abbruch.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled(_))
    }

    /// Der Job-Ausgang: `timed_out` (typisiert auf `Failed`), `Cancelled`
    /// oder `Failed`.
    #[must_use]
    pub fn into_outcome(self) -> JobOutcome {
        match self {
            Self::TimedOut(detail) => JobOutcome::timed_out(detail),
            Self::Cancelled(detail) => JobOutcome::Cancelled { reason: detail },
            Self::Failed(reason) => JobOutcome::Failed { reason },
        }
    }
}

/// `true` für `JobKind::Custom("memory_maintenance")`.
#[must_use]
pub fn is_memory_maintenance_kind(kind: &JobKind) -> bool {
    matches!(kind, JobKind::Custom(name) if name == MEMORY_MAINTENANCE_JOB_KIND)
}

/// Legt einen `Ready`-Job für `spec` im `jobs`-Store an und gibt seine Id
/// zurück. Blockiert nicht und führt nichts aus.
///
/// # Errors
/// Eine Beschreibung, wenn die Payload nicht serialisierbar ist, die Frist
/// `0` ist oder der Store den Job ablehnt.
pub fn admit_memory_maintenance(
    jobs: &JobStore,
    scope: JobScope,
    spec: &MemoryMaintenanceSpec,
) -> Result<WorkId, String> {
    if spec.deadline_secs == 0 {
        return Err("deadline_secs muss größer als 0 sein".to_owned());
    }
    let now = Timestamp::now();
    let wall = SignedDuration::from_secs(i64::try_from(spec.deadline_secs).unwrap_or(i64::MAX));
    let mut job = Job::new(
        WorkId::new(),
        JobKind::Custom(MEMORY_MAINTENANCE_JOB_KIND.to_owned()),
        Budget {
            max_tokens: None,
            max_wall: Some(wall),
            max_tool_calls: None,
        },
        RetryPolicy {
            max_attempts: MAINTENANCE_MAX_ATTEMPTS,
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

/// Reiht die Operation aus dem `/memory`-Befehl ein und kehrt sofort mit der
/// Job-Id zurück (nicht blockierend). Status und Abbruch laufen über die
/// vorhandenen Job-Werkzeuge.
///
/// # Errors
/// - [`OpError::NotAvailable`]: kein Job-Store im Kontext.
/// - [`OpError::Execution`]: der Store lehnt den Job ab.
pub(crate) fn enqueue_memory_maintenance(
    ctx: &OpContext,
    spec: &MemoryMaintenanceSpec,
) -> Result<OpOutput, OpError> {
    let jobs = ctx.service::<Arc<JobStore>>().ok_or_else(|| {
        OpError::NotAvailable("kein dauerhafter Job-Store konfiguriert".to_owned())
    })?;
    let submitter = ctx.service::<Principal>().map_or_else(
        || "memory-op".to_owned(),
        |principal| principal.id().to_owned(),
    );
    let binding = ctx.sandbox().workspace();
    let scope = JobScope::new(
        binding.tenant().clone(),
        binding.workspace().clone(),
        ApprovalActor::Operator { id: submitter },
    );
    let work_id = admit_memory_maintenance(jobs, scope, spec).map_err(|error| {
        OpError::Execution(format!(
            "Gedächtnis-Job konnte nicht eingereiht werden: {error}"
        ))
    })?;
    let label = spec.operation.label();
    Ok(OpOutput {
        text: format!(
            "Gedächtnis-Job {work_id} ({label}) eingereiht, Frist {} s. Status und Abbruch über \
             die Job-Werkzeuge (job status / job cancel).",
            spec.deadline_secs
        ),
        data: Some(serde_json::json!({
            "job_id": work_id.as_str(),
            "kind": MEMORY_MAINTENANCE_JOB_KIND,
            "op": label,
            "deadline_secs": spec.deadline_secs,
        })),
    })
}

/// Führt die Wartungsoperation synchron bis zum Ende oder bis zur Frist aus.
///
/// # Beschreibung
/// Reiner Handler-Kern: blockierend (Dateisystem, Lock-Wartezeit bis zur
/// Frist) — der Aufrufer (Job-Worker) führt ihn in `spawn_blocking` bzw. auf
/// einem eigenen Thread aus. Die Frist beginnt hier. Jeder Fehler, der nach
/// abgelaufener Frist auftritt, zählt als [`MaintenanceFailure::TimedOut`]
/// (die zugrunde liegenden Operationen brechen vor ihrem Commit-Punkt ab und
/// lassen den Bestand unverändert).
///
/// # Errors
/// [`MaintenanceFailure`].
pub fn execute_memory_maintenance(
    spec: &MemoryMaintenanceSpec,
) -> Result<serde_json::Value, MaintenanceFailure> {
    execute_memory_maintenance_with_cancel(spec, &CancelFlag::new())
}

/// Wie [`execute_memory_maintenance`], zusätzlich kooperativ abbrechbar:
/// `cancel` wird zwischen den Schritten (und beim Warten auf den Lock)
/// geprüft. Ein Abbruch endet als [`MaintenanceFailure::Cancelled`], ein
/// Fristablauf als [`MaintenanceFailure::TimedOut`]; beide lassen den
/// Bestand unverändert (Abbruch gewinnt, wenn beides zutrifft).
///
/// # Errors
/// [`MaintenanceFailure`].
pub fn execute_memory_maintenance_with_cancel(
    spec: &MemoryMaintenanceSpec,
    cancel: &CancelFlag,
) -> Result<serde_json::Value, MaintenanceFailure> {
    if spec.schema_version != MEMORY_MAINTENANCE_SCHEMA_VERSION {
        return Err(MaintenanceFailure::Failed(format!(
            "unbekannte Payload-Version {}",
            spec.schema_version
        )));
    }
    for root in [&spec.project_root, &spec.global_root]
        .into_iter()
        .flatten()
    {
        if !root.is_absolute() {
            return Err(MaintenanceFailure::Failed(
                "Fakt-Wurzeln müssen absolute Pfade sein".to_owned(),
            ));
        }
    }
    if spec.deadline_secs == 0 {
        return Err(MaintenanceFailure::Failed(
            "deadline_secs muss größer als 0 sein".to_owned(),
        ));
    }
    let deadline =
        Deadline::after(Duration::from_secs(spec.deadline_secs)).with_cancel(cancel.clone());
    let result = run_op(spec, deadline.clone());
    match result {
        Err(MaintenanceFailure::Failed(detail)) if deadline.cancelled() => {
            Err(MaintenanceFailure::Cancelled(detail))
        }
        Err(MaintenanceFailure::Failed(detail)) if deadline.expired() => {
            Err(MaintenanceFailure::TimedOut(detail))
        }
        other => other,
    }
}

/// Wie oft der Treiber den Job-Store auf einen Abbruch abfragt.
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(150);
/// Zusätzliche Wartezeit über die Frist hinaus, bevor ein hängender
/// blockierender Thread als zeitüberschritten aufgegeben wird.
const BLOCKING_GRACE: Duration = Duration::from_secs(5);

/// Ob der Store den Job als abgebrochen führt (Abbruch vor/während des Laufs).
fn store_says_cancelled(store: &JobStore, work_id: &WorkId) -> bool {
    store
        .get(work_id)
        .is_ok_and(|record| record.job.state == JobState::Cancelled)
}

/// Gemeinsamer asynchroner Treiber der Wartung: Worker (`harw serve`) und
/// In-Prozess-Ausführer der Runtime rufen genau diesen Pfad auf, beide unter
/// dem `DurableJobRunner` (ein Claim, Lease, Heartbeat, Commit).
///
/// # Beschreibung
/// Der Handler-Kern läuft in `spawn_blocking`. Ein Abbruch erreicht ihn auf
/// zwei Wegen, die beide dasselbe [`CancelFlag`] setzen: das Signal
/// `cancel_signal` (Lease-Verlust/Registry-Abbruch des Runners) und eine
/// Abfrage des Job-Stores alle 150 ms (ein Operator-`cancel` setzt den
/// Zustand `Cancelled`, bevor der Heartbeat es bemerkt). Der Kern prüft das
/// Flag zwischen den Schritten und bricht ohne Teilzustand ab.
///
/// # Rückgabe
/// `Succeeded`, `Cancelled` (Abbruch), `Failed` mit `timed_out: …`
/// (Fristablauf, auch wenn der blockierende Thread die Frist um
/// [`BLOCKING_GRACE`] überschreitet) oder `Failed`.
pub async fn run_memory_maintenance_job<F>(
    spec: MemoryMaintenanceSpec,
    work_id: WorkId,
    store: Arc<JobStore>,
    cancel_signal: F,
) -> JobOutcome
where
    F: std::future::Future<Output = ()> + Send,
{
    if store_says_cancelled(&store, &work_id) {
        return JobOutcome::Cancelled {
            reason: "cancelled before the memory maintenance started".to_owned(),
        };
    }
    let flag = CancelFlag::new();
    let wait = Duration::from_secs(spec.deadline_secs).saturating_add(BLOCKING_GRACE);
    let label = spec.operation.label();
    let thread_flag = flag.clone();
    let blocking = tokio::task::spawn_blocking(move || {
        execute_memory_maintenance_with_cancel(&spec, &thread_flag)
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
                if store_says_cancelled(&store, &work_id) {
                    flag.cancel();
                }
            }
            () = &mut hard_stop => break None,
        }
    };
    match joined {
        Some(Ok(Ok(result))) => JobOutcome::Succeeded { result },
        Some(Ok(Err(failure))) => {
            tracing::warn!(work_id = %work_id.as_str(), op = label, reason = %failure.reason(), "memory maintenance ended without success");
            failure.into_outcome()
        }
        Some(Err(join_error)) => {
            tracing::warn!(work_id = %work_id.as_str(), op = label, error = %join_error, "memory maintenance task failed");
            JobOutcome::Failed {
                reason: "memory maintenance task failed".to_owned(),
            }
        }
        None => {
            flag.cancel();
            MaintenanceFailure::TimedOut("the maintenance did not finish in time".to_owned())
                .into_outcome()
        }
    }
}

fn run_op(
    spec: &MemoryMaintenanceSpec,
    deadline: Deadline,
) -> Result<serde_json::Value, MaintenanceFailure> {
    match &spec.operation {
        MemoryMaintenanceOp::Consolidate { scope } => {
            let root = match scope {
                MaintenanceScope::Project => spec.project_root.as_deref(),
                MaintenanceScope::Global => spec.global_root.as_deref(),
            };
            consolidate(root, scope.fact_scope(), spec, deadline)
        }
        MemoryMaintenanceOp::Sweep => consolidate(
            spec.project_root.as_deref(),
            FactScope::Project,
            spec,
            deadline,
        ),
        MemoryMaintenanceOp::Forget { name } => {
            let output = crate::memory::forget_with_deadline(
                spec.project_root.clone(),
                spec.global_root.clone(),
                name,
                deadline.clone(),
            )
            .map_err(|error| MaintenanceFailure::Failed(error.to_string()))?;
            Ok(serde_json::json!({ "text": output.text }))
        }
        MemoryMaintenanceOp::PromoteToGlobal { name } => {
            let (Some(project_root), Some(global_root)) =
                (spec.project_root.as_deref(), spec.global_root.as_deref())
            else {
                return Err(MaintenanceFailure::Failed(
                    "Projekt- und globale Wurzel werden für die Promotion benötigt".to_owned(),
                ));
            };
            let store = FactStore::open(project_root, FactScope::Project)
                .map_err(|error| MaintenanceFailure::Failed(error.to_string()))?;
            let done = promote_fact_to_global(
                &store,
                global_root,
                name,
                &spec.project_label,
                time::OffsetDateTime::now_utc(),
                deadline,
            )
            .map_err(|error| match error {
                GlobalPromotionError::Deadline(detail) => interrupted(&detail),
                other => MaintenanceFailure::Failed(other.to_string()),
            })?;
            Ok(serde_json::json!({
                "fact": done.fact.name,
                "already_present": done.already_present,
                "sources": done.fact.sources,
            }))
        }
    }
}

// Abbruch oder Fristablauf einer Prüfstelle als typisierter Grund.
fn interrupted(detail: &harw_memory::consolidation::DeadlineExceeded) -> MaintenanceFailure {
    if detail.cancelled {
        MaintenanceFailure::Cancelled(detail.to_string())
    } else {
        MaintenanceFailure::TimedOut(detail.to_string())
    }
}

fn consolidate(
    root: Option<&Path>,
    scope: FactScope,
    spec: &MemoryMaintenanceSpec,
    deadline: Deadline,
) -> Result<serde_json::Value, MaintenanceFailure> {
    let Some(root) = root else {
        return Err(MaintenanceFailure::Failed(format!(
            "keine Fakt-Wurzel für den Scope {scope} aufgelöst"
        )));
    };
    consolidate_memories_with_options(root, scope, deadline, spec.max_unused_days)
        .map(|report| {
            serde_json::json!({
                "merged": report.merged,
                "written": report.written,
                "deleted": report.deleted,
                "conflicts": report.conflicts,
            })
        })
        .map_err(|error| match error {
            ConsolidationError::Deadline(detail) => interrupted(&detail),
            other => MaintenanceFailure::Failed(other.to_string()),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_job_runtime::JobState;
    use harw_memory::{Fact, FactType};
    use harw_operations::context::ServiceMap;
    use harw_types::{TenantId, WorkspaceId};

    fn fact(name: &str, scope: FactScope) -> Fact {
        let now = time::OffsetDateTime::now_utc();
        Fact {
            name: name.to_owned(),
            description: "d".to_owned(),
            fact_type: FactType::Fact,
            scope,
            created: now,
            updated: now,
            confidence: 1.0,
            sources: Vec::new(),
            tags: Vec::new(),
            body: "inhalt".to_owned(),
        }
    }

    fn scope() -> JobScope {
        JobScope::new(
            TenantId::from_str("test-tenant"),
            WorkspaceId::from_str("ws"),
            ApprovalActor::Operator {
                id: "tester".to_owned(),
            },
        )
    }

    fn spec_with_roots(
        op: MemoryMaintenanceOp,
        project: &Path,
        global: &Path,
        deadline_secs: u64,
    ) -> MemoryMaintenanceSpec {
        let mut spec = MemoryMaintenanceSpec::new(op, deadline_secs);
        spec.project_root = Some(project.to_path_buf());
        spec.global_root = Some(global.to_path_buf());
        spec
    }

    #[test]
    fn enqueue_returns_the_job_id_immediately_and_leaves_the_job_ready() -> TestResult {
        let state = tempfile::tempdir().map_err(ctx("state"))?;
        let project = tempfile::tempdir().map_err(ctx("project"))?;
        let global = tempfile::tempdir().map_err(ctx("global"))?;
        let store = FactStore::open(project.path(), FactScope::Project).map_err(ctx("open"))?;
        store
            .write(&fact("kurz", FactScope::Project))
            .map_err(ctx("seed"))?;
        let jobs = Arc::new(JobStore::new(state.path()));
        let mut services = ServiceMap::new();
        services.insert(Arc::clone(&jobs));
        let context = crate::knowledge_test_support::op_context(services)?;

        let spec = spec_with_roots(
            MemoryMaintenanceOp::Forget {
                name: "kurz".to_owned(),
            },
            project.path(),
            global.path(),
            30,
        );
        let started = std::time::Instant::now();
        let out = enqueue_memory_maintenance(&context, &spec).map_err(ctx("enqueue"))?;
        assert!(started.elapsed() < Duration::from_secs(5));

        let data = out.data.ok_or(TestError::Missing("data"))?;
        let id = data["job_id"]
            .as_str()
            .ok_or(TestError::Missing("job_id"))?;
        assert_eq!(data["kind"], MEMORY_MAINTENANCE_JOB_KIND);
        let record = jobs
            .get(&WorkId::from_str(id))
            .map_err(ctx("get enqueued job"))?;
        assert_eq!(record.job.state, JobState::Ready);
        assert!(is_memory_maintenance_kind(&record.job.kind));
        // Nichts wurde ausgeführt: der Fakt existiert noch.
        assert!(store.read("kurz").map_err(ctx("read"))?.is_some());
        Ok(())
    }

    #[test]
    fn enqueue_without_a_job_store_is_not_available() -> TestResult {
        let context = crate::knowledge_test_support::op_context(ServiceMap::new())?;
        let spec = MemoryMaintenanceSpec::new(MemoryMaintenanceOp::Sweep, 30);
        let result = enqueue_memory_maintenance(&context, &spec);
        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn payload_round_trips_and_rejects_unknown_fields() -> TestResult {
        let spec = MemoryMaintenanceSpec::new(
            MemoryMaintenanceOp::Consolidate {
                scope: MaintenanceScope::Global,
            },
            30,
        );
        let value = serde_json::to_value(&spec).map_err(ctx("to_value"))?;
        assert_eq!(value["operation"]["op"], "consolidate");
        let back: MemoryMaintenanceSpec = serde_json::from_value(value).map_err(ctx("back"))?;
        assert_eq!(back, spec);
        let bad = serde_json::json!({
            "schema_version": 1, "operation": {"op": "sweep"}, "project_root": null, "global_root": null,
            "project_label": "p", "deadline_secs": 1, "max_unused_days": 90, "bogus": 1
        });
        assert!(serde_json::from_value::<MemoryMaintenanceSpec>(bad).is_err());
        Ok(())
    }

    #[test]
    fn execute_forget_deletes_in_both_roots() -> TestResult {
        let project = tempfile::tempdir().map_err(ctx("project"))?;
        let global = tempfile::tempdir().map_err(ctx("global"))?;
        let ps = FactStore::open(project.path(), FactScope::Project).map_err(ctx("p"))?;
        let gs = FactStore::open(global.path(), FactScope::Global).map_err(ctx("g"))?;
        ps.write(&fact("kurz", FactScope::Project))
            .map_err(ctx("pw"))?;
        gs.write(&fact("kurz", FactScope::Global))
            .map_err(ctx("gw"))?;
        let spec = spec_with_roots(
            MemoryMaintenanceOp::Forget {
                name: "kurz".to_owned(),
            },
            project.path(),
            global.path(),
            30,
        );
        execute_memory_maintenance(&spec).map_err(|e| TestError::Unexpected(e.reason()))?;
        assert!(ps.read("kurz").map_err(ctx("r1"))?.is_none());
        assert!(gs.read("kurz").map_err(ctx("r2"))?.is_none());
        Ok(())
    }

    #[test]
    fn execute_promote_copies_the_fact_to_global() -> TestResult {
        let project = tempfile::tempdir().map_err(ctx("project"))?;
        let global = tempfile::tempdir().map_err(ctx("global"))?;
        let ps = FactStore::open(project.path(), FactScope::Project).map_err(ctx("p"))?;
        ps.write(&fact("kurz", FactScope::Project))
            .map_err(ctx("pw"))?;
        let spec = spec_with_roots(
            MemoryMaintenanceOp::PromoteToGlobal {
                name: "kurz".to_owned(),
            },
            project.path(),
            global.path(),
            30,
        );
        let value =
            execute_memory_maintenance(&spec).map_err(|e| TestError::Unexpected(e.reason()))?;
        assert_eq!(value["already_present"], false);
        let gs = FactStore::open(global.path(), FactScope::Global).map_err(ctx("g"))?;
        assert!(gs.read("kurz").map_err(ctx("read"))?.is_some());
        Ok(())
    }

    #[test]
    fn expired_deadline_aborts_without_touching_the_stores() -> TestResult {
        // Frist 0 ist über `execute_memory_maintenance` nicht zulässig; die
        // Operationen werden daher mit abgelaufener Frist direkt geprüft.
        let project = tempfile::tempdir().map_err(ctx("project"))?;
        let global = tempfile::tempdir().map_err(ctx("global"))?;
        let ps = FactStore::open(project.path(), FactScope::Project).map_err(ctx("p"))?;
        ps.write(&fact("kurz", FactScope::Project))
            .map_err(ctx("pw"))?;
        let expired = Deadline::after(Duration::ZERO);

        let promote = spec_with_roots(
            MemoryMaintenanceOp::PromoteToGlobal {
                name: "kurz".to_owned(),
            },
            project.path(),
            global.path(),
            30,
        );
        let result = run_op(&promote, expired.clone());
        assert!(
            matches!(result, Err(MaintenanceFailure::TimedOut(_))),
            "{result:?}"
        );
        let gs = FactStore::open(global.path(), FactScope::Global).map_err(ctx("g"))?;
        assert!(gs.list().map_err(ctx("list"))?.is_empty());

        let forget = spec_with_roots(
            MemoryMaintenanceOp::Forget {
                name: "kurz".to_owned(),
            },
            project.path(),
            global.path(),
            30,
        );
        let result = run_op(&forget, expired.clone());
        assert!(result.is_err());
        assert!(ps.read("kurz").map_err(ctx("read"))?.is_some());
        // Kein Lock bleibt zurück.
        assert!(!project.path().join("consolidation.lock").exists());
        assert!(!global.path().join("consolidation.lock").exists());
        Ok(())
    }

    #[test]
    fn admit_rejects_a_zero_deadline_and_admits_nothing() -> TestResult {
        let state = tempfile::tempdir().map_err(ctx("state"))?;
        let jobs = JobStore::new(state.path());
        let spec = MemoryMaintenanceSpec::new(MemoryMaintenanceOp::Sweep, 0);
        assert!(admit_memory_maintenance(&jobs, scope(), &spec).is_err());
        let page = jobs
            .list(&harw_session_store::JobListQuery::default())
            .map_err(ctx("list"))?;
        assert!(page.jobs.is_empty());
        Ok(())
    }

    // -- Abbruch (kooperativ) vs. Fristablauf ------------------------------

    use harw_job_runtime::JobDisposition;
    use harw_session_store::{CancelRequest, ClaimRequest};

    // Job-Store mit einem zugelassenen und geclaimten Forget-Job; die globale
    // Wurzel ist gesperrt, der Lauf wartet also auf den Lock.
    struct Fixture {
        _state: tempfile::TempDir,
        _project: tempfile::TempDir,
        global: tempfile::TempDir,
        store: Arc<JobStore>,
        spec: MemoryMaintenanceSpec,
        id: WorkId,
        fact_store: FactStore,
        held: Option<harw_memory::consolidation::ConsolidationLock>,
    }

    fn fixture(deadline_secs: u64) -> TestResult<Fixture> {
        let state = tempfile::tempdir().map_err(ctx("state"))?;
        let project = tempfile::tempdir().map_err(ctx("project"))?;
        let global = tempfile::tempdir().map_err(ctx("global"))?;
        let gs = FactStore::open(global.path(), FactScope::Global).map_err(ctx("g"))?;
        gs.write(&fact("kurz", FactScope::Global))
            .map_err(ctx("seed"))?;
        let held = harw_memory::consolidation::ConsolidationLock::try_acquire(global.path())
            .map_err(ctx("hold lock"))?;
        let store = Arc::new(JobStore::new(state.path()));
        let spec = spec_with_roots(
            MemoryMaintenanceOp::Forget {
                name: "kurz".to_owned(),
            },
            project.path(),
            global.path(),
            deadline_secs,
        );
        let id = admit_memory_maintenance(&store, scope(), &spec).map_err(ctx("admit"))?;
        Ok(Fixture {
            _state: state,
            _project: project,
            global,
            store,
            spec,
            id,
            fact_store: gs,
            held: Some(held),
        })
    }

    fn claim(store: &JobStore, id: &WorkId) -> TestResult {
        store
            .claim(
                id,
                &ClaimRequest {
                    worker_id: "test-worker".to_owned(),
                    lease_ttl: SignedDuration::from_secs(60),
                    now: Timestamp::now(),
                },
            )
            .map_err(ctx("claim"))?;
        Ok(())
    }

    fn cancel(store: &JobStore, id: &WorkId) -> TestResult {
        store
            .cancel(
                id,
                &CancelRequest {
                    cancelled_at: Timestamp::now(),
                    cancelled_by: ApprovalActor::Operator {
                        id: "tester".to_owned(),
                    },
                    reason: "test cancel".to_owned(),
                },
            )
            .map_err(ctx("cancel"))?;
        Ok(())
    }

    #[tokio::test]
    async fn cancel_during_a_run_ends_cancelled_not_timed_out_and_keeps_the_fact() -> TestResult {
        let mut fx = fixture(30)?;
        claim(&fx.store, &fx.id)?;
        let run = tokio::spawn(run_memory_maintenance_job(
            fx.spec.clone(),
            fx.id.clone(),
            Arc::clone(&fx.store),
            std::future::pending::<()>(),
        ));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!run.is_finished(), "waits on the held lock");
        cancel(&fx.store, &fx.id)?; // operator cancel: store flag only
        let outcome = tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .map_err(ctx("run observes the cancel"))?
            .map_err(ctx("join"))?;
        assert!(
            matches!(outcome, JobOutcome::Cancelled { .. }),
            "{outcome:?}"
        );
        assert_eq!(outcome.disposition(), JobDisposition::Cancelled);
        assert!(fx.fact_store.read("kurz").map_err(ctx("read"))?.is_some());
        let record = fx.store.get(&fx.id).map_err(ctx("get"))?;
        assert_eq!(record.job.state, JobState::Cancelled);
        drop(fx.held.take());
        Ok(())
    }

    #[tokio::test]
    async fn the_cancel_signal_alone_also_stops_the_run() -> TestResult {
        let mut fx = fixture(30)?;
        claim(&fx.store, &fx.id)?;
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let run = tokio::spawn(run_memory_maintenance_job(
            fx.spec.clone(),
            fx.id.clone(),
            Arc::clone(&fx.store),
            async move {
                let _ = rx.await;
            },
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = tx.send(());
        let outcome = tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .map_err(ctx("run observes the signal"))?
            .map_err(ctx("join"))?;
        assert!(
            matches!(outcome, JobOutcome::Cancelled { .. }),
            "{outcome:?}"
        );
        assert!(fx.fact_store.read("kurz").map_err(ctx("read"))?.is_some());
        drop(fx.held.take());
        Ok(())
    }

    #[tokio::test]
    async fn deadline_expiry_is_timed_out_and_distinct_from_cancel() -> TestResult {
        let mut fx = fixture(1)?;
        claim(&fx.store, &fx.id)?;
        let outcome = run_memory_maintenance_job(
            fx.spec.clone(),
            fx.id.clone(),
            Arc::clone(&fx.store),
            std::future::pending::<()>(),
        )
        .await;
        assert!(outcome.is_timed_out(), "{outcome:?}");
        assert_eq!(outcome.disposition(), JobDisposition::TimedOut);
        assert!(!matches!(outcome, JobOutcome::Cancelled { .. }));
        assert!(fx.fact_store.read("kurz").map_err(ctx("read"))?.is_some());
        drop(fx.held.take());
        Ok(())
    }

    #[tokio::test]
    async fn cancel_before_the_run_starts_touches_nothing() -> TestResult {
        let mut fx = fixture(30)?;
        cancel(&fx.store, &fx.id)?; // Ready -> Cancelled, never claimed
        let outcome = run_memory_maintenance_job(
            fx.spec.clone(),
            fx.id.clone(),
            Arc::clone(&fx.store),
            std::future::pending::<()>(),
        )
        .await;
        assert!(
            matches!(outcome, JobOutcome::Cancelled { .. }),
            "{outcome:?}"
        );
        assert!(fx.fact_store.read("kurz").map_err(ctx("read"))?.is_some());
        assert!(
            fx.global.path().join("consolidation.lock").exists(),
            "only our own lock"
        );
        drop(fx.held.take());
        Ok(())
    }

    #[tokio::test]
    async fn cancel_after_the_job_finished_is_rejected_and_keeps_the_result() -> TestResult {
        let fx = fixture(30)?;
        let Fixture {
            store,
            spec,
            id,
            mut held,
            ..
        } = fx;
        drop(held.take()); // no lock contention: the forget runs through
        claim(&store, &id)?;
        let outcome = run_memory_maintenance_job(
            spec,
            id.clone(),
            Arc::clone(&store),
            std::future::pending::<()>(),
        )
        .await;
        assert!(
            matches!(outcome, JobOutcome::Succeeded { .. }),
            "{outcome:?}"
        );
        let record = store.get(&id).map_err(ctx("get"))?;
        let token = record
            .lease
            .as_ref()
            .ok_or(TestError::Missing("lease"))?
            .token();
        store
            .complete(
                &id,
                &harw_session_store::CompleteRequest {
                    token,
                    completed_at: Timestamp::now(),
                    outcome,
                },
            )
            .map_err(ctx("complete"))?;
        let late = store.cancel(
            &id,
            &CancelRequest {
                cancelled_at: Timestamp::now(),
                cancelled_by: ApprovalActor::Operator {
                    id: "tester".to_owned(),
                },
                reason: "too late".to_owned(),
            },
        );
        assert!(late.is_err(), "a finished job cannot be cancelled");
        assert_eq!(
            store.get(&id).map_err(ctx("get"))?.job.state,
            JobState::Completed
        );
        Ok(())
    }

    #[test]
    fn failure_kinds_map_to_distinct_outcomes() {
        let timed = MaintenanceFailure::TimedOut("d".to_owned()).into_outcome();
        assert!(timed.is_timed_out());
        let cancelled = MaintenanceFailure::Cancelled("c".to_owned());
        assert!(cancelled.is_cancelled() && !cancelled.is_timed_out());
        assert!(matches!(
            cancelled.into_outcome(),
            JobOutcome::Cancelled { .. }
        ));
        let failed = MaintenanceFailure::Failed("f".to_owned()).into_outcome();
        assert_eq!(failed.disposition(), JobDisposition::Failed);
    }

    #[test]
    fn timed_out_reason_carries_the_prefix() {
        let failure = MaintenanceFailure::TimedOut("frist".to_owned());
        assert!(failure.is_timed_out());
        assert!(failure.reason().starts_with(TIMED_OUT_REASON_PREFIX));
        assert_eq!(MaintenanceFailure::Failed("x".to_owned()).reason(), "x");
    }

    #[test]
    fn execute_rejects_relative_roots_and_unknown_versions() {
        let mut spec = MemoryMaintenanceSpec::new(MemoryMaintenanceOp::Sweep, 30);
        spec.project_root = Some(PathBuf::from("relative/memories"));
        assert!(matches!(
            execute_memory_maintenance(&spec),
            Err(MaintenanceFailure::Failed(_))
        ));
        let mut spec = MemoryMaintenanceSpec::new(MemoryMaintenanceOp::Sweep, 30);
        spec.schema_version = 99;
        assert!(matches!(
            execute_memory_maintenance(&spec),
            Err(MaintenanceFailure::Failed(_))
        ));
    }
}
