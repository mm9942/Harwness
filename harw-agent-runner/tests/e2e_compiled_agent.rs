//! End-to-end tests of the compiled agent binary: compile a small worker
//! with `harw-agent-compiler`'s public API, append its artifact to the
//! `harw-agent-runner` binary itself
//! (`env!("CARGO_BIN_EXE_harw-agent-runner")`) with
//! `harw_agent_artifact::append_to_executable` — the exact steps
//! `harw-agent-compiler/src/commands.rs`'s `run_agent` and
//! `harw-agent-runner/src/verify.rs`'s own tests take — then spawn the
//! result and talk to it as a real process.
//!
//! # Offline-only scope
//! No test here ever lets the compiled agent talk to a model provider.
//! `RunnerContext::harwness`/`iface::approval::build_harwness` (what every
//! interface actually builds its session on) never call
//! `HarwnessBuilder::offline_echo`, and no env var switches the runner
//! itself to the built-in echo — that knob only exists on the SDK builder,
//! which this binary does not expose on its command line. So every test
//! that would have to run a turn (an actual model round trip) is
//! `#[ignore]`d with that reason instead of guessing at network access.
//! Everything that only needs the embedded artifact, the manifest, or the
//! protocol layer above a session runs for real.
//!
//! # Isolation
//! Each spawned child gets its own `HARW_HOME` (a fresh temp directory), so
//! a test never touches a real `~/.harw`. Every child is wrapped in
//! [`ChildGuard`], which kills and reaps it on drop — including on an early
//! return from `?` — so a failing assertion can never leave a process
//! behind. One-shot commands are read with a timeout
//! ([`run_with_timeout`]) so a hang never blocks the suite.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use harw_agent_artifact::{Artifact, append_to_executable, write_executable};
use harw_agent_compiler::{AgentInput, Compiled, Compiler, CompilerEnv, CompilerOptions};
use harw_agent_runner::error::{EXIT_DATA, EXIT_USAGE};
use harw_agent_runner::iface::mcp::MCP_PROTOCOL_VERSION;
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// How long a one-shot command (`--verify`/`--manifest`/…) may take before a
/// test gives up and kills it.
const SHORT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long an interactive child (MCP stdio, the HTTP listener) may take to
/// answer one request.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Compiling a small worker (harw-agent-compiler's public API)
// ---------------------------------------------------------------------------

/// The shared body every worker fixture here extends: `worker-base`, no
/// spawn children — a leaf agent, same shape as `reader`/`writer` in
/// `harw-agent-compiler/tests/compile.rs`.
const WORKER_BODY: &str = "schema = \"harwness.agent/v1\"\nversion = \"1.0.0\"\nextends = { id = \"harwness.agent.worker-base@1\" }\nrole = \"worker\"\n";

/// A worker definition admitted to read files only, exposing `cli`, `mcp`
/// and `http` (so `--interface`/MCP/HTTP tests all have something to talk
/// to); no `[models]` section, so compiling it never requires a provider
/// credential.
fn worker_definition(name: &str) -> String {
    format!(
        "id = \"acme.agent.{name}@1\"\nspecialization = \"{name}\"\n{WORKER_BODY}\n\
         [binary]\ninterfaces = [\"cli\", \"mcp\", \"http\"]\ndefault_interface = \"cli\"\n\n\
         [tools]\nadmitted = [\"fs.read\"]\n"
    )
}

/// An isolated `harw` home with `definitions` under `agents/<name>/`, mirroring
/// `harw-agent-compiler/tests/compile.rs`'s `home_with`.
fn home_with(definitions: &[(&str, &str)]) -> Result<(tempfile::TempDir, CompilerEnv), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    for (name, text) in definitions {
        let dir = home.join("agents").join(name);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("definition.toml"), text)?;
    }
    std::fs::create_dir_all(&home)?;
    let env = CompilerEnv::isolated(home, root.path().to_path_buf());
    Ok((root, env))
}

fn compile(env: &CompilerEnv, name: &str) -> Result<Compiled, Box<dyn std::error::Error>> {
    let mut compiler = Compiler::new(env.clone(), CompilerOptions::default())?;
    let compiled = compiler.compile_input(&AgentInput::Name(name.to_owned()))?;
    Ok(compiled)
}

