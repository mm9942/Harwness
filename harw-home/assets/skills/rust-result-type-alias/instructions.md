# Crate-weiter `Result<T>`-Alias

**Regel:** Definiere in jedem `error.rs` unmittelbar nach dem `Error`-Enum einen `pub type Result<T> = std::result::Result<T, Error>;`-Alias und importiere ihn an allen Call-Sites statt des vollständig qualifizierten Pfads.

**Warum:** Wiederholt auftretendes `std::result::Result<T, MeinFehler>` in Signaturen verschleiert die eigentliche Domain-Logik. Der Alias macht Signaturen knapp, vermeidet Import-Rauschen und zwingt alle Aufrufer, denselben Fehlertyp zu verwenden — kein versehentliches Mischen von Fehler-Enums aus verschiedenen Crates.

## Falsch

```rust
// error.rs
pub enum InvoiceError { /* ... */ }

// parser.rs — vollständiger Pfad überall wiederholt
use crate::error::InvoiceError;

fn parse_header(raw: &str) -> std::result::Result<Header, InvoiceError> { /* ... */ }
fn parse_positions(raw: &str) -> std::result::Result<Vec<Position>, InvoiceError> { /* ... */ }
```

## Richtig

```rust
// Datei: sgh-secureHUB/crates/securehub-error/src/error.rs:39
pub type Result<T> = std::result::Result<T, Error>;

// parser.rs — kurzer Import, lesbare Signaturen
use securehub_error::{Error, Result};

fn parse_header(raw: &str) -> Result<Header> { /* ... */ }
fn parse_positions(raw: &str) -> Result<Vec<Position>> { /* ... */ }

// Fehler erzeugen — kein Boilerplate
fn validate(value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(Error::Validation {
            field: "value".to_owned(),
            reason: "darf nicht leer sein".to_owned(),
        });
    }
    Ok(())
}
```

**Hausregeln:**
- Verbotene Crates: `anyhow`, `thiserror`, `log`, `env_logger`, `failure`
- `unwrap()` / `expect()` / `panic!` / `todo!()` / `unimplemented!()` sind in Produktionscode verboten — stattdessen `Err(Error::NotImplemented { … })`
- Fehler niemals stillschweigend verschlucken; immer propagieren oder explizit behandeln
- `.clone()` vermeiden; stattdessen `.to_owned()`, `.to_string()` usw.

## Quelle
- https://doc.rust-lang.org/book/ch09-02-recoverable-errors-with-result.html
