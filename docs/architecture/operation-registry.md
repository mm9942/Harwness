# Operation Registry — Single Source of Truth

> Status: implemented · Last reviewed: 2026-09-24

## Overview

HARW defines every command as a single `#[operation(...)]` declaration. From that
one annotated handler, the proc-macro emits an `OperationMeta` value that is
inserted into `OperationRegistry`. All downstream subsystems — TUI discovery, help
rendering, alias resolution, dispatch, and completion — derive their knowledge from
the same registry snapshot. No command is listed in two places; changing an
operation declaration is the only edit required to update the entire system.

---

## Pipeline

```
#[operation(name = "...", aliases = [...], command(path = "..."))]
        |
        | proc-macro expansion (harw-macros)
        v
   OperationMeta  ──────────────────────────►  OperationRegistry
        |                                              |
        | harw-ops::register_all()                     | harw-tui/src/registry.rs
        |                                              v
        |                               CommandRegistry::from_operation_registry
        |                                              |
        |                                              v
        |                             Discovery · Help · Completion · Dispatch
        |
        v
   (handler fn called at dispatch time)
```

`harw-ops::register_all()` populates `OperationRegistry` at startup.
`CommandRegistry::from_operation_registry` consumes the snapshot and produces the
`CommandRegistry` that the TUI and dispatch loop query at runtime.

---

## Metadata (harw-operations/src/operation.rs)

`OperationMeta` carries the following fields:

| Field        | Type / shape        | Meaning                                                      |
|--------------|---------------------|--------------------------------------------------------------|
| `name`       | `&'static str`      | Canonical command name; must be unique across the registry.  |
| `summary`    | `&'static str`      | One-line human-readable description shown in help output.    |
| `domain`     | `Domain`            | Logical grouping (e.g. `Model`, `Provider`, `Session`).      |
| `permission` | `Permission`        | Minimum capability level required to invoke the command.     |
| `surfaces`   | `&'static [Surface]`| Rendering surfaces where the command appears (TUI, CLI, …).  |
| `aliases`    | `&'static [&'static str]` | Short-form alternatives that resolve to the same handler. |
| `category`   | `Category`          | Display category used for grouping in help and completion.   |

---

## Aliases

Aliases declared on `OperationMeta` are propagated end-to-end by
`CommandRegistry::from_operation_registry` (harw-tui/src/registry.rs): every alias
in `OperationMeta.aliases` is written into `CommandSpec.aliases` without filtering
or transformation.

The invariant is strict: if `/model`, `/provider`, and `/effort` are the canonical
names, then `/m`, `/p`, and `/reasoning` declared as their respective aliases
dispatch to the exact same handler functions. There is no separate alias table —
resolving an alias and resolving the canonical name follow the same code path and
produce the same `CommandSpec`.

---

## Duplicate Detection

Both registry construction steps enforce uniqueness and fail explicitly on collision.

`OperationRegistry::try_register` returns `Err` when:

- A canonical name collides with an existing canonical name (case-insensitive
  comparison).
- An alias collides with any existing canonical name.
- An alias collides with any existing alias (regardless of which operation owns it).
- A self-collision occurs: the canonical name equals one of its own aliases, or the
  same alias string appears more than once within a single operation declaration.

`CommandRegistry::from_operation_registry` applies the same checks during the
conversion step and returns `Err` on any of the above conditions.

The infallible `OperationRegistry::register` is provided for convenience in
contexts where the caller can guarantee uniqueness (e.g., the built-in pack where
all names are statically known). It panics on any collision. Downstream code that
processes user-provided or dynamically loaded operation packs must use `try_register`
and propagate the error rather than panic.

---

## Built-in Pack (harw-ops::register_all)

The 18 built-in operations are registered explicitly by calling
`harw-ops::register_all()`. Each operation is added via a direct `register` call
in source order.

The decision to use explicit registration over an `inventory`-based auto-collection
channel was intentional:

- **Determinism**: explicit ordering makes the registry snapshot reproducible and
  diffable across builds.
- **Linker-independence**: `inventory`-style distributed registration depends on
  linker-section retention that can silently drop items in LTO or cross-compilation
  scenarios.
- **Auditability**: the complete built-in set is readable in one function without
  tracing linker magic.

An `inventory`-based channel is reserved for future dynamic packs (runtime plugin
loading, user-contributed extension crates). It is not consumed today.

---

## Multi-Segment Command Paths

`Surface::Command { path }` accepts paths with multiple `/`-separated segments
(e.g., `/agent/new`, `/session/export`). These are accepted and stored in the
registry unchanged — no silent truncation or flattening occurs.

Validation is performed by `CommandName::parse`, which rejects:

- Empty paths.
- Paths containing only whitespace.
- Paths containing control characters.

A multi-segment path that passes `CommandName::parse` is treated identically to a
single-segment path for purposes of dispatch, help, and completion indexing.

---

## Adding a New Command

1. Annotate the handler function:
   ```rust
   #[operation(name = "export", aliases = ["ex"], command(path = "/session/export"))]
   pub fn handle_export(ctx: &mut Context) -> Result<(), Error> { ... }
   ```

2. If the operation belongs in the built-in pack, add one line to
   `harw-ops::register_all`:
   ```rust
   registry.register(harw_ops::export::META);
   ```

3. That is the only change required. TUI discovery, help rendering, completion
   indexing, alias resolution, and dispatch all derive from the registry
   automatically — no secondary tables to update.

4. If the new operation declares aliases, add a unit test asserting that each alias
   resolves to the same handler as the canonical name. Place the test in
   `harw-tui/src/registry.rs` under `#[cfg(test)] mod tests`.

---

## What Has Not Moved Yet

- **IR-based agent instantiation**: Open. The pipeline from `harw-agent-dsl`
  through the DSL runtime to `AgentSession::new` is not yet wired. Operations
  continue to receive `AgentRole` values from `harw-types` directly; the DSL
  IR path is parsed and validated but not executed at session startup.

- **`harw::Harness::builder()` fluent API**: Open. The `harw` facade crate
  (`harw/src/lib.rs`) is present in the workspace and re-exports canonical
  types (`OperationMeta`, `OperationRegistry`, `CommandRegistry`, domain
  enums). A `Harness::builder()` constructor and its associated
  configuration DSL are not shipped.
