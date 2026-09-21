//! Durable, fenced job persistence for worker and MCP orchestration.
//!
//! The store owns filesystem locking and atomic record replacement; the pure
//! lifecycle rules remain in `harw-job-runtime`. Every mutation verifies the
//! current lease token, so an expired or restarted worker cannot overwrite a
//! job reclaimed by another worker.
//!
//! Jede Mutation (Claim, Renew, Complete, Cancel, Reconcile) synct nach dem
//! atomaren `persist()` zusätzlich das Elternverzeichnis des Job-Datensatzes;
//! ein hier verlorener Fortschritt hieße stillen Stillstand statt eines
//! bloß veralteten Zustands.
//!
//! A-STORE (G-020, F-179): ein defekter `*.json`-Datensatz legt `list` nicht
//! mehr lahm — er wird (unter seinem Record-Lock) nach
//! `<id>.json.corrupt-<ts>` verschoben und übersprungen. `admit` schreibt per
//! `persist_noclobber`. `reconcile_expired` arbeitet seitenweise
//! (`limit`/`cursor`) und überspringt einzelne gesperrte/defekte Jobs, statt
//! den ganzen Lauf abzubrechen. `unblock` führt einen `Blocked`-Job
//! (Approval-Pause) nach `Ready` zurück.

use std::fs::File;
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fs4::FileExt;
use harw_fsutil::OpenMode;
use harw_job_runtime::{
    JobCancellation, JobClaim, JobCompletion, JobOutcome, JobRuntimeError, JobState, Lease,
    LeaseToken, StoredJob,
};
use harw_types::{ApprovalActor, WorkId};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::error::{SessionStoreError, SessionStoreResult};
use crate::store::{persist_noclobber, quarantine_file};

