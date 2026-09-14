//! Hierarchical cooperative cancellation for turns, children and jobs.
//!
//! [`CancelToken`] is the crate-wide handle turn-loop, child-controller and
//! job code use to observe and request an abort. It exists to close F-160 /
//! G-017 (harwness-analyse, `x-findings-register-w1-w3.md` / `-w4.md`): the
//! turn-loop today has no cancel token at all, and Ctrl+C in the TUI is
//! merely buffered instead of aborting a running turn.
//!
//! # Responsibility
//! This module owns exactly the cancellation primitive: creating root
//! tokens, deriving child tokens, requesting cancellation with a reason,
//! reading the reason back, and awaiting cancellation. It does not decide
//! *when* to cancel (budget checks, `Ctrl+C` handling, lease loss detection
//! all live in their own call sites) and does not itself run any I/O.
//!
//! # Hierarchy contract
//! A token created via [`CancelToken::child`] is cancelled whenever its
//! parent (or any ancestor) is cancelled, but cancelling a child never
//! cancels its parent. If a child observes cancellation without having been
//! cancelled directly itself, [`CancelToken::reason`] reports
//! [`CancelReason::Parent`] — the concrete ancestor reason is not threaded
//! through, only the fact that the cancellation originated above it. A token
//! that was cancelled directly (root or child) keeps its own reason even if
//! an ancestor is cancelled afterwards with a different reason: the first
//! reason recorded for a given token always wins, and `cancel` is
//! idempotent.
//!
//! # Key types
//! - [`CancelReason`]: why a token was cancelled.
//! - [`CancelToken`]: the cloneable, shareable cancellation handle.
//!
//! # Concurrency
//! [`CancelToken`] is `Clone + Send + Sync` (via `tokio_util::sync::CancellationToken`,
//! itself built on an atomically refcounted tree, plus an `Arc<OnceLock<CancelReason>>`
//! for the reason). Cloning a token yields another handle to the *same* node
//! (cancelling one cancels the other, matching
//! `tokio_util::sync::CancellationToken::clone`); use [`CancelToken::child`]
//! to get a genuinely dependent-but-independent token. `cancel` and
//! `is_cancelled` never block; `cancelled` is an `async fn` that suspends
//! the calling task until cancellation is requested (immediately resolving
//! if already cancelled).
//!
//! # Errors
//! This module has no fallible operations and defines no error type.
//!
//! # Examples
//! ```
//! use harw_core::cancel::{CancelReason, CancelToken};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let parent = CancelToken::new();
//! let child = parent.child();
//!
//! parent.cancel(CancelReason::Shutdown);
//!
//! assert!(child.is_cancelled());
//! assert_eq!(child.reason(), Some(CancelReason::Parent));
//! assert_eq!(parent.reason(), Some(CancelReason::Shutdown));
//!
//! // Already cancelled, so this resolves immediately.
//! child.cancelled().await;
//! # }
//! ```

use std::sync::{Arc, OnceLock};

use tokio_util::sync::CancellationToken;

/// Why a [`CancelToken`] was cancelled.
///
/// # Description
/// Every call site that requests cancellation picks exactly one variant.
/// Only the first reason recorded on a given token is kept (see
/// [`CancelToken::cancel`]); later calls with a different reason are no-ops
/// with respect to the recorded reason (the underlying token stays
/// cancelled either way).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelReason {
    /// A human explicitly requested the abort (e.g. `Ctrl+C` in the TUI).
    User,
    /// Inherited: an ancestor token was cancelled and this token has no
    /// earlier reason of its own.
    Parent,
    /// A wall-clock, token or tool-call budget was exceeded.
    Budget,
    /// The durable worker lease backing this execution was lost or expired.
    LeaseLost,
    /// The process is shutting down (`SIGTERM`/`SIGINT` at the top level,
    /// graceful drain).
    Shutdown,
}

/// A cloneable, hierarchical cancellation handle.
///
/// # Description
/// Wraps a [`tokio_util::sync::CancellationToken`] for the actual
/// wake/propagate mechanics and adds a first-write-wins
/// [`CancelReason`] alongside it. See the module docs for the exact
/// parent/child and reason-precedence contract.
#[derive(Clone, Debug)]
pub struct CancelToken {
    inner: CancellationToken,
    reason: Arc<OnceLock<CancelReason>>,
}

