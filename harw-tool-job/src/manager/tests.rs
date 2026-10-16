//! Tests der Job-Verwaltung mit echten, kurzen Prozessen (`/bin/sh -c`).

use super::*;
use crate::event::JobEvent;
use crate::logs::{LogQuery, read_log, truncation_marker};
use crate::model::JobEndReason;
use crate::procfs::test_process_alive;
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
        .await
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

/// R18 (P5/P6): die Herkunft eines Jobs landet in `meta.json` und im
/// `Started`-Ereignis; der Start ohne Herkunft lässt alle Felder leer.
#[tokio::test]
async fn test_start_with_origin_records_call_id_and_tool() -> TestResult {
    let env = Env::new()?;
    let prepared = env.prepare("echo origin").await?;
    let origin = JobOrigin {
        call_id: Some("call-42".to_owned()),
        tool: Some("job.start".to_owned()),
        owner_agent: None,
    };
    let started = env
        .manager
        .start_with_origin(
            request("origin", "agent-a", &[]),
            prepared,
            Vec::new(),
            origin,
        )
        .await
        .map_err(ctx("start"))?;
    let id = started.meta.job_id.clone();
    assert_eq!(started.meta.origin_call_id.as_deref(), Some("call-42"));
    assert_eq!(started.meta.origin_tool.as_deref(), Some("job.start"));
    let on_disk = read_meta(&started.log_dir).ok_or(TestError::Missing("meta.json"))?;
    assert_eq!(on_disk.origin_call_id.as_deref(), Some("call-42"));
    let events = events_of(&env, &id);
    match events.first() {
        Some(JobEvent::Started {
            origin_call_id,
            origin_tool,
            owner_agent,
            ..
        }) => {
            assert_eq!(origin_call_id.as_deref(), Some("call-42"));
            assert_eq!(origin_tool.as_deref(), Some("job.start"));
            assert_eq!(owner_agent, &None);
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "expected Started first, got {other:?}"
            )));
        }
    }

    let prepared = env.prepare("echo plain").await?;
    let plain = env
        .manager
        .start(request("plain", "agent-a", &[]), prepared)
        .await
        .map_err(ctx("start plain"))?;
    assert_eq!(plain.meta.origin_call_id, None);
    assert_eq!(plain.meta.origin_tool, None);
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
        .await
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
    assert!(test_process_alive(child_pid, None));

    let stopped = env
        .manager
        .stop(&id, Caller::Agent("agent-a"), JobSignal::Term)
        .await
        .map_err(ctx("stop"))?;
    assert_eq!(stopped.meta.state, JobState::Stopped);

    let dead = eventually(LIMIT, || !test_process_alive(child_pid, None)).await;
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
        .await
        .map_err(ctx("start"))?;
    let id = started.meta.job_id.clone();

    let status = env
        .manager
        .await_exit(&id, Caller::Agent("agent-a"), LIMIT)
        .await
        .map_err(ctx("await_exit"))?;
    assert!(status.meta.state.is_terminal());
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
        .await
        .map_err(ctx("start"))?
        .meta
        .job_id;
    let status = env
        .manager
        .await_exit(&id, Caller::Agent("agent-a"), LIMIT)
        .await
        .map_err(ctx("await_exit"))?;
    assert!(status.meta.state.is_terminal());
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
        .await
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
        .await
        .map_err(ctx("start"))?
        .meta
        .job_id;
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
async fn test_await_exit_times_out_while_the_job_runs() -> TestResult {
    let env = Env::new()?;
    let prepared = env.prepare("sleep 30").await?;
    let id = env
        .manager
        .start(request("slow", "agent-a", &[]), prepared)
        .await
        .map_err(ctx("start"))?
        .meta
        .job_id;
    let caller = Caller::Agent("agent-a");
    let status = env
        .manager
        .await_exit(&id, caller, Duration::from_millis(100))
        .await
        .map_err(ctx("short wait"))?;
    assert!(!status.meta.state.is_terminal());

    env.manager
        .stop(&id, caller, JobSignal::Kill)
        .await
        .map_err(ctx("stop"))?;
    let status = env
        .manager
        .await_exit(&id, caller, LIMIT)
        .await
        .map_err(ctx("wait after stop"))?;
    assert!(status.meta.state.is_terminal());
    Ok(())
}

