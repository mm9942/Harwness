//! Durable attempt record (Job-Runtime-Doc §6, §15).
//!
//! The job record in `harw-job-store` carries the governance state
//! ([`harw_job_core::JobState`]). The finer attempt lifecycle
//! ([`LifecycleState`]), the recovery identity and the enforcement report
//! live in one sidecar per attempt: `attempts/<attempt-id>.json`
//! ([`ATTEMPTS_SIDECAR`]).
//!
//! The attempt id is derived from the job id and the store-issued fencing
//! epoch ([`attempt_id_for`]), so it is unique per claim, and every attempt
//! only ever writes its own sidecar: a runner that lost its lease can record
//! its own `Lost` outcome without touching the successor's attempt.

use harw_job_core::{
    AttemptId, CancellationCause, Deadline, ExitOutcome, LifecycleEvent, LifecycleState,
    LifecycleTransition, RunnerId, SandboxReport,
};
use harw_types::WorkId;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::error::RuntimeError;

/// Sidecar kind of attempt records in the job store.
pub const ATTEMPTS_SIDECAR: &str = "attempts";

/// Current schema version of [`AttemptRecord`].
pub const ATTEMPT_RECORD_VERSION: u16 = 1;

/// The attempt id of the claim with fencing `epoch` on `job`:
/// `<job-id>-e<epoch>`.
///
/// # Errors
/// [`RuntimeError::AttemptRecord`] if the job id is not a valid attempt id
/// component (it is always a valid store id, which is stricter).
pub fn attempt_id_for(job: &WorkId, epoch: u64) -> Result<AttemptId, RuntimeError> {
    let raw = format!("{}-e{epoch}", job.as_str());
    AttemptId::new(raw.clone()).map_err(|error| RuntimeError::AttemptRecord {
        attempt: raw,
        detail: error.to_string(),
    })
}

/// Persisted state of one attempt, generic over the executor's identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "I: Serialize",
    deserialize = "I: serde::de::DeserializeOwned"
))]
#[serde(deny_unknown_fields)]
pub struct AttemptRecord<I> {
    /// Schema version ([`ATTEMPT_RECORD_VERSION`]).
    pub version: u16,
    /// The job.
    pub job_id: WorkId,
    /// The attempt.
    pub attempt_id: AttemptId,
    /// Runner holding the lease of this attempt.
    pub runner_id: RunnerId,
    /// Fencing epoch of the claim.
    pub epoch: u64,
    /// Current lifecycle state.
    pub state: LifecycleState,
    /// Every validated transition, in order.
    pub transitions: Vec<LifecycleTransition>,
    /// Recovery identity of the primary process, once spawned.
    #[serde(default)]
    pub identity: Option<I>,
    /// Diagnostic PID of the primary.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Achieved enforcement, once known.
    #[serde(default)]
    pub sandbox: Option<SandboxReport>,
    /// When the body was spawned.
    #[serde(default)]
    pub started_at: Option<Timestamp>,
    /// Wall-clock deadline of the attempt.
    #[serde(default)]
    pub deadline: Option<Deadline>,
    /// How the primary ended, if observed.
    #[serde(default)]
    pub exit: Option<ExitOutcome>,
    /// Why the attempt was cancelled, if it was.
    #[serde(default)]
    pub cancellation: Option<CancellationCause>,
    /// Human-readable reason of a non-success terminal state.
    #[serde(default)]
    pub reason: Option<String>,
    /// Whether the attempt was resumed by a restarted coordinator.
    #[serde(default)]
    pub recovered: bool,
    /// Last update.
    pub updated_at: Timestamp,
}

impl<I> AttemptRecord<I> {
    /// A freshly claimed attempt: `Queued → Claimed`.
    ///
    /// # Errors
    /// Never in practice; the transition is part of the table.
    pub fn claimed(
        job_id: WorkId,
        attempt_id: AttemptId,
        runner_id: RunnerId,
        epoch: u64,
        now: Timestamp,
    ) -> Result<Self, RuntimeError> {
        let claim = LifecycleTransition::apply(LifecycleState::Queued, LifecycleEvent::Claim)?;
        Ok(Self {
            version: ATTEMPT_RECORD_VERSION,
            job_id,
            attempt_id,
            runner_id,
            epoch,
            state: claim.target_state(),
            transitions: vec![claim],
            identity: None,
            pid: None,
            sandbox: None,
            started_at: None,
            deadline: None,
            exit: None,
            cancellation: None,
            reason: None,
            recovered: false,
            updated_at: now,
        })
    }

