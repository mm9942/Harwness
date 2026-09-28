# Driving workers to a goal: the WorkDriver

> Status: implemented. In the tree: the `[work_driver]` table and its
> lowering (`harw-agent-dsl`, DSL §8.3); the pure decision core
> `WorkDriver::decide` (`harw-plan-bridge/src/work_driver.rs`); the
> sandboxed `VerificationExecutor` (`harw-plan-bridge/src/verify_exec.rs`);
> the `work_driver.enqueue` / `.status` / `.stop` operations and the
> worker report tool `work_driver.report` (`harw-ops/src/work_driver.rs`);
> and the job worker under `harw serve` that drives the rounds of one run
> in one claim (`harw-cli/src/job_worker_work_driver.rs`), with sandboxed
> verification (§7), provider pacing (§6) and a configurable number of
> concurrent runs (§6). R18 (contract
> `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md` §8) replaced
> the free-text status line of workers by the report tool (§4), and lets
> the UIA start a run (§2). Open: a custom orchestrator that extends
> `child-orchestrator-base@1` inherits the base's `[tools]`, and the runtime
> clamps it under that base — whether `work_driver.*` reaches its tool list
> depends on that base.

The WorkDriver is a supervisor loop. Given the current `Active` goal, it
hands small, isolated pieces of work to worker agents, runs one central
verification per wave, asks an optional judge about what evidence alone
cannot decide, and feeds precise feedback back to the same workers until
every acceptance criterion is evidenced — or a limit stops it. It never
declares the goal achieved; it proposes that to a human.

- Settings: [`[work_driver]` in the agent definition DSL, §8.3](../design/agent-definition-dsl.md)
- Decision core, in reference form: [planning tool design, "Work driver"](../design/planning-tool-v1.md)
- Example definition: [`examples/agents/driven-orchestrator/`](../../examples/agents/driven-orchestrator/)

## 1. Who may run it

An orchestrator whose definition has a `[work_driver]` table, and the UIA
(§2). The table is authority, not behavior: it grants no new right, it
only uses the right to delegate the definition already has. Lowering
therefore requires

- an orchestrator role (one that may spawn workers),
- a spawn depth above 0,
- `worker_role` — and `judge_role`, if set — listed as a spawn target in
  `[delegation] targets` or `[spawn] child_orchestrators`.

Otherwise lowering fails with `HARW-DRIVER-004`. The other diagnostics:
`HARW-DRIVER-001` unknown key, `-002` value out of range, `-003` missing or
malformed role name, `-005` blank `verify` entry.

```toml
[delegation]
targets = ["explorer", "executor", "analyst"]

[work_driver]
worker_role = "executor"          # required; must be a spawn target
judge_role = "analyst"            # optional; must be a spawn target
max_iterations = 8                # 1..=1000, default 8
max_parallel_workers = 3          # 1..=64, default 4
max_attempts_per_worker = 3       # 1..=100, default [lifecycle] max_attempts, else 4
stall_iterations = 2              # 1..=max_iterations, default 2
verify = ["cargo test --workspace", "cargo clippy --workspace -- -D warnings"]
token_budget = 800000             # optional, >= 1
wall_budget_secs = 5400           # optional, >= 1
```

`verify` is not copied from `[verification] commands`; list the commands
the driver should run explicitly. Check a definition with
`harw agent check <dir>` (see the [agent compiler guide](agent-compiler.md)).

## 2. Starting a run: the `work_driver.enqueue` tool

