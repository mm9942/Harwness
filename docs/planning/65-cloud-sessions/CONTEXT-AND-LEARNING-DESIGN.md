# Context management strand and automated learning (design)

Status: DESIGN, not implemented. Grounded in a read-only survey of consolidate/main (path:line in the survey; summarized here). Nothing here is built until approved.

## 1. Problem

- Context is managed in five places (harw-memory policy/selector, harw-runtime provider, harw-core budget/compaction, DSL context programs, harw-context ceiling). Nobody owns "what was offered, what was used, what was omitted and why"; omissions are only metrics.
- Learning is mostly library code without callers: `outcome_tracker`, `contradiction_index`, `learning.rs`, `promote`, LLM extraction, `ContextProposal`, context steward. Only capture (heuristic), consolidation, decay and the review-gated dream run are wired.
- No feedback signal: `record_usage` counts delivery, not benefit. No evaluation. DoD/Sentinel signals have no consumer.

## 2. Principles

1. One ledger, many readers: every context offer is recorded once (the *context ledger*); learning, tuning, doctor and retention read it.
2. Learned data is untrusted by default: provenance + trust class travel with every fact; nothing learned is promoted without a gate (auto only for low-risk, project-local, reversible changes).
3. Everything is a job: extraction, scoring, consolidation, forgetting run as jobs in the existing job system (async, deadline, cancel, visible in `harw jobs`). No second scheduler.
4. Declare once (macro): context sources and learning signals are declared in one table, like `retention_classes!`; config, registry, doctor and job read it.
5. Retention-aware: learning reads ephemeral data before it is swept, never from opt-in security classes except as derived counters.

## 3. Context strand

**C1 Context ledger** (new, small crate `harw-context-ledger`, ring A, no deps beyond harw-context/types): per turn an entry `{session, turn, offers: [{source, label, trust, tokens, fact_id?}], omitted: [{label, reason}], used: [label]}`. Writer: the existing assembly in harw-core/harw-runtime (one call site); storage: bounded JSONL under the profile, retention class `context_ledger` (ephemeral, 14 d default).
**C2 Single budget owner**: collapse the two assembly paths in harw-core (legacy byte-budget and `Assembly<Gathered..Rendered>`) onto the typed path; `ContextBudgetSpec::tighten` stays min-only; ledger records omission reasons.
**C3 Providers declare themselves** via one macro (`context_sources!`): id, trust cap, budget share, redaction rule, tenant scope. Today's MemoryFactsContextProvider, KnowledgeContextProvider, file index become table rows.
**C4 Use detection**: a cheap deterministic "was it used" signal per offered fragment (referenced in the answer/tool args, or a tool call followed within N steps); no model call. Feeds L1.
**C5 Tuning, not rewriting**: ledger-derived `ContextProposal` (must-include / exclude / budget shift) written as pending proposals (existing `/learn` path); applied only by the operator or by a per-project auto rule for budget shifts within the ceiling.

## 4. Learning loop

**L1 Feedback signal**: per fact, `delivered`, `used`, `corrected` (user correction detected via `score_correction`), `outcome` (OutcomeTracker verdict). Stored next to `usage.json` (additive fields). Decay uses `used`, not `delivered` (fixes: delivered-but-useless facts never decay).
**L2 Extraction job** (`learning_extract`, job kind like `memory_maintenance`): reads recent *ephemeral* inputs (turn summaries from the ledger, tool outcomes) and, in a second step, optionally runs the LLM extractor (`extraction.rs`, today pure) on a *redacted* transcript window. Output only to `_incoming` with provenance `{session, turn, tool, trust}`.
**L3 Gate**: candidates pass `redact`, injection screen (strip instruction-like text, trust=Data), contradiction check (`contradiction_index`), and a size/novelty check; low-risk project-local facts auto-consolidate, anything global/skill/profile stays a pending proposal (existing review).
**L4 Evaluation**: lightweight A/B-less check: facts with `used` and non-negative outcomes gain confidence, contradicted or corrected facts lose it and are queued for review; `harw doctor` shows precision (used/delivered) per class. No separate eval harness in v1; ledger replay later.
**L5 Forgetting**: decay (time) + outcome/contradiction-driven demotion + hard caps from `[memory]`; demoted facts are archived, not deleted (deletion only via retention opt-in).
**L6 Scheduling**: one scheduler = the job system: session-end enqueue (non-blocking), idle/cooldown like dream, cron optional. Retention sweep runs after the learning job for the same classes (order guaranteed by job dependency or time window).
**L7 DoD/Sentinel**: derived counters only (finding kind, count, trust, no payload) exposed as a context source `security_signals` (Evidence, never Instruction); opt-in per config.

## 5. Safety and isolation

- Tenant/project scope is part of every ledger entry and fact id; global promotion keeps the `promoted_from` + path/secret check (G4).
- Transcript-derived text is redacted before any model sees it for learning; the dream read path gets the same redaction (gap today).
- Prompt-injection: learned text only enters prompts fenced as untrusted data (exists); extraction never writes Instruction-class items.
- All learning jobs are deadline-bounded and abort without partial state; they never block a turn.

## 6. Work items (each ≤5 files; macros first, one cargo at a time)

| # | Item | Depends on |
|---|------|-----------|
| X1 | `context_sources!` macro + table (harw-macros, harw-context) | – |
| X2 | `harw-context-ledger` crate + retention class + single writer in assembly | X1 |
| X3 | Collapse the two assembly paths, omission reasons into ledger (harw-core) | X2 |
| X4 | Use-detection + L1 feedback fields in facts (additive) | X2 |
| X5 | Wire `OutcomeTracker`/`score_correction`/contradiction index into capture + consolidation | X4 |
| X6 | `learning_extract` job kind + redaction on dream/extraction read path | X4, G6 |
| X7 | Gate (L3) + proposals from ledger (C5) | X5 |
| X8 | `harw doctor` precision report + `/memory stats` | X4 |
| X9 | `security_signals` source (opt-in) | X1 |

## 7. Decisions needed

1. Auto-apply scope: which learned changes may apply without review (proposal: project-local facts and budget shifts within the ceiling only)?
2. LLM extraction in v1, or heuristic + feedback only first (proposal: heuristic + feedback first; LLM extraction behind a flag)?
3. Ledger retention default (proposal 14 days) and whether prompts/answers are ever stored (proposal: labels and counts only, never content).

## 8. Out of scope

Cross-tenant learning, training/fine-tuning, cloud sync of memory, embeddings/vector index (can plug in as a source later).
