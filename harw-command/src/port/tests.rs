use super::*;
use harw_job::{FsJobRecordStore, LinuxExecutor};

harw_test_support::define_test_error!(pub(crate));

type Port = JobCommandPort<FsJobRecordStore, LinuxExecutor>;

fn port(dir: &std::path::Path) -> TestResult<Port> {
    JobCommandPort::host(dir).map_err(TestError::Unexpected)
}

fn sh(script: &str, timeout: Duration) -> CommandRequest {
    let mut request = CommandRequest::new("/bin/sh", "/tmp", timeout);
    request.args = vec!["-c".into(), script.into()];
    request.env = vec![("PATH".into(), "/usr/bin:/bin".into())];
    request
}

fn dir(tag: &str) -> TestResult<std::path::PathBuf> {
    harw_test_support::unique_tmp_created("harw-command", tag).map_err(ctx("tmp"))
}

fn walk(root: &std::path::Path) -> TestResult<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).map_err(ctx("read_dir"))? {
            let path = entry.map_err(ctx("entry"))?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    Ok(files)
}

#[tokio::test]
async fn output_and_exit_code_come_through_the_runtime() -> TestResult {
    let root = dir("ok")?;
    let outcome = port(&root)?
        .run(
            sh("echo out; echo err >&2; exit 3", Duration::from_secs(20)),
            CancelToken::new(),
        )
        .await;
    assert_eq!(outcome.end, CommandEnd::Exited, "{outcome:?}");
    assert_eq!(outcome.exit_code, 3);
    assert_eq!(outcome.stdout, b"out\n");
    assert_eq!(outcome.stderr, b"err\n");
    assert_eq!(outcome.combined.len(), 8);
    assert!(!outcome.truncated);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[tokio::test]
async fn the_environment_is_exactly_the_request_and_never_persisted() -> TestResult {
    let root = dir("env")?;
    let mut request = sh("echo \"$HARW_SECRET\"; pwd", Duration::from_secs(20));
    request
        .env
        .push(("HARW_SECRET".into(), "s3cret-value".into()));
    let outcome = port(&root)?.run(request, CancelToken::new()).await;
    assert_eq!(outcome.stdout, b"s3cret-value\n/tmp\n", "{outcome:?}");
    for entry in walk(&root)? {
        let text = std::fs::read(&entry).map_err(ctx("read"))?;
        assert!(
            !String::from_utf8_lossy(&text).contains("s3cret-value"),
            "{} keeps the secret",
            entry.display()
        );
    }
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[tokio::test]
async fn the_deadline_kills_the_tree_and_keeps_partial_output() -> TestResult {
    let root = dir("deadline")?;
    let outcome = port(&root)?
        .run(
            sh("echo before; sleep 30", Duration::from_secs(1)),
            CancelToken::new(),
        )
        .await;
    assert_eq!(outcome.end, CommandEnd::TimedOut, "{outcome:?}");
    assert_eq!(outcome.stdout, b"before\n");
    assert!(outcome.elapsed < Duration::from_secs(15));
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[tokio::test]
async fn the_output_budget_stops_the_job() -> TestResult {
    let root = dir("budget")?;
    let mut request = sh("yes x", Duration::from_secs(20));
    request.max_output_bytes = 4096;
    let outcome = port(&root)?.run(request, CancelToken::new()).await;
    assert_eq!(outcome.end, CommandEnd::OutputLimit, "{outcome:?}");
    assert!(outcome.truncated);
    assert!(outcome.combined.len() <= 4096);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[tokio::test]
async fn cancelling_the_token_ends_the_job() -> TestResult {
    let root = dir("cancel")?;
    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        trigger.cancel(harw_types::cancel::CancelReason::User);
    });
    let outcome = port(&root)?
        .run(sh("echo up; sleep 30", Duration::from_secs(60)), cancel)
        .await;
    assert_eq!(outcome.end, CommandEnd::Cancelled, "{outcome:?}");
    assert!(outcome.elapsed < Duration::from_secs(15));
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[tokio::test]
async fn a_relative_directory_and_a_missing_program_fail_cleanly() -> TestResult {
    let root = dir("fail")?;
    let port = port(&root)?;
    let mut relative = sh("true", Duration::from_secs(5));
    relative.cwd = "relative/dir".into();
    let outcome = port.run(relative, CancelToken::new()).await;
    assert!(matches!(outcome.end, CommandEnd::Failed(_)), "{outcome:?}");
    let missing = CommandRequest::new("/nonexistent/program", "/tmp", Duration::from_secs(5));
    let outcome = port.run(missing, CancelToken::new()).await;
    // Through setsid the failure shows as exit 127 (as a shell reports it).
    assert!(
        matches!(outcome.end, CommandEnd::Failed(_)) || outcome.exit_code == 127,
        "{outcome:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[tokio::test]
async fn the_job_has_no_controlling_terminal_and_its_tree_dies_with_it() -> TestResult {
    let root = dir("session")?;
    let port = port(&root)?;
    // A session without controlling terminal: opening /dev/tty must fail.
    let outcome = port
        .run(
            sh(
                "(echo x > /dev/tty) 2>/dev/null && echo tty || echo notty",
                Duration::from_secs(20),
            ),
            CancelToken::new(),
        )
        .await;
    assert_eq!(outcome.stdout, b"notty\n", "{outcome:?}");
    // A grandchild must die with the deadline kill (no orphan in its own session).
    let marker = root.join("grandchild.pid");
    let script = format!("sleep 60 & echo $! > {}; wait", marker.display());
    let outcome = port
        .run(sh(&script, Duration::from_secs(1)), CancelToken::new())
        .await;
    assert_eq!(outcome.end, CommandEnd::TimedOut, "{outcome:?}");
    let pid: i32 = std::fs::read_to_string(&marker)
        .map_err(ctx("marker"))?
        .trim()
        .parse()
        .map_err(|_| TestError::Missing("grandchild pid"))?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists() || is_zombie(pid),
        "grandchild {pid} survived"
    );
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

fn is_zombie(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit(')')
            .next()
            .is_some_and(|rest| rest.trim_start().starts_with('Z'))
    })
}
