//! Behavior tests of the record store and the fenced job store.

use std::io;
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use harw_job_core::{
    Budget, Job, JobCancellation, JobKind, JobOutcome, JobScope, JobState, RetryPolicy, RunnerId,
    StoredJob, WorkId,
};
use harw_types::{ApprovalActor, TenantId, WorkspaceId};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::error::{Conflict, StaleLease, StoreError, StoreResult};
use crate::job_record_store::{ClaimTerms, FsJobRecordStore, JobRecordStore, JobTransition};
use crate::mechanics::token_fencing;
use crate::record_store::{RecordStore, StoreRecord};
use crate::test_support::{TestError, TestResult, ctx};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Note {
    id: String,
    value: u64,
}

impl StoreRecord for Note {
    fn record_id(&self) -> &str {
        &self.id
    }
}

fn note(id: &str, value: u64) -> Note {
    Note {
        id: id.to_owned(),
        value,
    }
}

fn note_store(root: &Path) -> TestResult<RecordStore<Note>> {
    Ok(RecordStore::create_ambient(&root.join("notes"))?)
}

fn records_path(root: &Path) -> std::path::PathBuf {
    root.join("notes").join("records")
}

fn later(now: Timestamp, secs: i64) -> TestResult<Timestamp> {
    now.checked_add(SignedDuration::from_secs(secs))
        .map_err(ctx("timestamp arithmetic"))
}

fn stored_job(id: &str, now: Timestamp) -> TestResult<StoredJob> {
    let mut job = Job::new(
        WorkId::from_str(id),
        JobKind::Worker,
        Budget::unbounded(),
        RetryPolicy {
            max_attempts: 2,
            base_delay: SignedDuration::from_secs(1),
            factor: 2.0,
            max_delay: SignedDuration::from_secs(10),
        },
        now,
    );
    job.mark_ready(now)
        .map_err(ctx("mark_ready on a fresh job"))?;
    Ok(StoredJob {
        job,
        scope: JobScope::new(
            TenantId::from_str("test-tenant"),
            WorkspaceId::from_str("test-workspace"),
            ApprovalActor::Operator {
                id: "test-operator".to_owned(),
            },
        ),
        input: serde_json::json!({"task": "review"}),
        submitted_at: now,
        not_before: now,
        lease: None,
        lease_epoch: 0,
        completion: None,
        cancellation: None,
        revision: 0,
        trace: None,
    })
}

fn runner(name: &str) -> TestResult<RunnerId> {
    RunnerId::new(name).map_err(ctx("runner id"))
}

fn terms(ttl_secs: i64, now: Timestamp) -> ClaimTerms {
    ClaimTerms {
        lease_ttl: SignedDuration::from_secs(ttl_secs),
        now,
    }
}

fn succeeded() -> JobOutcome {
    JobOutcome::Succeeded {
        result: serde_json::json!({}),
    }
}

fn channel_error(error: impl std::fmt::Display) -> StoreError {
    StoreError::Io(io::Error::other(error.to_string()))
}

fn increment(store: &RecordStore<Note>, id: &str) -> StoreResult<()> {
    loop {
        let outcome = store.update_locked(id, |record: &mut Note| {
            record.value += 1;
            Ok::<(), StoreError>(())
        });
        match outcome {
            Err(StoreError::Contended { .. }) => thread::yield_now(),
            other => return other,
        }
    }
}

fn file_names(dir: &Path) -> TestResult<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name();
        let name = name
            .to_str()
            .ok_or(TestError::Missing("UTF-8 file name"))?
            .to_owned();
        names.push(name);
    }
    names.sort();
    Ok(names)
}

// --- generic record store -------------------------------------------------

#[test]
fn create_then_load_round_trips_a_record_in_the_documented_layout() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = note_store(temp.path())?;
    store.create(&note("n-1", 7))?;

    assert_eq!(store.load("n-1")?, note("n-1", 7));
    assert!(records_path(temp.path()).join("n-1.json").is_file());
    assert!(temp.path().join("notes/locks/n-1.lock").is_file());
    let on_disk: Note =
        serde_json::from_slice(&std::fs::read(records_path(temp.path()).join("n-1.json"))?)?;
    assert_eq!(on_disk, note("n-1", 7));
    Ok(())
}

