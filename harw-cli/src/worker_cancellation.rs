//! Bridge from a persisted, fenced durable cancellation to the in-process
//! execution registry.
//!
//! # Responsibility scope
//! This module owns exactly one thing: delivering the in-process cancellation
//! signal for a lease the durable store has *already* fenced. It never mutates
//! durable state, and it never routes by holder, session, or worker name — the
//! only routing authority is the exact [`LeaseToken`] carried by the prior
//! [`Lease`]. A stale `epoch` or `nonce` therefore never reaches a current
//! execution.
//!
//! # Key type
//! [`RegistryWorkerCancellationSink`] implements
//! [`harw_mcp_server::WorkerCancellationSink`] over a shared
//! [`harw_core::JobExecutionRegistry`]. It performs a cheap synchronous
//! `contains` probe to answer `Requested` vs `Unavailable`, then detaches the
//! cooperative-then-forced cancel so the MCP response never blocks for the
//! grace period.
//!
//! # Concurrency
//! The sink must be invoked from within a Tokio runtime, because it detaches
//! the bounded cancel via [`tokio::spawn`]. The MCP `cancel_job` call site
//! guarantees this: it runs on the CLI's current-thread runtime, so an ambient
//! runtime is always present when the sink is called.
//!
//! # Errors
//! None are surfaced. Delivery is best-effort: a missing in-memory binding
//! (for example after a process restart, where the durable fence outlives the
//! in-memory execution) is deliberately non-fatal and reported as
//! [`WorkerCancellationStatus::Unavailable`].
//!
//! # Examples
//! ```ignore
//! use std::sync::Arc;
//!
//! use harw_core::JobExecutionRegistry;
//!
//! // `harw-cli` is a binary crate, so this internal module is not importable
//! // as a library path; the sink is constructed at the CLI composition root.
//! let _sink = RegistryWorkerCancellationSink::new(Arc::new(JobExecutionRegistry::new()));
//! ```

use std::sync::Arc;
use std::time::Duration;

use harw_core::JobExecutionRegistry;
use harw_job_runtime::Lease;
use harw_mcp_server::{WorkerCancellationSink, WorkerCancellationStatus};
use harw_types::WorkId;

/// Default cooperative grace before the registry force-aborts a worker.
///
/// # Description
/// The bounded window handed to [`JobExecutionRegistry::cancel`] between the
/// cooperative `request_graceful_cancel` and the forced `force_abort`. This is
/// the value used by [`RegistryWorkerCancellationSink::new`]; callers that need
/// a different bound construct the sink with
/// [`RegistryWorkerCancellationSink::with_grace`].
pub const DEFAULT_WORKER_CANCELLATION_GRACE: Duration = Duration::from_secs(10);

/// In-process delivery of a durable, fenced cancellation to a live execution.
///
/// # Description
/// Wraps a shared [`JobExecutionRegistry`] and forwards a persisted
/// cancellation to whatever execution is bound to the prior lease's exact
/// fencing token. Holds only the shared registry handle and the grace bound;
/// it never owns or infers a process identifier.
///
/// # Concurrency
/// Cheaply cloneable through [`Arc`]. `request_worker_cancellation` must run
/// inside a Tokio runtime because it detaches the bounded cancel via
/// [`tokio::spawn`].
pub struct RegistryWorkerCancellationSink {
    executions: Arc<JobExecutionRegistry>,
    grace: Duration,
}

impl RegistryWorkerCancellationSink {
    /// Constructs a sink using [`DEFAULT_WORKER_CANCELLATION_GRACE`].
    ///
    /// # Arguments
    /// - `executions` (`Arc<JobExecutionRegistry>`): the shared registry that a
    ///   job producer registers live executions into. Shared ownership is
    ///   transferred to the sink.
    ///
    /// # Returns
    /// A [`RegistryWorkerCancellationSink`] whose grace is the module default.
    #[must_use]
    pub fn new(executions: Arc<JobExecutionRegistry>) -> Self {
        Self::with_grace(executions, DEFAULT_WORKER_CANCELLATION_GRACE)
    }

    /// Constructs a sink with an explicit cooperative grace period.
    ///
    /// # Arguments
    /// - `executions` (`Arc<JobExecutionRegistry>`): the shared registry that a
    ///   job producer registers live executions into. Shared ownership is
    ///   transferred to the sink.
    /// - `grace` (`Duration`): the cooperative window before the registry
    ///   force-aborts an unresponsive worker.
    ///
    /// # Returns
    /// A [`RegistryWorkerCancellationSink`] bound to `grace`.
    #[must_use]
    pub fn with_grace(executions: Arc<JobExecutionRegistry>, grace: Duration) -> Self {
        Self { executions, grace }
    }
}