/// Default create-mode für `create`-Aufrufe, identisch zum bisherigen Verhalten:
/// `std::fs::OpenOptions` legt ohne `.mode(...)` mit `0o666` (abzüglich `umask`) an.
const DEFAULT_CREATE_MODE: u32 = 0o666;

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
/// [`StoredJob`] (`harw-job-runtime`) trägt kein Akteurs-/Freitextfeld für
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
        self.ensure_root()?;
        let path = self.record_path(&record.job.id)?;
        let lock = self.lock(&record.job.id)?;
        let result = if path_exists_without_following_symlinks(&path, "job record")? {
            Err(SessionStoreError::JobAlreadyExists {
                work_id: record.job.id.clone(),
            })
        } else {
            // F-179: atomar und ohne Überschreiben — ein Absturz hinterlässt nie
            // eine halbe Datei, ein paralleler Eintrag wird nie ersetzt.
            serde_json::to_vec(record)
                .map_err(SessionStoreError::from)
                .and_then(|bytes| match persist_noclobber(&path, &bytes) {
                    Err(SessionStoreError::PersistTargetExists { .. }) => {
                        Err(SessionStoreError::JobAlreadyExists {
                            work_id: record.job.id.clone(),
                        })
                    }
                    other => other,
                })
        };
        unlock(lock, result)
    }

    pub fn get(&self, work_id: &WorkId) -> SessionStoreResult<StoredJob> {
        let path = self.record_path(work_id)?;
        self.read_record(&path, work_id)
    }

    /// Lists an eventually-consistent snapshot. Point mutations take a
    /// per-record lock; list readers deliberately do not serialize every job.
    pub fn list(&self, query: &JobListQuery) -> SessionStoreResult<JobPage> {
        let records = self.records_dir();
        if !records.exists() {
            return Ok(JobPage {
                jobs: Vec::new(),
                next_cursor: None,
            });
        }
        let mut jobs = Vec::new();
        for entry in std::fs::read_dir(records)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::warn!(error = %error, "job record entry unreadable; skipped");
                    continue;
                }
            };
            match entry.file_type() {
                Ok(file_type) if file_type.is_symlink() => continue,
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        path = %entry.path().display(),
                        error = %error,
                        "job record type unreadable; skipped"
                    );
                    continue;
                }
            }
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let Some(record) = self.load_listed_record(&path) else {
                continue;
            };
            if query
                .states
                .as_ref()
                .is_some_and(|states| !states.contains(&record.job.state))
                || query.holder.as_ref().is_some_and(|holder| {
                    record.lease.as_ref().map(|lease| &lease.holder) != Some(holder)
                })
                || query
                    .cursor
                    .as_ref()
                    .is_some_and(|cursor| record.job.id.as_str() <= cursor.as_str())
            {
                continue;
            }
            jobs.push(record);
        }
        jobs.sort_by(|left, right| left.job.id.as_str().cmp(right.job.id.as_str()));
        let limit = query.limit.clamp(1, 100);
        let next_cursor = (jobs.len() > limit).then(|| jobs[limit - 1].job.id.clone());
        jobs.truncate(limit);
        Ok(JobPage { jobs, next_cursor })
    }

    /// Atomically acquires a new fenced lease for a Ready, eligible job.
    pub fn claim(&self, work_id: &WorkId, request: &ClaimRequest) -> SessionStoreResult<JobClaim> {
        if request.lease_ttl <= SignedDuration::ZERO {
            return Err(SessionStoreError::InvalidJobLeaseTtl);
        }
        let (claim, revision) = self.with_locked(work_id, |record| {
            if record.job.state != JobState::Ready {
                return Err(SessionStoreError::JobNotClaimable {
                    work_id: work_id.clone(),
                    state: record.job.state,
                });
            }
            if request.now < record.not_before {
                return Err(SessionStoreError::JobNotEligible {
                    work_id: work_id.clone(),
                    not_before: record.not_before,
                });
            }
            let epoch = record.lease_epoch.checked_add(1).ok_or_else(|| {
                SessionStoreError::JobLeaseEpochExhausted {
                    work_id: work_id.clone(),
                }
            })?;
            let nonce = WorkId::new().as_str().to_owned();
            let lease = Lease::acquire_fenced(
                work_id.clone(),
                request.worker_id.clone(),
                request.now,
                request.lease_ttl,
                epoch,
                nonce,
            )
            .map_err(|error| map_job_error(work_id, error))?;
            record.job.state = JobState::Running;
            record.job.updated_at = request.now;
            record.lease_epoch = epoch;
            record.lease = Some(lease.clone());
            record.revision = record.revision.saturating_add(1);
            Ok((
                JobClaim {
                    job: record.job.clone(),
                    scope: record.scope.clone(),
                    token: lease.token(),
                    lease,
                },
                record.revision,
            ))
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
            let renewed = {
                let lease = current_lease(record, &request.token, request.now)?;
                lease
                    .renew(request.now)
                    .map_err(|error| map_job_error(&work_id, error))?;
                lease.clone()
            };
            record.revision = record.revision.saturating_add(1);
            Ok(renewed)
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
            if is_terminal(record.job.state) {
                return Err(SessionStoreError::JobAlreadyTerminal {
                    work_id: work_id.clone(),
                    state: record.job.state,
                });
            }
            current_lease(record, &request.token, request.completed_at)?;
            let completion = JobCompletion {
                completed_at: request.completed_at,
                outcome: request.outcome.clone(),
            };
            record.job.state = outcome_state(&completion.outcome);
            record.job.updated_at = request.completed_at;
            record.lease = None;
            record.completion = Some(completion.clone());
            record.revision = record.revision.saturating_add(1);
            Ok((completion, record.revision))
        })?;
        self.publish(JobLifecycleEvent {
            work_id: work_id.clone(),
            state: outcome_state(&request.outcome),
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
            let previous_state = record.job.state;
            if !matches!(
                previous_state,
                JobState::Pending | JobState::Ready | JobState::Running
            ) {
                return Err(SessionStoreError::JobNotCancellable {
                    work_id: work_id.clone(),
                    state: previous_state,
                });
            }

            let prior_lease = record.lease.take();
            if previous_state == JobState::Running {
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
    /// (that shape lives in `harw-job-runtime`, outside this crate).
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
        let path = self.approval_path(work_id)?;
        match read_regular_file(&path) {
            Ok(bytes) => serde_json::from_slice::<JobApproval>(&bytes)
                .map(Some)
                .map_err(SessionStoreError::from),
            Err(SessionStoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(None)
            }
            Err(error) => Err(error),
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
    /// respecting the job's own [`harw_job_runtime::RetryPolicy`] ceiling.
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
            let Some(lease) = record.lease.clone() else {
                return Ok(None);
            };
            if record.job.state != JobState::Running || !lease.is_expired(now) {
                return Ok(None);
            }
            record.lease = None;
            record.lease_epoch = record.lease_epoch.saturating_add(1);
            let retry_scheduled_for = match record.job.record_failure(now) {
                Ok(delay) => Some(now.checked_add(delay).map_err(|error| {
                    SessionStoreError::JobRuntime {
                        work_id: work_id.clone(),
                        detail: error.to_string(),
                    }
                })?),
                Err(JobRuntimeError::RetryExhausted { .. }) => {
                    record.completion = Some(JobCompletion {
                        completed_at: now,
                        outcome: JobOutcome::Failed {
                            reason: "worker lease expired and retry budget is exhausted"
                                .to_owned(),
                        },
                    });
                    None
                }
                Err(error) => {
                    return Err(SessionStoreError::JobRuntime {
                        work_id: work_id.clone(),
                        detail: error.to_string(),
                    });
                }
            };
            if let Some(not_before) = retry_scheduled_for {
                record.not_before = not_before;
            }
            record.revision = record.revision.saturating_add(1);
            Ok(Some((
                ExpiredJob {
                    work_id: work_id.clone(),
                    expired_lease: lease,
                    reclaimed_at: now,
                    retry_scheduled_for,
                },
                record.job.state,
                record.revision,
            )))
        })
    }

    // Reads one listed record lock-free; corrupt records are quarantined and skipped.
    fn load_listed_record(&self, path: &Path) -> Option<StoredJob> {
        let bytes = match read_regular_file(path) {
            Ok(bytes) => bytes,
            Err(SessionStoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return None;
            }
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "job record unreadable; skipped"
                );
                return None;
            }
        };
        let expected = path.file_stem().and_then(|stem| stem.to_str());
        match serde_json::from_slice::<StoredJob>(&bytes) {
            Ok(record) if Some(record.job.id.as_str()) == expected => Some(record),
            Ok(_) => {
                self.quarantine_listed_record(
                    path,
                    "record id does not match its canonical path",
                );
                None
            }
            Err(error) => {
                self.quarantine_listed_record(path, &error.to_string());
                None
            }
        }
    }

    // G-020: moves a corrupt listed record aside, but only under its record lock
    // and only after re-confirming it is still corrupt.
    fn quarantine_listed_record(&self, path: &Path, detail: &str) {
        let Some(work_id) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(WorkId::from_str)
            .filter(|work_id| safe_component(work_id).is_ok())
        else {
            tracing::warn!(
                path = %path.display(),
                detail,
                "corrupt job record has an unsafe name; skipped without quarantine"
            );
            return;
        };
        if let Err(error) = self.ensure_root() {
            tracing::warn!(path = %path.display(), error = %error, "job store root unavailable");
            return;
        }
        let lock = match self.lock(&work_id) {
            Ok(lock) => lock,
            Err(error) => {
                tracing::warn!(
                    work_id = %work_id,
                    error = %error,
                    detail,
                    "corrupt job record skipped; quarantine deferred"
                );
                return;
            }
        };
        let result = match self.read_record(path, &work_id) {
            Err(SessionStoreError::CorruptJob { .. }) => quarantine_file(path),
            Ok(_) | Err(SessionStoreError::JobNotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        };
        match unlock(lock, result) {
            Ok(Some(quarantine)) => tracing::warn!(
                work_id = %work_id,
                quarantine = %quarantine.display(),
                detail,
                "corrupt job record quarantined"
            ),
            Ok(None) => {}
            Err(error) => tracing::warn!(
                work_id = %work_id,
                error = %error,
                "corrupt job record could not be quarantined; skipped"
            ),
        }
    }

    fn with_locked<T>(
        &self,
        work_id: &WorkId,
        operation: impl FnOnce(&mut StoredJob) -> SessionStoreResult<T>,
    ) -> SessionStoreResult<T> {
        self.ensure_root()?;
        let path = self.record_path(work_id)?;
        let lock = self.lock(work_id)?;
        let result = (|| {
            let mut record = match self.read_record(&path, work_id) {
                Ok(record) => record,
                Err(error @ SessionStoreError::CorruptJob { .. }) => {
                    // G-020: under the record lock the defective file is moved
                    // aside, so it cannot poison later scans; the caller still
                    // learns that this job is corrupt.
                    match quarantine_file(&path) {
                        Ok(Some(quarantine)) => tracing::warn!(
                            work_id = %work_id,
                            quarantine = %quarantine.display(),
                            "corrupt job record quarantined"
                        ),
                        Ok(None) => {}
                        Err(quarantine_error) => tracing::warn!(
                            work_id = %work_id,
                            error = %quarantine_error,
                            "corrupt job record could not be quarantined"
                        ),
                    }
                    return Err(error);
                }
                Err(error) => return Err(error),
            };
            let output = operation(&mut record)?;
            persist_json(&path, &record)?;
            Ok(output)
        })();
        unlock(lock, result)
    }

    fn publish(&self, event: JobLifecycleEvent) {
        self.event_sink.publish(event);
    }

    fn ensure_root(&self) -> SessionStoreResult<()> {
        std::fs::create_dir_all(self.records_dir())?;
        std::fs::create_dir_all(self.locks_dir())?;
        Ok(())
    }

    fn records_dir(&self) -> PathBuf {
        self.root.join("records")
    }

    fn locks_dir(&self) -> PathBuf {
        self.root.join("locks")
    }

    fn approvals_dir(&self) -> PathBuf {
        self.root.join("approvals")
    }

    fn approval_path(&self, work_id: &WorkId) -> SessionStoreResult<PathBuf> {
        Ok(self
            .approvals_dir()
            .join(safe_component(work_id)?)
            .with_extension("json"))
    }

    /// Atomically writes (or overwrites) the [`JobApproval`] sidecar for
    /// `approval.work_id`. Called after the job's own `Blocked → Ready`
    /// transition is already durable, so this failing never leaves the job
    /// record itself inconsistent — only the audit sidecar is affected.
    fn persist_approval(&self, approval: &JobApproval) -> SessionStoreResult<()> {
        std::fs::create_dir_all(self.approvals_dir())?;
        let path = self.approval_path(&approval.work_id)?;
        persist_json(&path, approval)
    }

    fn record_path(&self, work_id: &WorkId) -> SessionStoreResult<PathBuf> {
        Ok(self
            .records_dir()
            .join(safe_component(work_id)?)
            .with_extension("json"))
    }

    fn lock(&self, work_id: &WorkId) -> SessionStoreResult<File> {
        let path = self
            .locks_dir()
            .join(safe_component(work_id)?)
            .with_extension("lock");
        reject_symlink(&path, "job lock")?;
        let file = open_without_following_symlinks(
            OpenMode {
                read: true,
                write: true,
                create: true,
                create_new: false,
                truncate: false,
                append: false,
                mode: DEFAULT_CREATE_MODE,
            },
            &path,
        )?;
        FileExt::try_lock(&file).map_err(|error| match error {
            fs4::TryLockError::WouldBlock => SessionStoreError::JobLockContended {
                work_id: work_id.clone(),
            },
            fs4::TryLockError::Error(error) => SessionStoreError::Io(error),
        })?;
        Ok(file)
    }

    fn read_record(&self, path: &Path, work_id: &WorkId) -> SessionStoreResult<StoredJob> {
        reject_symlink(path, "job record")?;
        let open_result = open_without_following_symlinks(OpenMode::read_only(), path);
        let mut file = open_result.map_err(|error| {
            if matches!(
                &error,
                SessionStoreError::Io(io) if io.kind() == std::io::ErrorKind::NotFound
            ) {
                SessionStoreError::JobNotFound {
                    work_id: work_id.clone(),
                }
            } else {
                error
            }
        })?;
        if !file.metadata()?.file_type().is_file() {
            return Err(SessionStoreError::JobNotFound {
                work_id: work_id.clone(),
            });
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let record: StoredJob =
            serde_json::from_slice(&bytes).map_err(|error| SessionStoreError::CorruptJob {
                work_id: work_id.clone(),
                detail: error.to_string(),
            })?;
        if record.job.id != *work_id {
            return Err(SessionStoreError::CorruptJob {
                work_id: work_id.clone(),
                detail: "record id does not match its canonical path".to_owned(),
            });
        }
        Ok(record)
    }
}

fn current_lease<'a>(
    record: &'a mut StoredJob,
    token: &LeaseToken,
    now: Timestamp,
) -> SessionStoreResult<&'a mut Lease> {
    let work_id = record.job.id.clone();
    let lease = record
        .lease
        .as_mut()
        .ok_or_else(|| SessionStoreError::LeaseTokenMismatch {
            work_id: work_id.clone(),
        })?;
    if !lease.matches_token(token) {
        return Err(SessionStoreError::LeaseTokenMismatch { work_id });
    }
    if lease.is_expired(now) {
        return Err(SessionStoreError::JobLeaseExpired {
            work_id,
            expired_at: lease.expires_at,
        });
    }
    Ok(lease)
}

