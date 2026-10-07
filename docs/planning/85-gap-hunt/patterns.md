---
id: GAP-PATTERNS
title: Pattern catalog for gap hunts
status: living
date: 2026-09-27
tags: [gap-hunt, patterns, review, catalog]
related:
  - README.md
  - R15-patterns.md
  - ../70-decisions/DEC-003-provider-limits.md
  - ../70-decisions/DEC-007-worker-rights.md
---

# Pattern catalog

A reusable catalog for every gap hunt:
- Finders tag each finding with a pattern code.
- New patterns first come in as `NEW:<name>` and are added here as soon as
  they show up in more than one area.

Every entry has the same parts:
- **Recognize:** how a finder spots it.
- **Rule:** how the code does it correctly.
- **Tooling:** whether a gate can catch it permanently.

## Patterns in code

### M1 — Check after the success path (fail-open)
- **Recognize:**
  - The check sits in the `else`/error branch or in the fallback. Example:
    `if ok { return Succeed }` comes before the policy check.
  - Loose fallbacks: a free-text parser also reads "passed" in negated form.
  - The default permission wins.
  - An untrusted configuration layer can switch guards off.
  - File permissions are left to the umask.
  - Check first, use later (TOCTOU).
  - Errors are only logged where they should stop the flow.
- **Rule:**
  - The guard comes before the success path.
  - The default is deny.
  - The trust layer is explicit.
  - Permissions are set explicitly.
  - Check-then-use becomes open-then-check.
- **Tooling:** This is a review item. A dedicated finder with this lens
  pays off in every area.

### M2 — Limit and counting logic spread across layers
- **Recognize:** Provider, retry, and worker each have their own caps and
  counters.
  - A wait time is shortened.
  - A slot stays occupied during a wait.
  - The count is lost on restart.
- **Rule:**
  - Invariants live in one central place, see DEC-003.
  - Each layer only passes the values through.
  - State that must survive a restart is persisted.
- **Tooling:** an invariant test suite across all layers.

### M3 — Docs drift from the code
- **Recognize:**
  - Comments, guides, the ledger, or architecture documents describe the old
    state.
  - Especially after switching sources or dependencies.
  - Status claims arrive before the merge ("LANDED").
- **Rule:**
  - Whoever changes behavior greps the whole repo for the old term.
  - A status goes into the ledger only after the merge.
- **Tooling:** a final cross-file check (Opus) over the whole diff.

### M4 — Hotspot file
- **Recognize:** One file combines several responsibilities and collects a
  disproportionate number of findings.
- **Rule:** Split into modules. With "one agent per file", file size decides
  the parallelism.
- **Tooling:** Count findings per file and split the five largest first.

### M5 — Unbounded input at trust boundaries
- **Recognize:** `read_to_end`, line reads, event buffers, or `await` without
  an upper bound or timeout, especially in MCP, SSE, channels, and network
  code.
- **Rule:** Shared helpers for bounded reads and timeouts instead of
  per-site solutions.

### M6 — Unstructured concurrency
- **Recognize:**
  - detached `spawn`s per event or pump;
  - cancellation is not propagated, future state is lost;
  - tasks die unsupervised;
  - an accept error ends the listener;
  - a lock is held across `sleep`/`await`.
- **Rule:** Supervised tasks (JoinSet/handle with an owner), cancellation
  flows through, listeners survive errors on individual connections.

### M7 — Temp files
- **Recognize:** Predictable or insecure temp paths, especially for
  executables.
- **Rule:** `tempfile` with private permissions, nothing executable in shared
  temp directories.

### M8 — Files shared by several processes
- **Recognize:** Stale locks are taken over, writers work without a lock,
  history is not persisted.
- **Rule:** fs4 locks with a clear takeover rule, atomic writes
  (temp + rename).

### M9 — State without a terminal transition
- **Recognize:** A state has an entry but no safe exit. A job stays `Running`
  forever, a finished worker never releases its slot, a goal loop spins
  without a stop condition, an expired lease is never cleaned up.
