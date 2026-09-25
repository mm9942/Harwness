//! Tests der Job-Verwaltung mit echten, kurzen Prozessen (`/bin/sh -c`).

use super::*;
use crate::event::JobEvent;
use crate::logs::{LogQuery, read_log};
use crate::procfs::is_same_process_alive;
use crate::test_support::{Env, TestError, TestResult, ctx, eventually, request};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const LIMIT: Duration = Duration::from_secs(10);

fn events_of(env: &Env, id: &JobId) -> Vec<JobEvent> {
    env.recorder
        .notifications()
        .into_iter()
        .map(|notification| notification.event)
        .filter(|event| event.job_id() == id)
        .collect()
}

fn read_all(path: &Path) -> TestResult<Vec<String>> {
    let slice = read_log(
        path,
        &LogQuery {
            max_lines: 1000,
            max_bytes: 1024 * 1024,
            ..LogQuery::default()
        },
    )
    .map_err(ctx("read log"))?;
    Ok(slice.lines.into_iter().map(|(_, text)| text).collect())
}

#[tokio::test]
async fn test_start_status_logs_stop() -> TestResult {
    let env = Env::new()?;
    let prepared = env.prepare("echo hello; echo oops >&2; sleep 30").await?;
    let started = env
        .manager
        .start(request("sleeper", "agent-a", &[]), prepared)
        .map_err(ctx("start"))?;
    let id = started.meta.job_id.clone();
    assert_eq!(started.meta.state, JobState::Running);
    assert!(started.meta.pid.is_some());
    assert!(started.log_dir.join(META_FILE).is_file());

    let caller = Caller::Agent("agent-a");
    let seen = eventually(LIMIT, || {
        env.manager
            .status(&id, caller)
            .is_ok_and(|status| status.stdout_lines >= 1 && status.stderr_lines >= 1)
    })
    .await;
    assert!(seen, "output lines were not observed");

    let status = env.manager.status(&id, caller).map_err(ctx("status"))?;
    assert_eq!(status.meta.state, JobState::Running);
    assert!(status.runtime_secs.is_some());
    assert!(status.last_lines.iter().any(|line| line == "hello"));

    let dir = env.manager.log_dir(&id, caller).map_err(ctx("log dir"))?;
    assert_eq!(read_all(&dir.join(STDOUT_LOG))?, vec!["hello".to_owned()]);
    assert_eq!(read_all(&dir.join(STDERR_LOG))?, vec!["oops".to_owned()]);

    let stopped = env
        .manager
        .stop(&id, caller, JobSignal::Term)
        .await
        .map_err(ctx("stop"))?;
    assert_eq!(stopped.meta.state, JobState::Stopped);
    assert!(stopped.meta.stop_requested);
    assert!(stopped.meta.ended_at.is_some());

    let on_disk = read_meta(&dir).ok_or(TestError::Missing("meta.json after stop"))?;
    assert_eq!(on_disk.state, JobState::Stopped);

    let events = events_of(&env, &id);
    assert!(matches!(events.first(), Some(JobEvent::Started { .. })));
    assert!(matches!(
        events.last(),
        Some(JobEvent::Finished {
            state: JobState::Stopped,
            ..
        })
    ));
    Ok(())
}

#[tokio::test]
async fn test_stop_kills_whole_process_group() -> TestResult {
    let env = Env::new()?;
    // Das Kind ignoriert SIGTERM und überlebt die Shell — nur ein Signal an
    // die ganze Gruppe (bzw. das Aufräumen verbliebener Mitglieder) trifft es.
    let prepared = env
        .prepare("(trap '' TERM; exec sleep 30) & echo $!; wait")
        .await?;
    let started = env
        .manager
        .start(request("group", "agent-a", &[]), prepared)
        .map_err(ctx("start"))?;
    let id = started.meta.job_id.clone();
    let stdout = started.log_dir.join(STDOUT_LOG);

    let mut child_pid = None;
    let found = eventually(LIMIT, || {
        child_pid = read_all(&stdout).ok().and_then(|lines| {
            lines
                .first()
                .and_then(|line| line.trim().parse::<u32>().ok())
        });
        child_pid.is_some()
    })
    .await;
    let child_pid = child_pid
        .filter(|_| found)
        .ok_or(TestError::Missing("child pid"))?;
    assert!(is_same_process_alive(child_pid, None));

    let stopped = env
        .manager
        .stop(&id, Caller::Agent("agent-a"), JobSignal::Term)
        .await
        .map_err(ctx("stop"))?;
    assert_eq!(stopped.meta.state, JobState::Stopped);

    let dead = eventually(LIMIT, || !is_same_process_alive(child_pid, None)).await;
    assert!(dead, "child {child_pid} of the job survived job.stop");
    Ok(())
}

