//! `harw-auth-hub` binary.
//!
//! ```text
//! harw-auth-hub [--config PATH] [--socket PATH | --systemd-socket]
//! ```
//!
//! Order: parse the command line, load and validate the config and token
//! files (fail fast, before any socket exists), open the key store (sealed
//! store creation/unlock also happens before any socket exists), start the
//! Tokio runtime, build the CryptGuard stack, obtain the listener, serve
//! until SIGTERM or SIGINT, drain, exit. Logs go to stderr via `tracing`;
//! filter with `RUST_LOG` (default `info`; audit events use the target
//! `harw_auth_hub::audit`).

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use harw_auth_hub::config::KeyStoreConfig;
use harw_auth_hub::sealed::SealedProvider;
use harw_auth_hub::service::KeyStore;
use harw_auth_hub::{DEFAULT_CONFIG_PATH, HubConfig, HubError, HubListener, HubService};

/// Local Auth/Crypto Hub: CryptGuard KMS over HTTP/1 on an AF_UNIX socket.
#[derive(Debug, Parser)]
#[command(name = "harw-auth-hub", version)]
struct Cli {
    /// Config file (TOML, strict).
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,
    /// Override the socket path from the config.
    #[arg(long, conflicts_with = "systemd_socket")]
    socket: Option<PathBuf>,
    /// Use the socket passed by systemd socket activation.
    #[arg(long)]
    systemd_socket: bool,
}

fn main() -> ExitCode {
    init_tracing();
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "harw-auth-hub failed");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), HubError> {
    let mut config = HubConfig::load(&cli.config)?;
    if let Some(socket) = cli.socket {
        if !socket.is_absolute() {
            return Err(HubError::ConfigInvalid(format!(
                "--socket '{}' must be absolute",
                socket.display()
            )));
        }
        config.socket_path = socket;
    }
    let bearer = config.load_bearer_tokens()?;

    // Open the key store before any socket exists, so a bad or missing KEK
    // fails the process fast instead of after it starts accepting peers.
    let (store, persistence, store_path) = match &config.key_store {
        KeyStoreConfig::InMemory => (KeyStore::InMemory, "in-memory", None),
        KeyStoreConfig::Sealed { path, .. } => {
            let kek = config.load_kek()?.ok_or_else(|| {
                HubError::ConfigInvalid("sealed key store: no kek configured".to_owned())
            })?;
            let provider = SealedProvider::open_or_create(path, &kek)?;
            (
                KeyStore::Sealed(provider),
                "sealed-file",
                Some(path.clone()),
            )
        }
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(HubError::Runtime)?;
    runtime.block_on(async move {
        let shutdown = harw_auth_hub::shutdown_signal()?;
        let listener = if cli.systemd_socket {
            HubListener::from_systemd()?
        } else {
            HubListener::bind(&config.socket_path)?
        };
        let hub = HubService::build(&config, bearer, store)?;
        let socket = listener
            .owned_path()
            .map_or_else(|| "<systemd>".to_owned(), |p| p.display().to_string());
        let store_display = store_path.as_deref().map(|p| p.display().to_string());
        tracing::info!(
            %socket,
            peers = config.peers.len(),
            bearer_tokens = config.bearer_tokens.len(),
            persistence,
            store_path = store_display.as_deref(),
            "harw-auth-hub listening"
        );
        if persistence == "in-memory" {
            tracing::warn!("in-memory key store: keys are lost on restart");
        }
        harw_auth_hub::serve(listener, hub, shutdown).await
    })
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