#[test]
fn create_never_overwrites_an_existing_record() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = note_store(temp.path())?;
    store.create(&note("n-once", 7))?;

    assert!(matches!(
        store.create(&note("n-once", 8)),
        Err(StoreError::AlreadyExists { ref id }) if id == "n-once"
    ));
    assert_eq!(store.load("n-once")?.value, 7);
    Ok(())
}

#[test]
fn missing_records_and_unsafe_ids_are_typed_errors() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = note_store(temp.path())?;

    assert!(matches!(
        store.load("absent"),
        Err(StoreError::NotFound { .. })
    ));
    assert!(matches!(
        store.load("../escape"),
        Err(StoreError::InvalidId { .. })
    ));
    assert!(matches!(
        store.create(&note("a/b", 1)),
        Err(StoreError::InvalidId { .. })
    ));
    assert!(RecordStore::<Note>::open_existing_ambient(&temp.path().join("nope"))?.is_none());
    Ok(())
}

#[test]
fn a_held_record_lock_rejects_a_concurrent_writer_without_blocking() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = note_store(temp.path())?;
    store.create(&note("n-lock", 0))?;
    let shared = &store;
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();

    let held = thread::scope(|scope| -> TestResult<StoreResult<u64>> {
        let holder = scope.spawn(move || {
            shared.update_locked("n-lock", |record: &mut Note| {
                record.value += 1;
                entered_tx.send(()).map_err(channel_error)?;
                release_rx.recv().map_err(channel_error)?;
                Ok::<u64, StoreError>(record.value)
            })
        });
        entered_rx.recv().map_err(ctx("holder entered the lock"))?;

        let contended = shared.update_locked("n-lock", |record: &mut Note| {
            record.value += 100;
            Ok::<u64, StoreError>(record.value)
        });
        assert!(matches!(contended, Err(StoreError::Contended { .. })));

        release_tx.send(()).map_err(ctx("release the holder"))?;
        holder
            .join()
            .map_err(|_| TestError::Unexpected("lock holder thread panicked".to_owned()))
    })?;

    assert_eq!(held?, 1);
    assert_eq!(store.load("n-lock")?.value, 1);
    Ok(())
}

