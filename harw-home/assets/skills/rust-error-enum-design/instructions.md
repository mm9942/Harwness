# Handgeschriebener Error-Enum in Rust

**Regel:** Jedes Modul, das fehlschlagen kann, definiert einen eigenen `pub enum FooError` mit `Display`, `Debug` (delegiert an `Display`) und `impl std::error::Error` (mit `source()`). Kein `anyhow`, kein `thiserror`, kein `unwrap`/`expect`/`panic!` in Produktionspfaden.

**Warum:** `anyhow` versteckt Varianten hinter einem opaken Trait-Objekt — keine strukturierten `match`-Zweige möglich. `thiserror` generiert Code unsichtbar, der bei API-Änderungen der Bibliothek bricht. Der handgeschriebene Ansatz ist explizit, wartbar und entspricht dem Projektkodex (`CLAUDE.md`).

## Falsch

```rust
// anyhow verwischt den Fehlertyp — kein strukturiertes matching möglich
use anyhow::{anyhow, Result};

fn parse_config(path: &str) -> Result<Config> {
    let data = std::fs::read_to_string(path)?;  // anyhow::Error
    serde_json::from_str(&data).map_err(|e| anyhow!("json kaputt: {e}"))
}

// Oder: thiserror-Makro — generiert Display/From unsichtbar
#[derive(thiserror::Error, Debug)]
enum AppError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}
```

## Richtig

```rust
// Datei: apps/acme-app/src/iceberg/error.rs:35–84
use std::fmt;

pub enum IcebergError {
    /// Fehlende oder ungültige Konfiguration.
    Config(String),
    /// Iceberg-Katalog hat einen Fehler zurückgegeben.
    Catalog(iceberg::Error),
    /// Arrow-Schema- oder Array-Fehler.
    Arrow(arrow_schema::ArrowError),
    /// Tabelle nicht im Katalog gefunden.
    TableNotFound(String),
    /// JSON-Serialisierungs-/Deserialisierungsfehler.
    Json(serde_json::Error),
}

// Display: menschenlesbar, kein internes Jargon
impl fmt::Display for IcebergError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IcebergError::Config(msg)          => write!(f, "iceberg config: {msg}"),
            IcebergError::Catalog(e)           => write!(f, "iceberg catalog: {e}"),
            IcebergError::Arrow(e)             => write!(f, "iceberg arrow: {e}"),
            IcebergError::TableNotFound(table) => write!(f, "iceberg table not found: {table}"),
            IcebergError::Json(e)              => write!(f, "iceberg json: {e}"),
        }
    }
}

// Debug delegiert an Display — eine einzige Formatierungslogik
impl fmt::Debug for IcebergError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

// std::error::Error: source() liefert die eingebettete Ursache
impl std::error::Error for IcebergError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IcebergError::Catalog(e) => Some(e),
            IcebergError::Arrow(e)   => Some(e),
            IcebergError::Json(e)    => Some(e),
            _                        => None,
        }
    }
}

// Typ-Alias erspart ständiges Wiederholen des Fehlertyps
pub type Result<T> = std::result::Result<T, IcebergError>;
```

Ein weiteres Beispiel aus dem securehub-Crate zeigt denselben Aufbau mit `NotImplemented`-Variante (kein `todo!()`):

```rust
// Datei: crates/securehub-error/src/error.rs:61–134
pub enum Error {
    Io(std::io::Error),
    Config    { key: String, reason: String },
    Auth      { reason: String },
    Validation{ field: String, reason: String },
    NotFound  { entity: String, id: String },
    /// Statt todo!()/unimplemented!() → explizite Variante zurückgeben
    NotImplemented { feature: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e)                        => write!(f, "I/O error: {e}"),
            Self::Config   { key, reason }     => write!(f, "invalid config {key}: {reason}"),
            Self::NotFound { entity, id }      => write!(f, "{entity} not found: {id}"),
            Self::NotImplemented { feature }   => write!(f, "not implemented yet: {feature}"),
            // … alle weiteren Varianten …
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)  // Debug == Display
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _           => None,
        }
    }
}
```

## Hausregeln (immer einhalten)

- `unwrap()` / `expect()` verboten in Produktionspfaden (nur in Tests und vor dem Logger in `main`).
- `todo!()` / `unimplemented!()` verboten — stattdessen `Err(Error::NotImplemented { feature: "…".to_owned() })` zurückgeben.
- `panic!` verboten in Library-Code.
- `.clone()` vermeiden; stattdessen `.to_owned()` / `.to_string()` für String-Konvertierungen.
- Verbotene Crates: `anyhow`, `thiserror`, `log`, `env_logger`, `failure`.

## Quelle

- https://doc.rust-lang.org/book/ch09-00-error-handling.html

Allgemeine Variante: `error-type-design`
