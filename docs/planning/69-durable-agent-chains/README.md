# PL-69 — Durable Agent Chains

> **Status:** DRAFT  
> **Pinned baseline:** `mm9942/Harwness` `dev@153da05518cfd2cceb2cda738f9417cdf1653035`  
> **Scope:** durable, job-backed iterative agent chains  
> **First built-in chains:** `reasoning`, `generation`

## 1. Goal

Harw needs a reusable execution primitive for long-running model workflows that are neither a single turn nor a goal-specific WorkDriver run.

The primitive is a **durable chain**:

```text
job claim
   ↓
chain state
   ↓
step → observe → checkpoint → decide
   ↑                         ↓
   └──────── continue ───────┘
```

The chain may run for many model calls, survive worker/process replacement, checkpoint compact state, and resume without rebuilding the full reasoning/generation history from scratch.

This is intentionally **not** a search-specific subsystem and **not** a compaction subsystem.

Search, research, verification, critique, synthesis and other adaptive workflows are future compositions over the same chain substrate.

The first two concrete chain profiles are:

```text
reasoning
generation
```

## 2. CURRENT

At the pinned baseline Harw already has most of the mechanisms a durable chain should reuse, but they are owned by separate higher-level features.

### 2.1 Durable jobs exist

`harw-job-core::Job` provides:

- stable `WorkId`
- `JobKind`
- budget and usage
- lease-bound execution
- retry state
- terminal lifecycle states

`JobKind::Custom(String)` already allows composition-owned job types without adding domain variants to the generic job core.

### 2.2 WorkDriver already proves a durable supervisor loop

`work_driver` is a custom job kind. One claim drives multiple rounds, keeps durable sidecar state, respects provider pacing/concurrency and can continue or respawn workers.

Its decision algorithm is goal/criteria/verification specific, so it must **not** become the generic chain abstraction.

It is evidence that the job substrate can host a durable multi-round controller.

### 2.3 Child continuation already exists

Managed child runs can return a continuation token and later resume via `continue_from`.

This provides a provider-neutral continuation seam for chain steps that use child agents.

### 2.4 Cells already provide staged/fan-out execution

`harw-plan-bridge::cells` can schedule ordered stages, barriers and fan-out joins. Cells are useful *inside* a chain step, but they are not durable chain state and do not own checkpoint/resume policy.

### 2.5 Compaction/handoff exists but is not the chain

Session compaction and child handoff can reduce context before continuation.

They remain support mechanisms. A chain checkpoint must have its own typed state and must not be defined as "whatever the conversation summary says".

### 2.6 Missing today

There is no generic durable chain contract with:

- a stable chain kind
- typed, versioned chain input
- typed, versioned checkpoint state
- step ordinal / epoch
- explicit continuation decision
- chain-level stop conditions
- checkpoint cadence
- state migration/versioning
- job-worker dispatch shared by multiple chain profiles
- generic status/stop surface

## 3. TARGET

### 3.1 One substrate, multiple chain profiles

```text
                 ┌────────────────────────────┐
                 │ Durable Chain Runtime      │
                 │                            │
JobStore ───────▶│ claim / lease / budget     │
                 │ checkpoint / resume        │
                 │ pacing / cancel / timeout  │
                 │ observation journal        │
                 └────────────┬───────────────┘
                              │
                 ┌────────────┴────────────┐
                 ▼                         ▼
        ReasoningChain             GenerationChain
        profile                    profile
```

Future profiles reuse the same runtime:

```text
AdaptiveSearchChain
ResearchChain
CritiqueChain
SynthesisChain
VerificationChain
```

They are not separate schedulers.

### 3.2 Chain state is explicit

A chain checkpoint is a durable state transition, not raw model history.

Conceptual contract:

```rust
pub struct ChainCheckpoint {
    pub schema_version: u32,
    pub chain_kind: ChainKindName,
    pub epoch: u64,
    pub step: u64,
    pub state: serde_json::Value,
    pub continuation: Option<String>,
    pub usage: ChainUsage,
    pub updated_at: Timestamp,
}

pub enum ChainDecision {
    Continue,
    Wait,
    Complete,
    Blocked,
    Failed,
}
```

The initial implementation may keep the payload JSON at the composition boundary, while each built-in profile decodes it into a strict `deny_unknown_fields` type.

