//! Opt-in Linux system tests with owned child-process fixtures.
//!
//! Each test owns and reaps its children. No test signals a discovered arbitrary
//! process. External process/procfs access is ignored by default under R183.

use std::{
    io::{self, BufRead, BufReader},
    os::unix::process::ExitStatusExt,
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

// Different markers isolate concurrently running selector tests.
static NEXT_MARKER: AtomicU64 = AtomicU64::new(0);

// Own the child immediately so failure paths also reap the fixture.
struct Target(Child);

// Cleanup never panics during another assertion's unwind.
impl Drop for Target {
    fn drop(&mut self) {
        // Intentionally ignore kill failure: a test may already have reaped it.
        let _ = self.0.kill();
        // Intentionally ignore wait failure: cleanup cannot recover or double-panic.
        let _ = self.0.wait();
    }
}

// Spawn only a test-owned sleeper; readiness follows signal-handler installation.
fn target(ignore_term: bool, marker: &str) -> io::Result<Target> {
    let code = if ignore_term {
        "import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); print('ready',flush=True); time.sleep(60)"
    } else {
        "import time; print('ready',flush=True); time.sleep(60)"
    };
    let mut child = Target(
        Command::new("python3")
            .args(["-c", code, marker])
            .stdout(Stdio::piped())
            .spawn()?,
    );
    let Some(stdout) = child.0.stdout.take() else {
        return Err(io::Error::other("fixture has no readiness pipe"));
    };
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line)?;
    assert_eq!(
        line.trim(),
        "ready",
        "fixture must install handler before signalling"
    );
    Ok(child)
}

// Identify this fixture independently of other Python processes on the machine.
fn marker() -> String {
    format!(
        "killer_fixture_{}_{}",
        std::process::id(),
        NEXT_MARKER.fetch_add(1, Ordering::Relaxed)
    )
}

// Null stdin deliberately exercises the CLI's noninteractive contract.
fn cli(args: &[&str]) -> io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_killer"))
        .args(args)
        .stdin(Stdio::null())
        .output()
}

