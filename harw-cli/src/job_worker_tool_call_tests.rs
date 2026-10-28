//! Tests der Werkzeugaufruf-Buchung beim Dispatch (Review P1-b zu #151): der
//! [`LedgerToolMeter`] bucht jeden Aufruf gegen `max_tool_calls` und schreibt
//! ihn unter der aktuellen Lease fest, BEVOR das Werkzeug läuft. Jeder Fehler
//! (Budget, Lease, Speicher) weist den Aufruf ab.
//!
//! Den Speicherfehler erzeugt `usage_tests::break_store` (Sperrverzeichnis des
//! Job-Stores durch eine Datei ersetzt); Lease-Verlust und Reclaim laufen über
//! den echten Job-Store (`usage_tests::reclaim_and_claim_again`).
//!
//! Lease-Zaun (Review P1 4394111171): der Permit eines zugelassenen Aufrufs
//! hält die Job-Sperre, bis die Ausführung zurückkehrt; ein Reclaim in dieser
//! Zeit scheitert an der belegten Sperre und kommt erst danach durch.

use super::prompt_claim_guard_tests::temp_dir;
use super::usage_tests::{
    admitted_claim, admitted_retryable_claim, break_store, persisted_ledger, read_marker,
    reclaim_and_claim_again, repair_store,
};
use super::*;
use crate::test_support::{TestError, TestResult, ctx, some_or};
use harw_core::ToolCallMeter;
use harw_job_runtime::BudgetUsage;
use harw_types::ApprovalActor;

// ── Fixtures ──────────────────────────────────────────────────────────────

// Budget, das nur Werkzeugaufrufe begrenzt.
fn tool_budget(max_tool_calls: u32) -> Budget {
    Budget {
        max_tokens: None,
        max_wall: None,
        max_tool_calls: Some(max_tool_calls),
    }
}

// Der Meter, den die Worker-Pfade am `TurnControl` registrieren.
fn meter_for(ledger: &Arc<PromptTokenLedger>) -> LedgerToolMeter {
    LedgerToolMeter {
        ledger: Arc::clone(ledger),
    }
}

fn stored_usage(store: &JobStore, id: &str) -> TestResult<BudgetUsage> {
    let stored = store.get(&WorkId::from_str(id)).map_err(ctx("get job"))?;
    Ok(stored.job.usage)
}

fn stored_tool_calls(store: &JobStore, id: &str) -> TestResult<u32> {
    Ok(stored_usage(store, id)?.tool_calls)
}

// Erwartet eine Abweisung und liefert ihren Grund.
fn expect_refused(
    admitted: Result<harw_core::ToolCallPermit, String>,
    what: &str,
) -> TestResult<String> {
    match admitted {
        Err(reason) => Ok(reason),
        Ok(permit) => Err(TestError::Unexpected(format!(
            "{what}: the tool call must be refused, got {permit:?}"
        ))),
    }
}

// Ein Reclaim durch einen vertrauenswürdigen Operator.
fn try_reclaim(
    store: &JobStore,
    id: &str,
) -> Result<harw_session_store::ExpiredJob, harw_session_store::SessionStoreError> {
    store.reclaim(
        &WorkId::from_str(id),
        Timestamp::now(),
        ApprovalActor::Operator {
            id: "operator".to_owned(),
        },
    )
}

fn is_contended<T>(result: &Result<T, harw_session_store::SessionStoreError>) -> bool {
    matches!(
        result,
        Err(harw_session_store::SessionStoreError::JobLockContended { .. })
    )
}

// ── Buchung vor der Ausführung ────────────────────────────────────────────

