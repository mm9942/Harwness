//! Durable job consumer for `harw serve`.
//!
//! Jobs arrive here with their authority already resolved by admission.  Two
//! job families are executed, and they differ in exactly one thing: how much
//! authority the turn is allowed to carry.
//!
//! * [`JobKind::Worker`] / [`JobKind::Dream`] — a trusted `prompt`/`task`
//!   string is turned into one durable assistant turn under the runtime entry
//!   `EntryKind::JobPrompt`: no tools, no permissions, no ambient authority.
//! * [`JobKind::Custom`] named [`PLAN_NODE_JOB_KIND`] — a plan node admitted by
//!   `harw_plan_bridge::PlanJobBridge::admit_ready_nodes`.  The turn runs under
//!   `EntryKind::JobPlanNode`, narrowed (`harw_runtime::RuntimeNarrowing`) to
//!   the registry profile chosen by the node's kind and to the permissions of
//!   a sandbox derived by *reduction* from the node's mutation contract.
//!   Success and failure both flow back into the plan, so a node can never be
//!   left `InProgress` forever.
//!
//! # Runtime assembly (W2d-2 J1, CONTRACTS-W2d2.md §1.3)
//! Every executed job is assembled through
//! `crate::runtime_jobs::job_assembly` and gets its root session from
//! `harw_runtime::RuntimeAssembly::new_root_session`, under the session id
//! `durable-job-<work id>`.  Principal, sandbox, context ceiling, trace and
//! approval chain therefore come from the runtime profile table, not from this
//! module.  A worker without a HARW home ([`JobWorkerContext::runtime_root`]
//! `None`) cannot assemble a runtime and blocks every job before any model
//! call.
//!
//! # Approvals
//! A durable job runs unattended.  There is no user who could answer an
//! `AskUser` guardrail, so a paused turn is never awaited: for a plan-node job
//! it terminates the job as failed (see [`PauseDisposition`]).  The job
//! principal (`IngressSurface::JobWorker`) yields no approval actor, so the
//! session's `SpawnContext::approval_actor` is `None` and no identity is even
//! eligible to answer; no responder is mounted.
//!
//! # Concurrency
//! Every entry point is `async` and `Send`.  All shared state travels as
//! `Arc<…>`; the worker itself holds no global state — the plan store, the
//! inherited sandbox, and the plan actor are injected through
//! [`PlanNodeServices`], home and transcript root through
//! [`JobWorkerContext`].

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use harw_core::{
    AgentSession, DurableJobRunner, ExecutionControl, JobExecutionRegistry, ModelMessage,
    ModelProvider, StateStore, TranscriptStateStore, TurnInput, TurnOutcome, run_turn,
};
use harw_job_runtime::{JobClaim, JobKind, JobOutcome, JobState};
use harw_plan::admission::{MutationContract, PathRule};
use harw_plan::{Criterion, PlanNodeKind, PlanStore, TaskId, VerificationStep};
use harw_plan_bridge::{PlanJobBridge, offset_from_timestamp};
use harw_protocol::{SessionEvent, TurnEvent};
use harw_registry_defaults::profile::{IdentityOverrides, RegistryProfile};
use harw_runtime::{RuntimeAssembly, RuntimeNarrowing};
use harw_sandbox::{Permission, PermissionSet, SandboxSpec};
use harw_session_store::{ClaimRequest, JobListQuery, JobStore, TranscriptStore};
use harw_types::{SessionId, ThreadRef};

use crate::runtime_jobs::{JobAssemblyInputs, JobEntry, job_assembly, job_principal};
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

/// Reason recorded when a job is claimed by a worker without a HARW home
/// (CONTRACTS-W2d2.md E3): without a home there is no runtime assembly.
const MISSING_RUNTIME_ROOT: &str = "job runtime requires a HARW home";

/// Home and working directory every job runtime is assembled from.
///
/// # Description
/// `home` is the resolved HARW home (`--home` / `HARW_HOME`); `cwd` is the
/// working directory a prompt job's runtime discovers its project from.  A
/// plan-node job does not use `cwd`: its runtime is assembled below the
/// canonical workspace root of the derived node sandbox.
///
/// # Concurrency
/// Plain owned data; `Send + Sync`.
#[derive(Clone, Debug)]
pub struct JobRuntimeRoot {
    /// Resolved HARW home.
    pub home: PathBuf,
    /// Working directory of prompt-job runtimes.
    pub cwd: PathBuf,
}

/// Everything a worker poll needs besides store, registry, model and plan.
///
/// # Description
/// Built once by `harw serve` and shared as `Arc<JobWorkerContext>` with every
/// poll (CONTRACTS-W2d2.md §1.3).
///
/// # Concurrency
/// Immutable after construction; `Send + Sync`.
#[derive(Debug)]
pub struct JobWorkerContext {
    /// Root of the durable job transcripts.
    pub transcript_root: PathBuf,
    /// Ids of the currently configured MCP principals; a prompt job whose
    /// operator submitter is not in this set fails with a `scope:` reason
    /// before any model call.
    pub configured_submitters: Arc<BTreeSet<String>>,
    /// Home and cwd of the job runtimes; `None` blocks every executed job with
    /// `"job runtime requires a HARW home"` before any model call.
    pub runtime_root: Option<JobRuntimeRoot>,
}

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
/// - `plan_services` (`Option<Arc<PlanNodeServices>>`): plan store, inherited
///   sandbox and plan actor; `None` disables plan-node execution.
/// - `context` (`Arc<JobWorkerContext>`): transcript root, configured
///   submitters and runtime root; only the pointer is cloned per job.
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
    plan_services: Option<Arc<PlanNodeServices>>,
    context: Arc<JobWorkerContext>,
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
        let job_store = Arc::clone(&store);
        let services = plan_services.as_ref().map(Arc::clone);
        let context = Arc::clone(&context);
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
                            job_store,
                            services,
                            operation_control,
                            context,
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
/// - `plan_services` (`Option<Arc<PlanNodeServices>>`): plan store, inherited
///   sandbox and plan actor; `None` disables plan-node execution.
/// - `shutdown` (`watch::Receiver<bool>`): set to `true` to end the loop.
/// - `context` (`Arc<JobWorkerContext>`): transcript root, configured
///   submitters and runtime root, handed to every poll; see
///   [`run_job_worker_once`].
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
    plan_services: Option<Arc<PlanNodeServices>>,
    mut shutdown: watch::Receiver<bool>,
    context: Arc<JobWorkerContext>,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        let _ = run_job_worker_once(
            Arc::clone(&store),
            Arc::clone(&executions),
            Arc::clone(&provider),
            plan_services.as_ref().map(Arc::clone),
            Arc::clone(&context),
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
    job_store: Arc<JobStore>,
    plan_services: Option<Arc<PlanNodeServices>>,
    control: Arc<WorkerExecutionControl>,
    context: Arc<JobWorkerContext>,
) -> JobOutcome {
    let outcome = if is_plan_node_kind(&claim.job.kind) {
        execute_plan_node_claim(
            claim,
            input,
            provider,
            job_store,
            plan_services,
            Arc::clone(&control),
            &context,
        )
        .await
    } else {
        execute_prompt_claim(
            claim,
            input,
            provider,
            job_store,
            Arc::clone(&control),
            &context,
        )
        .await
    };
    control.signal_completion();
    outcome
}

