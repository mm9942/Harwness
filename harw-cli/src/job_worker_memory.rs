//! `memory_maintenance`-Jobs: Gedächtnis-Wartung als Job des Job-Workers.
//!
//! # Ein Claim = eine Wartungsoperation
//! `/memory consolidate|forget|promote --to-global` (`harw_ops::memory`) und der
//! Startup-Sweep der Runtime reihen einen Job der Art
//! `memory_maintenance` ein (`harw_ops::memory_job`). Dieser Worker claimt ihn
//! wie jede andere Job-Art (fenced Lease, Heartbeat und Commit über den
//! `DurableJobRunner`) und führt den gemeinsamen Handler-Kern
//! [`harw_ops::memory_job::execute_memory_maintenance`] aus.
//!
//! # Nicht blockierend
//! Die Wartung ist blockierende Dateiarbeit (inkl. Lock-Wartezeit bis zur
//! Frist). Sie läuft in `spawn_blocking`, nie auf dem Async-Executor; der
//! Aufruf liegt zudem in der Lane der langen Läufe (`WorkDriverLane`), damit
//! eine Wartung bis zu ihrer Frist keine anderen Job-Arten aufhält.
//!
//! # Frist und Ergebnis
//! Die Frist kommt aus der Job-Payload (`deadline_secs`). Ablauf bricht ohne
//! Teilzustand ab; der Job endet als `Failed` mit Grund `timed_out: …`
//! (`JobOutcome` kennt kein `TimedOut`, siehe `harw_ops::memory_job`). Eine
//! fehlerhafte Payload endet ebenfalls als `Failed`, ohne etwas zu berühren.
//! Ein vor dem Start bereits angeforderter Abbruch beendet den Job als
//! `Cancelled`; ein laufender Vorgang wird nicht unterbrochen (er ist durch
//! seine Frist begrenzt und committet erst nach dem letzten Fristcheck).

use std::time::Duration;

use harw_job_runtime::{JobClaim, JobKind, JobOutcome};
use harw_ops::memory_job::{
    MaintenanceFailure, MemoryMaintenanceSpec, execute_memory_maintenance,
    is_memory_maintenance_kind,
};

use super::WorkerExecutionControl;
use std::sync::Arc;

/// Zusätzliche Wartezeit über die Frist hinaus, bevor ein hängender
/// blockierender Thread als zeitüberschritten aufgegeben wird.
const BLOCKING_GRACE: Duration = Duration::from_secs(5);

/// Grund bei nicht lesbarer Payload (Details nur im Log).
const INVALID_PAYLOAD_REASON: &str = "invalid memory_maintenance payload";

/// `true` für die Job-Art der Gedächtnis-Wartung.
pub(super) fn is_memory_kind(kind: &JobKind) -> bool {
    is_memory_maintenance_kind(kind)
}

/// Führt einen geclaimten `memory_maintenance`-Job aus.
///
/// # Arguments
/// - `claim` (`JobClaim`): der geclaimte Job (nur Id für das Log).
/// - `input` (`serde_json::Value`): die Payload
///   ([`MemoryMaintenanceSpec`]).
/// - `control` (`Arc<WorkerExecutionControl>`): Abbruchsteuerung der Lease.
///
/// # Returns
/// `Succeeded { result }` bei Erfolg, sonst `Failed { reason }` (Fristablauf
/// mit `timed_out:`-Präfix) bzw. `Cancelled` bei Abbruch vor dem Start.
pub(super) async fn execute_memory_maintenance_claim(
    claim: JobClaim,
    input: serde_json::Value,
    control: Arc<WorkerExecutionControl>,
) -> JobOutcome {
    let work_id = claim.job.id.as_str().to_owned();
    let spec: MemoryMaintenanceSpec = match serde_json::from_value(input) {
        Ok(spec) => spec,
        Err(error) => {
            tracing::warn!(work_id = %work_id, %error, "memory_maintenance payload rejected");
            return JobOutcome::Failed {
                reason: INVALID_PAYLOAD_REASON.to_owned(),
            };
        }
    };
    if *control.cancellation().borrow() {
        return JobOutcome::Cancelled {
            reason: "cancelled before the memory maintenance started".to_owned(),
        };
    }

    let op = spec.operation.label();
    let wait = Duration::from_secs(spec.deadline_secs).saturating_add(BLOCKING_GRACE);
    let blocking = tokio::task::spawn_blocking(move || execute_memory_maintenance(&spec));
    match tokio::time::timeout(wait, blocking).await {
        Ok(Ok(Ok(result))) => JobOutcome::Succeeded { result },
        Ok(Ok(Err(failure))) => failed(&work_id, op, &failure),
        Ok(Err(join_error)) => {
            tracing::warn!(work_id = %work_id, op, error = %join_error, "memory maintenance task failed");
            JobOutcome::Failed {
                reason: "memory maintenance task failed".to_owned(),
            }
        }
        Err(_) => failed(
            &work_id,
            op,
            &MaintenanceFailure::TimedOut("the maintenance did not finish in time".to_owned()),
        ),
    }
}

