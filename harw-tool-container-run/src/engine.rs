//! Running one [`ContainerPlan`]: spawn, runtime read-back, bounded output.

use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use harw_tool_container::{ContainerPlan, Enforcement, InspectFacts, Readback, verify};
use harw_types::cancel::CancelToken;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use crate::inspect::parse_inspect;

/// Largest captured size of stdout and of stderr in bytes; more is drained and
/// dropped (`truncated`).
pub const MAX_STREAM_BYTES: usize = 64 * 1024;
/// How long the first successful inspect is awaited.
const INSPECT_WINDOW: Duration = Duration::from_secs(8);
/// Pause between inspect attempts.
const INSPECT_PAUSE: Duration = Duration::from_millis(100);
/// Time one engine helper command (inspect, kill) may take.
const HELPER_TIMEOUT: Duration = Duration::from_secs(10);
/// How long pipe readers are awaited after a kill.
const READER_GRACE_AFTER_KILL: Duration = Duration::from_secs(2);
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
    /// Runtime read-back; `None` when the container ended before the first
    /// inspect (nothing could be verified).
    pub readback: Option<Readback>,
}

/// Why a run did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// The engine process could not be started.
    Spawn(String),
    /// The read-back reported a violation; the container was killed. The list
    /// names the dimensions that were not enforced.
    NotEnforced(Vec<String>),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(reason) => write!(f, "could not start the container engine: {reason}"),
            Self::NotEnforced(dimensions) => write!(
                f,
                "the engine did not enforce the requested isolation ({}); the container was killed",
                dimensions.join(", ")
            ),
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
}

impl PodmanEngine {
    /// Engine whose process environment is exactly `env`.
    #[must_use]
    pub fn new(env: Vec<(String, String)>) -> Self {
        Self { env, grace: GRACE }
    }

    /// Extra wall time beyond the plan's timeout before the container is
    /// killed (tests shorten it).
    #[must_use]
    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    /// `podman [--connection=X] <verb> <args…>` with the plan's connection.
    fn helper(&self, plan: &ContainerPlan, verb: &str, extra: &[&str]) -> Command {
        let mut command = Command::new(plan.executable());
        if let Some(connection) = plan
            .args()
            .first()
            .filter(|arg| arg.starts_with("--connection="))
        {
            command.arg(connection);
        }
        command.arg(verb);
        command.args(extra);
        command.env_clear();
        command.envs(self.env.iter().map(|(k, v)| (k, v)));
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::null());
        command.kill_on_drop(true);
        command
    }

    async fn inspect(&self, plan: &ContainerPlan) -> Option<InspectFacts> {
        let mut command = self.helper(
            plan,
            "inspect",
            &["--type=container", plan.container_name()],
        );
        let output = tokio::time::timeout(HELPER_TIMEOUT, command.output())
            .await
            .ok()?
            .ok()?;
        if !output.status.success() {
            return None;
        }
        parse_inspect(&String::from_utf8_lossy(&output.stdout)).ok()
    }

    async fn kill(&self, plan: &ContainerPlan) {
        let mut command = self.helper(plan, "kill", &[plan.container_name()]);
        let _ = tokio::time::timeout(HELPER_TIMEOUT, command.output()).await;
    }
}

/// Reads at most `limit` bytes; the rest is drained and dropped.
async fn capture<R: AsyncRead + Unpin>(mut reader: R, limit: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = limit.saturating_sub(kept.len());
                if n > room {
                    truncated = true;
                }
                kept.extend_from_slice(&chunk[..n.min(room)]);
            }
        }
    }
    (kept, truncated)
}

