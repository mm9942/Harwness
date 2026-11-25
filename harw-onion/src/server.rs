//! Onion-service half (cargo feature `service`).
//!
//! [`OnionServer`] launches an arti onion service and runs its own accept
//! loop over rendezvous requests. Every accepted Tor stream is handed to the
//! caller's handler unchanged: Tor gives no client identity, authentication
//! happens above this crate.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::{Stream, StreamExt};
use safelog::DisplayRedacted;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::{JoinHandle, JoinSet};
use tor_cell::relaycell::msg::Connected;
use tor_hsservice::config::OnionServiceConfigBuilder;
use tor_hsservice::{
    HsNickname, OnionServiceConfig, RendRequest, RunningOnionService, StreamRequest,
};
use tor_proto::stream::IncomingStreamRequest;
use tracing::{debug, info};

use crate::client::{ArtiClient, create_client, map_arti_error, spawn_progress};
use crate::{BootstrapProgress, DEFAULT_VIRTUAL_PORT, OnionError, OnionLocator, OnionStream};

/// Default arti nickname of the harw session onion service. It names the
/// service's key and state directories inside the arti state directory.
pub const DEFAULT_NICKNAME: &str = "harw-session";

const DEFAULT_BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(300);

/// Admission limits of the onion-service accept loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcceptLimits {
    /// Maximum number of concurrent rendezvous circuits; further rendezvous
    /// requests are rejected before any circuit is built.
    pub max_circuits: usize,
    /// Maximum number of concurrent streams on one circuit; one more closes
    /// the circuit (also configured in arti).
    pub max_streams_per_circuit: u32,
    /// Maximum number of concurrent streams over all circuits; when exhausted
    /// the requesting circuit is closed.
    pub max_streams: usize,
    /// A circuit without live streams for this long is dropped.
    pub circuit_idle: Duration,
    /// Upper bound for building one rendezvous circuit.
    pub rendezvous_timeout: Duration,
}

impl Default for AcceptLimits {
    fn default() -> Self {
        Self {
            max_circuits: 64,
            max_streams_per_circuit: 8,
            max_streams: 64,
            circuit_idle: Duration::from_secs(30),
            rendezvous_timeout: Duration::from_secs(60),
        }
    }
}

/// Configuration of an [`OnionServer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnionServerConfig {
    /// arti state directory (includes the keystore, hence the persistent
    /// onion identity).
    pub state_dir: PathBuf,
    /// arti cache directory (directory documents).
    pub cache_dir: PathBuf,
    /// arti onion-service nickname.
    pub nickname: String,
    /// Virtual port clients connect to; only BEGIN requests to it are served.
    pub virtual_port: u16,
    /// Upper bound for the Tor bootstrap during [`OnionServer::launch`].
    pub bootstrap_timeout: Duration,
    /// Accept-loop admission limits.
    pub limits: AcceptLimits,
}

impl OnionServerConfig {
    /// Creates a configuration with [`DEFAULT_NICKNAME`],
    /// [`DEFAULT_VIRTUAL_PORT`], a 300 s bootstrap timeout and default limits.
    pub fn new(state_dir: impl Into<PathBuf>, cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            cache_dir: cache_dir.into(),
            nickname: DEFAULT_NICKNAME.to_owned(),
            virtual_port: DEFAULT_VIRTUAL_PORT,
            bootstrap_timeout: DEFAULT_BOOTSTRAP_TIMEOUT,
            limits: AcceptLimits::default(),
        }
    }
}

/// A running onion service.
///
/// Dropping it (or calling [`OnionServer::shutdown`]) aborts the accept loop
/// with all its circuits and handlers, stops progress reporting and drops the
/// onion service and the Tor client.
pub struct OnionServer {
    locator: OnionLocator,
    // Field order is drop order: tasks first, then the service, then the client.
    tasks: TaskGuard,
    service: Arc<RunningOnionService>,
    _client: Arc<ArtiClient>,
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<OnionServer>();
};

