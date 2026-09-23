//! Zerlegung von Rust-Quelltext an Elementgrenzen.
//!
//! # Verantwortungsbereich
//! Besitzt ausschließlich [`chunk_rust`] und ihre privaten Hilfsfunktionen
//! für Zeilenerkennung (`detect_item_keyword`, `is_doc_or_attr_line`) und
//! Bereichsbestimmung (`find_item_end`). Kein echter Rust-Parser: eine
//! zeilenbasierte Erkennung auf Spaltenebene genügt (siehe
//! „Bekannte Ungenauigkeiten" unten) und hält diese Crate frei von
//! Parser-Abhängigkeiten.
//!
//! # Nebenläufigkeit
//! [`chunk_rust`] ist eine reine Funktion ohne interne Veränderlichkeit;
//! sicher aus mehreren Threads parallel für unterschiedliche Eingaben
//! aufrufbar.
//!
//! # Fehler
//! Keine. Die Funktion gibt kein `Result` zurück — jeder `&str` ist eine
//! gültige Eingabe, auch fehlerhafter oder unvollständiger Rust-Quelltext
//! (siehe „Bekannte Ungenauigkeiten": eine offene, nie geschlossene Klammer
//! lässt den Rest der Datei in einem Chunk zusammenfallen, statt einen
//! Fehler zu erzeugen).
//!
//! # Examples
//! ```rust
//! use harw_lens_chunk::chunk_rust;
//! use harw_lens_types::SourceRef;
//!
//! let source = SourceRef::File { path: "src/lib.rs".to_owned() };
//! let text = "//! Modulkopf.\n\n/// Doku für foo.\npub fn foo() {}\n\npub fn bar() {}\n";
//! let chunks = chunk_rust(&source, text);
//! assert!(chunks[0].text.starts_with("//! Modulkopf."));
//! assert!(chunks[1].text.contains("/// Doku für foo."));
//! assert!(chunks[1].text.contains("pub fn foo()"));
//! ```

use harw_lens_types::{Chunk, SourceRef};

use crate::util::{extend_chunk_end, line_starts, make_chunk};

/// Modifikator-Schlüsselwörter, die vor einem Element-Schlüsselwort stehen
/// dürfen (in beliebiger Kombination, z. B. `pub async unsafe fn`).
const MODIFIERS: [&str; 4] = ["unsafe", "async", "const", "default"];

/// Element-Schlüsselwörter, die (nach optionalem `pub`/Modifikatoren) eine
/// neue Chunk-Grenze auslösen. `impl` wird gesondert behandelt, weil es nie
/// mit `pub` steht.
const KEYWORDS: [&str; 5] = ["fn", "struct", "enum", "trait", "mod"];

