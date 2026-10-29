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
use harw_job_exec::plan::ProcessCountGuard;
use harw_job_exec::report::{ProcessIdentity, nproc_exemption};
use harw_job_exec::{
    CgroupVerify, ExecPlanV1, PROC_SELF_CGROUP, TrampolineCommand, exit_code, unified_cgroup_path,
};
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

/// `RLIMIT_NPROC` (the job runtime's `process_count_max`) reaches the job:
/// the trampoline sets it before `exec`, soft = hard. The value never
/// exceeds the current hard limit and leaves room for the user's other
/// processes (the limit counts per real UID).
#[test]
fn test_process_count_rlimit_reaches_the_job() -> TestResult {
    let current = harw_job_linux::resources::current_rlimit(RlimitResource::Nproc);
    let count = current.hard.map_or(1 << 20, |hard| hard.min(1 << 20));
    let plan = ExecPlanV1::new("/bin/cat")
        .with_args(["/proc/self/limits"])
        .with_rlimits(RlimitSet::new().with(RlimitResource::Nproc, RlimitValue::fixed(count)));
    let run = run(&plan, &[])?;
    assert_eq!(run.code(), Some(0), "stderr: {}", run.stderr());
    let limits = String::from_utf8_lossy(&run.output.stdout);
    let Some(line) = limits
        .lines()
        .find(|line| line.starts_with("Max processes"))
    else {
        return fail(format!("no process limit in {limits}"));
    };
    let values: Vec<&str> = line.split_whitespace().skip(2).take(2).collect();
    let expected = count.to_string();
    assert_eq!(values, [expected.as_str(), expected.as_str()], "{line}");
    // PL-97 E2g: the limit is set either way, but it only binds an
    // identity the kernel does not exempt (the unsandboxed trampoline has
    // the test runner's identity).
    let expected = if nproc_exempt() {
        EnforcementState::NotEnforced
    } else {
        EnforcementState::Enforced
    };
    assert_eq!(
        run.report.map(|report| report.resource_limits),
        Some(expected)
    );
    Ok(())
}

/// Whether the test runner's identity (and so an unsandboxed
/// trampoline's) is exempt from `RLIMIT_NPROC`.
fn nproc_exempt() -> bool {
    nproc_exemption(ProcessIdentity::current()).is_some()
}

