---
id: W20-CONTROL-DECK-V2
title: "W20 Contract — portrait operator control deck"
status: proposed
date: 2026-10-01
parent: ../CONTROL-DECK-V2.md
---

# W20 Contract — portrait operator control deck

Pinned baseline: `dev@70faabed9182834792a5f36e67cd80535792542a`.

This is an implementation handoff contract. The planning PR itself must not
change runtime behavior.

## Invariants

1. Reuse the existing outer `PortraitDock` classification.
2. Use no more than the already bounded portrait dock height.
3. In readable split mode, render exactly three regions:
   - active agents upper left;
   - terminal aggregates upper right;
   - running managed jobs full width below.
4. Active agents stay concrete; terminal work aggregates by typed semantic
   category.
5. One terminal category normally occupies one row.
6. Aggregation is a projection; concrete agent/session/job identities remain
   inspectable.
7. Running jobs and terminal history are separate projections.
8. A job-backed child may appear simultaneously as a live agent and as a live
   managed job, but must contribute only once to terminal aggregation.
9. Category/provenance is typed. Missing provenance is `Unknown`; display
   names are never parsed to invent it.
10. Managed-job termination uses the existing `jobs` Operation path and its
    `CommandRegistry` admission.
11. The panel must not call `JobManager::stop` directly for an action already
    represented by `/jobs stop`.
12. Never turn a managed `JobId` into a raw PID kill path.
13. Raw OS-process aggregation/termination is outside W20.
14. Destructive actions are explicit and confirm the selected `JobId`.
15. `Ctrl+J` remains composer newline.
16. Wide desktop and narrow combined fallbacks are not redesigned by W20.
17. Modal approvals/dialogs outrank the deck.
18. R19 narrow-portrait status/composer/goal/host-mode behavior is a regression
    contract, not historical documentation only.
19. The projection design must remain compatible with later hosted-session
    attach/replay; it must not require the local TUI process to own work
    lifetime.

## Work packages

### W20-A — typed projection + correlation seam

Owned / likely touched:
- `harw-tool-job/src/manager.rs`;
- `harw-tool-job/src/model.rs`;
- `harw-tool-job/src/event.rs`;
- `harw-agent-runner/src/job_child_backend.rs`;
- `harw-tui/src/jobs_panel.rs` and/or `harw-tui/src/app/jobs_glue.rs`;
- `harw-tui/src/agent_monitor.rs`;
- tests.

Deliver:
- additive job provenance capable of identifying a job-backed child by
  `child_session_id`;
- a typed managed-job projection input;
- `active_agents_projection`;
- `terminal_aggregate_projection`;
- `running_jobs_projection`;
- deterministic category keys and counters;
- source-precedence/correlation rule: when one logical child is visible as both
  agent + managed job, the agent run owns the terminal count and the job is
  execution detail only;
- older/missing provenance becomes explicit `Unknown`, never guessed from a
  job display name.

Required tests:
- one child represented by both agent event and job metadata contributes one
  terminal aggregate;
- non-child managed jobs aggregate by typed tool origin;
- missing provenance stays unknown;
- active projections may show the same child in agent + job panes without
  double-counting terminal state.

No geometry, keybinding or operator-control changes.

### W20-B — deck geometry + portrait regression

Owned:
- `harw-tui/src/panes.rs`;
- `harw-tui-layout` only if typed internal geometry belongs there;
- layout/render tests.

Deliver:
- `DockAreas::ControlDeck { active_agents, terminal, running_jobs }`;
- existing `Combined` fallback;
- wide desktop unchanged;
- explicit preservation of the R19 narrow-portrait fixes.

Required geometry/regression cases:
- 160×40: current wide side panel unchanged;
- 99×40: portrait control deck;
- 80×40: three-region deck with full-width running jobs;
- 72×36: readable split deck;
- 68×36: split boundary;
- 67×36: combined fallback;
- 45×60: narrow portrait dock preserved;
- 40×28 … 59×80: portrait classification preserved;
- 39×40: summary/compact fallback;
- 50 columns + plan mark: marks and mode/model/context all remain visible;
- 40/45/55 host-warning combinations preserve host/plan/goal visibility;
- resize transitions request full redraw and keep selection valid;
- Ctrl+L mid-turn still requests full redraw.

### W20-C — renderer + independent deck state

Owned:
- `harw-tui/src/agent_monitor.rs`;
- app render glue;
- terminal constituent overlay/detail;
- renderer tests.

Deliver:
- three independent visible lists;
- independent selection and scroll state;
- terminal aggregate unseen-failure revision/badge;
- constituent drill-down ordered newest first;
- no second canonical agent/job store;
- legacy wide side-panel behavior may remain on the existing selection model
  until separately migrated.

### W20-D — direct job focus

Owned:
- `harw-tui/src/keybindings.rs`;
- panel/app key routing;
- help/keybinding tests.

Deliver:
- named `focus_jobs` action;
- candidate default `Alt+J`;
- Up/Down/Page/Home/End/Enter/Esc/F11 behavior;
- `Ctrl+J` unchanged.

### W20-E — managed-job stop through Operation dispatch

Owned:
- TUI command/control glue;
- confirmation dialog glue;
- operation-dispatch tests.

Deliver:
- panel-local `k` gesture;
- explicit confirmation naming the selected `JobId`;
- execution through the canonical operation path:
  `CommandRegistry::dispatch` → `CommandAdapter` → `/jobs stop <JobId>`
  → `JobsOperation::run` → `JobManager::stop`;
- the same permission tier, busy policy, service-map construction and
  `OpContext` rules as manually typing `/jobs stop`;
- no direct `JobManager::stop` from the key handler;
- no raw-process kill path.

## Verification

Each implementation wave records the repository-wide sequence:

```text
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -q -p xtask -- gates
cargo deny check
make -C dod clippy test
actionlint
```

Package-scoped TUI/layout tests may be run additionally for fast feedback.
They do not replace the workspace gates.

## Session-continuity handoff

W20 does not implement hosted-session lifecycle, but must not conflict with it.

The later first-party TUI attach path from PL-65 / PL-68 should be able to use
these projection types over replayed hosted-session and managed-job state.

Expected later behavior:

- opening the TUI discovers resumable/hosted sessions;
- choosing continue attaches/replays and resumes the chosen session directly;
- choosing not to continue does not destroy the old hosted session or its
  managed jobs;
- starting a new session is independent from old hosted work.

No W20 code should encode “TUI process exit == work lifetime ends” as a
projection invariant.

## Non-goals

- redesigning the wide desktop right column;
- replacing `AgentMonitor` / `JobManager` with a new store;
- adding a raw process supervisor;
- arbitrary OS-process terminal aggregation;
- arbitrary OS-process kill confirmation/execution;
- implementing an identity-bound `harw_killer` preview/execute API;
- implementing hosted-session attach/replay itself;
- implementing cgroup v2 job killing in the TUI;
- changing provider/model scheduling;
- making PL-71 semantic activity patterns a hard dependency.
