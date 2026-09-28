//! `harw-netsec` binary: see the library documentation.

use std::process::ExitCode;

use harw_netsec::cli::{CliCommand, USAGE, parse_args};
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    let command = match parse_args(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("harw-netsec: {error}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let args = match command {
        CliCommand::Help => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        CliCommand::Version => {
            println!("harw-netsec {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        CliCommand::Run(args) => args,
    };

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("harw-netsec: cannot start runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(harw_netsec::daemon::run(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "harw-netsec failed");
            eprintln!("harw-netsec: {error}");
            ExitCode::FAILURE
        }
    }
}
