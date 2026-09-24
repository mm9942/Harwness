//! Interne Hilfsfunktionen, gemeinsam genutzt von allen drei Zerlegungsstrategien.
//!
//! # Verantwortungsbereich
//! Besitzt die drei Bausteine, die `markdown`, `rust_code` und `plain`
//! sonst dreifach implementieren müssten: das Bauen eines [`Chunk`] aus
//! einem Byte-Bereich ([`make_chunk`]), das Bestimmen von Zeilenanfängen
//! ([`line_starts`]) und das sichere Runden auf UTF-8-Zeichengrenzen
//! ([`floor_char_boundary`], [`ceil_char_boundary`]). Dieses Modul ist rein
//! intern (`pub(crate)`) — es gehört nicht zur öffentlichen Fläche dieser
//! Crate.
//!
//! # Nebenläufigkeit
//! Alle Funktionen sind reine, zustandslose Funktionen ohne interne
//! Veränderlichkeit; sicher aus mehreren Threads aufrufbar.
//!
//! # Fehler
//! Keine — dieses Modul validiert nur über `debug_assert!`, es gibt keinen
//! `Result`-Rückgabetyp. Eine verletzte Invariante ist ein Programmierfehler
//! in einer der drei Strategien, kein Laufzeitfehler über nutzergesteuerte
//! Eingaben (jede Eingabe ist ein beliebiger `&str`; die Strategien selbst
//! garantieren, dass sie `make_chunk` nur mit gültigen Bereichen aufrufen).

use harw_lens_types::{ByteSpan, Chunk, ChunkDigest, SourceRef};

/// Baut einen [`Chunk`] aus einem Byte-Bereich `[start, end)` von `text`.
///
/// # Description
/// Zentralisiert die drei Schritte, die für jeden erzeugten Chunk gleich
/// sind: den Digest über exakt die Chunk-Bytes bilden (deterministisch,
/// siehe [`harw_types::ContentDigest::of`]), den [`ByteSpan`] direkt über
/// seine Felder setzen (nicht über [`ByteSpan::new`] — die Aufrufer dieser
/// Funktion garantieren `start <= end` bereits durch Konstruktion, siehe die
/// `debug_assert!`s unten) und den Chunk-Text als eigene `String`-Kopie von
/// `text[start..end]` abzulegen. Genau diese Kopie ist es, die
/// `text[chunk.span.start..chunk.span.end] == chunk.text` in den Tests
/// beweist.
///
/// # Arguments
/// - `source` (`&SourceRef`): Herkunft, wird geklont in den Chunk übernommen.
/// - `text` (`&str`): der volle Quelltext, aus dem der Bereich stammt.
/// - `start` (`usize`): Anfang des Bereichs, inklusive, in Bytes.
/// - `end` (`usize`): Ende des Bereichs, exklusiv, in Bytes.
///
/// # Returns
/// Ein neuer [`Chunk`] mit `span = ByteSpan { start, end }` und
/// `text = text[start..end].to_owned()`.
///
/// # Panics
/// Nur in Debug-Builds über `debug_assert!`, wenn ein Aufrufer aus dieser
/// Crate seine eigene Invariante verletzt (`start > end`, `end > text.len()`
/// oder eine der beiden Grenzen mitten in einem UTF-8-Zeichen). Das sind
/// interne Programmierfehler, keine aus Nutzereingaben erreichbaren Zustände
/// — in Release-Builds bleibt die Funktion ohne Prüfung, wie jede Funktion,
/// die mit `#[derive(Copy)]`-artigen Invarianten arbeitet.
///
/// # Examples
/// ```rust,ignore
/// // Nur intern sichtbar (pub(crate)); siehe `markdown.rs`/`plain.rs`/`rust_code.rs`
/// // für tatsächliche Aufrufstellen.
/// ```
pub(crate) fn make_chunk(source: &SourceRef, text: &str, start: usize, end: usize) -> Chunk {
    debug_assert!(
        start <= end,
        "chunk boundaries must be ordered: start={start}, end={end}"
    );
    debug_assert!(
        end <= text.len(),
        "chunk end {end} must not exceed text length {}",
        text.len()
    );
    debug_assert!(
        text.is_char_boundary(start) && text.is_char_boundary(end),
        "chunk boundaries [{start}, {end}) must fall on UTF-8 char boundaries"
    );

    let slice = &text[start..end];
    Chunk {
        digest: ChunkDigest(harw_types::ContentDigest::of(slice.as_bytes())),
        source: source.clone(),
        span: ByteSpan { start, end },
        text: slice.to_owned(),
    }
}

/// Berechnet die Zeilenanfänge von `text`, mit einem abschließenden
/// Sentinel gleich `text.len()`.
///
/// # Description
/// Eine „Zeile" endet inklusive ihres `\n` (falls vorhanden); die letzte
/// Zeile darf ohne `\n` enden. Für Text mit `n` Zeilen liefert diese
/// Funktion `n + 1` Werte: die `n` Startoffsets plus `text.len()` als
/// Sentinel, sodass Zeile `i` immer `text[starts[i]..starts[i + 1]]` ist —
/// auch für die letzte Zeile. Für leeren Text ist das Ergebnis `[0]` (eine
/// „leere" Zeile der Länge null, kein Aufrufer iteriert in diesem Fall
/// tatsächlich über `windows(2)`, da beide Chunker leeren Text vorab
/// gesondert behandeln).
///
/// # Arguments
/// - `text` (`&str`): der zu vermessende Text.
///
/// # Returns
/// `Vec<usize>` mit mindestens einem Element (`0`), aufsteigend sortiert,
/// letztes Element `== text.len()`.
///
/// # Examples
/// ```rust,ignore
/// // "a\nbb\nc" -> [0, 2, 5, 6]
/// ```
pub(crate) fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(i + 1);
        }
    }
    if starts[starts.len() - 1] != text.len() {
        starts.push(text.len());
    }
    starts
}

