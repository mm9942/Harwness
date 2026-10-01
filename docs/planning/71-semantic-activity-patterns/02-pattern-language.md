# PL-71 / 02 — Pattern Language

> Status: DRAFT  
> Parent: [README.md](README.md)

## Goal

Define a small deterministic language capable of expressing useful Harw activity patterns without embedding arbitrary executable code into configuration.

The first implementation should prefer a closed typed representation over an unrestricted scripting DSL.

## Pattern shape

Conceptual form:

```rust
struct PatternDefinition {
    id: PatternId,
    scope: PatternScope,
    key: KeyExpr,
    sequence: Vec<PatternTerm>,
    break_on: Vec<BreakCondition>,
    reducer: ReducerSpec,
    projection: ProjectionSpec,
}
```

## Primitive terms

Initial match terms:

```text
Event(tool = "fs.read")
Event(tool = "fs.edit")
Event(job_event = "status")
Pattern("file-read-burst@1")
AnyOf(...)
Repeat(term, min, max)
Optional(term)
```

No arbitrary regex over serialized events in v1.

## Field binding

A pattern may bind normalized fields from a known event schema.

```text
fs.edit(path = $p)
fs.read(path = $p)*
```

The matcher binds `$p` once and requires later occurrences to match the same canonical resource.

## Canonicalization

Resource keys must be canonicalized by the subsystem that owns the resource semantics.

- workspace-relative file paths reuse existing safe workspace/path resolution;
- WorkId and SessionId remain typed IDs;
- hosts reuse the normalized host vocabulary already used by network policy.

The pattern layer must not invent a second path-security resolver.

A resolver error (symlink loop, path outside the workspace, vanished file) makes
the event standalone: it is neither grouped nor dropped. Renames and symlinks
must never silently merge two resources into one group.

## Scope

Initial scopes:

```text
Call
Turn
Session
Job
Workspace
Process
```

A scope boundary closes the active instance unless the PatternDefinition explicitly allows continuation.

## Break conditions

Examples:

- different resource key;
- turn terminal event;
- unrelated mutation;
- timeout;
- explicit completion event;
- job terminal state;
- agent identity change.

Prefer explicit break rules over invisible heuristic timeouts.

## Reducers

A reducer converts matching observations into compact typed state.

```rust
struct FileActivityProjection {
    path: ResourcePath,
    reads: u64,
    edits: u64,
    searches: u64,
    last_range: Option<LineRange>,
    last_bytes: Option<u64>,
    failures: u64,
    state: ActivityState,
}
```

Reducers must be replayable:

```text
reduce(events[0..N]) == replay_from_zero(events[0..N])
```

## First built-in patterns

### file-read-burst@1

Match:

```text
fs.read(path=$p)+
```

Key:

```text
(session, turn, canonical_path)
```

State includes reads, last range, last byte count, running and failed counts.

### file-edit-read-loop@1

Match one or more writes/edits and subsequent reads/searches against the same resource in one bounded activity phase. Further edits of the same file may update the same instance when break conditions permit it.

### job-lifecycle@1

Match by WorkId across enqueue/start, status, logs, wait and terminal updates. The projection updates in place until terminal.

### agent-role-failure-burst@1

Match distinct concrete agent runs with the same parent scope, same role, same normalized failure class and bounded time/turn window. This is a compact view only; concrete SessionIds remain independently inspectable.

## Pattern composition

Higher-level patterns may consume lower-level PatternInstances.

```text
compile-fix-cycle@1 =
    file-edit-read-loop@1
    validation-run@1
    failure-diagnosis@1?
    Repeat(...)
```

Composition keeps macro patterns independent from raw tool names where possible.

## User/project definitions

A future TOML surface may allow declarative patterns, but built-ins should land first. Unknown fields fail closed and project-local pattern configuration cannot alter authority.

## AI proposal format

```rust
struct PatternProposal {
    candidate: PatternDefinition,
    examples: Vec<PatternExample>,
    counterexamples: Vec<PatternExample>,
    observed_count: u64,
    estimated_projection_reduction: f32,
    confidence: f32,
    rationale: String,
}
```

AI may emit proposals. Humans or trusted acceptance logic decide whether they become configured or compiled patterns.

## Tests

Required matcher tests:

- same-path reads merge;
- different paths do not merge incorrectly;
- break conditions close an instance;
- replay produces identical reducer state;
- malformed definitions fail closed;
- unknown tool semantics remain standalone;
- mutation patterns never hide an unrelated mutating call;
- pattern composition preserves provenance.

IMPLEMENTATION STATUS: planning only.