fn outcome_state(outcome: &JobOutcome) -> JobState {
    match outcome {
        JobOutcome::Succeeded { .. } => JobState::Completed,
        JobOutcome::Failed { .. } => JobState::Failed,
        JobOutcome::Cancelled { .. } => JobState::Cancelled,
        JobOutcome::Blocked { .. } => JobState::Blocked,
    }
}

fn map_job_error(work_id: &WorkId, error: JobRuntimeError) -> SessionStoreError {
    match error {
        JobRuntimeError::LeaseExpired { expired_at, .. } => SessionStoreError::JobLeaseExpired {
            work_id: work_id.clone(),
            expired_at,
        },
        error => SessionStoreError::JobRuntime {
            work_id: work_id.clone(),
            detail: error.to_string(),
        },
    }
}

fn is_terminal(state: JobState) -> bool {
    matches!(
        state,
        JobState::Completed | JobState::Failed | JobState::Cancelled
    )
}

fn safe_component(work_id: &WorkId) -> SessionStoreResult<&str> {
    let value = work_id.as_str();
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SessionStoreError::UnsafeJobPath(value.to_owned()));
    }
    Ok(value)
}

// Reads a regular file without following a final symlink.
fn read_regular_file(path: &Path) -> SessionStoreResult<Vec<u8>> {
    let mut file = open_without_following_symlinks(OpenMode::read_only(), path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(SessionStoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "job record is not a regular file",
        )));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn persist_json<T: Serialize>(path: &Path, value: &T) -> SessionStoreResult<()> {
    let parent = path.parent().ok_or_else(|| {
        SessionStoreError::Io(std::io::Error::other("job record path has no parent"))
    })?;
    let mut temp = NamedTempFile::new_in(parent)?;
    serde_json::to_writer(temp.as_file_mut(), value)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|error| SessionStoreError::Io(error.error))?;
    // Das Elternverzeichnis wird gesynct, weil jede Job-Mutation (Claim,
    // Renew, Complete, Cancel, Reconcile) über diese Funktion läuft: der
    // atomare `persist()` ersetzt den Verzeichniseintrag, nicht nur den
    // Dateiinhalt. Fällt dieser Eintrag nach einem Absturz auf die alte
    // Version zurück, sieht ein Worker eine überholte Lease oder einen
    // bereits abgeschlossenen Job wieder als offen an — aus verlorenem
    // Fortschritt würde stiller Stillstand oder doppelte Bearbeitung.
    sync_parent_directory(parent)?;
    Ok(())
}