#[tokio::test]
async fn test_detach_keeps_process_and_operator_can_stop_it() -> TestResult {
    let env = Env::new()?;
    let prepared = env.prepare("sleep 30").await?;
    let started = env
        .manager
        .start(request("detached", "agent-a", &[]), prepared)
        .await
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
    assert!(test_process_alive(pid, status.meta.proc_start_ticks));

    let stopped = env
        .manager
        .stop(&id, Caller::Operator, JobSignal::Term)
        .await
        .map_err(ctx("stop"))?;
    assert_eq!(stopped.meta.state, JobState::Stopped);
    assert!(!test_process_alive(pid, stopped.meta.proc_start_ticks));
    Ok(())
}

fn previous_meta(id: &str, pid: u32, ticks: Option<u64>) -> TestResult<JobMeta> {
    previous_meta_with(id, pid, ticks, None)
}

fn previous_meta_with(
    id: &str,
    pid: u32,
    ticks: Option<u64>,
    identity: Option<JobProcessIdentity>,
) -> TestResult<JobMeta> {
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
        identity,
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
        stragglers_reaped: false,
        log_truncated: false,
        launch_warnings: Vec::new(),
        origin_call_id: None,
        origin_tool: None,
        owner_agent: None,
    })
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_reload_marks_previous_jobs_detached_or_unknown() -> TestResult {
    let env = Env::new()?;
    let jobs_dir = env.state_dir().join("jobs");
    let own_pid = std::process::id();
    let own_ticks = crate::procfs::test_start_ticks(own_pid);
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

#[cfg(target_os = "linux")]
mod recovery {
    //! Wiederherstellung nach einem Neustart (Job-Runtime-Doc §21.4): ein
    //! neuer [`JobManager`] über demselben Zustandsverzeichnis.

    use super::*;
    use crate::event::RecordingNotifier;
    use crate::procfs::capture_identity;
    use std::os::unix::process::CommandExt as _;

    /// Ein eigener, fremder Prozess (`sleep 30`, eigene Gruppe), der am Ende
    /// sicher beendet und eingesammelt wird.
    struct Sleeper(std::process::Child);

    impl Sleeper {
        fn spawn() -> TestResult<Self> {
            let child = std::process::Command::new("sleep")
                .arg("30")
                .process_group(0)
                .spawn()
                .map_err(ctx("spawn sleep"))?;
            Ok(Self(child))
        }

        fn pid(&self) -> u32 {
            self.0.id()
        }

        fn identity(&self) -> TestResult<JobProcessIdentity> {
            capture_identity(self.pid()).ok_or(TestError::Missing("sleeper identity"))
        }
    }

    impl Drop for Sleeper {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn write_previous(env: &Env, meta: &JobMeta) -> TestResult {
        let dir = env.state_dir().join("jobs").join(meta.job_id.as_str());
        fs::create_dir_all(&dir).map_err(ctx("create old job dir"))?;
        write_meta(&dir, meta).map_err(ctx("write old meta"))
    }

    fn restart(env: &Env) -> TestResult<(Arc<JobManager>, Arc<RecordingNotifier>)> {
        let recorder = Arc::new(RecordingNotifier::default());
        let manager = JobManager::new(
            env.manager.config().clone(),
            Arc::clone(&recorder) as Arc<dyn JobNotifier>,
        )
        .map_err(ctx("restart"))?;
        Ok((manager, recorder))
    }

    fn job_id(raw: &str) -> TestResult<JobId> {
        JobId::parse(raw).ok_or(TestError::Missing("job id"))
    }

    fn state_of(manager: &JobManager, id: &JobId) -> TestResult<JobState> {
        Ok(manager
            .status(id, Caller::Operator)
            .map_err(ctx("status"))?
            .meta
            .state)
    }

    fn warnings(recorder: &RecordingNotifier, id: &JobId) -> Vec<String> {
        recorder
            .notifications()
            .into_iter()
            .filter_map(|notification| match notification.event {
                JobEvent::Warning {
                    job_id, message, ..
                } if &job_id == id => Some(message),
                _ => None,
            })
            .collect()
    }

    fn v2_meta(id: &str, pid: u32, identity: JobProcessIdentity) -> TestResult<JobMeta> {
        previous_meta_with(id, pid, Some(identity.start_ticks), Some(identity))
    }

    #[tokio::test]
    async fn test_alive_after_restart_is_detached_and_stoppable() -> TestResult {
        let env = Env::new()?;
        let sleeper = Sleeper::spawn()?;
        let pid = sleeper.pid();
        let identity = sleeper.identity()?;
        write_previous(&env, &v2_meta("job-prev-alive", pid, identity.clone())?)?;

        let (manager, recorder) = restart(&env)?;
        let id = job_id("job-prev-alive")?;
        assert_eq!(state_of(&manager, &id)?, JobState::Detached);
        assert!(warnings(&recorder, &id).is_empty());

        let stopped = manager
            .stop(&id, Caller::Operator, JobSignal::Term)
            .await
            .map_err(ctx("stop"))?;
        assert_eq!(stopped.meta.state, JobState::Stopped);
        assert!(stopped.meta.stop_requested);
        assert!(!test_process_alive(pid, Some(identity.start_ticks)));
        Ok(())
    }

    #[tokio::test]
    async fn test_own_job_survives_restart_after_exec_and_is_stoppable() -> TestResult {
        // The shell `exec`s `sleep`: the leader's program changes after the
        // start. `detach_all` records the last observation, so the next
        // session still proves the identity (exe included) and may stop it.
        let env = Env::new()?;
        let prepared = env.prepare("exec sleep 30").await?;
        let started = env
            .manager
            .start(request("exec", "agent-a", &[]), prepared)
            .await
            .map_err(ctx("start"))?;
        let id = started.meta.job_id.clone();
        let pid = started.meta.pid.ok_or(TestError::Missing("pid"))?;
        let shell = fs::canonicalize("/bin/sh")
            .map_err(ctx("resolve /bin/sh"))?
            .display()
            .to_string();
        let mut current = None;
        let execed = eventually(LIMIT, || {
            current = capture_identity(pid)
                .and_then(|identity| identity.executable)
                .filter(|exe| exe != &shell);
            current.is_some()
        })
        .await;
        let current = current
            .filter(|_| execed)
            .ok_or(TestError::Missing("job did not exec sleep"))?;

        assert_eq!(env.manager.detach_all().detached, 1);
        let on_disk = read_meta(&started.log_dir).ok_or(TestError::Missing("meta"))?;
        assert_eq!(on_disk.version, META_VERSION);
        let persisted = on_disk
            .identity
            .as_ref()
            .and_then(|identity| identity.executable.clone())
            .ok_or(TestError::Missing("persisted exe"))?;
        assert_eq!(persisted, current);

        let (manager, _recorder) = restart(&env)?;
        assert_eq!(state_of(&manager, &id)?, JobState::Detached);
        let stopped = manager
            .stop(&id, Caller::Agent("agent-a"), JobSignal::Term)
            .await
            .map_err(ctx("stop"))?;
        assert_eq!(stopped.meta.state, JobState::Stopped);
        assert!(!test_process_alive(pid, on_disk.proc_start_ticks));
        Ok(())
    }

    #[tokio::test]
    async fn test_exited_while_down_is_unknown() -> TestResult {
        let env = Env::new()?;
        let mut sleeper = Sleeper::spawn()?;
        let pid = sleeper.pid();
        let identity = sleeper.identity()?;
        sleeper.0.kill().map_err(ctx("kill sleeper"))?;
        sleeper.0.wait().map_err(ctx("reap sleeper"))?;
        write_previous(&env, &v2_meta("job-prev-gone", pid, identity)?)?;

        let (manager, _recorder) = restart(&env)?;
        let id = job_id("job-prev-gone")?;
        assert_eq!(state_of(&manager, &id)?, JobState::Unknown);
        let on_disk = read_meta(&env.state_dir().join("jobs").join(id.as_str()))
            .ok_or(TestError::Missing("meta"))?;
        assert_eq!(on_disk.state, JobState::Unknown);
        Ok(())
    }

    #[tokio::test]
    async fn test_reused_pid_is_never_signalled() -> TestResult {
        // The PID belongs to a live, unrelated process; the persisted start
        // time is someone else's. Neither reload nor stop may touch it.
        let env = Env::new()?;
        let sleeper = Sleeper::spawn()?;
        let pid = sleeper.pid();
        let real = sleeper.identity()?;
        let recorded = JobProcessIdentity {
            start_ticks: real.start_ticks.saturating_sub(1),
            ..real.clone()
        };
        write_previous(&env, &v2_meta("job-prev-reused", pid, recorded)?)?;

        let (manager, recorder) = restart(&env)?;
        let id = job_id("job-prev-reused")?;
        assert_eq!(state_of(&manager, &id)?, JobState::Unknown);
        let messages = warnings(&recorder, &id);
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            messages[0].contains("reused, not controlled"),
            "{messages:?}"
        );

        let after = manager
            .stop(&id, Caller::Operator, JobSignal::Kill)
            .await
            .map_err(ctx("stop"))?;
        assert_eq!(after.meta.state, JobState::Unknown);
        assert!(!after.meta.stop_requested);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            test_process_alive(pid, Some(real.start_ticks)),
            "unrelated process {pid} was signalled"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_v1_meta_still_loads_and_controls() -> TestResult {
        let env = Env::new()?;
        let sleeper = Sleeper::spawn()?;
        let pid = sleeper.pid();
        let ticks = sleeper.identity()?.start_ticks;
        let jobs_dir = env.state_dir().join("jobs");
        for (id, ticks_field) in [
            ("job-v1", format!(r#""proc_start_ticks":{ticks},"#)),
            ("job-v1-noticks", String::new()),
        ] {
            let dir = jobs_dir.join(id);
            fs::create_dir_all(&dir).map_err(ctx("create v1 dir"))?;
            let json = format!(
                r#"{{"version":1,"job_id":"{id}","name":"{id}","command":"sleep 30",
                   "state":"running","pid":{pid},{ticks_field}"executed_on_host":true,
                   "harw_instance":"harw-old","owner":{{"session":"old"}},
                   "created_at":"2026-01-01T00:00:00Z","notify_every_secs":60}}"#
            );
            fs::write(dir.join(META_FILE), json).map_err(ctx("write v1 meta"))?;
        }

        let (manager, recorder) = restart(&env)?;
        // Without a start time a PID alone is no proof: never controlled.
        let no_ticks = job_id("job-v1-noticks")?;
        assert_eq!(state_of(&manager, &no_ticks)?, JobState::Unknown);
        assert_eq!(warnings(&recorder, &no_ticks).len(), 1);
        assert!(test_process_alive(pid, Some(ticks)));

        let v1 = job_id("job-v1")?;
        let status = manager
            .status(&v1, Caller::Operator)
            .map_err(ctx("status"))?;
        assert_eq!(status.meta.state, JobState::Detached);
        assert_eq!(status.meta.version, 1);
        assert_eq!(
            status
                .meta
                .process_identity()
                .map(|identity| identity.executable),
            Some(None)
        );
        let stopped = manager
            .stop(&v1, Caller::Operator, JobSignal::Term)
            .await
            .map_err(ctx("stop v1"))?;
        assert_eq!(stopped.meta.state, JobState::Stopped);
        assert!(!test_process_alive(pid, Some(ticks)));
        Ok(())
    }

    #[tokio::test]
    async fn test_identity_mismatch_on_stop_is_refused() -> TestResult {
        let env = Env::new()?;
        let sleeper = Sleeper::spawn()?;
        let pid = sleeper.pid();
        let identity = sleeper.identity()?;
        write_previous(&env, &v2_meta("job-prev-swap", pid, identity.clone())?)?;
        let (manager, recorder) = restart(&env)?;
        let id = job_id("job-prev-swap")?;
        assert_eq!(state_of(&manager, &id)?, JobState::Detached);

        // Between reload and stop the PID "changes owner": simulated by a
        // different persisted program.
        let entry = lock(&manager.jobs)
            .get(&id)
            .cloned()
            .ok_or(TestError::Missing("entry"))?;
        lock(&entry.state).meta.identity = Some(JobProcessIdentity {
            executable: Some("/nonexistent/other-program".to_owned()),
            ..identity.clone()
        });

        let after = manager
            .stop(&id, Caller::Operator, JobSignal::Kill)
            .await
            .map_err(ctx("stop"))?;
        assert_eq!(after.meta.state, JobState::Unknown);
        assert!(!after.meta.stop_requested);
        assert!(test_process_alive(pid, Some(identity.start_ticks)));
        let messages = warnings(&recorder, &id);
        assert!(
            messages
                .iter()
                .any(|message| message.contains("sent no signal")),
            "{messages:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_detached_own_job_stop_kills_grandchildren() -> TestResult {
        let env = Env::new()?;
        let prepared = env
            .prepare("(trap '' TERM; exec sleep 30) & echo $!; wait")
            .await?;
        let started = env
            .manager
            .start(request("group", "agent-a", &[]), prepared)
            .await
            .map_err(ctx("start"))?;
        let id = started.meta.job_id.clone();
        let stdout = started.log_dir.join(STDOUT_LOG);
        let mut grandchild = None;
        let found = eventually(LIMIT, || {
            grandchild = read_all(&stdout).ok().and_then(|lines| {
                lines
                    .first()
                    .and_then(|line| line.trim().parse::<u32>().ok())
            });
            grandchild.is_some()
        })
        .await;
        let grandchild = grandchild
            .filter(|_| found)
            .ok_or(TestError::Missing("grandchild pid"))?;

        assert_eq!(env.manager.detach_all().detached, 1);
        let stopped = env
            .manager
            .stop(&id, Caller::Operator, JobSignal::Term)
            .await
            .map_err(ctx("stop"))?;
        assert_eq!(stopped.meta.state, JobState::Stopped);
        let dead = eventually(LIMIT, || !test_process_alive(grandchild, None)).await;
        assert!(
            dead,
            "grandchild {grandchild} survived job.stop of a detached job"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_stale_tmp_and_corrupt_files_are_ignored() -> TestResult {
        let env = Env::new()?;
        let jobs_dir = env.state_dir().join("jobs");
        let mut done = previous_meta("job-done", u32::MAX / 2, Some(1))?;
        done.state = JobState::Succeeded;
        write_previous(&env, &done)?;
        let done_dir = jobs_dir.join("job-done");
        fs::write(done_dir.join(format!("{META_FILE}.tmp")), b"{ half-written")
            .map_err(ctx("stale tmp"))?;
        let tmp_only = jobs_dir.join("job-tmp-only");
        fs::create_dir_all(&tmp_only).map_err(ctx("tmp-only dir"))?;
        fs::write(tmp_only.join(format!("{META_FILE}.tmp")), b"{}").map_err(ctx("tmp only"))?;
        let corrupt = jobs_dir.join("job-corrupt");
        fs::create_dir_all(&corrupt).map_err(ctx("corrupt dir"))?;
        fs::write(corrupt.join(META_FILE), b"not json").map_err(ctx("corrupt meta"))?;
        fs::write(jobs_dir.join("stale.lock"), b"").map_err(ctx("stray lock"))?;

        let (manager, _recorder) = restart(&env)?;
        let ids: Vec<String> = manager
            .list(Caller::Operator)
            .into_iter()
            .map(|status| status.meta.job_id.to_string())
            .collect();
        assert_eq!(ids, vec!["job-done".to_owned()]);
        assert_eq!(
            state_of(&manager, &job_id("job-done")?)?,
            JobState::Succeeded
        );
        Ok(())
    }
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
        .await
        .map_err(ctx("start"))?
        .meta
        .job_id;
    assert!(matches!(
        env.manager.check_capacity(),
        Err(JobError::Capacity { max: 1 })
    ));
    let Err(err) = env.manager.check_capacity() else {
        return Err(TestError::Missing("capacity error"));
    };
    // Die Meldung nennt den Konfigurationsschlüssel, mit dem man die Grenze hebt.
    assert!(err.to_string().contains("[jobs] max_running"), "{err}");
    let second = env.prepare("sleep 30").await?;
    assert!(matches!(
        env.manager
            .start(request("two", "agent-a", &[]), second)
            .await,
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
async fn test_capacity_is_adjustable_at_runtime_without_stopping_jobs() -> TestResult {
    let env = Env::with_config(|config| JobManagerConfig {
        max_running_jobs: 2,
        ..config
    })?;
    assert_eq!(env.manager.max_running(), 2);
    for name in ["one", "two"] {
        let prepared = env.prepare("sleep 30").await?;
        env.manager
            .start(request(name, "agent-a", &[]), prepared)
            .await
            .map_err(ctx("start"))?;
    }
    assert!(matches!(
        env.manager.check_capacity(),
        Err(JobError::Capacity { max: 2 })
    ));
    // Raising admits another job at once.
    assert_eq!(env.manager.set_max_running(3), 3);
    assert!(env.manager.check_capacity().is_ok());
    // Lowering (and the clamp to at least 1) stops nothing that runs.
    assert_eq!(env.manager.set_max_running(0), 1);
    assert_eq!(env.manager.running_count(), 2);
    assert!(matches!(
        env.manager.check_capacity(),
        Err(JobError::Capacity { max: 1 })
    ));
    assert_eq!(env.manager.set_max_running(10_000), 256);
    assert_eq!(env.manager.stop_all().await, 2);
    Ok(())
}

/// Wartet, bis der Job `id` beendet ist, und liefert seinen Status.
async fn wait_terminal(env: &Env, id: &JobId) -> TestResult<JobStatus> {
    let done = eventually(LIMIT, || {
        env.manager
            .status(id, Caller::Operator)
            .is_ok_and(|status| status.meta.state.is_terminal())
    })
    .await;
    if !done {
        return Err(TestError::Missing("job did not end in time"));
    }
    env.manager
        .status(id, Caller::Operator)
        .map_err(ctx("status"))
}

/// Meldungen aller `Warning`-Ereignisse des Jobs `id`.
fn warning_messages(env: &Env, id: &JobId) -> Vec<String> {
    events_of(env, id)
        .into_iter()
        .filter_map(|event| match event {
            JobEvent::Warning { message, .. } => Some(message),
            _ => None,
        })
        .collect()
}

/// Obergrenze einer gekürzten Logdatei: Budget plus eine Markerzeile.
fn capped_len(budget: u64) -> TestResult<u64> {
    let marker = u64::try_from(truncation_marker(budget).len()).map_err(ctx("marker length"))?;
    Ok(budget.saturating_add(marker))
}

#[tokio::test]
async fn test_normal_end_reaps_stragglers() -> TestResult {
    let env = Env::new()?;
    // Die Shell endet sofort mit 0; das Enkelkind ignoriert SIGTERM und
    // bleibt in der Prozessgruppe zurück — erst SIGKILL nach der Frist trifft es.
    let prepared = env
        .prepare("(trap '' TERM; exec sleep 30) & echo $!")
        .await?;
    let started = env
        .manager
        .start(request("straggler", "agent-a", &[]), prepared)
        .await
        .map_err(ctx("start"))?;
    let id = started.meta.job_id.clone();
    let stdout = started.log_dir.join(STDOUT_LOG);

    let status = wait_terminal(&env, &id).await?;
    let grandchild = read_all(&stdout)?
        .first()
        .and_then(|line| line.trim().parse::<u32>().ok())
        .ok_or(TestError::Missing("grandchild pid"))?;
    let dead = eventually(LIMIT, || !test_process_alive(grandchild, None)).await;
    assert!(dead, "grandchild {grandchild} survived the end of its job");

    assert_eq!(status.meta.state, JobState::Succeeded);
    assert!(status.meta.stragglers_reaped);
    assert_eq!(status.meta.end_reason(), Some(JobEndReason::Exited));
    let messages = warning_messages(&env, &id);
    assert!(
        messages
            .iter()
            .any(|message| message.contains("terminated")),
        "{messages:?}"
    );
    Ok(())
}

#[tokio::test]
async fn test_clean_end_reaps_nothing() -> TestResult {
    let env = Env::new()?;
    let prepared = env.prepare("echo done").await?;
    let id = env
        .manager
        .start(request("clean", "agent-a", &[]), prepared)
        .await
        .map_err(ctx("start"))?
        .meta
        .job_id;
    let status = wait_terminal(&env, &id).await?;
    assert_eq!(status.meta.state, JobState::Succeeded);
    assert!(!status.meta.stragglers_reaped);
    assert!(!status.meta.log_truncated);
    let messages = warning_messages(&env, &id);
    assert!(messages.is_empty(), "{messages:?}");
    Ok(())
}

#[tokio::test]
async fn test_small_log_budget_truncates_with_marker() -> TestResult {
    let env = Env::with_config(|c| JobManagerConfig {
        max_log_bytes: 1024,
        ..c
    })?;
    let prepared = env
        .prepare("i=0; while [ $i -lt 3000 ]; do echo line-$i; i=$((i+1)); done")
        .await?;
    let started = env
        .manager
        .start(request("chatty", "agent-a", &[]), prepared)
        .await
        .map_err(ctx("start"))?;
    let id = started.meta.job_id.clone();
    let status = wait_terminal(&env, &id).await?;
    assert_eq!(status.meta.state, JobState::Succeeded);
    assert!(status.meta.log_truncated);

    let marker = truncation_marker(1024);
    let text =
        fs::read_to_string(started.log_dir.join(STDOUT_LOG)).map_err(ctx("read stdout.log"))?;
    let len = u64::try_from(text.len()).map_err(ctx("log length"))?;
    assert!(len <= capped_len(1024)?, "stdout.log has {len} bytes");
    assert!(text.ends_with(&marker), "marker missing at the end");
    assert_eq!(text.matches(marker.as_str()).count(), 1);
    assert!(text.starts_with("line-0\n"));

    let stderr = fs::metadata(started.log_dir.join(STDERR_LOG)).map_err(ctx("stderr.log"))?;
    assert_eq!(stderr.len(), 0);

    let messages = warning_messages(&env, &id);
    let about_stdout: Vec<&String> = messages
        .iter()
        .filter(|message| message.contains("stdout.log"))
        .collect();
    assert_eq!(about_stdout.len(), 1, "{messages:?}");
    Ok(())
}

#[tokio::test]
async fn test_log_budget_applies_while_running() -> TestResult {
    let env = Env::with_config(|c| JobManagerConfig {
        max_log_bytes: 1024,
        ..c
    })?;
    let prepared = env
        .prepare("head -c 200000 /dev/zero | tr '\\0' 'x'; echo; sleep 30")
        .await?;
    let started = env
        .manager
        .start(request("flood", "agent-a", &[]), prepared)
        .await
        .map_err(ctx("start"))?;
    let id = started.meta.job_id.clone();
    let stdout = started.log_dir.join(STDOUT_LOG);
    let capped = capped_len(1024)?;

    // Die Überwachung kürzt schon während der Laufzeit, nicht erst am Ende.
    let bounded = eventually(LIMIT, || {
        let running_and_truncated = env
            .manager
            .status(&id, Caller::Operator)
            .is_ok_and(|status| {
                status.meta.state == JobState::Running && status.meta.log_truncated
            });
        running_and_truncated && fs::metadata(&stdout).is_ok_and(|meta| meta.len() <= capped)
    })
    .await;
    assert!(bounded, "log was not capped while the job ran");

    assert_eq!(env.manager.stop_all().await, 1);
    Ok(())
}

#[tokio::test]
async fn test_launch_warnings_recorded_and_notified() -> TestResult {
    let env = Env::new()?;
    let warning = "runs without resource limits: prlimit not found".to_owned();
    let started = env
        .manager
        .start_with_warnings(
            request("w", "agent-a", &[]),
            env.prepare("echo ok").await?,
            vec![warning.clone()],
        )
        .await
        .map_err(ctx("start"))?;
    let id = started.meta.job_id.clone();

    let status = env
        .manager
        .status(&id, Caller::Agent("agent-a"))
        .map_err(ctx("status"))?;
    assert_eq!(status.meta.launch_warnings, vec![warning.clone()]);

    let on_disk = read_meta(&started.log_dir).ok_or(TestError::Missing("meta.json"))?;
    assert_eq!(on_disk.launch_warnings, vec![warning.clone()]);

    let notified = eventually(LIMIT, || {
        env.recorder
            .notifications()
            .into_iter()
            .any(|notification| {
                notification.owner.session == "agent-a"
                    && matches!(
                        &notification.event,
                        JobEvent::Warning { job_id, message, .. }
                            if job_id == &id && message == &warning
                    )
            })
    })
    .await;
    assert!(notified, "no launch warning reached the owner");
    wait_terminal(&env, &id).await?;
    Ok(())
}

#[tokio::test]
async fn test_old_meta_json_loads_with_new_fields_defaulted() -> TestResult {
    let env = Env::new()?;
    let dir = env.state_dir().join("jobs").join("job-legacy");
    fs::create_dir_all(&dir).map_err(ctx("create legacy dir"))?;
    // Stand vor `stragglers_reaped`, `log_truncated` und `launch_warnings`.
    let json = r#"{"version":2,"job_id":"job-legacy","name":"legacy","command":"make",
        "state":"succeeded","exit_code":0,"executed_on_host":false,
        "harw_instance":"harw-old","owner":{"session":"old"},
        "created_at":"2026-01-01T00:00:00Z","ended_at":"2026-01-01T00:01:00Z",
        "notify_every_secs":60}"#;
    fs::write(dir.join(META_FILE), json).map_err(ctx("write legacy meta"))?;

    let reloaded = JobManager::new(env.manager.config().clone(), Arc::new(crate::NoopNotifier))
        .map_err(ctx("reload"))?;
    let id = JobId::parse("job-legacy").ok_or(TestError::Missing("job id"))?;
    let status = reloaded
        .status(&id, Caller::Operator)
        .map_err(ctx("status"))?;
    assert_eq!(status.meta.state, JobState::Succeeded);
    assert!(!status.meta.stragglers_reaped);
    assert!(!status.meta.log_truncated);
    assert!(status.meta.launch_warnings.is_empty());
    assert_eq!(status.meta.sandbox_profile(), "bwrap");
    assert_eq!(status.meta.end_reason(), Some(JobEndReason::Exited));
    Ok(())
}

#[tokio::test]
async fn test_spawn_failure_is_recorded_as_failed() -> TestResult {
    let env = Env::new()?;
    let mut command = tokio::process::Command::new("/nonexistent/harw-job-binary");
    command.stdin(std::process::Stdio::null());
    let prepared = PreparedJob {
        command,
        executed_on_host: true,
        env_cleared: false,
    };
    let result = env
        .manager
        .start(request("broken", "agent-a", &[]), prepared)
        .await;
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
        .await
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

    let status = env
        .manager
        .await_exit(&id, Caller::Agent("agent-a"), LIMIT)
        .await
        .map_err(ctx("await_exit"))?;
    assert!(status.meta.state.is_terminal());
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
        .await
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
    assert!(test_process_alive(child_pid, None));

    let stopped = env
        .manager
        .stop(&id, Caller::Agent("agent-a"), JobSignal::Term)
        .await
        .map_err(ctx("stop"))?;
    assert_eq!(stopped.meta.state, JobState::Stopped);

    let dead = eventually(LIMIT, || !test_process_alive(child_pid, None)).await;
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
        .await
        .map_err(ctx("start_piped"))?;
    let id = piped.job_id.clone();
    let items = drain_lines(&mut piped.stdout_lines).await?;
    let status = env
        .manager
        .await_exit(&id, Caller::Agent("agent-a"), LIMIT)
        .await
        .map_err(ctx("await_exit"))?;
    assert!(status.meta.state.is_terminal());
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
