//! Linux process termination CLI: select, preview, authorize, execute and report.
//! Delegates CLI parsing, procfs, kernel handles and privilege handling to modules.
//! Synchronous orchestration owns all handles; no detached tasks or worker threads.
//! Selection, authorization, I/O and helper errors are reported through the typed
//! Error domain. Binary examples illustrate usage; they are not Cargo doctests.
//! # Examples
//! ```no_run
//! std::process::Command::new("killer").args(["-p", "cargo", "--dry-run"]).status()?;
//! # Ok::<(), std::io::Error>(())
//! ```
#![forbid(unsafe_code)]
#[cfg(not(target_os = "linux"))]
compile_error!("killer requires Linux procfs and pidfd support");
mod cli;
mod engine;
mod error;
mod pidfd;
mod privilege;
mod process;
mod types;
mod typestate;

use crate::{
    cli::Cli,
    error::{Error, Result},
    types::{Outcome, Process},
    typestate::plan::{Approved, Plan},
};
use clap::Parser;
use serde::Serialize;
use std::{
    borrow::Cow,
    io::{self, IsTerminal, Write},
    process::ExitCode,
};

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
fn execute(plan: &Plan<Approved>, cli: &Cli) -> Vec<Outcome> {
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
            privilege::terminate_foreign(&foreign, cli, &privilege::SystemRunner)
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
fn run(cli: &Cli) -> Result<u8> {
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
    let outcomes = execute(&plan, cli);
    let code = u8::from(!outcomes.iter().all(Outcome::success));
    report(plan.targets(), cli, outcomes)?;
    Ok(code)
}
// Initialize diagnostics exactly once, then return documented exit semantics.
fn main() -> ExitCode {
    let cli = Cli::parse();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(cli.log)
        .with_target(false)
        .with_ansi(io::stderr().is_terminal())
        .with_writer(io::stderr)
        .finish();
    if let Err(error) = tracing::subscriber::set_global_default(subscriber) {
        // Logger setup failure precedes the availability of tracing; write the CLI fatal error directly.
        let mut out = io::stderr().lock();
        if writeln!(out, "Cannot initialize tracing: {error}").is_err() {
            return ExitCode::FAILURE;
        }
        return ExitCode::FAILURE;
    }
    match run(&cli) {
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
}
