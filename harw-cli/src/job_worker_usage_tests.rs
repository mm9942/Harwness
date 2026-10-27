//! Tests der Usage-Buchhaltung bei fehlgeschlagenem Festschreiben (Review P1 zu
//! #151): der Ledger schließt, der Verbrauch bleibt als durabler Marker
//! erhalten, und der nächste Claim verrechnet ihn vor jeder Modell-Runde.
//!
//! Den Speicherfehler erzeugt [`break_store`]: das Sperrverzeichnis des
//! Job-Stores wird durch eine Datei ersetzt, jede Mutation scheitert mit einem
//! I/O-Fehler (kein Lease-Fehler). Anders als entzogene Dateirechte wirkt das
//! auch, wenn die Tests als root laufen.

use super::prompt_claim_guard_tests::{
    FixedUsageProvider, admit, claim_as_worker, empty_request, failure_reason, operator_scope,
    record, run_once, temp_dir, tokens_used,
};
use super::usage_marker::{USAGE_UNRESOLVED_DIR, UnresolvedUsage, UsageMarkerStore};
use super::*;
use crate::test_support::{TestError, TestResult, ctx, some_or};
use harw_job_runtime::{BudgetUsage, StoredJob};
use harw_types::ApprovalActor;
use std::sync::atomic::{AtomicUsize, Ordering};

// ── Fixtures ──────────────────────────────────────────────────────────────

const PARKED_LOCKS: &str = "locks-parked";

// Ersetzt das Sperrverzeichnis des Job-Stores durch eine Datei.
fn break_store(store: &JobStore) -> TestResult {
    let locks = store.root().join(harw_job_store::LOCKS_DIR);
    std::fs::rename(&locks, store.root().join(PARKED_LOCKS)).map_err(ctx("park locks dir"))?;
    std::fs::write(&locks, b"not a directory").map_err(ctx("block locks dir"))?;
    Ok(())
}

// Macht [`break_store`] rückgängig.
fn repair_store(store: &JobStore) -> TestResult {
    let locks = store.root().join(harw_job_store::LOCKS_DIR);
    std::fs::remove_file(&locks).map_err(ctx("remove locks blocker"))?;
    std::fs::rename(store.root().join(PARKED_LOCKS), &locks).map_err(ctx("restore locks dir"))?;
    Ok(())
}

// Lässt einen Prompt-Job zu und claimt ihn als Worker.
fn admitted_claim(store: &JobStore, id: &str, budget: Budget) -> TestResult<JobClaim> {
    admit(
        store,
        &record(
            id,
            serde_json::json!({"prompt": "hello"}),
            budget,
            operator_scope(),
        )?,
    )?;
    claim_as_worker(store, id)
}

fn persisted_ledger(
    store: &Arc<JobStore>,
    claim: &JobClaim,
    budget: Budget,
) -> Arc<PromptTokenLedger> {
    PromptTokenLedger::persisted(
        budget,
        claim.job.usage.clone(),
        Arc::clone(store),
        claim.token.clone(),
    )
}

fn stored_tokens(store: &JobStore, id: &str) -> TestResult<u64> {
    let stored = store.get(&WorkId::from_str(id)).map_err(ctx("get job"))?;
    Ok(stored.job.usage.tokens)
}

fn read_marker(store: &JobStore, id: &str) -> TestResult<Option<UnresolvedUsage>> {
    UsageMarkerStore::in_job_root(store.root())
        .read(id)
        .map_err(TestError::Unexpected)
}

fn owed(tokens: u64) -> BudgetUsage {
    BudgetUsage {
        tokens,
        wall: SignedDuration::from_secs(7),
        tool_calls: 2,
    }
}

// ── Ledger: Fehler beim Festschreiben ─────────────────────────────────────

