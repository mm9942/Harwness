# Harwness Coding Philosophy

**Status:** Normative  
**Scope:** Research, planning, implementation, orchestration, integration, and verification  
**Companion document:** [`philosophy.md`](./philosophy.md)

> Research establishes reality.  
> Planning establishes contracts and dependencies.  
> Focused coding agents implement bounded work.  
> The compiler and verification pipeline decide whether the resulting system is coherent.

## 1. Purpose

This document defines how Harwness should be researched, designed, implemented, and evolved.

It is not a generic clean-code guide and it is not a formatting convention. It defines the development protocol used to transform uncertain goals into verified software while preserving architectural coherence under high parallelism.

`philosophy.md` defines what Harwness must fundamentally be. This document defines how changes to Harwness should be produced.

If the two documents appear to conflict, `philosophy.md` is authoritative. A development process is invalid when it produces code that violates the system philosophy, even when that code compiles and its local tests pass.

The central premise is:

> Parallelism is safe when boundaries are explicit, dependencies are ordered, authority is centralized, and completion is demonstrated by evidence.

---

## 2. Development Is a Staged Compilation Process

Development in Harwness is treated as a sequence of explicit transformations:

```text
Goal
→ scoped questions
→ verified research
→ architectural synthesis
→ dependency graph
→ implementation contracts
→ focused code patches
→ integrated workspace
→ compiler and test evidence
→ durable project knowledge
```

Each stage has a distinct responsibility.

- **Research** discovers what is currently true.
- **Architecture** decides which truths and constraints matter to the system.
- **Planning** converts architectural decisions into an executable dependency graph.
- **Focused coding agents** implement bounded nodes from that graph.
- **The orchestrator** owns integration and global coherence.
- **Rust, Clippy, tests, and end-to-end checks** determine whether the intended system was actually produced.

No stage may silently impersonate another:

- Research does not make unreviewed architectural decisions.
- Planning does not claim implementation.
- Coding workers do not redesign the system.
- Compilation does not prove behavioral correctness.
- A model statement does not prove completion.
- Documentation does not substitute for executable evidence.

The process must preserve the distinction between intent, implementation, and proof.

---

## 3. Greenfield Work Begins With Active Research

A new project must not begin by asking a coding model to generate a repository from training-memory assumptions.

Greenfield development begins by establishing a researched view of the current technical environment.

Before the implementation plan is written, the orchestrator defines:

- the desired outcome;
- the system boundary;
- explicit non-goals;
- required platforms and deployment environments;
- trust and authority boundaries;
- performance, reliability, and security constraints;
- relevant protocols, libraries, providers, runtimes, and standards;
- unresolved questions that could materially change the architecture.

The research phase then verifies the current state of the relevant ecosystem.

For dependencies and external APIs, this includes:

- the latest suitable stable release;
- current official documentation;
- actual current syntax;
- required and optional feature flags;
- minimum supported Rust version where relevant;
- compatibility with adjacent dependencies;
- deprecations and announced breaking changes;
- operational and security limitations;
- licensing and distribution constraints;
- whether a capability exists in stable, preview, nightly, or only as a proposal;
- whether a public claim is backed by an implementation or only by marketing.

The default dependency rule is:

> Use the newest suitable stable version that has been verified against current primary documentation and project constraints.

“Newest” does not mean blindly selecting the highest version number. A prerelease, nightly-only feature, incompatible MSRV, abandoned crate, or unstable protocol revision may be newer while still being the wrong choice.

Likewise, “stable” does not mean using an old familiar version recalled by a model. Version selection is an evidence-backed architectural decision.

---

## 4. Research Can Fan Out Aggressively

Information collection is parallelizable when question boundaries and evidence contracts are explicit.

The orchestrator first creates a research dependency graph. Independent research questions may then be assigned to parallel information-collection agents.

Example:

```text
Research Agent A
→ official API documentation and current syntax

Research Agent B
→ crate versions, feature flags, MSRV, and compatibility

Research Agent C
→ protocol semantics and lifecycle requirements

Research Agent D
→ security model, trust boundaries, and known failure modes

Research Agent E
→ comparable open-source implementations

Research Agent F
→ testing strategies, benchmarks, and operational constraints
```