#[tokio::test]
async fn test_exit_code_and_tail_in_finished_event() -> TestResult {
    let env = Env::new()?;
    let prepared = env.prepare("echo a; echo b; sleep 0.2; exit 3").await?;
    let started = env
        .manager
        .start(request("fails", "agent-a", &[]), prepared)
        .map_err(ctx("start"))?;
    let id = started.meta.job_id.clone();

    let (outcome, status) = env
        .manager
        .wait(&id, Caller::Agent("agent-a"), LIMIT, None)
        .await
        .map_err(ctx("wait"))?;
    assert_eq!(outcome, WaitOutcome::Finished);
    assert_eq!(status.meta.state, JobState::Failed);
    assert_eq!(status.meta.exit_code, Some(3));

    let events = events_of(&env, &id);
    let finished = events
        .iter()
        .find(|event| event.is_finished())
        .ok_or(TestError::Missing("finished event"))?;
    match finished {
        JobEvent::Finished {
            state,
            exit_code,
            tail,
            ..
        } => {
            assert_eq!(*state, JobState::Failed);
            assert_eq!(*exit_code, Some(3));
            assert_eq!(tail, &vec!["a".to_owned(), "b".to_owned()]);
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert!(finished.render_note().contains("exit code 3"));
    Ok(())
}

#[tokio::test]
async fn test_successful_job_is_succeeded() -> TestResult {
    let env = Env::new()?;
    let prepared = env.prepare("echo done").await?;
    let id = env
        .manager
        .start(request("ok", "agent-a", &[]), prepared)
        .map_err(ctx("start"))?
        .meta
        .job_id;
    let (outcome, status) = env
        .manager
        .wait(&id, Caller::Agent("agent-a"), LIMIT, None)
        .await
        .map_err(ctx("wait"))?;
    assert_eq!(outcome, WaitOutcome::Finished);
    assert_eq!(status.meta.state, JobState::Succeeded);
    assert_eq!(status.meta.exit_code, Some(0));
    Ok(())
}

#[tokio::test]
async fn test_only_creator_and_ancestors_control_a_job() -> TestResult {
    let env = Env::new()?;
    let prepared = env.prepare("sleep 30").await?;
    let id = env
        .manager
        .start(
            request("owned", "worker", &["orchestrator", "uia"]),
            prepared,
        )
        .map_err(ctx("start"))?
        .meta
        .job_id;

    assert!(env.manager.status(&id, Caller::Agent("worker")).is_ok());
    assert!(
        env.manager
            .status(&id, Caller::Agent("orchestrator"))
            .is_ok()
    );
    assert!(env.manager.status(&id, Caller::Agent("uia")).is_ok());
    assert!(matches!(
        env.manager.status(&id, Caller::Agent("sibling")),
        Err(JobError::NotFound(_))
    ));
    assert!(env.manager.list(Caller::Agent("sibling")).is_empty());
    assert_eq!(env.manager.list(Caller::Agent("uia")).len(), 1);
    assert!(matches!(
        env.manager
            .stop(&id, Caller::Agent("sibling"), JobSignal::Kill)
            .await,
        Err(JobError::NotFound(_))
    ));
    assert_eq!(
        env.manager
            .status(&id, Caller::Operator)
            .map(|s| s.meta.state)
            .ok(),
        Some(JobState::Running)
    );

    let stopped = env
        .manager
        .stop(&id, Caller::Agent("orchestrator"), JobSignal::Kill)
        .await
        .map_err(ctx("stop by ancestor"))?;
    assert_eq!(stopped.meta.state, JobState::Stopped);
    Ok(())
}

#[tokio::test]
async fn test_progress_and_error_events() -> TestResult {
    let env = Env::new()?;
    let prepared = env
        .prepare("sleep 0.2; printf '[1/4] a\\n[2/4] b\\n'; echo 'error: boom' >&2; sleep 0.6")
        .await?;
    let id = env
        .manager
        .start(request("build", "agent-a", &[]), prepared)
        .map_err(ctx("start"))?
        .meta
        .job_id;
    let (outcome, status) = env
        .manager
        .wait(&id, Caller::Agent("agent-a"), LIMIT, None)
        .await
        .map_err(ctx("wait"))?;
    // Der erste Meilenstein kommt vor dem Ende.
    assert_eq!(outcome, WaitOutcome::Milestone);
    assert!(!status.meta.state.is_terminal());

    let finished = eventually(LIMIT, || {
        env.manager
            .status(&id, Caller::Agent("agent-a"))
            .is_ok_and(|status| status.meta.state.is_terminal())
    })
    .await;
    assert!(finished);
    let status = env
        .manager
        .status(&id, Caller::Agent("agent-a"))
        .map_err(ctx("status"))?;
    let progress = status.meta.progress.ok_or(TestError::Missing("progress"))?;
    assert_eq!(progress.source, ProgressSource::Ninja);
    assert_eq!(
        (progress.done, progress.total, progress.percent),
        (Some(2), Some(4), Some(50))
    );
    assert_eq!(status.meta.errors, 1);

    let events = events_of(&env, &id);
    assert!(events.iter().any(|event| matches!(
        event,
        JobEvent::ErrorLines { lines, .. } if lines == &vec!["error: boom".to_owned()]
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        JobEvent::Progress { progress: Some(p), .. } if p.percent == Some(50)
    )));
    Ok(())
}

#[tokio::test]
async fn test_wait_times_out_then_sees_milestone() -> TestResult {
    let env = Env::new()?;
    let prepared = env
        .prepare("sleep 0.5; echo '[5/10] half'; sleep 30")
        .await?;
    let id = env
        .manager
        .start(request("slow", "agent-a", &[]), prepared)
        .map_err(ctx("start"))?
        .meta
        .job_id;
    let caller = Caller::Agent("agent-a");
    let (outcome, _) = env
        .manager
        .wait(&id, caller, Duration::from_millis(100), None)
        .await
        .map_err(ctx("short wait"))?;
    assert_eq!(outcome, WaitOutcome::Timeout);
    let (outcome, status) = env
        .manager
        .wait(&id, caller, LIMIT, None)
        .await
        .map_err(ctx("long wait"))?;
    assert_eq!(outcome, WaitOutcome::Milestone);
    assert_eq!(status.meta.progress.and_then(|p| p.percent), Some(50));

    let cancel = CancelToken::new();
    cancel.cancel(harw_types::cancel::CancelReason::User);
    let (outcome, _) = env
        .manager
        .wait(&id, caller, LIMIT, Some(&cancel))
        .await
        .map_err(ctx("cancelled wait"))?;
    assert_eq!(outcome, WaitOutcome::Cancelled);

    env.manager
        .stop(&id, caller, JobSignal::Kill)
        .await
        .map_err(ctx("stop"))?;
    Ok(())
}

#[tokio::test]
async fn test_detach_keeps_process_and_operator_can_stop_it() -> TestResult {
    let env = Env::new()?;
    let prepared = env.prepare("sleep 30").await?;
    let started = env
        .manager
        .start(request("detached", "agent-a", &[]), prepared)
        .map_err(ctx("start"))?;
    let id = started.meta.job_id.clone();
    let pid = started.meta.pid.ok_or(TestError::Missing("pid"))?;

    let summary = env.manager.detach_all();
    assert_eq!(summary.detached, 1);
    assert_eq!(summary.ends_with_harw, 0);
    assert_eq!(env.manager.running_count(), 0);
    let status = env
        .manager
        .status(&id, Caller::Operator)
        .map_err(ctx("status"))?;
    assert_eq!(status.meta.state, JobState::Detached);
    assert!(status.meta.detached);
    assert!(is_same_process_alive(pid, status.meta.proc_start_ticks));

    let stopped = env
        .manager
        .stop(&id, Caller::Operator, JobSignal::Term)
        .await
        .map_err(ctx("stop"))?;
    assert_eq!(stopped.meta.state, JobState::Stopped);
    assert!(!is_same_process_alive(pid, stopped.meta.proc_start_ticks));
    Ok(())
}

fn previous_meta(id: &str, pid: u32, ticks: Option<u64>) -> TestResult<JobMeta> {
    Ok(JobMeta {
        version: META_VERSION,
        job_id: JobId::parse(id).ok_or(TestError::Missing("job id"))?,
        name: id.to_owned(),
        command: "sleep 1000".to_owned(),
        cwd: None,
        env_keys: Vec::new(),
        state: JobState::Running,
        pid: Some(pid),
        proc_start_ticks: ticks,
        executed_on_host: true,
        harw_instance: "harw-old".to_owned(),
        owner: JobOwner::new("old-session", Vec::new()),
        created_at: Timestamp::now(),
        started_at: Some(Timestamp::now()),
        ended_at: None,
        exit_code: None,
        signal: None,
        stop_requested: false,
        detached: false,
        notify_every_secs: 60,
        progress: None,
        warnings: 0,
        errors: 0,
        launch_error: None,
    })
}

#[tokio::test]
async fn test_reload_marks_previous_jobs_detached_or_unknown() -> TestResult {
    let env = Env::new()?;
    let jobs_dir = env.state_dir().join("jobs");
    let own_pid = std::process::id();
    let own_ticks = process_start_ticks(own_pid);
    for meta in [
        previous_meta("job-old-alive", own_pid, own_ticks)?,
        previous_meta("job-old-gone", u32::MAX / 2, Some(1))?,
        // Gleiche PID, aber andere Startzeit: PID wurde wiederverwendet.
        previous_meta("job-old-reused", own_pid, own_ticks.map(|t| t + 1))?,
    ] {
        let dir = jobs_dir.join(meta.job_id.as_str());
        fs::create_dir_all(&dir).map_err(ctx("create old job dir"))?;
        write_meta(&dir, &meta).map_err(ctx("write old meta"))?;
    }

    let reloaded = JobManager::new(env.manager.config().clone(), Arc::new(crate::NoopNotifier))
        .map_err(ctx("reload"))?;
    let states: BTreeMap<String, JobState> = reloaded
        .list(Caller::Operator)
        .into_iter()
        .map(|status| (status.meta.job_id.to_string(), status.meta.state))
        .collect();
    assert_eq!(states.get("job-old-alive"), Some(&JobState::Detached));
    assert_eq!(states.get("job-old-gone"), Some(&JobState::Unknown));
    assert_eq!(states.get("job-old-reused"), Some(&JobState::Unknown));
    // Die alte Sitzung besitzt sie; ein neuer Agent sieht sie nicht.
    assert!(reloaded.list(Caller::Agent("new-session")).is_empty());

    let on_disk = read_meta(&jobs_dir.join("job-old-gone")).ok_or(TestError::Missing("meta"))?;
    assert_eq!(on_disk.state, JobState::Unknown);
    Ok(())
}

#[tokio::test]
async fn test_capacity_limit() -> TestResult {
    let env = Env::with_config(|config| JobManagerConfig {
        max_running_jobs: 1,
        ..config
    })?;
    let first = env.prepare("sleep 30").await?;
    let id = env
        .manager
        .start(request("one", "agent-a", &[]), first)
        .map_err(ctx("start"))?
        .meta
        .job_id;
    assert!(matches!(
        env.manager.check_capacity(),
        Err(JobError::Capacity { max: 1 })
    ));
    let second = env.prepare("sleep 30").await?;
    assert!(matches!(
        env.manager.start(request("two", "agent-a", &[]), second),
        Err(JobError::Capacity { max: 1 })
    ));
    assert_eq!(env.manager.stop_all().await, 1);
    assert_eq!(
        env.manager
            .status(&id, Caller::Operator)
            .map(|s| s.meta.state)
            .ok(),
        Some(JobState::Stopped)
    );
    Ok(())
}

#[tokio::test]
async fn test_spawn_failure_is_recorded_as_failed() -> TestResult {
    let env = Env::new()?;
    let mut command = tokio::process::Command::new("/nonexistent/harw-job-binary");
    command.stdin(Stdio::null());
    let prepared = PreparedJob {
        command,
        executed_on_host: true,
    };
    let result = env
        .manager
        .start(request("broken", "agent-a", &[]), prepared);
    assert!(matches!(result, Err(JobError::Spawn(_))));
    let jobs = env.manager.list(Caller::Agent("agent-a"));
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].meta.state, JobState::Failed);
    assert!(jobs[0].meta.launch_error.is_some());
    Ok(())
}