#[test]
fn concurrent_updates_are_serialized_by_the_record_lock() -> TestResult {
    const PER_THREAD: u64 = 25;
    let temp = tempfile::tempdir()?;
    let store = note_store(temp.path())?;
    store.create(&note("n-count", 0))?;
    let shared = &store;

    let results = thread::scope(|scope| {
        let workers: Vec<_> = (0..2)
            .map(|_| {
                scope.spawn(move || -> StoreResult<()> {
                    for _ in 0..PER_THREAD {
                        increment(shared, "n-count")?;
                    }
                    Ok(())
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join())
            .collect::<Vec<_>>()
    });
    for result in results {
        result.map_err(|_| TestError::Unexpected("worker thread panicked".to_owned()))??;
    }

    assert_eq!(store.load("n-count")?.value, 2 * PER_THREAD);
    Ok(())
}

#[test]
fn a_failed_mutation_writes_nothing() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = note_store(temp.path())?;
    store.create(&note("n-keep", 3))?;
    let path = records_path(temp.path()).join("n-keep.json");
    let before = std::fs::read(&path)?;

    let outcome = store.update_locked("n-keep", |record: &mut Note| {
        record.value = 99;
        Err::<(), StoreError>(StoreError::conflict(
            "n-keep",
            Conflict::Rejected {
                detail: "refused".to_owned(),
            },
        ))
    });

    assert!(matches!(outcome, Err(StoreError::Conflict { .. })));
    assert_eq!(std::fs::read(&path)?, before);
    Ok(())
}

#[test]
fn crash_orphaned_temp_files_are_ignored_and_swept_under_the_lock() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = note_store(temp.path())?;
    store.create(&note("n-crash", 1))?;
    let records = records_path(temp.path());
    // A crash between temp write and rename leaves a half-written temp file.
    std::fs::write(
        records.join(".n-crash.4242-1-0.tmp"),
        b"{\"id\":\"n-crash\",\"val",
    )?;
    // A legacy `tempfile` leftover cannot be attributed to a record.
    std::fs::write(records.join(".tmpAbC123"), b"{")?;

    let page = store.list(None, 10, |_| true)?;
    assert_eq!(page.records, vec![note("n-crash", 1)]);
    assert_eq!(store.load("n-crash")?, note("n-crash", 1));

    assert_eq!(store.remove_orphaned_temps()?, 1);
    assert_eq!(file_names(&records)?, vec![".tmpAbC123", "n-crash.json"]);

    // Regular writes never leave temp files behind.
    store.update_locked("n-crash", |record: &mut Note| {
        record.value = 2;
        Ok::<(), StoreError>(())
    })?;
    assert_eq!(file_names(&records)?, vec![".tmpAbC123", "n-crash.json"]);
    assert_eq!(store.load("n-crash")?.value, 2);
    Ok(())
}

#[test]
fn a_corrupt_record_is_reported_and_quarantined_under_its_lock() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = note_store(temp.path())?;
    store.ensure_layout()?;
    let records = records_path(temp.path());
    std::fs::write(records.join("n-bad.json"), b"{ not json")?;
    std::fs::write(
        records.join("n-other.json"),
        serde_json::to_vec(&note("n-elsewhere", 1))?,
    )?;

    assert!(matches!(
        store.load("n-bad"),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(matches!(
        store.load("n-other"),
        Err(StoreError::Corrupt { .. })
    ));

    let outcome = store.update_locked("n-bad", |record: &mut Note| {
        record.value = 1;
        Ok::<(), StoreError>(())
    });
    assert!(matches!(outcome, Err(StoreError::Corrupt { .. })));
    let names = file_names(&records)?;
    assert!(
        names
            .iter()
            .any(|name| name.starts_with("n-bad.json.corrupt-"))
    );
    assert!(!names.iter().any(|name| name == "n-bad.json"));
    assert!(matches!(
        store.load("n-bad"),
        Err(StoreError::NotFound { .. })
    ));
    Ok(())
}

#[test]
fn list_skips_and_quarantines_corrupt_records_instead_of_failing() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = note_store(temp.path())?;
    store.create(&note("n-a", 1))?;
    store.create(&note("n-b", 2))?;
    let records = records_path(temp.path());
    std::fs::write(records.join("n-c.json"), b"{ truncated")?;

    let page = store.list(None, 10, |_| true)?;

    assert_eq!(page.records, vec![note("n-a", 1), note("n-b", 2)]);
    assert!(
        file_names(&records)?
            .iter()
            .any(|name| name.starts_with("n-c.json.corrupt-"))
    );
    Ok(())
}

#[test]
fn list_paginates_by_id_with_cursor_filter_and_clamped_limit() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = note_store(temp.path())?;
    for (value, id) in (0_u64..).zip(["n-e", "n-c", "n-a", "n-d", "n-b"]) {
        store.create(&note(id, value))?;
    }
    let ids = |page: &crate::Page<Note>| -> Vec<String> {
        page.records
            .iter()
            .map(|record| record.id.clone())
            .collect()
    };

    let first = store.list(None, 2, |_| true)?;
    assert_eq!(ids(&first), vec!["n-a", "n-b"]);
    assert_eq!(first.next_cursor.as_deref(), Some("n-b"));
    let second = store.list(first.next_cursor.as_deref(), 2, |_| true)?;
    assert_eq!(ids(&second), vec!["n-c", "n-d"]);
    let third = store.list(second.next_cursor.as_deref(), 2, |_| true)?;
    assert_eq!(ids(&third), vec!["n-e"]);
    assert_eq!(third.next_cursor, None);

    let filtered = store.list(None, 10, |record| record.value >= 2)?;
    assert_eq!(ids(&filtered), vec!["n-a", "n-b", "n-d"]);
    assert_eq!(store.list(None, 0, |_| true)?.records.len(), 1);
    assert_eq!(store.list_all(|_| true)?.len(), 5);
    Ok(())
}

