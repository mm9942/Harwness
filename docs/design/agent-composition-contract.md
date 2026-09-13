# Agent Composition Contract — Design Only (post-0.2.0)

**Status:** design draft, no implementation. Nothing in this document changes
runtime behaviour and nothing here is required by the 0.2.0 milestone. It
exists to answer the question goal.md's bottom-up analysis raised: *what,
concretely, should `AgentProgram`, `AgentInstallation`, and
`AgentRuntimeAssembly` contain, and which existing crate/type sits at each
boundary?*

Per goal.md §6: this document does **not** attempt to declare the long-term
agent ontology stable. It is scoping work for a later wave.

## 1. The problem, stated precisely against real code

A source-grounded audit (2026-07-16) confirms three facts:

1. **`ExecutableAgentIr`** (`harw-agent-dsl/src/executable.rs`) already has the
   right *shape* — `id`, `role`, `specialization`, `authority`,
   `spawn_contract`, `job_template`, `context_program`, `tool_surface`,
   `lifecycle_machine`, `return_pipeline`, `trace`, `snapshot_id` — but several
   of those fields (`SpawnContract`, `JobTemplate`, `ContextProgram`,
   `ResolvedToolSurface`, `LifecycleMachine`, `ReturnPipeline`) are
   deliberately sparse placeholder structs (a handful of `Option<String>` /
   `Vec<String>` fields), not yet rich enough to drive real binding decisions.
2. **No code path exists** that takes an `ExecutableAgentIr` and produces or
   configures an `ExtensionRegistry` or `AgentSession`. The `harw` SDK
   example (`harw/tests/sdk_example.rs`) lowers a definition to IR and
   separately assembles a registry via `assemble_default_registry` — the two
   are never connected; only `ir.id.name` is asserted.
3. **No capability binder exists.** `ChildRegistryFactory`'s default
   `build_registry_with_capabilities` forwards `SpawnCapabilitySnapshot.suggestions`
   but silently discards `.activated` (`harw-core/src/child_controller.rs`).
   `assemble_default_registry` (`harw-registry-defaults`) never references
   `harw-catalog` at all.

So today there are three islands — **Static Definition** (`harw-agent-dsl`),
**Runtime Contributor** (`harw-extension-api` + `harw-core::AgentSession`),
and **Catalog/Selection** (`harw-catalog`) — with zero linking code between
them. This document proposes the two missing linking boundaries.

## 2. The three boundaries

```
ExecutableAgentIr  ──┐
Resolved Family/Org ─┼──(link)──▶  AgentInstallation  ──(instantiate)──▶  AgentRuntimeAssembly
Host Capability Catalog ─┘                                                       │
Model/Provider Catalog ──┘                                                       ▼
                                                                          AgentSession consumes it
```

### 2.1 `AgentProgram` — already exists, do not rename

What goal.md's analysis calls `AgentProgram` **is** `ExecutableAgentIr`. It
does not need a new type; it needs its placeholder fields filled in over
future waves (that work is explicitly out of scope here — see goal.md's
"Known Unstable" list: full IR-to-Runtime consumption).

### 2.2 `AgentInstallation` — proposed new type, `harw-core` or a new
`harw-agent-install` crate

Binds symbolic requirements from an `ExecutableAgentIr` against a concrete
host. Immutable once built, shareable (`Arc`) across sessions.

```rust
pub struct AgentInstallation {
    pub program_snapshot: SnapshotId,           // from ExecutableAgentIr
    pub definition_provenance: DefinitionId,    // from ExecutableAgentIr.id
    pub capability_bindings: Vec<CapabilityBinding>, // NEW — see 2.2.1
    pub allowed_model_routes: Vec<ModelRouteRef>,    // resolved via harw-model-catalog
    pub authority_envelope: PermissionSet,           // harw-sandbox, intersected with AuthorityCeiling
    pub spawn_table: BTreeMap<String, SpawnTargetRef>, // model-alias -> installed child program
}
```

#### 2.2.1 `CapabilityBinding` — the missing piece identified in §1.3

```rust
pub enum CapabilityBinding {
    ToolProvider(Arc<dyn ToolProvider>),
    ContextProvider(Arc<dyn ContextProvider>),
    InstructionsProvider(Arc<dyn InstructionsProvider>),
}
```

