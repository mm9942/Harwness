# Explicit Lifetime Annotations in Functions

**Regel:** Benenne eine Lifetime `'a` nur dann, wenn du dem Compiler mitteilen musst, dass eine Ausgabe-Referenz *von genau diesem Eingabe-Parameter* abhängt — nicht von einem anderen.

**Warum:** Lifetimes sind keine Laufzeit-Werte; sie sind Constraints, die dem Borrow-Checker mitteilen, wie lange eine Referenz gültig ist. Eine falsche Zuordnung erzeugt Use-after-free zur Kompilierzeit — nicht zur Laufzeit.

## Falsch — Compiler kann die Zuordnung nicht ableiten

```rust
// Zwei &str-Eingaben, eine &str-Ausgabe: welche Lifetime trägt die Ausgabe?
// Der Compiler verweigert die Kompilierung.
fn pick_longer(x: &str, y: &str) -> &str {
    if x.len() >= y.len() { x } else { y }
}
```

## Richtig — `'a` verbindet beide Eingaben mit der Ausgabe

```rust
// 'a = "so lange wie das kürzeste der beiden Inputs lebt".
// Kanonisches Lehrbuch-Beispiel (The Rust Programming Language, Kap. 10.3):
fn longest<'a>(x: &'a str, y: &'a str) -> &'a str {
    if x.len() >= y.len() { x } else { y }
}
```

## Richtig — Ausgabe hängt nur von *einem* Input ab

```rust
// 'a bindet batch an die Ausgabe; col_name ist nur zum Nachschlagen,
// beeinflusst die Ausgabe-Lifetime nicht.
// Real: apps/sgh-flow/src/iceberg/handler.rs:983
fn read_str_col<'a>(batch: &'a RecordBatch, col_name: &str, row: usize) -> Option<&'a str> {
    let col = batch.column_by_name(col_name)?;
    let arr = col.as_any().downcast_ref::<arrow_array::StringArray>()?;
    if arr.is_null(row) { None } else { Some(arr.value(row)) }
}
```

```rust
// 'a bindet payload an Vec<&'a JsonValue>; key ist nur ein Schlüssel-String.
// Real: apps/sgh-flow/src/db/repositories/invoice_read.rs:375
fn section_entries<'a>(payload: &'a JsonValue, key: &str) -> Option<Vec<&'a JsonValue>> {
    match payload.get(key) {
        Some(JsonValue::Array(items)) => Some(items.iter().collect()),
        Some(obj @ JsonValue::Object(_)) => Some(vec![obj]),
        _ => None,
    }
}
```

## Regel: wann welche Lifetime-Klammer

| Situation | Syntax |
|-----------|--------|
| Ausgabe kommt von genau einem von N Inputs | Nur den relevanten Input annotieren: `fn f<'a>(owner: &'a T, other: &str) -> &'a U` |
| Ausgabe kann von beiden Inputs kommen | Beide mit derselben Lifetime: `fn f<'a>(x: &'a str, y: &'a str) -> &'a str` |
| Kein Zusammenhang Eingabe ↔ Ausgabe nötig | Elision nutzen (kein `'a` schreiben) |

## Quelle
- <https://doc.rust-lang.org/book/ch10-03-lifetime-syntax.html> (Abschnitte "Generic Lifetimes in Functions" und "Lifetime Annotations in Function Signatures")