- **Rule:** Every state machine names its terminal states. Every path,
  including failure, cancellation, and restart, leads into one of them. At
  startup, orphaned states are reconciled.
- **Tooling:** Tests per terminal transition, plus a restart test with a state
  that was left behind mid-run.

### M10 — Visible first, persisted later
- **Recognize:** A process makes a state visible (memory, event, response)
  before it is durably written. After a crash or restart, what clients have
  already seen is missing. Examples: memory before the directory fsync, a
  session ID held only in memory, an event bus without durable replay.
- **Distinction:** M8 concerns several processes on one file, M10 the order
  within one process.
- **Rule:** Write first, then make visible. Where that is not possible,
  "acknowledged but not durable" is its own error case that callers can tell
  apart.

### Map to a pattern instead of NEW
Finders tag many findings as `NEW:<name>` that already have a pattern:
- `unbounded-read`, `unbounded-line-read`, `unbounded-connections`,
  `unbounded-event-buffer`, `unbounded-cache-growth`, `unbounded-await`,
  `no-timeout`: **M5**.
- `detached-sse-pump`, `detached-per-event-spawn`, `cancel-not-propagated`,
  `dropped-future-state`, `lock-across-sleep`, `unsupervised-…-death`:
  **M6**.
- `insecure-temp-exec`, `predictable-temp-exec`: **M7**.
- `stale-lock-takeover`, `unlocked multi-process writer`: **M8**.
- `expired-lease-never-reconciled`, `non-idempotent-recovery`: **M9**.
- `unpersisted-history-mutation`: **M10**.
- `let-chain`, `third-party-type-in-public-api`, `unused-dependency`,
  `dead-feature-flag`: **P8**. `pub-without-caller`: **P1**.

The finder prompt therefore lists all codes with one recognition sentence
each.

## Patterns in process

### P1 — Fixes create follow-up findings
Fixers resolve the finding but do not clean up the edges:
- docs in the same file,
- `expect` in new tests,
- API without callers,
- performance regressions.

**Countermeasure:** a checklist for fixers and reviewers (see the kit).

### P2 — The one-file rule creates half-built infrastructure
If a fix needs two files, the result is dead or unwired code, or a detour
within the file.

**Countermeasure:** Multi-file findings go into a contract wave (one
contract, then one agent per file), not to individual fixers. Diff limit
around 150 lines.

### P3 — Test share
Healthy fixes consist of 60–75 % tests. Outliers in production code point to
P2.

### P4 — The intent angle
When "intent" votes against, the finding is usually defense in depth rather
than an acute bug. This helps with prioritizing.

### P5 — Rule violations in tests and doctests
`.unwrap(`, `.expect(`, and `panic!(` in tests and doctests. Clippy does not
check doctests.

**Tooling:** an `xtask` gate.

### P6 — Throughput depends on the host
The workflow runs at most CPUs − 2 agents at the same time, **per
workflow**.

**Countermeasure:** a parallel split, that is, one workflow per area on
disjoint files.

### P7 — Keep paths canonical
Mixed absolute and relative paths produce duplicate groups and two fixers on
one file.

**Countermeasure:** Normalize paths to repo-relative before grouping.

### P8 — Rules without tooling, part 2
- public third-party types;
- let-chains despite the MSRV;
- unused dependencies, dead feature flags;
- book or author references.

**Tooling:** an `xtask` gate, `cargo-machete`/`udeps`, and an MSRV check.

### P9 — Measure the discriminating power of the verification angles
State of the workspace hunt with 8 areas, around 260 verified findings from
Opus finders:
- **reproduce** says "real" in 98.5 % of cases (257 to 4). With precise
  finders, this angle hardly discriminates.
- **intent** rejects 10 % (230 to 25). It is practically the only angle that
  discriminates (see P4).
- **scope** is rarely needed as a tie-breaker (13 times) and always agrees
  with "real".

**Countermeasure:** Keep measuring the yield per angle. Replace an angle that
never disagrees, or use it only for high severity. Example: intent first,
reproduce only for `critical`/`high`. In addition, add an adversarial
"exploitability" angle for M1 findings.