/// Zerlegt Rust-Quelltext an Elementgrenzen (`fn`, `struct`, `enum`,
/// `trait`, `impl`, `mod`); der Modulkopf wird ein eigener, vorangestellter
/// Chunk.
///
/// # Description
/// Zerlegungsregel, in Reihenfolge angewendet:
///
/// 1. **Modulkopf zuerst.** Die führenden, zusammenhängenden `//!`-Zeilen am
///    Dateianfang (falls vorhanden) bilden den **ersten** Chunk im
///    Ergebnis-`Vec`. Das ist die wichtigste Einzelentscheidung dieser
///    Strategie: [`Chunk`] trägt kein Gewichtsfeld, also drückt die
///    Reihenfolge die Gewichtung aus — der Modulkopf steht immer an Index 0,
///    wenn er existiert, unabhängig davon, wie viele weitere Chunks folgen.
///    Aufrufer, die „hohe Gewichtung" brauchen (z. B. beim Einbetten mit
///    einem Prioritäts-Bonus), lesen das aus der Position, nicht aus einem
///    Feld.
/// 2. **Elementgrenzen.** Ab dem Ende des Modulkopfs wird zeilenweise nach
///    einer Zeile ohne führende Einrückung gesucht, die (nach optionalem
///    `pub`/`pub(...)` und Modifikatoren wie `unsafe`/`async`/`const`) mit
///    `fn`, `struct`, `enum`, `trait`, `mod` oder `impl` beginnt, gefolgt von
///    einer Wortgrenze. Ein solches Element erstreckt sich von seinem
///    Schlüsselwort bis zur schließenden `}` auf gleicher Klammertiefe
///    (Klammerbilanz, bei `0` beginnend an der Schlüsselwortzeile) oder bis
///    zu einem `;` ohne vorherige `{` (z. B. `mod foo;`, `struct Einheit;`).
/// 3. **`///`-Block bleibt beim Element.** Direkt über dem
///    Schlüsselwort liegende, lückenlos zusammenhängende `///`- oder
///    `#[...]`-Zeilen (Dokumentation bzw. Attribute) gehören zum Chunk des
///    Elements, das sie beschreiben — nicht zum vorhergehenden Chunk. Die
///    Suche nach diesem Vorspann geht nicht weiter zurück als bis zum Ende
///    des vorherigen Chunks.
/// 4. **Lückentext.** Text zwischen zwei erkannten Elementen, der zu keinem
///    Vorspann gehört: besteht er **nur aus Leerraum** (Leerzeilen), wird er
///    stillschweigend dem **folgenden** Element vorangestellt (bzw. bei
///    Lückentext nach dem letzten Element dem **vorherigen** Element
///    angehängt) — eine einzelne Leerzeile bekommt keinen eigenen,
///    bedeutungslosen Chunk. Enthält die Lücke dagegen echten Inhalt (eine
///    `use`-Anweisung, ein freistehender Kommentar, eine Konstante, …),
///    bildet sie einen eigenen Chunk. So bleiben die `ByteSpan`s in jedem
///    Fall lückenlos, ohne die Chunk-Liste mit reinen Leerzeilen-Chunks zu
///    überladen.
///
/// # Bekannte Ungenauigkeiten
/// - **Kein echter Parser.** Die Erkennung arbeitet zeilenbasiert auf
///   Spaltenebene (Einrückungstiefe null, Schlüsselwort am Zeilenanfang,
///   naive Klammerbilanz). Ein `fn`, `struct` usw. **innerhalb** eines
///   Zeichenketten- oder Kommentarinhalts (z. B. in einem `///`-Beispiel mit
///   eingebettetem Code, oder in einem String-Literal) wird nicht von einem
///   echten Element unterschieden, sofern es an Spalte null steht — das ist
///   hinnehmbar, muss aber hier stehen.
/// - **Naive Klammerbilanz.** `{`/`}`-Zeichen innerhalb von
///   String-/Char-Literalen oder Kommentaren werden mitgezählt, als wären es
///   echte Code-Klammern. Ein `"{"` im Text eines Elements kann die erkannte
///   Grenze verschieben.
/// - **Nur Spalte null.** Elemente, die eingerückt sind (z. B. eine Methode
///   innerhalb eines `impl`-Blocks), werden nicht einzeln erkannt — sie
///   bleiben Teil des Chunks des sie umschließenden `impl`-Blocks, weil die
///   Suche nach dem nächsten Element erst nach dessen schließender Klammer
///   fortgesetzt wird.
/// - **Mehrzeilige Attribute** (z. B. `#[derive(\n    Clone,\n)]`) werden nur
///   auf ihrer ersten Zeile als Attribut erkannt; Folgezeilen ohne eigenes
///   `#[`-Präfix zählen nicht zum Vorspann und würden fälschlich in einen
///   Lückentext-Chunk vor dem Element fallen. In der Praxis wird ein solches
///   Attribut in aller Regel mit dem Element zusammen als ein Klammerobjekt
///   erkannt, sofern es geöffnete Klammern enthält, die auf gleicher
///   Zeilenkette bleiben — nicht garantiert.
/// - **Kein Suchen nach Setext- oder Block-Kommentaren** (`/* … */`): ein
///   Block-Kommentar direkt vor einem Element wird nicht als Vorspann erkannt
///   (nur `///` und `#[`/`#![`), er landet im Lückentext-Chunk.
///
/// # Arguments
/// - `source` (`&SourceRef`): Herkunft, wird in jeden erzeugten [`Chunk`]
///   übernommen.
/// - `text` (`&str`): der zu zerlegende Rust-Quelltext.
///
/// # Returns
/// `Vec<Chunk>`, deren `ByteSpan`s `text` lückenlos und ohne Überlappung
/// abdecken. Index 0 ist der Modulkopf-Chunk, sofern `text` mit `//!`
/// beginnt. Leerer `text` ergibt `vec![]`.
///
/// # Examples
/// ```rust
/// use harw_lens_chunk::chunk_rust;
/// use harw_lens_types::SourceRef;
///
/// let source = SourceRef::File { path: "src/lib.rs".to_owned() };
/// let text = "fn a() {}\nfn b() {}\n";
/// let chunks = chunk_rust(&source, text);
/// assert_eq!(chunks.len(), 2);
/// assert_eq!(chunks[0].text, "fn a() {}\n");
/// assert_eq!(chunks[1].text, "fn b() {}\n");
/// ```
#[must_use]
pub fn chunk_rust(source: &SourceRef, text: &str) -> Vec<Chunk> {
    if text.is_empty() {
        return Vec::new();
    }

    let starts = line_starts(text);
    let line_count = starts.len() - 1;

    // Step 1: leading contiguous `//!` lines form the module doc header.
    let mut doc_line_idx = 0usize;
    while doc_line_idx < line_count {
        let line = strip_eol(&text[starts[doc_line_idx]..starts[doc_line_idx + 1]]);
        if line.starts_with("//!") {
            doc_line_idx += 1;
        } else {
            break;
        }
    }
    let module_doc_end = starts[doc_line_idx];

    let mut chunks = Vec::new();
    if doc_line_idx > 0 {
        chunks.push(make_chunk(source, text, 0, module_doc_end));
    }

    // Step 2-4: scan the remainder for element boundaries.
    let mut cursor = module_doc_end;
    let mut line_idx = doc_line_idx;

    while line_idx < line_count {
        let line_start = starts[line_idx];
        let line_end = starts[line_idx + 1];
        let line = strip_eol(&text[line_start..line_end]);
        let has_no_indent = !line.starts_with(' ') && !line.starts_with('\t');

        if has_no_indent && detect_item_keyword(line) {
            let preamble_start_line = find_preamble_start(text, &starts, line_idx, cursor);
            let preamble_start = starts[preamble_start_line];
            let item_end = find_item_end(text, line_start);

            let chunk_start =
                if preamble_start > cursor && !text[cursor..preamble_start].trim().is_empty() {
                    chunks.push(make_chunk(source, text, cursor, preamble_start));
                    preamble_start
                } else {
                    // Pure whitespace between the previous chunk and this element's
                    // preamble (or no gap at all): fold it into this element's chunk
                    // instead of emitting a standalone blank-line chunk.
                    cursor
                };
            chunks.push(make_chunk(source, text, chunk_start, item_end));
            cursor = item_end;

            line_idx = line_index_at_or_after(&starts, item_end, line_idx + 1);
            continue;
        }

        line_idx += 1;
    }

    if cursor < text.len() {
        if text[cursor..].trim().is_empty() {
            match chunks.last_mut() {
                Some(last) => extend_chunk_end(last, text, text.len()),
                None => chunks.push(make_chunk(source, text, cursor, text.len())),
            }
        } else {
            chunks.push(make_chunk(source, text, cursor, text.len()));
        }
    }

    chunks
}

