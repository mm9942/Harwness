# Token Efficiency

> Status: partially implemented · Last reviewed: 2026-09-24

## Foundations already in the code

- `harw-core::history::ConversationHistory::tail_within_estimated_bytes` — a byte-budgeted history tail that never splits a call/result pair.
- `harw-core::context_budget` — a typed assembly pipeline (Gathered → Admitted → Budgeted → Rendered) with `ContextBudget`.
- `harw-context` — `DetailMode::{Full, Summary, References}`, `SectionStrength`, trust classes; reference-based rather than full-text rendering (`reference.rs`).
- `harw-context::ContextBudgetSpec::tighten` — monotone budget cuts; children never inherit more context than their parent, and per-section limits (`per_section`) can cap an individual section's share of the budget.
- Subagents: the child contract already returns only structured findings (Finding/Summary), never raw transcripts (`child.returns` = summary, `child.raw_transcript.*` excluded in `orchestrate.toml`).

## Provider-side prompt caching — Implemented

`harw-provider-http/src/cache_strategy.rs` decides, per provider and model, which cache-control markers an outgoing request body carries: none (`None`), an implicitly stable prefix with no explicit markers (`ImplicitPrefix`), or explicit `cache_control: {"type":"ephemeral"}` markers (`ExplicitEphemeral`). It covers two wire shapes:

- Chat-Completions-style bodies (`system`/`messages`/`tools`), used by OpenAI-compatible and DashScope chat endpoints (`apply_chat_cache_control`).
- Anthropic Messages bodies (`system` string, `tools`, `messages` with content arrays, `apply_messages_cache_control`).

The strategy is model-aware: a fixed list of DashScope models that support explicit cache control (`DASHSCOPE_EXPLICIT_MODEL_PREFIXES`), and a per-model `ModelToml::prompt_caching` override elsewhere in `harw-provider-http`. On an unexpected body shape (e.g. a missing `messages` field), the body is left unchanged rather than guessing (fail-closed: no marker rather than a misplaced one).

## Compaction of long sessions — Implemented

`harw-core/src/auto_compact.rs` implements a deterministic, no-model-call policy for when to compact the session history, used in the turn loop itself (not just as a manual `/compact` TUI command). Two triggers, combined:

1. **Task end (primary)**: when a task has finished and usage is already noticeable, compact then — summarizing a completed block is semantically cleaner than cutting mid-thought.
2. **Safety ceiling (secondary)**: regardless of task status, compact once usage crosses a fixed share of the context window — this stops one large task (e.g. exploration with many `fs.read` results) from blowing the window before it finishes.

The thresholds are relative to the configured context window rather than a fixed token count, since windows range from 128k (older models) to 1M+ (current frontier models): a 70% compact threshold (leaving headroom for the compact turn's own prompt + summary output, plus a normal reply after it), a 30% task-end threshold (below which compaction isn't worth the overhead), and an absolute ceiling (`DEFAULT_ABSOLUTE_CEILING_TOKENS`, 500,000 input tokens) — the effective threshold is `min(70% of window, ceiling)`.

## Subagent condensation

Already good: children return only findings, not transcripts.

- **Return budget**: `child.returns` in `orchestrate.toml` is `summary`, further bounded via `ContextBudgetSpec.per_section` (mechanism implemented; whether it's actually configured with a hard byte limit for every context program is worth spot-checking per program).
- **Dedup across parallel children**: multiple explorers on the same scope can return overlapping findings; the `digest` field on an evidence reference is available as a dedup key, but a join-time dedup pass using it is not confirmed implemented — open.

## Context-program discipline

- Use `DetailMode::References` in more context programs (currently set only in specific places; `orchestrate.toml` deliberately documents why it opts out where it does). Still open as a broader sweep.
- Reserve `must-include` for sections that are truly decision-relevant; leave the rest `preferred` so the budget trims there first. Still open as a broader sweep.

## Tool output at the source

- `fs.read`/`shell.exec` already cap output (a 64 KiB limit and other output limits). Anchoring "grep before read" as an explicit baseline instruction, so models don't read broadly in the first place, is not confirmed present in the current baseline instructions — open.
