//! Trusted boundary between untrusted MCP requests and durable job state.
//!
//! Transport sessions prove only protocol continuity. A configured ingress
//! resolver creates [`McpRequestContext`] from an authenticated identity; the
//! client-provided `clientInfo` field is never used as authority.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use harw_job_runtime::{
    Budget, Job, JobCompletion, JobKind, JobScope, JobState, Lease, RetryPolicy, StoredJob,
};
use harw_session_store::{CancelRequest, JobStore, SessionStoreError};
use harw_types::{ApprovalActor, TenantId, WorkId, WorkspaceId};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type McpSupervisorFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, McpSupervisorError>> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum McpJobCapability {
    SubmitOwn,
    ReadOwn,
    ReadWorkspace,
    CancelOwn,
    CancelWorkspace,
}

/// Kinds an MCP principal may submit. Custom runtime kinds remain a
/// composition-owned policy decision and cannot be minted from MCP input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpSubmittedJobKind {
    Dream,
    Worker,
}

impl McpSubmittedJobKind {
    fn into_runtime_kind(self) -> JobKind {
        match self {
            Self::Dream => JobKind::Dream,
            Self::Worker => JobKind::Worker,
        }
    }
}

/// Closed MCP submission input. Scope, identity, retry policy, scheduling,
/// and work identity are resolved at the durable supervisor boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpJobSubmission {
    pub kind: McpSubmittedJobKind,
    pub input: Value,
    #[serde(default)]
    pub budget: Option<Budget>,
}

/// Server-authenticated identity and its resolved workspace authority.
#[derive(Debug, Clone)]
pub struct McpPrincipal {
    actor: ApprovalActor,
    tenant: TenantId,
    workspace: WorkspaceId,
    capabilities: BTreeSet<McpJobCapability>,
}

impl McpPrincipal {
    /// This constructor is for trusted ingress/authentication resolvers only.
    #[must_use]
    pub fn from_trusted_ingress(
        actor: ApprovalActor,
        tenant: TenantId,
        workspace: WorkspaceId,
        capabilities: impl IntoIterator<Item = McpJobCapability>,
    ) -> Self {
        Self {
            actor,
            tenant,
            workspace,
            capabilities: capabilities.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn capabilities(&self) -> &BTreeSet<McpJobCapability> {
        &self.capabilities
    }
}

/// Context constructed by the server after authentication, never deserialized
/// from a tool argument.
#[derive(Debug, Clone)]
pub struct McpRequestContext {
    session_id: String,
    principal: McpPrincipal,
}

impl McpRequestContext {
    #[must_use]
    pub fn from_trusted_ingress(session_id: String, principal: McpPrincipal) -> Self {
        Self {
            session_id,
            principal,
        }
    }
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpJobStatus {
    pub work_id: WorkId,
    pub kind: String,
    pub state: JobState,
    pub submitted_at: Timestamp,
    pub updated_at: Timestamp,
    pub attempts: u32,
    pub revision: u64,
    pub completion: Option<JobCompletion>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpCancellationReceipt {
    pub work_id: WorkId,
    pub previous_state: JobState,
    pub cancelled_at: Timestamp,
    pub revision: u64,
    /// Whether the cancelled job had an active worker lease. This is metadata
    /// only; no lease token or worker identity is exposed to an MCP client.
    pub worker_signal_required: bool,
    /// Redacted outcome of asking the execution owner to stop a running
    /// worker. Durable cancellation has already succeeded regardless of this
    /// best-effort delivery result.
    pub worker_cancellation: WorkerCancellationStatus,
}

/// Redacted outcome of forwarding a persisted cancellation to the execution
/// owner. The MCP boundary deliberately never exposes lease credentials,
/// sandbox paths, or worker identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerCancellationStatus {
    /// The job had no active lease, so there was no worker to signal.
    NotRequired,
    /// The owning child-controller accepted a request to stop the worker.
    Requested,
    /// No execution controller is currently connected to accept the request.
    Unavailable,
}

/// Execution-side bridge for a cancellation which is already durable and
/// fenced. Implementations may signal a child process, mark a remote worker
/// for termination, or enqueue an out-of-process control message.
///
/// This trait is intentionally synchronous at the MCP boundary: it accepts a
/// best-effort handoff after the store has atomically persisted cancellation
/// and revoked the old lease. A sink must not attempt durable state changes.
pub trait WorkerCancellationSink: Send + Sync {
    fn request_worker_cancellation(
        &self,
        work_id: &WorkId,
        prior_lease: &Lease,
    ) -> WorkerCancellationStatus;
}

/// Safe default when the harness has no live child controller. It makes the
/// durable cancellation visible without pretending that a process was killed.
#[derive(Debug, Default)]
pub struct UnavailableWorkerCancellationSink;

impl WorkerCancellationSink for UnavailableWorkerCancellationSink {
    fn request_worker_cancellation(
        &self,
        _work_id: &WorkId,
        _prior_lease: &Lease,
    ) -> WorkerCancellationStatus {
        WorkerCancellationStatus::Unavailable
    }
}

#[derive(Debug)]
pub enum McpSupervisorError {
    NotAuthorized,
    InvalidSubmission(String),
    JobStore(SessionStoreError),
}

impl fmt::Display for McpSupervisorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAuthorized => f.write_str("MCP principal is not authorized for this job"),
            Self::InvalidSubmission(detail) => write!(f, "invalid MCP job submission: {detail}"),
            Self::JobStore(error) => write!(f, "durable job operation failed: {error}"),
        }
    }
}
impl std::error::Error for McpSupervisorError {}