### P10 — Confidence per category
Share of findings that survive verification:

| Category | survives |
|---|---:|
| M3 doc drift | 98 % |
| M1 | 91 % |
| M2 | 91 % |
| P5 | 90 % |
| NEW | 91 % |
| high and critical findings | 100 % |
| low findings | 89 % |

**Countermeasure:**
- Direct verification effort to where findings get rejected, that is, low
  findings and NEW.
- M3 can be confirmed cheaply by grep instead of with three model verifiers.
- `critical`/`high` from Opus finders need a verifier for scope and risk
  (scope), but no existence proof.

### P11 — One workflow, one branch
With all fixers in the same working tree, nobody may commit while any fixer
is writing, the tree stays dirty for hours, and an abort at the session limit
leaves half edits sitting between finished ones.

**Countermeasure:**
- Every writing wave gets its own git worktree on its own branch and commits
  immediately after its review.
- An integration branch collects the waves by merge; the central build runs
  only there.
- Waves stay file-disjoint. A wave that touches files of an earlier wave
  starts only after that wave's merge.
- After an abort, continue the wave with the **unchanged** script (resume):
  finished agents come from the journal, failed ones run again. With a
  changed script, P12 applies.

The same principle underlies DEC-045 for harw itself: one clone per cell host
with a merge barrier instead of many writers on one workspace.

### P12 — Resume only hits the unchanged prefix
A resume serves only the **longest unchanged prefix** of agent calls from the
cache. From the first changed or new call onward, everything runs live, even
fixers and coders that were already done. Example: a wave is retrofitted with
a re-review step, and after the first new re-review all later fixers run a
second time. The edits then land twice or overwrite each other, and the
tokens are lost.

**Countermeasure:**
- Never resume writing waves on changed logic.
- Make up a missing step (such as the re-review of a repair) as a separate
  agent, with the contract, finding, and repair report from the journal.
- Check whether double writes have already happened: compare the files'
  mtimes with the resume time.

### P13 — Verify against the base, not against the fixed tree
If findings are verified after the fact while their fixes are already in the
working tree, the verifiers read the fixed code. They then reject genuine
findings with "already fixed". In R16 this hit three of six re-verified
findings. All three were real on HEAD.

**Countermeasure:** `gap-verify` receives `base`, the commit the findings
were made against. Verifiers read `git show <base>:<file>`. "Already fixed in
the working tree" counts as real and is reported as `fixed_in_tree`.

### P14 — The working directory travels along
When the main session switches into a worktree with `cd`, its working
directory travels with it. Every agent that starts afterward and knows only
"the current directory" works in the wrong checkout:
- Fixers write into the foreign worktree.
- Reviewers see an empty diff there and report "fix missing".
- The repair follows the path from the review text and writes the fix a
  second time, again in the wrong tree, even if it starts in the right
  directory itself.

In R16 this hit two short `cd` windows. About 15 files from seven waves were
affected, including a duplicate, byte-identical fix and two different fixes
for the same finding.

**Countermeasure:**
- The main session never switches into a worktree with `cd`, but uses
  `git -C <path>` and absolute paths.
- Writing workflows require `root`, even for the main tree. Without `root`,
  `gap-fix` and `contract-wave` abort.
- A wave commit takes only the wave's named files. Foreign changes in the
  worktree are reported and reconciled after all waves have ended: if the
  main tree has no fix, it is moved over; if it is identical, it is
  discarded; if the two differ, a review decides.

### P15 — Agents write their assignment into the code
Fixers carry words from their assignment into comments: "Covers the brief's
three cases" or "see the fixer agent's report". The reader finds neither the
assignment nor the report, and the number in the sentence is often not even
correct.

**Countermeasure:** The fixer rules forbid references to assignment,
contract, report, wave, or agent in code and docs. An `xtask` gate searches
for these words.

### P16 — Ripple converges, but not to zero
Each ripple round finds less, almost only docs. Now and then, though, there
is a real error in it that the ripple fix itself introduced. In R16 the first
round after `wa-egress` found 11 items, the second 9, including a new
behavioral error (an idle counter counted from connection start). After
`r16-w4` there were 14 items, two of them HIGH.

