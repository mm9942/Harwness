---
id: PL94-W02-CHAT-FIRST-MINI-LIVE
title: "W02 — Chat-first miniature live agent monitor, no separate Progress Agent"
status: proposed
date: 2026-10-09
parent: ../README.md
baseline_dev: 090a9d2b786a3a6c025182d8da9c7b28bb7cbece
---

# W02 — Chat-first mini-live agent activity

> **Operator-approved UX clarification, 2026-10-09. PLAN ONLY.** The screenshot layout of three status windows above the main Chat is the reference. The earlier PL-94 proposal for a separate model-backed Progress Agent, automatically generated todo narratives, a progress bar and a second large output viewport is **superseded for the primary TUI design** by this contract. This wave makes no runtime code changes.

## CURRENT — confirmed source, not an implementation claim for W02

- [`harw-tui/src/child_stream.rs`](../../../harw-tui/src/child_stream.rs) already renders **admitted** live child assistant text, streamed *available/displayable* reasoning, grouped tool calls and statuses via `ChildStreamBlock`. It bounds entries and tails (`LIVE_MAX_CHARS = 2000`, `LIVE_MAX_LINES = 3`), redacts tool arguments/results and handles spawn/attach ordering. A remotely executing child with no bus stream shows no live data.
- [`harw-tui/src/agent_monitor.rs`](../../../harw-tui/src/agent_monitor.rs) tracks `TraceEntry::Text/Reasoning/ToolCall/ToolResult/Status`, `reasoning_preview`, phase, task, ID, parent and per-agent usage. Its trace is bounded (`MAX_TRACE_ENTRIES = 400`, `MAX_TRACE_BYTES = 64 KiB`). Monitoring trace and authorized child stream have **different semantics**; do not fabricate a full child transcript from a preview.
- [`harw-tui/src/app/child_stream_glue.rs`](../../../harw-tui/src/app/child_stream_glue.rs) attaches streams to the existing Chat and exposes mode `/agent stream <orchestrators|all|none>` and Ctrl+O expansion.
- [`harw-tui-layout/src/dock.rs`](../../../harw-tui-layout/src/dock.rs) and [`harw-tui/src/panes.rs`](../../../harw-tui/src/panes.rs) currently split portrait status as Agents/Jobs only. The third Finished/Failed rectangle and mini-live viewport are **not yet implemented**.
- [PL-94 W01](W01-activity-ranked-inline-chat.md) separately proposes dynamic positioning of running agent message cells in the primary chat; its behavior **must not** be confused with the compact, independently rendered W02 status/tail projection.

## TARGET — user-visible appearance

Portrait, illustrative only; leave the Chat the **largest** region:

```text
┌ ACTIVE AGENTS ──────────┬ ACTIVE JOBS ───────────┐
│ ● root · thinking       │ ● build · running       │
│ ● worker · coding       │ ○ tests · waiting       │
├ FINISHED / FAILED ───────────────────────────────┤
│ ✓ 11 · ✗ 38  (select for real details)            │
├ LIVE · root-orchestrator ─────────────────────────┤
│ ∴ Checking Lens and internal tool contracts…     │
│ ● fs.read · in progress · 1.2s                    │
├ MAIN CHAT ───────────────────────────────────────┤
│                                                 │
│   USER / UIA / assistant conversation            │
│   committed messages and authorized streams     │
│                                                 │
│                                                 │
│                                                 │
│                                                 │
├─────────────────────────────────────────────────┤
│ Composer / approvals / status                     │
└─────────────────────────────────────────────────┘
```

The three status windows retain **separate identities**: Active Agents, Active Jobs and Finished/Failed. Live is a **small fourth projection** and **not** a fourth persistent lifecycle store. Do not turn the screenshot's narrow "Progress" rectangle into a second chat with its own composer, a todo board, a model-generated progress summary or a permanently expanded reasoning transcript.

### Geometry invariants

1. Reserve footer/status, modal approvals and main Chat *first*. Chat spans full available portrait width and has precedence over all monitor detail. Live window uses the existing small progress-strip height budget; target ~3–4 content lines **maximum including compact tool/status**, with one border/title row as feasible. If space is short: clip live body first, then collapse to a one-line `LIVE role · last tool` header, then hide it in Summary mode while preserving accessible detail. Do **not** shrink Chat to preserve live output.
2. No terminal/mobile OS detection. Use actual Ratatui cell dimensions and the existing responsive fallbacks; use saturating geometry; no overlapping/zero-size invalid rectangles.
3. Landscape: retain the PL-94 anchors (Chat left and largest; top-center Finished/Failed; far-right Agents above Jobs). The middle WIP becomes an **authorized live-agent stream/detail view** where feasible, **not** a second independent model output. If Chat minimum isn't met, collapse live view before Chat. A more generous independently scrollable live view is permitted only in sufficiently spacious landscape.
4. Three status cards remain independently focusable; compact live normally remains nonfocused/non-scrolling, and detail/maximize only on deliberate operator action. Keyboard focus and composer draft stay stable.
5. Resizing, switching the selected monitor agent and expanding/collapsing view are **presentation only**. Never restart an agent, reparent a job, create a new model call or alter scheduling, context, permissions, user history, session ownership or token budget.

### Live source and selection

