//! Coordinator scenarios on Linux (Job-Runtime-Doc §21.2, §21.4).
//!
//! Every test runs real processes (`/bin/sh`, `sleep`) against a
//! file-backed store in a temporary directory. cgroup-dependent tests are
//! `#[ignore]`d (they need a delegated cgroup v2 root).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use harw_job_core::{
    AttemptId, CancellationCause, EnforcementState, ExitOutcome, JobClaim, JobOutcome, JobSpec,
    JobSpecEnvelope, JobState, LifecycleEvent, LifecycleState, LifecycleTransition,
    ResourceRequest, RetryPolicy, RunnerId, SandboxRequirement,
};
use harw_job_linux::cgroup::{CgroupBackend, CgroupV2Fs};
use harw_job_linux::{CgroupError, LinuxProcess, LinuxRecoveryIdentity, SignalKind};
use harw_job_store::{
    ClaimTerms, FsJobRecordStore, JobRecordStore, JobTransition, LEASE_EXPIRED_EXHAUSTED_REASON,
};
use harw_types::WorkId;
use jiff::{SignedDuration, Timestamp};
use tempfile::TempDir;

use super::attempt::{AttemptRecord, attempt_id_for};
use super::capture::OutputCapture;
use super::executor::AttemptContext;
use super::linux::{LinuxExecutor, LinuxExecutorOptions};
use super::runner::{
    Coordinator, CoordinatorConfig, JobHandle, JobResult, RecoveryDecision, retry_attempt_id,
};
use super::store::CoordinatorStore;
use crate::test_support::{TestError, TestResult, ctx};

/// Upper bound for any single wait; a hang fails instead of blocking CI.
const LIMIT: Duration = Duration::from_secs(30);

type LinuxCoordinator = Coordinator<FsJobRecordStore, LinuxExecutor>;

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::create_dir(dir.path().join("workspace")).map_err(ctx("workspace dir"))?;
        std::fs::create_dir(dir.path().join("status")).map_err(ctx("status dir"))?;
        Ok(Self { dir })
    }

    fn workspace(&self) -> PathBuf {
        self.dir.path().join("workspace")
    }

    fn status_dir(&self) -> PathBuf {
        self.dir.path().join("status")
    }

    fn store(&self) -> TestResult<FsJobRecordStore> {
        FsJobRecordStore::create_ambient(&self.dir.path().join("store")).map_err(ctx("store"))
    }

    fn coordinator(
        &self,
        runner: &RunnerId,
        options: LinuxExecutorOptions,
        ttl: Duration,
    ) -> TestResult<LinuxCoordinator> {
        let executor = LinuxExecutor::new(options).map_err(ctx("executor"))?;
        let config = CoordinatorConfig::new(runner.clone(), self.workspace()).with_lease_ttl(ttl);
        Coordinator::new(self.store()?, executor, config).map_err(ctx("coordinator"))
    }

    /// A coordinator whose jobs carry `retry`.
    fn retrying_coordinator(
        &self,
        runner: &RunnerId,
        retry: RetryPolicy,
    ) -> TestResult<LinuxCoordinator> {
        let executor =
            LinuxExecutor::new(LinuxExecutorOptions::default()).map_err(ctx("executor"))?;
        let config = CoordinatorConfig::new(runner.clone(), self.workspace())
            .with_lease_ttl(Duration::from_secs(3))
            .with_retry(retry);
        Coordinator::new(self.store()?, executor, config).map_err(ctx("coordinator"))
    }

    fn shim_options(&self) -> LinuxExecutorOptions {
        LinuxExecutorOptions {
            exit_status_dir: Some(self.status_dir()),
            ..LinuxExecutorOptions::default()
        }
    }
}

fn runner(name: &str) -> TestResult<RunnerId> {
    RunnerId::new(name).map_err(ctx("runner id"))
}

fn sh_spec(script: &str) -> TestResult<JobSpec> {
    JobSpec::command("/bin/sh")
        .arg("-c")
        .arg(script)
        .build()
        .map_err(ctx("spec"))
}

fn envelope(spec: JobSpec) -> TestResult<JobSpecEnvelope> {
    JobSpecEnvelope::new(spec).map_err(ctx("envelope"))
}

fn sh(script: &str) -> TestResult<JobSpecEnvelope> {
    envelope(sh_spec(script)?)
}

async fn wait(handle: JobHandle) -> TestResult<JobResult> {
    tokio::time::timeout(LIMIT, handle.wait())
        .await
        .map_err(ctx("job did not finish in time"))?
        .map_err(ctx("job ended with an error"))
}

fn read_attempt(
    store: &FsJobRecordStore,
    attempt: &str,
) -> TestResult<Option<AttemptRecord<LinuxRecoveryIdentity>>> {
    let Some(bytes) = store.read_attempt(attempt).map_err(ctx("read attempt"))? else {
        return Ok(None);
    };
    AttemptRecord::from_bytes(attempt, &bytes)
        .map(Some)
        .map_err(ctx("decode attempt"))
}

