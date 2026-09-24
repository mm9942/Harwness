# Geteilter Zustand und Nebenläufigkeit — sprachunabhängig

**Regel:**
1. **Unveränderliche Daten teilen, nicht kopieren**: ein Objekt, viele Referenzen (Rust `Arc::clone(&x)`, sonst einfach dieselbe Referenz auf ein immutable Objekt).
2. **Veränderlicher geteilter Zustand** lebt hinter **genau einem** Mechanismus: Lock, Atomic, Channel/Queue oder Actor/Owner-Task. Bevorzugt: Nachrichten statt geteiltem Speicher.
3. **Locks kurz halten** — nie über `await`, I/O oder Callbacks in fremden Code hinweg.
4. **Thread-Safety wird vom Typsystem oder Tooling geprüft**, nicht per Kommentar behauptet (Rust `Send`/`Sync` automatisch ableiten lassen, Go `-race`, Java `java.util.concurrent`-Typen).

**Warum:** Race Conditions sind nicht reproduzierbar, treten unter Last auf und zerstören Daten still. Deep copies pro Thread erzeugen divergierende Zustände („jeder Worker hat eine andere Config“). Locks über `await`/I/O erzeugen Deadlocks und Latenzspitzen.

## Wann anwenden

- Daten werden zwischen Threads, Goroutines, Tasks, Workern oder async-Handlern geteilt.
- Globale/modulweite mutable Variablen, Singletons mit Zustand, Caches.
- Check-then-act über ein `await` hinweg (`if (!cache.has(k)) { await load(); cache.set(k, …) }`).
- Zähler/Statistiken aus mehreren Threads.
- Rust: `Arc`, `Mutex`, `RwLock`, `Send`/`Sync`-Fehler, `unsafe impl Send`.

## Entscheidungstabelle

| Situation | Mittel |
|---|---|
| Unveränderliche Daten, viele Leser | eine immutable Instanz teilen (Rust `Arc<T>`, Java `record`/`List.copyOf`, Python frozen dataclass/tuple, TS `Object.freeze`/`readonly`) |
| Einfache Zähler/Flags | Atomics (`AtomicU64`, `AtomicLong`, `sync/atomic`) |
| Kleiner mutable Zustand, kurze Zugriffe | Mutex/Lock (`Arc<Mutex<T>>`, `sync.Mutex`, `threading.Lock`, `ReentrantLock`) |
| Zustand mit komplexer Logik / langen Operationen | ein Owner-Task/Actor + Channel/Queue |
| Viele Leser, seltene Schreiber | RwLock oder Copy-on-write-Snapshot (Referenz atomar austauschen) |
| Single-threaded async (Node, asyncio) | kein Lock für Speicher nötig — aber Check-then-act über `await` ist trotzdem ein Race |

## Falsch

```python
# Python: += ist nicht atomar — GIL hin oder her
counter = 0
def worker():
    global counter
    for _ in range(100_000):
        counter += 1            # lesen, addieren, schreiben: verlorene Updates
```

```typescript
// TypeScript/Node: Race über await — zwei Requests laden doppelt und überschreiben sich
async function getUser(id: string) {
  if (!cache.has(id)) {
    const u = await db.load(id);   // hier kann ein anderer Request dazwischen
    cache.set(id, u);
  }
  return cache.get(id)!;
}
```

```go
// Go: Map aus mehreren Goroutines schreiben → "fatal error: concurrent map writes"
go func() { stats["a"]++ }()
go func() { stats["b"]++ }()
```

```rust
// Rust: deep copy statt geteiltem Pointer; manuell behauptete Thread-Safety
let kek_for_task = (*kek).clone();         // Kopie der Schlüsselbytes pro Task
unsafe impl Send for MasterKek {}          // redundant und gefährlich, ohne SAFETY-Begründung
```

## Richtig

### Rust — `Arc::clone`, Mutex nur um den mutablen Teil

```rust
let kek: Arc<MasterKek> = Arc::new(MasterKek::load()?);
let for_task = Arc::clone(&kek);                    // nur Refcount++, gleiche Daten
tokio::spawn(async move { use_kek(&for_task).await });

let stats = Arc::new(Mutex::new(Stats::default()));
{
    let mut s = stats.lock().await;                 // kurzer Scope, kein .await darin
    s.requests += 1;
}
// MasterKek enthält nur Send+Sync-Felder → Compiler leitet Send+Sync selbst ab, kein unsafe impl.
```

### Go — Mutex im Struct, oder Owner-Goroutine + Channel