#[tokio::test]
async fn failed_usage_write_closes_the_ledger_and_writes_a_marker() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let claim = admitted_claim(&store, "usage-fail", Budget::unbounded())?;
    let ledger = persisted_ledger(&store, &claim, Budget::unbounded());

    ledger
        .charge_round(&tokens_used(40))
        .map_err(TestError::Unexpected)?;
    break_store(&store)?;
    let flushed = ledger.flush().await;

    let reason = match flushed {
        Err(UsageFlushError::NotRecorded(reason)) => reason,
        other => {
            return Err(TestError::Unexpected(format!(
                "expected NotRecorded, got {other:?}"
            )));
        }
    };
    assert!(reason.contains("record_usage failed"), "{reason}");
    let closed = some_or(ledger.exceeded(), "the failed write must close the ledger")?;
    assert!(closed.starts_with("usage:"), "{closed}");
    assert!(ledger.admit_round().is_err(), "no further model round");

    let marker = some_or(read_marker(&store, "usage-fail")?, "marker written")?;
    assert_eq!(marker.usage.tokens, 40);
    assert_eq!(marker.epoch, claim.token.epoch);
    assert_eq!(marker.work_id, "usage-fail");

    repair_store(&store)?;
    assert_eq!(
        stored_tokens(&store, "usage-fail")?,
        0,
        "the job record never saw the usage, only the marker did"
    );
    Ok(())
}

#[tokio::test]
async fn respond_fails_closed_when_usage_cannot_be_recorded() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let claim = admitted_claim(&store, "respond-closed", Budget::unbounded())?;
    let ledger = persisted_ledger(&store, &claim, Budget::unbounded());
    let inner = Arc::new(FixedUsageProvider {
        calls: AtomicUsize::new(0),
        tokens_per_call: 5,
    });
    let budgeted = BudgetedModelProvider {
        inner: Arc::clone(&inner) as Arc<dyn ModelProvider>,
        ledger: Arc::clone(&ledger),
    };

    assert!(
        budgeted.respond(empty_request()).await.is_ok(),
        "with an intact store the response is handed back"
    );
    break_store(&store)?;
    assert!(
        budgeted.respond(empty_request()).await.is_err(),
        "the response of a round whose usage is unrecorded must not be handed back"
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    assert!(
        budgeted.respond(empty_request()).await.is_err(),
        "a closed ledger fails the next round"
    );
    assert_eq!(
        inner.calls.load(Ordering::SeqCst),
        2,
        "a closed ledger must never reach the inner provider"
    );

    let marker = some_or(read_marker(&store, "respond-closed")?, "marker written")?;
    assert_eq!(marker.usage.tokens, 10, "both rounds are accounted");
    Ok(())
}

#[tokio::test]
async fn final_flush_failure_overrides_a_successful_outcome() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let claim = admitted_claim(&store, "final-flush", Budget::unbounded())?;
    let ledger = persisted_ledger(&store, &claim, Budget::unbounded());
    ledger
        .charge_round(&tokens_used(5))
        .map_err(TestError::Unexpected)?;
    break_store(&store)?;

    let outcome = ledger
        .finish(JobOutcome::Succeeded {
            result: serde_json::json!({"assistant": "done"}),
        })
        .await;
    match outcome {
        JobOutcome::Failed { reason } => {
            assert!(
                reason.starts_with("usage accounting unresolved"),
                "{reason}"
            );
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "unaccounted work must not report success, got {other:?}"
            )));
        }
    }

    // Ein Abbruch durch den Supervisor behauptet keinen Erfolg und bleibt.
    let cancelled = ledger
        .finish(JobOutcome::Cancelled {
            reason: "stopped".to_owned(),
        })
        .await;
    assert!(matches!(cancelled, JobOutcome::Cancelled { .. }));
    Ok(())
}

#[tokio::test]
async fn contended_job_lock_is_retried_instead_of_failing_the_job() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let claim = admitted_claim(&store, "contended", Budget::unbounded())?;
    let ledger = persisted_ledger(&store, &claim, Budget::unbounded());
    ledger
        .charge_round(&tokens_used(4))
        .map_err(TestError::Unexpected)?;

    // Ein gleichzeitiger Heartbeat hält die Job-Sperre kurz.
    let records = harw_job_store::RecordStore::<StoredJob>::create_ambient(store.root())
        .map_err(ctx("open record store"))?;
    let lock = records.lock("contended").map_err(ctx("take job lock"))?;
    let release = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        drop(lock);
    });

    ledger
        .flush()
        .await
        .map_err(ctx("flush under a briefly held lock"))?;
    release.await.map_err(ctx("release task"))?;

    assert_eq!(stored_tokens(&store, "contended")?, 4);
    assert!(
        ledger.exceeded().is_none(),
        "a brief contention is no failure"
    );
    assert!(read_marker(&store, "contended")?.is_none());
    Ok(())
}

