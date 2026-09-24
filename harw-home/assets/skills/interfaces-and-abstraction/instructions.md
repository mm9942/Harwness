# Interfaces, Generics und dynamischer Dispatch — sprachunabhängig

**Regel:**
1. **Benenne Interfaces/Traits/Protocols nach Verhalten** (`KeyRepository`, `Clock`, `Notifier`, `Resolver`), nicht nach Technologie (`PostgresDatabase`, `RedisThing`) oder Sammelbegriffen (`DataHandler`, `Manager`).
2. **Extrahiere ein Interface erst ab zwei Implementierungen** — eine davon darf der Test-Fake sein. Halte es **klein** und definiere es dort, wo es **konsumiert** wird.
3. **Generics** für einen homogenen Algorithmus über viele Typen (ein Typ pro Aufruf, Typsicherheit, bei Rust/C#/Go ggf. ohne Laufzeitkosten).
4. **Dynamischer Dispatch** (Interface-Wert, `dyn Trait`, Basisklasse) für heterogene Sammlungen, Factories, Plugins, Dependency Injection und austauschbare Implementierungen zur Laufzeit.
5. **Mehrere Constraints** lesbar in `where`/`bound`/`extends`-Klauseln statt in einer überladenen Kopfzeile.

**Warum:** Verhaltens-Namen machen Implementierungen austauschbar (Postgres, In-Memory, Mock erfüllen dieselbe Rolle). Kleine, konsumentenseitige Interfaces halten Kopplung gering. Die falsche Wahl zwischen Generics und dynamischem Dispatch führt entweder zu Typ-Parameter-Explosion (`AppState<A, B, C, D, …>`) oder zu unnötigen Casts und verlorener Typsicherheit.

## Wann anwenden

- Dieselbe Logik existiert für 2+ konkrete Typen (Copy-Paste mit anderem Typ).
- Du brauchst für Tests eine Fake-/Mock-Implementierung.
- Eine Factory liefert je nach Config verschiedene Implementierungen.
- Eine Sammlung/ein App-State hält verschiedene Implementierungen desselben Verhaltens.
- Typ-Constraints machen eine Signatur unlesbar.

## Generics vs. dynamischer Dispatch

| Frage | Ja → |
|---|---|
| Verschiedene Implementierungen zur Laufzeit (Prod/Mock/Staging, Plugin)? | dynamisch |
| Sammlung/Struct-Felder mit gemischten Implementierungen? | dynamisch |
| Ein Algorithmus, pro Aufruf ein fester Typ, Rückgabetyp hängt vom Eingabetyp ab? | Generics |
| Performance-kritisch in Rust/C++/C# (Inlining)? | Generics |
| TS/Java/Python: Typsicherheit über Container/Funktionen? | Generics (zur Laufzeit gelöscht — kein Performance-Argument) |

## Falsch

```typescript
// Nach Technologie benannt, riesig, vom Produzenten definiert
interface PostgresDatabase {
  createKeyset(orgId: string): Promise<string>;
  listKeysets(orgId: string): Promise<Keyset[]>;
  appendAudit(entry: AuditEntry): Promise<void>;
  // … 30 weitere Methoden, jeder Konsument braucht 2 davon
}
```

```python
# Duplizierte Logik pro Typ
def to_json_invoice(x: Invoice) -> bytes: return json.dumps(asdict(x)).encode()
def to_json_customer(x: Customer) -> bytes: return json.dumps(asdict(x)).encode()
```

```rust
// Typ-Parameter-Explosion, obwohl Implementierungen zur Laufzeit gewählt werden
struct AppState<IR: InvoiceReadRepository, IL: InvoiceListRepository, U: UserRepository> { /* … */ }
```

## Richtig

### Rust — Trait nach Verhalten, `where`, `Arc<dyn …>` im App-State

```rust
pub trait KeyRepository: Send + Sync {
    fn create_keyset(&self, org_id: Uuid, label: &str) -> impl Future<Output = Result<Uuid>> + Send;
}

// Generisch: ein Algorithmus, beliebiger serialisierbarer Typ
fn dto_response<T>(status: StatusCode, value: &T) -> HandlerResult
where
    T: serde::Serialize,
{
    /* … */
}

// Dynamisch: austauschbare Implementierungen ohne Typ-Parameter-Explosion
pub struct AppState {
    pub invoices: Arc<dyn InvoiceReadRepository>,
    pub users: Arc<dyn UserRepository>,
}
pub fn build_vault() -> Result<Box<dyn SecretVault>> { /* FileVault oder InMemoryVault */ }
```

(Hinweis: `dyn`-fähige Traits brauchen objektsichere Methoden; `async fn`/`impl Future` im Trait ist nicht `dyn`-kompatibel — dort boxed Futures oder ein separater Trait.)

### TypeScript — kleines Interface beim Konsumenten, Generics mit `extends`

```typescript
// im Modul, das es BRAUCHT:
export interface KeyRepository {
  createKeyset(orgId: string, label: string): Promise<string>;
}

export class KeysetService {
  constructor(private readonly keys: KeyRepository) {}  // Prod: PgKeyRepository, Test: InMemoryKeyRepository
}

function toJson<T extends object>(value: T): string {
  return JSON.stringify(value);
}
```

TypeScript ist strukturell typisiert: jede Klasse/jedes Objekt mit passenden Methoden erfüllt das Interface — kein `implements` nötig (aber als Dokumentation hilfreich).

### Python — `Protocol` (strukturell) und Generics (PEP 695, 3.12+)

```python
from typing import Protocol

class KeyRepository(Protocol):
    async def create_keyset(self, org_id: UUID, label: str) -> UUID: ...

class KeysetService:
    def __init__(self, keys: KeyRepository) -> None:
        self._keys = keys

def to_json[T: SupportsAsdict](value: T) -> bytes:   # vor 3.12: TypeVar("T", bound=SupportsAsdict)
    return json.dumps(value.asdict()).encode()
```

`Protocol` statt `ABC`, wenn Implementierungen nicht erben sollen (Fakes in Tests, Fremdklassen).

### Go — „accept interfaces, return structs“, Interface beim Konsumenten

```go
// package keyset (Konsument)
type KeyRepository interface {
    CreateKeyset(ctx context.Context, orgID uuid.UUID, label string) (uuid.UUID, error)
}

type Service struct{ keys KeyRepository }

func NewService(keys KeyRepository) *Service { return &Service{keys: keys} } // gibt konkreten Typ zurück

// Generics mit Constraint (Go 1.21+)
func MaxOf[T cmp.Ordered](xs []T) (T, bool) {
    var zero T
    if len(xs) == 0 {
        return zero, false
    }
    return slices.Max(xs), true
}
```

### Java — Interface + Generics mit Bounds

```java
public interface KeyRepository {
    UUID createKeyset(UUID orgId, String label);
}

static <T extends Comparable<? super T>> T maxOf(Collection<? extends T> xs) {
    return Collections.max(xs);
}
```

## Fallstricke

- **Interface für genau eine Implementierung ohne Testbedarf** — reine Indirektion. Erst extrahieren, wenn die zweite kommt.
- **Fette Interfaces** (20+ Methoden) zwingen Fakes, alles zu implementieren → nach Konsument aufteilen (Interface Segregation).
- **Technologie im Namen** (`S3Storage` als Interface) — die Implementierung heißt so, nicht der Vertrag (`BlobStore`).
- **Generics, wo der Typ nur durchgereicht wird** — `fn f<T: Trait>(x: T)` für eine Methode, die nur einmal mit einem Typ aufgerufen wird, ist Overengineering.
- **Downcasts/`instanceof`-Kaskaden** hinter einem Interface → das Interface fehlt eine Methode oder sollte eine Union/sealed Hierarchie sein (siehe `exhaustive-case-handling`).
- **Thread-Safety-Bounds vergessen**: in Rust `Send + Sync` als Supertrait, wenn das Objekt hinter `Arc<dyn …>` in Tasks geteilt wird.

## Checkliste

1. Benennt der Name ein **Verhalten**?
2. Gibt es mindestens zwei Implementierungen (inkl. Test-Fake)?
3. Ist das Interface klein und beim Konsumenten definiert?
4. Generics oder dynamischer Dispatch — passend zur Tabelle oben entschieden?
5. Sind mehrere Constraints in einer `where`/bound-Klausel lesbar?
6. Rust zusätzlich: `rust-traits-defining`, `rust-trait-bounds`, `rust-generic-functions`, `rust-monomorphization-vs-dyn`, `rust-box-dyn`.

## Quelle

- https://doc.rust-lang.org/book/ch10-02-traits.html
- https://doc.rust-lang.org/book/ch18-02-trait-objects.html
- https://www.typescriptlang.org/docs/handbook/2/generics.html
- https://typing.python.org/en/latest/spec/protocol.html
- https://go.dev/doc/effective_go#interfaces
- https://go.dev/doc/tutorial/generics
