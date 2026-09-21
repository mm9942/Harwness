# Tool-Kanon v1 — Rust-idiomatische Umsetzung

**Status:** Design-Anker.
**Bindet an:** `philosophy.md` §10 (Operations ≠ Tools ≠ Permissions), §11 (Tool-Design als eigene UI), §12 (monotone Authority-Reduktion), §16 Invarianten 14 & 15.
**Inspirationen (siehe `docs/research/tool-inventory.md`):** codex-rs, Hermes, OpenClaw.

> **Ist-Stand (2026-09)**: `harw-tool-canon` wurde nie gebaut. Das reale
> Tool-Vokabular im Baum ist stattdessen `fs.*` (`read`/`write`/`list`/
> `search`/`glob`/`grep`), `shell.exec`, `web.fetch`/`web.docs_rs`/
> `web.crates_io`, `lens.ask`, `deps.*`, `browser.*`. Das `#[tool]`-Makro
> existiert; Typestate-Approval, `ToolHandle` und `SyscallBoundary` aus diesem
> Dokument existieren nicht. Kanon-Tools ohne Gegenstück im Ist-Code:
> `apply_patch`, `web_search`, `tool_search`, `request_user_input`,
> `request_permissions`, `cron_schedule`, `execute_code`.

---

## 1. Was Harwness heute schon richtig macht

Bereits im Workspace:

- `harw-tools` — reine Vokabular-Boundary (`ToolCall`, `ToolExecutor`, `ToolExecutionContext`, `ToolSpec`, `JsonSchema`, `ToolOutput`, `TracedToolExecutor`). Redaction-Regel für Tracing bereits durchgesetzt.
- `harw-operations` — `Operation`-Trait + Adapter (`command.rs`, `model_tool.rs`) → philosophy.md §10 („eine Operation, mehrere Oberflächen").
- `harw-sandbox` — `SandboxSpec` als serverseitig aufgelöste Authority-Boundary.
- `harw-macros` — Proc-Macros für Operation-Deklaration (bereits vorhanden, verifizieren).
- `harw-job-runtime` — Lease/Fencing/Heartbeat für langlebige Jobs (philosophy.md §7).
- `harw-memory` — HOT/WARM/COLD-Store + Heartbeat + STM + Context-Rendering (Wave 1–3).

Das ist die Grundlage, die Codex/Hermes/OpenClaw so nicht besitzen. Der Rest sind konkrete Executor-Impls für die Kanon-Tools.

---

## 2. Rust-Idiome, mit denen wir Codex/Hermes/OpenClaw schlagen

### 2.1 Typestate für Approval-Stufen

Codex kodiert Approval als Runtime-Flag, Hermes über Python-Decorators, OpenClaw über `ToolAvailabilityExpression`. Rust erlaubt Compile-Time:

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
    // execute-Signatur zwingt Approval-Callback zur Compile-Zeit
    pub async fn execute(&self, input: In, approval: ApprovalGranted) -> Out { … }
}
```

Ein `Tool<NeedsApproval, …>` **kann nicht** ohne `ApprovalGranted`-Token ausgeführt werden. Kein Vergessen, kein Bypass.

### 2.2 Zero-Copy Tool-Schema

Codex und Hermes bauen JSON-Schemas jedes Mal zur Laufzeit auf. Rust kann sie `const`-inline haben:

```rust
pub const APPLY_PATCH_SCHEMA: &str = include_str!("../schemas/apply_patch.json");

pub const SPEC: ToolSpecStatic = ToolSpecStatic {
    name: "apply_patch",
    description: "…",
    parameters_json: APPLY_PATCH_SCHEMA,   // borrowed, not allocated
    strict: true,
};
```

Dann `serde_json::value::RawValue` als Grenze; erst der Dispatch parst gezielt in das getypte Input-Struct.

### 2.3 Enum-Dispatch statt trait objects

Für den Hot-Path (12 Kanon-Tools) ist Monomorphisierung günstiger als `Box<dyn ToolExecutor>`:

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

Registry ist dann `HashMap<ToolName, CanonExecutor>` (kein `Box<dyn>` in der Hot-Loop). Für Plugin-Tools bleibt `Box<dyn ToolExecutor>` verfügbar — hybrides Modell.

### 2.4 `#[tool]`-Proc-Macro

`harw-macros` erweitert um:

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

Macro erzeugt:
- `ToolSpec`-Konstante
- `impl ToolExecutor for ShellExecutor`
- Registry-Eintrag via inventory-Pattern
- Compile-Time-Check, dass `args`-Typ ein `#[derive(JsonSchema)]` trägt

Codex hat solche Macros nicht (freie Rust-Handler mit manueller Registry). Hermes hat Python-Decorators ohne Compile-Time-Garantie.

### 2.5 Deferred Loading als `enum ToolHandle`

Hermes 3-stufiges Loading (`tool_search`, `tool_describe`, `tool_call`) lässt sich in Rust als:

```rust
pub enum ToolHandle {
    Resident(ToolSpec, Arc<CanonExecutor>),
    Deferred { name: ToolName, load: fn() -> Arc<CanonExecutor> },
}
```

`Deferred` gibt dem Modell nur den Namen (billige Katalog-Antwort), der eigentliche Executor wird erst bei `tool_call` materialisiert. Kein Reflection-Overhead — Rust-`fn`-Pointer.

### 2.6 `Cow<'a, str>` für Tool-Inputs