#[test]
fn sidecars_round_trip_and_reserved_kinds_are_rejected() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = note_store(temp.path())?;

    assert_eq!(store.read_sidecar("approvals", "n-1")?, None);
    store.write_sidecar("approvals", "n-1", b"{\"ok\":true}")?;
    assert_eq!(
        store.read_sidecar("approvals", "n-1")?,
        Some(b"{\"ok\":true}".to_vec())
    );
    assert!(temp.path().join("notes/approvals/n-1.json").is_file());
    assert!(matches!(
        store.write_sidecar("records", "n-1", b"{}"),
        Err(StoreError::InvalidId { .. })
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn records_are_created_owner_only_like_tempfile() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let store = note_store(temp.path())?;
    store.create(&note("n-mode", 1))?;
    let path = records_path(temp.path()).join("n-mode.json");
    assert_eq!(
        std::fs::metadata(&path)?.permissions().mode() & 0o777,
        0o600
    );

    store.update_locked("n-mode", |record: &mut Note| {
        record.value = 2;
        Ok::<(), StoreError>(())
    })?;
    assert_eq!(
        std::fs::metadata(&path)?.permissions().mode() & 0o777,
        0o600
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinked_records_are_rejected_without_touching_their_target() -> TestResult {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let store = note_store(temp.path())?;
    store.create(&note("n-link", 1))?;
    let path = records_path(temp.path()).join("n-link.json");
    std::fs::remove_file(&path)?;
    let external = temp.path().join("external.json");
    let external_bytes = serde_json::to_vec(&note("n-link", 5))?;
    std::fs::write(&external, &external_bytes)?;
    symlink(&external, &path)?;

    assert!(matches!(store.load("n-link"), Err(StoreError::Io(_))));
    let outcome = store.update_locked("n-link", |record: &mut Note| {
        record.value = 9;
        Ok::<(), StoreError>(())
    });
    assert!(matches!(outcome, Err(StoreError::Io(_))));
    assert!(store.list(None, 10, |_| true)?.records.is_empty());
    assert_eq!(std::fs::read(&external)?, external_bytes);
    Ok(())
}

// --- fenced job store -----------------------------------------------------

#[test]
fn a_stale_runner_cannot_mutate_after_its_lease_was_reclaimed() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = FsJobRecordStore::create_ambient(&temp.path().join("jobs"))?;
    let now = Timestamp::now();
    let work_id = WorkId::from_str("job-fenced");
    store.create_job(stored_job(work_id.as_str(), now)?)?;

    let first = store.claim_job(&work_id, &runner("runner-a")?, terms(1, now))?;
    let expiry = later(now, 2)?;
    let page = store.reconcile_expired(expiry, 100, None)?;
    assert_eq!(page.expired.len(), 1);
    assert_eq!(page.next_cursor, None);

    let second = store.claim_job(&work_id, &runner("runner-b")?, terms(60, later(expiry, 2)?))?;
    assert!(token_fencing(&second.token).supersedes(token_fencing(&first.token)));

    let before = store.load(&work_id)?;
    let stale_complete = store.transition(
        &first,
        JobTransition::Complete {
            completed_at: later(expiry, 3)?,
            outcome: succeeded(),
        },
    );
    assert!(matches!(
        stale_complete,
        Err(StoreError::StaleLease {
            reason: StaleLease::TokenMismatch,
            ..
        })
    ));
    let stale_renew = store.transition(
        &first,
        JobTransition::Renew {
            now: later(expiry, 3)?,
        },
    );
    assert!(stale_renew.as_ref().is_err_and(StoreError::is_lease_lost));
    assert_eq!(store.load(&work_id)?, before);

    let done = store.transition(
        &second,
        JobTransition::Complete {
            completed_at: later(expiry, 4)?,
            outcome: succeeded(),
        },
    )?;
    assert_eq!(done.job.state, JobState::Completed);
    assert!(done.lease.is_none());
    Ok(())
}

#[test]
fn an_expired_lease_can_neither_renew_nor_complete() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = FsJobRecordStore::create_ambient(&temp.path().join("jobs"))?;
    let now = Timestamp::now();
    let work_id = WorkId::from_str("job-expired");
    store.create_job(stored_job(work_id.as_str(), now)?)?;
    let claim = store.claim_job(&work_id, &runner("runner-a")?, terms(1, now))?;

    let renew = store.transition(
        &claim,
        JobTransition::Renew {
            now: later(now, 2)?,
        },
    );
    assert!(matches!(
        renew,
        Err(StoreError::StaleLease {
            reason: StaleLease::Expired { .. },
            ..
        })
    ));
    let renewed = store.transition(
        &claim,
        JobTransition::Renew {
            now: now
                .checked_add(SignedDuration::from_millis(500))
                .map_err(ctx("now + 0.5s"))?,
        },
    )?;
    assert_eq!(renewed.job.state, JobState::Running);
    Ok(())
}

#[test]
fn claim_rejects_non_ready_ineligible_and_invalid_requests() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = FsJobRecordStore::create_ambient(&temp.path().join("jobs"))?;
    let now = Timestamp::now();
    let mut pending = stored_job("job-pending", now)?;
    pending.job.state = JobState::Pending;
    store.create_job(pending)?;
    let mut future = stored_job("job-future", now)?;
    future.not_before = later(now, 60)?;
    store.create_job(future)?;
    let runner_a = runner("runner-a")?;

    assert!(matches!(
        store.claim_job(&WorkId::from_str("job-pending"), &runner_a, terms(60, now)),
        Err(StoreError::Conflict {
            conflict: Conflict::NotClaimable {
                state: JobState::Pending
            },
            ..
        })
    ));
    assert!(matches!(
        store.claim_job(&WorkId::from_str("job-future"), &runner_a, terms(60, now)),
        Err(StoreError::Conflict {
            conflict: Conflict::NotEligible { .. },
            ..
        })
    ));
    assert!(matches!(
        store.claim_job(&WorkId::from_str("job-future"), &runner_a, terms(0, now)),
        Err(StoreError::Conflict {
            conflict: Conflict::InvalidLeaseTtl,
            ..
        })
    ));
    assert!(matches!(
        store.claim_job(&WorkId::from_str("job-absent"), &runner_a, terms(60, now)),
        Err(StoreError::NotFound { .. })
    ));
    Ok(())
}

