# PL-71 — Semantic Activity Patterns

> **Status:** DRAFT / planning only  
> **Pinned baseline:** `mm9942/Harwness@153da05518cfd2cceb2cda738f9417cdf1653035` (`dev`)  
> **Scope:** semantic activity patterns, derivation graph, live projections, macro metadata, job/agent/tool/chain integration  
> **Non-goal:** changing runtime authority or deleting raw events

## 1. Thesis

Harw already emits enough low-level events to describe what agents, tools and jobs are doing, but most consumers still render those events close to their raw shape.

That is correct for audit, replay and debugging. It is not always the best representation for a human or for another agent.

Repeated activity such as:

```text
fs.edit(path=A)
fs.read(path=A)
fs.read(path=A)
fs.read(path=A)
```

is often one semantic activity:

```text
A · Edit ×1 · Read ×3
```

Likewise:

```text
job.start -> job.status -> job.status -> job.logs -> job.wait
```

is one job lifecycle, and several same-role child failures may be one repeated failure pattern in a compact panel while remaining distinct runs in the detailed view.

The target is therefore not "smarter rendering". The target is a reusable semantic layer:

```text
declarations / tool metadata
            ↓
        raw events
            ↓
      pattern matcher
            ↓
      pattern instances
            ↓
      pattern graph
            ↓
 mutable projections
    ↓       ↓       ↓
   TUI     jobs    agents
   web     chains  analysis
   chat    skills  DoD correlation
```

Raw events remain append-only and authoritative as observations. Pattern projections are derived, mutable views.

## 2. CURRENT

The statements in this section describe the pinned baseline only.

### 2.1 Tool calls already update in place per call identity

`harw-tui::history_cell::ToolCell` is shared across the requested → running → completed lifecycle. The TUI does not append a second independent history object merely because a result arrived.

`TurnEventState::pending_tool_cells` keys the shared cell by `ToolCallId`.

This proves Harw already accepts the principle:

> one logical runtime thing may receive many updates without becoming many visible history rows.

### 2.2 ToolGroupCell already implements one hard-coded activity pattern

`ToolGroupCell` groups consecutive read-only calls from:

- `fs.read`
- `fs.search`
- `fs.grep`
- `fs.list`
- `fs.glob`
- `doc.read_pdf`

It counts categories and renders one collapsed summary.

Important current limitations:

- grouping is based on tool names, not semantic resource identity;
- it is intentionally restricted to consecutive read operations;
- a mutating tool such as `fs.edit` breaks the group;
- the group owns a `Vec<SharedToolCell>`, not a higher-level resource state;
- the implementation is TUI-local.

This is the immediate migration anchor, not a mistake to delete.

### 2.3 AgentMonitor already keeps mutable live state

`harw-tui::agent_monitor::AgentMonitor` stores one `AgentLive` per concrete agent identity and updates phase, tool, usage, context, previews, failures and bounded trace data in place.

It also already deduplicates some repeated trace content:

- streamed text/reasoning updates mutate the current trace entry;
- identical directly consecutive status messages are not appended again;
- tool calls/results are deduplicated by call id.

The compact panel, however, remains primarily identity-oriented. Repeated equivalent failures from different child identities are not a reusable semantic pattern outside this TUI code.

### 2.4 Tool macros have declarative metadata, but no activity semantics

Current `#[tool]` function attributes support:

- `name`
- `description`
- `permission`
- `host_from`
- `parallel_safe`

The macro generates runtime constants/prologues and a model-facing `ToolSpec`.

There is no `activity(...)`, resource-key metadata, semantic verb, pattern family or reducer hint.

### 2.5 ToolSpec is model-facing vocabulary

`harw_tools::FunctionToolSpec` currently carries:

- name
- description
- JSON parameters
- strictness

It should not automatically become the dumping ground for internal activity semantics. Provider-facing schema and Harw-internal semantic metadata have different consumers and compatibility pressure.

### 2.6 Skills are instruction fragments, not the pattern graph

Current `SkillToml` is a strict manifest containing:

- name
- enabled
- description
- instructions file
- tools
- MCPs

The agent DSL explicitly states that skills are additive instruction fragments and carry no authority.

