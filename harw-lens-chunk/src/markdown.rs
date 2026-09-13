//! Zerlegung von Markdown-Text an Überschriftsgrenzen.
//!
//! # Verantwortungsbereich
//! Besitzt ausschließlich [`chunk_markdown`] und ihre beiden privaten
//! Zeilenprädikate ([`is_atx_heading`], [`is_fence_delimiter`]). Diese
//! Strategie tut kein I/O und liest keine Datei — sie bekommt bereits
//! geladenen Text und eine [`SourceRef`] zur Beschriftung.
//!
//! # Nebenläufigkeit
//! [`chunk_markdown`] ist eine reine Funktion ohne interne Veränderlichkeit;
//! sicher aus mehreren Threads parallel für unterschiedliche Eingaben
//! aufrufbar.
//!
//! # Fehler
//! Keine. Die Funktion gibt kein `Result` zurück — jeder `&str` ist eine
//! gültige Eingabe, auch leerer Text (Ergebnis dann `vec![]`) und Text ohne
//! jede Überschrift (Ergebnis dann ein einziger Chunk über den ganzen Text).
//!
//! # Examples
//! ```rust
//! use harw_lens_chunk::chunk_markdown;
//! use harw_lens_types::SourceRef;
//!
//! let source = SourceRef::File { path: "README.md".to_owned() };
//! let text = "# Titel\n\nEinleitung.\n\n## Abschnitt\n\nInhalt.\n";
//! let chunks = chunk_markdown(&source, text);
//! assert_eq!(chunks.len(), 2);
//! assert!(chunks[0].text.starts_with("# Titel"));
//! assert!(chunks[1].text.starts_with("## Abschnitt"));
//! ```

use harw_lens_types::{Chunk, SourceRef};

use crate::util::{line_starts, make_chunk};

/// Zerlegt Markdown-Text an Überschriftsgrenzen; Codeblöcke bleiben ganz.
///
/// # Description
/// Zerlegungsregel: eine ATX-Überschrift (`#` bis `######`, gefolgt von
/// einem Leerzeichen/Tab oder Zeilenende, mit höchstens drei führenden
/// Leerzeichen — CommonMark-Regel) beginnt einen **neuen** Chunk; sie gehört
/// zu dem Chunk, den sie einleitet, nicht zum vorhergehenden. Ein
/// Fence-Codeblock (eine Zeile, die — nach höchstens drei führenden
/// Leerzeichen — mit drei Backticks oder drei Tilden (`~~~`) beginnt, bis
/// zur nächsten gleichartigen Fence-Zeile) wird dabei **nie** in der Mitte
/// geteilt:
/// solange eine Fence offen ist, lösen überschriftsähnliche Zeilen (z. B.
/// ein `# comment` in einem Shell-Codeblock) keine Chunk-Grenze aus. Text
/// vor der ersten Überschrift (falls vorhanden) bildet einen eigenen
/// führenden Chunk.
///
/// # Bekannte Ungenauigkeiten
/// - Nur Fence-Codeblöcke werden erkannt, keine 4-Leerzeichen-eingerückten
///   Codeblöcke (CommonMark kennt auch diese Form; sie wird hier wie
///   normaler Text behandelt und kann daher an einer Überschrift-ähnlichen
///   Zeile in ihrem Inneren doch geteilt werden — in eingerücktem Code ist
///   das selten, aber möglich).
/// - Setext-Überschriften (Text, gefolgt von einer Zeile aus `===` oder
///   `---`) werden nicht erkannt, nur ATX-Überschriften (`#`-Präfix).
/// - Eine Fence-Zeile mit falscher Zeichenzahl (z. B. eine schließende
///   Fence mit weniger Backticks als die öffnende) wird trotzdem als
///   Fence-Ende gewertet — es wird nur auf das Fence-Präfix geprüft, nicht
///   auf exakte Übereinstimmung von Zeichen und Länge.
/// - Eine nie geschlossene Fence lässt den Rest des Dokuments als Teil des
///   laufenden Chunks gelten (keine weitere Überschriftserkennung danach) —
///   ein bewusst konservativer Fallback statt eines Fehlers.
///
/// # Arguments
/// - `source` (`&SourceRef`): Herkunft, wird in jeden erzeugten [`Chunk`]
///   übernommen.
/// - `text` (`&str`): der zu zerlegende Markdown-Text.
///
/// # Returns
/// `Vec<Chunk>` in Textreihenfolge, deren `ByteSpan`s `text` lückenlos und
/// ohne Überlappung abdecken. Leerer `text` ergibt `vec![]`.
///
/// # Examples
/// ```rust
/// use harw_lens_chunk::chunk_markdown;
/// use harw_lens_types::SourceRef;
///
/// let source = SourceRef::File { path: "notes.md".to_owned() };
/// let text = "Vorspann.\n\n# Erste Überschrift\n\nText.\n";
/// let chunks = chunk_markdown(&source, text);
/// assert_eq!(chunks[0].text, "Vorspann.\n\n");
/// assert!(chunks[1].text.starts_with("# Erste Überschrift"));
/// ```
#[must_use]
pub fn chunk_markdown(source: &SourceRef, text: &str) -> Vec<Chunk> {
    if text.is_empty() {
        return Vec::new();
    }

    let starts = line_starts(text);
    let mut chunks = Vec::new();
    let mut chunk_start = 0usize;
    let mut in_fence = false;

    for window in starts.windows(2) {
        let (line_start, line_end) = (window[0], window[1]);
        let line = &text[line_start..line_end];

        if is_fence_delimiter(line) {
            in_fence = !in_fence;
            continue;
        }

        if !in_fence && line_start > chunk_start && is_atx_heading(line) {
            chunks.push(make_chunk(source, text, chunk_start, line_start));
            chunk_start = line_start;
        }
    }

    if chunk_start < text.len() {
        chunks.push(make_chunk(source, text, chunk_start, text.len()));
    }

    chunks
}

