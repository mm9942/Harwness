//! End-to-end run through the full Bubblewrap ∘ trampoline chain (PL-97 E1):
//! outer `harw-job-exec` → `bwrap` → inner `harw-job-exec` → the job.
//!
//! The test probes the host first and skips itself (passes with a note on
//! stderr) when any link of the chain is missing: a trusted `bwrap`,
//! unprivileged user namespaces, a usable Landlock ABI, a working sandbox,
//! or the `harw-job-exec` binary. The binary is taken from
//! `HARW_JOB_EXEC_BIN`, else from the target directory of this test
//! (`cargo build -p harw-job-exec` puts it there). `HARW_TEST_CGROUP_ROOT`
//! (a delegated cgroup v2 directory) adds the cgroup join and its
//! in-namespace verification. `HARW_REQUIRE_CHAIN_TEST=1` turns a skip into
//! a failure, for CI hosts that are expected to offer the whole chain.
//!
//! Besides the merged report it checks that the job cannot rewrite, remove
//! or replace the inner trampoline's report through the read-write attempt
//! directory (PL-97 security review, finding 4): only Landlock keeps it
//! out, and the backend refuses to run without Landlock (unit tests).

#![cfg(all(target_os = "linux", feature = "bwrap"))]

use std::fmt;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use harw_job_executor_bwrap::BwrapExecutor;
use harw_job_linux::SandboxPolicy;
use harw_job_runtime::{
    Coordinator, CoordinatorConfig, EnforcementState, HostReport, JobResult, JobSpec,
    JobSpecEnvelope, LifecycleState, LinuxExecutor, LinuxExecutorOptions, LinuxSandboxBackend,
    RunnerId, SandboxProfileName, SandboxReport, SandboxRequirement,
};
use harw_job_store::FsJobRecordStore;

/// Upper bound for one job; a hang fails instead of blocking CI.
const LIMIT: Duration = Duration::from_secs(60);

/// Set to `1` to fail instead of skipping when the chain is unavailable.
const REQUIRE_ENV: &str = "HARW_REQUIRE_CHAIN_TEST";

/// Tries to rewrite, truncate, remove, move and replace the report from
/// inside the sandbox. Prints `changed:<op>` for every attempt that worked
/// and `denied:<op>` for every one that did not. The redirections run in
/// subshells: a failed redirection of the special built-in `:` would end
/// the whole script.
const FORGE_REPORT: &str = r#"r=/run/harw-job/attempt/report.json
try() { op=$1; shift; if "$@"; then echo "changed:$op"; else echo "denied:$op"; fi; }
write() { (printf '{"forged":true}' > "$r"); }
truncate_report() { (: > "$r"); }
remove() { rm -f "$r" && ! test -e "$r"; }
try write write
try truncate truncate_report
try remove remove
try move mv "$r" "$r.moved"
try fifo mkfifo /run/harw-job/attempt/fifo
"#;

/// Every operation of [`FORGE_REPORT`].
const FORGE_OPS: [&str; 5] = ["write", "truncate", "remove", "move", "fifo"];

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

/// What the host offers for the chain, or why the test has nothing to
/// verify here.
struct Chain {
    bwrap: BwrapExecutor,
    trampoline: PathBuf,
    landlock_abi: u8,
    cgroup_root: Option<PathBuf>,
}

fn executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// `harw-job-exec` from `HARW_JOB_EXEC_BIN`, else `<target>/<profile>/`.
fn trampoline() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("HARW_JOB_EXEC_BIN").map(PathBuf::from) {
        return executable(&path).then_some(path);
    }
    // <target>/<profile>/deps/<this test> → <target>/<profile>/harw-job-exec
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.parent()?.join("harw-job-exec");
    executable(&candidate).then_some(candidate)
}