#[tokio::test]
async fn tool_calls_are_charged_and_persisted_before_execution() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let claim = admitted_claim(&store, "tool-charge", tool_budget(2))?;
    let ledger = persisted_ledger(&store, &claim, tool_budget(2));
    let meter = meter_for(&ledger);

    meter
        .admit_tool_call("fs.read")
        .await
        .map_err(TestError::Unexpected)?;
    assert_eq!(
        stored_tool_calls(&store, "tool-charge")?,
        1,
        "an admitted call is durable before the tool runs"
    );
    meter
        .admit_tool_call("fs.read")
        .await
        .map_err(TestError::Unexpected)?;
    assert_eq!(stored_tool_calls(&store, "tool-charge")?, 2);

    let reason = expect_refused(meter.admit_tool_call("fs.read").await, "third call")?;
    assert!(
        reason.starts_with("budget: tool-call limit of 2 is exhausted"),
        "{reason}"
    );
    assert_eq!(
        stored_tool_calls(&store, "tool-charge")?,
        2,
        "a refused call is not charged"
    );
    assert_eq!(
        ledger.exceeded().as_deref(),
        Some(reason.as_str()),
        "the exhausted budget closes the ledger"
    );
    assert!(ledger.admit_round().is_err(), "no further model round");
    expect_refused(
        meter.admit_tool_call("fs.write").await,
        "call after the budget refusal",
    )?;
    Ok(())
}

#[tokio::test]
async fn persist_failure_refuses_the_tool_call() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let claim = admitted_claim(&store, "tool-persist-fail", tool_budget(5))?;
    let ledger = persisted_ledger(&store, &claim, tool_budget(5));
    let meter = meter_for(&ledger);
    break_store(&store)?;

    let reason = expect_refused(
        meter.admit_tool_call("fs.write").await,
        "call with an unwritable job store",
    )?;
    assert!(reason.starts_with("usage:"), "{reason}");
    let closed = some_or(
        ledger.exceeded(),
        "an unrecordable call must close the ledger",
    )?;
    assert!(closed.starts_with("usage:"), "{closed}");
    assert!(ledger.admit_round().is_err(), "no further model round");
    expect_refused(
        meter.admit_tool_call("fs.read").await,
        "call after the persist failure",
    )?;

    // Der Verbrauch geht über den Marker in den nächsten Claim ein.
    let marker = some_or(read_marker(&store, "tool-persist-fail")?, "marker written")?;
    assert_eq!(marker.usage.tool_calls, 1);
    assert_eq!(marker.epoch, claim.token.epoch);

    repair_store(&store)?;
    assert_eq!(
        stored_tool_calls(&store, "tool-persist-fail")?,
        0,
        "the job record never saw the call"
    );
    Ok(())
}

#[tokio::test]
async fn stale_epoch_cannot_admit_or_record() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let first = admitted_retryable_claim(&store, "tool-stale", tool_budget(5))?;
    let stale = persisted_ledger(&store, &first, tool_budget(5));

    // Die Lease geht verloren; ein anderer Versuch hält den Job.
    let second = reclaim_and_claim_again(&store, "tool-stale").await?;
    assert!(second.token.epoch > first.token.epoch);
    let before = stored_usage(&store, "tool-stale")?;

    let reason = expect_refused(
        meter_for(&stale).admit_tool_call("fs.write").await,
        "call under a lost lease",
    )?;
    assert_eq!(reason, USAGE_LEASE_LOST_REASON);
    assert_eq!(
        stored_usage(&store, "tool-stale")?,
        before,
        "a stale epoch must not write usage"
    );
    assert!(
        read_marker(&store, "tool-stale")?.is_none(),
        "a lost lease writes no marker"
    );
    assert!(
        stale.admit_round().is_err(),
        "the stale ledger is closed for model rounds as well"
    );
    expect_refused(
        meter_for(&stale).admit_tool_call("fs.read").await,
        "second call under a lost lease",
    )?;

    // Der aktuelle Lease-Halter bucht ungehindert.
    let current = persisted_ledger(&store, &second, tool_budget(5));
    meter_for(&current)
        .admit_tool_call("fs.write")
        .await
        .map_err(TestError::Unexpected)?;
    assert_eq!(stored_tool_calls(&store, "tool-stale")?, 1);
    Ok(())
}

