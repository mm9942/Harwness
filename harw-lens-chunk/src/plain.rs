//! Zerlegung von Klartext in ein gleitendes Fenster mit Überlappung.
//!
//! # Verantwortungsbereich
//! Besitzt ausschließlich [`chunk_plain`] und die beiden Voreinstellungen
//! [`DEFAULT_TARGET_BYTES`]/[`DEFAULT_OVERLAP_BYTES`]. Anders als
//! [`crate::chunk_markdown`] und [`crate::chunk_rust`] kennt diese Strategie
//! keine Struktur im Eingabetext — sie funktioniert auf jedem `&str`,
//! unabhängig vom Format.
//!
//! # Nebenläufigkeit
//! [`chunk_plain`] ist eine reine Funktion ohne interne Veränderlichkeit;
//! sicher aus mehreren Threads parallel für unterschiedliche Eingaben
//! aufrufbar.
//!
//! # Fehler
//! Keine. Die Funktion gibt kein `Result` zurück; entartete Parameter
//! (`target_bytes == 0`, `overlap_bytes >= target_bytes`) werden intern
//! geklemmt (siehe `# Description` auf [`chunk_plain`]), nicht zurückgewiesen.
//!
//! # Examples
//! ```rust
//! use harw_lens_chunk::{chunk_plain, DEFAULT_OVERLAP_BYTES, DEFAULT_TARGET_BYTES};
//! use harw_lens_types::SourceRef;
//!
//! let source = SourceRef::File { path: "notes.txt".to_owned() };
//! let text = "Ein kurzer Text.";
//! let chunks = chunk_plain(&source, text, DEFAULT_TARGET_BYTES, DEFAULT_OVERLAP_BYTES);
//! assert_eq!(chunks.len(), 1);
//! assert_eq!(chunks[0].text, text);
//! ```

use harw_lens_types::{Chunk, SourceRef};

use crate::util::{ceil_char_boundary, floor_char_boundary, make_chunk};

/// Voreingestellte Fenstergröße in Bytes für [`chunk_plain`].
///
/// # Description
/// 2048 Bytes entsprechen — nach der in `harw-lens-types::BytesOverFour`
/// bereits verwendeten Konvention „vier Bytes je Kosteneinheit" — rund 512
/// Kosteneinheiten. 512 ist eine in Retrieval-Pipelines verbreitete
/// Zielgröße für einen Embedding-Chunk: groß genug, um einen
/// zusammenhängenden Gedanken zu tragen, klein genug, um in praktisch jedes
/// Embedding-Modell-Kontextfenster mehrfach zu passen. Es ist ein
/// Vorschlagswert für Aufrufer, die keinen eigenen Grund für eine andere
/// Zahl haben — [`chunk_plain`] selbst erzwingt ihn nicht.
pub const DEFAULT_TARGET_BYTES: usize = 2048;

/// Voreingestellte Überlappung in Bytes für [`chunk_plain`].
///
/// # Description
/// Rund zehn Prozent von [`DEFAULT_TARGET_BYTES`]. Genug, damit ein Satz,
/// der zufällig auf einer Fenstergrenze liegt, in beiden benachbarten Chunks
/// vollständig lesbar bleibt, ohne den gespeicherten Text durch zu große
/// Redundanz spürbar aufzublähen.
pub const DEFAULT_OVERLAP_BYTES: usize = 200;

