//! Crypto execution off the Tokio executor: [`CryptoWorkerHandle`].
//!
//! # Why
//!
//! CryptGuard's `CryptoService::call` runs the synchronous
//! `CryptoProvider::execute` inline, and `network_handle` drives it from a
//! single `tower::buffer` worker *task*. Every ML-KEM/ML-DSA operation
//! therefore runs on a Tokio executor thread and blocks it for the duration
//! of the operation; the `ConcurrencyLimit` inside the buffer is a no-op
//! because the inner future is always ready.
//!
//! [`CryptoWorkerHandle`] replaces that stack. It moves the provider onto
//! dedicated OS threads (`harw-kms-crypto-{i}`) fed by a bounded
//! [`std::sync::mpsc::sync_channel`]. Each request carries a
//! [`tokio::sync::oneshot`] sender for its reply, so the async side only
//! awaits a channel and never runs crypto.
//!
//! # Threads and provider ownership
//!
//! [`CryptoWorkerHandle::spawn`] starts **one** thread that owns the
//! provider outright (default). `CryptoProvider::execute` takes `&mut self`,
//! and the hub's provider stack (`KmsGuardProvider` in [`crate::guard`])
//! relies on `execute` calls being serialized, so one thread is the correct
//! choice for a stateful provider such as `InMemoryProvider`: requests are
//! executed strictly one at a time, exactly as behind the buffer worker.
//!
//! [`CryptoWorkerHandle::spawn_pool`] starts `N` threads, each with its
//! **own** provider built by a factory. That is only correct for providers
//! without per-instance key state (for example a client of an external,
//! internally synchronized HSM/KMS); `N` independent `InMemoryProvider`s
//! would be `N` disjoint key stores.
//!
//! # Load shedding and error mapping
//!
//! Errors are [`BoxError`]s that always contain a [`CryptoServiceError`], so
//! `crypt_guard_service::service_error` (used by `CryptoHttpService`)
//! recovers the typed error by downcast:
//!
//! | Situation | Error |
//! |---|---|
//! | queue full (`poll_ready` or `call`) | [`CryptoServiceError::Overloaded`]: the request was **not** executed |
//! | all worker threads gone | [`CryptoServiceError::Unavailable`] |
//! | reply lost (worker panicked mid-request) | [`CryptoServiceError::Unavailable`] |
//! | per-request timeout elapsed | [`CryptoServiceError::Unavailable`] |
//! | provider error | passed through unchanged |
//!
//! A timeout maps to `Unavailable`, not `Overloaded`, on purpose:
//! `Overloaded` promises the request was not executed, but a request that
//! was already running when the timer fired still completes (a mutation may
//! commit, just like a lost response). A request whose caller has given up
//! (timeout or dropped future) *before* a worker dequeues it is discarded
//! unexecuted. Both classes are retryable (HTTP 503 with `Retry-After`).
//!
//! `poll_ready` never waits: it reports `Overloaded` immediately when the
//! queue is at capacity. Unlike the Tower convention, such an error is not
//! terminal for the handle; the next `poll_ready` succeeds again once the
//! queue drains (`CryptoHttpService` clones the handle per request anyway).
//! The provider's own `CryptoProvider::poll_ready` is not consulted; the
//! providers used by the hub are always ready.
//!
//! # Shutdown
//!
//! Dropping the last handle closes the channel. Jobs already queued are
//! still executed (their callers hold only the reply receiver, not a
//! handle); once the queue is empty every worker exits and drops its
//! provider, which zeroizes key material.
//!
//! The returned futures use [`tokio::time::timeout`] when a request timeout
//! is configured, so they must be polled inside a Tokio runtime with the
//! time driver enabled.

use core::fmt;
use core::future::Future;
use core::num::NonZeroUsize;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crypt_guard_service::{
    CryptoProvider, CryptoRequest, CryptoResponse, CryptoServiceError, Service,
};
use tokio::sync::oneshot;

/// Boxed error returned by [`CryptoWorkerHandle`]; always wraps a
/// [`CryptoServiceError`].
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Future returned by [`CryptoWorkerHandle::call`].
pub type CryptoWorkerFuture =
    Pin<Box<dyn Future<Output = Result<CryptoResponse, BoxError>> + Send + 'static>>;

