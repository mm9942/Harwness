//! `intent_cycle.*` — start, observe and stop a durable read-only intent
//! cycle (PL-90 W03 production wiring).
//!
//! # Operations
//! | Name | Domain | Tier | Model tool | Web |
//! |---|---|---|---|---|
//! | `intent_cycle.start` | execution | operator | `approval = "always"` | `POST /api/intent-cycle/start`, `approval = "always"` |
//! | `intent_cycle.status` | execution | observer | `readonly`, `approval = "none"` | `GET /api/intent-cycle/status` |
//! | `intent_cycle.stop` | execution | operator | `approval = "always"` | `POST /api/intent-cycle/stop`, `approval = "always"` |
//!
//! Like `work_driver.*` there is no command surface: a cycle is started by
//! an agent (or the Web UI on its behalf) and every start asks a human.
//!
//! # What a cycle is
//! A [`ReadOnlyCycleSpec`]: an objective, a closed set of read targets
//! (`id -> workspace-relative path`) and one deterministic content check per
//! acceptance criterion. `intent_cycle.start` validates the spec with exactly
//! the code the job worker uses again on every claim
//! ([`ReadOnlyCycleSpec::plan`]), binds it to a revision-1 intent whose
//! digest covers every approved field, and admits a `Ready` job of kind
//! [`INTENT_CYCLE_JOB_KIND`]. The worker then drives
//! `harw_plan_bridge::CycleDriver` with the model proposer, the read-only
//! explorer and the content verifier. The model can only name target and
//! evidence ids; admission, reads, verification and the checkpoint fence are
//! trusted code.
//!
//! # Outcome
//! A cycle whose criteria are all covered by `Verified` evidence ends as a
//! succeeded job with `terminal = completion_proposed` and
//! `requires_owner_acceptance = true` — never as an accepted goal.
//! `Escalated`/`Blocked` end `Blocked`, `Failed` ends `Failed`.
//!
//! # Status
//! `intent_cycle.status` reads the job and, once the worker created it, the
//! bounded [`harw_plan_bridge::CycleStatus`] projection of the cycle record
//! under `<job-root>/`[`harw_plan_bridge::CYCLE_STORE_DIR`] — counts,
//! covered criteria, terminal state; no evidence bodies, no file content.

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

use harw_job_runtime::{Budget, Job, JobKind, JobScope, RetryPolicy, StoredJob, WorkId};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_plan_bridge::{
    ContentCheck, CycleLimits, CycleStore, DEFAULT_CYCLE_LIMITS, INTENT_CYCLE_JOB_KIND,
    ReadOnlyCycleSpec,
};
use harw_session_store::{CancelRequest, JobStore, SessionStoreError};
use harw_types::{ApprovalActor, Principal, TenantId};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

/// Schema version of [`IntentCycleJobInput`].
pub const INTENT_CYCLE_INPUT_SCHEMA_VERSION: u32 = 1;

/// Wall-clock budget of one cycle job.
const INTENT_CYCLE_MAX_WALL_SECS: i64 = 30 * 60;

/// Attempts: two (the original claim plus one reclaim). Consumed wall time and
/// tokens are persisted lease-fenced by `JobStore::record_usage` and flushed by
/// the worker's token ledger, so a reclaim after a lost lease resumes the
/// fenced checkpoint with the already consumed budget rather than a fresh one.
/// Usage whose write failed is left as a durable marker: a reclaim first
/// settles any unresolved usage marker, and an unsettleable marker fails the
/// job instead of granting fresh budget.
/// Provider outages are still retried inside the claim by the job worker; a
/// crash with the second attempt spent leaves the cycle non-terminal for an
/// explicit operator `/retry`.
const INTENT_CYCLE_MAX_ATTEMPTS: u32 = 2;

/// Upper bound (characters) of an acceptance criterion name.
const MAX_CRITERION_NAME_CHARS: usize = 128;

/// Upper bound (characters) of the cancellation reason.
const MAX_STOP_REASON_CHARS: usize = 200;

/// Default cancellation reason of `intent_cycle.stop`.
const DEFAULT_STOP_REASON: &str = "stopped through intent_cycle.stop";

// ── Arguments ────────────────────────────────────────────────────────────────

/// One read target of `intent_cycle.start`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadTargetArg {
    /// Id the model uses to name the target (`[A-Za-z0-9-_./:@#]`).
    pub id: String,
    /// Workspace-relative file path.
    pub path: String,
}

/// One acceptance criterion with its deterministic check.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceArg {
    /// Criterion name.
    pub criterion: String,
    /// Read target id whose content is checked.
    pub target: String,
    /// Substrings that must occur.
    #[serde(default)]
    pub must_contain: Vec<String>,
    /// Substrings that must not occur.
    #[serde(default)]
    pub must_not_contain: Vec<String>,
}

/// Narrowing-only limit overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CycleLimitArgs {
    #[serde(default)]
    pub max_transitions: Option<u32>,
    #[serde(default)]
    pub max_parallel_segments: Option<u16>,
    #[serde(default)]
    pub max_stall_transitions: Option<u32>,
}

