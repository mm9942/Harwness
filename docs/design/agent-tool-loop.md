# Agent-as-tool execution contract

Harwness uses two intentionally different multi-agent paths.

## Bounded micro-agent as a tool

A `ToolExecutor` may call a narrow specialist agent, validate its structured
output, and return it as `ToolOutput::Json` or `ToolOutput::Text`. The core
then writes a `ToolResultItem` into the **requesting session's** history and
starts the next model invocation for that same session. The manager therefore
keeps ownership of the user-facing answer and can synthesize multiple
specialist results.

```text
manager model -> tool call `pricing_advisor`
              -> executor runs bounded specialist
              -> ToolResult is appended to manager history
              -> manager model runs again with ToolResult visible
              -> final manager answer
```

This is the correct shape for the `as_tool` agents in the IVI example:
package suggestion, lead tracking, language detection, sentiment, and similar
bounded specialists. Each executor must validate its output contract before it
returns it. The manager must not expose a specialist's private JSON unless the
user-facing contract explicitly requires it.

## Handoff

A `transfer_to_<role>` tool call is not a micro-agent tool. It means the
specialist owns the next response. Harwness changes the session to
`WaitingForChild` and returns `TurnOutcome::AwaitingChild`; only
`resume_after_child` may append the child result and continue the paused turn.

Use this when the specialist should take over the conversation branch, not when
the manager needs a bounded fact, classification, or structured recommendation.
The pending handoff stores the child session ID, originating parent call ID,
and role; a child result with any different child/call pair is rejected before
the parent turn can resume.

## Loop and failure boundaries

One `run_turn` is one application turn:

1. Prepare context, instructions, history, and visible tools.
2. Call the current model.
3. Execute every returned normal tool call under approval policy.
4. Persist each `ToolResult` into the requesting session.
5. Call the same model again until there is a final answer, a handoff, or an
   approval pause.

Independent calls from one model response may be dispatched concurrently only
when every selected provider explicitly declares its tool parallel-safe and
there are no approval handlers. Handoffs and approval-governed calls always
remain serial. Results are joined and appended in the model's original call
order before the next model invocation.

Unexpected runtime/model failures transition the active session to `Failed`.
Approval requests and handoffs are expected pauses and preserve the existing
turn/session identity for later resumption. An approval pause stores the exact
tool call and request id in `WaitingForApproval`; resolution can only resume
that stored invocation, so an approval callback cannot change arguments or
approve a different call.

This follows the OpenAI Agents SDK model of a run: tool outputs stay in the
run's continuation history; tools and handoffs are separate ownership models.
