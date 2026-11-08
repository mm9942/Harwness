//! Tests von `job.suspend` / `job.resume` im Manager (PL-100 WP 3.5) mit
//! einem gefälschten cgroup-Backend hinter dem echten
//! `CgroupFreezer`: Besitz wie `job.stop`, Ziel nur die bewiesene
//! eigene cgroup, kein Übernehmen oder Aufheben eines fremden Freeze,
//! Absicht vor Wirkung (auch bei nicht beschreibbarem Zustandsverzeichnis),
//! Idempotenz, typisierte Fehler, Stop ohne Auftauen, Ablösen, Neustart,
//! Wettläufe mit dem Ende des Jobs, mit einer Warden-Eindämmung, die nach
//! harws Anhalten dazukommt (auch über ein echtes Markerverzeichnis samt
//! Sperre, die nach bestem Bemühen genommen wird), und mit veralteten
//! Schreibzugriffen der Überwachung auf `meta.json`; dazu ein gescheitertes
//! Wiedereinfrieren und eine nach dem Auftauen verschwundene cgroup, sowie
//! ein nach dem `rename` des Vermerks gescheiterter `fsync` des
//! Job-Verzeichnisses (Naht `DirSync`). Dazu die Phase des Anhaltens
//! (Absicht vor, Wirkung nach dem Einfrieren): ein Neustart mit noch
//! lebendem Job (Naht `JobManager::with_liveness`), nach dem eine nur als
//! Absicht vermerkte Suspendierung ungeklärt bleibt und `job.resume` keinen
//! Freeze außerhalb von harw aufhebt; eine belegte Wirkung, die nach dem
//! Neustart auftaut; ein nicht schreibbarer Vermerk der Wirkung; ältere
//! Einträge ohne Phase; das Klären durch ein erneutes `job.suspend` und
//! ein solches, das scheitert (das Anhalten bleibt, wie es war) oder auf
//! einen laufenden Stop trifft; eine nicht schreibbare Rücknahme des
//! Vermerks der Wirkung (eigener Fehler, synchronisierter Schreiber). Dazu
//! die erneute Prüfung direkt vor dem Einfrieren: ein Freeze außerhalb von
//! harw (oder ein Marker des Wardens), der während des Verzeichnis-Syncs der
//! Absicht kommt, wird weder bei einem frischen noch bei einem erneuten
//! `job.suspend` übernommen (auch nicht, wenn die Rücknahme scheitert, und
//! nicht nach einem Neustart); und nach einem abgelehnten `job.resume`
//! steht wieder `applied` auf der Platte (bzw., wenn das nicht geschrieben
//! werden kann, nur die Absicht, die sichere Seite).

use super::{
    Caller, DirSync, EntryState, JobEntry, JobError, JobManager, SUSPENDED_ON_RELOAD,
    SUSPENSION_UNRESOLVED_ON_RELOAD, SuspendTransition, SyncDirectory, lock, read_meta,
    stop_signal_for, write_meta,
};
use crate::event::RecordingNotifier;
use crate::event::{JobEvent, JobNotifier};
use crate::freeze::{ContainmentLease, FreezeError, JobFreezer};
use crate::model::{JobId, JobMeta, JobState, JobStatus, META_FILE, SuspendPhase};
use crate::procfs::{JobSignal, Liveness};
use crate::test_support::fake_cgroup::{
    FakeCgroup, FakeHolds, fake_kernel_id, freezer, freezer_over, freezer_with_holds, marker_index,
    write_marker,
};
use crate::test_support::{Env, TestError, TestResult, ctx, request, stub_job_meta};
use jiff::Timestamp;
use std::fs;
use std::io;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// `Env` with the fake freezer installed.
fn env_with_freezer(fake: &Arc<FakeCgroup>) -> TestResult<Env> {
    let env = Env::new()?;
    env.manager.set_freezer(freezer(fake));
    Ok(env)
}

/// Registers a running stub job of `worker` (ancestor `orchestrator`) with
/// a proven own cgroup `harw-job-<attempt>`.
fn running_job(env: &Env, id: &str, attempt: &str) -> TestResult<JobId> {
    let meta = stub_job_meta(
        id,
        JobState::Running,
        "worker",
        &["orchestrator"],
        Some(attempt),
    )?;
    env.manager
        .insert_test_job(meta, true)
        .map_err(ctx("insert stub job"))
}

/// The persisted `meta.json` of `id`.
fn on_disk(env: &Env, id: &JobId) -> TestResult<JobMeta> {
    let dir = env
        .manager
        .log_dir(id, Caller::Operator)
        .map_err(ctx("log dir"))?;
    read_meta(&dir).ok_or(TestError::Missing("meta.json"))
}

/// The current status of `id` (operator view).
fn status_of(env: &Env, id: &JobId) -> TestResult<JobStatus> {
    env.manager
        .status(id, Caller::Operator)
        .map_err(ctx("status"))
}

fn call(operation: &'static str, attempt: &str) -> (&'static str, String) {
    (operation, format!("harw-job-{attempt}"))
}

/// `now` minus `secs` seconds.
fn ago(secs: i64) -> TestResult<Timestamp> {
    Timestamp::now()
        .checked_sub(jiff::SignedDuration::from_secs(secs))
        .map_err(ctx("timestamp in the past"))
}

/// An own child process (`sleep 30`, its own process group) that is killed
/// and reaped at the end of the test.
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
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        // Best-effort cleanup: the process may already be dead (a test
        // killed it); neither failure changes the test result.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A detached (not monitored) stub job of `worker` that is still suspended
/// since `secs` seconds in `phase` (`None`: a record from before the
/// phase), with a proven own cgroup and our child `sleeper` as its adopted
/// group leader.
fn detached_suspended_job(
    env: &Env,
    id: &str,
    attempt: &str,
    sleeper: &Sleeper,
    secs: i64,
    phase: Option<SuspendPhase>,
) -> TestResult<JobId> {
    let mut meta = stub_job_meta(id, JobState::Detached, "worker", &[], Some(attempt))?;
    meta.detached = true;
    meta.pid = Some(sleeper.pid());
    meta.started_at = Some(ago(secs.saturating_add(10))?);
    meta.suspended_at = Some(ago(secs)?);
    meta.suspend_phase = phase;
    env.manager
        .insert_test_job_led(meta, false, sleeper.pid())
        .map_err(ctx("insert detached stub job"))
}

#[test]
fn suspend_and_resume_freeze_and_thaw_the_proven_own_cgroup() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-own", "a1")?;

    let suspended = env
        .manager
        .suspend(&id, Caller::Agent("worker"))
        .map_err(ctx("suspend"))?;
    assert_eq!(suspended.transition, SuspendTransition::Suspended);
    assert_eq!(suspended.status.meta.state, JobState::Suspended);
    assert!(suspended.status.meta.suspended_at.is_some());
    assert_eq!(suspended.frozen, Some(true));
    assert_eq!(fake.calls(), vec![call("freeze", "a1")]);
    // Frozen and recorded as harw's own: the effect after the intent.
    assert_eq!(
        suspended.status.meta.suspension_phase(),
        Some(SuspendPhase::Applied)
    );
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert!(persisted.suspended_at.is_some());
    assert_eq!(persisted.suspend_phase, Some(SuspendPhase::Applied));
    // A suspended job keeps its running slot.
    assert_eq!(env.manager.running_count(), 1);

    // An ancestor may resume it, exactly as it may stop it.
    let resumed = env
        .manager
        .resume(&id, Caller::Agent("orchestrator"))
        .map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(resumed.status.meta.state, JobState::Running);
    assert_eq!(resumed.status.meta.suspended_at, None);
    assert_eq!(resumed.frozen, Some(false));
    assert_eq!(fake.calls(), vec![call("freeze", "a1"), call("thaw", "a1")]);
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Running);
    assert_eq!(persisted.suspended_at, None);
    assert_eq!(persisted.suspend_phase, None);
    Ok(())
}

/// Same ownership rule as `job.stop`: a foreign job (also a sibling of the
/// owner) gets exactly the message of an unknown job, and nothing is frozen.
#[test]
fn foreign_jobs_are_refused_like_unknown_jobs() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-theirs", "b1")?;
    let unknown = JobId::parse("job-404").ok_or(TestError::Missing("job id"))?;

    for caller in [Caller::Agent("stranger"), Caller::Agent("sibling")] {
        for result in [
            env.manager.suspend(&id, caller),
            env.manager.resume(&id, caller),
        ] {
            match result {
                Err(JobError::NotFound(job)) => assert_eq!(job, id.to_string()),
                other => return Err(TestError::Unexpected(format!("{other:?}"))),
            }
        }
    }
    let foreign = env
        .manager
        .suspend(&id, Caller::Agent("stranger"))
        .err()
        .ok_or(TestError::Missing("foreign refusal"))?;
    let missing = env
        .manager
        .suspend(&unknown, Caller::Agent("worker"))
        .err()
        .ok_or(TestError::Missing("unknown refusal"))?;
    assert_eq!(
        foreign.to_string(),
        JobError::NotFound(id.to_string()).to_string()
    );
    assert_eq!(
        missing.to_string(),
        JobError::NotFound("job-404".to_owned()).to_string()
    );
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    assert_eq!(status_of(&env, &id)?.meta.state, JobState::Running);
    Ok(())
}

/// Suspending a suspended job and resuming a running one are typed,
/// non-fatal results. A repeated suspend re-applies the freeze (idempotent
/// in the kernel) and reports the kernel view; a resume of a running job
/// writes nothing.
#[test]
fn repeated_suspend_and_resume_are_idempotent() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-twice", "c1")?;
    let worker = Caller::Agent("worker");

    let never = env
        .manager
        .resume(&id, worker)
        .map_err(ctx("resume early"))?;
    assert_eq!(never.transition, SuspendTransition::NotSuspended);
    assert_eq!(never.status.meta.state, JobState::Running);
    assert_eq!(never.frozen, Some(false));
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());

    let first = env.manager.suspend(&id, worker).map_err(ctx("suspend 1"))?;
    let since = first.status.meta.suspended_at;
    let again = env.manager.suspend(&id, worker).map_err(ctx("suspend 2"))?;
    assert_eq!(first.transition, SuspendTransition::Suspended);
    assert_eq!(again.transition, SuspendTransition::AlreadySuspended);
    assert!(!again.transition.changed());
    assert_eq!(again.status.meta.state, JobState::Suspended);
    // The first suspension keeps its start.
    assert_eq!(again.status.meta.suspended_at, since);
    assert_eq!(again.frozen, Some(true));

    let resumed = env.manager.resume(&id, worker).map_err(ctx("resume 1"))?;
    let again = env.manager.resume(&id, worker).map_err(ctx("resume 2"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(again.transition, SuspendTransition::NotSuspended);
    assert_eq!(again.status.meta.state, JobState::Running);
    assert_eq!(again.frozen, Some(false));
    assert_eq!(
        fake.calls(),
        vec![
            call("freeze", "c1"),
            call("freeze", "c1"),
            call("thaw", "c1")
        ]
    );
    Ok(())
}

/// harw never takes over a freeze it did not record (an operator, the
/// Warden, a frozen parent cgroup; A3/O3), whether it is complete, still in
/// progress or inherited — and `job.resume` never lifts it. Nothing is
/// written to the kernel and the record stays as it was.
#[test]
fn a_freeze_set_outside_harw_is_neither_taken_over_nor_lifted() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-warden", "w1")?;
    let worker = Caller::Agent("worker");

    for (what, requested, frozen) in [
        ("complete", true, true),
        ("in progress", true, false),
        ("inherited from a parent", false, true),
    ] {
        fake.set_outside("harw-job-w1", requested, frozen);
        match env.manager.suspend(&id, worker) {
            Err(JobError::FrozenElsewhere(job)) => assert_eq!(job, "job-warden", "{what}"),
            other => return Err(TestError::Unexpected(format!("{what}: {other:?}"))),
        }
        match env.manager.resume(&id, worker) {
            Err(JobError::FrozenElsewhere(job)) => assert_eq!(job, "job-warden", "{what}"),
            other => return Err(TestError::Unexpected(format!("{what}: {other:?}"))),
        }
        let status = status_of(&env, &id)?;
        assert_eq!(status.meta.state, JobState::Running, "{what}");
        assert_eq!(status.meta.suspended_at, None, "{what}");
        assert_eq!(on_disk(&env, &id)?.suspended_at, None, "{what}");
    }
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());

    // Once the outside freeze is lifted by its owner, harw may suspend.
    fake.set_outside("harw-job-w1", false, false);
    let suspended = env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    assert_eq!(suspended.transition, SuspendTransition::Suspended);
    assert_eq!(fake.calls(), vec![call("freeze", "w1")]);
    Ok(())
}

/// A suspension lifted outside harw (the cgroup thawed behind its back) is
/// re-applied by the next `job.suspend`, which reports the kernel view
/// instead of claiming success blindly.
#[test]
fn a_freeze_lifted_outside_harw_is_reapplied() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-thawed", "x1")?;
    let worker = Caller::Agent("worker");

    env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    fake.set_outside("harw-job-x1", false, false);
    let again = env
        .manager
        .suspend(&id, worker)
        .map_err(ctx("suspend again"))?;
    assert_eq!(again.transition, SuspendTransition::AlreadySuspended);
    assert_eq!(again.frozen, Some(true));
    assert_eq!(again.status.meta.state, JobState::Suspended);
    assert_eq!(
        fake.calls(),
        vec![call("freeze", "x1"), call("freeze", "x1")]
    );
    Ok(())
}

/// A job whose stop is in its grace period is not frozen: that would hold
/// its TERM handler and turn the stop into a SIGKILL.
#[test]
fn a_job_being_stopped_is_not_suspended() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let mut meta = stub_job_meta("job-stopping", JobState::Running, "worker", &[], Some("s1"))?;
    meta.stop_requested = true;
    let id = env
        .manager
        .insert_test_job(meta, true)
        .map_err(ctx("insert"))?;
    match env.manager.suspend(&id, Caller::Agent("worker")) {
        Err(JobError::StopPending(job)) => assert_eq!(job, "job-stopping"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Running);
    assert_eq!(status.meta.suspended_at, None);
    Ok(())
}

