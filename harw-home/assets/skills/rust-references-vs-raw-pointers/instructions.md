# References vs. Raw Pointers — und warum `unsafe impl Send/Sync` fast nie nötig ist

**Regel:** Benutze `&T` / `&mut T` (borrow-checked, garantiert gültig). Schreibe `*const T` / `*mut T` nur für FFI oder Low-Level-Abstraktion — immer im kleinstmöglichen `unsafe`-Block, immer mit `// SAFETY:`-Kommentar. Schreibe `unsafe impl Send` / `unsafe impl Sync` nur dann, wenn der Compiler die Ableitung *nachweislich* ablehnt und du weißt warum.

**Warum:** Sichere Referenzen sind vom Borrow-Checker geprüft: kein Dangling, kein Data-Race, keine Null. Raw Pointers umgehen all das — der Compiler kann keine Garantien mehr geben. `Send` und `Sync` werden automatisch abgeleitet, wenn alle Felder des Typs selbst `Send`/`Sync` sind (`Vec<u8>`, `String`, `Arc<T>` usw.). Manuelle `unsafe impl` für solche Typen sind nicht nur überflüssig, sondern gefährlich: sie überschreiben die automatische Prüfung stillschweigend.

## Falsch

```rust
// MasterKek enthält nur Vec<u8> — der Compiler würde Send+Sync automatisch
// ableiten. Diese manuellen unsafe impl sind redundant UND riskant:
// sie würden auch dann noch kompilieren, wenn jemand ein !Send-Feld hinzufügt.
pub struct MasterKek {
    public_key: Vec<u8>,
    secret_key: Vec<u8>,
}

// NICHT SO — überflüssig, kein SAFETY-Kommentar, keine Begründung:
unsafe impl Send for MasterKek {}
unsafe impl Sync for MasterKek {}
```

## Richtig

```rust
// crates/securehub-service/src/master_kek.rs:70-76
// Vec<u8> ist Send + Sync → der Compiler leitet Send + Sync für MasterKek
// automatisch ab. Keine unsafe impl notwendig oder erlaubt.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct MasterKek {
    /// Raw ML-KEM-1024 public key bytes.
    public_key: Vec<u8>,
    /// Raw ML-KEM-1024 secret key bytes. Zeroized on drop.
    secret_key: Vec<u8>,
}
// Compiler gibt: MasterKek: Send + Sync — ohne eine Zeile unsafe.
```

Wenn `unsafe impl` wirklich nötig ist (z. B. weil ein Feld einen raw Pointer enthält), **muss** ein `SAFETY:`-Kommentar die Invariante benennen, die der Aufrufer garantiert:

```rust
// Nur als Beispiel — nicht aus dem Projekt:
pub struct RawBuf {
    ptr: *mut u8,  // raw pointer → !Send + !Sync automatisch
    len: usize,
}

// SAFETY: RawBuf besitzt den Speicher exklusiv. Der Pointer wird nie
// außerhalb des owning-Threads dereferenziert. Kein aliasing möglich,
// solange kein &mut RawBuf und kein Klon existiert.
unsafe impl Send for RawBuf {}
unsafe impl Sync for RawBuf {}
```

## Faustregeln

| Situation | Was tun |
|---|---|
| Alle Felder sind `Send`/`Sync` | nichts — Compiler leitet ab |
| Feld ist raw pointer oder `*mut T` | `unsafe impl` + SAFETY-Kommentar |
| Feld ist `Rc<T>` (bewusst nicht thread-safe) | kein `unsafe impl` — das wäre falsch |
| FFI-Struct mit extern-pointer | `unsafe impl` + SAFETY-Kommentar + Test |

## Quelle

- <https://doc.rust-lang.org/book/ch04-02-references-and-borrowing.html> — Kapitel 4: References and Borrowing (Borrow-Checker-Regeln, &T / &mut T)
- <https://doc.rust-lang.org/book/ch20-01-unsafe-rust.html> — Kapitel 20: Unsafe Rust (raw pointers, `unsafe impl Send/Sync`, SAFETY-Kommentar-Konvention)

Allgemeine Variante: `shared-state-and-concurrency`
