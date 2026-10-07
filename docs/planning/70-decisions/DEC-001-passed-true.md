---
id: DEC-001
title: Judge verdict `{"passed": bool}` — test convention, fail closed
status: accepted
date: 2026-09-27
tags: [decision, judge, work-driver]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../harw-plan-bridge/src/work_driver.rs
  - ../../harw-cli/src/job_worker_work_driver.rs
---

# DEC-001 — Judge verdict `{"passed": bool}` — test convention, fail closed

## Decision
The judge's verdict is `{"passed": bool, "comment": string, "missing": [string]}`.
`passed` follows the established test convention (`true` = passed) exclusively and
carries no separate, deviating semantics. Any response that cannot be read
unambiguously as a verdict counts as `passed: false` (fail closed) — never as `true`.

## Why
- Test frameworks, CI gates, and humans all read `passed: true` as
  "criterion met"; a custom meaning (e.g. inverted or multi-valued)
  would turn every integration into a source of errors.
- The judge is a small internal worker without tools and without follow-up questions
  (see DEC-002); it must be able to answer concisely and unambiguously without
  callers needing additional case distinctions.
- Models do not always return valid JSON or the expected field. Without a
  clear fail-closed rule, a broken or unreadable verdict would optimistically
  pass as "passed" and silently hide errors.
- Older field names (`met`, `verified`, `rationale`) must remain readable
  without changing the core semantics of `passed`.

## Consequences
- `JudgeVerdict::passed` is the only truth value the work driver evaluates for
  the judge criterion; `comment`/`missing` are explanatory extras only and
  flow into the next round as feedback, not into the decision.
- Tolerant parsing (`parse_verdict`) first tries a JSON object with
  `passed`/`met`/`verified`, then a plain-text marker `PASSED`/`FAILED`
  (`FAILED` wins if both appear), and otherwise falls back to `passed: false` with the
  raw text as the comment — there is no path on which unreadable text
  becomes `true`.
- Trade-off: a judge that writes only "looks good" without JSON, for example,
  is scored as not passed, even if it meant to agree on substance.
  This is a deliberate choice so that, in case of doubt, a false positive is never scored.
- New callers must not reinterpret or invert `passed` themselves;
  any extension of the verdict (additional fields) must follow this convention.

## Where in the code
- `harw-plan-bridge/src/work_driver.rs` — `struct JudgeVerdict` (field `passed`,
  alias `met`), `const JUDGE_INSTRUCTION` (requires exactly `{"passed": bool, ...}`).
- `harw-cli/src/job_worker_work_driver.rs` — `fn verdict_from_object` (reads
  `passed`/`met`/`verified`), `fn parse_verdict` (tolerant, fail closed: no
  recognizable verdict → `passed: false` with raw text as the comment).
- `harw-ops/src/work_driver.rs` — `last_judge: Option<JudgeVerdict>` as the
  stored state of the most recent evaluation.

## Related
- [DEC-002 Judge cheap reads](DEC-002-judge-cheap-reads.md)
- [DEC-006 Model-agnostic](DEC-006-model-agnostic.md)