This is a useful and important abstraction, but it is not a typed graph of observed behavior. PL-71 therefore treats skills as a possible consumer/projection of patterns, not as the canonical source of pattern semantics.

## 3. TARGET

### 3.1 Separate observation from interpretation

```text
Raw Activity Event
    immutable observation
            │
            ▼
Pattern Matcher / Reducer
    deterministic interpretation
            │
            ▼
Pattern Instance
    current semantic state
            │
            ├── links/relations
            ▼
Pattern Graph
            │
            ▼
Projection
    human/agent-facing view
```

A projection may change in place while the event stream remains complete.

### 3.2 Patterns may be deliberately specific

PL-71 rejects the idea that every special case must be erased into one universal abstraction.

A named, testable special case is healthy architecture.

Examples:

- `file-read-burst`
- `file-edit-read-loop`
- `job-lifecycle`
- `repeated-agent-role-failure`
- `compile-fix-cycle`
- `implementation-iteration`

The problem is not specificity. The problem is hidden, duplicated specificity scattered across renderers.

### 3.3 Patterns are linkable

A pattern definition has a stable ID and typed relations.

Example:

```text
pattern: file-edit-read-loop

specializes:
    file-activity-loop

composed_of:
    file-edit
    file-read-burst

related_to:
    compile-fix-cycle

can_derive:
    implementation-iteration
```

This makes patterns addressable from docs, chains, skills, tests and runtime projections.

### 3.4 Patterns are derivable

The graph permits bottom-up derivation:

```text
tool events
  ↓
file-edit-read-loop
  ↓
compile-fix-cycle
  ↓
implementation-iteration
```

and top-down decomposition for planning/explanation:

```text
implementation-iteration
  ↓
edit → inspect → validate → diagnose → repeat
```

Derivation never grants authority.

### 3.5 Skills become projections/consumers, not mandatory source truth

A future skill may reference patterns and present them as model-friendly instructions.

Conceptually:

```text
Pattern Graph
   ├── runtime matching
   ├── UI projection
   ├── chain transition hints
   └── skill projection
```

PL-71 does **not** remove current skills or change `SkillToml` yet.

### 3.6 AI may propose patterns; it does not silently install policy

An AI can discover repeated event sequences and produce a `PatternProposal` containing:

- candidate matcher;
- candidate grouping key;
- evidence count;
- examples/counterexamples;
- estimated compression/value;
- confidence;
- proposed relations.

Proposal → review → accepted pattern is the intended flow.

An AI-generated candidate does not become runtime authority, approval policy or executable code merely because confidence is high.

## 4. Core invariants

### I1 — Raw events are not erased

Pattern consolidation changes presentation and derived state, never the existence of raw tool/job/agent events.

### I2 — Identity and aggregation are different keys

A concrete tool call, agent run or job retains its own identity even when several identities share one compact projection.

### I3 — Projection is replaceable

The same raw events can feed TUI, Web, Telegram or analytics projections without changing the event producer.

### I4 — Pattern data carries no authority

A pattern may describe or predict a privileged operation. It cannot grant permission for that operation.

### I5 — Unknown semantics fail conservatively

A tool with no activity metadata remains a normal standalone event. It is never guessed into a mutating aggregation that would hide important state.

### I6 — Expansion reveals constituents

Any compact projection that hides multiple raw activities must have a path to inspect its constituent events/identities.

### I7 — Derivation is explainable

Every derived pattern instance can report which lower-level instances/events support it.

### I8 — Pattern matching is deterministic in v1

The first runtime matcher should be replayable and testable. AI is used for proposal/discovery, not for nondeterministic per-frame grouping.

## 5. Pattern scales

PL-71 intentionally supports several scales with the same vocabulary.

### Micro

Repeated calls against one resource.

```text
Read(path=A) × 5
```

### Meso

A bounded operational cycle.

```text
Edit(A) → Read(A) × N
```

### Macro

A workflow-shaped combination.

```text
Edit → inspect → cargo check → diagnose → edit
```

### Job / agent lifecycle

Long-running identity/state projection.

```text
start → running → progress* → wait → completed
```

