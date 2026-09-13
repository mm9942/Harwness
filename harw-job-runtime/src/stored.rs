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

#[cfg(test)]
mod tests {
    use super::*;
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
    fn record(trace: Option<TraceContext>) -> StoredJob {
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
        job.mark_ready(now).expect("new job admits into ready");
        StoredJob {
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
        }
    }

    #[test]
    fn stored_job_with_trace_context_roundtrips_through_serde() {
        let original = record(Some(sample_trace()));

        let json = serde_json::to_string(&original).expect("serializes");
        let decoded: StoredJob = serde_json::from_str(&json).expect("deserializes");

        assert_eq!(decoded, original);
    }

    #[test]
    fn stored_job_without_trace_context_roundtrips_through_serde() {
        let original = record(None);

        let json = serde_json::to_string(&original).expect("serializes");
        let decoded: StoredJob = serde_json::from_str(&json).expect("deserializes");

        assert_eq!(decoded, original);
        assert_eq!(decoded.trace, None);
    }

    #[test]
    fn a_stored_job_without_trace_omits_the_field_from_its_json() {
        let json = serde_json::to_string(&record(None)).expect("serializes");

        assert!(
            !json.contains("\"trace\""),
            "a None trace must be absent, not serialized as `\"trace\":null`: {json}"
        );
    }

    /// The most important test in this module: a `StoredJob` written to disk
    /// before trace propagation existed has exactly this shape — no `trace`
    /// key anywhere. The literal below is hand-written, not derived from
    /// `record(None)`, so it independently pins the pre-trace file format
    /// rather than testing today's serializer against itself.
    #[test]
    fn a_pre_trace_stored_job_file_still_deserializes_with_no_trace() {
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
            .expect("a pre-trace StoredJob file must still deserialize");

        assert_eq!(decoded.trace, None);
        assert_eq!(decoded.job.id, WorkId::from_str("work-legacy"));
        assert_eq!(decoded.revision, 0);
    }
}
