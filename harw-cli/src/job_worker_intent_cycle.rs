//! Intent-cycle worker: an `intent_cycle` job drives one PL-90 read-only
//! intent cycle through `harw_plan_bridge::CycleDriver`.
//!
//! # One claim, one fenced driver run
//! `intent_cycle.start` (`harw_ops::intent_cycle`) admits the job with the
//! approved [`harw_plan_bridge::ReadOnlyCycleSpec`] in its input. Every
//! claim:
//! 1. checks the claim fence and that the input does not redirect the scope;
//! 2. rebuilds admission, read-target map and acceptance checks from the
//!    stored spec with exactly the validation `intent_cycle.start` used
//!    ([`ReadOnlyCycleSpec::plan`](harw_plan_bridge::ReadOnlyCycleSpec::plan));
//! 3. derives the trusted authority ceiling of the job from the runtime
//!    profile table (`runtime_jobs::job_sandbox`, read-only explore entry:
//!    `{ReadWorkspace}` on the project root, nothing else);
//! 4. creates the durable cycle record on the first claim (epoch 0, so the
//!    first lease takes the regular reissue-and-rebind path) or loads it;
//! 5. runs the driver under the claim's [`LeaseToken`](harw_job_core::LeaseToken):
//!    [`ModelCycleProposer`] over the job's (budgeted) provider proposes,
//!    [`ExplorerCycleExecutor`] reads and verifies, [`CeilingReissuer`]
//!    re-cuts the persisted authority snapshot against the current ceiling.
//!
//! A retryable proposer outage (rate limit, overload) leaves the cycle
//! non-terminal. `JobOutcome::Failed` is terminal in the job store (retry
//! policies only apply to lease expiry/reclaim and an explicit operator
//! `retry`), so the worker itself re-runs the driver under the same lease after
//! a bounded backoff ([`PROPOSER_RETRY_BACKOFF`]); only when that is used up
//! does the job fail, with the cycle still non-terminal so an operator retry
//! resumes the fenced checkpoint. The job has two attempts
//! (`harw_ops::intent_cycle`): consumed wall time and tokens are persisted
//! lease-fenced (`PromptTokenLedger::persisted` flushes to
//! `JobStore::record_usage`), so a reclaim after a lost lease resumes with the
//! consumed budget (`remaining_wall` reads the persisted wall) instead of a
//! fresh one. Usage that could not be persisted is left as a durable marker;
//! every claim settles it first (`settle_claim_usage`) and fails the job
//! without a model call if it cannot. Everything else maps through
//! `harw_plan_bridge::job_outcome`: `CompletionProposed` is a succeeded job
//! that still needs owner acceptance.
//!
//! # Routes
//! The proposer runs on a [`RoutePolicy`] built by [`routes_from`]: the
//! default route is the configured `Explorer` internal model (when it runs on
//! the job's provider), falling back to the default model. When the default
//! model differs from the explorer model it becomes the escalation route: the
//! proposer switches to it after 2 consecutive refusals or 2 stalled
//! transitions (`RoutePolicy::with_escalation(route, 2, 2)`). The choice is
//! made from trusted counters only; no model output influences it. Both
//! candidates are canonicalised (catalog key, id or alias → the requested
//! model id) and admitted against the provider lifecycle on the job's
//! provider first, exactly like an explicit live selection
//! (`harw_runtime::live_model::check_explicit_selection`), because the
//! proposer puts the route's model straight into the request: a retired
//! default never becomes the escalation route, a retired explorer leaves the
//! default as the only route, a deprecated model stays with a warning.
//! Without any admitted model the job is blocked, and the blocked reason names
//! the actual cause (`RouteFailure`): the configuration could not be
//! read or assembled, no model is configured at all, the only Explorer model
//! cannot run on this job's provider (or resolves to the main model) and no
//! default exists, or every configured model is retired. No reasoning effort
//! is configured here (the internal-model settings carry none), so the
//! provider default applies.
//!
//! # Excerpt cache
//! One [`ExplorerReadCache`] per claim is shared by the executor (it fills the
//! cache with the text of every successful read) and the proposer (it shows
//! bounded excerpts of observed repository evidence, and only when the cached
//! text carries exactly the digest the checkpoint recorded). The cache is
//! process-local and digest-bound; it is not persisted. After a lease
//! takeover (the loaded checkpoint has `fence.sequence > 0`) the worker
//! re-reads the recorded evidence files once ([`warm_cache`], on a blocking
//! thread) so the proposer sees the same excerpts it would have seen without
//! the restart; files that changed meanwhile or are unreadable are skipped.
//!
//! # Learning and recall (W09a/W09b)
//! Only when the trusted job input names a project memories root
//! (`IntentCycleJobInput::learning_root`, written solely by
//! `intent_cycle.start` from the operator's resolved home); without it
//! nothing is learned or recalled. Before the driver runs, the matching
//! project lessons (at most eight lines) are handed to the proposer as
//! low-trust data (`ModelCycleProposer::with_lessons`). After the run,
//! whatever its result (wall-clock timeout included), the pending
//! observations are drained once into project pitfall facts and acknowledged
//! under the claim's lease; that also settles entries an earlier claim left
//! between commit and acknowledgement, including those it committed with a
//! terminal step: the driver rebinds the cycle to the claim's lease before it
//! returns, for an already terminal cycle too (only the fence epoch moves; no
//! step runs). There is no drain before the run: the store fence needs the
//! rebound lease, which only exists after the driver ran. A run that ended
//! before its rebind (refused reissue, resume or rebind, or the timeout)
//! leaves the drain refused (`NotRebound`) and the entries pending for the
//! next claim. Store access runs on blocking threads; a drain or recall
//! failure is only logged and never changes the job outcome.
//!
//! # Fenced to the live claim
//! The cycle store is opened with a lease guard
//! ([`CycleStore::with_lease_liveness`]) backed by
//! [`JobStore::hold_live_lease`]: every fenced read and write — the
//! driver's rebind, journal and commits as well as the drain's read,
//! decision and acknowledgement — first renews the claim's lease (a fresh
//! TTL window, persisted like a heartbeat), then runs while the job's record
//! lock is held and the token was verified under it as the current,
//! non-expired lease. No cancellation, reclaim or expiry scan can replace
//! the lease between that verification and the cycle write, and the write's
//! last step checks the renewed deadline before it is published
//! ([`JobLeaseGuard::still_live`]). Lock order: job record first, cycle
//! record second. The epoch fence alone would
//! still admit an expired lease until a reclaim rebinds the cycle. So once
//! the job lease has expired or been revoked (lost heartbeat, operator
//! cancellation), the driver's next write fails closed and the drain learns
//! nothing — whichever way the run ended, cancellation and wall-clock
//! timeout included; the observations stay pending for the claim that takes
//! over.
//!
//! The holds are short (one cycle-record write or read). A heartbeat renewal
//! that meets one sees `JobLockContended`, which `DurableJobRunner` treats as
//! transient and retries; `JobStore::hold_live_lease` itself retries a lock
//! it meets held by the heartbeat or the usage flush, so neither side makes
//! a live lease look lost.
//!
//! Learning is delivered in two phases (see
//! `intent_cycle_learning::drain_cycle_observations`): the drain commits its
//! decision under the held lease (accepted lessons stay in the outbox), then
//! writes the facts idempotently, and acknowledges each lesson only once its
//! fact is durable.
//!
//! # Workspace binding
//! The ceiling is cut from the worker's `runtime_root.cwd`. `claim.scope`
//! carries only the submitter's tenant/workspace *aliases* (the worker
//! sandbox always uses tenant `job`, workspace `project`), so the ids are not
//! comparable. `intent_cycle.start` therefore records the caller's canonical
//! workspace root in `IntentCycleJobInput::workspace_root`, and the worker
//! refuses (`Blocked`, before any record or model call) unless it equals the
//! canonical root of its own sandbox; a missing root is refused too.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use harw_core::ModelProvider;
use harw_core::cancel::{CancelReason, CancelToken};
use harw_job_runtime::{JobClaim, JobKind, JobOutcome, LeaseToken};
use harw_ops::intent_cycle::IntentCycleJobInput;
use harw_plan::PlanNodeKind;
use harw_plan_bridge::{
    CeilingReissuer, CycleCheckpoint, CycleDriver, CycleRecord, CycleRunError, CycleStore,
    CycleStoreError, DEFAULT_CACHE_BYTES, DEFAULT_MAX_READ_BYTES, ExplorerCycleExecutor,
    ExplorerReadCache, INTENT_CYCLE_JOB_KIND, LeaseHold, LeaseLivenessGuard, ModelCycleProposer,
    ProposerRoute, RoutePolicy, job_outcome, warm_cache,
};
use harw_runtime::live_model;
use harw_session_store::JobStore;
use harw_session_store::job_store::JobLeaseGuard;
use harw_types::{ModelId, SessionId};

