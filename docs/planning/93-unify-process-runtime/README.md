---
id: UNIFY-PROCESS-RUNTIME
title: One owner for Linux workload processes — everything that starts a process submits a job
status: proposed (step 1 built)
date: 2026-10-04
tags: [job-runtime, shell, tui, latex, jobs, architecture-rule]
related:
  - ../20-job-runtime/
  - ../67-containers/README.md
  - ../65-cloud-sessions/CONSOLIDATION.md
---

> **Rule (target):** Harw application code does not spawn Linux workload
> processes. It submits jobs. `LinuxExecutor` is the only owner of spawn,
> pidfd, cgroup, sandbox, kill, deadline and recovery.

## 1. What was checked (and what was found)

The proposal was checked against `consolidate/main` (which contains `dev`).

| Claim | Verdict | Evidence |
|---|---|---|
| `harw-job` is the public facade over `Coordinator` → `Executor` → `LinuxExecutor` (pidfd, process group, cgroup v2, Landlock/bwrap, deadline, recovery, lease/fencing) | **true** | `harw-job/src/lib.rs`, `harw-job-runtime/src/coordinator/{runner,linux}.rs` |
| `verify_exec.rs` already does `CommandRequest → JobSpec → submit → wait → JobResult` | **true** | `harw-plan-bridge/src/verify_exec.rs`: `CoordinatorVerifyRunner` |
| `shell.exec`, `!`/`!!`, `latex.*`, `host.sudo_exec` own their process lifecycle | **true** | `harw-tool-shell/src/{exec,exec/background,exec/operator,latex,sudo}.rs`, `harw-tui/src/command_exec.rs` (≈ 11 000 lines) |
| `job.start` is a second process runtime beside the coordinator | **true** | `harw-tool-job` (`manager.rs` 1 971 lines + `launcher.rs`) |
| `JobHandle` has no public output/event stream | **true** | only `id`, `attempt_id`, `cancel`, `wait` (before step 1) |
| `ResourceRequest` is narrower than `ShellLimits` | **true, and worse** | `LinuxExecutor` applied **no** rlimits at all on any backend; the Landlock trampoline could (`ExecPlanV1::with_rlimits`) but was never given any |
| `new_record` persists the full spec (env) | **true** | `StoredJob.input = to_value(envelope)`: env values end up in the durable store |
| `process.list`/`process.kill` stay separate | agreed | `harw-killer`, foreign PIDs are another authority domain |

### Gaps the proposal did not name (found while reading the code)

1. **Controlling terminal / session.** The host operator path (`!`) runs under
   `setsid --wait` so a command cannot touch the TUI's terminal. `LinuxJobGroup`
   only does `process_group(0)`. With `setsid` and without `process_group(0)` the
   program is exec'd in place and becomes session **and** group leader with
   `pgid == pid`, so group kill still works (that is why the shell avoids
   `process_group(0)` there). The executor needs a "new session" mode doing exactly that.
2. **Extra read-only binds.** `latex.*` binds TeX/font roots (`TEX_READ_ONLY_ROOTS`) and
   retargets `--chdir`; `shell.exec` has a host-path binding. `JobSpec` has no way to
   ask for additional read-only paths.
3. **Two bwrap planners.** `harw-sandbox::BwrapLauncher` (shell/latex) and
   `harw-job-executor-bwrap` (executor) are separate paths with different knobs
   (tmpfs size, `HostPathBinding`, `--unshare-all`). They must be reconciled before
   `shell.exec`'s sandbox path can move.
4. **Environment.** The host operator path inherits the whole host environment;
   `JobSpec.env` is an explicit allowlist. Both are fine, **but** the full environment
   must never be written to the store: see persistence classes below.
5. **Retry/lease semantics for interactive commands.** A `!` command must not be
   retried and must not be re-run after a restart; `Persistence` (below) plus
   `max_attempts = 1` give that, but the adapter has to say so explicitly.
6. **Old `JobManager` features** (progress parsing, log files, throttle, ownership,
   semaphore/`timed_out` from G6) have no counterpart in the coordinator; they stay as a
   *presentation/ownership layer* on top, they do not move into the executor.

## 2. Step 1 — built (this change)

All additive, in `harw-job-core` / `harw-job-runtime` / `harw-job`:

- **Live frames.** `JobHandle::subscribe()` and `Coordinator::submit_with(.., stream: true)`
  give a `JobFrames` stream: `AttemptStarted{pid, sandbox}`, `Stdout`, `Stderr`, `Note`,
  `AttemptEnded`. Lossy by design (bounded broadcast; a slow subscriber gets
  `Lagged(n)`, never slows the job), no cost without a subscriber, no loss at the start for
  a subscription taken at submit, the stream ends when the job is finished.