#[tokio::test]
async fn reclaim_sees_consumed_tool_calls() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let budget = tool_budget(3);
    let first = admitted_retryable_claim(&store, "tool-reclaim", budget.clone())?;
    let ledger = persisted_ledger(&store, &first, budget.clone());
    let meter = meter_for(&ledger);
    meter
        .admit_tool_call("fs.read")
        .await
        .map_err(TestError::Unexpected)?;
    meter
        .admit_tool_call("fs.write")
        .await
        .map_err(TestError::Unexpected)?;
    // Der Worker stirbt nach zwei Aufrufen.
    drop(meter);
    drop(ledger);

    let second = reclaim_and_claim_again(&store, "tool-reclaim").await?;
    assert_eq!(
        second.job.usage.tool_calls, 2,
        "the reclaim sees the calls consumed before the crash"
    );
    let settled = settle_unresolved_usage(&store, &second).map_err(TestError::Unexpected)?;
    assert_eq!(settled.tool_calls, 2);

    // Nur der Rest des Budgets bleibt: genau ein weiterer Aufruf.
    let reclaimed =
        PromptTokenLedger::persisted(budget, settled, Arc::clone(&store), second.token.clone());
    let meter = meter_for(&reclaimed);
    meter
        .admit_tool_call("fs.read")
        .await
        .map_err(TestError::Unexpected)?;
    let reason = expect_refused(
        meter.admit_tool_call("fs.read").await,
        "fourth call of the job",
    )?;
    assert!(
        reason.starts_with("budget: tool-call limit of 3 is exhausted"),
        "{reason}"
    );
    assert_eq!(stored_tool_calls(&store, "tool-reclaim")?, 3);
    Ok(())
}

// ── Lease-Zaun bis zur Seiteneffekt-Grenze (Review P1 4394111171) ──────────

#[tokio::test]
async fn reclaim_after_metering_waits_for_the_tool_and_then_refuses_the_old_worker() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let id = "tool-fence";
    let first = admitted_retryable_claim(&store, id, tool_budget(5))?;
    let ledger = persisted_ledger(&store, &first, tool_budget(5));
    let meter = meter_for(&ledger);

    // Gebucht und zugelassen; der Dispatcher hält den Permit, bis das
    // Werkzeug zurückkehrt.
    let permit = meter
        .admit_tool_call("fs.write")
        .await
        .map_err(TestError::Unexpected)?;
    assert_eq!(stored_tool_calls(&store, id)?, 1);

    // Ein zweiter Thread läuft ab und fordert zurück, sobald er darf.
    let (contended_tx, contended_rx) = std::sync::mpsc::channel::<()>();
    let reclaimer = std::thread::spawn({
        let store = Arc::clone(&store);
        move || -> Result<u32, String> {
            let mut contended = 0_u32;
            loop {
                match try_reclaim(&store, id) {
                    Ok(_reclaimed) => return Ok(contended),
                    Err(harw_session_store::SessionStoreError::JobLockContended { .. }) => {
                        contended = contended.saturating_add(1);
                        if contended == 1 && contended_tx.send(()).is_err() {
                            return Err("the test stopped listening".to_owned());
                        }
                        if contended > 5_000 {
                            return Err("the job lock was never released".to_owned());
                        }
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(other) => return Err(other.to_string()),
                }
            }
        }
    });
    contended_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .map_err(ctx("wait for the first reclaim attempt"))?;

    // Der Seiteneffekt des Werkzeugs: er läuft, während die Lease noch dem
    // alten Worker gehört, und der Reclaim wartet.
    let during = store.get(&WorkId::from_str(id)).map_err(ctx("get job"))?;
    let holder = some_or(during.lease, "the job is still leased")?;
    assert_eq!(holder.epoch, first.token.epoch);
    std::fs::write(temp.path().join("side-effect"), b"written under the lease")
        .map_err(ctx("tool side effect"))?;
    assert!(
        !reclaimer.is_finished(),
        "the reclaim must not pass while the permit is held"
    );

    // Das Werkzeug ist zurückgekehrt: der Permit fällt, der Reclaim kommt
    // durch.
    drop(permit);
    let contended = reclaimer
        .join()
        .map_err(|_| TestError::Unexpected("the reclaim thread panicked".to_owned()))?
        .map_err(TestError::Unexpected)?;
    assert!(contended >= 1, "the reclaim had to wait");
    let after = store.get(&WorkId::from_str(id)).map_err(ctx("get job"))?;
    assert!(after.lease.is_none(), "the old lease is revoked");

    // Der alte Worker wird danach abgewiesen und schreibt nichts mehr.
    let reason = expect_refused(
        meter.admit_tool_call("fs.write").await,
        "call after the reclaim",
    )?;
    assert_eq!(reason, USAGE_LEASE_LOST_REASON);
    let flushed = ledger.flush().await;
    assert!(
        matches!(flushed, Err(UsageFlushError::LeaseLost)),
        "{flushed:?}"
    );
    assert_eq!(stored_tool_calls(&store, id)?, 1);
    Ok(())
}

