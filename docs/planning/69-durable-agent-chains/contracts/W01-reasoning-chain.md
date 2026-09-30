# W01 — Reasoning Chain

> **Status:** DRAFT  
> **Job kind:** `chain.reasoning`  
> **Runtime contract:** W00-chain-runtime.md

## Purpose

A Reasoning Chain is a durable sequence of explicit analytical state transitions.

"Reasoning" here means the workflow and its inspectable working state — hypotheses, evidence, contradictions, decisions and next tests — not raw hidden provider chain-of-thought.

## State v1

Conceptual strict payload:

```rust
struct ReasoningStateV1 {
    objective: String,
    working_model: String,
    hypotheses: Vec<Hypothesis>,
    assumptions: Vec<Assumption>,
    evidence: Vec<EvidenceRef>,
    contradictions: Vec<Contradiction>,
    open_questions: Vec<Question>,
    decisions: Vec<Decision>,
    next_actions: Vec<Action>,
    confidence: Confidence,
    progress_revision: u64,
}
```

Every mutable item gets a stable ID.

A later step updates/refutes an existing item instead of duplicating the whole state as prose.

## Step contract

Each step receives a projection of the durable state and new observations.

The model/worker returns a structured delta:

```text
hypotheses_added
hypotheses_updated
hypotheses_refuted
evidence_added
contradictions_added/resolved
questions_added/resolved
decisions_added
next_actions
completion
```

The runtime validates the delta before merging it.

## Default cycle

```text
ORIENT
  current model + evidence + unresolved questions
      ↓
REASON
  competing explanations / implications
      ↓
DISCRIMINATE
  choose the observation/action with highest expected information value
      ↓
OBSERVE
  tool / child / user / external result
      ↓
UPDATE
  typed state delta
      ↓
CHECKPOINT
      ↓
continue | complete | block
```

## Adaptive search

Adaptive search is a capability of this profile, not a separate base scheduler.

When search tools are admitted:

```text
open question
   ↓
candidate queries
   ↓
select query from current evidence gaps
   ↓
search/read
   ↓
source/evidence assessment
   ↓
update hypotheses/questions
   ↓
select next query
```

This permits query strategy to change after every observation.

A fixed pre-generated list of queries is not required.

## Progress fingerprint

Suggested v1 fingerprint inputs:

- set of unresolved question IDs,
- hypothesis status/revision,
- evidence digests,
- contradiction status,
- decision IDs,
- objective completion state.

A changed paragraph with identical semantic sets does not reset the stall counter.

## Completion

A profile-specific completion predicate may require:

- no critical open questions;
- no unresolved high-severity contradictions;
- minimum evidence/support threshold;
- explicit terminal answer/synthesis.

A hard token or wall budget does not fabricate completion.

## Blocking

Return `Blocked` when the next discriminating action requires authority/input not currently available.

The checkpoint must preserve exactly what information is missing and why it matters.

## Tool use

Reasoning Chain can run:

- model-only,
- with read/search tools,
- with child analysts,
- with bounded execution tools if admitted.

The chain profile never grants those tools itself.

## First implementation tests

1. Three-step hypothesis refinement survives restart.
2. A refuted hypothesis remains refuted after resume.
3. Duplicate evidence digest is deduplicated.
4. Resolving an open question changes the progress fingerprint.
5. Rewording the working model alone does not fake progress.
6. Adaptive-search next query can change after contradictory evidence.
7. A tool/approval gap blocks with a resumable checkpoint.
8. No raw provider-private reasoning payload is required in the checkpoint.
