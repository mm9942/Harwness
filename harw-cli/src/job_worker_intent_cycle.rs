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
//! resumes the fenced checkpoint. Everything else maps through
//! `harw_plan_bridge::job_outcome`: `CompletionProposed` is a succeeded job
//! that still needs owner acceptance.
//!
//! # Workspace binding
//! The ceiling is cut from the worker's `runtime_root.cwd`. `claim.scope`
//! carries only the submitter's tenant/workspace *aliases* (the worker
//! sandbox always uses tenant `job`, workspace `project`), so the ids are not
//! comparable. `intent_cycle.start` therefore records the caller's canonical
//! workspace root in `IntentCycleJobInput::workspace_root`, and the worker
//! refuses (`Blocked`, before any record or model call) unless it equals the
//! canonical root of its own sandbox; a missing root is refused too.

use std::sync::Arc;
use std::time::Duration;

use harw_core::ModelProvider;
use harw_core::cancel::{CancelReason, CancelToken};
use harw_job_runtime::{JobClaim, JobKind, JobOutcome};
use harw_ops::intent_cycle::IntentCycleJobInput;
use harw_plan::PlanNodeKind;
use harw_plan_bridge::{
    CeilingReissuer, CycleCheckpoint, CycleDriver, CycleRecord, CycleRunError, CycleStore,
    CycleStoreError, ExplorerCycleExecutor, INTENT_CYCLE_JOB_KIND, ModelCycleProposer,
    ProposerRoute, RoutePolicy, job_outcome,
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
    claim: JobClaim,
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

    let Some(model) = proposer_model(context, runtime_root, &job_store, &provider, &claim) else {
        return JobOutcome::Blocked {
            reason: "intent cycle: no model is configured for the cycle proposer".to_owned(),
        };
    };

    let store = match CycleStore::open_in_job_root(job_store.root()) {
        Ok(store) => Arc::new(store),
        Err(error) => {
            return JobOutcome::Failed {
                reason: sanitize_failure(&format!("intent-cycle store unavailable: {error}")),
            };
        }
    };
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

    let ledger = PromptTokenLedger::new(claim.job.budget.clone(), claim.job.usage.clone());
    let budgeted: Arc<dyn ModelProvider> = Arc::new(BudgetedModelProvider {
        inner: Arc::clone(&provider),
        ledger: Arc::clone(&ledger),
    });
    let proposer = ModelCycleProposer::new(
        budgeted,
        RoutePolicy::new(ProposerRoute {
            model,
            effort: None,
            max_output_tokens: PROPOSAL_MAX_OUTPUT_TOKENS,
        }),
    );
    let executor = ExplorerCycleExecutor::new(authority.clone(), plan.targets, plan.checks);
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
    let outcome = match tokio::time::timeout(remaining, run).await {
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
    // A budget stop surfaces as a blocked/failed proposer; report the budget.
    let outcome = match (outcome, ledger.exceeded()) {
        (JobOutcome::Blocked { .. } | JobOutcome::Failed { .. }, Some(reason)) => {
            JobOutcome::Failed { reason }
        }
        (other, _) => other,
    };
    sanitize_outcome(outcome)
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

/// The proposer's model: the configured `Explorer` internal model when it
/// runs on the job's provider, else the default model.
fn proposer_model(
    context: &JobWorkerContext,
    runtime_root: &super::JobRuntimeRoot,
    job_store: &Arc<JobStore>,
    provider: &Arc<dyn ModelProvider>,
    claim: &JobClaim,
) -> Option<ModelId> {
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
    let model = match resolved.model {
        Some(model) if !main_model && same_provider => Some(model),
        _ => config.harness.default_model.clone(),
    }?;
    Some(ModelId::from(model.as_str()))
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
