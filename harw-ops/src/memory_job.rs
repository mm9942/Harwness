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
//! # Lücke der Job-API
//! [`harw_job_runtime::JobOutcome`] kennt kein `TimedOut`; der Zustand
//! `TimedOut` wird nur vom Koordinator-Pfad (`harw-job-runtime`) erreicht, nicht
//! über `JobStore::complete`. Ein Fristablauf wird deshalb als
//! `JobOutcome::Failed { reason: "timed_out: …" }` festgehalten
//! ([`MaintenanceFailure::reason`]); der Job-Zustand lautet `failed`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use harw_job_runtime::{Budget, Job, JobKind, JobScope, RetryPolicy, StoredJob, WorkId};
use harw_memory::capture::consolidate_memories_with_options;
use harw_memory::consolidation::{ConsolidationError, Deadline};
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
/// Präfix des Fehlergrunds bei Fristablauf (siehe Moduldoku, „Lücke der
/// Job-API“).
pub const TIMED_OUT_REASON_PREFIX: &str = "timed_out";
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
    /// Jeder andere Fehlschlag (ungültige Payload, Lock, I/O, Ablehnung).
    Failed(String),
}

impl MaintenanceFailure {
    /// Der Text für `JobOutcome::Failed { reason }`; Fristablauf beginnt mit
    /// [`TIMED_OUT_REASON_PREFIX`].
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::TimedOut(detail) => format!("{TIMED_OUT_REASON_PREFIX}: {detail}"),
            Self::Failed(detail) => detail.clone(),
        }
    }

    /// `true` bei Fristablauf.
    #[must_use]
    pub fn is_timed_out(&self) -> bool {
        matches!(self, Self::TimedOut(_))
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
    let jobs = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("kein dauerhafter Job-Store konfiguriert".to_owned()))?;
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
        OpError::Execution(format!("Gedächtnis-Job konnte nicht eingereiht werden: {error}"))
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
    if spec.schema_version != MEMORY_MAINTENANCE_SCHEMA_VERSION {
        return Err(MaintenanceFailure::Failed(format!(
            "unbekannte Payload-Version {}",
            spec.schema_version
        )));
    }
    for root in [&spec.project_root, &spec.global_root].into_iter().flatten() {
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
    let deadline = Deadline::after(Duration::from_secs(spec.deadline_secs));
    let result = run_op(spec, deadline);
    match result {
        Err(MaintenanceFailure::Failed(detail)) if deadline.expired() => {
            Err(MaintenanceFailure::TimedOut(detail))
        }
        other => other,
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
        MemoryMaintenanceOp::Sweep => {
            consolidate(spec.project_root.as_deref(), FactScope::Project, spec, deadline)
        }
        MemoryMaintenanceOp::Forget { name } => {
            let output = crate::memory::forget_with_deadline(
                spec.project_root.clone(),
                spec.global_root.clone(),
                name,
                deadline,
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
                GlobalPromotionError::Deadline(detail) => {
                    MaintenanceFailure::TimedOut(detail.to_string())
                }
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
            ConsolidationError::Deadline(detail) => {
                MaintenanceFailure::TimedOut(detail.to_string())
            }
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
        let id = data["job_id"].as_str().ok_or(TestError::Missing("job_id"))?;
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
        assert!(matches!(result, Err(OpError::NotAvailable(_))), "{result:?}");
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
        ps.write(&fact("kurz", FactScope::Project)).map_err(ctx("pw"))?;
        gs.write(&fact("kurz", FactScope::Global)).map_err(ctx("gw"))?;
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
        ps.write(&fact("kurz", FactScope::Project)).map_err(ctx("pw"))?;
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
        ps.write(&fact("kurz", FactScope::Project)).map_err(ctx("pw"))?;
        let expired = Deadline::after(Duration::ZERO);

        let promote = spec_with_roots(
            MemoryMaintenanceOp::PromoteToGlobal {
                name: "kurz".to_owned(),
            },
            project.path(),
            global.path(),
            30,
        );
        let result = run_op(&promote, expired);
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
        let result = run_op(&forget, expired);
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
