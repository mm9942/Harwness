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
//! # Bubblewrap ∘ trampoline (PL-97 E1)
//! [`LinuxSandboxBackend::BwrapTrampoline`] chains two trampoline stages
//! around `bwrap` (feature `bwrap`):
//!
//! ```text
//! harw-job-exec --plan <attempt>/outer.json          outer stage, process-group leader
//!   CgroupJoin::Attach (first) → rlimits → exec bwrap   no Landlock, no sandbox
//!   bwrap --unshare-user --unshare-ipc --unshare-pid --unshare-net --unshare-uts …
//!         --ro-bind <harw-job-exec> /run/harw-job/exec
//!         --bind <attempt> /run/harw-job/attempt -- [relay prefix]
//!     /run/harw-job/exec --plan /run/harw-job/attempt/inner.json --report …/report.json
//!       CgroupJoin::Verify → rlimits → NO_NEW_PRIVS → capability drop →
//!       Landlock (policy mapped to the namespace view) → exec <program>
//! ```
//!
//! Because the outer stage joins the job cgroup before it `exec`s `bwrap`,
//! `bwrap` and every descendant are born inside the cgroup: there is no
//! spawn-to-attach window, and the inner stage's verification (the
//! sandbox keeps the caller's cgroup namespace) proves it. The outer
//! attach from the supervisor (`spawn_in_cgroup`) stays as before and is
//! a no-op for the already joined process. The inner stage re-applies the
//! same rlimits (idempotent) so its report states them. The attempt
//! report is `harw_job_exec::merge_reports(bwrap predicted, inner report)`
//! with the resource dimension capped by the cgroup domain, known after the
//! job ran. A `Required` job is checked against the namespace layer and the
//! resource domain before anything runs; the inner stage checks the rest
//! itself (exit `126`). Each attempt gets a private directory below the
//! backend's `plan_dir` for both plans and the report; it is removed after
//! the job ended or a failed start. Landlock inside keeps the job out of
//! that directory; the report is read only if it is still a regular file.
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
//! # Recovery (Job-Runtime-Doc §15)
//! A reattached process is supervised through its verified pidfd. When its
//! persisted identity names the attempt's own job cgroup
//! (`harw-job-<attempt>`), the executor reopens that cgroup beneath its
//! delegated root ([`CgroupBackend::reopen`]) and accepts it only if the
//! reopened path equals the persisted one. After the primary ended (on its
//! own, or through cancel, deadline or lease loss) the whole cgroup is
//! killed — re-verified by
//! [`LinuxRecoveryIdentity::kill_job_verified`] — and removed, so no
//! descendant survives. If the cgroup cannot be reopened (no cgroup root
//! configured, reopen failure, path mismatch), termination falls back to
//! the primary process only; the executor logs that and reports it as the
//! first [`AttemptEvent::Error`] of the attempt, which the coordinator
//! records in the attempt's reason.
//!
//! # Limitations
//! - A recovered attempt's process group is not rebuilt: the graceful
//!   termination signal reaches the primary only; descendants are killed
//!   with the job cgroup (when it could be reopened).
//! - Between spawn and cgroup attach the child runs for a few microseconds
//!   outside the cgroup unless a trampoline backend is used (it joins the
//!   cgroup itself before `exec`).
//! - A job cgroup whose last member keeps running past `remove_cgroup`'s
//!   retry budget is not removed there and then; it is recorded instead of
//!   only logged. Once the first attempt starts, and only when a cgroup
//!   root is configured, a background task retries every recorded leftover
//!   on a fixed interval; [`LinuxExecutor::sweep_leftover_cgroups`] is also
//!   callable directly (e.g. from a test, or a caller that wants a sweep on
//!   its own schedule instead).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(feature = "bwrap")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
use crate::host::BackendKind;

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

/// How often the leftover-cgroup background task (spawned lazily on the
/// first attempt, see [`LinuxExecutor::ensure_leftover_sweep`]) retries
/// every cgroup [`remove_cgroup`] could not remove.
const LEFTOVER_SWEEP_INTERVAL: Duration = Duration::from_secs(30);

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
    /// Bubblewrap around the `harw-job-exec` trampoline (feature `bwrap`,
    /// PL-97 E1): an outer trampoline joins the job cgroup and applies the
    /// rlimits, then `exec`s `bwrap`; inside the namespace the trampoline
    /// verifies the cgroup and applies `NO_NEW_PRIVS`, the capability drop
    /// and Landlock planned against the namespace view (module docs).
    ///
    /// Requires Landlock ABI 5 or later (`select_backend` only picks it
    /// then), with actual full enforcement checked before exec even for
    /// best-effort jobs: Landlock keeps the job out of its attempt
    /// directory, which is bound read-write for the plan and the report.
    #[cfg(feature = "bwrap")]
    BwrapTrampoline {
        /// The trusted `bwrap` (and optional proxy relay).
        bwrap: harw_job_executor_bwrap::BwrapExecutor,
        /// Absolute path of the `harw-job-exec` binary (outer stage, and
        /// bound read-only into the sandbox for the inner stage).
        trampoline: PathBuf,
        /// Private (`0700`) directory; every attempt gets its own
        /// subdirectory for both plans and the report.
        plan_dir: PathBuf,
    },
}

impl LinuxSandboxBackend {
    /// The kind of this backend (for selection reports and logs).
    #[must_use]
    pub fn kind(&self) -> BackendKind {
        match self {
            Self::None => BackendKind::None,
            Self::LandlockTrampoline { .. } => BackendKind::LandlockTrampoline,
            #[cfg(feature = "bwrap")]
            Self::Bwrap(_) => BackendKind::Bwrap,
            #[cfg(feature = "bwrap")]
            Self::BwrapTrampoline { .. } => BackendKind::BwrapTrampoline,
        }
    }
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
    /// Start unsandboxed jobs in a new session (no controlling terminal, so
    /// `open("/dev/tty")` fails instead of stopping the job) through
    /// `setsid(1)` when one is available; otherwise they lead their own
    /// process group as usual.
    pub new_session: bool,
}

impl Default for LinuxExecutorOptions {
    fn default() -> Self {
        Self {
            sandbox: LinuxSandboxBackend::None,
            cgroup_root: None,
            exit_status_dir: None,
            termination: TerminationPolicy::default(),
            drain_timeout: Duration::from_secs(2),
            new_session: false,
        }
    }
}

/// `setsid(1)` at a fixed path, else on `PATH` (once per process).
fn resolve_setsid() -> Option<&'static Path> {
    static SETSID: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    SETSID
        .get_or_init(|| {
            let fixed = ["/usr/bin/setsid", "/bin/setsid", "/usr/local/bin/setsid"];
            if let Some(found) = fixed.iter().map(Path::new).find(|p| p.is_file()) {
                return Some(found.to_path_buf());
            }
            let path = std::env::var_os("PATH")?;
            std::env::split_paths(&path)
                .map(|dir| dir.join("setsid"))
                .find(|candidate| candidate.is_file())
        })
        .as_deref()
}

/// Linux [`Executor`] (see the module docs).
#[derive(Debug, Default)]
pub struct LinuxExecutor {
    options: LinuxExecutorOptions,
    cgroup: Option<Arc<CgroupV2Fs>>,
    /// Job cgroups [`remove_cgroup`] could not remove within its retry
    /// budget; retried by [`LinuxExecutor::sweep_leftover_cgroups`] instead
    /// of leaking silently for the life of the host.
    leftovers: LeftoverRegistry,
    /// Guards [`LinuxExecutor::ensure_leftover_sweep`] so the background
    /// sweep task is spawned at most once.
    sweep_spawned: AtomicBool,
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

    /// `setsid <program> <args>`: in place (same PID) when the caller is not a
    /// group leader, which is why the spawn must not make it one.
    fn wrap_in_session(self, setsid: &Path) -> Self {
        let mut args: Vec<OsString> = vec!["--wait".into(), self.program];
        args.extend(self.args);
        Self {
            program: setsid.as_os_str().to_owned(),
            args,
            env: self.env,
            cwd: self.cwd,
        }
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
    /// Predicted report of the namespace layer, merged with the trampoline
    /// report after exit (`BwrapTrampoline`).
    merge_with: Option<SandboxReport>,
    /// Per-attempt directory to remove after exit or a failed start
    /// (`BwrapTrampoline`).
    attempt_dir: Option<PathBuf>,
}

impl Prepared {
    /// A launch without trampoline files.
    fn plain(launch: Launch, report: SandboxReport) -> Self {
        Self {
            launch,
            report: Some(report),
            trampoline_report: None,
            trampoline_plan: None,
            merge_with: None,
            attempt_dir: None,
        }
    }

    /// Removes the files of a launch that never started.
    fn discard_files(&self) {
        if let Some(plan) = &self.trampoline_plan {
            let _ = std::fs::remove_file(plan);
        }
        if let Some(dir) = &self.attempt_dir {
            remove_attempt_dir(dir);
        }
    }
}

/// File name of the outer trampoline plan in an attempt directory.
#[cfg(feature = "bwrap")]
const OUTER_PLAN_FILE: &str = "outer.json";

/// Attempts to find an unused attempt-directory name before giving up.
#[cfg(feature = "bwrap")]
const ATTEMPT_DIR_ATTEMPTS: u32 = 16;

/// Per-process sequence number for attempt-directory names.
#[cfg(feature = "bwrap")]
static ATTEMPT_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Creates a fresh `0700` directory `<plan_dir>/harw-job-<attempt>.<pid>.<seq>`.
#[cfg(feature = "bwrap")]
fn create_attempt_dir(plan_dir: &Path, ctx: &AttemptContext) -> Result<PathBuf, RuntimeError> {
    use std::os::unix::fs::DirBuilderExt as _;
    let stem = cgroup_name(ctx);
    let mut last = plan_dir.to_path_buf();
    for _ in 0..ATTEMPT_DIR_ATTEMPTS {
        let sequence = ATTEMPT_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = plan_dir.join(format!("{stem}.{}.{sequence}", std::process::id()));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => last = dir,
            Err(error) => {
                return Err(RuntimeError::Sandbox {
                    detail: format!("attempt directory {}: {error}", dir.display()),
                });
            }
        }
    }
    Err(RuntimeError::Sandbox {
        detail: format!(
            "no unused attempt directory name (last tried {})",
            last.display()
        ),
    })
}

