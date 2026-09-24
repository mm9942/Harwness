# harw-memory Promotion/Decay Runtime

> Status: implemented · Last reviewed: 2026-09-24

## Invariants

1. **Never infer from silence.** Only explicit signals (correction,
   reflection, pattern hint) are valid triggers.
2. **Promotion after 3x in 7 days.**
3. **Decay: 30d unused -> COLD, 90d unused -> flagged** — never deleted
   silently.
4. **The signal log is the source of truth**, not `state.json` bookkeeping.
5. **Idempotent, state-machine driven.** A restart resumes at the last
   committed step.
6. **Local-first**, no network requests.

## `src/promote.rs`

```rust
/// Result of a single promotion pass.
pub struct PromotionOutcome {
    pub warm_created: usize,
    pub demoted_to_cold: usize,
    pub archived_warnings: Vec<String>,
}

/// Evaluates the signal log since `since` and returns candidates.
pub fn evaluate_signals(
    signals: &[Signal],
    now: OffsetDateTime,
    window: Duration,
    threshold: usize,
) -> Vec<PromotionCandidate>;

pub struct PromotionCandidate {
    pub namespace: String,   // e.g. "correction/thats-wrong"
    pub content: String,     // aggregated lines
    pub hit_count: usize,
}
```

## `FileMemoryStore::maintain()`

In the `WorkspaceSynced -> AgentCompleted` step:

1. Read the signal log since `state.last_maintenance` (diff only).
2. Call `evaluate_signals(...)` with a 7-day window and a threshold of 3.
3. For each candidate: write or extend `warm/<namespace>.md` (append, with a
   timestamped line).
4. Feed `warm_created` and `demoted_to_cold` into the `MaintenanceReport`.

In the `AgentCompleted -> BaselineCommitted` step:

1. Inspect all WARM files, compare `last_used` against `state.json`.
2. A file unused for 30 days is moved to `cold/` (atomic rename).
3. A file in `cold/` unused for 90 days is pushed to
   `archived_warnings` — but never deleted.

## `INDEX.md` writer

Rewritten after every successful `maintain()`:

```markdown
# Memory Index (updated 2026-07-16T00:23Z)

## HOT (`HOT.md`, X lines)

## WARM
| Namespace | Lines | Last used |
|-----------|--------|-----------------|

## COLD
| Namespace | Lines |
```

## `state.json` field

```json
{
  "last_maintenance": "…",
  "signals_seen": 42,
  "usage_index": { "namespace/foo": {"count": 3, "last_used": "…"} },
  "promoted_to_hot": 0,
  "demoted_to_cold": 0,
  "warm_created": 0,
  "cold_warnings_last_pass": []
}
```

## Boundaries (deliberately out of scope here)

- **No LLM consolidation.** That is the extraction/consolidation pipeline in
  `docs/design/memory-v3-ltm.md` §5.2–5.3.
- **No automatic HOT editor.** WARM -> HOT stays manual via
  `/memory promote <ns>`.
- **No cross-namespace merges.** One candidate corresponds to one namespace
  slot.

## Implementation

Implemented: `harw-memory/src/promote.rs`, wired into
`FileMemoryStore::maintain()` in `store.rs`/`file_store.rs`. See
`docs/design/memory-v2.md` §6 for the heartbeat that drives it.
