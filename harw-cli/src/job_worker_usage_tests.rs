//! Tests der Usage-Buchhaltung bei fehlgeschlagenem Festschreiben (Review P1 zu
//! #151): der Ledger schließt, der Verbrauch bleibt als durabler Marker
//! erhalten, und der nächste Claim verrechnet ihn vor jeder Modell-Runde.
//!
//! Den Speicherfehler erzeugt [`break_store`]: das Sperrverzeichnis des
//! Job-Stores wird durch eine Datei ersetzt, jede Mutation scheitert mit einem
//! I/O-Fehler (kein Lease-Fehler). Anders als entzogene Dateirechte wirkt das
//! auch, wenn die Tests als root laufen. Ebenso macht [`block_dir`] ein
//! Marker-Verzeichnis unbeschreibbar (Review B2 zu #151: Rundenreservierung).
//!
//! Die Reservierung einer Runde, deren Provider schon aufgerufen war, bleibt
//! bei Fehler und Abbruch liegen (Review P1 4394111159, 4394114784); jede
//! Reservierung entsteht und fällt unter der Job-Lease (Review P1 4394111166,
//! P2 4394114795) — die Wettläufe mit einem Reclaim werden über Kanäle und
//! die echte Job-Sperre gesteuert, ohne lange Pausen.

use super::prompt_claim_guard_tests::{
    FixedUsageProvider, admit, claim_as_worker, empty_request, failure_reason, operator_scope,
    record, run_once, temp_dir, tokens_used,
};
use super::usage_marker::{
    InFlightUsage, USAGE_INFLIGHT_DIR, USAGE_UNRESOLVED_DIR, UnresolvedUsage, UsageMarkerStore,
};
use super::*;
use crate::test_support::{TestError, TestResult, ctx, some_or};
use harw_core::ToolCallMeter;
use harw_job_runtime::{BudgetUsage, StoredJob};
use harw_types::ApprovalActor;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

// ── Fixtures ──────────────────────────────────────────────────────────────

const PARKED_LOCKS: &str = "locks-parked";

// Ersetzt das Sperrverzeichnis des Job-Stores durch eine Datei.
pub(super) fn break_store(store: &JobStore) -> TestResult {
    let locks = store.root().join(harw_job_store::LOCKS_DIR);
    std::fs::rename(&locks, store.root().join(PARKED_LOCKS)).map_err(ctx("park locks dir"))?;
    std::fs::write(&locks, b"not a directory").map_err(ctx("block locks dir"))?;
    Ok(())
}

// Macht [`break_store`] rückgängig.
pub(super) fn repair_store(store: &JobStore) -> TestResult {
    let locks = store.root().join(harw_job_store::LOCKS_DIR);
    std::fs::remove_file(&locks).map_err(ctx("remove locks blocker"))?;
    std::fs::rename(store.root().join(PARKED_LOCKS), &locks).map_err(ctx("restore locks dir"))?;
    Ok(())
}

// Ersetzt das (noch nicht vorhandene) Verzeichnis `dir` unterhalb der
// Job-Store-Wurzel durch eine Datei: jeder Zugriff darauf scheitert mit einem
// I/O-Fehler.
fn block_dir(store: &JobStore, dir: &str) -> TestResult {
    let path = store.root().join(dir);
    if path.exists() {
        return Err(TestError::Unexpected(format!(
            "'{dir}' must not exist before it is blocked"
        )));
    }
    std::fs::write(&path, b"not a directory").map_err(ctx("block marker dir"))?;
    Ok(())
}

// Macht [`block_dir`] rückgängig.
fn unblock_dir(store: &JobStore, dir: &str) -> TestResult {
    std::fs::remove_file(store.root().join(dir)).map_err(ctx("unblock marker dir"))?;
    Ok(())
}