/// Polls the attempt sidecar until it is `Running`.
async fn wait_for_running(store: &FsJobRecordStore, attempt: &str) -> TestResult {
    let started = std::time::Instant::now();
    while started.elapsed() < LIMIT {
        if let Some(record) = read_attempt(store, attempt)? {
            if record.state == LifecycleState::Running {
                return Ok(());
            }
            if record.is_terminal() {
                return Err(TestError::Unexpected(format!(
                    "attempt ended early as {} ({:?})",
                    record.state, record.reason
                )));
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err(TestError::Unexpected(
        "attempt never reached running".into(),
    ))
}

fn job_state(store: &FsJobRecordStore, job: &WorkId) -> TestResult<JobState> {
    Ok(store.load(job).map_err(ctx("load job"))?.job.state)
}

/// Plants a `Running` job held by `runner` whose attempt sidecar carries
/// `identity` (as a crashed coordinator would have left it).
fn plant_running_attempt(
    coordinator: &LinuxCoordinator,
    runner: &RunnerId,
    identity: impl FnOnce(&AttemptContext) -> TestResult<LinuxRecoveryIdentity>,
) -> TestResult<AttemptContext> {
    let store = coordinator.store();
    let now = Timestamp::now();
    let record = coordinator
        .new_record(&sh("sleep 30")?, now)
        .map_err(ctx("new record"))?;
    let job_id = record.job.id.clone();
    store.create_job(record).map_err(ctx("create job"))?;
    let claim = store
        .claim_job(
            &job_id,
            runner,
            ClaimTerms {
                lease_ttl: SignedDuration::from_secs(60),
                now,
            },
        )
        .map_err(ctx("claim"))?;
    let attempt_id = attempt_id_for(&job_id, claim.lease.epoch).map_err(ctx("attempt id"))?;
    let context = AttemptContext {
        job_id: job_id.clone(),
        attempt_id: attempt_id.clone(),
        runner_id: runner.clone(),
        lease_epoch: claim.lease.epoch,
        workspace_root: coordinator.config().workspace_root.clone(),
    };
    let mut attempt = AttemptRecord::claimed(
        job_id,
        attempt_id.clone(),
        runner.clone(),
        claim.lease.epoch,
        now,
    )
    .map_err(ctx("attempt"))?;
    attempt
        .apply(LifecycleEvent::Start, now)
        .map_err(ctx("start"))?;
    attempt
        .apply(LifecycleEvent::Spawned, now)
        .map_err(ctx("spawned"))?;
    attempt.identity = Some(identity(&context)?);
    let bytes = attempt.to_bytes().map_err(ctx("encode attempt"))?;
    store
        .write_attempt(attempt_id.as_str(), &bytes)
        .map_err(ctx("write attempt"))?;
    Ok(context)
}

fn spawn_sleep() -> TestResult<LinuxProcess> {
    let (process, _stdio) =
        LinuxProcess::spawn(Command::new("sleep").arg("30")).map_err(ctx("spawn sleep"))?;
    Ok(process)
}

fn capture(process: &LinuxProcess, context: &AttemptContext) -> TestResult<LinuxRecoveryIdentity> {
    LinuxRecoveryIdentity::capture(
        process,
        context.runner_id.clone(),
        context.attempt_id.clone(),
    )
    .map_err(ctx("capture identity"))
}

// ── Happy path and plain outcomes ───────────────────────────────────────────

#[tokio::test]
async fn echo_succeeds_and_is_recorded() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-echo")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let handle = coordinator
        .submit(sh("echo hello; echo oops >&2")?)
        .await
        .map_err(ctx("submit"))?;
    let job_id = handle.id().clone();
    let attempt_id = handle.attempt_id().clone();
    let result = wait(handle).await?;
    assert_eq!(result.state, LifecycleState::Succeeded, "{result:?}");
    assert!(result.is_success());
    assert_eq!(result.exit, Some(ExitOutcome::Exited(0)));
    assert_eq!(result.stdout, b"hello\n");
    assert_eq!(result.stderr, b"oops\n");
    assert_eq!(result.stdout_omitted, 0);
    assert_eq!(result.stderr_omitted, 0);
    assert!(!result.output_truncated);
    assert_eq!(result.stdout_tail(), b"hello\n");
    assert_eq!(result.stdout_total_bytes(), 6);
    assert!(!result.recovered);
    let report = result.sandbox.ok_or(TestError::Missing("sandbox report"))?;
    assert_eq!(report.filesystem, EnforcementState::NotEnforced);
    assert_eq!(report.resource_limits, EnforcementState::Enforced);

    let store = coordinator.store();
    assert_eq!(job_state(store, &job_id)?, JobState::Completed);
    let attempt = read_attempt(store, attempt_id.as_str())?.ok_or(TestError::Missing("attempt"))?;
    assert_eq!(attempt.state, LifecycleState::Succeeded);
    assert!(attempt.identity.is_some(), "identity was persisted");
    assert!(attempt.started_at.is_some());
    let states: Vec<_> = attempt
        .transitions
        .iter()
        .map(|transition| transition.target_state())
        .collect();
    assert_eq!(
        states,
        [
            LifecycleState::Claimed,
            LifecycleState::Starting,
            LifecycleState::Running,
            LifecycleState::Succeeded
        ]
    );
    Ok(())
}

#[tokio::test]
async fn large_output_keeps_head_and_tail() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-large-output")?;
    let executor = LinuxExecutor::new(LinuxExecutorOptions::default()).map_err(ctx("executor"))?;
    let mut config =
        CoordinatorConfig::new(runner, fixture.workspace()).with_lease_ttl(Duration::from_secs(3));
    config.output_capture = OutputCapture::new(8, 16);
    let coordinator =
        Coordinator::new(fixture.store()?, executor, config).map_err(ctx("coordinator"))?;
    // 5 + 2000 * 10 + 3 = 20008 bytes of stdout, 3 bytes of stderr.
    let script = "printf BEGIN; i=0; while [ $i -lt 2000 ]; do printf 0123456789; \
                  i=$((i+1)); done; printf END; printf err >&2";
    let handle = coordinator
        .submit(sh(script)?)
        .await
        .map_err(ctx("submit"))?;
    let result = wait(handle).await?;
    assert_eq!(result.state, LifecycleState::Succeeded, "{result:?}");
    assert!(result.output_truncated);
    assert_eq!(result.stdout, b"BEGIN0127890123456789END");
    assert_eq!(result.stdout_head(), b"BEGIN012");
    assert_eq!(result.stdout_tail(), b"7890123456789END");
    assert_eq!(result.stdout_omitted, 20008 - 24);
    assert_eq!(result.stdout_total_bytes(), 20008);
    assert_eq!(result.stderr, b"err");
    assert_eq!(result.stderr_omitted, 0);
    assert_eq!(result.stderr_tail(), b"err");
    Ok(())
}

#[tokio::test]
async fn nonzero_exit_fails() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-exit3")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let handle = coordinator
        .submit(sh("exit 3")?)
        .await
        .map_err(ctx("submit"))?;
    let job_id = handle.id().clone();
    let result = wait(handle).await?;
    assert_eq!(result.state, LifecycleState::Failed);
    assert_eq!(result.exit, Some(ExitOutcome::Exited(3)));
    assert_eq!(result.reason.as_deref(), Some("exited with status 3"));
    assert_eq!(job_state(coordinator.store(), &job_id)?, JobState::Failed);
    Ok(())
}

#[tokio::test]
async fn working_dir_and_env_come_from_the_spec() -> TestResult {
    let fixture = Fixture::new()?;
    std::fs::create_dir(fixture.workspace().join("sub")).map_err(ctx("sub dir"))?;
    let runner = runner("runner-env")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let spec = JobSpec::command("/bin/sh")
        .args([
            "-c",
            "pwd; echo \"$GREETING\"; echo \"home=${HOME:-unset}\"",
        ])
        .workspace(harw_job_core::WorkspacePath::new("sub").map_err(ctx("path"))?)
        .env("GREETING", "hi")
        .build()
        .map_err(ctx("spec"))?;
    let result = wait(
        coordinator
            .submit(envelope(spec)?)
            .await
            .map_err(ctx("submit"))?,
    )
    .await?;
    assert_eq!(result.state, LifecycleState::Succeeded, "{result:?}");
    let stdout = String::from_utf8_lossy(&result.stdout).into_owned();
    let mut lines = stdout.lines();
    let pwd = lines.next().ok_or(TestError::Missing("pwd line"))?;
    let expected = fixture.workspace().join("sub");
    let expected = expected.canonicalize().map_err(ctx("canonicalize"))?;
    assert_eq!(Path::new(pwd).canonicalize().map_err(ctx("pwd"))?, expected);
    assert_eq!(lines.next(), Some("hi"));
    assert_eq!(
        lines.next(),
        Some("home=unset"),
        "nothing is inherited implicitly"
    );
    Ok(())
}

// ── Deadline and cancellation ───────────────────────────────────────────────

#[tokio::test]
async fn deadline_times_out() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-deadline")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let spec = JobSpec::command("/bin/sh")
        .args(["-c", "sleep 30"])
        .timeout(SignedDuration::from_millis(300))
        .build()
        .map_err(ctx("spec"))?;
    let handle = coordinator
        .submit(envelope(spec)?)
        .await
        .map_err(ctx("submit"))?;
    let job_id = handle.id().clone();
    let result = wait(handle).await?;
    assert_eq!(result.state, LifecycleState::TimedOut, "{result:?}");
    assert_eq!(result.cancellation, Some(CancellationCause::Deadline));
    assert!(matches!(result.exit, Some(ExitOutcome::Signaled { .. })));
    assert_eq!(job_state(coordinator.store(), &job_id)?, JobState::Failed);
    Ok(())
}

