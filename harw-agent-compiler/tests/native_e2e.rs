//! A real native build (`harw agent build --native`, backend B): compile
//! `examples/agents/hello-analyst` with the compiler API, run
//! [`harw_agent_compiler::build`] with `native: true` against this checkout
//! (generated crate plus `cargo build --release`), then run the resulting
//! binary as a process.
//!
//! Ignored by default: the first run compiles harw's runner crates in
//! release mode and takes minutes. Run it with
//! `cargo test -p harw-agent-compiler --test native_e2e -- --ignored native`;
//! CI's `native-agent` job runs `scripts/native-e2e.sh`, the same checks
//! through the `harw` CLI.
//!
//! `NATIVE_E2E_HOME` (optional) names a persistent harw home, so the native
//! build cache (`<home>/cache/agent-builds/target`) is reused across runs.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use harw_agent_compiler::backend::BackendKind;
use harw_agent_compiler::{
    AgentInput, BuildOptions, Compiler, CompilerEnv, CompilerOptions, ProcessProbe, build,
};
use serde_json::Value;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// How long one invocation of the built agent may take.
const RUN_TIMEOUT: Duration = Duration::from_secs(120);

const AGENT_ID: &str = "harwness.example.hello-analyst@1";

/// A finished invocation of the built agent.
struct Finished {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs `exe args…` in `cwd` with `HARW_HOME=home` plus `envs`, stdin
/// closed, killing it after [`RUN_TIMEOUT`].
fn run(
    exe: &Path,
    args: &[&str],
    cwd: &Path,
    home: &Path,
    envs: &[(&str, &str)],
) -> Result<Finished, Box<dyn std::error::Error>> {
    let mut command = Command::new(exe);
    command
        .args(args)
        .current_dir(cwd)
        .env("HARW_HOME", home)
        .env_remove("HARW_OFFLINE_ECHO")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn()?;
    let mut stdout_pipe = child.stdout.take().ok_or("no stdout pipe")?;
    let mut stderr_pipe = child.stderr.take().ok_or("no stderr pipe")?;
    let stdout_reader = thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buffer);
        buffer
    });
    let stderr_reader = thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buffer);
        buffer
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > RUN_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("`{} {}` timed out", exe.display(), args.join(" ")).into());
        }
        thread::sleep(Duration::from_millis(20));
    };
    let stdout = stdout_reader.join().map_err(|_| "stdout reader panicked")?;
    let stderr = stderr_reader.join().map_err(|_| "stderr reader panicked")?;
    Ok(Finished {
        code: status.code(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

#[test]
#[ignore = "builds a native agent with cargo; run in CI native job"]
fn test_native_build_of_hello_analyst_verifies_and_answers() -> TestResult {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("the compiler crate has no parent directory")?
        .to_path_buf();
    let example = workspace
        .join("examples")
        .join("agents")
        .join("hello-analyst");
    assert!(
        example.join("definition.toml").is_file(),
        "missing {}",
        example.display()
    );

    let root = tempfile::tempdir()?;
    let home = std::env::var_os("NATIVE_E2E_HOME")
        .filter(|value| !value.is_empty())
        .map_or_else(|| root.path().join("home"), PathBuf::from);
    let cwd = root.path().join("cwd");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&cwd)?;

    // Isolated layers (only `home`), but the process's PATH/HOME so the
    // native backend finds cargo.
    let mut env = CompilerEnv::isolated(home.clone(), cwd.clone());
    env.path_var = std::env::var_os("PATH");
    env.user_home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);

    // The built-in definitions and the capability catalog, as `harw` installs
    // them at start.
    harw_registry_defaults::compiler_defaults::install();
    let mut compiler = Compiler::new(env.clone(), CompilerOptions::default())?;
    let compiled = compiler.compile_input(&AgentInput::Path(example))?;
    let digest = compiled.artifact.digest().to_string();

    let output = root.path().join("agent");
    let options = BuildOptions {
        native: true,
        harw_src: Some(workspace),
        output: Some(output.clone()),
        ..BuildOptions::default()
    };
    let mut progress = |line: &str| eprintln!("native-e2e: {line}");
    let report = build(&env, &compiled, &options, &ProcessProbe, &mut progress)?;
    assert_eq!(report.backend, BackendKind::Native);
    assert_eq!(report.artifact_digest, digest);
    assert!(report.native.is_some(), "no native output in the report");
    assert_eq!(report.output.as_deref(), Some(output.as_path()));
    assert!(output.is_file(), "no binary at {}", output.display());

    // --verify: the embedded artifact's hashes.
    let verify = run(&output, &["--verify"], &cwd, &home, &[])?;
    assert_eq!(verify.code, Some(0), "stderr: {}", verify.stderr);
    assert_eq!(verify.stdout.trim(), format!("ok: {digest}"));

    let verify_json = run(&output, &["--verify", "--json"], &cwd, &home, &[])?;
    assert_eq!(verify_json.code, Some(0), "stderr: {}", verify_json.stderr);
    let payload: Value = serde_json::from_str(verify_json.stdout.trim())?;
    assert_eq!(payload["ok"], Value::Bool(true), "{payload}");
    assert_eq!(
        payload["digest"].as_str(),
        Some(digest.as_str()),
        "{payload}"
    );

    // --manifest: the compiled rights, read-only.
    let manifest = run(&output, &["--manifest", "--json"], &cwd, &home, &[])?;
    assert_eq!(manifest.code, Some(0), "stderr: {}", manifest.stderr);
    let permissions: Value = serde_json::from_str(manifest.stdout.trim())?;
    let tools = permissions["tools"]
        .as_array()
        .ok_or("permissions.tools must be an array")?;
    assert!(tools.iter().any(|tool| tool == "fs.read"), "{permissions}");
    assert!(
        !tools.iter().any(|tool| tool == "fs.write"),
        "{permissions}"
    );
    assert_eq!(permissions["shell"], Value::Bool(false), "{permissions}");

    let manifest_text = run(&output, &["--manifest"], &cwd, &home, &[])?;
    assert_eq!(
        manifest_text.code,
        Some(0),
        "stderr: {}",
        manifest_text.stderr
    );
    assert!(
        manifest_text
            .stdout
            .contains(&format!("agent: {AGENT_ID} (hello-analyst)")),
        "{}",
        manifest_text.stdout
    );

    // --requirements --json: requirements, host report and admission.
    let requirements = run(&output, &["--requirements", "--json"], &cwd, &home, &[])?;
    assert_eq!(
        requirements.code,
        Some(0),
        "stderr: {}",
        requirements.stderr
    );
    let doc: Value = serde_json::from_str(requirements.stdout.trim())?;
    assert_eq!(doc["agent"], AGENT_ID, "{doc}");
    assert_eq!(doc["verdict"]["admitted"], true, "{doc}");

    // One-shot with the offline echo. A release build honors
    // HARW_OFFLINE_ECHO only with --offline-echo; [models].required_env
    // names ANTHROPIC_API_KEY, which the echo never uses but the runtime
    // checks is set.
    let reply = "native e2e offline echo";
    let answer = run(
        &output,
        &["--offline-echo", "What is in this folder?"],
        &cwd,
        &home,
        &[
            ("HARW_OFFLINE_ECHO", reply),
            ("ANTHROPIC_API_KEY", "native-e2e-not-a-key"),
        ],
    )?;
    assert_eq!(answer.code, Some(0), "stderr: {}", answer.stderr);
    assert!(answer.stdout.contains(reply), "{}", answer.stdout);
    assert!(
        answer
            .stderr
            .contains("offline echo: model calls are answered locally"),
        "{}",
        answer.stderr
    );
    Ok(())
}
