//! Pure, I/O-free lease and lifecycle mutations of one [`StoredJob`].
//!
//! Each function runs inside [`crate::RecordStore::update_locked`]: it gets
//! the freshly loaded record under the record lock, mutates it, and returns
//! either the result (the record is then persisted) or an error (nothing is
//! persisted, so partial mutations before an error are harmless).
//!
//! # Fencing
//! Every claim advances `lease_epoch` and embeds it in the lease token
//! ([`FencingToken`]). Cancellation and reclaim advance it again, so a token
//! from an earlier holder can never match the record's current epoch. A
//! mutation carrying a stale token is rejected with
//! [`StoreError::StaleLease`] and writes nothing (Job-Runtime-Doc §7, §21.4
//! "stale lease attempts mutation").

use harw_job_core::{
    FencingToken, JobCancellation, JobClaim, JobCompletion, JobOutcome, JobRuntimeError, JobState,
    Lease, LeaseToken, StoredJob,
};
use jiff::{SignedDuration, Timestamp};

use crate::error::{Conflict, StaleLease, StoreError, StoreResult};

/// The record's current fencing token (its last issued lease epoch).
#[must_use]
pub fn current_fencing(record: &StoredJob) -> FencingToken {
    FencingToken::new(record.lease_epoch)
}

/// The fencing token a lease token carries.
#[must_use]
pub fn token_fencing(token: &LeaseToken) -> FencingToken {
    FencingToken::new(token.epoch)
}

/// Returns the record's current lease if `token` still authorizes a write
/// at `now`.
///
/// # Errors
/// [`StoreError::StaleLease`]: no lease ([`StaleLease::Missing`]), a
/// mismatching token or superseded fencing epoch
/// ([`StaleLease::TokenMismatch`]), or an expired lease
/// ([`StaleLease::Expired`]).
pub fn verify_lease<'a>(
    record: &'a mut StoredJob,
    token: &LeaseToken,
    now: Timestamp,
) -> StoreResult<&'a mut Lease> {
    let current = current_fencing(record);
    let id = record.job.id.clone();
    let Some(lease) = record.lease.as_mut() else {
        return Err(StoreError::stale(id.as_str(), StaleLease::Missing));
    };
    if !lease.matches_token(token) || !token_fencing(token).admits(current) {
        return Err(StoreError::stale(id.as_str(), StaleLease::TokenMismatch));
    }
    if lease.is_expired(now) {
        return Err(StoreError::stale(
            id.as_str(),
            StaleLease::Expired {
                expired_at: lease.expires_at,
            },
        ));
    }
    Ok(lease)
}

/// Maps a job-model error: an expired lease is a lost lease, everything else
/// a rejected transition.
#[must_use]
pub fn job_error(id: &str, error: JobRuntimeError) -> StoreError {
    match error {
        JobRuntimeError::LeaseExpired { expired_at, .. } => {
            StoreError::stale(id, StaleLease::Expired { expired_at })
        }
        error => StoreError::conflict(
            id,
            Conflict::Rejected {
                detail: error.to_string(),
            },
        ),
    }
}

/// Terminal state a completion outcome leads to.
#[must_use]
pub fn outcome_state(outcome: &JobOutcome) -> JobState {
    match outcome {
        JobOutcome::Succeeded { .. } => JobState::Completed,
        JobOutcome::Failed { .. } => JobState::Failed,
        JobOutcome::Cancelled { .. } => JobState::Cancelled,
        JobOutcome::Blocked { .. } => JobState::Blocked,
    }
}

/// Whether `state` admits no further completion.
#[must_use]
pub fn is_terminal(state: JobState) -> bool {
    matches!(
        state,
        JobState::Completed | JobState::Failed | JobState::Cancelled
    )
}

