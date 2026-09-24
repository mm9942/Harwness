# Memory v3 — Long-Term Memory (LTM) with Scopes, Facts, and Consolidation

> Status: implemented · Last reviewed: 2026-09-24

Builds on `docs/design/memory-v2.md` (the STM/LTM HOT/WARM/COLD tiers,
signals, and heartbeat).

## 1. Why v3

v2 provides tiers, signals, and a deterministic heartbeat, but:

1. **No project scoping.** Everything lands in the profile; memories from
   project A show up in project B.
2. **No addressable facts.** HOT/WARM are line lists; a single fact cannot be
   individually edited, linked, sourced, or deleted.
3. **No model in the loop.** Memories only arise from explicit signals; a
   session where nobody runs `/memory record` leaves nothing behind.
4. **No usage-based decay.** There are no usage counters, so no justified
   eviction either.

v3 adds these four while leaving v2 in place as a fast, LLM-free read layer.

## 2. Storage locations by lifetime

| Scope | Location | Content |
|---|---|---|
| Session | Process (`short_term.rs`) | Ring buffer of the running conversation, no I/O |
| Project | `<project>/.harw/memories/` | Facts about this repo: architecture, conventions, decisions, open points |
| Global | `~/.harw/profiles/<p>/memories/` | Facts about the user, recurring preferences, cross-project tools |

Both persistent roots share the same layout (the existing `FileMemoryStore`
layout is unchanged; `facts/` and `MEMORY.md` are added):

```text
<root>/
  MEMORY.md              <- index: one line per fact, generated
  facts/<slug>.md        <- one fact per file (frontmatter + text)
  HOT.md  INDEX.md       <- v2, unchanged
  warm/<namespace>.md  cold/<namespace>.md
  signals/*.jsonl  state.json  workflow.json
  usage.json              <- usage counters per fact
```

The project's `.harw/memories` is deliberately **inside the repo**, so it is
versioned and shared. It grants no rights — permissions (allow rules, work
directories) live outside the repo per `docs/design/config-scopes.md` §2.

## 3. The fact

```markdown
---
name: tui-approval-arming
description: Why the approval dialog has an arming delay
type: decision            # fact | decision | preference | pitfall | reference
scope: project             # project | global
created: 2026-09-14T05:12:00Z
updated: 2026-09-14T05:12:00Z
confidence: 0.9            # 0.0-1.0
sources:                   # evidence, optional
  - session:01H…           # transcript
  - file:harw-tui/src/app.rs#L2402
tags: [tui, approval]
---

Keypresses only count after 250ms and only while the panel is visible.

**Why:** a turn can open a panel at exactly the moment the user is typing —
without the delay that would be an unintended approval.

Related: [[tui-tool-cell]], [[permissions-scopes]]
```

Rules:
- A fact is **one** thing, at most ~15 lines. Anything longer belongs in `docs/`.
- `name` is the filename without `.md`, kebab-case, stable — renaming breaks
  `[[links]]`.
- `[[name]]` may point at a fact that doesn't exist yet; that marks a gap.
- `type` steers selection: `preference` and `decision` are loaded by
  default, `reference` only on a keyword match.
- No secrets in a fact. Redaction runs over key/token patterns before write.

`MEMORY.md` is a generated index (`- [Title](facts/<slug>.md) — description`)
so a human or a small model can survey the set without reading every file.

## 4. Read path (hot path, no model)

`MemoryContextProvider` delivers at most `memory_token_budget` (default 1500
tokens) per turn:

1. **Always:** the project's `MEMORY.md` index, truncated to 40 lines.
2. **Always:** all facts with `type = preference` (global and project),
   sorted by `updated`.
3. **As needed:** facts with keyword matches against the user message
   (existing `context_selector`, BM25-like over `name`, `description`,
   `tags`, text) — project before global.
4. **Then:** `HOT.md` as in v2.

Every delivered fact increments `usage.json[slug].count` and sets
`last_used`. This costs one buffered write per turn, no read in the hot path.

No model call. Order and budget are deterministic and testable.

## 5. Write path

### 5.1 Immediate (deterministic)
- `/memory record <text> [--global]` writes a fact directly (default:
  project).
- Existing signals (`correction`, `reflection`, `pattern_hint`) are
  unchanged and still flow into HOT/WARM.

### 5.2 Post-session extraction (phase 1, model)
Triggered when a new session starts, in the background, never in the hot
path — so the running session pays nothing.

- Selection: completed project transcripts older than `idle_min_secs`
  (default 300), younger than `max_age_days` (default 30), not yet
  extracted (marker in `state.json`).
- Input: filtered turn items (user text, assistant conclusions, errors, file
  and command names), at most 30 KiB.
- The prompt requires strict structure: `facts: [{name, description, type,
  body, confidence, sources}]`, at most 5 per session, "nothing that is
  readable from the code or git log".
- The result is redacted, then stored as a **candidate** under
  `facts/_incoming/<slug>.md`.

### 5.3 Consolidation (phase 2, agent)
- Runs when `_incoming/` is non-empty, under a lock (one consolidation per
  root at a time).
- A dedicated agent (`memory-steward`, read-only tools plus write access
  below the memory root, no network, no approvals) receives: the index,
  affected existing facts, all candidates, and the diff since the last
  baseline.
- Task: merge rather than accumulate — merge duplicates, flag and resolve
  contradictions via `contradiction_index`, lower `confidence` on or delete
  stale facts, rewrite `MEMORY.md`.
- Afterward: reset the memory root's git baseline (project: a normal commit
  candidate, not auto-committed; global: its own `.git` in the root).

### 5.4 Eviction
On heartbeat (`maintain()`):
- `last_used` older than `max_unused_days` (default 90) and `usage_count ==
  0` -> `confidence *= 0.5`; below 0.2 -> moved to `cold/`.
- A fact whose `sources` have all disappeared (file deleted, transcript
  gone) is flagged, not automatically deleted.

## 6. Commands

| Command | Effect |
|---|---|
| `/memory` | Index with counts per scope and type |
| `/memory record <text> [--global]` | Write a fact |
| `/memory recall <keyword>` | Search both scopes, shows provenance |
| `/memory forget <name>` | Delete a fact (project or global), with confirmation |
| `/memory consolidate` | Trigger phase 2 immediately |
| `/memory stats` | Usage, decay, candidates |

Config `[memory]`: `enabled`, `project_enabled`, `extraction = true|false`,
`extraction_model`, `token_budget`, `max_unused_days`, `max_facts_per_session`.

## 7. Invariants

1. No model call in the read path.
2. A fact is a file; the file is the source of truth. The index and usage
   counters are always re-derivable.
3. Project memories never leave the project; global memories carry no
   project secrets (redaction plus a path check on write).
4. Extraction and consolidation are idempotent and never lose work to each
   other via markers and locks.
5. Every auto-generated fact carries its source; no source, no automatic
   fact.
6. Deletion is always possible and complete — including from the index and
   the counters.

## Implementation

Implemented: `harw-memory/src/facts.rs` (fact type, frontmatter, load/write/
delete, slug, redaction, `MEMORY.md` generation, usage counters),
`context_provider.rs` / `context_selector.rs` (read path and budget),
`extraction.rs` (phase 1), `consolidation.rs` and `contradiction_index.rs`
(phase 2), `outcome_tracker.rs` and `heartbeat.rs` (decay).
