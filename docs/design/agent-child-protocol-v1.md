# Agent Child Protocol v1 — `harwness.agent-child/v1`

> Status: implemented (#22, wave 3) · Last reviewed: 2026-09-25

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
  `--child <agent-id> --child-protocol harwness.agent-child/v1`. Parent and
  child are always the same binary — this is a private protocol between
  one `harw-agent-runner` process and another it started, never a public
  API a different program is expected to speak.

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

`JobChildBackend::run` then always writes a `Rights` frame first (§4.6),
then, if the run has a budget, a `Budget` frame (§4.5), then the `Task`
frame that actually starts the child's turn (§4.1). A parent may also send
`Rights`/`Budget`/`Mode` before the first `Task`; the child applies them
once its session exists.

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
| `continue_from` | string, optional | An opaque continuation token from a prior budget-ended run of this same child, if this resumes one. |

The child concatenates `task` and `context` (when present) into one prompt
text before sending it to its own session
(`format!("{task}\n\n{context}")` — `child::run_child_async`); `context`
is not yet passed to the model as a separate structured input.

### 4.2 `Message`

A follow-up message while the child's turn is already running
(parent-to-child chat, distinct from the initial `Task`).

```json
{"type": "message", "text": "hurry up"}
```

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

### 4.5 `Budget`

Updates the child's budget mid-run.

```json
{"type": "budget", "max_tokens": 1000, "max_tool_calls": null, "max_wall_time_ms": 60000}
```

All three fields are optional; an absent field leaves that budget
dimension as it was. `JobChildBackend` sends this once, right after
`Rights` and before `Task`, when the spawning run has a budget at all.

### 4.6 `Mode`

Switches the child between plan and live mode.

```json
{"type": "mode", "mode": "live"}
```

`mode` is `"plan"` (the child plans but does not act) or `"live"` (the
child acts).

### 4.7 `Rights`

Narrows the child's rights mid-run.

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

A backend only ever **narrows**: widening a child past its own manifest is
never valid, and `child::narrow_rights` enforces `min(child manifest,
rights from this frame)` field by field (tool set and host set
intersect, every boolean is AND'd) the moment this frame is applied. See
§6 for exactly when that happens today.

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
| `full_access` | bool | The manifest's full-access escape hatch. |

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

`id` is a fresh id, unique for this child's lifetime.

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
one of these frames (`child::RelayApprovals`); there is no local approval
policy inside a child process, only a relay to the parent.

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
| `cancelled` | Cancelled, via a `Cancel` frame or the child's own budget. |
| `failed` | Any other non-successful end (provider/turn error, refusal, truncation, …). |
| `budget_exhausted` | The run's budget ended it; `text` is the last (or handed-off) answer. |

`child::translate_turn_report` maps `harwness_sdk::TurnStatus::Completed`
to `completed`, `Cancelled` to `cancelled`, and `Truncated`/`Refused`/
`Failed` all to `failed` — the child side never produces
`budget_exhausted` today; that status exists in the protocol for a
budget-driven continuation flow (`continue_from` in `Task`, §4.1) that is
not yet wired end to end.

`text` is the final (or last, for a budget end) assistant text, if any.
`usage` is always present (defaulting to all zeros); its `fresh_tokens()`
is input plus output tokens, excluding cached reads, matching
`harw_types::TokenUsage::fresh_tokens`'s accounting unit.

### 5.5 `Usage`

A standalone usage update, sent independently of `Result` (for example
after each model round) so the parent can show live consumption without
waiting for the run to end.

```json
{"type": "usage", "usage": {"input_tokens": 3, "output_tokens": 1, "cached_input_tokens": 0, "tool_calls": 0, "wall_time_ms": 0}}
```

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

The child hit an error it could not otherwise report — a protocol decode
failure, or another failure before a session could even be built.

```json
{"type": "error", "message": "failed to build the child session: …"}
```

Distinct from `Result`'s `failed` status, which is a *result* of a turn
that ran: `Error` is sent when the child cannot even produce one (for
example before its first `Task` is understood, or if building its
`Harwness` fails).

## 6. Rights narrowing, and what is best-effort today

The child protocol's own rule (§4.7) is `min(child manifest, rights from
the parent's Rights frame)`, enforced by `child::narrow_rights` the moment
a `Rights` frame arrives. That much is exact. What feeds the *manifest*
side of that computation today, on the child-process side, is not yet the
same conversion the rest of the runner uses:

- `harw_runtime::embedded::EffectiveRights::from_manifest` (used by every
  top-level interface, §5.1 of the [agent compiler
  guide](../guides/agent-compiler.md)) reads `Permissions` field for field.
- `child::manifest_rights_of` — the child side's equivalent — instead
  reads `AgentIr::authority.capabilities`, a flat label list, and
  best-effort prefix-matches labels like `"network"`/`"filesystem.write"`/
  `"shell"`/`"host"`/`"full_access"` against it. This predates
  `EffectiveRights::from_manifest` and has not yet been replaced by it
  (see the module docs of `harw-agent-runner/src/child.rs`).

A second gap sits next to it: `child::run_child_async` computes the
narrowed `rights` value from whichever `Rights` frame arrives before the
first `Task`, but does not yet apply it to the session it builds — the
comment on that line (`// Applied to the built session below once that
seam exists`) marks the exact spot. Until both of these are closed, a
child process's actual tool/network/write/shell/host behavior still comes
from its own manifest as read by `Harwness::builder().embedded(..)`, not
from the (correctly computed, but not yet connected) narrowed value.

## 7. The parent side: `JobChildBackend`

`harw_core::child_backend::ChildBackend` is the trait a job-driven
delegation calls; `JobChildBackend` is this protocol's implementation of
it, over a real OS process:

1. Spawns `std::env::current_exe() --child <agent-id> --child-protocol
   stdio`, stdin/stdout/stderr all piped, in its **own process group**
   (`process_group(0)` on Unix) — the same guarantee `harw_tool_job` gives
   every job, so cancelling kills the whole group instead of leaking
   grandchildren.
2. Reads the first line and requires it to decode as `Hello` with a
   matching `protocol` (§3); any other first frame, or the process exiting
   before printing one, is a crash.
3. Writes `Rights`, then `Budget` if the run has one, then `Task` (§3).
4. Relays every subsequent frame: `Event` goes to the caller's `ChildIo`
   sink, `Question`/`ApprovalRequest` are awaited and answered with
   `Answer`, `Usage` is currently read and dropped, `Error` and `Result`
   end the loop.
5. On cancellation (`spec.cancel.cancelled()`), kills the whole process
   group (`rustix::process::kill_process_group`, `SIGTERM`) and waits for
   exit, rather than sending a graceful `Cancel` frame first — exactly one
   place (`run_one`'s own `tokio::select!`) decides "is this run
   cancelled", so there is no race between a `Cancel` frame and a kill.
6. A crash (the stdout stream ends without a `Result`) is reported as
   `ChildRunStatus::Crashed` with the process's exit code and its last 32
   lines of stderr.

`JobChildBackend` and the `--child` side of `harw-agent-runner` are both
implemented and unit-tested against a real spawned process (a `/bin/sh`
stand-in for the shell tests, a real embedded agent bundle for the
translation tests). What is **not** yet true is that a compiled agent's
own run reaches `JobChildBackend` automatically for
`[binary] child_execution = "job"`: nothing in `harw-runtime`'s embedded
assembly sets `RuntimeSpec::child_backend` for `EntryKind::CompiledAgent`
today (see [ADR 0001 §3.1](../adr/0001-agent-compiler.md#31-wave-3-consequences)).
Only an explicit `--child` invocation, or a test that constructs a
`JobChildBackend` directly, exercises this protocol as things stand.

## 8. Stability

This is a private wire format between one `harw-agent-runner` binary and
itself — parent and child are always built from the same executable — not
a public API. Variants may gain fields across versions as long as
`PROTOCOL_VERSION` is bumped alongside any change that is not purely
additive; §3's strict equality check means an old parent talking to a new
child (or the reverse) fails closed with a named version mismatch rather
than misinterpreting an unfamiliar frame.
