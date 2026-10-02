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
//! # Frist, Abbruch und Ergebnis
//! Die Frist kommt aus der Job-Payload (`deadline_secs`). Ablauf bricht ohne
//! Teilzustand ab; der Job endet typisiert als `timed_out`
//! (`JobOutcome::timed_out`, Drahtform `failed` + `timed_out: …`). Eine
//! fehlerhafte Payload endet als `Failed`, ohne etwas zu berühren.
//!
//! Abbruch ist kooperativ und unterscheidbar: der gemeinsame Treiber
//! (`harw_ops::memory_job::run_memory_maintenance_job`) setzt ein
//! `CancelFlag`, sobald (a) die Lease-Steuerung des Runners abbricht oder
//! (b) der Job-Store den Job als `Cancelled` führt (Poll alle 150 ms — der
//! Heartbeat allein würde bis zu TTL/3 brauchen). Der Kern prüft das Flag
//! zwischen den Schritten und endet ohne Teilzustand als `Cancelled`.

use harw_job_runtime::{JobClaim, JobKind, JobOutcome};
use harw_ops::memory_job::{
    MemoryMaintenanceSpec, is_memory_maintenance_kind, run_memory_maintenance_job,
};
use harw_session_store::JobStore;

use super::WorkerExecutionControl;
use std::sync::Arc;

/// Grund bei nicht lesbarer Payload (Details nur im Log).
const INVALID_PAYLOAD_REASON: &str = "invalid memory_maintenance payload";

/// `true` für die Job-Art der Gedächtnis-Wartung.
pub(super) fn is_memory_kind(kind: &JobKind) -> bool {
    is_memory_maintenance_kind(kind)
}