/// PL-97 E2g: a job that requires its process count cap is refused (exit
/// `SANDBOX_REFUSED`, the report written first) when the identity that
/// would exec it is exempt from `RLIMIT_NPROC` and no job cgroup covers
/// the request; an unprivileged identity runs it. Which branch runs
/// depends on the uid of the test runner (the decision itself is covered
/// for every identity by the unit tests with an injected identity).
#[test]
fn test_required_process_count_is_refused_for_an_exempt_identity() -> TestResult {
    let current = harw_job_linux::resources::current_rlimit(RlimitResource::Nproc);
    let count = current.hard.map_or(1 << 20, |hard| hard.min(1 << 20));
    let plan = sh("echo ran")
        .with_rlimits(RlimitSet::new().with(RlimitResource::Nproc, RlimitValue::fixed(count)))
        .with_process_count(ProcessCountGuard {
            required: true,
            cgroup_pids_cover: false,
        });
    let run = run(&plan, &[])?;
    let resource_limits = run.report.map(|report| report.resource_limits);
    if nproc_exempt() {
        assert_eq!(
            run.code(),
            Some(i32::from(exit_code::SANDBOX_REFUSED)),
            "stderr: {}",
            run.stderr()
        );
        assert!(run.stderr().contains("RLIMIT_NPROC"), "{}", run.stderr());
        assert!(run.output.stdout.is_empty(), "the job must not run");
        assert_eq!(resource_limits, Some(EnforcementState::NotEnforced));
    } else {
        assert_eq!(run.code(), Some(0), "stderr: {}", run.stderr());
        assert_eq!(String::from_utf8_lossy(&run.output.stdout), "ran\n");
        assert_eq!(resource_limits, Some(EnforcementState::Enforced));
    }
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
fn test_hard_filesystem_policy_protects_report_for_best_effort_job() -> TestResult {
    for requirement in [SandboxRequirement::None, SandboxRequirement::BestEffort] {
        let workspace = scratch()?;
        let plans = scratch()?;
        let marker = workspace.path().join("ran");
        let mut policy =
            SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, workspace.path())
                .hard_requirement();
        // Namespace networking is handled by bwrap in the composition.
        policy.network = harw_job_linux::NetworkPolicy::Allow;
        let plan = sh(r#"printf ran > "$MARKER"; printf forged > "$REPORT""#)
            .with_sandbox(policy, requirement);
        let mut launch = TrampolineCommand::new(Path::new(TRAMPOLINE), &plan, plans.path())
            .map_err(ctx("prepare protected report"))?;
        let report_path = launch.report_path().to_path_buf();
        let output = launch
            .command_mut()
            .env("MARKER", &marker)
            .env("REPORT", &report_path)
            .stdin(Stdio::null())
            .output()
            .map_err(ctx("spawn protected report"))?;
        if marker.exists() {
            let report = launch
                .read_report()
                .map_err(ctx("read protected report"))?
                .ok_or_else(|| TestError("job ran without a trustworthy report".to_owned()))?;
            assert_eq!(report.filesystem, EnforcementState::Enforced);
            assert!(!output.status.success(), "job must not rewrite its report");
        } else {
            assert_eq!(
                output.status.code(),
                Some(i32::from(exit_code::SANDBOX_REFUSED)),
                "unsupported filesystem must refuse before exec: {}",
                String::from_utf8_lossy(&output.stderr)
            );
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

/// The unified-hierarchy cgroup of the test process, which a spawned
/// trampoline inherits, if a plan may name it: `None` on a cgroup v1-only
/// host and at the root (`0::/`, e.g. a cgroup namespace root), which no
/// valid plan can expect.
fn own_job_like_cgroup() -> TestResult<Option<PathBuf>> {
    let content = std::fs::read_to_string(PROC_SELF_CGROUP).map_err(ctx("read own cgroup"))?;
    Ok(unified_cgroup_path(&content)
        .filter(|own| *own != "/")
        .map(PathBuf::from))
}

/// A `Nofile` limit `setrlimit` refuses for every caller, root included:
/// above `fs.nr_open`.
fn refused_nofile_limit() -> TestResult<RlimitSet> {
    let nr_open: u64 = std::fs::read_to_string("/proc/sys/fs/nr_open")
        .map_err(ctx("read fs.nr_open"))?
        .trim()
        .parse()
        .map_err(ctx("parse fs.nr_open"))?;
    Ok(RlimitSet::new().with(
        RlimitResource::Nofile,
        RlimitValue::fixed(nr_open.saturating_add(1)),
    ))
}

fn mismatched_cgroup() -> CgroupVerify {
    CgroupVerify {
        expected_proc_path: PathBuf::from("/harw-job-exec-test/not-this-cgroup"),
        limits: Some(EnforcementState::Enforced),
    }
}

#[test]
fn test_verified_cgroup_membership_runs_the_job() -> TestResult {
    let Some(own) = own_job_like_cgroup()? else {
        // Skipped, not passed: nothing a plan may expect on this host. The
        // mismatch tests below still cover the refusal.
        eprintln!("skipped: the test process is not in a verifiable job cgroup");
        return Ok(());
    };
    let marker_dir = scratch()?;
    let marker = marker_dir.path().join("ran");
    let plan = sh(r#"touch "$MARKER""#).with_cgroup(CgroupVerify {
        expected_proc_path: own,
        limits: Some(EnforcementState::Enforced),
    });
    let run = run(&plan, &[("MARKER", marker.as_path())])?;
    assert_eq!(run.code(), Some(0), "stderr: {}", run.stderr());
    assert!(marker.exists());
    assert_eq!(
        run.report.map(|report| report.resource_limits),
        Some(EnforcementState::Enforced)
    );
    Ok(())
}

#[test]
fn test_cgroup_mismatch_refuses_before_exec_and_records_the_dimension() -> TestResult {
    let workspace = scratch()?;
    let marker_dir = scratch()?;
    let marker = marker_dir.path().join("ran");
    let policy = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, workspace.path())
        .with_read_write(marker_dir.path());
    let plan = sh(r#"touch "$MARKER""#)
        .with_cgroup(mismatched_cgroup())
        .with_sandbox(policy, SandboxRequirement::BestEffort);
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
        stderr.contains("cgroup membership not verified")
            && stderr.contains("/harw-job-exec-test/not-this-cgroup"),
        "{stderr}"
    );
    let Some(report) = run.report else {
        return fail(format!("no report; stderr: {stderr}"));
    };
    // The refused dimension is recorded; the other controls still ran, so
    // the report names exactly what fell short.
    assert_eq!(report.resource_limits, EnforcementState::NotEnforced);
    assert_eq!(report.no_new_privs, EnforcementState::Enforced);
    assert_eq!(report.capabilities, EnforcementState::Enforced);
    Ok(())
}

#[test]
fn test_cgroup_mismatch_outranks_a_later_rlimit_failure() -> TestResult {
    let marker_dir = scratch()?;
    let marker = marker_dir.path().join("ran");
    let rlimits = refused_nofile_limit()?;

    // The rlimit alone really fails (125), so the next run proves the
    // precedence, not a lucky setrlimit.
    let alone = run(
        &sh(r#"touch "$MARKER""#).with_rlimits(rlimits.clone()),
        &[("MARKER", marker.as_path())],
    )?;
    assert_eq!(
        alone.code(),
        Some(i32::from(exit_code::SETUP_FAILED)),
        "stderr: {}",
        alone.stderr()
    );
    assert!(
        alone.stderr().contains("rlimits failed"),
        "{}",
        alone.stderr()
    );

    let plan = sh(r#"touch "$MARKER""#)
        .with_cgroup(mismatched_cgroup())
        .with_rlimits(rlimits);
    let run = run(&plan, &[("MARKER", marker.as_path())])?;
    let stderr = run.stderr();
    assert_eq!(
        run.code(),
        Some(i32::from(exit_code::SANDBOX_REFUSED)),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("cgroup membership not verified") && !stderr.contains("rlimits failed"),
        "{stderr}"
    );
    assert!(!marker.exists(), "the job must never run");
    Ok(())
}

#[test]
fn test_cgroup_mismatch_with_rlimits_reports_partial_limits() -> TestResult {
    let marker_dir = scratch()?;
    let marker = marker_dir.path().join("ran");
    let plan = sh(r#"touch "$MARKER""#)
        .with_cgroup(mismatched_cgroup())
        .with_rlimits(RlimitSet::new().with(RlimitResource::Nofile, RlimitValue::fixed(64)));
    let run = run(&plan, &[("MARKER", marker.as_path())])?;
    assert_eq!(
        run.code(),
        Some(i32::from(exit_code::SANDBOX_REFUSED)),
        "stderr: {}",
        run.stderr()
    );
    assert!(!marker.exists(), "the job must never run");
    // The rlimits hold, the cgroup limits do not: Partial, not Enforced.
    assert_eq!(
        run.report.map(|report| report.resource_limits),
        Some(EnforcementState::Partial)
    );
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