// Lässt einen Prompt-Job zu und claimt ihn als Worker.
pub(super) fn admitted_claim(store: &JobStore, id: &str, budget: Budget) -> TestResult<JobClaim> {
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

// Wie [`admitted_claim`], aber mit zwei Versuchen und kurzer Wartezeit bis
// zum Reclaim (wie `INTENT_CYCLE_MAX_ATTEMPTS`).
pub(super) fn admitted_retryable_claim(
    store: &JobStore,
    id: &str,
    budget: Budget,
) -> TestResult<JobClaim> {
    let mut stored = record(
        id,
        serde_json::json!({"prompt": "hello"}),
        budget,
        operator_scope(),
    )?;
    stored.job.retry = RetryPolicy {
        max_attempts: 2,
        base_delay: SignedDuration::from_millis(1),
        factor: 1.0,
        max_delay: SignedDuration::from_millis(1),
        jitter: 0.0,
    };
    admit(store, &stored)?;
    claim_as_worker(store, id)
}

// Der Lease-Halter ist tot: die Lease wird reclaimt und der Job erneut
// geclaimt (höhere Epoche).
pub(super) async fn reclaim_and_claim_again(store: &JobStore, id: &str) -> TestResult<JobClaim> {
    store
        .reclaim(
            &WorkId::from_str(id),
            Timestamp::now(),
            ApprovalActor::Operator {
                id: "operator".to_owned(),
            },
        )
        .map_err(ctx("reclaim"))?;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    claim_as_worker(store, id)
}

pub(super) fn persisted_ledger(
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

pub(super) fn read_marker(store: &JobStore, id: &str) -> TestResult<Option<UnresolvedUsage>> {
    UsageMarkerStore::in_job_root(store.root())
        .read(id)
        .map_err(TestError::Unexpected)
}

fn read_in_flight(store: &JobStore, id: &str) -> TestResult<Option<InFlightUsage>> {
    UsageMarkerStore::in_job_root(store.root())
        .read_in_flight(id)
        .map_err(TestError::Unexpected)
}

// Erwartet, dass die Verrechnung eines Claims scheitert, und liefert den Grund.
fn expect_unsettled(settled: Result<BudgetUsage, String>) -> TestResult<String> {
    match settled {
        Err(reason) => Ok(reason),
        Ok(usage) => Err(TestError::Unexpected(format!(
            "an unproven reservation must not settle, got {usage:?}"
        ))),
    }
}

/// Antwortet mit fester Usage; beim Aufruf Nummer `fail_storage_on` (ab 1)
/// fällt vorher der Job-Datensatz aus und mit `block_markers` auch das
/// Marker-Verzeichnis — der Speicher bricht also weg, während die Runde läuft
/// (nach ihrer Reservierung).
struct StorageFailingProvider {
    store: Arc<JobStore>,
    calls: AtomicUsize,
    tokens_per_call: u64,
    fail_storage_on: usize,
    block_markers: bool,
}

impl ModelProvider for StorageFailingProvider {
    fn respond<'a>(&'a self, _request: harw_core::ModelRequest) -> harw_core::ModelFuture<'a> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        let failed = if call == self.fail_storage_on {
            break_store(&self.store).and_then(|()| {
                if self.block_markers {
                    block_dir(&self.store, USAGE_UNRESOLVED_DIR)
                } else {
                    Ok(())
                }
            })
        } else {
            Ok(())
        };
        let mut response = harw_core::ModelResponse::text("fixed");
        response.usage = tokens_used(self.tokens_per_call);
        Box::pin(async move {
            failed.map_err(|error| {
                harw_core::ModelError::RequestFailed(format!("test fixture: {error}"))
            })?;
            Ok::<harw_core::ModelResponse, harw_core::ModelError>(response)
        })
    }
}

/// Antwortet erst, wenn der Test die Runde freigibt: Aufruf `n` wartet auf
/// den `n`-ten Empfänger und meldet dessen Token-Zahl.
struct GatedProvider {
    calls: AtomicUsize,
    gates: Mutex<VecDeque<(tokio::sync::oneshot::Receiver<()>, u64)>>,
}

impl ModelProvider for GatedProvider {
    fn respond<'a>(&'a self, _request: harw_core::ModelRequest) -> harw_core::ModelFuture<'a> {
        let gate = self
            .gates
            .lock()
            .ok()
            .and_then(|mut gates| gates.pop_front());
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let Some((gate, tokens)) = gate else {
                return Err(harw_core::ModelError::RequestFailed(
                    "test fixture: no gate left".to_owned(),
                ));
            };
            gate.await.map_err(|_| {
                harw_core::ModelError::RequestFailed("test fixture: gate dropped".to_owned())
            })?;
            let mut response = harw_core::ModelResponse::text("gated");
            response.usage = tokens_used(tokens);
            Ok::<harw_core::ModelResponse, harw_core::ModelError>(response)
        })
    }
}

type RoundHandle = tokio::task::JoinHandle<Result<harw_core::ModelResponse, harw_core::ModelError>>;

// Startet eine Modell-Runde über `budgeted` in einem eigenen Task.
fn spawn_round(budgeted: &Arc<BudgetedModelProvider>) -> RoundHandle {
    let budgeted = Arc::clone(budgeted);
    tokio::spawn(async move { budgeted.respond(empty_request()).await })
}

// Wartet, bis `calls` mindestens `count` erreicht (höchstens fünf Sekunden).
async fn wait_for_calls(calls: &AtomicUsize, count: usize) -> TestResult {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while calls.load(Ordering::SeqCst) < count {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    })
    .await
    .map_err(ctx("wait for the model call"))
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
    // Der Job-Datensatz fällt während Runde 2 aus, nach ihrer Reservierung
    // (mit kaputtem Store ließe sich die Runde gar nicht erst reservieren).
    let inner = Arc::new(StorageFailingProvider {
        store: Arc::clone(&store),
        calls: AtomicUsize::new(0),
        tokens_per_call: 5,
        fail_storage_on: 2,
        block_markers: false,
    });
    let budgeted = BudgetedModelProvider {
        inner: Arc::clone(&inner) as Arc<dyn ModelProvider>,
        ledger: Arc::clone(&ledger),
    };

    assert!(
        budgeted.respond(empty_request()).await.is_ok(),
        "with an intact store the response is handed back"
    );
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

