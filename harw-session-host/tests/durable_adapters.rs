//! S04: durable `ApprovalBackend` and `TranscriptSource` over the session store.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use harw_protocol::items::{ContentPart, TurnItem, UserMessageItem};
use harw_protocol::{ApprovalKind, ApprovalRequest, Cursor};
use harw_session_host::approvals::{ApprovalBackend, ResolveOutcome};
use harw_session_host::error::HostError;
use harw_session_host::replay::TranscriptSource;
use harw_session_host::{DurableApprovals, DurableTranscripts};
use harw_session_store::{RecordKind, TranscriptRecord, TranscriptStore};
use harw_types::{
    ApprovalActor, ApprovalId, ItemId, ReviewDecision, RiskLevel, SessionId, ThreadRef, TurnId,
    WorkId,
};
use jiff::{SignedDuration, Timestamp};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn session() -> SessionId {
    SessionId::from_str("session-a")
}

fn operator(id: &str) -> ApprovalActor {
    ApprovalActor::Operator { id: id.to_owned() }
}

fn request(requested_at: Timestamp, ttl: SignedDuration) -> Result<ApprovalRequest, jiff::Error> {
    Ok(ApprovalRequest {
        id: ApprovalId::new(),
        work_id: WorkId::new(),
        kind: ApprovalKind::DynamicTool {
            turn_id: TurnId::new(),
            tool_name: "shell".to_owned(),
            arguments: serde_json::json!({ "cmd": "ls" }),
        },
        summary: "run ls".to_owned(),
        risk: RiskLevel::Medium,
        requested_at,
        timeout_at: requested_at.checked_add(ttl)?,
        decisions: vec![ReviewDecision::Approved, ReviewDecision::Rejected],
    })
}

/// Requests as JSON (`ApprovalRequest` has no `PartialEq`).
fn wire(requests: &[ApprovalRequest]) -> Result<Vec<serde_json::Value>, serde_json::Error> {
    requests.iter().map(serde_json::to_value).collect()
}

fn now() -> Timestamp {
    Timestamp::now()
}

fn issued(approvals: &DurableApprovals) -> Result<ApprovalRequest, Box<dyn std::error::Error>> {
    let req = request(now(), SignedDuration::from_mins(10))?;
    approvals.issue(&session(), &req, &operator("alice"), None)?;
    Ok(req)
}

fn only_file(dir: &Path, suffix: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(suffix))
        {
            return Ok(path);
        }
    }
    Err(format!("no *{suffix} in {}", dir.display()).into())
}

// ---- approvals ------------------------------------------------------------

#[test]
fn approvals_survive_reopen_and_resolve_once() -> TestResult {
    let temp = tempfile::tempdir()?;
    let first = DurableApprovals::open(temp.path())?;
    let req = issued(&first)?;
    assert_eq!(
        wire(&first.pending(&session())?)?,
        vec![serde_json::to_value(&req)?]
    );
    assert_eq!(first.session_of(&req.id)?, Some(session()));
    drop(first);

    let reopened = DurableApprovals::open(temp.path())?;
    assert_eq!(
        wire(&reopened.pending(&session())?)?,
        vec![serde_json::to_value(&req)?]
    );
    assert_eq!(reopened.session_of(&req.id)?, Some(session()));
    let outcome = reopened.resolve(
        &req.id,
        ReviewDecision::Approved,
        None,
        &operator("bob"),
        now(),
    )?;
    assert_eq!(outcome, ResolveOutcome::Resolved);
    assert!(reopened.pending(&session())?.is_empty());
    drop(reopened);

    // The resolution survives another reopen; later responders lose.
    let third = DurableApprovals::open(temp.path())?;
    assert!(third.pending(&session())?.is_empty());
    let again = third.resolve(
        &req.id,
        ReviewDecision::Rejected,
        None,
        &operator("carol"),
        now(),
    )?;
    assert_eq!(
        again,
        ResolveOutcome::AlreadyResolved {
            by: "operator:bob".to_owned()
        }
    );
    // Same actor, same decision: still no second transition.
    let same = third.resolve(
        &req.id,
        ReviewDecision::Approved,
        None,
        &operator("bob"),
        now(),
    )?;
    assert!(matches!(same, ResolveOutcome::AlreadyResolved { .. }));
    Ok(())
}

