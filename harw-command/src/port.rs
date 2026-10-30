//! The port, the request/outcome vocabulary and the job-runtime adapter.
//!
//! # Workspace sandbox requests (PL-97 E2b)
//! [`CommandSandbox::Workspace`] runs a command in a sandbox over the
//! caller's own workspace root, with the caller's Bubblewrap modules and an
//! optional per-job write scope. The host port's runtime has `/` as its
//! workspace root, and both Bubblewrap backends refuse a host-root
//! workspace. So the host port routes such a request through
//! [`HostCommandExecutor`], which starts the attempt with the request's own
//! root, on a backend that honours the profile's network:
//!
//! | Profile network | Backend |
//! |---|---|
//! | denied (`no_network`, `read_only_analysis`) | `select_backend` for this host, as for every other sandboxed request |
//! | allowed or restricted (`workspace_build`, `network_restricted`) | `select_backend` with `needs_network`: no egress relay is configured, so both Bubblewrap backends are skipped (Landlock trampoline, else none) |
//!
//! The request travels as a [`JobSpec`] marked with a fresh idempotency key
//! (never reused, so it deduplicates nothing). The port registers the
//! request's root and modules under that key before it submits, and the
//! executor takes them when the attempt starts. An attempt whose key has
//! no registration is refused: nothing runs with the runtime's `/` root.
//! [`CommandPort::workspace_backend`] tells a caller which backend its
//! request would run on.
//!
//! What a workspace-marked key means:
//! - it never deduplicates: each submission gets a fresh key, so a second
//!   submission of the same command is a second job;
//! - the registration lives in this process's memory only: a persisted
//!   record replayed after a restart has a key without registration and is
//!   refused, never run;
//! - the key carries no meaning outside this port.
//!
//! This side channel is an interim mechanism (PL-97 E2b, pending owner
//! approval as a plan §8 amendment): the intended replacement is a typed
//! per-spec workspace root validated by the coordinator, with the modules
//! as typed executor input.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use harw_job::{
    BackendKind, CancellationCause, CoordinatorStore, Executor, ExitOutcome, FrameEvent,
    IdempotencyKey, JobFrame, JobResult, JobRuntime, JobSpec, LifecycleState, OutputFiles,
    Persistence, ResourceRequest, SandboxProfileName, SandboxRequirement, StdioHandoff,
    SubmitOptions, WorkspacePath,
};
use harw_job_core::spec::JobWriteScope;
use harw_sandbox::{HostPathBinding, SandboxProfile};
use harw_types::cancel::CancelToken;
use jiff::SignedDuration;

/// How long buffered frames are drained after the job reported its result.
const DRAIN: Duration = Duration::from_millis(250);

/// How long [`CommandPort::start`] waits for the job to report its PID.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// Prefix of the idempotency key that marks a workspace sandbox request
/// (module docs).
const WORKSPACE_KEY_PREFIX: &str = "harw-command-workspace-";

/// How a command is confined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSandbox {
    /// Directly on the host, no sandbox (the user's own `!` commands).
    Host,
    /// A named profile at the given strictness, over the runtime's own
    /// workspace root (`/` for the host port).
    Profile(SandboxProfileName, SandboxRequirement),
    /// A named profile over the caller's workspace, with its modules and
    /// write scope (PL-97 E2b, module docs). A port that cannot run it
    /// fails the request ([`CommandEnd::Failed`]); nothing runs.
    Workspace(Box<WorkspaceSandbox>),
}

/// A sandbox over the caller's own workspace ([`CommandSandbox::Workspace`]).
///
/// # Description
/// `root` is the job's workspace root: the profile's workspace grant, the
/// Bubblewrap workspace bind and the write scope all refer to it, and
/// [`CommandRequest::cwd`] must lie inside it. The modules are trusted
/// runtime input (the caller's own sandbox profile), never tool arguments;
/// they apply to the Bubblewrap backends. A write scope always runs under
/// [`SandboxRequirement::Required`], whatever `requirement` says
/// (`JobSpecBuilder::write_scope`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSandbox {
    /// Absolute workspace root of the job.
    pub root: PathBuf,
    /// The profile the job runs under.
    pub profile: SandboxProfileName,
    /// How strictly the profile must be enforced.
    pub requirement: SandboxRequirement,
    /// Optional per-job write scope (absolute paths inside `root`).
    pub write_scope: Option<JobWriteScope>,
    /// Cargo or tmux module of the caller's sandbox profile.
    pub modules: SandboxProfile,
    /// Host `PATH` binding of the caller.
    pub host_path: Option<HostPathBinding>,
    /// Size of the sandbox's `/tmp`.
    pub tmpfs_size: Option<NonZeroU64>,
}

impl WorkspaceSandbox {
    /// `profile` at `requirement` over `root`, without modules, host `PATH`,
    /// `/tmp` limit or write scope.
    #[must_use]
    pub fn new(
        root: impl Into<PathBuf>,
        profile: SandboxProfileName,
        requirement: SandboxRequirement,
    ) -> Self {
        Self {
            root: root.into(),
            profile,
            requirement,
            write_scope: None,
            modules: SandboxProfile::Strict,
            host_path: None,
            tmpfs_size: None,
        }
    }
}

/// Whether `profile` lets the job reach the network (module docs).
#[must_use]
fn needs_network(profile: SandboxProfileName) -> bool {
    match profile {
        SandboxProfileName::WorkspaceBuild | SandboxProfileName::NetworkRestricted => true,
        SandboxProfileName::ReadOnlyAnalysis | SandboxProfileName::NoNetwork => false,
    }
}

/// Where a command's output goes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CommandOutput {
    /// Collected in memory under [`CommandRequest::max_output_bytes`].
    #[default]
    Capture,
    /// Appended to these files by the runtime (they outlive this process; the
    /// outcome carries no output).
    Files {
        /// Standard output.
        stdout: PathBuf,
        /// Standard error.
        stderr: PathBuf,
    },
    /// Standard output is handed to the caller as a pipe
    /// ([`StartedCommand::stdout`]); standard error is appended to a file.
    StdoutPipe {
        /// Standard error.
        stderr: PathBuf,
    },
}

/// What the command's standard input is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommandStdin {
    /// `/dev/null`.
    #[default]
    Null,
    /// A pipe handed to the caller ([`StartedCommand::stdin`]). What the
    /// caller writes (a password, an interactive protocol) never becomes part
    /// of the job record.
    Pipe,
}

/// A command to run.
#[derive(Debug, Clone)]
pub struct CommandRequest {
    /// Program to execute (looked up through the environment's `PATH`).
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Absolute working directory.
    pub cwd: PathBuf,
    /// The complete environment of the process (nothing is inherited).
    pub env: Vec<(String, String)>,
    /// Wall-clock limit.
    pub timeout: Duration,
    /// Shared byte budget for stdout and stderr together.
    pub max_output_bytes: usize,
    /// Resource limits (rlimits, cgroup).
    pub limits: ResourceRequest,
    /// Confinement.
    pub sandbox: CommandSandbox,
    /// How much of the request the job record keeps; the environment is
    /// never persisted for [`Persistence::Ephemeral`] and
    /// [`Persistence::MetadataOnly`].
    pub persistence: Persistence,
    /// Where the output goes.
    pub output: CommandOutput,
    /// What standard input is.
    pub stdin: CommandStdin,
    /// Also deliver the live output as frames ([`StartedCommand::frames`]),
    /// for [`CommandPort::start`] with [`CommandOutput::Capture`].
    pub stream: bool,
}

