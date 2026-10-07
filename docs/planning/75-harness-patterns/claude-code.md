---
id: HP-CC
title: Research sheet — Claude Code
status: research
date: 2026-09-27
tags: [harness-patterns, research, claude-code, mcp, hooks, workflows]
related:
  - README.md
  - ../85-gap-hunt/README.md
  - ../70-decisions/DEC-003-provider-limits.md
---

# Research sheet: Claude Code

This is research carried out by a Sonnet agent (read-only). Every claim has a
docs URL; anything that could not be confirmed is marked. The sheet supplies
material for the cross-cutting pattern catalog ([README](README.md)).

## Findings

### 1. Headless operation and JSON output
- **Invocation and flags:**
  - `claude -p` with `--output-format text|json|stream-json`;
  - `--bare` skips hooks, skills, MCP, plugins, and the CLAUDE.md lookup;
  - plus `--allowedTools`, `--permission-mode`, `--permission-prompts none`, `--append-system-prompt[-file]`, and `--continue`/`--resume`.
  - Source: https://code.claude.com/docs/en/headless.md
- **Structured output:** `--output-format json --json-schema '<schema>'` returns the validated result as `structured_output` alongside the session metadata. An invalid schema aborts immediately.
- **Stream (`stream-json --verbose --include-partial-messages`):**
  - The first event is `system/init` with model, tools, `mcp_servers[]{name,status}`, plugins, errors, and capabilities.
  - Subagent messages carry `parent_tool_use_id`.
  - Retries arrive as `system/api_retry` (`attempt`, `max_retries`, `retry_delay_ms`, `error_status`).
  - The stream ends with `result`, carrying `total_cost_usd`, `modelUsage` per model, and `permission_denials`.
- *Not confirmed:* the literal field list of `num_turns`, `duration_ms`, and `is_error`, as well as the option shapes of `query()` in the Agent SDK.
- Structured output in the SDK (JSON Schema/Zod/Pydantic, retries up to `MAX_STRUCTURED_OUTPUT_RETRIES`): https://code.claude.com/docs/en/agent-sdk/structured-outputs.md

### 2. MCP
- **Scopes and transports:**
  - Scopes: local and user in `~/.claude.json`, project in `.mcp.json`.
  - Transports: stdio, streamable HTTP (recommended), SSE (deprecated), WebSocket.
- **Tool names:** `mcp__<server>__<tool>`; for plugins `mcp__plugin_<plugin>_<server>__<tool>`.
- **Miscellaneous:**
  - Resources and prompts with `list_changed`, OAuth, `claude mcp login`.
  - Output limits via `MAX_MCP_OUTPUT_TOKENS` (warning at 10k, truncation at 25k, overflow into a file).
  - `claude mcp serve` runs Claude Code itself as an MCP server.
  - Source: https://code.claude.com/docs/en/mcp.md
- **Structured tool output (MCP `2025-06-18`):** `outputSchema` on the tool and `structuredContent` in the result. The server MUST conform to it, the client SHOULD validate. https://modelcontextprotocol.io/specification/2025-06-18/server/tools
- **Elicitation:** `elicitation/create` with the responses `accept`, `decline`, or `cancel`, flat schemas only. Claude Code provides the hooks `Elicitation`/`ElicitationResult` for it.

### 3. Subagents and SDK
- **Frontmatter in `.claude/agents/*.md`:**
  - `name`, `description`, `tools`, `disallowedTools`, `model`, `permissionMode`, `maxTurns`;
  - `skills`, `mcpServers`, `hooks`, `memory`, `background`, `isolation`, `effort`, and more.
- **Return to the parent:** In the foreground, the call blocks and returns the result. In the background, a message arrives on completion; the agent can be resumed with `SendMessage`.
- **Limits:** `CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS=20` and a spawn depth of 3.
- Source: https://code.claude.com/docs/en/sub-agents.md
- **Cost:** `total_cost_usd`, `usage` per step (deduplicated by message ID), and `modelUsage`. The totals are stored in the transcript and restored on `resume`. https://code.claude.com/docs/en/agent-sdk/cost-tracking.md

### 4. Hooks
- **Events:**
  - Tools: `PreToolUse`, `PostToolUse`, `PostToolUseFailure`;
  - Session and prompt: `UserPromptSubmit`, `SessionStart`/`End`;
  - Stopping: `Stop`, `StopFailure`, `SubagentStart`/`Stop`;
  - Permissions: `PermissionRequest`/`Denied`;
  - Miscellaneous: `Notification`, `Pre`/`PostCompact`, `Pre`/`PostModelSwitch`, `Elicitation*`, `Setup`, and asynchronous file and cwd events.
- **Contract:**
  - stdin is JSON with `session_id`, `transcript_path`, `cwd`, `permission_mode`, `hook_event_name`, and `agent_id`.
  - stdout is JSON with `hookSpecificOutput.{permissionDecision, updatedInput, blockStopping, stopReason, additionalContext}`.
  - Exit codes: 0 means the JSON is read; 2 blocks, with the message taken from stderr; any other code does not block.
