//! A-STORE integration tests: tail repair under lock, per-record write limit,
//! quarantine of corrupt files, paginated reconciliation, `unblock` and
//! `persist_noclobber` (F-068, F-156, G-020, F-179).
//!
//! `unwrap()` is used throughout: these are tests, a panic is the failure signal.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Barrier};

use harw_job_runtime::{
    Budget, Job, JobKind, JobOutcome, JobScope, JobState, RetryPolicy, StoredJob,
};
use harw_session_store::job_store::ReconcilePage;
use harw_session_store::store::{MAX_RECORD_BYTES, TailRepair, persist_noclobber};
use harw_session_store::{
    ChildLeaseRecord, ChildLeaseStore, ClaimRequest, CompleteRequest, Freeze, FreezeStore,
    JobListQuery, JobStore, RecordKind, SessionStoreError, SessionStoreResult, TranscriptRecord,
    TranscriptStore,
};
use harw_types::{
    ApprovalActor, CgroupId, FindingId, SessionId, TenantId, ThreadRef, ToolCallId, WorkId,
    WorkspaceId,
};
use jiff::{SignedDuration, Timestamp};

fn fixed_time() -> Timestamp {
    "2026-01-01T00:00:00Z".parse::<Timestamp>().unwrap()
}

fn transcript_record(session: &str, sequence: u64, text: &str) -> TranscriptRecord {
    TranscriptRecord::new(
        SessionId::from_str(session),
        ThreadRef::from_str("root"),
        sequence,
        fixed_time(),
        RecordKind::Turn,
        serde_json::json!({ "p": text }),
    )
}

fn quarantine_entries(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".corrupt-"))
        .collect()
}

fn replay(store: &TranscriptStore, session: &str) -> Vec<TranscriptRecord> {
    store
        .reader(&SessionId::from_str(session))
        .unwrap()
        .collect::<SessionStoreResult<Vec<_>>>()
        .unwrap()
}

/// Writes one complete record followed by a torn (unterminated) second line.
fn write_torn_transcript(store: &TranscriptStore, session: &str) -> String {
    store.append(&transcript_record(session, 0, "durable")).unwrap();
    let path = store.transcript_path(&SessionId::from_str(session)).unwrap();
    let torn = r#"{"session_id":"session-a","thread":"ro"#;
    let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(torn.as_bytes()).unwrap();
    torn.to_owned()
}

#[test]
fn test_repair_tail_truncates_torn_line_and_quarantines_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let store = TranscriptStore::new(temp.path());
    let torn = write_torn_transcript(&store, "session-a");

    let outcome = store.repair_tail(&SessionId::from_str("session-a")).unwrap();

    let TailRepair::Repaired {
        removed_bytes,
        quarantine,
    } = outcome
    else {
        panic!("expected a repair, got {outcome:?}");
    };
    assert_eq!(removed_bytes, torn.len() as u64);
    assert_eq!(std::fs::read_to_string(&quarantine).unwrap(), torn);
    let records = replay(&store, "session-a");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].sequence, 0);
    assert_eq!(
        store.repair_tail(&SessionId::from_str("session-a")).unwrap(),
        TailRepair::Clean
    );
}

#[test]
fn test_repair_tail_missing_transcript_is_not_found() {
    let temp = tempfile::tempdir().unwrap();
    let store = TranscriptStore::new(temp.path());

    assert!(matches!(
        store.repair_tail(&SessionId::from_str("session-missing")),
        Err(SessionStoreError::NotFound { .. })
    ));
}

#[test]
fn test_append_repairs_torn_tail_before_writing() {
    let temp = tempfile::tempdir().unwrap();
    let store = TranscriptStore::new(temp.path());
    write_torn_transcript(&store, "session-a");

    store.append(&transcript_record("session-a", 1, "next")).unwrap();

    let sequences: Vec<u64> = replay(&store, "session-a")
        .iter()
        .map(|record| record.sequence)
        .collect();
    assert_eq!(sequences, vec![0, 1]);
    assert_eq!(quarantine_entries(temp.path()).len(), 1);
}

