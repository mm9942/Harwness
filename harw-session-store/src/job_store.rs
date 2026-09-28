//! Durable, fenced job persistence for worker and MCP orchestration.
//!
//! Thin Harwness adapter over `harw-job-store` (Eco-Doc §33): the generic
//! file mechanics — `records/<id>.json`, `locks/<id>.lock`, per-record fs4
//! try-locks, atomic temp + rename + parent-directory fsync, quarantine of
//! corrupt records, paginated listing — and the pure lease/fencing mutations
//! (claim, renew, complete, cancel, reclaim, expiry) live there. This module
//! keeps the Harwness semantics: the public request/response types, the
//! trusted-actor provenance (`ApprovalActor`), approval sidecars under
//! `jobs/approvals/`, `unblock`/`deny_blocked`/`retry`/`mark_ready`, and the
//! redacted [`JobLifecycleEvent`] publication. Public API and on-disk layout
//! and format are unchanged, so existing data stays readable.
//!
//! Every mutation verifies the current lease token and fencing epoch, so an
//! expired or restarted worker cannot overwrite a job reclaimed by another
//! worker. Jede Mutation (Claim, Renew, Complete, Cancel, Reconcile) synct
//! nach dem atomaren Ersetzen zusätzlich das Verzeichnis des
//! Job-Datensatzes; ein hier verlorener Fortschritt hieße stillen Stillstand
//! statt eines bloß veralteten Zustands.
//!
//! A-STORE (G-020, F-179): ein defekter `*.json`-Datensatz legt `list` nicht
//! mehr lahm — er wird (unter seinem Record-Lock) nach
//! `<id>.json.corrupt-<ts>` verschoben und übersprungen. `admit` schreibt
//! ohne Überschreiben (no-clobber). `reconcile_expired` arbeitet seitenweise
//! (`limit`/`cursor`) und überspringt einzelne gesperrte/defekte Jobs, statt
//! den ganzen Lauf abzubrechen. `unblock` führt einen `Blocked`-Job
//! (Approval-Pause) nach `Ready` zurück. `mark_ready` gibt einen
//! `Pending`-Job frei (`Pending → Ready`, idempotent für `Ready`); `reclaim`
//! holt einen einzelnen `Running`-Job wie der Massen-Reclaim zurück.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_job_core::{
    JobCancellation, JobClaim, JobCompletion, JobOutcome, JobRuntimeError, JobState, Lease,
    LeaseToken, StoredJob,
};
use harw_job_store::{
    Conflict, LEASE_EXPIRED_EXHAUSTED_REASON, RecordStore, StaleLease, StoreError, mechanics,
    validate_id,
};
use harw_types::{ApprovalActor, WorkId};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::error::{SessionStoreError, SessionStoreResult};

/// Sidecar kind (`jobs/approvals/<id>.json`) of [`JobApproval`] records.
const APPROVALS_KIND: &str = "approvals";

/// Reason recorded when an explicit [`JobStore::reclaim`] exhausts the retry
/// budget (unchanged wire text).
const RECLAIM_EXHAUSTED_REASON: &str = "job reclaimed and retry budget is exhausted";

/// Query for an eventually-consistent job list snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobListQuery {
    pub states: Option<Vec<JobState>>,
    pub holder: Option<String>,
    pub limit: usize,
    pub cursor: Option<WorkId>,
}