/// Default queue depth, matching CryptGuard's `StackConfig::default()`.
pub const DEFAULT_QUEUE_BOUND: usize = 128;

/// Default per-request timeout.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Configuration of the crypto worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CryptoWorkerConfig {
    /// Requests that may wait in the queue (not counting the ones being
    /// executed) before callers get [`CryptoServiceError::Overloaded`].
    pub queue_bound: NonZeroUsize,
    /// Upper bound on how long a caller waits for its reply (queueing plus
    /// execution). `None` waits indefinitely.
    pub request_timeout: Option<Duration>,
}

impl Default for CryptoWorkerConfig {
    fn default() -> Self {
        Self {
            queue_bound: NonZeroUsize::new(DEFAULT_QUEUE_BOUND).unwrap_or(NonZeroUsize::MIN),
            request_timeout: Some(DEFAULT_REQUEST_TIMEOUT),
        }
    }
}

type Reply = Result<CryptoResponse, CryptoServiceError>;

/// One queued request and the channel for its reply.
struct Job {
    request: CryptoRequest,
    reply: oneshot::Sender<Reply>,
}

/// State shared by all clones of a handle.
struct Shared {
    /// Jobs sent but not yet dequeued by a worker. Incremented *before*
    /// `try_send` and decremented on failure or dequeue, so it never
    /// underflows and is never below the real queue length.
    depth: AtomicUsize,
    queue_bound: usize,
    request_timeout: Option<Duration>,
}

/// Cloneable handle to crypto worker threads.
///
/// Implements `Service<CryptoRequest>` with [`BoxError`] errors, so it can
/// replace CryptGuard's `NetworkHandle` inside `CryptoHttpService`. Cloning
/// copies a channel sender and an `Arc`; no key material is reachable from
/// this type.
#[derive(Clone)]
pub struct CryptoWorkerHandle {
    sender: SyncSender<Job>,
    shared: Arc<Shared>,
}

