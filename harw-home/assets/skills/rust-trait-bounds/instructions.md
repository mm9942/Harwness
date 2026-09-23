# Rust Trait Bounds und `where`-Klauseln

**Regel:** Nutze `where`-Klauseln sobald ein Typ-Parameter mehr als einen Bound trägt oder die Signatur sonst unlesbar wird; kombiniere mehrere Bounds mit `+`.

**Warum:** Inline-Bounds (`<T: A + B + C>`) verstopfen den Funktionskopf und machen Signatur, Parameterliste und Rückgabetyp schwer lesbar. Eine `where`-Klausel trennt die Constraint-Deklaration sauber ab, ohne die Semantik zu ändern. Für Send + Sync ist diese Lesbarkeit besonders wichtig, da diese Bounds rein für Nebenläufigkeitssicherheit stehen und erkennbar bleiben müssen.

## Falsch

```rust
// Alle Bounds inline — beim dritten Parameter wird die Signatur unlesbar.
async fn forward_stream<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    reader: R,
    stream: &'static str,
    log_path: std::path::PathBuf,
) {
    // ...
}
```

## Richtig

```rust
use std::path::PathBuf;
use tokio::io::{AsyncRead, BufReader, AsyncBufReadExt};

/// Leitet den Ausgabestrom eines Kind-Prozesses zeilenweise weiter.
///
/// # Concurrency
/// `R` muss `Send + 'static` sein, da der Stream in einem Tokio-Task läuft.
// apps/sgh-flow/src/services/python_agent.rs:110-112
async fn forward_python_stream<R>(reader: R, stream: &'static str, log_path: PathBuf)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::debug!(target = "python_agent", %stream, %line);
    }
}
```

## Quelle
- https://doc.rust-lang.org/book/ch10-02-traits.html
