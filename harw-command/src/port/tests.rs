use super::*;
use crate::{CommandFrame, CommandOutput, CommandStdin};
#[cfg(target_os = "macos")]
use harw_job::DarwinExecutor as HostExecutor;
use harw_job::FsJobRecordStore;
#[cfg(target_os = "linux")]
use harw_job::LinuxExecutor as HostExecutor;

harw_test_support::define_test_error!(pub(crate));

type Port = JobCommandPort<FsJobRecordStore, HostExecutor>;

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
    // `/tmp` is a symlink on macOS (`/private/tmp`); the shell reports the real path.
    let tmp = std::fs::canonicalize("/tmp").map_err(ctx("canonicalize /tmp"))?;
    let expected = format!("s3cret-value\n{}\n", tmp.display());
    assert_eq!(outcome.stdout, expected.as_bytes(), "{outcome:?}");
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn is_zombie(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit(')')
            .next()
            .is_some_and(|rest| rest.trim_start().starts_with('Z'))
    })
}

#[tokio::test]
async fn a_started_command_writes_its_output_to_files_and_reports_its_pid() -> TestResult {
    let root = dir("start")?;
    let port = port(&root)?;
    let (out, err) = (root.join("out.log"), root.join("err.log"));
    let mut request = sh(
        "echo to-out; echo to-err >&2; exit 4",
        Duration::from_secs(20),
    );
    request.output = CommandOutput::Files {
        stdout: out.clone(),
        stderr: err.clone(),
    };
    let started = port.start(request).await.map_err(TestError::Unexpected)?;
    assert!(started.pid.is_some(), "{started:?}");
    let outcome = started.done.await;
    assert_eq!(outcome.end, CommandEnd::Exited, "{outcome:?}");
    assert_eq!(outcome.exit_code, 4);
    assert_eq!(std::fs::read(&out).map_err(ctx("out"))?, b"to-out\n");
    assert_eq!(std::fs::read(&err).map_err(ctx("err"))?, b"to-err\n");
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[tokio::test]
async fn piped_stdin_and_stdout_are_handed_over_and_stay_out_of_the_store() -> TestResult {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = dir("pipes")?;
    let port = port(&root)?;
    let mut request = sh(
        "read line; echo got:$line; echo to-err >&2",
        Duration::from_secs(20),
    );
    request.stdin = CommandStdin::Pipe;
    request.output = CommandOutput::StdoutPipe {
        stderr: root.join("err.log"),
    };
    let mut started = port.start(request).await.map_err(TestError::Unexpected)?;
    let mut stdin = started.stdin.take().ok_or(TestError::Missing("stdin"))?;
    let mut stdout = started.stdout.take().ok_or(TestError::Missing("stdout"))?;
    stdin
        .write_all(b"s3cret-password\n")
        .await
        .map_err(ctx("write"))?;
    drop(stdin);
    let mut text = String::new();
    stdout
        .read_to_string(&mut text)
        .await
        .map_err(ctx("read"))?;
    assert_eq!(text, "got:s3cret-password\n");
    let outcome = started.done.await;
    assert_eq!(outcome.end, CommandEnd::Exited, "{outcome:?}");
    assert_eq!(
        std::fs::read(root.join("err.log")).map_err(ctx("err"))?,
        b"to-err\n"
    );
    for entry in walk(&root)? {
        if entry.ends_with("err.log") {
            continue;
        }
        let bytes = std::fs::read(&entry).map_err(ctx("read"))?;
        assert!(
            !String::from_utf8_lossy(&bytes).contains("s3cret-password"),
            "{} keeps the secret",
            entry.display()
        );
    }
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[tokio::test]
async fn streamed_frames_and_cancel_work_on_a_started_command() -> TestResult {
    let root = dir("stream")?;
    let port = port(&root)?;
    let mut request = sh("echo first >&2; sleep 30", Duration::from_secs(60));
    request.stream = true;
    let mut started = port.start(request).await.map_err(TestError::Unexpected)?;
    let mut frames = started.frames.take().ok_or(TestError::Missing("frames"))?;
    let frame = tokio::time::timeout(Duration::from_secs(10), frames.next())
        .await
        .map_err(|_| TestError::Missing("a frame in time"))?;
    assert_eq!(frame, Some(CommandFrame::Stderr(b"first\n".to_vec())));
    (started.cancel)();
    let outcome = started.done.await;
    assert_eq!(outcome.end, CommandEnd::Cancelled, "{outcome:?}");
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
