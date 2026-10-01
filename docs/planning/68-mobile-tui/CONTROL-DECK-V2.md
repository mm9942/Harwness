---
id: PL-68-CONTROL-DECK-V2
title: "Portrait control deck v2 — active agents, terminal aggregates, running jobs"
status: proposed
date: 2026-10-01
tags: [planning, tui, portrait, agents, jobs, processes, operator-control]
related:
  - README.md
  - contracts/W20-control-deck-v2.md
  - ../71-semantic-activity-patterns/README.md
---

# Portrait control deck v2

> **Pinned CURRENT:** `dev@70faabed9182834792a5f36e67cd80535792542a`.
>
> **Evidence:** user-observed Harw TUI on 2026-10-01, with the existing portrait
> dock visible above the chat/work area.
>
> **Scope:** planning only. No runtime behavior is changed by this document.

## 1. Goal

Turn the existing portrait dock from a compact status projection into a small
operator control deck that uses at most about one third of the usable viewport.

The desired split is:

```text
┌ Active agents ───────────────────┬ Finished / dead ─────────────────┐
│ ● root-orchestrator   thinking   │ Explorer × 8   ✓ 6  ✗ 2  ⏹ 0   │
│ ● explorer#41         fs.read    │ Reviewer × 3   ✓ 3  ✗ 0  ⏹ 0   │
│ ● coder#12            cargo      │ cargo × 5      ✓ 4  ✗ 1         │
├ Running jobs ───────────────────────────────────────────────────────┤
│ ▸ cargo test --workspace  72%  18m12s   job-…   TERM→KILL          │
│   build webui           running  02m41s  job-…                      │
└─────────────────────────────────────────────────────────────────────┘
│                         CHAT / WORK                                 │
│                         ...                                         │
```

The three deck regions have deliberately different semantics:

1. **Active agents, upper left** — concrete live identities. Every running or
   waiting agent remains individually selectable.
2. **Terminal aggregates, upper right** — completed, cancelled and failed
   agents/processes are compacted by semantic category. There is normally only
   one row per category, e.g. one `Explorer` row even if twenty explorer runs
   have terminated.
3. **Running jobs, full-width lower strip** — only currently active jobs,
   optimized for navigation, status inspection and termination.

The deck is pinned. Chat scrolling never moves it.

## 2. CURRENT on the pinned baseline

### 2.1 Geometry

`harw-tui/src/panes.rs` already delegates portrait placement to
`harw-tui-layout` and exposes `split_dock_areas`.

The current split dock contains exactly two peer rectangles:

```text
Agents | Jobs
```

At widths below the split threshold it falls back to a combined dock.

This proposal keeps the existing outer portrait classification and changes only
the internal split when enough width and height exist.

### 2.2 Agent projection

`AgentMonitor::panel_entries()` currently builds one combined selection list:

1. active agents individually;
2. unseen failed agents individually;
3. completed/cancelled/seen-failed agents either individually or behind one
   global `Finished` row;
4. active jobs individually;
5. finished jobs behind one global `JobsFinished` row.

That is already a useful reduction, but it is not the category-based terminal
projection requested here. A failed explorer and ten completed explorers still
do not become one persistent `Explorer` category row.

### 2.3 Existing dock renderer

`render_dock` currently projects the same `panel_entries` into an
agents-only column and a jobs-only column. Both columns share the same
`selected` index and `panel_scroll`.

For v2 that single-list assumption should be removed from the portrait deck.
The three regions need independent selection/scroll state while still reading
from the same canonical monitor/job state.

### 2.4 Job data and detail

The current TUI already has the data the running-job strip needs:

- name;
- state;
- progress;
- runtime;
- failure reason;
- job id;
- detail view with origin, PID, logs and command;
- one-second refresh in `app/jobs_glue.rs`.

The v2 deck should therefore be a new projection, not a second job manager.

### 2.5 Termination paths already exist

Two different termination semantics exist and must not be blurred:

- managed jobs use the job control path (`job.stop` / `JobManager::stop`),
  which preserves ownership, job state and process-group semantics;
- raw OS processes can be terminated through `harw_killer::api`, whose
  public selection API is PID/name based and whose signaling path keeps pidfds
  internally. The API refuses foreign-user processes and never invokes sudo.

The TUI must never shell out to the `killer` binary.

## 3. Target geometry

The **outer** portrait dock stays bounded by the existing one-third design.

Conceptually:

```text
preferred_deck = usable_body_height / 3
deck_height = clamp(preferred_deck, min_deck_rows, max_deck_rows)
```

Inside the deck, when split mode is readable:

```text
deck
├── upper: ~55–60%
│   ├── active agents: ~60% width
│   └── terminal aggregates: ~40% width
└── running jobs: remaining height, full width
```

Recommended representation:

```rust
pub(crate) enum DockAreas {
    Combined(Rect),
    ControlDeck {
        active_agents: Rect,
        terminal: Rect,
        running_jobs: Rect,
    },
}
```

The exact ratios are renderer-tuning constants, not protocol.

