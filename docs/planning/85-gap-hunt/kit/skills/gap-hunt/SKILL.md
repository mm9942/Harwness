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

## Completion gates: when a wave may merge
- **gap-fix:** `complete` is true only when every file is `ok` or
  `repaired` (repair plus re-review) **and** the ripple check is `clear`
  (answered, no item) or deliberately `skipped`. `findings` means the
  checker named cross-file problems: each item has a stable id
  (`<key>-R<n>`) and stays open until it is fixed and re-verified, moved into
  a contract wave, or rejected by verification. `missing` means no answer.
- **contract-wave:** the declared cluster files are the trusted input. The
  contract must list each of them exactly once (`change=false` for a file
  that needs no edit) and nothing else, or the cluster ends as
  `contract-mismatch` before any coder starts. Files the findings name
  outside the cluster are reported as `uncovered` and fenced off in the
  contract prompt. Repairs run only after `ok=false` and only on declared
  files; a blocking problem outside the cluster ends it as `unresolved`.
- **Commit only the named files** of the wave, never `git add -A`: foreign
  changes in a worktree come from agents in the wrong checkout (catalog P14)
  and are reconciled separately.
- **Manifest:** on top of the content commit, write the wave's immutable
  manifest with `kit/wave_manifest.py` (base SHA, branch, file set, finding
  ids, workflow run ids, content head SHA, disposition, central-build
  commands) into `waves/<wave>.json` and commit it on the wave branch before
  the merge. A re-cut wave gets a new name; a manifest is never edited.
- The gate logic has tests: `node kit/tests/workflow-gates.test.js`.

## Goal contract: Sonnet 5 waves (catalog P19/P20)
Writing waves (`gap-fix`, `contract-wave`) follow the goal/plan method that
Harw uses:
- **Goal and base are required args.**
  - `goal: {id, statement, criteria: [{id, text}], invariants?}`
  - `base`: the pinned base commit, 7–40 hex digits.
  - A wave without either refuses to start.
- **Invariants.** Every agent prompt carries the goal, its criteria and these
  fixed invariants:
  - code, tests and gates win over planning docs, and contradictions go into
    `deltas`, never reconciled silently;
  - only a state merged into `dev` is CURRENT;
  - no security-relevant change is done without a regression test and a
    passed re-review;
  - the wave never declares the goal achieved.
- **Goal check.** After the files or clusters and the ripple, a read-only
  goal check reads `git diff <base>` and answers every criterion with `met`,
  `not-met` or `unknown`. `met` needs evidence: a file:line or a test name.
  `complete` requires every criterion `met` with evidence, on top of the
  earlier gates.
- **Result.** The wave returns `coverage`, `deltas`, `goal`, `base`,
  `achieved: false` and `achieve: 'human-only'`.
- **Security gate.** A file or cluster with a critical/high finding counts as
  `ok`/`repaired` only when the (re-)review names the regression test in the
  diff (`regression_test`). Otherwise it ends `unverified`.
- **Models.** `models: 'sonnet'` is the default: every agent runs on Sonnet 5.
  `models: 'tiered'` restores Opus for contracts, critical/high fixes, review
  of contracts, and ripple.
- **Parallelism.** `maxParallel` (default 3) caps files or clusters in
  flight. Size it to the token budget, not to CPUs, so one session limit does
  not stop every wave in the same stage.
- **Manifest.** `wave_manifest.py` writes schema 2 with `goal` (criteria,
  coverage, deltas, `achieved: false`). It refuses a `--base` that is not the
  wave's pinned base.
- **Frozen gates.** After the last merge, freeze the integration SHA and run
  the central build there with a clean tree. Record it with
  `kit/gate_record.py record --repo DIR --sha SHA --goal ID fmt=0 clippy=0 …`,
  which writes an immutable `waves/gates-<sha12>.json`. Every step must be
  listed. `gate_record.py check` accepts the record only while nothing but
  gate records changed after the frozen SHA; anything else needs a full new
  run.
- **Coordination.** One writer per branch. Notes between sessions go through
  git (`docs/planning/85-gap-hunt/coordination/` on its own branch), not
  through chat.

## Layered waves: macro → leaf crates → wiring
Features that add or change several crates run as `layered-wave`, not as one
large contract wave:
- **Macro.** One read-only planner turns the goal and the design documents
  into units with dependencies. A **leaf** unit builds exactly one crate:
  every file lies under that crate's directory, and one crate appears in
  only one leaf unit. A **wiring** unit connects finished crates in the
  composition roots: harw-runtime assembly, harw-cli, harw-tui, harw-config,
  the root `Cargo.toml` and `xtask/arch-policy.toml`.
- **Plan gate.** The plan is rejected before any coder starts if a file sits
  in two units, a leaf file is outside its crate, a crate is split, a leaf
  depends on wiring, the dependencies form a cycle or name unknown units, or
  an acceptance criterion has no unit. The same happens when the plan still
  lists owner decisions.
- **Levels.** Leaf units run level by level in dependency order (Kahn), then
  wiring. Each level is one `contract-wave` (nested `workflow()`) whose goal
  has one criterion per unit. A level starts only when the previous one is
  complete, so a failed leaf is never wired.
- **Goal.** The real acceptance criteria are checked once over the whole
  diff against the pinned base; `achieved` stays human-only.
- **No builds.** As in every wave, agents never build. The main session runs
  **one** central build, sequentially, after every wave has finished, and
  fixes what is red itself.

## Owner decisions travel as data (catalog P21)
Pass decisions the owner already made as `decided: [..]` to `contract-wave`
and `layered-wave`. They appear in the contract/macro prompt as resolved, so
a planner does not stop the wave again on a question that has been answered.

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
  A resume replays only the longest unchanged prefix of agent calls (same
  prompt and opts); from the first edited or new call on, everything runs
  live, writers included. So never resume a writing wave onto changed logic:
  run the missing step (for example a re-review) as a separate agent
  (catalog P12).
- Never `cd` into a worktree from the main session: the session's working
  directory follows the `cd`, and every agent started afterwards without an
  explicit root works in that checkout (catalog P14). Use `git -C` and
  absolute paths, and pass `root` to every writing workflow, the main tree
  included.
- Verify findings after fixes landed only against the base commit they were
  found in (`gap-verify` option `base`): verifiers reading the fixed tree
  reject real findings as "already fixed" (catalog P13).
- Look for these warning signs:
  - two fixers on one file (P7);
  - diffs far above the finding's size (P2);
  - fixes that trigger follow-up findings (P1).
