# Fremdfehler mit `.map_err` in eigene Varianten übersetzen

**Regel:** Verwende `.map_err(|e| Error::VariantName { message: e.to_string() })` um externe Fehlertypen an der Crate-Grenze in strukturierte eigene Varianten zu überführen. Ist ein `From<ForeignError>`-Impl vorhanden, kürze zu `.map_err(Error::from)` oder nutze direkt `?`.

**Warum:** `?` allein funktioniert nur wenn `From<ForeignError> for Error` implementiert ist. Viele externe Crates liefern Fehlertypen ohne `std::error::Error`-Impl oder mit inkompatiblen Lebenszeiten — dann muss die Konvertierung explizit an der Grenze stattfinden. Kontext (welche Operation scheiterte, an welchem Wert) geht sonst verloren.

## Falsch

```rust
// Fehler wird stillschweigend in einen String gedrückt — kein Typ, kein Kontext
fn seal(pk: &[u8], pt: &[u8]) -> Result<Vec<u8>> {
    let ciphertext = Encryptor::new()
        .seal(pk, pt)
        .unwrap(); // VERBOTEN in Produktion
    Ok(ciphertext)
}

// oder: anyhow wird importiert — VERBOTEN
use anyhow::Context;
fn seal(pk: &[u8], pt: &[u8]) -> anyhow::Result<Vec<u8>> { /* ... */ }
```

## Richtig

```rust
// Datei: sgh-secureHUB/crates/securehub-crypto/src/dispatch.rs:40-47
macro_rules! seal_arm {
    ($kem:ty, $aead:ty, $pk:expr, $pt:expr) => {
        Encryptor::<$kem, $aead>::new()
            .recipient($pk.to_vec())
            .plaintext($pt)
            .seal()
            .map_err(|e| Error::Crypto {
                message: e.to_string(),
            })
    };
}

// Wo From<ForeignError> implementiert ist — kurze Form:
use securehub_error::{Error, Result};

fn parse_json(raw: &str) -> Result<Config> {
    // serde_json::Error ist via From impl registriert → ? reicht
    let cfg: Config = serde_json::from_str(raw).map_err(Error::from)?;
    Ok(cfg)
}

// Kontext ergänzen wenn nötig:
fn read_key(path: &std::path::Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| Error::Io(e))
}
```

**Wann welche Form:**

| Situation | Pattern |
|---|---|
| `From<ForeignError>` impl vorhanden | `.map_err(Error::from)?` oder nur `?` |
| Kein `From` impl, strukturierte Variante | `.map_err(\|e\| Error::Variant { message: e.to_string() })?` |
| Zusätzlicher Kontext nötig | `.map_err(\|e\| Error::Variant { message: format!("beim Lesen von {path:?}: {e}") })?` |

**Hausregeln:**
- Verbotene Crates: `anyhow`, `thiserror`, `log`, `env_logger`, `failure`
- `unwrap()` / `expect()` / `panic!` / `todo!()` / `unimplemented!()` sind in Produktionscode verboten
- Fehler niemals stillschweigend verschlucken
- `.clone()` vermeiden; stattdessen spezifische Konvertierungsmethoden

## Quelle
- https://doc.rust-lang.org/book/ch09-02-recoverable-errors-with-result.html

Allgemeine Variante: `error-propagation`
