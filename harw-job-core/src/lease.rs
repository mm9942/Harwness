//! Work leases: acquire / renew / expiry against `jiff::Timestamp`
//! (knowledge-surfaces §6.2). Expiry comparison is implemented fully.

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use harw_types::WorkId;

use crate::error::{JobRuntimeError, JobRuntimeResult};

/// Opaque fencing credential for one issued worker lease.
///
/// A holder name alone is not sufficient after a restart: the same worker can
/// come back with a stale view of the world. `epoch` revokes every earlier
/// lease for a job and `nonce` makes accidental token reuse detectable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseToken {
    pub work_id: WorkId,
    pub epoch: u64,
    pub nonce: String,
}

/// A time-bounded claim on a [`WorkId`], held by a single worker identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lease {
    /// Work item this lease governs.
    pub work_id: WorkId,
    /// Opaque identity of the holder (agent/worker id as a string).
    pub holder: String,
    /// Monotonic fencing epoch issued by the durable job store.
    pub epoch: u64,
    /// Opaque per-issue nonce. This is never inferred from a worker name.
    pub nonce: String,
    /// Instant the lease was first acquired.
    pub acquired_at: Timestamp,
    /// Instant of the most recent renewal/heartbeat.
    pub heartbeat_at: Timestamp,
    /// Instant at (and after) which the lease is no longer valid.
    pub expires_at: Timestamp,
    /// Validity window applied on each acquire/renew.
    pub ttl: SignedDuration,
}

impl Lease {
    /// Acquire a fresh lease valid for `ttl` starting at `now`.
    pub fn acquire(
        work_id: WorkId,
        holder: impl Into<String>,
        now: Timestamp,
        ttl: SignedDuration,
    ) -> JobRuntimeResult<Self> {
        Self::acquire_fenced(work_id, holder, now, ttl, 0, WorkId::new().as_str())
    }

    /// Acquire a lease with a durable store-issued epoch and opaque nonce.
    ///
    /// The store owns epoch allocation. Callers that need restart-safe worker
    /// coordination must use this constructor rather than relying on the
    /// compatibility `acquire` helper.
    pub fn acquire_fenced(
        work_id: WorkId,
        holder: impl Into<String>,
        now: Timestamp,
        ttl: SignedDuration,
        epoch: u64,
        nonce: impl Into<String>,
    ) -> JobRuntimeResult<Self> {
        // Reject invalid policy input before any expiration arithmetic or
        // state transition can occur; zero/negative TTLs fail closed.
        if ttl <= SignedDuration::ZERO {
            return Err(JobRuntimeError::LeaseExpired {
                work_id,
                expired_at: now,
            });
        }
        let expires_at = now.checked_add(ttl)?;
        Ok(Self {
            work_id,
            holder: holder.into(),
            epoch,
            nonce: nonce.into(),
            acquired_at: now,
            heartbeat_at: now,
            expires_at,
            ttl,
        })
    }

    /// Returns the exact token a caller must present for renew/complete.
    #[must_use]
    pub fn token(&self) -> LeaseToken {
        LeaseToken {
            work_id: self.work_id.clone(),
            epoch: self.epoch,
            nonce: self.nonce.clone(),
        }
    }

    /// Checks the full fencing credential, not just the worker's display name.
    #[must_use]
    pub fn matches_token(&self, token: &LeaseToken) -> bool {
        self.work_id == token.work_id && self.epoch == token.epoch && self.nonce == token.nonce
    }

    /// Return `true` when the lease is no longer valid at `now`.
    #[must_use]
    pub fn is_expired(&self, now: Timestamp) -> bool {
        now >= self.expires_at
    }

    /// Extend the lease by its `ttl` from `now`; errors if already expired.
    pub fn renew(&mut self, now: Timestamp) -> JobRuntimeResult<()> {
        if self.is_expired(now) {
            return Err(JobRuntimeError::LeaseExpired {
                work_id: self.work_id.clone(),
                expired_at: self.expires_at,
            });
        }
        self.expires_at = now.checked_add(self.ttl)?;
        self.heartbeat_at = now;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn fenced_lease_requires_the_exact_epoch_and_nonce() -> TestResult {
        let now = Timestamp::now();
        let lease = Lease::acquire_fenced(
            WorkId::from_str("work-1"),
            "worker-a",
            now,
            SignedDuration::from_secs(60),
            4,
            "nonce-current",
        )
        .map_err(ctx("fenced lease acquisition must succeed"))?;
        assert!(lease.matches_token(&lease.token()));

        let mut stale_epoch = lease.token();
        stale_epoch.epoch = 3;
        assert!(!lease.matches_token(&stale_epoch));
        let mut stale_nonce = lease.token();
        stale_nonce.nonce = "nonce-stale".to_owned();
        assert!(!lease.matches_token(&stale_nonce));
        Ok(())
    }

    #[test]
    fn lease_rejects_non_positive_ttls() -> TestResult {
        // Use the upper timestamp bound so calculating `now + ttl` first would
        // produce a time-arithmetic error instead of rejecting the policy.
        let now = Timestamp::MAX;
        for ttl in [SignedDuration::ZERO, SignedDuration::from_secs(-1)] {
            let Err(error) = Lease::acquire(WorkId::from_str("work-1"), "worker-a", now, ttl)
            else {
                return Err(TestError::Unexpected(
                    "non-positive TTL must fail before expiration arithmetic".to_owned(),
                ));
            };
            assert!(matches!(
                error,
                JobRuntimeError::LeaseExpired { expired_at, .. } if expired_at == now
            ));
        }
        Ok(())
    }
}
