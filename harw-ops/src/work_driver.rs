//! `work_driver.*` — start, observe and stop a durable WorkDriver run.
//!
//! # Operations
//! | Name | Domain | Tier | Model tool | Web |
//! |---|---|---|---|---|
//! | `work_driver.enqueue` | execution | operator | `approval = "always"` | `POST /api/work-driver/enqueue`, `approval = "always"` |
//! | `work_driver.status` | execution | observer | `readonly`, `approval = "none"` | `GET /api/work-driver/status` |
//! | `work_driver.stop` | execution | operator | `approval = "always"` | `POST /api/work-driver/stop`, `approval = "always"` |
//!
//! There is deliberately **no** `command(...)` surface: the WorkDriver is
//! started by an orchestrator agent (or through the Web UI on its behalf),
//! never through a TUI slash command. `FromRawArgs` of every argument type
//! therefore only exists because the `#[operation]` macro always emits the
//! command parse arm; it is unreachable in a registry without a command
//! surface.
//!
//! # Gating: only callers whose IR has a `[work_driver]` section
//! Tool surfaces are registered per operation, not per role, so the gate
//! cannot be expressed at registration time. `work_driver.enqueue` gates at
//! run time instead, **before any store access**: the runtime puts an
//! `Arc<`[`WorkDriverCaller`]`>` into the [`OpContext`] only for a session
//! whose agent IR carries `work_driver` ([`WorkDriverCaller::from_ir`]).
//! Without it the operation answers [`OpError::NotAvailable`] — on every
//! surface: the effective limits are derived from the caller's
//! [`WorkDriverSpec`], and there is no default spec to fall back on. A Web
//! caller therefore also needs the runtime to name the orchestrator on whose
//! behalf the run is started.
//!
//! The one exception is the UIA (R18 F6, backlog C-04): the runtime puts
//! [`WorkDriverCaller::for_uia`] into the UIA root's context — the UIA
//! definition's own `[work_driver]` section if it has one, else
//! [`WorkDriverCaller::uia_default_spec`]. Nothing else changes for it: the
//! model surface asks a human on every call (`approval = "always"`) and the
//! overrides only narrow.
//!
//! # Worker report tool
//! [`WorkerReportToolProvider`] serves `work_driver.report`
//! ([`harw_plan_bridge::WORK_DRIVER_REPORT_TOOL`]), the structured return of
//! a WorkDriver worker (R18 D-E). It is not an operation: the job worker
//! registers it for WorkDriver worker turns only and reads the result from
//! the turn's [`WorkerReportSlot`].
//!
//! # Narrowing only
//! The job payload carries the caller's [`WorkDriverSpec`] narrowed by the
//! optional [`WorkDriverOverrides`]. Every override must be `>= 1` and not
//! larger than the caller's own value (a budget the caller leaves unbounded
//! may be bounded). A larger value is [`OpError::InvalidArguments`], never a
//! silent clamp. Roles and verification commands are not overridable at all
//! (`deny_unknown_fields`). The job budget mirrors the narrowed token and
//! wall-clock budgets, so the durable ledger enforces them too.
//!
//! # Job kind and payload
//! The admitted job has `JobKind::Custom(`[`WORK_DRIVER_JOB_KIND`]`)` and
//! input [`WorkDriverJobInput`]. `harw-runtime` defines the same literal as
//! `harw_runtime::job_ledger::WORK_DRIVER_JOB_KIND`; `harw-ops` cannot depend
//! on `harw-runtime` (the runtime depends on `harw-ops`), so the constant is
//! mirrored here and pinned by a test. [`WorkDriverJobInput::tenant`] is the
//! enqueuing caller's tenant scope (H12), stamped from [`OpContext::tenant`];
//! `enqueue`'s goal lookup goes through
//! [`harw_plan::tenant_scope::ScopedGoalStore`] with the same scope rule as
//! `goal.rs`, so a scoped caller cannot enqueue on a foreign tenant's goal
//! (it fails exactly like an unknown goal). The "one active run per goal"
//! guard ([`active_run_for_goal`]) is scoped the same way, through
//! [`crate::job_tenant::admits_job`]: `goal_id` alone carries no tenant
//! qualification, so without this a same-named goal of a foreign tenant
//! could block a caller's own run and leak the foreign job's id. An
//! unscoped (single-user) caller keeps seeing untenanted goals and every
//! tenant's active runs unchanged.
//!
//! # State sidecar
//! The job worker publishes its round state as [`WorkDriverState`] under
//! `<job-root>/work_driver/<work-id>.json` ([`state_sidecar_path`],
//! [`write_state_sidecar`], [`read_state_sidecar`]) — the same
//! `<kind>/<id>.json` layout the job store uses for its own sidecars.
//! `work_driver.status` reads it; a missing sidecar just means the driver has
//! not finished its first round yet.
//!
//! # Concurrency
//! The operation structs are stateless (`Send + Sync`). Job mutations go
//! through the per-record locks of [`JobStore`]; the "one active run per
//! goal" check before admission is best effort (two concurrent enqueues can
//! both pass it), the job worker must tolerate that.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use harw_agent_dsl::ir_v2::{AgentIr, WorkDriverSpec};
use harw_job_runtime::{Budget, Job, JobKind, JobScope, JobState, RetryPolicy, StoredJob, WorkId};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_plan::error::PlanError;
use harw_plan::goal::{GoalStatus, GoalStore};
use harw_plan::tenant_scope::ScopedGoalStore;
use harw_plan_bridge::{JudgeVerdict, OpContextPlanExt, VerificationState, WorkerState};
use harw_session_store::{CancelRequest, JobListQuery, JobStore, SessionStoreError};
use harw_types::{ApprovalActor, Principal, TenantId};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

/// `JobKind::Custom` discriminator of a WorkDriver job. Must equal
/// `harw_runtime::job_ledger::WORK_DRIVER_JOB_KIND` (see module docs).
pub const WORK_DRIVER_JOB_KIND: &str = "work_driver";

/// Schema version of [`WorkDriverJobInput`].
pub const WORK_DRIVER_INPUT_SCHEMA_VERSION: u32 = 1;

/// Schema version of [`WorkDriverState`].
pub const WORK_DRIVER_STATE_SCHEMA_VERSION: u32 = 1;

/// Sidecar directory below the job root (`<job-root>/work_driver/`).
pub const WORK_DRIVER_STATE_DIR: &str = "work_driver";

/// Attempts the durable job gets. One: a crashed driver is not restarted
/// behind the operator's back; `/retry` requeues it explicitly.
const WORK_DRIVER_MAX_ATTEMPTS: u32 = 1;

/// Page size of the "one active run per goal" scan.
const ACTIVE_SCAN_PAGE: usize = 200;

/// Default cancellation reason of `work_driver.stop`.
const DEFAULT_STOP_REASON: &str = "stopped through work_driver.stop";

// ── Caller service ───────────────────────────────────────────────────────────

/// The calling orchestrator's `[work_driver]` settings, put into the
/// [`OpContext`] by the runtime as `Arc<WorkDriverCaller>`.
///
/// # Description
/// Present only for a session whose agent IR has a `work_driver` section.
/// It is the trust anchor of `work_driver.enqueue`: the limits of every run
/// are derived from `spec` and can only be lowered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkDriverCaller {
    /// Registry role name of the orchestrator.
    pub role: String,
    /// The orchestrator's effective WorkDriver settings.
    pub spec: WorkDriverSpec,
}

/// Role name recorded as `orchestrator_role` for runs the UIA starts.
pub const UIA_WORK_DRIVER_ROLE: &str = "user-interface";

impl WorkDriverCaller {
    /// The caller service for the UIA (R18 F6, backlog C-04).
    ///
    /// # Description
    /// The UIA is not an orchestrator and usually has no `[work_driver]`
    /// section, yet it may start a WorkDriver run on the user's behalf. The
    /// runtime hook (`harw-runtime/src/assembly.rs`) puts this caller into the
    /// UIA root's [`OpContext`]. `configured` is the UIA definition's own
    /// `[work_driver]` section, if it has one; otherwise
    /// [`Self::uia_default_spec`] applies. Admission is unchanged: the model
    /// surface of `work_driver.enqueue` asks a human every time
    /// (`approval = "always"`), and the overrides can only narrow this spec.
    #[must_use]
    pub fn for_uia(configured: Option<&WorkDriverSpec>) -> Self {
        Self {
            role: UIA_WORK_DRIVER_ROLE.to_owned(),
            spec: configured.cloned().unwrap_or_else(Self::uia_default_spec),
        }
    }

    /// Default WorkDriver settings of the UIA: the DSL defaults
    /// (`WorkDriverSpec::DEFAULT_*`), the default worker role
    /// ([`harw_plan_bridge::DEFAULT_WORKER_ROLE`]), no judge role, no own
    /// `verify` commands (the goal's own `Command`/`Artifact` steps are
    /// still verified centrally) and no own budgets (the driver's safety
    /// defaults apply).
    #[must_use]
    pub fn uia_default_spec() -> WorkDriverSpec {
        WorkDriverSpec {
            max_iterations: WorkDriverSpec::DEFAULT_MAX_ITERATIONS,
            max_parallel_workers: WorkDriverSpec::DEFAULT_MAX_PARALLEL_WORKERS,
            max_attempts_per_worker: WorkDriverSpec::DEFAULT_MAX_ATTEMPTS_PER_WORKER,
            stall_iterations: WorkDriverSpec::DEFAULT_STALL_ITERATIONS
                .min(WorkDriverSpec::DEFAULT_MAX_ITERATIONS),
            worker_role: harw_plan_bridge::DEFAULT_WORKER_ROLE.to_owned(),
            judge_role: None,
            verify: Vec::new(),
            token_budget: None,
            wall_budget_secs: None,
        }
    }

    /// Builds the service from a role's IR; `None` without `[work_driver]`.
    #[must_use]
    pub fn from_ir(role: impl Into<String>, ir: &AgentIr) -> Option<Self> {
        Self::from_spec(role, ir.work_driver.as_ref())
    }

    /// Builds the service from a role's executable IR; `None` without
    /// `[work_driver]`. The runtime only holds the executable IR, so this is
    /// the constructor it uses to populate the [`OpContext`] service.
    #[must_use]
    pub fn from_executable(
        role: impl Into<String>,
        ir: &harw_agent_dsl::executable::ExecutableAgentIr,
    ) -> Option<Self> {
        Self::from_spec(role, ir.work_driver.as_ref())
    }

    /// Builds the service from a role name and an optional spec; `None`
    /// without a spec. `from_ir` and `from_executable` both delegate here so
    /// the "present only with `[work_driver]`" semantics stay in one place.
    #[must_use]
    pub fn from_spec(role: impl Into<String>, spec: Option<&WorkDriverSpec>) -> Option<Self> {
        spec.map(|spec| Self {
            role: role.into(),
            spec: spec.clone(),
        })
    }
}

// ── Arguments ────────────────────────────────────────────────────────────────