// ── Rundenreservierung (Review B2 zu #151) ────────────────────────────────

#[tokio::test]
async fn double_write_failure_then_crash_then_reclaim_cannot_spend_again() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let budget = Budget {
        max_tokens: Some(100),
        max_wall: None,
        max_tool_calls: None,
    };
    let first = admitted_retryable_claim(&store, "double-fail", budget.clone())?;
    let ledger = persisted_ledger(&store, &first, budget);
    let inner = Arc::new(StorageFailingProvider {
        store: Arc::clone(&store),
        calls: AtomicUsize::new(0),
        tokens_per_call: 30,
        fail_storage_on: 2,
        block_markers: true,
    });
    let budgeted = BudgetedModelProvider {
        inner: Arc::clone(&inner) as Arc<dyn ModelProvider>,
        ledger: Arc::clone(&ledger),
    };

    // Runde 1: verbucht, ihre Reservierung ist wieder entfernt.
    budgeted
        .respond(empty_request())
        .await
        .map_err(ctx("round 1"))?;
    assert_eq!(stored_tokens(&store, "double-fail")?, 30);
    assert!(
        read_in_flight(&store, "double-fail")?.is_none(),
        "a recorded round leaves no reservation"
    );

    // Runde 2: während des Modellaufrufs fallen Job-Datensatz UND
    // Marker-Verzeichnis aus; weder `record_usage` noch der Marker gelingen.
    let failed = budgeted.respond(empty_request()).await;
    assert!(
        failed.is_err(),
        "a round whose usage is neither recorded nor marked must fail closed"
    );
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    assert!(
        store.root().join(USAGE_UNRESOLVED_DIR).is_file(),
        "the fixture must have blocked the marker directory"
    );
    assert!(ledger.admit_round().is_err(), "no round after the failure");
    let reservation = some_or(
        read_in_flight(&store, "double-fail")?,
        "the reservation of round 2 survives the double failure",
    )?;
    assert_eq!(reservation.usage_before.tokens, 30);
    assert_eq!(reservation.epoch, first.token.epoch);

    // Der Worker stirbt; danach ist der Speicher wieder heil.
    drop(budgeted);
    drop(ledger);
    repair_store(&store)?;
    unblock_dir(&store, USAGE_UNRESOLVED_DIR)?;
    assert!(
        read_marker(&store, "double-fail")?.is_none(),
        "no unresolved-usage marker could be written"
    );

    // Reclaim: der Job-Datensatz kennt nur Runde 1 (30 von 100 Tokens) ...
    let second = reclaim_and_claim_again(&store, "double-fail").await?;
    assert!(second.token.epoch > first.token.epoch);
    assert_eq!(second.job.usage.tokens, 30);
    // ... die Reservierung beweist, dass Runde 2 nie verbucht wurde: kein
    // frisches Budget für die schon ausgegebenen Tokens.
    let reason = expect_unsettled(settle_unresolved_usage(&store, &second))?;
    assert!(
        reason.starts_with("usage accounting unresolved"),
        "{reason}"
    );
    assert!(reason.contains("in-flight model round"), "{reason}");
    assert!(
        read_in_flight(&store, "double-fail")?.is_some(),
        "an unproven reservation stays"
    );
    assert_eq!(stored_tokens(&store, "double-fail")?, 30);

    // Der asynchrone Pfad der Worker übernimmt ebenfalls keine Usage.
    let mut reclaimed = second.clone();
    let settled = settle_claim_usage(&store, &mut reclaimed).await;
    assert!(settled.is_err(), "{settled:?}");
    assert_eq!(reclaimed.job.usage, second.job.usage);
    Ok(())
}

#[tokio::test]
async fn in_flight_marker_is_stale_once_usage_advanced() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let first = admitted_retryable_claim(&store, "stale-reservation", Budget::unbounded())?;
    // Versuch 1 reservierte bei 20 Tokens und verbuchte danach 50; nur das
    // Entfernen der Reservierung ging verloren.
    UsageMarkerStore::in_job_root(store.root())
        .write_in_flight(&InFlightUsage::new(
            "stale-reservation",
            first.token.epoch,
            owed(20),
        ))
        .map_err(TestError::Unexpected)?;
    store
        .record_usage(&first.token.work_id, &first.token, &owed(50))
        .map_err(ctx("record usage"))?;

    let second = reclaim_and_claim_again(&store, "stale-reservation").await?;
    let settled = settle_unresolved_usage(&store, &second).map_err(TestError::Unexpected)?;

    assert_eq!(settled.tokens, 50);
    assert!(
        read_in_flight(&store, "stale-reservation")?.is_none(),
        "a reservation below the stored usage is proven recorded and removed"
    );
    Ok(())
}

