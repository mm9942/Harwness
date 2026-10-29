//! Sandboxed verify runner for the work driver (R15, C-09).
//!
//! # Description
//! [`build`] assembles a [`CoordinatorVerifyRunner`] over a file-backed job
//! store (`<home>/jobs/verify/records`) and a [`LinuxExecutor`] with a real
//! sandbox backend chosen by the shared resolver
//! [`harw_job_runtime::select_backend`] (PL-97 E1): bubblewrap ∘ trampoline,
//! bubblewrap, or the Landlock trampoline (`harw-job-exec`, found next to
//! the running binary; `PATH` is not searched). Verify jobs use the
//! `WorkspaceBuild` profile, which needs network access, so the bubblewrap
//! backends (proxy-only egress through a relay that is not configured
//! here) are always skipped. `HARW_VERIFY_BACKEND` chooses the request:
//! `auto` (default), `bwrap-trampoline`, `bwrap`, `landlock` or `none`.
//! An explicit `bwrap-trampoline` or `bwrap` therefore ends in no backend
//! (an explicit request has no fallback) and no verify runner is built.
//! The choice, every skipped backend and a per-dimension table
//! (namespace, filesystem, network, `no_new_privs`, capabilities, resource
//! limits, cgroup) are logged once. Workspace root is the (absolute)
//! working directory. The job environment is explicit: `PATH`, `HOME`, and
//! `CARGO_HOME`/`RUSTUP_HOME` when set; nothing else is inherited.
//!
//! # Fail closed
//! No backend (including an explicitly requested one that is unavailable:
//! there is no fallback then), an unknown `HARW_VERIFY_BACKEND`, a relative
//! working directory or any setup error → `None` with one `warn!` naming
//! the reason. A plan directory that cannot be created only rules out the
//! trampoline backends (recorded as their skip reason). The work driver then keeps its
//! existing fallback verifier; a command never runs unsandboxed through
//! this module. On non-Linux hosts [`build`] always returns `None`.
//!
//! # Limits
//! [`CoordinatorVerifyRunner`] always requests
//! `SandboxRequirement::Required`, which includes enforced resource limits.
//! Those need a delegated cgroup v2 root, resolved like the host command
//! port's (`harw_job_runtime::host::resolve_delegated_root`, PL-97 E3):
//! `[jobs] cgroup_root` of the home/profile config → `HARW_CGROUP_ROOT` →
//! the deprecated `HARW_VERIFY_CGROUP_ROOT` → the own cgroup when it is
//! delegated (the `harw-serve` user unit has `Delegate=yes`). It is only
//! used when the host probe confirms delegation. `[jobs] trampoline`
//! overrides where `harw-job-exec` is taken from. Without a root the resource
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
    BackendKind, BackendPreferences, BackendRequest, BackendSelection, Coordinator,
    CoordinatorConfig, HostReport, LinuxExecutor, LinuxExecutorOptions, RunnerId, UnknownBackend,
};
#[cfg(target_os = "linux")]
use harw_job_store::FsJobRecordStore;

/// Environment variable choosing the sandbox backend of verify jobs.
#[cfg(target_os = "linux")]
const BACKEND_ENV: &str = "HARW_VERIFY_BACKEND";