impl OnionServer {
    /// Bootstraps Tor, launches the onion service and starts accepting.
    ///
    /// `on_progress` receives every bootstrap progress change (also after
    /// launch). `handler` is called once per accepted stream; its future runs
    /// in its own task while the stream budget is held.
    ///
    /// # Errors
    /// [`OnionError::Config`] for an invalid nickname, port 0 or service
    /// configuration; [`OnionError::CryptoProvider`] / [`OnionError::Runtime`]
    /// when the Tor client cannot be created; [`OnionError::BootstrapTimeout`]
    /// or [`OnionError::Bootstrap`] when bootstrapping fails;
    /// [`OnionError::Outdated`] when the Tor network rejects this arti
    /// version; [`OnionError::Service`] when the service cannot be launched.
    pub async fn launch<P, H, F>(
        config: OnionServerConfig,
        on_progress: P,
        handler: H,
    ) -> Result<Self, OnionError>
    where
        P: FnMut(&BootstrapProgress) + Send + 'static,
        H: Fn(OnionStream) -> F + Send + Sync + 'static,
        F: Future<Output = ()> + Send + 'static,
    {
        if config.virtual_port == 0 {
            return Err(OnionError::Config(
                "virtual port must be 1..=65535".to_owned(),
            ));
        }
        let service_config = service_config(&config)?;
        let client = create_client(&config.state_dir, &config.cache_dir)?;

        let mut tasks = TaskGuard::default();
        let (progress, progress_task) = spawn_progress(&client);
        tasks.push(progress_task);
        tasks.push(tokio::spawn(forward_progress(progress, on_progress)));

        match tokio::time::timeout(config.bootstrap_timeout, client.bootstrap()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(map_arti_error(&e, OnionError::Bootstrap)),
            Err(_) => return Err(OnionError::BootstrapTimeout(config.bootstrap_timeout)),
        }

        let (service, rend_requests) = client
            .launch_onion_service(service_config)
            .map_err(|e| map_arti_error(&e, OnionError::Service))?
            .ok_or_else(|| OnionError::Service("onion service is disabled".to_owned()))?;
        let hsid = service
            .onion_address()
            .ok_or_else(|| OnionError::Service("onion service has no address".to_owned()))?;
        let locator =
            OnionLocator::new(&hsid.display_unredacted().to_string(), config.virtual_port)?;

        let ctx = Arc::new(AcceptCtx {
            handler,
            limits: config.limits,
            virtual_port: config.virtual_port,
            stream_slots: Arc::new(Semaphore::new(
                config.limits.max_streams.min(Semaphore::MAX_PERMITS),
            )),
        });
        tasks.push(tokio::spawn(accept_loop(rend_requests, ctx)));
        // Service launch / general Tor bootstrap are not evidence that clients
        // can fetch the hidden-service descriptor. Observe reachability for
        // the whole service lifetime, including degradation after startup.
        let status_service = Arc::clone(&service);
        tasks.push(tokio::spawn(async move {
            let mut events = std::pin::pin!(status_service.status_events());
            let mut last = None;
            loop {
                let reachable = status_service.status().state().is_fully_reachable();
                if last != Some(reachable) {
                    if reachable {
                        info!("onion service reports fully reachable");
                    } else {
                        tracing::warn!("onion service reachability not confirmed");
                    }
                    last = Some(reachable);
                }
                if events.next().await.is_none() {
                    tracing::warn!("onion service status stream ended");
                    break;
                }
            }
        }));

        info!(onion = %locator.redacted(), "onion service launched");
        info!(
            "arti may exit this process if the Tor network starts requiring protocols \
             this build lacks; see the harw-onion crate docs"
        );
        Ok(Self {
            locator,
            tasks,
            service,
            _client: client,
        })
    }

    /// The service's locator (persistent while the state directory is kept).
    #[must_use]
    pub fn locator(&self) -> &OnionLocator {
        &self.locator
    }

    /// Waits until arti believes the service is fully reachable (descriptor
    /// published). Returns `false` on timeout.
    pub async fn wait_reachable(&self, timeout: Duration) -> bool {
        if self.service.status().state().is_fully_reachable() {
            return true;
        }
        let mut events = std::pin::pin!(self.service.status_events());
        let wait = async {
            while let Some(status) = events.next().await {
                if status.state().is_fully_reachable() {
                    return true;
                }
            }
            false
        };
        tokio::time::timeout(timeout, wait).await.unwrap_or(false)
    }

    /// Stops the service (same as dropping it).
    pub fn shutdown(self) {
        drop(self);
    }
}

impl Drop for OnionServer {
    fn drop(&mut self) {
        // Aborting the accept loop drops its `JoinSet`s, which aborts every
        // circuit task and every handler task; the service and the client are
        // dropped right after this, in field order.
        self.tasks.abort_all();
    }
}