impl ContainerEngine for PodmanEngine {
    fn run<'a>(
        &'a self,
        plan: &'a ContainerPlan,
        cancel: Option<&'a CancelToken>,
    ) -> EngineFuture<'a, Result<EngineRun, EngineError>> {
        Box::pin(async move {
            let mut command = Command::from(plan.to_command(&self.env));
            command.stdout(Stdio::piped());
            command.stderr(Stdio::piped());
            command.kill_on_drop(true);
            let mut child = command
                .spawn()
                .map_err(|error| EngineError::Spawn(error.to_string()))?;
            let out = child.stdout.take();
            let err = child.stderr.take();
            let out_task = tokio::spawn(async move {
                match out {
                    Some(reader) => capture(reader, MAX_STREAM_BYTES).await,
                    None => (Vec::new(), false),
                }
            });
            let err_task = tokio::spawn(async move {
                match err {
                    Some(reader) => capture(reader, MAX_STREAM_BYTES).await,
                    None => (Vec::new(), false),
                }
            });

            let expected = plan.expected();
            let started = tokio::time::Instant::now();
            let mut readback: Option<Readback> = None;
            let mut violation: Option<Vec<String>> = None;
            let mut exited: Option<std::process::ExitStatus> = None;

            // Phase 1: runtime read-back while the container starts.
            while readback.is_none() && started.elapsed() < INSPECT_WINDOW {
                if let Ok(Some(status)) = child.try_wait() {
                    exited = Some(status);
                    break;
                }
                if let Some(facts) = self.inspect(plan).await {
                    let result = verify(expected, &facts);
                    let bad: Vec<String> = result
                        .entries()
                        .iter()
                        .filter(|(_, state)| *state == Enforcement::NotEnforced)
                        .map(|(dimension, _)| format!("{dimension:?}"))
                        .collect();
                    if !bad.is_empty() {
                        violation = Some(bad);
                    }
                    readback = Some(result);
                } else {
                    tokio::time::sleep(INSPECT_PAUSE).await;
                }
            }
            if let Some(dimensions) = violation {
                self.kill(plan).await;
                let _ = child.kill().await;
                return Err(EngineError::NotEnforced(dimensions));
            }

            // Phase 2: wait for exit, the wall-time limit or cancellation.
            let limit = Duration::from_secs(u64::from(plan.timeout_s())) + self.grace;
            let (mut timed_out, mut cancelled) = (false, false);
            let status = if let Some(status) = exited {
                Some(status)
            } else {
                let cancel_wait = async {
                    match cancel {
                        Some(token) => token.cancelled().await,
                        None => std::future::pending::<()>().await,
                    }
                };
                tokio::select! {
                    result = child.wait() => result.ok(),
                    () = tokio::time::sleep(limit) => {
                        timed_out = true;
                        None
                    }
                    () = cancel_wait => {
                        cancelled = true;
                        None
                    }
                }
            };
            if timed_out || cancelled {
                self.kill(plan).await;
                let _ = child.kill().await;
            }
            // After a kill a grandchild may still hold a pipe open: do not wait
            // for end-of-stream forever.
            let reader_wait = if timed_out || cancelled {
                READER_GRACE_AFTER_KILL
            } else {
                Duration::from_secs(3600)
            };
            let (stdout, out_truncated) = tokio::time::timeout(reader_wait, out_task)
                .await
                .map_or_else(|_| (Vec::new(), true), Result::unwrap_or_default);
            let (stderr, err_truncated) = tokio::time::timeout(reader_wait, err_task)
                .await
                .map_or_else(|_| (Vec::new(), true), Result::unwrap_or_default);
            Ok(EngineRun {
                output: RunOutput {
                    exit_code: status.and_then(|s| s.code()),
                    stdout: String::from_utf8_lossy(&stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    truncated: out_truncated || err_truncated,
                    timed_out,
                    cancelled,
                },
                readback,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_tool_container::{ImageRef, Profile, RunConfig, RunRequest};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const GOOD: &str = r#"[{
        "EffectiveCaps": null,
        "HostConfig": {"Privileged": false, "CapAdd": null, "CapDrop": ["ALL"],
            "NetworkMode": "none", "ReadonlyRootfs": true,
            "SecurityOpt": ["no-new-privileges"],
            "Memory": 1073741824, "MemorySwap": 1073741824, "PidsLimit": 256},
        "Mounts": [{"Destination": "/workspace", "RW": false}]
    }]"#;

    /// A fake `podman`: `run` prints and sleeps, `inspect` cats `inspect.json`
    /// (fails when absent), `kill` appends to `kill.log`.
    fn fake_podman(dir: &Path, run_body: &str) -> TestResult<String> {
        let path = dir.join("podman");
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  run) {run_body} ;;\n  inspect) cat \"$(dirname \"$0\")/inspect.json\" ;;\n  kill) echo killed >> \"$(dirname \"$0\")/kill.log\" ;;\nesac\n"
        );
        std::fs::write(&path, script)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        Ok(path.to_string_lossy().into_owned())
    }

    fn plan(executable: &str, workspace: &str, timeout: Option<u32>) -> TestResult<ContainerPlan> {
        let config = RunConfig::new(executable, workspace, "t1", "ses1")?;
        let image = ImageRef::parse(&format!("docker.io/library/rust@sha256:{HEX}"))?;
        let mut request = RunRequest::new(image, Profile::Hermetic, vec!["true".to_owned()])?;
        if let Some(seconds) = timeout {
            request = request.with_timeout_s(seconds);
        }
        Ok(ContainerPlan::build(&config, &request)?)
    }

    fn engine() -> PodmanEngine {
        PodmanEngine::new(vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())])
            .with_grace(Duration::ZERO)
    }

    #[tokio::test]
    async fn a_verified_run_returns_bounded_output_and_an_enforced_readback() -> TestResult {
        let dir = tempfile::tempdir()?;
        let exe = fake_podman(dir.path(), "echo out-line; echo err-line >&2; sleep 1")?;
        std::fs::write(dir.path().join("inspect.json"), GOOD)?;
        let plan = plan(&exe, &dir.path().to_string_lossy(), None)?;
        let run = engine().run(&plan, None).await?;
        assert_eq!(run.output.exit_code, Some(0));
        assert_eq!(run.output.stdout.trim(), "out-line");
        assert_eq!(run.output.stderr.trim(), "err-line");
        assert!(run.readback.as_ref().is_some_and(Readback::all_enforced));
        Ok(())
    }

    #[tokio::test]
    async fn a_reported_violation_kills_the_container() -> TestResult {
        let dir = tempfile::tempdir()?;
        let exe = fake_podman(dir.path(), "sleep 30")?;
        std::fs::write(
            dir.path().join("inspect.json"),
            GOOD.replace("\"none\"", "\"host\""),
        )?;
        let plan = plan(&exe, &dir.path().to_string_lossy(), None)?;
        let started = std::time::Instant::now();
        let result = engine().run(&plan, None).await;
        assert!(
            matches!(&result, Err(EngineError::NotEnforced(d)) if d == &["Network".to_owned()]),
            "{result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "killed, not waited out"
        );
        assert!(
            dir.path().join("kill.log").exists(),
            "engine kill was issued"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_container_that_ends_before_inspect_is_reported_unverified() -> TestResult {
        let dir = tempfile::tempdir()?;
        let exe = fake_podman(dir.path(), "echo quick")?;
        // No inspect.json: every inspect fails.
        let plan = plan(&exe, &dir.path().to_string_lossy(), None)?;
        let run = engine().run(&plan, None).await?;
        assert_eq!(run.output.stdout.trim(), "quick");
        assert!(
            run.readback.is_none(),
            "nothing was verified and it says so"
        );
        Ok(())
    }

    #[tokio::test]
    async fn output_beyond_the_bound_is_dropped_and_flagged() -> TestResult {
        let dir = tempfile::tempdir()?;
        let exe = fake_podman(dir.path(), "head -c 200000 /dev/zero | tr '\\0' x")?;
        let plan = plan(&exe, &dir.path().to_string_lossy(), None)?;
        let run = engine().run(&plan, None).await?;
        assert_eq!(run.output.stdout.len(), MAX_STREAM_BYTES);
        assert!(run.output.truncated);
        Ok(())
    }

    #[tokio::test]
    async fn the_wall_time_limit_kills_a_hanging_container() -> TestResult {
        let dir = tempfile::tempdir()?;
        let exe = fake_podman(dir.path(), "sleep 30")?;
        std::fs::write(dir.path().join("inspect.json"), GOOD)?;
        let plan = plan(&exe, &dir.path().to_string_lossy(), Some(1))?;
        let started = std::time::Instant::now();
        let run = engine().run(&plan, None).await?;
        assert!(run.output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(15));
        assert!(dir.path().join("kill.log").exists());
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_kills_the_container() -> TestResult {
        let dir = tempfile::tempdir()?;
        let exe = fake_podman(dir.path(), "sleep 30")?;
        std::fs::write(dir.path().join("inspect.json"), GOOD)?;
        let plan = plan(&exe, &dir.path().to_string_lossy(), None)?;
        let token = CancelToken::new();
        let canceller = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            canceller.cancel(harw_types::cancel::CancelReason::User);
        });
        let run = engine().run(&plan, Some(&token)).await?;
        assert!(run.output.cancelled);
        assert!(dir.path().join("kill.log").exists());
        Ok(())
    }

    #[tokio::test]
    async fn a_missing_engine_is_a_spawn_error() -> TestResult {
        let plan = plan("/nonexistent/podman", "/tmp", None)?;
        let result = engine().run(&plan, None).await;
        assert!(matches!(result, Err(EngineError::Spawn(_))), "{result:?}");
        Ok(())
    }
}
