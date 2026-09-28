//! End-to-end tests of the `harw-job-exec` binary: each test spawns the real
//! trampoline (`CARGO_BIN_EXE_harw-job-exec`), so every restriction lands in
//! a child process, never in the test runner.
//!
//! Landlock support differs between kernels (and container runtimes). The
//! sandbox tests therefore assert *consistency* between the report and the
//! observed behaviour: a write outside the allowed paths fails exactly when
//! the report says the filesystem dimension is (at least partially)
//! enforced.

#![cfg(target_os = "linux")]

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use harw_job_core::{EnforcementState, SandboxProfileName, SandboxReport, SandboxRequirement};
use harw_job_exec::{ExecPlanV1, TrampolineCommand, exit_code};
use harw_job_linux::{RlimitResource, RlimitSet, RlimitValue, SandboxPolicy};

const TRAMPOLINE: &str = env!("CARGO_BIN_EXE_harw-job-exec");

/// Test failure (the crate-private `TestError` is not reachable from an
/// integration test).
#[derive(Debug)]
struct TestError(String);

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TestError {}

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> Box<dyn std::error::Error> {
    move |error| Box::new(TestError(format!("{context}: {error}")))
}

fn fail<T>(message: String) -> TestResult<T> {
    Err(Box::new(TestError(message)))
}

/// A private scratch directory **outside** `/tmp` (every sandbox profile
/// may write `/tmp`, so a workspace there would not show a denial).
fn scratch() -> TestResult<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("harw-job-exec-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .map_err(ctx("scratch dir"))
}

struct Run {
    output: Output,
    report: Option<SandboxReport>,
}

impl Run {
    fn code(&self) -> Option<i32> {
        self.output.status.code()
    }

    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).into_owned()
    }
}

fn run(plan: &ExecPlanV1, env: &[(&str, &Path)]) -> TestResult<Run> {
    let dir = scratch()?;
    let mut launch =
        TrampolineCommand::new(Path::new(TRAMPOLINE), plan, dir.path()).map_err(ctx("prepare"))?;
    for (key, value) in env {
        launch.command_mut().env(key, value);
    }
    let output = launch
        .command_mut()
        .stdin(Stdio::null())
        .output()
        .map_err(ctx("spawn trampoline"))?;
    if launch.plan_path().exists() {
        return fail(format!(
            "plan file {} not consumed",
            launch.plan_path().display()
        ));
    }
    let report = launch.read_report().map_err(ctx("read report"))?;
    Ok(Run { output, report })
}

fn sh(script: &str) -> ExecPlanV1 {
    ExecPlanV1::new("/bin/sh").with_args(["-c", script])
}

#[test]
fn test_job_exit_status_passes_through_and_report_is_written() -> TestResult {
    let run = run(&sh("exit 7"), &[])?;
    assert_eq!(run.code(), Some(7), "stderr: {}", run.stderr());
    let Some(report) = run.report else {
        return fail("no report written".to_owned());
    };
    assert_eq!(report.filesystem, EnforcementState::NotEnforced);
    assert_eq!(report.no_new_privs, EnforcementState::NotEnforced);
    // Nothing requested → nothing missing.
    assert_eq!(report.resource_limits, EnforcementState::Enforced);
    Ok(())
}

#[test]
fn test_rlimits_reach_the_job() -> TestResult {
    let plan = sh("ulimit -n")
        .with_rlimits(RlimitSet::new().with(RlimitResource::Nofile, RlimitValue::fixed(64)));
    let run = run(&plan, &[])?;
    assert_eq!(run.code(), Some(0), "stderr: {}", run.stderr());
    assert_eq!(String::from_utf8_lossy(&run.output.stdout).trim(), "64");
    assert_eq!(
        run.report.map(|report| report.resource_limits),
        Some(EnforcementState::Enforced)
    );
    Ok(())
}

