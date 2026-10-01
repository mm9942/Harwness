---
id: PL-68-CONTROL-DECK-V2
title: "Portrait control deck v2 — active agents, terminal work aggregates, running jobs"
status: proposed
date: 2026-10-01
tags: [planning, tui, portrait, agents, jobs, operator-control]
related:
  - README.md
  - contracts/W20-control-deck-v2.md
  - ../65-cloud-sessions/README.md
  - ../68-local-cloud-websocket/README.md
  - ../71-semantic-activity-patterns/README.md
---

# Portrait control deck v2

> **Pinned CURRENT:** `dev@70faabed9182834792a5f36e67cd80535792542a`.
>
> **Evidence:** user-observed Harw TUI on 2026-10-01 plus source inspection of
> the TUI, operation registry, runtime assembly, job manager and agent runner.
>
> **Scope:** planning only. No runtime behavior is changed by this document.

## 1. Goal

Turn the existing portrait dock from a compact status projection into a bounded
operator control deck while preserving the current outer portrait layout.

The target split is:

```text
┌ Active agents ───────────────────┬ Finished / failed ────────────────┐
│ ● root-orchestrator   thinking   │ Explorer × 8   ✓ 6  ✗ 2  ⏹ 0   │
│ ● explorer#41         fs.read    │ Reviewer × 3   ✓ 3  ✗ 0  ⏹ 0   │
│ ● coder#12            cargo      │ job.start × 5  ✓ 4  ✗ 1         │
├ Running jobs ───────────────────────────────────────────────────────┤
│ ▸ cargo test --workspace  72%  18m12s   job-…                      │
│   agent-child-explorer   running  02m41s  job-…                     │
└─────────────────────────────────────────────────────────────────────┘
│                         CHAT / WORK                                 │
│                         ...                                         │
```

The three regions deliberately answer different questions:

1. **Active agents, upper left** — concrete live agent identities.
2. **Terminal aggregates, upper right** — completed/cancelled/failed work,
   compacted by semantic category without destroying concrete identities.
3. **Running jobs, full-width lower strip** — active managed jobs, optimized
   for inspection and operator control.

The deck is pinned. Chat scrolling never moves it.

## 2. CURRENT on the pinned baseline

### 2.1 Runtime and TUI wiring

The TUI is not the owner of command semantics.

The current chain is:

```text
#[operation]
    ↓
OperationMeta
    ↓
RuntimeAssembly.operations()
    ↓
CommandAdapter::from_operation
    ↓
CommandRegistry
    ↓
TUI discovery / help / admission / dispatch
    ↓
CommandAdapter::dispatch
    ↓
Operation::run
```

`ChatApp::with_memory` builds its command registry from the exact adapters
supplied by the composition root and only then merges the genuinely TUI-local
specs. Operation-backed commands therefore remain the canonical control path.

This matters for the control deck: a panel key may be a different *input
gesture*, but it must not become a second authority or mutation path.

### 2.2 Geometry

`harw-tui/src/panes.rs` already delegates portrait placement to
`harw-tui-layout` and exposes `split_dock_areas`.

The current readable portrait dock contains two peer rectangles:

```text
Agents | Jobs
```

Below the split threshold it falls back to the combined dock. W20 keeps the
existing outer `PortraitDock` classification and changes only the internal
split when enough room exists.

### 2.3 Agent projection

`AgentMonitor::panel_entries()` currently produces one combined selection
list:

1. active agents individually;
2. unseen failed agents individually;
3. completed/cancelled/seen-failed agents either individually or behind one
   global `Finished` row;
4. active jobs individually;
5. finished jobs behind one global `JobsFinished` row.

The current renderer then slices that one list into the two dock columns while
sharing one `selected` index and one scroll position.

W20 replaces that portrait projection only. It does not create a second
agent store.

### 2.4 Managed-job data

The TUI already receives `JobStatus` values from the session `JobManager`.
They contain the data required for the running-job strip and detail view:
job id, name, state, progress, runtime, process identity, owner, timestamps,
origin metadata, failure state and logs.

However, `JobRow::from_status` intentionally discards most provenance and
retains presentation fields only. Terminal aggregation therefore must not try
to infer semantic categories from `JobRow.name`.

### 2.5 Job-backed child agents create a correlation problem

Production compiled children run through
`harw-agent-runner::JobChildBackend`, which starts each child through
`JobManager::start_piped...`.

The same logical child can therefore be visible simultaneously as:

```text
AgentEvent / child_session_id
and
JobStatus / job_id
```

That is legitimate in the live deck: the upper-left pane shows the agent,
while the lower strip shows its execution job.

