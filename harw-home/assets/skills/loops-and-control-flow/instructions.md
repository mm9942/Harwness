# Schleifen: Iteration statt Index, Retry mit Wert, Labels statt Flags — sprachunabhängig

**Regel:**
1. **Über Elemente iterieren, nicht über Indizes** (`for x in xs`, `for (const x of xs)`, `for _, x := range xs`). Brauchst du den Index, nimm die Paar-Form (`enumerate`, `.entries()`, `range` mit Index, `.iter().enumerate()`).
2. **Transformationen als Pipeline** (`map`/`filter`/Comprehension/Stream) statt leerer Liste + Schleife + `push`, solange es lesbar bleibt.
3. **Verschachtelte Schleifen** mit gelabeltem `break`/`continue` verlassen — oder die innere Suche in eine Funktion mit `return` extrahieren. Keine `found`-Flags.
4. **Retry-Schleifen liefern den Wert direkt** (Rust `loop { break wert }`, sonst `return` aus einer Hilfsfunktion) statt „Variable = null, Schleife, danach null prüfen“.
5. **Konsum-Schleifen** laufen, **bis die Quelle erschöpft/geschlossen ist** (`while let Some(x) = rx.recv().await`, `for v := range ch`, `for line in f`, `for await (const x of stream)`), ohne manuelles `while True` + `break` + Force-Unwrap.

**Warum:** Index-Schleifen erzeugen Off-by-one-Fehler und Index-out-of-range-Crashes, und sie funktionieren nicht für Iteratoren/Streams. Flags und Hilfsvariablen verteilen eine Entscheidung auf mehrere Stellen. Retry-Schleifen mit `null`-Sentinel enden oft in einem ungeprüften Force-Unwrap nach der Schleife.

## Wann anwenden

- `for (let i = 0; i < xs.length; i++)`, `for i in range(len(xs))`, `for i := 0; i < len(xs); i++` ohne echten Indexbedarf.
- Verschachtelte Schleife mit `found = true; break;` und äußerem `if (found) break;`.
- Retry/Polling: „bis zu N Versuche, dann Fehler“.
- Channel/Queue/Stream/Datei bis zum Ende lesen.

## Falsch

```python
for i in range(len(rows)):
    process(rows[i])

result = None
for _ in range(10):
    try:
        result = connect()
        break
    except ConnectionError:
        time.sleep(0.2)
result.send(msg)              # AttributeError, wenn alle Versuche scheiterten
```

```typescript
let found = false;
for (const row of matrix) {
  for (const cell of row) {
    if (cell === target) { found = true; break; } // verlässt nur die innere Schleife
  }
  if (found) break;
}
```

```go
for {
    msg, ok := <-ch
    if !ok {
        break
    }
    handle(msg)
}
```

## Richtig

### Rust — Iterator-Chains, `loop` mit Wert, Labels, `while let`

```rust
let items: Vec<ItemRow> = rows.into_iter().map(ItemRow::from).collect();

let mut retries = 0;
let file = loop {
    match File::open(&path).await {
        Ok(f) => break f,                                   // Wert direkt aus der Schleife
        Err(_) if retries < 10 => { retries += 1; sleep(Duration::from_millis(200)).await; }
        Err(e) => return Err(Error::Io(e)),
    }
};

'outer: for row in &matrix {
    for cell in row {
        if *cell == target { break 'outer; }
    }
}

while let Some(envelope) = rx.recv().await {                // endet, wenn alle Sender weg sind
    route(envelope).await;
}
```

### Python — `enumerate`, Comprehension, Funktion statt Flag, `for … else`

```python
for i, row in enumerate(rows):
    process(i, row)

names = [u.name for u in users if u.active]

def find(matrix, target) -> tuple[int, int] | None:     # Python hat keine Labels → extrahieren
    for r, row in enumerate(matrix):
        for c, cell in enumerate(row):
            if cell == target:
                return r, c
    return None

def connect_with_retry(attempts: int = 10) -> Connection:
    for _ in range(attempts):
        try:
            return connect()                              # Wert direkt zurück
        except ConnectionError:
            time.sleep(0.2)
    raise ConnectError(f"no connection after {attempts} attempts")

with open(path, encoding="utf-8") as f:
    for line in f:                                        # bis EOF
        handle(line)
```

### TypeScript — `for…of`, `.entries()`, Labels, async iteration

```typescript
for (const [i, row] of rows.entries()) process(i, row);

const names = users.filter((u) => u.active).map((u) => u.name);

outer: for (const row of matrix) {
  for (const cell of row) {
    if (cell === target) break outer;
  }
}

async function connectWithRetry(attempts = 10): Promise<Connection> {
  for (let n = 1; n <= attempts; n++) {
    try { return await connect(); }
    catch (e) { if (n === attempts) throw new ConnectError("no connection", { cause: e }); }
    await sleep(200);
  }
  throw new ConnectError("attempts must be >= 1");
}

for await (const chunk of stream) handle(chunk);
```

### Go — `range`, Labels, `range` über Channel

```go
for i, row := range rows {
    process(i, row)
}

outer:
for _, row := range matrix {
    for _, cell := range row {
        if cell == target {
            break outer
        }
    }
}

for msg := range ch { // endet, wenn ch geschlossen wird
    handle(msg)
}
```

## Fallstricke

- **Pipeline-Overkill**: fünf verkettete `map/filter/reduce` mit Nebenwirkungen sind schlechter als eine klare Schleife. Seiteneffekte gehören in Schleifen, nicht in `map`.
- **Collection während der Iteration verändern** (siehe `ownership-and-resource-lifetimes`).
- **Retry ohne Obergrenze/Backoff** → Endlosschleife bzw. Lastspitze; immer Limit, Backoff, letzten Fehler als Ursache weitergeben.
- **Go-Loop-Variablen** vor Go 1.22 teilten sich eine Variable über alle Iterationen (Closures/Goroutines!). Ab 1.22 pro Iteration neu — `go.mod`-Version prüfen.
- **JS `for…in`** iteriert Keys (inkl. geerbter), nicht Werte — für Arrays `for…of`.
- **Python-Labels gibt es nicht** — Funktion extrahieren oder `for … else` bewusst einsetzen.
- **Index wirklich nötig** (Nachbarelemente, paralleles Schreiben): dann bewusst Index-Schleife mit sauberer Grenze, oder `zip`/`windows`/`pairwise`.

## Checkliste

1. Wird über Elemente statt Indizes iteriert (bzw. Index-Paar-Form)?
2. Sind reine Transformationen als Pipeline/Comprehension geschrieben — ohne Seiteneffekte darin?
3. Keine `found`-Flags: Label oder extrahierte Funktion?
4. Liefert eine Retry-Schleife den Wert direkt und hat Limit + Backoff + Ursache?
5. Laufen Konsum-Schleifen bis zum Ende der Quelle, ohne Force-Unwrap?
6. Rust zusätzlich: `rust-loops-and-labels`, `rust-while-let`.

## Quelle

- https://doc.rust-lang.org/book/ch03-05-control-flow.html
- https://doc.rust-lang.org/book/ch13-02-iterators.html
- https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Statements/label
- https://go.dev/ref/spec#Break_statements
- https://go.dev/blog/loopvar-preview
- https://docs.python.org/3/tutorial/controlflow.html#break-and-continue-statements-and-else-clauses-on-loops