/// Entfernt ein optionales `\r\n`/`\n`-Zeilenende vom Ende von `line`.
fn strip_eol(line: &str) -> &str {
    line.trim_end_matches(['\n', '\r'])
}

/// Prüft, ob `line` eine Dokumentations- oder Attributzeile ist, die als
/// Vorspann eines Elements zählt.
///
/// Private Hilfsfunktion für [`chunk_rust`]. Erkennt `///`, `//!`, `#[` und
/// `#![`-Präfixe (jeweils ohne führende Einrückung, siehe Aufrufer).
fn is_doc_or_attr_line(line: &str) -> bool {
    line.starts_with("///") || line.starts_with("//!") || line.starts_with("#[")
}

/// Läuft von der Element-Zeile `line_idx` rückwärts über zusammenhängende
/// `///`/`#[...]`-Zeilen, ohne vor `cursor` (Byte-Offset) zurückzugehen.
///
/// Private Hilfsfunktion für [`chunk_rust`]. Gibt den Zeilenindex zurück, ab
/// dem der Vorspann beginnt (`line_idx` selbst, falls keine Vorspannzeile
/// gefunden wurde).
fn find_preamble_start(text: &str, starts: &[usize], line_idx: usize, cursor: usize) -> usize {
    let mut preamble_line_idx = line_idx;
    while preamble_line_idx > 0 {
        let candidate_idx = preamble_line_idx - 1;
        let candidate_start = starts[candidate_idx];
        if candidate_start < cursor {
            break;
        }
        let candidate_end = starts[candidate_idx + 1];
        let candidate_line = strip_eol(&text[candidate_start..candidate_end]);
        if is_doc_or_attr_line(candidate_line) {
            preamble_line_idx = candidate_idx;
        } else {
            break;
        }
    }
    preamble_line_idx
}

