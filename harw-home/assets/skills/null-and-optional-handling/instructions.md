# Fehlende Werte: Guard Clauses, Optional → Fehler, kein Force-Unwrap — sprachunabhängig

**Regel:** Behandle „Wert fehlt“ **explizit und früh**: per Guard Clause (Early Return) am Anfang, damit der Happy Path flach bleibt. Entscheide bewusst, ob Fehlen ein **normaler Fall** (Default/Skip) oder ein **Fehler** ist — im Fehlerfall wird daraus ein strukturierter Fehler mit Kontext (was fehlte, warum es erwartet war). Kein Force-Unwrap (`x!`, `.get()`, `.unwrap()`, `.expect()`) in Produktionspfaden.

**Warum:** Force-Unwraps verschieben den Fehler in einen Crash ohne Kontext (`NullPointerException`, `TypeError: Cannot read properties of undefined`, `panic: called Option::unwrap() on a None value`). Tiefe `if (x != null) { if (y != null) { … } }`-Verschachtelung versteckt die eigentliche Logik. Truthiness-Checks verwechseln „fehlt“ mit „ist 0 / leer“.

## Wann anwenden

- Ein Wert kann `null`/`undefined`/`None`/`nil`/`Option::None`/`Optional.empty()` sein.
- Du schreibst eine Funktion, die mehrere optionale Vorbedingungen prüft.
- Pflicht-Konfiguration, Header, Map-Lookup, „erstes Element“, Env-Variable.
- Review findet `!`, `.get()`, `.unwrap()`, `if x:` auf Zahlen/Strings, `||` als Default.

## Entscheidungsbaum

| Situation | Muster |
|---|---|
| Fehlen ist normal, es gibt einen sinnvollen Default | `??` / `or`-Default nur bei Nicht-Falsy-Typen / `.unwrap_or(…)` / `orElse(…)` |
| Fehlen ist normal, Schritt wird übersprungen | `if let` / `if (x != null)` / `if x is not None` ohne else |
| Fehlen macht Weiterarbeit sinnlos | Guard Clause: früh `return`/`continue`/Fehler |
| Fehlen ist ein Fehler des Aufrufers/der Umgebung | in strukturierten Fehler umwandeln (lazy gebaut) |
| Nur Inhalt lesen, nichts übernehmen | optional chaining / `as_deref()` statt Kopie |

## Falsch

```typescript
// TypeScript: Force-Unwrap + || verschluckt gültige 0
const port = Number(process.env.PORT) || 8080; // PORT=0 → 8080
const token = req.headers.authorization!.slice(7); // Crash ohne Kontext
```

```python
# Python: Truthiness statt None-Check, tiefe Verschachtelung
def discount(order):
    if order:
        if order.customer:
            if order.customer.rate:          # rate == 0 wird wie "fehlt" behandelt
                return order.total * order.customer.rate
    return 0
```

```go
// Go: Map-Zugriff ohne comma-ok — fehlender Key liefert still den Nullwert
timeout := cfg["timeout"] // "" wenn nicht gesetzt, niemand merkt es
```

```java
// Java: Optional.get() ist ein versteckter Force-Unwrap
String secret = config.secret().get();
```

## Richtig

### Rust — `let … else`, `ok_or_else`, `as_deref`

```rust
fn bearer_token(headers: &HeaderMap) -> Result<&str> {
    let Some(value) = headers.get(AUTHORIZATION) else {
        return Err(Error::Auth { reason: "authorization header missing".to_owned() });
    };
    let value = value.to_str().map_err(|_| Error::Auth { reason: "non-ascii header".to_owned() })?;
    value.strip_prefix("Bearer ").ok_or_else(|| Error::Auth {
        reason: "authorization header is not Bearer".to_owned(), // nur im None-Fall alloziert
    })
}

let q: Option<&str> = filter.q.as_deref(); // borgen statt Option<String> klonen
```

### TypeScript — Guard Clause, `??`, `?.`

```typescript
function bearerToken(headers: Headers): string {
  const value = headers.get("authorization");
  if (value === null) throw new AuthError("authorization header missing");
  if (!value.startsWith("Bearer ")) throw new AuthError("authorization header is not Bearer");
  return value.slice("Bearer ".length);
}

const port = process.env.PORT !== undefined ? Number(process.env.PORT) : 8080;
const city = order.customer?.address?.city ?? "unknown"; // ?? greift nur bei null/undefined
```

### Python — `is None`, Early Return

```python
def discount(order: Order | None) -> Decimal:
    if order is None or order.customer is None:
        return Decimal(0)
    rate = order.customer.rate
    if rate is None:                     # 0 bleibt ein gültiger Rabatt
        return Decimal(0)
    return order.total * rate

def require_env(key: str) -> str:
    value = os.environ.get(key)
    if value is None:
        raise ConfigMissingError(key)    # Fehler mit Kontext statt KeyError irgendwo später
    return value
```

### Go — comma-ok, frühe Rückgabe

```go
raw, ok := cfg["timeout"]
if !ok {
    return fmt.Errorf("config key %q missing", "timeout")
}

u := findUser(id)
if u == nil {
    return ErrNotFound
}
// ab hier ist u garantiert != nil — flacher Happy Path
```

### Java — Optional nur als Rückgabetyp, lazy Fehler

```java
String secret = config.secret()
    .orElseThrow(() -> new ConfigMissingException("SECUREHUB_AUTH_TOKEN_SECRET"));
```

## Fallstricke

- **Eager Default/Fehler**: `.ok_or(Error::new(format!(…)))`, `opt.orElse(expensive())`, `dict.get(k, compute())` bauen den Wert **immer**. Lazy Varianten nutzen: `ok_or_else`, `orElseGet`/`orElseThrow(supplier)`, explizites `if`.
- **`||` / `or` für Defaults** behandelt `0`, `""`, `false` als fehlend → `??` bzw. `is None`.
- **TS `!` (non-null assertion)** ist ein unbewiesenes Versprechen — nur wenn der Typchecker es nachweislich nicht sehen kann, mit Kommentar.
- **Java `Optional` als Feld/Parameter** — gedacht als Rückgabetyp; Parameter lieber überladen oder nullable annotieren.
- **Go nil-Interface-Falle**: ein `*MyErr(nil)` in einem `error`-Interface ist `!= nil`.
- **Fehlen als Fehler modellieren, obwohl es normal ist** → Exceptions als Kontrollfluss.
- **Doppelter Check** (`if x.is_some() { x.unwrap() }`) → Pattern/Guard nutzen, das den Wert direkt bindet.

## Checkliste

1. Ist für jeden optionalen Wert entschieden: Default, Skip oder Fehler?
2. Stehen Guard Clauses am Anfang, ist der Happy Path flach?
3. Kein `!`, `.get()`, `.unwrap()`, `.expect()` in Produktionspfaden?
4. `??` / `is None` statt `||` / Truthiness bei Zahlen, Strings, Booleans?
5. Werden Fehler/Defaults lazy gebaut?
6. Enthält der Fehler, **was** fehlte und **warum** es erwartet war?
7. Rust zusätzlich: `rust-if-let-let-else`, `rust-ok-or-else`, `rust-as-ref`.

## Quelle

- https://doc.rust-lang.org/book/ch06-03-if-let.html
- https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Operators/Nullish_coalescing
- https://docs.python.org/3/library/stdtypes.html#truth-value-testing
- https://go.dev/ref/spec#Index_expressions
- https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/util/Optional.html