use super::{
    BudgetedModelProvider, JobWorkerContext, PromptTokenLedger, WorkerExecutionControl,
    check_claim_fence, check_input_declared_scope, job_state_store, sanitize_failure,
};
use crate::runtime_jobs::{JobAssemblyInputs, JobEntry, job_assembly, job_principal, job_sandbox};

/// Output cap of one proposal (one small JSON object).
const PROPOSAL_MAX_OUTPUT_TOKENS: u32 = 512;

/// Consecutive refusals after which the proposer escalates to the default model.
const ESCALATE_AFTER_REFUSALS: u32 = 2;

/// Stalled transitions after which the proposer escalates to the default model.
const ESCALATE_AT_STALL_TRANSITIONS: u32 = 2;

/// Fallback wall budget when the job carries none.
const DEFAULT_CYCLE_WALL: Duration = Duration::from_secs(30 * 60);

/// In-claim waits before the driver is re-run after a retryable proposer
/// outage; once used up the job fails.
const PROPOSER_RETRY_BACKOFF: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(20),
    Duration::from_secs(60),
];

/// Aborts the cancellation bridge when the claim future ends or is dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// `true` for the intent-cycle discriminator.
pub(super) fn is_intent_cycle_kind(kind: &JobKind) -> bool {
    matches!(kind, JobKind::Custom(name) if name == INTENT_CYCLE_JOB_KIND)
}

