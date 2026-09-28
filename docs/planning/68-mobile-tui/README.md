---
id: PL-68-MOBILE-TUI
title: "Mobile / portrait TUI — pinned top agent dock"
status: proposed
date: 2026-09-27
tags: [planning, tui, mobile, portrait, agents, jobs, responsive]
related:
  - ../../design/interaction-contract.md
  - ../../design/tui-command-contract.md
  - ../65-cloud-sessions/README.md
  - contracts/W00-phone-top-dock.md
---

# Mobile / portrait TUI — pinned top agent dock

> **Pinned CURRENT:** `dev@79b471d9b64544714d74306b3356711daf8c0f7a`.
>
> **User-observed baseline:** current Harw running on a phone in portrait mode
> over a terminal client on 2026-09-27. The observed layout matches the current
> narrow-terminal behavior in source: the full right agent panel is removed
> below 100 columns and reduced to the one-line `Agenten: …` summary near the
> bottom.
>
> This is a planning document. No implementation is claimed.

---

## 1. Goal

On a phone-sized **narrow but tall** terminal, keep the live agent/job state
visible without sacrificing the width of the conversation.

Target shape:

```text
┌──────────────────────────────────────────┐
│ AGENTS / JOBS — pinned, about 1/3 high  │
│ active agents     │ active/background jobs
│ failures/finished │ progress / elapsed
├──────────────────────────────────────────┤
│                                          │
│ CHAT / WORK AREA — about 2/3             │
│ transcript, tool calls, reasoning        │
│                                          │
│ status / goal / host mode                │
│ composer                                 │
└──────────────────────────────────────────┘
```

The top area is **pinned**: scrolling the transcript never scrolls it away.

Desktop behavior remains a right-side panel.

Tiny panes remain compact instead of forcing a dashboard into too little
space.

---

# 2. CURRENT

## 2.1 Responsive behavior already exists

`harw-tui/src/panes.rs` currently defines:

```rust
MIN_CHAT_WIDTH = 48
AGENTS_PANEL_MIN_TERMINAL_WIDTH = 100
AGENTS_SUMMARY_MIN_HEIGHT = 16
AGENTS_WIDTH = 44
```

Current behavior:

- `width >= 100`: agent/workbench right column appears;
- `width < 100 && height >= 16`: the right panel disappears and Harw emits
  the one-line agent summary;
- shorter terminals omit even that summary;
- maximized panel mode remains independent.

The tests in `app/scroll_routing.rs` explicitly verify the full-panel vs.
summary behavior.

That explains the current phone screenshots: the terminal is narrow enough to
lose the 44-column right panel, so the main transcript keeps full width and
`Agenten: ● …` is rendered above the lower status/composer region.

## 2.2 Agent state already contains jobs

`harw-tui/src/agent_monitor.rs` is not only an agent list.

Its panel rows already include:
- live/waiting/failed/finished agents;
- context gauges;
- active jobs;
- finished-job grouping;
- job runtime/progress state;
- optional live preview/detail state.

Therefore the phone dashboard must be a **new projection of existing
`AgentMonitor` state**, not a second monitor/store.

## 2.3 Input and scroll routing already know the agent rectangle

`app/scroll_routing.rs::FrameRegions` records the visible agent panel area.
Mouse-wheel/panel-scroll routing uses that rectangle and otherwise leaves chat
scrolling alone.

A top-docked panel can reuse this contract if `last_regions.agents` is updated
with the top rectangle.

## 2.4 The status line already degrades by priority

`status_line.rs::fit_segments` drops low-priority details before core
mode/approval/model information.

The new layout must not replace that logic. The phone dock is additional
monitoring space, not a new status-line authority.

## 2.5 Existing future phone plan is narrower than this request

PL-65 §5.4 currently plans a future `compact_layout.rs` for
`cols < 60 || rows < 20`:

- single pane;
- agents/jobs behind Tab;
- collapsed reasoning/tool summaries;
- full-screen approvals.

That is compatible with this proposal.

This plan adds an **intermediate portrait class** for terminals that are too
narrow for a side panel but tall enough to afford a pinned top dashboard.

---

# 3. TARGET

## 3.1 Four geometry classes

Do not detect Android, Termux, Termius, SSH, browser, or a device model.

Detect only terminal geometry.

```rust
enum TuiLayoutClass {
    WideSide,
    PortraitDock,
    NarrowSummary,
    Compact,
}
```

Proposed auto classification:

### `WideSide`

```text
cols >= 100
```

Keep today's right-side agent/workbench layout unchanged.

### `PortraitDock`

Initial tuning target:

```text
60 <= cols < 100
rows >= 28
agents_visible == true
```

Render agents/jobs as a top dock.

The exact threshold is an implementation constant to tune with renderer tests;
the important invariant is **narrow + sufficiently tall**, not "phone".

