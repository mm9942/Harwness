---
id: W00-MOBILE-TUI-TOP-DOCK
title: "W00 Contract — portrait top agent/jobs dock"
status: proposed
date: 2026-09-27
parent: ../README.md
---

# W00 Contract — portrait top agent/jobs dock

Pinned baseline: `dev@79b471d9b64544714d74306b3356711daf8c0f7a`.

This is a handoff contract for implementation. It fixes the behavior that
workers must preserve and the boundaries they must not redesign.

## 1. Invariants

1. `cols >= 100` keeps the current right-side panel behavior.
2. Narrow+tall terminals get a pinned top agent/jobs dock.
3. Too-short terminals keep the current summary/compact fallback.
4. Agent/job state has one owner: existing `AgentMonitor` + job rows.
5. Chat width remains full in portrait mode.
6. Top dock uses about one third of usable height, bounded by minimum/maximum
   rows and a lower-work-area minimum.
7. Status/composer stay at the bottom.
8. Modal dialogs/approvals outrank the dashboard.
9. F3/F4/F11/Esc semantics remain compatible.
10. Scroll routing is rectangle-based and works when the agent rectangle moves
    from right to top.
11. No per-region font-size claim is introduced.
12. Explorer/Workbench are out of W00 scope.

## 2. Proposed constants

Names may change during implementation, semantics may not:

```rust
WIDE_AGENT_PANEL_MIN_COLS = 100
PHONE_DOCK_MIN_COLS = 60
PHONE_DOCK_MIN_ROWS = 28
PHONE_DOCK_MIN_HEIGHT = 8
PHONE_DOCK_MAX_HEIGHT = 15
PHONE_DOCK_SPLIT_MIN_COLS = 68
PHONE_DOCK_TARGET_NUMERATOR = 1
PHONE_DOCK_TARGET_DENOMINATOR = 3
```

Do not hard-code a device/user-agent check.

## 3. Work packages

### W01 — layout classification

Owned area:
- `harw-tui/src/panes.rs`
- pane-specific tests

Add a semantic placement/class result.

Acceptance:
- exact geometry matrix in parent plan;
- wide behavior unchanged;
- summary/tiny behavior preserved;
- no renderer/state changes yet.

### W02 — dock projection

Owned area:
- `harw-tui/src/agent_monitor.rs`
- `harw-tui/src/jobs_panel.rs` only if a reusable formatting helper is needed

Acceptance:
- same underlying state;
- no duplicate polling/store;
- split agents/jobs at readable widths;
- compact combined rendering below split threshold;
- no wrapping that changes row counts unpredictably.

### W03 — viewport wiring

Owned area:
- viewport/render glue in `harw-tui/src/app.rs` or its extracted module

Acceptance:
- top dock pinned;
- chat directly below;
- bottom status/composer unchanged;
- overlays/dialogs preserve precedence.

### W04 — input routing

Owned area:
- `harw-tui/src/app/scroll_routing.rs`
- relevant key/focus tests

Acceptance:
- wheel in dock -> dock only;
- wheel in transcript -> transcript only;
- F3/F4/F11/Esc pass;
- focused dock consumes its navigation keys.

### W05 — real-device polish

No architecture rewrite.

Use:
- renderer tests;
- current phone screenshots;
- one new screenshot after implementation at comparable terminal geometry.

Tune only:
- thresholds;
- labels;
- priority/ellipsis;
- dock height clamp.

## 4. Regression matrix

Required test geometries:

```text
160x40  wide side
120x30  wide side
100x30  wide side boundary
 99x40  portrait dock
 80x40  portrait dock
 72x36  portrait split
 64x36  portrait combined/compact internal
 80x24  summary
 60x28  portrait minimum boundary
 59x40  compact/tiny path
 80x19  compact/tiny path
```

For each relevant geometry assert:
- chat rect;
- agent rect;
- placement semantic;
- status/composer presence.

Do not snapshot the entire screen for every case; assert structural geometry and
a few stable labels so wording changes do not make layout tests brittle.

## 5. Verification

Implementation integration SHA must be frozen before central verification:

```text
cargo fmt --all -- --check
cargo clippy -p harw-tui --all-targets -- -D warnings
cargo test -p harw-tui
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -q -p xtask -- gates
```

Any change after the frozen SHA invalidates the recorded result.

## 6. Reviewer checklist

Reviewer must confirm:
- no physical phone detection;
- no second agent/job state model;
- no regression to desktop side panel;
- no starvation of composer/work area;
- no hidden modal behind the dock;
- no input leakage from focused dock to composer;
- no "larger lower font" implementation claim in Ratatui;
- PL-65 compact layout remains compatible rather than silently superseded.
