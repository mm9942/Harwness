//! Daemon composition: config → store → listener → serve → shutdown.

use std::future::Future;
use std::sync::Arc;

use harw_types::SystemClock;
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};

use crate::cli::RunArgs;
use crate::config::NetsecConfig;
use crate::error::{NetsecError, NetsecResult};
use crate::server::{NetsecService, ServerSettings, bind_socket, remove_socket, serve};
use crate::store::NetsecStore;
use crate::systemd::listener_from_systemd;

/// Runs the daemon until SIGTERM or SIGINT.
///
/// # Description
/// Loads the configuration, opens the store (taking its single-writer
/// lock), seeds the configured zones, obtains the listener (systemd
/// activation or bind), serves, and on a signal drains connections. A socket
/// this process bound itself is removed on exit; an activated socket belongs
/// to systemd and is left alone.
///
/// # Errors
/// Any start-up failure (config, store, listener, signal registration).
pub async fn run(args: RunArgs) -> NetsecResult<()> {
    let config = NetsecConfig::load(&args.config)?;
    let store = Arc::new(NetsecStore::open(&config.state_dir, config.max_nodes)?);
    store.ensure_zones(&config.zones)?;
    let shutdown = shutdown_signal()?;

    let (listener, owned_path) = if args.systemd_socket {
        let std_listener = listener_from_systemd()?;
        let listener = UnixListener::from_std(std_listener)
            .map_err(NetsecError::io("register activated socket"))?;
        (listener, None)
    } else {
        (
            bind_socket(&config.socket_path).await?,
            Some(config.socket_path.clone()),
        )
    };

    let service = Arc::new(NetsecService::new(
        store,
        Arc::new(SystemClock),
        ServerSettings::from_config(&config),
    ));
    tracing::info!(
        socket = %config.socket_path.display(),
        activated = args.systemd_socket,
        allowed_uids = ?config.allowed_uids,
        "harw-netsec serving"
    );
    serve(listener, service, shutdown).await;
    tracing::info!("harw-netsec stopped");
    if let Some(path) = owned_path {
        remove_socket(&path)?;
    }
    Ok(())
}

/// Registers SIGTERM and SIGINT and returns a future resolving on either.
///
/// # Errors
/// [`NetsecError::Io`] if a handler cannot be installed.
pub fn shutdown_signal() -> NetsecResult<impl Future<Output = ()>> {
    let mut terminate =
        signal(SignalKind::terminate()).map_err(NetsecError::io("install SIGTERM handler"))?;
    let mut interrupt =
        signal(SignalKind::interrupt()).map_err(NetsecError::io("install SIGINT handler"))?;
    Ok(async move {
        tokio::select! {
            _ = terminate.recv() => tracing::info!("SIGTERM received; shutting down"),
            _ = interrupt.recv() => tracing::info!("SIGINT received; shutting down"),
        }
    })
}