#[test]
fn in_flight_marker_of_the_current_attempt_is_left_alone() -> TestResult {
    let temp = temp_dir()?;
    let store = JobStore::new(temp.path());
    let claim = admitted_claim(&store, "own-reservation", Budget::unbounded())?;
    UsageMarkerStore::in_job_root(store.root())
        .write_in_flight(&InFlightUsage::new(
            "own-reservation",
            claim.token.epoch,
            owed(500),
        ))
        .map_err(TestError::Unexpected)?;

    let settled = settle_unresolved_usage(&store, &claim).map_err(TestError::Unexpected)?;

    assert_eq!(settled, claim.job.usage);
    assert!(
        read_in_flight(&store, "own-reservation")?.is_some(),
        "a reservation of the same epoch belongs to this attempt"
    );
    Ok(())
}

#[tokio::test]
async fn unreadable_in_flight_marker_fails_the_job_without_a_model_call() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    admit(
        &store,
        &record(
            "unreadable-reservation",
            serde_json::json!({"prompt": "hello"}),
            Budget::unbounded(),
            operator_scope(),
        )?,
    )?;
    let dir = store.root().join(USAGE_INFLIGHT_DIR);
    std::fs::create_dir_all(&dir).map_err(ctx("create reservation dir"))?;
    std::fs::write(dir.join("unreadable-reservation.json"), b"{ not json")
        .map_err(ctx("write reservation"))?;
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

    let reason = failure_reason(&store, "unreadable-reservation")?;
    assert!(
        reason.starts_with("usage accounting unresolved"),
        "{reason}"
    );
    assert_eq!(
        inner.calls.load(Ordering::SeqCst),
        0,
        "a broken reservation must never look like no reservation"
    );
    Ok(())
}

#[tokio::test]
async fn reservation_write_failure_refuses_the_round_before_spending() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let claim = admitted_claim(&store, "reserve-fail", Budget::unbounded())?;
    let ledger = persisted_ledger(&store, &claim, Budget::unbounded());
    let inner = Arc::new(FixedUsageProvider {
        calls: AtomicUsize::new(0),
        tokens_per_call: 5,
    });
    let budgeted = BudgetedModelProvider {
        inner: Arc::clone(&inner) as Arc<dyn ModelProvider>,
        ledger: Arc::clone(&ledger),
    };
    block_dir(&store, USAGE_INFLIGHT_DIR)?;

    assert!(
        budgeted.respond(empty_request()).await.is_err(),
        "a round that cannot be reserved must be refused"
    );
    assert_eq!(
        inner.calls.load(Ordering::SeqCst),
        0,
        "the inner provider is never called without a durable reservation"
    );
    let closed = some_or(
        ledger.exceeded(),
        "the failed reservation must close the ledger",
    )?;
    assert!(closed.starts_with("usage:"), "{closed}");
    assert_eq!(ledger.snapshot().tokens, 0, "nothing was spent");
    assert_eq!(stored_tokens(&store, "reserve-fail")?, 0);

    // Auch mit wieder heilem Verzeichnis bleibt der Ledger geschlossen.
    unblock_dir(&store, USAGE_INFLIGHT_DIR)?;
    assert!(budgeted.respond(empty_request()).await.is_err());
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn concurrent_rounds_keep_the_reservation_until_every_round_is_recorded() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let first = admitted_retryable_claim(&store, "concurrent", Budget::unbounded())?;
    let ledger = persisted_ledger(&store, &first, Budget::unbounded());
    let (release_a, gate_a) = tokio::sync::oneshot::channel::<()>();
    let (release_b, gate_b) = tokio::sync::oneshot::channel::<()>();
    let inner = Arc::new(GatedProvider {
        calls: AtomicUsize::new(0),
        gates: Mutex::new(VecDeque::from([(gate_a, 40), (gate_b, 30)])),
    });
    let budgeted = Arc::new(BudgetedModelProvider {
        inner: Arc::clone(&inner) as Arc<dyn ModelProvider>,
        ledger: Arc::clone(&ledger),
    });

    // Zwei Worker-Turns teilen sich den Ledger (wie im WorkDriver).
    let round_a = spawn_round(&budgeted);
    wait_for_calls(&inner.calls, 1).await?;
    let round_b = spawn_round(&budgeted);
    wait_for_calls(&inner.calls, 2).await?;

    // Runde A endet und wird verbucht, während B noch läuft: die
    // Reservierung bleibt und wurde vor dem Festschreiben angehoben.
    release_a
        .send(())
        .map_err(|()| TestError::Unexpected("round A stopped waiting".to_owned()))?;
    round_a
        .await
        .map_err(ctx("round A task"))?
        .map_err(ctx("round A"))?;
    assert_eq!(stored_tokens(&store, "concurrent")?, 40);
    let reservation = some_or(
        read_in_flight(&store, "concurrent")?,
        "round B still holds the reservation",
    )?;
    assert_eq!(
        reservation.usage_before.tokens, 40,
        "the reservation must not let the recorded round A prove round B"
    );

    // Jetzt fällt der Speicher aus, B endet, und der Worker stirbt.
    break_store(&store)?;
    block_dir(&store, USAGE_UNRESOLVED_DIR)?;
    release_b
        .send(())
        .map_err(|()| TestError::Unexpected("round B stopped waiting".to_owned()))?;
    let round_b = round_b.await.map_err(ctx("round B task"))?;
    assert!(round_b.is_err(), "round B is neither recorded nor marked");
    drop(budgeted);
    drop(ledger);
    repair_store(&store)?;
    unblock_dir(&store, USAGE_UNRESOLVED_DIR)?;

    // Der Reclaim sieht nur Runde A; Runde B ist unbewiesen.
    let second = reclaim_and_claim_again(&store, "concurrent").await?;
    assert_eq!(second.job.usage.tokens, 40);
    let reason = expect_unsettled(settle_unresolved_usage(&store, &second))?;
    assert!(reason.contains("in-flight model round"), "{reason}");
    Ok(())
}