### `NarrowSummary`

For narrow terminals that do not have enough vertical room for a useful dock,
keep today's one-line summary.

Example target region:

```text
60 <= cols < 100
20 <= rows < 28
```

### `Compact`

PL-65 remains authoritative for the future thin-client compact profile:

```text
cols < 60 || rows < 20
```

Until that profile lands, the existing small-terminal behavior stays
fail-soft.

## 3.2 Top dock height

The dock is approximately one third of the usable viewport, but must not make
the lower work area unusable.

Proposed function:

```text
preferred = body_height / 3
dock_height = clamp(preferred, 8, 15)
```

Then enforce a minimum lower work area before enabling the dock.

The one-third rule is therefore a **target proportion**, not an unconditional
percentage.

On a typical portrait terminal:
- 36 rows body -> 12-row dock / 24-row work area;
- 42 rows body -> 14-row dock / 28-row work area;
- 30 rows body -> 10-row dock / 20-row work area.

Composer/status rows remain pinned at the bottom and are counted before the
dock decision.

## 3.3 Internal top-dock layout

The user-approved visual direction is:

```text
┌ Agents ─────────────────┬ Jobs ────────────────┐
│ ● root-orchestrator     │ 1 build workspace   │
│   thinking · 29% ctx   │   82% · 18m12s      │
│ ● assistant            │ 2 gateway daemon    │
│   waiting              │   running · 2m18s   │
│ ✗ failures / ✓ done    │ ✓ recent finished   │
└─────────────────────────┴───────────────────────┘
```

For enough width (initial target: `cols >= 68`):
- split the dock into roughly 50/50 columns;
- left = agents;
- right = jobs.

For 60–67 columns:
- keep the top dock;
- use one compact combined list based on the existing panel row model rather
  than squeezing two unreadable columns.

No new state is introduced.

## 3.4 Content priority

Phone top dock is monitoring, not a duplicate log.

### Agents column, priority

1. active/root/background orchestrators;
2. failures needing attention;
3. current phase + elapsed;
4. compact context percentage/gauge;
5. aggregate finished count;
6. provider/model only when room remains.

Do not render full reasoning/tool previews in the pinned dock.

### Jobs column, priority

1. active jobs;
2. progress/state;
3. elapsed;
4. recent failure;
5. aggregate finished jobs.

Finished history is summarized, not expanded by default.

## 3.5 Focus and controls

Preserve existing panel semantics:

- F3 toggles agent visibility;
- F4 cycles Chat <-> Agents where applicable;
- Esc returns focus to Chat;
- F11 keeps existing panel maximize/detail behavior;
- Enter on an agent/job continues to open the existing detail path.

When the top dock has focus:
- selection marker is visible;
- scroll keys operate on the dock;
- normal letter input must not leak into the composer.

When Chat has focus:
- typing remains unchanged;
- transcript scroll remains unchanged.

## 3.6 Workbench and Explorer

W00 changes only the **agent/jobs** presentation.

Do not silently redesign Explorer or Workbench.

- Wide terminals keep today's side-panel composition.
- Narrow portrait mode does not place Workbench inside the agent top dock.
- A future mobile panel switcher can give Explorer/Workbench their own full
  screen or tab mode.
- Existing F11 maximize remains the escape hatch.

This keeps the first migration narrow.

---

# 4. "Bigger text below" — terminal constraint

The approved mockup has visually larger text in the lower work area.

A terminal TUI cannot portably assign a larger font size to only one rectangle.
Ratatui receives a grid of cells; glyph size belongs to the terminal client.

Therefore Harw must **not** fake a variable-font feature.

Harw can improve phone readability by:
- keeping the work area full width;
- reserving at most about one third for the dock;
- removing redundant metadata from the dock;
- collapsing long child/tool detail in the dashboard;
- keeping the composer wide and stable;
- avoiding extra decorative rows;
- later exposing a phone/comfortable density profile if needed.

Actual font enlargement is a terminal-client setting.

If a future terminal protocol offers a portable, safe variable-font primitive,
that is a separate design.

---

# 5. DELTA

## `harw-tui/src/panes.rs`

Current:
- horizontal side-panel splitter;
- narrow collapse to no panel.

Target:
- explicit layout classification;
- a vertical `PortraitDock` branch;
- `PaneAreas` extended or replaced with geometry that can represent an
  agent rectangle above Chat;
- wide path behavior preserved byte-for-byte where possible.

Recommended shape:

```rust
pub(crate) enum AgentPlacement {
    Side,
    TopDock,
    Summary,
    Hidden,
}

pub(crate) struct PaneAreas {
    pub explorer: Option<Rect>,
    pub chat: Option<Rect>,
    pub agents: Option<Rect>,
    pub workbench: Option<Rect>,
    pub agent_placement: AgentPlacement,
}
```

