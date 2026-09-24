# Secrets sicher aus dem Speicher löschen mit `zeroize`

**Regel:** Jedes Struct, das Schlüsselmaterial oder Passwörter hält, trägt `#[derive(Zeroize, ZeroizeOnDrop)]`. Transiente Puffer (z. B. entschlüsselter Geheimschlüssel für eine Operation) werden in `Zeroizing<Vec<u8>>` gewickelt. Niemals rohe `Vec<u8>` für Secrets zurückgeben, die der Aufrufer danach liegen lässt.

**Warum:** Der Compiler darf `ptr::write(buf, 0)` optimieren, wenn er erkennt, dass der Wert danach nicht mehr gelesen wird. `zeroize` nutzt `core::ptr::write_volatile` + Speicher-Fence, die der Optimierer nicht eliminieren darf. Ohne das leben Schlüsselbytes nach dem Drop in befreitem Heap, bis der Allocator den Bereich überschreibt — unter Umständen Minuten oder länger.

## Falsch

```rust
// Transient unwrapped secret key — wird vom Aufrufer nach Benutzung gedroppt,
// bleibt aber als Klartext im Heap bis zur nächsten Allokation:
fn unwrap_secret_key(&self, enc: &[u8]) -> Result<Vec<u8>> {
    let secret_key = crypto::open_for_recipient(self.master_kek.secret_key(), enc)?;
    Ok(secret_key)   // keine Zeroize-Garantie beim Drop
}

// Owned struct ohne ZeroizeOnDrop — sk bleibt lesbar im Heap:
pub struct MasterKek {
    public_key: Vec<u8>,
    secret_key: Vec<u8>,  // ← kein automatisches Überschreiben
}
```

## Richtig

```rust
// crates/securehub-service/src/master_kek.rs:41,70-76
// Owned struct: beide Felder werden beim Drop überschrieben.
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct MasterKek {
    /// Raw ML-KEM-1024 public key bytes.
    public_key: Vec<u8>,
    /// Raw ML-KEM-1024 secret key bytes. Zeroized on drop.
    secret_key: Vec<u8>,
}

// crates/securehub-service/src/service.rs:69,208-218
// Transienter Puffer: Zeroizing<Vec<u8>> überschreibt beim Drop automatisch.
use zeroize::Zeroizing;

fn unwrap_secret_key(&self, encrypted_secret_key: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    // Wrap in `Zeroizing` so the unwrapped per-keyset ML-KEM secret key is
    // scrubbed from the heap when the caller drops it, rather than lingering
    // in freed memory.
    let secret_key = securehub_crypto::open_for_recipient(
        MASTER_KEK_KEM,
        MASTER_KEK_AEAD,
        self.master_kek.secret_key(),
        encrypted_secret_key,
    )?;
    Ok(Zeroizing::new(secret_key))
}
```

## Entscheidungsbaum

| Situation | Mittel |
|---|---|
| Struct hält Secrets als Felder (dauerhaft) | `#[derive(Zeroize, ZeroizeOnDrop)]` auf dem Struct |
| Transient buffer (nur für eine Operation) | `Zeroizing::new(vec)` — zeroize on drop |
| Bekannter Puffer, manuell löschen | `buf.zeroize()` direkt aufrufen |
| Kein Secret, normaler Wert | nichts — kein Overhead |

## Caveats (aus der Crate-Doku)

- `Vec` zeroize-t die aktuelle Kapazität; falls der Vec vorher realloziert hat, können frühere Kopien im Heap noch existieren. Für hochkritische Keys: direkt mit fixer Array-Größe arbeiten (`[u8; N]`).
- Kein Schutz gegen Microarchitektur-Angriffe (Spectre/Meltdown).
- Register-Clearing ist außerhalb des Scope — erfordert Inline-ASM oder Compiler-Support.

## Quelle

- <https://docs.rs/zeroize/latest/zeroize/> — `Zeroize`, `ZeroizeOnDrop`, `Zeroizing<T>` API + Sicherheitsgarantien
- <https://doc.rust-lang.org/book/ch15-03-drop.html> — Kapitel 15: Drop trait (wann Rust Destruktoren aufruft; Basis für ZeroizeOnDrop)

Allgemeine Variante: `secret-handling`
