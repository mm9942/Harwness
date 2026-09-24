# contract-fanout-migration — Contract Master + per-file fan-out orchestration

A harness for mechanical, multi-file migrations (e.g. swapping an HTTP framework, renaming a
cross-cutting API, converting a serialization format) where:
- The change touches a small, fixed set of files.
- Those files are **tightly coupled** — they must agree on exact type/function signatures.
- The work is mechanical (no architectural judgment calls left to make) but large enough in
  agent-count that the user wants deliberate parallelism rather than one agent doing it inline.

This is a **heavier, more expensive** pattern than just doing the rewrite directly or routing
through a single development-orchestrator pass. Use it when the user asks for it explicitly, or
when file count × coupling genuinely justifies the coordination overhead — not as a default for
small changes. If unsure whether the user wants the full fan-out vs. a leaner pass, ask (see
`AskUserQuestion`) rather than assuming — the overhead-vs-safety tradeoff is a real judgment call,
not a fixed rule.

## Structure

```
Top-level orchestrator (you, main loop)
├── writes Contract Master document FIRST, alone, before spawning anything
├── spawns one file-orchestrator per target file, run_in_background: true, all in one message
│   └── each file-orchestrator:
│       ├── reads the Contract Master + the current state of its file
│       ├── splits its file into ~5 responsibility areas
│       ├── spawns 5 subagents in parallel (one message, multiple tool calls)
│       │   └── each subagent returns ONLY a text code fragment — never writes the file itself
│       ├── assembles the fragments into one coherent, compilable file
│       └── writes the file itself (single writer per file — no race conditions)
├── waits for all file-orchestrators to report back
├── performs any workspace-wide step no subagent is allowed to run (see below)
└── delegates final verification to a build/test-verifier agent
```

## 1. Write the Contract Master first, alone

Before spawning anything, read enough of the current code (old implementation + the reference
implementation being migrated *to*, if one exists elsewhere in the codebase) to write one
document containing, for every file being migrated:

- Every public type/struct/enum that crosses a file boundary, with exact field types.
- Every function signature that another file calls (parameters, return type, error type).
- The reference pattern being followed (cite the exact file/line range of a working example
  already in the codebase, if one exists — don't invent a pattern from scratch when a workspace
  precedent exists).
- Non-negotiable constraints (banned dependencies, error-handling policy, logging policy —
  pull these from CLAUDE.md/AGENTS.md so every subagent inherits them without re-deriving).

Store it at a stable path subagents can all read (e.g.
`docs/contracts/<task>-contract.md` in the repository). This document is the only thing
that lets 15+ agents who never see each other's output produce mutually-compatible code.

## 2. Spawn file-orchestrators, not subagents, directly

Spawn exactly one Agent per target file, all `run_in_background: true`, all in a single tool-call
batch. Each file-orchestrator prompt must include:
- The Contract Master path (read it first, follow it exactly).
- The current file's path and a summary of what's being replaced and why.
- The reference implementation's path (if any).
- The 5 responsibility areas to split into (decide these based on the file's actual structure —
  e.g. types/router-setup/handler-A/handler-B/helpers for a small HTTP file).
- Explicit instruction: subagents return code as **text**, they do not write the file — only the
  file-orchestrator writes, to avoid concurrent-write races on the same file.
- Explicit instruction: neither the file-orchestrator nor its subagents may run build/test/package
  commands (`cargo`, `make`, `npm run`, etc.) — that's reserved for the top-level orchestrator,
  per this project's build-safety rules. State the exact forbidden commands.
- A response cap (e.g. "report back in under 150 words: what the final file exports, any
  deviations from the contract").

## 3. Reserve workspace-wide steps for yourself

Anything that touches shared state outside a single target file — dependency manifest edits
(`cargo add`, `package.json`), workspace-lock regeneration, deleting the old dependency — happens
in the top-level orchestrator only, after all file-orchestrators report back. Never delegate this
to a subagent; it's exactly the kind of step that races or duplicates badly across parallel
agents.

## 4. Verify, don't assume

Once all files are written and the manifest is updated, delegate to a dedicated build/test
verifier (e.g. `focused-build-verifier-agent` or the project's own `make check` / `make
clippy-tests` / `make test` sequence run by the top-level orchestrator). Report the verification
result to the user before declaring the migration done — assembled-from-fragments code is exactly
where signature drift between subagents likes to hide.

## Guardrails

- Don't reach for this pattern by default — it costs roughly 4x the agent-spawns of a single
  development-orchestrator pass for the same LOC. Confirm with the user first if the task size
  doesn't obviously justify it (see `[[acme-s3iced-axum-migration-fanout]]` memory for a concrete
  precedent where the user confirmed the full pattern despite the small LOC count).
- If a file-orchestrator's subagents disagree on a signature the Contract Master didn't specify,
  that's a Contract Master gap — the file-orchestrator resolves it by following the majority/most
  contract-consistent fragment, not by guessing per-fragment.
- Subagents that never see the whole file are prone to duplicating imports or producing
  incompatible `use` statements — the assembling file-orchestrator must dedupe and validate the
  final import block itself before writing.