/// Stable runner id of the verify coordinator (recovery finds its jobs).
#[cfg(target_os = "linux")]
const RUNNER_ID: &str = "work-driver-verify";

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
    let request = backend_request(std::env::var(BACKEND_ENV).ok().as_deref())
        .map_err(|error| format!("{BACKEND_ENV}: {error}"))?;
    let jobs = jobs_config(home);
    let cgroup_root = delegated_cgroup_root(
        jobs.cgroup_root.clone(),
        &harw_job_runtime::host::RootEnv::from_process_with_verify_alias(),
    );
    // Only the trampoline backends need the plan directory: without it
    // they are skipped (with the reason), the others stay selectable.
    let plan_dir = base.join("plans");
    let plan_dir = match create_private_dir(&plan_dir) {
        Ok(()) => Some(plan_dir),
        Err(reason) => {
            tracing::warn!(reason = %reason, "no private plan directory for verify jobs");
            None
        }
    };
    let host = HostReport::probe_with_cgroup_root(cgroup_root.as_deref());
    let selection = select_backend(&host, request, plan_dir, jobs.trampoline);
    tracing::info!(
        backend = %selection.chosen,
        reasons = ?selection.reasons,
        table = %dimension_table(selection.chosen, &host, cgroup_root.is_some()),
        chain = %verify_chain(home, cgroup_root.as_deref()).table(),
        "sandboxed verify backend"
    );
    if selection.chosen == BackendKind::None {
        return Err(format!(
            "no sandbox backend ({BACKEND_ENV}={request}): {}",
            selection.reasons.join("; ")
        ));
    }
    let executor = LinuxExecutor::new(LinuxExecutorOptions {
        sandbox: selection.backend,
        cgroup_root,
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

/// `[jobs]` of the trusted config layers below `home` (read-only); the
/// defaults with one `warn!` when the config cannot be loaded.
#[cfg(target_os = "linux")]
fn jobs_config(home: &Path) -> harw_config::harness_config::JobsToml {
    harw_home::config_layers(home)
        .map_err(|error| error.to_string())
        .and_then(|layers| harw_config::discover_config(&layers).map_err(|error| error.to_string()))
        .map(|config| config.harness.jobs)
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "config unreadable; verify jobs use the default [jobs] settings");
            harw_config::harness_config::JobsToml::default()
        })
}

/// The delegated cgroup root of verify jobs: the shared resolver
/// (`[jobs] cgroup_root` → `HARW_CGROUP_ROOT` → deprecated
/// `HARW_VERIFY_CGROUP_ROOT` → own delegated cgroup), used only when the
/// host probe confirms delegation; otherwise `None` with one `warn!`.
#[cfg(target_os = "linux")]
fn delegated_cgroup_root(
    config_root: Option<PathBuf>,
    env: &harw_job_runtime::host::RootEnv,
) -> Option<PathBuf> {
    use harw_job_runtime::host::{RootConfig, resolve_delegated_root};
    let config = RootConfig {
        cgroup_root: config_root,
    };
    let resolution = match resolve_delegated_root(&config, env) {
        Ok(resolution) => resolution,
        Err(error) => {
            tracing::warn!(%error, "verify jobs run without cgroup limits");
            return None;
        }
    };
    let Some(root) = resolution.root else {
        tracing::info!(
            hint = resolution.hint.as_deref().unwrap_or_default(),
            "verify jobs run without cgroup limits"
        );
        return None;
    };
    if let Some(hint) = &resolution.hint {
        tracing::warn!(%hint, "deprecated cgroup root variable");
    }
    if harw_job_runtime::HostReport::probe_with_cgroup_root(Some(&root)).cgroup_v2_delegated {
        Some(root)
    } else {
        tracing::warn!(
            root = %root.display(),
            source = %resolution.source,
            "not a delegated cgroup v2 root; verify jobs run without cgroup limits"
        );
        None
    }
}

/// The backend request from the value of [`BACKEND_ENV`]: unset or empty
/// means `auto`.
#[cfg(target_os = "linux")]
fn backend_request(value: Option<&str>) -> Result<BackendRequest, UnknownBackend> {
    match value.map(str::trim) {
        None | Some("") => Ok(BackendRequest::Auto),
        Some(name) => name.parse(),
    }
}

/// The shared resolver with the verify runner's preferences: the jobs need
/// network access (`WorkspaceBuild`), the trampoline is looked up next to
/// the running binary, `bwrap` at its fixed root-owned paths. Without a
/// `plan_dir` the trampoline backends are skipped.
#[cfg(target_os = "linux")]
fn select_backend(
    host: &HostReport,
    request: BackendRequest,
    plan_dir: Option<PathBuf>,
    trampoline: Option<PathBuf>,
) -> BackendSelection {
    harw_job_runtime::select_backend(
        host,
        &BackendPreferences {
            request,
            plan_dir,
            trampoline: trampoline
                .map(harw_job_runtime::ExecutableSource::Path)
                .unwrap_or_default(),
            needs_network: true,
            ..BackendPreferences::default()
        },
    )
}