```go
type Stats struct {
    mu     sync.Mutex
    counts map[string]int
}

func (s *Stats) Inc(key string) {           // Pointer-Receiver: Mutex nie kopieren (go vet copylocks)
    s.mu.Lock()
    defer s.mu.Unlock()
    s.counts[key]++
}

// Alternative: "Share memory by communicating"
incs := make(chan string)
go func() {
    counts := map[string]int{}              // gehört exklusiv dieser Goroutine
    for k := range incs {
        counts[k]++
    }
}()
```

Tests immer mit `go test -race`.

### Python — Lock bzw. Queue; asyncio: Lock pro Key oder Future-Cache

```python
lock = threading.Lock()
def worker() -> None:
    global counter
    for _ in range(100_000):
        with lock:
            counter += 1

# asyncio: laufende Ladeoperation teilen statt doppelt laden
_inflight: dict[str, asyncio.Task[User]] = {}
async def get_user(user_id: str) -> User:
    task = _inflight.get(user_id)
    if task is None:
        task = asyncio.create_task(db.load(user_id))
        _inflight[user_id] = task           # zwischen get und set liegt kein await → kein Race
    return await task
```

### TypeScript/Node — Promise cachen statt Ergebnis

```typescript
const inflight = new Map<string, Promise<User>>();
function getUser(id: string): Promise<User> {
  let p = inflight.get(id);
  if (!p) {
    p = db.load(id);
    inflight.set(id, p);                    // synchron gesetzt, bevor irgendein await läuft
  }
  return p;
}
```

Für echte Threads (`worker_threads`): Daten per `postMessage` übergeben (Kopie/Transfer); `SharedArrayBuffer` nur mit `Atomics`.

### Java — Concurrent-Typen statt eigener Synchronisation

```java
private final ConcurrentHashMap<String, LongAdder> counts = new ConcurrentHashMap<>();
void inc(String key) { counts.computeIfAbsent(key, k -> new LongAdder()).increment(); }
```

## Fallstricke

- **Lock über `await`/I/O** (Rust `std::sync::Mutex` über `.await`, Python `with lock:` um Netzwerk-Call) → Deadlock/Latenz. Daten unter Lock kopieren/entnehmen, Lock freigeben, dann I/O.
- **Nur Schreibzugriffe schützen**: Lesen ohne Lock sieht halbfertige Zustände (Go-Maps, Java `HashMap`).
- **Lock-Reihenfolge** bei mehreren Locks nicht fest → Deadlock. Eine globale Reihenfolge festlegen oder Locks vermeiden.
- **GIL ≠ Thread-Safety** und **Single-Thread-Event-Loop ≠ race-frei**: jedes `await` ist ein möglicher Wechselpunkt.
- **Deep copy „zur Sicherheit“** pro Thread → divergierende Zustände, verdoppelter Speicher, Secrets mehrfach im Speicher.
- **Thread-Safety per Hand behaupten** (`unsafe impl Send/Sync`, `@ThreadSafe`-Kommentar ohne Prüfung): überlebt spätere Änderungen nicht. Escape Hatches (`unsafe`, `any`, `# type: ignore`, `@SuppressWarnings`) nur minimal und immer mit Begründungskommentar (`// SAFETY: …`).
- **In-flight-Caches** (Promise/Task cachen): fehlgeschlagene Einträge wieder entfernen, sonst wird der Fehler für immer ausgeliefert.
- **Mutex kopieren** (Go: Struct mit `sync.Mutex` per Wert übergeben) → zwei unabhängige Locks.

## Checkliste

1. Welche Daten werden geteilt? Sind sie unveränderlich? Wenn ja: Referenz teilen, nicht kopieren.
2. Hat jeder mutable geteilte Zustand genau einen Schutzmechanismus?
3. Wird irgendein Lock über `await`/I/O/Callback gehalten?
4. Gibt es Check-then-act über einen Wechselpunkt (`await`, Lock-Freigabe)?
5. Wird Thread-Safety vom Typsystem/Tooling geprüft (Rust Send/Sync, `go test -race`, Concurrent-Collections)?
6. Sind Escape Hatches begründet kommentiert?
7. Rust zusätzlich: `rust-arc-sharing`, `rust-references-vs-raw-pointers`.

## Quelle

- https://doc.rust-lang.org/book/ch16-00-concurrency.html
- https://go.dev/blog/codelab-share
- https://go.dev/doc/articles/race_detector
- https://docs.python.org/3/library/threading.html#lock-objects
- https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/util/concurrent/package-summary.html
- https://nodejs.org/api/worker_threads.html