/// Compiles one worker named `name` in its own isolated home and returns the
/// artifact plus the temp home that must outlive it.
fn compile_worker(name: &str) -> Result<(tempfile::TempDir, Artifact), Box<dyn std::error::Error>> {
    let definition = worker_definition(name);
    let (root, env) = home_with(&[(name, &definition)])?;
    let compiled = compile(&env, name)?;
    Ok((root, compiled.artifact))
}

// ---------------------------------------------------------------------------
// Building the compiled agent binary
// ---------------------------------------------------------------------------

/// `runner ‖ artifact ‖ footer`, written to `dir/<name>` and made
/// executable — the same construction `harw-agent-compiler`'s artifact
/// backend and `run_agent` use, and what `harw-agent-runner/src/verify.rs`'s
/// own tests build by hand for a fake bundle.
fn build_agent_binary(
    dir: &Path,
    name: &str,
    artifact: &Artifact,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let runner_bytes = std::fs::read(env!("CARGO_BIN_EXE_harw-agent-runner"))?;
    let binary = append_to_executable(&runner_bytes, artifact);
    let path = dir.join(name);
    write_executable(&path, &binary)?;
    Ok(path)
}

/// As [`build_agent_binary`], but with one byte inside the *artifact*
/// region flipped before the footer is trusted — `harw-agent-artifact`'s own
/// `embed.rs` tests flip bytes at the same kind of offset
/// (`start..end`, i.e. after the runner bytes and before the footer) to
/// prove tampering is caught; this reuses that idea against a real runner
/// binary instead of a synthetic `RUNNER` blob.
fn build_tampered_agent_binary(
    dir: &Path,
    name: &str,
    artifact: &Artifact,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let runner_bytes = std::fs::read(env!("CARGO_BIN_EXE_harw-agent-runner"))?;
    let runner_len = runner_bytes.len();
    let mut binary = append_to_executable(&runner_bytes, artifact);
    let flip_at = runner_len + 8; // a few bytes into the artifact's own header.
    let byte = binary
        .get_mut(flip_at)
        .ok_or("artifact too small to tamper with")?;
    *byte ^= 0xFF;
    let path = dir.join(name);
    write_executable(&path, &binary)?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// Spawning: a kill-on-drop guard and a timed one-shot runner
// ---------------------------------------------------------------------------

/// Wraps a spawned child so it is always killed and reaped when the guard
/// drops, including on an early `?` return from a failing assertion — no
/// test here may leave a process behind.
struct ChildGuard(Child);

impl std::ops::Deref for ChildGuard {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A completed one-shot invocation.
struct Finished {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Spawns `exe args…` with `HARW_HOME=home`, stdin closed (no interface here
/// should ever block reading it once past `--verify`/`--manifest`/an
/// interface error), and waits up to `timeout` for it to exit, killing it
/// otherwise. Never hangs the suite: on timeout `ChildGuard::drop` reaps the
/// killed process and this returns an error instead of blocking.
fn run_with_timeout(
    exe: &Path,
    args: &[&str],
    home: &Path,
    timeout: Duration,
) -> Result<Finished, Box<dyn std::error::Error>> {
    let mut guard = ChildGuard(
        Command::new(exe)
            .args(args)
            .env("HARW_HOME", home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let mut stdout_pipe = guard.stdout.take().ok_or("no stdout pipe")?;
    let mut stderr_pipe = guard.stderr.take().ok_or("no stderr pipe")?;
    let stdout_handle = thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buffer);
        buffer
    });
    let stderr_handle = thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buffer);
        buffer
    });

    let start = Instant::now();
    let status = loop {
        if let Some(status) = guard.try_wait()? {
            break Some(status);
        }
        if start.elapsed() > timeout {
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let status = status.ok_or("child did not exit within the timeout")?;

    let stdout = stdout_handle
        .join()
        .map_err(|_| "stdout reader thread panicked")?;
    let stderr = stderr_handle
        .join()
        .map_err(|_| "stderr reader thread panicked")?;
    Ok(Finished {
        code: status.code(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

// ---------------------------------------------------------------------------
// A tiny MCP stdio client (newline-delimited JSON-RPC 2.0)
// ---------------------------------------------------------------------------

/// Talks to a spawned `--interface mcp` child over its stdio pipes. A
/// background thread reads response lines into a channel so `recv` can be
/// bounded by a timeout instead of blocking on `BufRead::read_line` forever.
struct McpClient {
    guard: ChildGuard,
    stdin: std::process::ChildStdin,
    lines: mpsc::Receiver<String>,
}

impl McpClient {
    fn spawn(exe: &Path, home: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut guard = ChildGuard(
            Command::new(exe)
                .args(["--interface", "mcp"])
                .env("HARW_HOME", home)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?,
        );
        let stdin = guard.stdin.take().ok_or("no stdin pipe")?;
        let stdout = guard.stdout.take().ok_or("no stdout pipe")?;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if tx.send(line.clone()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self { guard, stdin, lines: rx })
    }

    fn send(&mut self, request: &Value) -> Result<(), Box<dyn std::error::Error>> {
        let line = serde_json::to_string(request)?;
        writeln!(self.stdin, "{line}")?;
        self.stdin.flush()?;
        Ok(())
    }

    /// Reads one JSON-RPC line within [`IO_TIMEOUT`], skipping any
    /// out-of-band `notifications/*` message that isn't the response this
    /// call is waiting for.
    fn recv_response(&self) -> Result<Value, Box<dyn std::error::Error>> {
        let deadline = Instant::now() + IO_TIMEOUT;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or("timed out waiting for an MCP response")?;
            let line = self.lines.recv_timeout(remaining)?;
            let value: Value = serde_json::from_str(&line)?;
            if value.get("method").and_then(Value::as_str).is_some() && value.get("id").is_none() {
                continue; // a notification; keep waiting for the response.
            }
            return Ok(value);
        }
    }

    /// `initialize` then the required `notifications/initialized`, as any
    /// well-behaved client must send before `tools/list`/`tools/call`.
    fn initialize(&mut self) -> Result<Value, Box<dyn std::error::Error>> {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "harw-agent-runner-e2e-test", "version": "0"}
            }
        }))?;
        let response = self.recv_response()?;
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        Ok(response)
    }
}

