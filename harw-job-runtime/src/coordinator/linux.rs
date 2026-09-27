//! Linux executor (Job-Runtime-Doc §8–§13): `harw-job-linux` process groups
//! (pidfd authority, optional cgroup v2 job boundary) supervised by the
//! `harw-job-tokio` event loop.
//!
//! # Start order (Job-Runtime-Doc §9, §24)
//! 1. resource domain: create the job cgroup (if configured) with the
//!    requested limits;
//! 2. sandbox backend: plan it and predict the enforcement report;
//! 3. requirement check **before** anything runs
//!    ([`super::executor::check_requirement`]); the Landlock trampoline
//!    checks the full requirement itself and exits `126` without running
//!    the job;
//! 4. spawn as leader of a new process group → `pidfd_open` → cgroup
//!    attach (`LinuxJobGroup::spawn_in_cgroup`); the trampoline additionally
//!    joins the cgroup itself before `exec`;
//! 5. capture the [`LinuxRecoveryIdentity`] *after* the attach;
//! 6. adopt into Tokio (`AsyncFd` on the pidfd) and supervise.
//!
//! # Exit-status shim (optional)
//! pidfds do not survive a restart, and a reattached process is not our
//! child, so its exit status is unobservable. With
//! [`LinuxExecutorOptions::exit_status_dir`] the job runs under a tiny
//! `/bin/sh` wrapper that writes `$?` to `<dir>/<attempt>.status` when the
//! job ends; [`Executor::recorded_exit`] reads it back. The wrapper is the
//! primary process then (its identity is what recovery verifies); a job
//! killed by a signal is recorded by the shell as `128 + signal`.
//!
//! # Limitations
//! - A reattached process is supervised through its verified pidfd only
//!   (no process-group or cgroup handle is rebuilt), so termination of a
//!   recovered attempt signals the primary, not its descendants.
//! - Between spawn and cgroup attach the child runs for a few microseconds
//!   outside the cgroup unless the trampoline backend is used (it joins
//!   the cgroup itself before `exec`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use harw_job_core::{
    EnforcementState, ExitOutcome, JobSpec, SandboxProfileName, SandboxReport, SandboxRequirement,
};
use harw_job_linux::cgroup::{CgroupBackend, CgroupHandle, CgroupSpec, CgroupV2Fs};
use harw_job_linux::group::JobCgroup;
use harw_job_linux::{
    CgroupError, IdentityCheck, JobResources, LaunchError, LinuxJobGroup, LinuxRecoveryIdentity,
    ProcessError, RecoveryError, SandboxPolicy, TerminationPolicy,
};
use harw_job_tokio::{
    AsyncLinuxProcess, Canceller, ProcessEvent, SupervisedTarget, Supervisor, SupervisorConfig,
    SupervisorHandle,
};
use tokio::sync::mpsc;

use super::error::RuntimeError;
use super::executor::{
    AttemptContext, AttemptControl, AttemptEvent, AttemptEventSender, AttemptEvents, AttemptRun,
    Executor, Probe, StartedAttempt, check_requirement, requests_resource_limits,
    unsandboxed_report,
};

/// Shell used by the exit-status shim.
const SHIM_SHELL: &str = "/bin/sh";

/// The exit-status shim: `$1` is the status file, the rest is the job.
const SHIM_SCRIPT: &str = "status=\"$1\"; shift; \"$@\"; code=$?; \
                           printf '%s\\n' \"$code\" > \"$status\"; exit \"$code\"";

/// `argv[0]` of the shim shell (diagnostics in `ps`).
const SHIM_ARGV0: &str = "harw-job-shim";

/// Capacity of the attempt event channel.
const EVENT_CAPACITY: usize = 64;

/// Attempts to remove a job cgroup whose members are still exiting.
const CGROUP_REMOVE_ATTEMPTS: u32 = 50;