/// Redacted, authority-checked job operations. Lease tokens, resolved input,
/// sandbox paths and worker identities never cross this interface.
pub trait McpSupervisor: Send + Sync {
    fn submit_job<'a>(
        &'a self,
        context: &'a McpRequestContext,
        submission: McpJobSubmission,
    ) -> McpSupervisorFuture<'a, McpJobStatus>;
    fn job_status<'a>(
        &'a self,
        context: &'a McpRequestContext,
        work_id: WorkId,
    ) -> McpSupervisorFuture<'a, McpJobStatus>;
    fn cancel_job<'a>(
        &'a self,
        context: &'a McpRequestContext,
        work_id: WorkId,
        reason: String,
    ) -> McpSupervisorFuture<'a, McpCancellationReceipt>;
}

/// Durable implementation. Process signalling remains the owning child
/// controller's job; `worker_signal_required` tells it to signal the exact
/// captured fenced lease after cancellation is persisted.
pub struct DurableMcpSupervisor {
    store: Arc<JobStore>,
    worker_cancellation_sink: Arc<dyn WorkerCancellationSink>,
}

impl DurableMcpSupervisor {
    #[must_use]
    pub fn new(store: Arc<JobStore>) -> Self {
        Self::with_worker_cancellation_sink(store, Arc::new(UnavailableWorkerCancellationSink))
    }

    /// Composes durable cancellation with the process-owning controller.
    /// The sink is called only after [`JobStore::cancel`] has committed the
    /// terminal cancellation and fenced any prior running lease.
    #[must_use]
    pub fn with_worker_cancellation_sink(
        store: Arc<JobStore>,
        worker_cancellation_sink: Arc<dyn WorkerCancellationSink>,
    ) -> Self {
        Self {
            store,
            worker_cancellation_sink,
        }
    }
}