/// Prüft, ob `line` (ohne Zeilenende, ohne führende Einrückung) ein
/// Element-Schlüsselwort einleitet.
///
/// Private Hilfsfunktion für [`chunk_rust`]. Entfernt zunächst ein optionales
/// `pub`/`pub(...)`-Präfix, dann beliebig viele [`MODIFIERS`], und prüft
/// danach auf eines der [`KEYWORDS`] oder auf `impl`, jeweils gefolgt von
/// einer Wortgrenze (Leerraum, `<`, `(` oder `!` — für `mod foo!` gibt es
/// zwar keine Makro-Module, aber `!` schadet als Grenze nicht).
fn detect_item_keyword(line: &str) -> bool {
    let mut rest = line;

    if let Some(after_pub) = rest.strip_prefix("pub") {
        rest = if let Some(after_paren) = after_pub.strip_prefix('(') {
            match after_paren.find(')') {
                Some(close) => after_paren[close + 1..].trim_start(),
                None => return false,
            }
        } else if after_pub.is_empty() || after_pub.starts_with(char::is_whitespace) {
            after_pub.trim_start()
        } else {
            // "pub" was only a prefix of a longer identifier (e.g. "public"); ignore it.
            line
        };
    }

    loop {
        let mut advanced = false;
        for modifier in MODIFIERS {
            if let Some(after) = rest.strip_prefix(modifier) {
                if after.starts_with(char::is_whitespace) {
                    rest = after.trim_start();
                    advanced = true;
                    break;
                }
            }
        }
        if !advanced {
            break;
        }
    }

    let is_word_boundary = |c: char| c.is_whitespace() || c == '<' || c == '(' || c == '!';

    for keyword in KEYWORDS {
        if let Some(after) = rest.strip_prefix(keyword) {
            if after.is_empty() || after.starts_with(is_word_boundary) {
                return true;
            }
        }
    }

    if let Some(after) = rest.strip_prefix("impl") {
        if after.is_empty() || after.starts_with(|c: char| c.is_whitespace() || c == '<') {
            return true;
        }
    }

    false
}

