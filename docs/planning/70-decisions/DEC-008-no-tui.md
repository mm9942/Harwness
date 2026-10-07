---
id: DEC-008
title: WorkDriver without a TUI surface
status: accepted
date: 2026-09-27
tags: [decision, work-driver, tool-surface]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../docs/guides/work-driver.md
  - ../../docs/design/agent-definition-dsl.md
---

# DEC-008 — WorkDriver without a TUI surface

## Decision
The WorkDriver is exclusively a job kind (`work_driver`) plus three model
tools and the corresponding web routes. It gets no TUI slash command and no
`harw` subcommand surface of its own. It is started, observed and stopped
only by the orchestrator itself via `work_driver.enqueue`/`.status`/`.stop`,
or on its behalf via the web API. Both paths always require an approval for
`enqueue`/`stop`.

## Why
- A TUI surface would shift the tool semantics: a human at the terminal
  would have a different approval and rights path than the orchestrator,
  which triggers the same job through `work_driver.enqueue`. Two paths to
  the same effect are unnecessary and add a second rate-limit and
  authorization field to maintain.
- The WorkDriver is meant as the orchestrator's delegation mechanism
  (DEC-007), not as an interactive control panel for humans; a human stays
  in the loop through approval gates and `work_driver.status` (read-only,
  no approval).
- The tools are reachable only if the agent IR has a `[work_driver]`:
  lowering sets them, the roster clamp pins exactly these three tools, and
  everything else still falls through the base role's clamp.
- A slim surface (job kind + three tools + web route) can be audited
  entirely through the existing approval and rights paths (chapter R14),
  without adding another surface type to the roster, capability catalog and
  authorization.

## Consequences
- Anyone who wants to use the WorkDriver needs an orchestrator definition
  with a `[work_driver]` table; there is no terminal path that bypasses it.
- The web surface and the model-tool surface share the same
  `WorkDriverCaller`; a future third entry point (such as a TUI) would have
  to use the same caller and the same approval rules, not open a path of
  its own.
- Trade-off: observation is possible only via `work_driver.status` or the
  web API, not interactively at the terminal; this is accepted on purpose
  as long as no need for a live dashboard is demonstrated.
- According to the guide, the job worker that actually processes a
  `work_driver` job is only partly in the tree; the decision holds for the
  shape of the surface regardless of the implementation status.

## Where in the code
- `harw-runtime/src/services.rs` — `WORK_DRIVER_JOB_KIND = "work_driver"`; `RuntimeServices::with_work_driver_caller` binds `Arc<WorkDriverCaller>` only to the model-tool and web surface.
- `harw-agent-dsl/src/lower_v2.rs` — `WORK_DRIVER_ENQUEUE_TOOL`/`WORK_DRIVER_STATUS_TOOL`/`WORK_DRIVER_STOP_TOOL`, `lower_work_driver`, `grant_work_driver_tools`: the three tools come into existence only if `[work_driver]` was lowered successfully.
- `harw-registry-defaults/src/roster.rs` — the clamp keeps `work_driver.enqueue/status/stop` for an orchestrator with a `[work_driver]` section and removes them otherwise (`custom_orchestrator_with_work_driver_keeps_its_three_tools_through_the_clamp`, `custom_orchestrator_without_work_driver_loses_the_tool_in_the_clamp`).
- `harw-registry-defaults/src/capability_catalog.rs` — `WORK_DRIVER_TOOLS`, `work_driver.enqueue` is classified as `Shell`, `.status`/`.stop` as `Meta`.
- `harw-ops/src/work_driver.rs` — `WORK_DRIVER_JOB_KIND`, `WorkDriverCaller`, the actual `enqueue`/`status`/`stop` operations behind the tools.
- `docs/guides/work-driver.md:65` — "There is no TUI slash command and no `harw` subcommand for the WorkDriver."; section 2 describes the model tool and `POST /api/work-driver/enqueue` as the only two ways to start it.

## Related
- [DEC-006 Model-agnostic](DEC-006-model-agnostic.md)
- [DEC-007 Worker rights](DEC-007-worker-rights.md)