#[test]
fn unknown_expired_and_duplicate_issue() -> TestResult {
    let temp = tempfile::tempdir()?;
    let approvals = DurableApprovals::open(temp.path())?;
    assert!(approvals.pending(&session())?.is_empty());
    assert_eq!(approvals.session_of(&ApprovalId::new())?, None);
    assert_eq!(
        approvals.resolve(
            &ApprovalId::new(),
            ReviewDecision::Approved,
            None,
            &operator("a"),
            now()
        )?,
        ResolveOutcome::NotFound
    );

    let req = issued(&approvals)?;
    let later = now().checked_add(SignedDuration::from_hours(1))?;
    assert_eq!(
        approvals.resolve(
            &req.id,
            ReviewDecision::Approved,
            None,
            &operator("a"),
            later
        )?,
        ResolveOutcome::Expired
    );
    // Expiry did not consume it: a timely responder still wins.
    assert_eq!(
        approvals.resolve(
            &req.id,
            ReviewDecision::Approved,
            None,
            &operator("a"),
            now()
        )?,
        ResolveOutcome::Resolved
    );
    assert!(matches!(
        approvals.issue(&session(), &req, &operator("alice"), None),
        Err(HostError::Protocol(_))
    ));
    Ok(())
}

#[test]
fn concurrent_double_resolve_has_one_winner() -> TestResult {
    let temp = tempfile::tempdir()?;
    let approvals = Arc::new(DurableApprovals::open(temp.path())?);
    let req = issued(&approvals)?;

    let mut outcomes = Vec::new();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|n| {
                let approvals = Arc::clone(&approvals);
                let id = req.id.clone();
                scope.spawn(move || {
                    let decision = if n % 2 == 0 {
                        ReviewDecision::Approved
                    } else {
                        ReviewDecision::Rejected
                    };
                    approvals.resolve(&id, decision, None, &operator(&format!("op{n}")), now())
                })
            })
            .collect();
        for handle in handles {
            outcomes.push(handle.join());
        }
    });

    let mut winners = 0;
    let mut losers: Vec<String> = Vec::new();
    for outcome in outcomes {
        match outcome.map_err(|_| "resolver thread panicked")?? {
            ResolveOutcome::Resolved => winners += 1,
            ResolveOutcome::AlreadyResolved { by } => losers.push(by),
            other => return Err(format!("unexpected outcome {other:?}").into()),
        }
    }
    assert_eq!(winners, 1);
    assert_eq!(losers.len(), 7);
    // Every loser names the same single winner.
    assert!(losers.windows(2).all(|pair| pair[0] == pair[1]));
    Ok(())
}

#[test]
fn corrupt_approval_records_fail_closed() -> TestResult {
    // Torn payload: the request can neither be listed nor resolved.
    let temp = tempfile::tempdir()?;
    let approvals = DurableApprovals::open(temp.path())?;
    let req = issued(&approvals)?;
    let dir = temp.path().join("approvals").join("session-a");
    let payload = only_file(&dir, ".payload.json")?;
    std::fs::write(&payload, b"{\"id\": ")?;
    assert!(matches!(
        approvals.pending(&session()),
        Err(HostError::Storage(_))
    ));
    assert!(matches!(
        approvals.resolve(
            &req.id,
            ReviewDecision::Approved,
            None,
            &operator("a"),
            now()
        ),
        Err(HostError::Storage(_))
    ));

    // Torn pending record: same.
    let temp = tempfile::tempdir()?;
    let approvals = DurableApprovals::open(temp.path())?;
    let req = issued(&approvals)?;
    let dir = temp.path().join("approvals").join("session-a");
    std::fs::write(only_file(&dir, ".pending.json")?, b"not json")?;
    assert!(matches!(
        approvals.pending(&session()),
        Err(HostError::Storage(_))
    ));
    assert!(matches!(
        approvals.resolve(
            &req.id,
            ReviewDecision::Approved,
            None,
            &operator("a"),
            now()
        ),
        Err(HostError::Storage(_))
    ));

    // Torn resolution record: never "Resolved" again, never "pending" again.
    let temp = tempfile::tempdir()?;
    let approvals = DurableApprovals::open(temp.path())?;
    let req = issued(&approvals)?;
    assert_eq!(
        approvals.resolve(
            &req.id,
            ReviewDecision::Rejected,
            None,
            &operator("a"),
            now()
        )?,
        ResolveOutcome::Resolved
    );
    let dir = temp.path().join("approvals").join("session-a");
    std::fs::write(only_file(&dir, ".resolved.json")?, b"{\"request\":")?;
    assert!(matches!(
        approvals.pending(&session()),
        Err(HostError::Storage(_))
    ));
    assert!(matches!(
        approvals.resolve(
            &req.id,
            ReviewDecision::Approved,
            None,
            &operator("b"),
            now()
        ),
        Err(HostError::Storage(_))
    ));
    Ok(())
}

// ---- transcripts ----------------------------------------------------------

