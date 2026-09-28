//! End-to-end run through a real `bwrap`.
//!
//! Skips itself (passes with a note on stderr) when no trusted `bwrap` is
//! installed or when the host cannot create the sandbox (e.g. unprivileged
//! user namespaces disabled): the test then has nothing to verify.

#![cfg(target_os = "linux")]

use std::fmt;
use std::process::{Output, Stdio};

use harw_job_core::{
    JobScopeId, JobSpec, ResourceRequest, SandboxProfileName, SandboxRequirement, WorkspacePath,
};
use harw_job_executor_bwrap::{BwrapExecutor, BwrapJobPlan};
use harw_job_linux::SandboxPolicy;

/// Test failure (no `unwrap`/`expect`/`panic!` in tests).
struct TestError(String);

impl fmt::Debug for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

type TestResult<T = ()> = Result<T, TestError>;

fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |error| TestError(format!("{context}: {error}"))
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

fn run(plan: BwrapJobPlan) -> TestResult<Output> {
    let mut command = plan.into_command();
    command.stdin(Stdio::null());
    command.output().map_err(ctx("spawn bwrap"))
}

#[test]
fn echo_runs_and_system_paths_stay_read_only() -> TestResult {
    let executor = match BwrapExecutor::discover() {
        Ok(executor) => executor,
        Err(error) => {
            eprintln!("skipping: {error}");
            return Ok(());
        }
    };
    let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let policy = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, dir.path());

    let echo = executor
        .plan(&job("echo", &["ok"])?, &policy, dir.path())
        .map_err(ctx("plan echo"))?;
    let output = run(echo)?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() && stderr.contains("bwrap:") {
        eprintln!("skipping: bwrap cannot create a sandbox on this host: {stderr}");
        return Ok(());
    }
    assert!(output.status.success(), "echo failed: {stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok");

    let touch = executor
        .plan(&job("sh", &["-c", "touch /etc/x"])?, &policy, dir.path())
        .map_err(ctx("plan touch"))?;
    let output = run(touch)?;
    assert!(
        !output.status.success(),
        "writing /etc must fail inside the sandbox"
    );

    let write = executor
        .plan(
            &job("sh", &["-c", "echo hi > out.txt"])?,
            &policy,
            dir.path(),
        )
        .map_err(ctx("plan workspace write"))?;
    let output = run(write)?;
    assert!(
        output.status.success(),
        "workspace write failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let written =
        std::fs::read_to_string(dir.path().join("out.txt")).map_err(ctx("read out.txt"))?;
    assert_eq!(written.trim(), "hi");

    let read_only = SandboxPolicy::from_profile(SandboxProfileName::ReadOnlyAnalysis, dir.path());
    let denied = executor
        .plan(
            &job("sh", &["-c", "echo no > denied.txt"])?,
            &read_only,
            dir.path(),
        )
        .map_err(ctx("plan read-only write"))?;
    let output = run(denied)?;
    assert!(!output.status.success(), "read-only workspace was writable");
    assert!(!dir.path().join("denied.txt").exists());
    Ok(())
}
