# Memory v2 — STM/LTM, Self-Learning, Continuous Improvement

> Status: implemented · Last reviewed: 2026-09-24

Extends `docs/design/harw-memory.md` with an in-process short-term layer, a
deterministic promotion/decay heartbeat, and signal-based self-learning, all
without any model call in the hot path.

## 1. Goal

A token-frugal memory layer that:

1. **Short-Term Memory (STM)** — holds volatile turn context in-process (no I/O),
2. **Long-Term Memory (LTM)** — manages the persistent HOT/WARM/COLD tiers,
3. **Self-Learning** — extracts corrections, reflections, and pattern
   candidates from signals,
4. **Continuous Improvement (heartbeat)** — promotes/demotes/archives
   deterministically,

while delivering the smallest highly-significant token set per turn. No LLM
calls in the hot path; all rules are pure Rust.

## 2. Layer overview

```
+---------------------------------------------------------------+
|  Turn Context Assembly (harw-core, outside this crate)        |
|      ^ pulls (bounded)                                        |
+---------------------------------------------------------------+
   |                                    |
   |  hot() + recall(namespace,kw)      |  stm.snapshot()
   v                                    v
+-------------------------+    +----------------------------+
|  LTM (FileMemoryStore)  |    |  STM (in-process)          |
|  HOT <=100 lines        |    |  Ring buffer, N=32 default |
|  WARM per namespace     |    |  Salience score, TTL       |
|  COLD archive           |    |  Send + Sync via RwLock    |
+-------------------------+    +----------------------------+
        ^                             ^
        |  maintain() (heartbeat)     |  push(...) per turn
+---------------------------------------------------------------+
|  Signals (append-only JSONL)                                  |
|  correction | reflection | pattern_hint                       |
+---------------------------------------------------------------+
        ^
        |  detected + recorded from the running turn
```

## 3. Short-Term Memory (STM)

**File:** `harw-memory/src/short_term.rs`.

- Pure in-process storage — no filesystem I/O.
- `struct ShortTermMemory { inner: RwLock<Inner> }` (`Send + Sync`).
- `Inner` holds a `VecDeque<StmEntry>` with a `capacity` (default 32), a
  `token_budget: usize` (default 2048 tokens, estimated as
  `content.len() / 4`), and a `session_id`.
- `StmEntry { at: Timestamp, role: StmRole, salience: u8 /*0..=100*/, content: String }`.
- `StmRole { User, Assistant, Tool, System }`.
- API: `new`, `push`, `snapshot` (clones), `render(max_tokens)` (compresses
  from the back, most recent first, dropping the lowest-salience entries to
  stay under budget), `clear`.
- **Eviction:** over `capacity`, the lowest-salience entry is popped first
  (oldest on a tie). Over the token budget, the same rule applies until back
  under budget.

## 4. Long-Term Memory (LTM)

Implemented in `harw-memory/src/{store.rs,file_store.rs,types.rs,workflow.rs}`.
The tier contract is stable: `Tier::Hot::max_lines() = 100`,
`Warm = 200`, `Cold = None`.

STM does not feed LTM directly. Signals are still created explicitly via
`store.record(Signal::…)` (from the TUI/core), or promoted from an STM
reflection.

## 5. Self-Learning

**File:** `harw-memory/src/learning.rs` (extends `detect.rs`).

- `score_correction(text: &str) -> u8` — a 0..=100 score from a keyword set
  (via `detect::detect_correction`).
- `PatternCounter` — a `key -> (count, first_seen, last_seen)` map, persisted
  to `signals/patterns.json`, with `observe`, `is_promotable(key, now,
  window_days, threshold)` (default `window_days = 7`, `threshold = 3`), and
  `prune`.
- `LessonRule { keyword: &'static str, weight: u8 }` — a static table of
  correction-trigger keywords (German and English).
- `FileMemoryStore::maintain()` loads the `PatternCounter` state and promotes
  `PatternHint` signals to WARM once `is_promotable` returns true.

## 6. Continuous Improvement — heartbeat

**File:** `harw-memory/src/heartbeat.rs`.

- `Heartbeat<'a, M: Memory> { store: &'a M, clock: fn() -> Timestamp }`.
- Rules:
  - `PatternHint` 3x in 7d -> WARM (`domain/<key>.md`).
  - HOT entry unused for 30d -> WARM (`domain/inactive.md`).
  - WARM entry unused for 90d -> COLD.
  - HOT overflow (>100 lines) -> oldest by `last_used` -> WARM.
- `tick(store, now) -> MemoryResult<HeartbeatReport>` is idempotent.
- `HeartbeatReport { promoted, demoted, archived, hot_lines_after }`.

## 7. Cost and resource budget

| Aspect | Value |
|---|---|
| HOT tokens/turn | <= ~1,500 (~100 lines x 15 tokens) |
| STM tokens/turn | <= 2,048 (configurable default) |
| WARM load/turn | exactly 1 namespace (namespace + keyword match) |
| I/O per turn | 1x `hot()` + 1x `recall()` |
| I/O per heartbeat | O(HOT+WARM), append-only |
| Lock contention | RwLock (STM), file locks (LTM) |
| LLM calls | 0 in the memory layer |

## 8. LTM file layout

```
<root>/
  HOT.md
  workflow.json
  signals/
    corrections.jsonl
    reflections.jsonl
    patterns.jsonl
    patterns.json       # PatternCounter state (learning.rs)
  warm/
    domain/<key>.md
    project/<name>.md
  cold/
    <namespace>.md
```

## Implementation

Implemented: `harw-memory/src/short_term.rs`, `heartbeat.rs`, `learning.rs`.
The model-catalog behavior/router work referenced in earlier drafts of this
document is tracked separately — see `docs/design/model-catalog-v2.md`.
