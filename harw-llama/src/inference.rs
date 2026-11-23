//! Inference admission shared by the local providers of one device.

use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use harw_types::cancel::CancelToken;
use tokio::sync::{Mutex, OwnedMutexGuard};

/// A live image-concurrency setting and the gate for all local generations
/// sharing the same device budget. Clones share both.
#[derive(Clone, Debug)]
pub struct LocalInferenceControl {
    image_concurrency: Arc<AtomicU8>,
    gate: Arc<Mutex<()>>,
}

impl Default for LocalInferenceControl {
    fn default() -> Self {
        Self {
            image_concurrency: Arc::new(AtomicU8::new(1)),
            gate: Arc::new(Mutex::new(())),
        }
    }
}

impl LocalInferenceControl {
    /// Changes the maximum simultaneous image descriptions for future calls.
    /// Returns false and keeps the previous setting if `count` is not in 1..=3.
    #[must_use]
    pub fn set_image_concurrency(&self, count: u8) -> bool {
        if !(1..=3).contains(&count) {
            return false;
        }
        self.image_concurrency.store(count, Ordering::Relaxed);
        true
    }

    /// The number of image descriptions permitted for the next admitted call.
    #[must_use]
    pub fn image_concurrency(&self) -> usize {
        usize::from(self.image_concurrency.load(Ordering::Relaxed))
    }

    pub(crate) async fn begin(&self, cancel: Option<&CancelToken>) -> Option<InferencePermit> {
        let lock = Arc::clone(&self.gate).lock_owned();
        let guard = if let Some(cancel) = cancel {
            tokio::select! {
                biased;
                () = cancel.cancelled() => return None,
                guard = lock => guard,
            }
        } else {
            lock.await
        };
        Some(InferencePermit {
            _guard: Arc::new(guard),
            image_concurrency: self.image_concurrency(),
        })
    }
}

/// Blocking workers retain a clone: dropping a reply future must not allow
/// another turn to start while cancelled native workers are still exiting.
#[derive(Clone)]
pub(crate) struct InferencePermit {
    _guard: Arc<OwnedMutexGuard<()>>,
    image_concurrency: usize,
}

impl InferencePermit {
    pub(crate) fn image_concurrency(&self) -> usize {
        self.image_concurrency
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_types::cancel::CancelReason;

    #[test]
    fn range_is_validated_without_changing_last_selection() {
        let control = LocalInferenceControl::default();
        assert_eq!(control.image_concurrency(), 1);
        for count in [2, 3, 1, 2] {
            assert!(control.set_image_concurrency(count));
            assert_eq!(control.image_concurrency(), usize::from(count));
        }
        for count in [0, 4, u8::MAX] {
            assert!(!control.set_image_concurrency(count));
            assert_eq!(control.image_concurrency(), 2);
        }
    }

    #[tokio::test]
    async fn settings_apply_to_next_turn_after_detached_worker_releases_gate() {
        let control = LocalInferenceControl::default();
        let turn = control.begin(None).await.expect("turn admitted");
        assert_eq!(turn.image_concurrency(), 1);
        assert!(control.set_image_concurrency(3));
        assert_eq!(turn.image_concurrency(), 1);

        // Same clone a blocking worker holds if its turn future is dropped.
        let detached_worker = turn.clone();
        let (started, worker_started) = tokio::sync::oneshot::channel();
        let (release, wait_for_release) = std::sync::mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let _retained_permit = detached_worker;
            started.send(()).expect("signal worker start");
            wait_for_release.recv().expect("signal worker release");
        });
        worker_started.await.expect("worker started");
        drop(turn);
        assert!(control.gate.try_lock().is_err());

        release.send(()).expect("release worker");
        worker.await.expect("worker exited");
        let next = control
            .clone()
            .begin(None)
            .await
            .expect("next turn admitted");
        assert_eq!(next.image_concurrency(), 3);
    }

    #[tokio::test]
    async fn cancelled_turn_does_not_wait_behind_inference() {
        let control = LocalInferenceControl::default();
        let _turn = control.begin(None).await.expect("turn admitted");
        let cancel = CancelToken::new();
        cancel.cancel(CancelReason::User);
        assert!(control.begin(Some(&cancel)).await.is_none());
    }
}