impl fmt::Debug for CryptoWorkerHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CryptoWorkerHandle")
            .field("queue_bound", &self.shared.queue_bound)
            .field("request_timeout", &self.shared.request_timeout)
            .field("queued", &self.shared.depth.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl CryptoWorkerHandle {
    /// Start one worker thread (`harw-kms-crypto-0`) that owns `provider`.
    ///
    /// This is the right choice for stateful providers: all requests are
    /// executed one at a time on that thread.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the OS refuses to create the thread; the
    /// provider is dropped in that case.
    pub fn spawn<P: CryptoProvider>(provider: P, config: CryptoWorkerConfig) -> io::Result<Self> {
        Self::spawn_workers(core::iter::once(provider), config)
    }

    /// Start `threads` worker threads (`harw-kms-crypto-{i}`), each owning a
    /// provider built by `factory`.
    ///
    /// Only use this for providers without per-instance key state; see the
    /// module documentation.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the OS refuses to create a thread. Threads
    /// already started exit on their own because the channel closes.
    pub fn spawn_pool<P, F>(
        threads: NonZeroUsize,
        mut factory: F,
        config: CryptoWorkerConfig,
    ) -> io::Result<Self>
    where
        P: CryptoProvider,
        F: FnMut() -> P,
    {
        Self::spawn_workers((0..threads.get()).map(|_| factory()), config)
    }

    fn spawn_workers<P, I>(providers: I, config: CryptoWorkerConfig) -> io::Result<Self>
    where
        P: CryptoProvider,
        I: Iterator<Item = P>,
    {
        let (sender, receiver) = sync_channel::<Job>(config.queue_bound.get());
        let receiver = Arc::new(Mutex::new(receiver));
        let shared = Arc::new(Shared {
            depth: AtomicUsize::new(0),
            queue_bound: config.queue_bound.get(),
            request_timeout: config.request_timeout,
        });
        for (index, provider) in providers.enumerate() {
            let queue = Arc::clone(&receiver);
            let worker_shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name(format!("harw-kms-crypto-{index}"))
                .spawn(move || worker_loop(provider, &queue, &worker_shared))?;
        }
        Ok(Self { sender, shared })
    }

    /// Jobs currently waiting in the queue (advisory; for metrics and tests).
    pub fn queued(&self) -> usize {
        self.shared.depth.load(Ordering::Acquire)
    }
}

/// Body of one worker thread: take jobs until the channel closes.
fn worker_loop<P: CryptoProvider>(mut provider: P, queue: &Mutex<Receiver<Job>>, shared: &Shared) {
    loop {
        // Hold the lock only while waiting for the next job, never while
        // executing, so other workers can pick up work in the meantime.
        let job = {
            let Ok(receiver) = queue.lock() else {
                return;
            };
            match receiver.recv() {
                Ok(job) => job,
                // All handles dropped and the queue is empty: shut down.
                Err(_) => return,
            }
        };
        shared.depth.fetch_sub(1, Ordering::AcqRel);
        if job.reply.is_closed() {
            // The caller timed out or went away before we started: do not
            // execute. Dropping the request zeroizes its secret inputs.
            continue;
        }
        let Job { request, reply } = job;
        let result = provider.execute(request);
        // If the caller is gone, the response (possibly plaintext) is
        // dropped and zeroized here.
        drop(reply.send(result));
    }
    // `provider` is dropped when the loop returns, zeroizing key material.
}

fn boxed(err: CryptoServiceError) -> BoxError {
    Box::new(err)
}

impl Service<CryptoRequest> for CryptoWorkerHandle {
    type Response = CryptoResponse;
    type Error = BoxError;
    type Future = CryptoWorkerFuture;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.shared.depth.load(Ordering::Acquire) >= self.shared.queue_bound {
            Poll::Ready(Err(boxed(CryptoServiceError::Overloaded)))
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn call(&mut self, request: CryptoRequest) -> Self::Future {
        let (reply, response) = oneshot::channel();
        self.shared.depth.fetch_add(1, Ordering::AcqRel);
        let enqueued = match self.sender.try_send(Job { request, reply }) {
            Ok(()) => Ok(()),
            Err(err) => {
                self.shared.depth.fetch_sub(1, Ordering::AcqRel);
                // `err` owns the job; dropping it zeroizes the request.
                Err(match err {
                    TrySendError::Full(_) => CryptoServiceError::Overloaded,
                    TrySendError::Disconnected(_) => CryptoServiceError::Unavailable,
                })
            }
        };
        let request_timeout = self.shared.request_timeout;
        Box::pin(async move {
            enqueued.map_err(boxed)?;
            let outcome = match request_timeout {
                Some(limit) => match tokio::time::timeout(limit, response).await {
                    Ok(outcome) => outcome,
                    // Dropping `response` marks the job abandoned; a worker
                    // that has not started it yet skips it.
                    Err(_elapsed) => return Err(boxed(CryptoServiceError::Unavailable)),
                },
                None => response.await,
            };
            match outcome {
                Ok(result) => result.map_err(boxed),
                // The worker dropped the reply sender without answering
                // (it panicked while executing).
                Err(_closed) => Err(boxed(CryptoServiceError::Unavailable)),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use core::future::poll_fn;
    use core::num::NonZeroUsize;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use crypt_guard_service::{
        CryptoContext, CryptoOperation, CryptoProvider, CryptoRequest, CryptoResponse,
        CryptoServiceError, Decrypt, DescribeKey, Encrypt, GenerateKey, InMemoryProvider,
        KeyAlgorithm, KeyId, KeyNamespace, KeyRef, NullProvider, RequestId, SecretBytes, Service,
        VerificationResult, pq_hpke::DEFAULT_SUITE, service_error,
    };
    use tokio::sync::mpsc as tokio_mpsc;

    use super::{BoxError, CryptoWorkerConfig, CryptoWorkerHandle};
    use crate::test_support::{TestError, TestResult, ctx};

    const WAIT: Duration = Duration::from_secs(10);

    fn config(queue_bound: usize, timeout: Option<Duration>) -> TestResult<CryptoWorkerConfig> {
        Ok(CryptoWorkerConfig {
            queue_bound: NonZeroUsize::new(queue_bound).ok_or(TestError::Missing("queue bound"))?,
            request_timeout: timeout,
        })
    }

    fn key(id: &str) -> TestResult<KeyRef> {
        Ok(KeyRef::latest(
            KeyNamespace::new("test").map_err(ctx("namespace"))?,
            KeyId::new(id).map_err(ctx("key id"))?,
        ))
    }

    fn describe(id: &str) -> TestResult<CryptoOperation> {
        Ok(CryptoOperation::Describe(DescribeKey { key: key(id)? }))
    }

    fn crypto_context() -> CryptoContext {
        CryptoContext {
            info: Box::from(&b"harw-test-info"[..]),
            aad: Box::from(&b"harw-test-aad"[..]),
        }
    }

    /// `poll_ready` + `call`, exactly like `CryptoHttpService` does it.
    async fn exec(
        handle: &mut CryptoWorkerHandle,
        id: u128,
        op: CryptoOperation,
    ) -> Result<CryptoResponse, BoxError> {
        poll_fn(|cx| handle.poll_ready(cx)).await?;
        handle.call(CryptoRequest::new(RequestId(id), op)).await
    }

    async fn exec_typed(
        handle: &mut CryptoWorkerHandle,
        id: u128,
        op: CryptoOperation,
    ) -> Result<CryptoResponse, CryptoServiceError> {
        exec(handle, id, op).await.map_err(service_error)
    }

    /// Generate an HPKE key, encrypt, decrypt, and check the round trip.
    async fn round_trip(handle: &mut CryptoWorkerHandle, base: u128, id: &str) -> TestResult {
        let generated = exec_typed(
            handle,
            base,
            CryptoOperation::Generate(GenerateKey {
                namespace: KeyNamespace::new("test").map_err(ctx("namespace"))?,
                id: KeyId::new(id).map_err(ctx("key id"))?,
                algorithm: KeyAlgorithm::Hpke {
                    suite: DEFAULT_SUITE,
                },
            }),
        )
        .await
        .map_err(ctx("generate"))?;
        let key = match generated {
            CryptoResponse::KeyCreated { key, .. } => key,
            other => return Err(TestError::Unexpected(format!("generate: {other:?}"))),
        };
        let plaintext = format!("secret payload {id}");
        let encrypted = exec_typed(
            handle,
            base + 1,
            CryptoOperation::Encrypt(Encrypt {
                key: key.clone(),
                plaintext: SecretBytes::copy_from_slice(plaintext.as_bytes()),
                context: crypto_context(),
            }),
        )
        .await
        .map_err(ctx("encrypt"))?;
        let ciphertext = match encrypted {
            CryptoResponse::Ciphertext(blob) => blob,
            other => return Err(TestError::Unexpected(format!("encrypt: {other:?}"))),
        };
        let decrypted = exec_typed(
            handle,
            base + 2,
            CryptoOperation::Decrypt(Decrypt {
                key,
                ciphertext,
                context: crypto_context(),
            }),
        )
        .await
        .map_err(ctx("decrypt"))?;
        match decrypted {
            CryptoResponse::Plaintext(secret) if secret.as_ref() == plaintext.as_bytes() => Ok(()),
            other => Err(TestError::Unexpected(format!("decrypt: {other:?}"))),
        }
    }

    /// Test provider: reports each start, then blocks until the test opens
    /// the gate (bounded, so a regression fails instead of hanging).
    struct GateProvider {
        started: tokio_mpsc::UnboundedSender<()>,
        gate: std::sync::mpsc::Receiver<()>,
        executions: Arc<AtomicUsize>,
    }

    impl CryptoProvider for GateProvider {
        fn execute(
            &mut self,
            _request: CryptoRequest,
        ) -> Result<CryptoResponse, CryptoServiceError> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            let _ = self.started.send(());
            self.gate
                .recv_timeout(WAIT)
                .map_err(|_| CryptoServiceError::Internal)?;
            Ok(CryptoResponse::Verification(VerificationResult::Valid))
        }
    }

    struct Gate {
        handle: CryptoWorkerHandle,
        started: tokio_mpsc::UnboundedReceiver<()>,
        open: std::sync::mpsc::Sender<()>,
        executions: Arc<AtomicUsize>,
    }

    fn gate(config: CryptoWorkerConfig) -> TestResult<Gate> {
        let (started_tx, started) = tokio_mpsc::unbounded_channel();
        let (open, gate_rx) = std::sync::mpsc::channel();
        let executions = Arc::new(AtomicUsize::new(0));
        let provider = GateProvider {
            started: started_tx,
            gate: gate_rx,
            executions: Arc::clone(&executions),
        };
        let handle = CryptoWorkerHandle::spawn(provider, config).map_err(ctx("spawn worker"))?;
        Ok(Gate {
            handle,
            started,
            open,
            executions,
        })
    }

    async fn wait_started(started: &mut tokio_mpsc::UnboundedReceiver<()>) -> TestResult {
        tokio::time::timeout(WAIT, started.recv())
            .await
            .map_err(ctx("wait for execution start"))?
            .ok_or(TestError::Missing("start signal"))
    }

    #[tokio::test]
    async fn in_memory_generate_encrypt_decrypt_round_trip() -> TestResult {
        let mut handle =
            CryptoWorkerHandle::spawn(InMemoryProvider::new(), CryptoWorkerConfig::default())
                .map_err(ctx("spawn worker"))?;
        round_trip(&mut handle, 1, "k1").await
    }

    #[tokio::test]
    async fn many_concurrent_requests_on_current_thread_runtime() -> TestResult {
        let handle = CryptoWorkerHandle::spawn(InMemoryProvider::new(), config(64, Some(WAIT))?)
            .map_err(ctx("spawn worker"))?;
        let mut tasks = tokio::task::JoinSet::new();
        for i in 0..16_u128 {
            let mut handle = handle.clone();
            tasks.spawn(async move { round_trip(&mut handle, i * 10, &format!("k{i}")).await });
        }
        while let Some(joined) = tasks.join_next().await {
            joined.map_err(ctx("join task"))??;
        }
        Ok(())
    }

    #[tokio::test]
    async fn executor_keeps_running_while_crypto_executes() -> TestResult {
        // Default `#[tokio::test]` is a current_thread runtime: if `execute`
        // ran on the executor, the timer below could never fire before the
        // gate opens, and the gate is only opened after the timer fired.
        let Gate {
            handle,
            mut started,
            open,
            ..
        } = gate(config(4, Some(WAIT))?)?;
        let mut caller = handle.clone();
        let op = describe("k")?;
        let pending = tokio::spawn(async move { exec_typed(&mut caller, 1, op).await });
        wait_started(&mut started).await?;
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::time::sleep(Duration::from_millis(10)),
        )
        .await
        .map_err(ctx("timer fires while crypto runs"))?;
        open.send(()).map_err(ctx("open gate"))?;
        let response = pending
            .await
            .map_err(ctx("join caller"))?
            .map_err(ctx("gated request"))?;
        match response {
            CryptoResponse::Verification(VerificationResult::Valid) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[tokio::test]
    async fn full_queue_is_overloaded() -> TestResult {
        let Gate {
            mut handle,
            mut started,
            open,
            executions,
        } = gate(config(1, Some(WAIT))?)?;
        // First request: dequeued and blocked inside the provider.
        let mut first_handle = handle.clone();
        let op = describe("a")?;
        let first = tokio::spawn(async move { exec_typed(&mut first_handle, 1, op).await });
        wait_started(&mut started).await?;
        // Second request: fills the single queue slot.
        let mut second_handle = handle.clone();
        let op = describe("b")?;
        let second = tokio::spawn(async move { exec_typed(&mut second_handle, 2, op).await });
        tokio::task::yield_now().await;
        let mut waited = 0;
        while handle.queued() < 1 && waited < 1000 {
            tokio::time::sleep(Duration::from_millis(1)).await;
            waited += 1;
        }
        if handle.queued() != 1 {
            return Err(TestError::Unexpected(format!(
                "queued = {}",
                handle.queued()
            )));
        }
        // poll_ready sheds load instead of waiting.
        let ready = poll_fn(|cx| handle.poll_ready(cx)).await;
        match ready.map_err(service_error) {
            Err(CryptoServiceError::Overloaded) => {}
            other => return Err(TestError::Unexpected(format!("poll_ready: {other:?}"))),
        }
        // call without poll_ready also sheds.
        let third = handle
            .call(CryptoRequest::new(RequestId(3), describe("c")?))
            .await;
        match third.map_err(service_error) {
            Err(CryptoServiceError::Overloaded) => {}
            other => return Err(TestError::Unexpected(format!("call: {other:?}"))),
        }
        open.send(()).map_err(ctx("open gate 1"))?;
        open.send(()).map_err(ctx("open gate 2"))?;
        first
            .await
            .map_err(ctx("join first"))?
            .map_err(ctx("first"))?;
        second
            .await
            .map_err(ctx("join second"))?
            .map_err(ctx("second"))?;
        // The shed request was never executed.
        let count = executions.load(Ordering::SeqCst);
        if count != 2 {
            return Err(TestError::Unexpected(format!("executions = {count}")));
        }
        Ok(())
    }

    #[tokio::test]
    async fn timeout_is_unavailable_and_abandoned_jobs_are_skipped() -> TestResult {
        let Gate {
            mut handle,
            mut started,
            open,
            executions,
        } = gate(config(4, Some(Duration::from_millis(200)))?)?;
        let mut first_handle = handle.clone();
        let op = describe("a")?;
        let first = tokio::spawn(async move { exec_typed(&mut first_handle, 1, op).await });
        wait_started(&mut started).await?;
        // Queued behind the blocked first request; its caller gives up.
        let second = exec_typed(&mut handle, 2, describe("b")?).await;
        match second {
            Err(CryptoServiceError::Unavailable) => {}
            other => return Err(TestError::Unexpected(format!("second: {other:?}"))),
        }
        match first.await.map_err(ctx("join first"))? {
            Err(CryptoServiceError::Unavailable) => {}
            other => return Err(TestError::Unexpected(format!("first: {other:?}"))),
        }
        // Release the running request and allow one more execution.
        open.send(()).map_err(ctx("open gate 1"))?;
        open.send(()).map_err(ctx("open gate 2"))?;
        let mut third_handle = handle.clone();
        let op = describe("c")?;
        let third = tokio::spawn(async move { exec_typed(&mut third_handle, 3, op).await });
        third
            .await
            .map_err(ctx("join third"))?
            .map_err(ctx("third"))?;
        // First (already running) and third executed; the abandoned second
        // was skipped without reaching the provider.
        let count = executions.load(Ordering::SeqCst);
        if count != 2 {
            return Err(TestError::Unexpected(format!("executions = {count}")));
        }
        Ok(())
    }

    #[tokio::test]
    async fn errors_downcast_to_crypto_service_error() -> TestResult {
        let mut handle = CryptoWorkerHandle::spawn(NullProvider, CryptoWorkerConfig::default())
            .map_err(ctx("spawn worker"))?;
        let err = match exec(&mut handle, 1, describe("k")?).await {
            Err(err) => err,
            Ok(other) => return Err(TestError::Unexpected(format!("{other:?}"))),
        };
        // Exactly the downcast `crypt_guard_service::service_error` performs.
        match err.downcast::<CryptoServiceError>() {
            Ok(inner) if *inner == CryptoServiceError::Unsupported => Ok(()),
            Ok(inner) => Err(TestError::Unexpected(format!("{inner:?}"))),
            Err(other) => Err(TestError::Unexpected(format!(
                "not a CryptoServiceError: {other}"
            ))),
        }
    }

    struct DropProbe(tokio_mpsc::UnboundedSender<()>);

    impl CryptoProvider for DropProbe {
        fn execute(
            &mut self,
            _request: CryptoRequest,
        ) -> Result<CryptoResponse, CryptoServiceError> {
            Err(CryptoServiceError::Unsupported)
        }
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    #[tokio::test]
    async fn dropping_all_handles_stops_workers_and_drops_providers() -> TestResult {
        let (dropped_tx, mut dropped) = tokio_mpsc::unbounded_channel();
        let handle = CryptoWorkerHandle::spawn_pool(
            NonZeroUsize::new(3).ok_or(TestError::Missing("thread count"))?,
            || DropProbe(dropped_tx.clone()),
            CryptoWorkerConfig::default(),
        )
        .map_err(ctx("spawn pool"))?;
        drop(dropped_tx);
        let clone = handle.clone();
        drop(handle);
        drop(clone);
        // All three providers are dropped, then the channel closes.
        for _ in 0..3 {
            tokio::time::timeout(WAIT, dropped.recv())
                .await
                .map_err(ctx("wait for provider drop"))?
                .ok_or(TestError::Missing("drop signal"))?;
        }
        Ok(())
    }
}