/// Builds the arti onion-service configuration: nickname and per-circuit
/// stream limit only (no proof-of-work, no restricted discovery).
pub(crate) fn service_config(config: &OnionServerConfig) -> Result<OnionServiceConfig, OnionError> {
    let nickname = HsNickname::new(config.nickname.clone())
        .map_err(|e| OnionError::Config(format!("onion service nickname: {e}")))?;
    OnionServiceConfigBuilder::default()
        .nickname(nickname)
        .max_concurrent_streams_per_circuit(config.limits.max_streams_per_circuit)
        .build()
        .map_err(|e| OnionError::Config(e.to_string()))
}

/// Aborts the owned tasks when dropped.
#[derive(Default)]
struct TaskGuard(Vec<JoinHandle<()>>);

impl TaskGuard {
    fn push(&mut self, task: JoinHandle<()>) {
        self.0.push(task);
    }

    fn abort_all(&self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        self.abort_all();
    }
}

async fn forward_progress<P>(
    mut progress: tokio::sync::watch::Receiver<BootstrapProgress>,
    mut on_progress: P,
) where
    P: FnMut(&BootstrapProgress),
{
    let first = progress.borrow_and_update().clone();
    on_progress(&first);
    while progress.changed().await.is_ok() {
        let current = progress.borrow_and_update().clone();
        on_progress(&current);
    }
}

/// State shared by the accept loop and all circuit tasks.
struct AcceptCtx<H> {
    handler: H,
    limits: AcceptLimits,
    virtual_port: u16,
    stream_slots: Arc<Semaphore>,
}

/// Why a stream request closes its circuit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    /// BEGIN_DIR, RESOLVE or another non-BEGIN request.
    NotBegin,
    /// BEGIN to a port other than the virtual port.
    WrongPort,
    /// The circuit already carries `max_streams_per_circuit` live streams.
    CircuitFull,
}

impl Refusal {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotBegin => "not a BEGIN request",
            Self::WrongPort => "wrong virtual port",
            Self::CircuitFull => "per-circuit stream limit reached",
        }
    }
}

/// Pure per-circuit admission rule. `begin_port` is `Some(port)` for a BEGIN
/// request and `None` for anything else.
fn stream_verdict(
    begin_port: Option<u16>,
    virtual_port: u16,
    live_streams: usize,
    max_streams_per_circuit: u32,
) -> Result<(), Refusal> {
    let port = begin_port.ok_or(Refusal::NotBegin)?;
    if port != virtual_port {
        return Err(Refusal::WrongPort);
    }
    let max = usize::try_from(max_streams_per_circuit).unwrap_or(usize::MAX);
    if live_streams >= max {
        return Err(Refusal::CircuitFull);
    }
    Ok(())
}

async fn accept_loop<S, H, F>(rend_requests: S, ctx: Arc<AcceptCtx<H>>)
where
    S: Stream<Item = RendRequest> + Send + 'static,
    H: Fn(OnionStream) -> F + Send + Sync + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let mut rend_requests = Box::pin(rend_requests);
    let circuit_slots = Arc::new(Semaphore::new(
        ctx.limits.max_circuits.min(Semaphore::MAX_PERMITS),
    ));
    let mut circuits = JoinSet::new();
    loop {
        tokio::select! {
            next = rend_requests.next() => {
                let Some(rend) = next else { break };
                match Arc::clone(&circuit_slots).try_acquire_owned() {
                    Ok(permit) => {
                        circuits.spawn(serve_circuit(rend, permit, Arc::clone(&ctx)));
                    }
                    Err(_) => {
                        debug!("onion circuit limit reached; rejecting rendezvous request");
                        let _ = rend.reject().await;
                    }
                }
            }
            Some(_) = circuits.join_next(), if !circuits.is_empty() => {}
        }
    }
    debug!("onion rendezvous request stream ended");
    while circuits.join_next().await.is_some() {}
}