fn failed(work_id: &str, op: &str, failure: &MaintenanceFailure) -> JobOutcome {
    tracing::warn!(work_id, op, timed_out = failure.is_timed_out(), reason = %failure.reason(), "memory maintenance failed");
    JobOutcome::Failed {
        reason: failure.reason(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job_worker::{
        JobWorkerContext, MAX_CONCURRENT_WORK_DRIVER_RUNS, WorkDriverLane, WorkerServices,
        poll_ready_jobs,
    };
    use harw_core::{EchoModelProvider, JobExecutionRegistry};
    use harw_job_runtime::JobState;
    use harw_memory::{FactScope, FactStore};
    use harw_ops::memory_job::{
        MemoryMaintenanceOp, TIMED_OUT_REASON_PREFIX, admit_memory_maintenance,
    };
    use harw_session_store::JobStore;
    use harw_types::{ApprovalActor, TenantId, WorkId, WorkspaceId};
    use std::collections::BTreeSet;
    use std::path::Path;

    type TestError = Box<dyn std::error::Error>;
    type TestResult<T = ()> = Result<T, TestError>;

    // Legt `facts/<name>.md` einer Wurzel an (Frontmatter wie `FactStore`).
    fn seed_fact(root: &Path, name: &str, scope: FactScope) -> TestResult<FactStore> {
        let store = FactStore::open(root, scope)?;
        let text = format!(
            "---\nname: {name}\ndescription: d\ntype: fact\nscope: {}\n\
             created: 2026-01-01T00:00:00Z\nupdated: 2026-01-01T00:00:00Z\n\
             confidence: 1.00\nsources: []\ntags: []\n---\n\ninhalt\n",
            scope.as_str()
        );
        std::fs::write(root.join("facts").join(format!("{name}.md")), text)?;
        Ok(store)
    }

    fn scope() -> harw_job_runtime::JobScope {
        harw_job_runtime::JobScope::new(
            TenantId::from_str("tenant"),
            WorkspaceId::from_str("workspace"),
            ApprovalActor::Operator {
                id: "operator".to_owned(),
            },
        )
    }

    fn services(store: &Arc<JobStore>, root: &Path) -> WorkerServices {
        let context = Arc::new(JobWorkerContext {
            transcript_root: root.to_path_buf(),
            configured_submitters: Arc::new(BTreeSet::new()),
            runtime_root: None,
            knowledge: None,
            verify_runner: None,
        });
        WorkerServices::new(
            Arc::clone(store),
            Arc::new(JobExecutionRegistry::new()),
            Arc::new(EchoModelProvider::new("unused")),
            None,
            context,
        )
    }

    async fn run(services: &WorkerServices) -> usize {
        let mut lane = WorkDriverLane::new(MAX_CONCURRENT_WORK_DRIVER_RUNS);
        let completed = poll_ready_jobs(services, &mut lane).await;
        completed + lane.drain().await
    }

    fn stored(store: &JobStore, id: &WorkId) -> TestResult<harw_job_runtime::StoredJob> {
        Ok(store.get(id)?)
    }

    #[tokio::test]
    async fn handler_runs_a_forget_job_to_completion() -> TestResult {
        let state = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let global = tempfile::tempdir()?;
        let ps = seed_fact(project.path(), "kurz", FactScope::Project)?;
        let store = Arc::new(JobStore::new(state.path()));
        let mut spec = MemoryMaintenanceSpec::new(
            MemoryMaintenanceOp::Forget {
                name: "kurz".to_owned(),
            },
            30,
        );
        spec.project_root = Some(project.path().to_path_buf());
        spec.global_root = Some(global.path().to_path_buf());
        let id = admit_memory_maintenance(&store, scope(), &spec)?;
        assert_eq!(stored(&store, &id)?.job.state, JobState::Ready);

        let completed = run(&services(&store, state.path())).await;
        assert_eq!(completed, 1, "the job reached a terminal state");

        let record = stored(&store, &id)?;
        assert_eq!(record.job.state, JobState::Completed);
        assert!(ps.read("kurz")?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn handler_runs_a_sweep_job_to_completion() -> TestResult {
        let state = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let store = Arc::new(JobStore::new(state.path()));
        let mut spec = MemoryMaintenanceSpec::new(MemoryMaintenanceOp::Sweep, 120);
        spec.project_root = Some(project.path().to_path_buf());
        let id = admit_memory_maintenance(&store, scope(), &spec)?;

        run(&services(&store, state.path())).await;

        let record = stored(&store, &id)?;
        assert_eq!(record.job.state, JobState::Completed);
        let result = match record.completion.map(|c| c.outcome) {
            Some(JobOutcome::Succeeded { result }) => result,
            other => return Err(format!("expected Succeeded, got {other:?}").into()),
        };
        assert_eq!(result["merged"], 0);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_payload_fails_the_job_without_running_anything() -> TestResult {
        let state = tempfile::tempdir()?;
        let store = Arc::new(JobStore::new(state.path()));
        // Eine gültig zugelassene Vorlage, deren Eingabe dann kaputt ist.
        let spec = MemoryMaintenanceSpec::new(MemoryMaintenanceOp::Sweep, 30);
        let template = admit_memory_maintenance(&store, scope(), &spec)?;
        let mut record = stored(&store, &template)?;
        record.job.id = WorkId::from_str("bad-payload");
        record.input = serde_json::json!({ "schema_version": 1, "bogus": true });
        store.admit(&record)?;
        run(&services(&store, state.path())).await;
        let bad = stored(&store, &WorkId::from_str("bad-payload"))?;
        assert_eq!(bad.job.state, JobState::Failed);
        let reason = match bad.completion.map(|c| c.outcome) {
            Some(JobOutcome::Failed { reason }) => reason,
            other => return Err(format!("expected Failed, got {other:?}").into()),
        };
        assert_eq!(reason, INVALID_PAYLOAD_REASON);
        Ok(())
    }

    #[tokio::test]
    async fn held_lock_until_the_deadline_fails_the_job_timed_out_and_keeps_the_store() -> TestResult
    {
        let state = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let global = tempfile::tempdir()?;
        let gs = seed_fact(global.path(), "kurz", FactScope::Global)?;
        // Ein anderer Schreiber hält die globale Wurzel über die Frist hinaus.
        let held = harw_memory::consolidation::ConsolidationLock::try_acquire(global.path())?;
        let store = Arc::new(JobStore::new(state.path()));
        let mut spec = MemoryMaintenanceSpec::new(
            MemoryMaintenanceOp::Forget {
                name: "kurz".to_owned(),
            },
            1,
        );
        spec.project_root = Some(project.path().to_path_buf());
        spec.global_root = Some(global.path().to_path_buf());
        let id = admit_memory_maintenance(&store, scope(), &spec)?;

        run(&services(&store, state.path())).await;

        let record = stored(&store, &id)?;
        assert_eq!(record.job.state, JobState::Failed);
        let reason = match record.completion.map(|c| c.outcome) {
            Some(JobOutcome::Failed { reason }) => reason,
            other => return Err(format!("expected Failed, got {other:?}").into()),
        };
        assert!(reason.starts_with(TIMED_OUT_REASON_PREFIX), "{reason}");
        // Unverändert: der Fakt existiert noch.
        assert!(gs.read("kurz")?.is_some());
        drop(held);
        Ok(())
    }

    #[test]
    fn the_kind_is_recognised_and_supported() {
        let kind = JobKind::Custom("memory_maintenance".to_owned());
        assert!(is_memory_kind(&kind));
        assert!(crate::job_worker::is_supported_kind(&kind));
        assert!(!is_memory_kind(&JobKind::Worker));
    }
}