// ── Claim: Verrechnung vor jeder Runde ────────────────────────────────────

#[test]
fn settle_without_a_marker_returns_the_claim_usage() -> TestResult {
    let temp = temp_dir()?;
    let store = JobStore::new(temp.path());
    let claim = admitted_claim(&store, "no-marker", Budget::unbounded())?;

    let settled = settle_unresolved_usage(&store, &claim).map_err(TestError::Unexpected)?;
    assert_eq!(settled, claim.job.usage);
    Ok(())
}

#[test]
fn reclaim_after_failed_write_settles_the_marker_before_any_round() -> TestResult {
    let temp = temp_dir()?;
    let store = JobStore::new(temp.path());
    let claim = admitted_claim(&store, "settle-ok", Budget::unbounded())?;
    let markers = UsageMarkerStore::in_job_root(store.root());
    let unsettled = owed(120);
    markers
        .write(&UnresolvedUsage::new(
            "settle-ok",
            claim.token.epoch,
            unsettled.clone(),
            "disk full",
        ))
        .map_err(TestError::Unexpected)?;

    let settled = settle_unresolved_usage(&store, &claim).map_err(TestError::Unexpected)?;

    assert!(settled.tokens >= unsettled.tokens, "{settled:?}");
    assert!(settled.wall >= unsettled.wall, "{settled:?}");
    assert!(settled.tool_calls >= unsettled.tool_calls, "{settled:?}");
    assert!(
        read_marker(&store, "settle-ok")?.is_none(),
        "a settled marker is removed"
    );
    let stored = store
        .get(&WorkId::from_str("settle-ok"))
        .map_err(ctx("get job"))?;
    assert_eq!(stored.job.usage, settled, "the settled usage is durable");
    Ok(())
}

#[tokio::test]
async fn settle_claim_usage_replaces_the_usage_of_the_claim() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let mut claim = admitted_claim(&store, "settle-async", Budget::unbounded())?;
    UsageMarkerStore::in_job_root(store.root())
        .write(&UnresolvedUsage::new(
            "settle-async",
            claim.token.epoch,
            owed(33),
            "disk full",
        ))
        .map_err(TestError::Unexpected)?;

    settle_claim_usage(&store, &mut claim)
        .await
        .map_err(TestError::Unexpected)?;

    assert_eq!(claim.job.usage.tokens, 33);
    assert!(read_marker(&store, "settle-async")?.is_none());
    Ok(())
}

#[test]
fn settlement_failure_keeps_the_marker() -> TestResult {
    let temp = temp_dir()?;
    let store = JobStore::new(temp.path());
    let claim = admitted_claim(&store, "settle-fail", Budget::unbounded())?;
    let markers = UsageMarkerStore::in_job_root(store.root());
    markers
        .write(&UnresolvedUsage::new(
            "settle-fail",
            claim.token.epoch,
            owed(80),
            "disk full",
        ))
        .map_err(TestError::Unexpected)?;
    break_store(&store)?;

    let reason = match settle_unresolved_usage(&store, &claim) {
        Err(reason) => reason,
        Ok(usage) => {
            return Err(TestError::Unexpected(format!(
                "a marker that cannot be recorded must not settle, got {usage:?}"
            )));
        }
    };
    assert!(
        reason.starts_with("usage accounting unresolved"),
        "{reason}"
    );
    assert!(
        read_marker(&store, "settle-fail")?.is_some(),
        "an unsettled marker must stay"
    );

    repair_store(&store)?;
    let settled = settle_unresolved_usage(&store, &claim).map_err(TestError::Unexpected)?;
    assert_eq!(settled.tokens, 80);
    assert!(read_marker(&store, "settle-fail")?.is_none());
    Ok(())
}

