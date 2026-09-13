//! Durable job consumer for `harw serve`.
//!
//! Jobs arrive here with their authority already resolved by admission.  Two
//! job families are executed, and they differ in exactly one thing: how much
//! authority the turn is allowed to carry.
//!
//! * [`JobKind::Worker`] / [`JobKind::Dream`] — a trusted `prompt`/`task`
//!   string is turned into one durable assistant turn with *no* extensions,
//!   tools, or ambient authority at all.
//! * [`JobKind::Custom`] named [`PLAN_NODE_JOB_KIND`] — a plan node admitted by
//!   `harw_plan_bridge::PlanJobBridge::admit_ready_nodes`.  The turn gets a
//!   tool registry chosen by the node's kind and a sandbox derived by
//!   *reduction* from the mutation contract of that node.  Success and failure
//!   both flow back into the plan, so a node can never be left `InProgress`
//!   forever.
//!
//! # Approvals
//! A durable job runs unattended.  There is no user who could answer an
//! `AskUser` guardrail, so a paused turn is never awaited: for a plan-node job
//! it terminates the job as failed (see [`PauseDisposition`]), and the
//! session's [`SpawnContext::approval_actor`] is deliberately `None` so no
//! identity is even eligible to answer.
//!
//! # Concurrency
//! Every entry point is `async` and `Send`.  All shared state travels as
//! `Arc<…>`; the worker itself holds no global state — the plan store, the
//! inherited sandbox, and the plan actor are injected through
//! [`PlanNodeServices`].

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use harw_agent_dsl::roles::AgentRoleId;
use harw_core::{
    AgentSession, DurableJobRunner, ExecutionControl, JobExecutionRegistry, ModelMessage,
    ModelProvider, SpawnContext, StateStore, TranscriptStateStore, TurnInput, TurnOutcome,
    run_turn,
};
use harw_extension_api::{ExtensionRegistry, empty_extension_registry};
use harw_job_runtime::{JobClaim, JobKind, JobOutcome, JobState};
use harw_observe::TraceContext;
use harw_plan::admission::{MutationContract, PathRule};
use harw_plan::{Criterion, PlanNodeKind, PlanStore, TaskId, VerificationStep};
use harw_plan_bridge::{PlanJobBridge, offset_from_timestamp};
use harw_registry_defaults::profile::{IdentityOverrides, RegistryProfile, assemble_registry};
use harw_sandbox::{Permission, PermissionSet, SandboxSpec};
use harw_session_store::{ClaimRequest, JobListQuery, JobStore, TranscriptStore};
use harw_types::{AgentRole, SessionId, ThreadRef};
use jiff::{SignedDuration, Timestamp};
use tokio::sync::watch;

const WORKER_ID: &str = "harw-serve-job-worker";
const LEASE_TTL_SECONDS: i64 = 120;
const MAX_REASON_BYTES: usize = 160;

/// The [`JobKind::Custom`] discriminator carried by every job that was created
/// from a plan node.
///
/// # Description
/// This is the single source of truth for the string that
/// `harw_plan_bridge::PlanJobBridge::admit_ready_nodes` writes into
/// [`JobKind::Custom`] and that this worker matches on.  Both sides must agree
/// literally; the constant exists so the literal is written exactly once here.
///
/// # Concurrency
/// A `&'static str`; freely shareable.
pub const PLAN_NODE_JOB_KIND: &str = "plan-node";

/// Reason recorded when a plan-node job arrives without a configured plan store.
const MISSING_PLAN_SERVICES: &str =
    "plan-node job cannot run: this worker was started without a plan store";

/// The services a `plan-node` job needs on top of the plain job pipeline.
///
/// # Description
/// A plan-node job is only executable if three things are available that a
/// plain worker job does not need: the authoritative plan store (to read the
/// node's kind and to report the outcome back), the sandbox the job inherited
/// from its composition root (the *ceiling* that the node's mutation contract
/// may only narrow), and the actor name under which the plan mutations are
/// recorded.
///
/// They are passed explicitly instead of being reachable through global state:
/// a worker that could reach a plan store it was not given would be a second,
/// unaudited path into the plan.
///
/// # Concurrency
/// `Send + Sync`.  [`PlanStore`] requires `Send + Sync` itself, and
/// [`SandboxSpec`] is immutable data.  Share it as `Arc<PlanNodeServices>`; the
/// worker only ever clones the pointer.
pub struct PlanNodeServices {
    /// The authoritative plan store of the session that admitted the jobs.
    plan: Arc<dyn PlanStore>,
    /// The inherited sandbox; a node contract may only reduce this.
    sandbox: SandboxSpec,
    /// Actor recorded on every plan mutation this worker performs.
    actor: String,
}

impl PlanNodeServices {
    /// Bundles the plan-side services a plan-node job needs.
    ///
    /// # Arguments
    /// - `plan` (`Arc<dyn PlanStore>`): the authoritative plan store; only the
    ///   pointer is shared, never the store's contents.
    /// - `sandbox` (`SandboxSpec`): the authority ceiling every plan node runs
    ///   under.  Ownership is transferred; it is never widened afterwards.
    /// - `actor` (`String`): the actor recorded on plan mutations.  This is a
    ///   label, not a trust identity — the trust identity lives in the job's
    ///   [`harw_job_runtime::JobScope`].
    ///
    /// # Returns
    /// The bundled services, ready to be wrapped in an `Arc`.
    ///
    /// # Concurrency
    /// Pure construction; safe from any thread.
    #[must_use]
    pub fn new(plan: Arc<dyn PlanStore>, sandbox: SandboxSpec, actor: String) -> Self {
        Self {
            plan,
            sandbox,
            actor,
        }
    }

    /// Borrows the authoritative plan store.
    ///
    /// # Returns
    /// The plan store as a trait object reference.
    #[must_use]
    pub fn plan(&self) -> &dyn PlanStore {
        self.plan.as_ref()
    }

    /// Borrows the inherited sandbox that node contracts may only narrow.
    ///
    /// # Returns
    /// The frozen [`SandboxSpec`] of the job pipeline.
    #[must_use]
    pub fn sandbox(&self) -> &SandboxSpec {
        &self.sandbox
    }

    /// Borrows the actor name recorded on plan mutations.
    ///
    /// # Returns
    /// The actor label as a string slice.
    #[must_use]
    pub fn actor(&self) -> &str {
        &self.actor
    }
}

/// Runs one deterministic poll of Ready durable jobs.
///
/// # Description
/// The list is ordered by work ID by [`JobStore`].  Each candidate is still
/// claimed atomically, so another process claiming it between list and claim
/// cannot execute it here.  Unsupported job kinds are skipped; a `plan-node`
/// job is *never* skipped, even when `plan_services` is `None` — it then
/// terminates as [`JobOutcome::Blocked`] with a visible reason, because a
/// silently skipped plan job leaves its node `InProgress` forever.
///
/// # Arguments
/// - `store` (`Arc<JobStore>`): the durable job store; pointer is cloned.
/// - `executions` (`Arc<JobExecutionRegistry>`): registry the fenced
///   cancellation control is registered in.
/// - `provider` (`Arc<dyn ModelProvider>`): the model behind every turn.
/// - `transcript_root` (`&Path`): root of the durable job transcripts.
/// - `plan_services` (`Option<Arc<PlanNodeServices>>`): plan store, inherited
///   sandbox and plan actor; `None` disables plan-node execution.
///
/// # Returns
/// The number of jobs that reached a terminal state in this poll.
///
/// # Concurrency
/// Jobs are executed sequentially inside one poll; every execution is fenced by
/// [`DurableJobRunner`].
pub async fn run_job_worker_once(
    store: Arc<JobStore>,
    executions: Arc<JobExecutionRegistry>,
    provider: Arc<dyn ModelProvider>,
    transcript_root: &Path,
    plan_services: Option<Arc<PlanNodeServices>>,
) -> usize {
    let page = match store.list(&JobListQuery {
        states: Some(vec![JobState::Ready]),
        ..JobListQuery::default()
    }) {
        Ok(page) => page,
        Err(error) => {
            tracing::warn!(error = %error, "durable job poll failed");
            return 0;
        }
    };

    let runner = DurableJobRunner::new(Arc::clone(&store), executions);
    let mut completed = 0;
    for record in page.jobs {
        if record.not_before > Timestamp::now() || !is_supported_kind(&record.job.kind) {
            continue;
        }

        let work_id = record.job.id.clone();
        let input = record.input.clone();
        let provider = Arc::clone(&provider);
        let transcript_root = transcript_root.to_path_buf();
        let services = plan_services.as_ref().map(Arc::clone);
        let result = runner
            .run(
                &work_id,
                &ClaimRequest {
                    worker_id: WORKER_ID.to_owned(),
                    lease_ttl: SignedDuration::from_secs(LEASE_TTL_SECONDS),
                    now: Timestamp::now(),
                },
                move |claim| {
                    let control = WorkerExecutionControl::new();
                    let operation_control = Arc::clone(&control);
                    (
                        control as Arc<dyn ExecutionControl>,
                        execute_claim(
                            claim,
                            input,
                            provider,
                            transcript_root,
                            services,
                            operation_control,
                        ),
                    )
                },
            )
            .await;
        match result {
            Ok(_) => completed += 1,
            Err(error) => {
                tracing::warn!(work_id = %work_id.as_str(), error = %error, "durable job execution failed")
            }
        }
    }
    completed
}