/// Sandbox backend of the [`LinuxExecutor`].
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub enum LinuxSandboxBackend {
    /// No sandbox: every sandbox dimension is reported `not_enforced`, so a
    /// [`SandboxRequirement::Required`] job is refused before it runs.
    #[default]
    None,
    /// The `harw-job-exec` trampoline applies `NO_NEW_PRIVS`, the capability
    /// drop and Landlock to itself, reports what it enforced, checks the
    /// requirement (exit `126` if unmet) and `exec`s the job.
    LandlockTrampoline {
        /// Absolute path of the `harw-job-exec` binary.
        trampoline: PathBuf,
        /// Private (`0700`) directory for plan and report files.
        plan_dir: PathBuf,
    },
    /// Bubblewrap through `harw-job-executor-bwrap` (feature `bwrap`).
    #[cfg(feature = "bwrap")]
    Bwrap(harw_job_executor_bwrap::BwrapExecutor),
}

/// Options of the [`LinuxExecutor`].
#[derive(Debug, Clone)]
pub struct LinuxExecutorOptions {
    /// Sandbox backend applied to jobs whose requirement is not
    /// [`SandboxRequirement::None`].
    pub sandbox: LinuxSandboxBackend,
    /// Delegated cgroup v2 directory; every attempt gets its own child
    /// cgroup with the requested limits. `None`: process group only.
    pub cgroup_root: Option<PathBuf>,
    /// Directory for exit-status files (enables the exit-status shim, see
    /// the module docs). `None`: no shim.
    pub exit_status_dir: Option<PathBuf>,
    /// How to terminate a job on cancel, deadline or lease loss.
    pub termination: TerminationPolicy,
    /// How long output is drained after the primary exited.
    pub drain_timeout: Duration,
}

impl Default for LinuxExecutorOptions {
    fn default() -> Self {
        Self {
            sandbox: LinuxSandboxBackend::None,
            cgroup_root: None,
            exit_status_dir: None,
            termination: TerminationPolicy::default(),
            drain_timeout: Duration::from_secs(2),
        }
    }
}

/// Linux [`Executor`] (see the module docs).
#[derive(Debug, Default)]
pub struct LinuxExecutor {
    options: LinuxExecutorOptions,
    cgroup: Option<Arc<CgroupV2Fs>>,
}

/// What to exec, before it becomes a [`Command`].
struct Launch {
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(String, String)>,
    cwd: Option<PathBuf>,
}

impl Launch {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args).env_clear();
        for (name, value) in &self.env {
            command.env(name, value);
        }
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn wrap_in_shim(self, status: &Path) -> Self {
        let mut args: Vec<OsString> = vec![
            "-c".into(),
            SHIM_SCRIPT.into(),
            SHIM_ARGV0.into(),
            status.as_os_str().to_owned(),
            self.program,
        ];
        args.extend(self.args);
        Self {
            program: SHIM_SHELL.into(),
            args,
            env: self.env,
            cwd: self.cwd,
        }
    }
}

/// A prepared launch.
struct Prepared {
    launch: Launch,
    /// Report known before the body runs (`None` for the trampoline).
    report: Option<SandboxReport>,
    /// Trampoline report file to read after exit.
    trampoline_report: Option<PathBuf>,
    /// Trampoline plan file to discard on a failed spawn.
    trampoline_plan: Option<PathBuf>,
}

fn process_error(operation: &'static str, program: &str, error: ProcessError) -> RuntimeError {
    match error {
        ProcessError::Unsupported { operation } => RuntimeError::Unsupported {
            operation,
            detail: "the kernel lacks this primitive".to_owned(),
        },
        ProcessError::PermissionDenied { operation } => RuntimeError::PermissionDenied {
            operation,
            detail: "EPERM/EACCES".to_owned(),
        },
        ProcessError::Exhausted { operation } => RuntimeError::ResourceExhausted {
            operation,
            detail: "descriptors, memory or process slots exhausted".to_owned(),
        },
        ProcessError::AlreadyExited { pid } => RuntimeError::ProcessAlreadyExited { pid },
        ProcessError::Spawn(source) => RuntimeError::Spawn {
            program: program.to_owned(),
            source,
        },
        other => RuntimeError::Os {
            operation,
            detail: other.to_string(),
        },
    }
}

fn cgroup_error(error: CgroupError) -> RuntimeError {
    match error {
        CgroupError::Unsupported { feature } => RuntimeError::Unsupported {
            operation: "cgroup",
            detail: format!("missing interface {feature}"),
        },
        CgroupError::PermissionDenied { path } => RuntimeError::PermissionDenied {
            operation: "cgroup",
            detail: path,
        },
        CgroupError::Exhausted { path } => RuntimeError::ResourceExhausted {
            operation: "cgroup",
            detail: path,
        },
        other => RuntimeError::CgroupUnavailable {
            detail: other.to_string(),
        },
    }
}

