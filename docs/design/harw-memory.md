# harw-memory — Long-Term Memory & Continuous Improvement

> Status: implemented · Last reviewed: 2026-09-24

## Purpose

A persistent, token-frugal memory layer for agent sessions, combining tiered
storage (HOT/WARM/COLD), append-only signal logging, and periodic
consolidation.

## Non-goals

- No external database (SQLite optional, not required).
- No automatic model call on every turn.
- No "silent inference" — memory is only written from an explicit signal
  (correction, post-task reflection, or a pattern repeated 3+ times).
- Not a replacement for skill configs or role definitions.

## Core principles

1. **Token budget first.** HOT is bounded to ≤100 lines (~2 KB) by
   construction. WARM/COLD are only loaded on a match.
2. **Explicit over implicit.** Signals are appended when the user corrects
   something or a reflection is signaled after a turn.
3. **Append-only + periodic consolidation.** Signals are written append-only;
   promotion/demotion runs in a separate, idempotent `maintain()` pass.
4. **Local-first.** All data on local disk, no network access on the default
   path.

## On-disk layout

Default root: `<HARWNESS_HOME>/memory/` (falls back to `~/.harwness/memory/`).

```
<home>/memory/
├── HOT.md                # <=100 lines; always loaded; hand- or agent-written
├── INDEX.md              # tier overview (counters, timestamps)
├── warm/
│   ├── INDEX.md          # <namespace> -> file path, size, last_used
│   └── {namespace}.md    # e.g. project/harwness.md, domain/rust.md
├── cold/
│   └── {archived}.md     # same naming scheme as warm/
├── signals/
│   ├── corrections.jsonl # append-only, NDJSON
│   ├── reflections.jsonl # append-only
│   └── patterns.jsonl    # candidates with a counter
└── state.json            # last_maintenance, usage_counters, promotion_state
```

## Tiers

| Tier | Location | Size limit | Load semantics | Persistence |
|------|-----|-------------|---------------|------------|
| HOT | `HOT.md` | <=100 lines | On every session start | User- or agent-edited |
| WARM | `warm/{ns}.md` | <=200 lines/file | On `recall(query)` with a namespace/keyword match | Auto (promoted from signals) |
| COLD | `cold/{ns}.md` | unbounded | Only on explicit request | Auto (demoted from WARM) |

## Transitions

- **Signal -> WARM:** a pattern in `patterns.jsonl` becomes a WARM line after
  3+ occurrences in 7 days.
- **WARM -> HOT:** explicit user confirmation (`/memory promote <ns>`).
- **WARM -> COLD:** no recall for 30 days.
- **COLD -> deleted:** never without user confirmation.

## Rust API (core trait)

```rust
pub trait Memory: Send + Sync {
    /// HOT tier as full text (<=100 lines guaranteed).
    fn hot(&self) -> Result<String, MemoryError>;

    /// Recall WARM/COLD by namespace + keyword.
    fn recall<'a>(&self, query: RecallQuery<'a>) -> Result<Vec<Entry>, MemoryError>;

    /// Append a signal (append-only).
    fn record(&self, signal: Signal) -> Result<(), MemoryError>;

    /// Maintenance: promotion, decay, compaction. Idempotent, single-locked.
    fn maintain(&self) -> Result<MaintenanceReport, MemoryError>;

    /// Stats without loading content — counters/timestamps only.
    fn stats(&self) -> Result<Stats, MemoryError>;
}

pub enum Signal {
    Correction { text: String, context: Option<String> },
    Reflection { context: String, lesson: String },
    PatternHint { key: String, note: String },
}
```

This is the original crate skeleton; the current implementation extends it
with the short-term ring buffer, the fact store, and the router described in
`docs/design/memory-v2.md` and `docs/design/memory-v3-ltm.md`.

## Integration in Harwness

- **OpContext service.** `Arc<dyn Memory>` lives in `ServiceMap`; operations
  fetch it via `ctx.service::<Arc<dyn Memory>>()`.
- **System-prompt injection.** The turn loop appends `memory.hot()` to the
  system prompt, after the role instructions.
- **Slash command `/memory`.** Subcommands: `list`, `recall <keywords>`,
  `record correction <text>`, `record reflection <lesson>`, `promote <ns>`,
  `demote <ns>`, `stats`, `maintain`.
- **Tracing.** Every `recall` / `record` / `maintain` emits spans with
  `namespace`, `tier`, `hit_count`, `bytes_loaded`.

## Security

- No secrets in memory (redaction filter on `record`).
- No network access on the default path.
- Never stores credentials, health data, or third-party PII.

## Implementation

Implemented (see `harw-memory/src/store.rs`, `types.rs`, `file_store.rs`,
`detect.rs`, `learning.rs`, `heartbeat.rs`, `promote.rs`, `facts.rs`,
`short_term.rs`, `extraction.rs`, `consolidation.rs`). See
`docs/design/memory-v2.md` for the STM/heartbeat/learning layer and
`docs/design/memory-v3-ltm.md` for the fact store, scopes, and
extraction/consolidation pipeline.
