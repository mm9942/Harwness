# Deref Coercion: Lass den Compiler die Deref-Kette traversieren

**Regel:** Wenn du `&smartpointer_value` an eine Funktion übergibst, die `&InnerType` erwartet, schreibe einfach `&value` — der Compiler traversiert die Deref-Kette automatisch zur Kompilierzeit, ohne Laufzeitkosten.

**Warum:** Rust wendet `Deref::deref` so oft wie nötig an, um den Typ des Arguments mit dem des Parameters in Einklang zu bringen. `Zeroizing<Vec<u8>>` implementiert `Deref<Target = Vec<u8>>`; `Vec<u8>` implementiert `Deref<Target = [u8]>`. Eine Referenz `&Zeroizing<Vec<u8>>` wird daher automatisch zu `&[u8]` — zwei Schritte, null Boilerplate.

## Falsch

```rust
// Unnötige explizite Konvertierungen — alle redundant wenn Deref-Koercion greift
let secret_key: Zeroizing<Vec<u8>> = self.unwrap_secret_key(&kv.encrypted_secret_key)?;

// Variante 1: .as_slice()
securehub_crypto::wrapper::open_envelope(
    self.kem(),
    self.aead(),
    secret_key.as_slice(),   // redundant
    envelope_bytes,
    org_id,
    keyset_id,
    &purpose,
)?;

// Variante 2: Slicing-Syntax
securehub_crypto::wrapper::open_envelope(
    self.kem(),
    self.aead(),
    &secret_key[..],         // auch redundant
    envelope_bytes,
    org_id,
    keyset_id,
    &purpose,
)?;
```

## Richtig

```rust
// service.rs:208 — unwrap_secret_key gibt Zeroizing<Vec<u8>> zurück
let secret_key = self.unwrap_secret_key(&kv.encrypted_secret_key)?;
//  ^^^^^^^^^^ Typ: Zeroizing<Vec<u8>>

// service.rs:615,882 — open_envelope erwartet &[u8] für den dritten Parameter.
// &secret_key: &Zeroizing<Vec<u8>>
//   → &Vec<u8>   (Zeroizing: Deref<Target = Vec<u8>>)
//   → &[u8]      (Vec:       Deref<Target = [u8]>)
// Zwei Schritte, zur Kompilierzeit, keine Allokation.
securehub_crypto::wrapper::open_envelope(
    self.kem(),
    self.aead(),
    &secret_key,             // ✓ direkte Referenz — Koercion übernimmt den Rest
    envelope_bytes,
    org_id,
    keyset_id,
    &purpose,
)?;
```

**Faustregel für Funktionsparameter:** Deklariere Eingaben als `&[T]`, `&str`, `&Path` — nicht als `&Vec<T>`, `&String`, `&PathBuf`. Dann profitieren alle Aufrufer automatisch von Deref-Koercion, egal ob sie `Vec`, `Box<[T]>`, `Zeroizing<Vec<T>>` oder ein anderes `Deref`-fähiges Wrapper-Typ halten.

## Quelle

- <https://doc.rust-lang.org/book/ch15-02-deref.html> (Kapitel 15.2 "Treating Smart Pointers Like Regular References with the Deref Trait")
