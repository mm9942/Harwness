# PL-71 / 03 — Runtime and Projections

> Status: DRAFT  
> Parent: [README.md](README.md)

## Runtime split

The semantic layer should have three independent responsibilities:

```text
ingest
  RawEvent → normalized ActivityEvent

match/reduce
  ActivityEvent → PatternInstance updates

project
  PatternInstance updates → consumer-specific views
```

Do not combine rendering with matching.

## Append-only source, mutable derived state

Canonical event history remains append-only. Derived state is mutable.

```text
event #1 ─┐
event #2 ─┼─► reducer ─► projection revision 4
event #3 ─┘
```

A projection carries a revision so consumers can replace stale views safely.

## Suggested event envelope

Conceptual only:

```rust
struct ActivityEvent {
    event_id: EventId,
    occurred_at: Timestamp,
    owner: ActivityOwner,
    descriptor: ActivityDescriptor,
    resource: Option<ActivityKey>,
    lifecycle: LifecycleState,
    payload: ActivityPayload,
}
```

Adapters should translate existing events rather than force every subsystem to emit a parallel event immediately.

## Projection revision

```rust
struct ProjectionUpdate<T> {
    id: ProjectionId,
    revision: u64,
    state: T,
}
```

Consumers ignore stale revisions. This permits safe in-place update even if events are processed asynchronously.

## Determinism rules

- Time is evaluated only from event time (`occurred_at`), never from the wall
  clock inside a reducer. A timeout break is a synthetic tick event in the
  stream, so it is logged and replayable.
- Reducer input is one totally ordered stream. Order is defined by an
  `ingest_seq`: a single sequencer (the writer of the canonical event log)
  assigns a unique, gapless, monotonic number to each event when it is
  appended. Live ingestion and replay both reduce in `ingest_seq` order, so
  there are no ties. `occurred_at` is informational and used only for time
  breaks. A late arrival gets the next `ingest_seq`, never an earlier slot;
  the reducer handles it as a normal event (it may reopen a quiescent
  instance if its scope allows). Per-producer `(source, seq)` is kept as
  provenance, not as the order.
- A rebase is itself an event, not a silent mutation.
- v1 requires `ProjectionId` stability: the id is assigned when the instance
  is created and never changes. A rebase adds an alias binding
  (`Call(C)` -> `Job(W)`) to the same projection and does not create a new
  projection. Replacement via a `supersedes` link or tombstone is reserved for
  a later version and needs a consumer contract (retire old row/message)
  before it is allowed. Revisions are derived from the replayable stream
  (`ingest_seq` of the last applied event), not from a process-local counter,
  so they survive reconnect.

## Rebase

Some identities are learned only after an operation returns.

```text
job.start call_id = C
    ↓ result
work_id = W
```

The runtime needs a controlled rebase:

```text
ActivityKey::Call(C)
      ↓ bind
ActivityKey::Job(W)
```

Rebase preserves provenance and must not merge unrelated provisional activities.

## Concurrency

Two calls against the same resource may overlap. Reducers represent concurrent subactivity rather than assuming a single sequential call.

```text
Read(A)#1 running
Read(A)#2 running
Read(A)#1 done
Read(A)#2 done
```

Projection:

```text
A · Read ×2 · 2 running
→
A · Read ×2 · complete
```

## Terminality

A PatternDefinition determines whether an instance is Open, Quiescent or Terminal. A quiescent instance may reopen if its scope permits it.

A job lifecycle can stay open across idle periods. A turn-scoped read burst closes at turn completion.

## Retention

Projection retention differs from raw event retention.

Active projections and recent terminal groups can be retained under bounded policies; old projections should be reconstructable from canonical observations where available.

Do not make TUI retention the audit-retention policy.

## Replay

Deterministic reducers enable:

- TUI reconstruction after reconnect;
- Web reconstruction;
- matcher debugging;
- regression tests on recorded histories;
- evaluating candidate PatternDefinitions.

Replay is a first-class requirement.

## Consumer hints

Shared pattern state may carry neutral presentation hints such as severity, phase, progress fraction, primary label, counters and expandable constituent count.

It must not carry ratatui styles, HTML or Telegram markup in shared infrastructure.

## Failure visibility

Aggregation must not hide failure.

- any failed constituent MUST be visible in the compact state as a failure
  count or badge, including mutating constituents;
- failure counts remain visible in compact form;
- expansion reveals individual failed events;
- unrelated failures never merge merely to save rows.

## Current migration anchor

CURRENT `ToolGroupCell` already acts as a simple reducer:

```text
Vec<SharedToolCell> → tally → one collapsed line
```

Migration should wrap or translate that behavior first rather than delete it.

## Implementation stages

1. model ActivityDescriptor and ActivityKey;
2. adapter for current TUI tool events;
3. implement file-read-burst reducer;
4. render through a ToolGroupCell-compatible surface;
5. add file-edit-read-loop;
6. add job lifecycle;
7. add agent failure grouping;
8. extract reusable projection transport.

IMPLEMENTATION STATUS: planning only.
