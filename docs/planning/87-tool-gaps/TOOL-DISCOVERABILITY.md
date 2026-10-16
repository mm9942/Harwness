---
id: PL-88-TOOL-DISCOVERABILITY
title: "Tool discoverability and explicit defaults"
status: proposed
date: 2026-10-01
baseline: dev@70faabed9182834792a5f36e67cd80535792542a
---

# Tool discoverability and explicit defaults

## 1. Problem (observed)

- The model often does not know which tool exists, where it lives or how to
  call it.
- Defaults are not written in TOML: the scaffolded profile `config.toml`
  (`harw-home/src/scaffold.rs`) contains only a commented provider/model and
  `[mcp_listener]`; global `config.toml` only `[logging]`/`[onboarding]`.
  `harw-config` knows `[tools.plan]` (and `[tools.doc]` in merge), the other
  tool families have no visible section. Real defaults live in Rust code.
- `harw catalog` is a hidden legacy alias for model lists, not a tool listing.

## 2. Target

### 2.1 One tool index, derived from the registry

A typed `ToolIndex` generated from the same sources that register tools
(capability catalog, `#[tool]` metadata), never hand-written:

```text
name | family | one-line "use when" | input summary | permission tier |
enabled in this profile? | config section | related tools
```

Exposed three ways from the same data: an agent tool (`tool.index`, optional
family filter, cheap output), `harw tools [family]` for humans, and a short
generated block in the system prompt (families + "call `tool.index` for
details"), so the model learns where to look instead of receiving every schema.
Disabled-by-profile tools are listed with the reason, so "why can't I use X"
has an answer.

### 2.2 Defaults visible in TOML

- `harw config defaults` prints a complete, commented TOML with **every
  default value** taken from the Rust default structs (single source), including
  per-tool sections.
- Scaffold writes `config.defaults.toml` next to `config.toml` as a read-only
  reference (regenerated on update, never merged), and `config.toml` stays the
  user overlay.
- A test round-trips the generated file: it parses and equals the built-in
  default config. This makes it impossible for the file and code to drift.

### 2.3 Descriptions as a contract

A lint in the tool registry test: every tool has a non-empty "use when" line,
every input field a description, and every config-backed default is named. A
tool without them fails the gate.

## 3. Work order

1. `ToolIndex` type + generator from existing metadata (read-only, no behavior).
2. `harw tools` CLI and `tool.index` agent tool.
3. `config defaults` generator + round-trip test; scaffold writes the reference.
4. Registry lint.
5. System-prompt block.

## 4. Open questions for the owner

- Is the registry metadata sufficient for "use when" text, or must it be added
  to `#[tool]`? (Likely added; touches `harw-macros`.)
- Should `tool.index` be always loaded or deferred like other tools?
