//! Sandboxed verify runner for the work driver (R15, C-09).
//!
//! # Description
//! [`build`] assembles a [`CoordinatorVerifyRunner`] over a file-backed job
//! store (`<home>/jobs/verify/records`) and a [`LinuxExecutor`] with a real
//! sandbox backend: the Landlock trampoline (`harw-job-exec`, found next to
//! the running binary or on `PATH`) or, failing that, bubblewrap
//! (`BwrapExecutor::discover`). Workspace root is the (absolute) working
//! directory. The job environment is explicit: `PATH`, `HOME`, and
//! `CARGO_HOME`/`RUSTUP_HOME` when set; nothing else is inherited.
//!
//! # Fail closed
//! No backend, a relative working directory or any setup error → `None`
//! with one `warn!` naming the reason. The work driver then keeps its
//! existing fallback verifier; a command never runs unsandboxed through
//! this module. On non-Linux hosts [`build`] always returns `None`.
//!
//! # Limits
//! [`CoordinatorVerifyRunner`] always requests
//! `SandboxRequirement::Required`, which includes enforced resource limits.
//! Those need a delegated cgroup v2 root: set `HARW_VERIFY_CGROUP_ROOT` to
//! one (e.g. a `Delegate=yes` subgroup of the `harw serve` unit). It is only
//! used when the host probe confirms delegation. Without it the resource
//! dimension stays `Partial` and commands come back as
//! `RunnerError::NoSandbox` (step unverifiable) instead of passing: fail
//! closed, never unsandboxed.

use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(target_os = "linux")]
use harw_plan_bridge::verify_exec::CoordinatorVerifyRunner;
#[cfg(not(target_os = "linux"))]
use harw_plan_bridge::verify_exec::NoSandboxRunner;
use harw_plan_bridge::verify_exec::{CommandRequest, CommandRun, RunnerError, VerifyRunner};

#[cfg(target_os = "linux")]
use harw_job_runtime::{
    Coordinator, CoordinatorConfig, LinuxExecutor, LinuxExecutorOptions, LinuxSandboxBackend,
    RunnerId,
};
#[cfg(target_os = "linux")]
use harw_job_store::FsJobRecordStore;

/// Name of the Landlock trampoline binary.
#[cfg(target_os = "linux")]
const TRAMPOLINE_NAME: &str = "harw-job-exec";

/// Stable runner id of the verify coordinator (recovery finds its jobs).
#[cfg(target_os = "linux")]
const RUNNER_ID: &str = "work-driver-verify";

/// Environment variable naming a delegated cgroup v2 root for verify jobs.
#[cfg(target_os = "linux")]
const CGROUP_ROOT_ENV: &str = "HARW_VERIFY_CGROUP_ROOT";

/// Environment variables passed into every verify job (when set).
#[cfg(target_os = "linux")]
const PASSED_ENV: [&str; 4] = ["PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME"];

/// The coordinator-backed runner on Linux.
#[cfg(target_os = "linux")]
type Inner = CoordinatorVerifyRunner<FsJobRecordStore, LinuxExecutor>;

/// Placeholder elsewhere: [`build`] never constructs one there.
#[cfg(not(target_os = "linux"))]
type Inner = NoSandboxRunner;

/// Sandboxed [`VerifyRunner`] of the work driver (see the module docs).
///
/// # Concurrency
/// Cheap to clone (shared coordinator); `run` must be called inside a
/// Tokio runtime.
#[derive(Clone, Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct SandboxVerifier {
    inner: Arc<Inner>,
}

impl VerifyRunner for SandboxVerifier {
    async fn run(&self, request: &CommandRequest) -> Result<CommandRun, RunnerError> {
        self.inner.run(request).await
    }
}

/// Builds the sandboxed verify runner, or `None` (with one `warn!`) if no
/// sandbox backend is available or setup fails.
#[cfg(target_os = "linux")]
pub(crate) fn build(home: &Path, cwd: &Path) -> Option<SandboxVerifier> {
    match try_build(home, cwd) {
        Ok(verifier) => Some(verifier),
        Err(reason) => {
            tracing::warn!(
                reason = %reason,
                "sandboxed verify unavailable; work driver keeps its fallback verifier"
            );
            None
        }
    }
}