impl McpSupervisor for DurableMcpSupervisor {
    fn submit_job<'a>(
        &'a self,
        context: &'a McpRequestContext,
        submission: McpJobSubmission,
    ) -> McpSupervisorFuture<'a, McpJobStatus> {
        Box::pin(async move {
            if !context
                .principal
                .capabilities
                .contains(&McpJobCapability::SubmitOwn)
            {
                return Err(McpSupervisorError::NotAuthorized);
            }
            validate_submission(&submission)?;

            let now = Timestamp::now();
            let mut job = Job::new(
                WorkId::new(),
                submission.kind.into_runtime_kind(),
                submission.budget.unwrap_or_else(Budget::unbounded),
                default_retry_policy(),
                now,
            );
            job.mark_ready(now)
                .map_err(|error| McpSupervisorError::InvalidSubmission(error.to_string()))?;
            let record = StoredJob {
                job,
                scope: JobScope::new(
                    context.principal.tenant.clone(),
                    context.principal.workspace.clone(),
                    context.principal.actor.clone(),
                ),
                input: submission.input,
                submitted_at: now,
                not_before: now,
                lease: None,
                lease_epoch: 0,
                completion: None,
                cancellation: None,
                revision: 0,
                // McpRequestContext (session_id + principal) carries no trace
                // context; nothing upstream of this MCP boundary threads one
                // through yet.
                trace: None,
            };
            self.store
                .admit(&record)
                .map_err(McpSupervisorError::JobStore)?;
            Ok(status(&record))
        })
    }

    fn job_status<'a>(
        &'a self,
        context: &'a McpRequestContext,
        work_id: WorkId,
    ) -> McpSupervisorFuture<'a, McpJobStatus> {
        Box::pin(async move {
            let record = self
                .store
                .get(&work_id)
                .map_err(McpSupervisorError::JobStore)?;
            if !can_read(&context.principal, &record.scope) {
                return Err(McpSupervisorError::NotAuthorized);
            }
            Ok(status(&record))
        })
    }

    fn cancel_job<'a>(
        &'a self,
        context: &'a McpRequestContext,
        work_id: WorkId,
        reason: String,
    ) -> McpSupervisorFuture<'a, McpCancellationReceipt> {
        Box::pin(async move {
            let record = self
                .store
                .get(&work_id)
                .map_err(McpSupervisorError::JobStore)?;
            if !can_cancel(&context.principal, &record.scope) {
                return Err(McpSupervisorError::NotAuthorized);
            }
            let cancelled_at = Timestamp::now();
            let transition = self
                .store
                .cancel(
                    &work_id,
                    &CancelRequest {
                        cancelled_at,
                        cancelled_by: context.principal.actor.clone(),
                        reason,
                    },
                )
                .map_err(McpSupervisorError::JobStore)?;
            let worker_signal_required = transition.prior_lease.is_some();
            let worker_cancellation = match transition.prior_lease.as_ref() {
                Some(prior_lease) => match self
                    .worker_cancellation_sink
                    .request_worker_cancellation(&transition.work_id, prior_lease)
                {
                    cancellation_status @ (WorkerCancellationStatus::Requested
                    | WorkerCancellationStatus::Unavailable) => cancellation_status,
                    // A prior lease proves a worker signal was required. Do
                    // not let a faulty execution bridge report otherwise.
                    WorkerCancellationStatus::NotRequired => WorkerCancellationStatus::Unavailable,
                },
                None => WorkerCancellationStatus::NotRequired,
            };
            let record = self
                .store
                .get(&work_id)
                .map_err(McpSupervisorError::JobStore)?;
            Ok(McpCancellationReceipt {
                work_id,
                previous_state: transition.previous_state,
                cancelled_at,
                revision: record.revision,
                worker_signal_required,
                worker_cancellation,
            })
        })
    }
}