/// Arguments of `intent_cycle.start`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentCycleStartArgs {
    #[serde(default)]
    pub objective: Option<String>,
    #[serde(default)]
    pub read_targets: Vec<ReadTargetArg>,
    #[serde(default)]
    pub acceptance: Vec<AcceptanceArg>,
    #[serde(default)]
    pub limits: Option<CycleLimitArgs>,
    /// Optional token budget of the whole cycle.
    #[serde(default)]
    pub token_budget: Option<u64>,
}

impl harw_operations::FromRawArgs for IntentCycleStartArgs {
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Err(no_command_surface("intent_cycle.start"))
    }
}

impl harw_operations::OpArgsSchema for IntentCycleStartArgs {
    fn json_schema() -> harw_tools::JsonSchema {
        use harw_operations::op_schema::{
            array_schema, described_object_schema, integer_schema, string_schema,
        };
        let target = described_object_schema(
            "A file the cycle may read, named by an id.",
            vec![
                (
                    "id",
                    string_schema("Target id (letters, digits and - _ . / : @ #)."),
                ),
                ("path", string_schema("Workspace-relative file path.")),
            ],
            &["id", "path"],
        );
        let criterion = described_object_schema(
            "An acceptance criterion with a deterministic content check.",
            vec![
                ("criterion", string_schema("Criterion name.")),
                ("target", string_schema("Read target id that is checked.")),
                (
                    "must_contain",
                    array_schema(string_schema("Substring that must occur.")),
                ),
                (
                    "must_not_contain",
                    array_schema(string_schema("Substring that must not occur.")),
                ),
            ],
            &["criterion", "target"],
        );
        let limits = described_object_schema(
            "Optional limits; each may only LOWER the default.",
            vec![
                ("max_transitions", integer_schema("Transition budget.")),
                (
                    "max_parallel_segments",
                    integer_schema("Reads per transition."),
                ),
                (
                    "max_stall_transitions",
                    integer_schema("Transitions without progress before the cycle stops."),
                ),
            ],
            &[],
        );
        described_object_schema(
            "Starts a durable, read-only intent cycle: the model explores the named files, \
             a deterministic verifier checks each acceptance criterion. Returns the job id.",
            vec![
                (
                    "objective",
                    string_schema("What the cycle should establish."),
                ),
                ("read_targets", array_schema(target)),
                ("acceptance", array_schema(criterion)),
                ("limits", limits),
                ("token_budget", integer_schema("Token budget of the cycle.")),
            ],
            &["objective", "read_targets", "acceptance"],
        )
    }
}

/// Arguments of `intent_cycle.status`.
#[derive(Debug, Default, Deserialize, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct IntentCycleStatusArgs {
    /// Job id returned by `intent_cycle.start`.
    #[serde(default)]
    pub job_id: Option<String>,
}

impl harw_operations::FromRawArgs for IntentCycleStatusArgs {
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Err(no_command_surface("intent_cycle.status"))
    }
}

/// Arguments of `intent_cycle.stop`.
#[derive(Debug, Default, Deserialize, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct IntentCycleStopArgs {
    /// Job id returned by `intent_cycle.start`.
    #[serde(default)]
    pub job_id: Option<String>,
    /// Optional cancellation reason.
    #[serde(default)]
    pub reason: Option<String>,
}

impl harw_operations::FromRawArgs for IntentCycleStopArgs {
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Err(no_command_surface("intent_cycle.stop"))
    }
}

fn no_command_surface(name: &str) -> OpError {
    OpError::NotAvailable(format!(
        "`{name}` has no command surface; it is a model tool"
    ))
}

// ── Payload ──────────────────────────────────────────────────────────────────

/// Typed input of an intent-cycle job (`StoredJob::input`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentCycleJobInput {
    /// [`INTENT_CYCLE_INPUT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Id of the cycle record (equals the job id).
    pub cycle_id: String,
    /// Id of the revision-1 intent.
    pub intent_id: String,
    /// The approved spec.
    pub spec: ReadOnlyCycleSpec,
    /// Principal id that requested the cycle.
    pub requested_by: String,
    /// Tenant scope of the caller (H12), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<TenantId>,
    /// Canonical workspace root the cycle was approved for (the caller's
    /// sandbox binding). The job worker refuses to run the cycle against
    /// any other root; `None` is refused as well (fail closed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<std::path::PathBuf>,
}

impl IntentCycleJobInput {
    /// Decodes the input of a stored intent-cycle job.
    ///
    /// # Errors
    /// [`io::ErrorKind::InvalidData`] for another kind or a schema mismatch.
    pub fn from_job(record: &StoredJob) -> io::Result<Self> {
        if !is_intent_cycle_job(record) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "job {} is not an {INTENT_CYCLE_JOB_KIND} job",
                    record.job.id
                ),
            ));
        }
        let input: Self = serde_json::from_value(record.input.clone())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if input.schema_version != INTENT_CYCLE_INPUT_SCHEMA_VERSION
            || input.cycle_id != record.job.id.as_str()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "intent cycle input does not belong to this job",
            ));
        }
        Ok(input)
    }
}