async fn serve_circuit<H, F>(
    rend: RendRequest,
    _circuit_permit: OwnedSemaphorePermit,
    ctx: Arc<AcceptCtx<H>>,
) where
    H: Fn(OnionStream) -> F + Send + Sync + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let rendezvous = tokio::time::timeout(ctx.limits.rendezvous_timeout, rend.accept()).await;
    let mut requests = match rendezvous {
        Ok(Ok(requests)) => requests,
        Ok(Err(e)) => {
            debug!(error = %e, "onion rendezvous failed");
            return;
        }
        Err(_) => {
            debug!("onion rendezvous timed out");
            return;
        }
    };
    debug!("onion circuit established");
    // `requests` keeps the circuit open; returning drops it (and aborts the
    // circuit's handlers via `handlers`).
    let mut handlers = JoinSet::new();
    loop {
        let idle = handlers.is_empty();
        tokio::select! {
            next = requests.next() => {
                let Some(request) = next else { break };
                while handlers.try_join_next().is_some() {}
                if !admit_stream(&ctx, &mut handlers, request).await {
                    return;
                }
            }
            Some(_) = handlers.join_next(), if !handlers.is_empty() => {}
            () = tokio::time::sleep(ctx.limits.circuit_idle), if idle => {
                debug!("onion circuit idle; closing");
                return;
            }
        }
    }
    debug!("onion circuit closed by peer");
    while handlers.join_next().await.is_some() {}
}

/// Accepts one stream request or shuts the circuit down. Returns `false`
/// when the circuit was shut down.
async fn admit_stream<H, F>(
    ctx: &AcceptCtx<H>,
    handlers: &mut JoinSet<()>,
    request: StreamRequest,
) -> bool
where
    H: Fn(OnionStream) -> F + Send + Sync + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let begin_port = match request.request() {
        IncomingStreamRequest::Begin(begin) => Some(begin.port()),
        _ => None,
    };
    let admitted = stream_verdict(
        begin_port,
        ctx.virtual_port,
        handlers.len(),
        ctx.limits.max_streams_per_circuit,
    )
    .map_err(Refusal::as_str)
    .and_then(|()| {
        Arc::clone(&ctx.stream_slots)
            .try_acquire_owned()
            .map_err(|_| "global stream limit reached")
    });
    let stream_permit = match admitted {
        Ok(permit) => permit,
        Err(reason) => {
            debug!(reason, "closing onion circuit");
            let _ = request.shutdown_circuit();
            return false;
        }
    };
    match request.accept(Connected::new_empty()).await {
        Ok(data_stream) => {
            let stream: OnionStream = Box::pin(data_stream);
            let work = (ctx.handler)(stream);
            handlers.spawn(async move {
                work.await;
                drop(stream_permit);
            });
            debug!("onion stream accepted");
        }
        Err(e) => debug!(error = %e, "onion stream accept failed"),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn config_defaults() {
        let config = OnionServerConfig::new("/state", "/cache");
        assert_eq!(config.state_dir, PathBuf::from("/state"));
        assert_eq!(config.cache_dir, PathBuf::from("/cache"));
        assert_eq!(config.nickname, "harw-session");
        assert_eq!(config.virtual_port, 443);
        assert_eq!(config.bootstrap_timeout, Duration::from_secs(300));
        assert_eq!(
            config.limits,
            AcceptLimits {
                max_circuits: 64,
                max_streams_per_circuit: 8,
                max_streams: 64,
                circuit_idle: Duration::from_secs(30),
                rendezvous_timeout: Duration::from_secs(60),
            }
        );
    }

    #[test]
    fn service_config_builds() -> TestResult {
        let built = service_config(&OnionServerConfig::new("/state", "/cache"))?;
        assert_eq!(built.nickname().to_string(), "harw-session");
        Ok(())
    }

    #[test]
    fn invalid_nickname_is_a_config_error() {
        let mut config = OnionServerConfig::new("/state", "/cache");
        config.nickname = "bad nick!".to_owned();
        assert!(matches!(
            service_config(&config),
            Err(OnionError::Config(_))
        ));
    }

    #[test]
    fn stream_verdict_rules() {
        assert_eq!(stream_verdict(Some(443), 443, 0, 8), Ok(()));
        assert_eq!(stream_verdict(Some(443), 443, 7, 8), Ok(()));
        assert_eq!(
            stream_verdict(Some(443), 443, 8, 8),
            Err(Refusal::CircuitFull)
        );
        assert_eq!(stream_verdict(Some(80), 443, 0, 8), Err(Refusal::WrongPort));
        assert_eq!(stream_verdict(None, 443, 0, 8), Err(Refusal::NotBegin));
        assert_eq!(
            stream_verdict(Some(443), 443, 0, 0),
            Err(Refusal::CircuitFull)
        );
    }
}