impl CommandRequest {
    /// A host command with the given argv, working directory and timeout;
    /// ephemeral, empty environment, 1 MiB of output.
    #[must_use]
    pub fn new(program: impl Into<String>, cwd: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            env: Vec::new(),
            timeout,
            max_output_bytes: 1024 * 1024,
            limits: ResourceRequest::default(),
            sandbox: CommandSandbox::Host,
            persistence: Persistence::Ephemeral,
            output: CommandOutput::Capture,
            stdin: CommandStdin::Null,
            stream: false,
        }
    }

    /// The [`JobSpec`] the job runtime runs for this request.
    ///
    /// # Description
    /// The working directory is [`Self::cwd`] relative to the workspace
    /// root: `/` for [`CommandSandbox::Host`] and
    /// [`CommandSandbox::Profile`], the sandbox's own root for
    /// [`CommandSandbox::Workspace`]. The timeout becomes the attempt's
    /// wall-clock limit, [`Self::limits`] its resource request, the
    /// environment is exactly [`Self::env`]. A host command has
    /// [`SandboxRequirement::None`]. A workspace request carries its write
    /// scope, which makes it [`SandboxRequirement::Required`]. The
    /// idempotency key a port adds to mark the request is not part of this
    /// spec.
    ///
    /// # Errors
    /// A relative working directory, one outside the workspace root, a
    /// relative workspace root, a path that is not UTF-8, a timeout that
    /// does not fit, or a spec that fails validation.
    pub fn job_spec(&self) -> Result<JobSpec, String> {
        let root = match &self.sandbox {
            CommandSandbox::Workspace(workspace) => {
                if !workspace.root.is_absolute() {
                    return Err(format!(
                        "workspace root {:?} is not absolute",
                        workspace.root
                    ));
                }
                workspace.root.as_path()
            }
            CommandSandbox::Host | CommandSandbox::Profile(..) => Path::new("/"),
        };
        if !self.cwd.is_absolute() {
            return Err(format!("working directory {:?} is not absolute", self.cwd));
        }
        let relative = self.cwd.strip_prefix(root).map_err(|_| {
            format!(
                "working directory {:?} lies outside the workspace root {root:?}",
                self.cwd
            )
        })?;
        let working_dir = if relative.as_os_str().is_empty() {
            WorkspacePath::root()
        } else {
            let text = relative
                .to_str()
                .ok_or_else(|| "working directory is not valid UTF-8".to_owned())?;
            WorkspacePath::new(text).map_err(|error| error.to_string())?
        };
        let timeout =
            SignedDuration::try_from(self.timeout).map_err(|error| format!("timeout: {error}"))?;
        let (profile, requirement) = match &self.sandbox {
            CommandSandbox::Host => (SandboxProfileName::WorkspaceBuild, SandboxRequirement::None),
            CommandSandbox::Profile(profile, requirement) => (*profile, *requirement),
            CommandSandbox::Workspace(workspace) => (workspace.profile, workspace.requirement),
        };
        let mut builder = JobSpec::command(self.program.clone())
            .args(self.args.iter().cloned())
            .workspace(working_dir)
            .resources(self.limits.clone())
            .timeout(timeout)
            .sandbox(profile)
            .sandbox_requirement(requirement);
        if let CommandSandbox::Workspace(workspace) = &self.sandbox
            && let Some(scope) = &workspace.write_scope
        {
            builder = builder.write_scope(scope.clone());
        }
        for (name, value) in &self.env {
            builder = builder.env(name.clone(), value.clone());
        }
        builder.build().map_err(|error| error.to_string())
    }
}

/// How a command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandEnd {
    /// The process exited; see [`CommandOutcome::exit_code`].
    Exited,
    /// The process was killed by a signal.
    Signaled(i32),
    /// The wall-clock limit elapsed; the process tree was killed.
    TimedOut,
    /// The output budget was exceeded; the process tree was killed.
    OutputLimit,
    /// The caller cancelled; the process tree was killed.
    Cancelled,
    /// The job could not run (spawn, sandbox, admission, runtime error).
    Failed(String),
}

/// The stream a shared output budget cut first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// What a command produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    /// How it ended.
    pub end: CommandEnd,
    /// Exit code (`128 + signal` after a signal, `-1` if there was none).
    pub exit_code: i32,
    /// Standard output.
    pub stdout: Vec<u8>,
    /// Standard error.
    pub stderr: Vec<u8>,
    /// Both streams in arrival order.
    pub combined: Vec<u8>,
    /// Output was dropped (budget or slow consumer).
    pub truncated: bool,
    /// The stream the shared output budget cut first. `Some` whenever the port
    /// itself truncated the output (`truncated` as set by the collector);
    /// `None` when nothing was truncated or the provenance is unknown. The
    /// stdout and stderr lengths do not identify the cut stream: the budget is
    /// spent across both streams in frame-arrival order, so the shorter stream
    /// can be the one that was cut.
    pub truncated_stream: Option<OutputStream>,
    /// Frames this consumer missed (the output is then head/tail of the job
    /// result, not the live stream).
    pub lagged: u64,
    /// Wall-clock duration of the whole run.
    pub elapsed: Duration,
}

impl CommandOutcome {
    /// Combined output as lossy text.
    #[must_use]
    pub fn combined_text(&self) -> String {
        String::from_utf8_lossy(&self.combined).into_owned()
    }

    /// Whether the process exited with status 0.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.end == CommandEnd::Exited && self.exit_code == 0
    }

    fn failed(reason: impl Into<String>, started: Instant) -> Self {
        Self {
            end: CommandEnd::Failed(reason.into()),
            exit_code: -1,
            stdout: Vec::new(),
            stderr: Vec::new(),
            combined: Vec::new(),
            truncated: false,
            truncated_stream: None,
            lagged: 0,
            elapsed: started.elapsed(),
        }
    }
}

/// The future a [`CommandPort`] returns.
pub type RunFuture<'a> = Pin<Box<dyn Future<Output = CommandOutcome> + Send + 'a>>;

/// A command the runtime has started and that keeps running.
pub struct StartedCommand {
    /// The command's stdin when [`CommandStdin::Pipe`] was requested.
    pub stdin: Option<tokio::process::ChildStdin>,
    /// The command's stdout when [`CommandOutput::StdoutPipe`] was requested.
    pub stdout: Option<tokio::process::ChildStdout>,
    /// Live output when [`CommandRequest::stream`] was set.
    pub frames: Option<CommandFrames>,
    /// Stops the command (its whole process tree) through the runtime.
    pub cancel: Box<dyn Fn() + Send + Sync>,
    /// Diagnostic PID of the primary process (it leads its own session and
    /// process group), if known.
    pub pid: Option<u32>,
    /// Resolves when the command ended. Dropping it does not stop the command.
    pub done: RunFuture<'static>,
    /// The dedicated cgroup the runtime created for this attempt and
    /// attached its primary process to, beneath a delegated cgroup root
    /// (PL-97 E4). `None` whenever no such cgroup was created (no delegated
    /// root, a port without a Linux executor, or an attachment that could
    /// not be verified): fail-closed, never guessed.
    pub cgroup: Option<AttemptCgroup>,
}

/// What the runtime attests about the dedicated cgroup of a started attempt
/// (PL-97 E4): the attempt id it named the cgroup after
/// (`harw-job-<attempt>`) and the delegated root it created it directly
/// beneath. A job record turns this into its dedicated-cgroup proof; it
/// belongs to exactly the launch that returned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptCgroup {
    /// The attempt id the runtime assigned (`<work>-e<epoch>`).
    pub attempt_id: String,
    /// The delegated cgroup root, hierarchy-relative (e.g. `/harw.slice`),
    /// the direct parent of the attempt cgroup.
    pub delegated_root: String,
}

/// Most attested attempt cgroups the host port holds at once (attempts
/// whose start has not been collected yet); beyond, the oldest attestation
/// is dropped (that job simply gets no proof, fail-closed).
const MAX_ATTESTED_CGROUPS: usize = 4096;

/// Attempt id → delegated root of the attempts the host executor started
/// in their own cgroup (PL-97 E4), from the executor's start until
/// [`JobCommandPort::start`] collects it.
///
/// # Description
/// An attestation outlives its attempt: a command that exits before the
/// port has read its `AttemptStarted` frame still ran in its own cgroup,
/// and its job keeps the proof. Bounded by [`MAX_ATTESTED_CGROUPS`],
/// evicting the oldest entries first (attempt ids are never reused).
#[derive(Debug, Default)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct AttemptCgroups {
    attested: Mutex<AttestedLedger>,
}

/// The entries of [`AttemptCgroups`] and their insertion order.
#[derive(Debug, Default)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct AttestedLedger {
    roots: HashMap<String, String>,
    order: VecDeque<String>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl AttemptCgroups {
    /// Records that `attempt_id` runs in its own cgroup below `root`.
    fn record(&self, attempt_id: &str, root: String) {
        let mut ledger = self.attested.lock().unwrap_or_else(PoisonError::into_inner);
        if ledger.roots.insert(attempt_id.to_owned(), root).is_some() {
            return;
        }
        ledger.order.push_back(attempt_id.to_owned());
        while ledger.roots.len() > MAX_ATTESTED_CGROUPS {
            let Some(oldest) = ledger.order.pop_front() else {
                break;
            };
            if ledger.roots.remove(&oldest).is_some() {
                tracing::warn!(
                    attempt = oldest.as_str(),
                    "too many uncollected job cgroup attestations; the oldest gets no proof"
                );
            }
        }
    }

    /// Takes the attestation of `attempt_id`, if any.
    fn take(&self, attempt_id: &str) -> Option<AttemptCgroup> {
        let mut ledger = self.attested.lock().unwrap_or_else(PoisonError::into_inner);
        let delegated_root = ledger.roots.remove(attempt_id)?;
        ledger.order.retain(|id| id != attempt_id);
        Some(AttemptCgroup {
            attempt_id: attempt_id.to_owned(),
            delegated_root,
        })
    }

    /// Number of held attestations (tests).
    #[cfg(test)]
    fn len(&self) -> usize {
        self.attested
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .roots
            .len()
    }
}

/// The cgroup leaf the Linux executor creates for `attempt_id`:
/// `harw-job-<attempt>` with every character outside `[A-Za-z0-9_.-]`
/// replaced by `_` (mirrors `cgroup_name` in `harw-job-runtime`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn attempt_cgroup_leaf(attempt_id: &str) -> String {
    let attempt: String = attempt_id
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