/// `true` for a job of kind [`INTENT_CYCLE_JOB_KIND`].
#[must_use]
pub fn is_intent_cycle_job(record: &StoredJob) -> bool {
    matches!(&record.job.kind, JobKind::Custom(name) if name == INTENT_CYCLE_JOB_KIND)
}

/// Builds the spec from the arguments (narrowing limits only).
///
/// # Errors
/// [`OpError::InvalidArguments`] for duplicates, widening limits and every
/// [`ReadOnlyCycleSpec::plan`] refusal.
pub fn spec_from_args(
    args: &IntentCycleStartArgs,
    intent_id: &str,
) -> Result<ReadOnlyCycleSpec, OpError> {
    let invalid = |message: String| OpError::InvalidArguments(message);
    let objective = args
        .objective
        .as_deref()
        .map(str::trim)
        .filter(|objective| !objective.is_empty())
        .ok_or_else(|| invalid("objective is required".to_owned()))?
        .to_owned();
    let mut read_targets = BTreeMap::new();
    for target in &args.read_targets {
        if read_targets
            .insert(target.id.clone(), target.path.clone())
            .is_some()
        {
            return Err(invalid(format!(
                "read target `{}` is named twice",
                target.id
            )));
        }
    }
    let mut acceptance = BTreeMap::new();
    for criterion in &args.acceptance {
        if criterion.criterion.chars().count() > MAX_CRITERION_NAME_CHARS {
            return Err(invalid(format!(
                "acceptance criterion names are limited to {MAX_CRITERION_NAME_CHARS} characters"
            )));
        }
        let check = ContentCheck {
            target: criterion.target.clone(),
            must_contain: criterion.must_contain.clone(),
            must_not_contain: criterion.must_not_contain.clone(),
        };
        if acceptance
            .insert(criterion.criterion.clone(), check)
            .is_some()
        {
            return Err(invalid(format!(
                "acceptance criterion `{}` is named twice",
                criterion.criterion
            )));
        }
    }
    let limits = narrow_limits(args.limits.as_ref().unwrap_or(&CycleLimitArgs::default()))?;
    let spec = ReadOnlyCycleSpec::new(objective, read_targets, acceptance, Some(limits));
    spec.plan(intent_id)
        .map_err(|error| invalid(error.to_string()))?;
    Ok(spec)
}

fn narrow_limits(args: &CycleLimitArgs) -> Result<CycleLimits, OpError> {
    let base = DEFAULT_CYCLE_LIMITS;
    let lower = |field: &str, value: Option<u64>, limit: u64| -> Result<u64, OpError> {
        match value {
            None => Ok(limit),
            Some(value) if (1..=limit).contains(&value) => Ok(value),
            Some(value) => Err(OpError::InvalidArguments(format!(
                "limits.{field} = {value} must be between 1 and {limit}; limits may only be lowered"
            ))),
        }
    };
    let transitions = lower(
        "max_transitions",
        args.max_transitions.map(u64::from),
        u64::from(base.max_transitions),
    )?;
    let parallel = lower(
        "max_parallel_segments",
        args.max_parallel_segments.map(u64::from),
        u64::from(base.max_parallel_segments),
    )?;
    let stall = lower(
        "max_stall_transitions",
        args.max_stall_transitions.map(u64::from),
        u64::from(base.max_stall_transitions),
    )?;
    Ok(CycleLimits {
        max_transitions: u32::try_from(transitions).unwrap_or(base.max_transitions),
        max_parallel_segments: u16::try_from(parallel).unwrap_or(base.max_parallel_segments),
        max_stall_transitions: u32::try_from(stall).unwrap_or(base.max_stall_transitions),
        ..base
    })
}

// ── Operations ───────────────────────────────────────────────────────────────

