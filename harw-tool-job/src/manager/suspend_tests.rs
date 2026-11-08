//! Tests von `job.suspend` / `job.resume` im Manager (PL-100 WP 3.5) mit
//! einem gefälschten cgroup-Backend hinter dem echten
//! `CgroupFreezer`: Besitz wie `job.stop`, Ziel nur die bewiesene
//! eigene cgroup, kein Übernehmen oder Aufheben eines fremden Freeze,
//! Absicht vor Wirkung, Idempotenz, typisierte Fehler, Stop ohne Auftauen,
//! Ablösen, Neustart und Wettläufe mit dem Ende des Jobs.

use super::*;
use crate::event::RecordingNotifier;
use crate::test_support::fake_cgroup::{FakeCgroup, freezer};
use crate::test_support::{Env, TestError, TestResult, ctx, request, stub_job_meta};
use std::os::unix::process::CommandExt as _;

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

/// `Env` with a [`RacingFreezer`] armed for the running stub job `id`.
fn racing_env(
    fake: &Arc<FakeCgroup>,
    id: &str,
    attempt: &str,
    race: fn(&mut EntryState),
) -> TestResult<(Env, JobId)> {
    let env = Env::new()?;
    let job = running_job(&env, id, attempt)?;
    let entry = env
        .manager
        .entry(&job, Caller::Operator)
        .map_err(ctx("entry"))?;
    env.manager.set_freezer(Arc::new(RacingFreezer {
        inner: freezer(fake),
        entry: Mutex::new(Some(entry)),
        race,
    }));
    Ok((env, job))
}

/// The job ends between the record and the freeze: the freeze is undone at
/// once (no frozen cgroup outlives a refusal) and the refusal is typed.
#[test]
fn a_job_that_ends_while_being_frozen_is_thawed_again() -> TestResult {
    let fake = Arc::new(FakeCgroup::default());
    let (env, id) = racing_env(&fake, "job-race-end", "q1", end_job)?;
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
    let (env, id) = racing_env(&fake, "job-race-detach", "q2", detach_job)?;
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
