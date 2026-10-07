---
id: GAP-R15
title: Gap hunt R15 — patterns
status: living
date: 2026-09-27
tags: [gap-hunt, patterns, review, workflow]
related:
  - patterns.md
  - README.md
  - ../70-decisions/DEC-003-provider-limits.md
  - ../70-decisions/DEC-007-worker-rights.md
  - ../80-copilot-backlog/README.md
  - ../90-migration-ledger/MIGRATION_LEDGER.md
---

# Gap hunt R15 — patterns

Running notes on the gap hunt after R15. It is a workflow with
- 8 finder areas,
- 3 verification perspectives (reproduce / intent / scope),
- fixers that each edit only one file, and reviewers.

This note does not list the individual findings but the **patterns** behind them
and what we change in process and code as a result. The generally applicable
patterns have been carried over into the [pattern catalog](patterns.md); the
procedure is available as a reusable [kit](README.md).

## Patterns in the code

### M1 — The success path skips the check
The check sits in the error branch or the fallback instead of before the success
path. Evidence from round 1:
- **Coordinator:** `decide()` checks the sandbox requirement only on failure.
- **Judge fallback:** `parse_verdict` reads a negated "passed" as passed.
- **Evidence:** A failed `attach_evidence` after a green check is only
  logged.
- **Secrets:** The store relies on the umask instead of fixed permissions.
- **Audit chain:** Check first, then use (TOCTOU on the path).
- **Tenants:** `work_driver.enqueue` scans active runs across all tenants.

**Rule:** Checks come before the success path, and fallbacks refuse when in
doubt.

### M2 — The limit logic is spread across layers
Provider, retry, and worker each have their own caps and their own counting. Evidence:
- Pacing was cut off after 300 s.
- OpenAI holds the concurrency slot while waiting.
- A quota 429 got no cooldown.
- "A 429 costs no attempt" is lost on a restart.

No test suite checks the invariants from DEC-003 across all layers.

### M3 — Docs drift when sources change
After the switch from Git to crates.io, three places kept describing the
Git source:
- `Cargo.toml`
- `docs/architecture/dependency-review.md`
- `docs/architecture/crypto-drift-report.md`

On top of that came a premature "LANDED" in the ledger. Three finders reported
this independently of each other, each under a different category.

### M4 — Hotspot file
`harw-cli/src/job_worker_work_driver.rs` accounts for 7 of the first 29 findings. It bundles
verify, pacing, scope check, and the parsing of the judge verdict. With "one agent
per file", the file size serializes the work.

## Patterns in the process

### P1 — Fixes produce follow-up findings
More than half of the round 2 findings were caused by round 1 itself:
- docs not updated in the same file;
- `.expect()` in new tests;
- `pub` helpers without callers;
- performance regression (the whole workspace is re-read per block);
- half a quota fix.

Fixers solve the finding but do not clean up the edges. The reviewers checked only
the finding.

### P2 — The one-file rule produces half-infrastructure
If the clean fix needs two files, dead or unwired code results:
- a cgroup sweep that nothing calls;
- `preview_wait_for` without callers.

If the reviewer then demands the wiring, the fixer falls back to a
detour within the file (a detached `tokio::spawn` task in the executor).
That means scope creep: `linux.rs` +229 lines of production code for two findings.

### P3 — Test share is healthy, production share fluctuates
For fixes, 60–75 % of the new lines are tests. That is good. Outliers in
production code arise exactly at P2.

### P4 — The intent perspective is the most valuable naysayer
Almost all dissenting votes came from "intent":
- coordinator,
- router pacing,
- 300 s cap,
- telemetry doctests.

When it votes against, the finding is usually defense in depth rather than
an acute bug.

### P5 — Rules without tooling get broken
`expect`/`panic!` in tests and doctests reappear in every round.
Clippy does not check doctests.

### P6 — Throughput: the host is the limit, not the model
The container has 4 CPUs. The workflow runs at most CPUs − 2 = **2 agents
at a time**. Three verifiers per finding are the biggest item.

### P7 — Paths must be canonical
Finders reported paths mixed, absolute and relative. As a result, two
groups arose for the same file, and two fixers worked on
`linux.rs` at the same time.