/// Starts a durable read-only intent cycle.
///
/// # Errors
/// - [`OpError::NotAvailable`]: no job store or principal.
/// - [`OpError::InvalidArguments`]: an invalid spec.
/// - [`OpError::Execution`]: the job cannot be admitted.
#[operation(
    name = "intent_cycle.start",
    summary = "Startet einen dauerhaften, nur lesenden Intent-Zyklus mit deterministischer Abnahmeprüfung.",
    domain = "execution",
    permission = "operator",
    model_tool(approval = "always"),
    // Startet dauerhafte Arbeit mit Modellbudget: immer bestätigungspflichtig.
    web(path = "/api/intent-cycle/start", method = "post", approval = "always")
)]
async fn intent_cycle_start(
    ctx: &OpContext,
    args: IntentCycleStartArgs,
) -> Result<OpOutput, OpError> {
    let jobs = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;
    let principal = crate::plan::require_principal(ctx)?;
    let work_id = WorkId::new();
    let intent_id = format!("intent-{}", work_id.as_str());
    let spec = spec_from_args(&args, &intent_id)?;
    if args.token_budget == Some(0) {
        return Err(OpError::InvalidArguments(
            "token_budget must be at least 1".to_owned(),
        ));
    }
    let input = IntentCycleJobInput {
        schema_version: INTENT_CYCLE_INPUT_SCHEMA_VERSION,
        cycle_id: work_id.as_str().to_owned(),
        intent_id,
        spec,
        requested_by: principal.id().to_owned(),
        tenant: ctx.tenant().cloned(),
        workspace_root: Some(ctx.sandbox().workspace().canonical_root().to_path_buf()),
    };
    let record = admission_record(ctx, principal, work_id, &input, args.token_budget)?;
    let job_id = record.job.id.clone();
    jobs.admit(&record).map_err(|error| {
        OpError::Execution(format!("could not admit the intent_cycle job: {error}"))
    })?;
    Ok(OpOutput {
        text: format!(
            "Intent cycle job {job_id} admitted ({} read targets, {} acceptance criteria, max {} \
             transitions). Observe it with intent_cycle.status.",
            input.spec.read_targets.len(),
            input.spec.acceptance.len(),
            input.spec.limits.max_transitions,
        ),
        data: Some(serde_json::json!({
            "job_id": job_id.as_str(),
            "kind": INTENT_CYCLE_JOB_KIND,
            "intent_id": input.intent_id,
            "intent_digest": input.spec.digest().to_string(),
        })),
    })
}

fn admission_record(
    ctx: &OpContext,
    principal: &Principal,
    work_id: WorkId,
    input: &IntentCycleJobInput,
    token_budget: Option<u64>,
) -> Result<StoredJob, OpError> {
    let now = Timestamp::now();
    let budget = Budget {
        max_tokens: token_budget,
        max_wall: Some(SignedDuration::from_secs(INTENT_CYCLE_MAX_WALL_SECS)),
        max_tool_calls: None,
    };
    let mut job = Job::new(
        work_id,
        JobKind::Custom(INTENT_CYCLE_JOB_KIND.to_owned()),
        budget,
        RetryPolicy {
            max_attempts: INTENT_CYCLE_MAX_ATTEMPTS,
            base_delay: SignedDuration::from_secs(30),
            factor: 2.0,
            max_delay: SignedDuration::from_secs(300),
            jitter: 0.0,
        },
        now,
    );
    job.mark_ready(now)
        .map_err(|error| OpError::Execution(format!("intent_cycle job: {error}")))?;
    let binding = ctx.sandbox().workspace();
    let payload = serde_json::to_value(input)
        .map_err(|error| OpError::Execution(format!("intent_cycle payload: {error}")))?;
    Ok(StoredJob {
        job,
        scope: JobScope::new(
            binding.tenant().clone(),
            binding.workspace().clone(),
            ApprovalActor::Operator {
                id: principal.id().to_owned(),
            },
        ),
        input: payload,
        submitted_at: now,
        not_before: now,
        lease: None,
        lease_epoch: 0,
        completion: None,
        cancellation: None,
        revision: 0,
        trace: None,
    })
}

fn load_cycle_job(
    ctx: &OpContext,
    jobs: &JobStore,
    raw: Option<&str>,
) -> Result<StoredJob, OpError> {
    let raw = raw
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| OpError::InvalidArguments("job_id is required".to_owned()))?;
    let work_id = WorkId::try_from_str(raw)
        .map_err(|_| OpError::InvalidArguments(format!("malformed job id `{raw}`")))?;
    let record = match crate::job_tenant::get_bound_workspace_job(ctx, jobs, &work_id) {
        Ok(record) => record,
        Err(SessionStoreError::JobNotFound { .. }) => {
            return Err(OpError::InvalidArguments(format!(
                "unknown intent_cycle job `{raw}`"
            )));
        }
        Err(error) => {
            return Err(OpError::Execution(format!(
                "could not read job `{raw}`: {error}"
            )));
        }
    };
    if !is_intent_cycle_job(&record) {
        return Err(OpError::InvalidArguments(format!(
            "job `{raw}` is not an intent_cycle job"
        )));
    }
    Ok(record)
}