#[tokio::test]
async fn cancel_stops_the_job() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-cancel")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let handle = coordinator
        .submit(sh("sleep 30")?)
        .await
        .map_err(ctx("submit"))?;
    let job_id = handle.id().clone();
    wait_for_running(coordinator.store(), handle.attempt_id().as_str()).await?;
    handle.cancel();
    let result = wait(handle).await?;
    assert_eq!(result.state, LifecycleState::Cancelled, "{result:?}");
    assert_eq!(result.cancellation, Some(CancellationCause::User));
    assert!(matches!(
        result.exit,
        Some(ExitOutcome::Signaled { signal: 15, .. })
    ));
    let stored = coordinator.store().load(&job_id).map_err(ctx("load"))?;
    assert_eq!(stored.job.state, JobState::Cancelled);
    assert!(matches!(
        stored.completion.map(|completion| completion.outcome),
        Some(JobOutcome::Cancelled { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn coordinator_cancel_by_id() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-cancel-id")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let handle = coordinator
        .submit(sh("sleep 30")?)
        .await
        .map_err(ctx("submit"))?;
    wait_for_running(coordinator.store(), handle.attempt_id().as_str()).await?;
    assert!(coordinator.cancel(handle.id()));
    assert!(!coordinator.cancel(&WorkId::from_str("unknown-job")));
    let result = wait(handle).await?;
    assert_eq!(result.state, LifecycleState::Cancelled);
    Ok(())
}

// ── Sandbox requirement ─────────────────────────────────────────────────────

#[tokio::test]
async fn required_sandbox_unsatisfied_fails_without_running_the_body() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-required")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let spec = JobSpec::command("/bin/sh")
        .args(["-c", "touch marker"])
        .sandbox_requirement(SandboxRequirement::Required)
        .build()
        .map_err(ctx("spec"))?;
    let handle = coordinator
        .submit(envelope(spec)?)
        .await
        .map_err(ctx("submit"))?;
    let job_id = handle.id().clone();
    let result = wait(handle).await?;
    assert_eq!(result.state, LifecycleState::Failed, "{result:?}");
    assert_eq!(result.exit, None, "nothing was spawned");
    assert!(
        result
            .reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("policy:")),
        "{result:?}"
    );
    let report = result.sandbox.ok_or(TestError::Missing("report"))?;
    assert_eq!(report.filesystem, EnforcementState::NotEnforced);
    assert!(
        !fixture.workspace().join("marker").exists(),
        "the job body must not have run"
    );
    assert_eq!(job_state(coordinator.store(), &job_id)?, JobState::Failed);
    Ok(())
}

#[tokio::test]
async fn resource_limits_without_cgroup_are_reported_not_enforced() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-limits")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let spec = JobSpec::command("/bin/sh")
        .args(["-c", "true"])
        .resources(ResourceRequest {
            pids_max: Some(64),
            ..ResourceRequest::default()
        })
        .build()
        .map_err(ctx("spec"))?;
    let result = wait(
        coordinator
            .submit(envelope(spec)?)
            .await
            .map_err(ctx("submit"))?,
    )
    .await?;
    assert_eq!(result.state, LifecycleState::Succeeded);
    let report = result.sandbox.ok_or(TestError::Missing("report"))?;
    assert_eq!(report.resource_limits, EnforcementState::NotEnforced);
    Ok(())
}

// ── Leases ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn other_runner_cannot_touch_a_running_job() -> TestResult {
    let fixture = Fixture::new()?;
    let owner = runner("runner-owner")?;
    let intruder = runner("runner-intruder")?;
    let coordinator = fixture.coordinator(
        &owner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let other = fixture.coordinator(
        &intruder,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let handle = coordinator
        .submit(sh("sleep 0.5")?)
        .await
        .map_err(ctx("submit"))?;
    wait_for_running(coordinator.store(), handle.attempt_id().as_str()).await?;
    let recovered = other.recover(&intruder).await.map_err(ctx("recover"))?;
    assert!(recovered.is_empty(), "the intruder holds no lease");
    assert!(!other.cancel(handle.id()));
    let result = wait(handle).await?;
    assert_eq!(result.state, LifecycleState::Succeeded);
    Ok(())
}

#[tokio::test]
async fn stale_lease_stops_the_attempt_and_cannot_finalize() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-stale")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_millis(600),
    )?;
    let handle = coordinator
        .submit(sh("sleep 30")?)
        .await
        .map_err(ctx("submit"))?;
    let job_id = handle.id().clone();
    wait_for_running(coordinator.store(), handle.attempt_id().as_str()).await?;

    // The claim as the running coordinator holds it.
    let stored = coordinator.store().load(&job_id).map_err(ctx("load"))?;
    let lease = stored.lease.clone().ok_or(TestError::Missing("lease"))?;
    let stale_claim = JobClaim {
        job: stored.job.clone(),
        scope: stored.scope.clone(),
        token: lease.token(),
        lease,
    };

    // A reaper revokes the lease (as after an expiry).
    // Retried: a concurrent heartbeat holding the record lock makes the
    // reaper skip the job for this round.
    let mut revoked = false;
    for _ in 0..50 {
        let far_future = Timestamp::now()
            .checked_add(SignedDuration::from_secs(3600))
            .map_err(ctx("time"))?;
        let page = coordinator
            .store()
            .reconcile_expired(far_future, 10, None)
            .map_err(ctx("reconcile"))?;
        if page.expired.len() == 1 {
            revoked = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(revoked, "the reaper revoked the lease");

    let result = wait(handle).await?;
    assert_eq!(result.state, LifecycleState::Lost, "{result:?}");
    assert_eq!(result.cancellation, Some(CancellationCause::LeaseLost));
    assert!(matches!(result.exit, Some(ExitOutcome::Signaled { .. })));

    // The record carries the reaper's decision, not the stale runner's.
    let after = coordinator.store().load(&job_id).map_err(ctx("load"))?;
    assert_eq!(after.job.state, JobState::Failed);
    assert!(matches!(
        after.completion.map(|completion| completion.outcome),
        Some(JobOutcome::Failed { reason }) if reason == LEASE_EXPIRED_EXHAUSTED_REASON
    ));
    // A stale claim can never finalize.
    let stale = coordinator.store().transition(
        &stale_claim,
        JobTransition::Complete {
            completed_at: Timestamp::now(),
            outcome: JobOutcome::Cancelled {
                reason: "late".into(),
            },
        },
    );
    assert!(stale.is_err(), "stale lease must not mutate the job");
    Ok(())
}

// ── Recovery (Job-Runtime-Doc §15, §21.4) ───────────────────────────────────

fn current_thread() -> TestResult<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(ctx("runtime"))
}