/// Claims a `Ready`, eligible job for `holder`: advances the fencing epoch,
/// issues a fenced lease with `nonce`, sets `Running`, bumps the revision.
///
/// # Errors
/// [`Conflict::InvalidLeaseTtl`], [`Conflict::NotClaimable`],
/// [`Conflict::NotEligible`], [`Conflict::EpochExhausted`], or a mapped
/// job-model error ([`job_error`]).
pub fn claim(
    record: &mut StoredJob,
    holder: &str,
    lease_ttl: SignedDuration,
    now: Timestamp,
    nonce: String,
) -> StoreResult<JobClaim> {
    let id = record.job.id.clone();
    if lease_ttl <= SignedDuration::ZERO {
        return Err(StoreError::conflict(id.as_str(), Conflict::InvalidLeaseTtl));
    }
    if record.job.state != JobState::Ready {
        return Err(StoreError::conflict(
            id.as_str(),
            Conflict::NotClaimable {
                state: record.job.state,
            },
        ));
    }
    if now < record.not_before {
        return Err(StoreError::conflict(
            id.as_str(),
            Conflict::NotEligible {
                not_before: record.not_before,
            },
        ));
    }
    let epoch = current_fencing(record)
        .next()
        .ok_or_else(|| StoreError::conflict(id.as_str(), Conflict::EpochExhausted))?
        .get();
    let lease = Lease::acquire_fenced(id.clone(), holder, now, lease_ttl, epoch, nonce)
        .map_err(|error| job_error(id.as_str(), error))?;
    record.job.state = JobState::Running;
    record.job.updated_at = now;
    record.lease_epoch = epoch;
    record.lease = Some(lease.clone());
    record.revision = record.revision.saturating_add(1);
    Ok(JobClaim {
        job: record.job.clone(),
        scope: record.scope.clone(),
        token: lease.token(),
        lease,
    })
}

/// Extends the current lease by its TTL from `now` (heartbeat).
///
/// # Errors
/// [`StoreError::StaleLease`] from [`verify_lease`] or the lease itself.
pub fn renew(record: &mut StoredJob, token: &LeaseToken, now: Timestamp) -> StoreResult<Lease> {
    let renewed = {
        let id = record.job.id.clone();
        let lease = verify_lease(record, token, now)?;
        lease
            .renew(now)
            .map_err(|error| job_error(id.as_str(), error))?;
        lease.clone()
    };
    record.revision = record.revision.saturating_add(1);
    Ok(renewed)
}

/// Completes a non-terminal job exactly once with the current, unexpired
/// lease; the lease is dropped and the outcome recorded.
///
/// # Errors
/// [`StaleLease::TokenMismatch`] if the token names another job,
/// [`Conflict::AlreadyTerminal`], or [`verify_lease`] errors.
pub fn complete(
    record: &mut StoredJob,
    token: &LeaseToken,
    completed_at: Timestamp,
    outcome: JobOutcome,
) -> StoreResult<JobCompletion> {
    if token.work_id != record.job.id {
        return Err(StoreError::stale(
            record.job.id.as_str(),
            StaleLease::TokenMismatch,
        ));
    }
    if is_terminal(record.job.state) {
        return Err(StoreError::conflict(
            record.job.id.as_str(),
            Conflict::AlreadyTerminal {
                state: record.job.state,
            },
        ));
    }
    verify_lease(record, token, completed_at)?;
    let completion = JobCompletion {
        completed_at,
        outcome,
    };
    record.job.state = outcome_state(&completion.outcome);
    record.job.updated_at = completed_at;
    record.lease = None;
    record.completion = Some(completion.clone());
    record.revision = record.revision.saturating_add(1);
    Ok(completion)
}

/// Result of [`cancel`].
#[derive(Debug, Clone, PartialEq)]
pub struct Cancelled {
    /// State before the cancellation.
    pub previous_state: JobState,
    /// The lease that was revoked; an outer supervisor signals its holder.
    pub prior_lease: Option<Lease>,
    /// The recorded terminal completion.
    pub completion: JobCompletion,
}

/// Cancels a `Pending`, `Ready` or `Running` job. A running job's fencing
/// epoch is advanced before its lease is returned, so the old holder can
/// neither renew nor complete afterwards.
///
/// # Errors
/// [`Conflict::EmptyReason`], [`Conflict::NotCancellable`],
/// [`Conflict::EpochExhausted`].
pub fn cancel(record: &mut StoredJob, cancellation: JobCancellation) -> StoreResult<Cancelled> {
    let id = record.job.id.clone();
    if cancellation.reason.trim().is_empty() {
        return Err(StoreError::conflict(id.as_str(), Conflict::EmptyReason));
    }
    let previous_state = record.job.state;
    if !matches!(
        previous_state,
        JobState::Pending | JobState::Ready | JobState::Running
    ) {
        return Err(StoreError::conflict(
            id.as_str(),
            Conflict::NotCancellable {
                state: previous_state,
            },
        ));
    }
    let prior_lease = record.lease.take();
    if previous_state == JobState::Running {
        record.lease_epoch = record
            .lease_epoch
            .checked_add(1)
            .ok_or_else(|| StoreError::conflict(id.as_str(), Conflict::EpochExhausted))?;
    }
    let completion = JobCompletion {
        completed_at: cancellation.cancelled_at,
        outcome: JobOutcome::Cancelled {
            reason: cancellation.reason.clone(),
        },
    };
    record.job.state = JobState::Cancelled;
    record.job.updated_at = cancellation.cancelled_at;
    record.completion = Some(completion.clone());
    record.cancellation = Some(cancellation);
    record.revision = record.revision.saturating_add(1);
    Ok(Cancelled {
        previous_state,
        prior_lease,
        completion,
    })
}

