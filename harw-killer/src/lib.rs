//! Linux process termination: select, preview, authorize, execute and report.
//!
//! This library carries the complete `killer` CLI orchestration ([`run_cli`]) so
//! that the standalone `killer` binary and host CLIs (for example `harw kill`)
//! share one implementation, plus the programmatic agent API in [`api`].
//! CLI parsing, procfs, kernel handles and privilege handling live in private
//! modules. Synchronous orchestration owns all handles; no detached tasks or
//! worker threads. Selection, authorization, I/O and helper errors are reported
//! through the typed [`Error`] domain.
//!
//! # Examples
//! ```no_run
//! use harw_killer::{HelperInvocation, run_cli};
//! let code = run_cli(["killer", "-p", "cargo", "--dry-run"], HelperInvocation::Standalone);
//! # let _ = code;
//! ```
#![forbid(unsafe_code)]
#[cfg(not(target_os = "linux"))]
compile_error!("killer requires Linux procfs and pidfd support");

pub mod api;
mod cli;
mod engine;
mod error;
mod pidfd;
mod privilege;
mod process;
mod types;
mod typestate;

pub use crate::error::{Error, Result};

use crate::{
    cli::Cli,
    types::{Outcome, Process},
    typestate::plan::{Approved, Plan},
};
use clap::Parser;
use serde::Serialize;
use std::{
    borrow::Cow,
    ffi::OsString,
    io::{self, IsTerminal, Write},
    process::ExitCode,
};

/// How the sudo helper re-executes the current binary for foreign targets.
///
/// The privileged helper is the same executable started again under `sudo`
/// with the hidden `--helper` protocol. A host CLI that embeds [`run_cli`] as a
/// subcommand must name the subcommand path so the re-executed binary reaches
/// [`run_cli`] again.
///
/// # Examples
/// ```
/// use harw_killer::HelperInvocation;
/// // `harw kill --helper ...` for a host CLI with a `kill` subcommand:
/// let host = HelperInvocation::Subcommand(vec!["kill".into()]);
/// assert_ne!(host, HelperInvocation::Standalone);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HelperInvocation {
    /// `<current_exe> --helper ...` (the standalone `killer` binary).
    #[default]
    Standalone,
    /// `<current_exe> <prefix...> --helper ...`; the prefix is owned and passed
    /// verbatim as arguments directly after the executable.
    Subcommand(Vec<OsString>),
}

impl HelperInvocation {
    // Borrow the argument prefix inserted between executable and options.
    pub(crate) fn prefix(&self) -> &[OsString] {
        match self {
            Self::Standalone => &[],
            Self::Subcommand(prefix) => prefix,
        }
    }
}

