# Cow<'a, T> — Borrowed oder Owned ohne unnötige Allokation

**Regel:** Nutze `Cow<'a, B>` wenn eine Funktion im Normalfall borrows (kein Alloc), aber manchmal Ownership braucht (Mutation/Owned). Entscheide erst im `Owned`-Pfad zu allozieren.

**Warum:** Ein `-> String` zwingt zur Allokation selbst wenn der Input unverändert bleibt. `Cow::Borrowed` gibt eine Referenz zurück — null Allokation. `Cow::Owned` alloziert nur wenn nötig. Die Std-Docs: *"Provides clone-on-write semantics: the data is cloned only when mutation or ownership is required."*

## Falsch

```rust
// Alloziert immer — auch wenn kein ANSI vorhanden ist
fn strip_ansi(s: &str) -> String {
    if !s.contains('\x1b') {
        return s.to_owned();  // ❌ unnötige Allokation im Normalfall
    }
    // ... stripping logic ...
    String::new()
}
```

## Richtig

```rust
use std::borrow::Cow;

// Gibt Cow::Borrowed zurück wenn nichts zu tun — null Allokation.
// Gibt Cow::Owned zurück wenn ANSI entfernt werden musste.
//
// Realer Code: apps/acme-app/src/services/python_agent.rs:43
fn strip_ansi(s: &str) -> Cow<'_, str> {
    if !s.contains('\x1b') {
        return Cow::Borrowed(s);   // ✅ kein Alloc — nur Pointer
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() { break; }
            }
        } else {
            out.push(ch);
        }
    }
    Cow::Owned(out)   // ✅ alloziert nur wenn ANSI-Codes gefunden
}

// Aufruf: Cow<'_, str> implementiert Deref<Target=str> → direkt als &str nutzbar
let line = strip_ansi(&raw_log_line);
tracing::debug!(line = %line, "processed log");
```

## Weitere typische Muster

```rust
// Bytes: entweder PDF-embedded XML direkt oder konvertiert
// Realer Code: apps/acme-app/src/api/invoice_validate.rs:187
let xml_bytes: Cow<'_, [u8]> = if bytes.starts_with(b"%PDF") {
    Cow::Owned(extract_xml_from_pdf(bytes)?)   // ✅ alloziert nur für PDFs
} else {
    Cow::Borrowed(bytes)                        // ✅ kein Alloc für XML direkt
};
```

## Wann Cow verwenden?

| Situation | Mittel |
|---|---|
| Immer borrowen, nie mutieren | `&'a str` / `&'a [T]` |
| Immer ownen | `String` / `Vec<T>` |
| Manchmal borrow, manchmal own | `Cow<'a, str>` / `Cow<'a, [T]>` |
| Öffentliche API, Caller entscheidet | `impl Into<Cow<'a, str>>` |

## Quelle

- <https://doc.rust-lang.org/std/borrow/enum.Cow.html> — std::borrow::Cow (Enum, Clone-On-Write Semantics)

Allgemeine Variante: `function-signature-design`
