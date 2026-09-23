//! Durable job record DTOs shared by a persistence implementation and the
//! future MCP job service.
//!
//! These types intentionally do not perform I/O. They keep all server-resolved
//! job input, fencing state, scheduling metadata, and terminal outcome in one
//! serializable record so a store can make every lifecycle transition atomic.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use harw_observe::TraceContext;
use harw_types::{ApprovalActor, TenantId, WorkspaceId};

use crate::error::{JobRuntimeError, JobRuntimeResult};
use crate::job::JobState;
use crate::{Job, Lease, LeaseToken};

/// Immutable server-resolved authority boundary for one durable job.
///
/// A model or remote worker never supplies this value during claim, renew, or
/// completion. The admission boundary resolves it from configured tenant and
/// workspace bindings plus the authenticated submitter, then persists it with
/// the job for its entire lifetime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobScope {
    tenant: TenantId,
    workspace: WorkspaceId,
    submitter: ApprovalActor,
}

impl JobScope {
    #[must_use]
    pub fn new(tenant: TenantId, workspace: WorkspaceId, submitter: ApprovalActor) -> Self {
        Self {
            tenant,
            workspace,
            submitter,
        }
    }

    #[must_use]
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    #[must_use]
    pub fn workspace(&self) -> &WorkspaceId {
        &self.workspace
    }

    #[must_use]
    pub fn submitter(&self) -> &ApprovalActor {
        &self.submitter
    }
}

/// Result of atomically claiming a job from a durable store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobClaim {
    pub job: Job,
    /// Immutable server-resolved authority boundary inherited by the worker.
    pub scope: JobScope,
    pub lease: Lease,
    pub token: LeaseToken,
}

/// Terminal result recorded once a job can no longer be claimed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobCompletion {
    pub completed_at: Timestamp,
    pub outcome: JobOutcome,
}

/// Durable provenance for a supervisor-issued job cancellation.
///
/// This is stored separately from [`JobOutcome`] so terminal outcome handling
/// remains uniform while an audit consumer can still determine which trusted
/// principal cancelled the work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCancellation {
    pub cancelled_at: Timestamp,
    pub cancelled_by: ApprovalActor,
    pub reason: String,
}

/// A durable, model-readable summary of a job's terminal disposition.
///
/// `input` and `result` data are structured, but the harness—not a model—is
/// responsible for resolving their workspace, capabilities, and secret refs
/// before admission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum JobOutcome {
    Succeeded { result: serde_json::Value },
    Failed { reason: String },
    Cancelled { reason: String },
    Blocked { reason: String },
}

/// Canonical persistence record for one governed job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredJob {
    pub job: Job,
    /// Immutable tenant, workspace, and authenticated-submitter boundary
    /// resolved before the job is admitted.
    pub scope: JobScope,
    /// Server-resolved task payload. It must never contain an authority grant
    /// derived from a model request or raw secret material.
    pub input: serde_json::Value,
    pub submitted_at: Timestamp,
    /// Earliest instant a Ready job is eligible for another claim.
    pub not_before: Timestamp,
    /// The active fenced lease for a Running job, if any.
    pub lease: Option<Lease>,
    /// Last issued fencing epoch. A replacement lease always uses a larger
    /// value, making late zombie-worker writes rejectable.
    pub lease_epoch: u64,
    /// Terminal record, present only after the job reaches a terminal state.
    pub completion: Option<JobCompletion>,
    /// Present only for a supervisor-issued cancellation. This retains the
    /// trusted cancellation principal in durable state.
    pub cancellation: Option<JobCancellation>,
    /// Monotonic record revision for audit/event cursoring.
    pub revision: u64,
    /// The trace context under which this work was admitted.
    ///
    /// Optional because `StoredJob` is an existing on-disk file format:
    /// records written before trace propagation existed carry no `trace`
    /// field at all, and those files must keep deserializing. A `None` here
    /// therefore means "this record predates trace introduction", not "this
    /// job ran without a trace" — a record admitted after introduction always
    /// carries `Some`. Absent from the JSON entirely (not `null`) when unset,
    /// so a legacy file's byte-for-byte shape is never rewritten just by
    /// being read back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceContext>,
}

