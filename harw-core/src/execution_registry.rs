//! Live execution controls fenced by the durable worker lease token.
//!
//! The job store is the authority that issues and revokes [`LeaseToken`]s.
//! This module is the in-process bridge from that durable fence to a running
//! child.  In particular, it never indexes an execution by `holder`: a
//! restarted worker may reuse that display name while carrying a stale epoch
//! or nonce.
//!
//! # Key types
//! - [`JobExecutionRegistry`]: token-keyed map of live executions.
//! - [`ExecutionGuard`]: RAII binding that unregisters its exact token on drop
//!   (A-JOBRUN, F-071/G-021), so an aborted runner future cannot leave a
//!   dangling live execution behind.
//!
//! # Concurrency
//! The registry is `Send + Sync`; a `std::sync::Mutex` is held only for map
//! access and never across an `.await`.

use harw_job_core::LeaseToken;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use tracing::warn;

/// A cancellation handle owned by the execution runner.
///
/// `completion` must yield `true` when the underlying child/session has
/// stopped.  The registry only requests cooperative cancellation and, after
/// a bounded grace period, invokes `force_abort`; it does not own a process
/// identifier or infer one from a worker name.
pub trait ExecutionControl: Send + Sync {
    /// Ask the child to stop cooperatively.
    fn request_graceful_cancel(&self);

    /// Stop the child after the cooperative grace period elapsed.
    fn force_abort(&self);

    /// Subscribe to the child completion signal.  The receiver's current
    /// value must already be `true` when the child has completed.
    fn completion(&self) -> watch::Receiver<bool>;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ExecutionKey {
    work_id: String,
    epoch: u64,
    nonce: String,
}

impl From<&LeaseToken> for ExecutionKey {
    fn from(token: &LeaseToken) -> Self {
        Self {
            work_id: token.work_id.as_str().to_owned(),
            epoch: token.epoch,
            nonce: token.nonce.clone(),
        }
    }
}

struct ExecutionEntry {
    token: LeaseToken,
    control: Arc<dyn ExecutionControl>,
}

/// The result of trying to cancel one fenced execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancellationResult {
    /// No live execution is bound to this exact token.  This is non-fatal:
    /// after a restart, the durable fence can outlive the in-memory process.
    Missing,
    /// The child acknowledged completion during the grace period.
    Graceful,
    /// The child did not complete in time and was force-aborted.
    ForceAborted,
}

/// Errors from registry mutation. Cancellation of an unknown token is not an
/// error and is represented by [`CancellationResult::Missing`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionRegistryError {
    LockPoisoned,
    DuplicateToken { work_id: String, epoch: u64 },
}

impl fmt::Display for ExecutionRegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockPoisoned => f.write_str("live execution registry lock is poisoned"),
            Self::DuplicateToken { work_id, epoch } => {
                write!(
                    f,
                    "execution lease token already registered for {work_id} at epoch {epoch}"
                )
            }
        }
    }
}

impl std::error::Error for ExecutionRegistryError {}

/// In-process registry of currently running jobs.
///
/// The map key contains all three fencing components (`work_id`, `epoch`, and
/// `nonce`).  `LeaseToken` intentionally remains a wire type without a hash
/// implementation, so the private key avoids weakening that public contract
/// while retaining exact-token semantics.
#[derive(Default)]
pub struct JobExecutionRegistry {
    entries: Mutex<HashMap<ExecutionKey, ExecutionEntry>>,
}