/// Polls for durable jobs until `shutdown` is set.
///
/// # Description
/// This is intentionally a small composition loop. `harw serve` supplies the
/// shutdown watch channel; all job execution remains fenced by
/// [`DurableJobRunner`].
///
/// # Arguments
/// - `store` (`Arc<JobStore>`): the durable job store.
/// - `executions` (`Arc<JobExecutionRegistry>`): fenced cancellation registry.
/// - `provider` (`Arc<dyn ModelProvider>`): the model behind every turn.
/// - `transcript_root` (`&Path`): root of the durable job transcripts.
/// - `plan_services` (`Option<Arc<PlanNodeServices>>`): plan store, inherited
///   sandbox and plan actor; `None` disables plan-node execution.
/// - `shutdown` (`watch::Receiver<bool>`): set to `true` to end the loop.
///
/// # Returns
/// Nothing; returns once `shutdown` is observed as `true` or its sender is
/// dropped.
///
/// # Concurrency
/// Intended to be driven by a dedicated `tokio` task.
pub async fn run_job_worker(
    store: Arc<JobStore>,
    executions: Arc<JobExecutionRegistry>,
    provider: Arc<dyn ModelProvider>,
    transcript_root: &Path,
    plan_services: Option<Arc<PlanNodeServices>>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        let _ = run_job_worker_once(
            Arc::clone(&store),
            Arc::clone(&executions),
            Arc::clone(&provider),
            transcript_root,
            plan_services.as_ref().map(Arc::clone),
        )
        .await;
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
        }
    }
}

// Kinds this worker knows how to execute. Every other kind belongs to another
// consumer and is left untouched (fail closed: an unknown `Custom` name is not
// a plan node).
fn is_supported_kind(kind: &JobKind) -> bool {
    match kind {
        JobKind::Worker | JobKind::Dream => true,
        JobKind::Custom(name) => name == PLAN_NODE_JOB_KIND,
    }
}

// True only for the plan-node discriminator.
fn is_plan_node_kind(kind: &JobKind) -> bool {
    matches!(kind, JobKind::Custom(name) if name == PLAN_NODE_JOB_KIND)
}

// ──────────────────────────────────────────────────────────────────────────────
// Claim dispatch
// ──────────────────────────────────────────────────────────────────────────────

async fn execute_claim(
    claim: JobClaim,
    input: serde_json::Value,
    provider: Arc<dyn ModelProvider>,
    transcript_root: PathBuf,
    plan_services: Option<Arc<PlanNodeServices>>,
    control: Arc<WorkerExecutionControl>,
) -> JobOutcome {
    let outcome = if is_plan_node_kind(&claim.job.kind) {
        execute_plan_node_claim(
            claim,
            input,
            provider,
            transcript_root,
            plan_services,
            Arc::clone(&control),
        )
        .await
    } else {
        execute_prompt_claim(
            claim,
            input,
            provider,
            transcript_root,
            Arc::clone(&control),
        )
        .await
    };
    control.signal_completion();
    outcome
}

// The historical path: a trusted prompt string, no tools, no sandbox.
async fn execute_prompt_claim(
    claim: JobClaim,
    input: serde_json::Value,
    provider: Arc<dyn ModelProvider>,
    transcript_root: PathBuf,
    control: Arc<WorkerExecutionControl>,
) -> JobOutcome {
    match prompt_from_input(&input) {
        Ok(prompt) => {
            execute_turn(
                claim,
                prompt,
                provider,
                transcript_root,
                control,
                TurnSetup {
                    registry: empty_extension_registry(),
                    spawn_context: None,
                    pause: PauseDisposition::Blocked,
                },
            )
            .await
        }
        Err(reason) => JobOutcome::Blocked { reason },
    }
}

/// Derives the trace a claimed plan-node job's [`SpawnContext`] should carry.
///
/// # Description
/// AW1-01c. This is deliberately not a fresh root: a plan-node job continues
/// work that was already admitted with a trace (`StoredJob.trace`, AW1-01),
/// so inventing a new root here would look like continuity without being
/// one — worse than carrying no trace at all. The correct behaviour would be
/// to *inherit* that trace (same `trace_id`, a fresh span, the job's span as
/// `parent_span_id`), but [`JobClaim`] (`harw-job-runtime/src/stored.rs`)
/// does not carry the trace forward from the `StoredJob` it was claimed
/// from — `harw_session_store::JobStore::claim` builds `JobClaim` without
/// copying `record.trace`. There is nothing on `claim` to inherit yet, so
/// this returns `None` until `JobClaim` gains a `trace` field.
///
/// # Arguments
/// - `claim` (`&JobClaim`): the claimed job. Unused today — kept as a
///   parameter so a future `JobClaim` trace field can be threaded through
///   here without changing the call site.
///
/// # Returns
/// Always `None`, for the reason above.
fn plan_node_spawn_trace(_claim: &JobClaim) -> Option<TraceContext> {
    None
}

// The plan-node path: typed payload, contract-derived sandbox, role-derived
// tool registry, and a mandatory report back into the plan.
async fn execute_plan_node_claim(
    claim: JobClaim,
    input: serde_json::Value,
    provider: Arc<dyn ModelProvider>,
    transcript_root: PathBuf,
    plan_services: Option<Arc<PlanNodeServices>>,
    control: Arc<WorkerExecutionControl>,
) -> JobOutcome {
    let work_id = claim.job.id.as_str().to_owned();

    let Some(services) = plan_services else {
        tracing::error!(
            work_id = %work_id,
            "plan-node job claimed without a configured plan store"
        );
        return JobOutcome::Blocked {
            reason: MISSING_PLAN_SERVICES.to_owned(),
        };
    };

    // Everything below the payload parse is unreportable to the plan: without a
    // task ID there is no node to invalidate.
    let payload = match PlanNodePayload::parse(&input) {
        Ok(payload) => payload,
        Err(reason) => {
            tracing::error!(work_id = %work_id, reason = %reason, "plan-node payload rejected");
            return JobOutcome::Blocked { reason };
        }
    };
    let kind = match node_kind(services.plan(), &payload.task_id) {
        Ok(kind) => kind,
        Err(reason) => {
            tracing::error!(
                task = %payload.task_id,
                work_id = %work_id,
                reason = %reason,
                "plan-node job has no node in the current plan"
            );
            return JobOutcome::Blocked { reason };
        }
    };

    // From here the node exists, so every failure is reportable.
    let sandbox = match derive_plan_node_sandbox(services.sandbox(), &payload.contract) {
        Ok(sandbox) => sandbox,
        Err(reason) => {
            tracing::error!(
                task = %payload.task_id,
                work_id = %work_id,
                reason = %reason,
                "plan-node contract demands more authority than the job holds"
            );
            return fail_plan_node(&services, &payload.task_id, &work_id, reason);
        }
    };
    let may_write = sandbox.permissions().contains(Permission::WriteWorkspace);
    let profile = profile_for_node_kind(kind, may_write);

    let workspace_root = sandbox.workspace().canonical_root().to_path_buf();
    let assembled = match assemble_registry(
        profile,
        workspace_root,
        plan_node_identity(&payload, profile),
    ) {
        Ok(assembled) => assembled,
        Err(error) => {
            let reason = format!(
                "could not assemble the plan-node tool registry: {}",
                sanitize_failure(&error.to_string())
            );
            tracing::error!(
                task = %payload.task_id,
                work_id = %work_id,
                error = %error,
                "plan-node registry assembly failed"
            );
            return fail_plan_node(&services, &payload.task_id, &work_id, reason);
        }
    };

    tracing::info!(
        task = %payload.task_id,
        work_id = %work_id,
        kind = ?kind,
        profile = ?profile,
        may_write = may_write,
        "plan-node job starting"
    );

    // Computed before `claim` moves into `execute_turn` below.
    let trace = plan_node_spawn_trace(&claim);
    let outcome = execute_turn(
        claim,
        plan_node_prompt(&payload),
        provider,
        transcript_root,
        control,
        TurnSetup {
            registry: assembled.registry,
            spawn_context: Some(SpawnContext {
                sandbox,
                suggestions: None,
                capability_snapshot: None,
                // No identity may answer an approval: a durable job has no user.
                approval_actor: None,
                organizational_role: AgentRoleId::Worker,
                trace,
                // Root: a plan-node job has no parent session whose
                // already-cut ceiling it could inherit (see
                // `plan_node_spawn_trace` above for the analogous trace
                // reasoning) — the ceiling is created here, once. `Worker`
                // is a leaf in the §3 spawn matrix and never admits further
                // children, so this ceiling only ever governs this job's
                // own context assembly, never a cut for a grandchild.
                ceiling: Some(crate::root_context::local_root_context_ceiling()),
            }),
            pause: PauseDisposition::Failed,
        },
    )
    .await;

    report_plan_node_outcome(&services, &payload.task_id, &work_id, outcome)
}

