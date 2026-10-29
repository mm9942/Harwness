//! Pure mapping tests: no process is started, only argv and report are
//! inspected.

use std::path::{Path, PathBuf};

use harw_job_core::{
    EnforcementState as S, JobScopeId, JobSpec, ResourceRequest, SandboxProfileName, SandboxReport,
    SandboxRequirement, WorkspacePath,
};
use harw_job_linux::{BindMode, CapabilityPolicy, NetworkPolicy, SandboxPolicy};
use harw_sandbox::{NetworkMode, RelaySpec, SANDBOX_RELAY_PATH};

use super::{
    BwrapExecutor, BwrapJobPlan, BwrapTrampolinePlan, SANDBOX_ATTEMPT_DIR, SANDBOX_TRAMPOLINE_PATH,
    TrampolineMount,
};
use crate::error::{BwrapExecutorError, Dimension};
use crate::test_support::{TestError, TestResult, ctx};

const ALL_PROFILES: [SandboxProfileName; 4] = [
    SandboxProfileName::WorkspaceBuild,
    SandboxProfileName::ReadOnlyAnalysis,
    SandboxProfileName::NoNetwork,
    SandboxProfileName::NetworkRestricted,
];

fn executor() -> BwrapExecutor {
    BwrapExecutor::from_executable(PathBuf::from("/usr/bin/bwrap")).with_identity(1000, 1000)
}

fn relay() -> RelaySpec {
    RelaySpec {
        binary: PathBuf::from("/opt/harw/bin/harw-netns-relay"),
        listen_port: 1080,
        proxy_socket: PathBuf::from("/run/user/1000/harw/egress.sock"),
    }
}

fn job(program: &str, args: &[&str]) -> TestResult<JobSpec> {
    Ok(JobSpec {
        program: program.to_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        working_dir: WorkspacePath::root(),
        env: Vec::new(),
        resources: ResourceRequest::default(),
        sandbox: SandboxRequirement::BestEffort,
        sandbox_profile: SandboxProfileName::NoNetwork,
        idempotency_key: None,
        scope: JobScopeId::new("scope-a").map_err(ctx("scope"))?,
    })
}

/// A temporary workspace and its canonical path (the path bwrap binds).
fn workspace() -> TestResult<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let canonical = dir.path().canonicalize().map_err(ctx("canonicalize"))?;
    Ok((dir, canonical))
}