impl Default for JobListQuery {
    fn default() -> Self {
        Self {
            states: None,
            holder: None,
            limit: 50,
            cursor: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobPage {
    pub jobs: Vec<StoredJob>,
    pub next_cursor: Option<WorkId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaimRequest {
    pub worker_id: String,
    pub lease_ttl: SignedDuration,
    pub now: Timestamp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenewalRequest {
    pub token: LeaseToken,
    pub now: Timestamp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompleteRequest {
    pub token: LeaseToken,
    pub completed_at: Timestamp,
    pub outcome: JobOutcome,
}

/// A cancellation command accepted only from a trusted authority boundary.
///
/// The request must be constructed from an authenticated operator or paired
/// channel peer; raw model tool arguments are not a valid source for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelRequest {
    pub cancelled_at: Timestamp,
    pub cancelled_by: ApprovalActor,
    pub reason: String,
}

/// Atomic cancellation result, including the old worker lease that an outer
/// supervisor should signal after the durable fence has been persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CancellationTransition {
    pub work_id: WorkId,
    pub previous_state: JobState,
    pub prior_lease: Option<Lease>,
    pub completion: JobCompletion,
    pub revision: u64,
}

/// A requeue command accepted only from a trusted authority boundary, mirroring
/// [`CancelRequest`]'s provenance contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryRequest {
    pub retried_at: Timestamp,
    pub retried_by: ApprovalActor,
}

/// Durable record of who approved a `Blocked → Ready` transition and why.
///
/// # Beschreibung
/// [`StoredJob`] (`harw-job-core`) trägt kein Akteurs-/Freitextfeld für
/// eine Freigabe — dessen Schema liegt außerhalb dieser Crate. Diese Struktur
/// wird deshalb als eigenständiger Sidecar-Datensatz unter `jobs/approvals/`
/// persistiert, adressiert über dieselbe `WorkId` wie der Job-Datensatz
/// selbst, statt den Job-Datensatz-Typ zu erweitern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobApproval {
    pub work_id: WorkId,
    pub approved_at: Timestamp,
    pub approved_by: ApprovalActor,
    /// Optional operator free text, e.g. why the job was approved.
    pub note: Option<String>,
    /// The job record revision produced by the unblocking transition, so a
    /// reader can correlate this sidecar with the exact job state it approved.
    pub revision: u64,
}

/// One page of [`JobStore::reconcile_expired`].
///
/// `expired` holds the leases revoked on this page; `next_cursor` is the
/// scan position to pass as `cursor` for the next page (`None` = done). The
/// cursor advances over *scanned* Running jobs, not only revoked ones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReconcilePage {
    pub expired: Vec<ExpiredJob>,
    pub next_cursor: Option<WorkId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpiredJob {
    pub work_id: WorkId,
    pub expired_lease: Lease,
    pub reclaimed_at: Timestamp,
    pub retry_scheduled_for: Option<Timestamp>,
}

/// Redacted lifecycle notification emitted after a durable job mutation.
///
/// This deliberately contains no input, outcome payload, lease, worker
/// identity, or authority data. Consumers may use it to advance an event
/// cursor or wake a bounded transport without becoming a second source of
/// truth for job state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobLifecycleEvent {
    pub work_id: WorkId,
    pub state: JobState,
    pub revision: u64,
}

/// Composition-boundary publication hook for durable lifecycle events.
pub trait JobEventSink: Send + Sync {
    fn publish(&self, event: JobLifecycleEvent);
}

#[derive(Debug, Default)]
pub struct NoopJobEventSink;

impl JobEventSink for NoopJobEventSink {
    fn publish(&self, _event: JobLifecycleEvent) {}
}

/// File-backed job records. Lock granularity is one job, allowing unrelated
/// workers to claim or finish different jobs concurrently.
pub struct JobStore {
    root: PathBuf,
    event_sink: Arc<dyn JobEventSink>,
}

impl JobStore {
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.join("jobs"),
            event_sink: Arc::new(NoopJobEventSink),
        }
    }

    /// Constructs a store with a composition-provided lifecycle publisher.
    /// Publication happens only after the record has been durably replaced;
    /// sink behavior can never change the mutation result.
    #[must_use]
    pub fn new_with_event_sink(root: &Path, event_sink: Arc<dyn JobEventSink>) -> Self {
        Self {
            root: root.join("jobs"),
            event_sink,
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Persists a server-resolved job request. Existing work IDs are never
    /// overwritten, even if an earlier record has reached a terminal state.
    pub fn admit(&self, record: &StoredJob) -> SessionStoreResult<()> {
        // F-179: atomar und ohne Überschreiben — ein Absturz hinterlässt nie
        // eine halbe Datei, ein paralleler Eintrag wird nie ersetzt.
        self.store()?
            .create(record)
            .map_err(SessionStoreError::from)
    }

    pub fn get(&self, work_id: &WorkId) -> SessionStoreResult<StoredJob> {
        validate_id(work_id.as_str())?;
        match self.existing_store()? {
            Some(store) => store
                .load(work_id.as_str())
                .map_err(SessionStoreError::from),
            None => Err(SessionStoreError::JobNotFound {
                work_id: work_id.clone(),
            }),
        }
    }

    /// Lists an eventually-consistent snapshot. Point mutations take a
    /// per-record lock; list readers deliberately do not serialize every job.
    pub fn list(&self, query: &JobListQuery) -> SessionStoreResult<JobPage> {
        let Some(store) = self.existing_store()? else {
            return Ok(JobPage {
                jobs: Vec::new(),
                next_cursor: None,
            });
        };
        let page = store.list(
            query.cursor.as_ref().map(WorkId::as_str),
            query.limit,
            |record| {
                query
                    .states
                    .as_ref()
                    .is_none_or(|states| states.contains(&record.job.state))
                    && query.holder.as_ref().is_none_or(|holder| {
                        record.lease.as_ref().map(|lease| &lease.holder) == Some(holder)
                    })
            },
        )?;
        Ok(JobPage {
            jobs: page.records,
            next_cursor: page.next_cursor.map(WorkId::from_str),
        })
    }

    /// Atomically acquires a new fenced lease for a Ready, eligible job.
    pub fn claim(&self, work_id: &WorkId, request: &ClaimRequest) -> SessionStoreResult<JobClaim> {
        if request.lease_ttl <= SignedDuration::ZERO {
            return Err(SessionStoreError::InvalidJobLeaseTtl);
        }
        let (claim, revision) = self.with_locked(work_id, |record| {
            let nonce = WorkId::new().as_str().to_owned();
            let claim = mechanics::claim(
                record,
                &request.worker_id,
                request.lease_ttl,
                request.now,
                nonce,
            )?;
            Ok((claim, record.revision))
        })?;
        self.publish(JobLifecycleEvent {
            work_id: claim.job.id.clone(),
            state: claim.job.state,
            revision,
        });
        Ok(claim)
    }

    pub fn renew(&self, request: &RenewalRequest) -> SessionStoreResult<Lease> {
        let work_id = request.token.work_id.clone();
        self.with_locked(&work_id, |record| {
            mechanics::renew(record, &request.token, request.now).map_err(SessionStoreError::from)
        })
    }

    /// Completes a Running job exactly once with the current non-expired lease.
    pub fn complete(
        &self,
        work_id: &WorkId,
        request: &CompleteRequest,
    ) -> SessionStoreResult<JobCompletion> {
        if request.token.work_id != *work_id {
            return Err(SessionStoreError::LeaseTokenMismatch {
                work_id: work_id.clone(),
            });
        }
        let (completion, revision) = self.with_locked(work_id, |record| {
            let completion = mechanics::complete(
                record,
                &request.token,
                request.completed_at,
                request.outcome.clone(),
            )?;
            Ok((completion, record.revision))
        })?;
        self.publish(JobLifecycleEvent {
            work_id: work_id.clone(),
            state: mechanics::outcome_state(&request.outcome),
            revision,
        });
        Ok(completion)
    }

    /// Atomically cancels an unclaimed or running job.
    ///
    /// A running job's lease epoch is advanced before the old lease is
    /// returned, so a zombie worker cannot renew or complete after external
    /// cancellation signalling begins.
    pub fn cancel(
        &self,
        work_id: &WorkId,
        request: &CancelRequest,
    ) -> SessionStoreResult<CancellationTransition> {
        if request.reason.trim().is_empty() {
            return Err(SessionStoreError::InvalidJobCancellationReason);
        }
        let transition = self.with_locked(work_id, |record| {
            let cancelled = mechanics::cancel(
                record,
                JobCancellation {
                    cancelled_at: request.cancelled_at,
                    cancelled_by: request.cancelled_by.clone(),
                    reason: request.reason.clone(),
                },
            )?;
            Ok(CancellationTransition {
                work_id: work_id.clone(),
                previous_state: cancelled.previous_state,
                prior_lease: cancelled.prior_lease,
                completion: cancelled.completion,
                revision: record.revision,
            })
        })?;
        self.publish(JobLifecycleEvent {
            work_id: transition.work_id.clone(),
            state: JobState::Cancelled,
            revision: transition.revision,
        });
        Ok(transition)
    }

    /// Returns a `Blocked` job (e.g. paused for an approval) to `Ready`,
    /// recording who approved it and an optional note as a durable sidecar.
    ///
    /// # Description
    /// A-STORE (G-020): under the per-record lock the job must be
    /// [`JobState::Blocked`]; it becomes `Ready`, its `updated_at` is `now`,
    /// the blocking completion and any stale lease are cleared and the
    /// revision is bumped. `not_before`, attempts and the lease epoch stay
    /// unchanged. A redacted lifecycle event is published after the durable
    /// write. Once the job record is durable, `actor` and `note` are written
    /// to a [`JobApproval`] sidecar (see [`JobStore::get_approval`]) keyed by
    /// the same `work_id` — [`StoredJob`] itself carries no actor/note field
    /// (that shape lives in `harw-job-core`, outside this crate).
    ///
    /// # Arguments
    /// - `work_id` (`&WorkId`): the blocked job.
    /// - `now` (`Timestamp`): server time of the transition (injected).
    /// - `actor` (`ApprovalActor`): the trusted approving identity.
    /// - `note` (`Option<String>`): optional free text explaining the approval.
    ///
    /// # Returns
    /// The published [`JobLifecycleEvent`] (`state == Ready`).
    ///
    /// # Errors
    /// - [`SessionStoreError::JobNotBlocked`]: the job is in another state.
    /// - [`SessionStoreError::JobNotFound`], [`SessionStoreError::CorruptJob`]
    ///   (the record is quarantined), [`SessionStoreError::JobLockContended`],
    ///   [`SessionStoreError::UnsafeJobPath`], [`SessionStoreError::Io`],
    ///   [`SessionStoreError::Serde`] (approval sidecar write failure — the
    ///   job's own `Blocked → Ready` transition has already been persisted by
    ///   the time this can occur and is not rolled back).
    ///
    /// # Concurrency
    /// Takes the job's exclusive try-lock; never blocks. The caller is the
    /// trusted approval boundary — model input is not a valid source.
    pub fn unblock(
        &self,
        work_id: &WorkId,
        now: Timestamp,
        actor: ApprovalActor,
        note: Option<String>,
    ) -> SessionStoreResult<JobLifecycleEvent> {
        let event = self.with_locked(work_id, |record| {
            if record.job.state != JobState::Blocked {
                return Err(SessionStoreError::JobNotBlocked {
                    work_id: work_id.clone(),
                    state: record.job.state,
                });
            }
            record.job.state = JobState::Ready;
            record.job.updated_at = now;
            record.lease = None;
            record.completion = None;
            record.revision = record.revision.saturating_add(1);
            Ok(JobLifecycleEvent {
                work_id: work_id.clone(),
                state: JobState::Ready,
                revision: record.revision,
            })
        })?;
        self.persist_approval(&JobApproval {
            work_id: work_id.clone(),
            approved_at: now,
            approved_by: actor,
            note,
            revision: event.revision,
        })?;
        self.publish(event.clone());
        Ok(event)
    }

    /// Reads the durable [`JobApproval`] sidecar for `work_id`, if one exists.
    ///
    /// # Returns
    /// `Some(approval)` from the most recent [`JobStore::unblock`] call for
    /// this `work_id`, or `None` if the job has never been approved.
    ///
    /// # Errors
    /// [`SessionStoreError::Io`] or [`SessionStoreError::Serde`] if the
    /// sidecar exists but cannot be read or decoded.
    ///
    /// # Concurrency
    /// Lock-free read, like [`JobStore::list`]; may race a concurrent
    /// [`JobStore::unblock`] and observe either the old or the new sidecar.
    pub fn get_approval(&self, work_id: &WorkId) -> SessionStoreResult<Option<JobApproval>> {
        validate_id(work_id.as_str())?;
        let Some(store) = self.existing_store()? else {
            return Ok(None);
        };
        match store.read_sidecar(APPROVALS_KIND, work_id.as_str())? {
            Some(bytes) => serde_json::from_slice::<JobApproval>(&bytes)
                .map(Some)
                .map_err(SessionStoreError::from),
            None => Ok(None),
        }
    }

    /// Denies a job that is `Blocked` awaiting an approval, transitioning it
    /// to `Cancelled`.
    ///
    /// # Description
    /// A-STORE: [`JobStore::cancel`] explicitly excludes [`JobState::Blocked`]
    /// as a source state, so a job paused on an open approval request cannot
    /// be denied through it. This transition covers exactly that case: the
    /// source state must be `Blocked`; every other state is rejected with
    /// [`SessionStoreError::JobNotDeniable`] (callers should fall back to
    /// [`JobStore::cancel`] for those). The recorded outcome, cancellation
    /// provenance and revision bump mirror `cancel` exactly, so both paths
    /// produce an indistinguishable terminal shape.
    ///
    /// # Fencing
    /// A `Blocked` job's lease is already cleared by the time it reaches
    /// `Blocked` (see [`JobStore::complete`] with a `Blocked` outcome), so
    /// there is normally no active lease to fence. This still defensively
    /// mirrors [`JobStore::cancel`]'s convention: if a lease is nonetheless
    /// present, the fencing epoch is advanced before it is discarded, so a
    /// stale holder's token can never validate against a future claim.
    ///
    /// # Arguments
    /// - `work_id` (`&WorkId`): the blocked job.
    /// - `request` (`&CancelRequest`): denial timestamp, trusted actor and a
    ///   non-empty reason (reused from [`JobStore::cancel`]'s contract).
    ///
    /// # Returns
    /// The [`CancellationTransition`], `previous_state == Blocked`.
    ///
    /// # Errors
    /// - [`SessionStoreError::InvalidJobCancellationReason`]: empty reason.
    /// - [`SessionStoreError::JobNotDeniable`]: the job is not `Blocked`.
    /// - [`SessionStoreError::JobLeaseEpochExhausted`], plus the same
    ///   record-access errors as [`JobStore::cancel`].
    ///
    /// # Concurrency
    /// Takes the job's exclusive try-lock; never blocks. The caller is the
    /// trusted approval boundary — model input is not a valid source.
    pub fn deny_blocked(
        &self,
        work_id: &WorkId,
        request: &CancelRequest,
    ) -> SessionStoreResult<CancellationTransition> {
        if request.reason.trim().is_empty() {
            return Err(SessionStoreError::InvalidJobCancellationReason);
        }
        let transition = self.with_locked(work_id, |record| {
            let previous_state = record.job.state;
            if previous_state != JobState::Blocked {
                return Err(SessionStoreError::JobNotDeniable {
                    work_id: work_id.clone(),
                    state: previous_state,
                });
            }

            let prior_lease = record.lease.take();
            if prior_lease.is_some() {
                record.lease_epoch = record.lease_epoch.checked_add(1).ok_or_else(|| {
                    SessionStoreError::JobLeaseEpochExhausted {
                        work_id: work_id.clone(),
                    }
                })?;
            }
            let completion = JobCompletion {
                completed_at: request.cancelled_at,
                outcome: JobOutcome::Cancelled {
                    reason: request.reason.clone(),
                },
            };
            record.job.state = JobState::Cancelled;
            record.job.updated_at = request.cancelled_at;
            record.completion = Some(completion.clone());
            record.cancellation = Some(JobCancellation {
                cancelled_at: request.cancelled_at,
                cancelled_by: request.cancelled_by.clone(),
                reason: request.reason.clone(),
            });
            record.revision = record.revision.saturating_add(1);
            Ok(CancellationTransition {
                work_id: work_id.clone(),
                previous_state,
                prior_lease,
                completion,
                revision: record.revision,
            })
        })?;
        self.publish(JobLifecycleEvent {
            work_id: transition.work_id.clone(),
            state: JobState::Cancelled,
            revision: transition.revision,
        });
        Ok(transition)
    }

    /// Requeues a terminally `Failed` or `Cancelled` job for another attempt,
    /// respecting the job's own [`harw_job_core::RetryPolicy`] ceiling.
    ///
    /// # Description
    /// The source state must be [`JobState::Failed`] or
    /// [`JobState::Cancelled`]; any other state is rejected with
    /// [`SessionStoreError::JobNotRetryable`]. If `job.attempts` has already
    /// reached `job.retry.max_attempts`, the job is **not** requeued —
    /// [`SessionStoreError::JobRetryLimitExhausted`] is returned instead, so a
    /// caller can never silently retry past its own configured ceiling.
    /// Otherwise `attempts` is incremented, the job becomes `Ready`,
    /// `not_before` is set to `now` (immediately eligible — this is an
    /// explicit, operator-triggered requeue, not the backoff schedule used by
    /// [`JobStore::reconcile_expired`]), and any stale completion/cancellation
    /// record is cleared so [`JobStore::get`]/`review` reflect the fresh
    /// attempt, not the prior terminal one.
    ///
    /// # Fencing
    /// Mirrors [`JobStore::cancel`]'s convention: the fencing epoch is always
    /// advanced past whatever epoch the job carried in its prior life, so a
    /// worker holding a token from before the retry — even one for a lease
    /// that was already logically cleared — can never have it match a future
    /// claim.
    ///
    /// # Arguments
    /// - `work_id` (`&WorkId`): the job to requeue.
    /// - `request` (`&RetryRequest`): requeue timestamp and trusted actor.
    ///
    /// # Returns
    /// The published [`JobLifecycleEvent`] (`state == Ready`).
    ///
    /// # Errors
    /// - [`SessionStoreError::JobNotRetryable`]: the job is neither `Failed`
    ///   nor `Cancelled`.
    /// - [`SessionStoreError::JobRetryLimitExhausted`]: the retry policy has
    ///   no attempts left.
    /// - [`SessionStoreError::JobLeaseEpochExhausted`], plus the same
    ///   record-access errors as [`JobStore::cancel`].
    ///
    /// # Concurrency
    /// Takes the job's exclusive try-lock; never blocks. The caller is the
    /// trusted approval boundary — model input is not a valid source.
    pub fn retry(
        &self,
        work_id: &WorkId,
        request: &RetryRequest,
    ) -> SessionStoreResult<JobLifecycleEvent> {
        let event = self.with_locked(work_id, |record| {
            let previous_state = record.job.state;
            if !matches!(previous_state, JobState::Failed | JobState::Cancelled) {
                return Err(SessionStoreError::JobNotRetryable {
                    work_id: work_id.clone(),
                    state: previous_state,
                });
            }
            if record.job.attempts >= record.job.retry.max_attempts {
                return Err(SessionStoreError::JobRetryLimitExhausted {
                    work_id: work_id.clone(),
                    attempts: record.job.attempts,
                    max_attempts: record.job.retry.max_attempts,
                });
            }

            // Defense in depth: advance the fencing epoch on every retry, even
            // though a Failed/Cancelled job's lease is already cleared by the
            // transition that got it there — mirrors `cancel`'s convention so
            // no token from a prior life of this job can ever validate again.
            record.lease = None;
            record.lease_epoch = record.lease_epoch.saturating_add(1);

            record.job.attempts = record.job.attempts.saturating_add(1);
            record.job.state = JobState::Ready;
            record.job.updated_at = request.retried_at;
            record.not_before = request.retried_at;
            record.completion = None;
            record.cancellation = None;
            record.revision = record.revision.saturating_add(1);
            // `RetryRequest::retried_by` has no durable home on `StoredJob`
            // (same constraint as `JobApproval`, see `unblock`'s module doc);
            // it is logged so the requeue still has an audit trail.
            tracing::info!(
                work_id = %work_id,
                retried_by = ?request.retried_by,
                attempts = record.job.attempts,
                max_attempts = record.job.retry.max_attempts,
                "job requeued for retry"
            );
            Ok(JobLifecycleEvent {
                work_id: work_id.clone(),
                state: JobState::Ready,
                revision: record.revision,
            })
        })?;
        self.publish(event.clone());
        Ok(event)
    }

    /// Gibt einen `Pending`-Job für einen Worker-Claim frei (`Pending → Ready`).
    ///
    /// # Beschreibung
    /// Unter der Datensatz-Sperre validiert [`harw_job_core::Job::mark_ready`]
    /// den Übergang (Zustandsmaschine aus `harw-job-core`); der Job wird
    /// `Ready`, `updated_at` wird `now`, die Revision steigt, und nach dem
    /// durablen Schreiben wird ein redigiertes [`JobLifecycleEvent`]
    /// veröffentlicht. `not_before`, Versuche, Lease und Fencing-Epoch bleiben
    /// unverändert.
    ///
    /// Idempotent: ein bereits `Ready` stehender Job bleibt unverändert
    /// (keine Revision, kein Event); zurückgegeben wird dann sein aktueller
    /// Stand.
    ///
    /// # Argumente
    /// - `work_id` (`&WorkId`): der freizugebende Job.
    /// - `now` (`Timestamp`): Serverzeit des Übergangs (injiziert).
    ///
    /// # Rückgabe
    /// Das [`JobLifecycleEvent`] mit `state == Ready` und der aktuellen Revision.
    ///
    /// # Fehler
    /// - [`SessionStoreError::JobRuntime`]: der Job ist weder `Pending` noch
    ///   `Ready` (abgelehnt von der Zustandsmaschine, `InvalidState`); nichts
    ///   wird geschrieben.
    /// - [`SessionStoreError::JobNotFound`], [`SessionStoreError::CorruptJob`]
    ///   (der Datensatz wird in Quarantäne verschoben),
    ///   [`SessionStoreError::JobLockContended`],
    ///   [`SessionStoreError::UnsafeJobPath`], [`SessionStoreError::Io`],
    ///   [`SessionStoreError::Serde`].
    ///
    /// # Nebenläufigkeit
    /// Nimmt die exklusive Try-Sperre des Jobs; blockiert nie.
    pub fn mark_ready(
        &self,
        work_id: &WorkId,
        now: Timestamp,
    ) -> SessionStoreResult<JobLifecycleEvent> {
        let (event, changed) = self.with_locked(work_id, |record| {
            if record.job.state == JobState::Ready {
                return Ok((
                    JobLifecycleEvent {
                        work_id: work_id.clone(),
                        state: JobState::Ready,
                        revision: record.revision,
                    },
                    false,
                ));
            }
            record
                .job
                .mark_ready(now)
                .map_err(|error| map_job_error(work_id, error))?;
            record.revision = record.revision.saturating_add(1);
            tracing::info!(
                work_id = %work_id,
                revision = record.revision,
                "job marked ready"
            );
            Ok((
                JobLifecycleEvent {
                    work_id: work_id.clone(),
                    state: JobState::Ready,
                    revision: record.revision,
                },
                true,
            ))
        })?;
        if changed {
            self.publish(event.clone());
        }
        Ok(event)
    }

    /// Holt einen einzelnen `Running`-Job von seinem Halter zurück
    /// (`Running → Ready`, bzw. `Failed` bei erschöpfter Retry-Politik).
    ///
    /// # Beschreibung
    /// Einzel-Gegenstück zu [`JobStore::reconcile_expired`] mit denselben
    /// Schritten je Job: die Lease wird entfernt, die Fencing-Epoch
    /// weitergeschaltet, und [`harw_job_core::Job::record_failure`]
    /// (Zustandsmaschine aus `harw-job-core`) zählt einen Versuch. Bleibt
    /// Budget, wird der Job `Ready` und `not_before` auf `now + Backoff`
    /// gesetzt; ist die Retry-Politik erschöpft, wird er terminal `Failed`
    /// mit einer `JobOutcome::Failed`-Completion — genau wie beim
    /// Massen-Reclaim. Anders als dieser verlangt `reclaim` **keine**
    /// abgelaufene Lease: es ist ein ausdrücklicher, vertrauenswürdiger
    /// Übergang (z. B. ein Operator zieht eine Karte zurück). Die
    /// Fencing-Epoch sorgt dafür, dass der alte Halter danach weder
    /// verlängern noch abschließen kann.
    ///
    /// `reclaimed_by` hat im [`StoredJob`] keinen durablen Platz (gleiche
    /// Einschränkung wie bei [`JobStore::retry`]) und wird deshalb als
    /// Audit-Spur geloggt.
    ///
    /// # Argumente
    /// - `work_id` (`&WorkId`): der laufende Job.
    /// - `now` (`Timestamp`): Serverzeit des Übergangs (injiziert).
    /// - `reclaimed_by` (`ApprovalActor`): die vertrauenswürdige Identität,
    ///   die den Reclaim auslöst.
    ///
    /// # Rückgabe
    /// Ein [`ExpiredJob`] mit der entzogenen Lease; `retry_scheduled_for` ist
    /// `None`, wenn der Job wegen erschöpfter Retry-Politik `Failed` wurde.
    ///
    /// # Fehler
    /// - [`SessionStoreError::JobRuntime`]: der Job ist nicht `Running`
    ///   (`InvalidState` der Zustandsmaschine), trägt trotz `Running` keine
    ///   Lease, oder die Backoff-Zeit ist nicht darstellbar; nichts wird
    ///   geschrieben.
    /// - [`SessionStoreError::JobLeaseEpochExhausted`], plus dieselben
    ///   Datensatz-Zugriffsfehler wie [`JobStore::mark_ready`].
    ///
    /// # Nebenläufigkeit
    /// Nimmt die exklusive Try-Sperre des Jobs; blockiert nie. Der Aufrufer
    /// ist die vertrauenswürdige Autoritätsgrenze — Modell-Eingaben sind
    /// keine zulässige Quelle.
    pub fn reclaim(
        &self,
        work_id: &WorkId,
        now: Timestamp,
        reclaimed_by: ApprovalActor,
    ) -> SessionStoreResult<ExpiredJob> {
        let (reclaimed, state, revision) = self.with_locked(work_id, |record| {
            // Fehler im Closure werden nicht persistiert; die Reihenfolge der
            // Mutationen in `mechanics::reclaim` ist deshalb unkritisch.
            let reclaimed = mechanics::reclaim(record, now, RECLAIM_EXHAUSTED_REASON)?;
            tracing::info!(
                work_id = %work_id,
                reclaimed_by = ?reclaimed_by,
                holder = %reclaimed.expired_lease.holder,
                attempts = record.job.attempts,
                max_attempts = record.job.retry.max_attempts,
                state = ?record.job.state,
                "job reclaimed from its lease holder"
            );
            Ok((
                ExpiredJob {
                    work_id: work_id.clone(),
                    expired_lease: reclaimed.expired_lease,
                    reclaimed_at: now,
                    retry_scheduled_for: reclaimed.retry_scheduled_for,
                },
                record.job.state,
                record.revision,
            ))
        })?;
        self.publish(JobLifecycleEvent {
            work_id: reclaimed.work_id.clone(),
            state,
            revision,
        });
        Ok(reclaimed)
    }

    /// Revokes expired leases once, schedules retry eligibility, and returns
    /// the exact zombie lease the supervisor must cancel/observe.
    ///
    /// # Description
    /// A-STORE (G-020): paginated. One call scans at most `limit` Running jobs
    /// (clamped to `1..=100`, like [`JobStore::list`]) after `cursor`. A job
    /// that is lock-contended, vanished, corrupt (quarantined) or whose retry
    /// computation fails is skipped with a `warn!` instead of aborting the
    /// whole page. Loop until `next_cursor` is `None` to cover every job.
    ///
    /// # Errors
    /// [`SessionStoreError::Io`]/[`SessionStoreError::Serde`] on directory or
    /// persist failures.
    pub fn reconcile_expired(
        &self,
        now: Timestamp,
        limit: usize,
        cursor: Option<&WorkId>,
    ) -> SessionStoreResult<ReconcilePage> {
        let candidates = self.list(&JobListQuery {
            states: Some(vec![JobState::Running]),
            limit,
            cursor: cursor.cloned(),
            ..JobListQuery::default()
        })?;
        let mut expired = Vec::new();
        for record in candidates.jobs {
            let work_id = record.job.id.clone();
            let transition = match self.reconcile_one(&work_id, now) {
                Ok(transition) => transition,
                Err(
                    error @ (SessionStoreError::JobLockContended { .. }
                    | SessionStoreError::JobNotFound { .. }
                    | SessionStoreError::CorruptJob { .. }
                    | SessionStoreError::JobRuntime { .. }),
                ) => {
                    tracing::warn!(
                        work_id = %work_id,
                        error = %error,
                        "expired-lease reconciliation skipped a job"
                    );
                    continue;
                }
                Err(error) => return Err(error),
            };
            if let Some((transition, state, revision)) = transition {
                self.publish(JobLifecycleEvent {
                    work_id: transition.work_id.clone(),
                    state,
                    revision,
                });
                expired.push(transition);
            }
        }
        Ok(ReconcilePage {
            expired,
            next_cursor: candidates.next_cursor,
        })
    }

    // Revokes one expired lease under the record lock (body of the former loop).
    fn reconcile_one(
        &self,
        work_id: &WorkId,
        now: Timestamp,
    ) -> SessionStoreResult<Option<(ExpiredJob, JobState, u64)>> {
        self.with_locked(work_id, |record| {
            let Some(reclaimed) =
                mechanics::expire_if_due(record, now, LEASE_EXPIRED_EXHAUSTED_REASON)?
            else {
                return Ok(None);
            };
            Ok(Some((
                ExpiredJob {
                    work_id: work_id.clone(),
                    expired_lease: reclaimed.expired_lease,
                    reclaimed_at: now,
                    retry_scheduled_for: reclaimed.retry_scheduled_for,
                },
                record.job.state,
                record.revision,
            )))
        })
    }

    // Lock → load → mutate → persist → fsync → unlock, delegated to
    // `harw-job-store`. A corrupt record is quarantined under the lock and
    // reported as `CorruptJob` (G-020).
    fn with_locked<T>(
        &self,
        work_id: &WorkId,
        operation: impl FnOnce(&mut StoredJob) -> SessionStoreResult<T>,
    ) -> SessionStoreResult<T> {
        self.store()?.update_locked(work_id.as_str(), operation)
    }

    fn publish(&self, event: JobLifecycleEvent) {
        self.event_sink.publish(event);
    }

    // Opens (creating if needed) the job root with its `records/` and
    // `locks/` layout. The root is re-opened per operation, exactly like the
    // former path-based code re-resolved it.
    fn store(&self) -> SessionStoreResult<RecordStore<StoredJob>> {
        RecordStore::create_ambient(&self.root).map_err(SessionStoreError::from)
    }

    // Opens the job root for reading; `None` if it was never created.
    fn existing_store(&self) -> SessionStoreResult<Option<RecordStore<StoredJob>>> {
        RecordStore::open_existing_ambient(&self.root).map_err(SessionStoreError::from)
    }

    /// Atomically writes (or overwrites) the [`JobApproval`] sidecar for
    /// `approval.work_id`. Called after the job's own `Blocked → Ready`
    /// transition is already durable, so this failing never leaves the job
    /// record itself inconsistent — only the audit sidecar is affected.
    fn persist_approval(&self, approval: &JobApproval) -> SessionStoreResult<()> {
        let bytes = serde_json::to_vec(approval)?;
        self.store()?
            .write_sidecar(APPROVALS_KIND, approval.work_id.as_str(), &bytes)
            .map_err(SessionStoreError::from)
    }

    #[cfg(test)]
    fn ensure_root(&self) -> SessionStoreResult<()> {
        self.store().map(|_| ())
    }

    #[cfg(test)]
    fn records_dir(&self) -> PathBuf {
        self.root.join(harw_job_store::RECORDS_DIR)
    }

    #[cfg(test)]
    fn locks_dir(&self) -> PathBuf {
        self.root.join(harw_job_store::LOCKS_DIR)
    }

    #[cfg(test)]
    fn record_path(&self, work_id: &WorkId) -> SessionStoreResult<PathBuf> {
        let id = validate_id(work_id.as_str())?;
        Ok(self.records_dir().join(id).with_extension("json"))
    }
}

fn map_job_error(work_id: &WorkId, error: JobRuntimeError) -> SessionStoreError {
    SessionStoreError::from(mechanics::job_error(work_id.as_str(), error))
}

/// Maps the generic store error onto the job variants of
/// [`SessionStoreError`]. `harw-job-store` is only used by this module, so
/// every record id is a job [`WorkId`]; the mapping reproduces exactly the
/// variants the former in-crate implementation returned.
impl From<StoreError> for SessionStoreError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound { id } => Self::JobNotFound {
                work_id: WorkId::from_str(id),
            },
            StoreError::AlreadyExists { id } => Self::JobAlreadyExists {
                work_id: WorkId::from_str(id),
            },
            StoreError::Contended { id } => Self::JobLockContended {
                work_id: WorkId::from_str(id),
            },
            StoreError::InvalidId { id } => Self::UnsafeJobPath(id),
            StoreError::Corrupt { id, detail } => Self::CorruptJob {
                work_id: WorkId::from_str(id),
                detail,
            },
            StoreError::Conflict { id, conflict } => conflict_error(WorkId::from_str(id), conflict),
            StoreError::StaleLease { id, reason } => {
                let work_id = WorkId::from_str(id);
                match reason {
                    StaleLease::Missing | StaleLease::TokenMismatch => {
                        Self::LeaseTokenMismatch { work_id }
                    }
                    StaleLease::Expired { expired_at } => Self::JobLeaseExpired {
                        work_id,
                        expired_at,
                    },
                }
            }
            StoreError::Encode { source, .. } => Self::Serde(source),
            StoreError::Io(error) => Self::Io(error),
            StoreError::Unsupported { operation, detail } => Self::Io(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!("{operation}: {detail}"),
            )),
        }
    }
}