/// Verlängert `chunk` bis `new_end`, ohne `start` zu ändern.
///
/// # Description
/// Baut `text`, `span.end` und `digest` neu aus `[chunk.span.start, new_end)`
/// auf. Genutzt von `rust_code`, um reines Leerraum-Lückenmaterial am
/// Dateiende rückwärts in den letzten bereits erzeugten Chunk zu ziehen,
/// statt dafür einen eigenen, bedeutungslosen Chunk zu erzeugen.
///
/// # Arguments
/// - `chunk` (`&mut Chunk`): der zu verlängernde Chunk.
/// - `text` (`&str`): der volle Quelltext.
/// - `new_end` (`usize`): das neue Ende, muss `>= chunk.span.end` sein.
///
/// # Panics
/// Nur in Debug-Builds über `debug_assert!`, wenn `new_end` vor dem
/// bisherigen Ende liegt, über `text.len()` hinausreicht oder keine
/// UTF-8-Zeichengrenze ist — dieselbe interne Invariante wie bei
/// [`make_chunk`].
pub(crate) fn extend_chunk_end(chunk: &mut Chunk, text: &str, new_end: usize) {
    debug_assert!(
        new_end >= chunk.span.end,
        "extend_chunk_end must grow forward: {new_end} < {}",
        chunk.span.end
    );
    debug_assert!(
        new_end <= text.len(),
        "new_end {new_end} exceeds text length {}",
        text.len()
    );
    debug_assert!(
        text.is_char_boundary(new_end),
        "new_end {new_end} must fall on a UTF-8 char boundary"
    );

    let slice = &text[chunk.span.start..new_end];
    chunk.span.end = new_end;
    chunk.text = slice.to_owned();
    chunk.digest = ChunkDigest(harw_types::ContentDigest::of(slice.as_bytes()));
}

/// Rundet `index` auf die nächste UTF-8-Zeichengrenze ab (`<= index`).
///
/// # Description
/// Läuft rückwärts, bis `text.is_char_boundary(index)` gilt. Terminiert
/// spätestens bei `0`, was immer eine gültige Grenze ist. Entspricht der
/// (zur Zeit instabilen) Standardbibliotheksfunktion `str::floor_char_boundary`,
/// hier von Hand geschrieben, um keine Nightly-Funktion vorauszusetzen.
///
/// # Arguments
/// - `text` (`&str`): der Text, gegen den geprüft wird.
/// - `index` (`usize`): der zu rundende Byte-Index, darf `> text.len()` sein.
///
/// # Returns
/// Der größte gültige Zeichengrenzen-Index `<= min(index, text.len())`.
///
/// # Examples
/// ```rust,ignore
/// // floor_char_boundary("a€b", 2) -> 1  // 2 liegt mitten in '€' (3 Bytes)
/// ```
pub(crate) fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut idx = index.min(text.len());
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Rundet `index` auf die nächste UTF-8-Zeichengrenze auf (`>= index`).
///
/// # Description
/// Läuft vorwärts, bis `text.is_char_boundary(index)` gilt oder das Ende von
/// `text` erreicht ist. Entspricht der (zur Zeit instabilen)
/// Standardbibliotheksfunktion `str::ceil_char_boundary`, hier von Hand
/// geschrieben.
///
/// # Arguments
/// - `text` (`&str`): der Text, gegen den geprüft wird.
/// - `index` (`usize`): der zu rundende Byte-Index.
///
/// # Returns
/// Der kleinste gültige Zeichengrenzen-Index `>= index`, höchstens
/// `text.len()`.
///
/// # Examples
/// ```rust,ignore
/// // ceil_char_boundary("a€b", 2) -> 4  // 2 liegt mitten in '€' (Bytes 1..4)
/// ```
pub(crate) fn ceil_char_boundary(text: &str, index: usize) -> usize {
    let len = text.len();
    let mut idx = index.min(len);
    while idx < len && !text.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_line_starts_on_text_without_trailing_newline() {
        assert_eq!(line_starts("a\nbb\nc"), vec![0, 2, 5, 6]);
    }

    #[test]
    fn test_line_starts_on_text_with_trailing_newline() {
        assert_eq!(line_starts("a\nb\n"), vec![0, 2, 4]);
    }

    #[test]
    fn test_line_starts_on_empty_text() {
        assert_eq!(line_starts(""), vec![0]);
    }

    #[test]
    fn test_floor_char_boundary_steps_back_out_of_multibyte_char() {
        let text = "a€b"; // '€' is bytes [1, 4)
        assert_eq!(floor_char_boundary(text, 2), 1);
        assert_eq!(floor_char_boundary(text, 3), 1);
        assert_eq!(floor_char_boundary(text, 4), 4);
    }

    #[test]
    fn test_ceil_char_boundary_steps_forward_out_of_multibyte_char() {
        let text = "a€b"; // '€' is bytes [1, 4)
        assert_eq!(ceil_char_boundary(text, 2), 4);
        assert_eq!(ceil_char_boundary(text, 1), 1);
        assert_eq!(ceil_char_boundary(text, 100), text.len());
    }

    #[test]
    fn test_make_chunk_round_trips_text_via_span() {
        let source = SourceRef::File {
            path: "a.txt".to_owned(),
        };
        let text = "hello world";
        let chunk = make_chunk(&source, text, 0, 5);
        assert_eq!(chunk.text, "hello");
        assert_eq!(&text[chunk.span.start..chunk.span.end], chunk.text);
    }
}