/// Executes one claimed intent-cycle job (see module docs).
pub(super) async fn execute_intent_cycle_claim(
    mut claim: JobClaim,
    input: serde_json::Value,
    provider: Arc<dyn ModelProvider>,
    job_store: Arc<JobStore>,
    control: Arc<WorkerExecutionControl>,
    context: &JobWorkerContext,
) -> JobOutcome {
    let work_id = claim.job.id.clone();
    if let Err(reason) =
        check_claim_fence(&claim).and_then(|()| check_input_declared_scope(&claim.scope, &input))
    {
        tracing::warn!(work_id = %work_id.as_str(), reason = %reason, "intent-cycle job rejected before any model call");
        return JobOutcome::Failed { reason };
    }
    let record = match job_store.get(&work_id) {
        Ok(record) => record,
        Err(error) => {
            return JobOutcome::Failed {
                reason: sanitize_failure(&format!("intent-cycle job unreadable: {error}")),
            };
        }
    };
    let Ok(cycle_input) = IntentCycleJobInput::from_job(&record) else {
        return JobOutcome::Failed {
            reason: "intent-cycle input is malformed".to_owned(),
        };
    };
    let plan = match cycle_input.spec.plan(&cycle_input.intent_id) {
        Ok(plan) => plan,
        Err(error) => {
            return JobOutcome::Failed {
                reason: sanitize_failure(&format!("intent-cycle spec refused: {error}")),
            };
        }
    };
    let Some(runtime_root) = context.runtime_root.as_ref() else {
        return JobOutcome::Blocked {
            reason: super::MISSING_RUNTIME_ROOT.to_owned(),
        };
    };

    // Trusted ceiling: the read-only explore sandbox of the job's project.
    let sandbox = match job_sandbox(
        JobEntry::PlanNode {
            kind: PlanNodeKind::Explore,
            may_write: false,
        },
        &runtime_root.cwd,
    ) {
        Ok(sandbox) => sandbox,
        Err(error) => {
            tracing::error!(work_id = %work_id.as_str(), error = %error, "intent-cycle sandbox unavailable");
            return JobOutcome::Blocked {
                reason: "intent cycle: the job workspace sandbox is unavailable".to_owned(),
            };
        }
    };
    // The cycle was approved for one workspace root; the worker's ceiling
    // must be bound to exactly that root (fail closed, before any record or
    // model call). Ids differ by construction (tenant alias), so the
    // canonical roots are compared, as `ensure_same_workspace_root` does.
    if cycle_input.workspace_root.as_deref() != Some(sandbox.workspace().canonical_root()) {
        tracing::warn!(work_id = %work_id.as_str(), "intent-cycle workspace root mismatch");
        return JobOutcome::Blocked {
            reason:
                "intent cycle: the job was approved for another workspace than this worker serves"
                    .to_owned(),
        };
    }
    let authority = sandbox.authority().clone();

    let routes = match proposer_routes(context, runtime_root, &job_store, &provider, &claim) {
        Ok(routes) => routes,
        Err(failure) => return route_blocked(&failure),
    };

    // Fenced to the live claim, not only to its epoch (see "Fenced to the
    // live claim"): the driver and the learning drain share this store.
    let store = match CycleStore::open_in_job_root(job_store.root()) {
        Ok(store) => Arc::new(store.with_lease_liveness(lease_liveness(&job_store))),
        Err(error) => {
            return JobOutcome::Failed {
                reason: sanitize_failure(&format!("intent-cycle store unavailable: {error}")),
            };
        }
    };
    // Offene Usage eines früheren Versuchs zuerst verrechnen (Review P1 zu
    // #151): `remaining_wall` und der Ledger gehen vom verrechneten Stand aus,
    // ein Reclaim startet nie mit frischem Budget.
    if let Err(reason) = super::settle_claim_usage(&job_store, &mut claim).await {
        tracing::error!(work_id = %work_id.as_str(), reason = %reason, "intent-cycle usage accounting unresolved");
        return JobOutcome::Failed { reason };
    }
    // Budget first: an exhausted wall budget must never become a fresh one,
    // and no record is created or model called for it.
    let Some(remaining) = remaining_wall(&claim.job.budget, &claim.job.usage) else {
        return JobOutcome::Failed {
            reason: "budget: the wall-clock budget is already exhausted".to_owned(),
        };
    };
    if let Err(reason) = ensure_record(&store, &cycle_input, &claim, &plan.admission, &authority) {
        return JobOutcome::Failed { reason };
    }
    // Shared by the executor (fills it on every read) and the proposer (shows
    // digest-bound excerpts). Process-local, so a lease takeover starts empty
    // and is warmed from the recorded evidence.
    let cache = ExplorerReadCache::new(DEFAULT_CACHE_BYTES);
    if let Some(filled) = warm_after_takeover(
        Arc::clone(&store),
        cycle_input.cycle_id.clone(),
        cache.clone(),
        authority.clone(),
    )
    .await
    {
        tracing::info!(work_id = %work_id.as_str(), filled, "intent cycle excerpt cache warmed after takeover");
    }
    // Learning and recall only under the trusted project memories root.
    let learning_root = cycle_input.learning_root.as_deref();
    let lessons = recall_lessons(learning_root, plan.admission.objective()).await;

    let ledger = PromptTokenLedger::persisted(
        claim.job.budget.clone(),
        claim.job.usage.clone(),
        Arc::clone(&job_store),
        claim.token.clone(),
    );
    let _flusher = ledger.spawn_flusher(super::USAGE_FLUSH_INTERVAL);
    let budgeted: Arc<dyn ModelProvider> = Arc::new(BudgetedModelProvider {
        inner: Arc::clone(&provider),
        ledger: Arc::clone(&ledger),
    });
    let proposer = ModelCycleProposer::new(budgeted, routes)
        .with_excerpts(cache.clone())
        .with_lessons(lessons);
    let executor =
        ExplorerCycleExecutor::new(authority.clone(), plan.targets, plan.checks).with_cache(cache);
    let mut driver = CycleDriver::new(
        Arc::clone(&store),
        proposer,
        executor,
        Arc::new(CeilingReissuer::new(authority)),
    );

    // Job cancellation (stop, lease loss) → driver cancel token.
    let cancel = CancelToken::new();
    let _bridge = {
        let cancel = cancel.clone();
        let mut cancellation = control.cancellation();
        AbortOnDrop(tokio::spawn(async move {
            loop {
                if *cancellation.borrow() {
                    cancel.cancel(CancelReason::User);
                    return;
                }
                if cancellation.changed().await.is_err() {
                    return;
                }
            }
        }))
    };

    let cycle_id = cycle_input.cycle_id.clone();
    // Borrow the field up front: `plan.targets`/`plan.checks` were moved out and an
    // async block captures whole variables, not disjoint fields.
    let admission = &plan.admission;
    let run = async {
        let mut backoff = PROPOSER_RETRY_BACKOFF.iter();
        loop {
            let result = driver
                .run(&cycle_id, &claim.token, admission, &cancel)
                .await;
            match result {
                Err(CycleRunError::ProposerUnavailable(reason)) => {
                    let Some(wait) = backoff.next() else {
                        return Err(CycleRunError::ProposerUnavailable(reason));
                    };
                    tracing::info!(work_id = %work_id.as_str(), wait_secs = wait.as_secs(), reason = %reason, "intent cycle proposer unavailable; retrying in-claim");
                    // A cancel ends the wait; the next `run` then reports it.
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => {}
                        () = tokio::time::sleep(*wait) => {}
                    }
                }
                other => return other,
            }
        }
    };
    let finished = tokio::time::timeout(remaining, run).await;
    // Learn from what the run committed, whatever its result; the drain never
    // changes the outcome.
    drain_learning(&store, &cycle_id, &claim.token, learning_root).await;
    let outcome = match finished {
        Ok(result) => {
            if let Err(error) = &result {
                tracing::warn!(work_id = %work_id.as_str(), error = %error, "intent cycle run ended with an error");
            }
            job_outcome(&cycle_id, result)
        }
        Err(_) => JobOutcome::Failed {
            reason: "budget: the intent cycle exceeded its wall-clock budget".to_owned(),
        },
    };
    cancel.cancel(CancelReason::Budget);
    // Persist the final usage first; a lost lease closes the ledger, which the
    // match below then reports. Usage that cannot be recorded turns even a
    // successful outcome into `Failed` (`PromptTokenLedger::finish`).
    let outcome = ledger.finish(outcome).await;
    // A budget stop surfaces as a blocked/failed proposer; report the budget.
    let outcome = match (outcome, ledger.exceeded()) {
        (JobOutcome::Blocked { .. } | JobOutcome::Failed { .. }, Some(reason)) => {
            JobOutcome::Failed { reason }
        }
        (other, _) => other,
    };
    sanitize_outcome(outcome)
}

/// Lease guard of the cycle store: [`JobStore::hold_live_lease`] renews the
/// presented token's lease (a fresh commit window), verifies under the job's
/// record lock that it is still the current, non-expired lease and keeps
/// that lock until the cycle operation is done. Any error — expired,
/// revoked, replaced, terminal, unreadable or a lock that stays contended
/// beyond the store's retries — refuses the cycle operation (fail closed).
struct JobLeaseLiveness(Arc<JobStore>);

impl LeaseLivenessGuard for JobLeaseLiveness {
    fn hold(&self, token: &LeaseToken) -> Result<Box<dyn LeaseHold>, String> {
        let held = self
            .0
            .hold_live_lease(token)
            .map_err(|error| error.to_string())?;
        Ok(Box::new(HeldJobLease(held)))
    }
}

/// One held job lease; its deadline is the renewed `expires_at`
/// ([`JobLeaseGuard::still_live`]), checked by the cycle store right before
/// it publishes.
struct HeldJobLease(JobLeaseGuard);

impl LeaseHold for HeldJobLease {
    fn still_live(&self) -> Result<(), String> {
        self.0.still_live().map_err(|error| error.to_string())
    }
}

/// The cycle store's lease guard for `job_store` (see [`JobLeaseLiveness`]).
pub(super) fn lease_liveness(job_store: &Arc<JobStore>) -> Arc<dyn LeaseLivenessGuard> {
    Arc::new(JobLeaseLiveness(Arc::clone(job_store)))
}

/// Wall time left for this claim: `max_wall - used`, `None` once nothing is
/// left (never a fresh default); [`DEFAULT_CYCLE_WALL`] only without a budget.
fn remaining_wall(
    budget: &harw_job_runtime::Budget,
    usage: &harw_job_runtime::BudgetUsage,
) -> Option<Duration> {
    match budget.max_wall {
        Some(max_wall) => Duration::try_from(max_wall.saturating_sub(usage.wall))
            .ok()
            .filter(|left| !left.is_zero()),
        None => Some(DEFAULT_CYCLE_WALL),
    }
}

