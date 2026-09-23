# Lifetime Annotations in Structs and Cow<'a, T>

**Regel:** Jede Struct, die ein Referenz-Feld hält, braucht einen Lifetime-Parameter; die Struct-Instanz darf nicht länger leben als die Daten, auf die das Feld zeigt. Wenn eine Funktion manchmal borgt und manchmal alloziert, verwende `Cow<'a, T>` statt zwei Überladungen.

**Warum:** Ohne Lifetime-Parameter kann der Compiler die Gültigkeit der internen Referenz nicht prüfen — er würde einen Dangling-Pointer erlauben. `Cow` vermeidet unnötige Allokationen im Normalfall und alloziert nur wenn nötig.

## Falsch — Struct mit Referenz-Feld ohne Lifetime

```rust
// Compiliert nicht: der Compiler weiß nicht, wie lange `part` gültig ist.
struct InvoiceRef {
    part: &str,
}
```

## Richtig — Lifetime-Parameter bindet Struct-Lebensdauer an Daten

```rust
// Kanonisches Lehrbuch-Beispiel (The Rust Programming Language, Kap. 10.3):
// Eine Instanz von ImportantExcerpt kann den String, auf den `part` zeigt,
// nicht überleben — der Compiler erzwingt das.
struct ImportantExcerpt<'a> {
    part: &'a str,
}

fn first_sentence(text: &str) -> ImportantExcerpt<'_> {
    let sentence = text.split('.').next().unwrap_or(text);
    ImportantExcerpt { part: sentence }
}
```

## Richtig — Cow<'a, T>: borrow-or-own ohne Allokation im Normalfall

`Cow<'a, T>` hat zwei Varianten:
- `Cow::Borrowed(&'a T)` — keine Allokation, zeigt in die Eingabe
- `Cow::Owned(T::Owned)` — eigene Allokation, entsteht bei Bedarf

```rust
use std::borrow::Cow;

// Real: apps/sgh-flow/src/services/python_agent.rs:43
// Enthält ein Log-Zeile keine ANSI-Sequenzen, wird der original &str
// zurückgegeben (Cow::Borrowed). Enthält sie Sequenzen, entsteht ein
// neuer String (Cow::Owned) — nur dann wird alloziert.
fn strip_ansi(s: &str) -> Cow<'_, str> {
    if !s.contains('\x1b') {
        return Cow::Borrowed(s);          // kein Alloc
    }
    let mut out = String::with_capacity(s.len());
    // … ANSI-Sequenzen herausfiltern …
    Cow::Owned(out)                        // alloziert nur wenn nötig
}
```

```rust
use std::borrow::Cow;

// Real: apps/sgh-flow/src/api/invoice_validate.rs:187
// PDF-Bytes brauchen XML-Extraktion (Owned); plain XML kann direkt
// weiterverarbeitet werden (Borrowed).
let xml_bytes: Cow<'_, [u8]> = if bytes.starts_with(b"%PDF") {
    Cow::Owned(extract_embedded_xml_from_pdf(bytes)?)
} else {
    Cow::Borrowed(bytes)
};
// Weiterverarbeitung mit &*xml_bytes als &[u8] — kein bedingter Clone.
```

## impl-Block für Struct mit Lifetime

```rust
impl<'a> ImportantExcerpt<'a> {
    // Regel 3 greift: &self → Rückgabe-Lifetime = Lifetime von self.
    // Kein explizites 'a nötig.
    pub fn text(&self) -> &str {
        self.part
    }
}
```

## Quelle
- <https://doc.rust-lang.org/book/ch10-03-lifetime-syntax.html> (Abschnitt "Lifetime Annotations in Struct Definitions")
- <https://doc.rust-lang.org/std/borrow/enum.Cow.html>
