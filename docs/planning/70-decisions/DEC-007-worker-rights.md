---
id: DEC-007
title: Worker rights — read everywhere, write only within your own scope
status: accepted
date: 2026-09-27
tags: [decision, rights, sandbox, work-driver]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../../harw-registry-defaults/src/authority.rs
  - ../../../harw-ops/src/work_driver.rs
  - ../../../harw-plan-bridge/src/work_driver.rs
  - ../../../harw-cli/src/job_worker_work_driver.rs
---

# DEC-007 — Worker rights

## Decision
Every worker reads the entire workspace but writes only within the
`owned_paths` of its own scope; an empty scope means read-only.
This is enforced by the rights/sandbox layer, not just via the prompt.
`work_driver.status` requires `ReadWorkspace`, `work_driver.enqueue`
requires `ExecuteProcess`; enqueue and stop always ask for consent
(`approval = "always"`). Limit overrides on a call may only narrow the
spec's limits, never widen them.

## Why
- Reading across the whole workspace is necessary so a worker can understand
  context outside its own scope (shared types, callers, conventions)
  without being allowed to change anything there.
- Granting write permission per path via `owned_paths` alone would overburden
  the rights/sandbox layer; it only knows `{ReadWorkspace}` and
  `{ReadWorkspace, WriteWorkspace}`. The actual path boundary is checked
  separately by wave admission (`validate_patch` against `owned_paths`); a
  violation blocks acceptance of the patch.
- An empty scope (`owned_paths.is_empty()`) is the explicit read-only role:
  the worker gets only `{ReadWorkspace}` and the hint text "only read and
  report, change nothing".
- `work_driver.enqueue` starts a durable background job — like
  `job.start` — and therefore needs `ExecuteProcess`; `work_driver.status`
  and `work_driver.stop` only read the job and its history, like
  `job.status`/`job.stop`, and therefore need only `ReadWorkspace`.
- Enqueue/stop run as an operator action with `approval = "always"`: a human
  decides on the approval, not the model call itself.
- Overrides may only narrow (`narrow_spec`/`narrow_u32`/`narrow_budget`):
  a caller cannot widen its own limits beyond the bounds of the original
  spec; any attempt to raise a limit is rejected with an error.

## Consequences
- Workers with `owned_paths` get the registry profile `WorkspaceEdit`
  (rights `{Read, Write}`, only `fs.*`/`doc.*`/`explore.*`/workspace `deps.*`),
  workers without `owned_paths` get `ReadOnlyExplore` (`{Read}`); no
  worker gets process tools — the sandbox does not support path-granular
  write permission.
- Honest status: the rights/sandbox layer does not yet have a path-granular
  `WriteWorkspace`. `owned_paths` are therefore enforced after the fact,
  not preventively: a workspace snapshot before and after each wave
  (`snapshot_workspace`/`diff_snapshots`), checked with the same
  `validate_patch` mechanism as for plan nodes. A write outside the scope is
  neither prevented nor automatically rolled back — it blocks the wave and
  escalates to a human.
- Open follow-up: preventive, path-granular write permissions in the
  sandbox itself (instead of a snapshot diff after the fact), so that a
  scope violation never reaches the filesystem in the first place.
- Overrides are a pure narrowing API: new fields in
  `WorkDriverOverrides` must likewise go through the `narrow_*` helpers,
  otherwise a silent rights escalation results.
- Trade-off: no worker can accidentally inspect foreign code and
  modify it in parallel for good — but the violation is only detected after
  the write, not before; this calls for small, pre-planned scopes
  (see DEC-005) as additional damage limitation.

## Where in the code
- `harw-registry-defaults/src/authority.rs` — `tool_permission` for
  `work_driver.status` → `Permission::ReadWorkspace`, `work_driver.enqueue`
  → `Permission::ExecuteProcess`; `AuthorityReducer` upper bounds.
- `harw-ops/src/work_driver.rs` — `work_driver_enqueue`,
  `WorkDriverOverrides`, `narrow_spec`/`narrow_u32`/`narrow_budget`
  (overrides only narrow); `approval = "always"` on the
  `#[harw_macros::tool(...)]` declaration of `work_driver.enqueue`.
- `harw-plan-bridge/src/work_driver.rs` — `WorkScope::owned_paths`;
  `render_task` with the read-only hint "only read and report,
  change nothing" for an empty scope.
- `harw-cli/src/job_worker_work_driver.rs` — module comment: workers with
  `owned_paths` → `RegistryProfile::WorkspaceEdit` (`{Read, Write}`), without
  → `RegistryProfile::ReadOnlyExplore` (`{Read}`); `owned_rules` and the
  after-the-fact `validate_patch` admission (`snapshot_workspace`/
  `diff_snapshots` before/after the wave) against the `owned_paths`.

## Related
- [DEC-004 No parallel builds](DEC-004-no-parallel-builds.md)
- [DEC-005 Small scopes, many waves](DEC-005-small-scopes-waves.md)
- [DEC-006 Model-agnostic driver](DEC-006-model-agnostic.md)
- [DEC-008 No TUI](DEC-008-no-tui.md)
