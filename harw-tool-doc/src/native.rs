//! Native PDF-Textextraktion über die Crate `oxidize-pdf` (Spec:
//! `doc_read_pdf_design.md`, Abschnitt „Native Extraktion").
//!
//! # Verantwortung
//! Dieses Modul besitzt die synchrone, blockierende Extraktion von Text aus
//! einem bereits vollständig eingelesenen PDF-Byte-Puffer. Es öffnet das
//! Dokument über `oxidize_pdf::parser`, entsperrt es bei Verschlüsselung mit
//! einem leeren Passwort, ermittelt die Seitenzahl und liest die im
//! übergebenen [`crate::types::PageRange`] angeforderten Seiten aus. Es
//! entscheidet NICHT, ob Mistral OCR oder die native Extraktion verwendet
//! wird (das übernimmt `crate::tool`), führt selbst kein Datei-I/O aus (das
//! Einlesen der PDF-Datei ist Sache des Aufrufers) und startet keinen
//! eigenen Thread/Task — der Aufrufer führt [`extract_pages`] in
//! `tokio::task::spawn_blocking` aus.
//!
//! # Schlüsseltypen
//! - [`extract_pages`] — einzige öffentliche Funktion dieses Moduls.
//!
//! # Nebenläufigkeit
//! Rein funktional und zustandslos zwischen Aufrufen; `bytes` wird nur
//! gelesen (`&[u8]`), nichts wird geteilt. Nicht async — blockierend, deshalb
//! der Hinweis im Doc-Kommentar der Funktion. Jeder Aufruf in die
//! Fremd-Crate `oxidize-pdf` (nicht vertrauenswürdige PDF-Eingabe) ist über
//! `std::panic::catch_unwind` abgesichert, damit eine Panik dort nicht den
//! aufrufenden `spawn_blocking`-Task mitreißt.
//!
//! # Fehlertypen
//! Gibt ausschließlich [`DocToolError::Parse`], [`DocToolError::Encrypted`]
//! und [`DocToolError::ExtractionPanicked`] zurück.
//!
//! # Examples
//! ```rust,no_run
//! use harw_tool_doc::native::extract_pages;
//!
//! # fn run() -> harw_tool_doc::DocToolResult<()> {
//! let bytes = std::fs::read("bericht.pdf")?;
//! let document = extract_pages(&bytes, None)?;
//! for page in &document.pages {
//!     println!("Seite {}: {} Zeichen extrahiert", page.number, page.text.len());
//! }
//! # Ok(())
//! # }
//! ```

use crate::error::{DocToolError, DocToolResult};
use crate::types::{Backend, ExtractedDocument, ExtractedPage, PageRange};
use oxidize_pdf::parser::{ParseError, PdfDocument, PdfReader};
use std::io::Cursor;
use std::panic::{self, AssertUnwindSafe};

/// Platzhaltertext für eine Seite, deren Extraktion paniert ist oder einen
/// Parse-Fehler geliefert hat, ohne dass das gesamte Dokument abgebrochen
/// wird.
const PAGE_UNREADABLE_PLACEHOLDER: &str = "[Seite konnte nicht gelesen werden]";

/// Konkreter Reader-Typ: ein `Cursor` über eine eigene Kopie der Eingabe-Bytes
/// (siehe [`open_document`] — `oxidize_pdf::parser::PdfReader` verlangt
/// `Read + Seek`, ein `Cursor<Vec<u8>>` erfüllt das unabhängig von der
/// Lebenszeit des übergebenen `&[u8]`).
type CursorReader = Cursor<Vec<u8>>;