/// Fail closed: no freezer, no runtime proof, a proof beneath another root,
/// or a job that is not running under this session's supervision — nothing
/// is frozen and the state stays as it was.
#[test]
fn suspend_without_a_proven_target_fails_closed() -> TestResult {
    // No freezer installed.
    let env = Env::new()?;
    let id = running_job(&env, "job-nofreezer", "d1")?;
    assert!(matches!(
        env.manager.suspend(&id, Caller::Agent("worker")),
        Err(JobError::SuspendUnavailable)
    ));

    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    // No dedicated-cgroup proof (the job shares harw's cgroup).
    let unproven = stub_job_meta("job-unproven", JobState::Running, "worker", &[], None)?;
    let unproven = env
        .manager
        .insert_test_job(unproven, true)
        .map_err(ctx("insert unproven"))?;
    // A proof for a cgroup beneath another delegated root.
    let mut elsewhere = stub_job_meta(
        "job-elsewhere",
        JobState::Running,
        "worker",
        &[],
        Some("e1"),
    )?;
    if let Some(identity) = elsewhere.identity.as_mut() {
        identity.cgroup_path = Some("/other.slice/harw-job-e1".to_owned());
        if let Some(proof) = identity.dedicated_cgroup.as_mut() {
            proof.delegated_root = "/other.slice".to_owned();
        }
    }
    let elsewhere = env
        .manager
        .insert_test_job(elsewhere, true)
        .map_err(ctx("insert elsewhere"))?;
    for id in [&unproven, &elsewhere] {
        match env.manager.suspend(id, Caller::Agent("worker")) {
            Err(JobError::NoJobCgroup(job)) => assert_eq!(&job, id.as_str()),
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    // Ended, detached (not supervised; this stub has no process, so the
    // refresh before the check finds it gone) or not monitored: never
    // suspended.
    for (name, state, monitored, seen) in [
        ("job-ended", JobState::Stopped, true, JobState::Stopped),
        ("job-detached", JobState::Detached, false, JobState::Unknown),
        (
            "job-unmonitored",
            JobState::Running,
            false,
            JobState::Running,
        ),
    ] {
        let meta = stub_job_meta(name, state, "worker", &[], Some("f1"))?;
        let id = env
            .manager
            .insert_test_job(meta, monitored)
            .map_err(ctx("insert"))?;
        match env.manager.suspend(&id, Caller::Agent("worker")) {
            Err(JobError::NotSuspendable { job, state }) => {
                assert_eq!(job, name);
                assert_eq!(state, seen, "{name}");
            }
            other => return Err(TestError::Unexpected(format!("{name}: {other:?}"))),
        }
    }
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    for id in [&unproven, &elsewhere] {
        let status = status_of(&env, id)?;
        assert_eq!(status.meta.state, JobState::Running);
        assert_eq!(status.meta.suspended_at, None);
    }
    Ok(())
}

/// A failing freeze is a typed error; the record written before the freeze
/// (intent before effect) is taken back, in memory and on disk.
#[test]
fn freeze_failure_is_typed_and_rolls_the_record_back() -> TestResult {
    let fake = Arc::new(FakeCgroup::denying());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-denied", "g1")?;
    match env.manager.suspend(&id, Caller::Agent("worker")) {
        Err(JobError::Freeze {
            job,
            operation,
            source,
        }) => {
            assert_eq!(job, "job-denied");
            assert_eq!(operation, "freeze");
            assert_eq!(source, FreezeError::PermissionDenied);
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(fake.calls(), vec![call("freeze", "g1")]);
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Running);
    assert_eq!(status.meta.suspended_at, None);
    assert_eq!(status.meta.suspended_secs, 0);
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Running);
    assert_eq!(persisted.suspended_at, None);
    assert_eq!(persisted.suspend_phase, None);
    Ok(())
}

/// A stop never thaws a suspended job (a contained job must not run again,
/// A1): whatever signal was asked for, it gets SIGKILL while frozen. The
/// stub job has no process, so only the decision and the absence of a thaw
/// show.
#[tokio::test]
async fn stop_kills_a_suspended_job_while_frozen_without_thawing() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-stop", "h1")?;
    let worker = Caller::Agent("worker");

    env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    for requested in [
        JobSignal::Term,
        JobSignal::Int,
        JobSignal::Hup,
        JobSignal::Kill,
    ] {
        let (status, sent) = env
            .manager
            .stop_reporting_signal(&id, worker, requested)
            .await
            .map_err(ctx("stop"))?;
        assert_eq!(sent, JobSignal::Kill, "{}", requested.name());
        assert_eq!(status.meta.state, JobState::Suspended);
        assert!(status.meta.is_suspended());
    }
    assert_eq!(fake.calls(), vec![call("freeze", "h1")]);

    // A job that is not suspended gets the signal it was asked for.
    let plain = running_job(&env, "job-plain", "h2")?;
    let (_, sent) = env
        .manager
        .stop_reporting_signal(&plain, worker, JobSignal::Term)
        .await
        .map_err(ctx("stop plain"))?;
    assert_eq!(sent, JobSignal::Term);
    assert_eq!(
        stop_signal_for(&status_of(&env, &plain)?.meta, JobSignal::Hup),
        JobSignal::Hup
    );
    Ok(())
}

/// A detached, still suspended job that is stopped ends while frozen: it is
/// killed (never thawed), and the end closes its suspension and books the
/// suspended time. That holds for an unresolved suspension too (here a
/// record from before the phase), which `job.resume` refuses: `job.stop`
/// is the way to end it.
#[tokio::test]
async fn stopping_a_detached_suspended_job_kills_it_and_closes_the_suspension() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let sleeper = Sleeper::spawn()?;
    let id = detached_suspended_job(&env, "job-detached-stop", "k1", &sleeper, 30, None)?;
    assert!(status_of(&env, &id)?.meta.is_suspension_unresolved());
    match env.manager.resume(&id, Caller::Agent("worker")) {
        Err(JobError::SuspensionUnresolved(job)) => assert_eq!(job, "job-detached-stop"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }

    let (status, sent) = env
        .manager
        .stop_reporting_signal(&id, Caller::Agent("worker"), JobSignal::Term)
        .await
        .map_err(ctx("stop"))?;
    assert_eq!(sent, JobSignal::Kill);
    assert_eq!(status.meta.state, JobState::Stopped);
    assert!(status.meta.ended_at.is_some());
    assert_eq!(status.meta.suspended_at, None);
    assert!(
        status.meta.suspended_secs >= 30,
        "{}",
        status.meta.suspended_secs
    );
    assert!(!status.meta.is_suspended());
    assert_eq!(status.meta.suspend_phase, None);
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Stopped);
    assert_eq!(persisted.suspended_at, None);
    assert_eq!(persisted.suspend_phase, None);
    Ok(())
}

/// A supervised job that is suspended and then stopped with TERM ends by
/// SIGKILL (never thawed), and its monitor closes the suspension at the
/// end. The suspension is recorded directly (a job started here has no
/// proven cgroup to freeze), so the real process runs unfrozen.
#[tokio::test]
async fn a_suspended_job_ends_by_sigkill_and_its_monitor_closes_the_suspension() -> TestResult {
    let env = Env::new()?;
    let prepared = env.prepare("sleep 30").await?;
    let started = env
        .manager
        .start(request("frozen-end", "agent-a", &[]), prepared)
        .await
        .map_err(ctx("start"))?;
    let id = started.meta.job_id.clone();
    {
        let entry = env
            .manager
            .entry(&id, Caller::Operator)
            .map_err(ctx("entry"))?;
        let mut state = lock(&entry.state);
        state.meta.state = JobState::Suspended;
        state.meta.suspended_at = Some(ago(30)?);
    }

    let (status, sent) = env
        .manager
        .stop_reporting_signal(&id, Caller::Agent("agent-a"), JobSignal::Term)
        .await
        .map_err(ctx("stop"))?;
    assert_eq!(sent, JobSignal::Kill);
    assert_eq!(status.meta.state, JobState::Stopped);
    assert!(status.meta.stop_requested);
    assert!(status.meta.ended_at.is_some());
    assert_eq!(status.meta.suspended_at, None);
    assert!(
        status.meta.suspended_secs >= 30,
        "{}",
        status.meta.suspended_secs
    );
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Stopped);
    assert_eq!(persisted.suspended_at, None);
    assert!(
        persisted.suspended_secs >= 30,
        "{}",
        persisted.suspended_secs
    );
    Ok(())
}

/// `job.resume` on a detached job that is still suspended (suspended, then
/// detached; or from a previous session) thaws its proven cgroup, books the
/// suspended time and keeps it `detached`.
#[test]
fn resume_thaws_a_detached_suspended_job_and_keeps_it_detached() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let sleeper = Sleeper::spawn()?;
    let id = detached_suspended_job(
        &env,
        "job-detached-resume",
        "r1",
        &sleeper,
        30,
        Some(SuspendPhase::Applied),
    )?;
    assert!(status_of(&env, &id)?.meta.is_suspended());

    let resumed = env
        .manager
        .resume(&id, Caller::Operator)
        .map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(resumed.status.meta.state, JobState::Detached);
    assert_eq!(resumed.status.meta.suspended_at, None);
    assert!(
        resumed.status.meta.suspended_secs >= 30,
        "{}",
        resumed.status.meta.suspended_secs
    );
    assert_eq!(fake.calls(), vec![call("thaw", "r1")]);
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Detached);
    assert_eq!(persisted.suspended_at, None);
    assert_eq!(persisted.suspend_phase, None);
    Ok(())
}

/// `detach_all` keeps a suspended job suspended: `meta.json` says
/// `detached` with the open suspension, so the next session shows it as
/// suspended (B13).
#[test]
fn detach_all_keeps_a_suspended_job_suspended() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-detach", "l1")?;
    let suspended = env
        .manager
        .suspend(&id, Caller::Agent("worker"))
        .map_err(ctx("suspend"))?;
    let since = suspended.status.meta.suspended_at;
    assert!(since.is_some());

    assert_eq!(env.manager.detach_all().detached, 1);
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Detached);
    assert!(persisted.detached);
    assert_eq!(persisted.suspended_at, since);
    assert!(persisted.is_suspended());
    // Still harw's own suspension: the next session may resume it.
    assert_eq!(persisted.suspend_phase, Some(SuspendPhase::Applied));
    // Nothing was thawed on the way out.
    assert_eq!(fake.calls(), vec![call("freeze", "l1")]);
    Ok(())
}

/// A suspended job whose process vanished becomes `unknown`: the open
/// suspension is closed and booked (it never stays "suspended" without a
/// process), and the active time is unknown instead of growing.
#[test]
fn a_vanished_suspended_job_closes_its_suspension() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    // Detached, suspended, no process: the next refresh finds it gone.
    let mut meta = stub_job_meta(
        "job-vanished",
        JobState::Detached,
        "worker",
        &[],
        Some("v1"),
    )?;
    meta.started_at = Some(ago(60)?);
    meta.suspended_at = Some(ago(30)?);
    meta.suspend_phase = Some(SuspendPhase::Applied);
    let id = env
        .manager
        .insert_test_job(meta, false)
        .map_err(ctx("insert"))?;

    let outcome = env
        .manager
        .resume(&id, Caller::Agent("worker"))
        .map_err(ctx("resume"))?;
    assert_eq!(outcome.transition, SuspendTransition::NotSuspended);
    assert_eq!(outcome.frozen, None);
    let meta = &outcome.status.meta;
    assert_eq!(meta.state, JobState::Unknown);
    assert_eq!(meta.suspended_at, None);
    assert!(meta.suspended_secs >= 30, "{}", meta.suspended_secs);
    assert_eq!(meta.active_secs_at(Timestamp::now()), None);
    assert_eq!(meta.suspend_phase, None);
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Unknown);
    assert_eq!(persisted.suspended_at, None);
    assert_eq!(persisted.suspend_phase, None);
    Ok(())
}

/// The same after a restart: a job recorded as suspended whose process is
/// gone loads as `unknown` with its suspension closed.
#[test]
fn reload_closes_the_suspension_of_a_vanished_job() -> TestResult {
    let env = Env::new()?;
    let mut meta = stub_job_meta("job-gone", JobState::Suspended, "old-session", &[], None)?;
    meta.started_at = Some(ago(60)?);
    meta.suspended_at = Some(ago(30)?);
    meta.suspend_phase = Some(SuspendPhase::Intent);
    let dir = env.state_dir().join("jobs").join(meta.job_id.as_str());
    fs::create_dir_all(&dir).map_err(ctx("create old job dir"))?;
    write_meta(&dir, &meta).map_err(ctx("write old meta"))?;
    let id = meta.job_id.clone();

    let reloaded = JobManager::new(
        env.manager.config().clone(),
        Arc::new(RecordingNotifier::default()) as Arc<dyn JobNotifier>,
    )
    .map_err(ctx("reload"))?;
    let status = reloaded
        .status(&id, Caller::Operator)
        .map_err(ctx("status"))?;
    assert_eq!(status.meta.state, JobState::Unknown);
    assert_eq!(status.meta.suspended_at, None);
    assert!(
        status.meta.suspended_secs >= 30,
        "{}",
        status.meta.suspended_secs
    );
    assert!(!status.meta.is_suspended());
    assert_eq!(status.meta.suspend_phase, None);
    let persisted = read_meta(&dir).ok_or(TestError::Missing("meta.json"))?;
    assert_eq!(persisted.state, JobState::Unknown);
    assert_eq!(persisted.suspended_at, None);
    assert_eq!(persisted.suspend_phase, None);
    Ok(())
}

/// Suspended time is booked when the job resumes: it never counts as active
/// time (wall-clock semantics, see `crate::model`).
#[test]
fn resume_books_the_suspended_time() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let mut meta = stub_job_meta("job-clock", JobState::Running, "worker", &[], Some("i1"))?;
    let start = Timestamp::now();
    meta.started_at = Some(start);
    let id = env
        .manager
        .insert_test_job(meta, true)
        .map_err(ctx("insert"))?;
    env.manager
        .suspend(&id, Caller::Agent("worker"))
        .map_err(ctx("suspend"))?;
    // Pretend the suspension began 30 s ago (the stub has no real clock).
    {
        let entry = env
            .manager
            .entry(&id, Caller::Operator)
            .map_err(ctx("entry"))?;
        let mut state = lock(&entry.state);
        state.meta.started_at = Some(
            start
                .checked_sub(jiff::SignedDuration::from_secs(40))
                .map_err(ctx("start"))?,
        );
        state.meta.suspended_at = Some(
            start
                .checked_sub(jiff::SignedDuration::from_secs(30))
                .map_err(ctx("suspended at"))?,
        );
    }
    let resumed = env
        .manager
        .resume(&id, Caller::Agent("worker"))
        .map_err(ctx("resume"))?;
    let meta = &resumed.status.meta;
    assert!(meta.suspended_secs >= 30, "{}", meta.suspended_secs);
    // 40 s since the start, of which at least 30 s suspended.
    let active = meta
        .active_secs_at(Timestamp::now())
        .ok_or(TestError::Missing("active secs"))?;
    assert!(active <= 12, "{active}");
    Ok(())
}

/// A job that was suspended when harw ended (harw's own, recorded freeze)
/// stays visibly suspended after the restart (B13), its owner is told, and
/// `job.resume` without a proven cgroup fails closed. The stub's process is
/// this test process, so the test never stops it.
#[test]
fn a_suspended_job_of_a_previous_session_stays_visible() -> TestResult {
    let env = Env::new()?;
    let own_pid = std::process::id();
    let mut meta = stub_job_meta(
        "job-old-suspended",
        JobState::Suspended,
        "old-session",
        &[],
        None,
    )?;
    meta.pid = Some(own_pid);
    meta.proc_start_ticks = crate::procfs::test_start_ticks(own_pid);
    meta.suspended_at = Some(Timestamp::now());
    meta.suspend_phase = Some(SuspendPhase::Applied);
    let dir = env.state_dir().join("jobs").join(meta.job_id.as_str());
    fs::create_dir_all(&dir).map_err(ctx("create old job dir"))?;
    write_meta(&dir, &meta).map_err(ctx("write old meta"))?;
    let id = meta.job_id.clone();

    let recorder = Arc::new(RecordingNotifier::default());
    let reloaded = JobManager::new(
        env.manager.config().clone(),
        Arc::clone(&recorder) as Arc<dyn JobNotifier>,
    )
    .map_err(ctx("reload"))?;
    let status = reloaded
        .status(&id, Caller::Operator)
        .map_err(ctx("status"))?;
    assert_eq!(status.meta.state, JobState::Detached);
    assert!(status.meta.is_suspended());
    let warned = recorder.notifications().into_iter().any(|notification| {
        matches!(
            &notification.event,
            JobEvent::Warning { job_id, message, .. }
                if *job_id == id && message == SUSPENDED_ON_RELOAD
        )
    });
    assert!(warned, "{:?}", recorder.notifications());

    let owner = Caller::Agent("old-session");
    assert!(matches!(
        reloaded.resume(&id, owner),
        Err(JobError::SuspendUnavailable)
    ));
    let fake = Arc::new(FakeCgroup::default());
    reloaded.set_freezer(freezer(&fake));
    assert!(matches!(
        reloaded.resume(&id, owner),
        Err(JobError::NoJobCgroup(_))
    ));
    // Without a proven cgroup only the record remains: still suspended,
    // kernel view unknown, nothing written.
    let again = reloaded.suspend(&id, owner).map_err(ctx("suspend"))?;
    assert_eq!(again.transition, SuspendTransition::AlreadySuspended);
    assert_eq!(again.frozen, None);
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    Ok(())
}