Research agents do not receive vague instructions such as “research the topic.” They receive:

- a bounded question set;
- permitted source classes;
- required primary sources;
- an expected output schema;
- a freshness requirement;
- a clear stopping condition.

A research result should be representable as structured evidence:

```rust
struct ResearchFinding {
    question: String,
    conclusion: String,
    evidence: Vec<SourceReference>,
    verified_versions: Vec<VersionReference>,
    constraints: Vec<String>,
    compatibility_notes: Vec<String>,
    unresolved_questions: Vec<String>,
    confidence: Confidence,
}
```

The exact representation may differ, but the semantics should remain.

Research fan-out follows these rules:

1. **Question ownership is explicit.**  
   Each agent knows which questions it must answer and which questions belong elsewhere.

2. **Source boundaries are explicit.**  
   Official documentation, specifications, source repositories, release notes, and primary research are preferred over summaries.

3. **Outputs are normalized.**  
   The orchestrator must not receive unrelated essays that force the entire research phase to be repeated during synthesis.

4. **Duplication is deliberate.**  
   Overlapping research is useful when independently validating a high-risk claim. Accidental duplication is wasted parallelism.

5. **Unknowns remain visible.**  
   An agent must not invent certainty merely to complete its assignment.

6. **Research agents do not mutate implementation architecture.**  
   They provide evidence. They do not independently turn findings into incompatible code.

Research completion creates a synthesis barrier. Implementation planning begins only after the evidence required for architecture is available or an unresolved uncertainty has been explicitly accepted as a risk.

---

## 5. Architecture Is Synthesized Centrally

The primary orchestrator owns the system-wide interpretation of research.

It is responsible for:

- defining architectural boundaries;
- defining invariants;
- choosing dependency directions;
- resolving conflicting findings;
- defining public contracts;
- deciding which provider-specific behavior must remain visible;
- deciding what may be normalized and what must not be reduced to a lowest common denominator;
- defining migration and compatibility policy;
- identifying uncertainties that require prototypes;
- keeping all decisions aligned with `philosophy.md`.

Focused workers may report local observations, but architectural authority does not fragment across the fan-out.

This is not a rejection of parallel reasoning. Research, critique, validation, and implementation may all be parallelized. The requirement is that cross-cutting architectural decisions have one authoritative integration point.

A coherent architecture cannot reliably emerge from several workers independently modifying the same abstraction according to their own local interpretation.

---

## 6. Planning Is a First-Class Harness Tool

Planning must not exist only as prose in a prompt.

Harwness should provide a first-class, typed planning tool that models the implementation plan as durable runtime state. The tool is available to the orchestrator when enabled and may be completely disabled through configuration.

The tool exists to make planning explicit, inspectable, validated, and recoverable.

It does **not** grant execution authority. It does **not** run coding tasks by itself. It records and validates the graph from which jobs may later be admitted.

The conceptual boundary is:

```text
Goal
→ desired state

Plan Tool
→ versioned strategy and dependency graph

Job Runtime
→ admitted executable work

Focused Coding Agent
→ bounded implementation worker

Verification
→ evidence that the desired state was reached
```

### 6.1 Tool responsibilities

The planning tool should support operations equivalent to:

```text
create plan
inspect plan
add or update node
declare dependency
declare read/write scope
declare contracts
declare acceptance criteria
mark node ready
mark node blocked
invalidate stale nodes
record evidence
close or supersede plan revision
```

The public tool may be exposed as one discriminated operation or a small namespace of operations. The exact API is an implementation decision, but the semantic boundary must stay stable.

A possible request model is:

```rust
enum PlanAction {
    Create(CreatePlan),
    AddNode(AddPlanNode),
    UpdateNode(UpdatePlanNode),
    AddDependency(AddDependency),
    SetStatus(SetNodeStatus),
    AttachEvidence(AttachEvidence),
    Invalidate(InvalidateNodes),
    Inspect(InspectPlan),
    Supersede(SupersedePlan),
}
```

A plan node should preserve at least:

```rust
struct PlanNode {
    id: TaskId,
    objective: String,
    dependencies: Vec<TaskId>,
    input_contracts: Vec<ContractRef>,
    output_contracts: Vec<ContractRef>,
    read_scope: Vec<PathOrSymbol>,
    write_scope: Vec<PathOrSymbol>,
    forbidden_scope: Vec<PathOrSymbol>,
    acceptance_criteria: Vec<Criterion>,
    verification: Vec<VerificationStep>,
    invalidation_conditions: Vec<Condition>,
    status: PlanNodeStatus,
}
```

The exact Rust types may evolve. The retained semantics are normative.

### 6.2 Runtime ownership

The model proposes plan mutations.

The runtime owns:

- plan identity;
- revision numbers;
- persistence;
- validation;
- authorization;
- dependency-cycle detection;
- scope-conflict detection;
- transition legality;
- immutable history;
- actor identity;
- timestamps;
- evidence references.

Untrusted tool arguments must never be allowed to grant capabilities, worker identity, tenant scope, repository authority, or approval rights.

The planning tool follows the same rule as every other Harwness operation:

> The model proposes intent; the runtime owns authority and durable state.

### 6.3 Validation

Before accepting a plan mutation, the runtime should validate relevant invariants:

- referenced nodes exist;
- dependency additions do not create a cycle;
- completed dependencies cannot be silently replaced;
- active write scopes do not conflict unless explicitly serialized;
- forbidden scopes are not included in writable scopes;
- acceptance criteria are present for executable coding nodes;
- plan revisions are monotonic;
- stale workers cannot update a superseded revision;
- evidence references point to known artifacts, traces, or verification runs;
- status transitions are legal.

A valid graph is not necessarily a correct architecture, but an invalid graph should never be admitted merely because the model emitted syntactically valid JSON.

### 6.4 Configuration

The planning tool must be runtime-configurable.

A representative configuration may look like:

```toml
[tools.plan]
enabled = true
expose_to_models = true
persist = true
require_for_complex_work = true
validate_dependency_cycles = true
validate_write_conflicts = true
max_nodes = 256
```

At minimum, the implementation must support:

```toml
[tools.plan]
enabled = false
```

When disabled:

- the operation is not registered;
- it is not included in model tool schemas;
- model prompts must not claim that it is available;
- no request may implicitly re-enable it;
- existing persisted plans remain readable only if the surrounding policy explicitly allows it;
- ordinary internal model reasoning may still form an ephemeral strategy, but no durable plan-tool state is created.

Disabling the tool must not weaken unrelated safety boundaries. It only removes the explicit planning facility.

Configuration may also define modes such as:

```text
off
available
required-for-complex-work
required
```

However, a simple boolean disable path must always remain available.

### 6.5 Surface exposure

The planning capability may have several surfaces:

- a **model tool surface** for structured plan creation and updates;
- a **command surface** such as `/plan` for inspection and explicit user control;
- a **TUI or client projection** for graph visualization;
- an **internal orchestrator API** for validated runtime transitions.

A shared semantic operation does not imply identical permissions on every surface.

For example:

- the model may propose nodes;
- the user may approve, lock, supersede, or inspect them;
- a remote channel may have read-only access;
- focused coding workers may only read their assigned node and report evidence;
- only the primary orchestrator may alter cross-cutting dependencies.

### 6.6 Persistence and recovery

When persistence is enabled, the plan must survive:

- context compaction;
- model replacement;
- client restart;
- gateway restart;
- worker failure;
- process termination.

After restart, the runtime reconciles:

```text
persisted plan revision
+ durable job state
+ active leases
+ produced artifacts
+ verification evidence
→ current executable state
```

The plan is not reconstructed from transcript text when structured state is available.

### 6.7 Plan revisions

Plans evolve when evidence changes.

A plan mutation that changes a shared contract must create a new revision or explicitly supersede affected nodes.

When a contract changes:

1. create or advance the plan revision;
2. identify dependent nodes;
3. stop, invalidate, rebase, or regenerate stale work;
4. update scopes and acceptance criteria;
5. resume only against the current revision.

Workers must never silently compensate for an outdated contract by introducing local compatibility layers.