fn strings(plan: &BwrapJobPlan) -> Vec<String> {
    plan.args()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

fn has_triple(args: &[String], first: &str, second: &str, third: &str) -> bool {
    args.windows(3)
        .any(|window| matches!(window, [a, b, c] if a == first && b == second && c == third))
}

fn has_pair(args: &[String], first: &str, second: &str) -> bool {
    args.windows(2)
        .any(|window| matches!(window, [a, b] if a == first && b == second))
}

fn expect_unsupported(
    result: Result<BwrapJobPlan, BwrapExecutorError>,
    expected: Dimension,
) -> TestResult<String> {
    match result {
        Err(BwrapExecutorError::Unsupported { dimension, reason }) if dimension == expected => {
            Ok(reason)
        }
        Err(other) => Err(TestError::Unexpected(format!(
            "expected Unsupported({expected}), got error: {other}"
        ))),
        Ok(plan) => Err(TestError::Unexpected(format!(
            "expected Unsupported({expected}), got plan: {:?}",
            plan.args()
        ))),
    }
}

#[test]
fn every_profile_maps_to_a_hermetic_bwrap_plan() -> TestResult {
    let (_dir, root) = workspace()?;
    let ws = root.to_string_lossy().into_owned();
    let executor = executor().with_proxy_relay(relay());
    for profile in ALL_PROFILES {
        let policy = SandboxPolicy::from_profile(profile, &root);
        let plan = executor
            .plan(&job("true", &[])?, &policy, &root)
            .map_err(ctx("plan"))?;
        let args = strings(&plan);
        for flag in [
            "--die-with-parent",
            "--new-session",
            "--unshare-all",
            "--unshare-net",
            "--clearenv",
        ] {
            assert!(args.iter().any(|arg| arg == flag), "{profile:?}: {flag}");
        }
        assert!(
            !args.iter().any(|arg| arg == "--share-net"),
            "{profile:?}: host netns must never be shared"
        );
        // System trees read-only; policy read-only extras via --ro-bind-try.
        if Path::new("/usr").exists() {
            assert!(
                has_triple(&args, "--ro-bind", "/usr", "/usr"),
                "{profile:?}"
            );
        }
        assert!(
            has_triple(&args, "--ro-bind-try", "/etc", "/etc"),
            "{profile:?}"
        );
        assert!(
            has_triple(&args, "--ro-bind-try", "/sbin", "/sbin"),
            "{profile:?}"
        );
        // bwrap's own /proc, /dev and /tmp; never the host's.
        assert!(has_pair(&args, "--proc", "/proc"), "{profile:?}");
        assert!(has_pair(&args, "--dev", "/dev"), "{profile:?}");
        assert!(has_pair(&args, "--tmpfs", "/tmp"), "{profile:?}");
        for host in ["/proc", "/dev", "/tmp", "/sys", "/run"] {
            assert!(
                !has_triple(&args, "--ro-bind-try", host, host)
                    && !has_triple(&args, "--bind", host, host),
                "{profile:?}: host {host} must not be bound"
            );
        }
        assert_eq!(
            plan.withheld_paths(),
            [PathBuf::from("/sys"), PathBuf::from("/run")],
            "{profile:?}"
        );
        // Workspace: read-write except for read-only analysis.
        let writable = profile != SandboxProfileName::ReadOnlyAnalysis;
        assert_eq!(plan.workspace_writable(), writable, "{profile:?}");
        assert_eq!(
            has_triple(&args, "--bind", &ws, &ws),
            writable,
            "{profile:?}"
        );
        assert_eq!(
            has_triple(&args, "--ro-bind", &ws, &ws),
            !writable,
            "{profile:?}"
        );
        assert!(has_pair(&args, "--chdir", &ws), "{profile:?}");
        assert_eq!(args.last().map(String::as_str), Some("true"), "{profile:?}");
        assert_eq!(plan.executable(), Path::new("/usr/bin/bwrap"));
        assert_eq!(plan.workspace(), root.as_path());
    }
    Ok(())
}

#[test]
fn network_without_relay_is_unsupported() -> TestResult {
    let (_dir, root) = workspace()?;
    for profile in [
        SandboxProfileName::NetworkRestricted,
        SandboxProfileName::WorkspaceBuild,
    ] {
        let policy = SandboxPolicy::from_profile(profile, &root);
        let reason = expect_unsupported(
            executor().plan(&job("true", &[])?, &policy, &root),
            Dimension::Network,
        )?;
        assert!(reason.contains("proxy-only"), "{profile:?}: {reason}");
    }
    Ok(())
}

#[test]
fn network_with_relay_is_proxy_only() -> TestResult {
    let (_dir, root) = workspace()?;
    let executor = executor().with_proxy_relay(relay());
    for (profile, expected) in [
        (SandboxProfileName::NetworkRestricted, S::Partial),
        (SandboxProfileName::WorkspaceBuild, S::Enforced),
    ] {
        let policy = SandboxPolicy::from_profile(profile, &root);
        let plan = executor
            .plan(&job("curl", &["https://example.org"])?, &policy, &root)
            .map_err(ctx("plan"))?;
        assert_eq!(plan.network_mode(), &NetworkMode::ProxyOnly(relay()));
        assert_eq!(plan.report().network, expected, "{profile:?}");
        let args = strings(&plan);
        assert!(has_pair(&args, "--setenv", "ALL_PROXY"), "{profile:?}");
        let tail: Vec<&str> = args
            .iter()
            .skip_while(|arg| arg.as_str() != "--")
            .map(String::as_str)
            .collect();
        assert_eq!(
            tail,
            [
                "--",
                SANDBOX_RELAY_PATH,
                "1080",
                "/run/harw/egress.sock",
                "--",
                "curl",
                "https://example.org"
            ],
            "{profile:?}"
        );
    }
    Ok(())
}

#[test]
fn denied_network_uses_no_network_mode() -> TestResult {
    let (_dir, root) = workspace()?;
    for profile in [
        SandboxProfileName::NoNetwork,
        SandboxProfileName::ReadOnlyAnalysis,
    ] {
        let policy = SandboxPolicy::from_profile(profile, &root);
        // A configured relay is not used when the policy denies network.
        let plan = executor()
            .with_proxy_relay(relay())
            .plan(&job("true", &[])?, &policy, &root)
            .map_err(ctx("plan"))?;
        assert_eq!(plan.network_mode(), &NetworkMode::None, "{profile:?}");
        assert!(!strings(&plan).iter().any(|arg| arg == "ALL_PROXY"));
    }
    Ok(())
}

#[test]
fn report_prediction_is_partial_for_filesystem_and_open_for_resource_limits() -> TestResult {
    let (_dir, root) = workspace()?;
    let policy = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, &root);
    let plan = executor()
        .plan(&job("true", &[])?, &policy, &root)
        .map_err(ctx("plan"))?;
    assert_eq!(
        plan.report(),
        SandboxReport {
            filesystem: S::Partial,
            network: S::Enforced,
            no_new_privs: S::Enforced,
            capabilities: S::Enforced,
            resource_limits: S::NotEnforced,
        }
    );
    assert_eq!(plan.report().overall(), S::Partial);
    assert_eq!(
        plan.report().shortfalls(),
        ["filesystem", "resource_limits"]
    );
    Ok(())
}