// ── Races with the end of the job ────────────────────────────────────────────

/// A freezer that changes the job right at the kernel write (as its
/// monitor or `detach_all` would in between), then freezes through the
/// fake backend.
struct RacingFreezer {
    inner: Arc<dyn JobFreezer>,
    entry: Mutex<Option<Arc<JobEntry>>>,
    race: fn(&mut EntryState),
}

impl JobFreezer for RacingFreezer {
    fn delegated_root(&self) -> &str {
        self.inner.delegated_root()
    }

    fn freeze(&self, leaf: &str) -> Result<(), FreezeError> {
        if let Some(entry) = lock(&self.entry).take() {
            let mut state = lock(&entry.state);
            (self.race)(&mut state);
        }
        self.inner.freeze(leaf)
    }

    fn thaw(&self, leaf: &str) -> Result<(), FreezeError> {
        self.inner.thaw(leaf)
    }

    fn is_frozen(&self, leaf: &str) -> Result<bool, FreezeError> {
        self.inner.is_frozen(leaf)
    }

    fn freeze_requested(&self, leaf: &str) -> Result<bool, FreezeError> {
        self.inner.freeze_requested(leaf)
    }

    fn containment_held(&self, leaf: &str) -> Result<bool, FreezeError> {
        self.inner.containment_held(leaf)
    }

    fn containment_lease(&self) -> Result<ContainmentLease, FreezeError> {
        self.inner.containment_lease()
    }
}

/// What the monitor does when the job ends (here: stopped).
fn end_job(state: &mut EntryState) {
    let now = Timestamp::now();
    state.meta.state = JobState::Stopped;
    state.meta.ended_at = Some(now);
    state.meta.end_suspension(now);
}

/// What `detach_all` does to a supervised job.
fn detach_job(state: &mut EntryState) {
    state.monitored = false;
    state.meta.detached = true;
    state.meta.state = JobState::Detached;
}

/// `Env` with a [`RacingFreezer`] armed for the running stub job `id`; the
/// Warden's containment index is `holds`.
fn racing_env(
    fake: &Arc<FakeCgroup>,
    holds: &Arc<FakeHolds>,
    id: &str,
    attempt: &str,
    race: fn(&mut EntryState),
) -> TestResult<(Env, JobId)> {
    let env = Env::new()?;
    let job = running_job(&env, id, attempt)?;
    let racing = racing_freezer(&env, &job, freezer_with_holds(fake, holds), race)?;
    env.manager.set_freezer(Arc::new(racing));
    Ok((env, job))
}

/// A [`RacingFreezer`] over `inner`, armed for the job `job` of `env`.
fn racing_freezer(
    env: &Env,
    job: &JobId,
    inner: Arc<dyn JobFreezer>,
    race: fn(&mut EntryState),
) -> TestResult<RacingFreezer> {
    let entry = env
        .manager
        .entry(job, Caller::Operator)
        .map_err(ctx("entry"))?;
    Ok(RacingFreezer {
        inner,
        entry: Mutex::new(Some(entry)),
        race,
    })
}

/// The job ends between the record and the freeze: the freeze is undone at
/// once (no frozen cgroup outlives a refusal) and the refusal is typed.
#[test]
fn a_job_that_ends_while_being_frozen_is_thawed_again() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let holds = Arc::new(FakeHolds::default());
    let (env, id) = racing_env(&fake, &holds, "job-race-end", "q1", end_job)?;
    match env.manager.suspend(&id, Caller::Agent("worker")) {
        Err(JobError::NotSuspendable { job, state }) => {
            assert_eq!(job, "job-race-end");
            assert_eq!(state, JobState::Stopped);
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(fake.calls(), vec![call("freeze", "q1"), call("thaw", "q1")]);
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Stopped);
    assert_eq!(status.meta.suspended_at, None);
    assert!(!status.meta.is_suspended());
    // The closed suspension never became harw's own.
    assert_eq!(status.meta.suspend_phase, None);
    Ok(())
}

/// The job is detached between the record and the freeze: it stays frozen
/// and visibly suspended (`detached` with `suspended_at`), so `job.resume`
/// can thaw it later.
#[test]
fn a_job_detached_while_being_frozen_stays_suspended() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let holds = Arc::new(FakeHolds::default());
    let (env, id) = racing_env(&fake, &holds, "job-race-detach", "q2", detach_job)?;
    let outcome = env
        .manager
        .suspend(&id, Caller::Agent("worker"))
        .map_err(ctx("suspend"))?;
    assert_eq!(outcome.transition, SuspendTransition::Suspended);
    assert_eq!(outcome.status.meta.state, JobState::Detached);
    assert!(outcome.status.meta.suspended_at.is_some());
    assert!(outcome.status.meta.is_suspended());
    assert_eq!(fake.calls(), vec![call("freeze", "q2")]);
    // The record was written before the freeze, and its effect after it:
    // the detached job's suspension is harw's own.
    let persisted = on_disk(&env, &id)?;
    assert!(persisted.suspended_at.is_some());
    assert_eq!(persisted.state, JobState::Detached);
    assert_eq!(persisted.suspend_phase, Some(SuspendPhase::Applied));
    Ok(())
}

// ── Containment outside harw after harw's suspension (review finding A) ─────

/// `Env` with the fake freezer reading the containment index `holds`.
fn env_with_holds(fake: &Arc<FakeCgroup>, holds: &Arc<FakeHolds>) -> TestResult<Env> {
    let env = Env::new()?;
    env.manager.set_freezer(freezer_with_holds(fake, holds));
    Ok(env)
}

/// The Warden contains `harw-job-<attempt>`: marker first, then
/// `cgroup.freeze` = 1 (the bit harw may already have set).
fn warden_freezes(fake: &FakeCgroup, holds: &FakeHolds, attempt: &str) {
    let leaf = format!("harw-job-{attempt}");
    holds.hold(fake_kernel_id(&leaf));
    fake.set_outside(&leaf, true, true);
}

/// One-shot side effect of a [`HookFreezer`].
type Hook = Box<dyn FnOnce() + Send>;

/// A freezer that runs a one-shot hook right before the kernel write
/// (`freeze` or `thaw`) or the lease of the containment index, e.g. the
/// Warden containing the cgroup or the state directory turning unwritable
/// at that moment, then delegates.
struct HookFreezer {
    inner: Arc<dyn JobFreezer>,
    before_freeze: Mutex<Option<Hook>>,
    before_thaw: Mutex<Option<Hook>>,
    before_lease: Mutex<Option<Hook>>,
}

impl HookFreezer {
    fn new(inner: Arc<dyn JobFreezer>) -> Self {
        Self {
            inner,
            before_freeze: Mutex::new(None),
            before_thaw: Mutex::new(None),
            before_lease: Mutex::new(None),
        }
    }

    /// Runs `hook` once, right before the next `freeze`.
    fn before_freeze(self, hook: impl FnOnce() + Send + 'static) -> Self {
        *lock(&self.before_freeze) = Some(Box::new(hook));
        self
    }

    /// Runs `hook` once, right before the next `thaw`.
    fn before_thaw(self, hook: impl FnOnce() + Send + 'static) -> Self {
        *lock(&self.before_thaw) = Some(Box::new(hook));
        self
    }

    /// Runs `hook` once, right before the next `containment_lease` (in
    /// `job.resume`: after the release record, before any thaw).
    fn before_lease(self, hook: impl FnOnce() + Send + 'static) -> Self {
        *lock(&self.before_lease) = Some(Box::new(hook));
        self
    }
}

/// Takes the hook out of `slot` (the lock is released before it runs) and
/// runs it.
fn run_hook(slot: &Mutex<Option<Hook>>) {
    let hook = lock(slot).take();
    if let Some(hook) = hook {
        hook();
    }
}

impl JobFreezer for HookFreezer {
    fn delegated_root(&self) -> &str {
        self.inner.delegated_root()
    }

    fn freeze(&self, leaf: &str) -> Result<(), FreezeError> {
        run_hook(&self.before_freeze);
        self.inner.freeze(leaf)
    }

    fn thaw(&self, leaf: &str) -> Result<(), FreezeError> {
        run_hook(&self.before_thaw);
        self.inner.thaw(leaf)
    }

    fn is_frozen(&self, leaf: &str) -> Result<bool, FreezeError> {
        self.inner.is_frozen(leaf)
    }

    fn freeze_requested(&self, leaf: &str) -> Result<bool, FreezeError> {
        self.inner.freeze_requested(leaf)
    }

    fn containment_held(&self, leaf: &str) -> Result<bool, FreezeError> {
        self.inner.containment_held(leaf)
    }

    fn containment_lease(&self) -> Result<ContainmentLease, FreezeError> {
        run_hook(&self.before_lease);
        self.inner.containment_lease()
    }
}