/// Führt einen geclaimten `memory_maintenance`-Job aus.
///
/// # Arguments
/// - `claim` (`JobClaim`): der geclaimte Job.
/// - `input` (`serde_json::Value`): die Payload
///   ([`MemoryMaintenanceSpec`]).
/// - `job_store` (`Arc<JobStore>`): der Job-Store; ein Operator-`cancel`
///   setzt dort den Zustand `Cancelled`, den der Treiber zwischen den
///   Schritten abfragt.
/// - `control` (`Arc<WorkerExecutionControl>`): Abbruchsteuerung der Lease
///   (Registry-Abbruch, Lease-Verlust).
///
/// # Returns
/// `Succeeded { result }`, `Cancelled` bei Abbruch (vor dem Start oder
/// kooperativ während des Laufs), `Failed` mit `timed_out: …` bei Fristablauf,
/// sonst `Failed`.
pub(super) async fn execute_memory_maintenance_claim(
    claim: JobClaim,
    input: serde_json::Value,
    job_store: Arc<JobStore>,
    control: Arc<WorkerExecutionControl>,
) -> JobOutcome {
    let work_id = claim.job.id.clone();
    let spec: MemoryMaintenanceSpec = match serde_json::from_value(input) {
        Ok(spec) => spec,
        Err(error) => {
            tracing::warn!(work_id = %work_id.as_str(), %error, "memory_maintenance payload rejected");
            return JobOutcome::Failed {
                reason: INVALID_PAYLOAD_REASON.to_owned(),
            };
        }
    };
    let mut cancellation = control.cancellation();
    let signal = async move {
        // Ein gesetztes oder verworfenes Signal beendet das Warten; ein
        // verworfener Sender (Lauf endet) darf keinen Abbruch vortäuschen.
        loop {
            if *cancellation.borrow() {
                return;
            }
            if cancellation.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    };
    run_memory_maintenance_job(spec, work_id, job_store, signal).await
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
    use std::time::Duration;

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
        let mut lane = WorkDriverLane::with_lanes(harw_job_runtime::JobLanes::new(
            MAX_CONCURRENT_WORK_DRIVER_RUNS,
            2,
        ));
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

    fn cancel_request() -> harw_session_store::CancelRequest {
        harw_session_store::CancelRequest {
            cancelled_at: jiff::Timestamp::now(),
            cancelled_by: ApprovalActor::Operator {
                id: "operator".to_owned(),
            },
            reason: "test cancel".to_owned(),
        }
    }

    // Wartet, bis der Job `Running` ist (geclaimt vom Worker).
    async fn wait_running(store: &JobStore, id: &WorkId) -> TestResult {
        for _ in 0..200 {
            if stored(store, id)?.job.state == JobState::Running {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Err("job never reached running".into())
    }

    fn forget_spec(project: &Path, global: &Path, deadline: u64) -> MemoryMaintenanceSpec {
        let mut spec = MemoryMaintenanceSpec::new(
            MemoryMaintenanceOp::Forget {
                name: "kurz".to_owned(),
            },
            deadline,
        );
        spec.project_root = Some(project.to_path_buf());
        spec.global_root = Some(global.to_path_buf());
        spec
    }

    #[tokio::test]
    async fn cancel_during_the_run_ends_the_job_cancelled_and_keeps_the_fact() -> TestResult {
        let state = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let global = tempfile::tempdir()?;
        let gs = seed_fact(global.path(), "kurz", FactScope::Global)?;
        let held = harw_memory::consolidation::ConsolidationLock::try_acquire(global.path())?;
        let store = Arc::new(JobStore::new(state.path()));
        let spec = forget_spec(project.path(), global.path(), 30);
        let id = admit_memory_maintenance(&store, scope(), &spec)?;

        let svc = services(&store, state.path());
        let worker = tokio::spawn(async move { run(&svc).await });
        wait_running(&store, &id).await?;
        store.cancel(&id, &cancel_request())?;
        tokio::time::timeout(Duration::from_secs(15), worker).await??;

        let record = stored(&store, &id)?;
        assert_eq!(record.job.state, JobState::Cancelled);
        assert_eq!(record.disposition(), harw_job_runtime::JobDisposition::Cancelled);
        assert!(gs.read("kurz")?.is_some(), "no partial state");
        drop(held);
        Ok(())
    }

    #[tokio::test]
    async fn timed_out_is_a_first_class_disposition_distinct_from_cancelled() -> TestResult {
        let state = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let global = tempfile::tempdir()?;
        let _gs = seed_fact(global.path(), "kurz", FactScope::Global)?;
        let held = harw_memory::consolidation::ConsolidationLock::try_acquire(global.path())?;
        let store = Arc::new(JobStore::new(state.path()));
        let id = admit_memory_maintenance(
            &store,
            scope(),
            &forget_spec(project.path(), global.path(), 1),
        )?;
        run(&services(&store, state.path())).await;
        let record = stored(&store, &id)?;
        assert_eq!(record.disposition(), harw_job_runtime::JobDisposition::TimedOut);
        assert_ne!(record.job.state, JobState::Cancelled);
        drop(held);
        Ok(())
    }

    #[tokio::test]
    async fn raising_the_memory_lane_starts_the_waiting_job_and_exactly_one_worker_claims(
    ) -> TestResult {
        let state = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let global = tempfile::tempdir()?;
        let _gs = seed_fact(global.path(), "kurz", FactScope::Global)?;
        let held = harw_memory::consolidation::ConsolidationLock::try_acquire(global.path())?;
        let store = Arc::new(JobStore::new(state.path()));
        let spec = forget_spec(project.path(), global.path(), 30);
        let first = admit_memory_maintenance(&store, scope(), &spec)?;
        let second = admit_memory_maintenance(&store, scope(), &spec)?;
        let svc = services(&store, state.path());
        let mut lane = WorkDriverLane::new(MAX_CONCURRENT_WORK_DRIVER_RUNS);

        poll_ready_jobs(&svc, &mut lane).await;
        // The poll order follows the work ids: whichever came first runs.
        let mut running = None;
        for _ in 0..200 {
            for id in [&first, &second] {
                if stored(&store, id)?.job.state == JobState::Running {
                    running = Some(id.clone());
                }
            }
            if running.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let running = running.ok_or("no job reached running")?;
        let waiting = if running == first { &second } else { &first };
        assert_eq!(stored(&store, waiting)?.job.state, JobState::Ready, "lane of 1 is full");
        let status = lane.lanes.memory().status();
        assert_eq!((status.limit, status.in_use), (1, 1));

        harw_job_runtime::lanes::write_limit(state.path(), "memory", 2)?;
        assert_eq!(lane.lanes.apply_overrides(state.path()), vec!["memory"]);
        poll_ready_jobs(&svc, &mut lane).await;
        wait_running(&store, waiting).await?;
        assert_eq!(lane.lanes.memory().status().in_use, 2);

        // One worker id claimed each job exactly once (no second claimer).
        for id in [&first, &second] {
            let record = stored(&store, id)?;
            assert_eq!(record.lease_epoch, 1);
            assert_eq!(
                record.lease.as_ref().map(|l| l.holder.as_str()),
                Some(crate::job_worker::WORKER_ID)
            );
        }
        // Lowering stops nothing that runs.
        harw_job_runtime::lanes::write_limit(state.path(), "memory", 1)?;
        lane.lanes.apply_overrides(state.path());
        assert_eq!(stored(&store, &first)?.job.state, JobState::Running);
        assert_eq!(stored(&store, &second)?.job.state, JobState::Running);

        store.cancel(&first, &cancel_request())?;
        store.cancel(&second, &cancel_request())?;
        tokio::time::timeout(Duration::from_secs(15), lane.drain()).await?;
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