#[test]
fn restart_reattaches_the_running_job_and_finishes_it() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-restart")?;

    // Coordinator A starts the job, then "crashes": its runtime is dropped
    // without any finalization. The process keeps running.
    let crashed = current_thread()?;
    let (job_id, attempt_id) = crashed.block_on(async {
        let coordinator =
            fixture.coordinator(&runner, fixture.shim_options(), Duration::from_secs(5))?;
        let handle = coordinator
            // No output: the crashed runtime closed the read ends of the
            // pipes, a write would end the job with SIGPIPE.
            .submit(sh("sleep 1; exit 0")?)
            .await
            .map_err(ctx("submit"))?;
        wait_for_running(coordinator.store(), handle.attempt_id().as_str()).await?;
        Ok::<_, TestError>((handle.id().clone(), handle.attempt_id().clone()))
    })?;
    drop(crashed);

    let store = fixture.store()?;
    assert_eq!(job_state(&store, &job_id)?, JobState::Running);

    // Coordinator B (same runner id) recovers.
    let restarted = current_thread()?;
    restarted.block_on(async {
        let coordinator =
            fixture.coordinator(&runner, fixture.shim_options(), Duration::from_secs(5))?;
        let recovered = coordinator.recover(&runner).await.map_err(ctx("recover"))?;
        assert_eq!(recovered.len(), 1);
        let job = recovered
            .into_iter()
            .next()
            .ok_or(TestError::Missing("recovered job"))?;
        assert_eq!(job.job_id, job_id);
        assert_eq!(job.decision, RecoveryDecision::Reattached);
        let handle = job.handle.ok_or(TestError::Missing("handle"))?;
        assert_eq!(handle.attempt_id(), &attempt_id);
        let result = wait(handle).await?;
        assert_eq!(result.state, LifecycleState::Succeeded, "{result:?}");
        assert!(result.recovered);
        assert_eq!(result.exit, Some(ExitOutcome::Exited(0)));
        Ok::<_, TestError>(())
    })?;
    assert_eq!(job_state(&store, &job_id)?, JobState::Completed);
    let attempt =
        read_attempt(&store, attempt_id.as_str())?.ok_or(TestError::Missing("attempt"))?;
    assert!(attempt.recovered);
    assert!(
        !fixture
            .status_dir()
            .join(format!("{attempt_id}.status"))
            .exists(),
        "status file is released after finalization"
    );
    Ok(())
}

#[tokio::test]
async fn process_died_while_down_is_lost() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-died")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let mut sleeper = spawn_sleep()?;
    let context =
        plant_running_attempt(&coordinator, &runner, |context| capture(&sleeper, context))?;
    // The process dies (and is reaped) while no coordinator runs.
    sleeper.signal(SignalKind::Kill).map_err(ctx("kill"))?;
    sleeper.wait().map_err(ctx("reap"))?;

    let recovered = coordinator.recover(&runner).await.map_err(ctx("recover"))?;
    let job = recovered
        .into_iter()
        .next()
        .ok_or(TestError::Missing("recovered job"))?;
    assert!(
        matches!(&job.decision, RecoveryDecision::Lost(reason) if reason.contains("exited while the runner was down")),
        "{:?}",
        job.decision
    );
    let result = wait(job.handle.ok_or(TestError::Missing("handle"))?).await?;
    assert_eq!(result.state, LifecycleState::Lost);
    assert_eq!(
        job_state(coordinator.store(), &context.job_id)?,
        JobState::Failed
    );
    Ok(())
}

#[tokio::test]
async fn job_completed_before_recovery_uses_the_recorded_status() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-completed")?;
    let coordinator =
        fixture.coordinator(&runner, fixture.shim_options(), Duration::from_secs(3))?;
    let mut sleeper = spawn_sleep()?;
    let context =
        plant_running_attempt(&coordinator, &runner, |context| capture(&sleeper, context))?;
    sleeper.signal(SignalKind::Kill).map_err(ctx("kill"))?;
    sleeper.wait().map_err(ctx("reap"))?;
    // What the exit-status shim leaves behind for a clean exit.
    std::fs::write(
        fixture
            .status_dir()
            .join(format!("{}.status", context.attempt_id)),
        "0\n",
    )
    .map_err(ctx("status file"))?;

    let recovered = coordinator.recover(&runner).await.map_err(ctx("recover"))?;
    let job = recovered
        .into_iter()
        .next()
        .ok_or(TestError::Missing("recovered job"))?;
    assert_eq!(job.decision, RecoveryDecision::ExitedWhileDown);
    let result = wait(job.handle.ok_or(TestError::Missing("handle"))?).await?;
    assert_eq!(result.state, LifecycleState::Succeeded);
    assert_eq!(result.exit, Some(ExitOutcome::Exited(0)));
    assert_eq!(
        job_state(coordinator.store(), &context.job_id)?,
        JobState::Completed
    );
    Ok(())
}

#[tokio::test]
async fn pid_reuse_is_lost_and_the_unrelated_process_survives() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-pid-reuse")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    // A live, unrelated process now owns the persisted PID: the persisted
    // start time is not its start time.
    let mut unrelated = spawn_sleep()?;
    let context = plant_running_attempt(&coordinator, &runner, |context| {
        let mut identity = capture(&unrelated, context)?;
        let ticks = identity
            .process_start_time
            .ok_or(TestError::Missing("start time"))?;
        identity.process_start_time = Some(ticks.wrapping_add(12_345));
        Ok(identity)
    })?;

    let recovered = coordinator.recover(&runner).await.map_err(ctx("recover"))?;
    let job = recovered
        .into_iter()
        .next()
        .ok_or(TestError::Missing("recovered job"))?;
    assert!(
        matches!(&job.decision, RecoveryDecision::Lost(reason) if reason.contains("mismatch")),
        "{:?}",
        job.decision
    );
    let result = wait(job.handle.ok_or(TestError::Missing("handle"))?).await?;
    assert_eq!(result.state, LifecycleState::Lost);
    assert_eq!(
        job_state(coordinator.store(), &context.job_id)?,
        JobState::Failed
    );

    // The unrelated process was never signalled.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !unrelated.has_exited().map_err(ctx("has_exited"))?,
        "an unrelated process must never be killed"
    );
    unrelated
        .signal(SignalKind::Kill)
        .map_err(ctx("cleanup kill"))?;
    unrelated.wait().map_err(ctx("cleanup reap"))?;
    Ok(())
}

#[tokio::test]
async fn attempt_without_identity_is_lost() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-no-identity")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    // Crash after the claim, before any sidecar was written.
    let now = Timestamp::now();
    let record = coordinator
        .new_record(&sh("true")?, now)
        .map_err(ctx("record"))?;
    let job_id = record.job.id.clone();
    let store = coordinator.store();
    store.create_job(record).map_err(ctx("create"))?;
    store
        .claim_job(
            &job_id,
            &runner,
            ClaimTerms {
                lease_ttl: SignedDuration::from_secs(60),
                now,
            },
        )
        .map_err(ctx("claim"))?;

    let recovered = coordinator.recover(&runner).await.map_err(ctx("recover"))?;
    let job = recovered
        .into_iter()
        .next()
        .ok_or(TestError::Missing("recovered job"))?;
    assert!(
        matches!(job.decision, RecoveryDecision::Lost(_)),
        "{:?}",
        job.decision
    );
    let result = wait(job.handle.ok_or(TestError::Missing("handle"))?).await?;
    assert_eq!(result.state, LifecycleState::Lost);
    assert_eq!(job_state(store, &job_id)?, JobState::Failed);
    Ok(())
}