// The historical path: a trusted prompt string, no tools, no permissions —
// now assembled as `EntryKind::JobPrompt` (W2d-2 J1).
//
// P0.12 (Register F-123): Ohne diese Prüfungen hätte jeder `SubmitOwn`-Principal
// unbegrenzten Modellzugang auf Serverkosten. Deshalb gilt, in dieser
// Reihenfolge und jeweils fail closed:
// 1. Scope: Claim, Lease und `JobScope` müssen zum Worker- und
//    Principal-Kontext passen (`check_prompt_claim_scope`), sonst
//    `Failed{scope: …}` **ohne** Modellaufruf.
// 2. Prompt: eine fehlende oder leere Eingabe blockiert (`prompt_from_input`).
// 3. Budget: das Job-Budget wird auf die harten MCP-Obergrenzen gedeckelt
//    (`effective_prompt_budget`). Tokens werden nach jeder Modell-Runde aus der
//    gemeldeten Usage verbucht (`BudgetedModelProvider`); die Wanduhr läuft als
//    `tokio::time::timeout` um den ganzen Turn. Überschreitung ⇒
//    `Failed{budget: …}`, danach kein weiterer Modellaufruf.
// 4. Runtime (W2d-2 J1, E3): ohne HARW-Home `Blocked{MISSING_RUNTIME_ROOT}`;
//    ein Montagefehler endet als `Failed` — beides ohne Modellaufruf.
async fn execute_prompt_claim(
    claim: JobClaim,
    input: serde_json::Value,
    provider: Arc<dyn ModelProvider>,
    job_store: Arc<JobStore>,
    control: Arc<WorkerExecutionControl>,
    context: &JobWorkerContext,
) -> JobOutcome {
    let work_id = claim.job.id.as_str().to_owned();

    if let Err(reason) = check_prompt_claim_scope(&claim, &input, &context.configured_submitters)
    {
        tracing::warn!(work_id = %work_id, reason = %reason, "prompt job rejected before any model call");
        return JobOutcome::Failed { reason };
    }
    // `check_prompt_claim_scope` hat den Einreicher bereits als gültigen
    // Operator geprüft; der zweite Blick ist nur die fail-closed Entpackung.
    let submitter_id = match claim.scope.submitter() {
        harw_types::ApprovalActor::Operator { id } => id.to_owned(),
        harw_types::ApprovalActor::ChannelPeer { .. } => {
            return JobOutcome::Failed {
                reason: "scope: the job submitter has no valid operator id".to_owned(),
            };
        }
    };

    let prompt = match prompt_from_input(&input) {
        Ok(prompt) => prompt,
        Err(reason) => return JobOutcome::Blocked { reason },
    };

    let budget = effective_prompt_budget(&claim.job.budget);
    let wall = match prompt_wall_allowance(&budget, &claim.job.usage, &claim.lease, Timestamp::now())
    {
        Ok(wall) => wall,
        Err(reason) => {
            tracing::warn!(work_id = %work_id, reason = %reason, "prompt job has no wall-clock budget left");
            return JobOutcome::Failed { reason };
        }
    };

    let Some(runtime_root) = context.runtime_root.as_ref() else {
        tracing::warn!(work_id = %work_id, "prompt job claimed by a worker without a HARW home");
        return JobOutcome::Blocked {
            reason: MISSING_RUNTIME_ROOT.to_owned(),
        };
    };

    let ledger = PromptTokenLedger::new(budget, claim.job.usage.clone());
    let budgeted: Arc<dyn ModelProvider> = Arc::new(BudgetedModelProvider {
        inner: provider,
        ledger: Arc::clone(&ledger),
    });

    let assembled = assemble_job_turn(
        JobAssemblyInputs {
            entry: JobEntry::Prompt,
            home: &runtime_root.home,
            cwd: &runtime_root.cwd,
            principal: job_principal(&submitter_id),
            session_id: durable_session_id(&claim),
            state_store: job_state_store(&context.transcript_root),
            job_store,
            model: budgeted,
            narrowing: None,
        },
        PauseDisposition::Blocked,
        None,
    );
    let (setup, model) = match assembled {
        Ok(assembled) => assembled,
        Err(reason) => {
            tracing::error!(work_id = %work_id, reason = %reason, "prompt job runtime assembly failed");
            return JobOutcome::Failed { reason };
        }
    };

    let turn = execute_turn(claim, prompt, model, Arc::clone(&control), setup);
    match tokio::time::timeout(wall.duration, turn).await {
        // Ein Abbruch durch den Supervisor bleibt ein Abbruch; jeder andere
        // Ausgang eines Turns, der das Token-Budget gerissen hat, ist ein
        // Budget-Fehlschlag — auch wenn das Modell noch geantwortet hat.
        Ok(outcome @ JobOutcome::Cancelled { .. }) => outcome,
        Ok(outcome) => match ledger.exceeded() {
            Some(reason) => {
                tracing::warn!(work_id = %work_id, reason = %reason, "prompt job exceeded its token budget");
                JobOutcome::Failed { reason }
            }
            None => outcome,
        },
        Err(_elapsed) => {
            // Der Turn läuft in einem eigenen Task (`execute_turn`). Das
            // Verwerfen des Futures allein beendet ihn nicht: erst den Ledger
            // schließen (jede weitere Runde wird abgewiesen), dann den Task
            // hart abbrechen.
            ledger.close(wall.reason.clone());
            control.force_abort();
            control.clear_abort_handle();
            tracing::warn!(work_id = %work_id, reason = %wall.reason, "prompt job exceeded its wall-clock window");
            JobOutcome::Failed { reason: wall.reason }
        }
    }
}

/// Sicherheitsabstand zwischen dem Ende des Wanduhr-Fensters eines Prompt-Jobs
/// und dem Ablauf seiner Lease, damit `JobStore::complete` den Ausgang noch
/// unter gültiger Lease festschreiben kann (ohne Lease-Erneuerung, W4a).
const PROMPT_LEASE_COMMIT_MARGIN_SECONDS: i64 = 5;

// Scope-Durchsetzung für Prompt-Jobs (P0.12). Liefert `Err(reason)` mit dem
// Präfix `scope:`; Werte aus Scope oder Eingabe erscheinen bewusst nicht im
// Grund, weil der Grund über MCP an den Client zurückgeht.
//
// Worker-Kontext: `JobStore::claim` setzt `lease.holder` auf die `worker_id`
// der `ClaimRequest` und `token`/`lease` auf die geclaimte Work-ID. Ein Claim,
// der nicht genau diesem Worker und genau diesem Job gehört, wird nie
// ausgeführt.
//
// Principal-Kontext: Prompt-Jobs (`Worker`/`Dream`) werden in Produktion nur
// von `DurableMcpSupervisor::submit_job` zugelassen, das Tenant/Workspace aus
// dem authentifizierten `McpPrincipal` und als Einreicher dessen
// `ApprovalActor::Operator` setzt. Alles andere passt nicht zu diesem Pfad.
// Eine Eingabe, die selbst einen Tenant/Workspace adressiert, muss exakt dem
// serverseitig aufgelösten Scope entsprechen.
//
// Konfigurations-Kontext (W2d-1/C1, Restlücke aus W1-13): Der Einreicher muss
// zusätzlich ein *aktuell* konfigurierter MCP-Principal sein
// (`runtime_jobs::configured_principal_ids`, exakter Vergleich). Ein Job, dessen
// Principal seit der Einreichung aus der Config entfernt wurde, läuft nicht mehr.
fn check_prompt_claim_scope(
    claim: &JobClaim,
    input: &serde_json::Value,
    configured_submitters: &BTreeSet<String>,
) -> Result<(), String> {
    if claim.lease.holder != WORKER_ID {
        return Err("scope: the job lease is held by another worker".to_owned());
    }
    if claim.lease.work_id != claim.job.id || claim.token.work_id != claim.job.id {
        return Err("scope: the job lease names another job".to_owned());
    }
    if !is_scope_identifier(claim.scope.tenant().as_str()) {
        return Err("scope: the job tenant is not a valid identifier".to_owned());
    }
    if !is_scope_identifier(claim.scope.workspace().as_str()) {
        return Err("scope: the job workspace is not a valid identifier".to_owned());
    }
    match claim.scope.submitter() {
        harw_types::ApprovalActor::Operator { id } if is_scope_identifier(id) => {
            if !configured_submitters.contains(id) {
                return Err("scope: Einreicher ist kein konfigurierter MCP-Principal".to_owned());
            }
        }
        harw_types::ApprovalActor::Operator { .. } => {
            return Err("scope: the job submitter has no valid operator id".to_owned());
        }
        harw_types::ApprovalActor::ChannelPeer { .. } => {
            return Err(
                "scope: prompt jobs run only for authenticated MCP operators, not channel peers"
                    .to_owned(),
            );
        }
    }
    check_input_declared_scope(&claim.scope, input)
}

// Eine Eingabe darf den Scope nicht umlenken: `tenant`, `workspace` oder ein
// `scope`-Objekt müssen, falls vorhanden, exakt dem Scope des Jobs entsprechen.
// Eine Nicht-Objekt-Eingabe ist hier kein Scope-Fall; `prompt_from_input`
// blockiert sie danach.
fn check_input_declared_scope(
    scope: &harw_job_runtime::JobScope,
    input: &serde_json::Value,
) -> Result<(), String> {
    let Some(object) = input.as_object() else {
        return Ok(());
    };
    check_declared_scope_fields(scope, object, "job input")?;
    let Some(declared) = object.get("scope") else {
        return Ok(());
    };
    let Some(declared) = declared.as_object() else {
        return Err("scope: the job input declares a malformed scope".to_owned());
    };
    if declared
        .keys()
        .any(|key| !matches!(key.as_str(), "tenant" | "workspace"))
    {
        return Err("scope: the job input scope names an unknown field".to_owned());
    }
    check_declared_scope_fields(scope, declared, "job input scope")
}

fn check_declared_scope_fields(
    scope: &harw_job_runtime::JobScope,
    object: &serde_json::Map<String, serde_json::Value>,
    origin: &str,
) -> Result<(), String> {
    for (key, expected) in [
        ("tenant", scope.tenant().as_str()),
        ("workspace", scope.workspace().as_str()),
    ] {
        if let Some(declared) = object.get(key) {
            if declared.as_str() != Some(expected) {
                return Err(format!(
                    "scope: the {origin} declares a {key} other than the job's server-resolved {key}"
                ));
            }
        }
    }
    Ok(())
}

// Nicht leer, keine Rand-Leerzeichen, keine Steuerzeichen.
fn is_scope_identifier(value: &str) -> bool {
    !value.is_empty() && value.trim() == value && !value.chars().any(char::is_control)
}

// Deckelt das gespeicherte Budget eines Prompt-Jobs auf die harten
// MCP-Obergrenzen (`harw_mcp_server::supervisor::McpJobBudgetLimits`). Der
// Supervisor lehnt größere Budgets bereits ab; diese Deckelung schützt zusätzlich
// vor Altbeständen (`Budget::unbounded()` vor P0.12) und anderen Einreichern.
// Das Ergebnis hat nie ein `None`-Feld.
fn effective_prompt_budget(stored: &harw_job_runtime::Budget) -> harw_job_runtime::Budget {
    let ceiling = harw_mcp_server::supervisor::McpJobBudgetLimits::server_default();
    harw_job_runtime::Budget {
        max_tokens: Some(
            stored
                .max_tokens
                .map_or(ceiling.max_tokens(), |limit| limit.min(ceiling.max_tokens())),
        ),
        max_wall: Some(
            stored
                .max_wall
                .map_or(ceiling.max_wall(), |limit| limit.min(ceiling.max_wall())),
        ),
        max_tool_calls: Some(
            stored
                .max_tool_calls
                .map_or(ceiling.max_tool_calls(), |limit| {
                    limit.min(ceiling.max_tool_calls())
                }),
        ),
    }
}