#[test]
fn cancellation_fences_the_running_lease() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = FsJobRecordStore::create_ambient(&temp.path().join("jobs"))?;
    let now = Timestamp::now();
    let work_id = WorkId::from_str("job-cancel");
    store.create_job(stored_job(work_id.as_str(), now)?)?;
    let claim = store.claim_job(&work_id, &runner("runner-a")?, terms(60, now))?;

    let cancelled = store.cancel_job(
        &work_id,
        JobCancellation {
            cancelled_at: later(now, 1)?,
            cancelled_by: ApprovalActor::Operator {
                id: "operator-a".to_owned(),
            },
            reason: "operator stopped the task".to_owned(),
        },
    )?;
    assert_eq!(cancelled.previous_state, JobState::Running);
    assert_eq!(cancelled.prior_lease, Some(claim.lease.clone()));
    let persisted = store.load(&work_id)?;
    assert_eq!(persisted.job.state, JobState::Cancelled);
    assert!(persisted.lease_epoch > claim.token.epoch);

    assert!(matches!(
        store.transition(
            &claim,
            JobTransition::Renew {
                now: later(now, 2)?,
            },
        ),
        Err(StoreError::StaleLease {
            reason: StaleLease::Missing,
            ..
        })
    ));
    Ok(())
}

#[test]
fn the_recovery_set_holds_exactly_the_running_jobs_of_one_runner() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = FsJobRecordStore::create_ambient(&temp.path().join("jobs"))?;
    let now = Timestamp::now();
    let runner_a = runner("runner-a")?;
    let runner_b = runner("runner-b")?;
    for id in ["job-a1", "job-b1", "job-ready", "job-a-done"] {
        store.create_job(stored_job(id, now)?)?;
    }
    store.claim_job(&WorkId::from_str("job-a1"), &runner_a, terms(60, now))?;
    store.claim_job(&WorkId::from_str("job-b1"), &runner_b, terms(60, now))?;
    let done = store.claim_job(&WorkId::from_str("job-a-done"), &runner_a, terms(60, now))?;
    store.transition(
        &done,
        JobTransition::Complete {
            completed_at: later(now, 1)?,
            outcome: succeeded(),
        },
    )?;

    let recovery = store.load_recovery_set(&runner_a)?;

    let ids: Vec<&str> = recovery
        .iter()
        .map(|record| record.job.id.as_str())
        .collect();
    assert_eq!(ids, vec!["job-a1"]);
    assert!(store.load_recovery_set(&runner("runner-c")?)?.is_empty());
    Ok(())
}

#[test]
fn a_stored_job_survives_the_store_round_trip_unchanged() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = FsJobRecordStore::create_ambient(&temp.path().join("jobs"))?;
    let record = stored_job("job-round-trip", Timestamp::now())?;

    let created = store.create_job(record.clone())?;

    assert_eq!(created, record);
    assert_eq!(store.load(&WorkId::from_str("job-round-trip"))?, record);
    let raw = std::fs::read(temp.path().join("jobs/records/job-round-trip.json"))?;
    assert_eq!(raw, serde_json::to_vec(&record)?);
    Ok(())
}