impl CancelToken {
    /// Creates a new, non-cancelled root token with no reason set.
    ///
    /// # Returns
    /// A fresh [`CancelToken`] with no parent; cancelling any token derived
    /// from it via [`CancelToken::child`] never affects this one.
    ///
    /// # Concurrency
    /// Allocates a new `Arc`-backed node; safe to call from any thread.
    ///
    /// # Examples
    /// ```
    /// use harw_core::cancel::CancelToken;
    ///
    /// let token = CancelToken::new();
    /// assert!(!token.is_cancelled());
    /// assert_eq!(token.reason(), None);
    /// ```
    pub fn new() -> Self {
        Self {
            inner: CancellationToken::new(),
            reason: Arc::new(OnceLock::new()),
        }
    }

    /// Creates a token that is cancelled whenever `self` (or one of its own
    /// ancestors) is cancelled, without the reverse holding.
    ///
    /// # Returns
    /// A new [`CancelToken`] dependent on `self`. If `self` is already
    /// cancelled, the returned token is created already cancelled.
    ///
    /// # Concurrency
    /// Safe to call from any thread; does not block.
    ///
    /// # Examples
    /// ```
    /// use harw_core::cancel::{CancelReason, CancelToken};
    ///
    /// let parent = CancelToken::new();
    /// let child = parent.child();
    ///
    /// child.cancel(CancelReason::Budget);
    /// assert!(child.is_cancelled());
    /// assert!(!parent.is_cancelled());
    /// ```
    pub fn child(&self) -> Self {
        Self {
            inner: self.inner.child_token(),
            reason: Arc::new(OnceLock::new()),
        }
    }

    /// Requests cancellation of this token and every token derived from it
    /// via [`CancelToken::child`].
    ///
    /// # Arguments
    /// - `reason` (`CancelReason`): why cancellation is being requested.
    ///
    /// # Description
    /// Idempotent: the first call to `cancel` on a given token (or on a
    /// clone sharing its reason cell) fixes the reason [`CancelToken::reason`]
    /// will report; subsequent calls — with the same or a different reason —
    /// still guarantee the token is cancelled but do not change the
    /// recorded reason. This deliberately discards the `Result` of the
    /// underlying `OnceLock::set`: a losing writer is expected whenever two
    /// call sites race to cancel the same token, and "first reason wins" is
    /// exactly the contract this module documents.
    ///
    /// # Concurrency
    /// Safe to call concurrently from multiple threads/tasks; wakes every
    /// task awaiting [`CancelToken::cancelled`] on this token or a
    /// descendant.
    ///
    /// # Examples
    /// ```
    /// use harw_core::cancel::{CancelReason, CancelToken};
    ///
    /// let token = CancelToken::new();
    /// token.cancel(CancelReason::User);
    /// token.cancel(CancelReason::Budget); // no-op for the recorded reason
    /// assert_eq!(token.reason(), Some(CancelReason::User));
    /// ```
    pub fn cancel(&self, reason: CancelReason) {
        // First reason wins by contract; a losing `set` here just means
        // another call (possibly concurrent) already recorded one.
        let _ = self.reason.set(reason);
        self.inner.cancel();
    }

    /// Returns the reason this token was cancelled for, if any.
    ///
    /// # Returns
    /// - `Some(reason)` with the reason recorded directly on this token via
    ///   [`CancelToken::cancel`], if any was recorded.
    /// - `Some(CancelReason::Parent)` if this token is cancelled (because an
    ///   ancestor was cancelled) but no reason was ever recorded directly on
    ///   it.
    /// - `None` if this token is not cancelled.
    ///
    /// # Concurrency
    /// Safe to call from any thread; does not block.
    ///
    /// # Examples
    /// ```
    /// use harw_core::cancel::{CancelReason, CancelToken};
    ///
    /// let parent = CancelToken::new();
    /// let child = parent.child();
    /// assert_eq!(child.reason(), None);
    ///
    /// parent.cancel(CancelReason::LeaseLost);
    /// assert_eq!(child.reason(), Some(CancelReason::Parent));
    /// ```
    pub fn reason(&self) -> Option<CancelReason> {
        if let Some(reason) = self.reason.get() {
            return Some(*reason);
        }
        if self.inner.is_cancelled() {
            return Some(CancelReason::Parent);
        }
        None
    }