#[tokio::test]
async fn test_start_piped_echoes_stdin_tees_stdout_and_detects_exit() -> TestResult {
    let env = Env::new()?;
    let prepared = env.prepare_piped(&["/bin/cat"]).await?;
    let mut piped = env
        .manager
        .start_piped(request("echo", "agent-a", &[]), prepared)
        .map_err(ctx("start_piped"))?;
    let id = piped.job_id.clone();
    assert_eq!(piped.status.meta.state, JobState::Running);

    piped
        .stdin
        .write_all(b"hello\n")
        .await
        .map_err(ctx("write stdin"))?;
    piped.stdin.flush().await.map_err(ctx("flush stdin"))?;

    let line = tokio::time::timeout(LIMIT, piped.stdout_lines.recv())
        .await
        .map_err(ctx("recv timeout"))?
        .ok_or(TestError::Missing("stdout line"))?
        .map_err(ctx("stdout line"))?;
    assert_eq!(line, "hello");

    // `cat` sees EOF on stdin and exits 0.
    drop(piped.stdin);

    let (outcome, status) = env
        .manager
        .wait(&id, Caller::Agent("agent-a"), LIMIT, None)
        .await
        .map_err(ctx("wait"))?;
    assert_eq!(outcome, WaitOutcome::Finished);
    assert_eq!(status.meta.state, JobState::Succeeded);
    assert_eq!(status.meta.exit_code, Some(0));

    let dir = env
        .manager
        .log_dir(&id, Caller::Agent("agent-a"))
        .map_err(ctx("log dir"))?;
    assert_eq!(read_all(&dir.join(STDOUT_LOG))?, vec!["hello".to_owned()]);
    Ok(())
}