// ---------------------------------------------------------------------------
// A tiny HTTP/1.1 client over a raw TcpStream (no extra dev-dependency)
// ---------------------------------------------------------------------------

/// One GET, connection: close, parsed just enough for these tests: the
/// status code and the body. Retries the connect for a short while so the
/// test does not race the child's listener startup.
fn http_get(addr: &str, path: &str) -> Result<(u16, String), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + IO_TIMEOUT;
    loop {
        match TcpStream::connect(addr) {
            Ok(stream) => return read_http_response(stream, addr, path),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(Box::new(error));
                }
                thread::sleep(Duration::from_millis(30));
            }
        }
    }
}

fn read_http_response(
    mut stream: TcpStream,
    addr: &str,
    path: &str,
) -> Result<(u16, String), Box<dyn std::error::Error>> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes())?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let text = String::from_utf8_lossy(&response).into_owned();
    let mut parts = text.splitn(2, "\r\n\r\n");
    let head = parts.next().ok_or("empty HTTP response")?;
    let body = parts.next().unwrap_or_default().to_owned();
    let status_line = head.lines().next().ok_or("no status line")?;
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .ok_or("no status code in the status line")?
        .parse()?;
    Ok((code, body))
}

/// A free `127.0.0.1` port: bind a listener to port `0`, read back what the
/// OS assigned, then drop it — the small race until the next bind is the
/// usual, accepted cost of this pattern in tests.
fn free_loopback_port() -> Result<u16, Box<dyn std::error::Error>> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

#[test]
fn test_verify_reports_ok_for_a_freshly_compiled_artifact() -> TestResult {
    let (_root, artifact) = compile_worker("verifier")?;
    let out_dir = tempfile::tempdir()?;
    let exe = build_agent_binary(out_dir.path(), "verifier", &artifact)?;
    let home = tempfile::tempdir()?;

    let finished = run_with_timeout(&exe, &["--verify", "--json"], home.path(), SHORT_TIMEOUT)?;
    assert_eq!(finished.code, Some(0), "stderr: {}", finished.stderr);
    let payload: Value = serde_json::from_str(finished.stdout.trim())?;
    assert_eq!(payload["ok"], Value::Bool(true), "{payload}");
    assert_eq!(
        payload["digest"].as_str(),
        Some(artifact.digest().to_string().as_str())
    );
    Ok(())
}

