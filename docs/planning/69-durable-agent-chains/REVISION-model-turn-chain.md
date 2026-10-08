# PL-69 REVISION — Model-Turn-Chain

> **Status:** Revision des PL-69-Plans (durable-agent-chains) nach Design der Erfinderin (2026-09-29, Chat-Design-Session).
> **Bezug:** docs/planning/69-durable-agent-chains/ (README, W00–W02, PR #67). Diese Revision korrigiert die Grundstruktur, bevor W00–W02 implementiert werden.
> **Basis:** dev@153da055 (pinned in PL-69).

## Kernkorrektur: Zwei Profile → Eine Kette

W01 (reasoning) und W02 (generation) sind **keine zwei getrennten Profile**, sondern **Vorschausschnitte EINER Kette**: der **Model-Turn-Chain**. Jeder Modell-Turn ist ein Kettenglied und trägt als First-Class-Paar:

```
Turn := { model, effort }  — atomares Bündel, checkpointed im Chain-State
```

## Revision der fünf Prüfpunkte (Befund gegen PL-69-Draft)

### 1. Model+Effort-Bündel im checkpointed Chain-State
PL-69 hat `ReasoningStateV1.working_model` und „last successful model/provider route" — **der Effort-Anteil fehlt**. Revision: `ReasoningStateV1` um `working_effort: ReasoningEffort` erweitert; Checkpoint-Vertrag um `(model, effort)` als atomares Paar ergänzt. Persistenz-Schicht existiert bereits: `/models set <rolle> <modell> [effort]` (PR #70, `persist_role_reasoning_effort` → `reasoning.<feld>`).

### 2. Effort entkoppelt von Modellgröße
Effort ist ein First-Class-Config-Feld, **nicht** abgeleitet aus der Modellgröße. Konsequenz: **ein kleines Modell kann reasoning führen** — kleines Modell + hoher Effort in einer organisch geführten Kette ersetzt teilweise ein großes Modell (Kosten-/Latenzvorteil). Das ist ein Designziel, kein Zufallseffekt.

### 3. Die Kette ist aufschneidbar (cuttable)
PL-69 kennt nur global/provider-owned Pacing (W00 C8 — bleibt unverändert). Revision ergänzt: Die Kette lässt sich in **Segmente** teilen, die über **chain-interne Semaphoren** parallel *oder* sequenziell laufen. Jedes Segment trägt sein eigenes `(model, effort)`-Bündel und checkpointed typisiert. Chain-interne Semaphoren koordinieren nur die Segmente untereinander — Provider-Pacing/-Limits bleiben global und werden nie umgangen.

### 4. fan-out → fan-in Synthesis → Decision Point (Terminal)
Neue Kettensemantik: Ein Rundenmuster ist First-Class — parallel fan-out (n Segmente, gemischte Profile), konvergiert in **EINE Synthesis**, die **der letzte Schritt** der Runde ist, danach der **Decision Point**. Die Synthesis ist ein terminales FAN-IN („nach oben konsolidierend"), kein weiteres paralleles Segment und kein Stop-Condition-Fall. Der Decision Point ist das Terminal-Element der Chain-Semantik.

**Rekursivität:** Das Muster gilt **je Segment und je Kette** — ein Segment kann intern selbst fan-out/fan-in mit eigener kleiner Synthesis und Mini-Decision haben („innen drin"); die Kette selbst endet in einer Synthesis, die alles konsolidiert. W00 braucht dafür: Synthesis als Terminal-Typ mit Semaphore-Join-Semantik.

### 5. Wiederholungen + verschachtelte Ketten (ersetzt Non-Goal)
PL-69 schreibt als Non-Goal: „recursive self-spawning chain trees". **Revision hebt das auf** in abgegrenzter Form:
- **Wiederholungen:** dasselbe Segment iteriert mit gewanderten Fragen (Key-Drift-Iteration — bewährtes Muster aus der Analyse-Praxis: Fragen wandern mit den wandelnden Key Drivern).
- **Verschachtelung:** ein Segment kann selbst wieder eine Model-Turn-Chain sein (eigenes Bündel, eigene Semaphoren, eigener fan-out → Synthesis → Decision).
- **Hard Bound:** Rekursions-Tiefe als Chain-Level-Bound (analog `max_candidates` in W02), Wert festzulegen bei Implementierung.
- **Checkpoint-Merge:** Semantik, wie der Decision Point einer inneren Kette in den State der äußeren zurückfused — fehlt in W00/W01 vollständig, ist Pflichtelement dieser Revision.

## Interne Modellstellen als Chain-Segmente

Durch das Bündel-Design werden DREAM, COMPACTION, JOURNAL/DIARY-LEDGER und CONSOLIDATE selbst zu Kettengliedern: Als **Bundle an Bundle an Bundle kombiniert** innerhalb der organischen Kette — statt isolierter Nebenläufe. Die heutigen `/model internal`-Stellen (`dream_reflection`, `compaction_summary`, `memory_consolidation`) werden Chain-Segmente mit eigenem `(model, effort)`-Bündel. Compaction wird dadurch kein Kontext-Verlust-Ereignis, sondern ein Turn mit Bündel: kleines Modell + hoher Effort konsolidiert, checkpointed typisiert, next-turn-Config wandert mit.

## Konkretes Recipe-Beispiel: Pattern Analysis

```
Runde r (Scope wandert je Runde mit dem Erkenntnisstand):
  fan-out (parallel, semaphore-gated):
    ├─ Data Collection
    ├─ Link Analysis
    ├─ Pattern Analysis
    │    ├─ Trend Analysis    ┐ BUNDLE
    │    └─ Tendency Analysis ┘
    └─ Steps X, Y …
  fan-in: Aggregation — Collection(r-1) + Collection(r) konsolidiert,
          EMERGENZ: neue Infos entstehen, definieren Scope(r+1)
  → Synthesis (letzter Schritt, konsolidierend)
  → Decision Point → Runde r+1 (neuer Scope)
```

## Auswirkung auf Implementierungsreihenfolge (PL-69 §Scope)

Unverändert in der Reihenfolge, aber geändert im Inhalt:
1. Shared chain contracts/runtime — **jetzt inkl. Bündel-Typ, Segment- und Synthesis-/Decision-Terminal-Typen, Rekursions-Bound, Checkpoint-Merge**
2. Job adapter + checkpoint/lease fencing — unverändert
3. „Reasoning profile" → **Model-Turn-Chain-Preview (W01-Schnitte)**
4. „Generation profile" → **Model-Turn-Chain-Preview (W02-Schnitte)**
5. Model-aware continuation — jetzt direkt mit Effort-Entkopplung
6. Adaptive compositions — jetzt als Recipes (Pattern Analysis als erstes)

## Verwandte PRs

- **PR #70** (`feat/models-reasoning-effort`): Persistenz-Schicht für `(model, effort)` pro Rolle — Grundlage dieses Designs.
- **PL-71** (PR #68, semantic-activity-patterns): PatternInstances als strukturierte Beobachtungen für Chains — nachgelagert, nach dieser Revision gegenzulesen (Integrationsformulierung 06, diff 937–946).


---

## Addendum (2026-10-08): Cycle-composed agents and delegation-first organization

> **Status: TARGET / design extension; NOT an implemented chain runtime.**
> Verified reference: `dev@197a92e92d438bf6bf93b129bb964f4852686149`.
> Evidence motivating this addendum: a 2026-10-08 local SGH-Flow session export
> (not committed; it contains private environment configuration and must remain local).

### A. Agent as composed executable behavior — not a seventh authority role

For small open-weights models, including approximately 30B-parameter models without provider-native extended reasoning, the useful logical agent can be assembled from:

```text
ComposedAgent = AgentIdentity + AuthorityCeiling + ModelRoute
              + ChainRecipe + Tools/Skills + ContextProgram
              + ReturnContract + Budgets
```

`(model, effort)` remains the atomic model-turn route when effort is supported.
For a model without native effort, the chain supplies *explicit structured reasoning
operations*: orient, identify evidence gaps, retrieve targeted facts, compare
hypotheses, verify, synthesize, decide and repeat. Chain iterations are not
provider-private chain-of-thought and must not request hidden reasoning traces.

A `cycle-as-agent` is the **composed agent behavior** exposed as a named, versioned,
callable specialist. A cycle **does not gain a new `AgentRoleId`**: its owner retains
one of the six existing sealed organizational roles. A cycle is a *workflow primitive*;
a composed agent is the executable unit binding that primitive to a model, rights,
tools and a return contract. It is legitimate to call the resulting unit an agent.

### B. Three independent depth and budget dimensions

```text
authority/spawn depth  : can this caller admit another agent?
chain nesting depth   : can this recipe execute a bounded nested chain?
reasoning-round budget: how many structured model turns may occur?
```

One does not widen another. `max_depth=0` for a worker does not prohibit
*internal sequential chain turns*, but prohibits creating durable child agents.
Nested chains use the parent job/claim, authority ceiling, cancellation and
provider pacing. A subchain's state can be checkpointed without minting a
new privileged identity or role.

### C. Proposed built-in composed specialists

The following are **design targets**, not claims that each name is already a
registered built-in role:

| Target composition | Orchestrator scope | Bounded worker / chain recipes |
|---|---|---|
| Explore | repository inventory and scope assignment | tree/index scan, file-path discovery, graph edges, targeted source reading, evidence citations |
| Directory/File Research | exact file ownership and code evidence | grep/search -> narrow read -> cross-reference -> evidence review |
| Web Research | primary docs and changelogs, no workspace leakage | search -> fetch -> cross-source corroboration -> source freshness -> synthesis |
| Dependencies | package manifest / lock resolution and provenance | ecosystem metadata, installed source and advisory lookup |
| Evidence/Method Review | independent falsification | critic + method auditor, disputed claims returned to the owner |
| Scenario Analysis | hypotheses, key drivers, competing futures | scenario-player + systems-modeller + pattern-analyst |
| Matrix Games | game/adjudication workflow | matrix runner's existing controlled seat agents; preserve engine-owned dice and approvals |
| Implementation | scoped edit ownership | explorer -> plan -> authorized code writer -> central verification -> critic |

Present foundations include `research-orchestrator`, `coding-orchestrator`,
`analysis-orchestrator`, `matrix-game-master` and project/profile agent
packs such as `intel-analysis-orchestrator`,
`wargaming-orchestrator`, `dependency-research-orchestrator` and
`evidence-review-orchestrator`. They do **not** all have Root permission
to spawn as children today. The built-in Root's exact allowlist currently
contains only coding/research/analysis orchestrators. The Matrix Game Master
is a separate root in today's default design and must not be relabeled as
a child without a scoped capability and game-engine review.

### D. Default delegation-first planner

For a multi-domain request Root should produce a typed task graph, with each node:

```text
TaskNode {
  id, objective, dependency_ids[], scope, capability_requirements,
  expected_return_schema, evidence_requirements, max_budget,
  idempotency_key, owner_role_candidate, completion_predicate
}
```

Do not expose a full toolbox to every model turn. Build a small discoverable
capability index and materialize **only the authorized relevant** tool family
per node (registry and admission stay authoritative). An explicit capability
preflight should validate role visibility, child-orchestrator grants, model
availability, network host allowlist, sandbox dimensions, output schema and
remaining token/time/provider budget *before* enqueueing a wave.

```text
task -> classify -> break into independently verifiable nodes
     -> capability/model preflight -> bounded parallel wave
     -> evidence validation -> missing-fact directed next wave
     -> one synthesis -> decision -> repeat/complete/blocked
```

The orchestrator should read only a bounded workspace overview.
Full source inspection is performed by leaf workers with narrow scopes.
A useful default is at most four independent fan-out targets at once, subject
to the existing `MAX_WAVE_TARGETS=16`, provider pacing and admission limits;
this is not an unconditional concurrency guarantee.

### E. Error-directed chain behavior

A failed child call is not a reason to replay the same expensive task.
Classify failures deterministically:

- `provider_unauthorized` / HTTP 403 / model absent: reject that route;
  a retry must select an authorized known model or return `blocked`.
- `egress_denied`: do not retry with another tool to circumvent policy;
  request authorized scope or return `blocked`.
- `delegation_denied`: refresh *visible* targets only; do not invent
  permissions or child-orchestrator names.
- `budget_exhausted`: compact factual evidence and use
  `continue_from` where permitted; do not duplicate full transcripts.
- `no_progress`: stop, refocus the question, or return an evidence gap.
- `output_truncated`: break into small artifact units, persist versions,
  and resume from checkpoints; no oversized single-call rewrite.

A side-effecting segment requires idempotent admission and explicit rollback
or compensation; validation and model critique are not authorization.

### F. Durable contracts required before implementation

1. `AgentCompositionRef` binding a versioned `AgentIr` snapshot to a
   versioned `ChainRecipeRef`, model-route policy and return contract.
2. Strict `ChainRecipe` schema with typed segment IDs, dependencies,
   subchain bounds, per-segment model/effort, tool requests and outputs;
   model-generated configs remain data until validated.
3. Checkpoint: job/lease epoch, chain/segment/round IDs, per-segment durable
   semantic state, artifact digest/provenance, usage, and terminal decision.
4. Reducer for fan-in synthesis: join must retain missing/failed children
   as explicit gaps, not silently convert partial results to success.
5. Preflight and provider-compatible capability snapshots on composition,
   including fail-closed model credential and egress checks.
6. Unit and integration tests for auth/egress 403 loops, missing delegation,
   no-progress exhaustion, checkpoint/restart/lease fencing, duplicate work,
   truncated artifacts, privilege non-escalation and small-model performance.

### G. Incremental implementation waves

- **C0 (docs / landed separately):** refresh root/sub orchestration rulebooks,
  add this PL-69 addendum, avoid secret-bearing session exports in Git.
- **C1 (contract):** typed TaskNode, AgentCompositionRef, ChainRecipe schema,
  tests that authority does not change with chain nesting.
- **C2 (preflight):** role/model/network/tool-capability resolution and
  understandable rejection reasons, without widening sandbox grants.
- **C3 (adaptive discovery):** small authorized tool index,
  discovery → activation, typed DAG → `delegate_wave` for existing roles.
- **C4 (durable runner):** PL-69 W00 with atomic checkpoint, lease fencing,
  provider pacing, bounded segment/chain nesting.
- **C5 (small-model recipes):** File/Directory Explore and Web Research
  cycles, evaluated against single-turn baselines with reproducible tasks.
- **C6 (specialized orchestrators):** promote verified packs for Explore,
  Scenario, Matrix and Evidence Review through explicit Root allowlists;
  extend organizations/families/cells, but preserve matrix game authority.
- **C7 (measurement):** compare grounded evidence coverage, success rate,
  tokens per accepted finding, tool/permission failures, latency, wasted
  calls, and recovery after restart. Do not claim a small model is better
  until measurements demonstrate it.

The source-of-truth remains current Rust source, tests and runtime rules.
These additions are design targets rather than functionality provided by
a TOML file or this documentation update.
