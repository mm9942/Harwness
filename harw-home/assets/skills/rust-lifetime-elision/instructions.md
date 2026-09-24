# Rust Lifetime Elision

**Regel:** Schreibe keine Lifetime-Annotationen, wenn die drei Elision-Regeln eindeutig greifen; annotiere explizit sobald zwei oder mehr unabhängige Eingangs-Lifetimes auf einen Rückgabewert treffen.

**Warum:** Der Compiler wendet drei deterministische Regeln an. Scheitern alle drei, verweigert er die Kompilierung — dann müssen Lifetimes explizit benannt werden.

## Die drei Elision-Regeln

| Regel | Wann | Effekt |
|-------|------|--------|
| **1 — Input** | Jeder Referenz-Parameter bekommt automatisch eine eigene Lifetime. | `fn f(x: &str, y: &str)` → `fn f<'a,'b>(x: &'a str, y: &'b str)` |
| **2 — Single input** | Gibt es genau einen Eingangs-Lifetime-Parameter, wird er auf alle Ausgaben übertragen. | `fn f(x: &str) -> &str` → `fn f<'a>(x: &'a str) -> &'a str` |
| **3 — Self** | Hat die Funktion `&self` oder `&mut self`, wird dessen Lifetime auf alle Ausgaben übertragen. | `fn path(&self) -> &str` → Lifetime von `self` auf `&str` übertragen |

## Falsch — zwei &str-Eingaben, Rückgabe ohne Annotation

```rust
// Compiliert nicht: zwei unabhängige Input-Lifetimes ('a, 'b),
// Regel 2 greift nicht (mehr als eine), Regel 3 greift nicht (kein self).
fn first_word(s: &str, _sep: &str) -> &str {
    s.split_whitespace().next().unwrap_or(s)
}
```

## Richtig — Regel 2 (eine Eingabe) und Regel 3 (&self) greifen automatisch

```rust
// Regel 2: genau ein Referenz-Parameter → Annotation überflüssig.
fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or(s)
}

// Regel 3: &self → Rückgabe lebt so lange wie self.
// Real: apps/sgh-flow/src/iceberg/handler.rs:316
pub fn pipeline_run_id(&self) -> &str {
    &self.pipeline_run_id
}
```

## Richtig — zwei Eingaben, eine Ausgabe: explizite Annotation nötig

```rust
// Rückgabe gehört zu `root`, nicht zu `path` (der &str-Schlüsselpfad).
// Explizit, weil Regel 2 bei zwei Parametern nicht greift.
// Real: apps/sgh-flow/src/runtime.rs:482
fn toml_path<'a>(root: &'a toml::Value, path: &str) -> Option<&'a toml::Value> {
    path.split('.')
        .try_fold(root, |value, segment| value.get(segment))
}
```

**Merkhilfe:** Wenn der Rückgabewert nur von *einem* der mehreren Eingabe-Referenzen abhängt, muss annotiert werden, damit der Compiler weiß, welche Eingabe die Ausgabe am Leben hält.

## Quelle
- <https://doc.rust-lang.org/book/ch10-03-lifetime-syntax.html> (Abschnitt "Lifetime Elision")

Allgemeine Variante: `ownership-and-resource-lifetimes`
