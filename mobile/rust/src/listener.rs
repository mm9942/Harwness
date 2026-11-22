//! Change notifications from the core to the platform UI.
//!
//! The core never calls foreign code under one of its own locks. Changes are
//! queued to one dedicated thread, coalesced for a short window and delivered
//! there, so a slow or misbehaving listener cannot stall the session machinery.

use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

/// Window in which further changes are merged into one delivery.
const COALESCE: Duration = Duration::from_millis(20);

/// Receives change hints from the core. Every method only says that something
/// changed; the platform re-reads the state it cares about. Calls come from one
/// dedicated non-tokio thread, are coalesced and never made under a core lock.
#[uniffi::export(with_foreign)]
pub trait CoreListener: Send + Sync {
    /// The connection status (phase, route, host) changed.
    fn status_changed(&self);
    /// The session list changed.
    fn sessions_changed(&self);
    /// The window of one open session changed.
    fn session_changed(&self, session_id: String);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Status,
    Sessions,
    Session(String),
}

#[derive(Default)]
struct Slot {
    listener: Mutex<Option<Arc<dyn CoreListener>>>,
    /// Held by the worker for one whole delivery.
    calling: Mutex<()>,
    /// Set by the worker when it starts.
    worker: OnceLock<ThreadId>,
}

#[derive(Default)]
pub(crate) struct Notifier {
    slot: Arc<Slot>,
    /// Not shared with the worker, so the worker exits when the notifier drops.
    sender: Mutex<Option<mpsc::Sender<Change>>>,
}

impl Notifier {
    pub(crate) fn set(&self, listener: Arc<dyn CoreListener>) {
        {
            let mut guard = self
                .slot
                .listener
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *guard = Some(listener);
        }
        self.status();
        self.sessions();
    }

    /// After this returns the old listener is never called again, except when
    /// called from inside a callback, where it cannot wait for itself.
    pub(crate) fn clear(&self) {
        {
            let mut guard = self
                .slot
                .listener
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *guard = None;
        }
        if self.slot.worker.get() == Some(&thread::current().id()) {
            return;
        }
        drop(self.slot.calling.lock().unwrap_or_else(PoisonError::into_inner));
    }

    pub(crate) fn notify(&self, change: Change) {
        let has_listener = self
            .slot
            .listener
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some();
        if !has_listener {
            return;
        }
        let mut sender = self.sender.lock().unwrap_or_else(PoisonError::into_inner);
        if sender.is_none() {
            let (tx, rx) = mpsc::channel();
            let slot = Arc::clone(&self.slot);
            let spawned = thread::Builder::new()
                .name("harw-mobile-notify".into())
                .spawn(move || worker(&slot, &rx));
            if spawned.is_err() {
                return;
            }
            *sender = Some(tx);
        }
        let failed = sender
            .as_ref()
            .is_some_and(|tx| tx.send(change).is_err());
        if failed {
            *sender = None;
        }
    }

    pub(crate) fn status(&self) {
        self.notify(Change::Status);
    }

    pub(crate) fn sessions(&self) {
        self.notify(Change::Sessions);
    }

    pub(crate) fn session(&self, id: &str) {
        self.notify(Change::Session(id.to_owned()));
    }
}

#[derive(Default)]
struct Pending {
    status: bool,
    sessions: bool,
    ids: BTreeSet<String>,
}

impl Pending {
    fn add(&mut self, change: Change) {
        match change {
            Change::Status => self.status = true,
            Change::Sessions => self.sessions = true,
            Change::Session(id) => {
                self.ids.insert(id);
            }
        }
    }
}

fn worker(slot: &Slot, rx: &mpsc::Receiver<Change>) {
    let _ = slot.worker.set(thread::current().id());
    loop {
        let Ok(first) = rx.recv() else {
            return;
        };
        let mut pending = Pending::default();
        pending.add(first);
        let deadline = Instant::now() + COALESCE;
        let mut closed = false;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining) {
                Ok(change) => pending.add(change),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    closed = true;
                    break;
                }
            }
        }
        deliver(slot, pending);
        if closed {
            return;
        }
    }
}