#[test]
fn test_a_flipped_artifact_byte_fails_to_start() -> TestResult {
    let (_root, artifact) = compile_worker("tampered")?;
    let out_dir = tempfile::tempdir()?;
    let exe = build_tampered_agent_binary(out_dir.path(), "tampered", &artifact)?;
    let home = tempfile::tempdir()?;

    // No flags at all: the runner must refuse before ever picking an
    // interface, fail-closed, the same load path `--verify` takes.
    let finished = run_with_timeout(&exe, &[], home.path(), SHORT_TIMEOUT)?;
    assert_ne!(finished.code, Some(0), "stdout: {}", finished.stdout);
    assert_eq!(
        finished.code,
        Some(i32::from(EXIT_DATA)),
        "a tampered artifact must map to EXIT_DATA; stderr: {}",
        finished.stderr
    );
    assert!(
        finished.stderr.contains("embedded artifact") || finished.stderr.contains("tampered"),
        "{}",
        finished.stderr
    );

    // `--verify` on its own reports the same tamper as a plain pass/fail,
    // not the sysexits code above (see `verify::run_verify`'s doc).
    let verify = run_with_timeout(&exe, &["--verify"], home.path(), SHORT_TIMEOUT)?;
    assert_eq!(verify.code, Some(1), "stdout: {}", verify.stdout);
    Ok(())
}

#[test]
fn test_manifest_json_reports_the_compiled_permissions() -> TestResult {
    let (_root, artifact) = compile_worker("manifested")?;
    let out_dir = tempfile::tempdir()?;
    let exe = build_agent_binary(out_dir.path(), "manifested", &artifact)?;
    let home = tempfile::tempdir()?;

    let finished = run_with_timeout(&exe, &["--manifest", "--json"], home.path(), SHORT_TIMEOUT)?;
    assert_eq!(finished.code, Some(0), "stderr: {}", finished.stderr);
    let permissions: Value = serde_json::from_str(finished.stdout.trim())?;
    let tools = permissions["tools"]
        .as_array()
        .ok_or("permissions.tools must be an array")?;
    assert!(
        tools.iter().any(|tool| tool == "fs.read"),
        "{permissions}"
    );
    assert_eq!(permissions["shell"], Value::Bool(false), "{permissions}");
    assert_eq!(permissions["host"], Value::Bool(false), "{permissions}");
    Ok(())
}

#[test]
fn test_full_access_does_not_widen_the_manifest() -> TestResult {
    let (_root, artifact) = compile_worker("fullaccess")?;
    let out_dir = tempfile::tempdir()?;
    let exe = build_agent_binary(out_dir.path(), "fullaccess", &artifact)?;
    let home = tempfile::tempdir()?;

    let plain = run_with_timeout(&exe, &["--manifest", "--json"], home.path(), SHORT_TIMEOUT)?;
    let widened = run_with_timeout(
        &exe,
        &["--manifest", "--json", "--full-access"],
        home.path(),
        SHORT_TIMEOUT,
    )?;
    assert_eq!(plain.code, Some(0), "stderr: {}", plain.stderr);
    assert_eq!(widened.code, Some(0), "stderr: {}", widened.stderr);
    // `--manifest` answers straight from the verified artifact, before any
    // `RightsFlags` narrowing is ever applied — `--full-access` cannot
    // change what it prints.
    assert_eq!(plain.stdout, widened.stdout);
    Ok(())
}

