//! `harw completions`: verbindet den fertigen clap-Befehl
//! ([`crate::cli::command`], inklusive deutscher Hilfetexte) mit
//! `harw_completions`.
//!
//! Erzeugt das Vervollständigungsskript für `harw` (stdout) oder installiert
//! bzw. entfernt es über [`harw_completions::run_completions`]. Mit
//! `--all-binaries` wird `--install`/`--uninstall` zusätzlich an jedes
//! weitere harw-Programm aus [`DOD_BINARIES`] im `$PATH` weitergereicht;
//! fehlende Programme werden still übersprungen.

use std::ffi::OsStr;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use harw_completions::{HomeEnv, detect_shell, run_completions};

use crate::cli::CompletionsCommand;

/// Programmname, für den das `harw`-Vervollständigungsskript erzeugt wird.
const BIN_NAME: &str = "harw";

/// Weitere harw-Programme, an die `--all-binaries` Installation bzw. Entfernung weiterreicht.
pub const DOD_BINARIES: [&str; 4] = [
    "harw-sentinel",
    "harw-warden",
    "harw-probe-fs",
    "harw-probe-bpf",
];

/// Führt `harw completions` aus.
///
/// Gibt die `harw`-Vervollständigungen aus oder installiert bzw. entfernt
/// sie und reicht den Auftrag mit `--all-binaries` an jedes weitere
/// harw-Programm im `$PATH` weiter.
///
/// # Errors
/// Ein lesbarer `String`, wenn Shell-Erkennung, Erzeugung, Installation oder
/// ein weitergereichtes Programm scheitert (mehrere Fehler mit `"; "`
/// verbunden).
pub fn run(command: CompletionsCommand) -> Result<(), String> {
    let env = HomeEnv::from_process();
    {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        run_completions(
            &mut crate::cli::command(),
            BIN_NAME,
            &command.args,
            &env,
            &mut out,
        )
        .map_err(|error| error.to_string())?;
        out.flush()
            .map_err(|error| format!("Ausgabe nach stdout fehlgeschlagen: {error}"))?;
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
                failures.push(format!("{name}: Vervollständigung endete mit {status}"));
            }
            Err(error) => {
                tracing::warn!(binary = name, %error, "cannot start DoD binary");
                failures.push(format!(
                    "{name}: '{}' kann nicht gestartet werden: {error}",
                    binary.display()
                ));
            }
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Sucht die ausführbare Datei `name` in den Verzeichnissen von `path_var`.
///
/// Nutzt [`std::env::split_paths`]; ein Kandidat muss eine reguläre Datei
/// sein und unter Unix mindestens ein Ausführungsbit (`0o111`) tragen.
pub fn find_on_path(name: &str, path_var: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path_var)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

// Wahr, wenn `path` eine reguläre Datei mit Ausführungsbit (Unix) bzw. eine Datei (sonst) ist.
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