/// Scheitert beim ersten Aufruf, nachdem er angenommen wurde (wie ein
/// SSE-Stream, der Teile geliefert hat und dann mit einem Transportfehler
/// abreißt, bevor die Usage kommt); jeder weitere Aufruf antwortet mit
/// `tokens_per_call` Tokens.
struct PartialStreamProvider {
    calls: AtomicUsize,
    tokens_per_call: u64,
}

impl ModelProvider for PartialStreamProvider {
    fn respond<'a>(&'a self, _request: harw_core::ModelRequest) -> harw_core::ModelFuture<'a> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        let mut response = harw_core::ModelResponse::text("streamed");
        response.usage = tokens_used(self.tokens_per_call);
        Box::pin(async move {
            if call == 1 {
                return Err(harw_core::ModelError::RequestFailed(
                    "test fixture: stream truncated before the usage arrived".to_owned(),
                ));
            }
            Ok::<harw_core::ModelResponse, harw_core::ModelError>(response)
        })
    }
}

#[tokio::test]
async fn partial_stream_error_keeps_the_round_reserved() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let first = admitted_retryable_claim(&store, "partial-stream", Budget::unbounded())?;
    let ledger = persisted_ledger(&store, &first, Budget::unbounded());
    let inner = Arc::new(PartialStreamProvider {
        calls: AtomicUsize::new(0),
        tokens_per_call: 25,
    });
    let budgeted = BudgetedModelProvider {
        inner: Arc::clone(&inner) as Arc<dyn ModelProvider>,
        ledger: Arc::clone(&ledger),
    };

    // Runde 1: der Provider war aufgerufen, dann riss der Stream ab.
    assert!(budgeted.respond(empty_request()).await.is_err());
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    let reservation = some_or(
        read_in_flight(&store, "partial-stream")?,
        "a dispatched round without usage stays reserved",
    )?;
    assert!(reservation.unproven_spend, "{reservation:?}");
    assert!(
        ledger.exceeded().is_none(),
        "a provider error is no accounting failure"
    );

    // Runde 2 gelingt und wird verbucht: die Reservierung wird angehoben,
    // aber nicht entfernt.
    budgeted
        .respond(empty_request())
        .await
        .map_err(ctx("round 2"))?;
    assert_eq!(stored_tokens(&store, "partial-stream")?, 25);
    let raised = some_or(
        read_in_flight(&store, "partial-stream")?,
        "the unproven round keeps its reservation",
    )?;
    assert!(raised.unproven_spend, "{raised:?}");
    assert_eq!(
        raised.usage_before.tokens, 25,
        "raised to the recorded state"
    );

    // Letzter Flush, dann stirbt der Worker.
    let outcome = ledger
        .finish(JobOutcome::Succeeded {
            result: serde_json::json!({"assistant": "done"}),
        })
        .await;
    assert!(
        matches!(outcome, JobOutcome::Succeeded { .. }),
        "{outcome:?}"
    );
    assert!(
        read_in_flight(&store, "partial-stream")?.is_some_and(|kept| kept.unproven_spend),
        "the final flush must not clear an unproven round"
    );
    drop(budgeted);
    drop(ledger);

    // Der Reclaim sieht 25 Tokens; die abgerissene Runde 1 ist darin nicht
    // enthalten: kein frisches Budget.
    let second = reclaim_and_claim_again(&store, "partial-stream").await?;
    assert_eq!(second.job.usage.tokens, 25);
    let reason = expect_unsettled(settle_unresolved_usage(&store, &second))?;
    assert!(reason.contains("in-flight model round"), "{reason}");
    assert!(reason.contains("never accounted"), "{reason}");
    Ok(())
}

