# harw-plan v1 — First-Class Planning Tool

> Status: implemented · Last reviewed: 2026-09-24

Binding: `coding-philosophy.md` §6 (planning is first-class), §7 (a plan is
an executable dependency graph), §8 (write-scope disjointness), §15
(failure/recovery), §19 (authority not established by prose). Also
`philosophy.md` §5 (goal != plan), §6 (the model does not own processes), §12
(monotone authority reduction).

This design describes a **vertical slice**: a crate `harw-plan` with core
data types, an in-memory and a file store, validation, and a minimal
synchronous API. Adapters (command / model tool / channel), the MCP bridge,
and TUI projection were follow-on work, not part of this slice.

## 1. Scope

Exactly the semantics from `coding-philosophy.md` §6.1:

```
create plan | inspect plan | add or update node | declare dependency |
declare read/write scope | declare contracts | declare acceptance criteria |
mark node ready | mark node blocked | invalidate stale nodes |
record evidence | close or supersede plan revision
```

Explicitly not: job admission, executor invocation, LLM interaction. The
planning tool **describes** work — it does not **execute** any.

## 2. Core types

```rust
// harw-plan/src/types.rs

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use harw_types::{PlanId, TaskId, RevisionId, PathOrSymbol, ContractRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanNodeStatus {
    Draft, Ready, InProgress, Blocked, Completed, Superseded, Invalidated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Criterion {
    pub description: String,
    pub verification: Vec<VerificationStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VerificationStep {
    Command  { cmd: String, expect_exit: i32 },
    Artifact { path: String },
    TraceEvent { name: String },
    Manual   { note: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InvalidationCondition {
    ContractChanged(ContractRef),
    UpstreamNodeReopened(TaskId),
    RepoRevisionMoved,
    ManualInvalidate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub kind: String,          // "cargo-test", "clippy", "diff", "trace-span"
    pub locator: String,       // path, url, id
    #[serde(with = "time::serde::rfc3339")]
    pub attached_at: OffsetDateTime,
    pub actor: String,         // "worker-abc", "orchestrator", "human:alice"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanNode {
    pub id: TaskId,
    pub objective: String,
    pub dependencies: Vec<TaskId>,
    pub input_contracts: Vec<ContractRef>,
    pub output_contracts: Vec<ContractRef>,
    pub read_scope: Vec<PathOrSymbol>,
    pub write_scope: Vec<PathOrSymbol>,
    pub forbidden_scope: Vec<PathOrSymbol>,
    pub acceptance_criteria: Vec<Criterion>,
    pub invalidation_conditions: Vec<InvalidationCondition>,
    pub status: PlanNodeStatus,
    pub evidence: Vec<EvidenceRef>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: PlanId,
    pub revision: RevisionId,
    pub parent_revision: Option<RevisionId>,
    pub goal_statement: String,
    pub nodes: Vec<PlanNode>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}
```

`PlanId`, `TaskId`, `RevisionId`, `PathOrSymbol`, `ContractRef` live in
`harw-types`. Where not present there, declared as a newtype in
`harw-plan/src/ids.rs` with a TODO comment for later relocation.

---

## 3. Actions

```rust
// harw-plan/src/actions.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PlanAction {
    Create { plan_id: PlanId, goal: String },
    AddNode { node: PlanNode },
    UpdateNode { id: TaskId, objective: Option<String>, /* delta fields */ },
    AddDependency { child: TaskId, parent: TaskId },
    SetStatus { id: TaskId, status: PlanNodeStatus, reason: Option<String> },
    AttachEvidence { id: TaskId, evidence: EvidenceRef },
    Invalidate { ids: Vec<TaskId>, condition: InvalidationCondition },
    Supersede { new_parent_revision: RevisionId },
    Inspect,   // read-only
}
```

Every mutation runs through `PlanStore::apply(action) -> PlanResult<PlanEvent>`.
Actions are plain values; there is no `&mut PlanNode` in the API signature.

---

## 4. Store trait and reference implementations

```rust
// harw-plan/src/store.rs

pub trait PlanStore: Send + Sync {
    fn current(&self) -> PlanResult<Plan>;
    fn revision(&self) -> RevisionId;
    fn apply(&self, action: PlanAction, actor: &str) -> PlanResult<PlanEvent>;
    fn history(&self, since: Option<RevisionId>) -> PlanResult<Vec<PlanEvent>>;
}
```

Two implementations in this slice:

- `InMemoryPlanStore` — for tests, RwLock-backed.
- `FilePlanStore` — persistent under `<root>/plans/<plan_id>/rev-<n>.json`
  plus `history.jsonl` (append-only event log).

Both are `Send + Sync`.

---

## 5. Validation (coding-philosophy §6.3)

**File:** `harw-plan/src/validate.rs`.

`validate(&plan, &action)` must pass before `apply`. Binding rules:

