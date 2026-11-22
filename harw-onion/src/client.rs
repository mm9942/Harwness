//! Onion dialer (feature `client`) and the arti helpers shared with the
//! onion-service side.
//!
//! Tor is only a byte transport here: the streams returned by
//! [`OnionClient::dial`] carry no peer identity. Authentication happens in
//! the node transport layered on top.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use arti_client::config::{BoolOrAuto, TorClientConfigBuilder};
use arti_client::status::BootstrapStatus;
use arti_client::{ErrorKind, HasKind, StreamPrefs, TorClient, TorClientConfig};
use futures::StreamExt;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tor_rtcompat::tokio::TokioRustlsRuntime;

use crate::{BootstrapProgress, OnionError, OnionLocator, OnionStream};

/// The arti client type used by harw (tokio runtime with rustls TLS).
pub(crate) type ArtiClient = TorClient<TokioRustlsRuntime>;

/// Set once the "this build is outdated" warning has been logged.
static OUTDATED_WARNED: AtomicBool = AtomicBool::new(false);

/// Storage locations of an embedded Tor client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnionClientConfig {
    /// Persistent arti state directory (guards, keystore). Must be private.
    pub state_dir: PathBuf,
    /// arti directory cache (consensus, microdescriptors). Must be private.
    pub cache_dir: PathBuf,
}

impl OnionClientConfig {
    /// Creates a configuration from a state and a cache directory.
    pub fn new(state_dir: impl Into<PathBuf>, cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            cache_dir: cache_dir.into(),
        }
    }
}

/// An embedded Tor client that dials `.onion` services.
///
/// Dropping it aborts its background bootstrap and progress tasks.
pub struct OnionClient {
    client: Arc<ArtiClient>,
    progress: watch::Receiver<BootstrapProgress>,
    tasks: Vec<JoinHandle<()>>,
}

const _: fn() = || {
    fn assert<T: Send + Sync>() {}
    assert::<OnionClient>();
};

impl std::fmt::Debug for OnionClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnionClient")
            .field("progress", &*self.progress.borrow())
            .finish_non_exhaustive()
    }
}

impl OnionClient {
    /// Starts an embedded Tor client without waiting for the network.
    ///
    /// Installs the rustls crypto provider, creates the arti client and
    /// spawns its bootstrap plus a progress forwarder on the current tokio
    /// runtime (which must be a multi-thread runtime).
    pub fn launch(config: &OnionClientConfig) -> Result<Self, OnionError> {
        let client = create_client(&config.state_dir, &config.cache_dir)?;
        let (progress, progress_task) = spawn_progress(&client);
        let bootstrap_client = Arc::clone(&client);
        let bootstrap_task = tokio::spawn(async move {
            if let Err(error) = bootstrap_client.bootstrap().await {
                let error = map_arti_error(&error, OnionError::Bootstrap);
                tracing::debug!(%error, "initial tor bootstrap failed; dial will retry");
            }
        });
        tracing::info!("tor client started");
        Ok(Self {
            client,
            progress,
            tasks: vec![progress_task, bootstrap_task],
        })
    }

    /// Returns the current bootstrap progress snapshot without blocking.
    pub fn progress(&self) -> BootstrapProgress {
        self.progress.borrow().clone()
    }

    /// Returns a receiver that observes every bootstrap progress change.
    pub fn subscribe(&self) -> watch::Receiver<BootstrapProgress> {
        self.progress.clone()
    }

    /// Opens a Tor stream to `locator`, bootstrapping first if needed.
    ///
    /// The whole operation (bootstrap plus rendezvous) is bounded by
    /// `timeout`; exceeding it yields [`OnionError::ConnectTimeout`].
    pub async fn dial(
        &self,
        locator: &OnionLocator,
        timeout: Duration,
    ) -> Result<OnionStream, OnionError> {
        let attempt = async {
            self.client
                .bootstrap()
                .await
                .map_err(|e| map_arti_error(&e, OnionError::Bootstrap))?;
            let mut prefs = StreamPrefs::new();
            prefs.connect_to_onion_services(BoolOrAuto::Explicit(true));
            let stream = self
                .client
                .connect_with_prefs((locator.host(), locator.port()), &prefs)
                .await
                .map_err(|e| map_arti_error(&e, OnionError::Connect))?;
            Ok::<OnionStream, OnionError>(Box::pin(stream))
        };
        let result = match tokio::time::timeout(timeout, attempt).await {
            Ok(result) => result,
            Err(_) => Err(OnionError::ConnectTimeout(timeout)),
        };
        if let Err(error) = &result {
            tracing::debug!(target = %locator.redacted(), %error, "onion dial failed");
        }
        result
    }
}

