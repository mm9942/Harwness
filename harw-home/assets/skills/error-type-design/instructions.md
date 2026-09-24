# Typisierte Fehler-Hierarchien — sprachunabhängig

**Regel:** Jedes Modul bzw. jede fachliche Grenze (Package, Service, Library) hat **einen eigenen Fehler-Basistyp** mit klar benannten Fällen. Jeder Fall trägt **strukturierte Felder** (welcher Key, welche ID, welches Feld) statt nur eines Strings, und **erhält die Ursache** (`source()` / `cause` / `__cause__` / `Unwrap()` / `InnerException`). Nicht implementierte Pfade sind ein eigener Fehlerfall — kein Crash, kein `TODO`-Throw.

**Warum:** Aufrufer müssen auf Fehler **reagieren** können (Retry bei Timeout, 404 bei NotFound, 400 bei Validation). Das geht nur, wenn der Fehler einen unterscheidbaren Typ/Fall hat — nicht, wenn man Message-Strings parsen muss. Generische Sammeltypen (`Error`, `Exception`, `anyhow::Error`, `errors.New("...")` + String-Vergleich) verstecken die Fälle; verlorene Ursachen machen Debugging in Produktion zum Ratespiel.

## Wann anwenden

- Ein neues Modul/Package kann fehlschlagen und braucht einen Fehlertyp.
- Ein bestehendes Modul bekommt einen neuen Fehlerfall.
- Du siehst `throw new Error("…")`, `raise Exception("…")`, `panic`, `errors.New` mit späterem `err.Error() == "…"`, `catch (Exception e)` an einer Modulgrenze.
- Ein HTTP-/CLI-Layer muss Fehler auf Statuscodes/Exit-Codes abbilden.

## Designprinzipien

1. **Ein Basistyp pro Grenze**, darunter konkrete Fälle (Enum-Variante, Subklasse, Sentinel/Struct).
2. **Fälle nach Reaktion schneiden**, nicht nach Herkunft: `NotFound`, `Validation`, `Config`, `Unavailable` — nicht `PostgresError1`, `PostgresError2`.
3. **Strukturierte Felder**: `Config { key, reason }`, `NotFound { entity, id }` — der Aufrufer kann darauf matchen, der Logger kann sie als Felder ausgeben.
4. **Ursache erhalten**: fremde Fehler werden eingebettet, nicht zu `e.toString()` plattgedrückt.
5. **Message**: menschenlesbar, kurz, klein geschrieben, **ohne Secrets** (siehe `secret-handling`).
6. **`NotImplemented { feature }`** statt `todo!()`, `throw new Error("TODO")`, `raise NotImplementedError` in Produktionspfaden, die erreichbar sind.
7. **Kurzer Alias** für den Ergebnistyp, wo die Sprache das kennt (`type Result<T>` in Rust/TS).

## Falsch

```typescript
// TypeScript — String-Fehler, Aufrufer kann nur Messages parsen
async function loadConfig(path: string): Promise<Config> {
  const raw = await readFile(path, "utf8").catch(() => {
    throw new Error("config kaputt"); // Ursache weg, kein Typ, kein Key
  });
  return JSON.parse(raw);
}
```

```python
# Python — generische Exception, Ursache verloren
def load_config(path: str) -> Config:
    try:
        return Config(**json.loads(open(path).read()))
    except Exception:
        raise Exception("config kaputt")
```

```go
// Go — String-Vergleich auf Fehlermeldungen
if err != nil && err.Error() == "not found" { // bricht bei jeder Umformulierung
    return nil
}
```

## Richtig

### Rust (Hausstil: handgeschrieben, kein anyhow/thiserror)

```rust
use std::fmt;

pub enum ConfigError {
    Io(std::io::Error),
    Parse(serde_json::Error),
    Missing { key: String },
    NotImplemented { feature: String },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "config io: {e}"),
            Self::Parse(e) => write!(f, "config parse: {e}"),
            Self::Missing { key } => write!(f, "config key missing: {key}"),
            Self::NotImplemented { feature } => write!(f, "not implemented yet: {feature}"),
        }
    }
}
impl fmt::Debug for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Display::fmt(self, f) }
}
impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Parse(e) => Some(e),
            _ => None,
        }
    }
}
impl From<std::io::Error> for ConfigError {
    fn from(e: std::io::Error) -> Self { Self::Io(e) }
}
pub type Result<T> = std::result::Result<T, ConfigError>;
```

### TypeScript (Klassenhierarchie + `cause`, ES2022)