/// Creates the cycle record on the first claim; later claims must find a
/// record of the same job and intent.
fn ensure_record(
    store: &CycleStore,
    input: &IntentCycleJobInput,
    claim: &JobClaim,
    admission: &harw_plan_bridge::CycleAdmission,
    authority: &harw_authority::AuthorityContext,
) -> Result<(), String> {
    match store.load(&input.cycle_id) {
        Ok(existing) => {
            if existing.work_id != claim.job.id
                || existing.checkpoint.intent_id != admission.intent().id
                || existing.checkpoint.intent_digest != admission.intent().digest
            {
                return Err("intent cycle record does not belong to this job".to_owned());
            }
            Ok(())
        }
        Err(CycleStoreError::Store(harw_job_store::StoreError::NotFound { .. })) => {
            let record = CycleRecord::new(
                input.cycle_id.clone(),
                claim.job.id.clone(),
                CycleCheckpoint::initial(admission, authority, 0),
            )
            .map_err(|error| sanitize_failure(&format!("intent cycle record: {error}")))?;
            match store.create(&record) {
                Ok(()) => Ok(()),
                // A concurrent first claim created it; the fence decides.
                Err(CycleStoreError::Store(harw_job_store::StoreError::AlreadyExists {
                    ..
                })) => Ok(()),
                Err(error) => Err(sanitize_failure(&format!("intent cycle record: {error}"))),
            }
        }
        Err(error) => Err(sanitize_failure(&format!("intent cycle record: {error}"))),
    }
}

/// Refills `cache` from the evidence of a stored checkpoint after a lease
/// takeover. `None` when nothing was attempted (fresh cycle at sequence 0) or
/// the record could not be read; the warm-up is an optimisation, never a
/// reason to fail the claim.
async fn warm_after_takeover(
    store: Arc<CycleStore>,
    cycle_id: String,
    cache: ExplorerReadCache,
    authority: harw_authority::AuthorityContext,
) -> Option<usize> {
    let joined = tokio::task::spawn_blocking(move || {
        let record = store.load(&cycle_id)?;
        if record.checkpoint.fence.sequence == 0 {
            return Ok(None);
        }
        let filled = warm_cache(
            &cache,
            &authority,
            &record.checkpoint,
            DEFAULT_MAX_READ_BYTES,
        );
        Ok::<_, CycleStoreError>(Some(filled))
    })
    .await;
    match joined {
        Ok(Ok(filled)) => filled,
        Ok(Err(error)) => {
            tracing::warn!(error = %error, "intent cycle excerpt cache not warmed: record unreadable");
            None
        }
        Err(error) => {
            tracing::warn!(error = %error, "intent cycle excerpt cache warm-up task failed");
            None
        }
    }
}

/// Drains the pending observations of `cycle_id` into project lessons (see
/// `intent_cycle_learning::drain_cycle_observations`) on a blocking thread,
/// under the claim's lease. Does nothing without a learning root and never
/// fails the claim: the result is only logged.
///
/// Called once, after [`CycleDriver::run`] (on every result, the wall-clock
/// timeout included). The store fence only lets the lease read and
/// acknowledge the outbox once the cycle is rebound to it, and that rebind
/// happens inside `CycleDriver::run`; a drain before the run would always be
/// refused (`NotRebound`). The drain after the run also settles entries an
/// earlier incarnation committed but did not acknowledge: after the rebind
/// they are pending under the new fence and are acknowledged under the new
/// token. That includes a cycle the earlier incarnation already ended:
/// `CycleDriver::run` hands a terminal record over to a newer lease before
/// it returns `Terminal`. A run that ended before its rebind leaves the drain
/// refused (`NotRebound`) and the entries pending for the next claim.
/// `store` carries the claim's lease guard: if the job lease expired or was
/// revoked meanwhile, the read and the decision are refused and no lesson is
/// written, whichever way the run ended (cancellation and timeout included).
async fn drain_learning(
    store: &Arc<CycleStore>,
    cycle_id: &str,
    token: &LeaseToken,
    learning_root: Option<&Path>,
) {
    let Some(root) = learning_root else {
        return;
    };
    let (store, id, lease, root) = (
        Arc::clone(store),
        cycle_id.to_owned(),
        token.clone(),
        root.to_path_buf(),
    );
    let joined = tokio::task::spawn_blocking(move || {
        super::intent_cycle_learning::drain_cycle_observations(&store, &id, &lease, &root)
    })
    .await;
    match joined {
        Ok(Ok(report)) => {
            if report.acked > 0 || report.written > 0 {
                tracing::info!(
                    cycle_id,
                    acked = report.acked,
                    written = report.written,
                    screened_out = report.screened_out,
                    pending_materialization = report.pending_materialization,
                    "intent cycle observations drained into project lessons"
                );
            }
            if report.pending_materialization > 0 {
                tracing::warn!(
                    cycle_id,
                    pending_materialization = report.pending_materialization,
                    "intent cycle lessons stay accepted until their facts can be written"
                );
            }
        }
        Ok(Err(reason)) => {
            tracing::warn!(cycle_id, reason = %reason, "intent cycle observations not drained");
        }
        Err(error) => {
            tracing::warn!(cycle_id, error = %error, "intent cycle observation drain task failed");
        }
    }
}

/// Project lesson lines for the proposer prompt (see
/// `intent_cycle_learning::lesson_lines`), read on a blocking thread. Empty
/// without a learning root or when the recall task fails.
async fn recall_lessons(learning_root: Option<&Path>, objective: &str) -> Vec<String> {
    let Some(root) = learning_root.map(Path::to_path_buf) else {
        return Vec::new();
    };
    let objective = objective.to_owned();
    let now_unix = jiff::Timestamp::now().as_second();
    let joined = tokio::task::spawn_blocking(move || {
        super::intent_cycle_learning::lesson_lines(&root, &objective, now_unix)
    })
    .await;
    match joined {
        Ok(lines) => lines,
        Err(error) => {
            tracing::warn!(error = %error, "intent cycle lesson recall task failed");
            Vec::new()
        }
    }
}

/// Warum für den Cycle-Proposer keine Modellroute aufgebaut werden konnte.
///
/// Jede Variante ist ein eigener Verdrahtungsfehler und wird als eigener
/// Blocked-Grund gemeldet (`route_blocked`): ein fehlendes Modell darf nie als
/// "zurückgezogen" erscheinen und umgekehrt.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RouteFailure {
    /// Konfiguration nicht lesbar/zusammensetzbar (Grund sanitisiert).
    Configuration(String),
    /// Weder Explorer- noch Vorgabemodell konfiguriert.
    NoCandidate,
    /// Explorer verworfen (gleicher Provider wie Hauptmodell / anderer
    /// Provider) und kein Vorgabemodell.
    ExplorerUnusable {
        /// Kurze, sanitisierte Begründung (Hauptmodell oder Providerwechsel).
        reason: String,
    },
    /// Jeder konfigurierte Kandidat ist zurückgezogen (Lifecycle).
    AllRetired {
        /// Eine Ablehnung je zurückgezogenem Kandidaten (Slot und Modell).
        refusals: Vec<String>,
    },
}