#[tokio::test]
async fn test_start_piped_stop_kills_process_group() -> TestResult {
    let env = Env::new()?;
    // Wie `test_stop_kills_whole_process_group`: das Kind ignoriert SIGTERM
    // und überlebt die Shell — nur ein Signal an die ganze Gruppe trifft es.
    // Die PID des Kindes kommt über den Zeilenstrom von `start_piped`, nicht
    // über `STDOUT_LOG`.
    let prepared = env
        .prepare_piped(&[
            "/bin/sh",
            "-c",
            "(trap '' TERM; exec sleep 30) & echo $!; wait",
        ])
        .await?;
    let mut piped = env
        .manager
        .start_piped(request("group", "agent-a", &[]), prepared)
        .map_err(ctx("start_piped"))?;
    let id = piped.job_id.clone();

    let line = tokio::time::timeout(LIMIT, piped.stdout_lines.recv())
        .await
        .map_err(ctx("recv timeout"))?
        .ok_or(TestError::Missing("child pid line"))?
        .map_err(ctx("child pid line"))?;
    let child_pid: u32 = line
        .trim()
        .parse()
        .map_err(|_| TestError::Unexpected(format!("bad pid line: {line}")))?;
    assert!(is_same_process_alive(child_pid, None));

    let stopped = env
        .manager
        .stop(&id, Caller::Agent("agent-a"), JobSignal::Term)
        .await
        .map_err(ctx("stop"))?;
    assert_eq!(stopped.meta.state, JobState::Stopped);

    let dead = eventually(LIMIT, || !is_same_process_alive(child_pid, None)).await;
    assert!(dead, "child {child_pid} of the piped job survived job.stop");
    Ok(())
}