#[test]
fn test_repair_tail_parallel_callers_repair_exactly_once() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(TranscriptStore::new(temp.path()));
    write_torn_transcript(&store, "session-a");
    let workers = 8;
    let barrier = Arc::new(Barrier::new(workers));

    let handles: Vec<_> = (0..workers)
        .map(|_| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                store.repair_tail(&SessionId::from_str("session-a"))
            })
        })
        .collect();
    let outcomes: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();

    let repaired = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, Ok(TailRepair::Repaired { .. })))
        .count();
    assert_eq!(repaired, 1, "outcomes: {outcomes:?}");
    assert!(outcomes.iter().all(|outcome| matches!(
        outcome,
        Ok(TailRepair::Repaired { .. } | TailRepair::Clean)
            | Err(SessionStoreError::LockContended { .. })
    )));
    assert_eq!(quarantine_entries(temp.path()).len(), 1);
    assert_eq!(replay(&store, "session-a").len(), 1);
}

/// Builds a record whose JSONL line (including `\n`) is exactly `line_len` bytes.
fn record_with_line_len(line_len: usize) -> TranscriptRecord {
    let base = transcript_record("session-a", 0, "").to_jsonl_line().unwrap().len();
    transcript_record("session-a", 0, &"a".repeat(line_len - base))
}

#[test]
fn test_append_accepts_record_of_exactly_the_limit() {
    let temp = tempfile::tempdir().unwrap();
    let store = TranscriptStore::new(temp.path());
    let record = record_with_line_len(MAX_RECORD_BYTES);
    assert_eq!(record.to_jsonl_line().unwrap().len(), MAX_RECORD_BYTES);

    store.append(&record).unwrap();

    assert_eq!(replay(&store, "session-a"), vec![record]);
}

#[test]
fn test_append_rejects_record_one_byte_over_the_limit() {
    let temp = tempfile::tempdir().unwrap();
    let store = TranscriptStore::new(temp.path());
    let record = record_with_line_len(MAX_RECORD_BYTES + 1);

    let error = store.append(&record).unwrap_err();

    assert!(matches!(
        error,
        SessionStoreError::TranscriptRecordTooLarge { size, limit, .. }
            if size == MAX_RECORD_BYTES + 1 && limit == MAX_RECORD_BYTES
    ));
    assert!(!store
        .transcript_path(&SessionId::from_str("session-a"))
        .unwrap()
        .exists());
}

#[test]
fn test_rewrite_rejects_record_one_byte_over_the_limit() {
    let temp = tempfile::tempdir().unwrap();
    let store = TranscriptStore::new(temp.path());
    store.append(&transcript_record("session-a", 0, "kept")).unwrap();

    let error = store
        .rewrite(
            &SessionId::from_str("session-a"),
            &[record_with_line_len(MAX_RECORD_BYTES + 1)],
        )
        .unwrap_err();

    assert!(matches!(error, SessionStoreError::TranscriptRecordTooLarge { .. }));
    assert_eq!(replay(&store, "session-a").len(), 1);
}

#[test]
fn test_persist_noclobber_writes_new_file_and_refuses_existing_target() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("record.json");

    persist_noclobber(&target, b"first").unwrap();
    let error = persist_noclobber(&target, b"second").unwrap_err();

    assert!(matches!(error, SessionStoreError::PersistTargetExists { .. }));
    assert_eq!(std::fs::read(&target).unwrap(), b"first");
    let leftovers = std::fs::read_dir(temp.path()).unwrap().count();
    assert_eq!(leftovers, 1, "the rejected temp file must not remain");
}

#[cfg(unix)]
#[test]
fn test_persist_noclobber_does_not_write_through_a_symlink() {
    let temp = tempfile::tempdir().unwrap();
    let outside = temp.path().join("outside.json");
    std::fs::write(&outside, b"untrusted").unwrap();
    let link = temp.path().join("record.json");
    std::os::unix::fs::symlink(&outside, &link).unwrap();

    let error = persist_noclobber(&link, b"payload").unwrap_err();

    assert!(matches!(error, SessionStoreError::PersistTargetExists { .. }));
    assert_eq!(std::fs::read(&outside).unwrap(), b"untrusted");
}

fn stored_job(id: &str, now: Timestamp) -> StoredJob {
    let mut job = Job::new(
        WorkId::from_str(id),
        JobKind::Worker,
        Budget::unbounded(),
        RetryPolicy {
            max_attempts: 3,
            base_delay: SignedDuration::from_secs(1),
            factor: 2.0,
            max_delay: SignedDuration::from_secs(10),
        },
        now,
    );
    job.mark_ready(now).unwrap();
    StoredJob {
        job,
        scope: JobScope::new(
            TenantId::from_str("tenant"),
            WorkspaceId::from_str("workspace"),
            ApprovalActor::Operator {
                id: "operator".to_owned(),
            },
        ),
        input: serde_json::json!({ "task": "review" }),
        submitted_at: now,
        not_before: now,
        lease: None,
        lease_epoch: 0,
        completion: None,
        cancellation: None,
        revision: 0,
        trace: None,
    }
}