#[test]
fn job_env_and_working_dir_are_spliced_before_the_command() -> TestResult {
    let (_dir, root) = workspace()?;
    let policy = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, &root);
    let mut spec = job("make", &["all"])?;
    spec.env = vec![("FOO".to_owned(), "bar".to_owned())];
    spec.working_dir = WorkspacePath::new("sub/dir").map_err(ctx("working dir"))?;
    let plan = executor()
        .plan(&spec, &policy, &root)
        .map_err(ctx("plan"))?;
    let args = strings(&plan);
    let sub = root.join("sub/dir").to_string_lossy().into_owned();
    assert!(has_triple(&args, "--setenv", "FOO", "bar"));
    assert_eq!(args.iter().filter(|arg| *arg == "--chdir").count(), 1);
    let tail_start = args.len().saturating_sub(5);
    assert_eq!(
        args.get(tail_start..),
        Some(
            [
                "--chdir".to_owned(),
                sub,
                "--".to_owned(),
                "make".to_owned(),
                "all".to_owned()
            ]
            .as_slice()
        )
    );
    // The job environment comes after --clearenv.
    let clearenv = args
        .iter()
        .position(|arg| arg == "--clearenv")
        .ok_or(TestError::Missing("--clearenv"))?;
    let foo = args
        .iter()
        .position(|arg| arg == "FOO")
        .ok_or(TestError::Missing("FOO"))?;
    assert!(clearenv < foo);
    Ok(())
}

#[test]
fn extra_read_write_host_path_is_unsupported() -> TestResult {
    let (_dir, root) = workspace()?;
    let policy = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, &root)
        .with_read_write("/var/cache/harw-test");
    expect_unsupported(
        executor().plan(&job("true", &[])?, &policy, &root),
        Dimension::Filesystem,
    )?;
    let policy = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, &root)
        .with_read_write("/usr/local");
    expect_unsupported(
        executor().plan(&job("true", &[])?, &policy, &root),
        Dimension::Filesystem,
    )?;
    Ok(())
}

#[test]
fn writable_subpath_of_read_only_workspace_is_unsupported() -> TestResult {
    let (_dir, root) = workspace()?;
    let policy = SandboxPolicy::from_profile(SandboxProfileName::ReadOnlyAnalysis, &root)
        .with_read_write(root.join("out"));
    expect_unsupported(
        executor().plan(&job("true", &[])?, &policy, &root),
        Dimension::Filesystem,
    )?;
    Ok(())
}

#[test]
fn policy_without_workspace_is_unsupported() -> TestResult {
    let (_dir, root) = workspace()?;
    let mut policy = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, &root);
    policy.filesystem.read_write.retain(|path| path != &root);
    policy.filesystem.exec.retain(|path| path != &root);
    expect_unsupported(
        executor().plan(&job("true", &[])?, &policy, &root),
        Dimension::Filesystem,
    )?;
    Ok(())
}

#[test]
fn extra_read_only_paths_are_bound_and_ancestors_withheld() -> TestResult {
    // Nested workspace, so its parent is not the sandbox-provided /tmp.
    let (_dir, parent) = workspace()?;
    let root = parent.join("ws");
    std::fs::create_dir(&root).map_err(ctx("create workspace"))?;
    let policy = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, &root)
        .with_read_only("/opt/texlive")
        .with_read_only("/opt/texlive")
        .with_read_only(parent.clone());
    let plan = executor()
        .plan(&job("true", &[])?, &policy, &root)
        .map_err(ctx("plan"))?;
    let args = strings(&plan);
    let binds = args
        .windows(3)
        .filter(|window| {
            matches!(window, [a, b, c]
                if a == "--ro-bind-try" && b == "/opt/texlive" && c == "/opt/texlive")
        })
        .count();
    assert_eq!(binds, 1, "bound exactly once");
    let parent_text = parent.to_string_lossy().into_owned();
    assert!(!has_triple(
        &args,
        "--ro-bind-try",
        &parent_text,
        &parent_text
    ));
    assert_eq!(plan.withheld_paths().last(), Some(&parent));
    Ok(())
}