/// Liest den Zeilenstrom eines `start_piped`-Jobs bis zu seinem Ende.
async fn drain_lines(
    lines: &mut mpsc::UnboundedReceiver<Result<String, PipedLineError>>,
) -> TestResult<Vec<Result<String, PipedLineError>>> {
    let mut items = Vec::new();
    while let Some(item) = tokio::time::timeout(LIMIT, lines.recv())
        .await
        .map_err(ctx("recv timeout"))?
    {
        items.push(item);
    }
    Ok(items)
}

/// Startet `script` über `/bin/sh -c` als `start_piped`-Job mit der
/// Zeilengrenze `limit` und liefert alle Strom-Einträge sowie die
/// `STDOUT_LOG`-Zeilen, nachdem der Job beendet ist.
async fn run_piped_script(
    env: &Env,
    script: &str,
    limit: usize,
) -> TestResult<(Vec<Result<String, PipedLineError>>, Vec<String>)> {
    let prepared = env.prepare_piped(&["/bin/sh", "-c", script]).await?;
    let mut piped = env
        .manager
        .start_piped_with_line_limit(request("limit", "agent-a", &[]), prepared, limit)
        .map_err(ctx("start_piped"))?;
    let id = piped.job_id.clone();
    let items = drain_lines(&mut piped.stdout_lines).await?;
    let (outcome, _) = env
        .manager
        .wait(&id, Caller::Agent("agent-a"), LIMIT, None)
        .await
        .map_err(ctx("wait"))?;
    assert_eq!(outcome, WaitOutcome::Finished);
    let dir = env
        .manager
        .log_dir(&id, Caller::Agent("agent-a"))
        .map_err(ctx("log dir"))?;
    Ok((items, read_all(&dir.join(STDOUT_LOG))?))
}

