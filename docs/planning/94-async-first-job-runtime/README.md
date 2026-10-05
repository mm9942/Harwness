# PL-94 — Async-first, job-backed execution

Status: **implementation branch / validation pending**

Baseline: `dev@5d1853a3d94b66034382777ee332ecd0bf352590`

Implementation branch: `chatgpt/async-job-runtime-default`

Supersedes the integration assumptions in PL-93 where this document is stricter.

## Decision

Harw is **async-first and job-backed by default**.

A model, agent, TUI surface, Telegram surface, SDK client, or other application-level caller does not own workload process lifecycle. It submits governed work and receives a stable handle. Waiting is an explicit operation.

The durable job ledger identified by `WorkId` is the lifecycle source of truth. UI/background registries are projections and compatibility indexes, not schedulers.

## Invariants

1. **Agent delegation is async by default.**
   - `agents.delegate` and generated `transfer_to_*` calls submit a durable agent job.
   - The caller immediately receives `work_id` and `child_id`.
   - `background=false` or `wait=true` is the explicit synchronous compatibility path.
   - The rule propagates recursively to children, grandchildren, and later descendants.

2. **Agents are logical jobs, not fake shell commands.**
   - Agent admission still happens through `ManagedAgentSpawner` so authority, sandbox, budget, role, model routing, and session policy remain authoritative.
   - After admission, `AgentJobSubmitter` registers the run in the durable `JobStore`.
   - `DurableJobRunner` owns claim, fencing, heartbeat, cancellation, and terminal commit.
   - The child backend may execute an OS process, but that process is itself routed through `harw-command -> harw-job`.

3. **No UI-specific background semantics.**
   - TUI, CLI, Telegram, Web/Gateway, SDK, and workers consume the same job handle/state.
   - `AwaitingChild` is an explicit inline/legacy contract, not the default cross-surface protocol.
   - A surface does not need bespoke support for a background child lifecycle.

4. **Approvals fail closed without blocking the agent forever.**
   - If an approval responder exists, the child waits for that governed response.
   - If no responder is reachable, the requested operation is rejected.
   - The child turn then resumes so it can adapt or choose another path.
   - Missing UI is never treated as implicit approval.

5. **Application workload processes go through CommandPort.**
   - `shell.exec`, operator shell, sudo, LaTeX, job launch, compiled child execution, and container engine lifecycle use the job command runtime.
   - `fs.grep` is process-free and uses the internal symlink-safe search instead of spawning `rg`.
   - `container.run` submits Podman create/inspect/start/rm commands through `CommandPort`; absence of a command runtime fails closed.
   - The spawn ratchet rejects new application-level direct process launches.

6. **Infrastructure spawn exceptions remain explicit and narrow.**
   Allowed direct-spawn owners are limited to runtime executors, sandbox/bootstrap boundaries, service managers, build/release tooling, MCP stdio transport, and interactive host tooling that intentionally owns a terminal. Every exception is named in `xtask/spawn-policy.toml`; stale entries fail the gate.

## Current implementation

### Unified workload command path

PL-93's conflict-free implementation was ported onto the current `dev` baseline rather than merging its stale branch.

`harw-command` provides the application command port backed by `harw-job`. Shell, job tools, TUI operator shell, sudo/escalation, LaTeX and child backend execution are routed through it. Linux and Darwin job coordinators carry the execution semantics.

### Durable agent jobs

`harw-extension-api` now exposes:

- `AgentJobHandle { work_id, child }`
- `AgentJobSubmitter`

`harw-runtime::agent_job_wiring::RuntimeAgentJobSubmitter`:

1. receives an already-admitted child;
2. detaches it from parent lifetime;
3. admits a `JobKind::Custom("agent")` record;
4. claims and drives it using `DurableJobRunner`;
5. renews the lease while it runs;
6. links job cancellation to the child cancellation token;
7. commits success/failure/cancellation/blocked outcomes under the lease fence;
8. keeps `BackgroundChildren` only as a result/notification projection.

The runtime mounts the submitter once at the composition root and passes a weak deferred adapter into every child registry. This avoids a strong registry/spawner/runtime cycle while preserving async-by-default behavior for the full delegation tree.

### Non-blocking approval behavior

`child_approval::relay_child_approvals` is now the common child-approval continuation path. An undeliverable approval is rejected and fed back into the child turn rather than leaving the job parked solely because its current surface lacks approval UI.

### Process ratchet

PL-93's `xtask gates spawn` policy is retained and tightened. The previous `fs.grep` and `container.run` direct-spawn exceptions are removed.

## CURRENT / TARGET / DELTA

### CURRENT on this branch

- Workload command execution has one application-facing port.
- Agent delegation has a durable job submission capability.
- Root and descendant registries share the same logical job contract.
- Missing approval UI rejects-and-continues.
- Grep is process-free.
- Podman lifecycle commands use the job command port.
- Direct workload spawn exceptions are ratcheted.

### TARGET after validation

- No application-level workload process can bypass `CommandPort`.
- No default agent delegation can hold the parent turn in `AwaitingChild`.
- Every asynchronous agent has a stable `WorkId` visible to status/wait/cancel/result tooling.
- Surfaces render durable job state rather than implementing their own scheduler semantics.
- Crash/restart recovery can reclaim a durable agent record instead of treating it as an invisible background task.
- Remote/cloud placement can move the same job contract without changing agent-facing semantics.

### Remaining delta

- Run the full compiler/test/gate matrix on this exact branch SHA.
- Add/extend surface presentation for `work_id` where the current UI still prioritizes child/session IDs.
- Add crash/restart integration coverage for an in-flight durable agent job.
- Add explicit job-status/result lookup by `work_id` for agent jobs where only child-oriented result tooling currently exists.
- Replace the temporary local tenant binding in the root-runtime adapter with the canonical resolved tenant binding when that value is exposed at the composition root.
- Decide whether CLI/build/editor/MCP/service-manager direct process exceptions should later get dedicated ports; they are not model workload execution and are intentionally outside this wave.

## Required validation before merge

Run against the frozen head SHA:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo xtask gates all
cargo xtask gates spawn
cargo nextest run --workspace --all-features
cargo test --doc --workspace --all-features
cargo deny check
```

Additionally exercise:

- async root -> child -> grandchild delegation returns handles without `AwaitingChild`;
- explicit `wait=true` and `background=false` still use the inline path;
- cancellation by `WorkId` reaches the child;
- lease loss cancels the child and rejects stale completion;
- approval-present and approval-unavailable paths;
- `shell.exec`, `job.start`, sudo and operator shell all cross `CommandPort`;
- `container.run` create/inspect/start/rm all appear as job-runtime commands;
- no direct `rg` process is launched by `fs.grep`;
- runtime restart/reclaim of an in-flight agent job.

## Merge rule

Do not merge because the architecture looks right. Merge only when the exact head SHA passes the required gates and the durable-agent crash/restart test exists.