/// The per-dimension table of a backend choice on `host` (what the backend
/// can enforce there; see [`harw_job_runtime::predicted_dimensions`]).
#[cfg(target_os = "linux")]
fn dimension_table(kind: BackendKind, host: &HostReport, cgroup_root: bool) -> String {
    let mut table = format!("backend {kind}");
    for (dimension, state) in harw_job_runtime::predicted_dimensions(kind, host, cgroup_root) {
        table.push_str(&format!("\n  {dimension:<16} {state}"));
    }
    table
}

/// The execution chain report next to the dimension table (PL-97 E5):
/// the shared host facts ([`crate::dod_cmd::chain_inputs`], no service
/// state, no DoD) with the cgroup root these verify jobs actually use.
#[cfg(target_os = "linux")]
fn verify_chain(home: &Path, cgroup_root: Option<&Path>) -> harw_dod_client::ExecutionChainReport {
    let inputs = crate::dod_cmd::chain_inputs(
        home,
        crate::dod_cmd::ChainScope::HostOnly,
        Some(Ok(verify_chain_root(cgroup_root))),
    );
    harw_dod_client::ExecutionChainReport::build(&inputs)
}

/// The chain's cgroup root for the verify jobs: the root
/// [`delegated_cgroup_root`] confirmed, or none.
#[cfg(target_os = "linux")]
fn verify_chain_root(cgroup_root: Option<&Path>) -> harw_dod_client::ChainRoot {
    harw_dod_client::ChainRoot {
        root: cgroup_root.map(Path::to_path_buf),
        relative: cgroup_root.and_then(harw_job_runtime::host::cgroup_path_below_mount),
        source: "verify-runner".to_owned(),
        hint: cgroup_root
            .is_none()
            .then(|| harw_job_runtime::host::NO_ROOT_HINT.to_owned()),
        delegated: cgroup_root.is_some(),
    }
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
    fn verify_chain_reports_the_root_the_verify_jobs_use() -> TestResult {
        use harw_dod_client::{ChainLevel, ChainStage};
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let none = verify_chain(home.path(), None);
        if none.cgroup_root.level != ChainLevel::Miss
            || none.reached() >= Some(ChainStage::Configured)
        {
            return Err(TestError::Unexpected(format!(
                "no root must keep the chain below configured: {}",
                none.table()
            )));
        }
        let root = verify_chain_root(Some(Path::new("/sys/fs/cgroup/harw.slice")));
        if root.relative.as_deref() != Some("/harw.slice") || !root.delegated {
            return Err(TestError::Unexpected(format!("{root:?}")));
        }
        // Verify checks neither services nor DoD: those rows are skipped.
        for item in [&none.serve, &none.dod_target, &none.warden] {
            if item.level != ChainLevel::Skip {
                return Err(TestError::Unexpected(format!("{item:?}")));
            }
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_root_is_only_used_when_delegated() -> TestResult {
        use harw_job_runtime::host::RootEnv;
        let none = RootEnv::default();
        if delegated_cgroup_root(Some(PathBuf::from("relative/cgroup")), &none).is_some() {
            return Err(TestError::Unexpected(
                "relative root must be ignored".to_owned(),
            ));
        }
        let plain = tempfile::tempdir().map_err(ctx("tempdir"))?;
        if delegated_cgroup_root(Some(plain.path().to_path_buf()), &none).is_some() {
            return Err(TestError::Unexpected(
                "a directory outside the cgroup2 mount is no delegated root".to_owned(),
            ));
        }
        // The deprecated alias still feeds the resolver (and is still
        // checked for delegation).
        let alias = RootEnv {
            cgroup_root: None,
            deprecated_verify_root: Some("/sys/fs/cgroup/harw-e3-missing.slice".into()),
        };
        if delegated_cgroup_root(None, &alias).is_some() {
            return Err(TestError::Unexpected(
                "a missing cgroup is no delegated root".to_owned(),
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
    #[test]
    fn backend_request_defaults_to_auto_and_rejects_unknown_names() -> TestResult {
        for value in [None, Some(""), Some("  "), Some("auto")] {
            let request = backend_request(value).map_err(ctx("parse"))?;
            if request != BackendRequest::Auto {
                return Err(TestError::Unexpected(format!("{value:?} -> {request}")));
            }
        }
        for kind in BackendKind::ALL {
            let request = backend_request(Some(kind.as_str())).map_err(ctx("parse kind"))?;
            if request != BackendRequest::Only(kind) {
                return Err(TestError::Unexpected(format!("{kind} -> {request}")));
            }
        }
        if backend_request(Some("podman")).is_ok() {
            return Err(TestError::Unexpected(
                "unknown backend names must fail closed".to_owned(),
            ));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn verify_backend_skips_bubblewrap_for_networked_jobs() -> TestResult {
        let plan_dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let host = HostReport::probe();
        let selection = select_backend(
            &host,
            BackendRequest::Auto,
            Some(plan_dir.path().to_path_buf()),
            None,
        );
        // WorkspaceBuild needs network: proxy-only egress has no relay here.
        if selection.chosen.uses_bwrap() {
            return Err(TestError::Unexpected(format!(
                "auto chose {} for networked verify jobs: {:?}",
                selection.chosen, selection.reasons
            )));
        }
        let skipped_bwrap = selection
            .reasons
            .iter()
            .filter(|reason| reason.starts_with("bwrap") && reason.contains("network access"))
            .count();
        if skipped_bwrap != 2 {
            return Err(TestError::Unexpected(format!("{:?}", selection.reasons)));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_bubblewrap_requests_end_in_no_backend() -> TestResult {
        let plan_dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let host = HostReport::probe();
        for kind in [BackendKind::BwrapTrampoline, BackendKind::Bwrap] {
            let request = backend_request(Some(kind.as_str())).map_err(ctx("parse"))?;
            let selection =
                select_backend(&host, request, Some(plan_dir.path().to_path_buf()), None);
            // No relay for the networked jobs, and no fallback for an
            // explicit request: fail closed, whatever the host offers.
            if selection.chosen != BackendKind::None {
                return Err(TestError::Unexpected(format!(
                    "{kind} chose {}: {:?}",
                    selection.chosen, selection.reasons
                )));
            }
            let skipped = selection
                .reasons
                .first()
                .ok_or(TestError::Missing("reason"))?;
            if !(skipped.starts_with(&format!("{kind}: skipped: "))
                && skipped.contains("network access"))
            {
                return Err(TestError::Unexpected(skipped.clone()));
            }
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_plan_dir_skips_only_the_trampoline_backends() -> TestResult {
        let host = HostReport::probe();
        let selection = select_backend(&host, BackendRequest::Auto, None, None);
        if selection.chosen.uses_trampoline() {
            return Err(TestError::Unexpected(format!(
                "a trampoline backend without a plan directory: {:?}",
                selection.reasons
            )));
        }
        let without_plan_dir = selection
            .reasons
            .iter()
            .filter(|reason| reason.contains("no plan directory"))
            .count();
        if without_plan_dir != 2 {
            return Err(TestError::Unexpected(format!("{:?}", selection.reasons)));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dimension_table_names_every_dimension() -> TestResult {
        let host = HostReport::probe();
        let table = dimension_table(BackendKind::BwrapTrampoline, &host, false);
        let lines: Vec<&str> = table.lines().collect();
        if lines.first() != Some(&"backend bwrap-trampoline") || lines.len() != 8 {
            return Err(TestError::Unexpected(table));
        }
        for dimension in [
            "namespace",
            "filesystem",
            "network",
            "no_new_privs",
            "capabilities",
            "resource_limits",
            "cgroup",
        ] {
            if !lines
                .iter()
                .any(|line| line.trim_start().starts_with(dimension))
            {
                return Err(TestError::Unexpected(format!(
                    "{dimension} missing: {table}"
                )));
            }
        }
        // Without a cgroup root the job cgroup is not enforced.
        let cgroup = lines
            .iter()
            .find(|line| line.trim_start().starts_with("cgroup"))
            .ok_or(TestError::Missing("cgroup row"))?;
        if !cgroup.ends_with(" not_enforced") {
            return Err(TestError::Unexpected(table));
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
