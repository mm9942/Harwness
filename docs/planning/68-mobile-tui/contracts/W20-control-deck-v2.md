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
   - running jobs full width below.
4. Active entities stay concrete; terminal entities aggregate by category.
5. One terminal category normally occupies one row.
6. Aggregation is a projection; concrete IDs remain inspectable.
7. Running jobs and terminal history are separate projections.
8. Managed-job termination uses the job control path.
9. Raw-process termination may use `harw_killer::api`.
10. Never shell out to `killer` / `harw kill` from the TUI.
11. Never turn a managed JobId into a raw PID kill path.
12. Destructive actions are explicit and confirm the exact selected target.
13. `Ctrl+J` remains composer newline.
14. Wide desktop and narrow combined fallbacks are not redesigned by W20.
15. Modal approvals/dialogs outrank the deck.

## Work packages

### W20-A — projection extraction

Owned:
- `harw-tui/src/agent_monitor.rs`
- tests only

Deliver:
- `active_agents_projection`;
- `terminal_aggregate_projection`;
- `running_jobs_projection`;
- deterministic category key and counters.

No geometry or key changes.

### W20-B — deck geometry

Owned:
- `harw-tui/src/panes.rs`
- `harw-tui-layout` only if the typed geometry belongs there
- renderer/layout tests

Deliver:
- `DockAreas::ControlDeck { active_agents, terminal, running_jobs }`;
- existing `Combined` fallback below split threshold.

### W20-C — renderer + independent deck state

Owned:
- `harw-tui/src/agent_monitor.rs`
- app render glue

Deliver:
- three independent visible lists;
- independent selection/scroll;
- constituent drill-down for a terminal category;
- no second canonical store.

### W20-D — direct job focus

Owned:
- `harw-tui/src/keybindings.rs`
- panel/app key routing
- help/keybinding tests

Deliver:
- named `focus_jobs` action;
- candidate default `Alt+J`;
- Up/Down/Page/Home/End/Enter/Esc/F11 behavior.

### W20-E — termination action

Owned:
- `harw-tui/src/app/jobs_glue.rs`
- confirmation dialog glue
- tests

Deliver:
- panel-local `k` action;
- explicit target confirmation;
- managed job -> JobManager stop path;
- raw process -> killer library API only if raw processes are rendered.

## Verification

For each implementation wave:

```text
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p harw-tui
cargo test -p harw-tui-layout
cargo run -q -p xtask -- gates
```

If a wave does not touch `harw-tui-layout`, its package test may be skipped
only with that fact recorded in the PR.

## Non-goals

- redesigning the wide desktop right column;
- replacing AgentMonitor/JobManager with a new store;
- adding a new process supervisor;
- making terminal aggregation durable across restart without an existing
  durable source;
- implementing cgroup v2 job killing in the TUI;
- changing provider/model scheduling;
- making PL-71 semantic activity patterns a hard dependency.