fn item_record(sequence: u64) -> Result<TranscriptRecord, serde_json::Error> {
    let item = TurnItem::UserMessage(UserMessageItem {
        id: ItemId::from_str(format!("item-{sequence}")),
        content: vec![ContentPart::Text {
            text: format!("m{sequence}"),
        }],
    });
    Ok(TranscriptRecord::new(
        session(),
        ThreadRef::from_str("root"),
        sequence,
        Timestamp::UNIX_EPOCH,
        RecordKind::Item,
        serde_json::to_value(item)?,
    ))
}

fn seeded(count: u64) -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let store = TranscriptStore::new(temp.path());
    for sequence in 0..count {
        store.append(&item_record(sequence)?)?;
    }
    Ok(temp)
}

#[test]
fn transcripts_read_and_survive_reopen() -> TestResult {
    let temp = seeded(4)?;
    let source = DurableTranscripts::open(temp.path())?;
    assert_eq!(source.head(&session())?, 4);
    let sequences: Vec<u64> = source
        .read_from(&session(), 1, 10)?
        .iter()
        .map(|record| record.sequence)
        .collect();
    assert_eq!(sequences, vec![1, 2, 3]);
    assert_eq!(source.head(&SessionId::from_str("missing"))?, 0);
    drop(source);

    // Appends by the writer are visible to a freshly opened source.
    TranscriptStore::new(temp.path()).append(&item_record(4)?)?;
    let reopened = DurableTranscripts::open(temp.path())?;
    assert_eq!(reopened.head(&session())?, 5);
    assert_eq!(
        reopened.head_cursor(&session(), 2)?,
        Cursor {
            generation: 2,
            durable: 5,
            live: 0
        }
    );
    Ok(())
}

#[test]
fn replay_from_cursor_returns_exact_suffix() -> TestResult {
    let temp = seeded(5)?;
    let source = DurableTranscripts::open(temp.path())?;
    let from = Cursor {
        generation: 7,
        durable: 3,
        live: 0,
    };
    let frames = source.replay_from(&session(), 7, &from, 100)?;
    let durables: Vec<u64> = frames.iter().map(|frame| frame.cursor.durable).collect();
    assert_eq!(durables, vec![4, 5]);
    assert!(frames.iter().all(|frame| frame.cursor.generation == 7));
    // The head cursor yields nothing; `live` is not a transcript position.
    let head = Cursor {
        generation: 7,
        durable: 5,
        live: 9,
    };
    assert!(source.replay_from(&session(), 7, &head, 100)?.is_empty());
    Ok(())
}

#[test]
fn bad_cursor_is_rejected() -> TestResult {
    let temp = seeded(3)?;
    let source = DurableTranscripts::open(temp.path())?;
    let beyond = Cursor {
        generation: 1,
        durable: 4,
        live: 0,
    };
    assert!(matches!(
        source.replay_from(&session(), 1, &beyond, 10),
        Err(HostError::Protocol(_))
    ));
    let stale = Cursor {
        generation: 1,
        durable: 1,
        live: 0,
    };
    assert!(matches!(
        source.replay_from(&session(), 2, &stale, 10),
        Err(HostError::Protocol(_))
    ));
    assert!(matches!(
        source.check_cursor(&SessionId::from_str("missing"), 1, &stale),
        Err(HostError::Protocol(_))
    ));
    // A valid cursor passes.
    let ok = Cursor {
        generation: 1,
        durable: 3,
        live: 0,
    };
    source.check_cursor(&session(), 1, &ok)?;
    Ok(())
}

#[test]
fn torn_transcript_fails_closed_until_repaired() -> TestResult {
    let temp = seeded(2)?;
    let path = temp.path().join("session-a.jsonl");
    // Crash mid-append: a partial line without newline.
    let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
    file.write_all(b"{\"session_id\":\"session-a\",\"seq")?;
    drop(file);

    let source = DurableTranscripts::open(temp.path())?;
    assert!(matches!(
        source.head(&session()),
        Err(HostError::Storage(_))
    ));
    assert!(matches!(
        source.read_from(&session(), 0, 10),
        Err(HostError::Storage(_))
    ));
    let cursor = Cursor::start(1);
    assert!(source.replay_from(&session(), 1, &cursor, 10).is_err());

    source.repair_tail(&session())?;
    assert_eq!(source.head(&session())?, 2);
    assert_eq!(source.replay_from(&session(), 1, &cursor, 10)?.len(), 2);

    // A fully corrupt middle record is not repairable and stays closed.
    let mut lines: HashMap<usize, String> = HashMap::new();
    for (n, line) in std::fs::read_to_string(&path)?.lines().enumerate() {
        lines.insert(n, line.to_owned());
    }
    std::fs::write(
        &path,
        format!("garbage\n{}\n", lines.get(&1).cloned().unwrap_or_default()),
    )?;
    assert!(matches!(
        source.head(&session()),
        Err(HostError::Storage(_))
    ));
    Ok(())
}