---

## 7. The Plan Is an Executable Dependency Graph

A plan is not a prose checklist.

A valid implementation plan represents work as nodes with explicit dependencies, contracts, scopes, and evidence requirements.

The dependency graph determines execution order:

```text
research
→ synthesis
→ core types
→ contracts
→ independent implementations
→ integration
→ workspace verification
```

New abstractions follow the same process. The fact that an abstraction does not yet exist does not make it unplannable.

A new abstraction is decomposed into dependent nodes:

```text
semantic requirements
→ invariants
→ domain types
→ trait or protocol contract
→ reference behavior
→ adapters
→ consumers
→ integration tests
```

Independent nodes may run in parallel as soon as their dependencies and contracts are stable for the current plan revision.

The plan may be extended, corrected, or improved when new evidence appears. Plan changes must be explicit and propagated through the dependency graph.

---

## 8. Boundaries Create Parallelism

The limiting factor in agent parallelism is not the number of available models. It is the quality of the boundaries.

Two coding tasks are safely parallelizable when:

```text
their required dependencies are satisfied
AND
their mutation scopes do not conflict
AND
their shared interfaces are already defined
AND
their promised outputs are mutually compatible
```

The strongest practical condition is:

```text
WriteSet(Task A) ∩ WriteSet(Task B) = ∅
```

Read scopes may overlap heavily. Multiple workers may need to understand the same trait, domain type, plan, or neighboring module. Mutation ownership should remain exclusive during a batch.

Harwness primarily uses two focused coding task forms.

### 8.1 Range- or symbol-scoped tasks

The worker modifies only a defined symbol, function, implementation block, or line region.

This is appropriate when:

- the surrounding file is shared infrastructure;
- the required change is local;
- the interface is already stable;
- the orchestrator can identify a mutation boundary precisely.

### 8.2 File-scoped tasks

The worker owns a complete file for the duration of the batch and implements the assigned contract within that file.

This is appropriate when:

- the file has one clear responsibility;
- implementation freedom is useful inside the file;
- no other active worker needs to mutate it;
- integration occurs through previously defined types or traits.

The orchestrator may assign a directory or module scope only when that broader ownership is genuinely isolated.

Parallel work is not justified merely because two tasks have different descriptions. Two semantically different tasks that mutate the same central file are usually coupled. Ten similar provider adapters in ten independent files may be highly parallelizable.

The planning tool should use declared write scopes to detect or prevent unsafe simultaneous admission.

---

## 9. Focused Coding Agents Are Pure Implementation Workers

Focused coding agents are not miniature product teams.

Their purpose is to produce precise code updates against an already defined plan node.

They should receive:

- the exact objective;
- the relevant architectural context;
- the contract they must implement;
- satisfied dependency assumptions;
- the permitted write scope;
- forbidden files or symbols;
- acceptance criteria;
- required tests and verification commands;
- the plan revision and base revision they are implementing against.

A focused coding worker should:

- inspect only the context required to implement its task;
- write or update code;
- add or update tests within its scope;
- run relevant local formatting, compilation, Clippy, and test checks;
- report the produced patch and any concrete blocker;
- avoid unrelated cleanup;
- avoid broad research;
- avoid redesigning public contracts;
- avoid changing dependencies unless explicitly assigned;
- avoid editing shared integration points owned by the orchestrator;
- avoid replacing implementation with recommendations.

When a worker discovers that the assigned contract is impossible or internally inconsistent, it reports the blocker rather than expanding its own authority.

The worker may identify a required plan change. It may not unilaterally redefine the shared architecture.

---

## 10. Coding Fan-Out Is a Controlled Write Batch

A coding fan-out is a set of concurrent mutation jobs admitted against one plan revision and one known repository baseline.

Conceptually:

```text
Orchestrator
→ resolves ready plan nodes
→ validates dependencies
→ validates write-set disjointness
→ allocates workers and budgets
→ issues bounded coding jobs
→ waits at an integration barrier
→ gathers patches and evidence
→ performs global integration
```

Each worker should have a mutation lease over its declared scope.

A mutation lease means:

