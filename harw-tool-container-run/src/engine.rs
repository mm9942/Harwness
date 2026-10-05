//! Running one [`ContainerPlan`]: `create`, read back, verify, only then
//! `start`; bounded output.

use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use harw_tool_container::{ContainerId, ContainerPlan, Readback, Stage, StartRefusal};
use harw_types::cancel::CancelToken;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use crate::inspect::parse_inspect;

/// Largest captured size of stdout and of stderr in bytes; more is drained and
/// dropped (`truncated`).
pub const MAX_STREAM_BYTES: usize = 64 * 1024;
/// Time one engine helper command (`create`, `inspect`, `rm`) may take.
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);
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
}

impl PodmanEngine {
    /// Engine whose process environment is exactly `env`.
    #[must_use]
    pub fn new(env: Vec<(String, String)>) -> Self {
        Self { env, grace: GRACE }
    }

    /// Extra wall time beyond the plan's timeout before the container is
    /// removed (tests shorten it).
    #[must_use]
    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    /// The command of one lifecycle stage: argument vector and environment
    /// come from the plan (the environment is rebuilt from an allowlist, our
    /// `env` only supplies the values).
    fn command(&self, plan: &ContainerPlan, stage: Stage<'_>) -> Result<Command, EngineError> {
        let lookup = |name: &str| {
            self.env
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        let command = plan
            .to_command(stage, &lookup)
            .map_err(|error| EngineError::Unverified(error.to_string()))?;
        let mut command = Command::from(command);
        command.kill_on_drop(true);
        Ok(command)
    }

    /// Runs a short stage to completion with captured output.
    async fn short(
        &self,
        plan: &ContainerPlan,
        stage: Stage<'_>,
    ) -> Result<std::process::Output, EngineError> {
        let mut command = self.command(plan, stage)?;
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        let child = spawn_with_retry(&mut command).await?;
        tokio::time::timeout(HELPER_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| EngineError::Unverified("the engine did not answer in time".to_owned()))?
            .map_err(|error| EngineError::Spawn(error.to_string()))
    }

    /// `rm --force` (kills a running container too); best effort.
    async fn remove(&self, plan: &ContainerPlan, id: &ContainerId) {
        let _ = self.short(plan, Stage::Remove(id)).await;
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

/// `ETXTBSY` ("text file busy", errno 26) on `exec` means another thread of
/// this process still holds a write handle to the executable (a freshly
/// written or replaced binary, or an fd inherited by a concurrent fork until
/// that child execs). It is transient; retry a few times, nothing else.
const ETXTBSY: i32 = 26;
const SPAWN_ATTEMPTS: usize = 5;

async fn spawn_with_retry(command: &mut Command) -> Result<tokio::process::Child, EngineError> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error) if error.raw_os_error() == Some(ETXTBSY) && attempt < SPAWN_ATTEMPTS => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(error) => return Err(EngineError::Spawn(error.to_string())),
        }
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
            if !created.status.success() {
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
                Ok(output) if output.status.success() => output,
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

            // 4. start attached, with output bounds, the wall-time limit and
            // cancellation.
            let mut command = self.command(plan, Stage::Start(&verified))?;
            command.stdout(Stdio::piped());
            command.stderr(Stdio::piped());
            let mut child = match spawn_with_retry(&mut command).await {
                Ok(child) => child,
                Err(error) => {
                    self.remove(plan, &id).await;
                    return Err(error);
                }
            };
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

            let limit = Duration::from_secs(u64::from(plan.timeout_s())) + self.grace;
            let (mut timed_out, mut cancelled) = (false, false);
            let cancel_wait = async {
                match cancel {
                    Some(token) => token.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            };
            let status = tokio::select! {
                result = child.wait() => result.ok(),
                () = tokio::time::sleep(limit) => {
                    timed_out = true;
                    None
                }
                () = cancel_wait => {
                    cancelled = true;
                    None
                }
            };
            if timed_out || cancelled {
                let _ = child.kill().await;
            }
            // The container never outlives the call: `rm --force` also stops a
            // running one.
            self.remove(plan, &id).await;
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

    fn engine() -> PodmanEngine {
        PodmanEngine::new(vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())])
            .with_grace(Duration::ZERO)
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
        let run = engine().run(&plan, None).await?;
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
        let result = engine().run(&plan, None).await;
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
        let result = engine().run(&plan, None).await;
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
        let result = engine().run(&plan, None).await;
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
        let result = engine().run(&plan, None).await;
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
        let run = engine().run(&plan, None).await?;
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
        let run = engine().run(&plan, None).await?;
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
        let run = engine().run(&plan, Some(&token)).await?;
        assert!(run.output.cancelled);
        assert!(bin.path().join("rm.log").exists());
        Ok(())
    }

    #[tokio::test]
    async fn a_missing_engine_is_a_spawn_error() -> TestResult {
        let ws = tempfile::tempdir()?;
        let plan = plan("/nonexistent/podman", ws.path(), None)?;
        let result = engine().run(&plan, None).await;
        assert!(matches!(result, Err(EngineError::Spawn(_))), "{result:?}");
        Ok(())
    }
}
