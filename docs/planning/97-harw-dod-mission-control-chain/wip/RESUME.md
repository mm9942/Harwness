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

## Local consolidation verification (2026-10-29, E1b only)

The four E1b notes above describe the original snapshot, not the current
integration branch. Plain Bwrap root-workspace refusal, failed-output-open
attempt cleanup, and `HARW_VERIFY_BACKEND` documentation were already present
and their focused regressions pass. Report integrity needed one additional
execution-time guard: backend selection alone cannot guarantee Landlock is
actually applied, and direct backend construction bypasses selection.

The mapped inner policy now uses `hard_requirement()` regardless of the job's
`None`/`BestEffort`/`Required` choice. Missing or partial filesystem enforcement
therefore aborts before executing the job. Selection requires ABI >= 5 for
this composition (the project's full-enforcement target); auto-selection may
still choose plain Bwrap, while explicit composition selects no backend.
Existing `None`/`BestEffort` semantics can then run without a sandbox; `Required`
refuses. This does not produce a composed trampoline report. No policy
permissions were widened. The report remains outside job-granted paths.

Verification: all 23 executor mapping tests and 99 runtime unit tests passed
(3 existing environment-dependent tests ignored); the subprocess regression
checks that a best-effort/none job either runs with a protected report or is
refused before execution. Full privileged namespace/cgroup integration remains
host-dependent: the chain smoke test self-skipped because this environment has
no trusted Bubblewrap at the fixed paths. These checks do not mark the entire
E1b work package complete. The full `harw-job-exec` suite passed (26 unit and
12 subprocess tests); strict all-target Clippy passed for all three crates.