/// Removes an attempt directory and everything in it. Best effort.
fn remove_attempt_dir(dir: &Path) {
    if let Err(error) = std::fs::remove_dir_all(dir) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(path = %dir.display(), %error, "cannot remove attempt directory");
        }
    }
}

/// Writes `plan` as a new `0600` file (the trampoline refuses anything
/// else).
#[cfg(feature = "bwrap")]
fn write_plan(path: &Path, plan: &harw_job_exec::ExecPlanV1) -> Result<(), RuntimeError> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let sandbox_error = |detail: String| RuntimeError::Sandbox { detail };
    let bytes = plan
        .to_json()
        .map_err(|error| sandbox_error(error.to_string()))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| sandbox_error(format!("plan file {}: {error}", path.display())))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| sandbox_error(format!("plan file {}: {error}", path.display())))
}

/// The job cgroup as the chain plans need it.
#[cfg(feature = "bwrap")]
#[derive(Debug, Clone, Copy)]
struct ChainCgroup<'a> {
    /// The delegated root (`CgroupJoin::root`).
    root: &'a Path,
    /// The job cgroup's child name (`CgroupJoin::relative`).
    name: &'a str,
    /// `CgroupHandle::proc_path` (`CgroupVerify::expected_proc_path`).
    proc_path: &'a str,
}

/// The two trampoline plans of a `BwrapTrampoline` launch.
#[cfg(feature = "bwrap")]
#[derive(Debug)]
struct ChainPlans {
    /// Outer stage: join the cgroup, rlimits, `exec` `bwrap`.
    outer: harw_job_exec::ExecPlanV1,
    /// Inner stage: verify the cgroup, rlimits, sandbox, `exec` the job.
    inner: harw_job_exec::ExecPlanV1,
    /// Predicted report of the namespace layer.
    bwrap_report: SandboxReport,
    /// Policy paths without a rule inside the namespace.
    unmapped: Vec<PathBuf>,
    /// Policy paths `bwrap` deliberately does not bind.
    withheld: Vec<PathBuf>,
}

/// Maps a planning error of the Bubblewrap executor.
#[cfg(feature = "bwrap")]
fn bwrap_error(error: harw_job_executor_bwrap::BwrapExecutorError) -> RuntimeError {
    match error {
        harw_job_executor_bwrap::BwrapExecutorError::Unsupported { .. } => {
            RuntimeError::Unsupported {
                operation: "bwrap sandbox",
                detail: error.to_string(),
            }
        }
        other => RuntimeError::Sandbox {
            detail: other.to_string(),
        },
    }
}

