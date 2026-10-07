---
id: DEC-002
title: Judge — cheap reads, capped expensive output
status: accepted
date: 2026-09-27
tags: [decision, work-driver, cost, judge]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../guides/work-driver.md
---

# DEC-002 — Judge — cheap reads, capped expensive output

## Decision
The judge in the work driver runs as its own internal worker
(`InternalModelPoint::WorkDriverJudge`) with its own provider/model choice,
without tools. It may read a lot in the background (goal, criteria,
evidence), because across rounds most of that comes from the provider cache;
only the output is cost-relevant, and it is capped at minimal answers
(`{"passed": false}` in case of doubt) as well as a fixed upper limit of 256
output tokens. Currently the judge runs once per completed wave, after the
central verification (`decide` has no input for individual worker verdicts
within a wave); a call after every worker result is covered by the cost logic
and cheap, but only makes sense once `decide` consumes per-worker verdicts —
that is possible, but not yet active.

## Why
- A separate `InternalModelPoint` slot instead of reusing the main model: its
  own, cheaper model/provider for a pure classification task, independent of
  the choice of the main worker.
- No tools: the judge rules once from the evidence at hand, with
  no follow-up questions, no tool overhead, and no additional round ping-pong.
- A stable prompt prefix (`JUDGE_INSTRUCTION` + criteria) over a
  continued session instead of rebuilding each round: the repeated portion hits
  the provider cache, and only the variable part (new evidence) is a real
  miss against the cache discount — reading is therefore effectively cheap.
- Output is the expensive part at most providers (output tokens cost
  more than cache reads), hence the hard limit `JUDGE_MAX_OUTPUT_TOKENS =
  256` and a minimal answer form: `{"passed": bool}` suffices, `comment`/
  `missing` are optional and terse.
- Fail-closed fits the cost logic: unclear/unreadable answer → `passed:
  false` (see DEC-001) instead of an expensive follow-up or a second
  attempt.
- A call after every worker result instead of only per wave would be covered by
  the cost logic (reading stays a cache read, only the output counts), but
  would have to stay equally cheap — otherwise the judge's cost load scales
  linearly with the number of worker rounds instead of the number of waves.

## Consequences
- The judge needs its own configuration slot
  (`work_driver_judge` in `harw-config`) with its own fallback (fast
  model of the active provider, no OpenRouter default without an explicit
  choice) — analogous to `AutoClassifier`.
- Trade-off: the 256-token limit forces terse `comment`/`missing`
  fields; a judge that is supposed to give detailed reasoning does not fit this
  budget and would need a different slot.
- Trade-off: even in the current shape (once per wave, after
  verification), cache reads plus capped output still mean
  non-zero cost per wave; the decision only shifts the cost to
  the cheaper side (reading instead of writing), it does not eliminate it.
- A switch to "after every worker result" requires that `decide`
  receives per-worker verdicts as input — that is a later extension,
  not a present one.
- Future changes to the judge instruction (`JUDGE_INSTRUCTION`)
  must preserve cache-prefix stability, otherwise the cost advantage from the
  reused prefix is lost.

## Where in the code
- [harw-config/src/internal_models.rs](../../../harw-config/src/internal_models.rs) —
  `InternalModelPoint::WorkDriverJudge`, `key()` (`"work_driver_judge"`),
  `description()`, `uses_openrouter_default()`, `openrouter_default_model()`.
- [harw-cli/src/job_worker_work_driver.rs](../../../harw-cli/src/job_worker_work_driver.rs) —
  `JUDGE_MAX_OUTPUT_TOKENS: u32 = 256`, `load_run_config` (resolves
  `InternalModelPoint::WorkDriverJudge` via `resolve_internal_model`,
  yields `RunConfig.judge`).
- [harw-plan-bridge/src/work_driver.rs](../../../harw-plan-bridge/src/work_driver.rs) —
  `JudgeVerdict`, `JUDGE_INSTRUCTION` (stable prefix, no tools, no
  follow-up questions).

## Related
- [DEC-001 Judge verdict `passed: bool`](DEC-001-passed-true.md)
- [DEC-003 Provider limits](DEC-003-provider-limits.md)
- [DEC-005 Small scopes, many waves](DEC-005-small-scopes-waves.md)