/// Jeweils genau eine Zeile ohne Geheimnisse: der Text geht wörtlich in den
/// Blocked-Grund des Jobs. Er bleibt kurz, weil `sanitize_failure` Gründe auf
/// `MAX_REASON_BYTES` Zeichen kürzt und kein Modellname dabei abgeschnitten
/// werden soll.
impl std::fmt::Display for RouteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration(reason) => {
                write!(f, "the model configuration could not be assembled: {reason}")
            }
            Self::NoCandidate => f.write_str(
                "no model is configured for the cycle proposer (no Explorer internal model and no default model)",
            ),
            Self::ExplorerUnusable { reason } => write!(
                f,
                "Explorer model unusable and no default model configured ({reason})"
            ),
            Self::AllRetired { refusals } => write!(
                f,
                "no admitted model for the cycle proposer: {}",
                refusals.join("; ")
            ),
        }
    }
}

/// Blocked-Ausgang eines Jobs, dessen Proposer-Route nicht aufgebaut werden
/// konnte: Präfix `intent cycle:` plus der Text der [`RouteFailure`], wie jeder
/// Ausgangsgrund auf die erste Zeile und `MAX_REASON_BYTES` begrenzt.
fn route_blocked(failure: &RouteFailure) -> JobOutcome {
    JobOutcome::Blocked {
        reason: sanitize_failure(&format!("intent cycle: {failure}")),
    }
}

/// Builds the proposer's route policy (see [`routes_from`]) from the
/// configuration of the claim's job runtime (see [`routes_from_assembly`]).
///
/// The submitter only names the principal of the throwaway configuration
/// assembly: an operator id that passes the scope-identifier check, otherwise
/// a fixed name.
fn proposer_routes(
    context: &JobWorkerContext,
    runtime_root: &super::JobRuntimeRoot,
    job_store: &Arc<JobStore>,
    provider: &Arc<dyn ModelProvider>,
    claim: &JobClaim,
) -> Result<RoutePolicy, RouteFailure> {
    let submitter = match claim.scope.submitter() {
        harw_types::ApprovalActor::Operator { id } if super::is_scope_identifier(id) => id.clone(),
        _ => "intent-cycle".to_owned(),
    };
    routes_from_assembly(
        &context.transcript_root,
        runtime_root,
        job_store,
        provider,
        &submitter,
        claim.job.id.as_str(),
    )
}

/// Assembles the job runtime's configuration and derives the route policy
/// from it ([`config_routes`]).
///
/// # Errors
/// [`RouteFailure::Configuration`] if the runtime assembly cannot read or
/// assemble the configuration (the reason is the first line of the assembly
/// error, bounded by `sanitize_failure`; the full error is only logged);
/// otherwise the failures of [`config_routes`].
fn routes_from_assembly(
    transcript_root: &Path,
    runtime_root: &super::JobRuntimeRoot,
    job_store: &Arc<JobStore>,
    provider: &Arc<dyn ModelProvider>,
    submitter: &str,
    work_id: &str,
) -> Result<RoutePolicy, RouteFailure> {
    let assembly = job_assembly(JobAssemblyInputs {
        entry: JobEntry::Prompt,
        home: &runtime_root.home,
        cwd: &runtime_root.cwd,
        principal: job_principal(submitter),
        session_id: SessionId::from_str(format!("intent-cycle-{work_id}-config")),
        state_store: job_state_store(transcript_root),
        job_store: Arc::clone(job_store),
        model: Arc::clone(provider),
        narrowing: None,
    })
    .map_err(|error| {
        tracing::warn!(error = %error, "intent cycle could not read the configuration");
        RouteFailure::Configuration(sanitize_failure(&error))
    })?;
    let config = assembly.config();
    let active = provider
        .pinned_provider_id()
        .or_else(|| config.harness.default_provider.clone());
    // Both candidates run on the job's provider: admit them there like an
    // explicit live selection (as `LiveModelRouting` does), because
    // `ModelCycleProposer` puts the route's model straight into the request.
    let admit = |model: &ModelId| -> RouteAdmission {
        live_model::check_explicit_selection(config, active.as_deref(), model.as_str())
            .map_err(|error| error.to_string())
    };
    config_routes(config, active.as_deref(), &admit)
}

/// Route policy from a resolved configuration: the `Explorer` internal model
/// counts only when it is an explicit choice that runs on the job's provider
/// `active`; the default model is the second candidate.
///
/// # Errors
/// Everything [`routes_from`] reports; a missing candidate
/// ([`RouteFailure::NoCandidate`]) becomes [`RouteFailure::ExplorerUnusable`]
/// when the Explorer model was configured but rejected here (it resolves to
/// the main model, or it runs on another provider than the job).
fn config_routes(
    config: &harw_config::ResolvedConfig,
    active: Option<&str>,
    admit: &dyn Fn(&ModelId) -> RouteAdmission,
) -> Result<RoutePolicy, RouteFailure> {
    let resolved =
        harw_config::resolve_internal_model(config, harw_config::InternalModelPoint::Explorer);
    let same_provider = resolved.provider.is_none() || resolved.provider.as_deref() == active;
    let main_model = resolved.is_main_model();
    // Canonical model ids (catalog key, id or alias → the id that is actually
    // requested), so the escalation compares and requests real offerings.
    let canonical =
        |model: &str| ModelId::from(live_model::resolve_model_id(config, model).as_str());
    // Explorer candidate, or why a configured Explorer model is not used.
    let (explorer, rejection) = if main_model {
        // Without an entry there simply is no Explorer model; an entry
        // without a model forces the main model.
        let forced = config
            .harness
            .internal_models
            .choice(harw_config::InternalModelPoint::Explorer)
            .is_some();
        (None, forced.then(|| "set to the main model".to_owned()))
    } else {
        match resolved.model {
            Some(model) if same_provider => (Some(canonical(model.as_str())), None),
            Some(_) => {
                let explorer_provider = resolved.provider.as_deref().unwrap_or("unknown");
                let reason = match active {
                    Some(active) => {
                        format!(
                            "provider '{explorer_provider}' is not the job's provider '{active}'"
                        )
                    }
                    None => format!(
                        "provider '{explorer_provider}' differs from the job's provider (unknown)"
                    ),
                };
                (None, Some(reason))
            }
            None => (None, None),
        }
    };
    let default = config.harness.default_model.as_deref().map(canonical);
    routes_from(explorer, default, PROPOSAL_MAX_OUTPUT_TOKENS, admit).map_err(|failure| {
        match (failure, rejection) {
            (RouteFailure::NoCandidate, Some(reason)) => RouteFailure::ExplorerUnusable { reason },
            (failure, _) => failure,
        }
    })
}

/// Lifecycle verdict of a route candidate on the job's provider: `Ok(None)`
/// live, `Ok(Some(warning))` deprecated (kept, the warning is logged),
/// `Err(reason)` retired (never routed to).
type RouteAdmission = Result<Option<String>, String>;