- Reuse admitted and sanitized `ChildStreamBlock`, `AgentMonitor` trace/preview, phase and tool events. At most one selected live agent is shown in mini-live at any time; the source is keyed by trusted `(session_id, agent_id/run_id)` and scoped to the active UIA session.
- Default target: current active root-orchestrator if available; user selection in **Active Agents** pins an allowed worker/child when available. When a selected run finishes, show an honest terminal summary briefly and then return to a running relevant agent; never claim another agent's stream is that run's output.
- Only provider-exposed, policy-permitted, already displayable reasoning summaries or deltas are shown. No extraction or synthesis of private hidden chain-of-thought from provider internals. If reasoning is absent or forbidden, display actual tool/status/assistant observations. Label `thinking` as **phase**, not proof that text reasoning exists.
- Preserve original event order and redaction (including secret/path sanitization). Do not duplicate raw tool args, expose untrusted child private transcript or elevate lower-trust agent text to an instruction.
- Tools: short compact `name · state/duration` with error state, never a full JSON payload. Reasoning preview: capped escaped/redacted tail. Dedupe by stable event/stream ID, coalesce frequent token deltas to avoid terminal repaint storms; empty data shows `no visible events yet`, not invented narration.
- Failure and completed aggregate counts remain sourced from AgentMonitor + JobManager; a job-backed child must not be double-counted. The mini-live pane must not infer verification or Goal Achieved from agent prose.

### Reuse, no extra model or job

W02 does not create `ProgressAgent`, `ProgressProposalV1`, user-message sampling, automatic todo writes, model routing, additional context compilation, tools or background observer jobs. It is a **read-only Ratatui presentation projection** over existing agent events. It may share a bounded display view with W01 inline chat, but canonical conversation chronology and context must stay unchanged.

An optional future independent review/progress model would be a **separate opt-in proposal and ADR**, not an implicit dependency of W02.

## DELTA — suggested code ownership and rollout

| Wave | Single code owner | Work | Gate |
|---|---|---|---|
| U0 | `harw-tui-layout/src/{placement,dock}.rs` | Pure chat-first rectangles, separate Finished/Failed and mini-live region | Pure layout/unit tests |
| U1 | `harw-tui/src/{panes,app}.rs` + limited renderer | Render 3 cards + 3–4-line sanitized mini-live; reuse source identity, focus and resize | Snapshot tests, no producer/runtime modifications |
| U2 | `harw-tui/src/{agent_monitor,child_stream}.rs` adapters only if needed | Expose bounded safe preview of selected run; do not clone event pipeline or its storage | Producer/consumer tests and redaction |
| U3 | `harw-tui/src/app/scroll_routing.rs` + keybindings | Deliberate expansion/selection, independent Chat scroll, no stolen composer focus | Wheel/key/keyboard regression |
| U4 | real portrait/landscape runtime manual verification | Device terminal 46×78 plus 48×65, 70×90, 80×40, 120×42, 160×45, tiny/zero-size; busy root and child | Evidence log, screenshots, no invented green CI |

No shared-file parallel writes, no changes to PL-90 PR #136 code through this doc-only PR. Cheap `cargo fmt`/`cargo metadata` during implementation waves as requested; centralized exact-SHA focused/CI verification separately before merge. No claims that any of those runs already passed.

## MUST-PASS acceptance checks (NOT RUN)

1. At **46×78** (reference screenshot), three status regions and a mini-live header with up to 2 concise event lines appear; **main Chat remains the dominant, full-width region** with working composer and approval overlays.
2. For **48×65**, **70×90**, **39×60**, **48×19**, **80×40**, **120×42**, **160×45**, **1×1**, **0×0**: no overlap/panic/out-of-bounds; compact/tiny cases collapse live/status detail first.
3. Streaming test: root emits admitted reasoning delta, `ToolCall`, `ToolResult`, assistant response; the last relevant, sanitized two lines update in place. A provider with **no visible reasoning** shows truthful tool/status output without generating text.
4. Switching among root and worker changes only UI selection; agent/model/job run IDs, job scheduling, authorization, token usage and pending approvals remain untouched.
5. Another session's or sibling's private child stream **cannot** appear in the selected mini-live view; disabled `/agent stream` and remote nonstreaming sources receive honest fallback, not illegal reconstruction.
6. Count one job-backed child **once** in terminal aggregates; failures remain visible despite `delegate_wave.satisfied = true`.
7. No new LLM request, token charge, `ProgressAgent` or `JobManager` job arises from repaints or resizing; GUI rendering remains read-only.
8. W01 inline chat, Ctrl+O, `/agent stream`, history export and scrolling remain correct; no duplicated canonical transcript events.
9. Live clip limits and terminal widths use Unicode/ANSI-safe sanitization; no content-dependent out-of-control layout jumping.
10. Tests and any screenshot produced by future implementers are attached to the **actual implementation SHA**; this planning document itself is not a completed feature.

## Resolution of competing prior PL-94 proposals

The initial Oct 8 PL-94 README/W00 advocated an independently model-backed Progress Agent and a portrait progress bar. The **Oct 9 user-approved TUI correction takes precedence for the primary layout**: use existing observed active agent, compact stream+tool trace and a large chat. No model summary, synthetic todo, hidden reasoning, or second chat in portrait. Keep the original plan files as historical evidence until their owners explicitly migrate the wording; the parent README must prominently link this override so implementers do not build contradictory versions.

**IMPLEMENTATION STATUS: DESIGN ONLY.**
