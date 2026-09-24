# Tool Canon v1 — a Rust-idiomatic design

> Status: proposal · Last reviewed: 2026-09-24

**Ties into**: [`philosophy.md`](../philosophy/philosophy.md) §10 (operations ≠ tools ≠ permissions), §11 (tool design as its own UI), §12 (monotone authority reduction), §16 invariants 14 & 15.
**Inspiration** (see [`docs/research/tool-inventory.md`](../research/tool-inventory.md)): studying other open-source coding-agent projects' tool vocabularies (their own naming, not reproduced verbatim here).

> **Current state**: `harw-tool-canon`, the crate this document designs, was never built. The tool vocabulary that actually exists in the tree instead lives across several crates — `harw-tool-fs` (`fs.read`/`write`/`list`/`search`/`glob`/`grep`), `harw-tool-shell` (`shell.exec`), `harw-tool-web` (`web.fetch`/`web.docs_rs`/`web.crates_io`), `harw-tool-lens`, `harw-tool-deps`, `harw-tool-browser`, `harw-tool-doc`, `harw-tool-process`, `harw-tool-explorer`, `harw-tool-plan`, on top of the shared boundary crate `harw-tools`. The `#[tool]` proc-macro described in §2.4 does not exist; neither does the typestate approval, `ToolHandle`, or `SyscallBoundary` design below. None of the canon tool names in §2–§3 (`apply_patch`, `web_search`, `tool_search`, `request_user_input`, `request_permissions`, `cron_schedule`, `execute_code`) have a counterpart in the real code. This document is kept as a design reference for that possible future direction, not as a description of the current tool layer.

---

## 1. What Harwness already gets right

Already in the workspace:

- `harw-tools` — a pure vocabulary boundary (`ToolCall`, `ToolExecutor`, `ToolExecutionContext`, `ToolSpec`, `JsonSchema`, `ToolOutput`, `TracedToolExecutor`). A redaction rule for tracing is already enforced.
- `harw-operations` — the `Operation` trait plus adapters (`command.rs`, `model_tool.rs`) → philosophy.md §10 ("one operation, several surfaces").
- `harw-sandbox` — `SandboxSpec` as a server-side-resolved authority boundary.
- `harw-macros` — proc-macros for operation declaration.
- `harw-job-runtime` — lease/fencing/heartbeat for long-lived jobs (philosophy.md §7).
- `harw-memory` — HOT/WARM/COLD store plus heartbeat, STM, and context rendering.

That foundation is already in place. What's proposed below is a set of concrete executor implementations for a fixed canon of tools, built on top of it.

---

## 2. Rust idioms this design would use

### 2.1 Typestate for approval tiers

Rust allows encoding approval tiers at compile time rather than as a runtime flag or decorator:

```rust
pub struct AutoApprove;
pub struct NeedsApproval;
pub struct SandboxOnly;

pub trait ApprovalTier {}
impl ApprovalTier for AutoApprove {}
impl ApprovalTier for NeedsApproval {}
impl ApprovalTier for SandboxOnly {}

pub struct Tool<Approval: ApprovalTier, In, Out> {
    _marker: std::marker::PhantomData<(Approval, In, Out)>,
    // ...
}

impl<In, Out> Tool<NeedsApproval, In, Out> {
    // the execute signature forces an approval callback at compile time
    pub async fn execute(&self, input: In, approval: ApprovalGranted) -> Out { … }
}
```

A `Tool<NeedsApproval, …>` **cannot** run without an `ApprovalGranted` token. No forgetting, no bypass.

### 2.2 Zero-copy tool schema

Rather than building a JSON schema at runtime on every call, it can be `const`-inline:

```rust
pub const APPLY_PATCH_SCHEMA: &str = include_str!("../schemas/apply_patch.json");

pub const SPEC: ToolSpecStatic = ToolSpecStatic {
    name: "apply_patch",
    description: "…",
    parameters_json: APPLY_PATCH_SCHEMA,   // borrowed, not allocated
    strict: true,
};
```

Then `serde_json::value::RawValue` as the boundary; only dispatch parses it into the typed input struct.

### 2.3 Enum dispatch instead of trait objects

For the hot path (a fixed set of canon tools), monomorphization is cheaper than `Box<dyn ToolExecutor>`:

```rust
pub enum CanonExecutor {
    Shell(shell::ShellExecutor),
    ApplyPatch(patch::ApplyPatchExecutor),
    WebSearch(web::WebSearchExecutor),
    WebFetch(web::WebFetchExecutor),
    ToolSearch(discovery::ToolSearchExecutor),
    UpdatePlan(plan::UpdatePlanExecutor),
    RequestUserInput(approval::RequestUserInputExecutor),
    RequestPermissions(approval::RequestPermissionsExecutor),
    MemoryRead(memory::MemoryReadExecutor),
    MemoryWrite(memory::MemoryWriteExecutor),
    CronSchedule(cron::CronScheduleExecutor),
    DelegateTask(agent::DelegateTaskExecutor),
    ExecuteCode(sandbox::ExecuteCodeExecutor),
}

impl CanonExecutor {
    pub async fn execute(&self, ctx: &ToolExecutionContext, call: &ToolCall)
        -> ToolsResult<ToolOutput>
    { match self { … } }
}
```

The registry would then be `HashMap<ToolName, CanonExecutor>` (no `Box<dyn>` in the hot loop). Plugin tools would keep using `Box<dyn ToolExecutor>` — a hybrid model.

