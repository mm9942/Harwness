# Agent IR v1 — the existing pipeline as an explicit compiler channel

> Status: partially implemented · Last reviewed: 2026-09-25

**Design anchor** (descriptive, no new crate).
**Binds to:** [`agent-definition-dsl.md`](agent-definition-dsl.md) §17, [`coding-philosophy.md`](../philosophy/coding-philosophy.md) §6, §7, §11.

This document makes the **lowering stages** that already exist latently in
`harw-agent-dsl` explicit. It replaces no implementation — it names one.

Deliberately **no** `harw-ir`/`harw-lowering`/`harw-codegen` crate. Everything
stays in `harw-agent-dsl`. The value is in explicit boundaries between the
stages, not in new crates.

> **Amended by [ADR 0001](../adr/0001-agent-compiler.md).** §2 and §5 no
> longer rule out a compiler crate with more than one backend: compiling an
> agent to a standalone binary adds `harw-agent-artifact`,
> `harw-agent-compiler` and `harw-agent-runner`. The IR itself stays in
> `harw-agent-dsl` and becomes serializable and fully typed as **IR v2**
> (§6).

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

> **Amended by [ADR 0001](../adr/0001-agent-compiler.md).** The argument
> below still holds for *executing* an agent inside harw: the runtime crates
> are the backend. It no longer holds for *distributing* an agent. For that
> output there is one compiler crate, `harw-agent-compiler`, with two
> backends (artifact appended to a prebuilt runner, and `--native` Rust
> codegen plus cargo). Both reuse the runtime crates rather than replacing
> them. The consumer contract below is extended: consumers see `AgentIr`
> (§6), and `ResolvedAgentDefinition` becomes an input to lowering.

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

> **Amended by [ADR 0001](../adr/0001-agent-compiler.md).** The second
> bullet is withdrawn: there is now a compiler with two backends
> (`harw-agent-compiler`), and a runner binary (`harw-agent-runner`) as the
> target of the default backend. The first, third and fourth bullets still
> hold: IR v2 stays in `harw-agent-dsl`, and the only passes are the
> validation and pruning passes listed in §6.4.

- No separate `harw-ir` crate.
- No multi-backend executor (not needed — the individual runtime crates *are*
  the backends).
- No LLVM analogy. No MLIR. No IR pretty printer.
- No SSA transform, no phi nodes, no dominator tree. This is an agent
  runtime, not an optimizer.

The scope is precise: **name the lowering boundaries so the DSL invariants
(agent-definition-dsl.md §22) can be enforced in one place.** Nothing more,
nothing less.

---

## 6. IR v2 — serializable and fully typed

> Status: IR v2, the diagnostics catalog and snapshot hash v7 land in wave 1
> of #22. The compiler passes in §6.4 and the backends are **planned in
> #22** (waves 2–3). Decision record: [ADR 0001](../adr/0001-agent-compiler.md).

The fourth stage of §1 gets a successor. `ExecutableAgentIr` was an
in-memory struct without serde, and several tables of a definition were
never lowered into it. IR v2 is the complete, typed, serializable result
of lowering; everything downstream (runtime, compiler, runner) reads it.

```
Resolved*Definition + sources (instructions, skills, context programs)
    │  lower_v2 (harw-agent-dsl)
    ▼
AgentIr (schema "harwness.agent-ir/v2", serde, canonical JSON)
    ├─► ExecutableAgentIr (view: From<&AgentIr>) ─► runtime inside harw
    └─► harw-agent-compiler passes ─► artifact ─► runner / native binary
```

### 6.1 Schema and sections

`AgentIr` carries `schema = "harwness.agent-ir/v2"` and is deserialized
with `#[serde(deny_unknown_fields)]`: an unknown field is an error, not a
silently ignored key. Its sections are typed:

| Section | Content |
|---|---|
| identity | `id`, `version`, `name`, `description`, `role`, `specialization` |
| `Instructions` | the instruction text and its source hash, from `instructions_file` or `system.md` next to the definition; resolved in the DSL crate |
| `ToolSurface` | admitted and forbidden tools after all merges |
| `SpawnContract` | spawn depth and delegation targets |
| `Budget` | tokens, tool calls, wall time |
| `Lifecycle` | pause, `allow_rerun`, `max_attempts` |
| `ContextProgram` | sections with detail mode, trust class and strength |
| `ReturnPipeline` | a strict return contract (an unknown contract is an error) and its validators |
| `Models` | preferred provider, model and effort; fallbacks; required environment variables (DSL `[models]`) |
| `Limits`, `Work`, `Verification` | the corresponding DSL tables, typed |
| `Skills` | skill names with a content hash each |
| `Binary` | name, interfaces (`cli`, `repl`, `mcp`, `http`, `tui`), default interface (DSL `[binary]`) |
| `Permissions` | the rights manifest: tools, network hosts and modes, write paths, shell, host access, budgets |

`lower_v2(resolved, sources) -> Result<AgentIr, Diagnostics>` lowers every
table or rejects it with a diagnostic. Nothing is dropped silently.
`bind_context_program` moves into the DSL crate, so user definitions get a
context program bound just like built-in ones.

`ExecutableAgentIr` stays as a view built with `From<&AgentIr>`, so existing
consumers keep compiling while they migrate to `AgentIr`.

### 6.2 Snapshot hash v7

The snapshot hash moves from the hand-written byte stream of v6
(`harwness.executable-ir.snapshot/v6`) to **v7**: BLAKE3 over the
canonical serialization of `AgentIr`, domain-separated by a tag ending in
`/v7`. The canonical serialization is compact JSON with struct fields in
declaration order, map keys sorted, set-valued lists (admitted tools,
capabilities, forbidden tools) sorted, and order-carrying lists (validators,
context sections, skills) kept in declaration order.

Unlike v6, the hash covers the definition version, the instruction text,
the skill content hashes and the rights manifest. As before, the resolution
trace (it contains wall-clock timestamps) and the snapshot id itself are
excluded. Every v6 digest and golden snapshot is invalid under v7; the
domain tag keeps the two address spaces apart.

### 6.3 Diagnostics

Lowering and the compiler report diagnostics with a stable code
`HARW-<AREA>-NNN`, a severity (`error`, `warning`, `note`), a source
location (`file:line:column`, from `toml::Spanned` and `toml::de` spans)
and a help text. The area catalog lives in
`harw-agent-dsl/src/diagnostics.rs` and is authoritative; codes are never
reused for a different meaning. The areas and examples are listed in
[`agent-definition-dsl.md`](agent-definition-dsl.md) §20.

### 6.4 Passes

The passes of §3 remain candidates. The compiler (**planned in #22**) adds
the following, all of which validate or prune and none of which transform
control flow:

| Pass | Effect |
|---|---|
| `ValidateRoles` | the role and spawn matrix of the definition are admissible |
| `RightsCheck` | manifest ≤ base role ≤ author ceiling |
| `ResolveSkills` | resolve skills from the `SkillIndex` and embed their content |
| `ReachableTools` | determine which tool providers the admitted tools need |
| `PruneUnusedTools` | drop providers no admitted tool reaches (implements `RemoveUnusedTools` from §3 at provider level) |
| `ResolveModels` | resolve model preferences and put required environment variables into the manifest |

The serialized form of `AgentIr` is the header of the agent artifact:
[`agent-artifact-v1.md`](agent-artifact-v1.md).