// ──────────────────────────────────────────────────────────────────────────────
// Turn execution
// ──────────────────────────────────────────────────────────────────────────────

/// How a turn that paused instead of completing is scored.
///
/// # Description
/// A turn can end in [`TurnOutcome::AwaitingApproval`] (a guardrail wants a
/// user decision) or [`TurnOutcome::AwaitingChild`] (a handoff tool was
/// called).  Neither can ever be resumed inside a durable job, so the job is
/// terminated in both cases — the only question is *how*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PauseDisposition {
    /// Historical behaviour for `Worker`/`Dream` jobs: the job is blocked.
    Blocked,
    /// Plan-node behaviour: the job failed, and the node is invalidated.
    Failed,
}

/// Everything the turn needs beyond prompt and provider.
struct TurnSetup {
    /// The tool surface this turn may see.
    registry: ExtensionRegistry,
    /// The trusted context (sandbox, approval identity, organizational role).
    spawn_context: Option<SpawnContext>,
    /// How a paused turn is scored.
    pause: PauseDisposition,
}

async fn execute_turn(
    claim: JobClaim,
    prompt: String,
    provider: Arc<dyn ModelProvider>,
    transcript_root: PathBuf,
    control: Arc<WorkerExecutionControl>,
    setup: TurnSetup,
) -> JobOutcome {
    let TurnSetup {
        registry,
        spawn_context,
        pause,
    } = setup;

    let session_id = SessionId::from_str(format!("durable-job-{}", claim.job.id.as_str()));
    let durable_session_id = session_id.clone();
    let transcripts = TranscriptStateStore::new(TranscriptStore::new(&transcript_root), job_thread);
    let durable_transcript_root = transcript_root.clone();
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut session =
        AgentSession::new_with_id(session_id, AgentRole::Assistant, None, registry, event_tx);
    if let Some(context) = spawn_context {
        session = session.with_spawn_context(context);
    }

    let mut turn = tokio::spawn(async move {
        run_turn(
            &mut session,
            provider.as_ref(),
            &transcripts,
            TurnInput::user(prompt),
        )
        .await
        .map(|outcome| (outcome, session))
    });
    control.set_abort_handle(turn.abort_handle());
    let mut cancellation = control.cancellation();
    let result = tokio::select! {
        result = &mut turn => result,
        changed = cancellation.changed() => {
            if changed.is_ok() && *cancellation.borrow() {
                turn.abort();
                let _ = turn.await;
                control.clear_abort_handle();
                return JobOutcome::Cancelled { reason: "cancelled by trusted supervisor".to_owned() };
            }
            turn.await
        }
    };
    control.clear_abort_handle();
    match result {
        Ok(Ok((TurnOutcome::Completed, _session))) => {
            let durable_transcripts = TranscriptStateStore::new(
                TranscriptStore::new(&durable_transcript_root),
                job_thread,
            );
            match durable_transcripts.load_history(&durable_session_id).await {
                Ok(history) => JobOutcome::Succeeded {
                    result: serde_json::json!({ "assistant": last_assistant_text(&history) }),
                },
                Err(error) => JobOutcome::Failed {
                    reason: sanitize_failure(&error.to_string()),
                },
            }
        }
        Ok(Ok((paused, _session))) => paused_turn_outcome(&paused, pause),
        Ok(Err(error)) => JobOutcome::Failed {
            reason: sanitize_failure(&error.to_string()),
        },
        Err(error) if error.is_cancelled() => JobOutcome::Cancelled {
            reason: "cancelled by trusted supervisor".to_owned(),
        },
        Err(error) => JobOutcome::Failed {
            reason: sanitize_failure(&error.to_string()),
        },
    }
}

// Scores a turn that did not complete. Both pause variants are handled fail
// closed; nothing here ever waits for a decision that cannot arrive.
fn paused_turn_outcome(outcome: &TurnOutcome, disposition: PauseDisposition) -> JobOutcome {
    let reason = match outcome {
        // Unreachable in practice — `Completed` is handled by the caller — but
        // keeping the match total means a new variant is a compile error.
        TurnOutcome::Completed => "job turn reported a pause without pausing".to_owned(),
        TurnOutcome::AwaitingApproval { call_id, .. } => format!(
            "tool call '{call_id}' asked for user approval; a durable job has no user to ask"
        ),
        TurnOutcome::AwaitingChild { child, role, .. } => format!(
            "turn handed off to child session '{child}' in role '{role}'; the job worker cannot resume a child"
        ),
    };
    match disposition {
        PauseDisposition::Blocked => JobOutcome::Blocked { reason },
        PauseDisposition::Failed => JobOutcome::Failed { reason },
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Plan payload
// ──────────────────────────────────────────────────────────────────────────────

/// The execution contract a plan-node job carries in `StoredJob::input`.
///
/// # Description
/// Mirrors exactly what `harw_plan_bridge::job_bridge::node_payload` writes.
/// Every field is required: a plan job whose payload lost a field is a broken
/// contract between bridge and worker, and guessing a default there would
/// silently execute a node under an authority nobody wrote down.
///
/// The node's *kind* is deliberately absent from the payload — it is read from
/// the live plan instead (see [`node_kind`]), because the plan, not a stored
/// payload, is the authority on what a node is.
struct PlanNodePayload {
    /// The plan the node belongs to.
    plan_id: String,
    /// The node this job executes.
    task_id: TaskId,
    /// The plan revision the payload was produced from.
    plan_revision: u64,
    /// The mutation contract; the only source of the derived sandbox.
    contract: MutationContract,
    /// Human-readable objective of the node.
    objective: String,
    /// Acceptance criteria that must hold before the node may complete.
    acceptance_criteria: Vec<Criterion>,
    /// Paths/symbols the node may read.
    read_scope: Vec<String>,
    /// Paths/symbols the node may write.
    write_scope: Vec<String>,
    /// Paths/symbols the node may neither read nor write.
    forbidden_scope: Vec<String>,
}

impl PlanNodePayload {
    // Parses the payload field by field so a missing or ill-typed field names
    // itself in the error. Nested values are handed to `serde_json` typed, so
    // there is no `value["key"]` guessing anywhere in this module.
    fn parse(input: &serde_json::Value) -> Result<Self, String> {
        let Some(object) = input.as_object() else {
            return Err(format!(
                "'{PLAN_NODE_JOB_KIND}' job input must be a server-resolved object"
            ));
        };

        let plan_id = required_str(object, "plan_id")?.to_owned();
        let task_id = TaskId::new(required_str(object, "task_id")?);
        let plan_revision = required_u64(object, "plan_revision")?;
        let contract: MutationContract =
            serde_json::from_value(required_field(object, "contract")?.clone())
                .map_err(|error| field_error("contract", &error.to_string()))?;
        let objective = required_str(object, "objective")?.to_owned();
        let acceptance_criteria: Vec<Criterion> =
            serde_json::from_value(required_field(object, "acceptance_criteria")?.clone())
                .map_err(|error| field_error("acceptance_criteria", &error.to_string()))?;
        let read_scope = required_strings(object, "read_scope")?;
        let write_scope = required_strings(object, "write_scope")?;
        let forbidden_scope = required_strings(object, "forbidden_scope")?;

        if contract.task_id != task_id {
            return Err(format!(
                "'{PLAN_NODE_JOB_KIND}' job input names task '{task_id}' but its contract names '{}'",
                contract.task_id
            ));
        }

        Ok(Self {
            plan_id,
            task_id,
            plan_revision,
            contract,
            objective,
            acceptance_criteria,
            read_scope,
            write_scope,
            forbidden_scope,
        })
    }
}

// Uniform wording for a rejected payload field.
fn field_error(name: &str, detail: &str) -> String {
    format!(
        "'{PLAN_NODE_JOB_KIND}' job input field '{name}' is invalid: {}",
        sanitize_failure(detail)
    )
}

// Borrows a required field or names the one that is missing.
fn required_field<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<&'a serde_json::Value, String> {
    object.get(name).ok_or_else(|| {
        format!("'{PLAN_NODE_JOB_KIND}' job input is missing the required field '{name}'")
    })
}

// A required, non-blank string field.
fn required_str<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<&'a str, String> {
    let value = required_field(object, name)?;
    let text = value
        .as_str()
        .ok_or_else(|| field_error(name, "expected a string"))?
        .trim();
    if text.is_empty() {
        return Err(field_error(name, "expected a non-empty string"));
    }
    Ok(text)
}