### 3.1 Responsive fallback

- enough width (initially reuse the existing split threshold, currently 68):
  render the full three-region control deck;
- below the threshold: preserve the current combined portrait dock rather than
  crushing three unreadable panes;
- too short: preserve summary/compact fallback;
- wide desktop: keep the current right-side panel unless a separate desktop
  redesign is explicitly accepted.

This PR does not redefine the outer layout classes.

## 4. Active-agents pane

The left pane shows **concrete active identities**, not categories.

Include:

- active/waiting agent rows;
- role;
- short task/current tool when it fits;
- elapsed time;
- context gauge/percentage when space remains;
- failure-free active state.

Do not place completed or failed terminal runs here.

Sorting should remain stable and understandable:

1. root/orchestrator lineage first;
2. then current tree/insertion order;
3. never reorder every frame by token rate or elapsed time.

Selection opens the existing agent detail.

Agent cancellation continues to use the existing agent cancellation/stop
contract; it does **not** go through the process killer merely because the
agent happens to have a process underneath.

## 5. Terminal-aggregate pane

The upper-right pane is a compact terminal history for **agents and processes**.

### 5.1 Aggregation rule

Terminal entities are projected by semantic category:

```rust
enum TerminalCategory {
    AgentRole(String),
    JobKind(String),
    ProcessKind(String),
}
```

Initial category derivation:

- agent: normalized role/display role;
- managed job/process: job kind or origin tool when available;
- raw process: executable basename;
- unknown: explicit `unknown`, never guessed from free text.

Example:

```text
Explorer   × 12   ✓ 9   ✗ 2   ⏹ 1   last 14s
Cargo      ×  4   ✓ 3   ✗ 1          last 2m
Reviewer   ×  7   ✓ 7                last 8s
```

A terminated explorer run increments `Explorer`; it does not add another
persistent row.

### 5.2 Failure visibility

The current panel keeps unseen failures individually visible until they are
acknowledged. V2 changes the portrait projection: a failure joins its category
immediately, but the aggregate row is visually elevated when it contains new
unseen failures.

Suggested row state:

```text
Explorer × 12 · ✗ 2 NEW · last: timeout
```

Selecting the row opens a constituent list/detail overlay ordered newest first.
Acknowledgement marks the aggregate's current failure revision seen without
deleting history.

This satisfies both requirements:

- only one terminal row per category;
- failures are not silently hidden.

### 5.3 Projection only

Aggregation is not deletion.

Concrete session ids, job ids and process ids remain available for details,
audit and actions. If PL-71 semantic activity patterns lands, this pane can be a
consumer of its projection model; v2 must not depend on PL-71 to exist.

## 6. Running-jobs strip

The lower full-width pane contains **only active jobs**.

Each row should prefer, in order:

1. selection marker;
2. name;
3. progress;
4. runtime;
5. state;
6. short job id;
7. placement/PID only if room remains.

No finished-job clutter belongs in this strip.

Example:

```text
▸ cargo test --workspace  72% · 18m12s · running · job-9
  webui build             41% · 02m41s · running · job-12
```

Enter opens the existing job detail/log view.

## 7. Focus and keyboard control

The current global `F4` focus cycle remains valid, but the control deck needs
a direct job focus action so an operator can jump to running jobs without
cycling through unrelated panes.

Planned named action:

```text
focus_jobs
```

Candidate default:

```text
Alt+J
```

Do **not** use `Ctrl+J`; it is already the default composer newline action.

When the running-jobs strip has focus:

- Up/Down: previous/next job;
- PageUp/PageDown/Home/End: existing list semantics;
- Enter: job detail/log view;
- `k`: request graceful stop/kill action for selected target;
- Esc: return focus to Chat;
- `F11`: maximize the focused jobs region.

The exact default chord remains configurable through the existing keybinding
system.

## 8. "Kill with killer" without breaking job semantics

The UI should expose one operator concept — **terminate selected work** — but
route by target type.

Conceptual adapter:

```rust
enum TerminationTarget {
    ManagedJob(JobId),
    RawProcess(i32),
}

enum TerminationBackend {
    JobControl,
    KillerApi,
}
```

Routing:

```text
ManagedJob(job_id)
    -> JobManager::stop(..., TERM)
    -> existing grace/hard-stop behavior
    -> job metadata/state updated

RawProcess(pid)
    -> harw_killer::api::preview(...)
    -> confirm exact pid/name
    -> harw_killer::api::kill_own(...)
```

Important invariant: if a row has a `JobId`, **do not convert it to a PID and
call killer directly**. That would bypass job ownership/state and can leave the
UI ledger inconsistent.

The TUI must call the Rust library/API path, never spawn `harw kill` or
`killer` as a subprocess.

A future job backend may use pidfd/cgroup termination internally. That is
orthogonal to this UI contract.

### 8.1 Confirmation

A destructive action should show an explicit target:

```text
Stop job "cargo test --workspace" (job-9)?
TERM, then hard stop if it does not exit.
[Enter] stop   [Esc] cancel
```

