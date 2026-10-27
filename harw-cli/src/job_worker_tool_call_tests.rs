//! Tests der Werkzeugaufruf-Buchung beim Dispatch (Review P1-b zu #151): der
//! [`LedgerToolMeter`] bucht jeden Aufruf gegen `max_tool_calls` und schreibt
//! ihn unter der aktuellen Lease fest, BEVOR das Werkzeug läuft. Jeder Fehler
//! (Budget, Lease, Speicher) weist den Aufruf ab.
//!
//! Den Speicherfehler erzeugt `usage_tests::break_store` (Sperrverzeichnis des
//! Job-Stores durch eine Datei ersetzt); Lease-Verlust und Reclaim laufen über
//! den echten Job-Store (`usage_tests::reclaim_and_claim_again`).

use super::prompt_claim_guard_tests::temp_dir;
use super::usage_tests::{
    admitted_claim, admitted_retryable_claim, break_store, persisted_ledger, read_marker,
    reclaim_and_claim_again, repair_store,
};
use super::*;
use crate::test_support::{TestError, TestResult, ctx, some_or};
use harw_core::ToolCallMeter;
use harw_job_runtime::BudgetUsage;

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
fn expect_refused(admitted: Result<(), String>, what: &str) -> TestResult<String> {
    match admitted {
        Err(reason) => Ok(reason),
        Ok(()) => Err(TestError::Unexpected(format!(
            "{what}: the tool call must be refused"
        ))),
    }
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