/// Plans both stages of a `BwrapTrampoline` launch (module docs). Pure
/// apart from canonicalizing the workspace root.
#[cfg(feature = "bwrap")]
fn chain_plans(
    bwrap: &harw_job_executor_bwrap::BwrapExecutor,
    mount: &harw_job_executor_bwrap::TrampolineMount,
    spec: &JobSpec,
    workspace_root: &Path,
    cgroup: Option<ChainCgroup<'_>>,
    resource_limits: EnforcementState,
) -> Result<ChainPlans, RuntimeError> {
    let policy = policy_for(spec.sandbox_profile, workspace_root);
    let plan = bwrap
        .plan_with_trampoline(spec, &policy, workspace_root, mount)
        .map_err(bwrap_error)?;
    // The outer plan is JSON: the bwrap argv must be UTF-8.
    let bwrap_args = plan
        .bwrap()
        .args()
        .iter()
        .map(|arg| {
            arg.to_str()
                .map(str::to_owned)
                .ok_or_else(|| RuntimeError::Sandbox {
                    detail: format!("bwrap argument {arg:?} is not valid UTF-8"),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut outer = harw_job_exec::ExecPlanV1::new(plan.bwrap().executable()).with_args(bwrap_args);
    let mut inner = harw_job_exec::ExecPlanV1::new(&spec.program)
        .with_args(spec.args.iter().cloned())
        .with_sandbox(plan.inner_policy().clone(), spec.sandbox);
    if spec.resources.requests_rlimits() {
        let rlimits = rlimit_set(spec);
        outer = outer.with_rlimits(rlimits.clone());
        inner = inner.with_rlimits(rlimits);
    }
    if let Some(cgroup) = cgroup {
        outer = outer.with_cgroup(harw_job_exec::CgroupJoin {
            root: cgroup.root.to_path_buf(),
            relative: cgroup.name.to_owned(),
            limits: Some(resource_limits),
        });
        inner = inner.with_cgroup(harw_job_exec::CgroupVerify {
            expected_proc_path: PathBuf::from(cgroup.proc_path),
            limits: Some(resource_limits),
        });
    }
    Ok(ChainPlans {
        outer,
        inner,
        bwrap_report: plan.bwrap().report(),
        unmapped: plan.unmapped_paths().to_vec(),
        withheld: plan.withheld_paths().to_vec(),
    })
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

/// A job cgroup that stayed busy past every [`remove_cgroup`] attempt.
/// Tracked so [`LinuxExecutor::sweep_leftover_cgroups`] can retry it later
/// instead of only logging the leak: no other code path in this crate
/// sweeps a delegated root's stale `harw-job-*` children (unlike
/// `harw-job-store`'s temp-file sweep, there is no cgroup equivalent).
struct LeftoverCgroup {
    backend: Arc<dyn CgroupBackend>,
    name: String,
}

impl std::fmt::Debug for LeftoverCgroup {
    // `CgroupBackend` is not `Debug`; only the name is diagnostic anyway.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeftoverCgroup")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Job cgroups [`remove_cgroup`] could not remove, retried by
/// [`LinuxExecutor::sweep_leftover_cgroups`].
type LeftoverRegistry = Arc<Mutex<Vec<LeftoverCgroup>>>;

/// Records a job cgroup in `leftovers` (when given) so
/// [`LinuxExecutor::sweep_leftover_cgroups`] retries it later instead of
/// letting the caller's failure discard it silently.
fn push_leftover(
    leftovers: Option<&LeftoverRegistry>,
    backend: Arc<dyn CgroupBackend>,
    name: String,
) {
    let Some(registry) = leftovers else {
        return;
    };
    match registry.lock() {
        Ok(mut registry) => registry.push(LeftoverCgroup { backend, name }),
        Err(error) => {
            tracing::warn!(%error, "leftover cgroup registry poisoned; leak untracked");
        }
    }
}

/// Removes a job cgroup after its job ended: kill stragglers, then retry
/// removal while members are still exiting. If every attempt still finds it
/// busy, the cgroup is recorded in `leftovers` (when given) instead of only
/// logged, so [`LinuxExecutor::sweep_leftover_cgroups`] can retry it once
/// its last member has actually exited.
async fn remove_cgroup(cgroup: JobCgroup, leftovers: Option<&LeftoverRegistry>) {
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
                        // Same leak class as exhaustion below: still busy,
                        // just discovered while trying to reopen it instead
                        // of while removing it.
                        push_leftover(leftovers, backend, name);
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
    let name = handle.name().to_owned();
    tracing::warn!(cgroup = name, "job cgroup still busy; left in place");
    push_leftover(leftovers, backend, name);
}

/// Core of [`LinuxExecutor::sweep_leftover_cgroups`]: a free function (not
/// a method) so the background task
/// [`LinuxExecutor::ensure_leftover_sweep`] spawns can run it against a
/// cloned registry without borrowing the executor.
async fn sweep_registry(leftovers: &LeftoverRegistry) {
    let pending = match leftovers.lock() {
        Ok(mut leftovers) => std::mem::take(&mut *leftovers),
        Err(error) => {
            tracing::warn!(%error, "leftover cgroup registry poisoned; sweep skipped");
            return;
        }
    };
    for leftover in pending {
        match leftover.backend.reopen(&leftover.name) {
            Ok(handle) => {
                remove_cgroup(
                    JobCgroup::new(Arc::clone(&leftover.backend), handle),
                    Some(leftovers),
                )
                .await;
            }
            // Gone already: removed by an earlier sweep, or the kernel
            // dropped it once its last member actually exited.
            Err(CgroupError::NotFound { .. }) => {}
            Err(error) => {
                tracing::warn!(
                    cgroup = leftover.name,
                    %error,
                    "cannot reopen leftover job cgroup"
                );
                if let Ok(mut leftovers) = leftovers.lock() {
                    leftovers.push(leftover);
                }
            }
        }
    }
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
    /// Predicted namespace-layer report to merge the trampoline report
    /// into (`BwrapTrampoline`).
    merge_with: Option<SandboxReport>,
    /// Per-attempt directory removed after the report was read.
    attempt_dir: Option<PathBuf>,
    resource_limits: EnforcementState,
    /// Job cgroup of a recovered attempt, killed and removed after exit.
    recovered_cgroup: Option<RecoveredCgroup>,
    /// Supervision note reported as the first event (recovery fallback).
    note: Option<String>,
    /// Registry for a cgroup [`remove_cgroup`] could not remove; shared
    /// with the owning [`LinuxExecutor`] so
    /// [`LinuxExecutor::sweep_leftover_cgroups`] can retry it later.
    leftovers: LeftoverRegistry,
}

/// The job cgroup of a recovered attempt, reopened and path-verified.
struct RecoveredCgroup {
    backend: Arc<dyn CgroupBackend>,
    handle: CgroupHandle,
    identity: LinuxRecoveryIdentity,
}

impl RecoveredCgroup {
    /// Kills every remaining member (after re-verifying that the handle is
    /// the persisted cgroup) and removes the cgroup.
    async fn kill_and_remove(self, leftovers: &LeftoverRegistry) {
        let killed = self
            .identity
            .kill_job_verified(&*self.backend, &self.handle);
        match killed {
            Ok(_) => {
                remove_cgroup(JobCgroup::new(self.backend, self.handle), Some(leftovers)).await;
            }
            Err(error) => {
                // Not killed: same leak class as a job cgroup `remove_cgroup`
                // could not remove, so it is recorded the same way instead of
                // only logged; the next sweep retries the kill too (it goes
                // through `remove_cgroup`, which kills before it removes).
                let name = self.handle.name().to_owned();
                tracing::warn!(
                    cgroup = self.handle.proc_path(),
                    %error,
                    "recovered job cgroup was not killed; queued for a later sweep retry"
                );
                push_leftover(Some(leftovers), self.backend, name);
            }
        }
    }
}

/// What recovery can do about a recovered attempt's job cgroup.
#[derive(Debug, PartialEq, Eq)]
enum CgroupRecovery {
    /// The attempt ran without a job cgroup: process-only as before.
    NotUsed,
    /// The job cgroup was reopened; its path equals the persisted one.
    Reopened(CgroupHandle),
    /// The attempt had a job cgroup that cannot be controlled now;
    /// termination falls back to the primary process (the detail says why).
    ProcessOnly(String),
}

/// Reopens the job cgroup a recovered attempt recorded in its identity.
///
/// The persisted `cgroup_path` is metadata, not an authority: it only
/// selects the attempt's own cgroup name (`harw-job-<attempt>`) beneath
/// the executor's delegated root, and the reopened handle is accepted only
/// if its path equals the persisted one.
fn reopen_job_cgroup(
    backend: Option<&dyn CgroupBackend>,
    identity: &LinuxRecoveryIdentity,
    ctx: &AttemptContext,
) -> CgroupRecovery {
    let name = cgroup_name(ctx);
    let Some(recorded) = identity.cgroup_path.as_deref() else {
        return CgroupRecovery::NotUsed;
    };
    if recorded.rsplit('/').next() != Some(name.as_str()) {
        return CgroupRecovery::NotUsed;
    }
    let Some(backend) = backend else {
        return CgroupRecovery::ProcessOnly(format!(
            "job cgroup '{recorded}' not reopened: no cgroup root is configured; \
             termination is limited to the primary process"
        ));
    };
    match backend.reopen(&name) {
        Ok(handle) if handle.proc_path() == recorded => CgroupRecovery::Reopened(handle),
        Ok(handle) => CgroupRecovery::ProcessOnly(format!(
            "job cgroup not reopened: '{}' is not the persisted cgroup '{recorded}'; \
             termination is limited to the primary process",
            handle.proc_path()
        )),
        Err(error) => CgroupRecovery::ProcessOnly(format!(
            "job cgroup '{recorded}' not reopened: {error}; \
             termination is limited to the primary process"
        )),
    }
}

impl AfterExit {
    fn late_report(&self) -> Option<SandboxReport> {
        let path = self.trampoline_report.as_ref()?;
        // Only a regular file is read: a FIFO (or a symlink to one) planted
        // in the report's place would block the read below forever. With
        // `BwrapTrampoline` no sandboxed process is left at this point (the
        // pid namespace ended with its init), so the check cannot race with
        // the job.
        let regular = match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_file() => true,
            Ok(_) => {
                tracing::warn!(
                    path = %path.display(),
                    "trampoline report is not a regular file; the report is ignored"
                );
                false
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "cannot stat trampoline report");
                false
            }
        };
        let report = if regular {
            self.read_trampoline_report(path)
        } else {
            None
        };
        if let Err(error) = std::fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!(path = %path.display(), %error, "cannot remove trampoline report");
            }
        }
        // Both layers restrict the same process: per dimension the stronger
        // one counts, resource limits come from the trampoline only.
        report.map(|inner| match &self.merge_with {
            Some(bwrap) => harw_job_exec::merge_reports(bwrap, &inner),
            None => inner,
        })
    }

    /// Reads the trampoline report at `path` (a regular file), with the
    /// resource dimension capped by the cgroup domain.
    fn read_trampoline_report(&self, path: &Path) -> Option<SandboxReport> {
        match harw_job_exec::read_report(path) {
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
        }
    }
}

/// Forwards supervisor events to the coordinator; after the supervisor
/// ended, cleans up the job cgroup and emits the final `Exited`.
async fn forward(
    handle: SupervisorHandle,
    mut events: mpsc::Receiver<ProcessEvent>,
    sender: AttemptEventSender,
    mut after: AfterExit,
) {
    let mut exit = ExitOutcome::Unknown;
    let mut listening = true;
    if let Some(note) = after.note.take() {
        listening = sender
            .send(AttemptEvent::Error(format!("recovery: {note}")))
            .await;
    }
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
                    remove_cgroup(cgroup, Some(&after.leftovers)).await;
                }
            }
        }
        Err(error) => tracing::warn!(%error, "supervisor task failed"),
    }
    if let Some(cgroup) = after.recovered_cgroup.take() {
        cgroup.kill_and_remove(&after.leftovers).await;
    }
    let sandbox = after.late_report();
    if let Some(dir) = after.attempt_dir.take() {
        remove_attempt_dir(&dir);
    }
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
        let trampoline_paths = match &options.sandbox {
            LinuxSandboxBackend::LandlockTrampoline {
                trampoline,
                plan_dir,
            } => Some((trampoline, plan_dir)),
            #[cfg(feature = "bwrap")]
            LinuxSandboxBackend::BwrapTrampoline {
                trampoline,
                plan_dir,
                ..
            } => Some((trampoline, plan_dir)),
            _ => None,
        };
        if let Some((trampoline, plan_dir)) = trampoline_paths {
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
        Ok(Self {
            options,
            cgroup,
            leftovers: LeftoverRegistry::default(),
            sweep_spawned: AtomicBool::new(false),
        })
    }

    /// The options.
    #[must_use]
    pub fn options(&self) -> &LinuxExecutorOptions {
        &self.options
    }

    /// Number of job cgroups that stayed busy past every removal attempt
    /// and are still waiting for a sweep — a metric for callers that want
    /// to alert on the leak the module docs' recovery section describes,
    /// instead of relying on the log line `remove_cgroup` emits on
    /// exhaustion.
    #[must_use]
    pub fn leftover_cgroup_count(&self) -> usize {
        self.leftovers
            .lock()
            .map(|leftovers| leftovers.len())
            .unwrap_or(0)
    }

    /// Retries removal of every job cgroup that stayed busy past
    /// `remove_cgroup`'s retry budget (`CGROUP_REMOVE_ATTEMPTS`, ~0.5s).
    /// [`Self::ensure_leftover_sweep`] already calls this on
    /// [`LEFTOVER_SWEEP_INTERVAL`] once the first attempt starts; this
    /// method stays public so a test, or a caller that wants a sweep on its
    /// own schedule instead, can call it directly. A cgroup still busy is
    /// queued again for the next call instead of being dropped.
    pub async fn sweep_leftover_cgroups(&self) {
        sweep_registry(&self.leftovers).await;
    }

    /// Spawns the background task that calls [`Self::sweep_leftover_cgroups`]
    /// on [`LEFTOVER_SWEEP_INTERVAL`]; a no-op after the first call, and
    /// also a no-op when no cgroup root is configured (`self.cgroup` is
    /// fixed at construction, and without a root `create_cgroup` and
    /// recovery's `reopen_job_cgroup` never produce a leftover, so there is
    /// nothing to sweep).
    ///
    /// Called from [`Self::supervise`] (so on every `start`/`reattach`)
    /// instead of from [`Self::new`]: some callers build the executor
    /// before entering a Tokio runtime — `harw serve` builds the sandboxed
    /// verify runner during startup, before it builds and enters its own
    /// runtime — where `tokio::spawn` would panic. By the time an attempt
    /// runs, the executor is always driven from inside a runtime
    /// (`Self::supervise` already relies on that for its own
    /// `tokio::spawn`), so spawning here is safe.
    ///
    /// The spawn-once guard runs before the cgroup-root check on purpose:
    /// `self.cgroup` never changes after construction, so checking it
    /// first or second has the same outcome, and checking the guard first
    /// keeps it exercisable in a plain (non-async) test even when there is
    /// no cgroup root and thus no Tokio runtime required.
    fn ensure_leftover_sweep(&self) {
        if self.sweep_spawned.swap(true, Ordering::SeqCst) {
            return;
        }
        if self.cgroup.is_none() {
            return;
        }
        let leftovers = Arc::clone(&self.leftovers);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(LEFTOVER_SWEEP_INTERVAL).await;
                sweep_registry(&leftovers).await;
            }
        });
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
            (false, _) | (true, LinuxSandboxBackend::None) => {
                let mut launch = Launch {
                    program: OsString::from(&spec.program),
                    args: spec.args.iter().map(OsString::from).collect(),
                    env: spec.env.clone(),
                    cwd: Some(workdir),
                };
                let rlimits = apply_prlimit(&mut launch, spec);
                require_rlimits(spec, rlimits)?;
                Ok(Prepared::plain(
                    launch,
                    unsandboxed_report(worse(resource_limits, rlimits)),
                ))
            }
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
                // The trampoline applies the rlimits itself, before the
                // sandbox, and fails the job instead of continuing without.
                if spec.resources.requests_rlimits() {
                    plan = plan.with_rlimits(rlimit_set(spec));
                }
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
                    merge_with: None,
                    attempt_dir: None,
                })
            }
            #[cfg(feature = "bwrap")]
            (true, LinuxSandboxBackend::Bwrap(bwrap)) => {
                let policy = policy_for(spec.sandbox_profile, &ctx.workspace_root);
                let plan = bwrap
                    .plan(spec, &policy, &ctx.workspace_root)
                    .map_err(bwrap_error)?;
                let mut launch = Launch {
                    program: plan.executable().as_os_str().to_owned(),
                    args: plan.args().to_vec(),
                    // bwrap sets the job environment inside (--setenv).
                    env: Vec::new(),
                    cwd: None,
                };
                // `prlimit` runs in front of bwrap; the limits are inherited
                // by the sandboxed program.
                let rlimits = apply_prlimit(&mut launch, spec);
                require_rlimits(spec, rlimits)?;
                let report = SandboxReport {
                    resource_limits: worse(resource_limits, rlimits),
                    ..plan.report()
                };
                Ok(Prepared::plain(launch, report))
            }
            #[cfg(feature = "bwrap")]
            (
                true,
                LinuxSandboxBackend::BwrapTrampoline {
                    bwrap,
                    trampoline,
                    plan_dir,
                },
            ) => {
                let attempt_dir = create_attempt_dir(plan_dir, ctx)?;
                let mount = harw_job_executor_bwrap::TrampolineMount {
                    exec_path: trampoline.clone(),
                    attempt_dir,
                };
                let prepared = prepare_chain(spec, ctx, cgroup, resource_limits, bwrap, &mount);
                if prepared.is_err() {
                    remove_attempt_dir(&mount.attempt_dir);
                }
                prepared
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
        // Every `start`/`reattach` funnels through here, and by now the
        // executor is always driven from inside a Tokio runtime (the
        // `tokio::spawn` below already relies on that), so this is the
        // first point where arming the background sweep is safe.
        self.ensure_leftover_sweep();
        let (handle, events) = Supervisor::spawn(process, stdout, stderr, self.supervisor_config());
        let control = Arc::new(SupervisorControl {
            canceller: handle.canceller(),
        });
        let (sender, attempt_events) = AttemptEvents::channel(EVENT_CAPACITY);
        tokio::spawn(forward(handle, events, sender, after));
        AttemptRun::new(attempt_events, control)
    }
}

