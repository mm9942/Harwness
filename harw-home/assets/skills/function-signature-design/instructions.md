# Signaturen: allgemein annehmen, konkret zurückgeben, nur bei Bedarf kopieren

**Regel:**
1. **Parameter**: nimm die **allgemeinste lesende Abstraktion** an, die die Funktion wirklich braucht (`&str`/`&[T]`/`&Path`, `Iterable`/`Sequence`/`Mapping`, `readonly T[]`, `io.Reader`, `Collection<? extends T>`/`CharSequence`).
2. **Rückgabe**: gib den **konkreten, besessenen** Typ zurück (`String`/`Vec<T>`, `list`, `T[]`, `*Struct`, `List<T>`), wenn der Aufrufer ihn besitzen soll.
3. **Borgen statt besitzen**: eine Funktion, die nur liest, übernimmt keine Ownership und kopiert nicht defensiv.
4. **Copy-on-write**: wenn eine Transformation im Normalfall nichts ändert, gib die Eingabe unverändert (bzw. eine Sicht darauf) zurück und alloziere nur im Änderungsfall — und dokumentiere das.

**Warum:** Allgemeine Parameter machen Funktionen für mehr Aufrufer nutzbar, ohne dass diese konvertieren oder kopieren müssen (Literal, String, Wrapper, Generator, Datei, Netzwerkstrom). Konkrete Rückgaben geben dem Aufrufer die volle API. Unnötiges Besitzen/Kopieren kostet Allokationen und — schlimmer — verschleiert, wer die Daten verändert.

## Wann anwenden

- Neue öffentliche Funktion/Methode, oder Review einer Signatur.
- Aufrufer müssen vor dem Aufruf `list(...)`, `Array.from(...)`, `.to_string()`, `ioutil.ReadAll` o. Ä. machen.
- Parameter ist `List`/`ArrayList`/`list`/`string[]`/`&Vec<T>`/`&String`, obwohl nur iteriert/gelesen wird.
- Funktion nimmt eine Datei/einen Pfad, obwohl sie eigentlich nur Bytes liest.
- Funktion gibt immer eine neue Kopie zurück, obwohl sie meist nichts ändert.

## Tabelle: lesender Parameter → besser

| Sprache | Statt | Besser |
|---|---|---|
| Rust | `&String`, `&Vec<T>`, `&PathBuf`, `Option<&String>` | `&str`, `&[T]`, `&Path`, `impl AsRef<Path>`, `Option<&str>` (`.as_deref()`) |
| TypeScript | `T[]` (nur gelesen), `Map<K,V>` | `readonly T[]` / `Iterable<T>`, `ReadonlyMap<K,V>` |
| Python | `list[T]`, `dict[K,V]`, `str` für Pfade | `Iterable[T]` / `Sequence[T]`, `Mapping[K,V]`, `str \| os.PathLike[str]` |
| Go | `*os.File`, `[]string` nur für Iteration, `*bytes.Buffer` | `io.Reader` / `io.Writer`, `iter.Seq[string]` (1.23+) oder Slice, `fs.FS` |
| Java | `ArrayList<T>`, `String`, `File` | `Collection<? extends T>` / `Iterable<? extends T>`, `CharSequence`, `Path` |

## Falsch

```rust
fn store_secret(name: &String, plaintext: &Vec<u8>, cfg: &PathBuf) { /* … */ } // Aufrufer mit Literal muss allozieren
fn strip_ansi(s: &str) -> String { if !s.contains('\x1b') { return s.to_owned(); } /* … */ } // alloziert immer
```

```python
def total(prices: list[Decimal]) -> Decimal:  # Generator, tuple, dict.values() passen nicht
    return sum(prices, Decimal(0))

def normalize(tags: list[str]) -> list[str]:
    tags = copy.deepcopy(tags)                  # defensive Kopie, obwohl nur gelesen wird
    return [t.lower() for t in tags]
```

```go
func CountLines(f *os.File) (int, error) { /* … */ } // nicht nutzbar für HTTP-Body, gzip, strings.Reader, Tests
```

```typescript
function sum(xs: number[]): number { /* … */ } // readonly-Arrays des Aufrufers werden abgelehnt
```

## Richtig

### Rust — Slices, `AsRef`, `as_deref`, `Cow`