// ── cgroup v2 (needs a delegated root) ──────────────────────────────────────

/// Set `HARW_TEST_CGROUP_ROOT` to a delegated cgroup v2 directory and run
/// with `--ignored`.
#[tokio::test]
#[ignore = "needs a delegated cgroup v2 root in HARW_TEST_CGROUP_ROOT"]
async fn cgroup_limits_are_enforced_and_reported() -> TestResult {
    let root = std::env::var_os("HARW_TEST_CGROUP_ROOT")
        .map(PathBuf::from)
        .ok_or(TestError::Missing("HARW_TEST_CGROUP_ROOT"))?;
    let fixture = Fixture::new()?;
    let runner = runner("runner-cgroup")?;
    let options = LinuxExecutorOptions {
        cgroup_root: Some(root),
        ..LinuxExecutorOptions::default()
    };
    let coordinator = fixture.coordinator(&runner, options, Duration::from_secs(3))?;
    let spec = JobSpec::command("/bin/sh")
        .args(["-c", "cat /proc/self/cgroup"])
        .resources(ResourceRequest {
            memory_max: Some(64 * 1024 * 1024),
            pids_max: Some(32),
            ..ResourceRequest::default()
        })
        .build()
        .map_err(ctx("spec"))?;
    let result = wait(
        coordinator
            .submit(envelope(spec)?)
            .await
            .map_err(ctx("submit"))?,
    )
    .await?;
    assert_eq!(result.state, LifecycleState::Succeeded, "{result:?}");
    let report = result.sandbox.ok_or(TestError::Missing("report"))?;
    assert_eq!(report.resource_limits, EnforcementState::Enforced);
    let cgroup = String::from_utf8_lossy(&result.stdout).into_owned();
    assert!(
        cgroup.contains("harw-job-"),
        "job ran in its own cgroup: {cgroup}"
    );
    Ok(())
}

/// Reads the delegated cgroup root of the `#[ignore]`d cgroup scenarios.
fn cgroup_root() -> TestResult<PathBuf> {
    std::env::var_os("HARW_TEST_CGROUP_ROOT")
        .map(PathBuf::from)
        .ok_or(TestError::Missing("HARW_TEST_CGROUP_ROOT"))
}

/// The job cgroup name of an attempt (mirrors the executor's naming).
fn job_cgroup_name(attempt: &AttemptId) -> String {
    let safe: String = attempt
        .as_str()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    format!("harw-job-{safe}")
}

