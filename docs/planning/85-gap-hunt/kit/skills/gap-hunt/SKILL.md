---
name: gap-hunt
description: Workspace-wide gap hunt with many short-lived agents — parallel read-only area hunts (Opus finders + completeness critic + adversarial verify), consolidation, one-file fix batches on disjoint file sets, a contract wave for multi-file findings, one central build. Use when asked to find and fix gaps/bugs across the workspace or after a large round.
---

# Gap hunt

The pattern catalog is `docs/planning/85-gap-hunt/patterns.md`. The run logs are
`docs/planning/85-gap-hunt/R*-patterns.md`.

## Hard rules
- Subagents never build, and the build rule goes word for word into every
  agent prompt. The workflows already carry it.
- One file, one agent at a time. Paths are repo-relative (catalog P7).
- Only the main session builds, **once**, after all agents are done, following
  `CLAUDE.md`.
- One commit and one push per round, only when everything is green. The PR
  goes to `dev`.

## Steps
1. **Scout.** List the crates (`ls -d */ dod/crates/*/`) and cut them into 6–10
   **disjoint** areas. Paths that another process is working on right now go
   into `exclude`.
2. **Parallel cut** (catalog P6). The concurrency cap applies per workflow, at
   CPUs − 2. So start one `gap-hunt-area` run per area as its own top-level
   Workflow call, not nested with `workflow()`, because nested runs share the
   cap. Args: `{key, area, crates, exclude, verify?}`.
   Verification is tiered by pattern and severity (catalog P9/P10); pass
   `verify: 'classic'` to check every finding with reproduce and intent.
3. **Consolidate.**
   - Collect all `confirmed` entries and normalise paths.
   - Dedupe by file plus 20-line bucket.
   - Split into:
     - `oneFile == true` → fix batches;
     - `oneFile == false` → contract wave.
4. **Fix batches.** Group the single-file findings into **disjoint** file sets,
   for example by crate. Start one `gap-fix` run per set, in parallel with each
   other. Opus fixes critical and high findings; the lessons checklist (P1/P2)
   is built in.
5. **Contract wave for multi-file findings.** The main session writes a
   contract (signatures, call sites). Then one `focused-coder` agent per file,
   with the build rule in every prompt.
6. **Ripple.** Read the `ripple` items from the gap-fix results and fix the
   cross-file leftovers.
7. **Central build** as in `CLAUDE.md`, including fmt, clippy `-D warnings`,
   tests, gates, deny, DoD and actionlint. Fix red spots in the main session.
8. **Record.**
   - Add a run log `R<nn>-patterns.md` with the numbers per pattern.
   - Add new patterns (`NEW:*` that show up in more than one area) to the
     catalog.
   - Add a ledger entry.
   - Commit, push, PR to `dev`.

## Branches: one workflow, one branch (catalog P11)
- Cut one git worktree per writing workflow from the integration base, on its
  own branch (`<prefix>/<wave>`), under a locally ignored `.worktrees/`
  directory inside the repo, and pass its absolute path as `root`
  (`gap-fix`, `contract-wave`). Read-only workflows need no worktree.
- Waves must stay file-disjoint. A wave that touches files an earlier wave
  changed is cut after that wave was merged into the integration branch.
- When a wave finishes its review: commit in its worktree, push its branch,
  merge it into the integration branch (`--no-ff`), remove the worktree.
- Remove all worktrees before the central build: some gates walk the tree.

## Commit rhythm (batched at verified checkpoints)
- **When to commit:** at the **end of each workflow**, and in between whenever
  about **90 verifications** have finished since the last commit, counted
  across all running workflows. There must also be no fixer or repair agent
  writing. Counter: count the completed `verify:*` labels in all
  `journal.jsonl` files. Busy writers are `fix:*`/`repair:*` labels that
  started but have no result yet.
- **Planning and research:** once a note has passed its critic or review,
  commit it straight to `dev` (docs only).
- **Fix batches:** once the files have passed review (fixer → reviewer →
  repair if needed), commit them on the wave's own branch, push it and merge
  it into the integration branch. Do not commit them to `dev`.
- **Central build green:** open a PR to `dev` or fast-forward `dev`.
- **Rules:** Commit with `--no-verify`, because the pre-commit hook runs
  `git add -u`, and name the files explicitly. Never commit a file while a
  fixer is still writing to it.

## Watching during the run
- Read the journals under `…/subagents/workflows/<run>/journal.jsonl`:
  - `started` and `result` per label;
  - tally the patterns (`pattern` field) and severities.
- Stop and resume a workflow only while no fixer or repair is writing.
  Script changes must apply only to later rounds, so the earlier agent calls
  stay cached (same prompt and opts).
- Look for these warning signs:
  - two fixers on one file (P7);
  - diffs far above the finding's size (P2);
  - fixes that trigger follow-up findings (P1).