#[test]
fn test_requesting_an_interface_the_manifest_does_not_allow_reports_a_clear_error() -> TestResult {
    let (_root, artifact) = compile_worker("ifaceerror")?;
    let out_dir = tempfile::tempdir()?;
    let exe = build_agent_binary(out_dir.path(), "ifaceerror", &artifact)?;
    let home = tempfile::tempdir()?;

    // The fixture's `[binary] interfaces` is `["cli", "mcp", "http"]`; `repl`
    // is compiled into this runner build (it is a default feature) but not
    // one this agent's manifest allows, so this must fail with
    // `RunnerError::UnknownInterface`, not `NotCompiled`.
    let finished = run_with_timeout(
        &exe,
        &["--interface", "repl"],
        home.path(),
        SHORT_TIMEOUT,
    )?;
    assert_eq!(
        finished.code,
        Some(i32::from(EXIT_USAGE)),
        "stdout: {} stderr: {}",
        finished.stdout,
        finished.stderr
    );
    assert!(finished.stderr.contains("is not available"), "{}", finished.stderr);
    assert!(finished.stderr.contains("cli"), "{}", finished.stderr);
    Ok(())
}

#[test]
fn test_mcp_initialize_then_tools_list() -> TestResult {
    let (_root, artifact) = compile_worker("mcplist")?;
    let out_dir = tempfile::tempdir()?;
    let exe = build_agent_binary(out_dir.path(), "mcplist", &artifact)?;
    let home = tempfile::tempdir()?;

    let mut client = McpClient::spawn(&exe, home.path())?;
    let initialize = client.initialize()?;
    assert_eq!(
        initialize["result"]["protocolVersion"].as_str(),
        Some(MCP_PROTOCOL_VERSION)
    );

    client.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))?;
    let response = client.recv_response()?;
    let tools = response["result"]["tools"]
        .as_array()
        .ok_or("tools/list must return a tools array")?;
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(names, vec!["run", "status", "cancel"], "{response}");
    Ok(())
}

#[test]
fn test_mcp_tools_call_with_an_unknown_tool_is_refused() -> TestResult {
    let (_root, artifact) = compile_worker("mcprefuse")?;
    let out_dir = tempfile::tempdir()?;
    let exe = build_agent_binary(out_dir.path(), "mcprefuse", &artifact)?;
    let home = tempfile::tempdir()?;

    let mut client = McpClient::spawn(&exe, home.path())?;
    client.initialize()?;

    // `run`/`status`/`cancel` are this interface's whole tool surface
    // regardless of the manifest (see `iface::mcp`'s module doc); a name
    // outside it — here also one the manifest never admitted — is refused
    // without ever touching a session or a model.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {"name": "fs.write", "arguments": {}}
    }))?;
    let response = client.recv_response()?;
    assert!(response.get("result").is_none(), "{response}");
    assert_eq!(response["error"]["code"], Value::from(-32601), "{response}");
    Ok(())
}

