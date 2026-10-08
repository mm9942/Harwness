---
id: PL-94-ADAPTIVE-TUI-MISSION-CONTROL
title: "Adaptive TUI Mission Control — three pinned status cards and an isolated Progress Agent"
status: proposed
date: 2026-10-08
tags: [tui, responsive, portrait, landscape, agent-monitor, jobs, plans, progress]
related:
  - ../68-mobile-tui/README.md
  - ../65-cloud-sessions/README.md
  - ../90-intent-driven-composed-agents/README.md
  - ../../design/interaction-contract.md
  - contracts/W00-layout-progress-contract.md
---

# PL-94 — Adaptive TUI Mission Control

> PLANNING ONLY, not production implementation.
> Code baseline: mm9942/Harwness dev@197a92e92d438bf6bf93b129bb964f4852686149 (2026-10-08).
> Concurrent work: PL-90 PR #135 and implementation PR #136 (Harw-owned). This new, separate branch modifies only PL-94 planning files. Harw retains ownership of PR #136 and its runtime implementation.

## 0. Product intent and critical invariants

The TUI should provide four *distinct projections* without requiring the operator to leave the chat:

1. **Active agents**, always the first status card.
2. **Active jobs**, always the second status card.
3. **Finished and failed agents/jobs**, the third **independently bordered** status window, below the first two in portrait and **top-center in landscape**.
4. **Work in Progress** is backed by a separate bounded Progress Agent: **landscape** displays its own expanded live WIP window; **portrait displays only a compact derived progress strip**. The agent still runs in the background in portrait; its model-generated narration and separate output are hidden by default.

All three status windows are always logically distinct, separately bordered, individually focusable and independently scrollable. Their pinned position follows the orientation; Chat scroll never moves them, and Progress output scroll is independent. The composer, main agent conversation, approvals and job authority retain their existing owners.

**Not shared agent context:** the progress agent is an independent, read-only *session-scoped* observer. It must not receive hidden reasoning, complete child transcripts, other session messages, unrestricted Dream/Diary/global memory, model credentials or extra tool rights. A child/worker result is available only through a bounded authorized return/progress envelope.

## 1. CURRENT: source-verified baseline

| Component | What is implemented in the inspected tree | Gap relative to PL-94 |
| --- | --- | --- |
| harw-tui-layout/src/placement.rs | Pure Placement classifier: WideSide when cols >= 100; PortraitDock when cols >= 40 and rows >= 28; otherwise Summary/Compact. Portrait dock is 1/3 of body, clamped to 8..15. | Not driven by x-vs-y orientation; no separate 3-card cluster and progress rectangle. |
| harw-tui-layout/src/dock.rs | Top dock splits into two equal agent/job columns at width >= 68; below that it combines both into one list. | No third card or WIP output. |
| harw-tui/src/panes.rs / app.rs | Geometry mapping, existing wide right column and portrait top dock, status/composer reservations, panel focus/maximize. | No independent Progress pane/focus or 3-card geometry. |
| harw-tui/src/agent_monitor.rs | AgentMonitor projects live phases, tool/status preview, context budget, failures and finished agents. | Reuse its data; do not introduce a second agent store. |
| harw-tui/src/jobs_panel.rs / app/jobs_glue.rs | JobRow from JobManager and event/periodic refresh; completed and failed jobs already have classifications/reasons. | A dedicated completed/failed projection should combine these with agents without duplicating source truth. |
| harw-tui/src/app/scroll_routing.rs | FrameRegions and mouse/keyboard scroll routing distinguish agent dock vs transcript, plus modal precedence. | Expand to multiple independent rects and Progress focus. |
| harw-plan/src/types.rs, actions.rs, store.rs | Typed PlanNode statuses Draft/Ready/InProgress/Blocked/Completed/Superseded/Invalidated; mutations via PlanStore::apply, validated with evidence. | New agent suggestions must not bypass these transitions, invent completion or create an ungoverned duplicate todo store. |
| harw-tui/src/app/goal_marker_glue.rs | TUI polls actual PlanStore/GoalStore and updates goal marker and system lines. | Use as evidence/status input for Progress rather than replacing it. |
| harw-tui/src/app/btw.rs | Tool-free one-shot side-model for optional question; its snapshot can include a broad session context. | Not suitable unmodified: the Progress Agent needs a much narrower three-user-message + current-turn + typed-progress context. |
| harw-core/src/agent_events.rs | Live event hub for presentation; hub delivery is not authoritative committed history. | Durable resume/reconciliation needs session transcript / job / plan stores as source of truth. |