/// The delegated root to attest for a started attempt (PL-97 E4), or `None`.
///
/// `configured_root` is the executor's delegated cgroup root (a path under
/// the cgroup-v2 mount; `None`: the executor creates no job cgroups) and
/// `observed` the hierarchy-relative cgroup of the attempt's primary process
/// captured after it was attached (`0::<path>` of `/proc/<pid>/cgroup`).
/// Only when the observed cgroup is exactly `harw-job-<attempt>` and its
/// parent is the configured root (component-wise, never the hierarchy root)
/// is the parent returned; anything else fails closed.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn attested_delegated_root(
    configured_root: Option<&Path>,
    attempt_id: &str,
    observed: Option<&str>,
) -> Option<String> {
    let configured_root = configured_root.filter(|root| root.is_absolute())?;
    let rest = observed?.strip_prefix('/')?;
    if attempt_id.is_empty() || rest.is_empty() {
        return None;
    }
    let parts: Vec<&str> = rest.split('/').collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return None;
    }
    let (leaf, parent) = parts.split_last()?;
    if parent.is_empty() || *leaf != attempt_cgroup_leaf(attempt_id) {
        return None;
    }
    let parent = parent.join("/");
    configured_root
        .ends_with(Path::new(&parent))
        .then(|| format!("/{parent}"))
}

/// One piece of live output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandFrame {
    /// A chunk of standard output.
    Stdout(Vec<u8>),
    /// A chunk of standard error.
    Stderr(Vec<u8>),
    /// The consumer fell behind; this many frames were dropped for it.
    Lagged(u64),
}

/// A subscription to a started command's live output (lossy by design).
#[derive(Debug)]
pub struct CommandFrames {
    inner: harw_job::JobFrames,
}

impl CommandFrames {
    /// The next piece of output; `None` once the job is finished.
    pub async fn next(&mut self) -> Option<CommandFrame> {
        loop {
            match self.inner.next().await? {
                FrameEvent::Frame(JobFrame::Stdout(chunk)) => {
                    return Some(CommandFrame::Stdout(chunk.to_vec()));
                }
                FrameEvent::Frame(JobFrame::Stderr(chunk)) => {
                    return Some(CommandFrame::Stderr(chunk.to_vec()));
                }
                FrameEvent::Lagged(missed) => return Some(CommandFrame::Lagged(missed)),
                _ => {}
            }
        }
    }
}

impl std::fmt::Debug for StartedCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartedCommand")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// The future [`CommandPort::start`] returns.
pub type StartFuture<'a> =
    Pin<Box<dyn Future<Output = Result<StartedCommand, String>> + Send + 'a>>;

/// Runs commands. The only way application code starts a workload process.
pub trait CommandPort: std::fmt::Debug + Send + Sync {
    /// Runs `request` to completion or until `cancel` fires.
    fn run(&self, request: CommandRequest, cancel: CancelToken) -> RunFuture<'_>;

    /// Starts `request` and returns once the process runs. The wall-clock
    /// limit of the request still applies. Output goes where
    /// [`CommandRequest::output`] says.
    fn start(&self, request: CommandRequest) -> StartFuture<'_>;

    /// The sandbox backend a [`CommandSandbox::Workspace`] request with
    /// `profile` runs on (PL-97 E2b). `None`: this port runs no workspace
    /// requests (they fail without running), or cannot say which backend
    /// they get. Callers use it to keep their own path instead of
    /// submitting where the sandbox would be weaker.
    fn workspace_backend(&self, profile: SandboxProfileName) -> Option<BackendKind> {
        let _ = profile;
        None
    }
}

/// [`CommandPort`] over a [`JobRuntime`].
pub struct JobCommandPort<S, E> {
    runtime: JobRuntime<S, E>,
    /// Workspace requests: registrations and backend kinds (host port only).
    router: Option<Arc<WorkspaceRouter>>,
    /// Attempts the host executor started in their own cgroup (host port
    /// on Linux only; PL-97 E4).
    cgroups: Option<Arc<AttemptCgroups>>,
}

/// What a workspace request's attempt needs besides its spec (module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceLaunch {
    /// The job's workspace root.
    root: PathBuf,
    /// The profile reaches the network ([`needs_network`]).
    networked: bool,
    /// Cargo or tmux module.
    modules: SandboxProfile,
    /// Host `PATH` binding.
    host_path: Option<HostPathBinding>,
    /// Size of `/tmp`.
    tmpfs_size: Option<NonZeroU64>,
}

/// The registrations of workspace requests between submission and attempt
/// start, shared by the host port and its [`HostCommandExecutor`].
#[derive(Debug)]
struct WorkspaceRouter {
    /// Backend of requests whose profile denies the network.
    offline: BackendKind,
    /// Backend of requests whose profile reaches the network.
    online: BackendKind,
    launches: Mutex<HashMap<String, WorkspaceLaunch>>,
    counter: AtomicU64,
}

impl WorkspaceRouter {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn new(offline: BackendKind, online: BackendKind) -> Self {
        Self {
            offline,
            online,
            launches: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(0),
        }
    }

    fn backend(&self, profile: SandboxProfileName) -> BackendKind {
        if needs_network(profile) {
            self.online
        } else {
            self.offline
        }
    }

    /// Registers `launch` under a fresh key; the guard drops the
    /// registration unless the attempt took it first.
    fn register(self: &Arc<Self>, launch: WorkspaceLaunch) -> LaunchGuard {
        let sequence = self.counter.fetch_add(1, Ordering::Relaxed);
        let key = format!("{WORKSPACE_KEY_PREFIX}{}-{sequence}", std::process::id());
        self.launches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.clone(), launch);
        LaunchGuard {
            router: Arc::clone(self),
            key,
        }
    }

    /// Takes the registration of `key` (once).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn take(&self, key: &str) -> Option<WorkspaceLaunch> {
        self.launches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key)
    }
}

/// Removes a registration that no attempt took (failed submission, or an
/// attempt that never started).
struct LaunchGuard {
    router: Arc<WorkspaceRouter>,
    key: String,
}

impl Drop for LaunchGuard {
    fn drop(&mut self) {
        self.router
            .launches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.key);
    }
}

impl<S, E> std::fmt::Debug for JobCommandPort<S, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JobCommandPort")
    }
}

/// Directory below the host port's `state_dir` for the plan and report
/// files of the trampoline sandbox backends.
#[cfg(target_os = "linux")]
const SANDBOX_PLAN_DIR: &str = "sandbox-plans";

/// The sandbox backend of the host port: [`harw_job::select_backend`] for
/// this host, with a private plan directory below `state_dir` and every
/// other preference at its default (trampoline next to the running
/// executable, trusted `bwrap` at its fixed paths). Logs the choice and
/// every skipped candidate once (the selection for jobs whose profile
/// denies the network, [`host_backends`]).
#[cfg(all(test, target_os = "linux"))]
fn host_backend(state_dir: &std::path::Path) -> harw_job::BackendSelection {
    host_backends(state_dir).0
}

/// The host port's two selections (PL-97 E2b): for jobs whose profile
/// denies the network, and for jobs that reach it (`needs_network`, so the
/// Bubblewrap backends, which have no egress relay here, are skipped).
/// Logs both once.
#[cfg(all(test, target_os = "linux"))]
fn host_backends(
    state_dir: &std::path::Path,
) -> (harw_job::BackendSelection, harw_job::BackendSelection) {
    host_backends_with(state_dir, &HostPortOptions::default(), None)
}

/// [`host_backends`] with the trampoline override of `options` and the
/// host probed against `cgroup_root` (the resolved delegated root).
#[cfg(target_os = "linux")]
fn host_backends_with(
    state_dir: &std::path::Path,
    options: &HostPortOptions,
    cgroup_root: Option<&std::path::Path>,
) -> (harw_job::BackendSelection, harw_job::BackendSelection) {
    let plan_dir = state_dir.join(SANDBOX_PLAN_DIR);
    let plan_dir = match create_private_dir(&plan_dir) {
        Ok(()) => Some(plan_dir),
        Err(error) => {
            tracing::warn!(
                path = %plan_dir.display(),
                %error,
                "no private sandbox plan directory; trampoline backends are unavailable"
            );
            None
        }
    };
    let host = harw_job::HostReport::probe_with_cgroup_root(cgroup_root);
    let trampoline = match &options.trampoline {
        Some(path) => harw_job::ExecutableSource::Path(path.clone()),
        None => harw_job::ExecutableSource::Discover,
    };
    let offline = harw_job::select_backend(
        &host,
        &harw_job::BackendPreferences {
            plan_dir: plan_dir.clone(),
            trampoline: trampoline.clone(),
            ..harw_job::BackendPreferences::default()
        },
    );
    let online = harw_job::select_backend(
        &host,
        &harw_job::BackendPreferences {
            plan_dir,
            trampoline,
            needs_network: true,
            ..harw_job::BackendPreferences::default()
        },
    );
    tracing::info!(
        backend = %offline.chosen,
        reasons = ?offline.reasons,
        "command port sandbox backend"
    );
    tracing::info!(
        backend = %online.chosen,
        reasons = ?online.reasons,
        "command port sandbox backend for jobs that reach the network"
    );
    (offline, online)
}

/// Creates `dir` (and parents) and restricts it to `0700`.
#[cfg(target_os = "linux")]
fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Trusted inputs of the host port (PL-97 E3), taken from the merged config
/// by the composition root: `[jobs] cgroup_root` and `[jobs] trampoline`.
/// Both come from trusted layers only (`harw_config` drops them from an
/// untrusted project layer). The default is "nothing configured": the
/// delegated root then comes from `HARW_CGROUP_ROOT` or the own delegated
/// cgroup, the trampoline from next to the running executable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostPortOptions {
    /// `[jobs] cgroup_root`.
    pub cgroup_root: Option<PathBuf>,
    /// `[jobs] trampoline` (absolute path of `harw-job-exec`).
    pub trampoline: Option<PathBuf>,
}