It is **not** legitimate to count both records independently in the terminal
aggregate. A completed Explorer backed by a managed child job must still
produce exactly one Explorer terminal run.

The pinned baseline does not currently persist the child's
`child_session_id` into the job metadata. W20-A must add that correlation
before terminal aggregation is considered complete.

### 2.6 Raw OS processes are intentionally out of W20

The current `harw_killer::api::preview` path returns a point-in-time process
snapshot and releases the identity handles before a later `kill_own` call
performs a fresh selection.

That creates two problems for this proposal:

- a confirmation based only on PID/name cannot guarantee the exact process is
  still the same process at execution time;
- a live process snapshot cannot provide a durable terminal lifecycle/history
  after the process exits.

Therefore W20 does **not** aggregate or terminate arbitrary raw OS processes.
A later process-control slice may add that only after an identity-bound
preview/execute API and a bounded lifecycle source exist.

Managed jobs already have process identity, lifecycle and ownership semantics
and remain fully in scope.

## 3. Target geometry

The outer portrait dock stays bounded by the existing one-third design.

Conceptually:

```text
preferred_deck = usable_body_height / 3
deck_height = clamp(preferred_deck, min_deck_rows, max_deck_rows)
```

Inside a readable portrait dock:

```text
deck
├── upper: ~55–60%
│   ├── active agents: ~60% width
│   └── terminal aggregates: ~40% width
└── running jobs: remaining height, full width
```

Recommended typed result:

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

Exact ratios are renderer tuning, not protocol.

### 3.1 Responsive fallback

- readable portrait width: render the three-region control deck;
- below the existing split threshold: keep the current combined dock;
- too short: keep summary/compact fallback;
- wide desktop: keep the current right-side panel.

The existing R19 narrow-portrait fixes are regression requirements, not
optional historical behavior.

## 4. Projection model

W20 separates canonical runtime state from deck presentation state.

```text
canonical sources
  AgentEvent / AgentMonitor
  JobStatus snapshots
        │
        ▼
typed correlation layer
  AgentRunKey(child_session_id)
  ManagedJobKey(job_id)
  ManagedJobOrigin
        │
        ├── ActiveAgentsProjection
        ├── TerminalAggregateProjection
        └── RunningJobsProjection
```

### 4.1 Typed managed-job provenance

Do not derive job category from a display name.

W20-A adds additive provenance for job-backed children so a job can say that
it backs a specific child session. The exact serialized shape may evolve, but
the information must be typed at the projection boundary:

```rust
enum ManagedJobOrigin {
    AgentChild {
        child_session_id: SessionId,
    },
    Tool {
        tool: String,
        call_id: Option<String>,
    },
    Operator,
    Unknown,
}
```

A practical implementation may add an optional child-session field to
`JobOrigin` / `JobMeta` / `JobEvent`, plus a piped-start path that accepts
origin metadata. Older `meta.json` files must deserialize with no provenance
and become `Unknown`, never guessed from `name`.

### 4.2 Correlation and source precedence

Terminal accounting uses one canonical logical run.

Rules:

1. An agent event with `child_session_id` defines the agent run's category
   from its role.
2. A managed job with
   `ManagedJobOrigin::AgentChild { child_session_id }` is linked to that run.
3. If both exist, the **agent run owns the terminal count/category/outcome**;
   the job remains available as execution detail but is not counted again.
4. A non-child managed job is categorized by typed tool origin when present.
5. Missing provenance becomes an explicit `Unknown job` category.
6. Display text is never parsed to invent provenance.

This allows the live deck to show one child in both useful live views without
rendering it twice in terminal history.

## 5. Active-agents pane

The upper-left pane shows concrete active identities.

Include:

- active/waiting agent rows;
- role;
- short task/current tool when it fits;
- elapsed time;
- context gauge/percentage when space remains.

Do not place terminal runs here.

Sorting remains stable:

1. root/orchestrator lineage first;
2. current tree/insertion order;
3. no frame-by-frame reordering by token rate or elapsed time.

Enter opens the existing detail.

If direct agent cancellation is later exposed in this pane, it must route
through the existing `agent` operation/control contract, not through process
termination.

## 6. Terminal-aggregate pane

The upper-right pane is a bounded aggregate of terminal **agent runs and
managed jobs**.

Example:

```text
Explorer    × 12   ✓ 9   ✗ 2   ⏹ 1   last 14s
Reviewer    ×  7   ✓ 7                last 8s
job.start   ×  4   ✓ 3   ✗ 1          last 2m
Unknown job ×  1   ✗ 1                last 5m
```

