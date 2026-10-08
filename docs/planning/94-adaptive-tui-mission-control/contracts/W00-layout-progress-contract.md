---
id: W00-PL94-ADAPTIVE-MISSION-CONTROL
title: "W00 Contract — three status cards, responsive mission rail and Progress Agent"
status: proposed
date: 2026-10-08
parent: ../README.md
---

# W00 — Adaptive Mission Control acceptance contract

> **CURRENT UX DIRECTION (2026-10-09):** [W02 chat-first mini-live](W02-chat-first-mini-live.md) supersedes W00's model-backed Progress Agent, progress-only strip and expanded synthetic WIP as the primary TUI target. This W00 is kept for historical rationale and shared status/geometry invariants. New implementation must follow W02 for the compact real agent reasoning/tool preview and Chat-first capacity, without a second model invocation. No code was implemented by this documentation change.

> **DESIGN ONLY**, rooted in dev@197a92e92d438bf6bf93b129bb964f4852686149. See the [parent design](../README.md). No renderer, agent or new runtime listener is claimed as implemented.

## 1. Exact UI state sources and authority

| View | Read source | What it may write |
| --- | --- | --- |
| Active agents | Existing AgentMonitor, AgentEventHub projection and persisted child status where needed | UI focus/selection only |
| Active jobs | Existing JobManager JobStatus/JobRow and events | UI focus/selection only |
| Finished / failed | Finished/failed projections of those same agents/jobs, with verified lifecycle and failure reason | Mark UI acknowledgement; do not alter job outcome |
| Work in Progress | Authenticated session-specific last three committed user messages, current main turn's *public* observable state, PlanStore/GoalStore status and linked job/agent progress | Sanitized display; optionally bounded draft Todo **proposals** to the existing plan admission flow |
| Progress Agent | Isolated, least-privilege observer, optional model-generated compact progress narrative | New proposal document/turn-bounded output only; never parent/child history, authority or executable plan mutation |

**Status must never be inferred from free-form assistant output:** a claim that a test passed is still a claim until supported by an actual verified job/evidence result. WorkDriver/Goal owner acceptance is not delegated to the Progress Agent.

### 1.1 Existing unblocked orchestrator behavior — preserve it

**CURRENT at dev@197a92e (not a proposed PL-94 feature):** `harw-tui/src/app/background_agents.rs::BackgroundLauncher::wants_background` defaults TUI-root orchestrator handoffs to background execution unless the request explicitly uses `background: false`. Root/UIA chatting can proceed while that admitted child runs; results/notices use the current parent delivery queue. Existing tests cover detached launch and notice delivery. Do **not** add a second nonblocking executor, change this default or use window navigation to restart an agent.

**Limits of the existing contract:** background-by-default applies to TUI-root **orchestrator** targets, not all worker/tool calls. `/new`, `/resume` and TUI shutdown currently cancel background children; the external `/plan edit` path is not busy-safe. PL-94 must not claim these other behaviors work without separate code changes and tests.

**PL-94 enhancement (revised operator intent):** the primary interaction is an **attached, live, dynamically repositioned Root/child agent message directly in the main Chat**, *not* navigating to a separate agent session. [W01 — activity-ranked inline chat](W01-activity-ranked-inline-chat.md) defines stable visual slots counted from the composer: root normally #2 while idle, behind the active foreground UIA exchange (typically #3) during a new turn, #2 again after the final reply. With multiple background runs, prioritize their **display** via trustworthy meaningful activity, recency and optional manual pin; do not change scheduling, contexts or history. Existing AgentTree/scroll can still open details. Every focus/visual move preserves running jobs, the UIA composer draft, session IDs, canonical chronological history, PlanMode and permissions. No private child transcript may be implicitly attached.

## 2. Responsive rect contract