#[test]
fn test_job_list_quarantines_corrupt_record_instead_of_failing() {
    let temp = tempfile::tempdir().unwrap();
    let store = JobStore::new(temp.path());
    store.admit(&stored_job("work-good", Timestamp::now())).unwrap();
    let records = store.root().join("records");
    std::fs::write(records.join("work-bad.json"), b"{ not json").unwrap();

    let page = store.list(&JobListQuery::default()).unwrap();

    assert_eq!(page.jobs.len(), 1);
    assert_eq!(page.jobs[0].job.id, WorkId::from_str("work-good"));
    let quarantined = quarantine_entries(&records);
    assert_eq!(quarantined.len(), 1);
    assert!(quarantined[0].starts_with("work-bad.json.corrupt-"));
    assert!(matches!(
        store.get(&WorkId::from_str("work-bad")),
        Err(SessionStoreError::JobNotFound { .. })
    ));
    assert_eq!(store.list(&JobListQuery::default()).unwrap().jobs.len(), 1);
}

#[test]
fn test_job_admit_twice_is_already_exists() {
    let temp = tempfile::tempdir().unwrap();
    let store = JobStore::new(temp.path());
    let record = stored_job("work-once", Timestamp::now());
    store.admit(&record).unwrap();

    assert!(matches!(
        store.admit(&record),
        Err(SessionStoreError::JobAlreadyExists { .. })
    ));
}

#[test]
fn test_reconcile_expired_paginates_with_limit_and_cursor() {
    let temp = tempfile::tempdir().unwrap();
    let store = JobStore::new(temp.path());
    let now = Timestamp::now();
    for id in ["work-a", "work-b", "work-c"] {
        store.admit(&stored_job(id, now)).unwrap();
        store
            .claim(
                &WorkId::from_str(id),
                &ClaimRequest {
                    worker_id: "worker".to_owned(),
                    lease_ttl: SignedDuration::from_secs(1),
                    now,
                },
            )
            .unwrap();
    }
    let later = now.checked_add(SignedDuration::from_secs(5)).unwrap();

    let first: ReconcilePage = store.reconcile_expired(later, 2, None).unwrap();
    let first_ids: Vec<&str> = first.expired.iter().map(|job| job.work_id.as_str()).collect();
    assert_eq!(first_ids, vec!["work-a", "work-b"]);
    let cursor = first.next_cursor.clone().unwrap();
    assert_eq!(cursor, WorkId::from_str("work-b"));

    let second = store.reconcile_expired(later, 2, Some(&cursor)).unwrap();
    let second_ids: Vec<&str> = second.expired.iter().map(|job| job.work_id.as_str()).collect();
    assert_eq!(second_ids, vec!["work-c"]);
    assert_eq!(second.next_cursor, None);

    for id in ["work-a", "work-b", "work-c"] {
        assert_eq!(store.get(&WorkId::from_str(id)).unwrap().job.state, JobState::Ready);
    }
}

#[test]
fn test_unblock_returns_blocked_job_to_ready() {
    let temp = tempfile::tempdir().unwrap();
    let store = JobStore::new(temp.path());
    let now = Timestamp::now();
    let work_id = WorkId::from_str("work-paused");
    store.admit(&stored_job(work_id.as_str(), now)).unwrap();
    let claim = store
        .claim(
            &work_id,
            &ClaimRequest {
                worker_id: "worker".to_owned(),
                lease_ttl: SignedDuration::from_secs(60),
                now,
            },
        )
        .unwrap();
    store
        .complete(
            &work_id,
            &CompleteRequest {
                token: claim.token,
                completed_at: now,
                outcome: JobOutcome::Blocked {
                    reason: "awaiting_approval".to_owned(),
                },
            },
        )
        .unwrap();
    assert_eq!(store.get(&work_id).unwrap().job.state, JobState::Blocked);
    let later = now.checked_add(SignedDuration::from_secs(3)).unwrap();

    let event = store
        .unblock(
            &work_id,
            later,
            ApprovalActor::Operator {
                id: "operator-a".to_owned(),
            },
            Some("looks fine".to_owned()),
        )
        .unwrap();

    assert_eq!(event.state, JobState::Ready);
    let persisted = store.get(&work_id).unwrap();
    assert_eq!(persisted.job.state, JobState::Ready);
    assert_eq!(persisted.job.updated_at, later);
    assert_eq!(persisted.completion, None);
    assert_eq!(persisted.lease, None);
    assert_eq!(persisted.revision, event.revision);

    let approval = store.get_approval(&work_id).unwrap().unwrap();
    assert_eq!(approval.approved_at, later);
    assert_eq!(approval.note.as_deref(), Some("looks fine"));
    assert_eq!(approval.revision, event.revision);
    assert!(matches!(
        approval.approved_by,
        ApprovalActor::Operator { ref id } if id == "operator-a"
    ));
}

