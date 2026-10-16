//! Running one [`ContainerPlan`]: `create`, read back, verify, only then
//! `start`; bounded output.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use harw_command::{CommandEnd, CommandOutput, CommandPort, CommandRequest, CommandSandbox};
use harw_job::Persistence;
use harw_tool_container::{ContainerId, ContainerPlan, Readback, Stage, StartRefusal};
use harw_types::cancel::CancelToken;

use crate::inspect::parse_inspect;

/// Maximum captured output budget for one engine command; excess output is
/// dropped and reported as truncated by the job runtime.
pub const MAX_STREAM_BYTES: usize = 64 * 1024;
/// Time one engine helper command (`create`, `inspect`, `rm`) may take.
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);
/// Extra wall time beyond the plan's timeout before the container is killed.
const GRACE: Duration = Duration::from_secs(5);

/// Captured process result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOutput {
    /// Exit code (`None` when killed by a signal).
    pub exit_code: Option<i32>,
    /// Captured stdout (lossy UTF-8, bounded).
    pub stdout: String,
    /// Captured stderr (lossy UTF-8, bounded).
    pub stderr: String,
    /// Output beyond the bound was dropped.
    pub truncated: bool,
    /// The wall-time limit hit and the container was killed.
    pub timed_out: bool,
    /// The run was cancelled and the container killed.
    pub cancelled: bool,
}

/// What a run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineRun {
    /// Process result.
    pub output: RunOutput,
    /// The read-back the container passed **before** it was started (always
    /// present: a container that could not be verified never runs).
    pub readback: Option<Readback>,
}

