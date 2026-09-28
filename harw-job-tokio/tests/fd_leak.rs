//! Many concurrent supervisors leave no file descriptor behind.
//!
//! This is its own test binary with a single test: the descriptor count of
//! `/proc/self/fd` is process-wide, so no other test may run concurrently.

#![cfg(target_os = "linux")]

use std::fmt;
use std::process::{Command, Stdio};
use std::time::Duration;

use harw_job_linux::LinuxProcess;
use harw_job_tokio::{AsyncLinuxProcess, ProcessEvent, Supervisor, SupervisorConfig};

/// Failure of this test; replaces `panic!`/`unwrap`/`expect`.
enum TestError {
    /// A foreign error with context.
    Context {
        context: &'static str,
        source: String,
    },
    /// A result had an unexpected shape.
    Unexpected(String),
}

type TestResult<T = ()> = Result<T, TestError>;

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Context { context, source } => write!(f, "{context}: {source}"),
            Self::Unexpected(message) => write!(f, "unexpected result: {message}"),
        }
    }
}

impl fmt::Debug for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |error| TestError::Context {
        context,
        source: error.to_string(),
    }
}

const SUPERVISORS: usize = 50;

fn open_fds() -> TestResult<usize> {
    Ok(std::fs::read_dir("/proc/self/fd")
        .map_err(ctx("read /proc/self/fd"))?
        .count())
}

/// Runs one supervised job (every other one is cancelled) to completion.
async fn run_one(index: usize) -> TestResult {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(if index % 2 == 0 {
            "echo out; echo err >&2; exit 3"
        } else {
            "echo ready; exec sleep 30"
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (process, stdio) = LinuxProcess::spawn(&mut command).map_err(ctx("spawn"))?;
    let adopted = AsyncLinuxProcess::new(process).map_err(ctx("adopt"))?;
    let (handle, mut events) = Supervisor::spawn(
        adopted,
        stdio.stdout,
        stdio.stderr,
        SupervisorConfig::default(),
    );
    let cancel_on_output = index % 2 == 1;
    let mut last = None;
    while let Some(event) = events.recv().await {
        if cancel_on_output && matches!(event, ProcessEvent::Stdout(_)) {
            handle.cancel();
        }
        last = Some(event);
    }
    let process = handle.join().await.map_err(ctx("join"))?;
    drop(process);
    match last {
        Some(ProcessEvent::Exited(_)) => Ok(()),
        other => Err(TestError::Unexpected(format!("last event {other:?}"))),
    }
}

#[tokio::test]
async fn test_many_concurrent_supervisors_leak_no_fds() -> TestResult {
    // Warm-up: lets the runtime create its lazily opened descriptors.
    run_one(0).await?;
    let before = open_fds()?;
    let mut tasks = Vec::with_capacity(SUPERVISORS);
    for index in 0..SUPERVISORS {
        tasks.push(tokio::spawn(run_one(index)));
    }
    for task in tasks {
        tokio::time::timeout(Duration::from_secs(60), task)
            .await
            .map_err(ctx("supervisor timeout"))?
            .map_err(ctx("supervisor task"))??;
    }
    let after = open_fds()?;
    if after > before {
        return Err(TestError::Unexpected(format!(
            "descriptor leak: {before} open before, {after} after {SUPERVISORS} supervisors"
        )));
    }
    Ok(())
}
