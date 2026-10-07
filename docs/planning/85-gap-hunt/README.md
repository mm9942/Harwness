---
id: GAP-HUNT
title: Gap hunt — reusable kit
status: living
date: 2026-09-27
tags: [gap-hunt, workflow, reuse, patterns]
related:
  - patterns.md
  - R15-patterns.md
  - ../70-decisions/README.md
  - ../80-copilot-backlog/README.md
  - ../90-migration-ledger/MIGRATION_LEDGER.md
---

# Gap hunt

The gap hunt is a reusable procedure in which many short-lived agents find,
verify and fix gaps across the whole workspace, without any agent building.
It emerged from round R15.

| File | Purpose |
|---|---|
| [patterns.md](patterns.md) | Pattern catalog (M1–M10, P1–P18). The finders tag findings with it. |
| [R15-patterns.md](R15-patterns.md) | Run log R15: numbers, observations, lessons |
| [kit/workflows/gap-hunt-area.js](kit/workflows/gap-hunt-area.js) | Search and verify for one area, read-only |
| [kit/workflows/gap-fix.js](kit/workflows/gap-fix.js) | Fix, review and repair for a disjoint set of files, plus the cross-file check |
| [kit/workflows/gap-verify.js](kit/workflows/gap-verify.js) | Re-verification of existing findings (staged), optionally with a completeness critic; also for field reports |
| [kit/workflows/contract-wave.js](kit/workflows/contract-wave.js) | Multi-file findings per cluster: Opus contract, one coder per file, Opus cluster review, repair |
| [kit/skills/gap-hunt/SKILL.md](kit/skills/gap-hunt/SKILL.md) | Procedure for the orchestrating session |
| [kit/wave_manifest.py](kit/wave_manifest.py) | Writes a wave's immutable manifest to [waves/](waves/) |
| [kit/tests/workflow-gates.test.js](kit/tests/workflow-gates.test.js) | Tests of the completion gates (`node …`, no dependencies) |
| [kit/agents/](kit/agents/) | `focused-explorer` (read-only) and `focused-coder` (one file, never builds) |

## Why this procedure (decisions)
- **One agent per file, nobody builds:**
  - Parallel builds fill the disk and check states that never exist.
  - See the build rule in `CLAUDE.md`.
- **Read-only search kept separate from fixing:** Many search workflows can
  run at the same time without getting in each other's way. Writing happens
  only on disjoint sets of files.
- **Staged verification by pattern and severity (P9/P10), tie-break only
  on disagreement:**
  - M3 (doc drift): intent only, confirmed by grep.
  - critical/high: intent and scope; for M1 additionally "exploit"
    (exploitability).
  - Everything else: intent and reproduce, scope as the tie-break.
  - `args.verify = 'classic'` switches back to reproduce and intent for
    every finding.
  - Because the host limits parallelism (P6), this saves verifications exactly
    where findings are almost never rejected.
  - Every finding carries `votes` with perspective and verdict, so that P9 can
    keep being measured.
- **Opus for searching and critical fixes, Sonnet for verifying and small
  fixes:**
  - The search needs depth.
  - Staged cross-checking catches false alarms cheaply.
- **Parallel partitioning:**
  - The limit on concurrent agents applies per workflow, at CPUs − 2.
  - Several top-level runs on separate areas use up the quota,
    a single large run does not.
- **One workflow, one branch (P11):** Each wave works in its own
  git worktree on its own branch (`root` argument) and commits right after
  its review. An integration branch collects the waves by merge; only there
  does the central build run. The main working tree stays clean.
- **Multi-file findings as a contract wave:** Individual fixers otherwise
  produce half-built infrastructure (P2).
- **Completion only through hard gates:** `complete` comes from fixed code, not
  from a model report. A ripple finding blocks the wave, a contract
  must cover exactly the declared files, and every merged wave has
  an immutable manifest in [waves/](waves/).

## Installation (local, not versioned)
`.claude/` is deliberately not part of the project, via `.gitignore`. The kit
therefore lives here and is linked locally:

```sh
mkdir -p .claude/workflows .claude/skills .claude/agents
ln -sf ../../docs/planning/85-gap-hunt/kit/workflows/gap-hunt-area.js .claude/workflows/
ln -sf ../../docs/planning/85-gap-hunt/kit/workflows/gap-fix.js       .claude/workflows/
ln -sfn ../../docs/planning/85-gap-hunt/kit/skills/gap-hunt           .claude/skills/gap-hunt
ln -sf ../../docs/planning/85-gap-hunt/kit/agents/focused-explorer.md .claude/agents/
ln -sf ../../docs/planning/85-gap-hunt/kit/agents/focused-coder.md    .claude/agents/
```

After that, the workflows can be started by name, for example
`gap-hunt-area` with `{key, area, crates, exclude}` or `gap-fix` with
`{key, findings}`.