/// `model` if `admit` lets it run; a deprecated model is kept with a
/// warning, a retired one is dropped (logged once per claim) and comes back
/// as the refusal that names its slot and model (the lifecycle reason with
/// provider and replacement hint stays in the log: it would not fit into the
/// bounded job reason).
fn admitted(
    model: ModelId,
    slot: &'static str,
    admit: &dyn Fn(&ModelId) -> RouteAdmission,
) -> Result<ModelId, String> {
    match admit(&model) {
        Ok(None) => Ok(model),
        Ok(Some(warning)) => {
            tracing::warn!(slot, model = %model.as_str(), warning = %warning, "intent cycle proposer route is deprecated");
            Ok(model)
        }
        Err(reason) => {
            tracing::warn!(slot, model = %model.as_str(), reason = %reason, "intent cycle proposer route is retired and not used");
            Err(format!("{slot} model {} is retired", model.as_str()))
        }
    }
}

/// [`admitted`] for an optional candidate; the refusal of a retired one is
/// appended to `refusals`.
fn keep_admitted(
    model: Option<ModelId>,
    slot: &'static str,
    admit: &dyn Fn(&ModelId) -> RouteAdmission,
    refusals: &mut Vec<String>,
) -> Option<ModelId> {
    match admitted(model?, slot, admit) {
        Ok(model) => Some(model),
        Err(refusal) => {
            refusals.push(refusal);
            None
        }
    }
}

/// Route policy from the two candidate models, each admitted by `admit`
/// first (lifecycle on the job's provider; a retired candidate counts as
/// absent). The default slot is the explorer model, falling back to the
/// default model. The escalation slot is the default model, and only when it
/// differs from the explorer model (an escalation to the same model would
/// change nothing). A retired default therefore never becomes the
/// escalation; a retired explorer leaves the default as the only route.
/// `effort` stays `None`; the provider chooses.
///
/// # Errors
/// No admitted model at all blocks the job: [`RouteFailure::AllRetired`] with
/// one refusal per retired candidate, or [`RouteFailure::NoCandidate`] when
/// there was no candidate to begin with.
fn routes_from(
    explorer: Option<ModelId>,
    default: Option<ModelId>,
    max_output_tokens: u32,
    admit: &dyn Fn(&ModelId) -> RouteAdmission,
) -> Result<RoutePolicy, RouteFailure> {
    let mut refusals = Vec::new();
    let explorer = keep_admitted(explorer, "explorer", admit, &mut refusals);
    let default = keep_admitted(default, "default", admit, &mut refusals);
    let route = |model: ModelId| ProposerRoute {
        model,
        effort: None,
        max_output_tokens,
    };
    let escalation = match (&explorer, &default) {
        (Some(first), Some(second)) if first != second => Some(second.clone()),
        _ => None,
    };
    let Some(primary) = explorer.or(default) else {
        return Err(if refusals.is_empty() {
            RouteFailure::NoCandidate
        } else {
            RouteFailure::AllRetired { refusals }
        });
    };
    let policy = RoutePolicy::new(route(primary));
    Ok(match escalation {
        Some(model) => policy.with_escalation(
            route(model),
            ESCALATE_AFTER_REFUSALS,
            ESCALATE_AT_STALL_TRANSITIONS,
        ),
        None => policy,
    })
}