/// Das Wanduhr-Fenster eines Prompt-Turns samt dem Grund, der bei Ablauf
/// gemeldet wird.
struct PromptWallAllowance {
    /// Maximale Laufzeit des Turns.
    duration: std::time::Duration,
    /// `Failed`-Grund bei Ablauf (`budget:` oder `lease:`).
    reason: String,
}

// Verbleibendes Wanduhr-Budget, zusätzlich begrenzt durch die verbleibende
// Lease-Gültigkeit abzüglich `PROMPT_LEASE_COMMIT_MARGIN_SECONDS`: ohne
// Lease-Erneuerung (W4a) könnte ein längerer Turn seinen Ausgang ohnehin nicht
// mehr festschreiben und bliebe als `Running` liegen.
fn prompt_wall_allowance(
    budget: &harw_job_runtime::Budget,
    usage: &harw_job_runtime::BudgetUsage,
    lease: &harw_job_runtime::Lease,
    now: Timestamp,
) -> Result<PromptWallAllowance, String> {
    let Some(max_wall) = budget.max_wall else {
        return Err("budget: the job has no wall-clock limit".to_owned());
    };
    let budget_left = max_wall.saturating_sub(usage.wall);
    if budget_left <= SignedDuration::ZERO {
        return Err("budget: the wall-clock budget is already exhausted".to_owned());
    }
    let lease_left = now
        .duration_until(lease.expires_at)
        .saturating_sub(SignedDuration::from_secs(PROMPT_LEASE_COMMIT_MARGIN_SECONDS));
    if lease_left <= SignedDuration::ZERO {
        return Err("lease: too little lease time is left to run the job".to_owned());
    }
    let (window, reason) = if lease_left < budget_left {
        (
            lease_left,
            format!(
                "lease: the turn exceeded the {} ms left on the job lease",
                lease_left.as_millis()
            ),
        )
    } else {
        (
            budget_left,
            format!(
                "budget: wall-clock limit of {} ms exceeded",
                budget_left.as_millis()
            ),
        )
    };
    let duration = std::time::Duration::try_from(window)
        .map_err(|_| "budget: the wall-clock window is not representable".to_owned())?;
    Ok(PromptWallAllowance { duration, reason })
}

/// Token-Buchhaltung eines Prompt-Jobs über alle Modell-Runden seines Turns.
///
/// # Description
/// Quelle ist die `ModelResponse::usage` jeder Runde — dieselbe Größe, die
/// `harw_core::turn_loop::drive_turn` in `total_usage` und damit in
/// `AgentSession::total_usage` aufsummiert. Gebucht wird mit
/// [`harw_job_runtime::Budget::charge_tokens`] über `TokenUsage::total()`
/// (Input + Output). Nach dem ersten Überschreiten ist der Ledger dauerhaft
/// geschlossen.
///
/// # Concurrency
/// Geteilt als `Arc`; der `Mutex` wird nie über ein `await` gehalten.
struct PromptTokenLedger {
    budget: harw_job_runtime::Budget,
    state: Mutex<PromptTokenState>,
}

struct PromptTokenState {
    usage: harw_job_runtime::BudgetUsage,
    exceeded: Option<String>,
}

impl PromptTokenLedger {
    fn new(budget: harw_job_runtime::Budget, usage: harw_job_runtime::BudgetUsage) -> Arc<Self> {
        Arc::new(Self {
            budget,
            state: Mutex::new(PromptTokenState {
                usage,
                exceeded: None,
            }),
        })
    }

    // Ein vergifteter Mutex hält nur Zählerstände; sie bleiben lesbar.
    fn state(&self) -> std::sync::MutexGuard<'_, PromptTokenState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    // Vor jeder Modell-Runde: ein geschlossener oder bereits voll ausgeschöpfter
    // Ledger lässt keinen weiteren Aufruf zu.
    fn admit_round(&self) -> Result<(), String> {
        let mut state = self.state();
        if let Some(reason) = &state.exceeded {
            return Err(reason.clone());
        }
        if let Some(limit) = self.budget.max_tokens {
            if state.usage.tokens >= limit {
                let reason = format!(
                    "budget: token limit of {limit} is exhausted; no further model round"
                );
                state.exceeded = Some(reason.clone());
                return Err(reason);
            }
        }
        Ok(())
    }

    // Nach jeder Modell-Runde: verbucht die gemeldete Usage.
    fn charge_round(&self, usage: &harw_types::TokenUsage) -> Result<(), String> {
        let mut state = self.state();
        if let Some(reason) = &state.exceeded {
            return Err(reason.clone());
        }
        match self.budget.charge_tokens(&mut state.usage, usage.total()) {
            Ok(()) => Ok(()),
            Err(error) => {
                let reason = format!("budget: {error}");
                state.exceeded = Some(reason.clone());
                Err(reason)
            }
        }
    }

    // Schließt den Ledger (z. B. nach Ablauf der Wanduhr); ein bereits
    // gesetzter Grund bleibt erhalten.
    fn close(&self, reason: String) {
        let mut state = self.state();
        if state.exceeded.is_none() {
            state.exceeded = Some(reason);
        }
    }

    fn exceeded(&self) -> Option<String> {
        self.state().exceeded.clone()
    }
}

/// Ein [`ModelProvider`], der jede Runde gegen einen [`PromptTokenLedger`]
/// prüft und verbucht.
///
/// # Description
/// Vor der Runde wird zugelassen oder abgewiesen, ohne den inneren Provider zu
/// berühren; nach der Runde wird die Usage verbucht. Überschreitet sie das
/// Budget, endet der Turn mit einem Modellfehler, damit weder Tool-Runden noch
/// weitere Modellaufrufe folgen. `execute_prompt_claim` ersetzt diesen Fehler
/// durch den `budget:`-Grund des Ledgers.
///
/// # Concurrency
/// `Send + Sync`: nur `Arc`s.
struct BudgetedModelProvider {
    inner: Arc<dyn ModelProvider>,
    ledger: Arc<PromptTokenLedger>,
}

impl ModelProvider for BudgetedModelProvider {
    fn respond<'a>(&'a self, request: harw_core::ModelRequest) -> harw_core::ModelFuture<'a> {
        Box::pin(async move {
            self.ledger
                .admit_round()
                .map_err(harw_core::ModelError::RequestFailed)?;
            let response = self.inner.respond(request).await?;
            self.ledger
                .charge_round(&response.usage)
                .map_err(harw_core::ModelError::RequestFailed)?;
            Ok::<harw_core::ModelResponse, harw_core::ModelError>(response)
        })
    }
}