### 3.3 One claim can drive many steps

A normal chain run should not create a fresh durable job for each model call.

```text
READY
  ↓ claim
RUNNING
  ↓
step 0
  ↓ checkpoint
step 1
  ↓ checkpoint
step 2
  ↓
COMPLETE / BLOCK / CANCEL
```

The lease fences the chain runner. Checkpoints make restart/resume safe.

### 3.4 Chain state and model session lifetime are separate

```text
Job lifetime        hours / days
Chain checkpoint    durable
Model session       reusable when healthy
Model request       one step
Container/cell      potentially longer-lived than any one step
```

A chain may keep a warm session for cache locality but must be able to reconstruct its next request from the checkpoint plus bounded recent context.

### 3.5 No raw hidden chain-of-thought persistence requirement

"Reasoning chain" means a chain of **reasoning operations and explicit working state**, not a requirement to extract or persist private model chain-of-thought.

Persisted state may contain:

- hypotheses
- assumptions
- evidence references
- decisions
- uncertainties
- rejected alternatives with short reasons
- open questions
- next-step intent
- structured summaries supplied for continuation

Provider-native opaque reasoning blocks may remain session-local where required by a provider contract; they are not the durable chain API.

## 4. Shared chain contract

### 4.1 Job kinds

Initial composition-owned job kinds:

```text
chain.reasoning
chain.generation
```

Do not add variants to `harw-job-core::JobKind`; use `JobKind::Custom`.

A later generic discriminator may use:

```text
chain.<profile>
```

with strict profile lookup.

### 4.2 Versioned input

Every chain job input carries:

```text
schema_version
profile
requested_by
tenant/workspace binding
task
limits
profile-specific config
initial state (optional)
```

Input may request less authority/resources than the runner owns but never expands them.

### 4.3 Versioned checkpoint

Checkpoint must include:

```text
schema_version
work_id
profile
epoch
step
status
profile state
continuation handle if any
usage snapshot
last successful model/provider route
updated_at
```

Atomic write semantics should match the WorkDriver sidecar pattern initially, then converge on a generic job-state sidecar abstraction.

### 4.4 Epoch fencing

Every claimed run gets a chain epoch derived from the durable lease/revision.

A stale worker must not overwrite a newer checkpoint.

Conceptually:

```text
checkpoint.work_id == claim.work_id
checkpoint.epoch   == live lease epoch
checkpoint.step    > stored step
```

otherwise reject.

### 4.5 Step loop

```text
load checkpoint
      ↓
validate/migrate
      ↓
derive bounded step request
      ↓
provider pacing / budget check
      ↓
run model/agent step
      ↓
validate structured step result
      ↓
merge profile state
      ↓
atomic checkpoint
      ↓
profile decides continue | wait | complete | blocked | failed
```

No chain step may be considered committed until its checkpoint is durable.

### 4.6 Cancellation and restart

Cancellation uses the existing trusted job execution control.

On process crash:

- the current uncommitted step may be repeated;
- the last durable checkpoint is authoritative;
- idempotency keys should be derived from `work_id + epoch + step` for external side effects;
- a resumed runner never guesses whether a missing checkpoint "probably succeeded".

### 4.7 Budgets

Chain limits are independent dimensions:

```text
max_steps
max_model_calls
max_tokens
max_wall
max_tool_calls
max_no_progress_steps
checkpoint_every_steps
```

Provider concurrency and pacing remain global/provider-owned constraints and are never bypassed by a chain.

### 4.8 Checkpoint vs compaction

Checkpoint and compaction are related but not identical:

```text
compaction
  shrinks model-facing history

checkpoint
  records durable chain-facing state
```

A checkpoint can trigger compaction. A chain can also checkpoint even when no compaction is needed.

This distinction is required for stable long-running jobs.

## 5. Built-in chain 1 — Reasoning

### 5.1 Purpose

The Reasoning Chain repeatedly develops and tests an explicit working model of a problem.

It is useful for:

- architecture analysis
- intelligence analysis
- diagnosis
- hypothesis-driven debugging
- decision decomposition
- adaptive investigation
- planning before execution

### 5.2 Durable state

Initial typed state:

```text
objective
working_model
hypotheses[]
assumptions[]
evidence[]
contradictions[]
open_questions[]
decisions[]
next_actions[]
confidence
step
```

Items should carry stable IDs so later steps can update/refute them instead of reproducing the entire state as prose.

### 5.3 Step shape

```text
ORIENT
  read checkpoint + new observations

REASON
  refine hypotheses / causal model / alternatives

TEST
  identify discriminating observation or tool action

UPDATE
  merge result into durable state

DECIDE
  continue / conclude / block
```

The profile must be able to run without tools as a pure reasoning chain and with tools when its admitted runtime permits them.

### 5.4 Stop conditions

Complete when one of these holds:

- objective-specific completion predicate is satisfied;
- no material open question remains;
- requested confidence/evidence threshold is met;
- a terminal structured result is produced.

Block when external information/approval is required.

Fail on invalid state transition, exhausted hard budget, or unrecoverable provider/runtime error.

### 5.5 Adaptive search as a composition

Adaptive search is **not** its own base runtime.

A search-capable Reasoning Chain can represent:

```text
hypothesis
  ↓
information gap
  ↓
query/action choice
  ↓
observation
  ↓
update hypothesis
  ↓
choose next query
```

The next query is therefore selected from the current durable reasoning state, not from a fixed query list.

## 6. Built-in chain 2 — Generation

### 6.1 Purpose

The Generation Chain iteratively produces and improves an artifact.

It is useful for:

- code generation
- document generation
- design generation
- transformation pipelines
- structured synthesis
- iterative refactoring
- candidate generation + critique + revision

### 6.2 Durable state

Initial typed state:

```text
objective
constraints[]
artifact_kind
current_artifact_ref
versions[]
accepted_decisions[]
critique[]
failed_constraints[]
next_transform
quality_checks[]
step
```

Large artifacts should be stored by reference/digest where possible rather than copied into every checkpoint.

### 6.3 Step shape

```text
READ STATE
   ↓
GENERATE / TRANSFORM
   ↓
VALIDATE
   ↓
CRITIQUE
   ↓
ACCEPT | REVISE | BRANCH
   ↓
CHECKPOINT
```

A single generation step may internally use a Cell for parallel candidates or independent critics.

### 6.4 Branching

Generation may create bounded candidate branches:

```text
artifact v4
  ├─ candidate A
  ├─ candidate B
  └─ candidate C
          ↓
      selection
          ↓
       artifact v5
```

Candidate fan-out is ephemeral within a chain step unless explicitly checkpointed. It must not create unbounded recursive chains.

### 6.5 Stop conditions

Complete when:

- all required constraints pass;
- validator/acceptance contract succeeds;
- explicit iteration target is reached and a final artifact is selected.

Block on missing operator decision/approval.

Fail on invalid artifact state, exhausted hard budget, or unrecoverable execution error.

## 7. TOML / declarative configuration

The first implementation should use configuration to choose **policy and limits**, not to define arbitrary executable code.

Conceptual shape:

```toml
[chains]
enabled = true
max_running = 4

[chains.reasoning]
enabled = true
max_steps = 64
checkpoint_every_steps = 1
max_no_progress_steps = 6

[chains.generation]
enabled = true
max_steps = 48
checkpoint_every_steps = 1
max_candidates = 4
```

Profile/model routing should reuse `internal_models` rather than invent a second provider registry.

Project-local configuration may narrow limits. It must not increase host/global ceilings.

## 8. Job worker integration

Do not build a second daemon.

Extend the existing durable job worker dispatch:

```text
JobKind::Custom("chain.reasoning")
         │
         └── chain runner
               └── Reasoning profile

JobKind::Custom("chain.generation")
         │
         └── same chain runner
               └── Generation profile
```

The chain runner should get its own bounded lane, analogous to the WorkDriver lane, or share a future generic long-running-controller lane.

A chain must not monopolize the polling loop.

## 9. Persistent cells / containers

Durable chains are a natural workload for persistent execution cells.

The chain job remains durable outside the cell. A cell may host many sequential chain steps.

```text
durable Job + checkpoint
        │
        ▼
persistent cell
        │
        ├─ step
        ├─ step
        ├─ step
        └─ scrub/reuse
```

Cell loss does not lose chain state.

Chain state must not depend on container identity.

