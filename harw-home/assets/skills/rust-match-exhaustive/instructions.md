# Exhaustives `match` in Rust

**Regel:** Jeder `match`-Ausdruck muss alle möglichen Muster abdecken — fehlt ein Arm, verweigert der Compiler das Übersetzen. Das ist keine Warnung, sondern `error[E0004]: non-exhaustive patterns`.

**Warum:** Rust verschiebt die Korrektheitsprüfung von der Laufzeit in die Compilezeit. Ein vergessener Enum-Arm kann nie unbemerkt in Produktion gelangen — der Build schlägt fehl, bevor die Binärdatei entsteht. Bei Kreuzprodukten (KEM × AEAD, Status × Rolle, …) erzwingt der Compiler, dass jede Kombination explizit behandelt wird.

## Falsch

```rust
// Neues Enum-Variant MlKem2048 wurde hinzugefügt, aber der match nicht erweitert.
// → kompiliert NICHT: error[E0004]: non-exhaustive patterns: `(MlKem2048, _)` not covered
match (kem, aead) {
    (KemAlgorithm::MlKem512,  AeadAlgorithm::XChaCha20Poly1305) => { /* ... */ }
    (KemAlgorithm::MlKem512,  AeadAlgorithm::AesGcmSiv)         => { /* ... */ }
    (KemAlgorithm::MlKem768,  AeadAlgorithm::XChaCha20Poly1305) => { /* ... */ }
    (KemAlgorithm::MlKem768,  AeadAlgorithm::AesGcmSiv)         => { /* ... */ }
    // MlKem1024 fehlt → Compilefehler
    _ => unreachable!(), // ← Wildcard kaschiert fehlende Arme — verboten in Produktion
}
```

Der `_`-Wildcard-Arm ist gefährlich: Er kompiliert zwar, aber er verbirgt, dass eine Kombination nie implementiert wurde. `unreachable!()` und `todo!()` sind in Produktionspfaden verboten — verwende stattdessen einen konkreten Fehler oder brich mit `Err(Error::UnsupportedAlgorithm { ... })` ab.

## Richtig

```rust
// Datei: crates/securehub-crypto/src/dispatch.rs:102
// Jede (KemAlgorithm × AeadAlgorithm)-Kombination hat einen eigenen Arm.
// Wird KemAlgorithm ein neues Variant hinzugefügt, schlägt der Build hier fehl.
match (kem, aead) {
    (KemAlgorithm::MlKem512,  AeadAlgorithm::XChaCha20Poly1305) => {
        seal_arm!(MlKem512, XChaCha20Poly1305, recipient_public_key, plaintext)
    }
    (KemAlgorithm::MlKem512,  AeadAlgorithm::AesGcmSiv) => {
        seal_arm!(MlKem512, AesGcmSiv, recipient_public_key, plaintext)
    }
    (KemAlgorithm::MlKem768,  AeadAlgorithm::XChaCha20Poly1305) => {
        seal_arm!(MlKem768, XChaCha20Poly1305, recipient_public_key, plaintext)
    }
    (KemAlgorithm::MlKem768,  AeadAlgorithm::AesGcmSiv) => {
        seal_arm!(MlKem768, AesGcmSiv, recipient_public_key, plaintext)
    }
    (KemAlgorithm::MlKem1024, AeadAlgorithm::XChaCha20Poly1305) => {
        seal_arm!(MlKem1024, XChaCha20Poly1305, recipient_public_key, plaintext)
    }
    (KemAlgorithm::MlKem1024, AeadAlgorithm::AesGcmSiv) => {
        seal_arm!(MlKem1024, AesGcmSiv, recipient_public_key, plaintext)
    }
    // Kein _ — der Compiler sichert ab, dass jede neue Variante hier auftaucht.
}
```

### Wann ist ein Wildcard-Arm erlaubt?

Nur wenn du den Wert wirklich nicht brauchst und die Semantik „tu nichts / ignoriere Rest" korrekt ist:

```rust
match dice_roll {
    3 => add_fancy_hat(),
    7 => remove_fancy_hat(),
    _ => (), // explizit nichts tun — kein unreachable!, kein panic!
}
```

Niemals `_ => unreachable!()` oder `_ => panic!()` in Produktionspfaden. Wenn eine Kombination nicht unterstützt wird, gib einen Fehler zurück:

```rust
_ => Err(Error::UnsupportedAlgorithm {
    detail: format!("{kem:?} + {aead:?} ist nicht implementiert"),
}),
```

### Neue Enum-Variante hinzufügen — Checkliste

1. Variant in der Enum-Definition ergänzen.
2. `make check` ausführen → der Compiler zeigt jeden nicht abgedeckten `match`-Ausdruck.
3. Jeden gemeldeten `match` explizit erweitern — keinen `_`-Arm ergänzen.

## Quelle
- https://doc.rust-lang.org/book/ch06-02-match.html

Allgemeine Variante: `exhaustive-case-handling`
