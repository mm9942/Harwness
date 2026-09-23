# Rust Generic Functions

**Regel:** Schreibe eine einzige generische Funktion mit Trait-Bound statt dieselbe Logik für `u32`, `i64`, `String` usw. zu wiederholen.

**Warum:** Der Compiler erzeugt per Monomorphisierung typ-spezifischen Code — null Laufzeit-Overhead, aber kein duplizierter Quellcode. Der Typ-Parameter `T` ist eine Leerstelle, die der Aufrufer füllt; der Bound `T: Trait` stellt sicher, dass nur Typen mit der benötigten Fähigkeit akzeptiert werden.

## Falsch

```rust
// Drei nahezu identische Funktionen, jede Änderung muss dreifach gepflegt werden.
fn serialize_u32(status: u16, value: &u32) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

fn serialize_string(status: u16, value: &String) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

fn serialize_invoice(status: u16, value: &Invoice) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}
```

## Richtig

```rust
use hyper::StatusCode;
use serde::Serialize;

/// Erstellt eine HTTP-Antwort mit JSON-Körper für jeden serialisierbaren Typ.
///
/// # Errors
/// Gibt `HandlerResult` mit Status 500 zurück, wenn die Serialisierung fehlschlägt.
// apps/sgh-flow/src/api/admin_vector_store.rs:254
fn dto_response<T: Serialize>(status: StatusCode, value: &T) -> HandlerResult {
    match serde_json::to_value(value) {
        Ok(v) => json_response(status, &v),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("response serialisation failed: {e}"),
        ),
    }
}
```

## Quelle
- https://doc.rust-lang.org/book/ch10-01-syntax.html