fn recovery_error(error: RecoveryError) -> RuntimeError {
    match error {
        RecoveryError::IdentityMismatch { pid, reason } => RuntimeError::IdentityMismatch {
            pid,
            detail: reason.to_string(),
        },
        RecoveryError::AlreadyExited { pid } => RuntimeError::ProcessAlreadyExited { pid },
        RecoveryError::PermissionDenied { pid } => RuntimeError::PermissionDenied {
            operation: "inspect /proc",
            detail: format!("pid {pid}"),
        },
        RecoveryError::Process(error) => process_error("recover process", "", error),
        RecoveryError::Cgroup(error) => cgroup_error(error),
        other => RuntimeError::Os {
            operation: "recover process",
            detail: other.to_string(),
        },
    }
}

/// A cgroup child name for an attempt: `harw-job-<attempt>` with every
/// character outside `[A-Za-z0-9_.-]` replaced by `_`.
fn cgroup_name(ctx: &AttemptContext) -> String {
    let attempt: String = ctx
        .attempt_id
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
    format!("harw-job-{attempt}")
}

/// cgroup limits for a spec.
fn job_resources(spec: &JobSpec) -> JobResources {
    JobResources {
        memory_max: spec.resources.memory_max,
        cpu_weight: spec.resources.cpu_weight.map(u64::from),
        pids_max: spec.resources.pids_max.map(u64::from),
        ..JobResources::default()
    }
}

/// The weaker of two enforcement states.
fn weaker(a: EnforcementState, b: EnforcementState) -> EnforcementState {
    fn rank(state: EnforcementState) -> u8 {
        match state {
            EnforcementState::Enforced => 3,
            EnforcementState::Partial => 2,
            EnforcementState::NotEnforced => 1,
            EnforcementState::Unsupported => 0,
        }
    }
    if rank(a) <= rank(b) { a } else { b }
}

/// Removes a job cgroup after its job ended: kill stragglers, then retry
/// removal while members are still exiting.
async fn remove_cgroup(cgroup: JobCgroup) {
    let (backend, handle) = cgroup.into_parts();
    if let Err(error) = backend.kill(&handle) {
        tracing::debug!(cgroup = handle.proc_path(), %error, "cgroup kill before removal failed");
    }
    let mut handle = handle;
    for _ in 0..CGROUP_REMOVE_ATTEMPTS {
        let name = handle.name().to_owned();
        match backend.remove(handle) {
            Ok(()) => return,
            Err(CgroupError::Busy { .. }) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
                match backend.reopen(&name) {
                    Ok(reopened) => handle = reopened,
                    Err(error) => {
                        tracing::warn!(cgroup = name, %error, "cannot reopen busy job cgroup");
                        return;
                    }
                }
            }
            Err(error) => {
                tracing::warn!(cgroup = name, %error, "cannot remove job cgroup");
                return;
            }
        }
    }
    tracing::warn!("job cgroup still busy; left in place");
}

/// Supervisor-backed [`AttemptControl`].
struct SupervisorControl {
    canceller: Canceller,
}

impl AttemptControl for SupervisorControl {
    fn cancel(&self) {
        self.canceller.cancel();
    }
}

/// What the forwarding task does after the process ended.
struct AfterExit {
    trampoline_report: Option<PathBuf>,
    resource_limits: EnforcementState,
}

impl AfterExit {
    fn late_report(&self) -> Option<SandboxReport> {
        let path = self.trampoline_report.as_ref()?;
        let report = match harw_job_exec::read_report(path) {
            Ok(Some(report)) => Some(SandboxReport {
                // The trampoline only knows what its plan told it; without a
                // cgroup it cannot know the spec asked for limits.
                resource_limits: weaker(report.resource_limits, self.resource_limits),
                ..report
            }),
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "cannot read trampoline report");
                None
            }
        };
        if let Err(error) = std::fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!(path = %path.display(), %error, "cannot remove trampoline report");
            }
        }
        report
    }
}