- the worker has temporary authority to update only its assigned scope;
- no conflicting worker should be admitted concurrently;
- the lease is bound to a plan revision and repository base revision;
- stale workers cannot commit against superseded state;
- completion, expiry, cancellation, or invalidation releases the scope.

This does not require every implementation to use a literal filesystem lock. It requires the runtime to preserve the semantics of exclusive mutation ownership.

Coding batches should be as large as safely possible, not as large as numerically possible.

The aim is maximum useful concurrency under preserved coherence.

---

## 11. Shared Contracts Precede Parallel Consumers

A broad fan-out should not begin until the contracts needed by its workers are defined.

For example:

```text
domain types
→ provider trait
→ request and response semantics
→ error model
→ event model
→ reference adapter
→ parallel provider adapters
```

The orchestrator may first land a narrow vertical slice:

```text
one provider
→ one model request
→ one tool call
→ one event path
→ one store
→ one client
```

This slice is not an excuse to postpone architecture. It is an executable validation of the architecture before multiplying it across many workers.

Once the contract is proven sufficiently stable, independent implementations can fan out aggressively.

When an abstraction is intentionally provisional, that status must be visible in the plan. Provisional contracts should have bounded consumers and explicit invalidation conditions.

---

## 12. Rust Is the Structural Integration Barrier

Rust is not merely the implementation language. Its type system and compiler are part of the orchestration protocol.

After a fan-out, the compiler checks whether independently produced parts actually satisfy the same contracts:

- trait implementations match;
- generic bounds align;
- ownership and borrowing are valid;
- lifetimes are coherent;
- enum variants are handled;
- visibility boundaries are respected;
- feature combinations compile;
- `Send` and `Sync` requirements hold;
- error conversions are complete;
- APIs are used consistently.

Clippy with warnings denied extends this barrier:

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

This catches incomplete integrations such as:

- unused adapters;
- obsolete imports;
- dead transition code;
- accidental shadow abstractions;
- suspicious conversions;
- needless allocation patterns;
- branches made unreachable by a refactor.

The desired outcome is not merely a compiler that can be made green after extensive cleanup.

The process should produce small, contract-driven patches that naturally converge toward a clean compiler and Clippy result.

A green compiler proves structural coherence, not full correctness. Behavioral and operational claims still require tests and end-to-end evidence.

---

## 13. Integration Remains a First-Class Task

Disjoint patches are not automatically an integrated system.

After the coding barrier, the orchestrator owns:

- patch collection;
- revision validation;
- conflict detection;
- shared registry updates;
- composition-root wiring;
- migrations;
- feature integration;
- documentation required by the change;
- workspace-wide verification;
- removal of temporary compatibility code;
- confirmation that the resulting architecture still follows `philosophy.md`.

Tightly coupled work should remain under one atomic owner rather than being artificially split.

Examples include:

- a trait and the proc macro that generates it;
- a schema migration and the code that depends on its atomic semantics;
- a lifecycle transition and its persistent fencing logic;
- a public enum change and exhaustive consumer migration;
- a composition-root cutover.

The objective is not to maximize the number of workers attached to a feature. The objective is to minimize elapsed time without fragmenting ownership.

---

## 14. Verification Is Evidence, Not Confidence

A task is complete only when its acceptance criteria are supported by evidence.

Possible evidence includes:

- successful compilation;
- Clippy with warnings denied;
- unit tests;
- integration tests;
- end-to-end tests;
- protocol conformance tests;
- restart and recovery tests;
- migration tests;
- property tests;
- fuzzing;
- benchmarks;
- trace inspection;
- generated artifacts;
- a reviewed diff;
- a verified runtime demonstration.

Verification is layered:

```text
local worker checks
→ module or crate checks
→ integration checks
→ workspace checks
→ behavioral and operational checks
```

Focused workers run checks relevant to their assigned scope.

The orchestrator runs the global gates because only the integrated workspace can prove cross-task compatibility.

The planning tool should attach verification evidence to plan nodes rather than relying on a worker’s natural-language claim that a task is finished.

---

## 15. Failure, Recovery, and Invalidation Are Planned

Agent work can fail after producing partial effects.

