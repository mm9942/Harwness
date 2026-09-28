//! Pure mapping tests: no process is started, only argv and report are
//! inspected.

use std::path::{Path, PathBuf};

use harw_job_core::{
    EnforcementState as S, JobScopeId, JobSpec, ResourceRequest, SandboxProfileName, SandboxReport,
    SandboxRequirement, WorkspacePath,
};
use harw_job_linux::{CapabilityPolicy, SandboxPolicy};
use harw_sandbox::{NetworkMode, RelaySpec, SANDBOX_RELAY_PATH};

use super::{BwrapExecutor, BwrapJobPlan};
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