A `CapabilityBinder` (new, small trait) is the function type §1.3 shows does
not exist yet:

```rust
pub trait CapabilityBinder {
    fn bind(&self, activated: &[ActivatedCapability]) -> Vec<CapabilityBinding>;
}
```

This is the smallest change that closes the "discarded `.activated`" gap:
`ChildRegistryFactory::build_registry_with_capabilities`'s default impl
would call a `CapabilityBinder` instead of dropping the field.

### 2.3 `AgentRuntimeAssembly` — proposed thin wrapper, not a new god-object

```rust
pub struct AgentRuntimeAssembly {
    pub registry: ExtensionRegistry,       // built FROM AgentInstallation.capability_bindings
    pub activation_baseline: SessionActivation,
    pub authority: PermissionSet,          // AgentInstallation.authority_envelope, session-reduced
}
```

`AgentSession::new` already takes `ExtensionRegistry` — this type does not
replace `AgentSession`, it is the thing that *produces* the `ExtensionRegistry`
argument instead of `assemble_default_registry` doing it ad hoc from scratch
every time.

## 3. Explicit crate-to-boundary mapping

| Boundary | Existing type | Crate | Status |
|---|---|---|---|
| Program | `ExecutableAgentIr` | `harw-agent-dsl` | exists, placeholder-heavy |
| Installation | *(none)* | proposed: `harw-core` (new module) | not started |
| Installation′ capability binder | *(none — `.activated` discarded today)* | proposed: `harw-catalog` or `harw-core` | not started |
| Runtime Assembly | `ExtensionRegistry` (partial) | `harw-extension-api` | exists, built ad hoc |
| Session | `AgentSession` | `harw-core` | exists, unchanged by this proposal |
| Child admission | `ManagedAgentSpawner` | `harw-core` | exists; keys children by `String`, not `AgentRoleId` — see §4 |

## 4. One correction to goal.md's own analysis

goal.md's bottom-up text implies `ManagedAgentSpawner` could route by
`AgentRoleId`. The source audit confirms it routes by a **bare `String`**
(`with_role(name: impl Into<String>, ...)`), and `harw_types::AgentRole`
(session/message actor role) is a completely different enum from
`harw_agent_dsl::roles::AgentRoleId` (organizational role) — the spawner
never sees the DSL's role type at all today. Any future `SpawnTable` (§2.2)
would need to translate `AgentRoleId`/`DefinitionId` into the spawner's
existing string-keyed `roles: BTreeMap<String, ChildRoleDefinition>` rather
than assuming a typed role already flows through — that translation layer
does not exist and is not designed here.

## 5. Non-goals of this document

- Does not implement `AgentInstallation`, `AgentRuntimeAssembly`, or
  `CapabilityBinder`.
- Does not change `ExecutableAgentIr`, `AgentSession`, `ExtensionRegistry`,
  or `ManagedAgentSpawner`.
- Does not promise a timeline. It is scoping input for whoever picks up the
  next wave.

## 6. Suggested smallest first increment (if/when this is picked up)

A second source audit (against `harw-catalog/src/lib.rs`) found the actual
blocker to be one level deeper than first assumed: `ActivatedCapability`
carries `tools: Vec<String>` — bare **tool-name strings** — not
`Arc<dyn ToolProvider>` handles. There is no existing "host capability
catalog" anywhere in the workspace that maps a tool name (e.g. `"fs.read"`)
to the concrete provider that implements it; `ExtensionRegistry` is built by
hand-listing concrete providers (`FsToolProvider`, `ShellToolProvider`, …) in
`assemble_default_registry`, not by name lookup.

So `CapabilityBinder` as sketched in §2.2.1 is **not yet implementable**
without first deciding how a name-keyed host capability catalog is built and
who owns it (a new small registry crate? a static table in
`harw-registry-defaults`? something `harw-tool-fs`/`harw-tool-shell` each
register themselves into?). That is itself a real, non-trivial design
decision this document deliberately does not make — inventing an answer here
just to ship *some* code would contradict goal.md §6's caution against
declaring parts of the ontology stable prematurely.

Revised smallest first increment: **decide and document the host capability
catalog's ownership and lookup shape** (name → concrete provider) as a
follow-up design note, before writing `CapabilityBinder`. Implementing
`CapabilityBinder` itself only becomes a small, bounded task once that
decision exists.