The plan therefore includes failure semantics:

- what may be retried;
- what is idempotent;
- what must be rolled back;
- what artifacts may be reused;
- what invalidates downstream nodes;
- how a stale worker is fenced;
- how write scopes are released;
- how evidence is preserved;
- when human or orchestrator review is required.

A retry is not a new unrelated task. It is another execution attempt for the same plan node unless the plan has changed.

The runtime should distinguish:

```text
task identity
execution attempt
worker identity
plan revision
repository revision
```

Conflating these identities produces duplicate work and stale patches.

When a dependency changes, downstream nodes do not remain valid merely because their workers completed successfully. Their outputs must be revalidated against the new contract or explicitly invalidated.

---

## 16. Dependencies Are Verified Inputs

External dependencies are part of the architecture.

A dependency change requires evidence for:

- version choice;
- API compatibility;
- feature flags;
- transitive implications;
- MSRV impact;
- security posture;
- license compatibility;
- behavior under enabled workspace features;
- migration cost;
- fallback or replacement strategy where appropriate.

Dependency agents may research options in parallel. The orchestrator selects the version and records the decision.

Focused coding agents consume the selected dependency contract. They do not independently upgrade or substitute libraries unless that is their assigned task.

Lockfiles, generated code, schemas, and protocol versions are treated as integration artifacts, not incidental files.

---

## 17. Provider-Specific Reality Must Survive Abstraction

A shared abstraction must remove incidental differences without erasing meaningful capabilities.

For models and providers, the architecture should distinguish:

```text
provider protocol
model descriptor
declared capability
runtime policy
observed behavior
```

A provider adapter should preserve:

- tool-call semantics;
- tool-call identifiers;
- streaming event structure;
- reasoning controls;
- usage accounting;
- cache semantics;
- structured-output behavior;
- modality support;
- provider-specific errors;
- relevant lifecycle metadata.

Focused agents may implement separate adapters in parallel when the common contract is stable and each adapter owns an independent file or module.

The integration stage verifies that the abstraction remains semantically lossless.

---

## 18. Context and Memory Follow the Same Discipline

Context construction and memory evolution are not exempt from the research-plan-implementation protocol.

Memory changes should define:

- information classes;
- provenance;
- selection policy;
- retention;
- promotion;
- consolidation;
- invalidation;
- visibility;
- recovery;
- observability.

The canonical memory store remains model-independent.

Model-aware context rendering may vary by capability and observed behavior, but it must not create provider-specific truths.

Memory workflows should be represented as durable, idempotent state transitions. They should not depend on a transcript remaining in one context window.

---

## 19. Authority Is Never Delegated Through Prose

No worker gains authority because a prompt says it may act broadly.

Authority is derived from runtime state:

```text
system maximum
∩ principal capability
∩ agent definition
∩ plan node
∩ job scope
∩ channel restriction
∩ sandbox policy
=
effective worker authority
```

The plan describes intended work. It does not override permission boundaries.

A focused worker’s write scope must be enforced by available runtime mechanisms where practical and verified by diff inspection regardless.

Research agents are read-only unless explicitly assigned an artifact output location.

Remote channels, model outputs, tool arguments, MCP content, and worker reports are untrusted inputs. They may propose changes but may not expand their own authority.

---

## 20. Documentation Is Part of Integration

Documentation should describe the architecture that actually exists.

Normative documents include:

- `philosophy.md`;
- this `coding-philosophy.md`;
- explicit RFCs, ADRs, or contracts designated normative by the project.

Implementation documentation should be updated when behavior, public interfaces, configuration, or operating procedures change.

Documentation work may be assigned separately when its scope is independent, but documentation must not get ahead of unimplemented behavior.

A coding task is not complete merely because comments describe the intended future state.

---

## 21. Anti-Patterns

The following practices violate this coding philosophy.

### Coding before current research

Generating implementations from stale model memory before checking the actual dependency and API documentation.

### Unbounded research

Starting broad information collection without explicit questions, source boundaries, or completion criteria.

### Prose-only planning

Describing a complex implementation in text without dependencies, scopes, contracts, and verification nodes.