/// Reports an intent-cycle job and its bounded cycle status.
///
/// # Errors
/// - [`OpError::NotAvailable`]: no job store.
/// - [`OpError::InvalidArguments`]: missing, unknown or foreign job id.
/// - [`OpError::Execution`]: store failures.
#[operation(
    name = "intent_cycle.status",
    summary = "Zeigt Zustand, Fortschritt, abgedeckte Kriterien und Endzustand eines Intent-Zyklus.",
    domain = "execution",
    permission = "observer",
    model_tool(readonly, approval = "none"),
    // Reines Lesen von Job und begrenzter Zyklus-Projektion.
    web(path = "/api/intent-cycle/status", method = "get", approval = "none")
)]
async fn intent_cycle_status(
    ctx: &OpContext,
    args: IntentCycleStatusArgs,
) -> Result<OpOutput, OpError> {
    let jobs = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;
    let record = load_cycle_job(ctx, jobs, args.job_id.as_deref())?;
    let input = IntentCycleJobInput::from_job(&record).ok();
    let status = match CycleStore::open_existing_in_job_root(jobs.root()) {
        Ok(Some(store)) => store
            .load(record.job.id.as_str())
            .ok()
            .map(|cycle| cycle.status()),
        Ok(None) => None,
        Err(error) => {
            return Err(OpError::Execution(format!(
                "could not open the cycle store: {error}"
            )));
        }
    };
    let mut lines = vec![format!(
        "Intent cycle job {} — {:?}",
        record.job.id, record.job.state
    )];
    if let Some(input) = &input {
        lines.push(format!(
            "objective: {}",
            input.spec.objective.chars().take(200).collect::<String>()
        ));
    }
    match &status {
        None => lines.push("cycle not started yet".to_owned()),
        Some(status) => {
            lines.push(format!(
                "sequence {} (epoch {}), transitions {}, stall {}, evidence {}",
                status.sequence,
                status.epoch,
                status.transitions_used,
                status.stall_transitions,
                status.evidence
            ));
            lines.push(format!(
                "pending observations {}, dropped {}",
                status.pending_observations, status.observations_dropped
            ));
            lines.push(format!(
                "criteria covered: {}",
                if status.criteria_covered.is_empty() {
                    "none".to_owned()
                } else {
                    status.criteria_covered.join(", ")
                }
            ));
            if let Some(terminal) = status.terminal {
                lines.push(format!("terminal: {terminal:?}"));
            }
        }
    }
    Ok(OpOutput {
        text: lines.join("\n"),
        data: Some(serde_json::json!({
            "job_id": record.job.id.as_str(),
            "job_state": record.job.state,
            "intent_id": input.as_ref().map(|input| input.intent_id.clone()),
            "acceptance": input.as_ref().map(|input| input.spec.acceptance.keys().cloned().collect::<Vec<_>>()),
            "cycle": status,
        })),
    })
}