/// Waits until the job wrote its background child's PID to `path`.
fn read_pid_file(path: &Path) -> TestResult<u32> {
    let started = std::time::Instant::now();
    while started.elapsed() < LIMIT {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse::<u32>() {
                return Ok(pid);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(TestError::Missing("pid file"))
}

/// Whether `pid` is gone (or a zombie awaiting its reaper).
fn process_gone(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat
            .rsplit(')')
            .next()
            .map(str::trim_start)
            .is_some_and(|rest| rest.starts_with('Z') || rest.starts_with('X')),
    }
}

/// Polls until `pid` is gone; `false` if it is still alive after [`LIMIT`].
fn wait_gone(pid: u32) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < LIMIT {
        if process_gone(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Kills and removes a job cgroup a test left behind.
fn remove_job_cgroup(root: &Path, name: &str) -> TestResult {
    let backend = CgroupV2Fs::open(root).map_err(ctx("open cgroup root"))?;
    for _ in 0..100 {
        let handle = match backend.reopen(name) {
            Ok(handle) => handle,
            Err(CgroupError::NotFound { .. }) => return Ok(()),
            Err(error) => return Err(ctx("reopen cgroup")(error)),
        };
        let _ = backend.kill(&handle);
        match backend.remove(handle) {
            Ok(()) => return Ok(()),
            Err(CgroupError::Busy { .. }) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(ctx("remove cgroup")(error)),
        }
    }
    Err(TestError::Unexpected(format!(
        "job cgroup {name} stayed busy"
    )))
}

// ── Retry and requeue (Job-Runtime-Doc §6) ──────────────────────────────────

/// A retry policy of `max_attempts` with a constant `delay`.
fn retry_policy(max_attempts: u32, delay: SignedDuration) -> TestResult<RetryPolicy> {
    RetryPolicy::try_new(max_attempts, delay, 2.0, delay).map_err(ctx("retry policy"))
}

/// Polls until the attempt sidecar exists.
async fn wait_for_attempt(
    store: &FsJobRecordStore,
    attempt: &str,
) -> TestResult<AttemptRecord<LinuxRecoveryIdentity>> {
    let started = std::time::Instant::now();
    while started.elapsed() < LIMIT {
        if let Some(record) = read_attempt(store, attempt)? {
            return Ok(record);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err(TestError::Unexpected(format!(
        "attempt {attempt} was never persisted"
    )))
}

/// The id of attempt `number` of the claim that ran attempt `first`.
fn retry_id(
    store: &FsJobRecordStore,
    job: &WorkId,
    first: &AttemptId,
    number: u32,
) -> TestResult<AttemptId> {
    let record = read_attempt(store, first.as_str())?.ok_or(TestError::Missing("attempt 1"))?;
    retry_attempt_id(job, record.epoch, number).map_err(ctx("retry attempt id"))
}

fn runs(fixture: &Fixture) -> TestResult<usize> {
    let text =
        std::fs::read_to_string(fixture.workspace().join("runs")).map_err(ctx("runs file"))?;
    Ok(text.lines().count())
}

#[tokio::test]
async fn failed_attempt_is_retried_and_the_second_attempt_succeeds() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-retry")?;
    let coordinator =
        fixture.retrying_coordinator(&runner, retry_policy(3, SignedDuration::from_millis(50))?)?;
    // The first run leaves a marker and fails; the second finds it.
    let script = "echo run >> runs; \
                  if [ -e marker ]; then echo second; else touch marker; exit 7; fi";
    let handle = coordinator
        .submit(sh(script)?)
        .await
        .map_err(ctx("submit"))?;
    let job_id = handle.id().clone();
    let first = handle.attempt_id().clone();
    let result = wait(handle).await?;
    assert_eq!(result.state, LifecycleState::Succeeded, "{result:?}");
    assert_eq!(result.exit, Some(ExitOutcome::Exited(0)));
    assert_eq!(result.stdout, b"second\n");
    assert_eq!(result.reason, None);
    assert_eq!(runs(&fixture)?, 2);

    let store = coordinator.store();
    let second = retry_id(store, &job_id, &first, 2)?;
    assert_eq!(result.attempt_id, second, "the retry decided the outcome");
    assert_eq!(job_state(store, &job_id)?, JobState::Completed);

    let attempt1 = read_attempt(store, first.as_str())?.ok_or(TestError::Missing("attempt 1"))?;
    assert_eq!(attempt1.state, LifecycleState::Failed);
    assert_eq!(attempt1.exit, Some(ExitOutcome::Exited(7)));
    let attempt2 = read_attempt(store, second.as_str())?.ok_or(TestError::Missing("attempt 2"))?;
    assert_eq!(attempt2.state, LifecycleState::Succeeded);
    assert_eq!(
        attempt2.epoch, attempt1.epoch,
        "retries run under one claim"
    );
    let retry = attempt2
        .transitions
        .first()
        .ok_or(TestError::Missing("retry transition"))?;
    assert_eq!(retry.source_state(), LifecycleState::Failed);
    assert_eq!(retry.event(), LifecycleEvent::Retry);
    let states: Vec<_> = attempt2
        .transitions
        .iter()
        .map(|transition| transition.target_state())
        .collect();
    assert_eq!(
        states,
        [
            LifecycleState::Queued,
            LifecycleState::Claimed,
            LifecycleState::Starting,
            LifecycleState::Running,
            LifecycleState::Succeeded
        ]
    );
    let third = retry_id(store, &job_id, &first, 3)?;
    assert!(read_attempt(store, third.as_str())?.is_none());
    Ok(())
}

#[tokio::test]
async fn cancelled_attempt_is_never_retried() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-retry-cancel")?;
    let coordinator =
        fixture.retrying_coordinator(&runner, retry_policy(3, SignedDuration::from_millis(50))?)?;
    let handle = coordinator
        .submit(sh("echo run >> runs; sleep 30")?)
        .await
        .map_err(ctx("submit"))?;
    let job_id = handle.id().clone();
    let first = handle.attempt_id().clone();
    wait_for_running(coordinator.store(), first.as_str()).await?;
    handle.cancel();
    let result = wait(handle).await?;
    assert_eq!(result.state, LifecycleState::Cancelled, "{result:?}");
    assert_eq!(result.cancellation, Some(CancellationCause::User));
    assert_eq!(result.attempt_id, first);

    let store = coordinator.store();
    assert_eq!(job_state(store, &job_id)?, JobState::Cancelled);
    let second = retry_id(store, &job_id, &first, 2)?;
    // Give a (wrongly) scheduled retry the time to show up.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        read_attempt(store, second.as_str())?.is_none(),
        "a cancelled attempt must not be retried"
    );
    assert_eq!(runs(&fixture)?, 1);
    Ok(())
}

#[tokio::test]
async fn retry_limit_ends_the_job_failed() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-retry-limit")?;
    let coordinator =
        fixture.retrying_coordinator(&runner, retry_policy(2, SignedDuration::from_millis(20))?)?;
    let handle = coordinator
        .submit(sh("echo run >> runs; exit 5")?)
        .await
        .map_err(ctx("submit"))?;
    let job_id = handle.id().clone();
    let first = handle.attempt_id().clone();
    let result = wait(handle).await?;
    assert_eq!(result.state, LifecycleState::Failed, "{result:?}");
    assert_eq!(result.exit, Some(ExitOutcome::Exited(5)));
    assert_eq!(
        result.reason.as_deref(),
        Some("exited with status 5 (attempt 2)")
    );
    assert_eq!(runs(&fixture)?, 2, "max_attempts bounds the runs");

    let store = coordinator.store();
    let second = retry_id(store, &job_id, &first, 2)?;
    assert_eq!(result.attempt_id, second);
    let stored = store.load(&job_id).map_err(ctx("load"))?;
    assert_eq!(stored.job.state, JobState::Failed);
    assert!(matches!(
        stored.completion.map(|completion| completion.outcome),
        Some(JobOutcome::Failed { reason }) if reason.contains("exited with status 5 (attempt 2)")
    ));
    for (attempt, number) in [(&first, 1), (&second, 2)] {
        let record = read_attempt(store, attempt.as_str())?.ok_or(TestError::Missing("attempt"))?;
        assert_eq!(record.state, LifecycleState::Failed, "attempt {number}");
    }
    let third = retry_id(store, &job_id, &first, 3)?;
    assert!(read_attempt(store, third.as_str())?.is_none());
    Ok(())
}

#[tokio::test]
async fn cancel_during_the_backoff_ends_the_job_cancelled() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-retry-backoff")?;
    let coordinator =
        fixture.retrying_coordinator(&runner, retry_policy(2, SignedDuration::from_secs(60))?)?;
    let handle = coordinator
        .submit(sh("echo run >> runs; exit 1")?)
        .await
        .map_err(ctx("submit"))?;
    let job_id = handle.id().clone();
    let first = handle.attempt_id().clone();
    let store = coordinator.store();
    wait_for_attempt(store, first.as_str()).await?;
    let second = retry_id(store, &job_id, &first, 2)?;
    // Attempt 2 is persisted before its (60 s) backoff starts.
    let pending = wait_for_attempt(store, second.as_str()).await?;
    assert_eq!(pending.state, LifecycleState::Claimed);
    handle.cancel();
    let result = wait(handle).await?;
    assert_eq!(result.state, LifecycleState::Cancelled, "{result:?}");
    assert_eq!(result.attempt_id, second);
    assert_eq!(result.exit, None, "attempt 2 never ran");
    assert_eq!(job_state(store, &job_id)?, JobState::Cancelled);
    assert_eq!(runs(&fixture)?, 1);
    Ok(())
}

#[tokio::test]
async fn recovery_resumes_the_latest_retry_attempt() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-retry-recover")?;
    let coordinator =
        fixture.retrying_coordinator(&runner, retry_policy(3, SignedDuration::from_millis(50))?)?;
    let mut sleeper = spawn_sleep()?;
    // Attempt 1 failed; attempt 2 was running when the runner stopped.
    let first = plant_running_attempt(&coordinator, &runner, |context| capture(&sleeper, context))?;
    let store = coordinator.store();
    let mut attempt1 =
        read_attempt(store, first.attempt_id.as_str())?.ok_or(TestError::Missing("attempt 1"))?;
    let second_id = retry_attempt_id(&first.job_id, attempt1.epoch, 2).map_err(ctx("id"))?;
    let now = Timestamp::now();
    let mut attempt2 = AttemptRecord::claimed(
        first.job_id.clone(),
        second_id.clone(),
        runner.clone(),
        attempt1.epoch,
        now,
    )
    .map_err(ctx("attempt 2"))?;
    attempt2.identity = attempt1.identity.take();
    attempt1
        .apply(LifecycleEvent::Fail, now)
        .map_err(ctx("fail attempt 1"))?;
    attempt2.transitions.insert(
        0,
        LifecycleTransition::apply(LifecycleState::Failed, LifecycleEvent::Retry)
            .map_err(ctx("retry"))?,
    );
    for event in [LifecycleEvent::Start, LifecycleEvent::Spawned] {
        attempt2.apply(event, now).map_err(ctx("attempt 2 event"))?;
    }
    for record in [&attempt1, &attempt2] {
        let bytes = record.to_bytes().map_err(ctx("encode"))?;
        store
            .write_attempt(record.attempt_id.as_str(), &bytes)
            .map_err(ctx("write attempt"))?;
    }

    let recovered = coordinator.recover(&runner).await.map_err(ctx("recover"))?;
    let job = recovered
        .into_iter()
        .next()
        .ok_or(TestError::Missing("recovered job"))?;
    assert_eq!(job.decision, RecoveryDecision::Reattached);
    let handle = job.handle.ok_or(TestError::Missing("handle"))?;
    assert_eq!(
        handle.attempt_id(),
        &second_id,
        "the latest attempt is resumed"
    );
    assert!(coordinator.cancel(&first.job_id));
    let result = wait(handle).await?;
    assert_eq!(result.state, LifecycleState::Cancelled, "{result:?}");
    assert_eq!(result.attempt_id, second_id);
    assert!(result.recovered);
    assert_eq!(job_state(store, &first.job_id)?, JobState::Cancelled);
    sleeper.wait().map_err(ctx("reap"))?;
    Ok(())
}

