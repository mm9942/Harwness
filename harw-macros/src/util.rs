//! Gemeinsame Namens-Hilfsfunktionen für die Proc-Makro-Expansion.
//!
//! Enthält reine, zustandslose Konvertierungshelfer, die von mehreren
//! Makro-Modulen verwendet werden (aktuell [`crate::tool`] und
//! [`crate::operation`], die aus einem `snake_case`-Funktionsnamen einen
//! `PascalCase`-Struct-Bezeichner ableiten).

/// Convert a `snake_case` identifier into `PascalCase`.
pub(crate) fn pascal_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper = true;
    for c in s.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}
