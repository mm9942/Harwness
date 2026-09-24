# Der `?`-Operator zur Fehlerweiterleitung

**Regel:** `?` hinter einem `Result<T, E>`-Ausdruck ist die kanonische Kurzform für "bei `Ok(v)` weitermachen mit `v`, bei `Err(e)` sofort `return Err(From::from(e))`". Der Operator funktioniert nur in Funktionen, die `Result` (oder `Option`) zurückgeben.

**Warum:** `match`-Boilerplate an jeder Fehlerstelle verbirgt die eigentliche Logik. `?` macht Fehlerpfade explizit sichtbar ohne die Lesbarkeit zu belasten. Der automatische `From`-Aufruf bedeutet: wer `impl From<ForeignError> for MyError` schreibt, braucht kein `.map_err()`.

## Falsch

```rust
// Ausführlicher match — verdeckt die Absicht
fn read_config(path: &str) -> Result<Config, AppError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s)  => s,
        Err(e) => return Err(AppError::Io(e)),  // manuell, fehleranfällig
    };
    let cfg = match serde_json::from_str::<Config>(&raw) {
        Ok(c)  => c,
        Err(e) => return Err(AppError::Serde(e)),
    };
    Ok(cfg)
}
```

## Richtig

```rust
// Datei: apps/acme-app/src/runtime.rs:98–106
// bootstrap() und execute_command() geben beide Result<_, AppError> zurück.
// ? propagiert AppError automatisch nach oben — From-Impls erledigen die Konversion.

pub async fn run_cli(cli: CliArgs) -> Result<(), AppError> {
    bootstrap(&cli)?;                            // io::Error → AppError::Io via From
    if cli.command.is_some() {
        cli_command::execute_command(&cli).await?;  // serde/sqlx/… → AppError via From
        return Ok(());
    }
    serve(&cli).await
}

// Das kurze Äquivalent zum "Falsch"-Beispiel oben:
fn read_config(path: &str) -> Result<Config, AppError> {
    let raw = std::fs::read_to_string(path)?;       // io::Error → AppError::Io (From)
    let cfg: Config = serde_json::from_str(&raw)?;  // serde_json::Error → AppError::Serde (From)
    Ok(cfg)
}
```

## Wie `?` intern funktioniert

```
Ausdruck?
    ↓
match Ausdruck {
    Ok(val)  => val,
    Err(e)   => return Err(From::from(e)),
              //          ^^^^^^^^^^^
              // ruft impl From<E> for ReturnErrorType auf
}
```

Das heißt: `?` funktioniert **nur**, wenn `impl From<E> for ReturnErrorType` existiert. Fehlt das `From`-Impl, gibt der Compiler E0277 ("the trait `From<X>` is not implemented for `Y`").

## Häufige Fallstricke

### 1. `?` in einer Funktion, die `()` zurückgibt

```rust
// Falsch — main gibt () zurück, ? ist nicht erlaubt
fn main() {
    let f = std::fs::read_to_string("foo.txt")?;  // Compile-Fehler E0277
}

// Richtig — Rückgabetyp auf Result setzen
fn main() -> Result<(), AppError> {
    let f = std::fs::read_to_string("foo.txt")?;
    Ok(())
}
```

### 2. `?` über Fehlertypgrenzen ohne From-Impl

```rust
// Falsch — keine From-Konversion zwischen IcebergError und io::Error
fn foo() -> Result<(), IcebergError> {
    std::fs::read("x.bin")?;  // io::Error, aber kein From<io::Error> for IcebergError
    Ok(())
}

// Richtig — From-Impl ergänzen (siehe Skill rust-error-from-impls)
impl From<std::io::Error> for IcebergError {
    fn from(e: std::io::Error) -> Self { IcebergError::Config(e.to_string()) }
}
```

### 3. `?` auf `Option` statt `Result`

```rust
// Option<T>? gibt None zurück — nur erlaubt in fn die Option zurückgeben
fn first_line(text: &str) -> Option<&str> {
    text.lines().next()  // kein ? nötig — schon Option
}

// Option → Result: .ok_or_else() verwenden (siehe Skill rust-ok-or-else)
fn first_line_or_err(text: &str) -> Result<&str, AppError> {
    text.lines().next().ok_or_else(|| AppError::DataFormat("leere Eingabe".to_owned()))
}
```

## Methodenketten mit `?`

```rust
// Datei: apps/acme-app/src/iceberg/error.rs (Verwendungsmuster)
// Mehrere ?-Operatoren in einer Zeile — funktioniert, weil From-Impls vorhanden:
fn parse_and_store(raw: &str) -> Result<(), IcebergError> {
    let val: serde_json::Value = serde_json::from_str(raw)?;  // serde_json::Error → IcebergError::Json
    // … weitere Operationen mit ? …
    Ok(())
}
```

## Hausregeln

- Niemals Fehler mit `let _ = foo()?` oder `foo().ok()` stillschweigend verwerfen.
- `unwrap()` / `expect()` verboten in Produktionspfaden.
- Verbotene Crates: `anyhow`, `thiserror`, `log`, `env_logger`, `failure`.

## Quelle

- https://doc.rust-lang.org/book/ch09-02-recoverable-errors-with-result.html

Allgemeine Variante: `error-propagation`
