//! `harw-security-hub` daemon entry point.
//!
//! ```text
//! harw-security-hub [--config /etc/harw-security-hub/config.toml] [--check | --systemd-socket]
//! ```
//!
//! `--check` validates the config and exits without binding the socket.
//! `--systemd-socket` adopts the one socket passed by systemd socket
//! activation (`harw-security-hub.socket`) instead of binding
//! `server.socket` from the config, and fails hard if the activation
//! environment is missing or invalid (no fallback to binding a path).
//! `SIGTERM`/`SIGINT` trigger a graceful shutdown.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use harw_security_hub::config::{DEFAULT_CONFIG_PATH, HubConfig};

/// Local SecurityHub: policy, short-lived security contexts, read-only DoD
/// correlation.
#[derive(Debug, Parser)]
#[command(name = "harw-security-hub", version)]
struct Args {
    /// Config file.
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,
    /// Validate the config and exit.
    #[arg(long, conflicts_with = "systemd_socket")]
    check: bool,
    /// Use the socket passed by systemd socket activation.
    #[arg(long)]
    systemd_socket: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let config = match HubConfig::load(&args.config) {
        Ok(config) => config,
        Err(error) => {
            tracing::error!(%error, "configuration rejected");
            return ExitCode::FAILURE;
        }
    };
    if args.check {
        tracing::info!(config = %args.config.display(), "configuration valid");
        return ExitCode::SUCCESS;
    }
    let result = if args.systemd_socket {
        harw_security_hub::run_systemd(config).await
    } else {
        harw_security_hub::run(config).await
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "harw-security-hub failed");
            ExitCode::FAILURE
        }
    }
}