/// Bestimmt das Ende eines Elements per Klammerbilanz/Semikolon-Suche ab
/// `item_start`.
///
/// Private Hilfsfunktion für [`chunk_rust`]. Zählt `{`/`}` ab `item_start`;
/// erreicht die Bilanz nach mindestens einer geöffneten Klammer wieder
/// `0`, endet das Element am Zeilenende der schließenden Klammer. Wird ein
/// `;` erreicht, bevor überhaupt eine `{` gesehen wurde, endet das Element
/// dort (z. B. `mod foo;`). Läuft die Suche bis zum Textende, ohne dass eine
/// dieser Bedingungen eintritt (unvollständiger/kaputter Quelltext), endet
/// das Element am Textende — ein bewusst konservativer Fallback statt eines
/// Fehlers, siehe „Bekannte Ungenauigkeiten" auf [`chunk_rust`].
fn find_item_end(text: &str, item_start: usize) -> usize {
    let bytes = text.as_bytes();
    let mut depth: i32 = 0;
    let mut opened = false;
    let mut i = item_start;

    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                depth += 1;
                opened = true;
            }
            b'}' => {
                depth -= 1;
                if opened && depth <= 0 {
                    return end_of_line(text, i);
                }
            }
            b';' if !opened && depth == 0 => {
                return end_of_line(text, i);
            }
            _ => {}
        }
        i += 1;
    }
    text.len()
}

/// Gibt den Byte-Offset direkt nach dem Zeilenende zurück, das auf
/// `byte_pos` folgt (oder `text.len()`, falls keines mehr folgt).
///
/// Private Hilfsfunktion für [`find_item_end`]. `byte_pos` zeigt auf ein
/// ASCII-Trennzeichen (`;`, `{` oder `}`); der Rückgabewert liegt immer auf
/// einer gültigen Zeichengrenze, weil `\n` ein Ein-Byte-ASCII-Zeichen ist.
fn end_of_line(text: &str, byte_pos: usize) -> usize {
    match text[byte_pos..].find('\n') {
        Some(offset) => byte_pos + offset + 1,
        None => text.len(),
    }
}