impl Drop for OnionClient {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Installs aws-lc-rs as the process-default rustls provider unless another
/// provider is already installed, then requires that a default exists.
///
/// arti's rustls runtime panics when no process-default provider exists, so
/// this must run before any [`TokioRustlsRuntime`] or Tor client is created.
pub(crate) fn install_crypto_provider() -> Result<(), OnionError> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    if rustls::crypto::CryptoProvider::get_default().is_some() {
        Ok(())
    } else {
        Err(OnionError::CryptoProvider)
    }
}

/// Builds the arti configuration for the given state and cache directories.
///
/// arti's own permission checks on these directories stay enabled.
pub(crate) fn client_config(
    state_dir: &Path,
    cache_dir: &Path,
) -> Result<TorClientConfig, OnionError> {
    TorClientConfigBuilder::from_directories(state_dir, cache_dir)
        .build()
        .map_err(|e| OnionError::Config(e.to_string()))
}

/// Installs the crypto provider and creates an unbootstrapped arti client on
/// the current tokio runtime (bootstrap behaviour: on demand).
pub(crate) fn create_client(
    state_dir: &Path,
    cache_dir: &Path,
) -> Result<Arc<ArtiClient>, OnionError> {
    install_crypto_provider()?;
    let runtime = TokioRustlsRuntime::current().map_err(|e| OnionError::Runtime(e.to_string()))?;
    let config = client_config(state_dir, cache_dir)?;
    TorClient::with_runtime(runtime)
        .config(config)
        .create_unbootstrapped()
        .map_err(|e| map_arti_error(&e, OnionError::Config))
}

/// Maps an arti bootstrap status to harw's progress snapshot.
pub(crate) fn progress_of(status: &BootstrapStatus) -> BootstrapProgress {
    let fraction = status.as_frac().clamp(0.0, 1.0);
    // `as` saturates (NaN becomes 0); the value is already within 0..=100.
    let percent = ((fraction * 100.0).round() as u8).min(100);
    BootstrapProgress {
        percent,
        ready: status.ready_for_traffic(),
        blocked: status.blocked().map(|b| b.to_string()),
        summary: status.to_string(),
    }
}

/// Publishes the client's bootstrap progress into a watch channel.
///
/// The spawned task holds only the event stream, never the client itself,
/// so it does not keep the client alive.
pub(crate) fn spawn_progress(
    client: &ArtiClient,
) -> (watch::Receiver<BootstrapProgress>, JoinHandle<()>) {
    let (sender, receiver) = watch::channel(progress_of(&client.bootstrap_status()));
    let mut events = client.bootstrap_events();
    let task = tokio::spawn(async move {
        while let Some(status) = events.next().await {
            let progress = progress_of(&status);
            sender.send_if_modified(|current| {
                if *current == progress {
                    false
                } else {
                    *current = progress;
                    true
                }
            });
        }
    });
    (receiver, task)
}

/// Maps an arti error: an outdated-software verdict becomes
/// [`OnionError::Outdated`], anything else is wrapped with `wrap`.
pub(crate) fn map_arti_error(
    error: &arti_client::Error,
    wrap: fn(String) -> OnionError,
) -> OnionError {
    if matches!(error.kind(), ErrorKind::SoftwareDeprecated) {
        if !OUTDATED_WARNED.swap(true, Ordering::Relaxed) {
            tracing::warn!("the Tor network considers this build's arti outdated; update harw");
        }
        OnionError::Outdated
    } else {
        wrap(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn client_config_builds_for_private_dirs() -> TestResult {
        let dir = tempfile::tempdir()?;
        let state = dir.path().join("state");
        let cache = dir.path().join("cache");
        client_config(&state, &cache)?;
        Ok(())
    }

    #[test]
    fn vanguards_default_to_lite() -> TestResult {
        let dir = tempfile::tempdir()?;
        let cfg = client_config(&dir.path().join("state"), &dir.path().join("cache"))?;
        let v: &arti_client::config::vanguards::VanguardConfig = cfg.as_ref();
        assert_eq!(v.mode().to_string(), "lite");
        Ok(())
    }

    #[test]
    fn crypto_provider_install_is_idempotent() -> TestResult {
        install_crypto_provider()?;
        install_crypto_provider()?;
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            return Err("no default crypto provider after install".into());
        }
        Ok(())
    }

    #[test]
    fn progress_of_default_status_is_zero_and_not_ready() {
        let progress = progress_of(&BootstrapStatus::default());
        assert_eq!(progress.percent, 0);
        assert!(!progress.ready);
        assert!(!progress.summary.is_empty());
    }
}