- **Per-process rlimits in `JobSpec`.** `ResourceRequest` gains `address_space_max`
  (`RLIMIT_AS`), `cpu_time_max`, `file_size_max`, `open_files_max` (validated, backward
  compatible). `LinuxExecutor` applies them on every backend: through the trampoline plan
  (`with_rlimits`, applied before the sandbox, fails closed) or through a pinned `prlimit`
  in front of the program / of `bwrap` (`exec`s, so the PID stays the primary's). Without
  `prlimit` the limits are **not** applied and the report says `resource_limits:
  NotEnforced` (a `Required` job then fails before it runs).
- **Persistence classes.** `Persistence::{Durable, MetadataOnly, Ephemeral}` via
  `SubmitOptions`. `Durable` (default, unchanged) stores the full spec. `MetadataOnly`
  stores program, argument **count**, env **names**. `Ephemeral` stores nothing about the
  command. A redacted record is deliberately **not** a `JobSpecEnvelope`, so recovery finds
  no spec and can never re-run a command from it (it can still reattach to a verified
  process or finalize the record). The job itself always runs from the in-memory spec.

## 3. Next steps (revised order)

1. **Executor gaps 1–3** (new-session mode, extra read-only binds, one bwrap planner).
2. **Command adapter** in the composition root: `CommandRequest{argv or shell line, cwd,
   env policy, timeout, limits, sandbox, foreground/background, persistence}` →
   `JobSpec` → `submit_with` → `JobFrames`/`JobResult`. `shell.exec` = submit + await;
   `job.start` = submit + return the id.
3. Move `!`/`!!` (host, `Ephemeral`, one attempt), then `latex.*` (exact argv, sandbox),
   then `shell.exec` (sandbox path first, host path after).
4. `job.*` onto the runtime; `JobManager` shrinks to presentation/ownership.
5. Delete the old spawn/wait/kill code. `host.sudo_exec` last, after an ephemeral
   stdin/secret side channel exists (the password must never be part of a `JobSpec`).

## 4. Enforcing the rule (ratchet)

`cargo xtask gates spawn` lists every non-test source file in an application crate that
starts a process (`Command::new`, `TokioCommand`, `.spawn()`), with a reason. A new file
that spawns is a violation; an allowlisted file that no longer spawns is a violation too
(the list can only shrink). Infrastructure that is not a workload (MCP stdio servers,
the browser driver, the egress relay, the engine clients of the container tools,
`harw-killer`, the executors themselves) is allowlisted with its reason; the shell/job/TUI
surfaces carry `migrate: PL-93 step N`.

## 5. Status (branch `arch/unify-process-runtime`)

| Step | State |
|---|---|
| 1 frames / rlimits / persistence | done, tested (`harw-job-core`, `harw-job-runtime`) |
| 2 command adapter | done: `harw-command` (`CommandPort`, `JobCommandPort`, `run` and `start`); installed once by the composition root (`SessionJobs::open`, `run_chat`) — nothing installed means the surfaces fail closed |
| 3 `shell.exec` | done: host and bwrap launches run through the port; tool keeps planning (bwrap plan, prlimit wrapper, binds) |
| 4 TUI `!`/`!!` | done: `OperatorCommand::run` builds a `CommandRequest`, ephemeral, one attempt |
| 5 `latex.*` | done: same bwrap planning, spawn/deadline/kill by the runtime |
| 6 `job.start` | done for non-piped jobs: `JobManager::start` (now `async`) hands the prepared command to the runtime with **file output** (`SubmitOptions::output_files`), keeps ownership, logs, progress, stop/detach |
| 7 delete old code | done for the shell/latex/operator paths (`spawn_and_collect`, `BoundedCapture::drain`, `configure_stdio`, `host_shell_argv`) |
| 8 `host.sudo_exec` | open (needs the stdin/secret side channel) |

Executor gaps closed on the way: **new-session mode** (`LinuxExecutorOptions::new_session`:
`setsid` in place, process group = PID, tree kill intact, no controlling terminal; a missing
program is still a failed spawn), `require_rlimits` (no `prlimit` = no start), output to files.

### Known differences to the old paths
- `RLIMIT_NPROC` is not mapped (a user-wide ceiling, no counterpart in `ResourceRequest`).
- On non-Linux hosts no port is installed; `!`, `shell.exec`, `latex.*` and `job.start` report
  "no job runtime" instead of spawning (macOS needs `DarwinExecutor` behind the same port).
- Jobs stopped by `job.stop` are still signalled by `JobManager` through the identity-checked
  `OwnLeader` (adoption after a restart); the runtime only owns *starting* and supervising.

### Still on the ratchet (`xtask/spawn-policy.toml`, entries marked `migrate`)
`harw-tool-job` `start_piped` (agent child stdio), `harw-agent-runner` job child backend,
`harw-tool-shell` `sudo` and the background `PreparedJob` command builders, `harw-ops`
(`/agent`, `/diff`), `harw-tui` plan mode/clipboard helpers. Each needs either the stdin
side channel (piped children, sudo) or a decision that the call is tooling, not a workload.