// The plan-node path: typed payload, contract-derived permissions, role-derived
// registry profile (both applied as a `RuntimeNarrowing` of
// `EntryKind::JobPlanNode`), and a mandatory report back into the plan.
async fn execute_plan_node_claim(
    claim: JobClaim,
    input: serde_json::Value,
    provider: Arc<dyn ModelProvider>,
    job_store: Arc<JobStore>,
    plan_services: Option<Arc<PlanNodeServices>>,
    control: Arc<WorkerExecutionControl>,
    context: &JobWorkerContext,
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
    // The write *request* is the contract's: only a contract that names allowed
    // paths asks for `WriteWorkspace`. Whether it is granted is decided by the
    // derivation (plan-node table ∩ contract ∩ inherited) and read back below.
    // The derived sandbox is a *check* and the source of the narrowed
    // permissions; the session's sandbox itself is built by the assembly.
    let requests_write = !payload.contract.allowed_paths.is_empty();
    let sandbox = match derive_plan_node_sandbox(
        services.sandbox(),
        &payload.contract,
        kind,
        requests_write,
    ) {
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

    let Some(runtime_root) = context.runtime_root.as_ref() else {
        tracing::error!(
            task = %payload.task_id,
            work_id = %work_id,
            "plan-node job claimed by a worker without a HARW home"
        );
        return report_plan_node_outcome(
            &services,
            &payload.task_id,
            &work_id,
            JobOutcome::Blocked {
                reason: MISSING_RUNTIME_ROOT.to_owned(),
            },
        );
    };

    let (entry, narrowing) = plan_node_narrowing(&payload, kind, &sandbox);
    let profile = narrowing.registry_profile;
    let may_write = sandbox.permissions().contains(Permission::WriteWorkspace);
    let assembled = assemble_job_turn(
        JobAssemblyInputs {
            entry,
            home: &runtime_root.home,
            cwd: sandbox.workspace().canonical_root(),
            principal: job_principal(services.actor()),
            session_id: durable_session_id(&claim),
            state_store: job_state_store(&context.transcript_root),
            job_store,
            model: provider,
            narrowing: Some(narrowing),
        },
        PauseDisposition::Failed,
        // J1-F, kept as defense in depth: the narrowing binds the assembly's
        // sandbox to the derived workspace root (R0-F); this check only fires
        // on a genuine deviation.
        Some(&sandbox),
    );
    let (setup, model) = match assembled {
        Ok(assembled) => assembled,
        Err(reason) => {
            tracing::error!(
                task = %payload.task_id,
                work_id = %work_id,
                reason = %reason,
                "plan-node runtime assembly failed"
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

    let outcome = execute_turn(claim, plan_node_prompt(&payload), model, control, setup).await;

    report_plan_node_outcome(&services, &payload.task_id, &work_id, outcome)
}

// Fail-closed check of a plan node's assembled sandbox (J1-F). Since R0-F the
// narrowing carries `workspace_root`, so the assembly binds exactly the derived
// workspace root even when `discover_project` finds a marked project above it.
// The check stays as defense in depth: any deviation (a future assembly change,
// a path swapped between canonicalizations) fails closed. The workspace ids differ
// by construction (tenant alias), hence the canonical roots are compared, not
// `SandboxSpec::ensure_child_of`. Both roots are canonical already.
fn ensure_same_workspace_root(
    assembled: &SandboxSpec,
    derived: &SandboxSpec,
) -> Result<(), String> {
    let bound = assembled.workspace().canonical_root();
    let required = derived.workspace().canonical_root();
    if bound == required {
        Ok(())
    } else {
        Err(format!(
            "plan node workspace root mismatch: assembly bound {}, contract requires {}",
            bound.display(),
            required.display()
        ))
    }
}

// Job-Art und Verengung eines Plan-Knotens (CONTRACTS-W2d2.md §2 J1, E10):
// Profil nach Knotenart und tatsächlich gewährtem Schreibrecht, Identität des
// Plan-Knoten-Agenten, Rechte und Workspace-Root der abgeleiteten Sandbox (R0-F).
fn plan_node_narrowing(
    payload: &PlanNodePayload,
    kind: PlanNodeKind,
    sandbox: &SandboxSpec,
) -> (JobEntry, RuntimeNarrowing) {
    let may_write = sandbox.permissions().contains(Permission::WriteWorkspace);
    let profile = profile_for_node_kind(kind, may_write);
    (
        JobEntry::PlanNode { kind, may_write },
        RuntimeNarrowing {
            registry_profile: profile,
            identity: plan_node_identity(payload, profile),
            permissions: sandbox.permissions().clone(),
            // R0-F: bind the root sandbox to exactly the derived workspace root,
            // not to the project root discovered above it.
            workspace_root: Some(sandbox.workspace().canonical_root().to_path_buf()),
        },
    )
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
    /// The root session built by `RuntimeAssembly::new_root_session`.
    session: AgentSession,
    /// The durable history store the assembly was built with.
    state_store: Arc<dyn StateStore>,
    /// How a paused turn is scored.
    pause: PauseDisposition,
}

// The durable session id of a job: `durable-job-<work id>`. The same id is the
// assembly's registered root and the key of the job transcript.
fn durable_session_id(claim: &JobClaim) -> SessionId {
    SessionId::from_str(format!("durable-job-{}", claim.job.id.as_str()))
}

// The durable transcript store of all job sessions below `transcript_root`.
fn job_state_store(transcript_root: &Path) -> Arc<dyn StateStore> {
    Arc::new(TranscriptStateStore::new(
        TranscriptStore::new(transcript_root),
        job_thread,
    ))
}

// Assembles the job runtime and builds its root session (W2d-2 J1). The
// assembly is a local of this synchronous function and is dropped before any
// `await`: the root session owns its registry, and the model is handed out as
// a pointer. No responder is mounted — neither job entry has
// `AskResolution::Interactive`. The event receivers are dropped on purpose: a
// durable job has no live observer; its record is the transcript.
//
// `required_sandbox` (plan nodes only, J1-F): the assembled sandbox must be
// bound to exactly its workspace root, checked before any session exists.
//
// Returns the turn setup and the assembly's model, or the sanitized `Failed`
// reason of a failed assembly, workspace-root check or root session.
fn assemble_job_turn(
    inputs: JobAssemblyInputs<'_>,
    pause: PauseDisposition,
    required_sandbox: Option<&SandboxSpec>,
) -> Result<(TurnSetup, Arc<dyn ModelProvider>), String> {
    let session_id = inputs.session_id.clone();
    let state_store = Arc::clone(&inputs.state_store);
    let assembly: RuntimeAssembly = job_assembly(inputs).map_err(|error| {
        format!(
            "could not assemble the job runtime: {}",
            sanitize_failure(&error)
        )
    })?;
    if let Some(required) = required_sandbox {
        ensure_same_workspace_root(assembly.sandbox(), required)
            .map_err(|reason| sanitize_failure(&reason))?;
    }
    let model = Arc::clone(assembly.model());
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
    let (turn_tx, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();
    let root = assembly
        .new_root_session(session_id, event_tx, turn_tx, None)
        .map_err(|error| {
            format!(
                "could not create the job root session: {}",
                sanitize_failure(&error.to_string())
            )
        })?;
    Ok((
        TurnSetup {
            session: root.session,
            state_store,
            pause,
        },
        model,
    ))
}

async fn execute_turn(
    claim: JobClaim,
    prompt: String,
    provider: Arc<dyn ModelProvider>,
    control: Arc<WorkerExecutionControl>,
    setup: TurnSetup,
) -> JobOutcome {
    let TurnSetup {
        mut session,
        state_store,
        pause,
    } = setup;

    let session_id = durable_session_id(&claim);
    let durable_store = Arc::clone(&state_store);

    let mut turn = tokio::spawn(async move {
        run_turn(
            &mut session,
            provider.as_ref(),
            state_store.as_ref(),
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
            match durable_store.load_history(&session_id).await {
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
/// The permissions are not chosen freely from `inherited`.  They come from the
/// shared plan-node table (`crate::runtime_jobs::job_sandbox` with
/// [`crate::runtime_jobs::JobEntry::PlanNode`], i.e.
/// `harw_runtime::plan_node_sandbox`), are cut by the contract ceiling
/// ([`contract_permission_ceiling`]) and are never more than
/// `inherited.permissions()`:
///
/// `result ⊆ inherited ∩ plan-node table(kind, may_write) ∩ contract`.
///
/// The workspace binding stays the inherited one (the table sandbox is bound
/// to the same canonical root, but under its own tenant alias), and the
/// network scope is cut to the table's (empty) scope.  Every demand the
/// contract makes that `inherited` does not already hold is an error rather
/// than a grant, and a write scope that lexically leaves the workspace is
/// rejected, because such a path could never be a *reduction* of a
/// workspace-bound sandbox.
///
/// # Arguments
/// - `inherited` (`&SandboxSpec`): the authority the job itself holds.
/// - `contract` (`&MutationContract`): the node's mutation contract.
/// - `kind` (`PlanNodeKind`): the node's kind; non-writing kinds never keep
///   [`Permission::WriteWorkspace`].
/// - `may_write` (`bool`): whether the caller requests write access for this
///   node (the worker passes "the contract names allowed paths").
///
/// # Returns
/// The narrowed [`SandboxSpec`]; it always passes
/// [`SandboxSpec::ensure_child_of`] against `inherited`.
///
/// # Errors
/// Returns a human-readable reason when the contract names a path outside the
/// workspace, when it needs a permission the job does not hold, when the
/// plan-node table sandbox cannot be bound to the inherited workspace root, or
/// when the derived sandbox would not be a child of `inherited`.
///
/// # Concurrency
/// Synchronous; canonicalizes the inherited workspace root once (filesystem
/// read), otherwise pure.
fn derive_plan_node_sandbox(
    inherited: &SandboxSpec,
    contract: &MutationContract,
    kind: PlanNodeKind,
    may_write: bool,
) -> Result<SandboxSpec, String> {
    for rule in &contract.allowed_paths {
        let pattern = rule_pattern(rule);
        if leaves_workspace(pattern) {
            return Err(format!(
                "write scope '{pattern}' leaves the workspace; a plan contract may only narrow authority"
            ));
        }
    }

    // Befund W11 (Z2d-1-Review): diese Prüfung — Vertragsforderung ⊆ geerbte
    // Sandbox — läuft bewusst *vor* dem Knotenart-Schnitt weiter unten
    // (`job_sandbox`/`PlanNodeKind`-Tabelle) und ist fail-closed: sie sieht nur
    // die geerbte Sandbox, nicht das Zielprofil der Knotenart. Ein
    // Research-Knoten mit `allowed_paths` unter einer nur lesenden geerbten
    // Sandbox scheitert deshalb hier bereits mit einem Fehler, statt später
    // still auf `{Read}` zurückgeschnitten zu werden — Autorität wird nie
    // stillschweigend erweitert, ein zu weiter Vertrag wird abgelehnt statt
    // klammheimlich verengt.
    let ceiling = contract_permission_ceiling(contract);
    for permission in ceiling.iter() {
        if !inherited.permissions().contains(permission) {
            return Err(format!(
                "plan contract requires {permission:?}, which this job's sandbox does not grant; authority is only ever reduced"
            ));
        }
    }

    let root = inherited.workspace().canonical_root();
    let node = crate::runtime_jobs::job_sandbox(
        crate::runtime_jobs::JobEntry::PlanNode { kind, may_write },
        root,
    )
    .map_err(|error| {
        format!(
            "could not build the plan-node sandbox: {}",
            sanitize_failure(&error)
        )
    })?;

    // Table ∩ contract ∩ inherited on the permission axis; the inherited
    // binding is kept so the result is a child of `inherited`, and the network
    // scope is cut to the table's.
    let node_permissions = node
        .restrict(&ceiling)
        .restrict(inherited.permissions())
        .permissions()
        .clone();
    let derived = inherited.restrict_with(&node_permissions, node.network_scope());
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
    use harw_plan::{InMemoryPlanStore, PlanAction, PlanId, PlanNodeStatus};
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

    // Die Einreicher-Menge passend zu `ready_record` (Operator `operator`).
    fn operator_submitters() -> Arc<BTreeSet<String>> {
        Arc::new(BTreeSet::from(["operator".to_owned()]))
    }

    // Worker-Kontext mit Temp-Home (`harw_home::ensure_home`) und Temp-cwd
    // unter `root`; Transkripte liegen direkt unter `root`.
    fn job_context(root: &Path) -> Arc<JobWorkerContext> {
        Arc::new(JobWorkerContext {
            transcript_root: root.to_path_buf(),
            configured_submitters: operator_submitters(),
            runtime_root: Some(runtime_root_under(root)),
        })
    }

    // Legt Home und cwd der Job-Runtime unter `root` an.
    fn runtime_root_under(root: &Path) -> JobRuntimeRoot {
        let home = root.join("runtime-home");
        let cwd = root.join("runtime-cwd");
        if let Err(error) = harw_home::ensure_home(&home) {
            panic!("scaffold home: {error}");
        }
        if let Err(error) = std::fs::create_dir_all(&cwd) {
            panic!("create cwd: {error}");
        }
        JobRuntimeRoot { home, cwd }
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

        let derived = match derive_plan_node_sandbox(
            &inherited,
            &contract,
            PlanNodeKind::Coding,
            true,
        ) {
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

        let derived = match derive_plan_node_sandbox(
            &inherited,
            &contract,
            PlanNodeKind::Coding,
            false,
        ) {
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

        let error = match derive_plan_node_sandbox(
            &inherited,
            &contract,
            PlanNodeKind::Coding,
            true,
        ) {
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
            match derive_plan_node_sandbox(
                &inherited,
                &contract,
                PlanNodeKind::Coding,
                true,
            ) {
                Ok(_) => panic!("write scope '{pattern}' must be rejected"),
                Err(error) => assert!(
                    error.contains("leaves the workspace"),
                    "unclear message for '{pattern}': {error}"
                ),
            }
        }
    }

    #[test]
    fn test_derive_plan_node_sandbox_research_node_drops_write() {
        let dir = temp_dir();
        let inherited = sandbox_with(
            dir.path(),
            &[Permission::ReadWorkspace, Permission::WriteWorkspace],
        );
        // The contract asks for writing, but a research node never writes.
        let contract = contract_for("t-1", &[PathRule::DirectoryPrefix("src".to_owned())]);

        let derived =
            match derive_plan_node_sandbox(
                &inherited,
                &contract,
                PlanNodeKind::Research,
                true,
            ) {
                Ok(derived) => derived,
                Err(error) => panic!("derivation must succeed: {error}"),
            };
        assert!(derived.permissions().contains(Permission::ReadWorkspace));
        assert!(!derived.permissions().contains(Permission::WriteWorkspace));
        assert!(derived.ensure_child_of(&inherited).is_ok());
    }

    #[test]
    fn test_derive_plan_node_sandbox_never_exceeds_inherited() {
        let dir = temp_dir();
        let inherited = sandbox_with(
            dir.path(),
            &[
                Permission::ReadWorkspace,
                Permission::WriteWorkspace,
                Permission::ExecuteProcess,
                Permission::NetworkAccess,
                Permission::ReadSecrets,
            ],
        );
        let kinds = [
            PlanNodeKind::Research,
            PlanNodeKind::Explore,
            PlanNodeKind::Analysis,
            PlanNodeKind::Synthesis,
            PlanNodeKind::Contract,
            PlanNodeKind::Coding,
            PlanNodeKind::Integration,
            PlanNodeKind::Verification,
            PlanNodeKind::Docs,
            PlanNodeKind::Composite,
        ];
        let write_contract = contract_for("t-1", &[PathRule::Exact("src/lib.rs".to_owned())]);
        let read_contract = contract_for("t-1", &[]);
        for kind in kinds {
            for may_write in [false, true] {
                for contract in [&write_contract, &read_contract] {
                    let derived =
                        match derive_plan_node_sandbox(&inherited, contract, kind, may_write) {
                            Ok(derived) => derived,
                            Err(error) => panic!("derivation for {kind:?} must succeed: {error}"),
                        };
                    assert!(
                        derived
                            .permissions()
                            .is_subset_of(inherited.permissions()),
                        "{kind:?}/{may_write}: result exceeds the inherited sandbox"
                    );
                    assert!(derived.ensure_child_of(&inherited).is_ok());
                    assert_eq!(derived.workspace(), inherited.workspace());
                    assert!(derived.network_scope().is_empty());
                    for forbidden in [
                        Permission::ExecuteProcess,
                        Permission::NetworkAccess,
                        Permission::ReadSecrets,
                    ] {
                        assert!(
                            !derived.permissions().contains(forbidden),
                            "{kind:?}/{may_write}: {forbidden:?} must never survive"
                        );
                    }
                    let writes = derived.permissions().contains(Permission::WriteWorkspace);
                    let expected = may_write
                        && !contract.allowed_paths.is_empty()
                        && profile_for_node_kind(kind, true) == RegistryProfile::Full;
                    assert_eq!(writes, expected, "{kind:?}/{may_write}: write mismatch");
                }
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
            None,
            job_context(temp.path()),
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
            None,
            job_context(temp.path()),
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
            Some(services),
            job_context(temp.path()),
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
            None,
            job_context(temp.path()),
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
            Some(services),
            job_context(temp.path()),
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

    // ── Runtime assembly (W2d-2 J1) ───────────────────────────────────────

    #[tokio::test]
    async fn test_prompt_job_without_runtime_root_is_blocked_before_model_call() {
        let temp = temp_dir();
        let store = Arc::new(JobStore::new(temp.path()));
        admit(
            &store,
            &ready_record("homeless-job", serde_json::json!({"prompt": "hello"})),
        );
        let provider = Arc::new(RecordingModelProvider::new());
        let context = Arc::new(JobWorkerContext {
            transcript_root: temp.path().to_path_buf(),
            configured_submitters: operator_submitters(),
            runtime_root: None,
        });

        let completed = run_job_worker_once(
            Arc::clone(&store),
            Arc::new(JobExecutionRegistry::new()),
            Arc::clone(&provider) as Arc<dyn ModelProvider>,
            None,
            context,
        )
        .await;

        assert_eq!(completed, 1, "the job must reach a terminal state");
        assert!(
            provider.recorded().is_empty(),
            "a worker without a HARW home must never reach the model"
        );
        let JobOutcome::Blocked { reason } = completion_of(&store, "homeless-job") else {
            panic!("a prompt job without a runtime root must block");
        };
        assert_eq!(reason, "job runtime requires a HARW home");
    }

    #[tokio::test]
    async fn test_prompt_job_session_id_is_durable_job_id() {
        let temp = temp_dir();
        let store = Arc::new(JobStore::new(temp.path()));
        admit(
            &store,
            &ready_record("session-job", serde_json::json!({"prompt": "hello"})),
        );

        let completed = run_job_worker_once(
            Arc::clone(&store),
            Arc::new(JobExecutionRegistry::new()),
            Arc::new(EchoModelProvider::new("durable answer")),
            None,
            job_context(temp.path()),
        )
        .await;

        assert_eq!(completed, 1);
        assert!(matches!(
            completion_of(&store, "session-job"),
            JobOutcome::Succeeded { .. }
        ));
        // Der Verlauf liegt unter genau der Wurzel-Session-id `durable-job-<id>`,
        // die die Montage registriert und `new_root_session` akzeptiert hat.
        let history = match job_state_store(temp.path())
            .load_history(&SessionId::from_str("durable-job-session-job"))
            .await
        {
            Ok(history) => history,
            Err(error) => panic!("load durable job history: {error}"),
        };
        assert_eq!(last_assistant_text(&history), "durable answer");
    }

    #[test]
    fn test_plan_node_job_registry_is_narrowed_to_readonly_for_research() {
        let temp = temp_dir();
        let inherited = sandbox_with(
            temp.path(),
            &[Permission::ReadWorkspace, Permission::WriteWorkspace],
        );
        // The contract asks for writing, but a research node never writes.
        let contract = contract_for("t-1", &[PathRule::DirectoryPrefix("src".to_owned())]);
        let payload = match PlanNodePayload::parse(&plan_node_input(Some("t-1"), &contract)) {
            Ok(payload) => payload,
            Err(error) => panic!("bridge payload must parse: {error}"),
        };
        let derived = match derive_plan_node_sandbox(
            &inherited,
            &contract,
            PlanNodeKind::Research,
            true,
        ) {
            Ok(derived) => derived,
            Err(error) => panic!("derivation must succeed: {error}"),
        };

        let (entry, narrowing) = plan_node_narrowing(&payload, PlanNodeKind::Research, &derived);
        assert_eq!(
            entry,
            JobEntry::PlanNode {
                kind: PlanNodeKind::Research,
                may_write: false,
            }
        );
        assert_eq!(narrowing.registry_profile, RegistryProfile::ReadOnlyExplore);
        assert!(!narrowing.permissions.contains(Permission::WriteWorkspace));

        let runtime = runtime_root_under(temp.path());
        let assembly = match job_assembly(JobAssemblyInputs {
            entry,
            home: &runtime.home,
            cwd: derived.workspace().canonical_root(),
            principal: job_principal("test-runtime"),
            session_id: SessionId::from_str("durable-job-research"),
            state_store: job_state_store(temp.path()),
            job_store: Arc::new(JobStore::new(temp.path())),
            model: Arc::new(EchoModelProvider::new("x")),
            narrowing: Some(narrowing),
        }) {
            Ok(assembly) => assembly,
            Err(error) => panic!("narrowed plan-node assembly must build: {error}"),
        };

        let tools = assembly.rights_snapshot().tools;
        assert!(tools.iter().any(|tool| tool == "fs.read"), "tools: {tools:?}");
        for forbidden in ["fs.write", "shell.exec"] {
            assert!(
                !tools.iter().any(|tool| tool == forbidden),
                "a research node must not see '{forbidden}': {tools:?}"
            );
        }
        let permissions = assembly.sandbox().permissions();
        assert!(permissions.contains(Permission::ReadWorkspace));
        assert!(!permissions.contains(Permission::WriteWorkspace));
        assert!(!permissions.contains(Permission::ExecuteProcess));
    }

    // ── Workspace-root check (W2d-2 J1-F) ─────────────────────────────────

    // Binds a sandbox to `harness_root/relative` (`"."` binds the harness root
    // itself), like `harw_runtime::sandbox::root_sandbox` does for a project.
    fn sandbox_bound_to(
        harness_root: &Path,
        relative: &str,
        tenant: &str,
        permissions: &[Permission],
    ) -> SandboxSpec {
        if let Err(error) = std::fs::create_dir_all(harness_root.join(relative)) {
            panic!("create workspace '{relative}': {error}");
        }
        let tenant = TenantId::from_str(tenant);
        let workspace = WorkspaceId::from_str("workspace");
        let registry = match WorkspaceRegistry::build(
            harness_root,
            [WorkspaceRegistration {
                tenant: tenant.clone(),
                workspace: workspace.clone(),
                root: PathBuf::from(relative),
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

    #[test]
    fn test_ensure_same_workspace_root_rejects_parent_root() {
        let temp = temp_dir();
        // The derived (contract) sandbox is bound to the child directory; the
        // assembly bound its parent — a wider directory, even with fewer rights.
        let read_write = [Permission::ReadWorkspace, Permission::WriteWorkspace];
        let derived = sandbox_bound_to(temp.path(), "workspace", "tenant", &read_write);
        let read_only = [Permission::ReadWorkspace];
        let assembled = sandbox_bound_to(temp.path(), ".", "job-plan-node", &read_only);
        assert_ne!(
            assembled.workspace().canonical_root(),
            derived.workspace().canonical_root()
        );

        let reason = match ensure_same_workspace_root(&assembled, &derived) {
            Ok(()) => panic!("a parent root must never pass as the derived workspace"),
            Err(reason) => reason,
        };
        assert!(
            reason.starts_with("plan node workspace root mismatch: assembly bound "),
            "unclear message: {reason}"
        );
        assert!(
            reason.contains(&derived.workspace().canonical_root().display().to_string()),
            "the required root must be named: {reason}"
        );
    }

    #[test]
    fn test_ensure_same_workspace_root_accepts_identical_root() {
        let temp = temp_dir();
        // Different tenant alias and permissions, same canonical root: the
        // check is about the bound directory only.
        let read_only = [Permission::ReadWorkspace];
        let derived = sandbox_bound_to(temp.path(), "workspace", "tenant", &read_only);
        let assembled = sandbox_bound_to(temp.path(), "workspace", "job-plan-node", &read_only);

        assert_eq!(ensure_same_workspace_root(&assembled, &derived), Ok(()));
    }

    #[tokio::test]
    async fn test_plan_node_under_a_marked_parent_directory_binds_the_workspace_root() {
        let temp = temp_dir();
        // A project marker *above* the inherited workspace (`<temp>/workspace`):
        // `discover_project` resolves `<temp>` as the project root. Since R0-F
        // the narrowing binds the sandbox to `<temp>/workspace` regardless.
        if let Err(error) = std::fs::write(temp.path().join("Cargo.toml"), "[workspace]\n") {
            panic!("write project marker: {error}");
        }
        let store = Arc::new(JobStore::new(temp.path()));
        let contract = contract_for("t-1", &[]);
        admit(
            &store,
            &ready_record_of_kind(
                "marked-parent-job",
                JobKind::Custom(PLAN_NODE_JOB_KIND.to_owned()),
                plan_node_input(Some("t-1"), &contract),
            ),
        );
        let plan = InMemoryPlanStore::new();
        if let Err(error) = plan.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-test"),
                goal: "test goal".to_owned(),
            },
            "test",
        ) {
            panic!("create plan: {error}");
        }
        let mut node = harw_plan::testing::base_node("t-1");
        node.kind = PlanNodeKind::Research;
        node.status = PlanNodeStatus::Ready;
        if let Err(error) = plan.apply(PlanAction::AddNode { node }, "test") {
            panic!("add node: {error}");
        }
        let plan: Arc<dyn PlanStore> = Arc::new(plan);
        let inherited = sandbox_with(temp.path(), &[Permission::ReadWorkspace]);
        let services = Arc::new(PlanNodeServices::new(
            plan,
            inherited.clone(),
            "test-runtime".to_owned(),
        ));
        let provider = Arc::new(RecordingModelProvider::new());

        let completed = run_job_worker_once(
            Arc::clone(&store),
            Arc::new(JobExecutionRegistry::new()),
            Arc::clone(&provider) as Arc<dyn ModelProvider>,
            Some(services),
            job_context(temp.path()),
        )
        .await;

        assert_eq!(completed, 1, "the plan job must reach a terminal state");
        assert!(
            !provider.recorded().is_empty(),
            "a plan node below a marked parent directory must now reach the model"
        );
        match completion_of(&store, "marked-parent-job") {
            JobOutcome::Failed { reason } | JobOutcome::Blocked { reason } => assert!(
                !reason.starts_with("plan node workspace root mismatch")
                    && !reason.starts_with("could not assemble the job runtime"),
                "the assembly must bind the workspace root: {reason}"
            ),
            JobOutcome::Succeeded { .. } | JobOutcome::Cancelled { .. } => {}
        }

        // The bound root itself: the same narrowing the worker uses binds the
        // assembly's sandbox (and spawn context) to exactly the derived root,
        // while project discovery still stops at the marked parent.
        let payload = match PlanNodePayload::parse(&plan_node_input(Some("t-1"), &contract)) {
            Ok(payload) => payload,
            Err(error) => panic!("bridge payload must parse: {error}"),
        };
        let derived =
            match derive_plan_node_sandbox(&inherited, &contract, PlanNodeKind::Research, false) {
                Ok(derived) => derived,
                Err(error) => panic!("derivation must succeed: {error}"),
            };
        let (entry, narrowing) = plan_node_narrowing(&payload, PlanNodeKind::Research, &derived);
        assert_eq!(
            narrowing.workspace_root.as_deref(),
            Some(derived.workspace().canonical_root())
        );
        let runtime = runtime_root_under(temp.path());
        let assembly = match job_assembly(JobAssemblyInputs {
            entry,
            home: &runtime.home,
            cwd: derived.workspace().canonical_root(),
            principal: job_principal("test-runtime"),
            session_id: SessionId::from_str("durable-job-marked-parent"),
            state_store: job_state_store(temp.path()),
            job_store: Arc::new(JobStore::new(temp.path())),
            model: Arc::new(EchoModelProvider::new("x")),
            narrowing: Some(narrowing),
        }) {
            Ok(assembly) => assembly,
            Err(error) => panic!("plan-node assembly below a marked parent must build: {error}"),
        };
        let canonical_temp = match temp.path().canonicalize() {
            Ok(path) => path,
            Err(error) => panic!("canonicalize temp: {error}"),
        };
        assert_eq!(assembly.project().project_root, canonical_temp);
        assert_eq!(
            assembly.sandbox().workspace().canonical_root(),
            derived.workspace().canonical_root(),
            "the assembly binds the workspace root, not the marked parent"
        );
        assert_eq!(
            assembly.spawn_context().sandbox.workspace().canonical_root(),
            derived.workspace().canonical_root()
        );
        assert_eq!(
            ensure_same_workspace_root(assembly.sandbox(), &derived),
            Ok(())
        );
    }
}

#[cfg(test)]
mod prompt_claim_guard_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use harw_core::{EchoModelProvider, RecordingModelProvider};
    use harw_job_runtime::{Budget, BudgetUsage, Job, JobScope, Lease, RetryPolicy, StoredJob};
    use harw_mcp_server::supervisor::McpJobBudgetLimits;
    use harw_types::{ApprovalActor, ChannelId, PeerId, TenantId, TokenUsage, WorkId, WorkspaceId};

    // ── Fixtures ──────────────────────────────────────────────────────────

    fn temp_dir() -> tempfile::TempDir {
        match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("temp directory: {error}"),
        }
    }

    fn operator_scope() -> JobScope {
        JobScope::new(
            TenantId::from_str("tenant"),
            WorkspaceId::from_str("workspace"),
            ApprovalActor::Operator {
                id: "operator".to_owned(),
            },
        )
    }

    fn record(id: &str, input: serde_json::Value, budget: Budget, scope: JobScope) -> StoredJob {
        let now = Timestamp::now();
        let mut job = Job::new(
            WorkId::from_str(id),
            JobKind::Worker,
            budget,
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
            scope,
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

    fn failure_reason(store: &JobStore, id: &str) -> String {
        let stored = match store.get(&WorkId::from_str(id)) {
            Ok(stored) => stored,
            Err(error) => panic!("get '{id}': {error}"),
        };
        match stored.completion.map(|completion| completion.outcome) {
            Some(JobOutcome::Failed { reason }) => reason,
            other => panic!("job '{id}' must have failed, got {other:?}"),
        }
    }

    async fn run_once(store: &Arc<JobStore>, root: &Path, provider: Arc<dyn ModelProvider>) {
        run_job_worker_once(
            Arc::clone(store),
            Arc::new(JobExecutionRegistry::new()),
            provider,
            None,
            job_context_with_submitters(root, operator_submitters()),
        )
        .await;
    }

    // Die Einreicher-Menge passend zu `operator_scope` (Operator `operator`).
    fn operator_submitters() -> Arc<BTreeSet<String>> {
        Arc::new(BTreeSet::from(["operator".to_owned()]))
    }

    // Worker-Kontext mit Temp-Home (`harw_home::ensure_home`) und Temp-cwd
    // unter `root`; Transkripte liegen direkt unter `root`.
    fn job_context_with_submitters(
        root: &Path,
        configured_submitters: Arc<BTreeSet<String>>,
    ) -> Arc<JobWorkerContext> {
        let home = root.join("runtime-home");
        let cwd = root.join("runtime-cwd");
        if let Err(error) = harw_home::ensure_home(&home) {
            panic!("scaffold home: {error}");
        }
        if let Err(error) = std::fs::create_dir_all(&cwd) {
            panic!("create cwd: {error}");
        }
        Arc::new(JobWorkerContext {
            transcript_root: root.to_path_buf(),
            configured_submitters,
            runtime_root: Some(JobRuntimeRoot { home, cwd }),
        })
    }

    // Claimt einen zugelassenen Job als dieser Worker (`WORKER_ID`).
    fn claim_as_worker(store: &JobStore, id: &str) -> JobClaim {
        match store.claim(
            &WorkId::from_str(id),
            &ClaimRequest {
                worker_id: WORKER_ID.to_owned(),
                lease_ttl: SignedDuration::from_secs(LEASE_TTL_SECONDS),
                now: Timestamp::now(),
            },
        ) {
            Ok(claim) => claim,
            Err(error) => panic!("claim '{id}': {error}"),
        }
    }

    fn empty_request() -> harw_core::ModelRequest {
        harw_core::ModelRequest::new(
            harw_extension_api::LoadedInstructions {
                system_prompt: String::new(),
                fragments: Vec::new(),
            },
            Vec::new(),
            harw_core::ConversationHistory::new(),
            Vec::new(),
        )
    }

    /// Antwortet sofort mit fester Usage und zählt die Aufrufe.
    struct FixedUsageProvider {
        calls: AtomicUsize,
        tokens_per_call: u64,
    }

    impl ModelProvider for FixedUsageProvider {
        fn respond<'a>(&'a self, _request: harw_core::ModelRequest) -> harw_core::ModelFuture<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut response = harw_core::ModelResponse::text("fixed");
            response.usage = TokenUsage {
                input_tokens: self.tokens_per_call,
                output_tokens: 0,
                reasoning_tokens: None,
                cached_tokens: None,
            };
            Box::pin(async move { Ok(response) })
        }
    }

    /// Antwortet nie und zählt die Aufrufe.
    struct NeverRespondingProvider {
        calls: Arc<AtomicUsize>,
    }

    impl ModelProvider for NeverRespondingProvider {
        fn respond<'a>(&'a self, _request: harw_core::ModelRequest) -> harw_core::ModelFuture<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending::<
                Result<harw_core::ModelResponse, harw_core::ModelError>,
            >())
        }
    }

    // ── Scope ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_prompt_job_with_input_addressing_another_workspace_fails_without_model_call() {
        let temp = temp_dir();
        let store = Arc::new(JobStore::new(temp.path()));
        admit(
            &store,
            &record(
                "foreign-workspace",
                serde_json::json!({"task": "summarize", "workspace": "other-workspace"}),
                Budget::unbounded(),
                operator_scope(),
            ),
        );
        admit(
            &store,
            &record(
                "foreign-scope",
                serde_json::json!({"task": "summarize", "scope": {"tenant": "other-tenant"}}),
                Budget::unbounded(),
                operator_scope(),
            ),
        );
        let provider = Arc::new(RecordingModelProvider::new());

        run_once(&store, temp.path(), Arc::clone(&provider) as Arc<dyn ModelProvider>).await;

        assert!(
            provider.recorded().is_empty(),
            "a scope mismatch must never reach the model"
        );
        for id in ["foreign-workspace", "foreign-scope"] {
            let reason = failure_reason(&store, id);
            assert!(reason.starts_with("scope:"), "{id}: {reason}");
            assert!(!reason.contains("other-"), "reason must not echo input: {reason}");
        }
    }

    #[tokio::test]
    async fn test_prompt_job_with_matching_declared_scope_still_runs() {
        let temp = temp_dir();
        let store = Arc::new(JobStore::new(temp.path()));
        admit(
            &store,
            &record(
                "matching-scope",
                serde_json::json!({
                    "task": "summarize",
                    "workspace": "workspace",
                    "scope": {"tenant": "tenant", "workspace": "workspace"}
                }),
                Budget::unbounded(),
                operator_scope(),
            ),
        );
        let provider = Arc::new(RecordingModelProvider::new());

        run_once(&store, temp.path(), Arc::clone(&provider) as Arc<dyn ModelProvider>).await;

        assert_eq!(provider.recorded().len(), 1);
        let stored = match store.get(&WorkId::from_str("matching-scope")) {
            Ok(stored) => stored,
            Err(error) => panic!("get: {error}"),
        };
        assert!(matches!(
            stored.completion.map(|completion| completion.outcome),
            Some(JobOutcome::Succeeded { .. })
        ));
    }

    #[tokio::test]
    async fn test_prompt_job_from_a_channel_peer_or_malformed_scope_fails_without_model_call() {
        let temp = temp_dir();
        let store = Arc::new(JobStore::new(temp.path()));
        admit(
            &store,
            &record(
                "channel-peer",
                serde_json::json!({"task": "summarize"}),
                Budget::unbounded(),
                JobScope::new(
                    TenantId::from_str("tenant"),
                    WorkspaceId::from_str("workspace"),
                    ApprovalActor::ChannelPeer {
                        channel: ChannelId::from_str("telegram"),
                        peer: PeerId::from_str("peer"),
                    },
                ),
            ),
        );
        admit(
            &store,
            &record(
                "padded-workspace",
                serde_json::json!({"task": "summarize"}),
                Budget::unbounded(),
                JobScope::new(
                    TenantId::from_str("tenant"),
                    // Leere IDs weist schon die Deserialisierung ab; ein
                    // Rand-Leerzeichen erreicht den Worker und passt nicht.
                    WorkspaceId::from_str(" workspace"),
                    ApprovalActor::Operator {
                        id: "operator".to_owned(),
                    },
                ),
            ),
        );
        let provider = Arc::new(RecordingModelProvider::new());

        run_once(&store, temp.path(), Arc::clone(&provider) as Arc<dyn ModelProvider>).await;

        assert!(provider.recorded().is_empty());
        for id in ["channel-peer", "padded-workspace"] {
            let reason = failure_reason(&store, id);
            assert!(reason.starts_with("scope:"), "{id}: {reason}");
        }
    }

    #[tokio::test]
    async fn test_prompt_claim_held_by_another_worker_fails_without_model_call() {
        let temp = temp_dir();
        let store = JobStore::new(temp.path());
        admit(
            &store,
            &record(
                "foreign-lease",
                serde_json::json!({"task": "summarize"}),
                Budget::unbounded(),
                operator_scope(),
            ),
        );
        let claim = match store.claim(
            &WorkId::from_str("foreign-lease"),
            &ClaimRequest {
                worker_id: "some-other-worker".to_owned(),
                lease_ttl: SignedDuration::from_secs(LEASE_TTL_SECONDS),
                now: Timestamp::now(),
            },
        ) {
            Ok(claim) => claim,
            Err(error) => panic!("claim: {error}"),
        };
        let provider = Arc::new(RecordingModelProvider::new());

        let outcome = execute_prompt_claim(
            claim,
            serde_json::json!({"task": "summarize"}),
            Arc::clone(&provider) as Arc<dyn ModelProvider>,
            Arc::new(JobStore::new(temp.path())),
            WorkerExecutionControl::new(),
            &job_context_with_submitters(temp.path(), operator_submitters()),
        )
        .await;

        assert!(provider.recorded().is_empty());
        match outcome {
            JobOutcome::Failed { reason } => assert!(reason.starts_with("scope:"), "{reason}"),
            other => panic!("a foreign claim must fail, got {other:?}"),
        }
    }

    #[test]
    fn test_check_prompt_claim_scope_rejects_unconfigured_submitter() {
        let temp = temp_dir();
        let store = JobStore::new(temp.path());
        let input = serde_json::json!({"task": "summarize"});
        admit(
            &store,
            &record("unconfigured", input.clone(), Budget::unbounded(), operator_scope()),
        );
        let claim = claim_as_worker(&store, "unconfigured");

        for submitters in [
            BTreeSet::new(),
            BTreeSet::from(["other-operator".to_owned()]),
            // Exakter Vergleich: keine Normalisierung von Groß/Klein oder Rand.
            BTreeSet::from(["Operator".to_owned(), " operator".to_owned()]),
        ] {
            assert_eq!(
                check_prompt_claim_scope(&claim, &input, &submitters),
                Err("scope: Einreicher ist kein konfigurierter MCP-Principal".to_owned()),
                "submitters: {submitters:?}"
            );
        }
    }

    #[test]
    fn test_check_prompt_claim_scope_accepts_configured_submitter() {
        let temp = temp_dir();
        let store = JobStore::new(temp.path());
        let input = serde_json::json!({"task": "summarize"});
        admit(
            &store,
            &record("configured", input.clone(), Budget::unbounded(), operator_scope()),
        );
        let claim = claim_as_worker(&store, "configured");
        let submitters = BTreeSet::from(["alpha".to_owned(), "operator".to_owned()]);

        assert_eq!(check_prompt_claim_scope(&claim, &input, &submitters), Ok(()));
    }

    #[tokio::test]
    async fn test_run_job_worker_once_unconfigured_submitter_fails_without_model_call() {
        let temp = temp_dir();
        let store = Arc::new(JobStore::new(temp.path()));
        admit(
            &store,
            &record(
                "removed-principal",
                serde_json::json!({"prompt": "hello"}),
                Budget::unbounded(),
                operator_scope(),
            ),
        );
        let provider = Arc::new(RecordingModelProvider::new());

        let completed = run_job_worker_once(
            Arc::clone(&store),
            Arc::new(JobExecutionRegistry::new()),
            Arc::clone(&provider) as Arc<dyn ModelProvider>,
            None,
            job_context_with_submitters(
                temp.path(),
                Arc::new(BTreeSet::from(["someone-else".to_owned()])),
            ),
        )
        .await;

        assert_eq!(completed, 1, "the rejected job must reach a terminal state");
        assert_eq!(
            provider.recorded().len(),
            0,
            "an unconfigured submitter must never reach the model"
        );
        assert_eq!(
            failure_reason(&store, "removed-principal"),
            "scope: Einreicher ist kein konfigurierter MCP-Principal"
        );
    }

    // ── Budget ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_prompt_job_over_its_token_budget_fails_with_a_budget_reason() {
        let temp = temp_dir();
        let store = Arc::new(JobStore::new(temp.path()));
        admit(
            &store,
            &record(
                "token-overrun",
                serde_json::json!({"prompt": "hello"}),
                Budget {
                    max_tokens: Some(1),
                    max_wall: None,
                    max_tool_calls: None,
                },
                operator_scope(),
            ),
        );

        // `EchoModelProvider` meldet je Aufruf mindestens 1 Input- und 1
        // Output-Token (`approx(..).max(1)`), also mehr als das Limit 1.
        run_once(&store, temp.path(), Arc::new(EchoModelProvider::new("done"))).await;

        let reason = failure_reason(&store, "token-overrun");
        assert!(reason.starts_with("budget:"), "{reason}");
        assert!(reason.contains("tokens"), "{reason}");
    }

    #[tokio::test]
    async fn test_token_ledger_refuses_every_model_round_after_an_overrun() {
        let inner = Arc::new(FixedUsageProvider {
            calls: AtomicUsize::new(0),
            tokens_per_call: 10,
        });
        let ledger = PromptTokenLedger::new(
            Budget {
                max_tokens: Some(15),
                max_wall: None,
                max_tool_calls: None,
            },
            BudgetUsage::default(),
        );
        let budgeted = BudgetedModelProvider {
            inner: Arc::clone(&inner) as Arc<dyn ModelProvider>,
            ledger: Arc::clone(&ledger),
        };

        assert!(budgeted.respond(empty_request()).await.is_ok());
        assert!(ledger.exceeded().is_none());
        assert!(
            budgeted.respond(empty_request()).await.is_err(),
            "20 tokens exceed the limit of 15"
        );
        let Some(reason) = ledger.exceeded() else {
            panic!("the overrun must close the ledger");
        };
        assert!(reason.starts_with("budget:"), "{reason}");
        assert!(budgeted.respond(empty_request()).await.is_err());
        assert_eq!(
            inner.calls.load(Ordering::SeqCst),
            2,
            "a closed ledger must never reach the inner provider"
        );
    }

    #[tokio::test]
    async fn test_token_ledger_refuses_a_round_once_the_limit_is_reached_exactly() {
        let inner = Arc::new(FixedUsageProvider {
            calls: AtomicUsize::new(0),
            tokens_per_call: 10,
        });
        let ledger = PromptTokenLedger::new(
            Budget {
                max_tokens: Some(10),
                max_wall: None,
                max_tool_calls: None,
            },
            BudgetUsage::default(),
        );
        let budgeted = BudgetedModelProvider {
            inner: Arc::clone(&inner) as Arc<dyn ModelProvider>,
            ledger: Arc::clone(&ledger),
        };

        assert!(budgeted.respond(empty_request()).await.is_ok());
        assert!(budgeted.respond(empty_request()).await.is_err());
        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
        assert!(ledger.exceeded().is_some());
    }

    #[tokio::test]
    async fn test_prompt_job_over_its_wall_clock_budget_fails_and_stops_the_turn() {
        let temp = temp_dir();
        let store = Arc::new(JobStore::new(temp.path()));
        admit(
            &store,
            &record(
                "wall-overrun",
                serde_json::json!({"prompt": "hang"}),
                Budget {
                    max_tokens: None,
                    max_wall: Some(SignedDuration::from_millis(50)),
                    max_tool_calls: None,
                },
                operator_scope(),
            ),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(NeverRespondingProvider {
            calls: Arc::clone(&calls),
        });

        let finished = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            run_once(&store, temp.path(), provider),
        )
        .await;
        assert!(finished.is_ok(), "the wall-clock budget must end the job");

        let reason = failure_reason(&store, "wall-overrun");
        assert!(reason.starts_with("budget:"), "{reason}");
        assert!(reason.contains("wall-clock"), "{reason}");
        let calls_at_failure = calls.load(Ordering::SeqCst);
        assert!(calls_at_failure <= 1, "{calls_at_failure}");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            calls_at_failure,
            "the aborted turn must not call the model again"
        );
    }

    #[test]
    fn test_effective_prompt_budget_never_exceeds_the_mcp_ceiling() {
        let ceiling = McpJobBudgetLimits::server_default();

        assert_eq!(
            effective_prompt_budget(&Budget::unbounded()),
            ceiling.as_budget()
        );
        assert_eq!(
            effective_prompt_budget(&Budget {
                max_tokens: Some(u64::MAX),
                max_wall: Some(SignedDuration::from_hours(24)),
                max_tool_calls: Some(u32::MAX),
            }),
            ceiling.as_budget()
        );
        let small = Budget {
            max_tokens: Some(7),
            max_wall: Some(SignedDuration::from_secs(3)),
            max_tool_calls: Some(1),
        };
        assert_eq!(effective_prompt_budget(&small), small);
    }

    #[test]
    fn test_prompt_wall_allowance_is_bounded_by_budget_and_lease() {
        let now = Timestamp::now();
        let lease = match Lease::acquire(
            WorkId::from_str("wall"),
            WORKER_ID,
            now,
            SignedDuration::from_secs(120),
        ) {
            Ok(lease) => lease,
            Err(error) => panic!("lease: {error}"),
        };
        let budget = |wall: SignedDuration| Budget {
            max_tokens: Some(1),
            max_wall: Some(wall),
            max_tool_calls: Some(1),
        };

        let short = match prompt_wall_allowance(
            &budget(SignedDuration::from_secs(10)),
            &BudgetUsage::default(),
            &lease,
            now,
        ) {
            Ok(allowance) => allowance,
            Err(reason) => panic!("{reason}"),
        };
        assert_eq!(short.duration, std::time::Duration::from_secs(10));
        assert!(short.reason.starts_with("budget:"), "{}", short.reason);

        let long = match prompt_wall_allowance(
            &budget(SignedDuration::from_secs(600)),
            &BudgetUsage::default(),
            &lease,
            now,
        ) {
            Ok(allowance) => allowance,
            Err(reason) => panic!("{reason}"),
        };
        assert_eq!(
            long.duration,
            std::time::Duration::from_secs((120 - PROMPT_LEASE_COMMIT_MARGIN_SECONDS) as u64)
        );
        assert!(long.reason.starts_with("lease:"), "{}", long.reason);

        let spent = BudgetUsage {
            tokens: 0,
            wall: SignedDuration::from_secs(10),
            tool_calls: 0,
        };
        let exhausted = prompt_wall_allowance(
            &budget(SignedDuration::from_secs(10)),
            &spent,
            &lease,
            now,
        );
        assert!(matches!(exhausted, Err(ref reason) if reason.starts_with("budget:")));

        let expired = prompt_wall_allowance(
            &budget(SignedDuration::from_secs(10)),
            &BudgetUsage::default(),
            &lease,
            lease.expires_at,
        );
        assert!(matches!(expired, Err(ref reason) if reason.starts_with("lease:")));
    }
}