#[test]
fn root_and_relative_policy_paths_are_rejected() -> TestResult {
    let (_dir, root) = workspace()?;
    let policy =
        SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, &root).with_read_only("/");
    expect_unsupported(
        executor().plan(&job("true", &[])?, &policy, &root),
        Dimension::Filesystem,
    )?;
    let policy = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, &root)
        .with_read_only("/opt/../etc/shadow");
    match executor().plan(&job("true", &[])?, &policy, &root) {
        Err(BwrapExecutorError::InvalidPath { .. }) => Ok(()),
        other => Err(TestError::Unexpected(format!("{other:?}"))),
    }
}

#[test]
fn capability_keep_list_is_unsupported_but_empty_keep_is_drop_all() -> TestResult {
    let (_dir, root) = workspace()?;
    let mut policy = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, &root);
    policy.capabilities = CapabilityPolicy::Keep(vec!["CAP_NET_BIND_SERVICE".to_owned()]);
    expect_unsupported(
        executor().plan(&job("true", &[])?, &policy, &root),
        Dimension::Capabilities,
    )?;
    policy.capabilities = CapabilityPolicy::Keep(Vec::new());
    let plan = executor()
        .plan(&job("true", &[])?, &policy, &root)
        .map_err(ctx("plan"))?;
    assert_eq!(plan.report().capabilities, S::Enforced);
    Ok(())
}

#[test]
fn invalid_inputs_are_rejected_before_planning() -> TestResult {
    let (_dir, root) = workspace()?;
    let policy = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, &root);

    match BwrapExecutor::from_executable(PathBuf::from("bwrap")).plan(
        &job("true", &[])?,
        &policy,
        &root,
    ) {
        Err(BwrapExecutorError::RelativeExecutable { .. }) => {}
        other => return Err(TestError::Unexpected(format!("relative: {other:?}"))),
    }

    let mut spec = job("true", &[])?;
    spec.program = String::new();
    match executor().plan(&spec, &policy, &root) {
        Err(BwrapExecutorError::InvalidSpec(_)) => {}
        other => return Err(TestError::Unexpected(format!("empty program: {other:?}"))),
    }

    let missing = root.join("does-not-exist");
    match executor().plan(&job("true", &[])?, &policy, &missing) {
        Err(BwrapExecutorError::Workspace { .. }) => Ok(()),
        other => Err(TestError::Unexpected(format!(
            "missing workspace: {other:?}"
        ))),
    }
}

#[test]
fn into_command_launches_bwrap_with_the_planned_argv() -> TestResult {
    let (_dir, root) = workspace()?;
    let policy = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, &root);
    let plan = executor()
        .plan(&job("echo", &["ok"])?, &policy, &root)
        .map_err(ctx("plan"))?;
    let expected = plan.args().to_vec();
    let command = plan.into_command();
    assert_eq!(command.get_program(), "/usr/bin/bwrap");
    let actual: Vec<_> = command.get_args().map(ToOwned::to_owned).collect();
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn discover_reports_a_missing_backend_as_unsupported() -> TestResult {
    match BwrapExecutor::discover() {
        Ok(executor) => {
            assert!(executor.executable().is_absolute());
            assert!(executor.proxy_relay().is_none());
            Ok(())
        }
        Err(BwrapExecutorError::Unsupported {
            dimension: Dimension::Backend,
            ..
        }) => Ok(()),
        Err(other) => Err(TestError::Unexpected(other.to_string())),
    }
}

// ── bwrap ∘ trampoline (PL-97 E1) ───────────────────────────────────────────

/// Host paths of the trampoline composition, outside every test workspace.
fn mount() -> TrampolineMount {
    TrampolineMount {
        exec_path: PathBuf::from("/opt/harw/bin/harw-job-exec"),
        attempt_dir: PathBuf::from("/var/lib/harw/plans/harw-job-a1.7.0"),
    }
}

/// A workspace that is certainly beneath `/tmp`, which bwrap replaces by
/// its own tmpfs.
fn workspace_in_tmp() -> TestResult<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::Builder::new()
        .prefix("harw-bwrap-tramp-")
        .tempdir_in("/tmp")
        .map_err(ctx("tempdir in /tmp"))?;
    let canonical = dir.path().canonicalize().map_err(ctx("canonicalize"))?;
    Ok((dir, canonical))
}