- Compute from terminal cells `x=columns, y=rows`, not OS or model. `x>y` prefers landscape; `y>x` prefers portrait; `x==y` has a deterministic feasibility tie breaker.
- A three-column landscape composition is feasible only after separately budgeting Chat, the **middle Status/WIP column** and the **outer-right Agents/Jobs column**, plus gutters and footer. Provisional targets: Chat >= 64 columns, Middle >= 38, Right >= 30 (a tested dense profile may be smaller); otherwise choose **ShallowLandscape** with distinct switchable windows. Never detect orientation by device identity.
- In **PortraitStack**, Agents and Jobs sit in the pinned first row (two logical cards), **Finished/Failed** immediately underneath, then **only a compact, non-scrollable 2–3-row progress strip** and the main Chat. The Progress Agent still runs headlessly in portrait; no second output window, narrative, streaming text or expanded todo pane is shown by default. Composer/status remain at bottom.
- In **LandscapeThreeColumns**, **Chat** is left, **Finished/Failed (Status)** is a separately bordered window at the **top-center**, **WIP** is the larger separate window directly below, and independently bordered **Active Agents** (top) and **Active Jobs** (bottom) are stacked at the **outer-right edge**. Never merge them into one oversized right-hand monitoring window.
- In constrained `Summary`/compact mode, preserve **three individually addressable titled status window identities** as compact focusable counters/tabs with a detail switcher, plus a distinct WIP identity, even if all cannot render simultaneously. No blended status list, no zero-height rectangles.
- Minimum chat, modal approvals and footer win capacity competition. Compress WIP first and status detail second; do not clip approval prompts. Each visible window has its own border, rectangle, scroll position, focus/selection and maximize/detail. The top-center 'Status' is Finished/Failed, not the bottom terminal status line.
- Pure classifier; saturating arithmetic; no overlaps, out-of-bounds rectangles or unchecked conversion. A layout flip on resize does not modify agent/job execution, cancellation, history, plan or user input.
- Preserve current F2/F3/F4/F5/F11/Esc semantics; WIP focus must not steal keystrokes from composer. Hit test by rectangles, not assumptions that AgentMonitor is always on the right.

### 2.1 Portrait background observer / progress-only UI

**Operator requirement (2026-10-08):** The observer continues to run in the background in portrait, with the same isolated session, progress-event cursor and bounded budget. The TUI renders ONLY concise progress fields (verified completed/total count when known, optional bar, current task/phase, critical blocked/stale indication). It does not render the observer's generated text, reasoning, private transcript, streamed tokens or a second agent-output pane. If a denominator is not evidence-backed, show an indeterminate progress indicator rather than inventing a percentage. Expanded WIP is a landscape/default-detail presentation only. Orientation changes alter rendering, not job lifecycle, agent rights or state.

**Acceptance:** inject a multi-paragraph Progress Agent output in a portrait test; verify the agent/progress record updates but that rendered portrait includes only the derived compact status, never the generated prose. Assert no restart on portrait/landscape rotation and a useful fallback with the provider disabled.

## 3. Observer input/output contract

Trusted input projection only:

    ProgressInputV1 {
      principal: runtime-bound tenant/workspace/session/run,
      user_messages: last 0..3 committed user-authored messages IN THIS session,
      current_turn: {turn_id, user_intent_revision, phase, current_public_task, safe_output_tail?},
      progress: {plan_revision?, nodes?, agent_statuses?, job_statuses?, evidence_refs?, blockers?},
      watermark: committed source event sequence,
      input_digest: canonical hash over admitted projection
    }

Model output is an **untrusted proposal**:

    ProgressProposalV1 {
      source_session, source_turn, source_watermark, input_digest,
      narrative, next_step,
      todo_drafts: [{title, kind?, dependency_refs?, evidence_refs?}],
      proposed_status_changes: [{plan_node_ref, status, evidence_refs, reason}],
      uncertainties, optional_citations
    }

**Hard authorization:** input principal is set by runtime; a model cannot choose the session, project, agent, worktree, source index, tool grants or read scope. A child/sibling result appears only through its already admissible bounded return envelope. Never include private reasoning, raw prompts, raw tool args, unrelated user messages or secret material. Model narration and proposals are escaped/redacted, output capped and are never executable commands.

**Write and accuracy rules:** Progress Agent is read-only and tool-free in the MVP. The deterministic coordinator may hold session-local draft cards but may not mark a PlanNode Completed or Goal Achieved. Actual persisted changes require the existing validated PlanStore actions and approvals. `Completed` carries verified EvidenceRef; `Failed` and `Blocked` are never disguised as “almost done”.