/// Non-Linux: no coordinator sandbox backend, always `None`.
#[cfg(not(target_os = "linux"))]
pub(crate) fn build(home: &Path, cwd: &Path) -> Option<SandboxVerifier> {
    let _ = (home, cwd);
    tracing::warn!(
        reason = "no sandbox backend on this platform",
        "sandboxed verify unavailable; work driver keeps its fallback verifier"
    );
    None
}

#[cfg(target_os = "linux")]
fn try_build(home: &Path, cwd: &Path) -> Result<SandboxVerifier, String> {
    if !cwd.is_absolute() {
        return Err(format!(
            "working directory {} is not absolute",
            cwd.display()
        ));
    }
    let base = home.join("jobs").join("verify");
    let sandbox = select_backend(&base)?;
    let executor = LinuxExecutor::new(LinuxExecutorOptions {
        sandbox,
        cgroup_root: delegated_cgroup_root(std::env::var_os(CGROUP_ROOT_ENV).map(PathBuf::from)),
        ..LinuxExecutorOptions::default()
    })
    .map_err(|error| format!("linux executor: {error}"))?;
    let store = FsJobRecordStore::create_ambient(&base.join("records"))
        .map_err(|error| format!("job store {}: {error}", base.display()))?;
    let runner_id = RunnerId::new(RUNNER_ID).map_err(|error| format!("runner id: {error}"))?;
    let config = CoordinatorConfig::new(runner_id, cwd.to_path_buf());
    let coordinator = Coordinator::new(store, executor, config)
        .map_err(|error| format!("coordinator: {error}"))?;
    let mut runner = CoordinatorVerifyRunner::new(coordinator);
    for (name, value) in verify_env(|name| std::env::var(name).ok()) {
        runner = runner.with_env(name, value);
    }
    Ok(SandboxVerifier {
        inner: Arc::new(runner),
    })
}

/// `root` if it is absolute and the host probe confirms it as a delegated
/// cgroup v2 root; otherwise `None` (with one `warn!` when a root was set).
#[cfg(target_os = "linux")]
fn delegated_cgroup_root(root: Option<PathBuf>) -> Option<PathBuf> {
    let root = root?;
    let delegated = root.is_absolute()
        && harw_job_runtime::HostReport::probe_with_cgroup_root(Some(&root)).cgroup_v2_delegated;
    if delegated {
        Some(root)
    } else {
        tracing::warn!(
            root = %root.display(),
            "{CGROUP_ROOT_ENV} is not a delegated cgroup v2 root; verify jobs run without cgroup limits"
        );
        None
    }
}

/// Landlock trampoline if the binary is found, else bubblewrap.
#[cfg(target_os = "linux")]
fn select_backend(base: &Path) -> Result<LinuxSandboxBackend, String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let path_var = std::env::var_os("PATH");
    if let Some(trampoline) = find_trampoline(exe_dir.as_deref(), path_var.as_deref()) {
        let plan_dir = base.join("plans");
        create_private_dir(&plan_dir)?;
        return Ok(LinuxSandboxBackend::LandlockTrampoline {
            trampoline,
            plan_dir,
        });
    }
    harw_job_executor_bwrap::BwrapExecutor::discover()
        .map(LinuxSandboxBackend::Bwrap)
        .map_err(|error| format!("no {TRAMPOLINE_NAME} trampoline and no bwrap: {error}"))
}

/// Creates `dir` (and parents) and restricts it to `0700`.
#[cfg(target_os = "linux")]
fn create_private_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|error| format!("plan dir {}: {error}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("plan dir {} permissions: {error}", dir.display()))
}

/// Finds an executable `harw-job-exec`: first in `exe_dir` (the running
/// binary's directory), then in the absolute entries of `path_var`.
/// Relative `PATH` entries are skipped (the backend needs an absolute path).
#[cfg(target_os = "linux")]
fn find_trampoline(exe_dir: Option<&Path>, path_var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let from_path = path_var
        .map(|var| std::env::split_paths(var).collect::<Vec<_>>())
        .unwrap_or_default();
    exe_dir
        .map(Path::to_path_buf)
        .into_iter()
        .chain(from_path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(TRAMPOLINE_NAME))
        .find(|candidate| is_executable_file(candidate))
}