```rust
use std::borrow::Cow;
use std::path::Path;

fn store_secret(name: &str, plaintext: &[u8], cfg: &Path) { /* … */ }
fn set_env_if_absent(key: &str, value: impl AsRef<str>) { /* value.as_ref() */ }
let q: Option<&str> = filter.q.as_deref();          // Option<String> → Option<&str> ohne Klon

// Deref-Koerzion: &Zeroizing<Vec<u8>> → &Vec<u8> → &[u8] automatisch
open_envelope(&secret_key /* Zeroizing<Vec<u8>> */);

fn strip_ansi(s: &str) -> Cow<'_, str> {
    if !s.contains('\x1b') {
        return Cow::Borrowed(s);                     // kein Alloc im Normalfall
    }
    Cow::Owned(remove_escape_sequences(s))
}

fn build_key_name(prefix: &str, suffix: &str) -> String { format!("{prefix}-{suffix}") } // konkret, owned
```

### TypeScript — `readonly`/`Iterable` rein, konkretes Array raus

```typescript
function sum(xs: Iterable<number>): number {
  let total = 0;
  for (const x of xs) total += x;
  return total;
}
function topN(xs: readonly Score[], n: number): Score[] {
  return xs.toSorted((a, b) => b.value - a.value).slice(0, n); // neue, besessene Liste
}
```

### Python — `Iterable`/`Sequence`/`Mapping`, `PathLike`

```python
from collections.abc import Iterable, Mapping
from os import PathLike

def total(prices: Iterable[Decimal]) -> Decimal:
    return sum(prices, Decimal(0))

def normalize(tags: Iterable[str]) -> list[str]:        # keine defensive Kopie nötig
    return [t.lower() for t in tags]

def load(path: str | PathLike[str], overrides: Mapping[str, str]) -> Config: ...
```

### Go — kleine Interfaces rein, konkrete Structs raus

```go
func CountLines(r io.Reader) (int, error) {
    sc := bufio.NewScanner(r)
    n := 0
    for sc.Scan() {
        n++
    }
    return n, sc.Err()
}
// funktioniert mit *os.File, http.Response.Body, gzip.Reader, strings.NewReader("…") im Test
```

### Java

```java
static BigDecimal total(Iterable<? extends BigDecimal> prices) { /* … */ }
static List<String> normalize(Collection<String> tags) {
    return tags.stream().map(String::toLowerCase).toList();   // unveränderliche, konkrete Liste
}
```

## Fallstricke

- **Zu allgemein**: `Iterable` annehmen, aber zweimal iterieren → Generator/Stream ist beim zweiten Mal leer. Dann `Sequence`/`Collection`/Slice verlangen oder einmal materialisieren.
- **Copy-on-write + Aliasing**: wenn die Eingabe unverändert zurückkommt, darf der Aufrufer sie danach nicht als „eigene Kopie“ verändern — in GC-Sprachen dokumentieren oder immutable Typen nutzen (siehe `ownership-and-resource-lifetimes`).
- **Ownership übernehmen, nur um zu lesen** (Rust `fn f(s: String)`) — Aufrufer verliert den Wert oder muss klonen.
- **Rückgabe zu abstrakt** (`Iterable` statt `list`) nimmt dem Aufrufer `len()`, Indexzugriff, zweimaliges Iterieren — ohne Gewinn.
- **Interface-Parameter in Go, aber Rückgabe als Interface** — gegen „accept interfaces, return structs“.
- **Speichern verlangt Kopie**: Nimmt eine Funktion etwas an, um es **dauerhaft zu speichern**, ist Ownership (bzw. eine Kopie beim Speichern) korrekt.

## Checkliste

1. Nur gelesen? → allgemeinste lesende Abstraktion als Parameter.
2. Wird gespeichert oder verändert? → Ownership/Kopie bewusst an genau dieser Stelle.
3. Rückgabe konkret und besessen, wo der Aufrufer sie besitzen soll?
4. Wird mehrfach iteriert? → kein Einweg-Iterable annehmen.
5. Unveränderter Normalfall → Eingabe/Sicht zurückgeben statt neu allozieren (dokumentiert)?
6. Rust zusätzlich: `rust-borrow-vs-owned-params`, `rust-as-ref`, `rust-deref-coercion`, `rust-cow`.

## Quelle

- https://doc.rust-lang.org/book/ch04-03-slices.html
- https://doc.rust-lang.org/std/borrow/enum.Cow.html
- https://docs.python.org/3/library/collections.abc.html
- https://go.dev/doc/effective_go#interfaces_and_types
- https://www.typescriptlang.org/docs/handbook/2/objects.html#the-readonlyarray-type