- **Handler types:** `command`, `http`, `mcp_tool`, `prompt`, `agent`.
- **Stop hook with a goal:**
  - `blockStopping: true` forces the session to keep working.
  - `/goal` is a **per-session stop hook with a prompt**: a small model rules `met`, `not yet met`, or `impossible`.
  - Sources: https://code.claude.com/docs/en/hooks.md and https://code.claude.com/docs/en/goal.md

### 5. Workflows, loops, skills
- **Dynamic workflows:**
  - A JS script with `agent(prompt,{schema,label,phase,model,agentType})`, `parallel`, `pipeline`, `phase`, and `log`.
  - The output is secured by JSON Schema with retries.
  - Journal and script live under `~/.claude/projects/…`.
  - Resume fetches finished agents from the cache.
  - Limits: `CLAUDE_CODE_WORKFLOW_MAX_CONCURRENT_AGENTS` (default 16, CPU-dependent) **per run**, 4096 elements per call, 1000 agents per run.
  - Source: https://code.claude.com/docs/en/workflows.md
  - Our kit (`../85-gap-hunt/kit/`) runs on exactly this. harw itself does **not have it yet**.
- **`/loop` and cron:**
  - `CronCreate`/`List`/`Delete` with 5-field cron and a scheduler tick of 1 s.
  - Plus jitter, expiry after 7 days, bound to the session, restored on `--resume`.
  - Source: https://code.claude.com/docs/en/scheduled-tasks.md
- Skills, plugins, output styles, and the status line were identified but not read in detail.

### 6. Cost and telemetry
- **OTel metrics:** `claude_code.cost.usage`, `claude_code.token.usage`, `session.count`, `lines_of_code.count`, `active_time.total`.
- **OTel events:** `user_prompt`, `api_request`, `api_error`, `tool_decision`, `tool_result`.
- Source: https://code.claude.com/docs/en/monitoring-usage.md
- **`/usage`:** total cost, API and wall-clock time, tokens and cost per model, cache statistics, attribution by skill, subagent, and MCP. https://code.claude.com/docs/en/costs.md

## Capability matrix Claude Code → harw

| # | Claude Code | harw today | Proposal |
|---|---|---|---|
| 1 | `-p --output-format stream-json` | **partial:** `harw-cli/src/output.rs` (text/JSON only for command results); `chat.rs` is interactive; `harw serve` is a job runtime | `harw run -p … --output-format text\|json\|stream-json` with a harw-specific `HeadlessEvent` (NDJSON) and a final `result` from the cost status (#5) |
| 2 | Workflows (agent/parallel/pipeline, schema, journal, resume, cap) | **missing as a feature.** The closest relative is the WorkDriver: `harw-plan-bridge/src/work_driver.rs` (decision core), `verify_exec.rs` (central verify, DEC-004), `harw-ops/src/work_driver.rs`, `harw-cli/src/job_worker_work_driver.rs`. That is a fixed algorithm, not a script | new `harw-workflow`: a script with `agent/parallel/pipeline/phase/log`, schema validation with bounded retries, one `journal.jsonl` per run with resume from the cache, **one** shared cap with DEC-003. Agents go through the runner and admission, so "never builds" is compiled in rather than merely prompted |
| 3 | Hooks (Pre/PostToolUse, Stop, `blockStopping`) | **missing generically.** `/goal` (`harw-ops/src/goal.rs`, `harw-plan` Goal/Criterion) and the judge steps cover the stop/goal case in a hard-wired way | `harw-hooks`: `HookEvent`, `trait Hook`, closed `HookDecision` (`Allow`, `Deny`, `BlockStopping`), `CommandHook` with the stdin-JSON and exit-code contract, integrated into the turn loop and the WorkDriver round |
| 4 | `/loop` and cron | **missing** (no scheduler found) | `schedule` operation (create/list/delete) plus a scheduler that starts jobs via the job coordinator |
| 5 | Cost status (`total_cost_usd`, per model, API time) | **partial:** `harw-core/src/state_store.rs` (`total_usage`, tokens only), `harw-ops/src/usage.rs`, `harw-session-store/src/meta.rs` | `CostSummary {total_cost_usd, duration_api_ms, num_turns, per_model}` in the snapshot, price table with override, survives resume |
| 6 | MCP `outputSchema`/`structuredContent`, elicitation | **probably partial or missing:** `harw-mcp-client` `McpTool` has only `input_schema`; `tool_bridge.rs` and `harw-mcp-server/src/session.rs` are still unchecked. Both already use the `2025-06-18` protocol | add `output_schema` and `structured_content`, validate against the schema, put elicitation behind a hook |

## Roadmap proposal of this sheet
1. Cost and usage status
2. `harw-hooks`
3. MCP with structured output and elicitation
4. Headless `harw run -p`
5. Scheduler
6. `harw-workflow`
7. Shared concurrency cap with DEC-003
8. Port the gap-hunt kit to `harw-workflow` and compare it against the Claude Code run

The final order is set by the cross-cutting catalog.

## Open items
- Before the MCP step, read `harw-mcp-client/src/tool_bridge.rs` and `harw-mcp-server/src/session.rs`.
- The **script engine for `harw-workflow`** needs its own DEC:
  - The choice is between an embedded engine and a Rust builder API.
  - An embedded engine must not show up in public types.
- Avoid a duplicate concurrency cap (pattern M2).
- Still look up the fields in the Agent SDK.