/// A workspace next to the test binary (normally outside `/tmp`, so a
/// read-only workspace is not beneath the policy's read-write `/tmp`).
fn trampoline_workspace() -> TestResult<(tempfile::TempDir, PathBuf)> {
    let exe = std::env::current_exe().map_err(ctx("current exe"))?;
    let dir = exe
        .parent()
        .ok_or(TestError::Missing("test binary directory"))?;
    let dir = tempfile::Builder::new()
        .prefix("harw-bwrap-ws-")
        .tempdir_in(dir)
        .map_err(ctx("tempdir next to the test binary"))?;
    let canonical = dir.path().canonicalize().map_err(ctx("canonicalize"))?;
    Ok((dir, canonical))
}

/// Whether `profile` cannot be planned with the trampoline at `root`: a
/// read-only workspace beneath `/tmp` (see
/// `trampoline_plan_refuses_shadowed_read_only_binds`), e.g. when the
/// target directory itself lives in `/tmp`.
fn shadowed_in_tmp(profile: SandboxProfileName, root: &Path) -> bool {
    profile == SandboxProfileName::ReadOnlyAnalysis && root.starts_with("/tmp")
}

fn trampoline_plan(
    executor: &BwrapExecutor,
    profile: SandboxProfileName,
    root: &Path,
) -> TestResult<BwrapTrampolinePlan> {
    let policy = SandboxPolicy::from_profile(profile, root);
    executor
        .plan_with_trampoline(&job("true", &[])?, &policy, root, &mount())
        .map_err(ctx("plan with trampoline"))
}

/// Everything after the first `--` separator.
fn after_separator(args: &[String]) -> Vec<&str> {
    args.iter()
        .skip_while(|arg| arg.as_str() != "--")
        .skip(1)
        .map(String::as_str)
        .collect()
}

#[test]
fn trampoline_plan_binds_exec_and_attempt_dir_and_runs_the_inner_trampoline() -> TestResult {
    let (_dir, root) = workspace()?;
    let ws = root.to_string_lossy().into_owned();
    let plan = trampoline_plan(&executor(), SandboxProfileName::NoNetwork, &root)?;
    let args = strings(plan.bwrap());
    // Explicit namespace list, the caller's cgroup namespace is kept.
    let expected_head = [
        "--die-with-parent",
        "--new-session",
        "--unshare-user",
        "--unshare-ipc",
        "--unshare-pid",
        "--unshare-net",
        "--unshare-uts",
    ]
    .map(str::to_owned);
    assert_eq!(args.get(..expected_head.len()), Some(&expected_head[..]));
    assert!(!args.iter().any(|arg| arg == "--unshare-all"), "{args:?}");
    assert!(
        !args.iter().any(|arg| arg.starts_with("--unshare-cgroup")),
        "{args:?}"
    );
    assert!(!args.iter().any(|arg| arg == "--share-net"));
    // Both binds, behind the directories they need.
    assert!(has_triple(
        &args,
        "--ro-bind",
        "/opt/harw/bin/harw-job-exec",
        SANDBOX_TRAMPOLINE_PATH
    ));
    assert!(has_triple(
        &args,
        "--bind",
        "/var/lib/harw/plans/harw-job-a1.7.0",
        SANDBOX_ATTEMPT_DIR
    ));
    assert!(has_pair(&args, "--dir", "/run/harw-job"));
    let position = |needle: &str| args.iter().position(|arg| arg == needle);
    let exec_bind = position(SANDBOX_TRAMPOLINE_PATH).ok_or(TestError::Missing("exec bind"))?;
    let workspace_bind = args
        .windows(3)
        .position(|window| matches!(window, [a, b, c] if a == "--bind" && *b == ws && *c == ws))
        .ok_or(TestError::Missing("workspace bind"))?;
    let chdir = position("--chdir").ok_or(TestError::Missing("--chdir"))?;
    assert!(workspace_bind < exec_bind && exec_bind < chdir, "{args:?}");
    // The sandboxed command is the inner trampoline, not the job.
    assert_eq!(
        after_separator(&args),
        [
            SANDBOX_TRAMPOLINE_PATH,
            "--plan",
            "/run/harw-job/attempt/inner.json",
            "--report",
            "/run/harw-job/attempt/report.json"
        ]
    );
    assert_eq!(
        mount().inner_plan_path(),
        Path::new("/var/lib/harw/plans/harw-job-a1.7.0/inner.json")
    );
    assert_eq!(
        mount().inner_report_path(),
        Path::new("/var/lib/harw/plans/harw-job-a1.7.0/report.json")
    );
    // The namespace layer predicts the same report as the plain plan.
    let plain = executor()
        .plan(
            &job("true", &[])?,
            &SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, &root),
            &root,
        )
        .map_err(ctx("plain plan"))?;
    assert_eq!(plan.bwrap().report(), plain.report());
    assert!(strings(&plain).iter().any(|arg| arg == "--unshare-all"));
    assert_eq!(plan.withheld_paths(), plain.withheld_paths());
    Ok(())
}

