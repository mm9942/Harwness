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
//! Wiedereinfrieren und eine nach dem Auftauen verschwundene cgroup.

use super::{
    Caller, EntryState, JobEntry, JobError, JobManager, SUSPENDED_ON_RELOAD, SuspendTransition,
    lock, read_meta, stop_signal_for, write_meta,
};
use crate::event::RecordingNotifier;
use crate::event::{JobEvent, JobNotifier};
use crate::freeze::{ContainmentLease, FreezeError, JobFreezer};
use crate::model::{JobId, JobMeta, JobState, JobStatus, META_FILE};
use crate::procfs::JobSignal;
use crate::test_support::fake_cgroup::{
    FakeCgroup, FakeHolds, fake_kernel_id, freezer, freezer_over, freezer_with_holds, marker_index,
    write_marker,
};
use crate::test_support::{Env, TestError, TestResult, ctx, request, stub_job_meta};
use jiff::Timestamp;
use std::fs;
use std::os::unix::process::CommandExt as _;
use std::sync::atomic::{AtomicBool, Ordering};
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
/// since `secs` seconds, with a proven own cgroup and our child `sleeper`
/// as its adopted group leader.
fn detached_suspended_job(
    env: &Env,
    id: &str,
    attempt: &str,
    sleeper: &Sleeper,
    secs: i64,
) -> TestResult<JobId> {
    let mut meta = stub_job_meta(id, JobState::Detached, "worker", &[], Some(attempt))?;
    meta.detached = true;
    meta.pid = Some(sleeper.pid());
    meta.started_at = Some(ago(secs.saturating_add(10))?);
    meta.suspended_at = Some(ago(secs)?);
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
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert!(persisted.suspended_at.is_some());
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
/// suspended time.
#[tokio::test]
async fn stopping_a_detached_suspended_job_kills_it_and_closes_the_suspension() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let sleeper = Sleeper::spawn()?;
    let id = detached_suspended_job(&env, "job-detached-stop", "k1", &sleeper, 30)?;

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
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Stopped);
    assert_eq!(persisted.suspended_at, None);
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
    let id = detached_suspended_job(&env, "job-detached-resume", "r1", &sleeper, 30)?;
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
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Unknown);
    assert_eq!(persisted.suspended_at, None);
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
    let persisted = read_meta(&dir).ok_or(TestError::Missing("meta.json"))?;
    assert_eq!(persisted.state, JobState::Unknown);
    assert_eq!(persisted.suspended_at, None);
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

/// A job that was suspended when harw ended stays visibly suspended after
/// the restart (B13), its owner is told, and `job.resume` without a proven
/// cgroup fails closed. The stub's process is this test process, so the
/// test never stops it.
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
    // The record was written before the freeze.
    assert!(on_disk(&env, &id)?.suspended_at.is_some());
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
/// (`freeze` or `thaw`), e.g. the Warden containing the cgroup or the
/// state directory turning unwritable at that moment, then delegates.
struct HookFreezer {
    inner: Arc<dyn JobFreezer>,
    before_freeze: Mutex<Option<Hook>>,
    before_thaw: Mutex<Option<Hook>>,
}

impl HookFreezer {
    fn new(inner: Arc<dyn JobFreezer>) -> Self {
        Self {
            inner,
            before_freeze: Mutex::new(None),
            before_thaw: Mutex::new(None),
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
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert!(persisted.suspended_at.is_some());

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
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Suspended);
    assert!(status.meta.suspended_at.is_some());
    assert_eq!(on_disk(&env, &id)?.state, JobState::Suspended);
    Ok(())
}

/// Fail closed: while the containment index cannot be read, harw cannot
/// tell its own freeze from a Warden containment and thaws nothing; the job
/// stays suspended. A readable index lets `job.resume` work again.
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
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Suspended);
    assert!(status.meta.suspended_at.is_some());
    assert_eq!(on_disk(&env, &id)?.state, JobState::Suspended);

    holds.set_unreadable(false);
    let resumed = env.manager.resume(&id, worker).map_err(ctx("resume"))?;
    assert_eq!(resumed.transition, SuspendTransition::Resumed);
    assert_eq!(fake.calls(), vec![call("freeze", "z3"), call("thaw", "z3")]);
    Ok(())
}

/// The index turns unreadable right after the thaw: harw cannot rule out a
/// containment that came in between, so it freezes again (fail closed) and
/// the job stays suspended.
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
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Suspended);
    assert!(status.meta.suspended_at.is_some());
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
/// on. Once the directory is writable again, `job.suspend` works.
#[test]
fn an_unwritable_state_directory_aborts_suspend_before_freezing() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let env = env_with_freezer(&fake)?;
    let id = running_job(&env, "job-readonly", "y1")?;
    let blocker = record_blocker(&env, &id)?;
    fs::create_dir(&blocker).map_err(ctx("block meta.json"))?;

    match env.manager.suspend(&id, Caller::Agent("worker")) {
        Err(JobError::SuspendNotRecorded { job, .. }) => assert_eq!(job, "job-readonly"),
        other => return Err(TestError::Unexpected(format!("{other:?}"))),
    }
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
    // The intent reached the disk before the freeze; its rollback did not.
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert!(persisted.suspended_at.is_some());
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
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Suspended);
    assert_eq!(on_disk(&env, &id)?.state, JobState::Suspended);

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

    // Writable again: the next write corrects the record.
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
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert!(persisted.suspended_at.is_some());
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
/// suspended in memory and on disk. With a usable lease it thaws.
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
    let status = status_of(&env, &id)?;
    assert_eq!(status.meta.state, JobState::Suspended);
    assert!(status.meta.suspended_at.is_some());
    let persisted = on_disk(&env, &id)?;
    assert_eq!(persisted.state, JobState::Suspended);
    assert!(persisted.suspended_at.is_some());

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