The scales should compose rather than become unrelated subsystems.

## 6. Immediate motivating projections

### File activity

Raw:

```text
fs.edit(src/a.rs)
fs.read(src/a.rs, 120..180)
fs.read(src/a.rs, 180..240)
fs.read(src/a.rs, 240..300)
```

Compact:

```text
● src/a.rs · Edit ×1 · Read ×3
  last: 240..300 · 2.5 KiB
```

The row updates in place as further matching events arrive.

### Job lifecycle

Raw:

```text
job.start(build)
job.status(build)
job.status(build)
job.logs(build)
job.wait(build)
```

Compact while active:

```text
● build · running 18m32s · status ×2 · logs ×1
```

Terminal:

```text
✓ build · exit 0 · 21m04s · status ×2 · logs ×1
```

### Repeated agent failure

Raw identities remain:

```text
uia-writer#8119 failed(model request)
uia-writer#7903 failed(model request)
uia-writer#e4e9 failed(model request)
```

Compact projection:

```text
✗ uia-writer ×3 · model request failed
```

Opening the group reveals all three concrete runs.

## 7. Planning compartments

- [01-vocabulary-and-graph.md](01-vocabulary-and-graph.md) — canonical terms, IDs, relation types and provenance.
- [02-pattern-language.md](02-pattern-language.md) — deterministic pattern definition/matching language and examples.
- [03-runtime-and-projections.md](03-runtime-and-projections.md) — event reduction, mutable projections, replay, concurrency and rebasing.
- [04-macro-metadata.md](04-macro-metadata.md) — future `#[tool]` / `#[operation]` semantic metadata and compatibility rules.
- [05-skills-and-derivation.md](05-skills-and-derivation.md) — patterns vs. skills, AI proposals and derived instruction views.
- [06-integrations.md](06-integrations.md) — TUI, agents, jobs, chains, Web/Telegram and DoD.
- [07-migration-and-verification.md](07-migration-and-verification.md) — incremental implementation waves and tests.

## 8. Ownership direction

The reusable matcher/graph vocabulary belongs to shared infrastructure, not permanently inside `harw-tui`.

Exact crate placement is intentionally not frozen by this planning PR. Before implementation, dependency direction must be checked against `xtask/arch-policy.toml`.

Likely separation:

```text
shared pattern vocabulary / matcher
          ↑
runtime event adapters
          ↑
TUI / Web / jobs / chains / knowledge
```

The TUI remains a consumer.

## 9. Security / TCB effects

PL-71 is designed to remain outside privileged authority decisions.

Patterns can be used by DoD or policy code as **observations**, but a pattern match is not itself permission or enforcement authority.

No matcher result may:

- widen `PermissionSet`;
- mint host permits;
- enable container/node authority;
- bypass approval;
- synthesize secret access;
- turn correlation into an enforcement action without the existing trusted boundary.

## 10. DELTA

From CURRENT to TARGET:

1. extract activity semantics from TUI-local grouping into shared metadata/vocabulary;
2. preserve the existing ToolCell lifecycle and ToolGroupCell behavior while introducing a reducer-backed projection;
3. add stable pattern IDs and typed relations;
4. add deterministic resource/lifecycle matchers;
5. let agent/job panels consume projections;
6. add macro-generated activity metadata without polluting model-facing `ToolSpec`;
7. add proposal/discovery flow for AI-suggested patterns;
8. optionally project accepted patterns into skills/instructions later.

## 11. IMPLEMENTATION STATUS

```text
CURRENT:
  ToolCell mutable lifecycle                implemented
  ToolGroupCell read-only consecutive group implemented
  AgentMonitor mutable per-agent state      implemented
  tool macro metadata                       implemented for permission/etc.
  skills instruction model                  implemented

PLANNED ONLY:
  semantic ActivityDescriptor
  PatternDefinition / PatternInstance
  PatternGraph / typed relations
  generic ActivityReducer
  resource-keyed file patterns
  job lifecycle patterns
  cross-agent compact projections
  macro activity(...) metadata
  AI PatternProposal flow
  skill projection from pattern graph
```

No runtime behavior changes are claimed by this planning document.
