//! Background trigger of the automatic UIA build (#22).
//!
//! On a TUI start (and after `harw agent uia-new`) harw checks cheaply
//! whether the active UIA could be built (auto-build enabled, a UIA
//! configured, a runner installed) and, if so, starts
//! `harw agent auto-build-uia` as a detached process: it never blocks the
//! start, and the digest comparison and the build itself happen there
//! (`harw_agent_compiler::uia::auto_build_uia`). A failure is recorded and
//! shown once as a notice on the next start.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use harw_agent_compiler::CompilerEnv;
use harw_agent_compiler::backend::runner::locate_runner;
use harw_agent_compiler::cache::AgentCompilerSettings;

/// Lock file that keeps two starts from building at the same time.
const LOCK_FILE: &str = ".auto-build-uia.lock";

/// A lock older than this is considered stale (a crashed build).
const LOCK_STALE_SECS: u64 = 30 * 60;

/// Whether a detached auto-build should be started (cheap checks only).
#[must_use]
pub fn should_spawn(env: &CompilerEnv) -> bool {
    let settings = AgentCompilerSettings::load(env);
    settings.auto_build_uia()
        && settings.active_uia_definition.is_some()
        && locate_runner(env, None, &env.host_target).is_ok()
        && !lock_is_fresh(&env.bin_dir().join(LOCK_FILE))
}

fn lock_is_fresh(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age.as_secs() < LOCK_STALE_SECS)
}

/// Shows a failed auto-build once and starts a new one in the background
/// if needed. Never fails, never blocks.
pub fn on_start(home_override: Option<PathBuf>) {
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let Ok(env) = CompilerEnv::detect(home_override.clone(), cwd) else {
        return;
    };
    if let Some(notice) = harw_agent_compiler::uia::take_auto_build_notice(&env) {
        tracing::warn!(%notice, "agent.auto_build_uia.failed");
        eprintln!("harw: {notice} (details: harw agent doctor)");
    }
    if !should_spawn(&env) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let lock = env.bin_dir().join(LOCK_FILE);
    if std::fs::create_dir_all(env.bin_dir()).is_err() || std::fs::write(&lock, b"").is_err() {
        return;
    }
    let mut command = Command::new(exe);
    if let Some(home) = home_override {
        command.arg("--home").arg(home);
    }
    command
        .args(["agent", "auto-build-uia", "--release-lock"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match command.spawn() {
        Ok(mut child) => {
            tracing::debug!(pid = child.id(), "agent.auto_build_uia.spawned");
            // Reap the child when it ends; harw may exit first, the build
            // then simply continues on its own.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(error) => {
            let _ = std::fs::remove_file(&lock);
            tracing::debug!(%error, "agent.auto_build_uia.spawn_failed");
        }
    }
}

/// Removes the lock (the detached build calls this when it is done).
pub fn release_lock(env: &CompilerEnv) {
    let _ = std::fs::remove_file(env.bin_dir().join(LOCK_FILE));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_spawn_without_uia_or_runner() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let env = CompilerEnv::isolated(root.path().join("home"), root.path().to_path_buf());
        assert!(!should_spawn(&env), "no UIA, no runner");
        std::fs::create_dir_all(&env.home)?;
        std::fs::write(
            env.home.join("config.toml"),
            "active_uia_definition = \"user.agent.mia@1\"\n",
        )?;
        assert!(!should_spawn(&env), "a UIA but no runner: skipped silently");
        Ok(())
    }
}