/// Extrahiert Text aus den angeforderten Seiten eines bereits eingelesenen
/// PDF-Byte-Puffers.
///
/// # Description
/// Öffnet `bytes` über `oxidize_pdf::parser::PdfReader`, entsperrt das
/// Dokument bei Verschlüsselung mit einem leeren Passwort und liest je
/// angeforderter Seite den Text aus. Jeder Aufruf in die Fremd-Crate ist über
/// `std::panic::catch_unwind` abgesichert: eine Panik beim Öffnen oder beim
/// Ermitteln der Seitenzahl bricht die Funktion mit einem Fehler ab, eine
/// Panik oder ein Parse-Fehler auf einer einzelnen Seite wird stattdessen als
/// Platzhaltertext in genau diese eine Seite geschrieben, damit die übrigen
/// Seiten trotzdem geliefert werden. Diese Funktion blockiert den
/// aufrufenden Thread; der Aufrufer führt sie in
/// `tokio::task::spawn_blocking` aus.
///
/// # Arguments
/// - `bytes` (`&[u8]`): vollständiger Inhalt der PDF-Datei.
/// - `range` (`Option<PageRange>`): 1-basierter, angeforderter Seitenbereich.
///   `None` bedeutet: alle Seiten. Ein offenes Ende (`last == None`) bedeutet:
///   bis zur letzten Seite des Dokuments.
///
/// # Returns
/// [`ExtractedDocument`] mit `backend = Backend::Native`, der ermittelten
/// Gesamtseitenzahl (`total_pages`) und den Seiten im angeforderten Bereich,
/// aufsteigend sortiert. Liegt `range.first` hinter der letzten Seite des
/// Dokuments, liefert die Funktion ein `ExtractedDocument` mit leerem
/// `pages` zurück — das ist kein Fehler, der Aufrufer meldet dem Modell
/// „Datei hat nur N Seiten“.
///
/// # Errors
/// - [`DocToolError::Parse`]: das Dokument lässt sich nicht als PDF lesen
///   (kaputte Struktur, kein PDF o. Ä.) oder die Seitenzahl lässt sich nicht
///   ermitteln.
/// - [`DocToolError::Encrypted`]: das Dokument ist verschlüsselt und ein
///   leeres Passwort hat es nicht entsperrt.
/// - [`DocToolError::ExtractionPanicked`]: `oxidize-pdf` ist beim Öffnen des
///   Dokuments oder beim Ermitteln der Seitenzahl paniert (`page: None`).
///
/// # Concurrency
/// Blockierend, nicht async, kein geteilter veränderlicher Zustand. Sicher
/// aus mehreren Threads gleichzeitig mit unterschiedlichen `bytes`-Puffern
/// aufrufbar, da jeder Aufruf ausschließlich mit lokalen Daten arbeitet.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_doc::native::extract_pages;
///
/// # fn run() -> harw_tool_doc::DocToolResult<()> {
/// let bytes = std::fs::read("bericht.pdf")?;
/// let document = extract_pages(&bytes, None)?;
/// for page in &document.pages {
///     println!("Seite {}: {} Zeichen extrahiert", page.number, page.text.len());
/// }
/// # Ok(())
/// # }
/// ```
pub fn extract_pages(bytes: &[u8], range: Option<PageRange>) -> DocToolResult<ExtractedDocument> {
    let document = open_document(bytes)?;
    let page_count = read_page_count(&document)?;
    tracing::debug!(page_count, "PDF geöffnet, native Extraktion beginnt");

    let first = range.map(|r| r.first).unwrap_or(1);
    if first > page_count {
        tracing::debug!(
            first,
            page_count,
            "angeforderte Startseite liegt hinter dem Dokumentende"
        );
        return Ok(ExtractedDocument {
            total_pages: Some(page_count),
            pages: Vec::new(),
            backend: Backend::Native,
        });
    }
    let last = range
        .and_then(|r| r.last)
        .map(|last| last.min(page_count))
        .unwrap_or(page_count);

    let mut pages = Vec::new();
    for number in first..=last {
        pages.push(extract_one_page(&document, number));
    }

    Ok(ExtractedDocument {
        total_pages: Some(page_count),
        pages,
        backend: Backend::Native,
    })
}

// Öffnet das Dokument (Reader bauen, bei Verschlüsselung mit leerem Passwort
// entsperren, in ein `PdfDocument` überführen) als eine panik-geschützte
// Einheit; eine Panik darin wird als `ExtractionPanicked { page: None }`
// gemeldet.
fn open_document(bytes: &[u8]) -> DocToolResult<PdfDocument<CursorReader>> {
    let owned = bytes.to_vec();
    let outcome = panic::catch_unwind(AssertUnwindSafe(
        move || -> DocToolResult<PdfDocument<CursorReader>> {
            let mut reader = PdfReader::new(Cursor::new(owned)).map_err(map_parse_error)?;
            if reader.is_encrypted() {
                let unlocked = reader.try_empty_password().map_err(map_parse_error)?;
                if !unlocked {
                    return Err(DocToolError::Encrypted);
                }
            }
            Ok(PdfDocument::new(reader))
        },
    ));
    match outcome {
        Ok(result) => result,
        Err(_) => Err(DocToolError::ExtractionPanicked { page: None }),
    }
}