    /// Applies `event` through the lifecycle table.
    ///
    /// # Errors
    /// [`RuntimeError::Transition`] if the table rejects it (nothing
    /// changes).
    pub fn apply(&mut self, event: LifecycleEvent, now: Timestamp) -> Result<(), RuntimeError> {
        let transition = LifecycleTransition::apply(self.state, event)?;
        self.state = transition.target_state();
        self.transitions.push(transition);
        self.updated_at = now;
        Ok(())
    }

    /// Whether the attempt has reached a terminal state.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }
}

impl<I: Serialize> AttemptRecord<I> {
    /// Encodes the record as sidecar bytes.
    ///
    /// # Errors
    /// [`RuntimeError::AttemptRecord`] on an encoder failure.
    pub fn to_bytes(&self) -> Result<Vec<u8>, RuntimeError> {
        serde_json::to_vec_pretty(self).map_err(|error| RuntimeError::AttemptRecord {
            attempt: self.attempt_id.to_string(),
            detail: error.to_string(),
        })
    }
}

impl<I: serde::de::DeserializeOwned> AttemptRecord<I> {
    /// Decodes and version-checks a sidecar.
    ///
    /// # Errors
    /// [`RuntimeError::AttemptRecord`] for undecodable bytes or an unknown
    /// version.
    pub fn from_bytes(attempt: &str, bytes: &[u8]) -> Result<Self, RuntimeError> {
        let record: Self =
            serde_json::from_slice(bytes).map_err(|error| RuntimeError::AttemptRecord {
                attempt: attempt.to_owned(),
                detail: error.to_string(),
            })?;
        if record.version != ATTEMPT_RECORD_VERSION {
            return Err(RuntimeError::AttemptRecord {
                attempt: attempt.to_owned(),
                detail: format!(
                    "version {} is not supported (this build reads {ATTEMPT_RECORD_VERSION})",
                    record.version
                ),
            });
        }
        if record.attempt_id.as_str() != attempt {
            return Err(RuntimeError::AttemptRecord {
                attempt: attempt.to_owned(),
                detail: format!("record is keyed to attempt '{}'", record.attempt_id),
            });
        }
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::{AttemptRecord, attempt_id_for};
    use crate::test_support::{TestResult, ctx};
    use harw_job_core::{LifecycleEvent, LifecycleState, RunnerId};
    use harw_types::WorkId;
    use jiff::Timestamp;

    fn record() -> TestResult<AttemptRecord<u32>> {
        let job = WorkId::from_str("job-1");
        let attempt = attempt_id_for(&job, 3).map_err(ctx("attempt id"))?;
        assert_eq!(attempt.as_str(), "job-1-e3");
        AttemptRecord::claimed(
            job,
            attempt,
            RunnerId::new("runner-a").map_err(ctx("runner"))?,
            3,
            Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("claimed"))
    }

    #[test]
    fn lifecycle_is_validated_and_recorded() -> TestResult {
        let mut record = record()?;
        assert_eq!(record.state, LifecycleState::Claimed);
        record
            .apply(LifecycleEvent::Start, Timestamp::UNIX_EPOCH)
            .map_err(ctx("start"))?;
        record
            .apply(LifecycleEvent::Spawned, Timestamp::UNIX_EPOCH)
            .map_err(ctx("spawned"))?;
        assert!(
            record
                .apply(LifecycleEvent::Claim, Timestamp::UNIX_EPOCH)
                .is_err()
        );
        assert_eq!(record.state, LifecycleState::Running);
        record
            .apply(LifecycleEvent::Succeed, Timestamp::UNIX_EPOCH)
            .map_err(ctx("succeed"))?;
        assert!(record.is_terminal());
        assert_eq!(record.transitions.len(), 4);
        Ok(())
    }

    #[test]
    fn sidecar_round_trips_and_rejects_foreign_keys() -> TestResult {
        let mut record = record()?;
        record.identity = Some(42);
        let bytes = record.to_bytes().map_err(ctx("encode"))?;
        let back = AttemptRecord::<u32>::from_bytes("job-1-e3", &bytes).map_err(ctx("decode"))?;
        assert_eq!(back, record);
        assert!(AttemptRecord::<u32>::from_bytes("job-1-e4", &bytes).is_err());
        let mut future = record;
        future.version = 99;
        let bytes = future.to_bytes().map_err(ctx("encode"))?;
        assert!(AttemptRecord::<u32>::from_bytes("job-1-e3", &bytes).is_err());
        Ok(())
    }
}