/// Optional limit overrides of `work_driver.enqueue`; each may only lower
/// the caller's own limit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkDriverOverrides {
    /// Lower bound for `max_iterations`.
    #[serde(default)]
    pub max_iterations: Option<u32>,
    /// Lower bound for `max_parallel_workers`.
    #[serde(default)]
    pub max_parallel_workers: Option<u32>,
    /// Lower bound for `max_attempts_per_worker`.
    #[serde(default)]
    pub max_attempts_per_worker: Option<u32>,
    /// Lower bound for `stall_iterations`.
    #[serde(default)]
    pub stall_iterations: Option<u32>,
    /// Lower token budget of the whole run.
    #[serde(default)]
    pub token_budget: Option<u64>,
    /// Lower wall-clock budget of the whole run, in seconds.
    #[serde(default)]
    pub wall_budget_secs: Option<u64>,
}

/// Arguments of `work_driver.enqueue`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkDriverEnqueueArgs {
    /// Id of the goal to drive; must be the current, `Active` goal.
    #[serde(default)]
    pub goal_id: Option<String>,
    /// Plan to drive against; defaults to the plan bound to the goal.
    #[serde(default)]
    pub plan_id: Option<String>,
    /// Optional, narrowing-only limit overrides.
    #[serde(default)]
    pub overrides: Option<WorkDriverOverrides>,
}

impl harw_operations::FromRawArgs for WorkDriverEnqueueArgs {
    /// No command surface (see module docs).
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Err(no_command_surface("work_driver.enqueue"))
    }
}

impl harw_operations::OpArgsSchema for WorkDriverEnqueueArgs {
    fn json_schema() -> harw_tools::JsonSchema {
        use harw_operations::op_schema::{described_object_schema, integer_schema, string_schema};
        let overrides = described_object_schema(
            "Optional limit overrides. Each value must be at least 1 and may only LOWER the \
             caller's own [work_driver] limit; a larger value is rejected.",
            vec![
                (
                    "max_iterations",
                    integer_schema("Upper bound of driver rounds."),
                ),
                (
                    "max_parallel_workers",
                    integer_schema("Workers running at the same time."),
                ),
                (
                    "max_attempts_per_worker",
                    integer_schema("Attempts per worker task."),
                ),
                (
                    "stall_iterations",
                    integer_schema("Rounds without progress before the run stops."),
                ),
                ("token_budget", integer_schema("Token budget of the run.")),
                (
                    "wall_budget_secs",
                    integer_schema("Wall-clock budget of the run in seconds."),
                ),
            ],
            &[],
        );
        described_object_schema(
            "Starts a durable WorkDriver run on the current, active goal. Returns the job id.",
            vec![
                ("goal_id", string_schema("Id of the active goal to drive.")),
                (
                    "plan_id",
                    string_schema("Plan to drive against; default: the goal's bound plan."),
                ),
                ("overrides", overrides),
            ],
            &["goal_id"],
        )
    }
}

/// Arguments of `work_driver.status`.
#[derive(Debug, Default, Deserialize, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct WorkDriverStatusArgs {
    /// Job id returned by `work_driver.enqueue`.
    #[serde(default)]
    pub job_id: Option<String>,
}

impl harw_operations::FromRawArgs for WorkDriverStatusArgs {
    /// No command surface (see module docs).
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Err(no_command_surface("work_driver.status"))
    }
}

/// Arguments of `work_driver.stop`.
#[derive(Debug, Default, Deserialize, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct WorkDriverStopArgs {
    /// Job id returned by `work_driver.enqueue`.
    #[serde(default)]
    pub job_id: Option<String>,
    /// Optional cancellation reason.
    #[serde(default)]
    pub reason: Option<String>,
}

impl harw_operations::FromRawArgs for WorkDriverStopArgs {
    /// No command surface (see module docs).
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Err(no_command_surface("work_driver.stop"))
    }
}

fn no_command_surface(name: &str) -> OpError {
    OpError::NotAvailable(format!(
        "`{name}` has no command surface; it is a model tool of WorkDriver orchestrators"
    ))
}

// ── Payload and state sidecar ────────────────────────────────────────────────

/// Typed input of a WorkDriver job (`StoredJob::input`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkDriverJobInput {
    /// [`WORK_DRIVER_INPUT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The goal to drive.
    pub goal_id: String,
    /// The plan to drive against, if any.
    pub plan_id: Option<String>,
    /// Registry role of the orchestrator that enqueued the run.
    pub orchestrator_role: String,
    /// Principal id that requested the run.
    pub requested_by: String,
    /// Effective settings: the caller's spec narrowed by the overrides.
    pub spec: WorkDriverSpec,
    /// The enqueuing caller's tenant scope (H12), if any. Stamped by
    /// `work_driver.enqueue` from [`OpContext::tenant`]; absent for an
    /// unscoped (single-user) caller and for a job admitted before this
    /// field existed (old job input JSON still loads via
    /// `#[serde(default)]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<TenantId>,
}

impl WorkDriverJobInput {
    /// Decodes the input of a stored WorkDriver job.
    ///
    /// # Errors
    /// [`io::ErrorKind::InvalidData`] for a job of another kind or an input
    /// that does not match the schema.
    pub fn from_job(record: &StoredJob) -> io::Result<Self> {
        if !is_work_driver_job(record) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("job {} is not a {WORK_DRIVER_JOB_KIND} job", record.job.id),
            ));
        }
        serde_json::from_value(record.input.clone())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
}

/// Token usage of a run, as published by the job worker.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkDriverUsage {
    /// Tokens used over all workers.
    pub tokens_used: u64,
    /// Prompt-cache hit ratio over all workers (`0.0..=1.0`), if known.
    #[serde(default)]
    pub cache_hit_ratio: Option<f32>,
    /// Consecutive rounds without a newly met criterion.
    #[serde(default)]
    pub iterations_without_progress: u32,
}

/// Round state of a WorkDriver job (state sidecar, written by the job worker).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkDriverState {
    /// [`WORK_DRIVER_STATE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The job this state belongs to.
    pub work_id: WorkId,
    /// Current round (from `0`).
    pub iteration: u32,
    /// Every worker of the current wave.
    #[serde(default)]
    pub workers: Vec<WorkerState>,
    /// Central verification of the current wave.
    #[serde(default)]
    pub verification: VerificationState,
    /// Usage so far.
    #[serde(default)]
    pub usage: WorkDriverUsage,
    /// Actual parallel worker count this run, if narrowed by the provider
    /// below `spec.max_parallel_workers` (e.g. a rate-limited provider cap).
    /// `None` means the configured limit was not narrowed down.
    #[serde(default)]
    pub effective_parallel: Option<u32>,
    /// Count of HTTP 429 (rate limited) responses seen so far this run.
    #[serde(default)]
    pub rate_limited: u32,
    /// The judge's latest verdict, if one was obtained.
    #[serde(default)]
    pub last_judge: Option<JudgeVerdict>,
    /// Rationale lines of the latest driver decision.
    #[serde(default)]
    pub last_rationale: Vec<String>,
    /// When the worker last wrote this state.
    pub updated_at: Timestamp,
}

/// Path of the state sidecar of `work_id` below `store`'s job root.
///
/// # Errors
/// [`io::ErrorKind::InvalidInput`] for an id that is not a safe path segment.
pub fn state_sidecar_path(store: &JobStore, work_id: &WorkId) -> io::Result<PathBuf> {
    let id = work_id.as_str();
    let safe = !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !safe {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "work id is not a safe path segment",
        ));
    }
    Ok(store
        .root()
        .join(WORK_DRIVER_STATE_DIR)
        .join(format!("{id}.json")))
}

/// Reads the state sidecar; `Ok(None)` if the worker has not written one.
///
/// # Errors
/// [`io::ErrorKind::InvalidInput`] (unsafe id), [`io::ErrorKind::InvalidData`]
/// (symlink, not a regular file, undecodable), other read errors unchanged.
pub fn read_state_sidecar(
    store: &JobStore,
    work_id: &WorkId,
) -> io::Result<Option<WorkDriverState>> {
    let path = state_sidecar_path(store, work_id)?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "work_driver state sidecar is not a regular file",
        ));
    }
    let bytes = std::fs::read(&path)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Atomically writes (or replaces) the state sidecar of `state.work_id`
/// (temp file + fsync + rename).
///
/// # Errors
/// [`io::ErrorKind::InvalidInput`] (unsafe id), serialization or I/O errors.
pub fn write_state_sidecar(store: &JobStore, state: &WorkDriverState) -> io::Result<()> {
    use std::io::Write as _;
    let path = state_sidecar_path(store, &state.work_id)?;
    let Some(dir) = path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "sidecar path has no parent",
        ));
    };
    std::fs::create_dir_all(dir)?;
    let bytes = serde_json::to_vec(state).map_err(io::Error::other)?;
    let temp = dir.join(format!(
        ".{}.json.tmp-{}",
        state.work_id.as_str(),
        std::process::id()
    ));
    {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, &path)
}

/// `true` for a job of kind [`WORK_DRIVER_JOB_KIND`].
#[must_use]
pub fn is_work_driver_job(record: &StoredJob) -> bool {
    matches!(&record.job.kind, JobKind::Custom(kind) if kind == WORK_DRIVER_JOB_KIND)
}

// ── Worker report tool ───────────────────────────────────────────────────────

/// Holds the last valid [`WorkerReport`] of one worker turn.
///
/// # Description
/// The job worker creates one slot per WorkDriver worker turn, registers a
/// [`WorkerReportToolProvider`] over it for exactly that turn and reads it
/// with [`Self::take`] once the turn has ended. The last valid report of the
/// turn wins; a rejected report never reaches the slot.
///
/// # Concurrency
/// `Send + Sync`; a poisoned lock is recovered (the stored value is a plain
/// report, it cannot be half-written).
#[derive(Debug, Default)]
pub struct WorkerReportSlot {
    last: std::sync::Mutex<Option<harw_plan_bridge::WorkerReport>>,
}

impl WorkerReportSlot {
    /// An empty slot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes the last valid report, leaving the slot empty.
    #[must_use]
    pub fn take(&self) -> Option<harw_plan_bridge::WorkerReport> {
        match self.last.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }

    fn store(&self, report: harw_plan_bridge::WorkerReport) {
        match self.last.lock() {
            Ok(mut guard) => *guard = Some(report),
            Err(poisoned) => *poisoned.into_inner() = Some(report),
        }
    }
}

/// The tool `work_driver.report` ([`harw_plan_bridge::WORK_DRIVER_REPORT_TOOL`])
/// for one WorkDriver worker turn (R18 D-E, contract §8).
///
/// # Description
/// Registered by the job worker **only** for WorkDriver worker turns; no
/// registry profile and no operation registry carries it. A call is checked
/// with `WorkerReport` deserialization (`deny_unknown_fields`; a top-level
/// `null` counts as absent) and [`harw_plan_bridge::WorkerReport::validate`]
/// against the goal's criteria count. A rejected call returns a tool error
/// naming the rule, so the worker can correct it, and is not recorded.
/// The tool has no side effect beyond the slot.
#[derive(Debug, Clone)]
pub struct WorkerReportToolProvider {
    slot: Arc<WorkerReportSlot>,
    criteria_total: usize,
}