/// Stops an intent-cycle job through the durable job cancel path; the
/// driver's lease is fenced, a journaled step is reconciled on resume.
///
/// # Errors
/// - [`OpError::NotAvailable`]: no job store.
/// - [`OpError::InvalidArguments`]: missing, unknown or foreign job id.
/// - [`OpError::Execution`]: the store refuses the cancellation.
#[operation(
    name = "intent_cycle.stop",
    summary = "Bricht einen Intent-Zyklus über den dauerhaften Job-Abbruch ab.",
    domain = "execution",
    permission = "operator",
    model_tool(approval = "always"),
    // Irreversibel (Cancelled ist terminal): bestätigungspflichtig wie `stop`.
    web(path = "/api/intent-cycle/stop", method = "post", approval = "always")
)]
async fn intent_cycle_stop(
    ctx: &OpContext,
    args: IntentCycleStopArgs,
) -> Result<OpOutput, OpError> {
    let jobs = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;
    let principal = crate::plan::require_principal(ctx)?;
    let record = load_cycle_job(ctx, jobs, args.job_id.as_deref())?;
    let reason: String = args
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .unwrap_or(DEFAULT_STOP_REASON)
        .chars()
        .take(MAX_STOP_REASON_CHARS)
        .collect();
    let transition = jobs
        .cancel(
            &record.job.id,
            &CancelRequest {
                cancelled_at: Timestamp::now(),
                cancelled_by: ApprovalActor::Operator {
                    id: principal.id().to_owned(),
                },
                reason,
            },
        )
        .map_err(|error| {
            OpError::Execution(format!(
                "could not stop intent_cycle job `{}`: {error}",
                record.job.id
            ))
        })?;
    Ok(OpOutput {
        text: format!(
            "Stopped intent cycle job {} (was {:?}).",
            transition.work_id, transition.previous_state
        ),
        data: Some(serde_json::json!({
            "job_id": transition.work_id.as_str(),
            "previous_state": transition.previous_state,
            "revision": transition.revision,
        })),
    })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_job_runtime::LeaseToken;
    use harw_operations::context::ServiceMap;
    use harw_operations::{OpInput, Operation};
    use harw_plan_bridge::{
        CauseCode, CycleCheckpoint, CycleObservations, CycleProposal, CycleRecord,
        ObservationCategory, admit_cycle, apply_observations, build_observation,
    };
    use harw_types::{
        IngressSurface, PermissionTier, PrincipalKind, SessionId, TurnId, WorkspaceId,
    };

    struct Fixture {
        _dir: tempfile::TempDir,
        ctx: OpContext,
        jobs: Arc<JobStore>,
    }

    fn fixture() -> TestResult<Fixture> {
        fixture_scoped(None)
    }

    /// Like [`fixture`]; the context is scoped to `tenant` (H12) if given.
    fn fixture_scoped(tenant: Option<&str>) -> TestResult<Fixture> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::create_dir_all(dir.path().join("ws")).map_err(ctx("workspace"))?;
        let registry = WorkspaceRegistry::build(
            dir.path(),
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("workspace registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("resolve workspace"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        let jobs = Arc::new(JobStore::new(&dir.path().join("state")));
        let mut services = ServiceMap::new();
        services.insert(Arc::clone(&jobs));
        services.insert(Principal::trusted_ingress(
            PrincipalKind::Model,
            "model:explorer",
            IngressSurface::Cli,
            PermissionTier::Operator,
        ));
        let op_ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, services);
        let op_ctx = match tenant {
            Some(tenant) => op_ctx.with_tenant(TenantId::from_str(tenant)),
            None => op_ctx,
        };
        Ok(Fixture {
            _dir: dir,
            ctx: op_ctx,
            jobs,
        })
    }

    fn start_args() -> serde_json::Value {
        serde_json::json!({
            "objective": "does lib.rs define the marker?",
            "read_targets": [{"id": "lib", "path": "src/lib.rs"}],
            "acceptance": [{
                "criterion": "marker-defined",
                "target": "lib",
                "must_contain": ["fn marker"],
            }],
            "limits": {"max_transitions": 8},
        })
    }

    async fn run(
        op: &dyn Operation,
        ctx: &OpContext,
        args: serde_json::Value,
    ) -> Result<OpOutput, OpError> {
        op.run(ctx, OpInput::model_tool(args)).await
    }

    fn job_id(output: &OpOutput) -> TestResult<WorkId> {
        let raw = output
            .data
            .as_ref()
            .and_then(|data| data.get("job_id"))
            .and_then(serde_json::Value::as_str)
            .ok_or(TestError::Missing("job_id"))?;
        WorkId::try_from_str(raw).map_err(|_| TestError::Missing("valid job id"))
    }

    #[tokio::test]
    async fn start_admits_a_ready_job_with_the_validated_spec() -> TestResult {
        let fx = fixture()?;
        let output = run(&IntentCycleStartOperation, &fx.ctx, start_args())
            .await
            .map_err(ctx("start"))?;
        let id = job_id(&output)?;
        let record = fx.jobs.get(&id).map_err(ctx("job"))?;
        assert!(is_intent_cycle_job(&record));
        let input = IntentCycleJobInput::from_job(&record).map_err(ctx("input"))?;
        assert_eq!(input.cycle_id, id.as_str());
        assert_eq!(input.spec.limits.max_transitions, 8);
        assert!(input.spec.plan(&input.intent_id).is_ok());
        assert_eq!(record.job.retry.max_attempts, INTENT_CYCLE_MAX_ATTEMPTS);
        Ok(())
    }

    #[tokio::test]
    async fn retry_policy_allows_one_reclaim() -> TestResult {
        let fx = fixture()?;
        let output = run(&IntentCycleStartOperation, &fx.ctx, start_args())
            .await
            .map_err(ctx("start"))?;
        let id = job_id(&output)?;
        let record = fx.jobs.get(&id).map_err(ctx("job"))?;
        assert_eq!(record.job.retry.max_attempts, 2);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_specs_are_refused_before_admission() -> TestResult {
        let fx = fixture()?;
        let mut widened = start_args();
        widened["limits"]["max_transitions"] = serde_json::json!(1_000);
        let mut unchecked = start_args();
        unchecked["acceptance"][0]["target"] = serde_json::json!("other");
        let mut escape = start_args();
        escape["read_targets"][0]["path"] = serde_json::json!("../etc/passwd");
        let mut smuggled = start_args();
        smuggled["permission"] = serde_json::json!("full_access");
        for args in [widened, unchecked, escape, smuggled] {
            let result = run(&IntentCycleStartOperation, &fx.ctx, args).await;
            assert!(
                matches!(result, Err(OpError::InvalidArguments(_))),
                "{result:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn status_before_the_worker_ran_and_stop_cancels() -> TestResult {
        let fx = fixture()?;
        let output = run(&IntentCycleStartOperation, &fx.ctx, start_args())
            .await
            .map_err(ctx("start"))?;
        let id = job_id(&output)?;
        let status = run(
            &IntentCycleStatusOperation,
            &fx.ctx,
            serde_json::json!({"job_id": id.as_str()}),
        )
        .await
        .map_err(ctx("status"))?;
        assert!(
            status.text.contains("cycle not started yet"),
            "{}",
            status.text
        );
        let stopped = run(
            &IntentCycleStopOperation,
            &fx.ctx,
            serde_json::json!({"job_id": id.as_str()}),
        )
        .await
        .map_err(ctx("stop"))?;
        assert!(stopped.text.contains("Stopped"));
        let record = fx.jobs.get(&id).map_err(ctx("job"))?;
        assert_eq!(record.job.state, harw_job_runtime::JobState::Cancelled);
        Ok(())
    }

    #[tokio::test]
    async fn zero_token_budget_and_duplicates_are_refused() -> TestResult {
        let fx = fixture()?;
        let mut zero = start_args();
        zero["token_budget"] = serde_json::json!(0);
        let mut duplicate_target = start_args();
        duplicate_target["read_targets"] = serde_json::json!([
            {"id": "lib", "path": "src/lib.rs"},
            {"id": "lib", "path": "src/other.rs"},
        ]);
        let mut duplicate_criterion = start_args();
        duplicate_criterion["acceptance"] = serde_json::json!([
            {"criterion": "c", "target": "lib", "must_contain": ["a"]},
            {"criterion": "c", "target": "lib", "must_contain": ["b"]},
        ]);
        for args in [zero, duplicate_target, duplicate_criterion] {
            let result = run(&IntentCycleStartOperation, &fx.ctx, args).await;
            assert!(
                matches!(result, Err(OpError::InvalidArguments(_))),
                "{result:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn start_stamps_the_tenant_and_foreign_jobs_stay_hidden() -> TestResult {
        let fx = fixture_scoped(Some("test-tenant"))?;
        let output = run(&IntentCycleStartOperation, &fx.ctx, start_args())
            .await
            .map_err(ctx("start"))?;
        let id = job_id(&output)?;
        let record = fx.jobs.get(&id).map_err(ctx("job"))?;
        let input = IntentCycleJobInput::from_job(&record).map_err(ctx("input"))?;
        assert_eq!(input.tenant, Some(TenantId::from_str("test-tenant")));

        // A job of another tenant, admitted directly into the same store.
        let mut foreign = admission_record(
            &fx.ctx,
            &Principal::trusted_ingress(
                PrincipalKind::Model,
                "model:explorer",
                IngressSurface::Cli,
                PermissionTier::Operator,
            ),
            WorkId::new(),
            &input,
            None,
        )
        .map_err(ctx("foreign record"))?;
        foreign.scope = JobScope::new(
            TenantId::from_str("other-tenant"),
            WorkspaceId::from_str("ws"),
            ApprovalActor::Operator {
                id: "other".to_owned(),
            },
        );
        let foreign_id = foreign.job.id.clone();
        fx.jobs.admit(&foreign).map_err(ctx("admit foreign"))?;

        let args = serde_json::json!({"job_id": foreign_id.as_str()});
        let status = run(&IntentCycleStatusOperation, &fx.ctx, args.clone()).await;
        assert!(
            matches!(status, Err(OpError::InvalidArguments(_))),
            "{status:?}"
        );
        let stop = run(&IntentCycleStopOperation, &fx.ctx, args).await;
        assert!(
            matches!(stop, Err(OpError::InvalidArguments(_))),
            "{stop:?}"
        );
        let record = fx.jobs.get(&foreign_id).map_err(ctx("foreign job"))?;
        assert_eq!(record.job.state, harw_job_runtime::JobState::Ready);
        Ok(())
    }

    #[tokio::test]
    async fn other_workspace_jobs_of_the_same_tenant_stay_hidden() -> TestResult {
        let fx = fixture()?;
        let output = run(&IntentCycleStartOperation, &fx.ctx, start_args())
            .await
            .map_err(ctx("start"))?;
        let id = job_id(&output)?;
        let record = fx.jobs.get(&id).map_err(ctx("job"))?;
        let input = IntentCycleJobInput::from_job(&record).map_err(ctx("input"))?;
        let mut other = admission_record(
            &fx.ctx,
            &Principal::trusted_ingress(
                PrincipalKind::Model,
                "model:explorer",
                IngressSurface::Cli,
                PermissionTier::Operator,
            ),
            WorkId::new(),
            &input,
            None,
        )
        .map_err(ctx("other record"))?;
        other.scope = JobScope::new(
            TenantId::from_str("test-tenant"),
            WorkspaceId::from_str("other-ws"),
            ApprovalActor::Operator {
                id: "other".to_owned(),
            },
        );
        let other_id = other.job.id.clone();
        fx.jobs.admit(&other).map_err(ctx("admit other"))?;
        let args = serde_json::json!({"job_id": other_id.as_str()});
        let status = run(&IntentCycleStatusOperation, &fx.ctx, args.clone()).await;
        assert!(
            matches!(status, Err(OpError::InvalidArguments(_))),
            "{status:?}"
        );
        let stop = run(&IntentCycleStopOperation, &fx.ctx, args).await;
        assert!(
            matches!(stop, Err(OpError::InvalidArguments(_))),
            "{stop:?}"
        );
        let record = fx.jobs.get(&other_id).map_err(ctx("other job"))?;
        assert_eq!(record.job.state, harw_job_runtime::JobState::Ready);
        Ok(())
    }

    #[tokio::test]
    async fn stop_is_attributed_to_the_caller_and_bounds_the_reason() -> TestResult {
        let fx = fixture()?;
        let output = run(&IntentCycleStartOperation, &fx.ctx, start_args())
            .await
            .map_err(ctx("start"))?;
        let id = job_id(&output)?;
        run(
            &IntentCycleStopOperation,
            &fx.ctx,
            serde_json::json!({"job_id": id.as_str(), "reason": "x".repeat(5_000)}),
        )
        .await
        .map_err(ctx("stop"))?;
        let record = fx.jobs.get(&id).map_err(ctx("job"))?;
        let cancellation = record
            .cancellation
            .ok_or(TestError::Missing("cancellation"))?;
        assert_eq!(cancellation.reason.chars().count(), MAX_STOP_REASON_CHARS);
        assert_eq!(
            cancellation.cancelled_by,
            ApprovalActor::Operator {
                id: "model:explorer".to_owned()
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn overlong_criterion_names_are_refused() -> TestResult {
        let fx = fixture()?;
        let mut args = start_args();
        args["acceptance"][0]["criterion"] = serde_json::json!("c".repeat(129));
        let result = run(&IntentCycleStartOperation, &fx.ctx, args).await;
        assert!(
            matches!(result, Err(OpError::InvalidArguments(_))),
            "{result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn status_does_not_create_the_cycle_store() -> TestResult {
        let fx = fixture()?;
        let output = run(&IntentCycleStartOperation, &fx.ctx, start_args())
            .await
            .map_err(ctx("start"))?;
        let id = job_id(&output)?;
        run(
            &IntentCycleStatusOperation,
            &fx.ctx,
            serde_json::json!({"job_id": id.as_str()}),
        )
        .await
        .map_err(ctx("status"))?;
        let dir = fx.jobs.root().join(harw_plan_bridge::CYCLE_STORE_DIR);
        assert!(!dir.exists(), "status created {}", dir.display());
        Ok(())
    }

    #[tokio::test]
    async fn status_reports_pending_observations() -> TestResult {
        let fx = fixture()?;
        let output = run(&IntentCycleStartOperation, &fx.ctx, start_args())
            .await
            .map_err(ctx("start"))?;
        let id = job_id(&output)?;
        let stored = fx.jobs.get(&id).map_err(ctx("job"))?;
        let input = IntentCycleJobInput::from_job(&stored).map_err(ctx("input"))?;
        let plan = input.spec.plan(&input.intent_id).map_err(ctx("plan"))?;

        // The worker creates the record on its first claim; do the same by hand.
        let store = CycleStore::open_in_job_root(fx.jobs.root()).map_err(ctx("cycle store"))?;
        let initial = CycleCheckpoint::initial(&plan.admission, fx.ctx.sandbox().authority(), 0);
        let record = CycleRecord::new(input.cycle_id.clone(), id.clone(), initial.clone())
            .map_err(ctx("cycle record"))?;
        store.create(&record).map_err(ctx("create record"))?;

        // Without an observation the line reports an empty outbox.
        let before = run(
            &IntentCycleStatusOperation,
            &fx.ctx,
            serde_json::json!({"job_id": id.as_str()}),
        )
        .await
        .map_err(ctx("status before"))?;
        assert!(
            before.text.contains("pending observations 0, dropped 0"),
            "{}",
            before.text
        );

        // Journal one step and commit it together with one observation.
        let admitted = admit_cycle(
            &plan.admission,
            &initial,
            CycleProposal::Wait {
                reason: "waiting for the owner".to_owned(),
            },
        )
        .map_err(|_| TestError::Missing("admitted wait"))?;
        let token = LeaseToken {
            work_id: id.clone(),
            epoch: 0,
            nonce: "nonce-0".to_owned(),
        };
        store
            .begin_step(&input.cycle_id, &token, &admitted)
            .map_err(ctx("begin step"))?;
        let next = apply_observations(
            plan.admission.intent(),
            &initial,
            &admitted,
            CycleObservations::default(),
        )
        .map_err(|_| TestError::Missing("next checkpoint"))?;
        let observation = build_observation(
            &input.cycle_id,
            initial.fence,
            0,
            &initial,
            admitted.proposal(),
            ObservationCategory::ToolFailedNoEffect,
            CauseCode::ExecutorFailed,
            &[],
        );
        store
            .commit_step_observed(&input.cycle_id, &token, &next, &[observation])
            .map_err(ctx("commit step"))?;

        let status = run(
            &IntentCycleStatusOperation,
            &fx.ctx,
            serde_json::json!({"job_id": id.as_str()}),
        )
        .await
        .map_err(ctx("status"))?;
        assert!(
            status.text.contains("pending observations 1, dropped 0"),
            "{}",
            status.text
        );
        let cycle = status
            .data
            .as_ref()
            .and_then(|data| data.get("cycle"))
            .ok_or(TestError::Missing("cycle projection"))?;
        assert_eq!(
            cycle
                .get("pending_observations")
                .and_then(serde_json::Value::as_u64),
            Some(1),
            "{cycle}"
        );
        assert_eq!(
            cycle
                .get("observations_dropped")
                .and_then(serde_json::Value::as_u64),
            Some(0),
            "{cycle}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn status_refuses_other_job_kinds_and_unknown_ids() -> TestResult {
        let fx = fixture()?;
        let unknown = run(
            &IntentCycleStatusOperation,
            &fx.ctx,
            serde_json::json!({"job_id": WorkId::new().as_str()}),
        )
        .await;
        assert!(matches!(unknown, Err(OpError::InvalidArguments(_))));
        let missing = run(&IntentCycleStatusOperation, &fx.ctx, serde_json::json!({})).await;
        assert!(matches!(missing, Err(OpError::InvalidArguments(_))));
        Ok(())
    }
}
