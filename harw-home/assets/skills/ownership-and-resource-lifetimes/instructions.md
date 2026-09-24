# Ownership, Aliasing und Ressourcen-Lebensdauer — sprachunabhängig

**Regel:** Jedes Datum und jede Ressource hat **genau einen Owner**, der es verändern darf und für die Freigabe verantwortlich ist. Wer nur liest, bekommt eine **Referenz/Sicht**, keine Kopie. Kopien sind **bewusst und sichtbar** (`to_owned`, `structuredClone`, `copy.deepcopy`, `slices.Clone`, `List.copyOf`). Halte **keine Referenz in etwas, das du gleichzeitig veränderst oder weitergibst**. Ressourcen (Dateien, Connections, Locks, Temp-Verzeichnisse) werden **deterministisch** freigegeben: RAII/`Drop`, `with`, `defer`, `using`, try-with-resources.

**Warum:** Rust erzwingt das per Borrow-Checker. In GC-Sprachen gelten dieselben Regeln — nur prüft sie niemand: geteilte Listen werden unbemerkt verändert, Iteratoren werden ungültig, wiederverwendete Puffer überschreiben „alte“ Ergebnisse, und nicht geschlossene Handles laufen erst unter Last voll. Der GC gibt **Speicher** frei, aber **nicht rechtzeitig** Dateien, Sockets oder Locks.

## Wann anwenden

- Eine Funktion bekommt eine Liste/Map/Objekt und verändert es (oder soll es gerade nicht).
- Aufrufer „sieht plötzlich andere Daten“; Tests beeinflussen sich gegenseitig.
- Iteration + Mutation derselben Collection.
- Datei/Connection/Lock/Temp-Ressource wird geöffnet.
- Rust: E0382 (use after move), E0505 (move while borrowed), Lifetime-Fehler, `.clone()` überall.

## Kernideen und ihre Entsprechungen

| Idee | Rust | TypeScript | Python | Go |
|---|---|---|---|---|
| Nur lesen | `&T`, `&str`, `&[T]` | `readonly T[]`, `Readonly<T>` | `Sequence`, `Mapping`, `tuple` | Wert/Slice lesen, nicht `append` |
| Übergabe der Verantwortung | Move (`self` by value) | Konvention + Doku | Konvention + Doku | Konvention (z. B. „nach Send nicht mehr anfassen“) |
| Bewusste Kopie | `.to_owned()`, `.to_vec()` | `[...xs]`, `structuredClone` | `list(xs)`, `copy.deepcopy` | `slices.Clone`, `maps.Clone` |
| Deterministische Freigabe | `Drop` (RAII) | `try/finally`, `using` (TS 5.2+) | `with` | `defer` |

## Falsch

```python
# Python: mutable Default-Argument — ein Objekt für ALLE Aufrufe
def add_item(item, items=[]):
    items.append(item)
    return items

# Iteration + Mutation derselben Map
for key in cache:
    if expired(key):
        del cache[key]          # RuntimeError: dictionary changed size during iteration

f = open(path)                  # wird nie geschlossen, wenn parse() wirft
data = parse(f.read())
```

```go
// Go: append-Aliasing — beide Slices teilen sich das Backing-Array
base := make([]int, 3, 10)
a := append(base, 1)
b := append(base, 2)            // überschreibt a[3]! a == [0 0 0 2]

// Wiederverwendeter Puffer: Scanner.Bytes() gilt nur bis zum nächsten Scan()
var lines [][]byte
for sc.Scan() {
    lines = append(lines, sc.Bytes()) // alle Einträge zeigen in denselben Puffer
}
```

```typescript
// TypeScript: Funktion verändert das Array des Aufrufers
function topThree(scores: number[]): number[] {
  return scores.sort((a, b) => b - a).slice(0, 3); // sort() sortiert IN PLACE
}
```

```rust
// Rust E0505: Referenz in `request` lebt noch, während `request` wegbewegt wird
match (request.method(), handler) {
    (method, h) => dispatch(method, h, request).await, // method borgt aus request
}
```

## Richtig

### Rust — borgen zum Lesen, Move für Übergabe, abgeleitete Werte vorher ownen

```rust
fn log_name(name: &str) { tracing::info!(%name); }       // Aufrufer behält den String

let method = request.method().to_owned();                // eigener Wert, keine Referenz in request
match (&method, handler) {
    (_, h) => dispatch(&method, h, request).await,       // request darf jetzt bewegt werden
}

let table = parts[parts.len() - 1].to_owned();          // sichtbare, bewusste Kopie (&str → String)
// Kleine Copy-Werte hinter einem Guard: `*guard` statt `.clone()`
let id = *counter.lock().await;
```