/// The delegated cgroup root of the host port (PL-97 E3):
/// `harw_job_runtime::host::resolve_delegated_root`, confirmed delegated by
/// the host probe. A refused, missing or undelegated root means no cgroup
/// (today's behaviour), logged once with the reason or hint.
#[cfg(target_os = "linux")]
fn host_cgroup_root(options: &HostPortOptions) -> Option<PathBuf> {
    use harw_job_runtime::host::{RootConfig, RootEnv, resolve_delegated_root};
    let config = RootConfig {
        cgroup_root: options.cgroup_root.clone(),
    };
    let resolution = match resolve_delegated_root(&config, &RootEnv::from_process()) {
        Ok(resolution) => resolution,
        Err(error) => {
            tracing::warn!(%error, "cgroup root refused; jobs run without their own cgroup");
            return None;
        }
    };
    let Some(root) = resolution.root else {
        tracing::info!(
            hint = resolution.hint.as_deref().unwrap_or_default(),
            "command port without a delegated cgroup root"
        );
        return None;
    };
    if harw_job::HostReport::probe_with_cgroup_root(Some(&root)).cgroup_v2_delegated {
        tracing::info!(
            root = %root.display(),
            source = %resolution.source,
            "command port delegated cgroup root"
        );
        Some(root)
    } else {
        tracing::warn!(
            root = %root.display(),
            source = %resolution.source,
            "cgroup root is not a delegated cgroup v2 root (missing or not writable); jobs run \
             without their own cgroup"
        );
        None
    }
}

#[cfg(target_os = "linux")]
impl JobCommandPort<harw_job::FsJobRecordStore, HostCommandExecutor> {
    /// The host runtime: file-backed job records below `state_dir` and the
    /// Linux executor (pidfd + process group, host commands in a new
    /// session) with the sandbox backend [`harw_job::select_backend`] picks
    /// for this host (PL-97 E1): Bubblewrap ∘ trampoline, else Bubblewrap,
    /// else the Landlock trampoline, else none. Plan files of the
    /// trampoline backends live in `<state_dir>/sandbox-plans` (`0700`).
    ///
    /// The backend applies to sandboxed requests ([`CommandSandbox::Profile`])
    /// only; host commands ([`CommandSandbox::Host`]) run exactly as before.
    /// [`CommandSandbox::Workspace`] requests run on their own root
    /// (module docs, PL-97 E2b). The cgroup root is resolved as in
    /// [`JobCommandPort::host_with`] with nothing configured.
    ///
    /// # Errors
    /// The store or the runtime cannot be created.
    pub fn host(state_dir: &std::path::Path) -> Result<Self, String> {
        Self::host_with(state_dir, &HostPortOptions::default())
    }

    /// [`JobCommandPort::host`] with the trusted config inputs of
    /// `options` (PL-97 E3): the delegated cgroup root
    /// (`[jobs] cgroup_root` → `HARW_CGROUP_ROOT` → own delegated cgroup)
    /// goes into `LinuxExecutorOptions::cgroup_root` of every executor of
    /// the port, so each job gets its own `harw-job-<attempt>` cgroup below
    /// it; the trampoline override goes into the backend selection. No
    /// usable root keeps the no-cgroup behaviour (logged once); a root the
    /// executor cannot open is dropped with a warning instead of leaving
    /// the process without a job runtime.
    ///
    /// # Errors
    /// The store or the runtime cannot be created.
    pub fn host_with(
        state_dir: &std::path::Path,
        options: &HostPortOptions,
    ) -> Result<Self, String> {
        let store = harw_job::FsJobRecordStore::create_ambient(state_dir)
            .map_err(|error| format!("job store {}: {error}", state_dir.display()))?;
        let cgroup_root = host_cgroup_root(options);
        let (offline, online) = host_backends_with(state_dir, options, cgroup_root.as_deref());
        let router = Arc::new(WorkspaceRouter::new(offline.chosen, online.chosen));
        let cgroups = Arc::new(AttemptCgroups::default());
        let template = |cgroup_root: Option<PathBuf>| harw_job::LinuxExecutorOptions {
            new_session: true,
            cgroup_root,
            ..harw_job::LinuxExecutorOptions::default()
        };
        let executor = match HostCommandExecutor::new(
            template(cgroup_root.clone()),
            offline.backend.clone(),
            online.backend.clone(),
            Arc::clone(&router),
            Arc::clone(&cgroups),
        ) {
            Ok(executor) => executor,
            Err(error) if cgroup_root.is_some() => {
                tracing::warn!(
                    %error,
                    "delegated cgroup root unusable; jobs run without their own cgroup"
                );
                HostCommandExecutor::new(
                    template(None),
                    offline.backend,
                    online.backend,
                    Arc::clone(&router),
                    Arc::clone(&cgroups),
                )
                .map_err(|error| format!("job executor: {error}"))?
            }
            Err(error) => return Err(format!("job executor: {error}")),
        };
        let runtime = JobRuntime::builder()
            .store(store)
            .executor(executor)
            .workspace_root("/")
            .build()
            .map_err(|error| format!("job runtime: {error}"))?;
        Ok(Self {
            runtime,
            router: Some(router),
            cgroups: Some(cgroups),
        })
    }
}

/// Most distinct executors (module set × network class) the host port
/// keeps for workspace requests; a request beyond is refused.
#[cfg(target_os = "linux")]
const MAX_WORKSPACE_EXECUTORS: usize = 16;

/// The executor of the host port (PL-97 E2b): a Linux executor for host
/// commands and [`CommandSandbox::Profile`] requests, and one Linux
/// executor per module set and network class for
/// [`CommandSandbox::Workspace`] requests, each started with the request's
/// own workspace root (module docs).
#[cfg(target_os = "linux")]
pub struct HostCommandExecutor {
    /// Host commands and profile requests over the runtime's root.
    host: harw_job::LinuxExecutor,
    /// Options of every per-request executor, the sandbox aside.
    template: harw_job::LinuxExecutorOptions,
    /// Backend for workspace jobs whose profile denies the network.
    offline: harw_job::LinuxSandboxBackend,
    /// Backend for workspace jobs whose profile reaches the network.
    online: harw_job::LinuxSandboxBackend,
    router: Arc<WorkspaceRouter>,
    /// Per-request executors by launch shape (bounded).
    workspace: Mutex<Vec<(WorkspaceLaunch, Arc<harw_job::LinuxExecutor>)>>,
    /// The executor of every running workspace attempt, by attempt id.
    running: Mutex<HashMap<String, Arc<harw_job::LinuxExecutor>>>,
    /// Attempts started in their own cgroup (PL-97 E4), shared with the port.
    cgroups: Arc<AttemptCgroups>,
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for HostCommandExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostCommandExecutor")
            .field("offline", &self.offline.kind())
            .field("online", &self.online.kind())
            .finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
impl HostCommandExecutor {
    fn new(
        template: harw_job::LinuxExecutorOptions,
        offline: harw_job::LinuxSandboxBackend,
        online: harw_job::LinuxSandboxBackend,
        router: Arc<WorkspaceRouter>,
        cgroups: Arc<AttemptCgroups>,
    ) -> Result<Self, harw_job::RuntimeError> {
        let host = harw_job::LinuxExecutor::new(harw_job::LinuxExecutorOptions {
            sandbox: offline.clone(),
            ..template.clone()
        })?;
        Ok(Self {
            host,
            template,
            offline,
            online,
            router,
            workspace: Mutex::new(Vec::new()),
            running: Mutex::new(HashMap::new()),
            cgroups,
        })
    }

    /// The executor of host commands and profile requests.
    #[must_use]
    pub fn host_executor(&self) -> &harw_job::LinuxExecutor {
        &self.host
    }

