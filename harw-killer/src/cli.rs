//! Parse process selectors and shutdown policy with clap.
//!
//! [`Cli`] owns user input; selection and signalling are delegated to their
//! respective modules. Parsing creates no threads, acquires no locks and performs
//! no process inspection. Invalid arguments become clap diagnostics; invalid
//! durations originate as typed [`Error::InvalidInput`] values.
//!
//! # Examples
//!
//! ```no_run
//! # fn main() -> std::io::Result<()> {
//! let status = std::process::Command::new("killer")
//!     .args(["-p", "cargo", "--dry-run", "--log", "debug"])
//!     .status()?;
//! assert!(status.success());
//! # Ok(())
//! # }
//! ```

use crate::error::{Error, Result};
use clap::{ArgGroup, Parser};
use std::time::Duration;

/// Owned command-line inputs for a single selection and termination operation.
///
/// At least one selector is required. Names and PIDs form a union; the hidden
/// helper selector is reserved for the privilege boundary. The generated parser
/// validates argument relationships before any process can be signalled.
// Parser generates CommandFactory, FromArgMatches and Parser implementations;
// Debug generates field formatting. These derives keep clap's schema and parser
// consistent without a parallel handwritten argument definition.
#[derive(Debug, Parser)]
#[command(
    version,
    about = "Terminate Linux processes: KILL, wait, retry KILL",
    after_help = "Examples:\n  killer -p rustc rust-analyzer cargo -n\n  killer --process cargo --pid 1234 5678 --yes\n  killer -p node python3 -y -t 3",
    group(ArgGroup::new("selector").required(true).multiple(true).args(["process", "pid", "helper"]))
)]
pub(crate) struct Cli {
    /// Exact executable names; accepts multiple names and repeated -p flags.
    #[arg(short = 'p', long, num_args = 1.., value_name = "NAME", value_parser = process_name)]
    pub(crate) process: Vec<String>,
    /// Explicit positive PIDs; accepts multiple values and repeated --pid flags.
    #[arg(long, num_args = 1.., value_name = "PID", value_parser = clap::value_parser!(u32).range(1..=i32::MAX as i64))]
    pub(crate) pid: Vec<u32>,
    /// Restrict targets to effective UID.
    #[arg(long)]
    pub(crate) uid: Option<u32>,
    /// Seconds before retrying KILL on surviving targets (0..86400).
    #[arg(short, long, default_value = "5", value_parser = duration)]
    pub(crate) timeout: Duration,
    /// Seconds to wait after KILL before reporting a survivor (0..86400).
    #[arg(long, default_value = "2", value_parser = duration)]
    pub(crate) kill_wait: Duration,
    /// Inspect without signalling, confirmation or sudo.
    #[arg(short = 'n', long)]
    pub(crate) dry_run: bool,
    /// Skip interactive confirmation.
    #[arg(short = 'y', long)]
    pub(crate) yes: bool,
    /// Machine-readable report on stdout; diagnostics on stderr.
    #[arg(long)]
    pub(crate) json: bool,
    /// Never invoke sudo for foreign processes.
    #[arg(long)]
    pub(crate) no_sudo: bool,
    /// Diagnostic verbosity: error, warn, info, debug or trace.
    #[arg(long, default_value = "info", value_name = "LEVEL")]
    pub(crate) log: tracing::Level,
    /// Internal: parent PID, start ticks and held pidfd numbers.
    #[arg(long, hide = true, num_args = 3.., conflicts_with_all = ["dry_run", "process", "pid"])]
    pub(crate) helper: Vec<String>,
}

// Convert borrowed CLI seconds to a bounded duration without a panicking conversion.
/// Parse finite seconds in the inclusive range 0..86400.
///
/// Borrows `input` only for parsing; returns an owned `Duration`, rounded to
/// nanosecond precision. This function has no side effects or shared state.
///
/// # Errors
/// Returns [`Error::InvalidInput`] for malformed, non-finite or out-of-range
/// input, retaining the supplied text in the diagnostic reason.
fn duration(input: &str) -> Result<Duration> {
    let seconds: f64 = input.parse().map_err(|source| Error::InvalidInput {
        field: "duration",
        reason: format!("{input:?} is not a number of seconds: {source}"),
    })?;
    if !seconds.is_finite() || !(0.0..=86400.0).contains(&seconds) {
        return Err(Error::InvalidInput {
            field: "duration",
            reason: format!("{input:?} must be finite and within 0..86400 seconds"),
        });
    }
    Duration::try_from_secs_f64(seconds).map_err(|source| Error::InvalidInput {
        field: "duration",
        reason: format!("{input:?} cannot represent a duration: {source}"),
    })
}

// Reject empty names and paths: process selection uses executable basenames.
fn process_name(input: &str) -> Result<String> {
    if input.is_empty() || input.contains('/') || input.contains('\0') {
        return Err(Error::InvalidInput {
            field: "process",
            reason: "use a nonempty executable name, not a path".to_owned(),
        });
    }
    Ok(input.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Names and IDs are additive, repeatable and accept multiple values.
    #[test]
    fn test_cli_multiple_names_and_ids() {
        let cli = Cli::try_parse_from([
            "killer",
            "-p",
            "rustc",
            "rust-analyzer",
            "-p",
            "cargo",
            "--pid",
            "42",
            "43",
            "--pid",
            "44",
            "-ny",
        ])
        .unwrap();
        assert_eq!(cli.process, ["rustc", "rust-analyzer", "cargo"]);
        assert_eq!(cli.pid, [42, 43, 44]);
        assert!(cli.dry_run && cli.yes);
        assert_eq!(cli.timeout, Duration::from_secs(5));
    }
    // Invalid selectors must never turn into an implicit all-process selection.
    #[test]
    fn test_cli_invalid_selectors() {
        for args in [
            vec!["killer"],
            vec!["killer", "cargo"],
            vec!["killer", "-p"],
            vec!["killer", "-p", ""],
            vec!["killer", "-p", "/bin/cargo"],
            vec!["killer", "--pid", "0"],
            vec!["killer", "--pid=-1"],
            vec!["killer", "--pid", "2147483648"],
            vec!["killer", "--regex", "cargo"],
            vec!["killer", "--rustc"],
        ] {
            assert!(Cli::try_parse_from(&args).is_err(), "{args:?}");
        }
    }
    // Internal transport cannot be mixed with preview or user selections.
    #[test]
    fn test_cli_helper_conflicts() {
        for args in [
            vec!["killer", "-n", "--helper", "42", "900", "43:3"],
            vec!["killer", "-p", "cargo", "--helper", "42", "900", "43:3"],
            vec!["killer", "--pid", "43", "--helper", "42", "900", "43:3"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
        assert!(Cli::try_parse_from(["killer", "--helper", "42", "900", "43:3"]).is_ok());
    }
    // Both timeouts remain finite and bounded, with fractional seconds accepted.
    #[test]
    fn test_duration_bounds() {
        for input in ["NaN", "inf", "-1", "86401", "abc", ""] {
            assert!(duration(input).is_err());
        }
        assert_eq!(duration("0.125").unwrap(), Duration::from_millis(125));
        assert_eq!(duration("86400").unwrap(), Duration::from_secs(86400));
        for flag in ["--timeout=NaN", "--kill-wait=-1", "--log=verbose"] {
            assert!(Cli::try_parse_from(["killer", "-p", "cargo", flag]).is_err());
        }
    }
}