### Python — `None`-Default, Kopie beim Iterieren, `with`

```python
def add_item(item: Item, items: list[Item] | None = None) -> list[Item]:
    items = [] if items is None else list(items)   # eigene Kopie, Aufrufer unberührt
    items.append(item)
    return items

for key in list(cache):          # über Snapshot der Keys iterieren
    if expired(key):
        del cache[key]

with open(path, encoding="utf-8") as f:            # schließt auch bei Exception
    data = parse(f.read())
```

### Go — Kapazität begrenzen / klonen, Puffer kopieren, `defer`

```go
a := append(base[:len(base):len(base)], 1) // volle Slice-Expression erzwingt neues Array
b := append(slices.Clone(base), 2)

for sc.Scan() {
    lines = append(lines, bytes.Clone(sc.Bytes())) // eigene Kopie pro Zeile
}

f, err := os.Open(path)
if err != nil {
    return fmt.Errorf("open %s: %w", path, err)
}
defer f.Close()
```

### TypeScript — nicht-mutierende Varianten, `readonly`, `finally`/`using`

```typescript
function topThree(scores: readonly number[]): number[] {
  return scores.toSorted((a, b) => b - a).slice(0, 3); // ES2023, Original bleibt unverändert
}

const conn = await pool.connect();
try {
  await conn.query(sql);
} finally {
  conn.release();                                      // oder `await using` (TS 5.2+, Symbol.asyncDispose)
}
```

### Java — unveränderliche Kopien, try-with-resources

```java
public Order(List<Line> lines) { this.lines = List.copyOf(lines); } // Aufrufer kann nicht mehr hineinschreiben

try (var in = Files.newBufferedReader(path)) {
    return parse(in);
}
```

## Fallstricke

- **Shallow vs. deep copy**: `[...xs]`, `list(xs)`, `slices.Clone` kopieren nur die äußere Ebene; verschachtelte Objekte bleiben geteilt.
- **Defensive Kopie überall** ist das andere Extrem (Kosten, Rust: `.clone()`-Wildwuchs). Lieber Parameter als read-only typisieren (siehe `function-signature-design`) und nur beim **Speichern** kopieren.
- **Sichten, die den Owner überleben**: Go-`Scanner.Bytes()`, Node-`Buffer.subarray()`, Python-`memoryview`, Rust-Referenzen mit falscher Lifetime. Eine Sicht darf nicht länger leben als das, worauf sie zeigt — sonst kopieren.
- **Nach der Übergabe weiterbenutzen**: Objekt an Goroutine/Channel/Worker/Queue übergeben und danach noch verändern → Race. Rust verhindert das per Move; anderswo per Konvention „nach Übergabe nicht mehr anfassen“.
- **Finalizer/`__del__`/GC als Freigabe** — nicht deterministisch; immer explizit schließen.
- **Rust-Lifetimes**: nur annotieren, wenn die Elision-Regeln nicht greifen; Struct mit Referenzfeld nur, wenn die Struct wirklich kurzlebiger ist als die Daten — sonst owned Felder.

## Checkliste

1. Wer ist Owner dieses Datums/dieser Ressource? Ist das im Code oder in der Doku erkennbar?
2. Verändert die Funktion Eingaben? Wenn nein: read-only typisiert? Wenn ja: dokumentiert?
3. Sind Kopien bewusst (und nur dort, wo gespeichert oder verändert wird)?
4. Wird irgendwo iteriert und gleichzeitig dieselbe Collection verändert?
5. Lebt eine Sicht/Referenz länger als ihr Owner (wiederverwendete Puffer!)?
6. Wird jede geöffnete Ressource auch im Fehlerfall freigegeben (`with`/`defer`/`using`/`finally`/RAII)?
7. Rust zusätzlich: `rust-move-semantics`, `rust-to-owned-vs-clone`, `rust-borrow-checker-e0505`, `rust-lifetime-elision`, `rust-explicit-lifetimes`, `rust-struct-lifetimes`, `rust-deref-star`.

## Quelle

- https://doc.rust-lang.org/book/ch04-00-understanding-ownership.html
- https://go.dev/blog/slices-intro
- https://pkg.go.dev/bufio#Scanner.Bytes
- https://docs.python.org/3/reference/compound_stmts.html#the-with-statement
- https://www.typescriptlang.org/docs/handbook/release-notes/typescript-5-2.html
- https://docs.oracle.com/javase/tutorial/essential/exceptions/tryResourceClose.html