Do not infer placement from `agents.is_some()`; tests and input routing need
the semantic placement.

## `harw-tui/src/agent_monitor.rs`

Add a presentation helper for the dock; do not duplicate monitor state.

Potential functions:

```rust
render_agents_dock(...)
render_agent_column(...)
render_jobs_column(...)
```

Reuse:
- existing row labels;
- status colors;
- job rows;
- progress/context computation;
- selection/detail state.

The side panel remains its existing renderer.

## `harw-tui/src/app.rs` / render glue

Wire the top rectangle into the normal viewport composition.

Ordering invariant:

```text
overlay/dialog precedence
-> viewport body split
-> agent top dock
-> chat history/work area
-> status/goal/host-mode/composer
```

Open modal approvals still win over the dashboard.

## `app/scroll_routing.rs`

Use the top-dock agent rectangle in `FrameRegions.agents`.

Pointer/scroll behavior must follow geometry, not assume a right-hand x range.

## Tests

Expand existing renderer tests rather than create screenshot-only assertions.

---

# 6. Layout test matrix

At minimum:

| Geometry | Expected |
|---|---|
| 160x40 | current 44-col right agent panel |
| 120x30 | current right agent panel |
| 100x30 | right panel boundary still works |
| 99x40 | top dock, no right panel, chat full width below |
| 80x40 | top dock ~1/3 high |
| 72x36 | split agents/jobs top dock |
| 64x36 | top dock, compact/combined internal presentation if needed |
| 80x24 | one-line summary, no dock |
| 60x28 | phone dock minimum boundary |
| 59x40 | compact/single-pane path, not phone dock |
| 80x19 | compact/tiny path |
| 200x50 + Workbench | existing wide 60/40 right-column split unchanged |

Also verify:
- dock hidden when agents are toggled off;
- F3 restores it;
- F4 focus cycle reaches it;
- F11 maximize still fills the whole viewport;
- Esc returns to Chat;
- wheel over dock scrolls dock only;
- wheel over transcript scrolls transcript only;
- dialog/approval fully covers or correctly outranks dock;
- status/composer remain visible at every supported height;
- long agent names/job names clip rather than wrap unpredictably;
- Unicode display width remains correct.

---

# 7. Migration phases

## M1 — geometry only

- introduce layout-class/placement result;
- add renderer tests;
- preserve current wide and summary behavior;
- no visual dock renderer yet.

## M2 — top dock renderer

- render the existing AgentMonitor state into the top area;
- two columns where readable;
- combined compact projection at narrower widths;
- no new runtime/state model.

## M3 — input and focus

- scroll routing;
- focus marker;
- Enter detail;
- F3/F4/F11/Esc regression tests.

## M4 — phone density polish

- trim duplicate labels;
- prioritize errors/active work;
- verify real phone screenshots;
- tune thresholds only from renderer + real-device evidence.

## M5 — reconcile PL-65 compact client

When the remote/thin TUI compact layout lands:
- keep `PortraitDock` as the comfortable phone profile;
- keep `Compact` as the constrained/tiny profile;
- share classification/presentation helpers where possible;
- do not fork local-vs-remote responsive behavior.

---

# 8. Acceptance criteria

The first implementation is complete when:

1. A narrow/tall terminal no longer reduces the agent state to only the bottom
   summary; it renders a pinned top agent/jobs dock.
2. The dock uses roughly one third of usable height while preserving a useful
   lower work area.
3. Wide desktop layouts remain unchanged.
4. Tiny layouts remain summary/compact rather than being crushed by the dock.
5. The same `AgentMonitor`/job state drives both side and top renderers.
6. Chat scrolling cannot move the dock.
7. Dock scrolling cannot move the chat.
8. Existing focus/maximize/toggle semantics remain coherent.
9. Composer, goal/status and host-mode information stay visible.
10. No implementation claims per-region font sizing; font size remains a
    terminal-client concern.
11. Renderer tests cover the geometry matrix above.
12. No new runtime/network/security authority is introduced.

---

# 9. References

CURRENT:
- `harw-tui/src/panes.rs`
- `harw-tui/src/agent_monitor.rs`
- `harw-tui/src/jobs_panel.rs`
- `harw-tui/src/app/scroll_routing.rs`
- `harw-tui/src/status_line.rs`
- `docs/design/interaction-contract.md`
- `docs/design/tui-command-contract.md`

PLANNED:
- `docs/planning/65-cloud-sessions/README.md` §5.4 compact phone layout

Evidence:
- user-observed current portrait-phone TUI screenshots supplied 2026-09-27;
  they are not committed by this W00 planning PR.

---

# 10. Implementation status

```text
CURRENT:
  wide right-side panel + narrow one-line summary

PLANNED:
  portrait pinned top dock + existing wide + existing tiny fallback

IMPLEMENTED:
  not in this PR
```