### Architecture by worker consensus

Allowing several focused agents to independently redefine the same shared abstraction.

### Parallel work with overlapping mutation scopes

Assigning simultaneous workers to the same file or symbol without an explicit serialization strategy.

### Local success mistaken for integration

Accepting a worker’s passing unit test as proof that the workspace is coherent.

### Silent plan drift

Changing an interface while downstream workers continue against the previous version.

### Prompt-based permissions

Relying on instructions instead of runtime-enforced capabilities and scopes.

### Blind latest-version selection

Choosing a dependency solely because its version number is highest.

### Model declaration treated as evidence

Marking work complete because an agent says it is complete.

### Compatibility debris

Allowing workers to add unplanned wrappers, fallback paths, or duplicate abstractions to compensate for unclear contracts.

### Maximum fan-out as a goal

Creating more workers than the dependency graph can safely support.

---

## 22. Reference Workflow

A complete greenfield or major architectural workflow should resemble:

```text
1. Define goal, scope, constraints, and non-goals.
2. Create bounded research questions.
3. Fan out information-collection agents.
4. Normalize and verify evidence.
5. Synthesize architecture against philosophy.md.
6. Create a versioned plan through the planning tool.
7. Define dependencies, contracts, write scopes, and evidence gates.
8. Implement or validate the foundational vertical slice.
9. Admit all ready, non-conflicting coding nodes.
10. Run focused coding agents as pure write/update workers.
11. Gather patches at an integration barrier.
12. Perform composition-root and coupled integration work.
13. Run formatting, build, Clippy, tests, and end-to-end checks.
14. Attach evidence to plan nodes.
15. Reconcile failures, stale workers, and invalidated outputs.
16. Update documentation and durable project knowledge.
17. Mark the goal complete only when acceptance criteria are proven.
```

The same workflow scales down.

A small, isolated change may require only a tiny plan or no durable plan when the planning tool is configured as optional. Complexity determines ceremony; philosophy determines boundaries.

---

## 23. Normative Invariants

The following invariants summarize this document:

1. **Greenfield work begins with active, current research.**
2. **Primary documentation outranks model memory.**
3. **The newest suitable stable dependency is selected through evidence, not habit.**
4. **Research fan-out requires bounded questions and normalized outputs.**
5. **Architecture has one authoritative synthesis point.**
6. **Planning is a first-class, typed, configurable harness tool.**
7. **The planning tool may be disabled and must disappear cleanly when disabled.**
8. **The model proposes plan changes; the runtime validates and persists them.**
9. **A plan is a versioned dependency graph, not a prose checklist.**
10. **Plan changes explicitly invalidate or rebase dependent work.**
11. **Parallelism follows dependency readiness and non-overlapping write scopes.**
12. **Focused coding agents implement; they do not independently redesign.**
13. **Each active mutation scope has one owner.**
14. **Shared contracts precede broad fan-out.**
15. **Rust is a structural integration barrier, not the sole correctness proof.**
16. **Workspace-wide verification belongs to the orchestrator.**
17. **Completion requires evidence.**
18. **Retries preserve task identity and create new execution attempts.**
19. **Untrusted inputs never expand authority.**
20. **The final implementation must remain aligned with `philosophy.md`.**

---

## Conclusion

Harwness development is not organized around asking one model to write everything, nor around allowing many agents to improvise simultaneously.

It is organized around controlled transformations:

```text
current evidence
→ explicit architecture
→ validated plan
→ dependency-safe fan-out
→ bounded implementation
→ typed integration
→ verified behavior
```

The ability to use many agents safely does not come from their intelligence alone. It comes from the precision of the boundaries around them.

Research agents scale knowledge collection because their questions and outputs are bounded.

Focused coding agents scale implementation because their contracts and mutation scopes are bounded.

The planning tool makes those boundaries durable, inspectable, and recoverable.

The orchestrator preserves global meaning.

Rust rejects structural incoherence.

Tests and runtime evidence reject behavioral fiction.

This is how Harwness can increase parallelism without sacrificing clarity, produce cleaner compiler output instead of integration debris, and evolve new abstractions without surrendering architectural control.
