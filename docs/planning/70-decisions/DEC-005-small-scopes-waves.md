---
id: DEC-005
title: Small scopes, many waves
status: accepted
date: 2026-09-27
tags: [decision, work-driver, cost, scheduling]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../harw-plan-bridge/src/work_driver.rs
  - ../../harw-provider-http/src/cache_strategy.rs
---

# DEC-005 — Small scopes, many waves

## Decision
Each worker gets the smallest possible isolated `WorkScope` — normally one
file, at most a tightly bounded directory — instead of large multi-file
assignments. Workers are short-lived: progress comes from many waves
(`drive_settled_wave`/`drive_running_wave`), not from a few long runs.
Contract data (scope, criteria, handoff text) lives in the fields of
`WorkScope`/`WorkerState`, not in the coordinator's context.

## Why
- Small `owned_paths` minimize overlap between workers
  (`scopes_overlap`, `first_path_overlap` in `work_driver.rs`) and therefore
  merge conflicts and blocked waves.
- Short-lived workers with a narrow scope keep the context per run small;
  `continue_or_respawn` respawns only once `context_tokens_used` exceeds the
  `respawn_context_tokens` threshold (default 150,000) — small scopes hit this
  threshold less often.
- Continuing the same worker over several rounds (instead of spawning anew
  each time) keeps the prompt prefix stable and therefore cacheable
  (`resolve_cache_strategy`, `CacheStrategy::ImplicitPrefix` /
  `ExplicitEphemeral` in `harw-provider-http/src/cache_strategy.rs`); a
  respawned worker loses this cache and pays for the prefix again.
- Many small waves instead of a few large ones keep central verification
  (DEC-004) cheap: each wave checks only a few clearly delimited changes, and
  failures are easy to attribute to a scope.
- Contracts in files (scope definition, handoff text from `handoff()`) rather
  than in the coordinator context keep the driver itself low on state and make
  respawns lossless: the successor receives exactly the same contract again.

## Consequences
- On every new wave, the scheduler must check whether open criteria still
  allow uncovered, non-overlapping scopes; this is more bookkeeping than a
  single large assignment, but pays off in less rework.
- Trade-off: very small scopes produce more waves and therefore more
  verification runs overall — DEC-004 limits this with `verify.lock` to one
  run at a time, not to fewer runs in total.
- Respawns due to a context limit or a hard error must carry the full contract
  (scope, open criteria, progress so far) in the `handoff` text, since the new
  process otherwise knows nothing about its predecessor.

## Where in the code
- `harw-plan-bridge/src/work_driver.rs` — `WorkScope` (`owned_paths`,
  `criteria`), `scopes_overlap`, `first_path_overlap`, `drive_settled_wave`,
  `drive_running_wave`, `continue_or_respawn`, `respawn_context_tokens`,
  `handoff()`.
- `harw-provider-http/src/cache_strategy.rs` — `resolve_cache_strategy`,
  `CacheStrategy::{ImplicitPrefix, ExplicitEphemeral}`,
  `apply_chat_cache_control`, `apply_messages_cache_control`.

## Related
- [DEC-004 No parallel builds](DEC-004-no-parallel-builds.md)
- [DEC-007 Worker rights](DEC-007-worker-rights.md)
- [DEC-006 Model-agnostic](DEC-006-model-agnostic.md)