/// Synct das Elternverzeichnis eines soeben angelegten oder ersetzten
/// Job-Datensatzes. Ein `sync_all()` auf der Datei sichert nur ihren Inhalt;
/// der Verzeichniseintrag, der sie überhaupt auffindbar macht, liegt im
/// Verzeichnis-Inode und muss separat gesynct werden — sonst kann eine
/// vollständig geschriebene Datei nach einem Stromausfall trotzdem nicht
/// existieren. Ein Verzeichnis wird zum Lesen geöffnet (`File::open`), nicht
/// zum Schreiben; `sync_all()` erfasst dabei genau den Verzeichniseintrag.
/// Ein Sync-Fehler wird propagiert statt verschluckt, exakt wie in
/// `store.rs::sync_parent_directory`.
// Die eine Fassung liegt in `crate::durability`; sie stand vorher in fünf
// Dateien byte-gleich. Warum ein Eltern-fsync nötig ist, steht dort.
use crate::durability::sync_parent_directory;

fn path_exists_without_following_symlinks(
    path: &Path,
    description: &str,
) -> SessionStoreResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(symlink_error(path, description)),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(SessionStoreError::Io(error)),
    }
}

fn reject_symlink(path: &Path, description: &str) -> SessionStoreResult<()> {
    path_exists_without_following_symlinks(path, description).map(|_| ())
}