## 10. Security and authority

A chain never gains authority merely because it continues.

```text
authority(step N+1) <= authority(step N) <= job/runtime ceiling
```

Checkpoints are data, not authority.

A checkpoint must never be able to request:

- wider filesystem access
- wider network scope
- host persistence
- node enrollment
- container-engine access
- more powerful child roles

Those remain runtime/admission decisions.

## 11. Observability

Minimum events:

```text
chain.started
chain.step.started
chain.step.completed
chain.checkpoint.written
chain.waiting
chain.blocked
chain.completed
chain.failed
chain.cancelled
chain.resumed
```

Each should carry:

```text
work_id
profile
epoch
step
trace_id/span where available
usage delta
duration
```

Never log raw secrets or provider-private reasoning payloads.

## 12. DELTA / implementation waves

### W0 — contract

- add planning/contracts
- freeze names `chain.reasoning`, `chain.generation`
- define state/version/fencing rules
- no runtime behavior change

### W1 — generic chain core

Preferred ownership should be verified before coding. Candidates:

- a new generic `harw-chain` shared-infrastructure crate, or
- a small generic module in job/runtime infrastructure

Do **not** put the reusable chain state machine permanently into `harw-plan-bridge`; that crate is plan-domain integration.

Implement:

- `ChainProfile` trait or equivalent closed registry contract
- `ChainCheckpoint`
- `ChainDecision`
- budget/progress accounting
- deterministic transition tests

### W2 — job adapter

- custom job kinds
- typed inputs
- atomic sidecar
- lease-epoch fencing
- chain lane / cancellation
- status + stop operations

### W3 — Reasoning profile

- typed state
- strict structured step result
- merge/refute/update semantics
- no-progress detection
- checkpoint/resume tests

### W4 — Generation profile

- artifact reference/version state
- candidate/critique/selection step
- bounded branching
- checkpoint/resume tests

### W5 — context efficiency

- model-aware compaction budget
- chain checkpoint → bounded prompt projection
- larger continuation summaries where necessary
- keep checkpoint authoritative even if compaction fails

### W6 — compositions

First candidates:

- adaptive search using Reasoning
- generate → critique → revise using Generation
- reasoning → generation handoff
- generation → reasoning verification loop

## 13. Acceptance criteria

The first implementation is not complete until:

1. A `chain.reasoning` job survives worker restart and resumes from the last checkpoint.
2. A `chain.generation` job survives worker restart and preserves its selected artifact version.
3. Re-running an already committed step cannot overwrite a newer epoch/step.
4. A chain cannot widen the job/session authority through checkpoint data.
5. Provider pacing/concurrency remains shared with the existing runtime.
6. Cancellation reaches an in-flight model/child step and leaves the last committed checkpoint valid.
7. A chain can run many steps without spawning a new durable job per step.
8. Compaction failure does not destroy chain state.
9. Unknown checkpoint/input fields fail closed for the built-in profiles.
10. Reasoning and Generation share the same runner; no duplicate scheduler is introduced.

## 14. Non-goals of this draft

Not in the first two chain profiles:

- a dedicated SearchChain base type
- arbitrary user-supplied Rust/script steps
- a generic visual workflow language
- recursive self-spawning chain trees
- automatic authority escalation
- raw chain-of-thought persistence/export
- replacing WorkDriver
- replacing Cells
- replacing session compaction

## 15. References

CURRENT code/design inspected at the pinned baseline:

- `harw-job-core/src/job.rs`
- `harw-cli/src/job_worker.rs`
- `harw-cli/src/job_worker_work_driver.rs`
- `harw-plan-bridge/src/work_driver.rs`
- `harw-plan-bridge/src/cells.rs`
- `harw-core/src/child_handoff.rs`
- `harw-core/src/compaction.rs`
- `docs/guides/work-driver.md`
- `docs/planning/67-containers/cell.md`

## 16. IMPLEMENTATION STATUS

```text
CURRENT:
    durable jobs, WorkDriver loops, child continuation, cells, compaction exist

PLANNED:
    generic durable chain runtime
    reasoning profile
    generation profile

NOT YET LANDED:
    chain job kinds
    chain checkpoint schema
    chain runner
    reasoning/generation runtime profiles
    chain TOML
```