#[test]
fn test_read_only_analysis_denies_workspace_writes_when_landlock_enforces() -> TestResult {
    let workspace = scratch()?;
    let forbidden = workspace.path().join("forbidden");
    let policy =
        SandboxPolicy::from_profile(SandboxProfileName::ReadOnlyAnalysis, workspace.path());
    let plan =
        sh(r#"echo x > "$WORKDIR/forbidden""#).with_sandbox(policy, SandboxRequirement::BestEffort);
    let run = run(&plan, &[("WORKDIR", workspace.path())])?;
    let Some(report) = run.report else {
        return fail(format!("no report; stderr: {}", run.stderr()));
    };

    // NO_NEW_PRIVS needs no privilege and no Landlock: always enforced, and
    // with it the capability drop counts as enforced.
    assert_eq!(report.no_new_privs, EnforcementState::Enforced);
    assert_eq!(report.capabilities, EnforcementState::Enforced);

    let wrote = forbidden.exists();
    let succeeded = run.output.status.success();
    match report.filesystem {
        EnforcementState::Enforced | EnforcementState::Partial => {
            assert!(
                !wrote,
                "Landlock reported {} but the write succeeded",
                report.filesystem
            );
            assert!(!succeeded, "shell exit must reflect the denied redirect");
            assert_ne!(
                report.network,
                EnforcementState::Enforced,
                "TCP-only is never full"
            );
        }
        EnforcementState::NotEnforced | EnforcementState::Unsupported => {
            // No Landlock on this kernel: the write goes through, and the
            // report says so honestly.
            assert_eq!(succeeded, wrote, "stderr: {}", run.stderr());
            assert!(wrote, "without Landlock the write is allowed");
        }
    }
    Ok(())
}

#[test]
fn test_required_requirement_refuses_before_exec() -> TestResult {
    let workspace = scratch()?;
    let marker_dir = scratch()?;
    let marker = marker_dir.path().join("ran");
    let base = SandboxPolicy::from_profile(SandboxProfileName::ReadOnlyAnalysis, workspace.path())
        .with_read_write(marker_dir.path());
    // Best-effort Landlock: the report is produced and falls short (network
    // is TCP-only at best, or everything is unsupported) → requirement
    // check refuses. Hard-requirement Landlock: either the same, or the
    // sandbox step itself refuses. Both must end in 126 without the job.
    for policy in [base.clone(), base.hard_requirement()] {
        let plan = sh(r#"touch "$MARKER""#).with_sandbox(policy, SandboxRequirement::Required);
        let run = run(&plan, &[("MARKER", marker.as_path())])?;
        assert_eq!(
            run.code(),
            Some(i32::from(exit_code::SANDBOX_REFUSED)),
            "stderr: {}",
            run.stderr()
        );
        assert!(!marker.exists(), "the job must never run");
        let stderr = run.stderr();
        assert!(
            stderr.contains("requirement") || stderr.contains("sandbox refused"),
            "{stderr}"
        );
        if let Some(report) = run.report {
            assert!(
                !report.satisfies(SandboxRequirement::Required),
                "{report:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn test_best_effort_sandbox_still_runs_the_job() -> TestResult {
    let workspace = scratch()?;
    let marker_dir = scratch()?;
    let marker = marker_dir.path().join("ran");
    let policy = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, workspace.path())
        .with_read_write(marker_dir.path());
    let plan = sh(r#"touch "$MARKER""#).with_sandbox(policy, SandboxRequirement::BestEffort);
    let run = run(&plan, &[("MARKER", marker.as_path())])?;
    assert_eq!(run.code(), Some(0), "stderr: {}", run.stderr());
    assert!(marker.exists());
    Ok(())
}

#[test]
fn test_exec_failure_exits_127_after_report() -> TestResult {
    let run = run(
        &ExecPlanV1::new("/nonexistent/harw-job-exec-test-program"),
        &[],
    )?;
    assert_eq!(run.code(), Some(i32::from(exit_code::EXEC_FAILED)));
    assert!(run.stderr().contains("exec"), "{}", run.stderr());
    assert!(run.report.is_some(), "the report precedes exec");
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8], mode: u32) -> TestResult {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(ctx("create plan"))?;
    file.write_all(bytes).map_err(ctx("write plan"))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(ctx("chmod"))
}

fn raw_trampoline(args: &[&Path]) -> TestResult<Output> {
    let mut command = Command::new(TRAMPOLINE);
    for (index, arg) in args.iter().enumerate() {
        command
            .arg(if index == 0 { "--plan" } else { "--report" })
            .arg(arg);
    }
    command.output().map_err(ctx("spawn trampoline"))
}

#[test]
fn test_setup_failures_exit_125() -> TestResult {
    let setup = Some(i32::from(exit_code::SETUP_FAILED));
    let no_args = raw_trampoline(&[])?;
    assert_eq!(no_args.status.code(), setup);
    assert!(String::from_utf8_lossy(&no_args.stderr).contains("usage"));

    let dir = scratch()?;
    let v2: PathBuf = dir.path().join("v2.json");
    write_private(&v2, br#"{"version":2,"program":"/bin/true"}"#, 0o600)?;
    let output = raw_trampoline(&[v2.as_path()])?;
    assert_eq!(output.status.code(), setup);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unsupported plan version 2"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!v2.exists(), "plan removed even when rejected");

    let json = sh("exit 0").to_json().map_err(ctx("json"))?;
    let shared = dir.path().join("shared.json");
    write_private(&shared, &json, 0o644)?;
    let output = raw_trampoline(&[shared.as_path()])?;
    assert_eq!(output.status.code(), setup);
    assert!(String::from_utf8_lossy(&output.stderr).contains("rejected"));

    // An existing report file is never reused.
    let plan = dir.path().join("plan.json");
    write_private(&plan, &json, 0o600)?;
    let report = dir.path().join("report.json");
    write_private(&report, b"", 0o600)?;
    let output = raw_trampoline(&[plan.as_path(), report.as_path()])?;
    assert_eq!(output.status.code(), setup);
    Ok(())
}
