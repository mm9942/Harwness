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

/// Lock file that keeps two starts from building at the same time. Acquired
/// atomically by [`try_acquire_lock`], never by a plain check-then-write.
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

/// Atomically acquires the auto-build lock (`create_new`, not check-then-write):
/// two starts racing for the lock cannot both succeed. If the existing lock
/// is stale (a crashed build), it is removed and creation is retried once.
///
/// Returns `true` if the lock is now held by the caller, who must then
/// either spawn the build or remove the lock again on failure. Returns
/// `false` if another build already holds a fresh lock, or if the retry
/// after removing a stale lock lost a race to a third starter.
fn try_acquire_lock(path: &std::path::Path) -> bool {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if lock_is_fresh(path) {
                return false;
            }
            // Stale lock: remove it and retry exactly once. If the removal
            // fails (e.g. a third starter already replaced it), give up
            // instead of looping.
            std::fs::remove_file(path).is_ok()
                && std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .is_ok()
        }
        Err(_) => false,
    }
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
    if std::fs::create_dir_all(env.bin_dir()).is_err() || !try_acquire_lock(&lock) {
        // Either the directory could not be created, or another start
        // already holds a fresh lock (or won the race to replace a stale
        // one): do not spawn a second build.
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
    use crate::test_support::{TestResult, ctx, some_or};

    #[test]
    fn test_no_spawn_without_uia_or_runner() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("create temp dir"))?;
        let env = CompilerEnv::isolated(root.path().join("home"), root.path().to_path_buf());
        assert!(!should_spawn(&env), "no UIA, no runner");
        std::fs::create_dir_all(&env.home).map_err(ctx("create home dir"))?;
        std::fs::write(
            env.home.join("config.toml"),
            "active_uia_definition = \"user.agent.mia@1\"\n",
        )
        .map_err(ctx("write config.toml"))?;
        assert!(!should_spawn(&env), "a UIA but no runner: skipped silently");
        Ok(())
    }

    /// A fresh lock (just written by another start) must block a second
    /// acquire: this is the race the check-then-write bug allowed through.
    #[test]
    fn test_try_acquire_lock_blocks_on_fresh_lock() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("create temp dir"))?;
        let lock = root.path().join(LOCK_FILE);
        std::fs::write(&lock, b"").map_err(ctx("write lock file"))?;
        assert!(
            !try_acquire_lock(&lock),
            "a fresh lock must not be acquired a second time"
        );
        Ok(())
    }

    /// The first acquire on an absent lock succeeds; an immediate second
    /// acquire (simulating a racing start) must fail; after releasing the
    /// lock, acquiring it again must succeed.
    #[test]
    fn test_try_acquire_lock_first_wins_second_fails_then_released() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("create temp dir"))?;
        let lock = root.path().join(LOCK_FILE);
        assert!(
            try_acquire_lock(&lock),
            "no lock file yet: the first acquire must succeed"
        );
        assert!(
            !try_acquire_lock(&lock),
            "a lock just acquired must block an immediate second acquire"
        );
        std::fs::remove_file(&lock).map_err(ctx("remove lock file"))?;
        assert!(
            try_acquire_lock(&lock),
            "after releasing the lock, acquiring it again must succeed"
        );
        Ok(())
    }

    /// A stale lock (older than the crash threshold) is replaced, not
    /// left in place: `try_acquire_lock` must still succeed.
    #[test]
    fn test_try_acquire_lock_replaces_stale_lock() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("create temp dir"))?;
        let lock = root.path().join(LOCK_FILE);
        std::fs::write(&lock, b"").map_err(ctx("write lock file"))?;
        let stale = some_or(
            std::time::SystemTime::now()
                .checked_sub(std::time::Duration::from_secs(LOCK_STALE_SECS + 60)),
            "stale timestamp underflows SystemTime",
        )?;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&lock)
            .map_err(ctx("reopen lock file for mtime update"))?;
        file.set_modified(stale).map_err(ctx("set stale mtime"))?;
        assert!(
            try_acquire_lock(&lock),
            "a stale lock must be replaced, not block the acquire"
        );
        Ok(())
    }
}