/// The `BwrapTrampoline` launch: plans both stages, checks what is known
/// before anything runs, writes the plans into the attempt directory and
/// starts the outer stage (module docs).
#[cfg(feature = "bwrap")]
fn prepare_chain(
    spec: &JobSpec,
    ctx: &AttemptContext,
    cgroup: Option<&(Arc<CgroupV2Fs>, CgroupHandle)>,
    resource_limits: EnforcementState,
    bwrap: &harw_job_executor_bwrap::BwrapExecutor,
    mount: &harw_job_executor_bwrap::TrampolineMount,
) -> Result<Prepared, RuntimeError> {
    let membership = cgroup.map(|(backend, handle)| ChainCgroup {
        root: backend.root_path(),
        name: handle.name(),
        proc_path: handle.proc_path(),
    });
    let plans = chain_plans(
        bwrap,
        mount,
        spec,
        &ctx.workspace_root,
        membership,
        resource_limits,
    )?;
    // Lower bound before anything runs: the namespace layer's network and
    // the resource domain. The inner stage checks the rest itself.
    if spec.sandbox == SandboxRequirement::Required {
        check_requirement(
            spec.sandbox,
            &SandboxReport {
                network: plans.bwrap_report.network,
                resource_limits,
                ..SandboxReport::uniform(EnforcementState::Enforced)
            },
        )?;
    }
    if !plans.unmapped.is_empty() {
        tracing::debug!(
            unmapped = ?plans.unmapped,
            withheld = ?plans.withheld,
            "policy paths without a Landlock rule inside the bwrap namespace"
        );
    }
    write_plan(&mount.inner_plan_path(), &plans.inner)?;
    let outer_path = mount.attempt_dir.join(OUTER_PLAN_FILE);
    write_plan(&outer_path, &plans.outer)?;
    Ok(Prepared {
        launch: Launch {
            program: mount.exec_path.as_os_str().to_owned(),
            args: vec![
                OsString::from("--plan"),
                outer_path.clone().into_os_string(),
            ],
            // bwrap sets the job environment and directory inside.
            env: Vec::new(),
            cwd: None,
        },
        report: None,
        trampoline_report: Some(mount.inner_report_path()),
        trampoline_plan: Some(outer_path),
        merge_with: Some(plans.bwrap_report),
        attempt_dir: Some(mount.attempt_dir.clone()),
    })
}

/// Fixed search paths for `prlimit` (util-linux); `PATH` is never consulted.
const PRLIMIT_CANDIDATES: [&str; 2] = ["/usr/bin/prlimit", "/bin/prlimit"];

/// The per-process rlimits the spec asks for.
fn rlimit_set(spec: &JobSpec) -> harw_job_linux::RlimitSet {
    use harw_job_linux::{RlimitResource, RlimitSet, RlimitValue};
    let resources = &spec.resources;
    let mut set = RlimitSet::new();
    if let Some(bytes) = resources.address_space_max {
        set.set(RlimitResource::AddressSpace, RlimitValue::fixed(bytes));
    }
    if let Some(seconds) = resources.cpu_time_max {
        set.set(RlimitResource::Cpu, RlimitValue::fixed(seconds));
    }
    if let Some(bytes) = resources.file_size_max {
        set.set(RlimitResource::FileSize, RlimitValue::fixed(bytes));
    }
    if let Some(count) = resources.open_files_max {
        set.set(RlimitResource::Nofile, RlimitValue::fixed(u64::from(count)));
    }
    set
}

/// The more restrictive of two per-dimension states (what the report says
/// when two mechanisms each cover part of the request).
fn worse(a: EnforcementState, b: EnforcementState) -> EnforcementState {
    let rank = |state: EnforcementState| match state {
        EnforcementState::Enforced => 0,
        EnforcementState::Partial => 1,
        EnforcementState::NotEnforced => 2,
        _ => 3,
    };
    if rank(b) > rank(a) { b } else { a }
}

/// A job that demands its rlimits does not run without them.
fn require_rlimits(spec: &JobSpec, state: EnforcementState) -> Result<(), RuntimeError> {
    if spec.resources.require_rlimits && state != EnforcementState::Enforced {
        return Err(RuntimeError::Unsupported {
            operation: "rlimits",
            detail: "the job requires per-process rlimits but no prlimit is available".to_owned(),
        });
    }
    Ok(())
}