/// Failure reasons go back to MCP clients verbatim; keep them bounded.
fn sanitize_outcome(outcome: JobOutcome) -> JobOutcome {
    match outcome {
        JobOutcome::Failed { reason } => JobOutcome::Failed {
            reason: sanitize_failure(&reason),
        },
        JobOutcome::Blocked { reason } => JobOutcome::Blocked {
            reason: sanitize_failure(&reason),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_model_catalog::lifecycle;

    #[test]
    fn exhausted_wall_budget_is_never_refreshed() {
        use harw_job_runtime::{Budget, BudgetUsage};
        use jiff::SignedDuration;
        let budget = Budget {
            max_tokens: None,
            max_wall: Some(SignedDuration::from_secs(60)),
            max_tool_calls: None,
        };
        let mut usage = BudgetUsage::default();
        assert_eq!(
            remaining_wall(&budget, &usage),
            Some(Duration::from_secs(60))
        );
        usage.wall = SignedDuration::from_secs(25);
        assert_eq!(
            remaining_wall(&budget, &usage),
            Some(Duration::from_secs(35)),
            "a persisted usage.wall is honoured on reclaim"
        );
        usage.wall = SignedDuration::from_secs(60);
        assert_eq!(remaining_wall(&budget, &usage), None);
        usage.wall = SignedDuration::from_secs(3_600);
        assert_eq!(
            remaining_wall(&budget, &usage),
            None,
            "overdrawn stays exhausted"
        );
        let unbounded = Budget {
            max_wall: None,
            ..budget
        };
        assert_eq!(remaining_wall(&unbounded, &usage), Some(DEFAULT_CYCLE_WALL));
    }

    #[test]
    fn only_the_intent_cycle_kind_matches() {
        assert!(is_intent_cycle_kind(&JobKind::Custom(
            INTENT_CYCLE_JOB_KIND.to_owned()
        )));
        assert!(super::super::is_supported_kind(&JobKind::Custom(
            INTENT_CYCLE_JOB_KIND.to_owned()
        )));
        assert!(!is_intent_cycle_kind(&JobKind::Worker));
        assert!(!is_intent_cycle_kind(&JobKind::Custom(
            "work_driver".to_owned()
        )));
        assert_eq!(
            INTENT_CYCLE_JOB_KIND,
            harw_runtime::job_ledger::INTENT_CYCLE_JOB_KIND
        );
    }

    #[test]
    fn routes_escalate_to_the_default_model_when_it_differs() {
        use harw_plan_bridge::RouteSlot;
        fn model(name: &str) -> ModelId {
            ModelId::from(name)
        }
        let route = |name: &str| ProposerRoute {
            model: model(name),
            effort: None,
            max_output_tokens: 128,
        };
        let live = |_: &ModelId| -> RouteAdmission { Ok(None) };

        // Explorer a, default b: a runs by default, b takes over on escalation.
        let both = routes_from(Some(model("a")), Some(model("b")), 128, &live);
        assert_eq!(
            both,
            Ok(RoutePolicy::new(route("a")).with_escalation(
                route("b"),
                ESCALATE_AFTER_REFUSALS,
                ESCALATE_AT_STALL_TRANSITIONS
            ))
        );
        let Ok(both) = both else {
            return;
        };
        assert_eq!(both.select(0, 0), RouteSlot::Default);
        assert_eq!(both.select(0, 2), RouteSlot::Escalation);
        assert_eq!(both.select(2, 0), RouteSlot::Escalation);
        assert_eq!(
            both.route(RouteSlot::Escalation).map(|r| r.model.clone()),
            Some(model("b"))
        );

        // Same model twice: one route, nothing to escalate to.
        let same = routes_from(Some(model("a")), Some(model("a")), 128, &live);
        assert_eq!(same, Ok(RoutePolicy::new(route("a"))));
        assert!(same.is_ok_and(|policy| policy.route(RouteSlot::Escalation).is_none()));

        // No explorer model: the default model is the only route.
        assert_eq!(
            routes_from(None, Some(model("b")), 128, &live),
            Ok(RoutePolicy::new(route("b")))
        );
        // Explorer model without a default: one route as well.
        assert_eq!(
            routes_from(Some(model("a")), None, 128, &live),
            Ok(RoutePolicy::new(route("a")))
        );
        // No model at all blocks the job, and it says that nothing is configured.
        assert_eq!(
            routes_from(None, None, 128, &live),
            Err(RouteFailure::NoCandidate)
        );
    }

    /// The bundled Mistral lifecycle table at the injected instant `unix`.
    fn mistral_at(unix: i64) -> TestResult<impl Fn(&ModelId) -> RouteAdmission> {
        let now = lifecycle::OffsetDateTime::from_unix_timestamp(unix)
            .map_err(ctx("valid test instant"))?;
        Ok(move |model: &ModelId| {
            lifecycle::enforce_explicit("mistral", model.as_str(), now)
                .map_err(|error| error.to_string())
        })
    }

    /// 2026-08-01T00:00Z, before `mistral/labs-leanstral-1-5` retires.
    const BEFORE_RETIREMENT: i64 = 1_785_542_400;

    /// 2026-10-15T00:00Z, after it retired (2026-09-30).
    const AFTER_RETIREMENT: i64 = 1_792_022_400;

    fn route(name: &str) -> ProposerRoute {
        ProposerRoute {
            model: ModelId::from(name),
            effort: None,
            max_output_tokens: 128,
        }
    }

    #[test]
    fn retired_default_is_never_selected_for_escalation() -> TestResult {
        use harw_plan_bridge::RouteSlot;
        let explorer = || Some(ModelId::from("mistral-small-latest"));
        let retired = || Some(ModelId::from("labs-leanstral-1-5"));

        // Control: before the cutoff the default is a live escalation route.
        let before = routes_from(explorer(), retired(), 128, &mistral_at(BEFORE_RETIREMENT)?);
        assert_eq!(
            before,
            Ok(
                RoutePolicy::new(route("mistral-small-latest")).with_escalation(
                    route("labs-leanstral-1-5"),
                    ESCALATE_AFTER_REFUSALS,
                    ESCALATE_AT_STALL_TRANSITIONS
                )
            )
        );
        // After it: the explorer runs alone, nothing to escalate to.
        let after = routes_from(explorer(), retired(), 128, &mistral_at(AFTER_RETIREMENT)?)
            .map_err(ctx("the live explorer keeps the job running"))?;
        assert_eq!(after, RoutePolicy::new(route("mistral-small-latest")));
        assert!(after.route(RouteSlot::Escalation).is_none());
        assert_eq!(after.select(9, 9), RouteSlot::Default);
        // A deprecated default stays usable (with a warning).
        let deprecated = |_: &ModelId| -> RouteAdmission { Ok(Some("deprecated".to_owned())) };
        assert!(
            routes_from(explorer(), retired(), 128, &deprecated)
                .is_ok_and(|policy| policy.route(RouteSlot::Escalation).is_some())
        );
        Ok(())
    }

    #[test]
    fn retired_explorer_falls_back_to_a_live_default() -> TestResult {
        let after = mistral_at(AFTER_RETIREMENT)?;
        assert_eq!(
            routes_from(
                Some(ModelId::from("labs-leanstral-1-5")),
                Some(ModelId::from("mistral-large-latest")),
                128,
                &after
            ),
            Ok(RoutePolicy::new(route("mistral-large-latest")))
        );
        // Both retired: no route at all, the job is blocked (fail closed) and
        // the failure names both retired candidates.
        let both_retired = routes_from(
            Some(ModelId::from("labs-leanstral-1-5")),
            Some(ModelId::from("mistral-ocr-4-0")),
            128,
            &after,
        );
        let refusals = match both_retired {
            Err(RouteFailure::AllRetired { refusals }) => refusals,
            other => {
                return Err(TestError::Unexpected(format!(
                    "both retired must give AllRetired, got {other:?}"
                )));
            }
        };
        assert_eq!(refusals.len(), 2, "one refusal per retired candidate");
        assert!(
            refusals
                .iter()
                .any(|refusal| refusal.contains("explorer")
                    && refusal.contains("labs-leanstral-1-5"))
        );
        assert!(
            refusals
                .iter()
                .any(|refusal| refusal.contains("default") && refusal.contains("mistral-ocr-4-0"))
        );
        // A single retired candidate is reported as that candidate alone, not
        // as a missing configuration.
        assert_eq!(
            routes_from(None, Some(ModelId::from("mistral-ocr-4-0")), 128, &after),
            Err(RouteFailure::AllRetired {
                refusals: vec!["default model mistral-ocr-4-0 is retired".to_owned()]
            })
        );
        Ok(())
    }

    /// Provider, der nie angefragt wird: die Routenwahl liest nur die
    /// Konfiguration.
    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        fn respond<'a>(&'a self, _request: harw_core::ModelRequest) -> harw_core::ModelFuture<'a> {
            Box::pin(async {
                Err::<harw_core::ModelResponse, _>(harw_core::ModelError::RequestFailed(
                    "unused".to_owned(),
                ))
            })
        }
    }

    /// Der Fehler eines Routenergebnisses; eine Route ist hier ein Testfehler.
    fn failure_of(result: Result<RoutePolicy, RouteFailure>) -> TestResult<RouteFailure> {
        match result {
            Err(failure) => Ok(failure),
            Ok(policy) => Err(TestError::Unexpected(format!(
                "expected a route failure, got {policy:?}"
            ))),
        }
    }

    /// Konfiguration mit Provider `mistral` und einer Explorer-Wahl.
    fn explorer_config(provider: Option<&str>, model: Option<&str>) -> harw_config::ResolvedConfig {
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("mistral".to_owned());
        config.harness.internal_models.set_choice(
            harw_config::InternalModelPoint::Explorer,
            Some(harw_config::InternalModelChoice {
                provider: provider.map(str::to_owned),
                model: model.map(str::to_owned),
            }),
        );
        config
    }

    /// Modell der Route in `slot`.
    fn model_in(policy: &RoutePolicy, slot: harw_plan_bridge::RouteSlot) -> Option<ModelId> {
        policy.route(slot).map(|route| route.model.clone())
    }

    #[test]
    fn config_assembly_failure_is_reported_as_configuration() -> TestResult {
        const CANARY: &str = "sk-canary-4711";
        let temp = tempfile::tempdir().map_err(ctx("temp dir"))?;
        let home = temp.path().join("home");
        let cwd = temp.path().join("cwd");
        harw_home::ensure_home(&home).map_err(ctx("scaffold home"))?;
        std::fs::create_dir_all(&cwd).map_err(ctx("create cwd"))?;
        // Invalid TOML (garbage after the value) whose source line carries a
        // secret-looking value: the assembly cannot read the configuration.
        std::fs::write(
            home.join("config.toml"),
            format!("api_key = \"{CANARY}\" garbage\n"),
        )
        .map_err(ctx("write config"))?;
        let job_store = Arc::new(JobStore::new(&temp.path().join("store")));
        let provider: Arc<dyn ModelProvider> = Arc::new(UnusedProvider);

        let result = routes_from_assembly(
            temp.path(),
            &super::super::JobRuntimeRoot { home, cwd },
            &job_store,
            &provider,
            "operator",
            "config-broken",
        );
        let failure = failure_of(result)?;
        let RouteFailure::Configuration(reason) = &failure else {
            return Err(TestError::Unexpected(format!(
                "a broken configuration must be a Configuration failure, got {failure:?}"
            )));
        };
        // The precise (first-line) cause is exposed, not a retirement claim,
        // and the offending source line with its value is not.
        assert!(reason.contains("TOML"), "cause is named: {reason}");
        assert!(!reason.contains('\n'), "one line: {reason}");
        assert!(!reason.contains(CANARY), "no config content: {reason}");
        let JobOutcome::Blocked { reason: blocked } = route_blocked(&failure) else {
            return Err(TestError::Missing("a blocked outcome"));
        };
        assert!(blocked.starts_with("intent cycle: "), "{blocked}");
        assert!(blocked.contains("configuration"), "{blocked}");
        assert!(!blocked.contains("retired"), "{blocked}");
        assert!(!blocked.contains(CANARY), "{blocked}");
        Ok(())
    }

    #[test]
    fn cross_provider_explorer_without_default_is_reported_as_explorer_unusable() -> TestResult {
        use harw_plan_bridge::RouteSlot;
        let live = |_: &ModelId| -> RouteAdmission { Ok(None) };
        // The only candidate is an Explorer model on another provider than
        // the job's, and no default model exists.
        let config = explorer_config(Some("openrouter"), Some("nemotron-test"));
        let failure = failure_of(config_routes(&config, Some("mistral"), &live))?;
        let RouteFailure::ExplorerUnusable { reason } = &failure else {
            return Err(TestError::Unexpected(format!(
                "a cross-provider Explorer must be ExplorerUnusable, got {failure:?}"
            )));
        };
        assert!(reason.contains("openrouter"), "{reason}");
        assert!(reason.contains("mistral"), "{reason}");
        let text = failure.to_string();
        assert!(!text.contains("retired"), "{text}");
        assert!(!text.contains('\n'), "{text}");

        // An Explorer entry that forces the main model is unusable as well.
        let forced = explorer_config(None, None);
        let forced_failure = failure_of(config_routes(&forced, Some("mistral"), &live))?;
        assert!(
            matches!(&forced_failure, RouteFailure::ExplorerUnusable { reason } if reason.contains("main model")),
            "{forced_failure:?}"
        );

        // A default model on the job's provider carries the job despite the
        // rejected Explorer: there is nothing to report then.
        let mut with_default = config.clone();
        with_default.harness.default_model = Some("mistral-large-latest".to_owned());
        let policy = config_routes(&with_default, Some("mistral"), &live)
            .map_err(ctx("the default model keeps the job running"))?;
        assert_eq!(
            model_in(&policy, RouteSlot::Default),
            Some(ModelId::from("mistral-large-latest"))
        );
        assert!(policy.route(RouteSlot::Escalation).is_none());

        // Control: an Explorer model on the job's provider is the default
        // route, the default model the escalation.
        let mut same_provider = explorer_config(Some("mistral"), Some("mistral-small-latest"));
        same_provider.harness.default_model = Some("mistral-large-latest".to_owned());
        let policy = config_routes(&same_provider, Some("mistral"), &live)
            .map_err(ctx("an Explorer model on the job's provider is routable"))?;
        assert_eq!(
            model_in(&policy, RouteSlot::Default),
            Some(ModelId::from("mistral-small-latest"))
        );
        assert_eq!(
            model_in(&policy, RouteSlot::Escalation),
            Some(ModelId::from("mistral-large-latest"))
        );
        Ok(())
    }

    #[test]
    fn missing_and_retired_models_are_told_apart() -> TestResult {
        let live = |_: &ModelId| -> RouteAdmission { Ok(None) };
        // Nothing configured at all.
        assert_eq!(
            config_routes(
                &harw_config::ResolvedConfig::default(),
                Some("mistral"),
                &live
            ),
            Err(RouteFailure::NoCandidate)
        );
        // Configured, but retired: a lifecycle refusal naming the model, not
        // a missing configuration.
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_model = Some("mistral-large-latest".to_owned());
        let retired = |_: &ModelId| -> RouteAdmission { Err("retired".to_owned()) };
        assert_eq!(
            config_routes(&config, Some("mistral"), &retired),
            Err(RouteFailure::AllRetired {
                refusals: vec!["default model mistral-large-latest is retired".to_owned()]
            })
        );
        Ok(())
    }

    #[test]
    fn route_failures_are_blocked_with_a_distinct_one_line_reason() -> TestResult {
        let failures = [
            RouteFailure::Configuration("runtime config error: failed to parse TOML".to_owned()),
            RouteFailure::NoCandidate,
            RouteFailure::ExplorerUnusable {
                reason: "provider 'openrouter' is not the job's provider 'mistral'".to_owned(),
            },
            RouteFailure::AllRetired {
                refusals: vec![
                    "explorer model labs-leanstral-1-5 is retired".to_owned(),
                    "default model mistral-ocr-4-0 is retired".to_owned(),
                ],
            },
        ];
        let mut reasons = Vec::new();
        for failure in &failures {
            let JobOutcome::Blocked { reason } = route_blocked(failure) else {
                return Err(TestError::Missing("a blocked outcome"));
            };
            assert!(reason.starts_with("intent cycle: "), "{reason}");
            assert!(!reason.contains('\n'), "{reason}");
            assert!(
                reason.chars().count() <= super::super::MAX_REASON_BYTES,
                "within the reason bound, nothing cut: {reason}"
            );
            reasons.push(reason);
        }
        // Only the lifecycle failure talks about retirement, and it keeps both
        // models within the bound.
        let [configuration, no_candidate, unusable, retired] = reasons.as_slice() else {
            return Err(TestError::Missing("one reason per failure"));
        };
        for reason in [configuration, no_candidate, unusable] {
            assert!(!reason.contains("retired"), "{reason}");
        }
        assert!(retired.contains("labs-leanstral-1-5"), "{retired}");
        assert!(retired.contains("mistral-ocr-4-0"), "{retired}");
        for (index, reason) in reasons.iter().enumerate() {
            assert_eq!(
                reasons.iter().filter(|other| *other == reason).count(),
                1,
                "reason {index} is distinct"
            );
        }
        Ok(())
    }

    #[test]
    fn outcomes_are_sanitized_and_successes_pass_through() {
        let long = "x".repeat(10_000);
        let JobOutcome::Failed { reason } = sanitize_outcome(JobOutcome::Failed { reason: long })
        else {
            return;
        };
        assert!(reason.len() < 1_000);
        let ok = JobOutcome::Succeeded {
            result: serde_json::json!({"terminal": "completion_proposed"}),
        };
        assert_eq!(sanitize_outcome(ok.clone()), ok);
    }
}