A job-backed Explorer contributes to `Explorer`, not additionally to a second
job category.

### 6.1 Failure visibility

A terminal failure joins its aggregate immediately, while the category retains
an unseen-failure revision/badge.

Example:

```text
Explorer × 12 · ✗ 2 NEW · last: timeout
```

Selecting the aggregate opens a constituent list ordered newest first.
Acknowledgement advances the aggregate's seen revision; it does not delete
history.

### 6.2 Retention boundary

W20 only promises history for data the current sources actually retain.

- Agent terminal records are bounded by the current in-process
  `AgentMonitor`.
- Managed jobs retain their job metadata/logs according to the existing job
  subsystem.
- Cross-process/restart session replay belongs to PL-65 hosted sessions and
  must not be faked by the portrait projection.

When the first-party TUI attaches to a hosted session later, these projection
types should be reusable over replayed session/job state rather than tied to a
local `ChatApp`.

## 7. Running-jobs strip

The lower full-width pane contains only active managed jobs.

Each row prefers:

1. selection marker;
2. name;
3. progress;
4. runtime;
5. state;
6. short job id;
7. placement/PID only if room remains.

No finished-job clutter belongs here.

A job-backed child may appear here at the same time as its agent identity in
the upper-left pane. That is intentional: one is the agent topology view, the
other is the managed execution view.

Enter opens the existing job detail/log view.

## 8. Focus and keyboard control

The global F4 focus cycle remains valid, but running jobs need a direct focus
action.

Named action:

```text
focus_jobs
```

Candidate default:

```text
Alt+J
```

`Ctrl+J` remains composer newline.

When the running-jobs strip has focus:

- Up/Down: previous/next job;
- PageUp/PageDown/Home/End: list navigation;
- Enter: job detail/log view;
- `k`: request stop for the selected managed job;
- Esc: return focus to Chat;
- F11: maximize the focused jobs region.

All bindings remain configurable through the existing keybinding system.

## 9. Control actions use Operations, not panel-local authority

The panel is a projection and input surface. It must not call the underlying
manager directly for mutations when an operation already owns the contract.

For a selected managed job:

```text
k
 ↓
explicit confirmation
 ↓
CommandRegistry admission
 ↓
CommandAdapter for /jobs
 ↓
/jobs stop <JobId> [TERM]
 ↓
JobsOperation::run
 ↓
JobManager::stop
 ↓
job state/event refresh
```

The implementation may use a typed helper around the existing command
dispatcher, but it must preserve the same operation metadata, permission tier,
busy semantics, service map and `OpContext` as a normal `/jobs stop` command.

Specifically forbidden in W20 panel glue:

- direct `JobManager::stop` from the key handler;
- converting `JobId` to PID and killing the process;
- shelling out to `killer` or `harw kill`;
- a second TUI-only permission check that can diverge from
  `CommandRegistry::dispatch`.

This is the same architectural rule already used by the current TUI command
pipeline: input gesture may differ, authority does not.

### 9.1 Confirmation

The destructive action names the exact managed job:

```text
Stop job "cargo test --workspace" (job-9)?
TERM, then the existing job stop policy if it does not exit.
[Enter] stop   [Esc] cancel
```

The confirmation is a UI step only. Admission remains in the operation path.

## 10. Presentation state

The deck owns only presentation state:

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

No projection grants authority.

The existing wide side panel may continue using the legacy
`AgentMonitor.selected` path until separately migrated.

## 11. Source delta

### W20-A — typed projection/correlation seam

Likely touched:

- `harw-tool-job/src/manager.rs`
- `harw-tool-job/src/model.rs`
- `harw-tool-job/src/event.rs`
- `harw-agent-runner/src/job_child_backend.rs`
- `harw-tui/src/jobs_panel.rs` and/or `app/jobs_glue.rs`
- `harw-tui/src/agent_monitor.rs`
- tests

Deliver:

- additive child-session provenance for job-backed children;
- a typed managed-job projection input that preserves provenance;
- correlation/source-precedence tests;
- active/terminal/running projections;
- no visual change.

### W20-B — three-region geometry

Likely touched:

- `harw-tui/src/panes.rs`;
- `harw-tui-layout` only if typed internal geometry belongs there;
- layout tests.

Deliver:

- `DockAreas::ControlDeck { active_agents, terminal, running_jobs }`;
- existing combined fallback;
- no regression of R19 portrait placement/status behavior.

### W20-C — renderer + independent deck state

Likely touched:

- `harw-tui/src/agent_monitor.rs`;
- app render glue;
- terminal constituent overlay/detail;
- renderer tests.

Deliver:

- three independent visible lists;
- independent selection/scroll;
- aggregate unseen-failure revision;
- constituent drill-down;
- no second canonical store.

### W20-D — direct job focus

Likely touched:

- `harw-tui/src/keybindings.rs`;
- panel/app key routing;
- help/keybinding tests.

Deliver:

- named `focus_jobs` action;
- candidate default `Alt+J`;
- Up/Down/Page/Home/End/Enter/Esc/F11 behavior;
- `Ctrl+J` preserved.

### W20-E — managed-job stop through operation dispatch

Likely touched:

- TUI command/control glue;
- confirmation dialog glue;
- operation-dispatch tests.

Deliver:

- panel-local `k` gesture;
- explicit target confirmation;
- canonical `/jobs stop <JobId>` operation path;
- no direct manager stop from panel glue;
- no raw-process path.

## 12. Test matrix

Geometry and regression coverage:

| Case | Expected |
|---|---|
| 160×40 | current wide side panel unchanged |
| 99×40 | portrait control deck |
| 80×40 | three-region deck, jobs full width below |
| 72×36 | three-region deck remains readable |
| 68×36 | split boundary works |
| 67×36 | combined dock fallback |
| 45×60 | R19 portrait dock preserved |
| 40×28 … 59×80 | R19 narrow portrait classification preserved |
| 39×40 | summary/compact fallback |
| 50 wide + plan mark | two-row status preserves marks + mode/model/context |
| 40/45/55 + host warning combinations | host warning/plan/goal remain visible |
| resize split→combined | selections remain valid; full redraw |
| resize combined→split | selections rebase deterministically; full redraw |
| Ctrl+L mid-turn | full redraw preserved |

Behavior coverage:

- multiple active explorers remain concrete rows;
- a job-backed child is visible as agent + active job while running;
- that same child produces exactly one terminal aggregate count;
- one success + two failures of the same role produce one aggregate;
- missing job provenance stays `Unknown job` and is never guessed from name;
- unseen failures mark the aggregate without duplicate rows;
- finished jobs leave the running strip;
- `Alt+J` (or configured `focus_jobs`) focuses jobs without inserting text;
- `k` on a managed job reaches the `jobs` operation adapter;
- rejected permission/admission does not build a second mutation path;
- job stop updates state on the next normal refresh/event;
- dialogs/approvals outrank the deck;
- chat and all three deck regions scroll independently;
- composer, goal, status, mode/model/context and host-mode visibility from R19
  remain intact.

## 13. Verification

Each implementation wave records the repository-wide gate sequence:

```text
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -q -p xtask -- gates
cargo deny check
make -C dod clippy test
actionlint
```

Package-specific TUI/layout tests may be run additionally for fast feedback;
they do not replace the workspace gates.

## 14. Session continuity boundary

The separate hosted-session plan remains the owner of detach/re-attach
semantics.

The intended end state from PL-65 is:

```text
hosted session
  ├─ root + children continue without a TUI attachment
  ├─ managed jobs continue under host/job ownership
  └─ TUI attach/detach/reconnect is a client concern
```

W20 should therefore avoid hard-coding assumptions that the local TUI process
owns the lifetime of the work it displays.

The first-party reconnect/resume behavior is a follow-on integration:

- opening the TUI discovers resumable/hosted sessions;
- choosing continue attaches/replays and resumes directly;
- choosing not to continue does not destroy the old hosted session or its jobs;
- a new session can be started independently.

That is not implemented by this planning PR and should land through the
existing PL-65 / first-party TUI attach work rather than being bolted into
portrait rendering.

## 15. Acceptance criteria

W20 is complete when:

1. The portrait deck stays within the existing bounded top region.
2. Active agents occupy the upper-left pane as concrete runs.
3. Terminal agent runs and managed jobs occupy the upper-right pane as one row
   per semantic category.
4. A role such as `Explorer` appears at most once in terminal aggregation,
   including when its child execution is also a managed job.
5. Missing job provenance is explicit rather than inferred from display text.
6. Running managed jobs occupy one full-width strip below the upper panes.
7. The three regions have independent selection/scroll state.
8. Jobs have a direct configurable focus action without changing `Ctrl+J`.
9. A selected managed job is stopped through the same `jobs` operation
   admission/dispatch path as `/jobs stop`.
10. W20 introduces no raw-process kill/aggregation path.
11. Existing R19 narrow-portrait status/composer/goal/host-mode behavior remains
    covered.
12. No canonical agent/job identity is destroyed by visual aggregation.
13. The design remains compatible with later hosted-session replay/attach.
14. This PR itself changes documentation only.