impl WorkerCancellationSink for RegistryWorkerCancellationSink {
    /// Forwards a persisted, fenced cancellation to the live execution bound to
    /// the prior lease's exact token.
    ///
    /// # Description
    /// Probes the registry for the prior lease's fencing token. If no live
    /// execution is bound, returns [`WorkerCancellationStatus::Unavailable`].
    /// Otherwise it detaches the registry's own cooperative-then-forced
    /// `cancel` (bounded by the configured grace) via [`tokio::spawn`] and
    /// returns [`WorkerCancellationStatus::Requested`] immediately, so the MCP
    /// response is not held for the grace period. `work_id` is intentionally
    /// unused: the durable side has already fenced this lease, and the token is
    /// the sole routing authority. The token is never logged or serialized.
    ///
    /// # Arguments
    /// - `_work_id` (`&WorkId`): the cancelled job's identity. Unused; the
    ///   token derived from `prior_lease` is authoritative.
    /// - `prior_lease` (`&Lease`): the running lease the store fenced before
    ///   this call. Its token identifies the exact live execution to stop.
    ///
    /// # Returns
    /// [`WorkerCancellationStatus::Requested`] when a live execution was found
    /// and its bounded cancel was detached; [`WorkerCancellationStatus::Unavailable`]
    /// when no execution is bound to the exact token.
    ///
    /// # Concurrency
    /// Must be called inside a Tokio runtime; spawns a detached task that
    /// awaits [`JobExecutionRegistry::cancel`]. Does not block on the grace
    /// period.
    fn request_worker_cancellation(
        &self,
        _work_id: &WorkId,
        prior_lease: &Lease,
    ) -> WorkerCancellationStatus {
        // The only routing authority is the exact fencing token, never the
        // holder/worker name. The durable store has already fenced this lease
        // before we are called; we never touch durable state here.
        let token = prior_lease.token();
        if !self.executions.contains(&token) {
            return WorkerCancellationStatus::Unavailable;
        }
        // Detach the cooperative-then-forced cancellation so the MCP response
        // does not block for the grace period. The registry runs its own
        // bounded grace + force_abort; a Missing race is deliberately non-fatal.
        let executions = Arc::clone(&self.executions);
        let grace = self.grace;
        tokio::spawn(async move {
            let _ = executions.cancel(&token, grace).await;
        });
        WorkerCancellationStatus::Requested
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use harw_core::{DurableJobRunner, ExecutionControl};
    use harw_job_runtime::{Budget, Job, JobKind, JobOutcome, JobScope, RetryPolicy, StoredJob};
    use harw_session_store::{CancelRequest, ClaimRequest, JobStore};
    use harw_types::{ApprovalActor, TenantId, WorkspaceId};
    use jiff::{SignedDuration, Timestamp};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{oneshot, watch};

    #[tokio::test]
    async fn unavailable_when_no_execution_is_bound() {
        let executions = Arc::new(JobExecutionRegistry::new());
        let sink = RegistryWorkerCancellationSink::new(executions);
        let now = Timestamp::now();
        let lease = Lease::acquire_fenced(
            WorkId::from_str("w"),
            "holder",
            now,
            SignedDuration::from_secs(60),
            1,
            "nonce",
        )
        .unwrap();
        assert_eq!(
            sink.request_worker_cancellation(&WorkId::from_str("w"), &lease),
            WorkerCancellationStatus::Unavailable
        );
    }

    struct FakeControl {
        completion: watch::Sender<bool>,
        receiver: watch::Receiver<bool>,
        graceful: AtomicUsize,
        forced: AtomicUsize,
    }

    impl FakeControl {
        fn new() -> Arc<Self> {
            let (completion, receiver) = watch::channel(false);
            Arc::new(Self {
                completion,
                receiver,
                graceful: AtomicUsize::new(0),
                forced: AtomicUsize::new(0),
            })
        }
    }

    impl ExecutionControl for FakeControl {
        fn request_graceful_cancel(&self) {
            self.graceful.fetch_add(1, Ordering::SeqCst);
            self.completion.send_replace(true);
        }

        fn force_abort(&self) {
            self.forced.fetch_add(1, Ordering::SeqCst);
            self.completion.send_replace(true);
        }

        fn completion(&self) -> watch::Receiver<bool> {
            self.receiver.clone()
        }
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
        job.mark_ready(now).expect("new job is pending");
        StoredJob {
            job,
            scope: JobScope::new(
                TenantId::from_str("tenant"),
                WorkspaceId::from_str("workspace"),
                ApprovalActor::Operator {
                    id: "operator".to_owned(),
                },
            ),
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

    #[tokio::test]
    async fn durable_cancellation_reaches_registered_execution() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(JobStore::new(temp.path()));
        store.admit(&record("work-cancel")).unwrap();
        let work_id = WorkId::from_str("work-cancel");
        let executions = Arc::new(JobExecutionRegistry::new());
        let runner = DurableJobRunner::new(store.clone(), executions.clone());

        let control = FakeControl::new();
        let operation_control = control.clone();
        let (claimed, claimed_rx) = oneshot::channel();
        let run_work_id = work_id.clone();
        let run = tokio::spawn(async move {
            runner
                .run(
                    &run_work_id,
                    &ClaimRequest {
                        worker_id: "runner".to_owned(),
                        lease_ttl: SignedDuration::from_secs(60),
                        now: Timestamp::now(),
                    },
                    move |claim| {
                        let _ = claimed.send(claim.token.clone());
                        let mut done = operation_control.completion();
                        (operation_control as Arc<dyn ExecutionControl>, async move {
                            while !*done.borrow() {
                                if done.changed().await.is_err() {
                                    break;
                                }
                            }
                            JobOutcome::Succeeded {
                                result: serde_json::json!({"late": true}),
                            }
                        })
                    },
                )
                .await
        });

        let _token = claimed_rx.await.expect("claim reached operation");
        let transition = store
            .cancel(
                &work_id,
                &CancelRequest {
                    cancelled_at: Timestamp::now(),
                    cancelled_by: ApprovalActor::Operator {
                        id: "operator".to_owned(),
                    },
                    reason: "operator stop".to_owned(),
                },
            )
            .expect("durable cancellation");
        let prior = transition
            .prior_lease
            .as_ref()
            .expect("running lease was fenced");

        let sink =
            RegistryWorkerCancellationSink::with_grace(executions.clone(), Duration::from_secs(1));
        let status = sink.request_worker_cancellation(&transition.work_id, prior);
        assert_eq!(status, WorkerCancellationStatus::Requested);

        let mut waited = 0;
        while control.graceful.load(Ordering::SeqCst) == 0 && waited < 200 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            waited += 1;
        }
        assert_eq!(control.graceful.load(Ordering::SeqCst), 1);

        let _ = run.await;
    }
}
