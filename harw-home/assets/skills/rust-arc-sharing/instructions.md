# Arc<T> — Shared Immutable Data Across Threads

**Regel:** Teile unveränderliche Daten über Threads mit `Arc<T>`. Klone **nur den Pointer** mit `Arc::clone(&a)` — nie die inneren Daten.

**Warum:** `Arc::clone(&a)` ist ein atomarer Ref-Count-Increment (~ns). `(*arc).clone()` oder `.clone()` auf dem Innenwert kopiert alle Bytes der Datenstruktur — teuer und semantisch falsch: zwei Threads arbeiten dann auf unterschiedlichen Kopien, nicht auf einem gemeinsamen Wert.

Das Rust Book (Kap. 15.4) formuliert es so: *"The call to Rc::clone only increments the reference count, which doesn't take much time. Deep copies of data can take a lot of time. By using Rc::clone for reference counting, we can visually distinguish between the deep-copy kinds of clones and the kinds of clones that increase the reference count."* (`Arc` = thread-safe-Variante von `Rc`; gleiche Konvention.)

## Falsch

```rust
// Klont die gesamten Bytes von MasterKek — separate Kopie, kein geteilter Zustand
let kek: Arc<MasterKek> = Arc::new(MasterKek::new(raw_key));

let kek_for_handler = (*kek).clone();  // ❌ deep-copy der inneren Daten
let kek_for_task    = kek.as_ref().clone();  // ❌ ebenfalls deep-copy
```

## Richtig

```rust
use std::sync::Arc;

// Einmal allozieren, mehrfach als Pointer weitergeben.
let kek: Arc<MasterKek> = Arc::new(MasterKek::new(raw_key));

// Jeder Clone erhöht nur den atomaren Ref-Count — keine Datenkopie.
let kek_for_handler = Arc::clone(&kek);   // ✅
let kek_for_task    = Arc::clone(&kek);   // ✅

// Realer Einsatz in acme-secure-hub:
// securehub-service/src/service.rs:100
//   master_kek: Arc<MasterKek>,
// securehub-service/src/service.rs:137
//   pub fn new(pool: PgPool, master_kek: Arc<MasterKek>, ...) -> Self { ... }
//
// Realer Einsatz in acme-app:
// apps/acme-app/src/app.rs:113
//   pub type AppState = Arc<AppContext>;
tokio::spawn(async move { use_kek(kek_for_handler).await });
tokio::spawn(async move { use_kek(kek_for_task).await });
```

## Wann Arc, wann was anderes?

| Situation | Mittel |
|---|---|
| Unveränderliche Daten, mehrere Threads | `Arc<T>` |
| Daten müssen mutierbar sein | `Arc<Mutex<T>>` oder `Arc<RwLock<T>>` |
| Single-threaded shared ownership | `Rc<T>` (kein `Send`) |
| Nur borrowen, kein Ownership teilen | `&T` mit Lifetime |

## Quelle

- <https://doc.rust-lang.org/book/ch15-04-rc.html> — Kap. 15.4: Rc<T>, the Reference Counted Smart Pointer (Arc = thread-safe Variante, gleiche Clone-Konvention)

Allgemeine Variante: `shared-state-and-concurrency`