/// A regular file with at least one execute bit.
#[cfg(target_os = "linux")]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// The explicit job environment: every [`PASSED_ENV`] name `lookup` knows.
#[cfg(target_os = "linux")]
fn verify_env(lookup: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    PASSED_ENV
        .iter()
        .filter_map(|name| lookup(name).map(|value| ((*name).to_owned(), value)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn build_with_relative_cwd_is_none() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        if build(home.path(), Path::new("relative/dir")).is_some() {
            return Err(TestError::Unexpected(
                "relative cwd must not yield a verifier".to_owned(),
            ));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_root_is_only_used_when_delegated() -> TestResult {
        if delegated_cgroup_root(None).is_some() {
            return Err(TestError::Unexpected("no root must stay None".to_owned()));
        }
        if delegated_cgroup_root(Some(PathBuf::from("relative/cgroup"))).is_some() {
            return Err(TestError::Unexpected(
                "relative root must be ignored".to_owned(),
            ));
        }
        let plain = tempfile::tempdir().map_err(ctx("tempdir"))?;
        if delegated_cgroup_root(Some(plain.path().to_path_buf())).is_some() {
            return Err(TestError::Unexpected(
                "a plain directory is no delegated cgroup v2 root".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn verifier_is_clone_and_debug() {
        fn assert_traits<T: Clone + std::fmt::Debug + Send + Sync>() {}
        assert_traits::<SandboxVerifier>();
    }

    #[cfg(target_os = "linux")]
    fn write_executable(dir: &Path) -> TestResult<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(TRAMPOLINE_NAME);
        std::fs::write(&path, b"#!/bin/sh\n").map_err(ctx("write trampoline"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(ctx("chmod trampoline"))?;
        Ok(path)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn find_trampoline_prefers_exe_dir_and_falls_back_to_path() -> TestResult {
        let exe_dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path_dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let in_exe = write_executable(exe_dir.path())?;
        let in_path = write_executable(path_dir.path())?;
        let path_var = std::env::join_paths([path_dir.path()]).map_err(ctx("join PATH"))?;

        let found = find_trampoline(Some(exe_dir.path()), Some(&path_var));
        if found.as_deref() != Some(in_exe.as_path()) {
            return Err(TestError::Unexpected(format!(
                "exe dir first, got {found:?}"
            )));
        }
        let empty = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let found = find_trampoline(Some(empty.path()), Some(&path_var));
        if found.as_deref() != Some(in_path.as_path()) {
            return Err(TestError::Unexpected(format!(
                "PATH fallback, got {found:?}"
            )));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn find_trampoline_ignores_missing_non_executable_and_relative() -> TestResult {
        let empty = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let plain = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::write(plain.path().join(TRAMPOLINE_NAME), b"data")
            .map_err(ctx("write plain file"))?;
        let path_var = std::env::join_paths([Path::new("relative"), plain.path()])
            .map_err(ctx("join PATH"))?;

        let found = find_trampoline(Some(empty.path()), Some(&path_var));
        if found.is_some() {
            return Err(TestError::Unexpected(format!(
                "nothing to find, got {found:?}"
            )));
        }
        if find_trampoline(None, None).is_some() {
            return Err(TestError::Unexpected(
                "no dirs must find nothing".to_owned(),
            ));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn verify_env_passes_only_known_set_variables() -> TestResult {
        let env = verify_env(|name| match name {
            "PATH" => Some("/usr/bin".to_owned()),
            "HOME" => Some("/home/u".to_owned()),
            "SECRET" => Some("leak".to_owned()),
            _ => None,
        });
        let expected = vec![
            ("PATH".to_owned(), "/usr/bin".to_owned()),
            ("HOME".to_owned(), "/home/u".to_owned()),
        ];
        if env != expected {
            return Err(TestError::Unexpected(format!("env {env:?}")));
        }
        Ok(())
    }
}
