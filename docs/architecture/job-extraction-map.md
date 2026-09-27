# Job extraction map (Migration Phase 6 input)

> **Status:** descriptive, R11. Facts were checked against the code at `HEAD` (`339cbd1`) plus
> the R11 working tree (`harw-job-core` being introduced in W1). Plan references:
> Eco-Doc §14, §33, §34, §52, §57; Job-Runtime-Doc (`docs/planning/20-job-runtime/`) §5–§16, §26.
> Companion docs: `harw-dependency-inversions.md` (J1–J8), `harw-workspace-inventory.md`.

## 1. Current job code (verified)

| Location | Size | What it is | Harwness coupling |
|---|---|---|---|
| `harw-job-runtime/src/{budget,error,job,lease,retry,stored}.rs` | ~6 modules | Governance model: `Budget`/`BudgetUsage`, `JobRuntimeError`, `Job`/`JobKind`/`JobState` (Pending→Ready→Running→Completed/Blocked/Failed/Cancelled), fenced `Lease`/`LeaseToken`, `RetryPolicy`, `StoredJob`/`JobScope`/`JobClaim`/`JobCompletion`/`JobCancellation`/`JobOutcome`/`ReclaimOutcome` | `WorkId`, `TenantId`, `WorkspaceId`, `ApprovalActor` (harw-types), `TraceContext` (harw-observe), `HarwError` (harw-macros), `JobKind::{Dream,Worker}` |
| ↳ R11 W1 working tree | — | These modules moved **unchanged** to `harw-job-core/src/`. `harw-job-runtime/src/lib.rs` now only re-exports them. | same (listed as "Known inversions" in `harw-job-core/src/lib.rs`) |
| `harw-job-runtime` dependents (Cargo) | 8 | `harw-session-store`, `harw-core`, `harw-cli`, `harw-runtime`, `harw-plan-bridge`, `harw-mcp-server`, `harw-knowledge`, `harw-ops` (verified) | — |
| `harw-session-store/src/job_store.rs` | 2 438 lines | `JobStore`: `admit/get/list/claim/renew/complete/cancel/unblock/get_approval/deny_blocked/retry/mark_ready/reclaim/reconcile_expired`. Per-job `fs4` exclusive file lock. Layout `<root>/jobs/{records/<id>.json, locks/<id>.lock, approvals/<id>.json}`. Symlink rejection, `safe_component` id check, corrupt record → `<id>.json.corrupt-<ts>`. `JobEventSink` hook. | `ApprovalActor`/`JobApproval` (approval flow), `SessionStoreError`. Root = profile dir (`JobStore::new(<profile>)`) |
| `harw-core/src/durable_job_runner.rs` (+ `execution_registry.rs`) | 944 lines | `DurableJobRunner`: claim → register under the full lease token → heartbeat renew every TTL/3 → commit under the same token. Lease-loss cancel + force abort. Drop guard. `JobOutbox` commit outbox. `JobStore` calls in `spawn_blocking`. | `CancelToken` (harw-types), `JobExecutionRegistry` (harw-core) |
| `harw-cli/src/job_worker.rs` | 5 059 lines | `harw serve` durable consumer. Runs `Worker`/`Dream`/Telegram `Custom` jobs as `EntryKind::JobPrompt` turns (no tools). Plan-node admission. | Fully Harwness (runtime assembly, transcripts, plan, telegram) |
| `harw-runtime/src/job_wiring.rs` | 452 lines | `SessionJobs` (one `JobManager` per assembly, `<project>/.harw/state/jobs/<id>/`), `JobEventRouter` (delivers notes along `JobOwner::delivery_chain`, UI events, bus invalidation), lineage | Sessions, agents, TUI notes |
| `harw-runtime/src/job_ledger.rs` | 888 lines | `JobStoreTransitions`: kanban `JobTransitions` over the durable `JobStore` | Kanban (harw-knowledge) |
| `harw-tool-job` (`manager`, `procfs`, `model`, `launcher`, `event`, `logs`, `progress`, `throttle`, `tools`) | ~5 400 lines (+ tests) | Process jobs: `JobManager` (start / start_piped / stop / wait / list / reload), process group via `process_group(0)`, **identity = PID + `/proc/<pid>/stat` field 22 start ticks** (`is_same_process_alive`), **group kill via `rustix::process::kill_process_group`** + escalation to SIGKILL + stragglers. `meta.json` **v1** (`META_VERSION = 1`: pid, proc_start_ticks, executed_on_host, harw_instance, owner, state…), atomic tmp+rename. Log follow/tail/query. Progress/severity detection. Notification throttle. `job.*` tools. | `JobOwner`/`JobLineage` (sessions), `ShellJobLauncher` (→ `harw-tool-shell` permission/sandbox path), `ToolProvider`, `Permission::ExecuteProcess`, `CancelToken` |
| `harw-agent-runner/src/job_child_backend.rs` | 1 134 lines | `JobChildBackend`: `ChildBackend` over a child process speaking `harwness.agent-child/v1`. In production through `JobManager::start_piped` (own pgrp, logs, `stop` on cancel). | `harw-core::child_backend` |
| `harw-sandbox/src/bwrap.rs` | 2 650 lines | `BwrapLauncher` (`discover`, `with_profile/network_mode/tmpfs/identity/host_path/read_only_paths`, `plan` → `BwrapCommandPlan`, `spawn`). `/run/harw` relay/egress sockets. | `SandboxProfile`, `NetworkMode`, cargo/tmux profiles |

