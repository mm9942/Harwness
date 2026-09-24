# Agents as Tools

> Status: implemented · Last reviewed: 2026-09-24

**Scope**: `AgentToolAdapter` — child agents as callable tools inside the
parent LLM loop.

## Contents

1. [Motivation](#1-motivation)
2. [Core invariant — authority monotonicity](#2-core-invariant--authority-monotonicity)
3. [Semantics: handoff vs. agent-as-tool](#3-semantics-handoff-vs-agent-as-tool)
4. [`AgentToolAdapter` — sketch](#4-agenttooladapter--sketch)
5. [Budget & cancellation](#5-budget--cancellation)
6. [Transcript embedding](#6-transcript-embedding)
7. [Parent/child fencing on crash/resume](#7-parentchild-fencing-on-crashresume)
8. [Registration via the `#[operation]` macro](#8-registration-via-the-operation-macro)
9. [Deliberately out of scope](#9-deliberately-out-of-scope)
10. [Testing plan](#10-testing-plan)

## 1. Motivation

Classical agent handoffs (`transfer_to_agent`) are a full-transfer pattern:
the calling agent gives up control, and the new agent takes over the entire
loop. That is semantically simple but too coarse for many tasks — especially
when the parent needs to keep orchestrating after a specialized sub-task is
done.

**Agents as tools** inverts control: the parent agent's model calls a child
agent like an ordinary tool, waits for the structured result, and continues
its own loop. The child agent is fully encapsulated — its internal history,
tool calls, and intermediate steps are not directly visible to the parent
(unless `include_transcript` is enabled, see section 6).

### Reference: OpenAI Agents SDK

The OpenAI Agents SDK exposes this pattern via `agent.as_tool()`:

```python
researcher = Agent(name="researcher", instructions="...")
tool = researcher.as_tool(
    tool_name="research_topic",
    tool_description="Researches a topic and returns a summary.",
)
```

Harwness implements an equivalent pattern at the Rust level via
`AgentToolAdapter`, which uses the existing `Operation` contract and works
seamlessly with `ManagedAgentSpawner` and `AwaitingChild`.

### Why this pattern

- `ManagedAgentSpawner` provides safe child lifecycle management.
- `AwaitingChild` solves the synchronization problem for parent loops.
- Parallel tool execution is already implemented — a child agent is
  structurally just another tool that can be parallelized.
- The missing piece was the authority-reduction model described below.

## 2. Core invariant — authority monotonicity

> **A child agent never holds more authority than the parent call that
> created it.**

This invariant is the security-critical foundation of the whole adapter. It
is not optional and cannot be overridden at runtime.

### Concrete forms

| Dimension | Rule |
|---|---|
| **Sandbox permissions** | `child_permissions ⊆ parent_permissions` — a strict subset |
| **Capabilities** | A child can never enable a capability the parent doesn't have |
| **Approval level** | `child_approval_level ≤ parent_approval_level` (Owner > Operator > User > Observer) |
| **Tool whitelist** | The child only gets tools the parent explicitly delegates |
| **Network access** | The child's network scope is a subset of the parent's scope |

### Examples

```
Owner parent    -> can spawn an Observer child    OK (monotone)
Owner parent    -> can spawn an Operator child    OK
Observer parent -> can spawn an Owner child       REJECTED (authority violation, runtime error)
Operator parent -> can spawn an Operator child    OK (same level is allowed)
```

The `authority_reducer` function (section 4) is monotonically decreasing by
signature: it takes a `SandboxSpec` and returns a `SandboxSpec` that only
ever removes rights, never adds them. An implementation that adds rights is
rejected on the first `invoke` with `OpError::AuthorityViolation`.

### Runtime verification

`AgentToolAdapter::invoke` checks, before spawning:

1. `child_sandbox.permissions ⊆ ctx.sandbox().permissions` — set-theoretic.
2. `child_sandbox.approval_level ≤ ctx.sandbox().approval_level` — ordinal.
3. If either condition is violated: immediate `OpError::AuthorityViolation`,
   no spawn.

## 3. Semantics: handoff vs. agent-as-tool

The two central delegation patterns in Harwness:

### Handoff (`transfer_to_agent`)

- The current agent ends its loop.
- The conversation is fully handed to the new agent.
- The original agent has no further control afterward.
- Suited for: forwarding, escalation, specialized endpoints.
- Analogy: a phone transfer — the original operator hangs up.

### Agent-as-tool

- The parent agent calls the child agent like a tool.
- The parent loop continues after the child responds.
- The child agent is fully encapsulated (a black box).
- The child's result is a structured `OpOutput`.
- Suited for: research sub-tasks, specialized computation, code generation,
  summarization — anything with a defined end.
- Analogy: a function call — the caller gets a return value.

### Which pattern, when

| Criterion | Handoff | Agent-as-tool |
|---|---|---|
| Parent must keep working after delegation | No | Yes |
| Child result feeds into a parent decision | No | Yes |
| Conversation continuity stays with the parent | No | Yes |
| Budget control for the child is needed | Hard | Yes (native) |
| Multiple children in parallel | No | Yes |

## 4. `AgentToolAdapter` — sketch

`AgentToolAdapter` implements the `Operation` contract and so integrates
seamlessly into the existing tool dispatcher.

```rust
/// Adapter that exposes a child agent as a parent-callable tool.
/// Implements the `Operation` contract (see interaction-contract.md).
pub struct AgentToolAdapter {
    /// Definition of the child agent to spawn.
    child_agent: Arc<AgentDefinition>,

    /// Monotonically decreasing function: reduces the parent's SandboxSpec
    /// to a subset for the child. MUST NEVER add rights.
    authority_reducer: fn(&SandboxSpec) -> SandboxSpec,

    /// Budget limits for the child spawn (tokens, tool calls, wall time).
    budget: AgentBudget,

    /// Whether the child transcript is inlined into the parent context.
    include_transcript: bool,
}

impl AgentToolAdapter {
    /// Creates a new adapter. Immediately validates that `authority_reducer`
    /// is a monotone function (smoke test against a maximal sandbox).
    pub fn new(
        child_agent: Arc<AgentDefinition>,
        authority_reducer: fn(&SandboxSpec) -> SandboxSpec,
        budget: AgentBudget,
        include_transcript: bool,
    ) -> Result<Self, OpError> {
        let max_sandbox = SandboxSpec::maximum();
        let reduced = authority_reducer(&max_sandbox);
        if !reduced.is_subset_of(&max_sandbox) {
            return Err(OpError::AuthorityViolation(
                "authority_reducer adds rights — forbidden".into(),
            ));
        }
        Ok(Self { child_agent, authority_reducer, budget, include_transcript })
    }

    /// Calls the child agent and waits for its result.
    ///
    /// Flow:
    ///   1. Reduce authority (verified monotone).
    ///   2. Build the child OpContext (inherit session_id, fresh turn_id).
    ///   3. Clamp budget to the parent deadline (timeout propagation).
    ///   4. Spawn via ManagedAgentSpawner with budget enforcement.
    ///   5. Wait on AwaitingChild; a budget overrun -> OpError::Execution.
    ///   6. Optionally embed the transcript.
    ///   7. Return a structured OpOutput.
    pub async fn invoke(
        &self,
        ctx: &OpContext,
        input: OpInput,
    ) -> Result<OpOutput, OpError> {
        let child_sandbox = (self.authority_reducer)(ctx.sandbox());
        self.verify_authority_monotone(ctx.sandbox(), &child_sandbox)?;

        let child_ctx = OpContext::builder()
            .session_id(ctx.session_id())       // inherited
            .turn_id(TurnId::new_child())        // fresh
            .sandbox(child_sandbox)
            .cancellation_token(ctx.cancellation_token().child())
            .build();

        let effective_budget = self.budget.clamp_to_deadline(ctx.deadline());

        let spawner = ctx.managed_spawner();
        let child_handle = spawner.spawn(
            Arc::clone(&self.child_agent),
            child_ctx,
            input,
            effective_budget,
        ).await?;

        let child_result = child_handle.await_result().await?;

        let output = if self.include_transcript {
            child_result.into_output_with_transcript()
        } else {
            child_result.into_output()
        };

        Ok(output)
    }

    fn verify_authority_monotone(
        &self,
        parent: &SandboxSpec,
        child: &SandboxSpec,
    ) -> Result<(), OpError> {
        if !child.is_subset_of(parent) {
            return Err(OpError::AuthorityViolation(format!(
                "child sandbox {:?} is not a subset of parent sandbox {:?}",
                child, parent
            )));
        }
        Ok(())
    }
}
```

### `AgentBudget`

```rust
/// Budget limits for a child agent spawn.
pub struct AgentBudget {
    /// Maximum token count (input + output) across all of the child's turns.
    pub max_tokens: u32,

    /// Maximum number of tool calls by the child.
    pub max_tool_calls: u32,

    /// Maximum wall time in milliseconds.
    pub max_wall_time_ms: u64,
}

impl AgentBudget {
    /// Trims the budget so the child finishes before the parent's deadline.
    /// Returns the smaller of the two time limits.
    pub fn clamp_to_deadline(&self, parent_deadline: Option<Instant>) -> Self {
        match parent_deadline {
            None => self.clone(),
            Some(deadline) => {
                let remaining_ms = deadline
                    .duration_since(Instant::now())
                    .as_millis() as u64;
                Self {
                    max_wall_time_ms: self.max_wall_time_ms.min(remaining_ms),
                    ..self.clone()
                }
            }
        }
    }
}
```

## 5. Budget & cancellation

### Budget enforcement

The budget is enforced in the child turn loop. When a limit is exceeded, the
child loop ends immediately with `OpError::Execution("budget exceeded:
<dimension>")`. The parent receives this as a structured return value — not
as a panic or an unexpected termination.

| Limit | Enforcement point |
|---|---|
| `max_tokens` | Checked against accumulated tokens after every LLM call |
| `max_tool_calls` | Checked before every tool dispatch in the child loop |
| `max_wall_time_ms` | Polled via the cancellation token (~every 100ms) |

### Cancellation

The parent holds a `CancellationToken`. When the parent loop aborts (user
cancel, its own timeout, an error), a signal propagates to the child token
via `cancellation_token.child()`.

The child loop polls the token at least once per iteration (after every LLM
response). The maximum latency between a cancellation signal and the child
stopping is **under 100ms** in the normal case (no blocking I/O).

```rust
// In the child turn loop, after every LLM response:
if child_ctx.cancellation_token().is_cancelled() {
    return Err(OpError::Cancelled);
}
```

### Timeout propagation

```
parent_deadline present?
  no  -> child_budget unchanged
  yes -> child_budget.max_wall_time_ms = min(
             child_budget.max_wall_time_ms,
             parent_deadline - Instant::now()
         )
```

This ensures a child never runs longer than its parent has time left, even
if the child's own budget is configured generously.

## 6. Transcript embedding

### Default behavior (no transcript)

Only the child's final `OpOutput` flows into the parent context. The
child's internal history (tool calls, intermediate results, LLM turns) is
invisible to the parent.

```
Parent context:
  [... earlier turns ...]
  Tool call: spawn_researcher(input)
  Tool result: { summary: "...", confidence: 0.87 }   <- only this
  [next turn ...]
```

### Optional transcript (`include_transcript: true`)

When enabled, the full child transcript is embedded as a nested object in
`OpOutput` and so flows into the parent context. This significantly raises
token cost but can help with debugging, or where the parent needs to know
the child's reasoning path.

### Redaction rule (non-negotiable)

Regardless of `include_transcript` and regardless of any
`--log-sensitive` flag:

- **The child's system prompt is never logged** and never appears in the
  embedded transcript.
- **Tool arguments marked `sensitive: true` are never embedded** — they are
  replaced with `[REDACTED]`.
- This rule also applies to the child transcript inside the parent context:
  the security boundary between parent and child holds even when transcript
  embedding is active.

Reasoning: a parent agent could be compromised. The child's system prompt is
its authorization basis — it must never leak upward through the embedded
transcript.

## 7. Parent/child fencing on crash/resume

### Child crash during normal execution

`ManagedAgentSpawner` catches all child panics and errors and converts them
into structured `OpError` variants:

| Child failure | Parent receives |
|---|---|
| Unhandled panic | `OpError::Execution { class: "panic", message: "..." }` |
| Budget exceeded | `OpError::Execution { class: "budget_exceeded", ... }` |
| Tool failure in the child | `OpError::Execution { class: "tool_failure", ... }` |
| Network timeout in the child | `OpError::Execution { class: "timeout", ... }` |

The parent can react to any of these and decide whether to retry, fall
back, or propagate the error upward.

### Parent crash while a child is running

If the parent process ends unexpectedly (SIGKILL, OOM, panic),
`ManagedAgentSpawner::cancel_all_children()` runs via a `Drop` handler,
which:

1. Sends the cancellation signal to every running child token.
2. Waits at most **500ms** for a clean child exit.
3. Then force-terminates remaining child processes (SIGTERM -> SIGKILL).

### Resume after restart

After a crash/restart, only the **parent session** is restored. Child
agents are **not** restored. Reasoning:

- A child's authority is bound to the original parent call. After a parent
  restart that call is no longer active — a restored child would hold
  authority with no valid principal.
- "Reissuing" authority would require verifying the original `OpContext`,
  which is not safely possible without a full replay chain.
- Instead: the parent loop, on resume, restarts from the point where it
  issued the child tool call and re-spawns the child if needed, with a
  fresh budget.

## 8. Registration via the `#[operation]` macro

`AgentToolAdapter` integrates through the existing `#[operation]` macro. An
`agent_tool(...)` attribute triggers automatic adapter generation — no
manual `impl` block needed.

```rust
/// Delegates a research task to the "researcher" sub-agent.
/// Budget: 8,000 tokens, 20 tool calls, 30 seconds wall time.
///
/// Requires Operator permission; the child runs in read-only mode.
#[operation(
    name = "spawn_researcher",
    summary = "Delegates to the 'researcher' sub-agent with an 8k-token budget.",
    domain = "agents",
    permission = "operator",
    agent_tool(
        child = "researcher",
        authority = "reduce_to_read_only",
        budget = "8k_tokens,20_tool_calls,30s",
        include_transcript = false,
    ),
)]
async fn spawn_researcher(ctx: &OpContext, input: OpInput) -> Result<OpOutput, OpError> {
    /* Body generated by the `#[operation]` macro — no manual impl needed.
       The macro instantiates AgentToolAdapter with the attribute's
       parameters and delegates to `invoke`. */
}
```

### `authority` functions

The macro's `authority` parameter references a named function in crate
scope:

```rust
/// Reduces a SandboxSpec to read-only: removes all write and execute permissions.
fn reduce_to_read_only(parent: &SandboxSpec) -> SandboxSpec {
    parent.clone().without_permissions(&[
        Permission::FileWrite,
        Permission::FileDelete,
        Permission::ProcessSpawn,
        Permission::NetworkWrite,
    ])
}
```

## 9. Deliberately out of scope

### Agent self-modification

An agent cannot, at runtime, change its own system prompt, tool list, or
sandbox configuration. This also applies to child agents spawned through
`AgentToolAdapter`. Self-modification would endanger authority
monotonicity and is excluded on principle.

### Cross-session child reuse

A child agent spawned in one session cannot be reused in another session.
Every `AgentToolAdapter::invoke` call spawns a fresh child. Pooling and
reuse of children is not implemented.

### Explicit cost/token accounting persistence

Token cost and tool-call counts for a child are not persisted across
sessions by this adapter. They are included in the `AwaitingChild` response
and can be read by the parent, but there is no database layer aggregating
this data across sessions.

### Bidirectional communication during child execution

The parent cannot send messages to a running child. The pattern is: call ->
wait -> result. Streaming intermediate results is not supported.

## 10. Testing plan

### Unit tests

- **Authority reducer is monotone (property test):** for all `SandboxSpec`
  values `S` and all registered `authority_reducer` functions `f`,
  `f(S).is_subset_of(S) == true`.
- **Authority violation at construction:** `AgentToolAdapter::new` with an
  `authority_reducer` that adds rights returns
  `Err(OpError::AuthorityViolation)`.
- **Budget clamp against a deadline:** `AgentBudget::clamp_to_deadline` with
  `parent_deadline < budget.max_wall_time_ms` yields
  `effective_budget.max_wall_time_ms == remaining_ms`.

### Integration tests

- **Budget overrun -> `OpError::Execution`:** a child that deliberately
  generates more than its token budget returns
  `Err(OpError::Execution { class: "budget_exceeded", ... })`.
- **Parent cancel -> child sees `Cancelled` within < 100ms:** cancelling the
  parent token 50ms into a child call with a simulated 500ms response
  latency yields `Err(OpError::Cancelled)` measured under 100ms after
  cancel.
- **Runtime authority violation:** an `invoke()` call with a manually
  tampered `child_sandbox` (extra permissions added after reduction)
  returns `Err(OpError::AuthorityViolation(...))`.
- **Transcript redaction:** a child tool call carrying `sensitive: true`
  arguments, with `include_transcript: true`, must show
  `tool_call.arguments["api_key"] == "[REDACTED]"` in the embedded
  transcript.
- **End-to-end:** a parent spawning a mock "researcher" child receives the
  research result in `OpOutput.content`, continues its own loop, and the
  child session is removed from `ManagedAgentSpawner` afterward.

## Implementation

Implemented in `harw-core-bridge/src/agent_tool.rs`. The shipped adapter
extends this design with a few load-bearing additions not in the original
sketch:

- Child responses are validated against a typed `ChildReturnContract` from
  the child's agent IR, rather than passed through as free text; a contract
  violation produces a structured error *output*, not an `Err` — it is a
  data error from the child, not a runtime error in the tool.
- An unreadable `budget_hint` makes the tool unavailable outright; it never
  silently falls back to "no limit".
- `AwaitingChild`/`AwaitingApproval` pauses are only allowed when the
  child's `ChildRecord` carries `allow_pause = true`; otherwise the call
  fails closed with a precise message.
- Admission slots are released via an RAII guard (`ChildSlotGuard`) after
  the child's result is fully consumed (text copied, contract checked), and
  on every error, contract violation, budget abort, or dropped future — with
  the one exception of an allowed pause, where the caller owns continuation.
- Reasoning effort follows the same monotonic-reduction rule as the sandbox
  and budget (`clamp_child_reasoning_effort`): there is no effort override
  from model-supplied tool arguments, since those are model output and not
  proof of ownership.