#[test]
fn trampoline_plan_keeps_the_relay_prefix_first() -> TestResult {
    let (_dir, root) = workspace()?;
    let executor = executor().with_proxy_relay(relay());
    for profile in [
        SandboxProfileName::WorkspaceBuild,
        SandboxProfileName::NetworkRestricted,
    ] {
        let plan = trampoline_plan(&executor, profile, &root)?;
        let args = strings(plan.bwrap());
        assert_eq!(
            after_separator(&args),
            [
                SANDBOX_RELAY_PATH,
                "1080",
                "/run/harw/egress.sock",
                "--",
                SANDBOX_TRAMPOLINE_PATH,
                "--plan",
                "/run/harw-job/attempt/inner.json",
                "--report",
                "/run/harw-job/attempt/report.json"
            ],
            "{profile:?}"
        );
        // The trampoline binds come after the relay binds.
        let relay_bind = args
            .iter()
            .position(|arg| arg == SANDBOX_RELAY_PATH)
            .ok_or(TestError::Missing("relay bind"))?;
        let exec_bind = args
            .iter()
            .position(|arg| arg == SANDBOX_TRAMPOLINE_PATH)
            .ok_or(TestError::Missing("exec bind"))?;
        assert!(relay_bind < exec_bind, "{profile:?}: {args:?}");
    }
    Ok(())
}

#[test]
fn trampoline_path_map_covers_every_bind_of_the_plan() -> TestResult {
    let (_dir, root) = trampoline_workspace()?;
    let executor = executor().with_proxy_relay(relay());
    for profile in ALL_PROFILES {
        if shadowed_in_tmp(profile, &root) {
            eprintln!(
                "skipping {profile:?}: workspace {} is beneath /tmp",
                root.display()
            );
            continue;
        }
        let plan = trampoline_plan(&executor, profile, &root)?;
        let map = plan.path_map();
        let args = strings(plan.bwrap());
        // Every host bind of the argv is in the map, with its mode.
        let mut binds = 0;
        for window in args.windows(3) {
            let [flag, host, sandbox] = window else {
                continue;
            };
            let mode = match flag.as_str() {
                "--bind" => BindMode::ReadWrite,
                "--ro-bind" | "--ro-bind-try" => BindMode::ReadOnly,
                _ => continue,
            };
            binds += 1;
            assert_eq!(
                map.resolve(Path::new(host)),
                Some((PathBuf::from(sandbox), mode)),
                "{profile:?}: {flag} {host} {sandbox}"
            );
        }
        assert!(binds >= 6, "{profile:?}: {args:?}");
        // bwrap's own mounts are declared as provided by the namespace.
        for (path, mode) in [
            ("/proc", BindMode::ReadOnly),
            ("/dev", BindMode::ReadWrite),
            ("/tmp", BindMode::ReadWrite),
        ] {
            assert_eq!(
                map.resolve(Path::new(path)),
                Some((PathBuf::from(path), mode)),
                "{profile:?}"
            );
        }
        // And nothing else: every map entry is an argv bind or a provided mount.
        for bind in map.binds() {
            let provided = ["/proc", "/dev", "/tmp"]
                .iter()
                .any(|path| bind.host == Path::new(path) && bind.sandbox == bind.host);
            let flag = if bind.mode == BindMode::ReadWrite {
                vec!["--bind"]
            } else {
                vec!["--ro-bind", "--ro-bind-try"]
            };
            let host = bind.host.to_string_lossy();
            let sandbox = bind.sandbox.to_string_lossy();
            let in_argv = flag
                .iter()
                .any(|flag| has_triple(&args, flag, &host, &sandbox));
            assert!(provided || in_argv, "{profile:?}: {bind:?} not in {args:?}");
        }
        // The inner policy is the mapped policy (network aside), and only the
        // withheld paths fall out of it.
        let policy = SandboxPolicy::from_profile(profile, &root);
        assert_eq!(
            plan.inner_policy().filesystem,
            policy.filesystem.mapped(map),
            "{profile:?}"
        );
        assert_eq!(
            plan.unmapped_paths(),
            [PathBuf::from("/run"), PathBuf::from("/sys")],
            "{profile:?}"
        );
        assert_eq!(plan.inner_policy().capabilities, policy.capabilities);
        assert_eq!(plan.inner_policy().no_new_privs, policy.no_new_privs);
        assert_eq!(plan.inner_policy().landlock, policy.landlock);
    }
    Ok(())
}

