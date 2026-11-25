//! Shared Arti initialization and bootstrap progress reporting.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arti_client::config::TorClientConfigBuilder;
use arti_client::status::BootstrapStatus;
use arti_client::{ErrorKind, HasKind, TorClient, TorClientConfig};
use futures::StreamExt;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tor_rtcompat::tokio::TokioRustlsRuntime;

use crate::{BootstrapProgress, OnionError};

/// The Arti client type shared by the dialer and the service.
pub(crate) type ArtiClient = TorClient<TokioRustlsRuntime>;

static OUTDATED_WARNED: AtomicBool = AtomicBool::new(false);

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
    client_config_builder(state_dir, cache_dir)
        .build()
        .map_err(|error| OnionError::Config(error.to_string()))
}

pub(super) fn client_config_builder(state_dir: &Path, cache_dir: &Path) -> TorClientConfigBuilder {
    let mut builder = TorClientConfigBuilder::from_directories(state_dir, cache_dir);
    // Learned subsecond circuit deadlines survive process restarts. Keep
    // learning and guard state, with room for slower Onion circuits on both
    // clients and the publisher's introduction and rendezvous paths.
    builder
        .override_net_params()
        .insert("cbtmintimeout".into(), 5_000);
    builder
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
        .map_err(|e| map_arti_error(&e, OnionError::Runtime))
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
