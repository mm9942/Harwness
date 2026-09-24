# Tool Inventory: Codex / Hermes / OpenClaw

> Status: implemented · Last reviewed: 2026-09-24

A recon snapshot comparing the tool surfaces of three reference agent
stacks (Codex, Hermes, OpenClaw), used as a design comparison for
`harw-tools`. Binds to `philosophy.md` §10 (operations != tools !=
permissions), §11 (agent-computer interface), §12 (authority reduction).

---

## Overview table

| Tool | Kategorie | codex | hermes | openclaw | Notes |
|---|---|---|---|---|---|
| `shell_command` / `terminal` | Shell | `core/tools/handlers/shell_spec.rs` | `tools/terminal_tool.py` | — | Aliases `exec_command`, `local_shell`, `shell` |
| `unified_exec` | Shell/Sandbox | `tools/tool_config.rs` | — | — | PTY-based, ConPTY-gated |
| `apply_patch` | FS-Write | `core/tools/handlers/apply_patch_spec.rs` | — | — | Sequence-Diff + eigenes Approval-Event |
| `web_search` | Web | `tools/tool_spec.rs` | `tools/web_tools.py` | `agents/tools/web-search.ts` | Alle drei |
| `web_fetch`/`web_extract` | Web | — | `tools/web_tools.py` | `agents/tools/web-fetch.ts` | zwei Quellen |
| `image_generation` | Web/Media | `tools/tool_spec.rs` | — | — | Responses-API-nativ |
| `tool_search` | MCP-Bridge | `tools/tool_discovery.rs` | `tools/tool_search.py` | — | Deferred Discovery |
| `tool_describe` | MCP-Bridge | — | `tools/tool_search.py` | — | 3-stufiges Lazy-Loading |
| `tool_call` | MCP-Bridge | — | `tools/tool_search.py` | — | Bridge-Exec |
| `request_plugin_install` | Approval | `tools/tool_discovery.rs` | — | — | Elicitation |
| `update_plan` | Task/Todo | `core/tools/handlers/plan.rs` | — | — | Checklist |
| `request_user_input` | Approval | `core/tools/handlers/request_user_input_spec.rs` | — | — | Human-in-Loop |
| `request_permissions` | Approval | `core/tools/handlers/shell_spec.rs` | — | — | Escalation |
| `memory` | Memory | — | `tools/memory_tool.py` | — | R/W persistent |
| `clarify` | Approval | — | `tools/clarify_tool.py` | — | User-Gate |
| `execute_code` | Sandbox | — | `tools/code_execution_tool.py` | — | Sandbox-Runner |
| `computer`/`computer_use` | Browser | — | `tools/computer_use_tool.py` | `agents/tools/computer-tool.ts` | Desktop-Automation |
| `browser_cdp` | Browser | — | `tools/browser_cdp_tool.py` | — | Chrome-DevTools-Protocol |
| `crestodian` | Approval | — | — | `agents/tools/crestodian-tool.ts` | Deklarative Availability |
| `cron`/`cronjob` | Task | — | `tools/cronjob_tools.py` | `agents/tools/cron-tool.ts` | Scheduling |
| `delegate_task` | Agent-Spawn | — | `tools/delegate_tool.py` | — | Sub-Agent |
| `gateway` | Agent-Spawn | — | — | `agents/tools/gateway-tool.ts` | Cross-Runtime |
| `agents_list` | Agent-Spawn | — | — | `agents/tools/agents-list-tool.ts` | Introspect |
| `conversations_list`, `messages_read/send`, `events_poll/wait` | MCP-Bridge | — | — | `mcp/channel-tools.ts` | Channel-MCP |
| `vision_analyze`/`video_analyze` | Web/Media | — | `tools/vision_tools.py` | — | Multimodal |
| `text_to_speech` | Notification | — | `tools/tts_tool.py` | — | Audio-Out |
| `x_search` | Web | — | `tools/x_search_tool.py` | — | X/Twitter |
| `close_terminal` | Shell | — | `tools/close_terminal_tool.py` | — | Explizit-Lifecycle |
| `ha_*` (4) | IDE/IoT | — | `tools/homeassistant_tool.py` | — | Smart-Home |

---

## Clusters

- **Shell:** Codex has three tiers (`shell_command`, `unified_exec`,
  `run_user_shell_command`). Hermes collapses to `terminal` +
  `close_terminal`. OpenClaw delegates externally via `gateway`.
- **FS write:** Only Codex has `apply_patch` with its own approval event.
  Hermes/OpenClaw write through the shell.
- **Web:** `web_search` in all three, `web_fetch` in Hermes + OpenClaw,
  `image_generation` only in Codex.
- **Task/todo:** Codex manages `update_plan` (turn-local); Hermes/OpenClaw
  have `cron` (persistent).
- **Memory:** Only Hermes has a first-class `memory` tool — a clear gap in
  the others.
- **MCP bridge:** Codex has `tool_search` plus plugin-install elicitation.
  Hermes has a 3-stage `tool_search` -> `tool_describe` -> `tool_call`.
  OpenClaw has its own channel-MCP tools.
- **Agent spawn:** Hermes has `delegate_task`, OpenClaw has
  `gateway`/`agents_list`. Codex has no direct spawn-tool layer — it runs
  over the session protocol.
- **Sandbox/approval:** Codex goes deepest (Guardian, execpolicy, two
  approval-event classes). OpenClaw has a declarative
  `ToolAvailabilityExpression`. Hermes has `clarify`.

---

## Systematic differences

- **Codex** — deepest approval model; `apply_patch` as the canonical FS
  write; deferred tool loading in the Responses-API type.
- **OpenClaw** — no own shell/FS tool, everything via plugins/MCP; the only
  one with channel-MCP tools; a declarative availability DSL.
- **Hermes** — broadest range (memory, TTS, vision, smart home, X search); a
  3-stage tool-bridge pattern; the only one with a first-class `memory`
  tool.

---

## Gaps in `harw-tools` today

- **Memory** — no tool adapter for `harw-memory`, even though the backend
  is complete.
- **Agent spawn** — child agents are invoked via `AgentToolAdapter`
  (`harw-core-bridge`, see `docs/design/agents-as-tools.md`), not through a
  `harw-tools` adapter; there is no `harw-tool-*` crate for it.
- **Notification (TTS)** — no crate.
- **Cron** — `harw-job-runtime` exists, but there is no tool export for it.
- **Channel-MCP** — no adapter.

Browser/computer-use is no longer a gap: `harw-browser` and
`harw-tool-browser` exist.

---

## Harwness canon (priority tool list)

1. `shell_command` — shell/exec, sandbox-first
2. `apply_patch` — FS write via sequence diff
3. `web_search` — cross-source
4. `web_fetch` — cross-source
5. `tool_search` — MCP bridge / deferred discovery
6. `update_plan` — turn-scoped task state
7. `request_user_input` — human-in-the-loop
8. `request_permissions` — authority escalation
9. `memory_read` + `memory_write` — first-class memory (closing the Hermes gap)
10. `cron_schedule` — persistent scheduling
11. `delegate_task` — agent spawn with lease/budget (philosophy.md §6, §7)
12. `execute_code` — sandbox runner