/// Forwards supervisor events to the coordinator; after the supervisor
/// ended, cleans up the job cgroup and emits the final `Exited`.
async fn forward(
    handle: SupervisorHandle,
    mut events: mpsc::Receiver<ProcessEvent>,
    sender: AttemptEventSender,
    after: AfterExit,
) {
    let mut exit = ExitOutcome::Unknown;
    let mut listening = true;
    while let Some(event) = events.recv().await {
        let forwarded = match event {
            ProcessEvent::Stdout(chunk) => Some(AttemptEvent::Stdout(chunk.to_vec())),
            ProcessEvent::Stderr(chunk) => Some(AttemptEvent::Stderr(chunk.to_vec())),
            ProcessEvent::Exited(outcome) => {
                exit = outcome;
                None
            }
            ProcessEvent::SupervisorError(error) => Some(AttemptEvent::Error(error.to_string())),
            // Timeout / CancelRequested: the coordinator initiated them.
            _ => None,
        };
        if let (true, Some(event)) = (listening, forwarded) {
            // Keep draining even when nobody listens: the supervisor must
            // never block on a full channel.
            listening = sender.send(event).await;
        }
    }
    match handle.join().await {
        Ok(process) => {
            if let SupervisedTarget::Group(mut group) = process.into_target() {
                if let Some(cgroup) = group.take_cgroup() {
                    remove_cgroup(cgroup).await;
                }
            }
        }
        Err(error) => tracing::warn!(%error, "supervisor task failed"),
    }
    let sandbox = after.late_report();
    let _ = sender
        .send(AttemptEvent::Exited {
            outcome: exit,
            sandbox,
        })
        .await;
}

impl LinuxExecutor {
    /// Creates an executor; opens the delegated cgroup root if configured.
    ///
    /// # Errors
    /// [`RuntimeError::CgroupUnavailable`] (or a permission/unsupported
    /// variant) when the cgroup root cannot be used;
    /// [`RuntimeError::InvalidConfig`] for relative backend paths.
    pub fn new(options: LinuxExecutorOptions) -> Result<Self, RuntimeError> {
        if let LinuxSandboxBackend::LandlockTrampoline {
            trampoline,
            plan_dir,
        } = &options.sandbox
        {
            if !trampoline.is_absolute() || !plan_dir.is_absolute() {
                return Err(RuntimeError::InvalidConfig {
                    detail: "trampoline and plan_dir must be absolute paths".to_owned(),
                });
            }
        }
        if let Some(dir) = &options.exit_status_dir {
            if !dir.is_absolute() {
                return Err(RuntimeError::InvalidConfig {
                    detail: "exit_status_dir must be an absolute path".to_owned(),
                });
            }
        }
        let cgroup = match &options.cgroup_root {
            Some(root) => Some(Arc::new(CgroupV2Fs::open(root).map_err(cgroup_error)?)),
            None => None,
        };
        Ok(Self { options, cgroup })
    }

    /// The options.
    #[must_use]
    pub fn options(&self) -> &LinuxExecutorOptions {
        &self.options
    }

    fn status_path(&self, ctx: &AttemptContext) -> Option<PathBuf> {
        self.options
            .exit_status_dir
            .as_ref()
            .map(|dir| dir.join(format!("{}.status", ctx.attempt_id)))
    }

    fn create_cgroup(
        &self,
        spec: &JobSpec,
        ctx: &AttemptContext,
    ) -> Result<Option<(Arc<CgroupV2Fs>, CgroupHandle)>, RuntimeError> {
        let Some(backend) = &self.cgroup else {
            return Ok(None);
        };
        let handle = backend
            .create(&CgroupSpec {
                name: cgroup_name(ctx),
                resources: job_resources(spec),
            })
            .map_err(cgroup_error)?;
        Ok(Some((Arc::clone(backend), handle)))
    }