/// Prüft, ob `line` (inklusive optionalem Zeilenende) eine ATX-Überschrift
/// ist.
///
/// Private Hilfsfunktion für [`chunk_markdown`]. Erlaubt bis zu drei
/// führende Leerzeichen (CommonMark), verlangt danach ein bis sechs `#` und
/// entweder Zeilenende, ein Leerzeichen oder einen Tab.
fn is_atx_heading(line: &str) -> bool {
    let content = line.trim_end_matches(['\n', '\r']);
    let trimmed = content.trim_start_matches(' ');
    let leading_spaces = content.len() - trimmed.len();
    if leading_spaces > 3 {
        return false;
    }

    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&hashes) {
        return false;
    }

    matches!(trimmed.as_bytes().get(hashes), None | Some(b' ' | b'\t'))
}

/// Prüft, ob `line` eine Fence-Codeblock-Grenze ist (öffnend oder
/// schließend — beide sehen syntaktisch gleich aus).
///
/// Private Hilfsfunktion für [`chunk_markdown`]. Erlaubt bis zu drei
/// führende Leerzeichen, danach mindestens drei Backticks oder Tilden.
fn is_fence_delimiter(line: &str) -> bool {
    let content = line.trim_end_matches(['\n', '\r']);
    let trimmed = content.trim_start_matches(' ');
    let leading_spaces = content.len() - trimmed.len();
    if leading_spaces > 3 {
        return false;
    }
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_source() -> SourceRef {
        SourceRef::File {
            path: "doc.md".to_owned(),
        }
    }

    fn assert_gapless(text: &str, chunks: &[Chunk]) {
        assert!(!chunks.is_empty(), "expected at least one chunk");
        assert_eq!(chunks[0].span.start, 0);
        for pair in chunks.windows(2) {
            assert_eq!(
                pair[0].span.end, pair[1].span.start,
                "chunks must be contiguous without gaps or overlap"
            );
        }
        let last = chunks.last().expect("checked non-empty above");
        assert_eq!(last.span.end, text.len());
        for chunk in chunks {
            assert_eq!(&text[chunk.span.start..chunk.span.end], chunk.text);
        }
    }

    #[test]
    fn test_chunk_markdown_empty_text_yields_no_chunks() {
        assert_eq!(chunk_markdown(&file_source(), ""), Vec::new());
    }

    #[test]
    fn test_chunk_markdown_without_headings_yields_one_chunk() {
        let text = "just some text\nwith two lines\n";
        let chunks = chunk_markdown(&file_source(), text);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, text);
    }

    #[test]
    fn test_chunk_markdown_splits_at_headings() {
        let text = "Vorspann.\n\n# Erste\n\nText A.\n\n## Zweite\n\nText B.\n";
        let chunks = chunk_markdown(&file_source(), text);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].text, "Vorspann.\n\n");
        assert!(chunks[1].text.starts_with("# Erste"));
        assert!(chunks[1].text.contains("Text A."));
        assert!(chunks[2].text.starts_with("## Zweite"));
        assert_gapless(text, &chunks);
    }

    #[test]
    fn test_chunk_markdown_heading_at_start_has_no_leading_empty_chunk() {
        let text = "# Nur Titel\n\nText.\n";
        let chunks = chunk_markdown(&file_source(), text);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.starts_with("# Nur Titel"));
    }

    #[test]
    fn test_chunk_markdown_code_block_spanning_heading_marker_stays_whole() {
        // The '#' line inside the fence looks like a heading but must not
        // split the code block.
        let text = "# Titel\n\n```bash\n# this looks like a heading\necho hi\n```\n\n## Nächste\n";
        let chunks = chunk_markdown(&file_source(), text);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].text.contains("```bash"));
        assert!(chunks[0].text.contains("# this looks like a heading"));
        assert!(chunks[0].text.contains("echo hi"));
        assert!(chunks[0].text.trim_end().ends_with("```"));
        assert!(chunks[1].text.starts_with("## Nächste"));
        assert_gapless(text, &chunks);
    }

    #[test]
    fn test_chunk_markdown_byte_spans_cover_text_without_gaps() {
        let text = "# A\n\ntext\n\n# B\n\nmore\n\n### C\nend";
        let chunks = chunk_markdown(&file_source(), text);
        assert_gapless(text, &chunks);
    }

    #[test]
    fn test_chunk_markdown_is_deterministic_across_repeated_runs() {
        let source = file_source();
        let samples = [
            "# A\n\ntext\n\n## B\nmore\n",
            "no headings here at all\njust prose\n",
            "```\n# fenced, not a heading\n```\n\n# Real Heading\ncontent\n",
        ];
        for text in samples {
            let first = chunk_markdown(&source, text);
            let second = chunk_markdown(&source, text);
            assert_eq!(first, second, "chunking must be deterministic for {text:?}");
        }
    }
}