#[tokio::test]
async fn cancelled_round_keeps_the_round_reserved() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let first = admitted_retryable_claim(&store, "cancelled-round", Budget::unbounded())?;
    let ledger = persisted_ledger(&store, &first, Budget::unbounded());
    // Der Sender bleibt bis zum Ende am Leben: die Runde endet nur durch den
    // Abbruch, nie durch eine Antwort oder einen Fehler des Providers.
    let (_never_released, gate) = tokio::sync::oneshot::channel::<()>();
    let inner = Arc::new(GatedProvider {
        calls: AtomicUsize::new(0),
        gates: Mutex::new(VecDeque::from([(gate, 40)])),
    });
    let budgeted = Arc::new(BudgetedModelProvider {
        inner: Arc::clone(&inner) as Arc<dyn ModelProvider>,
        ledger: Arc::clone(&ledger),
    });

    // Die Runde läuft im Provider; dann bricht ein Timeout sie ab.
    let round = spawn_round(&budgeted);
    wait_for_calls(&inner.calls, 1).await?;
    round.abort();
    let aborted = round.await;
    assert!(
        aborted
            .as_ref()
            .is_err_and(tokio::task::JoinError::is_cancelled),
        "the round must have been cancelled mid-flight"
    );

    // Der nächste Flush (hier: der letzte vor dem Ausgang) schreibt den
    // unbewiesenen Verbrauch in die Reservierung und entfernt sie nicht.
    let outcome = ledger
        .finish(JobOutcome::Failed {
            reason: "timeout".to_owned(),
        })
        .await;
    assert!(matches!(outcome, JobOutcome::Failed { .. }), "{outcome:?}");
    let reservation = some_or(
        read_in_flight(&store, "cancelled-round")?,
        "a cancelled round stays reserved",
    )?;
    assert!(reservation.unproven_spend, "{reservation:?}");
    drop(budgeted);
    drop(ledger);

    let second = reclaim_and_claim_again(&store, "cancelled-round").await?;
    let reason = expect_unsettled(settle_unresolved_usage(&store, &second))?;
    assert!(reason.contains("in-flight model round"), "{reason}");
    Ok(())
}

#[tokio::test]
async fn error_before_dispatch_releases_the_reservation() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let budget = Budget {
        max_tokens: Some(10),
        max_wall: None,
        max_tool_calls: None,
    };
    let claim = admitted_claim(&store, "before-dispatch", budget.clone())?;
    let ledger = persisted_ledger(&store, &claim, budget);
    let inner = Arc::new(FixedUsageProvider {
        calls: AtomicUsize::new(0),
        tokens_per_call: 10,
    });
    let budgeted = BudgetedModelProvider {
        inner: Arc::clone(&inner) as Arc<dyn ModelProvider>,
        ledger: Arc::clone(&ledger),
    };

    // Eine reservierte, aber nie aufgerufene Runde gibt nichts aus: das
    // Verwerfen gibt sie frei, der nächste Flush entfernt die Reservierung.
    let reserved = ledger
        .reserve_round()
        .await
        .map_err(TestError::Unexpected)?;
    assert!(read_in_flight(&store, "before-dispatch")?.is_some());
    drop(reserved);
    ledger
        .flush()
        .await
        .map_err(ctx("flush after the release"))?;
    assert!(read_in_flight(&store, "before-dispatch")?.is_none());
    assert!(!ledger.state().unproven_spend);

    // Runde 1 schöpft das Budget aus; die Abweisung der nächsten durch den
    // Ledger liegt vor dem Aufruf und hinterlässt keine Reservierung.
    budgeted
        .respond(empty_request())
        .await
        .map_err(ctx("round 1"))?;
    assert!(read_in_flight(&store, "before-dispatch")?.is_none());
    let refused = budgeted.respond(empty_request()).await;
    assert!(refused.is_err(), "the exhausted budget refuses the round");
    assert_eq!(
        inner.calls.load(Ordering::SeqCst),
        1,
        "a refused round never reaches the provider"
    );
    assert!(read_in_flight(&store, "before-dispatch")?.is_none());
    let state = ledger.state();
    assert_eq!(state.rounds_in_flight, 0);
    assert!(!state.unproven_spend);
    Ok(())
}

// ── Rundenreservierung unter der Lease (Review P1 4394111166, P2 4394114795) ──