// ── Recovered job cgroup (needs a delegated root) ───────────────────────────

/// Starts `sleep 30 & …; wait` in a job cgroup, lets the coordinator
/// "crash", and returns the job, its attempt and the background child.
fn start_cgroup_job_and_crash(
    fixture: &Fixture,
    runner: &RunnerId,
    options: LinuxExecutorOptions,
) -> TestResult<(WorkId, AttemptId, u32)> {
    let crashed = current_thread()?;
    let (job_id, attempt_id) = crashed.block_on(async {
        let coordinator = fixture.coordinator(runner, options, Duration::from_secs(5))?;
        let handle = coordinator
            // No output: the crashed runtime closes the pipes' read ends.
            .submit(sh("sleep 30 & echo $! > child.pid; wait")?)
            .await
            .map_err(ctx("submit"))?;
        wait_for_running(coordinator.store(), handle.attempt_id().as_str()).await?;
        Ok::<_, TestError>((handle.id().clone(), handle.attempt_id().clone()))
    })?;
    drop(crashed);
    let child = read_pid_file(&fixture.workspace().join("child.pid"))?;
    Ok((job_id, attempt_id, child))
}

/// Set `HARW_TEST_CGROUP_ROOT` to a delegated cgroup v2 directory and run
/// with `--ignored`.
#[test]
#[ignore = "needs a delegated cgroup v2 root in HARW_TEST_CGROUP_ROOT"]
fn recovered_cancel_kills_the_reopened_job_cgroup() -> TestResult {
    let root = cgroup_root()?;
    let fixture = Fixture::new()?;
    let runner = runner("runner-cgroup-recover")?;
    let options = LinuxExecutorOptions {
        cgroup_root: Some(root.clone()),
        ..fixture.shim_options()
    };
    let (job_id, attempt_id, child) =
        start_cgroup_job_and_crash(&fixture, &runner, options.clone())?;
    assert!(!process_gone(child), "the background child runs");

    let restarted = current_thread()?;
    restarted.block_on(async {
        let coordinator = fixture.coordinator(&runner, options, Duration::from_secs(5))?;
        let recovered = coordinator.recover(&runner).await.map_err(ctx("recover"))?;
        let job = recovered
            .into_iter()
            .next()
            .ok_or(TestError::Missing("recovered job"))?;
        assert_eq!(job.job_id, job_id);
        assert_eq!(job.decision, RecoveryDecision::Reattached);
        let handle = job.handle.ok_or(TestError::Missing("handle"))?;
        handle.cancel();
        let result = wait(handle).await?;
        assert_eq!(result.state, LifecycleState::Cancelled, "{result:?}");
        assert!(
            !result
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("recovery:")),
            "the job cgroup was reopened: {result:?}"
        );
        Ok::<_, TestError>(())
    })?;
    assert!(
        wait_gone(child),
        "the descendant died with the reopened job cgroup"
    );
    assert!(
        !root.join(job_cgroup_name(&attempt_id)).exists(),
        "the recovered job cgroup was removed"
    );
    Ok(())
}

/// Set `HARW_TEST_CGROUP_ROOT` to a delegated cgroup v2 directory and run
/// with `--ignored`.
#[test]
#[ignore = "needs a delegated cgroup v2 root in HARW_TEST_CGROUP_ROOT"]
fn recovered_job_without_its_cgroup_falls_back_to_the_process() -> TestResult {
    let root = cgroup_root()?;
    let fixture = Fixture::new()?;
    let runner = runner("runner-cgroup-fallback")?;
    let options = LinuxExecutorOptions {
        cgroup_root: Some(root.clone()),
        ..fixture.shim_options()
    };
    let (_job_id, attempt_id, child) = start_cgroup_job_and_crash(&fixture, &runner, options)?;

    // The restarted runner has no cgroup root: process-only termination.
    let restarted = current_thread()?;
    let outcome = restarted.block_on(async {
        let coordinator =
            fixture.coordinator(&runner, fixture.shim_options(), Duration::from_secs(5))?;
        let recovered = coordinator.recover(&runner).await.map_err(ctx("recover"))?;
        let job = recovered
            .into_iter()
            .next()
            .ok_or(TestError::Missing("recovered job"))?;
        assert_eq!(job.decision, RecoveryDecision::Reattached);
        let handle = job.handle.ok_or(TestError::Missing("handle"))?;
        handle.cancel();
        wait(handle).await
    });
    let name = job_cgroup_name(&attempt_id);
    let survived = !process_gone(child);
    remove_job_cgroup(&root, &name)?;
    let result = outcome?;
    assert_eq!(result.state, LifecycleState::Cancelled, "{result:?}");
    assert!(
        result.reason.as_deref().is_some_and(|reason| {
            reason.contains("recovery: job cgroup") && reason.contains("not reopened")
        }),
        "the fallback is recorded: {result:?}"
    );
    assert!(
        survived,
        "process-only termination does not reach the descendant"
    );
    Ok(())
}

// --- live frames and persistence classes ---------------------------------

use super::frames::{FrameEvent, JobFrame, JobFrames};
use super::runner::{Persistence, SubmitOptions};

/// Reads every frame until the stream ends.
async fn collect_frames(mut frames: JobFrames) -> TestResult<Vec<FrameEvent>> {
    let mut all = Vec::new();
    loop {
        match tokio::time::timeout(LIMIT, frames.next())
            .await
            .map_err(ctx("frames did not end in time"))?
        {
            Some(event) => all.push(event),
            None => return Ok(all),
        }
    }
}