#[test]
fn trampoline_inner_network_leaves_the_namespace_in_charge() -> TestResult {
    let (_dir, root) = trampoline_workspace()?;
    let executor = executor().with_proxy_relay(relay());
    for (profile, expected) in [
        // Own empty netns: nothing left for Landlock.
        (SandboxProfileName::NoNetwork, NetworkPolicy::Allow),
        (SandboxProfileName::ReadOnlyAnalysis, NetworkPolicy::Allow),
        // Proxy-only egress already narrows "allow".
        (SandboxProfileName::WorkspaceBuild, NetworkPolicy::Allow),
        // A port allowlist behind the relay: connect to the relay only.
        (
            SandboxProfileName::NetworkRestricted,
            NetworkPolicy::ConnectTcpPorts(vec![1080]),
        ),
    ] {
        if shadowed_in_tmp(profile, &root) {
            eprintln!(
                "skipping {profile:?}: workspace {} is beneath /tmp",
                root.display()
            );
            continue;
        }
        let plan = trampoline_plan(&executor, profile, &root)?;
        assert_eq!(plan.inner_policy().network, expected, "{profile:?}");
    }
    Ok(())
}

#[test]
fn trampoline_plan_refuses_shadowed_read_only_binds() -> TestResult {
    // A read-only workspace beneath /tmp: the policy keeps /tmp read-write,
    // so Landlock would not keep the workspace bind read-only.
    let (_dir, root) = workspace_in_tmp()?;
    let policy = SandboxPolicy::from_profile(SandboxProfileName::ReadOnlyAnalysis, &root);
    let reason = match executor().plan_with_trampoline(&job("true", &[])?, &policy, &root, &mount())
    {
        Err(BwrapExecutorError::Unsupported {
            dimension: Dimension::Filesystem,
            reason,
        }) => reason,
        other => {
            return Err(TestError::Unexpected(format!(
                "expected a filesystem refusal, got {other:?}"
            )));
        }
    };
    assert!(reason.contains(root.to_string_lossy().as_ref()), "{reason}");
    // The plain bwrap plan has no Landlock layer and stays valid; so does a
    // writable workspace at the same place.
    assert!(executor().plan(&job("true", &[])?, &policy, &root).is_ok());
    let writable = trampoline_plan(&executor(), SandboxProfileName::NoNetwork, &root)?;
    assert!(
        writable
            .path_map()
            .shadowed_read_only_binds(
                &SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, &root).filesystem
            )
            .is_empty()
    );
    Ok(())
}

#[test]
fn trampoline_mount_inside_the_workspace_or_relative_is_refused() -> TestResult {
    let (_dir, root) = workspace()?;
    let policy = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, &root);
    let spec = job("true", &[])?;
    for inside in [
        TrampolineMount {
            exec_path: root.join("target/debug/harw-job-exec"),
            ..mount()
        },
        TrampolineMount {
            attempt_dir: root.join(".harw/plans/a1"),
            ..mount()
        },
    ] {
        let reason = expect_unsupported_trampoline(
            executor().plan_with_trampoline(&spec, &policy, &root, &inside),
            Dimension::Filesystem,
        )?;
        assert!(reason.contains("overlaps the workspace"), "{reason}");
    }
    // The host root as workspace (the host command port's root) is refused
    // with a clear reason instead of binding the whole host.
    let reason = expect_unsupported_trampoline(
        executor().plan_with_trampoline(
            &spec,
            &SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, Path::new("/")),
            Path::new("/"),
            &mount(),
        ),
        Dimension::Filesystem,
    )?;
    assert!(reason.contains("host root"), "{reason}");
    for relative in [
        TrampolineMount {
            exec_path: PathBuf::from("harw-job-exec"),
            ..mount()
        },
        TrampolineMount {
            attempt_dir: PathBuf::from("/var/lib/../plans"),
            ..mount()
        },
    ] {
        match executor().plan_with_trampoline(&spec, &policy, &root, &relative) {
            Err(BwrapExecutorError::InvalidPath { .. }) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected InvalidPath for {relative:?}, got {other:?}"
                )));
            }
        }
    }
    Ok(())
}