#[test]
fn test_http_healthz_and_manifest() -> TestResult {
    let (_root, artifact) = compile_worker("httpread")?;
    let out_dir = tempfile::tempdir()?;
    let exe = build_agent_binary(out_dir.path(), "httpread", &artifact)?;
    let home = tempfile::tempdir()?;
    let port = free_loopback_port()?;
    let addr = format!("127.0.0.1:{port}");

    let _guard = ChildGuard(
        Command::new(&exe)
            .args(["--interface", "http", "--listen", &addr])
            .env("HARW_HOME", home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?,
    );

    let (health_code, health_body) = http_get(&addr, "/healthz")?;
    assert_eq!(health_code, 200, "{health_body}");
    let health: Value = serde_json::from_str(&health_body)?;
    assert_eq!(health["status"], Value::from("ok"), "{health}");

    let (manifest_code, manifest_body) = http_get(&addr, "/manifest")?;
    assert_eq!(manifest_code, 200, "{manifest_body}");
    let manifest: Value = serde_json::from_str(&manifest_body)?;
    let tools = manifest["permissions"]["tools"]
        .as_array()
        .ok_or("manifest.permissions.tools must be an array")?;
    assert!(tools.iter().any(|tool| tool == "fs.read"), "{manifest}");
    let interfaces = manifest["interfaces"]
        .as_array()
        .ok_or("manifest.interfaces must be an array")?;
    let interfaces: Vec<&str> = interfaces.iter().filter_map(Value::as_str).collect();
    assert_eq!(interfaces, vec!["cli", "mcp", "http"], "{manifest}");
    Ok(())
}

#[test]
fn test_identical_input_builds_byte_identical_artifacts() -> TestResult {
    // Two independent homes, two independent compiles of the same
    // definition text: no shared cache, no shared timestamp source beyond
    // `CompilerOptions::default()` itself, matching
    // `test_evidence_critic_artifact_is_reproducible` in
    // `harw-agent-compiler/tests/compile.rs`.
    let definition = worker_definition("deterministic");
    let (_root_a, env_a) = home_with(&[("deterministic", &definition)])?;
    let (_root_b, env_b) = home_with(&[("deterministic", &definition)])?;
    let first = compile(&env_a, "deterministic")?;
    let second = compile(&env_b, "deterministic")?;
    assert_eq!(first.artifact.digest(), second.artifact.digest());
    assert_eq!(first.artifact.to_bytes(), second.artifact.to_bytes());
    Ok(())
}

// ---------------------------------------------------------------------------
// Model-dependent cases: no offline switch reaches these paths (see the
// module doc), so they are skipped rather than assumed to work without
// network or a configured provider credential.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "needs a real (or offline-echo) model provider: iface::cli's \
            build_harwness never calls HarwnessBuilder::offline_echo, so a \
            one-shot prompt here would try the manifest's configured \
            provider over the network"]
fn test_cli_one_shot_answers_with_echo() -> TestResult {
    let (_root, artifact) = compile_worker("clione")?;
    let out_dir = tempfile::tempdir()?;
    let exe = build_agent_binary(out_dir.path(), "clione", &artifact)?;
    let home = tempfile::tempdir()?;

    let finished = run_with_timeout(&exe, &["hello there"], home.path(), SHORT_TIMEOUT)?;
    assert_eq!(finished.code, Some(0), "stderr: {}", finished.stderr);
    assert!(finished.stdout.contains("echo:"), "{}", finished.stdout);
    Ok(())
}

#[test]
#[ignore = "needs a real (or offline-echo) model provider: HarwnessPromptRunner \
            builds its Harwness the same un-echoed way as iface::cli, so \
            tools/call run would try to reach a network provider"]
fn test_mcp_tools_call_run_answers_with_echo() -> TestResult {
    let (_root, artifact) = compile_worker("mcprun")?;
    let out_dir = tempfile::tempdir()?;
    let exe = build_agent_binary(out_dir.path(), "mcprun", &artifact)?;
    let home = tempfile::tempdir()?;

    let mut client = McpClient::spawn(&exe, home.path())?;
    client.initialize()?;
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {"name": "run", "arguments": {"prompt": "hello there"}}
    }))?;
    let response = client.recv_response()?;
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .ok_or("run must answer with text content")?;
    assert!(text.contains("echo:"), "{text}");
    Ok(())
}

#[test]
#[ignore = "needs a real (or offline-echo) model provider: iface::http builds \
            its Harwness (the AppState backend) the same un-echoed way, so \
            POST /run would try to reach a network provider"]
fn test_http_post_run_answers_with_echo() -> TestResult {
    let (_root, artifact) = compile_worker("httprun")?;
    let out_dir = tempfile::tempdir()?;
    let exe = build_agent_binary(out_dir.path(), "httprun", &artifact)?;
    let home = tempfile::tempdir()?;
    let port = free_loopback_port()?;
    let addr = format!("127.0.0.1:{port}");

    let _guard = ChildGuard(
        Command::new(&exe)
            .args(["--interface", "http", "--listen", &addr])
            .env("HARW_HOME", home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?,
    );

    // A hand-rolled POST: same idea as `http_get`, with a JSON body and a
    // `Content-Length` header hyper's reader requires.
    let deadline = Instant::now() + IO_TIMEOUT;
    let mut stream = loop {
        match TcpStream::connect(&addr) {
            Ok(stream) => break stream,
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(Box::new(error));
                }
                thread::sleep(Duration::from_millis(30));
            }
        }
    };
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    let body = serde_json::to_string(&json!({"prompt": "hello there"}))?;
    let request = format!(
        "POST /run HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes())?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let text = String::from_utf8_lossy(&response).into_owned();
    let response_body = text
        .splitn(2, "\r\n\r\n")
        .nth(1)
        .ok_or("empty HTTP response")?;
    let payload: Value = serde_json::from_str(response_body)?;
    let text = payload["text"].as_str().ok_or("/run must return text")?;
    assert!(text.contains("echo:"), "{text}");
    Ok(())
}
