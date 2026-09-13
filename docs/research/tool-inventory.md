# Tool-Inventar: Codex / Hermes / OpenClaw

**Status:** Recon-Ergebnis, versioniert für den Rust-Design-Vergleich.
**Quellen:** `codex-rs/`, `~/.hermes/`, `inspirations/openclaw/`.
**Bindet an:** `philosophy.md` §10 (Operations ≠ Tools ≠ Permissions), §11 (Agent-Computer-Schnittstelle), §12 (Authority-Reduktion).

---

## Übersichtstabelle

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

## Cluster

- **Shell:** Codex hat drei Stufen (`shell_command`, `unified_exec`, `run_user_shell_command`). Hermes fasst zu `terminal` + `close_terminal`. OpenClaw delegiert extern via `gateway`.
- **FS-Write:** Nur Codex mit `apply_patch` + eigenem Approval-Event. Hermes/OpenClaw schreiben via Shell durch.
- **Web:** `web_search` in allen dreien, `web_fetch` in Hermes + OpenClaw, `image_generation` nur Codex.
- **Task/Todo:** Codex verwaltet `update_plan` (Turn-lokal); Hermes/OpenClaw haben `cron` (dauerhaft).
- **Memory:** Nur Hermes hat First-Class-`memory`-Tool — klare Lücke bei den anderen.
- **MCP-Bridge:** Codex `tool_search` + Plugin-Install-Elicitation. Hermes 3-stufig `tool_search` → `tool_describe` → `tool_call`. OpenClaw eigene Channel-MCP-Tools.
- **Agent-Spawn:** Hermes `delegate_task`, OpenClaw `gateway`/`agents_list`. Codex keine direkte Spawn-Tool-Ebene — läuft über Session-Protokoll.
- **Sandbox/Approval:** Codex am tiefsten (Guardian, execpolicy, zwei Approval-Event-Klassen). OpenClaw deklarative `ToolAvailabilityExpression`. Hermes `clarify`.

---

## Systematische Unterschiede

- **Codex** — tiefstes Approval-Modell; `apply_patch` als kanonischer FS-Write; Deferred-Tool-Loading im Responses-API-Typ.
- **OpenClaw** — kein eigenes Shell/FS-Tool, alles über Plugins/MCP; einziger mit Channel-MCP-Tools; deklarative Availability-DSL.
- **Hermes** — breiteste Palette (Memory, TTS, Vision, Smart-Home, X-Search); 3-stufiges Tool-Bridge-Pattern; einziges mit First-Class-`memory`-Tool.

---

## Lücken in `harw-tools` heute

- **Memory** — kein Tool-Adapter für `harw-memory` (obwohl Backend fertig ist).
- **Agent-Spawn** — kein Adapter für Job-Runtime.
- **Browser/Computer-Use** — kein Crate.
- **Notification (TTS)** — kein Crate.
- **Cron** — `harw-job-runtime` existiert, aber kein Tool-Export.
- **Channel-MCP** — kein Adapter.

---

## Harw-Kanon (12 Tools, priorisiert)

1. `shell_command` — Shell/Exec, Sandbox-first
2. `apply_patch` — FS-Write via Sequence-Diff
3. `web_search` — cross-source
4. `web_fetch` — cross-source
5. `tool_search` — MCP-Bridge / Deferred Discovery
6. `update_plan` — Turn-Task-State
7. `request_user_input` — Human-in-Loop
8. `request_permissions` — Authority-Escalation
9. `memory_read` + `memory_write` — First-Class-Memory (Hermes-Lücke schließen)
10. `cron_schedule` — Persistent Scheduling
11. `delegate_task` — Agent-Spawn mit Lease/Budget (philosophy.md §6, §7)
12. `execute_code` — Sandbox-Runner