#[test]
fn host_root_workspace_is_refused_by_the_plain_plan_too() -> TestResult {
    // `/` as workspace would make every policy path a workspace subpath and
    // bind the whole host read-write; the plain plan refuses it like the
    // trampoline plan.
    for profile in [
        SandboxProfileName::NoNetwork,
        SandboxProfileName::ReadOnlyAnalysis,
    ] {
        let policy = SandboxPolicy::from_profile(profile, Path::new("/"));
        let reason = expect_unsupported(
            executor().plan(&job("true", &[])?, &policy, Path::new("/")),
            Dimension::Filesystem,
        )?;
        assert!(reason.contains("host root"), "{reason}");
    }
    Ok(())
}

/// Security review finding 4: the job's Landlock view never keeps any
/// access to its report (or write access to anything under
/// `/run/harw-job`), for every profile, with and without proxy-only egress.
#[test]
fn trampoline_report_is_not_writable_in_the_jobs_landlock_view() -> TestResult {
    let (_dir, root) = trampoline_workspace()?;
    let report = Path::new(SANDBOX_ATTEMPT_DIR).join("report.json");
    let cases = [
        (executor(), SandboxProfileName::NoNetwork),
        (executor(), SandboxProfileName::ReadOnlyAnalysis),
    ]
    .into_iter()
    .chain(
        ALL_PROFILES
            .into_iter()
            .map(|profile| (executor().with_proxy_relay(relay()), profile)),
    );
    for (executor, profile) in cases {
        if shadowed_in_tmp(profile, &root) {
            continue;
        }
        let plan = trampoline_plan(&executor, profile, &root)?;
        // The mount itself is read-write (the inner trampoline creates the
        // report there before it restricts itself) ...
        assert_eq!(
            plan.path_map().resolve(&mount().attempt_dir),
            Some((PathBuf::from(SANDBOX_ATTEMPT_DIR), BindMode::ReadWrite)),
            "{profile:?}"
        );
        // ... but no Landlock rule reaches the report: neither the report,
        // nor the attempt directory, nor an ancestor of them.
        let rules = harw_job_linux::sandbox::plan_landlock(plan.inner_policy()).rules;
        let reaching: Vec<&PathBuf> = rules
            .iter()
            .filter(|(path, _)| report.starts_with(path))
            .map(|(path, _)| path)
            .collect();
        assert!(reaching.is_empty(), "{profile:?}: {reaching:?}");
        assert!(
            !plan
                .inner_policy()
                .filesystem
                .read_write
                .iter()
                .any(|path| path.starts_with("/run/harw-job")),
            "{profile:?}"
        );
    }
    Ok(())
}

#[test]
fn inner_policy_writing_the_trampoline_dir_is_refused() -> TestResult {
    use harw_job_linux::FilesystemPolicy;
    for refused in [
        "/",
        "/run",
        "/run/harw-job",
        "/run/harw-job/attempt",
        "/run/harw-job/attempt/report.json",
    ] {
        let filesystem = FilesystemPolicy {
            read_write: vec![PathBuf::from("/work"), PathBuf::from(refused)],
            ..FilesystemPolicy::default()
        };
        match super::refuse_writable_trampoline_dir(&filesystem) {
            Err(BwrapExecutorError::Unsupported {
                dimension: Dimension::Filesystem,
                reason,
            }) => assert!(reason.contains(refused), "{refused}: {reason}"),
            other => {
                return Err(TestError::Unexpected(format!(
                    "{refused}: expected a filesystem refusal, got {other:?}"
                )));
            }
        }
    }
    // Read (and execute) access does not let the job rewrite anything, and
    // siblings of /run/harw-job are not covered.
    let allowed = FilesystemPolicy {
        read_only: vec![PathBuf::from("/run/harw-job/attempt")],
        read_write: vec![
            PathBuf::from("/tmp"),
            PathBuf::from("/run/harw-jobs"),
            PathBuf::from("/work"),
        ],
        exec: vec![PathBuf::from("/run/harw-job/exec")],
    };
    super::refuse_writable_trampoline_dir(&allowed).map_err(ctx("allowed policy"))?;
    Ok(())
}

fn expect_unsupported_trampoline(
    result: Result<BwrapTrampolinePlan, BwrapExecutorError>,
    expected: Dimension,
) -> TestResult<String> {
    match result {
        Err(BwrapExecutorError::Unsupported { dimension, reason }) if dimension == expected => {
            Ok(reason)
        }
        other => Err(TestError::Unexpected(format!(
            "expected Unsupported({expected}), got {other:?}"
        ))),
    }
}