// Report malformed output as a test error rather than unwrapping JSON parsing.
fn report(output: &Output) -> io::Result<serde_json::Value> {
    match serde_json::from_slice(&output.stdout) {
        Ok(value) => Ok(value),
        Err(error) => Err(io::Error::other(format!(
            "invalid CLI JSON: {error}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ))),
    }
}

// Verify that inspection neither signals nor mutates a selected child.
#[test]
#[ignore = "requires Linux procfs/pidfd and python3; run make test-system"]
fn test_cli_dry_run_leaves_target_alive() -> io::Result<()> {
    let mut p = target(false, &marker())?;
    let out = cli(&["--pid", &p.0.id().to_string(), "--dry-run", "--json"])?;
    assert!(out.status.success(), "{out:?}");
    let json = report(&out)?;
    assert_eq!(json["targets"][0]["pid"], p.0.id());
    assert!(p.0.try_wait()?.is_none());
    Ok(())
}

// Assert the first signal is the explicitly requested KILL.
#[test]
#[ignore = "requires Linux procfs/pidfd and python3; run make test-system"]
fn test_cli_first_signal_is_kill() -> io::Result<()> {
    let mut p = target(false, &marker())?;
    let out = cli(&["--pid", &p.0.id().to_string(), "--yes", "--json"])?;
    assert!(out.status.success(), "{out:?}");
    assert_eq!(report(&out)?["results"][0]["outcome"], "killed");
    assert_eq!(
        p.0.wait()?.signal(),
        Some(rustix::process::Signal::KILL.as_raw())
    );
    Ok(())
}

// TERM handlers cannot change the mandatory initial KILL behavior.
#[test]
#[ignore = "requires Linux procfs/pidfd and python3; run make test-system"]
fn test_cli_term_handler_does_not_affect_kill() -> io::Result<()> {
    let mut p = target(true, &marker())?;
    let out = cli(&[
        "--pid",
        &p.0.id().to_string(),
        "--yes",
        "--timeout",
        "0.1",
        "--json",
    ])?;
    assert!(out.status.success(), "{out:?}");
    assert_eq!(report(&out)?["results"][0]["outcome"], "killed");
    assert_eq!(
        p.0.wait()?.signal(),
        Some(rustix::process::Signal::KILL.as_raw())
    );
    Ok(())
}

// A noninteractive invocation cannot silently confirm a destructive action.
#[test]
#[ignore = "requires Linux procfs/pidfd and python3; run make test-system"]
fn test_cli_noninteractive_requires_yes() -> io::Result<()> {
    let mut p = target(false, &marker())?;
    let out = cli(&["--pid", &p.0.id().to_string()])?;
    assert_eq!(out.status.code(), Some(1));
    assert!(p.0.try_wait()?.is_none());
    Ok(())
}

// Read-only selection verifies protected ancestry without risking the test runner.
#[test]
#[ignore = "requires Linux procfs/pidfd and python3; run make test-system"]
fn test_cli_protects_parent_and_init() -> io::Result<()> {
    for pid in [1, std::process::id()] {
        assert_eq!(
            cli(&["--pid", &pid.to_string(), "--dry-run"])?
                .status
                .code(),
            Some(2)
        );
    }
    Ok(())
}

// Exercise parser failures at the binary boundary in addition to pure unit tests.
#[test]
#[ignore = "requires Linux procfs/pidfd and python3; run make test-system"]
fn test_cli_rejects_invalid_selectors_and_timeout() -> io::Result<()> {
    for args in [
        vec!["--pid", "0"],
        vec!["--pid", "-1"],
        vec!["--pid", "42", "--timeout", "NaN"],
        vec!["--pid", "42", "--timeout", "inf"],
        vec!["cargo", "--pid", "42"],
        vec!["[", "--regex"],
        vec![],
    ] {
        assert!(!cli(&args)?.status.success(), "{args:?}");
    }
    Ok(())
}

// A reaped PID is no longer selectable; no destructive request is needed.
#[test]
#[ignore = "requires Linux procfs/pidfd and python3; run make test-system"]
fn test_cli_vanished_pid_has_no_match() -> io::Result<()> {
    let mut p = target(false, &marker())?;
    let pid = p.0.id();
    p.0.kill()?;
    p.0.wait()?;
    assert_eq!(
        cli(&["--pid", &pid.to_string(), "--dry-run"])?
            .status
            .code(),
        Some(2)
    );
    Ok(())
}

// Multi-name selection is exact, and combines with IDs without duplicates.
#[test]
#[ignore = "requires Linux procfs/pidfd and python3; run make test-system"]
fn test_cli_multiple_names_and_pid_union() -> io::Result<()> {
    let mut child = target(false, &marker())?;
    let pid = child.0.id().to_string();
    let out = cli(&[
        "-p",
        "killer_nonexistent_one",
        "killer_nonexistent_two",
        "--pid",
        &pid,
        &pid,
        "-n",
        "--json",
    ])?;
    assert!(out.status.success(), "{out:?}");
    let value = report(&out)?;
    assert_eq!(value["targets"].as_array().map(Vec::len), Some(1));
    assert_eq!(value["targets"][0]["pid"], child.0.id());
    let executable = std::fs::read_link(format!("/proc/{pid}/exe"))?;
    let name = executable
        .file_name()
        .ok_or_else(|| io::Error::other("missing executable basename"))?
        .to_string_lossy();
    let out = cli(&[
        "-p",
        &name,
        "killer_nonexistent_two",
        "-p",
        &name,
        "--pid",
        &pid,
        "-n",
        "--json",
    ])?;
    assert!(out.status.success(), "{out:?}");
    let value = report(&out)?;
    let targets = value["targets"]
        .as_array()
        .ok_or_else(|| io::Error::other("missing targets array"))?;
    assert_eq!(
        targets.iter().filter(|t| t["pid"] == child.0.id()).count(),
        1
    );
    assert!(child.0.try_wait()?.is_none());
    Ok(())
}

// Repeated PID flags denote one target and must not duplicate signals/results.
#[test]
#[ignore = "requires Linux procfs/pidfd and python3; run make test-system"]
fn test_cli_duplicate_pid_is_deduplicated() -> io::Result<()> {
    let mut p = target(false, &marker())?;
    let pid = p.0.id().to_string();
    let out = cli(&["--pid", &pid, "--pid", &pid, "--dry-run", "--json"])?;
    assert!(out.status.success(), "{out:?}");
    assert_eq!(report(&out)?["targets"].as_array().map(Vec::len), Some(1));
    assert!(p.0.try_wait()?.is_none());
    Ok(())
}

// UID filtering also applies when the selector is an explicit PID.
#[test]
#[ignore = "requires Linux procfs/pidfd and python3; run make test-system"]
fn test_cli_uid_mismatch_has_no_match() -> io::Result<()> {
    let mut p = target(false, &marker())?;
    let other_uid = rustix::process::geteuid()
        .as_raw()
        .wrapping_add(1)
        .to_string();
    let out = cli(&[
        "--pid",
        &p.0.id().to_string(),
        "--uid",
        &other_uid,
        "--dry-run",
    ])?;
    assert_eq!(out.status.code(), Some(2));
    assert!(p.0.try_wait()?.is_none());
    Ok(())
}