impl WorkerReportToolProvider {
    /// The provider over `slot` for a goal with `criteria_total` acceptance
    /// criteria.
    #[must_use]
    pub fn new(slot: Arc<WorkerReportSlot>, criteria_total: usize) -> Self {
        Self {
            slot,
            criteria_total,
        }
    }
}

impl harw_extension_api::ToolProvider for WorkerReportToolProvider {
    fn tools(&self) -> Vec<harw_tools::ToolSpec> {
        vec![worker_report_tool_spec(self.criteria_total)]
    }

    fn executor(&self, name: &harw_tools::ToolName) -> Option<Arc<dyn harw_tools::ToolExecutor>> {
        (name.as_str() == harw_plan_bridge::WORK_DRIVER_REPORT_TOOL).then(|| {
            Arc::new(WorkerReportExecutor {
                slot: Arc::clone(&self.slot),
                criteria_total: self.criteria_total,
            }) as Arc<dyn harw_tools::ToolExecutor>
        })
    }
}

struct WorkerReportExecutor {
    slot: Arc<WorkerReportSlot>,
    criteria_total: usize,
}

impl harw_tools::ToolExecutor for WorkerReportExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a harw_tools::ToolExecutionContext,
        call: &'a harw_tools::ToolCall,
    ) -> harw_tools::ToolExecutorFuture<'a> {
        let arguments = call.arguments.clone();
        Box::pin(async move {
            Ok(record_worker_report(
                &self.slot,
                self.criteria_total,
                arguments,
            ))
        })
    }
}

/// Core of `work_driver.report` (testable without a sandbox context).
fn record_worker_report(
    slot: &WorkerReportSlot,
    criteria_total: usize,
    arguments: serde_json::Value,
) -> harw_tools::ToolOutput {
    let arguments = match arguments {
        serde_json::Value::Object(mut map) => {
            map.retain(|_, value| !value.is_null());
            serde_json::Value::Object(map)
        }
        other => other,
    };
    let report: harw_plan_bridge::WorkerReport = match serde_json::from_value(arguments) {
        Ok(report) => report,
        Err(error) => {
            return harw_tools::ToolOutput::error(format!(
                "{}: invalid arguments ({error}); nothing was recorded",
                harw_plan_bridge::WORK_DRIVER_REPORT_TOOL
            ));
        }
    };
    if let Err(error) = report.validate(criteria_total) {
        return harw_tools::ToolOutput::error(format!("{error}; nothing was recorded"));
    }
    let status = report.status;
    slot.store(report);
    harw_tools::ToolOutput::text(format!(
        "report recorded (status {}). End your turn now; a later valid report replaces this one.",
        match status {
            harw_plan_bridge::WorkerReportStatus::Done => "done",
            harw_plan_bridge::WorkerReportStatus::Partial => "partial",
            harw_plan_bridge::WorkerReportStatus::Blocked => "blocked",
            harw_plan_bridge::WorkerReportStatus::Failed => "failed",
        }
    ))
}

/// Tool spec of `work_driver.report`; the shape of
/// [`harw_plan_bridge::WorkerReport::input_schema`]. Not `strict`: optional
/// fields stay optional instead of becoming nullable.
fn worker_report_tool_spec(criteria_total: usize) -> harw_tools::ToolSpec {
    use harw_tools::{AdditionalProperties, FunctionToolSpec, JsonSchema, JsonSchemaType};
    let typed = |schema_type: JsonSchemaType, description: &str| JsonSchema {
        schema_type: Some(schema_type),
        description: Some(description.to_owned()),
        ..JsonSchema::default()
    };
    let array_of = |items: JsonSchema, description: &str| JsonSchema {
        items: Some(Box::new(items)),
        ..typed(JsonSchemaType::Array, description)
    };
    let mut status = typed(
        JsonSchemaType::String,
        "done: scope finished, waiting for the central verification; partial: can be \
         continued; blocked: needs a human decision (name it in `blockers`); failed: hard \
         failure.",
    );
    status.enum_values = Some(
        ["done", "partial", "blocked", "failed"]
            .into_iter()
            .map(serde_json::Value::from)
            .collect(),
    );
    let mut properties = std::collections::BTreeMap::new();
    properties.insert("status".to_owned(), status);
    properties.insert(
        "criteria_addressed".to_owned(),
        array_of(
            typed(JsonSchemaType::Integer, "Criterion index."),
            "Indices of the acceptance criteria you worked on (as listed in your task).",
        ),
    );
    properties.insert(
        "changed_paths".to_owned(),
        array_of(
            typed(JsonSchemaType::String, "Workspace-relative path."),
            "Files you changed, workspace-relative with `/`; no absolute paths, no `..`.",
        ),
    );
    properties.insert(
        "summary".to_owned(),
        typed(
            JsonSchemaType::String,
            "Short summary of what you did; must not be empty.",
        ),
    );
    properties.insert(
        "blockers".to_owned(),
        array_of(
            typed(JsonSchemaType::String, "One open question or obstacle."),
            "Open decisions or obstacles; required (non-empty) for status `blocked`.",
        ),
    );
    let parameters = JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        properties: Some(properties),
        required: Some(vec!["status".to_owned(), "summary".to_owned()]),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..JsonSchema::default()
    };
    harw_tools::ToolSpec::Function(FunctionToolSpec {
        name: harw_tools::ToolName::new(harw_plan_bridge::WORK_DRIVER_REPORT_TOOL),
        description: format!(
            "Report the outcome of your WorkDriver turn. Call it once at the end of every turn; \
             the last valid call wins, a text status line is not read. The goal has \
             {criteria_total} acceptance criteria (indices 0..{criteria_total}). An invalid \
             report is rejected with the reason and not recorded."
        ),
        parameters,
        strict: false,
    })
}

// ── Narrowing ────────────────────────────────────────────────────────────────

/// Narrows `base` by `overrides`; never widens.
///
/// # Errors
/// [`OpError::InvalidArguments`] for a zero value, a value larger than the
/// caller's limit, or a `stall_iterations` above the effective
/// `max_iterations`.
pub fn narrow_spec(
    base: &WorkDriverSpec,
    overrides: &WorkDriverOverrides,
) -> Result<WorkDriverSpec, OpError> {
    let max_iterations = narrow_u32(
        "max_iterations",
        base.max_iterations,
        overrides.max_iterations,
    )?;
    let max_parallel_workers = narrow_u32(
        "max_parallel_workers",
        base.max_parallel_workers,
        overrides.max_parallel_workers,
    )?;
    let max_attempts_per_worker = narrow_u32(
        "max_attempts_per_worker",
        base.max_attempts_per_worker,
        overrides.max_attempts_per_worker,
    )?;
    let stall_iterations = match overrides.stall_iterations {
        None => base.stall_iterations.min(max_iterations),
        Some(requested) => {
            let stall = narrow_u32("stall_iterations", base.stall_iterations, Some(requested))?;
            if stall > max_iterations {
                return Err(OpError::InvalidArguments(format!(
                    "overrides.stall_iterations = {stall} exceeds the effective \
                     max_iterations = {max_iterations}"
                )));
            }
            stall
        }
    };
    Ok(WorkDriverSpec {
        max_iterations,
        max_parallel_workers,
        max_attempts_per_worker,
        stall_iterations,
        worker_role: base.worker_role.clone(),
        judge_role: base.judge_role.clone(),
        verify: base.verify.clone(),
        token_budget: narrow_budget("token_budget", base.token_budget, overrides.token_budget)?,
        wall_budget_secs: narrow_budget(
            "wall_budget_secs",
            base.wall_budget_secs,
            overrides.wall_budget_secs,
        )?,
    })
}

fn narrow_u32(field: &str, base: u32, requested: Option<u32>) -> Result<u32, OpError> {
    match requested {
        None => Ok(base),
        Some(0) => Err(OpError::InvalidArguments(format!(
            "overrides.{field} must be at least 1"
        ))),
        Some(value) if value > base => Err(widening(field, value, base)),
        Some(value) => Ok(value),
    }
}

fn narrow_budget(
    field: &str,
    base: Option<u64>,
    requested: Option<u64>,
) -> Result<Option<u64>, OpError> {
    match (base, requested) {
        (base, None) => Ok(base),
        (_, Some(0)) => Err(OpError::InvalidArguments(format!(
            "overrides.{field} must be at least 1"
        ))),
        (Some(limit), Some(value)) if value > limit => Err(widening(field, value, limit)),
        (_, Some(value)) => Ok(Some(value)),
    }
}

fn widening(field: &str, value: impl std::fmt::Display, limit: impl std::fmt::Display) -> OpError {
    OpError::InvalidArguments(format!(
        "overrides.{field} = {value} would widen the caller's limit {limit}; \
         overrides may only lower limits"
    ))
}

// ── Operations ───────────────────────────────────────────────────────────────

