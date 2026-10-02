# W02 — Generation Chain

> **Status:** DRAFT  
> **Job kind:** `chain.generation`  
> **Runtime contract:** W00-chain-runtime.md

## Purpose

A Generation Chain iteratively creates, validates, critiques and revises an artifact while keeping durable version/constraint state.

The artifact may be code, text, configuration, a design, structured data or another generated object.

## State v1

Conceptual strict payload:

```rust
struct GenerationStateV1 {
    objective: String,
    artifact_kind: String,
    constraints: Vec<Constraint>,
    current: Option<ArtifactRef>,
    versions: Vec<ArtifactVersion>,
    accepted_decisions: Vec<GenerationDecision>,
    critiques: Vec<Critique>,
    failed_constraints: Vec<ConstraintFailure>,
    quality_checks: Vec<QualityCheck>,
    next_transform: Option<Transform>,
    progress_revision: u64,
}
```

Large artifacts are referenced by durable locator + digest where possible; the checkpoint should not duplicate megabytes of generated content.

## Default cycle

```text
READ CHECKPOINT
      ↓
GENERATE / TRANSFORM
      ↓
STORE CANDIDATE
      ↓
VALIDATE CONSTRAINTS
      ↓
CRITIQUE
      ↓
ACCEPT | REVISE | BRANCH
      ↓
CHECKPOINT
      ↓
continue | complete | block
```

## Candidate branching

One step may produce bounded alternatives:

```text
v7
 ├─ candidate A
 ├─ candidate B
 └─ candidate C
        ↓
    evaluate
        ↓
      v8
```

Rules:

- `max_candidates` is a hard bound;
- candidates are not new durable chain jobs by default;
- a Cell may execute candidate generation/critique in parallel;
- only explicitly checkpointed candidate refs survive restart;
- no recursive chain spawning merely because a candidate exists.

## Constraints

Constraints are first-class state, not only prompt text.

Examples:

- schema/format,
- compile/test checks,
- style rules,
- required/forbidden content,
- file/path limits,
- user acceptance criteria.

A revision step states which failures it is trying to repair.

## Progress fingerprint

Suggested v1 inputs:

- current artifact digest,
- set/status of constraint failures,
- accepted decision IDs,
- quality-check results,
- selected candidate ID.

Generating another artifact with the same digest and same failures is no progress.

## Completion

Complete only when the configured completion contract is satisfied.

Examples:

```text
all hard constraints pass
AND selected artifact exists
AND required validators pass
```

A model saying "looks good" is not sufficient when validators exist.

## Artifact ownership

The chain state stores references. Actual artifact writes remain governed by the existing filesystem/job authority.

Checkpoint data cannot create a writable path permission.

## Reasoning ↔ Generation composition

The two first profiles are intentionally composable:

```text
Reasoning Chain
  determines plan/constraints
        ↓
Generation Chain
  produces candidate
        ↓
Reasoning Chain
  diagnoses failures / revises strategy
        ↓
Generation Chain
  transforms next version
```

This composition must reuse the same job/provider/continuation substrate rather than nesting unbounded controllers.

## First implementation tests

1. Generated artifact v1 checkpoint resumes after worker restart.
2. Artifact digest prevents accidental duplicate version creation.
3. Failing hard constraint prevents completion.
4. Revision records exactly which constraint failures it addresses.
5. Bounded candidate branch selects one result and discards/archives others.
6. Cell failure during candidate generation leaves prior checkpoint valid.
7. Cancellation during generation cannot commit a partially validated version.
8. A checkpoint cannot widen write/network authority.
