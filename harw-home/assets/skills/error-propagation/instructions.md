# Fehler weiterreichen, übersetzen, mit Kontext anreichern — sprachunabhängig

**Regel:** Ein Fehler wird **sofort** zurückgegeben/weitergeworfen (Early Return), **genau einmal pro Schicht** mit dem Kontext angereichert, den nur diese Schicht kennt (welche Datei, welche ID, welche Operation), an der **Modulgrenze in den eigenen Fehlertyp übersetzt** — und die Ursache bleibt dabei erhalten. Ein Fehler wird **niemals** stillschweigend verschluckt.

**Warum:** Ohne Kontext liest man in Produktion nur `no such file or directory` — aber welche Datei, bei welchem Request? Ohne erhaltene Ursache fehlt der Stacktrace der eigentlichen Stelle. Verschluckte Fehler machen aus einem klaren Fehlschlag einen späteren, rätselhaften Folgefehler.

## Wann anwenden

- Du rufst eine Funktion auf, die fehlschlagen kann, und musst entscheiden: weiterreichen, übersetzen oder behandeln.
- Du siehst `except: pass`, leeres `catch {}`, `_ = f()` in Go, `.ok()`/`let _ =` in Rust, `.catch(() => {})`.
- Du siehst `throw new Error(e.message)`, `raise X` ohne `from`, `fmt.Errorf("...: %v", err)`.
- Fehler wird geloggt **und** weitergeworfen (doppelte Logs).
- Rust: `?` scheitert mit E0277, oder überall steht manuelles `match` / `.map_err`.

## Die drei Entscheidungen

| Situation | Tun |
|---|---|
| Diese Schicht kann nichts beitragen | unverändert weiterreichen (`?`, `return err`, kein try/catch) |
| Diese Schicht kennt nützlichen Kontext | **einmal** wrappen: Kontext + Ursache |
| Fremder Fehler überquert die Modulgrenze | in eigenen Fehlertyp übersetzen (Ursache als `cause`) |
| Diese Schicht kann den Fehler **wirklich** behandeln (Fallback, Retry, Default) | behandeln — und begründen/loggen, was passiert |

Geloggt wird **an genau einer Stelle** — typischerweise ganz oben (Request-Handler, `main`), wo entschieden wird, was mit dem Fehler passiert.

## Falsch

```python
# Python: verschluckt, dann Folgefehler an ganz anderer Stelle
try:
    cfg = load_config(path)
except Exception:
    pass
cfg.timeout  # NameError / AttributeError — die eigentliche Ursache ist weg
```

```typescript
// TypeScript: Ursache und Stacktrace verworfen
try { await db.insert(row); }
catch (e) { throw new Error("insert failed"); }
```

```go
// Go: kein Kontext, und %v zerstört die Kette für errors.Is/As
data, err := os.ReadFile(path)
if err != nil {
    return fmt.Errorf("failed: %v", err)
}
```

```rust
// Rust: manuelles match an jeder Stelle bzw. unwrap
let raw = match std::fs::read_to_string(path) { Ok(s) => s, Err(e) => return Err(AppError::Io(e)) };
let cfg: Config = serde_json::from_str(&raw).unwrap();
```

## Richtig

### Rust — `?` + `From`, `.map_err` für Kontext

```rust
fn read_config(path: &Path) -> Result<Config, AppError> {
    let raw = std::fs::read_to_string(path).map_err(|e| AppError::ConfigRead {
        path: path.to_path_buf(),
        source: e,                         // Ursache erhalten
    })?;
    let cfg: Config = serde_json::from_str(&raw)?; // From<serde_json::Error> for AppError
    Ok(cfg)
}
```

### TypeScript — `cause` (ES2022)

```typescript
async function readConfig(path: string): Promise<Config> {
  let raw: string;
  try {
    raw = await readFile(path, "utf8");
  } catch (e) {
    throw new ConfigReadError(`reading config ${path}`, { cause: e });
  }
  return parseConfig(raw); // wirft selbst typisiert — kein weiteres Wrappen
}
```

### Python — `raise … from e`

```python
def read_config(path: Path) -> Config:
    try:
        raw = path.read_text()
    except OSError as e:
        raise ConfigReadError(f"reading config {path}") from e
    return parse_config(raw)
```

`from None` nur, wenn die Ursache **bewusst** irrelevant ist (z. B. KeyError → eigener NotFound mit allen Infos).

### Go — `%w` + Kontext pro Schicht

```go
func readConfig(path string) (*Config, error) {
    data, err := os.ReadFile(path)
    if err != nil {
        return nil, fmt.Errorf("read config %s: %w", path, err) // %w erhält die Kette
    }
    cfg, err := parseConfig(data)
    if err != nil {
        return nil, fmt.Errorf("parse config %s: %w", path, err)
    }
    return cfg, nil
}
// ganz oben: errors.Is(err, fs.ErrNotExist) funktioniert weiterhin
```

### Java — Ursache im Konstruktor

```java
try {
    return Files.readString(path);
} catch (IOException e) {
    throw new ConfigReadException("reading config " + path, e);
}
```

## Fallstricke

- **Log-and-rethrow**: jede Schicht loggt → derselbe Fehler 5× im Log. Nur oben loggen.
- **Kontext-Stottern**: `read config: read config: open: …` — jede Schicht fügt nur **ihren** Kontext hinzu.
- **Go `%v` statt `%w`**: `errors.Is/As` sieht die Ursache nicht mehr.
- **Python `raise X` im `except` ohne `from`**: implizite Kette („During handling … another exception occurred“) sieht wie ein Bug im Handler aus — explizit `from e`.
- **TS: nicht ge-`await`-ete Promise** → Fehler landet als unhandled rejection statt im `catch`.
- **Zu breites Fangen**: `catch (Exception)` / `except Exception` um einen großen Block fängt auch Programmierfehler. So eng wie möglich fangen.
- **Fallback ohne Spur**: Default statt Fehler ist ok — aber mit Log/Metric, sonst merkt niemand, dass der Fallback dauerhaft greift.
- **Rust**: `?` braucht `From<E>` für den Rückgabefehler; in Funktionen ohne `Result`-Rückgabe nicht erlaubt.

## Checkliste

1. Gibt es irgendwo einen leeren catch / `pass` / `_ =` / `.ok()` ohne Begründung? → entfernen oder begründen.
2. Wird bei jedem Wrappen die Ursache erhalten (`cause`, `from e`, `%w`, Konstruktor-Cause, `source`)?
3. Fügt jede Schicht nur **ihren** Kontext hinzu?
4. Werden fremde Fehler an der Modulgrenze in den eigenen Typ übersetzt (siehe `error-type-design`)?
5. Wird genau einmal geloggt?
6. Ist der try-Block so klein wie möglich?
7. Rust zusätzlich: `rust-question-mark-operator`, `rust-map-err`, `rust-error-from-impls`.

## Quelle

- https://doc.rust-lang.org/book/ch09-02-recoverable-errors-with-result.html
- https://go.dev/blog/go1.13-errors
- https://docs.python.org/3/reference/simple_stmts.html#the-raise-statement
- https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Error/cause
