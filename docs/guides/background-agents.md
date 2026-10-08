# Background orchestrators: control, messaging, handoff

This guide describes how delegated agents run in the background, how parents and children talk to each other, what remains
when the budget runs out or a run is cancelled, and which limits apply.
The code is authoritative; the sources are listed at the end.

## 1. Overview

```text
User ──► UIA (root, TUI)
               │ transfer_to_<role> {task, context?, continue_from?}
               ▼
        Root orchestrator  ◄── agent.status / agent.result / agent.message / agent.cancel
               │ delegate_wave / transfer_to_*          ▲
               ▼                                        │ parent.message {info|question}
        Sub-orchestrators, workers ──────────────────────┘
```

Delegation is **background-only**: there is no synchronous mode and no
`background` / `wait` parameter. A start is a one-time return with ids; the
result and intermediate states are delivered to the caller automatically
(notifications / auto-turn). A start that is not admissible fails
immediately with a detailed error. The caller never blocks, so the user can
keep chatting while children run. A legacy `background:false` / `wait:true`
argument is answered with a tool error explaining this.

## 2. Orchestrators in the background

- When the UIA in the TUI hands off to any agent (orchestrator or worker,
  `transfer_to_<role>`), it runs in the background. The tool returns
  immediately with `{child_id, role, status, hint}`, the UIA's turn ends, and
  the user can keep working.
- Without a background executor (no launcher in the TUI, no agent job
  submitter in `harw-core`) the call fails with an error; there is no
  foreground fallback. `TurnOutcome::AwaitingChild` is no longer produced by
  delegation tools; it remains only for resuming persisted sessions.
- Milestones arrive through `parent.message`, the final result as a
  completion notice; both start an auto-turn when the UIA is idle.
- Once the orchestrator finishes, a UIA turn starts automatically with the
  result as soon as the UIA is idle. If a turn is already running, the
  result arrives as context in the next one.
- Approval, sudo and host-mode prompts from a background agent appear even
  while the TUI is idle.
- `/new` cancels any running background agents. On exit, the TUI asks once
  if any are still running. The status line names running background
  agents.

In the TUI:

| Command | Effect |
|---|---|
| `/agent` | agent tree with live values (root "UIA · \<name\>") |
| `/agent bg` | list background agents with progress |
| `/agent cancel <id>` | cancel your own background agent |
| `/agent stream <orchestrators\|all\|none>` | live block of children in the transcript (default from `[tui] child_stream`) |

The live stream shows a child's tool calls, reasoning and text as an
indented block under its agent line. `/agents` no longer exists.

## 3. Querying and controlling (parent-side tools)

| Tool | Arguments | Approval | Purpose |
|---|---|---|---|
| `agent.status` | `{child_id?}` | none (read-only) | running and recently finished background runs of the current session: tool calls, tokens, runtime, last step, journal excerpt, changed files |
| `agent.result` | `{child_id, offset?, max_bytes?, part?}` | none (read-only) | unabridged response text of one's own finished child, paginated (at most 48 KiB per page); `part: "journal"` returns the activity journal, even for running or cancelled children |
| `agent.message` | `{child_id, text}` | none | message to a running child of the current session |
| `agent.cancel` | `{child_id}` | **always** in `ask`/`auto` (`ALWAYS_ASK_TOOLS`); none under `full` | cancel a background run of the current session |

All four only know about children of the current session. The calling
session comes from the execution context, never from model arguments;
foreign and unknown IDs get the same message.

A child's response to its parent is truncated to 32 KiB (half from the
start, half from the end). The truncation marker names `agent.result` with
the `child_id`.

## 4. Messages between parent and child

```text
agent.message  { child_id, text }              // parent → own, running child
parent.message { text, kind?: info|question }  // child → direct parent
```

- The child reads an `agent.message` at its next turn boundary as
  "[message from <role>] …". This is how the user, via the UIA, gives
  course corrections without cancelling the run.
- `parent.message {kind: "info"}` reports an intermediate status, at most
  once every 30 s.
- `parent.message {kind: "question"}` waits up to 10 min for an answer.
  The parent's next `agent.message` answers it. Without an answer, the
  child gets "no answer — proceed with a reasoned assumption".
- Messages are plain text, at most 4 KiB, and grant no privileges.
  Mailboxes are bounded.

## 5. Budget end, cancellation, continuation

**Handoff at budget end.** The spawner holds back a reserve from a
child's token budget: 5%, at least 8000 tokens, at most 25%. The child's
turn ends already at `limit − reserve`. With the reserve, the same model
writes exactly one structured handoff (at most 3072 tokens, 60 s time
limit) with the sections: task, done, findings with evidence, open
points, recommended next steps, and state at cancellation. If that fails,
the last response goes back as a partial result, as before.

**Final report on cancellation and error.** If a child does not end
normally (time budget, lease expiry, cancellation, provider or tool
error), the parent gets a final report instead of a bare error. The first
line is machine-readable:

```text
[child_end status=cancelled handoff=…]
```

followed by the reason, the handoff (if possible), and a short summary of
the journal. The full journal is fetched with
`agent.result {child_id, part: "journal"}`.

**Continuation.** A child that ended at its budget can be continued with
the same role:

