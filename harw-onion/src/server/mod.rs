//! Onion-service half (cargo feature `service`).
//!
//! [`OnionServer`] launches an arti onion service and runs its own accept
//! loop over rendezvous requests. Every accepted Tor stream is handed to the
//! caller's handler unchanged: Tor gives no client identity, authentication
//! happens above this crate.

mod accept;
mod health;
mod identity;
mod supervisor;

pub use health::OnionServiceStatus;
pub use supervisor::OnionSupervisor;

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use safelog::DisplayRedacted;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;
use tor_hsservice::config::OnionServiceConfigBuilder;
use tor_hsservice::{HsNickname, OnionServiceConfig, RunningOnionService};
use tracing::info;

use crate::client::runtime::{ArtiClient, create_client, map_arti_error, spawn_progress};
use crate::{BootstrapProgress, DEFAULT_VIRTUAL_PORT, OnionError, OnionLocator, OnionStream};
use accept::{AcceptCtx, accept_loop};

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
    /// Previously published locator to verify before bringing the service online.
    pub expected_locator: Option<OnionLocator>,
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
            expected_locator: None,
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
    status: watch::Receiver<OnionServiceStatus>,
    stream_slots: Arc<Semaphore>,
    stream_capacity: usize,
    _service: Arc<RunningOnionService>,
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
        identity::verify_before_launch(&config, &service_config)?;
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
        if let Some(expected) = &config.expected_locator {
            identity::require_match(expected, &locator)?;
        }

        let stream_capacity = config.limits.max_streams.min(Semaphore::MAX_PERMITS);
        let stream_slots = Arc::new(Semaphore::new(stream_capacity));
        let ctx = Arc::new(AcceptCtx {
            handler,
            limits: config.limits,
            virtual_port: config.virtual_port,
            stream_slots: Arc::clone(&stream_slots),
        });
        let (health, status) = watch::channel(OnionServiceStatus::Publishing);
        let accept_health = health.clone();
        tasks.push(tokio::spawn(async move {
            accept_loop(rend_requests, ctx, accept_health).await;
        }));
        tasks.push(tokio::spawn(health::observe(Arc::clone(&service), health)));

        info!(onion = %locator.redacted(), "onion service launched");
        info!(
            "arti may exit this process if the Tor network starts requiring protocols \
             this build lacks; see the harw-onion crate docs"
        );
        Ok(Self {
            locator,
            tasks,
            status,
            stream_slots,
            stream_capacity,
            _service: service,
            _client: client,
        })
    }

    /// The service's locator (persistent while the state directory is kept).
    #[must_use]
    pub fn locator(&self) -> &OnionLocator {
        &self.locator
    }

    /// Observes Onion publication and terminal task failure throughout the service lifetime.
    #[must_use]
    pub fn status(&self) -> watch::Receiver<OnionServiceStatus> {
        self.status.clone()
    }

    /// Waits for Arti to report descriptor publication. Returns false on
    /// timeout or when the accept loop or service status stream terminates.
    pub async fn wait_reachable(&self, timeout: Duration) -> bool {
        let mut status = self.status();
        let wait = async {
            loop {
                match *status.borrow_and_update() {
                    OnionServiceStatus::Published => return true,
                    OnionServiceStatus::AcceptLoopEnded | OnionServiceStatus::StatusStreamEnded => {
                        return false;
                    }
                    OnionServiceStatus::Publishing => {}
                }
                if status.changed().await.is_err() {
                    return false;
                }
            }
        };
        tokio::time::timeout(timeout, wait).await.unwrap_or(false)
    }

    fn has_active_streams(&self) -> bool {
        self.stream_slots.available_permits() < self.stream_capacity
    }

    async fn stop_and_join(mut self) {
        self.tasks.abort_all();
        for task in self.tasks.0.drain(..) {
            let _ = task.await;
        }
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

#[cfg(test)]
mod tests {
    use super::accept::{Refusal, stream_verdict};
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
