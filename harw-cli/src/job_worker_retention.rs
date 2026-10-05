//! `retention_sweep`-Jobs: Aufbewahrungs-Sweeps als Job des Job-Workers.
//!
//! `harw cleanup` reiht einen Job der Art `retention_sweep` ein
//! (`harw_ops::retention_job`). Dieser Worker claimt ihn wie jede andere
//! Job-Art (fenced Lease, Heartbeat, Commit über den `DurableJobRunner`) und
//! führt den gemeinsamen Treiber
//! [`harw_ops::retention_job::run_retention_sweep_job`] aus: blockierende
//! Dateiarbeit in `spawn_blocking`, Abbruch kooperativ per Lease-Signal und
//! Store-Poll, Frist aus der Payload. Der Aufruf liegt in der Memory-Lane der
//! langen Läufe, damit ein Sweep andere Job-Arten nicht aufhält.

use harw_job_runtime::{JobClaim, JobKind, JobOutcome};
use harw_ops::retention_job::{
    RetentionSweepSpec, is_retention_sweep_kind, run_retention_sweep_job,
};
use harw_session_store::JobStore;

use super::WorkerExecutionControl;
use std::sync::Arc;

/// Grund bei nicht lesbarer Payload (Details nur im Log).
const INVALID_PAYLOAD_REASON: &str = "invalid retention_sweep payload";

/// `true` für die Job-Art der Aufbewahrungs-Sweeps.
pub(super) fn is_retention_kind(kind: &JobKind) -> bool {
    is_retention_sweep_kind(kind)
}

/// Führt einen geclaimten `retention_sweep`-Job aus.
///
/// # Returns
/// `Succeeded { result }`, `Cancelled` bei Abbruch, `Failed` mit
/// `timed_out: …` bei Fristablauf, sonst `Failed`.
pub(super) async fn execute_retention_sweep_claim(
    claim: JobClaim,
    input: serde_json::Value,
    job_store: Arc<JobStore>,
    control: Arc<WorkerExecutionControl>,
) -> JobOutcome {
    let work_id = claim.job.id.clone();
    let spec: RetentionSweepSpec = match serde_json::from_value(input) {
        Ok(spec) => spec,
        Err(error) => {
            tracing::warn!(work_id = %work_id.as_str(), %error, "retention_sweep payload rejected");
            return JobOutcome::Failed {
                reason: INVALID_PAYLOAD_REASON.to_owned(),
            };
        }
    };
    let mut cancellation = control.cancellation();
    let signal = async move {
        loop {
            if *cancellation.borrow() {
                return;
            }
            if cancellation.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    };
    run_retention_sweep_job(spec, work_id, job_store, signal).await
}