    /// The executor for `launch`: its network class's backend, with the
    /// modules on the Bubblewrap ones. Created on first use, then reused.
    fn executor_for(
        &self,
        launch: &WorkspaceLaunch,
    ) -> Result<Arc<harw_job::LinuxExecutor>, harw_job::RuntimeError> {
        use harw_job::LinuxSandboxBackend as Backend;
        let base = if launch.networked {
            &self.online
        } else {
            &self.offline
        };
        let with_modules = |bwrap: &harw_job_executor_bwrap::BwrapExecutor| {
            let mut bwrap = bwrap.clone().with_profile_modules(&launch.modules);
            if let Some(binding) = &launch.host_path {
                bwrap = bwrap.with_host_path(binding.clone());
            }
            if let Some(size) = launch.tmpfs_size {
                bwrap = bwrap.with_tmpfs_size(size);
            }
            bwrap
        };
        // Only the Bubblewrap backends mount modules; the others share one
        // executor per network class.
        let (backend, key) = match base {
            Backend::Bwrap(bwrap) => (Backend::Bwrap(with_modules(bwrap)), launch.clone()),
            Backend::BwrapTrampoline {
                bwrap,
                trampoline,
                plan_dir,
            } => (
                Backend::BwrapTrampoline {
                    bwrap: with_modules(bwrap),
                    trampoline: trampoline.clone(),
                    plan_dir: plan_dir.clone(),
                },
                launch.clone(),
            ),
            other => (
                other.clone(),
                WorkspaceLaunch {
                    root: PathBuf::new(),
                    networked: launch.networked,
                    modules: SandboxProfile::Strict,
                    host_path: None,
                    tmpfs_size: None,
                },
            ),
        };
        let key = WorkspaceLaunch {
            root: PathBuf::new(),
            ..key
        };
        let mut executors = self
            .workspace
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some((_, executor)) = executors.iter().find(|(shape, _)| *shape == key) {
            return Ok(Arc::clone(executor));
        }
        if executors.len() >= MAX_WORKSPACE_EXECUTORS {
            return Err(harw_job::RuntimeError::Sandbox {
                detail: format!(
                    "more than {MAX_WORKSPACE_EXECUTORS} distinct sandbox module sets; the \
                     workspace request is refused"
                ),
            });
        }
        let executor = Arc::new(harw_job::LinuxExecutor::new(
            harw_job::LinuxExecutorOptions {
                sandbox: backend,
                ..self.template.clone()
            },
        )?);
        executors.push((key, Arc::clone(&executor)));
        Ok(executor)
    }

    /// Attests the dedicated cgroup of an attempt `executor` just started
    /// (PL-97 E4): only when the executor has a delegated root (it then
    /// created `harw-job-<attempt>` and spawned the primary into it, or the
    /// start failed) and the identity captured after the attachment shows
    /// the primary in exactly that cgroup.
    fn attest_cgroup(
        &self,
        executor: &harw_job::LinuxExecutor,
        ctx: &harw_job_runtime::coordinator::AttemptContext,
        started: &harw_job_runtime::coordinator::StartedAttempt<
            <harw_job::LinuxExecutor as Executor>::Identity,
        >,
    ) {
        let observed = started
            .identity
            .as_ref()
            .and_then(|identity| identity.cgroup_path.as_deref());
        if let Some(root) = attested_delegated_root(
            executor.options().cgroup_root.as_deref(),
            ctx.attempt_id.as_str(),
            observed,
        ) {
            self.cgroups.record(ctx.attempt_id.as_str(), root);
        }
    }

    /// The executor that started the attempt of `ctx`.
    fn started_by(
        &self,
        ctx: &harw_job_runtime::coordinator::AttemptContext,
    ) -> Option<Arc<harw_job::LinuxExecutor>> {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(ctx.attempt_id.as_str())
            .cloned()
    }
}

#[cfg(target_os = "linux")]
impl Executor for HostCommandExecutor {
    type Identity = <harw_job::LinuxExecutor as Executor>::Identity;

    fn start(
        &self,
        spec: &JobSpec,
        ctx: &harw_job_runtime::coordinator::AttemptContext,
    ) -> Result<harw_job_runtime::coordinator::StartedAttempt<Self::Identity>, harw_job::RuntimeError>
    {
        let Some(key) = spec
            .idempotency_key
            .as_ref()
            .map(IdempotencyKey::as_str)
            .filter(|key| key.starts_with(WORKSPACE_KEY_PREFIX))
        else {
            let started = self.host.start(spec, ctx)?;
            self.attest_cgroup(&self.host, ctx, &started);
            return Ok(started);
        };
        // Never with the runtime's own root: without its registration the
        // attempt does not run.
        let Some(launch) = self.router.take(key) else {
            return Err(harw_job::RuntimeError::Sandbox {
                detail: "the workspace of this sandboxed command is no longer registered; it \
                         does not run without it"
                    .to_owned(),
            });
        };
        let executor = self.executor_for(&launch)?;
        let attempt = harw_job_runtime::coordinator::AttemptContext {
            workspace_root: launch.root,
            ..ctx.clone()
        };
        let started = executor.start(spec, &attempt)?;
        self.attest_cgroup(&executor, ctx, &started);
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(ctx.attempt_id.as_str().to_owned(), executor);
        Ok(started)
    }

    fn probe(
        &self,
        identity: &Self::Identity,
    ) -> Result<harw_job_runtime::coordinator::Probe, harw_job::RuntimeError> {
        self.host.probe(identity)
    }

    fn reattach(
        &self,
        identity: &Self::Identity,
        ctx: &harw_job_runtime::coordinator::AttemptContext,
    ) -> Result<harw_job_runtime::coordinator::AttemptRun, harw_job::RuntimeError> {
        self.host.reattach(identity, ctx)
    }

    fn recorded_exit(
        &self,
        ctx: &harw_job_runtime::coordinator::AttemptContext,
    ) -> Option<ExitOutcome> {
        match self.started_by(ctx) {
            Some(executor) => executor.recorded_exit(ctx),
            None => self.host.recorded_exit(ctx),
        }
    }

    fn finished(&self, ctx: &harw_job_runtime::coordinator::AttemptContext) {
        let started = self
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(ctx.attempt_id.as_str());
        // The attestation stays until the port collects it: a short attempt
        // can end before its start frame was read (bounded ledger).
        match started {
            Some(executor) => executor.finished(ctx),
            None => self.host.finished(ctx),
        }
    }
}

#[cfg(target_os = "macos")]
impl JobCommandPort<harw_job::FsJobRecordStore, harw_job::DarwinExecutor> {
    /// The host runtime on macOS: file-backed job records below `state_dir`
    /// and the Darwin executor (own process group, output through the runtime;
    /// no resource limits, see `DarwinExecutor`).
    ///
    /// # Errors
    /// The store or the runtime cannot be created.
    pub fn host(state_dir: &std::path::Path) -> Result<Self, String> {
        let store = harw_job::FsJobRecordStore::create_ambient(state_dir)
            .map_err(|error| format!("job store {}: {error}", state_dir.display()))?;
        let runtime = JobRuntime::builder()
            .store(store)
            .executor(harw_job::DarwinExecutor::default())
            .workspace_root("/")
            .build()
            .map_err(|error| format!("job runtime: {error}"))?;
        Ok(Self::new(runtime))
    }
}

impl<S: CoordinatorStore, E: Executor> JobCommandPort<S, E> {
    /// Wraps a runtime whose workspace root is `/` (commands name absolute
    /// working directories). Such a port runs no
    /// [`CommandSandbox::Workspace`] requests: their workspace root is not
    /// its runtime's.
    #[must_use]
    pub fn new(runtime: JobRuntime<S, E>) -> Self {
        Self {
            runtime,
            router: None,
            cgroups: None,
        }
    }

    /// Like [`JobCommandPort::new`], but collecting the attestations an
    /// executor records into `cgroups` (tests of the attestation chain).
    #[cfg(test)]
    fn with_attestations(runtime: JobRuntime<S, E>, cgroups: Arc<AttemptCgroups>) -> Self {
        Self {
            runtime,
            router: None,
            cgroups: Some(cgroups),
        }
    }

    /// The underlying runtime (job panel, recovery, shutdown).
    #[must_use]
    pub fn runtime(&self) -> &JobRuntime<S, E> {
        &self.runtime
    }

    /// The spec of `request`, and for a workspace request the registration
    /// that marks it ([`CommandRequest::job_spec`], module docs).
    fn spec(&self, request: &CommandRequest) -> Result<(JobSpec, Option<LaunchGuard>), String> {
        let mut spec = request.job_spec()?;
        let CommandSandbox::Workspace(workspace) = &request.sandbox else {
            return Ok((spec, None));
        };
        let Some(router) = &self.router else {
            return Err(
                "this command port cannot run a command in its caller's workspace sandbox"
                    .to_owned(),
            );
        };
        let guard = router.register(WorkspaceLaunch {
            root: workspace.root.clone(),
            networked: needs_network(workspace.profile),
            modules: workspace.modules.clone(),
            host_path: workspace.host_path.clone(),
            tmpfs_size: workspace.tmpfs_size,
        });
        spec.idempotency_key =
            Some(IdempotencyKey::new(guard.key.clone()).map_err(|error| error.to_string())?);
        Ok((spec, Some(guard)))
    }
}

/// Accumulates frames into the three views under one byte budget.
struct Collector {
    budget: usize,
    used: usize,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    combined: Vec<u8>,
    truncated: bool,
    /// The stream the budget cut first, recorded at the first cut only. The
    /// budget is spent across both streams in frame-arrival order, so the
    /// stream lengths alone do not say which stream was cut.
    cut: Option<OutputStream>,
    lagged: u64,
}

impl Collector {
    /// Returns whether the budget is now exceeded.
    fn push(&mut self, stdout: bool, chunk: &[u8]) -> bool {
        let room = self.budget.saturating_sub(self.used);
        let take = chunk.len().min(room);
        let kept = &chunk[..take];
        self.used += take;
        if take < chunk.len() {
            self.truncated = true;
            if self.cut.is_none() {
                self.cut = Some(if stdout {
                    OutputStream::Stdout
                } else {
                    OutputStream::Stderr
                });
            }
        }
        if stdout {
            self.stdout.extend_from_slice(kept);
        } else {
            self.stderr.extend_from_slice(kept);
        }
        self.combined.extend_from_slice(kept);
        take < chunk.len()
    }

