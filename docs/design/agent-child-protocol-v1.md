# Agent Child Protocol v1 — `harwness.agent-child/v1`

> Status: implemented (#22, waves 3–6) · Last reviewed: 2026-09-25

**Decision record:** [ADR 0001](../adr/0001-agent-compiler.md) §2.5–§2.6,
§3.1 ("Wave 3 consequences").
**Binds to:** [`agent-ir-v1.md`](agent-ir-v1.md) §6.1 (`Binary.child_execution`),
[`agent-artifact-v1.md`](agent-artifact-v1.md) §10 (the bundle a child's own
process reads).
**Implemented by:** `harw-agent-runner` — the frame types
(`src/child_protocol.rs`), the child-process side
(`src/child.rs::run_child`), and the parent-process side
(`src/job_child_backend.rs::JobChildBackend`).

This document is normative for the wire format; where the code and this
document disagree, the code (`harw-agent-runner/src/child_protocol.rs`)
wins and this document is a bug.

---

## 1. Why this protocol exists

A compiled agent binary carries its whole delegation closure as one bundle
([`agent-artifact-v1.md`](agent-artifact-v1.md) §10): an orchestrator's
artifact holds not only its own `AgentIr` but every agent it can start,
transitively. `[binary] child_execution` decides how the running binary
starts one of those children:

- `"in-process"` — the legacy in-process spawner, the same way `harw`
  itself always runs its children regardless of this setting.
- `"job"` (the default for a compiled binary) — each child runs as its
  **own OS process**: a second `harw-agent-runner` invocation, over the
  same executable and the same embedded artifact, started with
  `--child <agent-id> --child-protocol stdio`. Parent and child are always
  the same binary — this is a private protocol between one
  `harw-agent-runner` process and another it started, never a public API a
  different program is expected to speak.

The two names are different things. `stdio` is the *transport label* on
the command line (`JobChildBackend`'s `CurrentExeSpawner` always passes
exactly that; the runner's argument parser only requires `--child` and
`--child-protocol` together and does not interpret the value).
`harwness.agent-child/v1` is the *protocol version*: it never appears on
the command line, only in the child's `Hello` frame, where the parent
checks it (§3).

This document describes that second case: the JSON-lines protocol the two
processes speak over the child's own stdio.

## 2. Wire format

- One JSON object per line, `\n`-terminated, UTF-8
  (`child_protocol::encode_line`/`decode_line`).
- **Parent → child** frames ([`ParentToChild`](#4-parent-to-child-frames))
  travel on the child's **stdin**.
- **Child → parent** frames ([`ChildToParent`](#5-child-to-parent-frames))
  travel on the child's **stdout**.
- The child's **stderr** is logs only — free text, never a protocol line,
  and never parsed as one.
- Every frame is a JSON object tagged by a `"type"` field naming the
  variant in `snake_case` (`#[serde(tag = "type", rename_all =
  "snake_case")]`), so a frame from a future protocol version that adds an
  unrecognized variant fails to parse loudly instead of silently matching
  the wrong arm.
- A line whose `[`/`{` nesting is deeper than
  `MAX_JSON_NESTING_DEPTH = 64` is rejected as malformed before it reaches
  `serde_json` (`child_protocol::json_nesting_too_deep`, checked inside
  `decode_line`), so a tiny but deeply nested line cannot exhaust the
  parser's stack. The runner's `mcp` and `http` interfaces use the same
  bound.
- Every line is length-bounded: a line longer than `MAX_FRAME_BYTES`
  (1 MiB, excluding the `\n`) is refused by `child_protocol::FrameReader`
  before it is buffered past that bound, never truncated and parsed; a
  line that is not valid UTF-8 is refused the same way
  (`FrameReadError::TooLarge` / `InvalidUtf8`). Either is a protocol
  error, because the stream is out of sync afterwards:
  - **Child** (reading stdin): it sends one `Error` frame naming the
    problem and exits with a failure code, without a `Result`. Before the
    `Task` it has no session to stop; while a turn runs it cancels the
    session first (§4.4). A plain read error on stdin ends it without an
    `Error` frame (a failure exit before the `Task`, a cancelled turn
    after it).
  - **Parent** (reading the child's stdout): it fails the run
    (`ChildRunStatus::Failed` with the reader's message) and stops the
    child — `JobManager::stop` on the job path, a process-group kill in
    the test seam (§7). On the job path, stdout lines arrive already split
    by the job's stdout tee and are checked against `MAX_FRAME_BYTES` on
    arrival. A plain read error on the direct path counts as the stream
    ending (a crash, §7 step 6).

## 3. Handshake and versioning

The child's very first frame, always, is [`Hello`](#51-hello) — before
anything else, even before it has read a `Task`. The parent checks its
`protocol` field against the version it implements
(`child_protocol::verify_protocol`): today that is a **strict equality**
check against `PROTOCOL_VERSION = "harwness.agent-child/v1"`, not a
compatibility range, because there is exactly one supported version. A
mismatch fails the run closed with `ChildRunStatus::Failed`, naming both
versions; it never desyncs the stream by guessing at a frame shape it does
not recognize.

`JobChildBackend::run` then always writes, in this order: a `Rights`
frame (§4.7), a `Budget` frame if the run has a budget (§4.5), a `Mode`
frame (§4.6), and finally the `Task` frame that starts the child's turn
(§4.1).

On the child side, `Rights`, `Budget` and `Mode` are accepted only before
the first `Task`; the child collects them and applies them when it builds
its session. **`Rights` is required:** a `Task` that arrives before any
`Rights` frame makes the child send `Error` (`"Rights must precede
Task"`) and exit with a failure code — a child never starts a turn on its
bare manifest rights. A line that fails to decode before the `Task` makes
the child send `Error` and keep reading; the parent treats that `Error`
as the end of the run (§7). `Message` frames before the `Task` are
ignored.

## 4. Parent-to-child frames

Sent on the child's **stdin**.

### 4.1 `Task`

Starts (or resumes) the child's one turn.

```json
{"type": "task", "task": "review the PR", "context": {"pr": 42}, "continue_from": null}
```

| Field | Type | Meaning |
|---|---|---|
| `task` | string | The child's task text (its user turn). |
| `context` | JSON, optional | Structured context handed alongside `task`. |
| `continue_from` | string, optional | The `continuation` token of a prior budget-ended `Result` (§5.4) of this same child, if this resumes one. |

The child concatenates `task` and `context` (when present) into one prompt
text before sending it to its own session
(`format!("{task}\n\n{context}")` — `child::run_child_async`); `context`
is not yet passed to the model as a separate structured input.

**Continuation.** Without `continue_from` the child opens a fresh session
(`Harwness::session`). With it, the child parses the token as an SDK
session id and resumes that stored session (`Harwness::resume`) instead;
this works because the resumed process uses the same `HARW_HOME` as the
run that produced the token, so the session store is the same. The
resumed session runs under the rights, budget and mode of *this* run's
frames, not the earlier one's. A token that does not parse or names no
resumable session makes the child send `Error` (`"failed to resume the
child session: …"`) and exit with a failure code; no `Result` follows.

On the parent side the token travels end to end: `JobChildBackend` hands
a budget-ended `Result`'s `continuation` back as
`ChildRunOutcome::continuation`, `ManagedAgentSpawner` stores it as the
child's backend resume token, and the next `run_child_via_backend` for
that child passes it as `ChildRunSpec::continue_from`, which lands in this
field.

### 4.2 `Message`

A follow-up message while the child's turn is already running
(parent-to-child chat, distinct from the initial `Task`).

```json
{"type": "message", "text": "hurry up"}
```

Defined on the wire, but not implemented in either direction:
`JobChildBackend` never sends it, the child ignores it before the `Task`,
and while a turn runs the child treats it like any other unsupported
control frame (§4.4).

### 4.3 `Answer`

The parent's reply to a child's [`Question`](#52-question) or
[`ApprovalRequest`](#53-approvalrequest), matched by `id`.

```json
{"type": "answer", "question_id": "appr-1", "text": "approve"}
```

For an approval request, `text == "approve"` approves; any other text
denies with that text as the reason given back to the model. A `Question`
has no fixed convention for `text` — it is whatever free-text answer the
question asked for.

### 4.4 `Cancel`

Cancels the child's run. Terminal: the child does not resume after this.

```json
{"type": "cancel"}
```

Sent before any `Task` arrives, it ends the child cleanly with no `Result`
frame at all (there is nothing to report a result for yet). Sent while a
turn is running, the child's session is cancelled cooperatively and a
`Result` frame with `status: "cancelled"` follows once it settles.

While a turn runs, the child reads only `Answer` and `Cancel`. Any other
frame, or a line that fails to decode, makes it send `Error`
(`"unsupported or malformed mid-turn control frame; child cancelled"`)
and cancel its session; a closed stdin also cancels it. `JobChildBackend`
does not send `Cancel` itself (§7, step 5).

### 4.5 `Budget`

Sets the child's budget before its turn.

```json
{"type": "budget", "max_tokens": 1000, "max_tool_calls": null, "max_wall_time_ms": 60000}
```

All three fields are optional; an absent field leaves that budget
dimension as it was. `JobChildBackend` sends this once, right after
`Rights` and before `Mode`/`Task`, when the spawning run has a budget at
all (`run_child_via_backend` passes the child's remaining budget).

A `Budget` frame only ever **tightens**: the child folds each dimension
in with `min_limit` (the smaller of two set limits; a set limit beats an
unset one), and the result is intersected again with the child's own
manifest budget when it is applied (`EmbeddedAgent::with_rights`). The
wire carries wall time in milliseconds, the IR in seconds:
`JobChildBackend` sends `max_wall_secs × 1000`, the child converts back
with integer division (`max_wall_time_ms / 1000`, rounding down).
`max_tool_calls` above `u32::MAX` saturates to `u32::MAX`. Sent after the
`Task`, the frame is rejected (§4.4).

### 4.6 `Mode`

Switches the child between plan and live mode.

```json
{"type": "mode", "mode": "live"}
```

`mode` is `"plan"` (the child plans but does not act) or `"live"` (the
child acts). The child maps `plan` to `harwness_sdk::Mode::Plan` and
`live` to `harwness_sdk::Mode::Work`; without a `Mode` frame it stays in
`Plan`. `JobChildBackend` always sends one, `"live"` exactly when
`ChildRunSpec::live_mode` is set (`run_child_via_backend` sets it unless
the child's lineage is in plan mode). Sent after the `Task`, the frame is
rejected (§4.4).

### 4.7 `Rights`

Narrows the child's rights. Required before the first `Task` (§3); sent
after it, the frame is rejected (§4.4).

```json
{
  "type": "rights",
  "rights": {
    "tools": ["fs.read"],
    "network_hosts": [],
    "network_open": false,
    "write": false,
    "shell": false,
    "host": false,
    "full_access": false
  }
}
```

A backend only ever **narrows**: widening a child past its own ceiling is
never valid. Several `Rights` frames before the `Task` narrow each other
(`child::narrow_rights`: tool set and host set intersect, every boolean —
`full_access` included — is AND'd); the collected value is then
intersected with the child's own ceiling when the session is built. §6
describes that step and how the result reaches the child's session.

`ChildRights` (the shape carried in this frame and in `Result`'s
counterparts) mirrors `harw_runtime::embedded::EffectiveRights`'s shape
without naming that type directly, so the protocol can serialize cleanly
and stay stable independent of that crate's internals:

| Field | Type | Meaning |
|---|---|---|
| `tools` | set of strings | Exact tool names the child may call. |
| `network_hosts` | set of strings | Hosts the child may reach over the network. |
| `network_open` | bool | Unrestricted network access beyond `network_hosts`. |
| `write` | bool | Write access to the workspace. |
| `shell` | bool | Shell execution. |
| `host` | bool | Host-level (unsandboxed) execution. |
| `full_access` | bool | Automatic approval: when effective, the child's session approves tool calls itself instead of relaying them (§6). |

## 5. Child-to-parent frames

Sent on the child's **stdout**.

### 5.1 `Hello`

Always the child's first frame (§3).

```json
{"type": "hello", "agent_id": "evidence-critic", "protocol": "harwness.agent-child/v1", "digest": "blake3:deadbeef"}
```

| Field | Type | Meaning |
|---|---|---|
| `agent_id` | string | The child's own agent id inside the bundle. |
| `protocol` | string | The protocol the child implements; checked against `PROTOCOL_VERSION` (§3). |
| `digest` | string | The bundle's artifact digest, so the parent can confirm it started the binary it thinks it did. |

### 5.2 `Question`

The child has a free-text question for the parent (for example an
interactive-input tool); resolved by a matching `Answer` (§4.3).

```json
{"type": "question", "id": "q-1", "text": "which file?"}
```

`id` is a fresh id, unique for this child's lifetime. `JobChildBackend`
relays it to `ChildIo::on_question` and answers with the reply; the child
process itself does not send `Question` frames yet.

### 5.3 `ApprovalRequest`

The child asks approval for a held-back tool call; resolved by a matching
`Answer` (`"approve"` or a deny reason).

```json
{"type": "approval_request", "id": "appr-1", "tool": "fs.write", "args_summary": "write 12 lines to src/lib.rs"}
```

`args_summary` is a short, human-readable summary of the call's
arguments — never the raw arguments verbatim (`child::summarize_arguments`
truncates at 200 characters), both to keep the frame small and to avoid
echoing anything the parent should not print unredacted.

Every `ApprovalHandler` decision inside the child's own session becomes
one of these frames (`child::RelayApprovals`, ids `appr-1`, `appr-2`, …);
there is no local approval policy inside a child process, only a relay to
the parent. The parent's `ChildIo` answers `"approve"` or
`"deny: <reason>"`.

### 5.4 `Result`

The child's run ended. Exactly one `Result` frame per run, unless the run
ends without one at all (a crash the parent detects by the child's stdout
closing — see §7).

```json
{
  "type": "result",
  "status": "completed",
  "text": "done",
  "usage": {
    "input_tokens": 10,
    "output_tokens": 5,
    "cached_input_tokens": 0,
    "tool_calls": 2,
    "wall_time_ms": 1500
  }
}
```

`status` is one of:

| Status | Meaning |
|---|---|
| `completed` | Regular completion. |
| `cancelled` | Cancelled, via a `Cancel` frame or any cancel reason other than the budget. |
| `failed` | Any other non-successful end (provider/turn error, refusal, truncation, …). |
| `budget_exhausted` | The run's budget ended it; `text` is the last (or handed-off) answer and `continuation` resumes it. |

`child::translate_turn_report` maps `harwness_sdk::TurnStatus::Completed`
to `completed`, `Cancelled { reason: "budget" }` to `budget_exhausted`,
every other `Cancelled` to `cancelled`, and `Truncated`/`Refused`/
`Failed` all to `failed`. A session-level error (the turn could not run
at all) also becomes `failed`, with the error text as `text`.

`continuation` (string, optional) is the opaque token a later `Task`
hands back in `continue_from` (§4.1) — today the child's SDK session id.
The child sets it only with `budget_exhausted` and omits it otherwise:

```json
{"type": "result", "status": "budget_exhausted", "text": "partial", "continuation": "<session id>"}
```

The field is additive: a `Result` frame without it (from an older peer)
still decodes, as `None`. The parent keeps a token only on a
`budget_exhausted` result; one sent with any other status is dropped
rather than trusted.

On the parent side (`job_child_backend::from_wire_status`), `completed`
and `budget_exhausted` map to the `ChildRunStatus` of the same name,
`cancelled` to `Cancelled { reason: "child" }`, and `failed` to
`Failed { reason: "child reported a failed turn" }`.

`text` is the final (or last, for a budget end) assistant text, if any.
`usage` is always present (defaulting to all zeros; the child reports
`wall_time_ms` as `0` today); its `fresh_tokens()`
is input plus output tokens, excluding cached reads, matching
`harw_types::TokenUsage::fresh_tokens`'s accounting unit.

### 5.5 `Usage`

A standalone usage update, sent independently of `Result` (for example
after each model round) so the parent can show live consumption without
waiting for the run to end.

```json
{"type": "usage", "usage": {"input_tokens": 3, "output_tokens": 1, "cached_input_tokens": 0, "tool_calls": 0, "wall_time_ms": 0}}
```

Defined on the wire; the child process does not send it yet (live usage
reaches the parent inside `Event` frames of kind `usage`), and
`JobChildBackend` reads and drops it.

### 5.6 `Event`

A translated live event of the child's session.

```json
{"type": "event", "sdk_event": {"kind": "message", "text": "hi", "source": {"session_id": "…", "parent": null, "role": "assistant"}, "final_answer": true}}
```

`sdk_event` is `child::translate_event`'s JSON rendering of one
`harwness_sdk::SdkEvent` — a manual, explicit mapping (that SDK type is
`#[non_exhaustive]` and carries no `Serialize` impl on purpose), tagged by
its own `"kind"` field (`turn_started`, `text_delta`, `reasoning_delta`,
`message`, `tool_call`, `tool_result`, `child_spawned`, `child_completed`,
`usage`, `context`, `error`, `finished`, `lagged`). This inner shape is
not part of this protocol's own version contract: it changes whenever
`harwness_sdk::SdkEvent` does, independent of `PROTOCOL_VERSION`.

### 5.7 `Error`

The child hit an error it could not otherwise report.

```json
{"type": "error", "message": "failed to build the child session: …"}
```

The child sends it for: a line that fails to decode before the `Task`
(§3); a `Task` before any `Rights` (§3); a `continue_from` token it
cannot resume (§4.1, `"failed to resume the child session: …"`); an
unsupported or malformed frame while the turn runs (§4.4); an oversized
or non-UTF-8 line (§2); and a failure to build its `Harwness` or start
its session.

Distinct from `Result`'s `failed` status, which is a *result* of a turn
that ran: `Error` is sent when the child cannot even produce one. The
parent treats every `Error` as the end of the run:
`ChildRunStatus::Failed { reason: <message> }`, and the child's job is
stopped (§7).

## 6. Rights narrowing in the child process

The child's effective rights are its own ceiling intersected with every
`Rights` frame the parent sent:

```text
own ceiling  = child manifest narrowed by the child's runner flags
child rights = own ceiling ∩ parent's Rights frame(s)
```

- **Own ceiling.** `child::own_ceiling` converts the child's
  `AgentIr::permissions` with the same
  `harw_runtime::embedded::EffectiveRights::from_manifest` every
  top-level interface uses (§5.1 of the [agent compiler
  guide](../guides/agent-compiler.md)) and narrows it by the child
  process's own rights flags (`--deny-tool`, `--no-network`,
  `--read-only`, `--max-tokens`; `EffectiveRights::narrowed_by`). One
  exception: `full_access` is forced to `true` in the ceiling. A job
  child is never started with `--full-access`, so ANDing that flag in
  would make the parent's grant unreachable.
- **Parent's `Rights` frame(s).** Several frames before the `Task` narrow
  each other (§4.7). `child::effective_child_rights` then intersects the
  result, together with the collected `Budget` (§4.5), with the ceiling
  (`EffectiveRights::intersect`: sets intersect, booleans are AND'd, each
  budget limit takes the stricter value).

`full_access` is therefore effective only through the parent's grant. When
it is, the child builds its session with `ApprovalPolicy::FullAccess` and
approves tool calls itself; otherwise every approval is relayed to the
parent (§5.3).

**What the parent sends.** `ManagedAgentSpawner::run_child_via_backend`
(`harw-core/src/child_controller.rs`) fills `ChildRunSpec::rights` from
`backend_rights`, which computes exactly the rights the child would have
in-process:

- `tools`: the tools the child's session would see now
  (`turn_loop::collect_tools`), further intersected with the parent's
  activation that admission cut the child against.
- `write`/`shell`/`network_open`: `WriteWorkspace`/`ExecuteProcess`/
  `NetworkAccess`, each only if both the child's and the parent's sandbox
  hold it.
- `network_hosts`: the hosts of the child's network scope that the
  parent's scope allows. `PublicDns` and CIDR targets have no counterpart
  in `ChildRights` and are dropped for a job child (fail closed: the
  child loses that egress rather than gaining a wider one).
- `host`: set when a host-level tool (`host.*`) is among `tools`.
- `full_access`: `true` exactly when the root's approval mode reads
  `ApprovalMode::FullAccess`; without an approval-mode cell, never.

If the child, its sandbox or the parent cannot be found, or a lock is
poisoned, `backend_rights` fails closed to empty rights: a job child
started that way may do nothing. `JobChildBackend` passes the value on
unchanged (`to_wire_rights`); it never narrows or widens it itself.

The result is applied to the session the child actually runs:
`child::run_child_async` selects the child as the embedded root
(`EmbeddedAgent::for_agent`) and then calls `EmbeddedAgent::with_rights`
with the effective rights. `with_rights` intersects, so this step can only
narrow further. The session's tool/network/write/shell/host behavior
therefore comes from the effective value, not from the bare manifest.

## 7. The parent side: `JobChildBackend`

`harw_core::child_backend::ChildBackend` is the trait a job-driven
delegation calls (`ManagedAgentSpawner::run_child_via_backend` builds the
`ChildRunSpec`); `JobChildBackend` is this protocol's implementation of
it, over a real OS process:

1. Builds `std::env::current_exe() --child <agent-id> --child-protocol
   stdio`, plus `--offline-echo` when the parent itself runs with it
   (`JobChildBackend::new`'s `offline_echo`, set by
   `RunnerContext::builder` from the parent's own flag), stdin/stdout/
   stderr all piped, in its **own process group** (`process_group(0)` on
   Unix). In production (`JobChildBackend::new`)
   the command runs as a real job through
   `harw_tool_job::JobManager::start_piped` (job name
   `agent-child-<agent-id>`): stdout is teed into the job's stdout log
   and handed to the backend as a line stream, stderr goes into the
   job's stderr log. The test seam (`JobChildBackend::with_spawner`)
   spawns the command directly instead.
2. Reads the first line and requires it to decode as `Hello` with a
   matching `protocol` (§3). A version mismatch or any other first frame
   fails the run (`ChildRunStatus::Failed`); the stream ending before a
   first line is a crash.
3. Writes `Rights`, then `Budget` if the run has one, then `Mode`, then
   `Task` (§3).
4. Relays every subsequent frame: `Event` goes to the caller's `ChildIo`
   sink, `Question`/`ApprovalRequest` are awaited and answered with
   `Answer`, `Usage` is currently read and dropped, `Error` and `Result`
   end the loop. A `Result` becomes the `ChildRunOutcome`; its
   `continuation` is kept only for `budget_exhausted` (§5.4). A second
   `Hello`, an undecodable line, or an oversized or non-UTF-8 line (§2)
   fails the run. After `Failed`, the child's job (or, in the test seam,
   its process group) is stopped.
5. On cancellation (`spec.cancel.cancelled()`), stops the job
   (`JobManager::stop` with `SIGTERM`; the test seam kills the process
   group via `rustix::process::kill_process_group`) rather than sending
   a graceful `Cancel` frame first — exactly one place (the
   `tokio::select!` around the protocol loop) decides "is this run
   cancelled", so there is no race between a `Cancel` frame and a kill.
6. A crash (the stdout stream ends without a `Result`) is reported as
   `ChildRunStatus::Crashed` with the process's exit code (waited for up
   to 5 seconds on the job path) and its last 32 lines of stderr.

`JobChildBackend` and the `--child` side of `harw-agent-runner` are both
unit-tested against a real spawned process (a `/bin/sh` stand-in for the
backend tests, a real embedded agent bundle for the translation tests).
The wiring is automatic: `RunnerContext::builder`
(`harw-agent-runner/src/context.rs`) sets the job-backed child backend on
the `HarwnessBuilder` whenever the current root agent's
`[binary] child_execution` is `"job"` (the default); the mini TUI does
the same for its own `RuntimeSpec`. Because a `--child` process selects
its agent as the embedded root, a child that is itself an orchestrator
starts its own children the same way, so process isolation holds at every
depth of the delegation closure.

## 8. Stability

This is a private wire format between one `harw-agent-runner` binary and
itself — parent and child are always built from the same executable — not
a public API. Variants may gain fields across versions as long as
`PROTOCOL_VERSION` is bumped alongside any change that is not purely
additive; §3's strict equality check means an old parent talking to a new
child (or the reverse) fails closed with a named version mismatch rather
than misinterpreting an unfamiliar frame.