    fn prepare(
        &self,
        spec: &JobSpec,
        ctx: &AttemptContext,
        cgroup: Option<&(Arc<CgroupV2Fs>, CgroupHandle)>,
        resource_limits: EnforcementState,
    ) -> Result<Prepared, RuntimeError> {
        let workdir = ctx.workspace_root.join(spec.working_dir.as_str());
        // `SandboxRequirement::None`: no sandbox requested, run plainly.
        let sandboxed = spec.sandbox != SandboxRequirement::None;
        match (sandboxed, &self.options.sandbox) {
            (false, _) | (true, LinuxSandboxBackend::None) => Ok(Prepared {
                launch: Launch {
                    program: OsString::from(&spec.program),
                    args: spec.args.iter().map(OsString::from).collect(),
                    env: spec.env.clone(),
                    cwd: Some(workdir),
                },
                report: Some(unsandboxed_report(resource_limits)),
                trampoline_report: None,
                trampoline_plan: None,
            }),
            (
                true,
                LinuxSandboxBackend::LandlockTrampoline {
                    trampoline,
                    plan_dir,
                },
            ) => {
                // Lower bound known before spawning: the resource domain.
                if spec.sandbox == SandboxRequirement::Required
                    && resource_limits != EnforcementState::Enforced
                {
                    let report = SandboxReport {
                        resource_limits,
                        ..SandboxReport::uniform(EnforcementState::Enforced)
                    };
                    check_requirement(spec.sandbox, &report)?;
                }
                let policy = policy_for(spec.sandbox_profile, &ctx.workspace_root);
                let mut plan = harw_job_exec::ExecPlanV1::new(&spec.program)
                    .with_args(spec.args.iter().cloned())
                    .with_sandbox(policy, spec.sandbox);
                if let Some((backend, handle)) = cgroup {
                    plan = plan.with_cgroup(harw_job_exec::CgroupJoin {
                        root: backend.root_path().to_path_buf(),
                        relative: handle.name().to_owned(),
                        limits: Some(resource_limits),
                    });
                }
                let mut launch = harw_job_exec::TrampolineCommand::new(trampoline, &plan, plan_dir)
                    .map_err(|error| RuntimeError::Sandbox {
                        detail: error.to_string(),
                    })?;
                let command = launch.command_mut();
                let program = command.get_program().to_owned();
                let args = command.get_args().map(ToOwned::to_owned).collect();
                Ok(Prepared {
                    launch: Launch {
                        program,
                        args,
                        env: spec.env.clone(),
                        cwd: Some(workdir),
                    },
                    report: None,
                    trampoline_report: Some(launch.report_path().to_path_buf()),
                    trampoline_plan: Some(launch.plan_path().to_path_buf()),
                })
            }
            #[cfg(feature = "bwrap")]
            (true, LinuxSandboxBackend::Bwrap(bwrap)) => {
                let policy = policy_for(spec.sandbox_profile, &ctx.workspace_root);
                let plan = bwrap
                    .plan(spec, &policy, &ctx.workspace_root)
                    .map_err(|error| match error {
                        harw_job_executor_bwrap::BwrapExecutorError::Unsupported { .. } => {
                            RuntimeError::Unsupported {
                                operation: "bwrap sandbox",
                                detail: error.to_string(),
                            }
                        }
                        other => RuntimeError::Sandbox {
                            detail: other.to_string(),
                        },
                    })?;
                let report = SandboxReport {
                    resource_limits,
                    ..plan.report()
                };
                Ok(Prepared {
                    launch: Launch {
                        program: plan.executable().as_os_str().to_owned(),
                        args: plan.args().to_vec(),
                        // bwrap sets the job environment inside (--setenv).
                        env: Vec::new(),
                        cwd: None,
                    },
                    report: Some(report),
                    trampoline_report: None,
                    trampoline_plan: None,
                })
            }
        }
    }

    fn supervisor_config(&self) -> SupervisorConfig {
        SupervisorConfig {
            deadline: None,
            termination: self.options.termination,
            drain_timeout: self.options.drain_timeout,
            ..SupervisorConfig::default()
        }
    }

    fn supervise(
        &self,
        process: AsyncLinuxProcess,
        stdout: Option<std::process::ChildStdout>,
        stderr: Option<std::process::ChildStderr>,
        after: AfterExit,
    ) -> AttemptRun {
        let (handle, events) = Supervisor::spawn(process, stdout, stderr, self.supervisor_config());
        let control = Arc::new(SupervisorControl {
            canceller: handle.canceller(),
        });
        let (sender, attempt_events) = AttemptEvents::channel(EVENT_CAPACITY);
        tokio::spawn(forward(handle, events, sender, after));
        AttemptRun::new(attempt_events, control)
    }
}

/// The sandbox policy of a profile rooted at the workspace.
fn policy_for(profile: SandboxProfileName, workspace_root: &Path) -> SandboxPolicy {
    SandboxPolicy::from_profile(profile, workspace_root)
}

