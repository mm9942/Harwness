# `impl From<ForeignError> for MyError`

**Regel:** Für jeden fremden Fehlertyp, den ein Modul weiterleitet, wird ein `impl From<ForeignError> for MyError` geschrieben. Das macht `?` an jeder Fehlerstelle automatisch konvertierend — kein `.map_err(MyError::from)` nötig.

**Warum:** `?` ruft intern `From::from(e)` auf. Ohne passendes `From`-Impl muss jede Fehlerstelle manuell konvertiert werden — das führt zu Boilerplate und Auslassungen. Mit `From` ist die Konversion deklarativ in `error.rs` zentralisiert.

## Falsch

```rust
// Manuelles .map_err() an jeder Fehlerstelle — fehleranfällig, laut
fn load(path: &str) -> Result<Config, AppError> {
    let raw = std::fs::read_to_string(path)
        .map_err(AppError::Io)?;               // ← Boilerplate
    let cfg: Config = serde_json::from_str(&raw)
        .map_err(|e| AppError::Serde(e))?;    // ← nochmal Boilerplate
    Ok(cfg)
}
```

## Richtig

```rust
// Datei: apps/sgh-flow/src/error.rs:389–437
// Einmalig in error.rs definieren:

impl From<io::Error> for AppError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for AppError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value)
    }
}

impl From<sqlx::Error> for AppError {
    fn from(value: sqlx::Error) -> Self {
        Self::Sqlx(value)
    }
}

impl From<reqwest::Error> for AppError {
    fn from(value: reqwest::Error) -> Self {
        Self::Http(value)
    }
}

// Jetzt an der Aufrufstelle — kein .map_err() mehr:
async fn load_and_store(path: &str, pool: &sqlx::PgPool) -> Result<(), AppError> {
    let raw = std::fs::read_to_string(path)?;         // io::Error → AppError::Io
    let cfg: Config = serde_json::from_str(&raw)?;    // serde_json::Error → AppError::Serde
    sqlx::query!("INSERT INTO cfg …").execute(pool).await?;  // sqlx::Error → AppError::Sqlx
    Ok(())
}
```

Verschachtelte Fehlertypen (`IcebergError` → `AppError`) folgen demselben Muster:

```rust
// Datei: apps/sgh-flow/src/error.rs:461
impl From<IcebergError> for AppError {
    fn from(value: IcebergError) -> Self {
        Self::Iceberg(value)
    }
}

// Datei: apps/sgh-flow/src/iceberg/error.rs:86–100
impl From<iceberg::Error> for IcebergError {
    fn from(e: iceberg::Error) -> Self { IcebergError::Catalog(e) }
}

impl From<arrow_schema::ArrowError> for IcebergError {
    fn from(e: arrow_schema::ArrowError) -> Self { IcebergError::Arrow(e) }
}

impl From<serde_json::Error> for IcebergError {
    fn from(e: serde_json::Error) -> Self { IcebergError::Json(e) }
}
```

Sonderfall: wenn ein fremder Fehler keine sinnvolle Variante hat (z.B. ein Channel-Send-Fehler, der immer bedeutet "Empfänger tot"), kann `From` den Kontext verwerfen:

```rust
// Datei: apps/sgh-flow/src/error.rs:407–410
impl From<broadcast::error::SendError<crate::events::InvoiceEvent>> for AppError {
    fn from(_: broadcast::error::SendError<crate::events::InvoiceEvent>) -> Self {
        Self::BroadcastClosed  // Kontext unwichtig — Variante trägt genug Bedeutung
    }
}
```

## Checkliste

1. Jede neue Fehlerquelle in `error.rs` als Variante modellieren (`Io(io::Error)`, `Json(serde_json::Error)`, …).
2. `impl From<ForeignError> for MyError` direkt darunter schreiben.
3. `impl std::error::Error for MyError` → `source()` für Wrap-Varianten befüllen (Kausal-Kette erhalten).
4. Typ-Alias `pub type Result<T> = std::result::Result<T, MyError>` anlegen.
5. Alle Aufrufstellen nutzen nur noch `?` — kein `.unwrap()`, kein `.map_err()`.

## Hausregeln

- `unwrap()` / `expect()` verboten in Produktionspfaden.
- Fehler nie stillschweigend verwerfen (kein `let _ = foo()`).
- `.clone()` vermeiden; `.to_owned()` / `.to_string()` verwenden.
- Verbotene Crates: `anyhow`, `thiserror`, `log`, `env_logger`, `failure`.

## Quelle

- https://doc.rust-lang.org/book/ch09-02-recoverable-errors-with-result.html