/// Starts the launch through `prlimit` so the limits hold for the program
/// (and, rlimits being inherited, for everything it starts). `prlimit`
/// `exec`s the program, so the PID stays the primary's. Returns whether the
/// limits are applied: without a `prlimit` they are **not**, and the report
/// says so (a `Required` job then fails before it runs).
fn apply_prlimit(launch: &mut Launch, spec: &JobSpec) -> EnforcementState {
    let resources = &spec.resources;
    if !resources.requests_rlimits() {
        return EnforcementState::Enforced;
    }
    let Some(prlimit) = PRLIMIT_CANDIDATES
        .iter()
        .find(|candidate| Path::new(candidate).is_file())
    else {
        return EnforcementState::NotEnforced;
    };
    let mut wrapped: Vec<OsString> = Vec::new();
    if let Some(bytes) = resources.address_space_max {
        wrapped.push(format!("--as={bytes}").into());
    }
    if let Some(seconds) = resources.cpu_time_max {
        wrapped.push(format!("--cpu={seconds}").into());
    }
    if let Some(bytes) = resources.file_size_max {
        wrapped.push(format!("--fsize={bytes}").into());
    }
    if let Some(count) = resources.open_files_max {
        wrapped.push(format!("--nofile={count}").into());
    }
    wrapped.push("--".into());
    wrapped.push(std::mem::take(&mut launch.program));
    wrapped.append(&mut launch.args);
    launch.program = OsString::from(prlimit);
    launch.args = wrapped;
    EnforcementState::Enforced
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
                prepared.discard_files();
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
        // A new session only for plain (unsandboxed) launches: sandbox
        // backends manage their own namespaces and terminals.
        let session = if self.options.new_session && spec.sandbox == SandboxRequirement::None {
            resolve_setsid()
        } else {
            None
        };
        if let Some(setsid) = session {
            // `setsid` would report a missing program as exit status 127;
            // a path that does not exist is a failed spawn, as without it.
            let program = Path::new(&launch.program);
            if program.components().count() > 1 && !program.exists() {
                if let Some(plan) = &prepared.trampoline_plan {
                    let _ = std::fs::remove_file(plan);
                }
                if let Some(dir) = &prepared.attempt_dir {
                    remove_attempt_dir(dir);
                }
                release(cgroup);
                return Err(RuntimeError::Spawn {
                    program: spec.program.clone(),
                    source: std::io::Error::from(std::io::ErrorKind::NotFound),
                });
            }
            launch = launch.wrap_in_session(setsid);
        }
        // 4. spawn → pidfd → cgroup attach
        let mut command = launch.command();
        if let Some(files) = &ctx.output_files {
            let open = |path: &Path| {
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .mode(0o600)
                    .open(path)
                    .map(Stdio::from)
                    .map_err(|error| RuntimeError::Os {
                        operation: "open output file",
                        detail: format!("{}: {error}", path.display()),
                    })
            };
            let opened = open(&files.stdout)
                .and_then(|stdout| open(&files.stderr).map(|stderr| (stdout, stderr)));
            match opened {
                Ok((stdout, stderr)) => {
                    command.stdout(stdout).stderr(stderr);
                }
                Err(error) => {
                    // Nothing ran: discard the plans (the outer one carries
                    // the job environment) and the unused job cgroup.
                    if let Some(plan) = &prepared.trampoline_plan {
                        let _ = std::fs::remove_file(plan);
                    }
                    if let Some(dir) = &prepared.attempt_dir {
                        remove_attempt_dir(dir);
                    }
                    release(cgroup);
                    return Err(error);
                }
            }
        }
        if let Some(handoff) = &ctx.stdio_handoff {
            if handoff.stdin {
                command.stdin(Stdio::piped());
            }
            if handoff.stdout {
                command.stdout(Stdio::piped());
            }
        }
        let spawned = match cgroup {
            Some((backend, handle)) => {
                let backend: Arc<dyn CgroupBackend> = backend;
                LinuxJobGroup::spawn_in_cgroup_with(
                    &mut command,
                    JobCgroup::new(backend, handle),
                    session.is_some(),
                )
                .map_err(|error| match error {
                    LaunchError::Process(error) => process_error("spawn", &spec.program, error),
                    LaunchError::Cgroup(error) => cgroup_error(error),
                    other => RuntimeError::Os {
                        operation: "spawn in cgroup",
                        detail: other.to_string(),
                    },
                })
            }
            None if session.is_some() => LinuxJobGroup::spawn_in_new_session(&mut command)
                .map_err(|error| process_error("spawn", &spec.program, error)),
            None => LinuxJobGroup::spawn(&mut command)
                .map_err(|error| process_error("spawn", &spec.program, error)),
        };
        let (group, mut stdio) = match spawned {
            Ok(spawned) => spawned,
            Err(error) => {
                if let Some(plan) = &prepared.trampoline_plan {
                    let _ = std::fs::remove_file(plan);
                }
                if let Some(dir) = &prepared.attempt_dir {
                    remove_attempt_dir(dir);
                }
                return Err(error);
            }
        };
        let pid = group.primary().pid();
        if let Some(handoff) = &ctx.stdio_handoff {
            // The submitter owns these pipes from here on; the supervisor
            // neither reads nor closes them.
            handoff.deliver(super::executor::HandedStdio {
                stdin: if handoff.stdin {
                    stdio.stdin.take()
                } else {
                    None
                },
                stdout: if handoff.stdout {
                    stdio.stdout.take()
                } else {
                    None
                },
            });
        }
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
                if let Some(dir) = &prepared.attempt_dir {
                    remove_attempt_dir(dir);
                }
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
                merge_with: prepared.merge_with,
                attempt_dir: prepared.attempt_dir,
                resource_limits,
                recovered_cgroup: None,
                note: None,
                leftovers: Arc::clone(&self.leftovers),
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
        // `process` is already identity-verified (pidfd + start-time match,
        // `open_verified` above): unlike an unregistered `start()` failure,
        // leaving it running here would orphan a *proven* job outside every
        // supervision, budget and deadline enforcement. On adoption failure
        // it is therefore abandoned the same way `start()` abandons a
        // process it could not register with the reactor, instead of being
        // dropped unsignalled (module docs, "never signal what we can't
        // prove" — this one already is proven).
        let process = match AsyncLinuxProcess::new(process) {
            Ok(process) => process,
            Err(error) => {
                let detail = error.error.to_string();
                abandon(*error.target);
                return Err(RuntimeError::Os {
                    operation: "adopt recovered process",
                    detail,
                });
            }
        };
        // Only for a verified live process: reopen its job cgroup so that
        // termination reaches every descendant.
        let backend = self
            .cgroup
            .as_ref()
            .map(|backend| Arc::clone(backend) as Arc<dyn CgroupBackend>);
        let (recovered_cgroup, note) = match reopen_job_cgroup(backend.as_deref(), identity, ctx) {
            CgroupRecovery::NotUsed => (None, None),
            CgroupRecovery::Reopened(handle) => {
                tracing::info!(
                    pid = identity.pid,
                    cgroup = handle.proc_path(),
                    attempt = %ctx.attempt_id,
                    "reopened the job cgroup of the recovered attempt"
                );
                let recovered = backend.map(|backend| RecoveredCgroup {
                    backend,
                    handle,
                    identity: identity.clone(),
                });
                (recovered, None)
            }
            CgroupRecovery::ProcessOnly(detail) => {
                tracing::warn!(
                    pid = identity.pid,
                    attempt = %ctx.attempt_id,
                    %detail,
                    "recovered attempt falls back to process-only termination"
                );
                (None, Some(detail))
            }
        };
        tracing::info!(pid = identity.pid, job = %ctx.job_id, attempt = %ctx.attempt_id, "reattached verified process");
        Ok(self.supervise(
            process,
            None,
            None,
            AfterExit {
                trampoline_report: None,
                merge_with: None,
                attempt_dir: None,
                resource_limits: EnforcementState::NotEnforced,
                recovered_cgroup,
                note,
                leftovers: Arc::clone(&self.leftovers),
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
    use super::{
        CgroupRecovery, Launch, LeftoverCgroup, LeftoverRegistry, LinuxExecutor,
        LinuxExecutorOptions, RecoveredCgroup, cgroup_name, remove_cgroup, reopen_job_cgroup,
        weaker,
    };
    use crate::coordinator::executor::AttemptContext;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_job_core::{AttemptId, EnforcementState, RunnerId};
    use harw_job_linux::cgroup::{CgroupBackend, CgroupHandle, CgroupSpec, CgroupStats};
    use harw_job_linux::group::JobCgroup;
    use harw_job_linux::{CgroupError, LinuxProcess, LinuxRecoveryIdentity};
    use harw_types::WorkId;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    /// A backend whose `reopen` yields a handle with `proc_path` (or
    /// fails); every other operation fails.
    struct FakeBackend {
        proc_path: Option<String>,
    }

    fn denied(path: &str) -> CgroupError {
        CgroupError::PermissionDenied {
            path: path.to_owned(),
        }
    }

    impl CgroupBackend for FakeBackend {
        fn create(&self, spec: &CgroupSpec) -> Result<CgroupHandle, CgroupError> {
            Err(denied(&spec.name))
        }

        fn reopen(&self, name: &str) -> Result<CgroupHandle, CgroupError> {
            match &self.proc_path {
                Some(path) => Ok(CgroupHandle::new(name.to_owned(), path.clone(), None)),
                None => Err(denied(name)),
            }
        }

        fn attach(&self, group: &CgroupHandle, _process: &LinuxProcess) -> Result<(), CgroupError> {
            Err(denied(group.name()))
        }

        fn attach_self(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
            Err(denied(group.name()))
        }

        fn freeze(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
            Err(denied(group.name()))
        }

        fn thaw(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
            Err(denied(group.name()))
        }

        fn kill(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
            Err(denied(group.name()))
        }

        fn stats(&self, group: &CgroupHandle) -> Result<CgroupStats, CgroupError> {
            Err(denied(group.name()))
        }

        fn remove(&self, group: CgroupHandle) -> Result<(), CgroupError> {
            Err(denied(group.name()))
        }
    }

    /// A backend for `remove_cgroup`/`sweep_leftover_cgroups`: `kill` and
    /// `reopen` always succeed, `remove` stays [`CgroupError::Busy`] for
    /// the first `busy_calls` calls and then succeeds (every other
    /// operation fails, unused by these tests).
    struct FlakyRemoveBackend {
        proc_path: String,
        remove_calls: AtomicU32,
        busy_calls: u32,
    }

    impl CgroupBackend for FlakyRemoveBackend {
        fn create(&self, spec: &CgroupSpec) -> Result<CgroupHandle, CgroupError> {
            Err(denied(&spec.name))
        }

        fn reopen(&self, name: &str) -> Result<CgroupHandle, CgroupError> {
            Ok(CgroupHandle::new(
                name.to_owned(),
                self.proc_path.clone(),
                None,
            ))
        }

        fn attach(&self, group: &CgroupHandle, _process: &LinuxProcess) -> Result<(), CgroupError> {
            Err(denied(group.name()))
        }

        fn attach_self(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
            Err(denied(group.name()))
        }

        fn freeze(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
            Err(denied(group.name()))
        }

        fn thaw(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
            Err(denied(group.name()))
        }

        fn kill(&self, _group: &CgroupHandle) -> Result<(), CgroupError> {
            Ok(())
        }

        fn stats(&self, group: &CgroupHandle) -> Result<CgroupStats, CgroupError> {
            Err(denied(group.name()))
        }

        fn remove(&self, group: CgroupHandle) -> Result<(), CgroupError> {
            let call = self.remove_calls.fetch_add(1, Ordering::SeqCst);
            if call < self.busy_calls {
                Err(CgroupError::Busy {
                    path: group.name().to_owned(),
                })
            } else {
                Ok(())
            }
        }
    }

    fn attempt_context() -> TestResult<AttemptContext> {
        Ok(AttemptContext {
            job_id: WorkId::from_str("j"),
            attempt_id: AttemptId::new("j-e1").map_err(ctx("attempt"))?,
            runner_id: RunnerId::new("r").map_err(ctx("runner"))?,
            lease_epoch: 1,
            workspace_root: PathBuf::from("/"),
            output_files: None,
            stdio_handoff: None,
        })
    }

    fn identity(context: &AttemptContext, cgroup_path: Option<&str>) -> LinuxRecoveryIdentity {
        LinuxRecoveryIdentity {
            pid: 4242,
            process_start_time: Some(1),
            cgroup_path: cgroup_path.map(str::to_owned),
            executable: None,
            runner_id: context.runner_id.clone(),
            attempt_id: context.attempt_id.clone(),
        }
    }

    const JOB_CGROUP: &str = "/delegated/harw-job-j-e1";

    #[test]
    fn recovered_cgroup_is_reopened_only_with_the_persisted_path() -> TestResult {
        let context = attempt_context()?;
        let persisted = identity(&context, Some(JOB_CGROUP));
        let matching = FakeBackend {
            proc_path: Some(JOB_CGROUP.to_owned()),
        };
        assert_eq!(
            reopen_job_cgroup(Some(&matching as &dyn CgroupBackend), &persisted, &context),
            CgroupRecovery::Reopened(CgroupHandle::new(
                "harw-job-j-e1".to_owned(),
                JOB_CGROUP.to_owned(),
                None
            ))
        );
        // Same name beneath another root: not the persisted cgroup.
        let elsewhere = FakeBackend {
            proc_path: Some("/other/harw-job-j-e1".to_owned()),
        };
        assert!(matches!(
            reopen_job_cgroup(Some(&elsewhere as &dyn CgroupBackend), &persisted, &context),
            CgroupRecovery::ProcessOnly(detail) if detail.contains("is not the persisted cgroup")
        ));
        Ok(())
    }

    #[test]
    fn recovered_cgroup_falls_back_to_process_only() -> TestResult {
        let context = attempt_context()?;
        let persisted = identity(&context, Some(JOB_CGROUP));
        // No cgroup root configured on the restarted runner.
        assert!(matches!(
            reopen_job_cgroup(None, &persisted, &context),
            CgroupRecovery::ProcessOnly(detail) if detail.contains("no cgroup root is configured")
        ));
        // The cgroup cannot be reopened (gone, permissions).
        let failing = FakeBackend { proc_path: None };
        assert!(matches!(
            reopen_job_cgroup(Some(&failing as &dyn CgroupBackend), &persisted, &context),
            CgroupRecovery::ProcessOnly(detail) if detail.contains("not reopened")
        ));
        Ok(())
    }

    #[test]
    fn attempt_without_job_cgroup_needs_no_reopen() -> TestResult {
        let context = attempt_context()?;
        let backend = FakeBackend {
            proc_path: Some(JOB_CGROUP.to_owned()),
        };
        // The runner's own cgroup (no job cgroup was configured at start).
        let plain = identity(&context, Some("/user.slice/session-1.scope"));
        assert_eq!(
            reopen_job_cgroup(Some(&backend as &dyn CgroupBackend), &plain, &context),
            CgroupRecovery::NotUsed
        );
        // Another attempt's job cgroup is never adopted.
        let foreign = identity(&context, Some("/delegated/harw-job-j-e2"));
        assert_eq!(
            reopen_job_cgroup(Some(&backend as &dyn CgroupBackend), &foreign, &context),
            CgroupRecovery::NotUsed
        );
        let unknown = identity(&context, None);
        assert_eq!(
            reopen_job_cgroup(None, &unknown, &context),
            CgroupRecovery::NotUsed
        );
        Ok(())
    }

    #[test]
    fn cgroup_names_are_single_safe_components() -> TestResult {
        let context = AttemptContext {
            job_id: WorkId::from_str("j"),
            attempt_id: AttemptId::new("job:1.x-e2").map_err(ctx("attempt"))?,
            runner_id: RunnerId::new("r").map_err(ctx("runner"))?,
            lease_epoch: 1,
            workspace_root: PathBuf::from("/"),
            output_files: None,
            stdio_handoff: None,
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

    /// Regression for the finding that a job cgroup still busy past every
    /// `remove_cgroup` attempt was only logged, with no way to retry it
    /// later: it must now be recorded in the leftover registry instead.
    #[tokio::test]
    async fn remove_cgroup_leaves_a_leftover_when_never_free() -> TestResult {
        let backend: Arc<dyn CgroupBackend> = Arc::new(FlakyRemoveBackend {
            proc_path: JOB_CGROUP.to_owned(),
            remove_calls: AtomicU32::new(0),
            busy_calls: u32::MAX,
        });
        let handle = CgroupHandle::new("harw-job-j-e1".to_owned(), JOB_CGROUP.to_owned(), None);
        let leftovers: LeftoverRegistry = LeftoverRegistry::default();
        remove_cgroup(JobCgroup::new(backend, handle), Some(&leftovers)).await;
        let recorded = leftovers.lock().map_err(ctx("lock leftovers"))?;
        let leftover = recorded
            .first()
            .ok_or(TestError::Missing("leftover cgroup"))?;
        assert_eq!(leftover.name, "harw-job-j-e1");
        Ok(())
    }

    /// Without a registry, exhaustion still only logs — same behaviour as
    /// before this fix, for callers that never wire the registry up.
    #[tokio::test]
    async fn remove_cgroup_without_a_registry_does_not_panic() {
        let backend: Arc<dyn CgroupBackend> = Arc::new(FlakyRemoveBackend {
            proc_path: JOB_CGROUP.to_owned(),
            remove_calls: AtomicU32::new(0),
            busy_calls: u32::MAX,
        });
        let handle = CgroupHandle::new("harw-job-j-e1".to_owned(), JOB_CGROUP.to_owned(), None);
        remove_cgroup(JobCgroup::new(backend, handle), None).await;
    }

    /// `remove` always reports Busy; `reopen` always fails. Exercises
    /// `remove_cgroup`'s reopen-failure path inside its retry loop, not the
    /// exhaustion path below it: this used to only log and return, leaking
    /// the cgroup outside the registry even though it is the same class of
    /// defect as running out of removal attempts.
    struct ReopenAlwaysFailsBackend;

    impl CgroupBackend for ReopenAlwaysFailsBackend {
        fn create(&self, spec: &CgroupSpec) -> Result<CgroupHandle, CgroupError> {
            Err(denied(&spec.name))
        }

        fn reopen(&self, name: &str) -> Result<CgroupHandle, CgroupError> {
            Err(denied(name))
        }

        fn attach(&self, group: &CgroupHandle, _process: &LinuxProcess) -> Result<(), CgroupError> {
            Err(denied(group.name()))
        }

        fn attach_self(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
            Err(denied(group.name()))
        }

        fn freeze(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
            Err(denied(group.name()))
        }

        fn thaw(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
            Err(denied(group.name()))
        }

        fn kill(&self, _group: &CgroupHandle) -> Result<(), CgroupError> {
            Ok(())
        }

        fn stats(&self, group: &CgroupHandle) -> Result<CgroupStats, CgroupError> {
            Err(denied(group.name()))
        }

        fn remove(&self, group: CgroupHandle) -> Result<(), CgroupError> {
            Err(CgroupError::Busy {
                path: group.name().to_owned(),
            })
        }
    }

    #[tokio::test]
    async fn remove_cgroup_leaves_a_leftover_when_reopen_fails_mid_retry() -> TestResult {
        let backend: Arc<dyn CgroupBackend> = Arc::new(ReopenAlwaysFailsBackend);
        let handle = CgroupHandle::new("harw-job-j-e1".to_owned(), JOB_CGROUP.to_owned(), None);
        let leftovers: LeftoverRegistry = LeftoverRegistry::default();
        remove_cgroup(JobCgroup::new(backend, handle), Some(&leftovers)).await;
        let recorded = leftovers.lock().map_err(ctx("lock leftovers"))?;
        let leftover = recorded
            .first()
            .ok_or(TestError::Missing("leftover cgroup"))?;
        assert_eq!(leftover.name, "harw-job-j-e1");
        Ok(())
    }

    /// Regression for the finding that a recovered attempt whose cgroup
    /// could not even be killed was only logged, with no way to retry it
    /// later: it must now be recorded in the leftover registry too, just
    /// like a plain `remove_cgroup` exhaustion.
    #[tokio::test]
    async fn recovered_cgroup_kill_failure_leaves_a_leftover() -> TestResult {
        let context = attempt_context()?;
        // `identity.cgroup_path` deliberately does not match the handle's
        // `proc_path`, so `kill_job_verified` fails on the identity check
        // before it ever touches the backend — deterministic, no real pid
        // needed.
        let persisted = identity(&context, Some(JOB_CGROUP));
        let backend: Arc<dyn CgroupBackend> = Arc::new(ReopenAlwaysFailsBackend);
        let handle = CgroupHandle::new("harw-job-j-e1".to_owned(), "/other/path".to_owned(), None);
        let recovered = RecoveredCgroup {
            backend,
            handle,
            identity: persisted,
        };
        let leftovers: LeftoverRegistry = LeftoverRegistry::default();
        recovered.kill_and_remove(&leftovers).await;
        let recorded = leftovers.lock().map_err(ctx("lock leftovers"))?;
        let leftover = recorded
            .first()
            .ok_or(TestError::Missing("leftover cgroup"))?;
        assert_eq!(leftover.name, "harw-job-j-e1");
        Ok(())
    }

    /// [`LinuxExecutor::ensure_leftover_sweep`] must stay safe to call
    /// without a Tokio runtime when there is nothing to sweep, and must not
    /// re-arm on a second call.
    #[test]
    fn ensure_leftover_sweep_is_idempotent_and_skips_without_a_cgroup_root() {
        let executor = LinuxExecutor {
            options: LinuxExecutorOptions::default(),
            cgroup: None,
            leftovers: LeftoverRegistry::default(),
            sweep_spawned: AtomicBool::new(false),
        };
        executor.ensure_leftover_sweep();
        assert!(executor.sweep_spawned.load(Ordering::SeqCst));
        // Idempotent: a second call must not panic (e.g. by trying to
        // spawn again) and must leave the guard set.
        executor.ensure_leftover_sweep();
        assert!(executor.sweep_spawned.load(Ordering::SeqCst));
    }

    /// [`LinuxExecutor::sweep_leftover_cgroups`] retries a leftover and
    /// clears it once the cgroup is no longer busy.
    #[tokio::test]
    async fn sweep_leftover_cgroups_clears_a_cgroup_once_it_frees() -> TestResult {
        let backend: Arc<dyn CgroupBackend> = Arc::new(FlakyRemoveBackend {
            proc_path: JOB_CGROUP.to_owned(),
            remove_calls: AtomicU32::new(0),
            busy_calls: 0, // free on the very first retry
        });
        let executor = LinuxExecutor {
            options: LinuxExecutorOptions::default(),
            cgroup: None,
            leftovers: Arc::new(Mutex::new(vec![LeftoverCgroup {
                backend,
                name: "harw-job-j-e1".to_owned(),
            }])),
            sweep_spawned: AtomicBool::new(false),
        };
        assert_eq!(executor.leftover_cgroup_count(), 1);
        executor.sweep_leftover_cgroups().await;
        assert_eq!(executor.leftover_cgroup_count(), 0);
        Ok(())
    }

    /// A leftover whose cgroup cannot be reopened (still busy on the
    /// filesystem, but not gone) is queued again instead of being dropped.
    #[tokio::test]
    async fn sweep_leftover_cgroups_requeues_when_reopen_still_fails() -> TestResult {
        let backend: Arc<dyn CgroupBackend> = Arc::new(FakeBackend { proc_path: None });
        let executor = LinuxExecutor {
            options: LinuxExecutorOptions::default(),
            cgroup: None,
            leftovers: Arc::new(Mutex::new(vec![LeftoverCgroup {
                backend,
                name: "harw-job-j-e1".to_owned(),
            }])),
            sweep_spawned: AtomicBool::new(false),
        };
        executor.sweep_leftover_cgroups().await;
        assert_eq!(executor.leftover_cgroup_count(), 1);
        Ok(())
    }

    /// A leftover whose cgroup is already gone (removed by an earlier sweep,
    /// or by the kernel once its last member exited) is dropped, not
    /// requeued forever.
    #[tokio::test]
    async fn sweep_leftover_cgroups_drops_a_cgroup_that_is_already_gone() -> TestResult {
        /// `reopen` always reports the cgroup gone; every other operation
        /// is unused by this test.
        struct GoneBackend;

        impl CgroupBackend for GoneBackend {
            fn create(&self, spec: &CgroupSpec) -> Result<CgroupHandle, CgroupError> {
                Err(denied(&spec.name))
            }

            fn reopen(&self, name: &str) -> Result<CgroupHandle, CgroupError> {
                Err(CgroupError::NotFound {
                    path: name.to_owned(),
                })
            }

            fn attach(
                &self,
                group: &CgroupHandle,
                _process: &LinuxProcess,
            ) -> Result<(), CgroupError> {
                Err(denied(group.name()))
            }

            fn attach_self(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
                Err(denied(group.name()))
            }

            fn freeze(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
                Err(denied(group.name()))
            }

            fn thaw(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
                Err(denied(group.name()))
            }

            fn kill(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
                Err(denied(group.name()))
            }

            fn stats(&self, group: &CgroupHandle) -> Result<CgroupStats, CgroupError> {
                Err(denied(group.name()))
            }

            fn remove(&self, group: CgroupHandle) -> Result<(), CgroupError> {
                Err(denied(group.name()))
            }
        }

        let executor = LinuxExecutor {
            options: LinuxExecutorOptions::default(),
            cgroup: None,
            leftovers: Arc::new(Mutex::new(vec![LeftoverCgroup {
                backend: Arc::new(GoneBackend),
                name: "harw-job-j-e1".to_owned(),
            }])),
            sweep_spawned: AtomicBool::new(false),
        };
        executor.sweep_leftover_cgroups().await;
        assert_eq!(executor.leftover_cgroup_count(), 0);
        Ok(())
    }
}

/// Plan and report tests of the `BwrapTrampoline` backend (PL-97 E1); no
/// process is started.
#[cfg(all(test, feature = "bwrap"))]
mod chain_tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};

    use harw_job_core::{
        AttemptId, EnforcementState as S, JobSpec, ResourceRequest, RunnerId, SandboxProfileName,
        SandboxReport, SandboxRequirement,
    };
    use harw_job_exec::{CgroupJoin, CgroupMembership, CgroupVerify, ExecPlanV1};
    use harw_job_executor_bwrap::{
        BwrapExecutor, SANDBOX_ATTEMPT_DIR, SANDBOX_TRAMPOLINE_PATH, TrampolineMount,
    };
    use harw_types::WorkId;

    use super::{
        AfterExit, ChainCgroup, LeftoverRegistry, LinuxExecutor, LinuxExecutorOptions,
        LinuxSandboxBackend, OUTER_PLAN_FILE, RuntimeError, chain_plans, rlimit_set,
    };
    use crate::coordinator::executor::AttemptContext;
    use crate::host::BackendKind;
    use crate::test_support::{TestError, TestResult, ctx};

    const TRAMPOLINE: &str = "/opt/harw/bin/harw-job-exec";

    fn bwrap() -> BwrapExecutor {
        BwrapExecutor::from_executable(PathBuf::from("/usr/bin/bwrap")).with_identity(1000, 1000)
    }

    fn spec(requirement: SandboxRequirement, resources: ResourceRequest) -> TestResult<JobSpec> {
        JobSpec::command("make")
            .arg("all")
            .resources(resources)
            .sandbox(SandboxProfileName::NoNetwork)
            .sandbox_requirement(requirement)
            .build()
            .map_err(ctx("spec"))
    }

    fn rlimited() -> ResourceRequest {
        ResourceRequest {
            open_files_max: Some(256),
            ..ResourceRequest::default()
        }
    }

    /// A workspace and a private plan directory.
    fn dirs() -> TestResult<(tempfile::TempDir, PathBuf, PathBuf)> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let workspace = dir.path().join("workspace");
        let plans = dir.path().join("plans");
        std::fs::create_dir(&workspace).map_err(ctx("workspace"))?;
        std::fs::create_dir(&plans).map_err(ctx("plans"))?;
        std::fs::set_permissions(&plans, std::fs::Permissions::from_mode(0o700))
            .map_err(ctx("chmod plans"))?;
        let workspace = workspace
            .canonicalize()
            .map_err(ctx("canonical workspace"))?;
        Ok((dir, workspace, plans))
    }

    fn attempt_context(workspace: &Path) -> TestResult<AttemptContext> {
        Ok(AttemptContext {
            job_id: WorkId::from_str("j"),
            attempt_id: AttemptId::new("j-e1").map_err(ctx("attempt"))?,
            runner_id: RunnerId::new("r").map_err(ctx("runner"))?,
            lease_epoch: 1,
            workspace_root: workspace.to_path_buf(),
            output_files: None,
            stdio_handoff: None,
        })
    }

    fn strings(args: &[String]) -> Vec<&str> {
        args.iter().map(String::as_str).collect()
    }

    fn has_triple(args: &[String], triple: [&str; 3]) -> bool {
        args.windows(3)
            .any(|window| strings(window).as_slice() == triple.as_slice())
    }

    #[test]
    fn chain_plans_attach_outside_and_verify_inside() -> TestResult {
        let (_dir, workspace, plans) = dirs()?;
        let mount = TrampolineMount {
            exec_path: PathBuf::from(TRAMPOLINE),
            attempt_dir: plans.join("a1"),
        };
        let spec = spec(SandboxRequirement::BestEffort, rlimited())?;
        let cgroup = ChainCgroup {
            root: Path::new("/sys/fs/cgroup/harw.slice"),
            name: "harw-job-j-e1",
            proc_path: "/harw.slice/harw-job-j-e1",
        };
        let chain = chain_plans(
            &bwrap(),
            &mount,
            &spec,
            &workspace,
            Some(cgroup),
            S::Enforced,
        )
        .map_err(ctx("chain plans"))?;

        // Outer stage: joins the cgroup first, applies the rlimits, execs bwrap.
        let outer = &chain.outer;
        assert_eq!(outer.program, Path::new("/usr/bin/bwrap"));
        assert_eq!(
            outer.cgroup,
            Some(CgroupMembership::Attach(CgroupJoin {
                root: PathBuf::from("/sys/fs/cgroup/harw.slice"),
                relative: "harw-job-j-e1".to_owned(),
                limits: Some(S::Enforced),
            }))
        );
        assert_eq!(outer.rlimits, rlimit_set(&spec));
        assert!(outer.sandbox.is_none(), "no Landlock outside the namespace");
        assert_eq!(outer.sandbox_requirement, SandboxRequirement::None);
        let args = &outer.args;
        assert!(has_triple(
            args,
            ["--ro-bind", TRAMPOLINE, SANDBOX_TRAMPOLINE_PATH]
        ));
        let attempt = mount.attempt_dir.to_string_lossy().into_owned();
        assert!(has_triple(args, ["--bind", &attempt, SANDBOX_ATTEMPT_DIR]));
        for flag in [
            "--unshare-user",
            "--unshare-ipc",
            "--unshare-pid",
            "--unshare-net",
            "--unshare-uts",
        ] {
            assert!(args.iter().any(|arg| arg == flag), "{flag}: {args:?}");
        }
        assert!(!args.iter().any(|arg| arg == "--unshare-all"), "{args:?}");
        let separator = args
            .iter()
            .position(|arg| arg == "--")
            .ok_or(TestError::Missing("--"))?;
        assert_eq!(
            args.get(separator + 1..).map(strings),
            Some(vec![
                SANDBOX_TRAMPOLINE_PATH,
                "--plan",
                "/run/harw-job/attempt/inner.json",
                "--report",
                "/run/harw-job/attempt/report.json"
            ])
        );

        // Inner stage: verifies the cgroup, then the sandbox, then the job.
        let inner = &chain.inner;
        assert_eq!(inner.program, Path::new("make"));
        assert_eq!(inner.args, ["all"]);
        assert_eq!(
            inner.cgroup,
            Some(CgroupMembership::Verify(CgroupVerify {
                expected_proc_path: PathBuf::from("/harw.slice/harw-job-j-e1"),
                limits: Some(S::Enforced),
            }))
        );
        assert_eq!(inner.rlimits, rlimit_set(&spec));
        assert_eq!(inner.sandbox_requirement, SandboxRequirement::BestEffort);
        let decoded =
            harw_job_exec::ExecPlanV1::from_json(&inner.to_json().map_err(ctx("serialize inner"))?)
                .map_err(ctx("decode inner"))?;
        assert_eq!(
            decoded.sandbox.as_ref().map(|policy| policy.landlock),
            Some(harw_job_linux::LandlockMode::HardRequirement),
            "report protection survives the trampoline plan handoff"
        );
        let policy = inner
            .sandbox
            .as_ref()
            .ok_or(TestError::Missing("inner sandbox"))?;
        // In-namespace paths only: the workspace bind keeps its path, /sys
        // and /run are gone, the network is the namespace's job.
        assert!(policy.filesystem.read_write.contains(&workspace));
        assert!(
            !policy
                .filesystem
                .read_only
                .iter()
                .any(|path| path.starts_with("/sys") || path.starts_with("/run"))
        );
        assert_eq!(policy.network, harw_job_linux::NetworkPolicy::Allow);
        assert_eq!(
            chain.unmapped,
            [PathBuf::from("/run"), PathBuf::from("/sys")]
        );
        assert_eq!(
            chain.withheld,
            [PathBuf::from("/sys"), PathBuf::from("/run")]
        );
        assert_eq!(chain.bwrap_report.network, S::Enforced);
        // Both plans are valid trampoline plans.
        outer.validate().map_err(ctx("outer plan"))?;
        inner.validate().map_err(ctx("inner plan"))?;
        Ok(())
    }

    #[test]
    fn chain_plans_without_cgroup_root_have_no_membership() -> TestResult {
        let (_dir, workspace, plans) = dirs()?;
        let mount = TrampolineMount {
            exec_path: PathBuf::from(TRAMPOLINE),
            attempt_dir: plans.join("a1"),
        };
        let spec = spec(SandboxRequirement::BestEffort, ResourceRequest::default())?;
        let chain = chain_plans(&bwrap(), &mount, &spec, &workspace, None, S::Enforced)
            .map_err(ctx("chain plans"))?;
        assert_eq!(chain.outer.cgroup, None);
        assert_eq!(chain.inner.cgroup, None);
        assert!(chain.outer.rlimits.is_empty() && chain.inner.rlimits.is_empty());
        Ok(())
    }

    #[test]
    fn prepare_writes_both_plans_into_a_private_attempt_dir() -> TestResult {
        let (_dir, workspace, plans) = dirs()?;
        let executor = LinuxExecutor::new(LinuxExecutorOptions {
            sandbox: LinuxSandboxBackend::BwrapTrampoline {
                bwrap: bwrap(),
                trampoline: PathBuf::from(TRAMPOLINE),
                plan_dir: plans.clone(),
            },
            ..LinuxExecutorOptions::default()
        })
        .map_err(ctx("executor"))?;
        assert_eq!(
            executor.options().sandbox.kind(),
            BackendKind::BwrapTrampoline
        );
        let context = attempt_context(&workspace)?;
        let spec = spec(SandboxRequirement::BestEffort, rlimited())?;
        let prepared = executor
            .prepare(&spec, &context, None, S::Enforced)
            .map_err(ctx("prepare"))?;
        let attempt_dir = prepared
            .attempt_dir
            .clone()
            .ok_or(TestError::Missing("attempt dir"))?;
        assert_eq!(attempt_dir.parent(), Some(plans.as_path()));
        let name = attempt_dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(TestError::Missing("attempt dir name"))?;
        assert!(name.starts_with("harw-job-j-e1."), "{name}");
        let mode = |path: &Path| -> TestResult<u32> {
            Ok(std::fs::metadata(path)
                .map_err(ctx("metadata"))?
                .permissions()
                .mode()
                & 0o777)
        };
        assert_eq!(mode(&attempt_dir)?, 0o700);
        let outer_path = attempt_dir.join(OUTER_PLAN_FILE);
        let inner_path = attempt_dir.join("inner.json");
        assert_eq!(mode(&outer_path)?, 0o600);
        assert_eq!(mode(&inner_path)?, 0o600);
        // The launch is the outer trampoline with its plan, nothing else.
        assert_eq!(
            prepared.launch.program,
            std::ffi::OsString::from(TRAMPOLINE)
        );
        assert_eq!(
            prepared.launch.args,
            [
                std::ffi::OsString::from("--plan"),
                outer_path.clone().into_os_string()
            ]
        );
        assert!(prepared.launch.env.is_empty() && prepared.launch.cwd.is_none());
        assert_eq!(prepared.report, None, "known after the job ran");
        assert_eq!(
            prepared.trampoline_report,
            Some(attempt_dir.join("report.json"))
        );
        // The files hold exactly the planned stages.
        let mount = TrampolineMount {
            exec_path: PathBuf::from(TRAMPOLINE),
            attempt_dir: attempt_dir.clone(),
        };
        let chain = chain_plans(&bwrap(), &mount, &spec, &workspace, None, S::Enforced)
            .map_err(ctx("chain plans"))?;
        let read = |path: &Path| -> TestResult<ExecPlanV1> {
            ExecPlanV1::from_json(&std::fs::read(path).map_err(ctx("read plan"))?)
                .map_err(ctx("parse plan"))
        };
        assert_eq!(read(&outer_path)?, chain.outer);
        assert_eq!(read(&inner_path)?, chain.inner);
        assert_eq!(prepared.merge_with, Some(chain.bwrap_report));
        prepared.discard_files();
        assert!(!attempt_dir.exists());
        Ok(())
    }

    #[test]
    fn prepare_refuses_a_required_job_without_resource_domain_and_cleans_up() -> TestResult {
        let (_dir, workspace, plans) = dirs()?;
        let executor = LinuxExecutor::new(LinuxExecutorOptions {
            sandbox: LinuxSandboxBackend::BwrapTrampoline {
                bwrap: bwrap(),
                trampoline: PathBuf::from(TRAMPOLINE),
                plan_dir: plans.clone(),
            },
            ..LinuxExecutorOptions::default()
        })
        .map_err(ctx("executor"))?;
        let context = attempt_context(&workspace)?;
        let spec = spec(
            SandboxRequirement::Required,
            ResourceRequest {
                memory_max: Some(64 << 20),
                ..ResourceRequest::default()
            },
        )?;
        match executor.prepare(&spec, &context, None, S::NotEnforced) {
            Err(RuntimeError::SandboxRequirementNotMet { shortfalls, .. }) => {
                assert_eq!(shortfalls, ["resource_limits"]);
            }
            Err(other) => return Err(TestError::Unexpected(format!("{other}"))),
            Ok(_) => return Err(TestError::Unexpected("required job planned".to_owned())),
        }
        let left = std::fs::read_dir(&plans)
            .map_err(ctx("read plan dir"))?
            .count();
        assert_eq!(left, 0, "the attempt directory is removed");
        Ok(())
    }

    #[test]
    fn late_report_merges_the_namespace_layer_with_the_inner_report() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let report_path = dir.path().join("report.json");
        // What the inner trampoline reports with Landlock ABI ≥ 5 and no
        // network rules (the namespace's job).
        let inner = SandboxReport::uniform(S::Enforced);
        let mut bytes = serde_json::to_vec(&inner).map_err(ctx("encode report"))?;
        bytes.push(b'\n');
        std::fs::write(&report_path, bytes).map_err(ctx("write report"))?;
        let bwrap_report = SandboxReport {
            filesystem: S::Partial,
            network: S::Enforced,
            no_new_privs: S::Enforced,
            capabilities: S::Enforced,
            resource_limits: S::NotEnforced,
        };
        let after = AfterExit {
            trampoline_report: Some(report_path.clone()),
            merge_with: Some(bwrap_report),
            attempt_dir: None,
            // The cgroup domain fell short: capped below the inner report.
            resource_limits: S::Partial,
            recovered_cgroup: None,
            note: None,
            leftovers: LeftoverRegistry::default(),
        };
        let merged = after.late_report().ok_or(TestError::Missing("report"))?;
        assert_eq!(
            merged,
            SandboxReport {
                resource_limits: S::Partial,
                ..SandboxReport::uniform(S::Enforced)
            }
        );
        assert_eq!(
            merged,
            harw_job_exec::merge_reports(
                &bwrap_report,
                &SandboxReport {
                    resource_limits: S::Partial,
                    ..inner
                }
            )
        );
        assert!(!report_path.exists(), "the report is consumed");
        // No report (e.g. bwrap failed before the inner stage): unknown.
        assert_eq!(after.late_report(), None);
        Ok(())
    }

    #[test]
    fn late_report_reads_only_a_regular_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let elsewhere = dir.path().join("forged.json");
        let mut bytes =
            serde_json::to_vec(&SandboxReport::uniform(S::Enforced)).map_err(ctx("encode"))?;
        bytes.push(b'\n');
        std::fs::write(&elsewhere, bytes).map_err(ctx("write forged report"))?;
        let bwrap_report = SandboxReport {
            filesystem: S::Partial,
            resource_limits: S::NotEnforced,
            ..SandboxReport::uniform(S::Enforced)
        };
        let after = |report: &Path| AfterExit {
            trampoline_report: Some(report.to_path_buf()),
            merge_with: Some(bwrap_report),
            attempt_dir: None,
            resource_limits: S::Enforced,
            recovered_cgroup: None,
            note: None,
            leftovers: LeftoverRegistry::default(),
        };
        // A symlink in the report's place is not followed (it could name a
        // FIFO that blocks the reader, or a forged report).
        let link = dir.path().join("report.json");
        std::os::unix::fs::symlink(&elsewhere, &link).map_err(ctx("symlink"))?;
        assert_eq!(after(&link).late_report(), None);
        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "the planted entry is removed"
        );
        // Neither is any other non-regular file.
        let not_a_file = dir.path().join("dir-report.json");
        std::fs::create_dir(&not_a_file).map_err(ctx("mkdir"))?;
        assert_eq!(after(&not_a_file).late_report(), None);
        Ok(())
    }

    #[test]
    fn start_cleans_up_when_the_output_files_cannot_be_opened() -> TestResult {
        use crate::coordinator::executor::{Executor as _, OutputFiles};
        let (dir, workspace, plans) = dirs()?;
        let executor = LinuxExecutor::new(LinuxExecutorOptions {
            sandbox: LinuxSandboxBackend::BwrapTrampoline {
                bwrap: bwrap(),
                trampoline: PathBuf::from(TRAMPOLINE),
                plan_dir: plans.clone(),
            },
            ..LinuxExecutorOptions::default()
        })
        .map_err(ctx("executor"))?;
        let missing = dir.path().join("missing");
        let context = AttemptContext {
            output_files: Some(OutputFiles {
                stdout: missing.join("stdout"),
                stderr: missing.join("stderr"),
            }),
            ..attempt_context(&workspace)?
        };
        let spec = spec(SandboxRequirement::BestEffort, rlimited())?;
        match executor.start(&spec, &context) {
            Err(RuntimeError::Os { operation, .. }) => {
                assert_eq!(operation, "open output file");
            }
            Err(other) => return Err(TestError::Unexpected(format!("{other}"))),
            Ok(_) => return Err(TestError::Unexpected("attempt started".to_owned())),
        }
        let left = std::fs::read_dir(&plans)
            .map_err(ctx("read plan dir"))?
            .count();
        assert_eq!(
            left, 0,
            "the attempt directory (with both plans) is removed"
        );
        Ok(())
    }

    #[test]
    fn bwrap_trampoline_paths_must_be_absolute() {
        for (trampoline, plan_dir) in [("harw-job-exec", "/plans"), ("/bin/harw-job-exec", "plans")]
        {
            let result = LinuxExecutor::new(LinuxExecutorOptions {
                sandbox: LinuxSandboxBackend::BwrapTrampoline {
                    bwrap: bwrap(),
                    trampoline: PathBuf::from(trampoline),
                    plan_dir: PathBuf::from(plan_dir),
                },
                ..LinuxExecutorOptions::default()
            });
            assert!(
                matches!(result, Err(RuntimeError::InvalidConfig { .. })),
                "{trampoline} {plan_dir}"
            );
        }
        assert_eq!(LinuxSandboxBackend::None.kind(), BackendKind::None);
        assert_eq!(
            LinuxSandboxBackend::Bwrap(bwrap()).kind(),
            BackendKind::Bwrap
        );
        assert_eq!(
            LinuxSandboxBackend::LandlockTrampoline {
                trampoline: PathBuf::from(TRAMPOLINE),
                plan_dir: PathBuf::from("/plans"),
            }
            .kind(),
            BackendKind::LandlockTrampoline
        );
    }
}
