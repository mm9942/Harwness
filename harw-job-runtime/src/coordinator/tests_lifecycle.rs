//! End-to-end job-lifecycle scenarios on Linux (happy → fail → retry →
//! backoff → final), driven exclusively through the public coordinator API.
//!
//! Unlike the sibling `tests` module this file lives behind `#[cfg(test)]` in
//! the same crate, so it may reuse crate-internal helpers; the scenario itself
//! uses only public coordinator types (`Coordinator`, `JobHandle`,
//! `CoordinatorConfig`), no `test_support` bypass.

#![cfg(target_os = "linux")]

use std::time::Duration;

use harw_job_core::{
    ExitOutcome, JobState, LifecycleState, RetryPolicy, RunnerId,
};
use harw_job_store::FsJobRecordStore;
use jiff::SignedDuration;
use tempfile::TempDir;

use super::linux::{LinuxExecutor, LinuxExecutorOptions};
use super::runner::{Coordinator, CoordinatorConfig, JobHandle, JobResult};
use crate::test_support::{TestError, TestResult, ctx};

/// Upper bound for any single wait; a hang fails instead of blocking CI.
const LIMIT: Duration = Duration::from_secs(30);

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::create_dir(dir.path().join("workspace")).map_err(ctx("workspace dir"))?;
        Ok(Self { dir })
    }

    fn workspace(&self) -> std::path::PathBuf {
        self.dir.path().join("workspace")
    }

    fn store(&self) -> TestResult<FsJobRecordStore> {
        FsJobRecordStore::create_ambient(&self.dir.path().join("store")).map_err(ctx("store"))
    }

    fn coordinator(
        &self,
        runner: &RunnerId,
        retry: RetryPolicy,
    ) -> TestResult<Coordinator<FsJobRecordStore, LinuxExecutor>> {
        let executor =
            LinuxExecutor::new(LinuxExecutorOptions::default()).map_err(ctx("executor"))?;
        let config = CoordinatorConfig::new(runner.clone(), self.workspace())
            .with_lease_ttl(Duration::from_secs(3))
            .with_retry(retry);
        Coordinator::new(self.store()?, executor, config).map_err(ctx("coordinator"))
    }
}

fn runner(name: &str) -> TestResult<RunnerId> {
    RunnerId::new(name).map_err(ctx("runner id"))
}

fn policy(max_attempts: u32, delay: SignedDuration) -> TestResult<RetryPolicy> {
    RetryPolicy::try_new(max_attempts, delay, 2.0, delay).map_err(ctx("retry policy"))
}

fn sh(script: &str) -> TestResult<harw_job_core::JobSpecEnvelope> {
    let spec = harw_job_core::JobSpec::command("/bin/sh")
        .arg("-c")
        .arg(script)
        .build()
        .map_err(ctx("spec"))?;
    harw_job_core::JobSpecEnvelope::new(spec).map_err(ctx("envelope"))
}

async fn wait(handle: JobHandle) -> TestResult<JobResult> {
    tokio::time::timeout(LIMIT, handle.wait())
        .await
        .map_err(ctx("job did not finish in time"))?
        .map_err(ctx("wait"))
}

fn runs(fixture: &Fixture) -> TestResult<usize> {
    let text =
        std::fs::read_to_string(fixture.workspace().join("runs")).map_err(ctx("runs file"))?;
    Ok(text.lines().count())
}

#[tokio::test]
async fn lifecycle_happy_fail_retry_backoff_final() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("lifecycle-e2e")?;
    let coordinator = fixture.coordinator(&runner, policy(3, SignedDuration::from_millis(50))?)?;

    // Attempt 1: fails with exit 7. Attempt 2: sees the marker and succeeds.
    let script = "echo run >> runs; \
                  if [ -e marker ]; then echo done; else touch marker; exit 7; fi";
    let handle = coordinator
        .submit(sh(script)?)
        .await
        .map_err(ctx("submit"))?;
    let store = coordinator.store();
    let job_id = handle.id().clone();

    let result = wait(handle).await?;
    assert_eq!(result.state, LifecycleState::Succeeded, "{result:?}");
    assert_eq!(result.exit, Some(ExitOutcome::Exited(0)));
    assert_eq!(result.stdout, b"done\n");
    assert_eq!(result.reason, None);

    // Exactly two attempts ran: one failed, one succeeded after the backoff.
    assert_eq!(runs(&fixture)?, 2, "one retry, not more, not less");

    let stored = store.load(&job_id).map_err(ctx("load"))?;
    assert_eq!(stored.job.state, JobState::Completed);
    Ok(())
}

#[tokio::test]
async fn lifecycle_backoff_respects_the_attempt_cap() -> TestResult {
    let fixture = Fixture::new()?;
    let runner = runner("lifecycle-cap")?;
    let coordinator = fixture.coordinator(&runner, policy(2, SignedDuration::from_millis(20))?)?;

    let handle = coordinator
        .submit(sh("echo run >> runs; exit 5")?)
        .await
        .map_err(ctx("submit"))?;
    let job_id = handle.id().clone();
    let result = wait(handle).await?;
    assert_eq!(result.state, LifecycleState::Failed, "{result:?}");
    assert_eq!(result.exit, Some(ExitOutcome::Exited(5)));
    assert_eq!(runs(&fixture)?, 2, "max_attempts bounds the attempts");

    let store = coordinator.store();
    let stored = store.load(&job_id).map_err(ctx("load"))?;
    assert_eq!(stored.job.state, JobState::Failed);
    let reason = stored
        .completion
        .map(|completion| completion.outcome)
        .ok_or(TestError::Missing("completion"))?;
    assert!(matches!(
        reason,
        harw_job_core::JobOutcome::Failed { .. }
    ));
    Ok(())
}