#[tokio::test]
async fn reclaim_before_reserve_refuses_the_round_without_dispatch() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let first = admitted_retryable_claim(&store, "reserve-stale", Budget::unbounded())?;
    let stale = persisted_ledger(&store, &first, Budget::unbounded());
    let inner = Arc::new(FixedUsageProvider {
        calls: AtomicUsize::new(0),
        tokens_per_call: 5,
    });
    let budgeted = BudgetedModelProvider {
        inner: Arc::clone(&inner) as Arc<dyn ModelProvider>,
        ledger: Arc::clone(&stale),
    };

    // Die Lease läuft ab und wird zurückgefordert, bevor der alte Worker
    // seine nächste Runde reserviert.
    let second = reclaim_and_claim_again(&store, "reserve-stale").await?;
    assert!(second.token.epoch > first.token.epoch);

    assert!(
        budgeted.respond(empty_request()).await.is_err(),
        "a reclaimed worker must not start a round"
    );
    assert_eq!(
        inner.calls.load(Ordering::SeqCst),
        0,
        "the provider is never called under a lost lease"
    );
    assert!(
        read_in_flight(&store, "reserve-stale")?.is_none(),
        "the stale worker wrote no reservation"
    );
    let closed = some_or(stale.exceeded(), "the stale ledger is closed")?;
    assert!(closed.starts_with("lease:"), "{closed}");
    assert_eq!(stored_tokens(&store, "reserve-stale")?, 0);
    Ok(())
}

#[tokio::test]
async fn old_epoch_cleanup_cannot_unlink_the_new_epochs_reservation() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let id = "cleanup-race";
    let first = admitted_retryable_claim(&store, id, Budget::unbounded())?;
    let markers = UsageMarkerStore::in_job_root(store.root());
    // Epoche 1 hat eine Runde reserviert und wieder freigegeben; ihr nächster
    // Flush würde die Reservierung entfernen.
    let old = persisted_ledger(&store, &first, Budget::unbounded());
    let reserved = old.reserve_round().await.map_err(TestError::Unexpected)?;
    drop(reserved);

    // Phase 1: das Aufräumen von Epoche 1 hat seine Prüfung bestanden und
    // pausiert vor dem Entfernen. Solange es die Lease hält, kommt kein
    // Reclaim dazwischen.
    let (paused_tx, paused_rx) = std::sync::mpsc::channel::<()>();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel::<()>();
    let cleanup = std::thread::spawn({
        let store = Arc::clone(&store);
        let token = first.token.clone();
        let markers = markers.clone();
        move || {
            under_live_lease(&store, &token, &LeaseAccess::Free, || {
                let seen = markers.read_in_flight(id);
                let paused = paused_tx.send(());
                let resumed = resume_rx.recv();
                (
                    seen,
                    paused.is_ok() && resumed.is_ok(),
                    markers.remove_in_flight(id, token.epoch),
                )
            })
        }
    });
    paused_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .map_err(ctx("wait for the paused cleanup"))?;
    let contended = store.reclaim(
        &WorkId::from_str(id),
        Timestamp::now(),
        ApprovalActor::Operator {
            id: "operator".to_owned(),
        },
    );
    assert!(
        matches!(
            contended,
            Err(harw_session_store::SessionStoreError::JobLockContended { .. })
        ),
        "a reclaim must wait for the paused cleanup: {contended:?}"
    );
    resume_tx.send(()).map_err(ctx("resume the cleanup"))?;
    let joined = cleanup
        .join()
        .map_err(|_| TestError::Unexpected("the cleanup thread panicked".to_owned()))?;
    match joined {
        Ok((Ok(Some(_)), true, Ok(()))) => {}
        other => {
            return Err(TestError::Unexpected(format!(
                "the cleanup of epoch 1 must finish under its lease: {other:?}"
            )));
        }
    }

    // Phase 2: jetzt erst der Reclaim; Epoche 2 reserviert ihre Runde.
    let second = reclaim_and_claim_again(&store, id).await?;
    settle_unresolved_usage(&store, &second).map_err(TestError::Unexpected)?;
    let current = persisted_ledger(&store, &second, Budget::unbounded());
    let in_flight = current
        .reserve_round()
        .await
        .map_err(TestError::Unexpected)?;

    // Ein Aufräumen von Epoche 1, das erst jetzt weiterläuft, wird
    // abgewiesen, bevor es die Reservierung auch nur liest.
    let ran = std::sync::atomic::AtomicBool::new(false);
    let late = under_live_lease(&store, &first.token, &LeaseAccess::Free, || {
        ran.store(true, Ordering::SeqCst);
        markers.remove_in_flight(id, first.token.epoch)
    });
    assert!(matches!(late, Err(LeaseRefusal::Lost)), "{late:?}");
    assert!(
        !ran.load(Ordering::SeqCst),
        "the stale cleanup must not run"
    );
    // Ebenso der Flush des alten Ledgers (sein Schritt wäre das Entfernen).
    let flushed = old.flush().await;
    assert!(
        matches!(flushed, Err(UsageFlushError::LeaseLost)),
        "{flushed:?}"
    );

    let kept = some_or(
        read_in_flight(&store, id)?,
        "the reservation of epoch 2 survives",
    )?;
    assert_eq!(kept.epoch, second.token.epoch);
    drop(in_flight);
    Ok(())
}

