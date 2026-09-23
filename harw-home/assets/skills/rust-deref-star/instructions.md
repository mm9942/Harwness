# `*` Dereference — Copy Types Out of Guards and Wrappers Without Clone

**Regel:** To read a `Copy` value (integer, bool, Uuid, enum with Copy) that sits behind a `MutexGuard`, `Arc`, `Ref`, or any `Deref`-implementing wrapper, use `*guard` to copy the value out. Never call `.clone()` on a `Copy` type — it is redundant and misleads readers into thinking heap allocation or a reference-count bump is happening.

**Warum:** `*guard` calls `Deref::deref()` which returns `&T`, then the leading `*` reads through that reference. For `Copy` types this produces a bitwise copy on the stack — zero allocation. `.clone()` on a `Copy` type compiles to the same thing but signals "expensive clone" to every reader.

## Falsch

```rust
use tokio::sync::Mutex;
use std::sync::Arc;

async fn next_id(counter: &Arc<Mutex<u64>>) -> u64 {
    let guard = counter.lock().await;
    guard.clone()  // ✗ .clone() on a Copy type — misleads; u64 does not need it
}
```

## Richtig

```rust
use tokio::sync::Mutex;
use std::sync::Arc;

/// Copy a `u64` counter value out of a `MutexGuard` using `*deref`.
///
/// Real code: apps/sgh-flow/src/db/repositories/cost_center_approvers.rs:212-213
///   let mut id_guard = self.next_id.lock().await;  // Mutex<u64>
///   let id = *id_guard;                            // copy the u64 out
///   *id_guard += 1;                                // then mutate through the guard
async fn next_id(counter: &Arc<Mutex<u64>>) -> u64 {
    let guard = counter.lock().await;
    *guard  // ✓ dereferences MutexGuard<u64> → &u64 → copies u64 off the stack
}

/// Same pattern for a plain shared reference to a Copy field.
fn log_count(count: &u32) {
    let n = *count;   // ✓ copy the u32 — no .clone() needed
    println!("count = {n}");
}

/// Arc::clone clones the pointer, not the inner data.
/// To read a Copy value inside an Arc without cloning the Arc's data:
fn read_arc_value(arc: &Arc<u64>) -> u64 {
    **arc   // outer * dereferences Arc → &u64, inner * copies the u64
}
```

## Quelle
- https://doc.rust-lang.org/book/ch15-02-deref.html