fn probe() -> TestResult<Result<Chain, String>> {
    let bwrap = match BwrapExecutor::discover() {
        Ok(bwrap) => bwrap,
        Err(error) => return Ok(Err(error.to_string())),
    };
    let host = HostReport::probe();
    if host.user_namespaces == Some(false) {
        return Ok(Err("unprivileged user namespaces are disabled".to_owned()));
    }
    let Some(landlock_abi) = host.landlock.usable_abi() else {
        return Ok(Err("no usable Landlock ABI".to_owned()));
    };
    if landlock_abi < 5 {
        return Ok(Err(
            "composition requires Landlock ABI 5 or later".to_owned()
        ));
    }
    let Some(trampoline) = trampoline() else {
        return Ok(Err(
            "no harw-job-exec binary (set HARW_JOB_EXEC_BIN or `cargo build -p harw-job-exec`)"
                .to_owned(),
        ));
    };
    // The namespace list of the chain must work on this host.
    let dir = tempfile::tempdir().map_err(ctx("smoke tempdir"))?;
    let policy = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, dir.path());
    let smoke = JobSpec::command("true")
        .build()
        .map_err(ctx("smoke spec"))?;
    let mut command = bwrap
        .plan(&smoke, &policy, dir.path())
        .map_err(ctx("smoke plan"))?
        .into_command();
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(ctx("smoke bwrap"))?;
    if !output.status.success() {
        return Ok(Err(format!(
            "bwrap cannot create a sandbox here: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let cgroup_root = std::env::var_os("HARW_TEST_CGROUP_ROOT").map(PathBuf::from);
    Ok(Ok(Chain {
        bwrap,
        trampoline,
        landlock_abi,
        cgroup_root,
    }))
}

/// A coordinator with the `BwrapTrampoline` backend over temporary
/// directories (workspace, private plan directory, job store).
struct Fixture {
    dir: tempfile::TempDir,
    coordinator: Coordinator<FsJobRecordStore, LinuxExecutor>,
}

impl Fixture {
    fn new(chain: &Chain) -> TestResult<Self> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let workspace = dir.path().join("workspace");
        let plans = dir.path().join("plans");
        std::fs::create_dir(&workspace).map_err(ctx("workspace"))?;
        std::fs::create_dir(&plans).map_err(ctx("plans"))?;
        std::fs::set_permissions(&plans, std::fs::Permissions::from_mode(0o700))
            .map_err(ctx("chmod plans"))?;
        let executor = LinuxExecutor::new(LinuxExecutorOptions {
            sandbox: LinuxSandboxBackend::BwrapTrampoline {
                bwrap: chain.bwrap.clone(),
                trampoline: chain.trampoline.clone(),
                plan_dir: plans,
            },
            cgroup_root: chain.cgroup_root.clone(),
            ..LinuxExecutorOptions::default()
        })
        .map_err(ctx("executor"))?;
        let store =
            FsJobRecordStore::create_ambient(&dir.path().join("store")).map_err(ctx("store"))?;
        let runner = RunnerId::new("bwrap-trampoline-test").map_err(ctx("runner"))?;
        let workspace = workspace
            .canonicalize()
            .map_err(ctx("canonical workspace"))?;
        let coordinator =
            Coordinator::new(store, executor, CoordinatorConfig::new(runner, workspace))
                .map_err(ctx("coordinator"))?;
        Ok(Self { dir, coordinator })
    }

    async fn run(&self, program: &str, args: &[&str]) -> TestResult<JobResult> {
        let spec = JobSpec::command(program)
            .args(args.iter().map(|arg| (*arg).to_owned()))
            .sandbox(SandboxProfileName::NoNetwork)
            .sandbox_requirement(SandboxRequirement::BestEffort)
            .build()
            .map_err(ctx("spec"))?;
        let envelope = JobSpecEnvelope::new(spec).map_err(ctx("envelope"))?;
        let handle = self
            .coordinator
            .submit(envelope)
            .await
            .map_err(ctx("submit"))?;
        tokio::time::timeout(LIMIT, handle.wait())
            .await
            .map_err(ctx("job did not finish in time"))?
            .map_err(ctx("wait"))
    }

    fn plans_left(&self) -> TestResult<usize> {
        Ok(std::fs::read_dir(self.dir.path().join("plans"))
            .map_err(ctx("read plan dir"))?
            .count())
    }
}

#[tokio::test]
async fn bwrap_trampoline_chain_runs_true_and_merges_the_report() -> TestResult {
    let chain = match probe()? {
        Ok(chain) => chain,
        Err(reason) => {
            if std::env::var(REQUIRE_ENV).is_ok_and(|value| value.trim() == "1") {
                return Err(TestError(format!(
                    "{REQUIRE_ENV}=1 but the bwrap ∘ trampoline chain is unavailable: {reason}"
                )));
            }
            eprintln!("skipping the bwrap ∘ trampoline chain: {reason}");
            return Ok(());
        }
    };
    let fixture = Fixture::new(&chain)?;

    let result = fixture.run("true", &[]).await?;
    assert_eq!(
        result.state,
        LifecycleState::Succeeded,
        "{result:?}\nstderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    // bwrap's namespace (network, mounts) merged with the inner layer
    // (Landlock separating read from execute, NO_NEW_PRIVS, capabilities).
    let filesystem = if chain.landlock_abi >= 5 {
        EnforcementState::Enforced
    } else {
        EnforcementState::Partial
    };
    let genuine = SandboxReport {
        filesystem,
        ..SandboxReport::uniform(EnforcementState::Enforced)
    };
    assert_eq!(result.sandbox, Some(genuine), "{result:?}");
    assert_eq!(fixture.plans_left()?, 0, "the attempt directory is removed");

    // The job's view of its report: bound read-write, but Landlock denies
    // every change, so the merged report is the genuine one.
    let result = fixture.run("/bin/sh", &["-c", FORGE_REPORT]).await?;
    let stdout = String::from_utf8_lossy(&result.stdout);
    let outcome: Vec<&str> = stdout.lines().map(str::trim).collect();
    let expected: Vec<String> = FORGE_OPS.iter().map(|op| format!("denied:{op}")).collect();
    assert_eq!(
        outcome,
        expected,
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.state, LifecycleState::Succeeded, "{result:?}");
    assert_eq!(result.sandbox, Some(genuine), "{result:?}");
    assert_eq!(fixture.plans_left()?, 0, "the attempt directory is removed");

    // The distinction bwrap alone cannot make: /tmp is writable but not
    // executable under the inner Landlock layer.
    let result = fixture
        .run("/bin/sh", &["-c", "cp /bin/true /tmp/t && /tmp/t"])
        .await?;
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.state, LifecycleState::Failed, "{result:?}");
    assert!(stderr.contains("Permission denied"), "{stderr}");

    if chain.cgroup_root.is_some() {
        // Born inside the job cgroup, and the inner stage verified it.
        let result = fixture.run("cat", &["/proc/self/cgroup"]).await?;
        assert_eq!(result.state, LifecycleState::Succeeded, "{result:?}");
        let cgroup = String::from_utf8_lossy(&result.stdout);
        assert!(cgroup.contains("/harw-job-"), "{cgroup}");
    }
    Ok(())
}