    fn event(&mut self, event: FrameEvent) -> bool {
        match event {
            FrameEvent::Frame(JobFrame::Stdout(chunk)) => self.push(true, &chunk),
            FrameEvent::Frame(JobFrame::Stderr(chunk)) => self.push(false, &chunk),
            FrameEvent::Lagged(missed) => {
                self.lagged += missed;
                false
            }
            _ => false,
        }
    }
}

/// How a finished job maps onto the command vocabulary.
fn end_of(result: &JobResult, cancelled: bool, over_budget: bool) -> (CommandEnd, i32) {
    if over_budget {
        (CommandEnd::OutputLimit, -1)
    } else if result.cancellation == Some(CancellationCause::Deadline)
        || result.state == LifecycleState::TimedOut
    {
        (CommandEnd::TimedOut, -1)
    } else if cancelled || result.state == LifecycleState::Cancelled {
        (CommandEnd::Cancelled, -1)
    } else {
        match result.exit {
            Some(ExitOutcome::Exited(code)) => (CommandEnd::Exited, code),
            Some(ExitOutcome::Signaled { signal, .. }) => {
                (CommandEnd::Signaled(signal), 128 + signal)
            }
            _ => (
                CommandEnd::Failed(
                    result
                        .reason
                        .clone()
                        .unwrap_or_else(|| format!("job ended {:?}", result.state)),
                ),
                -1,
            ),
        }
    }
}

fn output_files(request: &CommandRequest) -> Option<OutputFiles> {
    match &request.output {
        CommandOutput::Capture => None,
        CommandOutput::Files { stdout, stderr } => Some(OutputFiles {
            stdout: stdout.clone(),
            stderr: stderr.clone(),
        }),
        // Standard output is piped by the handoff; the file is never written.
        CommandOutput::StdoutPipe { stderr } => Some(OutputFiles {
            stdout: PathBuf::from("/dev/null"),
            stderr: stderr.clone(),
        }),
    }
}

impl<S: CoordinatorStore, E: Executor> CommandPort for JobCommandPort<S, E> {
    fn workspace_backend(&self, profile: SandboxProfileName) -> Option<BackendKind> {
        self.router.as_ref().map(|router| router.backend(profile))
    }

    fn start(&self, request: CommandRequest) -> StartFuture<'_> {
        Box::pin(async move {
            let started = Instant::now();
            // Held until the attempt started (or never will).
            let (spec, _registration) = self.spec(&request)?;
            let want_stdin = request.stdin == CommandStdin::Pipe;
            let want_stdout = matches!(request.output, CommandOutput::StdoutPipe { .. });
            let (handoff, handed) = if want_stdin || want_stdout {
                let (handoff, receiver) = StdioHandoff::new(want_stdin, want_stdout);
                (Some(handoff), Some(receiver))
            } else {
                (None, None)
            };
            let options = SubmitOptions {
                persistence: request.persistence,
                stream: true,
                output_files: output_files(&request),
                stdio_handoff: handoff,
            };
            let (handle, frames) = self
                .runtime
                .submit_with(spec, options)
                .await
                .map_err(|error| error.to_string())?;
            let job_id = handle.id().clone();
            let runtime = self.runtime.clone();
            let Some(mut frames) = frames else {
                return Err("the runtime returned no frame stream".to_owned());
            };
            // The first frame of the attempt names the PID; a job that never
            // starts ends the stream instead.
            let mut began = false;
            let mut pid = None;
            let mut attempt = None;
            let wait_for_start = async {
                while let Some(event) = frames.next().await {
                    if let FrameEvent::Frame(JobFrame::AttemptStarted {
                        pid: started,
                        attempt_id,
                        ..
                    }) = event
                    {
                        pid = started;
                        attempt = Some(attempt_id);
                        began = true;
                        break;
                    }
                }
            };
            let _ = tokio::time::timeout(START_TIMEOUT, wait_for_start).await;
            let frames = request.stream.then_some(frames);
            // Only the attestation of this very attempt (PL-97 E4).
            let cgroup = match (&self.cgroups, &attempt) {
                (Some(cgroups), Some(attempt)) => cgroups.take(attempt.as_str()),
                _ => None,
            };
            if !began {
                // The attempt never started (spawn, sandbox or admission
                // failed): that is a failed start, not a running command.
                return match handle.wait().await {
                    Ok(result) => match end_of(&result, false, false).0 {
                        CommandEnd::Failed(message) => Err(message),
                        _ => Err("the job ended before it started".to_owned()),
                    },
                    Err(error) => Err(error.to_string()),
                };
            }
            let handed = match handed {
                Some(receiver) => match tokio::time::timeout(START_TIMEOUT, receiver).await {
                    Ok(Ok(handed)) => handed,
                    _ => {
                        return Err("the runtime did not hand over the standard streams".to_owned());
                    }
                },
                None => harw_job::HandedStdio::default(),
            };
            let stdin = handed
                .stdin
                .map(tokio::process::ChildStdin::from_std)
                .transpose()
                .map_err(|error| format!("stdin pipe: {error}"))?;
            let stdout = handed
                .stdout
                .map(tokio::process::ChildStdout::from_std)
                .transpose()
                .map_err(|error| format!("stdout pipe: {error}"))?;
            let done: RunFuture<'static> = Box::pin(async move {
                match handle.wait().await {
                    Ok(result) => {
                        let (end, exit_code) = end_of(&result, false, false);
                        CommandOutcome {
                            end,
                            exit_code,
                            stdout: result.stdout,
                            stderr: result.stderr,
                            combined: Vec::new(),
                            truncated: result.stdout_omitted > 0 || result.stderr_omitted > 0,
                            // The job caps each stream on its own here; there is
                            // no shared budget whose first cut could be named.
                            truncated_stream: None,
                            lagged: 0,
                            elapsed: started.elapsed(),
                        }
                    }
                    Err(error) => CommandOutcome::failed(error.to_string(), started),
                }
            });
            Ok(StartedCommand {
                stdin,
                stdout,
                frames: frames.map(|inner| CommandFrames { inner }),
                cancel: Box::new(move || {
                    runtime.cancel(&job_id);
                }),
                pid,
                done,
                cgroup,
            })
        })
    }

    fn run(&self, request: CommandRequest, cancel: CancelToken) -> RunFuture<'_> {
        Box::pin(async move {
            let started = Instant::now();
            // Held for the whole run; the attempt takes it when it starts.
            let (spec, _registration) = match self.spec(&request) {
                Ok(spec) => spec,
                Err(reason) => return CommandOutcome::failed(reason, started),
            };
            let options = SubmitOptions {
                persistence: request.persistence,
                stream: true,
                output_files: output_files(&request),
                stdio_handoff: None,
            };
            let (handle, frames) = match self.runtime.submit_with(spec, options).await {
                Ok(submitted) => submitted,
                Err(error) => return CommandOutcome::failed(error.to_string(), started),
            };
            let Some(mut frames) = frames else {
                return CommandOutcome::failed("the runtime returned no frame stream", started);
            };
            let id = handle.id().clone();
            let mut collector = Collector {
                budget: request.max_output_bytes,
                used: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
                combined: Vec::new(),
                truncated: false,
                cut: None,
                lagged: 0,
            };
            let mut wait = Box::pin(handle.wait());
            let (mut frames_open, mut stop_sent) = (true, false);
            let (mut cancelled, mut over_budget) = (false, false);
            let result = loop {
                tokio::select! {
                    result = &mut wait => break result,
                    event = frames.next(), if frames_open => match event {
                        None => frames_open = false,
                        Some(event) => {
                            if collector.event(event) {
                                over_budget = true;
                                if !stop_sent {
                                    stop_sent = true;
                                    self.runtime.cancel(&id);
                                }
                            }
                        }
                    },
                    () = cancel.cancelled(), if !stop_sent => {
                        stop_sent = true;
                        cancelled = true;
                        self.runtime.cancel(&id);
                    }
                }
            };
            while let Ok(Some(event)) = tokio::time::timeout(DRAIN, frames.next()).await {
                if collector.event(event) {
                    over_budget = true;
                }
            }
            let result = match result {
                Ok(result) => result,
                Err(error) => return CommandOutcome::failed(error.to_string(), started),
            };
            let (mut stdout, mut stderr, mut combined) =
                (collector.stdout, collector.stderr, collector.combined);
            let mut truncated = collector.truncated;
            let mut truncated_stream = collector.cut;
            if collector.lagged > 0 {
                // The live stream missed frames: the job result's head/tail is
                // the better record.
                stdout = result.stdout.clone();
                stderr = result.stderr.clone();
                combined = [stdout.clone(), stderr.clone()].concat();
                truncated = truncated || result.stdout_omitted > 0 || result.stderr_omitted > 0;
                // The job result names the omitted stream; it replaces the
                // collector's view together with the output itself.
                truncated_stream = if result.stdout_omitted > 0 {
                    Some(OutputStream::Stdout)
                } else if result.stderr_omitted > 0 {
                    Some(OutputStream::Stderr)
                } else {
                    truncated_stream
                };
            }
            let (end, exit_code) = end_of(&result, cancelled, over_budget);
            CommandOutcome {
                end,
                exit_code,
                stdout,
                stderr,
                combined,
                truncated,
                truncated_stream,
                lagged: collector.lagged,
                elapsed: started.elapsed(),
            }
        })
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