#[tokio::test]
async fn parallel_permits_share_one_hold() -> TestResult {
    let temp = temp_dir()?;
    let store = Arc::new(JobStore::new(temp.path()));
    let id = "tool-shared-hold";
    let claim = admitted_claim(&store, id, tool_budget(5))?;
    let ledger = persisted_ledger(&store, &claim, tool_budget(5));
    let meter = meter_for(&ledger);

    let first = meter
        .admit_tool_call("fs.read")
        .await
        .map_err(TestError::Unexpected)?;
    assert_eq!(stored_tool_calls(&store, id)?, 1);
    assert_eq!(ledger.hold_slot().strong_count(), 1);

    // Der zweite Aufruf derselben Antwort wird zugelassen, während der erste
    // noch läuft: er teilt den Halt, statt an der eigenen Sperre zu scheitern.
    let second = meter
        .admit_tool_call("fs.read")
        .await
        .map_err(TestError::Unexpected)?;
    assert!(ledger.exceeded().is_none(), "sharing is no failure");
    assert_eq!(
        ledger.hold_slot().strong_count(),
        2,
        "both permits hold the same lease hold"
    );
    // Gebucht ist er durabel als Marker (der Job-Datensatz ist gesperrt).
    let booked = some_or(read_marker(&store, id)?, "the second call is booked")?;
    assert_eq!(booked.usage.tool_calls, 2);
    assert_eq!(stored_tool_calls(&store, id)?, 1);
    // Auch ein Flush oder eine Rundenreservierung eines anderen Turns läuft
    // unter dem Halt, statt auf die Sperre zu warten.
    ledger.flush().await.map_err(ctx("flush under the hold"))?;
    let round = ledger
        .reserve_round()
        .await
        .map_err(TestError::Unexpected)?;
    drop(round);

    // Ein Reclaim kommt an keinem der beiden Permits vorbei.
    assert!(is_contended(&try_reclaim(&store, id)));
    drop(first);
    assert_eq!(ledger.hold_slot().strong_count(), 1);
    assert!(is_contended(&try_reclaim(&store, id)));
    drop(second);
    assert_eq!(
        ledger.hold_slot().strong_count(),
        0,
        "the last permit releases the hold"
    );
    let free = store
        .hold_live_lease(&claim.token)
        .map_err(ctx("the job lock is free again"))?;
    drop(free);

    // Der nächste Flush ohne Halt schreibt fest und räumt Marker und
    // Reservierung auf.
    ledger.flush().await.map_err(ctx("flush after the hold"))?;
    assert_eq!(stored_tool_calls(&store, id)?, 2);
    assert!(read_marker(&store, id)?.is_none(), "the booking is cleared");
    assert!(
        usage_marker::UsageMarkerStore::in_job_root(store.root())
            .read_in_flight(id)
            .map_err(TestError::Unexpected)?
            .is_none(),
        "the released round leaves no reservation"
    );
    Ok(())
}
