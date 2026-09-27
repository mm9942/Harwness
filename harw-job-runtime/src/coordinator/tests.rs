//! Coordinator scenarios on Linux (Job-Runtime-Doc §21.2, §21.4).
//!
//! Every test runs real processes (`/bin/sh`, `sleep`) against a
//! file-backed store in a temporary directory. cgroup-dependent tests are
//! `#[ignore]`d (they need a delegated cgroup v2 root).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use harw_job_core::{
    CancellationCause, EnforcementState, ExitOutcome, JobClaim, JobOutcome, JobSpec,
    JobSpecEnvelope, JobState, LifecycleEvent, LifecycleState, ResourceRequest, RunnerId,
    SandboxRequirement,
};
use harw_job_linux::{LinuxProcess, LinuxRecoveryIdentity, SignalKind};
use harw_job_store::{
    ClaimTerms, FsJobRecordStore, JobRecordStore, JobTransition, LEASE_EXPIRED_EXHAUSTED_REASON,
};
use harw_types::WorkId;
use jiff::{SignedDuration, Timestamp};
use tempfile::TempDir;

use super::attempt::{AttemptRecord, attempt_id_for};
use super::executor::AttemptContext;
use super::linux::{LinuxExecutor, LinuxExecutorOptions};
use super::runner::{Coordinator, CoordinatorConfig, JobHandle, JobResult, RecoveryDecision};
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
