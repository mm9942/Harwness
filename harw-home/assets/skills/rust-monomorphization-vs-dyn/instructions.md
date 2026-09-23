# Monomorphisierung vs. `dyn Trait`

**Regel:** Nutze Generics (`<T: Trait>`) wenn alle Werte denselben konkreten Typ haben und Performance wichtig ist; nutze `Arc<dyn Trait>` / `Box<dyn Trait>` wenn eine Sammlung verschiedene Implementierungen halten muss (Plugin-Muster, Dependency Injection, Test-Mocks).

**Warum:** Generics werden zur Compile-Zeit monomorphisiert — der Compiler emittiert eine Kopie der Funktion pro konkretem Typ. Das erlaubt Inlining und hat null vtable-Overhead, erzwingt aber Homogenität. `dyn Trait` hingegen löst den Methodenaufruf per vtable zur Laufzeit auf: geringe Kosten (~ns), dafür ist jede Implementierung einzeln austauschbar ohne Recompilierung des Aufrufers. Im sgh-flow-Stil sind Repositories genau dieser Fall: ein `AppState` hält viele verschiedene Repos als `Arc<dyn …>` damit Tests Mock-Implementierungen einstecken können.

## Falsch

```rust
// Generics erzwingen denselben Typ für alle Repos — nicht möglich wenn
// PgInvoiceReadRepository und MockInvoiceReadRepository je nach Umgebung
// eingesteckt werden sollen.
struct AppState<IR: InvoiceReadRepository, IL: InvoiceListRepository> {
    invoice_read: Arc<IR>,
    invoice_list: Arc<IL>,
    // ... weitere 10 Typ-Parameter → unlensbare Signatur
}
```

## Richtig

```rust
use std::sync::Arc;

/// Gemeinsamer App-Zustand; jedes Repository ist via `Arc<dyn …>` austauschbar.
///
/// # Concurrency
/// Alle `Arc<dyn …>`-Felder sind `Send + Sync` (durch den Trait-Bound sichergestellt).
/// Clone des `AppState` ist billig: es werden nur Arc-Pointer kopiert.
// apps/sgh-flow/src/app.rs:180-207
struct AppState {
    pub invoice_read: Arc<dyn InvoiceReadRepository>,
    pub invoice_list: Arc<dyn InvoiceListRepository>,
    pub sachkonto:    Arc<dyn SachkontoRepository>,
    // ... weitere Repos ohne Typ-Parameter-Explosion
}

// Traits müssen Send + Sync binden damit Arc<dyn …> threadsicher ist:
pub trait InvoiceReadRepository: Send + Sync {
    fn get(&self, id: uuid::Uuid) -> crate::error::AppResult<Invoice>;
}

// Generics weiterhin richtig für eine einzige homogene Operation:
fn serialize_response<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}
```

**Entscheidungsbaum:**

| Frage | Antwort → Wahl |
|---|---|
| Brauche ich verschiedene Implementierungen zur Laufzeit (Mock, Prod, Staging)? | Ja → `Arc<dyn Trait>` |
| Hält eine Sammlung (`Vec`, Struct-Felder) gemischte Typen? | Ja → `Arc<dyn Trait>` |
| Ist die Operation performance-kritisch und der Typ immer gleich? | Ja → Generics |
| Soll der Compiler alle Typen statisch prüfen ohne vtable? | Ja → Generics |

## Quelle
- https://doc.rust-lang.org/book/ch18-02-trait-objects.html