- Referenced nodes exist.
- `AddDependency` never creates a cycle (DFS over the dependency graph).
- `SetStatus` follows a legal transition matrix
  (Draft→Ready→InProgress→Completed/Blocked; Superseded is terminal).
- `SetStatus(Completed)` requires at least one `EvidenceRef`.
- `AddNode` enforces `forbidden_scope ∩ write_scope == ∅`.
- `AddNode` enforces that `write_scope` is disjoint from every active
  `write_scope` of nodes in status `Ready | InProgress` (coding-philosophy
  §8).
- `Supersede` increases `revision` monotonically.
- `Invalidate` is only legal for nodes that are not `Completed` (already
  committed work is only invalidated via `Supersede`).
- `AttachEvidence` is idempotent on `(id, kind, locator)`.

Errors live in `PlanError` with variants `CycleDetected`,
`IllegalTransition`, `ScopeConflict`, `NodeMissing`, `RevisionRegressed`,
`EvidenceMissing`, `ForbiddenScopeOverlap`, `Io`, `Serde`.

---

## 6. Configuration (coding-philosophy §6.4)

```toml
[tools.plan]
enabled = true
persist = true
require_for_complex_work = true
validate_dependency_cycles = true
validate_write_conflicts = true
max_nodes = 256
```

`enabled = false` must remove the registration **completely**:

- `PlanToolConfig` struct with `#[serde(default)]` on every field; default is
  disabled except for tests.
- `PlanStore` is only constructed when `PlanToolConfig::is_enabled()`.
- When disabled: **no** `pub use` and **no** registry entry. The consumer
  gets an `Option<Arc<dyn PlanStore>>` at compile time, or no reference at
  all.

---

## 7. Authority (coding-philosophy §19)

Untrusted `PlanAction` inputs may never:

- choose `actor` themselves (the runtime sets the actor from the session),
- change `plan_id` outside their own session,
- set `revision` directly (only `apply` increments it),
- overwrite `created_at`/`updated_at` (runtime timestamps).

These rules are enforced in `apply` by discarding the corresponding payload
fields — not by trusting the prompt.

---

## 8. Vertical-slice boundaries (coding-philosophy §11)

**In the slice:**

- All types from §2.
- `PlanAction` (§3).
- The `PlanStore` trait with `InMemoryPlanStore` + `FilePlanStore`.
- Full validation (§5) with a test per rule.
- `PlanToolConfig` with an `enabled = false` default (§6).

**Not in the slice (follow-on work):**

- Adapters (command / model tool / channel).
- MCP bridging.
- TUI projection.
- Cross-crate consumers (`harw-job-runtime`, `harw-tools`).

The slice is self-verifying via `cargo test -p harw-plan`.

## Implementation

Implemented: `harw-plan/src/{types,actions,store,file_store,validate,
memory_store,ids,config,error}.rs`, plus later additions
(`admission.rs`, `catalog.rs`, `goal.rs`, `goal_store.rs`, `graph.rs`,
`mutation.rs`) beyond this original slice.

## Work driver (`harw-plan-bridge::work_driver`)

`WorkDriver::decide(&WorkDriveInput) -> WorkDrivePlan` is the pure decision
core of a supervisor that drives worker agents toward a `Goal`. Like
`PlanController::reconcile` it does no I/O, reads no clock (`now` is
injected; wall time is `now - usage.started_at`) and sorts workers, criteria
and scope hints, so the same snapshot always yields the same steps.

Input: the goal, its `GoalReport`, the iteration, the current workers
(`WorkerState`: scope with disjoint `owned_paths` and the criterion indices it
covers, continuation `attempts`, last `WorkerResultSummary`, context size,
cache hit ratio), optional scope hints, a `BudgetUsageSnapshot`,
`WorkDriveLimits`, the wave's `VerificationState` and an optional
`JudgeVerdict`.

Steps and when they appear:

| Step | When |
|---|---|
| `Delegate` | an open criterion no worker covers; up to `max_parallel_workers`; a scope overlapping a running worker is deferred, overlapping new scopes are merged |
| `Continue` | the default follow-up: same worker, compact appended feedback (open criteria of its scope, failing lines, judge gaps) — keeps its prompt cache warm |
| `Respawn` | continuation chain exhausted, context above `respawn_context_tokens`, or a hard `Failed`; carries a compact handoff |
| `Verify` | once per wave, only when every worker is `Done`/`Blocked` |
| `Judge` | verification green, evidence complete, remaining criteria are manual or have no verification step |
| `ProposeAchieved` | all criteria and invariants evidenced, verification green, judge satisfied — a proposal only; `Achieved` is set by a human actor |
| `Escalate` | a `Blocked` worker, or a failure no worker owns |
| `GiveUp` | iteration, token, wall-time or stall limit reached |

The caller owns the state between rounds: it marks a worker running again
(`last_result = None`) after `Delegate`/`Continue`/`Respawn`, resets
`attempts` on respawn, resets verification and judge whenever the wave gets
new work, re-evaluates the goal after `Verify`, and counts
`iterations_without_progress`.
