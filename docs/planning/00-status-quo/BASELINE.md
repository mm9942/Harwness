# Harw Baseline Snapshot

> Baseline commit: `d3e0b965696e7a631687a73618c934cda7ad3d2c`
> Snapshot date: 2026-09-26

This file exists only to pin the current-state baseline used by the planning tree.

## Current invariants

- Root Cargo workspace is the main product workspace.
- Root workspace excludes `dod/`.
- `dod/` is an independent Cargo workspace with its own `Cargo.toml` and lockfile.
- Root and DoD both enforce `unsafe_code = "forbid"`.
- `harw-runtime` is the high-level L12 composition crate.
- `harw-job-runtime` owns governed background-work primitives, not scheduling/execution.
- `harw-tool-job` owns local long-running process-job tools and process supervision.
- Current process recovery uses PID + `/proc/<pid>/stat` start ticks.
- Current job stopping uses process-group signaling.
- Bubblewrap is installed by the Linux source installer when missing.
- Current installation documentation is Linux-focused.
- DoD/Warden security/dependency gates are current, implemented constraints.
- Agent compiler architecture is accepted and partially implemented.

## Refresh rule

When the repository changes, do not mutate this historical snapshot.
Create a new baseline snapshot or update the master planning index to point at a newer baseline.
