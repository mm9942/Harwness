# W00 — Durable Chain Runtime Contract

> **Status:** DRAFT  
> **Parent:** ../README.md  
> **Baseline:** `dev@153da05518cfd2cceb2cda738f9417cdf1653035`

## Contract

A durable chain is one governed job claim driving an ordered sequence of model/agent steps with a durable checkpoint between committed transitions.

```text
claim
  ↓
load checkpoint
  ↓
step N
  ↓
validate
  ↓
commit checkpoint N
  ↓
continue | wait | complete | block | fail
```

### C1 — Job ownership

- A chain is a `JobKind::Custom("chain.<profile>")`.
- The generic job core owns lifecycle, budget, retry and lease.
- The chain runtime owns step/checkpoint semantics.
- A profile owns only profile state and transition logic.

### C2 — Exactly one committed state

For `(work_id, epoch)`, checkpoint steps are monotonic.

```text
new.step > stored.step
new.epoch == active lease epoch
```

A stale runner cannot overwrite state from a newer claim.

### C3 — Commit boundary

A model response, child result or tool result is not durable progress until the resulting checkpoint has been atomically persisted.

After a crash the runner resumes from the last committed checkpoint and may replay the uncommitted step.

### C4 — Idempotency

External effects created by a chain step must be either:

1. naturally idempotent,
2. guarded by the existing approval/authority boundary, or
3. correlated with an idempotency key derived from `work_id + epoch + step + effect ordinal`.

The checkpoint never asserts that an effect happened merely because the model said it did.

### C5 — Authority monotonicity

Checkpoint data carries no authority.

```text
effective authority(step N+1)
    <= job/runtime authority ceiling
```

The profile cannot widen filesystem, network, process, persistence, node or child-spawn authority through its serialized state.

### C6 — Runtime reuse

The chain runtime may reuse:

- the same claimed job,
- the same model session,
- the same provider instance/rate limiter,
- the same persistent execution cell,
- child continuation handles.

It must not require a fresh durable job or fresh container for every step.

### C7 — Cancellation

Cancellation races every long-running step.

After cancellation:

- no later checkpoint may be committed;
- the previous committed checkpoint remains valid;
- child/process cancellation uses existing runtime mechanisms;
- the job reaches the governed cancellation outcome through the job store.

### C8 — Provider limits

A chain never owns a private provider-rate authority.

It uses the existing shared provider instance and respects:

- `pacing_wait()`,
- concurrency limits,
- 429 cooldown/backoff,
- model context/output limits.

### C9 — Checkpoint schema

Minimum envelope:

```rust
struct ChainCheckpointEnvelope {
    schema_version: u32,
    work_id: WorkId,
    profile: String,
    epoch: u64,
    step: u64,
    status: ChainStatus,
    state: serde_json::Value,
    continuation: Option<String>,
    usage: ChainUsage,
    updated_at: Timestamp,
}
```

Built-in profile payloads use strict typed decoding with `deny_unknown_fields`.

### C10 — Progress / anti-loop rule

Every profile defines a progress fingerprint over its typed state.

If the fingerprint does not materially change for more than `max_no_progress_steps`, the chain must either:

- choose a different transition,
- block for input,
- or fail as stalled.

Merely producing more prose is not progress.

### C11 — Prompt projection

The next model request is built from:

```text
stable profile instructions
+ task/objective
+ durable checkpoint projection
+ bounded recent observations
+ optional continuation state
```

It is not required to replay the full transcript.

Session compaction may assist this projection but is not the source of truth.

### C12 — Restart

On worker restart:

1. reclaim according to job-store lease rules;
2. load the latest valid checkpoint;
3. validate profile/schema/version;
4. reconstruct only the bounded next-step context;
5. continue at `step + 1`.

No heuristic recovery from logs or assistant prose.

## Initial runtime interface sketch

This is a design contract, not landed API:

```rust
trait ChainProfile {
    type State;
    type Observation;
    type Output;

    fn decode_state(value: serde_json::Value) -> Result<Self::State, ChainError>;
    fn project(&self, state: &Self::State) -> ChainRequest;
    fn apply(
        &self,
        state: Self::State,
        observation: Self::Observation,
    ) -> Result<ChainTransition<Self::State, Self::Output>, ChainError>;
}

enum ChainTransition<S, O> {
    Continue(S),
    Wait(S, WaitReason),
    Complete(S, O),
    Blocked(S, String),
    Failed(String),
}
```

The concrete ownership location remains open until dependency direction is checked. The reusable state machine should not be permanently owned by `harw-plan-bridge`.

## Verification

Required tests when W1/W2 land:

- stale epoch checkpoint rejected;
- same/lower step rejected;
- crash before checkpoint replays step;
- crash after checkpoint resumes next step;
- cancellation forbids post-cancel commit;
- malformed/unknown profile state rejected;
- no-progress threshold trips;
- profile state cannot alter authority;
- provider pacing is observed;
- chain runner does not block ordinary job polling.
