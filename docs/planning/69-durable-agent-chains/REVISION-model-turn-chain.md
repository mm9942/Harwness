# PL-69 REVISION — Model-Turn-Chain

> **Status:** Revision of the PL-69 plan (durable-agent-chains) following the inventor's design (2026-09-29, chat design session).
> **Reference:** docs/planning/69-durable-agent-chains/ (README, W00–W02, PR #67). This revision corrects the basic structure before W00–W02 are implemented.
> **Basis:** dev@153da055 (pinned in PL-69).

## Core correction: two profiles → one chain

W01 (reasoning) and W02 (generation) are **not two separate profiles**, but **preview cuts of ONE chain**: the **Model-Turn-Chain**. Every model turn is a link in the chain and carries, as a first-class pair:

```
Turn := { model, effort }  — atomic bundle, checkpointed in the chain state
```

## Revision of the five review points (findings against the PL-69 draft)

### 1. Model+effort bundle in the checkpointed chain state
PL-69 has `ReasoningStateV1.working_model` and "last successful model/provider route" — **the effort part is missing**. Revision: `ReasoningStateV1` is extended with `working_effort: ReasoningEffort`; the checkpoint contract is extended with `(model, effort)` as an atomic pair. The persistence layer already exists: `/models set <role> <model> [effort]` (PR #70, `persist_role_reasoning_effort` → `reasoning.<field>`).

### 2. Effort decoupled from model size
Effort is a first-class config field, **not** derived from model size. Consequence: **a small model can drive reasoning** — a small model plus high effort in an organically driven chain partly replaces a large model (cost/latency advantage). This is a design goal, not a side effect.

### 3. The chain is cuttable
PL-69 knows only global/provider-owned pacing (W00 C8 — remains unchanged). The revision adds: the chain can be divided into **segments** that run in parallel *or* sequentially via **chain-internal semaphores**. Each segment carries its own `(model, effort)` bundle and checkpoints in a typed way. Chain-internal semaphores coordinate only the segments among themselves — provider pacing/limits remain global and are never bypassed.

### 4. fan-out → fan-in synthesis → decision point (terminal)
New chain semantics: a round pattern is first-class — parallel fan-out (n segments, mixed profiles), converging in **ONE synthesis** that is **the last step** of the round, followed by the **decision point**. The synthesis is a terminal FAN-IN ("consolidating upward"), not another parallel segment and not a stop-condition case. The decision point is the terminal element of the chain semantics.

**Recursion:** the pattern applies **per segment and per chain** — a segment can itself contain an internal fan-out/fan-in with its own small synthesis and mini-decision ("inside"); the chain itself ends in a synthesis that consolidates everything. For this, W00 needs: synthesis as a terminal type with semaphore-join semantics.

### 5. Repetitions + nested chains (replaces non-goal)
PL-69 lists as a non-goal: "recursive self-spawning chain trees". **The revision lifts this** in a bounded form:
- **Repetitions:** the same segment iterates with drifting questions (key-drift iteration — a proven pattern from analysis practice: questions move along with the changing key drivers).
- **Nesting:** a segment can itself be a Model-Turn-Chain again (own bundle, own semaphores, own fan-out → synthesis → decision).
- **Hard bound:** recursion depth as a chain-level bound (analogous to `max_candidates` in W02), value to be set at implementation.
- **Checkpoint merge:** the semantics of how the decision point of an inner chain fuses back into the state of the outer one — completely missing in W00/W01, a mandatory element of this revision.

## Internal model slots as chain segments

Through the bundle design, DREAM, COMPACTION, JOURNAL/DIARY-LEDGER and CONSOLIDATE themselves become chain links: **bundle combined with bundle combined with bundle** within the organic chain — instead of isolated side runs. The current `/model internal` slots (`dream_reflection`, `compaction_summary`, `memory_consolidation`) become chain segments with their own `(model, effort)` bundle. Compaction thereby stops being a context-loss event and becomes a turn with a bundle: a small model plus high effort consolidates, checkpoints in a typed way, and the next-turn config travels along.

## Concrete recipe example: pattern analysis

```
Round r (scope shifts each round with the state of knowledge):
  fan-out (parallel, semaphore-gated):
    ├─ Data Collection
    ├─ Link Analysis
    ├─ Pattern Analysis
    │    ├─ Trend Analysis    ┐ BUNDLE
    │    └─ Tendency Analysis ┘
    └─ Steps X, Y …
  fan-in: Aggregation — Collection(r-1) + Collection(r) consolidated,
          EMERGENCE: new information arises, defines Scope(r+1)
  → Synthesis (last step, consolidating)
  → Decision Point → Round r+1 (new scope)
```

## Impact on implementation order (PL-69 §Scope)

Unchanged in order, but changed in content:
1. Shared chain contracts/runtime — **now incl. bundle type, segment and synthesis/decision terminal types, recursion bound, checkpoint merge**
2. Job adapter + checkpoint/lease fencing — unchanged
3. "Reasoning profile" → **Model-Turn-Chain preview (W01 cuts)**
4. "Generation profile" → **Model-Turn-Chain preview (W02 cuts)**
5. Model-aware continuation — now directly with effort decoupling
6. Adaptive compositions — now as recipes (pattern analysis first)

## Related PRs

- **PR #70** (`feat/models-reasoning-effort`): persistence layer for `(model, effort)` per role — foundation of this design.
- **PL-71** (PR #68, semantic-activity-patterns): PatternInstances as structured observations for chains — downstream, to be cross-read after this revision (integration wording 06, diff 937–946).