/// Review finding A: harw suspends, the Warden contains the same cgroup
/// afterwards (one shared freeze bit). `job.resume` must not lift that
/// containment: nothing is thawed and the job stays suspended, in memory
/// and on disk. Once the Warden released it (freeze bit, then marker),
/// `job.resume` works again.
#[test]
fn a_warden_freeze_after_suspend_is_not_lifted_by_resume() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let holds = Arc::new(FakeHolds::default());
    let env = env_with_holds(&fake, &holds)?;
    let id = running_job(&env, "job-contained", "z1")?;
    let worker = Caller::Agent("worker");

    env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    warden_freezes(&fake, &holds, "z1");
    for caller in [worker, Caller::Agent("orchestrator"), Caller::Operator] {
        match env.manager.resume(&id, caller) {
            Err(JobError::ContainmentHeld(job)) => assert_eq!(job, "job-contained"),
            other => return Err(TestError::Unexpected(format!("{caller:?}: {other:?}"))),
        }
    }
    // No thaw was written; the Warden's freeze is still in place.
    assert_eq!(fake.calls(), vec![call("freeze", "z1")]);
    let probe = freezer_with_holds(&fake, &holds);
    assert_eq!(probe.freeze_requested("harw-job-z1"), Ok(true));
    assert_eq!(probe.is_frozen("harw-job-z1"), Ok(true));
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Suspended);
    assert!(status.meta.suspended_at.is_some());
    // harw's freeze still holds: the release before the thaw was taken
    // back, harw's own suspension stays recorded.
    assert_eq!(status.meta.suspension_phase(), Some(SuspendPhase::Applied));
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert!(persisted.suspended_at.is_some());
    assert_eq!(persisted.suspend_phase, Some(SuspendPhase::Applied));

    // The Warden releases: thaw first, then the marker.
    fake.set_outside("harw-job-z1", false, false);
    holds.release(fake_kernel_id("harw-job-z1"));
    let resumed = env
        .manager
        .resume(&id, worker)
        .map_err(ctx("resume after the release"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(resumed.status.meta.state, JobState::Running);
    assert_eq!(fake.calls(), vec![call("freeze", "z1"), call("thaw", "z1")]);
    assert_eq!(on_disk(&env, &id)?.state, JobState::Running);
    Ok(())
}

/// A Warden that did not wait for harw's lease (it waits only briefly, then
/// contains without its lock) contains the cgroup in the window between
/// harw's check and harw's thaw: the thaw lifts it, but the check right
/// after the thaw sees the marker (written before the Warden froze) and
/// freezes again at once. The job stays suspended and the refusal is typed.
#[test]
fn a_warden_freeze_racing_the_resume_is_restored_at_once() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let holds = Arc::new(FakeHolds::default());
    let env = Env::new()?;
    let id = running_job(&env, "job-race-warden", "z2")?;
    let warden = {
        let (fake, holds) = (Arc::clone(&fake), Arc::clone(&holds));
        move || warden_freezes(&fake, &holds, "z2")
    };
    env.manager.set_freezer(Arc::new(
        HookFreezer::new(freezer_with_holds(&fake, &holds)).before_thaw(warden),
    ));
    let worker = Caller::Agent("worker");

    env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    match env.manager.resume(&id, worker) {
        Err(JobError::ContainmentHeld(job)) => assert_eq!(job, "job-race-warden"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    // Thawed once (lifting the Warden's freeze), then frozen again at once.
    assert_eq!(
        fake.calls(),
        vec![
            call("freeze", "z2"),
            call("thaw", "z2"),
            call("freeze", "z2")
        ]
    );
    let probe = freezer_with_holds(&fake, &holds);
    assert_eq!(probe.freeze_requested("harw-job-z2"), Ok(true));
    assert_eq!(probe.is_frozen("harw-job-z2"), Ok(true));
    assert_restored_applied(&env, &id)?;
    Ok(())
}

/// After a `job.resume` that thawed nothing (or froze again at once), `id`
/// is still suspended as harw's own (`applied`), in memory and on disk: the
/// release written before the thaw was taken back, so a restart would not
/// turn the refused resume into an unresolved suspension.
fn assert_restored_applied(env: &Env, id: &JobId) -> TestResult {
    let status = status_of(env, id)?;
    assert_eq!(status.meta.state, JobState::Suspended);
    assert!(status.meta.suspended_at.is_some());
    assert_eq!(status.meta.suspension_phase(), Some(SuspendPhase::Applied));
    let persisted = on_disk(env, id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert_eq!(persisted.suspended_at, status.meta.suspended_at);
    assert_eq!(persisted.suspend_phase, Some(SuspendPhase::Applied));
    Ok(())
}

/// Fail closed: while the containment index cannot be read, harw cannot
/// tell its own freeze from a Warden containment and thaws nothing; the job
/// stays suspended, harw's own (`applied`) in memory and on disk. A
/// readable index lets `job.resume` work again.
#[test]
fn an_unreadable_containment_index_fails_closed() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let holds = Arc::new(FakeHolds::default());
    let env = env_with_holds(&fake, &holds)?;
    let id = running_job(&env, "job-blind", "z3")?;
    let worker = Caller::Agent("worker");

    env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    holds.set_unreadable(true);
    match env.manager.resume(&id, worker) {
        Err(JobError::Freeze {
            job,
            operation,
            source,
        }) => {
            assert_eq!(job, "job-blind");
            assert_eq!(operation, "probe");
            assert!(
                matches!(source, FreezeError::ContainmentIndexUnreadable { .. }),
                "{source:?}"
            );
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(fake.calls(), vec![call("freeze", "z3")]);
    assert_restored_applied(&env, &id)?;

    holds.set_unreadable(false);
    let resumed = env.manager.resume(&id, worker).map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(fake.calls(), vec![call("freeze", "z3"), call("thaw", "z3")]);
    Ok(())
}

/// The index turns unreadable right after the thaw: harw cannot rule out a
/// containment that came in between, so it freezes again (fail closed) and
/// the job stays suspended, harw's own (`applied`) in memory and on disk.
#[test]
fn an_index_unreadable_after_the_thaw_restores_the_freeze() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let holds = Arc::new(FakeHolds::default());
    let env = Env::new()?;
    let id = running_job(&env, "job-blind-later", "z4")?;
    let blind = {
        let holds = Arc::clone(&holds);
        move || holds.set_unreadable(true)
    };
    env.manager.set_freezer(Arc::new(
        HookFreezer::new(freezer_with_holds(&fake, &holds)).before_thaw(blind),
    ));
    let worker = Caller::Agent("worker");

    env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    match env.manager.resume(&id, worker) {
        Err(JobError::Freeze {
            operation, source, ..
        }) => {
            assert_eq!(operation, "probe");
            assert!(
                matches!(source, FreezeError::ContainmentIndexUnreadable { .. }),
                "{source:?}"
            );
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(
        fake.calls(),
        vec![
            call("freeze", "z4"),
            call("thaw", "z4"),
            call("freeze", "z4")
        ]
    );
    // Frozen again at once: harw's freeze holds, so the release before the
    // thaw is taken back (`applied`), in memory and on disk.
    assert_restored_applied(&env, &id)?;
    Ok(())
}

/// The job ends between the record and the freeze while the Warden starts
/// containing the same cgroup (its marker is written right before harw's
/// freeze, its own freeze follows): the undo of harw's freeze must not lift
/// that containment, so nothing is thawed. The refusal stays typed.
#[test]
fn a_job_ending_while_being_frozen_is_not_thawed_under_a_warden_hold() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let holds = Arc::new(FakeHolds::default());
    let env = Env::new()?;
    let id = running_job(&env, "job-race-held", "q3")?;
    let racing = racing_freezer(&env, &id, freezer_with_holds(&fake, &holds), end_job)?;
    env.manager.set_freezer(Arc::new(
        HookFreezer::new(Arc::new(racing)).before_freeze(warden_marks(&holds, "q3")),
    ));
    match env.manager.suspend(&id, Caller::Agent("worker")) {
        Err(JobError::NotSuspendable { job, state }) => {
            assert_eq!(job, "job-race-held");
            assert_eq!(state, JobState::Stopped);
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(fake.calls(), vec![call("freeze", "q3")]);
    Ok(())
}

// ── Durable intent before the freeze (review finding B) ────────────────────

/// The path whose existence as a directory makes every `meta.json` write
/// of `id` fail (`write_meta` writes `meta.json.tmp` first), also as root.
fn record_blocker(env: &Env, id: &JobId) -> TestResult<std::path::PathBuf> {
    let dir = env
        .manager
        .log_dir(id, Caller::Operator)
        .map_err(ctx("log dir"))?;
    Ok(dir.join(format!("{META_FILE}.tmp")))
}

/// Review finding B: an unwritable state directory (full or read-only
/// volume) aborts `job.suspend` before anything is frozen: no durable
/// record, no freeze. The disk keeps the previous record and the job runs
/// on; with nothing renamed, the job directory is not synced either. Once
/// the directory is writable again, `job.suspend` works.
#[test]
fn an_unwritable_state_directory_aborts_suspend_before_freezing() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-readonly", "y1")?;
    let sync = ScriptedDirSync::new(&env, &fake, &id, 0)?.install(&env);
    let blocker = record_blocker(&env, &id)?;
    fs::create_dir(&blocker).map_err(ctx("block meta.json"))?;

    match env.manager.suspend(&id, Caller::Agent("worker")) {
        Err(JobError::SuspendNotRecorded { job, .. }) => assert_eq!(job, "job-readonly"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert!(sync.calls().is_empty(), "{:?}", sync.calls());
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Running);
    assert_eq!(status.meta.suspended_at, None);
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Running);
    assert_eq!(persisted.suspended_at, None);

    fs::remove_dir(&blocker).map_err(ctx("unblock meta.json"))?;
    let suspended = env
        .manager
        .suspend(&id, Caller::Agent("worker"))
        .map_err(ctx("suspend"))?;
    assert_eq!(suspended.transition, SuspendTransition::Suspended);
    assert_eq!(sync.calls(), synced_intent_then_applied());
    assert_eq!(fake.calls(), vec![call("freeze", "y1")]);
    assert!(on_disk(&env, &id)?.suspended_at.is_some());
    Ok(())
}

/// The intent was recorded, the freeze fails, and the state directory has
/// turned unwritable in between: the rollback cannot be written. That is
/// surfaced as its own typed error (with the freeze failure as source);
/// in memory the job runs on, and the disk still says suspended.
#[test]
fn a_failed_rollback_of_the_record_is_surfaced() -> TestResult {
    let fake = Arc::new(FakeCgroup::denying());
    let env = Env::new()?;
    let id = running_job(&env, "job-stuck-record", "y2")?;
    let blocker = record_blocker(&env, &id)?;
    let block = {
        let blocker = blocker.clone();
        move || {
            // A failure here shows as a plain `Freeze` error below, which
            // the match rejects; nothing is lost by not returning it.
            let _ = fs::create_dir(&blocker);
        }
    };
    env.manager.set_freezer(Arc::new(
        HookFreezer::new(freezer(&fake)).before_freeze(block),
    ));

    match env.manager.suspend(&id, Caller::Agent("worker")) {
        Err(JobError::RollbackNotRecorded { job, source, .. }) => {
            assert_eq!(job, "job-stuck-record");
            assert_eq!(source, FreezeError::PermissionDenied);
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert!(blocker.is_dir());
    assert_eq!(fake.calls(), vec![call("freeze", "y2")]);
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Running);
    assert_eq!(status.meta.suspended_at, None);
    assert_eq!(status.meta.suspend_phase, None);
    // The intent reached the disk before the freeze; its rollback did not.
    // Only the intent: a restart finds an unresolved suspension.
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert!(persisted.suspended_at.is_some());
    assert_eq!(persisted.suspend_phase, Some(SuspendPhase::Intent));
    Ok(())
}

// ── Review round 1: suspend vs. a held index, stale writes, lease ──────────

/// Polls `done` every millisecond for at most five seconds.
fn wait_until(mut done: impl FnMut() -> bool) -> TestResult {
    for _ in 0..5_000 {
        if done() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Err(TestError::Unexpected(
        "condition not reached within five seconds".to_owned(),
    ))
}

/// A Warden marker that already exists when harw suspends (the Warden is
/// between its marker and its freeze, or the marker is stale): harw takes
/// no share of that freeze bit, so the Warden's later release can never
/// silently lift a suspension of harw. Nothing is frozen or recorded.
///
/// Review round 2: an unreadable index does not stop `job.suspend`
/// (freezing is the safe direction, and refusing protects nothing because
/// `job.resume` asks the index again). It is the resume that fails closed
/// while the index stays unreadable; once it is readable, it thaws.
#[test]
fn suspend_refuses_a_held_cgroup_but_not_an_unreadable_index() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let holds = Arc::new(FakeHolds::default());
    let env = env_with_holds(&fake, &holds)?;
    let id = running_job(&env, "job-pre-held", "z6")?;
    let worker = Caller::Agent("worker");

    holds.hold(fake_kernel_id("harw-job-z6"));
    match env.manager.suspend(&id, worker) {
        Err(JobError::FrozenElsewhere(job)) => assert_eq!(job, "job-pre-held"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Running);
    assert_eq!(status.meta.suspended_at, None);
    assert_eq!(on_disk(&env, &id)?.suspended_at, None);

    holds.release(fake_kernel_id("harw-job-z6"));
    holds.set_unreadable(true);
    let suspended = env
        .manager
        .suspend(&id, worker)
        .map_err(ctx("suspend with an unreadable index"))?;
    assert_eq!(suspended.transition, SuspendTransition::Suspended);
    assert_eq!(fake.calls(), vec![call("freeze", "z6")]);
    assert!(on_disk(&env, &id)?.suspended_at.is_some());

    // The resume cannot rule out a containment: it thaws nothing.
    match env.manager.resume(&id, worker) {
        Err(JobError::Freeze {
            operation, source, ..
        }) => {
            assert_eq!(operation, "probe");
            assert!(
                matches!(source, FreezeError::ContainmentIndexUnreadable { .. }),
                "{source:?}"
            );
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(fake.calls(), vec![call("freeze", "z6")]);
    assert_restored_applied(&env, &id)?;

    holds.set_unreadable(false);
    let resumed = env.manager.resume(&id, worker).map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(fake.calls(), vec![call("freeze", "z6"), call("thaw", "z6")]);
    Ok(())
}

/// The monitor writes `meta.json` concurrently (progress, identity, end).
/// A write whose snapshot predates the suspension must never land after
/// `job.suspend`'s durable record, or a restart would find a frozen job
/// without harw's record. Here the monitor's write holds the write lock and
/// has taken its snapshot when `job.suspend` marks the job: `job.suspend`
/// waits for it and writes afterwards, before it freezes.
#[test]
fn a_stale_monitor_write_never_overwrites_the_suspension_record() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-stale-write", "y3")?;
    let entry = env
        .manager
        .entry(&id, Caller::Operator)
        .map_err(ctx("entry"))?;
    let manager = &env.manager;
    let job = &id;

    // The monitor is in the middle of a write: lock taken, snapshot taken
    // (still running), file not yet written.
    let writing = lock(&entry.persist_lock);
    let stale = entry.meta();
    let outcome = std::thread::scope(|scope| -> TestResult<_> {
        let suspending = scope.spawn(move || manager.suspend(job, Caller::Agent("worker")));
        let marked = wait_until(|| entry.state() == JobState::Suspended);
        // The stale write lands now; only then is the lock released.
        let written = write_meta(&entry.dir, &stale);
        drop(writing);
        let outcome = suspending
            .join()
            .map_err(|_| TestError::Unexpected("suspend thread panicked".to_owned()));
        marked?;
        written.map_err(ctx("stale write"))?;
        outcome
    })?;
    let outcome = outcome.map_err(ctx("suspend"))?;
    assert_eq!(outcome.transition, SuspendTransition::Suspended);
    assert_eq!(fake.calls(), vec![call("freeze", "y3")]);
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert!(persisted.suspended_at.is_some());
    // Any later monitor write takes its snapshot under the lock and so
    // carries the suspension as well.
    entry.persist();
    assert!(on_disk(&env, &id)?.suspended_at.is_some());
    Ok(())
}

/// The thaw worked, but the closing record cannot be written: the job is
/// resumed (it runs, so reporting otherwise would hide that), and the disk
/// keeps saying "suspended" (the safe side) until the next write corrects
/// it.
#[test]
fn a_resume_whose_record_cannot_be_written_still_resumes() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = Env::new()?;
    let id = running_job(&env, "job-resume-unrecorded", "y4")?;
    let blocker = record_blocker(&env, &id)?;
    let block = {
        let blocker = blocker.clone();
        move || {
            // A failure here shows as a written record below, which the
            // assertions reject; nothing is lost by not returning it.
            let _ = fs::create_dir(&blocker);
        }
    };
    env.manager.set_freezer(Arc::new(
        HookFreezer::new(freezer(&fake)).before_thaw(block),
    ));
    let worker = Caller::Agent("worker");

    env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    let resumed = env.manager.resume(&id, worker).map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(resumed.status.meta.state, JobState::Running);
    assert!(blocker.is_dir());
    assert_eq!(fake.calls(), vec![call("freeze", "y4"), call("thaw", "y4")]);
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Running);
    assert_eq!(status.meta.suspended_at, None);
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert!(persisted.suspended_at.is_some());
    // Released before the thaw: the stale record is only the intent, so a
    // restart before the next write refuses to thaw (never `applied`).
    assert_eq!(persisted.suspend_phase, Some(SuspendPhase::Intent));

    // Writable again: the next write corrects the record.
    fs::remove_dir(&blocker).map_err(ctx("unblock meta.json"))?;
    env.manager
        .entry(&id, Caller::Operator)
        .map_err(ctx("entry"))?
        .persist();
    let corrected = on_disk(&env, &id)?;
    assert_eq!(corrected.state, JobState::Running);
    assert_eq!(corrected.suspended_at, None);
    assert_eq!(corrected.suspend_phase, None);
    Ok(())
}

/// End to end over a real marker directory (`MarkerDirHolds`): harw holds
/// the index's shared lock from its check until after the thaw, so a
/// Warden that honours the lock protocol (exclusive lock around marker and
/// freeze) cannot contain the cgroup in between. A marker for the job's
/// cgroup (by kernel id) refuses the resume; once it is gone, the resume
/// thaws under the lock and releases the lock afterwards.
#[test]
fn resume_holds_the_index_lock_from_check_to_thaw() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let holds = marker_index(temp.path())?;
    let index = holds.dir().to_path_buf();
    // Whether the Warden's non-blocking exclusive lock had to wait during
    // harw's thaw (`None`: no thaw happened).
    let observed: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
    let warden_tries = {
        let (index, observed) = (index.clone(), Arc::clone(&observed));
        move || {
            let blocked = fs::File::open(&index)
                .is_ok_and(|dir| matches!(dir.try_lock(), Err(fs::TryLockError::WouldBlock)));
            *lock(&observed) = Some(blocked);
        }
    };
    let env = Env::new()?;
    let id = running_job(&env, "job-lease", "z5")?;
    env.manager.set_freezer(Arc::new(
        HookFreezer::new(freezer_over(&fake, Arc::new(holds.clone()))).before_thaw(warden_tries),
    ));
    let worker = Caller::Agent("worker");
    env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;

    let marker = write_marker(&holds, fake_kernel_id("harw-job-z5"))?;
    match env.manager.resume(&id, worker) {
        Err(JobError::ContainmentHeld(job)) => assert_eq!(job, "job-lease"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(fake.calls(), vec![call("freeze", "z5")]);
    assert_eq!(*lock(&observed), None, "no thaw may be attempted");

    fs::remove_file(&marker).map_err(ctx("remove marker"))?;
    let resumed = env.manager.resume(&id, worker).map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(fake.calls(), vec![call("freeze", "z5"), call("thaw", "z5")]);
    assert_eq!(
        *lock(&observed),
        Some(true),
        "the Warden's exclusive lock must wait while harw thaws"
    );
    // The lease ends with the resume.
    let after = fs::File::open(&index).map_err(ctx("open index"))?;
    after
        .try_lock()
        .map_err(ctx("exclusive lock after the resume"))?;
    Ok(())
}

// ── Review round 2: best-effort lease, refreeze, vanished cgroup, lost mark ─

/// What happens to the cgroup right after harw's first thaw in an
/// [`AfterThawFreezer`].
#[derive(Debug, Clone, Copy)]
enum AfterThaw {
    /// Every later freeze is denied (the refreeze fails).
    DenyFreeze,
    /// The cgroup is removed between the re-check and the refreeze: the
    /// freeze reports `Gone`, the probes still answer.
    GoneBeforeRefreeze,
    /// The job exited and its cgroup is gone: every later call reports
    /// `Gone`.
    Vanish,
}

/// A freezer whose cgroup changes as [`AfterThaw`] says once a thaw went
/// through; until then it delegates.
struct AfterThawFreezer {
    inner: Arc<dyn JobFreezer>,
    after: AfterThaw,
    thawed: AtomicBool,
}

impl AfterThawFreezer {
    fn new(inner: Arc<dyn JobFreezer>, after: AfterThaw) -> Self {
        Self {
            inner,
            after,
            thawed: AtomicBool::new(false),
        }
    }

    /// Whether the probes report the cgroup as gone.
    fn vanished(&self) -> bool {
        matches!(self.after, AfterThaw::Vanish) && self.thawed.load(Ordering::SeqCst)
    }
}

impl JobFreezer for AfterThawFreezer {
    fn delegated_root(&self) -> &str {
        self.inner.delegated_root()
    }

    fn freeze(&self, leaf: &str) -> Result<(), FreezeError> {
        if !self.thawed.load(Ordering::SeqCst) {
            return self.inner.freeze(leaf);
        }
        match self.after {
            AfterThaw::DenyFreeze => Err(FreezeError::PermissionDenied),
            AfterThaw::GoneBeforeRefreeze | AfterThaw::Vanish => Err(FreezeError::Gone),
        }
    }

    fn thaw(&self, leaf: &str) -> Result<(), FreezeError> {
        self.inner.thaw(leaf)?;
        self.thawed.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn is_frozen(&self, leaf: &str) -> Result<bool, FreezeError> {
        if self.vanished() {
            return Err(FreezeError::Gone);
        }
        self.inner.is_frozen(leaf)
    }

    fn freeze_requested(&self, leaf: &str) -> Result<bool, FreezeError> {
        if self.vanished() {
            return Err(FreezeError::Gone);
        }
        self.inner.freeze_requested(leaf)
    }

    fn containment_held(&self, leaf: &str) -> Result<bool, FreezeError> {
        if self.vanished() {
            return Err(FreezeError::Gone);
        }
        self.inner.containment_held(leaf)
    }

    fn containment_lease(&self) -> Result<ContainmentLease, FreezeError> {
        self.inner.containment_lease()
    }
}

/// A hook in which the Warden writes its marker for `harw-job-<attempt>`
/// (without freezing yet).
fn warden_marks(holds: &Arc<FakeHolds>, attempt: &str) -> impl FnOnce() + Send + 'static {
    let holds = Arc::clone(holds);
    let kernel_id = fake_kernel_id(&format!("harw-job-{attempt}"));
    move || holds.hold(kernel_id)
}

/// The Warden's marker shows up during harw's thaw and the refreeze is
/// denied: `job.resume` reports the failed `refreeze` (the log says so on
/// `error`), and the suspension is not closed: in memory and on disk the
/// job stays suspended.
#[test]
fn a_denied_refreeze_is_reported_and_keeps_the_suspension() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let holds = Arc::new(FakeHolds::default());
    let env = Env::new()?;
    let id = running_job(&env, "job-refreeze", "x1")?;
    let inner = AfterThawFreezer::new(freezer_with_holds(&fake, &holds), AfterThaw::DenyFreeze);
    env.manager.set_freezer(Arc::new(
        HookFreezer::new(Arc::new(inner)).before_thaw(warden_marks(&holds, "x1")),
    ));
    let worker = Caller::Agent("worker");

    env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    match env.manager.resume(&id, worker) {
        Err(JobError::Freeze {
            job,
            operation,
            source,
        }) => {
            assert_eq!(job, "job-refreeze");
            assert_eq!(operation, "refreeze");
            assert_eq!(source, FreezeError::PermissionDenied);
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    // Thawed once; the denied refreeze never reached the backend.
    assert_eq!(fake.calls(), vec![call("freeze", "x1"), call("thaw", "x1")]);
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Suspended);
    assert!(status.meta.suspended_at.is_some());
    // harw's freeze no longer holds: the suspension is not harw's own any
    // more but unresolved, in memory and on disk.
    assert!(status.meta.is_suspension_unresolved());
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert!(persisted.suspended_at.is_some());
    assert_eq!(persisted.suspend_phase, Some(SuspendPhase::Intent));
    // A second resume touches nothing.
    match env.manager.resume(&id, worker) {
        Err(JobError::SuspensionUnresolved(job)) => assert_eq!(job, "job-refreeze"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(fake.calls(), vec![call("freeze", "x1"), call("thaw", "x1")]);
    Ok(())
}

/// The job exits right after harw's thaw and its cgroup is removed, so the
/// re-check reports `Gone`. Nothing is left to contain: no refreeze, no
/// `refreeze` error (and no error-level alarm); the job counts as resumed
/// and the suspension is closed.
#[test]
fn a_cgroup_gone_after_the_thaw_counts_as_resumed() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let holds = Arc::new(FakeHolds::default());
    let env = Env::new()?;
    let id = running_job(&env, "job-vanished", "x2")?;
    env.manager.set_freezer(Arc::new(AfterThawFreezer::new(
        freezer_with_holds(&fake, &holds),
        AfterThaw::Vanish,
    )));
    let worker = Caller::Agent("worker");

    env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    let resumed = env.manager.resume(&id, worker).map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(resumed.frozen, None);
    assert_eq!(fake.calls(), vec![call("freeze", "x2"), call("thaw", "x2")]);
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Running);
    assert_eq!(status.meta.suspended_at, None);
    assert_eq!(on_disk(&env, &id)?.suspended_at, None);
    Ok(())
}

/// The re-check finds the Warden's marker, but the cgroup is removed before
/// the refreeze (`Gone`): there is nothing left to restore, so this is no
/// `refreeze` failure and the job counts as resumed.
#[test]
fn a_cgroup_removed_before_the_refreeze_is_no_failure() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let holds = Arc::new(FakeHolds::default());
    let env = Env::new()?;
    let id = running_job(&env, "job-gone-late", "x3")?;
    let inner = AfterThawFreezer::new(
        freezer_with_holds(&fake, &holds),
        AfterThaw::GoneBeforeRefreeze,
    );
    env.manager.set_freezer(Arc::new(
        HookFreezer::new(Arc::new(inner)).before_thaw(warden_marks(&holds, "x3")),
    ));
    let worker = Caller::Agent("worker");

    env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    let resumed = env.manager.resume(&id, worker).map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(fake.calls(), vec![call("freeze", "x3"), call("thaw", "x3")]);
    assert_eq!(status_of(&env, &id)?.meta.suspended_at, None);
    Ok(())
}

/// A lease that cannot be taken because the index cannot be trusted
/// refuses the resume (`probe`) before anything is thawed; the job stays
/// suspended, harw's own (`applied`), in memory and on disk. With a usable
/// lease it thaws.
#[test]
fn an_untrusted_lease_refuses_the_resume_before_any_thaw() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let holds = Arc::new(FakeHolds::default());
    let env = env_with_holds(&fake, &holds)?;
    let id = running_job(&env, "job-no-lease", "x4")?;
    let worker = Caller::Agent("worker");

    env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    holds.set_lease_unavailable(true);
    match env.manager.resume(&id, worker) {
        Err(JobError::Freeze {
            job,
            operation,
            source,
        }) => {
            assert_eq!(job, "job-no-lease");
            assert_eq!(operation, "probe");
            assert!(
                matches!(source, FreezeError::ContainmentIndexUnreadable { .. }),
                "{source:?}"
            );
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(fake.calls(), vec![call("freeze", "x4")]);
    assert_restored_applied(&env, &id)?;

    holds.set_lease_unavailable(false);
    let resumed = env.manager.resume(&id, worker).map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(fake.calls(), vec![call("freeze", "x4"), call("thaw", "x4")]);
    Ok(())
}

/// The index lock is best effort: while someone else holds it (any local
/// user can), `job.resume` goes on with check, thaw and re-check instead of
/// refusing. A marker still refuses it; without one it thaws.
#[test]
fn a_contended_index_lock_does_not_block_resume() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let holds = Arc::new(FakeHolds::default());
    let env = env_with_holds(&fake, &holds)?;
    let id = running_job(&env, "job-contended", "x5")?;
    let worker = Caller::Agent("worker");
    holds.set_lease_contended(true);

    env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    holds.hold(fake_kernel_id("harw-job-x5"));
    match env.manager.resume(&id, worker) {
        Err(JobError::ContainmentHeld(job)) => assert_eq!(job, "job-contended"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(fake.calls(), vec![call("freeze", "x5")]);

    holds.release(fake_kernel_id("harw-job-x5"));
    let resumed = env.manager.resume(&id, worker).map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(fake.calls(), vec![call("freeze", "x5"), call("thaw", "x5")]);
    assert_eq!(on_disk(&env, &id)?.state, JobState::Running);
    Ok(())
}

/// End to end over a real marker directory: an unprivileged process holds
/// the exclusive lock of the world-readable index directory for good. The
/// lease is contended after its bounded wait and `job.resume` still works
/// (no host-wide lockout); a marker still refuses it.
#[test]
fn a_foreign_exclusive_lock_on_the_index_does_not_block_resume() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let holds = marker_index(temp.path())?;
    let env = Env::new()?;
    let id = running_job(&env, "job-squatted", "x6")?;
    env.manager
        .set_freezer(freezer_over(&fake, Arc::new(holds.clone())));
    let worker = Caller::Agent("worker");
    env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;

    let squatter = fs::File::open(holds.dir()).map_err(ctx("open index"))?;
    squatter.try_lock().map_err(ctx("foreign exclusive lock"))?;
    let marker = write_marker(&holds, fake_kernel_id("harw-job-x6"))?;
    match env.manager.resume(&id, worker) {
        Err(JobError::ContainmentHeld(job)) => assert_eq!(job, "job-squatted"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(fake.calls(), vec![call("freeze", "x6")]);

    fs::remove_file(&marker).map_err(ctx("remove marker"))?;
    let resumed = env.manager.resume(&id, worker).map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(fake.calls(), vec![call("freeze", "x6"), call("thaw", "x6")]);
    drop(squatter);
    Ok(())
}

/// A monitor write (not synced) takes the write lock between
/// `job.suspend`'s in-memory mark and its durable write, and stores the
/// mark; the durable write then fails (a deferred error at `fsync`,
/// injected). Nothing is frozen, and the taken-back state is written too,
/// so `meta.json` does not claim a suspension that never happened (a
/// restart would otherwise reload the job as suspended and frozen).
#[test]
fn a_failed_intent_write_also_takes_back_a_mark_a_monitor_stored() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-unsynced-mark", "y5")?;
    let entry = env
        .manager
        .entry(&id, Caller::Operator)
        .map_err(ctx("entry"))?;
    let manager = &env.manager;
    let job = &id;
    entry.fail_next_durable_writes(1);

    // The monitor holds the write lock and takes its snapshot only after
    // the mark, so it stores "suspended".
    let writing = lock(&entry.persist_lock);
    let outcome = std::thread::scope(|scope| -> TestResult<_> {
        let suspending = scope.spawn(move || manager.suspend(job, Caller::Agent("worker")));
        let marked = wait_until(|| entry.state() == JobState::Suspended);
        let written = write_meta(&entry.dir, &entry.meta());
        drop(writing);
        let outcome = suspending
            .join()
            .map_err(|_| TestError::Unexpected("suspend thread panicked".to_owned()));
        marked?;
        written.map_err(ctx("monitor write"))?;
        outcome
    })?;
    match outcome {
        Err(JobError::SuspendNotRecorded { job, .. }) => assert_eq!(job, "job-unsynced-mark"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Running);
    assert_eq!(status.meta.suspended_at, None);
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Running);
    assert_eq!(persisted.suspended_at, None);
    Ok(())
}

// ── Durable directory entry of the record (review thread 4505015304) ───────

/// The error a [`ScriptedDirSync`] injects.
const INJECTED_DIR_SYNC_FAILURE: &str = "injected directory sync failure";

/// One call of a [`ScriptedDirSync`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct DirSyncCall {
    /// `meta.json` in the synced directory already carried `suspended_at`:
    /// the rename the sync makes durable had happened.
    record_suspended: bool,
    /// The suspension phase `meta.json` carried at the sync: the intent
    /// before the freeze, the effect after it.
    phase: Option<SuspendPhase>,
    /// The job's write lock (`persist_lock`) was held during the sync: no
    /// other writer could rename a file in between.
    write_lock_held: bool,
    /// Freeze/thaw calls the fake kernel had seen before the sync.
    kernel_calls: usize,
}

/// A [`DirSync`] that records every call, fails `failures` of them with an
/// injected I/O error (as a lost directory write-back would) — the first
/// ones, or those after the first `passes` calls ([`ScriptedDirSync::after`])
/// — and otherwise delegates to the sync the manager had before (the one
/// `JobManager::build` installs, [`SyncDirectory`]). A hook, if armed, runs
/// once at the first injected failure (e.g. the state directory turning
/// unwritable right then); another one, if armed, once at the first call,
/// failing or not (e.g. a freeze outside harw while the intent is synced).
struct ScriptedDirSync {
    fake: Arc<FakeCgroup>,
    entry: Arc<JobEntry>,
    inner: Arc<dyn DirSync>,
    passes: AtomicUsize,
    failures: AtomicUsize,
    calls: Mutex<Vec<DirSyncCall>>,
    on_failure: Mutex<Option<Hook>>,
    on_first_call: Mutex<Option<Hook>>,
}

impl ScriptedDirSync {
    /// Fails the first `failures` calls; `fake` is the kernel and `id` the
    /// job of `env` whose state each sync notes. The other calls go to the
    /// directory sync `env`'s manager has now.
    fn new(env: &Env, fake: &Arc<FakeCgroup>, id: &JobId, failures: usize) -> TestResult<Self> {
        let entry = env
            .manager
            .entry(id, Caller::Operator)
            .map_err(ctx("entry"))?;
        Ok(Self {
            fake: Arc::clone(fake),
            entry,
            inner: env.manager.dir_sync(),
            passes: AtomicUsize::new(0),
            failures: AtomicUsize::new(failures),
            calls: Mutex::new(Vec::new()),
            on_failure: Mutex::new(None),
            on_first_call: Mutex::new(None),
        })
    }

    /// Lets the first `passes` calls through before the failures start
    /// (e.g. `after(1)`: the intent is synced, the effect is not).
    fn after(self, passes: usize) -> Self {
        self.passes.store(passes, Ordering::SeqCst);
        self
    }

    /// Runs `hook` once, at the first injected failure.
    fn on_failure(self, hook: impl FnOnce() + Send + 'static) -> Self {
        *lock(&self.on_failure) = Some(Box::new(hook));
        self
    }

    /// Runs `hook` once, at the first call (after it is recorded, before it
    /// passes or fails): in `job.suspend`, while the intent is made durable,
    /// after its rename and before any kernel call.
    fn on_first_call(self, hook: impl FnOnce() + Send + 'static) -> Self {
        *lock(&self.on_first_call) = Some(Box::new(hook));
        self
    }

    /// Installs this seam as the directory sync of `env`'s manager.
    fn install(self, env: &Env) -> Arc<Self> {
        let sync = Arc::new(self);
        env.manager
            .set_dir_sync(Arc::clone(&sync) as Arc<dyn DirSync>);
        sync
    }

    /// All calls so far, in order.
    fn calls(&self) -> Vec<DirSyncCall> {
        lock(&self.calls).clone()
    }
}

impl DirSync for ScriptedDirSync {
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        let record = read_meta(dir);
        let record_suspended = record
            .as_ref()
            .is_some_and(|meta| meta.suspended_at.is_some());
        let phase = record.and_then(|meta| meta.suspend_phase);
        let write_lock_held = self.entry.persist_lock.try_lock().is_err();
        let kernel_calls = self.fake.calls().len();
        lock(&self.calls).push(DirSyncCall {
            record_suspended,
            phase,
            write_lock_held,
            kernel_calls,
        });
        run_hook(&self.on_first_call);
        let passed = self
            .passes
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok();
        let fail = !passed
            && self
                .failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_ok();
        if fail {
            run_hook(&self.on_failure);
            return Err(io::Error::other(INJECTED_DIR_SYNC_FAILURE));
        }
        self.inner.sync_dir(dir)
    }
}

/// The sync of the intent: after its rename (`meta.json` already said
/// suspended, as the intent), under the job's write lock, before any kernel
/// call.
fn intent_sync() -> DirSyncCall {
    DirSyncCall {
        record_suspended: true,
        phase: Some(SuspendPhase::Intent),
        write_lock_held: true,
        kernel_calls: 0,
    }
}

/// The one sync of a suspend that got as far as the rename of the intent,
/// but not to the freeze.
fn synced_after_rename() -> Vec<DirSyncCall> {
    vec![intent_sync()]
}

/// Both syncs of a complete suspend: the intent before the freeze, then the
/// effect (`applied`) after the one kernel call, each after its rename and
/// under the job's write lock.
fn synced_intent_then_applied() -> Vec<DirSyncCall> {
    vec![
        intent_sync(),
        DirSyncCall {
            record_suspended: true,
            phase: Some(SuspendPhase::Applied),
            write_lock_held: true,
            kernel_calls: 1,
        },
    ]
}

/// Review thread 4505015304: the record was renamed into place, but the
/// job directory cannot be synced, so a power loss could still bring back
/// the previous record (no `suspended_at`) while `cgroup.freeze` = 1 holds.
/// `job.suspend` fails typed before anything is frozen; the job runs on,
/// in memory and on disk (the rename is taken back; that it goes through
/// the synced writer shows
/// `the_rollback_after_a_failed_directory_sync_uses_the_synced_writer`).
/// Once the directory syncs again, `job.suspend` works.
#[test]
fn a_failed_directory_sync_after_the_rename_aborts_suspend_before_freezing() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-dirsync", "z1")?;
    let sync = ScriptedDirSync::new(&env, &fake, &id, 1)?.install(&env);

    match env.manager.suspend(&id, Caller::Agent("worker")) {
        Err(JobError::SuspendNotRecorded { job, source }) => {
            assert_eq!(job, "job-dirsync");
            assert_eq!(source.to_string(), INJECTED_DIR_SYNC_FAILURE);
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(sync.calls(), synced_after_rename());
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Running);
    assert_eq!(status.meta.suspended_at, None);
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Running);
    assert_eq!(persisted.suspended_at, None);

    let suspended = env
        .manager
        .suspend(&id, Caller::Agent("worker"))
        .map_err(ctx("suspend"))?;
    assert_eq!(suspended.transition, SuspendTransition::Suspended);
    assert_eq!(fake.calls(), vec![call("freeze", "z1")]);
    assert!(on_disk(&env, &id)?.suspended_at.is_some());
    Ok(())
}

/// The directory sync fails after the rename, and the state directory turns
/// unwritable right then, so the rollback of the record cannot be written
/// either: surfaced as its own typed error (with the sync failure as
/// source). Nothing is frozen, in memory the job runs on, and the disk
/// still says suspended (only as the intent) until the next write corrects
/// it. In this session memory counts: `job.resume` thaws nothing, not even
/// a freeze set outside harw later (after a restart the stale intent is an
/// unresolved suspension, see
/// `a_restart_after_a_failed_rollback_never_thaws_an_outside_freeze`).
#[test]
fn a_failed_rollback_after_a_failed_directory_sync_is_surfaced() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-dirsync-stuck", "z2")?;
    let blocker = record_blocker(&env, &id)?;
    let block = {
        let blocker = blocker.clone();
        move || {
            // A failure here shows as a plain `SuspendNotRecorded` below,
            // which the match rejects; nothing is lost by not returning it.
            let _ = fs::create_dir(&blocker);
        }
    };
    let sync = ScriptedDirSync::new(&env, &fake, &id, 1)?
        .on_failure(block)
        .install(&env);

    match env.manager.suspend(&id, Caller::Agent("worker")) {
        Err(JobError::SuspendRollbackNotRecorded { job, source, .. }) => {
            assert_eq!(job, "job-dirsync-stuck");
            assert_eq!(source.to_string(), INJECTED_DIR_SYNC_FAILURE);
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert!(blocker.is_dir());
    assert_eq!(sync.calls(), synced_after_rename());
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Running);
    assert_eq!(status.meta.suspended_at, None);
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert!(persisted.suspended_at.is_some());
    assert_eq!(persisted.suspend_phase, Some(SuspendPhase::Intent));

    let worker = Caller::Agent("worker");
    let resumed = env.manager.resume(&id, worker).map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::NotSuspended);
    fake.set_outside("harw-job-z2", true, true);
    match env.manager.resume(&id, worker) {
        Err(JobError::FrozenElsewhere(job)) => assert_eq!(job, "job-dirsync-stuck"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());

    fs::remove_dir(&blocker).map_err(ctx("unblock meta.json"))?;
    env.manager
        .entry(&id, Caller::Operator)
        .map_err(ctx("entry"))?
        .persist();
    let corrected = on_disk(&env, &id)?;
    assert_eq!(corrected.state, JobState::Running);
    assert_eq!(corrected.suspended_at, None);
    Ok(())
}

/// The rollback after a failed directory sync goes through the synced
/// writer (`fsync` before the `rename`, `JobEntry::try_persist`), not the
/// unsynced one of the monitor (`JobEntry::persist`): a durable write
/// failure armed at the sync failure, which only the synced writer
/// consumes, is what the rollback reports. Nothing is frozen, and the
/// armed failure is used up, so the next `job.suspend` works.
#[test]
fn the_rollback_after_a_failed_directory_sync_uses_the_synced_writer() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-dirsync-synced-rollback", "z4")?;
    let entry = env
        .manager
        .entry(&id, Caller::Operator)
        .map_err(ctx("entry"))?;
    // Armed at the injected sync failure, i.e. after the rename and before
    // the rollback is written.
    let arm = move || entry.fail_next_durable_writes(1);
    let sync = ScriptedDirSync::new(&env, &fake, &id, 1)?
        .on_failure(arm)
        .install(&env);
    let worker = Caller::Agent("worker");

    match env.manager.suspend(&id, worker) {
        Err(JobError::SuspendRollbackNotRecorded {
            job,
            source,
            persist,
        }) => {
            assert_eq!(job, "job-dirsync-synced-rollback");
            assert_eq!(source.to_string(), INJECTED_DIR_SYNC_FAILURE);
            assert_eq!(persist.to_string(), "injected durable write failure");
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(sync.calls(), synced_after_rename());
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Running);
    assert_eq!(status.meta.suspended_at, None);
    assert!(on_disk(&env, &id)?.suspended_at.is_some());

    let suspended = env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    assert_eq!(suspended.transition, SuspendTransition::Suspended);
    assert_eq!(fake.calls(), vec![call("freeze", "z4")]);
    assert!(on_disk(&env, &id)?.suspended_at.is_some());
    Ok(())
}

/// The successful path syncs the job directory exactly twice, each time
/// after the rename of the record and under the job's write lock: the
/// intent before the kernel freezes, the effect (`applied`) after it,
/// through the manager's own sync behind the counter (the real one, see
/// `the_manager_installs_the_real_directory_sync`). A repeated suspend
/// writes no new record and syncs nothing.
#[test]
fn suspend_syncs_the_job_directory_after_the_rename_and_before_the_freeze() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-dirsync-ok", "z3")?;
    let sync = ScriptedDirSync::new(&env, &fake, &id, 0)?.install(&env);
    let worker = Caller::Agent("worker");

    let outcome = env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    assert_eq!(outcome.transition, SuspendTransition::Suspended);
    assert_eq!(sync.calls(), synced_intent_then_applied());
    assert_eq!(fake.calls(), vec![call("freeze", "z3")]);
    assert!(on_disk(&env, &id)?.suspended_at.is_some());

    let again = env
        .manager
        .suspend(&id, worker)
        .map_err(ctx("suspend again"))?;
    assert_eq!(again.transition, SuspendTransition::AlreadySuspended);
    assert_eq!(sync.calls(), synced_intent_then_applied());
    Ok(())
}

/// The production directory sync syncs a real directory and reports the
/// error of one it cannot open (fail closed: `job.suspend` would not
/// freeze).
#[test]
fn the_production_directory_sync_reports_its_errors() -> TestResult {
    let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
    SyncDirectory
        .sync_dir(temp.path())
        .map_err(ctx("sync a real directory"))?;
    match SyncDirectory.sync_dir(&temp.path().join("missing")) {
        Err(err) => assert_eq!(err.kind(), io::ErrorKind::NotFound),
        Ok(()) => {
            return Err(TestError::Unexpected(
                "synced a directory that does not exist".to_owned(),
            ));
        }
    }
    Ok(())
}

/// `JobManager::build` installs the real directory sync, not a no-op: the
/// manager's own seam syncs an existing directory and reports the error
/// of a missing one. With the delegating `ScriptedDirSync`, this shows the
/// production path of `job.suspend` syncs the job directory for real.
#[test]
fn the_manager_installs_the_real_directory_sync() -> TestResult {
    let env = Env::new()?;
    let dir_sync = env.manager.dir_sync();
    let jobs_dir = env.manager.config().jobs_dir();
    dir_sync
        .sync_dir(&jobs_dir)
        .map_err(ctx("sync the jobs directory"))?;
    match dir_sync.sync_dir(&jobs_dir.join("missing")) {
        Err(err) => assert_eq!(err.kind(), io::ErrorKind::NotFound),
        Ok(()) => {
            return Err(TestError::Unexpected(
                "the default directory sync is a no-op".to_owned(),
            ));
        }
    }
    Ok(())
}

// ── Phase of a suspension: intent vs. applied (review thread 4505015304) ───

/// Liveness of every job without an own pidfd after a [`restart`]: alive,
/// identity proven. A test process cannot run in the fake job cgroup its
/// recorded identity names, so `/proc` would never prove it.
fn always_alive(_meta: &JobMeta) -> Liveness {
    Liveness::Alive
}

/// Restarts harw over the state directory of `env`: a new manager loads its
/// jobs as those of a previous session, each recorded process counts as
/// still alive ([`always_alive`]), and its freezer works on the same fake
/// kernel and reads the containment index `holds`.
fn restart(
    env: &Env,
    fake: &Arc<FakeCgroup>,
    holds: &Arc<FakeHolds>,
) -> TestResult<(Arc<JobManager>, Arc<RecordingNotifier>)> {
    let recorder = Arc::new(RecordingNotifier::default());
    let reloaded = JobManager::with_liveness(
        env.manager.config().clone(),
        Arc::clone(&recorder) as Arc<dyn JobNotifier>,
        always_alive,
    )
    .map_err(ctx("restart"))?;
    reloaded.set_freezer(freezer_with_holds(fake, holds));
    Ok((reloaded, recorder))
}

/// Whether `recorder` sent the owner of `id` the warning `message`.
fn warned(recorder: &RecordingNotifier, id: &JobId, message: &str) -> bool {
    recorder.notifications().into_iter().any(|notification| {
        matches!(
            &notification.event,
            JobEvent::Warning { job_id, message: text, .. } if job_id == id && text == message
        )
    })
}

/// Writes `meta` as the `meta.json` of a job of a previous session (no
/// manager knows it yet) and returns its job directory.
fn previous_session_record(env: &Env, meta: &JobMeta) -> TestResult<std::path::PathBuf> {
    let dir = env.state_dir().join("jobs").join(meta.job_id.as_str());
    fs::create_dir_all(&dir).map_err(ctx("create old job dir"))?;
    write_meta(&dir, meta).map_err(ctx("write old meta"))?;
    Ok(dir)
}

/// Review thread 4505015304, the owner's counterexample: the directory sync
/// after the rename of the intent fails, and so does the write of its
/// rollback, so `meta.json` keeps saying "suspended" although nothing was
/// frozen. harw restarts while the job's process still lives, and an
/// operator then freezes the job cgroup without a Warden marker. The record
/// holds only the intent, so the reloaded job is an unresolved suspension
/// (its owner is told), and neither `job.resume` (any caller) nor a new
/// `job.suspend` writes the cgroup: the operator's freeze stays in place
/// (A3/O3). `job.stop` still ends the job, to the end of the detached path
/// and without a thaw: the job has a live process (our child), whose
/// identity the stub record cannot prove, so no signal reaches it, but the
/// record ends (`unknown`) and the suspension closes, in memory and on
/// disk, with not a single cgroup write.
#[tokio::test]
async fn a_restart_after_a_failed_rollback_never_thaws_an_outside_freeze() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    // A running job with a real, live process, so the stop after the
    // restart has something to act on.
    let sleeper = Sleeper::spawn()?;
    let mut meta = stub_job_meta(
        "job-restart-intent",
        JobState::Running,
        "worker",
        &["orchestrator"],
        Some("u1"),
    )?;
    meta.pid = Some(sleeper.pid());
    let id = env
        .manager
        .insert_test_job(meta, true)
        .map_err(ctx("insert stub job"))?;
    let blocker = record_blocker(&env, &id)?;
    let block = {
        let blocker = blocker.clone();
        move || {
            // A failure here shows as a plain `SuspendNotRecorded` below,
            // which the match rejects; nothing is lost by not returning it.
            let _ = fs::create_dir(&blocker);
        }
    };
    let sync = ScriptedDirSync::new(&env, &fake, &id, 1)?
        .on_failure(block)
        .install(&env);
    match env.manager.suspend(&id, Caller::Agent("worker")) {
        Err(JobError::SuspendRollbackNotRecorded { job, source, .. }) => {
            assert_eq!(job, "job-restart-intent");
            assert_eq!(source.to_string(), INJECTED_DIR_SYNC_FAILURE);
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(sync.calls(), synced_after_rename());
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    // Nothing is frozen, but the stale record says "suspended": as the
    // intent only, never as harw's own freeze.
    let stale = on_disk(&env, &id)?;
    assert_eq!(stale.state, JobState::Suspended);
    assert!(stale.suspended_at.is_some());
    assert_eq!(stale.suspend_phase, Some(SuspendPhase::Intent));
    fs::remove_dir(&blocker).map_err(ctx("unblock meta.json"))?;

    // harw restarts; the job's process still lives.
    let holds = Arc::new(FakeHolds::default());
    let (reloaded, recorder) = restart(&env, &fake, &holds)?;
    let status = reloaded
        .status(&id, Caller::Operator)
        .map_err(ctx("status"))?;
    assert_eq!(status.meta.state, JobState::Detached);
    assert!(status.meta.is_suspended());
    assert!(status.meta.is_suspension_unresolved());
    assert!(
        warned(&recorder, &id, SUSPENSION_UNRESOLVED_ON_RELOAD),
        "{:?}",
        recorder.notifications()
    );
    assert!(!warned(&recorder, &id, SUSPENDED_ON_RELOAD));

    // An operator freezes the job cgroup, without a Warden marker.
    fake.set_outside("harw-job-u1", true, true);
    let owner = Caller::Agent("worker");
    for caller in [owner, Caller::Agent("orchestrator"), Caller::Operator] {
        match reloaded.resume(&id, caller) {
            Err(JobError::SuspensionUnresolved(job)) => assert_eq!(job, "job-restart-intent"),
            other => return Err(TestError::Unexpected(format!("{caller:?}: {other:?}"))),
        }
    }
    // A new suspend cannot prove that freeze is harw's either.
    match reloaded.suspend(&id, owner) {
        Err(JobError::SuspensionUnresolved(job)) => assert_eq!(job, "job-restart-intent"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    // Not a single cgroup write: the operator's freeze is still in place.
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let probe = freezer(&fake);
    assert_eq!(probe.freeze_requested("harw-job-u1"), Ok(true));
    assert_eq!(probe.is_frozen("harw-job-u1"), Ok(true));
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Detached);
    assert_eq!(persisted.suspend_phase, Some(SuspendPhase::Intent));
    assert_eq!(persisted.pid, Some(sleeper.pid()));

    // job.stop still works on it, through the detached path to the end:
    // SIGKILL chosen, never a thaw. The recorded identity (stub start time
    // and cgroup) does not match the live process, so harw sends it no
    // signal; the stop ends the record anyway and closes the suspension.
    let (stopped, sent) = reloaded
        .stop_reporting_signal(&id, owner, JobSignal::Term)
        .await
        .map_err(ctx("stop"))?;
    assert_eq!(sent, JobSignal::Kill);
    assert_eq!(stopped.meta.state, JobState::Unknown);
    assert!(stopped.meta.state.is_terminal());
    assert!(stopped.meta.ended_at.is_some());
    assert_eq!(stopped.meta.suspended_at, None);
    assert_eq!(stopped.meta.suspend_phase, None);
    assert!(!stopped.meta.is_suspended());
    let ended = on_disk(&env, &id)?;
    assert_eq!(ended.state, JobState::Unknown);
    assert!(ended.ended_at.is_some());
    assert_eq!(ended.suspended_at, None);
    assert_eq!(ended.suspend_phase, None);
    // Not a single cgroup write, and the operator's freeze is untouched.
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    assert_eq!(probe.freeze_requested("harw-job-u1"), Ok(true));
    Ok(())
}

/// A suspension whose effect was recorded durably (`applied`) is harw's own
/// after a restart too: the reloaded job (still alive) is reported as
/// suspended, and `job.resume` thaws it, books the time, keeps it detached
/// and closes the record.
#[test]
fn an_applied_suspension_resumes_after_a_restart() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-restart-applied", "u2")?;
    env.manager
        .suspend(&id, Caller::Agent("worker"))
        .map_err(ctx("suspend"))?;
    assert_eq!(
        on_disk(&env, &id)?.suspend_phase,
        Some(SuspendPhase::Applied)
    );

    let holds = Arc::new(FakeHolds::default());
    let (reloaded, recorder) = restart(&env, &fake, &holds)?;
    let status = reloaded
        .status(&id, Caller::Operator)
        .map_err(ctx("status"))?;
    assert_eq!(status.meta.state, JobState::Detached);
    assert_eq!(status.meta.suspension_phase(), Some(SuspendPhase::Applied));
    assert!(
        warned(&recorder, &id, SUSPENDED_ON_RELOAD),
        "{:?}",
        recorder.notifications()
    );
    assert!(!warned(&recorder, &id, SUSPENSION_UNRESOLVED_ON_RELOAD));

    let resumed = reloaded
        .resume(&id, Caller::Agent("worker"))
        .map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(resumed.status.meta.state, JobState::Detached);
    assert_eq!(resumed.status.meta.suspended_at, None);
    assert_eq!(resumed.frozen, Some(false));
    assert_eq!(fake.calls(), vec![call("freeze", "u2"), call("thaw", "u2")]);
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Detached);
    assert_eq!(persisted.suspended_at, None);
    assert_eq!(persisted.suspend_phase, None);
    Ok(())
}

/// The freeze worked, but its effect cannot be recorded (a deferred write
/// error of the synced writer, armed right before the kernel freezes, after
/// the intent was written): nothing is thawed. The job stays frozen and its
/// suspension unresolved, in memory and on disk only the intent, so
/// `job.resume` refuses, a new `job.suspend` cannot prove the freeze is
/// harw's (the cgroup is frozen) and refuses too, and `job.stop` picks
/// SIGKILL without a cgroup write. The stub has no process (and no monitor
/// that would end it), so only that choice and the absence of a thaw show
/// here; that a stop ends an unresolved suspension to the end shows
/// `a_restart_after_a_failed_rollback_never_thaws_an_outside_freeze` (and
/// `stopping_a_detached_suspended_job_kills_it_and_closes_the_suspension`).
#[tokio::test]
async fn an_unrecorded_effect_leaves_the_job_frozen_and_unresolved() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = Env::new()?;
    let id = running_job(&env, "job-effect-unwritten", "u3")?;
    let entry = env
        .manager
        .entry(&id, Caller::Operator)
        .map_err(ctx("entry"))?;
    let arm = move || entry.fail_next_durable_writes(1);
    env.manager.set_freezer(Arc::new(
        HookFreezer::new(freezer(&fake)).before_freeze(arm),
    ));
    let worker = Caller::Agent("worker");

    match env.manager.suspend(&id, worker) {
        Err(JobError::SuspendAppliedNotRecorded { job, source }) => {
            assert_eq!(job, "job-effect-unwritten");
            assert_eq!(source.to_string(), "injected durable write failure");
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    // Frozen once, never thawed.
    assert_eq!(fake.calls(), vec![call("freeze", "u3")]);
    let probe = freezer(&fake);
    assert_eq!(probe.freeze_requested("harw-job-u3"), Ok(true));
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Suspended);
    assert!(status.meta.suspended_at.is_some());
    assert!(status.meta.is_suspension_unresolved());
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert_eq!(persisted.suspend_phase, Some(SuspendPhase::Intent));

    match env.manager.resume(&id, worker) {
        Err(JobError::SuspensionUnresolved(job)) => assert_eq!(job, "job-effect-unwritten"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    match env.manager.suspend(&id, worker) {
        Err(JobError::SuspensionUnresolved(job)) => assert_eq!(job, "job-effect-unwritten"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(fake.calls(), vec![call("freeze", "u3")]);

    // Only the signal choice: SIGKILL while frozen, no thaw (no process).
    let (_, sent) = env
        .manager
        .stop_reporting_signal(&id, worker, JobSignal::Term)
        .await
        .map_err(ctx("stop"))?;
    assert_eq!(sent, JobSignal::Kill);
    assert_eq!(fake.calls(), vec![call("freeze", "u3")]);
    Ok(())
}

/// The same when the effect reached `meta.json` but its directory entry
/// cannot be synced: nothing is thawed, and the record goes back to the
/// intent, in memory and on disk, so neither this session nor a restart
/// thaws a freeze harw cannot prove.
#[test]
fn an_effect_whose_directory_entry_is_not_durable_stays_unresolved() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-effect-not-durable", "u4")?;
    let sync = ScriptedDirSync::new(&env, &fake, &id, 1)?
        .after(1)
        .install(&env);
    let worker = Caller::Agent("worker");

    match env.manager.suspend(&id, worker) {
        Err(JobError::SuspendAppliedNotRecorded { job, source }) => {
            assert_eq!(job, "job-effect-not-durable");
            assert_eq!(source.to_string(), INJECTED_DIR_SYNC_FAILURE);
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    // The intent was synced before the freeze; the effect was renamed into
    // place after it, but its sync failed.
    assert_eq!(sync.calls(), synced_intent_then_applied());
    assert_eq!(fake.calls(), vec![call("freeze", "u4")]);
    assert!(status_of(&env, &id)?.meta.is_suspension_unresolved());
    assert_eq!(
        on_disk(&env, &id)?.suspend_phase,
        Some(SuspendPhase::Intent)
    );
    match env.manager.resume(&id, worker) {
        Err(JobError::SuspensionUnresolved(job)) => assert_eq!(job, "job-effect-not-durable"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(fake.calls(), vec![call("freeze", "u4")]);
    Ok(())
}

/// Review round 2: the freeze worked, its effect cannot be recorded, and
/// the rollback of the record to the intent cannot be written either (two
/// deferred write errors of the synced writer, armed right before the
/// kernel freezes). That is its own typed error with both causes, not a
/// silently lost write: the rollback goes through the synced writer
/// (`JobEntry::try_persist`), which alone consumes the armed failures.
/// Nothing is thawed; in memory the suspension is unresolved, and the disk
/// keeps the intent written before the freeze.
#[test]
fn a_failed_rollback_of_an_unrecorded_effect_is_surfaced() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = Env::new()?;
    let id = running_job(&env, "job-effect-rollback-unwritten", "u8")?;
    let entry = env
        .manager
        .entry(&id, Caller::Operator)
        .map_err(ctx("entry"))?;
    let arm = move || entry.fail_next_durable_writes(2);
    env.manager.set_freezer(Arc::new(
        HookFreezer::new(freezer(&fake)).before_freeze(arm),
    ));
    let worker = Caller::Agent("worker");

    match env.manager.suspend(&id, worker) {
        Err(JobError::SuspendAppliedRollbackNotRecorded {
            job,
            source,
            persist,
        }) => {
            assert_eq!(job, "job-effect-rollback-unwritten");
            assert_eq!(source.to_string(), "injected durable write failure");
            assert_eq!(persist.to_string(), "injected durable write failure");
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    // Frozen once, never thawed.
    assert_eq!(fake.calls(), vec![call("freeze", "u8")]);
    assert_eq!(freezer(&fake).freeze_requested("harw-job-u8"), Ok(true));
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Suspended);
    assert!(status.meta.is_suspension_unresolved());
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert_eq!(persisted.suspend_phase, Some(SuspendPhase::Intent));
    match env.manager.resume(&id, worker) {
        Err(JobError::SuspensionUnresolved(job)) => {
            assert_eq!(job, "job-effect-rollback-unwritten");
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(fake.calls(), vec![call("freeze", "u8")]);
    Ok(())
}

/// Review round 2: the effect was renamed into `meta.json`, its directory
/// entry cannot be synced, and right then the state directory stops taking
/// the rollback to the intent (armed at the sync failure; only the synced
/// writer consumes it). The typed error carries both causes, because now
/// memory (unresolved) and disk (`applied`, the more permissive side)
/// disagree. This session goes by memory and refuses `job.resume`; nothing
/// is thawed; the next write corrects the disk.
#[test]
fn an_unwritable_rollback_after_a_failed_effect_sync_is_surfaced() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-effect-diverged", "u9")?;
    let entry = env
        .manager
        .entry(&id, Caller::Operator)
        .map_err(ctx("entry"))?;
    let arm = {
        let entry = Arc::clone(&entry);
        move || entry.fail_next_durable_writes(1)
    };
    let sync = ScriptedDirSync::new(&env, &fake, &id, 1)?
        .after(1)
        .on_failure(arm)
        .install(&env);
    let worker = Caller::Agent("worker");

    match env.manager.suspend(&id, worker) {
        Err(JobError::SuspendAppliedRollbackNotRecorded {
            job,
            source,
            persist,
        }) => {
            assert_eq!(job, "job-effect-diverged");
            assert_eq!(source.to_string(), INJECTED_DIR_SYNC_FAILURE);
            assert_eq!(persist.to_string(), "injected durable write failure");
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(sync.calls(), synced_intent_then_applied());
    assert_eq!(fake.calls(), vec![call("freeze", "u9")]);
    assert!(status_of(&env, &id)?.meta.is_suspension_unresolved());
    assert_eq!(
        on_disk(&env, &id)?.suspend_phase,
        Some(SuspendPhase::Applied)
    );
    match env.manager.resume(&id, worker) {
        Err(JobError::SuspensionUnresolved(job)) => assert_eq!(job, "job-effect-diverged"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(fake.calls(), vec![call("freeze", "u9")]);

    // The next write (here as the monitor's) brings the disk back to memory.
    entry.persist();
    assert_eq!(
        on_disk(&env, &id)?.suspend_phase,
        Some(SuspendPhase::Intent)
    );
    Ok(())
}

/// Registers a supervised stub job of `worker` that has been suspended for
/// 30 seconds with only its intent recorded (an unresolved suspension, as
/// after `SuspendAppliedNotRecorded` once the cgroup was thawed by hand),
/// its cgroup `harw-job-<attempt>` not frozen; `stop_requested` marks a
/// stop in its grace period. Returns the job and the suspension's start.
fn unresolved_job(
    env: &Env,
    id: &str,
    attempt: &str,
    stop_requested: bool,
) -> TestResult<(JobId, Timestamp)> {
    let since = ago(30)?;
    let mut meta = stub_job_meta(id, JobState::Suspended, "worker", &[], Some(attempt))?;
    meta.suspended_at = Some(since);
    meta.suspend_phase = Some(SuspendPhase::Intent);
    meta.stop_requested = stop_requested;
    let id = env
        .manager
        .insert_test_job(meta, true)
        .map_err(ctx("insert unresolved stub job"))?;
    Ok((id, since))
}

/// `id` is still the unresolved suspension [`unresolved_job`] registered,
/// in memory and on disk (suspended since `since`, only the intent, not
/// rolled back to running), and `job.resume` still refuses it.
fn assert_still_unresolved(env: &Env, id: &JobId, since: Timestamp) -> TestResult {
    for (place, meta) in [
        ("memory", status_of(env, id)?.meta),
        ("disk", on_disk(env, id)?),
    ] {
        assert_eq!(meta.state, JobState::Suspended, "{place}");
        assert_eq!(meta.suspended_at, Some(since), "{place}");
        assert_eq!(meta.suspend_phase, Some(SuspendPhase::Intent), "{place}");
        assert!(meta.is_suspension_unresolved(), "{place}");
    }
    match env.manager.resume(id, Caller::Agent("worker")) {
        Err(JobError::SuspensionUnresolved(job)) => assert_eq!(job, id.to_string()),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    Ok(())
}

/// Review round 2: a re-issued `job.suspend` on an unresolved suspension
/// that fails — its intent cannot be written, the job directory cannot be
/// synced after the rename, or the freeze is denied — leaves the
/// suspension exactly as it was: unresolved, with its original start and
/// state, in memory and on disk. Only a fresh suspension is taken back; an
/// unresolved one is never rolled back to running. The cgroup sees at most
/// the one freeze attempt and never a thaw, also not from the
/// `job.resume` that still refuses afterwards.
#[test]
fn a_failed_reissue_keeps_an_unresolved_suspension_as_it_was() -> TestResult {
    let worker = Caller::Agent("worker");

    // The intent cannot be written: nothing is frozen.
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let (id, since) = unresolved_job(&env, "job-reissue-unwritten", "r1", false)?;
    env.manager
        .entry(&id, Caller::Operator)
        .map_err(ctx("entry"))?
        .fail_next_durable_writes(1);
    match env.manager.suspend(&id, worker) {
        Err(JobError::SuspendNotRecorded { job, source }) => {
            assert_eq!(job, "job-reissue-unwritten");
            assert_eq!(source.to_string(), "injected durable write failure");
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    assert_still_unresolved(&env, &id, since)?;
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());

    // Renamed, but the job directory cannot be synced: nothing is frozen.
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let (id, since) = unresolved_job(&env, "job-reissue-not-durable", "r2", false)?;
    let sync = ScriptedDirSync::new(&env, &fake, &id, 1)?.install(&env);
    match env.manager.suspend(&id, worker) {
        Err(JobError::SuspendNotRecorded { job, source }) => {
            assert_eq!(job, "job-reissue-not-durable");
            assert_eq!(source.to_string(), INJECTED_DIR_SYNC_FAILURE);
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(sync.calls(), synced_after_rename());
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    assert_still_unresolved(&env, &id, since)?;
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());

    // The freeze is denied: the one attempt, no thaw.
    let fake = Arc::new(FakeCgroup::denying());
    let env = env_with_freezer(&fake)?;
    let (id, since) = unresolved_job(&env, "job-reissue-denied", "r3", false)?;
    match env.manager.suspend(&id, worker) {
        Err(JobError::Freeze {
            job,
            operation,
            source,
        }) => {
            assert_eq!(job, "job-reissue-denied");
            assert_eq!(operation, "freeze");
            assert_eq!(source, FreezeError::PermissionDenied);
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(fake.calls(), vec![call("freeze", "r3")]);
    assert_still_unresolved(&env, &id, since)?;
    assert_eq!(fake.calls(), vec![call("freeze", "r3")]);
    Ok(())
}

/// A re-issued `job.suspend` on an unresolved suspension whose stop is
/// already under way (grace period) is refused typed (`StopPending`)
/// before the cgroup is probed or written: the stop ends the job, there is
/// nothing to resolve. The record stays the unresolved intent.
#[test]
fn a_reissue_on_an_unresolved_suspension_being_stopped_is_refused() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let (id, since) = unresolved_job(&env, "job-reissue-stopping", "r4", true)?;
    match env.manager.suspend(&id, Caller::Agent("worker")) {
        Err(JobError::StopPending(job)) => assert_eq!(job, "job-reissue-stopping"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    assert!(status_of(&env, &id)?.meta.stop_requested);
    assert_still_unresolved(&env, &id, since)?;
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    Ok(())
}

/// A record written before the phase existed (`suspended_at`, no phase)
/// fails closed after a restart: an unresolved suspension, told to the
/// owner. While its cgroup is frozen — by the older harw or by someone
/// else, harw cannot tell — neither `job.resume` nor `job.suspend` writes
/// the cgroup, and the record keeps its shape.
#[test]
fn a_record_without_a_phase_is_an_unresolved_suspension_after_a_restart() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let mut meta = stub_job_meta(
        "job-legacy-suspended",
        JobState::Suspended,
        "worker",
        &[],
        Some("u5"),
    )?;
    meta.suspended_at = Some(ago(30)?);
    let dir = previous_session_record(&env, &meta)?;
    let text = fs::read_to_string(dir.join(META_FILE)).map_err(ctx("read old meta"))?;
    assert!(!text.contains("suspend_phase"), "{text}");
    let id = meta.job_id.clone();
    fake.set_outside("harw-job-u5", true, true);

    let holds = Arc::new(FakeHolds::default());
    let (reloaded, recorder) = restart(&env, &fake, &holds)?;
    let status = reloaded
        .status(&id, Caller::Operator)
        .map_err(ctx("status"))?;
    assert_eq!(status.meta.state, JobState::Detached);
    assert_eq!(status.meta.suspend_phase, None);
    assert_eq!(status.meta.suspension_phase(), Some(SuspendPhase::Intent));
    assert!(
        warned(&recorder, &id, SUSPENSION_UNRESOLVED_ON_RELOAD),
        "{:?}",
        recorder.notifications()
    );

    let owner = Caller::Agent("worker");
    match reloaded.resume(&id, owner) {
        Err(JobError::SuspensionUnresolved(job)) => assert_eq!(job, "job-legacy-suspended"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    match reloaded.suspend(&id, owner) {
        Err(JobError::SuspensionUnresolved(job)) => assert_eq!(job, "job-legacy-suspended"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let probe = freezer(&fake);
    assert_eq!(probe.is_frozen("harw-job-u5"), Ok(true));
    let persisted = read_meta(&dir).ok_or(TestError::Missing("meta.json"))?;
    assert_eq!(persisted.state, JobState::Detached);
    assert_eq!(persisted.suspend_phase, None);
    assert!(persisted.is_suspension_unresolved());
    Ok(())
}

/// Resolving an unresolved suspension: while its cgroup is not frozen, a
/// new `job.suspend` suspends the job again from scratch — intent, probe
/// again, freeze, effect — so only a freeze harw itself wrote onto a cgroup
/// it found unfrozen (before the intent and again right before the write;
/// a freeze in between refuses, see
/// `a_freeze_outside_harw_during_a_reissue_keeps_the_suspension_unresolved`,
/// only the microseconds between that probe and the write stay open)
/// becomes harw's own, and `job.resume` then thaws it. The suspension keeps
/// its start. A Warden marker on the cgroup refuses the new suspend like a
/// freeze does (nothing written).
#[test]
fn a_new_suspend_resolves_an_unresolved_suspension_only_from_an_unfrozen_cgroup() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let mut meta = stub_job_meta(
        "job-resolve",
        JobState::Suspended,
        "worker",
        &[],
        Some("u6"),
    )?;
    meta.suspended_at = Some(ago(30)?);
    meta.suspend_phase = Some(SuspendPhase::Intent);
    let dir = previous_session_record(&env, &meta)?;
    let id = meta.job_id.clone();

    let holds = Arc::new(FakeHolds::default());
    let (reloaded, _recorder) = restart(&env, &fake, &holds)?;
    let since = reloaded
        .status(&id, Caller::Operator)
        .map_err(ctx("status"))?
        .meta
        .suspended_at;
    assert!(since.is_some());
    let owner = Caller::Agent("worker");
    match reloaded.resume(&id, owner) {
        Err(JobError::SuspensionUnresolved(job)) => assert_eq!(job, "job-resolve"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    // The Warden holds the cgroup (marker first): harw shares no bit.
    holds.hold(fake_kernel_id("harw-job-u6"));
    match reloaded.suspend(&id, owner) {
        Err(JobError::SuspensionUnresolved(job)) => assert_eq!(job, "job-resolve"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    holds.release(fake_kernel_id("harw-job-u6"));

    // Unfrozen and unheld: harw freezes it itself and records the effect.
    let suspended = reloaded.suspend(&id, owner).map_err(ctx("suspend"))?;
    assert_eq!(suspended.transition, SuspendTransition::Suspended);
    assert_eq!(suspended.frozen, Some(true));
    assert_eq!(suspended.status.meta.state, JobState::Detached);
    assert_eq!(suspended.status.meta.suspended_at, since);
    assert_eq!(
        suspended.status.meta.suspension_phase(),
        Some(SuspendPhase::Applied)
    );
    assert_eq!(fake.calls(), vec![call("freeze", "u6")]);
    let persisted = read_meta(&dir).ok_or(TestError::Missing("meta.json"))?;
    assert_eq!(persisted.suspend_phase, Some(SuspendPhase::Applied));

    let resumed = reloaded.resume(&id, owner).map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(fake.calls(), vec![call("freeze", "u6"), call("thaw", "u6")]);
    let persisted = read_meta(&dir).ok_or(TestError::Missing("meta.json"))?;
    assert_eq!(persisted.suspended_at, None);
    assert_eq!(persisted.suspend_phase, None);
    assert!(
        persisted.suspended_secs >= 30,
        "{}",
        persisted.suspended_secs
    );
    Ok(())
}

/// `job.resume` first stops the record from claiming the freeze as harw's
/// own (back to the intent) and only then thaws, so no `applied` outlives
/// a thaw. When that record cannot be written, nothing is thawed and the
/// suspension stays harw's own (`ResumeNotRecorded`); once the record is
/// writable again, `job.resume` works.
#[test]
fn a_resume_whose_release_cannot_be_recorded_thaws_nothing() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-release-unwritten", "u7")?;
    let worker = Caller::Agent("worker");
    env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    let blocker = record_blocker(&env, &id)?;
    fs::create_dir(&blocker).map_err(ctx("block meta.json"))?;

    match env.manager.resume(&id, worker) {
        Err(JobError::ResumeNotRecorded { job, .. }) => assert_eq!(job, "job-release-unwritten"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(fake.calls(), vec![call("freeze", "u7")]);
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Suspended);
    assert_eq!(status.meta.suspension_phase(), Some(SuspendPhase::Applied));
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert_eq!(persisted.suspend_phase, Some(SuspendPhase::Applied));

    fs::remove_dir(&blocker).map_err(ctx("unblock meta.json"))?;
    let resumed = env.manager.resume(&id, worker).map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(fake.calls(), vec![call("freeze", "u7"), call("thaw", "u7")]);
    assert_eq!(on_disk(&env, &id)?.suspend_phase, None);
    Ok(())
}

// ── Review round 3: probe right before the freeze, restore after a refusal ──

/// The intent's `fsync`s take time: an operator freezes the job cgroup
/// (no Warden marker) while `job.suspend` syncs the job directory after the
/// intent's rename. The probe right before the kernel write sees that
/// freeze, so harw writes nothing to the cgroup (its write would be a
/// no-op, recorded as harw's own, which a later `job.resume` would thaw).
/// The fresh suspension is taken back, in memory and on disk (never
/// `applied`); the operator's freeze stays, and `job.resume` refuses to
/// lift it. A Warden marker that shows up in the same window (before the
/// Warden's own freeze) refuses the freeze the same way.
#[test]
fn a_freeze_outside_harw_during_the_intent_sync_is_never_taken_over() -> TestResult {
    let worker = Caller::Agent("worker");

    // An operator's freeze, without a marker.
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-recheck-operator", "v1")?;
    let operator = {
        let fake = Arc::clone(&fake);
        move || fake.set_outside("harw-job-v1", true, true)
    };
    let sync = ScriptedDirSync::new(&env, &fake, &id, 0)?
        .on_first_call(operator)
        .install(&env);
    match env.manager.suspend(&id, worker) {
        Err(JobError::FrozenElsewhere(job)) => assert_eq!(job, "job-recheck-operator"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    // The intent was synced; then no kernel write at all.
    assert_eq!(sync.calls(), synced_after_rename());
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    for (place, meta) in [
        ("memory", status_of(&env, &id)?.meta),
        ("disk", on_disk(&env, &id)?),
    ] {
        assert_eq!(meta.state, JobState::Running, "{place}");
        assert_eq!(meta.suspended_at, None, "{place}");
        assert_eq!(meta.suspend_phase, None, "{place}");
    }
    match env.manager.resume(&id, worker) {
        Err(JobError::FrozenElsewhere(job)) => assert_eq!(job, "job-recheck-operator"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let probe = freezer(&fake);
    assert_eq!(probe.freeze_requested("harw-job-v1"), Ok(true));
    assert_eq!(probe.is_frozen("harw-job-v1"), Ok(true));

    // The Warden's marker, written before its own freeze.
    let fake = Arc::new(FakeCgroup::default());
    let holds = Arc::new(FakeHolds::default());
    let env = env_with_holds(&fake, &holds)?;
    let id = running_job(&env, "job-recheck-warden", "v2")?;
    let sync = ScriptedDirSync::new(&env, &fake, &id, 0)?
        .on_first_call(warden_marks(&holds, "v2"))
        .install(&env);
    match env.manager.suspend(&id, worker) {
        Err(JobError::FrozenElsewhere(job)) => assert_eq!(job, "job-recheck-warden"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(sync.calls(), synced_after_rename());
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Running);
    assert_eq!(persisted.suspended_at, None);
    assert_eq!(persisted.suspend_phase, None);
    Ok(())
}

/// As above, and the rollback of the fresh mark cannot be written either
/// (a deferred write error, armed while the intent is synced): harw still
/// writes nothing to the cgroup, and the refusal comes with the failed
/// rollback. Memory says running, the disk keeps only the intent (never
/// `applied`). After a restart (the process still alive) that stale intent
/// is an unresolved suspension: `job.resume` refuses it, and the
/// operator's freeze stays.
#[test]
fn a_refused_freeze_whose_rollback_cannot_be_written_stays_fail_closed() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-recheck-unwritten", "v3")?;
    let entry = env
        .manager
        .entry(&id, Caller::Operator)
        .map_err(ctx("entry"))?;
    let operator = {
        let fake = Arc::clone(&fake);
        move || {
            fake.set_outside("harw-job-v3", true, true);
            entry.fail_next_durable_writes(1);
        }
    };
    let sync = ScriptedDirSync::new(&env, &fake, &id, 0)?
        .on_first_call(operator)
        .install(&env);
    let worker = Caller::Agent("worker");
    match env.manager.suspend(&id, worker) {
        Err(JobError::SuspendRefusedRollbackNotRecorded {
            job,
            refusal,
            persist,
        }) => {
            assert_eq!(job, "job-recheck-unwritten");
            assert_eq!(persist.to_string(), "injected durable write failure");
            match *refusal {
                JobError::FrozenElsewhere(refused) => assert_eq!(refused, "job-recheck-unwritten"),
                other => return Err(TestError::Unexpected(format!("refusal: {other:?}"))),
            }
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(sync.calls(), synced_after_rename());
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Running);
    assert_eq!(status.meta.suspended_at, None);
    let stale = on_disk(&env, &id)?;
    assert_eq!(stale.state, JobState::Suspended);
    assert!(stale.suspended_at.is_some());
    assert_eq!(stale.suspend_phase, Some(SuspendPhase::Intent));

    // harw restarts; the job's process still lives.
    let holds = Arc::new(FakeHolds::default());
    let (reloaded, _recorder) = restart(&env, &fake, &holds)?;
    let reloaded_status = reloaded
        .status(&id, Caller::Operator)
        .map_err(ctx("status"))?;
    assert_eq!(reloaded_status.meta.state, JobState::Detached);
    assert!(reloaded_status.meta.is_suspension_unresolved());
    match reloaded.resume(&id, worker) {
        Err(JobError::SuspensionUnresolved(job)) => assert_eq!(job, "job-recheck-unwritten"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let probe = freezer(&fake);
    assert_eq!(probe.freeze_requested("harw-job-v3"), Ok(true));
    assert_eq!(probe.is_frozen("harw-job-v3"), Ok(true));
    Ok(())
}

/// The same window on a re-issued `job.suspend` that would resolve an
/// unresolved suspension: an operator freezes the cgroup (no marker) while
/// the intent is made durable again. The probe right before the kernel
/// write refuses (`SuspensionUnresolved`): harw cannot prove that freeze is
/// its own, writes nothing to the cgroup and never records `applied`. The
/// suspension stays unresolved as it was, `job.resume` still refuses, and
/// the operator's freeze stays in place.
#[test]
fn a_freeze_outside_harw_during_a_reissue_keeps_the_suspension_unresolved() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let (id, since) = unresolved_job(&env, "job-reissue-raced", "v4", false)?;
    let operator = {
        let fake = Arc::clone(&fake);
        move || fake.set_outside("harw-job-v4", true, true)
    };
    let sync = ScriptedDirSync::new(&env, &fake, &id, 0)?
        .on_first_call(operator)
        .install(&env);
    match env.manager.suspend(&id, Caller::Agent("worker")) {
        Err(JobError::SuspensionUnresolved(job)) => assert_eq!(job, "job-reissue-raced"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    assert_eq!(sync.calls(), synced_after_rename());
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    assert_still_unresolved(&env, &id, since)?;
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let probe = freezer(&fake);
    assert_eq!(probe.freeze_requested("harw-job-v4"), Ok(true));
    assert_eq!(probe.is_frozen("harw-job-v4"), Ok(true));
    Ok(())
}

/// A refused `job.resume` takes its release back (`applied` again), but
/// that write fails (a deferred write error, armed together with an
/// untrusted lease after the release was written): nothing is thawed, the
/// refusal stays the `probe` failure. In this session memory governs: the
/// suspension stays harw's own (`applied`), and the next `job.resume` thaws
/// it and corrects the record. Until then the disk holds only the intent of
/// the release: the safe side (a restart would refuse to thaw, never thaw
/// a freeze it cannot prove).
#[test]
fn a_failed_restore_after_a_refused_resume_leaves_only_the_intent_on_disk() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let holds = Arc::new(FakeHolds::default());
    let env = Env::new()?;
    let id = running_job(&env, "job-restore-unwritten", "x7")?;
    let entry = env
        .manager
        .entry(&id, Caller::Operator)
        .map_err(ctx("entry"))?;
    let untrusted = {
        let holds = Arc::clone(&holds);
        move || {
            holds.set_lease_unavailable(true);
            entry.fail_next_durable_writes(1);
        }
    };
    env.manager.set_freezer(Arc::new(
        HookFreezer::new(freezer_with_holds(&fake, &holds)).before_lease(untrusted),
    ));
    let worker = Caller::Agent("worker");

    env.manager.suspend(&id, worker).map_err(ctx("suspend"))?;
    match env.manager.resume(&id, worker) {
        Err(JobError::Freeze {
            job,
            operation,
            source,
        }) => {
            assert_eq!(job, "job-restore-unwritten");
            assert_eq!(operation, "probe");
            assert!(
                matches!(source, FreezeError::ContainmentIndexUnreadable { .. }),
                "{source:?}"
            );
        }
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
    // Nothing thawed.
    assert_eq!(fake.calls(), vec![call("freeze", "x7")]);
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Suspended);
    assert_eq!(status.meta.suspension_phase(), Some(SuspendPhase::Applied));
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert_eq!(persisted.suspended_at, status.meta.suspended_at);
    assert_eq!(persisted.suspend_phase, Some(SuspendPhase::Intent));

    holds.set_lease_unavailable(false);
    let resumed = env.manager.resume(&id, worker).map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(fake.calls(), vec![call("freeze", "x7"), call("thaw", "x7")]);
    let corrected = on_disk(&env, &id)?;
    assert_eq!(corrected.state, JobState::Running);
    assert_eq!(corrected.suspended_at, None);
    assert_eq!(corrected.suspend_phase, None);
    Ok(())
}