Observations:
- There are **three state models**: governance `JobState`, process `harw_tool_job::JobState`, and the new
  `harw_job_core::LifecycleState`. There are **two ids**: `WorkId` and `harw_tool_job::JobId`.
- There are **two `jobs/` trees** with different roots: `JobStore` at `<profile>/jobs/{records,locks,approvals}`,
  and the process jobs at `<project>/.harw/state/jobs/<job_id>/{stdout.log,stderr.log,meta.json}`. Keep them distinct.
- Identity via `/proc` start ticks has no cfg gate. On non-Linux it silently returns `None`
  ("unknown") on reload.
- A process-group kill does not reach descendants that call `setsid` (Job-Runtime-Doc §10). That is the
  reason cgroup v2 is needed.

## 2. Concern → target crate

| Concern (today) | Source | Target crate | Notes |
|---|---|---|---|
| Job model, ids, lifecycle state machine, outcome, deadline, spec, enforcement report | new | **harw-job-core** | W1: done in working tree |
| Governance types `Budget`, `Lease`, `RetryPolicy`, `Job`, `StoredJob`… | `harw-job-runtime` | **harw-job-core** (moved). `harw-job-runtime` re-exports during migration. | Remove J1–J5 inversions in W2 |
| `JobScope` tenant/workspace/submitter | `harw-job-runtime::stored` | core: `JobScopeId`. Harwness mapping: session-store adapter | Eco §57 |
| `StoredJob::trace` | `harw-job-runtime::stored` | Harwness store adapter envelope | Eco §58 |
| Durable store mechanics: atomic write, per-job lock (fs4), fencing/epoch checks, reclaim/reconcile, corrupt quarantine, symlink rejection, `safe_component` | `harw-session-store/src/job_store.rs` | **harw-job-store** (generic, `cap-std` root handle per Job-Runtime-Doc §7) | Same on-disk layout (`jobs/records`, `jobs/locks`, `jobs/approvals`), same JSON |
| `JobStore` public API, `SessionStoreError` mapping, approvals (`JobApproval`, `deny_blocked`, `unblock`), `JobEventSink` | same | **harw-session-store** keeps a thin adapter with the **same API** over `harw-job-store` | Callers (core, cli, runtime, ops, plan-bridge, mcp-server, tui) are unchanged |
| Lease heartbeat, lease-loss cancel, commit outbox | `harw-core/src/durable_job_runner.rs` | **harw-job-runtime** (future coordinator; generic runner loop over `harw-job-store`) | `JobExecutionRegistry`/`CancelToken` binding stays in harw-core as an adapter |
| Job *content* execution (prompt turns, telegram, plan admission) | `harw-cli/src/job_worker.rs` | stays **harw-cli** (A) | Uses the coordinator instead of calling the store directly |
| Kanban ledger | `harw-runtime/src/job_ledger.rs` | stays **harw-runtime** (A) | Adapter over the session-store `JobStore` |
| Process spawn + identity: PID + start ticks → **pidfd** + `procfs` crate (start time, state) | `harw-tool-job/src/procfs.rs`, `manager.rs` | **harw-job-linux** (`process/`, `pidfd/`, `proc/`, `recovery/`) | `LinuxRecoveryIdentity` replaces `(pid, proc_start_ticks)`. Keep reading `meta.json` v1 fields. |
| Group termination (rustix `kill_process_group`, escalate, stragglers) | `harw-tool-job/src/procfs.rs`, `manager.rs` | **harw-job-linux** `cgroup/` (cgroup v2 `cgroup.kill`) with pgrp kill as fallback | `TerminationPolicy` (Job-Runtime-Doc §10) |
| Resource limits (cgroup v2 memory/cpu/pids, rlimits) | — (none today) | **harw-job-linux** `resources/` | `JobResources` type in core/spec |
| Sandbox policy + enforcement report (Landlock, cap drop, NO_NEW_PRIVS) | — (bwrap only) | **harw-job-linux** `sandbox/`, `capabilities/` → `SandboxReport` (core type) | Never report a single bool (Job-Runtime-Doc §12) |
| Pre-exec steps in the child (cgroup attach, rlimits, NO_NEW_PRIVS, cap drop, landlock, then `execve`) | — | **harw-job-exec**: a small safe **re-exec trampoline binary**. The parent spawns `harw-job-exec <policy-fd/json> -- argv…`. The trampoline applies each step with safe APIs (rustix/landlock) in its own process and then `exec`s the target. | Avoids `unsafe` `CommandExt::pre_exec`, so `unsafe_code = "forbid"` holds (Eco §39) |
| Async supervision: pidfd readiness, stdio pipes, log tee/follow, timeouts, event fan-in | `harw-tool-job/src/manager.rs`, `logs.rs` | **harw-job-tokio** (`AsyncFd<pidfd>`, log streaming) | `LogFollower`/`read_log`/`tail_of_file` move here |
| macOS backend (process group + kqueue `EVFILT_PROC`, no cgroups) | — | **harw-job-darwin** | Weaker guarantees, reported via `SandboxReport` |
| bwrap execution backend: `SandboxPolicy` → `harw-sandbox::BwrapLauncher` (`plan` + `spawn`) | `harw-tool-shell` → `harw-sandbox` | **harw-job-executor-bwrap** | Depends on `harw-sandbox` (I). Keeps `/run/harw/sandbox` sockets. |
| Public facade, feature-gated backends | — | **harw-job** | Consumers (tool-job, agent-runner, runtime) depend only on this |
| Coordinator: claim/lease/heartbeat/retry/recovery across store + executor | `harw-core` durable runner + `harw-tool-job` reload logic | **harw-job-runtime** (future role, Job-Runtime-Doc §14–§15) | Crate name kept; content changes after the re-export phase |
| `meta.json` v1 read/write | `harw-tool-job/src/model.rs` | **harw-job-store** (versioned process record, reads v1, writes v2 only after a deliberate bump) | Reload of `detached`/`unknown` jobs keeps working |