// Stable JSON report wrapper; targets borrow the live plan instead of cloning it.
// Expands to Serde object serialization with borrowed target metadata.
#[derive(Serialize)]
struct Report<'a> {
    // Version the outer schema independently from package releases.
    schema_version: u8,
    // True means no signals or sudo were used.
    dry_run: bool,
    // Snapshot displayed before authorization.
    targets: Vec<&'a Process>,
    // Empty for previews, otherwise one result per selected process.
    results: Vec<Outcome>,
}
// Escape terminal controls while borrowing ordinary strings unchanged.
fn visible(text: &str) -> Cow<'_, str> {
    if text.chars().any(char::is_control) {
        Cow::Owned(text.chars().flat_map(char::escape_debug).collect())
    } else {
        Cow::Borrowed(text)
    }
}
// Write user-facing tabular output; these are CLI data, not diagnostic logs.
fn preview(targets: &[types::Target]) -> Result<()> {
    let mut out = io::stdout().lock();
    writeln!(
        out,
        "{:>8} {:>8} {:>8}  {:<22} COMMAND",
        "PID", "PPID", "EUID", "NAME"
    )?;
    for target in targets {
        let p = &target.process;
        writeln!(
            out,
            "{:>8} {:>8} {:>8}  {:<22} {}",
            p.pid,
            p.ppid,
            p.uid,
            visible(&p.name),
            visible(&p.command)
        )?;
    }
    out.flush()?;
    Ok(())
}
// Require a terminal confirmation unless explicit --yes made intent clear.
fn confirm(cli: &Cli) -> Result<bool> {
    if cli.yes {
        return Ok(true);
    }
    if !io::stdin().is_terminal() {
        return Err(Error::Permission {
            reason: "noninteractive termination requires --yes; inspect with --dry-run",
        });
    }
    let mut prompt = io::stderr().lock();
    write!(
        prompt,
        "Send KILL to all displayed targets, then retry KILL for survivors? [y/N] "
    )?;
    prompt.flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes" | "j" | "ja"
    ))
}
// Only a statically approved plan may reach process termination.
fn execute(plan: &Plan<Approved>, cli: &Cli, helper: &HelperInvocation) -> Vec<Outcome> {
    let uid = rustix::process::geteuid();
    let (local, foreign): (Vec<_>, Vec<_>) = plan
        .targets()
        .iter()
        .partition(|t| uid.is_root() || t.process.uid == uid.as_raw());
    let handles: Vec<_> = local.iter().map(|t| (t.process.pid, &t.fd)).collect();
    let mut outcomes = engine::terminate(&handles, cli.timeout, cli.kill_wait);
    if !foreign.is_empty() {
        let result = if cli.no_sudo {
            Err(Error::Permission {
                reason: "foreign process requires privileges; --no-sudo is set",
            })
        } else {
            privilege::terminate_foreign(&foreign, cli, helper, &privilege::SystemRunner)
        };
        match result {
            Ok(mut reports) => outcomes.append(&mut reports),
            Err(e) => {
                tracing::error!(error=%e,"foreign process termination failed");
                outcomes.extend(foreign.iter().map(|t| Outcome::error(t.process.pid, &e)));
            }
        }
    }
    outcomes.sort_by_key(|result| result.pid);
    outcomes
}
// Serialize CLI data separately from structured diagnostic stderr.
fn report(plan: &[types::Target], cli: &Cli, outcomes: Vec<Outcome>) -> Result<()> {
    let mut out = io::stdout().lock();
    if cli.json {
        serde_json::to_writer_pretty(
            &mut out,
            &Report {
                schema_version: 1,
                dry_run: cli.dry_run,
                targets: plan.iter().map(|t| &t.process).collect(),
                results: outcomes,
            },
        )?;
        writeln!(out)?;
    } else {
        for result in outcomes {
            writeln!(
                out,
                "{}: {:?}{}",
                result.pid,
                result.outcome,
                result
                    .detail
                    .as_deref()
                    .map(|s| format!(" — {}", visible(s)))
                    .unwrap_or_default()
            )?;
        }
    }
    out.flush()?;
    Ok(())
}
// Manage regular CLI and the narrowly scoped privileged helper protocol.
fn run(cli: &Cli, helper: &HelperInvocation) -> Result<u8> {
    if !cli.helper.is_empty() {
        let outcomes = privilege::helper(cli)?;
        let code = u8::from(!outcomes.iter().all(Outcome::success));
        let mut out = io::stdout().lock();
        serde_json::to_writer(&mut out, &outcomes)?;
        writeln!(out)?;
        return Ok(code);
    }
    let plan = Plan::new(process::select(cli)?);
    tracing::info!(
        count = plan.targets().len(),
        dry_run = cli.dry_run,
        "selection complete"
    );
    if plan.targets().is_empty() {
        report(plan.targets(), cli, Vec::new())?;
        return Ok(2);
    }
    if !cli.json {
        preview(plan.targets())?;
    }
    if cli.dry_run {
        report(plan.targets(), cli, Vec::new())?;
        return Ok(0);
    }
    // JSON termination needs explicit intent so the selected targets never stay hidden behind a prompt.
    if cli.json && !cli.yes {
        return Err(Error::Permission {
            reason: "--json termination requires --yes; inspect with --dry-run --json",
        });
    }
    if !confirm(cli)? {
        tracing::info!("cancelled by user");
        return Ok(3);
    }
    let plan = plan.approve();
    let outcomes = execute(&plan, cli, helper);
    let code = u8::from(!outcomes.iter().all(Outcome::success));
    report(plan.targets(), cli, outcomes)?;
    Ok(code)
}
/// Parse `args`, initialize diagnostics and run the complete `killer` CLI.
///
/// `args` includes the program name as first element (as `std::env::args_os`).
/// `helper` determines how the sudo helper re-executes the current binary for
/// foreign-owned targets; see [`HelperInvocation`].
///
/// # Returns
/// The documented exit semantics: 0 success (or dry-run preview, `--help`,
/// `--version`), 1 failure or unsuccessful target, 2 no target selected or
/// invalid arguments (clap usage error), 3 cancelled by the user.
///
/// # Diagnostics
/// Installs a global `tracing` subscriber at the `--log` level on stderr. In
/// [`HelperInvocation::Standalone`] mode an already installed global subscriber
/// is a fatal error (as before); in [`HelperInvocation::Subcommand`] mode the
/// host's subscriber is kept.
///
/// # Examples
/// ```no_run
/// let code = harw_killer::run_cli(std::env::args_os(), harw_killer::HelperInvocation::Standalone);
/// # let _ = code;
/// ```
pub fn run_cli<I, T>(args: I, helper: HelperInvocation) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            // Clap prints help/version to stdout and usage errors to stderr.
            if error.print().is_err() {
                return ExitCode::FAILURE;
            }
            return ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(2));
        }
    };
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(cli.log)
        .with_target(false)
        .with_ansi(io::stderr().is_terminal())
        .with_writer(io::stderr)
        .finish();
    if let Err(error) = tracing::subscriber::set_global_default(subscriber)
        && helper == HelperInvocation::Standalone
    {
        // Logger setup failure precedes the availability of tracing; write the CLI fatal error directly.
        let mut out = io::stderr().lock();
        if writeln!(out, "Cannot initialize tracing: {error}").is_err() {
            return ExitCode::FAILURE;
        }
        return ExitCode::FAILURE;
    }
    match run(&cli, &helper) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            tracing::error!(error=%error,"killer failed");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Terminal escape sequences must not become executable control output.
    #[test]
    fn test_visible_escapes_terminal_control() {
        assert!(matches!(visible("cargo"), Cow::Borrowed("cargo")));
        assert_eq!(visible("a\nb\u{1b}[2J"), "a\\nb\\u{1b}[2J");
    }
    // Standalone re-execution adds nothing; subcommands keep their order.
    #[test]
    fn test_helper_invocation_prefix() {
        assert!(HelperInvocation::Standalone.prefix().is_empty());
        let host = HelperInvocation::Subcommand(vec!["tools".into(), "kill".into()]);
        assert_eq!(host.prefix(), [OsString::from("tools"), OsString::from("kill")]);
        assert_eq!(HelperInvocation::default(), HelperInvocation::Standalone);
    }
    // Usage errors never reach selection; help exits successfully.
    #[test]
    fn test_run_cli_parse_errors_do_not_select() {
        assert_eq!(
            run_cli(["killer"], HelperInvocation::Standalone),
            ExitCode::from(2)
        );
        assert_eq!(
            run_cli(["killer", "--regex", "x"], HelperInvocation::Standalone),
            ExitCode::from(2)
        );
    }
}
