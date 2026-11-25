//! Onion dialer (feature `client`) and the arti helpers shared with the
//! onion-service side.
//!
//! Tor is only a byte transport here: the streams returned by
//! [`OnionClient::dial`] carry no peer identity. Authentication happens in
//! the node transport layered on top.

mod isolation;
mod retry;
pub(crate) mod runtime;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arti_client::HasKind;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::{BootstrapProgress, OnionError, OnionLocator, OnionStream};
use retry::{circuit_retry_delay, dial_before_deadline, retry_circuit};
use runtime::{ArtiClient, create_client, map_arti_error, spawn_progress};

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
    /// `timeout`; deadline errors distinguish bootstrap from the onion dial.
    pub async fn dial(
        &self,
        locator: &OnionLocator,
        timeout: Duration,
    ) -> Result<OnionStream, OnionError> {
        self.dial_inner(locator, timeout, false, None).await
    }

    /// Opens a Tor stream with one fresh-circuit retry for a transient error
    /// or a stalled first dial. Bootstrap and both dials share `timeout`.
    pub async fn dial_with_retry(
        &self,
        locator: &OnionLocator,
        timeout: Duration,
    ) -> Result<OnionStream, OnionError> {
        self.dial_inner(locator, timeout, true, None).await
    }

    /// Bounds cold bootstrap and circuit establishment independently.
    pub async fn dial_with_bootstrap_budget(
        &self,
        locator: &OnionLocator,
        bootstrap_timeout: Duration,
        dial_timeout: Duration,
    ) -> Result<OnionStream, OnionError> {
        self.dial_inner(locator, bootstrap_timeout, true, Some(dial_timeout))
            .await
    }

    async fn dial_inner(
        &self,
        locator: &OnionLocator,
        timeout: Duration,
        retry: bool,
        dial_timeout: Option<Duration>,
    ) -> Result<OnionStream, OnionError> {
        let deadline = Instant::now() + timeout;
        let attempt = |deadline: Instant| async move {
            let connect = |fresh| async move {
                let prefs = isolation::preferences(locator, fresh);
                if fresh {
                    tracing::debug!(onion = %locator.redacted(), "retrying with a fresh onion circuit");
                }
                self.client
                    .connect_with_prefs((locator.host(), locator.port()), &prefs)
                    .await
            };
            let stream = if retry {
                let initial_budget = deadline.saturating_duration_since(Instant::now()) / 2;
                retry_circuit(initial_budget, connect, |error| {
                    circuit_retry_delay(error.kind())
                })
                .await
            } else {
                connect(false).await
            }
            .map_err(|error| map_arti_error(&error, OnionError::Connect))?;
            Ok::<OnionStream, OnionError>(Box::pin(stream))
        };
        let bootstrap = async {
            self.client
                .bootstrap()
                .await
                .map_err(|e| map_arti_error(&e, OnionError::Bootstrap))
        };
        let result = match dial_timeout {
            Some(dial_timeout) => {
                tokio::time::timeout_at(deadline, bootstrap)
                    .await
                    .map_err(|_| OnionError::BootstrapTimeout(timeout))??;
                let dial_deadline = Instant::now() + dial_timeout;
                tokio::time::timeout_at(dial_deadline, attempt(dial_deadline))
                    .await
                    .map_err(|_| OnionError::ConnectTimeout(dial_timeout))?
            }
            None => dial_before_deadline(deadline, timeout, bootstrap, attempt(deadline)).await,
        };
        if let Err(error) = &result {
            tracing::debug!(onion = %locator.redacted(), stage = ?error.stage(), "onion dial failed");
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

#[cfg(test)]
mod tests {
    use super::runtime::{client_config, install_crypto_provider, progress_of};
    use super::*;
    use arti_client::ErrorKind;
    use arti_client::status::BootstrapStatus;

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

    #[tokio::test]
    async fn onion_network_timeout_gets_one_fresh_attempt() {
        let mut attempts = Vec::new();
        let result = retry_circuit(
            Duration::from_secs(1),
            |fresh| {
                attempts.push(fresh);
                std::future::ready(Err::<(), _>(ErrorKind::TorNetworkTimeout))
            },
            |kind| circuit_retry_delay(*kind),
        )
        .await;
        assert_eq!(attempts, [false, true]);
        assert_eq!(result, Err(ErrorKind::TorNetworkTimeout));
    }

    #[tokio::test]
    async fn bootstrap_deadline_does_not_start_an_onion_dial() {
        let total = Duration::from_millis(10);
        let result = dial_before_deadline::<()>(
            Instant::now() + total,
            total,
            std::future::pending(),
            async { panic!("expired bootstrap must not start an onion dial") },
        )
        .await;
        assert!(matches!(result, Err(OnionError::BootstrapTimeout(t)) if t == total));
    }

    #[tokio::test]
    async fn onion_dial_uses_the_same_deadline_after_bootstrap() {
        let total = Duration::from_secs(120);
        // A past deadline must not be replaced with another 120-second budget.
        let result = dial_before_deadline::<()>(
            Instant::now() - Duration::from_millis(1),
            total,
            std::future::ready(Ok(())),
            std::future::pending(),
        )
        .await;
        assert!(matches!(result, Err(OnionError::ConnectTimeout(t)) if t == total));
    }

    #[tokio::test]
    async fn other_onion_connect_errors_are_returned_without_retry() {
        let result = retry_circuit(
            Duration::from_secs(1),
            |fresh| {
                assert!(!fresh);
                std::future::ready(Err::<(), _>(ErrorKind::OnionServiceWrongClientAuth))
            },
            |kind| circuit_retry_delay(*kind),
        )
        .await;
        assert_eq!(result, Err(ErrorKind::OnionServiceWrongClientAuth));
    }

    #[tokio::test]
    async fn stalled_onion_dial_leaves_time_for_a_fresh_attempt() {
        let result = retry_circuit(
            Duration::ZERO,
            |fresh| async move {
                if fresh {
                    Ok::<_, ErrorKind>(())
                } else {
                    std::future::pending().await
                }
            },
            |kind| circuit_retry_delay(*kind),
        )
        .await;
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn incomplete_onion_directory_has_a_retry_delay() {
        assert_eq!(
            circuit_retry_delay(ErrorKind::BootstrapRequired),
            Some(Duration::from_secs(2))
        );
    }
}