#[tokio::test]
async fn test_start_piped_line_at_limit_passes() -> TestResult {
    let env = Env::new()?;
    let (items, log) = run_piped_script(&env, "printf 'abcdefgh\\nxy\\r\\n'", 8).await?;
    assert_eq!(items, vec![Ok("abcdefgh".to_owned()), Ok("xy".to_owned())]);
    assert_eq!(log, vec!["abcdefgh".to_owned(), "xy".to_owned()]);
    Ok(())
}

#[tokio::test]
async fn test_start_piped_overlong_line_ends_stream_with_error() -> TestResult {
    let env = Env::new()?;
    let (items, log) = run_piped_script(&env, "printf 'abc\\nabcdefghi\\nafter\\n'", 8).await?;
    assert_eq!(
        items,
        vec![
            Ok("abc".to_owned()),
            Err(PipedLineError::TooLong { limit: 8 })
        ]
    );
    assert_eq!(log.first().map(String::as_str), Some("abc"));
    assert!(
        log.iter()
            .any(|line| line.contains("exceeds the 8-byte limit")),
        "{log:?}"
    );
    assert!(!log.iter().any(|line| line.contains("after")), "{log:?}");
    Ok(())
}

#[tokio::test]
async fn test_start_piped_endless_line_is_cut_off_at_the_limit() -> TestResult {
    let env = Env::new()?;
    // Eine Zeile ohne `\n`, die ohne Grenze endlos wüchse; `head` ist nur das
    // Sicherheitsnetz (64 MiB). Mit der Grenze endet der Strom nach 1 MiB,
    // die Pipe wird geschlossen und die Pipeline stirbt an `SIGPIPE`.
    let (items, log) = run_piped_script(
        &env,
        "yes | tr -d '\\n' | head -c 67108864",
        DEFAULT_MAX_PIPED_LINE_BYTES,
    )
    .await?;
    assert_eq!(
        items,
        vec![Err(PipedLineError::TooLong {
            limit: DEFAULT_MAX_PIPED_LINE_BYTES
        })]
    );
    assert!(
        log.iter().all(|line| line.len() < 1024) && log.len() == 1,
        "{log:?}"
    );
    Ok(())
}

#[test]
fn test_effective_notify_every_clamps() {
    let config = JobManagerConfig::new("/tmp/unused");
    assert_eq!(config.effective_notify_every(None), Duration::from_secs(60));
    assert_eq!(config.effective_notify_every(Some(0)), Duration::ZERO);
    assert_eq!(
        config.effective_notify_every(Some(1)),
        Duration::from_secs(10)
    );
    assert_eq!(
        config.effective_notify_every(Some(99_999)),
        Duration::from_secs(3600)
    );
    assert_eq!(
        config.effective_notify_every(Some(120)),
        Duration::from_secs(120)
    );
}

#[test]
fn test_milestone_key_buckets() {
    let snapshot = |percent: Option<u8>, phase: &str| ProgressSnapshot {
        source: ProgressSource::Cargo,
        percent,
        done: Some(3),
        total: None,
        phase: Some(phase.to_owned()),
    };
    assert_eq!(
        milestone_key(&snapshot(Some(41), "x")),
        milestone_key(&snapshot(Some(49), "y"))
    );
    assert_ne!(
        milestone_key(&snapshot(Some(49), "x")),
        milestone_key(&snapshot(Some(50), "x"))
    );
    assert_eq!(
        milestone_key(&snapshot(None, "compiling a")),
        milestone_key(&snapshot(None, "compiling b"))
    );
    assert_ne!(
        milestone_key(&snapshot(None, "compiling a")),
        milestone_key(&snapshot(None, "finished dev"))
    );
}