/// Findet den kleinsten Zeilenindex `>= from_idx`, dessen Zeilenanfang
/// `>= byte_offset` liegt.
///
/// Private Hilfsfunktion für [`chunk_rust`], um nach einem gefundenen
/// Element die Zeilensuche exakt an dessen (immer auf einer Zeilengrenze
/// liegendem) Ende fortzusetzen.
fn line_index_at_or_after(starts: &[usize], byte_offset: usize, from_idx: usize) -> usize {
    let mut idx = from_idx;
    while idx < starts.len() && starts[idx] < byte_offset {
        idx += 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn file_source() -> SourceRef {
        SourceRef::File {
            path: "src/lib.rs".to_owned(),
        }
    }

    fn assert_gapless(text: &str, chunks: &[Chunk]) -> TestResult {
        assert!(!chunks.is_empty(), "expected at least one chunk");
        assert_eq!(chunks[0].span.start, 0);
        for pair in chunks.windows(2) {
            assert_eq!(
                pair[0].span.end, pair[1].span.start,
                "chunks must be contiguous without gaps or overlap"
            );
        }
        let last = chunks
            .last()
            .ok_or(TestError::Missing("checked non-empty above"))?;
        assert_eq!(last.span.end, text.len());
        for chunk in chunks {
            assert_eq!(&text[chunk.span.start..chunk.span.end], chunk.text);
        }
        Ok(())
    }

    #[test]
    fn test_chunk_rust_empty_text_yields_no_chunks() {
        assert_eq!(chunk_rust(&file_source(), ""), Vec::new());
    }

    #[test]
    fn test_chunk_rust_module_doc_is_first_and_separate_chunk() -> TestResult {
        let text = "//! Modulkopf.\n//! Zweite Zeile.\n\nfn foo() {}\n";
        let chunks = chunk_rust(&file_source(), text);
        assert_eq!(chunks[0].text, "//! Modulkopf.\n//! Zweite Zeile.\n");
        assert!(chunks.iter().skip(1).any(|c| c.text.contains("fn foo()")));
        assert_gapless(text, &chunks)
    }

    #[test]
    fn test_chunk_rust_without_module_doc_has_no_leading_doc_chunk() {
        let text = "fn foo() {}\n";
        let chunks = chunk_rust(&file_source(), text);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "fn foo() {}\n");
    }

    #[test]
    fn test_chunk_rust_doc_comment_stays_with_its_element() -> TestResult {
        let text = "/// Doku für foo.\npub fn foo() {}\n\npub fn bar() {}\n";
        let chunks = chunk_rust(&file_source(), text);
        let foo_chunk = chunks
            .iter()
            .find(|c| c.text.contains("pub fn foo()"))
            .ok_or(TestError::Missing("foo chunk exists"))?;
        assert!(foo_chunk.text.starts_with("/// Doku für foo."));
        let bar_chunk = chunks
            .iter()
            .find(|c| c.text.contains("pub fn bar()"))
            .ok_or(TestError::Missing("bar chunk exists"))?;
        assert!(!bar_chunk.text.contains("Doku für foo"));
        assert_gapless(text, &chunks)
    }

    #[test]
    fn test_chunk_rust_two_consecutive_fns_are_separated() -> TestResult {
        let text = "fn a() {}\nfn b() {}\n";
        let chunks = chunk_rust(&file_source(), text);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "fn a() {}\n");
        assert_eq!(chunks[1].text, "fn b() {}\n");
        assert_gapless(text, &chunks)
    }

    #[test]
    fn test_chunk_rust_unit_struct_ends_at_semicolon() {
        let text = "struct Marker;\nfn after() {}\n";
        let chunks = chunk_rust(&file_source(), text);
        assert_eq!(chunks[0].text, "struct Marker;\n");
        assert_eq!(chunks[1].text, "fn after() {}\n");
    }

    #[test]
    fn test_chunk_rust_impl_block_is_one_chunk_including_nested_methods() -> TestResult {
        let text = "struct Foo;\n\nimpl Foo {\n    fn method(&self) -> u32 {\n        1\n    }\n}\n\nfn after() {}\n";
        let chunks = chunk_rust(&file_source(), text);
        let impl_chunk = chunks
            .iter()
            .find(|c| c.text.contains("impl Foo"))
            .ok_or(TestError::Missing("impl chunk exists"))?;
        assert!(impl_chunk.text.contains("fn method"));
        assert!(impl_chunk.text.trim_end().ends_with('}'));
        // The blank line before `fn after` is folded into its chunk rather
        // than becoming its own standalone whitespace chunk.
        let last = chunks.last().ok_or(TestError::Missing("has chunks"))?;
        assert!(last.text.contains("fn after() {}"));
        assert_gapless(text, &chunks)
    }

    #[test]
    fn test_chunk_rust_byte_spans_cover_text_without_gaps() -> TestResult {
        let text = "//! Kopf.\n\nuse std::fmt;\n\n/// Doku.\npub struct Foo {\n    pub x: u32,\n}\n\nfn helper() {}\n";
        let chunks = chunk_rust(&file_source(), text);
        assert_gapless(text, &chunks)
    }

    #[test]
    fn test_chunk_rust_is_deterministic_across_repeated_runs() {
        let source = file_source();
        let samples = [
            "//! Kopf.\n\nfn a() {}\nfn b() {}\n",
            "/// Doku.\npub struct Foo {\n    pub x: u32,\n}\n",
            "mod inner;\n\nimpl Foo {\n    fn m(&self) {}\n}\n",
        ];
        for text in samples {
            let first = chunk_rust(&source, text);
            let second = chunk_rust(&source, text);
            assert_eq!(first, second, "chunking must be deterministic for {text:?}");
        }
    }
}