    /// Returns `true` if this token (or an ancestor it derives from) has
    /// been cancelled.
    ///
    /// # Returns
    /// `true` once cancellation has been requested anywhere on this token's
    /// ancestor chain; `false` otherwise.
    ///
    /// # Concurrency
    /// Safe to call from any thread; does not block.
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    /// Awaits cancellation of this token.
    ///
    /// # Description
    /// Suspends the calling task until [`CancelToken::cancel`] is called on
    /// this token or on an ancestor it was derived from. Resolves
    /// immediately if the token is already cancelled at the time of the
    /// call.
    ///
    /// # Concurrency
    /// Intended to be used with `tokio::select!` alongside the work being
    /// guarded, so cancellation can race a normal completion path.
    ///
    /// # Examples
    /// ```
    /// use harw_core::cancel::{CancelReason, CancelToken};
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// let token = CancelToken::new();
    /// token.cancel(CancelReason::Shutdown);
    /// token.cancelled().await; // returns immediately: already cancelled
    /// # }
    /// ```
    pub async fn cancelled(&self) {
        self.inner.cancelled().await;
    }
}

impl Default for CancelToken {
    /// Equivalent to [`CancelToken::new`].
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_is_not_cancelled() {
        let token = CancelToken::new();
        assert!(!token.is_cancelled());
        assert_eq!(token.reason(), None);
    }

    #[test]
    fn test_default_matches_new() {
        let token = CancelToken::default();
        assert!(!token.is_cancelled());
        assert_eq!(token.reason(), None);
    }

    #[test]
    fn test_parent_cancel_propagates_to_child_with_reason_parent() {
        let parent = CancelToken::new();
        let child = parent.child();

        parent.cancel(CancelReason::Shutdown);

        assert!(parent.is_cancelled());
        assert!(child.is_cancelled());
        assert_eq!(parent.reason(), Some(CancelReason::Shutdown));
        assert_eq!(child.reason(), Some(CancelReason::Parent));
    }

    #[test]
    fn test_child_cancel_does_not_propagate_to_parent() {
        let parent = CancelToken::new();
        let child = parent.child();

        child.cancel(CancelReason::Budget);

        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
        assert_eq!(parent.reason(), None);
        assert_eq!(child.reason(), Some(CancelReason::Budget));
    }

    #[test]
    fn test_child_keeps_own_reason_when_parent_cancels_afterward() {
        let parent = CancelToken::new();
        let child = parent.child();

        child.cancel(CancelReason::LeaseLost);
        parent.cancel(CancelReason::User);

        // The child already had its own reason before the parent cancelled;
        // that reason must not be overwritten by the inherited `Parent`.
        assert_eq!(child.reason(), Some(CancelReason::LeaseLost));
        assert_eq!(parent.reason(), Some(CancelReason::User));
    }

    #[test]
    fn test_cancel_first_reason_wins_and_is_idempotent() {
        let token = CancelToken::new();

        token.cancel(CancelReason::User);
        token.cancel(CancelReason::Budget);
        token.cancel(CancelReason::Shutdown);

        assert!(token.is_cancelled());
        assert_eq!(token.reason(), Some(CancelReason::User));
    }

    #[tokio::test]
    async fn test_cancelled_future_completes_after_cancel() {
        let token = CancelToken::new();
        let waiter = token.clone();

        let handle = tokio::spawn(async move {
            waiter.cancelled().await;
        });

        token.cancel(CancelReason::Shutdown);

        handle
            .await
            .expect("cancelled() task must not panic or be aborted");
    }

    #[tokio::test]
    async fn test_cancelled_future_resolves_immediately_if_already_cancelled() {
        let token = CancelToken::new();
        token.cancel(CancelReason::User);

        // Must not hang: the token is already cancelled before `.await`.
        token.cancelled().await;
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn test_cancelled_future_completes_via_parent_cancellation() {
        let parent = CancelToken::new();
        let child = parent.child();

        let handle = tokio::spawn(async move {
            child.cancelled().await;
        });

        parent.cancel(CancelReason::Budget);

        handle
            .await
            .expect("child cancelled() task must not panic or be aborted");
    }
}