fn deliver(slot: &Slot, pending: Pending) {
    let _calling = slot.calling.lock().unwrap_or_else(PoisonError::into_inner);
    let listener = slot
        .listener
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let Some(listener) = listener else {
        return;
    };
    // A panicking listener must not take the worker down.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if pending.status {
            listener.status_changed();
        }
        if pending.sessions {
            listener.sessions_changed();
        }
        for id in pending.ids {
            listener.session_changed(id);
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Polls `condition` every 10 ms for at most 5 s.
    fn wait_until(condition: impl Fn() -> bool) -> bool {
        for _ in 0..500 {
            if condition() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        condition()
    }

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<String>>,
        threads: Mutex<Vec<Option<String>>>,
        delay: Duration,
        finished: AtomicBool,
        started: AtomicBool,
    }

    impl Recorder {
        fn record(&self, call: String) {
            self.started.store(true, Ordering::SeqCst);
            thread::sleep(self.delay);
            lock(&self.calls).push(call);
            lock(&self.threads).push(thread::current().name().map(str::to_owned));
            self.finished.store(true, Ordering::SeqCst);
        }

        fn calls(&self) -> Vec<String> {
            lock(&self.calls).clone()
        }
    }

    impl CoreListener for Recorder {
        fn status_changed(&self) {
            self.record("status".into());
        }
        fn sessions_changed(&self) {
            self.record("sessions".into());
        }
        fn session_changed(&self, session_id: String) {
            self.record(format!("session:{session_id}"));
        }
    }

    #[test]
    fn notify_without_listener_spawns_nothing() {
        let notifier = Notifier::default();
        notifier.status();
        notifier.sessions();
        notifier.session("s1");
        assert!(lock(&notifier.sender).is_none());
        assert!(notifier.slot.worker.get().is_none());
    }

    #[test]
    fn calls_run_on_the_notify_thread() -> TestResult {
        let notifier = Notifier::default();
        let recorder = Arc::new(Recorder::default());
        notifier.set(recorder.clone());
        assert!(wait_until(|| recorder.calls().len() >= 2));
        let threads = lock(&recorder.threads).clone();
        assert!(threads
            .iter()
            .all(|name| name.as_deref() == Some("harw-mobile-notify")));
        Ok(())
    }

    #[test]
    fn bursts_are_coalesced() -> TestResult {
        let notifier = Notifier::default();
        let recorder = Arc::new(Recorder::default());
        notifier.set(recorder.clone());
        for index in 0..100 {
            notifier.status();
            notifier.session(&format!("s{index:03}"));
        }
        let expected = (0..100).map(|i| format!("session:s{i:03}")).collect::<Vec<_>>();
        assert!(wait_until(|| {
            let calls = recorder.calls();
            expected.iter().all(|id| calls.contains(id))
        }));
        let status_calls = recorder.calls().iter().filter(|c| *c == "status").count();
        assert!(status_calls < 100, "{status_calls} status calls");
        Ok(())
    }

    #[test]
    fn clear_waits_for_an_inflight_call_and_stops_delivery() -> TestResult {
        let notifier = Notifier::default();
        let recorder = Arc::new(Recorder {
            delay: Duration::from_millis(100),
            ..Recorder::default()
        });
        notifier.set(recorder.clone());
        assert!(wait_until(|| recorder.started.load(Ordering::SeqCst)));
        recorder.finished.store(false, Ordering::SeqCst);
        notifier.clear();
        assert!(
            recorder.finished.load(Ordering::SeqCst),
            "clear returned while a call was running"
        );
        let before = recorder.calls().len();
        notifier.status();
        notifier.session("late");
        thread::sleep(Duration::from_millis(250));
        assert_eq!(recorder.calls().len(), before);
        Ok(())
    }

    struct Panicker {
        calls: AtomicUsize,
    }

    impl CoreListener for Panicker {
        fn status_changed(&self) {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("listener failure");
            }
        }
        fn sessions_changed(&self) {}
        fn session_changed(&self, _session_id: String) {}
    }

    #[test]
    fn panicking_listener_does_not_stop_the_worker() -> TestResult {
        let notifier = Notifier::default();
        let listener = Arc::new(Panicker {
            calls: AtomicUsize::new(0),
        });
        notifier.set(listener.clone());
        assert!(wait_until(|| listener.calls.load(Ordering::SeqCst) >= 1));
        notifier.status();
        assert!(wait_until(|| listener.calls.load(Ordering::SeqCst) >= 2));
        Ok(())
    }

    struct SelfClearing {
        notifier: Arc<Notifier>,
        done: AtomicBool,
    }

    impl CoreListener for SelfClearing {
        fn status_changed(&self) {
            self.notifier.clear();
            self.done.store(true, Ordering::SeqCst);
        }
        fn sessions_changed(&self) {}
        fn session_changed(&self, _session_id: String) {}
    }

    #[test]
    fn clear_from_inside_a_callback_does_not_deadlock() -> TestResult {
        let notifier = Arc::new(Notifier::default());
        let listener = Arc::new(SelfClearing {
            notifier: Arc::clone(&notifier),
            done: AtomicBool::new(false),
        });
        notifier.set(listener.clone());
        assert!(wait_until(|| listener.done.load(Ordering::SeqCst)));
        Ok(())
    }
}