/// Öffnet `path` ohne dem letzten Pfadglied als Symlink zu folgen
/// (F-006: das architekturabhängig falsche `O_NOFOLLOW` wurde durch
/// `harw_fsutil::open_nofollow` ersetzt, das die Konstante über
/// `rustix::fs::OFlags::NOFOLLOW` plattformkorrekt bezieht).
fn open_without_following_symlinks(mode: OpenMode, path: &Path) -> SessionStoreResult<File> {
    #[cfg(unix)]
    {
        harw_fsutil::open_nofollow(path, mode).map_err(SessionStoreError::Io)
    }
    #[cfg(not(unix))]
    {
        let mut options = OpenOptions::new();
        options
            .read(mode.read)
            .write(mode.write || mode.append)
            .create(mode.create)
            .create_new(mode.create_new)
            .truncate(mode.truncate)
            .append(mode.append);
        options.open(path).map_err(SessionStoreError::Io)
    }
}

fn symlink_error(path: &Path, description: &str) -> SessionStoreError {
    SessionStoreError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "{description} must not be a symbolic link: {}",
            path.display()
        ),
    ))
}

fn unlock<T>(lock: File, result: SessionStoreResult<T>) -> SessionStoreResult<T> {
    let unlock = FileExt::unlock(&lock).map_err(SessionStoreError::Io);
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_job_runtime::{Budget, Job, JobKind, RetryPolicy};
    use harw_observe::TraceContext;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<JobLifecycleEvent>>);

    impl JobEventSink for RecordingSink {
        fn publish(&self, event: JobLifecycleEvent) {
            self.0.lock().unwrap().push(event);
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
        job.mark_ready(now).unwrap();
        StoredJob {
            job,
            scope: harw_job_runtime::JobScope::new(
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
        }
    }

    fn sample_trace() -> TraceContext {
        TraceContext {
            trace_id: "a".repeat(32),
            span_id: "b".repeat(16),
            parent_span_id: None,
        }
    }

    #[test]
    fn admitted_job_with_trace_context_roundtrips_through_the_real_store_path() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let admitted = StoredJob {
            trace: Some(sample_trace()),
            ..record("work-traced")
        };

        store.admit(&admitted).unwrap();

        let persisted = store.get(&WorkId::from_str("work-traced")).unwrap();
        assert_eq!(persisted, admitted);
        assert_eq!(persisted.trace, Some(sample_trace()));
    }

    #[test]
    fn admitted_job_without_trace_context_roundtrips_through_the_real_store_path() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let admitted = record("work-untraced");
        assert_eq!(admitted.trace, None);

        store.admit(&admitted).unwrap();

        let persisted = store.get(&WorkId::from_str("work-untraced")).unwrap();
        assert_eq!(persisted, admitted);
        assert_eq!(persisted.trace, None);
    }

    #[test]
    fn a_stale_worker_cannot_complete_after_reconciliation_and_reclaim() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        store.admit(&record("work-1")).unwrap();
        let now = Timestamp::now();
        let first = store
            .claim(
                &WorkId::from_str("work-1"),
                &ClaimRequest {
                    worker_id: "worker-a".to_owned(),
                    lease_ttl: SignedDuration::from_secs(1),
                    now,
                },
            )
            .unwrap();
        let after_expiry = now.checked_add(SignedDuration::from_secs(2)).unwrap();
        assert_eq!(
            store
                .reconcile_expired(after_expiry, 100, None)
                .unwrap()
                .expired
                .len(),
            1
        );
        let second = store
            .claim(
                &WorkId::from_str("work-1"),
                &ClaimRequest {
                    worker_id: "worker-b".to_owned(),
                    lease_ttl: SignedDuration::from_secs(60),
                    now: after_expiry
                        .checked_add(SignedDuration::from_secs(2))
                        .unwrap(),
                },
            )
            .unwrap();
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
    }

    #[test]
    fn cancellation_fences_a_running_lease_and_keeps_scope_immutable() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let admitted = record("work-cancel");
        let scope = admitted.scope.clone();
        store.admit(&admitted).unwrap();
        let now = Timestamp::now();
        let claim = store
            .claim(
                &WorkId::from_str("work-cancel"),
                &ClaimRequest {
                    worker_id: "worker-a".to_owned(),
                    lease_ttl: SignedDuration::from_secs(60),
                    now,
                },
            )
            .unwrap();
        let cancelled_at = now.checked_add(SignedDuration::from_secs(1)).unwrap();
        let transition = store
            .cancel(
                &WorkId::from_str("work-cancel"),
                &CancelRequest {
                    cancelled_at,
                    cancelled_by: ApprovalActor::Operator {
                        id: "operator-a".to_owned(),
                    },
                    reason: "operator stopped the task".to_owned(),
                },
            )
            .unwrap();
        assert_eq!(transition.previous_state, JobState::Running);
        assert_eq!(transition.prior_lease, Some(claim.lease.clone()));
        assert_eq!(claim.scope, scope);
        assert!(matches!(
            transition.completion.outcome,
            JobOutcome::Cancelled { .. }
        ));

        let persisted = store.get(&WorkId::from_str("work-cancel")).unwrap();
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
    }

    #[test]
    fn cancellation_accepts_pending_and_ready_jobs_without_a_lease() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let now = Timestamp::now();
        let mut pending = record("work-pending");
        pending.job.state = JobState::Pending;
        store.admit(&pending).unwrap();
        store.admit(&record("work-ready")).unwrap();

        for work_id in ["work-pending", "work-ready"] {
            let transition = store
                .cancel(
                    &WorkId::from_str(work_id),
                    &CancelRequest {
                        cancelled_at: now,
                        cancelled_by: ApprovalActor::Operator {
                            id: "operator-a".to_owned(),
                        },
                        reason: "superseded".to_owned(),
                    },
                )
                .unwrap();
            assert!(matches!(
                transition.previous_state,
                JobState::Pending | JobState::Ready
            ));
            assert!(transition.prior_lease.is_none());
            let persisted = store.get(&WorkId::from_str(work_id)).unwrap();
            assert_eq!(persisted.job.state, JobState::Cancelled);
            assert_eq!(persisted.lease_epoch, 0);
        }
    }

    #[test]
    fn durable_transitions_publish_only_redacted_state_and_revision() {
        let temp = tempfile::tempdir().unwrap();
        let sink = Arc::new(RecordingSink::default());
        let store = JobStore::new_with_event_sink(temp.path(), sink.clone());
        let work_id = WorkId::from_str("work-events");
        store.admit(&record(work_id.as_str())).unwrap();
        let now = Timestamp::now();

        let claim = store
            .claim(
                &work_id,
                &ClaimRequest {
                    worker_id: "worker-a".to_owned(),
                    lease_ttl: SignedDuration::from_secs(60),
                    now,
                },
            )
            .unwrap();
        let completed_at = now.checked_add(SignedDuration::from_secs(1)).unwrap();
        store
            .complete(
                &work_id,
                &CompleteRequest {
                    token: claim.token,
                    completed_at,
                    outcome: JobOutcome::Succeeded {
                        result: serde_json::json!({"secret": "must-not-publish"}),
                    },
                },
            )
            .unwrap();

        let events = sink.0.lock().unwrap().clone();
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
        let encoded = serde_json::to_string(&events).unwrap();
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("worker-a"));
    }

    #[test]
    fn cancellation_and_reconciliation_publish_terminal_and_retry_states() {
        let temp = tempfile::tempdir().unwrap();
        let sink = Arc::new(RecordingSink::default());
        let store = JobStore::new_with_event_sink(temp.path(), sink.clone());
        let cancelled_id = WorkId::from_str("work-cancel-event");
        store.admit(&record(cancelled_id.as_str())).unwrap();
        store
            .cancel(
                &cancelled_id,
                &CancelRequest {
                    cancelled_at: Timestamp::now(),
                    cancelled_by: ApprovalActor::Operator {
                        id: "operator-a".to_owned(),
                    },
                    reason: "stop".to_owned(),
                },
            )
            .unwrap();

        let expired_id = WorkId::from_str("work-reconcile-event");
        store.admit(&record(expired_id.as_str())).unwrap();
        let started = Timestamp::now();
        store
            .claim(
                &expired_id,
                &ClaimRequest {
                    worker_id: "worker-b".to_owned(),
                    lease_ttl: SignedDuration::from_secs(1),
                    now: started,
                },
            )
            .unwrap();
        let after_expiry = started.checked_add(SignedDuration::from_secs(2)).unwrap();
        let expired = store.reconcile_expired(after_expiry, 100, None).unwrap();
        assert_eq!(expired.expired.len(), 1);

        let events = sink.0.lock().unwrap().clone();
        assert_eq!(events[0].state, JobState::Cancelled);
        assert_eq!(events[0].revision, 1);
        assert_eq!(events[1].state, JobState::Running);
        assert_eq!(events[1].revision, 1);
        assert_eq!(events[2].state, JobState::Ready);
        assert_eq!(events[2].revision, 2);
        assert!(events.windows(2).skip(1).all(|pair| pair[1].revision > 0));
    }

    #[test]
    fn sync_parent_directory_reports_a_missing_directory_without_panicking() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("does-not-exist");

        assert!(matches!(
            sync_parent_directory(&missing),
            Err(SessionStoreError::Io(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn list_skips_symlinked_json_records() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        store.admit(&record("work-regular")).unwrap();

        let external = temp.path().join("external.json");
        std::fs::write(&external, b"not a durable job").unwrap();
        symlink(&external, store.records_dir().join("work-linked.json")).unwrap();

        let page = store.list(&JobListQuery::default()).unwrap();

        assert_eq!(page.jobs.len(), 1);
        assert_eq!(page.jobs[0].job.id, WorkId::from_str("work-regular"));
    }

    #[cfg(unix)]
    #[test]
    fn get_rejects_a_symlinked_job_record_without_reading_its_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-linked-read");
        store.admit(&record(work_id.as_str())).unwrap();

        let path = store.record_path(&work_id).unwrap();
        std::fs::remove_file(&path).unwrap();
        let external = temp.path().join("external-record.json");
        let external_record = record("work-linked-read");
        let external_bytes = serde_json::to_vec(&external_record).unwrap();
        std::fs::write(&external, &external_bytes).unwrap();
        symlink(&external, &path).unwrap();

        assert!(matches!(store.get(&work_id), Err(SessionStoreError::Io(_))));
        assert_eq!(std::fs::read(&external).unwrap(), external_bytes);
    }

    #[cfg(unix)]
    #[test]
    fn mutation_rejects_a_symlinked_job_record_without_overwriting_its_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-linked-mutate");
        store.admit(&record(work_id.as_str())).unwrap();

        let path = store.record_path(&work_id).unwrap();
        std::fs::remove_file(&path).unwrap();
        let external = temp.path().join("external-record.json");
        let external_bytes = b"untrusted record target".to_vec();
        std::fs::write(&external, &external_bytes).unwrap();
        symlink(&external, &path).unwrap();

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
        assert_eq!(std::fs::read(&external).unwrap(), external_bytes);
    }

    #[cfg(unix)]
    #[test]
    fn admit_rejects_a_symlinked_lock_sidecar_without_following_its_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        store.ensure_root().unwrap();
        let work_id = WorkId::from_str("work-linked-lock");
        let lock_path = store.locks_dir().join("work-linked-lock.lock");
        let external = temp.path().join("external.lock");
        let external_bytes = b"untrusted lock target".to_vec();
        std::fs::write(&external, &external_bytes).unwrap();
        symlink(&external, &lock_path).unwrap();

        assert!(matches!(
            store.admit(&record(work_id.as_str())),
            Err(SessionStoreError::Io(_))
        ));
        assert_eq!(std::fs::read(&external).unwrap(), external_bytes);
        assert!(!store.record_path(&work_id).unwrap().exists());
    }

    fn blocked_record(id: &str) -> StoredJob {
        let mut job = record(id);
        job.job.state = JobState::Blocked;
        job
    }

    fn terminal_record(id: &str, state: JobState, attempts: u32) -> StoredJob {
        let mut job = record(id);
        job.job.state = state;
        job.job.attempts = attempts;
        job
    }

    #[test]
    fn unblock_moves_a_blocked_job_to_ready_and_persists_the_approval_sidecar() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-approve");
        store.admit(&blocked_record(work_id.as_str())).unwrap();
        let now = Timestamp::now();

        let event = store
            .unblock(
                &work_id,
                now,
                ApprovalActor::Operator {
                    id: "operator-a".to_owned(),
                },
                Some("looks fine".to_owned()),
            )
            .unwrap();

        assert_eq!(event.state, JobState::Ready);
        let persisted = store.get(&work_id).unwrap();
        assert_eq!(persisted.job.state, JobState::Ready);
        assert!(persisted.lease.is_none());
        assert!(persisted.completion.is_none());

        let approval = store
            .get_approval(&work_id)
            .unwrap()
            .expect("approval sidecar recorded");
        assert_eq!(approval.work_id, work_id);
        assert_eq!(approval.note.as_deref(), Some("looks fine"));
        assert_eq!(approval.revision, event.revision);
        assert!(matches!(
            approval.approved_by,
            ApprovalActor::Operator { ref id } if id == "operator-a"
        ));
    }

    #[test]
    fn get_approval_returns_none_for_a_job_that_was_never_approved() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-never-approved");
        store.admit(&record(work_id.as_str())).unwrap();

        assert_eq!(store.get_approval(&work_id).unwrap(), None);
    }

    #[test]
    fn unblock_fails_for_a_job_that_is_not_blocked_and_writes_no_sidecar() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-not-blocked");
        store.admit(&record(work_id.as_str())).unwrap();

        let error = store
            .unblock(
                &work_id,
                Timestamp::now(),
                ApprovalActor::Operator {
                    id: "operator-a".to_owned(),
                },
                None,
            )
            .unwrap_err();

        assert!(matches!(error, SessionStoreError::JobNotBlocked { .. }));
        assert_eq!(store.get_approval(&work_id).unwrap(), None);
    }

    #[test]
    fn deny_blocked_cancels_a_blocked_job_with_actor_and_reason() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-deny-blocked");
        store.admit(&blocked_record(work_id.as_str())).unwrap();

        let transition = store
            .deny_blocked(
                &work_id,
                &CancelRequest {
                    cancelled_at: Timestamp::now(),
                    cancelled_by: ApprovalActor::Operator {
                        id: "operator-a".to_owned(),
                    },
                    reason: "not safe to proceed".to_owned(),
                },
            )
            .unwrap();

        assert_eq!(transition.previous_state, JobState::Blocked);
        let persisted = store.get(&work_id).unwrap();
        assert_eq!(persisted.job.state, JobState::Cancelled);
        assert!(matches!(
            persisted.cancellation,
            Some(JobCancellation { ref reason, .. }) if reason == "not safe to proceed"
        ));
    }

    #[test]
    fn deny_blocked_rejects_a_job_that_is_not_blocked() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-deny-not-blocked");
        store.admit(&record(work_id.as_str())).unwrap();

        let error = store
            .deny_blocked(
                &work_id,
                &CancelRequest {
                    cancelled_at: Timestamp::now(),
                    cancelled_by: ApprovalActor::Operator {
                        id: "operator-a".to_owned(),
                    },
                    reason: "not safe to proceed".to_owned(),
                },
            )
            .unwrap_err();

        assert!(matches!(error, SessionStoreError::JobNotDeniable { .. }));
    }

    #[test]
    fn deny_blocked_rejects_an_empty_reason_before_touching_the_store() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-deny-empty-reason");
        store.admit(&blocked_record(work_id.as_str())).unwrap();

        let error = store
            .deny_blocked(
                &work_id,
                &CancelRequest {
                    cancelled_at: Timestamp::now(),
                    cancelled_by: ApprovalActor::Operator {
                        id: "operator-a".to_owned(),
                    },
                    reason: "   ".to_owned(),
                },
            )
            .unwrap_err();

        assert!(matches!(
            error,
            SessionStoreError::InvalidJobCancellationReason
        ));
        assert_eq!(store.get(&work_id).unwrap().job.state, JobState::Blocked);
    }

    #[test]
    fn retry_requeues_a_failed_job_and_increments_attempts() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-retry-failed");
        store
            .admit(&terminal_record(work_id.as_str(), JobState::Failed, 1))
            .unwrap();

        let event = store
            .retry(
                &work_id,
                &RetryRequest {
                    retried_at: Timestamp::now(),
                    retried_by: ApprovalActor::Operator {
                        id: "operator-a".to_owned(),
                    },
                },
            )
            .unwrap();

        assert_eq!(event.state, JobState::Ready);
        let persisted = store.get(&work_id).unwrap();
        assert_eq!(persisted.job.state, JobState::Ready);
        assert_eq!(persisted.job.attempts, 2);
        assert!(persisted.completion.is_none());
        assert!(persisted.cancellation.is_none());
    }

    #[test]
    fn retry_requeues_a_cancelled_job() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-retry-cancelled");
        store
            .admit(&terminal_record(work_id.as_str(), JobState::Cancelled, 0))
            .unwrap();

        let event = store
            .retry(
                &work_id,
                &RetryRequest {
                    retried_at: Timestamp::now(),
                    retried_by: ApprovalActor::Operator {
                        id: "operator-a".to_owned(),
                    },
                },
            )
            .unwrap();

        assert_eq!(event.state, JobState::Ready);
        assert_eq!(store.get(&work_id).unwrap().job.attempts, 1);
    }

    #[test]
    fn retry_rejects_a_job_that_is_not_failed_or_cancelled() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-retry-ready");
        store.admit(&record(work_id.as_str())).unwrap();

        let error = store
            .retry(
                &work_id,
                &RetryRequest {
                    retried_at: Timestamp::now(),
                    retried_by: ApprovalActor::Operator {
                        id: "operator-a".to_owned(),
                    },
                },
            )
            .unwrap_err();

        assert!(matches!(error, SessionStoreError::JobNotRetryable { .. }));
    }

    #[test]
    fn retry_returns_a_typed_error_and_does_not_requeue_once_the_limit_is_exhausted() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-retry-exhausted");
        // `record()`'s fixture policy has max_attempts: 2.
        store
            .admit(&terminal_record(work_id.as_str(), JobState::Failed, 2))
            .unwrap();

        let error = store
            .retry(
                &work_id,
                &RetryRequest {
                    retried_at: Timestamp::now(),
                    retried_by: ApprovalActor::Operator {
                        id: "operator-a".to_owned(),
                    },
                },
            )
            .unwrap_err();

        assert!(matches!(
            error,
            SessionStoreError::JobRetryLimitExhausted {
                attempts: 2,
                max_attempts: 2,
                ..
            }
        ));
        // The job must still be exactly as it was — no silent requeue.
        let persisted = store.get(&work_id).unwrap();
        assert_eq!(persisted.job.state, JobState::Failed);
        assert_eq!(persisted.job.attempts, 2);
    }

    #[test]
    fn retry_advances_the_fencing_epoch_so_a_stale_token_cannot_complete_the_new_attempt() {
        let temp = tempfile::tempdir().unwrap();
        let store = JobStore::new(temp.path());
        let work_id = WorkId::from_str("work-retry-fencing");
        let mut stale = terminal_record(work_id.as_str(), JobState::Failed, 1);
        // Simulate the epoch the job carried from its prior (now-stale) lease.
        stale.lease_epoch = 5;
        store.admit(&stale).unwrap();

        store
            .retry(
                &work_id,
                &RetryRequest {
                    retried_at: Timestamp::now(),
                    retried_by: ApprovalActor::Operator {
                        id: "operator-a".to_owned(),
                    },
                },
            )
            .unwrap();

        let after_retry = store.get(&work_id).unwrap();
        assert!(after_retry.lease_epoch > 5);

        // A fresh claim must issue a token beyond the pre-retry epoch, so a
        // worker still holding a token minted under epoch 5 can never match.
        let claim = store
            .claim(
                &work_id,
                &ClaimRequest {
                    worker_id: "worker-a".to_owned(),
                    lease_ttl: SignedDuration::from_secs(60),
                    now: Timestamp::now(),
                },
            )
            .unwrap();
        assert!(claim.token.epoch > 5);
    }
}