Viele Tool-Inputs sind kurze Referenzen (Pfade, IDs). Aktuelle Codex-Executor allokieren neue `String`. Wir können mit `Cow<'a, str>` sowohl Borrow als auch Owned akzeptieren — spart Alloc auf dem Hot-Path.

### 2.7 `#[non_exhaustive]` auf Output-Enums

Codex bricht bei Updates seiner ToolResult-Variante regelmäßig Konsumenten. Rust-Enum `#[non_exhaustive]` erzwingt beim Consumer explizites `_ => …`, das Erweiterungen ohne SemVer-Break erlaubt.

### 2.8 Sandbox als Trait-Bound

`ExecuteCodeExecutor` und `ShellExecutor` sollen ausschließlich mit einem `Sandbox: SyscallBoundary` arbeiten. Kein „unsandboxed Fallback in Debug-Builds":

```rust
pub struct ShellExecutor<S: SyscallBoundary + Send + Sync> { sandbox: S }
```

Compile-Time-Garantie, dass ein Test-Sandbox nicht in Produktion landet, wenn der Feature-Flag anders steht.

### 2.9 Lease-getragene Agent-Spawns

`DelegateTaskExecutor` liefert kein reines `ChildAgentId` zurück, sondern ein `LeaseToken<ChildAgent>` (aus `harw-job-runtime`). Das Typsystem verhindert, dass ein Caller den Child ansprechen kann, ohne ihn geleased zu halten (philosophy.md §7).

### 2.10 `tracing`-Spans als Compile-Time-Contract

Wir haben schon `TracedToolExecutor` als Blanket-Impl. Ausbaustufe: `#[tool]`-Macro deklariert Spans als `const &'static str`-Namen, sodass Konsumenten keine „ad-hoc"-Spans schreiben und Log-Aggregation stabil bleibt.

---

## 3. Zielarchitektur

```
harw-tools (Boundary, existiert)
    ├── ToolSpec / ToolCall / ToolOutput / ToolExecutor
    └── TracedToolExecutor (redaction)

harw-operations (Semantische Ops, existiert)
    ├── Operation-Trait
    └── Adapter { command, model_tool, channel }

harw-tool-canon (NEU — dieser Fanout)
    ├── canon.rs                 → enum CanonExecutor + Registry
    ├── shell.rs                 → shell_command
    ├── patch.rs                 → apply_patch
    ├── web.rs                   → web_search + web_fetch
    ├── discovery.rs             → tool_search (Deferred-Handles)
    ├── plan.rs                  → update_plan
    ├── approval.rs              → request_user_input + request_permissions
    ├── memory_tools.rs          → memory_read + memory_write (nutzt harw-memory)
    ├── cron.rs                  → cron_schedule (nutzt harw-job-runtime)
    ├── agent_spawn.rs           → delegate_task (LeaseToken-Rückgabe)
    └── exec_code.rs             → execute_code (Sandbox-Trait-Bound)
```

Jede Tool-Datei ist **file-scoped** — perfekter Fanout-Kandidat, disjunkte WriteSets.

---

## 4. Effizienz-Gewinne gegenüber Referenz

| Aspekt | Codex | Hermes | OpenClaw | harw-tool-canon |
|---|---|---|---|---|
| Approval-Fehler zur Compile-Zeit | nein | nein | teilweise (DSL) | **ja** (typestate) |
| Zero-Copy Schema | nein | nein | nein | **ja** (`include_str!`) |
| Enum-Dispatch Hot-Path | nein (dyn) | n/a | nein | **ja** |
| Deferred-Loading als `fn`-Pointer | ähnlich | Python-lazy | nein | **schneller** |
| Sandbox als Trait-Bound | Runtime-Check | Runtime | Runtime | **Compile-Time** |
| Lease-getragener Agent-Spawn | manuell | manuell | manuell | **typgetragen** |
| Redaction-Contract | Konvention | Konvention | Konvention | **Blanket-Trait** (bereits) |
| Tool-Descriptor als `const` | nein | nein | nein | **ja** |

---

## 5. Fanout-Plan (Wave 5)

Diese Datei ist der Vertrag. Der eigentliche Fanout in `harw-tool-canon` läuft in Wave 5 mit einem Focused-Coding-Task-Agent pro Tool-Datei (12 Agents), plus 1 Agent für `canon.rs` (Enum + Registry).

**Voraussetzung vor Wave 5:**
- Neues Crate `harw-tool-canon` mit `Cargo.toml` (Deps: `harw-tools`, `harw-sandbox`, `harw-memory`, `harw-job-runtime`, `harw-macros`, `serde`, `serde_json`, `tokio` per feature).
- Skeleton-`lib.rs` mit `pub mod canon;` — die konkreten Module werden erst vom Fanout angelegt.
- Ein Beispiel-Tool (`shell.rs`) als Referenz-Contract für die anderen Agents.

**WriteSet-Disjunktheit:** je Tool exakt eine Datei; `lib.rs` und `canon.rs` bleiben beim Orchestrator.

---

## 6. Nicht-Ziele

- Kein Ersatz für `harw-operations` — der `Operation`-Layer bleibt semantische Boundary.
- Kein Plugin/MCP-Adapter in Wave 5 — Kanon-Tools sind Rust-nativ. MCP-Bridge kommt separat.
- Keine Browser-/Computer-Use-Tools in Wave 5 — die brauchen platform-specific Backends.
- Keine Vision/TTS/Smart-Home-Tools — Provider-spezifisch, gehören zu `harw-provider-*`.