/// Result of [`reclaim`] and [`expire_if_due`].
#[derive(Debug, Clone, PartialEq)]
pub struct Reclaimed {
    /// The revoked lease (the zombie holder to cancel/observe).
    pub expired_lease: Lease,
    /// When the job becomes claimable again; `None` if the retry budget is
    /// exhausted and the job is now terminally `Failed`.
    pub retry_scheduled_for: Option<Timestamp>,
}

/// Explicitly takes a `Running` job away from its (live or dead) holder: the
/// lease is dropped, the fencing epoch advanced, and a failed attempt
/// recorded. With retry budget left the job is `Ready` again at
/// `now + backoff`; otherwise it is `Failed` with `exhausted_reason`.
///
/// # Errors
/// A mapped job-model error (not `Running`, unrepresentable backoff),
/// [`Conflict::Rejected`] if a `Running` job carries no lease,
/// [`Conflict::EpochExhausted`].
pub fn reclaim(
    record: &mut StoredJob,
    now: Timestamp,
    exhausted_reason: &str,
) -> StoreResult<Reclaimed> {
    let id = record.job.id.clone();
    let prior_lease = record.lease.take();
    let retry_scheduled_for = match record.job.record_failure(now) {
        Ok(delay) => Some(
            now.checked_add(delay)
                .map_err(|error| job_error(id.as_str(), JobRuntimeError::Time(error)))?,
        ),
        Err(JobRuntimeError::RetryExhausted { .. }) => {
            record.completion = Some(JobCompletion {
                completed_at: now,
                outcome: JobOutcome::Failed {
                    reason: exhausted_reason.to_owned(),
                },
            });
            None
        }
        Err(error) => return Err(job_error(id.as_str(), error)),
    };
    let Some(expired_lease) = prior_lease else {
        return Err(StoreError::conflict(
            id.as_str(),
            Conflict::Rejected {
                detail: "job is Running but carries no lease to reclaim".to_owned(),
            },
        ));
    };
    record.lease_epoch = record
        .lease_epoch
        .checked_add(1)
        .ok_or_else(|| StoreError::conflict(id.as_str(), Conflict::EpochExhausted))?;
    if let Some(not_before) = retry_scheduled_for {
        record.not_before = not_before;
    }
    record.revision = record.revision.saturating_add(1);
    Ok(Reclaimed {
        expired_lease,
        retry_scheduled_for,
    })
}

/// Revokes the lease of a `Running` job only if it has expired at `now`
/// (restart reconciliation). `Ok(None)`: nothing to do, nothing changed.
///
/// # Errors
/// [`Conflict::Rejected`] if the retry schedule cannot be computed.
pub fn expire_if_due(
    record: &mut StoredJob,
    now: Timestamp,
    exhausted_reason: &str,
) -> StoreResult<Option<Reclaimed>> {
    let Some(lease) = record.lease.clone() else {
        return Ok(None);
    };
    if record.job.state != JobState::Running || !lease.is_expired(now) {
        return Ok(None);
    }
    let id = record.job.id.clone();
    record.lease = None;
    record.lease_epoch = record.lease_epoch.saturating_add(1);
    let rejected =
        |detail: String| StoreError::conflict(id.as_str(), Conflict::Rejected { detail });
    let retry_scheduled_for = match record.job.record_failure(now) {
        Ok(delay) => Some(
            now.checked_add(delay)
                .map_err(|error| rejected(error.to_string()))?,
        ),
        Err(JobRuntimeError::RetryExhausted { .. }) => {
            record.completion = Some(JobCompletion {
                completed_at: now,
                outcome: JobOutcome::Failed {
                    reason: exhausted_reason.to_owned(),
                },
            });
            None
        }
        Err(error) => return Err(rejected(error.to_string())),
    };
    if let Some(not_before) = retry_scheduled_for {
        record.not_before = not_before;
    }
    record.revision = record.revision.saturating_add(1);
    Ok(Some(Reclaimed {
        expired_lease: lease,
        retry_scheduled_for,
    }))
}