fn same_scope(principal: &McpPrincipal, scope: &JobScope) -> bool {
    principal.tenant == *scope.tenant() && principal.workspace == *scope.workspace()
}
fn can_read(principal: &McpPrincipal, scope: &JobScope) -> bool {
    same_scope(principal, scope)
        && (principal
            .capabilities
            .contains(&McpJobCapability::ReadWorkspace)
            || (principal.capabilities.contains(&McpJobCapability::ReadOwn)
                && principal.actor == *scope.submitter()))
}
fn validate_submission(submission: &McpJobSubmission) -> Result<(), McpSupervisorError> {
    if !submission.input.is_object() {
        return Err(McpSupervisorError::InvalidSubmission(
            "input must be a JSON object".to_owned(),
        ));
    }
    if let Some(budget) = &submission.budget {
        if budget.max_tokens == Some(0)
            || budget.max_tool_calls == Some(0)
            || budget
                .max_wall
                .is_some_and(|wall| wall <= jiff::SignedDuration::ZERO)
        {
            return Err(McpSupervisorError::InvalidSubmission(
                "budget limits must be greater than zero when provided".to_owned(),
            ));
        }
    }
    Ok(())
}
fn default_retry_policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 2,
        base_delay: jiff::SignedDuration::from_secs(1),
        factor: 2.0,
        max_delay: jiff::SignedDuration::from_secs(10),
    }
}
fn can_cancel(principal: &McpPrincipal, scope: &JobScope) -> bool {
    same_scope(principal, scope)
        && (principal
            .capabilities
            .contains(&McpJobCapability::CancelWorkspace)
            || (principal
                .capabilities
                .contains(&McpJobCapability::CancelOwn)
                && principal.actor == *scope.submitter()))
}
fn status(record: &StoredJob) -> McpJobStatus {
    McpJobStatus {
        work_id: record.job.id.clone(),
        kind: job_kind(&record.job.kind),
        state: record.job.state,
        submitted_at: record.submitted_at,
        updated_at: record.job.updated_at,
        attempts: record.job.attempts,
        revision: record.revision,
        completion: record.completion.clone(),
    }
}
fn job_kind(kind: &JobKind) -> String {
    match kind {
        JobKind::Dream => "dream".to_owned(),
        JobKind::Worker => "worker".to_owned(),
        JobKind::Custom(value) => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use harw_job_runtime::{Budget, Job, RetryPolicy};
    use harw_session_store::ClaimRequest;
    use jiff::SignedDuration;

    fn scope() -> JobScope {
        JobScope::new(
            TenantId::from_str("tenant-a"),
            WorkspaceId::from_str("workspace-a"),
            ApprovalActor::Operator {
                id: "mia".to_owned(),
            },
        )
    }

    fn principal(
        actor: &str,
        tenant: &str,
        workspace: &str,
        capabilities: Vec<McpJobCapability>,
    ) -> McpPrincipal {
        McpPrincipal::from_trusted_ingress(
            ApprovalActor::Operator {
                id: actor.to_owned(),
            },
            TenantId::from_str(tenant),
            WorkspaceId::from_str(workspace),
            capabilities,
        )
    }

    fn context() -> McpRequestContext {
        McpRequestContext::from_trusted_ingress(
            "mcp-session-test".to_owned(),
            principal(
                "mia",
                "tenant-a",
                "workspace-a",
                vec![McpJobCapability::CancelOwn],
            ),
        )
    }

    fn submit_context() -> McpRequestContext {
        McpRequestContext::from_trusted_ingress(
            "mcp-submit-session-test".to_owned(),
            principal(
                "mia",
                "tenant-a",
                "workspace-a",
                vec![McpJobCapability::SubmitOwn, McpJobCapability::ReadOwn],
            ),
        )
    }

    fn record(id: &str) -> StoredJob {
        let now = Timestamp::now();
        let mut job = Job::new(
            WorkId::from_str(id),
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
        job.mark_ready(now).unwrap();
        StoredJob {
            job,
            scope: scope(),
            input: serde_json::json!({"task": "test"}),
            submitted_at: now,
            not_before: now,
            lease: None,
            lease_epoch: 0,
            completion: None,
            cancellation: None,
            revision: 0,
            trace: None,
        }
    }

    struct PersistedTransitionSink {
        store: Arc<JobStore>,
        calls: Mutex<Vec<WorkId>>,
    }

    impl PersistedTransitionSink {
        fn new(store: Arc<JobStore>) -> Self {
            Self {
                store,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<WorkId> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl WorkerCancellationSink for PersistedTransitionSink {
        fn request_worker_cancellation(
            &self,
            work_id: &WorkId,
            prior_lease: &Lease,
        ) -> WorkerCancellationStatus {
            // This observes the durable store at signal time. It proves the
            // supervisor did not contact a worker before the cancellation and
            // its fence were committed.
            let persisted = self.store.get(work_id).unwrap();
            assert_eq!(persisted.job.state, JobState::Cancelled);
            assert!(persisted.lease.is_none());
            assert!(persisted.lease_epoch > prior_lease.epoch);
            self.calls.lock().unwrap().push(work_id.clone());
            WorkerCancellationStatus::Requested
        }
    }

    struct InvalidNotRequiredSink;

    impl WorkerCancellationSink for InvalidNotRequiredSink {
        fn request_worker_cancellation(
            &self,
            _work_id: &WorkId,
            _prior_lease: &Lease,
        ) -> WorkerCancellationStatus {
            WorkerCancellationStatus::NotRequired
        }
    }

    #[test]
    fn own_capabilities_do_not_cross_actor_or_scope() {
        let job_scope = scope();
        let owner = principal(
            "mia",
            "tenant-a",
            "workspace-a",
            vec![McpJobCapability::ReadOwn, McpJobCapability::CancelOwn],
        );
        assert!(can_read(&owner, &job_scope));
        assert!(can_cancel(&owner, &job_scope));
        let another_actor = principal(
            "other",
            "tenant-a",
            "workspace-a",
            vec![McpJobCapability::ReadOwn],
        );
        assert!(!can_read(&another_actor, &job_scope));
        let another_workspace = principal(
            "mia",
            "tenant-a",
            "workspace-b",
            vec![McpJobCapability::CancelWorkspace],
        );
        assert!(!can_cancel(&another_workspace, &job_scope));
    }

    #[test]
    fn workspace_grants_are_tenant_bounded() {
        let job_scope = scope();
        let workspace_reader = principal(
            "other",
            "tenant-a",
            "workspace-a",
            vec![McpJobCapability::ReadWorkspace],
        );
        assert!(can_read(&workspace_reader, &job_scope));
        let foreign_tenant = principal(
            "other",
            "tenant-b",
            "workspace-a",
            vec![McpJobCapability::ReadWorkspace],
        );
        assert!(!can_read(&foreign_tenant, &job_scope));
    }

    #[tokio::test]
    async fn pending_and_ready_cancellation_do_not_signal_a_worker() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(JobStore::new(temp.path()));
        let mut pending = record("pending-cancel");
        pending.job.state = JobState::Pending;
        store.admit(&pending).unwrap();
        store.admit(&record("ready-cancel")).unwrap();

        let sink = Arc::new(PersistedTransitionSink::new(Arc::clone(&store)));
        let supervisor =
            DurableMcpSupervisor::with_worker_cancellation_sink(Arc::clone(&store), sink.clone());
        for work_id in ["pending-cancel", "ready-cancel"] {
            let receipt = supervisor
                .cancel_job(
                    &context(),
                    WorkId::from_str(work_id),
                    "superseded".to_owned(),
                )
                .await
                .unwrap();
            assert!(!receipt.worker_signal_required);
            assert_eq!(
                receipt.worker_cancellation,
                WorkerCancellationStatus::NotRequired
            );
        }
        assert!(sink.calls().is_empty());
    }

    #[tokio::test]
    async fn running_cancellation_signals_only_after_durable_fence() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(JobStore::new(temp.path()));
        store.admit(&record("running-cancel")).unwrap();
        let now = Timestamp::now();
        store
            .claim(
                &WorkId::from_str("running-cancel"),
                &ClaimRequest {
                    worker_id: "worker-a".to_owned(),
                    lease_ttl: SignedDuration::from_secs(60),
                    now,
                },
            )
            .unwrap();

        let sink = Arc::new(PersistedTransitionSink::new(Arc::clone(&store)));
        let supervisor =
            DurableMcpSupervisor::with_worker_cancellation_sink(Arc::clone(&store), sink.clone());
        let receipt = supervisor
            .cancel_job(
                &context(),
                WorkId::from_str("running-cancel"),
                "operator stopped task".to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.previous_state, JobState::Running);
        assert!(receipt.worker_signal_required);
        assert_eq!(
            receipt.worker_cancellation,
            WorkerCancellationStatus::Requested
        );
        assert_eq!(sink.calls(), vec![WorkId::from_str("running-cancel")]);
    }

    #[tokio::test]
    async fn running_cancellation_reports_unavailable_without_a_live_controller() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(JobStore::new(temp.path()));
        store.admit(&record("running-unavailable")).unwrap();
        store
            .claim(
                &WorkId::from_str("running-unavailable"),
                &ClaimRequest {
                    worker_id: "worker-a".to_owned(),
                    lease_ttl: SignedDuration::from_secs(60),
                    now: Timestamp::now(),
                },
            )
            .unwrap();

        let receipt = DurableMcpSupervisor::new(store)
            .cancel_job(
                &context(),
                WorkId::from_str("running-unavailable"),
                "operator stopped task".to_owned(),
            )
            .await
            .unwrap();

        assert!(receipt.worker_signal_required);
        assert_eq!(
            receipt.worker_cancellation,
            WorkerCancellationStatus::Unavailable
        );
    }

    #[tokio::test]
    async fn running_cancellation_fails_closed_for_an_invalid_sink_status() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(JobStore::new(temp.path()));
        store.admit(&record("running-invalid-status")).unwrap();
        store
            .claim(
                &WorkId::from_str("running-invalid-status"),
                &ClaimRequest {
                    worker_id: "worker-a".to_owned(),
                    lease_ttl: SignedDuration::from_secs(60),
                    now: Timestamp::now(),
                },
            )
            .unwrap();

        let receipt = DurableMcpSupervisor::with_worker_cancellation_sink(
            store,
            Arc::new(InvalidNotRequiredSink),
        )
        .cancel_job(
            &context(),
            WorkId::from_str("running-invalid-status"),
            "operator stopped task".to_owned(),
        )
        .await
        .unwrap();

        assert!(receipt.worker_signal_required);
        assert_eq!(
            receipt.worker_cancellation,
            WorkerCancellationStatus::Unavailable
        );
    }

    #[tokio::test]
    async fn submit_admits_a_ready_job_in_the_authenticated_scope() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(JobStore::new(temp.path()));
        let supervisor = DurableMcpSupervisor::new(Arc::clone(&store));

        let submitted = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Worker,
                    input: serde_json::json!({"task": "durable MCP work"}),
                    budget: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(submitted.kind, "worker");
        assert_eq!(submitted.state, JobState::Ready);
        let stored = store.get(&submitted.work_id).unwrap();
        assert_eq!(stored.scope, scope());
        assert_eq!(
            stored.input,
            serde_json::json!({"task": "durable MCP work"})
        );
        assert_eq!(stored.job.budget, Budget::unbounded());
        assert_eq!(stored.job.retry, default_retry_policy());
    }

    #[tokio::test]
    async fn submit_requires_capability_and_validates_untrusted_fields() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(JobStore::new(temp.path()));
        let supervisor = DurableMcpSupervisor::new(store);

        let unauthorized = supervisor
            .submit_job(
                &context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Dream,
                    input: serde_json::json!({"task": "not allowed"}),
                    budget: None,
                },
            )
            .await;
        assert!(matches!(
            unauthorized,
            Err(McpSupervisorError::NotAuthorized)
        ));

        let invalid_input = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Dream,
                    input: serde_json::json!(["not", "an", "object"]),
                    budget: None,
                },
            )
            .await;
        assert!(matches!(
            invalid_input,
            Err(McpSupervisorError::InvalidSubmission(_))
        ));

        let invalid_budget = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Dream,
                    input: serde_json::json!({"task": "bad budget"}),
                    budget: Some(Budget {
                        max_tokens: Some(0),
                        max_wall: None,
                        max_tool_calls: None,
                    }),
                },
            )
            .await;
        assert!(matches!(
            invalid_budget,
            Err(McpSupervisorError::InvalidSubmission(_))
        ));
    }
}