/// Ergebnis eines erfolgreichen Reclaims einer verwaisten Lease (siehe
/// [`StoredJob::reclaim`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReclaimOutcome {
    /// Die Lease wurde freigegeben; der Job ist wieder `Ready` und für einen
    /// erneuten Claim erreichbar.
    Requeued,
    /// Die Retry-Policy ist erschöpft; der Job wechselt terminal auf `Failed`.
    Exhausted,
}

impl StoredJob {
    /// Reclaimt eine abgelaufene Lease, deren Halter nicht mehr lebt.
    ///
    /// # Beschreibung
    /// Der Aufrufer übergibt ein Lebendigkeits-Prädikat `is_holder_alive`,
    /// das prüft, ob der aktuelle Lease-Halter noch existiert (Prozess-,
    /// Session- oder Heartbeat-Quelle außerhalb dieser Crate — diese Funktion
    /// sondiert selbst keine Prozesse). Ein Reclaim greift ausschließlich,
    /// wenn **beide** Bedingungen zutreffen: die Lease ist bereits abgelaufen
    /// ([`Lease::is_expired`]) **und** das Prädikat meldet den Halter als
    /// nicht mehr lebendig. Eine noch gültige Lease sowie eine abgelaufene
    /// Lease eines weiterhin lebenden, nur langsamen Halters bleiben
    /// unangetastet und lösen [`JobRuntimeError::LeaseContended`] aus.
    ///
    /// Beim tatsächlichen Reclaim wird zuerst der Fencing-Epoch erhöht und
    /// die gespeicherte Lease gelöscht, bevor der Fehlversuch über
    /// [`Job::record_failure`] verbucht wird (Retry-Zähler hoch, RetryPolicy
    /// respektiert; bei Erschöpfung terminal `Failed`). Ein späterer
    /// Complete- oder Renew-Versuch des alten Halters trägt noch den alten,
    /// jetzt überholten Epoch: jeder künftige Claim vergibt einen Epoch
    /// größer als `lease_epoch`, sodass das alte Fencing-Token nicht mehr
    /// gültig sein kann.
    ///
    /// # Argumente
    /// - `now` (`Timestamp`): Zeitpunkt der Reclaim-Prüfung.
    /// - `is_holder_alive` (`FnOnce(&str) -> bool`): Lebendigkeits-Prädikat
    ///   für den Namen/die Identität des aktuellen Lease-Halters.
    ///
    /// # Rückgabe
    /// [`ReclaimOutcome::Requeued`], wenn der Job wieder `Ready` ist, sonst
    /// [`ReclaimOutcome::Exhausted`], wenn die Retry-Policy erschöpft ist und
    /// der Job terminal auf `Failed` steht.
    ///
    /// # Fehler
    /// - [`JobRuntimeError::InvalidState`]: keine aktive Lease vorhanden
    ///   (Job ist nicht `Running`).
    /// - [`JobRuntimeError::LeaseContended`]: die Lease ist noch gültig, oder
    ///   sie ist zwar abgelaufen, aber der Halter gilt laut Prädikat noch als
    ///   lebendig — in beiden Fällen ist ein Reclaim nicht zulässig.
    ///
    /// # Nebenläufigkeit
    /// Reine Datenmutation ohne I/O, Locks oder Threads. Die aufrufende
    /// Store-Schicht ist dafür verantwortlich, konkurrierende Reclaim-/
    /// Claim-Versuche auf demselben Datensatz zu serialisieren.
    ///
    /// # Beispiele
    /// ```rust,no_run
    /// use harw_job_runtime::stored::StoredJob;
    ///
    /// fn reclaim_if_stranded(record: &mut StoredJob, now: jiff::Timestamp) {
    ///     // `is_holder_alive` liefert hier bewusst immer `false`, um einen
    ///     // vollständig verwaisten Halter zu simulieren.
    ///     let _ = record.reclaim(now, |_holder| false);
    /// }
    /// ```
    pub fn reclaim<F>(
        &mut self,
        now: Timestamp,
        is_holder_alive: F,
    ) -> JobRuntimeResult<ReclaimOutcome>
    where
        F: FnOnce(&str) -> bool,
    {
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| JobRuntimeError::InvalidState {
                work_id: self.job.id.clone(),
                expected: JobState::Running,
                actual: self.job.state,
            })?;