/// Starts a durable WorkDriver run on the current, active goal.
///
/// # Order (fail closed)
/// 1. [`WorkDriverCaller`] in the context — else `NotAvailable`, before any
///    store access.
/// 2. Arguments: `goal_id` present, `plan_id` well-formed, overrides narrow.
/// 3. Job store, `[tools.plan]` enabled, goal store, principal.
/// 4. The goal is the current goal and `Active`; a bound plan must match.
/// 5. No other non-terminal WorkDriver job on the same goal.
/// 6. Admit a `Ready` job of kind [`WORK_DRIVER_JOB_KIND`].
///
/// # Returns
/// The job id (text and `data.job_id`).
///
/// # Errors
/// - [`OpError::NotAvailable`]: no caller spec, no job/goal store, tool
///   disabled, no principal.
/// - [`OpError::InvalidArguments`]: missing/unknown goal, inactive goal,
///   plan mismatch, widening override.
/// - [`OpError::Execution`]: store failures, an already active run.
#[operation(
    name = "work_driver.enqueue",
    summary = "Startet einen dauerhaften WorkDriver-Lauf auf dem aktiven Ziel (nur Orchestratoren mit [work_driver]).",
    domain = "execution",
    permission = "operator",
    model_tool(approval = "always"),
    // Startet dauerhafte Arbeit (Worker, Verifikation, Budget): immer
    // bestätigungspflichtig, identisch zur ModelTool-Fläche.
    web(path = "/api/work-driver/enqueue", method = "post", approval = "always")
)]
async fn work_driver_enqueue(
    ctx: &OpContext,
    args: WorkDriverEnqueueArgs,
) -> Result<OpOutput, OpError> {
    // ── Gate: only orchestrators whose IR has [work_driver] ─────────────────
    let caller = ctx.service::<Arc<WorkDriverCaller>>().ok_or_else(|| {
        OpError::NotAvailable(
            "work_driver.enqueue is only available to agents whose definition has a \
             [work_driver] section"
                .to_owned(),
        )
    })?;

    let goal_id = args
        .goal_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| OpError::InvalidArguments("goal_id is required".to_owned()))?
        .to_owned();
    let requested_plan = match args.plan_id.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some(crate::plan::parse_plan_id(raw)?),
        _ => None,
    };
    let spec = narrow_spec(&caller.spec, &args.overrides.unwrap_or_default())?;

    let jobs = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;
    let config = harw_operations::require_service!(ctx.plan_config(), "Goal-Store");
    config.require_enabled().map_err(|error| {
        OpError::NotAvailable(format!(
            "goal tooling is not usable ({error}); enable it with `[tools.plan] enabled = true`"
        ))
    })?;
    let goals_handle = harw_operations::require_service!(ctx.goal_store(), "Goal-Store");
    // H12: a scoped caller only sees goals of its own tenant; a foreign
    // goal behaves like "no goal" (same error as an unknown goal id), same
    // scope rule as `goal.rs`. An unscoped (single-user) caller keeps
    // seeing untenanted goals unchanged.
    let scoped = ScopedGoalStore::new(goals_handle.as_ref(), ctx.tenant().cloned());
    let goals: &dyn GoalStore = &scoped;
    let principal = crate::plan::require_principal(ctx)?;

    // ── Goal: current, matching, Active ──────────────────────────────────────
    let goal = match goals.current() {
        Ok(goal) => goal,
        Err(PlanError::GoalNotFound) => {
            return Err(OpError::InvalidArguments(format!(
                "unknown goal `{goal_id}`: no goal is set"
            )));
        }
        Err(error) => return Err(OpError::Execution(format!("goal store: {error}"))),
    };
    if goal.id.as_str() != goal_id {
        return Err(OpError::InvalidArguments(format!(
            "unknown goal `{goal_id}`: the current goal is `{}`",
            goal.id
        )));
    }
    if goal.status != GoalStatus::Active {
        return Err(OpError::InvalidArguments(format!(
            "goal `{goal_id}` is {:?}; only an Active goal can be driven",
            goal.status
        )));
    }
    let plan_id = match (requested_plan, goal.plan_id.as_ref()) {
        (Some(requested), Some(bound)) if requested.as_str() != bound.as_str() => {
            return Err(OpError::InvalidArguments(format!(
                "goal `{goal_id}` is bound to plan `{}`, not `{}`",
                bound.as_str(),
                requested.as_str()
            )));
        }
        (Some(requested), _) => Some(requested.as_str().to_owned()),
        (None, bound) => bound.map(|plan| plan.as_str().to_owned()),
    };

    if let Some(existing) = active_run_for_goal(ctx, jobs, &goal_id)? {
        return Err(OpError::Execution(format!(
            "goal `{goal_id}` is already driven by job `{existing}`; stop it first \
             (work_driver.stop)"
        )));
    }

    // ── Admission ────────────────────────────────────────────────────────────
    let input = WorkDriverJobInput {
        schema_version: WORK_DRIVER_INPUT_SCHEMA_VERSION,
        goal_id: goal_id.clone(),
        plan_id,
        orchestrator_role: caller.role.clone(),
        requested_by: principal.id().to_owned(),
        spec,
        tenant: ctx.tenant().cloned(),
    };
    let record = admission_record(ctx, principal, &input)?;
    let work_id = record.job.id.clone();
    jobs.admit(&record).map_err(|error| {
        OpError::Execution(format!("could not admit the work_driver job: {error}"))
    })?;

    Ok(OpOutput {
        text: format!(
            "WorkDriver job {work_id} admitted for goal `{goal_id}` (max {} rounds, {} parallel \
             workers). Observe it with work_driver.status.",
            input.spec.max_iterations, input.spec.max_parallel_workers
        ),
        data: Some(serde_json::json!({
            "job_id": work_id.as_str(),
            "kind": WORK_DRIVER_JOB_KIND,
            "input": &input,
        })),
    })
}

/// Builds the `Ready` job record for `input`.
fn admission_record(
    ctx: &OpContext,
    principal: &Principal,
    input: &WorkDriverJobInput,
) -> Result<StoredJob, OpError> {
    let now = Timestamp::now();
    let budget = Budget {
        max_tokens: input.spec.token_budget,
        max_wall: input
            .spec
            .wall_budget_secs
            .map(|secs| SignedDuration::from_secs(i64::try_from(secs).unwrap_or(i64::MAX))),
        max_tool_calls: None,
    };
    let mut job = Job::new(
        WorkId::new(),
        JobKind::Custom(WORK_DRIVER_JOB_KIND.to_owned()),
        budget,
        RetryPolicy {
            max_attempts: WORK_DRIVER_MAX_ATTEMPTS,
            base_delay: SignedDuration::from_secs(30),
            factor: 2.0,
            max_delay: SignedDuration::from_secs(300),
            jitter: 0.0,
        },
        now,
    );
    job.mark_ready(now)
        .map_err(|error| OpError::Execution(format!("work_driver job: {error}")))?;
    let binding = ctx.sandbox().workspace();
    let payload = serde_json::to_value(input)
        .map_err(|error| OpError::Execution(format!("work_driver payload: {error}")))?;
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

/// The id of a non-terminal WorkDriver job on `goal_id`, if any.
///
/// `goal_id` alone carries no tenant qualification (H12): without a tenant
/// check a scoped caller could be blocked by — and learn the job id of — a
/// same-named goal of a foreign tenant. This scan therefore applies the same
/// visibility rule as every other job-reading path in this module
/// (`crate::job_tenant::admits_job`): a scoped caller only ever sees, and is
/// only ever blocked by, its own tenant's active runs; an unscoped
/// (single-user) caller keeps seeing every tenant's runs unchanged.
fn active_run_for_goal(
    ctx: &OpContext,
    jobs: &JobStore,
    goal_id: &str,
) -> Result<Option<WorkId>, OpError> {
    let mut query = JobListQuery {
        states: Some(vec![
            JobState::Pending,
            JobState::Ready,
            JobState::Running,
            JobState::Blocked,
        ]),
        holder: None,
        limit: ACTIVE_SCAN_PAGE,
        cursor: None,
    };
    loop {
        let page = jobs
            .list(&query)
            .map_err(|error| OpError::Execution(format!("could not list jobs: {error}")))?;
        for record in &page.jobs {
            let same_goal = record
                .input
                .get("goal_id")
                .and_then(serde_json::Value::as_str)
                == Some(goal_id);
            let visible = crate::job_tenant::admits_job(ctx, record);
            if is_work_driver_job(record) && same_goal && visible {
                return Ok(Some(record.job.id.clone()));
            }
        }
        match page.next_cursor {
            Some(cursor) => query.cursor = Some(cursor),
            None => return Ok(None),
        }
    }
}

/// Loads a visible WorkDriver job by the caller-supplied id.
///
/// # Errors
/// `InvalidArguments` for a missing/malformed id, an unknown (or foreign
/// tenant's) job and a job of another kind; `Execution` for store failures.
fn load_driver_job(
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
    let record = match crate::job_tenant::get_visible_job(ctx, jobs, &work_id) {
        Ok(record) => record,
        Err(SessionStoreError::JobNotFound { .. }) => {
            return Err(OpError::InvalidArguments(format!(
                "unknown work_driver job `{raw}`"
            )));
        }
        Err(error) => {
            return Err(OpError::Execution(format!(
                "could not read job `{raw}`: {error}"
            )));
        }
    };
    if !is_work_driver_job(&record) {
        return Err(OpError::InvalidArguments(format!(
            "job `{raw}` is not a work_driver job"
        )));
    }
    Ok(record)
}

/// Reports a WorkDriver job: job state, round, workers, verification, usage
/// and the judge's latest verdict.
///
/// # Errors
/// - [`OpError::NotAvailable`]: no job store.
/// - [`OpError::InvalidArguments`]: missing, unknown or foreign job id.
/// - [`OpError::Execution`]: store or sidecar read failure.
#[operation(
    name = "work_driver.status",
    summary = "Zeigt Runde, Worker, Verifikation, Verbrauch und letztes Bewerter-Urteil eines WorkDriver-Laufs.",
    domain = "execution",
    permission = "observer",
    model_tool(readonly, approval = "none"),
    // Reines Lesen von Job-Datensatz und Zustands-Sidecar, keine Mutation.
    web(path = "/api/work-driver/status", method = "get", approval = "none")
)]
async fn work_driver_status(
    ctx: &OpContext,
    args: WorkDriverStatusArgs,
) -> Result<OpOutput, OpError> {
    let jobs = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;
    let record = load_driver_job(ctx, jobs, args.job_id.as_deref())?;
    let input = WorkDriverJobInput::from_job(&record).ok();
    let state = read_state_sidecar(jobs, &record.job.id).map_err(|error| {
        OpError::Execution(format!(
            "could not read the work_driver state of `{}`: {error}",
            record.job.id
        ))
    })?;
    Ok(render_status(&record, input.as_ref(), state.as_ref()))
}

fn render_status(
    record: &StoredJob,
    input: Option<&WorkDriverJobInput>,
    state: Option<&WorkDriverState>,
) -> OpOutput {
    let mut lines = vec![format!(
        "WorkDriver job {} — {:?}",
        record.job.id, record.job.state
    )];
    if let Some(input) = input {
        lines.push(format!(
            "goal `{}`{}, orchestrator `{}`, limits: {} rounds, {} parallel, {} attempts/worker",
            input.goal_id,
            input
                .plan_id
                .as_deref()
                .map(|plan| format!(" (plan `{plan}`)"))
                .unwrap_or_default(),
            input.orchestrator_role,
            input.spec.max_iterations,
            input.spec.max_parallel_workers,
            input.spec.max_attempts_per_worker,
        ));
    }
    let cache_hit_ratio = state.and_then(|state| {
        state
            .usage
            .cache_hit_ratio
            .or_else(|| mean_cache_hit_ratio(&state.workers))
    });
    match state {
        None => lines.push("no driver round recorded yet".to_owned()),
        Some(state) => {
            lines.push(format!("iteration {}", state.iteration));
            lines.push(format!("workers ({}):", state.workers.len()));
            for worker in &state.workers {
                let outcome = worker.last_result.as_ref().map_or_else(
                    || "running".to_owned(),
                    |result| outcome_label(&result.outcome),
                );
                lines.push(format!(
                    "  {} scope `{}` [{}] attempts {} — {outcome}",
                    worker.worker_id,
                    worker.scope.id,
                    worker.scope.owned_paths.join(", "),
                    worker.attempts,
                ));
            }
            lines.push(format!(
                "verification: {}",
                verification_label(&state.verification)
            ));
            lines.push(format!(
                "usage: {} tokens, cache hit ratio {}",
                state.usage.tokens_used,
                cache_hit_ratio.map_or_else(|| "unknown".to_owned(), |ratio| format!("{ratio:.2}"))
            ));
            if let Some(parallel) = state.effective_parallel {
                lines.push(format!("parallel: {parallel} (provider-capped)"));
            }
            if state.rate_limited > 0 {
                lines.push(format!("rate limited: {}×", state.rate_limited));
            }
            match &state.last_judge {
                None => lines.push("judge: no verdict".to_owned()),
                Some(verdict) => lines.push(format!(
                    "judge: {} — {}",
                    if verdict.passed {
                        "passed"
                    } else {
                        "not passed"
                    },
                    verdict.comment
                )),
            }
        }
    }
    let data = serde_json::json!({
        "job_id": record.job.id.as_str(),
        "job_state": record.job.state,
        "input": input,
        "iteration": state.map(|state| state.iteration),
        "workers": state.map(|state| &state.workers),
        "verification": state.map(|state| &state.verification),
        "usage": state.map(|state| serde_json::json!({
            "tokens_used": state.usage.tokens_used,
            "cache_hit_ratio": cache_hit_ratio,
            "iterations_without_progress": state.usage.iterations_without_progress,
        })),
        "effective_parallel": state.and_then(|state| state.effective_parallel),
        "rate_limited": state.map(|state| state.rate_limited),
        "last_judge": state.and_then(|state| state.last_judge.as_ref()),
        "updated_at": state.map(|state| state.updated_at),
    });
    OpOutput {
        text: lines.join("\n"),
        data: Some(data),
    }
}

