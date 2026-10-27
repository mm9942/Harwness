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
//! made from trusted counters only; no model output influences it. Without any
//! configured model the job is blocked. No reasoning effort is configured
//! here (the internal-model settings carry none), so the provider default
//! applies.
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
//! The cycle store is opened with a lease liveness check
//! ([`CycleStore::with_lease_liveness`]) backed by
//! [`JobStore::verify_lease`]: every fenced read and write — the driver's
//! rebind, journal and commits as well as the drain's read and
//! acknowledgement — first proves that the claim's token is still the
//! current, non-expired job lease (read-only; nothing is renewed). The epoch
//! fence alone would still admit an expired lease until a reclaim rebinds
//! the cycle. So once the job lease has expired or been revoked (lost
//! heartbeat, operator cancellation), the driver's next write fails closed
//! and the drain learns nothing — whichever way the run ended, cancellation
//! and wall-clock timeout included; the observations stay pending for the
//! claim that takes over.
//! The drain acknowledges before it writes any fact, so the lease-fenced
//! acknowledgement is the commit of every lesson (see
//! `intent_cycle_learning::drain_cycle_observations`).
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
    ExplorerReadCache, INTENT_CYCLE_JOB_KIND, LeaseLivenessCheck, ModelCycleProposer,
    ProposerRoute, RoutePolicy, job_outcome, warm_cache,
};
use harw_session_store::JobStore;
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

    let Some(routes) = proposer_routes(context, runtime_root, &job_store, &provider, &claim) else {
        return JobOutcome::Blocked {
            reason: "intent cycle: no model is configured for the cycle proposer".to_owned(),
        };
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

/// Liveness check of the cycle store: the presented token must still be the
/// current, non-expired lease of its job ([`JobStore::verify_lease`], which
/// reads the job record and writes nothing). Any error — expired, revoked,
/// replaced, terminal or unreadable — refuses the cycle operation (fail
/// closed).
pub(super) fn lease_liveness(job_store: &Arc<JobStore>) -> LeaseLivenessCheck {
    let job_store = Arc::clone(job_store);
    Arc::new(move |token: &LeaseToken| {
        job_store
            .verify_lease(token)
            .map_err(|error| error.to_string())
    })
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
/// `store` carries the claim's liveness check: if the job lease expired or
/// was revoked meanwhile, the read and the acknowledgement are refused and no
/// lesson is written, whichever way the run ended (cancellation and timeout
/// included).
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
                    "intent cycle observations drained into project lessons"
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

/// Builds the proposer's route policy (see [`routes_from`]) from the
/// configuration: the `Explorer` internal model counts only when it is an
/// explicit choice that runs on the job's provider.
fn proposer_routes(
    context: &JobWorkerContext,
    runtime_root: &super::JobRuntimeRoot,
    job_store: &Arc<JobStore>,
    provider: &Arc<dyn ModelProvider>,
    claim: &JobClaim,
) -> Option<RoutePolicy> {
    let submitter = match claim.scope.submitter() {
        harw_types::ApprovalActor::Operator { id } if super::is_scope_identifier(id) => id.clone(),
        _ => "intent-cycle".to_owned(),
    };
    let assembly = job_assembly(JobAssemblyInputs {
        entry: JobEntry::Prompt,
        home: &runtime_root.home,
        cwd: &runtime_root.cwd,
        principal: job_principal(&submitter),
        session_id: SessionId::from_str(format!("intent-cycle-{}-config", claim.job.id.as_str())),
        state_store: job_state_store(&context.transcript_root),
        job_store: Arc::clone(job_store),
        model: Arc::clone(provider),
        narrowing: None,
    })
    .map_err(
        |error| tracing::warn!(error = %error, "intent cycle could not read the configuration"),
    )
    .ok()?;
    let config = assembly.config();
    let active = provider
        .pinned_provider_id()
        .or_else(|| config.harness.default_provider.clone());
    let resolved =
        harw_config::resolve_internal_model(config, harw_config::InternalModelPoint::Explorer);
    let same_provider = resolved.provider.is_none() || resolved.provider == active;
    let main_model = resolved.is_main_model();
    let explorer = match resolved.model {
        Some(model) if !main_model && same_provider => Some(ModelId::from(model.as_str())),
        _ => None,
    };
    let default = config.harness.default_model.as_deref().map(ModelId::from);
    routes_from(explorer, default, PROPOSAL_MAX_OUTPUT_TOKENS)
}

/// Route policy from the two candidate models. The default slot is the
/// explorer model, falling back to the default model. The escalation slot is
/// the default model, and only when it differs from the explorer model (an
/// escalation to the same model would change nothing). No models at all gives
/// `None`: the job is blocked. `effort` stays `None`; the provider chooses.
fn routes_from(
    explorer: Option<ModelId>,
    default: Option<ModelId>,
    max_output_tokens: u32,
) -> Option<RoutePolicy> {
    let route = |model: ModelId| ProposerRoute {
        model,
        effort: None,
        max_output_tokens,
    };
    let escalation = match (&explorer, &default) {
        (Some(first), Some(second)) if first != second => Some(second.clone()),
        _ => None,
    };
    let policy = RoutePolicy::new(route(explorer.or(default)?));
    Some(match escalation {
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

        // Explorer a, default b: a runs by default, b takes over on escalation.
        let both = routes_from(Some(model("a")), Some(model("b")), 128);
        assert_eq!(
            both,
            Some(RoutePolicy::new(route("a")).with_escalation(
                route("b"),
                ESCALATE_AFTER_REFUSALS,
                ESCALATE_AT_STALL_TRANSITIONS
            ))
        );
        let Some(both) = both else {
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
        let same = routes_from(Some(model("a")), Some(model("a")), 128);
        assert_eq!(same, Some(RoutePolicy::new(route("a"))));
        assert!(same.is_some_and(|policy| policy.route(RouteSlot::Escalation).is_none()));

        // No explorer model: the default model is the only route.
        assert_eq!(
            routes_from(None, Some(model("b")), 128),
            Some(RoutePolicy::new(route("b")))
        );
        // Explorer model without a default: one route as well.
        assert_eq!(
            routes_from(Some(model("a")), None, 128),
            Some(RoutePolicy::new(route("a")))
        );
        // No model at all blocks the job.
        assert_eq!(routes_from(None, None, 128), None);
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