/// Kills and reaps a target that could not be adopted.
fn abandon(target: SupervisedTarget) {
    let policy = TerminationPolicy {
        grace_period: Duration::ZERO,
        ..TerminationPolicy::default()
    };
    match target {
        SupervisedTarget::Group(mut group) => {
            if let Err(error) = group.terminate(&policy) {
                tracing::warn!(%error, "cannot terminate unadoptable job");
            }
        }
        SupervisedTarget::Process(process) => {
            let _ = process.signal(harw_job_linux::SignalKind::Kill);
        }
    }
}

impl Executor for LinuxExecutor {
    type Identity = LinuxRecoveryIdentity;

    fn start(
        &self,
        spec: &JobSpec,
        ctx: &AttemptContext,
    ) -> Result<StartedAttempt<Self::Identity>, RuntimeError> {
        spec.validate()?;
        // 1. resource domain
        let cgroup = self.create_cgroup(spec, ctx)?;
        let resource_limits = if !requests_resource_limits(spec) {
            EnforcementState::Enforced
        } else {
            cgroup
                .as_ref()
                .and_then(|(_, handle)| handle.limits_enforcement())
                .unwrap_or(EnforcementState::NotEnforced)
        };
        let release = |cgroup: Option<(Arc<CgroupV2Fs>, CgroupHandle)>| {
            if let Some((backend, handle)) = cgroup {
                if let Err(error) = backend.remove(handle) {
                    tracing::warn!(%error, "cannot remove unused job cgroup");
                }
            }
        };
        // 2. + 3. sandbox plan and requirement check before anything runs
        let prepared = match self.prepare(spec, ctx, cgroup.as_ref(), resource_limits) {
            Ok(prepared) => prepared,
            Err(error) => {
                release(cgroup);
                return Err(error);
            }
        };
        if let Some(report) = &prepared.report {
            if let Err(error) = check_requirement(spec.sandbox, report) {
                release(cgroup);
                return Err(error);
            }
        }
        let status_path = self.status_path(ctx);
        let mut launch = prepared.launch;
        // `/proc/<pid>/exe` is only part of the identity when the primary is
        // our own shim, which never `exec`s. A job (or the trampoline) may
        // legitimately `exec`; start time and cgroup identify it then.
        let keep_executable = status_path.is_some();
        if let Some(status) = &status_path {
            if let Err(error) = std::fs::remove_file(status) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(path = %status.display(), %error, "cannot remove stale status file");
                }
            }
            launch = launch.wrap_in_shim(status);
        }
        // 4. spawn → pidfd → cgroup attach
        let mut command = launch.command();
        let spawned = match cgroup {
            Some((backend, handle)) => {
                let backend: Arc<dyn CgroupBackend> = backend;
                LinuxJobGroup::spawn_in_cgroup(&mut command, JobCgroup::new(backend, handle))
                    .map_err(|error| match error {
                        LaunchError::Process(error) => process_error("spawn", &spec.program, error),
                        LaunchError::Cgroup(error) => cgroup_error(error),
                        other => RuntimeError::Os {
                            operation: "spawn in cgroup",
                            detail: other.to_string(),
                        },
                    })
            }
            None => LinuxJobGroup::spawn(&mut command)
                .map_err(|error| process_error("spawn", &spec.program, error)),
        };
        let (group, stdio) = match spawned {
            Ok(spawned) => spawned,
            Err(error) => {
                if let Some(plan) = &prepared.trampoline_plan {
                    let _ = std::fs::remove_file(plan);
                }
                return Err(error);
            }
        };
        let pid = group.primary().pid();
        // 5. identity after attach
        let identity = match LinuxRecoveryIdentity::capture(
            group.primary(),
            ctx.runner_id.clone(),
            ctx.attempt_id.clone(),
        ) {
            Ok(mut identity) => {
                if !keep_executable {
                    identity.executable = None;
                }
                Some(identity)
            }
            Err(error) => {
                tracing::warn!(pid, %error, "cannot capture recovery identity; attempt will be unrecoverable");
                None
            }
        };
        // 6. async supervision
        let process = match AsyncLinuxProcess::from_group(group) {
            Ok(process) => process,
            Err(error) => {
                let detail = error.error.to_string();
                abandon(*error.target);
                return Err(RuntimeError::Os {
                    operation: "adopt process",
                    detail,
                });
            }
        };
        tracing::info!(pid, job = %ctx.job_id, attempt = %ctx.attempt_id, "attempt started");
        let run = self.supervise(
            process,
            stdio.stdout,
            stdio.stderr,
            AfterExit {
                trampoline_report: prepared.trampoline_report,
                resource_limits,
            },
        );
        Ok(StartedAttempt {
            identity,
            sandbox: prepared.report,
            pid: Some(pid),
            run,
        })
    }

    fn probe(&self, identity: &Self::Identity) -> Result<Probe, RuntimeError> {
        Ok(match identity.verify() {
            Ok(IdentityCheck::Alive) => Probe::Alive,
            Ok(IdentityCheck::Exited) => Probe::Exited,
            Ok(IdentityCheck::Mismatch(reason)) => Probe::Mismatch(reason.to_string()),
            Ok(other) => Probe::Unverifiable(format!("{other:?}")),
            Err(error) => Probe::Unverifiable(error.to_string()),
        })
    }

    fn reattach(
        &self,
        identity: &Self::Identity,
        ctx: &AttemptContext,
    ) -> Result<AttemptRun, RuntimeError> {
        let process = identity.open_verified().map_err(recovery_error)?;
        let process = AsyncLinuxProcess::new(process).map_err(|error| RuntimeError::Os {
            operation: "adopt recovered process",
            detail: error.to_string(),
        })?;
        tracing::info!(pid = identity.pid, job = %ctx.job_id, attempt = %ctx.attempt_id, "reattached verified process");
        Ok(self.supervise(
            process,
            None,
            None,
            AfterExit {
                trampoline_report: None,
                resource_limits: EnforcementState::NotEnforced,
            },
        ))
    }

    fn recorded_exit(&self, ctx: &AttemptContext) -> Option<ExitOutcome> {
        let path = self.status_path(ctx)?;
        let text = std::fs::read_to_string(&path).ok()?;
        // The shim terminates the line; without it the write was cut off.
        let line = text.strip_suffix('\n')?;
        line.trim().parse::<i32>().ok().map(ExitOutcome::Exited)
    }

    fn finished(&self, ctx: &AttemptContext) {
        if let Some(path) = self.status_path(ctx) {
            if let Err(error) = std::fs::remove_file(&path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::debug!(path = %path.display(), %error, "cannot remove status file");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Launch, cgroup_name, weaker};
    use crate::coordinator::executor::AttemptContext;
    use crate::test_support::{TestResult, ctx};
    use harw_job_core::{AttemptId, EnforcementState, RunnerId};
    use harw_types::WorkId;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    #[test]
    fn cgroup_names_are_single_safe_components() -> TestResult {
        let context = AttemptContext {
            job_id: WorkId::from_str("j"),
            attempt_id: AttemptId::new("job:1.x-e2").map_err(ctx("attempt"))?,
            runner_id: RunnerId::new("r").map_err(ctx("runner"))?,
            workspace_root: PathBuf::from("/"),
        };
        assert_eq!(cgroup_name(&context), "harw-job-job_1.x-e2");
        Ok(())
    }

    #[test]
    fn weaker_picks_the_lower_state() {
        use EnforcementState as S;
        assert_eq!(weaker(S::Enforced, S::NotEnforced), S::NotEnforced);
        assert_eq!(weaker(S::Partial, S::Enforced), S::Partial);
        assert_eq!(weaker(S::Unsupported, S::Partial), S::Unsupported);
    }

    #[test]
    fn shim_wraps_program_and_arguments() {
        let launch = Launch {
            program: "echo".into(),
            args: vec!["a b".into()],
            env: vec![("K".into(), "V".into())],
            cwd: None,
        }
        .wrap_in_shim(Path::new("/tmp/x.status"));
        assert_eq!(launch.program, OsString::from("/bin/sh"));
        assert_eq!(
            launch.args.get(2..),
            Some(
                &[
                    OsString::from("harw-job-shim"),
                    OsString::from("/tmp/x.status"),
                    OsString::from("echo"),
                    OsString::from("a b"),
                ][..]
            )
        );
        assert_eq!(launch.env, vec![("K".to_owned(), "V".to_owned())]);
    }
}