```typescript
export class ConfigError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = new.target.name; // korrekter Name auch in Subklassen
  }
}
export class ConfigMissingError extends ConfigError {
  constructor(readonly key: string) {
    super(`config key missing: ${key}`);
  }
}
export class ConfigParseError extends ConfigError {
  constructor(readonly path: string, cause: unknown) {
    super(`config parse failed: ${path}`, { cause });
  }
}

// Aufrufer reagiert typisiert:
try { await loadConfig(p); }
catch (e) {
  if (e instanceof ConfigMissingError) return defaults(e.key);
  throw e;
}
```

Alternative ohne Exceptions: diskriminierte Union als Ergebnistyp

```typescript
export type ConfigFailure =
  | { kind: "missing"; key: string }
  | { kind: "parse"; path: string; cause: unknown };
export type Result<T, E = ConfigFailure> = { ok: true; value: T } | { ok: false; error: E };
```

### Python (Basisklasse pro Modul, Felder als Attribute)

```python
class ConfigError(Exception):
    """Basis aller Fehler des config-Moduls."""

class ConfigMissingError(ConfigError):
    def __init__(self, key: str) -> None:
        super().__init__(f"config key missing: {key}")
        self.key = key

class ConfigParseError(ConfigError):
    def __init__(self, path: str) -> None:
        super().__init__(f"config parse failed: {path}")
        self.path = path

def load_config(path: str) -> Config:
    try:
        data = json.loads(Path(path).read_text())
    except json.JSONDecodeError as e:
        raise ConfigParseError(path) from e   # Ursache in __cause__
    ...
```

### Go (Sentinels + Struct-Fehler + `errors.Is/As`)

```go
var ErrNotFound = errors.New("not found") // Sentinel für "Art" des Fehlers

type ConfigError struct {
    Key    string
    Reason string
    Err    error // Ursache
}

func (e *ConfigError) Error() string { return fmt.Sprintf("invalid config %s: %s", e.Key, e.Reason) }
func (e *ConfigError) Unwrap() error { return e.Err }

// Aufrufer:
var ce *ConfigError
switch {
case errors.As(err, &ce):
    log.Printf("bad key %s", ce.Key)
case errors.Is(err, ErrNotFound):
    return defaults, nil
}
```

### Java / C# (kurz)

```java
public sealed class ConfigException extends Exception
        permits ConfigMissingException, ConfigParseException {
    protected ConfigException(String msg, Throwable cause) { super(msg, cause); }
}
public final class ConfigMissingException extends ConfigException {
    private final String key;
    public ConfigMissingException(String key) { super("config key missing: " + key, null); this.key = key; }
    public String key() { return key; }
}
```

```csharp
public class ConfigException(string message, Exception? inner = null) : Exception(message, inner);
public sealed class ConfigMissingException(string key) : ConfigException($"config key missing: {key}")
{
    public string Key { get; } = key;
}
```

## Fallstricke

- **Generischer Sammeltyp an der Library-Grenze** (`Error`, `Exception`, `anyhow::Error`, `interface{}`-Fehler): Aufrufer können nicht mehr unterscheiden. Hausregel Rust: `anyhow`/`thiserror` verboten.
- **Ursache zu String plattgedrückt** (`new Error(e.message)`, `Exception(str(e))`, `fmt.Errorf("%v", err)`): Stacktrace und Typ weg.
- **Zu feine Fälle**: ein Fall pro Aufrufstelle statt pro Reaktion — wird unwartbar.
- **Secrets in Messages** (Token, Connection-String mit Passwort).
- **Fehler für Kontrollfluss** im Happy Path (z. B. `NotFound` als normales Ergebnis von „existiert?“) → dort lieber `Option`/`null`/`bool` zurückgeben (siehe `null-and-optional-handling`).
- **Python `except Exception` fängt zu viel**; eigene Basisklasse erlaubt gezieltes `except ConfigError`.
- **TS `catch (e)` ist `unknown`** — vor Feldzugriff per `instanceof` einengen.

## Checkliste

1. Gibt es genau einen Fehler-Basistyp für diese Grenze?
2. Ist jeder Fall nach der **Reaktion des Aufrufers** geschnitten?
3. Tragen Fälle strukturierte Felder statt nur Text?
4. Wird die Ursache überall erhalten (`source`/`cause`/`from e`/`%w`+`Unwrap`/`InnerException`)?
5. Enthalten Messages keine Secrets?
6. Ist `NotImplemented` ein Fehlerfall statt Crash/TODO?
7. Existiert (wo sinnvoll) ein kurzer `Result`-Alias?
8. Rust zusätzlich: `rust-error-enum-design`, `rust-error-from-impls`, `rust-result-type-alias`.

## Quelle

- https://doc.rust-lang.org/book/ch09-00-error-handling.html
- https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Error/cause
- https://docs.python.org/3/tutorial/errors.html#user-defined-exceptions
- https://go.dev/blog/go1.13-errors
- https://docs.oracle.com/en/java/javase/21/language/sealed-classes-and-interfaces.html