/// Zerlegt `text` in ein gleitendes Fenster mit Überlappung, an Wort- oder
/// Absatzgrenzen ausgerichtet.
///
/// # Description
/// Jeder Chunk beginnt bei `start` und endet spätestens bei
/// `start + target_bytes` Bytes weiter — außer beim letzten Chunk, der bis
/// zum Textende reicht, und außer wenn diese Grenze mitten in einem Wort
/// läge: dann wird sie auf die letzte Absatzgrenze (`\n\n`) oder, falls
/// keine im Fenster liegt, auf das letzte Leerzeichen davor zurückgezogen.
/// Gibt es innerhalb des Fensters weder eine Absatz- noch eine Wortgrenze
/// (ein einzelnes, sehr langes „Wort"), wird hart bei der Byte-Grenze
/// geschnitten. In jedem Fall liegt die tatsächlich gewählte Grenze auf
/// einer UTF-8-Zeichengrenze — nie mitten in einem Mehrbyte-Zeichen
/// (Umlaute, Emoji, …).
///
/// Der nächste Chunk beginnt `overlap_bytes` Bytes vor dem Ende des
/// vorherigen (wieder auf die nächste Zeichengrenze aufgerundet): die
/// Überlappung existiert, damit eine Aussage, die zufällig auf einer
/// Fenstergrenze liegt, nicht in beiden Hälften unverständlich wird. Die
/// letzten Bytes eines Chunks erscheinen deshalb wortwörtlich am Anfang des
/// nächsten.
///
/// **Parameter-Klemmung:** `target_bytes` wird intern auf mindestens `1`
/// angehoben (ein Aufruf mit `0` liefe sonst endlos ohne Fortschritt);
/// `overlap_bytes` wird auf höchstens `target_bytes - 1` gekappt (eine
/// Überlappung, die die volle Fenstergröße erreicht oder überschreitet,
/// ließe das Fenster nie vorrücken). Es gibt deshalb keinen Fehlerfall für
/// diese beiden Parameter — nur ein degeneriertes, aber terminierendes
/// Ergebnis.
///
/// # Bekannte Ungenauigkeiten
/// - Die Grenzsuche erkennt Absätze nur über ein exaktes `\n\n` (zwei
///   aufeinanderfolgende Zeilenumbrüche ohne Leerraum dazwischen); ein
///   Absatz, der durch eine Zeile mit nur Leerzeichen getrennt ist, wird
///   nicht als Absatzgrenze erkannt und fällt auf die Wortgrenzensuche
///   zurück.
/// - „Wort" bedeutet hier ausschließlich `char::is_whitespace`; Satzzeichen
///   ohne umgebenden Leerraum (z. B. ein Komma direkt vor einem
///   Zeilenumbruch) zählen nicht als eigene Grenze.
/// - Bei sehr kleinem `target_bytes` relativ zur Zeichenbreite (z. B.
///   `target_bytes = 1` bei einem Text aus vierbytigen Emoji) kann ein
///   einzelner Chunk größer als `target_bytes` ausfallen, weil mindestens
///   ein vollständiges Zeichen aufgenommen wird, um Fortschritt zu
///   garantieren.
///
/// # Arguments
/// - `source` (`&SourceRef`): Herkunft, wird in jeden erzeugten [`Chunk`]
///   übernommen.
/// - `text` (`&str`): der zu zerlegende Text.
/// - `target_bytes` (`usize`): angestrebte Fenstergröße in Bytes.
/// - `overlap_bytes` (`usize`): angestrebte Überlappung in Bytes zwischen
///   aufeinanderfolgenden Chunks.
///
/// # Returns
/// `Vec<Chunk>` in Textreihenfolge. Aufeinanderfolgende `ByteSpan`s
/// überlappen sich um (mindestens) die tatsächlich wirksame Überlappung und
/// lassen keine Lücke; der erste Chunk beginnt bei `0`, der letzte endet bei
/// `text.len()`. Leerer `text` ergibt `vec![]`.
///
/// # Examples
/// ```rust
/// use harw_lens_chunk::chunk_plain;
/// use harw_lens_types::SourceRef;
///
/// let source = SourceRef::File { path: "notes.txt".to_owned() };
/// let text = "eins zwei drei vier fünf sechs sieben acht neun zehn";
/// let chunks = chunk_plain(&source, text, 20, 5);
/// assert!(chunks.len() > 1);
/// // Die Überlappung ist ein wortwörtlicher Suffix/Präfix-Zusammenhang:
/// let a_end = &chunks[0].text[chunks[0].text.len().saturating_sub(5)..];
/// assert!(chunks[1].text.starts_with(a_end) || chunks[1].text.contains(a_end));
/// ```
#[must_use]
pub fn chunk_plain(
    source: &SourceRef,
    text: &str,
    target_bytes: usize,
    overlap_bytes: usize,
) -> Vec<Chunk> {
    if text.is_empty() {
        return Vec::new();
    }

    let len = text.len();
    let target = target_bytes.max(1);
    let overlap = overlap_bytes.min(target.saturating_sub(1));

    let mut chunks = Vec::new();
    let mut start = 0usize;

    loop {
        let raw_end = (start + target).min(len);
        let end = if raw_end >= len {
            len
        } else {
            align_boundary(text, start, raw_end)
        };

        chunks.push(make_chunk(source, text, start, end));

        if end >= len {
            break;
        }

        let raw_next_start = end.saturating_sub(overlap).max(start + 1);
        start = ceil_char_boundary(text, raw_next_start).min(end);
    }

    chunks
}