/// Why a run did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// The engine process could not be started.
    Spawn(String),
    /// The read-back did not prove every restriction; the container was
    /// removed without ever running. The list names the dimensions that were
    /// not enforced or could not be verified.
    NotEnforced(Vec<String>),
    /// The engine could not create or read back the container, or a bind
    /// source changed; nothing ran.
    Unverified(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(reason) => write!(f, "could not start the container engine: {reason}"),
            Self::NotEnforced(dimensions) => write!(
                f,
                "the engine did not prove the requested isolation ({}); the container was removed without running",
                dimensions.join(", ")
            ),
            Self::Unverified(reason) => {
                write!(
                    f,
                    "the container could not be verified, nothing ran: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for EngineError {}

/// Boxed future of an engine call.
pub type EngineFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Runs plans. A seam so tools are testable without an engine.
pub trait ContainerEngine: Send + Sync {
    /// Runs `plan` to completion, timeout or cancellation.
    fn run<'a>(
        &'a self,
        plan: &'a ContainerPlan,
        cancel: Option<&'a CancelToken>,
    ) -> EngineFuture<'a, Result<EngineRun, EngineError>>;
}

/// Podman via its CLI, with a cleared environment
/// ([`harw_tool_container::engine_environment`]).
#[derive(Debug, Clone)]
pub struct PodmanEngine {
    env: Vec<(String, String)>,
    grace: Duration,
    port: Option<std::sync::Arc<dyn CommandPort>>,
}

impl PodmanEngine {
    /// Engine whose process environment is exactly `env`.
    #[must_use]
    pub fn new(env: Vec<(String, String)>) -> Self {
        Self {
            env,
            grace: GRACE,
            port: None,
        }
    }

    /// Explicit command-runtime injection for embedders and tests.
    #[must_use]
    pub fn with_command_port(mut self, port: std::sync::Arc<dyn CommandPort>) -> Self {
        self.port = Some(port);
        self
    }

    fn port(&self) -> Result<std::sync::Arc<dyn CommandPort>, EngineError> {
        self.port
            .clone()
            .or_else(harw_command::installed)
            .ok_or_else(|| {
                EngineError::Spawn(
                    "no job-backed command runtime is installed for the container engine"
                        .to_owned(),
                )
            })
    }

    /// Extra wall time beyond the plan's timeout before the container is
    /// removed (tests shorten it).
    #[must_use]
    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    /// Convert one verified lifecycle stage into a job-backed command request.
    fn request(
        &self,
        plan: &ContainerPlan,
        stage: Stage<'_>,
        timeout: Duration,
    ) -> Result<CommandRequest, EngineError> {
        let lookup = |name: &str| {
            self.env
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        let command = plan
            .to_command(stage, &lookup)
            .map_err(|error| EngineError::Unverified(error.to_string()))?;

        let mut request = CommandRequest::new(
            command.get_program().to_string_lossy().into_owned(),
            std::path::PathBuf::from("/"),
            timeout,
        );
        request.args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        request.env = command
            .get_envs()
            .filter_map(|(name, value)| {
                value.map(|value| {
                    (
                        name.to_string_lossy().into_owned(),
                        value.to_string_lossy().into_owned(),
                    )
                })
            })
            .collect();
        request.max_output_bytes = MAX_STREAM_BYTES;
        request.sandbox = CommandSandbox::Host;
        request.persistence = Persistence::MetadataOnly;
        request.output = CommandOutput::Capture;
        Ok(request)
    }

    /// Runs one engine helper stage through the shared job runtime.
    async fn short(
        &self,
        plan: &ContainerPlan,
        stage: Stage<'_>,
    ) -> Result<harw_command::CommandOutcome, EngineError> {
        let request = self.request(plan, stage, HELPER_TIMEOUT)?;
        let outcome = self.port()?.run(request, CancelToken::default()).await;
        match &outcome.end {
            CommandEnd::Failed(reason) => Err(EngineError::Spawn(reason.clone())),
            CommandEnd::TimedOut => Err(EngineError::Unverified(
                "the engine did not answer in time".to_owned(),
            )),
            CommandEnd::Cancelled => Err(EngineError::Unverified(
                "the engine helper was cancelled".to_owned(),
            )),
            _ => Ok(outcome),
        }
    }

    /// `rm --force` (kills a running container too); best effort.
    async fn remove(&self, plan: &ContainerPlan, id: &ContainerId) {
        let _ = self.short(plan, Stage::Remove(id)).await;
    }
}

/// First line of an engine's stderr, bounded, for an error message.
fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(200)
        .collect()
}

impl ContainerEngine for PodmanEngine {
    fn run<'a>(
        &'a self,
        plan: &'a ContainerPlan,
        cancel: Option<&'a CancelToken>,
    ) -> EngineFuture<'a, Result<EngineRun, EngineError>> {
        Box::pin(async move {
            // 1. create: the container exists, nothing runs.
            let created = self.short(plan, Stage::Create).await?;
            if !created.is_success() {
                return Err(EngineError::Unverified(format!(
                    "create failed: {}",
                    first_line(&created.stderr)
                )));
            }
            let id = ContainerId::parse(String::from_utf8_lossy(&created.stdout).trim()).map_err(
                |_| EngineError::Unverified("the engine did not print a container id".to_owned()),
            )?;

            // 2. read back what the engine applied.
            let inspected = match self.short(plan, Stage::Inspect(&id)).await {
                Ok(output) if output.is_success() => output,
                _ => {
                    self.remove(plan, &id).await;
                    return Err(EngineError::Unverified(
                        "the container could not be read back".to_owned(),
                    ));
                }
            };
            let facts = match parse_inspect(&String::from_utf8_lossy(&inspected.stdout)) {
                Ok(facts) => facts,
                Err(reason) => {
                    self.remove(plan, &id).await;
                    return Err(EngineError::Unverified(format!(
                        "unreadable inspect output: {reason}"
                    )));
                }
            };

            // 3. verify; only a `VerifiedContainer` can be started.
            let readback = plan.verify(&id, &facts);
            let verified = match plan.authorize_start(&id, &facts) {
                Ok(verified) => verified,
                Err(refusal) => {
                    self.remove(plan, &id).await;
                    return Err(match refusal {
                        StartRefusal::Sources(error) => EngineError::Unverified(error.to_string()),
                        StartRefusal::NotEnforced(readback) => EngineError::NotEnforced(
                            readback
                                .failures()
                                .iter()
                                .map(|dimension| format!("{dimension:?}"))
                                .collect(),
                        ),
                    });
                }
            };

            // 4. start attached. The Podman client itself is a normal
            // job-runtime command; cancellation and the wall-clock limit are
            // enforced by CommandPort/harw-job, never by a direct spawn here.
            let limit = Duration::from_secs(u64::from(plan.timeout_s())) + self.grace;
            let request = self.request(plan, Stage::Start(&verified), limit)?;
            let token = cancel.cloned().unwrap_or_default();
            let outcome = self.port()?.run(request, token).await;
            let timed_out = outcome.end == CommandEnd::TimedOut;
            let cancelled = outcome.end == CommandEnd::Cancelled;
            let truncated = outcome.truncated || outcome.end == CommandEnd::OutputLimit;
            let exit_code = match outcome.end {
                CommandEnd::Exited => Some(outcome.exit_code),
                CommandEnd::Signaled(_)
                | CommandEnd::TimedOut
                | CommandEnd::OutputLimit
                | CommandEnd::Cancelled => None,
                CommandEnd::Failed(reason) => {
                    self.remove(plan, &id).await;
                    return Err(EngineError::Spawn(reason));
                }
            };
            // The container never outlives the call.
            self.remove(plan, &id).await;
            let stdout = outcome.stdout;
            let stderr = outcome.stderr;
            Ok(EngineRun {
                output: RunOutput {
                    exit_code,
                    stdout: String::from_utf8_lossy(&stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    truncated,
                    timed_out,
                    cancelled,
                },
                readback: Some(readback),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_tool_container::{HostPath, ImageRef, Profile, RunConfig, RunRequest};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const CID: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";

    /// The inspect document a correct engine would return for `plan`.
    fn inspect_json(plan: &ContainerPlan, tweak: impl FnOnce(&mut serde_json::Value)) -> String {
        let expected = plan.expected();
        let mounts: Vec<serde_json::Value> = expected
            .mounts
            .iter()
            .map(|m| {
                serde_json::json!({
                    "Type": "bind", "Source": m.source,
                    "Destination": m.destination, "RW": !m.read_only
                })
            })
            .collect();
        let mut doc = serde_json::json!([{
            "Id": CID,
            "EffectiveCaps": null,
            "HostConfig": {
                "Privileged": false, "CapAdd": null,
                "NetworkMode": "none", "ReadonlyRootfs": true,
                "SecurityOpt": ["no-new-privileges"],
                "Memory": expected.memory_bytes, "MemorySwap": expected.memory_bytes,
                "PidsLimit": expected.pids_limit
            },
            "Config": {"Timeout": expected.timeout_s},
            "Mounts": mounts
        }]);
        tweak(&mut doc[0]);
        doc.to_string()
    }

    /// A fake `podman`: `create` prints the id, `inspect` cats `inspect.json`
    /// (fails when absent), `start` runs `run_body`, `rm` appends to
    /// `rm.log`. Every verb is logged to `calls.log`.
    fn fake_podman(dir: &Path, run_body: &str) -> TestResult<String> {
        let path = dir.join("podman");
        let script = format!(
            "#!/bin/sh
d=\"$(dirname \"$0\")\"\necho \"$1\" >> \"$d/calls.log\"\ncase \"$1\" in\n  create) echo {CID} ;;\n  inspect) cat \"$d/inspect.json\" ;;\n  start) {run_body} ;;\n  rm) echo removed >> \"$d/rm.log\" ;;\nesac\n"
        );
        std::fs::write(&path, script)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        Ok(path.to_string_lossy().into_owned())
    }

    fn calls(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn plan(executable: &str, workspace: &Path, timeout: Option<u32>) -> TestResult<ContainerPlan> {
        let ws = HostPath::canonicalize(&workspace.to_string_lossy())?;
        let config = RunConfig::new(executable, &ws, "t1", "ses1")?;
        let image = ImageRef::parse(&format!("docker.io/library/rust@sha256:{HEX}"))?;
        let mut request = RunRequest::new(image, Profile::Hermetic, vec!["true".to_owned()])?;
        if let Some(seconds) = timeout {
            request = request.with_timeout_s(seconds);
        }
        Ok(ContainerPlan::build(&config, &request)?)
    }

    fn engine(state_dir: &Path) -> TestResult<PodmanEngine> {
        let port = harw_command::JobCommandPort::host(&state_dir.join("jobs"))
            .map_err(std::io::Error::other)?;
        let port: std::sync::Arc<dyn harw_command::CommandPort> = std::sync::Arc::new(port);
        Ok(
            PodmanEngine::new(vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())])
                .with_command_port(port)
                .with_grace(Duration::ZERO),
        )
    }

    /// A workspace directory with the fake engine next to it (the engine
    /// directory is not the mounted one).
    fn setup(run_body: &str) -> TestResult<(tempfile::TempDir, tempfile::TempDir, String)> {
        let bin = tempfile::tempdir()?;
        let ws = tempfile::tempdir()?;
        let exe = fake_podman(bin.path(), run_body)?;
        Ok((bin, ws, exe))
    }

    #[tokio::test]
    async fn a_verified_container_is_created_inspected_started_and_removed_in_order() -> TestResult
    {
        let (bin, ws, exe) = setup("echo out-line; echo err-line >&2; sleep 1")?;
        let plan = plan(&exe, ws.path(), None)?;
        std::fs::write(bin.path().join("inspect.json"), inspect_json(&plan, |_| {}))?;
        let run = engine(bin.path())?.run(&plan, None).await?;
        assert_eq!(run.output.exit_code, Some(0));
        assert_eq!(run.output.stdout.trim(), "out-line");
        assert_eq!(run.output.stderr.trim(), "err-line");
        assert!(run.readback.as_ref().is_some_and(Readback::all_enforced));
        assert_eq!(calls(bin.path()), ["create", "inspect", "start", "rm"]);
        Ok(())
    }

    #[tokio::test]
    async fn an_unenforced_dimension_is_removed_and_never_started() -> TestResult {
        let (bin, ws, exe) = setup("echo SHOULD-NOT-RUN")?;
        let plan = plan(&exe, ws.path(), None)?;
        std::fs::write(
            bin.path().join("inspect.json"),
            inspect_json(&plan, |doc| {
                doc["HostConfig"]["NetworkMode"] = "host".into()
            }),
        )?;
        let result = engine(bin.path())?.run(&plan, None).await;
        assert!(
            matches!(&result, Err(EngineError::NotEnforced(d)) if d == &["Network".to_owned()]),
            "{result:?}"
        );
        assert_eq!(calls(bin.path()), ["create", "inspect", "rm"], "no start");
        Ok(())
    }

    #[tokio::test]
    async fn capabilities_count_only_when_the_effective_set_is_empty() -> TestResult {
        let (bin, ws, exe) = setup("echo SHOULD-NOT-RUN")?;
        let plan = plan(&exe, ws.path(), None)?;
        // The engine claims CapDrop ALL but the process still holds a capability.
        std::fs::write(
            bin.path().join("inspect.json"),
            inspect_json(&plan, |doc| {
                doc["HostConfig"]["CapDrop"] = serde_json::json!(["ALL"]);
                doc["EffectiveCaps"] = serde_json::json!(["CAP_NET_RAW"]);
            }),
        )?;
        let result = engine(bin.path())?.run(&plan, None).await;
        assert!(
            matches!(&result, Err(EngineError::NotEnforced(d)) if d == &["Capabilities".to_owned()]),
            "{result:?}"
        );
        assert!(!calls(bin.path()).contains(&"start".to_owned()));
        Ok(())
    }

    #[tokio::test]
    async fn an_engine_that_cannot_be_read_back_means_nothing_runs() -> TestResult {
        let (bin, ws, exe) = setup("echo SHOULD-NOT-RUN")?;
        let plan = plan(&exe, ws.path(), None)?;
        // No inspect.json: every inspect fails.
        let result = engine(bin.path())?.run(&plan, None).await;
        assert!(
            matches!(result, Err(EngineError::Unverified(_))),
            "{result:?}"
        );
        assert_eq!(calls(bin.path()), ["create", "inspect", "rm"]);
        Ok(())
    }

    #[tokio::test]
    async fn another_container_id_in_the_read_back_is_refused() -> TestResult {
        let (bin, ws, exe) = setup("echo SHOULD-NOT-RUN")?;
        let plan = plan(&exe, ws.path(), None)?;
        std::fs::write(
            bin.path().join("inspect.json"),
            inspect_json(&plan, |doc| doc["Id"] = "cd".repeat(32).into()),
        )?;
        let result = engine(bin.path())?.run(&plan, None).await;
        assert!(
            matches!(&result, Err(EngineError::NotEnforced(d)) if d == &["Identity".to_owned()]),
            "{result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn output_beyond_the_bound_is_dropped_and_flagged() -> TestResult {
        let (bin, ws, exe) = setup("head -c 200000 /dev/zero | tr '\\0' x")?;
        let plan = plan(&exe, ws.path(), None)?;
        std::fs::write(bin.path().join("inspect.json"), inspect_json(&plan, |_| {}))?;
        let run = engine(bin.path())?.run(&plan, None).await?;
        assert_eq!(run.output.stdout.len(), MAX_STREAM_BYTES);
        assert!(run.output.truncated);
        Ok(())
    }

    #[tokio::test]
    async fn the_wall_time_limit_removes_a_hanging_container() -> TestResult {
        let (bin, ws, exe) = setup("sleep 30")?;
        let plan = plan(&exe, ws.path(), Some(1))?;
        std::fs::write(bin.path().join("inspect.json"), inspect_json(&plan, |_| {}))?;
        let started = std::time::Instant::now();
        let run = engine(bin.path())?.run(&plan, None).await?;
        assert!(run.output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(15));
        assert!(bin.path().join("rm.log").exists(), "rm --force was issued");
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_removes_the_container() -> TestResult {
        let (bin, ws, exe) = setup("sleep 30")?;
        let plan = plan(&exe, ws.path(), None)?;
        std::fs::write(bin.path().join("inspect.json"), inspect_json(&plan, |_| {}))?;
        let token = CancelToken::new();
        let canceller = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            canceller.cancel(harw_types::cancel::CancelReason::User);
        });
        let run = engine(bin.path())?.run(&plan, Some(&token)).await?;
        assert!(run.output.cancelled);
        assert!(bin.path().join("rm.log").exists());
        Ok(())
    }

    #[tokio::test]
    async fn a_missing_engine_is_a_spawn_error() -> TestResult {
        let ws = tempfile::tempdir()?;
        let plan = plan("/nonexistent/podman", ws.path(), None)?;
        let result = engine(ws.path())?.run(&plan, None).await;
        assert!(matches!(result, Err(EngineError::Spawn(_))), "{result:?}");
        Ok(())
    }
}
