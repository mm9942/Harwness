# .to_owned() / .to_string() / .to_vec() statt .clone()

**Regel:** Verwende spezifische Konversionsmethoden (`to_owned()`, `to_string()`, `to_vec()`, `to_path_buf()`) um von borrowed zu owned zu konvertieren. Verwende `Arc::clone(&arc)` um einen Arc-Zeiger zu kopieren. Niemals `.clone()` wo eine semantisch präzisere Methode existiert.

**Warum:** `.clone()` ist generisch und klärt nicht, was kopiert wird. `"foo".to_owned()` drückt aus: "ich konvertiere einen &str zu einem neuen String". `Arc::clone(&x)` macht explizit, dass nur der Zeiger (nicht der Inhalt) dupliziert wird — atomarer Refcount-Increment, keine Heap-Allokation. Das Buch (Kapitel 4.3) empfiehlt `&str` als Funktionsparameter genau um `.clone()` beim Aufrufer zu vermeiden.

## Falsch
```rust
// String-Felder in HashMap eintragen via .clone() — verdeckt, was passiert
// (Muster aus apps/sgh-flow/src/iceberg/config.rs:258-265)
fn to_catalog_props(&self) -> HashMap<String, String> {
    let mut props = HashMap::new();
    props.insert("credential".to_string(), self.credential.clone());     // ❌
    props.insert("s3.endpoint".to_string(), self.s3_endpoint.clone());   // ❌
    props.insert("s3.access-key-id".to_string(), self.s3_access_key.clone()); // ❌
    props
}

// Arc "klonen" via .clone() — verschleiert, dass nur der Zeiger dupliziert wird
let repo2 = audit_repo.clone();  // ❌ — liest sich wie Inhalt-Klon
```

## Richtig
```rust
// Slice-Element zu owned String: .to_owned() auf &str
// (aus apps/sgh-flow/src/iceberg/handler.rs:484)
let table_name = parts[parts.len() - 1].to_owned(); // &str → String ✅

// Error-Variante mit owned String bauen: .to_owned() auf &str-Literal
IcebergError::TableNotFound(table_ident_str.to_owned())  // ✅

// &str → String für HashMap-Schlüssel:
props.insert("credential".to_owned(), self.credential.clone());
//           ^^^^^^^^^^ .to_owned() auf &str-Literal ✅
// Anmerkung: self.credential ist bereits ein String; hier ist .clone() noch nötig
// — aber sobald ownership übertragen werden kann, besser std::mem::take oder into()

// Konversionstabelle
let s: &str        = "hello";
let owned: String  = s.to_owned();      // &str  → String ✅

let sl: &[i32]     = &[1, 2, 3];
let v: Vec<i32>    = sl.to_vec();       // &[T]  → Vec<T> ✅

let p: &std::path::Path   = std::path::Path::new("/tmp");
let pb: std::path::PathBuf = p.to_path_buf(); // &Path → PathBuf ✅

// Arc-Zeiger duplizieren: Arc::clone(&x) — kein Heap-Klon, nur Refcount++
// (Muster aus apps/sgh-flow/src/app.rs:359, :366, :367, …)
let handler_ref = Arc::clone(&audit_repo);  // ✅ explizit: Zeiger, nicht Inhalt
let handler_ref2 = Arc::clone(&users_repo); // ✅

// ToOwned-Trait: allgemeines Muster (aus std)
let v: &[i32] = &[1, 2];
let vv: Vec<i32> = v.to_owned(); // via ToOwned blanket impl ✅
```

## Quelle
- https://doc.rust-lang.org/book/ch04-03-slices.html (The Slice Type, Kapitel 4.3 — &str vs String)
- https://doc.rust-lang.org/std/borrow/trait.ToOwned.html (ToOwned Trait — to_owned() Implementierungen)
