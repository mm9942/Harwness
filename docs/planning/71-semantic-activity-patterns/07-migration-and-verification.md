# PL-71 / 07 — Migration and Verification

> Status: DRAFT  
> Parent: [README.md](README.md)

## Migration principle

Do not rewrite the existing TUI around a speculative pattern engine.

Wrap, test, migrate, then retire local grouping code.

## P0 — docs/contracts

This planning PR.

Deliver vocabulary, invariants, initial patterns, macro seam, integration map and CURRENT annotations. No behavior change.

## P1 — shared vocabulary

Add minimal shared types:

- ActivityDescriptor
- ActivityKey
- PatternId
- PatternRelation
- PatternInstance metadata

Architecture gates verify that shared vocabulary does not depend on TUI/application layers.

## P2 — current ToolGroupCell adapter

Model current consecutive read grouping as `file-read-burst@1`.

Behavior remains visually equivalent before new patterns land.

## P3 — resource-keyed filesystem activity

Add file-edit-read-loop, normalized path key, range/byte counters, compact in-place projection and expansion to raw calls.

This is the first user-visible improvement.

## P4 — macro metadata

Extend `#[tool]` with optional activity metadata and migrate filesystem tools first.

Hand-written tools continue through explicit descriptors.

## P5 — jobs

Add job-lifecycle pattern/projection.

Verify WorkId rebasing, status/log/wait consolidation, terminal visibility and raw-call inspection.

## P6 — agents

Add repeated-agent-role-failure grouping.

Keep unseen or unique failures individually prominent. Group only when normalized failure class and scope match.

## P7 — graph / derivation

Add higher-level composition such as compile-fix-cycle and implementation-iteration.

Every derived instance exposes provenance.

## P8 — AI proposals

Generate proposals on demand over recorded traces.

Acceptance requires examples, counterexamples, deterministic matcher candidate, replay test and no authority changes.

## Verification matrix

### Replay

```text
first reduction == replay reduction
```

### Security

- unknown metadata → standalone;
- matcher failure → standalone or visible error, never hidden event;
- pattern state cannot mint Permission or Authority;
- expansion reveals failed and mutating constituents.

### UI

- repeated reads update one compact row;
- edit+read loop stays one resource projection when allowed;
- different paths separate;
- terminal projections stop showing running state;
- resize/re-render does not duplicate semantic rows.

### Agents

- same-role same-error runs may aggregate;
- different errors do not aggregate;
- active child remains individually visible;
- expanded group exposes concrete IDs.

### Jobs

- start/status/log/wait consolidate;
- unrelated WorkIds never merge;
- rebase from call id to WorkId is collision-safe.

## Acceptance criteria

1. TUI no longer owns the only definition of read grouping.
2. Raw events remain complete and inspectable.
3. Resource-keyed file edit/read activity updates in place.
4. Agent and job projections use the same shared semantic vocabulary.
5. Built-in patterns have stable IDs and deterministic replay tests.
6. Macro metadata is descriptive and authority-independent.
7. Unknown or unclassified tools remain safely visible.
8. Pattern derivation exposes provenance.
9. Skills can reference patterns without gaining authority.
10. AI proposals cannot auto-install privileged behavior.

IMPLEMENTATION STATUS: planning only.