fn stdout_of(events: &[FrameEvent]) -> Vec<u8> {
    events
        .iter()
        .filter_map(|event| match event {
            FrameEvent::Frame(JobFrame::Stdout(bytes)) => Some(bytes.to_vec()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[tokio::test]
async fn frames_stream_the_output_between_the_attempt_boundaries() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-frames")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let (handle, frames) = coordinator
        .submit_with(
            sh("echo hello; echo oops >&2")?,
            SubmitOptions {
                stream: true,
                ..SubmitOptions::default()
            },
        )
        .await
        .map_err(ctx("submit"))?;
    let frames = frames.ok_or(TestError::Missing("subscription"))?;
    let result = wait(handle).await?;
    let events = collect_frames(frames).await?;

    assert!(
        matches!(
            events.first(),
            Some(FrameEvent::Frame(JobFrame::AttemptStarted { .. }))
        ),
        "the stream opens with the attempt: {events:?}"
    );
    assert_eq!(stdout_of(&events), b"hello\n");
    let stderr: Vec<u8> = events
        .iter()
        .filter_map(|event| match event {
            FrameEvent::Frame(JobFrame::Stderr(bytes)) => Some(bytes.to_vec()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(stderr, b"oops\n");
    assert!(
        matches!(
            events.last(),
            Some(FrameEvent::Frame(JobFrame::AttemptEnded {
                outcome: ExitOutcome::Exited(0),
                ..
            }))
        ),
        "and closes with its end: {events:?}"
    );
    // The result still has everything.
    assert_eq!(result.stdout, b"hello\n");
    Ok(())
}

#[tokio::test]
async fn a_later_subscription_ends_when_the_job_is_finished() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-late-frames")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let handle = coordinator
        .submit(sh("sleep 0.3; echo late")?)
        .await
        .map_err(ctx("submit"))?;
    let frames = handle.subscribe();
    let result = wait(handle).await?;
    assert!(result.is_success());
    let events = collect_frames(frames).await?;
    assert_eq!(stdout_of(&events), b"late\n");
    // Once finished, a new subscription is an already finished stream.
    Ok(())
}

#[tokio::test]
async fn a_subscriber_that_never_reads_does_not_slow_the_job_down() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-slow-subscriber")?;
    let executor = LinuxExecutor::new(LinuxExecutorOptions::default()).map_err(ctx("executor"))?;
    let mut config =
        CoordinatorConfig::new(runner, fixture.workspace()).with_lease_ttl(Duration::from_secs(3));
    config.frame_buffer = 2;
    let coordinator =
        Coordinator::new(fixture.store()?, executor, config).map_err(ctx("coordinator"))?;
    let (handle, frames) = coordinator
        .submit_with(
            sh("i=0; while [ $i -lt 3000 ]; do echo line-$i; i=$((i+1)); done")?,
            SubmitOptions {
                stream: true,
                ..SubmitOptions::default()
            },
        )
        .await
        .map_err(ctx("submit"))?;
    let mut frames = frames.ok_or(TestError::Missing("subscription"))?;
    // Nobody reads while the job runs.
    let result = wait(handle).await?;
    assert!(result.is_success(), "{result:?}");
    assert!(result.stdout_total_bytes() > 20_000);
    // The subscriber learns that it missed frames, and then the stream ends.
    let first = tokio::time::timeout(LIMIT, frames.next())
        .await
        .map_err(ctx("first frame"))?;
    assert!(
        matches!(first, Some(FrameEvent::Lagged(missed)) if missed > 0),
        "{first:?}"
    );
    Ok(())
}

fn stored_input(coordinator: &LinuxCoordinator, job: &WorkId) -> TestResult<String> {
    Ok(coordinator
        .store()
        .load(job)
        .map_err(ctx("load job"))?
        .input
        .to_string())
}

fn secret_job() -> TestResult<JobSpecEnvelope> {
    envelope(
        JobSpec::command("/bin/sh")
            .arg("-c")
            .arg("echo ran-$HARW_TEST_SECRET >/dev/null; echo SECRET-ARGUMENT-in-argv >/dev/null")
            .env("HARW_TEST_SECRET", "topsecret-value")
            .build()
            .map_err(ctx("spec"))?,
    )
}

#[tokio::test]
async fn the_persistence_class_decides_what_the_job_record_keeps() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-persistence")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let mut recorded = Vec::new();
    for persistence in [
        Persistence::Durable,
        Persistence::MetadataOnly,
        Persistence::Ephemeral,
    ] {
        let (handle, _) = coordinator
            .submit_with(
                secret_job()?,
                SubmitOptions {
                    persistence,
                    ..SubmitOptions::default()
                },
            )
            .await
            .map_err(ctx("submit"))?;
        let job_id = handle.id().clone();
        let result = wait(handle).await?;
        assert!(result.is_success(), "the job runs the same way: {result:?}");
        recorded.push((persistence, stored_input(&coordinator, &job_id)?));
    }
    let [(_, durable), (_, metadata), (_, ephemeral)] = recorded.as_slice() else {
        return Err(TestError::Missing("three records"));
    };
    assert!(durable.contains("topsecret-value") && durable.contains("SECRET-ARGUMENT"));
    for text in [metadata, ephemeral] {
        assert!(!text.contains("topsecret-value"), "no env value: {text}");
        assert!(!text.contains("SECRET-ARGUMENT"), "no argument: {text}");
    }
    assert!(
        metadata.contains("HARW_TEST_SECRET"),
        "names are kept: {metadata}"
    );
    assert!(metadata.contains("/bin/sh") && metadata.contains("arg_count"));
    assert!(
        !ephemeral.contains("/bin/sh"),
        "nothing about the command: {ephemeral}"
    );
    Ok(())
}

#[tokio::test]
async fn a_redacted_record_can_never_be_read_back_as_a_runnable_spec() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-redacted")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let now = Timestamp::now();
    for persistence in [Persistence::MetadataOnly, Persistence::Ephemeral] {
        let record = coordinator
            .new_record_with(&secret_job()?, now, persistence)
            .map_err(ctx("record"))?;
        // Recovery parses the input as an envelope to get a spec; for a
        // redacted record that must fail, so nothing is re-run from it.
        assert!(
            serde_json::from_value::<JobSpecEnvelope>(record.input.clone()).is_err(),
            "{persistence:?}: {}",
            record.input
        );
    }
    Ok(())
}

// --- per-process rlimits ---------------------------------------------------

fn prlimit_present() -> bool {
    ["/usr/bin/prlimit", "/bin/prlimit"]
        .iter()
        .any(|path| std::path::Path::new(path).is_file())
}

#[tokio::test]
async fn per_process_rlimits_reach_the_program_and_are_reported() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-rlimits")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let spec = JobSpec::command("/bin/sh")
        .arg("-c")
        .arg("ulimit -n; ulimit -t")
        .resources(ResourceRequest {
            open_files_max: Some(77),
            cpu_time_max: Some(33),
            ..ResourceRequest::default()
        })
        .build()
        .map_err(ctx("spec"))?;
    let handle = coordinator
        .submit(envelope(spec)?)
        .await
        .map_err(ctx("submit"))?;
    let result = wait(handle).await?;
    assert!(result.is_success(), "{result:?}");
    let report = result.sandbox.ok_or(TestError::Missing("report"))?;
    if prlimit_present() {
        assert_eq!(String::from_utf8_lossy(&result.stdout), "77\n33\n");
        assert_eq!(report.resource_limits, EnforcementState::Enforced);
    } else {
        // No prlimit: the limits are not applied, and the report says so.
        assert_eq!(report.resource_limits, EnforcementState::NotEnforced);
    }
    Ok(())
}

#[tokio::test]
async fn a_job_without_rlimits_is_not_wrapped() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("runner-no-rlimits")?;
    let coordinator = fixture.coordinator(
        &runner,
        LinuxExecutorOptions::default(),
        Duration::from_secs(3),
    )?;
    let handle = coordinator
        .submit(sh("echo plain")?)
        .await
        .map_err(ctx("submit"))?;
    let result = wait(handle).await?;
    assert_eq!(result.stdout, b"plain\n");
    let report = result.sandbox.ok_or(TestError::Missing("report"))?;
    assert_eq!(report.resource_limits, EnforcementState::Enforced);
    Ok(())
}