There is no TUI slash command and no `harw` subcommand for the WorkDriver.
It is started by the orchestrator itself through the model tool
`work_driver.enqueue` (or through the Web API `POST
/api/work-driver/enqueue` on an orchestrator's behalf). Both surfaces
always require approval.

**The UIA** may start a run too (R18 F6): the runtime gives the UIA root
the caller `WorkDriverCaller::for_uia` — the UIA definition's own
`[work_driver]` table if it has one, otherwise the default spec
`WorkDriverCaller::uia_default_spec` (DSL defaults: 8 rounds, 4 parallel
workers, 4 attempts, stall 2; worker role `implementer`; no judge role; no
own `verify` commands; no own budgets). The run is recorded with
`orchestrator_role = "user-interface"`. Nothing else changes: the tool asks
a human on every call and overrides can only narrow.

| Tool | Does | Approval |
|---|---|---|
| `work_driver.enqueue` | admits a durable `work_driver` job for the current goal, returns its job id | always |
| `work_driver.status` | reads the job and its round state | none (read-only) |
| `work_driver.stop` | cancels the job, with an optional reason | always |

Arguments of `work_driver.enqueue`:

| Argument | Meaning |
|---|---|
| `goal_id` (required) | id of the current goal; it must be `Active` |
| `plan_id` | plan to drive against; defaults to the plan bound to the goal, and must match it if both are set |
| `overrides` | optional limits for this run: `max_iterations`, `max_parallel_workers`, `max_attempts_per_worker`, `stall_iterations`, `token_budget`, `wall_budget_secs` |

The call fails closed, in this order: no caller (no `[work_driver]` in the
caller's IR and not the UIA: `NotAvailable`, before any store is touched);
a missing `goal_id`; an
override that would widen a limit; no job store, goal tooling disabled
(`[tools.plan] enabled = true` is needed) or no principal; a goal that is
not the current one or not `Active`; a plan mismatch; another active run on
the same goal.

Overrides can only **narrow**. Each must be at least 1 and not larger than
the orchestrator's own value (a budget the definition leaves unbounded may
be bounded); a larger value is rejected, never silently clamped. Roles and
`verify` cannot be overridden at all. The job's budget mirrors the
effective token and wall-clock budgets, so the job ledger enforces them
too. The job gets a single attempt: a crashed driver is not restarted
behind the operator's back.

`work_driver.status` returns the job plus the round state the job worker
publishes under `<job-root>/work_driver/<job-id>.json`: round number,
workers with their scopes and last results, the wave's verification state,
token usage and cache hit ratio, the judge's latest verdict and the
rationale lines of the latest decision. A missing state file just means
the first round has not finished.

## 3. What one round decides

`WorkDriver::decide` is pure: no I/O, no randomness, no system clock (`now`
is injected; wall time is `now - started_at`). Workers, criteria and scope
hints are sorted first, so the same snapshot always yields the same plan.
Its input is the goal, its evaluation report, the round number, the
current workers, scope hints (derived from the plan, see §5), usage so far,
the limits, the wave's verification state and an optional judge verdict. Its output is an
ordered list of steps plus one rationale line per decision. Executing the
steps is the caller's job.

The decision runs in this order:

1. **Terminal goal** (`Achieved`, `Abandoned`, `Superseded`): nothing to do.
2. **Ready to propose**: every worker is settled (`Done` or `Blocked`), all
   evidence-decidable criteria are met, no invariant is violated, the
   wave's verification passed, and — if criteria remain that only the
   judge can decide — the judge said `passed`. The only step is
   `ProposeAchieved`.
3. **A limit is hit** (checked in this order: rounds, tokens, wall time,
   stall): the only step is `GiveUp` with the limit and its numbers. A
   proposal in the same snapshot wins over a limit.
4. **Escalate** every `Blocked` worker with its question.
5. **Settled wave** (every worker `Done`/`Blocked`):
   - verification not yet run → one `Verify` over all workers of the wave;
   - verification failed → `Continue` each `Done` worker with the open
     criteria of its scope and the failing lines that mention its paths
     (lines that mention no worker's path go to everyone); if no worker
     owns the failure, `Escalate`;
   - verification passed but evidence still open or invariants violated →
     `Continue` the responsible workers with those gaps;
   - verification passed, evidence complete → `Judge` the remaining
     criteria; after a negative verdict its `missing` list becomes
     feedback, or a dedicated `judge-followup` scope if no worker is
     available to continue.
6. **Running wave**: a worker that reported `Partial` is continued with its
   open criteria; one that reported `Failed` is respawned with a handoff.
7. **Delegate** open criteria that no worker covers yet, into the free
   slots. A worker covers the criteria of its scope plus the ones it
   reported in `criteria_addressed` (§4); the open ones among them are its
   feedback.

`Continue` becomes `Respawn` when the worker's context exceeds the respawn
threshold or its continuation chain has used up `max_attempts_per_worker`.

| Step | Meaning |
|---|---|
| `Delegate` | start a new worker for an uncovered scope, with a self-contained task |
| `Continue` | append short feedback to the same worker |
| `Respawn` | replace a worker; the new one gets a compact handoff, not the old transcript |
| `Verify` | one central verification over the combined state of the wave |
| `Judge` | ask the judge about criteria evidence cannot decide |
| `ProposeAchieved` | propose to a human that the goal is achieved |
| `Escalate` | a human decision is needed |
| `GiveUp` | a limit ended the run |

Between rounds the caller carries the state: a worker that got
`Delegate`/`Continue`/`Respawn` is running again, `attempts` resets on
respawn, verification and judge verdict reset whenever the wave gets new
work, the goal is re-evaluated after `Verify`, and rounds without progress
are counted for the stall limit (§6).

## 4. How a worker reports: `work_driver.report`

Every WorkDriver worker turn — and only those — gets the tool
`work_driver.report`. The worker ends its turn by calling it; a status line
or JSON block in its text is not read.

| Argument | Meaning |
|---|---|
| `status` (required) | `done` (scope finished, ready for the central verification), `partial` (continue later), `blocked` (a human must decide), `failed` (hard failure) |
| `summary` (required) | short summary; must not be empty |
| `criteria_addressed` | indices of the goal's acceptance criteria the worker worked on (as numbered in its task) |
| `changed_paths` | workspace-relative files it changed (`/`, no absolute path, no `..`, no `:`) |
| `blockers` | open questions; required (non-empty) for `blocked` |

- Unknown fields and invalid values are rejected as a tool error that names
  the rule; the worker can correct and call again. A rejected report is not
  recorded. A top-level `null` counts as absent.
- The last valid report of the turn wins.
- `blocked`/`failed` carry the blockers (joined with `; `), else the
  summary, as their reason; `changed_paths` become the worker's artifacts,
  merged with the files the driver saw change.
- **No report** → the turn counts as `partial` with the summary
  `no report: <last text>` and the suggestion to call the tool. It is never
  read as `done`.
- `criteria_addressed` only steers routing (who gets feedback on which
  criterion, which criterion counts as covered). Criteria and the goal are
  only ever met by verification or a human.

The first task of a worker is structured: `## Goal`, `## Scope`,
`## Acceptance criteria` (index, description, `verified by: …`),
`## Owned paths`, `## Notes` (e.g. the judge's gaps), `## Rules`, the
central `verify` commands of the run, and `## Report` (the contract
above). Follow-ups send only `## Driver feedback` (and an
`## Operator answer` after an escalation). The worker's role shapes its
turn: a read-only role (`harw_ops::kanban::role_access`) never writes, and
the instructions of a custom role definition lead its stable prompt
prefix.

## 5. Design principles

**Smallest isolated scopes.** Every open criterion becomes its own scope
unless a scope hint groups it with others. Scope hints come from the plan
bound to the goal (`scope_hints_from_plan`): a plan node whose
`write_scope` names concrete paths (globs are skipped; a symbol entry
counts with its file) and that carries one of the goal's acceptance
criteria (same description, or same verification steps) becomes the hint
`plan-<node id>`. A scope without a hint owns the artifact paths of its
criterion's verification steps. A criterion without such paths — for
example one verified only by a command — gets the **workspace scope**
(`.`): its worker may change any file that no other worker owns. The
judge-followup scope is a workspace scope as well.

**Parallel workers, disjoint owned paths.** Each scope owns paths that only
its worker changes. Two scopes overlap when a path equals another or lies
inside its directory. A new scope that overlaps a running worker is
deferred to a later round; two new scopes of the same round that overlap
are merged into one. At most `max_parallel_workers` workers exist at once.
The workspace scope overlaps nothing in this check; instead the job worker
runs at most one workspace-scope worker per concurrently running block,
and after each block it checks every changed file: a file in a block
worker's own paths belongs to it, any other file belongs to the block's
workspace-scope worker, and a file in the paths of any other worker of the
run — or with no owner at all — blocks the whole run for a human (fail
closed).

**Continue the same worker.** Follow-up work goes to the worker that
already holds the scope, as short appended feedback: only the open criteria
of its scope and the failing evidence that concerns it. Its prompt prefix
stays unchanged, so its prompt cache stays warm. A fresh worker is the
exception — exhausted attempts, a context above the threshold, or a hard
failure — and it gets a compact handoff (scope, owned paths, what was done,
artifacts, suggested next step, open points), not the predecessor's
history.

**Central verification, once per wave.** Workers never build or test
themselves; every delegated task says so. `Verify` appears only when
*every* worker of the wave has reported `Done` or `Blocked`, never per
worker. One verification over the complete, combined state is the only one
that means anything: a worker's own green run verifies an intermediate
state that never ships, and parallel builds fight over the same build
cache.

**A judge with a stable prefix.** Criteria without a verification step, or
with a manual one, cannot be decided from evidence. They go to the judge
only after verification is green and all other evidence is complete. The
question starts with a fixed lead sentence and then lists the criteria by
index and description, so repeated judge calls share their prefix. The
verdict (`passed`, `comment`, `missing`; the old names `met` and
`rationale` are still read) feeds back into the loop.

**Never sets `Achieved`.** The driver only proposes. Only a human actor may
move a goal to `Achieved`: the user confirms the proposal with
`/goal achieve <reason>`. The model cannot call that action.

## 6. Limits

Every limit is absolute: reached means `GiveUp`.

| Limit | From | Stops when |
|---|---|---|
| rounds | `max_iterations` | the round number reaches it |
| tokens | `token_budget` | tokens used over all workers reach it |
| wall time | `wall_budget_secs` | `now - started_at` reaches it |
| stall | `stall_iterations` | that many consecutive rounds without progress: after a verification, progress is a new maximum of met criteria **or** a new minimum of failing verification steps; the first measurement of a claim only sets the baseline |
| parallelism | `max_parallel_workers` | further scopes wait for a free slot |
| continuation chain | `max_attempts_per_worker` | the worker is respawned instead of continued |

The token and wall-clock budgets are also written into the durable job's
budget, so the job ledger enforces them independently of the driver.

**Provider limits.** Workers share the service's provider instance and its
rate limiter. The job worker respects the provider's limits instead of
working around them ([DEC-003](../planning/70-decisions/DEC-003-provider-limits.md)):

- **Pacing:** before each wave chunk it awaits
  `ModelProvider::pacing_wait()` — the wait that header-reported limits, a
  429 cooldown and the configured TPM/RPM budgets demand.
- **Wave width (AIMD):** at most the provider's concurrency minus one slot
  for orchestrator and judge; halved after an HTTP 429, +1 after a clean
  wave.
- **429 back-off:** a 429 is not a worker failure; the worker's session
  continues after `Retry-After` or an exponential, capped back-off, without
  using up an attempt.

**Concurrent runs.** A run holds its claim for all its rounds, so the job
worker drives `work_driver` jobs in their own lane. The lane size is
`JobWorkerOptions::max_work_driver_runs`: 2 by default, never above
`[jobs] max_running` (`JobWorkerOptions::from_config`). While the lane is
full, further `work_driver` jobs stay `Ready`.

## 7. Verification

`VerificationExecutor` runs one verification per call over a list of
steps, sequentially, at wave level. It never reports "passed" without
evidence:

- **Commands** (`verify` entries and `Command` steps of criteria) never run
  unsandboxed on the host. Each goes to the job coordinator as a job with
  sandbox profile `SandboxProfileName::WorkspaceBuild` and
  `SandboxRequirement::Required`; the
  coordinator refuses to start it if the backend cannot enforce every
  dimension. Under `harw serve` the job worker builds this coordinator
  itself: backend is the Landlock trampoline `harw-job-exec` (next to the
  `harw` binary or on `PATH`), else `bwrap`; job records go to
  `<home>/jobs/verify`, the workspace root is the `serve` working
  directory, and the environment is reduced to `PATH`, `HOME`,
  `CARGO_HOME` and `RUSTUP_HOME`. Without a sandbox backend every command
  step escalates as `Unverifiable`; it never runs unsandboxed. The job is
  always `Required`, which includes enforced resource limits: set
  `HARW_VERIFY_CGROUP_ROOT` to a delegated cgroup v2 root (for example a
  `Delegate=yes` subgroup of the `harw serve` unit). Without one, commands
  also come back `Unverifiable`.
- Only one verification runs per workspace at a time: the executor holds
  an exclusive lock on `<workspace>/.harw/verify.lock`.
- The executor additionally checks the returned sandbox report; if it is
  not fully enforced, the result is discarded as `Unverifiable` even when
  the exit code matches.
- A command is split into words **without a shell** (single and double
  quotes and backslash are understood). Shell syntax — pipes, redirections,
  `$` expansion, globs, `;`, `&&`, variable assignments — is rejected as
  `Unverifiable` before it reaches the runner. Write one command per
  `verify` entry.
- An unknown exit, a runner error or an empty run is `Unverifiable`.
- **Artifact** steps of the goal's criteria and invariants are part of
  every central verification (deduplicated by path): each checks existence
  and a content digest under the workspace root. A passed artifact becomes
  evidence of kind `diff` with the step's path as locator, which is how the
  goal evaluation matches it. **Trace** and **manual** steps are not sent
  to the executor (they would always be `Unverifiable`); manual criteria go
  to the judge.

Each executed command yields an evidence reference whose kind is inferred
from the command (`cargo test`/`nextest` → `CargoTest`, `cargo clippy` →
`Clippy`, otherwise `Other`), with a digest over the tail of its output. Failing lines
become the `failing` list that the driver routes back to the workers whose
paths they mention.

**Judge answers.** The judge must answer with a JSON verdict
(`passed`, optional `comment`, `missing`). An answer without one is not
read as "not passed": the verdict stays open and the next round asks
again, reminding it of the format. After 3 such answers in a row the run
escalates (`work_driver:NeedsInput`) with the reason and the last answer.

## 8. Example

[`examples/agents/driven-orchestrator/`](../../examples/agents/driven-orchestrator/)
is a child orchestrator on `child-orchestrator-base@1` with a
`[work_driver]` table. `worker_role = "executor"` and
`judge_role = "analyst"` are both listed in `[delegation] targets`, and the
base's `max_depth = 1` provides the spawn depth, so it lowers without
`HARW-DRIVER-004`:

```sh
harw agent check ./examples/agents/driven-orchestrator
```

Its `system.md` tells the orchestrator when to call `work_driver.enqueue`,
how to report `work_driver.status`, and to hand the final confirmation to
the user as `/goal achieve`.