```json
{ "targets": [ { "role": "explorer", "continue_from": "child-7", "task": "module C only" } ] }
```

`continue_from` is available in `delegate_wave` targets and with
`transfer_to_<role>`. The new child gets the handoff as its first context
(`task` is then optional) and a fresh budget. At most three continuations
per original child. The continuation goes through normal admission, and
its sandbox may not be wider than its predecessor's.

## 6. Requesting host mode

If a command fails against the sandbox, a shell-capable agent can request
host mode:

```json
{ "command": "…", "request_host": { "reason": "build needs access to /dev/kvm" } }
```

- The TUI shows the host-mode prompt with the requester (role, child ID,
  path in the agent tree) and the reason. The choices are the same as for
  `/sandbox-lease`: once, or for the session.
- Once approved, the command runs through the existing host path. The
  "for the session" choice applies process-wide: every shell-capable
  agent in that session then runs on the host without a new prompt.
  The phase has no expiry and the model cannot end it: only you end it,
  with `Ctrl+H` or by typing `/sandbox-lease revoke`. A session switch
  (`/new`, `/resume`) builds a fresh runtime and therefore also starts
  without a phase ("host mode ended (new session)."), as does quitting
  harw.
- If a normal sandboxed run recognizably fails because of the sandbox,
  the model gets the fields `sandbox_denial` and `host_mode_hint`. This is
  only a hint, never an automatic retry on the host.
- Outside the TUI, every such request ends with a hard error.

Root commands are separate, and they work: only `uia-shell-worker` and
`host-process-worker` may use `host.sudo_exec {argv, reason}`, so the UIA
delegates a root command to `uia-shell-worker` with the exact command and
a reason, and other agents hand the exact `argv` back to their parent. You
see the exact argv in its own TUI window, approve it and type your
password there if sudo asks for one (passwordless sudo only asks for
approval). The password never reaches the model; agents never ask for it
in chat or use `sudo -S`. Only without a TUI (`serve`, Telegram, one-shot)
is there no such window — then harw names the command for you to run
yourself. Details: [`mediated-process-execution.md`](../design/mediated-process-execution.md#root-commands-hostsudo_exec).

## 7. Approvals for child agents

If a child needs an approval, it appears in the normal approval dialog
with "requested by …". The child waits up to 10 min. Each approval only
covers that one call: no mode change and no rule is added for the child. A
question from the root takes priority; the child's question reappears
afterward.

Children follow the root's approval mode, at most `auto` (see
[`tui-roles-models-modes.md`](../design/tui-roles-models-modes.md) §3.1).

## 8. Limits

| Limit | Value | Configurable |
|---|---|---|
| concurrent orchestrators of the UIA | 1 (1-4) | `[agents] max_root_orchestrators` |
| concurrent sub-orchestrators per tree | 2 (1-6) | `[agents] max_sub_orchestrators` |
| sub-orchestrator nesting | 2 (1-3) | `[agents] max_sub_orchestrator_depth` |
| general child depth | 4 (1-6) | `[agents] max_spawn_depth` |
| wall-clock limit of an orchestrator | 3600 s | `[spawn.budget] max_wall_secs` of the definition |
| continuations per child | 3 | fixed |
| message length | 4 KiB | fixed |
| `parent.message` info / question | 1 per 30 s / 10 min wait | fixed |
| child approval | 10 min wait, one-time | fixed |
| `shell.exec` time limit | 30 s, build/test 600 s, `timeout_secs` up to 900 s (30-3600) | `[shell] max_timeout_secs` |

- Invalid values in `[agents]` and `[shell]` are clamped. An untrusted
  project may only lower them.
- A rejection at an orchestration limit starts with "Orchestration
  limit:" and reaches the model as a tool error; the turn continues.
- **Lease instead of a time limit:** a child's lease (15 min) is a
  liveness signal. While the child runs, a heartbeat extends it and the
  leases of all ancestors, even during a long build. A stuck child still
  ends at its wall-clock limit.

## 9. In practice

- **Long task:** the UIA hands off to the orchestrator, keeps working, and
  checks in with `agent.status` as needed. The result arrives on its own.
- **Course correction:** "Tell the orchestrator to skip module B" → the
  UIA sends `agent.message`.
- **Child asks a question:** it appears at the UIA; its answer goes back
  via `agent.message`.
- **Budget exhausted:** read the handoff, continue with `continue_from` if
  needed.
- **Cancelled:** read `[child_end …]`, fetch the journal with
  `agent.result {part: "journal"}`, restart in a targeted way.
- **Sandbox blocked:** heed `sandbox_denial` and request host mode with a
  reason, instead of reworking the command.

## Sources

`harw-core/src/background_children.rs`, `child_comms.rs`,
`child_handoff.rs`, `child_lease_heartbeat.rs`, `turn_loop.rs`
(`handoff_tool_spec`); `harw-core-bridge/src/agent_status.rs`,
`agent_background.rs`, `agent_result.rs`, `agent_messaging.rs`,
`delegate_wave.rs`; `harw-tool-shell/src/exec/escalation.rs`;
`harw-config/src/agent_limits.rs`, `shell_limits.rs`;
`harw-tui/src/app/background_agents.rs`, `child_approvals.rs`,
`turn_safety.rs`.
