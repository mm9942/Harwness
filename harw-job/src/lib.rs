//! `harw-job` — the public facade of the Harw job runtime
//! (Job-Runtime-Doc §16, Phase 9).
//!
//! The common case is small:
//!
//! ```no_run
//! # #[cfg(target_os = "linux")]
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use harw_job::{
//!     FsJobRecordStore, JobRuntime, JobSpec, LinuxExecutor, ResourceRequest, SandboxProfile,
//!     WorkspacePath,
//! };
//!
//! let store = FsJobRecordStore::create_ambient(std::path::Path::new("/var/lib/app/jobs"))?;
//! let runtime = JobRuntime::builder()
//!     .store(store)
//!     .executor(LinuxExecutor::default())
//!     .workspace_root("/srv/workspace")
//!     .build()?;
//!
//! let job = runtime
//!     .submit(
//!         JobSpec::command("cargo")
//!             .arg("test")
//!             .workspace(WorkspacePath::new("crates/app")?)
//!             .resources(ResourceRequest::default())
//!             .sandbox(SandboxProfile::WorkspaceBuild),
//!     )
//!     .await?;
//!
//! let outcome = job.wait().await?;
//! assert!(outcome.is_success());
//! # Ok(())
//! # }
//! ```
//!
//! The common user does not need to know pidfds, procfs, cgroup files,
//! Landlock ABIs or Tokio `AsyncFd`s. Experts use the lower crates directly
//! (`harw-job-core`, `harw-job-store`, `harw-job-linux`, `harw-job-tokio`,
//! `harw-job-runtime`), or [`JobRuntime::coordinator`].
//!
//! # Restartability
//! Give the runtime a stable [`JobRuntimeBuilder::runner_id`] and call
//! [`JobRuntime::recover`] after a restart: verified live processes are
//! reattached, everything else is finalized deterministically and no
//! unverified process is ever signalled (Job-Runtime-Doc §15). To learn the
//! exit status of jobs that end while the runtime is down, enable the
//! exit-status shim (`LinuxExecutorOptions::exit_status_dir`).
//!
//! # Host capability probe
//! `harw_job::HostReport::probe()` reports, without side effects, which
//! sandbox backends this host offers and the strongest [`SandboxReport`]
//! a job can get here — the input of runtime admission (PL-90).
//!
//! # Public API boundary
//! Only first-party types appear in this API (Job-Runtime-Doc §2.5).
//!
//! # Features
//! - `bwrap`: the Bubblewrap sandbox backend (`LinuxSandboxBackend::Bwrap`).

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::time::Duration;

pub use harw_job_core::{
    AttemptId, CancellationCause, EnforcementState, ExitOutcome, IdempotencyKey, JobScopeId,
    JobSpec, JobSpecBuilder, JobSpecEnvelope, LifecycleState, ResourceRequest, RunnerId,
    SandboxProfileName, SandboxReport, SandboxRequirement, SpecError, WorkspacePath,
};
#[cfg(target_os = "macos")]
pub use harw_job_runtime::coordinator::DarwinExecutor;
pub use harw_job_runtime::coordinator::{
    Coordinator, CoordinatorConfig, CoordinatorStore, Executor, JobHandle, JobResult,
    OutputCapture, RecoveredJob, RecoveryDecision, RuntimeError,
};
#[cfg(target_os = "linux")]
pub use harw_job_runtime::coordinator::{LinuxExecutor, LinuxExecutorOptions, LinuxSandboxBackend};
pub use harw_job_runtime::host::{HostFacts, HostLandlock, HostReport};
pub use harw_job_store::FsJobRecordStore;
pub use harw_types::WorkId;

/// The sandbox profile name, under the name Job-Runtime-Doc §16 uses.
pub type SandboxProfile = SandboxProfileName;

/// A submitted job (see [`JobHandle`]).
pub type Job = JobHandle;

/// Anything [`JobRuntime::submit`] accepts: a [`JobSpec`], its builder, or
/// a versioned [`JobSpecEnvelope`].
pub trait IntoJobSpec {
    /// Validates and wraps the spec.
    ///
    /// # Errors
    /// [`RuntimeError::InvalidSpec`].
    fn into_envelope(self) -> Result<JobSpecEnvelope, RuntimeError>;
}

impl IntoJobSpec for JobSpec {
    fn into_envelope(self) -> Result<JobSpecEnvelope, RuntimeError> {
        Ok(JobSpecEnvelope::new(self)?)
    }
}

impl IntoJobSpec for JobSpecBuilder {
    fn into_envelope(self) -> Result<JobSpecEnvelope, RuntimeError> {
        Ok(JobSpecEnvelope::new(self.build()?)?)
    }
}

impl IntoJobSpec for JobSpecEnvelope {
    fn into_envelope(self) -> Result<JobSpecEnvelope, RuntimeError> {
        // Re-validate: an envelope may come from the wire.
        let spec = self.into_spec()?;
        Ok(JobSpecEnvelope::new(spec)?)
    }
}

/// Placeholder for a builder part that has not been set yet.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unset;

/// Builder of a [`JobRuntime`]; `store` and `executor` are required (the
/// type system enforces it).
#[derive(Debug)]
#[must_use]
pub struct JobRuntimeBuilder<S, E> {
    store: S,
    executor: E,
    runner_id: Option<RunnerId>,
    workspace_root: Option<PathBuf>,
    lease_ttl: Option<Duration>,
    output_capture: Option<OutputCapture>,
}

impl<S, E> JobRuntimeBuilder<S, E> {
    /// Sets the job store (e.g. [`FsJobRecordStore`]).
    pub fn store<S2: CoordinatorStore>(self, store: S2) -> JobRuntimeBuilder<S2, E> {
        JobRuntimeBuilder {
            store,
            executor: self.executor,
            runner_id: self.runner_id,
            workspace_root: self.workspace_root,
            lease_ttl: self.lease_ttl,
            output_capture: self.output_capture,
        }
    }

    /// Sets the executor (e.g. `LinuxExecutor::default()`).
    pub fn executor<E2: Executor>(self, executor: E2) -> JobRuntimeBuilder<S, E2> {
        JobRuntimeBuilder {
            store: self.store,
            executor,
            runner_id: self.runner_id,
            workspace_root: self.workspace_root,
            lease_ttl: self.lease_ttl,
            output_capture: self.output_capture,
        }
    }

    /// Sets the runner id. Keep it stable across restarts so
    /// [`JobRuntime::recover`] finds this runner's jobs. Default:
    /// `runner-<pid>` (not stable).
    pub fn runner_id(mut self, runner_id: RunnerId) -> Self {
        self.runner_id = Some(runner_id);
        self
    }

    /// Sets the absolute workspace root that `JobSpec::working_dir` is
    /// relative to. Default: the current directory.
    pub fn workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    /// Sets the lease TTL (default 30 s; heartbeats run at TTL/3).
    pub fn lease_ttl(mut self, ttl: Duration) -> Self {
        self.lease_ttl = Some(ttl);
        self
    }

    /// Sets how much stdout/stderr a [`JobResult`] keeps per stream: the
    /// first `head_bytes` and the last `tail_bytes` (default
    /// [`OutputCapture::default`], 16 KiB head + 64 KiB tail).
    pub fn output_capture(mut self, capture: OutputCapture) -> Self {
        self.output_capture = Some(capture);
        self
    }
}

impl<S: CoordinatorStore, E: Executor> JobRuntimeBuilder<S, E> {
    /// Builds the runtime.
    ///
    /// # Errors
    /// [`RuntimeError::InvalidConfig`] for a relative workspace root, a
    /// lease TTL below the minimum, or an unusable default runner id /
    /// current directory.
    pub fn build(self) -> Result<JobRuntime<S, E>, RuntimeError> {
        let runner_id = match self.runner_id {
            Some(runner_id) => runner_id,
            None => RunnerId::new(format!("runner-{}", std::process::id())).map_err(|error| {
                RuntimeError::InvalidConfig {
                    detail: format!("default runner id: {error}"),
                }
            })?,
        };
        let workspace_root = match self.workspace_root {
            Some(root) => root,
            None => std::env::current_dir().map_err(|error| RuntimeError::InvalidConfig {
                detail: format!("current directory as workspace root: {error}"),
            })?,
        };
        let mut config = CoordinatorConfig::new(runner_id, workspace_root);
        if let Some(ttl) = self.lease_ttl {
            config.lease_ttl = ttl;
        }
        if let Some(capture) = self.output_capture {
            config.output_capture = capture;
        }
        Ok(JobRuntime {
            coordinator: Coordinator::new(self.store, self.executor, config)?,
        })
    }
}

/// The job runtime: submit jobs, wait for them, recover after a restart.
#[derive(Debug)]
pub struct JobRuntime<S, E> {
    coordinator: Coordinator<S, E>,
}

impl<S, E> Clone for JobRuntime<S, E> {
    fn clone(&self) -> Self {
        Self {
            coordinator: self.coordinator.clone(),
        }
    }
}

impl JobRuntime<Unset, Unset> {
    /// Starts building a runtime.
    pub fn builder() -> JobRuntimeBuilder<Unset, Unset> {
        JobRuntimeBuilder {
            store: Unset,
            executor: Unset,
            runner_id: None,
            workspace_root: None,
            lease_ttl: None,
            output_capture: None,
        }
    }
}

impl<S: CoordinatorStore, E: Executor> JobRuntime<S, E> {
    /// Submits a job and starts it. Must be called within a Tokio runtime.
    ///
    /// # Errors
    /// [`RuntimeError::InvalidSpec`] or store errors while recording and
    /// claiming the job. Execution outcomes come from [`JobHandle::wait`].
    pub async fn submit(&self, spec: impl IntoJobSpec) -> Result<Job, RuntimeError> {
        self.coordinator.submit(spec.into_envelope()?).await
    }

    /// Recovers this runner's running jobs after a restart (see
    /// [`Coordinator::recover`]).
    ///
    /// # Errors
    /// Store errors while listing the recovery set.
    pub async fn recover(&self) -> Result<Vec<RecoveredJob>, RuntimeError> {
        let runner = self.coordinator.config().runner_id.clone();
        self.coordinator.recover(&runner).await
    }

    /// Requests cancellation of an active job. Returns whether it was
    /// active in this runtime.
    pub fn cancel(&self, job: &WorkId) -> bool {
        self.coordinator.cancel(job)
    }

    /// This runtime's runner id.
    #[must_use]
    pub fn runner_id(&self) -> &RunnerId {
        &self.coordinator.config().runner_id
    }

    /// The underlying coordinator (expert API).
    #[must_use]
    pub fn coordinator(&self) -> &Coordinator<S, E> {
        &self.coordinator
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{
        EnforcementState, FsJobRecordStore, HostReport, IntoJobSpec, JobRuntime, JobSpec,
        LifecycleState, LinuxExecutor, RunnerId, RuntimeError, SandboxProfile, SpecError,
        WorkspacePath,
    };
    use std::fmt;
    use std::time::Duration;

    /// Test failure (no panics, Bible R087/R165/R182).
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

    #[tokio::test]
    async fn facade_runs_a_job_end_to_end() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(workspace.join("app")).map_err(ctx("workspace"))?;
        let store =
            FsJobRecordStore::create_ambient(&dir.path().join("jobs")).map_err(ctx("store"))?;
        let runtime = JobRuntime::builder()
            .store(store)
            .executor(LinuxExecutor::default())
            .runner_id(RunnerId::new("facade-runner").map_err(ctx("runner"))?)
            .workspace_root(&workspace)
            .lease_ttl(Duration::from_secs(3))
            .build()
            .map_err(ctx("build"))?;
        let job = runtime
            .submit(
                JobSpec::command("/bin/sh")
                    .arg("-c")
                    .arg("echo from-facade")
                    .workspace(WorkspacePath::new("app").map_err(ctx("path"))?)
                    .sandbox(SandboxProfile::WorkspaceBuild),
            )
            .await
            .map_err(ctx("submit"))?;
        let outcome = tokio::time::timeout(Duration::from_secs(30), job.wait())
            .await
            .map_err(ctx("timeout"))?
            .map_err(ctx("wait"))?;
        assert!(outcome.is_success(), "{outcome:?}");
        assert_eq!(outcome.state, LifecycleState::Succeeded);
        assert_eq!(outcome.stdout, b"from-facade\n");
        assert!(runtime.recover().await.map_err(ctx("recover"))?.is_empty());
        Ok(())
    }

    #[test]
    fn builder_rejects_a_relative_workspace_root() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FsJobRecordStore::create_ambient(dir.path()).map_err(ctx("store"))?;
        let built = JobRuntime::builder()
            .store(store)
            .executor(LinuxExecutor::default())
            .workspace_root("relative/root")
            .build();
        assert!(matches!(built, Err(RuntimeError::InvalidConfig { .. })));
        Ok(())
    }

    #[test]
    fn host_report_is_reachable_through_the_facade() {
        let report = HostReport::probe();
        assert_eq!(report.target_os, "linux");
        // NO_NEW_PRIVS and the capability drop are always possible on Linux.
        assert_eq!(report.best_report.no_new_privs, EnforcementState::Enforced);
        assert_eq!(report.best_report.capabilities, EnforcementState::Enforced);
    }

    #[test]
    fn invalid_specs_are_rejected_before_submission() {
        assert!(matches!(
            JobSpec::command("").into_envelope(),
            Err(RuntimeError::InvalidSpec(SpecError::EmptyProgram))
        ));
    }
}