/// The host port's backend choice (PL-97 E1).
#[cfg(all(test, target_os = "linux"))]
mod backend_tests {
    use std::os::unix::fs::PermissionsExt as _;

    use harw_job::BackendKind;

    use super::{HostPortOptions, JobCommandPort, SANDBOX_PLAN_DIR, host_backend};

    harw_test_support::define_test_error!(pub(crate));

    #[test]
    fn host_port_runs_the_selected_backend() -> TestResult {
        let dir =
            harw_test_support::unique_tmp_created("harw-command", "backend").map_err(ctx("tmp"))?;
        let port = JobCommandPort::host(&dir).map_err(TestError::Unexpected)?;
        // The same host gives the same decision.
        let selection = host_backend(&dir);
        let backend = &port
            .runtime()
            .coordinator()
            .executor()
            .host_executor()
            .options()
            .sandbox;
        assert_eq!(backend.kind(), selection.chosen, "{:?}", selection.reasons);
        // Never silent: one line per candidate tried, up to the chosen one.
        let tried = BackendKind::ALL
            .iter()
            .position(|kind| *kind == selection.chosen)
            .ok_or(TestError::Missing("chosen kind in BackendKind::ALL"))?
            + 1;
        assert_eq!(selection.reasons.len(), tried, "{:?}", selection.reasons);
        let last = selection
            .reasons
            .last()
            .ok_or(TestError::Missing("reason"))?;
        assert!(
            last.starts_with(&format!("{}: selected", selection.chosen)),
            "{last}"
        );
        // The trampoline backends get a private plan directory.
        let plan_dir = dir.join(SANDBOX_PLAN_DIR);
        let mode = std::fs::metadata(&plan_dir)
            .map_err(ctx("plan dir"))?
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        std::fs::remove_dir_all(&dir).map_err(ctx("cleanup"))?;
        Ok(())
    }

    /// PL-97 E3: a configured root that is refused (relative), missing or
    /// not delegated leaves the port working without a cgroup root.
    #[test]
    fn host_port_drops_an_unusable_cgroup_root_and_keeps_running() -> TestResult {
        for root in [
            "relative/harw.slice",
            "/tmp/harw-not-a-cgroup",
            "/sys/fs/cgroup/harw-e3-does-not-exist.slice",
        ] {
            let dir = harw_test_support::unique_tmp_created("harw-command", "cgroup-root")
                .map_err(ctx("tmp"))?;
            let options = HostPortOptions {
                cgroup_root: Some(std::path::PathBuf::from(root)),
                trampoline: None,
            };
            let port = JobCommandPort::host_with(&dir, &options).map_err(TestError::Unexpected)?;
            let executor = port.runtime().coordinator().executor().host_executor();
            assert_eq!(executor.options().cgroup_root, None, "{root}");
            std::fs::remove_dir_all(&dir).map_err(ctx("cleanup"))?;
        }
        Ok(())
    }

    /// PL-97 E3: a delegated root from `[jobs] cgroup_root` reaches
    /// `LinuxExecutorOptions::cgroup_root` (host-gated by
    /// `HARW_TEST_CGROUP_ROOT`, a delegated cgroup v2 directory).
    #[test]
    fn host_port_passes_a_delegated_config_root_to_the_executor() -> TestResult {
        let Some(root) = std::env::var_os("HARW_TEST_CGROUP_ROOT").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: HARW_TEST_CGROUP_ROOT is not set");
            return Ok(());
        };
        if !harw_job::HostReport::probe_with_cgroup_root(Some(&root)).cgroup_v2_delegated {
            eprintln!("skipped: HARW_TEST_CGROUP_ROOT is not delegated");
            return Ok(());
        }
        let dir = harw_test_support::unique_tmp_created("harw-command", "cgroup-config")
            .map_err(ctx("tmp"))?;
        let options = HostPortOptions {
            cgroup_root: Some(root.clone()),
            trampoline: None,
        };
        let port = JobCommandPort::host_with(&dir, &options).map_err(TestError::Unexpected)?;
        let executor = port.runtime().coordinator().executor().host_executor();
        assert_eq!(
            executor.options().cgroup_root.as_deref(),
            Some(root.as_path())
        );
        std::fs::remove_dir_all(&dir).map_err(ctx("cleanup"))?;
        Ok(())
    }

    /// PL-97 E3: `[jobs] trampoline` replaces the lookup next to the
    /// running executable; a path that is no executable rules out both
    /// trampoline backends (with the reason), never silently.
    #[test]
    fn host_port_uses_the_configured_trampoline_path() -> TestResult {
        let dir = harw_test_support::unique_tmp_created("harw-command", "trampoline")
            .map_err(ctx("tmp"))?;
        let missing = dir.join("no-such-harw-job-exec");
        let options = HostPortOptions {
            cgroup_root: None,
            trampoline: Some(missing),
        };
        let port = JobCommandPort::host_with(&dir, &options).map_err(TestError::Unexpected)?;
        let kind = port
            .runtime()
            .coordinator()
            .executor()
            .host_executor()
            .options()
            .sandbox
            .kind();
        assert!(
            !matches!(
                kind,
                BackendKind::BwrapTrampoline | BackendKind::LandlockTrampoline
            ),
            "{kind}"
        );
        std::fs::remove_dir_all(&dir).map_err(ctx("cleanup"))?;
        Ok(())
    }
}