fn mean_cache_hit_ratio(workers: &[WorkerState]) -> Option<f32> {
    let known: Vec<f32> = workers
        .iter()
        .filter_map(|worker| worker.cache_hit_ratio)
        .collect();
    if known.is_empty() {
        return None;
    }
    let count = u16::try_from(known.len()).map_or(f32::from(u16::MAX), f32::from);
    Some(known.iter().sum::<f32>() / count)
}

fn outcome_label(outcome: &harw_plan_bridge::WorkerOutcome) -> String {
    use harw_plan_bridge::WorkerOutcome;
    match outcome {
        WorkerOutcome::Done => "done".to_owned(),
        WorkerOutcome::Partial => "partial".to_owned(),
        WorkerOutcome::Blocked { reason } => format!("blocked: {reason}"),
        WorkerOutcome::Failed { reason } => format!("failed: {reason}"),
    }
}

fn verification_label(state: &VerificationState) -> String {
    match state {
        VerificationState::NotRun => "not run".to_owned(),
        VerificationState::Passed => "passed".to_owned(),
        VerificationState::Failed { failing } => {
            format!("failed ({} failing: {})", failing.len(), failing.join("; "))
        }
    }
}

/// Stops a WorkDriver job through the durable job cancel path
/// ([`JobStore::cancel`]); a running driver's lease is fenced, so it cannot
/// renew or complete afterwards.
///
/// # Errors
/// - [`OpError::NotAvailable`]: no job store.
/// - [`OpError::InvalidArguments`]: missing, unknown, foreign or non-driver
///   job id.
/// - [`OpError::Execution`]: the store refuses the cancellation (e.g. the job
///   is already terminal).
#[operation(
    name = "work_driver.stop",
    summary = "Bricht einen WorkDriver-Lauf über den dauerhaften Job-Abbruch ab.",
    domain = "execution",
    permission = "operator",
    model_tool(approval = "always"),
    // Irreversibel (Cancelled ist terminal): bestätigungspflichtig wie `stop`.
    web(path = "/api/work-driver/stop", method = "post", approval = "always")
)]
async fn work_driver_stop(ctx: &OpContext, args: WorkDriverStopArgs) -> Result<OpOutput, OpError> {
    let jobs = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;
    let record = load_driver_job(ctx, jobs, args.job_id.as_deref())?;
    let reason = args
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .unwrap_or(DEFAULT_STOP_REASON);
    let transition = jobs
        .cancel(
            &record.job.id,
            &CancelRequest {
                cancelled_at: Timestamp::now(),
                // Same authority source as `/cancel`: the approval gate of
                // this operation is the operator's decision.
                cancelled_by: ApprovalActor::Operator {
                    id: "work_driver.stop".to_owned(),
                },
                reason: reason.to_owned(),
            },
        )
        .map_err(|error| {
            OpError::Execution(format!(
                "could not stop work_driver job `{}`: {error}",
                record.job.id
            ))
        })?;
    Ok(OpOutput {
        text: format!(
            "Stopped WorkDriver job {} (was {:?}, revision {}).",
            transition.work_id, transition.previous_state, transition.revision
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
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_operations::context::ServiceMap;
    use harw_operations::{OpInput, Operation};
    use harw_plan::goal::{Goal, GoalAction, GoalId};
    use harw_plan::{InMemoryGoalStore, PlanToolConfig};
    use harw_plan_bridge::{WorkScope, WorkerOutcome, WorkerResultSummary};
    use harw_types::{
        IngressSurface, PermissionTier, PrincipalKind, SessionId, TenantId, TurnId, WorkspaceId,
    };
    use time::OffsetDateTime;

    const GOAL: &str = "g-1";

    fn spec() -> WorkDriverSpec {
        WorkDriverSpec {
            max_iterations: 8,
            max_parallel_workers: 4,
            max_attempts_per_worker: 4,
            stall_iterations: 2,
            worker_role: "executor".to_owned(),
            judge_role: Some("evaluator".to_owned()),
            verify: vec!["cargo test".to_owned()],
            token_budget: Some(1_000_000),
            wall_budget_secs: None,
        }
    }

    fn goal(status: GoalStatus) -> Goal {
        Goal {
            id: GoalId::new(GOAL),
            revision: 0,
            statement: "the driver works".to_owned(),
            non_goals: Vec::new(),
            invariants: Vec::new(),
            acceptance_criteria: Vec::new(),
            constraints: Vec::new(),
            open_questions: Vec::new(),
            status,
            plan_id: None,
            plan_revision: None,
            evidence: Vec::new(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            tenant: None,
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        ctx: OpContext,
        jobs: Arc<JobStore>,
    }

    /// Context with job store, goal store (goal in `status`), enabled plan
    /// tooling, a model principal and — if `caller` — the WorkDriver caller.
    fn fixture(caller: bool, status: GoalStatus) -> TestResult<Fixture> {
        fixture_with(
            caller.then(|| WorkDriverCaller {
                role: "work-orchestrator".to_owned(),
                spec: spec(),
            }),
            status,
        )
    }

    /// Like [`fixture`], with an explicit caller service (or none).
    fn fixture_with(caller: Option<WorkDriverCaller>, status: GoalStatus) -> TestResult<Fixture> {
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

        let goals: Arc<dyn GoalStore> = Arc::new(InMemoryGoalStore::default());
        goals
            .apply(GoalAction::Set { goal: goal(status) }, "human:test")
            .map_err(ctx("set goal"))?;
        let jobs = Arc::new(JobStore::new(&dir.path().join("state")));

        let mut services = ServiceMap::new();
        services.insert(Arc::clone(&jobs));
        services.insert(goals);
        services.insert(PlanToolConfig::enabled_defaults());
        services.insert(Principal::trusted_ingress(
            PrincipalKind::Model,
            "model:orchestrator",
            IngressSurface::Cli,
            PermissionTier::Operator,
        ));
        if let Some(caller) = caller {
            services.insert(Arc::new(caller));
        }
        Ok(Fixture {
            _dir: dir,
            ctx: OpContext::new(SessionId::new(), TurnId::new(), sandbox, services),
            jobs,
        })
    }

    /// Like [`fixture`], but the goal belongs to `goal_tenant` (H12) and the
    /// returned context is itself scoped to `caller_tenant` — used to test
    /// tenant stamping and cross-tenant isolation of `work_driver.enqueue`.
    fn fixture_scoped(
        status: GoalStatus,
        goal_tenant: Option<&str>,
        caller_tenant: Option<&str>,
    ) -> TestResult<Fixture> {
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

        let goals: Arc<dyn GoalStore> = Arc::new(InMemoryGoalStore::default());
        let mut owned_goal = goal(status);
        owned_goal.tenant = goal_tenant.map(TenantId::from_str);
        goals
            .apply(GoalAction::Set { goal: owned_goal }, "human:test")
            .map_err(ctx("set goal"))?;
        let jobs = Arc::new(JobStore::new(&dir.path().join("state")));

        let mut services = ServiceMap::new();
        services.insert(Arc::clone(&jobs));
        services.insert(goals);
        services.insert(PlanToolConfig::enabled_defaults());
        services.insert(Principal::trusted_ingress(
            PrincipalKind::Model,
            "model:orchestrator",
            IngressSurface::Cli,
            PermissionTier::Operator,
        ));
        services.insert(Arc::new(WorkDriverCaller {
            role: "work-orchestrator".to_owned(),
            spec: spec(),
        }));
        let op_ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, services);
        let op_ctx = match caller_tenant {
            Some(tenant) => op_ctx.with_tenant(TenantId::from_str(tenant)),
            None => op_ctx,
        };
        Ok(Fixture {
            _dir: dir,
            ctx: op_ctx,
            jobs,
        })
    }

    async fn run_model(
        op: &dyn Operation,
        ctx: &OpContext,
        args: serde_json::Value,
    ) -> Result<OpOutput, OpError> {
        op.run(ctx, OpInput::model_tool(args)).await
    }

    fn job_id_of(output: &OpOutput) -> TestResult<WorkId> {
        output
            .data
            .as_ref()
            .and_then(|data| data.get("job_id"))
            .and_then(serde_json::Value::as_str)
            .map(WorkId::from_str)
            .ok_or(TestError::Missing("data.job_id"))
    }

    /// `[work_driver]` orchestrator definition text (DSL §8.3), compiled by
    /// [`compiled_executable_ir`] below.
    const DRIVER_DEFINITION: &str = r#"schema = "harwness.agent/v1"
id = "acme.agent.driver@1"
version = "1.0.0"
role = "child-orchestrator"
specialization = "driver"

[spawn]
max_depth = 2

[delegation]
targets = ["executor", "critic"]

[lifecycle]
max_attempts = 3

[verification]
commands = ["cargo test"]

[work_driver]
worker_role = "executor"
judge_role = "critic"
"#;

    /// Compiles [`DRIVER_DEFINITION`] into an [`AgentIr`], the same
    /// `compile_agent` path `harw-agent-dsl/tests/work_driver.rs` uses.
    fn compiled_ir() -> TestResult<AgentIr> {
        use harw_agent_dsl::diagnostics::SourceFile;
        use harw_agent_dsl::ids::DefinitionId;
        use harw_agent_dsl::layers::DefinitionLayer;
        use harw_agent_dsl::lower_v2::{LowerSources, compile_agent};

        let files = [SourceFile::new(
            DefinitionLayer::UserGlobal,
            "agents/driver/definition.toml",
            DRIVER_DEFINITION,
        )];
        let target = DefinitionId::parse("acme.agent.driver@1").map_err(ctx("parse target id"))?;
        compile_agent(
            &target,
            &LowerSources::new(&files),
            OffsetDateTime::UNIX_EPOCH,
        )
        .map_err(|diagnostics| TestError::Unexpected(diagnostics.to_string()))
    }

    /// Same definition without the `[work_driver]` table.
    fn compiled_ir_without_work_driver() -> TestResult<AgentIr> {
        use harw_agent_dsl::diagnostics::SourceFile;
        use harw_agent_dsl::ids::DefinitionId;
        use harw_agent_dsl::layers::DefinitionLayer;
        use harw_agent_dsl::lower_v2::{LowerSources, compile_agent};

        let text = DRIVER_DEFINITION.replace(
            "[work_driver]\nworker_role = \"executor\"\njudge_role = \"critic\"\n",
            "",
        );
        let files = [SourceFile::new(
            DefinitionLayer::UserGlobal,
            "agents/driver/definition.toml",
            &text,
        )];
        let target = DefinitionId::parse("acme.agent.driver@1").map_err(ctx("parse target id"))?;
        compile_agent(
            &target,
            &LowerSources::new(&files),
            OffsetDateTime::UNIX_EPOCH,
        )
        .map_err(|diagnostics| TestError::Unexpected(diagnostics.to_string()))
    }

    #[test]
    fn from_executable_mirrors_from_ir_with_a_work_driver_section() -> TestResult {
        use harw_agent_dsl::executable::ExecutableAgentIr;

        let agent_ir = compiled_ir()?;
        assert!(
            agent_ir.work_driver.is_some(),
            "fixture must have [work_driver]"
        );
        let executable_ir = ExecutableAgentIr::from(&agent_ir);

        let from_ir = WorkDriverCaller::from_ir("driver", &agent_ir)
            .ok_or(TestError::Missing("WorkDriverCaller::from_ir"))?;
        let from_executable = WorkDriverCaller::from_executable("driver", &executable_ir)
            .ok_or(TestError::Missing("WorkDriverCaller::from_executable"))?;
        assert_eq!(from_ir, from_executable);
        assert_eq!(from_executable.role, "driver");
        assert_eq!(from_executable.spec.worker_role, "executor");
        assert_eq!(from_executable.spec.judge_role, Some("critic".to_owned()));
        Ok(())
    }

    #[test]
    fn from_executable_is_none_without_a_work_driver_section() -> TestResult {
        use harw_agent_dsl::executable::ExecutableAgentIr;

        let agent_ir = compiled_ir_without_work_driver()?;
        assert!(
            agent_ir.work_driver.is_none(),
            "fixture must lack [work_driver]"
        );
        let executable_ir = ExecutableAgentIr::from(&agent_ir);

        assert!(WorkDriverCaller::from_ir("driver", &agent_ir).is_none());
        assert!(WorkDriverCaller::from_executable("driver", &executable_ir).is_none());
        Ok(())
    }

    #[test]
    fn from_spec_underlies_both_constructors() {
        assert_eq!(WorkDriverCaller::from_spec("r", None), None);
        let caller =
            WorkDriverCaller::from_spec("r", Some(&spec())).expect("Some spec must build a caller");
        assert_eq!(caller.role, "r");
        assert_eq!(caller.spec, spec());
    }

    #[test]
    fn job_kind_matches_the_runtime_literal() {
        // Mirror of `harw_runtime::job_ledger::WORK_DRIVER_JOB_KIND`.
        assert_eq!(WORK_DRIVER_JOB_KIND, "work_driver");
    }

    #[test]
    fn operation_names_match_the_capability_catalog() {
        use harw_registry_defaults::capability_catalog::{
            WORK_DRIVER_ENQUEUE_TOOL, WORK_DRIVER_STATUS_TOOL, WORK_DRIVER_STOP_TOOL,
        };
        assert_eq!(
            WorkDriverEnqueueOperation.meta().name,
            WORK_DRIVER_ENQUEUE_TOOL
        );
        assert_eq!(
            WorkDriverStatusOperation.meta().name,
            WORK_DRIVER_STATUS_TOOL
        );
        assert_eq!(WorkDriverStopOperation.meta().name, WORK_DRIVER_STOP_TOOL);
    }

    #[test]
    fn no_operation_has_a_command_surface() {
        use harw_operations::Surface;
        for op in [
            &WorkDriverEnqueueOperation as &dyn Operation,
            &WorkDriverStatusOperation,
            &WorkDriverStopOperation,
        ] {
            assert!(
                !op.meta()
                    .surfaces
                    .iter()
                    .any(|surface| matches!(surface, Surface::Command { .. })),
                "{} must not have a command surface",
                op.meta().name
            );
        }
    }

    #[tokio::test]
    async fn enqueue_is_refused_on_the_model_surface_without_a_work_driver_caller() -> TestResult {
        let fx = fixture(false, GoalStatus::Active)?;
        let result = run_model(
            &WorkDriverEnqueueOperation,
            &fx.ctx,
            serde_json::json!({ "goal_id": GOAL }),
        )
        .await;
        match result {
            Err(OpError::NotAvailable(message)) => assert!(message.contains("[work_driver]")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable, got {other:?}"
                )));
            }
        }
        let page = fx
            .jobs
            .list(&JobListQuery::default())
            .map_err(ctx("list"))?;
        assert!(page.jobs.is_empty(), "no job may be admitted");
        Ok(())
    }

    #[tokio::test]
    async fn enqueue_admits_a_ready_job_with_the_narrowed_spec() -> TestResult {
        let fx = fixture(true, GoalStatus::Active)?;
        let output = run_model(
            &WorkDriverEnqueueOperation,
            &fx.ctx,
            serde_json::json!({
                "goal_id": GOAL,
                "overrides": { "max_iterations": 3, "token_budget": 5000, "wall_budget_secs": 600 }
            }),
        )
        .await
        .map_err(ctx("enqueue"))?;
        let work_id = job_id_of(&output)?;
        let record = fx.jobs.get(&work_id).map_err(ctx("get job"))?;
        assert!(is_work_driver_job(&record));
        assert_eq!(record.job.state, JobState::Ready);
        let input = WorkDriverJobInput::from_job(&record).map_err(ctx("decode input"))?;
        assert_eq!(input.goal_id, GOAL);
        assert_eq!(input.orchestrator_role, "work-orchestrator");
        assert_eq!(input.spec.max_iterations, 3);
        assert_eq!(input.spec.max_parallel_workers, 4);
        assert_eq!(input.spec.stall_iterations, 2);
        assert_eq!(input.spec.token_budget, Some(5000));
        assert_eq!(input.spec.wall_budget_secs, Some(600));
        assert_eq!(record.job.budget.max_tokens, Some(5000));
        assert_eq!(
            record.job.budget.max_wall,
            Some(SignedDuration::from_secs(600))
        );

        // A second run on the same goal is refused while the first is active.
        let second = run_model(
            &WorkDriverEnqueueOperation,
            &fx.ctx,
            serde_json::json!({ "goal_id": GOAL }),
        )
        .await;
        assert!(
            matches!(second, Err(OpError::Execution(ref message)) if message.contains(work_id.as_str())),
            "expected an active-run refusal, got {second:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn widening_overrides_are_invalid_arguments() -> TestResult {
        let fx = fixture(true, GoalStatus::Active)?;
        for overrides in [
            serde_json::json!({ "max_iterations": 9 }),
            serde_json::json!({ "max_parallel_workers": 5 }),
            serde_json::json!({ "max_attempts_per_worker": 5 }),
            serde_json::json!({ "stall_iterations": 3 }),
            serde_json::json!({ "token_budget": 1_000_001 }),
            serde_json::json!({ "max_iterations": 0 }),
            serde_json::json!({ "max_iterations": 1, "stall_iterations": 2 }),
            // Roles and verification commands are not overridable at all.
            serde_json::json!({ "worker_role": "root" }),
        ] {
            let result = run_model(
                &WorkDriverEnqueueOperation,
                &fx.ctx,
                serde_json::json!({ "goal_id": GOAL, "overrides": overrides }),
            )
            .await;
            assert!(
                matches!(result, Err(OpError::InvalidArguments(_))),
                "{overrides}: expected InvalidArguments, got {result:?}"
            );
        }
        let page = fx
            .jobs
            .list(&JobListQuery::default())
            .map_err(ctx("list"))?;
        assert!(page.jobs.is_empty(), "no job may be admitted");
        Ok(())
    }

    #[test]
    fn narrowing_lowers_and_clamps_stall_but_never_widens() -> TestResult {
        let narrowed = narrow_spec(
            &spec(),
            &WorkDriverOverrides {
                max_iterations: Some(1),
                ..WorkDriverOverrides::default()
            },
        )
        .map_err(ctx("narrow"))?;
        assert_eq!(narrowed.max_iterations, 1);
        assert_eq!(
            narrowed.stall_iterations, 1,
            "stall follows max_iterations down"
        );
        assert_eq!(narrowed.worker_role, "executor");

        let unchanged = narrow_spec(&spec(), &WorkDriverOverrides::default()).map_err(ctx("id"))?;
        assert_eq!(unchanged, spec());

        let bounded = narrow_spec(
            &spec(),
            &WorkDriverOverrides {
                wall_budget_secs: Some(60),
                ..WorkDriverOverrides::default()
            },
        )
        .map_err(ctx("bound an unbounded budget"))?;
        assert_eq!(bounded.wall_budget_secs, Some(60));
        Ok(())
    }

    #[tokio::test]
    async fn an_inactive_goal_is_refused() -> TestResult {
        for status in [GoalStatus::Draft, GoalStatus::Blocked] {
            let fx = fixture(true, status)?;
            let result = run_model(
                &WorkDriverEnqueueOperation,
                &fx.ctx,
                serde_json::json!({ "goal_id": GOAL }),
            )
            .await;
            assert!(
                matches!(result, Err(OpError::InvalidArguments(ref message)) if message.contains("Active")),
                "{status:?}: expected InvalidArguments, got {result:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn an_unknown_goal_is_refused() -> TestResult {
        let fx = fixture(true, GoalStatus::Active)?;
        let result = run_model(
            &WorkDriverEnqueueOperation,
            &fx.ctx,
            serde_json::json!({ "goal_id": "other-goal" }),
        )
        .await;
        assert!(
            matches!(result, Err(OpError::InvalidArguments(ref message)) if message.contains("unknown goal")),
            "expected InvalidArguments, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn status_of_an_unknown_job_is_an_error() -> TestResult {
        let fx = fixture(true, GoalStatus::Active)?;
        let result = run_model(
            &WorkDriverStatusOperation,
            &fx.ctx,
            serde_json::json!({ "job_id": "no-such-job" }),
        )
        .await;
        assert!(
            matches!(result, Err(OpError::InvalidArguments(ref message)) if message.contains("unknown")),
            "expected InvalidArguments, got {result:?}"
        );
        let missing = run_model(&WorkDriverStatusOperation, &fx.ctx, serde_json::Value::Null).await;
        assert!(matches!(missing, Err(OpError::InvalidArguments(_))));
        Ok(())
    }

    #[tokio::test]
    async fn status_reports_the_state_sidecar_and_stop_cancels() -> TestResult {
        let fx = fixture(true, GoalStatus::Active)?;
        let output = run_model(
            &WorkDriverEnqueueOperation,
            &fx.ctx,
            serde_json::json!({ "goal_id": GOAL }),
        )
        .await
        .map_err(ctx("enqueue"))?;
        let work_id = job_id_of(&output)?;

        let before = run_model(
            &WorkDriverStatusOperation,
            &fx.ctx,
            serde_json::json!({ "job_id": work_id.as_str() }),
        )
        .await
        .map_err(ctx("status before a round"))?;
        assert!(before.text.contains("no driver round recorded yet"));

        let state = WorkDriverState {
            schema_version: WORK_DRIVER_STATE_SCHEMA_VERSION,
            work_id: work_id.clone(),
            iteration: 2,
            workers: vec![WorkerState {
                worker_id: "w-1".to_owned(),
                scope: WorkScope {
                    id: "scope-a".to_owned(),
                    summary: "a".to_owned(),
                    owned_paths: vec!["src/a.rs".to_owned()],
                    criteria: vec![0],
                },
                attempts: 1,
                last_result: Some(WorkerResultSummary {
                    outcome: WorkerOutcome::Done,
                    summary: "done".to_owned(),
                    artifacts: Vec::new(),
                    suggested_next: None,
                    criteria_addressed: Vec::new(),
                }),
                context_tokens_used: 1000,
                cache_hit_ratio: Some(0.5),
            }],
            verification: VerificationState::Failed {
                failing: vec!["test a".to_owned()],
            },
            usage: WorkDriverUsage {
                tokens_used: 1234,
                cache_hit_ratio: None,
                iterations_without_progress: 0,
            },
            effective_parallel: Some(3),
            rate_limited: 2,
            last_judge: Some(JudgeVerdict {
                passed: false,
                comment: "criterion 0 open".to_owned(),
                missing: vec!["x".to_owned()],
            }),
            last_rationale: Vec::new(),
            updated_at: Timestamp::now(),
        };
        write_state_sidecar(&fx.jobs, &state).map_err(ctx("write sidecar"))?;
        let read_back = read_state_sidecar(&fx.jobs, &work_id).map_err(ctx("read sidecar"))?;
        assert_eq!(read_back.as_ref(), Some(&state));

        let after = run_model(
            &WorkDriverStatusOperation,
            &fx.ctx,
            serde_json::json!({ "job_id": work_id.as_str() }),
        )
        .await
        .map_err(ctx("status after a round"))?;
        for needle in [
            "iteration 2",
            "w-1",
            "scope-a",
            "failed (1 failing",
            "1234 tokens",
            "0.50",
            "not passed",
            "parallel: 3 (provider-capped)",
            "rate limited: 2×",
        ] {
            assert!(
                after.text.contains(needle),
                "{needle} missing in:\n{}",
                after.text
            );
        }

        let stopped = run_model(
            &WorkDriverStopOperation,
            &fx.ctx,
            serde_json::json!({ "job_id": work_id.as_str() }),
        )
        .await
        .map_err(ctx("stop"))?;
        assert!(stopped.text.contains("Stopped"));
        let record = fx.jobs.get(&work_id).map_err(ctx("get"))?;
        assert_eq!(record.job.state, JobState::Cancelled);

        // Once stopped, a new run on the same goal may start.
        run_model(
            &WorkDriverEnqueueOperation,
            &fx.ctx,
            serde_json::json!({ "goal_id": GOAL }),
        )
        .await
        .map_err(ctx("enqueue after stop"))?;
        Ok(())
    }

    #[tokio::test]
    async fn stop_refuses_a_job_of_another_kind() -> TestResult {
        let fx = fixture(true, GoalStatus::Active)?;
        let now = Timestamp::now();
        let job = Job::new(
            WorkId::from_str("plain-job"),
            JobKind::Worker,
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: 1,
                base_delay: SignedDuration::from_secs(1),
                factor: 1.0,
                max_delay: SignedDuration::from_secs(1),
                jitter: 0.0,
            },
            now,
        );
        fx.jobs
            .admit(&StoredJob {
                job,
                scope: JobScope::new(
                    TenantId::from_str("test-tenant"),
                    WorkspaceId::from_str("ws"),
                    ApprovalActor::Operator { id: "t".to_owned() },
                ),
                input: serde_json::json!({}),
                submitted_at: now,
                not_before: now,
                lease: None,
                lease_epoch: 0,
                completion: None,
                cancellation: None,
                revision: 0,
                trace: None,
            })
            .map_err(ctx("admit"))?;
        let result = run_model(
            &WorkDriverStopOperation,
            &fx.ctx,
            serde_json::json!({ "job_id": "plain-job" }),
        )
        .await;
        assert!(
            matches!(result, Err(OpError::InvalidArguments(ref message)) if message.contains("not a work_driver job")),
            "expected InvalidArguments, got {result:?}"
        );
        let record = fx
            .jobs
            .get(&WorkId::from_str("plain-job"))
            .map_err(ctx("get"))?;
        assert_eq!(record.job.state, JobState::Pending, "foreign job untouched");
        Ok(())
    }

    // ── New fields (effective_parallel, rate_limited, tenant) ──────────────

    #[test]
    fn old_sidecar_json_without_the_new_fields_still_loads() -> TestResult {
        // A sidecar as `write_state_sidecar` would have written it before
        // `effective_parallel` and `rate_limited` existed: build a current
        // state and strip the two new keys back out.
        let full = WorkDriverState {
            schema_version: WORK_DRIVER_STATE_SCHEMA_VERSION,
            work_id: WorkId::from_str("w-old"),
            iteration: 1,
            workers: Vec::new(),
            verification: VerificationState::NotRun,
            usage: WorkDriverUsage::default(),
            effective_parallel: Some(5),
            rate_limited: 3,
            last_judge: None,
            last_rationale: Vec::new(),
            updated_at: Timestamp::now(),
        };
        let mut value = serde_json::to_value(&full).map_err(ctx("serialize"))?;
        let object = value.as_object_mut().ok_or_else(|| {
            TestError::Unexpected("state did not serialize to an object".to_owned())
        })?;
        object.remove("effective_parallel");
        object.remove("rate_limited");

        let decoded: WorkDriverState =
            serde_json::from_value(value).map_err(ctx("decode old-shaped sidecar JSON"))?;
        assert_eq!(decoded.work_id, full.work_id);
        assert_eq!(decoded.iteration, 1);
        assert_eq!(
            decoded.effective_parallel, None,
            "old sidecars have no cap on record"
        );
        assert_eq!(decoded.rate_limited, 0, "old sidecars saw no 429 counter");
        Ok(())
    }

    #[tokio::test]
    async fn enqueue_records_the_callers_tenant() -> TestResult {
        let fx = fixture_scoped(GoalStatus::Active, Some("tenant-a"), Some("tenant-a"))?;
        let output = run_model(
            &WorkDriverEnqueueOperation,
            &fx.ctx,
            serde_json::json!({ "goal_id": GOAL }),
        )
        .await
        .map_err(ctx("enqueue as tenant-a"))?;
        let work_id = job_id_of(&output)?;
        let record = fx.jobs.get(&work_id).map_err(ctx("get job"))?;
        let input = WorkDriverJobInput::from_job(&record).map_err(ctx("decode input"))?;
        assert_eq!(input.tenant, Some(TenantId::from_str("tenant-a")));

        // An unscoped caller on an untenanted goal stamps no tenant.
        let unscoped = fixture_scoped(GoalStatus::Active, None, None)?;
        let unscoped_output = run_model(
            &WorkDriverEnqueueOperation,
            &unscoped.ctx,
            serde_json::json!({ "goal_id": GOAL }),
        )
        .await
        .map_err(ctx("enqueue unscoped"))?;
        let unscoped_work_id = job_id_of(&unscoped_output)?;
        let unscoped_record = unscoped
            .jobs
            .get(&unscoped_work_id)
            .map_err(ctx("get job"))?;
        let unscoped_input =
            WorkDriverJobInput::from_job(&unscoped_record).map_err(ctx("decode input"))?;
        assert_eq!(unscoped_input.tenant, None);
        Ok(())
    }

    #[tokio::test]
    async fn a_scoped_caller_cannot_enqueue_on_a_foreign_tenants_goal() -> TestResult {
        // Reference: the same goal id, but no goal exists at all for an
        // unrelated tenant — this is the "unknown goal" error shape.
        let missing = fixture_scoped(GoalStatus::Active, None, Some("tenant-b"))?;
        let reference = run_model(
            &WorkDriverEnqueueOperation,
            &missing.ctx,
            serde_json::json!({ "goal_id": GOAL }),
        )
        .await;

        // A goal that belongs to tenant-a, approached by a tenant-b caller.
        let foreign = fixture_scoped(GoalStatus::Active, Some("tenant-a"), Some("tenant-b"))?;
        let result = run_model(
            &WorkDriverEnqueueOperation,
            &foreign.ctx,
            serde_json::json!({ "goal_id": GOAL }),
        )
        .await;

        assert!(
            matches!(result, Err(OpError::InvalidArguments(ref message)) if message.contains("unknown goal")),
            "expected InvalidArguments(unknown goal), got {result:?}"
        );
        assert_eq!(
            format!("{result:?}"),
            format!("{reference:?}"),
            "a foreign goal must fail exactly like a missing one"
        );
        let page = foreign
            .jobs
            .list(&JobListQuery::default())
            .map_err(ctx("list"))?;
        assert!(
            page.jobs.is_empty(),
            "no job may be admitted against a foreign goal"
        );
        Ok(())
    }

    /// A `Ready` work_driver job for `tenant` driving `goal_id`, admitted
    /// directly into a `JobStore` (bypassing `work_driver.enqueue` itself) —
    /// used to simulate a foreign tenant's already-active run.
    fn foreign_active_job(id: &str, tenant: &str, goal_id: &str) -> StoredJob {
        let now = Timestamp::now();
        let mut job = Job::new(
            WorkId::from_str(id),
            JobKind::Custom(WORK_DRIVER_JOB_KIND.to_owned()),
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: 1,
                base_delay: SignedDuration::from_secs(1),
                factor: 1.0,
                max_delay: SignedDuration::from_secs(1),
                jitter: 0.0,
            },
            now,
        );
        job.state = JobState::Ready;
        StoredJob {
            job,
            scope: JobScope::new(
                TenantId::from_str(tenant),
                WorkspaceId::from_str("ws"),
                ApprovalActor::Operator {
                    id: "test-operator".to_owned(),
                },
            ),
            input: serde_json::json!({ "goal_id": goal_id }),
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

    /// H12 regression for the active-run guard: a same-named goal of a
    /// foreign tenant must neither block a scoped caller's own run nor leak
    /// the foreign job's id — the same rule `load_driver_job` already
    /// applies to single-job reads (see module docs, `Job kind and payload`).
    #[tokio::test]
    async fn a_same_named_goal_of_a_foreign_tenant_does_not_block_or_leak() -> TestResult {
        // tenant-a already drives its own goal `GOAL`, admitted directly
        // (not through `enqueue`, which would itself be scoped to tenant-a).
        let fx = fixture_scoped(GoalStatus::Active, Some("tenant-b"), Some("tenant-b"))?;
        let foreign_id = "job-tenant-a-active-run";
        fx.jobs
            .admit(&foreign_active_job(foreign_id, "tenant-a", GOAL))
            .map_err(ctx("admit tenant-a's active run"))?;

        // tenant-b's own, unrelated goal happens to share the same id.
        let output = run_model(
            &WorkDriverEnqueueOperation,
            &fx.ctx,
            serde_json::json!({ "goal_id": GOAL }),
        )
        .await
        .map_err(ctx("enqueue must not be blocked by a foreign tenant's run"))?;

        let rendered = format!("{output:?}");
        assert!(
            !rendered.contains(foreign_id),
            "response must not leak the foreign tenant's job id: {rendered}"
        );
        Ok(())
    }

    /// Unscoped (single-user) behaviour is unchanged by the H12 fix above:
    /// without a tenant scope the caller still sees every tenant's active
    /// runs and is still blocked by one on the same goal id.
    #[tokio::test]
    async fn an_unscoped_caller_is_still_blocked_by_any_tenants_active_run() -> TestResult {
        let fx = fixture(true, GoalStatus::Active)?;
        let existing_id = "job-tenant-x-active-run";
        fx.jobs
            .admit(&foreign_active_job(existing_id, "tenant-x", GOAL))
            .map_err(ctx("admit tenant-x's active run"))?;

        let result = run_model(
            &WorkDriverEnqueueOperation,
            &fx.ctx,
            serde_json::json!({ "goal_id": GOAL }),
        )
        .await;
        assert!(
            matches!(result, Err(OpError::Execution(ref message)) if message.contains(existing_id)),
            "expected an active-run refusal naming the existing job, got {result:?}"
        );
        Ok(())
    }

    // ── work_driver.report (R18 D-E) ─────────────────────────────────────

    use harw_plan_bridge::{WORK_DRIVER_REPORT_TOOL, WorkerReport, WorkerReportStatus};

    fn recorded_text(output: &harw_tools::ToolOutput) -> TestResult<&str> {
        match output {
            harw_tools::ToolOutput::Text { content } => Ok(content.as_str()),
            other => Err(TestError::Unexpected(format!(
                "expected a recorded report, got {other:?}"
            ))),
        }
    }

    fn rejection(output: &harw_tools::ToolOutput) -> TestResult<&str> {
        match output {
            harw_tools::ToolOutput::Error { message } => Ok(message.as_str()),
            other => Err(TestError::Unexpected(format!(
                "expected a tool error, got {other:?}"
            ))),
        }
    }

    /// WD-01: a valid report call is recorded and becomes exactly
    /// `WorkerReport::into_summary`.
    #[test]
    fn wd01_a_valid_report_is_recorded_and_summarised() -> TestResult {
        let slot = WorkerReportSlot::new();
        let arguments = serde_json::json!({
            "status": "done",
            "criteria_addressed": [1, 0],
            "changed_paths": ["src/a.rs"],
            "summary": "a fertig",
        });
        let output = record_worker_report(&slot, 2, arguments.clone());
        assert!(recorded_text(&output)?.contains("status done"));
        let report = slot.take().ok_or(TestError::Missing("recorded report"))?;
        let expected: WorkerReport = serde_json::from_value(arguments).map_err(ctx("decode"))?;
        assert_eq!(report, expected);
        let summary = report.into_summary();
        assert_eq!(summary.outcome, harw_plan_bridge::WorkerOutcome::Done);
        assert_eq!(summary.artifacts, vec!["src/a.rs".to_owned()]);
        assert_eq!(summary.criteria_addressed, vec![0, 1]);
        assert!(slot.take().is_none(), "take empties the slot");
        Ok(())
    }

    /// WD-03: an invalid report is a tool error to the worker and is not
    /// recorded.
    #[test]
    fn wd03_an_invalid_report_is_a_tool_error_and_not_counted() -> TestResult {
        let slot = WorkerReportSlot::new();
        for (arguments, needle) in [
            (
                serde_json::json!({ "status": "blocked", "summary": "s" }),
                "blocker",
            ),
            (
                serde_json::json!({ "status": "done", "summary": "s", "criteria_addressed": [2] }),
                "out of range",
            ),
            (
                serde_json::json!({ "status": "done", "summary": "s", "changed_paths": ["../x"] }),
                "../x",
            ),
            (
                serde_json::json!({ "status": "done", "summary": " " }),
                "summary is empty",
            ),
            (
                serde_json::json!({ "status": "done", "summary": "s", "outcome": "done" }),
                "invalid arguments",
            ),
            (
                serde_json::json!({ "status": "finished", "summary": "s" }),
                "invalid arguments",
            ),
            (serde_json::json!("done"), "invalid arguments"),
        ] {
            let output = record_worker_report(&slot, 2, arguments.clone());
            let message = rejection(&output)?;
            assert!(message.contains(needle), "{arguments}: {message}");
            assert!(message.contains("nothing was recorded"), "{message}");
            assert!(slot.take().is_none(), "{arguments} must not be recorded");
        }
        Ok(())
    }

    /// WD-04: the last valid report of the turn wins; a later invalid one
    /// does not replace it. A top-level `null` counts as absent.
    #[test]
    fn wd04_the_last_valid_report_of_the_turn_wins() -> TestResult {
        let slot = WorkerReportSlot::new();
        recorded_text(&record_worker_report(
            &slot,
            1,
            serde_json::json!({ "status": "partial", "summary": "erst halb" }),
        ))?;
        recorded_text(&record_worker_report(
            &slot,
            1,
            serde_json::json!({ "status": "done", "summary": "fertig", "blockers": null }),
        ))?;
        rejection(&record_worker_report(
            &slot,
            1,
            serde_json::json!({ "status": "failed", "summary": "" }),
        ))?;
        let report = slot.take().ok_or(TestError::Missing("recorded report"))?;
        assert_eq!(report.status, WorkerReportStatus::Done);
        assert_eq!(report.summary, "fertig");
        Ok(())
    }

    #[test]
    fn the_report_tool_serves_exactly_one_non_strict_tool() -> TestResult {
        use harw_extension_api::ToolProvider as _;
        let provider = WorkerReportToolProvider::new(Arc::new(WorkerReportSlot::new()), 3);
        let tools = provider.tools();
        let [harw_tools::ToolSpec::Function(spec)] = tools.as_slice() else {
            return Err(TestError::Unexpected(format!("{tools:?}")));
        };
        assert_eq!(spec.name.as_str(), WORK_DRIVER_REPORT_TOOL);
        assert!(!spec.strict);
        assert!(spec.description.contains("3 acceptance criteria"));
        assert_eq!(
            spec.parameters.required,
            Some(vec!["status".to_owned(), "summary".to_owned()])
        );
        let schema = serde_json::to_value(&spec.parameters).map_err(ctx("schema"))?;
        let reference = WorkerReport::input_schema();
        let keys = |value: &serde_json::Value| -> std::collections::BTreeSet<String> {
            value["properties"]
                .as_object()
                .map(|map| map.keys().cloned().collect())
                .unwrap_or_default()
        };
        assert_eq!(keys(&schema), keys(&reference));
        assert_eq!(
            schema["properties"]["status"]["enum"],
            reference["properties"]["status"]["enum"]
        );
        assert!(
            provider
                .executor(&harw_tools::ToolName::new(WORK_DRIVER_REPORT_TOOL))
                .is_some()
        );
        assert!(
            provider
                .executor(&harw_tools::ToolName::new("work_driver.enqueue"))
                .is_none()
        );
        Ok(())
    }

    /// WD-05: `work_driver.report` is offered by no registry profile and by no
    /// operation — only the job worker registers it, for WorkDriver worker
    /// turns.
    #[test]
    fn wd05_the_report_tool_is_not_offered_outside_work_driver_workers() {
        use harw_registry_defaults::profile::RegistryProfile;
        for profile in RegistryProfile::ALL {
            assert!(
                !profile
                    .registered_tool_names()
                    .contains(&WORK_DRIVER_REPORT_TOOL),
                "{profile:?} must not carry {WORK_DRIVER_REPORT_TOOL}"
            );
        }
        let mut registry = harw_operations::registry::OperationRegistry::new();
        crate::register_all(&mut registry);
        let _registered = crate::register_work_driver_tools(
            &mut registry,
            &harw_plan::PlanToolConfig::enabled_defaults(),
        );
        assert!(registry.find_by_name(WORK_DRIVER_REPORT_TOOL).is_none());
    }

    // ── UIA caller (R18 F6) ──────────────────────────────────────────────

    #[test]
    fn the_uia_caller_uses_its_own_section_or_the_default_spec() {
        let default = WorkDriverCaller::for_uia(None);
        assert_eq!(default.role, UIA_WORK_DRIVER_ROLE);
        assert_eq!(default.spec, WorkDriverCaller::uia_default_spec());
        assert_eq!(
            default.spec.worker_role,
            harw_plan_bridge::DEFAULT_WORKER_ROLE
        );
        assert!(default.spec.stall_iterations <= default.spec.max_iterations);
        let configured = WorkDriverCaller::for_uia(Some(&spec()));
        assert_eq!(configured.spec, spec());
        assert_eq!(configured.role, UIA_WORK_DRIVER_ROLE);
    }

    /// WD-06: the UIA's `work_driver.enqueue` requires approval on every
    /// surface and admits a job with the UIA caller spec.
    #[tokio::test]
    async fn wd06_uia_enqueue_requires_approval_and_uses_the_uia_spec() -> TestResult {
        use harw_operations::{ApprovalPolicy, Surface};
        let surfaces = &WorkDriverEnqueueOperation.meta().surfaces;
        assert!(
            surfaces.iter().any(|surface| matches!(
                surface,
                Surface::ModelTool {
                    approval: ApprovalPolicy::Always,
                    ..
                }
            )),
            "model surface must always ask: {surfaces:?}"
        );
        assert!(
            !surfaces.iter().any(|surface| matches!(
                surface,
                Surface::ModelTool {
                    approval: ApprovalPolicy::None,
                    ..
                }
            )),
            "no model surface may skip the approval"
        );

        let fx = fixture_with(Some(WorkDriverCaller::for_uia(None)), GoalStatus::Active)?;
        let output = run_model(
            &WorkDriverEnqueueOperation,
            &fx.ctx,
            serde_json::json!({ "goal_id": GOAL }),
        )
        .await
        .map_err(ctx("enqueue as the UIA"))?;
        let work_id = job_id_of(&output)?;
        let record = fx.jobs.get(&work_id).map_err(ctx("get job"))?;
        let input = WorkDriverJobInput::from_job(&record).map_err(ctx("decode input"))?;
        assert_eq!(input.orchestrator_role, UIA_WORK_DRIVER_ROLE);
        assert_eq!(input.spec, WorkDriverCaller::uia_default_spec());

        // Overrides narrow the UIA spec like any other caller's.
        let widened = run_model(
            &WorkDriverEnqueueOperation,
            &fx.ctx,
            serde_json::json!({
                "goal_id": GOAL,
                "overrides": { "max_iterations": WorkDriverSpec::DEFAULT_MAX_ITERATIONS + 1 }
            }),
        )
        .await;
        assert!(
            matches!(widened, Err(OpError::InvalidArguments(_))),
            "{widened:?}"
        );
        Ok(())
    }
}