PL-68 already shipped an initial PortraitDock; PL-94 is a **functional extension and aspect-aware redesign**, not a claim that the existing top dock is missing.

## 2. TARGET: adaptive layout based on terminal x/y

### 2.1 Orientation is derived from geometry, never an OS or device name

Use x = terminal columns; y = terminal rows. Compute the orientation after reading terminal dimensions:

- x > y -> landscape preference.
- y > x -> portrait preference.
- x == y -> balanced; choose the feasible layout without discontinuity.
- **Space checks outrank orientation preference.** Cells are not square pixels, so a 90x50 terminal may still lack width for a legible chat+rail. Use available grid dimensions and minimum usable rectangles; do not pretend x>y alone guarantees a side rail.
- Do not identify Android, Termius, tmux, SSH, desktop, font size or hardware model.
- Keep current Summary/Compact behavior when no layout can display all four panes legibly. The three status windows remain independently addressable by title/tab and fullscreen detail, not merged into a shared monitoring panel or zero-height clipped cards.

Suggested pure types, final naming Harw-owned:

    OrientationHint = Portrait | Landscape | Balanced
    MissionPlacement = PortraitStack | LandscapeThreeColumns | ShallowLandscape | CompactSummary
    StatusRects = {agents, jobs, completed_failed}
    MissionRects = {status, wip, chat, footer}

### 2.2 Portrait — three status windows, a progress-only strip and full-width chat