// Ermittelt die Gesamtseitenzahl, ebenfalls panik-geschützt; eine Panik hier
// gilt wie beim Öffnen als fataler Fehler für das gesamte Dokument.
fn read_page_count(document: &PdfDocument<CursorReader>) -> DocToolResult<u32> {
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        document.page_count().map_err(map_parse_error)
    }));
    match outcome {
        Ok(result) => result,
        Err(_) => Err(DocToolError::ExtractionPanicked { page: None }),
    }
}

// Extrahiert eine einzelne Seite. Panik ODER Parse-Fehler auf dieser einen
// Seite brechen die Gesamtextraktion nicht ab: die Seite erhält stattdessen
// den Platzhaltertext, alle anderen Seiten werden trotzdem geliefert.
fn extract_one_page(document: &PdfDocument<CursorReader>, number: u32) -> ExtractedPage {
    let index0 = number.saturating_sub(1);
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        document
            .extract_text_from_page(index0)
            .map_err(map_parse_error)
    }));
    let text = match outcome {
        Ok(Ok(extracted)) => normalize_text(&extracted.text),
        Ok(Err(error)) => {
            tracing::warn!(page = number, error = %error, "Seite konnte nicht extrahiert werden");
            PAGE_UNREADABLE_PLACEHOLDER.to_owned()
        }
        Err(_) => {
            tracing::warn!(page = number, "Extraktion der Seite ist paniert");
            PAGE_UNREADABLE_PLACEHOLDER.to_owned()
        }
    };
    ExtractedPage { number, text }
}

// Übersetzt einen `oxidize-pdf`-Parse-Fehler in den Crate-Fehlertyp:
// verschlüsselungsbezogene Varianten werden zu `Encrypted`, alles andere zu
// `Parse` mit der `Display`-Textform des ursprünglichen Fehlers.
fn map_parse_error(error: ParseError) -> DocToolError {
    match error {
        ParseError::PdfLocked | ParseError::EncryptionNotSupported | ParseError::WrongPassword => {
            DocToolError::Encrypted
        }
        other => DocToolError::Parse {
            reason: other.to_string(),
        },
    }
}

// Normalisiert extrahierten Seitentext: `\r\n` -> `\n`, nachgestellte
// Leerzeichen je Zeile entfernt, mehr als zwei aufeinanderfolgende Leerzeilen
// auf zwei zusammengefasst.
fn normalize_text(raw: &str) -> String {
    let unified = raw.replace("\r\n", "\n");
    let mut result: Vec<&str> = Vec::new();
    let mut blank_run = 0usize;
    for line in unified.split('\n') {
        let trimmed = line.trim_end_matches(' ');
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run <= 2 {
                result.push(trimmed);
            }
        } else {
            blank_run = 0;
            result.push(trimmed);
        }
    }
    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    // Reine Hilfsfunktion, kein PDF nötig: prüft CRLF-Normalisierung und das
    // Zusammenfassen von mehr als zwei Leerzeilen auf zwei.
    #[test]
    fn test_normalize_text_unifies_crlf_and_collapses_blank_lines() -> TestResult {
        let raw = "Zeile1  \r\nZeile2\r\n\r\n\r\n\r\nZeile3   ";
        let normalized = normalize_text(raw);
        let expected = "Zeile1\nZeile2\n\n\nZeile3";
        if normalized != expected {
            return Err(TestError::Unexpected(format!(
                "normalisierter Text weicht ab: {normalized:?} != {expected:?}"
            )));
        }
        Ok(())
    }

    // Fehlerpfad: kein PDF -> Parse-Fehler, kein Panik. Kein Dateisystem,
    // kein Netz.
    #[test]
    fn test_extract_pages_non_pdf_bytes_returns_parse_error() -> TestResult {
        let result = extract_pages(b"dies ist keine PDF-Datei, nur Text", None);
        match result {
            Err(DocToolError::Parse { .. }) => Ok(()),
            Err(other) => Err(TestError::Unexpected(format!(
                "unerwarteter Fehler statt Parse: {other}"
            ))),
            Ok(_) => Err(TestError::Unexpected(
                "erwarteter Parse-Fehler blieb aus".to_owned(),
            )),
        }
    }

    // Fehlerpfad: leere Bytes -> irgendein Fehler, kein Panik.
    #[test]
    fn test_extract_pages_empty_bytes_returns_error_without_panic() -> TestResult {
        let result = extract_pages(&[], None);
        if result.is_ok() {
            return Err(TestError::Unexpected(
                "erwarteter Fehler bei leeren Bytes blieb aus".to_owned(),
            ));
        }
        Ok(())
    }
}
