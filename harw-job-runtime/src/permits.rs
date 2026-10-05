//! A resizable permit pool: the variable semaphore behind every job lane.
//!
//! # Responsibility
//! [`ResizablePermits`] bounds how many jobs of one lane run at the same time
//! and can be resized at runtime without a restart:
//!
//! - **Raising** the limit releases waiters immediately.
//! - **Lowering** the limit never stops a running job: permits that are free
//!   are withdrawn at once, permits that are in use are withdrawn when their
//!   holder finishes (the *debt*). New acquisitions wait until enough permits
//!   are free again.
//! - The limit is clamped to `MIN_PERMITS..=MAX_PERMITS`; a zero-permit lane
//!   would leave every job queued forever.
//!
//! # Invariant
//! `available + in_use - debt == limit`, guarded by one mutex; the tokio
//! semaphore itself only ever sees `add_permits` / `forget_permits`.
//!
//! # Concurrency
//! Cheap to clone (`Arc`); all methods take `&self`. Permits are owned and
//! `Send`, so they can be held by spawned tasks.

use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Smallest permitted limit: a lane always admits at least one job.
pub const MIN_PERMITS: usize = 1;
/// Largest permitted limit (matches the `[jobs] max_running` range).
pub const MAX_PERMITS: usize = 256;

/// A snapshot of a pool, for status output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermitStatus {
    /// Configured number of concurrent holders.
    pub limit: usize,
    /// Permits currently held by running jobs.
    pub in_use: usize,
    /// Permits free right now (`0` while the pool is over its limit).
    pub available: usize,
}

impl PermitStatus {
    /// Whether more jobs run than the limit allows (a lowering is pending and
    /// drains as the running jobs finish).
    #[must_use]
    pub const fn draining(&self) -> bool {
        self.in_use > self.limit
    }
}

#[derive(Debug)]
struct State {
    limit: usize,
    // Permits to withdraw as their holders finish after a lowering.
    debt: usize,
    in_use: usize,
}

#[derive(Debug)]
struct Inner {
    semaphore: Arc<Semaphore>,
    state: Mutex<State>,
}

impl Inner {
    // A poisoned lock only means another holder panicked; the counters stay
    // consistent (every critical section is a few assignments).
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A resizable concurrency limit; see the module docs.
#[derive(Debug, Clone)]
pub struct ResizablePermits {
    inner: Arc<Inner>,
}

/// One held permit; releasing it (drop) frees a slot, or pays down the debt of
/// an earlier lowering.
#[derive(Debug)]
pub struct Permit {
    permit: Option<OwnedSemaphorePermit>,
    owner: Arc<Inner>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut state = self.owner.state();
        state.in_use = state.in_use.saturating_sub(1);
        if let Some(permit) = self.permit.take() {
            if state.debt > 0 {
                state.debt -= 1;
                permit.forget();
            }
            // Otherwise the permit returns to the semaphore as it drops.
        }
    }
}

impl ResizablePermits {
    /// A pool with `limit` permits (clamped to `MIN_PERMITS..=MAX_PERMITS`).
    #[must_use]
    pub fn new(limit: usize) -> Self {
        let limit = limit.clamp(MIN_PERMITS, MAX_PERMITS);
        Self {
            inner: Arc::new(Inner {
                semaphore: Arc::new(Semaphore::new(limit)),
                state: Mutex::new(State {
                    limit,
                    debt: 0,
                    in_use: 0,
                }),
            }),
        }
    }

    /// The configured limit.
    #[must_use]
    pub fn limit(&self) -> usize {
        self.inner.state().limit
    }

    /// Current limit, usage and free permits.
    #[must_use]
    pub fn status(&self) -> PermitStatus {
        let state = self.inner.state();
        PermitStatus {
            limit: state.limit,
            in_use: state.in_use,
            available: self.inner.semaphore.available_permits(),
        }
    }

    /// Sets the limit (clamped) and returns the limit now in effect.
    ///
    /// Raising wakes waiters immediately; lowering never interrupts a holder.
    pub fn set_limit(&self, limit: usize) -> usize {
        let target = limit.clamp(MIN_PERMITS, MAX_PERMITS);
        let mut state = self.inner.state();
        let current = state.limit;
        if target > current {
            let grow = target - current;
            let pay = state.debt.min(grow);
            state.debt -= pay;
            self.inner.semaphore.add_permits(grow - pay);
        } else if target < current {
            let shrink = current - target;
            let withdrawn = self.inner.semaphore.forget_permits(shrink);
            state.debt += shrink - withdrawn;
        }
        state.limit = target;
        target
    }

