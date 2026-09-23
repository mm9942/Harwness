//! `harw completions` adapter: `Cli::command()` -> `harw_completions`.
//!
//! Generates the completion script for `harw` (stdout) or installs/uninstalls
//! it via [`harw_completions::run_completions`]. With `--all-binaries` it also
//! forwards `--install`/`--uninstall` to every harw DoD binary found on
//! `$PATH` ([`DOD_BINARIES`]); missing binaries are skipped silently.

use std::ffi::OsStr;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use clap::CommandFactory;
use harw_completions::{HomeEnv, detect_shell, run_completions};

use crate::cli::{Cli, CompletionsCommand};

/// Binary name the `harw` completion script is generated for.
const BIN_NAME: &str = "harw";

/// harw DoD binaries that `--all-binaries` forwards the install/uninstall to.
pub const DOD_BINARIES: [&str; 4] = [
    "harw-sentinel",
    "harw-warden",
    "harw-probe-fs",
    "harw-probe-bpf",
];

/// Runs `harw completions` (spec section 4).
///
/// Prints or installs/uninstalls the `harw` completions and, with
/// `--all-binaries`, delegates to every DoD binary found on `$PATH`.
///
/// # Errors
/// A human-readable `String` when shell detection, generation, installation
/// or any delegated DoD binary fails (failures are joined with `"; "`).
pub fn run(command: CompletionsCommand) -> Result<(), String> {
    let env = HomeEnv::from_process();
    {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        run_completions(&mut Cli::command(), BIN_NAME, &command.args, &env, &mut out)
            .map_err(|error| error.to_string())?;
        out.flush().map_err(|error| format!("write '<stdout>' failed: {error}"))?;
    }

    if !command.all_binaries {
        return Ok(());
    }

    let shell = match command.args.shell {
        Some(shell) => shell,
        None => detect_shell(env.shell.as_deref()).map_err(|error| error.to_string())?,
    };
    let Some(path_var) = std::env::var_os("PATH") else {
        tracing::debug!("PATH is not set; no DoD binaries to delegate to");
        return Ok(());
    };
    let action = if command.args.install {
        "--install"
    } else {
        "--uninstall"
    };

    let mut failures: Vec<String> = Vec::new();
    for name in DOD_BINARIES {
        let Some(binary) = find_on_path(name, &path_var) else {
            tracing::debug!(binary = name, "DoD binary not found on PATH; skipping");
            continue;
        };
        tracing::info!(
            binary = name,
            path = %binary.display(),
            shell = %shell,
            action,
            dry_run = command.args.dry_run,
            "delegating completions to DoD binary"
        );
        let mut child = ProcessCommand::new(&binary);
        child.arg("completions").arg(shell.to_string()).arg(action);
        if command.args.dry_run {
            child.arg("--dry-run");
        }
        match child.status() {
            Ok(status) if status.success() => {}
            Ok(status) => {
                tracing::warn!(binary = name, %status, "DoD binary completions failed");
                failures.push(format!("{name}: completions exited with {status}"));
            }
            Err(error) => {
                tracing::warn!(binary = name, %error, "cannot start DoD binary");
                failures.push(format!("{name}: cannot run '{}': {error}", binary.display()));
            }
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Finds an executable file `name` in the directories of `path_var`.
///
/// Uses [`std::env::split_paths`]; a candidate must be a regular file and, on
/// Unix, carry at least one execute bit (`0o111`).
pub fn find_on_path(name: &str, path_var: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path_var)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

// True when `path` is a regular file with an execute bit (Unix) / a file (other).
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
