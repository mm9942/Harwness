//! The port, the request/outcome vocabulary and the job-runtime adapter.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::{Duration, Instant};

use harw_job::{
    CancellationCause, CoordinatorStore, Executor, ExitOutcome, FrameEvent, JobFrame, JobResult,
    JobRuntime, JobSpec, LifecycleState, OutputFiles, Persistence, ResourceRequest,
    SandboxProfileName, SandboxRequirement, StdioHandoff, SubmitOptions, WorkspacePath,
};
use harw_types::cancel::CancelToken;
use jiff::SignedDuration;

/// How long buffered frames are drained after the job reported its result.
const DRAIN: Duration = Duration::from_millis(250);

/// How long [`CommandPort::start`] waits for the job to report its PID.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// How a command is confined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSandbox {
    /// Directly on the host, no sandbox (the user's own `!` commands).
    Host,
    /// A named profile at the given strictness.
    Profile(SandboxProfileName, SandboxRequirement),
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
}

/// [`CommandPort`] over a [`JobRuntime`].
pub struct JobCommandPort<S, E> {
    runtime: JobRuntime<S, E>,
}

impl<S, E> std::fmt::Debug for JobCommandPort<S, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JobCommandPort")
    }
}

#[cfg(target_os = "linux")]
impl JobCommandPort<harw_job::FsJobRecordStore, harw_job::LinuxExecutor> {
    /// The host runtime: file-backed job records below `state_dir` and the
    /// Linux executor (pidfd + process group, no sandbox backend; sandboxed
    /// commands need a configured executor).
    ///
    /// # Errors
    /// The store or the runtime cannot be created.
    pub fn host(state_dir: &std::path::Path) -> Result<Self, String> {
        let store = harw_job::FsJobRecordStore::create_ambient(state_dir)
            .map_err(|error| format!("job store {}: {error}", state_dir.display()))?;
        let runtime = JobRuntime::builder()
            .store(store)
            .executor(
                harw_job::LinuxExecutor::new(harw_job::LinuxExecutorOptions {
                    new_session: true,
                    ..harw_job::LinuxExecutorOptions::default()
                })
                .map_err(|error| format!("job executor: {error}"))?,
            )
            .workspace_root("/")
            .build()
            .map_err(|error| format!("job runtime: {error}"))?;
        Ok(Self::new(runtime))
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
    /// working directories).
    #[must_use]
    pub fn new(runtime: JobRuntime<S, E>) -> Self {
        Self { runtime }
    }

    /// The underlying runtime (job panel, recovery, shutdown).
    #[must_use]
    pub fn runtime(&self) -> &JobRuntime<S, E> {
        &self.runtime
    }

    fn spec(request: &CommandRequest) -> Result<JobSpec, String> {
        let relative = request
            .cwd
            .strip_prefix("/")
            .map_err(|_| format!("working directory {:?} is not absolute", request.cwd))?;
        let working_dir = if relative.as_os_str().is_empty() {
            WorkspacePath::root()
        } else {
            let text = relative
                .to_str()
                .ok_or_else(|| "working directory is not valid UTF-8".to_owned())?;
            WorkspacePath::new(text).map_err(|error| error.to_string())?
        };
        let timeout = SignedDuration::try_from(request.timeout)
            .map_err(|error| format!("timeout: {error}"))?;
        let (profile, requirement) = match request.sandbox {
            CommandSandbox::Host => (SandboxProfileName::WorkspaceBuild, SandboxRequirement::None),
            CommandSandbox::Profile(profile, requirement) => (profile, requirement),
        };
        let mut builder = JobSpec::command(request.program.clone())
            .args(request.args.iter().cloned())
            .workspace(working_dir)
            .resources(request.limits.clone())
            .timeout(timeout)
            .sandbox(profile)
            .sandbox_requirement(requirement);
        for (name, value) in &request.env {
            builder = builder.env(name.clone(), value.clone());
        }
        builder.build().map_err(|error| error.to_string())
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
    fn start(&self, request: CommandRequest) -> StartFuture<'_> {
        Box::pin(async move {
            let started = Instant::now();
            let spec = Self::spec(&request)?;
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
            let wait_for_start = async {
                while let Some(event) = frames.next().await {
                    if let FrameEvent::Frame(JobFrame::AttemptStarted { pid: started, .. }) = event
                    {
                        pid = started;
                        began = true;
                        break;
                    }
                }
            };
            let _ = tokio::time::timeout(START_TIMEOUT, wait_for_start).await;
            let frames = request.stream.then_some(frames);
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
            })
        })
    }

    fn run(&self, request: CommandRequest, cancel: CancelToken) -> RunFuture<'_> {
        Box::pin(async move {
            let started = Instant::now();
            let spec = match Self::spec(&request) {
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
            if collector.lagged > 0 {
                // The live stream missed frames: the job result's head/tail is
                // the better record.
                stdout = result.stdout.clone();
                stderr = result.stderr.clone();
                combined = [stdout.clone(), stderr.clone()].concat();
                truncated = truncated || result.stdout_omitted > 0 || result.stderr_omitted > 0;
            }
            let (end, exit_code) = end_of(&result, cancelled, over_budget);
            CommandOutcome {
                end,
                exit_code,
                stdout,
                stderr,
                combined,
                truncated,
                lagged: collector.lagged,
                elapsed: started.elapsed(),
            }
        })
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