No executable-name wildcard kill from a selected managed job.

## 9. State model

Do not make `AgentMonitor` own three unrelated duplicated stores.

Preferred split:

```text
canonical live state
  AgentMonitor agents
  JobRow snapshots
  optional process snapshots
        │
        ├── ActiveAgentsProjection
        ├── TerminalAggregateProjection
        └── RunningJobsProjection
```

Portrait UI state owns only presentation state:

```rust
struct ControlDeckUi {
    focus: DeckFocus,
    active_selected: usize,
    terminal_selected: usize,
    jobs_selected: usize,
    active_scroll: usize,
    terminal_scroll: usize,
    jobs_scroll: usize,
}
```

No projection may grant authority.

## 10. Source delta

### `harw-tui-layout`

- keep outer `PortraitDock` classification;
- if internal dock geometry belongs here, add a typed three-region result;
- otherwise leave outer geometry untouched and split only in `harw-tui`.

### `harw-tui/src/panes.rs`

- replace the two-way split result for readable portrait widths with the
  three-region control deck;
- preserve `Combined` fallback;
- keep wide desktop behavior unchanged.

### `harw-tui/src/agent_monitor.rs`

- extract active-agent projection from the current combined
  `panel_entries()`;
- add terminal category projection;
- keep existing side-panel behavior until separately migrated;
- do not make one global `selected` index serve three deck panes.

### `harw-tui/src/jobs_panel.rs`

- reuse `JobRow`, `job_line` formatting primitives and detail data;
- add a running-only row projection optimized for full width.

### `harw-tui/src/app/jobs_glue.rs`

- keep one-second refresh and existing job manager;
- add selected-job stop action through the canonical JobManager path;
- keep detail opening unchanged.

### `harw-tui/src/keybindings.rs`

- add `FocusJobs` / `focus_jobs` as a configurable action;
- candidate default `Alt+J`;
- preserve `Ctrl+J` composer newline.

### `harw-killer`

- no required runtime change for v2;
- raw-process actions may call the existing library API;
- do not add a TUI-specific CLI wrapper.

## 11. Migration phases

### W20-A — projections, no visual change

- define read-only active/terminal/running projections;
- add category aggregation tests;
- keep current renderer.

### W20-B — three-region geometry

- introduce typed control-deck areas;
- renderer tests for portrait sizes;
- keep combined fallback.

### W20-C — independent focus/selection

- independent selection and scroll state per region;
- F4 integration;
- add `focus_jobs` action.

### W20-D — running-job operator controls

- Enter opens detail;
- `k` opens explicit stop confirmation;
- managed jobs route through JobManager/job-stop semantics;
- raw process termination, if surfaced, routes through `harw_killer::api`.

### W20-E — terminal archive polish

- one row per semantic category;
- unseen-failure badge/revision;
- constituent drill-down;
- renderer width tests.

## 12. Test matrix

At minimum:

| Geometry | Expected |
|---|---|
| 160x40 | current wide side panel unchanged |
| 99x40 | portrait control deck |
| 80x40 | three-region deck, jobs full width below |
| 72x36 | three-region deck remains readable |
| 68x36 | split boundary works |
| 67x36 | existing combined dock fallback |
| 45x60 | combined dock fallback, not crushed three-pane mode |
| 80x24 | summary/compact fallback |
| resize split→combined | selection stays valid, no panic |
| resize combined→split | active/job selection rebased deterministically |

Behavior tests:

- multiple active explorers remain concrete rows;
- once terminated, explorer runs collapse to one `Explorer` terminal row;
- one success + two failures of the same role produce one row with correct
  counts;
- unseen failures mark the aggregate without spawning duplicate rows;
- running jobs never appear in terminal aggregates;
- finished jobs disappear from the running strip and increment terminal
  projection;
- `Alt+J` (or configured `focus_jobs`) focuses jobs without inserting text;
- `k` on a managed job never invokes raw PID/name selection;
- job stop updates the job state seen on the next refresh;
- raw-process kill never targets a foreign UID through the API;
- dialogs/approvals still outrank the deck;
- chat and each deck region scroll independently.

## 13. Acceptance criteria

V2 is complete when:

1. The portrait deck never exceeds the existing bounded ~one-third top region.
2. Active agents occupy the upper-left pane as concrete runs.
3. Terminal agents/processes occupy the upper-right pane as one row per
   semantic category.
4. A role such as `Explorer` appears at most once in the terminal pane unless
   the operator drills into its constituents.
5. Running jobs occupy one full-width strip below the two upper panes.
6. Jobs can be reached directly by a configurable keyboard action.
7. A selected managed job can be stopped from the TUI without bypassing job
   ownership/state semantics.
8. Raw-process termination uses `harw_killer::api`, not a spawned CLI.
9. The three panes have independent selection/scroll state.
10. Existing wide/combined/summary fallbacks remain coherent.
11. No canonical event/job/agent identity is destroyed by visual aggregation.
12. This planning PR changes documentation only.