### 2.4 A `#[tool]` proc-macro

`harw-macros` would gain:

```rust
#[tool(
    name = "shell_command",
    approval = "sandbox_only",
    schema = "schemas/shell.json",
    surface = "model_tool",
)]
pub async fn shell(ctx: &ToolExecutionContext, args: ShellArgs) -> Result<ShellOutput, ShellError> {
    // …
}
```

The macro would generate: a `ToolSpec` constant, an `impl ToolExecutor for ShellExecutor`, a registry entry via an inventory pattern, and a compile-time check that the `args` type derives `JsonSchema`.

### 2.5 Deferred loading as `enum ToolHandle`

A three-stage loading model (search → describe → call, as several coding-agent tool layers use) can be expressed in Rust as:

```rust
pub enum ToolHandle {
    Resident(ToolSpec, Arc<CanonExecutor>),
    Deferred { name: ToolName, load: fn() -> Arc<CanonExecutor> },
}
```

`Deferred` gives the model only the name (a cheap catalog entry); the actual executor materializes only on `tool_call` — no reflection overhead, a plain Rust `fn` pointer.

### 2.6 `Cow<'a, str>` for tool inputs

Many tool inputs are short references (paths, IDs). Using `Cow<'a, str>` lets an executor accept either a borrow or an owned value — saving an allocation on the hot path.

### 2.7 `#[non_exhaustive]` on output enums

A `#[non_exhaustive]` enum forces consumers to write an explicit `_ => …` arm, which lets the tool-output shape grow without a semver break.

### 2.8 Sandbox as a trait bound

`ExecuteCodeExecutor` and `ShellExecutor` would work exclusively with a `Sandbox: SyscallBoundary`. No "unsandboxed fallback in debug builds":

```rust
pub struct ShellExecutor<S: SyscallBoundary + Send + Sync> { sandbox: S }
```

A compile-time guarantee that a test sandbox can't end up in production behind a differently-set feature flag.

### 2.9 Lease-carried agent spawns

`DelegateTaskExecutor` would return not a plain `ChildAgentId` but a `LeaseToken<ChildAgent>` (from `harw-job-runtime`). The type system then prevents a caller from addressing the child without holding its lease (philosophy.md §7).

### 2.10 `tracing` spans as a compile-time contract

`TracedToolExecutor` already exists as a blanket impl. A further step: the `#[tool]` macro would declare span names as `const &'static str`s, so consumers never write ad-hoc spans and log aggregation stays stable.

---

## 3. Target architecture

```
harw-tools (boundary, exists)
    ├── ToolSpec / ToolCall / ToolOutput / ToolExecutor
    └── TracedToolExecutor (redaction)

harw-operations (semantic ops, exists)
    ├── Operation trait
    └── Adapter { command, model_tool, channel }

harw-tool-canon (proposed — not built)
    ├── canon.rs                 → enum CanonExecutor + registry
    ├── shell.rs                 → shell_command
    ├── patch.rs                 → apply_patch
    ├── web.rs                   → web_search + web_fetch
    ├── discovery.rs             → tool_search (deferred handles)
    ├── plan.rs                  → update_plan
    ├── approval.rs              → request_user_input + request_permissions
    ├── memory_tools.rs          → memory_read + memory_write (uses harw-memory)
    ├── cron.rs                  → cron_schedule (uses harw-job-runtime)
    ├── agent_spawn.rs           → delegate_task (returns a LeaseToken)
    └── exec_code.rs             → execute_code (sandbox trait bound)
```

Each tool file would be file-scoped — a natural candidate for splitting the implementation work across disjoint write sets, one file at a time.

---

## 4. Expected efficiency gains

| Aspect | Typical dynamic-dispatch tool layer | `harw-tool-canon` (proposed) |
|---|---|---|
| Approval errors caught at compile time | no | **yes** (typestate) |
| Zero-copy schema | no | **yes** (`include_str!`) |
| Enum dispatch on the hot path | no (`dyn`) | **yes** |
| Deferred loading as an `fn` pointer | varies | **yes**, no reflection |
| Sandbox as a trait bound | runtime check | **compile time** |
| Lease-carried agent spawn | manual | **type-carried** |
| Redaction contract | convention | **blanket trait** (already true today) |
| Tool descriptor as a `const` | no | **yes** |

---

## 5. Build plan, if taken up

This document is the contract; the actual build of `harw-tool-canon` would split naturally into one focused task per tool file (roughly a dozen), plus one for `canon.rs` (the enum and registry).

**Prerequisites before starting**:
- A new crate `harw-tool-canon` with its own `Cargo.toml` (deps: `harw-tools`, `harw-sandbox`, `harw-memory`, `harw-job-runtime`, `harw-macros`, `serde`, `serde_json`, `tokio` behind a feature).
- A skeleton `lib.rs` with `pub mod canon;` — the concrete modules would be added one at a time.
- One example tool (`shell.rs`) as the reference contract for the rest.

**Write-set discipline**: exactly one file per tool; `lib.rs` and `canon.rs` stay with whoever coordinates the build.

---

## 6. Non-goals

- Not a replacement for `harw-operations` — the `Operation` layer remains the semantic boundary.
- No plugin/MCP adapter in this design — canon tools are Rust-native; an MCP bridge would be separate.
- No browser/computer-use tools here — those need platform-specific backends.
- No vision/TTS/smart-home tools — provider-specific, and belong in `harw-provider-*`.