### What stays in `harw-tool-job` (A)

| Stays | Why |
|---|---|
| `job.start/status/logs/stop/list/wait` tool schemas, `JobToolProvider`, `JOB_*_TOOL` consts, `job_start_command_text`, `shell_quote` | Agent tool surface |
| `JobOwner`, `JobLineage`/`FnLineage`/`NoLineage`, `Caller` ownership checks | Harwness session semantics |
| `JobNotifier`, `JobEvent`, `JobNotification`, `render_note`, `ChannelNotifier`/`RecordingNotifier` | Agent-facing notifications |
| `progress` (detection), `throttle` (notification rate) | Presentation policy for agents. `progress` could later move to I if reused. |
| `ShellJobLauncher` (permission/sudo check via `harw-tool-shell`) | Harwness authority path. It becomes an adapter that builds a `JobSpec` for `harw-job`. |

## 3. Migration order (per concern: identify → wrap → test → migrate caller → remove old path)

| Step | Concern | Identify | Wrap | Test | Migrate callers | Remove old path |
|---|---|---|---|---|---|---|
| 1 | Model/governance (W1) | modules in `harw-job-runtime` | `harw-job-core` + re-exports from `harw-job-runtime` | existing tests move with the modules; new lifecycle tests | none needed (re-export) | later: drop re-exports once callers import `harw_job_core` |
| 2 | Harwness semantics in core (W2, J1–J5) | `JobScope`, `trace`, `WorkId`, `JobKind`, `Value` | generic types exist in core; add an adapter in session-store | **golden JSON** round-trip of existing `StoredJob` records (byte-identical) | session-store, core, cli, ops, mcp-server | old fields in core |
| 3 | Store mechanics (W2) | `job_store.rs` internals | `harw-job-store` behind the unchanged `harw_session_store::JobStore` API | the existing `job_store.rs` test suite runs unchanged against the adapter. Layout fixture test (`records/locks/approvals`). | none (same API) | private impl in session-store |
| 4 | Process identity + kill | `procfs.rs`, `manager.rs` kill path | `harw-job-linux` (`pidfd` + `procfs` crate) behind the existing `is_same_process_alive`/`signal_group` fns | PID-reuse test, `meta.json` v1 reload fixture, setsid-escape test (cgroup) | `harw-tool-job` manager | hand-rolled `/proc/<pid>/stat` parser |
| 5 | Child setup | none today | `harw-job-exec` trampoline + `harw-job-linux` cgroup/rlimit/landlock | `SandboxReport` per dimension; no `unsafe`; the trampoline execs the exact argv | `harw-tool-job` launcher, `harw-job-executor-bwrap` | — |
| 6 | Async supervision + logs | `manager.rs` stdio/log tee, `logs.rs` | `harw-job-tokio` | log follow/tail/query tests move with the code | `harw-tool-job`, `harw-agent-runner` (`start_piped`) | duplicate code in tool-job |
| 7 | bwrap backend | `harw-tool-shell::prepare_background_launch` → `BwrapLauncher` | `harw-job-executor-bwrap` | existing sandbox plan tests | `ShellJobLauncher` | direct bwrap spawn in tool-shell for jobs |
| 8 | Facade + coordinator | durable runner, tool-job reload | `harw-job` facade, `harw-job-runtime` coordinator | heartbeat/lease-loss tests from `durable_job_runner.rs` | harw-core, harw-agent-runner (fixes inversion R3), harw-runtime `job_wiring` | `DurableJobRunner` internals in core (a thin adapter stays) |
| 9 | Darwin | — | `harw-job-darwin` | macOS CI job | facade feature | — |

Rules: one concern per PR (Eco §45). The on-disk formats (`StoredJob` JSON, `jobs/` layout,
`meta.json` v1) do not change during steps 1–8. Each step leaves the old public API working
until its "remove" column is done.
