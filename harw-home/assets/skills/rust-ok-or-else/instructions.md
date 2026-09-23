# `Option` → `Result` mit `.ok_or_else`

**Regel:** Wandle ein `None`-Ergebnis immer mit `.ok_or_else(|| Error::VariantName { … })` in einen strukturierten Fehler um. Verwende niemals `.unwrap()`, `.expect()` oder `if let … { panic!(…) }` in Produktionscode.

**Warum:** `Option` drückt Abwesenheit aus, `Result` drückt Erfolg-oder-Fehler aus. An der Grenze zwischen beiden muss der Kontext (was fehlte, warum es erwartet war) explizit im Fehlerwert gespeichert werden. `.ok_or_else` nimmt eine Closure, die nur dann ausgeführt wird wenn tatsächlich `None` vorliegt — damit entstehen keine unnötigen Allokationen im Erfolgsfall.

## Falsch

```rust
// unwrap — VERBOTEN in Produktion
fn get_secret(config: &AppConfig) -> &str {
    config.auth_token_secret.as_deref().unwrap()
}

// expect — VERBOTEN in Produktion
fn get_secret(config: &AppConfig) -> &str {
    config.auth_token_secret.as_deref().expect("secret muss gesetzt sein")
}

// match mit panic — VERBOTEN
fn get_secret(config: &AppConfig) -> Result<&str> {
    match config.auth_token_secret.as_deref() {
        Some(s) => Ok(s),
        None => panic!("secret fehlt"),  // VERBOTEN
    }
}
```

## Richtig

```rust
// Datei: sgh-secureHUB/crates/securehub-api/src/auth.rs:209-221
use securehub_error::{Error, Result};

fn bearer_token(headers: &hyper::HeaderMap) -> Result<&str> {
    let value = headers
        .get(hyper::header::AUTHORIZATION)
        .ok_or_else(|| securehub_error::Error::Auth {
            reason: "Authorization header is missing".to_owned(),
        })?
        .to_str()
        .map_err(|_| securehub_error::Error::Auth {
            reason: "Authorization header contains non-ASCII characters".to_owned(),
        })?;

    value.strip_prefix("Bearer ").ok_or_else(|| securehub_error::Error::Auth {
        reason: "Authorization header does not use the Bearer scheme".to_owned(),
    })
}

// Weiteres Beispiel — Config-Pflichtfeld:
fn require_secret(config: &AppConfig) -> Result<&str> {
    config
        .auth_token_secret
        .as_deref()
        .ok_or_else(|| Error::Config {
            key: "SECUREHUB_AUTH_TOKEN_SECRET".to_owned(),
            reason: "muss für JWT-Verifikation gesetzt sein".to_owned(),
        })
}
```

**Faustregel `.ok_or` vs. `.ok_or_else`:**

| Variante | Wann | Beispiel |
|---|---|---|
| `.ok_or(Error::Static)` | Fehler ist ein `Copy`-Wert oder billiges Literal | `.ok_or(Error::NotFound)` |
| `.ok_or_else(\|\| Error::Dynamic { … })` | Fehlerfeld enthält String-Allokation | `.ok_or_else(\|\| Error::Config { key: k.to_owned(), … })` |

Bevorzuge `.ok_or_else` im Zweifel — korrekt, keine Allokation im Erfolgsfall.

**Hausregeln:**
- Verbotene Crates: `anyhow`, `thiserror`, `log`, `env_logger`, `failure`
- `unwrap()` / `expect()` / `panic!` / `todo!()` / `unimplemented!()` sind in Produktionscode verboten — stattdessen `Err(Error::NotImplemented { … })`
- Fehler niemals stillschweigend verschlucken
- `.clone()` vermeiden; stattdessen `.to_owned()`, `.to_string()` usw.

## Quelle
- https://doc.rust-lang.org/book/ch09-02-recoverable-errors-with-result.html
