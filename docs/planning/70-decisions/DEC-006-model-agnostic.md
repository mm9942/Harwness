---
id: DEC-006
title: Model-agnostic work driver
status: accepted
date: 2026-09-27
tags: [decision, work-driver, provider-abstraction]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../harw-plan-bridge/src/work_driver.rs
  - ../../harw-provider-http/src/tool_names.rs
---

# DEC-006 — Model-agnostic work driver

## Decision
The work driver assesses a worker's progress and state exclusively through
generic, provider-independent artifacts: result text, changed files within
its own `owned_paths`, the verification result, and the judge verdict. It
never judges based on tool names, the number of calls, or provider-specific
trace structure. Traces remain opaque and are passed on unchanged; the
handover on a respawn takes the form of a pure text handoff.

## Why
- Providers differ in tool-call semantics, trace formats, and permitted tool
  names — logic that depends on these breaks with every provider switch or
  update.
- Progress can be measured robustly only by what actually matters: the text
  result, the files actually changed within the worker's own scope, whether
  the central verification (DEC-004) passes, and the judge verdict
  (DEC-001).
- Tool names must satisfy provider-specific character rules (e.g. stricter
  wire formats than internal names); a reversible codec allows internal
  names to stay free-form (dots, longer names) and translates them only for
  transmission.
- A pure text handoff on respawn is readable regardless of provider and
  model and requires no knowledge of the predecessor's internal trace
  structure.

## Consequences
- New providers can be connected without touching the work driver's
  progress logic — only the tool-name codec and the provider integration
  itself have to satisfy the character rules.
- Trade-off: The driver cannot offer fine-grained, provider-specific
  diagnostics (e.g. "tool X was not called"); it deliberately relies on
  coarser, generic signals.
- The codec must resolve collisions after sanitization/truncation to 64
  characters unambiguously and remain lossless in both directions
  (encode/decode) — this must be re-checked with every change to
  `tool_names.rs`.
- Handoff texts are free text and must contain all information needed for a
  respawn (open items, state so far, permitted paths), since the successor
  has no structured access to the predecessor's trace.

## Where in the code
- `harw-plan-bridge/src/work_driver.rs` — `continue_or_respawn`, `handoff`
  (lines ~850–910): progress/respawn decision based on `owned_paths`, the
  judge verdict (`JudgeVerdict.passed`), and context size, not based on tool
  calls.
- `harw-plan-bridge/src/work_driver.rs` — `WorkDriveStep::Respawn { handoff, .. }`
  (line ~378ff.): the handoff is a plain `String`.
- `harw-provider-http/src/tool_names.rs` — `ToolNameCodec` (`register`,
  `encode`, `decode`, `sanitize`, `MAX_WIRE_NAME_LEN`): reversible codec
  targeting the strictest common rule `^[A-Za-z_][A-Za-z0-9_-]{0,63}$`.

## Related
- [DEC-001 Judge verdict passed:bool](DEC-001-passed-true.md)
- [DEC-004 No parallel builds](DEC-004-no-parallel-builds.md)
- [DEC-007 Worker rights](DEC-007-worker-rights.md)
- [DEC-008 No TUI](DEC-008-no-tui.md)