/// Findet die beste Schnittgrenze `<= raw_end` für ein Fenster, das bei
/// `start` beginnt.
///
/// Private Hilfsfunktion für [`chunk_plain`]. Bevorzugt eine Absatzgrenze
/// (`\n\n`), dann die letzte Wortgrenze, sonst einen harten Schnitt auf der
/// nächsten Zeichengrenze `>= start + 1` (garantiert Fortschritt).
fn align_boundary(text: &str, start: usize, raw_end: usize) -> usize {
    let safe_raw_end = floor_char_boundary(text, raw_end).max(start);
    if safe_raw_end <= start {
        return ceil_char_boundary(text, start + 1);
    }

    let window = &text[start..safe_raw_end];

    if let Some(rel) = window.rfind("\n\n") {
        let cut = start + rel + 2;
        if cut > start {
            return cut;
        }
    }

    if let Some(rel) = window.rfind(char::is_whitespace) {
        let ch_len = window[rel..].chars().next().map_or(1, char::len_utf8);
        let cut = start + rel + ch_len;
        if cut > start {
            return cut;
        }
    }

    safe_raw_end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn file_source() -> SourceRef {
        SourceRef::File {
            path: "notes.txt".to_owned(),
        }
    }

    fn assert_gapless_with_overlap(text: &str, chunks: &[Chunk]) -> TestResult {
        assert!(!chunks.is_empty(), "expected at least one chunk");
        assert_eq!(chunks[0].span.start, 0);
        for pair in chunks.windows(2) {
            assert!(
                pair[1].span.start <= pair[0].span.end,
                "no gap allowed between consecutive windows"
            );
            assert!(
                pair[1].span.start > pair[0].span.start,
                "windows must make forward progress"
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
    fn test_chunk_plain_empty_text_yields_no_chunks() {
        assert_eq!(chunk_plain(&file_source(), "", 100, 10), Vec::new());
    }

    #[test]
    fn test_chunk_plain_short_text_yields_single_chunk() {
        let text = "kurzer Text";
        let chunks = chunk_plain(&file_source(), text, 1000, 100);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, text);
        assert_eq!(chunks[0].span.start, 0);
        assert_eq!(chunks[0].span.end, text.len());
    }

    #[test]
    fn test_chunk_plain_produces_overlap_between_consecutive_chunks() {
        let text = "eins zwei drei vier fünf sechs sieben acht neun zehn elf zwölf";
        let chunks = chunk_plain(&file_source(), text, 20, 8);
        assert!(chunks.len() > 1, "expected multiple windows");
        for pair in chunks.windows(2) {
            let prev = &pair[0];
            let next = &pair[1];
            assert!(
                next.span.start < prev.span.end,
                "expected overlap: next starts at {} before prev ends at {}",
                next.span.start,
                prev.span.end
            );
            let overlap_text = &text[next.span.start..prev.span.end];
            assert!(prev.text.ends_with(overlap_text));
            assert!(next.text.starts_with(overlap_text));
        }
    }

    #[test]
    fn test_chunk_plain_never_splits_multibyte_characters() -> TestResult {
        // Contains umlauts (2-byte) and an emoji (4-byte).
        let text = "Müller sagt Grüße 😀 über die Straße nach München und züruck 🎉 mit Freunden";
        for target in [5usize, 10, 17, 30] {
            let chunks = chunk_plain(&file_source(), text, target, 3);
            for chunk in &chunks {
                assert!(text.is_char_boundary(chunk.span.start));
                assert!(text.is_char_boundary(chunk.span.end));
            }
            assert_gapless_with_overlap(text, &chunks)?;
        }
        Ok(())
    }

    #[test]
    fn test_chunk_plain_zero_target_bytes_is_clamped_and_terminates() -> TestResult {
        let text = "ab cd ef";
        let chunks = chunk_plain(&file_source(), text, 0, 0);
        assert!(!chunks.is_empty());
        assert_gapless_with_overlap(text, &chunks)?;
        Ok(())
    }

    #[test]
    fn test_chunk_plain_overlap_greater_than_target_is_clamped() -> TestResult {
        let text = "ab cd ef gh ij kl mn op";
        // overlap_bytes far exceeds target_bytes; must still terminate and progress.
        let chunks = chunk_plain(&file_source(), text, 4, 1000);
        assert!(!chunks.is_empty());
        assert_gapless_with_overlap(text, &chunks)?;
        Ok(())
    }

    #[test]
    fn test_chunk_plain_byte_spans_cover_text_with_overlap() -> TestResult {
        let text = "Absatz eins mit mehreren Wörtern.\n\nAbsatz zwei mit weiteren Wörtern.\n\nAbsatz drei.";
        let chunks = chunk_plain(&file_source(), text, 30, 10);
        assert_gapless_with_overlap(text, &chunks)?;
        Ok(())
    }

    #[test]
    fn test_chunk_plain_prefers_paragraph_boundary_over_word_boundary() {
        let text = "erste Zeile hier\n\nzweite Zeile dort ist laenger";
        let chunks = chunk_plain(&file_source(), text, 20, 0);
        assert!(chunks[0].text.ends_with("\n\n"));
    }

    #[test]
    fn test_chunk_plain_is_deterministic_across_repeated_runs() {
        let source = file_source();
        let samples: [(&str, usize, usize); 3] = [
            ("eins zwei drei vier fünf sechs sieben acht neun", 15, 5),
            ("Absatz eins.\n\nAbsatz zwei.\n\nAbsatz drei.", 12, 4),
            ("München Straße 😀 züruck 🎉 Freunden", 10, 3),
        ];
        for (text, target, overlap) in samples {
            let first = chunk_plain(&source, text, target, overlap);
            let second = chunk_plain(&source, text, target, overlap);
            assert_eq!(first, second, "chunking must be deterministic for {text:?}");
        }
    }
}