#[tokio::test]
async fn old_epoch_write_cannot_overwrite_a_newer_reservation() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let id = "overwrite-race";
    let first = admitted_retryable_claim(&store, id, Budget::unbounded())?;
    let inner = Arc::new(FixedUsageProvider {
        calls: AtomicUsize::new(0),
        tokens_per_call: 5,
    });
    let stale_provider = |ledger: &Arc<PromptTokenLedger>| BudgetedModelProvider {
        inner: Arc::clone(&inner) as Arc<dyn ModelProvider>,
        ledger: Arc::clone(ledger),
    };
    let blocked = persisted_ledger(&store, &first, Budget::unbounded());
    let refused = persisted_ledger(&store, &first, Budget::unbounded());

    // Epoche 2 hält die Lease (ein laufender Werkzeugaufruf) und reserviert
    // unter diesem Halt ihre nächste Runde.
    let second = reclaim_and_claim_again(&store, id).await?;
    let current = persisted_ledger(&store, &second, Budget::unbounded());
    let permit = LedgerToolMeter {
        ledger: Arc::clone(&current),
    }
    .admit_tool_call("fs.write")
    .await
    .map_err(TestError::Unexpected)?;
    let in_flight = current
        .reserve_round()
        .await
        .map_err(TestError::Unexpected)?;
    let before = some_or(read_in_flight(&store, id)?, "reservation of epoch 2")?;
    assert_eq!(before.epoch, second.token.epoch);

    // Der alte Worker wartet vergeblich auf die Sperre und reserviert nichts.
    assert!(
        stale_provider(&blocked)
            .respond(empty_request())
            .await
            .is_err()
    );
    let closed = some_or(blocked.exceeded(), "the blocked ledger is closed")?;
    assert!(closed.starts_with("usage:"), "{closed}");
    drop(permit);
    // Ohne Halt wird er abgewiesen, weil seine Lease nicht mehr gilt.
    assert!(
        stale_provider(&refused)
            .respond(empty_request())
            .await
            .is_err()
    );
    let closed = some_or(refused.exceeded(), "the refused ledger is closed")?;
    assert!(closed.starts_with("lease:"), "{closed}");
    assert_eq!(
        inner.calls.load(Ordering::SeqCst),
        0,
        "the stale worker never reaches the provider"
    );
    let markers = UsageMarkerStore::in_job_root(store.root());
    let stale = InFlightUsage::new(id, first.token.epoch, owed(999));
    let written = under_live_lease(&store, &first.token, &LeaseAccess::Free, || {
        markers.write_in_flight(&stale)
    });
    assert!(matches!(written, Err(LeaseRefusal::Lost)), "{written:?}");
    // Zweite Linie: auch ohne Lease-Prüfung lehnt der Marker-Speicher die
    // ältere Epoche ab.
    assert!(markers.write_in_flight(&stale).is_err());

    let after = some_or(read_in_flight(&store, id)?, "reservation of epoch 2")?;
    assert_eq!(after, before, "the newer reservation is untouched");
    drop(in_flight);
    Ok(())
}

#[tokio::test]
async fn overrun_round_stays_reserved_and_blocks_a_reclaim() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let budget = Budget {
        max_tokens: Some(50),
        max_wall: None,
        max_tool_calls: None,
    };
    let first = admitted_retryable_claim(&store, "overrun", budget.clone())?;
    let ledger = persisted_ledger(&store, &first, budget);
    let inner = Arc::new(FixedUsageProvider {
        calls: AtomicUsize::new(0),
        tokens_per_call: 30,
    });
    let budgeted = BudgetedModelProvider {
        inner: Arc::clone(&inner) as Arc<dyn ModelProvider>,
        ledger: Arc::clone(&ledger),
    };

    budgeted
        .respond(empty_request())
        .await
        .map_err(ctx("round 1"))?;
    // Runde 2 meldet 30 weitere Tokens, die das Budget von 50 überschreiten:
    // ausgegeben, aber nicht verbuchbar.
    assert!(budgeted.respond(empty_request()).await.is_err());
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    let outcome = ledger
        .finish(JobOutcome::Blocked {
            reason: "stopped".to_owned(),
        })
        .await;
    assert!(matches!(outcome, JobOutcome::Blocked { .. }), "{outcome:?}");
    assert_eq!(stored_tokens(&store, "overrun")?, 30);
    let reservation = some_or(
        read_in_flight(&store, "overrun")?,
        "the unaccounted round stays reserved",
    )?;
    assert_eq!(reservation.usage_before.tokens, 30);

    let second = reclaim_and_claim_again(&store, "overrun").await?;
    let reason = expect_unsettled(settle_unresolved_usage(&store, &second))?;
    assert!(reason.contains("in-flight model round"), "{reason}");
    Ok(())
}