/// Workspace sandbox requests (PL-97 E2b).
#[cfg(all(test, target_os = "linux"))]
mod workspace_tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use harw_job::{
        AttemptId, BackendKind, Executor as _, FsJobRecordStore, JobRuntime, RunnerId,
        SandboxProfileName, SandboxRequirement, WorkspacePath,
    };
    use harw_job_core::spec::JobWriteScope;
    use harw_types::WorkId;
    use harw_types::cancel::CancelToken;

    use super::{
        CommandEnd, CommandPort, CommandRequest, CommandSandbox, JobCommandPort,
        WORKSPACE_KEY_PREFIX, WorkspaceSandbox, host_backends,
    };

    harw_test_support::define_test_error!(pub(crate));

    fn dir(tag: &str) -> TestResult<PathBuf> {
        harw_test_support::unique_tmp_created("harw-command", tag).map_err(ctx("tmp"))
    }

    fn workspace_request(root: &Path, script: &str) -> CommandRequest {
        let mut request = CommandRequest::new("/bin/sh", root, Duration::from_secs(20));
        request.args = vec!["-c".into(), script.into()];
        request.sandbox = CommandSandbox::Workspace(Box::new(WorkspaceSandbox::new(
            root,
            SandboxProfileName::NoNetwork,
            SandboxRequirement::BestEffort,
        )));
        request
    }

    #[test]
    fn job_spec_maps_a_workspace_request() -> TestResult {
        let mut request = workspace_request(Path::new("/work/repo"), "true");
        request.cwd = PathBuf::from("/work/repo/sub");
        let spec = request.job_spec().map_err(TestError::Unexpected)?;
        assert_eq!(spec.program, "/bin/sh");
        assert_eq!(spec.args, ["-c", "true"]);
        assert_eq!(
            spec.working_dir,
            WorkspacePath::new("sub").map_err(ctx("path"))?
        );
        assert_eq!(spec.sandbox_profile, SandboxProfileName::NoNetwork);
        assert_eq!(spec.sandbox, SandboxRequirement::BestEffort);
        assert!(spec.resources.wall_timeout.is_some());
        assert_eq!(spec.idempotency_key, None, "the port adds the mark");
        assert_eq!(spec.write_scope, None);

        // A write scope makes it `required`.
        let scope = JobWriteScope::new(["/work/repo/src"]).map_err(ctx("scope"))?;
        if let CommandSandbox::Workspace(workspace) = &mut request.sandbox {
            workspace.write_scope = Some(scope.clone());
        }
        let spec = request.job_spec().map_err(TestError::Unexpected)?;
        assert_eq!(spec.sandbox, SandboxRequirement::Required);
        assert_eq!(spec.write_scope, Some(scope));

        // Outside the root, or a relative root: refused.
        request.cwd = PathBuf::from("/work/other");
        assert!(
            request
                .job_spec()
                .is_err_and(|error| error.contains("outside"))
        );
        let mut relative = workspace_request(Path::new("repo"), "true");
        relative.cwd = PathBuf::from("/repo");
        assert!(
            relative
                .job_spec()
                .is_err_and(|error| error.contains("not absolute"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_port_without_router_refuses_workspace_requests() -> TestResult {
        let root = dir("no-router")?;
        let runtime = JobRuntime::builder()
            .store(FsJobRecordStore::create_ambient(&root).map_err(ctx("store"))?)
            .executor(
                harw_job::LinuxExecutor::new(harw_job::LinuxExecutorOptions::default())
                    .map_err(ctx("executor"))?,
            )
            .workspace_root("/")
            .build()
            .map_err(ctx("runtime"))?;
        let port = JobCommandPort::new(runtime);
        assert_eq!(port.workspace_backend(SandboxProfileName::NoNetwork), None);
        let marker = root.join("marker");
        let script = format!("touch {}", marker.display());
        let outcome = port
            .run(workspace_request(&root, &script), CancelToken::new())
            .await;
        assert!(
            matches!(&outcome.end, CommandEnd::Failed(message) if message.contains("workspace")),
            "{outcome:?}"
        );
        assert!(!marker.exists(), "nothing ran");
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn the_host_port_reports_a_backend_per_network_class() -> TestResult {
        let root = dir("classes")?;
        let port = JobCommandPort::host(&root).map_err(TestError::Unexpected)?;
        let (offline, online) = host_backends(&root);
        for profile in [
            SandboxProfileName::NoNetwork,
            SandboxProfileName::ReadOnlyAnalysis,
        ] {
            assert_eq!(port.workspace_backend(profile), Some(offline.chosen));
        }
        for profile in [
            SandboxProfileName::WorkspaceBuild,
            SandboxProfileName::NetworkRestricted,
        ] {
            let kind = port
                .workspace_backend(profile)
                .ok_or(TestError::Missing("online backend"))?;
            assert_eq!(kind, online.chosen);
            // No egress relay is configured: a networked job never gets a
            // Bubblewrap backend, which would refuse it.
            assert!(!kind.uses_bwrap(), "{kind}");
        }
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[tokio::test]
    async fn a_workspace_command_runs_on_its_own_root() -> TestResult {
        let state = dir("ws-state")?;
        let root = dir("ws-root")?
            .canonicalize()
            .map_err(ctx("canonicalize"))?;
        let port = JobCommandPort::host(&state).map_err(TestError::Unexpected)?;
        let kind = port.workspace_backend(SandboxProfileName::NoNetwork);
        if !kind.is_some_and(BackendKind::uses_bwrap) {
            eprintln!("skipped: no Bubblewrap backend on this host ({kind:?})");
            return Ok(());
        }
        let mut request = workspace_request(&root, "pwd; echo ok > marker; cat marker");
        request.env = vec![("PATH".into(), "/usr/bin:/bin".into())];
        let outcome = port.run(request, CancelToken::new()).await;
        if outcome.end != CommandEnd::Exited {
            // A host whose bwrap cannot start here (user namespaces) is not
            // a failure of the routing.
            eprintln!("skipped: the sandbox did not start: {outcome:?}");
            return Ok(());
        }
        assert_eq!(outcome.exit_code, 0, "{outcome:?}");
        assert_eq!(
            String::from_utf8_lossy(&outcome.stdout),
            format!("{}\nok\n", root.display())
        );
        assert!(root.join("marker").exists(), "the workspace is writable");
        let router = port.router.as_ref().ok_or(TestError::Missing("router"))?;
        assert!(
            router
                .launches
                .lock()
                .map_err(|_| TestError::Missing("lock"))?
                .is_empty(),
            "the registration is taken"
        );
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[tokio::test]
    async fn a_scoped_command_without_an_enforcing_backend_never_runs() -> TestResult {
        let state = dir("scope-state")?;
        let root = dir("scope-root")?
            .canonicalize()
            .map_err(ctx("canonicalize"))?;
        let port = JobCommandPort::host(&state).map_err(TestError::Unexpected)?;
        if port.workspace_backend(SandboxProfileName::NoNetwork)
            == Some(BackendKind::BwrapTrampoline)
        {
            eprintln!("skipped: the trampoline chain can enforce a scope here");
            return Ok(());
        }
        std::fs::create_dir_all(root.join("src")).map_err(ctx("src"))?;
        let marker = root.join("marker");
        let mut request = workspace_request(&root, &format!("touch {}", marker.display()));
        if let CommandSandbox::Workspace(workspace) = &mut request.sandbox {
            workspace.write_scope =
                Some(JobWriteScope::new([root.join("src")]).map_err(ctx("scope"))?);
        }
        let outcome = port.run(request, CancelToken::new()).await;
        assert!(matches!(outcome.end, CommandEnd::Failed(_)), "{outcome:?}");
        assert!(!marker.exists(), "no process ran");
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn an_unregistered_workspace_mark_is_refused() -> TestResult {
        let state = dir("unregistered")?;
        let port = JobCommandPort::host(&state).map_err(TestError::Unexpected)?;
        let mut spec = workspace_request(Path::new("/"), "true")
            .job_spec()
            .map_err(TestError::Unexpected)?;
        spec.idempotency_key = Some(
            harw_job::IdempotencyKey::new(format!("{WORKSPACE_KEY_PREFIX}stale"))
                .map_err(ctx("key"))?,
        );
        let context = harw_job_runtime::coordinator::AttemptContext {
            job_id: WorkId::from_str("j"),
            attempt_id: AttemptId::new("j-e1").map_err(ctx("attempt"))?,
            runner_id: RunnerId::new("r").map_err(ctx("runner"))?,
            lease_epoch: 1,
            workspace_root: PathBuf::from("/"),
            output_files: None,
            stdio_handoff: None,
        };
        let executor = port.runtime().coordinator().executor();
        assert!(matches!(
            executor.start(&spec, &context),
            Err(harw_job::RuntimeError::Sandbox { .. })
        ));
        let _ = std::fs::remove_dir_all(&state);
        Ok(())
    }
}

/// The dedicated-cgroup attestation of started attempts (PL-97 E4).
#[cfg(test)]
mod cgroup_attestation_tests {
    use std::path::Path;

    use super::{
        AttemptCgroup, AttemptCgroups, MAX_ATTESTED_CGROUPS, attempt_cgroup_leaf,
        attested_delegated_root,
    };

    #[test]
    fn only_the_attempt_cgroup_directly_beneath_the_configured_root_is_attested() {
        let root = Some(Path::new("/sys/fs/cgroup/user.slice/harw.slice"));
        // The genuine case: `harw-job-<attempt>` directly beneath the root.
        assert_eq!(
            attested_delegated_root(
                root,
                "job-1-e1",
                Some("/user.slice/harw.slice/harw-job-job-1-e1")
            )
            .as_deref(),
            Some("/user.slice/harw.slice")
        );
        // The executor's leaf naming replaces unsafe characters.
        assert_eq!(attempt_cgroup_leaf("job:1.x-e2"), "harw-job-job_1.x-e2");
        assert_eq!(
            attested_delegated_root(
                root,
                "job:1.x-e2",
                Some("/user.slice/harw.slice/harw-job-job_1.x-e2")
            )
            .as_deref(),
            Some("/user.slice/harw.slice")
        );
        let refused = [
            // No delegated root: the executor created no job cgroup.
            (None, "a", Some("/user.slice/harw.slice/harw-job-a")),
            // A relative configured root is never trusted.
            (
                Some(Path::new("user.slice/harw.slice")),
                "a",
                Some("/user.slice/harw.slice/harw-job-a"),
            ),
            // No observed cgroup.
            (root, "a", None),
            // The primary is still in the shared cgroup.
            (root, "a", Some("/user.slice/harw.slice")),
            (root, "a", Some("/user.slice/session-2.scope")),
            // Leaf of another attempt, or a prefix of this one.
            (root, "a", Some("/user.slice/harw.slice/harw-job-b")),
            (root, "a-e1", Some("/user.slice/harw.slice/harw-job-a-e10")),
            // Not directly beneath the configured root.
            (root, "a", Some("/user.slice/harw.slice/x/harw-job-a")),
            (root, "a", Some("/other.slice/harw-job-a")),
            (root, "a", Some("/harw.slice/harw-job-a/leaf")),
            // The hierarchy root is never a delegated root.
            (Some(Path::new("/sys/fs/cgroup")), "a", Some("/harw-job-a")),
            // Malformed observations and attempt ids.
            (root, "a", Some("user.slice/harw.slice/harw-job-a")),
            (root, "a", Some("/user.slice//harw.slice/harw-job-a")),
            (root, "a", Some("/user.slice/../harw.slice/harw-job-a")),
            (root, "", Some("/user.slice/harw.slice/harw-job-")),
        ];
        for (configured, attempt, observed) in refused {
            assert_eq!(
                attested_delegated_root(configured, attempt, observed),
                None,
                "{configured:?} {attempt:?} {observed:?}"
            );
        }
    }

    #[test]
    fn an_attestation_is_collected_once_and_only_for_its_attempt() {
        let ledger = AttemptCgroups::default();
        ledger.record("a-e1", "/harw.slice".to_owned());
        assert_eq!(ledger.take("b-e1"), None);
        assert_eq!(
            ledger.take("a-e1"),
            Some(AttemptCgroup {
                attempt_id: "a-e1".to_owned(),
                delegated_root: "/harw.slice".to_owned(),
            })
        );
        assert_eq!(ledger.take("a-e1"), None, "never handed out twice");
    }

    #[test]
    fn a_full_ledger_evicts_the_oldest_attestation() {
        let ledger = AttemptCgroups::default();
        for index in 0..=MAX_ATTESTED_CGROUPS {
            ledger.record(&format!("a{index}-e1"), "/harw.slice".to_owned());
        }
        assert_eq!(ledger.len(), MAX_ATTESTED_CGROUPS);
        assert_eq!(ledger.take("a0-e1"), None, "the oldest went first");
        assert!(ledger.take("a1-e1").is_some());
        let newest = format!("a{MAX_ATTESTED_CGROUPS}-e1");
        assert!(ledger.take(&newest).is_some());
    }
}