// A required unsigned integer field.
fn required_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<u64, String> {
    required_field(object, name)?
        .as_u64()
        .ok_or_else(|| field_error(name, "expected an unsigned integer"))
}

// A required array-of-strings field.
fn required_strings(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<Vec<String>, String> {
    let parsed: Vec<String> = serde_json::from_value(required_field(object, name)?.clone())
        .map_err(|error| field_error(name, &error.to_string()))?;
    Ok(parsed)
}

// Reads the node's kind from the live plan. The payload does not carry it on
// purpose: a stale payload must not be able to claim a different role.
fn node_kind(plan: &dyn PlanStore, task: &TaskId) -> Result<PlanNodeKind, String> {
    let snapshot = plan.current().map_err(|error| {
        format!(
            "plan snapshot unavailable for task '{task}': {}",
            sanitize_failure(&error.to_string())
        )
    })?;
    snapshot
        .nodes
        .iter()
        .find(|node| &node.id == task)
        .map(|node| node.kind)
        .ok_or_else(|| format!("plan node '{task}' is not part of the current plan"))
}

// ──────────────────────────────────────────────────────────────────────────────
// Authority derivation
// ──────────────────────────────────────────────────────────────────────────────

/// The permission ceiling a mutation contract authorises.
///
/// # Description
/// The contract speaks about exactly one axis: which repository paths the node
/// may change.  It therefore authorises reading the workspace, and writing it
/// only when it actually names allowed paths.  Everything else
/// (`ExecuteProcess`, `NetworkAccess`, `ReadSecrets`, `ManagePlugins`,
/// `ReadCargoRegistry`) is *not* mentioned by the contract and is consequently
/// not part of the ceiling — a plan node never gains authority this worker was
/// not told to grant.
fn contract_permission_ceiling(contract: &MutationContract) -> PermissionSet {
    let mut granted = vec![Permission::ReadWorkspace];
    if !contract.allowed_paths.is_empty() {
        granted.push(Permission::WriteWorkspace);
    }
    PermissionSet::from_policy(granted)
}

// The literal pattern a path rule is built from.
fn rule_pattern(rule: &PathRule) -> &str {
    match rule {
        PathRule::Exact(pattern)
        | PathRule::DirectoryPrefix(pattern)
        | PathRule::ExtensionSuffix(pattern)
        | PathRule::Glob(pattern) => pattern,
    }
}

// Purely lexical containment check: no filesystem access, so an unwritten path
// is judged exactly like an existing one. Absolute paths, drive prefixes, home
// expansion and `..` all leave the workspace and are rejected.
fn leaves_workspace(pattern: &str) -> bool {
    let trimmed = pattern.trim();
    if trimmed.is_empty() || trimmed.starts_with('~') {
        return true;
    }
    Path::new(trimmed).components().any(|component| {
        matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )
    })
}

/// Derives the sandbox a plan node runs under by reducing the inherited one.
///
/// # Description
/// The operation is monotone in both directions it can go: the returned
/// sandbox is `inherited ∩ contract_ceiling`, and every demand the contract
/// makes that `inherited` does not already hold is an error rather than a
/// grant.  It also rejects a write scope that lexically leaves the workspace,
/// because such a path could never be a *reduction* of a workspace-bound
/// sandbox.
///
/// # Arguments
/// - `inherited` (`&SandboxSpec`): the authority the job itself holds.
/// - `contract` (`&MutationContract`): the node's mutation contract.
///
/// # Returns
/// The narrowed [`SandboxSpec`]; it always passes
/// [`SandboxSpec::ensure_child_of`] against `inherited`.
///
/// # Errors
/// Returns a human-readable reason when the contract names a path outside the
/// workspace, when it needs a permission the job does not hold, or when the
/// derived sandbox would not be a child of `inherited`.
///
/// # Concurrency
/// Pure; safe from any thread.
fn derive_plan_node_sandbox(
    inherited: &SandboxSpec,
    contract: &MutationContract,
) -> Result<SandboxSpec, String> {
    for rule in &contract.allowed_paths {
        let pattern = rule_pattern(rule);
        if leaves_workspace(pattern) {
            return Err(format!(
                "write scope '{pattern}' leaves the workspace; a plan contract may only narrow authority"
            ));
        }
    }

    let ceiling = contract_permission_ceiling(contract);
    for permission in ceiling.iter() {
        if !inherited.permissions().contains(permission) {
            return Err(format!(
                "plan contract requires {permission:?}, which this job's sandbox does not grant; authority is only ever reduced"
            ));
        }
    }

    let derived = inherited.restrict(&ceiling);
    derived.ensure_child_of(inherited).map_err(|error| {
        format!(
            "derived plan-node sandbox is not a reduction of the job sandbox: {}",
            sanitize_failure(&error.to_string())
        )
    })?;
    Ok(derived)
}