## Workspace-wide hunt (8 areas, interim status)

The table shows unverified raw findings from the Opus finders, 345 in total, tagged
by pattern. Verification is still running.

| Pattern | Raw findings | Share |
|---|---:|---:|
| P5 Rule violation in tests/doctests | 100 | 29 % |
| M1 Fail-open / check after the success path | 85 | 25 % |
| NEW (see below) | 73 | 21 % |
| M3 Docs drift | 64 | 19 % |
| M2 Limit/counting logic across layers | 23 | 7 % |

By severity: 1 critical, 28 high, 95 medium, 221 low. The critical finding:
- `harw-config/src/merge.rs` — an untrusted `.harw` in the repo
  can enable full access and disable guards. This is M1 at the level of the
  configuration layers.

### New patterns from the breadth
- **M5 — Unbounded inputs:** Reads, lines, event buffers, and `await`
  run without an upper bound or timeout, often at trust boundaries (MCP, SSE,
  channels).
- **M6 — Unstructured concurrency:**
  - detached `spawn`s per event or SSE pump;
  - cancellation is not propagated, future state is lost;
  - a runner dies unsupervised;
  - an accept error terminates the listener.
- **M7 — Temp files:** Temp paths for executables are predictable
  or insecure.
- **M8 — Files shared by several processes:** Takeover of stale locks, writers
  without a lock.
- **P8 — Rules without tooling, part 2:**
  - public foreign types (about 8 findings);
  - let-chains despite MSRV 1.85;
  - unused dependencies and dead feature flags;
  - a book or author reference.

  As with P5, the rule is documented but not enforced.

### What follows from this
- **Tools before humans:** P5 and P8 together account for over a third of all
  findings. An `xtask` gate checks:
  - `.unwrap(`/`.expect(`/`panic!(` in tests and doctests;
  - let-chains;
  - foreign types in `pub` signatures;
  - book and author references.

  In addition there are `cargo-machete`/`udeps` for unused dependencies and an
  MSRV check (`cargo +1.85 check` or clippy `incompatible_msrv`). This
  eliminates an entire class of findings permanently.
- **M1 is architecture, not an isolated bug:** Every area has such findings.
  We need a rule and a review item of its own: "Guard before the
  success path, default refuses, trust layer explicit."
- **M5 and M6 at trust boundaries** belong in a shared helper layer:
  bounded reads, timeouts, supervised tasks. Today every site implements
  this itself.

## What has already changed in the workflow (from round 2)
- **Paths:** normalized (P7). Deduplication ignores the category (M3).
- **Finders:** run on Opus.
- **Fixers for `critical`/`high`:** Opus.
- **Checklist for fixers and reviewers** (P1, P2):
  - update docs in the file;
  - no `expect`/`panic!` in new tests;
  - no API without callers;
  - no performance regression;
  - at most about 150 lines.
- **Verification:** Two perspectives verify first. The third decides only
  on disagreement (P6).
- **Closing step:** An Opus agent searches the entire diff for effects across
  file boundaries (P2, M3).

## Proposals for a follow-up round
1. **M1:** Rule in `.github/copilot-instructions.md` and in the review list,
   plus a dedicated finder "check after the success path".
2. **M2:** an invariant test suite for DEC-003. Examples:
   - no slot during a wait;
   - a 429 never costs an attempt, not even after a restart;
   - pacing is never shortened.
3. **M4:** split `job_worker_work_driver.rs` into modules:
   `verify`, `pacing`, `scope`, `verdict`.
4. **P5:** an `xtask` gate for `.unwrap(`, `.expect(` and `panic!(` in tests
   and doc comments.
5. **P2:** Automatically turn multi-file findings into a contract wave: a
   planner writes the contract, file agents work against it.
6. **P6:** Workflow parallelism depends on the host, not on the quota. The
   subscription supports 15–20 concurrent agents, the 4-CPU container only 2 per
   workflow. For large hunts there are three ways:
   - **Parallel split:** one workflow per search area, each with its own,
     disjoint set of files. The main session coordinates multi-file findings.
   - **Finders via the Agent tool** (up to 20 at a time), then verification
     and fixing in the workflow.
   - **A cloud environment with more CPUs** (see C-10 "Cloud Home").
