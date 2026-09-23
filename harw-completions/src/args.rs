//! Reusable clap arguments for a `completions` subcommand (feature `clap-args`).
//!
//! Spec source: `harw-completions-CODING-DESIGN.md` §1 (src/args.rs).
//!
//! # Responsibility
//! Provides [`CompletionsArgs`] (flattenable into any clap command),
//! [`CompletionsSubcommand`] (a ready-made `completions` subcommand for
//! binaries without their own subcommand enum) and [`run_completions`], which
//! dispatches to printing, [`install`] or [`uninstall`].
//!
//! # Concurrency
//! Synchronous; writes only to the provided writer and the filesystem.
//!
//! # Errors
//! Propagates [`CompletionError`] from shell detection and install/uninstall;
//! writer failures become [`CompletionError::Io`] with path `"<stdout>"`.
//!
//! # Examples
//! ```no_run
//! use harw_completions::{CompletionsArgs, HomeEnv, Shell, run_completions};
//!
//! let args = CompletionsArgs { shell: Some(Shell::Bash), install: false, uninstall: false, dry_run: false };
//! let mut cmd = clap::Command::new("demo");
//! let mut out = Vec::new();
//! run_completions(&mut cmd, "demo", &args, &HomeEnv::default(), &mut out)?;
//! # Ok::<(), harw_completions::CompletionError>(())
//! ```

use std::io::Write;

use clap_complete::Shell;
use tracing::info;

use crate::error::{CompletionError, CompletionResult};
use crate::install::{InstallOptions, install, uninstall};
use crate::locations::HomeEnv;
use crate::shell::{detect_shell, generate_script};

/// Arguments of the `completions` subcommand.
#[derive(Debug, Clone, clap::Args)]
#[command(group(
    clap::ArgGroup::new("completions_action")
        .args(["install", "uninstall"])
        .multiple(false)
))]
pub struct CompletionsArgs {
    /// Target shell; detected from $SHELL when omitted.
    #[arg(value_enum)]
    pub shell: Option<Shell>,
    /// Install the completion script (replaces older harw-managed installations).
    #[arg(long)]
    pub install: bool,
    /// Remove every harw-managed completion file, rc block and cache for this binary.
    #[arg(long, conflicts_with = "install")]
    pub uninstall: bool,
    /// Show what --install/--uninstall would do without changing anything.
    #[arg(long, requires = "completions_action")]
    pub dry_run: bool,
}

/// Ready-made subcommand enum for binaries that only need `completions`.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum CompletionsSubcommand {
    /// Generate or install shell completions for this binary.
    #[command(alias = "completion")]
    Completions(CompletionsArgs),
}

// Maps a writer failure to the `<stdout>` I/O error.
fn stdout_err(source: std::io::Error) -> CompletionError {
    CompletionError::Io {
        op: "write",
        path: "<stdout>".to_owned(),
        source,
    }
}

/// Runs the `completions` subcommand: prints the script, or installs /
/// uninstalls it and prints the report (design doc §1, src/args.rs).
///
/// # Errors
/// - [`CompletionError::ShellUndetected`] when no shell was given and `$SHELL`
///   is unusable.
/// - Any error of [`install`] / [`uninstall`].
/// - [`CompletionError::Io`] (`"<stdout>"`) when writing to `out` fails.
pub fn run_completions(
    cmd: &mut clap::Command,
    bin: &str,
    args: &CompletionsArgs,
    env: &HomeEnv,
    out: &mut dyn Write,
) -> CompletionResult<()> {
    let shell = match args.shell {
        Some(shell) => shell,
        None => detect_shell(env.shell.as_deref())?,
    };
    let opts = InstallOptions {
        dry_run: args.dry_run,
    };
    info!(
        bin,
        shell = %shell,
        install = args.install,
        uninstall = args.uninstall,
        dry_run = args.dry_run,
        "running completions"
    );

    let bytes: Vec<u8> = if args.install {
        let report = install(cmd, bin, shell, env, &opts)?;
        report_bytes(&report.to_string())
    } else if args.uninstall {
        let report = uninstall(bin, shell, env, &opts)?;
        report_bytes(&report.to_string())
    } else {
        generate_script(cmd, bin, shell)
    };

    out.write_all(&bytes).map_err(stdout_err)?;
    out.flush().map_err(stdout_err)
}

// Turns a rendered report into output bytes ending with exactly one trailing newline.
fn report_bytes(rendered: &str) -> Vec<u8> {
    let mut text = rendered.to_owned();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.into_bytes()
}