/// Picks the tool registry profile for a node kind.
///
/// # Description
/// A coding node is not a research node: only kinds that actually produce code
/// or documents get the writing profile, and only when the derived sandbox
/// really grants [`Permission::WriteWorkspace`].  Everything else stays
/// read-only.
///
/// [`RegistryProfile::Research`] is intentionally never selected: the derived
/// sandbox never carries [`Permission::NetworkAccess`] (the mutation contract
/// says nothing about the network), so advertising web tools would advertise
/// tools that must fail.
fn profile_for_node_kind(kind: PlanNodeKind, may_write: bool) -> RegistryProfile {
    match kind {
        PlanNodeKind::Coding
        | PlanNodeKind::Integration
        | PlanNodeKind::Verification
        | PlanNodeKind::Docs
        | PlanNodeKind::Contract => {
            if may_write {
                RegistryProfile::Full
            } else {
                RegistryProfile::ReadOnlyExplore
            }
        }
        PlanNodeKind::Research
        | PlanNodeKind::Explore
        | PlanNodeKind::Analysis
        | PlanNodeKind::Synthesis
        | PlanNodeKind::Composite => RegistryProfile::ReadOnlyExplore,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Plan reporting
// ──────────────────────────────────────────────────────────────────────────────

// Invalidates the node and returns the matching job outcome.
fn fail_plan_node(
    services: &PlanNodeServices,
    task: &TaskId,
    work_id: &str,
    reason: String,
) -> JobOutcome {
    report_plan_node_failure(services, task, work_id, &reason);
    JobOutcome::Failed { reason }
}

// Best-effort invalidation. A failure here is loud but must not mask the
// original job outcome.
fn report_plan_node_failure(
    services: &PlanNodeServices,
    task: &TaskId,
    work_id: &str,
    reason: &str,
) {
    if let Err(error) =
        PlanJobBridge::on_job_failed(services.plan(), task, work_id, reason, services.actor())
    {
        tracing::error!(
            task = %task,
            work_id = %work_id,
            error = %error,
            "could not invalidate the plan node of a failed job"
        );
    }
}

// Maps a terminal job outcome onto the plan. Every non-success path reports,
// so no node is left `InProgress` by a finished job.
fn report_plan_node_outcome(
    services: &PlanNodeServices,
    task: &TaskId,
    work_id: &str,
    outcome: JobOutcome,
) -> JobOutcome {
    match outcome {
        JobOutcome::Succeeded { result } => {
            let summary = plan_node_summary(&result);
            match PlanJobBridge::on_job_completed(
                services.plan(),
                task,
                work_id,
                &summary,
                services.actor(),
                offset_from_timestamp(Timestamp::now()),
            ) {
                Ok(()) => {
                    tracing::info!(
                        task = %task,
                        work_id = %work_id,
                        "plan node completed by durable job"
                    );
                    JobOutcome::Succeeded { result }
                }
                Err(error) => {
                    tracing::error!(
                        task = %task,
                        work_id = %work_id,
                        error = %error,
                        "job succeeded but the plan node could not be completed"
                    );
                    let reason = format!(
                        "job succeeded but the plan node could not be completed: {}",
                        sanitize_failure(&error.to_string())
                    );
                    report_plan_node_failure(services, task, work_id, &reason);
                    JobOutcome::Failed { reason }
                }
            }
        }
        JobOutcome::Failed { reason } => {
            report_plan_node_failure(services, task, work_id, &reason);
            JobOutcome::Failed { reason }
        }
        JobOutcome::Blocked { reason } => {
            report_plan_node_failure(services, task, work_id, &reason);
            JobOutcome::Blocked { reason }
        }
        JobOutcome::Cancelled { reason } => {
            report_plan_node_failure(services, task, work_id, &reason);
            JobOutcome::Cancelled { reason }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Prompt and identity
// ──────────────────────────────────────────────────────────────────────────────

// Identity of the plan-node agent. It advertises exactly the profile's tools
// and says out loud that nobody can be asked anything.
fn plan_node_identity(payload: &PlanNodePayload, profile: RegistryProfile) -> IdentityOverrides {
    IdentityOverrides {
        agent_name: Some(format!("plan-node-{}", payload.task_id)),
        role_description: Some(format!(
            "{} executing plan node '{}'",
            profile.role_description(),
            payload.task_id
        )),
        extra_context: vec![
            "You run unattended inside a durable job. There is no user: an approval request ends the job as failed."
                .to_owned(),
            format!(
                "You may only modify paths inside the write scope of plan node '{}'.",
                payload.task_id
            ),
        ],
    }
}

// Renders the node contract as the turn's user prompt.
fn plan_node_prompt(payload: &PlanNodePayload) -> String {
    let mut prompt = format!(
        "You are executing plan node '{}' of plan '{}' at plan revision {}.\n\nObjective:\n{}\n",
        payload.task_id, payload.plan_id, payload.plan_revision, payload.objective
    );

    if payload.acceptance_criteria.is_empty() {
        prompt.push_str("\nAcceptance criteria: none were recorded for this node.\n");
    } else {
        prompt.push_str("\nAcceptance criteria:\n");
        for criterion in &payload.acceptance_criteria {
            prompt.push_str(&format!("- {}\n", criterion.description));
            for step in &criterion.verification {
                prompt.push_str(&format!("  verify: {}\n", verification_line(step)));
            }
        }
    }

    push_scope(
        &mut prompt,
        "Write scope (the only paths you may change)",
        &payload.write_scope,
    );
    push_scope(&mut prompt, "Read scope", &payload.read_scope);
    push_scope(
        &mut prompt,
        "Forbidden scope (never read, never write)",
        &payload.forbidden_scope,
    );

    prompt.push_str(
        "\nThis turn runs unattended as a durable job. No user is available, so do not ask for \
         approval or confirmation — an approval request ends this job as failed. If the objective \
         cannot be reached inside the scope above, say so plainly and stop.\n",
    );
    prompt
}

// Appends one scope section; an empty scope is stated rather than omitted.
fn push_scope(prompt: &mut String, title: &str, entries: &[String]) {
    prompt.push_str(&format!("\n{title}:\n"));
    if entries.is_empty() {
        prompt.push_str("- (empty)\n");
        return;
    }
    for entry in entries {
        prompt.push_str(&format!("- {entry}\n"));
    }
}

// One-line rendering of a verification step.
fn verification_line(step: &VerificationStep) -> String {
    match step {
        VerificationStep::Command { cmd, expect_exit } => {
            format!("run `{cmd}` and expect exit code {expect_exit}")
        }
        VerificationStep::Artifact { path } => format!("artifact `{path}` must exist"),
        VerificationStep::TraceEvent { name } => format!("trace event `{name}` must be emitted"),
        VerificationStep::Manual { note } => format!("manual check: {note}"),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Shared helpers
// ──────────────────────────────────────────────────────────────────────────────

fn prompt_from_input(input: &serde_json::Value) -> Result<String, String> {
    let Some(object) = input.as_object() else {
        return Err("job input must be a server-resolved object".to_owned());
    };
    for key in ["prompt", "task"] {
        if let Some(value) = object.get(key).and_then(serde_json::Value::as_str) {
            let prompt = value.trim();
            if !prompt.is_empty() {
                return Ok(prompt.to_owned());
            }
        }
    }
    Err("job input requires a non-empty prompt or task".to_owned())
}

fn job_thread(session_id: &SessionId) -> ThreadRef {
    ThreadRef::from_str(format!("durable-job-{}", session_id.as_str()))
}

fn last_assistant_text(history: &harw_core::ConversationHistory) -> String {
    history
        .to_model_messages()
        .into_iter()
        .rev()
        .find_map(|message| match message {
            ModelMessage::Assistant { text } if !text.trim().is_empty() => Some(text),
            _ => None,
        })
        .unwrap_or_else(|| "(no assistant text)".to_owned())
}

fn sanitize_failure(detail: &str) -> String {
    let first_line = detail.lines().next().unwrap_or("job turn failed");
    first_line.chars().take(MAX_REASON_BYTES).collect()
}

// Condenses the job result into the one-line summary the plan records as the
// reason of the `Completed` status change.
fn plan_node_summary(result: &serde_json::Value) -> String {
    let text = result
        .get("assistant")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return "job completed without assistant text".to_owned();
    }
    text.lines()
        .next()
        .unwrap_or(text)
        .chars()
        .take(MAX_REASON_BYTES)
        .collect()
}

/// Per-claim cancellation control registered under the exact fenced lease.
struct WorkerExecutionControl {
    cancelled: watch::Sender<bool>,
    cancellation: watch::Receiver<bool>,
    completed: watch::Sender<bool>,
    completion: watch::Receiver<bool>,
    abort_handle: Mutex<Option<tokio::task::AbortHandle>>,
}

impl WorkerExecutionControl {
    fn new() -> Arc<Self> {
        let (cancelled, cancellation) = watch::channel(false);
        let (completed, completion) = watch::channel(false);
        Arc::new(Self {
            cancelled,
            cancellation,
            completed,
            completion,
            abort_handle: Mutex::new(None),
        })
    }

    fn cancellation(&self) -> watch::Receiver<bool> {
        self.cancellation.clone()
    }

    fn set_abort_handle(&self, handle: tokio::task::AbortHandle) {
        if let Ok(mut slot) = self.abort_handle.lock() {
            *slot = Some(handle);
        }
    }

    fn clear_abort_handle(&self) {
        if let Ok(mut slot) = self.abort_handle.lock() {
            *slot = None;
        }
    }

    fn signal_completion(&self) {
        let _ = self.completed.send(true);
    }
}

impl ExecutionControl for WorkerExecutionControl {
    fn request_graceful_cancel(&self) {
        let _ = self.cancelled.send(true);
    }

    fn force_abort(&self) {
        if let Ok(slot) = self.abort_handle.lock() {
            if let Some(handle) = slot.as_ref() {
                handle.abort();
            }
        }
    }

    fn completion(&self) -> watch::Receiver<bool> {
        self.completion.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_core::{EchoModelProvider, JobExecutionRegistry, RecordingModelProvider};
    use harw_job_runtime::{Budget, Job, JobScope, RetryPolicy, StoredJob};
    use harw_plan::InMemoryPlanStore;
    use harw_plan::admission::RepoRevision;
    use harw_plan::ids::RevisionId;
    use harw_sandbox::{WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{ApprovalActor, ItemId, TenantId, ToolCallId, WorkId, WorkspaceId};

    // ── Fixtures ──────────────────────────────────────────────────────────

    fn temp_dir() -> tempfile::TempDir {
        match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("temp directory: {error}"),
        }
    }

    fn ready_record(id: &str, input: serde_json::Value) -> StoredJob {
        ready_record_of_kind(id, JobKind::Worker, input)
    }

    fn ready_record_of_kind(id: &str, kind: JobKind, input: serde_json::Value) -> StoredJob {
        let now = Timestamp::now();
        let mut job = Job::new(
            WorkId::from_str(id),
            kind,
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: 1,
                base_delay: SignedDuration::from_secs(1),
                factor: 1.0,
                max_delay: SignedDuration::from_secs(1),
            },
            now,
        );
        if let Err(error) = job.mark_ready(now) {
            panic!("mark_ready: {error}");
        }
        StoredJob {
            job,
            scope: JobScope::new(
                TenantId::from_str("tenant"),
                WorkspaceId::from_str("workspace"),
                ApprovalActor::Operator {
                    id: "operator".to_owned(),
                },
            ),
            input,
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

    fn admit(store: &JobStore, record: &StoredJob) {
        if let Err(error) = store.admit(record) {
            panic!("admit: {error}");
        }
    }

    fn completion_of(store: &JobStore, id: &str) -> JobOutcome {
        let stored = match store.get(&WorkId::from_str(id)) {
            Ok(stored) => stored,
            Err(error) => panic!("get '{id}': {error}"),
        };
        match stored.completion {
            Some(completion) => completion.outcome,
            None => panic!("job '{id}' has no completion record"),
        }
    }

    // Builds a real workspace binding under `root/workspace`.
    fn sandbox_with(root: &Path, permissions: &[Permission]) -> SandboxSpec {
        let workspace_root = root.join("workspace");
        if let Err(error) = std::fs::create_dir_all(&workspace_root) {
            panic!("create workspace: {error}");
        }
        let tenant = TenantId::from_str("tenant");
        let workspace = WorkspaceId::from_str("workspace");
        let registry = match WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: tenant.clone(),
                workspace: workspace.clone(),
                root: PathBuf::from("workspace"),
            }],
        ) {
            Ok(registry) => registry,
            Err(error) => panic!("workspace registry: {error}"),
        };
        let binding = match registry.resolve(&tenant, &workspace) {
            Ok(binding) => binding,
            Err(error) => panic!("resolve workspace: {error}"),
        };
        SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy(permissions.iter().copied()),
        )
    }

    fn contract_for(task: &str, allowed: &[PathRule]) -> MutationContract {
        MutationContract {
            base_revision: RepoRevision("sha".to_owned()),
            plan_revision: RevisionId::new(2),
            task_id: TaskId::new(task),
            allowed_paths: allowed.to_vec(),
            forbidden_paths: Vec::new(),
        }
    }

    // The exact payload shape `PlanJobBridge::admit_ready_nodes` writes.
    fn plan_node_input(task: Option<&str>, contract: &MutationContract) -> serde_json::Value {
        let contract_value = match serde_json::to_value(contract) {
            Ok(value) => value,
            Err(error) => panic!("serialize contract: {error}"),
        };
        let mut object = serde_json::Map::new();
        object.insert("plan_id".to_owned(), serde_json::json!("p-test"));
        if let Some(task) = task {
            object.insert("task_id".to_owned(), serde_json::json!(task));
        }
        object.insert("plan_revision".to_owned(), serde_json::json!(2));
        object.insert("contract".to_owned(), contract_value);
        object.insert("objective".to_owned(), serde_json::json!("do the thing"));
        object.insert("acceptance_criteria".to_owned(), serde_json::json!([]));
        object.insert("read_scope".to_owned(), serde_json::json!(["src"]));
        object.insert("write_scope".to_owned(), serde_json::json!(["src/lib.rs"]));
        object.insert("forbidden_scope".to_owned(), serde_json::json!([]));
        serde_json::Value::Object(object)
    }

    fn plan_services(root: &Path, permissions: &[Permission]) -> Arc<PlanNodeServices> {
        let plan: Arc<dyn PlanStore> = Arc::new(InMemoryPlanStore::new());
        Arc::new(PlanNodeServices::new(
            plan,
            sandbox_with(root, permissions),
            "test-runtime".to_owned(),
        ))
    }

    // ── Kind dispatch ─────────────────────────────────────────────────────

    #[test]
    fn test_is_supported_kind_accepts_plan_node_and_rejects_other_custom_kinds() {
        assert!(is_supported_kind(&JobKind::Worker));
        assert!(is_supported_kind(&JobKind::Dream));
        assert!(is_supported_kind(&JobKind::Custom(
            PLAN_NODE_JOB_KIND.to_owned()
        )));
        assert!(!is_supported_kind(&JobKind::Custom(
            "etwas-anderes".to_owned()
        )));
        assert!(!is_supported_kind(&JobKind::Custom(String::new())));
    }

    #[test]
    fn test_is_plan_node_kind_matches_only_the_plan_node_discriminator() {
        assert!(is_plan_node_kind(&JobKind::Custom(
            PLAN_NODE_JOB_KIND.to_owned()
        )));
        assert!(!is_plan_node_kind(&JobKind::Worker));
        assert!(!is_plan_node_kind(&JobKind::Custom("plan_node".to_owned())));
    }

    // ── Payload parsing ───────────────────────────────────────────────────

    #[test]
    fn test_plan_node_payload_parse_rejects_a_missing_task_id() {
        let contract = contract_for("t-1", &[PathRule::Exact("src/lib.rs".to_owned())]);
        let error = match PlanNodePayload::parse(&plan_node_input(None, &contract)) {
            Ok(_) => panic!("a payload without 'task_id' must not parse"),
            Err(error) => error,
        };
        assert!(error.contains("task_id"), "unclear message: {error}");
        assert!(error.contains("missing"), "unclear message: {error}");
    }

    #[test]
    fn test_plan_node_payload_parse_accepts_a_bridge_payload() {
        let contract = contract_for("t-1", &[PathRule::Exact("src/lib.rs".to_owned())]);
        let payload = match PlanNodePayload::parse(&plan_node_input(Some("t-1"), &contract)) {
            Ok(payload) => payload,
            Err(error) => panic!("bridge payload must parse: {error}"),
        };
        assert_eq!(payload.task_id, TaskId::new("t-1"));
        assert_eq!(payload.plan_id, "p-test");
        assert_eq!(payload.plan_revision, 2);
        assert_eq!(payload.objective, "do the thing");
        assert_eq!(payload.write_scope, vec!["src/lib.rs".to_owned()]);
        assert_eq!(payload.contract.allowed_paths.len(), 1);
    }

    #[test]
    fn test_plan_node_payload_parse_rejects_a_contract_for_another_task() {
        let contract = contract_for("t-other", &[PathRule::Exact("src/lib.rs".to_owned())]);
        let error = match PlanNodePayload::parse(&plan_node_input(Some("t-1"), &contract)) {
            Ok(_) => panic!("a contract naming another task must not parse"),
            Err(error) => error,
        };
        assert!(error.contains("t-other"), "unclear message: {error}");
    }

    #[test]
    fn test_plan_node_payload_parse_rejects_a_non_object_input() {
        let error = match PlanNodePayload::parse(&serde_json::json!("just a string")) {
            Ok(_) => panic!("a non-object payload must not parse"),
            Err(error) => error,
        };
        assert!(error.contains("object"), "unclear message: {error}");
    }

    // ── Authority derivation ──────────────────────────────────────────────

    #[test]
    fn test_derive_plan_node_sandbox_reduces_to_read_and_write_only() {
        let dir = temp_dir();
        let inherited = sandbox_with(
            dir.path(),
            &[
                Permission::ReadWorkspace,
                Permission::WriteWorkspace,
                Permission::ExecuteProcess,
                Permission::NetworkAccess,
            ],
        );
        let contract = contract_for("t-1", &[PathRule::DirectoryPrefix("src".to_owned())]);

        let derived = match derive_plan_node_sandbox(&inherited, &contract) {
            Ok(derived) => derived,
            Err(error) => panic!("derivation must succeed: {error}"),
        };

        assert!(derived.permissions().contains(Permission::ReadWorkspace));
        assert!(derived.permissions().contains(Permission::WriteWorkspace));
        assert!(!derived.permissions().contains(Permission::ExecuteProcess));
        assert!(!derived.permissions().contains(Permission::NetworkAccess));
        assert!(derived.ensure_child_of(&inherited).is_ok());
    }

    #[test]
    fn test_derive_plan_node_sandbox_without_write_scope_keeps_only_read() {
        let dir = temp_dir();
        let inherited = sandbox_with(
            dir.path(),
            &[Permission::ReadWorkspace, Permission::WriteWorkspace],
        );
        let contract = contract_for("t-1", &[]);

        let derived = match derive_plan_node_sandbox(&inherited, &contract) {
            Ok(derived) => derived,
            Err(error) => panic!("derivation must succeed: {error}"),
        };
        assert!(derived.permissions().contains(Permission::ReadWorkspace));
        assert!(!derived.permissions().contains(Permission::WriteWorkspace));
    }

    #[test]
    fn test_derive_plan_node_sandbox_rejects_a_write_scope_beyond_the_inherited_sandbox() {
        let dir = temp_dir();
        // The job may only read; the contract demands writing.
        let inherited = sandbox_with(dir.path(), &[Permission::ReadWorkspace]);
        let contract = contract_for("t-1", &[PathRule::Exact("src/lib.rs".to_owned())]);

        let error = match derive_plan_node_sandbox(&inherited, &contract) {
            Ok(_) => panic!("a contract may never widen the inherited sandbox"),
            Err(error) => error,
        };
        assert!(error.contains("WriteWorkspace"), "unclear message: {error}");
    }

    #[test]
    fn test_derive_plan_node_sandbox_rejects_a_write_scope_escaping_the_workspace() {
        let dir = temp_dir();
        let inherited = sandbox_with(
            dir.path(),
            &[Permission::ReadWorkspace, Permission::WriteWorkspace],
        );
        for escaping in [
            PathRule::Exact("../outside.rs".to_owned()),
            PathRule::DirectoryPrefix("/etc".to_owned()),
            PathRule::Glob("~/secrets/**".to_owned()),
            PathRule::Exact("   ".to_owned()),
        ] {
            let pattern = rule_pattern(&escaping).to_owned();
            let contract = contract_for("t-1", &[escaping]);
            match derive_plan_node_sandbox(&inherited, &contract) {
                Ok(_) => panic!("write scope '{pattern}' must be rejected"),
                Err(error) => assert!(
                    error.contains("leaves the workspace"),
                    "unclear message for '{pattern}': {error}"
                ),
            }
        }
    }

    #[test]
    fn test_contract_permission_ceiling_names_only_workspace_permissions() {
        let with_write = contract_permission_ceiling(&contract_for(
            "t-1",
            &[PathRule::Exact("src/lib.rs".to_owned())],
        ));
        let granted: Vec<Permission> = with_write.iter().collect();
        assert_eq!(
            granted,
            vec![Permission::ReadWorkspace, Permission::WriteWorkspace]
        );

        let read_only = contract_permission_ceiling(&contract_for("t-1", &[]));
        assert_eq!(
            read_only.iter().collect::<Vec<_>>(),
            vec![Permission::ReadWorkspace]
        );
    }

    #[test]
    fn test_profile_for_node_kind_separates_coding_from_research() {
        assert_eq!(
            profile_for_node_kind(PlanNodeKind::Coding, true),
            RegistryProfile::Full
        );
        assert_eq!(
            profile_for_node_kind(PlanNodeKind::Docs, true),
            RegistryProfile::Full
        );
        // Without write authority even a coding node stays read-only.
        assert_eq!(
            profile_for_node_kind(PlanNodeKind::Coding, false),
            RegistryProfile::ReadOnlyExplore
        );
        assert_eq!(
            profile_for_node_kind(PlanNodeKind::Research, true),
            RegistryProfile::ReadOnlyExplore
        );
        assert_eq!(
            profile_for_node_kind(PlanNodeKind::Analysis, true),
            RegistryProfile::ReadOnlyExplore
        );
    }

    // ── Trace (AW1-01c) ──────────────────────────────────────────────────

    /// Documents the AW1-01c decision: even when the claimed job's on-disk
    /// `StoredJob` carried a trace, the resulting `JobClaim` does not — so
    /// `plan_node_spawn_trace` cannot inherit one and must return `None`
    /// rather than inventing a fresh root. This pins the exact gap
    /// (`JobClaim` in `harw-job-runtime/src/stored.rs`) so a follow-up node
    /// can prove it closed by turning this same assertion into `is_some()`.
    #[test]
    fn plan_node_spawn_trace_is_none_even_when_the_stored_job_carries_a_trace() {
        let temp = temp_dir();
        let store = JobStore::new(temp.path());
        let mut record = ready_record_of_kind(
            "plan-node-traced",
            JobKind::Custom(PLAN_NODE_JOB_KIND.to_owned()),
            serde_json::json!({"task_id": "t-1"}),
        );
        record.trace = Some(
            TraceContext::new("a".repeat(32), "b".repeat(16)).expect("sample trace is valid hex"),
        );
        admit(&store, &record);

        let claim = store
            .claim(
                &WorkId::from_str("plan-node-traced"),
                &ClaimRequest {
                    worker_id: WORKER_ID.to_owned(),
                    lease_ttl: SignedDuration::from_secs(LEASE_TTL_SECONDS),
                    now: Timestamp::now(),
                },
            )
            .expect("claim a ready job");

        assert!(
            plan_node_spawn_trace(&claim).is_none(),
            "JobClaim does not carry StoredJob.trace forward yet — see AW1-01c report"
        );
    }

    // ── Approval handling ─────────────────────────────────────────────────

    #[test]
    fn test_paused_turn_outcome_fails_a_plan_node_on_awaiting_approval() {
        let paused = TurnOutcome::AwaitingApproval {
            call_id: ToolCallId::from_str("call-1"),
            request: ItemId::from_str("item-1"),
        };
        let JobOutcome::Failed { reason } = paused_turn_outcome(&paused, PauseDisposition::Failed)
        else {
            panic!("an approval request must fail a plan-node job");
        };
        assert!(reason.contains("call-1"), "unclear message: {reason}");
        assert!(reason.contains("no user"), "unclear message: {reason}");
    }

    #[test]
    fn test_paused_turn_outcome_fails_a_plan_node_on_awaiting_child() {
        let paused = TurnOutcome::AwaitingChild {
            child: SessionId::from_str("child-1"),
            call_id: ToolCallId::from_str("call-2"),
            role: "worker".to_owned(),
        };
        assert!(matches!(
            paused_turn_outcome(&paused, PauseDisposition::Failed),
            JobOutcome::Failed { .. }
        ));
    }

    #[test]
    fn test_paused_turn_outcome_blocks_a_prompt_job_on_awaiting_approval() {
        let paused = TurnOutcome::AwaitingApproval {
            call_id: ToolCallId::from_str("call-3"),
            request: ItemId::from_str("item-3"),
        };
        assert!(matches!(
            paused_turn_outcome(&paused, PauseDisposition::Blocked),
            JobOutcome::Blocked { .. }
        ));
    }

    // ── Worker loop ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_run_job_worker_once_completes_a_ready_job_with_durable_assistant_text() {
        let temp = temp_dir();
        let store = Arc::new(JobStore::new(temp.path()));
        admit(
            &store,
            &ready_record("ready-job", serde_json::json!({"prompt": "hello"})),
        );
        let completed = run_job_worker_once(
            Arc::clone(&store),
            Arc::new(JobExecutionRegistry::new()),
            Arc::new(EchoModelProvider::new("done")),
            temp.path(),
            None,
        )
        .await;
        assert_eq!(completed, 1);
        let JobOutcome::Succeeded { result } = completion_of(&store, "ready-job") else {
            panic!("ready job should durably succeed");
        };
        assert_eq!(result["assistant"], "done");
    }

    #[tokio::test]
    async fn test_run_job_worker_once_blocks_malformed_input_without_calling_model() {
        let temp = temp_dir();
        let store = Arc::new(JobStore::new(temp.path()));
        admit(
            &store,
            &ready_record("bad-input", serde_json::json!({"prompt": "  "})),
        );
        let provider = Arc::new(RecordingModelProvider::new());
        run_job_worker_once(
            Arc::clone(&store),
            Arc::new(JobExecutionRegistry::new()),
            Arc::clone(&provider) as Arc<dyn ModelProvider>,
            temp.path(),
            None,
        )
        .await;
        assert!(provider.recorded().is_empty());
        assert!(matches!(
            completion_of(&store, "bad-input"),
            JobOutcome::Blocked { .. }
        ));
    }

    #[tokio::test]
    async fn test_run_job_worker_once_blocks_a_plan_node_without_task_id_without_calling_model() {
        let temp = temp_dir();
        let store = Arc::new(JobStore::new(temp.path()));
        let contract = contract_for("t-1", &[PathRule::Exact("src/lib.rs".to_owned())]);
        admit(
            &store,
            &ready_record_of_kind(
                "plan-job",
                JobKind::Custom(PLAN_NODE_JOB_KIND.to_owned()),
                plan_node_input(None, &contract),
            ),
        );
        let provider = Arc::new(RecordingModelProvider::new());
        let services = plan_services(
            temp.path(),
            &[Permission::ReadWorkspace, Permission::WriteWorkspace],
        );

        let completed = run_job_worker_once(
            Arc::clone(&store),
            Arc::new(JobExecutionRegistry::new()),
            Arc::clone(&provider) as Arc<dyn ModelProvider>,
            temp.path(),
            Some(services),
        )
        .await;

        assert_eq!(completed, 1, "the plan job must reach a terminal state");
        assert!(
            provider.recorded().is_empty(),
            "a malformed plan payload must never reach the model"
        );
        let JobOutcome::Blocked { reason } = completion_of(&store, "plan-job") else {
            panic!("a plan job without 'task_id' must block");
        };
        assert!(reason.contains("task_id"), "unclear message: {reason}");
    }

    #[tokio::test]
    async fn test_run_job_worker_once_blocks_a_plan_node_without_plan_services() {
        let temp = temp_dir();
        let store = Arc::new(JobStore::new(temp.path()));
        let contract = contract_for("t-1", &[PathRule::Exact("src/lib.rs".to_owned())]);
        admit(
            &store,
            &ready_record_of_kind(
                "orphan-plan-job",
                JobKind::Custom(PLAN_NODE_JOB_KIND.to_owned()),
                plan_node_input(Some("t-1"), &contract),
            ),
        );
        let provider = Arc::new(RecordingModelProvider::new());

        run_job_worker_once(
            Arc::clone(&store),
            Arc::new(JobExecutionRegistry::new()),
            Arc::clone(&provider) as Arc<dyn ModelProvider>,
            temp.path(),
            None,
        )
        .await;

        assert!(provider.recorded().is_empty());
        let JobOutcome::Blocked { reason } = completion_of(&store, "orphan-plan-job") else {
            panic!("a plan job without plan services must block, never be skipped");
        };
        assert_eq!(reason, MISSING_PLAN_SERVICES);
    }

    #[tokio::test]
    async fn test_run_job_worker_once_blocks_a_plan_node_missing_from_the_plan() {
        let temp = temp_dir();
        let store = Arc::new(JobStore::new(temp.path()));
        let contract = contract_for("t-ghost", &[PathRule::Exact("src/lib.rs".to_owned())]);
        admit(
            &store,
            &ready_record_of_kind(
                "ghost-plan-job",
                JobKind::Custom(PLAN_NODE_JOB_KIND.to_owned()),
                plan_node_input(Some("t-ghost"), &contract),
            ),
        );
        let provider = Arc::new(RecordingModelProvider::new());
        let services = plan_services(temp.path(), &[Permission::ReadWorkspace]);

        run_job_worker_once(
            Arc::clone(&store),
            Arc::new(JobExecutionRegistry::new()),
            Arc::clone(&provider) as Arc<dyn ModelProvider>,
            temp.path(),
            Some(services),
        )
        .await;

        assert!(provider.recorded().is_empty());
        assert!(matches!(
            completion_of(&store, "ghost-plan-job"),
            JobOutcome::Blocked { .. }
        ));
    }

    #[tokio::test]
    async fn test_job_execution_registry_uses_the_exact_fenced_token() {
        use std::time::Duration;

        let control = WorkerExecutionControl::new();
        let token = harw_job_runtime::LeaseToken {
            work_id: WorkId::from_str("fenced"),
            epoch: 7,
            nonce: "nonce".to_owned(),
        };
        let registry = JobExecutionRegistry::new();
        // Methodensyntax statt `Arc::clone(&control)`: die UFCS-Form würde ihren
        // Typparameter aus dem erwarteten `Arc<dyn ExecutionControl>` inferieren
        // und bereits ein Trait-Objekt als Argument verlangen — die
        // Unsized-Coercion käme zu spät.
        let control_dyn: Arc<dyn harw_core::ExecutionControl> = control.clone();
        if let Err(error) = registry.register(token.clone(), control_dyn) {
            panic!("register: {error}");
        }
        assert!(registry.contains(&token));
        control.signal_completion();
        let cancelled = match registry.cancel(&token, Duration::from_millis(1)).await {
            Ok(result) => result,
            Err(error) => panic!("cancel: {error}"),
        };
        assert_eq!(cancelled, harw_core::CancellationResult::Graceful);
        assert!(!registry.contains(&token));
        assert!(!registry.contains(&harw_job_runtime::LeaseToken {
            work_id: WorkId::from_str("fenced"),
            epoch: 8,
            nonce: "nonce".to_owned(),
        }));
    }

    // ── Summary rendering ─────────────────────────────────────────────────

    #[test]
    fn test_plan_node_summary_condenses_the_assistant_text() {
        let summary =
            plan_node_summary(&serde_json::json!({"assistant": "done\nsecond line ignored"}));
        assert_eq!(summary, "done");
    }

    #[test]
    fn test_plan_node_summary_handles_a_missing_assistant_field() {
        let summary = plan_node_summary(&serde_json::json!({}));
        assert_eq!(summary, "job completed without assistant text");
    }

    #[test]
    fn test_plan_node_prompt_states_the_scopes_and_forbids_asking() {
        let contract = contract_for("t-1", &[PathRule::Exact("src/lib.rs".to_owned())]);
        let payload = match PlanNodePayload::parse(&plan_node_input(Some("t-1"), &contract)) {
            Ok(payload) => payload,
            Err(error) => panic!("bridge payload must parse: {error}"),
        };
        let prompt = plan_node_prompt(&payload);
        assert!(prompt.contains("plan node 't-1'"), "prompt: {prompt}");
        assert!(prompt.contains("src/lib.rs"), "prompt: {prompt}");
        assert!(
            prompt.contains("do not ask for approval"),
            "prompt: {prompt}"
        );
    }
}