fn conflict_error(work_id: WorkId, conflict: Conflict) -> SessionStoreError {
    match conflict {
        Conflict::NotClaimable { state } => SessionStoreError::JobNotClaimable { work_id, state },
        Conflict::NotEligible { not_before } => SessionStoreError::JobNotEligible {
            work_id,
            not_before,
        },
        Conflict::AlreadyTerminal { state } => {
            SessionStoreError::JobAlreadyTerminal { work_id, state }
        }
        Conflict::NotCancellable { state } => {
            SessionStoreError::JobNotCancellable { work_id, state }
        }
        Conflict::InvalidLeaseTtl => SessionStoreError::InvalidJobLeaseTtl,
        Conflict::EmptyReason => SessionStoreError::InvalidJobCancellationReason,
        Conflict::EpochExhausted => SessionStoreError::JobLeaseEpochExhausted { work_id },
        Conflict::Rejected { detail } => SessionStoreError::JobRuntime { work_id, detail },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::durability::sync_parent_directory;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_job_core::{Budget, Job, JobKind, RetryPolicy};
    use harw_observe::TraceContext;
    use std::sync::{Arc, Mutex, PoisonError};

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<JobLifecycleEvent>>);

    impl JobEventSink for RecordingSink {
        fn publish(&self, event: JobLifecycleEvent) {
            // Trait-Signatur liefert `()`; ein vergifteter Mutex darf in
            // Tests trotzdem nicht paniken, deshalb wird der Guard aus dem
            // `PoisonError` zurückgewonnen statt `.unwrap()` aufzurufen.
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event);
        }
    }

    fn record(id: &str) -> TestResult<StoredJob> {
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
        job.mark_ready(now)
            .map_err(ctx("mark_ready on a fresh job"))?;
        Ok(StoredJob {
            job,
            scope: harw_job_core::JobScope::new(
                harw_types::TenantId::from_str("test-tenant"),
                harw_types::WorkspaceId::from_str("test-workspace"),
                ApprovalActor::Operator {
                    id: "test-operator".to_owned(),
                },
            ),
            input: serde_json::json!({"task": "review"}),
            submitted_at: now,
            not_before: now,
            lease: None,
            lease_epoch: 0,
            completion: None,
            cancellation: None,
            revision: 0,
            trace: None,
        })
    }

    fn sample_trace() -> TraceContext {
        TraceContext {
            trace_id: "a".repeat(32),
            span_id: "b".repeat(16),
            parent_span_id: None,
        }
    }

    #[test]
    fn admitted_job_with_trace_context_roundtrips_through_the_real_store_path() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let admitted = StoredJob {
            trace: Some(sample_trace()),
            ..record("work-traced")?
        };

        store.admit(&admitted)?;

        let persisted = store.get(&WorkId::from_str("work-traced"))?;
        assert_eq!(persisted, admitted);
        assert_eq!(persisted.trace, Some(sample_trace()));
        Ok(())
    }

    #[test]
    fn admitted_job_without_trace_context_roundtrips_through_the_real_store_path() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let admitted = record("work-untraced")?;
        assert_eq!(admitted.trace, None);

        store.admit(&admitted)?;

        let persisted = store.get(&WorkId::from_str("work-untraced"))?;
        assert_eq!(persisted, admitted);
        assert_eq!(persisted.trace, None);
        Ok(())
    }

    #[test]
    fn a_stale_worker_cannot_complete_after_reconciliation_and_reclaim() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        store.admit(&record("work-1")?)?;
        let now = Timestamp::now();
        let first = store.claim(
            &WorkId::from_str("work-1"),
            &ClaimRequest {
                worker_id: "worker-a".to_owned(),
                lease_ttl: SignedDuration::from_secs(1),
                now,
            },
        )?;
        let after_expiry = now
            .checked_add(SignedDuration::from_secs(2))
            .map_err(ctx("now + 2s"))?;
        assert_eq!(
            store
                .reconcile_expired(after_expiry, 100, None)?
                .expired
                .len(),
            1
        );
        let second = store.claim(
            &WorkId::from_str("work-1"),
            &ClaimRequest {
                worker_id: "worker-b".to_owned(),
                lease_ttl: SignedDuration::from_secs(60),
                now: after_expiry
                    .checked_add(SignedDuration::from_secs(2))
                    .map_err(ctx("after_expiry + 2s"))?,
            },
        )?;
        assert!(second.token.epoch > first.token.epoch);
        assert!(matches!(
            store.complete(
                &WorkId::from_str("work-1"),
                &CompleteRequest {
                    token: first.token,
                    completed_at: after_expiry,
                    outcome: JobOutcome::Succeeded {
                        result: serde_json::json!({})
                    },
                },
            ),
            Err(SessionStoreError::LeaseTokenMismatch { .. })
        ));
        Ok(())
    }

    #[test]
    fn cancellation_fences_a_running_lease_and_keeps_scope_immutable() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let admitted = record("work-cancel")?;
        let scope = admitted.scope.clone();
        store.admit(&admitted)?;
        let now = Timestamp::now();
        let claim = store.claim(
            &WorkId::from_str("work-cancel"),
            &ClaimRequest {
                worker_id: "worker-a".to_owned(),
                lease_ttl: SignedDuration::from_secs(60),
                now,
            },
        )?;
        let cancelled_at = now
            .checked_add(SignedDuration::from_secs(1))
            .map_err(ctx("now + 1s"))?;
        let transition = store.cancel(
            &WorkId::from_str("work-cancel"),
            &CancelRequest {
                cancelled_at,
                cancelled_by: ApprovalActor::Operator {
                    id: "operator-a".to_owned(),
                },
                reason: "operator stopped the task".to_owned(),
            },
        )?;
        assert_eq!(transition.previous_state, JobState::Running);
        assert_eq!(transition.prior_lease, Some(claim.lease.clone()));
        assert_eq!(claim.scope, scope);
        assert!(matches!(
            transition.completion.outcome,
            JobOutcome::Cancelled { .. }
        ));

        let persisted = store.get(&WorkId::from_str("work-cancel"))?;
        assert_eq!(persisted.scope, scope);
        assert_eq!(persisted.job.state, JobState::Cancelled);
        assert!(persisted.lease.is_none());
        assert!(persisted.lease_epoch > claim.token.epoch);
        assert_eq!(persisted.revision, transition.revision);
        assert!(matches!(
            persisted.cancellation,
            Some(JobCancellation {
                cancelled_by: ApprovalActor::Operator { ref id },
                ..
            }) if id == "operator-a"
        ));
        assert!(matches!(
            store.renew(&RenewalRequest {
                token: claim.token,
                now: cancelled_at,
            }),
            Err(SessionStoreError::LeaseTokenMismatch { .. })
        ));
        Ok(())
    }

    #[test]
    fn cancellation_accepts_pending_and_ready_jobs_without_a_lease() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let now = Timestamp::now();
        let mut pending = record("work-pending")?;
        pending.job.state = JobState::Pending;
        store.admit(&pending)?;
        store.admit(&record("work-ready")?)?;

        for work_id in ["work-pending", "work-ready"] {
            let transition = store.cancel(
                &WorkId::from_str(work_id),
                &CancelRequest {
                    cancelled_at: now,
                    cancelled_by: ApprovalActor::Operator {
                        id: "operator-a".to_owned(),
                    },
                    reason: "superseded".to_owned(),
                },
            )?;
            assert!(matches!(
                transition.previous_state,
                JobState::Pending | JobState::Ready
            ));
            assert!(transition.prior_lease.is_none());
            let persisted = store.get(&WorkId::from_str(work_id))?;
            assert_eq!(persisted.job.state, JobState::Cancelled);
            assert_eq!(persisted.lease_epoch, 0);
        }
        Ok(())
    }

    #[test]
    fn durable_transitions_publish_only_redacted_state_and_revision() -> TestResult {
        let temp = tempfile::tempdir()?;
        let sink = Arc::new(RecordingSink::default());
        let store = JobStore::new_with_event_sink(temp.path(), sink.clone());
        let work_id = WorkId::from_str("work-events");
        store.admit(&record(work_id.as_str())?)?;
        let now = Timestamp::now();

        let claim = store.claim(
            &work_id,
            &ClaimRequest {
                worker_id: "worker-a".to_owned(),
                lease_ttl: SignedDuration::from_secs(60),
                now,
            },
        )?;
        let completed_at = now
            .checked_add(SignedDuration::from_secs(1))
            .map_err(ctx("now + 1s"))?;
        store.complete(
            &work_id,
            &CompleteRequest {
                token: claim.token,
                completed_at,
                outcome: JobOutcome::Succeeded {
                    result: serde_json::json!({"secret": "must-not-publish"}),
                },
            },
        )?;

        let events = sink
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        assert_eq!(
            events,
            vec![
                JobLifecycleEvent {
                    work_id: work_id.clone(),
                    state: JobState::Running,
                    revision: 1,
                },
                JobLifecycleEvent {
                    work_id,
                    state: JobState::Completed,
                    revision: 2,
                },
            ]
        );
        let encoded = serde_json::to_string(&events)?;
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("worker-a"));
        Ok(())
    }

    #[test]
    fn cancellation_and_reconciliation_publish_terminal_and_retry_states() -> TestResult {
        let temp = tempfile::tempdir()?;
        let sink = Arc::new(RecordingSink::default());
        let store = JobStore::new_with_event_sink(temp.path(), sink.clone());
        let cancelled_id = WorkId::from_str("work-cancel-event");
        store.admit(&record(cancelled_id.as_str())?)?;
        store.cancel(
            &cancelled_id,
            &CancelRequest {
                cancelled_at: Timestamp::now(),
                cancelled_by: ApprovalActor::Operator {
                    id: "operator-a".to_owned(),
                },
                reason: "stop".to_owned(),
            },
        )?;

        let expired_id = WorkId::from_str("work-reconcile-event");
        store.admit(&record(expired_id.as_str())?)?;
        let started = Timestamp::now();
        store.claim(
            &expired_id,
            &ClaimRequest {
                worker_id: "worker-b".to_owned(),
                lease_ttl: SignedDuration::from_secs(1),
                now: started,
            },
        )?;
        let after_expiry = started
            .checked_add(SignedDuration::from_secs(2))
            .map_err(ctx("started + 2s"))?;
        let expired = store.reconcile_expired(after_expiry, 100, None)?;
        assert_eq!(expired.expired.len(), 1);

        let events = sink
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        assert_eq!(events[0].state, JobState::Cancelled);
        assert_eq!(events[0].revision, 1);
        assert_eq!(events[1].state, JobState::Running);
        assert_eq!(events[1].revision, 1);
        assert_eq!(events[2].state, JobState::Ready);
        assert_eq!(events[2].revision, 2);
        assert!(events.windows(2).skip(1).all(|pair| pair[1].revision > 0));
        Ok(())
    }

    #[test]
    fn sync_parent_directory_reports_a_missing_directory_without_panicking() -> TestResult {
        let temp = tempfile::tempdir()?;
        let missing = temp.path().join("does-not-exist");

        assert!(matches!(
            sync_parent_directory(&missing),
            Err(SessionStoreError::Io(_))
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn list_skips_symlinked_json_records() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        store.admit(&record("work-regular")?)?;

        let external = temp.path().join("external.json");
        std::fs::write(&external, b"not a durable job")?;
        symlink(&external, store.records_dir().join("work-linked.json"))?;

        let page = store.list(&JobListQuery::default())?;

        assert_eq!(page.jobs.len(), 1);
        assert_eq!(page.jobs[0].job.id, WorkId::from_str("work-regular"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn get_rejects_a_symlinked_job_record_without_reading_its_target() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-linked-read");
        store.admit(&record(work_id.as_str())?)?;

        let path = store.record_path(&work_id)?;
        std::fs::remove_file(&path)?;
        let external = temp.path().join("external-record.json");
        let external_record = record("work-linked-read")?;
        let external_bytes = serde_json::to_vec(&external_record)?;
        std::fs::write(&external, &external_bytes)?;
        symlink(&external, &path)?;

        assert!(matches!(store.get(&work_id), Err(SessionStoreError::Io(_))));
        assert_eq!(std::fs::read(&external)?, external_bytes);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn mutation_rejects_a_symlinked_job_record_without_overwriting_its_target() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-linked-mutate");
        store.admit(&record(work_id.as_str())?)?;

        let path = store.record_path(&work_id)?;
        std::fs::remove_file(&path)?;
        let external = temp.path().join("external-record.json");
        let external_bytes = b"untrusted record target".to_vec();
        std::fs::write(&external, &external_bytes)?;
        symlink(&external, &path)?;

        assert!(matches!(
            store.cancel(
                &work_id,
                &CancelRequest {
                    cancelled_at: Timestamp::now(),
                    cancelled_by: ApprovalActor::Operator {
                        id: "operator-a".to_owned(),
                    },
                    reason: "stop".to_owned(),
                },
            ),
            Err(SessionStoreError::Io(_))
        ));
        assert_eq!(std::fs::read(&external)?, external_bytes);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn admit_rejects_a_symlinked_lock_sidecar_without_following_its_target() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        store.ensure_root()?;
        let work_id = WorkId::from_str("work-linked-lock");
        let lock_path = store.locks_dir().join("work-linked-lock.lock");
        let external = temp.path().join("external.lock");
        let external_bytes = b"untrusted lock target".to_vec();
        std::fs::write(&external, &external_bytes)?;
        symlink(&external, &lock_path)?;

        assert!(matches!(
            store.admit(&record(work_id.as_str())?),
            Err(SessionStoreError::Io(_))
        ));
        assert_eq!(std::fs::read(&external)?, external_bytes);
        assert!(!store.record_path(&work_id)?.exists());
        Ok(())
    }

    fn blocked_record(id: &str) -> TestResult<StoredJob> {
        let mut job = record(id)?;
        job.job.state = JobState::Blocked;
        Ok(job)
    }

    fn terminal_record(id: &str, state: JobState, attempts: u32) -> TestResult<StoredJob> {
        let mut job = record(id)?;
        job.job.state = state;
        job.job.attempts = attempts;
        Ok(job)
    }

    #[test]
    fn unblock_moves_a_blocked_job_to_ready_and_persists_the_approval_sidecar() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-approve");
        store.admit(&blocked_record(work_id.as_str())?)?;
        let now = Timestamp::now();

        let event = store.unblock(
            &work_id,
            now,
            ApprovalActor::Operator {
                id: "operator-a".to_owned(),
            },
            Some("looks fine".to_owned()),
        )?;

        assert_eq!(event.state, JobState::Ready);
        let persisted = store.get(&work_id)?;
        assert_eq!(persisted.job.state, JobState::Ready);
        assert!(persisted.lease.is_none());
        assert!(persisted.completion.is_none());

        let approval = store
            .get_approval(&work_id)?
            .ok_or(TestError::Missing("approval sidecar recorded"))?;
        assert_eq!(approval.work_id, work_id);
        assert_eq!(approval.note.as_deref(), Some("looks fine"));
        assert_eq!(approval.revision, event.revision);
        assert!(matches!(
            approval.approved_by,
            ApprovalActor::Operator { ref id } if id == "operator-a"
        ));
        Ok(())
    }

    #[test]
    fn get_approval_returns_none_for_a_job_that_was_never_approved() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-never-approved");
        store.admit(&record(work_id.as_str())?)?;

        assert_eq!(store.get_approval(&work_id)?, None);
        Ok(())
    }

    #[test]
    fn unblock_fails_for_a_job_that_is_not_blocked_and_writes_no_sidecar() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-not-blocked");
        store.admit(&record(work_id.as_str())?)?;

        let result = store.unblock(
            &work_id,
            Timestamp::now(),
            ApprovalActor::Operator {
                id: "operator-a".to_owned(),
            },
            None,
        );

        assert!(matches!(
            result,
            Err(SessionStoreError::JobNotBlocked { .. })
        ));
        assert_eq!(store.get_approval(&work_id)?, None);
        Ok(())
    }

    #[test]
    fn deny_blocked_cancels_a_blocked_job_with_actor_and_reason() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-deny-blocked");
        store.admit(&blocked_record(work_id.as_str())?)?;

        let transition = store.deny_blocked(
            &work_id,
            &CancelRequest {
                cancelled_at: Timestamp::now(),
                cancelled_by: ApprovalActor::Operator {
                    id: "operator-a".to_owned(),
                },
                reason: "not safe to proceed".to_owned(),
            },
        )?;

        assert_eq!(transition.previous_state, JobState::Blocked);
        let persisted = store.get(&work_id)?;
        assert_eq!(persisted.job.state, JobState::Cancelled);
        assert!(matches!(
            persisted.cancellation,
            Some(JobCancellation { ref reason, .. }) if reason == "not safe to proceed"
        ));
        Ok(())
    }

    #[test]
    fn deny_blocked_rejects_a_job_that_is_not_blocked() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-deny-not-blocked");
        store.admit(&record(work_id.as_str())?)?;

        let result = store.deny_blocked(
            &work_id,
            &CancelRequest {
                cancelled_at: Timestamp::now(),
                cancelled_by: ApprovalActor::Operator {
                    id: "operator-a".to_owned(),
                },
                reason: "not safe to proceed".to_owned(),
            },
        );

        assert!(matches!(
            result,
            Err(SessionStoreError::JobNotDeniable { .. })
        ));
        Ok(())
    }

    #[test]
    fn deny_blocked_rejects_an_empty_reason_before_touching_the_store() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-deny-empty-reason");
        store.admit(&blocked_record(work_id.as_str())?)?;

        let result = store.deny_blocked(
            &work_id,
            &CancelRequest {
                cancelled_at: Timestamp::now(),
                cancelled_by: ApprovalActor::Operator {
                    id: "operator-a".to_owned(),
                },
                reason: "   ".to_owned(),
            },
        );

        assert!(matches!(
            result,
            Err(SessionStoreError::InvalidJobCancellationReason)
        ));
        assert_eq!(store.get(&work_id)?.job.state, JobState::Blocked);
        Ok(())
    }

    #[test]
    fn retry_requeues_a_failed_job_and_increments_attempts() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-retry-failed");
        store.admit(&terminal_record(work_id.as_str(), JobState::Failed, 1)?)?;

        let event = store.retry(
            &work_id,
            &RetryRequest {
                retried_at: Timestamp::now(),
                retried_by: ApprovalActor::Operator {
                    id: "operator-a".to_owned(),
                },
            },
        )?;

        assert_eq!(event.state, JobState::Ready);
        let persisted = store.get(&work_id)?;
        assert_eq!(persisted.job.state, JobState::Ready);
        assert_eq!(persisted.job.attempts, 2);
        assert!(persisted.completion.is_none());
        assert!(persisted.cancellation.is_none());
        Ok(())
    }

    #[test]
    fn retry_requeues_a_cancelled_job() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-retry-cancelled");
        store.admit(&terminal_record(work_id.as_str(), JobState::Cancelled, 0)?)?;

        let event = store.retry(
            &work_id,
            &RetryRequest {
                retried_at: Timestamp::now(),
                retried_by: ApprovalActor::Operator {
                    id: "operator-a".to_owned(),
                },
            },
        )?;

        assert_eq!(event.state, JobState::Ready);
        assert_eq!(store.get(&work_id)?.job.attempts, 1);
        Ok(())
    }

    #[test]
    fn retry_rejects_a_job_that_is_not_failed_or_cancelled() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-retry-ready");
        store.admit(&record(work_id.as_str())?)?;

        let result = store.retry(
            &work_id,
            &RetryRequest {
                retried_at: Timestamp::now(),
                retried_by: ApprovalActor::Operator {
                    id: "operator-a".to_owned(),
                },
            },
        );

        assert!(matches!(
            result,
            Err(SessionStoreError::JobNotRetryable { .. })
        ));
        Ok(())
    }

    #[test]
    fn retry_returns_a_typed_error_and_does_not_requeue_once_the_limit_is_exhausted() -> TestResult
    {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-retry-exhausted");
        // `record()`'s fixture policy has max_attempts: 2.
        store.admit(&terminal_record(work_id.as_str(), JobState::Failed, 2)?)?;

        let result = store.retry(
            &work_id,
            &RetryRequest {
                retried_at: Timestamp::now(),
                retried_by: ApprovalActor::Operator {
                    id: "operator-a".to_owned(),
                },
            },
        );

        assert!(matches!(
            result,
            Err(SessionStoreError::JobRetryLimitExhausted {
                attempts: 2,
                max_attempts: 2,
                ..
            })
        ));
        // The job must still be exactly as it was — no silent requeue.
        let persisted = store.get(&work_id)?;
        assert_eq!(persisted.job.state, JobState::Failed);
        assert_eq!(persisted.job.attempts, 2);
        Ok(())
    }

    #[test]
    fn retry_advances_the_fencing_epoch_so_a_stale_token_cannot_complete_the_new_attempt()
    -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-retry-fencing");
        let mut stale = terminal_record(work_id.as_str(), JobState::Failed, 1)?;
        // Simulate the epoch the job carried from its prior (now-stale) lease.
        stale.lease_epoch = 5;
        store.admit(&stale)?;

        store.retry(
            &work_id,
            &RetryRequest {
                retried_at: Timestamp::now(),
                retried_by: ApprovalActor::Operator {
                    id: "operator-a".to_owned(),
                },
            },
        )?;

        let after_retry = store.get(&work_id)?;
        assert!(after_retry.lease_epoch > 5);

        // A fresh claim must issue a token beyond the pre-retry epoch, so a
        // worker still holding a token minted under epoch 5 can never match.
        let claim = store.claim(
            &work_id,
            &ClaimRequest {
                worker_id: "worker-a".to_owned(),
                lease_ttl: SignedDuration::from_secs(60),
                now: Timestamp::now(),
            },
        )?;
        assert!(claim.token.epoch > 5);
        Ok(())
    }

    fn pending_record(id: &str) -> TestResult<StoredJob> {
        let mut job = record(id)?;
        job.job.state = JobState::Pending;
        Ok(job)
    }

    fn operator() -> ApprovalActor {
        ApprovalActor::Operator {
            id: "operator-a".to_owned(),
        }
    }

    fn claim_for(store: &JobStore, work_id: &WorkId, now: Timestamp) -> TestResult<JobClaim> {
        Ok(store.claim(
            work_id,
            &ClaimRequest {
                worker_id: "worker-a".to_owned(),
                lease_ttl: SignedDuration::from_secs(60),
                now,
            },
        )?)
    }

    fn recorded_events(sink: &RecordingSink) -> Vec<JobLifecycleEvent> {
        sink.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    #[test]
    fn mark_ready_moves_a_pending_job_to_ready_and_is_idempotent() -> TestResult {
        let temp = tempfile::tempdir()?;
        let sink = Arc::new(RecordingSink::default());
        let store = JobStore::new_with_event_sink(temp.path(), sink.clone());
        let work_id = WorkId::from_str("work-mark-ready");
        store.admit(&pending_record(work_id.as_str())?)?;
        let now = Timestamp::now();

        let event = store.mark_ready(&work_id, now)?;
        assert_eq!(event.state, JobState::Ready);
        assert_eq!(event.revision, 1);
        let persisted = store.get(&work_id)?;
        assert_eq!(persisted.job.state, JobState::Ready);
        assert_eq!(persisted.job.updated_at, now);
        assert_eq!(persisted.revision, 1);
        assert!(persisted.lease.is_none());

        // Ein zweiter Aufruf ändert nichts und veröffentlicht nichts.
        let again = store.mark_ready(&work_id, now)?;
        assert_eq!(again, event);
        assert_eq!(store.get(&work_id)?.revision, 1);
        assert_eq!(recorded_events(&sink), vec![event]);

        // Der freigegebene Job ist beanspruchbar.
        claim_for(&store, &work_id, now)?;
        Ok(())
    }

    #[test]
    fn mark_ready_rejects_every_state_other_than_pending_or_ready() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let blocked = WorkId::from_str("work-mark-ready-blocked");
        store.admit(&blocked_record(blocked.as_str())?)?;
        let running = WorkId::from_str("work-mark-ready-running");
        store.admit(&record(running.as_str())?)?;
        claim_for(&store, &running, Timestamp::now())?;
        let failed = WorkId::from_str("work-mark-ready-failed");
        store.admit(&terminal_record(failed.as_str(), JobState::Failed, 1)?)?;

        for (work_id, state) in [
            (&blocked, JobState::Blocked),
            (&running, JobState::Running),
            (&failed, JobState::Failed),
        ] {
            let before = store.get(work_id)?;
            assert!(matches!(
                store.mark_ready(work_id, Timestamp::now()),
                Err(SessionStoreError::JobRuntime { .. })
            ));
            let after = store.get(work_id)?;
            assert_eq!(after.job.state, state);
            assert_eq!(after.revision, before.revision);
        }
        Ok(())
    }

    #[test]
    fn reclaim_returns_a_live_running_job_to_ready_and_fences_the_old_lease() -> TestResult {
        let temp = tempfile::tempdir()?;
        let sink = Arc::new(RecordingSink::default());
        let store = JobStore::new_with_event_sink(temp.path(), sink.clone());
        let work_id = WorkId::from_str("work-reclaim");
        store.admit(&record(work_id.as_str())?)?;
        let now = Timestamp::now();
        let claim = claim_for(&store, &work_id, now)?;

        // Die Lease läuft noch (60 s) — der Einzel-Reclaim verlangt keinen Ablauf.
        let reclaimed = store.reclaim(&work_id, now, operator())?;
        assert_eq!(reclaimed.work_id, work_id);
        assert_eq!(reclaimed.expired_lease, claim.lease);
        assert_eq!(reclaimed.reclaimed_at, now);
        let retry_at = now
            .checked_add(SignedDuration::from_secs(1))
            .map_err(ctx("now + 1s"))?;
        assert_eq!(reclaimed.retry_scheduled_for, Some(retry_at));

        let persisted = store.get(&work_id)?;
        assert_eq!(persisted.job.state, JobState::Ready);
        assert_eq!(persisted.job.attempts, 1);
        assert!(persisted.lease.is_none());
        assert!(persisted.completion.is_none());
        assert_eq!(persisted.not_before, retry_at);
        assert!(persisted.lease_epoch > claim.token.epoch);
        assert_eq!(
            recorded_events(&sink).last(),
            Some(&JobLifecycleEvent {
                work_id: work_id.clone(),
                state: JobState::Ready,
                revision: persisted.revision,
            })
        );

        // Der alte Halter kann weder abschließen noch verlängern.
        assert!(matches!(
            store.complete(
                &work_id,
                &CompleteRequest {
                    token: claim.token.clone(),
                    completed_at: now,
                    outcome: JobOutcome::Succeeded {
                        result: serde_json::json!({}),
                    },
                },
            ),
            Err(SessionStoreError::LeaseTokenMismatch { .. })
        ));
        assert!(matches!(
            store.renew(&RenewalRequest {
                token: claim.token,
                now,
            }),
            Err(SessionStoreError::LeaseTokenMismatch { .. })
        ));
        Ok(())
    }

    #[test]
    fn reclaim_fails_the_job_once_the_retry_budget_is_exhausted() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-reclaim-exhausted");
        let mut stored = record(work_id.as_str())?;
        stored.job.attempts = 1;
        store.admit(&stored)?;
        let now = Timestamp::now();
        claim_for(&store, &work_id, now)?;

        let reclaimed = store.reclaim(&work_id, now, operator())?;
        assert_eq!(reclaimed.retry_scheduled_for, None);
        let persisted = store.get(&work_id)?;
        assert_eq!(persisted.job.state, JobState::Failed);
        assert_eq!(persisted.job.attempts, 2);
        assert!(persisted.lease.is_none());
        assert!(matches!(
            persisted.completion.map(|done| done.outcome),
            Some(JobOutcome::Failed { .. })
        ));
        Ok(())
    }

    #[test]
    fn reclaim_rejects_a_job_that_is_not_running_and_writes_nothing() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-reclaim-ready");
        store.admit(&record(work_id.as_str())?)?;
        let before = store.get(&work_id)?;

        assert!(matches!(
            store.reclaim(&work_id, Timestamp::now(), operator()),
            Err(SessionStoreError::JobRuntime { .. })
        ));
        let after = store.get(&work_id)?;
        assert_eq!(after, before);
        Ok(())
    }
}
