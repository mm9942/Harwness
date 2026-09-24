# Architecture: TuiSessionController (Long-lived)

> Status: implemented · Last reviewed: 2026-09-24

Companion to `docs/architecture/operation-registry.md`.

---

## Why long-lived?

In 0.1.0 a fresh `TuiSessionController` was constructed inside each command
handler. This caused three problems:

1. **State loss** — any pending change queued by one command was discarded when
   the next command created a new controller.
2. **Turn-boundary unsafety** — applying session mutations mid-turn (while
   `run_turn_streaming` held the session) risked data races between the renderer
   thread and the turn thread.
3. **Duplicate source of truth** — each command handler held its own view of
   effort/model/provider, which could diverge from what the turn loop actually saw.

The fix: `ChatApp` owns one `Arc<TuiSessionController>`. The Arc is cloned into
`build_services()` so command handlers can queue mutations without creating a new
controller. The Arc is also cloned into the main loop, which applies pending state
at the only safe point in the turn cycle.

---

## Turn-cycle hook: `apply_pending_controller_state`

```
User input arrives
        |
        v
apply_pending_controller_state(session, controller)   <-- safe boundary
        |
        v
run_turn_streaming(session, ...)
        |
        v
render output
        |
        v
(next user input)
```

`apply_pending_controller_state` is called **before** `run_turn_streaming` starts.
At that point no streaming I/O is in progress and the session is not borrowed by
any other thread, making it safe to mutate.

The function signature (simplified):

```rust
fn apply_pending_controller_state(
    session: &mut AgentSession,
    controller: &TuiSessionController,
)
```

It calls `controller.apply_to_session(session)` which flushes all three pending
fields.

---

## Inner state: generation counters

`TuiSessionController` tracks two `u64` counters:

| Counter             | Meaning                                          |
|---------------------|--------------------------------------------------|
| `generation`        | Incremented every time a field is queued         |
| `applied_generation`| Set to `generation` after a successful apply     |

When `generation == applied_generation`, the controller is clean — no pending
mutations. This avoids redundant session writes on turns where nothing changed.

The three tracked fields:

| Field              | Type                    | Session target          |
|--------------------|-------------------------|-------------------------|
| `reasoning_effort` | `Option<ReasoningEffort>` | `AgentSession::reasoning_effort` |
| `active_model`     | `Option<ModelId>`        | `AgentSession::active_model`     |
| `active_provider`  | `Option<ProviderId>`     | `AgentSession::active_provider`  |

---

## Flow from controller into ModelRequest

```
Command handler
  └─ controller.queue_model(model_id)
       └─ inner.generation += 1
            └─ inner.active_model = Some(model_id)

apply_pending_controller_state (safe boundary)
  └─ controller.apply_to_session(&mut session)
       └─ session.active_model = inner.active_model
       └─ session.active_provider = inner.active_provider
       └─ session.reasoning_effort = inner.reasoning_effort
       └─ inner.applied_generation = inner.generation

run_turn_streaming
  └─ turn_loop::build_request(&session)
       └─ ModelRequest {
            model_id:    session.active_model.clone(),
            provider_id: session.active_provider.clone(),
            effort:      session.reasoning_effort,
            ...
          }
```

---

## Threading rules

| Thread          | Allowed operations                                      |
|-----------------|---------------------------------------------------------|
| Renderer thread | `controller.queue_*()` — acquires the inner `Mutex` briefly |
| Turn thread     | Reads `AgentSession` fields (after apply, no lock needed) |
| Main loop       | Calls `apply_pending_controller_state` at boundary       |

The inner `Mutex` is held only during queue and apply operations — never across
a streaming I/O call. The turn thread never touches the controller directly; it
reads the already-applied session fields.