**Countermeasure:**
- A ripple finding blocks the wave (`rippleStatus = findings`).
- The second round runs as a contract wave whose Opus verification takes
  over the cross-check. No further ripple run follows.
- Pure doc items are collected rather than hunted one by one.

### P17 — The same solution three times
`harw-authority`, `harw-security-hub`, and `harw-tool-plan` each built safe
file opening (no symlink, no FIFO, size, owner) themselves, although
`harw_fsutil` offers it. Three crates had the same bug (reading the euid as
the owner of `/proc/self`). The cause is the one-file rule: the fixer may not
change `Cargo.toml` and therefore rebuilds it locally.

**Countermeasure:**
- Finder and fixer prompts name the workspace helpers.
- If a fix needs a new dependency, it goes into a contract wave.

### P18 — Repair without a blocking finding
`contract-wave` started a repair agent for every problem the reviewer named.
This also applied after `ok=true` and to files outside the cluster that the
reviewer had expressly marked as "for information only". Nobody ever
re-verified these changes: in `ripple-authz` it hit three foreign files,
including 69 lines of code.

**Countermeasure:**
- Repairs run only after `ok=false` and only on declared files.
- Hints belong in a separate `notes` field.
- A blocking problem outside the cluster ends the cluster as `unresolved`.

### P19 — The session limit is a shared point of failure
A single fan-out of many parallel waves (w1–w8, w4-2, w8-2, ripple-web,
pr-baseline, blocker cards, field report) ran into the account's session
limit at the same time. The fixers were mostly done, while the late stages
(review, repair, ripple) died. What remained were many changes in the tree
that lacked exactly the verification.

**Countermeasure:**
- Parallelism is set by the token budget, not by CPUs: `maxParallel`
  (default 3) limits files or clusters in flight.
- A wave is completed in full (review, commit, manifest) before the next one
  starts.
- A stage without an answer never counts as passed.

### P20 — More agents do not mean more throughput
Every follow-up workflow (`-2`, ripple, `contract-c-2`) existed because its
predecessor was incomplete. Coordination costs grew faster than progress.
Almost every error from P12–P19 sits at a handoff between agents, not in the
program logic.

**Countermeasure (working method from harw's goal/plan model):**
- Every writing wave serves **one** goal with acceptance criteria and
  invariants and runs against **one** pinned base.
- A wave is `complete` only when a goal check reports `met` for every
  criterion, with evidence (file:line or test name). Contradictions between
  code and planning documents are reported as `deltas`, never resolved
  silently.
- A critical or high finding counts only with a regression test that the
  (re-)review names.
- The wave never declares its goal achieved; `achieve` stays with the human.
- The central build runs against a frozen SHA and is recorded for that
  (`kit/gate_record.py`). Any change afterward invalidates the run.

### P21 — Decided questions come back as questions
The secrets wave stopped twice before coding. The second time, five
decisions by the user had already been made, but they appeared only as prose
in the goal and finding. The planner read them as open points, partly asked
the same questions again, and set `feasible=false`.

**Countermeasure:**
- Decisions already made go into the wave as a separate `decided` field and
  appear in the contract prompt expressly as settled.
- Only a new, unforeseen blocker may trigger `feasible=false`.

### P22 — An agent delivers a dummy
When resuming the mobile TUI wave, a freshly started contract agent returned
a dummy instead of a contract: the file `a` and the summary `test`. The
schema was formally satisfied.

**Countermeasure:** The hard gates before every coder. The declared file set
checked against the contract set caught the dummy as `contract-mismatch`
before a single line was written. Gates check contents against requirements,
not just the form.

### P23 — New files are invisible to `git diff`
Reviews and goal checks read `git diff <base> -- <files>`. Newly created,
still untracked files (`auth_migrate.rs`, the whole new crate
`harw-tui-layout`) do not appear there; a review would have missed them, and
a goal checker took a finished crate for empty.

**Countermeasure:** Review, re-review, and goal prompts list new files via
`git status --short --untracked-files=all` and read them directly.