impl JobExecutionRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind one live execution to its exact durable lease token.
    pub fn register(
        &self,
        token: LeaseToken,
        control: Arc<dyn ExecutionControl>,
    ) -> Result<(), ExecutionRegistryError> {
        let key = ExecutionKey::from(&token);
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| ExecutionRegistryError::LockPoisoned)?;
        if entries.contains_key(&key) {
            return Err(ExecutionRegistryError::DuplicateToken {
                work_id: token.work_id.to_string(),
                epoch: token.epoch,
            });
        }
        entries.insert(key, ExecutionEntry { token, control });
        Ok(())
    }

    /// Binds one live execution and returns an RAII guard for the binding.
    ///
    /// # Description
    /// Identical to [`JobExecutionRegistry::register`], but the returned
    /// [`ExecutionGuard`] unregisters the exact token when it is dropped
    /// without an explicit [`ExecutionGuard::release`]. This is the binding the
    /// durable runner uses, so cancelling or aborting the runner future frees
    /// the live execution slot.
    ///
    /// # Arguments
    /// - `token` (`LeaseToken`): the complete fencing credential, moved in.
    /// - `control` (`Arc<dyn ExecutionControl>`): the cancellation handle.
    ///
    /// # Returns
    /// An [`ExecutionGuard`] owning a pointer clone of this registry.
    ///
    /// # Errors
    /// - [`ExecutionRegistryError::DuplicateToken`]: the token is already bound.
    /// - [`ExecutionRegistryError::LockPoisoned`]: the map lock is poisoned.
    ///
    /// # Concurrency
    /// Takes the internal mutex briefly; does not await.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use harw_core::JobExecutionRegistry;
    /// let registry = Arc::new(JobExecutionRegistry::new());
    /// // let guard = registry.register_guarded(token, control)?;
    /// // drop(guard); // unregisters the exact token
    /// ```
    pub fn register_guarded(
        self: &Arc<Self>,
        token: LeaseToken,
        control: Arc<dyn ExecutionControl>,
    ) -> Result<ExecutionGuard, ExecutionRegistryError> {
        self.register(token.clone(), control)?;
        Ok(ExecutionGuard {
            registry: Arc::clone(self),
            token,
            armed: true,
        })
    }

    /// Remove a live binding only when the full token matches.
    pub fn unregister(&self, token: &LeaseToken) -> Result<bool, ExecutionRegistryError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| ExecutionRegistryError::LockPoisoned)?;
        Ok(entries.remove(&ExecutionKey::from(token)).is_some())
    }

    /// Returns `true` when a live execution is currently bound to this exact
    /// fencing token.
    ///
    /// # Description
    /// A cheap synchronous probe over the full `(work_id, epoch, nonce)`
    /// fencing identity. It exists so a cancellation sink can decide
    /// `Requested` vs `Unavailable` without awaiting the bounded grace period
    /// that [`JobExecutionRegistry::cancel`] performs. A poisoned lock is
    /// treated as "not present" rather than panicking.
    ///
    /// # Arguments
    /// - `token` (`&LeaseToken`): the exact fencing credential to look up. A
    ///   stale `epoch` or `nonce` never matches a current execution.
    ///
    /// # Returns
    /// `true` if and only if a live execution is registered under the exact
    /// token; `false` otherwise (including a poisoned lock).
    ///
    /// # Concurrency
    /// Acquires the internal `Mutex` briefly and releases it before returning;
    /// it does not await, spawn, or hold the lock across a suspension point.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_core::JobExecutionRegistry;
    /// let registry = JobExecutionRegistry::new();
    /// // `registry.contains(&token)` is false until an execution is registered.
    /// ```
    #[must_use]
    pub fn contains(&self, token: &LeaseToken) -> bool {
        self.entries
            .lock()
            .map(|entries| entries.contains_key(&ExecutionKey::from(token)))
            .unwrap_or(false)
    }

    /// Request cancellation, then force-abort after `grace` if completion was
    /// not observed.  A missing binding is deliberately non-fatal.
    pub async fn cancel(
        &self,
        token: &LeaseToken,
        grace: Duration,
    ) -> Result<CancellationResult, ExecutionRegistryError> {
        let entry = {
            let entries = self
                .entries
                .lock()
                .map_err(|_| ExecutionRegistryError::LockPoisoned)?;
            entries.get(&ExecutionKey::from(token)).map(|entry| {
                // Keep the token in the entry as an assertion against future
                // changes to key construction and clone only the handle.
                debug_assert_eq!(entry.token, *token);
                Arc::clone(&entry.control)
            })
        };
        let Some(control) = entry else {
            return Ok(CancellationResult::Missing);
        };

        control.request_graceful_cancel();
        let mut completion = control.completion();
        if *completion.borrow() {
            self.unregister(token)?;
            return Ok(CancellationResult::Graceful);
        }
        let completed = tokio::time::timeout(grace, async {
            loop {
                if completion.changed().await.is_err() || *completion.borrow() {
                    break;
                }
            }
        })
        .await
        .is_ok();
        if completed {
            self.unregister(token)?;
            Ok(CancellationResult::Graceful)
        } else {
            control.force_abort();
            self.unregister(token)?;
            Ok(CancellationResult::ForceAborted)
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .map(|entries| entries.len())
            .unwrap_or(0)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// RAII binding of one live execution to its exact lease token.
///
/// # Description
/// Created by [`JobExecutionRegistry::register_guarded`]. Dropping an armed
/// guard removes the binding (a poisoned lock is logged, never panics);
/// [`ExecutionGuard::release`] removes it explicitly and reports the result.
///
/// # Concurrency
/// `Send + Sync`; holds an `Arc` to the registry and a cloned token.
#[must_use = "dropping the guard immediately unregisters the execution"]
pub struct ExecutionGuard {
    registry: Arc<JobExecutionRegistry>,
    token: LeaseToken,
    armed: bool,
}

impl ExecutionGuard {
    /// Returns the exact fencing token this guard keeps registered.
    #[must_use]
    pub fn token(&self) -> &LeaseToken {
        &self.token
    }

    /// Unregisters the binding now and disarms the drop hook.
    ///
    /// # Returns
    /// `Ok(true)` when the binding was still present, `Ok(false)` when it had
    /// already been removed (for example by [`JobExecutionRegistry::cancel`]).
    ///
    /// # Errors
    /// - [`ExecutionRegistryError::LockPoisoned`]: the map lock is poisoned.
    pub fn release(mut self) -> Result<bool, ExecutionRegistryError> {
        self.armed = false;
        self.registry.unregister(&self.token)
    }
}

impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Err(error) = self.registry.unregister(&self.token) {
            warn!(
                work_id = %self.token.work_id,
                epoch = self.token.epoch,
                error = %error,
                "execution guard could not unregister on drop"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;
    use harw_types::WorkId;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeControl {
        graceful: AtomicUsize,
        forced: AtomicUsize,
        completion: watch::Sender<bool>,
        receiver: watch::Receiver<bool>,
    }

    impl FakeControl {
        fn new(completed: bool) -> Arc<Self> {
            let (completion, receiver) = watch::channel(completed);
            Arc::new(Self {
                graceful: AtomicUsize::new(0),
                forced: AtomicUsize::new(0),
                completion,
                receiver,
            })
        }

        fn complete(&self) {
            self.completion.send_replace(true);
        }
    }

    impl ExecutionControl for FakeControl {
        fn request_graceful_cancel(&self) {
            self.graceful.fetch_add(1, Ordering::SeqCst);
        }

        fn force_abort(&self) {
            self.forced.fetch_add(1, Ordering::SeqCst);
            self.completion.send_replace(true);
        }

        fn completion(&self) -> watch::Receiver<bool> {
            self.receiver.clone()
        }
    }

    fn token(epoch: u64, nonce: &str) -> LeaseToken {
        LeaseToken {
            work_id: WorkId::from_str("work-1"),
            epoch,
            nonce: nonce.to_owned(),
        }
    }

    #[tokio::test]
    async fn stale_epoch_or_nonce_cannot_cancel_current_execution() -> TestResult {
        let registry = JobExecutionRegistry::new();
        let control = FakeControl::new(false);
        registry.register(token(2, "current"), control.clone())?;

        assert_eq!(
            registry
                .cancel(&token(1, "current"), Duration::from_millis(1))
                .await?,
            CancellationResult::Missing
        );
        assert_eq!(
            registry
                .cancel(&token(2, "stale"), Duration::from_millis(1))
                .await?,
            CancellationResult::Missing
        );
        assert_eq!(control.graceful.load(Ordering::SeqCst), 0);
        assert_eq!(registry.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn missing_binding_is_nonfatal() -> TestResult {
        let registry = JobExecutionRegistry::new();
        assert_eq!(
            registry
                .cancel(&token(9, "gone"), Duration::from_millis(1))
                .await?,
            CancellationResult::Missing
        );
        Ok(())
    }

    #[tokio::test]
    async fn graceful_completion_removes_binding() -> TestResult {
        let registry = JobExecutionRegistry::new();
        let control = FakeControl::new(false);
        registry.register(token(3, "graceful"), control.clone())?;
        let completion = control.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(2)).await;
            completion.complete();
        });
        assert_eq!(
            registry
                .cancel(&token(3, "graceful"), Duration::from_millis(100))
                .await?,
            CancellationResult::Graceful
        );
        assert_eq!(control.graceful.load(Ordering::SeqCst), 1);
        assert_eq!(control.forced.load(Ordering::SeqCst), 0);
        assert!(registry.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn grace_timeout_force_aborts_and_removes_binding() -> TestResult {
        let registry = JobExecutionRegistry::new();
        let control = FakeControl::new(false);
        registry.register(token(4, "timeout"), control.clone())?;
        assert_eq!(
            registry
                .cancel(&token(4, "timeout"), Duration::from_millis(2))
                .await?,
            CancellationResult::ForceAborted
        );
        assert_eq!(control.graceful.load(Ordering::SeqCst), 1);
        assert_eq!(control.forced.load(Ordering::SeqCst), 1);
        assert!(registry.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn contains_matches_only_the_exact_token() -> TestResult {
        let registry = JobExecutionRegistry::new();
        let control = FakeControl::new(false);
        registry.register(token(2, "current"), control.clone())?;

        assert!(registry.contains(&token(2, "current")));
        assert!(!registry.contains(&token(1, "current")));
        assert!(!registry.contains(&token(2, "stale")));

        assert!(registry.unregister(&token(2, "current"))?);
        assert!(!registry.contains(&token(2, "current")));
        Ok(())
    }

    #[test]
    fn test_register_guarded_drop_unregisters_exact_token() -> TestResult {
        let registry = Arc::new(JobExecutionRegistry::new());
        let control = FakeControl::new(false);
        let guard = registry.register_guarded(token(5, "guarded"), control)?;
        assert!(registry.contains(&token(5, "guarded")));
        assert_eq!(guard.token(), &token(5, "guarded"));
        drop(guard);
        assert!(registry.is_empty());
        Ok(())
    }

    #[test]
    fn test_register_guarded_release_reports_presence() -> TestResult {
        let registry = Arc::new(JobExecutionRegistry::new());
        let guard = registry.register_guarded(token(6, "release"), FakeControl::new(false))?;
        assert!(guard.release()?);
        assert!(registry.is_empty());

        let guard = registry.register_guarded(token(7, "gone"), FakeControl::new(false))?;
        assert!(registry.unregister(&token(7, "gone"))?);
        assert!(!guard.release()?);
        Ok(())
    }

    #[test]
    fn test_register_guarded_rejects_duplicate_token() -> TestResult {
        let registry = Arc::new(JobExecutionRegistry::new());
        let _guard = registry.register_guarded(token(8, "dup"), FakeControl::new(false))?;
        assert!(matches!(
            registry.register_guarded(token(8, "dup"), FakeControl::new(false)),
            Err(ExecutionRegistryError::DuplicateToken { epoch: 8, .. })
        ));
        assert_eq!(registry.len(), 1);
        Ok(())
    }
}