#[test]
fn test_unblock_rejects_job_that_is_not_blocked() {
    let temp = tempfile::tempdir().unwrap();
    let store = JobStore::new(temp.path());
    let work_id = WorkId::from_str("work-ready");
    store.admit(&stored_job(work_id.as_str(), Timestamp::now())).unwrap();
    let before = store.get(&work_id).unwrap();

    let error = store
        .unblock(
            &work_id,
            Timestamp::now(),
            ApprovalActor::Operator {
                id: "operator-a".to_owned(),
            },
            None,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        SessionStoreError::JobNotBlocked { state: JobState::Ready, .. }
    ));
    assert_eq!(store.get(&work_id).unwrap(), before);
}

fn lease(child: &str, expires_in_seconds: i64) -> ChildLeaseRecord {
    let admitted_at = Timestamp::now();
    ChildLeaseRecord {
        child: SessionId::from_str(child),
        parent: SessionId::from_str("parent-1"),
        handoff_call_id: ToolCallId::from_str("call-1"),
        role: "worker".to_owned(),
        depth: 1,
        admitted_at,
        lease_expires_at: admitted_at
            .checked_add(SignedDuration::from_secs(expires_in_seconds))
            .unwrap(),
        trace: None,
    }
}

#[test]
fn test_child_lease_scan_quarantines_half_written_record() {
    let temp = tempfile::tempdir().unwrap();
    let store = ChildLeaseStore::new(temp.path());
    let good = lease("child-good", -1);
    store.admit(&good).unwrap();
    std::fs::write(store.root().join("child-torn.active.json"), b"").unwrap();

    assert_eq!(store.active().unwrap(), vec![good.clone()]);
    assert_eq!(store.claim_expired(Timestamp::now()).unwrap(), vec![good]);
    let quarantined = quarantine_entries(store.root());
    assert_eq!(quarantined.len(), 1);
    assert!(quarantined[0].starts_with("child-torn.active.json.corrupt-"));
}

#[test]
fn test_child_lease_admit_twice_is_already_exists_and_leaves_no_temp_file() {
    let temp = tempfile::tempdir().unwrap();
    let store = ChildLeaseStore::new(temp.path());
    let record = lease("child-once", 60);
    store.admit(&record).unwrap();

    assert!(matches!(
        store.admit(&record),
        Err(SessionStoreError::ChildLeaseAlreadyExists { .. })
    ));
    let names: Vec<String> = std::fs::read_dir(store.root())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["child-once.active.json".to_owned()]);
}

#[test]
fn test_freeze_scan_quarantines_corrupt_record() {
    let temp = tempfile::tempdir().unwrap();
    let store = FreezeStore::new(temp.path());
    let good = Freeze {
        cgroup: CgroupId::from_str("cgroup-1"),
        finding: FindingId::from_str("finding-1"),
        frozen_at: fixed_time(),
        expires_at: None,
    };
    store.freeze(&good).unwrap();
    std::fs::write(
        store
            .root()
            .join("cgroup-2.finding-2.p00000000000000000001.active.json"),
        b"{\"cgroup\":",
    )
    .unwrap();

    assert_eq!(store.active().unwrap(), vec![good.clone()]);
    assert!(store.reconcile_expired(Timestamp::now()).unwrap().is_empty());
    assert_eq!(quarantine_entries(store.root()).len(), 1);
    assert!(matches!(
        store.freeze(&good),
        Err(SessionStoreError::FreezeAlreadyExists { .. })
    ));
}