        // Eine noch gültige Lease, oder eine abgelaufene Lease eines nur
        // langsamen, aber lebenden Halters, darf nicht reclaimt werden.
        if !lease.is_expired(now) || is_holder_alive(lease.holder.as_str()) {
            return Err(JobRuntimeError::LeaseContended {
                work_id: self.job.id.clone(),
                holder: lease.holder.clone(),
            });
        }

        // Fencing zuerst: jeder künftige Claim vergibt einen höheren Epoch,
        // sodass ein Complete/Renew mit dem alten Token danach nicht mehr
        // passen kann, selbst wenn der alte Halter doch noch antwortet.
        self.lease_epoch = self.lease_epoch.saturating_add(1);
        self.lease = None;

        match self.job.record_failure(now) {
            Ok(_delay) => Ok(ReclaimOutcome::Requeued),
            Err(JobRuntimeError::RetryExhausted { .. }) => Ok(ReclaimOutcome::Exhausted),
            Err(other) => Err(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::{Budget, Job, JobKind, RetryPolicy};
    use harw_types::WorkId;
    use jiff::SignedDuration;

    fn scope() -> JobScope {
        JobScope::new(
            TenantId::from_str("test-tenant"),
            WorkspaceId::from_str("test-workspace"),
            ApprovalActor::Operator {
                id: "test-operator".to_owned(),
            },
        )
    }

    fn sample_trace() -> TraceContext {
        TraceContext {
            trace_id: "a".repeat(32),
            span_id: "b".repeat(16),
            parent_span_id: None,
        }
    }

    /// Builds a `StoredJob` with the given `trace`, otherwise identical to
    /// every other fixture in this module — isolates the field under test.
    fn record(trace: Option<TraceContext>) -> TestResult<StoredJob> {
        let now = Timestamp::now();
        let mut job = Job::new(
            WorkId::from_str("work-1"),
            JobKind::Worker,
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: 2,
                base_delay: SignedDuration::from_secs(1),
                factor: 2.0,
                max_delay: SignedDuration::from_secs(10),
            },
            now,
        );
        job.mark_ready(now)
            .map_err(ctx("new job admits into ready"))?;
        Ok(StoredJob {
            job,
            scope: scope(),
            input: serde_json::json!({"task": "review"}),
            submitted_at: now,
            not_before: now,
            lease: None,
            lease_epoch: 0,
            completion: None,
            cancellation: None,
            revision: 0,
            trace,
        })
    }

    /// Builds a `StoredJob` whose job is actually `Running` under a fenced
    /// lease at `epoch`, so reclaim tests exercise a realistic pre-state.
    fn claimed_record(
        now: Timestamp,
        ttl: SignedDuration,
        holder: &str,
        epoch: u64,
    ) -> TestResult<StoredJob> {
        let mut record = record(None)?; // job.state == Ready already
        let _ = record
            .job
            .claim(holder, now, ttl)
            .map_err(ctx("job claims into running"))?;
        let lease = Lease::acquire_fenced(
            record.job.id.clone(),
            holder,
            now,
            ttl,
            epoch,
            "nonce-under-test",
        )
        .map_err(ctx("fenced lease acquires"))?;
        record.lease_epoch = epoch;
        record.lease = Some(lease);
        Ok(record)
    }

    #[test]
    fn reclaim_fails_when_no_lease_is_present() -> TestResult {
        let mut record = record(None)?; // job.state == Ready, lease == None
        let now = Timestamp::now();

        let Err(error) = record.reclaim(now, |_holder| false) else {
            return Err(TestError::Unexpected(
                "reclaim without an active lease must fail".into(),
            ));
        };

        assert!(matches!(
            error,
            JobRuntimeError::InvalidState {
                expected: JobState::Running,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn reclaim_refuses_a_lease_that_has_not_expired_yet() -> TestResult {
        let now = Timestamp::now();
        let mut record = claimed_record(now, SignedDuration::from_secs(60), "worker-a", 1)?;
        let before = record.clone();

        // holder liveness is irrelevant here
        let Err(error) = record.reclaim(now, |_holder| false) else {
            return Err(TestError::Unexpected(
                "a still-valid lease must not be reclaimed".into(),
            ));
        };

        assert!(matches!(error, JobRuntimeError::LeaseContended { .. }));
        assert_eq!(
            record, before,
            "a rejected reclaim must not mutate the record"
        );
        Ok(())
    }

    #[test]
    fn reclaim_refuses_an_expired_lease_whose_holder_is_still_alive() -> TestResult {
        let now = Timestamp::now();
        let mut record = claimed_record(now, SignedDuration::from_secs(1), "worker-a", 1)?;
        let later = now
            .checked_add(SignedDuration::from_secs(2))
            .map_err(ctx("later timestamp in range"))?;
        let before = record.clone();

        // live but slow: must not be reclaimed
        let Err(error) = record.reclaim(later, |_holder| true) else {
            return Err(TestError::Unexpected(
                "a live-but-slow holder must not be reclaimed".into(),
            ));
        };

        assert!(matches!(error, JobRuntimeError::LeaseContended { .. }));
        assert_eq!(
            record, before,
            "a rejected reclaim must not mutate the record"
        );
        Ok(())
    }

    #[test]
    fn reclaim_requeues_an_expired_lease_whose_holder_is_dead() -> TestResult {
        let now = Timestamp::now();
        let mut record = claimed_record(now, SignedDuration::from_secs(1), "worker-a", 1)?;
        let old_token = record
            .lease
            .as_ref()
            .ok_or(TestError::Missing("lease on claimed record"))?
            .token();
        let later = now
            .checked_add(SignedDuration::from_secs(2))
            .map_err(ctx("later timestamp in range"))?;

        let outcome = record
            .reclaim(later, |_holder| false)
            .map_err(ctx("an orphaned expired lease must be reclaimable"))?;

        assert_eq!(outcome, ReclaimOutcome::Requeued);
        assert_eq!(record.job.state, JobState::Ready);
        assert_eq!(record.job.attempts, 1);
        assert!(record.lease.is_none(), "the stale lease must be cleared");
        assert!(
            record.lease_epoch > old_token.epoch,
            "fencing must advance the epoch past the reclaimed lease's token"
        );
        Ok(())
    }

    #[test]
    fn reclaim_moves_to_failed_once_the_retry_policy_is_exhausted() -> TestResult {
        let now = Timestamp::now();
        let mut record = claimed_record(now, SignedDuration::from_secs(1), "worker-a", 1)?;
        record.job.retry = RetryPolicy {
            max_attempts: 1,
            base_delay: SignedDuration::from_secs(1),
            factor: 2.0,
            max_delay: SignedDuration::from_secs(10),
        };
        let later = now
            .checked_add(SignedDuration::from_secs(2))
            .map_err(ctx("later timestamp in range"))?;

        let outcome = record.reclaim(later, |_holder| false).map_err(ctx(
            "reclaim itself succeeds even when the policy is exhausted",
        ))?;

        assert_eq!(outcome, ReclaimOutcome::Exhausted);
        assert_eq!(record.job.state, JobState::Failed);
        assert!(record.lease.is_none());
        Ok(())
    }

    #[test]
    fn reclaim_fencing_epoch_prevents_the_old_token_from_matching_a_future_lease() -> TestResult {
        let now = Timestamp::now();
        let mut record = claimed_record(now, SignedDuration::from_secs(1), "worker-a", 5)?;
        let stale_token = record
            .lease
            .as_ref()
            .ok_or(TestError::Missing("lease on claimed record"))?
            .token();
        let later = now
            .checked_add(SignedDuration::from_secs(2))
            .map_err(ctx("later timestamp in range"))?;

        record
            .reclaim(later, |_holder| false)
            .map_err(ctx("orphaned lease reclaims"))?;

        // Any subsequent claim must be issued at an epoch beyond the one the
        // stranded holder still carries, so its stale token can never match
        // again (mirrors `harw-session-store`'s claim(): next epoch =
        // lease_epoch + 1).
        let next_epoch = record.lease_epoch.saturating_add(1);
        assert!(next_epoch > stale_token.epoch);
        let next_lease = Lease::acquire_fenced(
            record.job.id.clone(),
            "worker-b",
            later,
            SignedDuration::from_secs(60),
            next_epoch,
            "nonce-after-reclaim",
        )
        .map_err(ctx("a fresh lease can be issued after reclaim"))?;
        assert!(!next_lease.matches_token(&stale_token));
        Ok(())
    }

    #[test]
    fn stored_job_with_trace_context_roundtrips_through_serde() -> TestResult {
        let original = record(Some(sample_trace()))?;

        let json = serde_json::to_string(&original).map_err(ctx("serializes"))?;
        let decoded: StoredJob = serde_json::from_str(&json).map_err(ctx("deserializes"))?;

        assert_eq!(decoded, original);
        Ok(())
    }

    #[test]
    fn stored_job_without_trace_context_roundtrips_through_serde() -> TestResult {
        let original = record(None)?;

        let json = serde_json::to_string(&original).map_err(ctx("serializes"))?;
        let decoded: StoredJob = serde_json::from_str(&json).map_err(ctx("deserializes"))?;

        assert_eq!(decoded, original);
        assert_eq!(decoded.trace, None);
        Ok(())
    }

    #[test]
    fn a_stored_job_without_trace_omits_the_field_from_its_json() -> TestResult {
        let json = serde_json::to_string(&record(None)?).map_err(ctx("serializes"))?;

        assert!(
            !json.contains("\"trace\""),
            "a None trace must be absent, not serialized as `\"trace\":null`: {json}"
        );
        Ok(())
    }

    /// The most important test in this module: a `StoredJob` written to disk
    /// before trace propagation existed has exactly this shape — no `trace`
    /// key anywhere. The literal below is hand-written, not derived from
    /// `record(None)`, so it independently pins the pre-trace file format
    /// rather than testing today's serializer against itself.
    #[test]
    fn a_pre_trace_stored_job_file_still_deserializes_with_no_trace() -> TestResult {
        let legacy = r#"{
            "job": {
                "id": "work-legacy",
                "kind": "worker",
                "state": "ready",
                "budget": {
                    "max_tokens": null,
                    "max_wall": null,
                    "max_tool_calls": null
                },
                "usage": {
                    "tokens": 0,
                    "wall": "0s",
                    "tool_calls": 0
                },
                "retry": {
                    "max_attempts": 4,
                    "base_delay": "1s",
                    "factor": 2.0,
                    "max_delay": "30s"
                },
                "attempts": 0,
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z"
            },
            "scope": {
                "tenant": "test-tenant",
                "workspace": "test-workspace",
                "submitter": {
                    "kind": "operator",
                    "id": "test-operator"
                }
            },
            "input": {"task": "review"},
            "submitted_at": "2024-01-01T00:00:00Z",
            "not_before": "2024-01-01T00:00:00Z",
            "lease": null,
            "lease_epoch": 0,
            "completion": null,
            "cancellation": null,
            "revision": 0
        }"#;

        let decoded: StoredJob = serde_json::from_str(legacy)
            .map_err(ctx("a pre-trace StoredJob file must still deserialize"))?;

        assert_eq!(decoded.trace, None);
        assert_eq!(decoded.job.id, WorkId::from_str("work-legacy"));
        assert_eq!(decoded.revision, 0);
        Ok(())
    }
}
