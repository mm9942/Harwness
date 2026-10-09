# PL-71 / 01 — Vocabulary and Pattern Graph

> Status: DRAFT  
> Parent: [README.md](README.md)

## Purpose

This note fixes the vocabulary used by the rest of PL-71. The goal is to prevent "event", "activity", "pattern", "projection" and "skill" from collapsing into synonyms.

## Core terms

### RawEvent

An immutable runtime observation emitted by an existing subsystem.

Examples:

- `TurnEvent::ToolCallRequested`
- `TurnEvent::ToolCallCompleted`
- `AgentEvent`
- durable job state changes
- process/job supervisor events

Raw events are not rewritten merely because a higher-level pattern is recognized.

### ActivityDescriptor

Static semantic metadata attached to an event producer or operation family.

```text
domain   = filesystem
verb     = read
resource = argument:path
effect   = observe
```

This describes structural meaning. It is not a permission grant.

### ActivityKey

The normalized identity used for grouping observations.

```text
File("/workspace/src/lib.rs")
Job(work_id)
Agent(session_id)
Host("docs.rs")
Chain(work_id, step)
```

An ActivityKey is not necessarily the concrete event identity.

### PatternDefinition

A stable, versioned declaration describing which events or child patterns can form a semantic pattern.

Examples:

- `file-read-burst@1`
- `file-edit-read-loop@1`
- `job-lifecycle@1`
- `repeated-agent-role-failure@1`

### PatternInstance

A concrete match of one PatternDefinition over one bounded set of observations.

```text
file-edit-read-loop@1
resource = src/lib.rs
edits = 2
reads = 7
state = active
```

### Projection

A mutable consumer-facing view derived from one or more PatternInstances.

Examples include one TUI line, one Web card, one Telegram progress message or one compact agent-status summary. Projection is allowed to replace its own previous visible state.

### PatternGraph

The graph connecting PatternDefinitions through typed relations and PatternInstances through provenance/support edges.

### SkillProjection

A model-facing instruction fragment derived from one or more accepted patterns. It remains advisory instruction context and carries no authority.

## Identity model

Never overload one identifier for all semantics.

```text
EventId
  exact observation

ToolCallId / SessionId / WorkId
  concrete runtime identity

ActivityKey
  semantic resource/lifecycle identity

PatternInstanceId
  derived semantic instance

ProjectionId
  mutable consumer view identity
```

Example:

```text
ToolCallId("call-9")
    └─ ActivityKey::File("src/lib.rs")
          └─ PatternInstance(file-read-burst)
                └─ ProjectionId("tui:file:src/lib.rs")
```

## Pattern IDs

Recommended grammar:

```text
<domain>-<meaning>@<version>
```

Human labels may change; IDs remain stable references.

## Relation types

Initial graph vocabulary:

```rust
enum PatternRelation {
    Generalizes,
    Specializes,
    ComposedOf,
    Precedes,
    Follows,
    CorrelatesWith,
    DerivedFrom,
    Supersedes,
    AppliesTo,
    ProjectsAs,
}
```

`Generalizes` and `Specializes` describe conceptual hierarchy. `ComposedOf` links macro patterns to smaller patterns. `DerivedFrom` is provenance, not inheritance. `CorrelatesWith` is descriptive only and never implies causation or authority.

## Provenance

Every derived instance should be explainable through bounded provenance.

```rust
struct PatternEvidence {
    event_ids: Vec<EventId>,
    child_instances: Vec<PatternInstanceId>,
    first_seen: Timestamp,
    last_seen: Timestamp,
}
```

A compact projection may omit detailed provenance visually, but it must be retrievable.

## Confidence

Deterministic built-in patterns do not require probabilistic confidence. AI-discovered proposals may carry confidence before acceptance:

```text
PatternProposal
  confidence = 0.94
  observations = 318
  counterexamples = 7
```

After acceptance, the first runtime matcher remains deterministic unless a later design explicitly introduces probabilistic matching.

## Pattern lifecycle

```text
candidate
   ↓ review
accepted
   ↓
active
   ↓
superseded / deprecated
```

Pattern versions are immutable once accepted. Semantic changes create a new version.

## Authority boundary

None of these are authority tokens:

- PatternDefinition
- PatternInstance
- ActivityKey
- PatternEvidence
- SkillProjection
- AI confidence

A pattern may explain that a privileged operation occurred or is likely next. Existing authority and approval systems still decide whether it may occur.

## CURRENT / PLANNED

CURRENT:
- concrete runtime IDs exist;
- TUI has mutable per-tool/per-agent state;
- no generic PatternDefinition or PatternGraph type exists.

PLANNED:
- stable semantic IDs;
- typed relations;
- provenance;
- versioned pattern graph.

IMPLEMENTATION STATUS: planning only.