    /// A permit if one is free right now.
    #[must_use]
    pub fn try_acquire(&self) -> Option<Permit> {
        let permit = Arc::clone(&self.inner.semaphore).try_acquire_owned().ok()?;
        Some(self.wrap(permit))
    }

    /// Waits for a free permit. Returns `None` only if the pool was closed,
    /// which never happens for a pool owned through this type.
    pub async fn acquire(&self) -> Option<Permit> {
        let permit = Arc::clone(&self.inner.semaphore)
            .acquire_owned()
            .await
            .ok()?;
        Some(self.wrap(permit))
    }

    fn wrap(&self, permit: OwnedSemaphorePermit) -> Permit {
        self.inner.state().in_use += 1;
        Permit {
            permit: Some(permit),
            owner: Arc::clone(&self.inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PERMITS, MIN_PERMITS, ResizablePermits};
    use crate::test_support::{TestError, TestResult, ctx};
    use std::time::Duration;

    #[test]
    fn the_limit_is_clamped_to_at_least_one() {
        assert_eq!(ResizablePermits::new(0).limit(), MIN_PERMITS);
        let pool = ResizablePermits::new(3);
        assert_eq!(pool.set_limit(0), MIN_PERMITS);
        assert_eq!(pool.set_limit(usize::MAX), MAX_PERMITS);
    }

    #[test]
    fn status_reports_limit_used_and_free_permits() -> TestResult {
        let pool = ResizablePermits::new(3);
        let first = pool.try_acquire().ok_or(TestError::Missing("first"))?;
        let second = pool.try_acquire().ok_or(TestError::Missing("second"))?;
        let status = pool.status();
        assert_eq!((status.limit, status.in_use, status.available), (3, 2, 1));
        assert!(!status.draining());
        drop((first, second));
        assert_eq!(pool.status().in_use, 0);
        assert_eq!(pool.status().available, 3);
        Ok(())
    }

    #[tokio::test]
    async fn raising_the_limit_starts_a_waiting_job_immediately() -> TestResult {
        let pool = ResizablePermits::new(1);
        let held = pool.try_acquire().ok_or(TestError::Missing("held"))?;
        let waiter = {
            let pool = pool.clone();
            tokio::spawn(async move { pool.acquire().await.is_some() })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished(), "must wait while the pool is full");

        pool.set_limit(2);
        let started = tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .map_err(ctx("waiter starts after the limit was raised"))?
            .map_err(ctx("waiter task"))?;
        assert!(started);
        drop(held);
        Ok(())
    }

    #[test]
    fn lowering_stops_no_running_job_and_blocks_new_ones_until_enough_are_free() -> TestResult {
        let pool = ResizablePermits::new(3);
        let a = pool.try_acquire().ok_or(TestError::Missing("a"))?;
        let b = pool.try_acquire().ok_or(TestError::Missing("b"))?;
        let c = pool.try_acquire().ok_or(TestError::Missing("c"))?;

        pool.set_limit(1);
        let status = pool.status();
        assert_eq!((status.limit, status.in_use, status.available), (1, 3, 0));
        assert!(status.draining(), "three run, one is allowed");
        assert!(pool.try_acquire().is_none());

        drop(a);
        assert!(pool.try_acquire().is_none(), "2 running > limit 1");
        drop(b);
        assert!(pool.try_acquire().is_none(), "1 running == limit 1");
        assert!(!pool.status().draining());
        drop(c);
        let again = pool
            .try_acquire()
            .ok_or(TestError::Missing("after drain"))?;
        assert!(pool.try_acquire().is_none(), "the lowered limit holds");
        drop(again);
        Ok(())
    }

    #[test]
    fn lowering_withdraws_free_permits_at_once() -> TestResult {
        let pool = ResizablePermits::new(4);
        let held = pool.try_acquire().ok_or(TestError::Missing("held"))?;
        pool.set_limit(2);
        let status = pool.status();
        assert_eq!((status.limit, status.in_use, status.available), (2, 1, 1));
        let second = pool.try_acquire().ok_or(TestError::Missing("second"))?;
        assert!(pool.try_acquire().is_none());
        drop((held, second));
        assert_eq!(pool.status().available, 2);
        Ok(())
    }

    #[test]
    fn raising_after_lowering_cancels_the_pending_debt() -> TestResult {
        let pool = ResizablePermits::new(2);
        let a = pool.try_acquire().ok_or(TestError::Missing("a"))?;
        let b = pool.try_acquire().ok_or(TestError::Missing("b"))?;
        pool.set_limit(1); // debt 1
        pool.set_limit(2); // debt paid, nothing to add
        drop(a);
        let status = pool.status();
        assert_eq!((status.limit, status.in_use, status.available), (2, 1, 1));
        drop(b);
        assert_eq!(pool.status().available, 2);
        Ok(())
    }
}