#[tokio::test]
async fn unsettleable_marker_fails_the_job_without_a_model_call() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    admit(
        &store,
        &record(
            "unsettleable",
            serde_json::json!({"prompt": "hello"}),
            Budget::unbounded(),
            operator_scope(),
        )?,
    )?;
    // Eine Marker-Datei, die sich nicht lesen lässt: fail closed.
    let dir = store.root().join(USAGE_UNRESOLVED_DIR);
    std::fs::create_dir_all(&dir).map_err(ctx("create marker dir"))?;
    std::fs::write(dir.join("unsettleable.json"), b"{ not json").map_err(ctx("write marker"))?;
    let inner = Arc::new(FixedUsageProvider {
        calls: AtomicUsize::new(0),
        tokens_per_call: 1,
    });

    run_once(
        &store,
        temp.path(),
        Arc::clone(&inner) as Arc<dyn ModelProvider>,
    )
    .await?;

    let reason = failure_reason(&store, "unsettleable")?;
    assert!(
        reason.starts_with("usage accounting unresolved"),
        "{reason}"
    );
    assert_eq!(
        inner.calls.load(Ordering::SeqCst),
        0,
        "no model call while the accounting is unresolved"
    );
    Ok(())
}

// ── Regression: Schreibfehler, Absturz, Reclaim ───────────────────────────

#[tokio::test]
async fn failed_write_then_crash_then_reclaim_does_not_get_a_fresh_budget() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let budget = Budget {
        max_tokens: Some(100),
        max_wall: None,
        max_tool_calls: None,
    };
    // Zwei Versuche, wie `INTENT_CYCLE_MAX_ATTEMPTS`; kurze Wartezeit bis zum
    // Reclaim.
    let mut stored = record(
        "crash-reclaim",
        serde_json::json!({"prompt": "hello"}),
        budget.clone(),
        operator_scope(),
    )?;
    stored.job.retry = RetryPolicy {
        max_attempts: 2,
        base_delay: SignedDuration::from_millis(1),
        factor: 1.0,
        max_delay: SignedDuration::from_millis(1),
        jitter: 0.0,
    };
    admit(&store, &stored)?;

    // Versuch 1: Runde 1 wird festgeschrieben, bei Runde 2 scheitert das Schreiben.
    let first = claim_as_worker(&store, "crash-reclaim")?;
    let ledger = persisted_ledger(&store, &first, budget.clone());
    ledger
        .charge_round(&tokens_used(60))
        .map_err(TestError::Unexpected)?;
    ledger.flush().await.map_err(ctx("flush of round 1"))?;
    ledger
        .charge_round(&tokens_used(30))
        .map_err(TestError::Unexpected)?;
    break_store(&store)?;
    let flushed = ledger.flush().await;
    assert!(
        matches!(flushed, Err(UsageFlushError::NotRecorded(_))),
        "{flushed:?}"
    );
    assert!(ledger.admit_round().is_err(), "no round after the failure");

    // Der Worker stirbt; Speicher wieder heil; die Lease wird reclaimt.
    repair_store(&store)?;
    store
        .reclaim(
            &WorkId::from_str("crash-reclaim"),
            Timestamp::now(),
            ApprovalActor::Operator {
                id: "operator".to_owned(),
            },
        )
        .map_err(ctx("reclaim"))?;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    // Versuch 2: ohne Marker sähe der Claim nur die ältere Usage (60 Tokens) ...
    let second = claim_as_worker(&store, "crash-reclaim")?;
    assert_eq!(
        second.job.usage.tokens, 60,
        "the job record only knows round 1"
    );
    // ... mit Marker geht er von 90 Tokens aus und hat nur noch 10 übrig.
    let settled = settle_unresolved_usage(&store, &second).map_err(TestError::Unexpected)?;
    assert_eq!(settled.tokens, 90);
    assert_eq!(stored_tokens(&store, "crash-reclaim")?, 90);
    assert!(read_marker(&store, "crash-reclaim")?.is_none());

    let reclaimed =
        PromptTokenLedger::persisted(budget, settled, Arc::clone(&store), second.token.clone());
    assert!(reclaimed.admit_round().is_ok(), "10 tokens are left");
    assert!(
        reclaimed.charge_round(&tokens_used(11)).is_err(),
        "the reclaim must not get a fresh budget of 100 tokens"
    );
    Ok(())
}
