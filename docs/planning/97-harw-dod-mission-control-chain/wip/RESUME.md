# PL-97 work in progress — resume notes (safety commit, 2026-10-29)

This directory holds the orchestration inputs of the PL-97 implementation
session. It was committed as a safety snapshot when the owner's usage limit
was nearly exhausted. Remove it in X1 once the plan is complete.

## Landed and verified (one commit per work package)

C1, D2, M1, D1, C4a, B4, E1a, B1, M2, E3b, plus the plan amendments (§8)
and the tracked harw-activity fixtures.

## In the safety commit, NOT yet verified as a whole

| Zone | State when stopped | What is left |
|---|---|---|
| C2 portrait windows (`harw-tui/src/mission/*`, `panes.rs`, `app.rs`, `app/render.rs`, keys, help) | code done, both reviews done, fix agent was running | re-run the C2 fix against the review findings, then fmt/clippy/tests for harw-tui |
| E1b BwrapTrampoline backend (`harw-job-runtime`, `harw-job-executor-bwrap`, `harw-sandbox/src/bwrap.rs`, `harw-command/src/port.rs`, `harw-cli/src/verify_sandbox.rs`, `Makefile`, `harw-install/src/uninstall.rs`) | code done, both reviews done (no blockers), fix agent was running | fix: report.json must not be job-writable when Landlock is missing; plain Bwrap must refuse a `/` workspace; attempt dir leak on output-open failure; doc/HARW_VERIFY_BACKEND mismatch |
| D3 `harw dod` CLI, `dod` operations, model tools, triage agents | code done, reviews were running | run both reviews and the fix; then B2 (queued after D3 in the same lane) |
| B3 `harw skill` CLI and `/skills preview|validate` | code done, both reviews done, fix agent was running | re-run the fix |
| E3c probe profile wiring + read-only Landlock root on the cgroup tree | coding agent was running | finish and review |

Security review findings (Mythos 5.1 session, separate PR from branch
`security/pl97-review-findings`): finding 1 fixed here; 4 and 5 must be
honoured by E1b/E2a; 7 goes to M3.

## How to resume

The Workflow runs are resumable from the same session only. In a new session
use `zone-runner.js` with the zone specs in this directory (`args.zones`,
optional `args.lanes`, `args.busy`). Zone order after this snapshot:
finish C2, E1b, D3, B3, E3c → B2, C3 ∥ C4, E2a → E2b, E3 (after D3), D4,
M3, D5, E4, C4b, E5, C5, B5, X1. Before each commit run the zone gates and
stage only that zone's files (Cargo.lock per package).