**Lifecycle:** event-triggered coalesced updates after trusted commits, plan changes, job progress or child return. Do not trigger an LLM on every rendered frame, output token, tool heartbeat or polling interval. One bounded pending model update/session; reject stale output after new user correction, plan revision or session move. If no provider, render deterministic status with explicit deferred/stale labels. Preserve a cursor/high-water mark for restart; lossy AgentEventHub notifications are only wakeups.

### 3.1 Activity-ranked inline chat contract

The separate [W01 contract](W01-activity-ranked-inline-chat.md) is mandatory for the main Chat: stable per-agent visual handles; foreground UIA/user answer always ahead of background; multiple background runs sorted by meaningful observed activity with dwell/hysteresis, not by token spam; no duplicate or rearranged canonical transcript cells; identity-preserving scrollback and after-the-fact result retrieval. This is presentation priority, **never** execution or authority priority. The chat remains the largest viewport in portrait AND landscape. A paused/quiet background run remains reachable even if another run is temporarily ranked higher.

## 4. First-pass test gates

### Layout
1. Geometry at `48x65`, `70x90`, `80x40`, `100x30`, `120x42`, `160x45`, `200x50`, `39x60`, `48x19`, `1x1` and `0x0`; x/y tie and crossing on resize. If full layout is feasible assert five individual window rectangles and exact anchor placement.
2. Rectangles non-overlapping and within viewport; status/composer modal reserve, focus, F2/F3/F4/F5/F11/Esc, Unicode width, modal priority and independent wheel routing unchanged.
3. Three distinct status views share source truth with existing monitor and JobRows; no duplicate lifecycle. In landscape assert Finished/Failed **top-center**, expanded WIP **center below** and Agents/Jobs **separately stacked far-right**. In portrait assert a compact progress-only strip under the three separate status windows, with no separate observer-output pane; the main Chat remains full width. Focus, borders, selection and scroll belong to each window; finished/failed reason visible.
4. No unbounded rendering or excessive main-chat area loss.

### Inline chat ranking

- At least three concurrently running agents with disjoint session IDs: the highest legitimate activity moves to the foreground BACKGROUND slot while all agents keep executing with unchanged job scheduling.
- During an active UIA user/assistant turn, background items move behind that pair and return closer after the assistant finalizes. No duplicate messages or export pollution.
- Token spam/ReasoningDelta cannot steal rank from real milestones, unread operator questions or tool outcomes. Hysteresis prevents flicker.
- Manual scroll/focus stays anchored to the same cell when ranking changes; manual pin is respected; no private sibling contents displayed.

### Progress
5. Exactly three newest committed **user** messages or fewer; no sibling/other session content; latest correction supersedes stale output.
6. Untrusted model output cannot mutate PlanStore, JobManager, active agent turns, permissions or Goal achieved state.
7. Job/agent failure remains visible even when a model writes a successful-sounding summary; verified evidence wins.
8. Agent failure, provider outage or disabled feature leaves deterministic **progress** visible, not an infinite spinner: a compact verified count/current phase in portrait, expanded stale/deferred WIP in landscape.
9. Same session resume restores cursor and chosen plan; switching session cannot reuse stale observer output.
10. Multiple concurrent sessions keep separate observer states, jobs, budgets and output; no hidden reasoning leakage.
11. Runtime output/text remains capped and redacted; all user/agent/tool content treated as lower-trust data.
12. No new hot-path full-history scan, unconditional model call or accidental restart of main worker on resize.

## 5. Wave owner boundaries

- M1: `harw-tui-layout` pure classifier, geometry tests.
- M2: `harw-tui` independent status windows, portrait progress-only strip, focus/scroll, renderer tests.
- M3: runtime-to-TUI typed progress projection without an LLM.
- M4: optional separated Progress Agent job with strict minimal context; portrait/landscape switching must never stop, reparent or restart its background run.
- M5: validator for model todo drafts using existing PlanStore; retain human-only acceptance.
- M6: reconcile PL-90 session index and job progress without sharing agent context.

This W00 should not be used to justify touching Harw-owned code in PR #136 or moving runtime policy into `harw-tui-layout`. Record actual implementation SHAs and tests before any item is relabeled COMPLETE.