**User clarification (2026-10-08):** The Progress Agent continues in the background in portrait, but the TUI should show **only its derived progress** — not a second agent chat, a streamed model answer, a verbose narrative, an expanded todo panel or a second live-output viewport.

    +------------------------------------------------+
    | ACTIVE AGENTS         | ACTIVE JOBS             |
    | root: thinking       | job-42: verify 65%       |
    | child: testing       | job-43: running          |
    +------------------------------------------------+
    | FINISHED 3    FAILED 1  (details on demand)    |
    +------------------------------------------------+
    | PROGRESS   verified 2/4   [######------]      |
    | Current task: isolate guards · running         |
    +------------------------------------------------+
    | MAIN CHAT / ACTIVE ROOT OUTPUT                 |
    | conversation, tool events and input history     |
    |                                                |
    |                                                |
    +------------------------------------------------+
    | status, approvals, input/composer               |
    +------------------------------------------------+

**Portrait progress strip:** a compact read-only, non-scrollable status projection, normally 2–3 rows, up to 4 only if a blocker/stale warning needs space. Render only the current task/phase, verified-completed vs total task counts, an optional progress bar (only with a trustworthy denominator), and critical blocked/stale state. When progress cannot be quantified, show an indeterminate marker rather than inventing a percentage. The main chat gets the remaining height.

The separate Progress Agent still retains its **own session/job/budget, event cursor and narrowly scoped inputs**. It may update structured progress/todo proposals on meaningful events, but the portrait renderer never displays its narrative or transcript by default. An explicitly selected detail view is optional and not part of the always-on portrait layout. All three existing status categories remain as distinct bordered windows with independent selection/scroll.

**Rotation preserves execution:** portrait/landscape switching changes projection only. Do not cancel, restart, reparent, share context or alter the background observer's tool/permission ceiling; moving to landscape exposes the larger WIP window, while returning to portrait collapses only presentation.

### 2.3 Landscape — three columns with five independent windows

**User-approved placement (2026-10-08):** Chat is the left column. **FINISHED / FAILED (Status) is top-center**, with the larger **WORK IN PROGRESS** window directly beneath it. The **far-right edge** holds **ACTIVE AGENTS** above **ACTIVE JOBS**. Each labeled item is a *separate* TUI window, not merely a subsection of a single large monitoring panel.

    +--------------------------------+---------------------------+--------------------+
    |                                | FINISHED / FAILED         | ACTIVE AGENTS      |
    |                                | done 3 / failed 1         | root: thinking     |
    |                                +---------------------------+ child: testing     |
    | MAIN CHAT / ACTIVE TURN        |                           +--------------------+
    |                                | WORK IN PROGRESS          | ACTIVE JOBS        |
    | independent transcript         | current user intent       | build: running     |
    | visible assistant/tool output  | [x] verified task         | test: pending      |
    | and conversation               | [>] active task           |                    |
    |                                | [ ] next task             |                    |
    |                                | progress agent narration  |                    |
    |                                | evidence / next step      |                    |
    +--------------------------------+---------------------------+--------------------+
    | status / goal / approvals / composer (pinned footer)                      |
    +-------------------------------------------------------------------------+

The word **status window** means FINISHED / FAILED, which is **not** the terminal's bottom status line. All five independently bordered windows have their own rectangle, focus state, scroll position, selection, title and optional maximize/detail view. Status data still comes from the existing AgentMonitor and JobManager: separate visual windows do not imply separate lifecycle stores.

- **Left — Chat:** full available body height, separate history scrolling and input focus, never squeezed below a tested chat minimum.
- **Center top — Finished/Failed:** normally 4–8 rows, newest failures with reasons plus finished aggregates/details; independently scrolled.
- **Center below — Work in Progress:** the larger remaining center area for the dedicated session-bounded Progress Agent and verified todo/progress projection.
- **Outer right top — Active Agents:** its own agent list/window, including phase and running task; never placed inside the Job window.
- **Outer right below — Active Jobs:** its own jobs list/window, including progress and runtime; split right column vertically with enough rows for both.
- **Bottom — status/composer:** remains usable; approval overlays always take precedence.

Landscape is selected by x>y **as a preference**, then tested for available cell width/height. Provisional comfortable bounds: Chat >= 64 cols, Center >= 38 cols, OuterRight >= 30 cols, plus gutters; a denser tested three-column mode may use Chat >= 48, Center >= 34, OuterRight >= 26. Required vertical feasibility: independent Agents/Jobs windows >= 5 rows each, center Finished/Failed >= 3 rows, center WIP >= 7 rows. Treat these as initial test targets, not permanent hardcoded constants.

If x>y but there is not enough space, **ShallowLandscape still preserves separate window identities**. Prefer readable Chat plus tiled/individually switchable status windows and an expandable WIP view. Never silently merge all monitoring rows into one combined status canvas; when dimensions are tiny, only one independently titled panel may be expanded at a time, but the three categories remain navigable as distinct tabs/counters. A 0x0 terminal cannot render five boxes simultaneously.

Portrait retains separate Agents/Jobs windows side by side and Finished/Failed below, then shows only a **compact progress strip** above full-width Chat. Expanded Progress Agent narrative belongs to landscape or an explicitly opened detail view. Rotation must not restart, cancel, share or reparent agent runs. Explorer/Workbench/F11 and modal approvals keep their existing precedence and ownership.

### 2.4 Allocation and degradation order

1. Reserve status line, composer, approval overlays and any immutable prompt affordances.
2. Reserve minimum chat height/width and available space for focused detail.
3. Allocate three independently bordered status windows at the layout-specific anchors: portrait (Agents/Jobs row, Finished/Failed below) or landscape (Finished/Failed center-top; Agents/Jobs stacked at outer right).
4. Allocate the **expanded, independently scrollable WIP** below Finished/Failed in landscape. In portrait allocate **only a 2–3-row progress-only strip**, not a second output or scrolling WIP pane.
5. In constrained terminals, shrink WIP first, then show compact independently titled status tiles or explicit tabs; never merge three windows into one undifferentiated monitoring canvas. If all cannot fit at once, allow one at a time with distinct focus/identity. Never clip composer or hide a blocking approval.
6. When no progress agent/provider is available, the WIP pane uses deterministic typed status; it never disappears into an empty spinner.

The goal is a predictable responsive projection, not a per-region font-size change (terminals have one cell font setting). All widths/heights are finite and pure functions of geometry and explicitly registered visibility/focus.

## 3. TARGET: dedicated read-only Progress Agent

The **Progress Agent is not the main assistant**, not a substitute root orchestrator, not the job manager and not a global memory consumer. It is a separate, bounded, optional model-backed observer session behind the existing Harw job/agent infrastructure. It yields a model-written second output and session-local todo proposal projection.

### 3.1 Minimum exact input contract

The agent sees ONLY:

1. **The last three committed user-authored messages from the current authenticated session**, in order, with source IDs, immutable sequence and timestamps. If fewer exist, supply fewer. Never replace them with three assistant messages or with messages from parent/sibling agent sessions.
2. **The current active main-agent turn** as a bounded, public/renderable projection: active turn ID, latest user-intent binding, current phase/task, redacted visible assistant response tail (if needed) and timestamps. No full model reasoning, raw tool arguments, private chain-of-thought, hidden provider prompts or arbitrary inherited context.
3. **Typed current progress/status**, from the admitted AgentMonitor/JobManager/PlanStore/GoalStore scoped to that same session/work intent, including accepted plan revision, task references, verified evidence, failures, active jobs and phase changes. A job without a trusted relation to the current session is not included.

All input identity and visibility is bound by the trusted runtime, **never populated by untrusted model text**. A Progress Agent cannot inspect another session, turn or tenant via self-chosen IDs. Tooling can be absent: the agent does not need arbitrary fs/shell/web access. The separate model call never mutates the main agent's history, memory or private state.

### 3.2 Output and execution separation

The cheap, deterministic stage keeps a session-scoped progress projection **without an LLM**. On committed user correction, accepted plan change, job transition, child return or independent verification outcome, it updates a revisioned signal snapshot and may enqueue a bounded model-backed summarization/update job. Avoid launching an LLM on every token, timer tick or status repaint; coalesce events, deduplicate by source sequence and cancel obsolete output.

The Progress Agent produces a **proposal**, not facts:

    ProgressProposalV1 {
      source_session, source_turn, observed_watermark, model_job_id,
      active_intent_ref, summary_text, next_step_text,
      suggested_todos: [{id_hint, title, dependency_refs, kind, evidence_refs}],
      proposed_status_changes: [{task_ref, status, evidence_refs, rationale}],
      unresolved_questions, confidence_label, stale_after?
    }

The runtime reconciles this against trusted current stores, checks provenance and version, rejects stale proposals, then renders a sanitized, bounded output. It must not call PlanStore::apply directly from generated text, mark a task Completed from a model assertion, activate a new role/tool, or auto-accept a Goal. It may create **session-local draft Todo suggestions** automatically; promotion into a persisted executable plan is through existing PlanAction/PlanStore validation and applicable approvals. A finished job is not automatically a verified accepted goal.

If the model fails, is unavailable, over budget or the worker is down, show deterministic event-backed WIP with a visible stale/deferred marker. Status rendering must remain nonblocking.

### 3.3 Desired UX

- **Landscape WIP:** scoped goal/intent, current plan revision, last durable update time and source. **Portrait strip:** verified progress count/optional bar, current task/phase and essential blocked/stale state only.
- At most 4–6 visible active todos by default; expandable list includes blocked/recently finished.
- Each todo distinguishes Suggested, Ready, Running, Blocked, Verified Done, Superseded/Invalidated. Verified Done only with real PlanStore evidence or equivalent admitted verifier.
- Concise model-generated narration may appear in the **expanded landscape WIP** or an explicitly opened detail view. **Never show the agent's prose by default in portrait.**
- Optional selection of a todo shows its source user message, related Job/Agent, file/PR evidence and verification state, subject to existing visibility.
- The expanded **landscape WIP** scroll is isolated from Chat/Agents/Jobs; the **portrait progress strip is non-scrollable** and cannot steal keyboard input from Chat. Explicit detail/maximize uses existing keybindings. Pausing the Progress Agent never pauses the main turn.
- Progress narration is a separate panel, never silently injected as a new user message or a new main-agent turn; it must not spam chat history.

### 3.4 Cost, reliability, and governance

- Start with deterministic projection from existing stores. Add optional model narration only after proving safe source classification and replay.
- Budget the observer separately (tokens, calls, cancellation, lease, time), with one bounded in-flight update per session.
- Prefer on-event refresh; use a modest debounce/cooldown after meaningful changes and no periodic LLM call while the state is unchanged.
- Associate each proposal with session ID, input digest, typed observation watermark and intent revision. Ignore stale responses after user correction, session change, cancellation or plan mutation.
- Durable cursor/high-water mark allows safe restart and replay from committed records. AgentEventHub is an optional UI wakeup, **not** a commit log or proof of completed work.
- No cross-project/global write, no background promotion of private data to Memory Palace, no direct GitHub write, no shell or tool approval bypass.
- Explicit opt-out/config with a deterministic local monitoring fallback; agent cost/locality policy must be visible to the operator.

## 4. DELTA: crate ownership (do not edit Harw-owned PR #136 files concurrently)

| Owner | Implementation direction | Boundary |
| --- | --- | --- |
| harw-tui-layout | Orientation hint, feasibility classifier, three status rectangles, Progress rect, bounded allocation and pure property tests | Geometry only; no Harw runtime, memory, provider or ratatui dependence |
| harw-tui::panes / app render | Convert rectangles; render Status cluster/WIP output; selection, clipping, focus/fullscreen, resize | Presentation only; do not create another JobManager/PlanStore |
| harw-tui::agent_monitor / jobs_panel | Active/completed/failed projections of existing AgentMonitor and JobRow, no cloned lifecycle state | Status derived from existing sources |
| harw-tui::app::scroll_routing | Generalize region routing for Agents, Jobs, History and WIP, modal-first | Input semantics; preserve existing F2/F3/F4/F5/F11/Esc |
| harw-runtime composition | Build session-scoped ProgressSignalCoordinator and admitted optional read-only model job | No privileged cross-session actor; no hooks into DoD TCB |
| harw-plan / harw-plan-bridge | Reuse current Goal/Plan/WorkDriver authority; validate todo proposals and verified transitions | Do not grant model text write/achieve authority |
| harw-core + session transcript adapter | Committed last-three-user-message selection and active public turn status by trusted identity | No broad history or sibling STM injection |

Existing PL-68/mobile layout and PL-65/remote TUI contracts remain references. Coordinate any WIP data feed with PL-90 session/message indexing and run-isolation work, but **do not block the pure layout wave on embeddings or global learning**.

## 5. Phased parallel development

- **M0 — contracts only (THIS PR):** pin source evidence, test matrix, portrait/landscape wireframes, security/authority contract, owner/file boundaries. No runtime behavior changed.
- **M1 — pure geometry:** new placement/rect classification with x/y orientation and feasibility constraints; keep the existing placement as compatibility fallback. Unit/property tests across zero, tiny, boundary and wide terminal sizes.
- **M2 — three status cards + compact portrait progress:** independently render status windows; add a read-only progress strip in portrait, not a second agent-output pane. Preserve Composer, Workbench, overlays, and maximize. Renderer/interaction tests.
- **M3 — deterministic WIP:** 3 user message refs, current turn, typed Plan/Goal/Job status; display event-backed todo/progress projection; replay/resume tests. No model call yet.
- **M4 — model-backed observer:** separate tool-free governed Progress Agent job, bounded input/output, dedup/stale result handling, costs/visibility, side-output styling. Test missing-model and stale-turn behavior.
- **M5 — Plan/Goal integration:** auto-generated *draft* todos, admitted promotion/update, evidence-bound completion and conflict handling; owner acceptance preserved.
- **M6 — reconciliation with Harw's PL-90:** connect fully scoped message index, child/job outcomes and optional existing knowledge feeds only after their isolation and authorization gates pass. Final real-phone/desktop QA.

Parallelism policy: new PL-94 branch and separate Draft PR, own worktrees and files. No code edits in Harw's #136 branch. Coordinate touched files before M1 and rebase on current dev after PL-90 merges; merge only with explicit human approval and verified gates.

## 6. Verification matrix (mandatory, not yet run)

| Screen x by y | Expected |
| --- | --- |
| 48x65 | PortraitStack: separate Agents/Jobs, Finished/Failed; **progress-only strip**; full-width Chat, no expanded agent output |
| 45x80 | PortraitStack with clipped narrow status cards and compact progress only; observer keeps running |
| 70x90 | PortraitStack with compact progress-only strip, no second output/narration |
| 60x60 | Balanced, chosen by feasible layout; deterministic |
| 80x40 | ShallowLandscape: compact independently titled status windows/tabs; no combined monitoring panel |
| 100x30 | ShallowLandscape, separate focusable status identities without overlapping; composer/chat usable |
| 120x42 | Dense 3-column landscape if feasible, or ShallowLandscape; center Status above WIP, right Agents above Jobs |
| 160x45 | Three columns: Chat left; center Status above WIP; Agents above Jobs at outer right |
| 200x50 | Full five independently bordered windows in three columns; Explorer/Workbench preserved |
| 39x60 | CompactSummary: all three categories reachable via labelled counts/detail |
| 48x19 | CompactSummary, never crowd out composer/status |
| 1x1 and 0x0 | No panic, no rectangle overflow or subtraction underflow |
| x/y crossing on live resize | Reclassify and redraw immediately, preserve focus/selection and running jobs |

Also test non-overlapping rects and **five independently bordered windows in landscape**; in portrait only **three status windows + compact progress strip + Chat, no expanded second-output window**. Verify landscape anchors (center Status/WIP, right Agents/Jobs), modal priority, Unicode cell width, no leaked raw reasoning, independent scrolling and keybindings. Rotating mid-turn must not spawn/cancel the progress observer.

**Observer tests:** exactly three most recent *user-authored* committed messages; correct session/turn/tenant; a background child with same trace never leaks private transcript; stale update is rejected; active agent turn advances; session resume rehydrates cursor; job failure is not called verified success; no raw agent reasoning; no side-effect tools; two concurrent sessions get independent WIP; missing provider uses deterministic fallback; no unbounded full-history re-scan.

**Todo tests:** automatic proposal stays draft until an admitted PlanStore operation; verified completion carries EvidenceRef; a newer user correction supersedes stale todos rather than letting the observer silently rewrite the owner's goal. WorkDriver and human-only goal achievement checks still hold.

## 7. Definition of done and implementation status

A real Harw TUI viewport in portrait or landscape distinguishes **active agents**, **active jobs** and **completed/failed**. **Landscape** additionally shows the expanded WIP/second-output window. **Portrait** keeps the Progress Agent running but renders **only a compact verified progress strip**, not a second agent output or narrative. Both stay accurate during active root turns and background jobs. The WIP agent may propose todos from the last three user messages/current turn/progress signals but cannot share other agents' private context or treat proposals as verified outcomes.

**Current status: M0 design only.** The audited existing PL-68 dock, goal marker, AgentMonitor, JobRow and /btw are implemented foundations. M1–M6 are PROPOSED until independently tested code and corresponding hashes are attached in this Draft PR. No Cargo, TUI render test or real-device test has run as a result of this document.

See [W00 contract](contracts/W00-layout-progress-contract.md) for acceptance invariants and schema details.
