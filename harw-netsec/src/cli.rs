//! Command line of the `harw-netsec` binary.
//!
//! ```text
//! harw-netsec [--config PATH] [--systemd-socket]
//! harw-netsec --help | --version
//! ```

use std::ffi::OsString;
use std::path::PathBuf;

use crate::config::DEFAULT_CONFIG_PATH;
use crate::error::{NetsecError, NetsecResult};

/// Usage text.
pub const USAGE: &str = "\
usage: harw-netsec [--config PATH] [--systemd-socket]
       harw-netsec --help | --version

  --config PATH       configuration file (default: /etc/harw-netsec/config.toml)
  --systemd-socket    take the listening socket from systemd (LISTEN_FDS=1)
                      instead of binding the configured path
";

/// Arguments of a daemon run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunArgs {
    /// Configuration file.
    pub config: PathBuf,
    /// Use systemd socket activation.
    pub systemd_socket: bool,
}

/// What the binary was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    /// Run the daemon.
    Run(RunArgs),
    /// Print usage.
    Help,
    /// Print the version.
    Version,
}

/// Parses the arguments after the program name.
///
/// # Errors
/// [`NetsecError::Config`] for unknown flags, missing values or repeats.
pub fn parse_args<I>(args: I) -> NetsecResult<CliCommand>
where
    I: IntoIterator<Item = OsString>,
{
    let usage_error = |reason: String| NetsecError::Config { reason };
    let mut config: Option<PathBuf> = None;
    let mut systemd_socket = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let Some(flag) = arg.to_str() else {
            return Err(usage_error("arguments must be valid UTF-8".to_owned()));
        };
        match flag {
            "--help" | "-h" => return Ok(CliCommand::Help),
            "--version" | "-V" => return Ok(CliCommand::Version),
            "--systemd-socket" => {
                if systemd_socket {
                    return Err(usage_error("--systemd-socket given twice".to_owned()));
                }
                systemd_socket = true;
            }
            "--config" => {
                if config.is_some() {
                    return Err(usage_error("--config given twice".to_owned()));
                }
                let Some(value) = args.next() else {
                    return Err(usage_error("--config needs a path".to_owned()));
                };
                config = Some(PathBuf::from(value));
            }
            other => return Err(usage_error(format!("unknown argument {other:?}"))),
        }
    }
    Ok(CliCommand::Run(RunArgs {
        config: config.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH)),
        systemd_socket,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn parse(args: &[&str]) -> NetsecResult<CliCommand> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn test_defaults() -> TestResult {
        match parse(&[])? {
            CliCommand::Run(run) => {
                assert_eq!(run.config, PathBuf::from(DEFAULT_CONFIG_PATH));
                assert!(!run.systemd_socket);
                Ok(())
            }
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn test_flags() -> TestResult {
        let command = parse(&["--config", "/etc/x.toml", "--systemd-socket"])?;
        assert_eq!(
            command,
            CliCommand::Run(RunArgs {
                config: PathBuf::from("/etc/x.toml"),
                systemd_socket: true,
            })
        );
        assert_eq!(parse(&["--help"])?, CliCommand::Help);
        assert_eq!(parse(&["-V"])?, CliCommand::Version);
        Ok(())
    }

    #[test]
    fn test_errors() {
        assert!(parse(&["--config"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
        assert!(parse(&["--config", "a", "--config", "b"]).is_err());
        assert!(parse(&["--systemd-socket", "--systemd-socket"]).is_err());
    }
}
