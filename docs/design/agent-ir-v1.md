# Agent IR v1 — the existing pipeline as an explicit compiler channel

> Status: partially implemented · Last reviewed: 2026-09-24

**Design anchor** (descriptive, no new crate).
**Binds to:** [`agent-definition-dsl.md`](agent-definition-dsl.md) §17, [`coding-philosophy.md`](../philosophy/coding-philosophy.md) §6, §7, §11.

This document makes the **lowering stages** that already exist latently in
`harw-agent-dsl` explicit. It replaces no implementation — it names one.

Deliberately **no** `harw-ir`/`harw-lowering`/`harw-codegen` crate. Everything
stays in `harw-agent-dsl`. The value is in explicit boundaries between the
stages, not in new crates.

---

## 1. The four stages

```
TOML Source
    │  parse.rs (frontend / parser)
    ▼
Raw*Definition  (AST — frontend output, syntax-close)
    │  resolve.rs (semantic analysis + lowering)
    ▼
Resolved*Definition  (IR — normalized, monomorphic, verified)
    │  Consumer (runtime / job admission / executor)
    ▼
Runtime Effect  (backend — jobs, tool surface, leases, model requests)
```

All earlier stages are implemented:

- Frontend: `parse.rs::parse_toml`
- AST: `raw::RawAgentDefinition`
- Analysis + lowering: `resolve.rs::resolve_definition`
- Semantic verifier: `authority.rs::AuthorityCeiling::is_reduction_of`
- IR: `resolved::ResolvedAgentDefinition` with `ResolutionTrace` as provenance
- AST for Family: `family::RawFamilyDefinition`
- AST for Organization/Clan/Cell: `organization::RawOrganizationDefinition`
- IR for Family: `family::ResolvedFamily`
- IR for Organization: `organization::ResolvedOrganization`
- Fourth stage, runtime-shaped IR: `executable::ExecutableAgentIr`
  (`harw-agent-dsl/src/executable.rs`), produced by `executable::lower`

---

## 2. Why there are no separate backend crates

A compiler produces machine code. Harwness produces **runtime effects** — and
those don't live in a backend crate, they live in `harw-job-runtime`,
`harw-plan`, `harw-tools`, `harw-provider`. Those crates are the backend.

The consumer contract is always the same:

```rust
fn admit_agent(resolved: ResolvedAgentDefinition, org: &ResolvedOrganization) -> JobHandle;
```

What the compiler comparison clarifies:

- **The AST stays syntax-close.** No cross-reference resolution, no merge
  semantics.
- **The IR is normalized and verified.** After `resolve_definition` the
  following hold: no cycles, no authority elevation, all refs resolved.
- **Consumers only ever see IR.** No runtime consumer may see
  `RawAgentDefinition` — that's why `raw` stays internal to `harw-agent-dsl`
  and only `resolved` is exported.

---

## 3. Optimization passes (later work, declared here)

The following passes would be useful — but **none of them is a prerequisite**
for a working system. They are candidates for future waves; none are
implemented.

| Pass | Effect |
|---|---|
| `RemoveUnusedTools` | remove tools with no consumer in the plan graph from the tool surface |
| `MinimizeContext` | drop context items no rendering path references |
| `CollapseRedundantPolicies` | share two identical context policies |
| `PartitionWriteSets` | make write sets orthogonal so fanout is guaranteed disjoint |
| `InsertVerificationBarrier` | insert a `harw-plan` barrier node before every merge candidate |
| `EnforceToolChoiceReset` | force `reset_tool_choice=true` after every terminal return |
| `LowerGoalGraphToJobs` | flatten expressive goal graphs into executable job sequences |
| `InsertDurabilityBoundaries` | insert a `persist-before-rerun` marker before every state effect |

The most relevant pass for fanout discipline is **`PartitionWriteSets`** — the
one manually checked today during multi-agent fanout planning. If it existed
as a compiler pass, that manual check would disappear.

---

## 4. "Borrow checker, Harwness edition"

If the IR carries a `write_scope: Vec<PathRule>` field per job template and an
analyzer iterates the job graph, a conflict could be reported like:

```
HARW-BORROW-017:
    mutable mutation lease for `harw-tui/src/app.rs`
    is already held by job `input-editor-wiring` (plan revision 42)
    job `chat-scroll-wiring` cannot acquire overlapping write scope
    hint: split plan node or serialize execution
```

This already exists as a runtime check in `harw-plan/src/admission.rs`. An IR
pass would move it to plan-compile time — like a real borrow checker.

---

## 5. What this document does *not* justify

- No separate `harw-ir` crate.
- No multi-backend executor (not needed — the individual runtime crates *are*
  the backends).
- No LLVM analogy. No MLIR. No IR pretty printer.
- No SSA transform, no phi nodes, no dominator tree. This is an agent
  runtime, not an optimizer.

The scope is precise: **name the lowering boundaries so the DSL invariants
(agent-definition-dsl.md §22) can be enforced in one place.** Nothing more,
nothing less.
