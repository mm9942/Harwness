//! Shared lifetime of an admitted tool effect, including blocking work.
//!
//! This is not authority and grants no permissions. A trusted dispatcher
//! attaches an opaque guard whose destruction releases an existing lease.
//! Executors must retain a clone in work that outlives their returned future.

use std::any::Any;
use std::fmt;
use std::sync::{Arc, Mutex};

/// Opaque guard released only after its last holder is dropped.
///
/// The mutex makes an arbitrary `Send` guard shareable without exposing or
/// accessing it. It is never locked; it exists only to give the owned guard
/// the `Sync` boundary required by cloned execution contexts. There is no
/// serialization or authority-restoration API.
#[derive(Clone)]
pub struct ToolLifetimeGuard {
    inner: Arc<Mutex<Box<dyn Any + Send>>>,
}

impl ToolLifetimeGuard {
    /// Holds a runtime-issued guard until all effect/context copies end.
    #[must_use]
    pub fn new(guard: Box<dyn Any + Send>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(guard)),
        }
    }
}

impl fmt::Debug for ToolLifetimeGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolLifetimeGuard")
            .field("holders", &Arc::strong_count(&self.inner))
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountDrop(Arc<AtomicUsize>);
    impl Drop for CountDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn last_holder_releases_exactly_once() {
        let count = Arc::new(AtomicUsize::new(0));
        let guard = ToolLifetimeGuard::new(Box::new(CountDrop(Arc::clone(&count))));
        let effect = guard.clone();
        drop(guard);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        drop(effect);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
